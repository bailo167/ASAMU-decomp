//! The ASAMU pawn/controller script layer on top of the native physics.
//!
//! The original moves the player with the stock native pawn physics
//! ([`crate::ue3_movement`]); everything specific to *A Story About My
//! Uncle* — walking/sprint/story speeds, the jump and its variable height,
//! landings, the story-mode zoom, eye height and view bob, the power jump,
//! the grapple gun and the rocket boots — is UnrealScript in
//! `asamu.ASAMUPawn`, `asamu.ASAMUPlayerController`,
//! `asamu.ASAMUPlayerInput`, `asamu.ASAMUPowerJump`, `asamu.GrappleGun` and
//! `asamu.ASAMURocketBoots`. This module reimplements the pawn/controller
//! part from the behavioural specifications
//! `docs/reverse-engineering/ABILITIES.md` (rule ids A-…) and `GRAPPLE.md`
//! (G-…), written independently in our own words; no script text is
//! reproduced here. The gun and the boots live in [`crate::grapple_gun`] and
//! [`crate::rocket_boots`]; this module calls them in the original's frame
//! order.
//!
//! It runs inside [`crate::sim::step_with`] whenever
//! [`PlayerParams::pawn`] is `Some` (the [`PlayerParams::asamu_original`]
//! set); its run-time state is [`PlayerState::script`] ([`PawnScript`]).
//!
//! # Order within one tick (ABILITIES.md §15, GRAPPLE.md G-TM-1/2)
//!
//! 1. **Input events**, synchronously, before any actor ticks: sprint
//!    press/release, jump release (`ReleaseJump`), jump press (sets the
//!    controller's jump flag and runs the rocket boots' `RocketBoostKeyDown`
//!    at once, A-JP-1/A-RB-2), power-jump key down/up, `use`, then the fire
//!    button (`StopFire`/`StartFire` of the grapple gun; G-IN-5: fire
//!    processing is synchronous inside the input event, so the attach trace
//!    uses the view of the previous tick). The order of several key events
//!    within one tick is UNKNOWN in the original; we use this fixed order.
//!    The fire trace sees the world as the previous tick left it
//!    ([`crate::sim::begin_step`]).
//! 2. **Map actors**: the recharge crystal's refill of the grapple budget
//!    (G-CT-4). The other map actors (movers, crystal state code, attractor
//!    pads) are ticked by `asamu-game` between [`crate::sim::begin_step`]
//!    and [`crate::sim::finish_step`], after the attach's handler calls
//!    reached them; physics and the gun then use the updated world.
//! 3. **Controller**, by controller state:
//!    `PlayerWalking`: acceleration from the move axes along the pawn's yaw
//!    **before** this tick's rotation update (A-WK-1), zero while the
//!    move-input lock is held (A-IL-1); then the look update; then the jump
//!    attempt `DoJump` if the jump flag is set. `Grappling` (attached,
//!    G-PH-1): zero acceleration, look update, jump flag discarded.
//!    `ReleaseGrapple` (the first controller tick after a grapple release,
//!    G-RL-7): no move and no look update, jump flag discarded.
//! 4. **Pawn**: latent state code (jump-release damping, zoom steps) →
//!    native physics (walking, falling or — attached — flying, with the
//!    `Landed` event inside it, see [`crate::movement::PawnHooks`]) → eye
//!    height and view bob (`UpdateEyeHeight`).
//! 5. **Power-jump actor**: its latent state code (charge timer, the jump or
//!    leap itself).
//! 6. **Rocket boots**: their latent state code (charge and boost writes).
//! 7. **Grapple gun**: crosshair, anchor follow, pull and proximity release,
//!    released-flag reset, counter clamp, then its timers (instant release,
//!    refire check). Velocity writes of 5–7 are integrated by the next
//!    tick's physics.
//!
//! Latent `Sleep` follows G-TM-3 (`poll_sleep`): frame-quantised, polled
//! from the tick after it was issued, woken when the remaining time is below
//! half of that tick's `dt`. `GotoState` to a different state clears the
//! state's local variables (G-TM-5) and restarts its code at `Begin` on the
//! next state-code run.
//!
//! # Scope (stage 2)
//!
//! Ported: A-WK-1…4 (acceleration, `GroundSpeed` writers, sprint state
//! machine), A-JP-1…4 (jump, release damping, no double jump), A-AC-1/2
//! (`AirControl` 0.3 → 0.35), the §5 landing table (grapple budget refill,
//! rocket boots re-arm/cancel, `NotLandable` floors from the world's actor
//! tags), A-PJ-1…6 (power jump and leap), A-RB-1…8 (rocket boots,
//! [`crate::rocket_boots`]), A-IL-1 (move-input lock counter), A-ST-1…4
//! (story mode, zoom), A-CM-1…4 (view location, pitch limit, FOV, eye
//! height and walk bob) and the whole grapple of GRAPPLE.md
//! ([`crate::grapple_gun`]: targeting, acceptance, attach, flying physics
//! with the pull, every release path, budget, Kismet events). The
//! controller states `Grappling` (attached: zero acceleration, jump flag
//! discarded, G-PH-1) and `ReleaseGrapple` (one tick without move and look,
//! G-RL-7) follow the gun. Not ported: death fade and checkpoints (§11,
//! `asamu-game` keeps its graybox rules), cinematic mode, hand/jump bob and
//! camera animations (visual only), sounds (reported as events).

use asamu_core::Provenance;
use asamu_core::coords::{ue_forward_flat, ue_right_flat};
use asamu_core::det_math;
use asamu_core::rotator::wrap_radians;
use glam::Vec3;
use serde::{Deserialize, Serialize};

use crate::events::SimEvent;
use crate::grapple_gun::{self, GunState, ReleaseReason};
use crate::input::InputFrame;
use crate::movement::{Landing, LocomotionIntent, MovementModel, PawnHooks};
use crate::params::{PawnParams, PlayerParams};
use crate::rocket_boots::{self, BootsEvent, RocketBoots};
use crate::sim::{PlayerState, StepEvents};
use crate::world::{CollisionShape, CollisionWorld};

// ---------------------------------------------------------------------------
// Literal constants of the original script code (provenance: ScriptCode).
// Values and the rules using them: ABILITIES.md (confidence noted per item).
// ---------------------------------------------------------------------------

/// `asamu.ASAMUPawn`, the class holding most of the script constants.
pub const SCRIPT_CLASS_PAWN: &str = "asamu.ASAMUPawn";

