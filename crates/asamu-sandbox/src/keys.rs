//! The tunable-parameter catalogue.
//!
//! The key space is the dotted names of
//! [`PlayerParams::provenance_report`](asamu_player::PlayerParams::provenance_report)
//! for the Classic set (`movement.gravity_z`, `gun.max_distance`, ...), which
//! `asamu-player`'s own tests pin to the serialized structure. The Sandbox
//! adds no parameter and renames none: [`Catalog::classic`] is *derived* from
//! that report and from the types of the serialized leaves, so a parameter
//! added to the Classic set appears here by itself.
//!
//! Two things in this module are **ours**, not statements about the
//! original:
//!
//! - the [`Effect`] of a key (when a changed value reaches the running
//!   pawn), a Sandbox-owned table ([`EFFECTS`]) whose rows were read off the
//!   places the simulation reads or copies each value, and which the tests
//!   in `tests/keys.rs` and `tests/relatch.rs` hold to the simulation's
//!   actual behaviour;
//! - the nudge [`KeyInfo::step`], a round fraction of the Classic value
//!   computed when the catalogue is built (no number is written down here).

use std::sync::OnceLock;

use asamu_core::Provenance;
use asamu_player::PlayerParams;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// A value a key can be set to, as it appears in a profile file.
///
/// Untagged: `true`, `3`, `0.5` and `"inelastic"` are a bool, an integer, a
/// float and a choice name. An integer is accepted for a float key.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum TuneValue {
    /// A flag.
    Bool(bool),
    /// An integer (also accepted for float keys).
    Int(i64),
    /// A number.
    Float(f64),
    /// The name of a choice (see [`ValueKind::Choice`]).
    Text(String),
}

impl std::fmt::Display for TuneValue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Bool(v) => write!(f, "{v}"),
            Self::Int(v) => write!(f, "{v}"),
            Self::Float(v) => write!(f, "{v}"),
            Self::Text(v) => f.write_str(v),
        }
    }
}

/// The type of a key's value.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ValueKind {
    /// `f32` in the parameter set.
    Float,
    /// `i32` in the parameter set.
    Int,
    /// `bool`.
    Bool,
    /// One of the listed names (an enum in the parameter set).
    Choice(&'static [&'static str]),
}

/// When a changed value reaches the running pawn. Sandbox-owned table.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Effect {
    /// Read from the parameter set every tick: the next tick uses it.
    Live,
    /// Copied into a run-time value by the script layer; a retune
    /// ([`crate::relatch`]) brings the copy up to date.
    Latched,
    /// Read only when the pawn, gun or boots spawn: it applies to games the
    /// session builds itself, not to a game that is already running.
    SpawnOnly,
    /// Changing it has no effect on the Classic pipeline (the reason says
    /// why, e.g. only the placeholder model reads it).
    Inert(&'static str),
    /// Not classified yet (shown without a badge).
    Unclassified,
}

/// One tunable parameter.
#[derive(Clone, Debug, PartialEq)]
pub struct KeyInfo {
    /// Dotted name, e.g. `movement.jump_velocity`.
    pub key: String,
    /// Value type.
    pub kind: ValueKind,
    /// Unit, from the provenance report.
    pub unit: &'static str,
    /// What the parameter does, from the provenance report.
    pub description: &'static str,
    /// The Classic value.
    pub classic: TuneValue,
    /// Where the Classic value comes from.
    pub classic_provenance: Provenance,
    /// When a change takes effect.
    pub effect: Effect,
    /// Size of one user-interface nudge. Ours; not a limit.
    pub step: f64,
}

/// Why only the placeholder movement model reads a value.
const PLACEHOLDER_MODEL_ONLY: &str =
    "only the placeholder movement model reads it; the Classic pipeline never does";
/// Why a class default never reaches the Classic pawn.
const SCRIPT_OVERWRITES: &str =
    "the script layer runs on its own run-time value instead of this class default";
/// Why the debug rope grapple's values never reach the Classic pawn.
const DEBUG_ROPE_ONLY: &str =
    "only the debug rope grapple reads it; the Classic pipeline uses the grapple gun";
/// Why a recorded-only value has no effect.
const NEVER_READ: &str = "recorded only: nothing reads it";

/// The group whose keys are all [`Effect::Inert`]: the debug rope grapple,
/// which runs only without the script layer.
pub const INERT_GROUP: &str = "grapple";

