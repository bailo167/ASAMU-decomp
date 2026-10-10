//! NPCs and story actors as deterministic state machines
//! (`docs/reverse-engineering/NPCS.md`).
//!
//! Behaviour comes from local reading of the original's script classes
//! (`ASAMUNPC*`, `ASAMUNPC_Worm*`, `ASAMUWorm*Volume`, `ASAMUInteractable_Actor`,
//! `ASAMUCollectible`, `ASAMUGlowFlower`, `ASAMUSoundMakingFoliage`,
//! `ASAMUBackpackMaddie` and their Kismet actions/events), their class
//! defaults, the worm's `AnimTree` data and the engine natives that run them.
//! Everything here is described and implemented in our own words; no script
//! text is reproduced. Constants carry their source.
//!
//! What the shipped maps contain (CONFIRMED, map census in NPCS.md): one
//! `ASAMUNPC_WormPawn` (AG-Darkcave) with one `ASAMUWormScreamVolume` and no
//! shadow volume; **no** `ASAMUNPC_MaddiePawn`, `ASAMUNPC_VillagerPawn`,
//! `ASAMUBackpackMaddie` or `PathNode`. Maddie and the villagers appear as
//! `SkeletalMeshActor`s that loop an ambient animation or follow Matinee
//! ([`SkinnedActorDef`]). The Maddie/villager/backpack state machines are
//! implemented from their scripts for completeness and are exercised by
//! synthetic tests only.
//!
//! Timing follows the engine rules already used by [`crate::objects`]:
//! latent sleeps per GRAPPLE.md G-TM-3, timers per G-TM-4 (strict `>`),
//! state-local variables cleared on a state change (G-TM-5), and the
//! state-code runner `AActor::ProcessState` (CONFIRMED (native) @
//! 0x100B40E20): a `GotoState` executed *by state code* continues with the
//! new state's code in the same tick (at most four state changes per tick);
//! a `GotoState` from an event, timer or animation callback runs the new
//! state's code at the actor's next state-code run.
//!
//! Random choices use [`Rng`] (SplitMix64, ours), seeded per actor; the
//! original's random stream is not reproducible.
//!
//! Skinned actors ([`SkinnedActorDef`]) also carry simulation state
//! ([`SkinnedAnimState`]): their own sequence node ticks and fires its
//! animation notifies ([`crate::anim`]; Kismet `SeqEvent_AnimNotify`, sound
//! notifies), Matinee's animation tracks drive a slot node, and
//! `SeqAct_SetLookAtTarget` and Matinee's skeletal-control tracks set the
//! look-at offsets and strengths the renderer uses.
//!
//! Conventions: UE3 axes and UU (see the crate docs). The NPC pawns'
//! collision cylinders ([`PawnCollisionDef`], [`NpcRuntime::pawn_cylinders`])
//! block the player through the collision scene
//! ([`crate::collision::CollisionScene::add_cylinder_mesh`]); the game places
//! them.

use std::collections::{BTreeMap, BTreeSet};

use glam::{DVec2, DVec3, Vec3};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::anim::{NotifyDef, NotifyKind, SequenceInfo, SequenceNode};
use crate::gameplay::Rng;
use crate::objects::{LATENT_WAKE_FRACTION, ObjectEvent};
use crate::scene::{
    DataSource, HullJson, LoadOptions, Mat4, SceneError, StreamingLevelJson, SubLevel, VolumeJson,
    actor_id, object_name, param, param_bool, param_f32, param_i64, param_str,
};

// ---------------------------------------------------------------------------
// Constants (each with its source; see NPCS.md for the tables).
// ---------------------------------------------------------------------------

/// `PlayerPush`, uu/s. ScriptDefault `asamu.ASAMUNPC_Worm.PlayerPush`.
/// CONFIRMED (cdo).
pub const WORM_PLAYER_PUSH: f32 = 500.0;
/// `wakeUpTime`, s. ScriptDefault `asamu.ASAMUNPC_Worm`. CONFIRMED (cdo).
pub const WORM_WAKE_UP_TIME: f32 = 4.0;
/// `awakeTimeMin`, s. ScriptDefault `asamu.ASAMUNPC_Worm`. CONFIRMED (cdo).
pub const WORM_AWAKE_TIME_MIN: f32 = 6.0;
/// `awakeTimeMax`, s. ScriptDefault `asamu.ASAMUNPC_Worm`. CONFIRMED (cdo).
pub const WORM_AWAKE_TIME_MAX: f32 = 8.0;
/// `alertedSleepTime`, s. ScriptDefault `asamu.ASAMUNPC_Worm`. CONFIRMED (cdo).
pub const WORM_ALERTED_SLEEP_TIME: f32 = 1.0;
/// `alertedTime`, s. ScriptDefault `asamu.ASAMUNPC_Worm`. CONFIRMED (cdo).
pub const WORM_ALERTED_TIME: f32 = 4.0;
/// `fallAsleepTime`, s. ScriptDefault `asamu.ASAMUNPC_Worm`. CONFIRMED (cdo).
pub const WORM_FALL_ASLEEP_TIME: f32 = 2.0;
/// `sleepTimeMin`, s. ScriptDefault `asamu.ASAMUNPC_Worm`. CONFIRMED (cdo).
pub const WORM_SLEEP_TIME_MIN: f32 = 12.0;
/// `sleepTimeMax`, s. ScriptDefault `asamu.ASAMUNPC_Worm`. CONFIRMED (cdo).
pub const WORM_SLEEP_TIME_MAX: f32 = 14.0;
/// `screamTimeMax`, s. ScriptDefault `asamu.ASAMUNPC_Worm`. CONFIRMED (cdo).
pub const WORM_SCREAM_TIME_MAX: f32 = 8.0;
/// `zVelocityOffset`, uu/s. ScriptDefault `asamu.ASAMUNPC_Worm`. CONFIRMED (cdo).
pub const WORM_Z_VELOCITY_OFFSET: f32 = -50.0;
/// `lookAroundSpeed`. ScriptDefault `asamu.ASAMUNPC_Worm`. CONFIRMED (cdo).
pub const WORM_LOOK_AROUND_SPEED: f32 = 0.5;
/// `positionSensitivity`, uu. ScriptDefault `asamu.ASAMUNPC_Worm`. CONFIRMED (cdo).
pub const WORM_POSITION_SENSITIVITY: f32 = 25.0;
/// Step of every worm loop (light fades, player checks, pushes), s.
/// ScriptCode `asamu.ASAMUNPC_Worm`. CONFIRMED (src).
pub const WORM_STEP: f32 = 0.1;
/// Factor on `lookAroundSpeed` while the player is discovered. ScriptCode
/// `asamu.ASAMUNPC_Worm.Tick`. CONFIRMED (src).
pub const WORM_DISCOVERED_AIM_FACTOR: f32 = 5.0;
/// `RandRange(0, 100)` threshold of the worm's look-side choices. ScriptCode
/// `asamu.ASAMUNPC_Worm` / `ASAMUNPC_WormPawn`. CONFIRMED (src).
pub const WORM_LOOK_CHANCE: f32 = 50.0;
/// Light `Radius` of the worm pawn's spot light. ScriptDefault
/// `asamu.ASAMUNPC_WormPawn.Radius`. CONFIRMED (cdo); the placed worm
/// overrides it (100 000).
pub const WORM_LIGHT_RADIUS: f32 = 15_000.0;
/// Light `Brightness`. ScriptDefault `asamu.ASAMUNPC_WormPawn`. CONFIRMED
/// (cdo); the placed worm overrides it (25).
pub const WORM_LIGHT_BRIGHTNESS: f32 = 2_500.0;
/// `OuterConeAngle`, degrees. ScriptDefault `asamu.ASAMUNPC_WormPawn`.
/// CONFIRMED (cdo); placed worm 40.
pub const WORM_LIGHT_OUTER_CONE: f32 = 90.0;
/// `InnerConeAngle`, degrees. ScriptDefault `asamu.ASAMUNPC_WormPawn`.
/// CONFIRMED (cdo); placed worm 15.
pub const WORM_LIGHT_INNER_CONE: f32 = 45.0;

/// `GroundSpeed` of every NPC pawn, uu/s. ScriptDefault
/// `asamu.ASAMUNPC_Pawn.GroundSpeed`. CONFIRMED (cdo).
pub const NPC_GROUND_SPEED: f32 = 200.0;
/// Collision radius of `ASAMUNPC_Pawn` (and Maddie/worm, which inherit the
/// template). Template `Engine.Default__Pawn.CollisionCylinder`, unchanged
/// through `GamePawn`, `UDKPawn` and `ASAMUNPC_Pawn`. CONFIRMED (cdo).
pub const NPC_COLLISION_RADIUS: f32 = 34.0;
/// Collision half-height of `ASAMUNPC_Pawn`. Same template. CONFIRMED (cdo).
pub const NPC_COLLISION_HALF_HEIGHT: f32 = 78.0;
/// Collision radius of `ASAMUNPC_VillagerPawn`. Template
/// `asamu.Default__ASAMUNPC_VillagerPawn.CollisionCylinder`. CONFIRMED (cdo).
pub const VILLAGER_COLLISION_RADIUS: f32 = 16.0;
/// Collision half-height of `ASAMUNPC_VillagerPawn`. Same template.
/// CONFIRMED (cdo).
pub const VILLAGER_COLLISION_HALF_HEIGHT: f32 = 52.0;
/// `reachedDestinationTolerance`, uu. ScriptDefault
/// `asamu.ASAMUNPC_Villager`. CONFIRMED (cdo).
pub const VILLAGER_REACHED_TOLERANCE: f32 = 300.0;
/// `SightRadius` of the villager pawn, uu. ScriptDefault `Engine.Pawn`.
/// CONFIRMED (cdo).
pub const VILLAGER_SIGHT_RADIUS: f32 = 5_000.0;
/// `PeripheralVision` (cosine) of the villager pawn. ScriptDefault
/// `asamu.ASAMUNPC_VillagerPawn`. CONFIRMED (cdo).
pub const VILLAGER_PERIPHERAL_VISION: f32 = 0.4;
/// Chance (percent of `RandRange(0, 100)`) that an idle villager pauses
/// first. ScriptCode `asamu.ASAMUNPC_Villager` state `Idle`. CONFIRMED (src).
pub const VILLAGER_IDLE_PAUSE_CHANCE: f32 = 20.0;
/// Idle pause range, s. ScriptCode `asamu.ASAMUNPC_Villager`. CONFIRMED (src).
pub const VILLAGER_IDLE_PAUSE: (f32, f32) = (1.0, 3.0);
/// Wait at the end of a scripted path, s. ScriptCode
/// `asamu.ASAMUNPC_Villager` state `WalkingScriptedPath`. CONFIRMED (src).
pub const VILLAGER_PATH_END_WAIT: (f32, f32) = (0.0, 5.0);
/// Poll interval of the villager's walking loops, s. ScriptCode. CONFIRMED (src).
pub const VILLAGER_POLL: f32 = 0.1;

/// Distance at which Maddie leaves `TalkingWithPlayer`, uu. ScriptCode
/// `asamu.ASAMUNPC_MaddiePawn`. CONFIRMED (src).
pub const MADDIE_TALK_RANGE: f32 = 200.0;
/// Animation the backpack Maddie plays for `Wave`. ScriptCode
/// `asamu.ASAMUBackpackMaddie.GetAnimFromEnum`. CONFIRMED (src).
pub const BACKPACK_WAVE_SEQUENCE: &str = "Maddie_Arm_Animtest";
/// Mesh of the backpack Maddie. Template
/// `asamu.Default__ASAMUBackpackMaddie.SkeletalMeshComponent0`. CONFIRMED (cdo).
pub const BACKPACK_MESH: &str = "Maddie.Meshes.Maddie_Arm_Animtest";
/// Socket of the grapple hand mesh the backpack attaches to. ScriptCode
/// `asamu.ASAMUBackpackMaddie.AttachMaddieToPlayer`. CONFIRMED (src).
pub const BACKPACK_SOCKET: &str = "RootSocket";

/// Pick-up trigger radius of a collectible, uu. Template
/// `asamu.Default__ASAMUCollectible.Trigger`. CONFIRMED (cdo).
pub const COLLECTIBLE_TRIGGER_RADIUS: f32 = 50.0;
/// Pick-up trigger half-height, uu. Same template. CONFIRMED (cdo).
pub const COLLECTIBLE_TRIGGER_HALF_HEIGHT: f32 = 40.0;
/// Pick-up trigger offset above the actor, uu. Same template
/// (`Translation`). CONFIRMED (cdo).
pub const COLLECTIBLE_TRIGGER_Z: f32 = 40.0;
/// `COLLECTIBLES_PER_LEVEL`. ScriptDefault `asamu.ASAMUProgressionManager`.
/// CONFIRMED (cdo; SAVE.md 6.3).
pub const COLLECTIBLES_PER_LEVEL: usize = 5;
/// Total collectibles (5 levels × 5). CONFIRMED (cdo, map; SAVE.md 6.3).
pub const COLLECTIBLES_TOTAL: usize = 25;
/// `TOTAL_INTERACTABLES_COUNT` (optional story keys). ScriptDefault
/// `asamu.ASAMUProgressionManager`. CONFIRMED (cdo; SAVE.md 6.3).
pub const STORY_ITEMS_TOTAL: usize = 11;

/// Default `MaxInteractTimes` of a story interactable. ScriptDefault
/// `asamu.ASAMUInteractable_Actor`. CONFIRMED (cdo).
pub const INTERACTABLE_MAX_INTERACT_TIMES: i32 = 1;
/// Fade steps of the interact symbol (the loop runs this plus one times).
/// ScriptCode `asamu.ASAMUInteractable_Actor` (`STEPS_PER_SECOND`).
/// CONFIRMED (src).
pub const INTERACTABLE_FADE_STEPS: u32 = 100;
/// Latent sleep per fade step, s. ScriptCode `asamu.ASAMUInteractable_Actor`
/// (`SLEEP_TIME`). CONFIRMED (src).
pub const INTERACTABLE_FADE_SLEEP: f32 = 0.01;

/// `glowDuration`, s. ScriptDefault `asamu.ASAMUGlowFlower`. CONFIRMED (cdo).
pub const GLOW_FLOWER_DURATION: f32 = 10.0;
/// `FadeTime`, s. ScriptDefault `asamu.ASAMUGlowFlower`. CONFIRMED (cdo).
pub const GLOW_FLOWER_FADE_TIME: f32 = 1.0;
/// Update rate of the glow loops, s. ScriptCode `asamu.ASAMUGlowFlower`
/// (`UPDATE_RATE`). CONFIRMED (src).
pub const GLOW_FLOWER_UPDATE_RATE: f32 = 0.016_667;

/// Touch trigger radius of sound-making foliage, uu. Template
/// `asamu.Default__ASAMUSoundMakingFoliage.Trigger`. CONFIRMED (cdo).
pub const FOLIAGE_TRIGGER_RADIUS: f32 = 50.0;
/// Touch trigger half-height, uu. Same template. CONFIRMED (cdo).
pub const FOLIAGE_TRIGGER_HALF_HEIGHT: f32 = 40.0;
/// Touch trigger offset above the actor, uu. Same template. CONFIRMED (cdo).
pub const FOLIAGE_TRIGGER_Z: f32 = 40.0;

/// Grid cell of the foliage touch index, uu (ours; performance only).
const FOLIAGE_CELL: f32 = 1024.0;

// ---------------------------------------------------------------------------
// Small helpers.
// ---------------------------------------------------------------------------

/// G-TM-3 latent sleep poll; `true` when the sleep ended this tick.
fn poll_sleep(remaining: &mut Option<f32>, dt: f32) -> bool {
    let Some(r) = *remaining else {
        return false;
    };
    let r = r - dt;
    if f64::from(r) < LATENT_WAKE_FRACTION * f64::from(dt) {
        *remaining = None;
        true
    } else {
        *remaining = Some(r);
        false
    }
}

/// UnrealScript `Normal` (zero below a squared length of 1e-8).
fn safe_normal(v: Vec3) -> Vec3 {
    let sq = v.length_squared();
    if sq == 1.0 {
        v
    } else if sq.is_finite() && sq >= 1.0e-8 {
        v * (1.0 / sq.sqrt())
    } else {
        Vec3::ZERO
    }
}

/// Stock `FInterpTo` (TENTATIVE: UE3 formula, the native was not re-read).
#[must_use]
pub fn finterp_to(current: f32, target: f32, dt: f32, speed: f32) -> f32 {
    if speed <= 0.0 {
        return target;
    }
    let dist = target - current;
    if dist * dist < 1.0e-8 {
        return target;
    }
    current + dist * (dt * speed).clamp(0.0, 1.0)
}

/// A one-shot timer (G-TM-4: fires when the count exceeds the rate).
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ScriptTimer {
    /// Elapsed, s.
    pub count: f32,
    /// Rate, s.
    pub rate: f32,
}

impl ScriptTimer {
    fn new(rate: f32) -> Self {
        Self { count: 0.0, rate }
    }

    /// Advances; `true` when it fires (the caller removes it).
    fn advance(&mut self, dt: f32) -> bool {
        self.count += dt;
        self.count > self.rate
    }
}

/// A vertical cylinder (trigger shapes).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct NpcCylinder {
    /// Centre, UU.
    pub center: Vec3,
    /// Radius, UU.
    pub radius: f32,
    /// Half-height, UU.
    pub half_height: f32,
}

/// Interval of `t ∈ [0, 1]` where the swept player cylinder `a → b`
/// (radius `r`, half-height `h`) overlaps `c` (touching excluded).
fn swept_cylinder(a: Vec3, b: Vec3, r: f32, h: f32, c: &NpcCylinder) -> Option<(f64, f64)> {
    let (a, b) = (a.as_dvec3(), b.as_dvec3());
    let center = c.center.as_dvec3();
    let d = b - a;
    let (mut lo, mut hi) = (0.0f64, 1.0f64);
    let hz = f64::from(h) + f64::from(c.half_height);
    let z0 = a.z - center.z;
    if d.z == 0.0 {
        if z0.abs() >= hz {
            return None;
        }
    } else {
        let mut t0 = (-hz - z0) / d.z;
        let mut t1 = (hz - z0) / d.z;
        if t0 > t1 {
            core::mem::swap(&mut t0, &mut t1);
        }
        lo = lo.max(t0);
        hi = hi.min(t1);
    }
    let rr = f64::from(r) + f64::from(c.radius);
    let p0 = DVec2::new(a.x - center.x, a.y - center.y);
    let v = DVec2::new(d.x, d.y);
    let qa = v.dot(v);
    let qb = p0.dot(v);
    let qc = p0.dot(p0) - rr * rr;
    if qa == 0.0 {
        if qc >= 0.0 {
            return None;
        }
    } else {
        let disc = qb * qb - qa * qc;
        if disc <= 0.0 {
            return None;
        }
        let s = disc.sqrt();
        lo = lo.max((-qb - s) / qa);
        hi = hi.min((-qb + s) / qa);
    }
    (lo < hi && lo.is_finite() && hi.is_finite()).then_some((lo, hi))
}

/// `true` when the player cylinder at `p` overlaps `c` (touching excluded).
fn overlaps_cylinder(p: Vec3, r: f32, h: f32, c: &NpcCylinder) -> bool {
    let dz = (p.z - c.center.z).abs();
    let dxy = (p.truncate() - c.center.truncate()).length();
    dz < h + c.half_height && dxy < r + c.radius
}

// ---------------------------------------------------------------------------
// Convex volumes (worm scream / shadow volumes).
// ---------------------------------------------------------------------------

/// A convex hull with outward planes `n · p = w`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct NpcHull {
    /// `[nx, ny, nz, w]` per plane (outward unit normal).
    pub planes: Vec<[f64; 4]>,
    /// Bounds minimum.
    pub min: Vec3,
    /// Bounds maximum.
    pub max: Vec3,
}

impl NpcHull {
    /// An axis-aligned box hull (synthetic data and tests).
    #[must_use]
    pub fn from_box(min: Vec3, max: Vec3) -> Self {
        let (lo, hi) = (min.min(max).as_dvec3(), min.max(max).as_dvec3());
        Self {
            planes: vec![
                [1.0, 0.0, 0.0, hi.x],
                [-1.0, 0.0, 0.0, -lo.x],
                [0.0, 1.0, 0.0, hi.y],
                [0.0, -1.0, 0.0, -lo.y],
                [0.0, 0.0, 1.0, hi.z],
                [0.0, 0.0, -1.0, -lo.z],
            ],
            min: min.min(max),
            max: min.max(max),
        }
    }

