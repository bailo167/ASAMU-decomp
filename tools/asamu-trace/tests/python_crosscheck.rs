//! The Python recorder (tools/trace-recorder) against the Rust reference:
//! its own self-tests (with the Mac and the Windows layout), its LLDB front
//! end against a stand-in `lldb` module, its Windows front end against a
//! fake 32-bit image, and its raw → canonical converter against
//! `asamu_trace::convert` on pseudo-random recordings. Skips when no
//! `python3` is on the PATH.

#![allow(clippy::unwrap_used)]

mod common;

use std::path::Path;

use asamu_trace::convert::{ConvertOptions, MoveInput, convert};
use asamu_trace::move_input::MoveFrame;
use asamu_trace::raw::{OPTIONAL_FIELDS, RawBinding, RawFile, RawPlayer, RawTimer};
use asamu_trace::sidecar::RecorderLog;

use common::{python, recorder_dir, run_python};

#[test]
fn python_self_tests() {
    let Some(py) = python() else {
        eprintln!("skipped: no python3");
        return;
    };
    let dir = recorder_dir();
    let core = dir.join("asamu_recorder_core.py");
    let out = run_python(py, &[core.as_os_str(), "selftest".as_ref()]);
    assert!(out.contains("selftest ok"), "{out}");
    let glue = dir.join("test_lldb_glue.py");
    let out = run_python(py, &[glue.as_os_str()]);
    assert!(out.contains("lldb glue test ok"), "{out}");
    // The same core with the Windows layout (4-byte pointers, UTF-16).
    let win_layout = dir.join("layout_win_x86.json");
    let out = run_python(
        py,
        &[
            core.as_os_str(),
            "selftest".as_ref(),
            "--layout".as_ref(),
            win_layout.as_os_str(),
        ],
    );
    assert!(
        out.contains("selftest ok (layout win-x86-steam-1822049, 4-byte pointers"),
        "{out}"
    );
}

/// The Windows front end on its fake 32-bit image (frame order played one
/// event per memory read; no Windows needed).
#[test]
fn python_windows_front_end_tests() {
    let Some(py) = python() else {
        eprintln!("skipped: no python3");
        return;
    };
    let glue = recorder_dir().join("test_win_glue.py");
    let out = run_python(py, &[glue.as_os_str()]);
    assert!(out.contains("win glue test ok"), "{out}");
}

fn crosscheck(py: &str, raw: &RawFile, dir: &Path, stem: &str) -> usize {
    crosscheck_with(py, raw, dir, stem, &ConvertOptions::default())
}

/// Converts `raw` with both converters under `opts` and compares every
/// trace: samples bit for bit, metadata and notes. Returns the number of
/// traces. With `opts.log` the sidecar's text is `stats`, which the Python
/// converter finds next to the raw file as the recorder leaves it there.
fn crosscheck_with(
    py: &str,
    raw: &RawFile,
    dir: &Path,
    stem: &str,
    opts: &ConvertOptions,
) -> usize {
    crosscheck_files(py, raw, None, dir, stem, opts)
}

fn crosscheck_files(
    py: &str,
    raw: &RawFile,
    stats: Option<&str>,
    dir: &Path,
    stem: &str,
    opts: &ConvertOptions,
) -> usize {
    let raw_path = dir.join(format!("{stem}.raw.jsonl"));
    std::fs::write(&raw_path, raw.to_jsonl_string().unwrap()).unwrap();
    let mut opts = opts.clone();
    if let Some(text) = stats {
        std::fs::write(dir.join(format!("{stem}.stats.json")), text).unwrap();
        opts.log = Some(RecorderLog::from_json_str(text).unwrap());
    }
    let move_input = match opts.move_input {
        MoveInput::Auto => "auto",
        MoveInput::Keys => "keys",
        MoveInput::Acceleration => "acceleration",
    };
    let move_frame = match opts.move_frame {
        MoveFrame::Original => "original",
        MoveFrame::Yaw => "yaw",
    };
    let rust = convert(raw, &opts).unwrap();
    let python = common::python_convert(
        py,
        &raw_path,
        &dir.join(stem),
        &[
            "--move-input".as_ref(),
            move_input.as_ref(),
            "--move-frame".as_ref(),
            move_frame.as_ref(),
        ],
        rust.len(),
    );
    common::same_traces(&python, &rust);
    rust.len()
}

