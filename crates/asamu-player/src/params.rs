//! Movement, grapple, camera and pawn-script parameters.
//!
//! # Two parameter sets
//!
//! - [`PlayerParams::asamu_original`]: the values of *A Story About My
//!   Uncle*, recovered from the original's class default objects and `.ini`
//!   files (`docs/reverse-engineering/DEFAULTS.md`,
//!   `docs/reverse-engineering/data/defaults/*.json`). Every recovered value
//!   carries [`Provenance::ScriptDefault`] (`class` = the class whose default
//!   object stores it, `property` = the property name) or
//!   [`Provenance::Config`] (`key` = `[Section] Key`). A test
//!   (`tests/original_params.rs`) checks each value and provenance against
//!   the committed JSON, so drift is caught. The only placeholders left in
//!   this set are values read **only** by the debug models of the raw
//!   pipeline: `movement.gravity_z` and `movement.braking_deceleration`
//!   ([`crate::movement::PlaceholderMovement`]) and the rope group
//!   `grapple.*` ([`crate::grapple`], the placeholder rope grapple that runs
//!   only without the script layer). It enables the ASAMU pawn script layer
//!   ([`PlayerParams::pawn`], [`crate::pawn`]) with the original grapple gun
//!   ([`PlayerParams::gun`], [`crate::grapple_gun`]) and rocket boots
//!   ([`PlayerParams::boots`], [`crate::rocket_boots`]).
//! - [`PlayerParams::placeholder`] (= [`Default`]): the graybox
//!   placeholders, kept for the placeholder model and for tests. They were
//!   chosen by us so the graybox prototype is playable; they are **not**
//!   values from the original game (except `movement.world_gravity_z`). If
//!   one happens to coincide with an engine default, that coincidence is
//!   **not** evidence about ASAMU. Each one is a [`Param`] carrying
//!   [`Provenance::Placeholder`] with a note. This set has no pawn script
//!   parameters (`pawn: None`), so the simulation runs the raw physics
//!   without the ASAMU script layer.
//!
//! Replacing a value means calling [`Param::set`] with the real
//! [`Provenance`] (script default, script code, native code, config or
//! measured trace) — never editing the number alone.
//!
//! All distances are Unreal units (UU), times are seconds, angles as noted.
//! [`PlayerParams::provenance_report`] lists every parameter; a test asserts
//! the report is complete.

use asamu_core::{Param, Provenance};
use serde::{Deserialize, Serialize};
use thiserror::Error;

fn ph<T>(value: T, note: &str) -> Param<T> {
    Param::placeholder(value, note)
}

/// A value stored by the class default object of `class` (the most derived
/// class in the inheritance chain whose default object stores it).
fn sd<T>(value: T, class: &str, property: &str) -> Param<T> {
    Param::new(
        value,
        Provenance::ScriptDefault {
            class: class.to_owned(),
            property: property.to_owned(),
        },
    )
}

/// A value from an original `.ini` file; `key` is written `[Section] Key`.
fn cfg<T>(value: T, file: &str, section: &str, key: &str) -> Param<T> {
    Param::new(
        value,
        Provenance::Config {
            file: file.to_owned(),
            key: format!("[{section}] {key}"),
        },
    )
}

/// `asamu.ASAMUPawn`, the player pawn class (package `asamu` in `Startup.upk`).
pub const CLASS_ASAMU_PAWN: &str = "asamu.ASAMUPawn";
/// `UTGame.UTPawn`, the parent of `ASAMUPawn`.
pub const CLASS_UT_PAWN: &str = "UTGame.UTPawn";
/// `UDKBase.UDKPawn`.
pub const CLASS_UDK_PAWN: &str = "UDKBase.UDKPawn";
/// `Engine.Pawn`.
pub const CLASS_PAWN: &str = "Engine.Pawn";
/// `Engine.Actor`.
pub const CLASS_ACTOR: &str = "Engine.Actor";
/// `Engine.PhysicsVolume` (the only physics volume at run time is the
/// level's default one; no AG map places a `PhysicsVolume`).
pub const CLASS_PHYSICS_VOLUME: &str = "Engine.PhysicsVolume";
/// `asamu.ASAMUPowerJump`, the power-jump actor spawned by the pawn.
pub const CLASS_ASAMU_POWER_JUMP: &str = "asamu.ASAMUPowerJump";
/// `asamu.GrappleGun`, the grapple gun (`UDKWeapon`) given to the pawn.
pub const CLASS_GRAPPLE_GUN: &str = "asamu.GrappleGun";
/// `Engine.Weapon`, the stock weapon class `GrappleGun` derives from.
pub const CLASS_WEAPON: &str = "Engine.Weapon";
/// `asamu.ASAMURocketBoots`, the rocket-boots actor spawned by the pawn.
pub const CLASS_ASAMU_ROCKET_BOOTS: &str = "asamu.ASAMURocketBoots";
/// Config file holding `[UTGame.UTPawn]` and `[Engine.WorldInfo]`.
pub const CONFIG_FILE_GAME: &str = "ASAMU/Config/DefaultGame.ini";
/// Config file holding the player settings (`[ASAMU.ASAMUSettingsManager]`).
pub const CONFIG_FILE_SETTINGS: &str = "ASAMU/Config/DefaultSettings.ini";

/// Rotator units in a full turn (UE3 `Rotator`), for pitch limits.
const ROTATOR_UNITS_PER_TURN: f32 = 65_536.0;

/// Converts rotator units to degrees (`18000` → `98.876953125`, exact in `f32`).
#[must_use]
pub fn rotator_units_to_degrees(units: f32) -> f32 {
    units * (360.0 / ROTATOR_UNITS_PER_TURN)
}

/// Original config file holding `[Engine.WorldInfo] DefaultGravityZ`
/// (relative to the game's `Contents/Resources` directory; no user paths).
pub const DEFAULT_GRAVITY_CONFIG_FILE: &str = "ASAMU/Config/DefaultGame.ini";

/// Config key (`[Section] Key`) of the original's default world gravity.
pub const DEFAULT_GRAVITY_CONFIG_KEY: &str = "[Engine.WorldInfo] DefaultGravityZ";

/// `DefaultGravityZ` from the original's `ASAMU/Config/DefaultGame.ini`
/// (`[Engine.WorldInfo] DefaultGravityZ=-520.0`, overriding the engine's
/// `BaseGame.ini` value). CONFIRMED (config bytes; read by
/// `AWorldInfo::GetGravityZ @ 0x100AD7B10` via WorldInfo+0x600, see
/// `docs/reverse-engineering/NATIVE_PHYSICS.md` 4.1). Per-map overrides
/// (WorldInfo `GlobalGravityZ`/`WorldGravityZ`, GravityVolumes) are UNKNOWN.
pub const CONFIG_DEFAULT_GRAVITY_Z: f32 = -520.0;