    /// `true` when `p` is inside or on the hull (`Volume.Encompasses` tests the
    /// actor's location point; TENTATIVE stock rule).
    #[must_use]
    pub fn contains(&self, p: Vec3) -> bool {
        if p.cmplt(self.min).any() || p.cmpgt(self.max).any() {
            return false;
        }
        let q = p.as_dvec3();
        self.planes
            .iter()
            .all(|pl| DVec3::new(pl[0], pl[1], pl[2]).dot(q) <= pl[3] + 1.0e-6)
    }

    /// Builds a world-space hull from the importer's convex element (planes
    /// oriented away from the vertex centroid; recomputed from the triangles
    /// when the importer left them out). `None` for degenerate input.
    #[must_use]
    pub fn from_json(h: &HullJson, offset: Vec3) -> Option<Self> {
        let off = offset.as_dvec3();
        let verts: Vec<DVec3> = h
            .vertices
            .iter()
            .map(|v| Vec3::from_array(*v).as_dvec3() + off)
            .filter(|v| v.is_finite())
            .collect();
        if verts.len() < 4 {
            return None;
        }
        let centroid = verts.iter().copied().sum::<DVec3>() / verts.len() as f64;
        let mut planes = Vec::new();
        for p in &h.planes {
            let n = DVec3::new(f64::from(p[0]), f64::from(p[1]), f64::from(p[2]));
            let len = n.length();
            if !(len.is_finite() && len > 0.0 && p[3].is_finite()) {
                continue;
            }
            let n = n / len;
            let w = f64::from(p[3]) / len + n.dot(off);
            planes.push(if n.dot(centroid) > w {
                [-n.x, -n.y, -n.z, -w]
            } else {
                [n.x, n.y, n.z, w]
            });
        }
        if planes.is_empty() {
            for t in &h.triangles {
                let (Some(a), Some(b), Some(c)) = (
                    h.vertices.get(t[0] as usize),
                    h.vertices.get(t[1] as usize),
                    h.vertices.get(t[2] as usize),
                ) else {
                    continue;
                };
                let [a, b, c] = [a, b, c].map(|v| Vec3::from_array(*v).as_dvec3() + off);
                let n = (b - a).cross(c - a);
                let len = n.length();
                if !(len.is_finite() && len > 1.0e-12) {
                    continue;
                }
                let n = n / len;
                let n = if n.dot(a - centroid) < 0.0 { -n } else { n };
                planes.push([n.x, n.y, n.z, n.dot(a)]);
            }
        }
        if planes.is_empty() {
            return None;
        }
        let mut min = DVec3::splat(f64::INFINITY);
        let mut max = DVec3::splat(f64::NEG_INFINITY);
        for v in &verts {
            min = min.min(*v);
            max = max.max(*v);
        }
        Some(Self {
            planes,
            min: min.as_vec3() - Vec3::splat(1.0e-3),
            max: max.as_vec3() + Vec3::splat(1.0e-3),
        })
    }
}

// ---------------------------------------------------------------------------
// Definitions (from the importer's scenes, or synthetic).
// ---------------------------------------------------------------------------

/// The worm controller's tuning (`asamu.ASAMUNPC_Worm` class defaults; the
/// controller is spawned at run time, so no map can override them).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct WormParams {
    /// `PlayerPush`, uu/s.
    pub player_push: f32,
    /// `wakeUpTime`, s.
    pub wake_up_time: f32,
    /// `awakeTimeMin`, s.
    pub awake_time_min: f32,
    /// `awakeTimeMax`, s.
    pub awake_time_max: f32,
    /// `alertedSleepTime`, s.
    pub alerted_sleep_time: f32,
    /// `alertedTime`, s.
    pub alerted_time: f32,
    /// `fallAsleepTime`, s.
    pub fall_asleep_time: f32,
    /// `sleepTimeMin`, s.
    pub sleep_time_min: f32,
    /// `sleepTimeMax`, s.
    pub sleep_time_max: f32,
    /// `screamTimeMax`, s.
    pub scream_time_max: f32,
    /// `zVelocityOffset`, uu/s.
    pub z_velocity_offset: f32,
    /// `lookAroundSpeed`.
    pub look_around_speed: f32,
    /// `positionSensitivity`, uu.
    pub position_sensitivity: f32,
}

impl WormParams {
    /// The class defaults (CONFIRMED (cdo)).
    pub const ORIGINAL: Self = Self {
        player_push: WORM_PLAYER_PUSH,
        wake_up_time: WORM_WAKE_UP_TIME,
        awake_time_min: WORM_AWAKE_TIME_MIN,
        awake_time_max: WORM_AWAKE_TIME_MAX,
        alerted_sleep_time: WORM_ALERTED_SLEEP_TIME,
        alerted_time: WORM_ALERTED_TIME,
        fall_asleep_time: WORM_FALL_ASLEEP_TIME,
        sleep_time_min: WORM_SLEEP_TIME_MIN,
        sleep_time_max: WORM_SLEEP_TIME_MAX,
        scream_time_max: WORM_SCREAM_TIME_MAX,
        z_velocity_offset: WORM_Z_VELOCITY_OFFSET,
        look_around_speed: WORM_LOOK_AROUND_SPEED,
        position_sensitivity: WORM_POSITION_SENSITIVITY,
    };
}

impl Default for WormParams {
    fn default() -> Self {
        Self::ORIGINAL
    }
}

/// The worm's eye spot light (visual).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct WormLight {
    /// `Radius` at full strength, uu.
    pub radius: f32,
    /// `Brightness` at full strength.
    pub brightness: f32,
    /// `LightColor` (R, G, B, A).
    pub color: [u8; 4],
    /// `OuterConeAngle`, degrees.
    pub outer_cone: f32,
    /// `InnerConeAngle`, degrees.
    pub inner_cone: f32,
}

impl Default for WormLight {
    fn default() -> Self {
        Self {
            radius: WORM_LIGHT_RADIUS,
            brightness: WORM_LIGHT_BRIGHTNESS,
            color: [255, 255, 255, 255],
            outer_cone: WORM_LIGHT_OUTER_CONE,
            inner_cone: WORM_LIGHT_INNER_CONE,
        }
    }
}

/// A skeletal mesh component of a placed actor (presentation).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SkinnedComponent {
    /// Component object name.
    pub name: String,
    /// `SkeletalMesh` object path.
    pub mesh: String,
    /// Component world matrix (UE3 row-vector convention).
    pub local_to_world: Mat4,
    /// `HiddenGame`.
    pub hidden: bool,
    /// The component's own animation node, when the importer exports it
    /// (see [`AnimHint`]).
    pub animation: Option<AnimHint>,
}

/// The `AnimNodeSequence` a component plays at level start (`AnimSeqName`,
/// `bLooping`, `bPlaying`, `CurrentTime`, `Rate`). Read from the optional
/// component field `animation` of the scene JSON, else from the importer's
/// `matinee/<map>.actors.json` (`asamu-import matinee`; NPCS.md §6).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AnimHint {
    /// `AnimSeqName`.
    pub sequence: String,
    /// `bLooping`.
    #[serde(default)]
    pub looping: bool,
    /// `bPlaying`.
    #[serde(default)]
    pub playing: bool,
    /// `CurrentTime`, s.
    #[serde(default)]
    pub start_time: f32,
    /// `Rate`.
    #[serde(default = "one")]
    pub rate: f32,
}

fn one() -> f32 {
    1.0
}

/// The placed worm (`ASAMUNPC_WormPawn` + its `ASAMUNPC_Worm` controller).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct WormDef {
    /// Actor id.
    pub id: u32,
    /// Object name.
    pub name: String,
    /// Pawn location, UU (the push direction is measured from it).
    pub location: Vec3,
    /// Pawn rotation (pitch, yaw, roll).
    pub rotation: [i32; 3],
    /// `RandomLookAtTargets` locations (stored; the AI never uses them:
    /// `LookAtNewSpot` is never scheduled, CONFIRMED (src)).
    pub look_targets: Vec<Vec3>,
    /// Spot light.
    pub light: WormLight,
    /// Controller tuning.
    pub params: WormParams,
    /// Skeletal mesh components (presentation).
    pub meshes: Vec<SkinnedComponent>,
}

/// What a worm volume does.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WormVolumeRole {
    /// `ASAMUWormScreamVolume`: the worm only reacts while the player is
    /// inside one.
    Scream,
    /// `ASAMUWormShadowVolume`: the worm ignores a player inside one (none
    /// placed).
    Shadow,
}

/// A worm scream/shadow volume.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct WormVolumeDef {
    /// Actor id.
    pub id: u32,
    /// Object name.
    pub name: String,
    /// Role.
    pub role: WormVolumeRole,
    /// Convex hulls (world space).
    pub hulls: Vec<NpcHull>,
}

impl WormVolumeDef {
    /// `CheckForPawn` (`Encompasses`): the point is inside one of the hulls.
    #[must_use]
    pub fn encompasses(&self, p: Vec3) -> bool {
        self.hulls.iter().any(|h| h.contains(p))
    }
}

/// `ASAMUNPC_MaddiePawn` (none placed in the shipped maps).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MaddieDef {
    /// Actor id.
    pub id: u32,
    /// Object name.
    pub name: String,
    /// Location, UU.
    pub location: Vec3,
    /// Rotation.
    pub rotation: [i32; 3],
    /// Skeletal mesh components.
    pub meshes: Vec<SkinnedComponent>,
}

/// A navigation point (`PathNode`, `PlayerStart`, ...): villager destinations.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct NavPoint {
    /// Actor id.
    pub id: u32,
    /// Location, UU.
    pub location: Vec3,
    /// Its cylinder radius (0 when unknown).
    pub radius: f32,
}

/// `ASAMUNPC_VillagerPawn` (none placed in the shipped maps).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct VillagerDef {
    /// Actor id.
    pub id: u32,
    /// Object name.
    pub name: String,
    /// Start location, UU.
    pub location: Vec3,
    /// Start rotation.
    pub rotation: [i32; 3],
    /// `bUseScriptedPath`.
    pub use_scripted_path: bool,
    /// `scriptedPath` (resolved path nodes).
    pub scripted_path: Vec<NavPoint>,
    /// Skeletal mesh components.
    pub meshes: Vec<SkinnedComponent>,
}

/// An `ASAMUCollectible`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CollectibleDef {
    /// Actor id.
    pub id: u32,
    /// Level (package) the actor belongs to.
    pub level: String,
    /// Object name (`ASAMUCollectible_3`); the save key is
    /// `TheWorld.PersistentLevel.<name>` (SAVE.md 9.2).
    pub name: String,
    /// Location, UU.
    pub location: Vec3,
    /// Pick-up trigger.
    pub trigger: NpcCylinder,
}

/// An `ASAMUInteractable_Actor` (story interactable / optional story item).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct StoryItemDef {
    /// Actor id.
    pub id: u32,
    /// Level (package).
    pub level: String,
    /// Object name.
    pub name: String,
    /// Location, UU.
    pub location: Vec3,
    /// `MaxInteractTimes` (0 = unlimited).
    pub max_interact_times: i32,
    /// `bIsOptional` (counts for the `INTERACT_ALL_STORY` achievement).
    pub optional: bool,
    /// `bParentInteractable`.
    pub parent: bool,
    /// `linkedParentActor` (id).
    pub linked_parent: Option<u32>,
    /// `linkedInteractables` as stored (ids).
    pub linked_children: Vec<u32>,
    /// The actor has a `GlowMesh` (the interact symbol that fades).
    pub has_glow: bool,
}

/// An `ASAMUGlowFlower` (its glow; the grapple rules live in the player crate).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct GlowFlowerDef {
    /// Actor id.
    pub id: u32,
    /// Object name.
    pub name: String,
    /// Location, UU.
    pub location: Vec3,
    /// `glowDuration`, s.
    pub glow_duration: f32,
    /// `FadeTime`, s.
    pub fade_time: f32,
    /// `EditorFlowerArray` light actor ids (presentation).
    pub lights: Vec<u32>,
}

/// An `ASAMUSoundMakingFoliage` (plays its `TouchSound` on every touch).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FoliageDef {
    /// Actor id.
    pub id: u32,
    /// Touch trigger.
    pub trigger: NpcCylinder,
    /// `TouchSound` (sound cue path).
    pub sound: Option<String>,
}

/// How a placed skinned actor is animated.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SkinnedDrive {
    /// Loops the component's own animation node (villagers at rest).
    Ambient,
    /// Driven by a Matinee (`SeqAct_Interp` references it): its animation
    /// tracks pose it through [`NpcRuntime::set_anim_position`].
    Matinee,
}

/// A placed actor with skeletal meshes that is not an NPC pawn
/// (`SkeletalMeshActor*`, `Villager`): presentation only.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SkinnedActorDef {
    /// Actor id.
    pub id: u32,
    /// Object name.
    pub name: String,
    /// Class path.
    pub class: String,
    /// `bHidden`.
    pub hidden: bool,
    /// Ambient or Matinee-driven.
    pub drive: SkinnedDrive,
    /// Its skeletal mesh components.
    pub components: Vec<SkinnedComponent>,
}

/// A `SkelControlLookAt` of a skinned actor's anim tree (the importer's
/// `<map>.actors.json`, `asamu-import matinee`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LookAtControlDef {
    /// `ControlName`.
    pub control: String,
    /// Bone of the control's list.
    #[serde(default)]
    pub bone: Option<String>,
    /// `LookAtAxis` (`AXIS_X`, ...).
    #[serde(default)]
    pub look_at_axis: String,
    /// `UpAxis`.
    #[serde(default)]
    pub up_axis: String,
    /// `bInvertLookAtAxis`.
    #[serde(default)]
    pub invert_look_at_axis: bool,
    /// `bInvertUpAxis`.
    #[serde(default)]
    pub invert_up_axis: bool,
    /// `bEnableLimit`.
    #[serde(default)]
    pub enable_limit: bool,
    /// `bLimitBasedOnRefPose`.
    #[serde(default)]
    pub limit_based_on_ref_pose: bool,
    /// `MaxAngle`, degrees.
    #[serde(default)]
    pub max_angle: f32,
    /// `OuterMaxAngle`, degrees.
    #[serde(default)]
    pub outer_max_angle: f32,
    /// `DeadZoneAngle`, degrees.
    #[serde(default)]
    pub dead_zone_angle: f32,
    /// `bAllowRotationX/Y/Z`.
    #[serde(default = "all_axes")]
    pub allow_rotation: [bool; 3],
    /// `AllowRotationSpace`.
    #[serde(default)]
    pub allow_rotation_space: String,
    /// `TargetLocationInterpSpeed`.
    #[serde(default)]
    pub target_interp_speed: f32,
    /// `ControlStrength` at level start.
    #[serde(default = "one")]
    pub control_strength: f32,
    /// `BlendInTime`, s.
    #[serde(default)]
    pub blend_in_time: f32,
    /// `BlendOutTime`, s.
    #[serde(default)]
    pub blend_out_time: f32,
}

fn all_axes() -> [bool; 3] {
    [true; 3]
}

/// What drives an actor's look-at controls.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LookAtDriver {
    /// `SkeletalMeshActorMATWithFollowCollision.Tick`: the head controls aim
    /// at the player pawn's location plus the head offset, the eye controls
    /// plus the eye offset (CONFIRMED (src); NPCS.md §6).
    Player,
    /// `ASAMUNPC_WormPawn.SetLookAtTarget`: all four controls aim at the
    /// worm controller's aim (CONFIRMED (src)).
    WormAim,
}

/// The look-at setup of a skinned actor or the worm.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LookAtDef {
    /// Actor id.
    pub id: u32,
    /// Driver.
    pub driver: LookAtDriver,
    /// Head control names (`headLookAtControlNames`; the worm's four).
    pub head: Vec<String>,
    /// Eye control names (`eyesLookAtControlNames`).
    pub eyes: Vec<String>,
    /// The tree's look-at controls (all of them, by name).
    pub controls: Vec<LookAtControlDef>,
}

/// An NPC pawn's collision cylinder: it blocks the player (the pawn's
/// collision type becomes block-all-but-weapons at begin play, so
/// zero-extent traces such as the grapple pass through; CONFIRMED (src,
/// `ASAMUNPC_Pawn`)).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct PawnCollisionDef {
    /// Actor id.
    pub id: u32,
    /// `CollisionRadius`, UU (the placed cylinder component's, else the
    /// template's).
    pub radius: f32,
    /// `CollisionHeight` (half height), UU.
    pub half_height: f32,
}

/// Every NPC-related definition of a loaded map (all levels).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct NpcScene {
    /// Worms.
    pub worms: Vec<WormDef>,
    /// Worm scream/shadow volumes.
    pub worm_volumes: Vec<WormVolumeDef>,
    /// Maddie pawns.
    pub maddies: Vec<MaddieDef>,
    /// Villager pawns.
    pub villagers: Vec<VillagerDef>,
    /// Navigation points.
    pub nav_points: Vec<NavPoint>,
    /// Collectibles.
    pub collectibles: Vec<CollectibleDef>,
    /// Story interactables.
    pub story_items: Vec<StoryItemDef>,
    /// Glow flowers.
    pub flowers: Vec<GlowFlowerDef>,
    /// Sound-making foliage.
    pub foliage: Vec<FoliageDef>,
    /// Other skinned actors (presentation).
    pub skinned: Vec<SkinnedActorDef>,
    /// Object name (lower case) → actor id, per level, for Kismet references.
    pub names: BTreeMap<String, u32>,
    /// Sequence timing and notifies by lower-case mesh path and lower-case
    /// sequence name (from the skeletal manifest and the importer's
    /// `anim_notifies.json`; empty without them).
    #[serde(default)]
    pub anims: BTreeMap<String, BTreeMap<String, SequenceInfo>>,
    /// Look-at setups (skinned look-at actors, the worm).
    #[serde(default)]
    pub look_at: Vec<LookAtDef>,
    /// NPC pawn collision cylinders.
    #[serde(default)]
    pub pawn_collision: Vec<PawnCollisionDef>,
    /// Non-fatal problems.
    pub warnings: Vec<String>,
}

impl NpcScene {
    /// The timing and notifies of `sequence` on `mesh` (case-insensitive;
    /// a mesh path with the map package prefix also matches the bare path).
    #[must_use]
    pub fn sequence_info(&self, mesh: &str, sequence: &str) -> Option<&SequenceInfo> {
        let key = mesh.to_ascii_lowercase();
        let seqs = self.anims.get(&key).or_else(|| {
            key.split_once('.')
                .and_then(|(_, rest)| self.anims.get(rest))
        })?;
        seqs.get(&sequence.to_ascii_lowercase())
    }

    /// Index of skinned actor `id` in [`Self::skinned`].
    #[must_use]
    pub fn skinned_index(&self, id: u32) -> Option<usize> {
        self.skinned.iter().position(|s| s.id == id)
    }

    /// The look-at setup of actor `id`.
    #[must_use]
    pub fn look_at_of(&self, id: u32) -> Option<&LookAtDef> {
        self.look_at.iter().find(|l| l.id == id)
    }

    /// Actor id of an object path or name (Kismet variable values), searched
    /// case-insensitively by object name.
    #[must_use]
    pub fn actor_by_name(&self, path_or_name: &str) -> Option<u32> {
        self.names
            .get(&object_name(path_or_name).to_ascii_lowercase())
            .copied()
    }

