//! What a checkpoint save keeps of a level script, and how a load puts it
//! back (KISMET_RUNTIME.md §12, SAVE.md §4.3).
//!
//! The original's world snapshot (`SaveGameState`, written at every new
//! latest checkpoint) holds, of the level's Kismet (CONFIRMED (src), read
//! locally and restated):
//!
//! - every **event** of every root sequence: the rest of its re-trigger
//!   delay (`max(ReTriggerDelay − (now − ActivationTime), 0)`), its trigger
//!   count and `bEnabled`;
//! - every **variable** of the classes bool, float, int, object, string and
//!   vector: its value (an object as its path);
//! - every **Matinee** (`SeqAct_Interp`): position, playing, paused.
//!
//! Nothing else: no active op, no delayed activation, no latent action in
//! mid-wait (a `SeqAct_Delay` counting down at the save is gone after a
//! load), no output impulse, no narrator queue, and nothing an action did to
//! an actor (hidden, destroyed, toggled, collision) apart from where a
//! Matinee's position puts its actors.
//!
//! The load runs when the player's pawn spawns, that is after the game
//! sequence's begin-play (which already activated the level-loaded events
//! with a linked first output) and before the match start's second
//! notification (KISMET_RUNTIME.md §3). Per saved object that is still found
//! by its path:
//!
//! - an event gets `ActivationTime = now + saved rest`, the saved `bEnabled`
//!   and the saved trigger count. (The activation check compares the time
//!   since `ActivationTime` with the whole delay, so an event saved in
//!   mid-delay waits its delay *plus* the saved rest after a load. Kept.)
//! - a variable gets the saved value; an object that is not found becomes
//!   null;
//! - a Matinee that was not playing is moved to its saved position as a
//!   jump; one that was playing is moved there without a jump and receives
//!   a forced Play, which the next update runs. Then `bPaused` is set.
//!   `SetPosition` on an action that is not active builds its group
//!   instances, updates every track to the position, moves the bound actors
//!   in the interpolating physics at once, and tears the instances down
//!   again (CONFIRMED (native): `USeqAct_Interp::SetPosition @ 0x1008A8170`),
//!   which also gives the view and the fade back, so a director track
//!   leaves nothing on screen, and stops the sounds its sound tracks had
//!   just started ([`Runtime::drop_director_outputs`]). The event track's
//!   keys between the two positions fire as in any update that is not a
//!   jump: their output impulses stay on the action and are delivered when
//!   it is next processed, for a Matinee that was playing in the first
//!   update after the load (KISMET_RUNTIME.md §12, "What a continued game
//!   does not have").
//!
//! After that the pawn is put at the latest checkpoint and every
//! `SavedGameStateLoaded` event is told the checkpoint index
//! ([`Runtime::saved_game_state_loaded`]). The shipped levels guard their
//! openings with exactly this: the event leads to a comparison of a bool
//! variable that the opening's first action sets, so a load that restored
//! the variable skips the opening (AG-ParadiseCave, AG-BeautifulCity three
//! times, AG-Darkcave twice, AG-StarHaven).
//!
//! Float text: the original writes floats with four decimals (SAVE.md §3.5,
//! CONFIRMED (native)), so a restored position or float variable is the
//! saved one rounded to 0.0001. [`Runtime::capture_state`] rounds the same
//! way.
//!
//! Not saved by ours (differences from the list above, none of which a
//! shipped level's script can tell): vector variables (the runtime has no
//! such class; no shipped level sequence uses one), the stored value of
//! random variables and of player variables (both are computed on every
//! read).

use std::collections::BTreeMap;

use crate::graph::NodeKind;
use crate::host::{Host, NullHost, Output};
use crate::ops::OpClass;
use crate::runtime::{Latent, Runtime};
use crate::value::KValue;

/// The saved state of one event.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EventState {
    /// Rest of the re-trigger delay at the save, seconds (never negative).
    pub retrigger_left: f32,
    /// `TriggerCount`.
    pub trigger_count: i32,
    /// `bEnabled`.
    pub enabled: bool,
}

/// The saved value of one variable.
#[derive(Debug, Clone, PartialEq)]
pub enum VarState {
    /// `SeqVar_Bool`.
    Bool(bool),
    /// `SeqVar_Int`.
    Int(i32),
    /// `SeqVar_Float` (four decimals).
    Float(f32),
    /// `SeqVar_Object`: the object's path (`None`: null).
    Object(Option<String>),
    /// `SeqVar_String`.
    Str(String),
}

