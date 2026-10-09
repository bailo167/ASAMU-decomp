//! End-to-end smoke runs of converted maps: the whole game frame (Kismet,
//! Matinee movers, NPCs, the player) driven by a deterministic pseudo-random
//! input script, and the story chain followed through each map's level
//! transition.
//!
//! Everything here is ours (test tooling, not original behaviour). It needs
//! the user's converted data (`asamu-import levels`, `meshes --collision`,
//! `kismet`, `matinee`); `examples/smoke.rs` prints the summaries and the
//! ignored `tests/smoke.rs` asserts them.
//!
//! - [`run_map`]: loads a map with [`crate::load_level_with_kismet`], runs
//!   `ticks` frames of [`InputScript`] input and checks every frame that the
//!   player, the camera, the movers, the Kismet-moved actors and the NPCs
//!   stay finite ([`SmokeSummary::problems`]); outputs, NPC events, deaths
//!   and transitions are counted.
//! - [`chain_step`] / [`follow_story_chain`]: finds the touch events whose
//!   links (through remote events) lead to the map's story exit — an `open
//!   <map>` console command, or the level streaming that brings `TheCore`
//!   into AG-IceCave — teleports the player into that trigger's volume to
//!   simulate reaching the end, and runs until the transition appears. When
//!   the exit is gated on the level's progress (AG-Workshop's exit waits on
//!   its story interactions), the level's story interactions are performed
//!   first, through the player's story-mode fire where it reaches the item.
//!   A disabled exit trigger is enabled the way play does it: the triggers
//!   whose links turn its touch event on ([`enabler_triggers`]) are touched
//!   first and the run waits for what they start. Every shortcut that does
//!   not go through the game (a touch or interaction sent to Kismet
//!   directly, a toggle pulsed by hand) is recorded in the [`ChainStep`].
//!   The credits movie TheCore opens is ended at once (the app shows a
//!   credits screen instead; the Scaleform movie is not ported), as
//!   `SeqEvent_CreditsEnded` needs.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::Path;

use asamu_kismet::{ActorRef, Graph, KValue, OpClass, Output};
use asamu_player::InputFrame;
use asamu_player::world::{CollisionShape, CollisionWorld};
use glam::Vec3;

use crate::npc::NpcEvent;
use crate::save::{ChapterId, FRONT_END_MAP};
use crate::{Game, LevelScript, LevelScriptError, ScriptedTick, load_level_with_kismet};

/// Default seed of [`InputScript`] (ours).
pub const DEFAULT_SEED: u64 = 0x5EED_A5A3_0001_0001;

/// Largest number of graph nodes one trigger search visits.
const SEARCH_LIMIT: usize = 2_000;

/// Frames [`Attempt::enable_through_play`] waits after touching an enabling
/// trigger (ours: 200 s at 60 Hz, longer than the cutscene TheCore's credit
/// trigger waits for, whose `InterpData` is 143.6 s long).
const ENABLE_WAIT_TICKS: u32 = 12_000;

/// Horizontal distances (UU) from a story item at which
/// [`Attempt::interact_by_fire`] looks for a free spot to fire from (ours;
/// all inside the shipped `interactRange`, GRAPPLE.md G-AC-0).
const REACH_DISTANCES: [f32; 4] = [90.0, 130.0, 60.0, 170.0];

/// A deterministic pseudo-random input script (SplitMix64): held movement,
/// sprint, grapple and power-jump patterns that change every 10–90 ticks,
/// small random look deltas every tick, occasional jumps and `use` presses.
/// The same seed always gives the same frames.
#[derive(Clone, Debug)]
pub struct InputScript {
    state: u64,
    hold: InputFrame,
    left: u32,
}

impl InputScript {
    /// A script seeded with `seed`.
    #[must_use]
    pub fn new(seed: u64) -> Self {
        Self {
            state: seed,
            hold: InputFrame::default(),
            left: 0,
        }
    }

    fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// Uniform in `[0, 1)`.
    fn unit(&mut self) -> f32 {
        (self.next_u64() >> 40) as f32 / (1u64 << 24) as f32
    }

    fn chance(&mut self, p: f32) -> bool {
        self.unit() < p
    }

    fn axis(&mut self) -> f32 {
        let u = self.unit();
        if u < 0.6 {
            1.0
        } else if u < 0.75 {
            -1.0
        } else {
            0.0
        }
    }

    /// The next frame's input.
    pub fn next_frame(&mut self) -> InputFrame {
        if self.left == 0 {
            let strafe = self.unit();
            self.hold = InputFrame {
                move_forward: self.axis(),
                move_right: if strafe < 0.2 {
                    -1.0
                } else if strafe < 0.4 {
                    1.0
                } else {
                    0.0
                },
                sprint_held: self.chance(0.3),
                grapple_held: self.chance(0.25),
                power_jump_held: self.chance(0.1),
                jump_held: self.chance(0.2),
                ..InputFrame::default()
            };
            self.left = 10 + (self.next_u64() % 81) as u32;
        }
        self.left -= 1;
        InputFrame {
            look_yaw_delta: (self.unit() - 0.5) * 0.08,
            look_pitch_delta: (self.unit() - 0.5) * 0.04,
            jump_pressed: self.chance(0.03),
            use_pressed: self.chance(0.01),
            ..self.hold
        }
    }
}

/// The variant name of a Kismet output (`PlaySound`, `NarratorLine`, ...).
#[must_use]
pub fn output_kind(o: &Output) -> String {
    variant_name(&format!("{o:?}"))
}