    /// Checks definitions for non-finite values or bad sizes; returns the
    /// problems (empty when valid).
    #[must_use]
    pub fn validate(&self) -> Vec<String> {
        let mut out = Vec::new();
        let mut ids = BTreeSet::new();
        let mut claim = |id: u32, what: &str, out: &mut Vec<String>| {
            if !ids.insert(id) {
                out.push(format!("{what} {id}: duplicate actor id"));
            }
        };
        for w in &self.worms {
            claim(w.id, "worm", &mut out);
            if !w.location.is_finite() {
                out.push(format!("worm {}: non-finite location", w.id));
            }
        }
        for c in &self.collectibles {
            claim(c.id, "collectible", &mut out);
            if !(c.trigger.center.is_finite() && c.trigger.radius >= 0.0) {
                out.push(format!("collectible {}: bad trigger", c.id));
            }
        }
        for s in &self.story_items {
            claim(s.id, "story item", &mut out);
        }
        for f in &self.flowers {
            claim(f.id, "glow flower", &mut out);
            if !(f.glow_duration.is_finite() && f.fade_time.is_finite()) {
                out.push(format!("glow flower {}: non-finite times", f.id));
            }
        }
        for f in &self.foliage {
            claim(f.id, "foliage", &mut out);
        }
        out
    }
}

// ---------------------------------------------------------------------------
// Scene loading.
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Deserialize)]
struct NpcSceneFile {
    format: String,
    version: u32,
    package: String,
    #[serde(default)]
    streaming_levels: Vec<StreamingLevelJson>,
    #[serde(default)]
    actors: Vec<NpcActorJson>,
}

#[derive(Clone, Debug, Deserialize)]
struct NpcActorJson {
    slot: usize,
    name: String,
    class: String,
    #[serde(default)]
    kind: String,
    #[serde(default)]
    location: [f32; 3],
    #[serde(default)]
    rotation: [i32; 3],
    #[serde(default)]
    hidden: bool,
    #[serde(default)]
    components: Vec<NpcComponentJson>,
    #[serde(default)]
    params: BTreeMap<String, Value>,
    #[serde(default)]
    volume: Option<VolumeJson>,
    #[serde(default)]
    matinee: Vec<Value>,
}

const IDENTITY: Mat4 = [
    [1.0, 0.0, 0.0, 0.0],
    [0.0, 1.0, 0.0, 0.0],
    [0.0, 0.0, 1.0, 0.0],
    [0.0, 0.0, 0.0, 1.0],
];

fn identity() -> Mat4 {
    IDENTITY
}

#[derive(Clone, Debug, Deserialize)]
struct NpcComponentJson {
    #[serde(default)]
    name: String,
    #[serde(default)]
    kind: String,
    #[serde(default = "identity")]
    local_to_world: Mat4,
    #[serde(default)]
    hidden_game: bool,
    #[serde(default)]
    skeletal_mesh: Option<String>,
    #[serde(default)]
    cylinder: Option<[f32; 2]>,
    #[serde(default)]
    animation: Option<AnimHint>,
}

/// Scene format and version accepted (as `scene::load_map`).
const SCENE_FORMAT: &str = "asamu-scene";
const SCENE_VERSION: u32 = 1;

fn read_npc_scene(
    source: &dyn DataSource,
    name: &str,
    opts: &LoadOptions,
) -> Result<NpcSceneFile, SceneError> {
    let wanted = format!("{name}.scene.json");
    let files = source.list("levels")?;
    let file = files
        .iter()
        .find(|f| **f == wanted)
        .or_else(|| files.iter().find(|f| f.eq_ignore_ascii_case(&wanted)))
        .ok_or_else(|| SceneError::MissingMap(name.to_owned()))?;
    let path = format!("levels/{file}");
    let data = source.read(&path, opts.max_file_bytes)?;
    let scene: NpcSceneFile = serde_json::from_slice(&data).map_err(|e| SceneError::Json {
        path: path.clone(),
        message: e.to_string(),
    })?;
    if scene.format != SCENE_FORMAT || scene.version != SCENE_VERSION {
        return Err(SceneError::Format {
            path,
            message: format!(
                "format {} version {} (expected {SCENE_FORMAT} {SCENE_VERSION})",
                scene.format, scene.version
            ),
        });
    }
    if scene.actors.len() > opts.max_actors {
        return Err(SceneError::Format {
            path,
            message: format!("{} actors exceeds the limit", scene.actors.len()),
        });
    }
    Ok(scene)
}

/// Loads the NPC definitions of the levels of an already loaded map
/// (`LoadedMap::levels`, so actor ids match the collision and gameplay
/// actors exactly). A level that cannot be read is a warning.
#[must_use]
pub fn load_npc_scene(
    source: &dyn DataSource,
    levels: &[SubLevel],
    opts: &LoadOptions,
) -> NpcScene {
    let mut out = NpcScene::default();
    let mut names = Vec::new();
    for (i, level) in levels.iter().enumerate().take(64) {
        let Ok(index) = u8::try_from(i) else {
            break;
        };
        match read_npc_scene(source, &level.name, opts) {
            Ok(scene) => {
                collect_npcs(&mut out, &scene, index, level.offset);
                names.push((index, level.name.clone()));
            }
            Err(e) => out.warnings.push(format!("level {}: {e}", level.name)),
        }
    }
    attach_extras(&mut out, source, &names, opts);
    out
}

/// Loads the NPC definitions of `map` and its streamed sub-levels with the
/// same level indexing as `scene::load_map` (for use without a loaded map).
///
/// # Errors
/// The map's own scene is missing or malformed.
pub fn load_npc_scene_for_map(
    source: &dyn DataSource,
    map: &str,
    opts: &LoadOptions,
) -> Result<NpcScene, SceneError> {
    let main = read_npc_scene(source, map, opts)?;
    let mut out = NpcScene::default();
    collect_npcs(&mut out, &main, 0, Vec3::ZERO);
    let mut names = vec![(0u8, map.to_owned())];
    let mut index: u8 = 1;
    for s in &main.streaming_levels {
        let Some(pkg) = s.package_name.as_deref() else {
            continue;
        };
        if index >= 64 {
            out.warnings
                .push("more than 63 streaming levels; the rest are ignored".to_owned());
            break;
        }
        match read_npc_scene(source, pkg, opts) {
            Ok(scene) => {
                let offset = Vec3::from_array(s.offset);
                let offset = if offset.is_finite() {
                    offset
                } else {
                    Vec3::ZERO
                };
                collect_npcs(&mut out, &scene, index, offset);
                names.push((index, pkg.to_owned()));
                index = index.saturating_add(1);
            }
            Err(e) => out.warnings.push(format!("streaming level {pkg}: {e}")),
        }
    }
    attach_extras(&mut out, source, &names, opts);
    Ok(out)
}

// ---------------------------------------------------------------------------
// Skinned actors' animation data (`asamu-import matinee` extras, skeletal
// manifest): ambient sequence nodes, look-at controls, notifies.
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Default, Deserialize)]
struct ActorsFileJson {
    #[serde(default)]
    format: String,
    #[serde(default)]
    anim_nodes: Vec<AnimNodeJson>,
    #[serde(default)]
    look_at_controls: Vec<LookAtControlJson>,
}

#[derive(Clone, Debug, Deserialize)]
struct AnimNodeJson {
    #[serde(default)]
    actor: String,
    #[serde(default)]
    component: String,
    #[serde(default)]
    sequence: Option<String>,
    #[serde(default)]
    looping: bool,
    #[serde(default)]
    playing: bool,
    #[serde(default)]
    start_time: f32,
    #[serde(default = "one")]
    rate: f32,
}

#[derive(Clone, Debug, Deserialize)]
struct LookAtControlJson {
    #[serde(default)]
    actor: String,
    #[serde(flatten)]
    control: LookAtControlDef,
}

#[derive(Clone, Debug, Default, Deserialize)]
struct AnimNotifiesJson {
    #[serde(default)]
    notifies: Vec<AnimNotifyJson>,
}

#[derive(Clone, Debug, Deserialize)]
struct AnimNotifyJson {
    #[serde(default)]
    path: String,
    #[serde(default)]
    class: String,
    #[serde(default)]
    notify_name: Option<String>,
    #[serde(default)]
    sound_cue: Option<String>,
    #[serde(default)]
    follow_actor: bool,
    #[serde(default)]
    ignore_if_actor_hidden: bool,
    #[serde(default)]
    bone: Option<String>,
    #[serde(default = "one")]
    volume: f32,
    #[serde(default = "one")]
    pitch: f32,
    #[serde(default = "one")]
    percent_to_play: f32,
}

/// `format` of the importer's `<map>.actors.json`.
pub const ACTORS_FORMAT: &str = "asamu-matinee-actors";

fn read_optional<T: serde::de::DeserializeOwned + Default>(
    source: &dyn DataSource,
    path: &str,
    opts: &LoadOptions,
    warnings: &mut Vec<String>,
) -> Option<T> {
    let data = source.read(path, opts.max_file_bytes).ok()?;
    match serde_json::from_slice(&data) {
        Ok(v) => Some(v),
        Err(e) => {
            warnings.push(format!("{path}: {e}"));
            None
        }
    }
}

/// What a notify object does (`anim_notifies.json` entry; footsteps and
/// other classes are not acted on).
fn notify_kind(n: &AnimNotifyJson) -> NotifyKind {
    let class = n.class.rsplit('.').next().unwrap_or(&n.class);
    if class.eq_ignore_ascii_case("AnimNotify_Kismet") {
        // A notify without a name (`NAME_None`) does nothing.
        match n
            .notify_name
            .as_deref()
            .filter(|name| !name.is_empty() && !name.eq_ignore_ascii_case("None"))
        {
            Some(name) => NotifyKind::Kismet {
                name: name.to_owned(),
            },
            None => NotifyKind::Other {
                what: n.class.clone(),
            },
        }
    } else if class.eq_ignore_ascii_case("AnimNotify_Sound")
        && let Some(cue) = &n.sound_cue
    {
        let finite = |v: f32| if v.is_finite() { v } else { 1.0 };
        NotifyKind::Sound {
            cue: cue.clone(),
            follow_actor: n.follow_actor,
            bone: n.bone.clone(),
            volume: finite(n.volume),
            pitch: finite(n.pitch),
            percent_to_play: finite(n.percent_to_play),
            ignore_if_hidden: n.ignore_if_actor_hidden,
        }
    } else {
        NotifyKind::Other {
            what: n.class.clone(),
        }
    }
}

/// Reads the optional extras of the levels `(level index, package)`: the
/// importer's `matinee/<level>.actors.json` (ambient sequence nodes fill
/// components without an `animation`, look-at controls join the actors'
/// [`LookAtDef`]s), and the skeletal manifest with `matinee/anim_notifies.json`
/// for the sequences the skinned actors and worms use. Missing files are
/// normal (older conversions); a malformed one is a warning.
fn attach_extras(
    out: &mut NpcScene,
    source: &dyn DataSource,
    levels: &[(u8, String)],
    opts: &LoadOptions,
) {
    let mut warnings = Vec::new();
    for (index, name) in levels {
        let path = format!("matinee/{name}.actors.json");
        let Some(file) = read_optional::<ActorsFileJson>(source, &path, opts, &mut warnings) else {
            continue;
        };
        if file.format != ACTORS_FORMAT {
            warnings.push(format!("{path}: unsupported format {}", file.format));
            continue;
        }
        let in_level = |id: u32| (id >> 16) == u32::from(*index);
        for n in &file.anim_nodes {
            let Some(seq) = n.sequence.clone().filter(|s| !s.is_empty()) else {
                continue;
            };
            let actor = object_name(&n.actor);
            let Some(def) = out
                .skinned
                .iter_mut()
                .find(|d| in_level(d.id) && d.name.eq_ignore_ascii_case(actor))
            else {
                continue;
            };
            if let Some(c) = def
                .components
                .iter_mut()
                .find(|c| c.name.eq_ignore_ascii_case(&n.component))
                && c.animation.is_none()
                && n.start_time.is_finite()
                && n.rate.is_finite()
            {
                c.animation = Some(AnimHint {
                    sequence: seq,
                    looping: n.looping,
                    playing: n.playing,
                    start_time: n.start_time,
                    rate: n.rate,
                });
            }
        }
        for c in &file.look_at_controls {
            let actor = object_name(&c.actor);
            let Some(id) = out
                .skinned
                .iter()
                .find(|d| in_level(d.id) && d.name.eq_ignore_ascii_case(actor))
                .map(|d| d.id)
                .or_else(|| {
                    out.worms
                        .iter()
                        .find(|w| in_level(w.id) && w.name.eq_ignore_ascii_case(actor))
                        .map(|w| w.id)
                })
            else {
                continue;
            };
            if let Some(l) = out.look_at.iter_mut().find(|l| l.id == id)
                && !l.controls.iter().any(|x| x.control == c.control.control)
            {
                l.controls.push(c.control.clone());
            }
        }
    }
    // Sequence data for the meshes the skinned actors and worms use.
    let meshes: BTreeSet<String> = out
        .skinned
        .iter()
        .flat_map(|d| d.components.iter())
        .chain(out.worms.iter().flat_map(|w| w.meshes.iter()))
        .map(|c| c.mesh.to_ascii_lowercase())
        .collect();
    if !meshes.is_empty()
        && let Ok(index) = SkeletalIndex::load(source, opts)
    {
        let names: BTreeMap<String, NotifyKind> = read_optional::<AnimNotifiesJson>(
            source,
            "matinee/anim_notifies.json",
            opts,
            &mut warnings,
        )
        .unwrap_or_default()
        .notifies
        .iter()
        .map(|n| (n.path.to_ascii_lowercase(), notify_kind(n)))
        .collect();
        for mesh in meshes {
            let Some(info) = index.get(&mesh) else {
                continue;
            };
            let seqs = out.anims.entry(mesh).or_default();
            for a in &info.animations {
                let notifies = a
                    .notifies
                    .iter()
                    .filter(|n| n.time.is_finite())
                    .map(|n| NotifyDef {
                        time: n.time,
                        duration: if n.duration.is_finite() {
                            n.duration
                        } else {
                            0.0
                        },
                        kind: n
                            .path
                            .as_deref()
                            .and_then(|p| names.get(&p.to_ascii_lowercase()).cloned())
                            .unwrap_or_else(|| NotifyKind::Other {
                                what: n.path.clone().unwrap_or_default(),
                            }),
                    })
                    .collect();
                seqs.entry(a.sequence.to_ascii_lowercase())
                    .or_insert(SequenceInfo {
                        length: a.length,
                        rate_scale: a.rate_scale,
                        notifies,
                    });
            }
        }
    }
    out.warnings.extend(warnings);
}

fn has_class(actor: &NpcActorJson, class: &str) -> bool {
    actor.class.eq_ignore_ascii_case(class)
}

fn object_list(params: &BTreeMap<String, Value>, name: &str) -> Vec<String> {
    match param(params, name) {
        Some(Value::Array(items)) => items
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect(),
        _ => Vec::new(),
    }
}

/// The trigger cylinder of an actor (the component its `CylinderComponent`
/// parameter names, else the first cylinder), placed by the component's world
/// matrix; `default` (radius, half-height, z offset) when there is none.
fn trigger_of(actor: &NpcActorJson, offset: Vec3, default: (f32, f32, f32)) -> NpcCylinder {
    let wanted = param_str(&actor.params, "CylinderComponent").map(object_name);
    let comp = actor
        .components
        .iter()
        .filter(|c| c.kind == "cylinder")
        .find(|c| wanted.is_none_or(|w| c.name.eq_ignore_ascii_case(w)))
        .or_else(|| actor.components.iter().find(|c| c.kind == "cylinder"));
    let location = Vec3::from_array(actor.location) + offset;
    if let Some(c) = comp
        && let Some([r, h]) = c.cylinder
        && r.is_finite()
        && h.is_finite()
        && r >= 0.0
        && h >= 0.0
    {
        let m = c.local_to_world;
        let center = Vec3::new(m[3][0], m[3][1], m[3][2]) + offset;
        if center.is_finite() {
            return NpcCylinder {
                center,
                radius: r,
                half_height: h,
            };
        }
    }
    NpcCylinder {
        center: location + Vec3::Z * default.2,
        radius: default.0,
        half_height: default.1,
    }
}

fn skinned_components(actor: &NpcActorJson, offset: Vec3) -> Vec<SkinnedComponent> {
    actor
        .components
        .iter()
        .filter(|c| c.kind == "skeletal_mesh")
        .filter_map(|c| {
            let mesh = c.skeletal_mesh.clone()?;
            let mut m = c.local_to_world;
            m[3][0] += offset.x;
            m[3][1] += offset.y;
            m[3][2] += offset.z;
            m.iter()
                .flatten()
                .all(|v| v.is_finite())
                .then(|| SkinnedComponent {
                    name: c.name.clone(),
                    mesh,
                    local_to_world: m,
                    hidden: c.hidden_game,
                    animation: c.animation.clone().filter(|a| {
                        a.start_time.is_finite() && a.rate.is_finite() && !a.sequence.is_empty()
                    }),
                })
        })
        .collect()
}

/// The pawn's collision cylinder: its placed `CylinderComponent` (radius,
/// half height), else `default` (the class template's).
fn pawn_collision(actor: &NpcActorJson, id: u32, default: (f32, f32)) -> PawnCollisionDef {
    let (radius, half_height) = actor
        .components
        .iter()
        .filter(|c| c.kind == "cylinder")
        .find_map(|c| c.cylinder)
        .filter(|[r, h]| r.is_finite() && h.is_finite() && *r > 0.0 && *h > 0.0)
        .map_or(default, |[r, h]| (r, h));
    PawnCollisionDef {
        id,
        radius,
        half_height,
    }
}

/// The worm pawn's look-at controls (`ASAMUNPC_WormPawn.PostInitAnimTree`
/// finds these four by name; CONFIRMED (src)).
pub const WORM_LOOK_AT_CONTROLS: [&str; 4] =
    ["Spine4", "Spine_45_LookAT", "EyeLookat", "HeadLookat"];

fn color_param(params: &BTreeMap<String, Value>, name: &str, default: [u8; 4]) -> [u8; 4] {
    let Some(m) = param(params, name).and_then(Value::as_object) else {
        return default;
    };
    let c = |k: &str, d: u8| {
        m.iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(k))
            .and_then(|(_, v)| v.as_i64())
            .and_then(|v| u8::try_from(v).ok())
            .unwrap_or(d)
    };
    [
        c("R", default[0]),
        c("G", default[1]),
        c("B", default[2]),
        c("A", default[3]),
    ]
}

fn finite_or(v: f32, default: f32) -> f32 {
    if v.is_finite() { v } else { default }
}

