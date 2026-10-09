//! The original rocket boots (`asamu.ASAMURocketBoots`), reimplemented from
//! the behavioural specification `docs/reverse-engineering/ABILITIES.md` §7
//! (rule ids A-RB-…) in our own words; no script text is reproduced here.
//!
//! The boots are an actor with the states `Ready` (initial), `Boosting`,
//! `Unavailable` and `UnavailableAndPlayedSound`. The jump key's press also
//! runs the boost handler, immediately in the input event ([`boost_key`],
//! A-RB-2). The `Boosting` state code ([`run_state_code`], called after the
//! power-jump actor in the frame order of GRAPPLE.md G-TM-2) is a latent
//! timeline with frame-quantised sleeps (G-TM-3) and **absolute** velocity
//! writes (A-RB-3/4):
//!
//! | Phase | Writes |
//! |---|---|
//! | charge, while `τ < boostDelay/2`, `τ` += 0.1 | `V := V0·(boostDelay − 2τ)/boostDelay` (×1.0, 0.8, 0.6, 0.4, 0.2) |
//! | gap | 0.1 s then `boostDelay/2` without writes |
//! | boost, while `τ < boostDuration`, `τ` += 0.05 | `V := â·S·(1 + s)/2 + R(â)·(s, C·s·sin θ, C·s·cos θ)`, `s = (D − τ)/D`, `θ = (τ/D)·totalSpinAngle` in radians |
//! | end | move-input lock −1, → `Unavailable` |
//!
//! `V0` is the velocity when the state code starts (the key-press frame,
//! after that frame's physics), `â` the aim sampled at the first boost
//! write (the camera's cached point of view: the view as the previous tick
//! ended, [`crate::sim::PlayerState::aim_direction`]), `R(â)` the rotation of the aim's rotator (zero roll),
//! `S`/`C` the boost/spiral strengths. While the grapple is attached and the
//! gun last measured more than `fMaxDistance` to the anchor, a boost write
//! keeps only the aim term and the boost ends (G-IX-3). Any landing re-arms
//! the boots (`Unavailable…` → `Ready`) or cancels a running boost
//! (A-RB-5/6) — see [`player_landed`].
//!
//! Deviation (TENTATIVE magnitude): the original converts the aim to an
//! integer rotator before rotating the spiral offset; we rotate with the
//! exact aim direction (angular difference ≤ 2π/65 536).

use glam::Vec3;
use serde::{Deserialize, Serialize};

use crate::events::SimEvent;
use crate::params::{PlayerParams, RocketBootsParams};
use crate::pawn::{ScriptConstant, poll_sleep};
use crate::sim::{PlayerState, StepEvents};
use asamu_core::det_math;

/// `asamu.ASAMURocketBoots`.
pub const SCRIPT_CLASS_BOOTS: &str = "asamu.ASAMURocketBoots";

/// Charge step: `τ` advances by this and the code sleeps this long between
/// charge writes (A-RB-3). ScriptCode `asamu.ASAMURocketBoots` state
/// `Boosting`. CONFIRMED (src+bc).
pub const CHARGE_STEP: f32 = 0.1;
/// Boost step: `τ` advances by this and the code sleeps this long between
/// boost writes (A-RB-4). ScriptCode `asamu.ASAMURocketBoots` state
/// `Boosting`. CONFIRMED (src+bc).
pub const BOOST_STEP: f32 = 0.05;
/// Forward (local X) component of the corkscrew offset before scaling
/// (A-RB-4: `(s, C·s·sin θ, C·s·cos θ)`). ScriptCode
/// `asamu.ASAMURocketBoots` state `Boosting`. CONFIRMED (src+bc).
pub const SPIRAL_FORWARD: f32 = 1.0;
/// Degrees → radians factor of the (misnamed) helper that turns the spin
/// angle into radians (A-RB-4); the script literal 0.01745329252 rounds to
/// this `f32`. ScriptCode `asamu.ASAMURocketBoots.ConvertRadiansToDegrees`.
/// CONFIRMED (src).
pub const DEGREES_TO_RADIANS: f32 = 0.017_453_292;

macro_rules! boots_constant {
    ($name:ident, $unit:expr, $function:expr, $meaning:expr) => {
        ScriptConstant {
            name: stringify!($name),
            value: $name as f64,
            unit: $unit,
            class: SCRIPT_CLASS_BOOTS,
            function: $function,
            meaning: $meaning,
        }
    };
}

