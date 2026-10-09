//! Locomotion (walking / falling) behind a swappable [`MovementModel`].
//!
//! # PLACEHOLDER model
//!
//! [`PlaceholderMovement`] is **our own simple model**, written so the graybox
//! is playable and the rest of the pipeline (grapple, traces, parity harness)
//! can be built and tested. It is not derived from the original game:
//!
//! - grounded: horizontal velocity accelerates towards the wish velocity
//!   (`max_ground_speed · input`) at `ground_acceleration`, or towards zero at
//!   `braking_deceleration` with no input; no gravity; vertical velocity 0.
//! - jump: when grounded and jump was pressed, vertical velocity is set to
//!   `jump_velocity`.
//! - airborne: gravity, plus "air control" that can only *add* speed along the
//!   wish direction up to `max_ground_speed` (at `air_control ·
//!   ground_acceleration`), so momentum is never braked in the air.
//! - external acceleration (the grapple pull) is added in both modes. While
//!   grounded the floor supports the player: only the horizontal part is
//!   applied, and the player lifts off only if the external upward
//!   acceleration exceeds gravity (vertical velocity `(g + a_z) · dt`, i.e.
//!   the net upward acceleration); otherwise vertical velocity stays 0. This
//!   keeps a weak grapple pull from making `grounded` flicker every tick.
//! - downward speed is capped at `max_fall_speed`.
//! - integration is semi-implicit Euler (velocity first, then position), then a
//!   collide-and-slide move with step-up while walking, then a floor probe
//!   that snaps to walkable floors and detects landing / walking off ledges.
//!
//! # The original's model
//!
//! The original moves the player with the stock UE3/UDK native pawn physics
//! (`APawn::physWalking`, `AUDKPawn::physFalling`, `AUDKPawn::CalcVelocity`,
//! see `docs/reverse-engineering/NATIVE_PHYSICS.md`), parameterised by script
//! defaults. That port is [`crate::ue3_movement::Ue3PawnMovement`], a second
//! [`MovementModel`]; [`MovementModelKind`] selects between the two at run
//! time ([`MovementModelKind::Ue3Pawn`] is its default). The raw
//! [`crate::sim::step`] still uses [`PlaceholderMovement`].
//!
//! # Script hooks
//!
//! The original's pawn script writes `GroundSpeed` and `AirControl` at run
//! time (sprint, story mode, landings) and reacts to landings *inside* the
//! native physics (`processLanded` calls the pawn's `Landed` event before the
//! rest of the tick walks). [`MovementModel::advance_scripted`] takes those
//! values and that event through [`PawnHooks`]; [`MovementModel::advance`]
//! uses [`ClassDefaults`] (the parameter values, no reaction), which is
//! bit-identical to the behaviour before the hooks existed.

use glam::Vec3;
use serde::{Deserialize, Serialize};

use crate::params::MovementParams;
use crate::sim::{PlayerState, StepEvents};
use crate::ue3_movement::Ue3PawnMovement;
use crate::world::{CONTACT_SKIN, CollisionShape, CollisionWorld, MIN_MOVE};

/// Maximum collide-and-slide iterations per move. Implementation detail of
/// the placeholder model, not a gameplay constant (the original's movement
/// iteration budget is a separate, unrecovered quantity).
pub const MAX_SLIDE_ITERATIONS: usize = 4;

/// Floor probe distance (UU) while airborne. Numerical tolerance, not a
/// gameplay constant.
pub const AIRBORNE_FLOOR_PROBE: f32 = 2.0 * CONTACT_SKIN;

/// What the player wants to do this tick, in world terms.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct LocomotionIntent {
    /// Horizontal unit vector (or zero) of desired movement, UE3 axes.
    pub wish_dir: Vec3,
    /// Input magnitude in `[0, 1]`.
    pub wish_scale: f32,
    /// Jump was pressed this tick.
    pub jump: bool,
}

/// A landing, as seen by the pawn script's `Landed` event (called by the
/// native `processLanded` before the pawn switches to walking).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Landing {
    /// Floor normal of the landing hit.
    pub hit_normal: Vec3,
    /// Velocity at the landing (after the native landing-velocity rule).
    pub velocity: Vec3,
    /// Collision-centre position at the landing.
    pub location: Vec3,
    /// The floor actor carries the tag `NotLandable` (ABILITIES.md §5), from
    /// the landing hit's [`crate::world::Surface`]. ([`PlaceholderMovement`]
    /// always reports `false`.)
    pub not_landable: bool,
}

