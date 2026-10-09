//! Gameplay actors of converted levels and their run-time rules.
//!
//! Definitions ([`SceneActors`]) are extracted from the importer's scenes by
//! [`crate::scene::load_map`]; [`SceneRuntime`] holds their state during play.
//! Rules come from `docs/reverse-engineering/ABILITIES.md` §11 (A-CP-*,
//! A-DT-*), `GRAPPLE.md` §12 (G-WO-*) and local reading of the
//! `ASAMUCheckpoint`, `ASAMUCheckpointManager`, `ASAMUKillZone`,
//! `ASAMUDynamicKillZone`, `ASAMUFallingRock`, `ASAMUFallingWhenGrappledRock`
//! and `ASAMUFallingRockManager` scripts (behaviour described in our own
//! words below; no script text is reproduced).
//!
//! # Checkpoints (A-CP-1..5)
//!
//! All checkpoints are kept sorted by `checkpointIndex` (equal indices keep
//! level order; the original's sort only warns — TENTATIVE order). The level
//! remembers one "latest" index (none at a fresh start). The respawn lookup
//! uses that index **as a position** in the sorted list (no stored index, or
//! a negative one → the first checkpoint; a position past the end → no
//! checkpoint).
//! Activating a checkpoint (touch by the player, unless it is triggered from
//! Kismet; or Kismet `SeqAct_TriggerCheckpoint`) requires `bEnabled` and not
//! yet activated; it registers its index, which becomes the latest only if
//! the lookup currently returns nothing or a checkpoint with a smaller index
//! (and then the original saves the game: [`WorldEvent::CheckpointSaved`]).
//! At level start (no save) checkpoint 0 is registered, which normally
//! changes nothing. Spawn position: the spawn-point actor's location (or the
//! checkpoint's), plus `spawnPointOffset` rotated by the spawn actor's
//! rotation (local offset, rotate-to-spawn-actor and a spawn actor all set),
//! by the checkpoint's rotation (local offset otherwise), or unrotated.
//! Spawn rotation: the spawn actor's if `bRotatePlayerToSpawnPointRot` and it
//! exists, else the checkpoint's.
//!
//! # Touches and deaths
//!
//! The player's collision cylinder is tested against checkpoint and trigger
//! cylinders (cylinder–cylinder; the trigger cylinders are not scaled by the
//! actor's draw scale — TENTATIVE) and against the convex hulls of kill
//! zones and trigger volumes (the hull planes pushed out by the cylinder's
//! support, exact on faces and slightly generous at hull edges — TENTATIVE
//! approximation of the engine's extent checks). The test sweeps the
//! player's path of the tick, so thin volumes are not skipped; touches are
//! processed in the order the path enters them. A touch begins once and
//! ends when the overlap ends ([`WorldEvent::Touch`] /
//! [`WorldEvent::UnTouch`]). Touching an `ASAMUKillZone` or an enabled
//! `ASAMUDynamicKillZone` kills the player (A-DT-1); so does going below
//! `WorldInfo.KillZ`, raised once per descent below it (the engine would
//! raise it every tick, which during the death fade would keep restarting
//! the fade — A-DT-3 says whether KillZ is reachable is UNKNOWN; our
//! once-per-descent rule keeps the simulation from stalling).
//!
//! # Falling rocks (G-WO-3/4, CONFIRMED (src) unless noted)
//!
//! `ASAMUFallingRock` starts inactive. Activated (Kismet
//! `SeqAct_ToggleFallingRocksActive`), it runs a latent loop every
//! `updateRate` seconds: optionally interpolates its spin rate towards a
//! random target and rotates, moves down by its current fall speed ×
//! `updateRate`, then raises the speed by `AccelRate × updateRate` up to a
//! random maximum in `[fallingLowerRate, fallingHigherRate]`; after falling
//! more than `fallDistance` it either hides and stops (`respawnAtStart`
//! false) or jumps back to its start with speed 0 and new random targets.
//! Grappled while falling, it decelerates by `decelRate × updateRate` per
//! step (still moving) until the speed is at most 0.1; released, it resumes
//! the falling loop. `ASAMUFallingWhenGrappledRock` waits; the first grapple
//! makes it fall `fallDistance`: each step moves by the current speed, then
//! the speed becomes `final × timeFallen / reachMaxSpeedTime` until that time
//! (so the first step uses the full random speed and the second none), then
//! `final`. Every player death resets these rocks to their start with a new
//! random speed. Moves ignore collision here (TENTATIVE: the original moves
//! the actor with its encroachment rules). Random values come from a seeded
//! deterministic generator, not the original's random stream.

use std::collections::BTreeMap;

use glam::{DVec3, Vec3};
use serde::{Deserialize, Serialize};

use crate::collision::{CollisionScene, Instance};
use crate::objects::{
    ATTRACTOR_DEFAULT_DURATION, ATTRACTOR_DEFAULT_RANGE, ATTRACTOR_DEFAULT_STRENGTH, Attractor,
    CRYSTAL_DEFAULT_RECHARGE_DELAY, GlowFlower, INTERACTABLE_DEFAULT_MAX_INTERACT_TIMES,
    Interactable, LATENT_WAKE_FRACTION, RechargeCrystal, WorldEvent,
};
use crate::rotation;
use crate::scene::{
    self, DynamicBody, SceneFile, actor_id, object_name, param_bool, param_f32, param_i64,
    param_str, param_vec3,
};

/// The player start.
#[derive(Clone, Debug, PartialEq)]
pub struct PlayerStartDef {
    /// Actor id.
    pub id: u32,
    /// Object name.
    pub name: String,
    /// `Location` (where the pawn's centre spawns).
    pub location: Vec3,
    /// `Rotation`.
    pub rotation: [i32; 3],
    /// `bEnabled`.
    pub enabled: bool,
    /// `bPrimaryStart`.
    pub primary: bool,
    /// Half-height of its cylinder (the floor is about this far below).
    pub half_height: f32,
}

/// An `ASAMUCheckpoint`.
#[derive(Clone, Debug, PartialEq)]
pub struct CheckpointDef {
    /// Actor id.
    pub id: u32,
    /// Object name.
    pub name: String,
    /// `checkpointIndex`.
    pub index: i32,
    /// `bEnabled` at level start.
    pub enabled: bool,
    /// `bTriggeredFromKismet` (no touch collision).
    pub triggered_from_kismet: bool,
    /// `Location`.
    pub location: Vec3,
    /// `Rotation`.
    pub rotation: [i32; 3],
    /// Cylinder radius (`CollisionRadius`).
    pub radius: f32,
    /// Cylinder half-height (`CollisionHeight`).
    pub half_height: f32,
    /// Respawn position (A-CP-4).
    pub spawn_location: Vec3,
    /// Respawn rotation (A-CP-4).
    pub spawn_rotation: [i32; 3],
}

/// What a touch volume does.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VolumeKind {
    /// `ASAMUKillZone`: touching kills.
    KillZone,
    /// `ASAMUDynamicKillZone`: touching kills while enabled (Kismet toggle).
    DynamicKillZone,
    /// `TriggerVolume` and subclasses: touch events for Kismet.
    TriggerVolume,
}

/// A convex hull for touch tests.
#[derive(Clone, Debug, PartialEq)]
pub struct Hull {
    /// Outward unit normals and offsets (`n · p = w` on the plane).
    pub planes: Vec<(DVec3, f64)>,
    /// Bounds.
    pub min: Vec3,
    /// Bounds.
    pub max: Vec3,
}

