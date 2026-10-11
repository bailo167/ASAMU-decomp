//! Tuning as a user does it: change a parameter in a session on a hand-made
//! arena (ours; not original content), then measure what the pawn does.
//!
//! Every test is differential: the same scripted action is measured under
//! the Classic set and under an override, in this recreation, and only the
//! *direction* of the difference is asserted. No number of the simulation is
//! written down here, and nothing in this file is a measurement of the
//! original. Run with `--nocapture` to see the measured values.
#![allow(clippy::unwrap_used)]

use asamu_game::{Game, LevelScript, TickReport};
use asamu_player::InputFrame;
use asamu_sandbox::arena::{
    GRAPPLE_LAB, MOVEMENT_LAB, RANGE_HOOK_FRACTIONS, STAIR_RISERS, build_arena, range_hook,
    stair_lane_start,
};
use asamu_sandbox::command::{Command, CommandError, TeleportTarget};
use asamu_sandbox::keys::{Catalog, TuneValue};
use asamu_sandbox::overlay::ParamSetLabel;
use asamu_sandbox::profile::Profile;
use asamu_sandbox::session::{Outcome, Session, SimCx};
use asamu_sandbox::telemetry::{JumpStats, SwingStats};
use glam::Vec3;

/// A session and the game it runs, driven the way a host drives them:
/// reconcile, rules, the game's own tick, observe.
struct Lab {
    session: Session,
    game: Game,
    script: Option<LevelScript>,
}

impl Lab {
    fn on(arena: &str) -> Self {
        Self::with(arena, Profile::classic())
    }

    fn with(arena: &str, profile: Profile) -> Self {
        let session = Session::new(profile).unwrap();
        let mut game = session.new_game(build_arena(arena).unwrap()).unwrap();
        game.start();
        Self {
            session,
            game,
            script: None,
        }
    }

    fn exec(&mut self, cmd: Command) -> Result<Outcome, CommandError> {
        self.session.execute(
            cmd,
            &mut SimCx {
                game: &mut self.game,
                script: &mut self.script,
            },
        )
    }

    fn tick(&mut self, input: &InputFrame) -> TickReport {
        {
            let mut cx = SimCx {
                game: &mut self.game,
                script: &mut self.script,
            };
            self.session.reconcile(&mut cx).unwrap();
            self.session.before_tick(&mut cx);
        }
        let report = self.game.tick(input).unwrap();
        self.session
            .after_tick(&self.game, self.script.as_ref(), &report);
        report
    }

    fn idle(&mut self, ticks: usize) {
        for _ in 0..ticks {
            self.tick(&InputFrame::default());
        }
    }

    /// Sets `key` to `factor` times its Classic value and returns the value.
    fn scale(&mut self, key: &str, factor: f64) -> f64 {
        let value = classic(key) * factor;
        self.exec(Command::SetParam {
            key: key.to_owned(),
            value: TuneValue::Float(value),
        })
        .unwrap_or_else(|e| panic!("{key} x{factor}: {e}"));
        assert!(matches!(
            self.session.label(),
            ParamSetLabel::Modified { .. }
        ));
        value
    }
}

/// The Classic value of a float key.
fn classic(key: &str) -> f64 {
    match Catalog::classic().get(key).map(|info| info.classic.clone()) {
        Some(TuneValue::Float(v)) => v,
        Some(TuneValue::Int(v)) => v as f64,
        other => panic!("{key} is not a number: {other:?}"),
    }
}

fn forward() -> InputFrame {
    InputFrame {
        move_forward: 1.0,
        ..InputFrame::default()
    }
}

/// A full standing jump (jump held), as the read-outs report it.
fn standing_jump(lab: &mut Lab) -> JumpStats {
    lab.idle(10);
    assert!(lab.game.player().grounded);
    for i in 0..4000 {
        let report = lab.tick(&InputFrame {
            jump_pressed: i == 0,
            jump_held: true,
            ..InputFrame::default()
        });
        if i > 0 && report.events.landed.is_some() {
            return lab.session.telemetry().last_jump().unwrap();
        }
    }
    panic!("the jump never landed");
}

/// Horizontal speed after `ticks` ticks of holding forward on the runway.
fn cruise(lab: &mut Lab, sprint: bool, ticks: usize) -> f32 {
    let input = InputFrame {
        sprint_held: sprint,
        ..forward()
    };
    for _ in 0..ticks {
        lab.tick(&input);
    }
    assert!(lab.game.player().grounded, "still on the runway");
    lab.game.player().horizontal_speed()
}