/// Gathers the NPC-related actors of one level.
fn collect_npcs(out: &mut NpcScene, scene: &NpcSceneFile, level: u8, offset: Vec3) {
    // Name → (id, location, cylinder radius) for references within the level.
    let mut by_name: BTreeMap<String, (u32, Vec3, f32)> = BTreeMap::new();
    for a in &scene.actors {
        if let Some(id) = actor_id(level, a.slot) {
            let radius = a
                .components
                .iter()
                .find_map(|c| c.cylinder.map(|x| x[0]))
                .filter(|r| r.is_finite() && *r >= 0.0)
                .unwrap_or(0.0);
            let location = Vec3::from_array(a.location) + offset;
            by_name.insert(a.name.to_ascii_lowercase(), (id, location, radius));
            out.names.entry(a.name.to_ascii_lowercase()).or_insert(id);
        }
    }
    let lookup = |path: &str| {
        by_name
            .get(&object_name(path).to_ascii_lowercase())
            .copied()
    };
    for a in &scene.actors {
        let Some(id) = actor_id(level, a.slot) else {
            out.warnings.push(format!(
                "{}: slot {} does not fit an actor id",
                a.name, a.slot
            ));
            continue;
        };
        let location = Vec3::from_array(a.location) + offset;
        if !location.is_finite() {
            out.warnings
                .push(format!("{}: non-finite location", a.name));
            continue;
        }
        let p = &a.params;
        if has_class(a, "asamu.ASAMUNPC_WormPawn") {
            let defaults = WormLight::default();
            out.worms.push(WormDef {
                id,
                name: a.name.clone(),
                location,
                rotation: a.rotation,
                look_targets: object_list(p, "RandomLookAtTargets")
                    .iter()
                    .filter_map(|t| lookup(t).map(|x| x.1))
                    .collect(),
                light: WormLight {
                    radius: finite_or(param_f32(p, "Radius", defaults.radius), defaults.radius),
                    brightness: finite_or(
                        param_f32(p, "Brightness", defaults.brightness),
                        defaults.brightness,
                    ),
                    color: color_param(p, "LightColor", defaults.color),
                    outer_cone: finite_or(
                        param_f32(p, "OuterConeAngle", defaults.outer_cone),
                        defaults.outer_cone,
                    ),
                    inner_cone: finite_or(
                        param_f32(p, "InnerConeAngle", defaults.inner_cone),
                        defaults.inner_cone,
                    ),
                },
                params: WormParams::ORIGINAL,
                meshes: skinned_components(a, offset),
            });
            out.pawn_collision.push(pawn_collision(
                a,
                id,
                (NPC_COLLISION_RADIUS, NPC_COLLISION_HALF_HEIGHT),
            ));
            out.look_at.push(LookAtDef {
                id,
                driver: LookAtDriver::WormAim,
                head: WORM_LOOK_AT_CONTROLS
                    .iter()
                    .map(|c| (*c).to_owned())
                    .collect(),
                eyes: Vec::new(),
                controls: Vec::new(),
            });
        } else if has_class(a, "asamu.ASAMUWormScreamVolume")
            || has_class(a, "asamu.ASAMUWormShadowVolume")
        {
            let role = if has_class(a, "asamu.ASAMUWormScreamVolume") {
                WormVolumeRole::Scream
            } else {
                WormVolumeRole::Shadow
            };
            let hulls: Vec<NpcHull> = a
                .volume
                .as_ref()
                .map(|v| {
                    v.hulls
                        .iter()
                        .filter_map(|h| NpcHull::from_json(h, offset))
                        .collect()
                })
                .unwrap_or_default();
            if hulls.is_empty() {
                out.warnings
                    .push(format!("{}: worm volume without usable hulls", a.name));
            }
            out.worm_volumes.push(WormVolumeDef {
                id,
                name: a.name.clone(),
                role,
                hulls,
            });
        } else if has_class(a, "asamu.ASAMUNPC_MaddiePawn") {
            out.maddies.push(MaddieDef {
                id,
                name: a.name.clone(),
                location,
                rotation: a.rotation,
                meshes: skinned_components(a, offset),
            });
            out.pawn_collision.push(pawn_collision(
                a,
                id,
                (NPC_COLLISION_RADIUS, NPC_COLLISION_HALF_HEIGHT),
            ));
        } else if has_class(a, "asamu.ASAMUNPC_VillagerPawn") {
            let scripted_path = object_list(p, "scriptedPath")
                .iter()
                .filter_map(|t| {
                    lookup(t).map(|(nid, loc, radius)| NavPoint {
                        id: nid,
                        location: loc,
                        radius,
                    })
                })
                .collect();
            out.villagers.push(VillagerDef {
                id,
                name: a.name.clone(),
                location,
                rotation: a.rotation,
                use_scripted_path: param_bool(p, "bUseScriptedPath", false),
                scripted_path,
                meshes: skinned_components(a, offset),
            });
            out.pawn_collision.push(pawn_collision(
                a,
                id,
                (VILLAGER_COLLISION_RADIUS, VILLAGER_COLLISION_HALF_HEIGHT),
            ));
        } else if a.kind == "collectible" {
            out.collectibles.push(CollectibleDef {
                id,
                level: scene.package.clone(),
                name: a.name.clone(),
                location,
                trigger: trigger_of(
                    a,
                    offset,
                    (
                        COLLECTIBLE_TRIGGER_RADIUS,
                        COLLECTIBLE_TRIGGER_HALF_HEIGHT,
                        COLLECTIBLE_TRIGGER_Z,
                    ),
                ),
            });
        } else if has_class(a, "asamu.ASAMUInteractable_Actor") {
            let max = i32::try_from(param_i64(
                p,
                "MaxInteractTimes",
                i64::from(INTERACTABLE_MAX_INTERACT_TIMES),
            ))
            .unwrap_or(INTERACTABLE_MAX_INTERACT_TIMES);
            out.story_items.push(StoryItemDef {
                id,
                level: scene.package.clone(),
                name: a.name.clone(),
                location,
                max_interact_times: max,
                optional: param_bool(p, "bIsOptional", false),
                parent: param_bool(p, "bParentInteractable", false),
                linked_parent: param_str(p, "linkedParentActor")
                    .and_then(lookup)
                    .map(|x| x.0),
                linked_children: object_list(p, "linkedInteractables")
                    .iter()
                    .filter_map(|t| lookup(t).map(|x| x.0))
                    .collect(),
                // Class default `GlowMesh` is a subobject; a null override
                // removes it.
                has_glow: !matches!(param(p, "GlowMesh"), Some(Value::Null)),
            });
        } else if has_class(a, "asamu.ASAMUGlowFlower") {
            let duration = param_f32(p, "glowDuration", GLOW_FLOWER_DURATION);
            let fade = param_f32(p, "FadeTime", GLOW_FLOWER_FADE_TIME);
            out.flowers.push(GlowFlowerDef {
                id,
                name: a.name.clone(),
                location,
                glow_duration: duration,
                fade_time: fade,
                lights: object_list(p, "EditorFlowerArray")
                    .iter()
                    .filter_map(|t| lookup(t).map(|x| x.0))
                    .collect(),
            });
        } else if has_class(a, "asamu.ASAMUSoundMakingFoliage") {
            out.foliage.push(FoliageDef {
                id,
                trigger: trigger_of(
                    a,
                    offset,
                    (
                        FOLIAGE_TRIGGER_RADIUS,
                        FOLIAGE_TRIGGER_HALF_HEIGHT,
                        FOLIAGE_TRIGGER_Z,
                    ),
                ),
                sound: param_str(p, "TouchSound").map(str::to_owned),
            });
        } else if a.kind == "player_start" || a.class.to_ascii_lowercase().contains("pathnode") {
            out.nav_points.push(NavPoint {
                id,
                location,
                radius: by_name
                    .get(&a.name.to_ascii_lowercase())
                    .map_or(0.0, |x| x.2),
            });
        } else {
            let components = skinned_components(a, offset);
            let head = object_list(p, "headLookAtControlNames");
            let eyes = object_list(p, "eyesLookAtControlNames");
            if !components.is_empty() && (!head.is_empty() || !eyes.is_empty()) {
                out.look_at.push(LookAtDef {
                    id,
                    driver: LookAtDriver::Player,
                    head,
                    eyes,
                    controls: Vec::new(),
                });
            }
            if !components.is_empty() {
                out.skinned.push(SkinnedActorDef {
                    id,
                    name: a.name.clone(),
                    class: a.class.clone(),
                    hidden: a.hidden,
                    drive: if a.matinee.is_empty() {
                        SkinnedDrive::Ambient
                    } else {
                        SkinnedDrive::Matinee
                    },
                    components,
                });
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Skeletal manifest index (presentation: which glTF and animation to play).
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Default, Deserialize)]
struct SkelManifestJson {
    #[serde(default)]
    meshes: BTreeMap<String, SkelMeshJson>,
}

#[derive(Clone, Debug, Deserialize)]
struct SkelMeshJson {
    #[serde(default)]
    package: String,
    #[serde(default = "one")]
    scale: f32,
    #[serde(default)]
    lods: Vec<SkelLodJson>,
    #[serde(default)]
    anim_sets: Vec<SkelAnimSetJson>,
    #[serde(default)]
    sockets: Vec<SkelSocketJson>,
}

#[derive(Clone, Debug, Deserialize)]
struct SkelLodJson {
    #[serde(default)]
    lod: usize,
    gltf: String,
}

#[derive(Clone, Debug, Deserialize)]
struct SkelAnimSetJson {
    #[serde(default)]
    path: String,
    #[serde(default)]
    sequences: Vec<SkelSequenceJson>,
}

#[derive(Clone, Debug, Deserialize)]
struct SkelSequenceJson {
    animation: usize,
    #[serde(default)]
    name: String,
    #[serde(default)]
    sequence_name: String,
    #[serde(default)]
    length: f32,
    #[serde(default = "one")]
    rate_scale: f32,
    /// `(time, notify object path, comment, duration)`.
    #[serde(default)]
    notifies: Vec<(f32, Option<String>, String, f32)>,
}

/// One notify of a converted sequence (skeletal manifest).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SkelNotify {
    /// `Time`, s.
    pub time: f32,
    /// The notify object's path (`None` when unset).
    pub path: Option<String>,
    /// The editor `Comment`.
    pub comment: String,
    /// `Duration`, s.
    pub duration: f32,
}

#[derive(Clone, Debug, Deserialize)]
struct SkelSocketJson {
    #[serde(default)]
    name: String,
    #[serde(default)]
    bone: String,
}

/// One animation of a converted skeletal mesh.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SkeletalAnimInfo {
    /// glTF animation index.
    pub index: usize,
    /// glTF animation name (`<AnimSet>/<SequenceName>`).
    pub name: String,
    /// AnimSet object path.
    pub anim_set: String,
    /// `SequenceName`.
    pub sequence: String,
    /// `SequenceLength`, s.
    pub length: f32,
    /// `RateScale`.
    pub rate_scale: f32,
    /// `Notifies`.
    #[serde(default)]
    pub notifies: Vec<SkelNotify>,
}

/// One converted skeletal mesh (`skeletal/manifest.json` of `asamu-import
/// skeletal`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SkeletalMeshInfo {
    /// Object path (manifest key).
    pub path: String,
    /// Package it was converted from.
    pub package: String,
    /// LOD 0 `.gltf`, relative to the converted directory (`skeletal/...`).
    pub gltf: String,
    /// glTF units per UU.
    pub scale: f32,
    /// Animations.
    pub animations: Vec<SkeletalAnimInfo>,
    /// Socket `(name, bone)` pairs.
    pub sockets: Vec<(String, String)>,
}

impl SkeletalMeshInfo {
    /// The animation playing `sequence` (case-insensitive `SequenceName`).
    #[must_use]
    pub fn animation(&self, sequence: &str) -> Option<&SkeletalAnimInfo> {
        self.animations
            .iter()
            .find(|a| a.sequence.eq_ignore_ascii_case(sequence))
    }

    /// A stand-in animation when the component's own node is unknown: the
    /// first whose sequence name contains `idle`, else the first (ours, not
    /// original behaviour).
    #[must_use]
    pub fn idle_animation(&self) -> Option<&SkeletalAnimInfo> {
        self.animations
            .iter()
            .find(|a| a.sequence.to_ascii_lowercase().contains("idle"))
            .or_else(|| self.animations.first())
    }
}

/// `true` for a relative, `/`-separated path that stays below its root on
/// every platform: no empty, `.` or `..` component, no leading separator, no
/// backslash, drive or stream colon (`C:`, `file:x`), no asset-label `#`,
/// no control characters, and not absurdly long.
fn safe_relative_path(p: &str) -> bool {
    !p.is_empty()
        && p.len() <= 1024
        && !p
            .chars()
            .any(|c| c == '\\' || c == ':' || c == '#' || c.is_control())
        && p.split('/').all(|c| !c.is_empty() && c != "." && c != "..")
}

/// Index of the converted skeletal meshes.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SkeletalIndex {
    /// Meshes by lower-case object path.
    pub meshes: BTreeMap<String, SkeletalMeshInfo>,
}

impl SkeletalIndex {
    /// Reads `skeletal/manifest.json`.
    ///
    /// # Errors
    /// Missing, oversize or malformed manifest.
    pub fn load(source: &dyn DataSource, opts: &LoadOptions) -> Result<Self, SceneError> {
        let path = "skeletal/manifest.json";
        let data = source.read(path, opts.max_file_bytes)?;
        let m: SkelManifestJson = serde_json::from_slice(&data).map_err(|e| SceneError::Json {
            path: path.to_owned(),
            message: e.to_string(),
        })?;
        let mut meshes = BTreeMap::new();
        for (key, mesh) in m.meshes {
            let Some(lod0) = mesh.lods.iter().find(|l| l.lod == 0).or(mesh.lods.first()) else {
                continue;
            };
            if !safe_relative_path(&lod0.gltf) {
                continue;
            }
            let animations = mesh
                .anim_sets
                .iter()
                .flat_map(|set| {
                    set.sequences.iter().map(move |s| SkeletalAnimInfo {
                        index: s.animation,
                        name: s.name.clone(),
                        anim_set: set.path.clone(),
                        sequence: s.sequence_name.clone(),
                        length: if s.length.is_finite() { s.length } else { 0.0 },
                        rate_scale: if s.rate_scale.is_finite() {
                            s.rate_scale
                        } else {
                            1.0
                        },
                        notifies: s
                            .notifies
                            .iter()
                            .take(1024)
                            .map(|(time, path, comment, duration)| SkelNotify {
                                time: *time,
                                path: path.clone(),
                                comment: comment.clone(),
                                duration: *duration,
                            })
                            .collect(),
                    })
                })
                .collect();
            meshes.insert(
                key.to_ascii_lowercase(),
                SkeletalMeshInfo {
                    path: key.clone(),
                    package: mesh.package.clone(),
                    gltf: format!("skeletal/{}", lod0.gltf),
                    scale: if mesh.scale.is_finite() && mesh.scale > 0.0 {
                        mesh.scale
                    } else {
                        1.0
                    },
                    animations,
                    sockets: mesh
                        .sockets
                        .iter()
                        .map(|s| (s.name.clone(), s.bone.clone()))
                        .collect(),
                },
            );
        }
        Ok(Self { meshes })
    }

    /// The mesh for a scene reference (case-insensitive; a reference with the
    /// map package prefix also matches the bare path and vice versa).
    #[must_use]
    pub fn get(&self, mesh_path: &str) -> Option<&SkeletalMeshInfo> {
        let key = mesh_path.to_ascii_lowercase();
        if let Some(m) = self.meshes.get(&key) {
            return Some(m);
        }
        // `Pkg.Group.Name` ↔ `Group.Name`.
        if let Some((_, rest)) = key.split_once('.')
            && let Some(m) = self.meshes.get(rest)
        {
            return Some(m);
        }
        self.meshes
            .iter()
            .find(|(k, _)| k.split_once('.').is_some_and(|(_, r)| r == key))
            .map(|(_, m)| m)
    }
}

// ---------------------------------------------------------------------------
// Events and effects.
// ---------------------------------------------------------------------------

/// `SeqEvent_WormEvents` outputs (index = output port). CONFIRMED (src, cdo
/// port labels).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WormEventKind {
    /// Output 0.
    WakingUp,
    /// Output 1.
    Awaken,
    /// Output 2.
    FallingAsleep,
    /// Output 3.
    Alerted,
    /// Output 4.
    Screaming,
    /// Output 5.
    StoppedScreaming,
    /// Output 6.
    FinishedAlerted,
}

impl WormEventKind {
    /// The Kismet output index.
    #[must_use]
    pub fn output(self) -> usize {
        match self {
            Self::WakingUp => 0,
            Self::Awaken => 1,
            Self::FallingAsleep => 2,
            Self::Alerted => 3,
            Self::Screaming => 4,
            Self::StoppedScreaming => 5,
            Self::FinishedAlerted => 6,
        }
    }
}

/// An NPC-side event for Kismet, audio, UI or saving.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum NpcEvent {
    /// Every `SeqEvent_WormEvents` in the level fires this output (when
    /// enabled; `MaxTriggerCount` 0 by default).
    Worm {
        /// Worm actor id.
        worm: u32,
        /// Output.
        kind: WormEventKind,
    },
    /// The worm controller changed state (presentation).
    WormState {
        /// Worm actor id.
        worm: u32,
        /// New state.
        state: WormStateName,
    },
    /// `StartCameraShake` / `StopCameraShake` (camera anim
    /// `Zeth_CameraStuffs.MonsterGrowl`, looping).
    CameraShake {
        /// Worm actor id.
        worm: u32,
        /// Start (true) or stop.
        start: bool,
    },
    /// `ASAMUCollectible.Collect`: progression registration, sounds, and
    /// Kismet `SeqEvent_CollectibleCollected` (output 0 of every instance).
    CollectibleCollected {
        /// Collectible actor id.
        id: u32,
    },
    /// Kismet `SeqEvent_ActorInteractedWith` (instances whose originator is
    /// this actor fire output 0; every instance counts the activation).
    ActorInteractedWith {
        /// The interacted actor (a parent forwards with itself).
        originator: u32,
    },
    /// `RegisterInteractedInteractable` (optional story item); `None` is the
    /// original's stand-alone quirk (it registers its null parent, SAVE.md Q5).
    StoryItemRegistered {
        /// The registered item.
        item: Option<u32>,
    },
    /// A sound-making foliage actor was touched (play its `TouchSound`).
    FoliageTouched {
        /// Foliage actor id.
        id: u32,
    },
    /// A glow flower was grappled (spawn its grappled particles).
    GlowFlowerGrappled {
        /// Flower actor id.
        id: u32,
    },
    /// A glow flower starts (or restarts) its glow (play its glow sound).
    GlowFlowerGlow {
        /// Flower actor id.
        id: u32,
    },
    /// An `AnimNotify_Kismet` of a skinned actor's own animation fired
    /// (every `SeqEvent_AnimNotify` of the actor with this name is checked;
    /// Matinee-driven notifies reach Kismet inside the Matinee update
    /// instead).
    AnimNotify {
        /// Actor id.
        actor: u32,
        /// `NotifyName`.
        name: String,
    },
    /// An `AnimNotify_Sound` fired (play the cue at the actor or its bone).
    AnimSound {
        /// Actor id.
        actor: u32,
        /// Sound cue path.
        cue: String,
        /// `VolumeMultiplier`.
        volume: f32,
        /// `PitchMultiplier`.
        pitch: f32,
        /// `BoneName`, if any.
        bone: Option<String>,
    },
}

/// An NPC effect on the player that the game applies.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "effect", rename_all = "snake_case")]
pub enum NpcEffect {
    /// The worm's push: release the grapple (`ReleaseGrappleButton`), set
    /// Falling physics, then add `delta_v` to the velocity.
    PushPlayer {
        /// Worm actor id.
        worm: u32,
        /// Velocity added, uu/s.
        delta_v: Vec3,
    },
    /// The scream lasted `screamTimeMax`: `PlayerDied` (the worm's own
    /// `NotifyKilled` handling has already been applied).
    KillPlayer {
        /// Worm actor id.
        worm: u32,
    },
}

/// Events and effects of one NPC update.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct NpcOutput {
    /// Events in order.
    pub events: Vec<NpcEvent>,
    /// Effects in order.
    pub effects: Vec<NpcEffect>,
}

// ---------------------------------------------------------------------------
// The worm.
// ---------------------------------------------------------------------------

/// `ASAMUNPC_Worm` states.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WormStateName {
    /// `Disabled` (auto state).
    #[default]
    Disabled,
    /// `Idle` (entry of `StartWorm`; goes straight to `WakingUp`).
    Idle,
    /// `Sleeping`.
    Sleeping,
    /// `WakingUp`.
    WakingUp,
    /// `Awake`.
    Awake,
    /// `Alerted`.
    Alerted,
    /// `Screaming`.
    Screaming,
    /// `ScriptedMove` (never entered; logs only).
    ScriptedMove,
    /// `ScriptedRouteMove` (never entered; logs only).
    ScriptedRouteMove,
}

/// Animation sequence nodes of the worm's `AnimTree`
/// (`Dark_Cave_worm.Animations.WormAnimTree`; node → sequence CONFIRMED
/// (data)).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WormAnim {
    /// `FallAsleep` → `Worm_FallAsleep`.
    FallAsleep,
    /// `SleepIdle` → `Worm_SleepIdle`.
    SleepIdle,
    /// `WakeUp` → `Worm_WakeUp1`.
    WakeUp,
    /// `DiscoveredIdle` → `Worm_DiscoveredIdle`.
    DiscoveredIdle,
    /// `Scream` → `Worm_Scream`.
    Scream,
    /// `ShortLookleft` → `Worm_ShortLookLeft`.
    ShortLookLeft,
    /// `ShortLookRight` → `Worm_ShortLookRight`.
    ShortLookRight,
    /// `LongLookLeft` → `Worm_LongLookLeft`.
    LongLookLeft,
    /// `LongLookRight` → `Worm_LongLookRight`.
    LongLookRight,
    /// `LeftToMiddle` → `Worm_LookLeftToMiddle`.
    LeftToMiddle,
    /// `RightToMiddle` → `Worm_LookRightToMiddle`.
    RightToMiddle,
}

/// Number of worm animation nodes.
pub const WORM_ANIM_COUNT: usize = 11;