#[test]
fn python_and_rust_converters_agree() {
    let Some(py) = python() else {
        eprintln!("skipped: no python3");
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let (scripted, _) = common::scripted_recording(80, 10);
    assert_eq!(crosscheck(py, &scripted, dir.path(), "scripted"), 1);
    let mut segments = 0;
    for (i, seed) in [1_u64, 7, 99, 2024].into_iter().enumerate() {
        let raw = common::varied_recording(seed, 400, i % 2 == 0);
        segments += crosscheck(py, &raw, dir.path(), &format!("varied{seed}"));
    }
    assert!(segments >= 8, "only {segments} segments exercised");
    // Those recordings exercise the event notes: teleports (their positions
    // are random), level-script state changes of every kind and attaches
    // seen in the used-grapple counter only.
    let mut kinds = std::collections::BTreeSet::new();
    for (i, seed) in [1_u64, 7, 99, 2024].into_iter().enumerate() {
        let raw = common::varied_recording(seed, 400, i % 2 == 0);
        for seg in convert(&raw, &ConvertOptions::default()).unwrap() {
            for n in seg
                .trace
                .meta
                .notes
                .iter()
                .filter(|n| n.starts_with("event: "))
            {
                for what in [
                    "teleport(s)",
                    " and ",
                    "story mode on",
                    "story mode off",
                    "GroundSpeed set (a console speed)",
                    "grapple capacity changed",
                    "rocket boots enabled",
                    "rocket boots disabled",
                    "inside one frame",
                    "each with an attached sample",
                ] {
                    if n.contains(what) {
                        kinds.insert(what);
                    }
                }
            }
        }
    }
    assert_eq!(kinds.len(), 10, "event notes exercised: {kinds:?}");

    // Recordings without benchmark mode: every frame has its own length.
    // Both converters write no tick rate and the same sample times (the f64
    // sum of the f32 frame lengths).
    let lengths = common::jittery_lengths(3, 150, 60.0);
    let (variable, _) = common::scripted_recording_variable(&lengths, 500);
    assert_eq!(crosscheck(py, &variable, dir.path(), "variable"), 1);
    // Our own runs with a grapple flight (an attach inside one frame) and
    // with a respawn (a teleport), and a run whose eye height changes on
    // more ticks than the state note takes.
    let flight: Vec<common::Step> = (1..=130)
        .map(|i| common::Step {
            d_pitch: match i {
                3 => 3950,
                89 => -3000,
                _ => 0,
            },
            grapple: (10..87).contains(&i) || (90..110).contains(&i),
            ..common::Step::default()
        })
        .collect();
    let (flight, _) = common::scripted_steps(&flight, 100, false);
    assert_eq!(crosscheck(py, &flight, dir.path(), "flight"), 1);
    let fall: Vec<common::Step> = (1..=200)
        .map(|i| common::Step {
            forward: if i <= 60 { -1 } else { 0 },
            ..common::Step::default()
        })
        .collect();
    let (fall, _) = common::scripted_steps(&fall, 100, false);
    assert_eq!(crosscheck(py, &fall, dir.path(), "fall"), 1);
    for (raw, want) in [(&flight, "inside one frame"), (&fall, "teleport(s)")] {
        let t = &convert(raw, &ConvertOptions::default()).unwrap()[0].trace;
        assert!(
            t.meta
                .notes
                .iter()
                .any(|n| n.starts_with("event: ") && n.contains(want)),
            "{want}: {:#?}",
            t.meta.notes
        );
    }
    let mut restless = common::varied_recording(31, 60, true);
    let template = restless
        .records
        .iter()
        .find(|r| r.player.is_some() && r.world.as_ref().is_some_and(|w| !w.paused))
        .unwrap()
        .clone();
    let ticks = asamu_trace::state::EYE_HEIGHT_MAX_CHANGES + 3;
    restless.records = (0..ticks)
        .map(|i| {
            let mut r = template.clone();
            r.frame = 10 + i as u64;
            r.player.as_mut().unwrap().eye_height = Some(30.0 + (i % 2) as f32);
            r
        })
        .collect();
    assert_eq!(crosscheck(py, &restless, dir.path(), "restless"), 1);
    let t = &convert(&restless, &ConvertOptions::default()).unwrap()[0].trace;
    assert!(
        t.meta
            .notes
            .iter()
            .any(|n| n.starts_with("check: EyeHeight left out of the state: note")),
        "{:#?}",
        &t.meta.notes[..t.meta.notes.len().saturating_sub(2)]
    );
    let rust = convert(&variable, &ConvertOptions::default()).unwrap();
    assert_eq!(rust[0].trace.meta.tick_rate, None);
    let mut variable_segments = 0;
    for seed in [11_u64, 12, 13] {
        let mut raw = common::varied_recording(seed, 300, seed % 2 == 1);
        common::with_frame_lengths(&mut raw, seed);
        let segs = convert(&raw, &ConvertOptions::default()).unwrap();
        // (A two-sample run has one frame length, which is a fixed rate.)
        assert!(
            segs.iter()
                .all(|s| s.trace.samples.len() < 3 || s.trace.meta.tick_rate.is_none())
        );
        variable_segments += crosscheck(py, &raw, dir.path(), &format!("variable{seed}"));
    }
    assert!(variable_segments >= 6, "only {variable_segments} segments");

    // A level loaded again between two consecutive frames (the world's clock
    // goes back, nothing else changes): both converters end the run there.
    let (mut reloaded, _) = common::scripted_recording(80, 10);
    let before = convert(&reloaded, &ConvertOptions::default())
        .unwrap()
        .len();
    for r in &mut reloaded.records[40..] {
        let w = r.world.as_mut().unwrap();
        w.time_seconds -= 0.5;
        w.real_time_seconds -= 0.5;
    }
    assert_eq!(
        crosscheck(py, &reloaded, dir.path(), "reloaded"),
        before + 1
    );

    // Recordings made with a gamepad: the move axes from the pawn's
    // acceleration, in both converters bit for bit; also when asked for on
    // pseudo-random recordings (pawn rotations with pitch and roll, zero,
    // unit-length, tiny and huge accelerations, flying with and without gun
    // data), in either frame, and with the keys forced.
    let (pad, _) = common::scripted_gamepad_recording(150, 3_000);
    assert_eq!(crosscheck(py, &pad, dir.path(), "pad"), 1);
    let derived = convert(&pad, &ConvertOptions::default()).unwrap();
    assert!(
        derived[0]
            .trace
            .samples
            .iter()
            .any(|s| s.input.move_forward != 0.0 && s.input.move_right != 0.0),
        "the pad run moves diagonally"
    );
    let mut moved = 0;
    for (i, seed) in [21_u64, 22, 23, 24].into_iter().enumerate() {
        let mut raw = common::varied_recording(seed, 400, i % 2 == 0);
        common::with_accelerations(&mut raw, seed);
        for (move_input, move_frame) in [
            (MoveInput::Acceleration, MoveFrame::Original),
            (MoveInput::Acceleration, MoveFrame::Yaw),
            (MoveInput::Auto, MoveFrame::Original),
            (MoveInput::Keys, MoveFrame::Original),
        ] {
            let opts = ConvertOptions {
                move_input,
                move_frame,
                ..ConvertOptions::default()
            };
            let stem = format!("accel{seed}-{move_input:?}-{move_frame:?}");
            crosscheck_with(py, &raw, dir.path(), &stem, &opts);
            if move_input == MoveInput::Acceleration {
                moved += convert(&raw, &opts)
                    .unwrap()
                    .iter()
                    .flat_map(|s| &s.trace.samples)
                    .filter(|s| s.input.move_forward != 0.0 || s.input.move_right != 0.0)
                    .count();
            }
        }
    }
    assert!(moved > 800, "only {moved} derived samples exercised");

    // A hostile binding table (wide alias fan-out, self references, empty
    // parts): both converters stop at the same expansion budget.
    let mut raw = common::varied_recording(5, 300, true);
    let wide = |next: &str, k: usize| {
        let mut parts = vec![next.to_owned(); k];
        parts.push(String::new());
        parts.join("|")
    };
    for i in 0..6 {
        raw.header.bindings.push(RawBinding {
            name: format!("L{i}"),
            command: wide(&format!("L{}", i + 1), 12),
        });
    }
    raw.header.bindings.extend(
        [
            ("L6", "Axis aBaseY Speed=1 | Jump | L0"),
            ("W", "L0 | Axis aBaseY Speed=-1"),
            ("S", "L3 | Axis aBaseY Speed=-1 | Axis aBaseY Speed=-1"),
            ("D", "D | D | Axis aStrafe Speed=+1"),
        ]
        .into_iter()
        .map(|(n, c)| RawBinding {
            name: n.into(),
            command: c.into(),
        }),
    );
    assert!(crosscheck(py, &raw, dir.path(), "hostile") >= 1);
}

/// The names of the optional members are the recorder's: this reader's list
/// is `Optional.MEMBERS` of the Python core, and the version-1 members the
/// recorder strips for its version-1 view are the ones the Rust structures
/// write for a record without optional members.
#[test]
fn optional_member_names_are_the_recorders() {
    let Some(py) = python() else {
        eprintln!("skipped: no python3");
        return;
    };
    let script = "import sys, json\nsys.path.insert(0, sys.argv[1])\nimport asamu_recorder_core as core\n\
         print(json.dumps({'optional': sorted(m for ms in core.Optional.MEMBERS.values() for m in ms), \
         'header': sorted(core.V1_HEADER), 'player': sorted(core.V1_PLAYER), 'gun': sorted(core.V1_GUN)}))";
    let dir = recorder_dir();
    let out = run_python(py, &["-c".as_ref(), script.as_ref(), dir.as_os_str()]);
    let lists: serde_json::Value = serde_json::from_str(out.trim()).unwrap();
    let strings = |v: &serde_json::Value| -> Vec<String> {
        v.as_array()
            .unwrap()
            .iter()
            .map(|x| x.as_str().unwrap().to_owned())
            .collect()
    };
    assert_eq!(strings(&lists["optional"]), OPTIONAL_FIELDS);
    // A recording without optional members, written by the Rust structures.
    let (plain, _) = common::scripted_recording(3, 10);
    let text = plain.to_jsonl_string().unwrap();
    let mut lines = text.lines();
    let members = |line: &str, path: &[&str]| -> Vec<String> {
        let mut v: serde_json::Value = serde_json::from_str(line).unwrap();
        for key in path {
            v = v[*key].take();
        }
        let mut keys: Vec<String> = v.as_object().unwrap().keys().cloned().collect();
        keys.sort();
        keys
    };
    assert_eq!(
        members(lines.next().unwrap(), &[]),
        strings(&lists["header"])
    );
    let record = lines.next().unwrap();
    assert_eq!(members(record, &["player"]), strings(&lists["player"]));
    assert_eq!(members(record, &["player", "gun"]), strings(&lists["gun"]));
}

/// Removes the optional member `name` (one of [`OPTIONAL_FIELDS`]) from a
/// record's player part.
fn strip_member(p: &mut RawPlayer, name: &str) {
    match name {
        "player.base_eye_height" => p.base_eye_height = None,
        "player.base_level" => p.base_level = None,
        "player.bob" => p.bob = None,
        "player.camera_pov" => p.camera_pov = None,
        "player.cylinder" => p.cylinder = None,
        "player.floor" => p.floor = None,
        "player.fov_default" => p.fov_default = None,
        "player.fov_lock" => p.fov_lock = None,
        "player.fov_locked" => p.fov_locked = None,
        "player.gun.state" => {
            if let Some(g) = p.gun.as_mut() {
                g.state = None;
            }
        }
        "player.gun.timers" => {
            if let Some(g) = p.gun.as_mut() {
                g.timers = None;
            }
        }
        "player.walk_bob" => p.walk_bob = None,
        other => panic!("not an optional member: {other}"),
    }
}

/// `raw` without the optional members in `names`, in every record and in the
/// header's list.
fn without_members(raw: &RawFile, names: &[&str]) -> RawFile {
    let mut out = raw.clone();
    for p in out.records.iter_mut().filter_map(|r| r.player.as_mut()) {
        for name in names {
            strip_member(p, name);
        }
    }
    out.header
        .optional_fields
        .retain(|f| !names.contains(&f.as_str()));
    out
}

/// The fields a member gives the `state:` note (docs/TRACE_CAPTURE.md 3.5);
/// none for the members no conversion uses, and none for the camera's lock,
/// which is in the samples' FOV column.
fn state_keys_of(member: &str) -> &'static [&'static str] {
    match member {
        "player.base_eye_height" => &["base_eye_height"],
        "player.base_level" => &["base_level"],
        "player.cylinder" => &["cylinder_half_height", "cylinder_radius"],
        "player.floor" => &["floor"],
        "player.gun.state" => &["weapon_state"],
        "player.gun.timers" => &["gun_timers", "refire_count", "refire_rate"],
        _ => &[],
    }
}