/// Ground/air locomotion parameters (consumed by the movement model).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MovementParams {
    /// Gravity along Z in UU/s² (negative = down).
    pub gravity_z: Param<f32>,
    /// Maximum horizontal speed reached by walking input, UU/s.
    pub max_ground_speed: Param<f32>,
    /// Horizontal acceleration towards the wish velocity on the ground, UU/s².
    pub ground_acceleration: Param<f32>,
    /// Horizontal deceleration on the ground with no input, UU/s².
    pub braking_deceleration: Param<f32>,
    /// Fraction of `ground_acceleration` available while airborne, `[0, 1]`.
    pub air_control: Param<f32>,
    /// Vertical velocity set by a jump, UU/s.
    pub jump_velocity: Param<f32>,
    /// Radius of the collision shape, UU.
    pub capsule_radius: Param<f32>,
    /// Half of the collision shape's height, UU.
    pub capsule_half_height: Param<f32>,
    /// Highest ledge the walking move steps up onto, UU.
    pub step_height: Param<f32>,
    /// Cap on downward speed, UU/s.
    pub max_fall_speed: Param<f32>,
    /// Minimum floor-normal Z for a surface to count as walkable, `(0, 1]`.
    pub walkable_floor_z: Param<f32>,
    /// World gravity along Z in UU/s² used by
    /// [`crate::ue3_movement::Ue3PawnMovement`] (UE3 `WorldInfo` gravity:
    /// `GlobalGravityZ` when non-zero, else `DefaultGravityZ`). The
    /// placeholder model uses [`Self::gravity_z`] instead.
    pub world_gravity_z: Param<f32>,
    /// Pawn gravity multiplier (UE3 `UDKPawn.CustomGravityScaling`, final
    /// factor of `AUDKPawn::GetGravityZ`).
    pub custom_gravity_scaling: Param<f32>,
    /// Physics-volume ground friction (UE3 `PhysicsVolume.GroundFriction`):
    /// walking turning friction, braking factor `2·friction`, slope slide.
    pub ground_friction: Param<f32>,
    /// Physics-volume terminal velocity (UE3 `PhysicsVolume.TerminalVelocity`),
    /// a 3-D speed clamp applied after each falling sub-step, UU/s.
    pub terminal_velocity: Param<f32>,
    /// Pawn flag (Pawn+0x298 bit 51, hypothesis `bLimitFallAccel`) that
    /// enables the air-acceleration limiter (`AccelRate · AirControl`).
    pub limit_fall_accel: Param<bool>,
    /// UE3 `UDKPawn.SlopeBoostFriction`: 0 disables the clamp that stops a
    /// falling slide from gaining height ("slope boosting"); any other value
    /// keeps the clamp on surfaces without a physical material (all surfaces
    /// of our collision worlds).
    pub slope_boost_friction: Param<f32>,
    /// UE3 `Pawn.MovementSpeedModifier`: final factor of `MaxSpeedModifier`,
    /// which scales the walking speed cap (not the acceleration).
    pub movement_speed_modifier: Param<f32>,
    /// UE3 `Pawn.AirSpeed`, the flying speed cap (class default). The
    /// original only flies while the grapple is attached, and the grapple
    /// gun overwrites the run-time value with `gun.grapple_accel`
    /// (`GRAPPLE.md` G-PH-4); without the script layer the flying physics
    /// reads this value.
    pub air_speed: Param<f32>,
    /// UE3 `PhysicsVolume.FluidFriction`: flying drag is `0.5 ×` this
    /// (`GRAPPLE.md` G-PH-3). One implicit physics volume.
    pub fluid_friction: Param<f32>,
}

impl Default for MovementParams {
    fn default() -> Self {
        Self {
            gravity_z: ph(
                -1000.0,
                "graybox placeholder used only by PlaceholderMovement (the UE3 pawn model uses \
                 movement.world_gravity_z x movement.custom_gravity_scaling, doubled in effect by \
                 the falling refinement)",
            ),
            max_ground_speed: ph(
                450.0,
                "graybox placeholder (debugging); the original value is in PlayerParams::asamu_original (UTPawn GroundSpeed)",
            ),
            ground_acceleration: ph(
                3000.0,
                "graybox placeholder (debugging); the original value is in PlayerParams::asamu_original (Pawn AccelRate)",
            ),
            braking_deceleration: ph(
                3000.0,
                "graybox placeholder used only by PlaceholderMovement (the original brakes with \
                 2 x ground friction, see movement.ground_friction)",
            ),
            air_control: ph(
                0.25,
                "graybox placeholder (debugging); the original value is in PlayerParams::asamu_original (ASAMUPawn AirControl)",
            ),
            jump_velocity: ph(
                450.0,
                "graybox placeholder (debugging); the original value is in PlayerParams::asamu_original (ASAMUPawn JumpZ)",
            ),
            capsule_radius: ph(
                20.0,
                "graybox placeholder (debugging); the original value is in PlayerParams::asamu_original (UTPawn collision cylinder radius)",
            ),
            capsule_half_height: ph(
                45.0,
                "graybox placeholder (debugging); the original value is in PlayerParams::asamu_original (UTPawn collision cylinder height)",
            ),
            step_height: ph(
                18.0,
                "graybox placeholder (debugging); the original value is in PlayerParams::asamu_original (UTPawn MaxStepHeight)",
            ),
            max_fall_speed: ph(
                4000.0,
                "graybox placeholder used only by PlaceholderMovement (the original clamps the \
                 3-D falling speed to movement.terminal_velocity)",
            ),
            walkable_floor_z: ph(
                0.7,
                "graybox placeholder (debugging); the original value is in PlayerParams::asamu_original (UTPawn WalkableFloorZ)",
            ),
            world_gravity_z: Param::new(
                CONFIG_DEFAULT_GRAVITY_Z,
                Provenance::Config {
                    file: DEFAULT_GRAVITY_CONFIG_FILE.to_owned(),
                    key: DEFAULT_GRAVITY_CONFIG_KEY.to_owned(),
                },
            ),
            custom_gravity_scaling: ph(
                1.0,
                "graybox placeholder (debugging); the original value is in PlayerParams::asamu_original (UDKPawn CustomGravityScaling)",
            ),
            ground_friction: ph(
                6.0,
                "graybox placeholder (debugging); the original value is in PlayerParams::asamu_original (PhysicsVolume GroundFriction)",
            ),
            terminal_velocity: ph(
                4000.0,
                "graybox placeholder (debugging); the original value is in PlayerParams::asamu_original (ASAMUPawn fTerminalVelocity)",
            ),
            limit_fall_accel: ph(
                true,
                "graybox placeholder (debugging); the original value is in PlayerParams::asamu_original (Pawn bLimitFallAccel)",
            ),
            slope_boost_friction: ph(
                0.5,
                "graybox placeholder (debugging); the original value is in PlayerParams::asamu_original (UTPawn SlopeBoostFriction)",
            ),
            movement_speed_modifier: ph(
                1.0,
                "graybox placeholder (debugging); the original value is in PlayerParams::asamu_original (Pawn MovementSpeedModifier)",
            ),
            air_speed: ph(
                450.0,
                "graybox placeholder (debugging; nothing flies without the script layer); the original value is in PlayerParams::asamu_original (UTPawn AirSpeed)",
            ),
            fluid_friction: ph(
                0.3,
                "graybox placeholder (debugging; nothing flies without the script layer); the original value is in PlayerParams::asamu_original (PhysicsVolume FluidFriction)",
            ),
        }
    }
}

impl MovementParams {
    /// The graybox placeholders (same as [`Default`]).
    #[must_use]
    pub fn placeholder() -> Self {
        Self::default()
    }

    /// The original's values (see the module docs). `gravity_z` and
    /// `braking_deceleration` stay placeholders: only
    /// [`crate::movement::PlaceholderMovement`] reads them.
    #[must_use]
    pub fn asamu_original() -> Self {
        let placeholder = Self::default();
        Self {
            gravity_z: placeholder.gravity_z,
            max_ground_speed: sd(440.0, CLASS_UT_PAWN, "GroundSpeed"),
            ground_acceleration: sd(2048.0, CLASS_PAWN, "AccelRate"),
            braking_deceleration: placeholder.braking_deceleration,
            air_control: sd(0.3, CLASS_ASAMU_PAWN, "AirControl"),
            jump_velocity: sd(1000.0, CLASS_ASAMU_PAWN, "JumpZ"),
            capsule_radius: sd(21.0, CLASS_UT_PAWN, "CollisionCylinder.CollisionRadius"),
            capsule_half_height: sd(44.0, CLASS_UT_PAWN, "CollisionCylinder.CollisionHeight"),
            step_height: sd(26.0, CLASS_UT_PAWN, "MaxStepHeight"),
            max_fall_speed: sd(2500.0, CLASS_ASAMU_PAWN, "MaxFallSpeed"),
            walkable_floor_z: sd(0.78, CLASS_UT_PAWN, "WalkableFloorZ"),
            world_gravity_z: cfg(
                CONFIG_DEFAULT_GRAVITY_Z,
                CONFIG_FILE_GAME,
                "Engine.WorldInfo",
                "DefaultGravityZ",
            ),
            custom_gravity_scaling: sd(1.0, CLASS_UDK_PAWN, "CustomGravityScaling"),
            ground_friction: sd(8.0, CLASS_PHYSICS_VOLUME, "GroundFriction"),
            // `ASAMUPawn.PostBeginPlay` writes `fTerminalVelocity` into the
            // physics volume's `TerminalVelocity` (volume default 4000), so
            // the effective value is the pawn's (ABILITIES.md §1).
            terminal_velocity: sd(10_000.0, CLASS_ASAMU_PAWN, "fTerminalVelocity"),
            limit_fall_accel: sd(true, CLASS_PAWN, "bLimitFallAccel"),
            slope_boost_friction: sd(0.2, CLASS_UT_PAWN, "SlopeBoostFriction"),
            movement_speed_modifier: sd(1.0, CLASS_PAWN, "MovementSpeedModifier"),
            air_speed: sd(440.0, CLASS_UT_PAWN, "AirSpeed"),
            fluid_friction: sd(0.3, CLASS_PHYSICS_VOLUME, "FluidFriction"),
        }
    }
}