fn variant_name(debug: &str) -> String {
    debug.split([' ', '{', '(']).next().unwrap_or("").to_owned()
}

/// What one [`run_map`] saw.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SmokeSummary {
    /// Map.
    pub map: String,
    /// Frames run.
    pub ticks: u64,
    /// The map has a Kismet export.
    pub has_kismet: bool,
    /// NPC-side actors simulated (worms, Maddies, villagers, collectibles,
    /// story items, glow flowers, foliage).
    pub npc_actors: usize,
    /// Kismet outputs by kind.
    pub outputs: BTreeMap<String, usize>,
    /// NPC events by kind.
    pub npc_events: BTreeMap<String, usize>,
    /// Level transitions (`open <map>`), in order.
    pub transitions: Vec<String>,
    /// Death sequences started.
    pub deaths: usize,
    /// Respawns.
    pub respawns: u32,
    /// Checkpoints activated.
    pub checkpoints: usize,
    /// Matinee movers, and how many moved.
    pub movers: (usize, usize),
    /// The movers that moved: world actor id, start and final location.
    pub moved: Vec<(u32, [f32; 3], [f32; 3])>,
    /// Highest player speed, uu/s.
    pub max_speed: f32,
    /// Non-finite values met (first few), with the tick.
    pub problems: Vec<String>,
    /// Interpreter errors (capped by the runtime).
    pub kismet_errors: Vec<String>,
    /// Host routing problems.
    pub host_errors: Vec<String>,
    /// Final player position, UU.
    pub final_position: [f32; 3],
}

impl SmokeSummary {
    /// One line for logs.
    #[must_use]
    pub fn line(&self) -> String {
        format!(
            "{}: {} ticks, kismet {}, npc actors {}, deaths {}, respawns {}, checkpoints {}, \
             movers {}/{} moved, max speed {:.0}, transitions {:?}, problems {}, kismet errors {}, \
             host errors {}\n  outputs {:?}\n  npc events {:?}",
            self.map,
            self.ticks,
            self.has_kismet,
            self.npc_actors,
            self.deaths,
            self.respawns,
            self.checkpoints,
            self.movers.1,
            self.movers.0,
            self.max_speed,
            self.transitions,
            self.problems.len(),
            self.kismet_errors.len(),
            self.host_errors.len(),
            self.outputs,
            self.npc_events,
        )
    }
}

/// Non-finite values of the game and script after a frame (empty = fine).
#[must_use]
pub fn non_finite(game: &Game, script: Option<&LevelScript>) -> Vec<String> {
    let mut out = Vec::new();
    let p = game.player();
    if !p.is_finite() {
        out.push(format!("player state {:?} {:?}", p.position, p.velocity));
    }
    if !game.eye_position().is_finite() || !game.fov().is_finite() {
        out.push("camera".to_owned());
    }
    if let Some(s) = script {
        for id in s.mover_ids() {
            if s.mover_location(id).is_some_and(|l| !l.is_finite()) {
                out.push(format!("mover {id}"));
            }
        }
        for (id, l, _) in s.moved_actors() {
            if !l.is_finite() {
                out.push(format!("moved actor {id}"));
            }
        }
    }
    if let Some(n) = game.npcs() {
        let rt = n.runtime();
        for w in &rt.worms {
            if !w.current_aim.is_finite() {
                out.push("worm aim".to_owned());
            }
        }
        for v in &rt.villagers {
            if !v.location.is_finite() {
                out.push("villager".to_owned());
            }
        }
    }
    out
}

fn npc_actor_count(game: &Game) -> usize {
    game.npcs().map_or(0, |n| {
        let s = n.scene();
        s.worms.len()
            + s.maddies.len()
            + s.villagers.len()
            + s.collectibles.len()
            + s.story_items.len()
            + s.flowers.len()
            + s.foliage.len()
    })
}

/// Loads `map` with Kismet and NPCs and runs `ticks` frames of
/// [`InputScript`] input seeded with `seed`.
///
/// # Errors
/// The map could not be loaded.
pub fn run_map(
    dir: &Path,
    map: &str,
    ticks: u64,
    seed: u64,
) -> Result<SmokeSummary, LevelScriptError> {
    let (mut game, mut script) = load_level_with_kismet(dir, map)?;
    game.start();
    let mut sum = SmokeSummary {
        map: map.to_owned(),
        has_kismet: script.is_some(),
        npc_actors: npc_actor_count(&game),
        ..SmokeSummary::default()
    };
    let starts: BTreeMap<u32, Vec3> = script
        .as_ref()
        .map(|s| {
            s.mover_ids()
                .into_iter()
                .filter_map(|id| s.mover_location(id).map(|l| (id, l)))
                .collect()
        })
        .unwrap_or_default();
    let mut input = InputScript::new(seed);
    for i in 0..ticks {
        let frame = input.next_frame();
        let report = match script.as_mut() {
            Some(s) => s.tick(&mut game, &frame).map(|t| {
                for o in &t.outputs {
                    *sum.outputs.entry(output_kind(o)).or_insert(0) += 1;
                    if let Output::LevelTransition { map, .. } = o {
                        sum.transitions.push(map.clone());
                    }
                }
                t.report
            }),
            None => game.tick(&frame),
        };
        let Some(report) = report else { break };
        sum.ticks += 1;
        for e in game.npc_events() {
            *sum.npc_events
                .entry(variant_name(&format!("{e:?}")))
                .or_insert(0) += 1;
        }
        if report.died.is_some() {
            sum.deaths += 1;
        }
        if report.checkpoint_activated.is_some() {
            sum.checkpoints += 1;
        }
        sum.max_speed = sum.max_speed.max(game.player().speed());
        if sum.problems.len() < 8 {
            for p in non_finite(&game, script.as_ref()) {
                sum.problems.push(format!("tick {i}: {p}"));
            }
        }
    }
    sum.respawns = game.respawn_count();
    sum.final_position = game.player().position.to_array();
    if let Some(s) = &script {
        sum.moved = starts
            .iter()
            .filter_map(|(id, l)| {
                let n = s.mover_location(*id)?;
                (n != *l).then(|| (*id, l.to_array(), n.to_array()))
            })
            .collect();
        sum.movers = (starts.len(), sum.moved.len());
        sum.kismet_errors = s.runtime().errors().to_vec();
        sum.host_errors = s.host_errors().to_vec();
    }
    Ok(sum)
}