impl WormAnim {
    /// All nodes, in index order.
    pub const ALL: [Self; WORM_ANIM_COUNT] = [
        Self::FallAsleep,
        Self::SleepIdle,
        Self::WakeUp,
        Self::DiscoveredIdle,
        Self::Scream,
        Self::ShortLookLeft,
        Self::ShortLookRight,
        Self::LongLookLeft,
        Self::LongLookRight,
        Self::LeftToMiddle,
        Self::RightToMiddle,
    ];

    fn index(self) -> usize {
        self as usize
    }

    /// The node's `AnimSeqName`. CONFIRMED (data, the AnimTree's nodes).
    #[must_use]
    pub fn sequence(self) -> &'static str {
        match self {
            Self::FallAsleep => "Worm_FallAsleep",
            Self::SleepIdle => "Worm_SleepIdle",
            Self::WakeUp => "Worm_WakeUp1",
            Self::DiscoveredIdle => "Worm_DiscoveredIdle",
            Self::Scream => "Worm_Scream",
            Self::ShortLookLeft => "Worm_ShortLookLeft",
            Self::ShortLookRight => "Worm_ShortLookRight",
            Self::LongLookLeft => "Worm_LongLookLeft",
            Self::LongLookRight => "Worm_LongLookRight",
            Self::LeftToMiddle => "Worm_LookLeftToMiddle",
            Self::RightToMiddle => "Worm_LookRightToMiddle",
        }
    }

    /// `SequenceLength` of the sequence, s (`RateScale` 1, not stored).
    /// CONFIRMED (data, `Dark_Cave_worm.Animations.WormAnimSet`).
    #[must_use]
    pub fn length(self) -> f32 {
        match self {
            Self::FallAsleep => 2.5,
            Self::SleepIdle => 5.833_333_5,
            Self::WakeUp => 3.958_333_3,
            Self::DiscoveredIdle => 3.125,
            Self::Scream => 0.833_333_3,
            Self::ShortLookLeft => 1.458_333_4,
            Self::ShortLookRight => 1.416_666_6,
            Self::LongLookLeft => 2.458_333_3,
            Self::LongLookRight => 2.5,
            Self::LeftToMiddle => 1.291_666_6,
            Self::RightToMiddle => 1.916_666_6,
        }
    }

    /// `bCauseActorAnimEnd` (only the six look nodes). CONFIRMED (data).
    #[must_use]
    pub fn causes_actor_anim_end(self) -> bool {
        matches!(
            self,
            Self::ShortLookLeft
                | Self::ShortLookRight
                | Self::LongLookLeft
                | Self::LongLookRight
                | Self::LeftToMiddle
                | Self::RightToMiddle
        )
    }
}

/// One `AnimNodeSequence`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct AnimNodeState {
    /// `bPlaying`.
    pub playing: bool,
    /// `bLooping`.
    pub looping: bool,
    /// `CurrentTime`, s.
    pub position: f32,
}

/// The worm's `AnimTree` state as the script drives it: the active child of
/// every blend list and every sequence node's play state. Native rules
/// (CONFIRMED (native)): `AnimNodeBlendList::SetActiveChild` changes the
/// weights and, when the list has `bPlayActiveChild` and the new active child
/// is a sequence node, *replays* that node (`ReplayAnim`: `PlayAnim` from
/// time 0 with the node's own looping flag and rate) — on every call, also
/// when the child was already active; `PlayAnim` on a blend node restarts
/// every sequence node below it; a non-looping node that reaches its end
/// stops and, with `bCauseActorAnimEnd`, calls the pawn's `OnAnimEnd`; only
/// relevant (weighted) nodes tick. In this tree (CONFIRMED (data)) every list
/// has `bPlayActiveChild` except `StateAnimation` and `WokeUpState`;
/// `LookAroundList`'s children are lists, so its switches replay nothing,
/// while the three look lists, `SleepState` and `AlertState` replay the
/// selected sequence. Simplification (TENTATIVE): only the fully active path
/// ticks (cross-blends are ignored).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct WormAnimTree {
    /// `StateAnimation`: 0 sleep, 1 woke-up, 2 alert.
    pub state_anims: u8,
    /// `SleepState`: 0 `FallAsleep`, 1 `SleepIdle`.
    pub sleep_anims: u8,
    /// `WokeUpState`: 0 waking, 1 awake (look-around).
    pub woke_up: u8,
    /// `AlertState`: 0 `DiscoveredIdle`, 1 `Scream`.
    pub alert_anims: u8,
    /// `LookAroundList`: 0 left list, 1 middle list, 2 right list.
    pub look_around: u8,
    /// `LookingLeftList`: 0 `LongLookRight`, 1 `LeftToMiddle`.
    pub look_left: u8,
    /// `LookingMiddleList`: 0 `ShortLookleft`, 1 `ShortLookRight`.
    pub look_middle: u8,
    /// `LookingRightList`: 0 `LongLookLeft`, 1 `RightToMiddle`.
    pub look_right: u8,
    /// The pawn's `lookingDirection` (0 left, 1 middle, 2 right).
    pub looking_direction: u8,
    /// Look-at controller alpha target (visual).
    pub look_alpha: f32,
    /// Sequence nodes, indexed by [`WormAnim`].
    pub nodes: [AnimNodeState; WORM_ANIM_COUNT],
}

impl Default for WormAnimTree {
    fn default() -> Self {
        Self::after_begin_play()
    }
}

impl WormAnimTree {
    /// The tree as instanced from the template (active children from the
    /// stored `TargetWeight`s; `bPlaying`/`bLooping` from the nodes), after
    /// the pawn's begin-play sets the look-around list to the middle.
    #[must_use]
    pub fn after_begin_play() -> Self {
        let mut nodes = [AnimNodeState::default(); WORM_ANIM_COUNT];
        nodes[WormAnim::ShortLookLeft.index()].playing = true;
        nodes[WormAnim::ShortLookRight.index()].playing = true;
        nodes[WormAnim::SleepIdle.index()].looping = true;
        nodes[WormAnim::DiscoveredIdle.index()].looping = true;
        Self {
            state_anims: 0,
            sleep_anims: 0,
            woke_up: 0,
            alert_anims: 0,
            look_around: 1,
            look_left: 0,
            look_middle: 0,
            look_right: 0,
            looking_direction: 1,
            look_alpha: 0.0,
            nodes,
        }
    }

    /// The node of the look-around path.
    #[must_use]
    pub fn look_node(&self) -> WormAnim {
        match self.look_around {
            0 => {
                if self.look_left == 0 {
                    WormAnim::LongLookRight
                } else {
                    WormAnim::LeftToMiddle
                }
            }
            2 => {
                if self.look_right == 0 {
                    WormAnim::LongLookLeft
                } else {
                    WormAnim::RightToMiddle
                }
            }
            _ => {
                if self.look_middle == 0 {
                    WormAnim::ShortLookLeft
                } else {
                    WormAnim::ShortLookRight
                }
            }
        }
    }

    /// The fully active leaf node.
    #[must_use]
    pub fn active(&self) -> WormAnim {
        match self.state_anims {
            0 => {
                if self.sleep_anims == 0 {
                    WormAnim::FallAsleep
                } else {
                    WormAnim::SleepIdle
                }
            }
            1 => {
                if self.woke_up == 0 {
                    WormAnim::WakeUp
                } else {
                    self.look_node()
                }
            }
            _ => {
                if self.alert_anims == 0 {
                    WormAnim::DiscoveredIdle
                } else {
                    WormAnim::Scream
                }
            }
        }
    }

    /// State of a node.
    #[must_use]
    pub fn node(&self, anim: WormAnim) -> AnimNodeState {
        self.nodes[anim.index()]
    }

    /// `SetActiveChild` on a list with `bPlayActiveChild` whose new child is
    /// the sequence `anim`: `ReplayAnim` (from 0, own looping flag and rate).
    fn replay(&mut self, anim: WormAnim) {
        let n = &mut self.nodes[anim.index()];
        n.playing = true;
        n.position = 0.0;
    }

    fn play(&mut self, anims: &[WormAnim], looping: bool) {
        for a in anims {
            let n = &mut self.nodes[a.index()];
            n.playing = true;
            n.looping = looping;
            n.position = 0.0;
        }
    }

    /// `StateAnims.PlayAnim(bLoop, 1, 0)`: every node of the tree.
    fn play_all(&mut self, looping: bool) {
        self.play(&WormAnim::ALL, looping);
    }

    /// `SleepAnims.PlayAnim(bLoop, 1, 0)`.
    fn play_sleep(&mut self, looping: bool) {
        self.play(&[WormAnim::FallAsleep, WormAnim::SleepIdle], looping);
    }

    /// The pawn's `ResetAnimPositions` (positions only; play state kept).
    fn reset_positions(&mut self) {
        for a in [
            WormAnim::FallAsleep,
            WormAnim::WakeUp,
            WormAnim::DiscoveredIdle,
            WormAnim::SleepIdle,
            WormAnim::Scream,
        ] {
            self.nodes[a.index()].position = 0.0;
        }
    }

    /// `FallAsleepAnim.SetPosition(0)` and `WakeUpAnim.SetPosition(0)`.
    fn reset_sleep_wake(&mut self) {
        self.nodes[WormAnim::FallAsleep.index()].position = 0.0;
        self.nodes[WormAnim::WakeUp.index()].position = 0.0;
    }

    /// Advances the active node by `dt` (rate 1); returns a node that ended
    /// and calls the actor's `OnAnimEnd`.
    fn advance(&mut self, dt: f32) -> Option<WormAnim> {
        if !(dt.is_finite() && dt > 0.0) {
            return None;
        }
        let active = self.active();
        let length = active.length();
        let n = &mut self.nodes[active.index()];
        if !n.playing {
            return None;
        }
        n.position += dt;
        if n.position <= length {
            return None;
        }
        if n.looping {
            n.position = if length > 0.0 {
                n.position.rem_euclid(length)
            } else {
                0.0
            };
            return None;
        }
        n.position = length;
        n.playing = false;
        active.causes_actor_anim_end().then_some(active)
    }
}

/// Where the worm's state code resumes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WormPc {
    /// The state's `Begin`.
    #[default]
    Begin,
    /// `Sleeping`: after a fade step.
    SleepFade,
    /// `Sleeping`: after the random sleep.
    SleepRandom,
    /// `WakingUp`: after a fade step.
    WakeFade,
    /// `Awake`: after a check step.
    AwakeCheck,
    /// `Alerted`: after `alertedSleepTime`.
    AlertedSleep,
    /// `Alerted`: after a check step.
    AlertedCheck,
    /// `Screaming`: after a push step.
    ScreamPush,
}

/// Run-time state of one worm (controller + pawn).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct WormState {
    /// Controller state.
    pub state: WormStateName,
    /// State code is runnable (set by `GotoState` and after a wake-up).
    pub code_pending: bool,
    /// Remaining latent sleep, s.
    pub sleep: Option<f32>,
    /// Resume point of the state code.
    pub pc: WormPc,
    /// State-local `elapsedTime` / `elapsedAwakeTime`, s (cleared on a state
    /// change).
    pub elapsed: f32,
    /// State-local `fullAwakeTime`, s.
    pub full_awake_time: f32,
    /// State-local `bSleepIsQueued`.
    pub sleep_is_queued: bool,
    /// `bPaused` (`SeqAct_PauseWorm`).
    pub paused: bool,
    /// `bScreaming`.
    pub screaming: bool,
    /// `bPlayerDiscovered` (aim at the player).
    pub player_discovered: bool,
    /// `bWormAwake`.
    pub awake: bool,
    /// `bWormAlerted`.
    pub alerted: bool,
    /// `bWormWasShutDown`.
    pub shut_down: bool,
    /// `playerPosition`: reference for "the player moved".
    pub player_position: Vec3,
    /// `CurrentAim` (look-at target, visual).
    pub current_aim: Vec3,
    /// `lightstrength` (0..1, visual).
    pub light_strength: f32,
    /// `FinishedAlertTimer`.
    pub alert_timer: Option<ScriptTimer>,
    /// `CancelScreamTimer`.
    pub cancel_scream_timer: Option<ScriptTimer>,
    /// The pawn's animation tree.
    pub anim: WormAnimTree,
    /// Random stream.
    pub rng: Rng,
}

impl WormState {
    /// The state at level start (`Disabled`, its code pending).
    #[must_use]
    pub fn new(seed: u64) -> Self {
        Self {
            state: WormStateName::Disabled,
            code_pending: true,
            sleep: None,
            pc: WormPc::Begin,
            elapsed: 0.0,
            full_awake_time: 0.0,
            sleep_is_queued: false,
            paused: false,
            screaming: false,
            player_discovered: false,
            awake: false,
            alerted: false,
            shut_down: false,
            player_position: Vec3::ZERO,
            current_aim: Vec3::ZERO,
            light_strength: 0.0,
            alert_timer: None,
            cancel_scream_timer: None,
            anim: WormAnimTree::after_begin_play(),
            rng: Rng(seed),
        }
    }

    /// `GotoState`: `EndState` of the old state and cleared state locals when
    /// the state differs (G-TM-5); the code restarts at `Begin`.
    fn goto_state(&mut self, id: u32, new: WormStateName, out: &mut NpcOutput) {
        if new != self.state {
            match self.state {
                WormStateName::Awake => self.awake = false,
                WormStateName::Alerted => self.alerted = false,
                WormStateName::Screaming => self.screaming = false,
                _ => {}
            }
            self.elapsed = 0.0;
            self.full_awake_time = 0.0;
            self.sleep_is_queued = false;
            self.state = new;
            out.events.push(NpcEvent::WormState {
                worm: id,
                state: new,
            });
        }
        self.sleep = None;
        self.pc = WormPc::Begin;
        self.code_pending = true;
    }

    /// `StartWorm` (Kismet `SeqAct_StartWorm`).
    pub fn start(&mut self, id: u32, out: &mut NpcOutput) {
        self.goto_state(id, WormStateName::Idle, out);
        self.shut_down = false;
    }

    /// `ShutDownWorm` (Kismet `SeqAct_ShutDownWorm`; in `Awake` it also
    /// queues the sleep).
    pub fn shut_down(&mut self) {
        if self.state == WormStateName::Awake {
            self.sleep_is_queued = true;
        }
        self.shut_down = true;
    }

    /// `ToggleWormActive` (Kismet `SeqAct_PauseWorm`: input `UnPause` →
    /// false, `Pause` → true). Only the `Awake` check reads it.
    pub fn set_paused(&mut self, paused: bool) {
        self.paused = paused;
    }

    /// `ResetSleepTimer` (Kismet `SeqAct_WormResetSleepTimer`; only `Sleeping`
    /// has a body: it restarts the falling-asleep fade counter).
    pub fn reset_sleep_timer(&mut self) {
        if self.state == WormStateName::Sleeping {
            self.elapsed = 0.0;
        }
    }

    /// `NotifyKilled` (the game reports a player death: kill zones, dynamic
    /// kill zones; the worm's own kill applies it itself). Only `Screaming`
    /// reacts: it stops screaming and goes to sleep.
    pub fn notify_killed(&mut self, id: u32, out: &mut NpcOutput) {
        if self.state == WormStateName::Screaming {
            self.stop_screaming(id, out);
        }
    }

    fn stop_screaming(&mut self, id: u32, out: &mut NpcOutput) {
        out.events.push(NpcEvent::Worm {
            worm: id,
            kind: WormEventKind::StoppedScreaming,
        });
        out.events.push(NpcEvent::CameraShake {
            worm: id,
            start: false,
        });
        self.go_to_sleep(id, out);
    }

    /// `GoToSleep`.
    fn go_to_sleep(&mut self, id: u32, out: &mut NpcOutput) {
        out.events.push(NpcEvent::Worm {
            worm: id,
            kind: WormEventKind::FallingAsleep,
        });
        self.anim.reset_sleep_wake();
        self.anim.look_alpha = 0.0;
        self.goto_state(id, WormStateName::Sleeping, out);
    }

    /// The presentation of the worm: active animation node and its time, light
    /// strength (0..1) and look-at target.
    #[must_use]
    pub fn presentation(&self) -> (WormAnim, f32, f32, Vec3) {
        let a = self.anim.active();
        (
            a,
            self.anim.node(a).position,
            self.light_strength,
            self.current_aim,
        )
    }
}

/// What the worm sees of the player this tick.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WormSenses {
    /// Player pawn location, UU.
    pub player: Vec3,
    /// The player is inside a scream volume.
    pub in_scream_volume: bool,
    /// The player is inside a shadow volume.
    pub in_shadow: bool,
}

/// Result of one stretch of state code.
enum Step {
    Sleep(f32),
    End,
    Goto(WormStateName),
}

/// `IsPlayerMoving` (side effect: a non-moving check updates the reference).
fn worm_player_moving(w: &mut WormState, params: &WormParams, player: Vec3) -> bool {
    if (player - w.player_position).length() > params.position_sensitivity {
        return true;
    }
    w.player_position = player;
    false
}

/// One tick of a worm: the pawn's animation (and its end callbacks), the
/// controller's script `Tick` (aim), its state code, then its timers
/// (G-TM-1).
pub fn tick_worm(
    w: &mut WormState,
    def: &WormDef,
    senses: &WormSenses,
    dt: f32,
    out: &mut NpcOutput,
) {
    if !(dt.is_finite() && dt > 0.0) {
        return;
    }
    // Pawn: animation (TENTATIVE: the pawn ticks before its controller).
    if let Some(ended) = w.anim.advance(dt) {
        worm_anim_end(w, def.id, ended, out);
    }
    // Controller `Tick`: aim at the player (discovered) or back to the world
    // origin (CONFIRMED (src): the "centre" is the origin vector).
    let p = &def.params;
    let (target, alpha) = if w.player_discovered {
        (
            senses.player,
            p.look_around_speed * WORM_DISCOVERED_AIM_FACTOR * dt,
        )
    } else {
        (Vec3::ZERO, p.look_around_speed * dt)
    };
    w.current_aim += (target - w.current_aim) * alpha;
    // State code.
    process_worm_state(w, def, senses, dt, out);
    // Timers.
    if let Some(t) = &mut w.alert_timer
        && t.advance(dt)
    {
        w.alert_timer = None;
        if w.state == WormStateName::Alerted {
            // `FinishedAlertTimer` (only defined in `Alerted`; TENTATIVE: a
            // timer naming a function the current state lacks does nothing).
            out.events.push(NpcEvent::Worm {
                worm: def.id,
                kind: WormEventKind::FinishedAlerted,
            });
            w.anim.state_anims = 1;
            w.goto_state(def.id, WormStateName::Awake, out);
        }
    }
    if let Some(t) = &mut w.cancel_scream_timer
        && t.advance(dt)
    {
        // `CancelScreamTimer`: stopped-screaming event, `PlayerDied`, then
        // `NotifyKilled` reaches the worm (stops screaming in `Screaming`).
        w.cancel_scream_timer = None;
        out.events.push(NpcEvent::Worm {
            worm: def.id,
            kind: WormEventKind::StoppedScreaming,
        });
        out.effects.push(NpcEffect::KillPlayer { worm: def.id });
        w.notify_killed(def.id, out);
    }
}

/// `OnAnimEnd` of the worm pawn for the look nodes, and the controller's
/// look callbacks (only `Awake` has bodies).
fn worm_anim_end(w: &mut WormState, id: u32, node: WormAnim, out: &mut NpcOutput) {
    let to_side = match node {
        WormAnim::LongLookLeft | WormAnim::ShortLookLeft => {
            w.anim.look_around = 0;
            w.anim.looking_direction = 0;
            true
        }
        WormAnim::LongLookRight | WormAnim::ShortLookRight => {
            w.anim.look_around = 2;
            w.anim.looking_direction = 2;
            true
        }
        WormAnim::LeftToMiddle | WormAnim::RightToMiddle => {
            w.anim.look_around = 1;
            w.anim.looking_direction = 1;
            false
        }
        _ => return,
    };
    if w.state != WormStateName::Awake {
        return;
    }
    if to_side {
        // `FinishedLookingToSide` (short-circuit order kept for the random
        // stream).
        let middle =
            w.sleep_is_queued || w.rng.range(0.0, 100.0) >= WORM_LOOK_CHANCE || w.shut_down;
        if middle {
            worm_look_to_middle(w);
        } else {
            worm_look_to_side(w);
        }
    } else if w.sleep_is_queued || w.shut_down {
        // `FinishedLookingToMiddle` → `GoToSleep`.
        w.go_to_sleep(id, out);
    } else {
        worm_look_to_side(w);
    }
}

