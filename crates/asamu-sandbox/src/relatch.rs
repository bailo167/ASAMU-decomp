//! Bringing a running pawn up to a new parameter set.
//!
//! Most parameters are read from the set every tick. A few are copied into
//! run-time values by the script layer when the pawn starts or changes mode
//! (ground speed, air control, jump velocity, FOV, the grapple's air speed).
//! After `Game::set_params` swapped the set, [`relatch`] rewrites exactly
//! those copies, and only where the run-time value still equals what the
//! *old* set would have latched, so anything the level or the player changed
//! since is left alone. The pawn's start routine is never re-run: it would
//! reset abilities, the grapple budget and story state.
//!
//! [`retune`] is the one place the Sandbox calls `Game::set_params`.
//!
//! The rules below are **ours** (Sandbox tooling). They are read off the
//! places `asamu-player` copies each value (`pawn::start`, the sprint and
//! story-mode speed changes, the landing handler, the gun's spawn and
//! release) and `tests/relatch.rs` compares each against a pawn that was
//! started with the new set outright.

use asamu_game::{Game, SetParamsError};
use asamu_player::{PawnParams, PawnStateName, PlayerParams, PlayerState};

/// What [`relatch`] did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RelatchReport {
    /// Run-time values rewritten from the new set (parameter keys).
    pub relatched: Vec<&'static str>,
    /// Changed keys that only take effect at the next spawn.
    pub deferred: Vec<&'static str>,
}

/// Bit-for-bit equality: "still the value the old set latched".
fn same(a: f32, b: f32) -> bool {
    a.to_bits() == b.to_bits()
}

/// A ground speed the script layer writes, as it computes it.
#[derive(Clone, Copy)]
enum SpeedMode {
    /// `move_speed` (pawn start, sprint removed, story mode left).
    Walk,
    /// `move_speed * sprint_speed_multiplier`.
    Sprint,
    /// `move_speed * story_speed_multiplier`.
    Story,
}

impl SpeedMode {
    /// The ground speed this mode latches from `pawn`, with the script
    /// layer's own expression (the result must match bit for bit).
    fn speed(self, pawn: &PawnParams) -> f32 {
        match self {
            Self::Walk => pawn.move_speed.value,
            Self::Sprint => pawn.move_speed.value * pawn.sprint_speed_multiplier.value,
            Self::Story => pawn.move_speed.value * pawn.story_speed_multiplier.value,
        }
    }

    /// The keys among this mode's factors whose value differs between
    /// `old` and `new`.
    fn changed_keys(self, old: &PawnParams, new: &PawnParams) -> Vec<&'static str> {
        let mut keys = Vec::new();
        if !same(old.move_speed.value, new.move_speed.value) {
            keys.push("pawn.move_speed");
        }
        let multiplier = match self {
            Self::Walk => None,
            Self::Sprint => Some((
                "pawn.sprint_speed_multiplier",
                old.sprint_speed_multiplier.value,
                new.sprint_speed_multiplier.value,
            )),
            Self::Story => Some((
                "pawn.story_speed_multiplier",
                old.story_speed_multiplier.value,
                new.story_speed_multiplier.value,
            )),
        };
        if let Some((key, before, after)) = multiplier
            && !same(before, after)
        {
            keys.push(key);
        }
        keys
    }
}