/// The maps converted in `dir` (`levels/<map>.scene.json`), sorted.
#[must_use]
pub fn converted_maps(dir: &Path) -> Vec<String> {
    let mut out: Vec<String> = std::fs::read_dir(dir.join("levels"))
        .map(|rd| {
            rd.filter_map(Result::ok)
                .filter_map(|e| {
                    let name = e.file_name().to_str()?.to_owned();
                    name.strip_suffix(".scene.json").map(str::to_owned)
                })
                .collect()
        })
        .unwrap_or_default();
    out.sort();
    out
}

/// What a trigger search found: a touch event's originator and the story
/// exit its links reach.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExitTrigger {
    /// The touch event node.
    pub event: usize,
    /// Its originator (the trigger or volume).
    pub actor: ActorRef,
    /// Object path of the originator.
    pub path: String,
    /// The exit: `open:<map>` or `stream:<level>` (or `stream` without a
    /// level name).
    pub exit: String,
    /// Ops between the event and the exit.
    pub depth: usize,
}

/// The map of an `open <map>[?options]` command, if any.
fn open_target(command: &str) -> Option<&str> {
    let rest = command.trim();
    let head = rest.get(..5)?;
    if !head.eq_ignore_ascii_case("open ") {
        return None;
    }
    let target = rest.get(5..)?.trim();
    Some(target.split_once('?').map_or(target, |(m, _)| m).trim())
}

/// The story exits of op `id`: `open` commands to a map other than the
/// front end (`open:<map>`), level streaming (`stream`) and the credits
/// movie (`credits`).
fn exits_of(g: &Graph, id: usize) -> Vec<String> {
    let Some(n) = g.node(id) else {
        return Vec::new();
    };
    match n.class {
        OpClass::ConsoleCommand => {
            let mut commands: Vec<&str> = n
                .param("Commands")
                .map(|v| v.items().iter().filter_map(KValue::as_str).collect())
                .unwrap_or_default();
            if let Some(c) = n.param("Command").and_then(KValue::as_str) {
                commands.push(c);
            }
            commands
                .into_iter()
                .filter_map(open_target)
                .filter(|m| !m.is_empty() && !m.eq_ignore_ascii_case(FRONT_END_MAP))
                .map(|m| format!("open:{m}"))
                .collect()
        }
        OpClass::MultiLevelStreaming | OpClass::LevelStreaming => vec!["stream".to_owned()],
        OpClass::OpenMovie
            if n.param("Movie")
                .and_then(KValue::as_obj)
                .is_some_and(is_credits_movie) =>
        {
            vec!["credits".to_owned()]
        }
        _ => Vec::new(),
    }
}

/// The credits movie (`ASAMUFrontEndFlash.asamu_credits`).
fn is_credits_movie(path: &str) -> bool {
    path.to_ascii_lowercase().contains("credits")
}

/// The touch events whose output links, followed through remote events
/// (`SeqAct_ActivateRemoteEvent` → every `SeqEvent_RemoteEvent` of the same
/// name), reach a story exit; nearest first.
#[must_use]
pub fn exit_triggers(g: &Graph) -> Vec<ExitTrigger> {
    search_touches(g, |g, id, _| exits_of(g, id).into_iter().next())
}

/// The touch events (other than `event` itself) whose links reach the
/// "Turn On" or "Toggle" input of a `SeqAct_Toggle` that lists touch event
/// `event` (`exit` = `enable:<event>`); nearest first. In play, touching
/// them (and waiting for what they start, e.g. a Matinee whose `Completed`
/// output runs the toggle) is what enables `event`.
#[must_use]
pub fn enabler_triggers(g: &Graph, event: usize) -> Vec<ExitTrigger> {
    search_touches(g, |g, id, input| {
        let n = g.node(id)?;
        (n.class == OpClass::Toggle
            && (input == 0 || input == 2)
            && n.event_links.iter().any(|l| l.events.contains(&event)))
        .then(|| format!("enable:{event}"))
    })
    .into_iter()
    .filter(|t| t.event != event)
    .collect()
}

