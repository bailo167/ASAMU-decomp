//! The Sandbox recording container: a recording made in a session is its
//! own file format, tagged inside and out, and never a parity trace.
#![allow(clippy::unwrap_used)]

use std::collections::BTreeMap;

use asamu_game::Game;
use asamu_player::{InputFrame, Trace};
use asamu_sandbox::command::{Command, CommandError, TimeOp, Toggle};
use asamu_sandbox::keys::TuneValue;
use asamu_sandbox::profile::Profile;
use asamu_sandbox::recording::{
    MAX_HEADER_ACTION_BYTES, RECORDING_FORMAT, RECORDING_SUFFIX, RECORDING_VERSION, RecordingError,
    SandboxRecording, TRACE_LEVEL_PREFIX, TRACE_NOTE_NOT_PARITY,
};
use asamu_sandbox::rules::{GrappleRule, Rules, Switch};
use asamu_sandbox::session::{Outcome, Session, SimCx};

fn started_graybox() -> Game {
    let mut game = Game::graybox().unwrap();
    game.start();
    game
}

fn forward() -> InputFrame {
    InputFrame {
        move_forward: 1.0,
        ..InputFrame::default()
    }
}

fn exec(session: &mut Session, game: &mut Game, cmd: Command) -> Result<Outcome, CommandError> {
    let mut script = None;
    session.execute(
        cmd,
        &mut SimCx {
            game,
            script: &mut script,
        },
    )
}

fn record(on: Toggle) -> Command {
    Command::Record { on }
}

fn run(game: &mut Game, ticks: usize) {
    for _ in 0..ticks {
        game.tick(&forward()).unwrap();
    }
}

fn to_text(recording: &SandboxRecording) -> String {
    let mut bytes = Vec::new();
    recording.write_jsonl(&mut bytes).unwrap();
    String::from_utf8(bytes).unwrap()
}

/// A recording of a session that tuned, set rules and used time control.
fn busy_recording() -> (SandboxRecording, Session, Game) {
    let mut game = started_graybox();
    let mut session = Session::classic();
    let scaling = f64::from(game.params().movement.custom_gravity_scaling.value);
    let rules = Rules {
        grapples: GrappleRule::Unlimited,
        rocket_boots: Switch::On,
        auto_refill: true,
    };
    for cmd in [
        Command::SetParam {
            key: "movement.custom_gravity_scaling".to_owned(),
            value: TuneValue::Float(scaling * 0.5),
        },
        Command::SetRules { rules },
        Command::Time {
            op: TimeOp::Scale { value: 0.5 },
        },
    ] {
        exec(&mut session, &mut game, cmd).unwrap();
    }
    run(&mut game, 12);
    exec(&mut session, &mut game, record(Toggle::On)).unwrap();
    run(&mut game, 30);
    exec(&mut session, &mut game, Command::RefillGrapples).unwrap();
    run(&mut game, 30);
    let outcome = exec(&mut session, &mut game, record(Toggle::Off)).unwrap();
    assert!(!game.is_recording());
    (
        outcome.recording.expect("a finished recording"),
        session,
        game,
    )
}

#[test]
fn header_fields() {
    let (recording, session, game) = busy_recording();
    let header = &recording.header;
    assert_eq!(header.format, RECORDING_FORMAT);
    assert_eq!(header.version, RECORDING_VERSION);
    assert!(header.not_parity);
    assert_eq!(header.level, game.level().name);
    assert_eq!(header.param_set, "modified");
    assert_eq!(header.profile, session.profile().name);
    assert_eq!(header.overrides, *session.overlay());
    assert_eq!(header.overrides.len(), 1);
    assert_eq!(header.rules, *session.rules());
    assert!(header.time_control_used);
    assert!(!header.actions_truncated);
    // Every command up to the end of the recording, with its tick.
    let logged: Vec<(u64, &str)> = header
        .actions
        .iter()
        .map(|a| (a.tick, a.cmd.name()))
        .collect();
    assert_eq!(
        logged,
        [
            (0, "set_param"),
            (0, "set_rules"),
            (0, "time"),
            (12, "record"),
            (42, "refill_grapples"),
        ]
    );
    // The embedded trace: the initial sample and one per tick.
    assert_eq!(recording.trace.samples.len(), 61);
    assert_eq!(recording.trace.samples.first().map(|s| s.tick), Some(12));
    assert_eq!(recording.trace.samples.last().map(|s| s.tick), Some(72));
    let name = recording.file_name();
    assert!(
        name.starts_with("asamu-sandbox-graybox-test-hand-made-"),
        "{name}"
    );
    assert!(
        name.ends_with(&format!("-tick72{RECORDING_SUFFIX}")),
        "{name}"
    );
}