/// Jump-release damping factor applied to `V.z` (A-JP-3).
/// ScriptCode `asamu.ASAMUPawn` state `ReleasedJump`. CONFIRMED (src+bc).
pub const JUMP_RELEASE_MULTIPLIER: f32 = 0.7;
/// Latent sleep between damping steps, s (A-JP-3).
/// ScriptCode `asamu.ASAMUPawn` state `ReleasedJump`. CONFIRMED (src+bc).
pub const JUMP_RELEASE_INTERVAL: f32 = 0.1;
/// The damping loop runs while `V.z` exceeds this, UU/s (A-JP-3).
/// ScriptCode `asamu.ASAMUPawn` state `ReleasedJump`. CONFIRMED (src+bc).
pub const JUMP_RELEASE_MIN_VELOCITY_Z: f32 = 0.05;
/// A normal landing faster than this (`V.z < −200`) resets the eye-smoothing
/// baseline `OldZ` to the landing height (§5). ScriptCode `asamu.ASAMUPawn`
/// event `Landed`. CONFIRMED (src).
pub const LANDED_EYE_RESET_SPEED: f32 = 200.0;
/// Second landing cue below `−MaxFallSpeed × this` (§5). ScriptCode
/// `asamu.ASAMUPawn` event `Landed`. CONFIRMED (src).
pub const LAND_CUE_FALL_SPEED_FACTOR: f32 = 0.5;
/// `HasLanded` returns to `Idle` after this latent sleep, s (cosmetic).
/// ScriptCode `asamu.ASAMUPawn` state `HasLanded`. CONFIRMED (src).
pub const HAS_LANDED_IDLE_DELAY: f32 = 1.0;
/// Zoom step (latent sleep and FOV step fraction `step / zoomDuration`), s
/// (A-ST-4). ScriptCode `asamu.ASAMUPawn` state `Zooming`. CONFIRMED (src+bc).
pub const ZOOM_STEP: f32 = 0.016;
/// Eye-height smoothing `k = min(EYE_SMOOTH_MAX, EYE_SMOOTH_RATE · dt /
/// CustomTimeDilation)` (A-CM-4). ScriptCode `asamu.ASAMUPawn`
/// `UpdateEyeHeight`. CONFIRMED (src).
pub const EYE_SMOOTH_RATE: f32 = 10.0;
/// See [`EYE_SMOOTH_RATE`].
pub const EYE_SMOOTH_MAX: f32 = 0.9;
/// While walking, `EyeHeight ≥ −EYE_MIN_HEIGHT_FACTOR · CollisionHeight`
/// (−22 for the default cylinder; A-CM-4). ScriptCode `asamu.ASAMUPawn`
/// `UpdateEyeHeight`. CONFIRMED (src).
pub const EYE_MIN_HEIGHT_FACTOR: f32 = 0.5;
/// The ceiling probe runs when `CollisionHeight − EyeHeight` is below this,
/// UU (A-CM-4). ScriptCode `asamu.ASAMUPawn` `UpdateEyeHeight`. CONFIRMED (src).
pub const EYE_CEILING_MARGIN: f32 = 12.0;
/// Half-extent of the box swept up by the ceiling probe, UU (A-CM-4).
/// ScriptCode `asamu.ASAMUPawn` `UpdateEyeHeight`. CONFIRMED (src).
pub const EYE_CEILING_PROBE_EXTENT: f32 = 12.0;
/// Below this speed the bob phase advances at [`BOB_IDLE_RATE`] and there is
/// no vertical bob, UU/s (A-CM-4). ScriptCode `asamu.ASAMUPawn`
/// `UpdateEyeHeight`. CONFIRMED (src).
pub const BOB_IDLE_SPEED: f32 = 10.0;
/// Bob phase rate when nearly standing (A-CM-4). ScriptCode
/// `asamu.ASAMUPawn` `UpdateEyeHeight`. CONFIRMED (src).
pub const BOB_IDLE_RATE: f32 = 0.2;
/// Lateral bob `sin(BOB_LATERAL_FREQUENCY · phase)` (A-CM-4). ScriptCode
/// `asamu.ASAMUPawn` `UpdateEyeHeight`. CONFIRMED (src).
pub const BOB_LATERAL_FREQUENCY: f32 = 8.0;
/// Vertical bob `sin(BOB_VERTICAL_FREQUENCY · phase)` (A-CM-4). ScriptCode
/// `asamu.ASAMUPawn` `UpdateEyeHeight`. CONFIRMED (src).
pub const BOB_VERTICAL_FREQUENCY: f32 = 16.0;
/// Vertical bob amplitude factor relative to `Bob` (A-CM-4). ScriptCode
/// `asamu.ASAMUPawn` `UpdateEyeHeight`. CONFIRMED (src).
pub const BOB_VERTICAL_FACTOR: f32 = 0.75;
/// `Bob` is clamped to `±BOB_CLAMP` (A-CM-4). ScriptCode `asamu.ASAMUPawn`
/// `UpdateEyeHeight`. CONFIRMED (src).
pub const BOB_CLAMP: f32 = 0.05;
/// Off the ground the walk bob decays by `1 − min(1, BOB_AIR_DECAY_RATE·dt)`
/// (A-CM-4). ScriptCode `asamu.ASAMUPawn` `UpdateEyeHeight`. CONFIRMED (src).
pub const BOB_AIR_DECAY_RATE: f32 = 8.0;
/// Walk bob scale when `bWeaponBob` is false (A-CM-4). ScriptCode
/// `asamu.ASAMUPawn` `UpdateEyeHeight`. CONFIRMED (src).
pub const BOB_DISABLED_FACTOR: f32 = 0.1;
/// A latent sleep wakes when the remaining time is below this fraction of
/// the tick's `dt` (evaluated in `f64`; G-TM-3). NativeCode
/// `AActor::execPollSleep @ 0x100B35B70` (data 0x1016BC0C0). CONFIRMED.
pub const LATENT_WAKE_FRACTION: f64 = 0.5;

/// One literal constant of the original script code, with its source.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ScriptConstant {
    /// Rust constant name.
    pub name: &'static str,
    /// Value.
    pub value: f64,
    /// Unit.
    pub unit: &'static str,
    /// Script class.
    pub class: &'static str,
    /// Function, event or state holding it.
    pub function: &'static str,
    /// What it does.
    pub meaning: &'static str,
}

impl ScriptConstant {
    /// [`Provenance::ScriptCode`] of the constant.
    #[must_use]
    pub fn provenance(&self) -> Provenance {
        Provenance::ScriptCode {
            class: self.class.to_owned(),
            function: self.function.to_owned(),
        }
    }
}

macro_rules! script_constant {
    ($name:ident, $unit:expr, $function:expr, $meaning:expr) => {
        ScriptConstant {
            name: stringify!($name),
            value: $name as f64,
            unit: $unit,
            class: SCRIPT_CLASS_PAWN,
            function: $function,
            meaning: $meaning,
        }
    };
}

/// Every script-code constant this module uses (for `docs/PARITY.md`).
pub const SCRIPT_CONSTANTS: &[ScriptConstant] = &[
    script_constant!(
        JUMP_RELEASE_MULTIPLIER,
        "factor",
        "ReleasedJump",
        "V.z factor per damping step after the jump key is released"
    ),
    script_constant!(
        JUMP_RELEASE_INTERVAL,
        "s",
        "ReleasedJump",
        "latent sleep between damping steps"
    ),
    script_constant!(
        JUMP_RELEASE_MIN_VELOCITY_Z,
        "uu/s",
        "ReleasedJump",
        "damping continues while V.z exceeds this"
    ),
    script_constant!(
        LANDED_EYE_RESET_SPEED,
        "uu/s",
        "Landed",
        "V.z < -this resets the eye-smoothing baseline at a normal landing"
    ),
    script_constant!(
        LAND_CUE_FALL_SPEED_FACTOR,
        "factor",
        "Landed",
        "second landing cue below -MaxFallSpeed x this"
    ),
    script_constant!(
        HAS_LANDED_IDLE_DELAY,
        "s",
        "HasLanded",
        "HasLanded -> Idle delay (cosmetic)"
    ),
    script_constant!(
        ZOOM_STEP,
        "s",
        "Zooming",
        "zoom step: latent sleep and FOV fraction step/zoomDuration"
    ),
    script_constant!(
        EYE_SMOOTH_RATE,
        "1/s",
        "UpdateEyeHeight",
        "eye smoothing k = min(0.9, 10 dt / CustomTimeDilation)"
    ),
    script_constant!(
        EYE_SMOOTH_MAX,
        "fraction",
        "UpdateEyeHeight",
        "upper bound of the eye smoothing factor"
    ),
    script_constant!(
        EYE_MIN_HEIGHT_FACTOR,
        "factor",
        "UpdateEyeHeight",
        "walking EyeHeight >= -this x CollisionHeight"
    ),
    script_constant!(
        EYE_CEILING_MARGIN,
        "uu",
        "UpdateEyeHeight",
        "ceiling probe when CollisionHeight - EyeHeight < this"
    ),
    script_constant!(
        EYE_CEILING_PROBE_EXTENT,
        "uu",
        "UpdateEyeHeight",
        "half-extent of the ceiling probe box"
    ),
    script_constant!(
        BOB_IDLE_SPEED,
        "uu/s",
        "UpdateEyeHeight",
        "below this speed: idle bob rate, no vertical bob"
    ),
    script_constant!(
        BOB_IDLE_RATE,
        "factor",
        "UpdateEyeHeight",
        "bob phase rate when nearly standing"
    ),
    script_constant!(
        BOB_LATERAL_FREQUENCY,
        "factor",
        "UpdateEyeHeight",
        "lateral bob sin(8 phase)"
    ),
    script_constant!(
        BOB_VERTICAL_FREQUENCY,
        "factor",
        "UpdateEyeHeight",
        "vertical bob sin(16 phase)"
    ),
    script_constant!(
        BOB_VERTICAL_FACTOR,
        "factor",
        "UpdateEyeHeight",
        "vertical bob amplitude 0.75 Bob speed"
    ),
    script_constant!(
        BOB_CLAMP,
        "factor",
        "UpdateEyeHeight",
        "Bob clamped to +-this"
    ),
    script_constant!(
        BOB_AIR_DECAY_RATE,
        "1/s",
        "UpdateEyeHeight",
        "walk-bob decay off the ground"
    ),
    script_constant!(
        BOB_DISABLED_FACTOR,
        "factor",
        "UpdateEyeHeight",
        "walk-bob scale when bWeaponBob is false"
    ),
];