/// Breadth-first search from every touch event along output links (and
/// remote events) for the first op `found(graph, op, input it was reached
/// at)` accepts; at most [`SEARCH_LIMIT`] (op, input) pairs per event.
fn search_touches(
    g: &Graph,
    found: impl Fn(&Graph, usize, usize) -> Option<String>,
) -> Vec<ExitTrigger> {
    // Remote events by name (lower case).
    let mut remote: BTreeMap<String, Vec<usize>> = BTreeMap::new();
    for n in &g.nodes {
        if n.class == OpClass::RemoteEvent
            && let Some(name) = n.param("EventName").and_then(KValue::as_str)
        {
            remote
                .entry(name.to_ascii_lowercase())
                .or_default()
                .push(n.id);
        }
    }
    let mut out = Vec::new();
    for ev in g.nodes.iter().filter(|n| n.class == OpClass::Touch) {
        let Some(path) = ev.event.as_ref().and_then(|e| e.originator.clone()) else {
            continue;
        };
        let Some(actor) = g.actor_by_path(&path) else {
            continue;
        };
        let mut seen = BTreeSet::new();
        let mut queue = VecDeque::from([(ev.id, 0usize, 0usize)]);
        let mut hit: Option<(String, usize)> = None;
        while let Some((id, input, depth)) = queue.pop_front() {
            if seen.len() >= SEARCH_LIMIT || !seen.insert((id, input)) {
                continue;
            }
            if depth > 0
                && let Some(what) = found(g, id, input)
            {
                hit = Some((what, depth));
                break;
            }
            let Some(n) = g.node(id) else { continue };
            for o in &n.outputs {
                for (t, i) in &o.links {
                    queue.push_back((*t, *i, depth + 1));
                }
            }
            if n.class == OpClass::ActivateRemoteEvent
                && let Some(name) = n.param("EventName").and_then(KValue::as_str)
            {
                for r in remote
                    .get(&name.to_ascii_lowercase())
                    .map(Vec::as_slice)
                    .unwrap_or(&[])
                {
                    queue.push_back((*r, 0, depth + 1));
                }
            }
        }
        if let Some((exit, depth)) = hit {
            out.push(ExitTrigger {
                event: ev.id,
                actor,
                path,
                exit,
                depth,
            });
        }
    }
    out.sort_by_key(|t| (t.depth, t.event));
    out
}

/// One map of the story chain.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ChainStep {
    /// Map.
    pub map: String,
    /// The next story map (level table order).
    pub expected: Option<String>,
    /// Touch-event originators that lead to the exit (paths).
    pub candidates: Vec<String>,
    /// The trigger(s) whose touch produced the transition, in touch order.
    pub triggers: Vec<String>,
    /// A touch event had to be enabled first (the toggles that enable it were
    /// pulsed; in play a cutscene does that).
    pub enabled_by_toggle: bool,
    /// The triggers whose touch events were enabled that way (paths).
    pub enabled: Vec<String>,
    /// Triggers touched first because their links enable a disabled exit
    /// trigger's touch event, as in play (paths; [`enabler_triggers`]).
    pub enabled_by_touch: Vec<String>,
    /// A teleport did not register a touch; the touch was sent to Kismet
    /// directly.
    pub direct_touch: bool,
    /// The exit needed the level's progress first: every story interaction
    /// Kismet listens for was performed (in graph order, with time for the
    /// cutscenes they start) before the touch.
    pub progressed: bool,
    /// Story interactions performed through the player's story-mode fire
    /// (aimed at the interactable from a free spot within reach; the game's
    /// story-item model reported them): originator paths.
    pub interactions_fired: Vec<String>,
    /// Story interactions the fire could not reach (no free spot with a clear
    /// line, not a story item, or not in story mode), sent to Kismet
    /// directly: originator paths.
    pub interactions_injected: Vec<String>,
    /// Sub-levels Kismet streamed in on the way.
    pub streamed: Vec<String>,
    /// The credits movie opened (and was ended).
    pub credits: bool,
    /// The first story transition seen.
    pub reached: Option<String>,
    /// Frames from the first touch to the transition.
    pub ticks: u64,
    /// Attempts that did not lead out.
    pub notes: Vec<String>,
}

impl ChainStep {
    /// The transition reached the expected map (case-insensitive).
    #[must_use]
    pub fn ok(&self) -> bool {
        match (&self.reached, &self.expected) {
            (Some(r), Some(e)) => r.eq_ignore_ascii_case(e),
            _ => false,
        }
    }
}

/// Where to teleport the player to touch actor `id`: a trigger's centre, or
/// the centre of a volume's first hull's bounds.
fn trigger_point(game: &Game, id: u32) -> Option<Vec3> {
    let map = game.scene_map()?;
    if let Some(t) = map.actors.triggers.iter().find(|t| t.id == id) {
        return Some(t.location);
    }
    let v = map.actors.volumes.iter().find(|v| v.id == id)?;
    let h = v.hulls.first()?;
    Some((h.min + h.max) * 0.5)
}

/// Enables the touch event `event` by pulsing the "Turn On" input of every
/// `SeqAct_Toggle` linked to it; returns whether any was found.
fn enable_event(script: &mut LevelScript, event: usize) -> bool {
    let toggles: Vec<usize> = script
        .runtime()
        .graph()
        .nodes
        .iter()
        .filter(|n| {
            n.class == OpClass::Toggle && n.event_links.iter().any(|l| l.events.contains(&event))
        })
        .map(|n| n.id)
        .collect();
    for t in &toggles {
        script.runtime_mut().force_input(*t, 0);
    }
    !toggles.is_empty()
}

/// A run of the chain search on one fresh load.
struct Attempt<'a> {
    game: Game,
    script: LevelScript,
    step: &'a mut ChainStep,
    /// Frames run since the first touch.
    ticks: u64,
    /// The transition, once seen.
    reached: Option<String>,
    /// Credit triggers already touched.
    credit_touches: BTreeSet<usize>,
    /// Originators the game's story-item model reported as interacted with
    /// (world actor ids).
    interacted: BTreeSet<u32>,
}

