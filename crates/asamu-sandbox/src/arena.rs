//! Built-in lab arenas.
//!
//! Hand-made box levels, written by us for the Sandbox: **no original
//! content and no data derived from it.** Every position, size, count and
//! factor in this file was chosen by us. What ties an arena to the game is
//! its *rulers*: distance marks, platform gaps, riser heights and hook
//! distances are multiples of Classic quantities, taken when the arena is
//! built. They therefore stay at Classic values while the player tunes a
//! session, which is the point: the arena is the fixed yardstick.
//!
//! Two kinds of ruler:
//!
//! - **Parameter rulers** are Classic parameter values read from
//!   [`PlayerParams::asamu_original`] (walking and sprint speed, step
//!   height, the grapple's reach and release distance). No number of the
//!   original is written in this file.
//! - **Jump rulers** (apex, and the ground a walking jump covers) are
//!   **measured in this recreation**: a pawn with the Classic set makes a
//!   full jump (jump held) on a flat floor through the game's own tick, once
//!   per process. They describe what this recreation does with the Classic
//!   set today. They are not measurements of the original and not evidence
//!   of parity, and they move if the recreation's physics is corrected.
//!
//! Every arena has origin `LevelOrigin::HandMadeGraybox` and the name
//! `sandbox arena: <id> (hand-made, not original content)`. No arena uses a
//! chapter's map name or title, so the save system never takes one for a
//! chapter. Box levels cannot express slopes.
//!
//! | Id | Stations |
//! |---|---|
//! | [`MOVEMENT_LAB`] | a runway with a post every second of Classic walking (and sprinting); lanes of platforms with rising heights across four gap widths; stair lanes whose risers go from half to one and a half times the Classic step height |
//! | [`GRAPPLE_LAB`] | a fan of hooks at fractions of the Classic grapple range; hooks around the Classic release distance; a grapple-able beam; top-only, bottom-only and not-landable surfaces; a wall that cannot be grappled; a recharge crystal; a moving block; an attractor pad |

use std::sync::OnceLock;

use asamu_core::DEFAULT_TICK_RATE_HZ;
use asamu_game::Game;
use asamu_player::InputFrame;
use asamu_player::PlayerParams;
use asamu_player::world::CONTACT_SKIN;
use asamu_world::objects::CRYSTAL_DEFAULT_RECHARGE_DELAY;
use asamu_world::{
    Attractor, Checkpoint, GrapplePoint, Level, LevelAbilities, LevelOrigin, Mover, MoverPath,
    RechargeCrystal, SpawnPoint, StaticBox, SurfaceTag,
};
use glam::Vec3;

use crate::telemetry::{JumpStats, Telemetry};

/// Id of the movement arena.
pub const MOVEMENT_LAB: &str = "movement-lab";
/// Id of the grapple arena.
pub const GRAPPLE_LAB: &str = "grapple-lab";

/// A built-in arena, for a stage list.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ArenaInfo {
    /// Identifier, as taken by `--arena` (e.g. `movement-lab`).
    pub id: &'static str,
    /// Short title.
    pub title: &'static str,
    /// One line on what the arena is for.
    pub summary: &'static str,
}

/// A text label placed in an arena (a ruler mark, a station name).
#[derive(Clone, Debug, PartialEq)]
pub struct ArenaMarker {
    /// Where the label belongs, UU.
    pub position: Vec3,
    /// The label.
    pub text: String,
}

const ARENAS: [ArenaInfo; 2] = [
    ArenaInfo {
        id: MOVEMENT_LAB,
        title: "Movement lab",
        summary: "Runway with a mark every second of Classic walking, jump lanes with rising \
                  platforms, stairs around the Classic step height (hand-made)",
    },
    ArenaInfo {
        id: GRAPPLE_LAB,
        title: "Grapple lab",
        summary: "Hooks at fractions of the Classic grapple range and release distance, tagged \
                  surfaces, a crystal, a moving block and an attractor pad (hand-made)",
    },
];

/// The built-in arenas.
#[must_use]
pub fn builtin_arenas() -> &'static [ArenaInfo] {
    &ARENAS
}

/// The level name of the arena `id`:
/// `sandbox arena: <id> (hand-made, not original content)`.
#[must_use]
pub fn arena_level_name(id: &str) -> String {
    format!("sandbox arena: {id} (hand-made, not original content)")
}

