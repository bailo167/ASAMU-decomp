//! Port of the original's native pawn movement physics.
//!
//! [`Ue3PawnMovement`] reimplements the stock UE3/UDK native walking and
//! falling routines that move the player in *A Story About My Uncle*
//! (`APawn::startNewPhysics`, `APawn::physWalking`, `APawn::stepUp`,
//! `AUDKPawn::CalcVelocity` with the inlined `APawn::ApplyVelocityBraking`,
//! `AUDKPawn::physFalling` → `APawn::physFalling`, `APawn::processLanded`,
//! `AActor::TwoWallAdjust`, `AUDKPawn::CalculateSlopeSlide`,
//! `AUDKPawn::GetGravityZ`). It was written **independently from the
//! behavioural spec** `docs/reverse-engineering/NATIVE_PHYSICS.md` (section
//! numbers below refer to it), not from decompiled code.
//!
//! # What is ported (spec sections)
//!
//! - 1.3/1.4 mode dispatch and sub-stepping: sub-step `= remaining` if
//!   `≤ 0.05`, else `min(0.05, remaining/2)`; at most 8 sub-steps per tick
//!   across modes; slices below 0.0003 s are ignored; walking → falling gives
//!   back the untravelled part of the sub-step; falling → walking carries
//!   `remaining + step·(1 − Hit.Time)` on a direct landing and **zero** after
//!   a slide.
//! - 2.1/2.3 the UDK `CalcVelocity` (acceleration = input direction ×
//!   `AccelRate`, turning friction, braking in 0.03 s pieces with factor
//!   `2·friction` and a time-averaged result, 3-D speed cap
//!   `GroundSpeed · MaxSpeedModifier`).
//! - 3.1–3.5 walking (once-per-call velocity update, uphill moves through
//!   `stepUp`, floor probe `MaxStepHeight + 2`, hover band 1.9–2.4 → 2.15,
//!   floor-trace skip, steep-slope slide, fall decision, slope gravity slide,
//!   velocity re-derived from displacement with `Z = 0`).
//! - 4.1–4.8 falling (gravity chain, air-control wall probe and limiter,
//!   semi-implicit sub-steps, landing at `WalkableFloorZ` with the
//!   0.003 / 0.1 landing-velocity rule, wall/ceiling slides, `TwoWallAdjust`,
//!   V-crease landing, the `V = 2·avg − V_old` refinement that doubles the
//!   effective acceleration, `TerminalVelocity` clamp).
//! - 5.2 `processLanded` bookkeeping (floor, base, force-floor-check,
//!   unit-length acceleration).
//!
//! # Scope decisions and gaps (see `docs/PARITY.md`, "Movement model")
//!
//! - **Script is not modelled.** The controller/pawn script (`PlayerMove`,
//!   `DoJump`, `MayFall`, `NotifyHitWall`, `HitWall`, `Landed`,
//!   `NotifyJumpApex`, ...) is replaced by: acceleration `= AccelRate ·`
//!   input direction (TENTATIVE, spec 9.3); jump `Velocity.Z = JumpZ` and
//!   Physics = Falling (TENTATIVE, spec 5.1); `MayFall` leaves the
//!   permission flag set (STRONG for a running player, spec 3.4); all hit and
//!   landing notifications are no-ops. Consequently nothing sets
//!   `bJustTeleported`, and `processHitWall` never changes the physics mode.
//! - **World mapping.** Every surface of a [`CollisionWorld`] is static
//!   world geometry belonging to one actor (like BSP): it is step-up-able,
//!   can be a base, never forces floor re-traces and has no physical
//!   material. There are no physics volumes: one implicit volume supplies
//!   `ground_friction` and `terminal_velocity`, with no zone velocity, no
//!   water and no gravity volumes.
//! - **Not ported:** crouching / walk-slowly and their ledge checks (3.6/3.7;
//!   `CheckForLedges` output is UNKNOWN), the `processLanded` sanity trace /
//!   `FindSpot` / random kick (5.2 step 1), the UDK stuck-falling nudge
//!   (4.9), rotation (6), swimming/flying/other modes (10).
//! - **Collision skin.** UE3's exact pull-back inside line checks is UNKNOWN
//!   (spec 9.5 q7). `MoveActor` here pulls the reported hit time back by
//!   [`CONTACT_SKIN`] along the move (a numerical tolerance of this port, not
//!   a gameplay constant) and inflates the horizontal radius by
//!   [`MOVE_RADIUS_INFLATION`] (STRONG, `UWorld::MoveActor`).
//! - **Contract-violating collision results** (NaN or out-of-range hit
//!   times, non-finite or clearly non-unit normals) are repaired, and
//!   non-finite moves are refused, so a broken [`CollisionWorld`] cannot
//!   make the state non-finite. Robustness only; results from a conforming
//!   world pass through bit for bit.
//! - **Grapple bridge.** `external_accel` (the placeholder grapple pull) is
//!   not part of the original physics: while falling it is added to the
//!   acceleration after the air-control limiter; while walking it lifts the
//!   pawn into falling when it beats gravity, otherwise its horizontal part
//!   is added to the walking velocity after `CalcVelocity`.
//!
//! # Determinism
//!
//! `f32` arithmetic in a fixed order with IEEE basic operations and `sqrt`
//! only; no hashing, no RNG, no globals; collision queries are deterministic
//! by the [`CollisionWorld`] contract. Last-bit agreement with the original
//! executable is not a goal (its vector normalisation/division helpers may
//! round differently — UNKNOWN).

use glam::{Vec2, Vec3};
use serde::{Deserialize, Serialize};

use crate::movement::{LocomotionIntent, MovementModel};
use crate::params::MovementParams;
use crate::sim::{PlayerState, StepEvents};
use crate::world::{CONTACT_SKIN, CollisionShape, CollisionWorld, Hit};

// ---------------------------------------------------------------------------
// Algorithm constants of the native code (provenance: NativeCode). Addresses
// are those cited in NATIVE_PHYSICS.md section 8 (x86_64 Mac executable).
// ---------------------------------------------------------------------------