impl Attempt<'_> {
    /// One frame; notes transitions, credits and streamed levels. `false`
    /// when the game stopped.
    fn frame(&mut self) -> bool {
        self.step_with(&InputFrame::default()).is_some()
    }

    /// One frame with `input`; returns the tick (`None` when the game
    /// stopped).
    fn step_with(&mut self, input: &InputFrame) -> Option<ScriptedTick> {
        let t = self.script.tick(&mut self.game, input)?;
        self.ticks += 1;
        for e in &t.npc_events {
            if let NpcEvent::ActorInteractedWith { originator } = e {
                self.interacted.insert(*originator);
            }
        }
        for o in &t.outputs {
            match o {
                Output::LevelTransition { map, .. }
                    if !map.eq_ignore_ascii_case(FRONT_END_MAP) && self.reached.is_none() =>
                {
                    self.reached = Some(map.clone());
                }
                Output::OpenMovie {
                    movie: Some(movie), ..
                } if is_credits_movie(movie) => {
                    // The app's credits screen ends; here at once.
                    self.step.credits = true;
                    self.script.runtime_mut().credits_ended();
                }
                _ => {}
            }
        }
        if let Some(map) = self.game.scene_map() {
            let newly: Vec<String> = map
                .levels
                .iter()
                .skip(1)
                .filter(|l| {
                    self.game.is_level_streamed(&l.name)
                        && self.script.runtime().is_level_attached(&l.name)
                        && !self
                            .step
                            .streamed
                            .iter()
                            .any(|s| s.eq_ignore_ascii_case(&l.name))
                })
                .map(|l| l.name.clone())
                .collect();
            self.step.streamed.extend(newly);
        }
        Some(t)
    }

    /// Interacts with story item `id` through the player's story-mode fire
    /// ([`Self::fire_at_item`]): at the item itself, else at its linked
    /// children (a child's interaction notifies its parent). `true` when the
    /// game's story-item model reported the interaction with `id`.
    fn interact_by_fire(&mut self, id: u32) -> bool {
        if !self.game.in_story_mode() {
            return false;
        }
        // Children: the parent's `linkedInteractables` and every item whose
        // `linkedParentActor` is the parent.
        let children: Vec<u32> = self.game.npcs().map_or_else(Vec::new, |n| {
            let items = &n.scene().story_items;
            let mut out: Vec<u32> = items
                .iter()
                .find(|s| s.id == id)
                .map(|s| s.linked_children.clone())
                .unwrap_or_default();
            for s in items.iter().filter(|s| s.linked_parent == Some(id)) {
                if !out.contains(&s.id) {
                    out.push(s.id);
                }
            }
            out
        });
        for target in std::iter::once(id).chain(children) {
            if self.interacted.contains(&id) {
                break;
            }
            self.fire_at_item(target);
        }
        self.interacted.contains(&id)
    }

    /// From the first free spot around story item `id` (rings of
    /// [`REACH_DISTANCES`] at a few heights) whose eye ray hits the item, the
    /// player aims at it and presses fire once. `false` when no such spot
    /// was found (nothing fired).
    fn fire_at_item(&mut self, id: u32) -> bool {
        let Some(location) = self
            .game
            .npcs()
            .and_then(|n| n.scene().story_items.iter().find(|s| s.id == id))
            .map(|s| s.location)
        else {
            return false;
        };
        let shape = CollisionShape {
            radius: self.game.params().movement.capsule_radius.value,
            half_height: self.game.params().movement.capsule_half_height.value,
        };
        let eye_offset = self.game.eye_position() - self.game.player().position;
        for dz in [0.0f32, -40.0, 40.0, -80.0] {
            for d in REACH_DISTANCES {
                for k in 0..16u8 {
                    let a = f32::from(k) * std::f32::consts::TAU / 16.0;
                    let p = location + Vec3::new(a.cos() * d, a.sin() * d, dz);
                    if self.game.world().overlaps(p, shape) {
                        continue;
                    }
                    let eye = p + eye_offset;
                    let to = location - eye;
                    let dist = to.length();
                    if !dist.is_finite() || dist < 1.0 {
                        continue;
                    }
                    let dir = to / dist;
                    let hits_item = self
                        .game
                        .world()
                        .raycast(eye, dir, dist + 100.0)
                        .is_some_and(|h| h.surface.actor == Some(id));
                    if !hits_item {
                        continue;
                    }
                    let player = self.game.player_mut();
                    player.position = p;
                    player.velocity = Vec3::ZERO;
                    player.yaw = dir.y.atan2(dir.x);
                    player.pitch = dir.z.clamp(-1.0, 1.0).asin();
                    player.pawn.force_floor_check = true;
                    let fire = InputFrame {
                        grapple_held: true,
                        ..InputFrame::default()
                    };
                    for input in [fire, InputFrame::default(), InputFrame::default()] {
                        if self.step_with(&input).is_none() {
                            break;
                        }
                    }
                    return true;
                }
            }
        }
        false
    }

    /// Runs up to `n` frames, stopping at a transition.
    fn run(&mut self, n: u64) {
        for _ in 0..n {
            if self.reached.is_some() || !self.frame() {
                return;
            }
        }
    }

    /// Touches `trigger`: when its touch event is disabled, first the way
    /// play enables it ([`Self::enable_through_play`]), else by pulsing the
    /// toggles linked to it; then [`Self::teleport_touch`].
    fn touch(&mut self, trigger: &ExitTrigger) {
        if !self.script.runtime().is_enabled(trigger.event)
            && !self.enable_through_play(trigger)
            && enable_event(&mut self.script, trigger.event)
        {
            self.step.enabled_by_toggle = true;
            self.step.enabled.push(trigger.path.clone());
            self.frame();
        }
        self.teleport_touch(trigger);
    }

    /// Touches the triggers whose links enable `trigger`'s touch event
    /// ([`enabler_triggers`]; enabled ones only, nearest first) and waits up
    /// to [`ENABLE_WAIT_TICKS`] after each for what they start (e.g. a
    /// Matinee whose end runs the toggle). `true` once the event is enabled.
    fn enable_through_play(&mut self, trigger: &ExitTrigger) -> bool {
        let enablers: Vec<ExitTrigger> =
            enabler_triggers(self.script.runtime().graph(), trigger.event)
                .into_iter()
                .filter(|e| self.script.runtime().is_enabled(e.event))
                .take(3)
                .collect();
        for e in &enablers {
            self.teleport_touch(e);
            self.step.enabled_by_touch.push(e.path.clone());
            for _ in 0..ENABLE_WAIT_TICKS {
                if self.script.runtime().is_enabled(trigger.event) {
                    return true;
                }
                if self.reached.is_some() || !self.frame() {
                    return false;
                }
            }
        }
        self.script.runtime().is_enabled(trigger.event)
    }

    /// Teleports the player into `trigger`'s volume for one frame, and falls
    /// back to a direct Kismet touch when the teleport registers none.
    fn teleport_touch(&mut self, trigger: &ExitTrigger) {
        let id = self.script.world_id(trigger.actor);
        let point = id.and_then(|id| trigger_point(&self.game, id));
        let mut touched = false;
        if let Some(p) = point {
            let player = self.game.player_mut();
            player.position = p;
            player.velocity = Vec3::ZERO;
            player.pawn.force_floor_check = true;
            if let Some(t) = self.step_with(&InputFrame::default()) {
                touched = t.report.world.iter().any(
                    |e| matches!(e, asamu_world::WorldEvent::Touch { id: tid } if Some(tid) == id),
                );
            }
        }
        if !touched {
            // A trigger the teleport cannot reach (collision disabled at
            // start, a hull whose bounds' centre lies outside it): the touch
            // goes to Kismet directly.
            self.step.direct_touch = true;
            self.script.runtime_mut().touch(trigger.actor, true);
        }
        self.step.triggers.push(trigger.path.clone());
    }

    /// Touches the credit triggers of streamed levels not touched yet.
    fn touch_credit_triggers(&mut self) {
        if self.step.streamed.is_empty() {
            return;
        }
        let pending: Vec<ExitTrigger> = exit_triggers(self.script.runtime().graph())
            .into_iter()
            .filter(|t| t.exit == "credits" && !self.credit_touches.contains(&t.event))
            .collect();
        for t in pending.iter().take(1) {
            self.credit_touches.insert(t.event);
            self.touch(t);
        }
    }

    /// Runs up to `n` frames, touching credit triggers once streamed.
    fn run_to_exit(&mut self, n: u64) {
        let mut left = n;
        while left > 0 && self.reached.is_none() {
            let chunk = left.min(60);
            self.run(chunk);
            left -= chunk;
            self.touch_credit_triggers();
        }
    }
}

