//! Movement, grapple and camera parameters.
//!
//! # Almost every default here is a PLACEHOLDER
//!
//! Only one gameplay value recovered from *A Story About My Uncle* is wired
//! into this crate: [`MovementParams::world_gravity_z`] (`DefaultGravityZ`
//! from the original's `ASAMU/Config/DefaultGame.ini`, provenance
//! [`Provenance::Config`]). Everything else is still a placeholder (see
//! `docs/reverse-engineering/GAMEPLAY_LEADS.md` and
//! `docs/reverse-engineering/NATIVE_PHYSICS.md` section 9.1).
//! The placeholder defaults below were chosen by us so the graybox prototype is playable;
//! they are **not** values from the original game and were not deliberately
//! taken from stock UE3/UDK defaults either. If one happens to coincide with
//! an engine default, that coincidence is **not** evidence about ASAMU. Each
//! one is a [`Param`] carrying [`Provenance::Placeholder`] with a note saying
//! what should replace it. Engine property names mentioned in the notes (e.g.
//! `GroundSpeed`) are *leads* for where to look in the original packages, all
//! **TENTATIVE** until confirmed. Replacing a value means calling
//! [`Param::set`] with the real [`Provenance`] (script default, native code,
//! config or measured trace) — never editing the number alone.
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

/// Original config file holding `[Engine.WorldInfo] DefaultGravityZ`
/// (relative to the game's `Contents/Resources` directory; no user paths).
pub const DEFAULT_GRAVITY_CONFIG_FILE: &str = "ASAMU/Config/DefaultGame.ini";

/// Config key of the original's default world gravity.
pub const DEFAULT_GRAVITY_CONFIG_KEY: &str = "Engine.WorldInfo.DefaultGravityZ";

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
                "graybox placeholder; replace with value recovered from ASAMU pawn defaults \
                 (lead: GroundSpeed, TENTATIVE)",
            ),
            ground_acceleration: ph(
                3000.0,
                "graybox placeholder; replace with value recovered from ASAMU pawn defaults \
                 (lead: AccelRate, TENTATIVE)",
            ),
            braking_deceleration: ph(
                3000.0,
                "graybox placeholder used only by PlaceholderMovement (the original brakes with \
                 2 x ground friction, see movement.ground_friction)",
            ),
            air_control: ph(
                0.25,
                "graybox placeholder; replace with value recovered from ASAMU pawn defaults \
                 (lead: AirControl, TENTATIVE)",
            ),
            jump_velocity: ph(
                450.0,
                "graybox placeholder; replace with value recovered from ASAMU pawn defaults \
                 (lead: JumpZ, TENTATIVE)",
            ),
            capsule_radius: ph(
                20.0,
                "graybox placeholder (human-sized at the presentation scale); replace with the \
                 ASAMU pawn collision component radius (lead: CollisionRadius, TENTATIVE)",
            ),
            capsule_half_height: ph(
                45.0,
                "graybox placeholder (human-sized at the presentation scale); replace with the \
                 ASAMU pawn collision component height (lead: CollisionHeight, TENTATIVE)",
            ),
            step_height: ph(
                18.0,
                "graybox placeholder; replace with value recovered from ASAMU pawn defaults \
                 (lead: MaxStepHeight, TENTATIVE)",
            ),
            max_fall_speed: ph(
                4000.0,
                "graybox placeholder used only by PlaceholderMovement (the original clamps the \
                 3-D falling speed to movement.terminal_velocity)",
            ),
            walkable_floor_z: ph(
                0.7,
                "graybox placeholder; replace with the original's walkable-slope threshold \
                 (lead: WalkableFloorZ, TENTATIVE)",
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
                "graybox placeholder (neutral factor); replace with the ASAMU pawn default \
                 (lead: UDKPawn CustomGravityScaling, TENTATIVE)",
            ),
            ground_friction: ph(
                6.0,
                "graybox placeholder; replace with the PhysicsVolume ground friction of the \
                 original maps (lead: PhysicsVolume GroundFriction, TENTATIVE)",
            ),
            terminal_velocity: ph(
                4000.0,
                "graybox placeholder (same number as movement.max_fall_speed); replace with the \
                 original's PhysicsVolume terminal velocity (lead: TerminalVelocity, TENTATIVE)",
            ),
            limit_fall_accel: ph(
                true,
                "graybox placeholder; the default of the air-acceleration limit flag (Pawn+0x298 \
                 bit 51) is UNKNOWN (lead: bLimitFallAccel in pawn defaults, TENTATIVE)",
            ),
            slope_boost_friction: ph(
                0.5,
                "graybox placeholder (only zero vs non-zero matters without physical materials); \
                 replace with the ASAMU pawn default (lead: UDKPawn SlopeBoostFriction, TENTATIVE)",
            ),
            movement_speed_modifier: ph(
                1.0,
                "graybox placeholder (neutral factor); replace with the ASAMU pawn default \
                 (lead: Pawn MovementSpeedModifier, TENTATIVE)",
            ),
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

