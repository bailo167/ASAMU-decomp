//! The runtime graph: the importer's per-map Kismet export
//! (`<converted>/kismet/<map>.kismet.json`, format [`RUNTIME_FORMAT`]),
//! validated and indexed for the interpreter.
//!
//! The JSON is written by `tools/asamu-import/src/kismet.rs` from
//! `asamu_ue3::kismet::KismetGraph` (level-scope nodes only; prefab
//! archetypes are not executed). Every reference is a node id, an actor
//! path (resolved through the `actors` table) or a plain object path.
//! Input is untrusted: ids are range-checked, links into missing ports are
//! dropped with a warning, sizes are capped, nothing panics.
//!
//! Several graphs (a persistent level and its streamed sub-levels) can be
//! merged into one [`Graph`] with [`Graph::merge`]; node ids of later graphs
//! are offset, and their root sequences stay detached until the runtime
//! streams the level in (as the engine nests a streamed level's sequence
//! under the persistent one when it becomes visible; KISMET_RUNTIME.md).

use std::collections::BTreeMap;

use serde::Deserialize;
use thiserror::Error;

use crate::ops::OpClass;
use crate::value::KValue;

/// `format` of a runtime graph file.
pub const RUNTIME_FORMAT: &str = "asamu-kismet-runtime";
/// Supported `version`.
pub const RUNTIME_VERSION: u32 = 1;
/// Most nodes accepted in one graph.
pub const MAX_NODES: usize = 1 << 20;
/// Most links (output links, variable links, event links) per port.
pub const MAX_PORT_LINKS: usize = 1 << 14;
/// Most ports of each kind per node.
pub const MAX_PORTS: usize = 1 << 10;
/// Most warnings kept.
pub const MAX_WARNINGS: usize = 256;

/// Errors that reject a graph file.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum GraphError {
    /// Not valid JSON of the expected shape.
    #[error("invalid runtime graph JSON: {0}")]
    Json(String),
    /// Wrong `format` or unsupported `version`.
    #[error("unsupported runtime graph ({format} v{version})")]
    Format {
        /// `format` found.
        format: String,
        /// `version` found.
        version: u32,
    },
    /// Node ids are not `0..n` in order, or too many nodes.
    #[error("bad node table: {0}")]
    Nodes(String),
}

// ------------------------------------------------------------------ JSON

/// One input link (JSON).
#[derive(Debug, Clone, Default, Deserialize)]
pub struct InputJson {
    /// `LinkDesc`.
    #[serde(default)]
    pub desc: String,
    /// `ActivateDelay`.
    #[serde(default)]
    pub delay: f32,
    /// `bDisabled`.
    #[serde(default)]
    pub disabled: bool,
}

/// One activation link of an output (JSON).
#[derive(Debug, Clone, Copy, Default, Deserialize)]
pub struct LinkJson {
    /// Target node id.
    pub op: usize,
    /// Target input index.
    #[serde(default)]
    pub input: usize,
}

/// One output link (JSON).
#[derive(Debug, Clone, Default, Deserialize)]
pub struct OutputJson {
    /// `LinkDesc`.
    #[serde(default)]
    pub desc: String,
    /// `ActivateDelay`.
    #[serde(default)]
    pub delay: f32,
    /// `bDisabled`.
    #[serde(default)]
    pub disabled: bool,
    /// Activation links.
    #[serde(default)]
    pub links: Vec<LinkJson>,
}

/// One variable link (JSON).
#[derive(Debug, Clone, Default, Deserialize)]
pub struct VarLinkJson {
    /// `LinkDesc`.
    #[serde(default)]
    pub desc: String,
    /// `PropertyName` (the op property the variables feed).
    #[serde(default)]
    pub property: Option<String>,
    /// `bWriteable`.
    #[serde(default)]
    pub writeable: bool,
    /// `bSequenceNeedsPublishing` (such links are not written back).
    #[serde(default)]
    pub skip_publish: bool,
    /// Linked variable node ids (named variables already resolved).
    #[serde(default)]
    pub vars: Vec<usize>,
}

/// One event link (JSON).
#[derive(Debug, Clone, Default, Deserialize)]
pub struct EventLinkJson {
    /// `LinkDesc`.
    #[serde(default)]
    pub desc: String,
    /// Linked event node ids.
    #[serde(default)]
    pub events: Vec<usize>,
}