/// Every script-code constant of the script layer: [`SCRIPT_CONSTANTS`]
/// (pawn), [`grapple_gun::GUN_SCRIPT_CONSTANTS`] and
/// [`rocket_boots::BOOTS_SCRIPT_CONSTANTS`].
pub fn all_script_constants() -> impl Iterator<Item = &'static ScriptConstant> {
    SCRIPT_CONSTANTS
        .iter()
        .chain(grapple_gun::GUN_SCRIPT_CONSTANTS)
        .chain(rocket_boots::BOOTS_SCRIPT_CONSTANTS)
}

/// Renders [`all_script_constants`] as a Markdown table (used for
/// `docs/PARITY.md`).
#[must_use]
pub fn script_constants_markdown_table() -> String {
    let mut s =
        String::from("| Constant | Value | Unit | Provenance | Meaning |\n|---|---|---|---|---|\n");
    for c in all_script_constants() {
        // `value` holds an exact f32; print it as the f32 it came from.
        s.push_str(&format!(
            "| `{}` | {} | {} | {} | {} |\n",
            c.name,
            c.value as f32,
            c.unit,
            c.provenance(),
            c.meaning
        ));
    }
    s
}

// ---------------------------------------------------------------------------
// Latent sleep (G-TM-3).
// ---------------------------------------------------------------------------

/// Polls a latent `Sleep`: `remaining` is reduced by `dt`; the sleep ends
/// when the remaining time is below half of `dt` (overshoot discarded).
/// Returns `true` when the sleep ended this tick (`*remaining` is then
/// `None`); `false` while still sleeping or when nothing sleeps.
pub(crate) fn poll_sleep(remaining: &mut Option<f32>, dt: f32) -> bool {
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

/// Advances an actor timer (G-TM-4): `dt` is added to the count (timer
/// dilation 1); the timer fires when the count **strictly exceeds** `rate`.
/// A one-shot timer then disappears (`*count = None`); a looping timer fires
/// `int(count / rate)` times and keeps the remainder. Returns how many
/// times it fired (0 when no timer is set).
pub(crate) fn poll_timer(count: &mut Option<f32>, rate: f32, looping: bool, dt: f32) -> u32 {
    let Some(c) = *count else {
        return 0;
    };
    let c = c + dt;
    let fires = c > rate;
    if !fires {
        *count = Some(c);
        return 0;
    }
    if !looping {
        *count = None;
        return 1;
    }
    let positive_rate = rate > 0.0;
    if !positive_rate {
        *count = Some(0.0);
        return 1;
    }
    // Truncating float → int conversion saturates in Rust.
    let n = ((c / rate) as u32).max(1);
    *count = Some(c - rate * n as f32);
    n
}

// ---------------------------------------------------------------------------
// Pawn script state.
// ---------------------------------------------------------------------------

/// The pawn's script state (`asamu.ASAMUPawn` states; `IdleIdle`/`Running`
/// (animation only) are folded into `Idle`).
///
/// Only `Jumped`, `ReleasedJump`, `StoryState` and `Zooming` have
/// behavioural code, but every other state matters by **not** being
/// `Jumped`/`ReleasedJump`: entering one ends the jump-release damping
/// (ABILITIES.md A-JP-3).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PawnStateName {
    /// `Idle` (auto state).
    #[default]
    Idle,
    /// `Jumped`: a jump was attempted; `ReleaseJump` starts the damping.
    Jumped,
    /// `ReleasedJump`: the `V.z` damping loop (A-JP-3).
    ReleasedJump,
    /// `Shooting`: entered when the grapple attaches (GRAPPLE.md §6; its
    /// code only drives animation). Ends the damping, and a jump-key release
    /// while attached does nothing (G-IX-1).
    Shooting,
    /// `Release`: entered on every grapple release (GRAPPLE.md G-RL-1). Its
    /// code (a cosmetic hand-over to `FallingState` after a second) is not
    /// modelled; behaviourally it is "not `Jumped`".
    Release,
    /// `FallingState` (wind sound; after a damping loop or a power jump).
    FallingState,
    /// `HasLanded` (→ `Idle` after 1 s).
    HasLanded,
    /// `StoryState` (story mode, A-ST-1…3).
    StoryState,
    /// `Zooming`, pushed on top of `StoryState` (A-ST-4).
    Zooming,
}

/// Where the `Zooming` state code continues after a latent sleep.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ZoomLabel {
    /// Inside the zoom-in loop (`i` counts up).
    #[default]
    Zoom,
    /// Holding the zoomed-in FOV.
    Hold,
    /// Inside the zoom-out loop (`i` counts down).
    Exit,
}

/// State-local variables of `Zooming` (cleared on a state change, G-TM-5).
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ZoomLocals {
    /// Resume point after the current sleep.
    pub label: ZoomLabel,
    /// Loop counter.
    pub i: i32,
    /// Key released or zoom disabled: zoom out at the next check.
    pub should_exit: bool,
    /// FOV reached when the direction changed.
    pub actual_fov: f32,
}

/// The pawn's latent state machine.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct PawnCode {
    /// Current state.
    pub state: PawnStateName,
    /// The state code (re)starts at `Begin` at the next state-code run.
    pub begin_pending: bool,
    /// Remaining time of the latent sleep the code waits in, if any.
    pub sleep: Option<f32>,
    /// `Zooming` locals.
    pub zoom: ZoomLocals,
}

impl PawnCode {
    /// `GotoState(new)`.
    pub fn goto(&mut self, new: PawnStateName) {
        if new != self.state {
            self.zoom = ZoomLocals::default();
        }
        self.state = new;
        self.sleep = None;
        self.begin_pending = true;
    }

    /// In `StoryState` or `Zooming` (`IsInState('StoryState')`).
    #[must_use]
    pub fn is_story(&self) -> bool {
        matches!(
            self.state,
            PawnStateName::StoryState | PawnStateName::Zooming
        )
    }
}

/// Sprint flags (A-WK-4).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SprintState {
    /// `bSprinting`: sprint speed applied.
    pub active: bool,
    /// `bSprintAfterLanding`: apply the sprint at the next landing.
    pub armed: bool,
}

/// `ASAMUPowerJump` states (A-PJ; `Unavailable` is never entered).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PowerJumpStateName {
    /// `Ready` (auto state).
    #[default]
    Ready,
    /// `Charging`: the charge timer runs.
    Charging,
    /// `Jumping`: performs the jump or leap at its next state-code run.
    Jumping,
    /// `Canceled`: back to `Ready` at its next state-code run.
    Canceled,
}

/// The power-jump actor's latent state machine.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct PowerJump {
    /// Current state.
    pub state: PowerJumpStateName,
    /// The state code (re)starts at `Begin` at the next state-code run.
    pub begin_pending: bool,
    /// Remaining time of the latent sleep, if any.
    pub sleep: Option<f32>,
    /// `Charging` local `bDoneCharging`.
    pub charged: bool,
}

