//! The deterministic player step function.
//!
//! [`step`] advances a [`PlayerState`] by one fixed tick given an
//! [`InputFrame`], the parameters and a [`CollisionWorld`]. It is pure in the
//! sense that matters for parity work: no globals, no RNG, no wall-clock, no
//! hash-map iteration, no hidden state between calls; the same inputs always
//! produce bit-identical outputs.
//!
//! # Cross-platform determinism
//!
//! The simulation only uses IEEE-754 basic arithmetic, `sqrt` (correctly
//! rounded by IEEE-754), exact operations (comparisons, `min`/`max`, `round`,
//! `rem_euclid`, `f32 ↔ f64` conversions) and the deterministic trigonometry in
//! `asamu_core::det_math` (never the platform `sin`/`cos`). Rust never fuses
//! `a * b + c` into an FMA on its own and the supported targets have no
//! extended-precision `f32`/`f64` arithmetic, so results are expected to be
//! bit-identical on Windows, Linux and macOS. `det_math` pins this with golden
//! values; whole-run identity across platforms has **not** yet been checked on
//! CI (it needs the same trace produced on each OS).
//!
//! # Hostile input
//!
//! - Input frames are sanitized ([`InputFrame::sanitized`]).
//! - A non-finite or non-positive `dt` is a no-op (nothing changes, including
//!   look and grapple input).
//! - A `dt` above [`MAX_STEP_DT`] is clamped to it ([`StepEvents::dt_clamped`]).
//! - If the incoming state is non-finite, or the step would produce a
//!   non-finite state (only possible with invalid parameters or absurd
//!   magnitudes), the state is left exactly as it was and
//!   [`StepEvents::non_finite_rejected`] is set.
//!
//! # Raw physics vs the ASAMU script layer
//!
//! When [`PlayerParams::pawn`] is `Some` (the original parameter set,
//! [`PlayerParams::asamu_original`]), a tick runs the ASAMU script layer
//! around the movement model ([`crate::pawn`], whose module docs give the
//! order: input events → map actors → controller → pawn state code →
//! physics with `Landed` → eye height → power-jump actor → rocket boots →
//! grapple gun), with the original grapple ([`crate::grapple_gun`]). The
//! input events form the first half of the tick ([`begin_step`]) and the
//! rest the second ([`finish_step`]), so a caller can tick map-placed actors
//! in between, as the original does ([`PendingStep`]). When it
//! is `None` (the placeholder set) the tick runs the raw physics below with
//! the placeholder rope grapple ([`crate::grapple`]), unchanged from before
//! the script layer existed:
//!
//! 1. Sanitize input; apply look deltas (yaw wrapped to `[-π, π)`, pitch
//!    clamped to `±max_pitch_degrees`).
//! 2. Grapple input: release if attached and not held (velocity untouched);
//!    on the press edge, try to attach (see [`crate::grapple`]). Attaching and
//!    releasing can never happen in the same tick (attach needs the button
//!    held, release needs it up).
//! 3. Build the locomotion intent from the move axes and yaw.
//! 4. Movement model step (velocity integration, collide-and-slide, floor
//!    probe), with the grapple pull as external acceleration.
//! 5. Rope constraint (position correction + outward velocity removal), then
//!    the attached speed cap.
//! 6. `landed` / `left_ground` are reported for the *net* change of
//!    `grounded` over the tick, so a landing undone by the rope in the same
//!    tick reports nothing.

use asamu_core::coords::{ue_forward_flat, ue_right_flat, ue_view_direction};
use asamu_core::rotator::wrap_radians;
use glam::Vec3;
use serde::{Deserialize, Serialize};

use crate::events::EventLog;
use crate::grapple::{self, GrappleEvent, GrappleState};
use crate::grapple_gun::GunEvents;
use crate::input::InputFrame;
use crate::movement::{LocomotionIntent, MovementModel, PlaceholderMovement};
use crate::params::PlayerParams;
use crate::pawn::{LandingOutcome, PawnScript, PowerJumpEvent};
use crate::rocket_boots::BootsEvent;
use crate::ue3_movement::PawnPhysicsState;
use crate::world::CollisionWorld;