/// Minimum time slice `startNewPhysics` simulates, s.
/// NativeCode `APawn::startNewPhysics @ 0x100AD8C30` (data 0x101663900).
pub const MIN_TICK_TIME: f32 = 0.0003;
/// Movement sub-steps per tick across all modes (`Iterations > 7` refused).
/// NativeCode `APawn::startNewPhysics @ 0x100AD8C30`, `APawn::physWalking`,
/// `APawn::physFalling` (immediate).
pub const MAX_ITERATIONS: u32 = 8;
/// Longest sub-step, s. NativeCode `APawn::physWalking @ 0x100ADA470`,
/// `APawn::physFalling @ 0x100ADF900` (data 0x101636058).
pub const MAX_SUBSTEP: f32 = 0.05;
/// Sub-step = remaining · this when remaining exceeds [`MAX_SUBSTEP`].
/// NativeCode `APawn::physWalking`/`APawn::physFalling` (data 0x101636054).
pub const SUBSTEP_FRACTION: f32 = 0.5;
/// Braking integration piece, s. NativeCode `APawn::ApplyVelocityBraking @
/// 0x100ADC2C0`, `AUDKPawn::CalcVelocity @ 0x100F6BCA0` (data 0x101731DB0).
pub const BRAKING_SUBSTEP: f32 = 0.03;
/// Braking decays velocity by `1 − 2·friction·h` per piece (computed as
/// `V + V` in the native code). NativeCode `APawn::ApplyVelocityBraking`.
pub const BRAKING_FRICTION_FACTOR: f32 = 2.0;
/// Braked speeds below `sqrt(this)` = 10 uu/s snap to zero.
/// NativeCode `APawn::ApplyVelocityBraking` (data 0x10163713C).
pub const BRAKE_STOP_SPEED_SQUARED: f32 = 100.0;
/// `SafeNormal` returns zero below this squared length.
/// NativeCode (data 0x10163FED4), used by `AUDKPawn::CalcVelocity` and others.
pub const SAFE_NORMAL_MIN_SIZE_SQUARED: f32 = 1.0e-8;
/// Per-component "nearly zero" tolerance. NativeCode (data 0x10163FED8).
pub const NEARLY_ZERO: f32 = 1.0e-4;
/// Floors with normal Z at least this are moved over directly (no step-up).
/// NativeCode `APawn::physWalking` (data 0x101724EC4).
pub const FLAT_FLOOR_Z: f32 = 0.98;
/// Added to `MaxStepHeight` for the step-up height and the floor probe.
/// NativeCode `APawn::physWalking`, `APawn::stepUp @ 0x100ADCFA0`
/// (data 0x101653788).
pub const STEP_FUDGE: f32 = 2.0;
/// Hover above which a walking pawn is snapped down; assumed floor distance
/// when the floor trace is skipped. NativeCode `APawn::physWalking`
/// (data 0x10173D2B0).
pub const MAX_FLOOR_DIST: f32 = 2.4;
/// Hover below which a walking pawn is pushed up. NativeCode
/// `APawn::physWalking` (data 0x10173D2B4).
pub const MIN_FLOOR_DIST: f32 = 1.9;
/// Target hover above the floor. NativeCode `APawn::physWalking`
/// (data 0x10173D2B8).
pub const TARGET_FLOOR_DIST: f32 = 2.15;
/// Hit time assumed when the floor trace is skipped; also the steep-slope
/// push-off, the minimum landing hit time and the same-wall nudge.
/// NativeCode `APawn::physWalking`, `APawn::physFalling`,
/// `AActor::TwoWallAdjust @ 0x100970D20` (data 0x1016457B8).
pub const POINT_ONE: f32 = 0.1;
/// Slope gravity slide only on floors with normal Z below this.
/// NativeCode `APawn::physWalking` (data 0x1016A7A60).
pub const SLOPE_SLIDE_MAX_FLOOR_Z: f32 = 0.99;
/// Slope gravity slide only when `floor.z · friction` is below this.
/// NativeCode `APawn::physWalking` (data 0x10173D2BC).
pub const SLOPE_SLIDE_MAX_Z_FRICTION: f32 = 3.3;
/// Friction floor in the slope-slide distance. NativeCode
/// `APawn::physWalking` (data 0x101636054).
pub const SLOPE_SLIDE_MIN_FRICTION: f32 = 0.5;
/// `dot(down, normal) > this` counts as a (near-)vertical wall in `stepUp`.
/// NativeCode `APawn::stepUp @ 0x100ADCFA0` (data 0x10173D2C8).
pub const STEP_UP_WALL_DOT: f32 = -0.08;
/// `stepUp` repeats when `|Delta|² · Hit.Time` exceeds this.
/// NativeCode `APawn::stepUp @ 0x100ADCFA0` (data 0x10173D2CC).
pub const STEP_UP_REPEAT_THRESHOLD: f32 = 144.0;
/// Air control at or below this is ignored by the wall probe / bound logic.
/// NativeCode `APawn::physFalling @ 0x100ADF900` (data 0x101636058).
pub const AIR_CONTROL_MIN: f32 = 0.05;
/// Horizontal speed below which the air limiter adds a start-up boost.
/// NativeCode `APawn::physFalling` (data 0x101682598).
pub const AIR_LOW_SPEED: f32 = 10.0;
/// Landing velocity is recomputed only if `step · Hit.Time` exceeds this.
/// NativeCode `APawn::physFalling` (data 0x10173D2D4).
pub const LANDING_MIN_CONTACT_TIME: f32 = 0.003;
/// Same-wall tolerance in `TwoWallAdjust` (evaluated in `f64`).
/// NativeCode `AActor::TwoWallAdjust @ 0x100970D20` (data 0x1016393A0).
pub const TWO_WALL_SAME_TOLERANCE: f64 = 1.0e-4;
/// Horizontal extent inflation of the `MoveActor` sweep (STRONG).
/// NativeCode `UWorld::MoveActor @ 0x1008E94A0` (data 0x10171DA60).
pub const MOVE_RADIUS_INFLATION: f32 = 1.001;

/// Safety bound on `stepUp` repetitions per call. Not a gameplay value: the
/// native code has no bound; the repetition condition shrinks the move each
/// time, so sane inputs never come close.
pub const MAX_STEP_UP_REPEATS: u32 = 64;
/// Safety bound on braking pieces (`dt` up to ~31,000 s). Not a gameplay
/// value; protects hostile `dt` from an unbounded loop.
pub const MAX_BRAKING_PIECES: u32 = 1 << 20;
/// Hit normals whose squared length is off by more than this are treated as
/// a broken [`CollisionWorld`] result and renormalised. Not a gameplay
/// value; unit normals from a conforming world are never touched.
pub const NON_UNIT_NORMAL_TOLERANCE: f32 = 1.0e-3;

/// The physics mode the pawn is in.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PhysicsMode {
    /// `PHYS_Walking` (1).
    #[default]
    Walking,
    /// `PHYS_Falling` (2).
    Falling,
}

/// Native-physics bookkeeping that persists between ticks
/// ([`PlayerState::pawn`]). The physics mode itself is
/// [`PlayerState::grounded`] (walking ⇔ grounded).
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct PawnPhysicsState {
    /// Floor normal (UE3 `Pawn.Floor`, Pawn+0x37C); zero when unknown.
    pub floor: Vec3,
    /// The pawn has a base (world geometry). Set by landing and by walking
    /// floor checks, cleared when falling starts.
    pub based: bool,
    /// Force a floor trace on the next walking sub-step (Pawn+0x298 bit 27;
    /// set when entering walking).
    pub force_floor_check: bool,
}

