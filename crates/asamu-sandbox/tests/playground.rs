//! The playground, played by the simulation itself: it loads through the
//! game's own loader without any game data, the pawn has its full suit, the
//! things that turn and move do so by the game layer's rules and carry a
//! pawn and a grapple's anchor, every route can be done by a simple pilot,
//! the catch returns the pawn to the hub, and a session on it keeps its
//! promises (labels, timed routes, save states, a pristine session changes
//! nothing).
//!
//! Everything measured here is this recreation running the Classic set on a
//! level of ours. It says nothing about the original. The pilots are crude
//! on purpose (aim, hold, let go, steer at the landing): a route they finish
//! is one a player can.
#![allow(clippy::unwrap_used)]

use asamu_game::{Game, LevelScript, TickReport};
use asamu_player::InputFrame;
use asamu_sandbox::command::{Command, SlotOp, TeleportTarget, Toggle};
use asamu_sandbox::inspect::Inspection;
use asamu_sandbox::playground::{
    self, CAROUSEL_CENTRE, CAROUSEL_RADIUS, CAROUSEL_RATE, CAROUSEL_ROCKS, CATCH_CENTRE, FAR_AIR,
    FAR_EDGE, FAR_SHOT_FRACTION, FAR_SHOTS, FERRY_CROSSING, FERRY_TRAVEL, FERRY_WAIT, HUB_RADIUS,
    HUB_START, MEDIUM_CHAIN, NEAR_CHAIN, PLAYGROUND_GRAPPLES, PLAYGROUND_LEVEL_NAME,
    PLAYGROUND_MAP, RUNWAY_Y, Role, TOWER_BASE, TOWER_HEIGHT, TOWER_HOOK, TOWER_LEDGES,
    WHEEL_CENTRE, WHEEL_GONDOLAS, WHEEL_RADIUS, WHEEL_RATE, YARD, load_playground, playground,
};
use asamu_sandbox::session::{Outcome, Session, SimCx};
use glam::Vec3;

const TICKS_PER_SECOND: usize = 60;

/// A started game on the playground with its script and a pristine session
/// around every tick, the way the app runs one.
struct Rig {
    game: Game,
    script: Option<LevelScript>,
    session: Session,
}

impl Rig {
    fn new() -> Self {
        let (mut game, script) = load_playground().unwrap();
        game.start();
        let mut rig = Self {
            game,
            script: Some(script),
            session: Session::classic(),
        };
        rig.idle(5);
        rig
    }

    fn tick(&mut self, input: &InputFrame) -> TickReport {
        self.session.before_tick(&mut SimCx {
            game: &mut self.game,
            script: &mut self.script,
        });
        let report = self
            .script
            .as_mut()
            .unwrap()
            .tick(&mut self.game, input)
            .unwrap()
            .report;
        self.session
            .after_tick(&self.game, self.script.as_ref(), &report);
        report
    }

    fn idle(&mut self, ticks: usize) {
        for _ in 0..ticks {
            self.tick(&InputFrame::default());
        }
    }

    fn run(&mut self, command: Command) -> Outcome {
        self.session
            .execute(
                command,
                &mut SimCx {
                    game: &mut self.game,
                    script: &mut self.script,
                },
            )
            .unwrap()
    }

    /// Places the pawn on the floor point `feet`, looking along `yaw`, and
    /// lets it settle.
    fn stand_at(&mut self, feet: Vec3, yaw: f32) {
        let lift = self.game.params().movement.capsule_half_height.value + 2.0;
        self.run(Command::Teleport {
            to: TeleportTarget::Position {
                position: (feet + Vec3::Z * lift).to_array(),
                yaw: Some(yaw),
                pitch: Some(0.0),
            },
        });
        self.idle(10);
    }

    /// The floor point under the pawn.
    fn feet(&self) -> Vec3 {
        self.game.player().position
            - Vec3::Z * self.game.params().movement.capsule_half_height.value
    }

    /// Seconds of simulated time so far.
    fn time(&self) -> f64 {
        self.game.clock().tick() as f64 * self.game.clock().dt_f64()
    }

    /// Turns the view to look at `target` from the eye.
    fn aim_at(&mut self, target: Vec3) {
        let d = target - self.game.eye_position();
        let player = self.game.player_mut();
        player.yaw = d.y.atan2(d.x);
        player.pitch = (d.z / d.length().max(1.0)).clamp(-1.0, 1.0).asin();
    }

    /// Where the moving piece `name` is now (its shape's origin) and how it
    /// is turned.
    fn transform_of(&self, name: &str) -> (Vec3, [i32; 3]) {
        let piece = playground().piece(name).unwrap();
        self.script
            .as_ref()
            .unwrap()
            .mover_transform(piece.actor.unwrap())
            .unwrap()
    }

    fn where_is(&self, name: &str) -> Vec3 {
        self.transform_of(name).0
    }

