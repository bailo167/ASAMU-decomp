//! A level script's state through a checkpoint save and a load
//! (`asamu_kismet::save_state`). The graphs and Matinees are written by hand
//! here (no game data); the guarded opening is the shape the shipped levels
//! use, in our own nodes.

#![allow(clippy::unwrap_used)]

use std::collections::BTreeMap;
use std::sync::Arc;

use asamu_kismet::graph::Graph;
use asamu_kismet::host::{Host, Output};
use asamu_kismet::matinee::MatineeSet;
use asamu_kismet::{ActorInfo, EventState, MatineeState, Runtime, ScriptState, VarState};
use serde_json::{Value, json};

const DT: f32 = 1.0 / 60.0;

fn path(name: &str) -> String {
    format!("T.TheWorld.PersistentLevel.{name}")
}

/// A node of the runtime graph: `links` are `(output label, [(op, input)])`.
fn node(id: usize, class: &str, kind: &str, name: &str) -> serde_json::Map<String, Value> {
    let mut m = serde_json::Map::new();
    m.insert("id".into(), json!(id));
    m.insert("class".into(), json!(class));
    m.insert("kind".into(), json!(kind));
    m.insert("parent".into(), json!(0));
    m.insert("path".into(), json!(format!("T.Seq.{name}")));
    m
}

fn outs(links: &[(&str, &[(usize, usize)])]) -> Value {
    json!(
        links
            .iter()
            .map(|(d, l)| {
                json!({"desc": d, "links": l.iter().map(|(o, i)| json!({"op": o, "input": i})).collect::<Vec<_>>()})
            })
            .collect::<Vec<_>>()
    )
}

fn with(
    mut m: serde_json::Map<String, Value>,
    k: &str,
    v: Value,
) -> serde_json::Map<String, Value> {
    m.insert(k.into(), v);
    m
}

fn action(
    id: usize,
    class: &str,
    name: &str,
    out: &[(usize, usize)],
) -> serde_json::Map<String, Value> {
    let m = node(id, class, "action", name);
    let m = with(m, "inputs", json!([{"desc": "In"}]));
    let m = with(m, "outputs", outs(&[("Out", out)]));
    with(m, "auto_activate_outputs", json!(true))
}

fn bool_var(id: usize, name: &str, v: bool) -> serde_json::Map<String, Value> {
    with(
        node(id, "Engine.SeqVar_Bool", "variable", name),
        "var",
        json!({"value": v}),
    )
}

fn matinee_node(
    id: usize,
    name: &str,
    completed: &[(usize, usize)],
    data: usize,
    door: usize,
) -> serde_json::Map<String, Value> {
    let m = node(id, "Engine.SeqAct_Interp", "action", name);
    let m = with(
        m,
        "inputs",
        json!([{"desc": "Play"}, {"desc": "Reverse"}, {"desc": "Stop"}, {"desc": "Pause"}, {"desc": "Change Dir"}]),
    );
    let m = with(
        m,
        "outputs",
        outs(&[("Completed", completed), ("Reversed", &[]), ("Open", &[])]),
    );
    let m = with(
        m,
        "variables",
        json!([{"desc": "Data", "property": null, "vars": [data]}, {"desc": "Door", "property": null, "vars": [door]}]),
    );
    let m = with(m, "auto_activate_outputs", json!(true));
    let m = with(m, "latent", json!(true));
    with(m, "latent_base", json!(true))
}

fn graph(nodes: Vec<serde_json::Map<String, Value>>) -> Graph {
    let members: Vec<usize> = (1..=nodes.len()).collect();
    let mut all = vec![{
        let mut s = node(0, "Engine.Sequence", "sequence", "Main");
        s.insert("parent".into(), Value::Null);
        s.insert("members".into(), json!(members));
        s
    }];
    all.extend(nodes);
    Graph::from_json_slice(
        &serde_json::to_vec(&json!({
            "format": asamu_kismet::RUNTIME_FORMAT, "version": asamu_kismet::RUNTIME_VERSION,
            "package": "T", "nodes": all,
            "actors": [
                {"path": path("Door"), "name": "Door", "class": "Engine.InterpActor", "kind": "interp_actor",
                 "package": "T", "slot": 10, "location": [10.0, 0.0, 0.0]},
                {"path": path("Trigger"), "name": "Trigger", "class": "Engine.TriggerVolume", "kind": "trigger_volume",
                 "package": "T", "slot": 11, "location": [0.0, 0.0, 0.0]}
            ]
        }))
        .unwrap(),
    )
    .unwrap()
}

