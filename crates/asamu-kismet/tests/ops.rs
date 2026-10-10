//! Synthetic graphs for the scheduler and every implemented class. Graphs
//! are written by hand here (no game data).

#![allow(clippy::unwrap_used)]

use std::collections::BTreeMap;
use std::sync::Arc;

use asamu_kismet::graph::Graph;
use asamu_kismet::host::{Host, Output, ToggleMode};
use asamu_kismet::matinee::MatineeSet;
use asamu_kismet::{ActorInfo, KValue, OpClass, Runtime};
use serde_json::{Value, json};

// ------------------------------------------------------------------ harness

/// A node builder.
#[derive(Clone)]
struct N(serde_json::Map<String, Value>);

fn node(id: usize, class: &str, kind: &str) -> N {
    let mut m = serde_json::Map::new();
    m.insert("id".into(), json!(id));
    m.insert("class".into(), json!(class));
    m.insert("kind".into(), json!(kind));
    m.insert("parent".into(), json!(0));
    N(m)
}

fn act(id: usize, class: &str) -> N {
    node(id, class, "action")
        .inputs(&["In"])
        .out(&[("Out", &[])])
        .auto()
}

impl N {
    fn set(mut self, k: &str, v: Value) -> N {
        self.0.insert(k.into(), v);
        self
    }
    fn parent(self, p: Option<usize>) -> N {
        self.set("parent", json!(p))
    }
    fn path(self, p: &str) -> N {
        self.set("path", json!(p))
    }
    fn inputs(self, d: &[&str]) -> N {
        let v: Vec<Value> = d.iter().map(|d| json!({"desc": d})).collect();
        self.set("inputs", json!(v))
    }
    fn input_delays(self, d: &[(&str, f32)]) -> N {
        let v: Vec<Value> = d
            .iter()
            .map(|(d, t)| json!({"desc": d, "delay": t}))
            .collect();
        self.set("inputs", json!(v))
    }
    fn out(self, outs: &[(&str, &[(usize, usize)])]) -> N {
        let v: Vec<Value> = outs
            .iter()
            .map(|(d, l)| {
                json!({"desc": d, "links": l.iter().map(|(o, i)| json!({"op": o, "input": i})).collect::<Vec<_>>()})
            })
            .collect();
        self.set("outputs", json!(v))
    }
    fn out_delay(self, d: &str, delay: f32, l: &[(usize, usize)]) -> N {
        let links: Vec<Value> = l
            .iter()
            .map(|(o, i)| json!({"op": o, "input": i}))
            .collect();
        self.set(
            "outputs",
            json!([{"desc": d, "delay": delay, "links": links}]),
        )
    }
    fn vars(self, v: &[(&str, Option<&str>, &[usize])]) -> N {
        let v: Vec<Value> = v
            .iter()
            .map(|(d, p, ids)| json!({"desc": d, "property": p, "vars": ids}))
            .collect();
        self.set("variables", json!(v))
    }
    fn events(self, ids: &[usize]) -> N {
        self.set("event_links", json!([{"desc": "Event", "events": ids}]))
    }
    fn params(self, p: Value) -> N {
        self.set("params", p)
    }
    fn auto(self) -> N {
        self.set("auto_activate_outputs", json!(true))
    }
    fn no_auto(self) -> N {
        self.set("auto_activate_outputs", json!(false))
    }
    fn latent(self) -> N {
        self.set("latent", json!(true))
    }
    fn latent_base(self) -> N {
        self.set("latent", json!(true))
            .set("latent_base", json!(true))
    }
    fn event(self, e: Value) -> N {
        self.set("event", e)
    }
    fn enabled(self, on: bool) -> N {
        self.set("enabled", json!(on))
    }
    fn value(self, v: Value) -> N {
        self.set("var", json!({"value": v}))
    }
    fn members(self, m: &[usize]) -> N {
        self.set("members", json!(m))
    }
}

fn seq(id: usize, members: &[usize]) -> N {
    node(id, "Engine.Sequence", "sequence")
        .parent(None)
        .members(members)
}

/// Level-loaded event `id` whose first output fires `links`.
fn loaded(id: usize, links: &[(usize, usize)]) -> N {
    node(id, "Engine.SeqEvent_LevelLoaded", "event")
        .out(&[
            ("Loaded and Visible", links),
            ("Beginning of Level", &[]),
            ("Level Reset", &[]),
        ])
        .event(json!({"max_trigger_count": 1}))
}

fn var_bool(id: usize, v: bool) -> N {
    node(id, "Engine.SeqVar_Bool", "variable").value(json!(v))
}

fn var_int(id: usize, v: i32) -> N {
    node(id, "Engine.SeqVar_Int", "variable").value(json!(v))
}

fn var_float(id: usize, v: f32) -> N {
    node(id, "Engine.SeqVar_Float", "variable").value(json!(v))
}

fn var_obj(id: usize, path: Option<&str>) -> N {
    node(id, "Engine.SeqVar_Object", "variable").value(json!({"$obj": path}))
}

fn player(id: usize) -> N {
    node(id, "Engine.SeqVar_Player", "variable")
}

fn actor(name: &str, class: &str, kind: &str, slot: usize) -> Value {
    json!({"path": format!("T.TheWorld.PersistentLevel.{name}"), "name": name, "class": class,
           "kind": kind, "package": "T", "slot": slot, "location": [slot as f32, 0.0, 0.0]})
}

fn path(name: &str) -> String {
    format!("T.TheWorld.PersistentLevel.{name}")
}

/// Builds a graph from nodes given in any order (sorted by id).
fn graph(nodes: Vec<N>, actors: Vec<Value>) -> Graph {
    let mut nodes: Vec<serde_json::Map<String, Value>> = nodes.into_iter().map(|n| n.0).collect();
    nodes.sort_by_key(|m| m.get("id").and_then(Value::as_u64).unwrap_or(0));
    Graph::from_json_slice(
        &serde_json::to_vec(&json!({
            "format": asamu_kismet::RUNTIME_FORMAT, "version": asamu_kismet::RUNTIME_VERSION,
            "package": "T", "nodes": nodes, "actors": actors
        }))
        .unwrap(),
    )
    .unwrap()
}

/// A host that records every call.
#[derive(Default)]
struct Rec {
    calls: Vec<String>,
    story: bool,
    time_trial: bool,
    xf: BTreeMap<String, ([f32; 3], Option<[i32; 3]>)>,
}

impl Host for Rec {
    fn is_time_trial(&self) -> bool {
        self.time_trial
    }
    fn in_story_mode(&self) -> bool {
        self.story
    }
    fn set_story_mode(&mut self, on: bool) {
        self.story = on;
        self.calls.push(format!("story {on}"));
    }
    fn set_spawn_in_story_mode(&mut self, on: bool) {
        self.calls.push(format!("spawn_story {on}"));
    }
    fn set_max_grapples(&mut self, n: i32) {
        self.calls.push(format!("max_grapples {n}"));
    }
    fn enable_grapple(&mut self, e: bool) {
        self.calls.push(format!("grapple {e}"));
    }
    fn enable_rocket_boots(&mut self, e: bool) {
        self.calls.push(format!("boots {e}"));
    }
    fn hide_grapple_gun(&mut self, h: bool, a: bool, v: bool) {
        self.calls.push(format!("hide_gun {h} {a} {v}"));
    }
    fn set_zoom_available(&mut self, on: bool) {
        self.calls.push(format!("zoom {on}"));
    }
    fn trigger_checkpoint(&mut self, a: &ActorInfo) -> bool {
        self.calls.push(format!("checkpoint {}", a.name));
        true
    }
    fn set_checkpoint_enabled(&mut self, a: &ActorInfo, e: bool) -> bool {
        self.calls
            .push(format!("checkpoint_enabled {} {e}", a.name));
        true
    }
    fn activate_attractor(&mut self, a: &ActorInfo) -> bool {
        self.calls.push(format!("attractor {}", a.name));
        true
    }
    fn set_falling_rocks_active(&mut self, on: bool) {
        self.calls.push(format!("rocks {on}"));
    }
    fn set_level_streamed(&mut self, p: &str, l: bool, v: bool) -> bool {
        self.calls.push(format!("stream {p} {l} {v}"));
        true
    }
    fn toggle_actor(&mut self, a: &ActorInfo, m: ToggleMode) {
        self.calls.push(format!("toggle {} {m:?}", a.name));
    }
    fn set_actor_hidden(&mut self, a: &ActorInfo, m: ToggleMode) {
        self.calls.push(format!("hidden {} {m:?}", a.name));
    }
    fn destroy_actor(&mut self, a: &ActorInfo) {
        self.calls.push(format!("destroy {}", a.name));
    }
    fn change_collision(&mut self, a: &ActorInfo, c: bool, b: bool) {
        self.calls.push(format!("collision {} {c} {b}", a.name));
    }
    fn set_actor_transform(&mut self, a: &ActorInfo, l: [f32; 3], r: Option<[i32; 3]>) {
        self.xf.insert(a.name.clone(), (l, r));
    }
    fn player_rotation(&self) -> [i32; 3] {
        [100, 200, 300]
    }
    fn teleport_player(&mut self, l: [f32; 3], r: Option<[i32; 3]>) -> bool {
        self.calls.push(format!("teleport {l:?} {r:?}"));
        true
    }
    fn set_player_velocity(&mut self, v: [f32; 3]) {
        self.calls.push(format!("velocity {v:?}"));
    }
    fn kill_player(&mut self) {
        self.calls.push("kill".into());
    }
}

fn rt(g: Graph) -> Runtime {
    Runtime::new(Arc::new(g), Arc::new(MatineeSet::default()))
}

fn rt_m(g: Graph, m: MatineeSet) -> Runtime {
    Runtime::new(Arc::new(g), Arc::new(m))
}

const DT: f32 = 1.0 / 60.0;

fn ticks(r: &mut Runtime, h: &mut Rec, n: usize) -> Vec<Output> {
    let mut out = Vec::new();
    for _ in 0..n {
        r.tick(DT, h);
        out.extend(r.take_outputs());
    }
    out
}