impl PowerJump {
    /// `GotoState(new)` of the power-jump actor.
    pub fn goto(&mut self, new: PowerJumpStateName) {
        if new != self.state {
            self.charged = false;
        }
        self.state = new;
        self.sleep = None;
        self.begin_pending = true;
    }

    /// `CancelPowerJump`: → `Canceled`.
    pub fn cancel(&mut self) {
        self.goto(PowerJumpStateName::Canceled);
    }
}

/// Run-time state of the ASAMU script layer ([`PlayerState::script`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct PawnScript {
    /// Pawn start (`PostBeginPlay`) has run; until then the layer is unused.
    pub started: bool,
    /// Run-time `GroundSpeed`, UU/s (A-WK-3).
    pub ground_speed: f32,
    /// Run-time `AirControl` (A-AC-2).
    pub air_control: f32,
    /// Run-time `JumpZ`, UU/s.
    pub jump_z: f32,
    /// The pawn's script state.
    pub code: PawnCode,
    /// Sprint flags.
    pub sprint: SprintState,
    /// Move-input lock counter (A-IL-1).
    pub move_input_lock: u8,
    /// `bZoomEnabled` at run time.
    pub zoom_enabled: bool,
    /// Camera FOV (locked by the `FOV` command; changed by the zoom), degrees.
    pub fov: f32,
    /// Run-time `EyeHeight`, UU above the collision centre.
    pub eye_height: f32,
    /// Bob phase `BobTime`.
    pub bob_time: f32,
    /// `WalkBob` view offset, UU.
    pub walk_bob: Vec3,
    /// `OldZ`: collision-centre height before this tick's physics (or the
    /// landing height after a fast landing).
    pub old_z: f32,
    /// The power-jump actor.
    pub power_jump: PowerJump,
    /// Jump key level of the previous tick (release edge).
    pub jump_was_held: bool,
    /// Sprint key level of the previous tick.
    pub sprint_was_held: bool,
    /// Power-jump key level of the previous tick.
    pub power_jump_was_held: bool,
    /// The controller is in `ReleaseGrapple` (GRAPPLE.md G-RL-7): set by a
    /// grapple release, consumed by the next controller tick, which then
    /// runs no move and no look update and drops the jump flag.
    #[serde(default)]
    pub release_gap: bool,
    /// Run-time `AirSpeed` (the flying speed cap), UU/s: the class default
    /// until the grapple gun writes `fGrappleAccel` (G-PH-4).
    #[serde(default)]
    pub air_speed: f32,
    /// The grapple gun ([`crate::grapple_gun`]).
    #[serde(default)]
    pub gun: GunState,
    /// The rocket boots ([`crate::rocket_boots`]).
    #[serde(default)]
    pub boots: RocketBoots,
    /// Yaw of the camera's cached point of view (`CameraCache.POV`), radians:
    /// the view rotation as the previous tick ended. Every aim of the script
    /// layer reads it (fire trace, refire, crosshair, boost aim; GRAPPLE.md
    /// G-TG-1) because the camera updates once per frame after all actors
    /// ticked. Recorded when a tick begins ([`crate::sim::begin_step`]).
    #[serde(default)]
    pub pov_yaw: f32,
    /// Pitch of the camera's cached point of view, radians (see
    /// [`Self::pov_yaw`]).
    #[serde(default)]
    pub pov_pitch: f32,
}

impl PawnScript {
    /// In story mode (`StoryState` or `Zooming`).
    #[must_use]
    pub fn is_story(&self) -> bool {
        self.code.is_story()
    }

    /// View offset from the collision centre: `(0, 0, EyeHeight) + WalkBob`
    /// (A-CM-1).
    #[must_use]
    pub fn view_offset(&self) -> Vec3 {
        Vec3::new(0.0, 0.0, self.eye_height) + self.walk_bob
    }

    /// `true` if every float is finite.
    #[must_use]
    pub fn is_finite(&self) -> bool {
        self.ground_speed.is_finite()
            && self.air_control.is_finite()
            && self.jump_z.is_finite()
            && self.fov.is_finite()
            && self.eye_height.is_finite()
            && self.bob_time.is_finite()
            && self.walk_bob.is_finite()
            && self.old_z.is_finite()
            && self.code.sleep.is_none_or(f32::is_finite)
            && self.code.zoom.actual_fov.is_finite()
            && self.power_jump.sleep.is_none_or(f32::is_finite)
            && self.air_speed.is_finite()
            && self.gun.is_finite()
            && self.boots.is_finite()
            && self.pov_yaw.is_finite()
            && self.pov_pitch.is_finite()
    }

    fn apply_sprint_speed(&mut self, pawn: &PawnParams) {
        self.ground_speed = pawn.move_speed.value * pawn.sprint_speed_multiplier.value;
        self.sprint.active = true;
    }

    fn remove_sprint_speed(&mut self, pawn: &PawnParams) {
        self.ground_speed = pawn.move_speed.value;
        self.sprint.active = false;
    }

    /// `EnableInput(true)` / `IgnoreMoveInput(false)`: one lock released.
    pub(crate) fn release_move_input(&mut self) {
        self.move_input_lock = self.move_input_lock.saturating_sub(1);
    }
}

// ---------------------------------------------------------------------------
// Events.
// ---------------------------------------------------------------------------

/// Which `Landed` handler ran.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LandingHandler {
    /// `ASAMUPawn.Landed`.
    Normal,
    /// `StoryState.Landed`.
    Story,
    /// The floor is tagged `NotLandable`: the normal handler returned at once.
    NotLandable,
}

/// The extra landing sound cue (§5).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LandCue {
    /// `V.z < −MaxFallSpeed`: the "falling damage" landing cue (sound only;
    /// there is no falling damage).
    FallingDamage,
    /// `V.z < −MaxFallSpeed/2`: the heavier landing cue.
    Heavy,
}

/// What a landing did (§5 decision table).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct LandingOutcome {
    /// Handler that ran.
    pub handler: LandingHandler,
    /// Landing `V.z`, UU/s.
    pub velocity_z: f32,
    /// Hard landing (`V.z < −hardLandingThreshold`, normal handler only):
    /// camera animation, rumble, particle and decal — cosmetic.
    pub hard: bool,
    /// The landing sound (and grunt, Kismet `SeqEvent_PlayerLanded`) plays.
    pub sound: bool,
    /// Extra landing cue, if any.
    pub cue: Option<LandCue>,
    /// The armed sprint was applied.
    pub sprint_applied: bool,
    /// The grapple budget was refilled (both handlers; not on `NotLandable`
    /// floors, G-CT-4).
    #[serde(default)]
    pub grapples_refilled: bool,
    /// A running rocket boost was cancelled (A-RB-6).
    #[serde(default)]
    pub boost_canceled: bool,
}

/// Power-jump events.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum PowerJumpEvent {
    /// The charge completed (hand light on).
    Charged,
    /// Released too early or while not walking.
    Canceled,
    /// The power jump (`leap: false`) or power leap (`leap: true`) ran;
    /// `jumped` tells whether the inner jump attempt succeeded.
    Fired {
        /// Power leap instead of the vertical power jump.
        leap: bool,
        /// The pawn was walking, so the jump attempt succeeded.
        jumped: bool,
    },
}

// ---------------------------------------------------------------------------
// Public script entry points (Kismet / console equivalents).
// ---------------------------------------------------------------------------