/// How the rope length evolves while attached.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RopeMode {
    /// Length fixed at attach time; the rope can go slack but never stretch.
    Inelastic,
    /// Length ratchets down to the current distance whenever the player gets
    /// closer (never below `min_rope_length`), so pull reels the player in.
    ShortenToDistance,
}

/// What happens to the player's velocity when the grapple is released.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReleaseMode {
    /// Velocity is kept bit-for-bit (momentum preserved).
    PreserveVelocity,
}

/// Parameters of the placeholder **rope** grapple ([`crate::grapple`]),
/// read only by the raw pipeline without the script layer (the original
/// grapple gun reads [`GrappleGunParams`]).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GrappleParams {
    /// Maximum distance from the eye at which a target can be grappled, UU.
    pub max_range: Param<f32>,
    /// Acceleration towards the anchor while attached, UU/s².
    pub pull_acceleration: Param<f32>,
    /// Pull stops (and the rope never shortens) below this distance, UU.
    pub min_rope_length: Param<f32>,
    /// Rope length behaviour.
    pub rope_mode: Param<RopeMode>,
    /// Velocity behaviour on release.
    pub release_mode: Param<ReleaseMode>,
    /// Speed cap applied while attached, UU/s.
    pub attached_max_speed: Param<f32>,
}

impl Default for GrappleParams {
    fn default() -> Self {
        Self {
            max_range: ph(
                2500.0,
                "graybox placeholder of the debug rope grapple (raw pipeline only); the \
                 original grapple uses gun.max_distance (GrappleGun fMaxDistance)",
            ),
            pull_acceleration: ph(
                900.0,
                "graybox placeholder of the debug rope grapple (raw pipeline only); the \
                 original pull is 10^7/d uu/s^2 (GRAPPLE.md G-PH-2)",
            ),
            min_rope_length: ph(
                60.0,
                "graybox placeholder of the debug rope grapple (raw pipeline only); the \
                 original has no rope (GRAPPLE.md G-PH-5)",
            ),
            rope_mode: ph(
                RopeMode::Inelastic,
                "graybox placeholder of the debug rope grapple (raw pipeline only); the \
                 original has no rope (GRAPPLE.md G-PH-5)",
            ),
            release_mode: ph(
                ReleaseMode::PreserveVelocity,
                "graybox placeholder of the debug rope grapple (raw pipeline only); the \
                 original keeps the velocity except on a proximity release (G-RL-1/2)",
            ),
            attached_max_speed: ph(
                2500.0,
                "graybox placeholder of the debug rope grapple (raw pipeline only); the \
                 original caps at AirSpeed = gun.grapple_accel (G-PH-3/4)",
            ),
        }
    }
}

/// First-person camera parameters.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CameraParams {
    /// Horizontal field of view in degrees (UE3 FOV is horizontal). In the
    /// original: the player setting `FOV`, issued as the `FOV` console command
    /// at login, which locks the camera FOV (ABILITIES.md A-CM-3); only the
    /// story-mode zoom changes it during play.
    pub fov_degrees: Param<f32>,
    /// Eye height above the collision-shape centre, UU (UE3
    /// `Pawn.BaseEyeHeight`: the value the run-time `EyeHeight` relaxes to,
    /// see [`crate::pawn`]).
    pub eye_height: Param<f32>,
    /// Pitch limit (symmetric) in degrees. In the original
    /// `UTPawn.ViewPitchMax` = 18000 rotator units ≈ 98.9° (above the
    /// vertical; `ViewPitchMin` is −18000), ABILITIES.md A-CM-2.
    pub max_pitch_degrees: Param<f32>,
}

impl Default for CameraParams {
    fn default() -> Self {
        Self {
            fov_degrees: ph(
                90.0,
                "graybox placeholder (debugging); the original value is in PlayerParams::asamu_original (settings FOV)",
            ),
            eye_height: ph(
                38.0,
                "graybox placeholder (debugging); the original value is in PlayerParams::asamu_original (UTPawn BaseEyeHeight)",
            ),
            max_pitch_degrees: ph(
                89.0,
                "graybox placeholder (debugging); the original value is in PlayerParams::asamu_original (UTPawn ViewPitchMax)",
            ),
        }
    }
}

impl CameraParams {
    /// The original's values.
    #[must_use]
    pub fn asamu_original() -> Self {
        Self {
            fov_degrees: cfg(
                90.0,
                CONFIG_FILE_SETTINGS,
                "ASAMU.ASAMUSettingsManager",
                "FOV",
            ),
            eye_height: sd(38.0, CLASS_UT_PAWN, "BaseEyeHeight"),
            max_pitch_degrees: sd(
                rotator_units_to_degrees(18_000.0),
                CLASS_UT_PAWN,
                "ViewPitchMax",
            ),
        }
    }
}

/// Class-default and config values read by the ASAMU pawn script layer
/// ([`crate::pawn`]): walking/sprint/story speeds, landing, zoom, view bob
/// and the power jump. Rules and literal constants of the script code are
/// named constants in [`crate::pawn`] instead (provenance
/// [`Provenance::ScriptCode`]).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PawnParams {
    /// `ASAMUPawn.MoveSpeed`: the walking `GroundSpeed` written at pawn start
    /// and whenever sprint or story speed is removed, UU/s.
    pub move_speed: Param<f32>,
    /// `ASAMUPawn.bSprintSpeedMultiplier` (a float despite the `b`): sprint
    /// `GroundSpeed = MoveSpeed × this`.
    pub sprint_speed_multiplier: Param<f32>,
    /// `ASAMUPawn.storyModeSpeedMultiplier`: story-mode
    /// `GroundSpeed = MoveSpeed × this`.
    pub story_speed_multiplier: Param<f32>,
    /// `UTPawn.DefaultAirControl`: `AirControl` after every normal landing.
    pub landed_air_control: Param<f32>,
    /// `ASAMUPawn.hardLandingThreshold`: a landing with `V.z` below minus
    /// this is a hard landing (cosmetic effects only), UU/s.
    pub hard_landing_threshold: Param<f32>,
    /// `ASAMUPawn.normalLandSoundVelocityThreshold`: landings with `V.z` at
    /// or below minus this play the landing sound, UU/s.
    pub land_sound_threshold: Param<f32>,
    /// `ASAMUPawn.bZoomEnabled`: story-mode zoom available at start.
    pub zoom_enabled: Param<bool>,
    /// `ASAMUPawn.zoomFOV`: zoomed-in FOV, degrees.
    pub zoom_fov: Param<f32>,
    /// `ASAMUPawn.zoomDuration`: nominal zoom time, s.
    pub zoom_duration: Param<f32>,
    /// `UTPawn.Bob` (globalconfig): walk-bob amplitude factor.
    pub bob: Param<f32>,
    /// `UTPawn.bWeaponBob` (globalconfig): full walk bob when true, ×0.1
    /// when false.
    pub weapon_bob: Param<bool>,
    /// `ASAMUPawn.storyModeBobSpeedMultiplier`: bob phase rate in story mode.
    pub bob_rate_story: Param<f32>,
    /// `ASAMUPawn.sprintingBobSpeedMultiplier`: bob phase rate while sprinting.
    pub bob_rate_sprint: Param<f32>,
    /// `ASAMUPawn.walkingNormallyBobSpeedMultiplier`: bob phase rate otherwise.
    pub bob_rate_walk: Param<f32>,
    /// `Actor.CustomTimeDilation`: divides `dt` in the eye-height smoothing.
    pub custom_time_dilation: Param<f32>,
    /// `ASAMUPowerJump.powerJumpChargeTime`: hold time to charge, s.
    pub power_jump_charge_time: Param<f32>,
    /// `ASAMUPowerJump.powerJumpStrength`: `JumpZ` used by the vertical power
    /// jump, UU/s.
    pub power_jump_strength: Param<f32>,
    /// `ASAMUPowerJump.powerLeapHorizontalStrengthMultiplier`: horizontal
    /// velocity factor of the power leap.
    pub power_leap_horizontal_multiplier: Param<f32>,
    /// `ASAMUPowerJump.powerLeapVerticalStrength`: `V.z` set by the power
    /// leap, UU/s.
    pub power_leap_vertical_strength: Param<f32>,
}

