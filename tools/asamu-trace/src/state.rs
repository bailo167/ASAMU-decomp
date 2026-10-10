//! The script state the recorder read on every frame, kept in the trace's
//! notes so that a replay can start from any tick.
//!
//! Schema v1 samples carry position, velocity, view, FOV, the grapple state
//! and `grounded`. What else decides the pawn's next move (the latched
//! `GroundSpeed`, `AirControl`, the sprint flag, the grapple budget, ...) is
//! in the raw records only. The converter writes it into one notes line,
//!
//! ```text
//! state: {"changes":[[0,{...every field...}],[57,{"physics":2}],...],"v":1}
//! ```
//!
//! a list of `[tick, {field: value}]` entries in tick order: the entry of
//! tick 0 holds every field, a later entry the fields whose value differs
//! from the tick before. Tick `k` is sample `k`, the state of raw record
//! `R_k`. (`EyeHeight` is left out: it changes on most frames.) The `init:`
//! note (the state of tick 0, older field set) is still written for readers
//! of earlier conversions.
//!
//! | Field | Raw record | |
//! |---|---|---|
//! | `physics`, `base` | `physics`, `base` | pawn `Physics`, name of its `Base` |
//! | `ground_speed`, `air_speed`, `jump_z`, `air_control` | same | the pawn's run-time values |
//! | `sprinting`, `has_jumped`, `power_jumped`, `has_released_jump`, `is_falling` | `pawn_flags` | `ASAMUPawn` flags (`null` without them) |
//! | `grappling`, `released`, `can_grapple`, `times_grappled`, `max_grapples` | `gun` | grapple gun (`null` without one) |
//! | `boots_enabled`, `boots_finished` | `boots` | rocket boots (`null` without them) |
//!
//! `tools/trace-recorder/asamu_recorder_core.py` (`_state_note`) writes the
//! same line; `tests/python_crosscheck.rs` compares the two as JSON.

use std::collections::BTreeMap;

use anyhow::{Context, Result, bail};
use asamu_player::PawnParams;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::convert::InitState;
use crate::raw::RawPlayer;

/// Prefix of the notes line carrying the state timeline.
pub const STATE_NOTE_PREFIX: &str = "state: ";
/// Version of the timeline's encoding.
pub const STATE_NOTE_VERSION: u64 = 1;

/// The recorded script state at one tick. A `None` field was not recorded
/// (no gun, flags or boots data; a trace with only an `init:` note).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct TickState {
    /// `AirControl`.
    #[serde(default)]
    pub air_control: Option<f32>,
    /// `AirSpeed`.
    #[serde(default)]
    pub air_speed: Option<f32>,
    /// Name of the pawn's `Base` actor.
    #[serde(default)]
    pub base: Option<String>,
    /// Rocket boots `bEnabled`.
    #[serde(default)]
    pub boots_enabled: Option<bool>,
    /// Rocket boots `bFinished`.
    #[serde(default)]
    pub boots_finished: Option<bool>,
    /// Gun `bCanGrapple` (the fire latch).
    #[serde(default)]
    pub can_grapple: Option<bool>,
    /// Gun `bIsGrappling`.
    #[serde(default)]
    pub grappling: Option<bool>,
    /// `GroundSpeed`.
    #[serde(default)]
    pub ground_speed: Option<f32>,
    /// Pawn `bHasJumped`.
    #[serde(default)]
    pub has_jumped: Option<bool>,
    /// Pawn `bHasReleasedJump`.
    #[serde(default)]
    pub has_released_jump: Option<bool>,
    /// Pawn `bIsFalling`.
    #[serde(default)]
    pub is_falling: Option<bool>,
    /// `JumpZ`.
    #[serde(default)]
    pub jump_z: Option<f32>,
    /// Gun `iMaxGrapples`.
    #[serde(default)]
    pub max_grapples: Option<i32>,
    /// Pawn `Physics`.
    #[serde(default)]
    pub physics: Option<u8>,
    /// Pawn `bPowerJumped`.
    #[serde(default)]
    pub power_jumped: Option<bool>,
    /// Gun `bReleasedGrapple`.
    #[serde(default)]
    pub released: Option<bool>,
    /// Pawn `bSprinting`.
    #[serde(default)]
    pub sprinting: Option<bool>,
    /// Gun `iTimesGrappled`.
    #[serde(default)]
    pub times_grappled: Option<i32>,
}

