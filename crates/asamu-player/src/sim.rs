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
//! # Rotator angles (script layer)
//!
//! The original keeps every rotation as integer rotator units (65536 per
//! turn) and turns a rotation into direction vectors through a table of
//! 16384 sines that drops the low two bits of each angle. With the script
//! layer the view angles are therefore integers too: [`apply_look`] decodes
//! the unit a public radian field holds ([`view_units`]), adds the look delta
//! as an integer ([`look_delta_units`]), applies the original's pitch limit
//! ([`limit_view_pitch`]) and stores the exact radian image of the result,
//! so nothing accumulates in floating point. The directions the script layer
//! derives from rotations ([`rotator_axes_flat`], [`rotator_vector`]) use
//! the table ([`angle_table_index`], [`angle_table_entry`]). Evidence:
//! `docs/reverse-engineering/ABILITIES.md` A-WK-1, A-CM-2, A-CM-8 and
//! A-CM-9. The raw pipeline below keeps its own exact-radian arithmetic.
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
use asamu_core::det_math;
use asamu_core::rotator::{
    normalize_rotator_axis, radians_to_rotator_units, rotator_units_to_radians, wrap_radians,
};
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

// ---------------------------------------------------------------------------
// Rotator angles as the original uses them.
// ---------------------------------------------------------------------------

/// Low bits of a rotator angle that the original's sine table ignores.
///
/// NativeCode `FRotator::Vector @ 0x1000FC740` and `UObject::execGetAxes @
/// 0x1000C3C80` (macOS build): each angle is shifted right by this many bits
/// and masked to [`ANGLE_TABLE_ENTRIES`]` − 1` before the lookup, so an angle
/// is floored to a multiple of 4 units (towards −∞, also for negative angles:
/// the shift works on the two's-complement bits) and everything above bit 15
/// is dropped. CONFIRMED (native); measured on the Windows recordings (the
/// table axes of the floored yaw fit 2,349 of 2,349 on-axis accelerations,
/// the yaw truncated towards zero none of the 1,112 with a negative yaw
/// that is no multiple of 4; gated test `tests/parity_recordings.rs`).
pub const ANGLE_TABLE_SHIFT: u32 = 2;

/// Entries of the original's sine table (one per 4 rotator units).
///
/// NativeCode `FGlobalMath::FGlobalMath @ 0x1000FA0A0` (loop bound 0x4000;
/// the Windows build's constructor at `0x5B1E70` is the same). CONFIRMED
/// (native).
pub const ANGLE_TABLE_ENTRIES: u32 = 16_384;

/// A quarter turn in rotator units: the table has no cosines, a cosine is
/// the sine entry of the angle plus this (same two native routines as
/// [`ANGLE_TABLE_SHIFT`], immediate 0x4000). CONFIRMED (native).
pub const QUARTER_TURN_UNITS: i32 = 16_384;

/// The table index the original uses for a rotator angle: bits 2 to 15 of
/// its two's-complement value ([`ANGLE_TABLE_SHIFT`]).
#[must_use]
pub fn angle_table_index(units: i32) -> u16 {
    // The mask keeps 14 bits, so the value always fits.
    ((units as u32 >> ANGLE_TABLE_SHIFT) & (ANGLE_TABLE_ENTRIES - 1)) as u16
}

/// Entry `index` (taken modulo [`ANGLE_TABLE_ENTRIES`]) of the original's
/// sine table.
///
/// The constructor computes, for `i` in `0..16384`, the single-precision
/// sine of the argument `f32(f64(f32(i) · 2) · π · (1 / 16384))`: the index
/// is doubled as an `f32`, widened, multiplied by the `f64` π and by 2⁻¹⁴,
/// and narrowed to `f32` before the sine. That argument rounding is part of
/// the values: the entry for 180° is `sin(f32(π))` = −8.742278e-8, not 0.
/// NativeCode `FGlobalMath::FGlobalMath @ 0x1000FA0A0` (constants π at
/// `0x101645730`, 2⁻¹⁴ at `0x101645740`; the Windows constructor at
/// `0x5B1E70` reads 2.0, π and 2⁻¹⁴ the same way). CONFIRMED (native);
/// measured: with these entries all 2,349 recorded on-axis accelerations
/// are within 8.2e-8 rad of the axis; the exact sine of the floored angle
/// is up to 2.3e-7 rad away, more than 1e-7 on 330 (gated test
/// `tests/parity_recordings.rs`).
///
/// The sine itself is the platform's in the original (`sinf` on macOS,
/// `__libm_sse2_sinf` of the Visual C++ 2010 runtime on Windows, which
/// evaluates in double precision and narrows once); ours is
/// [`det_math::sin`], the same on every platform. That the two agree in the
/// last bit of every entry is STRONG, not CONFIRMED (the game's table has
/// not been read out).
#[must_use]
pub fn angle_table_entry(index: u16) -> f32 {
    let i = u32::from(index) & (ANGLE_TABLE_ENTRIES - 1);
    // Exact: `i` < 2¹⁴ and its double are integers an `f32` holds.
    let doubled = i as f32 + i as f32;
    let argument = (f64::from(doubled)
        * core::f64::consts::PI
        * (1.0 / f64::from(ANGLE_TABLE_ENTRIES))) as f32;
    det_math::sin(argument)
}

