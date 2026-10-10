//! Shared helpers for the integration tests: synthetic raw recordings.
//!
//! `scripted_recording` makes a "fake original" from **our own** simulation:
//! it runs the graybox game with scripted inputs and writes what the recorder
//! would have read (state at the start of each frame, keys of the coming
//! frame). Converting and replaying it must reproduce our run, which tests
//! the whole convert → replay → compare chain. `scripted_recording_variable`
//! does the same with a different length for every frame (the original
//! without benchmark mode), on `asamu_trace::stepper`. `varied_recording` is
//! a pseudo-random recording that exercises every conversion branch, for the
//! Python/Rust cross-check; `with_frame_lengths` gives any recording
//! variable frame lengths.

#![allow(dead_code)]

use asamu_core::glam::Vec3;
use asamu_game::Game;
use asamu_player::trace::TraceGrappleState;
use asamu_player::{InputFrame, Trace};
use asamu_player::{TraceMeta, TraceSample};
use asamu_trace::convert::units_to_radians;
use asamu_trace::raw::{
    RAW_FORMAT, RAW_VERSION, RawAxes, RawBinding, RawBoots, RawFile, RawGun, RawHeader,
    RawPawnFlags, RawPlayer, RawRecord, RawWorld,
};
use asamu_trace::stepper::VariableStepper;
use asamu_trace::timestep::FrameLength;

/// The repository root.
pub fn repo_root() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// The default keyboard bindings (names and commands as in `DefaultInput.ini`).
pub fn bindings() -> Vec<RawBinding> {
    [
        ("GBA_MoveForward", "Axis aBaseY Speed=1.0"),
        ("GBA_Backward", "Axis aBaseY Speed=-1.0"),
        ("GBA_StrafeLeft", "Axis aStrafe Speed=-1.0"),
        ("GBA_StrafeRight", "Axis aStrafe Speed=+1.0"),
        ("GBA_Jump", "Jump | Axis aUp Speed=+1.0 AbsoluteAxis=100"),
        ("GBA_Fire", "StartFire | OnRelease StopFire"),
        ("GBA_Use", "use"),
        ("GBA_Sprint", "StartSprinting | OnRelease StopSprinting"),
        ("GBA_ReleaseableJump", "Jump | OnRelease ReleaseJump"),
        (
            "GBA_PowerJump",
            "PowerJumpKeyDown | OnRelease PowerJumpKeyUp",
        ),
        ("MouseX", "Count bXAxis | Axis aMouseX"),
        ("SpaceBar", "GBA_ReleaseableJump | RocketBoostKeyDown"),
        ("LeftShift", "GBA_Sprint"),
        ("W", "GBA_MoveForward"),
        ("S", "GBA_Backward"),
        ("A", "GBA_StrafeLeft"),
        ("D", "GBA_StrafeRight"),
        ("E", "GBA_Use"),
        ("LeftMouseButton", "GBA_Fire"),
        ("RightMouseButton", "GBA_PowerJump"),
    ]
    .into_iter()
    .map(|(n, c)| RawBinding {
        name: n.into(),
        command: c.into(),
    })
    .collect()
}

/// A raw header.
pub fn header(bindings: Vec<RawBinding>) -> RawHeader {
    RawHeader {
        format: RAW_FORMAT.into(),
        version: RAW_VERSION,
        recorder: "integration-test".into(),
        layout: "mac-x86_64-steam-1822049".into(),
        game_build: Some("steam-1822049-mac".into()),
        sample_point: "entry of UWorld::Tick (synthetic)".into(),
        scenario: Some("synthetic".into()),
        launch_options: Some("-BENCHMARK -FPS=60".into()),
        benchmarking: Some(true),
        fixed_delta_time: Some(f64::from(1.0_f32 / 60.0)),
        bindings,
        notes: vec!["made by the asamu-trace tests".into()],
    }
}

