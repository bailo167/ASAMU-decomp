//! Hostile and malformed inputs: the graph and Matinee parsers and the
//! interpreter must reject or contain bad data without panicking.

#![allow(clippy::unwrap_used)]

use std::sync::Arc;

use asamu_kismet::graph::Graph;
use asamu_kismet::matinee::MatineeSet;
use asamu_kismet::{NullHost, Runtime};
use serde_json::json;

fn valid() -> serde_json::Value {
    json!({
        "format": asamu_kismet::RUNTIME_FORMAT, "version": asamu_kismet::RUNTIME_VERSION, "package": "T",
        "nodes": [
            {"id": 0, "class": "Engine.Sequence", "kind": "sequence", "members": [1, 2, 3]},
            {"id": 1, "class": "Engine.SeqEvent_LevelLoaded", "kind": "event", "parent": 0,
             "outputs": [{"desc": "Loaded and Visible", "links": [{"op": 2, "input": 0}]}],
             "event": {"max_trigger_count": 0}},
            {"id": 2, "class": "Engine.SeqAct_Delay", "kind": "action", "parent": 0,
             "inputs": [{"desc": "Start"}, {"desc": "Stop"}, {"desc": "Pause"}],
             "outputs": [{"desc": "Finished", "delay": 0.1, "links": [{"op": 2, "input": 0}]}],
             "variables": [{"desc": "Duration", "property": "Duration", "vars": [3]}],
             "params": {"Duration": 0.0, "DefaultDuration": 0.05, "bStartWillRestart": true},
             "latent": true, "latent_base": true},
            {"id": 3, "class": "Engine.SeqVar_Float", "kind": "variable", "parent": 0, "var": {"value": 0.05}}
        ]
    })
}

/// A tiny deterministic generator for mutations.
struct Lcg(u64);

impl Lcg {
    fn next(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1);
        self.0 >> 33
    }
}

#[test]
fn truncated_and_corrupted_graphs_never_panic() {
    let bytes = serde_json::to_vec(&valid()).unwrap();
    for cut in 0..bytes.len() {
        let _ = Graph::from_json_slice(&bytes[..cut]);
    }
    let mut rng = Lcg(42);
    for _ in 0..2000 {
        let mut b = bytes.clone();
        for _ in 0..(1 + rng.next() % 4) {
            let i = (rng.next() as usize) % b.len();
            b[i] = (rng.next() & 0xFF) as u8;
        }
        if let Ok(g) = Graph::from_json_slice(&b) {
            // Whatever parses also runs.
            let mut r = Runtime::new(Arc::new(g), Arc::new(MatineeSet::default()));
            for _ in 0..30 {
                r.tick(1.0 / 60.0, &mut NullHost);
            }
        }
    }
}

#[test]
fn hostile_values_are_contained() {
    let mut v = valid();
    // Out-of-range ids, self-parenting, cycles through members, bad link
    // inputs, an absurd delay, a negative trigger count.
    v["nodes"][1]["parent"] = json!(1);
    v["nodes"][0]["members"] = json!([0, 1, 2, 3, 99, 1, 1]);
    v["nodes"][2]["outputs"][0]["links"] =
        json!([{"op": 2, "input": 9}, {"op": 1, "input": 0}, {"op": 7}]);
    v["nodes"][2]["inputs"][0]["delay"] = json!(1.0e30);
    v["nodes"][1]["event"]["max_trigger_count"] = json!(-5);
    let g = Graph::from_json_slice(&serde_json::to_vec(&v).unwrap()).unwrap();
    assert!(!g.warnings.is_empty());
    let mut r = Runtime::new(Arc::new(g), Arc::new(MatineeSet::default()));
    for _ in 0..120 {
        r.tick(1.0 / 60.0, &mut NullHost);
    }
    // Non-finite and negative frame times are treated as zero.
    r.tick(f32::NAN, &mut NullHost);
    r.tick(-1.0, &mut NullHost);
    // A self-feeding delay loop keeps running without growing state.
    let mut ok = valid();
    ok["nodes"][2]["outputs"][0]["delay"] = json!(0.0);
    let g = Graph::from_json_slice(&serde_json::to_vec(&ok).unwrap()).unwrap();
    let mut r = Runtime::new(Arc::new(g), Arc::new(MatineeSet::default()));
    for _ in 0..600 {
        r.tick(1.0 / 60.0, &mut NullHost);
    }
    assert!(r.activate_count(2) >= 1);
}