/// The parameter values the port reads, resolved from [`MovementParams`].
/// UE3 property → field mapping (names TENTATIVE per spec section 7).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PawnTuning {
    /// `GroundSpeed` (Pawn+0x33C) ← `movement.max_ground_speed`.
    pub ground_speed: f32,
    /// `AccelRate` (Pawn+0x34C) ← `movement.ground_acceleration`.
    pub accel_rate: f32,
    /// `AirControl` (Pawn+0x35C) ← `movement.air_control`.
    pub air_control: f32,
    /// `JumpZ` (Pawn+0x350) ← `movement.jump_velocity`.
    pub jump_z: f32,
    /// `MaxStepHeight` (Pawn+0x250) ← `movement.step_height`.
    pub max_step_height: f32,
    /// `WalkableFloorZ` (Pawn+0x258) ← `movement.walkable_floor_z`.
    pub walkable_floor_z: f32,
    /// Collision cylinder radius ← `movement.capsule_radius`.
    pub collision_radius: f32,
    /// Collision cylinder half-height (UE3 `CollisionHeight`) ←
    /// `movement.capsule_half_height`.
    pub collision_half_height: f32,
    /// `PhysicsVolume.GroundFriction` ← `movement.ground_friction`.
    pub ground_friction: f32,
    /// `PhysicsVolume.TerminalVelocity` ← `movement.terminal_velocity`.
    pub terminal_velocity: f32,
    /// Pawn gravity = world gravity × `CustomGravityScaling`
    /// (`AUDKPawn::GetGravityZ`) ← `movement.world_gravity_z ·
    /// movement.custom_gravity_scaling`.
    pub gravity_z: f32,
    /// Pawn+0x298 bit 51 (`bLimitFallAccel`) ← `movement.limit_fall_accel`.
    pub limit_fall_accel: bool,
    /// `SlopeBoostFriction` (UDKPawn+0x78C) ← `movement.slope_boost_friction`.
    pub slope_boost_friction: f32,
    /// `MaxSpeedModifier()` for a human, not crouched, not walking slowly:
    /// `MovementSpeedModifier` ← `movement.movement_speed_modifier`.
    pub max_speed_modifier: f32,
}

impl PawnTuning {
    /// Resolves the values from `params`.
    #[must_use]
    pub fn from_params(params: &MovementParams) -> Self {
        Self {
            ground_speed: params.max_ground_speed.value,
            accel_rate: params.ground_acceleration.value,
            air_control: params.air_control.value,
            jump_z: params.jump_velocity.value,
            max_step_height: params.step_height.value,
            walkable_floor_z: params.walkable_floor_z.value,
            collision_radius: params.capsule_radius.value,
            collision_half_height: params.capsule_half_height.value,
            ground_friction: params.ground_friction.value,
            terminal_velocity: params.terminal_velocity.value,
            gravity_z: params.world_gravity_z.value * params.custom_gravity_scaling.value,
            limit_fall_accel: params.limit_fall_accel.value,
            slope_boost_friction: params.slope_boost_friction.value,
            max_speed_modifier: params.movement_speed_modifier.value,
        }
    }
}

/// One movement sub-step taken during a tick.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct SubStep {
    /// Mode the sub-step ran in.
    pub mode: PhysicsMode,
    /// Time removed from the mode's remaining budget, s (the sub-step
    /// length; for a walking "zero move" the whole remainder).
    pub time: f32,
}

/// Time accounting of one tick. Invariant (up to `f32` rounding):
/// `consumed_time() − returned_time + dropped_time = dt`.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct TickStats {
    substeps: [SubStep; MAX_ITERATIONS as usize],
    count: usize,
    /// Time given back by mode transitions (walking → falling: untravelled
    /// part of the sub-step; direct landing: `step · (1 − Hit.Time)`).
    pub returned_time: f32,
    /// Time never simulated: refused slices (< [`MIN_TICK_TIME`] or budget
    /// exhausted), the rest after a slide landing (carried as zero) or a
    /// reverted walking step.
    pub dropped_time: f32,
    /// Walking ↔ falling transitions during the tick (including a jump).
    pub mode_changes: u32,
}

impl TickStats {
    fn record(&mut self, mode: PhysicsMode, time: f32) {
        if let Some(slot) = self.substeps.get_mut(self.count) {
            *slot = SubStep { mode, time };
            self.count += 1;
        }
    }

    /// The sub-steps in order (at most [`MAX_ITERATIONS`]).
    #[must_use]
    pub fn substeps(&self) -> &[SubStep] {
        self.substeps.get(..self.count).unwrap_or(&[])
    }

    /// Number of sub-steps in `mode`.
    #[must_use]
    pub fn count(&self, mode: PhysicsMode) -> usize {
        self.substeps().iter().filter(|s| s.mode == mode).count()
    }

    /// Sum of [`SubStep::time`].
    #[must_use]
    pub fn consumed_time(&self) -> f32 {
        self.substeps().iter().map(|s| s.time).sum()
    }
}

/// The port of the original's native pawn physics (see module docs).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Ue3PawnMovement;

// ---------------------------------------------------------------------------
// Vector helpers with the native semantics.
// ---------------------------------------------------------------------------

/// UE3 `SafeNormal`: unit-length input unchanged, squared length below
/// [`SAFE_NORMAL_MIN_SIZE_SQUARED`] → zero, else `v / |v|`.
#[must_use]
pub fn safe_normal(v: Vec3) -> Vec3 {
    let sq = v.length_squared();
    if sq == 1.0 {
        v
    } else if sq < SAFE_NORMAL_MIN_SIZE_SQUARED || !sq.is_finite() {
        Vec3::ZERO
    } else {
        v * (1.0 / sq.sqrt())
    }
}

/// UE3 `IsNearlyZero`: every component below [`NEARLY_ZERO`] in magnitude.
#[must_use]
pub fn nearly_zero(v: Vec3) -> bool {
    v.x.abs() < NEARLY_ZERO && v.y.abs() < NEARLY_ZERO && v.z.abs() < NEARLY_ZERO
}

/// Sub-step length for `remaining` seconds (spec 1.4).
#[must_use]
pub fn substep(remaining: f32) -> f32 {
    if remaining > MAX_SUBSTEP {
        (remaining * SUBSTEP_FRACTION).min(MAX_SUBSTEP)
    } else {
        remaining
    }
}

/// `APawn::ApplyVelocityBraking` (spec 2.3): decays `velocity` by
/// `1 − 2·friction·h` in pieces `h ≤ 0.03` s and returns the time-weighted
/// average of the post-update velocities (pieces reversed relative to the
/// start velocity are left out), snapped to zero on reversal or below 10 uu/s.
#[must_use]
pub fn apply_velocity_braking(velocity: Vec3, dt: f32, friction: f32) -> Vec3 {
    let start = velocity;
    let mut v = velocity;
    let mut average = Vec3::ZERO;
    let mut t = dt;
    let mut pieces = 0;
    while t > 0.0 && pieces < MAX_BRAKING_PIECES {
        let h = t.min(BRAKING_SUBSTEP);
        let next = t - h;
        if next >= t {
            break; // `h` lost to rounding (absurd dt); stop instead of spinning.
        }
        t = next;
        pieces += 1;
        v -= (v + v) * h * friction;
        if v.dot(start) > 0.0 {
            average += v * (h / dt);
        }
    }
    if average.dot(start) < 0.0 || average.length_squared() < BRAKE_STOP_SPEED_SQUARED {
        Vec3::ZERO
    } else {
        average
    }
}

/// Result of [`udk_calc_velocity`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CalcVelocityResult {
    /// New velocity.
    pub velocity: Vec3,
    /// New acceleration (`accel_dir · accel_rate`, overwriting the script value).
    pub acceleration: Vec3,
}

