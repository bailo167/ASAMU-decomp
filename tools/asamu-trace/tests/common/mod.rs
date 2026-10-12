//! Shared helpers for the integration tests: synthetic raw recordings.
//!
//! `scripted_recording` makes a "fake original" from **our own** simulation:
//! it runs the graybox game with scripted inputs and writes what the recorder
//! would have read (state at the start of each frame with the script state
//! our simulation has there, keys of the coming frame). Converting and
//! replaying it must reproduce our run, which tests the whole convert →
//! replay → compare chain. `scripted_gamepad_recording` is the same run as a
//! gamepad would leave it: buttons under their pad names, no move key, and
//! the move axes only in the pawn's acceleration. `scripted_recording_variable`
//! does the same with a different length for every frame (the original
//! without benchmark mode), on `asamu_trace::stepper`. `varied_recording` is
//! a pseudo-random recording that exercises every conversion branch, for the
//! Python/Rust cross-check; `with_frame_lengths` gives any recording
//! variable frame lengths; `with_optional_members` gives one the optional
//! members of recorder 0.2.0 and `recorder_stats` the sidecar that recorder
//! writes next to it (markers, the dropped-frame log).

#![allow(dead_code)]

use asamu_core::coords::{ue_forward_flat, ue_right_flat};
use asamu_core::glam::Vec3;
use asamu_game::Game;
use asamu_player::trace::TraceGrappleState;
use asamu_player::{InputFrame, PlayerState, Trace};
use asamu_player::{TraceMeta, TraceSample};
use asamu_trace::convert::units_to_radians;
use asamu_trace::raw::{
    OPTIONAL_FIELDS, RAW_FORMAT, RAW_VERSION, RawAxes, RawBinding, RawBob, RawBoots, RawCameraPov,
    RawCylinder, RawFile, RawGun, RawHeader, RawPawnFlags, RawPlayer, RawRecord, RawTimer,
    RawWorld,
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

/// The bindings of a recording made with a gamepad: the keyboard's plus the
/// pad's buttons and stick axes (names and commands as the game reports
/// them).
pub fn gamepad_bindings() -> Vec<RawBinding> {
    let mut b = bindings();
    b.extend(
        [
            (
                "GBA_StrafeLeft_Gamepad",
                "Axis aStrafe Speed=1.0 DeadZone=0.4",
            ),
            (
                "GBA_MoveForward_Gamepad",
                "Axis aBaseY Speed=1.0 DeadZone=0.4",
            ),
            ("XboxTypeS_LeftX", "GBA_StrafeLeft_Gamepad"),
            ("XboxTypeS_LeftY", "GBA_MoveForward_Gamepad"),
            ("XboxTypeS_RightShoulder", "GBA_PowerJump"),
            ("XboxTypeS_RightTrigger", "GBA_Fire"),
            ("XboxTypeS_LeftShoulder", "GBA_Sprint"),
            ("XboxTypeS_A", "GBA_ReleaseableJump | RocketBoostKeyDown"),
            ("XboxTypeS_X", "GBA_Use"),
        ]
        .into_iter()
        .map(|(n, c)| RawBinding {
            name: n.into(),
            command: c.into(),
        }),
    );
    b
}

/// Pad button names held for a step (the stick is not a key).
pub fn gamepad_keys(s: &Step) -> Vec<String> {
    [
        (s.jump, "XboxTypeS_A"),
        (s.grapple, "XboxTypeS_RightTrigger"),
        (s.sprint, "XboxTypeS_LeftShoulder"),
        (s.power, "XboxTypeS_RightShoulder"),
        (s.use_, "XboxTypeS_X"),
    ]
    .into_iter()
    .filter(|(on, _)| *on)
    .map(|(_, name)| name.to_owned())
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
        optional_fields: vec![],
        optional_layout: None,
    }
}