/// One scripted tick.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Step {
    pub forward: i32,
    pub right: i32,
    pub jump: bool,
    pub grapple: bool,
    pub sprint: bool,
    pub power: bool,
    pub use_: bool,
    /// Yaw change this tick, rotator units.
    pub d_yaw: i32,
    /// Pitch change this tick, rotator units.
    pub d_pitch: i32,
}

/// Inputs of ticks 1..=n: walk, sprint, strafe, one turn, one pitch change,
/// a jump held and released, a grapple attempt.
pub fn script(n: usize) -> Vec<Step> {
    (1..=n)
        .map(|i| Step {
            forward: if i <= 70 {
                1
            } else if (90..100).contains(&i) {
                -1
            } else {
                0
            },
            right: if (25..40).contains(&i) { 1 } else { 0 },
            jump: (40..52).contains(&i),
            grapple: (60..90).contains(&i),
            sprint: (5..30).contains(&i),
            power: false,
            use_: i == 105,
            d_yaw: if i == 12 { 2048 } else { 0 },
            d_pitch: if i == 20 { 1024 } else { 0 },
        })
        .collect()
}

/// Key names held for a step.
pub fn keys(s: &Step) -> Vec<String> {
    let mut k = Vec::new();
    match s.forward {
        1 => k.push("W"),
        -1 => k.push("S"),
        _ => {}
    }
    match s.right {
        1 => k.push("D"),
        -1 => k.push("A"),
        _ => {}
    }
    for (on, name) in [
        (s.jump, "SpaceBar"),
        (s.grapple, "LeftMouseButton"),
        (s.sprint, "LeftShift"),
        (s.power, "RightMouseButton"),
        (s.use_, "E"),
    ] {
        if on {
            k.push(name);
        }
    }
    k.into_iter().map(str::to_owned).collect()
}

/// The logical input of a step (edges against the previous step).
pub fn input(s: &Step, prev: Option<&Step>) -> InputFrame {
    let p = prev.copied().unwrap_or_default();
    InputFrame {
        move_forward: s.forward as f32,
        move_right: s.right as f32,
        look_yaw_delta: units_to_radians(s.d_yaw),
        look_pitch_delta: units_to_radians(s.d_pitch),
        jump_pressed: s.jump && !p.jump,
        jump_held: s.jump,
        grapple_held: s.grapple,
        sprint_held: s.sprint,
        power_jump_held: s.power,
        use_pressed: prev.is_some() && s.use_ && !p.use_,
    }
}

fn world(frame: u64) -> RawWorld {
    RawWorld {
        map: Some("graybox".into()),
        time_seconds: frame as f32 / 60.0,
        real_time_seconds: frame as f32 / 60.0,
        delta_seconds: 1.0 / 60.0,
        time_dilation: 1.0,
        paused: false,
    }
}

fn base_player() -> RawPlayer {
    RawPlayer {
        controller_class: Some("ASAMUPlayerController".into()),
        pawn_class: Some("ASAMUPawn".into()),
        pawn_id: 0,
        location: Vec3::ZERO,
        velocity: Vec3::ZERO,
        acceleration: Vec3::ZERO,
        pawn_rotation: [0; 3],
        view_rotation: [0; 3],
        physics: 1,
        base: None,
        fov_camera: Some(90.0),
        fov_controller: 90.0,
        keys: vec![],
        pressed_jump: false,
        axes: None,
        ground_speed: 440.0,
        air_speed: 440.0,
        jump_z: 1000.0,
        air_control: 0.3,
        eye_height: Some(38.0),
        gun: None,
        pawn_flags: None,
        boots: None,
    }
}

