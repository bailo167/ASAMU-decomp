//! The interpreter: UE3's sequence execution model (KISMET_RUNTIME.md §2)
//! over a [`Graph`], driven once per frame by [`Runtime::tick`].
//!
//! Engine rules reproduced here (CONFIRMED from the decompiled
//! `USequence::UpdateOp`, `ExecuteActiveOps`, `QueueSequenceOp`,
//! `QueueDelayedSequenceOp`, `USequenceOp::ActivateOutputLink`,
//! `ForceActivateOutput`, `DeActivated`, `USequenceEvent::CheckActivate`,
//! `ActivateEvent`, the variable populate/publish functions and
//! `USequence::BeginPlay` / `NotifyMatchStarted`; described in our own
//! words, nothing copied):
//!
//! - Every sequence keeps a stack of active ops. An update first counts
//!   down delayed activations, re-queues latent ops deferred from the last
//!   frame, then pops ops from the top until the stack is empty (at most
//!   999 steps). When the stack empties, events whose activation was queued
//!   while they were still pending are activated.
//! - Processing an op: values flow from linked variables into the op's
//!   properties; an inactive op is activated (class behaviour), then
//!   updated (latent ops may stay active and are re-queued at the end of
//!   the update); a finished op is deactivated (class behaviour) and its
//!   properties are written back to the linked variables. Outputs that
//!   carry an impulse activate their links (with a delay through the
//!   delayed list). Each further impulse queued on a non-latent op's input
//!   runs it again.
//! - A latent op already updated in this frame is deferred to the next.
//! - A sequence updates its own stack, then its nested sequences in member
//!   order.
//! - Events check their trigger count, re-trigger delay and player-only
//!   flag before activating; an event still pending queues further
//!   activations.

use std::collections::{BTreeMap, VecDeque};
use std::sync::Arc;

use crate::graph::{ActorRef, Graph, NodeKind};
use crate::host::{Host, Output};
use crate::matinee::MatineeSet;
use crate::narrator::Narrator;
use crate::ops::OpClass;
use crate::value::KValue;

/// Most ops one sequence processes in one update before aborting
/// (`ExecuteActiveOps` gives up after 999 steps; CONFIRMED constant 0x3E6).
pub const MAX_STEPS: u32 = 999;
/// Most errors kept.
pub const MAX_ERRORS: usize = 256;
/// Most outputs buffered between [`Runtime::take_outputs`] calls.
pub const MAX_OUTPUTS: usize = 4096;
/// Most event activations queued per sequence while their events are still
/// pending (ours; the engine's array is unbounded, the shipped maps never
/// queue more than a handful).
pub const MAX_QUEUED_ACTIVATIONS: usize = 4096;
/// Most attractor pads waiting for their `Finished` output (ours).
pub const MAX_PENDING_ATTRACTORS: usize = 1024;
/// Deepest attachment chain carried by a moving base (ours; the shipped
/// maps attach at most two levels deep).
pub const MAX_ATTACH_DEPTH: usize = 8;

/// Magic object paths for the player and the world in object values.
pub(crate) const PLAYER_PATH: &str = "$player";
pub(crate) const WORLD_PATH: &str = "$world";

/// A resolved object reference.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Obj {
    /// Null.
    #[default]
    None,
    /// The level's `WorldInfo`.
    World,
    /// The player (pawn or controller).
    Player,
    /// An actor of the graph's actor table.
    Actor(ActorRef),
    /// A Kismet node.
    Node(usize),
    /// Any other object (sound, material, class...).
    Path(String),
}

impl Obj {
    /// True for a non-null object.
    #[must_use]
    pub fn is_some(&self) -> bool {
        !matches!(self, Obj::None)
    }
}

/// An output that carries an impulse: its delay and links.
type FiringOutput = (f32, Vec<(usize, usize)>);

/// A delayed activation (`FActivateOp`).
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Delayed {
    pub(crate) op: usize,
    pub(crate) input: usize,
    pub(crate) remaining: f32,
}

/// An event activation queued while the event was pending.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct QueuedActivation {
    pub(crate) event: usize,
    pub(crate) originator: Obj,
    pub(crate) instigator: Obj,
    pub(crate) indices: Option<Vec<usize>>,
    pub(crate) push_top: bool,
}

/// Per-sequence execution state.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct SeqState {
    pub(crate) stack: Vec<usize>,
    pub(crate) delayed: Vec<Delayed>,
    pub(crate) deferred: Vec<usize>,
    pub(crate) queued: VecDeque<QueuedActivation>,
    pub(crate) nested: Vec<usize>,
}

/// Latent state of an op.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) enum Latent {
    /// None.
    #[default]
    None,
    /// `SeqAct_Delay`.
    Delay {
        remaining: f32,
        active: bool,
        start_frame: u64,
    },
    /// `SeqAct_PlaySound`.
    Sound {
        duration: f32,
        delay_reached: bool,
        stopped: bool,
    },
    /// `SeqAct_CameraFade`.
    Fade { remaining: f32 },
    /// `SeqAct_Interp`.
    Interp(Box<crate::interp::InterpRun>),
}

/// Per-op execution state.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct OpState {
    pub(crate) active: bool,
    pub(crate) activate_count: u32,
    pub(crate) last_update_frame: Option<u64>,
    pub(crate) in_impulse: Vec<bool>,
    pub(crate) in_queued: Vec<u32>,
    pub(crate) out_impulse: Vec<bool>,
    pub(crate) props: BTreeMap<String, KValue>,
    pub(crate) enabled: bool,
    pub(crate) trigger_count: i32,
    pub(crate) activation_time: f64,
    pub(crate) originator: Obj,
    pub(crate) instigator: Obj,
    pub(crate) touched: bool,
    pub(crate) script_count: i32,
    pub(crate) aborted: bool,
    pub(crate) latent: Latent,
}

/// Counters for coverage reports.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RuntimeStats {
    /// Op activations by class name.
    pub activations: BTreeMap<String, u64>,
    /// Activations of classes without interpreter behaviour, by class.
    pub unhandled: BTreeMap<String, u64>,
    /// Updates that hit [`MAX_STEPS`].
    pub step_limit_hits: u64,
    /// Kismet updates run.
    pub updates: u64,
}

/// A pending attractor pad (`SeqAct_ToggleAttractor` waits for `Finished`).
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct PendingAttractor {
    pub(crate) node: usize,
    pub(crate) actor: ActorRef,
}

/// A beat timer of a `SeqEvent_TrackBeat` (`ASAMUBeatEventActor`).
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Beat {
    pub(crate) event: usize,
    pub(crate) interval: f32,
    pub(crate) remaining: Option<f32>,
}

/// The Kismet interpreter for one level (and its streamed sub-levels).
#[derive(Debug, Clone)]
pub struct Runtime {
    pub(crate) graph: Arc<Graph>,
    pub(crate) matinee: Arc<MatineeSet>,
    pub(crate) frame: u64,
    pub(crate) time: f64,
    pub(crate) ops: Vec<OpState>,
    pub(crate) vars: Vec<KValue>,
    pub(crate) seqs: Vec<SeqState>,
    pub(crate) level_attached: Vec<bool>,
    pub(crate) outputs: Vec<Output>,
    pub(crate) narrator: Narrator,
    pub(crate) beats: Vec<Beat>,
    pub(crate) tracks: Vec<String>,
    pub(crate) save_strings: BTreeMap<String, i32>,
    pub(crate) rand_seed: u32,
    pub(crate) rand_state: u64,
    pub(crate) tutorial_id: i32,
    pub(crate) attractors: Vec<PendingAttractor>,
    pub(crate) actor_xf: BTreeMap<ActorRef, ([f32; 3], [i32; 3])>,
    pub(crate) touch_events: BTreeMap<ActorRef, Vec<usize>>,
    /// Actors attached to each actor (from the actor table's `base`).
    pub(crate) children: BTreeMap<ActorRef, Vec<ActorRef>>,
    /// Attached actors' transforms relative to their base (UE3
    /// `RelativeLocation`/`RelativeRotation` of a hard attachment).
    pub(crate) attach_rel: BTreeMap<ActorRef, crate::matinee::Matrix>,
    pub(crate) errors: Vec<String>,
    pub(crate) stats: RuntimeStats,
    pub(crate) started: bool,
    pub(crate) time_trial: bool,
    pub(crate) start_save_index: Option<i32>,
}