/// A brush volume that reports touches.
#[derive(Clone, Debug, PartialEq)]
pub struct TouchVolumeDef {
    /// Actor id.
    pub id: u32,
    /// Object name.
    pub name: String,
    /// Class.
    pub class: String,
    /// Role.
    pub kind: VolumeKind,
    /// Collision hulls.
    pub hulls: Vec<Hull>,
    /// Collides at level start.
    pub enabled: bool,
}

/// A `Trigger` (cylinder).
#[derive(Clone, Debug, PartialEq)]
pub struct TriggerDef {
    /// Actor id.
    pub id: u32,
    /// Object name.
    pub name: String,
    /// Centre.
    pub location: Vec3,
    /// `CollisionRadius`.
    pub radius: f32,
    /// `CollisionHeight`.
    pub half_height: f32,
    /// Collides at level start.
    pub enabled: bool,
}

/// Falling-rock class.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RockKind {
    /// `ASAMUFallingRock`.
    Falling,
    /// `ASAMUFallingWhenGrappledRock`.
    FallingWhenGrappled,
}

/// Instance values of a falling rock (class defaults in `DEFAULTS.md` §7).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RockParams {
    /// `respawnAtStart`.
    pub respawn_at_start: bool,
    /// `fallingLowerRate`.
    pub falling_lower_rate: f32,
    /// `fallingHigherRate`.
    pub falling_higher_rate: f32,
    /// `fallDistance`.
    pub fall_distance: f32,
    /// `AccelRate`.
    pub accel_rate: f32,
    /// `decelRate`.
    pub decel_rate: f32,
    /// `updateRate` (latent step, s).
    pub update_rate: f32,
    /// `bShouldRotate`.
    pub should_rotate: bool,
    /// `rotationLowerRate` (X → roll, Y → yaw, Z → pitch).
    pub rotation_lower_rate: Vec3,
    /// `rotationHigherRate`.
    pub rotation_higher_rate: Vec3,
    /// `RotationAccelRate`.
    pub rotation_accel_rate: f32,
    /// `reachMaxSpeedTime` (falling-when-grappled rocks).
    pub reach_max_speed_time: f32,
}

/// A falling rock.
#[derive(Clone, Debug, PartialEq)]
pub struct RockDef {
    /// Actor id.
    pub id: u32,
    /// Object name.
    pub name: String,
    /// Class.
    pub kind: RockKind,
    /// Start location.
    pub location: Vec3,
    /// Start rotation.
    pub rotation: [i32; 3],
    /// Values.
    pub params: RockParams,
    /// Index into `LoadedMap::bodies` (its collision), if any.
    pub body: Option<usize>,
}

/// The gameplay actors of a loaded map (every level).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SceneActors {
    /// Player starts.
    pub player_starts: Vec<PlayerStartDef>,
    /// Checkpoints (level order).
    pub checkpoints: Vec<CheckpointDef>,
    /// Kill zones and trigger volumes.
    pub volumes: Vec<TouchVolumeDef>,
    /// Triggers.
    pub triggers: Vec<TriggerDef>,
    /// Recharge crystals.
    pub crystals: Vec<RechargeCrystal>,
    /// Glow flowers.
    pub flowers: Vec<GlowFlower>,
    /// Story-mode interactables.
    pub interactables: Vec<Interactable>,
    /// Attractor pads.
    pub attractors: Vec<Attractor>,
    /// Falling rocks.
    pub rocks: Vec<RockDef>,
    /// `InterpActor`s (static for now; Matinee is not imported).
    pub movers: usize,
    /// `ASAMUCollectible`s (not modelled yet).
    pub collectibles: usize,
}

impl SceneActors {
    /// The start the game uses: the first enabled primary start, else the
    /// first enabled one, else the first.
    #[must_use]
    pub fn player_start(&self) -> Option<&PlayerStartDef> {
        self.player_starts
            .iter()
            .find(|p| p.enabled && p.primary)
            .or_else(|| self.player_starts.iter().find(|p| p.enabled))
            .or_else(|| self.player_starts.first())
    }
}