/// What the recorder would have read during the run `ours` (sample `i` =
/// the state at the start of frame `first_frame + i`, with the keys of the
/// coming step). `timing(i)` gives record `i`'s world timing and its tick
/// argument.
fn records_of(
    ours: &Trace,
    steps: &[Step],
    first_frame: u64,
    max_grapples: i32,
    boots: bool,
    timing: impl Fn(usize, u64) -> (RawWorld, Option<f32>),
) -> Vec<RawRecord> {
    let mut records = Vec::new();
    let (mut yaw, mut pitch) = (0_i32, 0_i32);
    for (i, s) in ours.samples.iter().enumerate() {
        if i > 0 {
            yaw += steps[i - 1].d_yaw;
            pitch += steps[i - 1].d_pitch;
        }
        let next = steps.get(i);
        let prev_step = i.checked_sub(1).and_then(|j| steps.get(j));
        let attached = s.grapple_state == TraceGrappleState::Attached;
        let mut p = base_player();
        p.location = s.position;
        p.velocity = s.velocity;
        p.view_rotation = [pitch, yaw, 0];
        p.pawn_rotation = [0, yaw, 0];
        p.physics = if attached {
            4
        } else if s.grounded {
            1
        } else {
            2
        };
        p.fov_camera = Some(s.fov);
        p.fov_controller = s.fov;
        p.keys = next.map(keys).unwrap_or_default();
        p.pressed_jump = next.is_some_and(|n| n.jump && !prev_step.is_some_and(|q| q.jump));
        p.gun = Some(RawGun {
            grappling: attached,
            released: false,
            can_grapple: true,
            anchor: s.grapple_anchor,
            grapple_location: s.grapple_anchor.unwrap_or(Vec3::ZERO),
            distance: 0.0,
            times_grappled: 0,
            max_grapples,
        });
        p.boots = Some(RawBoots {
            enabled: boots,
            finished: false,
        });
        p.pawn_flags = Some(RawPawnFlags {
            has_jumped: false,
            power_jumped: false,
            has_released_jump: false,
            sprinting: false,
            is_falling: !s.grounded,
        });
        let frame = first_frame + i as u64;
        let (world, dt_arg) = timing(i, frame);
        records.push(RawRecord {
            frame,
            dt_arg,
            world: Some(world),
            player: Some(p),
        });
    }
    records
}

/// A "fake original" raw recording of `n` ticks made with our graybox game,
/// and our own runtime trace of the same run.
pub fn scripted_recording(n: usize, first_frame: u64) -> (RawFile, Trace) {
    let steps = script(n);
    let mut g = Game::graybox().expect("graybox");
    let (max_grapples, boots) = (
        g.player().script.gun.max_grapples,
        g.player().script.boots.enabled,
    );
    g.start();
    g.start_recording();
    for (i, s) in steps.iter().enumerate() {
        let prev = i.checked_sub(1).map(|j| &steps[j]);
        g.tick(&input(s, prev)).expect("tick");
    }
    let ours = g.stop_recording().expect("recording");
    assert_eq!(ours.samples.len(), n + 1);
    let records = records_of(
        &ours,
        &steps,
        first_frame,
        max_grapples,
        boots,
        |_, frame| (world(frame), Some(1.0 / 60.0)),
    );
    (
        RawFile {
            header: header(bindings()),
            records,
        },
        ours,
    )
}

/// Frame lengths as a game without a fixed step produces them: around
/// `1 / fps` with a few percent of jitter, a hitch now and then, never two
/// alike in a row.
pub fn jittery_lengths(seed: u64, n: usize, fps: f32) -> Vec<f32> {
    let mut r = Lcg(seed);
    let mut last = 0.0_f32;
    (0..n)
        .map(|i| {
            let mut d = 1.0 / fps * (1.0 + r.f32(0.06));
            if i % 97 == 50 {
                d *= 3.0;
            }
            if d.to_bits() == last.to_bits() {
                d += 1e-5;
            }
            last = d;
            d
        })
        .collect()
}

