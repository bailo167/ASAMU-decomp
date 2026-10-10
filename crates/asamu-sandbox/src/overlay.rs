//! Overrides on the Classic parameter set.
//!
//! An [`Overlay`] is a map from parameter key ([`crate::keys`]) to value. It
//! never stores a whole parameter set: [`Overlay::apply`] starts from
//! [`PlayerParams::asamu_original`] every time and replaces only the listed
//! leaves, giving each replaced leaf placeholder provenance whose note
//! starts with [`OVERRIDE_NOTE_PREFIX`]. An empty overlay therefore *is* the
//! Classic set, value for value and provenance for provenance: it is
//! returned straight from the Classic constructor, with no serialization in
//! between.
//!
//! Only existing leaves are addressable, so the script-layer groups (`pawn`,
//! `gun`, `boots`) can neither appear nor disappear and the simulation
//! pipeline cannot switch.
//!
//! There is no range table here. Whether a value is acceptable is decided by
//! [`PlayerParams::validate`], the Classic set's own check, on the whole
//! resulting set; its requirement text is what a refused change reports.

use std::collections::BTreeMap;

use asamu_core::Provenance;
use asamu_player::PlayerParams;
use asamu_player::params::ParamError;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::keys::{Catalog, KeyInfo, TuneValue, ValueKind, leaf_mut};

/// The only base a profile may name in v1: the Classic set
/// ([`PlayerParams::asamu_original`]).
pub const BASE_CLASSIC: &str = "asamu_original";

/// Every override's provenance note starts with this, so a tuned value can
/// never be read as an original one.
pub const OVERRIDE_NOTE_PREFIX: &str = "sandbox override";

/// Upper bound on the entries of one overlay (hostile profile files).
pub const MAX_OVERRIDES: usize = 256;

/// One nudge moves a float on a grid this many times finer than the key's
/// step (ours), so repeated nudges give round decimals instead of
/// accumulating binary noise.
const NUDGE_GRID_PER_STEP: f64 = 1000.0;

/// Overrides on the Classic set, by parameter key.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Overlay(BTreeMap<String, TuneValue>);

impl Overlay {
    /// No override: the Classic set.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Number of overrides.
    #[must_use]
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// The override of `key`, if any.
    #[must_use]
    pub fn get(&self, key: &str) -> Option<&TuneValue> {
        self.0.get(key)
    }