/// The saved state of one Matinee.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MatineeState {
    /// `Position`, seconds (four decimals).
    pub position: f32,
    /// `bIsPlaying`.
    pub playing: bool,
    /// `bPaused`.
    pub paused: bool,
}

/// What a checkpoint save keeps of a level script, keyed by object path.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ScriptState {
    /// Events.
    pub events: BTreeMap<String, EventState>,
    /// Variables.
    pub variables: BTreeMap<String, VarState>,
    /// Matinees.
    pub matinees: BTreeMap<String, MatineeState>,
}

impl ScriptState {
    /// True when nothing was saved.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.events.is_empty() && self.variables.is_empty() && self.matinees.is_empty()
    }
}

/// A float as the original's save text holds it: four decimals (`%.4f`),
/// read back. A value that is not finite is saved as 0.
#[must_use]
pub fn save_float(x: f32) -> f32 {
    if !x.is_finite() {
        return 0.0;
    }
    format!("{x:.4}").parse().unwrap_or(0.0)
}

impl Runtime {
    /// The level script's state as a checkpoint save keeps it (module docs):
    /// the events, variables and Matinees of every attached level.
    #[must_use]
    pub fn capture_state(&self) -> ScriptState {
        let mut state = ScriptState::default();
        for node in &self.graph.nodes {
            if !self
                .level_attached
                .get(node.level)
                .copied()
                .unwrap_or(false)
            {
                continue;
            }
            let Some(op) = self.ops.get(node.id) else {
                continue;
            };
            match node.kind {
                NodeKind::Event => {
                    let delay = node.event.as_ref().map_or(0.0, |e| e.retrigger_delay);
                    let since = (self.time - op.activation_time) as f32;
                    state.events.insert(
                        node.path.clone(),
                        EventState {
                            retrigger_left: save_float((delay - since).max(0.0)),
                            trigger_count: op.trigger_count,
                            enabled: op.enabled,
                        },
                    );
                }
                NodeKind::Variable => {
                    let value = self.vars.get(node.id);
                    let saved = match (node.class, value) {
                        (OpClass::VarBool, Some(v)) => Some(VarState::Bool(v.as_bool())),
                        (OpClass::VarInt, Some(v)) => Some(VarState::Int(v.as_int())),
                        (OpClass::VarFloat, Some(v)) => {
                            Some(VarState::Float(save_float(v.as_float())))
                        }
                        (OpClass::VarObject, Some(v)) => {
                            Some(VarState::Object(v.as_obj().map(str::to_owned)))
                        }
                        (OpClass::VarString, Some(v)) => {
                            Some(VarState::Str(v.as_str().unwrap_or("").to_owned()))
                        }
                        _ => None,
                    };
                    if let Some(saved) = saved {
                        state.variables.insert(node.path.clone(), saved);
                    }
                }
                _ if node.class == OpClass::Interp => {
                    let saved = match &op.latent {
                        Latent::Interp(run) => MatineeState {
                            position: save_float(run.playback.position),
                            playing: run.playback.playing,
                            paused: run.playback.paused,
                        },
                        _ => MatineeState {
                            position: 0.0,
                            playing: false,
                            paused: false,
                        },
                    };
                    state.matinees.insert(node.path.clone(), saved);
                }
                _ => {}
            }
        }
        state
    }

    /// The saved script state the level start's load restores (the game
    /// info's load at the pawn's spawn; [`Runtime::set_start_save_index`]
    /// gives the checkpoint index its "save loaded" events receive). `None`
    /// (the default) restores nothing: a level entered without a save of
    /// its own. Has no effect once the level has started.
    pub fn set_start_state(&mut self, state: Option<ScriptState>) {
        self.start_state = state;
    }

    /// The state [`Runtime::set_start_state`] set and the level start has
    /// not taken yet.
    #[must_use]
    pub fn start_state(&self) -> Option<&ScriptState> {
        self.start_state.as_ref()
    }