/// World-space hull planes, outward (from the importer's planes, oriented
/// away from the vertex centroid; recomputed from the triangles when the
/// importer left them out).
fn hull_from_json(h: &scene::HullJson, offset: Vec3) -> Option<Hull> {
    let verts: Vec<DVec3> = h
        .vertices
        .iter()
        .map(|v| (Vec3::from_array(*v) + offset).as_dvec3())
        .filter(|v| v.is_finite())
        .collect();
    if verts.len() < 4 {
        return None;
    }
    // Orientation reference: the centroid of the vertices the triangles use
    // (an unused stray vertex must not flip the planes), else of all.
    let used: Vec<DVec3> = {
        let mut seen = vec![false; h.vertices.len()];
        for t in &h.triangles {
            if t.iter().all(|&i| (i as usize) < h.vertices.len()) {
                for &i in t {
                    if let Some(s) = seen.get_mut(i as usize) {
                        *s = true;
                    }
                }
            }
        }
        h.vertices
            .iter()
            .zip(&seen)
            .filter(|(_, s)| **s)
            .map(|(v, _)| (Vec3::from_array(*v) + offset).as_dvec3())
            .filter(|v| v.is_finite())
            .collect()
    };
    let reference = if used.len() >= 4 { &used } else { &verts };
    let centroid = reference.iter().copied().sum::<DVec3>() / reference.len() as f64;
    let mut planes = Vec::new();
    let off = offset.as_dvec3();
    for p in &h.planes {
        let n = DVec3::new(f64::from(p[0]), f64::from(p[1]), f64::from(p[2]));
        let len = n.length();
        if !(len.is_finite() && len > 0.0 && p[3].is_finite()) {
            continue;
        }
        let n = n / len;
        let w = f64::from(p[3]) / len + n.dot(off);
        planes.push(if n.dot(centroid) > w {
            (-n, -w)
        } else {
            (n, w)
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
            let tri = [a, b, c].map(|v| (Vec3::from_array(*v) + offset).as_dvec3());
            if let Some(n) = crate::collision::cylinder::triangle_normal(&tri) {
                let n = if n.dot(tri[0] - centroid) < 0.0 {
                    -n
                } else {
                    n
                };
                planes.push((n, n.dot(tri[0])));
            }
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
    let (min, max) = crate::collision::affine::round_out(min, max);
    Some(Hull { planes, min, max })
}

/// The cylinder component of an actor (the one its `CylinderComponent`
/// parameter names, else the first).
fn cylinder_of(actor: &scene::ActorJson) -> Option<[f32; 2]> {
    let wanted = param_str(&actor.params, "CylinderComponent").map(object_name);
    actor
        .components
        .iter()
        .filter(|c| c.kind == "cylinder")
        .find(|c| wanted.is_none_or(|w| c.name.eq_ignore_ascii_case(w)))
        .or_else(|| actor.components.iter().find(|c| c.kind == "cylinder"))
        .and_then(|c| c.cylinder)
        .filter(|c| c[0].is_finite() && c[1].is_finite() && c[0] >= 0.0 && c[1] >= 0.0)
}

/// Gathers the gameplay actors of one level.
pub(crate) fn collect_actors(
    out: &mut SceneActors,
    scene: &SceneFile,
    level: u8,
    offset: Vec3,
    warnings: &mut Vec<String>,
) {
    // Name → (location, rotation) for spawn-point and parent references.
    let by_name: BTreeMap<String, (Vec3, [i32; 3], usize)> = scene
        .actors
        .iter()
        .map(|a| {
            (
                a.name.to_ascii_lowercase(),
                (Vec3::from_array(a.location) + offset, a.rotation, a.slot),
            )
        })
        .collect();
    let lookup = |path: Option<&str>| {
        let path = path?;
        by_name
            .get(&object_name(path).to_ascii_lowercase())
            .copied()
    };
    for a in &scene.actors {
        let Some(id) = actor_id(level, a.slot) else {
            continue;
        };
        let location = Vec3::from_array(a.location) + offset;
        if !location.is_finite() {
            warnings.push(format!("{}: non-finite location", a.name));
            continue;
        }
        let p = &a.params;
        match a.kind.as_str() {
            "player_start" => out.player_starts.push(PlayerStartDef {
                id,
                name: a.name.clone(),
                location,
                rotation: a.rotation,
                enabled: param_bool(p, "bEnabled", true),
                primary: param_bool(p, "bPrimaryStart", true),
                half_height: cylinder_of(a).map_or(0.0, |c| c[1]),
            }),
            "checkpoint" => {
                let Some([radius, half_height]) = cylinder_of(a) else {
                    warnings.push(format!("{}: checkpoint without a cylinder", a.name));
                    continue;
                };
                let spawn_actor = lookup(param_str(p, "spawnPointActor"));
                let rotate = param_bool(p, "bRotatePlayerToSpawnPointRot", true);
                let local = param_bool(p, "bOffsetLocalSpace", true);
                let offset_v = param_vec3(p, "spawnPointOffset")
                    .unwrap_or(Vec3::ZERO)
                    .as_dvec3();
                let base = spawn_actor.map_or(location, |s| s.0);
                let rotated = if local {
                    match spawn_actor {
                        Some(s) if rotate => rotation::rotate_vector(offset_v, s.1),
                        _ => rotation::rotate_vector(offset_v, a.rotation),
                    }
                } else {
                    offset_v
                };
                let spawn_rotation = match (rotate, spawn_actor) {
                    (true, Some(s)) => s.1,
                    _ => a.rotation,
                };
                if rotate && spawn_actor.is_none() {
                    warnings.push(format!(
                        "{}: rotate to spawn point without a spawn point actor (checkpoint rotation used)",
                        a.name
                    ));
                }
                out.checkpoints.push(CheckpointDef {
                    id,
                    name: a.name.clone(),
                    index: i32::try_from(param_i64(p, "checkpointIndex", 0)).unwrap_or(0),
                    enabled: param_bool(p, "bEnabled", true),
                    triggered_from_kismet: param_bool(p, "bTriggeredFromKismet", false),
                    location,
                    rotation: a.rotation,
                    radius,
                    half_height,
                    spawn_location: (base.as_dvec3() + rotated).as_vec3(),
                    spawn_rotation,
                });
            }
            "kill_zone" | "dynamic_kill_zone" | "trigger_volume" => {
                let kind = match a.kind.as_str() {
                    "kill_zone" => VolumeKind::KillZone,
                    "dynamic_kill_zone" => VolumeKind::DynamicKillZone,
                    _ => VolumeKind::TriggerVolume,
                };
                let hulls: Vec<Hull> = a
                    .volume
                    .as_ref()
                    .map(|v| {
                        v.hulls
                            .iter()
                            .filter_map(|h| hull_from_json(h, offset))
                            .collect()
                    })
                    .unwrap_or_default();
                if hulls.is_empty() {
                    warnings.push(format!("{}: touch volume without usable hulls", a.name));
                    continue;
                }
                let brush_collides = a
                    .components
                    .iter()
                    .find(|c| c.kind == "brush")
                    .is_none_or(|c| c.collide_actors);
                out.volumes.push(TouchVolumeDef {
                    id,
                    name: a.name.clone(),
                    class: a.class.clone(),
                    kind,
                    hulls,
                    enabled: a.collide_actors && brush_collides,
                });
            }
            "trigger" => {
                if let Some([radius, half_height]) = cylinder_of(a) {
                    out.triggers.push(TriggerDef {
                        id,
                        name: a.name.clone(),
                        location,
                        radius,
                        half_height,
                        enabled: a.collide_actors,
                    });
                }
            }
            "recharge_crystal" => {
                let parent =
                    lookup(param_str(p, "linkedParentCrystal")).and_then(|x| actor_id(level, x.2));
                out.crystals.push(RechargeCrystal {
                    id,
                    center: location,
                    half_extent: 1.0,
                    recharge_delay: param_f32(p, "RechargeDelay", CRYSTAL_DEFAULT_RECHARGE_DELAY)
                        .max(0.0),
                    should_recharge: param_bool(p, "bShouldRecharge", true),
                    parent_crystal: param_bool(p, "bParentCrystal", false),
                    linked_parent: parent,
                });
            }
            "tele_pad_attractor" => {
                let duration = param_f32(p, "attractDuration", ATTRACTOR_DEFAULT_DURATION);
                out.attractors.push(Attractor {
                    id,
                    position: location,
                    attract_duration: if duration > 0.0 {
                        duration
                    } else {
                        ATTRACTOR_DEFAULT_DURATION
                    },
                    range: param_f32(p, "Range", ATTRACTOR_DEFAULT_RANGE),
                    strength: param_f32(p, "Strength", ATTRACTOR_DEFAULT_STRENGTH),
                    velocity_base_amount: param_f32(p, "velocityBaseAmount", 0.0),
                });
            }
            "falling_rock" | "falling_when_grappled_rock" => {
                let kind = if a.kind == "falling_rock" {
                    RockKind::Falling
                } else {
                    RockKind::FallingWhenGrappled
                };
                // Class defaults from DEFAULTS.md §7 where an instance omits a value.
                let params = RockParams {
                    respawn_at_start: param_bool(p, "respawnAtStart", true),
                    falling_lower_rate: param_f32(p, "fallingLowerRate", 9.82),
                    falling_higher_rate: param_f32(p, "fallingHigherRate", 9.82),
                    fall_distance: param_f32(p, "fallDistance", 1000.0),
                    accel_rate: param_f32(p, "AccelRate", 700.0),
                    decel_rate: param_f32(p, "decelRate", 1000.0),
                    update_rate: param_f32(p, "updateRate", 0.017),
                    should_rotate: param_bool(p, "bShouldRotate", false),
                    rotation_lower_rate: param_vec3(p, "rotationLowerRate")
                        .unwrap_or(Vec3::splat(-10_000.0)),
                    rotation_higher_rate: param_vec3(p, "rotationHigherRate")
                        .unwrap_or(Vec3::splat(10_000.0)),
                    rotation_accel_rate: param_f32(p, "RotationAccelRate", 1.0),
                    reach_max_speed_time: param_f32(p, "reachMaxSpeedTime", 0.0),
                };
                out.rocks.push(RockDef {
                    id,
                    name: a.name.clone(),
                    kind,
                    location,
                    rotation: a.rotation,
                    params,
                    body: None,
                });
            }
            "interp_actor" => out.movers += 1,
            "collectible" => out.collectibles += 1,
            _ if a.class.eq_ignore_ascii_case("asamu.ASAMUGlowFlower") => {
                out.flowers.push(GlowFlower {
                    id,
                    center: location,
                    half_extent: 1.0,
                })
            }
            _ if a
                .class
                .eq_ignore_ascii_case("asamu.ASAMUInteractable_Actor") =>
            {
                let max = i32::try_from(param_i64(
                    p,
                    "MaxInteractTimes",
                    i64::from(INTERACTABLE_DEFAULT_MAX_INTERACT_TIMES),
                ))
                .unwrap_or(INTERACTABLE_DEFAULT_MAX_INTERACT_TIMES);
                out.interactables.push(Interactable {
                    id,
                    min: location - Vec3::ONE,
                    max: location + Vec3::ONE,
                    max_interact_times: max,
                    label: a.name.clone(),
                });
            }
            _ => {}
        }
    }
}

/// Fills in sizes from the collision geometry and links rocks to bodies.
pub(crate) fn finish_actors(
    out: &mut SceneActors,
    collision: &CollisionScene,
    dynamic: &[Instance],
    bodies: &[DynamicBody],
) {
    let mut bounds: BTreeMap<u32, (Vec3, Vec3)> = BTreeMap::new();
    for inst in collision.statics().iter().chain(dynamic) {
        if let Some(id) = inst.info.actor {
            let e = bounds
                .entry(id)
                .or_insert((inst.bounds.min, inst.bounds.max));
            e.0 = e.0.min(inst.bounds.min);
            e.1 = e.1.max(inst.bounds.max);
        }
    }
    let half = |id: u32, center: Vec3| {
        bounds.get(&id).map_or(32.0, |(lo, hi)| {
            let h = (*hi - center).max(center - *lo).max_element();
            if h.is_finite() && h > 0.0 { h } else { 32.0 }
        })
    };
    for c in &mut out.crystals {
        c.half_extent = half(c.id, c.center);
    }
    for f in &mut out.flowers {
        f.half_extent = half(f.id, f.center);
    }
    for i in &mut out.interactables {
        if let Some((lo, hi)) = bounds.get(&i.id)
            && lo.cmplt(*hi).all()
        {
            i.min = *lo;
            i.max = *hi;
        }
    }
    let body_of: BTreeMap<u32, usize> = bodies
        .iter()
        .enumerate()
        .rev()
        .map(|(i, b)| (b.actor, i))
        .collect();
    for r in &mut out.rocks {
        r.body = body_of.get(&r.id).copied();
    }
}

// ---------------------------------------------------------------------------
// Run time.
// ---------------------------------------------------------------------------

/// Why the player died.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeathCause {
    /// Touched an `ASAMUKillZone`.
    KillZone,
    /// Touched an enabled `ASAMUDynamicKillZone`.
    DynamicKillZone,
    /// Went below `WorldInfo.KillZ`.
    KillZ,
    /// Kismet `SeqAct_PlayerDied`, the `PlayerDied` exec, restart from
    /// checkpoint, or a test.
    Scripted,
}

/// The checkpoint list of a level (A-CP-1..4; see the module docs).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct CheckpointManager {
    /// Indices into the checkpoint definitions, sorted by `checkpointIndex`.
    pub sorted: Vec<usize>,
    /// The level's latest registered `checkpointIndex` (`None` = −1,
    /// nothing stored).
    pub latest: Option<i32>,
    /// Per definition: activated.
    pub activated: Vec<bool>,
    /// Per definition: `bEnabled`.
    pub enabled: Vec<bool>,
}

impl CheckpointManager {
    /// The manager for `defs` at level start.
    #[must_use]
    pub fn new(defs: &[CheckpointDef]) -> Self {
        let mut sorted: Vec<usize> = (0..defs.len()).collect();
        sorted.sort_by_key(|&i| (defs.get(i).map_or(0, |d| d.index), i));
        Self {
            sorted,
            latest: None,
            activated: vec![false; defs.len()],
            enabled: defs.iter().map(|d| d.enabled).collect(),
        }
    }

    /// The checkpoint the respawn uses (A-CP-4), as a definition index.
    #[must_use]
    pub fn lookup(&self) -> Option<usize> {
        // Only a stored value above −1 is used as a position; −1 (nothing
        // stored) and any lower value (a registered negative
        // `checkpointIndex`) give the first checkpoint, as in the original.
        match self.latest.and_then(|i| usize::try_from(i).ok()) {
            None => self.sorted.first().copied(),
            Some(i) => self.sorted.get(i).copied(),
        }
    }

    /// `RegisterCompletedCheckpoint`: returns `true` when `index` became the
    /// latest (the original saves the game then).
    pub fn register(&mut self, defs: &[CheckpointDef], index: i32) -> bool {
        let current = self.lookup().and_then(|i| defs.get(i));
        if current.is_none_or(|c| c.index < index) {
            self.latest = Some(index);
            true
        } else {
            false
        }
    }

    /// `ActivateCheckpoint` of definition `def` (touch or Kismet).
    pub fn activate(
        &mut self,
        defs: &[CheckpointDef],
        def: usize,
        events: &mut Vec<WorldEvent>,
    ) -> bool {
        let Some(d) = defs.get(def) else {
            return false;
        };
        let ready = !self.activated.get(def).copied().unwrap_or(true)
            && self.enabled.get(def).copied().unwrap_or(false);
        if !ready {
            return false;
        }
        if let Some(a) = self.activated.get_mut(def) {
            *a = true;
        }
        events.push(WorldEvent::CheckpointActivated {
            id: d.id,
            index: d.index,
        });
        if self.register(defs, d.index) {
            events.push(WorldEvent::CheckpointSaved { index: d.index });
        }
        true
    }

    /// Respawn position and rotation (`None` when the lookup finds no
    /// checkpoint).
    #[must_use]
    pub fn respawn(&self, defs: &[CheckpointDef]) -> Option<(Vec3, [i32; 3], u32)> {
        let d = defs.get(self.lookup()?)?;
        Some((d.spawn_location, d.spawn_rotation, d.id))
    }
}

/// Named latent states of the rocks.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RockStateName {
    /// `ASAMUFallingRock.InActive` (auto state).
    #[default]
    Inactive,
    /// `ASAMUFallingRock.isUnGrappled` (falling loop).
    Falling,
    /// `ASAMUFallingRock.isGrappled` (decelerating).
    Grappled,
    /// `ASAMUFallingWhenGrappledRock.Idle` (auto state).
    Idle,
    /// `ASAMUFallingWhenGrappledRock.HasBeenGrappled` (one-way fall).
    Dropping,
}