/// Run-time pawn values written by script and the script's reaction to a
/// landing, as read by the movement model during one tick.
pub trait PawnHooks {
    /// Current `GroundSpeed` (walking cap; air `BoundSpeed` threshold), UU/s.
    fn ground_speed(&self) -> f32;
    /// Current `AirControl`.
    fn air_control(&self) -> f32;
    /// Current `AirSpeed` (the flying speed cap), UU/s. The grapple gun sets
    /// it to `fGrappleAccel` (GRAPPLE.md G-PH-4).
    fn air_speed(&self) -> f32;
    /// The pawn's `Landed` event. Afterwards the model re-reads
    /// [`Self::ground_speed`] and [`Self::air_control`] for the rest of the
    /// tick.
    fn landed(&mut self, landing: &Landing);
}

/// [`PawnHooks`] with the class defaults of [`MovementParams`] and no
/// reaction to landings (the behaviour without a script layer).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ClassDefaults {
    /// `movement.max_ground_speed`.
    pub ground_speed: f32,
    /// `movement.air_control`.
    pub air_control: f32,
    /// `movement.air_speed`.
    pub air_speed: f32,
}

impl ClassDefaults {
    /// The values of `params`.
    #[must_use]
    pub fn new(params: &MovementParams) -> Self {
        Self {
            ground_speed: params.max_ground_speed.value,
            air_control: params.air_control.value,
            air_speed: params.air_speed.value,
        }
    }
}

impl PawnHooks for ClassDefaults {
    fn ground_speed(&self) -> f32 {
        self.ground_speed
    }

    fn air_control(&self) -> f32 {
        self.air_control
    }

    fn air_speed(&self) -> f32 {
        self.air_speed
    }

    fn landed(&mut self, _landing: &Landing) {}
}

/// A locomotion model: integrates velocity and moves the player through the
/// world for one tick.
///
/// Implementations may only modify `state.position`, `state.velocity`,
/// `state.grounded` and `state.pawn` (model bookkeeping), must be
/// deterministic, and must not use global state.
pub trait MovementModel {
    /// Advances locomotion by `dt` seconds with run-time pawn values and the
    /// landing reaction supplied by `hooks` (see the module docs).
    /// `external_accel` (UU/s²) is added on top of the model's own forces
    /// (used for the grapple pull).
    #[allow(clippy::too_many_arguments)]
    fn advance_scripted<W: CollisionWorld + ?Sized, H: PawnHooks + ?Sized>(
        &self,
        state: &mut PlayerState,
        intent: &LocomotionIntent,
        external_accel: Vec3,
        params: &MovementParams,
        hooks: &mut H,
        world: &W,
        dt: f32,
        events: &mut StepEvents,
    );

    /// [`Self::advance_scripted`] with [`ClassDefaults`].
    #[allow(clippy::too_many_arguments)]
    fn advance<W: CollisionWorld + ?Sized>(
        &self,
        state: &mut PlayerState,
        intent: &LocomotionIntent,
        external_accel: Vec3,
        params: &MovementParams,
        world: &W,
        dt: f32,
        events: &mut StepEvents,
    ) {
        let mut hooks = ClassDefaults::new(params);
        self.advance_scripted(
            state,
            intent,
            external_accel,
            params,
            &mut hooks,
            world,
            dt,
            events,
        );
    }
}

/// The documented PLACEHOLDER locomotion model (see module docs).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PlaceholderMovement;

/// Run-time choice of locomotion model (implements [`MovementModel`] by
/// dispatching to the selected model).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MovementModelKind {
    /// [`PlaceholderMovement`] (kept for debugging).
    Placeholder,
    /// [`Ue3PawnMovement`], the port of the original's native pawn physics
    /// (default).
    #[default]
    Ue3Pawn,
}

impl MovementModelKind {
    /// Stable machine-friendly name (`"placeholder"` / `"ue3_pawn"`).
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Placeholder => "placeholder",
            Self::Ue3Pawn => "ue3_pawn",
        }
    }
}

impl MovementModel for MovementModelKind {
    fn advance_scripted<W: CollisionWorld + ?Sized, H: PawnHooks + ?Sized>(
        &self,
        state: &mut PlayerState,
        intent: &LocomotionIntent,
        external_accel: Vec3,
        params: &MovementParams,
        hooks: &mut H,
        world: &W,
        dt: f32,
        events: &mut StepEvents,
    ) {
        match self {
            Self::Placeholder => PlaceholderMovement.advance_scripted(
                state,
                intent,
                external_accel,
                params,
                hooks,
                world,
                dt,
                events,
            ),
            Self::Ue3Pawn => Ue3PawnMovement.advance_scripted(
                state,
                intent,
                external_accel,
                params,
                hooks,
                world,
                dt,
                events,
            ),
        }
    }
}