/// Builds the arena named `id`; `None` for an unknown id.
///
/// The level is the same every time: an arena depends on nothing but the
/// Classic set, which is a constant of the build.
#[must_use]
pub fn build_arena(id: &str) -> Option<Level> {
    built(id).map(|arena| arena.level.clone())
}

/// The labels of the arena named `id` (empty for an unknown id).
#[must_use]
pub fn arena_markers(id: &str) -> Vec<ArenaMarker> {
    built(id)
        .map(|arena| arena.markers.clone())
        .unwrap_or_default()
}

/// A level and its labels, built together so they cannot drift apart.
struct Arena {
    level: Level,
    markers: Vec<ArenaMarker>,
}

/// The built-in arenas, built once per process (in [`ARENAS`] order).
fn built(id: &str) -> Option<&'static Arena> {
    static BUILT: OnceLock<[Arena; 2]> = OnceLock::new();
    let index = ARENAS.iter().position(|info| info.id == id)?;
    BUILT
        .get_or_init(|| {
            let rulers = Rulers::classic();
            [movement_lab(rulers), grapple_lab(rulers)]
        })
        .get(index)
}

// ---------------------------------------------------------------------------
// Rulers: the Classic quantities the arenas are measured in.
// ---------------------------------------------------------------------------

/// The Classic quantities the rulers are built from (see the module docs).
#[derive(Clone, Copy, Debug, PartialEq)]
struct Rulers {
    /// `pawn.move_speed`, uu/s.
    walk_speed: f32,
    /// `pawn.move_speed` × `pawn.sprint_speed_multiplier`, uu/s.
    sprint_speed: f32,
    /// `movement.step_height`, UU.
    step_height: f32,
    /// `movement.capsule_half_height`, UU.
    half_height: f32,
    /// `camera.eye_height`, UU above the collision centre.
    eye_height: f32,
    /// `gun.max_distance`, UU.
    grapple_range: f32,
    /// `gun.release_distance`, UU.
    release_distance: f32,
    /// Apex of a full standing jump above the standing height, UU
    /// (measured in this recreation, or the fallback estimate).
    jump_apex: f32,
    /// Ground covered by a full jump at walking speed, take-off to landing,
    /// UU (measured in this recreation, or the fallback estimate).
    walking_jump_distance: f32,
    /// The jump rulers were measured (`false`: the measurement did not
    /// finish and the ballistic fallback is in use).
    jump_measured: bool,
}

impl Rulers {
    /// The rulers of the Classic set, built once per process (the Classic
    /// set is a constant of the build, so the cache cannot go stale).
    fn classic() -> &'static Self {
        static CLASSIC: OnceLock<Rulers> = OnceLock::new();
        CLASSIC.get_or_init(|| Self::of(&PlayerParams::asamu_original()))
    }

    fn of(p: &PlayerParams) -> Self {
        let m = &p.movement;
        // The Classic set has the script-layer groups; the fallbacks only
        // keep this total if that ever changed.
        let walk_speed = p
            .pawn
            .as_ref()
            .map_or(m.max_ground_speed.value, |pawn| pawn.move_speed.value);
        let sprint = p
            .pawn
            .as_ref()
            .map_or(1.0, |pawn| pawn.sprint_speed_multiplier.value);
        let (grapple_range, release_distance) = p.gun.as_ref().map_or(
            (p.grapple.max_range.value, p.grapple.min_rope_length.value),
            |gun| (gun.max_distance.value, gun.release_distance.value),
        );
        let measured = measure_jumps(p);
        // Fallback only: the textbook arc from jump velocity and gravity.
        let gravity = (m.world_gravity_z.value * m.custom_gravity_scaling.value)
            .abs()
            .max(1.0);
        let estimate = (
            m.jump_velocity.value * m.jump_velocity.value / (2.0 * gravity),
            walk_speed * 2.0 * m.jump_velocity.value / gravity,
        );
        let (jump_apex, walking_jump_distance) = measured.unwrap_or(estimate);
        Self {
            walk_speed,
            sprint_speed: walk_speed * sprint,
            step_height: m.step_height.value,
            half_height: m.capsule_half_height.value,
            eye_height: p.camera.eye_height.value,
            grapple_range,
            release_distance,
            jump_apex,
            walking_jump_distance,
            jump_measured: measured.is_some(),
        }
    }

    /// Collision centre of a pawn placed on a floor at `feet`.
    fn standing_centre(&self, feet: Vec3) -> Vec3 {
        feet + Vec3::Z * (self.half_height + CONTACT_SKIN)
    }
}