/// Run-time state of one rock.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct RockState {
    /// Current state.
    pub state: RockStateName,
    /// Current location.
    pub location: Vec3,
    /// Current rotation.
    pub rotation: [i32; 3],
    /// `bHidden` (still collides, as in UE3).
    pub hidden: bool,
    /// `currentFallRate`.
    pub fall_rate: f32,
    /// `currentMaxFallRate` (falling rocks).
    pub max_fall_rate: f32,
    /// `currentRotationRate` (pitch, yaw, roll).
    pub rotation_rate: [i32; 3],
    /// `currentDesiredRotationRate`.
    pub desired_rotation_rate: [i32; 3],
    /// State code restarts at `Begin` at the next run.
    pub begin_pending: bool,
    /// Remaining latent sleep.
    pub sleep: Option<f32>,
    /// State-local `finalFallRate` (falling-when-grappled rocks).
    pub final_fall_rate: f32,
    /// State-local `timeFallen`.
    pub time_fallen: f32,
}

/// SplitMix64 (deterministic; see the module docs).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rng(pub u64);

impl Rng {
    fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// `FRand()` stand-in: `[0, 1)`.
    pub fn frand(&mut self) -> f32 {
        (self.next_u64() >> 40) as f32 / (1u64 << 24) as f32
    }

    /// `RandRange(lo, hi)` = `lo + (hi − lo) · FRand()` (`lo` when that is
    /// not finite, e.g. for hostile bounds near `f32::MAX`).
    pub fn range(&mut self, lo: f32, hi: f32) -> f32 {
        let v = lo + (hi - lo) * self.frand();
        if v.is_finite() { v } else { lo }
    }
}

/// G-TM-3 latent sleep poll.
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

/// UnrealScript `rotator * float` (components truncated toward zero).
fn rot_scale(r: [i32; 3], s: f32) -> [i32; 3] {
    r.map(|c| {
        let v = c as f32 * s;
        if v.is_finite() { v as i32 } else { 0 }
    })
}