    /// The overrides in key order.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &TuneValue)> {
        self.0.iter().map(|(k, v)| (k.as_str(), v))
    }

    /// Overrides `key`. Commits only if the whole resulting set validates;
    /// on error the overlay is unchanged. A value equal to the Classic one
    /// removes the entry.
    ///
    /// # Errors
    /// Unknown key, wrong type, unrepresentable value, a set that fails
    /// [`PlayerParams::validate`], or too many overrides.
    pub fn set(&mut self, key: &str, value: TuneValue) -> Result<(), OverlayError> {
        let info = lookup(key)?;
        let value = coerce(info, value)?;
        let mut next = self.0.clone();
        if same_value(info, &value, &info.classic) {
            next.remove(key);
        } else {
            next.insert(info.key.clone(), value);
        }
        build(&next)?;
        self.0 = next;
        Ok(())
    }

    /// Moves `key` by `steps` nudges of its catalogue step times `scale`
    /// (bools and choices step through their values) and returns the new
    /// value. Same commit rule as [`Overlay::set`].
    ///
    /// Floats land on round decimals (a grid a thousand times finer than
    /// the step) and snap back onto the Classic value when a nudge passes
    /// within half a grid cell of it, so "up, then down" is Classic again.
    ///
    /// # Errors
    /// As [`Overlay::set`]; also a `scale` that is not a positive number.
    pub fn nudge(&mut self, key: &str, steps: i32, scale: f64) -> Result<TuneValue, OverlayError> {
        let info = lookup(key)?;
        if !(scale.is_finite() && scale > 0.0) {
            return Err(OverlayError::NotRepresentable {
                key: info.key.clone(),
            });
        }
        let current = self.0.get(key).unwrap_or(&info.classic);
        let next = match (info.kind, current) {
            (ValueKind::Float, TuneValue::Float(now)) => {
                let classic = match info.classic {
                    TuneValue::Float(c) => c,
                    _ => *now,
                };
                TuneValue::Float(nudged_float(
                    *now,
                    classic,
                    info.step,
                    f64::from(steps),
                    scale,
                ))
            }
            (ValueKind::Float, TuneValue::Int(now)) => {
                TuneValue::Float(*now as f64 + f64::from(steps) * info.step * scale)
            }
            (ValueKind::Int, TuneValue::Int(now)) => {
                // Whole steps only: at least one unit per nudge.
                let unit = (info.step * scale).round().max(1.0);
                let delta = f64::from(steps) * unit;
                if delta.abs() > f64::from(u32::MAX) {
                    return Err(OverlayError::NotRepresentable {
                        key: info.key.clone(),
                    });
                }
                TuneValue::Int(now.saturating_add(delta as i64))
            }
            (ValueKind::Bool, TuneValue::Bool(now)) => TuneValue::Bool(*now ^ (steps % 2 != 0)),
            (ValueKind::Choice(names), TuneValue::Text(now)) => {
                match names.iter().position(|n| *n == now.as_str()) {
                    Some(index) => {
                        let count = names.len() as i64;
                        let next = (index as i64 + i64::from(steps)).rem_euclid(count);
                        let name = names.get(next as usize).copied().unwrap_or(now.as_str());
                        TuneValue::Text(name.to_owned())
                    }
                    // No known alternatives: nothing to step through.
                    None => TuneValue::Text(now.clone()),
                }
            }
            // An entry of the wrong type (only possible in an overlay read
            // from a file): refuse rather than guess.
            _ => return Err(wrong_type(info)),
        };
        self.set(key, next)?;
        Ok(self.0.get(key).unwrap_or(&info.classic).clone())
    }

    /// Removes the override of `key`; `true` if there was one.
    pub fn clear(&mut self, key: &str) -> bool {
        self.0.remove(key).is_some()
    }

    /// Removes every override.
    pub fn clear_all(&mut self) {
        self.0.clear();
    }

    /// The Classic parameters with the overrides applied. An empty overlay
    /// returns [`PlayerParams::asamu_original`] itself (no serialization
    /// round trip).
    ///
    /// # Errors
    /// An entry that does not apply (e.g. from an edited profile file): an
    /// unknown key, a wrong type, an unrepresentable value, too many
    /// entries, or a resulting set that fails [`PlayerParams::validate`].
    pub fn apply(&self) -> Result<PlayerParams, OverlayError> {
        build(&self.0)
    }

    /// The same overrides in the form [`Overlay::set`] would have stored
    /// them (integers given for float keys become floats, entries equal to
    /// the Classic value are dropped), after checking that the whole set
    /// applies. For overlays that were read from a file rather than built
    /// through [`Overlay::set`].
    ///
    /// # Errors
    /// As [`Overlay::apply`].
    pub fn normalized(&self) -> Result<Self, OverlayError> {
        build(&self.0)?;
        let mut out = BTreeMap::new();
        for (key, value) in &self.0 {
            let info = lookup(key)?;
            let value = coerce(info, value.clone())?;
            if !same_value(info, &value, &info.classic) {
                out.insert(info.key.clone(), value);
            }
        }
        Ok(Self(out))
    }
}

/// The catalogue entry of `key`, or the error naming the closest key.
fn lookup(key: &str) -> Result<&'static KeyInfo, OverlayError> {
    let catalog = Catalog::shared();
    catalog.get(key).ok_or_else(|| OverlayError::UnknownKey {
        key: key.to_owned(),
        suggestion: catalog.suggest(key).map(str::to_owned),
    })
}