impl Runtime {
    /// A runtime at level load (nothing activated yet; the first
    /// [`Runtime::tick`] runs the level-start events).
    #[must_use]
    pub fn new(graph: Arc<Graph>, matinee: Arc<MatineeSet>) -> Runtime {
        let n = graph.nodes.len();
        let mut ops = Vec::with_capacity(n);
        let mut vars = Vec::with_capacity(n);
        for node in &graph.nodes {
            ops.push(OpState {
                in_impulse: vec![false; node.inputs.len()],
                in_queued: vec![0; node.inputs.len()],
                out_impulse: vec![false; node.outputs.len()],
                props: node.params.clone(),
                enabled: node.enabled,
                ..OpState::default()
            });
            vars.push(initial_var(&graph, node.id));
        }
        let mut seqs = vec![SeqState::default(); n];
        for node in &graph.nodes {
            if node.kind != NodeKind::Sequence {
                continue;
            }
            if let Some(s) = seqs.get_mut(node.id) {
                s.nested = node
                    .members
                    .iter()
                    .copied()
                    .filter(|m| {
                        graph.node(*m).is_some_and(|x| {
                            x.kind == NodeKind::Sequence && x.parent == Some(node.id)
                        })
                    })
                    .collect();
            }
        }
        let mut level_attached = vec![false; graph.levels.len()];
        if let Some(first) = level_attached.first_mut() {
            *first = true;
        }
        let mut touch_events: BTreeMap<ActorRef, Vec<usize>> = BTreeMap::new();
        let roots: Vec<usize> = graph.levels.iter().flat_map(|l| l.roots.clone()).collect();
        for e in graph.find_by_class(&roots, OpClass::Touch) {
            if let Some(a) = graph
                .node(e)
                .and_then(|x| x.event.as_ref())
                .and_then(|ev| ev.originator.as_deref())
                .and_then(|p| graph.actor_by_path(p))
            {
                touch_events.entry(a).or_default().push(e);
            }
        }
        let mut children: BTreeMap<ActorRef, Vec<ActorRef>> = BTreeMap::new();
        for (i, a) in graph.actors.iter().enumerate() {
            if let (Some(b), Ok(i)) = (a.base, u32::try_from(i))
                && b != ActorRef(i)
            {
                children.entry(b).or_default().push(ActorRef(i));
            }
        }
        Runtime {
            graph,
            matinee,
            frame: 0,
            time: 0.0,
            ops,
            vars,
            seqs,
            level_attached,
            outputs: Vec::new(),
            narrator: Narrator::default(),
            beats: Vec::new(),
            tracks: Vec::new(),
            save_strings: BTreeMap::new(),
            rand_seed: 0,
            rand_state: 0x5EED_4B15_4D45_7001,
            tutorial_id: 0,
            attractors: Vec::new(),
            actor_xf: BTreeMap::new(),
            touch_events,
            children,
            attach_rel: BTreeMap::new(),
            errors: Vec::new(),
            stats: RuntimeStats::default(),
            started: false,
            time_trial: false,
            start_save_index: Some(-1),
        }
    }

    /// The graph.
    #[must_use]
    pub fn graph(&self) -> &Graph {
        &self.graph
    }

    /// Kismet updates run so far.
    #[must_use]
    pub fn frame(&self) -> u64 {
        self.frame
    }

    /// Accumulated simulated time (`WorldInfo.TimeSeconds` analogue).
    #[must_use]
    pub fn time(&self) -> f64 {
        self.time
    }

    /// Errors and warnings recorded while running (capped).
    #[must_use]
    pub fn errors(&self) -> &[String] {
        &self.errors
    }

    /// Activation counters.
    #[must_use]
    pub fn stats(&self) -> &RuntimeStats {
        &self.stats
    }

    /// Takes the outputs emitted since the last call.
    pub fn take_outputs(&mut self) -> Vec<Output> {
        std::mem::take(&mut self.outputs)
    }

    /// The general save manager's strings (`EditOrAddSaveString`).
    #[must_use]
    pub fn save_strings(&self) -> &BTreeMap<String, i32> {
        &self.save_strings
    }

    /// Replaces the save strings (carry them across level loads; the
    /// original keeps them in the save game).
    pub fn set_save_strings(&mut self, strings: BTreeMap<String, i32>) {
        self.save_strings = strings;
    }

    /// What the game info's level-start load reports to the
    /// saved-game-state events (`SaveGameState.TriggerLoadedGameEvent`):
    /// `Some(-1)` (the default) is a level without a save of its own,
    /// `Some(i)` a save restored at checkpoint `i`, `None` skips the event
    /// (play-in-editor, or a map without the ASAMU game info).
    pub fn set_start_save_index(&mut self, index: Option<i32>) {
        self.start_save_index = index;
    }

    /// Selects the time-trial game type (read by `SeqCond_IsTimeTrial` when
    /// the host does not override it).
    pub fn set_time_trial(&mut self, on: bool) {
        self.time_trial = on;
    }

    /// The current value of variable node `id`.
    #[must_use]
    pub fn var_value(&self, id: usize) -> Option<&KValue> {
        self.vars.get(id)
    }

    /// True while op `id` is active.
    #[must_use]
    pub fn is_active(&self, id: usize) -> bool {
        self.ops.get(id).is_some_and(|o| o.active)
    }

    /// How often op `id` was activated.
    #[must_use]
    pub fn activate_count(&self, id: usize) -> u32 {
        self.ops.get(id).map_or(0, |o| o.activate_count)
    }

    /// `bEnabled` of event or sequence `id`.
    #[must_use]
    pub fn is_enabled(&self, id: usize) -> bool {
        self.ops.get(id).is_some_and(|o| o.enabled)
    }

    /// Current Matinee position of `SeqAct_Interp` node `id`.
    #[must_use]
    pub fn interp_position(&self, id: usize) -> Option<f32> {
        match &self.ops.get(id)?.latent {
            Latent::Interp(r) => Some(r.playback.position),
            _ => None,
        }
    }

    /// The transform Matinee last gave actor `a` (or its placement).
    #[must_use]
    pub fn actor_transform(&self, a: ActorRef) -> Option<([f32; 3], [i32; 3])> {
        self.actor_xf
            .get(&a)
            .copied()
            .or_else(|| self.graph.actor(a).map(|x| (x.location, x.rotation)))
    }

