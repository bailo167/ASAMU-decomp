//! The built-in arenas: hand-made by us, valid, labelled as ours, never
//! taken for a chapter, and playable without any game data.
//!
//! The behavioural checks hold an arena's rulers against this recreation
//! running the Classic set (a low riser is climbed and a high one is not,
//! the in-range hook is accepted and the out-of-range one is not). They say
//! nothing about the original.
#![allow(clippy::unwrap_used)]

use asamu_game::smoke::{DEFAULT_SEED, InputScript};
use asamu_game::{Game, save};
use asamu_player::{InputFrame, PlayerParams};
use asamu_sandbox::arena::{
    ArenaInfo, CLOSE_PAD_CHECKPOINT, GRAPPLE_LAB, JUMP_LANE_GAPS, JUMP_LANE_PLATFORMS,
    JUMP_PLATFORM_LENGTH, JUMP_PLATFORM_RISE, MOVEMENT_LAB, RANGE_HOOK_FRACTIONS,
    RANGE_HOOK_HALF_EXTENT, RELEASE_HOOK_FRACTIONS, STAIR_RISERS, STAIR_STEPS, arena_level_name,
    arena_markers, build_arena, builtin_arenas, jump_lane_start, range_hook, release_hook,
    stair_lane_start,
};
use asamu_sandbox::command::{Command, TeleportTarget};
use asamu_sandbox::inspect::Inspection;
use asamu_sandbox::overlay::ParamSetLabel;
use asamu_sandbox::profile::Profile;
use asamu_sandbox::session::{Session, SimCx};
use asamu_sandbox::telemetry::Telemetry;
use asamu_world::{LevelOrigin, StaticBox, SurfaceTag};
use glam::Vec3;

fn started(id: &str) -> Game {
    let mut game = Session::classic()
        .new_game(build_arena(id).unwrap())
        .unwrap();
    game.start();
    game
}

fn teleport(game: &mut Game, to: TeleportTarget) {
    let mut script = None;
    Session::classic()
        .execute(
            Command::Teleport { to },
            &mut SimCx {
                game,
                script: &mut script,
            },
        )
        .unwrap();
}

/// Stands the player on the floor point `feet`, facing +X, and lets it
/// settle.
fn stand_at(game: &mut Game, feet: Vec3) {
    let lift = game.params().movement.capsule_half_height.value + 1.0;
    teleport(
        game,
        TeleportTarget::Position {
            position: (feet + Vec3::Z * lift).to_array(),
            yaw: Some(0.0),
            pitch: Some(0.0),
        },
    );
    for _ in 0..10 {
        game.tick(&InputFrame::default()).unwrap();
    }
    assert!(game.player().grounded);
}

/// Points the view at `target` with one tick of look input.
fn look_at(game: &mut Game, target: Vec3) {
    let d = target - game.eye_position();
    let p = *game.player();
    let input = InputFrame {
        look_yaw_delta: d.y.atan2(d.x) - p.yaw,
        look_pitch_delta: d.z.atan2(d.truncate().length()) - p.pitch,
        ..InputFrame::default()
    };
    game.tick(&input).unwrap();
}

fn forward() -> InputFrame {
    InputFrame {
        move_forward: 1.0,
        ..InputFrame::default()
    }
}

fn boxes_labelled<'a>(boxes: &'a [StaticBox], prefix: &str) -> Vec<&'a StaticBox> {
    boxes
        .iter()
        .filter(|b| b.label.starts_with(prefix))
        .collect()
}

// ---------------------------------------------------------------------------
// What every arena must be.
// ---------------------------------------------------------------------------