/// Level start → `action` (id 2, input `input`), plus `extra` nodes; runs
/// `n` ticks.
fn start_with(
    action: N,
    input: usize,
    extra: Vec<N>,
    actors: Vec<Value>,
    n: usize,
) -> (Runtime, Rec, Vec<Output>) {
    let mut members = vec![1, 2];
    let mut nodes = vec![loaded(1, &[(2, input)]), action];
    for e in extra {
        if let Some(id) = e.0.get("id").and_then(Value::as_u64) {
            members.push(id as usize);
        }
        nodes.push(e);
    }
    nodes.push(seq(0, &members));
    let mut r = rt(graph(nodes, actors));
    let mut h = Rec::default();
    let out = ticks(&mut r, &mut h, n);
    (r, h, out)
}

// ------------------------------------------------------------------ scheduler

#[test]
fn level_loaded_fires_once_and_links_run_in_order() {
    let g = graph(
        vec![
            seq(0, &[1, 2, 3]),
            loaded(1, &[(2, 0), (3, 0)]),
            act(2, "asamu.SeqAct_SetMaxGrapples").params(json!({"Grapples": 1})),
            act(3, "asamu.SeqAct_SetMaxGrapples").params(json!({"Grapples": 2})),
        ],
        vec![],
    );
    let mut r = rt(g);
    let mut h = Rec::default();
    ticks(&mut r, &mut h, 5);
    assert_eq!(h.calls, vec!["max_grapples 1", "max_grapples 2"]);
    assert_eq!(r.activate_count(1), 1);
}

#[test]
fn output_and_input_delays_postpone_activation() {
    let g = graph(
        vec![
            seq(0, &[1, 2, 3]),
            loaded(1, &[(2, 0)]),
            act(2, "Engine.SeqAct_Log").out_delay("Out", 0.5, &[(3, 0)]),
            act(3, "asamu.SeqAct_SetMaxGrapples")
                .input_delays(&[("In", 0.25)])
                .params(json!({"Grapples": 4})),
        ],
        vec![],
    );
    let mut r = rt(g);
    let mut h = Rec::default();
    // 0.75 s = 45 frames after the first update.
    ticks(&mut r, &mut h, 45);
    assert!(h.calls.is_empty(), "{:?}", h.calls);
    ticks(&mut r, &mut h, 2);
    assert_eq!(h.calls, vec!["max_grapples 4"]);
}

#[test]
fn queued_impulses_run_a_non_latent_op_again() {
    // Two outputs of the event both hit the same input in one frame.
    let g = graph(
        vec![
            seq(0, &[1, 2]),
            node(1, "Engine.SeqEvent_LevelLoaded", "event")
                .out(&[("Loaded and Visible", &[(2, 0), (2, 0)])])
                .event(json!({"max_trigger_count": 1})),
            act(2, "Engine.SeqAct_AddInt")
                .vars(&[
                    ("A", Some("ValueA"), &[3]),
                    ("IntResult", Some("IntResult"), &[3]),
                ])
                .params(json!({"ValueA": 0, "ValueB": 1, "IntResult": 0})),
            var_int(3, 10),
        ],
        vec![],
    );
    let mut r = rt(g);
    let mut h = Rec::default();
    ticks(&mut r, &mut h, 2);
    assert_eq!(r.var_value(3), Some(&KValue::Int(12)));
    assert_eq!(r.activate_count(2), 2);
}

#[test]
fn runaway_loops_stop_at_the_step_limit() {
    // A log op feeding itself.
    let g = graph(
        vec![
            seq(0, &[1, 2]),
            loaded(1, &[(2, 0)]),
            act(2, "Engine.SeqAct_Log").out(&[("Out", &[(2, 0)])]),
        ],
        vec![],
    );
    let mut r = rt(g);
    let mut h = Rec::default();
    ticks(&mut r, &mut h, 2);
    assert!(r.stats().step_limit_hits >= 1);
    assert!(
        r.errors()
            .iter()
            .any(|e| e.contains("max Kismet execution steps"))
    );
}

#[test]
fn disabled_inputs_outputs_events_and_sequences_block() {
    let g = graph(
        vec![
            seq(0, &[1, 2, 3, 4, 5]),
            loaded(1, &[(2, 0), (3, 0)]),
            act(2, "asamu.SeqAct_SetMaxGrapples")
                .set("inputs", json!([{"desc": "In", "disabled": true}]))
                .params(json!({"Grapples": 1})),
            act(3, "Engine.SeqAct_Log").set(
                "outputs",
                json!([{"desc": "Out", "disabled": true, "links": [{"op": 4, "input": 0}]}]),
            ),
            act(4, "asamu.SeqAct_SetMaxGrapples").params(json!({"Grapples": 2})),
            // A disabled sub-sequence: its level-loaded event never fires.
            seq(5, &[6, 7]).parent(Some(0)).enabled(false),
            loaded(6, &[(7, 0)]).parent(Some(5)),
            act(7, "asamu.SeqAct_SetMaxGrapples")
                .parent(Some(5))
                .params(json!({"Grapples": 3})),
        ],
        vec![],
    );
    let mut r = rt(g);
    let mut h = Rec::default();
    ticks(&mut r, &mut h, 3);
    assert!(h.calls.is_empty(), "{:?}", h.calls);
}

#[test]
fn remote_events_reach_listeners_in_other_sequences_next_in_order() {
    let g = graph(
        vec![
            seq(0, &[1, 2, 3, 4]),
            loaded(1, &[(2, 0)]),
            act(2, "Engine.SeqAct_ActivateRemoteEvent").params(json!({"EventName": "Go"})),
            seq(3, &[5, 6]).parent(Some(0)),
            seq(4, &[7, 8]).parent(Some(0)),
            node(5, "Engine.SeqEvent_RemoteEvent", "event")
                .parent(Some(3))
                .out(&[("Out", &[(6, 0)])])
                .params(json!({"EventName": "go"})),
            act(6, "asamu.SeqAct_SetMaxGrapples")
                .parent(Some(3))
                .params(json!({"Grapples": 5})),
            node(7, "Engine.SeqEvent_RemoteEvent", "event")
                .parent(Some(4))
                .out(&[("Out", &[(8, 0)])])
                .params(json!({"EventName": "Other"})),
            act(8, "asamu.SeqAct_SetMaxGrapples")
                .parent(Some(4))
                .params(json!({"Grapples": 6})),
        ],
        vec![],
    );
    let mut r = rt(g);
    let mut h = Rec::default();
    // The root runs before its nested sequences: the listener in sequence 3
    // runs in the same update.
    ticks(&mut r, &mut h, 1);
    assert_eq!(h.calls, vec!["max_grapples 5"]);
    ticks(&mut r, &mut h, 3);
    assert_eq!(h.calls, vec!["max_grapples 5"]);
}

#[test]
fn touch_events_follow_trigger_counts_retrigger_delays_and_untouch() {
    let g = graph(
        vec![
            seq(0, &[1, 2, 3, 4]),
            node(1, "Engine.SeqEvent_Touch", "event")
                .out(&[
                    ("Touched", &[(2, 0)]),
                    ("UnTouched", &[(3, 0)]),
                    ("Empty", &[(4, 0)]),
                ])
                .event(json!({"originator": path("Trig"), "max_trigger_count": 0,
                               "retrigger_delay": 0.5, "player_only": true})),
            act(2, "asamu.SeqAct_SetMaxGrapples").params(json!({"Grapples": 1})),
            act(3, "asamu.SeqAct_SetMaxGrapples").params(json!({"Grapples": 2})),
            act(4, "asamu.SeqAct_SetMaxGrapples").params(json!({"Grapples": 3})),
        ],
        vec![actor("Trig", "Engine.Trigger", "trigger", 7)],
    );
    let a = g.actor_by_path(&path("Trig")).unwrap();
    let mut r = rt(g);
    let mut h = Rec::default();
    // The untouch check measures the re-trigger delay from level start (its
    // activation time is zeroed for the check), so start after 1 s.
    ticks(&mut r, &mut h, 60);
    r.touch(a, true);
    ticks(&mut r, &mut h, 1);
    assert_eq!(h.calls, vec!["max_grapples 1"]);
    // Untouch: UnTouched and Empty (in link order of the outputs).
    r.touch(a, false);
    ticks(&mut r, &mut h, 1);
    assert_eq!(h.calls[1..], ["max_grapples 2", "max_grapples 3"]);
    // Re-touch within the re-trigger delay: ignored; after it: fires.
    r.touch(a, true);
    ticks(&mut r, &mut h, 1);
    assert_eq!(h.calls.len(), 3);
    r.touch(a, false);
    ticks(&mut r, &mut h, 40);
    r.touch(a, true);
    ticks(&mut r, &mut h, 1);
    assert_eq!(h.calls.last().map(String::as_str), Some("max_grapples 1"));
}

#[test]
fn single_use_touch_has_no_untouch() {
    let g = graph(
        vec![
            seq(0, &[1, 2, 3]),
            node(1, "Engine.SeqEvent_Touch", "event")
                .out(&[
                    ("Touched", &[(2, 0)]),
                    ("UnTouched", &[(3, 0)]),
                    ("Empty", &[]),
                ])
                .event(json!({"originator": path("Vol"), "max_trigger_count": 1,
                               "retrigger_delay": 0.1, "player_only": true})),
            act(2, "asamu.SeqAct_ToggleGrapple").params(json!({"Enable": true})),
            act(3, "asamu.SeqAct_ToggleGrapple").params(json!({"Enable": false})),
        ],
        vec![actor("Vol", "Engine.TriggerVolume", "trigger_volume", 3)],
    );
    let a = g.actor_by_path(&path("Vol")).unwrap();
    let mut r = rt(g);
    let mut h = Rec::default();
    r.touch(a, true);
    r.touch(a, false);
    r.touch(a, true);
    ticks(&mut r, &mut h, 3);
    assert_eq!(h.calls, vec!["grapple true"]);
}

// ------------------------------------------------------------------ ASAMU events