/// Ticks of holding forward until the pawn has covered `distance` along +X.
fn ticks_to_cover(lab: &mut Lab, distance: f32) -> u32 {
    let from = lab.game.player().position.x;
    for tick in 1..=6000 {
        lab.tick(&forward());
        if lab.game.player().position.x - from >= distance {
            return tick;
        }
    }
    panic!("never covered {distance} uu");
}

/// Looks at `target` (one tick), then holds the grapple button for one
/// tick. `true` if the gun attached.
fn fire_at(lab: &mut Lab, target: Vec3) -> bool {
    let d = target - lab.game.eye_position();
    let p = *lab.game.player();
    lab.tick(&InputFrame {
        look_yaw_delta: d.y.atan2(d.x) - p.yaw,
        look_pitch_delta: d.z.atan2(d.truncate().length()) - p.pitch,
        ..InputFrame::default()
    });
    let report = lab.tick(&InputFrame {
        grapple_held: true,
        ..InputFrame::default()
    });
    report.events.gun.attached.is_some()
}

/// Attaches to `target` and keeps the button down until the gun lets go by
/// itself (the pawn reached the hook). The swing as the read-outs report it.
fn swing(lab: &mut Lab, target: Vec3) -> SwingStats {
    assert!(fire_at(lab, target), "the hook is in reach");
    let held = InputFrame {
        grapple_held: true,
        ..InputFrame::default()
    };
    for _ in 0..1200 {
        let report = lab.tick(&held);
        if report.events.gun.released.is_some() {
            return lab.session.telemetry().last_swing().unwrap();
        }
    }
    panic!("the gun never let go");
}

// ---------------------------------------------------------------------------
// Movement.
// ---------------------------------------------------------------------------

#[test]
fn jump_velocity_and_gravity_scale_change_the_jump() {
    let base = standing_jump(&mut Lab::on(MOVEMENT_LAB));

    let mut strong = Lab::on(MOVEMENT_LAB);
    strong.scale("movement.jump_velocity", 1.5);
    let strong = standing_jump(&mut strong);

    let mut weak = Lab::on(MOVEMENT_LAB);
    weak.scale("movement.jump_velocity", 0.5);
    let weak = standing_jump(&mut weak);

    let mut floaty = Lab::on(MOVEMENT_LAB);
    floaty.scale("movement.custom_gravity_scaling", 0.5);
    let floaty = standing_jump(&mut floaty);

    let mut heavy = Lab::on(MOVEMENT_LAB);
    heavy.scale("movement.custom_gravity_scaling", 2.0);
    let heavy = standing_jump(&mut heavy);

    println!(
        "MEASURE jump apex uu: classic {:.1} | jump x1.5 {:.1} | jump x0.5 {:.1} | gravity x0.5 \
         {:.1} | gravity x2 {:.1}",
        base.apex_height,
        strong.apex_height,
        weak.apex_height,
        floaty.apex_height,
        heavy.apex_height
    );
    println!(
        "MEASURE jump airtime s: classic {:.3} | jump x1.5 {:.3} | jump x0.5 {:.3} | gravity \
         x0.5 {:.3} | gravity x2 {:.3}",
        base.airtime, strong.airtime, weak.airtime, floaty.airtime, heavy.airtime
    );
    assert!(strong.apex_height > base.apex_height);
    assert!(strong.takeoff_speed > base.takeoff_speed);
    assert!(weak.apex_height < base.apex_height);
    assert!(floaty.apex_height > base.apex_height);
    assert!(floaty.airtime > base.airtime);
    assert!(heavy.apex_height < base.apex_height);
    assert!(heavy.airtime < base.airtime);
}