#[test]
fn there_are_two_arenas_with_distinct_ids() {
    let arenas = builtin_arenas();
    let ids: Vec<&str> = arenas.iter().map(|a| a.id).collect();
    assert_eq!(ids, [MOVEMENT_LAB, GRAPPLE_LAB]);
    for ArenaInfo { id, title, summary } in arenas {
        assert!(!title.is_empty() && !summary.is_empty(), "{id}");
        // Ids are what `--arena` takes: plain lower-case words.
        assert!(
            id.bytes().all(|b| b.is_ascii_lowercase() || b == b'-'),
            "{id}"
        );
    }
    assert!(build_arena("").is_none());
    assert!(build_arena("Movement-Lab").is_none());
    assert!(build_arena("graybox").is_none());
}

#[test]
fn every_arena_validates() {
    for info in builtin_arenas() {
        let level = build_arena(info.id).unwrap();
        assert_eq!(level.validate(), Ok(()), "{}", info.id);
        assert!(!level.static_boxes.is_empty(), "{}", info.id);
        assert!(!level.checkpoints.is_empty(), "{}", info.id);
        assert!(level.player_start.feet.z > level.kill_z, "{}", info.id);
        // Building is deterministic: the same level every time.
        assert_eq!(build_arena(info.id).unwrap(), level, "{}", info.id);
        assert_eq!(
            arena_markers(info.id),
            arena_markers(info.id),
            "{}",
            info.id
        );
    }
}

#[test]
fn every_arena_is_labelled_hand_made_and_is_never_a_chapter() {
    for info in builtin_arenas() {
        let level = build_arena(info.id).unwrap();
        assert_eq!(level.origin, LevelOrigin::HandMadeGraybox, "{}", info.id);
        assert_eq!(level.name, arena_level_name(info.id));
        assert!(
            level.name.ends_with("(hand-made, not original content)"),
            "{}",
            level.name
        );
        assert!(level.name.starts_with("sandbox arena: "), "{}", level.name);
        // The save system finds chapters by map name and by title; an arena
        // is neither, whichever of its names is asked about.
        for name in [level.name.as_str(), info.id, info.title] {
            assert!(!name.contains("AG-"), "{name}");
            assert_eq!(save::chapter_of_map(name, None), None, "{name}");
            assert_eq!(save::chapter_of_map(name, Some(name)), None, "{name}");
            assert_eq!(save::chapter_of_map(info.id, Some(name)), None, "{name}");
        }
    }
}

#[test]
fn arenas_load_without_any_game_data() {
    // A game on an arena is a hand-made level: boxes in memory, no
    // converted scene, the Classic parameter set.
    for info in builtin_arenas() {
        let mut game = started(info.id);
        assert!(game.scene_map().is_none(), "{}", info.id);
        assert_eq!(*game.params(), PlayerParams::asamu_original());
        assert!(game.player().grounded, "{}: spawned standing", info.id);
        assert!(game.player().is_finite());
        let report = game.tick(&InputFrame::default()).unwrap();
        assert!(!report.respawned);
        // The session sees it as one of ours.
        let session = Session::classic();
        let look = Inspection::new(&game, None, &session);
        assert!(look.world().hand_made);
        assert_eq!(look.summary().label, ParamSetLabel::Classic);
        assert!(look.summary().rewind_available);
        // Every checkpoint is a teleport target, after the start.
        let targets = look.teleport_targets();
        assert_eq!(targets.len(), 1 + game.level().checkpoints.len());
    }
}

#[test]
fn a_scripted_run_stays_finite() {
    for info in builtin_arenas() {
        let mut game = started(info.id);
        let mut session = Session::classic();
        let mut inputs = InputScript::new(DEFAULT_SEED);
        let mut script = None;
        for tick in 0..600 {
            session.before_tick(&mut SimCx {
                game: &mut game,
                script: &mut script,
            });
            let report = game.tick(&inputs.next_frame()).unwrap();
            session.after_tick(&game, None, &report);
            assert!(
                !report.events.non_finite_rejected,
                "{}: tick {tick}",
                info.id
            );
            assert!(game.player().is_finite(), "{}: tick {tick}", info.id);
        }
        assert_eq!(game.clock().tick(), 600);
        assert!(session.is_pristine());
    }
}