/// Event values (JSON).
#[derive(Debug, Clone, Default, Deserialize)]
pub struct EventJson {
    /// `Originator` path.
    #[serde(default)]
    pub originator: Option<String>,
    /// `MaxTriggerCount` (0 = unlimited).
    #[serde(default)]
    pub max_trigger_count: i32,
    /// `ReTriggerDelay` seconds.
    #[serde(default)]
    pub retrigger_delay: f32,
    /// `bPlayerOnly`.
    #[serde(default)]
    pub player_only: bool,
    /// `Priority`.
    #[serde(default)]
    pub priority: i32,
}

/// Variable values (JSON).
#[derive(Debug, Clone, Default, Deserialize)]
pub struct VarJson {
    /// `VarName`.
    #[serde(default)]
    pub var_name: Option<String>,
    /// The value property's value (`bValue`, `IntValue`, `ObjValue`, ...).
    #[serde(default)]
    pub value: serde_json::Value,
    /// `SeqVar_Named.FindVarName`.
    #[serde(default)]
    pub find_var_name: Option<String>,
}

/// One node (JSON).
#[derive(Debug, Clone, Default, Deserialize)]
pub struct NodeJson {
    /// Node id (equals its index).
    pub id: usize,
    /// Object path.
    #[serde(default)]
    pub path: String,
    /// Qualified class (`Engine.SeqAct_Toggle`).
    #[serde(default)]
    pub class: String,
    /// Role (`sequence`, `event`, `action`, `condition`, `variable`, ...).
    #[serde(default)]
    pub kind: String,
    /// Parent sequence id.
    #[serde(default)]
    pub parent: Option<usize>,
    /// `ObjName` (editor label).
    #[serde(default)]
    pub obj_name: Option<String>,
    /// `bEnabled` (events and sequences).
    #[serde(default)]
    pub enabled: Option<bool>,
    /// Input links.
    #[serde(default)]
    pub inputs: Vec<InputJson>,
    /// Output links.
    #[serde(default)]
    pub outputs: Vec<OutputJson>,
    /// Variable links.
    #[serde(default)]
    pub variables: Vec<VarLinkJson>,
    /// Event links.
    #[serde(default)]
    pub event_links: Vec<EventLinkJson>,
    /// Sequence members (`SequenceObjects` order).
    #[serde(default)]
    pub members: Vec<usize>,
    /// Event values.
    #[serde(default)]
    pub event: Option<EventJson>,
    /// Variable values.
    #[serde(default)]
    pub var: Option<VarJson>,
    /// Class-specific property values (effective).
    #[serde(default)]
    pub params: BTreeMap<String, serde_json::Value>,
    /// `bAutoActivateOutputLinks` (class default).
    #[serde(default)]
    pub auto_activate_outputs: bool,
    /// `bLatentExecution` (class default).
    #[serde(default)]
    pub latent: bool,
    /// The class derives from `SeqAct_Latent` (its deactivation picks the
    /// finished/aborted output).
    #[serde(default)]
    pub latent_base: bool,
}

/// One referenced actor (JSON).
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ActorJson {
    /// Object path.
    pub path: String,
    /// Object name.
    #[serde(default)]
    pub name: String,
    /// Qualified class.
    #[serde(default)]
    pub class: String,
    /// Gameplay kind (scene classification).
    #[serde(default)]
    pub kind: String,
    /// Level package that holds it.
    #[serde(default)]
    pub package: String,
    /// Index in `ULevel::Actors`.
    #[serde(default)]
    pub slot: usize,
    /// `Location`.
    #[serde(default)]
    pub location: [f32; 3],
    /// `Rotation` (pitch, yaw, roll).
    #[serde(default)]
    pub rotation: [i32; 3],
    /// `DrawScale`.
    #[serde(default = "one")]
    pub draw_scale: f32,
    /// `DrawScale3D`.
    #[serde(default = "ones")]
    pub draw_scale3d: [f32; 3],
    /// `PrePivot`.
    #[serde(default)]
    pub pre_pivot: [f32; 3],
    /// `Base` path.
    #[serde(default)]
    pub base: Option<String>,
    /// `bHidden`.
    #[serde(default)]
    pub hidden: bool,
}

fn one() -> f32 {
    1.0
}

fn ones() -> [f32; 3] {
    [1.0; 3]
}