#[test]
fn walking_and_sprint_speed_change_how_fast_the_runway_is_covered() {
    let mut base = Lab::on(MOVEMENT_LAB);
    let base_walk = cruise(&mut base, false, 180);
    let mut base_sprint = Lab::on(MOVEMENT_LAB);
    let base_sprint = cruise(&mut base_sprint, true, 180);

    let mut fast = Lab::on(MOVEMENT_LAB);
    fast.scale("pawn.move_speed", 1.5);
    let fast_walk = cruise(&mut fast, false, 180);

    let mut slow = Lab::on(MOVEMENT_LAB);
    slow.scale("pawn.move_speed", 0.5);
    let slow_walk = cruise(&mut slow, false, 180);

    let mut sprinter = Lab::on(MOVEMENT_LAB);
    sprinter.scale("pawn.sprint_speed_multiplier", 1.5);
    let sprinter_walk = cruise(&mut sprinter, false, 180);
    let mut sprinter = Lab::on(MOVEMENT_LAB);
    sprinter.scale("pawn.sprint_speed_multiplier", 1.5);
    let sprinter_sprint = cruise(&mut sprinter, true, 180);

    println!(
        "MEASURE ground speed uu/s: walk classic {base_walk:.1} | move_speed x1.5 {fast_walk:.1} \
         | move_speed x0.5 {slow_walk:.1} || sprint classic {base_sprint:.1} | sprint mult x1.5 \
         {sprinter_sprint:.1} (walking with it {sprinter_walk:.1})"
    );
    assert!(base_sprint > base_walk, "sprinting is faster than walking");
    assert!(fast_walk > base_walk * 1.2);
    assert!(slow_walk < base_walk * 0.8);
    assert!(sprinter_sprint > base_sprint * 1.2);
    assert_eq!(
        sprinter_walk, base_walk,
        "the sprint multiplier leaves walking alone"
    );

    // The same seen as time: a stretch of four seconds of Classic walking.
    let stretch = 4.0 * base_walk;
    let base_ticks = ticks_to_cover(&mut Lab::on(MOVEMENT_LAB), stretch);
    let mut fast = Lab::on(MOVEMENT_LAB);
    fast.scale("pawn.move_speed", 1.5);
    let fast_ticks = ticks_to_cover(&mut fast, stretch);
    println!(
        "MEASURE ticks to cover {stretch:.0} uu from a standstill: classic {base_ticks} | \
         move_speed x1.5 {fast_ticks}"
    );
    assert!(fast_ticks < base_ticks);
}

#[test]
fn a_speed_change_reaches_a_pawn_that_is_already_walking() {
    // The point of live tuning: no respawn, no restart.
    let mut lab = Lab::on(MOVEMENT_LAB);
    let before = cruise(&mut lab, false, 120);
    lab.scale("pawn.move_speed", 1.5);
    let after = cruise(&mut lab, false, 120);
    lab.exec(Command::ResetAllParams).unwrap();
    assert_eq!(lab.session.label(), ParamSetLabel::Classic);
    let back = cruise(&mut lab, false, 120);
    println!(
        "MEASURE live retune while walking uu/s: {before:.1} -> {after:.1} -> reset {back:.1}"
    );
    assert!(after > before * 1.2);
    // Slowing down to the Classic speed approaches it from the other side
    // than speeding up did, so the two differ in the last digits.
    assert!(
        (back - before).abs() < 0.01 * before,
        "reset returns the Classic walking speed: {back} vs {before}"
    );
}

#[test]
fn air_control_changes_how_far_a_jump_can_be_steered() {
    // Jump on the spot, then hold forward in the air.
    let drift = |lab: &mut Lab| {
        lab.idle(10);
        let from = lab.game.player().position;
        for i in 0..4000 {
            let report = lab.tick(&InputFrame {
                jump_pressed: i == 0,
                jump_held: true,
                move_forward: if i > 0 { 1.0 } else { 0.0 },
                ..InputFrame::default()
            });
            if i > 0 && report.events.landed.is_some() {
                break;
            }
        }
        (lab.game.player().position - from).truncate().length()
    };
    let base = drift(&mut Lab::on(MOVEMENT_LAB));
    // The pawn's air control is the landed value from its first landing on,
    // and the take-off value before it: tune both, as a user would.
    let mut more = Lab::on(MOVEMENT_LAB);
    more.scale("movement.air_control", 2.0);
    more.scale("pawn.landed_air_control", 2.0);
    let more = drift(&mut more);
    let mut less = Lab::on(MOVEMENT_LAB);
    less.scale("movement.air_control", 0.25);
    less.scale("pawn.landed_air_control", 0.25);
    let less = drift(&mut less);
    println!(
        "MEASURE air drift of a standing jump uu: classic {base:.1} | air control x2 {more:.1} | \
         x0.25 {less:.1}"
    );
    assert!(more > base);
    assert!(less < base);
}