/// The Sandbox's effect table: every classified key outside [`INERT_GROUP`].
/// A key that is not listed is [`Effect::Unclassified`].
///
/// Where each row comes from (`crates/asamu-player`):
///
/// - **Live**: read through the movement tuning or the controller on every
///   tick. Listed only when `tests/keys.rs` shows the very next tick
///   changing; other values that are read on use stay unclassified.
/// - **Latched**: copied into the pawn's run-time values when the pawn
///   starts, sprints, enters or leaves story mode, lands, or (the air speed)
///   when the gun spawns and after every release.
/// - **SpawnOnly**: read when the gun and the boots spawn, then overwritten
///   by the level's ability state.
/// - **Inert**: see the reason text of each row.
pub const EFFECTS: &[(&str, Effect)] = &[
    ("movement.world_gravity_z", Effect::Live),
    ("movement.custom_gravity_scaling", Effect::Live),
    ("movement.ground_acceleration", Effect::Live),
    ("movement.ground_friction", Effect::Live),
    ("movement.movement_speed_modifier", Effect::Live),
    ("movement.terminal_velocity", Effect::Live),
    ("camera.max_pitch_degrees", Effect::Live),
    ("pawn.move_speed", Effect::Latched),
    ("pawn.sprint_speed_multiplier", Effect::Latched),
    ("pawn.story_speed_multiplier", Effect::Latched),
    ("movement.air_control", Effect::Latched),
    ("pawn.landed_air_control", Effect::Latched),
    ("movement.jump_velocity", Effect::Latched),
    ("camera.fov_degrees", Effect::Latched),
    ("pawn.zoom_enabled", Effect::Latched),
    ("gun.grapple_accel", Effect::Latched),
    ("gun.initial_max_grapples", Effect::SpawnOnly),
    ("gun.initial_can_grapple", Effect::SpawnOnly),
    ("boots.initial_enabled", Effect::SpawnOnly),
    ("movement.gravity_z", Effect::Inert(PLACEHOLDER_MODEL_ONLY)),
    (
        "movement.braking_deceleration",
        Effect::Inert(PLACEHOLDER_MODEL_ONLY),
    ),
    (
        "movement.max_ground_speed",
        Effect::Inert(SCRIPT_OVERWRITES),
    ),
    ("movement.air_speed", Effect::Inert(SCRIPT_OVERWRITES)),
    ("gun.max_speed", Effect::Inert(NEVER_READ)),
];

/// The names each choice key accepts (the serialized names of the enum in
/// the parameter set). A choice key that is not listed has no known
/// alternatives and cannot be stepped.
const CHOICES: &[(&str, &[&str])] = &[
    ("grapple.rope_mode", &["inelastic", "shorten_to_distance"]),
    ("grapple.release_mode", &["preserve_velocity"]),
];

/// The [`Effect`] of `key`. A key missing from the table is
/// [`Effect::Unclassified`], so a new Classic parameter never breaks the
/// Sandbox.
#[must_use]
pub fn effect_of(key: &str) -> Effect {
    if key
        .split_once('.')
        .is_some_and(|(group, _)| group == INERT_GROUP)
    {
        return Effect::Inert(DEBUG_ROPE_ONLY);
    }
    EFFECTS
        .iter()
        .find(|(name, _)| *name == key)
        .map_or(Effect::Unclassified, |(_, effect)| *effect)
}

/// The serialized leaf of `key` (`group.field`) in a serialized parameter
/// set: the object holding exactly `value` and `provenance`.
pub(crate) fn leaf<'a>(root: &'a Value, key: &str) -> Option<&'a serde_json::Map<String, Value>> {
    let (group, field) = key.split_once('.')?;
    let leaf = root.get(group)?.get(field)?.as_object()?;
    (leaf.len() == 2 && leaf.contains_key("value") && leaf.contains_key("provenance"))
        .then_some(leaf)
}

/// [`leaf`], mutable.
pub(crate) fn leaf_mut<'a>(
    root: &'a mut Value,
    key: &str,
) -> Option<&'a mut serde_json::Map<String, Value>> {
    let (group, field) = key.split_once('.')?;
    let leaf = root.get_mut(group)?.get_mut(field)?.as_object_mut()?;
    (leaf.len() == 2 && leaf.contains_key("value") && leaf.contains_key("provenance"))
        .then_some(leaf)
}

/// The kind and value of a serialized leaf value.
fn classify(key: &str, value: &Value) -> Option<(ValueKind, TuneValue)> {
    match value {
        Value::Bool(b) => Some((ValueKind::Bool, TuneValue::Bool(*b))),
        Value::Number(n) => {
            // The serializer writes an `i32` as an integer and an `f32` as
            // a float (its exact `f64` value), so the number's own type
            // tells the two apart.
            if let Some(i) = n.as_i64() {
                Some((ValueKind::Int, TuneValue::Int(i)))
            } else {
                n.as_f64()
                    .map(|f| (ValueKind::Float, TuneValue::Float(short_decimal(f as f32))))
            }
        }
        Value::String(s) => {
            let choices = CHOICES
                .iter()
                .find(|(name, _)| *name == key)
                .map_or(&[][..], |(_, names)| *names);
            Some((ValueKind::Choice(choices), TuneValue::Text(s.clone())))
        }
        Value::Null | Value::Array(_) | Value::Object(_) => None,
    }
}