fn float(v: f32, what: &str) -> Result<Value> {
    serde_json::Number::from_f64(f64::from(v))
        .map(Value::Number)
        .with_context(|| format!("{what} is not finite"))
}

fn opt<T: Into<Value>>(v: Option<T>) -> Value {
    v.map_or(Value::Null, Into::into)
}

/// The timeline fields of one raw record, by name (sorted).
fn fields_of(p: &RawPlayer) -> Result<BTreeMap<&'static str, Value>> {
    let mut m = BTreeMap::new();
    m.insert("air_control", float(p.air_control, "AirControl")?);
    m.insert("air_speed", float(p.air_speed, "AirSpeed")?);
    m.insert("base", opt(p.base.clone()));
    m.insert("boots_enabled", opt(p.boots.map(|b| b.enabled)));
    m.insert("boots_finished", opt(p.boots.map(|b| b.finished)));
    m.insert("can_grapple", opt(p.gun.map(|g| g.can_grapple)));
    m.insert("grappling", opt(p.gun.map(|g| g.grappling)));
    m.insert("ground_speed", float(p.ground_speed, "GroundSpeed")?);
    m.insert("has_jumped", opt(p.pawn_flags.map(|f| f.has_jumped)));
    m.insert(
        "has_released_jump",
        opt(p.pawn_flags.map(|f| f.has_released_jump)),
    );
    m.insert("is_falling", opt(p.pawn_flags.map(|f| f.is_falling)));
    m.insert("jump_z", float(p.jump_z, "JumpZ")?);
    m.insert("max_grapples", opt(p.gun.map(|g| g.max_grapples)));
    m.insert("physics", Value::from(p.physics));
    m.insert("power_jumped", opt(p.pawn_flags.map(|f| f.power_jumped)));
    m.insert("released", opt(p.gun.map(|g| g.released)));
    m.insert("sprinting", opt(p.pawn_flags.map(|f| f.sprinting)));
    m.insert("times_grappled", opt(p.gun.map(|g| g.times_grappled)));
    Ok(m)
}

/// The `state:` notes line for the players of a run (`players[k]` is the
/// player of sample `k`).
///
/// # Errors
/// A value that is not finite.
pub fn state_note(players: &[&RawPlayer]) -> Result<String> {
    let mut changes: Vec<(u64, BTreeMap<&'static str, Value>)> = Vec::new();
    let mut last: Option<BTreeMap<&'static str, Value>> = None;
    for (k, p) in players.iter().enumerate() {
        let cur = fields_of(p)?;
        let diff: BTreeMap<&'static str, Value> = match &last {
            None => cur.clone(),
            Some(prev) => cur
                .iter()
                .filter(|(name, v)| prev.get(*name) != Some(v))
                .map(|(name, v)| (*name, v.clone()))
                .collect(),
        };
        if !diff.is_empty() {
            changes.push((k as u64, diff));
        }
        last = Some(cur);
    }
    let mut top: BTreeMap<&'static str, Value> = BTreeMap::new();
    top.insert("changes", serde_json::to_value(&changes)?);
    top.insert("v", Value::from(STATE_NOTE_VERSION));
    Ok(format!(
        "{STATE_NOTE_PREFIX}{}",
        serde_json::to_string(&top)?
    ))
}

/// What a recorded `GroundSpeed` shows about story mode: `Some(true)` for
/// the story speed, `Some(false)` for the walking or the sprint speed, `None`
/// for anything else. The pawn's own script writes these three values only
/// (ABILITIES.md A-WK-3: pawn start, sprint applied or removed, story mode
/// entered or left); another value comes from a console `SetSpeed`, which
/// the Kismet of AG-Workshop and AG-Epilogue issues, and says nothing about
/// the story state. The speeds are computed from `pawn` as the script does.
#[must_use]
pub fn story_mode_shown(ground_speed: f32, pawn: &PawnParams) -> Option<bool> {
    let walk = pawn.move_speed.value;
    if ground_speed == walk * pawn.story_speed_multiplier.value {
        Some(true)
    } else if ground_speed == walk || ground_speed == walk * pawn.sprint_speed_multiplier.value {
        Some(false)
    } else {
        None
    }
}