/// Pawn start (`PostBeginPlay`, A-WK-3 "pawn start"): `GroundSpeed =
/// MoveSpeed`, not sprinting, power jump `Ready`, `EyeHeight` and FOV from
/// the defaults/settings, rocket boots spawned (disabled), grapple gun
/// spawned (capacity 0, latch set, `AirSpeed` := `fGrappleAccel`). No-op
/// when `params.pawn` is `None`.
pub fn start(state: &mut PlayerState, params: &PlayerParams) {
    let Some(pawn) = &params.pawn else {
        return;
    };
    let m = &params.movement;
    state.script = PawnScript {
        started: true,
        ground_speed: pawn.move_speed.value,
        air_control: m.air_control.value,
        jump_z: m.jump_velocity.value,
        code: PawnCode {
            state: PawnStateName::Idle,
            begin_pending: true,
            ..PawnCode::default()
        },
        sprint: SprintState::default(),
        move_input_lock: 0,
        zoom_enabled: pawn.zoom_enabled.value,
        fov: params.camera.fov_degrees.value,
        // `EyeHeight` and `BaseEyeHeight` defaults are both 38 (UTPawn CDO).
        eye_height: params.camera.eye_height.value,
        bob_time: 0.0,
        walk_bob: Vec3::ZERO,
        old_z: state.position.z,
        power_jump: PowerJump::default(),
        jump_was_held: false,
        sprint_was_held: false,
        power_jump_was_held: false,
        release_gap: false,
        air_speed: m.air_speed.value,
        gun: GunState::default(),
        boots: RocketBoots::default(),
        pov_yaw: state.yaw,
        pov_pitch: state.pitch,
    };
    // The pawn spawns its rocket boots, the game gives it the grapple gun
    // (whose spawn writes `AirSpeed`, G-PH-4).
    rocket_boots::spawn_for(&mut state.script.boots, params);
    grapple_gun::spawn_for(&mut state.script, params);
}

/// The common grapple release (GRAPPLE.md G-RL-1) from outside the tick
/// (death, story mode; G-RL-5): physics → Falling, pawn → `Release` (ends
/// the jump-release damping, A-JP-3), controller → `ReleaseGrapple` (the
/// next controller tick has no move, no look and drops the jump flag,
/// G-RL-7), Kismet `SeqEvent_PlayerReleasedGrapple`. Velocity is untouched.
/// Returns the events raised (nothing when not attached: the original
/// ignores a release then).
pub fn release_grapple(state: &mut PlayerState, reason: ReleaseReason) -> StepEvents {
    grapple_gun::release_from_outside(state, reason)
}

/// `EnterStoryState` (A-ST-1; Kismet `SeqAct_ToggleStoryMode`): stop
/// sprinting (outside story mode only), release the grapple, story speed,
/// cancel the power jump, → `StoryState` (also from `Zooming`, which leaves
/// the FOV where it was). Returns the events of the grapple release.
pub fn enter_story_mode(state: &mut PlayerState, params: &PlayerParams) -> StepEvents {
    let Some(pawn) = &params.pawn else {
        return StepEvents::default();
    };
    if !state.script.started {
        start(state, params);
    }
    if !state.script.is_story() {
        // TryToStopSprinting (empty inside story mode).
        state.script.remove_sprint_speed(pawn);
        state.script.sprint.armed = false;
    }
    // Releases an attached grapple (physics Falling, pawn → `Release`,
    // controller → `ReleaseGrapple`); the state change below overrides
    // `Release`.
    let events = release_grapple(state, ReleaseReason::Story);
    let s = &mut state.script;
    s.ground_speed = pawn.move_speed.value * pawn.story_speed_multiplier.value;
    s.power_jump.cancel();
    s.code.goto(PawnStateName::StoryState);
    events
}

/// `ExitStoryState` (A-ST-3): `GroundSpeed = MoveSpeed`, the grapple hand
/// shown (which clears the "hand hidden" flag, G-AC-3), → `Idle`; from
/// `Zooming` the settings FOV is restored first. No effect outside story
/// mode.
pub fn exit_story_mode(state: &mut PlayerState, params: &PlayerParams) {
    let Some(pawn) = &params.pawn else {
        return;
    };
    let s = &mut state.script;
    match s.code.state {
        PawnStateName::StoryState => {}
        PawnStateName::Zooming => s.fov = params.camera.fov_degrees.value,
        _ => return,
    }
    s.ground_speed = pawn.move_speed.value;
    grapple_gun::hide_grapple_gun(state, false, true, true);
    state.script.code.goto(PawnStateName::Idle);
}

/// `ToggleZoomAvailable` (Kismet `SeqAct_ToggleZoomAvailable`); disabling it
/// while zoomed zooms out.
pub fn set_zoom_available(state: &mut PlayerState, enabled: bool) {
    let s = &mut state.script;
    s.zoom_enabled = enabled;
    if !enabled && s.code.state == PawnStateName::Zooming {
        s.code.zoom.should_exit = true;
    }
}

/// The pawn's `Landed` event (§5 decision table) for `landing`. Called by
/// the movement model at the moment the native `processLanded` calls it.
pub fn on_landed(
    script: &mut PawnScript,
    params: &PlayerParams,
    pawn: &PawnParams,
    landing: &Landing,
) -> LandingOutcome {
    let vz = landing.velocity.z;
    let max_fall = params.movement.max_fall_speed.value;
    let sound = vz <= -pawn.land_sound_threshold.value;
    let cue = if !sound {
        None
    } else if vz < -max_fall {
        Some(LandCue::FallingDamage)
    } else if vz < max_fall * -LAND_CUE_FALL_SPEED_FACTOR {
        Some(LandCue::Heavy)
    } else {
        None
    };
    if script.is_story() {
        // StoryState.Landed: no NotLandable check, no state change, no
        // AirControl reset, no eye baseline; the budget is refilled, the
        // boots re-armed (or a boost cancelled), the armed sprint is applied
        // but stays armed.
        script.gun.times_grappled = 0;
        let boost_canceled =
            rocket_boots::player_landed(&mut script.boots) == Some(BootsEvent::Canceled);
        script.release_move_input();
        let sprint_applied = script.sprint.armed;
        if sprint_applied {
            script.apply_sprint_speed(pawn);
        }
        return LandingOutcome {
            handler: LandingHandler::Story,
            velocity_z: vz,
            hard: false,
            sound,
            cue,
            sprint_applied,
            grapples_refilled: true,
            boost_canceled,
        };
    }
    if landing.not_landable {
        return LandingOutcome {
            handler: LandingHandler::NotLandable,
            velocity_z: vz,
            hard: false,
            sound: false,
            cue: None,
            sprint_applied: false,
            grapples_refilled: false,
            boost_canceled: false,
        };
    }
    let hard = vz < -pawn.hard_landing_threshold.value;
    // The gun's `PlayerLanded` refills the budget (G-CT-4); the power jump's
    // landing hook does nothing in practice (§6); the boots re-arm or cancel
    // a boost (A-RB-5/6); then one move-input lock is released.
    script.gun.times_grappled = 0;
    let boost_canceled =
        rocket_boots::player_landed(&mut script.boots) == Some(BootsEvent::Canceled);
    script.release_move_input();
    let sprint_applied = script.sprint.armed;
    if sprint_applied {
        script.apply_sprint_speed(pawn);
    }
    script.sprint.armed = false;
    script.code.goto(PawnStateName::HasLanded);
    // No falling damage: `TakeFallingDamage` is overridden with nothing and
    // the stock horizontal ×0.1 landing slowdown is disabled (A-JP / §5).
    if vz < -LANDED_EYE_RESET_SPEED {
        script.old_z = landing.location.z;
    }
    script.air_control = pawn.landed_air_control.value;
    // `SetBaseEyeheight` (when the sound plays) re-sets BaseEyeHeight to its
    // default, which nothing changes: no effect.
    LandingOutcome {
        handler: LandingHandler::Normal,
        velocity_z: vz,
        hard,
        sound,
        cue,
        sprint_applied,
        grapples_refilled: true,
        boost_canceled,
    }
}

// ---------------------------------------------------------------------------
// The scripted tick.
// ---------------------------------------------------------------------------

struct ScriptHooks<'a> {
    script: &'a mut PawnScript,
    params: &'a PlayerParams,
    pawn: &'a PawnParams,
    outcome: Option<LandingOutcome>,
}

