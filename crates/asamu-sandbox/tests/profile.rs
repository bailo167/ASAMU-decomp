//! Profiles: the file format, hostile input, the built-in presets and the
//! profile store.
//!
//! Every profile here is ours. The presets' numbers are factors of Classic
//! values read at run time; no test below writes a Classic number down.

use std::path::PathBuf;

use asamu_player::PlayerParams;
use asamu_sandbox::keys::{Catalog, TuneValue};
use asamu_sandbox::overlay::{
    BASE_CLASSIC, MAX_OVERRIDES, OverlayError, ParamSetLabel, param_set_label,
};
use asamu_sandbox::profile::{
    MAX_PROFILE_BYTES, PROFILE_FORMAT, PROFILE_VERSION, Profile, ProfileError, ProfileStore,
    SandboxDirs, valid_profile_name,
};
use asamu_sandbox::rules::{GrappleRule, Rules, Switch};
use asamu_sandbox::session::Session;
use asamu_sandbox::time::SPEED_STEPS;

/// The example profile committed with the crate.
const EXAMPLE: &str = include_str!("../examples/profiles/floaty.json");

fn example() -> Profile {
    Profile::from_json_slice(EXAMPLE.as_bytes()).expect("the committed example is valid")
}

/// A fresh, empty directory under the system's temporary directory, removed
/// again when dropped.
struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "asamu-sandbox-test-{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        Self(dir)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn the_committed_example_parses() {
    let profile = example();
    assert_eq!(profile.format, PROFILE_FORMAT);
    assert_eq!(profile.version, PROFILE_VERSION);
    assert_eq!(profile.name, "floaty");
    assert!(valid_profile_name(&profile.name));
    assert!(
        profile.description.contains("ours"),
        "{}",
        profile.description
    );
    assert_eq!(profile.base, BASE_CLASSIC);
    assert_eq!(profile.overrides.len(), 2);
    assert_eq!(
        profile.rules,
        Rules {
            grapples: GrappleRule::Unlimited,
            rocket_boots: Switch::On,
            auto_refill: false,
        }
    );
    assert_eq!(profile.time_scale, None);
    assert!(profile.extensions.is_empty());
    assert!(!profile.is_pristine());
    assert_eq!(profile.check(), Ok(()));

    // It is a set a session can run, labelled as modified.
    let params = profile.overrides.apply().unwrap();
    assert_eq!(params.validate(), Ok(()));
    assert_eq!(
        param_set_label(&params),
        ParamSetLabel::Modified { overrides: 2 }
    );
    let session = Session::new(profile).unwrap();
    assert_eq!(*session.params(), params);
}

#[test]
fn profiles_round_trip() {
    let mut profile = example();
    profile.time_scale = Some(0.5);
    profile.extensions.insert(
        "future-tool".to_owned(),
        serde_json::json!({"kept": [1, 2, 3]}),
    );
    let json = profile.to_json_pretty().unwrap();
    assert!(json.contains("\n  \"format\""), "pretty-printed: {json}");
    let back = Profile::from_json_slice(json.as_bytes()).unwrap();
    assert_eq!(back, profile);
    // Extensions are kept as read, not interpreted.
    assert_eq!(back.extensions["future-tool"]["kept"][2], 3);
    // Stable: writing what was read gives the same bytes.
    assert_eq!(back.to_json_pretty().unwrap(), json);

    // The classic profile too, and it stays pristine.
    let classic = Profile::classic();
    let json = classic.to_json_pretty().unwrap();
    let back = Profile::from_json_slice(json.as_bytes()).unwrap();
    assert_eq!(back, classic);
    assert!(back.is_pristine());
    assert_eq!(
        back.overrides.apply().unwrap(),
        PlayerParams::asamu_original()
    );

    // Optional fields may be left out; `base` may not (see below).
    let minimal = format!(
        r#"{{"format": "{PROFILE_FORMAT}", "version": {PROFILE_VERSION}, "name": "bare",
            "base": "{BASE_CLASSIC}"}}"#
    );
    let bare = Profile::from_json_slice(minimal.as_bytes()).unwrap();
    assert!(bare.is_pristine());
    assert_eq!(bare.description, "");
    assert_eq!(bare.rules, Rules::default());
}