#[test]
fn player_events_fire_their_listeners() {
    let g = graph(
        vec![
            seq(0, &(1..=16).collect::<Vec<_>>()),
            node(1, "asamu.SeqEvent_PlayerLanded", "event").out(&[("Out", &[(2, 0)])]),
            act(2, "asamu.SeqAct_SetMaxGrapples").params(json!({"Grapples": 1})),
            node(3, "asamu.SeqEvent_PlayerDied", "event").out(&[("Out", &[(4, 0)])]),
            act(4, "asamu.SeqAct_SetMaxGrapples").params(json!({"Grapples": 2})),
            node(5, "asamu.SeqEvent_PlayerRocketBoosted", "event")
                .out(&[("Charging", &[(6, 0)]), ("Boost", &[(7, 0)])]),
            act(6, "asamu.SeqAct_SetMaxGrapples").params(json!({"Grapples": 3})),
            act(7, "asamu.SeqAct_SetMaxGrapples").params(json!({"Grapples": 4})),
            node(8, "asamu.SeqEvent_PlayerReleasedGrapple", "event")
                .out(&[("Out", &[(9, 0)])])
                .event(json!({"max_trigger_count": 0})),
            act(9, "asamu.SeqAct_SetMaxGrapples").params(json!({"Grapples": 5})),
            node(10, "asamu.SeqEvent_PlayerGrappled", "event")
                .out(&[("Grappled", &[(11, 0)])])
                .vars(&[("GrappleActor", Some("inputActor"), &[12])])
                .event(json!({"max_trigger_count": 0})),
            act(11, "asamu.SeqAct_SetMaxGrapples").params(json!({"Grapples": 6})),
            var_obj(12, Some(&path("Lever"))),
            node(13, "asamu.SeqEvent_CollectibleCollected", "event")
                .out(&[("Out", &[(14, 0)])])
                .event(json!({"max_trigger_count": 0})),
            act(14, "asamu.SeqAct_SetMaxGrapples").params(json!({"Grapples": 7})),
            node(15, "asamu.SeqEvent_CreditsEnded", "event").out(&[("Ended", &[(16, 0)])]),
            act(16, "asamu.SeqAct_SetMaxGrapples").params(json!({"Grapples": 8})),
        ],
        vec![actor("Lever", "asamu.ASAMUInteractable_Actor", "other", 9)],
    );
    let lever = g.actor_by_path(&path("Lever")).unwrap();
    let mut r = rt(g);
    let mut h = Rec::default();
    r.player_landed();
    r.player_died();
    r.player_rocket_boosted(false);
    r.player_rocket_boosted(true);
    r.player_released_grapple();
    // Grappling the world does not match `inputActor`; the lever does.
    r.player_grappled(None);
    ticks(&mut r, &mut h, 1);
    r.player_grappled(Some(lever));
    r.collectible_collected();
    r.credits_ended();
    ticks(&mut r, &mut h, 2);
    let mut calls = h.calls.clone();
    calls.sort();
    let want: Vec<String> = [1, 2, 3, 4, 5, 6, 7, 7, 8]
        .iter()
        .map(|n| format!("max_grapples {n}"))
        .collect();
    // The collectible's own activation handler also forces its output, so
    // the linked action runs twice (as in the original).
    assert_eq!(calls, want);
}

#[test]
fn interacted_with_counts_calls_and_matches_the_originator() {
    let g = graph(
        vec![
            seq(0, &[1, 2]),
            node(1, "asamu.SeqEvent_ActorInteractedWith", "event")
                .out(&[("Interacted", &[(2, 0)])])
                .event(json!({"originator": path("Book"), "max_trigger_count": 2})),
            act(2, "asamu.SeqAct_SetMaxGrapples").params(json!({"Grapples": 1})),
        ],
        vec![
            actor("Book", "asamu.ASAMUInteractable_Actor", "other", 1),
            actor("Mug", "asamu.ASAMUInteractable_Actor", "other", 2),
        ],
    );
    let book = g.actor_by_path(&path("Book")).unwrap();
    let mug = g.actor_by_path(&path("Mug")).unwrap();
    let mut r = rt(g);
    let mut h = Rec::default();
    r.actor_interacted_with(mug); // counts, no match
    r.actor_interacted_with(book); // counts, fires
    r.actor_interacted_with(book); // count exhausted
    ticks(&mut r, &mut h, 2);
    assert_eq!(h.calls, vec!["max_grapples 1"]);
}

#[test]
fn saved_state_anim_notify_worm_used_destroyed_events() {
    let g = graph(
        vec![
            seq(0, &(1..=12).collect::<Vec<_>>()),
            node(
                1,
                "asamu.SaveGameState_SeqEvent_SavedGameStateLoaded",
                "event",
            )
            .out(&[("Loaded", &[(2, 0)])])
            .params(json!({"Index": 3})),
            act(2, "asamu.SeqAct_SetMaxGrapples").params(json!({"Grapples": 1})),
            node(3, "Engine.SeqEvent_AnimNotify", "event")
                .out(&[("Out", &[(4, 0)])])
                .params(json!({"NotifyName": "Talk"}))
                .event(json!({"originator": path("Npc"), "max_trigger_count": 0})),
            act(4, "asamu.SeqAct_SetMaxGrapples").params(json!({"Grapples": 2})),
            node(5, "asamu.SeqEvent_WormEvents", "event")
                .out(&[("WakingUp", &[]), ("Awaken", &[(6, 0)])]),
            act(6, "asamu.SeqAct_SetMaxGrapples").params(json!({"Grapples": 3})),
            node(7, "Engine.SeqEvent_Used", "event")
                .out(&[("Used", &[(8, 0)])])
                .event(json!({"originator": path("Npc"), "max_trigger_count": 0})),
            act(8, "asamu.SeqAct_SetMaxGrapples").params(json!({"Grapples": 4})),
            node(9, "Engine.SeqEvent_Destroyed", "event")
                .out(&[("Out", &[(10, 0)])])
                .event(json!({"originator": path("Npc"), "max_trigger_count": 0})),
            act(10, "asamu.SeqAct_SetMaxGrapples").params(json!({"Grapples": 5})),
            node(11, "asamu.SeqEvent_NarratorEvents", "event").out(&[
                ("Started narrating", &[(12, 0)]),
                ("Finished narrating", &[]),
            ]),
            act(12, "asamu.SeqAct_SetMaxGrapples").params(json!({"Grapples": 6})),
        ],
        vec![actor("Npc", "Engine.SkeletalMeshActor", "other", 4)],
    );
    let npc = g.actor_by_path(&path("Npc")).unwrap();
    let mut r = rt(g);
    r.set_start_save_index(Some(3));
    let mut h = Rec::default();
    r.anim_notify(npc, "talk");
    r.anim_notify(npc, "other");
    r.worm_event(1);
    r.used(npc);
    r.destroyed(npc);
    ticks(&mut r, &mut h, 2);
    let mut calls = h.calls.clone();
    calls.sort();
    assert_eq!(
        calls,
        [
            "max_grapples 1",
            "max_grapples 2",
            "max_grapples 3",
            "max_grapples 4",
            "max_grapples 5"
        ]
    );
}

/// `UAnimNotify_Kismet::Notify`: only the notify's own actor's events with
/// its name fire (instigator = the actor), and a nameless notify fires
/// nothing — not even the events whose `NotifyName` is unset (exported as
/// "None"; 2 such events ship in AG-BeautifulCity).
#[test]
fn anim_notifies_match_by_actor_and_name_and_nameless_ones_never_fire() {
    let event = |id: usize, name: &str, npc: &str, to: usize| {
        node(id, "Engine.SeqEvent_AnimNotify", "event")
            .out(&[("Out", &[(to, 0)])])
            .params(json!({"NotifyName": name}))
            .event(json!({"originator": path(npc), "max_trigger_count": 0}))
    };
    let g = graph(
        vec![
            seq(0, &[1, 2, 3, 4, 5, 6]),
            event(1, "Talk_1", "A", 4),
            event(2, "None", "A", 5),
            event(3, "Talk_1", "B", 6),
            act(4, "asamu.SeqAct_SetMaxGrapples").params(json!({"Grapples": 1})),
            act(5, "asamu.SeqAct_SetMaxGrapples").params(json!({"Grapples": 2})),
            act(6, "asamu.SeqAct_SetMaxGrapples").params(json!({"Grapples": 3})),
        ],
        vec![
            actor("A", "Engine.SkeletalMeshActor", "other", 4),
            actor("B", "Engine.SkeletalMeshActor", "other", 5),
        ],
    );
    let a = g.actor_by_path(&path("A")).unwrap();
    let mut r = rt(g);
    let mut h = Rec::default();
    ticks(&mut r, &mut h, 1);
    // Nameless notifies, in any spelling.
    for name in ["None", "none", ""] {
        r.anim_notify(a, name);
    }
    ticks(&mut r, &mut h, 2);
    assert!(h.calls.is_empty(), "{:?}", h.calls);
    assert_eq!(r.activate_count(2), 0);
    // An unknown name fires nothing; the right one fires A's event only.
    r.anim_notify(a, "Talk_2");
    r.anim_notify(a, "talk_1");
    ticks(&mut r, &mut h, 2);
    assert_eq!(h.calls, vec!["max_grapples 1"]);
    assert_eq!(
        (
            r.activate_count(1),
            r.activate_count(2),
            r.activate_count(3)
        ),
        (1, 0, 0)
    );
    assert!(r.errors().is_empty(), "{:?}", r.errors());
}

#[test]
fn track_beats_pulse_on_their_interval() {
    let g = graph(
        vec![
            seq(0, &[1, 2, 3]),
            node(1, "asamu.SeqAct_AddAdaptiveTracks", "action")
                .out(&[("Out", &[])])
                .params(json!({"tracksToAdd": [{"$struct": "TrackHolder", "ID": "Beat", "numberOfTracks": 1}]})),
            node(2, "asamu.SeqEvent_TrackBeat", "event")
                .out(&[("Beat", &[(3, 0)])])
                .params(json!({"trackID": "Beat", "beatAmount": 0.5})),
            act(3, "Engine.SeqAct_AddInt")
                .vars(&[("A", Some("ValueA"), &[4]), ("IntResult", Some("IntResult"), &[4])])
                .params(json!({"ValueA": 0, "ValueB": 1, "IntResult": 0})),
            var_int(4, 0),
        ],
        vec![],
    );
    let mut r = rt(g);
    let mut h = Rec::default();
    let out = ticks(&mut r, &mut h, 62);
    assert!(
        out.iter()
            .any(|o| matches!(o, Output::AdaptiveTracks { .. }))
    );
    // Beats after 0.5 s and 1.0 s (each runs in the next update).
    assert_eq!(r.var_value(4), Some(&KValue::Int(2)));
}