#[test]
fn every_builtin_profile_plays_on_every_arena() {
    // The rulers stay Classic whatever the session runs: the level a tuned
    // session gets is the same level.
    for info in builtin_arenas() {
        let level = build_arena(info.id).unwrap();
        for profile in Profile::builtin() {
            let name = profile.name.clone();
            let session = Session::new(profile).unwrap();
            let mut game = session.new_game(build_arena(info.id).unwrap()).unwrap();
            assert_eq!(*game.level(), level, "{}: {name}", info.id);
            game.start();
            let mut inputs = InputScript::new(DEFAULT_SEED ^ 0xA5);
            for _ in 0..200 {
                let report = game.tick(&inputs.next_frame()).unwrap();
                assert!(!report.events.non_finite_rejected, "{}: {name}", info.id);
            }
            assert!(game.player().is_finite(), "{}: {name}", info.id);
        }
    }
}

#[test]
fn markers_label_the_stations() {
    let movement = arena_markers(MOVEMENT_LAB);
    let has = |markers: &[asamu_sandbox::arena::ArenaMarker], needle: &str| {
        markers.iter().any(|m| m.text.contains(needle))
    };
    assert!(has(&movement, "runway"));
    assert!(has(&movement, "1 s walking"));
    assert!(has(&movement, "1 s sprinting"));
    assert!(has(&movement, "MaxStepHeight"));
    assert!(has(&movement, "Classic walking jump"));
    // The jump rulers say whose jump they are.
    assert!(has(&movement, "this recreation"));
    assert!(has(&movement, "not the original's"));
    assert_eq!(
        movement
            .iter()
            .filter(|m| m.text.starts_with("riser"))
            .count(),
        STAIR_RISERS.len()
    );
    assert_eq!(
        movement
            .iter()
            .filter(|m| m.text.starts_with("gap"))
            .count(),
        JUMP_LANE_GAPS.len()
    );

    let grapple = arena_markers(GRAPPLE_LAB);
    assert_eq!(
        grapple
            .iter()
            .filter(|m| m.text.contains("fMaxDistance"))
            .count(),
        RANGE_HOOK_FRACTIONS.len()
    );
    assert_eq!(
        grapple
            .iter()
            .filter(|m| m.text.contains("fGrappleReleaseDistance"))
            .count(),
        RELEASE_HOOK_FRACTIONS.len()
    );
    for needle in [
        "TopOnly",
        "BottomOnly",
        "NotLandable",
        "not grapple-able",
        "crystal",
        "moving block",
        "attractor",
    ] {
        assert!(has(&grapple, needle), "{needle}");
    }
    assert!(arena_markers("nowhere").is_empty());
}

// ---------------------------------------------------------------------------
// movement-lab: the rulers against the Classic set in this recreation.
// ---------------------------------------------------------------------------

#[test]
fn the_runway_posts_are_one_second_of_classic_walking_apart() {
    let mut game = started(MOVEMENT_LAB);
    let level = game.level().clone();
    let mut marks: Vec<f32> = boxes_labelled(&level.static_boxes, "walk mark")
        .iter()
        .map(|b| (b.min.x + b.max.x) * 0.5)
        .collect();
    marks.sort_by(f32::total_cmp);
    assert_eq!(marks.len(), 12);
    // Evenly spaced from the start line at x = 0.
    let spacing = marks[0];
    for (i, x) in marks.iter().enumerate() {
        assert!((x - spacing * (i + 1) as f32).abs() < 0.01, "{x}");
    }
    let pawn = PlayerParams::asamu_original().pawn.unwrap();
    assert!((spacing - pawn.move_speed.value).abs() < 0.01);
    // Sprint posts stand at the sprint speed's seconds.
    let sprint = boxes_labelled(&level.static_boxes, "sprint mark");
    assert!(!sprint.is_empty());
    let first_sprint = sprint
        .iter()
        .map(|b| (b.min.x + b.max.x) * 0.5)
        .fold(f32::MAX, f32::min);
    assert!(
        (first_sprint - pawn.move_speed.value * pawn.sprint_speed_multiplier.value).abs() < 0.01
    );

    // Walking from the hub, the pawn crosses the start line at speed and
    // passes one post per second from then on.
    let rate = game.clock().tick_rate_hz();
    let mut crossings = Vec::new();
    let mut next = 0;
    for _ in 0..2000 {
        let report = game.tick(&forward()).unwrap();
        assert!(!report.respawned);
        if game.player().position.x >= marks[next] {
            crossings.push(report.tick);
            next += 1;
            if next == 6 {
                break;
            }
        }
    }
    assert_eq!(crossings.len(), 6);
    for pair in crossings.windows(2) {
        let seconds = (pair[1] - pair[0]) as f64 / rate;
        assert!(
            (seconds - 1.0).abs() <= 2.0 / rate,
            "{seconds} s between posts"
        );
    }
}