/// The [`OverlayError::WrongType`] of `info`'s kind.
fn wrong_type(info: &KeyInfo) -> OverlayError {
    OverlayError::WrongType {
        key: info.key.clone(),
        expected: match info.kind {
            ValueKind::Float => "a number",
            ValueKind::Int => "a whole number",
            ValueKind::Bool => "true or false",
            ValueKind::Choice(_) => "one of its choice names",
        },
    }
}

/// Checks `value` against the key's type and range of representation and
/// returns it in the key's own variant (an integer for a float key becomes
/// a float).
fn coerce(info: &KeyInfo, value: TuneValue) -> Result<TuneValue, OverlayError> {
    let unrepresentable = || OverlayError::NotRepresentable {
        key: info.key.clone(),
    };
    match (info.kind, value) {
        (ValueKind::Float, TuneValue::Float(v)) => {
            if v.is_finite() && (v as f32).is_finite() {
                Ok(TuneValue::Float(v))
            } else {
                Err(unrepresentable())
            }
        }
        (ValueKind::Float, TuneValue::Int(v)) => Ok(TuneValue::Float(v as f64)),
        (ValueKind::Int, TuneValue::Int(v)) => {
            if i32::try_from(v).is_ok() {
                Ok(TuneValue::Int(v))
            } else {
                Err(unrepresentable())
            }
        }
        (ValueKind::Bool, TuneValue::Bool(v)) => Ok(TuneValue::Bool(v)),
        (ValueKind::Choice(names), TuneValue::Text(v)) => {
            // An enum without a list of names here is checked by the
            // parameter set's own deserializer in `build`.
            if names.is_empty() || names.contains(&v.as_str()) {
                Ok(TuneValue::Text(v))
            } else {
                Err(wrong_type(info))
            }
        }
        _ => Err(wrong_type(info)),
    }
}

/// Whether two coerced values of `info`'s kind are the same value **in the
/// parameter set**: floats are compared as the `f32` the set stores, bit
/// for bit.
fn same_value(info: &KeyInfo, a: &TuneValue, b: &TuneValue) -> bool {
    match (info.kind, a, b) {
        (ValueKind::Float, TuneValue::Float(a), TuneValue::Float(b)) => {
            (*a as f32).to_bits() == (*b as f32).to_bits()
        }
        _ => a == b,
    }
}

/// A float moved by `steps` nudges of `step * scale`, on the nudge grid.
fn nudged_float(now: f64, classic: f64, step: f64, steps: f64, scale: f64) -> f64 {
    let raw = now + steps * step * scale;
    let grid = step / NUDGE_GRID_PER_STEP;
    if !(raw.is_finite() && grid.is_finite() && grid > 0.0) {
        return raw;
    }
    if (raw - classic).abs() <= grid / 2.0 {
        return classic;
    }
    let snapped = (raw / grid).round() * grid;
    // Print-and-parse removes the last-bit noise of the multiplication, so
    // the value reads as the decimal it is meant to be.
    let digits = decimals_of(grid);
    format!("{snapped:.digits$}")
        .parse::<f64>()
        .ok()
        .filter(|v| v.is_finite())
        .unwrap_or(snapped)
}

/// Number of decimal places needed to write `grid` (a power of ten times 1,
/// 2 or 5), at most 12.
fn decimals_of(grid: f64) -> usize {
    let mut places = 0;
    let mut unit = grid;
    while unit < 1.0 && places < 12 {
        unit *= 10.0;
        places += 1;
    }
    places
}

/// The JSON form of a coerced value as the parameter set stores it.
fn json_value(info: &KeyInfo, value: &TuneValue) -> Result<Value, OverlayError> {
    let unrepresentable = || OverlayError::NotRepresentable {
        key: info.key.clone(),
    };
    match value {
        TuneValue::Bool(v) => Ok(Value::Bool(*v)),
        TuneValue::Int(v) => i32::try_from(*v)
            .map(Value::from)
            .map_err(|_| unrepresentable()),
        TuneValue::Float(v) => {
            // Exactly the `f32` the set will hold, widened back: the same
            // form the serializer writes for every untouched value.
            let narrowed = *v as f32;
            if !narrowed.is_finite() {
                return Err(unrepresentable());
            }
            serde_json::Number::from_f64(f64::from(narrowed))
                .map(Value::Number)
                .ok_or_else(unrepresentable)
        }
        TuneValue::Text(v) => Ok(Value::String(v.clone())),
    }
}

