//! The parameter overlay: what it accepts, what it refuses and why, and
//! what an accepted override looks like in the resulting parameter set.
//!
//! The Classic values used here are read from the catalogue at run time;
//! the test values are ours.

use std::collections::BTreeMap;

use asamu_core::Provenance;
use asamu_player::params::ParamError;
use asamu_player::{PlayerParams, RopeMode};
use asamu_sandbox::keys::{Catalog, TuneValue, ValueKind};
use asamu_sandbox::overlay::{
    MAX_OVERRIDES, OVERRIDE_NOTE_PREFIX, Overlay, OverlayError, ParamSetLabel, overridden_keys,
    param_set_label,
};

fn classic_float(key: &str) -> f64 {
    match Catalog::shared().get(key).map(|info| &info.classic) {
        Some(TuneValue::Float(v)) => *v,
        other => panic!("{key} is not a float key: {other:?}"),
    }
}

fn step(key: &str) -> f64 {
    Catalog::shared()
        .get(key)
        .unwrap_or_else(|| panic!("{key} is a key"))
        .step
}

/// The requirement text Classic validation reports for `edit`.
fn classic_requirement(edit: impl FnOnce(&mut PlayerParams)) -> (&'static str, &'static str) {
    let mut params = PlayerParams::asamu_original();
    edit(&mut params);
    match params.validate() {
        Err(ParamError::OutOfRange {
            name, requirement, ..
        }) => (name, requirement),
        Ok(()) => panic!("the edit was expected to be invalid"),
    }
}

#[test]
fn an_unknown_key_is_refused_with_a_suggestion() {
    let mut overlay = Overlay::default();
    let cases = [
        ("movement.jump_velocty", Some("movement.jump_velocity")),
        ("jump_velocity", Some("movement.jump_velocity")),
        ("pawn.jump_velocity", Some("movement.jump_velocity")),
        ("Movement.Jump_Velocity", Some("movement.jump_velocity")),
        ("gun.max_distanse", Some("gun.max_distance")),
        ("boots.boost_strenght", Some("boots.boost_strength")),
        ("completely.unrelated_thing", None),
        ("", None),
        (
            "movement.jump_velocity.value",
            Some("movement.jump_velocity"),
        ),
    ];
    for (key, suggestion) in cases {
        let error = overlay.set(key, TuneValue::Float(1.0)).unwrap_err();
        assert_eq!(
            error,
            OverlayError::UnknownKey {
                key: key.to_owned(),
                suggestion: suggestion.map(str::to_owned),
            },
            "{key:?}"
        );
        let text = error.to_string();
        assert_eq!(
            text.contains("did you mean"),
            suggestion.is_some(),
            "{text}"
        );
        assert!(overlay.is_empty());
        // The same refusal from every entry point.
        assert!(matches!(
            overlay.nudge(key, 1, 1.0),
            Err(OverlayError::UnknownKey { .. })
        ));
    }
    // A provenance or value path is not a key: only leaves are addressable,
    // so no override can reach a group or a provenance.
    for not_a_key in [
        "pawn",
        "gun",
        "boots",
        "movement",
        "movement.gravity_z.provenance",
    ] {
        assert!(matches!(
            overlay.set(not_a_key, TuneValue::Bool(false)),
            Err(OverlayError::UnknownKey { .. })
        ));
    }
    // A hostile, very long key is refused without a suggestion search.
    let long = "x".repeat(100_000);
    assert_eq!(
        overlay.set(&long, TuneValue::Int(1)),
        Err(OverlayError::UnknownKey {
            key: long.clone(),
            suggestion: None
        })
    );
}