/// Sine of a rotator angle as the original looks it up
/// ([`angle_table_entry`] of [`angle_table_index`]).
#[must_use]
pub fn rotator_sin(units: i32) -> f32 {
    angle_table_entry(angle_table_index(units))
}

/// Cosine of a rotator angle as the original looks it up: the sine entry a
/// quarter turn on ([`QUARTER_TURN_UNITS`]).
#[must_use]
pub fn rotator_cos(units: i32) -> f32 {
    angle_table_entry(angle_table_index(units.wrapping_add(QUARTER_TURN_UNITS)))
}

/// The unit vector of a rotation without roll as the original builds it
/// (UnrealScript `vector(rotation)`): `(cos p · cos y, cos p · sin y, sin p)`
/// with table sines and cosines and single-precision products; the result is
/// not normalised again. NativeCode `FRotator::Vector @ 0x1000FC740`, reached
/// from `UObject::execRotatorToVector @ 0x1000C03E0`. CONFIRMED (native).
#[must_use]
pub fn rotator_vector(pitch_units: i32, yaw_units: i32) -> Vec3 {
    let (sp, cp) = (rotator_sin(pitch_units), rotator_cos(pitch_units));
    let (sy, cy) = (rotator_sin(yaw_units), rotator_cos(yaw_units));
    Vec3::new(cp * cy, cp * sy, sp)
}

/// Forward and right axes of a rotation with yaw only, as the original's
/// `GetAxes` builds them: `X = (cos y, sin y, 0)`, `Y = (−sin y, cos y, 0)`
/// with table values. NativeCode `UObject::execGetAxes @ 0x1000C3C80`; with
/// pitch and roll 0 its general rows `X = (cp·cy, cp·sy, sp)`,
/// `Y = (sr·sp·cy − cr·sy, sr·sp·sy + cr·cy, −sr·cp)` reduce to these bit for
/// bit, because the table's entries for 0° and 90° are exactly 0 and 1.
/// CONFIRMED (native).
#[must_use]
pub fn rotator_axes_flat(yaw_units: i32) -> (Vec3, Vec3) {
    let (sy, cy) = (rotator_sin(yaw_units), rotator_cos(yaw_units));
    (Vec3::new(cy, sy, 0.0), Vec3::new(-sy, cy, 0.0))
}

/// Rotator units per radian.
const UNITS_PER_RADIAN: f64 = 65_536.0 / core::f64::consts::TAU;

/// The rotator unit a radian value stands for: the nearest unit, reduced to
/// the low 16 bits as a signed value (`[-32768, 32767]`).
///
/// Every angle the script layer stores is the radian image
/// (`asamu_core::rotator::rotator_units_to_radians`) of such a unit, and this
/// returns that unit exactly; a value written from outside (a teleport, a
/// test) is taken as its nearest unit. The original's view yaw is not
/// reduced (it keeps counting turns), but every reader of it drops the bits
/// above 15 ([`ANGLE_TABLE_SHIFT`]), so the reduction changes nothing.
#[must_use]
pub fn view_units(radians: f32) -> i32 {
    normalize_rotator_axis(radians_to_rotator_units(radians))
}

/// Relative size of the `f32` rounding of a radian value: a look delta
/// within this of a whole number of units is that number
/// ([`look_delta_units`]). A numerical bound of the radian interface
/// (one `f32` ulp), not a value of the game.
pub const LOOK_DELTA_SNAP: f64 = f32::EPSILON as f64;

/// The integer rotator delta of a look input given in radians.
///
/// The original computes the turn of a frame as a float in rotator units
/// and assigns it to the integer rotator: the fraction is dropped, towards
/// zero, every frame (stock `PlayerController.UpdateRotation`, a
/// float-to-int cast in its bytecode; NativeCode `UObject::execFloatToInt @
/// 0x1000B8AB0` truncates). CONFIRMED (src, native); in the recordings every
/// delta is a whole number of units (45,223 of 45,223 ticks).
///
/// Our interface carries radians, which cannot hold a whole number of units
/// exactly, so a value within [`LOOK_DELTA_SNAP`] (relative) of a whole
/// number is that number; anything else is truncated towards zero like the
/// original's cast. Out-of-range values saturate, a non-finite one is 0.
#[must_use]
pub fn look_delta_units(delta_radians: f32) -> i32 {
    let units = f64::from(delta_radians) * UNITS_PER_RADIAN;
    if !units.is_finite() {
        return 0;
    }
    let nearest = units.round();
    let whole = (units - nearest).abs() <= units.abs() * LOOK_DELTA_SNAP;
    // Float → int casts saturate in Rust.
    if whole {
        nearest as i32
    } else {
        units.trunc() as i32
    }
}