#[test]
fn reading_normalizes_the_overrides() {
    let jump = match &Catalog::shared()
        .get("movement.jump_velocity")
        .unwrap()
        .classic
    {
        TuneValue::Float(v) => *v,
        other => panic!("{other:?}"),
    };
    // An integer for a float key, and an entry that only restates Classic.
    let text = format!(
        r#"{{"format": "{PROFILE_FORMAT}", "version": {PROFILE_VERSION}, "name": "tidy",
            "base": "{BASE_CLASSIC}",
            "overrides": {{"movement.ground_friction": 3, "movement.jump_velocity": {jump}}}}}"#
    );
    let profile = Profile::from_json_slice(text.as_bytes()).unwrap();
    assert_eq!(profile.overrides.len(), 1);
    assert_eq!(
        profile.overrides.get("movement.ground_friction"),
        Some(&TuneValue::Float(3.0))
    );
    // A profile that only restates Classic values is the classic profile.
    let text = format!(
        r#"{{"format": "{PROFILE_FORMAT}", "version": {PROFILE_VERSION}, "name": "same",
            "base": "{BASE_CLASSIC}", "overrides": {{"movement.jump_velocity": {jump}}}}}"#
    );
    let profile = Profile::from_json_slice(text.as_bytes()).unwrap();
    assert!(profile.is_pristine());
    assert_eq!(
        profile.overrides.apply().unwrap(),
        PlayerParams::asamu_original()
    );
}

/// The example with one piece of its text replaced.
fn edited(from: &str, to: &str) -> String {
    assert!(EXAMPLE.contains(from), "the example contains {from:?}");
    EXAMPLE.replacen(from, to, 1)
}

