//! `PlayerParams::asamu_original()` must match the committed gameplay
//! defaults (`docs/reverse-engineering/data/defaults/*.json`, produced by the
//! `gameplay_defaults` example of `asamu-inspect`; see
//! `docs/reverse-engineering/DEFAULTS.md`): every recovered value and its
//! provenance (class whose default object stores it + property, or config
//! file + `[Section] Key`). Fails on drift in either direction.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use asamu_core::Provenance;
use asamu_player::PlayerParams;
use asamu_player::params::rotator_units_to_degrees;
use serde_json::Value;

fn defaults_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/reverse-engineering/data/defaults")
}

fn load(file: &str) -> Value {
    let path = defaults_dir().join(file);
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("{} is readable: {e}", path.display()));
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("{file} parses: {e}"))
}

/// The JSON file (queried class) holding each recovered parameter.
const SOURCES: &[(&str, &str)] = &[
    ("movement.max_ground_speed", "ASAMUPawn.json"),
    ("movement.ground_acceleration", "ASAMUPawn.json"),
    ("movement.air_control", "ASAMUPawn.json"),
    ("movement.jump_velocity", "ASAMUPawn.json"),
    ("movement.capsule_radius", "ASAMUPawn.json"),
    ("movement.capsule_half_height", "ASAMUPawn.json"),
    ("movement.step_height", "ASAMUPawn.json"),
    ("movement.max_fall_speed", "ASAMUPawn.json"),
    ("movement.walkable_floor_z", "ASAMUPawn.json"),
    ("movement.world_gravity_z", "WorldInfo.json"),
    ("movement.custom_gravity_scaling", "ASAMUPawn.json"),
    ("movement.ground_friction", "PhysicsVolume.json"),
    ("movement.terminal_velocity", "ASAMUPawn.json"),
    ("movement.limit_fall_accel", "ASAMUPawn.json"),
    ("movement.slope_boost_friction", "ASAMUPawn.json"),
    ("movement.movement_speed_modifier", "ASAMUPawn.json"),
    ("movement.air_speed", "ASAMUPawn.json"),
    ("movement.fluid_friction", "PhysicsVolume.json"),
    ("camera.fov_degrees", "ASAMUSettingsManager.json"),
    ("camera.eye_height", "ASAMUPawn.json"),
    ("camera.max_pitch_degrees", "ASAMUPawn.json"),
    ("pawn.move_speed", "ASAMUPawn.json"),
    ("pawn.sprint_speed_multiplier", "ASAMUPawn.json"),
    ("pawn.story_speed_multiplier", "ASAMUPawn.json"),
    ("pawn.landed_air_control", "ASAMUPawn.json"),
    ("pawn.hard_landing_threshold", "ASAMUPawn.json"),
    ("pawn.land_sound_threshold", "ASAMUPawn.json"),
    ("pawn.zoom_enabled", "ASAMUPawn.json"),
    ("pawn.zoom_fov", "ASAMUPawn.json"),
    ("pawn.zoom_duration", "ASAMUPawn.json"),
    ("pawn.bob", "ASAMUPawn.json"),
    ("pawn.weapon_bob", "ASAMUPawn.json"),
    ("pawn.bob_rate_story", "ASAMUPawn.json"),
    ("pawn.bob_rate_sprint", "ASAMUPawn.json"),
    ("pawn.bob_rate_walk", "ASAMUPawn.json"),
    ("pawn.custom_time_dilation", "ASAMUPawn.json"),
    ("pawn.power_jump_charge_time", "ASAMUPowerJump.json"),
    ("pawn.power_jump_strength", "ASAMUPowerJump.json"),
    (
        "pawn.power_leap_horizontal_multiplier",
        "ASAMUPowerJump.json",
    ),
    ("pawn.power_leap_vertical_strength", "ASAMUPowerJump.json"),
    ("gun.max_distance", "GrappleGun.json"),
    ("gun.weapon_range", "GrappleGun.json"),
    ("gun.release_distance", "GrappleGun.json"),
    ("gun.grapple_accel", "GrappleGun.json"),
    ("gun.max_speed", "GrappleGun.json"),
    ("gun.fire_interval", "GrappleGun.json"),
    ("gun.instant_release_delay", "GrappleGun.json"),
    ("gun.top_grapple_angle", "GrappleGun.json"),
    ("gun.bottom_grapple_angle", "GrappleGun.json"),
    ("gun.interact_range", "GrappleGun.json"),
    ("gun.initial_max_grapples", "GrappleGun.json"),
    ("gun.initial_can_grapple", "GrappleGun.json"),
    ("boots.boost_delay", "ASAMURocketBoots.json"),
    ("boots.boost_duration", "ASAMURocketBoots.json"),
    ("boots.boost_strength", "ASAMURocketBoots.json"),
    ("boots.boost_spiral_strength", "ASAMURocketBoots.json"),
    ("boots.total_spin_angle", "ASAMURocketBoots.json"),
    ("boots.boost_exhausted_delay", "ASAMURocketBoots.json"),
    ("boots.initial_enabled", "ASAMURocketBoots.json"),
];