/// Complete simulation state of the player. All values in UE3 axes / UU.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct PlayerState {
    /// Centre of the collision shape, UU.
    pub position: Vec3,
    /// Velocity, UU/s.
    pub velocity: Vec3,
    /// View yaw in radians, `[-π, π)` (UE3 convention: + turns right).
    pub yaw: f32,
    /// View pitch in radians (+ looks up).
    pub pitch: f32,
    /// Standing on a walkable floor.
    pub grounded: bool,
    /// State of the placeholder **rope** grapple (raw pipeline only; the
    /// original grapple of the script layer is [`PawnScript::gun`]). Use
    /// [`Self::grapple_anchor`] to ask "attached, and where?" for either.
    pub grapple: GrappleState,
    /// Grapple button level on the previous tick (press-edge detection).
    pub grapple_was_held: bool,
    /// Native-physics bookkeeping carried between ticks by
    /// [`crate::ue3_movement::Ue3PawnMovement`] (floor normal, base,
    /// force-floor-check). Ignored by [`PlaceholderMovement`]. Not part of
    /// the trace format, so replaying a trace from a recorded sample starts
    /// from the default (unbased) value.
    #[serde(default)]
    pub pawn: PawnPhysicsState,
    /// Run-time state of the ASAMU script layer ([`crate::pawn`]): speeds,
    /// sprint, script state, eye height and bob, FOV, power jump. Unused
    /// (`started == false`) without pawn parameters. Not part of the trace
    /// format.
    #[serde(default)]
    pub script: PawnScript,
}

impl PlayerState {
    /// A new airborne, idle state at `position` facing `yaw`.
    #[must_use]
    pub fn new(position: Vec3, yaw: f32) -> Self {
        Self {
            position,
            yaw: wrap_radians(yaw),
            ..Self::default()
        }
    }

    /// Unit view direction (UE3 axes).
    #[must_use]
    pub fn view_direction(&self) -> Vec3 {
        ue_view_direction(self.yaw, self.pitch)
    }

    /// Unit aim direction of the script layer during a tick: the camera's
    /// cached point-of-view rotation ([`PawnScript::pov_yaw`], the view as
    /// the previous tick ended; GRAPPLE.md G-TG-1). Without the script layer
    /// it is [`Self::view_direction`].
    #[must_use]
    pub fn aim_direction(&self) -> Vec3 {
        if self.script.started {
            ue_view_direction(self.script.pov_yaw, self.script.pov_pitch)
        } else {
            self.view_direction()
        }
    }

    /// The grapple anchor while attached: the original grapple gun's
    /// ([`PawnScript::gun`]) or the placeholder rope's ([`Self::grapple`]).
    #[must_use]
    pub fn grapple_anchor(&self) -> Option<Vec3> {
        self.script.gun.anchor().or(self.grapple.anchor())
    }

    /// `true` while either grapple is attached.
    #[must_use]
    pub fn is_grapple_attached(&self) -> bool {
        self.grapple_anchor().is_some()
    }

    /// Speed, UU/s.
    #[must_use]
    pub fn speed(&self) -> f32 {
        self.velocity.length()
    }

    /// Horizontal speed, UU/s.
    #[must_use]
    pub fn horizontal_speed(&self) -> f32 {
        self.velocity.truncate().length()
    }

    /// `true` if every float in the state is finite.
    #[must_use]
    pub fn is_finite(&self) -> bool {
        let grapple_ok = match self.grapple {
            GrappleState::Idle => true,
            GrappleState::Attached {
                anchor,
                rope_length,
            } => anchor.is_finite() && rope_length.is_finite(),
        };
        self.position.is_finite()
            && self.velocity.is_finite()
            && self.yaw.is_finite()
            && self.pitch.is_finite()
            && self.pawn.floor.is_finite()
            && grapple_ok
            && self.script.is_finite()
    }

    /// The view (camera) location, UU: `position + (0, 0, EyeHeight) +
    /// WalkBob` with the script layer's run-time eye height and bob
    /// (ABILITIES.md A-CM-1), else `position + camera.eye_height`.
    #[must_use]
    pub fn view_location(&self, params: &PlayerParams) -> Vec3 {
        if self.script.started {
            self.position + self.script.view_offset()
        } else {
            self.position + Vec3::Z * params.camera.eye_height.value
        }
    }