/// The state timeline of a trace.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct StateTimeline {
    /// `(tick, changed fields)` in tick order.
    changes: Vec<(u64, BTreeMap<String, Value>)>,
}

#[derive(Deserialize)]
struct Encoded {
    v: u64,
    changes: Vec<(u64, BTreeMap<String, Value>)>,
}

impl StateTimeline {
    /// Parses the `state:` note of a trace, if it has one.
    ///
    /// # Errors
    /// A `state:` note that is not valid JSON, has another version, or
    /// whose ticks are not in increasing order.
    pub fn from_notes(notes: &[String]) -> Result<Option<Self>> {
        let Some(json) = notes
            .iter()
            .rev()
            .find_map(|n| n.strip_prefix(STATE_NOTE_PREFIX))
        else {
            return Ok(None);
        };
        let e: Encoded = serde_json::from_str(json).context("invalid state: note")?;
        if e.v != STATE_NOTE_VERSION {
            bail!(
                "state: note version {} (supported: {STATE_NOTE_VERSION})",
                e.v
            );
        }
        if e.changes.windows(2).any(|w| w[0].0 >= w[1].0) {
            bail!("state: note with ticks out of order");
        }
        Ok(Some(Self { changes: e.changes }))
    }

    /// The state at `tick`: every change up to and including it applied in
    /// order. `None` before the first entry.
    ///
    /// # Errors
    /// A field with a value of the wrong type.
    pub fn at(&self, tick: u64) -> Result<Option<TickState>> {
        let mut all: BTreeMap<&str, &Value> = BTreeMap::new();
        let mut any = false;
        for (t, fields) in &self.changes {
            if *t > tick {
                break;
            }
            any = true;
            for (name, v) in fields {
                all.insert(name.as_str(), v);
            }
        }
        if !any {
            return Ok(None);
        }
        let state: TickState = serde_json::from_value(serde_json::to_value(&all)?)
            .with_context(|| format!("invalid state: note (at tick {tick})"))?;
        Ok(Some(state))
    }

    /// Number of entries.
    #[must_use]
    pub fn len(&self) -> usize {
        self.changes.len()
    }

    /// `true` without entries.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.changes.is_empty()
    }
}