/// The pawn's `LookToMiddle` (the look lists replay the chosen node,
/// `bPlayActiveChild`).
fn worm_look_to_middle(w: &mut WormState) {
    match w.anim.looking_direction {
        0 => {
            w.anim.reset_positions();
            w.anim.look_left = 1;
            w.anim.replay(WormAnim::LeftToMiddle);
        }
        2 => {
            w.anim.reset_positions();
            w.anim.look_right = 1;
            w.anim.replay(WormAnim::RightToMiddle);
        }
        _ => {}
    }
}

/// The pawn's `LookToSide` (the look lists replay the chosen node).
fn worm_look_to_side(w: &mut WormState) {
    match w.anim.looking_direction {
        0 => {
            w.anim.reset_positions();
            w.anim.look_left = 0;
            w.anim.replay(WormAnim::LongLookRight);
        }
        1 => {
            let left = w.rng.range(0.0, 100.0) < WORM_LOOK_CHANCE;
            w.anim.reset_positions();
            w.anim.look_middle = if left { 0 } else { 1 };
            w.anim.replay(if left {
                WormAnim::ShortLookLeft
            } else {
                WormAnim::ShortLookRight
            });
        }
        _ => {
            w.anim.reset_positions();
            w.anim.look_right = 0;
            w.anim.replay(WormAnim::LongLookLeft);
        }
    }
}

/// `AActor::ProcessState` for the worm controller (see the module docs).
fn process_worm_state(
    w: &mut WormState,
    def: &WormDef,
    senses: &WormSenses,
    dt: f32,
    out: &mut NpcOutput,
) {
    if w.sleep.is_some() {
        if !poll_sleep(&mut w.sleep, dt) {
            return;
        }
    } else if !w.code_pending {
        return;
    }
    let mut changes = 0u32;
    loop {
        match worm_step(w, def, senses, out) {
            Step::Sleep(t) => {
                w.sleep = Some(t);
                w.code_pending = true;
                return;
            }
            Step::End => {
                w.code_pending = false;
                return;
            }
            Step::Goto(state) => {
                w.goto_state(def.id, state, out);
                if changes >= 4 {
                    return;
                }
                changes += 1;
            }
        }
    }
}

/// Runs the current state's code from its resume point to the next latent
/// sleep, its end, or a state change.
fn worm_step(w: &mut WormState, def: &WormDef, s: &WormSenses, out: &mut NpcOutput) -> Step {
    let id = def.id;
    let p = &def.params;
    match w.state {
        WormStateName::Disabled => {
            w.anim.state_anims = 0;
            w.anim.sleep_anims = 1;
            w.anim.replay(WormAnim::SleepIdle);
            w.anim.play_sleep(true);
            w.anim.look_alpha = 0.0;
            Step::End
        }
        WormStateName::Idle => Step::Goto(WormStateName::WakingUp),
        WormStateName::ScriptedMove | WormStateName::ScriptedRouteMove => Step::End,
        WormStateName::Sleeping => {
            match w.pc {
                WormPc::SleepFade => w.elapsed += WORM_STEP,
                WormPc::SleepRandom => {
                    if w.shut_down {
                        return Step::End;
                    }
                    // `GoToWakeUp`.
                    w.anim.reset_sleep_wake();
                    return Step::Goto(WormStateName::WakingUp);
                }
                _ => {
                    w.elapsed = 0.0;
                    out.events.push(NpcEvent::CameraShake {
                        worm: id,
                        start: false,
                    });
                    w.anim.state_anims = 0;
                    w.anim.sleep_anims = 0;
                    w.anim.replay(WormAnim::FallAsleep);
                    w.anim.play_all(false);
                    w.anim.reset_positions();
                    w.player_discovered = false;
                    w.screaming = false;
                    w.cancel_scream_timer = None;
                }
            }
            if w.elapsed < p.fall_asleep_time {
                w.light_strength = 1.0 - w.elapsed / p.fall_asleep_time;
                w.pc = WormPc::SleepFade;
                return Step::Sleep(WORM_STEP);
            }
            if !w.shut_down {
                let t = w.rng.range(p.sleep_time_min, p.sleep_time_max);
                w.pc = WormPc::SleepRandom;
                return Step::Sleep(t);
            }
            Step::End
        }
        WormStateName::WakingUp => {
            if w.pc == WormPc::WakeFade {
                w.elapsed += WORM_STEP;
            } else {
                out.events.push(NpcEvent::Worm {
                    worm: id,
                    kind: WormEventKind::WakingUp,
                });
                w.elapsed = 0.0;
                w.anim.woke_up = 0;
                w.anim.state_anims = 1;
                w.anim.look_alpha = 0.0;
                w.anim.play_all(false);
            }
            if w.elapsed < p.wake_up_time {
                w.light_strength = w.elapsed / p.wake_up_time;
                w.pc = WormPc::WakeFade;
                return Step::Sleep(WORM_STEP);
            }
            // `GoToAwake`.
            w.anim.state_anims = 1;
            Step::Goto(WormStateName::Awake)
        }
        WormStateName::Awake => {
            if w.pc != WormPc::AwakeCheck {
                out.events.push(NpcEvent::Worm {
                    worm: id,
                    kind: WormEventKind::Awaken,
                });
                w.sleep_is_queued = false;
                w.full_awake_time = w.rng.range(p.awake_time_min, p.awake_time_max);
                w.awake = true;
                w.light_strength = 1.0;
                w.anim.looking_direction = 1;
                w.anim.woke_up = 1;
                w.player_position = s.player;
                w.anim.look_alpha = 0.0;
            }
            if w.elapsed < w.full_awake_time {
                // `CheckPlayer` (Awake: also gated by `bPaused`).
                let found = s.in_scream_volume
                    && !w.paused
                    && !s.in_shadow
                    && worm_player_moving(w, p, s.player);
                if found {
                    // `FoundPlayer` (the three look lists replay their
                    // child 0).
                    w.anim.looking_direction = 1;
                    w.anim.woke_up = 1;
                    w.anim.look_around = 1;
                    w.anim.look_right = 0;
                    w.anim.replay(WormAnim::LongLookLeft);
                    w.anim.look_left = 0;
                    w.anim.replay(WormAnim::LongLookRight);
                    w.anim.look_middle = 0;
                    w.anim.replay(WormAnim::ShortLookLeft);
                    return Step::Goto(WormStateName::Alerted);
                }
                w.elapsed += WORM_STEP;
                w.pc = WormPc::AwakeCheck;
                return Step::Sleep(WORM_STEP);
            }
            w.sleep_is_queued = true;
            Step::End
        }
        WormStateName::Alerted => {
            match w.pc {
                WormPc::AlertedSleep => w.player_position = s.player,
                WormPc::AlertedCheck => {}
                _ => {
                    out.events.push(NpcEvent::Worm {
                        worm: id,
                        kind: WormEventKind::Alerted,
                    });
                    w.player_discovered = true;
                    w.alerted = true;
                    w.anim.state_anims = 2;
                    w.anim.alert_anims = 0;
                    w.anim.replay(WormAnim::DiscoveredIdle);
                    w.anim.reset_positions();
                    w.anim.play_all(false);
                    w.anim.look_alpha = 1.0;
                    w.alert_timer = Some(ScriptTimer::new(p.alerted_time));
                    w.pc = WormPc::AlertedSleep;
                    return Step::Sleep(p.alerted_sleep_time);
                }
            }
            // `CheckPlayer` (Alerted: no pause gate) → `NoticedPlayer`.
            let noticed = s.in_scream_volume && !s.in_shadow && worm_player_moving(w, p, s.player);
            if noticed {
                w.alert_timer = None;
                return Step::Goto(WormStateName::Screaming);
            }
            w.pc = WormPc::AlertedCheck;
            Step::Sleep(WORM_STEP)
        }
        WormStateName::Screaming => {
            if w.pc != WormPc::ScreamPush {
                out.events.push(NpcEvent::Worm {
                    worm: id,
                    kind: WormEventKind::Screaming,
                });
                out.events.push(NpcEvent::CameraShake {
                    worm: id,
                    start: true,
                });
                if w.cancel_scream_timer.is_none() {
                    w.cancel_scream_timer = Some(ScriptTimer::new(p.scream_time_max));
                }
                w.anim.look_alpha = 1.0;
                w.screaming = true;
                w.anim.alert_anims = 1;
                w.anim.replay(WormAnim::Scream);
            }
            // `PushPlayer`.
            let dir = safe_normal(s.player - def.location) * p.player_push;
            out.effects.push(NpcEffect::PushPlayer {
                worm: id,
                delta_v: Vec3::new(dir.x, dir.y, p.z_velocity_offset),
            });
            if !s.in_scream_volume {
                // `StopScreaming` (from state code: `Sleeping` runs at once).
                out.events.push(NpcEvent::Worm {
                    worm: id,
                    kind: WormEventKind::StoppedScreaming,
                });
                out.events.push(NpcEvent::CameraShake {
                    worm: id,
                    start: false,
                });
                out.events.push(NpcEvent::Worm {
                    worm: id,
                    kind: WormEventKind::FallingAsleep,
                });
                w.anim.reset_sleep_wake();
                w.anim.look_alpha = 0.0;
                return Step::Goto(WormStateName::Sleeping);
            }
            w.pc = WormPc::ScreamPush;
            Step::Sleep(WORM_STEP)
        }
    }
}

// ---------------------------------------------------------------------------
// Maddie, the backpack Maddie and villagers (never placed; see module docs).
// ---------------------------------------------------------------------------

/// `ASAMUNPC_MaddiePawn` states.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MaddieStateName {
    /// `Idle` (auto state, empty).
    #[default]
    Idle,
    /// `TalkingWithPlayer`: no script enters it (CONFIRMED (src)).
    TalkingWithPlayer,
}

/// Run-time state of a Maddie pawn.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct MaddieState {
    /// State.
    pub state: MaddieStateName,
    /// Head/eye look-at target (her `Tick` aims both at the player).
    pub look_target: Vec3,
}

impl MaddieState {
    /// Enters `TalkingWithPlayer` (no original caller).
    pub fn talk(&mut self) {
        self.state = MaddieStateName::TalkingWithPlayer;
    }

    /// One tick: look at the player; `TalkingWithPlayer` returns to `Idle` as
    /// soon as the player is at least 200 uu away. (Its loop has no latent
    /// call, so while the player stays close the original would trip the
    /// engine's runaway-loop guard; TENTATIVE, never reached.)
    pub fn tick(&mut self, def: &MaddieDef, player: Vec3) {
        self.look_target = player;
        if self.state == MaddieStateName::TalkingWithPlayer
            && (player - def.location).length() >= MADDIE_TALK_RANGE
        {
            self.state = MaddieStateName::Idle;
        }
    }
}

/// Backpack Maddie animations (`BackpackMaddieAnims`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BackpackAnim {
    /// `Wave` → [`BACKPACK_WAVE_SEQUENCE`].
    Wave,
}

/// The backpack Maddie (`ASAMUBackpackMaddie`, spawned by
/// `SeqAct_MaddieBackpack`; no shipped map uses the action).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BackpackState {
    /// The actor exists, attached to the hand mesh's `RootSocket`.
    pub attached: bool,
    /// Last animation started.
    pub anim: Option<BackpackAnim>,
    /// Times an animation was started (presentation restarts on change).
    pub anim_serial: u32,
}

impl BackpackState {
    /// `SeqAct_MaddieBackpack` input `Enable` (spawn if needed, attach) or
    /// `Disable` (detach + destroy).
    pub fn set_enabled(&mut self, enable: bool) {
        if enable {
            self.attached = true;
        } else {
            *self = Self {
                anim_serial: self.anim_serial,
                ..Self::default()
            };
        }
    }

    /// `SeqAct_PlayMaddieBackpackAnim` (needs the spawned arms).
    pub fn play(&mut self, anim: BackpackAnim) -> bool {
        if !self.attached {
            return false;
        }
        self.anim = Some(anim);
        self.anim_serial = self.anim_serial.wrapping_add(1);
        true
    }
}

/// `ASAMUNPC_Villager` states.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VillagerStateName {
    /// `Disabled`.
    Disabled,
    /// `Idle` (auto state).
    #[default]
    Idle,
    /// `WalkingScriptedPath`.
    WalkingScriptedPath,
    /// `Roaming`.
    Roaming,
    /// `TalkWithPawn` (pushed by `StartTalkingWithPawn`; no original caller).
    TalkWithPawn,
}

/// Where the villager's state code resumes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VillagerPc {
    /// `Begin`.
    #[default]
    Begin,
    /// `Idle`: after the pause.
    IdlePause,
    /// Moving to the path point `path_index` (first move).
    PathFirstMove,
    /// Path forward: moving.
    PathForwardMove,
    /// Path forward: after the poll sleep.
    PathForwardPoll,
    /// After the end wait.
    PathEndWait,
    /// Path back: moving.
    PathBackMove,
    /// Path back: after the poll sleep.
    PathBackPoll,
    /// Roaming: moving.
    RoamMove,
    /// Roaming: after the poll sleep.
    RoamPoll,
}

/// A saved state for `PushState`/`PopState`.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct VillagerSaved {
    /// State.
    pub state: VillagerStateName,
    /// Resume point.
    pub pc: VillagerPc,
    /// Remaining sleep.
    pub sleep: Option<f32>,
}

/// Run-time state of a villager pawn + controller.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct VillagerState {
    /// State.
    pub state: VillagerStateName,
    /// Code runnable.
    pub code_pending: bool,
    /// Remaining latent sleep.
    pub sleep: Option<f32>,
    /// Resume point.
    pub pc: VillagerPc,
    /// Pawn location, UU.
    pub location: Vec3,
    /// Pawn yaw, rotator units.
    pub yaw: i32,
    /// Moving (walk animation).
    pub moving: bool,
    /// `currentScriptedPathIndex`.
    pub path_index: usize,
    /// Roaming target.
    pub move_target: Option<NavPoint>,
    /// `Enemy` (the seen player).
    pub enemy: bool,
    /// Pushed state (`TalkWithPawn`).
    pub saved: Option<VillagerSaved>,
    /// Pawn being talked to (location, for the focus).
    pub talk_focus: Option<Vec3>,
    /// Random stream.
    pub rng: Rng,
}

impl VillagerState {
    /// The state at level start.
    #[must_use]
    pub fn new(def: &VillagerDef, seed: u64) -> Self {
        Self {
            state: VillagerStateName::Idle,
            code_pending: true,
            sleep: None,
            pc: VillagerPc::Begin,
            location: def.location,
            yaw: def.rotation[1],
            moving: false,
            path_index: 0,
            move_target: None,
            enemy: false,
            saved: None,
            talk_focus: None,
            rng: Rng(seed),
        }
    }

    fn goto(&mut self, s: VillagerStateName) {
        self.state = s;
        self.pc = VillagerPc::Begin;
        self.sleep = None;
        self.code_pending = true;
        self.moving = false;
    }

    /// `StartTalkingWithPawn` (pushes `TalkWithPawn`).
    pub fn start_talking(&mut self, pawn_location: Vec3) {
        if self.state == VillagerStateName::TalkWithPawn {
            return;
        }
        self.saved = Some(VillagerSaved {
            state: self.state,
            pc: self.pc,
            sleep: self.sleep,
        });
        self.talk_focus = Some(pawn_location);
        self.state = VillagerStateName::TalkWithPawn;
        self.pc = VillagerPc::Begin;
        self.sleep = None;
        self.code_pending = true;
        self.moving = false;
    }

    /// `StopTalking` (only `TalkWithPawn` pops back).
    pub fn stop_talking(&mut self) {
        if self.state != VillagerStateName::TalkWithPawn {
            return;
        }
        if let Some(s) = self.saved.take() {
            self.state = s.state;
            self.pc = s.pc;
            self.sleep = s.sleep;
            self.code_pending = true;
        }
        self.talk_focus = None;
    }
}

/// `atan(z)` for `|z| ≤ 1` from IEEE basic operations only (two half-angle
/// reductions and a series), so the result is the same on every platform
/// (the platform `atan2` is not guaranteed to be).
fn det_atan(z: f64) -> f64 {
    let r1 = z / (1.0 + (1.0 + z * z).sqrt());
    let r2 = r1 / (1.0 + (1.0 + r1 * r1).sqrt());
    let x2 = r2 * r2;
    let mut term = r2;
    let mut sum = r2;
    for k in 1..12u32 {
        term *= -x2;
        sum += term / f64::from(2 * k + 1);
    }
    4.0 * sum
}

/// Deterministic `atan2(y, x)` (see [`det_atan`]).
fn det_atan2(y: f64, x: f64) -> f64 {
    use std::f64::consts::{FRAC_PI_2, PI};
    if !(x.is_finite() && y.is_finite()) || (x == 0.0 && y == 0.0) {
        return 0.0;
    }
    if y.abs() <= x.abs() {
        let a = det_atan(y / x);
        if x > 0.0 {
            a
        } else if y >= 0.0 {
            a + PI
        } else {
            a - PI
        }
    } else {
        let a = det_atan(x / y);
        if y > 0.0 {
            FRAC_PI_2 - a
        } else {
            -FRAC_PI_2 - a
        }
    }
}

/// UE3 yaw (rotator units) of a direction (deterministic).
fn yaw_units(dir: Vec3) -> i32 {
    let a = det_atan2(f64::from(dir.y), f64::from(dir.x));
    ((a * 32_768.0 / std::f64::consts::PI).round() as i64 & 0xFFFF) as i32
}

/// Moves `v` toward `target` (XY) at `speed` for `dt`; returns `true` when
/// it reached the target's reach radius (TENTATIVE kinematic stand-in for the
/// native `MoveToward` + walking physics + `ReachedDestination`).
fn villager_move(v: &mut VillagerState, target: &NavPoint, dt: f32) -> bool {
    let reach = VILLAGER_COLLISION_RADIUS + target.radius;
    let to = (target.location - v.location).truncate();
    let dist = to.length();
    if dist <= reach {
        v.moving = false;
        return true;
    }
    let step = NPC_GROUND_SPEED * dt;
    let dir = to / dist;
    v.yaw = yaw_units(dir.extend(0.0));
    if step >= dist - reach {
        let travel = (dist - reach).max(0.0);
        v.location += (dir * travel).extend(0.0);
        v.moving = false;
        true
    } else {
        v.location += (dir * step).extend(0.0);
        v.moving = true;
        false
    }
}

/// One tick of a villager (`nav` = the level's navigation points for
/// `FindRandomDest`; TENTATIVE: reachability and path finding are not
/// modelled).
pub fn tick_villager(
    v: &mut VillagerState,
    def: &VillagerDef,
    nav: &[NavPoint],
    player: Vec3,
    dt: f32,
) {
    if !(dt.is_finite() && dt > 0.0) {
        return;
    }
    // Sight (TENTATIVE: no line-of-sight test): `SeePlayer` / `EnemyNotVisible`
    // only matter in `Roaming`.
    if v.state == VillagerStateName::Roaming {
        let to = player - v.location;
        // The engine's table-based rotation (deterministic).
        let facing = crate::rotation::forward([0, v.yaw, 0]).as_vec3();
        let visible = to.length() <= VILLAGER_SIGHT_RADIUS
            && safe_normal(to).dot(facing) >= VILLAGER_PERIPHERAL_VISION;
        v.enemy = visible;
    }
    // Latent movement in progress.
    match v.pc {
        VillagerPc::PathFirstMove | VillagerPc::PathForwardMove | VillagerPc::PathBackMove => {
            if let Some(t) = def.scripted_path.get(v.path_index)
                && !villager_move(v, t, dt)
            {
                return;
            }
            v.code_pending = true;
        }
        VillagerPc::RoamMove => {
            if let Some(t) = v.move_target
                && !villager_move(v, &t, dt)
            {
                return;
            }
            v.code_pending = true;
        }
        _ => {}
    }
    if v.sleep.is_some() {
        if !poll_sleep(&mut v.sleep, dt) {
            return;
        }
    } else if !v.code_pending {
        return;
    }
    for _ in 0..5 {
        let next = villager_step(v, def, nav, player);
        match next {
            VillagerStep::Sleep(t) => {
                v.sleep = Some(t);
                v.code_pending = true;
                return;
            }
            VillagerStep::Latent => {
                v.code_pending = false;
                return;
            }
            VillagerStep::End => {
                v.code_pending = false;
                return;
            }
            VillagerStep::Goto(s) => v.goto(s),
        }
    }
}