#[test]
fn malformed_matinee_files_are_rejected_or_ignored() {
    let mut m = MatineeSet::default();
    assert!(m.add_json(b"[", 0).is_err());
    assert!(
        m.add_json(br#"{"format": "asamu-matinee", "version": 2}"#, 0)
            .is_err()
    );
    // Unknown track types parse as `Other`; missing data is skipped.
    m.add_json(
        br#"{"format": "asamu-matinee", "version": 1, "actions": [{"node": 5, "scope": "level", "interp_data": "X"}],
             "interp_data": [{"path": "Y", "groups": [{"tracks": [{"data": {"type": "brand_new"}}]}]}]}"#,
        usize::MAX,
    )
    .unwrap();
    assert!(m.data_of(4).is_none());
}

/// Runs `f` on a thread and fails if it does not finish in time (a hang in
/// the interpreter must fail the test, not stall the suite).
fn within(seconds: u64, f: impl FnOnce() + Send + 'static) {
    let (tx, rx) = std::sync::mpsc::channel();
    let worker = std::thread::spawn(move || {
        f();
        let _ = tx.send(());
    });
    match rx.recv_timeout(std::time::Duration::from_secs(seconds)) {
        Ok(()) => {}
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
            panic!("interpreter did not finish (hang)")
        }
        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
            // The worker panicked: re-raise its panic here.
            if let Err(e) = worker.join() {
                std::panic::resume_unwind(e);
            }
        }
    }
}

/// An event whose class flags claim latent execution, activated many times
/// in one update, must not spin: the unqueued activation of an event that
/// is still pending re-queues it and the update ends.
#[test]
fn a_latent_flagged_event_activated_repeatedly_does_not_hang() {
    within(20, || {
        let mut nodes = vec![
            json!({"id": 0, "class": "Engine.Sequence", "kind": "sequence",
                   "members": (1..=12).collect::<Vec<usize>>()}),
            json!({"id": 1, "class": "Engine.SeqEvent_LevelLoaded", "kind": "event", "parent": 0,
                   "outputs": [{"desc": "Loaded and Visible",
                                "links": (2..=11).map(|i| json!({"op": i, "input": 0})).collect::<Vec<_>>()}],
                   "event": {"max_trigger_count": 0}}),
        ];
        for i in 2..=11 {
            nodes.push(
                json!({"id": i, "class": "Engine.SeqAct_ActivateRemoteEvent", "kind": "action",
                              "parent": 0, "inputs": [{"desc": "In"}], "outputs": [{"desc": "Out"}],
                              "params": {"EventName": "Go"}, "auto_activate_outputs": true}),
            );
        }
        nodes.push(
            json!({"id": 12, "class": "Engine.SeqEvent_RemoteEvent", "kind": "event", "parent": 0,
                          "outputs": [{"desc": "Out"}], "params": {"EventName": "Go"},
                          "latent": true, "latent_base": true}),
        );
        let doc = json!({"format": asamu_kismet::RUNTIME_FORMAT, "version": asamu_kismet::RUNTIME_VERSION,
                         "package": "T", "nodes": nodes});
        let g = Graph::from_json_slice(&serde_json::to_vec(&doc).unwrap()).unwrap();
        let mut r = Runtime::new(Arc::new(g), Arc::new(MatineeSet::default()));
        // The level-loaded event (unlimited trigger count) fires at begin
        // play and again at match start: 20 remote activations. The pending
        // ones are delivered one per update.
        for _ in 0..40 {
            let before = r.activate_count(12);
            r.tick(1.0 / 60.0, &mut NullHost);
            assert!(r.activate_count(12) - before <= 2);
        }
        assert_eq!(r.activate_count(12), 20);
        assert_eq!(r.stats().step_limit_hits, 0);
    });
}