impl PawnHooks for ScriptHooks<'_> {
    fn ground_speed(&self) -> f32 {
        self.script.ground_speed
    }

    fn air_control(&self) -> f32 {
        self.script.air_control
    }

    fn air_speed(&self) -> f32 {
        self.script.air_speed
    }

    fn landed(&mut self, landing: &Landing) {
        let outcome = on_landed(self.script, self.params, self.pawn, landing);
        if self.outcome.is_none() {
            self.outcome = Some(outcome);
        }
    }
}

/// Physics is `Walking` (grounded; a grappling pawn flies).
fn is_walking(state: &PlayerState) -> bool {
    state.grounded && !state.pawn.flying
}

/// `ASAMUPawn.DoJump` (A-JP-2). Returns whether the jump succeeded.
fn do_jump(state: &mut PlayerState, pawn: &PawnParams, events: &mut StepEvents) -> bool {
    let s = &mut state.script;
    if s.is_story() {
        return false; // overridden with an empty function in story mode
    }
    if s.sprint.active {
        s.remove_sprint_speed(pawn);
        s.sprint.armed = true;
    }
    s.code.goto(PawnStateName::Jumped);
    if !is_walking(state) {
        return false;
    }
    state.velocity.z = state.script.jump_z;
    // setPhysics(Falling) drops the base.
    state.grounded = false;
    state.pawn.based = false;
    events.jumped = true;
    true
}

/// The input events of one tick (step 1 of the module docs).
fn input_events(
    state: &mut PlayerState,
    input: &InputFrame,
    pawn: &PawnParams,
    events: &mut StepEvents,
) {
    let walking = is_walking(state);
    let flying_or_falling = !walking;
    let s = &mut state.script;

    // Sprint (`StartSprinting` / `StopSprinting`; empty in story mode).
    if input.sprint_held != s.sprint_was_held && !s.is_story() {
        if input.sprint_held {
            if flying_or_falling {
                s.sprint.armed = true;
            } else {
                s.apply_sprint_speed(pawn);
            }
        } else {
            s.remove_sprint_speed(pawn);
            s.sprint.armed = false;
        }
    }
    s.sprint_was_held = input.sprint_held;

    // `ReleaseJump` does something only in `Jumped`.
    if s.jump_was_held && !input.jump_held && s.code.state == PawnStateName::Jumped {
        s.code.goto(PawnStateName::ReleasedJump);
    }
    s.jump_was_held = input.jump_held;

    // The jump key's press: `Jump` only sets the controller's flag (read by
    // the controller move below); `RocketBoostKeyDown` runs at once (A-RB-2).
    if input.jump_pressed
        && let Some(e) = rocket_boots::boost_key(state)
    {
        events.boots = Some(e);
    }
    let s = &mut state.script;

    // Power-jump key (`PowerJumpKeyDown` / `PowerJumpKeyUp`).
    if input.power_jump_held && !s.power_jump_was_held {
        match s.code.state {
            PawnStateName::StoryState => {
                if s.zoom_enabled {
                    // PushState('Zooming').
                    s.code.state = PawnStateName::Zooming;
                    s.code.zoom = ZoomLocals::default();
                    s.code.sleep = None;
                    s.code.begin_pending = true;
                }
            }
            PawnStateName::Zooming => {}
            _ => {
                if s.power_jump.state == PowerJumpStateName::Ready {
                    s.power_jump.goto(PowerJumpStateName::Charging);
                }
            }
        }
    } else if !input.power_jump_held && s.power_jump_was_held {
        match s.code.state {
            PawnStateName::StoryState => {}
            PawnStateName::Zooming => s.code.zoom.should_exit = true,
            _ => {
                if s.power_jump.state == PowerJumpStateName::Charging {
                    if s.power_jump.charged && walking {
                        s.power_jump.goto(PowerJumpStateName::Jumping);
                    } else {
                        s.power_jump.cancel();
                        events.power_jump = Some(PowerJumpEvent::Canceled);
                    }
                }
            }
        }
    }
    s.power_jump_was_held = input.power_jump_held;

    // `use` only does something in story mode.
    if input.use_pressed && s.is_story() {
        events.use_requested = true;
    }
}

/// Runs the pawn's latent state code (before physics).
fn run_pawn_code(state: &mut PlayerState, params: &PlayerParams, pawn: &PawnParams, dt: f32) {
    let s = &mut state.script;
    let woke = poll_sleep(&mut s.code.sleep, dt);
    if !woke && (s.code.sleep.is_some() || !s.code.begin_pending) {
        return;
    }
    let begin = !woke;
    s.code.begin_pending = false;
    match s.code.state {
        PawnStateName::ReleasedJump => {
            // while (V.z > 0.05) { V.z *= 0.7; Sleep(0.1); } → FallingState
            if state.velocity.z > JUMP_RELEASE_MIN_VELOCITY_Z {
                state.velocity.z *= JUMP_RELEASE_MULTIPLIER;
                s.code.sleep = Some(JUMP_RELEASE_INTERVAL);
            } else {
                s.code.goto(PawnStateName::FallingState);
            }
        }
        PawnStateName::HasLanded => {
            if begin {
                s.code.sleep = Some(HAS_LANDED_IDLE_DELAY);
            } else {
                s.code.goto(PawnStateName::Idle);
            }
        }
        PawnStateName::Zooming => run_zoom_code(s, params, pawn, begin),
        // Animation/sound-only state code.
        PawnStateName::Idle
        | PawnStateName::Jumped
        | PawnStateName::Shooting
        | PawnStateName::Release
        | PawnStateName::FallingState
        | PawnStateName::StoryState => {}
    }
}

/// The `Zooming` state code (A-ST-4). `B` = settings FOV, `Z` = `zoomFOV`,
/// `T` = `zoomDuration`, `h` = [`ZOOM_STEP`]; FOV steps
/// `B − (B − target)·(h/T)·i`, one per latent sleep of `h`.
fn run_zoom_code(s: &mut PawnScript, params: &PlayerParams, pawn: &PawnParams, begin: bool) {
    #[derive(Clone, Copy)]
    enum Pc {
        Zoom,
        Hold,
        Exit,
    }
    let base = params.camera.fov_degrees.value;
    let zoom_fov = pawn.zoom_fov.value;
    let duration = pawn.zoom_duration.value;
    let steps = duration / ZOOM_STEP;
    let fraction = ZOOM_STEP / duration;
    // `int(T / h)`: float → int truncates (saturating in Rust).
    let exit_start = steps as i32;
    let z = &mut s.code.zoom;
    let mut pc = if begin {
        z.should_exit = false;
        z.i = 0;
        Pc::Zoom
    } else {
        match z.label {
            ZoomLabel::Zoom => {
                z.i = z.i.saturating_add(1);
                Pc::Zoom
            }
            ZoomLabel::Hold => Pc::Hold,
            ZoomLabel::Exit => {
                z.i = z.i.saturating_sub(1);
                Pc::Exit
            }
        }
    };
    // Every path ends in a sleep or the pop; the bound only guards against
    // pathological parameters.
    for _ in 0..8 {
        match pc {
            Pc::Zoom => {
                if (z.i as f32) < steps {
                    s.fov = base - (base - zoom_fov) * fraction * z.i as f32;
                    if z.should_exit {
                        z.actual_fov = s.fov;
                        z.i = exit_start;
                        pc = Pc::Exit;
                        continue;
                    }
                    z.label = ZoomLabel::Zoom;
                    s.code.sleep = Some(ZOOM_STEP);
                    return;
                }
                s.fov = zoom_fov;
                pc = Pc::Hold;
            }
            Pc::Hold => {
                if z.should_exit {
                    z.actual_fov = s.fov;
                    z.i = exit_start;
                    pc = Pc::Exit;
                    continue;
                }
                z.label = ZoomLabel::Hold;
                s.code.sleep = Some(ZOOM_STEP);
                return;
            }
            Pc::Exit => {
                if z.i > 0 {
                    s.fov = base - (base - z.actual_fov) * fraction * z.i as f32;
                    if !z.should_exit {
                        z.actual_fov = s.fov;
                        z.i = 0;
                        pc = Pc::Zoom;
                        continue;
                    }
                    z.label = ZoomLabel::Exit;
                    s.code.sleep = Some(ZOOM_STEP);
                    return;
                }
                s.fov = base;
                // PopState → StoryState (whose code has already finished).
                s.code.state = PawnStateName::StoryState;
                s.code.sleep = None;
                s.code.begin_pending = false;
                s.code.zoom = ZoomLocals::default();
                return;
            }
        }
    }
}

