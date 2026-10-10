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
//! `R_k`. The `init:` note (the state of tick 0, older field set) is still
//! written for readers of earlier conversions.
//!
//! `eye_height` changes on most frames of a moving pawn (26,283 of the 45,237
//! samples of the first three recordings), so it makes the line long: about
//! 45 bytes per change. A trace's meta line may have 1 MiB
//! (`asamu_player::trace::MAX_LINE_BYTES`), so a run with more than
//! [`EYE_HEIGHT_MAX_CHANGES`] changes is written without the field (a format
//! limit, not a game value; the converter says so in a `check:` note).
//!
//! | Field | Raw record | |
//! |---|---|---|
//! | `physics`, `base` | `physics`, `base` | pawn `Physics`, name of its `Base` |
//! | `ground_speed`, `air_speed`, `jump_z`, `air_control` | same | the pawn's run-time values |
//! | `eye_height` | `eye_height` | pawn `EyeHeight`: the camera height above the pawn's centre, before the walk bob (`null` when the recorder did not read it; absent from conversions made before it was kept, and from runs with too many changes) |
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
/// Most changes of `eye_height` a run may have for the field to be written
/// (see the module docs: a limit of the notes line, not a game value).
pub const EYE_HEIGHT_MAX_CHANGES: usize = 16_000;

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
    /// Pawn `EyeHeight`, UU above the collision centre (before the bob).
    #[serde(default)]
    pub eye_height: Option<f32>,
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

/// `true` when the run's `eye_height` goes into the `state:` note: it
/// changes on at most [`EYE_HEIGHT_MAX_CHANGES`] ticks.
#[must_use]
pub fn eye_height_kept(players: &[&RawPlayer]) -> bool {
    eye_height_changes(players) <= EYE_HEIGHT_MAX_CHANGES
}

/// Ticks of the run whose `eye_height` differs from the tick before.
#[must_use]
pub fn eye_height_changes(players: &[&RawPlayer]) -> usize {
    players
        .windows(2)
        .filter(|w| matches!(w, [a, b] if a.eye_height != b.eye_height))
        .count()
}