/// `RInterpTo(current, target, dt, speed)` (UE3 stock behaviour, TENTATIVE).
fn rinterp_to(current: [i32; 3], target: [i32; 3], dt: f32, speed: f32) -> [i32; 3] {
    if dt == 0.0 || current == target {
        return current;
    }
    if speed <= 0.0 {
        return target;
    }
    let alpha = (dt * speed).clamp(0.0, 1.0);
    let delta = [0, 1, 2].map(|k| rotation::normalize_axis(target[k].wrapping_sub(current[k])));
    if delta == [0, 0, 0] {
        return target;
    }
    let step = rot_scale(delta, alpha);
    [0, 1, 2].map(|k| rotation::normalize_axis(current[k].wrapping_add(step[k])))
}

/// Touch-volume state of the scene.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SceneRuntime {
    /// Checkpoints.
    pub checkpoints: CheckpointManager,
    /// Per checkpoint: the player touches it.
    pub checkpoint_touching: Vec<bool>,
    /// Per volume: touching / enabled.
    pub volume_touching: Vec<bool>,
    /// Per volume: collision on.
    pub volume_enabled: Vec<bool>,
    /// Per trigger: touching.
    pub trigger_touching: Vec<bool>,
    /// Per trigger: collision on.
    pub trigger_enabled: Vec<bool>,
    /// The player was below `KillZ` at the last update.
    pub below_kill_z: bool,
    /// Rocks.
    pub rocks: Vec<RockState>,
    /// Loaded levels (bit per level index).
    pub level_mask: u64,
    /// Random stream for the rocks.
    pub rng: Rng,
}

/// A touch found along the player's path.
#[derive(Clone, Copy, Debug, PartialEq)]
struct TouchHit {
    t: f64,
    order: u8,
    index: usize,
}

/// Interval of `t ∈ [0, 1]` where the swept cylinder `p(t) = a + t·(b − a)`
/// (radius `r`, half-height `h`) overlaps the cylinder `(c, rc, hc)`
/// (touching excluded); `None` if empty.
fn cylinder_interval(
    a: DVec3,
    b: DVec3,
    r: f64,
    h: f64,
    c: DVec3,
    rc: f64,
    hc: f64,
) -> Option<(f64, f64)> {
    let d = b - a;
    let (mut lo, mut hi) = (0.0f64, 1.0f64);
    // Vertical: |a.z + t·d.z − c.z| < h + hc.
    let hz = h + hc;
    let z0 = a.z - c.z;
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
    // Horizontal: |p0 + t·v| < r + rc.
    let rr = r + rc;
    let p0 = glam::DVec2::new(a.x - c.x, a.y - c.y);
    let v = glam::DVec2::new(d.x, d.y);
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
    (lo < hi).then_some((lo, hi))
}

/// Interval of `t ∈ [0, 1]` where the swept cylinder overlaps the hull
/// (planes pushed out by the cylinder's support).
fn hull_interval(a: DVec3, b: DVec3, r: f64, h: f64, hull: &Hull) -> Option<(f64, f64)> {
    let d = b - a;
    let lo_box = a.min(b) - DVec3::new(r, r, h);
    let hi_box = a.max(b) + DVec3::new(r, r, h);
    if lo_box.cmpgt(hull.max.as_dvec3()).any() || hi_box.cmplt(hull.min.as_dvec3()).any() {
        return None;
    }
    let (mut lo, mut hi) = (0.0f64, 1.0f64);
    for (n, w) in &hull.planes {
        let e = r * (n.x * n.x + n.y * n.y).sqrt() + h * n.z.abs();
        // Inside the pushed-out plane: n·p(t) − e < w.
        let f0 = n.dot(a) - e - w;
        let f1 = n.dot(d);
        if f1 == 0.0 {
            if f0 >= 0.0 {
                return None;
            }
            continue;
        }
        let t = -f0 / f1;
        if f1 > 0.0 {
            hi = hi.min(t);
        } else {
            lo = lo.max(t);
        }
        if lo >= hi {
            return None;
        }
    }
    (lo < hi).then_some((lo, hi))
}

/// What a touch update found.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TouchOutcome {
    /// The first death of the path, with the killing actor (`None` for KillZ).
    pub death: Option<(DeathCause, Option<u32>)>,
}

impl SceneRuntime {
    /// The runtime at level start (A-CP-5: checkpoint 0 registered).
    #[must_use]
    pub fn new(actors: &SceneActors, level_mask: u64, seed: u64) -> Self {
        let mut checkpoints = CheckpointManager::new(&actors.checkpoints);
        checkpoints.register(&actors.checkpoints, 0);
        let mut rng = Rng(seed);
        let rocks = actors
            .rocks
            .iter()
            .map(|r| {
                // PostBeginPlay: start position, initial speeds and spin.
                let mut s = RockState {
                    location: r.location,
                    rotation: r.rotation,
                    ..RockState::default()
                };
                match r.kind {
                    RockKind::Falling => {
                        s.state = RockStateName::Inactive;
                        s.max_fall_rate =
                            rng.range(r.params.falling_lower_rate, r.params.falling_higher_rate);
                        s.desired_rotation_rate = desired_spin(&mut rng, &r.params);
                    }
                    RockKind::FallingWhenGrappled => {
                        s.state = RockStateName::Idle;
                        s.fall_rate =
                            rng.range(r.params.falling_lower_rate, r.params.falling_higher_rate);
                    }
                }
                s
            })
            .collect();
        Self {
            checkpoints,
            checkpoint_touching: vec![false; actors.checkpoints.len()],
            volume_touching: vec![false; actors.volumes.len()],
            volume_enabled: actors.volumes.iter().map(|v| v.enabled).collect(),
            trigger_touching: vec![false; actors.triggers.len()],
            trigger_enabled: actors.triggers.iter().map(|t| t.enabled).collect(),
            below_kill_z: false,
            rocks,
            level_mask,
            rng,
        }
    }

    fn level_loaded(&self, id: u32) -> bool {
        let level = id >> 16;
        level < 64 && self.level_mask & (1u64 << level) != 0
    }