impl PawnParams {
    /// The original's values (class defaults of `asamu.ASAMUPawn`, its
    /// parents and `asamu.ASAMUPowerJump`; `Bob`/`bWeaponBob` from config).
    #[must_use]
    pub fn asamu_original() -> Self {
        Self {
            move_speed: sd(440.0, CLASS_ASAMU_PAWN, "MoveSpeed"),
            sprint_speed_multiplier: sd(2.0, CLASS_ASAMU_PAWN, "bSprintSpeedMultiplier"),
            story_speed_multiplier: sd(0.6, CLASS_ASAMU_PAWN, "storyModeSpeedMultiplier"),
            landed_air_control: sd(0.35, CLASS_UT_PAWN, "DefaultAirControl"),
            hard_landing_threshold: sd(2000.0, CLASS_ASAMU_PAWN, "hardLandingThreshold"),
            land_sound_threshold: sd(500.0, CLASS_ASAMU_PAWN, "normalLandSoundVelocityThreshold"),
            zoom_enabled: sd(true, CLASS_ASAMU_PAWN, "bZoomEnabled"),
            zoom_fov: sd(50.0, CLASS_ASAMU_PAWN, "zoomFOV"),
            zoom_duration: sd(0.3, CLASS_ASAMU_PAWN, "zoomDuration"),
            bob: cfg(0.01, CONFIG_FILE_GAME, "UTGame.UTPawn", "Bob"),
            weapon_bob: cfg(true, CONFIG_FILE_GAME, "UTGame.UTPawn", "bWeaponBob"),
            bob_rate_story: sd(0.55, CLASS_ASAMU_PAWN, "storyModeBobSpeedMultiplier"),
            bob_rate_sprint: sd(0.85, CLASS_ASAMU_PAWN, "sprintingBobSpeedMultiplier"),
            bob_rate_walk: sd(0.65, CLASS_ASAMU_PAWN, "walkingNormallyBobSpeedMultiplier"),
            custom_time_dilation: sd(1.0, CLASS_ACTOR, "CustomTimeDilation"),
            power_jump_charge_time: sd(0.6, CLASS_ASAMU_POWER_JUMP, "powerJumpChargeTime"),
            power_jump_strength: sd(1600.0, CLASS_ASAMU_POWER_JUMP, "powerJumpStrength"),
            power_leap_horizontal_multiplier: sd(
                2.0,
                CLASS_ASAMU_POWER_JUMP,
                "powerLeapHorizontalStrengthMultiplier",
            ),
            power_leap_vertical_strength: sd(
                750.0,
                CLASS_ASAMU_POWER_JUMP,
                "powerLeapVerticalStrength",
            ),
        }
    }
}

/// Class-default values of the original grapple gun (`asamu.GrappleGun` →
/// `UDKWeapon` → `Engine.Weapon`) read by [`crate::grapple_gun`]
/// (`docs/reverse-engineering/GRAPPLE.md` §2). Literal constants of the gun's
/// script code are named constants in [`crate::grapple_gun`] instead.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GrappleGunParams {
    /// `fMaxDistance`: a hit counts only if it is closer than this to the
    /// eye (G-AC-5); also the rocket-boost break distance, UU.
    pub max_distance: Param<f32>,
    /// `Engine.Weapon.WeaponRange`: length of the fire trace (G-TG-1), UU.
    pub weapon_range: Param<f32>,
    /// `fGrappleReleaseDistance`: automatic release closer than this to the
    /// anchor, with the velocity halved (G-PH-2, G-RL-2), UU.
    pub release_distance: Param<f32>,
    /// `fGrappleAccel`: written into the pawn's `AirSpeed`, the flying speed
    /// cap (G-PH-4), UU/s.
    pub grapple_accel: Param<f32>,
    /// `fGrappleMaxSpeed`: never read by any script (G-PH, finding 2);
    /// recorded for completeness, UU/s.
    pub max_speed: Param<f32>,
    /// `FireInterval[0]`: period of the firing state's refire check
    /// (G-IN-2), s.
    pub fire_interval: Param<f32>,
    /// `instantReleaseDelay`: one-shot timer after attaching to a
    /// release-instant target (G-RL-3), s.
    pub instant_release_delay: Param<f32>,
    /// `TOP_GRAPPLE_ANGLE`: `TopOnlyGrappleAble` faces need normal Z at least
    /// this (G-AC-4).
    pub top_grapple_angle: Param<f32>,
    /// `BOTTOM_GRAPPLE_ANGLE`: `BottomOnlyGrappleAble` faces need normal Z at
    /// most minus this (G-AC-4).
    pub bottom_grapple_angle: Param<f32>,
    /// `interactRange`: story-mode interaction range (G-AC-0), UU.
    pub interact_range: Param<f32>,
    /// `iMaxGrapples` before Kismet or a save sets it (G-CT-2): 0.
    pub initial_max_grapples: Param<i32>,
    /// `bCanGrapple` at spawn: the fire latch starts set (G-IN-3).
    pub initial_can_grapple: Param<bool>,
}

impl GrappleGunParams {
    /// The original's values (`GRAPPLE.md` §2, `DEFAULTS.md` §6.2).
    #[must_use]
    pub fn asamu_original() -> Self {
        Self {
            max_distance: sd(5000.0, CLASS_GRAPPLE_GUN, "fMaxDistance"),
            weapon_range: sd(16_384.0, CLASS_WEAPON, "WeaponRange"),
            release_distance: sd(200.0, CLASS_GRAPPLE_GUN, "fGrappleReleaseDistance"),
            grapple_accel: sd(2000.0, CLASS_GRAPPLE_GUN, "fGrappleAccel"),
            max_speed: sd(10_000.0, CLASS_GRAPPLE_GUN, "fGrappleMaxSpeed"),
            fire_interval: sd(0.1, CLASS_GRAPPLE_GUN, "FireInterval[0]"),
            instant_release_delay: sd(0.05, CLASS_GRAPPLE_GUN, "instantReleaseDelay"),
            top_grapple_angle: sd(0.8, CLASS_GRAPPLE_GUN, "TOP_GRAPPLE_ANGLE"),
            bottom_grapple_angle: sd(0.8, CLASS_GRAPPLE_GUN, "BOTTOM_GRAPPLE_ANGLE"),
            interact_range: sd(200.0, CLASS_GRAPPLE_GUN, "interactRange"),
            initial_max_grapples: sd(0, CLASS_GRAPPLE_GUN, "iMaxGrapples"),
            initial_can_grapple: sd(true, CLASS_GRAPPLE_GUN, "bCanGrapple"),
        }
    }
}

/// Class-default values of the original rocket boots (`asamu.ASAMURocketBoots`)
/// read by [`crate::rocket_boots`] (`docs/reverse-engineering/ABILITIES.md`
/// §7).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RocketBootsParams {
    /// `boostDelay`: charge length before the boost (A-RB-3/4), s.
    pub boost_delay: Param<f32>,
    /// `boostDuration`: boost length (A-RB-3/4), s.
    pub boost_duration: Param<f32>,
    /// `boostStrength`: boost speed along the aim at the start (it falls to
    /// half by the end), UU/s.
    pub boost_strength: Param<f32>,
    /// `boostSpiralStrength`: corkscrew amplitude at the start, UU/s.
    pub boost_spiral_strength: Param<f32>,
    /// `totalSpinAngle`: corkscrew angle over the whole boost, degrees.
    pub total_spin_angle: Param<f32>,
    /// `boostExhaustedDelay`: "exhausted" sound throttle (A-RB-5), s.
    pub boost_exhausted_delay: Param<f32>,
    /// `bEnabled` at spawn: false until Kismet enables the boots (A-RB-1).
    pub initial_enabled: Param<bool>,
}

impl RocketBootsParams {
    /// The original's values (`ABILITIES.md` §7, `DEFAULTS.md` §6.3).
    #[must_use]
    pub fn asamu_original() -> Self {
        Self {
            boost_delay: sd(1.0, CLASS_ASAMU_ROCKET_BOOTS, "boostDelay"),
            boost_duration: sd(2.0, CLASS_ASAMU_ROCKET_BOOTS, "boostDuration"),
            boost_strength: sd(2500.0, CLASS_ASAMU_ROCKET_BOOTS, "boostStrength"),
            boost_spiral_strength: sd(1000.0, CLASS_ASAMU_ROCKET_BOOTS, "boostSpiralStrength"),
            total_spin_angle: sd(720.0, CLASS_ASAMU_ROCKET_BOOTS, "totalSpinAngle"),
            boost_exhausted_delay: sd(1.0, CLASS_ASAMU_ROCKET_BOOTS, "boostExhaustedDelay"),
            initial_enabled: sd(false, CLASS_ASAMU_ROCKET_BOOTS, "bEnabled"),
        }
    }
}