/// `AUDKPawn::CalcVelocity` as called by walking (spec 2.1; `bFluid = 0`,
/// `bBrake = 1`, `bBuoyant = 0`). `accel_dir` is unit length or zero;
/// `max_speed` is `GroundSpeed · MaxSpeedModifier()`.
#[must_use]
pub fn udk_calc_velocity(
    velocity: Vec3,
    accel_dir: Vec3,
    accel_rate: f32,
    dt: f32,
    max_speed: f32,
    friction: f32,
) -> CalcVelocityResult {
    let acceleration = accel_dir * accel_rate;
    let mut v = velocity;
    if acceleration != Vec3::ZERO {
        // Turning friction: pulls the velocity direction towards the input.
        let speed = v.length();
        v -= (v - accel_dir * speed) * friction * dt;
    } else {
        v = apply_velocity_braking(v, dt, friction);
    }
    // The fluid-friction factor `1 − bFluid·F·dt` is exactly 1 here.
    v += acceleration * dt;
    if v.length_squared() > max_speed * max_speed {
        v = safe_normal(v) * max_speed;
    }
    CalcVelocityResult {
        velocity: v,
        acceleration,
    }
}

/// `AActor::TwoWallAdjust` (spec 4.8): adjusts `delta` after hitting a second
/// wall `normal` while sliding along `old_normal`.
#[must_use]
pub fn two_wall_adjust(
    desired_dir: Vec3,
    delta: Vec3,
    normal: Vec3,
    old_normal: Vec3,
    hit_time: f32,
) -> Vec3 {
    let dot = old_normal.dot(normal);
    if dot <= 0.0 {
        // Walls meet at 90° or less: slide along the crease.
        let crease = safe_normal(normal.cross(old_normal));
        let mut d = crease * delta.dot(crease) * (1.0 - hit_time);
        if d.dot(desired_dir) < 0.0 {
            d = -d;
        }
        d
    } else {
        let mut d = (delta - normal * delta.dot(normal)) * (1.0 - hit_time);
        if d.dot(desired_dir) <= 0.0 {
            d = Vec3::ZERO;
        } else if (f64::from(dot) - 1.0).abs() < TWO_WALL_SAME_TOLERANCE {
            // Same wall twice: nudge away from it.
            d += normal * POINT_ONE;
        }
        d
    }
}

/// `AUDKPawn::CalculateSlopeSlide` (spec 4.7) for surfaces without a physical
/// material: project onto the surface; unless `slope_boost_friction` is 0,
/// an upward slide may not climb more than the move itself did.
#[must_use]
pub fn calculate_slope_slide(
    adjusted: Vec3,
    normal: Vec3,
    hit_time: f32,
    slope_boost_friction: f32,
) -> Vec3 {
    let mut slide = (adjusted - normal * adjusted.dot(normal)) * (1.0 - hit_time);
    if slope_boost_friction != 0.0 && slide.z > 0.0 {
        slide.z = slide.z.min(adjusted.z * (1.0 - hit_time));
    }
    slide
}

// ---------------------------------------------------------------------------
// The simulation of one tick.
// ---------------------------------------------------------------------------

/// A blocking-hit record (`FCheckResult` subset).
#[derive(Clone, Copy, Debug, PartialEq)]
struct MoveHit {
    time: f32,
    normal: Vec3,
    start_penetrating: bool,
}

impl MoveHit {
    const NONE: Self = Self {
        time: 1.0,
        normal: Vec3::ZERO,
        start_penetrating: false,
    };

    fn blocked(&self) -> bool {
        self.time < 1.0
    }

    /// Converts a world hit, repairing results that break the
    /// [`CollisionWorld`] contract (robustness only, not original
    /// behaviour): a NaN time blocks at once, times are clamped to `[0, 1]`,
    /// non-finite normals become zero and clearly non-unit normals are
    /// renormalised. Contract-abiding hits (time in `[0, 1]`, unit normal)
    /// pass through bit for bit.
    fn from_world(hit: &Hit) -> Self {
        let time = if hit.time.is_nan() {
            0.0
        } else {
            hit.time.clamp(0.0, 1.0)
        };
        let n = hit.normal;
        let normal = if !n.is_finite() {
            Vec3::ZERO
        } else if (n.length_squared() - 1.0).abs() > NON_UNIT_NORMAL_TOLERANCE {
            safe_normal(n)
        } else {
            n
        };
        Self {
            time,
            normal,
            start_penetrating: hit.start_penetrating,
        }
    }
}

/// What a mode handler asks `startNewPhysics` to do next.
enum Flow {
    /// The tick is over.
    Done,
    /// Run the (possibly new) mode with this budget.
    Continue { remaining: f32, iterations: u32 },
}

struct Pawn<'w, W: CollisionWorld + ?Sized> {
    world: &'w W,
    t: PawnTuning,
    move_shape: CollisionShape,
    trace_shape: CollisionShape,
    location: Vec3,
    velocity: Vec3,
    acceleration: Vec3,
    mode: PhysicsMode,
    floor: Vec3,
    based: bool,
    force_floor_check: bool,
    external: Vec3,
    stats: TickStats,
    landed_impact: Option<f32>,
}

