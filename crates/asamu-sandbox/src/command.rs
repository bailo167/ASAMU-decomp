//! Every mutation a Sandbox session can perform, as data.
//!
//! Hotkeys, panel buttons and (later) a console all build a [`Command`] and
//! hand it to [`Session::execute`](crate::session::Session::execute), the
//! single mutation entry point. Commands serialise to JSON
//! (`{"cmd":"set_param","key":"...","value":0.5}`), which is how the action
//! log of a recording stores them.
//!
//! [`Command::affects_sim`] tells the commands that change the simulation
//! (parameters, the player, the world) from the ones that only change the
//! session's own state (slot selection, bookmarks, time control, recording).

use asamu_game::SetParamsError;
use serde::{Deserialize, Serialize};

use crate::keys::TuneValue;
use crate::overlay::OverlayError;
use crate::profile::Profile;
use crate::rules::Rules;
use crate::snapshot::SnapshotError;

/// One mutation of a session or of the game it runs.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum Command {
    /// Override a parameter.
    SetParam {
        /// Parameter key.
        key: String,
        /// New value.
        value: TuneValue,
    },
    /// Move a parameter by `steps` catalogue steps times `scale`.
    NudgeParam {
        /// Parameter key.
        key: String,
        /// Number of steps (negative: down).
        steps: i32,
        /// Multiplier on the step size (e.g. 10 or 0.1).
        scale: f64,
    },
    /// Back to the Classic value.
    ResetParam {
        /// Parameter key.
        key: String,
    },
    /// Every parameter back to the Classic value.
    ResetAllParams,
    /// Replace the session's overrides, rules and time scale by a profile's.
    LoadProfile {
        /// The profile (already parsed).
        profile: Box<Profile>,
    },
    /// Replace the sticky rules.
    SetRules {
        /// The new rules.
        rules: Rules,
    },
    /// Set the grapple capacity once (the level may change it again).
    SetMaxGrapples {
        /// Capacity; negative means unlimited.
        n: i32,
    },
    /// Next grapple capacity of 0, 1, 2, 3, unlimited.
    CycleGrapples,
    /// Rocket boots on or off.
    RocketBoots {
        /// On, off or toggle.
        on: Toggle,
    },
    /// Story mode on or off.
    StoryMode {
        /// On, off or toggle.
        on: Toggle,
    },
    /// Refill the grapple budget.
    RefillGrapples,
    /// Re-arm the rocket boots.
    ResetBoots,
    /// Activate every attractor pad of the level.
    ActivateAttractors,
    /// Respawn at the active checkpoint.
    Respawn,
    /// Start the death sequence (converted levels), else respawn.
    Kill,
    /// Move the player.
    Teleport {
        /// Where to.
        to: TeleportTarget,
    },
    /// Remember the player's position and view under a name.
    SetMark {
        /// Bookmark name.
        name: String,
    },
    /// Frozen-world fly placement on or off.
    Fly {
        /// On, off or toggle.
        on: Toggle,
    },
    /// Time control.
    Time {
        /// The operation.
        op: TimeOp,
    },
    /// Save-state slots.
    Slot {
        /// The operation.
        op: SlotOp,
    },
    /// Back one rewind keyframe (hand-made levels).
    Rewind,
    /// Sandbox recording on or off.
    Record {
        /// On, off or toggle.
        on: Toggle,
    },
}

/// On, off, or the opposite of the current state.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Toggle {
    /// Turn on.
    On,
    /// Turn off.
    Off,
    /// Flip.
    Toggle,
}

/// Where a teleport goes.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TeleportTarget {
    /// A position of the collision centre (UU); yaw and pitch in radians
    /// (`None`: keep the current view).
    Position {
        /// Collision-centre position, UU.
        position: [f32; 3],
        /// View yaw, radians.
        yaw: Option<f32>,
        /// View pitch, radians.
        pitch: Option<f32>,
    },
    /// Where the crosshair hits.
    AimPoint,
    /// The level's player start.
    Start,
    /// A checkpoint's spawn.
    Checkpoint {
        /// Checkpoint id.
        id: u32,
    },
    /// The next entry of the level's teleport-target list.
    NextTarget,
    /// A bookmark set with [`Command::SetMark`].
    Mark {
        /// Bookmark name.
        name: String,
    },
}