#[test]
fn step_height_decides_which_stairs_are_climbed() {
    // The tallest stair lane: risers above the Classic step height.
    let lane = STAIR_RISERS.len() - 1;
    let riser = STAIR_RISERS[lane] * classic("movement.step_height") as f32;
    let climb = |lab: &mut Lab| {
        let feet = stair_lane_start(lane).unwrap();
        let half_height = lab.game.params().movement.capsule_half_height.value;
        lab.exec(Command::Teleport {
            to: TeleportTarget::Position {
                position: (feet + Vec3::Z * (half_height + 1.0)).to_array(),
                yaw: Some(0.0),
                pitch: Some(0.0),
            },
        })
        .unwrap();
        lab.idle(30);
        let floor = lab.game.player().position.z;
        let mut top = floor;
        // Long enough to reach the stairs, short enough not to walk off
        // their far end.
        for _ in 0..150 {
            lab.tick(&forward());
            top = top.max(lab.game.player().position.z);
        }
        top - floor
    };
    let base = climb(&mut Lab::on(MOVEMENT_LAB));
    let mut tall = Lab::on(MOVEMENT_LAB);
    tall.scale("movement.step_height", 2.0);
    let tall = climb(&mut tall);
    println!(
        "MEASURE height gained on the {}x-riser stairs uu: classic {base:.1} | step_height x2 \
         {tall:.1} (one riser is {riser:.1})",
        STAIR_RISERS[lane]
    );
    assert!(base < 0.5 * riser, "Classic is stopped by the tall risers");
    assert!(tall > 0.9 * riser, "a doubled step height climbs them");
}

// ---------------------------------------------------------------------------
// Grapple.
// ---------------------------------------------------------------------------

#[test]
fn grapple_reach_decides_which_hooks_attach() {
    // The fan's last hook is just outside the Classic reach on purpose.
    let last = RANGE_HOOK_FRACTIONS.len() - 1;
    let inside = range_hook(2).unwrap();
    let outside = range_hook(last).unwrap();

    let mut base = Lab::on(GRAPPLE_LAB);
    base.idle(10);
    let base_outside = fire_at(&mut base, outside);
    let mut base = Lab::on(GRAPPLE_LAB);
    base.idle(10);
    let base_inside = fire_at(&mut base, inside);

    let mut long = Lab::on(GRAPPLE_LAB);
    long.scale("gun.max_distance", 2.0);
    long.idle(10);
    let long_outside = fire_at(&mut long, outside);

    let mut short = Lab::on(GRAPPLE_LAB);
    short.scale("gun.max_distance", 0.5);
    short.idle(10);
    let short_inside = fire_at(&mut short, inside);

    println!(
        "MEASURE attach: hook at {}x reach: classic {base_inside}, reach x0.5 {short_inside} || \
         hook at {}x reach: classic {base_outside}, reach x2 {long_outside}",
        RANGE_HOOK_FRACTIONS[2], RANGE_HOOK_FRACTIONS[last]
    );
    assert!(base_inside && !base_outside);
    assert!(long_outside, "twice the reach attaches the far hook");
    assert!(
        !short_inside,
        "half the reach no longer attaches the 0.75x hook"
    );
}

#[test]
fn grapple_acceleration_changes_the_swing() {
    // The half-range hook, pulled until the gun lets go.
    let hook = range_hook(1).unwrap();
    let run = |factor: Option<f64>| {
        let mut lab = Lab::on(GRAPPLE_LAB);
        if let Some(factor) = factor {
            lab.scale("gun.grapple_accel", factor);
        }
        lab.idle(10);
        swing(&mut lab, hook)
    };
    let base = run(None);
    let strong = run(Some(2.0));
    let weak = run(Some(0.5));
    println!(
        "MEASURE pull to the {}x hook ({:.0} uu): peak speed uu/s classic {:.0} | accel x2 {:.0} \
         | accel x0.5 {:.0} || duration s classic {:.2} | x2 {:.2} | x0.5 {:.2}",
        RANGE_HOOK_FRACTIONS[1],
        base.attach_distance,
        base.peak_speed,
        strong.peak_speed,
        weak.peak_speed,
        base.duration,
        strong.duration,
        weak.duration
    );
    assert!(strong.peak_speed > base.peak_speed);
    assert!(strong.duration < base.duration);
    assert!(weak.peak_speed < base.peak_speed);
    assert!(weak.duration > base.duration);
}