/// The collision shape described by `params`.
#[must_use]
pub fn collision_shape(params: &MovementParams) -> CollisionShape {
    CollisionShape {
        radius: params.capsule_radius.value,
        half_height: params.capsule_half_height.value,
    }
}

/// Removes the component of `v` going into a surface with normal `n`.
#[must_use]
pub fn clip_velocity(v: Vec3, n: Vec3) -> Vec3 {
    let into = v.dot(n);
    if into < 0.0 { v - n * into } else { v }
}

#[derive(Clone, Copy, Debug)]
struct StepUp {
    height: f32,
    walkable_z: f32,
}

/// Collide-and-slide: moves from `start` by `delta`, sliding along surfaces
/// and clipping `velocity` against them. Returns the final position.
fn slide_move<W: CollisionWorld + ?Sized>(
    world: &W,
    shape: CollisionShape,
    start: Vec3,
    velocity: &mut Vec3,
    delta: Vec3,
    step: Option<StepUp>,
) -> Vec3 {
    let mut pos = start;
    let mut remaining = delta;
    let mut planes = [Vec3::ZERO; MAX_SLIDE_ITERATIONS];
    let mut plane_count = 0;
    for _ in 0..MAX_SLIDE_ITERATIONS {
        if remaining.length() < MIN_MOVE {
            break;
        }
        let Some(hit) = world.sweep_capsule(pos, pos + remaining, shape) else {
            pos += remaining;
            break;
        };
        pos = hit.position + hit.normal * CONTACT_SKIN;
        remaining *= 1.0 - hit.time;

        // Walking into a wall-like surface: try to step up. A successful step
        // consumes the rest of the move.
        if let Some(s) = step
            && hit.normal.z.abs() < s.walkable_z
        {
            let horizontal = Vec3::new(remaining.x, remaining.y, 0.0);
            if let Some(p) = try_step_up(world, shape, pos, horizontal, s) {
                pos = p;
                break;
            }
        }

        remaining = clip_velocity(remaining, hit.normal);
        *velocity = clip_velocity(*velocity, hit.normal);
        // Crease handling: if the new direction goes back into an earlier
        // plane, slide along the intersection line of the two planes.
        for &p in &planes[..plane_count] {
            if remaining.dot(p) < 0.0 {
                let crease = p.cross(hit.normal).normalize_or_zero();
                remaining = crease * remaining.dot(crease);
                *velocity = crease * velocity.dot(crease);
            }
        }
        if plane_count < planes.len() {
            planes[plane_count] = hit.normal;
            plane_count += 1;
        }
    }
    pos
}

/// Tries to step up onto a ledge no higher than `s.height`: move up, move
/// horizontally, move down onto a walkable surface. Returns the new position.
fn try_step_up<W: CollisionWorld + ?Sized>(
    world: &W,
    shape: CollisionShape,
    pos: Vec3,
    horizontal: Vec3,
    s: StepUp,
) -> Option<Vec3> {
    if horizontal.length() < MIN_MOVE || s.height <= CONTACT_SKIN {
        return None;
    }
    let up_target = pos + Vec3::Z * s.height;
    let raised_pos = match world.sweep_capsule(pos, up_target, shape) {
        Some(h) => h.position + h.normal * CONTACT_SKIN,
        None => up_target,
    };
    let raised = raised_pos.z - pos.z;
    if raised <= CONTACT_SKIN {
        return None;
    }
    let forward_target = raised_pos + horizontal;
    let forward_pos = match world.sweep_capsule(raised_pos, forward_target, shape) {
        Some(h) => h.position + h.normal * CONTACT_SKIN,
        None => forward_target,
    };
    if (forward_pos - raised_pos).truncate().length() < MIN_MOVE {
        return None;
    }
    let down_target = forward_pos - Vec3::Z * (raised + 2.0 * CONTACT_SKIN);
    match world.sweep_capsule(forward_pos, down_target, shape) {
        Some(h) if h.normal.z >= s.walkable_z && !h.start_penetrating => {
            Some(h.position + h.normal * CONTACT_SKIN)
        }
        _ => None,
    }
}

/// Probes down from `pos` by `distance` for a walkable floor. Returns the
/// snapped position on success.
fn probe_floor<W: CollisionWorld + ?Sized>(
    world: &W,
    shape: CollisionShape,
    pos: Vec3,
    distance: f32,
    walkable_z: f32,
) -> Option<Vec3> {
    let hit = world.sweep_capsule(pos, pos - Vec3::Z * distance, shape)?;
    if hit.normal.z >= walkable_z {
        Some(if hit.start_penetrating {
            pos
        } else {
            hit.position + hit.normal * CONTACT_SKIN
        })
    } else {
        None
    }
}