/// A time-control operation.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TimeOp {
    /// Freeze or unfreeze the simulation.
    Freeze(Toggle),
    /// Run `n` ticks while frozen.
    Step {
        /// Number of ticks.
        n: u32,
    },
    /// One speed step down.
    Slower,
    /// One speed step up.
    Faster,
    /// Set the speed factor.
    Scale {
        /// Speed factor (1 = normal).
        value: f32,
    },
    /// Unfrozen, normal speed.
    Reset,
}

/// A save-state slot operation (`slot` counts from 0).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SlotOp {
    /// Make this the selected slot.
    Select {
        /// Slot index.
        slot: usize,
    },
    /// Save the simulation into the slot.
    Save {
        /// Slot index.
        slot: usize,
    },
    /// Restore the simulation from the slot.
    Load {
        /// Slot index.
        slot: usize,
    },
    /// Empty the slot.
    Clear {
        /// Slot index.
        slot: usize,
    },
}

impl Command {
    /// `true` when the command changes the simulation (parameters, player,
    /// world), as opposed to the session's own state (selection, bookmarks,
    /// time control, recording).
    ///
    /// This is a classification for tools that read an action log. It is
    /// not what [`Session::is_pristine`](crate::session::Session::is_pristine)
    /// goes by: that is stricter, and any executed command, of either kind,
    /// ends it. Nothing in the Sandbox calls this yet.
    #[must_use]
    pub fn affects_sim(&self) -> bool {
        match self {
            Self::SetParam { .. }
            | Self::NudgeParam { .. }
            | Self::ResetParam { .. }
            | Self::ResetAllParams
            | Self::LoadProfile { .. }
            | Self::SetRules { .. }
            | Self::SetMaxGrapples { .. }
            | Self::CycleGrapples
            | Self::RocketBoots { .. }
            | Self::StoryMode { .. }
            | Self::RefillGrapples
            | Self::ResetBoots
            | Self::ActivateAttractors
            | Self::Respawn
            | Self::Kill
            | Self::Teleport { .. }
            | Self::Fly { .. }
            | Self::Rewind => true,
            Self::Slot { op } => matches!(op, SlotOp::Load { .. }),
            Self::SetMark { .. } | Self::Time { .. } | Self::Record { .. } => false,
        }
    }

    /// The command's serialized name (`set_param`, `teleport`, ...), for
    /// messages.
    #[must_use]
    pub fn name(&self) -> &'static str {
        match self {
            Self::SetParam { .. } => "set_param",
            Self::NudgeParam { .. } => "nudge_param",
            Self::ResetParam { .. } => "reset_param",
            Self::ResetAllParams => "reset_all_params",
            Self::LoadProfile { .. } => "load_profile",
            Self::SetRules { .. } => "set_rules",
            Self::SetMaxGrapples { .. } => "set_max_grapples",
            Self::CycleGrapples => "cycle_grapples",
            Self::RocketBoots { .. } => "rocket_boots",
            Self::StoryMode { .. } => "story_mode",
            Self::RefillGrapples => "refill_grapples",
            Self::ResetBoots => "reset_boots",
            Self::ActivateAttractors => "activate_attractors",
            Self::Respawn => "respawn",
            Self::Kill => "kill",
            Self::Teleport { .. } => "teleport",
            Self::SetMark { .. } => "set_mark",
            Self::Fly { .. } => "fly",
            Self::Time { .. } => "time",
            Self::Slot { .. } => "slot",
            Self::Rewind => "rewind",
            Self::Record { .. } => "record",
        }
    }
}

impl Toggle {
    /// The state this asks for, given the `current` one.
    #[must_use]
    pub fn resolve(self, current: bool) -> bool {
        match self {
            Self::On => true,
            Self::Off => false,
            Self::Toggle => !current,
        }
    }
}