/// Durations of a referenced sound cue (JSON).
#[derive(Debug, Clone, Copy, Default, Deserialize, PartialEq)]
pub struct SoundJson {
    /// `SoundCue.Duration` (cooker-computed; 10000 marks a looping cue).
    #[serde(default)]
    pub duration: Option<f32>,
    /// `Duration` of the first wave node (depth first from `FirstNode`).
    #[serde(default)]
    pub first_wave_duration: Option<f32>,
}

/// One sample of a Matinee cross-check probe (JSON).
#[derive(Debug, Clone, Copy, Default, Deserialize, PartialEq)]
pub struct ProbeSample {
    /// Matinee time.
    pub t: f32,
    /// World location from `asamu_ue3::matinee`.
    pub location: [f32; 3],
    /// Rotation from `asamu_ue3::matinee`.
    pub rotation: [i32; 3],
}

/// A Matinee cross-check probe (JSON): one bound actor of one move track,
/// sampled by the importer with `asamu_ue3::matinee` (initial transform at
/// `position`), for comparison with this crate's port.
#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
pub struct ProbeJson {
    /// `SeqAct_Interp` node id.
    pub action: usize,
    /// Group name.
    pub group: String,
    /// Bound actor path.
    pub actor: String,
    /// Action position the initial transform was taken at.
    #[serde(default)]
    pub position: f32,
    /// Samples.
    #[serde(default)]
    pub samples: Vec<ProbeSample>,
}

/// The whole file (JSON).
#[derive(Debug, Clone, Default, Deserialize)]
pub struct GraphJson {
    /// [`RUNTIME_FORMAT`].
    pub format: String,
    /// [`RUNTIME_VERSION`].
    pub version: u32,
    /// Map package.
    #[serde(default)]
    pub package: String,
    /// Nodes in id order.
    #[serde(default)]
    pub nodes: Vec<NodeJson>,
    /// Actors referenced by the graph.
    #[serde(default)]
    pub actors: Vec<ActorJson>,
    /// Sound cues referenced by sound and narrator actions (path → durations).
    #[serde(default)]
    pub sounds: BTreeMap<String, SoundJson>,
    /// Matinee cross-check probes.
    #[serde(default)]
    pub probes: Vec<ProbeJson>,
}

// ------------------------------------------------------------------ model

/// Role of a node.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum NodeKind {
    /// A sequence.
    Sequence,
    /// An event.
    Event,
    /// An action.
    Action,
    /// A condition.
    Condition,
    /// A variable (including `InterpData`).
    Variable,
    /// A comment frame or anything else.
    Other,
}

impl NodeKind {
    fn parse(s: &str) -> NodeKind {
        match s {
            "sequence" => NodeKind::Sequence,
            "event" => NodeKind::Event,
            "action" => NodeKind::Action,
            "condition" => NodeKind::Condition,
            "variable" => NodeKind::Variable,
            _ => NodeKind::Other,
        }
    }

    /// True for kinds with ports that the interpreter executes.
    #[must_use]
    pub fn is_op(self) -> bool {
        matches!(
            self,
            NodeKind::Sequence | NodeKind::Event | NodeKind::Action | NodeKind::Condition
        )
    }
}

/// Index into [`Graph::actors`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ActorRef(pub u32);

/// A referenced actor.
#[derive(Debug, Clone, PartialEq)]
pub struct ActorInfo {
    /// Object path.
    pub path: String,
    /// Object name.
    pub name: String,
    /// Qualified class.
    pub class: String,
    /// Scene kind (`trigger`, `interp_actor`, ...).
    pub kind: String,
    /// Level package.
    pub package: String,
    /// `ULevel::Actors` index.
    pub slot: usize,
    /// Placed location.
    pub location: [f32; 3],
    /// Placed rotation.
    pub rotation: [i32; 3],
    /// `DrawScale`.
    pub draw_scale: f32,
    /// `DrawScale3D`.
    pub draw_scale3d: [f32; 3],
    /// `PrePivot`.
    pub pre_pivot: [f32; 3],
    /// Base actor, if attached and in the table.
    pub base: Option<ActorRef>,
    /// `bHidden`.
    pub hidden: bool,
}

impl ActorInfo {
    /// Short class name (`Trigger`).
    #[must_use]
    pub fn class_name(&self) -> &str {
        self.class.rsplit('.').next().unwrap_or(&self.class)
    }
}

/// An input link.
#[derive(Debug, Clone, PartialEq)]
pub struct Input {
    /// Label.
    pub desc: String,
    /// `ActivateDelay`.
    pub delay: f32,
    /// `bDisabled`.
    pub disabled: bool,
}

