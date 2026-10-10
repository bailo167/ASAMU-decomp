//! Hostile and extreme input to the Sandbox: profile files, recordings,
//! the profile store's directory, and parameter values at the edge of what
//! the Classic validation accepts.
//!
//! A profile is a file a user can edit, so everything here treats it as
//! untrusted: nothing may panic, hang, allocate without bound, or leave the
//! Sandbox's own directory. Every value in this file is ours and chosen to be
//! awkward; none is a value of the original and no test asserts what the
//! simulation *does* with such a value, only that the Sandbox survives it.
#![allow(clippy::unwrap_used)]

use std::path::PathBuf;
use std::sync::mpsc;
use std::time::Duration;

use asamu_core::DEFAULT_TICK_RATE_HZ;
use asamu_game::asamu_world::graybox_test_level;
use asamu_game::smoke::{DEFAULT_SEED, InputScript};
use asamu_game::{Game, LevelScript};
use asamu_player::{InputFrame, PlayerParams};
use asamu_sandbox::arena::{GRAPPLE_LAB, MOVEMENT_LAB, build_arena};
use asamu_sandbox::command::{Command, SlotOp, TeleportTarget, TimeOp, Toggle};
use asamu_sandbox::inspect::Inspection;
use asamu_sandbox::keys::{Catalog, TuneValue, ValueKind};
use asamu_sandbox::overlay::{BASE_CLASSIC, MAX_OVERRIDES, Overlay, OverlayError};
use asamu_sandbox::predict::predict;
use asamu_sandbox::profile::{
    MAX_PROFILE_BYTES, PROFILE_FORMAT, Profile, ProfileError, ProfileStore,
};
use asamu_sandbox::recording::{RecordingError, SandboxRecording};
use asamu_sandbox::rules::{GrappleRule, Rules, Switch};
use asamu_sandbox::session::{PLACEMENT_LIMIT, Session, SimCx};
use asamu_world::fixtures::SceneFixture;
use asamu_world::scene::{self, LoadOptions, MemorySource};
use glam::Vec3;
use serde_json::Value;

/// The example profile committed with the crate.
const EXAMPLE: &str = include_str!("../examples/profiles/floaty.json");

/// How long one guarded piece of work may take before it counts as a hang.
/// Generous: the slowest case below takes well under a second.
const WATCHDOG: Duration = Duration::from_secs(60);

/// A small deterministic generator (xorshift64*), so a failure reproduces.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }
}

/// What a profile that parsed must also survive: every later use of it.
fn assert_usable(profile: &Profile, what: &str) {
    assert_eq!(profile.check(), Ok(()), "{what}: check");
    let params = profile.overrides.apply();
    assert!(params.is_ok(), "{what}: apply: {params:?}");
    assert!(profile.overrides.len() <= MAX_OVERRIDES, "{what}");
    let session = Session::new(profile.clone());
    assert!(session.is_ok(), "{what}: session: {:?}", session.err());
    // It is written back as something that reads as itself.
    let json = profile.to_json_pretty().unwrap();
    assert!(
        json.len() <= MAX_PROFILE_BYTES * 2,
        "{what}: grew on writing"
    );
    assert_eq!(
        Profile::from_json_slice(json.as_bytes()).as_ref(),
        Ok(profile),
        "{what}: round trip"
    );
}

/// Runs `work` on its own thread and fails with `what` if it panics or does
/// not come back within [`WATCHDOG`].
fn guarded<T: Send + 'static>(what: &str, work: impl FnOnce() -> T + Send + 'static) -> T {
    let (done, result) = mpsc::channel();
    let worker = std::thread::Builder::new()
        .name(what.to_owned())
        .spawn(move || {
            let _ = done.send(work());
        })
        .unwrap();
    match result.recv_timeout(WATCHDOG) {
        Ok(value) => {
            worker.join().unwrap();
            value
        }
        Err(mpsc::RecvTimeoutError::Timeout) => {
            panic!("{what}: no result within {WATCHDOG:?} (a hang)")
        }
        Err(mpsc::RecvTimeoutError::Disconnected) => panic!("{what}: panicked"),
    }
}

// ---------------------------------------------------------------------------
// Profile files.
// ---------------------------------------------------------------------------

/// Tokens a hostile file might hold where a value belongs.
const SPLICES: [&str; 28] = [
    "null",
    "true",
    "[]",
    "{}",
    "[[]]",
    "{\"a\":{}}",
    "0",
    "-0",
    "-1",
    "1e999",
    "-1e999",
    "1e39",
    "1e-400",
    "9223372036854775807",
    "9223372036854775808",
    "-9223372036854775809",
    "18446744073709551616",
    "0.1e+5",
    "NaN",
    "Infinity",
    "\"\"",
    "\"\\u0000\"",
    "\"\\ud800\"",
    "\"../../etc/passwd\"",
    "\"asamu_original\"",
    "\"movement.gravity_z\"",
    ",",
    ":",
];