// ---------------------------------------------------------------------------
// Camera.
// ---------------------------------------------------------------------------

#[test]
fn field_of_view_follows_at_once() {
    let mut lab = Lab::on(MOVEMENT_LAB);
    lab.idle(5);
    let before = lab.game.fov();
    let wanted = lab.scale("camera.fov_degrees", 1.25) as f32;
    let at_once = lab.game.fov();
    lab.idle(5);
    let after = lab.game.fov();
    println!("MEASURE fov deg: classic {before} | x1.25 at once {at_once} | after 5 ticks {after}");
    assert_eq!(at_once, wanted, "no tick needed");
    assert_eq!(after, wanted);
    assert!(after > before);
}

// ---------------------------------------------------------------------------
// The keys the quick tuner starts with, and the other abilities.
// ---------------------------------------------------------------------------

#[test]
fn after_the_first_landing_air_steering_is_the_landed_air_control() {
    // The pawn copies `movement.air_control` when it starts and
    // `pawn.landed_air_control` at every landing. So once it has landed,
    // the second key is the one that steers a jump; the first one only
    // reaches a pawn that has not landed yet (after a spawn or respawn).
    let second_jump_drift = |key: Option<&str>| {
        let mut lab = Lab::on(MOVEMENT_LAB);
        // A first jump, so that the pawn has landed once.
        standing_jump(&mut lab);
        if let Some(key) = key {
            lab.scale(key, 2.0);
        }
        lab.idle(10);
        let from = lab.game.player().position;
        for i in 0..4000 {
            let report = lab.tick(&InputFrame {
                jump_pressed: i == 0,
                jump_held: true,
                move_forward: if i > 0 { 1.0 } else { 0.0 },
                ..InputFrame::default()
            });
            if i > 0 && report.events.landed.is_some() {
                break;
            }
        }
        (lab.game.player().position - from).truncate().length()
    };
    let base = second_jump_drift(None);
    let landed = second_jump_drift(Some("pawn.landed_air_control"));
    let start = second_jump_drift(Some("movement.air_control"));
    println!(
        "MEASURE air drift of the second jump uu: classic {base:.1} | pawn.landed_air_control x2 \
         {landed:.1} | movement.air_control x2 {start:.1}"
    );
    assert!(landed > base, "the landed value steers every later jump");
    assert_eq!(start, base, "the start value no longer reaches this pawn");
}

#[test]
fn boost_strength_changes_the_rocket_boost() {
    // Jump, press jump again in the air (the rocket boots), and ride the
    // boost with forward held: the top speed reached.
    let boost = |factor: Option<f64>| {
        let mut lab = Lab::on(MOVEMENT_LAB);
        if let Some(factor) = factor {
            lab.scale("boots.boost_strength", factor);
        }
        lab.idle(10);
        let mut started = false;
        let mut top = 0.0_f32;
        for i in 0..600 {
            let report = lab.tick(&InputFrame {
                jump_pressed: i == 0 || i == 20,
                jump_held: i < 40,
                ..forward()
            });
            started |= report.events.boots.is_some();
            top = top.max(lab.game.player().speed());
            if i > 20 && report.events.landed.is_some() {
                break;
            }
        }
        assert!(started, "the boots answered the second press");
        top
    };
    let base = boost(None);
    let strong = boost(Some(2.0));
    let weak = boost(Some(0.25));
    println!(
        "MEASURE top speed of a rocket boost uu/s: classic {base:.0} | boost_strength x2 \
         {strong:.0} | x0.25 {weak:.0}"
    );
    assert!(strong > base);
    assert!(weak < base);
}