impl<W: CollisionWorld + ?Sized> Pawn<'_, W> {
    /// `UWorld::MoveActor`: sweep by `delta`, stop at the first blocking hit
    /// (pulled back by [`CONTACT_SKIN`] along the move).
    fn move_actor(&mut self, delta: Vec3) -> MoveHit {
        // A non-finite move (only reachable through a contract-violating
        // world or absurd magnitudes) is refused rather than propagated.
        if delta == Vec3::ZERO || !delta.is_finite() {
            return MoveHit::NONE;
        }
        let end = self.location + delta;
        match self
            .world
            .sweep_capsule(self.location, end, self.move_shape)
        {
            None => {
                self.location = end;
                MoveHit::NONE
            }
            Some(hit) => {
                let mut hit = MoveHit::from_world(&hit);
                let len = delta.length();
                hit.time = if len > 0.0 {
                    (hit.time - CONTACT_SKIN / len).clamp(0.0, 1.0)
                } else {
                    0.0
                };
                self.location += delta * hit.time;
                hit
            }
        }
    }

    /// `SingleLineCheck` with the collision cylinder (no pull-back).
    fn line_check(&self, start: Vec3, end: Vec3) -> MoveHit {
        match self.world.sweep_capsule(start, end, self.trace_shape) {
            None => MoveHit::NONE,
            Some(hit) => MoveHit::from_world(&hit),
        }
    }

    fn set_falling(&mut self) {
        if self.mode != PhysicsMode::Falling {
            self.mode = PhysicsMode::Falling;
            self.stats.mode_changes += 1;
        }
        // `AActor::setPhysics` drops the base for modes other than walking.
        self.based = false;
    }

    /// `APawn::startNewPhysics` (spec 1.3), as a loop over mode handlers.
    fn start_new_physics(&mut self, mut remaining: f32, mut iterations: u32) {
        loop {
            if remaining.is_nan() || remaining < MIN_TICK_TIME || iterations >= MAX_ITERATIONS {
                if remaining > 0.0 {
                    self.stats.dropped_time += remaining;
                }
                return;
            }
            let flow = match self.mode {
                PhysicsMode::Walking => self.phys_walking(remaining, iterations),
                PhysicsMode::Falling => self.phys_falling(remaining, iterations),
            };
            match flow {
                Flow::Done => return,
                Flow::Continue {
                    remaining: r,
                    iterations: i,
                } => {
                    remaining = r;
                    iterations = i;
                }
            }
        }
    }

    /// `APawn::physWalking` (spec 3.1–3.5) for a running player.
    fn phys_walking(&mut self, dt: f32, mut iterations: u32) -> Flow {
        let walkable = self.t.walkable_floor_z;
        self.velocity.z = 0.0;
        self.acceleration.z = 0.0;
        let accel_dir = if self.acceleration.x == 0.0 && self.acceleration.y == 0.0 {
            Vec3::ZERO
        } else {
            safe_normal(self.acceleration)
        };
        let calc = udk_calc_velocity(
            self.velocity,
            accel_dir,
            self.t.accel_rate,
            dt,
            self.t.ground_speed * self.t.max_speed_modifier,
            self.t.ground_friction,
        );
        self.velocity = calc.velocity;
        self.acceleration = calc.acceleration;
        // Grapple bridge (not original): horizontal external acceleration.
        if self.external.x != 0.0 || self.external.y != 0.0 {
            self.velocity.x += self.external.x * dt;
            self.velocity.y += self.external.y * dt;
        }
        // No zone velocity (no physics volumes).
        let desired_move = Vec3::new(self.velocity.x, self.velocity.y, 0.0);

        let old_location = self.location;
        let old_floor = self.floor;
        let old_based = self.based;
        let mut remaining = dt;
        while remaining > 0.0 && iterations < MAX_ITERATIONS {
            let step = substep(remaining);
            iterations += 1;
            let delta = desired_move * step;
            let step_start = self.location;
            let zero_move = nearly_zero(delta);
            let mut blocked = false;
            if zero_move {
                self.stats.record(PhysicsMode::Walking, remaining);
                remaining = 0.0;
            } else {
                self.stats.record(PhysicsMode::Walking, step);
                remaining -= step;
                // No ledge check: only crouched / walking-slowly pawns ask.
                let hit = if self.floor.z >= FLAT_FLOOR_Z || self.floor.dot(delta) >= 0.0 {
                    self.move_actor(delta)
                } else {
                    // Moving uphill on a slope: go through stepUp.
                    MoveHit {
                        time: 0.0,
                        normal: self.floor,
                        start_penetrating: false,
                    }
                };
                if hit.blocked() {
                    // World geometry is step-up-able: always stepUp (3.2).
                    blocked = true;
                    self.step_up(safe_normal(delta), delta * (1.0 - hit.time), hit);
                }
            }

            // ---- floor check and height adjustment (3.3) ----
            let mut floor_hit;
            let floor_dist;
            let floor_is_base;
            if !blocked && zero_move && self.based && !self.force_floor_check {
                // Trace skipped: reuse the old floor.
                floor_hit = MoveHit {
                    time: POINT_ONE,
                    normal: self.floor,
                    start_penetrating: false,
                };
                floor_dist = MAX_FLOOR_DIST;
                floor_is_base = true;
            } else {
                self.force_floor_check = false;
                let probe = self.t.max_step_height + STEP_FUDGE;
                floor_hit = self.line_check(self.location, self.location - Vec3::Z * probe);
                floor_dist = probe * floor_hit.time;
                self.floor = floor_hit.normal;
                floor_is_base = self.based && floor_hit.blocked();
            }
            let mut steep_wall_block = false;
            if floor_hit.normal.z >= walkable
                || nearly_zero(delta)
                || delta.dot(floor_hit.normal) >= 0.0
            {
                if !floor_hit.blocked()
                    || floor_hit.start_penetrating
                    || (floor_dist <= MAX_FLOOR_DIST && floor_is_base)
                {
                    if floor_dist < MIN_FLOOR_DIST && !floor_hit.start_penetrating {
                        self.move_actor(Vec3::new(0.0, 0.0, TARGET_FLOOR_DIST - floor_dist));
                        floor_hit.time = 0.0;
                    }
                } else {
                    // Floor farther than 2.4 or a new base: snap to 2.15
                    // (`ShouldCatchAir` is always false for this pawn).
                    let snap = self.move_actor(Vec3::new(0.0, 0.0, TARGET_FLOOR_DIST - floor_dist));
                    if snap.blocked() {
                        floor_hit = snap;
                    }
                    self.based = true;
                }
            } else {
                // Too steep and pushing into it: slide down the slope.
                let n = floor_hit.normal;
                let drop = Vec3::new(0.0, 0.0, -self.t.max_step_height);
                let slide = n * POINT_ONE + (drop - n * drop.dot(n));
                let h = self.move_actor(slide);
                if h.blocked() {
                    self.floor = h.normal;
                    self.based = true;
                    floor_hit = h;
                    // TENTATIVE interpretation of the "wall also hit" flag.
                    steep_wall_block = h.normal.z < walkable;
                }
            }

            // ---- fall / ledge decision (3.4) ----
            if !floor_hit.blocked() || floor_hit.normal.z < walkable {
                if steep_wall_block {
                    // Revert the step (teleport back, stop, re-base).
                    self.velocity = Vec3::ZERO;
                    self.acceleration = Vec3::ZERO;
                    self.location = old_location;
                    // Re-attach to the old base with the old floor; skipped
                    // when there was no old base (world geometry is never
                    // in the spec's other skip cases).
                    if old_based {
                        self.floor = old_floor;
                        self.based = true;
                    }
                    self.stats.dropped_time += remaining;
                    return Flow::Done;
                }
                // StartFalling: give back the untravelled part of the step.
                let delta_size = delta.length();
                let carry = if delta_size == 0.0 {
                    0.0
                } else {
                    let moved = Vec2::new(
                        self.location.x - step_start.x,
                        self.location.y - step_start.y,
                    )
                    .length();
                    let frac = (moved / delta_size).min(1.0);
                    remaining + step * (1.0 - frac)
                };
                self.stats.returned_time += carry - remaining;
                self.velocity.z = 0.0;
                self.set_falling();
                return Flow::Continue {
                    remaining: carry,
                    iterations,
                };
            }

            // ---- slope gravity slide (3.5) ----
            let friction = self.t.ground_friction;
            if self.floor.z < SLOPE_SLIDE_MAX_FLOOR_Z
                && self.floor.z * friction < SLOPE_SLIDE_MAX_Z_FRICTION
            {
                let g = self.t.gravity_z * dt / (2.0 * friction.max(SLOPE_SLIDE_MIN_FRICTION)) * dt;
                let gravity_move = Vec3::new(0.0, 0.0, g);
                let slide = gravity_move - self.floor * self.floor.dot(gravity_move);
                if slide.dot(gravity_move) >= 0.0 {
                    self.move_actor(slide);
                }
            }
        }
        if remaining > 0.0 {
            self.stats.dropped_time += remaining;
        }
        // Still walking: velocity = actual displacement / dt, Z = 0.
        self.velocity = (self.location - old_location) / dt;
        self.velocity.z = 0.0;
        Flow::Done
    }

    /// `APawn::stepUp` (spec 3.2.1) for a walking pawn, with the repetition
    /// of step 4 written as a loop. Every path through the loop body ends
    /// with the planned step down (then returns, or repeats), so when the
    /// [`MAX_STEP_UP_REPEATS`] safety bound is exhausted the pawn has already
    /// stepped down and the rest of the move is simply not made.
    fn step_up(&mut self, desired_dir: Vec3, delta: Vec3, hit: MoveHit) {
        let down = Vec3::NEG_Z;
        let step_down = down * (self.t.max_step_height + STEP_FUDGE);
        let mut delta = delta;
        let mut hit = hit;
        for _ in 0..MAX_STEP_UP_REPEATS {
            // 1./2. Near-vertical wall or walkable surface: up, then across.
            // (A non-walkable slope hit while walking moves nothing here.)
            if down.dot(hit.normal) > STEP_UP_WALL_DOT || hit.normal.z >= self.t.walkable_floor_z {
                self.move_actor(-step_down);
                hit = self.move_actor(delta);
            }
            // 3. Clear: step down and finish.
            if !hit.blocked() {
                self.move_actor(step_down);
                return;
            }
            // 4. Still a wall and enough of the move was made: step down and
            // repeat with the rest (climbs successive stairs).
            if down.dot(hit.normal) > STEP_UP_WALL_DOT
                && delta.length_squared() * hit.time > STEP_UP_REPEAT_THRESHOLD
            {
                self.move_actor(step_down);
                delta *= 1.0 - hit.time;
                continue;
            }
            // 5. Slide along the horizontalised wall normal (no nearly-zero
            // skip here), one TwoWallAdjust retry, then step down.
            let n = safe_normal(Vec3::new(hit.normal.x, hit.normal.y, 0.0));
            let slide = (delta - n * delta.dot(n)) * (1.0 - hit.time);
            if slide.dot(delta) >= 0.0 {
                let second = self.move_actor(slide);
                if second.blocked() {
                    // TENTATIVE: the first (horizontalised) normal is the
                    // "old" normal of the adjustment.
                    let adjusted =
                        two_wall_adjust(desired_dir, slide, second.normal, n, second.time);
                    self.move_actor(adjusted);
                }
            }
            self.move_actor(step_down);
            return;
        }
    }

    /// `AUDKPawn::physFalling` → `APawn::physFalling` (spec 4.2–4.8).
    fn phys_falling(&mut self, dt: f32, mut iterations: u32) -> Flow {
        let walkable = self.t.walkable_floor_z;
        let old_acceleration = self.acceleration;
        self.acceleration.z = 0.0;

        // ---- air control and acceleration limit (4.2) ----
        let mut tick_air_control = self.t.air_control;
        if tick_air_control > AIR_CONTROL_MIN {
            let dir = safe_normal(Vec3::new(self.acceleration.x, self.acceleration.y, 0.0));
            let test_walk = (Vec3::new(self.velocity.x, self.velocity.y, 0.0)
                + dir * (self.t.accel_rate * tick_air_control))
                * dt;
            if test_walk != Vec3::ZERO
                && self
                    .line_check(self.location, self.location + test_walk)
                    .blocked()
            {
                tick_air_control = 0.0; // no air control into walls
            }
        }
        let mut bound_speed = 0.0_f32;
        if self.t.limit_fall_accel {
            let mut max_accel = self.t.accel_rate * tick_air_control;
            let speed_2d = Vec2::new(self.velocity.x, self.velocity.y).length();
            if tick_air_control > 0.0 && speed_2d < AIR_LOW_SPEED {
                max_accel += (AIR_LOW_SPEED - speed_2d) / dt;
            } else if speed_2d >= self.t.ground_speed {
                if tick_air_control > AIR_CONTROL_MIN {
                    bound_speed = speed_2d;
                } else {
                    max_accel = 1.0;
                }
            }
            if self.acceleration.length() > max_accel {
                self.acceleration = safe_normal(self.acceleration) * max_accel;
            }
        }
        let gravity = self.t.gravity_z;

        // ---- sub-steps (4.3) ----
        let mut remaining = dt;
        while remaining > 0.0 && iterations < MAX_ITERATIONS {
            let step = substep(remaining);
            iterations += 1;
            self.stats.record(PhysicsMode::Falling, step);
            let old_location = self.location;
            let mut old_velocity = self.velocity;
            // NewFallVelocity in air: V + A·dt (no fluid friction/buoyancy).
            let fall_accel = Vec3::new(
                self.acceleration.x + self.external.x,
                self.acceleration.y + self.external.y,
                gravity + self.external.z,
            );
            self.velocity = old_velocity + fall_accel * step;
            if bound_speed != 0.0
                && Vec2::new(self.velocity.x, self.velocity.y).length() > bound_speed
            {
                let n = safe_normal(Vec3::new(self.velocity.x, self.velocity.y, 0.0));
                self.velocity.x = n.x * bound_speed;
                self.velocity.y = n.y * bound_speed;
            }
            // Semi-implicit: the move uses the NEW velocity (no zone velocity).
            let adjusted = self.velocity * step;
            let hit = self.move_actor(adjusted);
            remaining -= step;

            if hit.blocked() {
                if hit.normal.z >= walkable {
                    // ---- landing (4.4) ----
                    let contact = step * hit.time;
                    if contact > LANDING_MIN_CONTACT_TIME && hit.time > POINT_ONE {
                        self.velocity = (self.location - old_location) / contact;
                    }
                    let returned = step * (1.0 - hit.time);
                    self.stats.returned_time += returned;
                    self.process_landed(hit.normal);
                    return Flow::Continue {
                        remaining: remaining + returned,
                        iterations,
                    };
                }
                // ---- walls and ceilings (4.5) ----
                let slide = calculate_slope_slide(
                    adjusted,
                    hit.normal,
                    hit.time,
                    self.t.slope_boost_friction,
                );
                if slide.dot(adjusted) >= 0.0 {
                    let second = self.move_actor(slide);
                    if second.blocked() {
                        if second.normal.z >= walkable {
                            return self.land_after_slide(second.normal, remaining, iterations);
                        }
                        let adjusted_slide = two_wall_adjust(
                            safe_normal(adjusted),
                            slide,
                            second.normal,
                            hit.normal,
                            second.time,
                        );
                        let ditch = hit.normal.z > 0.0
                            && second.normal.z > 0.0
                            && adjusted_slide.z == 0.0
                            && hit.normal.dot(second.normal) < 0.0;
                        let third = self.move_actor(adjusted_slide);
                        if ditch || (third.blocked() && third.normal.z >= walkable) {
                            let normal = if third.blocked() {
                                third.normal
                            } else {
                                second.normal
                            };
                            return self.land_after_slide(normal, remaining, iterations);
                        }
                    }
                }
                // No horizontal extrapolation after wall contact.
                let average = (self.location - old_location) / step;
                old_velocity.x = average.x;
                old_velocity.y = average.y;
            }

            // ---- velocity refinement (4.6) ----
            let mut v = (self.location - old_location) / step;
            if old_velocity.z >= 0.0 || v.z < old_velocity.z {
                v = v * 2.0 - old_velocity;
            }
            if v.length() > self.t.terminal_velocity {
                v = safe_normal(v) * self.t.terminal_velocity;
            }
            self.velocity = v;
        }
        if remaining > 0.0 {
            self.stats.dropped_time += remaining;
        }
        self.acceleration = old_acceleration;
        Flow::Done
    }

    /// A landing detected after a slide carries zero time.
    fn land_after_slide(&mut self, normal: Vec3, remaining: f32, iterations: u32) -> Flow {
        if remaining > 0.0 {
            self.stats.dropped_time += remaining;
        }
        self.process_landed(normal);
        Flow::Continue {
            remaining: 0.0,
            iterations,
        }
    }

    /// `APawn::processLanded` (spec 5.2) without the sanity trace / FindSpot
    /// rejection and without script notifications.
    fn process_landed(&mut self, normal: Vec3) {
        self.floor = normal;
        if self.landed_impact.is_none() {
            self.landed_impact = Some(self.velocity.z);
        }
        // SetPostLandedPhysics → setPhysics(Walking): base on the floor
        // actor, force a floor check.
        if self.mode != PhysicsMode::Walking {
            self.mode = PhysicsMode::Walking;
            self.stats.mode_changes += 1;
        }
        self.based = true;
        self.force_floor_check = true;
        self.acceleration = safe_normal(self.acceleration);
    }
}