/// A "fake original" recorded **without a fixed time step**: tick `i` of the
/// script runs with the frame length `lengths[i]` (on
/// `asamu_trace::stepper::VariableStepper`, the graybox game with a `dt` per
/// tick). Returns the raw recording as the recorder would have written it
/// (each record's `DeltaSeconds` is the previous frame's length, `dt_arg`
/// the coming frame's) and our own trace of the run (`tick_rate: null`,
/// time = the `f64` sum of the lengths).
pub fn scripted_recording_variable(lengths: &[f32], first_frame: u64) -> (RawFile, Trace) {
    let n = lengths.len();
    let steps = script(n);
    let game = Game::graybox().expect("graybox");
    let (max_grapples, boots) = (
        game.player().script.gun.max_grapples,
        game.player().script.boots.enabled,
    );
    let mut sim = VariableStepper::from_game(&game);
    let mut meta = TraceMeta::runtime(Some("graybox".into()), None);
    meta.notes
        .push("variable frame lengths; made by the asamu-trace tests".into());
    let mut ours = Trace::new(meta);
    ours.samples.push(TraceSample::capture(
        0,
        0.0,
        &InputFrame::default(),
        sim.player(),
        sim.fov(),
    ));
    let mut times = vec![0.0_f64];
    for (i, s) in steps.iter().enumerate() {
        let prev = i.checked_sub(1).map(|j| &steps[j]);
        let frame_input = input(s, prev);
        let before = times[i];
        let after = before + f64::from(lengths[i]);
        sim.tick(&frame_input, FrameLength::of(after - before).dt(), after);
        times.push(after);
        ours.samples.push(TraceSample::capture(
            i as u64 + 1,
            after,
            &frame_input,
            sim.player(),
            sim.fov(),
        ));
    }
    let records = records_of(
        &ours,
        &steps,
        first_frame,
        max_grapples,
        boots,
        |i, frame| {
            let mut w = world(frame);
            // The length of the frame that ended at this record (the first
            // record's is from before the recording).
            w.delta_seconds = i.checked_sub(1).map_or(1.0 / 59.0, |j| lengths[j]);
            w.time_seconds = 100.0 + times[i] as f32;
            w.real_time_seconds = w.time_seconds;
            (w, lengths.get(i).copied())
        },
    );
    let mut h = header(bindings());
    h.launch_options = Some("-WINDOWED".into());
    h.benchmarking = Some(false);
    h.fixed_delta_time = Some(f64::from(1.0_f32 / 30.0));
    (RawFile { header: h, records }, ours)
}

/// Gives every record of `raw` its own `DeltaSeconds` (and a matching tick
/// argument on the record before it), as a recording without benchmark mode
/// has them.
pub fn with_frame_lengths(raw: &mut RawFile, seed: u64) {
    let lengths = jittery_lengths(seed, raw.records.len(), 60.0);
    for (i, d) in lengths.iter().enumerate() {
        if let Some(w) = raw.records[i].world.as_mut() {
            w.delta_seconds = *d;
        }
        if let Some(prev) = i.checked_sub(1) {
            raw.records[prev].dt_arg = Some(*d);
        }
    }
    raw.header.benchmarking = Some(false);
}

/// Deterministic pseudo-random numbers (64-bit LCG, high bits).
pub struct Lcg(pub u64);

impl Lcg {
    pub fn next(&mut self) -> u32 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        (self.0 >> 33) as u32
    }

    pub fn below(&mut self, n: u32) -> u32 {
        self.next() % n.max(1)
    }

    pub fn f32(&mut self, scale: f32) -> f32 {
        (self.next() as f32 / u32::MAX as f32 - 0.5) * 2.0 * scale
    }
}