#[test]
fn malformed_profiles_are_refused() {
    let parse = |text: &str| Profile::from_json_slice(text.as_bytes());

    // Unknown fields, anywhere.
    for text in [
        edited(r#""version": 1,"#, r#""version": 1, "surprise": true,"#),
        edited(r#""auto_refill": false"#, r#""auto_refil": false"#),
        edited(
            r#""auto_refill": false"#,
            r#""auto_refill": false, "god_mode": true"#,
        ),
    ] {
        assert!(matches!(parse(&text), Err(ProfileError::Json(_))), "{text}");
    }

    // `base` is required, and only the Classic base exists.
    assert!(matches!(
        parse(&edited(r#""base": "asamu_original","#, "")),
        Err(ProfileError::Json(message)) if message.contains("base")
    ));
    for other in ["placeholder", "", "ASAMU_ORIGINAL", "asamu_original "] {
        assert_eq!(
            parse(&edited(
                r#""base": "asamu_original""#,
                &format!(r#""base": "{other}""#)
            )),
            Err(ProfileError::UnknownBase(other.to_owned()))
        );
    }

    // Wrong or missing format marker; other versions.
    assert_eq!(
        parse(&edited("asamu-sandbox-profile", "asamu-trace")),
        Err(ProfileError::WrongFormat("asamu-trace".to_owned()))
    );
    assert_eq!(
        parse(&edited(r#""format": "asamu-sandbox-profile","#, "")),
        Err(ProfileError::WrongFormat(String::new()))
    );
    for version in [0u32, 2, 99, u32::MAX] {
        assert_eq!(
            parse(&edited(
                r#""version": 1"#,
                &format!(r#""version": {version}"#)
            )),
            Err(ProfileError::UnsupportedVersion(version))
        );
    }
    // A later version is named as such even when it has fields this build
    // does not know.
    assert_eq!(
        parse(&edited(
            r#""version": 1,"#,
            r#""version": 2, "new_in_version_two": {},"#
        )),
        Err(ProfileError::UnsupportedVersion(2))
    );
    for bad_version in [r#""version": "1""#, r#""version": 1.5"#, r#""version": -1"#] {
        assert!(matches!(
            parse(&edited(r#""version": 1"#, bad_version)),
            Err(ProfileError::Json(_))
        ));
    }
    assert!(matches!(
        parse(&edited(r#""version": 1,"#, "")),
        Err(ProfileError::Json(_))
    ));

    // Names that are not file-safe.
    for name in ["", "Floaty", "../up", "a b", "dot.json", &"x".repeat(41)] {
        assert_eq!(
            parse(&edited(
                r#""name": "floaty""#,
                &format!(r#""name": "{name}""#)
            )),
            Err(ProfileError::BadName(name.to_owned())),
            "{name:?}"
        );
    }

    // Overrides that do not apply.
    let with_overrides = |overrides: &str| {
        let start = EXAMPLE
            .find(r#""overrides""#)
            .expect("the example has overrides");
        let end = EXAMPLE.find(r#""rules""#).expect("the example has rules");
        format!(
            r#"{}"overrides": {overrides}, {}"#,
            &EXAMPLE[..start],
            &EXAMPLE[end..]
        )
    };
    assert!(parse(&with_overrides("{}")).is_ok());
    assert!(matches!(
        parse(&with_overrides(r#"{"movement.gravity_scale": 0.5}"#)),
        Err(ProfileError::Overlay(OverlayError::UnknownKey { .. }))
    ));
    assert!(matches!(
        parse(&with_overrides(r#"{"movement.jump_velocity": true}"#)),
        Err(ProfileError::Overlay(OverlayError::WrongType { .. }))
    ));
    assert!(matches!(
        parse(&with_overrides(r#"{"movement.air_control": 12.0}"#)),
        Err(ProfileError::Overlay(OverlayError::Invalid(_)))
    ));
    assert!(matches!(
        parse(&with_overrides(r#"{"movement.jump_velocity": 1e300}"#)),
        Err(ProfileError::Overlay(OverlayError::NotRepresentable { .. }))
    ));
    // A whole parameter set is not an override: profiles hold values only.
    assert!(matches!(
        parse(&with_overrides(
            r#"{"movement.jump_velocity": {"value": 1.0, "provenance": {"kind": "config"}}}"#
        )),
        Err(ProfileError::Json(_))
    ));
    assert!(matches!(
        parse(&with_overrides(r#"{"pawn": null}"#)),
        Err(ProfileError::Json(_))
    ));
    let many: Vec<String> = (0..=MAX_OVERRIDES)
        .map(|i| format!(r#""k.k{i}": 1"#))
        .collect();
    assert_eq!(
        parse(&with_overrides(&format!("{{{}}}", many.join(",")))),
        Err(ProfileError::Overlay(OverlayError::TooMany {
            limit: MAX_OVERRIDES
        }))
    );

    // Time scales outside the time control's range.
    let slowest = SPEED_STEPS.iter().copied().fold(f32::INFINITY, f32::min);
    let fastest = SPEED_STEPS
        .iter()
        .copied()
        .fold(f32::NEG_INFINITY, f32::max);
    for ok in [slowest, 1.0, fastest] {
        let text = edited(r#""time_scale": null"#, &format!(r#""time_scale": {ok}"#));
        assert_eq!(parse(&text).map(|p| p.time_scale), Ok(Some(ok)));
    }
    for bad in [
        "0",
        "-1",
        "1e9",
        &format!("{}", slowest / 2.0),
        &format!("{}", fastest * 2.0),
    ] {
        let text = edited(r#""time_scale": null"#, &format!(r#""time_scale": {bad}"#));
        assert!(
            matches!(parse(&text), Err(ProfileError::Json(message)) if message.contains("time_scale")),
            "{bad}"
        );
    }

    // Not a profile at all.
    for text in [
        "",
        " ",
        "null",
        "[]",
        "{}",
        "42",
        "\"profile\"",
        "{\"format\": 5}",
        "\u{feff}{}",
    ] {
        assert!(parse(text).is_err(), "{text:?}");
    }
    assert!(Profile::from_json_slice(&[0xff, 0xfe, 0x00, 0x7b]).is_err());
    assert!(Profile::from_json_slice(&[0u8; 64]).is_err());
}

#[test]
fn oversize_and_truncated_input_is_refused_without_panics() {
    // Too large: refused by size, before any parsing.
    let padding = " ".repeat(MAX_PROFILE_BYTES);
    let oversize = format!("{EXAMPLE}{padding}");
    assert_eq!(
        Profile::from_json_slice(oversize.as_bytes()),
        Err(ProfileError::TooLarge {
            bytes: oversize.len(),
            limit: MAX_PROFILE_BYTES
        })
    );
    // Exactly at the limit it is read.
    let fits = format!("{EXAMPLE}{}", " ".repeat(MAX_PROFILE_BYTES - EXAMPLE.len()));
    assert_eq!(fits.len(), MAX_PROFILE_BYTES);
    assert_eq!(Profile::from_json_slice(fits.as_bytes()), Ok(example()));
    // Large and hostile within the limit: deep nesting, a huge string.
    let deep = format!(
        r#"{{"format": "{PROFILE_FORMAT}", "version": 1, "name": "deep", "base": "{BASE_CLASSIC}",
            "extensions": {{"nest": {}{}}}}}"#,
        "[".repeat(50_000),
        "]".repeat(50_000)
    );
    assert!(deep.len() < MAX_PROFILE_BYTES);
    assert!(matches!(
        Profile::from_json_slice(deep.as_bytes()),
        Err(ProfileError::Json(_))
    ));
    // The same file with a shallow extension is fine.
    let shallow = deep
        .replace(&"[".repeat(50_000), "[")
        .replace(&"]".repeat(50_000), "]");
    assert!(Profile::from_json_slice(shallow.as_bytes()).is_ok());
    let long_name = edited(
        r#""name": "floaty""#,
        &format!(r#""name": "{}""#, "a".repeat(100_000)),
    );
    assert!(matches!(
        Profile::from_json_slice(long_name.as_bytes()),
        Err(ProfileError::BadName(_))
    ));

    // Every truncation of the example is an error, never a panic and never
    // a half-read profile.
    let bytes = EXAMPLE.trim_end().as_bytes();
    for cut in 0..bytes.len() {
        assert!(
            Profile::from_json_slice(&bytes[..cut]).is_err(),
            "the first {cut} bytes were accepted"
        );
    }
    assert!(Profile::from_json_slice(bytes).is_ok());

    // Every single-byte corruption either fails or yields a profile that
    // passes every check (a changed digit can still be a valid value).
    for at in 0..bytes.len() {
        for replacement in [b'0', b'"', b'}', b',', 0x00, 0xff] {
            let mut corrupt = bytes.to_vec();
            corrupt[at] = replacement;
            if let Ok(profile) = Profile::from_json_slice(&corrupt) {
                assert_eq!(profile.check(), Ok(()), "byte {at} = {replacement:#04x}");
                assert!(profile.overrides.apply().is_ok());
            }
        }
    }
}

#[test]
fn profile_names_are_file_stems() {
    for good in ["a", "floaty", "super-jump", "run_2", "0", &"x".repeat(40)] {
        assert!(valid_profile_name(good), "{good}");
    }
    for bad in [
        "",
        " ",
        "Floaty",
        "has space",
        "dot.json",
        ".",
        "..",
        "../up",
        "a/b",
        "a\\b",
        "c:",
        "nul\0",
        "\u{e9}",
        "new\nline",
        &"x".repeat(41),
    ] {
        assert!(!valid_profile_name(bad), "{bad:?}");
    }
}

#[test]
fn every_builtin_validates() {
    let builtin = Profile::builtin();
    let names: Vec<&str> = builtin.iter().map(|p| p.name.as_str()).collect();
    assert_eq!(
        names,
        [
            "classic",
            "moon",
            "heavy",
            "super-jump",
            "ice",
            "sprinter",
            "long-reach",
            "infinite-grapple",
            "bullet-time"
        ]
    );
    // How many overrides each preset is meant to have (a preset that lost
    // one because a Classic value changed shows up here).
    let expected_overrides = [0, 1, 2, 2, 2, 2, 1, 0, 0];
    let classic = PlayerParams::asamu_original();
    for (profile, overrides) in builtin.iter().zip(expected_overrides) {
        let name = &profile.name;
        assert_eq!(profile.check(), Ok(()), "{name}");
        assert!(valid_profile_name(name), "{name}");
        assert_eq!(profile.base, BASE_CLASSIC, "{name}");
        assert_eq!(profile.overrides.len(), overrides, "{name}");
        assert!(!profile.description.is_empty(), "{name}");

        // Reads back as itself, and makes a session.
        let json = profile.to_json_pretty().unwrap();
        assert_eq!(
            &Profile::from_json_slice(json.as_bytes()).unwrap(),
            profile,
            "{name}"
        );
        let params = profile.overrides.apply().unwrap();
        assert_eq!(params.validate(), Ok(()), "{name}");
        let session = Session::new(profile.clone()).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!(*session.params(), params, "{name}");

        if name == "classic" {
            assert_eq!(profile, &Profile::classic());
            assert!(profile.is_pristine());
            assert_eq!(params, classic);
        } else {
            // Every other preset changes something and says it is ours.
            assert!(!profile.is_pristine(), "{name}");
            assert!(profile.description.contains("Ours."), "{name}");
        }
        // Presets with overrides are never labelled Classic; presets
        // without (a rule, a speed) run the Classic set itself.
        assert_eq!(
            param_set_label(&params) == ParamSetLabel::Classic,
            overrides == 0,
            "{name}"
        );
        // Each override really is a factor of the Classic value: a float
        // that is not the Classic one, with the same sign.
        for (key, value) in profile.overrides.iter() {
            let info = Catalog::shared().get(key).unwrap();
            let (TuneValue::Float(v), TuneValue::Float(c)) = (value, &info.classic) else {
                panic!("{name}: {key} is not a float override");
            };
            assert!(v != c && v * c > 0.0, "{name}: {key} = {v} (Classic {c})");
        }
        if let Some(scale) = profile.time_scale {
            assert!(SPEED_STEPS.contains(&scale), "{name}: {scale}");
        }
    }
    // The presets are built from Classic each time: the same every call.
    assert_eq!(Profile::builtin(), builtin);
}

#[test]
fn the_store_saves_lists_and_loads() {
    let temp = TempDir::new("store");
    let dirs = SandboxDirs {
        root: temp.0.join("sandbox"),
    };
    let store = ProfileStore::new(dirs.profiles());
    assert_eq!(store.dir(), dirs.profiles());
    // Nothing is created until something is saved.
    assert!(store.list().is_empty());
    assert!(!dirs.root.exists());
    assert_eq!(
        store.load("floaty"),
        Err(ProfileError::NotFound("floaty".to_owned()))
    );

    let profile = example();
    let path = store.save(&profile).unwrap();
    assert_eq!(path, dirs.profiles().join("floaty.json"));
    assert!(
        path.starts_with(&dirs.root),
        "saved under the Sandbox's own root"
    );
    assert_eq!(store.load("floaty"), Ok(profile.clone()));
    // What is on disk is the profile's own JSON.
    let on_disk = std::fs::read_to_string(&path).unwrap();
    assert_eq!(on_disk.trim_end(), profile.to_json_pretty().unwrap());

    // Saving again replaces, and leaves no temporary file behind.
    let mut changed = profile.clone();
    changed.description = "changed".to_owned();
    changed.rules.auto_refill = true;
    assert_eq!(store.save(&changed).unwrap(), path);
    assert_eq!(store.load("floaty"), Ok(changed));
    let files: Vec<String> = std::fs::read_dir(store.dir())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(files, ["floaty.json"]);

    // More profiles, listed sorted; other files are ignored.
    for name in ["zebra", "alpha-2", "moon"] {
        store
            .save(&Profile {
                name: name.to_owned(),
                ..Profile::classic()
            })
            .unwrap();
    }
    std::fs::write(store.dir().join("notes.txt"), "not a profile").unwrap();
    std::fs::write(store.dir().join("Bad Name.json"), "{}").unwrap();
    std::fs::write(store.dir().join(".hidden.json"), "{}").unwrap();
    std::fs::create_dir(store.dir().join("folder.json")).unwrap();
    assert_eq!(store.list(), ["alpha-2", "floaty", "moon", "zebra"]);

    // A file is known by its file name, whatever name it carries inside.
    std::fs::copy(
        store.dir().join("zebra.json"),
        store.dir().join("copy.json"),
    )
    .unwrap();
    assert_eq!(store.load("copy").unwrap().name, "copy");
}

#[test]
fn the_store_refuses_bad_names_and_bad_files() {
    let temp = TempDir::new("refuse");
    let store = ProfileStore::new(temp.0.join("profiles"));
    // Names that could leave the directory never reach the file system.
    for name in ["", "..", "../escape", "a/b", "Upper", "x.json"] {
        assert_eq!(
            store.load(name),
            Err(ProfileError::BadName(name.to_owned())),
            "{name:?}"
        );
        let profile = Profile {
            name: name.to_owned(),
            ..Profile::classic()
        };
        assert_eq!(
            store.save(&profile),
            Err(ProfileError::BadName(name.to_owned())),
            "{name:?}"
        );
    }
    assert!(!temp.0.exists(), "a refused save creates nothing");

    // A profile that would not load again is not written.
    let unloadable = [
        Profile {
            base: "placeholder".to_owned(),
            ..Profile::classic()
        },
        Profile {
            version: PROFILE_VERSION + 1,
            ..Profile::classic()
        },
        Profile {
            format: "something-else".to_owned(),
            ..Profile::classic()
        },
        Profile {
            time_scale: Some(f32::NAN),
            ..Profile::classic()
        },
        Profile {
            time_scale: Some(0.0),
            ..Profile::classic()
        },
        Profile {
            overrides: serde_json::from_str(r#"{"movement.air_control": 9.0}"#).unwrap(),
            ..Profile::classic()
        },
    ];
    for profile in &unloadable {
        assert!(store.save(profile).is_err(), "{profile:?}");
    }
    assert!(store.list().is_empty());
    assert!(!temp.0.exists());

    // Files that are not valid profiles are errors on load, not panics.
    std::fs::create_dir_all(store.dir()).unwrap();
    std::fs::write(store.dir().join("garbage.json"), b"\xff\xfe not json").unwrap();
    std::fs::write(store.dir().join("empty.json"), b"").unwrap();
    std::fs::write(
        store.dir().join("huge.json"),
        vec![b' '; MAX_PROFILE_BYTES + 10],
    )
    .unwrap();
    std::fs::create_dir(store.dir().join("folder.json")).unwrap();
    assert!(matches!(store.load("garbage"), Err(ProfileError::Json(_))));
    assert!(matches!(store.load("empty"), Err(ProfileError::Json(_))));
    assert_eq!(
        store.load("huge"),
        Err(ProfileError::TooLarge {
            bytes: MAX_PROFILE_BYTES + 1,
            limit: MAX_PROFILE_BYTES
        }),
        "only one byte past the limit is ever read"
    );
    assert_eq!(
        store.load("folder"),
        Err(ProfileError::NotFound("folder".to_owned()))
    );
    // They are still listed by name (listing opens no file).
    assert_eq!(store.list(), ["empty", "garbage", "huge"]);
}

#[test]
fn sandbox_directories_are_their_own() {
    let dirs = SandboxDirs {
        root: PathBuf::from("data").join("sandbox"),
    };
    let all = [dirs.profiles(), dirs.recordings(), dirs.dumps()];
    for (i, dir) in all.iter().enumerate() {
        assert!(dir.starts_with(&dirs.root), "{}", dir.display());
        assert_ne!(*dir, dirs.root);
        for other in &all[i + 1..] {
            assert_ne!(dir, other);
        }
        // Never the saves directory or a file of the save root.
        assert!(!dir.ends_with("saves"));
    }
    // The default location is a `sandbox` folder of its own inside the user
    // data root: beside `saves/` and `settings.json`, never inside them.
    if let Some(default) = SandboxDirs::default_location() {
        assert_eq!(
            default.root.file_name().and_then(|n| n.to_str()),
            Some("sandbox")
        );
        let save_root = asamu_game::save::SaveStore::default_root().unwrap();
        assert_eq!(default.root.parent(), Some(save_root.as_path()));
        assert!(!default.root.starts_with(save_root.join("saves")));
    }
}