/// Grapple parameters.
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
                "graybox placeholder; replace with the grapple range from ASAMU script defaults \
                 (class not yet identified)",
            ),
            pull_acceleration: ph(
                900.0,
                "graybox placeholder; replace with the grapple pull behaviour recovered from \
                 ASAMU script (UNKNOWN whether it is an acceleration at all)",
            ),
            min_rope_length: ph(
                60.0,
                "graybox placeholder; numerical guard so pull never reaches the anchor; \
                 replace once the original's behaviour near the anchor is known",
            ),
            rope_mode: ph(
                RopeMode::Inelastic,
                "graybox placeholder; the original's rope model (fixed length vs reel-in vs \
                 spring) is UNKNOWN",
            ),
            release_mode: ph(
                ReleaseMode::PreserveVelocity,
                "graybox placeholder; whether the original modifies velocity on release is \
                 UNKNOWN (verify with traces)",
            ),
            attached_max_speed: ph(
                2500.0,
                "graybox placeholder; it is UNKNOWN whether the original caps swing speed",
            ),
        }
    }
}

/// First-person camera parameters.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CameraParams {
    /// Horizontal field of view in degrees (UE3 FOV is horizontal; TENTATIVE
    /// for ASAMU).
    pub fov_degrees: Param<f32>,
    /// Eye height above the collision-shape centre, UU.
    pub eye_height: Param<f32>,
    /// Pitch limit (symmetric) in degrees.
    pub max_pitch_degrees: Param<f32>,
}

impl Default for CameraParams {
    fn default() -> Self {
        Self {
            fov_degrees: ph(
                90.0,
                "graybox placeholder; replace with the FOV from ASAMU camera/viewport defaults \
                 or config (lead: ASAMU.ASAMUViewportClient, TENTATIVE)",
            ),
            eye_height: ph(
                38.0,
                "graybox placeholder; replace with value recovered from ASAMU pawn defaults \
                 (lead: BaseEyeHeight, TENTATIVE)",
            ),
            max_pitch_degrees: ph(
                89.0,
                "graybox placeholder; replace with the original's view pitch limits \
                 (lead: camera/controller pitch clamp, TENTATIVE)",
            ),
        }
    }
}

/// All player simulation parameters.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlayerParams {
    /// Locomotion.
    pub movement: MovementParams,
    /// Grapple.
    pub grapple: GrappleParams,
    /// Camera.
    pub camera: CameraParams,
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

impl PlayerParams {
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
            "Downward speed cap",
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
            v > 0.0 && v < 90.0,
            "in (0, 90)",
        )?;
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
        let params = PlayerParams::default();
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
                    assert_eq!(key, "Engine.WorldInfo.DefaultGravityZ");
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
             ASAMU/Config/DefaultGame.ini Engine.WorldInfo.DefaultGravityZ |"
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
        p.camera.max_pitch_degrees.value = 95.0;
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
        let p = PlayerParams::default();
        let json = serde_json::to_string_pretty(&p).unwrap();
        let back: PlayerParams = serde_json::from_str(&json).unwrap();
        assert_eq!(back, p);
    }
}