/// `UpdateEyeHeight` (A-CM-4), after physics.
fn update_eye_height<W: CollisionWorld + ?Sized>(
    state: &mut PlayerState,
    params: &PlayerParams,
    pawn: &PawnParams,
    world: &W,
    dt: f32,
) {
    let walking = is_walking(state);
    let base = params.camera.eye_height.value;
    let collision_height = params.movement.capsule_half_height.value;
    let s = &mut state.script;
    let k = (EYE_SMOOTH_RATE * dt / pawn.custom_time_dilation.value).min(EYE_SMOOTH_MAX);
    if walking {
        // Steps are absorbed (eye − ΔZ) and relaxed back to the base height.
        s.eye_height = ((s.eye_height - state.position.z + s.old_z) * (1.0 - k) + base * k)
            .max(-EYE_MIN_HEIGHT_FACTOR * collision_height);
    } else {
        s.eye_height = s.eye_height * (1.0 - k) + base * k;
    }

    // Walk bob (the stock landing-dip branches never activate for ASAMU).
    let bob = pawn.bob.value.clamp(-BOB_CLAMP, BOB_CLAMP);
    if walking {
        let speed = state.velocity.length();
        if speed < BOB_IDLE_SPEED {
            s.bob_time += BOB_IDLE_RATE * dt;
        } else {
            let rate = if s.is_story() {
                pawn.bob_rate_story.value
            } else if s.sprint.active {
                pawn.bob_rate_sprint.value
            } else {
                pawn.bob_rate_walk.value
            };
            s.bob_time += dt * rate;
        }
        // Pawn right axis (pitch is 0 while walking).
        let right = ue_right_flat(state.yaw);
        let mut walk_bob = right * bob * speed * det_math::sin(BOB_LATERAL_FREQUENCY * s.bob_time);
        // The stock additive vertical term is never set, so it stays 0.
        walk_bob.z = 0.0;
        if speed > BOB_IDLE_SPEED {
            walk_bob.z += BOB_VERTICAL_FACTOR
                * bob
                * speed
                * det_math::sin(BOB_VERTICAL_FREQUENCY * s.bob_time);
        }
        s.walk_bob = walk_bob;
    } else {
        s.bob_time = 0.0;
        s.walk_bob *= 1.0 - (BOB_AIR_DECAY_RATE * dt).min(1.0);
    }
    if !pawn.weapon_bob.value {
        s.walk_bob *= BOB_DISABLED_FACTOR;
    }

    // Ceiling probe: keep the view point out of the geometry above.
    if collision_height - s.eye_height < EYE_CEILING_MARGIN {
        let step = params.movement.step_height.value;
        let start = state.position + s.walk_bob;
        let end = start + Vec3::Z * (step + collision_height);
        // The original sweeps a box of half-extent 12; our worlds sweep
        // upright cylinders (radius and half-height 12) — same for the
        // vertical contact with horizontal ceilings.
        let probe = CollisionShape {
            radius: EYE_CEILING_PROBE_EXTENT,
            half_height: EYE_CEILING_PROBE_EXTENT,
        };
        let max_eye = match world.sweep_capsule(start, end, probe) {
            Some(hit) => hit.position.z - state.position.z,
            None => collision_height + step,
        };
        s.eye_height = s.eye_height.min(max_eye);
    }
}

/// Runs the power-jump actor's latent state code (after the pawn's tick).
fn run_power_jump_code(
    state: &mut PlayerState,
    pawn: &PawnParams,
    dt: f32,
    events: &mut StepEvents,
) {
    let pj = &mut state.script.power_jump;
    let woke = poll_sleep(&mut pj.sleep, dt);
    if !woke && (pj.sleep.is_some() || !pj.begin_pending) {
        return;
    }
    pj.begin_pending = false;
    match pj.state {
        PowerJumpStateName::Ready => {}
        PowerJumpStateName::Charging => {
            if woke {
                pj.charged = true;
                events.power_jump = Some(PowerJumpEvent::Charged);
            } else {
                pj.charged = false;
                pj.sleep = Some(pawn.power_jump_charge_time.value);
            }
        }
        PowerJumpStateName::Canceled => pj.goto(PowerJumpStateName::Ready),
        PowerJumpStateName::Jumping => {
            let old_jump_z = state.script.jump_z;
            let v = state.velocity;
            let leap = state.script.sprint.active
                && (v.x.abs() > 0.0 || v.y.abs() > 0.0 || v.z.abs() > 0.0);
            let jumped = if leap {
                state.script.move_input_lock = state.script.move_input_lock.saturating_add(1);
                let jumped = do_jump(state, pawn, events);
                let m = pawn.power_leap_horizontal_multiplier.value;
                state.velocity.x *= m;
                state.velocity.y *= m;
                state.velocity.z = pawn.power_leap_vertical_strength.value;
                jumped
            } else {
                state.script.jump_z = pawn.power_jump_strength.value;
                let jumped = do_jump(state, pawn, events);
                state.script.jump_z = old_jump_z;
                jumped
            };
            state.script.jump_z = old_jump_z;
            state.script.code.goto(PawnStateName::FallingState);
            state.script.power_jump.goto(PowerJumpStateName::Ready);
            events.power_jump = Some(PowerJumpEvent::Fired { leap, jumped });
        }
    }
}

/// The fire button's input events (synchronous, before the actor ticks,
/// GRAPPLE.md G-IN-5): press and release edges of the button level, handed
/// to the grapple gun ([`grapple_gun::fire_button`]).
fn fire_input_event<W: CollisionWorld + ?Sized>(
    state: &mut PlayerState,
    input: &InputFrame,
    params: &PlayerParams,
    world: &W,
    events: &mut StepEvents,
) {
    let pressed = input.grapple_held && !state.grapple_was_held;
    let released = !input.grapple_held && state.grapple_was_held;
    state.grapple_was_held = input.grapple_held;
    grapple_gun::fire_button(state, pressed, released, params, world, events);
}

/// The controller's per-tick move (`PlayerTick` → the state's `PlayerMove`)
/// for its three states, after the input events. Returns the acceleration
/// direction and the input magnitude (ignored by the native port).
///
/// - `ReleaseGrapple` (one tick after a release, G-RL-7): no move and no look
///   update (the pawn keeps the zero acceleration of `Grappling`); entering
///   `PlayerWalking` at the end of the tick clears the jump flag.
/// - `Grappling` (attached, G-PH-1): zero acceleration, look, jump flag
///   discarded.
/// - `PlayerWalking` (A-WK-1, A-JP-1): acceleration from the move axes
///   (zeroed under the move-input lock, A-IL-1) along the pawn yaw **before**
///   this tick's look update, then the look update, then the jump attempt.
fn controller_move(
    state: &mut PlayerState,
    input: &InputFrame,
    params: &PlayerParams,
    pawn: &PawnParams,
    events: &mut StepEvents,
) -> (Vec3, f32) {
    if state.script.release_gap {
        state.script.release_gap = false;
        return (Vec3::ZERO, 0.0);
    }
    let attached = state.script.gun.is_attached();
    let (forward, right) = if attached || state.script.move_input_lock > 0 {
        (0.0, 0.0)
    } else {
        (input.move_forward, input.move_right)
    };
    let pawn_yaw = state.yaw;
    let wish = ue_forward_flat(pawn_yaw) * forward + ue_right_flat(pawn_yaw) * right;
    let wish_dir = wish.normalize_or_zero();
    let max_pitch = params.camera.max_pitch_degrees.value.to_radians();
    state.yaw = wrap_radians(state.yaw + input.look_yaw_delta);
    state.pitch = (state.pitch + input.look_pitch_delta)
        .max(-max_pitch)
        .min(max_pitch);
    if input.jump_pressed && !attached {
        do_jump(state, pawn, events);
    }
    let scale = if wish_dir == Vec3::ZERO {
        0.0
    } else {
        glam::Vec2::new(forward, right).length().min(1.0)
    };
    (wish_dir, scale)
}