fn property<'a>(json: &'a Value, name: &str) -> &'a Value {
    json["properties"]
        .as_array()
        .expect("properties array")
        .iter()
        .find(|p| p["name"] == name)
        .unwrap_or_else(|| panic!("{} has no property {name}", json["class"]))
}

/// A value found in the JSON together with where it came from.
struct Found {
    value: Value,
    provenance: Provenance,
    confidence: String,
}

/// Looks up the value the provenance points to and rebuilds the provenance
/// from the JSON's own source fields.
fn find(json: &Value, wanted: &Provenance) -> Found {
    match wanted {
        Provenance::ScriptDefault { property: name, .. } => {
            if let Some((component, member)) = name.split_once('.') {
                // Component template values (the collision cylinder).
                let template = &json["component_templates"][component];
                let entry = template["values"]
                    .as_array()
                    .expect("template values")
                    .iter()
                    .find(|v| v["name"] == member)
                    .unwrap_or_else(|| panic!("template {component} has no {member}"));
                let from = entry["value_from"].as_str().expect("value_from");
                // `UTGame.Default__UTPawn.CollisionCylinder` → `UTGame.UTPawn`.
                let (package, rest) = from.split_once(".Default__").expect("template path");
                let class = rest.split('.').next().expect("class");
                return Found {
                    value: entry["value"].clone(),
                    provenance: Provenance::ScriptDefault {
                        class: format!("{package}.{class}"),
                        property: name.clone(),
                    },
                    confidence: "CONFIRMED".to_owned(),
                };
            }
            // `Name[i]`: element i of an array property.
            let (base, index) = match name.strip_suffix(']').and_then(|n| n.split_once('[')) {
                Some((base, i)) => (base, Some(i.parse::<usize>().expect("array index"))),
                None => (name.as_str(), None),
            };
            let p = property(json, base);
            let source = p["source"].as_str().expect("source");
            // `zero`: no default object in the chain stores the property, so
            // it has the zero value; the class is the declaring one
            // (DEFAULTS.md §1.1, STRONG).
            let class = match source {
                "cdo" | "inherited" => p["value_from"].as_str().expect("value_from"),
                "zero" => p["declared_in"].as_str().expect("declared_in"),
                other => panic!("{name}: source {other} is not a class default"),
            };
            let value = match index {
                Some(i) => p["value"][i].clone(),
                None => p["value"].clone(),
            };
            Found {
                value,
                provenance: Provenance::ScriptDefault {
                    class: class.to_owned(),
                    property: name.clone(),
                },
                confidence: p["confidence"].as_str().expect("confidence").to_owned(),
            }
        }
        Provenance::Config { key, .. } => {
            let name = key.rsplit(' ').next().expect("key name");
            let p = property(json, name);
            assert_eq!(p["source"], "config", "{name} must come from config");
            let c = &p["config"];
            Found {
                value: p["value"].clone(),
                provenance: Provenance::Config {
                    file: c["file"].as_str().expect("file").to_owned(),
                    key: format!(
                        "[{}] {}",
                        c["section"].as_str().expect("section"),
                        c["key"].as_str().expect("key")
                    ),
                },
                confidence: p["confidence"].as_str().expect("confidence").to_owned(),
            }
        }
        other => panic!("unexpected provenance {other}"),
    }
}