#[test]
fn power_jump_strength_changes_the_power_jump() {
    // Hold the power-jump button past its charge time, let go, and measure
    // the jump the read-outs report.
    let power_jump = |factor: Option<f64>| {
        let mut lab = Lab::on(MOVEMENT_LAB);
        if let Some(factor) = factor {
            lab.scale("pawn.power_jump_strength", factor);
        }
        lab.idle(10);
        let charge = lab.game.params().pawn.as_ref().unwrap();
        let ticks =
            (charge.power_jump_charge_time.value / lab.game.clock().dt()).ceil() as usize + 10;
        for _ in 0..ticks {
            lab.tick(&InputFrame {
                power_jump_held: true,
                ..InputFrame::default()
            });
        }
        for i in 0..4000 {
            let report = lab.tick(&InputFrame::default());
            if i > 0 && report.events.landed.is_some() {
                return lab.session.telemetry().last_jump().unwrap();
            }
        }
        panic!("the power jump never landed");
    };
    let plain = standing_jump(&mut Lab::on(MOVEMENT_LAB));
    let base = power_jump(None);
    let strong = power_jump(Some(1.5));
    println!(
        "MEASURE power jump apex uu: plain jump {:.1} | power jump classic {:.1} | \
         power_jump_strength x1.5 {:.1}",
        plain.apex_height, base.apex_height, strong.apex_height
    );
    assert!(base.apex_height > plain.apex_height);
    assert!(strong.apex_height > base.apex_height);
}

// ---------------------------------------------------------------------------
// A session from a profile file, and the save-state loop.
// ---------------------------------------------------------------------------

#[test]
fn the_example_profile_plays_as_it_says() {
    // The committed example: half the gravity scale, zoom off, unlimited
    // grapples, boots on.
    let bytes = std::fs::read(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/examples/profiles/floaty.json"
    ))
    .unwrap();
    let floaty = Profile::from_json_slice(&bytes).unwrap();
    assert_eq!(floaty.name, "floaty");

    let base = standing_jump(&mut Lab::on(MOVEMENT_LAB));
    let mut lab = Lab::with(MOVEMENT_LAB, floaty);
    let jump = standing_jump(&mut lab);
    println!(
        "MEASURE example profile, jump apex uu: classic {:.1} | floaty {:.1}",
        base.apex_height, jump.apex_height
    );
    assert!(jump.apex_height > 1.5 * base.apex_height);
    let script = &lab.game.player().script;
    assert!(!script.zoom_enabled);
    assert!(script.boots.enabled);
    assert!(script.gun.max_grapples >= asamu_player::grapple_gun::UNLIMITED_GRAPPLES);
    assert!(matches!(
        lab.session.label(),
        ParamSetLabel::Modified { overrides: 2 }
    ));
    assert!(lab.session.banner(&lab.game).contains("MODIFIED"));
}

#[test]
fn save_change_one_number_load_retry() {
    use asamu_sandbox::command::SlotOp;
    // The loop the save states are for: the same jump from the same state,
    // under two values, without restarting anything.
    let mut lab = Lab::on(MOVEMENT_LAB);
    lab.idle(20);
    lab.exec(Command::Slot {
        op: SlotOp::Save { slot: 0 },
    })
    .unwrap();
    let at_save = *lab.game.player();
    let first = standing_jump(&mut lab);

    lab.scale("movement.jump_velocity", 1.5);
    let outcome = lab
        .exec(Command::Slot {
            op: SlotOp::Load { slot: 0 },
        })
        .unwrap();
    assert!(outcome.discontinuity);
    assert_eq!(lab.game.player().position, at_save.position);
    assert_eq!(lab.game.clock().tick(), 20);
    let second = standing_jump(&mut lab);
    println!(
        "MEASURE save, tune, load, retry: apex {:.1} uu, then {:.1} uu from the same state",
        first.apex_height, second.apex_height
    );
    assert!(second.apex_height > first.apex_height);
}

// ---------------------------------------------------------------------------
// Messing around is safe.
// ---------------------------------------------------------------------------

/// Values a curious user might try for `info`: nothing, tiny, huge, the
/// largest number the parameter can hold, the other switch position.
fn wild_values(info: &asamu_sandbox::keys::KeyInfo) -> Vec<TuneValue> {
    use asamu_sandbox::keys::ValueKind;
    match (info.kind, &info.classic) {
        (ValueKind::Float, TuneValue::Float(classic)) => [
            0.0,
            classic * 1.0e-3,
            classic * 1.0e3,
            -classic,
            f64::from(f32::MAX),
            f64::from(f32::MIN_POSITIVE),
        ]
        .into_iter()
        .map(TuneValue::Float)
        .collect(),
        (ValueKind::Int, _) => [-1, 0, 1, i64::from(i32::MAX), i64::from(i32::MIN)]
            .into_iter()
            .map(TuneValue::Int)
            .collect(),
        (ValueKind::Bool, TuneValue::Bool(on)) => vec![TuneValue::Bool(!on)],
        (ValueKind::Choice(names), _) => names
            .iter()
            .map(|name| TuneValue::Text((*name).to_owned()))
            .collect(),
        _ => Vec::new(),
    }
}