/// Ticks one measured jump may take (a bound; a jump that has not landed by
/// then is not used).
const MEASURE_TICKS: usize = 3600;
/// Ticks of walking before the walking jump, so the pawn is at full speed
/// (ours).
const MEASURE_RUN_UP_TICKS: usize = 120;

/// A full jump (jump held until landing) of a pawn with `params` on a flat
/// hand-made floor, through the game's own tick: the apex of a standing
/// jump and the ground covered by a jump at walking speed. `None` if a jump
/// did not start or did not land, or a value is not usable.
fn measure_jumps(params: &PlayerParams) -> Option<(f32, f32)> {
    let apex = measured_jump(params, 0)?.apex_height;
    let distance = measured_jump(params, MEASURE_RUN_UP_TICKS)?.distance;
    (apex.is_finite() && apex > 1.0 && distance.is_finite() && distance > 1.0)
        .then_some((apex, distance))
}

/// One measured jump after `run_up` ticks of walking forward.
fn measured_jump(params: &PlayerParams, run_up: usize) -> Option<JumpStats> {
    // A floor long enough for the run-up and the jump, and nothing else.
    let mut flat = level("ruler", Vec3::ZERO, -1000.0);
    flat.abilities = LevelAbilities::default();
    flat.static_boxes.push(solid(
        v(-1000.0, -1000.0, -SLAB),
        v(60_000.0, 1000.0, 0.0),
        "measuring floor",
    ));
    let mut game = Game::new(flat, params.clone(), DEFAULT_TICK_RATE_HZ).ok()?;
    game.start();
    let mut telemetry = Telemetry::default();
    let walking = if run_up > 0 { 1.0 } else { 0.0 };
    // Settle, then run up.
    for _ in 0..(10 + run_up) {
        let input = InputFrame {
            move_forward: walking,
            ..InputFrame::default()
        };
        let report = game.tick(&input)?;
        telemetry.observe(&game, &report);
    }
    for tick in 0..MEASURE_TICKS {
        let input = InputFrame {
            move_forward: walking,
            jump_pressed: tick == 0,
            jump_held: true,
            ..InputFrame::default()
        };
        let report = game.tick(&input)?;
        telemetry.observe(&game, &report);
        if tick == 0 && !report.events.jumped {
            return None;
        }
        if let Some(jump) = telemetry.last_jump() {
            return Some(jump);
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Small builders.
// ---------------------------------------------------------------------------

fn v(x: f32, y: f32, z: f32) -> Vec3 {
    Vec3::new(x, y, z)
}

fn solid(min: Vec3, max: Vec3, label: impl Into<String>) -> StaticBox {
    StaticBox::new(min, max, false, label)
}

fn marker(position: Vec3, text: impl Into<String>) -> ArenaMarker {
    ArenaMarker {
        position,
        text: text.into(),
    }
}

fn checkpoint(id: u32, min: Vec3, max: Vec3, feet: Vec3, yaw: f32) -> Checkpoint {
    Checkpoint {
        id,
        min,
        max,
        spawn: SpawnPoint { feet, yaw },
    }
}

fn level(id: &str, player_start: Vec3, kill_z: f32) -> Level {
    Level {
        name: arena_level_name(id),
        origin: LevelOrigin::HandMadeGraybox,
        static_boxes: Vec::new(),
        grapple_points: Vec::new(),
        player_start: SpawnPoint {
            feet: player_start,
            yaw: 0.0,
        },
        checkpoints: Vec::new(),
        kill_z,
        crystals: Vec::new(),
        flowers: Vec::new(),
        movers: Vec::new(),
        interactables: Vec::new(),
        attractors: Vec::new(),
        // Everything on, three grapples: the graybox's test configuration
        // (ours), so the lab is playable without touching the rules.
        abilities: LevelAbilities::graybox_test(),
    }
}

// ---------------------------------------------------------------------------
// movement-lab
// ---------------------------------------------------------------------------

/// Thickness of floors, UU (ours).
const SLAB: f32 = 100.0;
/// The hub strip the stations start from: `x` from here to 0 (ours).
const HUB_BACK: f32 = -600.0;
/// Where the player stands in the hub, `x` (ours): far enough behind the
/// start line (`x = 0`) to cross it at full walking speed.
const HUB_STAND_X: f32 = -300.0;
/// Seconds of Classic walking the runway is long (ours).
const RUNWAY_WALK_SECONDS: u8 = 12;
/// Half the runway's width, UU (ours).
const RUNWAY_HALF_WIDTH: f32 = 360.0;
/// Gap of each jump lane as a fraction of the Classic walking-jump distance
/// (ours).
pub const JUMP_LANE_GAPS: [f32; 4] = [0.25, 0.5, 0.75, 1.0];
/// Platforms per jump lane (ours).
pub const JUMP_LANE_PLATFORMS: u8 = 4;
/// Rise from one platform to the next as a fraction of the Classic jump apex
/// (ours).
pub const JUMP_PLATFORM_RISE: f32 = 0.15;
/// Length of a jump platform as a fraction of the Classic walking-jump
/// distance (ours).
pub const JUMP_PLATFORM_LENGTH: f32 = 0.5;
/// Thickness of a jump platform, UU (ours).
const JUMP_PLATFORM_THICKNESS: f32 = 40.0;
/// Riser of each stair lane as a fraction of the Classic step height (ours):
/// from half of it to one and a half times.
pub const STAIR_RISERS: [f32; 6] = [0.5, 0.9, 1.0, 1.1, 1.25, 1.5];
/// Steps per stair lane (ours).
pub const STAIR_STEPS: u8 = 3;
/// Depth of one stair step, UU (ours).
const STAIR_DEPTH: f32 = 120.0;

/// `y` of the centre of jump lane `lane` (ours).
fn jump_lane_y(lane: usize) -> f32 {
    900.0 + 600.0 * lane as f32
}

/// `y` of the centre of stair lane `lane` (ours).
fn stair_lane_y(lane: usize) -> f32 {
    -800.0 - 300.0 * lane as f32
}

/// Where a pawn stands in the hub in front of jump lane `lane` (the floor
/// point under its feet), facing the lane (+X). `None` for a lane that does
/// not exist.
#[must_use]
pub fn jump_lane_start(lane: usize) -> Option<Vec3> {
    (lane < JUMP_LANE_GAPS.len()).then(|| v(HUB_STAND_X, jump_lane_y(lane), 0.0))
}

/// Where a pawn stands in the hub in front of stair lane `lane` (the floor
/// point under its feet), facing the stairs (+X). `None` for a lane that
/// does not exist.
#[must_use]
pub fn stair_lane_start(lane: usize) -> Option<Vec3> {
    (lane < STAIR_RISERS.len()).then(|| v(HUB_STAND_X, stair_lane_y(lane), 0.0))
}

fn movement_lab(r: &Rulers) -> Arena {
    let mut level = level(MOVEMENT_LAB, v(HUB_STAND_X, 0.0, 0.0), -2000.0);
    let mut markers = Vec::new();
    let boxes = &mut level.static_boxes;

    // The hub: one strip all three stations start from.
    boxes.push(solid(
        v(HUB_BACK, -2500.0, -SLAB),
        v(0.0, 3000.0, 0.0),
        "hub floor",
    ));

    // Station 1: the runway. The start line is x = 0; a post stands at every
    // second of Classic walking (left edge) and sprinting (right edge).
    let runway_marks = f32::from(RUNWAY_WALK_SECONDS) * r.walk_speed;
    let runway_length = runway_marks + 400.0;
    boxes.push(solid(
        v(0.0, -RUNWAY_HALF_WIDTH, -SLAB),
        v(runway_length, RUNWAY_HALF_WIDTH, 0.0),
        "runway",
    ));
    markers.push(marker(
        v(0.0, 0.0, 120.0),
        "runway start line (posts: left, each second of Classic walking; right, of sprinting)",
    ));
    for second in 1..=RUNWAY_WALK_SECONDS {
        let x = f32::from(second) * r.walk_speed;
        boxes.push(solid(
            v(x - 10.0, -RUNWAY_HALF_WIDTH, 0.0),
            v(x + 10.0, -RUNWAY_HALF_WIDTH + 20.0, 40.0),
            format!("walk mark {second} s"),
        ));
        markers.push(marker(
            v(x, -RUNWAY_HALF_WIDTH + 10.0, 70.0),
            format!("{second} s walking ({x:.0} uu)"),
        ));
    }
    for second in 1..=RUNWAY_WALK_SECONDS {
        let x = f32::from(second) * r.sprint_speed;
        if x > runway_marks {
            break;
        }
        boxes.push(solid(
            v(x - 10.0, RUNWAY_HALF_WIDTH - 20.0, 0.0),
            v(x + 10.0, RUNWAY_HALF_WIDTH, 80.0),
            format!("sprint mark {second} s"),
        ));
        markers.push(marker(
            v(x, RUNWAY_HALF_WIDTH - 10.0, 110.0),
            format!("{second} s sprinting ({x:.0} uu)"),
        ));
    }

    // Station 2: jump lanes. Each lane has its own gap; every platform is one
    // rise higher than the one before. A fall ends in a respawn at the
    // station's checkpoint.
    let jump = r.walking_jump_distance;
    let rise = JUMP_PLATFORM_RISE * r.jump_apex;
    let platform_length = JUMP_PLATFORM_LENGTH * jump;
    let lane_half_width = 200.0;
    markers.push(marker(
        v(-150.0, jump_lane_y(0) - 320.0, 160.0),
        if r.jump_measured {
            "jump lanes: gaps and rises are fractions of a full Classic jump as this \
             recreation makes it (measured here; not the original's)"
        } else {
            "jump lanes: gaps and rises are fractions of a Classic jump (estimate)"
        },
    ));
    for (lane, fraction) in JUMP_LANE_GAPS.iter().enumerate() {
        let y = jump_lane_y(lane);
        let gap = fraction * jump;
        markers.push(marker(
            v(-100.0, y, 100.0),
            format!("gap {fraction:.2} x Classic walking jump ({gap:.0} uu)"),
        ));
        for k in 1..=JUMP_LANE_PLATFORMS {
            let n = f32::from(k);
            let x = n * gap + (n - 1.0) * platform_length;
            let top = n * rise;
            boxes.push(solid(
                v(x, y - lane_half_width, top - JUMP_PLATFORM_THICKNESS),
                v(x + platform_length, y + lane_half_width, top),
                format!("jump platform {} of lane {}", k, lane + 1),
            ));
            markers.push(marker(
                v(x + platform_length * 0.5, y, top + 70.0),
                format!(
                    "+{:.2} x Classic jump apex ({top:.0} uu)",
                    n * JUMP_PLATFORM_RISE
                ),
            ));
        }
    }

    // Station 3: stair lanes on their own floor. A pawn walks up low risers
    // and is stopped by high ones; the lanes show where that changes.
    let stairs_end = f32::from(STAIR_STEPS) * STAIR_DEPTH + 400.0;
    boxes.push(solid(
        v(0.0, -2500.0, -SLAB),
        v(stairs_end + 300.0, -600.0, 0.0),
        "stairs floor",
    ));
    for (lane, fraction) in STAIR_RISERS.iter().enumerate() {
        let y = stair_lane_y(lane);
        let riser = fraction * r.step_height;
        markers.push(marker(
            v(-80.0, y, 100.0),
            format!("riser {fraction:.2} x Classic MaxStepHeight ({riser:.1} uu)"),
        ));
        for step in 1..=STAIR_STEPS {
            let n = f32::from(step);
            let x0 = (n - 1.0) * STAIR_DEPTH;
            let x1 = if step == STAIR_STEPS {
                stairs_end
            } else {
                n * STAIR_DEPTH
            };
            boxes.push(solid(
                v(x0, y - 120.0, 0.0),
                v(x1, y + 120.0, n * riser),
                format!("stair {} of lane {}", step, lane + 1),
            ));
        }
    }

    // One checkpoint per station (touching it makes it the respawn point).
    level.checkpoints = vec![
        checkpoint(
            1,
            v(HUB_BACK, -RUNWAY_HALF_WIDTH, 0.0),
            v(0.0, RUNWAY_HALF_WIDTH, 200.0),
            v(HUB_STAND_X, 0.0, 0.0),
            0.0,
        ),
        checkpoint(
            2,
            v(HUB_BACK, 700.0, 0.0),
            v(0.0, 3000.0, 200.0),
            v(HUB_STAND_X, jump_lane_y(0), 0.0),
            0.0,
        ),
        checkpoint(
            3,
            v(HUB_BACK, -2500.0, 0.0),
            v(0.0, -600.0, 200.0),
            v(HUB_STAND_X, stair_lane_y(0), 0.0),
            0.0,
        ),
    ];
    Arena { level, markers }
}

// ---------------------------------------------------------------------------
// grapple-lab
// ---------------------------------------------------------------------------

/// Fan hooks: distance from the start's eye point as a fraction of the
/// Classic grapple range (ours). The last one is out of range on purpose.
pub const RANGE_HOOK_FRACTIONS: [f32; 5] = [0.25, 0.5, 0.75, 0.95, 1.05];
/// Close hooks: distance from the pawn standing on the close-range pad as a
/// fraction of the Classic release distance (ours): two inside, two outside.
pub const RELEASE_HOOK_FRACTIONS: [f32; 4] = [0.5, 0.9, 1.25, 2.0];
/// Half edge of a fan hook's cube, UU (ours).
pub const RANGE_HOOK_HALF_EXTENT: f32 = 50.0;
/// Half edge of a close hook's cube, UU (ours).
pub const RELEASE_HOOK_HALF_EXTENT: f32 = 12.0;
/// Id of the checkpoint on the close-range pad.
pub const CLOSE_PAD_CHECKPOINT: u32 = 2;
/// Where the close-range pad is (the floor point the pawn stands on), ours.
const CLOSE_PAD: Vec3 = Vec3::new(0.0, -1500.0, 2.0);

/// Unit directions built from Pythagorean triples, so placing the hooks
/// needs no trigonometry: `(cos, sin)` of the yaw, then of the elevation.
/// The fan opens ahead of the start (+X); every other station of the arena
/// lies outside it, so each fan hook is in plain sight from the start.
const FAN_YAWS: [(f32, f32); 5] = [(0.6, -0.8), (0.8, -0.6), (1.0, 0.0), (0.8, 0.6), (0.6, 0.8)];
const FAN_ELEVATION: (f32, f32) = (0.96, 0.28);
const CLOSE_YAWS: [(f32, f32); 4] = [(0.6, -0.8), (0.8, -0.6), (0.8, 0.6), (0.6, 0.8)];
const CLOSE_ELEVATION: (f32, f32) = (0.8, 0.6);

fn direction(yaw: (f32, f32), elevation: (f32, f32)) -> Vec3 {
    v(yaw.0 * elevation.0, yaw.1 * elevation.0, elevation.1)
}

fn range_hook_at(r: &Rulers, index: usize) -> Option<Vec3> {
    let eye = r.standing_centre(Vec3::ZERO) + Vec3::Z * r.eye_height;
    let fraction = RANGE_HOOK_FRACTIONS.get(index)?;
    let yaw = FAN_YAWS.get(index)?;
    Some(eye + direction(*yaw, FAN_ELEVATION) * (fraction * r.grapple_range))
}

fn release_hook_at(r: &Rulers, index: usize) -> Option<Vec3> {
    let centre = r.standing_centre(CLOSE_PAD);
    let fraction = RELEASE_HOOK_FRACTIONS.get(index)?;
    let yaw = CLOSE_YAWS.get(index)?;
    Some(centre + direction(*yaw, CLOSE_ELEVATION) * (fraction * r.release_distance))
}

/// Centre of fan hook `index` of the grapple lab (see
/// [`RANGE_HOOK_FRACTIONS`]); `None` past the last one.
#[must_use]
pub fn range_hook(index: usize) -> Option<Vec3> {
    range_hook_at(Rulers::classic(), index)
}

/// Centre of close hook `index` of the grapple lab (see
/// [`RELEASE_HOOK_FRACTIONS`]); `None` past the last one.
#[must_use]
pub fn release_hook(index: usize) -> Option<Vec3> {
    release_hook_at(Rulers::classic(), index)
}

fn grapple_lab(r: &Rulers) -> Arena {
    let mut level = level(GRAPPLE_LAB, Vec3::ZERO, -1500.0);
    let mut markers = Vec::new();

    // One big floor, so a failed swing ends on the ground and not in a
    // respawn. It reaches past the farthest hook ahead and holds the other
    // stations behind and beside the start.
    let reach = (1.05 * r.grapple_range).max(4500.0);
    level.static_boxes.push(solid(
        v(-4500.0, -reach, -SLAB),
        v(reach + 1500.0, reach, 0.0),
        "ground",
    ));
    markers.push(marker(
        v(0.0, 0.0, 140.0),
        "start: the fan of hooks ahead is measured from here",
    ));

    // Station 1: the range fan, ahead.
    for (index, fraction) in RANGE_HOOK_FRACTIONS.iter().enumerate() {
        let Some(position) = range_hook_at(r, index) else {
            continue;
        };
        level.grapple_points.push(GrapplePoint {
            position,
            half_extent: RANGE_HOOK_HALF_EXTENT,
        });
        markers.push(marker(
            position + Vec3::Z * 110.0,
            format!(
                "hook at {fraction:.2} x Classic fMaxDistance ({:.0} uu from the start)",
                fraction * r.grapple_range
            ),
        ));
    }

    // Station 2: hooks around the release distance, measured from a pawn
    // standing on the pad, to the right of the start.
    level.static_boxes.push(solid(
        v(CLOSE_PAD.x - 150.0, CLOSE_PAD.y - 150.0, 0.0),
        v(CLOSE_PAD.x + 150.0, CLOSE_PAD.y + 150.0, CLOSE_PAD.z),
        "close-range pad",
    ));
    markers.push(marker(
        CLOSE_PAD + Vec3::Z * 160.0,
        "close-range pad: the small hooks are measured from a pawn standing here",
    ));
    for (index, fraction) in RELEASE_HOOK_FRACTIONS.iter().enumerate() {
        let Some(position) = release_hook_at(r, index) else {
            continue;
        };
        level.grapple_points.push(GrapplePoint {
            position,
            half_extent: RELEASE_HOOK_HALF_EXTENT,
        });
        markers.push(marker(
            position + Vec3::Z * 40.0,
            format!(
                "hook at {fraction:.2} x Classic fGrappleReleaseDistance ({:.0} uu)",
                fraction * r.release_distance
            ),
        ));
    }

    // Station 3: surfaces, to the left of the start and a little behind it.
    let boxes = &mut level.static_boxes;
    boxes.push(
        StaticBox::new(
            v(-600.0, 1400.0, 0.0),
            v(-200.0, 1800.0, 300.0),
            true,
            "TopOnlyGrappleAble block",
        )
        .with_tag(SurfaceTag::TopOnlyGrappleAble),
    );
    markers.push(marker(
        v(-400.0, 1600.0, 370.0),
        "TopOnlyGrappleAble: only its top face takes the grapple",
    ));
    boxes.push(
        StaticBox::new(
            v(-600.0, 2200.0, 500.0),
            v(-200.0, 2600.0, 560.0),
            true,
            "BottomOnlyGrappleAble slab",
        )
        .with_tag(SurfaceTag::BottomOnlyGrappleAble),
    );
    markers.push(marker(
        v(-400.0, 2400.0, 630.0),
        "BottomOnlyGrappleAble: only its underside takes the grapple",
    ));
    boxes.push(
        StaticBox::new(
            v(-600.0, 3000.0, 0.0),
            v(-200.0, 3400.0, 20.0),
            true,
            "NotLandable pad",
        )
        .with_tag(SurfaceTag::NotLandable),
    );
    markers.push(marker(
        v(-400.0, 3200.0, 90.0),
        "NotLandable: landing here does not count as a landing",
    ));
    boxes.push(solid(
        v(-1240.0, 1400.0, 0.0),
        v(-1200.0, 3400.0, 800.0),
        "wall (not grapple-able)",
    ));
    markers.push(marker(v(-1220.0, 2400.0, 870.0), "wall: not grapple-able"));

    // Station 4: a beam and the objects, behind the start.
    boxes.push(StaticBox::new(
        v(-2700.0, -300.0, 700.0),
        v(-1500.0, 300.0, 760.0),
        true,
        "grapple beam",
    ));
    markers.push(marker(v(-2100.0, 0.0, 830.0), "grapple-able beam"));
    level.crystals.push(RechargeCrystal {
        id: 101,
        center: v(-1500.0, -2500.0, 600.0),
        half_extent: 30.0,
        recharge_delay: CRYSTAL_DEFAULT_RECHARGE_DELAY,
        should_recharge: true,
        parent_crystal: false,
        linked_parent: None,
    });
    markers.push(marker(
        v(-1500.0, -2500.0, 680.0),
        "recharge crystal: grappling it refills the grapples",
    ));
    level.movers.push(Mover {
        id: 301,
        min: v(-2900.0, -3600.0, 500.0),
        max: v(-2700.0, -3400.0, 560.0),
        // Our own back-and-forth path; not a motion of the original.
        path: MoverPath::PingPong {
            offset: v(0.0, 1500.0, 0.0),
            period: 8.0,
        },
        label: "moving block (our path)".into(),
    });
    markers.push(marker(
        v(-2800.0, -3500.0, 640.0),
        "moving block: the anchor follows it",
    ));
    level
        .attractors
        .push(Attractor::as_placed(501, v(-2500.0, 1500.0, 250.0)));
    markers.push(marker(
        v(-2500.0, 1500.0, 330.0),
        "attractor pad (inactive until activated)",
    ));

    level.checkpoints = vec![
        checkpoint(
            1,
            v(-300.0, -300.0, 0.0),
            v(300.0, 300.0, 200.0),
            Vec3::ZERO,
            0.0,
        ),
        checkpoint(
            CLOSE_PAD_CHECKPOINT,
            v(CLOSE_PAD.x - 150.0, CLOSE_PAD.y - 150.0, 0.0),
            v(CLOSE_PAD.x + 150.0, CLOSE_PAD.y + 150.0, 250.0),
            CLOSE_PAD,
            0.0,
        ),
        // In front of the surfaces, looking at them (towards -X).
        checkpoint(
            3,
            v(0.0, 1400.0, 0.0),
            v(400.0, 3400.0, 200.0),
            v(200.0, 2400.0, 0.0),
            std::f32::consts::PI,
        ),
    ];
    Arena { level, markers }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parameter_rulers_come_from_the_classic_set() {
        let p = PlayerParams::asamu_original();
        let r = Rulers::classic();
        let pawn = p.pawn.as_ref().unwrap();
        let gun = p.gun.as_ref().unwrap();
        assert_eq!(r.walk_speed, pawn.move_speed.value);
        assert_eq!(
            r.sprint_speed,
            pawn.move_speed.value * pawn.sprint_speed_multiplier.value
        );
        assert_eq!(r.step_height, p.movement.step_height.value);
        assert_eq!(r.half_height, p.movement.capsule_half_height.value);
        assert_eq!(r.eye_height, p.camera.eye_height.value);
        assert_eq!(r.grapple_range, gun.max_distance.value);
        assert_eq!(r.release_distance, gun.release_distance.value);
        // Cached: the same rulers every time.
        assert_eq!(*Rulers::classic(), Rulers::of(&p));
    }

    #[test]
    fn jump_rulers_are_measured_in_this_recreation() {
        let r = Rulers::classic();
        assert!(r.jump_measured, "the Classic jump lands on the flat floor");
        assert!(r.jump_apex > 1.0 && r.jump_apex.is_finite());
        assert!(r.walking_jump_distance > 1.0 && r.walking_jump_distance.is_finite());
        // The measurement is repeatable (the simulation is deterministic).
        let p = PlayerParams::asamu_original();
        assert_eq!(
            measure_jumps(&p),
            Some((r.jump_apex, r.walking_jump_distance))
        );
        // A standing jump covers no ground; a walking one does.
        assert!(measured_jump(&p, 0).unwrap().distance < 1.0);
        assert!(measured_jump(&p, MEASURE_RUN_UP_TICKS).unwrap().distance > 1.0);
    }

    #[test]
    fn hook_distances_are_the_labelled_fractions() {
        let r = Rulers::classic();
        let eye = r.standing_centre(Vec3::ZERO) + Vec3::Z * r.eye_height;
        for (index, fraction) in RANGE_HOOK_FRACTIONS.iter().enumerate() {
            let hook = range_hook(index).unwrap();
            let distance = (hook - eye).length();
            assert!(
                (distance - fraction * r.grapple_range).abs() < 0.5,
                "{distance}"
            );
        }
        let centre = r.standing_centre(CLOSE_PAD);
        for (index, fraction) in RELEASE_HOOK_FRACTIONS.iter().enumerate() {
            let hook = release_hook(index).unwrap();
            let distance = (hook - centre).length();
            assert!(
                (distance - fraction * r.release_distance).abs() < 0.01,
                "{distance}"
            );
        }
        assert!(range_hook(RANGE_HOOK_FRACTIONS.len()).is_none());
        assert!(release_hook(RELEASE_HOOK_FRACTIONS.len()).is_none());
    }

    #[test]
    fn markers_and_levels_are_built_together() {
        for info in builtin_arenas() {
            let level = build_arena(info.id).unwrap();
            let markers = arena_markers(info.id);
            assert!(!markers.is_empty(), "{}", info.id);
            assert!(markers.iter().all(|m| m.position.is_finite()));
            assert!(markers.iter().all(|m| !m.text.is_empty()));
            assert_eq!(level.name, arena_level_name(info.id));
        }
        assert!(build_arena("no-such-arena").is_none());
        assert!(arena_markers("no-such-arena").is_empty());
    }
}