#[test]
fn low_risers_are_climbed_and_high_ones_stop_the_pawn() {
    let level = build_arena(MOVEMENT_LAB).unwrap();
    let step_height = PlayerParams::asamu_original().movement.step_height.value;
    let lowest = 0;
    let highest = STAIR_RISERS.len() - 1;
    assert!(STAIR_RISERS[lowest] < 1.0 && STAIR_RISERS[highest] > 1.0);
    assert!(STAIR_RISERS.windows(2).all(|w| w[0] < w[1]));
    assert!(stair_lane_start(STAIR_RISERS.len()).is_none());

    let climb = |lane: usize| {
        let mut game = Session::classic().new_game(level.clone()).unwrap();
        game.start();
        stand_at(&mut game, stair_lane_start(lane).unwrap());
        let rest = game.player().position.z;
        let mut top = rest;
        for _ in 0..200 {
            let report = game.tick(&forward()).unwrap();
            assert!(!report.respawned);
            top = top.max(game.player().position.z);
        }
        top - rest
    };
    // The lowest riser: up all the steps.
    let riser = STAIR_RISERS[lowest] * step_height;
    let climbed = climb(lowest);
    let whole = f32::from(STAIR_STEPS) * riser;
    assert!((climbed - whole).abs() < 1.0, "{climbed} of {whole}");
    // The highest riser: not even the first step.
    let riser = STAIR_RISERS[highest] * step_height;
    let climbed = climb(highest);
    assert!(
        climbed < riser * 0.5,
        "{climbed} against a riser of {riser}"
    );

    // The lanes' boxes are the labelled multiples of the step height.
    for (lane, fraction) in STAIR_RISERS.iter().enumerate() {
        let label = format!("stair 1 of lane {}", lane + 1);
        let first = boxes_labelled(&level.static_boxes, &label);
        assert_eq!(first.len(), 1, "{label}");
        assert!((first[0].max.z - fraction * step_height).abs() < 0.001);
    }
}