/// `Pawn.ReachedDestination(MoveTarget)` (TENTATIVE stand-in: horizontal
/// distance within the two cylinder radii; no target counts as reached).
fn villager_reached(v: &VillagerState) -> bool {
    v.move_target.is_none_or(|t| {
        (t.location - v.location).truncate().length() <= VILLAGER_COLLISION_RADIUS + t.radius
    })
}

enum VillagerStep {
    Sleep(f32),
    Latent,
    End,
    Goto(VillagerStateName),
}

fn villager_step(
    v: &mut VillagerState,
    def: &VillagerDef,
    nav: &[NavPoint],
    player: Vec3,
) -> VillagerStep {
    let last = def.scripted_path.len().saturating_sub(1);
    match v.state {
        VillagerStateName::Disabled => VillagerStep::End,
        VillagerStateName::TalkWithPawn => {
            // `MoveToward(FindPathToward(self))` finds no path (no move);
            // `Focus` = the pawn.
            if let Some(f) = v.talk_focus {
                v.yaw = yaw_units(f - v.location);
            }
            VillagerStep::End
        }
        VillagerStateName::Idle => {
            if v.pc == VillagerPc::Begin && v.rng.range(0.0, 100.0) < VILLAGER_IDLE_PAUSE_CHANCE {
                v.pc = VillagerPc::IdlePause;
                return VillagerStep::Sleep(
                    v.rng.range(VILLAGER_IDLE_PAUSE.0, VILLAGER_IDLE_PAUSE.1),
                );
            }
            if def.use_scripted_path {
                VillagerStep::Goto(VillagerStateName::WalkingScriptedPath)
            } else {
                VillagerStep::Goto(VillagerStateName::Roaming)
            }
        }
        VillagerStateName::WalkingScriptedPath => {
            if def.scripted_path.is_empty() {
                // `MoveToward(None)` fails at once and both loops are empty:
                // only the end wait remains, then `Begin` again.
                return VillagerStep::Sleep(
                    v.rng
                        .range(VILLAGER_PATH_END_WAIT.0, VILLAGER_PATH_END_WAIT.1),
                );
            }
            match v.pc {
                VillagerPc::Begin => {
                    v.path_index = 0;
                    v.pc = VillagerPc::PathFirstMove;
                    VillagerStep::Latent
                }
                VillagerPc::PathFirstMove | VillagerPc::PathForwardPoll => {
                    if v.path_index < last {
                        v.path_index += 1;
                        v.pc = VillagerPc::PathForwardMove;
                        return VillagerStep::Latent;
                    }
                    v.pc = VillagerPc::PathEndWait;
                    VillagerStep::Sleep(
                        v.rng
                            .range(VILLAGER_PATH_END_WAIT.0, VILLAGER_PATH_END_WAIT.1),
                    )
                }
                VillagerPc::PathForwardMove => {
                    v.pc = VillagerPc::PathForwardPoll;
                    VillagerStep::Sleep(VILLAGER_POLL)
                }
                VillagerPc::PathEndWait | VillagerPc::PathBackPoll => {
                    if v.path_index > 0 {
                        v.path_index -= 1;
                        v.pc = VillagerPc::PathBackMove;
                        return VillagerStep::Latent;
                    }
                    v.pc = VillagerPc::Begin;
                    VillagerStep::Goto(VillagerStateName::WalkingScriptedPath)
                }
                VillagerPc::PathBackMove => {
                    v.pc = VillagerPc::PathBackPoll;
                    VillagerStep::Sleep(VILLAGER_POLL)
                }
                _ => VillagerStep::End,
            }
        }
        VillagerStateName::Roaming => match v.pc {
            VillagerPc::Begin => {
                if nav.is_empty() {
                    // `FindRandomDest` found nothing.
                    return VillagerStep::Goto(VillagerStateName::Disabled);
                }
                let pick = (v.rng.frand() * nav.len() as f32) as usize;
                v.move_target = nav.get(pick.min(nav.len() - 1)).copied();
                // `Roam`: the loop tests "reached" before the first move.
                if villager_reached(v) {
                    return VillagerStep::Goto(VillagerStateName::Idle);
                }
                v.pc = VillagerPc::RoamMove;
                VillagerStep::Latent
            }
            VillagerPc::RoamMove => {
                if v.enemy && (player - v.location).length() < VILLAGER_REACHED_TOLERANCE {
                    // CONFIRMED (src) quirk: faces the direction of the
                    // player's *location vector*, not the player.
                    v.yaw = yaw_units(safe_normal(player));
                }
                v.pc = VillagerPc::RoamPoll;
                VillagerStep::Sleep(VILLAGER_POLL)
            }
            VillagerPc::RoamPoll => {
                if villager_reached(v) {
                    return VillagerStep::Goto(VillagerStateName::Idle);
                }
                v.pc = VillagerPc::RoamMove;
                VillagerStep::Latent
            }
            _ => VillagerStep::End,
        },
    }
}

// ---------------------------------------------------------------------------
// Story interactables, collectibles, glow flowers, foliage.
// ---------------------------------------------------------------------------

/// `ASAMUInteractable_Actor` states.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StoryItemStateName {
    /// `Idle` (auto state).
    #[default]
    Idle,
    /// `FadingDown`: the interact symbol fades over 101 steps.
    FadingDown,
    /// `Disabled`: no uses left.
    Disabled,
}

/// Run-time state of a story interactable.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct StoryItemState {
    /// `timesInteractedWith`.
    pub times: i32,
    /// State.
    pub state: StoryItemStateName,
    /// Code runnable.
    pub code_pending: bool,
    /// Remaining latent sleep.
    pub sleep: Option<f32>,
    /// Fade loop index (`forIndex`).
    pub fade_index: u32,
    /// The glow material's `Enabled` parameter (1 visible .. 0 gone).
    pub glow: f32,
}

impl Default for StoryItemState {
    fn default() -> Self {
        Self {
            times: 0,
            state: StoryItemStateName::Idle,
            code_pending: true,
            sleep: None,
            fade_index: 0,
            glow: 1.0,
        }
    }
}

impl StoryItemState {
    fn goto(&mut self, s: StoryItemStateName) {
        self.state = s;
        self.sleep = None;
        self.code_pending = true;
        self.fade_index = 0;
    }

    /// Uses left (`None` = unlimited).
    #[must_use]
    pub fn accepts(&self, def: &StoryItemDef) -> bool {
        def.max_interact_times == 0 || self.times < def.max_interact_times
    }
}

/// Run-time state of a collectible.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CollectibleState {
    /// `bCollected`.
    pub collected: bool,
    /// The player touches its trigger.
    pub touching: bool,
}

/// `ASAMUGlowFlower` code positions (labels and loop checks of `Glowing`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FlowerPc {
    /// `Begin` of `Glowing`.
    #[default]
    Begin,
    /// `StartGlowing` label.
    Start,
    /// Fade-in loop check.
    FadeIn,
    /// Hold loop check.
    Hold,
    /// `StopGlowing` label.
    Stop,
    /// Fade-out loop check.
    FadeOut,
}

/// Run-time state of a glow flower.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct GlowFlowerState {
    /// In `Glowing` (else `NotGlowing`).
    pub glowing: bool,
    /// Code runnable.
    pub code_pending: bool,
    /// Remaining latent sleep.
    pub sleep: Option<f32>,
    /// Resume point.
    pub pc: FlowerPc,
    /// State-local `glowTimeRemaining`, s.
    pub glow_time_remaining: f32,
    /// State-local `currentFadeTime`, s.
    pub current_fade_time: f32,
    /// State-local `bWasGrappled`.
    pub was_grappled: bool,
    /// Brightness alpha of its lights (0..1).
    pub alpha: f32,
}

/// Foliage index: grid cell → foliage indices. A trigger wider than
/// [`FOLIAGE_MAX_CELL_SPAN`] cells is kept in a separate list that every
/// query tests, so the grid holds at most `(span + 1)²` entries per plant
/// whatever the (possibly hostile) radii are.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct FoliageGrid {
    cells: BTreeMap<(i32, i32), Vec<usize>>,
    #[serde(default)]
    large: Vec<usize>,
}

/// Widest trigger (in grid cells per axis) stored in the foliage grid
/// (ours; performance and memory bound only).
const FOLIAGE_MAX_CELL_SPAN: i32 = 4;

fn cell_of(v: f32) -> i32 {
    let c = (v / FOLIAGE_CELL).floor();
    if c.is_finite() {
        c.clamp(i32::MIN as f32, i32::MAX as f32) as i32
    } else {
        0
    }
}

impl FoliageGrid {
    fn new(foliage: &[FoliageDef]) -> Self {
        let mut cells: BTreeMap<(i32, i32), Vec<usize>> = BTreeMap::new();
        let mut large = Vec::new();
        for (i, f) in foliage.iter().enumerate() {
            let c = &f.trigger;
            if !(c.center.is_finite() && c.radius.is_finite()) {
                continue;
            }
            let (x0, x1) = (
                cell_of(c.center.x - c.radius),
                cell_of(c.center.x + c.radius),
            );
            let (y0, y1) = (
                cell_of(c.center.y - c.radius),
                cell_of(c.center.y + c.radius),
            );
            if x1.saturating_sub(x0) > FOLIAGE_MAX_CELL_SPAN
                || y1.saturating_sub(y0) > FOLIAGE_MAX_CELL_SPAN
            {
                large.push(i);
                continue;
            }
            for x in x0..=x1 {
                for y in y0..=y1 {
                    cells.entry((x, y)).or_default().push(i);
                }
            }
        }
        Self { cells, large }
    }

    /// Candidate indices near the segment's bounds (sorted, unique).
    fn candidates(&self, lo: Vec3, hi: Vec3) -> Vec<usize> {
        let (x0, x1) = (cell_of(lo.x), cell_of(hi.x));
        let (y0, y1) = (cell_of(lo.y), cell_of(hi.y));
        let mut out: BTreeSet<usize> = self.large.iter().copied().collect();
        if x1.saturating_sub(x0) > 256 || y1.saturating_sub(y0) > 256 {
            // A teleport-sized move: test everything.
            for v in self.cells.values() {
                out.extend(v.iter().copied());
            }
        } else {
            for x in x0..=x1 {
                for y in y0..=y1 {
                    if let Some(v) = self.cells.get(&(x, y)) {
                        out.extend(v.iter().copied());
                    }
                }
            }
        }
        out.into_iter().collect()
    }
}

// ---------------------------------------------------------------------------
// The NPC runtime.
// ---------------------------------------------------------------------------

/// Run-time state of every NPC-related actor of a map.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct NpcRuntime {
    /// Worms (same order as `NpcScene::worms`).
    pub worms: Vec<WormState>,
    /// Maddie pawns.
    pub maddies: Vec<MaddieState>,
    /// Villagers.
    pub villagers: Vec<VillagerState>,
    /// Collectibles.
    pub collectibles: Vec<CollectibleState>,
    /// Story interactables.
    pub story_items: Vec<StoryItemState>,
    /// Glow flowers.
    pub flowers: Vec<GlowFlowerState>,
    /// Per foliage actor: touching.
    pub foliage_touching: Vec<bool>,
    /// The backpack Maddie.
    pub backpack: BackpackState,
    /// Time-trial game (collectibles hidden and non-colliding).
    pub time_trial: bool,
    /// Skinned actors' animation and look-at state (same order as
    /// `NpcScene::skinned`).
    #[serde(default)]
    pub skinned: Vec<SkinnedAnimState>,
    /// Per story item: its children (`linkedInteractables` + children that
    /// registered with it at begin play), indices.
    children: Vec<Vec<usize>>,
    foliage_grid: FoliageGrid,
    /// Draws for `AnimNotify_Sound.PercentToPlay` (ours).
    #[serde(default)]
    notify_rng: Rng,
}

/// `SeqAct_SetLookAtTarget` state of a look-at actor.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct LookAtState {
    /// The stored target (the original's tick never reads it, NPCS.md §6).
    pub target: Option<u32>,
    /// `lookAtOffset` added to the player pawn's location for the head.
    pub head_offset: Vec3,
    /// `eyesLookAtOffset` for the eyes.
    pub eyes_offset: Vec3,
}

/// A skinned actor's animation, skeletal-control and look-at state.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SkinnedAnimState {
    /// Its first skeletal component's own sequence node (ambient actors),
    /// ticking every frame.
    pub ambient: Option<SequenceNode>,
    /// The slot node Matinee's `SetAnimPosition` drives, once a Matinee has
    /// animated the actor (it then holds the pose Matinee left).
    pub matinee: Option<SequenceNode>,
    /// Control strengths Matinee set (`SetSkelControlStrength`), by name.
    pub controls: BTreeMap<String, f32>,
    /// Look-at offsets.
    pub look_at: LookAtState,
}

impl SkinnedAnimState {
    /// The node that poses the mesh: Matinee's once used, else the ambient
    /// one.
    #[must_use]
    pub fn current(&self) -> Option<&SequenceNode> {
        self.matinee.as_ref().or(self.ambient.as_ref())
    }
}

/// Seed of the notify draws (ours).
const NOTIFY_SEED: u64 = 0x4E4F_5449_4659_0001;

/// The stock skeletal actor class whose `SetAnimPosition` script event
/// drives the component's own `AnimNodeSequence` (its `...MAT` subclasses
/// override the event with the native slot path).
pub const PLAIN_SKELETAL_ACTOR_CLASS: &str = "Engine.SkeletalMeshActor";

/// SplitMix-style seed mixing per actor (ours).
fn actor_seed(seed: u64, id: u32) -> u64 {
    seed ^ (u64::from(id).wrapping_mul(0x9E37_79B9_7F4A_7C15))
}

impl NpcRuntime {
    /// The state at level start.
    #[must_use]
    pub fn new(scene: &NpcScene, seed: u64, time_trial: bool) -> Self {
        let index_of: BTreeMap<u32, usize> = scene
            .story_items
            .iter()
            .enumerate()
            .map(|(i, s)| (s.id, i))
            .collect();
        let mut children: Vec<Vec<usize>> = scene
            .story_items
            .iter()
            .map(|s| {
                s.linked_children
                    .iter()
                    .filter_map(|c| index_of.get(c).copied())
                    .collect()
            })
            .collect();
        // `AddChildCrystal`: a non-parent with a linked parent registers.
        for (i, s) in scene.story_items.iter().enumerate() {
            if !s.parent
                && let Some(p) = s.linked_parent.and_then(|p| index_of.get(&p))
                && let Some(list) = children.get_mut(*p)
                && !list.contains(&i)
            {
                list.push(i);
            }
        }
        Self {
            worms: scene
                .worms
                .iter()
                .map(|w| WormState::new(actor_seed(seed, w.id)))
                .collect(),
            maddies: vec![MaddieState::default(); scene.maddies.len()],
            villagers: scene
                .villagers
                .iter()
                .map(|v| VillagerState::new(v, actor_seed(seed, v.id)))
                .collect(),
            collectibles: vec![CollectibleState::default(); scene.collectibles.len()],
            story_items: vec![StoryItemState::default(); scene.story_items.len()],
            flowers: vec![GlowFlowerState::default(); scene.flowers.len()],
            foliage_touching: vec![false; scene.foliage.len()],
            backpack: BackpackState::default(),
            time_trial,
            skinned: scene
                .skinned
                .iter()
                .map(|d| SkinnedAnimState {
                    ambient: d
                        .components
                        .first()
                        .and_then(|c| c.animation.as_ref())
                        .map(|a| {
                            SequenceNode::new(
                                Some(a.sequence.clone()),
                                a.start_time,
                                a.rate,
                                a.playing,
                                a.looping,
                            )
                        }),
                    ..SkinnedAnimState::default()
                })
                .collect(),
            children,
            foliage_grid: FoliageGrid::new(&scene.foliage),
            notify_rng: Rng(seed ^ NOTIFY_SEED),
        }
    }

    /// The fired notifies `fired` (indices into `info`) of skinned actor
    /// `id`: sounds become events, Kismet notifies are returned (or, with
    /// `kismet_events`, become [`NpcEvent::AnimNotify`] events).
    fn route_notifies(
        &mut self,
        id: u32,
        info: &SequenceInfo,
        fired: &[usize],
        kismet_events: bool,
        out: &mut NpcOutput,
    ) -> Vec<String> {
        let mut names = Vec::new();
        for &i in fired {
            let Some(n) = info.notifies.get(i) else {
                continue;
            };
            match &n.kind {
                NotifyKind::Kismet { name } => {
                    if kismet_events {
                        out.events.push(NpcEvent::AnimNotify {
                            actor: id,
                            name: name.clone(),
                        });
                    } else {
                        names.push(name.clone());
                    }
                }
                NotifyKind::Sound {
                    cue,
                    volume,
                    pitch,
                    bone,
                    percent_to_play,
                    ..
                } => {
                    // `PercentToPlay` ≥ 1 always plays, else a draw below it.
                    if 1.0 <= *percent_to_play || self.notify_rng.frand() < *percent_to_play {
                        out.events.push(NpcEvent::AnimSound {
                            actor: id,
                            cue: cue.clone(),
                            volume: *volume,
                            pitch: *pitch,
                            bone: bone.clone(),
                        });
                    }
                }
                NotifyKind::Other { .. } => {}
            }
        }
        names
    }

    /// Ticks the skinned actors' own sequence nodes (`TickAnim`) and fires
    /// their notifies (anim nodes tick while not rendered:
    /// `bTickAnimNodesWhenNotRendered` defaults true and the
    /// `SkeletalMeshActor` template keeps it; STRONG (cdo)).
    fn tick_skinned(&mut self, scene: &NpcScene, dt: f32, out: &mut NpcOutput) {
        for i in 0..self.skinned.len().min(scene.skinned.len()) {
            let Some(def) = scene.skinned.get(i) else {
                continue;
            };
            let Some(mesh) = def.components.first().map(|c| c.mesh.as_str()) else {
                continue;
            };
            let Some(state) = self.skinned.get_mut(i) else {
                continue;
            };
            let Some(node) = state.ambient.as_mut() else {
                continue;
            };
            let Some(info) = node
                .sequence
                .as_deref()
                .and_then(|s| scene.sequence_info(mesh, s))
            else {
                continue;
            };
            let fired = node.tick(dt, true, Some(info));
            if !fired.is_empty() {
                let info = info.clone();
                self.route_notifies(def.id, &info, &fired, true, out);
            }
        }
    }

    /// The `SequenceLength` of `sequence` on skinned actor `id`'s mesh.
    #[must_use]
    pub fn anim_sequence_length(&self, scene: &NpcScene, id: u32, sequence: &str) -> Option<f32> {
        let def = scene.skinned.iter().find(|d| d.id == id)?;
        let mesh = def.components.first()?.mesh.as_str();
        scene.sequence_info(mesh, sequence).map(|i| i.length)
    }

    /// Matinee's `SetAnimPosition` on skinned actor `id`: the slot node (a
    /// plain `SkeletalMeshActor`: its own sequence node) switches to
    /// `sequence` and moves to `position`, firing notifies when `fire`.
    /// Sound notifies become events in `out`; the `NotifyName`s of the
    /// Kismet notifies fired are returned (the Kismet runtime activates them
    /// inside the Matinee update). `None` for an unknown actor.
    #[allow(clippy::too_many_arguments)]
    pub fn set_anim_position(
        &mut self,
        scene: &NpcScene,
        id: u32,
        sequence: &str,
        position: f32,
        fire: bool,
        looping: bool,
        out: &mut NpcOutput,
    ) -> Option<Vec<String>> {
        let i = scene.skinned_index(id)?;
        let def = scene.skinned.get(i)?;
        let mesh = def.components.first().map(|c| c.mesh.clone());
        // A plain `SkeletalMeshActor`'s script event drives the component's
        // own sequence node (and does nothing without one); the `...MAT`
        // classes drive a slot node of their tree (CONFIRMED (src, native);
        // `crate::anim`).
        let plain = def.class.eq_ignore_ascii_case(PLAIN_SKELETAL_ACTOR_CLASS);
        let info = mesh
            .as_deref()
            .and_then(|m| scene.sequence_info(m, sequence))
            .cloned();
        let state = self.skinned.get_mut(i)?;
        let fired = if plain {
            match state.ambient.as_mut() {
                Some(node) => node.actor_set(sequence, position, fire, looping, info.as_ref()),
                None => Vec::new(),
            }
        } else {
            let node = state.matinee.get_or_insert_with(SequenceNode::default);
            node.matinee_set(sequence, position, fire, looping, info.as_ref())
        };
        Some(match info {
            Some(info) if !fired.is_empty() => self.route_notifies(id, &info, &fired, false, out),
            _ => Vec::new(),
        })
    }