/// The provenance of an overridden value: a placeholder whose note says it
/// is a Sandbox override and what the Classic value and its source are. It
/// depends on the key only, never on the value chosen.
fn override_provenance(info: &KeyInfo) -> Provenance {
    Provenance::placeholder(format!(
        "{OVERRIDE_NOTE_PREFIX} (not the original's value); Classic: {} from {}",
        info.classic, info.classic_provenance
    ))
}

/// The Classic parameters with `entries` applied (see [`Overlay::apply`]).
fn build(entries: &BTreeMap<String, TuneValue>) -> Result<PlayerParams, OverlayError> {
    if entries.is_empty() {
        // The Classic set itself: no serialization, no validation pass, no
        // chance of changing a bit.
        return Ok(PlayerParams::asamu_original());
    }
    if entries.len() > MAX_OVERRIDES {
        return Err(OverlayError::TooMany {
            limit: MAX_OVERRIDES,
        });
    }
    let shape = |what: &str| OverlayError::Shape(what.to_owned());
    let mut root = serde_json::to_value(PlayerParams::asamu_original())
        .map_err(|e| OverlayError::Shape(format!("the Classic set did not serialize: {e}")))?;
    for (key, value) in entries {
        let info = lookup(key)?;
        let value = coerce(info, value.clone())?;
        let json = json_value(info, &value)?;
        let provenance = serde_json::to_value(override_provenance(info))
            .map_err(|e| OverlayError::Shape(format!("a provenance did not serialize: {e}")))?;
        let leaf = leaf_mut(&mut root, key)
            .ok_or_else(|| shape("a catalogue key has no leaf in the serialized set"))?;
        leaf.insert("value".to_owned(), json);
        leaf.insert("provenance".to_owned(), provenance);
    }
    let params: PlayerParams = serde_json::from_value(root)
        .map_err(|e| OverlayError::Shape(format!("the set did not read back: {e}")))?;
    params.validate()?;
    Ok(params)
}

/// The " (did you mean ...?)" tail of [`OverlayError::UnknownKey`].
fn did_you_mean(suggestion: &Option<String>) -> String {
    suggestion
        .as_ref()
        .map(|s| format!(" (did you mean {s:?}?)"))
        .unwrap_or_default()
}

/// Why an override was refused.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum OverlayError {
    /// No such parameter.
    #[error("unknown parameter {key:?}{}", did_you_mean(.suggestion))]
    UnknownKey {
        /// The key as given.
        key: String,
        /// The closest existing key, if any is close.
        suggestion: Option<String>,
    },
    /// The value has the wrong type for the key.
    #[error("{key} expects {expected}")]
    WrongType {
        /// The key.
        key: String,
        /// What the key takes, e.g. `a number`.
        expected: &'static str,
    },
    /// Not finite, or outside the range of the parameter's type (`f32` /
    /// `i32`).
    #[error("{key}: the value cannot be represented")]
    NotRepresentable {
        /// The key.
        key: String,
    },
    /// The resulting set fails [`PlayerParams::validate`] (carries the
    /// Classic requirement text).
    #[error(transparent)]
    Invalid(#[from] ParamError),
    /// More than [`MAX_OVERRIDES`] entries.
    #[error("too many overrides (at most {limit})")]
    TooMany {
        /// The limit.
        limit: usize,
    },
    /// The parameter set did not survive the round trip (internal).
    #[error("internal: {0}")]
    Shape(String),
}