#[test]
fn jump_lanes_are_measured_in_full_classic_jumps_of_this_recreation() {
    let level = build_arena(MOVEMENT_LAB).unwrap();
    // Measure a full standing jump and a full walking jump here, on the
    // arena's own runway, with the Classic set.
    let jump = |walking: bool| {
        let mut game = Session::classic().new_game(level.clone()).unwrap();
        game.start();
        let mut telemetry = Telemetry::default();
        let walk = if walking { 1.0 } else { 0.0 };
        let run_up = InputFrame {
            move_forward: walk,
            ..InputFrame::default()
        };
        for _ in 0..150 {
            let report = game.tick(&run_up).unwrap();
            telemetry.observe(&game, &report);
        }
        for i in 0..2000 {
            let input = InputFrame {
                jump_pressed: i == 0,
                jump_held: true,
                ..run_up
            };
            let report = game.tick(&input).unwrap();
            telemetry.observe(&game, &report);
            if telemetry.last_jump().is_some() {
                break;
            }
        }
        telemetry
            .last_jump()
            .expect("the jump landed on the runway")
    };
    let apex = jump(false).apex_height;
    let distance = jump(true).distance;

    for (lane, fraction) in JUMP_LANE_GAPS.iter().enumerate() {
        let platforms = boxes_labelled(
            &level.static_boxes,
            &format!("jump platform 1 of lane {}", lane + 1),
        );
        assert_eq!(platforms.len(), 1);
        let first = platforms[0];
        // The gap from the hub's edge (x = 0), the rise and the length are
        // the labelled fractions of the measured jump.
        assert!(
            (first.min.x - fraction * distance).abs() < 0.5,
            "{}",
            first.min.x
        );
        assert!(
            (first.max.z - JUMP_PLATFORM_RISE * apex).abs() < 0.5,
            "{}",
            first.max.z
        );
        let length = first.max.x - first.min.x;
        assert!(
            (length - JUMP_PLATFORM_LENGTH * distance).abs() < 0.5,
            "{length}"
        );
        // Each later platform is one gap further and one rise higher.
        for k in 2..=JUMP_LANE_PLATFORMS {
            let label = format!("jump platform {k} of lane {}", lane + 1);
            let next = boxes_labelled(&level.static_boxes, &label);
            assert_eq!(next.len(), 1, "{label}");
            let n = f32::from(k);
            assert!((next[0].max.z - n * JUMP_PLATFORM_RISE * apex).abs() < 0.5);
            let x = n * fraction * distance + (n - 1.0) * length;
            assert!((next[0].min.x - x).abs() < 1.0);
        }
    }
    assert!(jump_lane_start(JUMP_LANE_GAPS.len()).is_none());
}

#[test]
fn a_full_classic_walking_jump_reaches_the_three_quarter_lane() {
    // The lane whose gap is three quarters of a walking jump: a full jump
    // from the hub's edge lands on its first platform.
    let lane = JUMP_LANE_GAPS
        .iter()
        .position(|f| (*f - 0.75).abs() < 1.0e-6)
        .expect("a 0.75 lane");
    let level = build_arena(MOVEMENT_LAB).unwrap();
    let platform = boxes_labelled(
        &level.static_boxes,
        &format!("jump platform 1 of lane {}", lane + 1),
    )[0]
    .clone();
    let mut game = Session::classic().new_game(level).unwrap();
    game.start();
    stand_at(&mut game, jump_lane_start(lane).unwrap());
    let rest_above_floor = game.player().position.z;

    let radius = game.params().movement.capsule_radius.value;
    let mut jumped = false;
    let mut landed = None;
    for _ in 0..2000 {
        let p = *game.player();
        let jump_now = !jumped && p.grounded && p.position.x >= -radius;
        jumped |= jump_now;
        let input = InputFrame {
            jump_pressed: jump_now,
            jump_held: jumped,
            ..forward()
        };
        let report = game.tick(&input).unwrap();
        assert!(!report.respawned, "the jump fell short or long");
        if jumped && report.events.landed.is_some() {
            landed = Some(game.player().position);
            break;
        }
    }
    let landed = landed.expect("the jump landed");
    assert!(
        landed.x > platform.min.x && landed.x < platform.max.x,
        "landed at {landed:?}, platform {:?} to {:?}",
        platform.min,
        platform.max
    );
    // Standing on the platform's top, as high above it as above the hub.
    assert!((landed.z - platform.max.z - rest_above_floor).abs() < 1.0);
}

// ---------------------------------------------------------------------------
// grapple-lab.
// ---------------------------------------------------------------------------