/// A fake original that keeps our simulation's own samples: the trace
/// converted from `raw` (its notes: the state timeline, the events) with the
/// samples of `ours`, the run `raw` was made from. The converted samples
/// hold the same run with the view angles gone through rotator units, which
/// can move a yaw by one bit; these are our run bit for bit, as a test of
/// exactness needs them.
pub fn fake_original(raw: &RawFile, ours: &Trace) -> Trace {
    let segments = asamu_trace::convert::convert(raw, &Default::default()).expect("convert");
    assert_eq!(segments.len(), 1);
    let converted = &segments[0].trace;
    assert_eq!(converted.samples.len(), ours.samples.len());
    for (a, b) in converted.samples.iter().zip(&ours.samples) {
        assert_eq!(a.input, b.input, "tick {}", a.tick);
        assert_eq!(a.position, b.position, "tick {}", a.tick);
    }
    let mut samples = ours.samples.clone();
    for (s, c) in samples.iter_mut().zip(&converted.samples) {
        s.time = c.time;
    }
    Trace {
        meta: converted.meta.clone(),
        samples,
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
        air_speed: 2000.0,
        jump_z: 1000.0,
        air_control: 0.3,
        eye_height: Some(38.0),
        gun: None,
        pawn_flags: None,
        boots: None,
        base_level: None,
        fov_default: None,
        fov_locked: None,
        fov_lock: None,
        camera_pov: None,
        floor: None,
        base_eye_height: None,
        walk_bob: None,
        bob: None,
        cylinder: None,
    }
}

/// The base actor name the fake recordings give a pawn that stands on
/// something (hand-made levels have no actor names).
pub const FLOOR_ACTOR: &str = "StaticMeshActor_1";