    /// The horizontal FOV in degrees: the script layer's run-time FOV (the
    /// story-mode zoom changes it), else `camera.fov_degrees`.
    #[must_use]
    pub fn fov(&self, params: &PlayerParams) -> f32 {
        if self.script.started {
            self.script.fov
        } else {
            params.camera.fov_degrees.value
        }
    }
}

/// Largest `dt` (seconds) a single [`step`] simulates; larger values are
/// clamped to it.
///
/// **Numerical safety bound, not a gameplay value**: it keeps one call from
/// producing enormous displacements (and `f32` precision loss) when a caller
/// passes a stall-sized `dt`. Fixed-step callers at 4 Hz or faster never reach
/// it. How the original handles long frames is UNKNOWN.
pub const MAX_STEP_DT: f32 = 0.25;

/// Things that happened during one tick (for HUD, audio, tests, traces).
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct StepEvents {
    /// A jump started this tick.
    pub jumped: bool,
    /// Landed this tick (net: airborne at the start of the tick, grounded at
    /// the end); value is the vertical velocity just before impact (UU/s).
    pub landed: Option<f32>,
    /// Left the ground without jumping (net: grounded at the start of the
    /// tick, airborne at the end), e.g. walked off a ledge or was lifted by
    /// the grapple.
    pub left_ground: bool,
    /// Placeholder rope grapple attach / miss / release (raw pipeline; the
    /// original grapple reports in [`Self::gun`]).
    pub grapple: Option<GrappleEvent>,
    /// Geometry blocked the rope's positional correction this tick.
    pub rope_correction_blocked: bool,
    /// `dt` exceeded [`MAX_STEP_DT`] and was clamped.
    pub dt_clamped: bool,
    /// The incoming state was non-finite, or the step would have produced a
    /// non-finite state; the state was left unchanged.
    pub non_finite_rejected: bool,
    /// The script layer's `Landed` reaction (first landing of the tick).
    #[serde(default)]
    pub landing: Option<LandingOutcome>,
    /// A power-jump event.
    #[serde(default)]
    pub power_jump: Option<PowerJumpEvent>,
    /// `use` was pressed in story mode (interact with usable actors).
    #[serde(default)]
    pub use_requested: bool,
    /// Original grapple gun: fire outcome, attach, release
    /// ([`crate::grapple_gun`]).
    #[serde(default)]
    pub gun: GunEvents,
    /// Rocket-boots event ([`crate::rocket_boots`]).
    #[serde(default)]
    pub boots: Option<BootsEvent>,
    /// Kismet-visible events and grapple handler calls, in order
    /// ([`crate::events`]).
    #[serde(default)]
    pub kismet: EventLog,
}

/// Builds the locomotion intent from sanitized input and the view yaw.
#[must_use]
pub fn locomotion_intent(input: &InputFrame, yaw: f32) -> LocomotionIntent {
    let raw = glam::Vec2::new(input.move_forward, input.move_right);
    let scale = raw.length().min(1.0);
    let wish = ue_forward_flat(yaw) * input.move_forward + ue_right_flat(yaw) * input.move_right;
    let wish_dir = wish.normalize_or_zero();
    LocomotionIntent {
        wish_dir,
        wish_scale: if wish_dir == Vec3::ZERO { 0.0 } else { scale },
        jump: input.jump_pressed,
    }
}

/// Advances `state` by one tick of `dt` seconds with the
/// [`PlaceholderMovement`] locomotion model.
///
/// A non-finite or non-positive `dt` leaves the state untouched; see the
/// module docs for the other guards. `params` are expected to pass
/// [`PlayerParams::validate`] (as `asamu_game::Game` enforces); invalid ones
/// never cause a panic or a non-finite state, but behave arbitrarily.
pub fn step<W: CollisionWorld + ?Sized>(
    state: &mut PlayerState,
    input: &InputFrame,
    params: &PlayerParams,
    world: &W,
    dt: f32,
) -> StepEvents {
    step_with(&PlaceholderMovement, state, input, params, world, dt)
}

/// [`step`] with an explicit locomotion model: [`begin_step`] and
/// [`finish_step`] with the same world.
pub fn step_with<M: MovementModel, W: CollisionWorld + ?Sized>(
    model: &M,
    state: &mut PlayerState,
    input: &InputFrame,
    params: &PlayerParams,
    world: &W,
    dt: f32,
) -> StepEvents {
    let pending = begin_step(state, input, params, world, dt);
    finish_step(model, state, pending, params, world)
}