#[test]
fn round_trip() {
    let (recording, _, _) = busy_recording();
    let text = to_text(&recording);
    // Line 1 the header, line 2 the trace's meta line, then the samples.
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 2 + recording.trace.samples.len());
    assert!(
        lines[0]
            .starts_with(r#"{"format":"asamu-sandbox-recording","version":1,"not_parity":true,"#)
    );
    assert!(lines[1].starts_with(r#"{"format":"asamu-trace""#));

    let read = SandboxRecording::read_jsonl(text.as_bytes()).unwrap();
    assert_eq!(read, recording);
    // Writing what was read gives the same bytes.
    assert_eq!(to_text(&read), text);
    // Blank lines and CRLF are tolerated, as the trace reader tolerates them.
    let spaced = format!("\n\n{}", text.replace('\n', "\r\n"));
    assert_eq!(
        SandboxRecording::read_jsonl(spaced.as_bytes()).unwrap(),
        recording
    );
}

#[test]
fn pristine_sessions_are_tagged_too() {
    // No override, no rule, no command: the recording is still a Sandbox
    // recording and still not a parity trace.
    let mut game = started_graybox();
    let session = Session::classic();
    assert!(session.is_pristine());
    game.start_recording();
    run(&mut game, 20);
    let trace = game.stop_recording().unwrap();
    let untouched = trace.clone();
    let recording = SandboxRecording::finish(trace, &session);

    assert!(recording.header.not_parity);
    assert_eq!(recording.header.param_set, "classic");
    assert_eq!(recording.header.profile, "classic");
    assert!(recording.header.overrides.is_empty());
    assert!(recording.header.rules.is_default());
    assert!(!recording.header.time_control_used);
    assert!(recording.header.actions.is_empty());

    let meta = &recording.trace.meta;
    let level = meta.level.as_deref().unwrap();
    assert!(level.starts_with(TRACE_LEVEL_PREFIX), "{level}");
    assert_eq!(
        level.strip_prefix(TRACE_LEVEL_PREFIX),
        untouched.meta.level.as_deref()
    );
    assert!(meta.notes.iter().any(|n| n == TRACE_NOTE_NOT_PARITY));
    // The Classic set did run, and the note about it is left as recorded.
    for note in &untouched.meta.notes {
        assert!(meta.notes.contains(note), "{note}");
    }
    // Only the tags were added: the samples are the game's, bit for bit.
    assert_eq!(recording.trace.samples, untouched.samples);

    // The parity reader refuses the container.
    let text = to_text(&recording);
    assert!(Trace::from_jsonl_str(&text).is_err());
    // Tagging twice changes nothing more.
    let again = SandboxRecording::finish(recording.trace.clone(), &session);
    assert_eq!(again, recording);
}

#[test]
fn a_tuned_recording_is_never_labelled_original() {
    let original_note = |trace: &Trace| {
        trace
            .meta
            .notes
            .iter()
            .any(|n| n.starts_with("parameters: original"))
    };
    // The game's own recorder labels by the script layer being present, so
    // a tuned game would say "original".
    let (recording, _, _) = busy_recording();
    assert_eq!(recording.header.param_set, "modified");
    assert!(!original_note(&recording.trace));
    assert!(
        recording
            .trace
            .meta
            .notes
            .iter()
            .any(|n| n.starts_with("parameters: MODIFIED")),
        "{:?}",
        recording.trace.meta.notes
    );

    // Tuned for part of the recording and back to Classic before its end:
    // still not a Classic recording.
    let mut game = started_graybox();
    let mut session = Session::classic();
    exec(&mut session, &mut game, record(Toggle::On)).unwrap();
    run(&mut game, 10);
    exec(
        &mut session,
        &mut game,
        Command::SetParam {
            key: "pawn.zoom_enabled".to_owned(),
            value: TuneValue::Bool(false),
        },
    )
    .unwrap();
    run(&mut game, 10);
    exec(&mut session, &mut game, Command::ResetAllParams).unwrap();
    run(&mut game, 10);
    assert!(session.recording_ran_modified());
    let recording = exec(&mut session, &mut game, record(Toggle::Off))
        .unwrap()
        .recording
        .unwrap();
    assert!(recording.header.overrides.is_empty(), "Classic at the end");
    assert_eq!(recording.header.param_set, "modified");
    assert!(!original_note(&recording.trace));

    // Tuned and reset before the recording started: that recording did run
    // on the Classic set throughout (the commands are in its action list).
    exec(&mut session, &mut game, record(Toggle::On)).unwrap();
    run(&mut game, 10);
    let recording = exec(&mut session, &mut game, record(Toggle::Off))
        .unwrap()
        .recording
        .unwrap();
    assert_eq!(recording.header.param_set, "classic");
    assert!(original_note(&recording.trace));
    assert!(recording.header.actions.len() >= 4);
    assert!(recording.header.not_parity);
    assert!(
        recording
            .trace
            .meta
            .level
            .as_deref()
            .is_some_and(|l| l.starts_with(TRACE_LEVEL_PREFIX))
    );
}

#[test]
fn the_embedded_trace_is_an_ordinary_trace_that_says_sandbox() {
    let (recording, _, _) = busy_recording();
    let text = to_text(&recording);
    // The whole file is not a trace...
    assert!(Trace::from_jsonl_str(&text).is_err());
    // ...its tail is, and that trace still says what it is.
    let tail = text.split_once('\n').unwrap().1;
    let trace = Trace::from_jsonl_str(tail).unwrap();
    assert_eq!(trace, recording.trace);
    assert!(
        trace
            .meta
            .level
            .as_deref()
            .is_some_and(|l| l.starts_with(TRACE_LEVEL_PREFIX))
    );
    assert!(trace.meta.notes.iter().any(|n| n == TRACE_NOTE_NOT_PARITY));
}

#[test]
fn session_end_returns_a_running_recording() {
    let mut game = started_graybox();
    let mut session = Session::classic();
    let mut script = None;
    // Nothing running: nothing returned.
    assert!(
        session
            .finish(&mut SimCx {
                game: &mut game,
                script: &mut script
            })
            .is_none()
    );
    exec(&mut session, &mut game, record(Toggle::Toggle)).unwrap();
    assert!(game.is_recording());
    run(&mut game, 5);
    let recording = session
        .finish(&mut SimCx {
            game: &mut game,
            script: &mut script,
        })
        .expect("the running recording");
    assert!(!game.is_recording());
    assert_eq!(recording.trace.samples.len(), 6);
    assert!(recording.header.not_parity);
    // Toggling when nothing runs starts one; "off" twice is not an error.
    exec(&mut session, &mut game, record(Toggle::Off)).unwrap();
    let outcome = exec(&mut session, &mut game, record(Toggle::Off)).unwrap();
    assert!(outcome.recording.is_none());
    assert!(!game.is_recording());
}

#[test]
fn file_names_are_safe_and_carry_the_suffix() {
    let (mut recording, _, _) = busy_recording();
    for level in [
        "sandbox arena: movement-lab (hand-made, not original content)",
        "",
        "../../etc/x",
        "A B\tC\\D:E*F?\"G<H>I|J",
        "\u{e9}\u{4e16}\u{754c}",
        &"x".repeat(500),
    ] {
        recording.header.level = level.to_owned();
        let name = recording.file_name();
        assert!(name.starts_with("asamu-sandbox-"), "{name}");
        assert!(name.ends_with(RECORDING_SUFFIX), "{name}");
        let stem = name.strip_suffix(RECORDING_SUFFIX).unwrap();
        assert!(
            stem.bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-'),
            "{name}"
        );
        assert!(name.len() < 100, "{name}");
        assert!(name.contains("-tick72"), "{name}");
    }
    recording.header.level = String::new();
    assert_eq!(
        recording.file_name(),
        format!("asamu-sandbox-level-tick72{RECORDING_SUFFIX}")
    );
}

#[test]
fn hostile_input_is_refused_without_panicking() {
    let (recording, _, _) = busy_recording();
    let text = to_text(&recording);
    let (header, tail) = text.split_once('\n').unwrap();
    let read = |s: &str| SandboxRecording::read_jsonl(s.as_bytes());

    // Not a recording at all.
    for bad in ["", "\n\n", "not json\n", "[]\n", "42\n", "{}\n"] {
        assert!(
            matches!(read(bad), Err(RecordingError::NotARecording(_))),
            "{bad:?}"
        );
    }
    // A parity trace is not a recording (its first line is trace metadata).
    assert!(matches!(read(tail), Err(RecordingError::NotARecording(_))));
    // Another version, named as such even if its fields differ.
    let later = header.replacen(r#""version":1"#, r#""version":2,"new_field":true"#, 1);
    assert_eq!(
        read(&format!("{later}\n{tail}")).err(),
        Some(RecordingError::UnsupportedVersion(2))
    );
    // A recording never claims to be a parity run.
    let claims = header.replacen(r#""not_parity":true"#, r#""not_parity":false"#, 1);
    assert!(matches!(
        read(&format!("{claims}\n{tail}")),
        Err(RecordingError::NotARecording(_))
    ));
    // Unknown and missing header fields.
    let extra = header.replacen(r#""version":1"#, r#""version":1,"surprise":1"#, 1);
    assert!(matches!(
        read(&format!("{extra}\n{tail}")),
        Err(RecordingError::NotARecording(_))
    ));
    let missing = header.replacen(r#""not_parity":true,"#, "", 1);
    assert!(matches!(
        read(&format!("{missing}\n{tail}")),
        Err(RecordingError::NotARecording(_))
    ));
    // Not UTF-8.
    let mut bytes = vec![0xFF, 0xFE, b'\n'];
    bytes.extend_from_slice(tail.as_bytes());
    assert!(matches!(
        SandboxRecording::read_jsonl(bytes.as_slice()),
        Err(RecordingError::NotARecording(_))
    ));

    // A header without a trace, and a damaged trace.
    assert!(matches!(
        read(&format!("{header}\n")),
        Err(RecordingError::Trace(_))
    ));
    // Cut in the middle of a line (never exactly at the end of one).
    let cut = tail[..tail.len() / 2].trim_end_matches('\n');
    let cut = &cut[..cut.len() - 1];
    assert!(matches!(
        read(&format!("{header}\n{cut}")),
        Err(RecordingError::Trace(_))
    ));
    let garbage = format!("{header}\n{tail}not a sample\n");
    assert!(matches!(read(&garbage), Err(RecordingError::Trace(_))));

    // An embedded trace without the Sandbox tags (a parity-looking trace
    // put under a recording header) is refused, in both directions.
    let mut plain = started_graybox();
    plain.start_recording();
    run(&mut plain, 5);
    let untagged = plain.stop_recording().unwrap();
    let smuggled = format!("{header}\n{}", untagged.to_jsonl_string().unwrap());
    assert!(matches!(read(&smuggled), Err(RecordingError::Trace(_))));
    let forged = SandboxRecording {
        header: recording.header.clone(),
        trace: untagged,
    };
    let mut out = Vec::new();
    assert!(matches!(
        forged.write_jsonl(&mut out),
        Err(RecordingError::Trace(_))
    ));
    assert!(out.is_empty(), "nothing is written for a refused recording");

    // Every truncation of a valid file is an error or a shorter recording,
    // never a panic.
    for end in (0..text.len()).step_by(97) {
        if let Some(prefix) = text.get(..end) {
            let _ = read(prefix);
        }
    }
}

#[test]
fn a_header_that_is_not_a_sandbox_header_is_not_written() {
    let (recording, _, _) = busy_recording();
    let mut wrong_format = recording.clone();
    wrong_format.header.format = "asamu-trace".to_owned();
    let mut wrong_version = recording.clone();
    wrong_version.header.version = RECORDING_VERSION + 1;
    let mut claims_parity = recording.clone();
    claims_parity.header.not_parity = false;
    for (bad, expected_version) in [
        (wrong_format, None),
        (wrong_version, Some(RECORDING_VERSION + 1)),
        (claims_parity, None),
    ] {
        let mut out = Vec::new();
        let result = bad.write_jsonl(&mut out);
        match expected_version {
            Some(v) => assert_eq!(result, Err(RecordingError::UnsupportedVersion(v))),
            None => assert!(matches!(result, Err(RecordingError::NotARecording(_)))),
        }
        assert!(out.is_empty());
    }
}

#[test]
fn an_oversized_action_list_is_cut_and_the_header_says_so() {
    // Profiles may carry free-form extensions; thirty large ones in the log
    // exceed the header's budget.
    let mut game = started_graybox();
    let mut session = Session::classic();
    let mut extensions = BTreeMap::new();
    extensions.insert(
        "blob".to_owned(),
        serde_json::Value::String("x".repeat(MAX_HEADER_ACTION_BYTES / 20)),
    );
    let profile = Profile {
        extensions,
        ..Profile::classic()
    };
    let loads = 30;
    for _ in 0..loads {
        exec(
            &mut session,
            &mut game,
            Command::LoadProfile {
                profile: Box::new(profile.clone()),
            },
        )
        .unwrap();
    }
    assert_eq!(session.log().len(), loads);
    assert!(!session.log().truncated());
    exec(&mut session, &mut game, record(Toggle::On)).unwrap();
    run(&mut game, 3);
    let recording = exec(&mut session, &mut game, record(Toggle::Off))
        .unwrap()
        .recording
        .unwrap();
    assert!(recording.header.actions_truncated);
    assert!(recording.header.actions.len() < loads);
    assert!(!recording.header.actions.is_empty());
    // Still a file this build reads back.
    let text = to_text(&recording);
    let header_line = text.lines().next().unwrap();
    assert!(header_line.len() <= MAX_HEADER_ACTION_BYTES + 4096);
    assert_eq!(
        SandboxRecording::read_jsonl(text.as_bytes()).unwrap(),
        recording
    );
}