    /// Updates touches for the player's path `from → to` (cylinder `r`, `h`)
    /// and `KillZ`; emits events; returns the first death.
    #[allow(clippy::too_many_arguments)]
    pub fn update_touches(
        &mut self,
        actors: &SceneActors,
        kill_z: f32,
        from: Vec3,
        to: Vec3,
        radius: f32,
        half_height: f32,
        events: &mut Vec<WorldEvent>,
    ) -> TouchOutcome {
        let a = from.as_dvec3();
        let b = to.as_dvec3();
        let (r, h) = (f64::from(radius), f64::from(half_height));
        let mut outcome = TouchOutcome::default();
        if !(a.is_finite() && b.is_finite()) {
            return outcome;
        }
        let mut entered: Vec<TouchHit> = Vec::new();
        // Checkpoints (order 0), volumes (1), triggers (2).
        for (i, c) in actors.checkpoints.iter().enumerate() {
            let collides = !c.triggered_from_kismet && self.level_loaded(c.id);
            let span = if collides {
                cylinder_interval(
                    a,
                    b,
                    r,
                    h,
                    c.location.as_dvec3(),
                    f64::from(c.radius),
                    f64::from(c.half_height),
                )
            } else {
                None
            };
            let was = self.checkpoint_touching.get(i).copied().unwrap_or(false);
            let now = span.is_some_and(|s| s.1 >= 1.0);
            if let Some(s) = span
                && !was
            {
                entered.push(TouchHit {
                    t: s.0,
                    order: 0,
                    index: i,
                });
            }
            if let Some(f) = self.checkpoint_touching.get_mut(i) {
                *f = now;
            }
        }
        for (i, v) in actors.volumes.iter().enumerate() {
            let enabled =
                self.volume_enabled.get(i).copied().unwrap_or(false) && self.level_loaded(v.id);
            let span = if enabled {
                v.hulls
                    .iter()
                    .filter_map(|hull| hull_interval(a, b, r, h, hull))
                    .fold(None, |acc: Option<(f64, f64)>, s| {
                        Some(acc.map_or(s, |x| (x.0.min(s.0), x.1.max(s.1))))
                    })
            } else {
                None
            };
            let was = self.volume_touching.get(i).copied().unwrap_or(false);
            let now = span.is_some_and(|s| s.1 >= 1.0);
            if let Some(s) = span
                && !was
            {
                entered.push(TouchHit {
                    t: s.0,
                    order: 1,
                    index: i,
                });
            }
            if was && !now {
                events.push(WorldEvent::UnTouch { id: v.id });
            }
            if let Some(f) = self.volume_touching.get_mut(i) {
                *f = now;
            }
        }
        for (i, tr) in actors.triggers.iter().enumerate() {
            let enabled =
                self.trigger_enabled.get(i).copied().unwrap_or(false) && self.level_loaded(tr.id);
            let span = if enabled {
                cylinder_interval(
                    a,
                    b,
                    r,
                    h,
                    tr.location.as_dvec3(),
                    f64::from(tr.radius),
                    f64::from(tr.half_height),
                )
            } else {
                None
            };
            let was = self.trigger_touching.get(i).copied().unwrap_or(false);
            let now = span.is_some_and(|s| s.1 >= 1.0);
            if let Some(s) = span
                && !was
            {
                entered.push(TouchHit {
                    t: s.0,
                    order: 2,
                    index: i,
                });
            }
            if was && !now {
                events.push(WorldEvent::UnTouch { id: tr.id });
            }
            if let Some(f) = self.trigger_touching.get_mut(i) {
                *f = now;
            }
        }
        entered.sort_by(|x, y| {
            x.t.total_cmp(&y.t)
                .then(x.order.cmp(&y.order))
                .then(x.index.cmp(&y.index))
        });
        for hit in entered {
            match hit.order {
                0 => {
                    self.checkpoints
                        .activate(&actors.checkpoints, hit.index, events);
                }
                1 => {
                    let Some(v) = actors.volumes.get(hit.index) else {
                        continue;
                    };
                    events.push(WorldEvent::Touch { id: v.id });
                    let cause = match v.kind {
                        VolumeKind::KillZone => Some(DeathCause::KillZone),
                        VolumeKind::DynamicKillZone => Some(DeathCause::DynamicKillZone),
                        VolumeKind::TriggerVolume => None,
                    };
                    if let Some(c) = cause
                        && outcome.death.is_none()
                    {
                        outcome.death = Some((c, Some(v.id)));
                    }
                    if !self
                        .volume_touching
                        .get(hit.index)
                        .copied()
                        .unwrap_or(false)
                    {
                        // Passed through within the tick.
                        events.push(WorldEvent::UnTouch { id: v.id });
                    }
                }
                _ => {
                    let Some(t) = actors.triggers.get(hit.index) else {
                        continue;
                    };
                    events.push(WorldEvent::Touch { id: t.id });
                    if !self
                        .trigger_touching
                        .get(hit.index)
                        .copied()
                        .unwrap_or(false)
                    {
                        events.push(WorldEvent::UnTouch { id: t.id });
                    }
                }
            }
        }
        let below = to.z < kill_z;
        if below && !self.below_kill_z && outcome.death.is_none() {
            outcome.death = Some((DeathCause::KillZ, None));
        }
        self.below_kill_z = below;
        outcome
    }

    /// Forgets every touch (after a teleport the next update starts fresh,
    /// so volumes the player is still inside touch again).
    pub fn clear_touches(&mut self) {
        self.checkpoint_touching.iter_mut().for_each(|t| *t = false);
        self.volume_touching.iter_mut().for_each(|t| *t = false);
        self.trigger_touching.iter_mut().for_each(|t| *t = false);
    }

    /// Kismet `SeqAct_TriggerCheckpoint`: activates the checkpoint `id`.
    pub fn trigger_checkpoint(
        &mut self,
        actors: &SceneActors,
        id: u32,
        events: &mut Vec<WorldEvent>,
    ) -> bool {
        match actors.checkpoints.iter().position(|c| c.id == id) {
            Some(i) => self.checkpoints.activate(&actors.checkpoints, i, events),
            None => false,
        }
    }

    /// Kismet `SeqAct_ToggleCheckpointEnable`.
    pub fn set_checkpoint_enabled(&mut self, actors: &SceneActors, id: u32, enabled: bool) -> bool {
        match actors.checkpoints.iter().position(|c| c.id == id) {
            Some(i) => {
                if let Some(e) = self.checkpoints.enabled.get_mut(i) {
                    *e = enabled;
                }
                true
            }
            None => false,
        }
    }

    /// Kismet `SeqAct_Toggle` on a dynamic volume (collision on/off). A
    /// disabled volume stops touching; enabling it while the player is
    /// inside touches again at the next update.
    pub fn set_volume_enabled(&mut self, actors: &SceneActors, id: u32, enabled: bool) -> bool {
        match actors.volumes.iter().position(|v| v.id == id) {
            Some(i) => {
                if let Some(e) = self.volume_enabled.get_mut(i) {
                    *e = enabled;
                }
                if !enabled && let Some(t) = self.volume_touching.get_mut(i) {
                    *t = false;
                }
                true
            }
            None => false,
        }
    }

    /// Kismet `SeqAct_ToggleFallingRocksActive` (all `ASAMUFallingRock`s).
    pub fn set_falling_rocks_active(&mut self, actors: &SceneActors, active: bool) {
        for (s, d) in self.rocks.iter_mut().zip(&actors.rocks) {
            if d.kind == RockKind::Falling {
                goto(
                    s,
                    if active {
                        RockStateName::Falling
                    } else {
                        RockStateName::Inactive
                    },
                );
            }
        }
    }

    /// `Grappled` handler of the rock `id`; returns whether a rock reacted.
    pub fn rock_grappled(
        &mut self,
        actors: &SceneActors,
        id: u32,
        events: &mut Vec<WorldEvent>,
    ) -> bool {
        let Some(i) = actors.rocks.iter().position(|r| r.id == id) else {
            return false;
        };
        let Some(s) = self.rocks.get_mut(i) else {
            return false;
        };
        match s.state {
            RockStateName::Falling => {
                goto(s, RockStateName::Grappled);
                true
            }
            RockStateName::Idle => {
                goto(s, RockStateName::Dropping);
                events.push(WorldEvent::RockReleased { id });
                true
            }
            _ => false,
        }
    }

    /// `UnGrappled` handler of the rock `id`.
    pub fn rock_ungrappled(&mut self, actors: &SceneActors, id: u32) -> bool {
        let Some(i) = actors.rocks.iter().position(|r| r.id == id) else {
            return false;
        };
        match self.rocks.get_mut(i) {
            Some(s) if s.state == RockStateName::Grappled => {
                goto(s, RockStateName::Falling);
                true
            }
            _ => false,
        }
    }

    /// The death hook: every falling-when-grappled rock back to its start
    /// with a new random speed (`ResetAllGrappleRocks`).
    pub fn reset_grapple_rocks(&mut self, actors: &SceneActors) {
        for (s, d) in self.rocks.iter_mut().zip(&actors.rocks) {
            if d.kind == RockKind::FallingWhenGrappled {
                s.fall_rate = self
                    .rng
                    .range(d.params.falling_lower_rate, d.params.falling_higher_rate);
                s.location = d.location;
                goto(s, RockStateName::Idle);
            }
        }
    }