    /// Holds the grapple on the target `at` names (asked every tick, so it
    /// may move) until the gun lets go, or until the pawn is within
    /// `let_go_at` of it. Ends with one tick without the button. `false`:
    /// it never attached.
    fn pull(&mut self, at: &dyn Fn(&Rig) -> Vec3, let_go_at: f32) -> bool {
        let mut attached = false;
        for i in 0..900 {
            let target = at(self);
            self.aim_at(target);
            self.tick(&InputFrame {
                grapple_held: true,
                ..InputFrame::default()
            });
            let anchored = self.game.player().script.gun.anchor().is_some();
            let near = (at(self) - self.game.player().position).length() < let_go_at;
            if anchored && !near {
                attached = true;
            } else if attached || anchored {
                attached = true;
                break;
            } else if i > 30 {
                break;
            }
        }
        self.tick(&InputFrame::default());
        attached
    }

    /// Steers at the point `at` names (forward held, in the air or on the
    /// ground) until the pawn stands. `false`: it did not within `ticks`,
    /// or it died.
    fn land_at(&mut self, at: &dyn Fn(&Rig) -> Vec3, ticks: usize) -> bool {
        for _ in 0..ticks {
            let d = at(self) - self.game.player().position;
            self.game.player_mut().yaw = d.y.atan2(d.x);
            let forward = if d.truncate().length() < 50.0 {
                0.0
            } else {
                1.0
            };
            let report = self.tick(&InputFrame {
                move_forward: forward,
                ..InputFrame::default()
            });
            if report.respawned {
                return false;
            }
            if self.game.player().grounded {
                return true;
            }
        }
        false
    }

    /// Walks towards `point` until within 40 uu of it in plan.
    fn walk_to(&mut self, point: Vec3) {
        for _ in 0..1800 {
            let d = point - self.game.player().position;
            if d.truncate().length() < 40.0 {
                break;
            }
            self.game.player_mut().yaw = d.y.atan2(d.x);
            self.game.player_mut().pitch = 0.0;
            self.tick(&InputFrame {
                move_forward: 1.0,
                ..InputFrame::default()
            });
        }
        self.idle(20);
    }

    /// Whether the pawn stands on a flat top centred on `top` that reaches
    /// `radius` from it.
    fn stands_on(&self, top: Vec3, radius: f32) -> bool {
        let feet = self.feet();
        self.game.player().grounded
            && (feet.z - top.z).abs() < 12.0
            && (feet - top).truncate().length() < radius
    }

    fn grapples_used(&self) -> i32 {
        self.game.player().script.gun.times_grappled
    }

    fn reach(&self) -> f32 {
        self.game
            .params()
            .gun
            .as_ref()
            .map_or(0.0, |gun| gun.max_distance.value)
    }
}

fn piece_location(name: &str) -> Vec3 {
    playground().piece(name).unwrap().location
}

fn yaw_to(from: Vec3, to: Vec3) -> f32 {
    (to - from).y.atan2((to - from).x)
}