#[test]
fn a_value_of_the_wrong_type_is_refused() {
    let mut overlay = Overlay::default();
    let wrong = [
        ("movement.jump_velocity", TuneValue::Bool(true)),
        ("movement.jump_velocity", TuneValue::Text("high".to_owned())),
        ("movement.limit_fall_accel", TuneValue::Int(1)),
        ("movement.limit_fall_accel", TuneValue::Float(1.0)),
        (
            "movement.limit_fall_accel",
            TuneValue::Text("true".to_owned()),
        ),
        ("gun.initial_max_grapples", TuneValue::Float(2.0)),
        ("gun.initial_max_grapples", TuneValue::Bool(true)),
        ("grapple.rope_mode", TuneValue::Int(1)),
        ("grapple.rope_mode", TuneValue::Text("elastic".to_owned())),
        ("grapple.rope_mode", TuneValue::Text("Inelastic".to_owned())),
        ("grapple.release_mode", TuneValue::Text(String::new())),
    ];
    for (key, value) in wrong {
        match overlay.set(key, value.clone()) {
            Err(OverlayError::WrongType { key: k, expected }) => {
                assert_eq!(k, key);
                assert!(!expected.is_empty());
            }
            other => panic!("{key} = {value:?}: {other:?}"),
        }
        assert!(overlay.is_empty());
    }
    // An integer is a number: accepted for a float key and stored as one.
    overlay
        .set("movement.jump_velocity", TuneValue::Int(1234))
        .unwrap();
    assert_eq!(
        overlay.get("movement.jump_velocity"),
        Some(&TuneValue::Float(1234.0))
    );
    assert_eq!(
        overlay.apply().unwrap().movement.jump_velocity.value,
        1234.0
    );
}

#[test]
fn values_that_cannot_be_represented_are_refused() {
    let mut overlay = Overlay::default();
    let key = "movement.jump_velocity";
    for value in [
        f64::NAN,
        f64::INFINITY,
        f64::NEG_INFINITY,
        f64::MAX,
        f64::from(f32::MAX) * 2.0,
    ] {
        assert_eq!(
            overlay.set(key, TuneValue::Float(value)),
            Err(OverlayError::NotRepresentable {
                key: key.to_owned()
            }),
            "{value}"
        );
        assert!(overlay.is_empty());
    }
    // An `i32` parameter does not take what an `i32` cannot hold.
    for value in [i64::from(i32::MAX) + 1, i64::from(i32::MIN) - 1, i64::MAX] {
        assert_eq!(
            overlay.set("gun.initial_max_grapples", TuneValue::Int(value)),
            Err(OverlayError::NotRepresentable {
                key: "gun.initial_max_grapples".to_owned()
            }),
            "{value}"
        );
    }
    overlay
        .set(
            "gun.initial_max_grapples",
            TuneValue::Int(i64::from(i32::MAX)),
        )
        .unwrap();
    // A nudge scale must be a positive number.
    for scale in [0.0, -1.0, f64::NAN, f64::INFINITY] {
        assert!(matches!(
            overlay.nudge(key, 1, scale),
            Err(OverlayError::NotRepresentable { .. })
        ));
    }
}