/// How a chain attempt makes the level's progress before touching the exit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Progress {
    /// No story interactions.
    None,
    /// Story interactions through the player's story-mode fire only.
    Fire,
    /// Through the fire, and sent to Kismet directly where it cannot reach.
    FireOrInject,
}

/// Performs every story interaction Kismet listens for
/// (`SeqEvent_ActorInteractedWith` originators, graph order), each followed
/// by `settle` frames: through the player's story-mode fire where it can
/// reach the item ([`Attempt::interact_by_fire`]), else, with `inject`, by
/// sending the interaction to Kismet directly. Originators an earlier
/// interaction already reported (a child interactable notifies its parent)
/// are skipped.
fn perform_interactions(a: &mut Attempt<'_>, settle: u64, inject: bool) {
    let actors: Vec<(ActorRef, String)> = {
        let g = a.script.runtime().graph();
        let mut seen = BTreeSet::new();
        g.nodes
            .iter()
            .filter(|n| n.class == OpClass::ActorInteractedWith)
            .filter_map(|n| n.event.as_ref()?.originator.as_deref())
            .filter_map(|p| g.actor_by_path(p).map(|r| (r, p.to_owned())))
            .filter(|(x, _)| seen.insert(*x))
            .collect()
    };
    for (actor, path) in actors {
        let id = a.script.world_id(actor);
        if id.is_some_and(|id| a.interacted.contains(&id)) {
            continue;
        }
        if id.is_some_and(|id| a.interact_by_fire(id)) {
            a.step.interactions_fired.push(path);
        } else if inject {
            a.script.runtime_mut().actor_interacted_with(actor);
            a.step.interactions_injected.push(path);
        } else {
            continue;
        }
        a.run(settle);
    }
}