/// A Matinee of `length` seconds on node `node`: the door rises 100 uu per
/// second, an event key `Open` at half a second, and a director group with
/// a fade track from 0 to 1.
fn matinee(node: usize, length: f32, settings: Value) -> MatineeSet {
    let key = |t: f32, z: f32| json!({"in": t, "out": [0.0, 0.0, z], "arrive": [0.0, 0.0, 0.0], "leave": [0.0, 0.0, 0.0], "mode": "linear"});
    let fade =
        |t: f32, a: f32| json!({"in": t, "out": a, "arrive": 0.0, "leave": 0.0, "mode": "linear"});
    let mut action = json!({"path": "T.I", "node": node, "scope": "level", "interp_data": "T.Data",
        "bindings": [{"link": 1, "label": "Door", "group": "Door",
                      "targets": [{"variable": "T.V", "object": path("Door")}]}]});
    action
        .as_object_mut()
        .unwrap()
        .insert("settings".into(), settings);
    let mut m = MatineeSet::default();
    m.add_json(
        &serde_json::to_vec(&json!({
            "format": "asamu-matinee", "version": 1, "package": "T",
            "actions": [action],
            "interp_data": [{"path": "T.Data", "length": length, "groups": [
                {"kind": "group", "name": "Door", "tracks": [
                    {"class": "Engine.InterpTrackMove", "data": {"type": "move", "move_frame": "relative_to_initial",
                        "pos": {"points": [key(0.0, 0.0), key(length, 100.0 * length)]},
                        "euler": {"points": [key(0.0, 0.0)]}}},
                    {"class": "Engine.InterpTrackEvent", "data": {"type": "event", "fire_forwards": true,
                        "keys": [{"time": 0.5, "name": "Open"}]}}]},
                {"kind": "director", "name": "Dir", "tracks": [
                    {"class": "Engine.InterpTrackFade", "data": {"type": "fade",
                        "curve": {"points": [fade(0.0, 0.0), fade(length, 1.0)]}}}]}]}]
        }))
        .unwrap(),
        0,
    )
    .unwrap();
    m
}

/// Records what reaches the game.
#[derive(Default)]
struct Rec {
    grapples: Vec<i32>,
    door: Option<[f32; 3]>,
    door_moves: usize,
    xf: BTreeMap<String, [f32; 3]>,
}

impl Host for Rec {
    fn set_max_grapples(&mut self, n: i32) {
        self.grapples.push(n);
    }
    fn set_actor_transform(&mut self, a: &ActorInfo, l: [f32; 3], _r: Option<[i32; 3]>) {
        if a.name == "Door" {
            self.door = Some(l);
            self.door_moves += 1;
        }
        self.xf.insert(a.name.clone(), l);
    }
}

fn ticks(r: &mut Runtime, h: &mut Rec, n: usize) -> Vec<Output> {
    let mut out = Vec::new();
    for _ in 0..n {
        r.tick(DT, h);
        out.extend(r.take_outputs());
    }
    out
}