    /// Puts a saved state back, as the original's load does (module docs).
    /// Entries whose object is not in an attached level are skipped, as the
    /// original skips a path it does not find; entries of a kind the object
    /// is not are skipped too.
    pub fn restore_state(&mut self, state: &ScriptState, host: &mut dyn Host) {
        let now = self.time;
        let attached: Vec<usize> = self
            .graph
            .nodes
            .iter()
            .filter(|n| self.level_attached.get(n.level).copied().unwrap_or(false))
            .map(|n| n.id)
            .collect();
        let mut matinees = Vec::new();
        for id in attached {
            let Some(node) = self.graph.node(id) else {
                continue;
            };
            match node.kind {
                NodeKind::Event => {
                    let Some(saved) = state.events.get(&node.path).copied() else {
                        continue;
                    };
                    let rest = if saved.retrigger_left.is_finite() {
                        saved.retrigger_left.max(0.0)
                    } else {
                        0.0
                    };
                    if let Some(op) = self.ops.get_mut(id) {
                        op.activation_time = now + f64::from(rest);
                        op.enabled = saved.enabled;
                        op.trigger_count = saved.trigger_count;
                    }
                }
                NodeKind::Variable => {
                    let Some(saved) = state.variables.get(&node.path) else {
                        continue;
                    };
                    let value = match (node.class, saved) {
                        (OpClass::VarBool, VarState::Bool(b)) => KValue::Bool(*b),
                        (OpClass::VarInt, VarState::Int(i)) => KValue::Int(*i),
                        (OpClass::VarFloat, VarState::Float(f)) if f.is_finite() => {
                            KValue::Float(*f)
                        }
                        (OpClass::VarObject, VarState::Object(o)) => KValue::Obj(o.clone()),
                        (OpClass::VarString, VarState::Str(s)) => KValue::Str(s.clone()),
                        _ => continue,
                    };
                    if let Some(slot) = self.vars.get_mut(id) {
                        *slot = value;
                    }
                }
                _ if node.class == OpClass::Interp => {
                    if let Some(saved) = state.matinees.get(&node.path).copied() {
                        matinees.push((id, saved));
                    }
                }
                _ => {}
            }
        }
        // The Matinees come after every event and variable in the save.
        for (id, saved) in matinees {
            if !saved.position.is_finite() {
                continue;
            }
            if saved.playing {
                if self.interp_set_position(id, saved.position, false, host) {
                    self.force_input(id, 0);
                }
            } else {
                self.interp_set_position(id, saved.position, true, host);
            }
            self.interp_set_paused(id, saved.paused);
        }
    }

    /// **Ours, not the original's**: the variables of this level as its own
    /// start leaves them, for a save that was written without script state
    /// (our builds up to 2026-10-12 saved none). The level start (begin
    /// play, the "save loaded" events with index −1, the match start) and
    /// one update without time run on a copy of this runtime against a host
    /// that does nothing, and the copy's variables are returned; the game is
    /// not touched and this runtime is unchanged. Restoring them is what
    /// keeps the levels' openings, which the shipped scripts guard with a
    /// bool variable set in that first update, from playing again on such a
    /// save. Everything else such a save lacks stays as at a fresh level
    /// start: trigger counts (one-time prompts and cutscenes fire again
    /// where the player walks into them again), `bEnabled` switches and the
    /// Matinees' positions. Empty once the level has started.
    #[must_use]
    pub fn level_start_variables(&self) -> BTreeMap<String, VarState> {
        if self.started {
            return BTreeMap::new();
        }
        let mut copy = self.clone();
        copy.start_state = None;
        copy.start_save_index = Some(-1);
        copy.tick(0.0, &mut NullHost);
        copy.capture_state().variables
    }

    /// Outputs a `SetPosition` on an action that is not active must not
    /// leave behind: the engine's teardown of the temporary group instances
    /// restores the view target and the fade, so the director track's cut
    /// and the fade track's amount of that update never reach the screen;
    /// and it stops the audio component a sound track's instance started in
    /// that update (STRONG: the stock sound track instance stops its sound
    /// when it is torn down, unless the track is set to let the sound
    /// outlive its Matinee; not re-read natively), so a sound key between
    /// the action's position and the new one is not heard. PLACEHOLDER for
    /// the tracks with that setting, which the converted Matinee data does
    /// not hold: their sound would be heard in the original.
    pub(crate) fn drop_director_outputs(&mut self, from: usize, node: usize) {
        let mut index = 0;
        self.outputs.retain(|o| {
            let keep = index < from
                || !matches!(
                    o,
                    Output::MatineeCut { node: n, .. }
                        | Output::MatineeFade { node: n, .. }
                        | Output::MatineeSound { node: n, .. }
                        if *n == node
                );
            index += 1;
            keep
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn floats_are_saved_with_four_decimals() {
        assert_eq!(save_float(1.234_56), 1.2346);
        assert_eq!(save_float(3.043_831_3), 3.0438);
        assert_eq!(save_float(0.000_04), 0.0);
        assert_eq!(save_float(-0.000_06), -0.0001);
        assert_eq!(save_float(f32::NAN), 0.0);
        assert_eq!(save_float(f32::INFINITY), 0.0);
        assert_eq!(save_float(f32::MAX), f32::MAX);
    }
}