/// One step of the story chain on `map`: find the triggers whose touch
/// leads to the map's exit and try, each attempt on a fresh load and with up
/// to `max_ticks` frames after the touches: every trigger alone (nearest
/// first), then all of them in sequence (farthest first: a trigger that
/// starts a narration before the one at the exit), then both again after
/// the level's story interactions performed through the player's fire, then
/// once more with the interactions the fire cannot reach sent to Kismet
/// directly. A trigger whose touch streams a level in is followed by the
/// streamed level's credit trigger.
///
/// # Errors
/// The map could not be loaded.
pub fn chain_step(
    dir: &Path,
    map: &str,
    expected: Option<&str>,
    max_ticks: u64,
) -> Result<ChainStep, LevelScriptError> {
    let mut step = ChainStep {
        map: map.to_owned(),
        expected: expected.map(str::to_owned),
        ..ChainStep::default()
    };
    let (_, script) = load_level_with_kismet(dir, map)?;
    let Some(script) = script else {
        step.notes.push("no Kismet export".to_owned());
        return Ok(step);
    };
    let triggers: Vec<ExitTrigger> = exit_triggers(script.runtime().graph())
        .into_iter()
        .filter(|t| t.exit != "credits")
        .take(6)
        .collect();
    step.candidates = triggers.iter().map(|t| t.path.clone()).collect();
    let mut plans: Vec<(Progress, Vec<&ExitTrigger>)> = Vec::new();
    for progress in [Progress::None, Progress::Fire, Progress::FireOrInject] {
        for t in &triggers {
            plans.push((progress, vec![t]));
        }
        if triggers.len() > 1 {
            plans.push((progress, triggers.iter().rev().collect()));
        }
    }
    for (progress, touches) in plans {
        let (mut game, Some(script)) = load_level_with_kismet(dir, map)? else {
            break;
        };
        game.start();
        let mut s = ChainStep {
            map: step.map.clone(),
            expected: step.expected.clone(),
            candidates: step.candidates.clone(),
            progressed: progress != Progress::None,
            ..ChainStep::default()
        };
        let mut a = Attempt {
            game,
            script,
            step: &mut s,
            ticks: 0,
            reached: None,
            credit_touches: BTreeSet::new(),
            interacted: BTreeSet::new(),
        };
        // Level start settles (cutscene starts, abilities).
        a.run(60);
        if progress != Progress::None {
            perform_interactions(&mut a, 1_200, progress == Progress::FireOrInject);
        }
        a.ticks = 0;
        for (i, t) in touches.iter().enumerate() {
            a.touch(t);
            if i + 1 < touches.len() {
                a.run(1_800);
            }
        }
        a.run_to_exit(max_ticks);
        let reached = a.reached.take();
        let ticks = a.ticks;
        let errors = a.script.runtime().errors().len();
        drop(a);
        let what = format!(
            "{}{:?}",
            match progress {
                Progress::None => "",
                Progress::Fire => "fired interactions + ",
                Progress::FireOrInject => "fired/injected interactions + ",
            },
            touches.iter().map(|t| t.path.as_str()).collect::<Vec<_>>()
        );
        if let Some(r) = reached {
            s.reached = Some(r);
            s.ticks = ticks;
            s.notes = std::mem::take(&mut step.notes);
            if errors > 0 {
                s.notes.push(format!("{errors} interpreter errors"));
            }
            return Ok(s);
        }
        step.notes
            .push(format!("{what}: no transition in {max_ticks} ticks"));
    }
    Ok(step)
}