/// Self-attached and mutually attached actors moved by Matinee: the
/// attachment carry is depth-limited and terminates.
#[test]
fn cyclic_attachments_terminate() {
    within(20, || {
        let p = |n: &str| format!("T.TheWorld.PersistentLevel.{n}");
        let doc = json!({
            "format": asamu_kismet::RUNTIME_FORMAT, "version": asamu_kismet::RUNTIME_VERSION, "package": "T",
            "nodes": [
                {"id": 0, "class": "Engine.Sequence", "kind": "sequence", "members": [1, 2, 3, 4]},
                {"id": 1, "class": "Engine.SeqEvent_LevelLoaded", "kind": "event", "parent": 0,
                 "outputs": [{"desc": "Loaded and Visible", "links": [{"op": 2, "input": 0}]}]},
                {"id": 2, "class": "Engine.SeqAct_Interp", "kind": "action", "parent": 0,
                 "inputs": [{"desc": "Play"}, {"desc": "Reverse"}, {"desc": "Stop"}, {"desc": "Pause"}, {"desc": "Change Dir"}],
                 "outputs": [{"desc": "Completed"}, {"desc": "Reversed"}],
                 "variables": [{"desc": "Data", "vars": [3]}, {"desc": "G", "vars": [4]}],
                 "latent": true, "latent_base": true},
                {"id": 3, "path": "T.Data", "class": "Engine.InterpData", "kind": "variable", "parent": 0},
                {"id": 4, "class": "Engine.SeqVar_Object", "kind": "variable", "parent": 0,
                 "var": {"value": {"$obj": p("A")}}}
            ],
            "actors": [
                {"path": p("A"), "name": "A", "class": "Engine.InterpActor", "package": "T", "slot": 1, "base": p("B")},
                {"path": p("B"), "name": "B", "class": "Engine.InterpActor", "package": "T", "slot": 2, "base": p("A")},
                {"path": p("C"), "name": "C", "class": "Engine.InterpActor", "package": "T", "slot": 3, "base": p("C")}
            ]
        });
        let key = |t: f32, x: f32| json!({"in": t, "out": [x, 0.0, 0.0], "arrive": [0.0, 0.0, 0.0], "leave": [0.0, 0.0, 0.0], "mode": "linear"});
        let mut m = MatineeSet::default();
        m.add_json(
            &serde_json::to_vec(&json!({
                "format": "asamu-matinee", "version": 1, "package": "T",
                "actions": [{"path": "T.I", "node": 2, "scope": "level", "interp_data": "T.Data",
                             "bindings": [{"link": 1, "label": "G", "group": "G",
                                           "targets": [{"variable": "T.V", "object": p("A")}]}]}],
                "interp_data": [{"path": "T.Data", "length": 1.0, "groups": [{"kind": "group", "name": "G", "tracks": [
                    {"class": "Engine.InterpTrackMove", "data": {"type": "move", "move_frame": "world",
                        "pos": {"points": [key(0.0, 0.0), key(1.0, 500.0)]},
                        "euler": {"points": [key(0.0, 0.0), key(1.0, 0.0)]}}}]}]}]
            }))
            .unwrap(),
            0,
        )
        .unwrap();
        let g = Graph::from_json_slice(&serde_json::to_vec(&doc).unwrap()).unwrap();
        let mut r = Runtime::new(Arc::new(g), Arc::new(m));
        for _ in 0..90 {
            r.tick(1.0 / 60.0, &mut NullHost);
        }
        // A and B carry each other until the depth limit; C (attached to
        // itself) is never touched.
        let moved: Vec<_> = r.moved_actors().map(|(a, _, _)| a.0).collect();
        assert_eq!(moved, vec![0, 1]);
        assert!(r.errors().is_empty(), "{:?}", r.errors());
    });
}