/// All player simulation parameters.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlayerParams {
    /// Locomotion.
    pub movement: MovementParams,
    /// The placeholder **rope** grapple of the debug configuration
    /// ([`crate::grapple`]); read only by the raw pipeline (`pawn: None`).
    pub grapple: GrappleParams,
    /// Camera.
    pub camera: CameraParams,
    /// Values of the ASAMU pawn script layer. `Some` runs the script layer
    /// ([`crate::pawn`]) in [`crate::sim::step_with`]; `None` (the
    /// placeholder set) runs the raw physics without it.
    #[serde(default)]
    pub pawn: Option<PawnParams>,
    /// Values of the original grapple gun, read by the script layer. `None`
    /// means the pawn has no grapple gun (fire does nothing).
    #[serde(default)]
    pub gun: Option<GrappleGunParams>,
    /// Values of the original rocket boots, read by the script layer. `None`
    /// means the pawn has no rocket boots.
    #[serde(default)]
    pub boots: Option<RocketBootsParams>,
}

/// One row of [`PlayerParams::provenance_report`].
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ParamReportEntry {
    /// Dotted path matching the serialized structure, e.g. `movement.gravity_z`.
    pub name: String,
    /// The value, formatted.
    pub value: String,
    /// Unit of the value.
    pub unit: &'static str,
    /// What the parameter does.
    pub description: &'static str,
    /// Where the value came from.
    pub provenance: Provenance,
}

/// Errors from [`PlayerParams::validate`].
#[derive(Debug, Clone, PartialEq, Error)]
pub enum ParamError {
    /// A value is outside its allowed range (or not finite).
    #[error("parameter {name} = {value} is invalid: {requirement}")]
    OutOfRange {
        /// Dotted parameter name.
        name: &'static str,
        /// Offending value.
        value: f32,
        /// What the value must satisfy.
        requirement: &'static str,
    },
}

fn num(
    out: &mut Vec<ParamReportEntry>,
    name: &str,
    p: &Param<f32>,
    unit: &'static str,
    description: &'static str,
) {
    out.push(ParamReportEntry {
        name: name.to_owned(),
        value: format!("{}", p.value),
        unit,
        description,
        provenance: p.provenance.clone(),
    });
}

fn int(
    out: &mut Vec<ParamReportEntry>,
    name: &str,
    p: &Param<i32>,
    unit: &'static str,
    description: &'static str,
) {
    out.push(ParamReportEntry {
        name: name.to_owned(),
        value: format!("{}", p.value),
        unit,
        description,
        provenance: p.provenance.clone(),
    });
}

fn gun_report(out: &mut Vec<ParamReportEntry>, g: &GrappleGunParams) {
    num(
        out,
        "gun.max_distance",
        &g.max_distance,
        "uu",
        "Grapple acceptance range from the eye (also the boost break distance)",
    );
    num(
        out,
        "gun.weapon_range",
        &g.weapon_range,
        "uu",
        "Length of the grapple fire trace",
    );
    num(
        out,
        "gun.release_distance",
        &g.release_distance,
        "uu",
        "Automatic release (velocity halved) below this anchor distance",
    );
    num(
        out,
        "gun.grapple_accel",
        &g.grapple_accel,
        "uu/s",
        "Written into AirSpeed: the flying speed cap while attached",
    );
    num(
        out,
        "gun.max_speed",
        &g.max_speed,
        "uu/s",
        "Never read by the original script (recorded only)",
    );
    num(
        out,
        "gun.fire_interval",
        &g.fire_interval,
        "s",
        "Refire-check period of the firing state",
    );
    num(
        out,
        "gun.instant_release_delay",
        &g.instant_release_delay,
        "s",
        "Release timer after grappling a release-instant target",
    );
    num(
        out,
        "gun.top_grapple_angle",
        &g.top_grapple_angle,
        "normal z",
        "TopOnlyGrappleAble faces need normal z >= this",
    );
    num(
        out,
        "gun.bottom_grapple_angle",
        &g.bottom_grapple_angle,
        "normal z",
        "BottomOnlyGrappleAble faces need normal z <= -this",
    );
    num(
        out,
        "gun.interact_range",
        &g.interact_range,
        "uu",
        "Story-mode interaction range of the fire button",
    );
    int(
        out,
        "gun.initial_max_grapples",
        &g.initial_max_grapples,
        "count",
        "Grapple capacity before Kismet/save sets it",
    );
    choice(
        out,
        "gun.initial_can_grapple",
        &g.initial_can_grapple,
        "Fire latch at spawn",
    );
}

fn boots_report(out: &mut Vec<ParamReportEntry>, b: &RocketBootsParams) {
    num(
        out,
        "boots.boost_delay",
        &b.boost_delay,
        "s",
        "Rocket-boost charge time",
    );
    num(
        out,
        "boots.boost_duration",
        &b.boost_duration,
        "s",
        "Rocket-boost duration",
    );
    num(
        out,
        "boots.boost_strength",
        &b.boost_strength,
        "uu/s",
        "Boost speed along the aim at the start (half at the end)",
    );
    num(
        out,
        "boots.boost_spiral_strength",
        &b.boost_spiral_strength,
        "uu/s",
        "Corkscrew amplitude at the start",
    );
    num(
        out,
        "boots.total_spin_angle",
        &b.total_spin_angle,
        "deg",
        "Corkscrew angle over the whole boost",
    );
    num(
        out,
        "boots.boost_exhausted_delay",
        &b.boost_exhausted_delay,
        "s",
        "Exhausted-sound throttle",
    );
    choice(
        out,
        "boots.initial_enabled",
        &b.initial_enabled,
        "Rocket boots enabled at spawn",
    );
}

fn choice<T: Serialize>(
    out: &mut Vec<ParamReportEntry>,
    name: &str,
    p: &Param<T>,
    description: &'static str,
) {
    let value = match serde_json::to_value(&p.value) {
        Ok(serde_json::Value::String(s)) => s,
        Ok(other) => other.to_string(),
        Err(e) => format!("<unserializable: {e}>"),
    };
    out.push(ParamReportEntry {
        name: name.to_owned(),
        value,
        unit: "-",
        description,
        provenance: p.provenance.clone(),
    });
}

fn pawn_report(out: &mut Vec<ParamReportEntry>, p: &PawnParams) {
    num(
        out,
        "pawn.move_speed",
        &p.move_speed,
        "uu/s",
        "Walking GroundSpeed set by the pawn script",
    );
    num(
        out,
        "pawn.sprint_speed_multiplier",
        &p.sprint_speed_multiplier,
        "factor",
        "Sprint GroundSpeed = move_speed x this",
    );
    num(
        out,
        "pawn.story_speed_multiplier",
        &p.story_speed_multiplier,
        "factor",
        "Story-mode GroundSpeed = move_speed x this",
    );
    num(
        out,
        "pawn.landed_air_control",
        &p.landed_air_control,
        "fraction",
        "AirControl after a normal landing",
    );
    num(
        out,
        "pawn.hard_landing_threshold",
        &p.hard_landing_threshold,
        "uu/s",
        "Hard (cosmetic) landing when V.z < -this",
    );
    num(
        out,
        "pawn.land_sound_threshold",
        &p.land_sound_threshold,
        "uu/s",
        "Landing sound when V.z <= -this",
    );
    choice(
        out,
        "pawn.zoom_enabled",
        &p.zoom_enabled,
        "Story-mode zoom available at start",
    );
    num(
        out,
        "pawn.zoom_fov",
        &p.zoom_fov,
        "deg (horizontal)",
        "Zoomed-in FOV (story mode)",
    );
    num(
        out,
        "pawn.zoom_duration",
        &p.zoom_duration,
        "s",
        "Nominal zoom time",
    );
    num(
        out,
        "pawn.bob",
        &p.bob,
        "factor",
        "Walk-bob amplitude factor",
    );
    choice(
        out,
        "pawn.weapon_bob",
        &p.weapon_bob,
        "Full walk bob (x0.1 when false)",
    );
    num(
        out,
        "pawn.bob_rate_story",
        &p.bob_rate_story,
        "factor",
        "Bob phase rate in story mode",
    );
    num(
        out,
        "pawn.bob_rate_sprint",
        &p.bob_rate_sprint,
        "factor",
        "Bob phase rate while sprinting",
    );
    num(
        out,
        "pawn.bob_rate_walk",
        &p.bob_rate_walk,
        "factor",
        "Bob phase rate when walking normally",
    );
    num(
        out,
        "pawn.custom_time_dilation",
        &p.custom_time_dilation,
        "factor",
        "Divides dt in the eye-height smoothing",
    );
    num(
        out,
        "pawn.power_jump_charge_time",
        &p.power_jump_charge_time,
        "s",
        "Power-jump charge time",
    );
    num(
        out,
        "pawn.power_jump_strength",
        &p.power_jump_strength,
        "uu/s",
        "JumpZ of the vertical power jump",
    );
    num(
        out,
        "pawn.power_leap_horizontal_multiplier",
        &p.power_leap_horizontal_multiplier,
        "factor",
        "Horizontal velocity factor of the power leap",
    );
    num(
        out,
        "pawn.power_leap_vertical_strength",
        &p.power_leap_vertical_strength,
        "uu/s",
        "V.z set by the power leap",
    );
}