/// What the recorder would have read during the run `ours` (sample `i` =
/// the state at the start of frame `first_frame + i`, with the script state
/// `states[i]` our simulation had there and the keys of the coming step).
/// With `gamepad` the buttons carry their pad names, no move key is held and
/// the pawn's acceleration holds what the controller's walking move writes
/// from the step's move axes (`AccelRate` along them, in the pawn's yaw
/// before the step's turn). `timing(i)` gives record `i`'s world timing and
/// its tick argument.
fn records_of(
    ours: &Trace,
    states: &[PlayerState],
    steps: &[Step],
    first_frame: u64,
    gamepad: bool,
    timing: impl Fn(usize, u64) -> (RawWorld, Option<f32>),
) -> Vec<RawRecord> {
    assert_eq!(ours.samples.len(), states.len());
    let mut records = Vec::new();
    let (mut yaw, mut pitch) = (0_i32, 0_i32);
    for (i, s) in ours.samples.iter().enumerate() {
        let mut acceleration = Vec3::ZERO;
        if i > 0 {
            let step = &steps[i - 1];
            if gamepad {
                let before = units_to_radians(yaw);
                let wish = ue_forward_flat(before) * step.forward as f32
                    + ue_right_flat(before) * step.right as f32;
                acceleration = wish.normalize_or_zero() * 2048.0;
            }
            yaw += step.d_yaw;
            pitch += step.d_pitch;
        }
        let next = steps.get(i);
        let prev_step = i.checked_sub(1).and_then(|j| steps.get(j));
        let attached = s.grapple_state == TraceGrappleState::Attached;
        let script = &states[i].script;
        let mut p = base_player();
        p.location = s.position;
        p.velocity = s.velocity;
        p.acceleration = acceleration;
        p.ground_speed = script.ground_speed;
        p.air_speed = script.air_speed;
        p.jump_z = script.jump_z;
        p.air_control = script.air_control;
        p.view_rotation = [pitch, yaw, 0];
        p.pawn_rotation = [0, yaw, 0];
        p.eye_height = Some(script.eye_height);
        p.base = states[i].pawn.based.then(|| FLOOR_ACTOR.to_owned());
        p.physics = if attached {
            4
        } else if s.grounded {
            1
        } else {
            2
        };
        p.fov_camera = Some(s.fov);
        p.fov_controller = s.fov;
        p.keys = next
            .map(if gamepad { gamepad_keys } else { keys })
            .unwrap_or_default();
        p.pressed_jump = next.is_some_and(|n| n.jump && !prev_step.is_some_and(|q| q.jump));
        p.gun = Some(RawGun {
            grappling: attached,
            released: script.gun.released,
            can_grapple: script.gun.can_grapple,
            anchor: s.grapple_anchor,
            grapple_location: s.grapple_anchor.unwrap_or(Vec3::ZERO),
            distance: 0.0,
            times_grappled: script.gun.times_grappled,
            max_grapples: script.gun.max_grapples,
            state: None,
            timers: None,
        });
        p.boots = Some(RawBoots {
            enabled: script.boots.enabled,
            finished: false,
        });
        p.pawn_flags = Some(RawPawnFlags {
            has_jumped: false,
            power_jumped: false,
            has_released_jump: false,
            sprinting: script.sprint.active,
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
    scripted(n, first_frame, false)
}

/// The same run as a gamepad would leave it in a recording (see
/// [`records_of`]).
pub fn scripted_gamepad_recording(n: usize, first_frame: u64) -> (RawFile, Trace) {
    scripted(n, first_frame, true)
}

fn scripted(n: usize, first_frame: u64, gamepad: bool) -> (RawFile, Trace) {
    scripted_steps(&script(n), first_frame, gamepad)
}

/// A "fake original" of the given steps (see [`scripted_recording`]).
pub fn scripted_steps(steps: &[Step], first_frame: u64, gamepad: bool) -> (RawFile, Trace) {
    let g = Game::graybox().expect("graybox");
    scripted_steps_on(g, steps, first_frame, gamepad, |_, _| {})
}

/// The graybox game with a **test-only copy** of the original's parameters
/// that `change` has altered (the crates' parameters stay as they are): the
/// "original" of a test that checks how a difference in one rule shows in a
/// replay made with the unaltered parameters.
pub fn graybox_with(change: impl FnOnce(&mut asamu_player::PlayerParams)) -> Game {
    let mut params = asamu_player::PlayerParams::asamu_original();
    change(&mut params);
    let level = Game::graybox().expect("graybox").level().clone();
    Game::new(level, params, asamu_core::DEFAULT_TICK_RATE_HZ).expect("game")
}

/// [`scripted_steps`] on a game of the caller's (the graybox with its own
/// parameters: [`graybox_with`]). `level_script(k, game)` runs after tick
/// `k` and before the recorder reads the state of record `k` (so what it
/// changes is in record `k` and acts from tick `k + 1` on, like a level
/// script's change in the original).
pub fn scripted_steps_on(
    mut g: Game,
    steps: &[Step],
    first_frame: u64,
    gamepad: bool,
    mut level_script: impl FnMut(usize, &mut Game),
) -> (RawFile, Trace) {
    let n = steps.len();
    let mut states = vec![*g.player()];
    g.start();
    g.start_recording();
    for (i, s) in steps.iter().enumerate() {
        let prev = i.checked_sub(1).map(|j| &steps[j]);
        g.tick(&input(s, prev)).expect("tick");
        level_script(i + 1, &mut g);
        states.push(*g.player());
    }
    let ours = g.stop_recording().expect("recording");
    assert_eq!(ours.samples.len(), n + 1);
    let records = records_of(&ours, &states, steps, first_frame, gamepad, |_, frame| {
        (world(frame), Some(1.0 / 60.0))
    });
    (
        RawFile {
            header: header(if gamepad {
                gamepad_bindings()
            } else {
                bindings()
            }),
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
    let game = Game::graybox().expect("graybox");
    scripted_variable_on(&game, &script(lengths.len()), lengths, first_frame)
}

/// [`scripted_recording_variable`] with the caller's game (its level and
/// parameters: [`graybox_with`]) and steps (one per frame length).
pub fn scripted_variable_on(
    game: &Game,
    steps: &[Step],
    lengths: &[f32],
    first_frame: u64,
) -> (RawFile, Trace) {
    assert_eq!(steps.len(), lengths.len());
    let mut sim = VariableStepper::from_game(game);
    let mut states = vec![*sim.player()];
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
        states.push(*sim.player());
        times.push(after);
        ours.samples.push(TraceSample::capture(
            i as u64 + 1,
            after,
            &frame_input,
            sim.player(),
            sim.fov(),
        ));
    }
    let records = records_of(&ours, &states, steps, first_frame, false, |i, frame| {
        let mut w = world(frame);
        // The length of the frame that ended at this record (the first
        // record's is from before the recording).
        w.delta_seconds = i.checked_sub(1).map_or(1.0 / 59.0, |j| lengths[j]);
        w.time_seconds = 100.0 + times[i] as f32;
        w.real_time_seconds = w.time_seconds;
        (w, lengths.get(i).copied())
    });
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

/// Gives the records of `raw` pawn rotations (with pitch and roll, the yaw
/// mostly the view's) and accelerations of every kind a converter has to
/// read: none, `AccelRate` in any direction, the unit length a landing
/// leaves, tiny, huge, with a vertical part.
pub fn with_accelerations(raw: &mut RawFile, seed: u64) {
    let mut r = Lcg(seed ^ 0x9E37_79B9_7F4A_7C15);
    for rec in &mut raw.records {
        let Some(p) = rec.player.as_mut() else {
            continue;
        };
        let yaw = if r.below(10) == 0 {
            r.next() as i32
        } else {
            p.view_rotation[1]
        };
        let pitch = match r.below(6) {
            0 => r.below(20_000) as i32 - 10_000,
            1 => 16_384,
            _ => 0,
        };
        let roll = if r.below(3) == 0 {
            r.below(4000) as i32 - 2000
        } else {
            0
        };
        p.pawn_rotation = [pitch, yaw, roll];
        let angle = r.f32(std::f32::consts::PI);
        let length = match r.below(12) {
            0..=3 => 0.0,
            4..=8 => 2048.0,
            9 => 1.0,
            10 => 1.0e-30,
            _ => 3.0e37,
        };
        p.acceleration = Vec3::new(
            length * angle.cos(),
            length * angle.sin(),
            if r.below(5) == 0 { -520.0 } else { 0.0 },
        );
    }
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
/// and without gun data, ground speeds of every kind, eye heights, and
/// (the positions being random) a teleport on almost every tick.
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
        // The speeds the pawn's script, story mode and a console command
        // give; an eye height that is absent, at rest or anywhere.
        p.ground_speed = [440.0, 440.0, 440.0, 880.0, 264.0, 132.0][r.below(6) as usize];
        p.eye_height = match r.below(4) {
            0 => None,
            1 => Some(38.0),
            _ => Some(20.0 + r.below(300) as f32 / 10.0),
        };
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
            state: None,
            timers: None,
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

/// Gives the records of `raw` the optional members of recorder 0.2.0
/// (docs/TRACE_CAPTURE.md 6.10), each group on most records and missing on
/// some, with every kind of value a converter has to read: a camera FOV
/// lock that is off, on with a usable locked FOV, on with 0 or 250 degrees;
/// a base level that is a name or null; floor normals that are zero, up or
/// tilted; a weapon state that is a name or null; timer lists that are
/// empty, hold a refire check under either spelling (running or paused) or
/// only another timer.
pub fn with_optional_members(raw: &mut RawFile, seed: u64) {
    let mut r = Lcg(seed ^ 0x0200_0BAD_5EED);
    let mut count = 0.0_f32;
    for rec in &mut raw.records {
        let Some(p) = rec.player.as_mut() else {
            continue;
        };
        if r.below(12) != 0 {
            p.fov_default = Some(90.0);
            p.fov_locked = Some(r.below(3) == 0);
            p.fov_lock = Some(match r.below(8) {
                0 => 0.0,
                1 => 250.0,
                n => 40.0 + n as f32 * 2.5,
            });
        }
        if r.below(10) != 0 {
            let level = ["AG-Workshop", "freds_place"][r.below(2) as usize];
            p.base_level = Some(p.base.as_ref().map(|_| level.to_owned()));
        }
        if r.below(10) != 0 {
            p.floor = Some(match r.below(4) {
                0 => Vec3::ZERO,
                1 => Vec3::new(0.6, 0.0, 0.8),
                _ => Vec3::Z,
            });
        }
        if r.below(10) != 0 {
            p.base_eye_height = Some(if r.below(6) == 0 { 43.0 } else { 38.0 });
        }
        if r.below(10) != 0 {
            p.walk_bob = Some(Vec3::new(0.0, r.f32(0.5), r.f32(2.0)));
            p.bob = Some(RawBob {
                bob: 0.01,
                land: r.f32(1.0),
                jump: 0.0,
                applied: r.f32(1.0),
                time: r.f32(100.0).abs(),
                just_landed: r.below(9) == 0,
                land_recovery: r.below(3) == 0,
            });
        }
        if r.below(10) != 0 {
            p.camera_pov = Some(RawCameraPov {
                location: p.location + Vec3::new(0.0, 0.0, 38.0),
                rotation: p.view_rotation,
            });
        }
        if r.below(10) != 0 {
            p.cylinder = Some(RawCylinder {
                radius: 21.0,
                half_height: if r.below(20) == 0 { 61.6 } else { 44.0 },
                translation: Vec3::ZERO,
                collision_component: true,
            });
        }
        if let Some(g) = p.gun.as_mut() {
            if r.below(10) != 0 {
                g.state = Some(match r.below(5) {
                    0 => None,
                    1 => Some("WeaponFiring".to_owned()),
                    2 => Some("WeaponEquipping".to_owned()),
                    _ => Some("Active".to_owned()),
                });
            }
            if r.below(10) != 0 {
                let mut timers = Vec::new();
                if r.below(3) != 0 {
                    count = if r.below(4) == 0 {
                        0.0
                    } else {
                        count + 1.0 / 60.0
                    };
                    let name = if r.below(5) == 0 {
                        "refirechecktimer"
                    } else {
                        "RefireCheckTimer"
                    };
                    timers.push(RawTimer {
                        name: name.to_owned(),
                        rate: 0.1,
                        count,
                        looping: r.below(2) == 0,
                        paused: r.below(15) == 0,
                    });
                }
                if r.below(2) == 0 {
                    timers.push(RawTimer {
                        name: "InstantReleaseTimer".to_owned(),
                        rate: 0.05,
                        count: r.f32(0.05).abs(),
                        looping: false,
                        paused: false,
                    });
                }
                g.timers = Some(timers);
            }
        }
    }
    raw.header.recorder = "asamu_win 0.2.0".to_owned();
    raw.header.optional_fields = OPTIONAL_FIELDS.iter().map(|s| (*s).to_owned()).collect();
    raw.header.optional_layout = Some(Some("win-x86-steam-1822049-optional".to_owned()));
}

/// The sidecar recorder 0.2.0 would write next to `raw` (`<name>.stats.json`,
/// see `asamu_trace::sidecar`), as JSON text: the three numbers that tie it
/// to the recording, a dropped-frame log, and markers.
///
/// The log: frames without a player before the first record; for the gaps
/// between records one entry, two entries of different reasons, or none (a
/// gap the log does not explain); `no-input` entries for records before a
/// gap and, as no recorder writes them, for a few records in the middle of
/// consecutive frames. The markers: at records, before the first record
/// (`frame` null), in frames that have no record, and after the last
/// record, with names that need care in a notes line.
pub fn recorder_stats(raw: &RawFile, seed: u64) -> String {
    use serde_json::json;
    let mut r = Lcg(seed ^ 0x5747_A754);
    let frames: Vec<u64> = raw.records.iter().map(|rec| rec.frame).collect();
    let (first, last) = (frames.first().copied(), frames.last().copied());
    let mut drops = Vec::new();
    if let Some(f) = first {
        drops.push(
            json!({"frame": f.saturating_sub(50), "last": f.saturating_sub(1), "count": 7,
                          "reason": "skipped", "detail": "no-player"}),
        );
    }
    let reasons = [
        (
            "missed",
            Some("the counter advanced by 3 between two polls"),
        ),
        ("torn", Some("the counter moved while the sample was read")),
        (
            "late",
            Some(
                "GDeltaTime changed before the sample was complete (the next frame's time update had run)",
            ),
        ),
        ("unconfirmed", None),
        ("skipped", Some("paused")),
    ];
    let entry = |r: &mut Lcg, lo: u64, hi: u64| {
        let (reason, detail) = reasons[r.below(reasons.len() as u32) as usize];
        json!({"frame": lo, "last": hi, "count": hi - lo + 1, "reason": reason, "detail": detail})
    };
    for w in frames.windows(2) {
        let (a, b) = (w[0], w[1]);
        if b > a + 1 {
            if r.below(3) == 0 {
                drops.push(json!({"frame": a, "last": a, "count": 1, "reason": "no-input",
                    "detail": "record written without its input: the tick start of its frame was not seen"}));
            }
            match r.below(4) {
                0 => {}
                1 if b > a + 2 => {
                    drops.push(entry(&mut r, a + 1, a + 1));
                    drops.push(entry(&mut r, a + 2, b - 1));
                }
                _ => drops.push(entry(&mut r, a + 1, b - 1)),
            }
        } else if b == a + 1 && r.below(45) == 0 {
            drops.push(
                json!({"frame": a, "last": a, "count": 1, "reason": "no-input", "detail": null}),
            );
        }
    }
    let names = [
        "zoom hold 1",
        "wall 45",
        "say \"cheese\", twice",
        "Zürich → 東京",
        "tab\tand\nbreak",
        "a name that is much longer than the sixty-four characters the recorder keeps of it",
        "stairs",
        "end",
    ];
    let mut markers = Vec::new();
    for (i, name) in names.iter().enumerate() {
        let frame = match (i, first, last) {
            (0, _, _) | (_, None, _) | (_, _, None) => None,
            (1, Some(f), _) => Some(f),
            (7, _, Some(l)) => Some(l + 100),
            (_, Some(f), Some(l)) => Some(f + u64::from(r.below((l - f + 1) as u32))),
        };
        markers.push((frame, i, *name));
    }
    // The recorder lists them in the order they were set.
    markers.sort_by_key(|(frame, i, _)| (frame.map_or(0, |f| f + 1), *i));
    // (Shown only where a gap has no entry.)
    let omitted = if seed.is_multiple_of(2) { 0 } else { 3 };
    let markers: Vec<serde_json::Value> = markers
        .into_iter()
        .map(|(frame, i, name)| {
            json!({"name": name, "id": format!("m{i}"), "frame": frame,
                   "records": frame.map_or(0, |f| frames.iter().filter(|x| **x <= f).count()),
                   "state": if frame.is_some() { "recording" } else { "waiting-for-player" },
                   "utc": "2026-10-12T10:15:00Z"})
        })
        .collect();
    json!({
        "recorder": "asamu_win 0.2.0", "state": "finished", "file": "synthetic.raw.jsonl",
        "records": frames.len(), "gaps": frames.windows(2).filter(|w| w[1] != w[0] + 1).count(),
        "first_frame": first, "last_frame": last,
        "stats": {"written": frames.len(), "torn": 0, "skipped": {"no-player": 7},
                  "window_ms": {"mean": 9.5, "p99": 12.0, "max": 13.25},
                  "drops": drops, "drops_omitted": omitted},
        "optional": null, "markers": markers, "markers_omitted": 0,
        "updated_utc": "2026-10-12T10:16:00Z"
    })
    .to_string()
}

// ---- the Python recorder (tools/trace-recorder)

/// A Python 3.8+ interpreter on the PATH, if any.
pub fn python() -> Option<&'static str> {
    ["python3", "python"].into_iter().find(|p| {
        std::process::Command::new(p)
            .args([
                "-c",
                "import sys; sys.exit(0 if sys.version_info >= (3, 8) else 1)",
            ])
            .output()
            .is_ok_and(|o| o.status.success())
    })
}

/// `tools/trace-recorder`.
pub fn recorder_dir() -> std::path::PathBuf {
    repo_root().join("tools/trace-recorder")
}

/// Runs `py -I -B args...` and returns its standard output; panics with
/// both streams when it fails.
pub fn run_python(py: &str, args: &[&std::ffi::OsStr]) -> String {
    let out = std::process::Command::new(py)
        .args(["-I", "-B"])
        .args(args)
        .output()
        .expect("python");
    assert!(
        out.status.success(),
        "python {args:?} failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// The notes of the two converters agree: text for text, and the JSON notes
/// (the state timeline, the older `init:` note, the markers) value for
/// value.
pub fn same_notes(py: &[String], rs: &[String]) {
    assert_eq!(py.len(), rs.len(), "notes differ:\n{py:#?}\n{rs:#?}");
    for (a, b) in py.iter().zip(rs) {
        let json = [
            asamu_trace::convert::INIT_NOTE_PREFIX,
            asamu_trace::state::STATE_NOTE_PREFIX,
            asamu_trace::segments::MARKERS_NOTE_PREFIX,
        ]
        .into_iter()
        .find_map(|p| Some((a.strip_prefix(p)?, b.strip_prefix(p)?)));
        match json {
            Some((ja, jb)) => {
                let va: serde_json::Value = serde_json::from_str(ja).expect("JSON note");
                let vb: serde_json::Value = serde_json::from_str(jb).expect("JSON note");
                assert_eq!(va, vb, "JSON notes differ");
            }
            None => assert_eq!(a, b),
        }
    }
}

/// The Python converter's traces for the raw file at `raw_path` (written
/// into `out_dir`), with `extra` arguments; `count` traces are expected.
pub fn python_convert(
    py: &str,
    raw_path: &std::path::Path,
    out_dir: &std::path::Path,
    extra: &[&std::ffi::OsStr],
    count: usize,
) -> Vec<Trace> {
    std::fs::create_dir_all(out_dir).expect("out dir");
    let core = recorder_dir().join("asamu_recorder_core.py");
    let mut args: Vec<&std::ffi::OsStr> = vec![
        core.as_os_str(),
        "convert".as_ref(),
        raw_path.as_os_str(),
        "--out-dir".as_ref(),
        out_dir.as_os_str(),
    ];
    args.extend_from_slice(extra);
    let printed = run_python(py, &args);
    assert_eq!(printed.lines().count(), count, "{printed}");
    let name = raw_path
        .file_name()
        .and_then(|n| n.to_str())
        .expect("file name");
    asamu_trace::convert::output_names(name, count)
        .iter()
        .map(|n| {
            let text = std::fs::read_to_string(out_dir.join(n)).expect("trace");
            Trace::from_jsonl_str(&text).expect("valid trace")
        })
        .collect()
}

/// Both converters' traces of one recording are the same: samples bit for
/// bit, metadata, notes.
pub fn same_traces(python: &[Trace], rust: &[asamu_trace::convert::Segment]) {
    assert_eq!(python.len(), rust.len());
    for (i, (p, seg)) in python.iter().zip(rust).enumerate() {
        let r = &seg.trace;
        assert_eq!(p.samples, r.samples, "segment {i}: samples differ");
        assert_eq!(p.meta.format, r.meta.format);
        assert_eq!(p.meta.schema_version, r.meta.schema_version);
        assert_eq!(p.meta.source, r.meta.source);
        assert_eq!(p.meta.game_build, r.meta.game_build);
        assert_eq!(p.meta.level, r.meta.level);
        assert_eq!(p.meta.tick_rate, r.meta.tick_rate);
        assert_eq!(p.meta.units, r.meta.units);
        same_notes(&p.meta.notes, &r.meta.notes);
    }
}