/// The timeline fields of one raw record, by name (sorted); `eye_height`
/// only when asked for.
fn fields_of(p: &RawPlayer, with_eye_height: bool) -> Result<BTreeMap<&'static str, Value>> {
    let mut m = BTreeMap::new();
    m.insert("air_control", float(p.air_control, "AirControl")?);
    m.insert("air_speed", float(p.air_speed, "AirSpeed")?);
    m.insert("base", opt(p.base.clone()));
    m.insert("boots_enabled", opt(p.boots.map(|b| b.enabled)));
    m.insert("boots_finished", opt(p.boots.map(|b| b.finished)));
    m.insert("can_grapple", opt(p.gun.map(|g| g.can_grapple)));
    if with_eye_height {
        let eye = match p.eye_height {
            Some(v) => float(v, "EyeHeight")?,
            None => Value::Null,
        };
        m.insert("eye_height", eye);
    }
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
    let with_eye_height = eye_height_kept(players);
    let mut changes: Vec<(u64, BTreeMap<&'static str, Value>)> = Vec::new();
    let mut last: Option<BTreeMap<&'static str, Value>> = None;
    for (k, p) in players.iter().enumerate() {
        let cur = fields_of(p, with_eye_height)?;
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

    /// A reader for ticks in increasing order ([`TimelineCursor`]): one
    /// pass over the entries instead of one per tick.
    #[must_use]
    pub fn cursor(&self) -> TimelineCursor<'_> {
        TimelineCursor {
            timeline: self,
            next: 0,
            fields: BTreeMap::new(),
            state: None,
        }
    }

    /// Calls `f(tick, before, after)` for every entry but the first: the
    /// state the tick before had and the state the entry leaves.
    ///
    /// # Errors
    /// A field with a value of the wrong type.
    pub fn for_each_change(&self, mut f: impl FnMut(u64, &TickState, &TickState)) -> Result<()> {
        let mut cursor = self.cursor();
        let mut before: Option<TickState> = None;
        for (tick, _) in &self.changes {
            let after = cursor.advance(*tick)?.cloned();
            if let (Some(b), Some(a)) = (&before, &after) {
                f(*tick, b, a);
            }
            before = after;
        }
        Ok(())
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

/// Reads a [`StateTimeline`] at ticks that never decrease.
#[derive(Clone, Debug)]
pub struct TimelineCursor<'a> {
    timeline: &'a StateTimeline,
    /// Index of the first entry not applied yet.
    next: usize,
    fields: BTreeMap<&'a str, &'a Value>,
    state: Option<TickState>,
}

impl TimelineCursor<'_> {
    /// The state at `tick` (as [`StateTimeline::at`]). A `tick` before the
    /// last one asked for gets the state of that last one: the cursor only
    /// moves forward.
    ///
    /// # Errors
    /// A field with a value of the wrong type.
    pub fn advance(&mut self, tick: u64) -> Result<Option<&TickState>> {
        let mut applied = false;
        while let Some((t, fields)) = self.timeline.changes.get(self.next) {
            if *t > tick {
                break;
            }
            for (name, v) in fields {
                self.fields.insert(name.as_str(), v);
            }
            self.next += 1;
            applied = true;
        }
        if applied {
            let state: TickState = serde_json::from_value(serde_json::to_value(&self.fields)?)
                .with_context(|| format!("invalid state: note (at tick {tick})"))?;
            self.state = Some(state);
        }
        Ok(self.state.as_ref())
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
    use crate::raw::{RawBoots, RawPawnFlags, RawPlayer};

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

    /// The eye height is a timeline field like the others, unless it changes
    /// on more ticks than a notes line takes.
    #[test]
    fn eye_height_in_the_timeline() {
        let at = |eye: Option<f32>| {
            let mut p = player(0.0, 0, &[]);
            p.eye_height = eye;
            p
        };
        let (a, b, c, none) = (at(Some(38.0)), at(Some(36.5)), at(Some(37.25)), at(None));
        let run = [&a, &a, &b, &c, &c, &none, &a];
        assert_eq!(eye_height_changes(&run), 4);
        assert!(eye_height_kept(&run));
        let note = state_note(&run).unwrap();
        let t = StateTimeline::from_notes(&[note]).unwrap().unwrap();
        assert_eq!(t.len(), 5, "ticks 0, 2, 3, 5, 6");
        let eyes: Vec<Option<f32>> = (0..7)
            .map(|k| t.at(k).unwrap().unwrap().eye_height)
            .collect();
        assert_eq!(
            eyes,
            [
                Some(38.0),
                Some(38.0),
                Some(36.5),
                Some(37.25),
                Some(37.25),
                None,
                Some(38.0)
            ]
        );
        // A cursor reads the same states in one pass; it never goes back.
        let mut cursor = t.cursor();
        for k in 0..7 {
            assert_eq!(cursor.advance(k).unwrap(), t.at(k).unwrap().as_ref(), "{k}");
        }
        assert_eq!(cursor.advance(2).unwrap(), t.at(6).unwrap().as_ref());
        assert_eq!(cursor.advance(u64::MAX).unwrap(), t.at(6).unwrap().as_ref());
        let late = StateTimeline::from_notes(&[
            "state: {\"v\":1,\"changes\":[[4,{\"physics\":1}]]}".into(),
        ])
        .unwrap()
        .unwrap();
        let mut early = late.cursor();
        assert_eq!(early.advance(3).unwrap(), None, "before the first entry");
        assert_eq!(early.advance(4).unwrap().unwrap().physics, Some(1));
        // The changes, each with the state before it.
        let mut seen = Vec::new();
        t.for_each_change(|tick, before, after| {
            seen.push((tick, before.eye_height, after.eye_height));
        })
        .unwrap();
        assert_eq!(
            seen,
            [
                (2, Some(38.0), Some(36.5)),
                (3, Some(36.5), Some(37.25)),
                (5, Some(37.25), None),
                (6, None, Some(38.0))
            ]
        );
        // A value that is not finite cannot be written.
        let nan = at(Some(f32::NAN));
        assert!(state_note(&[&nan]).is_err());
        // More changes than a line takes: the field is left out everywhere
        // (an entry per tick would not fit the meta line), nothing else is.
        let (lo, hi) = (at(Some(30.0)), at(Some(31.0)));
        let restless: Vec<&RawPlayer> = (0..EYE_HEIGHT_MAX_CHANGES + 2)
            .map(|k| if k % 2 == 0 { &lo } else { &hi })
            .collect();
        assert_eq!(eye_height_changes(&restless), EYE_HEIGHT_MAX_CHANGES + 1);
        assert!(!eye_height_kept(&restless));
        let note = state_note(&restless).unwrap();
        assert!(!note.contains("eye_height"), "{}", &note[..200]);
        let t = StateTimeline::from_notes(&[note]).unwrap().unwrap();
        assert_eq!(t.len(), 1);
        let s = t.at(5).unwrap().unwrap();
        assert_eq!((s.eye_height, s.ground_speed), (None, Some(440.0)));
        // Exactly at the limit it stays.
        let at_limit = &restless[..=EYE_HEIGHT_MAX_CHANGES];
        assert!(eye_height_kept(at_limit));
        let note = state_note(at_limit).unwrap();
        assert!(
            note.len() < asamu_player::trace::MAX_LINE_BYTES,
            "{} bytes",
            note.len()
        );
        let t = StateTimeline::from_notes(&[note]).unwrap().unwrap();
        assert_eq!(t.len(), EYE_HEIGHT_MAX_CHANGES + 1);
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