/// An output link.
#[derive(Debug, Clone, PartialEq)]
pub struct OutputPort {
    /// Label.
    pub desc: String,
    /// `ActivateDelay`.
    pub delay: f32,
    /// `bDisabled`.
    pub disabled: bool,
    /// `(target op, target input)` pairs, in stored order.
    pub links: Vec<(usize, usize)>,
}

/// A variable link.
#[derive(Debug, Clone, PartialEq)]
pub struct VarLink {
    /// Label.
    pub desc: String,
    /// `PropertyName`, lower case.
    pub property: Option<String>,
    /// `bWriteable`.
    pub writeable: bool,
    /// `bSequenceNeedsPublishing`.
    pub skip_publish: bool,
    /// Variable node ids.
    pub vars: Vec<usize>,
}

/// An event link.
#[derive(Debug, Clone, PartialEq)]
pub struct EventLink {
    /// Label.
    pub desc: String,
    /// Event node ids.
    pub events: Vec<usize>,
}

/// Event settings.
#[derive(Debug, Clone, PartialEq)]
pub struct EventDef {
    /// Originator actor path.
    pub originator: Option<String>,
    /// `MaxTriggerCount`.
    pub max_trigger_count: i32,
    /// `ReTriggerDelay`.
    pub retrigger_delay: f32,
    /// `bPlayerOnly`.
    pub player_only: bool,
}

/// Variable settings.
#[derive(Debug, Clone, PartialEq)]
pub struct VarDef {
    /// `VarName`.
    pub var_name: Option<String>,
    /// Initial value.
    pub value: KValue,
}

/// One node.
#[derive(Debug, Clone, PartialEq)]
pub struct Node {
    /// Id (index into [`Graph::nodes`]).
    pub id: usize,
    /// Object path.
    pub path: String,
    /// Qualified class.
    pub class_name: String,
    /// Interpreter class.
    pub class: OpClass,
    /// Role.
    pub kind: NodeKind,
    /// Parent sequence.
    pub parent: Option<usize>,
    /// `bEnabled` (events and sequences; true otherwise).
    pub enabled: bool,
    /// Inputs.
    pub inputs: Vec<Input>,
    /// Outputs.
    pub outputs: Vec<OutputPort>,
    /// Variable links.
    pub variables: Vec<VarLink>,
    /// Event links.
    pub event_links: Vec<EventLink>,
    /// Sequence members.
    pub members: Vec<usize>,
    /// Event settings.
    pub event: Option<EventDef>,
    /// Variable settings.
    pub var: Option<VarDef>,
    /// Class-specific properties (lower-case names).
    pub params: BTreeMap<String, KValue>,
    /// `bAutoActivateOutputLinks`.
    pub auto_activate_outputs: bool,
    /// `bLatentExecution`.
    pub latent: bool,
    /// Derives from `SeqAct_Latent`.
    pub latent_base: bool,
    /// Index of the level ([`Graph::levels`]) the node belongs to.
    pub level: usize,
}

impl Node {
    /// Short class name.
    #[must_use]
    pub fn class_short(&self) -> &str {
        self.class_name
            .rsplit('.')
            .next()
            .unwrap_or(&self.class_name)
    }

    /// Effective property `name` (case-insensitive).
    #[must_use]
    pub fn param(&self, name: &str) -> Option<&KValue> {
        self.params.get(&name.to_ascii_lowercase())
    }

    /// Index of the output labelled `desc` (case-insensitive).
    #[must_use]
    pub fn output_named(&self, desc: &str) -> Option<usize> {
        self.outputs
            .iter()
            .position(|o| o.desc.eq_ignore_ascii_case(desc))
    }
}

/// One level of a (merged) graph.
#[derive(Debug, Clone, PartialEq)]
pub struct GraphLevel {
    /// Package name.
    pub package: String,
    /// Root sequence ids.
    pub roots: Vec<usize>,
}

/// A validated graph.
#[derive(Debug, Clone, PartialEq)]
pub struct Graph {
    /// Levels: index 0 is the persistent level, later ones are streamed
    /// sub-levels merged with [`Graph::merge`].
    pub levels: Vec<GraphLevel>,
    /// Nodes.
    pub nodes: Vec<Node>,
    /// Referenced actors.
    pub actors: Vec<ActorInfo>,
    actor_by_path: BTreeMap<String, ActorRef>,
    node_by_path: BTreeMap<String, usize>,
    /// Sound durations by lower-case cue path.
    pub sounds: BTreeMap<String, SoundJson>,
    /// Matinee cross-check probes.
    pub probes: Vec<ProbeJson>,
    /// Problems found while validating.
    pub warnings: Vec<String>,
}