/// A pseudo-random recording covering the conversion branches: gaps, paused
/// frames, missing players, map and pawn changes, FOV fallbacks, axes that
/// disagree with the keys, odd bindings, rotation wraps, grapple states with
/// and without gun data.
pub fn varied_recording(seed: u64, n: usize, with_bindings: bool) -> RawFile {
    let mut r = Lcg(seed);
    let names = [
        "W",
        "S",
        "A",
        "D",
        "SpaceBar",
        "LeftMouseButton",
        "LeftShift",
        "RightMouseButton",
        "E",
        "F7",
        "Q",
        "Unbound",
    ];
    let mut b = bindings();
    b.push(RawBinding {
        name: "Q".into(),
        command: "  axis  aStrafe   Speed=-.5 | GBA_Use|onrelease jump".into(),
    });
    b.push(RawBinding {
        name: "F7".into(),
        command: "GBA_QuickLoad".into(),
    });
    b.push(RawBinding {
        name: "W".into(),
        command: "GBA_MoveForward | GBA_Sprint".into(),
    });
    let mut records = Vec::new();
    let mut frame = 5_000_u64;
    let (mut yaw, mut pitch) = (r.next() as i32, 0_i32);
    let mut pawn_id = 0;
    for i in 0..n {
        frame += if r.below(40) == 0 {
            2 + u64::from(r.below(5))
        } else {
            1
        };
        yaw = yaw.wrapping_add(r.below(9000) as i32 - 4500);
        pitch = (pitch + r.below(800) as i32 - 400).clamp(-16384, 16383);
        if r.below(150) == 0 {
            pawn_id += 1;
        }
        let mut p = base_player();
        p.pawn_id = pawn_id;
        p.location = Vec3::new(r.f32(5000.0), r.f32(5000.0), r.f32(800.0));
        p.velocity = Vec3::new(r.f32(2000.0), r.f32(2000.0), r.f32(1500.0));
        p.view_rotation = [pitch.wrapping_add(65536 * (r.below(3) as i32 - 1)), yaw, 0];
        p.physics = [1, 1, 1, 2, 2, 4, 0][r.below(7) as usize];
        p.base = (r.below(3) == 0).then(|| format!("StaticMeshActor_{}", r.below(50)));
        p.fov_camera = match r.below(10) {
            0 => None,
            1 => Some(0.0),
            _ => Some(90.0 - r.below(40) as f32),
        };
        p.fov_controller = 90.0;
        let k = r.below(4);
        p.keys = (0..k)
            .map(|_| names[r.below(names.len() as u32) as usize].to_owned())
            .collect();
        p.pressed_jump = r.below(8) == 0;
        p.axes = (r.below(4) != 0).then(|| RawAxes {
            base_y: [-1200.0, 0.0, 1200.0][r.below(3) as usize],
            strafe: [-1200.0, 0.0, 600.0][r.below(3) as usize],
            forward: 0.0,
            turn: r.f32(10.0),
            look_up: r.f32(10.0),
            mouse_x: r.f32(5.0),
            mouse_y: r.f32(5.0),
        });
        p.air_control = 0.3 + r.f32(0.05);
        p.gun = (r.below(5) != 0).then(|| RawGun {
            grappling: r.below(4) == 0,
            released: false,
            can_grapple: true,
            anchor: (r.below(3) != 0)
                .then(|| Vec3::new(r.f32(4000.0), r.f32(4000.0), r.f32(900.0))),
            grapple_location: Vec3::new(r.f32(4000.0), 1.5, -2.25),
            distance: r.f32(1000.0).abs(),
            times_grappled: r.below(3) as i32,
            max_grapples: r.below(5) as i32 - 1,
        });
        p.boots = (r.below(2) == 0).then(|| RawBoots {
            enabled: r.below(2) == 0,
            finished: false,
        });
        p.pawn_flags = (r.below(2) == 0).then(|| RawPawnFlags {
            has_jumped: false,
            power_jumped: false,
            has_released_jump: false,
            sprinting: r.below(2) == 0,
            is_falling: false,
        });
        let mut w = world(frame);
        w.delta_seconds = if r.below(30) == 0 { 0.02 } else { 1.0 / 60.0 };
        w.paused = r.below(60) == 0;
        if i > n / 2 {
            w.map = Some("AG-Cave".into());
        }
        records.push(RawRecord {
            frame,
            dt_arg: (r.below(2) == 0).then_some(1.0 / 60.0),
            world: Some(w),
            player: (r.below(70) != 0).then_some(p),
        });
    }
    let mut h = header(if with_bindings { b } else { vec![] });
    if !with_bindings {
        h.launch_options = None;
        h.scenario = Some(String::new());
    }
    RawFile { header: h, records }
}