/// Rewrites the pawn's latched run-time values after the parameter set
/// changed from `old` to `new`. Does nothing when the sets are equal, and
/// never touches a value that no longer equals what `old` would have
/// latched.
///
/// - **Ground speed** follows `pawn.move_speed` times the story multiplier
///   in story mode, else the sprint multiplier while sprinting, else 1.
///   (Story mode can also hold the sprint speed after a landing applied an
///   armed sprint, and a pawn can keep its sprint flag across story mode at
///   walking speed; whichever of the state's possible speeds the pawn
///   actually holds is the one that follows.)
/// - **Air control** follows `movement.air_control` while it still holds
///   that value, else `pawn.landed_air_control` (after the first landing).
/// - **Jump velocity**, **FOV** (not while zooming) and **zoom
///   availability** follow their parameters directly.
/// - **Air speed** follows `gun.grapple_accel` when the pawn has the gun.
/// - A changed **collision cylinder** forces a floor check and moves a
///   standing pawn's centre by the half-height difference, keeping its feet
///   where they were.
/// - Spawn-only keys are reported in [`RelatchReport::deferred`].
pub fn relatch(player: &mut PlayerState, old: &PlayerParams, new: &PlayerParams) -> RelatchReport {
    let mut report = RelatchReport::default();
    if old == new {
        return report;
    }

    // The collision cylinder (read from the set every tick; only the pawn's
    // footing needs help).
    let (old_m, new_m) = (&old.movement, &new.movement);
    let radius_changed = !same(old_m.capsule_radius.value, new_m.capsule_radius.value);
    let height_changed = !same(
        old_m.capsule_half_height.value,
        new_m.capsule_half_height.value,
    );
    if radius_changed || height_changed {
        player.pawn.force_floor_check = true;
        if radius_changed {
            report.relatched.push("movement.capsule_radius");
        }
        if height_changed {
            if player.grounded {
                let lift = new_m.capsule_half_height.value - old_m.capsule_half_height.value;
                if lift.is_finite() {
                    player.position.z += lift;
                }
            }
            report.relatched.push("movement.capsule_half_height");
        }
    }

    // Everything below is the script layer's run-time state.
    let (Some(old_pawn), Some(new_pawn)) = (&old.pawn, &new.pawn) else {
        return report;
    };
    if !player.script.started {
        return report;
    }
    let script = &mut player.script;

    // Ground speed: the speeds this state can hold, most specific first.
    let modes: &[SpeedMode] = if script.is_story() {
        if script.sprint.active {
            &[SpeedMode::Story, SpeedMode::Sprint]
        } else {
            &[SpeedMode::Story]
        }
    } else if script.sprint.active {
        &[SpeedMode::Sprint, SpeedMode::Walk]
    } else {
        &[SpeedMode::Walk]
    };
    if let Some(mode) = modes
        .iter()
        .copied()
        .find(|mode| same(script.ground_speed, mode.speed(old_pawn)))
    {
        let next = mode.speed(new_pawn);
        if !same(script.ground_speed, next) {
            script.ground_speed = next;
            report
                .relatched
                .extend(mode.changed_keys(old_pawn, new_pawn));
        }
    }

    // Air control: the start value until the first normal landing, the
    // landed value afterwards.
    if same(script.air_control, old_m.air_control.value) {
        if !same(script.air_control, new_m.air_control.value) {
            script.air_control = new_m.air_control.value;
            report.relatched.push("movement.air_control");
        }
    } else if same(script.air_control, old_pawn.landed_air_control.value)
        && !same(script.air_control, new_pawn.landed_air_control.value)
    {
        script.air_control = new_pawn.landed_air_control.value;
        report.relatched.push("pawn.landed_air_control");
    }

    // Jump velocity.
    if same(script.jump_z, old_m.jump_velocity.value)
        && !same(script.jump_z, new_m.jump_velocity.value)
    {
        script.jump_z = new_m.jump_velocity.value;
        report.relatched.push("movement.jump_velocity");
    }

    // FOV: the zoom owns it while it runs (and reads the new base itself).
    if script.code.state != PawnStateName::Zooming
        && same(script.fov, old.camera.fov_degrees.value)
        && !same(script.fov, new.camera.fov_degrees.value)
    {
        script.fov = new.camera.fov_degrees.value;
        report.relatched.push("camera.fov_degrees");
    }

    // Zoom availability, unless the level changed it since the start.
    if old_pawn.zoom_enabled.value != new_pawn.zoom_enabled.value
        && script.zoom_enabled == old_pawn.zoom_enabled.value
    {
        script.zoom_enabled = new_pawn.zoom_enabled.value;
        report.relatched.push("pawn.zoom_enabled");
    }

    // Air speed: the gun writes its own value at spawn and after every
    // release; without the gun the class default stays.
    match (&old.gun, &new.gun) {
        (Some(old_gun), Some(new_gun)) if script.gun.spawned => {
            if same(script.air_speed, old_gun.grapple_accel.value)
                && !same(script.air_speed, new_gun.grapple_accel.value)
            {
                script.air_speed = new_gun.grapple_accel.value;
                report.relatched.push("gun.grapple_accel");
            }
        }
        _ => {
            if same(script.air_speed, old_m.air_speed.value)
                && !same(script.air_speed, new_m.air_speed.value)
            {
                script.air_speed = new_m.air_speed.value;
                report.relatched.push("movement.air_speed");
            }
        }
    }

    // Read at spawn only: reported, not applied.
    if let (Some(old_gun), Some(new_gun)) = (&old.gun, &new.gun) {
        if old_gun.initial_max_grapples.value != new_gun.initial_max_grapples.value {
            report.deferred.push("gun.initial_max_grapples");
        }
        if old_gun.initial_can_grapple.value != new_gun.initial_can_grapple.value {
            report.deferred.push("gun.initial_can_grapple");
        }
    }
    if let (Some(old_boots), Some(new_boots)) = (&old.boots, &new.boots)
        && old_boots.initial_enabled.value != new_boots.initial_enabled.value
    {
        report.deferred.push("boots.initial_enabled");
    }
    report
}

/// Makes `game` run `next`: `Game::set_params`, then [`relatch`]. Writes
/// nothing at all when `next` already is the game's set.
///
/// # Errors
/// `next` is invalid or would add or remove a script-layer group; the game
/// is unchanged then.
pub fn retune(game: &mut Game, next: PlayerParams) -> Result<RelatchReport, SetParamsError> {
    if next == *game.params() {
        return Ok(RelatchReport::default());
    }
    let old = game.params().clone();
    game.set_params(next)?;
    let new = game.params().clone();
    Ok(relatch(game.player_mut(), &old, &new))
}