/// The shipped levels' guard, in our nodes: "save loaded" → compare the
/// flag → false → set the flag → the opening (a marker action and a
/// Matinee whose end runs another marker).
fn guarded_opening() -> (Graph, MatineeSet) {
    let saved = with(
        node(
            1,
            "asamu.SaveGameState_SeqEvent_SavedGameStateLoaded",
            "event",
            "Loaded",
        ),
        "outputs",
        outs(&[("Loaded", &[(2, 0)])]),
    );
    let compare = {
        let m = node(2, "Engine.SeqCond_CompareBool", "condition", "Compare");
        let m = with(m, "inputs", json!([{"desc": "In"}]));
        let m = with(
            m,
            "outputs",
            outs(&[("True", &[(9, 0)]), ("False", &[(4, 0)])]),
        );
        with(
            m,
            "variables",
            json!([{"desc": "Bool", "property": null, "vars": [3]}]),
        )
    };
    let set = {
        let m = action(4, "Engine.SeqAct_SetBool", "Set", &[(6, 0), (7, 0)]);
        let m = with(
            m,
            "variables",
            json!([{"desc": "Value", "property": null, "vars": [5]}, {"desc": "Target", "property": null, "vars": [3]}]),
        );
        with(m, "params", json!({"DefaultValue": true}))
    };
    let g = graph(vec![
        saved,
        compare,
        bool_var(3, "Seen", false),
        set,
        bool_var(5, "Yes", true),
        with(
            action(6, "asamu.SeqAct_SetMaxGrapples", "OpeningStarts", &[]),
            "params",
            json!({"Grapples": 1}),
        ),
        matinee_node(7, "Flight", &[(8, 0)], 10, 11),
        with(
            action(8, "asamu.SeqAct_SetMaxGrapples", "OpeningEnds", &[]),
            "params",
            json!({"Grapples": 2}),
        ),
        with(
            action(9, "asamu.SeqAct_SetMaxGrapples", "AfterLoad", &[]),
            "params",
            json!({"Grapples": 9}),
        ),
        with(
            node(10, "Engine.InterpData", "variable", "Data"),
            "path",
            json!("T.Data"),
        ),
        with(
            node(11, "Engine.SeqVar_Object", "variable", "DoorVar"),
            "var",
            json!({"value": {"$obj": path("Door")}}),
        ),
    ]);
    (g, matinee(7, 1.0, json!({})))
}

fn runtime(g: Graph, m: MatineeSet) -> Runtime {
    Runtime::new(Arc::new(g), Arc::new(m))
}

#[test]
fn a_load_restores_the_flag_and_the_opening_does_not_play_again() {
    let (g, m) = guarded_opening();
    // A fresh start: the opening plays to its end.
    let mut fresh = runtime(g.clone(), m.clone());
    let mut h = Rec::default();
    let out = ticks(&mut fresh, &mut h, 90);
    assert_eq!(h.grapples, vec![1, 2], "opening started and ended");
    assert!(out.iter().any(|o| matches!(o, Output::MatineeFade { .. })));
    assert_eq!(fresh.interp_position(7), Some(1.0));
    let state = fresh.capture_state();
    assert_eq!(
        state.variables.get("T.Seq.Seen"),
        Some(&VarState::Bool(true))
    );
    assert_eq!(
        state.matinees.get("T.Seq.Flight"),
        Some(&MatineeState {
            position: 1.0,
            playing: false,
            paused: false
        })
    );
    assert_eq!(
        state.events.get("T.Seq.Loaded"),
        Some(&EventState {
            retrigger_left: 0.0,
            trigger_count: 0,
            enabled: true
        }),
        "a forced output counts no trigger"
    );
    assert_eq!(
        state.variables.get("T.Seq.DoorVar"),
        Some(&VarState::Object(Some(path("Door"))))
    );
    assert!(
        !state.variables.contains_key("T.Data"),
        "Matinee data is no saved variable"
    );

    // The same level loaded from that save.
    let mut loaded = runtime(g, m);
    loaded.set_start_state(Some(state.clone()));
    loaded.set_start_save_index(Some(3));
    let mut h = Rec::default();
    let out = ticks(&mut loaded, &mut h, 1);
    assert_eq!(
        h.grapples,
        vec![9],
        "the flag is true: the other branch runs"
    );
    assert_eq!(h.door, Some([10.0, 0.0, 100.0]), "the door is where it was");
    assert_eq!(h.door_moves, 1);
    assert!(
        !out.iter()
            .any(|o| matches!(o, Output::MatineeFade { .. } | Output::MatineeCut { .. })),
        "the director group of a Matinee that is only put in place shows nothing: {out:?}"
    );
    let out = ticks(&mut loaded, &mut h, 120);
    assert_eq!(h.grapples, vec![9]);
    assert!(!loaded.is_active(7));
    assert_eq!(loaded.activate_count(7), 0);
    assert_eq!(loaded.activate_count(4), 0);
    assert_eq!(h.door_moves, 1);
    assert!(
        !out.iter()
            .any(|o| matches!(o, Output::MatineeFade { .. } | Output::MatineeCut { .. }))
    );
    assert!(loaded.start_state().is_none(), "taken by the level start");
    // And a save of the loaded level is the save it came from.
    assert_eq!(loaded.capture_state(), state);
}