/// What a parameter set is, by comparison with the two known sets.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ParamSetLabel {
    /// Exactly [`PlayerParams::asamu_original`].
    Classic,
    /// Exactly [`PlayerParams::placeholder`].
    Placeholder,
    /// Anything else; never to be labelled as the original's values.
    Modified {
        /// Number of keys that differ from the Classic set.
        overrides: usize,
    },
}

/// Labels a parameter set by **equality** with the Classic and placeholder
/// sets (never by whether the script layer is present).
#[must_use]
pub fn param_set_label(params: &PlayerParams) -> ParamSetLabel {
    if *params == PlayerParams::asamu_original() {
        ParamSetLabel::Classic
    } else if *params == PlayerParams::placeholder() {
        ParamSetLabel::Placeholder
    } else {
        ParamSetLabel::Modified {
            overrides: overridden_keys(params).len(),
        }
    }
}

/// The keys whose value or provenance differs from the Classic set (or that
/// `params` does not have at all), in report order.
#[must_use]
pub fn overridden_keys(params: &PlayerParams) -> Vec<String> {
    let report = params.provenance_report();
    let ours: BTreeMap<&str, _> = report.iter().map(|e| (e.name.as_str(), e)).collect();
    PlayerParams::asamu_original()
        .provenance_report()
        .into_iter()
        .filter(|classic| {
            // The report prints each float in its shortest exact form, so
            // equal text means equal bits.
            ours.get(classic.name.as_str())
                .is_none_or(|e| e.value != classic.value || e.provenance != classic.provenance)
        })
        .map(|classic| classic.name)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The serialized `value` of every leaf, floats as exact bits.
    fn value_bits(params: &PlayerParams) -> BTreeMap<String, String> {
        let root = serde_json::to_value(params).unwrap();
        params
            .provenance_report()
            .into_iter()
            .map(|e| {
                let value = &crate::keys::leaf(&root, &e.name).unwrap()["value"];
                let bits = match value {
                    Value::Number(n) if n.is_f64() => {
                        format!("f32:{:08x}", (n.as_f64().unwrap() as f32).to_bits())
                    }
                    other => other.to_string(),
                };
                (e.name, bits)
            })
            .collect()
    }

    #[test]
    fn overriding_a_key_with_its_own_value_changes_no_bit() {
        let classic = PlayerParams::asamu_original();
        let classic_bits = value_bits(&classic);
        let classic_report = classic.provenance_report();
        let catalog = Catalog::shared();
        assert_eq!(catalog.len(), classic_report.len());
        for info in catalog.iter() {
            // `set` would drop an entry equal to Classic, so the entry is
            // put in directly: this is the serialization round trip of the
            // whole set with exactly one leaf rewritten.
            let overlay = Overlay(BTreeMap::from([(info.key.clone(), info.classic.clone())]));
            let params = overlay
                .apply()
                .unwrap_or_else(|e| panic!("{}: {e}", info.key));

            // Every value, the overridden one included, keeps its bits.
            assert_eq!(value_bits(&params), classic_bits, "{}", info.key);
            // Typed spot checks on the same result (not through JSON).
            assert_eq!(
                params.movement.custom_gravity_scaling.value.to_bits(),
                classic.movement.custom_gravity_scaling.value.to_bits()
            );
            assert_eq!(
                params.camera.max_pitch_degrees.value.to_bits(),
                classic.camera.max_pitch_degrees.value.to_bits()
            );
            assert_eq!(
                params.grapple.rope_mode.value,
                classic.grapple.rope_mode.value
            );
            assert_eq!(
                params.gun.as_ref().map(|g| g.initial_max_grapples.value),
                classic.gun.as_ref().map(|g| g.initial_max_grapples.value)
            );

            // Only that key's provenance differs, and it says "override".
            let report = params.provenance_report();
            assert_eq!(report.len(), classic_report.len());
            for (entry, original) in report.iter().zip(&classic_report) {
                assert_eq!(entry.name, original.name);
                assert_eq!(entry.value, original.value, "{}", entry.name);
                if entry.name == info.key {
                    match &entry.provenance {
                        Provenance::Placeholder { note } => {
                            assert!(note.starts_with(OVERRIDE_NOTE_PREFIX), "{note}");
                        }
                        other => panic!("{}: {other}", entry.name),
                    }
                } else {
                    assert_eq!(entry.provenance, original.provenance, "{}", entry.name);
                }
            }
            assert_eq!(overridden_keys(&params), vec![info.key.clone()]);
            assert_eq!(
                param_set_label(&params),
                ParamSetLabel::Modified { overrides: 1 }
            );
        }
    }

    #[test]
    fn every_key_overridden_at_once_still_changes_no_bit() {
        let classic = PlayerParams::asamu_original();
        let all: BTreeMap<String, TuneValue> = Catalog::shared()
            .iter()
            .map(|info| (info.key.clone(), info.classic.clone()))
            .collect();
        let count = all.len();
        let params = Overlay(all).apply().unwrap();
        assert_eq!(value_bits(&params), value_bits(&classic));
        assert_eq!(overridden_keys(&params).len(), count);
        assert_eq!(params.placeholder_names().len(), count);
        assert_eq!(
            param_set_label(&params),
            ParamSetLabel::Modified { overrides: count }
        );
    }

    #[test]
    fn nudged_floats_land_on_round_decimals_and_return_to_classic() {
        // Arithmetic only; the numbers are not parameter values.
        let classic = f64::from(0.3_f32);
        let up = nudged_float(classic, classic, 0.02, 1.0, 1.0);
        assert_eq!(up, 0.32);
        let down = nudged_float(up, classic, 0.02, -1.0, 1.0);
        assert_eq!(down.to_bits(), classic.to_bits(), "exactly Classic again");
        assert_eq!(nudged_float(0.32, classic, 0.02, 1.0, 0.1), 0.322);
        assert_eq!(nudged_float(0.32, classic, 0.02, 3.0, 10.0), 0.92);
        // A Classic value off the grid: up lands on the grid, down snaps home.
        let odd = 98.876_953_125;
        let up = nudged_float(odd, odd, 5.0, 1.0, 1.0);
        assert_eq!(up, 103.875);
        assert_eq!(nudged_float(up, odd, 5.0, -1.0, 1.0), odd);
        // Zero steps is the value itself (on the grid).
        assert_eq!(nudged_float(0.32, classic, 0.02, 0.0, 1.0), 0.32);
        // Nothing sensible to do with a broken step.
        assert!(nudged_float(1.0, 1.0, f64::NAN, 1.0, 1.0).is_nan());
        assert_eq!(decimals_of(0.00002), 5);
        assert_eq!(decimals_of(0.005), 3);
        assert_eq!(decimals_of(20.0), 0);
    }

    #[test]
    fn labels_compare_whole_sets() {
        assert_eq!(
            param_set_label(&PlayerParams::asamu_original()),
            ParamSetLabel::Classic
        );
        assert_eq!(
            param_set_label(&PlayerParams::placeholder()),
            ParamSetLabel::Placeholder
        );
        assert!(overridden_keys(&PlayerParams::asamu_original()).is_empty());
        // Same values, different provenance: not Classic.
        let mut relabelled = PlayerParams::asamu_original();
        let value = relabelled.movement.jump_velocity.value;
        relabelled.movement.jump_velocity.set(
            value,
            Provenance::placeholder("same number, not from the original"),
        );
        assert_eq!(
            param_set_label(&relabelled),
            ParamSetLabel::Modified { overrides: 1 }
        );
        assert_eq!(
            overridden_keys(&relabelled),
            vec!["movement.jump_velocity".to_owned()]
        );
        // The script layer being present says nothing: a set with the layer
        // and one changed value is modified, not Classic.
        let mut tuned = PlayerParams::asamu_original();
        tuned.movement.custom_gravity_scaling.value *= 0.5;
        assert!(tuned.pawn.is_some());
        assert_eq!(
            param_set_label(&tuned),
            ParamSetLabel::Modified { overrides: 1 }
        );
    }
}