fn warn(w: &mut Vec<String>, msg: String) {
    if w.len() < MAX_WARNINGS {
        w.push(msg);
    }
}

impl Graph {
    /// Parses and validates a runtime graph file.
    ///
    /// # Errors
    /// Invalid JSON, wrong format/version, or a broken node table.
    pub fn from_json_slice(data: &[u8]) -> Result<Graph, GraphError> {
        let file: GraphJson =
            serde_json::from_slice(data).map_err(|e| GraphError::Json(e.to_string()))?;
        Graph::from_file(file)
    }

    /// Validates a parsed file.
    ///
    /// # Errors
    /// Wrong format/version, or a broken node table.
    pub fn from_file(file: GraphJson) -> Result<Graph, GraphError> {
        if file.format != RUNTIME_FORMAT || file.version != RUNTIME_VERSION {
            return Err(GraphError::Format {
                format: file.format,
                version: file.version,
            });
        }
        let n = file.nodes.len();
        if n > MAX_NODES {
            return Err(GraphError::Nodes(format!("{n} nodes exceed the limit")));
        }
        if let Some((i, bad)) = file.nodes.iter().enumerate().find(|(i, x)| x.id != *i) {
            return Err(GraphError::Nodes(format!(
                "node at index {i} has id {}",
                bad.id
            )));
        }
        let mut warnings = Vec::new();
        let kinds: Vec<NodeKind> = file
            .nodes
            .iter()
            .map(|x| NodeKind::parse(&x.kind))
            .collect();
        let input_counts: Vec<usize> = file.nodes.iter().map(|x| x.inputs.len()).collect();
        let is =
            |id: usize, want: &dyn Fn(NodeKind) -> bool| kinds.get(id).is_some_and(|k| want(*k));

        // Actors.
        let mut actors: Vec<ActorInfo> = Vec::with_capacity(file.actors.len());
        let mut actor_by_path: BTreeMap<String, ActorRef> = BTreeMap::new();
        for a in &file.actors {
            let key = a.path.to_ascii_lowercase();
            if actor_by_path.contains_key(&key) || actors.len() >= MAX_NODES {
                continue;
            }
            let Ok(idx) = u32::try_from(actors.len()) else {
                break;
            };
            actor_by_path.insert(key, ActorRef(idx));
            actors.push(ActorInfo {
                path: a.path.clone(),
                name: a.name.clone(),
                class: a.class.clone(),
                kind: a.kind.clone(),
                package: a.package.clone(),
                slot: a.slot,
                location: a.location,
                rotation: a.rotation,
                draw_scale: a.draw_scale,
                draw_scale3d: a.draw_scale3d,
                pre_pivot: a.pre_pivot,
                base: None,
                hidden: a.hidden,
            });
        }
        for (a, j) in actors.iter_mut().zip(&file.actors) {
            a.base = j
                .base
                .as_ref()
                .and_then(|b| actor_by_path.get(&b.to_ascii_lowercase()).copied());
        }

        let mut nodes = Vec::with_capacity(n);
        for (id, x) in file.nodes.iter().enumerate() {
            let kind = kinds.get(id).copied().unwrap_or(NodeKind::Other);
            let parent = x.parent.filter(|p| {
                let ok = is(*p, &|k| k == NodeKind::Sequence);
                if !ok {
                    warn(&mut warnings, format!("node {id}: bad parent {p}"));
                }
                ok
            });
            let inputs: Vec<Input> = x
                .inputs
                .iter()
                .take(MAX_PORTS)
                .map(|i| Input {
                    desc: i.desc.clone(),
                    delay: finite_or_zero(i.delay),
                    disabled: i.disabled,
                })
                .collect();
            let mut outputs = Vec::new();
            for (oi, o) in x.outputs.iter().take(MAX_PORTS).enumerate() {
                let mut links = Vec::new();
                for l in o.links.iter().take(MAX_PORT_LINKS) {
                    let in_range = input_counts.get(l.op).is_some_and(|c| l.input < *c);
                    if is(l.op, &NodeKind::is_op) && in_range {
                        links.push((l.op, l.input));
                    } else {
                        warn(
                            &mut warnings,
                            format!("node {id} output {oi}: bad link to {}:{}", l.op, l.input),
                        );
                    }
                }
                outputs.push(OutputPort {
                    desc: o.desc.clone(),
                    delay: finite_or_zero(o.delay),
                    disabled: o.disabled,
                    links,
                });
            }
            let variables = x
                .variables
                .iter()
                .take(MAX_PORTS)
                .map(|v| VarLink {
                    desc: v.desc.clone(),
                    property: v
                        .property
                        .as_ref()
                        .filter(|p| !p.is_empty() && !p.eq_ignore_ascii_case("None"))
                        .map(|p| p.to_ascii_lowercase()),
                    writeable: v.writeable,
                    skip_publish: v.skip_publish,
                    vars: v
                        .vars
                        .iter()
                        .take(MAX_PORT_LINKS)
                        .copied()
                        .filter(|t| is(*t, &|k| k == NodeKind::Variable))
                        .collect(),
                })
                .collect();
            let event_links = x
                .event_links
                .iter()
                .take(MAX_PORTS)
                .map(|e| EventLink {
                    desc: e.desc.clone(),
                    events: e
                        .events
                        .iter()
                        .take(MAX_PORT_LINKS)
                        .copied()
                        .filter(|t| is(*t, &|k| k == NodeKind::Event))
                        .collect(),
                })
                .collect();
            let members = x
                .members
                .iter()
                .copied()
                .filter(|m| *m < n && *m != id)
                .collect();
            let params = x
                .params
                .iter()
                .take(MAX_PORT_LINKS)
                .map(|(k, v)| (k.to_ascii_lowercase(), KValue::from_json(v)))
                .collect();
            nodes.push(Node {
                id,
                path: x.path.clone(),
                class_name: x.class.clone(),
                class: OpClass::from_class(&x.class, kind),
                kind,
                parent,
                enabled: x.enabled.unwrap_or(true),
                inputs,
                outputs,
                variables,
                event_links,
                members,
                event: x.event.as_ref().map(|e| EventDef {
                    originator: e.originator.clone().filter(|o| !o.is_empty()),
                    max_trigger_count: e.max_trigger_count,
                    retrigger_delay: finite_or_zero(e.retrigger_delay),
                    player_only: e.player_only,
                }),
                var: x.var.as_ref().map(|v| VarDef {
                    var_name: v.var_name.clone(),
                    value: KValue::from_json(&v.value),
                }),
                params,
                auto_activate_outputs: x.auto_activate_outputs,
                latent: x.latent,
                latent_base: x.latent_base,
                level: 0,
            });
        }
        // Parent cycles cannot occur through `parent` validity alone; break
        // any by checking the chain length.
        for id in 0..nodes.len() {
            let mut cur = nodes.get(id).and_then(|x| x.parent);
            let mut steps = 0usize;
            while let Some(p) = cur {
                steps += 1;
                if steps > 64 || p == id {
                    warn(&mut warnings, format!("node {id}: parent chain too deep"));
                    if let Some(x) = nodes.get_mut(id) {
                        x.parent = None;
                    }
                    break;
                }
                cur = nodes.get(p).and_then(|x| x.parent);
            }
        }
        let roots = nodes
            .iter()
            .filter(|x| x.kind == NodeKind::Sequence && x.parent.is_none())
            .map(|x| x.id)
            .collect();
        let mut node_by_path = BTreeMap::new();
        for x in &nodes {
            if !x.path.is_empty() {
                node_by_path
                    .entry(x.path.to_ascii_lowercase())
                    .or_insert(x.id);
            }
        }
        Ok(Graph {
            levels: vec![GraphLevel {
                package: file.package,
                roots,
            }],
            nodes,
            actors,
            actor_by_path,
            node_by_path,
            sounds: file
                .sounds
                .into_iter()
                .map(|(k, v)| (k.to_ascii_lowercase(), v))
                .collect(),
            probes: file.probes,
            warnings,
        })
    }