/// Why a command was refused. The session and the game are unchanged then.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum CommandError {
    /// A parameter override was refused.
    #[error(transparent)]
    Overlay(#[from] OverlayError),
    /// The game refused the parameter set.
    #[error(transparent)]
    Params(#[from] SetParamsError),
    /// A save state could not be used.
    #[error(transparent)]
    Snapshot(#[from] SnapshotError),
    /// The command cannot run in the current state (e.g. a teleport while
    /// the grapple is attached); the text says why.
    #[error("{0}")]
    Refused(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One of every variant (and every operand shape).
    fn samples() -> Vec<Command> {
        let toggles = [Toggle::On, Toggle::Off, Toggle::Toggle];
        let mut out = vec![
            Command::SetParam {
                key: "movement.jump_velocity".to_owned(),
                value: TuneValue::Float(0.5),
            },
            Command::SetParam {
                key: "pawn.zoom_enabled".to_owned(),
                value: TuneValue::Bool(false),
            },
            Command::SetParam {
                key: "gun.initial_max_grapples".to_owned(),
                value: TuneValue::Int(3),
            },
            Command::SetParam {
                key: "grapple.rope_mode".to_owned(),
                value: TuneValue::Text("inelastic".to_owned()),
            },
            Command::NudgeParam {
                key: "movement.air_control".to_owned(),
                steps: -2,
                scale: 0.1,
            },
            Command::ResetParam {
                key: "movement.air_control".to_owned(),
            },
            Command::ResetAllParams,
            Command::LoadProfile {
                profile: Box::new(Profile::classic()),
            },
            Command::SetRules {
                rules: Rules {
                    grapples: crate::rules::GrappleRule::Fixed(2),
                    rocket_boots: crate::rules::Switch::On,
                    auto_refill: true,
                },
            },
            Command::SetMaxGrapples { n: -1 },
            Command::CycleGrapples,
            Command::RefillGrapples,
            Command::ResetBoots,
            Command::ActivateAttractors,
            Command::Respawn,
            Command::Kill,
            Command::SetMark {
                name: "a".to_owned(),
            },
            Command::Rewind,
        ];
        for on in toggles {
            out.extend([
                Command::RocketBoots { on },
                Command::StoryMode { on },
                Command::Fly { on },
                Command::Record { on },
                Command::Time {
                    op: TimeOp::Freeze(on),
                },
            ]);
        }
        for to in [
            TeleportTarget::Position {
                position: [1.0, -2.5, 300.0],
                yaw: Some(1.5),
                pitch: None,
            },
            TeleportTarget::AimPoint,
            TeleportTarget::Start,
            TeleportTarget::Checkpoint { id: 7 },
            TeleportTarget::NextTarget,
            TeleportTarget::Mark {
                name: "a".to_owned(),
            },
        ] {
            out.push(Command::Teleport { to });
        }
        for op in [
            TimeOp::Step { n: 3 },
            TimeOp::Slower,
            TimeOp::Faster,
            TimeOp::Scale { value: 0.25 },
            TimeOp::Reset,
        ] {
            out.push(Command::Time { op });
        }
        for op in [
            SlotOp::Select { slot: 0 },
            SlotOp::Save { slot: 1 },
            SlotOp::Load { slot: 2 },
            SlotOp::Clear { slot: 3 },
        ] {
            out.push(Command::Slot { op });
        }
        out
    }

    #[test]
    fn every_command_round_trips_through_json() {
        for cmd in samples() {
            let json = serde_json::to_string(&cmd).unwrap();
            let back: Command = serde_json::from_str(&json).unwrap();
            assert_eq!(back, cmd, "{json}");
        }
    }

    #[test]
    fn the_name_is_the_serialized_tag() {
        for cmd in samples() {
            let json = serde_json::to_value(&cmd).unwrap();
            assert_eq!(json["cmd"], cmd.name(), "{json}");
        }
    }

    #[test]
    fn session_only_commands_do_not_affect_the_simulation() {
        let session_only = |cmd: &Command| {
            matches!(
                cmd,
                Command::SetMark { .. }
                    | Command::Time { .. }
                    | Command::Record { .. }
                    | Command::Slot {
                        op: SlotOp::Select { .. } | SlotOp::Save { .. } | SlotOp::Clear { .. }
                    }
            )
        };
        for cmd in samples() {
            assert_eq!(cmd.affects_sim(), !session_only(&cmd), "{cmd:?}");
        }
    }

    #[test]
    fn toggles_resolve_against_the_current_state() {
        for current in [false, true] {
            assert!(Toggle::On.resolve(current));
            assert!(!Toggle::Off.resolve(current));
            assert_eq!(Toggle::Toggle.resolve(current), !current);
        }
    }

    #[test]
    fn commands_are_tagged_with_cmd_in_snake_case() {
        let json = serde_json::to_string(&Command::SetParam {
            key: "movement.jump_velocity".to_owned(),
            value: TuneValue::Float(0.5),
        })
        .unwrap();
        assert_eq!(
            json,
            r#"{"cmd":"set_param","key":"movement.jump_velocity","value":0.5}"#
        );
        assert_eq!(
            serde_json::to_string(&Command::ResetAllParams).unwrap(),
            r#"{"cmd":"reset_all_params"}"#
        );
        assert!(serde_json::from_str::<Command>(r#"{"cmd":"no_such_command"}"#).is_err());
    }
}