impl From<&InitState> for TickState {
    /// The fields an `init:` note has (a trace converted before the
    /// timeline existed); they describe the first sample only.
    fn from(i: &InitState) -> Self {
        Self {
            air_control: Some(i.air_control),
            base: i.base.clone(),
            ground_speed: Some(i.ground_speed),
            jump_z: Some(i.jump_z),
            max_grapples: i.max_grapples,
            physics: Some(i.physics),
            boots_enabled: i.rocket_boots,
            sprinting: i.sprinting,
            times_grappled: i.times_grappled,
            ..Self::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::raw::tests::player;
    use crate::raw::{RawBoots, RawPawnFlags};

    #[test]
    fn timeline_round_trip() {
        let mut a = player(0.0, 0, &[]);
        a.ground_speed = 264.0;
        a.air_control = 0.35;
        a.base = Some("StaticMeshActor_7".into());
        let mut b = a.clone();
        b.physics = 2;
        b.base = None;
        b.gun.as_mut().unwrap().times_grappled = 1;
        let mut c = b.clone();
        c.ground_speed = 880.0;
        c.pawn_flags = Some(RawPawnFlags {
            has_jumped: true,
            power_jumped: false,
            has_released_jump: false,
            sprinting: true,
            is_falling: true,
        });
        c.boots = Some(RawBoots {
            enabled: false,
            finished: true,
        });
        // Ticks 1 and 2 repeat tick 0; tick 5 repeats tick 4.
        let run = [&a, &a, &a, &b, &c, &c];
        let note = state_note(&run).unwrap();
        assert!(note.starts_with("state: {\"changes\":[[0,{\"air_control\":"));
        assert!(note.ends_with("],\"v\":1}"), "{note}");
        let t = StateTimeline::from_notes(&["x".into(), note.clone()])
            .unwrap()
            .unwrap();
        assert_eq!(t.len(), 3, "{note}");
        let s0 = t.at(0).unwrap().unwrap();
        assert_eq!(s0.ground_speed, Some(264.0));
        assert_eq!(s0.air_control, Some(0.35));
        assert_eq!(s0.base.as_deref(), Some("StaticMeshActor_7"));
        assert_eq!(s0.physics, Some(1));
        assert_eq!(s0.max_grapples, Some(2));
        assert_eq!(t.at(2).unwrap().unwrap(), s0);
        let s3 = t.at(3).unwrap().unwrap();
        assert_eq!((s3.physics, s3.base.clone()), (Some(2), None));
        assert_eq!(s3.times_grappled, Some(1));
        assert_eq!(s3.ground_speed, Some(264.0));
        let s4 = t.at(4).unwrap().unwrap();
        assert_eq!(s4.ground_speed, Some(880.0));
        assert_eq!(s4.sprinting, Some(true));
        assert_eq!(s4.boots_finished, Some(true));
        assert_eq!(t.at(5).unwrap().unwrap(), s4);
        assert_eq!(t.at(u64::MAX).unwrap().unwrap(), s4);

        // Without gun, flags and boots the fields are null.
        let mut bare = a.clone();
        bare.gun = None;
        bare.pawn_flags = None;
        bare.boots = None;
        let t = StateTimeline::from_notes(&[state_note(&[&bare]).unwrap()])
            .unwrap()
            .unwrap();
        let s = t.at(0).unwrap().unwrap();
        assert_eq!(
            (s.max_grapples, s.sprinting, s.boots_enabled),
            (None, None, None)
        );
        assert_eq!(s.jump_z, Some(1000.0));
    }

    #[test]
    fn hostile_notes_are_errors() {
        assert_eq!(StateTimeline::from_notes(&[]).unwrap(), None);
        assert_eq!(
            StateTimeline::from_notes(&["init: {}".into()]).unwrap(),
            None
        );
        for bad in [
            "state: ",
            "state: []",
            "state: {\"v\":2,\"changes\":[]}",
            "state: {\"v\":1,\"changes\":[[3,{}],[3,{}]]}",
            "state: {\"v\":1,\"changes\":[[5,{}],[4,{}]]}",
            "state: {\"v\":1,\"changes\":[[-1,{}]]}",
        ] {
            assert!(StateTimeline::from_notes(&[bad.into()]).is_err(), "{bad}");
        }
        // A wrong value type shows when the tick is read.
        let t = StateTimeline::from_notes(&[
            "state: {\"v\":1,\"changes\":[[2,{\"physics\":\"walking\"}]]}".into(),
        ])
        .unwrap()
        .unwrap();
        assert_eq!(t.at(1).unwrap(), None, "before the first entry");
        assert!(t.at(2).is_err());
        // Unknown fields are ignored (a later converter may add some).
        let t = StateTimeline::from_notes(&[
            "state: {\"v\":1,\"changes\":[[0,{\"physics\":1,\"new_field\":[1,2]}]]}".into(),
        ])
        .unwrap()
        .unwrap();
        assert_eq!(t.at(0).unwrap().unwrap().physics, Some(1));
        // A value that is not finite cannot be written.
        let mut p = player(0.0, 0, &[]);
        p.ground_speed = f32::NAN;
        assert!(state_note(&[&p]).is_err());
    }

    #[test]
    fn story_mode_from_ground_speed() {
        let params = asamu_player::PlayerParams::asamu_original();
        let pawn = params.pawn.as_ref().unwrap();
        assert_eq!(story_mode_shown(264.0, pawn), Some(true));
        assert_eq!(story_mode_shown(440.0, pawn), Some(false));
        assert_eq!(story_mode_shown(880.0, pawn), Some(false));
        // The Workshop's and the Epilogue's console speeds show nothing.
        assert_eq!(story_mode_shown(132.0, pawn), None);
        assert_eq!(story_mode_shown(66.0, pawn), None);
        assert_eq!(story_mode_shown(f32::NAN, pawn), None);
    }

    #[test]
    fn init_note_as_tick_state() {
        let init = InitState {
            air_control: 0.3,
            base: None,
            ground_speed: 440.0,
            jump_z: 1000.0,
            max_grapples: Some(3),
            physics: 1,
            rocket_boots: Some(true),
            sprinting: Some(false),
            times_grappled: Some(1),
        };
        let s = TickState::from(&init);
        assert_eq!(s.max_grapples, Some(3));
        assert_eq!(s.boots_enabled, Some(true));
        assert_eq!(s.air_speed, None);
        assert_eq!(s.can_grapple, None);
    }
}