#[test]
fn mutated_profiles_never_panic_and_a_read_profile_is_always_usable() {
    let base = EXAMPLE.trim_end().as_bytes();
    let mut rng = Rng(0x5A4D_B0C5_0000_0001);
    let (mut accepted, mut refused) = (0_u32, 0_u32);
    for case in 0..6000 {
        let mut bytes = base.to_vec();
        // One to four edits per case.
        for _ in 0..=rng.below(4) {
            let at = rng.below(bytes.len().max(1));
            match rng.below(6) {
                // Flip a bit.
                0 => {
                    if let Some(byte) = bytes.get_mut(at) {
                        *byte ^= 1 << rng.below(8);
                    }
                }
                // Drop a run of bytes.
                1 => {
                    let end = (at + 1 + rng.below(12)).min(bytes.len());
                    bytes.drain(at..end);
                }
                // Repeat a run of bytes.
                2 => {
                    let end = (at + 1 + rng.below(24)).min(bytes.len());
                    let run = bytes[at..end].to_vec();
                    bytes.splice(at..at, run);
                }
                // Put a hostile token somewhere.
                3 => {
                    let token = SPLICES[rng.below(SPLICES.len())].as_bytes().to_vec();
                    bytes.splice(at..at, token);
                }
                // Replace the value after a colon by a hostile token.
                4 => {
                    let colons: Vec<usize> = bytes
                        .iter()
                        .enumerate()
                        .filter(|(_, b)| **b == b':')
                        .map(|(i, _)| i)
                        .collect();
                    if !colons.is_empty() {
                        let colon = colons[rng.below(colons.len())];
                        let end = bytes[colon..]
                            .iter()
                            .position(|b| matches!(b, b',' | b'}' | b'\n'))
                            .map_or(bytes.len(), |n| colon + n);
                        let token = SPLICES[rng.below(SPLICES.len())].as_bytes().to_vec();
                        bytes.splice(colon + 1..end, token);
                    }
                }
                // A raw byte, valid UTF-8 or not.
                _ => bytes.insert(at, (rng.next() & 0xff) as u8),
            }
        }
        match Profile::from_json_slice(&bytes) {
            Ok(profile) => {
                accepted += 1;
                assert_usable(&profile, &format!("case {case}"));
            }
            Err(error) => {
                refused += 1;
                // The message is a bounded sentence, not an echo of the file.
                assert!(error.to_string().len() < 4096, "case {case}: {error}");
            }
        }
    }
    // The generator does reach both outcomes (it is not all noise).
    assert!(refused > 1000, "{refused} refused");
    assert!(accepted > 10, "{accepted} accepted");
}

#[test]
fn every_field_refuses_or_survives_every_wrong_type() {
    let example: Value = serde_json::from_str(EXAMPLE).unwrap();
    let wrong: Vec<Value> = [
        "null",
        "true",
        "false",
        "0",
        "1",
        "-1",
        "2",
        "1.5",
        "4294967296",
        "1e300",
        "\"\"",
        "\"x\"",
        "\"asamu-sandbox-profile\"",
        "[]",
        "[1]",
        "{}",
        "{\"a\":1}",
        "{\"fixed\":3}",
        "{\"fixed\":-2147483649}",
        "{\"movement.gravity_z\":[]}",
        "{\"movement.gravity_z\":{\"value\":1}}",
        "{\"grapples\":7}",
    ]
    .iter()
    .map(|text| serde_json::from_str(text).unwrap())
    .collect();
    let fields: Vec<String> = example.as_object().unwrap().keys().cloned().collect();
    assert_eq!(fields.len(), 9, "the example sets every field: {fields:?}");
    let mut refused = 0_u32;
    for field in &fields {
        for value in &wrong {
            let mut edited = example.clone();
            edited[field.as_str()] = value.clone();
            let text = serde_json::to_vec(&edited).unwrap();
            match Profile::from_json_slice(&text) {
                Ok(profile) => assert_usable(&profile, &format!("{field} = {value}")),
                Err(_) => refused += 1,
            }
        }
        // The field left out: only the documented optional ones may be.
        let mut without = example.clone();
        without.as_object_mut().unwrap().remove(field);
        let read = Profile::from_json_slice(&serde_json::to_vec(&without).unwrap());
        let optional = matches!(
            field.as_str(),
            "description" | "overrides" | "rules" | "time_scale" | "extensions"
        );
        assert_eq!(read.is_ok(), optional, "{field} left out: {read:?}");
    }
    // Most wrong types are refused outright.
    assert!(refused > 100, "{refused}");

    // Wrong types that must never be read as a profile.
    for (field, value) in [
        ("format", "5"),
        ("format", "null"),
        ("version", "\"1\""),
        ("version", "1.5"),
        ("version", "-1"),
        ("version", "4294967296"),
        ("name", "7"),
        ("name", "[\"floaty\"]"),
        ("base", "null"),
        ("base", "{}"),
        ("overrides", "[]"),
        ("overrides", "\"movement.gravity_z\""),
        ("overrides", "{\"movement.gravity_z\":null}"),
        ("overrides", "{\"movement.gravity_z\":[1]}"),
        ("overrides", "{\"pawn.zoom_enabled\":1}"),
        ("overrides", "{\"pawn.zoom_enabled\":\"true\"}"),
        ("overrides", "{\"gun.initial_max_grapples\":1.5}"),
        ("overrides", "{\"gun.initial_max_grapples\":2147483648}"),
        ("overrides", "{\"grapple.rope_mode\":\"no such mode\"}"),
        ("overrides", "{\"grapple.rope_mode\":3}"),
        ("rules", "[]"),
        ("rules", "{\"grapples\":\"many\"}"),
        ("rules", "{\"grapples\":{\"fixed\":1.5}}"),
        ("rules", "{\"grapples\":{\"fixed\":2147483648}}"),
        ("rules", "{\"rocket_boots\":true}"),
        ("rules", "{\"auto_refill\":\"yes\"}"),
        ("rules", "{\"surprise\":1}"),
        ("time_scale", "\"fast\""),
        ("time_scale", "0"),
        ("time_scale", "-1"),
        ("time_scale", "1e9"),
        ("extensions", "[]"),
        ("extensions", "7"),
    ] {
        let mut edited = example.clone();
        edited[field] = serde_json::from_str(value).unwrap();
        let read = Profile::from_json_slice(&serde_json::to_vec(&edited).unwrap());
        assert!(read.is_err(), "{field} = {value} was read: {read:?}");
    }
}