#[test]
fn an_out_of_range_value_carries_the_classic_requirement_and_changes_nothing() {
    let mut overlay = Overlay::default();
    overlay
        .set("movement.ground_acceleration", TuneValue::Float(1234.5))
        .unwrap();
    let before = overlay.clone();

    // Single-value rules: the refusal is Classic validation's own error.
    let (name, requirement) = classic_requirement(|p| p.movement.air_control.value = 1.5);
    match overlay.set("movement.air_control", TuneValue::Float(1.5)) {
        Err(OverlayError::Invalid(ParamError::OutOfRange {
            name: n,
            value,
            requirement: r,
        })) => {
            assert_eq!((n, r), (name, requirement));
            assert_eq!(n, "movement.air_control");
            assert_eq!(value, 1.5);
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(
        overlay, before,
        "a refused set leaves the overlay as it was"
    );

    let error = overlay
        .set("movement.capsule_radius", TuneValue::Float(-3.0))
        .unwrap_err();
    assert!(
        error.to_string().contains("movement.capsule_radius"),
        "{error}"
    );
    assert!(matches!(error, OverlayError::Invalid(_)));
    assert_eq!(overlay, before);

    assert!(matches!(
        overlay.set("pawn.zoom_duration", TuneValue::Float(0.0)),
        Err(OverlayError::Invalid(_))
    ));
    assert!(matches!(
        overlay.set("gun.fire_interval", TuneValue::Int(0)),
        Err(OverlayError::Invalid(_))
    ));
    assert!(matches!(
        overlay.set("boots.boost_duration", TuneValue::Float(-1.0)),
        Err(OverlayError::Invalid(_))
    ));
    assert_eq!(overlay, before);

    // A rule across two values: the step height must stay below the full
    // collision height. The whole resulting set is what is validated.
    let half_height = classic_float("movement.capsule_half_height");
    let too_high = half_height * 2.0 + 1.0;
    let (name, requirement) =
        classic_requirement(|p| p.movement.step_height.value = too_high as f32);
    match overlay.set("movement.step_height", TuneValue::Float(too_high)) {
        Err(OverlayError::Invalid(ParamError::OutOfRange {
            name: n,
            requirement: r,
            ..
        })) => assert_eq!((n, r), (name, requirement)),
        other => panic!("{other:?}"),
    }
    assert_eq!(overlay, before);
    // With a taller cylinder first, the same step height is fine ...
    overlay
        .set(
            "movement.capsule_half_height",
            TuneValue::Float(half_height * 2.0),
        )
        .unwrap();
    overlay
        .set("movement.step_height", TuneValue::Float(too_high))
        .unwrap();
    // ... and then the cylinder cannot shrink back under it.
    let tall = overlay.clone();
    assert!(matches!(
        overlay.set(
            "movement.capsule_half_height",
            TuneValue::Float(half_height)
        ),
        Err(OverlayError::Invalid(_))
    ));
    assert!(!overlay.clear("movement.nothing"));
    assert_eq!(overlay, tall);

    // A nudge that would leave the range is refused the same way.
    let mut overlay = Overlay::default();
    let floor_z = classic_float("movement.walkable_floor_z");
    let steps_past_one = ((1.0 - floor_z) / step("movement.walkable_floor_z")).ceil() as i32 + 1;
    assert!(matches!(
        overlay.nudge("movement.walkable_floor_z", steps_past_one, 1.0),
        Err(OverlayError::Invalid(_))
    ));
    assert!(overlay.is_empty());
}

#[test]
fn override_notes_start_with_the_prefix_and_name_the_classic_source() {
    let classic = PlayerParams::asamu_original();
    let mut overlay = Overlay::default();
    overlay
        .set("movement.custom_gravity_scaling", TuneValue::Float(0.5))
        .unwrap();
    overlay
        .set("pawn.zoom_enabled", TuneValue::Bool(false))
        .unwrap();
    overlay
        .set("gun.initial_max_grapples", TuneValue::Int(2))
        .unwrap();
    overlay
        .set(
            "grapple.rope_mode",
            TuneValue::Text("shorten_to_distance".to_owned()),
        )
        .unwrap();
    assert_eq!(overlay.len(), 4);
    let keys: Vec<&str> = overlay.iter().map(|(key, _)| key).collect();
    assert_eq!(
        keys,
        [
            "grapple.rope_mode",
            "gun.initial_max_grapples",
            "movement.custom_gravity_scaling",
            "pawn.zoom_enabled"
        ],
        "key order"
    );

    let tuned = overlay.apply().unwrap();
    // The values are in the set, typed.
    assert_eq!(tuned.movement.custom_gravity_scaling.value, 0.5);
    assert_eq!(
        tuned.pawn.as_ref().map(|p| p.zoom_enabled.value),
        Some(false)
    );
    assert_eq!(
        tuned.gun.as_ref().map(|g| g.initial_max_grapples.value),
        Some(2)
    );
    assert_eq!(tuned.grapple.rope_mode.value, RopeMode::ShortenToDistance);
    assert_eq!(tuned.validate(), Ok(()));

    // Each override is a placeholder whose note starts with the prefix and
    // says what the Classic value was and where it comes from; everything
    // else keeps its Classic value and provenance.
    let classic_report: BTreeMap<String, _> = classic
        .provenance_report()
        .into_iter()
        .map(|e| (e.name.clone(), e))
        .collect();
    for entry in tuned.provenance_report() {
        let original = &classic_report[&entry.name];
        if overlay.get(&entry.name).is_some() {
            let Provenance::Placeholder { note } = &entry.provenance else {
                panic!("{}: {}", entry.name, entry.provenance);
            };
            assert!(note.starts_with(OVERRIDE_NOTE_PREFIX), "{note}");
            assert!(note.contains("not the original's value"), "{note}");
            assert!(
                note.contains(&format!("Classic: {} from", original.value)),
                "{}: {note}",
                entry.name
            );
            assert!(
                note.ends_with(&original.provenance.to_string()),
                "{}: {note}",
                entry.name
            );
            assert_ne!(entry.value, original.value, "{}", entry.name);
        } else {
            assert_eq!(&entry, original, "{}", entry.name);
        }
    }
    assert_eq!(
        param_set_label(&tuned),
        ParamSetLabel::Modified { overrides: 4 }
    );
    assert_eq!(overridden_keys(&tuned).len(), 4);
    // The note depends on the key only: another value, the same note.
    let mut other = Overlay::default();
    other
        .set("movement.custom_gravity_scaling", TuneValue::Float(0.25))
        .unwrap();
    assert_eq!(
        other
            .apply()
            .unwrap()
            .movement
            .custom_gravity_scaling
            .provenance,
        tuned.movement.custom_gravity_scaling.provenance
    );
}

#[test]
fn too_many_overrides_are_refused() {
    let entries = |count: usize| -> Overlay {
        let map: BTreeMap<String, TuneValue> = (0..count)
            .map(|i| (format!("extra.key_{i:04}"), TuneValue::Int(i as i64)))
            .collect();
        serde_json::from_value(serde_json::to_value(map).unwrap()).unwrap()
    };
    let too_many = entries(MAX_OVERRIDES + 1);
    assert_eq!(too_many.len(), MAX_OVERRIDES + 1);
    let limit = OverlayError::TooMany {
        limit: MAX_OVERRIDES,
    };
    assert_eq!(too_many.apply(), Err(limit.clone()));
    assert_eq!(too_many.normalized(), Err(limit.clone()));
    let mut editing = too_many.clone();
    assert_eq!(
        editing.set("movement.jump_velocity", TuneValue::Float(1.0)),
        Err(limit)
    );
    assert_eq!(editing, too_many);
    // At the limit the entries themselves are looked at (and refused for
    // what they are).
    assert!(matches!(
        entries(MAX_OVERRIDES).apply(),
        Err(OverlayError::UnknownKey { .. })
    ));
    // The catalogue is far smaller than the limit: every key at once fits.
    assert!(Catalog::shared().len() < MAX_OVERRIDES);
}

#[test]
fn floats_are_judged_as_the_f32_the_set_stores() {
    let key = "movement.air_control";
    let classic = classic_float(key);
    let mut overlay = Overlay::default();
    // A different decimal that narrows to the same `f32` is the Classic
    // value: no entry.
    overlay
        .set(key, TuneValue::Float(classic + 1.0e-12))
        .unwrap();
    assert!(overlay.is_empty());
    overlay
        .set(key, TuneValue::Float(f64::from(classic as f32)))
        .unwrap();
    assert!(overlay.is_empty());
    // The value is kept as given and applied as its nearest `f32`.
    let wanted = classic + 0.123_456_789_012;
    overlay.set(key, TuneValue::Float(wanted)).unwrap();
    assert_eq!(overlay.get(key), Some(&TuneValue::Float(wanted)));
    assert_eq!(
        overlay
            .apply()
            .unwrap()
            .movement
            .air_control
            .value
            .to_bits(),
        (wanted as f32).to_bits()
    );
}

#[test]
fn nudges_step_through_values_by_kind() {
    let mut overlay = Overlay::default();

    // Floats: the catalogue step times the scale, on round decimals.
    let key = "movement.jump_velocity";
    let (classic, size) = (classic_float(key), step(key));
    let close = |result: Result<TuneValue, OverlayError>, steps: f64| {
        let expected = classic + steps * size;
        match result {
            Ok(TuneValue::Float(v)) => assert!(
                (v - expected).abs() <= 1.0e-9 * expected.abs().max(1.0),
                "{v} instead of {expected}"
            ),
            other => panic!("{other:?}"),
        }
    };
    close(overlay.nudge(key, 1, 1.0), 1.0);
    close(overlay.nudge(key, 2, 10.0), 21.0);
    close(overlay.nudge(key, -1, 0.1), 20.9);
    close(overlay.nudge(key, 0, 1.0), 20.9);
    assert_eq!(overlay.len(), 1);
    assert_eq!(overlay.nudge(key, -209, 0.1), Ok(TuneValue::Float(classic)));
    assert!(overlay.is_empty(), "back on Classic: the entry is gone");

    // Flags toggle on odd step counts.
    let key = "pawn.zoom_enabled";
    let Some(TuneValue::Bool(on)) = Catalog::shared().get(key).map(|i| i.classic.clone()) else {
        panic!("{key} is a flag");
    };
    assert_eq!(overlay.nudge(key, 1, 1.0), Ok(TuneValue::Bool(!on)));
    assert_eq!(overlay.nudge(key, 2, 1.0), Ok(TuneValue::Bool(!on)));
    assert_eq!(overlay.nudge(key, -3, 5.0), Ok(TuneValue::Bool(on)));
    assert!(overlay.is_empty());

    // Whole numbers move by at least one.
    let key = "gun.initial_max_grapples";
    let Some(TuneValue::Int(count)) = Catalog::shared().get(key).map(|i| i.classic.clone()) else {
        panic!("{key} is a whole number");
    };
    assert_eq!(overlay.nudge(key, 2, 1.0), Ok(TuneValue::Int(count + 2)));
    assert_eq!(overlay.nudge(key, 1, 0.1), Ok(TuneValue::Int(count + 3)));
    assert_eq!(overlay.nudge(key, -1, 3.0), Ok(TuneValue::Int(count)));
    assert!(overlay.is_empty());

    // Choices cycle through their names and wrap.
    let key = "grapple.rope_mode";
    let info = Catalog::shared().get(key).unwrap();
    let ValueKind::Choice(names) = info.kind else {
        panic!("{key} is a choice");
    };
    assert!(names.len() >= 2);
    let TuneValue::Text(start) = &info.classic else {
        panic!("{key} holds a name");
    };
    let at = names.iter().position(|n| *n == start.as_str()).unwrap();
    let name = |offset: usize| TuneValue::Text(names[(at + offset) % names.len()].to_owned());
    assert_eq!(overlay.nudge(key, 1, 1.0), Ok(name(1)));
    assert_eq!(
        overlay.nudge(key, names.len() as i32, 1.0),
        Ok(name(1)),
        "a full cycle"
    );
    assert_eq!(overlay.nudge(key, -1, 1.0), Ok(name(0)));
    assert!(overlay.is_empty());
    // A choice with one name has nowhere to go.
    let single = "grapple.release_mode";
    let classic = Catalog::shared().get(single).unwrap().classic.clone();
    assert_eq!(overlay.nudge(single, 1, 1.0), Ok(classic));
    assert!(overlay.is_empty());
}

#[test]
fn an_overlay_read_from_a_file_is_checked_and_normalized() {
    // The file form is a plain map; it round-trips.
    let mut overlay = Overlay::default();
    overlay
        .set("movement.custom_gravity_scaling", TuneValue::Float(0.5))
        .unwrap();
    overlay
        .set("pawn.zoom_enabled", TuneValue::Bool(false))
        .unwrap();
    let json = serde_json::to_string(&overlay).unwrap();
    assert_eq!(
        json,
        r#"{"movement.custom_gravity_scaling":0.5,"pawn.zoom_enabled":false}"#
    );
    assert_eq!(serde_json::from_str::<Overlay>(&json).unwrap(), overlay);

    // Hand-written: an integer for a float key, and an entry that states
    // the Classic value.
    let jump = classic_float("movement.jump_velocity");
    let written: Overlay = serde_json::from_str(&format!(
        r#"{{"movement.ground_friction": 3, "movement.jump_velocity": {jump}}}"#
    ))
    .unwrap();
    assert_eq!(written.len(), 2);
    // Applied as written: both leaves are overrides (the one equal to
    // Classic too: it is pinned, and labelled as not original).
    let applied = written.apply().unwrap();
    assert_eq!(applied.movement.ground_friction.value, 3.0);
    assert_eq!(overridden_keys(&applied).len(), 2);
    // Normalized: what `set` would have stored.
    let normalized = written.normalized().unwrap();
    assert_eq!(normalized.len(), 1);
    assert_eq!(
        normalized.get("movement.ground_friction"),
        Some(&TuneValue::Float(3.0))
    );
    assert_eq!(overridden_keys(&normalized.apply().unwrap()).len(), 1);

    // Entries that do not apply are errors, never skipped.
    for (bad, what) in [
        (r#"{"movement.jump_velocity": "fast"}"#, "wrong type"),
        (r#"{"movement.no_such_thing": 1.0}"#, "unknown key"),
        (r#"{"movement.air_control": 7.5}"#, "out of range"),
        (r#"{"movement.jump_velocity": 1e300}"#, "unrepresentable"),
        (
            r#"{"gun.initial_max_grapples": 2.5}"#,
            "fraction for a whole number",
        ),
    ] {
        let overlay: Overlay = serde_json::from_str(bad).unwrap();
        assert!(overlay.apply().is_err(), "{what}");
        assert!(overlay.normalized().is_err(), "{what}");
    }
    // Things that are not a map of simple values do not even read.
    for not_an_overlay in [
        "[]",
        "3",
        r#"{"movement.jump_velocity": [1]}"#,
        r#"{"movement.jump_velocity": {"value": 1}}"#,
        r#"{"movement.jump_velocity": null}"#,
    ] {
        assert!(serde_json::from_str::<Overlay>(not_an_overlay).is_err());
    }
}