/// Every script-code constant of this module (for `docs/PARITY.md`).
pub const BOOTS_SCRIPT_CONSTANTS: &[ScriptConstant] = &[
    boots_constant!(
        CHARGE_STEP,
        "s",
        "Boosting",
        "charge write step and latent sleep"
    ),
    boots_constant!(
        BOOST_STEP,
        "s",
        "Boosting",
        "boost write step and latent sleep"
    ),
    boots_constant!(
        SPIRAL_FORWARD,
        "uu/s",
        "Boosting",
        "local X of the corkscrew offset before scaling"
    ),
    boots_constant!(
        DEGREES_TO_RADIANS,
        "rad/deg",
        "ConvertRadiansToDegrees",
        "spin angle (degrees) to radians"
    ),
];

/// The boots' states (A-RB).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BootsStateName {
    /// `Ready` (auto state): a key press while falling starts a boost.
    #[default]
    Ready,
    /// `Boosting`: the charge/boost timeline.
    Boosting,
    /// `Unavailable`: used up until the next landing.
    Unavailable,
    /// `UnavailableAndPlayedSound`: as `Unavailable`; returns there after
    /// `boostExhaustedDelay` (sound throttling only).
    UnavailableAndPlayedSound,
}

/// Where the `Boosting` code continues after a latent sleep.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BoostLabel {
    /// The charge loop.
    #[default]
    Charge,
    /// After the gap: the boost starts.
    BoostStart,
    /// The boost loop.
    Boost,
}

/// Run-time state of the rocket boots.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct RocketBoots {
    /// The pawn has the boots (spawned at pawn start).
    pub spawned: bool,
    /// Current state.
    pub state: BootsStateName,
    /// `bEnabled` (Kismet `SeqAct_ToggleRocketBoots`, save load; A-RB-1).
    pub enabled: bool,
    /// The state code (re)starts at `Begin` at the next state-code run.
    pub begin_pending: bool,
    /// Remaining time of the latent sleep, if any.
    pub sleep: Option<f32>,
    /// Resume point of the `Boosting` code after the sleep.
    pub resume: BoostLabel,
    /// `Boosting` local `timeSinceLaunchStarted` (zeroed on entering the
    /// state, G-TM-5), s.
    pub tau: f32,
    /// `Boosting` local `oldVelocity` (velocity at the start of the charge).
    pub v0: Vec3,
    /// `Boosting` local `aimDir` (sampled at the first boost write).
    pub aim: Vec3,
}

/// Rocket-boots events of one tick.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BootsEvent {
    /// A key press started a boost (`Ready` → `Boosting`).
    Started,
    /// A key press while used up: the "exhausted" sound.
    Exhausted,
    /// The boost phase began (after the charge).
    BoostBegan,
    /// The boost ended normally (→ `Unavailable`).
    Finished,
    /// A landing cancelled the boost.
    Canceled,
}

impl RocketBoots {
    /// The boots as spawned with `params`.
    #[must_use]
    pub fn spawn(params: &RocketBootsParams) -> Self {
        Self {
            spawned: true,
            enabled: params.initial_enabled.value,
            ..Self::default()
        }
    }

    /// `GotoState(new)`: a different state zeroes the `Boosting` locals
    /// (G-TM-5); the code restarts at `Begin` at the next state-code run.
    pub fn goto(&mut self, new: BootsStateName) {
        if new != self.state {
            self.tau = 0.0;
            self.v0 = Vec3::ZERO;
            self.aim = Vec3::ZERO;
            self.resume = BoostLabel::default();
        }
        self.state = new;
        self.sleep = None;
        self.begin_pending = true;
    }

    /// `true` if every float is finite.
    #[must_use]
    pub fn is_finite(&self) -> bool {
        self.tau.is_finite()
            && self.v0.is_finite()
            && self.aim.is_finite()
            && self.sleep.is_none_or(f32::is_finite)
    }
}

/// Spawns the boots for a started pawn.
pub(crate) fn spawn_for(boots: &mut RocketBoots, params: &PlayerParams) {
    *boots = params
        .boots
        .as_ref()
        .map_or_else(RocketBoots::default, RocketBoots::spawn);
}

/// `EnableRocketBoots(enable)` (Kismet `SeqAct_ToggleRocketBoots`, save load;
/// A-RB-1): only the flag; a running boost continues.
pub fn enable_rocket_boots(state: &mut PlayerState, enable: bool) {
    state.script.boots.enabled = enable;
}