/// Follows the story chain from AG-Workshop: each map's reached transition
/// is the next map loaded. Stops at the Epilogue, at a map that is not
/// converted, or where no transition is reached.
#[must_use]
pub fn follow_story_chain(dir: &Path, max_ticks: u64) -> Vec<Result<ChainStep, LevelScriptError>> {
    let mut out = Vec::new();
    let mut current = ChapterId::Workshop.map_name().to_owned();
    for _ in 0..ChapterId::ALL.len() {
        let chapter = ChapterId::from_map_name(&current);
        if chapter == Some(ChapterId::Epilogue) {
            break;
        }
        let expected = chapter.and_then(ChapterId::next).map(ChapterId::map_name);
        let step = chain_step(dir, &current, expected, max_ticks);
        let next = step.as_ref().ok().and_then(|s| s.reached.clone());
        out.push(step);
        match next {
            Some(n) => current = n,
            None => break,
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn input_script_is_deterministic_and_finite() {
        let mut a = InputScript::new(DEFAULT_SEED);
        let mut b = InputScript::new(DEFAULT_SEED);
        let mut c = InputScript::new(DEFAULT_SEED + 1);
        let mut differs = false;
        for _ in 0..5_000 {
            let (fa, fb, fc) = (a.next_frame(), b.next_frame(), c.next_frame());
            assert_eq!(fa, fb);
            assert!(fa.is_finite());
            assert!(fa.look_yaw_delta.abs() <= 0.04 && fa.look_pitch_delta.abs() <= 0.02);
            differs |= fa != fc;
        }
        assert!(differs, "another seed gives another script");
    }

    #[test]
    fn open_targets_and_variant_names() {
        assert_eq!(open_target("open AG-DarkCave"), Some("AG-DarkCave"));
        assert_eq!(
            open_target(" OPEN ASAMUFrontEndMap?game=X "),
            Some("ASAMUFrontEndMap")
        );
        assert_eq!(open_target("setspeed 0.3"), None);
        assert_eq!(open_target("open"), None);
        assert_eq!(variant_name("PlaySound { node: 1 }"), "PlaySound");
        assert_eq!(variant_name("SuitOnAnimation"), "SuitOnAnimation");
    }

    /// Touch event 1 plays a Matinee whose `Completed` turns on (input 0)
    /// the toggle of touch event 4; touch event 5 only turns it off (input
    /// 1): 1 enables 4, 5 does not, and 4 does not enable itself.
    #[test]
    fn enabler_triggers_follow_links_to_turn_on_inputs() {
        let nodes = r#"[
            {"id": 0, "class": "Engine.Sequence", "kind": "sequence", "members": [1, 2, 3, 4, 5]},
            {"id": 1, "class": "Engine.SeqEvent_Touch", "kind": "event", "parent": 0,
             "outputs": [{"desc": "Touched", "links": [{"op": 2, "input": 0}]}],
             "event": {"originator": "T.TheWorld.PersistentLevel.Trigger_0"}},
            {"id": 2, "class": "Engine.SeqAct_Interp", "kind": "action", "parent": 0,
             "inputs": [{"desc": "Play"}], "outputs": [{"desc": "Completed", "links": [{"op": 3, "input": 0}]}],
             "latent": true, "latent_base": true},
            {"id": 3, "class": "Engine.SeqAct_Toggle", "kind": "action", "parent": 0,
             "inputs": [{"desc": "Turn On"}, {"desc": "Turn Off"}, {"desc": "Toggle"}],
             "outputs": [{"desc": "Out"}], "event_links": [{"desc": "Event", "events": [4]}]},
            {"id": 4, "class": "Engine.SeqEvent_Touch", "kind": "event", "parent": 0, "enabled": false,
             "outputs": [{"desc": "Touched", "links": [{"op": 3, "input": 0}]}],
             "event": {"originator": "T.TheWorld.PersistentLevel.Trigger_1"}},
            {"id": 5, "class": "Engine.SeqEvent_Touch", "kind": "event", "parent": 0,
             "outputs": [{"desc": "Touched", "links": [{"op": 3, "input": 1}]}],
             "event": {"originator": "T.TheWorld.PersistentLevel.Trigger_2"}}
        ]"#;
        let actors = r#"[
            {"path": "T.TheWorld.PersistentLevel.Trigger_0", "name": "Trigger_0", "class": "Engine.Trigger",
             "kind": "trigger", "package": "T", "slot": 1},
            {"path": "T.TheWorld.PersistentLevel.Trigger_1", "name": "Trigger_1", "class": "Engine.Trigger",
             "kind": "trigger", "package": "T", "slot": 2},
            {"path": "T.TheWorld.PersistentLevel.Trigger_2", "name": "Trigger_2", "class": "Engine.Trigger",
             "kind": "trigger", "package": "T", "slot": 3}
        ]"#;
        let doc = format!(
            r#"{{"format": "{}", "version": {}, "package": "T", "nodes": {nodes}, "actors": {actors}}}"#,
            asamu_kismet::RUNTIME_FORMAT,
            asamu_kismet::RUNTIME_VERSION
        );
        let g = Graph::from_json_slice(doc.as_bytes()).unwrap();
        let found = enabler_triggers(&g, 4);
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found[0].event, 1);
        assert_eq!(found[0].exit, "enable:4");
        assert_eq!(found[0].depth, 2);
        assert!(enabler_triggers(&g, 1).is_empty());
        assert!(exit_triggers(&g).is_empty());
    }

    /// A graph whose trigger reaches `open AG-Next` through a remote event,
    /// and whose second trigger only opens the front end.
    #[test]
    fn exit_triggers_follow_remote_events() {
        let nodes = r#"[
            {"id": 0, "class": "Engine.Sequence", "kind": "sequence", "members": [1, 2, 3, 4, 5, 6]},
            {"id": 1, "class": "Engine.SeqEvent_Touch", "kind": "event", "parent": 0,
             "outputs": [{"desc": "Touched", "links": [{"op": 2, "input": 0}]}],
             "event": {"originator": "T.TheWorld.PersistentLevel.Trigger_0", "max_trigger_count": 1}},
            {"id": 2, "class": "Engine.SeqAct_ActivateRemoteEvent", "kind": "action", "parent": 0,
             "inputs": [{"desc": "In"}], "outputs": [{"desc": "Out"}], "params": {"EventName": "Exit"}},
            {"id": 3, "class": "Engine.SeqEvent_RemoteEvent", "kind": "event", "parent": 0,
             "outputs": [{"desc": "Out", "links": [{"op": 4, "input": 0}]}], "params": {"EventName": "exit"}},
            {"id": 4, "class": "Engine.SeqAct_ConsoleCommand", "kind": "action", "parent": 0,
             "inputs": [{"desc": "In"}], "outputs": [{"desc": "Out"}],
             "params": {"Commands": ["open AG-Next?x=1"]}},
            {"id": 5, "class": "Engine.SeqEvent_Touch", "kind": "event", "parent": 0,
             "outputs": [{"desc": "Touched", "links": [{"op": 6, "input": 0}]}],
             "event": {"originator": "T.TheWorld.PersistentLevel.Trigger_1"}},
            {"id": 6, "class": "Engine.SeqAct_ConsoleCommand", "kind": "action", "parent": 0,
             "inputs": [{"desc": "In"}], "outputs": [{"desc": "Out"}],
             "params": {"Commands": ["open ASAMUFrontEndMap"]}}
        ]"#;
        let actors = r#"[
            {"path": "T.TheWorld.PersistentLevel.Trigger_0", "name": "Trigger_0", "class": "Engine.Trigger",
             "kind": "trigger", "package": "T", "slot": 1},
            {"path": "T.TheWorld.PersistentLevel.Trigger_1", "name": "Trigger_1", "class": "Engine.Trigger",
             "kind": "trigger", "package": "T", "slot": 2}
        ]"#;
        let doc = format!(
            r#"{{"format": "{}", "version": {}, "package": "T", "nodes": {nodes}, "actors": {actors}}}"#,
            asamu_kismet::RUNTIME_FORMAT,
            asamu_kismet::RUNTIME_VERSION
        );
        let g = Graph::from_json_slice(doc.as_bytes()).unwrap();
        let found = exit_triggers(&g);
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found[0].event, 1);
        assert_eq!(found[0].exit, "open:AG-Next");
        assert_eq!(found[0].depth, 3);
        assert_eq!(found[0].path, "T.TheWorld.PersistentLevel.Trigger_0");
    }
}