    /// Merges streamed sub-level graphs into a persistent-level graph.
    /// Node ids and actor references of each sub-level are offset; its roots
    /// form a new [`GraphLevel`] entry.
    #[must_use]
    pub fn merge(mut self, subs: Vec<Graph>) -> Graph {
        for sub in subs {
            let node_off = self.nodes.len();
            let actor_off = u32::try_from(self.actors.len()).unwrap_or(u32::MAX);
            if node_off.saturating_add(sub.nodes.len()) > MAX_NODES {
                warn(
                    &mut self.warnings,
                    format!("sub-level {} skipped: too many nodes", sub.package()),
                );
                continue;
            }
            let level = self.levels.len();
            let shift = |i: usize| i + node_off;
            for mut x in sub.nodes {
                x.id = shift(x.id);
                x.parent = x.parent.map(shift);
                x.level = level;
                for o in &mut x.outputs {
                    for l in &mut o.links {
                        l.0 = shift(l.0);
                    }
                }
                for v in &mut x.variables {
                    for t in &mut v.vars {
                        *t = shift(*t);
                    }
                }
                for e in &mut x.event_links {
                    for t in &mut e.events {
                        *t = shift(*t);
                    }
                }
                for m in &mut x.members {
                    *m = shift(*m);
                }
                if !x.path.is_empty() {
                    self.node_by_path
                        .entry(x.path.to_ascii_lowercase())
                        .or_insert(x.id);
                }
                self.nodes.push(x);
            }
            for mut a in sub.actors {
                a.base = a.base.map(|b| ActorRef(b.0.saturating_add(actor_off)));
                let key = a.path.to_ascii_lowercase();
                let idx = ActorRef(u32::try_from(self.actors.len()).unwrap_or(u32::MAX));
                self.actor_by_path.entry(key).or_insert(idx);
                self.actors.push(a);
            }
            for (k, v) in sub.sounds {
                self.sounds.entry(k).or_insert(v);
            }
            for mut p in sub.probes {
                p.action = shift(p.action);
                self.probes.push(p);
            }
            for w in sub.warnings {
                warn(&mut self.warnings, w);
            }
            self.levels.push(GraphLevel {
                package: sub
                    .levels
                    .first()
                    .map(|l| l.package.clone())
                    .unwrap_or_default(),
                roots: sub
                    .levels
                    .first()
                    .map(|l| l.roots.iter().map(|r| shift(*r)).collect())
                    .unwrap_or_default(),
            });
        }
        self
    }