/// `RocketBoostKeyDown` → `OnRocketBoostInput` (A-RB-2, A-RB-5), run in the
/// jump key's input event. `Ready`: starts a boost only while physics is
/// `Falling`, the boots are enabled and the pawn is not in story mode (incl.
/// zoom). `Unavailable`: the exhausted sound and `UnavailableAndPlayedSound`.
pub fn boost_key(state: &mut PlayerState) -> Option<BootsEvent> {
    let falling = !state.grounded && !state.pawn.flying;
    let story = state.script.is_story();
    let b = &mut state.script.boots;
    if !b.spawned {
        return None;
    }
    match b.state {
        BootsStateName::Ready => {
            if falling && b.enabled && !story {
                b.goto(BootsStateName::Boosting);
                Some(BootsEvent::Started)
            } else {
                None
            }
        }
        BootsStateName::Unavailable => {
            b.goto(BootsStateName::UnavailableAndPlayedSound);
            Some(BootsEvent::Exhausted)
        }
        BootsStateName::Boosting | BootsStateName::UnavailableAndPlayedSound => None,
    }
}

/// `PlayerLanded` of the boots (called by both landing handlers, not on
/// `NotLandable` floors): re-arm after a boost (A-RB-5) or cancel a running
/// boost (`ResetBoots`, A-RB-6; the landing handler's own lock decrement
/// releases the boost's move-input lock).
pub fn player_landed(boots: &mut RocketBoots) -> Option<BootsEvent> {
    match boots.state {
        BootsStateName::Unavailable | BootsStateName::UnavailableAndPlayedSound => {
            boots.goto(BootsStateName::Ready);
            None
        }
        BootsStateName::Boosting => {
            boots.goto(BootsStateName::Ready);
            Some(BootsEvent::Canceled)
        }
        BootsStateName::Ready => None,
    }
}

/// `ResetBoots` (the player reset after a death when the boots are enabled,
/// A-DT-2): back to `Ready`, cancelling a boost.
pub fn reset_boots(boots: &mut RocketBoots) {
    boots.goto(BootsStateName::Ready);
}

/// `R(â)·local`: rotates a local vector by the zero-roll rotation whose X
/// axis is the unit vector `aim` (UE3 `local >> Rotator(aim)`): X = aim,
/// Y = horizontal right, Z = up of the aim frame. Straight up/down aims use
/// yaw 0.
#[must_use]
pub fn rotate_by_aim(local: Vec3, aim: Vec3) -> Vec3 {
    let h = (aim.x * aim.x + aim.y * aim.y).sqrt();
    let (cos_yaw, sin_yaw) = if h > 0.0 {
        (aim.x / h, aim.y / h)
    } else {
        (1.0, 0.0)
    };
    let (cos_pitch, sin_pitch) = (h, aim.z);
    let x_axis = Vec3::new(cos_pitch * cos_yaw, cos_pitch * sin_yaw, sin_pitch);
    let y_axis = Vec3::new(-sin_yaw, cos_yaw, 0.0);
    let z_axis = Vec3::new(-sin_pitch * cos_yaw, -sin_pitch * sin_yaw, cos_pitch);
    x_axis * local.x + y_axis * local.y + z_axis * local.z
}

/// The boots' latent state code for one tick (after the power-jump actor,
/// before the gun; G-TM-2). Velocity writes are integrated by the next
/// tick's physics.
pub fn run_state_code(
    state: &mut PlayerState,
    params: &PlayerParams,
    dt: f32,
    events: &mut StepEvents,
) {
    let Some(bp) = params.boots.as_ref() else {
        return;
    };
    let b = &mut state.script.boots;
    if !b.spawned {
        return;
    }
    let woke = poll_sleep(&mut b.sleep, dt);
    if !woke && (b.sleep.is_some() || !b.begin_pending) {
        return;
    }
    let begin = !woke;
    b.begin_pending = false;
    match b.state {
        BootsStateName::Boosting => run_boosting(state, params, bp, begin, events),
        BootsStateName::UnavailableAndPlayedSound => {
            if begin {
                b.sleep = Some(bp.boost_exhausted_delay.value);
            } else {
                b.goto(BootsStateName::Unavailable);
                // `Unavailable` has no state code.
                b.begin_pending = false;
            }
        }
        BootsStateName::Ready | BootsStateName::Unavailable => {}
    }
}