    /// `SetSkelControlStrength(control, strength)` on skinned actor `id`
    /// (Matinee); `false` for an unknown actor.
    pub fn set_skel_control_strength(
        &mut self,
        scene: &NpcScene,
        id: u32,
        control: &str,
        strength: f32,
    ) -> bool {
        let Some(state) = scene
            .skinned_index(id)
            .and_then(|i| self.skinned.get_mut(i))
        else {
            return false;
        };
        if strength.is_finite() {
            state
                .controls
                .insert(control.to_ascii_lowercase(), strength);
        }
        true
    }

    /// `SeqAct_SetLookAtTarget` on actor `id`: the stored target and the
    /// offsets; `false` for an unknown actor.
    pub fn set_look_at(
        &mut self,
        scene: &NpcScene,
        id: u32,
        target: Option<u32>,
        head_offset: Vec3,
        eyes_offset: Vec3,
    ) -> bool {
        let Some(state) = scene
            .skinned_index(id)
            .and_then(|i| self.skinned.get_mut(i))
        else {
            return false;
        };
        let finite = |v: Vec3| if v.is_finite() { v } else { Vec3::ZERO };
        state.look_at = LookAtState {
            target,
            head_offset: finite(head_offset),
            eyes_offset: finite(eyes_offset),
        };
        true
    }

    /// The strength of look-at control `control` of actor `id` (Matinee's
    /// latest, else the control's own `ControlStrength`).
    #[must_use]
    pub fn control_strength(&self, scene: &NpcScene, id: u32, control: &str) -> f32 {
        let set = scene
            .skinned_index(id)
            .and_then(|i| self.skinned.get(i))
            .and_then(|s| s.controls.get(&control.to_ascii_lowercase()).copied());
        set.unwrap_or_else(|| {
            scene
                .look_at_of(id)
                .and_then(|l| {
                    l.controls
                        .iter()
                        .find(|c| c.control.eq_ignore_ascii_case(control))
                })
                .map_or(1.0, |c| c.control_strength)
        })
    }

    /// The NPC pawns' collision cylinders where the pawns are now (the worm
    /// and Maddie stand still; villagers walk).
    #[must_use]
    pub fn pawn_cylinders(&self, scene: &NpcScene) -> Vec<(u32, NpcCylinder)> {
        let mut out = Vec::new();
        for c in &scene.pawn_collision {
            let center = scene
                .worms
                .iter()
                .find(|w| w.id == c.id)
                .map(|w| w.location)
                .or_else(|| {
                    scene
                        .villagers
                        .iter()
                        .zip(&self.villagers)
                        .find(|(d, _)| d.id == c.id)
                        .map(|(_, v)| v.location)
                })
                .or_else(|| {
                    scene
                        .maddies
                        .iter()
                        .find(|m| m.id == c.id)
                        .map(|m| m.location)
                });
            if let Some(center) = center.filter(|p| p.is_finite()) {
                out.push((
                    c.id,
                    NpcCylinder {
                        center,
                        radius: c.radius,
                        half_height: c.half_height,
                    },
                ));
            }
        }
        out
    }

    /// What the worms see of the player at `player`.
    #[must_use]
    pub fn worm_senses(scene: &NpcScene, player: Vec3) -> WormSenses {
        let mut senses = WormSenses {
            player,
            in_scream_volume: false,
            in_shadow: false,
        };
        for v in &scene.worm_volumes {
            if v.encompasses(player) {
                match v.role {
                    WormVolumeRole::Scream => senses.in_scream_volume = true,
                    WormVolumeRole::Shadow => senses.in_shadow = true,
                }
            }
        }
        senses
    }

    /// Ticks the map-placed NPC actors for one frame (after the player's
    /// input events, before its controller and pawn, like the other map
    /// actors; G-TM-2): worms, Maddie, villagers, story-item fades, glow
    /// flowers. `player` is the pawn's location at that point.
    pub fn tick_actors(&mut self, scene: &NpcScene, player: Vec3, dt: f32, out: &mut NpcOutput) {
        if !(dt.is_finite() && dt > 0.0) || !player.is_finite() {
            return;
        }
        let senses = Self::worm_senses(scene, player);
        for (w, def) in self.worms.iter_mut().zip(&scene.worms) {
            tick_worm(w, def, &senses, dt, out);
        }
        for (m, def) in self.maddies.iter_mut().zip(&scene.maddies) {
            m.tick(def, player);
        }
        for (v, def) in self.villagers.iter_mut().zip(&scene.villagers) {
            tick_villager(v, def, &scene.nav_points, player, dt);
        }
        for (s, def) in self.story_items.iter_mut().zip(&scene.story_items) {
            tick_story_item(s, def, dt);
        }
        for (f, def) in self.flowers.iter_mut().zip(&scene.flowers) {
            tick_flower(f, def, dt, out);
        }
        self.tick_skinned(scene, dt, out);
    }

    /// Touches along the player's path of the tick (`before` → `after`,
    /// cylinder `radius` × `half_height`): collectibles (pick-up) and
    /// sound-making foliage, in path order.
    pub fn update_touches(
        &mut self,
        scene: &NpcScene,
        before: Vec3,
        after: Vec3,
        radius: f32,
        half_height: f32,
        out: &mut NpcOutput,
    ) {
        if !(before.is_finite() && after.is_finite() && radius.is_finite())
            || !half_height.is_finite()
        {
            return;
        }
        // (t, order, index): collectibles first on ties (level order).
        let mut hits: Vec<(f64, u8, usize)> = Vec::new();
        if !self.time_trial {
            for (i, (state, def)) in self
                .collectibles
                .iter_mut()
                .zip(&scene.collectibles)
                .enumerate()
            {
                let entered = swept_cylinder(before, after, radius, half_height, &def.trigger);
                let now = overlaps_cylinder(after, radius, half_height, &def.trigger);
                if let Some((t, _)) = entered
                    && !state.touching
                {
                    hits.push((t, 0, i));
                }
                state.touching = now;
            }
        }
        let lo = before.min(after) - Vec3::splat(radius + FOLIAGE_TRIGGER_RADIUS * 4.0);
        let hi = before.max(after) + Vec3::splat(radius + FOLIAGE_TRIGGER_RADIUS * 4.0);
        let near = self.foliage_grid.candidates(lo, hi);
        let near_set: BTreeSet<usize> = near.iter().copied().collect();
        for i in near {
            let (Some(def), Some(touching)) =
                (scene.foliage.get(i), self.foliage_touching.get_mut(i))
            else {
                continue;
            };
            let entered = swept_cylinder(before, after, radius, half_height, &def.trigger);
            if let Some((t, _)) = entered
                && !*touching
            {
                hits.push((t, 1, i));
            }
            *touching = overlaps_cylinder(after, radius, half_height, &def.trigger);
        }
        // Foliage far from the path is no longer touched.
        for (i, t) in self.foliage_touching.iter_mut().enumerate() {
            if *t && !near_set.contains(&i) {
                *t = false;
            }
        }
        hits.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)).then(a.2.cmp(&b.2)));
        for (_, kind, i) in hits {
            if kind == 0 {
                if let (Some(state), Some(def)) =
                    (self.collectibles.get_mut(i), scene.collectibles.get(i))
                    && !state.collected
                {
                    state.collected = true;
                    out.events
                        .push(NpcEvent::CollectibleCollected { id: def.id });
                }
            } else if let Some(def) = scene.foliage.get(i) {
                out.events.push(NpcEvent::FoliageTouched { id: def.id });
            }
        }
    }

    /// Clears every touch (teleports: a respawn starts untouched).
    pub fn clear_touches(&mut self) {
        for c in &mut self.collectibles {
            c.touching = false;
        }
        self.foliage_touching.fill(false);
    }

    /// Applies a grapple handler call or story interaction (from the player
    /// simulation): glow flowers react to `Grappled`, story items to
    /// `InteractWith`.
    pub fn apply_object_event(
        &mut self,
        scene: &NpcScene,
        event: ObjectEvent,
        out: &mut NpcOutput,
    ) {
        match event {
            ObjectEvent::Grappled(id) => {
                if let Some(i) = scene.flowers.iter().position(|f| f.id == id)
                    && let Some(f) = self.flowers.get_mut(i)
                {
                    out.events.push(NpcEvent::GlowFlowerGrappled { id });
                    if f.glowing {
                        // `Glowing.Grappled`: remember and reset the glow time.
                        f.was_grappled = true;
                        if let Some(def) = scene.flowers.get(i) {
                            f.glow_time_remaining = def.glow_duration;
                        }
                    } else {
                        // `NotGlowing.Grappled` → `Glowing` (locals cleared).
                        *f = GlowFlowerState {
                            glowing: true,
                            code_pending: true,
                            alpha: f.alpha,
                            ..GlowFlowerState::default()
                        };
                    }
                }
            }
            ObjectEvent::UnGrappled(_) => {}
            ObjectEvent::InteractWith(id) => self.interact_with(scene, id, out),
        }
    }

    /// `ASAMUInteractable_Actor.InteractWith` (story-mode fire within range).
    pub fn interact_with(&mut self, scene: &NpcScene, id: u32, out: &mut NpcOutput) {
        let Some(i) = scene.story_items.iter().position(|s| s.id == id) else {
            return;
        };
        let (Some(def), Some(state)) = (scene.story_items.get(i), self.story_items.get_mut(i))
        else {
            return;
        };
        if !state.accepts(def) {
            return;
        }
        state.times = state.times.saturating_add(1);
        // `FadeOutInteractSymbol` (only `Idle` with a glow mesh fades).
        if state.state == StoryItemStateName::Idle && def.has_glow {
            state.goto(StoryItemStateName::FadingDown);
        }
        out.events
            .push(NpcEvent::ActorInteractedWith { originator: id });
        // `NotifyInteracted`.
        if def.parent {
            if let Some(s) = self.story_items.get_mut(i) {
                s.goto(StoryItemStateName::FadingDown);
            }
            self.disable_children(scene, i);
            if def.optional {
                out.events
                    .push(NpcEvent::StoryItemRegistered { item: Some(id) });
            }
        } else if let Some(parent) = def.linked_parent {
            if let Some(p) = scene.story_items.iter().position(|s| s.id == parent) {
                // `ChildToSelfWasInteracted` on the parent.
                if let Some(s) = self.story_items.get_mut(p) {
                    s.goto(StoryItemStateName::FadingDown);
                }
                if scene.story_items.get(p).is_some_and(|d| d.optional) {
                    out.events
                        .push(NpcEvent::StoryItemRegistered { item: Some(parent) });
                }
                out.events
                    .push(NpcEvent::ActorInteractedWith { originator: parent });
                self.disable_children(scene, p);
            }
        } else if def.optional {
            // Registers its (null) parent: the `<level>None` key.
            out.events
                .push(NpcEvent::StoryItemRegistered { item: None });
        }
    }

    /// `DisableChildInteractables`: uses exhausted, fade.
    fn disable_children(&mut self, scene: &NpcScene, parent: usize) {
        let list = self.children.get(parent).cloned().unwrap_or_default();
        for c in list {
            if let (Some(s), Some(d)) = (self.story_items.get_mut(c), scene.story_items.get(c)) {
                s.times = d.max_interact_times;
                s.goto(StoryItemStateName::FadingDown);
            }
        }
    }

    /// Kismet `SeqAct_StartWorm` for the worm actor `id`.
    pub fn start_worm(&mut self, scene: &NpcScene, id: u32, out: &mut NpcOutput) -> bool {
        self.worm_mut(scene, id).map(|w| w.start(id, out)).is_some()
    }

    /// Kismet `SeqAct_ShutDownWorm`.
    pub fn shut_down_worm(&mut self, scene: &NpcScene, id: u32) -> bool {
        self.worm_mut(scene, id).map(WormState::shut_down).is_some()
    }

    /// Kismet `SeqAct_PauseWorm` (`paused` = input `Pause`).
    pub fn pause_worm(&mut self, scene: &NpcScene, id: u32, paused: bool) -> bool {
        self.worm_mut(scene, id)
            .map(|w| w.set_paused(paused))
            .is_some()
    }

    /// Kismet `SeqAct_WormResetSleepTimer`.
    pub fn worm_reset_sleep_timer(&mut self, scene: &NpcScene, id: u32) -> bool {
        self.worm_mut(scene, id)
            .map(WormState::reset_sleep_timer)
            .is_some()
    }

    /// `GameInfo.NotifyKilled` reaching every NPC controller (kill-zone
    /// deaths and the worm's own kill).
    pub fn notify_player_killed(&mut self, scene: &NpcScene, out: &mut NpcOutput) {
        for (w, def) in self.worms.iter_mut().zip(&scene.worms) {
            w.notify_killed(def.id, out);
        }
    }

    fn worm_mut(&mut self, scene: &NpcScene, id: u32) -> Option<&mut WormState> {
        let i = scene.worms.iter().position(|w| w.id == id)?;
        self.worms.get_mut(i)
    }

    /// Restores a collectible's `bCollected` (save snapshot).
    pub fn set_collected(&mut self, scene: &NpcScene, id: u32, collected: bool) -> bool {
        let Some(i) = scene.collectibles.iter().position(|c| c.id == id) else {
            return false;
        };
        self.collectibles
            .get_mut(i)
            .map(|c| c.collected = collected)
            .is_some()
    }

    /// Collected collectibles of this map.
    #[must_use]
    pub fn collected_count(&self) -> usize {
        self.collectibles.iter().filter(|c| c.collected).count()
    }
}

/// One tick of a story item's state code (`FadingDown`).
fn tick_story_item(s: &mut StoryItemState, def: &StoryItemDef, dt: f32) {
    let woke = poll_sleep(&mut s.sleep, dt);
    if !woke && (s.sleep.is_some() || !s.code_pending) {
        return;
    }
    let begin = !woke;
    s.code_pending = false;
    if s.state != StoryItemStateName::FadingDown {
        return;
    }
    if begin {
        s.glow = 1.0;
        s.fade_index = 0;
    } else {
        s.fade_index = s.fade_index.saturating_add(1);
    }
    if s.fade_index <= INTERACTABLE_FADE_STEPS {
        s.glow =
            (INTERACTABLE_FADE_STEPS as f32 - s.fade_index as f32) / INTERACTABLE_FADE_STEPS as f32;
        s.sleep = Some(INTERACTABLE_FADE_SLEEP);
        s.code_pending = true;
        return;
    }
    // From state code: the new state's (empty) code runs at once.
    s.state = if s.accepts(def) {
        StoryItemStateName::Idle
    } else {
        StoryItemStateName::Disabled
    };
    s.fade_index = 0;
}

/// One tick of a glow flower's state code (`Glowing`). Labels: `Begin`
/// resets the glow time and jumps to `StartGlowing` (glow sound, fade in,
/// hold while time remains), which falls into `StopGlowing` (fade out; a
/// grapple during the fade jumps back to `StartGlowing`; at the end alpha 0
/// and `NotGlowing`). Every loop step updates first and then sleeps
/// `UPDATE_RATE` (CONFIRMED (src)).
fn tick_flower(f: &mut GlowFlowerState, def: &GlowFlowerDef, dt: f32, out: &mut NpcOutput) {
    if !f.glowing {
        return;
    }
    if f.sleep.is_some() {
        if !poll_sleep(&mut f.sleep, dt) {
            return;
        }
    } else if !f.code_pending {
        return;
    }
    let rate = GLOW_FLOWER_UPDATE_RATE;
    let fade = def.fade_time;
    let mut pc = f.pc;
    for _ in 0..16 {
        match pc {
            FlowerPc::Begin => {
                f.glow_time_remaining = def.glow_duration;
                pc = FlowerPc::Start;
            }
            FlowerPc::Start => {
                out.events.push(NpcEvent::GlowFlowerGlow { id: def.id });
                pc = FlowerPc::FadeIn;
            }
            FlowerPc::FadeIn => {
                if f.current_fade_time < fade {
                    f.alpha = finterp_to(f.current_fade_time / fade, 1.0, rate, fade);
                    f.current_fade_time += rate;
                    break;
                }
                f.alpha = 1.0;
                pc = FlowerPc::Hold;
            }
            FlowerPc::Hold => {
                if f.glow_time_remaining > 0.0 {
                    f.glow_time_remaining -= rate;
                    break;
                }
                pc = FlowerPc::Stop;
            }
            FlowerPc::Stop => {
                f.current_fade_time = 0.0;
                pc = FlowerPc::FadeOut;
            }
            FlowerPc::FadeOut => {
                if f.current_fade_time < fade {
                    if f.was_grappled {
                        f.was_grappled = false;
                        pc = FlowerPc::Start;
                        continue;
                    }
                    f.alpha = finterp_to(1.0 - f.current_fade_time / fade, 0.0, rate, fade);
                    f.current_fade_time += rate;
                    break;
                }
                // `GotoState('NotGlowing')` (its code is empty).
                f.alpha = 0.0;
                f.glowing = false;
                f.code_pending = false;
                f.sleep = None;
                f.pc = FlowerPc::Begin;
                return;
            }
        }
    }
    f.pc = pc;
    f.sleep = Some(rate);
    f.code_pending = true;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn swept_cylinder_finds_entry() {
        let c = NpcCylinder {
            center: Vec3::new(100.0, 0.0, 40.0),
            radius: 50.0,
            half_height: 40.0,
        };
        let hit = swept_cylinder(
            Vec3::new(0.0, 0.0, 44.0),
            Vec3::new(200.0, 0.0, 44.0),
            21.0,
            44.0,
            &c,
        )
        .unwrap();
        assert!((hit.0 - (100.0 - 71.0) / 200.0).abs() < 1e-9, "{hit:?}");
        assert!(
            swept_cylinder(
                Vec3::new(0.0, 500.0, 0.0),
                Vec3::new(200.0, 500.0, 0.0),
                21.0,
                44.0,
                &c
            )
            .is_none()
        );
    }

    #[test]
    fn hull_contains_points() {
        let h = NpcHull::from_box(Vec3::splat(-10.0), Vec3::splat(10.0));
        assert!(h.contains(Vec3::ZERO));
        assert!(h.contains(Vec3::splat(10.0)));
        assert!(!h.contains(Vec3::new(10.5, 0.0, 0.0)));
    }

    #[test]
    fn finterp_matches_the_stock_formula() {
        assert_eq!(finterp_to(0.0, 1.0, 0.016_667, 1.0), 0.016_667);
        assert_eq!(finterp_to(1.0, 1.0, 0.1, 1.0), 1.0);
        assert_eq!(finterp_to(0.0, 1.0, 0.1, 0.0), 1.0);
    }

    #[test]
    fn deterministic_atan2_is_accurate() {
        for i in 0..720 {
            let a = f64::from(i) * std::f64::consts::PI / 360.0 - std::f64::consts::PI + 1e-3;
            let (y, x) = (a.sin() * 3.0, a.cos() * 3.0);
            assert!((det_atan2(y, x) - y.atan2(x)).abs() < 1e-12, "{a}");
        }
        assert_eq!(det_atan2(0.0, 0.0), 0.0);
        assert_eq!(det_atan2(f64::NAN, 1.0), 0.0);
    }

    #[test]
    fn yaw_units_follow_ue3() {
        assert_eq!(yaw_units(Vec3::X), 0);
        assert_eq!(yaw_units(Vec3::Y), 16_384);
        assert_eq!(yaw_units(-Vec3::Y), 49_152);
    }
}