#[test]
fn every_original_param_matches_the_committed_defaults() {
    let params = PlayerParams::asamu_original();
    let report = params.provenance_report();
    let recovered: BTreeSet<&str> = report
        .iter()
        .filter(|e| !e.provenance.is_placeholder())
        .map(|e| e.name.as_str())
        .collect();
    let listed: BTreeSet<&str> = SOURCES.iter().map(|(n, _)| *n).collect();
    assert_eq!(
        recovered, listed,
        "every recovered parameter needs a JSON source in this test"
    );

    for (name, file) in SOURCES {
        let entry = report
            .iter()
            .find(|e| e.name == *name)
            .expect("listed parameter is in the report");
        let json = load(file);
        let found = find(&json, &entry.provenance);
        // Provenance: same class/property or file/key (config sections are
        // case-insensitive in UE3; the JSON lower-cases the `asamu` package).
        match (&entry.provenance, &found.provenance) {
            (
                Provenance::Config { file: f1, key: k1 },
                Provenance::Config { file: f2, key: k2 },
            ) => {
                assert_eq!(f1, f2, "{name}: config file");
                assert!(k1.eq_ignore_ascii_case(k2), "{name}: key {k1} vs {k2}");
            }
            (a, b) => assert_eq!(a, b, "{name}: provenance"),
        }
        assert!(
            found.confidence == "CONFIRMED" || found.confidence == "STRONG",
            "{name}: confidence {}",
            found.confidence
        );
        // Value.
        match &found.value {
            Value::Bool(b) => assert_eq!(entry.value, b.to_string(), "{name}"),
            Value::Number(n) => {
                let mut expected = n.as_f64().expect("number") as f32;
                if *name == "camera.max_pitch_degrees" {
                    expected = rotator_units_to_degrees(expected);
                }
                let actual: f32 = entry.value.parse().expect("numeric value");
                assert_eq!(
                    actual.to_bits(),
                    expected.to_bits(),
                    "{name}: {actual} vs JSON {expected}"
                );
            }
            other => panic!("{name}: unexpected JSON value {other}"),
        }
    }
}

#[test]
fn related_defaults_agree_with_the_model_assumptions() {
    let pawn = load("ASAMUPawn.json");
    // EyeHeight starts equal to BaseEyeHeight (the script layer starts the
    // run-time eye height from camera.eye_height).
    assert_eq!(property(&pawn, "EyeHeight")["value"], 38.0);
    assert_eq!(property(&pawn, "BaseEyeHeight")["value"], 38.0);
    // The pitch limit is symmetric.
    assert_eq!(property(&pawn, "ViewPitchMin")["value"], -18000.0);
    assert_eq!(property(&pawn, "ViewPitchMax")["value"], 18000.0);
    // GroundSpeed's class default equals MoveSpeed (written at pawn start).
    assert_eq!(
        property(&pawn, "GroundSpeed")["value"],
        property(&pawn, "MoveSpeed")["value"]
    );
    // The physics volume's own TerminalVelocity (4000) is overwritten by the
    // pawn's fTerminalVelocity at pawn start.
    let volume = load("PhysicsVolume.json");
    assert_eq!(property(&volume, "TerminalVelocity")["value"], 4000.0);
    assert_eq!(property(&pawn, "fTerminalVelocity")["value"], 10000.0);
    // The world's gravity is the config value; no map overrides it
    // (DEFAULTS.md §7).
    let world = load("WorldInfo.json");
    assert_eq!(property(&world, "DefaultGravityZ")["value"], -520.0);
    // The grapple gun writes fGrappleAccel into AirSpeed (G-PH-4); the
    // pawn's class default AirSpeed is 440 and fGrappleMaxSpeed is recorded
    // but never read.
    let gun = load("GrappleGun.json");
    assert_eq!(property(&gun, "fGrappleAccel")["value"], 2000.0);
    assert_eq!(property(&pawn, "AirSpeed")["value"], 440.0);
    assert_eq!(
        property(&gun, "FireInterval")["value"],
        serde_json::json!([0.1])
    );
    assert_eq!(
        property(&gun, "WeaponFireTypes")["value"],
        serde_json::json!(["EWFT_InstantHit"])
    );
    assert_eq!(property(&gun, "Spread")["value"], serde_json::json!([0.0]));
    assert_eq!(
        property(&gun, "topOnlyGrappleTag")["value"],
        "TopOnlyGrappleAble"
    );
    assert_eq!(
        property(&gun, "bottomOnlyGrappleTag")["value"],
        "BottomOnlyGrappleAble"
    );
    assert_eq!(
        property(&gun, "grappleInteractableTag")["value"],
        "grappleInteractable"
    );
    // The flying drag uses the physics volume's FluidFriction (0.3).
    assert_eq!(property(&volume, "FluidFriction")["value"], 0.3);
    // The attractor's class defaults; every placed instance overrides them
    // (DEFAULTS.md §7; the values asamu-world uses).
    let pad = load("ASAMUTelePad_Attractor.json");
    assert_eq!(property(&pad, "Range")["value"], 500.0);
    assert_eq!(property(&pad, "Strength")["value"], 500.0);
    assert_eq!(property(&pad, "attractDuration")["value"], 10.0);
    assert_eq!(property(&pad, "velocityBaseAmount")["value"], 0.0);
    let placed = &load("map_instances.json")["classes"]["asamu.ASAMUTelePad_Attractor"]["overridden_properties"];
    assert_eq!(placed["Range"]["top_values"][0][0], 1000.0);
    assert_eq!(placed["Strength"]["top_values"][0][0], 200.0);
    assert_eq!(placed["velocityBaseAmount"]["top_values"][0][0], 0.05);
    // Recharge crystals and interactables (asamu-world class defaults).
    let crystal = load("ASAMURechargeCrystal.json");
    assert_eq!(property(&crystal, "RechargeDelay")["value"], 10.0);
    assert_eq!(property(&crystal, "bShouldRecharge")["value"], true);
}