/// Snaps `state` onto a walkable floor within `max_drop` UU below it. Returns
/// `true` (and sets `grounded`, zeroes vertical velocity) if a floor was found.
pub fn place_on_floor<W: CollisionWorld + ?Sized>(
    state: &mut PlayerState,
    params: &MovementParams,
    world: &W,
    max_drop: f32,
) -> bool {
    let shape = collision_shape(params);
    match probe_floor(
        world,
        shape,
        state.position,
        max_drop,
        params.walkable_floor_z.value,
    ) {
        Some(p) => {
            state.position = p;
            state.velocity.z = 0.0;
            state.grounded = true;
            true
        }
        None => {
            state.grounded = false;
            false
        }
    }
}

impl MovementModel for PlaceholderMovement {
    fn advance_scripted<W: CollisionWorld + ?Sized, H: PawnHooks + ?Sized>(
        &self,
        state: &mut PlayerState,
        intent: &LocomotionIntent,
        external_accel: Vec3,
        params: &MovementParams,
        hooks: &mut H,
        world: &W,
        dt: f32,
        events: &mut StepEvents,
    ) {
        // The placeholder has no flying model of its own: a flying pawn (only
        // the original grapple sets it) moves with the native-physics port's
        // `physFlying`.
        if state.pawn.flying {
            Ue3PawnMovement.advance_scripted(
                state,
                intent,
                external_accel,
                params,
                hooks,
                world,
                dt,
                events,
            );
            return;
        }
        let shape = collision_shape(params);
        let walkable_z = params.walkable_floor_z.value;
        let was_grounded = state.grounded;
        let mut v = state.velocity;

        // Jump.
        if state.grounded && intent.jump {
            v.z = params.jump_velocity.value;
            state.grounded = false;
            events.jumped = true;
        }

        let max_speed = hooks.ground_speed() * intent.wish_scale;
        let has_wish = intent.wish_dir != Vec3::ZERO && max_speed > 0.0;
        if state.grounded {
            // Accelerate the horizontal velocity towards the wish velocity.
            let vh = Vec3::new(v.x, v.y, 0.0);
            let target = if has_wish {
                intent.wish_dir * max_speed
            } else {
                Vec3::ZERO
            };
            let rate = if has_wish {
                params.ground_acceleration.value
            } else {
                params.braking_deceleration.value
            };
            let delta = target - vh;
            let max_change = rate * dt;
            let new_vh = if delta.length() <= max_change {
                target
            } else {
                vh + delta.clamp_length_max(max_change)
            };
            // The floor supports the player: lift off only when the external
            // acceleration beats gravity (see module docs).
            let net_up = params.gravity_z.value + external_accel.z;
            let vz = if net_up > 0.0 { net_up * dt } else { 0.0 };
            v = Vec3::new(
                new_vh.x + external_accel.x * dt,
                new_vh.y + external_accel.y * dt,
                vz,
            );
        } else {
            // Air control: only adds speed along the wish direction.
            if has_wish {
                let current = Vec3::new(v.x, v.y, 0.0).dot(intent.wish_dir);
                let add = max_speed - current;
                if add > 0.0 {
                    let accel =
                        (hooks.air_control() * params.ground_acceleration.value * dt).min(add);
                    v += intent.wish_dir * accel;
                }
            }
            v.z += params.gravity_z.value * dt;
            v += external_accel * dt;
        }

        if v.z < -params.max_fall_speed.value {
            v.z = -params.max_fall_speed.value;
        }

        let moving_up = v.z > 0.0;
        let impact_z = v.z;
        let step = state.grounded.then_some(StepUp {
            height: params.step_height.value,
            walkable_z,
        });
        let delta = v * dt;
        let pos = slide_move(world, shape, state.position, &mut v, delta, step);
        state.position = pos;

        // Floor probe: snap to walkable floors, detect landing / leaving.
        if moving_up {
            state.grounded = false;
        } else {
            let probe = if state.grounded {
                params.step_height.value + 2.0 * CONTACT_SKIN
            } else {
                AIRBORNE_FLOOR_PROBE
            };
            match probe_floor(world, shape, state.position, probe, walkable_z) {
                Some(p) => {
                    state.position = p;
                    v.z = 0.0;
                    state.grounded = true;
                }
                None => state.grounded = false,
            }
        }

        if state.grounded && !was_grounded {
            events.landed = Some(impact_z);
            // The placeholder has no sub-steps: the script's landing
            // reaction runs after the whole move.
            hooks.landed(&Landing {
                hit_normal: Vec3::Z,
                velocity: Vec3::new(v.x, v.y, impact_z),
                location: state.position,
                not_landable: false,
            });
        }
        if was_grounded && !state.grounded && !events.jumped {
            events.left_ground = true;
        }
        state.velocity = v;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::world::BoxWorld;

    #[test]
    fn clip_only_removes_inward_component() {
        assert_eq!(
            clip_velocity(Vec3::new(1.0, 0.0, -2.0), Vec3::Z),
            Vec3::new(1.0, 0.0, 0.0)
        );
        assert_eq!(
            clip_velocity(Vec3::new(1.0, 0.0, 2.0), Vec3::Z),
            Vec3::new(1.0, 0.0, 2.0)
        );
    }

    #[test]
    fn steps_onto_low_ledge_but_not_high_wall() {
        let params = MovementParams::default();
        let step_h = params.step_height.value;
        let shape = collision_shape(&params);
        let half = shape.half_height;
        let low = BoxWorld::new().with_ground(0.0, false).with_box(
            Vec3::new(100.0, -200.0, 0.0),
            Vec3::new(400.0, 200.0, step_h * 0.5),
            false,
        );
        let high = BoxWorld::new().with_ground(0.0, false).with_box(
            Vec3::new(100.0, -200.0, 0.0),
            Vec3::new(400.0, 200.0, step_h * 3.0),
            false,
        );
        let start = Vec3::new(0.0, 0.0, half + CONTACT_SKIN);
        let step = Some(StepUp {
            height: step_h,
            walkable_z: params.walkable_floor_z.value,
        });
        let mut v = Vec3::new(500.0, 0.0, 0.0);
        let p = slide_move(&low, shape, start, &mut v, Vec3::new(200.0, 0.0, 0.0), step);
        assert!(p.x > 150.0, "stepped forward: {p}");
        assert!(
            (p.z - (half + step_h * 0.5 + CONTACT_SKIN)).abs() < 1e-3,
            "on the ledge: {p}"
        );
        let mut v = Vec3::new(500.0, 0.0, 0.0);
        let p = slide_move(
            &high,
            shape,
            start,
            &mut v,
            Vec3::new(200.0, 0.0, 0.0),
            step,
        );
        assert!(p.x < 100.0 - shape.radius + 1e-3, "blocked: {p}");
        assert_eq!(v.x, 0.0, "velocity into the wall removed");
    }

    #[test]
    fn slides_along_walls_and_creases() {
        let params = MovementParams::default();
        let shape = collision_shape(&params);
        let w = BoxWorld::new().with_ground(0.0, false).with_box(
            Vec3::new(100.0, -500.0, 0.0),
            Vec3::new(200.0, 500.0, 500.0),
            false,
        );
        let start = Vec3::new(0.0, 0.0, shape.half_height + CONTACT_SKIN);
        let mut v = Vec3::new(100.0, 100.0, 0.0);
        let p = slide_move(&w, shape, start, &mut v, Vec3::new(200.0, 200.0, 0.0), None);
        assert!(p.x <= 100.0 - shape.radius);
        assert!(p.y > 150.0, "kept sliding along the wall: {p}");
        assert_eq!(v, Vec3::new(0.0, 100.0, 0.0));
        // Diagonal down into the floor-wall crease slides along the crease.
        let mut v = Vec3::new(100.0, 100.0, -100.0);
        let p = slide_move(
            &w,
            shape,
            start,
            &mut v,
            Vec3::new(200.0, 200.0, -200.0),
            None,
        );
        assert!(p.y > 150.0, "{p}");
        assert!(p.z >= shape.half_height, "{p}");
        assert!(v.x.abs() < 1e-3 && v.z.abs() < 1e-3, "{v}");
    }

    #[test]
    fn place_on_floor_snaps_or_fails() {
        let params = MovementParams::default();
        let w = BoxWorld::new().with_ground(0.0, false);
        let mut s = PlayerState::new(Vec3::new(0.0, 0.0, 100.0), 0.0);
        assert!(place_on_floor(&mut s, &params, &w, 200.0));
        assert!(s.grounded);
        assert!((s.position.z - (params.capsule_half_height.value + CONTACT_SKIN)).abs() < 1e-4);
        let mut s = PlayerState::new(Vec3::new(0.0, 0.0, 1000.0), 0.0);
        assert!(!place_on_floor(&mut s, &params, &w, 10.0));
        assert!(!s.grounded);
    }
}