/// What [`finish_step`] does with a [`PendingStep`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PendingMode {
    /// Invalid `dt`: nothing happens.
    NoOp,
    /// Non-finite incoming state: rejected, nothing changes.
    Rejected,
    /// The tick runs.
    Run,
}

/// A tick whose input events have run ([`begin_step`]) and whose actor
/// ticks are still to come ([`finish_step`]).
///
/// The original processes key events before any actor ticks (GRAPPLE.md
/// G-IN-5 / G-TM-2): the fire trace of a press sees the map-placed actors
/// (movers, recharge crystals) where the previous frame left them, and
/// handler calls made by the attach (`Grappled`) reach those actors before
/// their own tick. A caller that simulates map actors (`asamu-game`) ticks
/// them between the two calls and hands [`finish_step`] the updated world;
/// [`step_with`] uses one world for both.
#[derive(Clone, Copy, Debug, PartialEq)]
#[must_use = "a pending step must be finished with `finish_step`"]
pub struct PendingStep {
    /// The state before the tick (restored when the tick would produce a
    /// non-finite state).
    before: PlayerState,
    /// The sanitized input.
    input: InputFrame,
    /// The (clamped) tick length.
    dt: f32,
    /// Events raised by the input events.
    events: StepEvents,
    /// What happens next.
    mode: PendingMode,
}

impl PendingStep {
    /// Events raised so far (by the input events: fire, attach and its
    /// handler calls, boost key, power-jump key-up, `use`).
    #[must_use]
    pub fn events(&self) -> &StepEvents {
        &self.events
    }

    /// The tick length the step simulates, `None` when the step does
    /// nothing (invalid `dt` or a rejected non-finite state).
    #[must_use]
    pub fn dt(&self) -> Option<f32> {
        (self.mode == PendingMode::Run).then_some(self.dt)
    }
}

/// First half of a tick: validates `dt` and the state, sanitizes the input
/// and runs the input events (sprint, jump, rocket-boost key, power-jump
/// key, `use`, fire) against `input_world` — with the script layer only;
/// the raw pipeline does everything in [`finish_step`]. See [`PendingStep`].
pub fn begin_step<W: CollisionWorld + ?Sized>(
    state: &mut PlayerState,
    input: &InputFrame,
    params: &PlayerParams,
    input_world: &W,
    dt: f32,
) -> PendingStep {
    let mut pending = PendingStep {
        before: *state,
        input: input.sanitized(),
        dt: 0.0,
        events: StepEvents::default(),
        mode: PendingMode::NoOp,
    };
    if !(dt.is_finite() && dt > 0.0) {
        return pending;
    }
    if !state.is_finite() {
        pending.events.non_finite_rejected = true;
        pending.mode = PendingMode::Rejected;
        return pending;
    }
    pending.dt = if dt > MAX_STEP_DT {
        pending.events.dt_clamped = true;
        MAX_STEP_DT
    } else {
        dt
    };
    pending.mode = PendingMode::Run;
    if let Some(pawn) = &params.pawn {
        crate::pawn::scripted_input_events(
            state,
            &pending.input,
            params,
            pawn,
            input_world,
            &mut pending.events,
        );
    }
    pending
}

/// Second half of a tick (see [`PendingStep`]): map-actor effects on the
/// player, controller, pawn physics, power jump, rocket boots, grapple gun —
/// against `world`. Returns all events of the tick (those of the input
/// events first). If the tick would leave a non-finite state, the state
/// before [`begin_step`] is restored and only `non_finite_rejected` (and
/// `dt_clamped`) are reported.
pub fn finish_step<M: MovementModel, W: CollisionWorld + ?Sized>(
    model: &M,
    state: &mut PlayerState,
    pending: PendingStep,
    params: &PlayerParams,
    world: &W,
) -> StepEvents {
    let PendingStep {
        before,
        input,
        dt,
        mut events,
        mode,
    } = pending;
    if mode != PendingMode::Run {
        return events;
    }

    if let Some(pawn) = &params.pawn {
        crate::pawn::scripted_actor_ticks(
            model,
            state,
            &input,
            params,
            pawn,
            world,
            dt,
            &mut events,
        );
    } else {
        raw_tick(model, state, &input, params, world, dt, &mut events);
    }

    // 6. Report the net grounded transition.
    if before.grounded == state.grounded {
        events.landed = None;
        events.left_ground = false;
    } else if state.grounded {
        // The impact velocity comes from the movement model.
        events.left_ground = false;
    } else {
        events.landed = None;
        events.left_ground = !events.jumped;
    }

    if !state.is_finite() {
        *state = before;
        return StepEvents {
            non_finite_rejected: true,
            dt_clamped: events.dt_clamped,
            ..StepEvents::default()
        };
    }
    events
}