/// The original's view pitch limit for a pitch in rotator units.
///
/// The pitch is reduced to `0..=65535`; a value strictly above `max_units`
/// (`ViewPitchMax`) and strictly below `65535 − max_units`
/// (`65535 + ViewPitchMin`, with `ViewPitchMin = −ViewPitchMax`) is moved to
/// the nearer end: to `max_units` below half a turn, else to
/// `65535 − max_units`. Returned as a signed 16-bit value, so the limits are
/// `+max_units` and `−max_units − 1` (the lower one is one unit further out
/// because the rule adds the minimum to 65535, not to 65536): +18000 and
/// −18001 for the original's 18000. Stock `PlayerController.LimitViewRotation`
/// (the controller class does not override it), called for the pawn by
/// `Pawn.ProcessViewRotation`. CONFIRMED (src); the upper limit is measured
/// (50 records at exactly 18000, none above), the lower one is in no
/// recording.
///
/// It is a forbidden band, not a saturating clamp: a delta of more than
/// 162° in one tick can jump across it. Invalid limits (not finite, or not
/// in `0..32768`) give an arbitrary but finite result.
#[must_use]
pub fn limit_view_pitch(pitch_units: i32, max_units: f32) -> i32 {
    let pitch = pitch_units & 0xFFFF;
    // The original compares the integer with the two float limits.
    let value = pitch as f32;
    let lower = 65_535.0 - max_units;
    let limited = if value > max_units && value < lower {
        // Float → int assignment: truncation (saturating in Rust).
        if pitch < 32_768 {
            max_units as i32
        } else {
            lower as i32
        }
    } else {
        pitch
    };
    normalize_rotator_axis(limited)
}

/// `ViewPitchMax` in rotator units, from the camera parameter (degrees):
/// exactly 18000 for the original's value.
fn view_pitch_max_units(params: &PlayerParams) -> f32 {
    (f64::from(params.camera.max_pitch_degrees.value) * (65_536.0 / 360.0)) as f32
}

/// The controller's look update of the script layer, in rotator units
/// (stock `PlayerController.UpdateRotation` → `Pawn.ProcessViewRotation`):
/// the integer delta ([`look_delta_units`]) is added to the yaw and to the
/// pitch, and the pitch is limited ([`limit_view_pitch`]).
///
/// The integers are decoded from and stored back into the public radian
/// fields ([`view_units`] and its exact inverse), so a replay that feeds
/// whole-unit deltas reproduces yaw and pitch exactly on every tick. A field
/// whose unit does not change is left bit for bit as it is (a yaw outside
/// `[-π, π)` is wrapped as before).
pub fn apply_look(state: &mut PlayerState, input: &InputFrame, params: &PlayerParams) {
    let yaw = view_units(state.yaw);
    let new_yaw = normalize_rotator_axis(yaw.wrapping_add(look_delta_units(input.look_yaw_delta)));
    if new_yaw != yaw {
        state.yaw = rotator_units_to_radians(new_yaw);
    } else if !(-core::f32::consts::PI..core::f32::consts::PI).contains(&state.yaw) {
        state.yaw = wrap_radians(state.yaw);
    }
    let pitch = view_units(state.pitch);
    let new_pitch = limit_view_pitch(
        pitch.wrapping_add(look_delta_units(input.look_pitch_delta)),
        view_pitch_max_units(params),
    );
    if new_pitch != pitch {
        state.pitch = rotator_units_to_radians(new_pitch);
    }
}

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

    /// View yaw in rotator units ([`view_units`] of [`Self::yaw`]): with the
    /// script layer the integer the look update works on.
    #[must_use]
    pub fn yaw_units(&self) -> i32 {
        view_units(self.yaw)
    }

    /// View pitch in rotator units ([`view_units`] of [`Self::pitch`]).
    #[must_use]
    pub fn pitch_units(&self) -> i32 {
        view_units(self.pitch)
    }

    /// The direction the original builds from the current view rotation
    /// ([`rotator_vector`]: table angles, each floored to 4 units). What a
    /// fire trace issued by the next tick's input events aims along.
    #[must_use]
    pub fn native_view_direction(&self) -> Vec3 {
        rotator_vector(self.pitch_units(), self.yaw_units())
    }

    /// Aim direction of the script layer during a tick: the vector the
    /// original builds ([`rotator_vector`]) from the camera's cached
    /// point-of-view rotation ([`PawnScript::pov_yaw`], the view as the
    /// previous tick ended; GRAPPLE.md G-TG-1). Without the script layer it
    /// is [`Self::view_direction`].
    #[must_use]
    pub fn aim_direction(&self) -> Vec3 {
        if self.script.started {
            rotator_vector(
                view_units(self.script.pov_pitch),
                view_units(self.script.pov_yaw),
            )
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