// ------------------------------------------------------------------ engine actions

#[test]
fn add_int_set_bool_set_float_set_object_set_int_set_string() {
    let g = graph(
        vec![
            seq(0, &(1..=16).collect::<Vec<_>>()),
            loaded(1, &[(2, 0)]),
            act(2, "Engine.SeqAct_AddInt")
                .out(&[("Out", &[(5, 0)])])
                .vars(&[
                    ("A", Some("ValueA"), &[3]),
                    ("IntResult", Some("IntResult"), &[4]),
                ])
                .params(json!({"ValueA": 0, "ValueB": 2, "IntResult": 0, "FloatResult": 0.0})),
            var_int(3, 5),
            var_int(4, 0),
            act(5, "Engine.SeqAct_SetBool")
                .out(&[("Out", &[(8, 0)])])
                .vars(&[("Value", None, &[6, 7]), ("Target", None, &[9])])
                .params(json!({"DefaultValue": false})),
            var_bool(6, true),
            var_bool(7, true),
            act(8, "Engine.SeqAct_SetFloat")
                .out(&[("Out", &[(11, 0)])])
                .vars(&[
                    ("Value", Some("Value"), &[10, 10]),
                    ("Target", Some("Target"), &[12]),
                ])
                .params(json!({"Value": [], "Target": 0.0})),
            var_bool(9, false),
            var_float(10, 1.5),
            act(11, "Engine.SeqAct_SetObject")
                .out(&[("Out", &[(14, 0)])])
                .vars(&[
                    ("Value", Some("Value"), &[13]),
                    ("Target", Some("Targets"), &[15]),
                ])
                .params(
                    json!({"Value": {"$obj": null}, "DefaultValue": {"$obj": null}, "Targets": []}),
                ),
            var_float(12, 0.0),
            var_obj(13, Some(&path("Thing"))),
            act(14, "Engine.SeqAct_SetInt")
                .vars(&[
                    ("Value", Some("Value"), &[3]),
                    ("Target", Some("Target"), &[16]),
                ])
                .params(json!({"Value": 0, "Target": 0})),
            var_obj(15, None),
            var_int(16, 0),
        ],
        vec![actor("Thing", "Engine.StaticMeshActor", "static_mesh", 1)],
    );
    let mut r = rt(g);
    let mut h = Rec::default();
    ticks(&mut r, &mut h, 2);
    assert_eq!(r.var_value(4), Some(&KValue::Int(7)));
    assert_eq!(r.var_value(9), Some(&KValue::Bool(true)));
    assert_eq!(r.var_value(12), Some(&KValue::Float(3.0)));
    assert_eq!(r.var_value(15), Some(&KValue::Obj(Some(path("Thing")))));
    assert_eq!(r.var_value(16), Some(&KValue::Int(5)));
}

#[test]
fn compare_conditions_pick_outputs() {
    // CompareInt 3 vs 5 → "A <= B" and "A < B"; CompareFloat 2 vs 2 → three;
    // CompareBool (true, false) → False; Increment 4+1 vs 5 → ==; IsPIE → No.
    let target = |id: usize, n: i32| {
        act(id, "Engine.SeqAct_AddInt")
            .params(json!({"ValueA": n, "ValueB": 0}))
            .no_auto()
    };
    let mut nodes = vec![
        seq(0, &(1..=30).collect::<Vec<_>>()),
        loaded(1, &[(2, 0), (3, 0), (4, 0), (5, 0), (6, 0), (7, 0)]),
        node(2, "Engine.SeqCond_CompareInt", "condition")
            .inputs(&["In"])
            .out(&[
                ("A <= B", &[(10, 0)]),
                ("A > B", &[(11, 0)]),
                ("A == B", &[(12, 0)]),
                ("A < B", &[(13, 0)]),
                ("A >= B", &[(14, 0)]),
            ])
            .params(json!({"ValueA": 3, "ValueB": 5})),
        node(3, "Engine.SeqCond_CompareFloat", "condition")
            .inputs(&["In"])
            .out(&[
                ("A <= B", &[(15, 0)]),
                ("A > B", &[(16, 0)]),
                ("A == B", &[(17, 0)]),
                ("A < B", &[(18, 0)]),
                ("A >= B", &[(19, 0)]),
            ])
            .params(json!({"ValueA": 2.0, "ValueB": 2.0})),
        node(4, "Engine.SeqCond_CompareBool", "condition")
            .inputs(&["In"])
            .out(&[("True", &[(20, 0)]), ("False", &[(21, 0)])])
            .vars(&[("Bool", None, &[8, 9])]),
        node(5, "Engine.SeqCond_Increment", "condition")
            .inputs(&["In"])
            .out(&[
                ("A <= B", &[]),
                ("A > B", &[]),
                ("A == B", &[(22, 0)]),
                ("A < B", &[]),
                ("A >= B", &[]),
            ])
            .vars(&[("A", Some("ValueA"), &[23])])
            .params(json!({"IncrementAmount": 1, "ValueA": 0, "ValueB": 5})),
        node(6, "Engine.SeqCond_IsPIE", "condition")
            .inputs(&["In"])
            .out(&[("Yes", &[(24, 0)]), ("No", &[(25, 0)])]),
        node(7, "Engine.SeqCond_CompareObject", "condition")
            .inputs(&["In"])
            .out(&[("A == B", &[(26, 0)]), ("A != B", &[(27, 0)])])
            .vars(&[("A", None, &[28]), ("B", None, &[29])]),
        var_bool(8, true),
        var_bool(9, false),
        var_int(23, 4),
        player(28),
        player(29),
    ];
    for id in (10..=22).chain(24..=27) {
        nodes.push(target(id, id as i32));
    }
    let mut r = rt(graph(nodes, vec![]));
    let mut h = Rec::default();
    ticks(&mut r, &mut h, 2);
    let ran: Vec<usize> = (10..=27).filter(|id| r.activate_count(*id) > 0).collect();
    assert_eq!(ran, vec![10, 13, 15, 17, 19, 21, 22, 25, 26]);
    // Increment wrote its new value back.
    assert_eq!(r.var_value(23), Some(&KValue::Int(5)));
}

#[test]
fn toggle_sets_bools_events_and_actors() {
    let g = graph(
        vec![
            seq(0, &[1, 2, 3, 4, 5, 6]),
            loaded(1, &[(2, 2)]),
            node(2, "Engine.SeqAct_Toggle", "action")
                .inputs(&["Turn On", "Turn Off", "Toggle"])
                .out(&[("Out", &[(6, 0)])])
                .vars(&[("Target", Some("Targets"), &[3]), ("Bool", None, &[4])])
                .events(&[5])
                .params(json!({"Targets": []}))
                .auto(),
            var_obj(3, Some(&path("Light"))),
            var_bool(4, false),
            node(5, "Engine.SeqEvent_Touch", "event")
                .enabled(true)
                .out(&[("Touched", &[]), ("UnTouched", &[]), ("Empty", &[])]),
            node(6, "Engine.SeqAct_ToggleHidden", "action")
                .inputs(&["Hide", "UnHide", "Toggle"])
                .vars(&[("Target", Some("Targets"), &[3]), ("Bool", None, &[])])
                .params(json!({"Targets": []}))
                .auto(),
        ],
        vec![actor("Light", "Engine.PointLightToggleable", "light", 2)],
    );
    let mut r = rt(g);
    let mut h = Rec::default();
    let out = ticks(&mut r, &mut h, 2);
    assert_eq!(r.var_value(4), Some(&KValue::Bool(true)));
    assert!(!r.is_enabled(5));
    assert_eq!(h.calls, vec!["toggle Light Toggle", "hidden Light On"]);
    assert!(out.iter().any(|o| matches!(
        o,
        Output::ActorToggled {
            mode: ToggleMode::Toggle,
            ..
        }
    )));
    assert!(out.iter().any(|o| matches!(
        o,
        Output::ActorHidden {
            mode: ToggleMode::On,
            ..
        }
    )));
}

#[test]
fn delay_waits_and_can_be_restarted_or_stopped() {
    let g = graph(
        vec![
            seq(0, &[1, 2, 3, 4]),
            loaded(1, &[(2, 0)]),
            node(2, "Engine.SeqAct_Delay", "action")
                .inputs(&["Start", "Stop", "Pause"])
                .out(&[("Finished", &[(3, 0)]), ("Aborted", &[])])
                .vars(&[("Duration", Some("Duration"), &[4])])
                .params(json!({"bStartWillRestart": true, "DefaultDuration": 1.0, "Duration": 0.0}))
                .auto()
                .latent_base(),
            act(3, "asamu.SeqAct_SetMaxGrapples").params(json!({"Grapples": 9})),
            var_float(4, 0.5),
        ],
        vec![],
    );
    let mut r = rt(g);
    let mut h = Rec::default();
    // The activation frame does not count: 0.5 s = 30 more frames.
    ticks(&mut r, &mut h, 30);
    assert!(h.calls.is_empty());
    ticks(&mut r, &mut h, 2);
    assert_eq!(h.calls, vec!["max_grapples 9"]);
    assert!(!r.is_active(2));
    // Restart, then stop: nothing fires.
    r.force_input(2, 0);
    ticks(&mut r, &mut h, 10);
    r.force_input(2, 1);
    ticks(&mut r, &mut h, 60);
    assert_eq!(h.calls.len(), 1);
}