/// A busy input for tick `tick`: forward, sprinting every other cycle, a
/// held jump, a second press in the air (the boots), a power-jump charge,
/// then a look at the nearest grapple target and a held grapple.
fn busy_input(game: &Game, tick: usize) -> InputFrame {
    let phase = tick % 240;
    let player = game.player();
    let mut input = InputFrame {
        sprint_held: (tick / 240) % 2 == 1,
        ..forward()
    };
    match phase {
        30 => {
            input.jump_pressed = true;
            input.jump_held = true;
        }
        31..=44 => input.jump_held = true,
        45 => input.jump_pressed = !player.grounded,
        100..=149 => input.power_jump_held = true,
        160 => {
            let eye = game.eye_position();
            let nearest = game
                .level()
                .grapple_points
                .iter()
                .map(|point| point.position)
                .min_by(|a, b| a.distance(eye).total_cmp(&b.distance(eye)));
            if let Some(target) = nearest {
                let to = target - eye;
                input.look_yaw_delta = to.y.atan2(to.x) - player.yaw;
                input.look_pitch_delta = to.z.atan2(to.truncate().length()) - player.pitch;
            }
        }
        161..=220 => input.grapple_held = true,
        _ => {}
    }
    input
}

#[test]
fn any_accepted_value_plays_on_and_a_reset_and_respawn_recover() {
    // Whatever the overlay accepts for a key, set in the middle of a busy
    // run on each arena: the session and the game carry on without a panic,
    // the player's state stays finite (the simulation refuses a step that
    // would not be), and "reset all, respawn" gives a pawn that stands and
    // plays under the Classic set again.
    let (mut accepted, mut refused, mut stalled) = (0_u32, 0_u32, Vec::new());
    for arena in [MOVEMENT_LAB, GRAPPLE_LAB] {
        for info in Catalog::classic().iter() {
            for value in wild_values(info) {
                let mut lab = Lab::on(arena);
                for tick in 0..60 {
                    let input = busy_input(&lab.game, tick);
                    lab.tick(&input);
                }
                let set = lab.exec(Command::SetParam {
                    key: info.key.clone(),
                    value: value.clone(),
                });
                if set.is_err() {
                    refused += 1;
                    assert_eq!(lab.session.label(), ParamSetLabel::Classic);
                    continue;
                }
                accepted += 1;
                let what = format!("{arena}: {} = {value}", info.key);
                let mut rejected_steps = 0_u32;
                for tick in 60..360 {
                    let input = busy_input(&lab.game, tick);
                    let report = lab.tick(&input);
                    rejected_steps += u32::from(report.events.non_finite_rejected);
                    assert!(lab.game.player().is_finite(), "{what}: tick {tick}");
                }
                if rejected_steps > 0 {
                    stalled.push(format!("{what} ({rejected_steps} steps refused)"));
                }

                lab.exec(Command::ResetAllParams).unwrap();
                lab.exec(Command::Respawn).unwrap();
                assert_eq!(lab.session.label(), ParamSetLabel::Classic, "{what}");
                assert_eq!(
                    *lab.game.params(),
                    asamu_player::PlayerParams::asamu_original(),
                    "{what}"
                );
                // The respawn keeps what the pawn was in the middle of (a
                // charged power jump still fires), so: until it stands.
                let mut stands = false;
                for _ in 0..1200 {
                    let report = lab.tick(&InputFrame::default());
                    assert!(
                        !report.events.non_finite_rejected,
                        "{what}: after the reset"
                    );
                    let player = lab.game.player();
                    stands = player.grounded && player.velocity == Vec3::ZERO;
                    if stands {
                        break;
                    }
                }
                assert!(stands, "{what}: stands after the reset");
            }
        }
    }
    println!(
        "MEASURE wild values: {accepted} accepted and played, {refused} refused by the Classic \
         validation; the simulation refused steps under {} of the accepted ones: {stalled:?}",
        stalled.len()
    );
    assert!(accepted > 0 && refused > 0);
}