fn validate_pawn(p: &PawnParams) -> Result<(), ParamError> {
    fn check(
        name: &'static str,
        v: f32,
        ok: bool,
        requirement: &'static str,
    ) -> Result<(), ParamError> {
        if v.is_finite() && ok {
            Ok(())
        } else {
            Err(ParamError::OutOfRange {
                name,
                value: v,
                requirement,
            })
        }
    }
    let non_negative: [(&'static str, f32); 13] = [
        ("pawn.move_speed", p.move_speed.value),
        (
            "pawn.sprint_speed_multiplier",
            p.sprint_speed_multiplier.value,
        ),
        (
            "pawn.story_speed_multiplier",
            p.story_speed_multiplier.value,
        ),
        (
            "pawn.hard_landing_threshold",
            p.hard_landing_threshold.value,
        ),
        ("pawn.land_sound_threshold", p.land_sound_threshold.value),
        ("pawn.bob_rate_story", p.bob_rate_story.value),
        ("pawn.bob_rate_sprint", p.bob_rate_sprint.value),
        ("pawn.bob_rate_walk", p.bob_rate_walk.value),
        (
            "pawn.power_jump_charge_time",
            p.power_jump_charge_time.value,
        ),
        ("pawn.power_jump_strength", p.power_jump_strength.value),
        (
            "pawn.power_leap_horizontal_multiplier",
            p.power_leap_horizontal_multiplier.value,
        ),
        (
            "pawn.power_leap_vertical_strength",
            p.power_leap_vertical_strength.value,
        ),
        ("pawn.zoom_duration", p.zoom_duration.value),
    ];
    for (name, v) in non_negative {
        check(name, v, v >= 0.0, ">= 0")?;
    }
    let v = p.zoom_duration.value;
    check("pawn.zoom_duration", v, v > 0.0, "> 0")?;
    let v = p.landed_air_control.value;
    check(
        "pawn.landed_air_control",
        v,
        (0.0..=1.0).contains(&v),
        "in [0, 1]",
    )?;
    let v = p.zoom_fov.value;
    check("pawn.zoom_fov", v, v > 0.0 && v < 180.0, "in (0, 180)")?;
    let v = p.bob.value;
    check("pawn.bob", v, true, "finite")?;
    let v = p.custom_time_dilation.value;
    check("pawn.custom_time_dilation", v, v > 0.0, "> 0")?;
    Ok(())
}

fn check_param(
    name: &'static str,
    v: f32,
    ok: bool,
    requirement: &'static str,
) -> Result<(), ParamError> {
    if v.is_finite() && ok {
        Ok(())
    } else {
        Err(ParamError::OutOfRange {
            name,
            value: v,
            requirement,
        })
    }
}

fn validate_gun(g: &GrappleGunParams) -> Result<(), ParamError> {
    let v = g.max_distance.value;
    check_param("gun.max_distance", v, v > 0.0, "> 0")?;
    let v = g.weapon_range.value;
    check_param("gun.weapon_range", v, v > 0.0, "> 0")?;
    let v = g.release_distance.value;
    check_param("gun.release_distance", v, v >= 0.0, ">= 0")?;
    let v = g.grapple_accel.value;
    check_param("gun.grapple_accel", v, v >= 0.0, ">= 0")?;
    let v = g.max_speed.value;
    check_param("gun.max_speed", v, true, "finite")?;
    let v = g.fire_interval.value;
    check_param("gun.fire_interval", v, v > 0.0, "> 0")?;
    let v = g.instant_release_delay.value;
    check_param("gun.instant_release_delay", v, v >= 0.0, ">= 0")?;
    let v = g.top_grapple_angle.value;
    check_param("gun.top_grapple_angle", v, true, "finite")?;
    let v = g.bottom_grapple_angle.value;
    check_param("gun.bottom_grapple_angle", v, true, "finite")?;
    let v = g.interact_range.value;
    check_param("gun.interact_range", v, v >= 0.0, ">= 0")?;
    Ok(())
}

fn validate_boots(b: &RocketBootsParams) -> Result<(), ParamError> {
    let v = b.boost_delay.value;
    check_param("boots.boost_delay", v, v > 0.0, "> 0")?;
    let v = b.boost_duration.value;
    check_param("boots.boost_duration", v, v > 0.0, "> 0")?;
    let v = b.boost_strength.value;
    check_param("boots.boost_strength", v, true, "finite")?;
    let v = b.boost_spiral_strength.value;
    check_param("boots.boost_spiral_strength", v, true, "finite")?;
    let v = b.total_spin_angle.value;
    check_param("boots.total_spin_angle", v, true, "finite")?;
    let v = b.boost_exhausted_delay.value;
    check_param("boots.boost_exhausted_delay", v, v >= 0.0, ">= 0")?;
    Ok(())
}

impl PlayerParams {
    /// The graybox placeholders (same as [`Default`]): placeholder movement,
    /// grapple and camera values and no pawn script layer.
    #[must_use]
    pub fn placeholder() -> Self {
        Self::default()
    }

    /// The original game's values with provenance (see the module docs):
    /// the ASAMU pawn script layer with the original grapple gun and rocket
    /// boots. Only values read by the debug models (the placeholder movement
    /// model and the rope grapple of the raw pipeline) are placeholders.
    #[must_use]
    pub fn asamu_original() -> Self {
        Self {
            movement: MovementParams::asamu_original(),
            grapple: GrappleParams::default(),
            camera: CameraParams::asamu_original(),
            pawn: Some(PawnParams::asamu_original()),
            gun: Some(GrappleGunParams::asamu_original()),
            boots: Some(RocketBootsParams::asamu_original()),
        }
    }

    /// Every parameter with its value, unit, description and provenance, in a
    /// stable order. Names are dotted paths matching the serde structure.
    #[must_use]
    pub fn provenance_report(&self) -> Vec<ParamReportEntry> {
        let m = &self.movement;
        let g = &self.grapple;
        let c = &self.camera;
        let mut out = Vec::new();
        num(
            &mut out,
            "movement.gravity_z",
            &m.gravity_z,
            "uu/s^2",
            "Gravity along Z (negative = down)",
        );
        num(
            &mut out,
            "movement.max_ground_speed",
            &m.max_ground_speed,
            "uu/s",
            "Max horizontal walking speed",
        );
        num(
            &mut out,
            "movement.ground_acceleration",
            &m.ground_acceleration,
            "uu/s^2",
            "Ground acceleration towards wish velocity",
        );
        num(
            &mut out,
            "movement.braking_deceleration",
            &m.braking_deceleration,
            "uu/s^2",
            "Ground deceleration without input",
        );
        num(
            &mut out,
            "movement.air_control",
            &m.air_control,
            "fraction",
            "Share of ground acceleration available in air",
        );
        num(
            &mut out,
            "movement.jump_velocity",
            &m.jump_velocity,
            "uu/s",
            "Vertical velocity set by a jump",
        );
        num(
            &mut out,
            "movement.capsule_radius",
            &m.capsule_radius,
            "uu",
            "Collision shape radius",
        );
        num(
            &mut out,
            "movement.capsule_half_height",
            &m.capsule_half_height,
            "uu",
            "Collision shape half-height",
        );
        num(
            &mut out,
            "movement.step_height",
            &m.step_height,
            "uu",
            "Max step-up height while walking",
        );
        num(
            &mut out,
            "movement.max_fall_speed",
            &m.max_fall_speed,
            "uu/s",
            "Downward speed cap of PlaceholderMovement (original MaxFallSpeed: landing sound cues only)",
        );
        num(
            &mut out,
            "movement.walkable_floor_z",
            &m.walkable_floor_z,
            "normal z",
            "Min floor normal Z that counts as walkable",
        );
        num(
            &mut out,
            "movement.world_gravity_z",
            &m.world_gravity_z,
            "uu/s^2",
            "World gravity along Z for the UE3 pawn model",
        );
        num(
            &mut out,
            "movement.custom_gravity_scaling",
            &m.custom_gravity_scaling,
            "factor",
            "Pawn gravity multiplier (UE3 pawn model)",
        );
        num(
            &mut out,
            "movement.ground_friction",
            &m.ground_friction,
            "1/s",
            "Ground friction (UE3 pawn model)",
        );
        num(
            &mut out,
            "movement.terminal_velocity",
            &m.terminal_velocity,
            "uu/s",
            "3-D speed clamp while falling (UE3 pawn model)",
        );
        choice(
            &mut out,
            "movement.limit_fall_accel",
            &m.limit_fall_accel,
            "Air-acceleration limiter enabled (UE3 pawn model)",
        );
        num(
            &mut out,
            "movement.slope_boost_friction",
            &m.slope_boost_friction,
            "friction",
            "0 = falling slides may gain height (UE3 pawn model)",
        );
        num(
            &mut out,
            "movement.movement_speed_modifier",
            &m.movement_speed_modifier,
            "factor",
            "Walking speed-cap multiplier (UE3 pawn model)",
        );
        num(
            &mut out,
            "movement.air_speed",
            &m.air_speed,
            "uu/s",
            "Flying speed cap (class default; the gun writes gun.grapple_accel)",
        );
        num(
            &mut out,
            "movement.fluid_friction",
            &m.fluid_friction,
            "1/s",
            "Physics-volume fluid friction (flying drag 0.5 x this)",
        );
        num(
            &mut out,
            "grapple.max_range",
            &g.max_range,
            "uu",
            "Max grapple distance from the eye",
        );
        num(
            &mut out,
            "grapple.pull_acceleration",
            &g.pull_acceleration,
            "uu/s^2",
            "Acceleration towards the anchor while attached",
        );
        num(
            &mut out,
            "grapple.min_rope_length",
            &g.min_rope_length,
            "uu",
            "No pull / no shortening below this distance",
        );
        choice(
            &mut out,
            "grapple.rope_mode",
            &g.rope_mode,
            "Rope length behaviour",
        );
        choice(
            &mut out,
            "grapple.release_mode",
            &g.release_mode,
            "Velocity behaviour on release",
        );
        num(
            &mut out,
            "grapple.attached_max_speed",
            &g.attached_max_speed,
            "uu/s",
            "Speed cap while attached",
        );
        num(
            &mut out,
            "camera.fov_degrees",
            &c.fov_degrees,
            "deg (horizontal)",
            "Field of view",
        );
        num(
            &mut out,
            "camera.eye_height",
            &c.eye_height,
            "uu",
            "Eye height above collision centre",
        );
        num(
            &mut out,
            "camera.max_pitch_degrees",
            &c.max_pitch_degrees,
            "deg",
            "Symmetric view pitch limit",
        );
        if let Some(p) = &self.pawn {
            pawn_report(&mut out, p);
        }
        if let Some(g) = &self.gun {
            gun_report(&mut out, g);
        }
        if let Some(b) = &self.boots {
            boots_report(&mut out, b);
        }
        out
    }

    /// `true` if every parameter is still a placeholder (false for the
    /// defaults since `movement.world_gravity_z` was recovered from config).
    #[must_use]
    pub fn all_placeholders(&self) -> bool {
        self.provenance_report()
            .iter()
            .all(|e| e.provenance.is_placeholder())
    }

    /// Names of parameters that are still placeholders.
    #[must_use]
    pub fn placeholder_names(&self) -> Vec<String> {
        self.provenance_report()
            .into_iter()
            .filter(|e| e.provenance.is_placeholder())
            .map(|e| e.name)
            .collect()
    }

    /// Renders the report as a Markdown table (used for `docs/PARITY.md`).
    #[must_use]
    pub fn provenance_markdown_table(&self) -> String {
        let mut s = String::from(
            "| Parameter | Value | Unit | Provenance | Note / source |\n|---|---|---|---|---|\n",
        );
        for e in self.provenance_report() {
            let detail = match &e.provenance {
                Provenance::Placeholder { note } => note.clone(),
                other => other.to_string(),
            };
            s.push_str(&format!(
                "| `{}` | {} | {} | {} | {} |\n",
                e.name,
                e.value,
                e.unit,
                e.provenance.kind(),
                detail.replace('|', "\\|")
            ));
        }
        s
    }

    /// Checks every numeric parameter is finite and in a sane range.
    ///
    /// # Errors
    /// The first [`ParamError::OutOfRange`] found.
    pub fn validate(&self) -> Result<(), ParamError> {
        fn check(
            name: &'static str,
            v: f32,
            ok: bool,
            requirement: &'static str,
        ) -> Result<(), ParamError> {
            if v.is_finite() && ok {
                Ok(())
            } else {
                Err(ParamError::OutOfRange {
                    name,
                    value: v,
                    requirement,
                })
            }
        }
        let m = &self.movement;
        let g = &self.grapple;
        let c = &self.camera;
        let v = m.gravity_z.value;
        check("movement.gravity_z", v, v <= 0.0, "<= 0")?;
        let v = m.max_ground_speed.value;
        check("movement.max_ground_speed", v, v >= 0.0, ">= 0")?;
        let v = m.ground_acceleration.value;
        check("movement.ground_acceleration", v, v >= 0.0, ">= 0")?;
        let v = m.braking_deceleration.value;
        check("movement.braking_deceleration", v, v >= 0.0, ">= 0")?;
        let v = m.air_control.value;
        check(
            "movement.air_control",
            v,
            (0.0..=1.0).contains(&v),
            "in [0, 1]",
        )?;
        let v = m.jump_velocity.value;
        check("movement.jump_velocity", v, v >= 0.0, ">= 0")?;
        let v = m.capsule_radius.value;
        check("movement.capsule_radius", v, v > 0.0, "> 0")?;
        let v = m.capsule_half_height.value;
        check("movement.capsule_half_height", v, v > 0.0, "> 0")?;
        let v = m.step_height.value;
        check(
            "movement.step_height",
            v,
            v >= 0.0 && v < 2.0 * m.capsule_half_height.value,
            ">= 0 and < full collision height",
        )?;
        let v = m.max_fall_speed.value;
        check("movement.max_fall_speed", v, v > 0.0, "> 0")?;
        let v = m.walkable_floor_z.value;
        check(
            "movement.walkable_floor_z",
            v,
            v > 0.0 && v <= 1.0,
            "in (0, 1]",
        )?;
        let v = m.world_gravity_z.value;
        check("movement.world_gravity_z", v, v <= 0.0, "<= 0")?;
        let v = m.custom_gravity_scaling.value;
        check("movement.custom_gravity_scaling", v, v >= 0.0, ">= 0")?;
        let v = m.ground_friction.value;
        check("movement.ground_friction", v, v >= 0.0, ">= 0")?;
        let v = m.terminal_velocity.value;
        check("movement.terminal_velocity", v, v > 0.0, "> 0")?;
        let v = m.slope_boost_friction.value;
        check("movement.slope_boost_friction", v, v >= 0.0, ">= 0")?;
        let v = m.movement_speed_modifier.value;
        check("movement.movement_speed_modifier", v, v >= 0.0, ">= 0")?;
        let v = g.max_range.value;
        check("grapple.max_range", v, v > 0.0, "> 0")?;
        let v = g.pull_acceleration.value;
        check("grapple.pull_acceleration", v, v >= 0.0, ">= 0")?;
        let v = g.min_rope_length.value;
        check("grapple.min_rope_length", v, v >= 0.0, ">= 0")?;
        let v = g.attached_max_speed.value;
        check("grapple.attached_max_speed", v, v > 0.0, "> 0")?;
        let v = c.fov_degrees.value;
        check("camera.fov_degrees", v, v > 0.0 && v < 180.0, "in (0, 180)")?;
        let v = c.eye_height.value;
        check("camera.eye_height", v, true, "finite")?;
        let v = c.max_pitch_degrees.value;
        check(
            "camera.max_pitch_degrees",
            v,
            v > 0.0 && v < 180.0,
            "in (0, 180)",
        )?;
        let v = m.air_speed.value;
        check("movement.air_speed", v, v >= 0.0, ">= 0")?;
        let v = m.fluid_friction.value;
        check("movement.fluid_friction", v, v >= 0.0, ">= 0")?;
        if let Some(p) = &self.pawn {
            validate_pawn(p)?;
        }
        if let Some(g) = &self.gun {
            validate_gun(g)?;
        }
        if let Some(b) = &self.boots {
            validate_boots(b)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Collects dotted paths of every serialized `Param` (an object with
    /// exactly `value` and `provenance` keys).
    fn collect_param_paths(v: &serde_json::Value, prefix: &str, out: &mut Vec<String>) {
        if let serde_json::Value::Object(map) = v {
            if map.len() == 2 && map.contains_key("value") && map.contains_key("provenance") {
                out.push(prefix.to_owned());
                return;
            }
            for (k, child) in map {
                let path = if prefix.is_empty() {
                    k.clone()
                } else {
                    format!("{prefix}.{k}")
                };
                collect_param_paths(child, &path, out);
            }
        }
    }

    #[test]
    fn report_is_complete() {
        for params in [PlayerParams::placeholder(), PlayerParams::asamu_original()] {
            let json = serde_json::to_value(&params).unwrap();
            let mut from_serde = Vec::new();
            collect_param_paths(&json, "", &mut from_serde);
            from_serde.sort();
            let mut from_report: Vec<String> = params
                .provenance_report()
                .into_iter()
                .map(|e| e.name)
                .collect();
            let unique_before = from_report.len();
            from_report.sort();
            from_report.dedup();
            assert_eq!(from_report.len(), unique_before, "duplicate report names");
            assert_eq!(
                from_report, from_serde,
                "provenance_report() must list every Param"
            );
            assert!(!from_report.is_empty());
        }
    }

    #[test]
    fn original_set_is_recovered_except_placeholder_model_values_and_grapple() {
        let params = PlayerParams::asamu_original();
        assert_eq!(params.validate(), Ok(()));
        assert!(params.pawn.is_some());
        let mut placeholders = params.placeholder_names();
        placeholders.sort();
        let mut expected = vec![
            "movement.gravity_z".to_owned(),
            "movement.braking_deceleration".to_owned(),
        ];
        expected.extend(
            [
                "max_range",
                "pull_acceleration",
                "min_rope_length",
                "rope_mode",
                "release_mode",
                "attached_max_speed",
            ]
            .map(|n| format!("grapple.{n}")),
        );
        expected.sort();
        assert_eq!(placeholders, expected);
        for e in params.provenance_report() {
            match &e.provenance {
                Provenance::ScriptDefault { class, property } => {
                    assert!(!class.is_empty() && !property.is_empty(), "{}", e.name);
                }
                Provenance::Config { file, key } => {
                    assert!(file.starts_with("ASAMU/Config/"), "{}", e.name);
                    assert!(
                        key.starts_with('[') && key.contains("] "),
                        "{}: {key}",
                        e.name
                    );
                }
                Provenance::Placeholder { note } => {
                    assert!(note.contains("placeholder"), "{}", e.name);
                }
                other => panic!("{} has unexpected provenance {other}", e.name),
            }
        }
        // 18000 rotator units = 98.876953125 degrees, exact in f32.
        assert_eq!(
            params.camera.max_pitch_degrees.value,
            rotator_units_to_degrees(18_000.0)
        );
        assert_eq!(
            f64::from(params.camera.max_pitch_degrees.value),
            18_000.0 * 360.0 / 65_536.0
        );
        assert_eq!(PlayerParams::placeholder(), PlayerParams::default());
        assert!(PlayerParams::placeholder().pawn.is_none());
    }

    #[test]
    fn pawn_params_are_validated() {
        let bad: [fn(&mut PawnParams); 6] = [
            |p| p.move_speed.value = -1.0,
            |p| p.zoom_duration.value = 0.0,
            |p| p.landed_air_control.value = 1.5,
            |p| p.zoom_fov.value = 180.0,
            |p| p.custom_time_dilation.value = 0.0,
            |p| p.bob.value = f32::NAN,
        ];
        for (i, tweak) in bad.iter().enumerate() {
            let mut p = PlayerParams::asamu_original();
            if let Some(pawn) = p.pawn.as_mut() {
                tweak(pawn);
            }
            assert!(p.validate().is_err(), "tweak {i}");
        }
    }

    #[test]
    fn defaults_are_placeholders_with_notes_except_recovered_config() {
        let params = PlayerParams::default();
        assert!(!params.all_placeholders());
        let report = params.provenance_report();
        assert_eq!(params.placeholder_names().len(), report.len() - 1);
        for e in &report {
            match &e.provenance {
                Provenance::Placeholder { note } => {
                    assert!(
                        note.contains("placeholder"),
                        "{}: note must say placeholder",
                        e.name
                    );
                }
                Provenance::Config { file, key } => {
                    assert_eq!(e.name, "movement.world_gravity_z");
                    assert_eq!(file, "ASAMU/Config/DefaultGame.ini");
                    assert_eq!(key, "[Engine.WorldInfo] DefaultGravityZ");
                    assert_eq!(e.value, "-520");
                }
                other => panic!("{} has unexpected default provenance {other}", e.name),
            }
            assert!(!e.unit.is_empty() && !e.description.is_empty() && !e.value.is_empty());
        }
        let table = params.provenance_markdown_table();
        assert_eq!(table.lines().count(), report.len() + 2);
        assert!(table.contains("`grapple.rope_mode` | inelastic |"));
        assert!(table.contains("`movement.limit_fall_accel` | true |"));
        assert!(table.contains(
            "`movement.world_gravity_z` | -520 | uu/s^2 | config | config \
             ASAMU/Config/DefaultGame.ini [Engine.WorldInfo] DefaultGravityZ |"
        ));
    }

    #[test]
    fn recovering_a_value_removes_it_from_placeholder_names() {
        let mut params = PlayerParams::default();
        let before = params.placeholder_names().len();
        params.movement.gravity_z.set(
            -1.0,
            Provenance::MeasuredTrace {
                trace_id: "example".into(),
            },
        );
        assert!(!params.all_placeholders());
        assert_eq!(params.placeholder_names().len(), before - 1);
        assert!(
            !params
                .placeholder_names()
                .contains(&"movement.gravity_z".to_owned())
        );
    }

    #[test]
    fn defaults_validate_and_bad_values_are_rejected() {
        assert_eq!(PlayerParams::default().validate(), Ok(()));
        let mut p = PlayerParams::default();
        p.movement.capsule_radius.value = f32::NAN;
        assert!(matches!(
            p.validate(),
            Err(ParamError::OutOfRange {
                name: "movement.capsule_radius",
                ..
            })
        ));
        let mut p = PlayerParams::default();
        p.camera.max_pitch_degrees.value = 180.0;
        assert!(p.validate().is_err());
        let mut p = PlayerParams::default();
        p.movement.step_height.value = 1000.0;
        assert!(p.validate().is_err());
        let bad: [fn(&mut PlayerParams); 6] = [
            |p| p.movement.world_gravity_z.value = 1.0,
            |p| p.movement.custom_gravity_scaling.value = f32::NAN,
            |p| p.movement.ground_friction.value = -0.5,
            |p| p.movement.terminal_velocity.value = 0.0,
            |p| p.movement.slope_boost_friction.value = f32::INFINITY,
            |p| p.movement.movement_speed_modifier.value = -1.0,
        ];
        for (i, tweak) in bad.iter().enumerate() {
            let mut p = PlayerParams::default();
            tweak(&mut p);
            assert!(p.validate().is_err(), "tweak {i}");
        }
    }

    #[test]
    fn serde_round_trip() {
        for p in [PlayerParams::default(), PlayerParams::asamu_original()] {
            let json = serde_json::to_string_pretty(&p).unwrap();
            let back: PlayerParams = serde_json::from_str(&json).unwrap();
            assert_eq!(back, p);
        }
    }
}