#[test]
fn console_commands_streaming_and_player_actions() {
    let g = graph(
        vec![
            seq(0, &(1..=8).collect::<Vec<_>>()),
            loaded(1, &[(2, 0), (4, 0), (5, 0), (6, 0)]),
            node(2, "Engine.SeqAct_ConsoleCommand", "action")
                .inputs(&["In"])
                .out(&[("Out", &[])])
                .vars(&[("Target", Some("Targets"), &[3])])
                .params(json!({"Commands": ["open AG-Next", "SetSpeed 0.5"], "Targets": []}))
                .auto(),
            player(3),
            node(4, "Engine.SeqAct_MultiLevelStreaming", "action")
                .inputs(&["Load", "Unload"])
                .out(&[("Finished", &[])])
                .params(
                    json!({"Levels": [{"$struct": "LevelStreamingNameCombo", "LevelName": "Sub"}],
                               "bMakeVisibleAfterLoad": true}),
                )
                .auto()
                .latent_base(),
            node(5, "Engine.SeqAct_Teleport", "action")
                .inputs(&["In"])
                .out(&[("Out", &[])])
                .vars(&[
                    ("Target", Some("Targets"), &[3]),
                    ("Destination", None, &[7]),
                ])
                .params(json!({"bUpdateRotation": true, "Targets": []}))
                .auto(),
            node(6, "Engine.SeqAct_SetVelocity", "action")
                .inputs(&["In"])
                .out(&[("Out", &[])])
                .vars(&[("Target", Some("Targets"), &[3])])
                .params(
                    json!({"VelocityDir": {"$struct": "Vector", "X": 0.0, "Y": 0.0, "Z": 2.0},
                               "VelocityMag": 300.0, "Targets": []}),
                )
                .auto(),
            var_obj(7, Some(&path("Dest"))),
            act(8, "asamu.SeqAct_PlayerDied"),
        ],
        vec![actor("Dest", "Engine.Note", "other", 6)],
    );
    let mut r = rt(g);
    let mut h = Rec::default();
    let out = ticks(&mut r, &mut h, 2);
    assert!(out.contains(&Output::LevelTransition {
        map: "AG-Next".into(),
        options: None
    }));
    assert!(out.contains(&Output::ConsoleCommand {
        command: "SetSpeed 0.5".into()
    }));
    assert!(h.calls.contains(&"stream Sub true true".to_owned()));
    assert!(
        h.calls
            .contains(&"teleport [6.0, 0.0, 0.0] Some([0, 0, 0])".to_owned())
    );
    assert!(h.calls.contains(&"velocity [0.0, 0.0, 300.0]".to_owned()));
    assert!(r.errors().is_empty(), "{:?}", r.errors());
    // The player-died action.
    r.force_input(8, 0);
    ticks(&mut r, &mut h, 1);
    assert_eq!(h.calls.last().map(String::as_str), Some("kill"));
}

#[test]
fn presentation_actions_emit_outputs() {
    let pres = [
        (
            "Engine.SeqAct_CameraShake",
            vec!["Start", "Stop"],
            json!({"ShakeScale": 0.5}),
        ),
        (
            "Engine.SeqAct_PlayMusicTrack",
            vec!["In"],
            json!({"MusicTrack": {"$struct": "MusicTrackStruct"}}),
        ),
        (
            "Engine.SeqAct_SetSoundMode",
            vec!["Start", "Stop"],
            json!({"bTopPriority": true}),
        ),
        (
            "Engine.SeqAct_SetMatInstScalarParam",
            vec!["In"],
            json!({"ParamName": "P", "ScalarValue": 1.0}),
        ),
        (
            "Engine.SeqAct_ToggleCinematicMode",
            vec!["Enable", "Disable", "Toggle"],
            json!({"bHidePlayer": true}),
        ),
        (
            "Engine.SeqAct_ToggleHUD",
            vec!["Show", "Hide", "Toggle"],
            json!({}),
        ),
        (
            "Engine.SeqAct_SetCameraTarget",
            vec!["In"],
            json!({"CameraTarget": {"$obj": null}}),
        ),
        (
            "GFxUI.GFxAction_OpenMovie",
            vec!["In"],
            json!({"Movie": {"$obj": "M.Movie"}}),
        ),
        (
            "asamu.SeqAction_GFx_CustomInvoke_AS3_Menu",
            vec!["In"],
            json!({"FunctionPath": "root.screen", "InvokeFunction": "F"}),
        ),
        (
            "asamu.SeqAct_ShowTitleLogo",
            vec!["Show", "Hide"],
            json!({}),
        ),
        (
            "asamu.SeqAct_UnlockASAMUAchievement",
            vec!["In"],
            json!({"achievementToUnlock": "ACH"}),
        ),
        (
            "asamu.SeqAct_ToggleCrosshair",
            vec!["Show", "Hide"],
            json!({"Fade": false}),
        ),
        (
            "asamu.SeqAct_ToggleRestartFromCheckpointOption",
            vec!["Enable", "Disable"],
            json!({}),
        ),
        (
            "asamu.SeqAct_DisablePauseMenu",
            vec!["Enable Pausemenu", "Disable Pausemenu"],
            json!({}),
        ),
        (
            "asamu.SeqAct_SetLookAtTarget",
            vec!["LookAt", "StopLookAt"],
            json!({}),
        ),
        (
            "asamu.SeqAct_SetVelocityConeMaterial",
            vec!["In"],
            json!({"Mat": {"$obj": "M.Mat"}}),
        ),
        (
            "asamu.SeqAct_EditMultiplierForAllTracks",
            vec!["In"],
            json!({"multiplierID": "Mute", "Multiplier": 0.0}),
        ),
        (
            "asamu.SeqAct_SetAdaptiveTrackVolumeMultiplier",
            vec!["In"],
            json!({"trackID": "T", "multiplierID": "Mute"}),
        ),
        (
            "asamu.SeqAct_ShowTutorialPopup",
            vec!["In"],
            json!({"msg": {"$struct": "tutorialMsg"}, "Id": 0}),
        ),
        (
            "asamu.SeqAct_HideTutorialPopup",
            vec!["In"],
            json!({"Id": 0}),
        ),
        ("asamu.SeqAct_PlaySuitOnAnimation", vec!["In"], json!({})),
        (
            "asamu.SeqAct_SetGameFinished",
            vec!["True", "False"],
            json!({"gameFinished": false}),
        ),
        (
            "asamu.SeqAct_StartWorm",
            vec!["Start"],
            json!({"wormPawn": {"$obj": "T.Worm"}}),
        ),
        (
            "asamu.SeqAct_PauseWorm",
            vec!["UnPause", "Pause"],
            json!({"wormPawn": {"$obj": "T.Worm"}}),
        ),
        (
            "asamu.SeqAct_ShutDownWorm",
            vec!["Start"],
            json!({"wormPawn": {"$obj": "T.Worm"}}),
        ),
        (
            "asamu.SeqAct_ToggleFollowCollision",
            vec!["Enable", "Disable"],
            json!({"followCollisionActor": {"$obj": "T.Skel"}}),
        ),
        (
            "asamu.SeqAct_AddAdaptiveTracks",
            vec![],
            json!({"tracksToAdd": []}),
        ),
    ];
    for (class, inputs, params) in pres {
        let a = node(2, class, "action")
            .inputs(&inputs)
            .out(&[("Out", &[])])
            .params(params)
            .auto();
        let input = 0;
        let (r, h, out) = if inputs.is_empty() {
            // Started by the adaptive music manager at level start.
            start_with(a, 0, vec![], vec![], 2)
        } else {
            start_with(a, input, vec![], vec![], 2)
        };
        assert!(!out.is_empty(), "{class}: no output");
        assert!(
            !out.iter().any(|o| matches!(o, Output::Unhandled { .. })),
            "{class}"
        );
        assert!(r.errors().is_empty(), "{class}: {:?}", r.errors());
        assert!(h.calls.is_empty(), "{class}: {:?}", h.calls);
    }
}

/// `SeqAct_PlayCameraAnim` (native `Activated`): Play wins over Stop, the
/// output carries every play parameter, and the action does nothing without
/// an animation or without a player among its targets.
#[test]
fn play_camera_anim_needs_an_animation_and_a_player_target() {
    let action = |params: Value, target: bool| {
        let vars: &[usize] = if target { &[3] } else { &[] };
        node(2, "Engine.SeqAct_PlayCameraAnim", "action")
            .inputs(&["Play", "Stop"])
            .out(&[("Out", &[])])
            .vars(&[("Target", Some("Targets"), vars)])
            .params(params)
            .auto()
    };
    let full = json!({"CameraAnim": {"$obj": "Pkg.Anims.Nod"}, "Rate": 2.0, "IntensityScale": 0.5,
                      "BlendInTime": 0.1, "BlendOutTime": 0.4, "bLoop": true,
                      "bRandomStartTime": true, "Targets": []});
    let cam = |out: &[Output]| -> Vec<Output> {
        out.iter()
            .filter(|o| matches!(o, Output::CameraAnim { .. }))
            .cloned()
            .collect()
    };
    // Play, with the player as target.
    let (r, _, out) = start_with(action(full.clone(), true), 0, vec![player(3)], vec![], 2);
    assert_eq!(
        cam(&out),
        vec![Output::CameraAnim {
            play: true,
            anim: Some("Pkg.Anims.Nod".into()),
            looping: true,
            rate: 2.0,
            scale: 0.5,
            blend_in: 0.1,
            blend_out: 0.4,
            random_start: true,
        }]
    );
    assert!(r.errors().is_empty(), "{:?}", r.errors());
    // Stop.
    let (_, _, out) = start_with(action(full.clone(), true), 1, vec![player(3)], vec![], 2);
    assert!(matches!(
        cam(&out).as_slice(),
        [Output::CameraAnim { play: false, anim: Some(a), .. }] if a == "Pkg.Anims.Nod"
    ));
    // Both inputs in one update: Play.
    let g = graph(
        vec![
            seq(0, &[1, 2, 3]),
            loaded(1, &[(2, 1), (2, 0)]),
            action(full.clone(), true),
            player(3),
        ],
        vec![],
    );
    let mut r = rt(g);
    let mut h = Rec::default();
    let out = ticks(&mut r, &mut h, 2);
    assert!(matches!(
        cam(&out).as_slice(),
        [Output::CameraAnim { play: true, .. }]
    ));
    // No target, or no animation: nothing.
    let (r, _, out) = start_with(action(full, false), 0, vec![], vec![], 2);
    assert!(cam(&out).is_empty(), "{out:?}");
    assert!(r.errors().is_empty(), "{:?}", r.errors());
    let (_, _, out) = start_with(
        action(json!({"Rate": 1.0, "Targets": []}), true),
        0,
        vec![player(3)],
        vec![],
        2,
    );
    assert!(cam(&out).is_empty(), "{out:?}");
}

