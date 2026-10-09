//! The placeholder **rope** grapple of the debug configuration.
//!
//! # PLACEHOLDER behaviour (debug only)
//!
//! This is **our own** rope model, kept for the raw pipeline that runs
//! without the ASAMU script layer ([`crate::PlayerParams::placeholder`]).
//! The original grapple — no rope, a `10⁷/d` pull in flying physics,
//! release below 200 uu, a grapple budget — is ported in
//! [`crate::grapple_gun`] from `docs/reverse-engineering/GRAPPLE.md` and runs
//! whenever the script layer does ([`crate::PlayerParams::asamu_original`]).
//! This model is not evidence of anything about the original:
//!
//! - **Acquire**: on the *press edge* of the grapple button, cast a ray from the
//!   eye along the view direction up to `max_range`. The first surface hit must
//!   be grapple-able; anything else (or nothing) is a miss. Holding the button
//!   after a miss does not retry.
//! - **Attach**: the anchor is the hit point; the rope length is the current
//!   distance from the player's collision centre to the anchor.
//! - **While attached**: a pull acceleration towards the anchor (only beyond
//!   `min_rope_length`) is fed to the movement model; after the move the rope
//!   constraint is enforced: if the player is farther than the rope length they
//!   are moved back onto the rope sphere (a collision-aware sweep) and any
//!   outward radial velocity is removed. Inward and tangential velocity is
//!   untouched, so swinging keeps its tangential momentum.
//! - **Standing on a taut rope**: when the player is grounded and the rope can
//!   reach the floor (`rope_length > |Δz|`), the correction keeps the height and
//!   moves the player horizontally back onto the rope sphere, and only the
//!   horizontal outward velocity is removed: the rope tethers the player on
//!   the floor instead of hoisting them a fraction of a unit every tick. When
//!   the rope is shorter than the height difference it lifts the player off
//!   the floor (radial correction; `grounded` becomes false).
//! - The rope is a straight segment: it does not wrap around geometry. If
//!   geometry blocks the correction the constraint can be violated until the
//!   player is free again ([`RopeOutcome::correction_blocked`]).
//! - **Release** (button no longer held): detach; velocity is kept bit-for-bit
//!   ([`crate::params::ReleaseMode::PreserveVelocity`]).

use glam::Vec3;
use serde::{Deserialize, Serialize};

use crate::movement::collision_shape;
use crate::params::{PlayerParams, RopeMode};
use crate::sim::PlayerState;
use crate::world::{CONTACT_SKIN, CollisionWorld};

/// Tolerance (UU) within which the rope counts as taut. Numerical tolerance,
/// not a gameplay constant.
pub const ROPE_TAUT_EPSILON: f32 = 1.0e-3;

/// Grapple state.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum GrappleState {
    /// Not attached.
    #[default]
    Idle,
    /// Attached to `anchor` with a rope of `rope_length` UU.
    Attached {
        /// World-space anchor point, UU.
        anchor: Vec3,
        /// Current maximum rope length, UU.
        rope_length: f32,
    },
}

impl GrappleState {
    /// `true` when attached.
    #[must_use]
    pub fn is_attached(&self) -> bool {
        matches!(self, Self::Attached { .. })
    }

    /// The anchor, when attached.
    #[must_use]
    pub fn anchor(&self) -> Option<Vec3> {
        match self {
            Self::Attached { anchor, .. } => Some(*anchor),
            Self::Idle => None,
        }
    }

    /// The rope length, when attached.
    #[must_use]
    pub fn rope_length(&self) -> Option<f32> {
        match self {
            Self::Attached { rope_length, .. } => Some(*rope_length),
            Self::Idle => None,
        }
    }
}

/// What the grapple would hit if fired now.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Aim {
    /// A grapple-able surface within range.
    Grappleable {
        /// Hit point, UU.
        point: Vec3,
        /// Distance from the eye, UU.
        distance: f32,
    },
    /// The first surface within range is not grapple-able.
    Blocked {
        /// Hit point, UU.
        point: Vec3,
        /// Distance from the eye, UU.
        distance: f32,
    },
    /// Nothing within range.
    OutOfRange,
}

/// Grapple events produced by a tick.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum GrappleEvent {
    /// The grapple attached.
    Attached {
        /// Anchor point, UU.
        anchor: Vec3,
        /// Initial rope length, UU.
        rope_length: f32,
    },
    /// The grapple was fired but did not attach.
    Missed {
        /// What the ray found.
        aim: Aim,
    },
    /// The grapple was released.
    Released {
        /// Velocity at the moment of release (kept unchanged), UU/s.
        velocity: Vec3,
    },
}

/// Eye position (UU) for `state`.
#[must_use]
pub fn eye_position(state: &PlayerState, params: &PlayerParams) -> Vec3 {
    state.view_location(params)
}

/// Casts the grapple ray from the eye along the view direction.
#[must_use]
pub fn aim<W: CollisionWorld + ?Sized>(
    state: &PlayerState,
    params: &PlayerParams,
    world: &W,
) -> Aim {
    let eye = eye_position(state, params);
    let dir = state.view_direction();
    match world.raycast(eye, dir, params.grapple.max_range.value) {
        Some(hit) if hit.grapple_able && !hit.start_penetrating => Aim::Grappleable {
            point: hit.position,
            distance: hit.distance,
        },
        Some(hit) => Aim::Blocked {
            point: hit.position,
            distance: hit.distance,
        },
        None => Aim::OutOfRange,
    }
}