#[test]
fn without_a_state_the_same_load_plays_the_opening() {
    let (g, m) = guarded_opening();
    let mut r = runtime(g, m);
    r.set_start_save_index(Some(3));
    let mut h = Rec::default();
    ticks(&mut r, &mut h, 90);
    assert_eq!(h.grapples, vec![1, 2]);
}

#[test]
fn the_variables_of_the_level_s_own_start_stand_in_for_a_save_without_script_state() {
    let (g, m) = guarded_opening();
    let r = runtime(g.clone(), m.clone());
    let vars = r.level_start_variables();
    assert_eq!(vars.get("T.Seq.Seen"), Some(&VarState::Bool(true)));
    assert_eq!(vars.get("T.Seq.Yes"), Some(&VarState::Bool(true)));
    // The runtime itself did not start.
    assert_eq!(r.frame(), 0);
    assert_eq!(r.activate_count(4), 0);
    // Restored alone, they skip the opening; the Matinee stays at its start.
    let mut loaded = runtime(g, m);
    loaded.set_start_state(Some(ScriptState {
        variables: vars,
        ..ScriptState::default()
    }));
    let mut h = Rec::default();
    ticks(&mut loaded, &mut h, 90);
    assert_eq!(h.grapples, vec![9]);
    assert_eq!(h.door, None);
    assert!(
        loaded.level_start_variables().is_empty(),
        "only before the start"
    );
}

/// A touch event with one trigger and a re-trigger delay, leading to a
/// marker action.
fn touch_graph(max: i32, delay: f32) -> Graph {
    let touch = {
        let m = node(1, "Engine.SeqEvent_Touch", "event", "Touch");
        let m = with(
            m,
            "outputs",
            outs(&[("Touched", &[(2, 0)]), ("UnTouched", &[]), ("Empty", &[])]),
        );
        with(
            m,
            "event",
            json!({"originator": path("Trigger"), "max_trigger_count": max, "retrigger_delay": delay, "player_only": true}),
        )
    };
    graph(vec![
        touch,
        with(
            action(2, "asamu.SeqAct_SetMaxGrapples", "Prompt", &[]),
            "params",
            json!({"Grapples": 5}),
        ),
    ])
}

#[test]
fn a_one_time_event_stays_used_after_a_load() {
    let g = touch_graph(1, 0.0);
    let trigger = g.actor_by_path(&path("Trigger")).unwrap();
    let mut r = runtime(g.clone(), MatineeSet::default());
    let mut h = Rec::default();
    ticks(&mut r, &mut h, 2);
    r.touch(trigger, true);
    ticks(&mut r, &mut h, 2);
    assert_eq!(h.grapples, vec![5]);
    let state = r.capture_state();
    assert_eq!(state.events["T.Seq.Touch"].trigger_count, 1);

    let mut loaded = runtime(g.clone(), MatineeSet::default());
    loaded.set_start_state(Some(state));
    let mut h = Rec::default();
    ticks(&mut loaded, &mut h, 2);
    loaded.touch(trigger, true);
    ticks(&mut loaded, &mut h, 2);
    assert!(h.grapples.is_empty(), "the prompt does not come again");

    // The level entered without a save: it does.
    let mut again = runtime(g, MatineeSet::default());
    let mut h = Rec::default();
    ticks(&mut again, &mut h, 2);
    again.touch(trigger, true);
    ticks(&mut again, &mut h, 2);
    assert_eq!(h.grapples, vec![5]);
}