impl Ue3PawnMovement {
    /// [`MovementModel::advance`] that also returns the tick's time
    /// accounting. A non-finite or non-positive `dt` is a no-op.
    #[allow(clippy::too_many_arguments)]
    pub fn advance_with_stats<W: CollisionWorld + ?Sized>(
        &self,
        state: &mut PlayerState,
        intent: &LocomotionIntent,
        external_accel: Vec3,
        params: &MovementParams,
        world: &W,
        dt: f32,
        events: &mut StepEvents,
    ) -> TickStats {
        if !(dt.is_finite() && dt > 0.0) {
            return TickStats::default();
        }
        let t = PawnTuning::from_params(params);
        let trace_shape = CollisionShape {
            radius: t.collision_radius,
            half_height: t.collision_half_height,
        };
        let move_shape = CollisionShape {
            radius: t.collision_radius * MOVE_RADIUS_INFLATION,
            half_height: t.collision_half_height,
        };
        let was_walking = state.grounded;
        let external = if external_accel.is_finite() {
            external_accel
        } else {
            Vec3::ZERO
        };
        let mut pawn = Pawn {
            world,
            t,
            move_shape,
            trace_shape,
            location: state.position,
            velocity: state.velocity,
            // [script] PlayerMove: Acceleration = AccelRate · Normal(input)
            // (TENTATIVE, spec 9.3; analog magnitude discarded).
            acceleration: intent.wish_dir * t.accel_rate,
            mode: if was_walking {
                PhysicsMode::Walking
            } else {
                PhysicsMode::Falling
            },
            floor: state.pawn.floor,
            based: state.pawn.based && was_walking,
            force_floor_check: state.pawn.force_floor_check,
            external,
            stats: TickStats::default(),
            landed_impact: None,
        };
        // [script] DoJump: Velocity.Z = JumpZ, Physics = Falling (TENTATIVE,
        // spec 5.1).
        if pawn.mode == PhysicsMode::Walking && intent.jump {
            pawn.velocity.z = t.jump_z;
            pawn.set_falling();
            events.jumped = true;
        }
        // Grapple bridge (not original): a pull that beats gravity lifts off.
        if pawn.mode == PhysicsMode::Walking && t.gravity_z + external.z > 0.0 {
            pawn.set_falling();
        }
        pawn.start_new_physics(dt, 0);

        state.position = pawn.location;
        state.velocity = pawn.velocity;
        state.grounded = pawn.mode == PhysicsMode::Walking;
        state.pawn = PawnPhysicsState {
            floor: pawn.floor,
            based: pawn.based,
            force_floor_check: pawn.force_floor_check,
        };
        if !was_walking && state.grounded {
            events.landed = pawn.landed_impact;
        }
        if was_walking && !state.grounded && !events.jumped {
            events.left_ground = true;
        }
        pawn.stats
    }
}