/// The `f64` that reads as the shortest decimal identifying `value` (`0.3`
/// rather than the exact `0.30000001192092896`), so a Classic value is shown
/// and written the way the parameter report prints it. It narrows back to
/// exactly `value`; should that ever not hold, the exact widening is used.
fn short_decimal(value: f32) -> f64 {
    let exact = f64::from(value);
    format!("{value}")
        .parse::<f64>()
        .ok()
        .filter(|short| (*short as f32).to_bits() == value.to_bits())
        .unwrap_or(exact)
}

/// A round nudge size for a float whose Classic value is `classic`: one
/// twentieth of its magnitude, rounded to 1, 2 or 5 times a power of ten.
/// Ours. Exact decimal (built from text, not by repeated multiplication).
fn nudge_step(classic: f64) -> f64 {
    /// For a Classic value of zero (or a broken one).
    const FALLBACK: f64 = 0.1;
    let target = classic.abs() / 20.0;
    if !(target.is_finite() && target > 0.0) {
        return FALLBACK;
    }
    // target = mantissa * 10^exponent with mantissa in [1, 10).
    let mut mantissa = target;
    let mut exponent: i32 = 0;
    while mantissa >= 10.0 && exponent < 300 {
        mantissa /= 10.0;
        exponent += 1;
    }
    while mantissa < 1.0 && exponent > -300 {
        mantissa *= 10.0;
        exponent -= 1;
    }
    let digit = if mantissa < 1.5 {
        1
    } else if mantissa < 3.5 {
        2
    } else if mantissa < 7.5 {
        5
    } else {
        exponent += 1;
        1
    };
    format!("{digit}e{exponent}")
        .parse::<f64>()
        .ok()
        .filter(|step| step.is_finite() && *step > 0.0)
        .unwrap_or(FALLBACK)
}

/// Every tunable key, in provenance-report order.
#[derive(Clone, Debug, PartialEq)]
pub struct Catalog {
    entries: Vec<KeyInfo>,
}

impl Catalog {
    /// The catalogue of the Classic set, derived from its provenance report
    /// and the types of its serialized leaves.
    #[must_use]
    pub fn classic() -> Self {
        Self::shared().clone()
    }

    /// The same catalogue, built once per process and borrowed. The Classic
    /// set is a constant of the build, so the cache can never go stale.
    #[must_use]
    pub fn shared() -> &'static Self {
        static CLASSIC: OnceLock<Catalog> = OnceLock::new();
        CLASSIC.get_or_init(|| Self::of(&PlayerParams::asamu_original()))
    }

    /// The catalogue of `params`: one entry per row of its report whose
    /// serialized leaf has a simple value.
    fn of(params: &PlayerParams) -> Self {
        // A parameter set always serializes (plain structs, string keys); a
        // failure would leave the catalogue empty, which the tests refuse.
        let root = serde_json::to_value(params).unwrap_or(Value::Null);
        let entries = params
            .provenance_report()
            .into_iter()
            .filter_map(|entry| {
                let value = leaf(&root, &entry.name)?.get("value")?;
                let (kind, classic) = classify(&entry.name, value)?;
                let step = match &classic {
                    TuneValue::Float(v) => nudge_step(*v),
                    TuneValue::Bool(_) | TuneValue::Int(_) | TuneValue::Text(_) => 1.0,
                };
                Some(KeyInfo {
                    effect: effect_of(&entry.name),
                    key: entry.name,
                    kind,
                    unit: entry.unit,
                    description: entry.description,
                    classic,
                    classic_provenance: entry.provenance,
                    step,
                })
            })
            .collect();
        Self { entries }
    }

    /// The entry of `key`.
    #[must_use]
    pub fn get(&self, key: &str) -> Option<&KeyInfo> {
        self.entries.iter().find(|e| e.key == key)
    }

    /// Every entry, in report order.
    pub fn iter(&self) -> impl Iterator<Item = &KeyInfo> {
        self.entries.iter()
    }

    /// Number of keys.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// No key at all (never the case for the Classic catalogue).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// The groups (`movement`, `grapple`, `camera`, `pawn`, `gun`, `boots`)
    /// in report order.
    #[must_use]
    pub fn groups(&self) -> Vec<&str> {
        let mut groups: Vec<&str> = Vec::new();
        for entry in &self.entries {
            let group = entry
                .key
                .split_once('.')
                .map_or(entry.key.as_str(), |g| g.0);
            if !groups.contains(&group) {
                groups.push(group);
            }
        }
        groups
    }

    /// The closest existing key to a mistyped one, if any is close: the one
    /// key with that field name when the group was left out or is wrong,
    /// else the key within a few single-character edits.
    #[must_use]
    pub fn suggest(&self, unknown: &str) -> Option<&str> {
        /// Longest input worth comparing (hostile profile files).
        const MAX_LEN: usize = 96;
        let unknown = unknown.trim();
        if unknown.is_empty() || unknown.len() > MAX_LEN {
            return None;
        }
        let wanted = unknown.to_ascii_lowercase();
        if let Some(exact) = self.entries.iter().find(|e| e.key == wanted) {
            return Some(&exact.key);
        }
        // The field name alone, or under the wrong group.
        let field = wanted.rsplit_once('.').map_or(wanted.as_str(), |p| p.1);
        let mut same_field = self
            .entries
            .iter()
            .filter(|e| e.key.split_once('.').is_some_and(|(_, f)| f == field));
        if let (Some(only), None) = (same_field.next(), same_field.next()) {
            return Some(&only.key);
        }
        // A few typos.
        let budget = (wanted.len() / 4).max(2);
        self.entries
            .iter()
            .map(|e| (edit_distance(&wanted, &e.key), e))
            .filter(|(distance, _)| *distance <= budget)
            .min_by_key(|(distance, _)| *distance)
            .map(|(_, e)| e.key.as_str())
    }
}