#[test]
fn a_disabled_event_stays_disabled_and_a_saved_delay_adds_to_the_whole_delay() {
    let g = touch_graph(0, 2.0);
    let trigger = g.actor_by_path(&path("Trigger")).unwrap();
    let mut r = runtime(g.clone(), MatineeSet::default());
    let mut h = Rec::default();
    ticks(&mut r, &mut h, 1);
    r.touch(trigger, true);
    // Half a second later 1.5 s of the delay are left.
    ticks(&mut r, &mut h, 30);
    let state = r.capture_state();
    let saved = state.events["T.Seq.Touch"];
    assert_eq!(saved.trigger_count, 1);
    assert!((saved.retrigger_left - 1.5).abs() < 1e-3, "{saved:?}");
    assert_eq!(
        saved.retrigger_left,
        asamu_kismet::save_state::save_float(saved.retrigger_left)
    );

    // After the load the event fires again once its whole delay and the
    // saved rest have passed (the original's arithmetic), not before.
    let mut loaded = runtime(g.clone(), MatineeSet::default());
    loaded.set_start_state(Some(state.clone()));
    let mut h = Rec::default();
    ticks(&mut loaded, &mut h, 1);
    let fires_after = |r: &mut Runtime, h: &mut Rec, n: usize| {
        ticks(r, h, n);
        r.touch(trigger, false);
        r.touch(trigger, true);
        ticks(r, h, 2);
        !h.grapples.is_empty()
    };
    assert!(
        !fires_after(&mut loaded, &mut h, 60),
        "1 s: the saved rest is not over"
    );
    assert!(
        !fires_after(&mut loaded, &mut h, 118),
        "3 s: delay not over yet"
    );
    assert!(fires_after(&mut loaded, &mut h, 40), "past 3.5 s");

    // `bEnabled` comes from the save.
    let mut off = state;
    off.events.get_mut("T.Seq.Touch").unwrap().enabled = false;
    off.events.get_mut("T.Seq.Touch").unwrap().retrigger_left = 0.0;
    let mut loaded = runtime(g, MatineeSet::default());
    loaded.set_start_state(Some(off));
    let mut h = Rec::default();
    ticks(&mut loaded, &mut h, 400);
    assert!(!loaded.is_enabled(1));
    loaded.touch(trigger, true);
    ticks(&mut loaded, &mut h, 2);
    assert!(h.grapples.is_empty());
}

/// A looping mover started by the level-loaded event.
fn mover() -> (Graph, MatineeSet) {
    let loaded = {
        let m = node(1, "Engine.SeqEvent_LevelLoaded", "event", "LevelLoaded");
        let m = with(
            m,
            "outputs",
            outs(&[
                ("Loaded and Visible", &[(2, 0)]),
                ("Beginning of Level", &[]),
                ("Level Reset", &[]),
            ]),
        );
        with(m, "event", json!({"max_trigger_count": 1}))
    };
    let g = graph(vec![
        loaded,
        matinee_node(2, "Lift", &[], 3, 4),
        with(
            node(3, "Engine.InterpData", "variable", "Data"),
            "path",
            json!("T.Data"),
        ),
        with(
            node(4, "Engine.SeqVar_Object", "variable", "DoorVar"),
            "var",
            json!({"value": {"$obj": path("Door")}}),
        ),
    ]);
    (g, matinee(2, 4.0, json!({"looping": true})))
}

#[test]
fn a_matinee_that_was_playing_goes_on_from_its_saved_position() {
    let (g, m) = mover();
    let mut r = runtime(g.clone(), m.clone());
    let mut h = Rec::default();
    ticks(&mut r, &mut h, 90);
    let state = r.capture_state();
    let saved = state.matinees["T.Seq.Lift"];
    assert!(saved.playing && !saved.paused);
    assert!((saved.position - 1.5).abs() < 0.02, "{saved:?}");
    assert_eq!(
        saved.position,
        asamu_kismet::save_state::save_float(saved.position)
    );
    assert_eq!(
        state.events["T.Seq.LevelLoaded"].trigger_count, 1,
        "the level-loaded event used its one trigger"
    );

    let mut loaded = runtime(g.clone(), m.clone());
    loaded.set_start_state(Some(state));
    let mut h = Rec::default();
    ticks(&mut loaded, &mut h, 1);
    // Put in place by the load, then played on by the first update.
    let p = loaded.interp_position(2).unwrap();
    assert!(
        (p - (saved.position + DT)).abs() < 1e-4,
        "{p} after {saved:?}"
    );
    let z = h.door.unwrap()[2];
    assert!((z - 100.0 * p).abs() < 0.01, "door at {z}");
    assert!(loaded.is_active(2));
    assert_eq!(
        loaded.activate_count(1),
        1,
        "level loaded fires on a load as on a fresh start"
    );
    ticks(&mut loaded, &mut h, 59);
    let p2 = loaded.interp_position(2).unwrap();
    assert!((p2 - (saved.position + 1.0)).abs() < 1e-3, "{p2}");

    // A paused one stays paused where it was.
    let mut paused = ScriptState::default();
    paused.matinees.insert(
        "T.Seq.Lift".into(),
        MatineeState {
            position: 2.0,
            playing: false,
            paused: true,
        },
    );
    // (Level loaded still pulses Play, which clears the pause: the saved
    // flag only lasts where nothing plays the Matinee.)
    let mut loaded = runtime(g, m);
    loaded.set_start_state(Some(paused));
    let mut h = Rec::default();
    ticks(&mut loaded, &mut h, 1);
    let p = loaded.interp_position(2).unwrap();
    assert!((p - (2.0 + DT)).abs() < 1e-4, "{p}");
}