#[test]
fn the_grapple_lab_has_its_stations() {
    let level = build_arena(GRAPPLE_LAB).unwrap();
    let tags: Vec<SurfaceTag> = level.static_boxes.iter().map(|b| b.tag).collect();
    for tag in [
        SurfaceTag::TopOnlyGrappleAble,
        SurfaceTag::BottomOnlyGrappleAble,
        SurfaceTag::NotLandable,
    ] {
        assert_eq!(tags.iter().filter(|t| **t == tag).count(), 1, "{tag:?}");
    }
    assert!(
        level
            .static_boxes
            .iter()
            .any(|b| b.label.contains("beam") && b.grapple_able)
    );
    assert!(
        level
            .static_boxes
            .iter()
            .any(|b| b.label.contains("wall") && !b.grapple_able)
    );
    assert_eq!(
        level.grapple_points.len(),
        RANGE_HOOK_FRACTIONS.len() + RELEASE_HOOK_FRACTIONS.len()
    );
    assert_eq!(level.crystals.len(), 1);
    assert_eq!(level.movers.len(), 1);
    assert_eq!(level.attractors.len(), 1);
    assert!(
        level
            .checkpoints
            .iter()
            .any(|c| c.id == CLOSE_PAD_CHECKPOINT)
    );
    assert!(RANGE_HOOK_FRACTIONS.iter().any(|f| *f > 1.0));
    assert!(RELEASE_HOOK_FRACTIONS.iter().any(|f| *f < 1.0));
    assert!(RELEASE_HOOK_FRACTIONS.iter().any(|f| *f > 1.0));
}

#[test]
fn fan_hooks_are_in_plain_sight_and_in_reach_up_to_the_classic_range() {
    let mut game = started(GRAPPLE_LAB);
    for _ in 0..10 {
        game.tick(&InputFrame::default()).unwrap();
    }
    let range = PlayerParams::asamu_original()
        .gun
        .unwrap()
        .max_distance
        .value;
    for (index, fraction) in RANGE_HOOK_FRACTIONS.iter().enumerate() {
        let hook = range_hook(index).unwrap();
        look_at(&mut game, hook);
        let aim = game.gun_aim().unwrap();
        // Nothing stands between the start and the hook: the trace ends on
        // the hook's own cube, at the labelled distance.
        let hit = aim.impact.hit.expect("the hook is in the crosshair");
        assert!(hit.grapple_able, "hook {index}");
        let on_cube = (aim.impact.location - hook).abs().max_element();
        assert!(
            on_cube <= RANGE_HOOK_HALF_EXTENT + 0.5,
            "hook {index}: {on_cube}"
        );
        let labelled = fraction * range;
        assert!(
            (aim.distance - labelled).abs() <= RANGE_HOOK_HALF_EXTENT * 2.0,
            "hook {index}: {} vs {labelled}",
            aim.distance
        );
        // In reach below the Classic range, out of reach beyond it.
        assert_eq!(aim.acceptable, *fraction < 1.0, "hook {index}");
    }
    assert!(range_hook(RANGE_HOOK_FRACTIONS.len()).is_none());
}

#[test]
fn close_hooks_sit_on_both_sides_of_the_classic_release_distance() {
    let mut game = started(GRAPPLE_LAB);
    teleport(
        &mut game,
        TeleportTarget::Checkpoint {
            id: CLOSE_PAD_CHECKPOINT,
        },
    );
    for _ in 0..10 {
        game.tick(&InputFrame::default()).unwrap();
    }
    assert!(game.player().grounded);
    let release = PlayerParams::asamu_original()
        .gun
        .unwrap()
        .release_distance
        .value;
    for (index, fraction) in RELEASE_HOOK_FRACTIONS.iter().enumerate() {
        let hook = release_hook(index).unwrap();
        look_at(&mut game, hook);
        let aim = game.gun_aim().unwrap();
        assert!(aim.impact.hit.is_some(), "hook {index}");
        assert!(aim.acceptable, "hook {index}: well inside the range");
        // From the pawn's centre to where the grapple would anchor.
        let anchor_distance = (aim.impact.location - game.player().position).length();
        assert_eq!(
            anchor_distance < release,
            *fraction < 1.0,
            "hook {index}: {anchor_distance} against {release}"
        );
    }
    assert!(release_hook(RELEASE_HOOK_FRACTIONS.len()).is_none());
}