impl MovementModel for Ue3PawnMovement {
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
        self.advance_with_stats(state, intent, external_accel, params, world, dt, events);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn safe_normal_semantics() {
        assert_eq!(safe_normal(Vec3::X), Vec3::X);
        assert_eq!(
            safe_normal(Vec3::new(0.0, 3.0, 4.0)),
            Vec3::new(0.0, 0.6, 0.8)
        );
        assert_eq!(safe_normal(Vec3::new(5e-5, 5e-5, 0.0)), Vec3::ZERO);
        assert_eq!(safe_normal(Vec3::ZERO), Vec3::ZERO);
        assert_eq!(safe_normal(Vec3::new(f32::INFINITY, 0.0, 0.0)), Vec3::ZERO);
        assert!(nearly_zero(Vec3::new(9.9e-5, -9.9e-5, 0.0)));
        assert!(!nearly_zero(Vec3::new(1e-4, 0.0, 0.0)));
    }

    #[test]
    fn substep_rule() {
        assert_eq!(substep(0.016), 0.016);
        assert_eq!(substep(0.05), 0.05);
        assert_eq!(substep(0.06), 0.03);
        assert_eq!(substep(0.08), 0.04);
        assert_eq!(substep(0.1), 0.05);
        assert_eq!(substep(10.0), 0.05);
    }

    /// Closed form of the braking average in f64 (spec 2.3).
    fn braking_reference(v0: f64, dt: f64, friction: f64) -> f64 {
        let mut v = v0;
        let mut avg = 0.0;
        let mut t = dt;
        while t > 0.0 {
            let h = t.min(0.03);
            t -= h;
            v *= 1.0 - 2.0 * h * friction;
            if v * v0 > 0.0 {
                avg += v * h / dt;
            }
        }
        if avg * v0 < 0.0 || avg * avg < 100.0 {
            0.0
        } else {
            avg
        }
    }

    #[test]
    fn braking_matches_the_piecewise_formula() {
        for &(v0, dt, f) in &[
            (400.0_f32, 1.0 / 62.0_f32, 8.0_f32),
            (400.0, 1.0 / 30.0, 8.0),
            (400.0, 0.1, 6.0),
            (1234.5, 0.25, 2.0),
            (-300.0, 0.07, 4.0),
        ] {
            let got = apply_velocity_braking(Vec3::new(v0, 0.0, 0.0), dt, f).x;
            let want = braking_reference(f64::from(v0), f64::from(dt), f64::from(f));
            assert!(
                (f64::from(got) - want).abs() <= 1e-4 * want.abs().max(1.0),
                "v0 {v0} dt {dt} F {f}: {got} vs {want}"
            );
            assert!(got.abs() < v0.abs());
        }
        // Single piece, explicit: 400·(1 − 2·8·(1/62)).
        let dt = 1.0 / 62.0_f32;
        let one = apply_velocity_braking(Vec3::new(400.0, 0.0, 0.0), dt, 8.0);
        assert!((one.x - 400.0 * (1.0 - 16.0 * dt)).abs() < 1e-3, "{one}");
        // Two pieces (0.03 + 0.00333): time-weighted average, not end value.
        let dt = 1.0 / 30.0_f32;
        let two = apply_velocity_braking(Vec3::new(400.0, 0.0, 0.0), dt, 8.0).x;
        let v1 = 400.0 * (1.0 - 16.0 * 0.03);
        let h2 = dt - 0.03;
        let v2 = v1 * (1.0 - 16.0 * h2);
        let expected = v1 * 0.03 / dt + v2 * h2 / dt;
        assert!((two - expected).abs() < 1e-3, "{two} vs {expected}");
        assert!(two > v2, "average exceeds the end value");
    }