#[test]
fn a_matinee_with_rewind_on_play_starts_over_after_a_load_as_its_play_input_says() {
    let (g, _) = mover();
    let m = matinee(2, 4.0, json!({"looping": true, "rewind_on_play": true}));
    let mut state = ScriptState::default();
    state.matinees.insert(
        "T.Seq.Lift".into(),
        MatineeState {
            position: 3.0,
            playing: true,
            paused: false,
        },
    );
    let mut loaded = runtime(g, m);
    loaded.set_start_state(Some(state));
    let mut h = Rec::default();
    ticks(&mut loaded, &mut h, 1);
    let p = loaded.interp_position(2).unwrap();
    assert!((p - DT).abs() < 1e-4, "{p}");
}

#[test]
fn entries_that_do_not_fit_the_level_are_skipped() {
    let (g, m) = guarded_opening();
    let mut state = ScriptState::default();
    // Another level's objects, a variable under an event's path and the
    // other way round, a Matinee under a variable's path, numbers that are
    // not numbers.
    state.events.insert(
        "Other.Seq.Touch".into(),
        EventState {
            retrigger_left: 1.0,
            trigger_count: 7,
            enabled: false,
        },
    );
    state.events.insert(
        "T.Seq.Seen".into(),
        EventState {
            retrigger_left: f32::NAN,
            trigger_count: i32::MIN,
            enabled: false,
        },
    );
    state.events.insert(
        "T.Seq.Loaded".into(),
        EventState {
            retrigger_left: f32::INFINITY,
            trigger_count: i32::MAX,
            enabled: true,
        },
    );
    state
        .variables
        .insert("T.Seq.Loaded".into(), VarState::Bool(true));
    state
        .variables
        .insert("T.Seq.Seen".into(), VarState::Str("yes".into()));
    state.variables.insert("T.Seq.Yes".into(), VarState::Int(0));
    state
        .variables
        .insert("T.Seq.DoorVar".into(), VarState::Float(f32::NAN));
    state.matinees.insert(
        "T.Seq.Seen".into(),
        MatineeState {
            position: 1.0,
            playing: true,
            paused: true,
        },
    );
    state.matinees.insert(
        "T.Seq.Flight".into(),
        MatineeState {
            position: f32::NAN,
            playing: true,
            paused: true,
        },
    );
    state.matinees.insert(
        "T.Seq.Set".into(),
        MatineeState {
            position: 0.5,
            playing: true,
            paused: false,
        },
    );
    let mut loaded = runtime(g, m);
    loaded.set_start_state(Some(state));
    let mut h = Rec::default();
    ticks(&mut loaded, &mut h, 90);
    // Nothing of it took: the opening plays as on a fresh start.
    assert_eq!(h.grapples, vec![1, 2]);
    assert!(loaded.errors().is_empty(), "{:?}", loaded.errors());
}

#[test]
fn a_position_beyond_the_matinee_is_an_ordinary_set_position() {
    let (g, m) = guarded_opening();
    for position in [-5.0, 0.0, 0.25, 1.0, 50.0, 1.0e30, f32::MAX, f32::MIN] {
        for playing in [false, true] {
            let mut state = ScriptState::default();
            state
                .variables
                .insert("T.Seq.Seen".into(), VarState::Bool(true));
            state.matinees.insert(
                "T.Seq.Flight".into(),
                MatineeState {
                    position,
                    playing,
                    paused: false,
                },
            );
            let mut loaded = runtime(g.clone(), m.clone());
            loaded.set_start_state(Some(state));
            let mut h = Rec::default();
            ticks(&mut loaded, &mut h, 200);
            assert!(
                loaded.errors().is_empty(),
                "{position} {playing}: {:?}",
                loaded.errors()
            );
            assert!(
                h.door.unwrap().iter().all(|c| c.is_finite()),
                "{position} {playing}"
            );
        }
    }
}