#[test]
fn original_values_spot_check() {
    // The headline numbers, independent of the JSON (ABILITIES.md §1).
    let p = PlayerParams::asamu_original();
    let m = &p.movement;
    assert_eq!(m.max_ground_speed.value, 440.0);
    assert_eq!(m.ground_acceleration.value, 2048.0);
    assert_eq!(m.jump_velocity.value, 1000.0);
    assert_eq!(m.air_control.value, 0.3);
    assert_eq!(m.capsule_radius.value, 21.0);
    assert_eq!(m.capsule_half_height.value, 44.0);
    assert_eq!(m.step_height.value, 26.0);
    assert_eq!(m.walkable_floor_z.value, 0.78);
    assert_eq!(m.ground_friction.value, 8.0);
    assert_eq!(m.terminal_velocity.value, 10_000.0);
    assert!(m.limit_fall_accel.value);
    assert_eq!(m.world_gravity_z.value, -520.0);
    assert_eq!(p.camera.eye_height.value, 38.0);
    assert_eq!(p.camera.fov_degrees.value, 90.0);
    let pawn = p.pawn.as_ref().expect("pawn params");
    assert_eq!(pawn.move_speed.value, 440.0);
    assert_eq!(pawn.sprint_speed_multiplier.value, 2.0);
    assert_eq!(pawn.story_speed_multiplier.value, 0.6);
    assert_eq!(pawn.landed_air_control.value, 0.35);
    let gun = p.gun.as_ref().expect("gun params");
    assert_eq!(gun.max_distance.value, 5000.0);
    assert_eq!(gun.release_distance.value, 200.0);
    assert_eq!(gun.grapple_accel.value, 2000.0);
    assert_eq!(gun.max_speed.value, 10_000.0);
    assert_eq!(gun.weapon_range.value, 16_384.0);
    assert_eq!(gun.fire_interval.value, 0.1);
    assert_eq!(gun.initial_max_grapples.value, 0);
    let boots = p.boots.as_ref().expect("boots params");
    assert_eq!(boots.boost_strength.value, 2500.0);
    assert!(!boots.initial_enabled.value);
    assert_eq!(
        m.max_ground_speed.provenance,
        Provenance::ScriptDefault {
            class: "UTGame.UTPawn".into(),
            property: "GroundSpeed".into()
        }
    );
    assert_eq!(
        m.world_gravity_z.provenance,
        Provenance::Config {
            file: "ASAMU/Config/DefaultGame.ini".into(),
            key: "[Engine.WorldInfo] DefaultGravityZ".into()
        }
    );
}