    /// Package of the persistent level.
    #[must_use]
    pub fn package(&self) -> &str {
        self.levels.first().map_or("", |l| l.package.as_str())
    }

    /// The node `id`.
    #[must_use]
    pub fn node(&self, id: usize) -> Option<&Node> {
        self.nodes.get(id)
    }

    /// The actor `r`.
    #[must_use]
    pub fn actor(&self, r: ActorRef) -> Option<&ActorInfo> {
        self.actors.get(r.0 as usize)
    }

    /// The actor with object path `path` (case-insensitive).
    #[must_use]
    pub fn actor_by_path(&self, path: &str) -> Option<ActorRef> {
        self.actor_by_path.get(&path.to_ascii_lowercase()).copied()
    }

    /// The node with object path `path` (case-insensitive).
    #[must_use]
    pub fn node_by_path(&self, path: &str) -> Option<usize> {
        self.node_by_path.get(&path.to_ascii_lowercase()).copied()
    }

    /// Sound durations of cue `path`.
    #[must_use]
    pub fn sound(&self, path: &str) -> Option<SoundJson> {
        self.sounds.get(&path.to_ascii_lowercase()).copied()
    }

    /// Index of the level named `package` (case-insensitive).
    #[must_use]
    pub fn level_index(&self, package: &str) -> Option<usize> {
        self.levels
            .iter()
            .position(|l| l.package.eq_ignore_ascii_case(package))
    }

    /// Every node of `class` under the roots `roots`, depth first in
    /// `SequenceObjects` order (`USequence::FindSeqObjectsByClass` with
    /// recursion).
    #[must_use]
    pub fn find_by_class(&self, roots: &[usize], class: OpClass) -> Vec<usize> {
        let mut out = Vec::new();
        let mut visited = vec![false; self.nodes.len()];
        for r in roots {
            self.collect(*r, class, &mut out, &mut visited, 0);
        }
        out
    }

    fn collect(
        &self,
        seq: usize,
        class: OpClass,
        out: &mut Vec<usize>,
        visited: &mut [bool],
        depth: usize,
    ) {
        if depth > 64 {
            return;
        }
        match visited.get_mut(seq) {
            Some(v) if !*v => *v = true,
            _ => return,
        }
        let Some(s) = self.nodes.get(seq) else {
            return;
        };
        for m in &s.members {
            let Some(x) = self.nodes.get(*m) else {
                continue;
            };
            if x.class == class {
                out.push(*m);
            }
            if x.kind == NodeKind::Sequence {
                self.collect(*m, class, out, visited, depth + 1);
            }
        }
    }
}