/// The lift of [`mover`] on a Matinee of four seconds that is not looping,
/// with a sound track: a key at half a second and one at two and a half.
fn sounding_lift() -> (Graph, MatineeSet) {
    let (g, _) = mover();
    let key = |t: f32, z: f32| json!({"in": t, "out": [0.0, 0.0, z], "arrive": [0.0, 0.0, 0.0], "leave": [0.0, 0.0, 0.0], "mode": "linear"});
    let mut m = MatineeSet::default();
    m.add_json(
        &serde_json::to_vec(&json!({
            "format": "asamu-matinee", "version": 1, "package": "T",
            "actions": [{"path": "T.I", "node": 2, "scope": "level", "interp_data": "T.Data",
                "settings": {},
                "bindings": [{"link": 1, "label": "Door", "group": "Door",
                              "targets": [{"variable": "T.V", "object": path("Door")}]}]}],
            "interp_data": [{"path": "T.Data", "length": 4.0, "groups": [
                {"kind": "group", "name": "Door", "tracks": [
                    {"class": "Engine.InterpTrackMove", "data": {"type": "move", "move_frame": "relative_to_initial",
                        "pos": {"points": [key(0.0, 0.0), key(4.0, 400.0)]},
                        "euler": {"points": [key(0.0, 0.0)]}}},
                    {"class": "Engine.InterpTrackSound", "data": {"type": "sound", "keys": [
                        {"time": 0.5, "sound": "T.Early", "volume": 1.0, "pitch": 1.0},
                        {"time": 2.5, "sound": "T.Late", "volume": 1.0, "pitch": 1.0}]}}]}]}]
        }))
        .unwrap(),
        0,
    )
    .unwrap();
    (g, m)
}

fn cues(outputs: &[Output]) -> Vec<String> {
    outputs
        .iter()
        .filter_map(|o| match o {
            Output::MatineeSound { cue, .. } => cue.clone(),
            _ => None,
        })
        .collect()
}

/// A Matinee that was playing at the save is put at its saved position
/// without a jump, from the start of the freshly loaded action. The sound
/// keys in between are not heard again (the engine stops what the temporary
/// track instances started when it tears them down); a key after the saved
/// position plays when the Matinee gets there. The same for a Matinee that
/// was not playing, which is put in place as a jump.
#[test]
fn the_sound_keys_before_a_saved_position_are_not_heard_again() {
    let (g, m) = sounding_lift();
    let mut r = runtime(g.clone(), m.clone());
    let mut h = Rec::default();
    let heard = ticks(&mut r, &mut h, 90);
    assert_eq!(cues(&heard), vec!["T.Early"], "the fresh start, 1.5 s");
    let state = r.capture_state();
    let saved = state.matinees["T.Seq.Lift"];
    assert!(
        saved.playing && (saved.position - 1.5).abs() < 0.02,
        "{saved:?}"
    );

    let mut loaded = runtime(g.clone(), m.clone());
    loaded.set_start_state(Some(state));
    let mut h = Rec::default();
    let first = ticks(&mut loaded, &mut h, 1);
    assert_eq!(
        cues(&first),
        Vec::<String>::new(),
        "the load and its first update"
    );
    let z = h.door.unwrap()[2];
    assert!(
        (z - 100.0 * (saved.position + DT)).abs() < 0.01,
        "door at {z}"
    );
    let later = ticks(&mut loaded, &mut h, 89);
    assert_eq!(cues(&later), vec!["T.Late"], "played on to 3 s");

    // Finished before the save: put at its end as a jump, nothing heard;
    // level loaded plays it from there and it ends at once.
    let mut done = ScriptState::default();
    done.matinees.insert(
        "T.Seq.Lift".into(),
        MatineeState {
            position: 4.0,
            playing: false,
            paused: false,
        },
    );
    let mut loaded = runtime(g, m);
    loaded.set_start_state(Some(done));
    let mut h = Rec::default();
    let heard = ticks(&mut loaded, &mut h, 120);
    assert_eq!(cues(&heard), Vec::<String>::new());
    assert!((h.door.unwrap()[2] - 400.0).abs() < 0.01);
}