    /// Current location of the rock `id`.
    #[must_use]
    pub fn rock_location(&self, actors: &SceneActors, id: u32) -> Option<Vec3> {
        let i = actors.rocks.iter().position(|r| r.id == id)?;
        self.rocks.get(i).map(|s| s.location)
    }

    /// Runs the rocks' latent code for one tick and moves their collision
    /// (`dynamic` instances placed through `bodies`).
    pub fn tick_rocks(
        &mut self,
        actors: &SceneActors,
        bodies: &[DynamicBody],
        collision: &CollisionScene,
        dynamic: &mut [Instance],
        dt: f32,
    ) {
        for (i, def) in actors.rocks.iter().enumerate() {
            let Some(s) = self.rocks.get_mut(i) else {
                break;
            };
            let before = (s.location, s.rotation);
            run_rock(s, def, dt, &mut self.rng);
            if !(s.location.is_finite() && s.fall_rate.is_finite()) {
                // Hostile parameters (rates near `f32::MAX`) must not put a
                // non-finite location into the collision or the grapple's
                // anchor: the rock stops where it was.
                s.location = before.0;
                s.fall_rate = 0.0;
            }
            if (s.location, s.rotation) != before
                && let Some(body) = def.body.and_then(|b| bodies.get(b))
            {
                let actor = scene::body_transform(body, s.location, s.rotation);
                for (index, relative) in &body.parts {
                    if let Some(inst) = dynamic.get_mut(*index) {
                        collision.place_dynamic(inst, relative.then(&actor));
                    }
                }
            }
        }
    }
}

fn desired_spin(rng: &mut Rng, p: &RockParams) -> [i32; 3] {
    // Roll from X, yaw from Y, pitch from Z (assignment truncates).
    let roll = rng.range(p.rotation_lower_rate.x, p.rotation_higher_rate.x);
    let yaw = rng.range(p.rotation_lower_rate.y, p.rotation_higher_rate.y);
    let pitch = rng.range(p.rotation_lower_rate.z, p.rotation_higher_rate.z);
    [pitch as i32, yaw as i32, roll as i32]
}

/// `GotoState` (state locals zeroed, code restarts at `Begin`).
fn goto(s: &mut RockState, state: RockStateName) {
    if s.state != state {
        s.final_fall_rate = 0.0;
        s.time_fallen = 0.0;
    }
    s.state = state;
    s.sleep = None;
    s.begin_pending = true;
}