/// Handles grapple input for one tick (before movement). Returns the event,
/// if any. `grapple_held` is the sanitized input level.
pub fn handle_input<W: CollisionWorld + ?Sized>(
    state: &mut PlayerState,
    grapple_held: bool,
    params: &PlayerParams,
    world: &W,
) -> Option<GrappleEvent> {
    let pressed = grapple_held && !state.grapple_was_held;
    state.grapple_was_held = grapple_held;
    match state.grapple {
        GrappleState::Attached { .. } if !grapple_held => {
            state.grapple = GrappleState::Idle;
            // ReleaseMode::PreserveVelocity: velocity is not touched.
            Some(GrappleEvent::Released {
                velocity: state.velocity,
            })
        }
        GrappleState::Idle if pressed => match aim(state, params, world) {
            Aim::Grappleable { point, .. } => {
                let rope_length = state
                    .position
                    .distance(point)
                    .max(params.grapple.min_rope_length.value);
                state.grapple = GrappleState::Attached {
                    anchor: point,
                    rope_length,
                };
                Some(GrappleEvent::Attached {
                    anchor: point,
                    rope_length,
                })
            }
            other => Some(GrappleEvent::Missed { aim: other }),
        },
        _ => None,
    }
}

/// Pull acceleration (UU/s²) towards the anchor for the current state.
#[must_use]
pub fn pull_acceleration(state: &PlayerState, params: &PlayerParams) -> Vec3 {
    match state.grapple {
        GrappleState::Attached { anchor, .. } => {
            let to_anchor = anchor - state.position;
            let dist = to_anchor.length();
            if dist > params.grapple.min_rope_length.value && dist > 0.0 {
                to_anchor / dist * params.grapple.pull_acceleration.value
            } else {
                Vec3::ZERO
            }
        }
        GrappleState::Idle => Vec3::ZERO,
    }
}

/// What [`enforce_rope`] did this tick.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RopeOutcome {
    /// Geometry blocked the positional correction (the constraint may then be
    /// violated until the player is free again).
    pub correction_blocked: bool,
    /// The rope lifted a grounded player off the floor (`grounded` was
    /// cleared).
    pub lifted_off_ground: bool,
}

/// Enforces the rope after the movement step (see module docs).
pub fn enforce_rope<W: CollisionWorld + ?Sized>(
    state: &mut PlayerState,
    params: &PlayerParams,
    world: &W,
) -> RopeOutcome {
    let GrappleState::Attached {
        anchor,
        mut rope_length,
    } = state.grapple
    else {
        return RopeOutcome::default();
    };
    let min_len = params.grapple.min_rope_length.value;
    let mut outcome = RopeOutcome::default();

    let offset = state.position - anchor;
    let dist = offset.length();
    if params.grapple.rope_mode.value == RopeMode::ShortenToDistance && dist < rope_length {
        rope_length = dist.max(min_len).min(rope_length);
    }

    if dist > rope_length && dist > 0.0 {
        // Grounded and the rope reaches the floor: keep the height, correct
        // horizontally onto the rope sphere (tether).
        let dz = offset.z;
        let horizontal = Vec3::new(offset.x, offset.y, 0.0);
        let horizontal_len = horizontal.length();
        let tether_target = (state.grounded && rope_length > dz.abs() && horizontal_len > 0.0)
            .then(|| {
                let radius = (rope_length * rope_length - dz * dz).sqrt();
                let h = horizontal / horizontal_len * radius;
                // Keep z bit-exact (anchor.z + dz would round).
                Vec3::new(anchor.x + h.x, anchor.y + h.y, state.position.z)
            });
        let target = tether_target.unwrap_or(anchor + offset / dist * rope_length);
        let shape = collision_shape(&params.movement);
        match world.sweep_capsule(state.position, target, shape) {
            None => state.position = target,
            Some(hit) => {
                state.position = hit.position + hit.normal * CONTACT_SKIN;
                outcome.correction_blocked = true;
            }
        }
        if tether_target.is_none() && state.grounded {
            state.grounded = false;
            outcome.lifted_off_ground = true;
        }
    }

    let offset = state.position - anchor;
    let dist = offset.length();
    if dist > 0.0 && dist >= rope_length - ROPE_TAUT_EPSILON {
        // Grounded (tethered): remove only the horizontal outward velocity so
        // the player stays on the floor. Otherwise the radial outward part.
        let n = if state.grounded {
            Vec3::new(offset.x, offset.y, 0.0).normalize_or_zero()
        } else {
            offset / dist
        };
        let outward = state.velocity.dot(n);
        if outward > 0.0 {
            state.velocity -= n * outward;
        }
    }

    state.grapple = GrappleState::Attached {
        anchor,
        rope_length,
    };
    outcome
}

/// Applies the attached speed cap.
pub fn apply_speed_cap(state: &mut PlayerState, params: &PlayerParams) {
    if !state.grapple.is_attached() {
        return;
    }
    let cap = params.grapple.attached_max_speed.value;
    let speed = state.velocity.length();
    if speed > cap && speed > 0.0 {
        state.velocity *= cap / speed;
    }
}