#[test]
fn a_profile_is_an_object_with_named_fields_not_a_list_in_field_order() {
    // The fields of the example, in declaration order, as a JSON array: the
    // derived reader of a struct would take this.
    let positional = format!(
        r#"["{PROFILE_FORMAT}", 1, "listed", "", "{BASE_CLASSIC}", {{}}, {{}}, null, {{}}]"#
    );
    for file in [
        positional.clone(),
        format!("  \n\t{positional}"),
        format!(r#"["{PROFILE_FORMAT}", 1]"#),
        "[]".to_owned(),
    ] {
        let read = Profile::from_json_slice(file.as_bytes());
        assert!(read.is_err(), "{file}: {read:?}");
    }
    // The same for the rules inside a profile.
    let with_rules = |rules: &str| {
        let text = format!(
            r#"{{"format":"{PROFILE_FORMAT}","version":1,"name":"r","base":"{BASE_CLASSIC}",
                "rules":{rules}}}"#
        );
        Profile::from_json_slice(text.as_bytes())
    };
    for listed in ["[]", r#"["unlimited"]"#, r#"["unlimited", "on", true]"#] {
        let read = with_rules(listed);
        assert!(
            matches!(read, Err(ProfileError::Json(_))),
            "{listed}: {read:?}"
        );
    }
    // Named fields are what the format is.
    let named =
        with_rules(r#"{"grapples": "unlimited", "rocket_boots": "on", "auto_refill": true}"#)
            .unwrap();
    assert_eq!(
        named.rules,
        Rules {
            grapples: GrappleRule::Unlimited,
            rocket_boots: Switch::On,
            auto_refill: true
        }
    );
    assert_eq!(with_rules("{}").unwrap().rules, Rules::default());
    // Leading white space before the object is still JSON.
    let spaced = format!("\r\n\t {}", EXAMPLE);
    assert!(Profile::from_json_slice(spaced.as_bytes()).is_ok());
    // A later version is still named as such, whatever shape its rules have.
    let later = format!(
        r#"{{"format":"{PROFILE_FORMAT}","version":2,"name":"r","base":"{BASE_CLASSIC}","rules":[]}}"#
    );
    assert_eq!(
        Profile::from_json_slice(later.as_bytes()),
        Err(ProfileError::UnsupportedVersion(2))
    );
}

#[test]
fn non_finite_and_out_of_range_numbers_are_refused() {
    let with = |overrides: &str| {
        let text = format!(
            r#"{{"format":"{PROFILE_FORMAT}","version":1,"name":"n","base":"{BASE_CLASSIC}",
                "overrides":{overrides}}}"#
        );
        Profile::from_json_slice(text.as_bytes())
    };
    // Not JSON numbers at all, or beyond what a parameter can hold.
    for bad in [
        r#"{"movement.jump_velocity": NaN}"#,
        r#"{"movement.jump_velocity": Infinity}"#,
        r#"{"movement.jump_velocity": -Infinity}"#,
        r#"{"movement.jump_velocity": 1e999}"#,
        r#"{"movement.jump_velocity": 1e39}"#,
        r#"{"movement.jump_velocity": -1e39}"#,
        r#"{"movement.jump_velocity": 3.5e38}"#,
        r#"{"movement.jump_velocity": 400000000000000000000000000000000000000}"#,
        r#"{"gun.initial_max_grapples": 2147483648}"#,
        r#"{"gun.initial_max_grapples": -2147483649}"#,
        r#"{"gun.initial_max_grapples": 1e3}"#,
    ] {
        let read = with(bad);
        assert!(read.is_err(), "{bad}: {read:?}");
    }
    // The largest values that are representable are read exactly or refused
    // by the Classic validation: either way nothing is silently changed.
    for edge in [
        format!(r#"{{"movement.jump_velocity": {}}}"#, f64::from(f32::MAX)),
        format!(
            r#"{{"movement.jump_velocity": {:e}}}"#,
            f64::from(f32::MIN_POSITIVE)
        ),
        r#"{"movement.jump_velocity": 9223372036854775807}"#.to_owned(),
        r#"{"movement.jump_velocity": 18446744073709551615}"#.to_owned(),
        r#"{"gun.initial_max_grapples": 2147483647}"#.to_owned(),
        r#"{"gun.initial_max_grapples": -2147483648}"#.to_owned(),
    ] {
        if let Ok(profile) = with(&edge) {
            assert_usable(&profile, &edge);
            let params = profile.overrides.apply().unwrap();
            assert!(params.movement.jump_velocity.value.is_finite(), "{edge}");
        }
    }
}