#[test]
fn time_trial_actions_need_the_time_trial_game() {
    let make = |tt: bool| {
        let g = graph(
            vec![
                seq(0, &[1, 2, 3, 4, 5]),
                loaded(1, &[(2, 0), (3, 0), (4, 0)]),
                node(2, "asamu.SeqAct_StartTimeTrial", "action")
                    .inputs(&["Start"])
                    .out(&[("Out", &[])])
                    .auto(),
                node(3, "asamu.SeqAct_EndTimeTrial", "action")
                    .inputs(&["End"])
                    .out(&[("Out", &[])])
                    .auto(),
                node(4, "asamu.SeqCond_IsTimeTrial", "condition")
                    .inputs(&["In"])
                    .out(&[("Yes", &[(5, 0)]), ("No", &[])]),
                act(5, "asamu.SeqAct_SetMaxGrapples").params(json!({"Grapples": 1})),
            ],
            vec![],
        );
        let mut r = rt(g);
        let mut h = Rec {
            time_trial: tt,
            ..Rec::default()
        };
        let out = ticks(&mut r, &mut h, 2);
        (out, h.calls)
    };
    let (out, calls) = make(false);
    assert!(out.is_empty() && calls.is_empty());
    let (out, calls) = make(true);
    assert!(out.contains(&Output::TimeTrial { start: true }));
    assert!(out.contains(&Output::TimeTrial { start: false }));
    assert_eq!(calls, vec!["max_grapples 1"]);
}

#[test]
fn play_sound_fires_out_then_finished_or_stopped() {
    let g = Graph::from_json_slice(
        &serde_json::to_vec(&json!({
            "format": asamu_kismet::RUNTIME_FORMAT, "version": 1, "package": "T",
            "nodes": [
                seq(0, &[1, 2, 3, 4]).0,
                loaded(1, &[(2, 0)]).0,
                node(2, "Engine.SeqAct_PlaySound", "action")
                    .inputs(&["Play", "Stop"])
                    .out(&[("Out", &[(3, 0)]), ("Finished", &[(4, 0)]), ("Stopped", &[]), ("BeforeEnd", &[])])
                    .params(json!({"PlaySound": {"$obj": "S.Cue"}, "ExtraDelay": 0.0, "VolumeMultiplier": 1.0,
                                   "PitchMultiplier": 1.0, "BeforeEndTime": 0.0}))
                    .auto()
                    .latent_base().0,
                act(3, "asamu.SeqAct_SetMaxGrapples").params(json!({"Grapples": 1})).0,
                act(4, "asamu.SeqAct_SetMaxGrapples").params(json!({"Grapples": 2})).0
            ],
            "sounds": {"S.Cue": {"duration": 0.5, "first_wave_duration": 0.5}}
        }))
        .unwrap(),
    )
    .unwrap();
    let mut r = rt(g);
    let mut h = Rec::default();
    let out = ticks(&mut r, &mut h, 2);
    assert!(
        out.iter()
            .any(|o| matches!(o, Output::PlaySound { cue: Some(c), .. } if c == "S.Cue"))
    );
    assert_eq!(h.calls, vec!["max_grapples 1"]);
    ticks(&mut r, &mut h, 40);
    assert_eq!(h.calls, vec!["max_grapples 1", "max_grapples 2"]);
}

#[test]
fn camera_fade_fires_finished_after_its_time() {
    let a = node(2, "Engine.SeqAct_CameraFade", "action")
        .inputs(&["In"])
        .out(&[("Out", &[]), ("Finished", &[(3, 0)])])
        .params(json!({"FadeOpacity": 1.0, "FadeTime": 0.25}))
        .no_auto()
        .latent();
    let (r, h, out) = start_with(
        a,
        0,
        vec![act(3, "asamu.SeqAct_SetMaxGrapples").params(json!({"Grapples": 1}))],
        vec![],
        20,
    );
    assert!(out.iter().any(|o| matches!(o, Output::CameraFade { .. })));
    assert_eq!(h.calls, vec!["max_grapples 1"]);
    assert!(!r.is_active(2));
}

// ------------------------------------------------------------------ ASAMU actions

#[test]
fn ability_and_story_actions_call_the_host() {
    let cases: Vec<(&str, Vec<&str>, usize, Value, &str)> = vec![
        (
            "asamu.SeqAct_SetMaxGrapples",
            vec!["In"],
            0,
            json!({"Grapples": -1}),
            "max_grapples -1",
        ),
        (
            "asamu.SeqAct_ToggleGrapple",
            vec!["In"],
            0,
            json!({"Enable": true}),
            "grapple true",
        ),
        (
            "asamu.SeqAct_ToggleRocketBoots",
            vec!["In"],
            0,
            json!({"Enable": false}),
            "boots false",
        ),
        (
            "asamu.SeqAct_ToggleVisibleGrapple",
            vec!["Hide", "Show"],
            0,
            json!({"AnimateHand": true, "Visibility": false}),
            "hide_gun true true false",
        ),
        (
            "asamu.SeqAct_ToggleVisibleGrapple",
            vec!["Hide", "Show"],
            1,
            json!({"AnimateHand": false, "Visibility": true}),
            "hide_gun false false true",
        ),
        (
            "asamu.SeqAct_ToggleZoomAvailable",
            vec!["Enable", "Disable"],
            1,
            json!({}),
            "zoom false",
        ),
        (
            "asamu.SeqAct_ToggleSpawnInStoryMode",
            vec!["Enable", "Disable"],
            0,
            json!({}),
            "spawn_story true",
        ),
        (
            "asamu.SeqAct_ToggleFallingRocksActive",
            vec!["Activate", "Disable"],
            0,
            json!({}),
            "rocks true",
        ),
        (
            "asamu.SeqAct_ToggleStoryMode",
            vec!["Enable", "Disable", "Toggle"],
            0,
            json!({}),
            "story true",
        ),
        (
            "asamu.SeqAct_ToggleStoryMode",
            vec!["Enable", "Disable", "Toggle"],
            2,
            json!({}),
            "story true",
        ),
    ];
    for (class, inputs, input, params, want) in cases {
        let a = node(2, class, "action")
            .inputs(&inputs)
            .out(&[("Out", &[])])
            .params(params)
            .auto();
        let (_, h, _) = start_with(a, input, vec![], vec![], 2);
        assert_eq!(h.calls, vec![want.to_owned()], "{class} input {input}");
    }
    // Disabling story mode outside story mode does nothing.
    let a = node(2, "asamu.SeqAct_ToggleStoryMode", "action")
        .inputs(&["Enable", "Disable", "Toggle"])
        .out(&[("Out", &[])])
        .auto();
    let (_, h, _) = start_with(a, 1, vec![], vec![], 2);
    assert!(h.calls.is_empty());
}

#[test]
fn checkpoint_and_attractor_actions_use_their_actor() {
    let actors = vec![
        actor("Cp", "asamu.ASAMUCheckpoint", "checkpoint", 1),
        actor(
            "Pad",
            "asamu.ASAMUTelePad_Attractor",
            "tele_pad_attractor",
            2,
        ),
    ];
    let a = act(2, "asamu.SeqAct_TriggerCheckpoint")
        .params(json!({"checkpointPositionObject": {"$obj": path("Cp")}}));
    let (_, h, _) = start_with(a, 0, vec![], actors.clone(), 2);
    assert_eq!(h.calls, vec!["checkpoint Cp"]);
    let a = node(2, "asamu.SeqAct_ToggleCheckpointEnable", "action")
        .inputs(&["Activate", "Disable"])
        .out(&[("Out", &[])])
        .vars(&[("Checkpoint", Some("Checkpoint"), &[3])])
        .params(json!({"Checkpoint": {"$obj": null}}))
        .auto();
    let (_, h, _) = start_with(a, 1, vec![var_obj(3, Some(&path("Cp")))], actors.clone(), 2);
    assert_eq!(h.calls, vec!["checkpoint_enabled Cp false"]);
    // The attractor fires Out at once and Finished when the pad ends.
    let a = node(2, "asamu.SeqAct_ToggleAttractor", "action")
        .inputs(&["In"])
        .out(&[("Out", &[(3, 0)]), ("Finished", &[(4, 0)])])
        .vars(&[("Attractor", Some("Attractor"), &[5])])
        .params(json!({"Attractor": {"$obj": null}}))
        .no_auto();
    let (mut r, mut h, _) = start_with(
        a,
        0,
        vec![
            act(3, "asamu.SeqAct_SetMaxGrapples").params(json!({"Grapples": 1})),
            act(4, "asamu.SeqAct_SetMaxGrapples").params(json!({"Grapples": 2})),
            var_obj(5, Some(&path("Pad"))),
        ],
        actors,
        3,
    );
    assert_eq!(h.calls, vec!["attractor Pad", "max_grapples 1"]);
    let pad = r.graph().actor_by_path(&path("Pad")).unwrap();
    r.attractor_finished(pad);
    ticks(&mut r, &mut h, 1);
    assert_eq!(h.calls.last().map(String::as_str), Some("max_grapples 2"));
}

#[test]
fn save_strings_round_trip_and_branch() {
    let g = graph(
        vec![
            seq(0, &(1..=7).collect::<Vec<_>>()),
            loaded(1, &[(2, 0)]),
            node(2, "asamu.SeqAct_GetSaveStringValue", "action")
                .inputs(&["In"])
                .out(&[("FoundSave", &[(5, 0)]), ("DidntFindSave", &[(3, 0)])])
                .vars(&[("ID", Some("Id"), &[]), ("Value", Some("Value"), &[6])])
                .params(json!({"Id": "Door", "Value": 0}))
                .no_auto(),
            act(3, "asamu.SeqAct_EditOrAddSaveString")
                .out(&[("Out", &[(4, 0)])])
                .params(json!({"Id": "Door", "Value": 7})),
            node(4, "asamu.SeqAct_GetSaveStringValue", "action")
                .inputs(&["In"])
                .out(&[("FoundSave", &[(5, 0)]), ("DidntFindSave", &[])])
                .vars(&[("ID", Some("Id"), &[]), ("Value", Some("Value"), &[6])])
                .params(json!({"Id": "Door", "Value": 0}))
                .no_auto(),
            act(5, "asamu.SeqAct_SetMaxGrapples").params(json!({"Grapples": 1})),
            var_int(6, 0),
            var_int(7, 0),
        ],
        vec![],
    );
    let mut r = rt(g);
    let mut h = Rec::default();
    ticks(&mut r, &mut h, 3);
    assert_eq!(r.save_strings().get("Door"), Some(&7));
    assert_eq!(r.var_value(6), Some(&KValue::Int(7)));
    assert_eq!(h.calls, vec!["max_grapples 1"]);
}