fn finite_or_zero(x: f32) -> f32 {
    if x.is_finite() { x } else { 0.0 }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn file(nodes: serde_json::Value) -> Vec<u8> {
        serde_json::to_vec(&json!({
            "format": RUNTIME_FORMAT, "version": RUNTIME_VERSION, "package": "T",
            "nodes": nodes,
            "actors": [{"path": "T.TheWorld.PersistentLevel.A", "name": "A", "class": "Engine.Trigger",
                        "kind": "trigger", "package": "T", "slot": 3, "base": "T.TheWorld.PersistentLevel.A"}]
        }))
        .unwrap()
    }

    #[test]
    fn validates_ids_links_and_formats() {
        let g = Graph::from_json_slice(&file(json!([
            {"id": 0, "class": "Engine.Sequence", "kind": "sequence", "members": [1, 2, 99]},
            {"id": 1, "class": "Engine.SeqAct_Toggle", "kind": "action", "parent": 0,
             "inputs": [{"desc": "Turn On"}], "outputs": [{"desc": "Out", "links": [{"op": 2, "input": 0}, {"op": 2, "input": 5}, {"op": 7}]}],
             "variables": [{"desc": "Bool", "property": "None", "vars": [2, 3]}]},
            {"id": 2, "class": "Engine.SeqVar_Bool", "kind": "variable", "parent": 77},
            {"id": 3, "class": "Engine.SeqVar_Bool", "kind": "variable", "parent": 0}
        ])))
        .unwrap();
        assert_eq!(g.levels[0].roots, vec![0]);
        assert_eq!(g.nodes[0].members, vec![1, 2]);
        // Variables are not ops: all three links are dropped.
        assert!(g.nodes[1].outputs[0].links.is_empty());
        assert_eq!(g.nodes[1].variables[0].property, None);
        assert_eq!(g.nodes[1].variables[0].vars, vec![2, 3]);
        assert_eq!(g.nodes[2].parent, None);
        assert!(!g.warnings.is_empty());
        assert_eq!(g.nodes[1].class, OpClass::Toggle);
        let a = g.actor_by_path("t.theworld.persistentlevel.a").unwrap();
        assert_eq!(g.actor(a).unwrap().slot, 3);
        assert_eq!(g.actor(a).unwrap().base, Some(a));
        assert_eq!(g.find_by_class(&[0], OpClass::Toggle), vec![1]);
        // Wrong ids, wrong format.
        assert!(matches!(
            Graph::from_json_slice(&file(json!([{"id": 1}]))),
            Err(GraphError::Nodes(_))
        ));
        let bad = serde_json::to_vec(&json!({"format": "x", "version": 1})).unwrap();
        assert!(matches!(
            Graph::from_json_slice(&bad),
            Err(GraphError::Format { .. })
        ));
        assert!(matches!(
            Graph::from_json_slice(b"{"),
            Err(GraphError::Json(_))
        ));
    }

    #[test]
    fn merge_offsets_sub_levels() {
        let a = Graph::from_json_slice(&file(json!([
            {"id": 0, "path": "T.Main", "class": "Engine.Sequence", "kind": "sequence", "members": [1]},
            {"id": 1, "class": "Engine.SeqAct_Delay", "kind": "action", "parent": 0, "inputs": [{"desc": "Start"}]}
        ])))
        .unwrap();
        let b = Graph::from_json_slice(&file(json!([
            {"id": 0, "path": "S.Main", "class": "Engine.Sequence", "kind": "sequence", "members": [1]},
            {"id": 1, "class": "Engine.SeqAct_Delay", "kind": "action", "parent": 0,
             "inputs": [{"desc": "Start"}], "outputs": [{"desc": "Finished", "links": [{"op": 1}]}]}
        ])))
        .unwrap();
        let m = a.merge(vec![b]);
        assert_eq!(m.levels.len(), 2);
        assert_eq!(m.levels[1].roots, vec![2]);
        assert_eq!(m.nodes[3].parent, Some(2));
        assert_eq!(m.nodes[3].outputs[0].links, vec![(3, 0)]);
        assert_eq!(m.nodes[3].level, 1);
        assert_eq!(m.node_by_path("s.main"), Some(2));
    }
}