/// One rock's state code for a tick.
fn run_rock(s: &mut RockState, def: &RockDef, dt: f32, rng: &mut Rng) {
    let woke = poll_sleep(&mut s.sleep, dt);
    if !woke && (s.sleep.is_some() || !s.begin_pending) {
        return;
    }
    let begin = !woke;
    s.begin_pending = false;
    let p = &def.params;
    let step = p.update_rate;
    match s.state {
        RockStateName::Inactive | RockStateName::Idle => {}
        RockStateName::Falling => {
            if p.should_rotate {
                s.rotation_rate = rinterp_to(
                    s.rotation_rate,
                    s.desired_rotation_rate,
                    step,
                    p.rotation_accel_rate,
                );
                let add = rot_scale(s.rotation_rate, step);
                s.rotation = [0, 1, 2].map(|k| s.rotation[k].wrapping_add(add[k]));
            }
            let mv = -s.fall_rate * step;
            s.fall_rate = (s.fall_rate + p.accel_rate * step).min(s.max_fall_rate);
            s.location.z += mv;
            if def.location.z - s.location.z > p.fall_distance {
                if !p.respawn_at_start {
                    s.hidden = true;
                    return; // the loop breaks; no further code
                }
                s.location = def.location;
                s.fall_rate = 0.0;
                s.max_fall_rate = rng.range(p.falling_lower_rate, p.falling_higher_rate);
                s.desired_rotation_rate = desired_spin(rng, p);
            }
            s.sleep = Some(step);
        }
        RockStateName::Grappled => {
            if s.fall_rate > 0.1 {
                let mv = -s.fall_rate * step;
                s.fall_rate = (s.fall_rate - p.decel_rate * step).max(0.0);
                s.location.z += mv;
                s.sleep = Some(step);
            }
        }
        RockStateName::Dropping => {
            if begin {
                s.final_fall_rate = s.fall_rate;
            }
            if def.location.z - s.location.z < p.fall_distance {
                s.location.z -= s.fall_rate * step;
                s.fall_rate =
                    if s.time_fallen < p.reach_max_speed_time && p.reach_max_speed_time > 0.0 {
                        s.final_fall_rate * (s.time_fallen / p.reach_max_speed_time)
                    } else {
                        s.final_fall_rate
                    };
                s.time_fallen += step;
                s.sleep = Some(step);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cp(id: u32, index: i32) -> CheckpointDef {
        CheckpointDef {
            id,
            name: format!("cp{id}"),
            index,
            enabled: true,
            triggered_from_kismet: false,
            location: Vec3::new(id as f32 * 1000.0, 0.0, 0.0),
            rotation: [0, 0, 0],
            radius: 100.0,
            half_height: 50.0,
            spawn_location: Vec3::new(id as f32 * 1000.0, 0.0, 100.0),
            spawn_rotation: [0, 0, 0],
        }
    }

    #[test]
    fn checkpoint_rules_follow_the_original_manager() {
        // Indices 0..3, stored out of order.
        let defs = vec![cp(10, 2), cp(11, 0), cp(12, 1), cp(13, 3)];
        let mut m = CheckpointManager::new(&defs);
        assert_eq!(m.sorted, vec![1, 2, 0, 3]);
        // Fresh start: lookup is the lowest index; registering 0 changes nothing.
        assert_eq!(m.lookup(), Some(1));
        assert!(!m.register(&defs, 0));
        assert_eq!(m.latest, None);
        let mut ev = Vec::new();
        // Index 2 is newer: latest becomes 2 (a position: the third sorted entry).
        assert!(m.activate(&defs, 0, &mut ev));
        assert_eq!(m.latest, Some(2));
        assert_eq!(m.respawn(&defs).map(|r| r.2), Some(10));
        assert!(ev.contains(&WorldEvent::CheckpointSaved { index: 2 }));
        // Lower index activates but does not move the latest.
        ev.clear();
        assert!(m.activate(&defs, 2, &mut ev));
        assert_eq!(m.latest, Some(2));
        assert!(
            !ev.iter()
                .any(|e| matches!(e, WorldEvent::CheckpointSaved { .. }))
        );
        // Already activated: nothing.
        assert!(!m.activate(&defs, 0, &mut ev));
        // Disabled: nothing.
        m.enabled[3] = false;
        assert!(!m.activate(&defs, 3, &mut ev));
        // An index used as a position past the end finds no checkpoint, so
        // any later registration wins.
        m.latest = Some(7);
        assert_eq!(m.lookup(), None);
        assert!(m.register(&defs, 1));
        assert_eq!(m.latest, Some(1));
        // A negative stored index (a checkpoint numbered below 0) is not a
        // position: the lookup falls back to the first checkpoint.
        let negative = vec![cp(20, -3), cp(21, 0)];
        let mut n = CheckpointManager::new(&negative);
        assert_eq!(n.lookup(), Some(0));
        n.latest = Some(-3);
        assert_eq!(n.lookup(), Some(0));
        // Registering 0 at level start then saves (A-CP-5: lowest index < 0).
        assert!(n.register(&negative, 0));
        assert_eq!(n.respawn(&negative).map(|r| r.2), Some(20));
        // No checkpoints at all: register(0) stores 0, lookup stays empty.
        let mut empty = CheckpointManager::new(&[]);
        assert!(empty.register(&[], 0));
        assert!(empty.respawn(&[]).is_none());
    }

    #[test]
    fn swept_intervals() {
        // Cylinder target at the origin (r 100, h 50); player r 21, h 44.
        let iv = cylinder_interval(
            DVec3::new(-500.0, 0.0, 0.0),
            DVec3::new(500.0, 0.0, 0.0),
            21.0,
            44.0,
            DVec3::ZERO,
            100.0,
            50.0,
        )
        .unwrap();
        assert!((iv.0 - (500.0 - 121.0) / 1000.0).abs() < 1e-12);
        assert!((iv.1 - (500.0 + 121.0) / 1000.0).abs() < 1e-12);
        // Above it: no overlap (|dz| = 94 is not < 94).
        assert!(
            cylinder_interval(
                DVec3::new(-500.0, 0.0, 94.0),
                DVec3::new(500.0, 0.0, 94.0),
                21.0,
                44.0,
                DVec3::ZERO,
                100.0,
                50.0
            )
            .is_none()
        );
        // Box hull [-100, 100]^3.
        let planes = vec![
            (DVec3::X, 100.0),
            (DVec3::NEG_X, 100.0),
            (DVec3::Y, 100.0),
            (DVec3::NEG_Y, 100.0),
            (DVec3::Z, 100.0),
            (DVec3::NEG_Z, 100.0),
        ];
        let hull = Hull {
            planes,
            min: Vec3::splat(-100.0),
            max: Vec3::splat(100.0),
        };
        let iv = hull_interval(
            DVec3::new(0.0, 0.0, 1000.0),
            DVec3::new(0.0, 0.0, 0.0),
            21.0,
            44.0,
            &hull,
        )
        .unwrap();
        assert!((iv.0 - (1000.0 - 144.0) / 1000.0).abs() < 1e-12);
        assert_eq!(iv.1, 1.0);
        assert!(
            hull_interval(
                DVec3::new(0.0, 300.0, 1000.0),
                DVec3::new(0.0, 300.0, 0.0),
                21.0,
                44.0,
                &hull
            )
            .is_none()
        );
    }

    fn rock(kind: RockKind) -> RockDef {
        RockDef {
            id: 7,
            name: "rock".into(),
            kind,
            location: Vec3::new(0.0, 0.0, 1000.0),
            rotation: [0, 0, 0],
            params: RockParams {
                respawn_at_start: true,
                falling_lower_rate: 600.0,
                falling_higher_rate: 600.0,
                fall_distance: 100.0,
                accel_rate: 700.0,
                decel_rate: 1000.0,
                update_rate: 0.017,
                should_rotate: false,
                rotation_lower_rate: Vec3::ZERO,
                rotation_higher_rate: Vec3::ZERO,
                rotation_accel_rate: 1.0,
                reach_max_speed_time: 1.0,
            },
            body: None,
        }
    }

    #[test]
    fn falling_rock_cycle_and_grapple_deceleration() {
        let def = rock(RockKind::Falling);
        let actors = SceneActors {
            rocks: vec![def.clone()],
            ..SceneActors::default()
        };
        let mut rt = SceneRuntime::new(&actors, 1, 1);
        let dt = 1.0 / 60.0;
        let scene = CollisionScene::empty();
        // Inactive until activated.
        for _ in 0..10 {
            rt.tick_rocks(&actors, &[], &scene, &mut [], dt);
        }
        assert_eq!(rt.rocks[0].location, def.location);
        rt.set_falling_rocks_active(&actors, true);
        let mut fell = false;
        let mut respawned = false;
        let mut prev = def.location.z;
        for _ in 0..600 {
            rt.tick_rocks(&actors, &[], &scene, &mut [], dt);
            let z = rt.rocks[0].location.z;
            if z < prev {
                fell = true;
            }
            if z > prev {
                respawned = true;
                assert_eq!(z, def.location.z);
            }
            prev = z;
            assert!(rt.rocks[0].fall_rate <= 600.0);
        }
        assert!(fell && respawned);
        // Grappled: decelerates to a stop, then resumes when released.
        let mut ev = Vec::new();
        for _ in 0..10 {
            rt.tick_rocks(&actors, &[], &scene, &mut [], dt);
        }
        assert!(rt.rock_grappled(&actors, 7, &mut ev));
        assert_eq!(rt.rocks[0].state, RockStateName::Grappled);
        for _ in 0..200 {
            rt.tick_rocks(&actors, &[], &scene, &mut [], dt);
        }
        assert!(rt.rocks[0].fall_rate <= 0.1);
        let stopped = rt.rocks[0].location;
        rt.tick_rocks(&actors, &[], &scene, &mut [], dt);
        assert_eq!(rt.rocks[0].location, stopped);
        assert!(rt.rock_ungrappled(&actors, 7));
        for _ in 0..30 {
            rt.tick_rocks(&actors, &[], &scene, &mut [], dt);
        }
        assert!(rt.rocks[0].location.z < stopped.z);
        // Deactivated: stays put.
        rt.set_falling_rocks_active(&actors, false);
        let here = rt.rocks[0].location;
        for _ in 0..30 {
            rt.tick_rocks(&actors, &[], &scene, &mut [], dt);
        }
        assert_eq!(rt.rocks[0].location, here);
    }

    #[test]
    fn falling_when_grappled_rock_drops_once_and_resets() {
        let def = rock(RockKind::FallingWhenGrappled);
        let actors = SceneActors {
            rocks: vec![def.clone()],
            ..SceneActors::default()
        };
        let mut rt = SceneRuntime::new(&actors, 1, 9);
        let dt = 1.0 / 60.0;
        let scene = CollisionScene::empty();
        let mut ev = Vec::new();
        assert!(rt.rock_grappled(&actors, 7, &mut ev));
        assert_eq!(ev, vec![WorldEvent::RockReleased { id: 7 }]);
        // First step moves by the full speed (600 · 0.017 = 10.2), then the
        // speed ramps from 0.
        rt.tick_rocks(&actors, &[], &scene, &mut [], dt);
        assert!((def.location.z - rt.rocks[0].location.z - 10.2).abs() < 1e-3);
        assert_eq!(rt.rocks[0].fall_rate, 0.0);
        for _ in 0..2000 {
            rt.tick_rocks(&actors, &[], &scene, &mut [], dt);
        }
        let bottom = rt.rocks[0].location.z;
        assert!(def.location.z - bottom >= 100.0);
        assert!(def.location.z - bottom < 120.0);
        // A second grapple does nothing; a death resets it.
        assert!(!rt.rock_grappled(&actors, 7, &mut ev));
        rt.reset_grapple_rocks(&actors);
        assert_eq!(rt.rocks[0].location, def.location);
        assert_eq!(rt.rocks[0].state, RockStateName::Idle);
    }

    #[test]
    fn rinterp_and_rotator_scaling() {
        assert_eq!(rot_scale([1000, -1000, 7], 0.017), [17, -17, 0]);
        let r = rinterp_to([0, 0, 0], [0, 2000, 0], 0.017, 1.0);
        assert_eq!(r, [0, 34, 0]);
        assert_eq!(rinterp_to([5, 5, 5], [5, 5, 5], 0.017, 1.0), [5, 5, 5]);
        assert_eq!(rinterp_to([0, 0, 0], [0, 100, 0], 0.017, 0.0), [0, 100, 0]);
        let mut rng = Rng(3);
        for _ in 0..100 {
            let v = rng.range(470.0, 2189.0);
            assert!((470.0..2189.0).contains(&v));
        }
    }
}