#[test]
fn narrator_lines_play_queue_and_finish() {
    let g = Graph::from_json_slice(
        &serde_json::to_vec(&json!({
            "format": asamu_kismet::RUNTIME_FORMAT, "version": 1, "package": "T",
            "nodes": [
                seq(0, &[1, 2, 3, 4, 5, 6, 7]).0,
                loaded(1, &[(2, 0), (3, 0)]).0,
                node(2, "asamu.SeqAct_NarratorLine", "action")
                    .inputs(&["AddLine", "RemoveLine"])
                    .out(&[("Out", &[]), ("NO", &[]), ("FinishedLine", &[(4, 0)])])
                    .params(json!({"Id": "A", "Cue": {"$obj": "N.A"}, "CueVolume": 1.0, "Delay": 0.0}))
                    .no_auto().latent_base().0,
                node(3, "asamu.SeqAct_NarratorLine", "action")
                    .inputs(&["AddLine", "RemoveLine"])
                    .out(&[("Out", &[]), ("NO", &[]), ("FinishedLine", &[(5, 0)])])
                    .params(json!({"Id": "B", "Cue": {"$obj": "N.B"}, "CueVolume": 1.0, "Delay": 0.25}))
                    .no_auto().latent_base().0,
                act(4, "asamu.SeqAct_SetMaxGrapples").params(json!({"Grapples": 1})).0,
                act(5, "asamu.SeqAct_SetMaxGrapples").params(json!({"Grapples": 2})).0,
                node(6, "asamu.SeqEvent_NarratorEvents", "event")
                    .out(&[("Started narrating", &[]), ("Finished narrating", &[(7, 0)])]).0,
                act(7, "asamu.SeqAct_SetMaxGrapples").params(json!({"Grapples": 3})).0
            ],
            "sounds": {"N.A": {"duration": 0.5}, "N.B": {"duration": 0.5}}
        }))
        .unwrap(),
    )
    .unwrap();
    let mut r = rt(g);
    let mut h = Rec::default();
    let out = ticks(&mut r, &mut h, 120);
    let lines: Vec<&str> = out
        .iter()
        .filter_map(|o| match o {
            Output::NarratorLine { id, .. } => Some(id.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(lines, vec!["A", "B"]);
    assert_eq!(
        h.calls,
        vec!["max_grapples 1", "max_grapples 2", "max_grapples 3"]
    );
}

#[test]
fn rotation_to_player_and_destroy_and_collision_targets() {
    let actors = vec![actor("Statue", "Engine.InterpActor", "interp_actor", 3)];
    let a = act(2, "asamu.SeqAct_SetRotationToPlayerRotation").params(
        json!({"Pitch": false, "Yaw": true, "Roll": true, "Obj": {"$obj": path("Statue")}}),
    );
    let (_, h, _) = start_with(a, 0, vec![], actors.clone(), 2);
    assert_eq!(
        h.xf.get("Statue"),
        Some(&([3.0, 0.0, 0.0], Some([0, 200, 300])))
    );
    let a = act(2, "Engine.SeqAct_Destroy")
        .vars(&[("Target", Some("Targets"), &[3])])
        .params(json!({"Targets": []}));
    let (_, h, out) = start_with(
        a,
        0,
        vec![var_obj(3, Some(&path("Statue")))],
        actors.clone(),
        2,
    );
    assert_eq!(h.calls, vec!["destroy Statue"]);
    assert!(
        out.iter()
            .any(|o| matches!(o, Output::ActorDestroyed { .. }))
    );
    let a = act(2, "Engine.SeqAct_ChangeCollision")
        .vars(&[("Target", Some("Targets"), &[3])])
        .params(json!({"bCollideActors": true, "bBlockActors": false, "Targets": []}));
    let (_, h, _) = start_with(a, 0, vec![var_obj(3, Some(&path("Statue")))], actors, 2);
    assert_eq!(h.calls, vec!["collision Statue true false"]);
}

#[test]
fn gate_log_and_unknown_classes() {
    let g = graph(
        vec![
            seq(0, &[1, 2, 3, 4]),
            loaded(1, &[(2, 0), (4, 0)]),
            node(2, "Engine.SeqAct_Gate", "action")
                .inputs(&["In", "Open", "Close", "Toggle"])
                .out(&[("Out", &[(3, 0)])])
                .params(json!({"bOpen": true}))
                .no_auto(),
            act(3, "asamu.SeqAct_SetMaxGrapples").params(json!({"Grapples": 1})),
            act(4, "Mod.SeqAct_Mystery"),
        ],
        vec![],
    );
    let mut r = rt(g);
    let mut h = Rec::default();
    let out = ticks(&mut r, &mut h, 2);
    assert_eq!(h.calls, vec!["max_grapples 1"]);
    assert!(out.iter().any(|o| matches!(o, Output::Unhandled { .. })));
    assert_eq!(r.stats().unhandled.get("Mod.SeqAct_Mystery"), Some(&1));
    assert!(OpClass::ALL.contains(&OpClass::Gate));
}

#[test]
fn random_variables_are_deterministic_and_in_range() {
    let make = || {
        let g = graph(
            vec![
                seq(0, &[1, 2, 3, 4, 5]),
                loaded(1, &[(2, 0)]),
                act(2, "Engine.SeqAct_AddInt")
                    .out(&[("Out", &[(2, 0)])])
                    .vars(&[
                        ("A", Some("ValueA"), &[3]),
                        ("IntResult", Some("IntResult"), &[4]),
                    ])
                    .params(json!({"ValueA": 0, "ValueB": 0, "IntResult": 0})),
                node(3, "Engine.SeqVar_RandomInt", "variable").params(json!({"Min": 2, "Max": 5})),
                var_int(4, 0),
                node(5, "Engine.SeqVar_RandomFloat", "variable")
                    .params(json!({"Min": -1.0, "Max": 1.0})),
            ],
            vec![],
        );
        let mut r = rt(g);
        let mut h = Rec::default();
        let mut seen = Vec::new();
        for _ in 0..20 {
            r.force_input(2, 0);
            ticks(&mut r, &mut h, 1);
            seen.push(r.var_value(4).cloned());
        }
        seen
    };
    let a = make();
    assert_eq!(a, make());
    assert!(
        a.iter()
            .all(|v| matches!(v, Some(KValue::Int(n)) if (2..=5).contains(n)))
    );
}

// ------------------------------------------------------------------ Matinee

#[test]
fn interp_plays_moves_fires_events_and_completes() {
    let g = graph(
        vec![
            seq(0, &[1, 2, 3, 4, 5, 6]),
            loaded(1, &[(2, 0)]),
            node(2, "Engine.SeqAct_Interp", "action")
                .inputs(&["Play", "Reverse", "Stop", "Pause", "Change Dir"])
                .out(&[
                    ("Completed", &[(5, 0)]),
                    ("Reversed", &[]),
                    ("Open", &[(6, 0)]),
                ])
                .vars(&[("Data", None, &[3]), ("Door", None, &[4])])
                .auto()
                .latent_base(),
            node(3, "Engine.InterpData", "variable").path("T.Data"),
            var_obj(4, Some(&path("Door"))),
            act(5, "asamu.SeqAct_SetMaxGrapples").params(json!({"Grapples": 1})),
            act(6, "asamu.SeqAct_SetMaxGrapples").params(json!({"Grapples": 2})),
        ],
        vec![actor("Door", "Engine.InterpActor", "interp_actor", 10)],
    );
    let key = |t: f32, z: f32| json!({"in": t, "out": [0.0, 0.0, z], "arrive": [0.0, 0.0, 0.0], "leave": [0.0, 0.0, 0.0], "mode": "linear"});
    let mut m = MatineeSet::default();
    m.add_json(
        &serde_json::to_vec(&json!({
            "format": "asamu-matinee", "version": 1, "package": "T",
            "actions": [{"path": "T.I", "node": 2, "scope": "level", "interp_data": "T.Data",
                         "bindings": [{"link": 1, "label": "Door", "group": "Door",
                                       "targets": [{"variable": "T.V", "object": path("Door")}]}]}],
            "interp_data": [{"path": "T.Data", "length": 1.0, "groups": [
                {"kind": "group", "name": "Door", "tracks": [
                    {"class": "Engine.InterpTrackMove", "data": {"type": "move", "move_frame": "relative_to_initial",
                        "pos": {"points": [key(0.0, 0.0), key(1.0, 100.0)]},
                        "euler": {"points": [key(0.0, 0.0)]}}},
                    {"class": "Engine.InterpTrackEvent", "data": {"type": "event", "fire_forwards": true,
                        "keys": [{"time": 0.5, "name": "Open"}]}}]},
                {"kind": "director", "name": "Dir", "tracks": [
                    {"class": "Engine.InterpTrackFade", "data": {"type": "fade",
                        "curve": {"points": [{"in": 0.0, "out": 0.0, "arrive": 0.0, "leave": 0.0, "mode": "linear"},
                                             {"in": 1.0, "out": 1.0, "arrive": 0.0, "leave": 0.0, "mode": "linear"}]}}}]}]}]
        }))
        .unwrap(),
        0,
    )
    .unwrap();
    let mut r = rt_m(g, m);
    let mut h = Rec::default();
    let out = ticks(&mut r, &mut h, 40);
    assert_eq!(h.calls, vec!["max_grapples 2"], "event key at 0.5 s");
    let (loc, _) = h.xf["Door"];
    assert!(loc[2] > 50.0 && loc[2] < 70.0, "{loc:?}");
    ticks(&mut r, &mut h, 30);
    let (loc, rot) = h.xf["Door"];
    assert!((loc[2] - 100.0).abs() < 1e-3 && loc[0] == 10.0, "{loc:?}");
    assert_eq!(rot, Some([0, 0, 0]));
    assert_eq!(h.calls, vec!["max_grapples 2", "max_grapples 1"]);
    assert!(!r.is_active(2));
    assert!(out.iter().any(|o| matches!(o, Output::MatineeFade { .. })));
    // Reverse plays back to the start; `Reversed` has no links.
    r.force_input(2, 1);
    ticks(&mut r, &mut h, 70);
    let (loc, _) = h.xf["Door"];
    assert!(loc[2].abs() < 1e-3, "{loc:?}");
    assert_eq!(r.interp_position(2), Some(0.0));
}

// ------------------------------------------------------------------ verification additions

/// Three activations of one event in one update: the second and third are
/// queued while it is pending and unqueued one at a time when the stack
/// runs empty (decompiled `USequence::ExecuteActiveOps`). A `SeqAct_Latent`
/// event already updated in this frame is deferred to the next, so the
/// activations spread over frames instead of spinning.
#[test]
fn queued_event_activations_unqueue_one_at_a_time_across_frames() {
    let remote =
        |id: usize| act(id, "Engine.SeqAct_ActivateRemoteEvent").params(json!({"EventName": "Go"}));
    let g = graph(
        vec![
            seq(0, &[1, 2, 3, 4, 5, 6]),
            loaded(1, &[(2, 0), (3, 0), (4, 0)]),
            remote(2),
            remote(3),
            remote(4),
            // Hostile flags: an event claiming to be a latent action.
            node(5, "Engine.SeqEvent_RemoteEvent", "event")
                .out(&[("Out", &[(6, 0)])])
                .params(json!({"EventName": "Go"}))
                .latent_base(),
            act(6, "Engine.SeqAct_Log"),
        ],
        vec![],
    );
    let mut r = rt(g);
    let mut h = Rec::default();
    let mut counts = Vec::new();
    for _ in 0..4 {
        ticks(&mut r, &mut h, 1);
        counts.push(r.activate_count(6));
    }
    assert_eq!(counts, vec![1, 2, 3, 3]);
    assert!(r.errors().is_empty(), "{:?}", r.errors());
}

/// `SeqAct_CameraFade` is latent but not a `SeqAct_Latent`: pulsed again
/// after it ran in this update, it is processed (and counts down) again in
/// the same update instead of waiting for the next (the engine's deferral
/// tests the `SeqAct_Latent` class).
#[test]
fn latent_actions_outside_seqact_latent_run_again_in_the_same_update() {
    let fade = node(2, "Engine.SeqAct_CameraFade", "action")
        .inputs(&["In"])
        .out(&[("Out", &[]), ("Finished", &[(4, 0)])])
        .params(json!({"FadeOpacity": 1.0, "FadeTime": DT * 2.5}))
        .no_auto()
        .latent();
    let g = graph(
        vec![
            seq(0, &[1, 2, 3, 4]),
            loaded(1, &[(2, 0), (3, 0)]),
            fade,
            act(3, "Engine.SeqAct_Log").out(&[("Out", &[(2, 0)])]),
            act(4, "asamu.SeqAct_SetMaxGrapples").params(json!({"Grapples": 1})),
        ],
        vec![],
    );
    let mut r = rt(g);
    let mut h = Rec::default();
    ticks(&mut r, &mut h, 1);
    assert!(h.calls.is_empty());
    // Two count-downs in update 1 leave half a frame: done in update 2.
    ticks(&mut r, &mut h, 1);
    assert_eq!(h.calls, vec!["max_grapples 1"]);
}

/// `USeqAct_MultiLevelStreaming::Activated` does not call the latent base,
/// so the action is never "aborted": with two outputs it still finishes on
/// output 0.
#[test]
fn multi_level_streaming_finishes_on_its_first_output() {
    let a = node(2, "Engine.SeqAct_MultiLevelStreaming", "action")
        .inputs(&["Load", "Unload"])
        .out(&[("Finished", &[(3, 0)]), ("Aborted", &[(4, 0)])])
        .params(json!({"Levels": [{"$struct": "LevelStreamingNameCombo", "LevelName": "Sub"}]}))
        .auto()
        .latent_base();
    let (_, h, _) = start_with(
        a,
        0,
        vec![
            act(3, "asamu.SeqAct_SetMaxGrapples").params(json!({"Grapples": 1})),
            act(4, "asamu.SeqAct_SetMaxGrapples").params(json!({"Grapples": 9})),
        ],
        vec![],
        3,
    );
    // Both outputs of an auto-activating class would fire through the
    // generic path; the latent deactivation picks exactly one.
    assert_eq!(h.calls, vec!["stream Sub true false", "max_grapples 1"]);
}

/// An attached actor's move track is evaluated in its base's current frame
/// (`GetMoveRefFrame` reads the base matrix on every call), so it follows a
/// base that another Matinee moves.
#[test]
fn attached_matinee_actor_follows_its_moving_base() {
    let interp = |id: usize, out_link: Option<(usize, usize)>, data: usize, obj: usize| {
        let links: Vec<(usize, usize)> = out_link.into_iter().collect();
        node(id, "Engine.SeqAct_Interp", "action")
            .inputs(&["Play", "Reverse", "Stop", "Pause", "Change Dir"])
            .out(&[("Completed", &links), ("Reversed", &[])])
            .vars(&[("Data", None, &[data]), ("G", None, &[obj])])
            .auto()
            .latent_base()
    };
    let g = graph(
        vec![
            seq(0, &[1, 2, 3, 4, 5, 6, 7, 8]),
            loaded(1, &[(2, 0), (5, 0)]),
            interp(2, None, 3, 4),
            node(3, "Engine.InterpData", "variable").path("T.ShipData"),
            var_obj(4, Some(&path("Ship"))),
            interp(5, None, 6, 7),
            node(6, "Engine.InterpData", "variable").path("T.FlagData"),
            var_obj(7, Some(&path("Flag"))),
            act(8, "Engine.SeqAct_Log"),
        ],
        vec![
            actor("Ship", "Engine.InterpActor", "interp_actor", 100),
            json!({"path": path("Flag"), "name": "Flag", "class": "Engine.InterpActor",
                   "kind": "interp_actor", "package": "T", "slot": 7,
                   "location": [100.0, 0.0, 50.0], "base": path("Ship")}),
        ],
    );
    let key = |t: f32, v: [f32; 3]| json!({"in": t, "out": v, "arrive": [0.0, 0.0, 0.0], "leave": [0.0, 0.0, 0.0], "mode": "linear"});
    let action = |node: usize, data: &str, group: &str, obj: &str| {
        json!({"path": format!("T.I{node}"), "node": node, "scope": "level", "interp_data": data,
               "bindings": [{"link": 1, "label": group, "group": group,
                             "targets": [{"variable": "T.V", "object": path(obj)}]}]})
    };
    let data = |p: &str, group: &str, end: [f32; 3]| {
        json!({"path": p, "length": 1.0, "groups": [{"kind": "group", "name": group, "tracks": [
            {"class": "Engine.InterpTrackMove", "data": {"type": "move", "move_frame": "relative_to_initial",
                "pos": {"points": [key(0.0, [0.0; 3]), key(1.0, end)]},
                "euler": {"points": [key(0.0, [0.0; 3])]}}}]}]})
    };
    let mut m = MatineeSet::default();
    m.add_json(
        &serde_json::to_vec(&json!({
            "format": "asamu-matinee", "version": 1, "package": "T",
            "actions": [action(2, "T.ShipData", "G", "Ship"), action(5, "T.FlagData", "G", "Flag")],
            "interp_data": [data("T.ShipData", "G", [1000.0, 0.0, 0.0]),
                            data("T.FlagData", "G", [0.0, 0.0, 30.0])]
        }))
        .unwrap(),
        0,
    )
    .unwrap();
    let mut r = rt_m(g, m);
    let mut h = Rec::default();
    ticks(&mut r, &mut h, 90);
    let (ship, _) = h.xf["Ship"];
    let (flag, _) = h.xf["Flag"];
    assert!((ship[0] - 1100.0).abs() < 1e-2, "{ship:?}");
    // The flag rose 30 on its own track and travelled 1000 with the ship.
    assert!(
        (flag[0] - 1100.0).abs() < 1e-2 && (flag[2] - 80.0).abs() < 1e-2,
        "{flag:?}"
    );
    assert!(r.errors().is_empty(), "{:?}", r.errors());
}

/// Event and sound keys placed in the director group fire like any group's
/// (the shipped maps keep 11 event tracks there, e.g. TheCore's narrator
/// cues).
#[test]
fn director_group_event_and_sound_keys_fire() {
    let g = graph(
        vec![
            seq(0, &[1, 2, 3, 4]),
            loaded(1, &[(2, 0)]),
            node(2, "Engine.SeqAct_Interp", "action")
                .inputs(&["Play", "Reverse", "Stop", "Pause", "Change Dir"])
                .out(&[("Completed", &[]), ("Reversed", &[]), ("Line_1", &[(4, 0)])])
                .vars(&[("Data", None, &[3])])
                .auto()
                .latent_base(),
            node(3, "Engine.InterpData", "variable").path("T.Data"),
            act(4, "asamu.SeqAct_SetMaxGrapples").params(json!({"Grapples": 4})),
        ],
        vec![],
    );
    let mut m = MatineeSet::default();
    m.add_json(
        &serde_json::to_vec(&json!({
            "format": "asamu-matinee", "version": 1, "package": "T",
            "actions": [{"path": "T.I", "node": 2, "scope": "level", "interp_data": "T.Data", "bindings": []}],
            "interp_data": [{"path": "T.Data", "length": 1.0, "groups": [
                {"kind": "director", "name": "DirGroup", "tracks": [
                    {"class": "Engine.InterpTrackEvent", "data": {"type": "event", "fire_forwards": true,
                        "keys": [{"time": 0.25, "name": "Line_1"}]}},
                    {"class": "Engine.InterpTrackSound", "data": {"type": "sound",
                        "keys": [{"time": 0.5, "sound": "T.Cue", "volume": 1.0, "pitch": 1.0}]}}]}]}]
        }))
        .unwrap(),
        0,
    )
    .unwrap();
    let mut r = rt_m(g, m);
    let mut h = Rec::default();
    let out = ticks(&mut r, &mut h, 70);
    assert_eq!(h.calls, vec!["max_grapples 4"]);
    assert!(out.iter().any(|o| matches!(
        o,
        Output::MatineeSound { cue: Some(c), actor: None, .. } if c == "T.Cue"
    )));
    assert!(r.errors().is_empty(), "{:?}", r.errors());
}