/// The keys of recorder 0.2.0 that the `state:` notes of `segs` hold.
fn optional_state_keys(segs: &[asamu_trace::convert::Segment]) -> Vec<String> {
    let all: Vec<&str> = OPTIONAL_FIELDS
        .iter()
        .flat_map(|m| state_keys_of(m).iter().copied())
        .collect();
    let mut found = std::collections::BTreeSet::new();
    for seg in segs {
        let note = seg
            .trace
            .meta
            .notes
            .iter()
            .find_map(|n| n.strip_prefix(asamu_trace::state::STATE_NOTE_PREFIX))
            .unwrap();
        let v: serde_json::Value = serde_json::from_str(note).unwrap();
        for change in v["changes"].as_array().unwrap() {
            for key in change[1].as_object().unwrap().keys() {
                if all.contains(&key.as_str()) {
                    found.insert(key.clone());
                }
            }
        }
    }
    found.into_iter().collect()
}

fn samples_of(segs: &[asamu_trace::convert::Segment]) -> Vec<&[asamu_player::TraceSample]> {
    segs.iter().map(|s| s.trace.samples.as_slice()).collect()
}

/// Every optional member absent in turn, and every one alone: both
/// converters agree on each of the 24 recordings, and each member has
/// exactly the effect docs/TRACE_CAPTURE.md 3.3 and 3.5 give it.
///
/// - The samples depend on two members only, the camera's lock flag and its
///   locked FOV, and on both together: without either one no sample has a
///   locked FOV, so the samples are those of the same recording without any
///   optional member (the cached view FOV). Without any other member the
///   samples are those of the full recording.
/// - A member's fields are in the `state:` note exactly when a record
///   carries the member.
#[test]
fn python_and_rust_agree_with_each_optional_member_absent_or_alone() {
    let Some(py) = python() else {
        eprintln!("skipped: no python3");
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let mut full = common::varied_recording(51, 240, true);
    common::with_optional_members(&mut full, 51);
    let stats = common::recorder_stats(&full, 51);
    let opts = ConvertOptions {
        log: Some(RecorderLog::from_json_str(&stats).unwrap()),
        ..ConvertOptions::default()
    };
    assert_eq!(full.optional_counts().len(), OPTIONAL_FIELDS.len());
    let plain = without_members(&full, &OPTIONAL_FIELDS);
    assert!(plain.optional_counts().is_empty() && plain.header.optional_fields.is_empty());
    let with_all = convert(&full, &opts).unwrap();
    let with_none = convert(&plain, &opts).unwrap();
    assert!(with_all.len() >= 2);
    // The recording zooms: with the lock the samples are not the blind ones.
    assert_ne!(samples_of(&with_all), samples_of(&with_none));
    assert!(optional_state_keys(&with_none).is_empty());
    let every_key: Vec<String> = {
        let mut keys: Vec<String> = OPTIONAL_FIELDS
            .iter()
            .flat_map(|m| state_keys_of(m).iter().map(|k| (*k).to_owned()))
            .collect();
        keys.sort();
        keys
    };
    assert_eq!(optional_state_keys(&with_all), every_key);
    let lock = ["player.fov_lock", "player.fov_locked"];
    let lock_read = |segs: &[asamu_trace::convert::Segment]| {
        segs.iter()
            .flat_map(|s| &s.trace.meta.notes)
            .any(|n| n.starts_with(asamu_trace::convert::FOV_LOCK_NOTE_PREFIX))
    };
    assert!(lock_read(&with_all) && !lock_read(&with_none));

    for (i, member) in OPTIONAL_FIELDS.iter().enumerate() {
        // Absent in turn.
        let absent = without_members(&full, &[member]);
        let counts = absent.optional_counts();
        assert!(!counts.contains_key(member), "{member}");
        assert_eq!(counts.len(), OPTIONAL_FIELDS.len() - 1, "{member}");
        crosscheck_files(
            py,
            &absent,
            Some(&stats),
            dir.path(),
            &format!("absent{i}"),
            &ConvertOptions::default(),
        );
        let segs = convert(&absent, &opts).unwrap();
        let blind = lock.contains(member);
        assert_eq!(
            samples_of(&segs),
            samples_of(if blind { &with_none } else { &with_all }),
            "without {member}"
        );
        let mut want = every_key.clone();
        want.retain(|k| !state_keys_of(member).contains(&k.as_str()));
        assert_eq!(optional_state_keys(&segs), want, "without {member}");
        // The lock is read exactly when its flag is there.
        assert_eq!(
            lock_read(&segs),
            *member != "player.fov_locked",
            "without {member}"
        );

        // Alone.
        let others: Vec<&str> = OPTIONAL_FIELDS
            .iter()
            .copied()
            .filter(|m| m != member)
            .collect();
        let alone = without_members(&full, &others);
        assert_eq!(
            alone.optional_counts().keys().copied().collect::<Vec<_>>(),
            [*member]
        );
        assert_eq!(alone.header.optional_fields, [*member]);
        crosscheck_files(
            py,
            &alone,
            Some(&stats),
            dir.path(),
            &format!("alone{i}"),
            &ConvertOptions::default(),
        );
        let segs = convert(&alone, &opts).unwrap();
        // One member alone never shows a zoom: the flag has no locked FOV to
        // go with it, the locked FOV no flag.
        assert_eq!(samples_of(&segs), samples_of(&with_none), "{member} alone");
        let want: Vec<String> = state_keys_of(member)
            .iter()
            .map(|k| (*k).to_owned())
            .collect();
        assert_eq!(optional_state_keys(&segs), want, "{member} alone");
        assert_eq!(
            lock_read(&segs),
            *member == "player.fov_locked",
            "{member} alone"
        );
    }

    // What the header says about the recorder and the members decides
    // nothing: the records do. A header that names an older recorder, one
    // that announces nothing, and one that announces members no record has.
    let mut old_name = full.clone();
    old_name.header.recorder = "asamu_win 0.1.0".to_owned();
    crosscheck_files(
        py,
        &old_name,
        Some(&stats),
        dir.path(),
        "old-name",
        &ConvertOptions::default(),
    );
    let segs = convert(&old_name, &opts).unwrap();
    assert_eq!(samples_of(&segs), samples_of(&with_all));
    assert_eq!(optional_state_keys(&segs), every_key);

    let mut unannounced = full.clone();
    unannounced.header.optional_fields.clear();
    unannounced.header.optional_layout = None;
    crosscheck_files(
        py,
        &unannounced,
        Some(&stats),
        dir.path(),
        "unannounced",
        &ConvertOptions::default(),
    );
    assert_eq!(convert(&unannounced, &opts).unwrap(), with_all);

    let mut announced_only = plain.clone();
    announced_only.header.optional_fields = full.header.optional_fields.clone();
    announced_only.header.optional_layout = full.header.optional_layout.clone();
    crosscheck_files(
        py,
        &announced_only,
        Some(&stats),
        dir.path(),
        "announced-only",
        &ConvertOptions::default(),
    );
    assert_eq!(convert(&announced_only, &opts).unwrap(), with_none);
}

/// Recordings with the optional members of recorder 0.2.0 and its sidecar
/// (markers, the dropped-frame log): both converters write the same traces,
/// samples bit for bit, notes text for text.
#[test]
fn python_and_rust_agree_on_recorder_0_2_0_recordings() {
    let Some(py) = python() else {
        eprintln!("skipped: no python3");
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let mut seen = std::collections::BTreeSet::new();
    let wanted = [
        "fov: camera FOV lock read",
        "fov: cached view FOV",
        "fov: controller FOVAngle used",
        "check: the camera's FOV lock is set without a usable LockedFOV",
        "check: the weapon's refire timer is paused",
        "without a record before this run",
        "without a record after this run",
        "whose record was written without its frame's input",
        "no entry in the recorder's dropped-frame log",
        "frames were not logged: the log was full",
        " marker(s) (set with the recorder's mark command",
        "(set before the first record)",
        "(set before this run, at frame ",
        "(set after this run, at frame ",
        "markers: {",
        "\"refire_count\":",
        "\"refire_rate\":null",
        "\"floor\":[",
        "\"weapon_state\":\"WeaponFiring\"",
        "\"weapon_state\":null",
        "\"base_level\":\"freds_place\"",
        "\"cylinder_half_height\":",
        "\"gun_timers\":0",
    ];
    let mut segments = 0;
    let mut zoomed = 0;
    for (i, seed) in [41_u64, 42, 43, 44, 45, 46].into_iter().enumerate() {
        let mut raw = common::varied_recording(seed, 400, i % 2 == 0);
        common::with_optional_members(&mut raw, seed);
        if i >= 3 {
            common::with_accelerations(&mut raw, seed);
            common::with_frame_lengths(&mut raw, seed);
        }
        let stats = common::recorder_stats(&raw, seed);
        let opts = ConvertOptions {
            move_input: if i >= 3 {
                MoveInput::Acceleration
            } else {
                MoveInput::Auto
            },
            ..ConvertOptions::default()
        };
        let stem = format!("optional{seed}");
        segments += crosscheck_files(py, &raw, Some(&stats), dir.path(), &stem, &opts);
        // The same recording without its sidecar, and with the sidecar the
        // Python converter is told to ignore.
        crosscheck_files(py, &raw, None, dir.path(), &format!("bare{seed}"), &opts);
        let log = RecorderLog::from_json_str(&stats).unwrap();
        let with_log = ConvertOptions {
            log: Some(log),
            ..opts.clone()
        };
        let segs = convert(&raw, &with_log).unwrap();
        // The log's no-input records split runs that the frame numbers
        // alone would keep whole.
        assert!(segs.len() >= convert(&raw, &opts).unwrap().len());
        for seg in &segs {
            for n in &seg.trace.meta.notes {
                for w in wanted {
                    if n.contains(w) {
                        seen.insert(w);
                    }
                }
            }
        }
        // The zoom is in the samples: where a record's lock is set with a
        // usable locked FOV, the sample has that FOV.
        for seg in &segs {
            let first = raw
                .records
                .iter()
                .position(|r| r.frame == seg.first_frame)
                .unwrap();
            for (k, s) in seg.trace.samples.iter().enumerate() {
                let p = raw.records[first + k].player.as_ref().unwrap();
                if let (Some(true), Some(lock)) = (p.fov_locked, p.fov_lock)
                    && lock > 0.0
                    && lock < 180.0
                {
                    assert_eq!(
                        s.fov,
                        lock,
                        "seed {seed}, frame {}",
                        raw.records[first + k].frame
                    );
                    zoomed += 1;
                }
            }
        }
    }
    assert!(segments >= 12, "only {segments} segments exercised");
    assert!(zoomed > 200, "only {zoomed} zoomed samples exercised");
    let missing: Vec<&str> = wanted
        .iter()
        .filter(|w| !seen.contains(*w))
        .copied()
        .collect();
    assert!(missing.is_empty(), "not exercised: {missing:#?}");

    // The Python converter without the sidecar (`--no-stats`) and with one
    // named on the command line (`--stats`) is the Rust one without and with.
    let mut raw = common::varied_recording(47, 300, true);
    common::with_optional_members(&mut raw, 47);
    let stats = common::recorder_stats(&raw, 47);
    let raw_path = dir.path().join("named.raw.jsonl");
    std::fs::write(&raw_path, raw.to_jsonl_string().unwrap()).unwrap();
    std::fs::write(dir.path().join("named.stats.json"), &stats).unwrap();
    let elsewhere = dir.path().join("elsewhere.json");
    std::fs::write(&elsewhere, &stats).unwrap();
    let without = convert(&raw, &ConvertOptions::default()).unwrap();
    let python = common::python_convert(
        py,
        &raw_path,
        &dir.path().join("named-no-stats"),
        &["--no-stats".as_ref()],
        without.len(),
    );
    common::same_traces(&python, &without);
    let with = convert(
        &raw,
        &ConvertOptions {
            log: Some(RecorderLog::from_json_str(&stats).unwrap()),
            ..ConvertOptions::default()
        },
    )
    .unwrap();
    assert_ne!(
        with.iter().map(|s| &s.trace.meta.notes).collect::<Vec<_>>(),
        without
            .iter()
            .map(|s| &s.trace.meta.notes)
            .collect::<Vec<_>>()
    );
    let python = common::python_convert(
        py,
        &raw_path,
        &dir.path().join("named-stats"),
        &["--stats".as_ref(), elsewhere.as_os_str()],
        with.len(),
    );
    common::same_traces(&python, &with);

    // The budget of the state note: a refire count and a floor normal that
    // change on every tick of a long run. 14,000 changes of the count cost
    // 728,000 bytes, more than the 720,000 there are: left out. The floor's
    // 7,000 changes (700,000 bytes) then fit. Both converters agree.
    let mut long = common::varied_recording(48, 60, true);
    let template = long
        .records
        .iter()
        .find(|r| {
            r.world.as_ref().is_some_and(|w| !w.paused)
                && r.player.as_ref().is_some_and(|p| p.gun.is_some())
        })
        .unwrap()
        .clone();
    long.records = (0..=14_000_u64)
        .map(|i| {
            let mut r = template.clone();
            r.frame = 10 + i;
            let p = r.player.as_mut().unwrap();
            p.eye_height = Some(38.0);
            p.floor = Some(if i <= 7_000 && i % 2 == 1 {
                asamu_core::glam::Vec3::new(0.6, 0.0, 0.8)
            } else {
                asamu_core::glam::Vec3::Z
            });
            p.gun.as_mut().unwrap().timers = Some(vec![RawTimer {
                name: "RefireCheckTimer".into(),
                rate: 0.1,
                count: (i % 6) as f32 / 60.0,
                looping: true,
                paused: false,
            }]);
            r
        })
        .collect();
    assert_eq!(crosscheck(py, &long, dir.path(), "budget"), 1);
    let t = &convert(&long, &ConvertOptions::default()).unwrap()[0].trace;
    assert!(
        t.meta.notes.iter().any(|n| n
            == "check: the refire timer's count left out of the state: note (14000 changes of \
                52 bytes; 720000 of the 720000 bytes for values that change on most ticks were \
                left)"),
        "{:#?}",
        &t.meta.notes[..t.meta.notes.len().saturating_sub(2)]
    );
    let state = t
        .meta
        .notes
        .iter()
        .find(|n| n.starts_with("state: "))
        .unwrap();
    assert!(state.contains("\"floor\":[") && !state.contains("refire_count"));
    assert!(state.contains("\"refire_rate\":"), "the timer's rate stays");
}