/// Steps 1-5 of the raw pipeline (module docs).
fn raw_tick<M: MovementModel, W: CollisionWorld + ?Sized>(
    model: &M,
    state: &mut PlayerState,
    input: &InputFrame,
    params: &PlayerParams,
    world: &W,
    dt: f32,
    events: &mut StepEvents,
) {
    // 1. Look. (`max`/`min` instead of `clamp`: `f32::clamp` panics on NaN or
    // inverted bounds, which invalid parameters could produce.)
    let max_pitch = params.camera.max_pitch_degrees.value.to_radians();
    state.yaw = wrap_radians(state.yaw + input.look_yaw_delta);
    state.pitch = (state.pitch + input.look_pitch_delta)
        .max(-max_pitch)
        .min(max_pitch);

    // 2. Grapple input.
    events.grapple = grapple::handle_input(state, input.grapple_held, params, world);

    // 3-4. Locomotion with grapple pull.
    let intent = locomotion_intent(input, state.yaw);
    let pull = grapple::pull_acceleration(state, params);
    model.advance(state, &intent, pull, &params.movement, world, dt, events);

    // 5. Rope constraint and speed cap.
    let rope = grapple::enforce_rope(state, params, world);
    events.rope_correction_blocked = rope.correction_blocked;
    grapple::apply_speed_cap(state, params);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::world::BoxWorld;

    #[test]
    fn bad_dt_is_a_no_op() {
        let params = PlayerParams::default();
        let world = BoxWorld::new();
        let mut s = PlayerState::new(Vec3::new(0.0, 0.0, 100.0), 0.0);
        let before = s;
        let input = InputFrame {
            move_forward: 1.0,
            ..InputFrame::default()
        };
        for dt in [0.0, -1.0, f32::NAN, f32::INFINITY] {
            assert_eq!(
                step(&mut s, &input, &params, &world, dt),
                StepEvents::default()
            );
            assert_eq!(s, before);
        }
    }

    #[test]
    fn look_wraps_yaw_and_clamps_pitch() {
        let params = PlayerParams::default();
        let world = BoxWorld::new();
        let mut s = PlayerState::new(Vec3::ZERO, 3.0);
        let input = InputFrame {
            look_yaw_delta: 0.5,
            look_pitch_delta: 10.0,
            ..InputFrame::default()
        };
        step(&mut s, &input, &params, &world, 1.0 / 60.0);
        assert!((s.yaw - (3.5 - core::f32::consts::TAU)).abs() < 1e-5);
        assert_eq!(s.pitch, params.camera.max_pitch_degrees.value.to_radians());
    }

    #[test]
    fn intent_normalizes_diagonals() {
        let input = InputFrame {
            move_forward: 1.0,
            move_right: 1.0,
            ..InputFrame::default()
        };
        let i = locomotion_intent(&input, 0.0);
        assert!((i.wish_dir - Vec3::new(1.0, 1.0, 0.0).normalize()).length() < 1e-6);
        assert_eq!(i.wish_scale, 1.0);
        let i = locomotion_intent(&InputFrame::default(), 0.0);
        assert_eq!(i.wish_dir, Vec3::ZERO);
        assert_eq!(i.wish_scale, 0.0);
        let half = InputFrame {
            move_right: -0.5,
            ..InputFrame::default()
        };
        let i = locomotion_intent(&half, 0.0);
        assert!((i.wish_dir - Vec3::NEG_Y).length() < 1e-6);
        assert_eq!(i.wish_scale, 0.5);
    }
}