/// The `Boosting` timeline (module docs), from `Begin` or the resume label.
fn run_boosting(
    state: &mut PlayerState,
    params: &PlayerParams,
    bp: &RocketBootsParams,
    begin: bool,
    events: &mut StepEvents,
) {
    #[derive(Clone, Copy)]
    enum Pc {
        Begin,
        Charge,
        BoostStart,
        Boost,
        End,
    }
    let delay = bp.boost_delay.value;
    let duration = bp.boost_duration.value;
    let strength = bp.boost_strength.value;
    let spiral_strength = bp.boost_spiral_strength.value;
    let spin = bp.total_spin_angle.value;
    let max_distance = params.gun.as_ref().map(|g| g.max_distance.value);
    let mut pc = if begin {
        Pc::Begin
    } else {
        match state.script.boots.resume {
            BoostLabel::Charge => Pc::Charge,
            BoostLabel::BoostStart => Pc::BoostStart,
            BoostLabel::Boost => Pc::Boost,
        }
    };
    // Every path ends in a sleep or a state change; the bound only guards
    // against pathological parameters.
    for _ in 0..8 {
        match pc {
            Pc::Begin => {
                // Charge start: remember V0, Kismet output 0, take the
                // move-input lock only if nothing holds it (A-IL-1).
                state.script.boots.v0 = state.velocity;
                events
                    .kismet
                    .push(SimEvent::PlayerRocketBoosted { boosting: false });
                if state.script.move_input_lock == 0 {
                    state.script.move_input_lock = 1;
                }
                pc = Pc::Charge;
            }
            Pc::Charge => {
                let b = &mut state.script.boots;
                if b.tau < delay / 2.0 {
                    state.velocity = b.v0 * ((delay - b.tau * 2.0) / delay);
                    b.tau += CHARGE_STEP;
                    b.sleep = Some(CHARGE_STEP);
                    b.resume = BoostLabel::Charge;
                } else {
                    b.sleep = Some(delay / 2.0);
                    b.resume = BoostLabel::BoostStart;
                }
                return;
            }
            Pc::BoostStart => {
                events
                    .kismet
                    .push(SimEvent::PlayerRocketBoosted { boosting: true });
                events.boots = Some(BootsEvent::BoostBegan);
                // `GetAdjustedAim`: the camera's cached point of view (the
                // view as the previous tick ended, G-TG-1).
                let aim = state.aim_direction();
                let b = &mut state.script.boots;
                b.tau = 0.0;
                b.aim = aim;
                pc = Pc::Boost;
            }
            Pc::Boost => {
                let tau = state.script.boots.tau;
                if tau >= duration {
                    pc = Pc::End;
                    continue;
                }
                let aim = state.script.boots.aim;
                let angle = ((tau / duration) * spin) * DEGREES_TO_RADIANS;
                let remaining = (duration - tau) / duration;
                let (sin, cos) = det_math::sin_cos(angle);
                let spiral =
                    Vec3::new(SPIRAL_FORWARD, sin * spiral_strength, cos * spiral_strength)
                        * remaining;
                let push = aim * strength;
                state.velocity = (push * remaining) * 0.5 + push * 0.5;
                // G-IX-3: too far from the anchor → keep only the aim term and
                // end the boost.
                let too_far = state.script.gun.is_attached()
                    && max_distance.is_some_and(|m| state.script.gun.distance > m);
                if too_far {
                    pc = Pc::End;
                    continue;
                }
                state.velocity += rotate_by_aim(spiral, aim);
                let b = &mut state.script.boots;
                b.tau += BOOST_STEP;
                b.sleep = Some(BOOST_STEP);
                b.resume = BoostLabel::Boost;
                return;
            }
            Pc::End => {
                // `EnableInput(true)`: one move-input lock level released.
                state.script.move_input_lock = state.script.move_input_lock.saturating_sub(1);
                let b = &mut state.script.boots;
                b.goto(BootsStateName::Unavailable);
                b.begin_pending = false;
                events.boots = Some(BootsEvent::Finished);
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rotation_maps_x_to_the_aim_and_keeps_zero_roll() {
        for aim in [
            Vec3::X,
            Vec3::new(0.6, 0.8, 0.0),
            Vec3::new(0.0, 0.6, 0.8),
            Vec3::new(-0.36, 0.48, -0.8),
        ] {
            let x = rotate_by_aim(Vec3::X, aim);
            assert!((x - aim).length() < 1e-6, "{aim}: {x}");
            let y = rotate_by_aim(Vec3::Y, aim);
            assert!(y.z.abs() < 1e-7, "right axis stays horizontal (zero roll)");
            let z = rotate_by_aim(Vec3::Z, aim);
            assert!(z.z >= 0.0, "up axis points up");
            assert!(x.dot(y).abs() < 1e-6 && x.dot(z).abs() < 1e-6 && y.dot(z).abs() < 1e-6);
        }
        // Straight up: yaw 0, so local up maps to -X.
        let z = rotate_by_aim(Vec3::Z, Vec3::Z);
        assert!((z - Vec3::NEG_X).length() < 1e-6);
    }

    #[test]
    fn goto_zeroes_the_boosting_locals_only_on_a_state_change() {
        let mut b = RocketBoots {
            state: BootsStateName::Boosting,
            tau: 0.3,
            v0: Vec3::ONE,
            ..RocketBoots::default()
        };
        b.goto(BootsStateName::Boosting);
        assert_eq!(b.tau, 0.3);
        b.goto(BootsStateName::Ready);
        assert_eq!((b.tau, b.v0), (0.0, Vec3::ZERO));
    }
}