    /// Every actor Kismet has moved so far (Matinee move tracks,
    /// `SetRotationToPlayerRotation`), with its latest location and
    /// rotation, in actor-table order (for rendering).
    pub fn moved_actors(&self) -> impl Iterator<Item = (ActorRef, [f32; 3], [i32; 3])> + '_ {
        self.actor_xf.iter().map(|(a, (l, r))| (*a, *l, *r))
    }

    /// Moves graph actor `a` (Matinee move tracks,
    /// `SetRotationToPlayerRotation`; `rotation` `None` keeps its rotation)
    /// and carries the actors attached to it, as UE3's hard attachment does:
    /// each attached actor (actor-table `base`, recursively) keeps the
    /// transform relative to its base that it had when it last moved by
    /// itself (or when its base first moved), and is re-placed from that
    /// relative transform and the base's new transform (no accumulated
    /// error). Hard attachment covers 118 of the 120 passengers of
    /// StarHaven's airships and all of ParadiseCave's and BeautifulCity's;
    /// soft attachment is treated the same way (TENTATIVE).
    pub(crate) fn place_actor(
        &mut self,
        a: ActorRef,
        location: [f32; 3],
        rotation: Option<[i32; 3]>,
        host: &mut dyn Host,
    ) {
        self.place_actor_depth(a, location, rotation, host, 0);
    }

    fn place_actor_depth(
        &mut self,
        a: ActorRef,
        location: [f32; 3],
        rotation: Option<[i32; 3]>,
        host: &mut dyn Host,
        depth: usize,
    ) {
        use crate::matinee::{
            matrix_mul, matrix_rotator, rigid_inverse, rotation_translation_matrix as tm,
        };
        let Some(info) = self.graph.actor(a).cloned() else {
            return;
        };
        let old = self.current_transform(a, &*host);
        let new_rot = rotation.unwrap_or_else(|| old.map_or(info.rotation, |o| o.1));
        let children = if depth < MAX_ATTACH_DEPTH {
            self.children.get(&a).cloned().unwrap_or_default()
        } else {
            Vec::new()
        };
        // Children that have not moved yet take their relation to this
        // actor's transform before the move.
        if let Some((ol, or)) = old
            && !children.is_empty()
        {
            let inv = rigid_inverse(&tm(or, ol));
            for c in &children {
                if !self.attach_rel.contains_key(c)
                    && let Some((cl, cr)) = self.current_transform(*c, &*host)
                {
                    self.attach_rel.insert(*c, matrix_mul(&tm(cr, cl), &inv));
                }
            }
        }
        self.actor_xf.insert(a, (location, new_rot));
        host.set_actor_transform(&info, location, rotation);
        // Moved by itself: its relation to its own base changes.
        if depth == 0
            && let Some(b) = info.base.filter(|b| *b != a)
            && let Some((bl, br)) = self.current_transform(b, &*host)
        {
            self.attach_rel.insert(
                a,
                matrix_mul(&tm(new_rot, location), &rigid_inverse(&tm(br, bl))),
            );
        }
        if children.is_empty() || old == Some((location, new_rot)) {
            return;
        }
        let base = tm(new_rot, location);
        for c in children {
            let Some(rel) = self.attach_rel.get(&c).copied() else {
                continue;
            };
            let m = matrix_mul(&rel, &base);
            let [x, y, z, _] = m[3];
            self.place_actor_depth(
                c,
                [x, y, z],
                Some(matrix_rotator(&m, false)),
                host,
                depth + 1,
            );
        }
    }

    pub(crate) fn error(&mut self, msg: String) {
        if self.errors.len() < MAX_ERRORS {
            self.errors.push(msg);
        }
    }

    pub(crate) fn emit(&mut self, o: Output) {
        if self.outputs.len() < MAX_OUTPUTS {
            self.outputs.push(o);
        } else {
            self.error("output buffer full; output dropped".to_owned());
        }
    }

    // ---------------------------------------------------------------- levels

    /// Root sequences of the game sequence (the persistent level's roots).
    pub(crate) fn game_roots(&self) -> Vec<usize> {
        self.graph
            .levels
            .first()
            .map(|l| l.roots.clone())
            .unwrap_or_default()
    }

    /// Roots of every attached level (for searches over the game sequence).
    pub(crate) fn attached_roots(&self) -> Vec<usize> {
        self.graph
            .levels
            .iter()
            .zip(&self.level_attached)
            .filter(|(_, a)| **a)
            .flat_map(|(l, _)| l.roots.clone())
            .collect()
    }

    /// Attaches a streamed level's sequences under the persistent root and
    /// runs their begin-play events (`UWorld::AddToWorld`: the streamed
    /// level's root sequence is appended to the game sequence's members and
    /// nested sequences, then begins play). Returns false for an unknown or
    /// already attached level.
    pub fn attach_level(&mut self, package: &str) -> bool {
        let Some(i) = self.graph.level_index(package) else {
            return false;
        };
        if self.level_attached.get(i).copied().unwrap_or(true) {
            return false;
        }
        if let Some(a) = self.level_attached.get_mut(i) {
            *a = true;
        }
        let roots = self
            .graph
            .levels
            .get(i)
            .map(|l| l.roots.clone())
            .unwrap_or_default();
        if let Some(top) = self.game_roots().first().copied()
            && let Some(s) = self.seqs.get_mut(top)
        {
            for r in &roots {
                if !s.nested.contains(r) {
                    s.nested.push(*r);
                }
            }
        }
        if self.started {
            for r in roots {
                self.begin_play(r, 0);
            }
        }
        true
    }

    /// True when level `package`'s scripts run.
    #[must_use]
    pub fn is_level_attached(&self, package: &str) -> bool {
        self.graph
            .level_index(package)
            .and_then(|i| self.level_attached.get(i).copied())
            .unwrap_or(false)
    }

    // ---------------------------------------------------------------- frame

    /// One Kismet update of `dt` seconds: the level-start events on the first
    /// call, then every game sequence (root first, nested sequences after),
    /// then the narrator's and beat actors' timers (which tick with the
    /// actors, after Kismet; what they activate runs in the next update).
    pub fn tick(&mut self, dt: f32, host: &mut dyn Host) {
        let dt = if dt.is_finite() && dt >= 0.0 { dt } else { 0.0 };
        self.frame += 1;
        self.time += f64::from(dt);
        if !self.started {
            self.start(host);
        }
        self.stats.updates += 1;
        for r in self.game_roots() {
            self.update_sequence(r, dt, host, 0);
        }
        self.tick_narrator(dt);
        self.tick_beats(dt);
    }

    /// Level start, in the order the engine and the ASAMU game info run it:
    /// `USequence::BeginPlay` on the game sequence (level-loaded events with
    /// a linked first output fire it); then the match starts: the pawn
    /// spawns, which starts the adaptive music manager (`AddAdaptiveTracks`
    /// run directly, beat events initialised) and the game info's load
    /// (saved-game-state events, see [`Runtime::set_start_save_index`]);
    /// finally `NotifyMatchStarted` (level-loaded events fire their first
    /// output again, subject to their trigger count, and their second output
    /// when linked).
    fn start(&mut self, host: &mut dyn Host) {
        self.started = true;
        for r in self.attached_roots() {
            self.begin_play(r, 0);
        }
        self.init_adaptive_music(host);
        if let Some(i) = self.start_save_index {
            self.saved_game_state_loaded(i);
        }
        for r in self.attached_roots() {
            self.notify_match_started(r, 0);
        }
    }

    fn begin_play(&mut self, seq: usize, depth: usize) {
        if depth > 64 {
            return;
        }
        let nested = self
            .seqs
            .get(seq)
            .map(|s| s.nested.clone())
            .unwrap_or_default();
        for n in nested {
            self.begin_play(n, depth + 1);
        }
        self.activate_level_loaded(seq, 0);
    }

    fn notify_match_started(&mut self, seq: usize, depth: usize) {
        if depth > 64 {
            return;
        }
        let nested = self
            .seqs
            .get(seq)
            .map(|s| s.nested.clone())
            .unwrap_or_default();
        for n in nested {
            self.notify_match_started(n, depth + 1);
        }
        let members = self
            .graph
            .node(seq)
            .map(|s| s.members.clone())
            .unwrap_or_default();
        for m in members {
            self.level_loaded_member(m, 0);
            self.level_loaded_member(m, 1);
        }
    }

    fn activate_level_loaded(&mut self, seq: usize, index: usize) {
        let members = self
            .graph
            .node(seq)
            .map(|s| s.members.clone())
            .unwrap_or_default();
        for m in members {
            self.level_loaded_member(m, index);
        }
    }

    fn level_loaded_member(&mut self, m: usize, index: usize) {
        let linked = self.graph.node(m).is_some_and(|x| {
            x.class == OpClass::LevelLoaded
                && x.outputs.get(index).is_some_and(|o| !o.links.is_empty())
        });
        if linked {
            self.check_activate(m, Obj::World, Obj::None, false, Some(vec![index]), false);
        }
    }

    fn sequence_enabled(&self, seq: usize) -> bool {
        let mut cur = Some(seq);
        let mut steps = 0;
        while let Some(s) = cur {
            if !self.ops.get(s).is_some_and(|o| o.enabled) {
                return false;
            }
            steps += 1;
            if steps > 64 {
                break;
            }
            cur = self.graph.node(s).and_then(|x| x.parent);
        }
        true
    }

    fn update_sequence(&mut self, seq: usize, dt: f32, host: &mut dyn Host, depth: usize) {
        if depth > 64 || !self.ops.get(seq).is_some_and(|o| o.enabled) {
            return;
        }
        self.execute_active_ops(seq, dt, host);
        let nested = self
            .seqs
            .get(seq)
            .map(|s| s.nested.clone())
            .unwrap_or_default();
        for n in nested {
            self.update_sequence(n, dt, host, depth + 1);
        }
    }

    // ---------------------------------------------------------------- queues

    /// `USequence::QueueSequenceOp` on the op's parent sequence: no
    /// duplicates; `push_top` puts it on top (processed next), otherwise at
    /// the bottom.
    pub(crate) fn queue_op(&mut self, op: usize, push_top: bool) {
        let Some(seq) = self.graph.node(op).and_then(|x| x.parent) else {
            return;
        };
        let Some(s) = self.seqs.get_mut(seq) else {
            return;
        };
        if s.stack.contains(&op) {
            return;
        }
        if push_top {
            s.stack.push(op);
        } else {
            s.stack.insert(0, op);
        }
    }

    /// `USequence::QueueDelayedSequenceOp`: restarts an existing delayed
    /// activation of the same input, else adds one.
    fn queue_delayed(&mut self, seq: usize, op: usize, input: usize, delay: f32) {
        let Some(s) = self.seqs.get_mut(seq) else {
            return;
        };
        if let Some(d) = s
            .delayed
            .iter_mut()
            .find(|d| d.op == op && d.input == input)
        {
            d.remaining = delay;
            return;
        }
        s.delayed.push(Delayed {
            op,
            input,
            remaining: delay,
        });
    }

    fn link_delay(&self, out_delay: f32, target: usize, input: usize) -> f32 {
        out_delay
            + self
                .graph
                .node(target)
                .and_then(|x| x.inputs.get(input))
                .map_or(0.0, |i| i.delay)
    }

    /// `ActivateOutputLink`: sets the output's impulse (processed when the op
    /// finishes its processing step) unless the output is disabled.
    pub(crate) fn activate_output(&mut self, op: usize, index: usize) -> bool {
        let disabled = self
            .graph
            .node(op)
            .and_then(|x| x.outputs.get(index))
            .is_none_or(|o| o.disabled);
        if disabled {
            return false;
        }
        if let Some(i) = self
            .ops
            .get_mut(op)
            .and_then(|o| o.out_impulse.get_mut(index))
        {
            *i = true;
        }
        true
    }

    /// `ForceActivateOutput`: activates the linked inputs at once (bottom of
    /// their sequence's stack) or through the delayed list, without setting
    /// the output's own impulse and without checking disabled inputs.
    pub(crate) fn force_output(&mut self, op: usize, index: usize) {
        let Some(node) = self.graph.node(op) else {
            return;
        };
        let Some(out) = node.outputs.get(index) else {
            return;
        };
        let parent = node.parent;
        let links = out.links.clone();
        let delay = out.delay;
        for (target, input) in links {
            let d = self.link_delay(delay, target, input);
            if d <= 0.0 {
                if let Some(i) = self
                    .ops
                    .get_mut(target)
                    .and_then(|o| o.in_impulse.get_mut(input))
                {
                    *i = true;
                    self.queue_op(target, false);
                }
            } else if let Some(p) = parent {
                self.queue_delayed(p, target, input, d);
            }
        }
    }

    /// `ForceActivateInput`: pulses input `index` of `op` and queues the op
    /// at the bottom of its sequence (tools, tests, debugging).
    pub fn force_input(&mut self, op: usize, index: usize) {
        if let Some(i) = self
            .ops
            .get_mut(op)
            .and_then(|o| o.in_impulse.get_mut(index))
        {
            *i = true;
            self.queue_op(op, false);
        }
    }

    fn receive_impulse(&mut self, target: usize, input: usize, push_top: bool) {
        let disabled = self
            .graph
            .node(target)
            .and_then(|x| x.inputs.get(input))
            .is_none_or(|i| i.disabled);
        if disabled {
            return;
        }
        let Some(st) = self.ops.get_mut(target) else {
            return;
        };
        let (Some(imp), Some(q)) = (st.in_impulse.get_mut(input), st.in_queued.get_mut(input))
        else {
            return;
        };
        if *imp {
            *q = q.saturating_add(1);
        }
        *imp = true;
        self.queue_op(target, push_top);
    }

    // ---------------------------------------------------------------- execute

    fn execute_active_ops(&mut self, seq: usize, dt: f32, host: &mut dyn Host) {
        // Delayed activations.
        let mut i = 0;
        while let Some(d) = self.seqs.get_mut(seq).and_then(|s| s.delayed.get_mut(i)) {
            d.remaining -= dt;
            if d.remaining <= 0.0 {
                let (op, input) = (d.op, d.input);
                if let Some(s) = self.seqs.get_mut(seq) {
                    s.delayed.remove(i);
                }
                self.receive_impulse(op, input, false);
            } else {
                i += 1;
            }
        }
        // Latent ops deferred from the previous frame go to the bottom.
        while let Some(op) = self.seqs.get_mut(seq).and_then(|s| s.deferred.pop()) {
            self.queue_op(op, false);
        }
        let mut still_active: Vec<usize> = Vec::new();
        let mut steps = 0u32;
        // The engine only enters its loop with a non-empty stack; queued
        // event activations are unqueued one at a time, each time the stack
        // runs empty, and the loop ends when that activation queued nothing
        // (an event that is still active re-queues itself instead, so this
        // never spins).
        let stack_empty = |rt: &Runtime| rt.seqs.get(seq).is_none_or(|s| s.stack.is_empty());
        while !stack_empty(self) {
            if steps >= MAX_STEPS {
                self.stats.step_limit_hits += 1;
                self.error(format!(
                    "sequence {seq}: max Kismet execution steps exceeded, aborting"
                ));
                break;
            }
            let Some(op) = self.seqs.get_mut(seq).and_then(|s| s.stack.pop()) else {
                break;
            };
            steps += 1;
            self.process_op(seq, op, dt, host, &mut still_active);
            if stack_empty(self) {
                let next = self.seqs.get_mut(seq).and_then(|s| s.queued.pop_front());
                if let Some(q) = next {
                    self.activate_event(
                        q.event,
                        q.originator,
                        q.instigator,
                        q.indices.as_deref(),
                        q.push_top,
                        true,
                    );
                }
            }
        }
        for op in still_active.into_iter().rev() {
            if self.ops.get(op).is_some_and(|o| o.active) {
                self.queue_op(op, true);
            }
        }
    }

    fn process_op(
        &mut self,
        seq: usize,
        op: usize,
        dt: f32,
        host: &mut dyn Host,
        still_active: &mut Vec<usize>,
    ) {
        let Some((latent, latent_base, class_short)) = self
            .graph
            .node(op)
            .map(|n| (n.latent, n.latent_base, n.class_short().to_owned()))
        else {
            return;
        };
        let frame = self.frame;
        // Only `SeqAct_Latent` subclasses record their update time and are
        // deferred when queued again in the same frame (the engine tests the
        // class, not `bLatentExecution`; a latent op such as
        // `SeqAct_CameraFade` is simply processed again).
        if latent_base
            && self.ops.get(op).is_some_and(|o| o.active)
            && self
                .ops
                .get(op)
                .and_then(|o| o.last_update_frame)
                .is_some_and(|f| f == frame)
        {
            if let Some(s) = self.seqs.get_mut(seq) {
                s.deferred.push(op);
            }
            return;
        }
        self.populate(op);
        if !self.ops.get(op).is_some_and(|o| o.active) {
            if let Some(o) = self.ops.get_mut(op) {
                o.active = true;
                o.activate_count = o.activate_count.saturating_add(1);
            }
            *self.stats.activations.entry(class_short).or_insert(0) += 1;
            self.activated(op, host);
        }
        if latent_base && let Some(o) = self.ops.get_mut(op) {
            o.last_update_frame = Some(frame);
        }
        let mut pending: Vec<(usize, usize)> = Vec::new();
        if self.ops.get(op).is_some_and(|o| o.active) {
            let finished = self.update_op(op, dt, host);
            if let Some(o) = self.ops.get_mut(op) {
                o.active = !finished;
            }
            if !finished && !latent {
                self.error(format!("op {op} still active while not latent"));
            } else {
                if !finished {
                    if !still_active.contains(&op) {
                        still_active.push(op);
                    }
                } else {
                    self.deactivated(op, host);
                    self.publish(op);
                }
                // Outputs with impulses activate their links.
                let outs: Vec<FiringOutput> = match self.graph.node(op) {
                    Some(n) => n
                        .outputs
                        .iter()
                        .enumerate()
                        .filter(|(i, _)| {
                            self.ops
                                .get(op)
                                .and_then(|o| o.out_impulse.get(*i))
                                .copied()
                                .unwrap_or(false)
                        })
                        .map(|(_, o)| (o.delay, o.links.clone()))
                        .collect(),
                    None => Vec::new(),
                };
                for (delay, links) in outs {
                    for (target, input) in links {
                        let d = self.link_delay(delay, target, input);
                        if d <= 0.0 {
                            pending.push((target, input));
                        } else {
                            self.queue_delayed(seq, target, input, d);
                        }
                    }
                }
            }
        }
        // Inputs: further queued impulses re-run a non-latent op.
        let n_inputs = self.ops.get(op).map_or(0, |o| o.in_impulse.len());
        for i in 0..n_inputs {
            let requeue = {
                let Some(o) = self.ops.get_mut(op) else {
                    break;
                };
                let q = o.in_queued.get(i).copied().unwrap_or(0);
                if q < 1 || latent {
                    if let Some(x) = o.in_impulse.get_mut(i) {
                        *x = false;
                    }
                    if let Some(x) = o.in_queued.get_mut(i) {
                        *x = 0;
                    }
                    false
                } else {
                    if let Some(x) = o.in_queued.get_mut(i) {
                        *x -= 1;
                    }
                    true
                }
            };
            if requeue {
                self.queue_op(op, false);
            }
        }
        if let Some(o) = self.ops.get_mut(op) {
            o.out_impulse.fill(false);
        }
        for (target, input) in pending.into_iter().rev() {
            self.receive_impulse(target, input, true);
        }
    }

    // ---------------------------------------------------------------- variables

    pub(crate) fn resolve(&self, v: &KValue) -> Obj {
        match v.as_obj() {
            None => Obj::None,
            Some(PLAYER_PATH) => Obj::Player,
            Some(WORLD_PATH) => Obj::World,
            Some(p) => {
                if let Some(a) = self.graph.actor_by_path(p) {
                    if self
                        .graph
                        .actor(a)
                        .is_some_and(|x| x.class_name().eq_ignore_ascii_case("WorldInfo"))
                    {
                        return Obj::World;
                    }
                    return Obj::Actor(a);
                }
                if let Some(n) = self.graph.node_by_path(p) {
                    return Obj::Node(n);
                }
                Obj::Path(p.to_owned())
            }
        }
    }

    pub(crate) fn obj_value(&self, o: &Obj) -> KValue {
        match o {
            Obj::None => KValue::Obj(None),
            Obj::World => KValue::Obj(Some(WORLD_PATH.to_owned())),
            Obj::Player => KValue::Obj(Some(PLAYER_PATH.to_owned())),
            Obj::Actor(a) => KValue::Obj(self.graph.actor(*a).map(|x| x.path.clone())),
            Obj::Node(n) => KValue::Obj(self.graph.node(*n).map(|x| x.path.clone())),
            Obj::Path(p) => KValue::Obj(Some(p.clone())),
        }
    }

    /// Reads variable `id` (random variables draw a new value each read).
    pub(crate) fn read_var(&mut self, id: usize) -> KValue {
        let class = self.graph.node(id).map_or(OpClass::Unknown, |x| x.class);
        match class {
            OpClass::VarRandomInt => {
                let (lo, hi) = self.var_range(id);
                KValue::Int(self.random_int(lo.as_int(), hi.as_int()))
            }
            OpClass::VarRandomFloat => {
                let (lo, hi) = self.var_range(id);
                KValue::Float(self.random_float(lo.as_float(), hi.as_float()))
            }
            _ => self.vars.get(id).cloned().unwrap_or_default(),
        }
    }

    fn var_range(&self, id: usize) -> (KValue, KValue) {
        let n = self.graph.node(id);
        let get = |k: &str| n.and_then(|x| x.param(k)).cloned().unwrap_or_default();
        (get("Min"), get("Max"))
    }

    /// Writes variable `id`, converting to its class.
    pub(crate) fn write_var(&mut self, id: usize, v: KValue) {
        let class = self.graph.node(id).map_or(OpClass::Unknown, |x| x.class);
        let converted = match class {
            OpClass::VarBool => KValue::Bool(v.as_bool()),
            OpClass::VarInt => KValue::Int(v.as_int()),
            OpClass::VarFloat => KValue::Float(v.as_float()),
            OpClass::VarString => match v {
                KValue::Str(s) => KValue::Str(s),
                other => KValue::Str(match other {
                    KValue::Int(i) => i.to_string(),
                    KValue::Float(f) => f.to_string(),
                    KValue::Bool(b) => b.to_string(),
                    _ => String::new(),
                }),
            },
            OpClass::VarObject => match v {
                KValue::Obj(o) => KValue::Obj(o),
                _ => KValue::Obj(None),
            },
            // Player, random and data variables are not writable.
            OpClass::VarPlayer
            | OpClass::VarRandomInt
            | OpClass::VarRandomFloat
            | OpClass::InterpData => return,
            _ => v,
        };
        if let Some(slot) = self.vars.get_mut(id) {
            *slot = converted;
        }
    }

    /// Variable ids linked to variable links labelled `desc`
    /// (`GetOpVars`, case-insensitive).
    pub(crate) fn vars_of(&self, op: usize, desc: &str) -> Vec<usize> {
        self.graph.node(op).map_or_else(Vec::new, |n| {
            n.variables
                .iter()
                .filter(|l| l.desc.eq_ignore_ascii_case(desc))
                .flat_map(|l| l.vars.iter().copied())
                .collect()
        })
    }

    /// Values flow from linked variables into the op's properties (links
    /// with a `PropertyName`): ints and floats are summed, bools ANDed,
    /// objects take the first non-null, arrays get one element per linked
    /// variable, strings take the first.
    pub(crate) fn populate(&mut self, op: usize) {
        let Some(node) = self.graph.node(op) else {
            return;
        };
        let links: Vec<(String, Vec<usize>)> = node
            .variables
            .iter()
            .filter_map(|l| {
                let p = l.property.clone()?;
                (!l.vars.is_empty()).then(|| (p, l.vars.clone()))
            })
            .collect();
        for (prop, vars) in links {
            let current = self
                .ops
                .get(op)
                .and_then(|o| o.props.get(&prop))
                .cloned()
                .unwrap_or_default();
            let values: Vec<KValue> = vars.iter().map(|v| self.read_var(*v)).collect();
            let first_class = vars
                .first()
                .and_then(|v| self.graph.node(*v))
                .map_or(OpClass::Unknown, |x| x.class);
            let new = if matches!(current, KValue::Array(_)) {
                // One element per linked variable (null objects stay null).
                KValue::Array(values)
            } else {
                match first_class {
                    OpClass::VarInt | OpClass::VarRandomInt => {
                        KValue::Int(values.iter().fold(0i32, |a, v| a.wrapping_add(v.as_int())))
                    }
                    OpClass::VarFloat | OpClass::VarRandomFloat => {
                        KValue::Float(values.iter().fold(0.0f32, |a, v| a + v.as_float()))
                    }
                    OpClass::VarBool => KValue::Bool(values.iter().all(KValue::as_bool)),
                    OpClass::VarObject | OpClass::VarPlayer | OpClass::InterpData => values
                        .into_iter()
                        .find(|v| !matches!(v, KValue::Obj(None)))
                        .unwrap_or(KValue::Obj(None)),
                    _ => values.into_iter().next().unwrap_or_default(),
                }
            };
            if let Some(o) = self.ops.get_mut(op) {
                o.props.insert(prop, new);
            }
        }
    }

    /// Values flow from the op's properties back into the linked variables
    /// (links with a `PropertyName`, unless `bSequenceNeedsPublishing`):
    /// every linked variable takes the property (arrays element by element).
    pub(crate) fn publish(&mut self, op: usize) {
        let Some(node) = self.graph.node(op) else {
            return;
        };
        let links: Vec<(String, Vec<usize>)> = node
            .variables
            .iter()
            .filter(|l| !l.skip_publish)
            .filter_map(|l| {
                let p = l.property.clone()?;
                (!l.vars.is_empty()).then(|| (p, l.vars.clone()))
            })
            .collect();
        for (prop, vars) in links {
            let Some(value) = self.ops.get(op).and_then(|o| o.props.get(&prop)).cloned() else {
                continue;
            };
            match value {
                KValue::Array(items) => {
                    for (v, item) in vars.iter().zip(items) {
                        self.write_var(*v, item);
                    }
                }
                other => {
                    for v in vars {
                        self.write_var(v, other.clone());
                    }
                }
            }
        }
    }

    /// The op property `name` (lower case).
    pub(crate) fn prop(&self, op: usize, name: &str) -> KValue {
        self.ops
            .get(op)
            .and_then(|o| o.props.get(&name.to_ascii_lowercase()))
            .cloned()
            .unwrap_or_default()
    }

    pub(crate) fn set_prop(&mut self, op: usize, name: &str, v: KValue) {
        if let Some(o) = self.ops.get_mut(op) {
            o.props.insert(name.to_ascii_lowercase(), v);
        }
    }

    /// True when input `i` of `op` carries an impulse.
    pub(crate) fn impulse(&self, op: usize, i: usize) -> bool {
        self.ops
            .get(op)
            .and_then(|o| o.in_impulse.get(i))
            .copied()
            .unwrap_or(false)
    }

    // ---------------------------------------------------------------- random

    /// Stand-in for the C library's `rand()` used by `SeqVar_RandomInt`
    /// (ours: a seeded 64-bit LCG; the original's stream is not
    /// reproducible).
    fn next_rand(&mut self) -> i32 {
        self.rand_state = self
            .rand_state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        ((self.rand_state >> 33) & 0x7FFF_FFFF) as i32
    }

    /// `SeqVar_RandomInt::GetRef` arithmetic (CONFIRMED): `rand() %
    /// (max + 1 − min) + min`, the bounds swapped when min > max.
    pub(crate) fn random_int(&mut self, min: i32, max: i32) -> i32 {
        let r = self.next_rand();
        let (lo, hi) = if min < max { (min, max) } else { (max, min) };
        let span = i64::from(hi) - i64::from(lo) + 1;
        if span <= 0 {
            return lo;
        }
        (i64::from(r) % span + i64::from(lo)) as i32
    }

    /// `SeqVar_RandomFloat::GetRef` (CONFIRMED arithmetic of the engine's
    /// `appSRand`: seed = seed · 0x0BB38435 + 0x3619636B; the mantissa bits
    /// make a float in [1, 2) whose fraction scales `max − min`; ours starts
    /// from seed 0).
    pub(crate) fn random_float(&mut self, min: f32, max: f32) -> f32 {
        self.rand_seed = self
            .rand_seed
            .wrapping_mul(0x0BB3_8435)
            .wrapping_add(0x3619_636B);
        let f = f32::from_bits((self.rand_seed & 0x007F_FFFF) | 0x3F80_0000);
        (max - min) * (f - (f as i32) as f32) + min
    }

    // ---------------------------------------------------------------- events

    /// `USequenceEvent::CheckActivate`.
    pub(crate) fn check_activate(
        &mut self,
        ev: usize,
        originator: Obj,
        instigator: Obj,
        test: bool,
        indices: Option<Vec<usize>>,
        push_top: bool,
    ) -> bool {
        let Some(node) = self.graph.node(ev) else {
            return false;
        };
        if node.kind != NodeKind::Event {
            return false;
        }
        if let Some(p) = node.parent
            && !self.sequence_enabled(p)
        {
            return false;
        }
        let def = node.event.clone();
        let max = def.as_ref().map_or(0, |d| d.max_trigger_count);
        let delay = def.as_ref().map_or(0.0, |d| d.retrigger_delay);
        let player_only = def.as_ref().is_some_and(|d| d.player_only);
        if !originator.is_some() {
            return false;
        }
        if player_only && instigator != Obj::Player {
            return false;
        }
        let Some(st) = self.ops.get(ev) else {
            return false;
        };
        if !(max == 0 || st.trigger_count < max) {
            return false;
        }
        if delay == 0.0 || st.trigger_count == 0 {
            if test {
                return true;
            }
        } else {
            let elapsed = (self.time - st.activation_time) as f32;
            if test {
                return delay < elapsed;
            }
            if elapsed <= delay {
                return false;
            }
        }
        if st.enabled {
            self.activate_event(
                ev,
                originator,
                instigator,
                indices.as_deref(),
                push_top,
                false,
            );
        }
        true
    }

    /// `USequenceEvent::ActivateEvent`.
    pub(crate) fn activate_event(
        &mut self,
        ev: usize,
        originator: Obj,
        instigator: Obj,
        indices: Option<&[usize]>,
        push_top: bool,
        from_queued: bool,
    ) {
        let time = self.time;
        let Some(st) = self.ops.get_mut(ev) else {
            return;
        };
        st.originator = originator.clone();
        st.instigator = instigator.clone();
        if !from_queued {
            st.activation_time = time;
            st.trigger_count = st.trigger_count.saturating_add(1);
        }
        let Some(parent) = self.graph.node(ev).and_then(|x| x.parent) else {
            return;
        };
        if st.active {
            let full = self
                .seqs
                .get(parent)
                .is_some_and(|s| s.queued.len() >= MAX_QUEUED_ACTIVATIONS);
            if full {
                self.error(format!(
                    "event {ev}: activation queue full; activation dropped"
                ));
                return;
            }
            if let Some(s) = self.seqs.get_mut(parent) {
                s.queued.push_back(QueuedActivation {
                    event: ev,
                    originator,
                    instigator,
                    indices: indices.map(<[usize]>::to_vec),
                    push_top,
                });
            }
            return;
        }
        st.active = true;
        st.activate_count = st.activate_count.saturating_add(1);
        let class_name = self
            .graph
            .node(ev)
            .map(|x| x.class_short().to_owned())
            .unwrap_or_default();
        *self.stats.activations.entry(class_name).or_insert(0) += 1;
        self.event_script_activated(ev);
        // Instigator variables, then property links.
        let inst_value = self.obj_value(&instigator);
        for v in self.vars_of(ev, "Instigator") {
            self.write_var(v, inst_value.clone());
        }
        self.publish(ev);
        let n_out = self.graph.node(ev).map_or(0, |x| x.outputs.len());
        match indices {
            None => {
                for i in 0..n_out {
                    self.activate_output(ev, i);
                }
            }
            Some(list) => {
                for &i in list {
                    if i < n_out {
                        self.activate_output(ev, i);
                    }
                }
            }
        }
        self.queue_op(ev, push_top);
    }

    /// Events of `class` in the attached game sequence (depth-first member
    /// order), the way the game's script finds them.
    pub(crate) fn events_of(&self, class: OpClass) -> Vec<usize> {
        self.graph.find_by_class(&self.attached_roots(), class)
    }

    /// Calls a script-side event function that fires output `index` when the
    /// event is enabled (`PlayerLanded`, `PlayerDied`, narrator events...).
    fn fire_enabled(&mut self, class: OpClass, index: usize) {
        for e in self.events_of(class) {
            if self.ops.get(e).is_some_and(|o| o.enabled) {
                self.force_output(e, index);
            }
        }
    }

    // ---------------------------------------------------------------- game events

    /// The player began (`touching`) or stopped touching `actor`
    /// (`USeqEvent_Touch::CheckTouchActivate` / `CheckUnTouchActivate`): the
    /// touch events whose originator is the actor fire `Touched`, or
    /// `UnTouched` (plus `Empty` once nothing touches) after an earlier
    /// touch.
    pub fn touch(&mut self, actor: ActorRef, touching: bool) {
        let events = self.touch_events.get(&actor).cloned().unwrap_or_default();
        for e in events {
            if !self.ops.get(e).is_some_and(|o| o.enabled) {
                continue;
            }
            if touching {
                if !self.check_activate(e, Obj::Actor(actor), Obj::Player, true, None, false) {
                    continue;
                }
                if let Some(o) = self.ops.get_mut(e) {
                    o.touched = true;
                }
                self.activate_event(e, Obj::Actor(actor), Obj::Player, Some(&[0]), false, false);
            } else {
                if !self.ops.get(e).is_some_and(|o| o.touched) {
                    continue;
                }
                // The check runs with the activation time zeroed and the
                // player-only flag cleared.
                let saved = self.ops.get(e).map_or(0.0, |o| o.activation_time);
                if let Some(o) = self.ops.get_mut(e) {
                    o.activation_time = 0.0;
                }
                let ok = self.check_activate(e, Obj::Actor(actor), Obj::Player, true, None, false);
                if let Some(o) = self.ops.get_mut(e) {
                    o.activation_time = saved;
                }
                if !ok {
                    continue;
                }
                if let Some(o) = self.ops.get_mut(e) {
                    o.touched = false;
                }
                self.activate_event(
                    e,
                    Obj::Actor(actor),
                    Obj::Player,
                    Some(&[1, 2]),
                    false,
                    false,
                );
            }
        }
    }

    /// `GrappleGun.TriggerPlayerGrappledEvent`: every player-grappled event is
    /// checked with the grappled interactable actor (or the world) as
    /// originator and output indices {1, 1}; the event's own activation
    /// handler pulses its output only when its `inputActor` is the
    /// originator.
    pub fn player_grappled(&mut self, actor: Option<ActorRef>) {
        let originator = actor.map_or(Obj::World, Obj::Actor);
        for e in self.events_of(OpClass::PlayerGrappled) {
            self.check_activate(
                e,
                originator.clone(),
                Obj::None,
                false,
                Some(vec![1, 1]),
                false,
            );
        }
    }

    /// `TriggerPlayerReleasedGrappleEvent`.
    pub fn player_released_grapple(&mut self) {
        for e in self.events_of(OpClass::PlayerReleasedGrapple) {
            self.check_activate(e, Obj::World, Obj::None, false, None, false);
        }
    }

    /// The pawn's landing event (`PlayerLanded` on every event).
    pub fn player_landed(&mut self) {
        self.fire_enabled(OpClass::PlayerLanded, 0);
    }

    /// The pawn's death event (`PlayerDied` on every event; raised by the
    /// death sequence's reset, see `asamu_world::WorldEvent::PlayerRespawned`).
    pub fn player_died(&mut self) {
        self.fire_enabled(OpClass::PlayerDied, 0);
    }

    /// Rocket boots: charge start (`boosting == false`, output 0) or boost
    /// start (output 1).
    pub fn player_rocket_boosted(&mut self, boosting: bool) {
        self.fire_enabled(OpClass::PlayerRocketBoosted, usize::from(boosting));
    }

    /// `ASAMUInteractable_Actor` interaction: every interacted-with event
    /// counts the call (up to its trigger count) and fires when its
    /// originator is the actor.
    pub fn actor_interacted_with(&mut self, actor: ActorRef) {
        for e in self.events_of(OpClass::ActorInteractedWith) {
            let max = self
                .graph
                .node(e)
                .and_then(|x| x.event.as_ref())
                .map_or(0, |d| d.max_trigger_count);
            let originator = self
                .graph
                .node(e)
                .and_then(|x| x.event.as_ref())
                .and_then(|d| d.originator.as_deref())
                .and_then(|p| self.graph.actor_by_path(p));
            let Some(st) = self.ops.get_mut(e) else {
                continue;
            };
            if max == 0 || st.script_count < max {
                st.script_count = st.script_count.saturating_add(1);
                if originator == Some(actor) {
                    self.force_output(e, 0);
                }
            }
        }
    }

    /// `ASAMUCollectible` pickup (checked with the world as originator).
    pub fn collectible_collected(&mut self) {
        for e in self.events_of(OpClass::CollectibleCollected) {
            self.check_activate(e, Obj::World, Obj::None, false, None, false);
        }
    }

    /// A saved game state was loaded (`SaveLoaded(i)`): events with index −1
    /// or `i` fire.
    pub fn saved_game_state_loaded(&mut self, index: i32) {
        for e in self.events_of(OpClass::SavedGameStateLoaded) {
            let want = self
                .graph
                .node(e)
                .and_then(|x| x.param("Index"))
                .map_or(-1, KValue::as_int);
            if want == -1 || want == index {
                self.force_output(e, 0);
            }
        }
    }

    /// The credits finished (`SeqEvent_CreditsEnded`).
    pub fn credits_ended(&mut self) {
        for e in self.events_of(OpClass::CreditsEnded) {
            self.force_output(e, 0);
        }
    }

    /// A worm event (`output`: 0 waking up … 6 finished alerted).
    pub fn worm_event(&mut self, output: usize) {
        self.fire_enabled(OpClass::WormEvents, output);
    }

    /// An animation notify on `actor` (`SeqEvent_AnimNotify` whose originator
    /// is the actor and whose `NotifyName` matches; TENTATIVE dispatch).
    pub fn anim_notify(&mut self, actor: ActorRef, notify: &str) {
        for e in self.events_of(OpClass::AnimNotify) {
            let Some(node) = self.graph.node(e) else {
                continue;
            };
            let orig = node
                .event
                .as_ref()
                .and_then(|d| d.originator.as_deref())
                .and_then(|p| self.graph.actor_by_path(p));
            let name_ok = node
                .param("NotifyName")
                .and_then(KValue::as_str)
                .is_some_and(|n| n.eq_ignore_ascii_case(notify));
            if orig == Some(actor) && name_ok {
                self.check_activate(e, Obj::Actor(actor), Obj::None, false, None, false);
            }
        }
    }

    /// `SeqEvent_Used` on `actor` (stock event; not in the shipped maps).
    pub fn used(&mut self, actor: ActorRef) {
        self.actor_event(OpClass::Used, actor);
    }

    /// `SeqEvent_Destroyed` on `actor` (stock event; not in the shipped maps).
    pub fn destroyed(&mut self, actor: ActorRef) {
        self.actor_event(OpClass::Destroyed, actor);
    }

    fn actor_event(&mut self, class: OpClass, actor: ActorRef) {
        for e in self.events_of(class) {
            let orig = self
                .graph
                .node(e)
                .and_then(|x| x.event.as_ref())
                .and_then(|d| d.originator.as_deref())
                .and_then(|p| self.graph.actor_by_path(p));
            if orig == Some(actor) {
                self.check_activate(e, Obj::Actor(actor), Obj::Player, false, None, false);
            }
        }
    }

    /// An attractor pad started by `SeqAct_ToggleAttractor` finished
    /// (`Finished` output).
    pub fn attractor_finished(&mut self, actor: ActorRef) {
        let done: Vec<usize> = self
            .attractors
            .iter()
            .filter(|p| p.actor == actor)
            .map(|p| p.node)
            .collect();
        self.attractors.retain(|p| p.actor != actor);
        for n in done {
            self.force_output(n, 1);
        }
    }

    // ---------------------------------------------------------------- music

    fn init_adaptive_music(&mut self, _host: &mut dyn Host) {
        for a in self
            .graph
            .find_by_class(&self.attached_roots(), OpClass::AddAdaptiveTracks)
        {
            let tracks = self.prop(a, "tracksToAdd");
            for t in tracks.items() {
                if let Some(id) = t.field("ID").and_then(KValue::as_str) {
                    self.tracks.push(id.to_owned());
                }
            }
            self.emit(Output::AdaptiveTracks { tracks });
            // The original activates output 0 ("Out") as part of the action; every
            // shipped instance links it to a mute-all multiplier edit, so the stems
            // start silent (AUDIO.md, "Adaptive music", cross-check 1).
            self.force_output(a, 0);
        }
        for e in self.events_of(OpClass::TrackBeat) {
            let Some(node) = self.graph.node(e) else {
                continue;
            };
            let id = node
                .param("trackID")
                .and_then(KValue::as_str)
                .unwrap_or("")
                .to_owned();
            let interval = node.param("beatAmount").map_or(0.0, KValue::as_float);
            if self.tracks.iter().any(|t| t.eq_ignore_ascii_case(&id)) {
                self.beats.push(Beat {
                    event: e,
                    interval,
                    remaining: Some(interval),
                });
            }
        }
    }

    /// Beat actors: a latent `Sleep(beatAmount)` loop that pulses the event
    /// after each sleep (wakes when the remaining time is below half a frame,
    /// GRAPPLE.md G-TM-3).
    fn tick_beats(&mut self, dt: f32) {
        let mut fire = Vec::new();
        for b in &mut self.beats {
            if let Some(r) = b.remaining.as_mut() {
                *r -= dt;
                if *r < dt * 0.5 {
                    fire.push(b.event);
                    *r = b.interval;
                }
            }
        }
        for e in fire {
            self.force_output(e, 0);
        }
    }

    // ---------------------------------------------------------------- narrator

    fn tick_narrator(&mut self, dt: f32) {
        let calls = self.narrator.tick(dt);
        self.apply_narrator_calls(calls);
    }

    pub(crate) fn apply_narrator_calls(&mut self, calls: Vec<crate::narrator::Call>) {
        use crate::narrator::Call;
        for c in calls {
            match c {
                Call::Play {
                    node,
                    id,
                    cue,
                    volume,
                } => {
                    self.emit(Output::NarratorLine {
                        node,
                        id,
                        cue,
                        volume,
                    });
                }
                Call::Stop { id } => self.emit(Output::NarratorStop { id }),
                Call::Finished { node } => self.force_output(node, 2),
                Call::Started => self.fire_enabled(OpClass::NarratorEvents, 0),
                Call::AllFinished => self.fire_enabled(OpClass::NarratorEvents, 1),
            }
        }
    }

    pub(crate) fn cue_duration(&self, cue: Option<&str>) -> f32 {
        cue.and_then(|c| self.graph.sound(c))
            .and_then(|s| s.duration)
            .unwrap_or(0.0)
    }

    pub(crate) fn first_wave_duration(&self, cue: Option<&str>) -> f32 {
        cue.and_then(|c| self.graph.sound(c))
            .and_then(|s| s.first_wave_duration)
            .unwrap_or(0.0)
    }
}

/// The initial value of variable node `id`.
fn initial_var(graph: &Graph, id: usize) -> KValue {
    let Some(node) = graph.node(id) else {
        return KValue::None;
    };
    if node.kind != NodeKind::Variable {
        return KValue::None;
    }
    let v = node
        .var
        .as_ref()
        .map(|v| v.value.clone())
        .unwrap_or_default();
    match node.class {
        OpClass::VarBool => KValue::Bool(v.as_bool()),
        OpClass::VarInt => KValue::Int(v.as_int()),
        OpClass::VarFloat => KValue::Float(v.as_float()),
        OpClass::VarString => KValue::Str(v.as_str().unwrap_or("").to_owned()),
        OpClass::VarObject => match v {
            KValue::Obj(o) => KValue::Obj(o),
            _ => KValue::Obj(None),
        },
        OpClass::VarPlayer => KValue::Obj(Some(PLAYER_PATH.to_owned())),
        OpClass::InterpData => KValue::Obj(Some(node.path.clone())),
        _ => v,
    }
}