/// Step 1 of a scripted tick (see the module docs for the order): the input
/// events, against the world as the previous tick left it (`input_world`).
/// `input` is already sanitized; the caller handles the guards.
pub(crate) fn scripted_input_events<W: CollisionWorld + ?Sized>(
    state: &mut PlayerState,
    input: &InputFrame,
    params: &PlayerParams,
    pawn: &PawnParams,
    input_world: &W,
    events: &mut StepEvents,
) {
    if !state.script.started {
        start(state, params);
    }
    // The camera updated after every actor of the previous tick: its cached
    // point of view holds the view rotation as that tick ended (G-TG-1).
    state.script.pov_yaw = state.yaw;
    state.script.pov_pitch = state.pitch;
    input_events(state, input, pawn, events);
    fire_input_event(state, input, params, input_world, events);
}

/// Steps 2–7 of a scripted tick (see the module docs for the order), after
/// [`scripted_input_events`] and the caller's map actors. `input` is already
/// sanitized; `dt` is valid; the caller handles the finite-state guard and
/// the net grounded transition.
#[allow(clippy::too_many_arguments)]
pub(crate) fn scripted_actor_ticks<M: MovementModel, W: CollisionWorld + ?Sized>(
    model: &M,
    state: &mut PlayerState,
    input: &InputFrame,
    params: &PlayerParams,
    pawn: &PawnParams,
    world: &W,
    dt: f32,
    events: &mut StepEvents,
) {
    if !state.script.started {
        start(state, params);
    }

    // 2. Map actors (the recharge crystal's refill, G-CT-4; the caller ticks
    // the other map actors between the two halves of the tick).
    grapple_gun::apply_pending_crystal_refill(state);

    // 3. Controller.
    let (wish_dir, wish_scale) = controller_move(state, input, params, pawn, events);

    // 4. Pawn: state code, physics (with `Landed`), eye height.
    run_pawn_code(state, params, pawn, dt);
    state.script.old_z = state.position.z;
    let intent = LocomotionIntent {
        wish_dir,
        wish_scale,
        jump: false,
    };
    let mut script = state.script;
    let outcome = {
        let mut hooks = ScriptHooks {
            script: &mut script,
            params,
            pawn,
            outcome: None,
        };
        model.advance_scripted(
            state,
            &intent,
            Vec3::ZERO,
            &params.movement,
            &mut hooks,
            world,
            dt,
            events,
        );
        hooks.outcome
    };
    state.script = script;
    events.landing = outcome;
    if outcome.is_some_and(|o| o.sound) {
        // Kismet `SeqEvent_PlayerLanded` (both handlers; the pawn is visible).
        events.kismet.push(SimEvent::PlayerLanded);
    }
    update_eye_height(state, params, pawn, world, dt);

    // 5. Power-jump actor.
    run_power_jump_code(state, pawn, dt, events);

    // 6. Rocket boots.
    rocket_boots::run_state_code(state, params, dt, events);

    // 7. Grapple gun.
    grapple_gun::tick(state, params, world, dt, events);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn latent_sleep_is_frame_quantised() {
        // Sleep(0.1) at 60 Hz wakes in the 6th tick after it was issued, at
        // 30 Hz in the 3rd (ABILITIES.md §15).
        for (dt, ticks) in [(1.0_f32 / 60.0, 6), (1.0 / 30.0, 3)] {
            let mut sleep = Some(0.1_f32);
            let mut n = 0;
            loop {
                n += 1;
                if poll_sleep(&mut sleep, dt) {
                    break;
                }
                assert!(n < 100);
            }
            assert_eq!(n, ticks, "dt {dt}");
            assert_eq!(sleep, None);
        }
        // Sleep(0.016) lasts one tick at 60 Hz.
        let mut sleep = Some(ZOOM_STEP);
        assert!(poll_sleep(&mut sleep, 1.0 / 60.0));
        // Nothing sleeping: never wakes.
        let mut none = None;
        assert!(!poll_sleep(&mut none, 1.0 / 60.0));
    }

    #[test]
    fn goto_clears_locals_only_on_a_state_change() {
        let mut c = PawnCode {
            state: PawnStateName::Zooming,
            zoom: ZoomLocals {
                i: 5,
                ..ZoomLocals::default()
            },
            sleep: Some(0.5),
            ..PawnCode::default()
        };
        c.goto(PawnStateName::Zooming);
        assert_eq!(c.zoom.i, 5, "same state keeps locals");
        assert!(c.begin_pending && c.sleep.is_none());
        c.goto(PawnStateName::StoryState);
        assert_eq!(c.zoom, ZoomLocals::default());
        let mut pj = PowerJump {
            state: PowerJumpStateName::Charging,
            charged: true,
            ..PowerJump::default()
        };
        pj.goto(PowerJumpStateName::Charging);
        assert!(pj.charged);
        pj.cancel();
        assert!(!pj.charged && pj.state == PowerJumpStateName::Canceled);
    }

    #[test]
    fn script_constant_table_lists_every_constant_with_script_code_provenance() {
        assert_eq!(SCRIPT_CONSTANTS.len(), 20);
        let all = all_script_constants().count();
        assert_eq!(
            all,
            20 + grapple_gun::GUN_SCRIPT_CONSTANTS.len()
                + rocket_boots::BOOTS_SCRIPT_CONSTANTS.len()
        );
        let table = script_constants_markdown_table();
        assert_eq!(table.lines().count(), all + 2);
        assert!(table.contains(
            "| `JUMP_RELEASE_MULTIPLIER` | 0.7 | factor | script code asamu.ASAMUPawn.ReleasedJump |"
        ));
        assert!(table.contains("| `ZOOM_STEP` | 0.016 | s |"));
        assert!(table.contains(
            "| `PULL_NUMERATOR` | 10000 | factor | script code asamu.GrappleGun.UpdateGrapple |"
        ));
        assert!(table.contains("| `COUNTER_CLAMP` | 3 | count | script code asamu.GrappleGunLightManager.UpdateLights |"));
        assert!(
            table.contains(
                "| `BOOST_STEP` | 0.05 | s | script code asamu.ASAMURocketBoots.Boosting |"
            )
        );
        for c in all_script_constants() {
            assert_eq!(c.provenance().kind(), "script_code");
        }
    }

    #[test]
    fn timers_fire_when_the_count_strictly_exceeds_the_rate() {
        // One-shot 0.05 s at 60 Hz: 0.0167, 0.0333, 0.05000000x > 0.05 → 3rd.
        let dt = 1.0_f32 / 60.0;
        let mut t = Some(0.0_f32);
        let fired: Vec<u32> = (0..4)
            .map(|_| poll_timer(&mut t, 0.05, false, dt))
            .collect();
        assert_eq!(fired, vec![0, 0, 1, 0]);
        assert_eq!(t, None);
        // Exactly equal never fires (strict comparison).
        let mut t = Some(0.0_f32);
        assert_eq!(poll_timer(&mut t, 0.25, false, 0.25), 0);
        assert_eq!(poll_timer(&mut t, 0.25, false, 0.25), 1);
        // Looping: int(count / rate) fires, the remainder is kept.
        let mut t = Some(0.0_f32);
        assert_eq!(poll_timer(&mut t, 0.1, true, 0.25), 2);
        assert!((t.unwrap() - 0.05).abs() < 1e-6);
        // No timer: nothing.
        let mut none = None;
        assert_eq!(poll_timer(&mut none, 0.1, true, 1.0), 0);
    }
}