/// A looping Matinee stepped by an absurd frame time or play rate stays
/// cheap and finite-or-ignored (no million-iteration unwinding per step).
#[test]
fn looping_playback_with_hostile_steps_is_bounded() {
    use asamu_kismet::matinee::{InterpSettings, Playback};
    within(20, || {
        let settings = InterpSettings {
            looping: true,
            play_rate: 1.0e30,
            ..InterpSettings::default()
        };
        let mut p = Playback::new(1.0e-6, settings);
        p.playing = true;
        for _ in 0..10_000 {
            let _ = p.step(1.0);
        }
        p.reverse = true;
        for _ in 0..10_000 {
            let _ = p.step(1.0);
        }
    });
}

/// Animation-control tracks, notify windows and camera animations with
/// absurd values (non-finite rates and times, zero-length loops, huge
/// steps) stay bounded and never panic.
#[test]
fn hostile_animation_tracks_and_camera_animations_are_bounded() {
    use asamu_kismet::anim::update_track;
    use asamu_kismet::camera_anim::{CameraAnim, CameraAnimPlayer, CameraAnimSet, Pov};
    use asamu_kismet::matinee::{AnimControlKey, AnimControlTrack};
    within(20, || {
        let key = |t: f32, rate: f32, looping: bool| AnimControlKey {
            start_time: t,
            sequence: Some("S".into()),
            start_offset: f32::NAN,
            end_offset: 1.0e30,
            play_rate: rate,
            looping,
            reverse: true,
        };
        let track = AnimControlTrack {
            keys: vec![
                key(0.0, 1.0e30, true),
                key(f32::NAN, f32::INFINITY, true),
                key(1.0, -1.0e30, false),
            ],
            ..AnimControlTrack::default()
        };
        for len in [
            Some(0.0),
            Some(1.0e-30),
            Some(f32::NAN),
            Some(f32::INFINITY),
            None,
        ] {
            for (last, new) in [
                (0.0, 1.0e30),
                (0.0, f32::NAN),
                (f32::NAN, 5.0),
                (-1.0e30, 1.0e30),
            ] {
                let calls = update_track(&track, 0, last, new, false, &|_| len);
                assert!(calls.len() < 10_000, "{}", calls.len());
            }
        }
        // Camera animations: hostile JSON is rejected or contained; hostile
        // play parameters never loop or panic.
        let mut set = CameraAnimSet::default();
        assert!(set.add_json(b"{\"camera_anims\": 7}").is_err());
        assert_eq!(
            set.add_json(br#"{"camera_anims": [{"path": "A", "length": 1e39, "base_fov": null}]}"#)
                .unwrap_or(0),
            0
        );
        let anim = std::sync::Arc::new(CameraAnim {
            path: "A".into(),
            length: 0.0,
            base_fov: f32::NAN,
            move_track: None,
            fov_track: None,
        });
        let mut p = CameraAnimPlayer::new();
        for (rate, blend) in [
            (f32::NAN, f32::NAN),
            (1.0e30, 0.0),
            (-1.0, -5.0),
            (0.0, f32::INFINITY),
        ] {
            p.play(
                &anim,
                rate,
                f32::INFINITY,
                blend,
                blend,
                true,
                true,
                f32::NAN,
                false,
            );
            p.play(&anim, rate, 1.0, blend, blend, false, true, 1.0e30, true);
        }
        let pov = Pov {
            location: [0.0; 3],
            rotation: [i32::MAX, i32::MIN, 7],
            fov: 90.0,
        };
        for dt in [0.0, 1.0 / 60.0, f32::NAN, 1.0e30, -1.0] {
            for _ in 0..100 {
                p.advance(dt);
                let _ = p.apply(pov);
            }
        }
        p.stop_all(false);
        p.stop_all(true);
        p.advance(0.0);
        assert!(p.active().is_empty());
    });
}