#[test]
fn it_loads_through_the_games_loader_without_game_data() {
    let (game, script) = load_playground().unwrap();
    assert_eq!(game.map_name(), Some(PLAYGROUND_MAP));
    assert!(playground::is_playground(&game));
    assert_eq!(playground::level_label(&game), PLAYGROUND_LEVEL_NAME);
    // Never a chapter of the game: the save system does not know it.
    assert_eq!(game.chapter(), None);
    let map = game.scene_map().unwrap();
    assert!(map.warnings.is_empty(), "{:?}", map.warnings);
    // The two hubs turn, and everything attached to them is carried.
    assert_eq!(script.turning_actors().len(), 2);
    let moving = playground()
        .pieces
        .iter()
        .filter(|p| !matches!(p.role, Role::Fixed | Role::Decor | Role::Crystal))
        .count();
    assert_eq!(script.mover_ids().len(), moving);
    // Two hubs, a rock and a hook per carousel place, a gondola and a hook
    // per wheel place, the ferry.
    assert_eq!(
        moving,
        2 + 2 * CAROUSEL_ROCKS as usize + 2 * WHEEL_GONDOLAS as usize + 1
    );
    // The data is gone again: nothing of it stays on disk. (A base of this
    // test's own: other tests load the playground at the same time.)
    let base = std::env::temp_dir().join(format!(
        "asamu-sandbox-playground-test-base-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(&base).unwrap();
    let (again, _) = playground::load_playground_under(&base).unwrap();
    assert!(playground::is_playground(&again));
    assert_eq!(std::fs::read_dir(&base).unwrap().count(), 0);
    // A place that cannot hold the data is an error, not a panic.
    assert!(playground::load_playground_under(&base.join("nothing-here")).is_err());
    std::fs::remove_dir_all(&base).unwrap();
}

#[test]
fn the_pawn_starts_on_the_hub_with_the_full_suit() {
    let mut rig = Rig::new();
    rig.idle(30);
    let player = rig.game.player();
    assert!(player.grounded, "standing on the hub");
    assert!((rig.feet() - HUB_START).length() < 30.0, "{}", rig.feet());
    assert_eq!(player.script.gun.max_grapples, PLAYGROUND_GRAPPLES);
    assert!(player.script.boots.enabled);
    let script = rig.script.as_ref().unwrap();
    assert!(
        script.host_errors().is_empty(),
        "{:?}",
        script.host_errors()
    );
    assert!(
        script.runtime().errors().is_empty(),
        "{:?}",
        script.runtime().errors()
    );
    // A pristine session is still pristine after watching all this.
    assert!(rig.session.is_pristine());
}

/// What the layout was fitted to: the jumps of the Classic set in this
/// recreation, measured on the runway and printed (`--nocapture`). The
/// asserts are the relations the level relies on, not the numbers.
#[test]
fn the_layout_fits_what_the_classic_set_does() {
    let mut measured = Vec::new();
    for (label, forward, sprint, power) in [
        ("standing jump", 0.0, false, false),
        ("walking jump", 1.0, false, false),
        ("sprinting jump", 1.0, true, false),
        ("power jump", 0.0, false, true),
        ("power leap", 1.0, true, true),
    ] {
        let mut rig = Rig::new();
        rig.stand_at(Vec3::new(-2000.0, RUNWAY_Y, 0.0), 0.0);
        let run = InputFrame {
            move_forward: forward,
            sprint_held: sprint,
            ..InputFrame::default()
        };
        // Up to speed, the power jump charged meanwhile.
        for _ in 0..120 {
            rig.tick(&InputFrame {
                power_jump_held: power,
                ..run
            });
        }
        let from = rig.game.player().position;
        let (mut apex, mut ticks) = (0.0f32, 0);
        for i in 0..2000 {
            rig.tick(&InputFrame {
                jump_pressed: i == 0 && !power,
                jump_held: !power,
                ..run
            });
            apex = apex.max(rig.game.player().position.z - from.z);
            ticks = i;
            if i > 8 && rig.game.player().grounded {
                break;
            }
        }
        let far = rig.game.player().position.x - from.x;
        eprintln!(
            "MEASURE {label}: apex {apex:.0} uu, {:.2} s, {far:.0} uu far",
            ticks as f32 / TICKS_PER_SECOND as f32
        );
        measured.push((apex, far));
    }
    let (jump_apex, _) = measured[0];
    let (_, walking_far) = measured[1];
    let (power_apex, _) = measured[3];
    let (_, leap_far) = measured[4];
    // The tower's ledges are a jump apart, its top is not, and a power jump
    // reaches it.
    let step = TOWER_LEDGES[0];
    assert!(TOWER_LEDGES.windows(2).all(|w| w[1] - w[0] <= step));
    let last = TOWER_HEIGHT - TOWER_LEDGES[TOWER_LEDGES.len() - 1];
    assert!(jump_apex > step && jump_apex < last && power_apex > last);
    // The near chain: gaps of 520 uu for a walking jump, 1,700 for the
    // leap, and a step of 880 for the power jump.
    let rise = NEAR_CHAIN[4].0.z - NEAR_CHAIN[3].0.z;
    assert!(walking_far > 520.0 * 1.3 && leap_far > 1700.0 * 1.2 && power_apex > rise * 1.2);
    assert!(
        walking_far < 1700.0,
        "the wide gap is wide to a walking jump"
    );
}

/// The near chain without the grapple: three walking jumps, the leap over
/// the wide gap, the power jump up the last step; and the route's clock
/// times it from the arch to the frame.
#[test]
fn the_near_chain_can_be_done_on_foot_and_is_timed() {
    let mut rig = Rig::new();
    let near = playground()
        .courses
        .iter()
        .position(|c| c.id == "near")
        .unwrap();
    // Under the start arch: the clock is ready, and does not run yet.
    let mut from = Vec3::new(1250.0, 0.0, 0.0);
    rig.stand_at(from, 0.0);
    let clock = rig.game.clock();
    assert_eq!(
        rig.session.course().running(clock.tick(), clock.dt()),
        Some((near, 0.0))
    );
    for (i, (top, radius)) in NEAR_CHAIN.into_iter().enumerate() {
        let yaw = yaw_to(from, top);
        let leap = i == 3;
        let power = i == 4;
        // Where the jump is made: this far from the landing's centre.
        let gap = if leap {
            1700.0
        } else if power {
            250.0
        } else {
            520.0
        };
        let jump_at = radius + gap + 70.0;
        let mut jumped = false;
        let mut charged = 0;
        for _ in 0..1200 {
            rig.game.player_mut().yaw = yaw;
            rig.game.player_mut().pitch = 0.0;
            let at_edge = (top - rig.feet()).truncate().length() < jump_at;
            let mut input = InputFrame {
                move_forward: 1.0,
                sprint_held: leap,
                ..InputFrame::default()
            };
            if power && !jumped {
                // Walk to the edge, stand, charge, let go.
                if at_edge {
                    charged += 1;
                    input.move_forward = 0.0;
                    input.power_jump_held = charged < 60;
                    jumped = charged >= 60;
                }
            } else if leap && !jumped {
                // Charge on the run, let go at the edge.
                charged += 1;
                input.power_jump_held = !(at_edge && charged > 45);
                jumped = !input.power_jump_held;
            } else if !jumped && at_edge && rig.game.player().grounded {
                input.jump_pressed = true;
                jumped = true;
            }
            input.jump_held = jumped && !leap && !power;
            let report = rig.tick(&input);
            if report.respawned || (jumped && rig.stands_on(top, radius)) {
                break;
            }
        }
        assert!(
            rig.stands_on(top, radius),
            "landing {} of the near chain on foot: feet at {}",
            i + 1,
            rig.feet()
        );
        from = top;
    }
    // Through the finish frame.
    let finish = playground().courses[near].finish;
    rig.walk_to((finish.min + finish.max) * 0.5);
    let run = rig.session.course().last().expect("the route was timed");
    assert_eq!(run.course, near);
    assert!(
        run.best && run.seconds > 5.0 && run.seconds < 120.0,
        "{run:?}"
    );
    assert_eq!(rig.session.course().best(near), Some(run.seconds));
    let line = rig.session.course().line(&rig.game).unwrap();
    assert!(line.starts_with("Near chain: "), "{line}");
    eprintln!("MEASURE near chain on foot: {line}");
    // A teleport ends an attempt without a time.
    rig.stand_at(Vec3::new(1250.0, 0.0, 0.0), 0.0);
    rig.walk_to(Vec3::new(1440.0, 0.0, 0.0));
    let clock = rig.game.clock();
    assert!(
        rig.session
            .course()
            .running(clock.tick(), clock.dt())
            .is_some_and(|(_, seconds)| seconds > 0.0)
    );
    rig.stand_at(HUB_START, 0.0);
    let clock = rig.game.clock();
    assert_eq!(rig.session.course().running(clock.tick(), clock.dt()), None);
    assert_eq!(rig.session.course().last(), Some(run));
}

/// The near and the medium chain by grapple: from each landing, a pull to
/// the hook over the next one, and the drop onto it.
#[test]
fn the_chains_can_be_done_by_grapple() {
    for (label, chain, first) in [
        (
            "near",
            NEAR_CHAIN.to_vec(),
            Vec3::new(HUB_RADIUS - 150.0, 0.0, 0.0),
        ),
        (
            "medium",
            MEDIUM_CHAIN.to_vec(),
            Vec3::new(0.0, HUB_RADIUS - 150.0, 0.0),
        ),
    ] {
        let mut rig = Rig::new();
        rig.stand_at(first, 0.0);
        let reach = rig.reach();
        for (i, (top, radius)) in chain.into_iter().enumerate() {
            let hook = piece_location(&format!("{label} hook {}", i + 1));
            let shot = (hook - rig.game.eye_position()).length();
            assert!(shot < reach, "{label} {}: a shot of {shot}", i + 1);
            if label == "medium" {
                // Far beyond a jump (the sprinting jump covers about 1100).
                assert!(shot > 3000.0, "{shot}");
            }
            assert!(rig.pull(&|_| hook, 0.0), "{label} hook {}", i + 1);
            assert!(
                rig.land_at(&|_| top, 600) && rig.stands_on(top, radius),
                "{label} landing {}: feet at {}",
                i + 1,
                rig.feet()
            );
            // A landing gives the budget back.
            rig.idle(5);
            assert_eq!(rig.grapples_used(), 0);
        }
    }
}

/// The far chain: shots at the edge of the reach, then the stretch without
/// a landing, where the crystals are what makes the budget last.
#[test]
fn the_far_chain_needs_its_crystals_and_can_be_done() {
    let mut rig = Rig::new();
    rig.stand_at(FAR_EDGE, -std::f32::consts::FRAC_PI_2);
    let reach = rig.reach();
    for i in 0..FAR_SHOTS {
        let hook = piece_location(&format!("far hook {}", i + 1));
        let top = piece_location(&format!("far island {}", i + 1));
        let shot = (hook - rig.game.eye_position()).length();
        assert!(
            shot < reach && shot > 0.9 * reach,
            "shot {} is {shot} of a reach of {reach} (meant: {FAR_SHOT_FRACTION})",
            i + 1
        );
        assert!(rig.pull(&|_| hook, 0.0), "far hook {}", i + 1);
        assert!(
            rig.land_at(&|_| top, 600) && rig.stands_on(top, 680.0),
            "far island {}: feet at {}",
            i + 1,
            rig.feet()
        );
        rig.walk_to(top + Vec3::new(0.0, -480.0, 0.0));
    }
    // In the air: every pull is let go early and the next thing is taken.
    let mut used = Vec::new();
    let last = FAR_AIR.len() - 1;
    for (i, is_crystal) in FAR_AIR.into_iter().enumerate() {
        let name = if is_crystal {
            format!("far crystal {}", i + 1)
        } else {
            format!("far air hook {}", i + 1)
        };
        let target = piece_location(&name);
        let let_go = if i == last { 0.0 } else { 1000.0 };
        assert!(
            rig.pull(&|_| target, let_go),
            "{name}: not taken from {} (used so far: {used:?})",
            rig.game.player().position
        );
        assert!(!rig.game.player().grounded, "{name}: no landing on the way");
        used.push(rig.grapples_used());
    }
    // Two hooks use two of the three; the crystal is the third and gives
    // them all back.
    assert_eq!(used, [1, 2, 0, 1, 2, 0, 1]);
    let end = piece_location("far end");
    assert!(
        rig.land_at(&|_| end, 900) && rig.stands_on(end, 950.0),
        "the far end: feet at {}",
        rig.feet()
    );
}

/// The tower: up by a jump per ledge and a power jump to the top; up by its
/// own hook from the bridge; and down, off the board, into the catch, which
/// wakes the pawn at the hub.
#[test]
fn the_tower_is_climbed_and_the_long_drop_ends_at_the_hub() {
    let mut rig = Rig::new();
    // Apex of a jump with the Classic set in this recreation, uu (measured
    // by `tuning_effects`; here only to say what a "jump apart" means).
    let apex_of_a_jump = 481.0;
    let first = piece_location("tower ledge 1");
    let out = (first - TOWER_BASE).truncate();
    rig.stand_at(TOWER_BASE + Vec3::new(-out.y, out.x, 0.0) * 1.15, 0.0);
    let mut below = 0.0;
    for (i, height) in TOWER_LEDGES.into_iter().enumerate() {
        assert!(
            height - below < apex_of_a_jump,
            "ledge {} is a jump up",
            i + 1
        );
        below = height;
        let ledge = piece_location(&format!("tower ledge {}", i + 1));
        rig.tick(&InputFrame {
            jump_pressed: true,
            jump_held: true,
            ..InputFrame::default()
        });
        let mut on = false;
        for _ in 0..400 {
            let d = ledge - rig.game.player().position;
            rig.game.player_mut().yaw = d.y.atan2(d.x);
            rig.tick(&InputFrame {
                move_forward: if d.truncate().length() < 50.0 {
                    0.0
                } else {
                    1.0
                },
                jump_held: true,
                ..InputFrame::default()
            });
            if rig.game.player().grounded {
                on = rig.stands_on(ledge, 260.0);
                break;
            }
        }
        assert!(on, "tower ledge {}: feet at {}", i + 1, rig.feet());
        // To the side of this ledge that the next thing does not cover.
        let next = playground()
            .piece(&format!("tower ledge {}", i + 2))
            .map_or(TOWER_BASE, |p| p.location);
        let away = (ledge - next).truncate().normalize_or_zero().extend(0.0);
        rig.walk_to(ledge + away * 150.0);
    }
    // The power jump: charge, let go, steer onto the top.
    let top = TOWER_BASE + Vec3::Z * TOWER_HEIGHT;
    assert!(
        TOWER_HEIGHT - below > apex_of_a_jump,
        "the top is more than a jump up"
    );
    for k in 0..75 {
        rig.tick(&InputFrame {
            power_jump_held: k < 65,
            ..InputFrame::default()
        });
    }
    assert!(
        rig.land_at(&|_| top, 600) && rig.stands_on(top, 330.0),
        "the tower's top by a power jump: feet at {}",
        rig.feet()
    );

    // By the hook, from the bridge: one long pull.
    let mut rig = Rig::new();
    rig.stand_at(TOWER_BASE + Vec3::new(-850.0, -850.0, 0.0), 0.0);
    let hook = TOWER_BASE + TOWER_HOOK;
    let shot = (hook - rig.game.eye_position()).length();
    assert!(shot < rig.reach() && shot > 0.85 * rig.reach(), "{shot}");
    assert!(rig.pull(&|_| hook, 0.0), "the tower's hook from the bridge");
    assert!(
        rig.land_at(&|_| top, 600) && rig.stands_on(top, 330.0),
        "the tower's top by its hook: feet at {}",
        rig.feet()
    );

    // Off the board: a long fall, the catch, the hub.
    rig.walk_to(TOWER_BASE + Vec3::new(250.0, 300.0, TOWER_HEIGHT));
    rig.walk_to(TOWER_BASE + Vec3::new(1100.0, 300.0, TOWER_HEIGHT));
    assert!(
        rig.stands_on(
            rig.feet().truncate().extend(TOWER_BASE.z + TOWER_HEIGHT),
            1.0
        ),
        "on the board: feet at {}",
        rig.feet()
    );
    rig.game.player_mut().yaw = 0.0;
    let mut left_at = None;
    let mut woke = None;
    let mut lowest = 0.0f32;
    for i in 0..(20 * TICKS_PER_SECOND) {
        let report = rig.tick(&InputFrame {
            move_forward: 1.0,
            ..InputFrame::default()
        });
        if report.respawned {
            woke = Some(i);
            break;
        }
        if left_at.is_none() && !rig.game.player().grounded {
            left_at = Some(i);
        }
        lowest = lowest.min(rig.feet().z);
    }
    let fell_for = (woke.expect("the catch ends the fall") - left_at.expect("it left the board"))
        as f32
        / TICKS_PER_SECOND as f32;
    let fell = TOWER_BASE.z + TOWER_HEIGHT - lowest;
    assert!(
        fell > 5000.0 && fell_for > 2.5,
        "a long drop: {fell} uu in {fell_for} s"
    );
    // Not the depths below the level: the catch.
    assert!(lowest > CATCH_CENTRE.z - 1500.0, "{lowest}");
    assert!(
        (rig.game.player().position - HUB_START).length() < 80.0,
        "{}",
        rig.game.player().position
    );
    eprintln!("MEASURE long drop: {fell:.0} uu in {fell_for:.1} s");
    // And straight into the catch from above it, without the tower.
    rig.stand_at(CATCH_CENTRE + Vec3::Z * 500.0, 0.0);
    let mut woke = false;
    for _ in 0..(5 * TICKS_PER_SECOND) {
        woke |= rig.tick(&InputFrame::default()).respawned;
    }
    assert!(woke);
    rig.idle(30);
    assert!(rig.stands_on(HUB_START, 60.0), "{}", rig.feet());
}

/// The runway: the sprint between its gates is timed, and each ramp throws
/// a sprinting pawn onto the landing beyond it.
#[test]
fn the_runway_is_timed_and_its_ramps_reach_their_landings() {
    let sprint = playground()
        .courses
        .iter()
        .position(|c| c.id == "sprint")
        .unwrap();
    for (label, from, heading, landing) in [
        (
            "ahead",
            Vec3::new(200.0, RUNWAY_Y, 0.0),
            0.0f32,
            "runway landing ahead",
        ),
        (
            "behind",
            Vec3::new(-500.0, RUNWAY_Y, 0.0),
            std::f32::consts::PI,
            "runway landing behind",
        ),
    ] {
        let mut rig = Rig::new();
        rig.stand_at(from, heading);
        let top = piece_location(landing);
        let mut left_the_ramp = false;
        let mut highest = 0.0f32;
        for _ in 0..(30 * TICKS_PER_SECOND) {
            rig.game.player_mut().yaw = heading;
            rig.game.player_mut().pitch = 0.0;
            let report = rig.tick(&InputFrame {
                move_forward: 1.0,
                sprint_held: true,
                ..InputFrame::default()
            });
            assert!(!report.respawned, "{label}: fell at {}", rig.feet());
            highest = highest.max(rig.feet().z);
            left_the_ramp |= highest > 300.0 && !rig.game.player().grounded;
            if left_the_ramp && rig.game.player().grounded {
                break;
            }
        }
        eprintln!(
            "MEASURE ramp {label}: up to {highest:.0} uu, down at {:.0}",
            rig.feet()
        );
        assert!(highest > 350.0, "{label}: the ramp was climbed ({highest})");
        assert!(
            rig.stands_on(top, 900.0),
            "{label}: feet at {}, the landing is at {top}",
            rig.feet()
        );
        if label == "ahead" {
            let run = rig.session.course().last().expect("the sprint was timed");
            assert_eq!(run.course, sprint);
            // 5,300 uu between the gates at the sprinting speed (880 uu/s
            // with the Classic set): about six seconds.
            assert!(run.seconds > 4.0 && run.seconds < 9.0, "{run:?}");
        }
    }
}

/// The carousel, the wheel and the ferry move by the game layer's own
/// rules, at the rates and on the timetable the level gives them.
#[test]
fn the_wheels_turn_and_the_ferry_keeps_its_timetable() {
    let mut rig = Rig::new();
    let before: Vec<Vec3> = (1..=CAROUSEL_ROCKS)
        .map(|i| rig.where_is(&format!("carousel rock {i}")))
        .collect();
    rig.idle(10 * TICKS_PER_SECOND);
    // The hubs have turned by their rate times the time, in whole units.
    let elapsed = rig.time();
    let turned = |rate: i32| (f64::from(rate) * elapsed) as i32;
    let carousel = rig.transform_of("carousel hub");
    assert_eq!(carousel.0, CAROUSEL_CENTRE);
    assert!(
        (carousel.1[1] - turned(CAROUSEL_RATE)).abs() <= 1,
        "{:?} against {}",
        carousel.1,
        turned(CAROUSEL_RATE)
    );
    let wheel = rig.transform_of("wheel hub");
    assert_eq!(wheel.0, WHEEL_CENTRE);
    assert!(
        (wheel.1[2] - turned(WHEEL_RATE)).abs() <= 1,
        "{:?} against {}",
        wheel.1,
        turned(WHEEL_RATE)
    );
    // The rocks went round with it: same height, same distance from the
    // axis, somewhere else, turned like the hub.
    for i in 1..=CAROUSEL_ROCKS {
        let (at, rotation) = rig.transform_of(&format!("carousel rock {i}"));
        let was = before[i as usize - 1];
        assert!((at.z - was.z).abs() < 0.5, "{at}");
        let arm = (at - CAROUSEL_CENTRE).truncate().length();
        assert!((arm - CAROUSEL_RADIUS).abs() < 1.0, "{arm}");
        assert!((at - was).length() > 1000.0, "{at} from {was}");
        assert!((rotation[1] - carousel.1[1]).abs() <= 4, "{rotation:?}");
        // Its hook is still right above it.
        let hook = rig.where_is(&format!("carousel hook {i}"));
        assert!((hook - at - Vec3::Z * playground::RIDER_HOOK_LIFT).length() < 1.0);
    }
    // The gondolas went round the axle and stayed level.
    for i in 1..=WHEEL_GONDOLAS {
        let (at, rotation) = rig.transform_of(&format!("gondola {i}"));
        assert_eq!(rotation, [0; 3], "gondola {i} keeps level");
        assert!((at.x - WHEEL_CENTRE.x).abs() < 0.5);
        let arm = Vec3::new(0.0, at.y - WHEEL_CENTRE.y, at.z - WHEEL_CENTRE.z).length();
        assert!((arm - WHEEL_RADIUS).abs() < 1.0, "{arm}");
        let was = piece_location(&format!("gondola {i}"));
        assert!((at - was).length() > 1000.0, "{at} from {was}");
    }
    // The ferry: away after one crossing, back after the whole timetable,
    // halfway in the middle of a crossing.
    let rest = piece_location("ferry");
    let half = f64::from(FERRY_WAIT + FERRY_CROSSING);
    let period = 2.0 * half;
    let at_time = |rig: &mut Rig, time: f64| {
        while rig.time() < time {
            rig.idle(1);
        }
        rig.where_is("ferry")
    };
    let far_side = at_time(&mut rig, period + half + 1.0);
    assert!(
        (far_side - (rest + FERRY_TRAVEL)).length() < 15.0,
        "{far_side}"
    );
    let home = at_time(&mut rig, 2.0 * period + 1.0);
    assert!((home - rest).length() < 15.0, "{home}");
    let halfway = at_time(
        &mut rig,
        2.0 * period + f64::from(FERRY_WAIT) + f64::from(FERRY_CROSSING) * 0.5,
    );
    assert!(
        (halfway - (rest + FERRY_TRAVEL * 0.5)).length() < 60.0,
        "{halfway}"
    );
}

/// Whatever moves carries a pawn that stands on it.
#[test]
fn a_pawn_rides_the_ferry_the_carousel_and_the_wheel() {
    for (name, radius, lift) in [
        ("ferry", 300.0, 30.0),
        ("carousel rock 1", 290.0, 0.0),
        ("carousel rock 2", 290.0, 0.0),
        ("gondola 1", 330.0, 0.0),
        ("gondola 4", 330.0, 0.0),
    ] {
        let mut rig = Rig::new();
        let top = rig.where_is(name) + Vec3::Z * lift;
        rig.stand_at(top, 0.0);
        let start = rig.where_is(name);
        let mut travelled = 0.0f32;
        for second in 0..24 {
            rig.idle(TICKS_PER_SECOND);
            let now = rig.where_is(name) + Vec3::Z * lift;
            travelled = travelled.max((rig.where_is(name) - start).length());
            assert!(
                rig.stands_on(now, radius),
                "{name} after {} s: feet at {}, it is at {now}",
                second + 1,
                rig.feet()
            );
        }
        assert!(travelled > 1500.0, "{name} moved {travelled}");
    }
}

/// "Grapple and ride": a pull to the hook that rides over a carousel rock,
/// with the anchor riding along, ends over the rock; and the same on the
/// big wheel.
#[test]
fn a_grapple_takes_a_moving_hook_and_the_pawn_ends_on_what_carries_it() {
    for (hook_name, ride_name, from, radius) in [
        (
            "carousel hook 1",
            "carousel rock 1",
            YARD + Vec3::new(0.0, -500.0, 0.0),
            290.0,
        ),
        (
            "gondola hook 1",
            "gondola 1",
            Vec3::new(YARD.x, 2050.0, YARD.z),
            330.0,
        ),
    ] {
        let mut rig = Rig::new();
        rig.stand_at(from, 0.0);
        // Wait for it to come round.
        let mut came = false;
        for _ in 0..(60 * TICKS_PER_SECOND) {
            if (rig.where_is(hook_name) - rig.game.eye_position()).length() < 2600.0 {
                came = true;
                break;
            }
            rig.idle(1);
        }
        assert!(came, "{hook_name} comes within reach of {from}");
        // The first ticks of the pull: the anchor is on the hook and moves
        // with it.
        let mut offsets = Vec::new();
        let mut anchors = Vec::new();
        for _ in 0..30 {
            let target = rig.where_is(hook_name);
            rig.aim_at(target);
            rig.tick(&InputFrame {
                grapple_held: true,
                ..InputFrame::default()
            });
            if let Some(anchor) = rig.game.player().script.gun.anchor() {
                offsets.push(anchor - rig.where_is(hook_name));
                anchors.push(anchor);
            }
        }
        assert!(offsets.len() > 15, "{hook_name}: attached");
        let (first, last) = (offsets[0], offsets[offsets.len() - 1]);
        assert!(
            (first.length() - last.length()).abs() < 2.0 && first.length() < 260.0,
            "{hook_name}: the anchor stays on the hook ({first} then {last})"
        );
        let moved = (anchors[anchors.len() - 1] - anchors[0]).length();
        assert!(moved > 20.0, "{hook_name}: the anchor rode {moved} uu");
        // On to the end of the pull, and down onto what carries the hook.
        assert!(rig.pull(&|rig| rig.where_is(hook_name), 0.0));
        assert!(
            rig.land_at(&|rig| rig.where_is(ride_name), 600),
            "{ride_name}: feet at {}",
            rig.feet()
        );
        for second in 0..6 {
            rig.idle(TICKS_PER_SECOND);
            let now = rig.where_is(ride_name);
            assert!(
                rig.stands_on(now, radius),
                "{ride_name} {} s after landing: feet at {}, it is at {now}",
                second + 1,
                rig.feet()
            );
        }
    }
}

/// A session on the playground names it as ours everywhere, keeps save
/// states with everything that moves, and changes nothing when pristine.
#[test]
fn a_session_on_the_playground_keeps_its_promises() {
    // Labels: never "converted from the original".
    let mut rig = Rig::new();
    let loader_name = rig.game.level().name.clone();
    assert!(loader_name.contains("converted"), "{loader_name}");
    let banner = rig.session.banner(&rig.game);
    assert!(banner.contains(PLAYGROUND_LEVEL_NAME), "{banner}");
    assert!(!banner.contains("converted"), "{banner}");
    let summary = Inspection::new(&rig.game, rig.script.as_ref(), &rig.session).summary();
    assert_eq!(summary.level, PLAYGROUND_LEVEL_NAME);
    assert!(summary.scripted);
    let world = Inspection::new(&rig.game, rig.script.as_ref(), &rig.session).world();
    assert!(
        world.hand_made,
        "the tools call it hand-made, not converted"
    );
    assert_eq!(world.level, PLAYGROUND_LEVEL_NAME);
    assert_eq!(world.checkpoints, 1 + playground().stations.len() - 1);
    rig.run(Command::Record { on: Toggle::On });
    rig.idle(10);
    let recording = rig
        .run(Command::Record { on: Toggle::Off })
        .recording
        .expect("a recording");
    assert_eq!(recording.header.level, PLAYGROUND_LEVEL_NAME);
    let traced = recording.trace.meta.level.clone().unwrap_or_default();
    assert!(
        traced.ends_with(PLAYGROUND_LEVEL_NAME) && !traced.contains("converted"),
        "{traced}"
    );
    // The teleport targets of the tools: the start and every station.
    let targets = rig.session.teleport_targets(&rig.game);
    assert_eq!(targets.len(), 1 + playground().stations.len());

    // A save state holds what turns and moves.
    let mut rig = Rig::new();
    rig.idle(200);
    let at_save = (
        rig.transform_of("carousel hub"),
        rig.where_is("ferry"),
        rig.where_is("gondola 3"),
        rig.game.player().position,
    );
    rig.run(Command::Slot {
        op: SlotOp::Save { slot: 0 },
    });
    rig.stand_at(YARD, 0.0);
    rig.idle(400);
    assert_ne!(rig.where_is("gondola 3"), at_save.2);
    rig.run(Command::Slot {
        op: SlotOp::Load { slot: 0 },
    });
    let restored = (
        rig.transform_of("carousel hub"),
        rig.where_is("ferry"),
        rig.where_is("gondola 3"),
        rig.game.player().position,
    );
    assert_eq!(restored, at_save);

    // A pristine session around every tick, against no session at all: the
    // same inputs give the same game, tick for tick.
    let script_input = |tick: usize| InputFrame {
        move_forward: if tick % 300 < 200 { 1.0 } else { 0.0 },
        move_right: if tick % 170 < 40 { -1.0 } else { 0.0 },
        look_yaw_delta: if tick.is_multiple_of(90) { 0.2 } else { 0.0 },
        jump_pressed: tick % 240 == 30,
        jump_held: tick % 240 >= 30 && tick % 240 < 60,
        grapple_held: tick % 400 > 300,
        sprint_held: tick % 500 > 250,
        ..InputFrame::default()
    };
    let mut with = Rig::new();
    let (mut game, script) = load_playground().unwrap();
    game.start();
    let mut script = Some(script);
    for _ in 0..5 {
        script
            .as_mut()
            .unwrap()
            .tick(&mut game, &InputFrame::default())
            .unwrap();
    }
    for tick in 0..1500 {
        let input = script_input(tick);
        let a = with.tick(&input);
        let b = script
            .as_mut()
            .unwrap()
            .tick(&mut game, &input)
            .unwrap()
            .report;
        assert_eq!(a, b, "tick {tick}");
        assert_eq!(
            format!("{:?}", with.game.player()),
            format!("{:?}", game.player()),
            "tick {tick}"
        );
    }
    assert!(with.session.is_pristine());
    assert_eq!(
        with.script.as_ref().unwrap().moved_actors(),
        script.as_ref().unwrap().moved_actors()
    );
}