    #[test]
    fn braking_snaps_slow_or_reversed_velocity_to_zero() {
        // Below 10 uu/s after the average.
        assert_eq!(
            apply_velocity_braking(Vec3::new(10.5, 0.0, 0.0), 1.0 / 60.0, 8.0),
            Vec3::ZERO
        );
        // 2·F·h > 2 flips the sign: reversed pieces are skipped → zero.
        assert_eq!(
            apply_velocity_braking(Vec3::new(400.0, 0.0, 0.0), 0.03, 50.0),
            Vec3::ZERO
        );
        // Still above 10 uu/s.
        assert_ne!(
            apply_velocity_braking(Vec3::new(20.0, 0.0, 0.0), 1.0 / 60.0, 1.0),
            Vec3::ZERO
        );
        // Absurd dt terminates.
        let v = apply_velocity_braking(Vec3::new(400.0, 0.0, 0.0), 1.0e9, 8.0);
        assert!(v.is_finite());
    }

    #[test]
    fn calc_velocity_turning_friction_and_cap() {
        // Aligned input: friction term is exactly zero.
        let r = udk_calc_velocity(
            Vec3::new(100.0, 0.0, 0.0),
            Vec3::X,
            2000.0,
            0.01,
            1000.0,
            8.0,
        );
        assert_eq!(r.velocity, Vec3::new(120.0, 0.0, 0.0));
        assert_eq!(r.acceleration, Vec3::new(2000.0, 0.0, 0.0));
        // Perpendicular input: v -= (v − d·|v|)·F·dt, then + a·dt.
        let r = udk_calc_velocity(
            Vec3::new(100.0, 0.0, 0.0),
            Vec3::Y,
            2000.0,
            0.01,
            1000.0,
            8.0,
        );
        let fdt = 8.0 * 0.01;
        let expected = Vec3::new(100.0 - 100.0 * fdt, 100.0 * fdt + 20.0, 0.0);
        assert!((r.velocity - expected).length() < 1e-4, "{}", r.velocity);
        // Cap is 3-D and applied after acceleration.
        let r = udk_calc_velocity(
            Vec3::new(990.0, 0.0, 0.0),
            Vec3::X,
            2000.0,
            0.01,
            1000.0,
            8.0,
        );
        assert_eq!(r.velocity, Vec3::new(1000.0, 0.0, 0.0));
        // No input: braking.
        let r = udk_calc_velocity(
            Vec3::new(400.0, 0.0, 0.0),
            Vec3::ZERO,
            2000.0,
            0.01,
            1000.0,
            8.0,
        );
        assert_eq!(r.acceleration, Vec3::ZERO);
        assert_eq!(
            r.velocity,
            apply_velocity_braking(Vec3::new(400.0, 0.0, 0.0), 0.01, 8.0)
        );
        // Zero AccelRate also brakes (exact-zero acceleration test).
        let r = udk_calc_velocity(Vec3::new(400.0, 0.0, 0.0), Vec3::X, 0.0, 0.01, 1000.0, 8.0);
        assert!(r.velocity.x < 400.0);
    }

    #[test]
    fn two_wall_adjust_cases() {
        // Perpendicular walls (dot = 0): slide along the crease (Z axis here
        // is the crease of the X and Y walls).
        let d = two_wall_adjust(
            Vec3::new(1.0, 1.0, 1.0).normalize(),
            Vec3::new(1.0, 1.0, 1.0),
            Vec3::NEG_Y,
            Vec3::NEG_X,
            0.5,
        );
        assert!((d - Vec3::new(0.0, 0.0, 0.5)).length() < 1e-6, "{d}");
        // Crease direction is flipped to follow the desired direction.
        let d = two_wall_adjust(
            Vec3::NEG_Z,
            Vec3::new(1.0, 1.0, -1.0),
            Vec3::NEG_Y,
            Vec3::NEG_X,
            0.0,
        );
        assert!((d - Vec3::new(0.0, 0.0, -1.0)).length() < 1e-6, "{d}");
        // Obtuse walls: project on the new wall; reject moves against desire.
        let n = Vec3::new(-1.0, 1.0, 0.0).normalize();
        let d = two_wall_adjust(Vec3::Y, Vec3::new(1.0, 1.0, 0.0), n, Vec3::NEG_X, 0.0);
        assert!(d.dot(n).abs() < 1e-6 && d.dot(Vec3::Y) > 0.0, "{d}");
        let d = two_wall_adjust(Vec3::NEG_Y, Vec3::new(1.0, 1.0, 0.0), n, Vec3::NEG_X, 0.0);
        assert_eq!(d, Vec3::ZERO);
        // Same wall twice: nudge 0.1 along the normal.
        let d = two_wall_adjust(
            Vec3::Y,
            Vec3::new(1.0, 1.0, 0.0),
            Vec3::NEG_X,
            Vec3::NEG_X,
            0.0,
        );
        assert!((d - Vec3::new(-0.1, 1.0, 0.0)).length() < 1e-6, "{d}");
    }

    #[test]
    fn world_hits_pass_through_unless_they_break_the_contract() {
        let hit = |time: f32, normal: Vec3| Hit {
            time,
            distance: 0.0,
            position: Vec3::ZERO,
            normal,
            grapple_able: false,
            start_penetrating: true,
        };
        // Conforming: bit-identical (time in [0, 1], unit normal; also a unit
        // normal whose squared length is not exactly 1 in f32).
        let n = Vec3::new(-(1.0_f32 - 0.8 * 0.8).sqrt(), 0.0, 0.8);
        for (t, n) in [(0.37, n), (0.0, Vec3::Z), (1.0, Vec3::NEG_X)] {
            let m = MoveHit::from_world(&hit(t, n));
            assert_eq!(m.time.to_bits(), t.to_bits());
            assert_eq!(
                m.normal.to_array().map(f32::to_bits),
                n.to_array().map(f32::to_bits)
            );
            assert!(m.start_penetrating);
        }
        // Broken: repaired.
        assert_eq!(MoveHit::from_world(&hit(f32::NAN, Vec3::Z)).time, 0.0);
        assert_eq!(MoveHit::from_world(&hit(-2.0, Vec3::Z)).time, 0.0);
        assert_eq!(MoveHit::from_world(&hit(f32::INFINITY, Vec3::Z)).time, 1.0);
        let m = MoveHit::from_world(&hit(0.5, Vec3::new(0.0, f32::NAN, 1.0)));
        assert_eq!(m.normal, Vec3::ZERO);
        let m = MoveHit::from_world(&hit(0.5, Vec3::new(0.0, 0.0, 5.0)));
        assert_eq!(m.normal, Vec3::Z);
        let m = MoveHit::from_world(&hit(0.5, Vec3::splat(1.0e30)));
        assert_eq!(m.normal, Vec3::ZERO);
    }

    #[test]
    fn slope_slide_clamp_depends_on_slope_boost_friction() {
        // Moving horizontally into a 45° ramp facing −X: the slide goes up.
        let n = Vec3::new(-1.0, 0.0, 1.0).normalize();
        let adjusted = Vec3::new(10.0, 0.0, 0.0);
        let free = calculate_slope_slide(adjusted, n, 0.0, 0.0);
        assert!((free - Vec3::new(5.0, 0.0, 5.0)).length() < 1e-5, "{free}");
        let clamped = calculate_slope_slide(adjusted, n, 0.0, 0.5);
        assert_eq!(clamped.z, 0.0, "no height gained beyond the move's own");
        assert!((clamped.x - 5.0).abs() < 1e-5);
    }
}