#[test]
fn deep_nesting_is_an_error_wherever_it_sits() {
    // As deep as fits under the size limit.
    let depth = MAX_PROFILE_BYTES / 2 - 512;
    let arrays = format!("{}{}", "[".repeat(depth), "]".repeat(depth));
    let objects = format!("{}1{}", "{\"a\":".repeat(depth / 5), "}".repeat(depth / 5));
    let head =
        format!(r#""format":"{PROFILE_FORMAT}","version":1,"name":"deep","base":"{BASE_CLASSIC}""#);
    for nest in [&arrays, &objects] {
        for file in [
            // The whole file.
            nest.clone(),
            // Each place a value can sit.
            format!("{{{head},\"overrides\":{nest}}}"),
            format!("{{{head},\"overrides\":{{\"movement.gravity_z\":{nest}}}}}"),
            format!("{{{head},\"rules\":{nest}}}"),
            format!("{{{head},\"rules\":{{\"grapples\":{nest}}}}}"),
            format!("{{{head},\"time_scale\":{nest}}}"),
            format!("{{{head},\"description\":{nest}}}"),
            format!("{{{head},\"extensions\":{nest}}}"),
            format!("{{{head},\"extensions\":{{\"later\":{nest}}}}}"),
            format!("{{\"format\":{nest}}}"),
            format!("{{\"version\":{nest},\"format\":\"{PROFILE_FORMAT}\"}}"),
        ] {
            assert!(file.len() <= MAX_PROFILE_BYTES, "{}", file.len());
            let label = format!(
                "{} bytes starting {:?}",
                file.len(),
                &file[..60.min(file.len())]
            );
            let read = guarded(&label, move || Profile::from_json_slice(file.as_bytes()));
            assert!(read.is_err(), "{label}");
        }
    }
    // A modest nest in the one free-form field is kept as written.
    let modest = format!(
        "{{{head},\"extensions\":{{\"later\":{}{}}}}}",
        "[".repeat(40),
        "]".repeat(40)
    );
    let profile = Profile::from_json_slice(modest.as_bytes()).unwrap();
    assert_usable(&profile, "a modest nest");
    assert!(profile.extensions.contains_key("later"));
}

#[test]
fn huge_profiles_are_bounded() {
    let head =
        format!(r#""format":"{PROFILE_FORMAT}","version":1,"name":"big","base":"{BASE_CLASSIC}""#);
    // More overrides than there are parameters: refused by count, whatever
    // the keys are.
    let many: String = (0..=MAX_OVERRIDES)
        .map(|n| format!("\"no.such_key_{n}\":1"))
        .collect::<Vec<_>>()
        .join(",");
    let read = Profile::from_json_slice(format!("{{{head},\"overrides\":{{{many}}}}}").as_bytes());
    assert_eq!(
        read,
        Err(ProfileError::Overlay(OverlayError::TooMany {
            limit: MAX_OVERRIDES
        }))
    );
    // The same key over and over is one override (the last one wins) or an
    // error, never a pile.
    let key = Catalog::classic()
        .iter()
        .find(|info| info.kind == ValueKind::Bool)
        .unwrap()
        .key
        .clone();
    let repeated: String = (0..5000)
        .map(|n| format!("\"{key}\":{}", n % 2 == 0))
        .collect::<Vec<_>>()
        .join(",");
    let text = format!("{{{head},\"overrides\":{{{repeated}}}}}");
    assert!(text.len() < MAX_PROFILE_BYTES);
    if let Ok(profile) = Profile::from_json_slice(text.as_bytes()) {
        assert!(profile.overrides.len() <= 1);
        assert_usable(&profile, "a repeated key");
    }
    // Long strings in every string field.
    let long = "a".repeat(MAX_PROFILE_BYTES - 1024);
    for file in [
        format!("{{{head},\"description\":\"{long}\"}}"),
        format!("{{{head},\"overrides\":{{\"{long}\":1}}}}"),
        format!("{{{head},\"overrides\":{{\"grapple.rope_mode\":\"{long}\"}}}}"),
        format!("{{{head},\"extensions\":{{\"{long}\":\"{long}\"}}}}"),
        format!(r#"{{"format":"{long}","version":1,"name":"big","base":"{BASE_CLASSIC}"}}"#),
        format!(r#"{{"format":"{PROFILE_FORMAT}","version":1,"name":"big","base":"{long}"}}"#),
    ] {
        let label = format!("{} bytes", file.len());
        let read = guarded(&label, move || Profile::from_json_slice(file.as_bytes()));
        match read {
            // A long description is only text.
            Ok(profile) => assert_eq!(profile.name, "big"),
            // An error does not carry megabytes around.
            Err(error) => assert!(error.to_string().len() <= MAX_PROFILE_BYTES + 1024),
        }
    }
    // One byte over the limit is refused unread, whatever it holds.
    let over = vec![b'['; MAX_PROFILE_BYTES + 1];
    assert!(matches!(
        Profile::from_json_slice(&over),
        Err(ProfileError::TooLarge { .. })
    ));
}

// ---------------------------------------------------------------------------
// The profile store's directory.
// ---------------------------------------------------------------------------

/// A fresh, empty directory under the system's temporary directory, removed
/// again when dropped.
struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "asamu-sandbox-hostile-{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn names_that_would_leave_the_store_are_refused_before_any_file_is_touched() {
    let root = TempDir::new("names");
    let dir = root.0.join("profiles");
    let store = ProfileStore::new(dir.clone());
    // A file a traversal would reach.
    let outside = root.0.join("outside.json");
    std::fs::write(&outside, EXAMPLE).unwrap();
    for name in [
        "../outside",
        "..",
        ".",
        "",
        "a/b",
        "a\\b",
        "/etc/passwd",
        "C:\\x",
        "c:x",
        "outside.json",
        "name\0",
        "name\n",
        " name",
        "NAME",
        "caf\u{e9}",
        "\u{202e}gnp",
        "~",
        "$HOME",
        "%APPDATA%",
        "a b",
        &"a".repeat(41),
    ] {
        assert!(
            matches!(store.load(name), Err(ProfileError::BadName(_))),
            "load {name:?}"
        );
        let mut profile = Profile::classic();
        profile.name = name.to_owned();
        assert!(
            matches!(store.save(&profile), Err(ProfileError::BadName(_))),
            "save {name:?}"
        );
    }
    // Nothing was created, and the file outside is as it was.
    assert!(!dir.exists(), "a refused save created the directory");
    assert_eq!(std::fs::read_to_string(&outside).unwrap(), EXAMPLE);
    let entries: Vec<_> = std::fs::read_dir(&root.0).unwrap().flatten().collect();
    assert_eq!(entries.len(), 1, "{entries:?}");
}

#[cfg(unix)]
#[test]
fn links_in_the_store_are_neither_followed_nor_written_through() {
    use std::os::unix::fs::symlink;

    let root = TempDir::new("links");
    let dir = root.0.join("profiles");
    std::fs::create_dir_all(&dir).unwrap();
    let store = ProfileStore::new(dir.clone());
    // A file outside the store, and a link to it under a profile's name.
    let outside = root.0.join("secret.json");
    std::fs::write(&outside, EXAMPLE).unwrap();
    symlink(&outside, dir.join("linked.json")).unwrap();
    // A link to nowhere, and one to a directory.
    symlink(root.0.join("missing.json"), dir.join("dangling.json")).unwrap();
    symlink(&root.0, dir.join("folder.json")).unwrap();

    // Not listed, not loaded.
    assert!(store.list().is_empty(), "{:?}", store.list());
    for name in ["linked", "dangling", "folder"] {
        assert_eq!(
            store.load(name),
            Err(ProfileError::NotFound(name.to_owned())),
            "{name}"
        );
    }

    // Saving under the link's name replaces the link, not what it points to.
    let mut mine = Profile::classic();
    mine.name = "linked".to_owned();
    mine.description = "ours".to_owned();
    let path = store.save(&mine).unwrap();
    assert_eq!(path, dir.join("linked.json"));
    assert!(
        std::fs::symlink_metadata(&path).unwrap().is_file(),
        "the link was replaced by a regular file"
    );
    assert_eq!(
        std::fs::read_to_string(&outside).unwrap(),
        EXAMPLE,
        "the file outside the store was written through the link"
    );
    assert_eq!(store.load("linked"), Ok(mine));
    assert_eq!(store.list(), ["linked"]);

    // A link lying in wait under the temporary name a save writes first
    // (`.<name>.json.<process id>.tmp`) is removed, not written through.
    let trap = dir.join(format!(".trapped.json.{}.tmp", std::process::id()));
    symlink(&outside, &trap).unwrap();
    let mut trapped = Profile::classic();
    trapped.name = "trapped".to_owned();
    store.save(&trapped).unwrap();
    assert_eq!(
        std::fs::read_to_string(&outside).unwrap(),
        EXAMPLE,
        "the file outside the store was written through the temporary name"
    );
    assert!(std::fs::symlink_metadata(&trap).is_err());
    assert_eq!(store.load("trapped"), Ok(trapped));
    // No temporary file is left behind.
    let names: Vec<String> = std::fs::read_dir(&dir)
        .unwrap()
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();
    assert!(
        names.iter().all(|name| !name.ends_with(".tmp")),
        "{names:?}"
    );
}

#[test]
fn a_crowded_or_strange_store_directory_is_listed_within_bounds() {
    let root = TempDir::new("crowd");
    let dir = root.0.join("profiles");
    std::fs::create_dir_all(&dir).unwrap();
    for n in 0..700 {
        std::fs::write(dir.join(format!("p{n}.json")), b"").unwrap();
    }
    for odd in [
        "UPPER.json",
        "has space.json",
        ".hidden.json",
        "x.json.tmp",
        "noext",
    ] {
        std::fs::write(dir.join(odd), b"{}").unwrap();
    }
    let store = ProfileStore::new(dir);
    let listed = store.list();
    // Bounded, sorted, and only names a profile can have.
    assert!(listed.len() <= 512, "{}", listed.len());
    assert!(listed.len() >= 500, "{}", listed.len());
    assert!(listed.windows(2).all(|pair| pair[0] < pair[1]));
    assert!(
        listed
            .iter()
            .all(|name| name.starts_with('p') && name.is_ascii())
    );
    // An empty file of a listed name is an error, not a panic.
    assert!(matches!(store.load("p0"), Err(ProfileError::Json(_))));
    // A directory that is not there, or is a file, lists nothing.
    assert!(ProfileStore::new(root.0.join("absent")).list().is_empty());
    let file = root.0.join("a-file");
    std::fs::write(&file, b"x").unwrap();
    let on_a_file = ProfileStore::new(file);
    assert!(on_a_file.list().is_empty());
    assert!(on_a_file.load("p0").is_err());
    assert!(on_a_file.save(&Profile::classic()).is_err());
}

// ---------------------------------------------------------------------------
// Recordings.
// ---------------------------------------------------------------------------

/// A short recording of a tuned session with a few logged commands.
fn a_recording() -> SandboxRecording {
    let mut session = Session::classic();
    let mut game = session.new_game(graybox_test_level()).unwrap();
    game.start();
    let mut script: Option<LevelScript> = None;
    let mut run = |session: &mut Session, game: &mut Game, cmd: Command| {
        session
            .execute(
                cmd,
                &mut SimCx {
                    game,
                    script: &mut script,
                },
            )
            .unwrap()
    };
    run(&mut session, &mut game, Command::Record { on: Toggle::On });
    run(
        &mut session,
        &mut game,
        Command::SetParam {
            key: "movement.custom_gravity_scaling".to_owned(),
            value: TuneValue::Float(0.5),
        },
    );
    run(
        &mut session,
        &mut game,
        Command::Teleport {
            to: TeleportTarget::Start,
        },
    );
    for _ in 0..12 {
        game.tick(&InputFrame::default()).unwrap();
    }
    run(&mut session, &mut game, Command::Record { on: Toggle::Off })
        .recording
        .unwrap()
}

#[test]
fn mutated_recordings_never_panic_and_never_read_as_a_parity_trace() {
    let recording = a_recording();
    let mut text = Vec::new();
    recording.write_jsonl(&mut text).unwrap();
    assert_eq!(
        SandboxRecording::read_jsonl(text.as_slice()).as_ref(),
        Ok(&recording)
    );
    // Every truncation: an error, or (cut between two sample lines) a
    // shorter recording that is still tagged.
    for cut in 0..text.len() {
        match SandboxRecording::read_jsonl(&text[..cut]) {
            Ok(read) => {
                assert!(read.header.not_parity, "cut at {cut}");
                assert!(read.trace.samples.len() <= recording.trace.samples.len());
            }
            Err(RecordingError::NotARecording(_) | RecordingError::Trace(_)) => {}
            Err(other) => panic!("cut at {cut}: {other}"),
        }
    }
    let mut rng = Rng(0x0BAD_5EED_0000_0003);
    for case in 0..3000 {
        let mut bytes = text.clone();
        for _ in 0..=rng.below(3) {
            if bytes.is_empty() {
                break;
            }
            let at = rng.below(bytes.len());
            match rng.below(4) {
                0 => bytes[at] ^= 1 << rng.below(8),
                1 => {
                    let end = (at + 1 + rng.below(40)).min(bytes.len());
                    bytes.drain(at..end);
                }
                2 => {
                    let token = SPLICES[rng.below(SPLICES.len())].as_bytes().to_vec();
                    bytes.splice(at..at, token);
                }
                _ => bytes.insert(at, (rng.next() & 0xff) as u8),
            }
        }
        if let Ok(read) = SandboxRecording::read_jsonl(bytes.as_slice()) {
            // Whatever survived is still a tagged Sandbox recording, and it
            // can be written again.
            assert!(read.header.not_parity, "case {case}");
            assert!(
                read.trace
                    .meta
                    .level
                    .as_deref()
                    .is_some_and(|level| level.starts_with("sandbox:")),
                "case {case}"
            );
            let name = read.file_name();
            assert!(
                name.is_ascii()
                    && !name.contains(['/', '\\', ':'])
                    && !name.contains("..")
                    && name.len() < 120,
                "case {case}: {name:?}"
            );
        }
        // The parity reader takes none of them for a trace.
        if let Ok(utf8) = std::str::from_utf8(&bytes) {
            let first = utf8.lines().find(|line| !line.trim().is_empty());
            if first.is_some_and(|line| line.contains("asamu-sandbox-recording")) {
                assert!(
                    asamu_player::Trace::from_jsonl_str(utf8).is_err(),
                    "case {case}: read as a parity trace"
                );
            }
        }
    }
    // A header line without an end is cut off at the limit, not swallowed.
    let endless = vec![b'{'; 9 << 20];
    let read = guarded("an endless header line", move || {
        SandboxRecording::read_jsonl(endless.as_slice())
    });
    assert!(matches!(read, Err(RecordingError::NotARecording(_))));
}

#[test]
fn a_recording_is_named_safely_whatever_its_level_is_called() {
    let mut recording = a_recording();
    for level in [
        "../../outside",
        "..\\..\\outside",
        "/etc/passwd",
        "C:\\Windows\\system32",
        "con",
        "a\0b",
        "a\nb",
        "\u{202e}txt.exe",
        "caf\u{e9} \u{4e16}\u{754c}",
        "",
        "....",
        &"x".repeat(10_000),
    ] {
        recording.header.level = level.to_owned();
        let name = recording.file_name();
        assert!(name.starts_with("asamu-sandbox-"), "{level:?}: {name}");
        assert!(name.ends_with(".sbxrec.jsonl"), "{level:?}: {name}");
        assert!(
            name.bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'.'),
            "{level:?}: {name}"
        );
        assert!(!name.contains(".."), "{level:?}: {name}");
        assert!(name.len() <= 100, "{level:?}: {} bytes", name.len());
        // One path component, on every platform.
        assert_eq!(
            std::path::Path::new(&name).components().count(),
            1,
            "{level:?}: {name}"
        );
    }
}

// ---------------------------------------------------------------------------
// Parameter values at the edge of what the validation accepts.
// ---------------------------------------------------------------------------

/// Awkward values for a key: far from Classic in both directions, zero, the
/// other sign, and the ends of the type. Ours; chosen to break things.
fn extremes(kind: ValueKind, classic: &TuneValue) -> Vec<TuneValue> {
    match (kind, classic) {
        (ValueKind::Float, TuneValue::Float(classic)) => {
            let sign = if *classic < 0.0 { -1.0 } else { 1.0 };
            let mut out = vec![
                0.0,
                -classic,
                classic * 1.0e-3,
                classic * 1.0e3,
                classic * 1.0e6,
                sign * 1.0e-30,
                sign * 1.0e12,
                sign * 1.0e30,
                sign * f64::from(f32::MIN_POSITIVE),
                sign * f64::from(f32::MAX),
                -sign * 1.0e30,
            ];
            out.dedup();
            out.into_iter().map(TuneValue::Float).collect()
        }
        (ValueKind::Int, _) => [0, 1, -1, 2, 1000, i64::from(i32::MAX), i64::from(i32::MIN)]
            .into_iter()
            .map(TuneValue::Int)
            .collect(),
        (ValueKind::Bool, TuneValue::Bool(classic)) => vec![TuneValue::Bool(!classic)],
        (ValueKind::Choice(names), _) => names
            .iter()
            .map(|name| TuneValue::Text((*name).to_owned()))
            .collect(),
        _ => Vec::new(),
    }
}

/// The input of the next tick: a random walk with a jump, a second jump in
/// the air (the rocket boots), and the grapple fired at the nearest target.
/// Reads the game to aim; never writes it.
fn drive(script: &mut InputScript, tick: usize, game: &Game) -> InputFrame {
    let mut input = script.next_frame();
    let phase = tick % 120;
    let player = game.player();
    match phase {
        10 => {
            input.jump_pressed = true;
            input.jump_held = true;
        }
        11..=24 => input.jump_held = true,
        25 => input.jump_pressed = !player.grounded,
        50 => {
            input.grapple_held = false;
            let eye = game.eye_position();
            let level = game.level();
            let target = level
                .grapple_points
                .iter()
                .map(|point| point.position)
                .chain(level.crystals.iter().map(|crystal| crystal.center))
                .min_by(|a, b| a.distance(eye).total_cmp(&b.distance(eye)));
            if let Some(target) = target {
                let to = target - eye;
                input.look_yaw_delta = to.y.atan2(to.x) - player.yaw;
                input.look_pitch_delta = to.z.atan2(to.truncate().length()) - player.pitch;
            }
        }
        51..=90 => {
            input.look_yaw_delta = 0.0;
            input.look_pitch_delta = 0.0;
            input.grapple_held = true;
        }
        _ => {}
    }
    input
}

/// What a session does to a game in a host, compressed: reconcile, the
/// commands a user reaches for, rules before and read-outs after every tick
/// of the game's own tick, and every read-only view. Returns the number of
/// ticks that ran.
fn exercise(session: &mut Session, game: &mut Game) -> usize {
    let mut script: Option<LevelScript> = None;
    let mut inputs = InputScript::new(DEFAULT_SEED);
    game.start();
    let mut ran = 0;
    for tick in 0..240 {
        {
            let mut cx = SimCx {
                game: &mut *game,
                script: &mut script,
            };
            let _ = session.reconcile(&mut cx);
            let command = match tick {
                40 => Some(Command::Slot {
                    op: SlotOp::Save { slot: 0 },
                }),
                70 => Some(Command::Teleport {
                    to: TeleportTarget::AimPoint,
                }),
                100 => Some(Command::Teleport {
                    to: TeleportTarget::Start,
                }),
                130 => Some(Command::Fly { on: Toggle::On }),
                131 => Some(Command::Time {
                    op: TimeOp::Freeze(Toggle::Off),
                }),
                160 => Some(Command::Slot {
                    op: SlotOp::Load { slot: 0 },
                }),
                180 => Some(Command::Rewind),
                200 => Some(Command::Kill),
                220 => Some(Command::Respawn),
                _ => None,
            };
            if let Some(command) = command {
                // Refusals are fine; panics and hangs are not.
                let _ = session.execute(command, &mut cx);
            }
            if session.flying() {
                session.fly_move(&mut cx, Vec3::new(1.0e9, -1.0e9, 1.0e9));
            }
            session.before_tick(&mut cx);
        }
        let input = drive(&mut inputs, tick, game);
        if let Some(report) = game.tick(&input) {
            ran += 1;
            session.after_tick(game, script.as_ref(), &report);
        }
        if tick % 60 == 59 {
            let inspection = Inspection::new(game, script.as_ref(), session);
            let _ = inspection.to_json_pretty();
            let _ = inspection.aim();
            let _ = inspection.teleport_targets();
            let _ = predict(game, &input, 120);
            let _ = session.banner(game);
        }
    }
    ran
}

#[test]
fn extreme_values_the_validation_accepts_neither_panic_nor_hang_a_session() {
    let catalog = Catalog::classic();
    let mut cases: Vec<(String, TuneValue)> = Vec::new();
    let mut refused = 0_u32;
    for info in catalog.iter() {
        for value in extremes(info.kind, &info.classic) {
            let mut overlay = Overlay::default();
            match overlay.set(&info.key, value.clone()) {
                // Equal to Classic after all: nothing to run.
                Ok(()) if overlay.is_empty() => {}
                Ok(()) => cases.push((info.key.clone(), value)),
                Err(_) => refused += 1,
            }
        }
    }
    // Both happen: the validation refuses some extremes and lets others in.
    assert!(refused > 20, "{refused} refused");
    assert!(cases.len() > 200, "{} accepted", cases.len());

    let mut failures: Vec<String> = Vec::new();
    let mut ticks = 0_usize;
    for (key, value) in &cases {
        for stage in ["graybox", MOVEMENT_LAB, GRAPPLE_LAB] {
            let what = format!("{key} = {value} on {stage}");
            let (key, value) = (key.clone(), value.clone());
            let outcome = std::panic::catch_unwind(|| {
                guarded(&what, move || {
                    let mut profile = Profile::classic();
                    profile.overrides.set(&key, value).unwrap();
                    let mut session = Session::new(profile).unwrap();
                    let level = if stage == "graybox" {
                        graybox_test_level()
                    } else {
                        build_arena(stage).unwrap()
                    };
                    // A set the validation accepts may still be one the level
                    // cannot be built with; that is a refusal, not a failure.
                    match session.new_game(level) {
                        Ok(mut game) => exercise(&mut session, &mut game),
                        Err(_) => 0,
                    }
                })
            });
            match outcome {
                Ok(ran) => ticks += ran,
                Err(_) => failures.push(what),
            }
        }
    }
    assert!(
        failures.is_empty(),
        "{} of {} extreme cases panicked or hung:\n{}",
        failures.len(),
        cases.len() * 3,
        failures.join("\n")
    );
    assert!(ticks > 10_000, "the cases did run: {ticks} ticks");
    // Shown with `--nocapture`.
    println!(
        "{} keys: {} extreme values accepted and run on 3 stages ({ticks} ticks), {refused} refused",
        catalog.len(),
        cases.len()
    );
}

/// A converted level (synthetic: written here, no game data): a floor and a
/// player start, with the start sunk into the floor so a teleport there has
/// to look for room.
fn converted_game() -> Game {
    const MAP: &str = "SandboxHostileMap";
    let mut src = MemorySource::new();
    let mut s = SceneFixture::new(MAP, -5000.0);
    s.set_bsp(
        vec![
            [-4000.0, -4000.0, 0.0],
            [4000.0, -4000.0, 0.0],
            [4000.0, 4000.0, 0.0],
            [-4000.0, 4000.0, 0.0],
        ],
        vec![[0, 1, 2], [0, 2, 3]],
    );
    s.player_start(Vec3::new(0.0, 0.0, 1.0), 0);
    s.write(&mut src);
    let loaded = scene::load_map(&src, MAP, &LoadOptions::default()).unwrap();
    let mut game =
        Game::from_loaded_map(loaded, PlayerParams::asamu_original(), DEFAULT_TICK_RATE_HZ)
            .unwrap();
    game.start();
    game
}

#[test]
fn a_teleport_looks_for_room_in_bounded_time_whatever_the_cylinder_is() {
    // The collision cylinder is a parameter: any positive size validates
    // (`None`: the Classic value).
    let huge = f64::from(f32::MAX);
    for (radius, half_height) in [
        (Some(1.0e3), Some(1.0e3)),
        (Some(1.0e6), Some(1.0e6)),
        (Some(1.0e12), Some(1.0e12)),
        (Some(1.0e30), Some(1.0e30)),
        (Some(huge), Some(huge)),
        (Some(1.0e-3), Some(1.0e30)),
        (Some(1.0e30), None),
        (None, Some(1.0e9)),
    ] {
        let what = format!("cylinder {radius:?} x {half_height:?}");
        let outcome = guarded(&what, move || {
            let mut profile = Profile::classic();
            for (key, value) in [
                ("movement.capsule_half_height", half_height),
                ("movement.capsule_radius", radius),
            ] {
                if let Some(value) = value {
                    profile.overrides.set(key, TuneValue::Float(value)).unwrap();
                }
            }
            let mut session = Session::new(profile).unwrap();
            let mut game = converted_game();
            let mut script: Option<LevelScript> = None;
            let mut cx = SimCx {
                game: &mut game,
                script: &mut script,
            };
            session.reconcile(&mut cx).unwrap();
            let before = cx.game.player().position;
            let result = session.execute(
                Command::Teleport {
                    to: TeleportTarget::Start,
                },
                &mut cx,
            );
            (result.is_ok(), before, game.player().position)
        });
        // It answered. A teleport that went through put the player within
        // the placement limit; a refused one moved nothing.
        let (teleported, before, after) = outcome;
        assert!(after.is_finite(), "{what}: {outcome:?}");
        if teleported {
            assert!(
                after.abs().max_element() <= PLACEMENT_LIMIT,
                "{what}: {outcome:?}"
            );
        } else {
            assert_eq!(after, before, "{what}");
        }
    }
    // With the Classic cylinder the start that is sunk into the floor is
    // lifted clear of it: the search still does its job.
    let mut session = Session::classic();
    let mut game = converted_game();
    let mut script: Option<LevelScript> = None;
    let mut cx = SimCx {
        game: &mut game,
        script: &mut script,
    };
    session
        .execute(
            Command::Teleport {
                to: TeleportTarget::Start,
            },
            &mut cx,
        )
        .unwrap();
    let movement = PlayerParams::asamu_original().movement;
    let (radius, half_height) = (
        movement.capsule_radius.value,
        movement.capsule_half_height.value,
    );
    let placed = game.player().position;
    assert!(placed.z + 1.0 >= half_height, "{placed}");
    assert!(placed.z <= 1.0 + 2.0 * (radius + half_height), "{placed}");
}

#[test]
fn rules_and_commands_with_extreme_operands_are_refused_or_harmless() {
    let mut session = Session::classic();
    let mut game = session.new_game(build_arena(GRAPPLE_LAB).unwrap()).unwrap();
    game.start();
    let mut script: Option<LevelScript> = None;
    let mut inputs = InputScript::new(DEFAULT_SEED);
    let commands = [
        Command::SetRules {
            rules: Rules {
                grapples: GrappleRule::Fixed(i32::MAX),
                rocket_boots: Switch::On,
                auto_refill: true,
            },
        },
        Command::SetRules {
            rules: Rules {
                grapples: GrappleRule::Fixed(i32::MIN),
                rocket_boots: Switch::Off,
                auto_refill: true,
            },
        },
        Command::SetRules {
            rules: Rules::default(),
        },
        Command::SetMaxGrapples { n: i32::MAX },
        Command::SetMaxGrapples { n: i32::MIN },
        Command::NudgeParam {
            key: "movement.jump_velocity".to_owned(),
            steps: i32::MAX,
            scale: f64::MAX,
        },
        Command::NudgeParam {
            key: "movement.jump_velocity".to_owned(),
            steps: i32::MIN,
            scale: f64::MIN_POSITIVE,
        },
        Command::NudgeParam {
            key: "gun.initial_max_grapples".to_owned(),
            steps: i32::MIN,
            scale: 1.0e300,
        },
        Command::NudgeParam {
            key: "pawn.zoom_enabled".to_owned(),
            steps: i32::MIN,
            scale: f64::NAN,
        },
        Command::NudgeParam {
            key: "grapple.rope_mode".to_owned(),
            steps: i32::MIN,
            scale: 1.0,
        },
        Command::SetParam {
            key: "x".repeat(100_000),
            value: TuneValue::Text("y".repeat(100_000)),
        },
        Command::ResetParam {
            key: "\u{0}".to_owned(),
        },
        Command::Teleport {
            to: TeleportTarget::Position {
                position: [f32::MAX, f32::MIN, f32::NAN],
                yaw: Some(f32::INFINITY),
                pitch: Some(f32::NAN),
            },
        },
        Command::Teleport {
            to: TeleportTarget::Position {
                position: [9.0e5, -9.0e5, 9.0e5],
                yaw: Some(1.0e30),
                pitch: Some(-1.0e30),
            },
        },
        Command::Teleport {
            to: TeleportTarget::Checkpoint { id: u32::MAX },
        },
        Command::Teleport {
            to: TeleportTarget::Mark {
                name: "never set".to_owned(),
            },
        },
        Command::SetMark {
            name: "m".repeat(10_000),
        },
        Command::SetMark {
            name: "line\nbreak".to_owned(),
        },
        Command::Time {
            op: TimeOp::Scale { value: f32::NAN },
        },
        Command::Time {
            op: TimeOp::Scale { value: 1.0e30 },
        },
        Command::Time {
            op: TimeOp::Step { n: u32::MAX },
        },
        Command::Time { op: TimeOp::Reset },
        Command::Slot {
            op: SlotOp::Load { slot: usize::MAX },
        },
        Command::Slot {
            op: SlotOp::Save { slot: usize::MAX },
        },
        Command::Slot {
            op: SlotOp::Select { slot: 4 },
        },
        Command::Slot {
            op: SlotOp::Clear { slot: usize::MAX },
        },
    ];
    for (index, command) in commands.into_iter().enumerate() {
        let name = command.name();
        let mut cx = SimCx {
            game: &mut game,
            script: &mut script,
        };
        if let Err(error) = session.execute(command, &mut cx) {
            // A refusal is a sentence. (An unknown key is quoted back in
            // full; the user interface clips what it shows.)
            let text = error.to_string();
            assert!(
                text.len() < 400 || text.starts_with("unknown parameter"),
                "{index} {name}: {} bytes",
                text.len()
            );
        }
        for tick in 0..30 {
            let mut cx = SimCx {
                game: &mut game,
                script: &mut script,
            };
            session.before_tick(&mut cx);
            let input = drive(&mut inputs, index * 30 + tick, &game);
            let report = game.tick(&input).unwrap();
            session.after_tick(&game, script.as_ref(), &report);
        }
        // Pending steps and bookmarks stay bounded whatever was asked for.
        assert!(session.time().pending_steps() <= 3600, "{index} {name}");
        assert!(session.log().len() <= 4096, "{index} {name}");
    }
    assert!(game.player().position.is_finite());
}