/// Levenshtein distance between two short ASCII-ish strings (bytes).
fn edit_distance(a: &str, b: &str) -> usize {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    let mut row: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.iter().enumerate() {
        let mut diagonal = row.first().copied().unwrap_or(0);
        if let Some(first) = row.first_mut() {
            *first = i + 1;
        }
        for (j, cb) in b.iter().enumerate() {
            let above = row.get(j + 1).copied().unwrap_or(usize::MAX);
            let left = row.get(j).copied().unwrap_or(usize::MAX);
            let cost = usize::from(ca != cb);
            let best = (diagonal + cost)
                .min(above.saturating_add(1))
                .min(left.saturating_add(1));
            diagonal = above;
            if let Some(cell) = row.get_mut(j + 1) {
                *cell = best;
            }
        }
    }
    row.last().copied().unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nudge_steps_are_round_decimals() {
        // Values chosen for the arithmetic, not taken from any parameter.
        for (value, step) in [
            (400.0, 20.0),
            (-400.0, 20.0),
            (2000.0, 100.0),
            (1.0, 0.05),
            (0.4, 0.02),
            (0.02, 0.001),
            (30.0, 2.0),
            (160.0, 10.0),
            (9.0e9, 5.0e8),
        ] {
            assert_eq!(nudge_step(value), step, "{value}");
        }
        for broken in [0.0, -0.0, f64::NAN, f64::INFINITY, f64::MIN_POSITIVE / 4.0] {
            let step = nudge_step(broken);
            assert!(step.is_finite() && step > 0.0, "{broken}: {step}");
        }
    }

    #[test]
    fn edit_distance_counts_single_character_edits() {
        assert_eq!(edit_distance("", ""), 0);
        assert_eq!(edit_distance("abc", "abc"), 0);
        assert_eq!(edit_distance("abc", ""), 3);
        assert_eq!(edit_distance("", "abc"), 3);
        assert_eq!(edit_distance("kitten", "sitting"), 3);
        assert_eq!(edit_distance("gravity", "gravty"), 1);
    }

    #[test]
    fn the_shared_catalogue_is_the_classic_catalogue() {
        let fresh = Catalog::of(&PlayerParams::asamu_original());
        assert_eq!(*Catalog::shared(), fresh);
        assert_eq!(Catalog::classic(), fresh);
        assert!(!fresh.is_empty());
        assert_eq!(fresh.len(), fresh.iter().count());
    }

    #[test]
    fn leaves_are_addressed_by_group_and_field_only() {
        let root = serde_json::to_value(PlayerParams::asamu_original()).unwrap();
        assert!(leaf(&root, "movement.gravity_z").is_some());
        for bad in [
            "",
            "movement",
            "movement.",
            ".gravity_z",
            "movement.gravity_z.value",
            "movement.gravity_z.provenance",
            "nothing.here",
        ] {
            assert!(leaf(&root, bad).is_none(), "{bad:?}");
        }
        // A group that is absent (the placeholder set has no script layer).
        let placeholder = serde_json::to_value(PlayerParams::placeholder()).unwrap();
        assert!(leaf(&placeholder, "pawn.move_speed").is_none());
    }
}
