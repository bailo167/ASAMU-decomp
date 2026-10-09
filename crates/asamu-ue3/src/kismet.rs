//! Kismet (UE3 sequence) graph recovery for map packages.
//!
//! A map's visual scripting lives in exports whose class derives from
//! `Engine.SequenceObject`: sequences (`Sequence`, `PrefabSequence`,
//! `PrefabSequenceContainer`), operations (`SequenceEvent`, `SequenceAction`,
//! `SequenceCondition`, all `SequenceOp`s), variables (`SequenceVariable`,
//! including `InterpData`) and comment frames (`SequenceFrame`). Every link is
//! an ordinary tagged property (see `docs/reverse-engineering/KISMET.md`):
//!
//! ```text
//! SequenceOp.InputLinks[]    SeqOpInputLink    { LinkDesc, ActivateDelay, bDisabled, LinkedOp }
//! SequenceOp.OutputLinks[]   SeqOpOutputLink   { Links[] { LinkedOp, InputLinkIdx }, LinkDesc,
//!                                                ActivateDelay, bDisabled, LinkedOp }
//! SequenceOp.VariableLinks[] SeqVarLink        { ExpectedType, LinkedVariables[], LinkDesc,
//!                                                LinkVar, PropertyName, bWriteable, MinVars, MaxVars }
//! SequenceOp.EventLinks[]    SeqEventLink      { ExpectedType, LinkedEvents[], LinkDesc }
//! SequenceObject.ParentSequence, Sequence.SequenceObjects[]
//! ```
//!
//! [`build_graph`] turns those properties into a [`KismetGraph`]: one node per
//! Kismet export and typed edges (activation, variable, Matinee, event,
//! sub-sequence boundary, plus derived remote-event and named-variable edges).
//! Values a cooked object does not store are taken from its archetype (prefab
//! instances, with archetype-internal references remapped onto the instance)
//! or from its class defaults. Links that do not resolve to a node of the
//! right kind are reported as [`DanglingLink`]s instead of being dropped.
//!
//! The full graph is original game data (object names, comments, actor
//! references): keep exports local. [`KismetGraph::summary`] reduces a graph
//! to counts, class names and map names for publication.
//!
//! Hostile-input discipline: every chain walk is bounded, derived edges are
//! capped, and malformed objects become warnings, never panics.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};
use std::fmt::Write as _;
use std::sync::Arc;

use serde::Serialize;

use crate::model::PackageSet;
use crate::object::{decode_object, export_class_path, in_class_default_object, qualified_path};
use crate::package::{MAX_OUTER_DEPTH, Package};
use crate::property::{ObjRef, Property, Value};
use crate::schema::{PropertyType, Schema, last_component};
use crate::types::PackageIndex;

/// `format` field of the JSON graph export.
pub const GRAPH_FORMAT: &str = "asamu-kismet-graph";
/// `version` field of the JSON graph export (bumped on incompatible changes).
pub const GRAPH_VERSION: u32 = 1;
/// Deepest sequence nesting followed through `ParentSequence`.
pub const MAX_SEQUENCE_DEPTH: usize = 64;
/// Longest archetype chain followed.
pub const MAX_ARCHETYPE_DEPTH: usize = 16;
/// Most derived (remote-event and named-variable) edges kept per graph.
pub const MAX_DERIVED_EDGES: usize = 1 << 20;
/// Most nodes visited when tracing a milestone back to its triggers.
pub const MAX_TRACE_NODES: usize = 4096;
/// Most incoming edges examined when tracing a milestone back to its triggers.
pub const MAX_TRACE_STEPS: usize = 1 << 16;
/// Most warnings kept per graph (the rest are counted).
pub const MAX_GRAPH_WARNINGS: usize = 512;

// ------------------------------------------------------------------ model

/// Role of a Kismet object, from its class hierarchy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum NodeKind {
    /// `Sequence` and subclasses (sub-sequences, prefab sequences).
    Sequence,
    /// `SequenceEvent`.
    Event,
    /// `SequenceAction`.
    Action,
    /// `SequenceCondition`.
    Condition,
    /// `SequenceVariable` (including `InterpData`).
    Variable,
    /// `SequenceFrame` (editor comment box).
    Frame,
    /// Another `SequenceOp`.
    Op,
    /// Another `SequenceObject`.
    Object,
}

impl NodeKind {
    /// Lower-case name.
    pub fn name(self) -> &'static str {
        match self {
            NodeKind::Sequence => "sequence",
            NodeKind::Event => "event",
            NodeKind::Action => "action",
            NodeKind::Condition => "condition",
            NodeKind::Variable => "variable",
            NodeKind::Frame => "frame",
            NodeKind::Op => "op",
            NodeKind::Object => "object",
        }
    }

    /// True for kinds that carry input/output/variable/event links.
    pub fn is_op(self) -> bool {
        matches!(
            self,
            NodeKind::Sequence
                | NodeKind::Event
                | NodeKind::Action
                | NodeKind::Condition
                | NodeKind::Op
        )
    }
}

/// Classify a class chain (lower-case short class names, nearest first, as
/// returned by [`Schema::class_chain`]). `None` unless the chain contains
/// `SequenceObject`.
pub fn classify_chain(chain: &[String]) -> Option<NodeKind> {
    if !chain.iter().any(|c| c == "sequenceobject") {
        return None;
    }
    for c in chain {
        let kind = match c.as_str() {
            "sequenceframe" => NodeKind::Frame,
            "sequence" | "prefabsequence" | "prefabsequencecontainer" => NodeKind::Sequence,
            "sequenceevent" => NodeKind::Event,
            "sequenceaction" => NodeKind::Action,
            "sequencecondition" => NodeKind::Condition,
            "sequencevariable" => NodeKind::Variable,
            "sequenceop" => NodeKind::Op,
            "sequenceobject" => NodeKind::Object,
            _ => continue,
        };
        return Some(kind);
    }
    Some(NodeKind::Object)
}

/// Kind of a sequence node.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SequenceKind {
    /// A sequence without a parent (a level's `Main_Sequence`).
    Root,
    /// A nested sequence.
    Sub,
    /// `PrefabSequenceContainer` (holds a level's prefab instance sequences).
    PrefabContainer,
    /// `PrefabSequence` inside a level's sequence tree.
    PrefabInstance,
    /// `PrefabSequence` outside any level (the prefab's own archetype).
    PrefabArchetype,
}

/// Where a node lives.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum NodeScope {
    /// In a sequence tree rooted in a `Level` (executed with the map).
    Level,
    /// In a sequence tree rooted in a `Prefab` (an archetype, not executed).
    Prefab,
    /// Neither (orphaned or unresolvable parent chain).
    Detached,
}

/// Which layer a value came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Origin {
    /// Stored on the object itself.
    Own,
    /// Inherited from the object's archetype (references remapped).
    Archetype,
    /// Inherited from the class defaults.
    ClassDefault,
    /// Not stored anywhere: the type's zero value.
    Zero,
}

/// One input link of an op.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct InputPort {
    /// Label.
    pub desc: String,
    /// `ActivateDelay` in seconds.
    pub activate_delay: f32,
    /// `bDisabled`.
    pub disabled: bool,
}

/// One output link of an op.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct OutputPort {
    /// Label.
    pub desc: String,
    /// `ActivateDelay` in seconds.
    pub activate_delay: f32,
    /// `bDisabled`.
    pub disabled: bool,
    /// Number of stored `Links` entries.
    pub links: usize,
}

/// One variable link of an op.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct VariablePort {
    /// Label.
    pub desc: String,
    /// `ExpectedType` class path.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expected_type: Option<String>,
    /// `PropertyName`: the op property the linked variables feed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub property_name: Option<String>,
    /// `LinkVar` (sub-sequence external variable name).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub link_var: Option<String>,
    /// `bWriteable`.
    pub writeable: bool,
    /// `MinVars`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub min_vars: Option<i32>,
    /// `MaxVars`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_vars: Option<i32>,
    /// Number of stored `LinkedVariables` entries.
    pub links: usize,
}

/// One event link of an op.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct EventPort {
    /// Label.
    pub desc: String,
    /// `ExpectedType` class path.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expected_type: Option<String>,
    /// Number of stored `LinkedEvents` entries.
    pub links: usize,
}

/// Event-specific values (`SequenceEvent`).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct EventInfo {
    /// `Originator` actor path.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub originator: Option<String>,
    /// Class name of the originator (when it is an export of this package).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub originator_class: Option<String>,
    /// `MaxTriggerCount` (0 = unlimited).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_trigger_count: Option<i32>,
    /// `ReTriggerDelay` in seconds.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retrigger_delay: Option<f32>,
    /// `bPlayerOnly`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub player_only: Option<bool>,
    /// `bClientSideOnly`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub client_side_only: Option<bool>,
    /// `Priority`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub priority: Option<i32>,
}

/// Variable-specific values (`SequenceVariable`).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct VariableInfo {
    /// `VarName` (named variables are found by it).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub var_name: Option<String>,
    /// Property holding the value (`ObjValue`, `IntValue`, ...).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value_property: Option<String>,
    /// The value.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<Value>,
    /// Where the value came from.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value_origin: Option<Origin>,
    /// `SeqVar_Named.FindVarName`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub find_var_name: Option<String>,
    /// `SeqVar_External.VariableLabel`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

/// A class-specific property value of a node.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Param {
    /// Property name.
    pub name: String,
    /// Static array index.
    pub array_index: i32,
    /// Value.
    pub value: Value,
    /// Where it came from.
    pub origin: Origin,
}

/// One Kismet object.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct KismetNode {
    /// Node id (index into [`KismetGraph::nodes`]; nodes are in export order).
    pub id: usize,
    /// Export index (0-based).
    pub export_index: usize,
    /// Qualified object path.
    pub path: String,
    /// Object name.
    pub name: String,
    /// Qualified class path.
    pub class: String,
    /// Role.
    pub kind: NodeKind,
    /// True when the class comes from the game's own `asamu` script package.
    pub custom: bool,
    /// Where the node lives.
    pub scope: NodeScope,
    /// Parent sequence node.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent: Option<usize>,
    /// Archetype path (prefab instances).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub archetype: Option<String>,
    /// `ObjName` (editor label).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub obj_name: Option<String>,
    /// `ObjComment` (editor comment).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub comment: Option<String>,
    /// `bEnabled` (events and sequences).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    /// Input links.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub inputs: Vec<InputPort>,
    /// Output links.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub outputs: Vec<OutputPort>,
    /// Variable links.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub variables: Vec<VariablePort>,
    /// Event links.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub events: Vec<EventPort>,
    /// Event values.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub event: Option<EventInfo>,
    /// Variable values.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub variable: Option<VariableInfo>,
    /// Class-specific property values (declared below the generic Kismet
    /// base classes), effective after archetype and class-default merging.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub params: Vec<Param>,
    /// Decoder notes for this object.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}

impl KismetNode {
    /// Short class name (`SeqAct_Interp`).
    pub fn class_name(&self) -> &str {
        last_component(&self.class)
    }

    /// The effective value of class-specific property `name` (index 0).
    pub fn param(&self, name: &str) -> Option<&Param> {
        self.params
            .iter()
            .find(|p| p.array_index == 0 && p.name.eq_ignore_ascii_case(name))
    }
}

/// Sequence node details.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SequenceInfo {
    /// The sequence's node id.
    pub node: usize,
    /// Kind.
    pub kind: SequenceKind,
    /// Nesting depth (0 = root).
    pub depth: usize,
    /// Member node ids (from `SequenceObjects`, in stored order).
    pub members: Vec<usize>,
}

/// Edge type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EdgeKind {
    /// Activation: `OutputLinks[from_port].Links[]` → `InputLinks[to_port]`.
    Output,
    /// `VariableLinks[from_port].LinkedVariables[]` → variable.
    Variable,
    /// A `SeqAct_Interp` variable link to its `InterpData`.
    Matinee,
    /// `EventLinks[from_port].LinkedEvents[]` → event.
    Event,
    /// `Sequence.InputLinks[from_port].LinkedOp` → inner op.
    SubsequenceInput,
    /// `Sequence.OutputLinks[from_port].LinkedOp` → inner op.
    SubsequenceOutput,
    /// Derived: `SeqAct_ActivateRemoteEvent` → `SeqEvent_RemoteEvent` with the
    /// same `EventName` in the same tree.
    RemoteEvent,
    /// Derived: `SeqVar_Named` → variable whose `VarName` is its `FindVarName`
    /// in the same tree.
    NamedVariable,
}

impl EdgeKind {
    /// Lower-case name.
    pub fn name(self) -> &'static str {
        match self {
            EdgeKind::Output => "output",
            EdgeKind::Variable => "variable",
            EdgeKind::Matinee => "matinee",
            EdgeKind::Event => "event",
            EdgeKind::SubsequenceInput => "subsequence_input",
            EdgeKind::SubsequenceOutput => "subsequence_output",
            EdgeKind::RemoteEvent => "remote_event",
            EdgeKind::NamedVariable => "named_variable",
        }
    }
}

/// A typed edge between two nodes.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct KismetEdge {
    /// Type.
    pub kind: EdgeKind,
    /// Source node.
    pub from: usize,
    /// Source port index (output/variable/event link index).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub from_port: Option<usize>,
    /// Target node.
    pub to: usize,
    /// Target port (input link index for activation edges).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub to_port: Option<usize>,
    /// Output-link `ActivateDelay` when non-zero.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub delay: Option<f32>,
    /// True for edges inferred by name matching rather than stored.
    pub derived: bool,
    /// True when source and target have different parent sequences.
    pub cross_sequence: bool,
}

/// Why a stored link does not resolve.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DanglingReason {
    /// The reference is null.
    NullTarget,
    /// The reference is an import (an object in another package).
    Import,
    /// The reference is an export that is not a Kismet object.
    NotKismet,
    /// The target is a Kismet object of the wrong kind for the field.
    WrongKind,
    /// `InputLinkIdx` is outside the target's input links.
    InputIndexOutOfRange,
    /// The target lives in a different scope (e.g. a level op linking into a
    /// prefab archetype).
    OtherScope,
    /// The reference could not be resolved at all.
    BadIndex,
}

/// A stored link that does not resolve to a node of the right kind.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DanglingLink {
    /// Source node.
    pub from: usize,
    /// Field (`OutputLinks[0].Links[1]`, `SequenceObjects[4]`, ...).
    pub field: String,
    /// Target path as stored.
    pub target: String,
    /// Reason.
    pub reason: DanglingReason,
}

/// A name-matched link with no match.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct UnresolvedLink {
    /// Source node.
    pub from: usize,
    /// The edge kind that would have been created.
    pub kind: EdgeKind,
    /// The name looked up (`EventName` / `FindVarName`).
    pub name: String,
}

/// Construction statistics.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct GraphStats {
    /// Kismet exports found.
    pub kismet_exports: usize,
    /// Exports whose payload failed to decode (kept as nodes without values).
    pub decode_failures: usize,
    /// Decoder warnings across all nodes.
    pub decode_warnings: usize,
    /// Port arrays taken from an archetype rather than stored on the object.
    pub ports_from_archetype: usize,
    /// Nodes whose `ParentSequence` disagrees with the sequence listing them.
    pub parent_mismatches: usize,
    /// Nodes with a parent sequence that does not list them in `SequenceObjects`.
    pub unlisted_members: usize,
    /// Classes named like Kismet classes whose hierarchy could not be resolved.
    pub unresolved_classes: usize,
}

/// A map's Kismet graph.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct KismetGraph {
    /// Always [`GRAPH_FORMAT`].
    pub format: String,
    /// Always [`GRAPH_VERSION`].
    pub version: u32,
    /// Package name (file stem).
    pub package: String,
    /// Sequence nodes.
    pub sequences: Vec<SequenceInfo>,
    /// Nodes, in export order.
    pub nodes: Vec<KismetNode>,
    /// Edges, stored ones first (in node/port order), then derived ones.
    pub edges: Vec<KismetEdge>,
    /// Stored links that do not resolve.
    pub dangling: Vec<DanglingLink>,
    /// Name-matched links without a match.
    pub unresolved: Vec<UnresolvedLink>,
    /// Statistics.
    pub stats: GraphStats,
    /// Notes.
    pub warnings: Vec<String>,
}

// ------------------------------------------------------------------ defaults

/// Source of merged class defaults (root-first, child overrides parent).
pub trait ClassDefaults {
    /// Default values of `class_path`; empty when unknown.
    fn class_defaults(&self, class_path: &str) -> Vec<Property>;
}

/// No class defaults (every unstored value is the type's zero).
#[derive(Debug, Clone, Copy, Default)]
pub struct NoClassDefaults;

impl ClassDefaults for NoClassDefaults {
    fn class_defaults(&self, _class_path: &str) -> Vec<Property> {
        Vec::new()
    }
}

impl ClassDefaults for PackageSet {
    fn class_defaults(&self, class_path: &str) -> Vec<Property> {
        match self.inherited_defaults(class_path) {
            Ok(d) => d
                .values
                .into_iter()
                .map(|v| Property {
                    name: v.name,
                    type_name: v.type_name,
                    array_index: v.array_index,
                    size: 0,
                    struct_name: None,
                    enum_name: None,
                    value: v.value,
                    offset: 0,
                })
                .collect(),
            Err(_) => Vec::new(),
        }
    }
}

/// Build the graph of a package opened in `set`.
pub fn build_graph_for(set: &PackageSet, lp: &crate::model::LoadedPackage) -> KismetGraph {
    build_graph(&lp.package, &lp.name, set, set)
}

// ------------------------------------------------------------------ values

fn field<'a>(v: &'a Value, name: &str) -> Option<&'a Value> {
    match v {
        Value::Struct { fields, .. } => fields
            .iter()
            .find(|f| f.array_index == 0 && f.name.eq_ignore_ascii_case(name))
            .map(|f| &f.value),
        _ => None,
    }
}

fn as_int(v: Option<&Value>) -> Option<i32> {
    match v? {
        Value::Int(i) => Some(*i),
        Value::Byte(b) => Some(i32::from(*b)),
        _ => None,
    }
}

fn as_float(v: Option<&Value>) -> Option<f32> {
    match v? {
        Value::Float(f) => Some(*f),
        _ => None,
    }
}

fn as_bool(v: Option<&Value>) -> Option<bool> {
    match v? {
        Value::Bool(b) => Some(*b),
        Value::Int(i) => Some(*i != 0),
        Value::Byte(b) => Some(*b != 0),
        _ => None,
    }
}

fn as_str(v: Option<&Value>) -> Option<&str> {
    match v? {
        Value::Str(s) => Some(s),
        _ => None,
    }
}

fn as_name(v: Option<&Value>) -> Option<&str> {
    match v? {
        Value::Name(s) | Value::Enum(s) if !s.eq_ignore_ascii_case("None") => Some(s),
        _ => None,
    }
}

fn as_obj(v: Option<&Value>) -> Option<&ObjRef> {
    match v? {
        Value::Object(o) | Value::Interface(o) => Some(o),
        _ => None,
    }
}

fn items(v: &Value) -> &[Value] {
    match v {
        Value::Array(items) => items,
        _ => &[],
    }
}

fn zero_of(ty: &PropertyType) -> Option<Value> {
    Some(match ty {
        PropertyType::Int => Value::Int(0),
        PropertyType::Float => Value::Float(0.0),
        PropertyType::Bool => Value::Bool(false),
        PropertyType::Str => Value::Str(String::new()),
        PropertyType::Name => Value::Name("None".to_owned()),
        PropertyType::Byte { enum_path: None } => Value::Byte(0),
        PropertyType::Object { .. } | PropertyType::Class { .. } => Value::Object(ObjRef {
            index: 0,
            path: "None".to_owned(),
        }),
        PropertyType::Array { .. } => Value::Array(Vec::new()),
        _ => return None,
    })
}

/// Key of a property for merging: lower-case name and static array index.
fn prop_key(p: &Property) -> (String, i32) {
    (p.name.to_ascii_lowercase(), p.array_index)
}

/// Tagged structs merge member by member; everything else replaces.
/// Linear in the number of members (an index, not a scan per member).
fn merge_into(slot: &mut Value, from: &Value, depth: usize) {
    match (slot, from) {
        (
            Value::Struct {
                binary: false,
                fields: a,
                ..
            },
            Value::Struct {
                binary: false,
                fields: b,
                ..
            },
        ) if depth < crate::property::MAX_VALUE_DEPTH => {
            let mut index: HashMap<(String, i32), usize> = HashMap::with_capacity(a.len());
            for (i, q) in a.iter().enumerate() {
                index.entry(prop_key(q)).or_insert(i);
            }
            for p in b {
                match index.get(&prop_key(p)).copied() {
                    Some(i) => {
                        if let Some(q) = a.get_mut(i) {
                            merge_into(&mut q.value, &p.value, depth + 1);
                        }
                    }
                    None => {
                        index.insert(prop_key(p), a.len());
                        a.push(p.clone());
                    }
                }
            }
        }
        (slot, v) => *slot = v.clone(),
    }
}

/// A merged top-level property.
#[derive(Debug, Clone)]
struct EffProp {
    prop: Property,
    origin: Origin,
}

type Effective = Arc<Vec<EffProp>>;

fn eff_get<'a>(eff: &'a [EffProp], name: &str) -> Option<&'a EffProp> {
    eff.iter()
        .find(|e| e.prop.array_index == 0 && e.prop.name.eq_ignore_ascii_case(name))
}

fn merge_layer(into: &mut Vec<EffProp>, layer: &[Property], origin: Origin) {
    let mut index: HashMap<(String, i32), usize> = HashMap::with_capacity(into.len());
    for (i, e) in into.iter().enumerate() {
        index.entry(prop_key(&e.prop)).or_insert(i);
    }
    for p in layer {
        match index.get(&prop_key(p)).copied() {
            Some(i) => {
                if let Some(e) = into.get_mut(i) {
                    merge_into(&mut e.prop.value, &p.value, 0);
                    e.origin = origin;
                }
            }
            None => {
                index.insert(prop_key(p), into.len());
                into.push(EffProp {
                    prop: p.clone(),
                    origin,
                });
            }
        }
    }
}

// ------------------------------------------------------------------ builder

/// Generic Kismet base classes whose properties are bookkeeping, not params.
const BASE_CLASSES: &[&str] = &[
    "object",
    "sequenceobject",
    "sequenceop",
    "sequenceaction",
    "sequenceevent",
    "sequencecondition",
    "sequencevariable",
    "seqact_latent",
    "sequence",
    "sequenceframe",
    "prefabsequence",
    "prefabsequencecontainer",
];

/// Value property of well-known variable classes (by short class name).
const VARIABLE_VALUES: &[(&str, &str)] = &[
    ("seqvar_object", "ObjValue"),
    ("seqvar_int", "IntValue"),
    ("seqvar_float", "FloatValue"),
    ("seqvar_bool", "bValue"),
    ("seqvar_string", "StrValue"),
    ("seqvar_name", "NameValue"),
    ("seqvar_vector", "VectValue"),
    ("seqvar_objectlist", "ObjList"),
    ("seqvar_player", "PlayerIdx"),
];

#[derive(Debug, Clone)]
enum Target {
    Node(usize),
    Null,
    Import(String),
    NotKismet(String),
    Bad(String),
}

struct Raw {
    export: usize,
    class: String,
    chain: Vec<String>,
    kind: NodeKind,
    own: Vec<Property>,
    warnings: Vec<String>,
    failed: bool,
}

struct Builder<'a> {
    pkg: &'a Package,
    own_name: &'a str,
    schema: &'a dyn Schema,
    defaults: &'a dyn ClassDefaults,
    raw: Vec<Raw>,
    paths: Vec<String>,
    by_export: HashMap<usize, usize>,
    archetype: Vec<Option<usize>>,
    remap_scope: Vec<Option<usize>>,
    remap: HashMap<(usize, usize), usize>,
    class_defaults: HashMap<String, Arc<Vec<Property>>>,
    effective: HashMap<usize, Effective>,
    warnings: Vec<String>,
    dropped_warnings: usize,
    stats: GraphStats,
}

impl Builder<'_> {
    fn warn(&mut self, msg: String) {
        if self.warnings.len() < MAX_GRAPH_WARNINGS {
            self.warnings.push(msg);
        } else {
            self.dropped_warnings = self.dropped_warnings.saturating_add(1);
        }
    }

    fn defaults_of(&mut self, class: &str) -> Arc<Vec<Property>> {
        let key = class.to_ascii_lowercase();
        if let Some(d) = self.class_defaults.get(&key) {
            return d.clone();
        }
        let d = Arc::new(self.defaults.class_defaults(class));
        self.class_defaults.insert(key, d.clone());
        d
    }

    fn node_of_index(&self, idx: i32) -> Option<usize> {
        let export = PackageIndex(idx).export_index()?;
        self.by_export.get(&export).copied()
    }

    /// Rewrite references into an archetype graph onto the instance graph.
    fn remap_value(&self, v: &mut Value, scope: usize, depth: usize) {
        if depth > crate::property::MAX_VALUE_DEPTH {
            return;
        }
        match v {
            Value::Object(o) | Value::Interface(o) => {
                if let Some(arch) = self.node_of_index(o.index)
                    && let Some(&inst) = self.remap.get(&(scope, arch))
                    && let Some(raw) = self.raw.get(inst)
                    && let Some(pi) = PackageIndex::from_export(raw.export)
                {
                    o.index = pi.0;
                    o.path = self.paths.get(inst).cloned().unwrap_or_default();
                }
            }
            Value::Delegate { object, .. } => {
                let mut wrapped = Value::Object(object.clone());
                self.remap_value(&mut wrapped, scope, depth + 1);
                if let Value::Object(o) = wrapped {
                    *object = o;
                }
            }
            Value::Array(items) => {
                for i in items {
                    self.remap_value(i, scope, depth + 1);
                }
            }
            Value::Struct { fields, .. } => {
                for f in fields {
                    self.remap_value(&mut f.value, scope, depth + 1);
                }
            }
            _ => {}
        }
    }

    fn effective_of(&mut self, n: usize, depth: usize) -> Effective {
        if let Some(e) = self.effective.get(&n) {
            return e.clone();
        }
        let Some(raw) = self.raw.get(n) else {
            return Arc::new(Vec::new());
        };
        let class = raw.class.clone();
        let mut out: Vec<EffProp> = Vec::new();
        let defaults = self.defaults_of(&class);
        merge_layer(&mut out, &defaults, Origin::ClassDefault);
        if let Some(a) = self.archetype.get(n).copied().flatten() {
            if depth >= MAX_ARCHETYPE_DEPTH {
                self.warn(format!(
                    "{}: archetype chain longer than {MAX_ARCHETYPE_DEPTH}",
                    self.paths.get(n).cloned().unwrap_or_default()
                ));
            } else {
                let arch = self.effective_of(a, depth + 1);
                let scope = self.remap_scope.get(n).copied().flatten();
                let inherited: Vec<Property> = arch
                    .iter()
                    .filter(|e| matches!(e.origin, Origin::Own | Origin::Archetype))
                    .map(|e| {
                        let mut p = e.prop.clone();
                        if let Some(s) = scope {
                            self.remap_value(&mut p.value, s, 0);
                        }
                        p
                    })
                    .collect();
                merge_layer(&mut out, &inherited, Origin::Archetype);
            }
        }
        if let Some(raw) = self.raw.get(n) {
            merge_layer(&mut out, &raw.own, Origin::Own);
        }
        let e = Arc::new(out);
        self.effective.insert(n, e.clone());
        e
    }

    fn target(&self, r: &ObjRef) -> Target {
        if r.index == 0 {
            return Target::Null;
        }
        if r.index < 0 {
            return Target::Import(r.path.clone());
        }
        let Some(export) = PackageIndex(r.index).export_index() else {
            return Target::Bad(r.path.clone());
        };
        if export >= self.pkg.exports.len() {
            return Target::Bad(r.path.clone());
        }
        match self.by_export.get(&export) {
            Some(&n) => Target::Node(n),
            None => Target::NotKismet(r.path.clone()),
        }
    }

    fn outer_node(&self, n: usize) -> Option<usize> {
        let raw = self.raw.get(n)?;
        let e = self.pkg.exports.get(raw.export)?;
        let o = e.outer_index.export_index()?;
        self.by_export.get(&o).copied()
    }
}

/// Bookkeeping-free class-specific values of a node.
fn params_of(
    schema: &dyn Schema,
    class: &str,
    eff: &[EffProp],
    known: &mut HashMap<String, Arc<Vec<(String, PropertyType)>>>,
) -> Vec<Param> {
    let key = class.to_ascii_lowercase();
    let declared = match known.get(&key) {
        Some(d) => d.clone(),
        None => {
            let mut v = Vec::new();
            let mut seen = HashSet::new();
            for d in schema.property_link(class) {
                let owner = d
                    .path
                    .rsplit_once('.')
                    .map(|(o, _)| last_component(o).to_ascii_lowercase())
                    .unwrap_or_default();
                let lname = d.name.to_ascii_lowercase();
                let base = BASE_CLASSES.contains(&owner.as_str());
                if !base && seen.insert(lname) {
                    v.push((d.name.clone(), d.ty.clone()));
                }
            }
            let d = Arc::new(v);
            known.insert(key, d.clone());
            d
        }
    };
    let mut by_name: HashMap<String, Vec<&EffProp>> = HashMap::new();
    for e in eff {
        by_name
            .entry(e.prop.name.to_ascii_lowercase())
            .or_default()
            .push(e);
    }
    let mut out = Vec::new();
    for (name, ty) in declared.iter() {
        let mut any = false;
        for e in by_name
            .get(&name.to_ascii_lowercase())
            .map(Vec::as_slice)
            .unwrap_or_default()
        {
            any = true;
            out.push(Param {
                name: e.prop.name.clone(),
                array_index: e.prop.array_index,
                value: e.prop.value.clone(),
                origin: e.origin,
            });
        }
        if !any && let Some(z) = zero_of(ty) {
            out.push(Param {
                name: name.clone(),
                array_index: 0,
                value: z,
                origin: Origin::Zero,
            });
        }
    }
    // Stored values the schema does not declare (unknown classes) are kept.
    if declared.is_empty() {
        for e in eff.iter().filter(|e| e.origin != Origin::ClassDefault) {
            if !is_bookkeeping(&e.prop.name) {
                out.push(Param {
                    name: e.prop.name.clone(),
                    array_index: e.prop.array_index,
                    value: e.prop.value.clone(),
                    origin: e.origin,
                });
            }
        }
    }
    out
}

fn is_bookkeeping(name: &str) -> bool {
    const NAMES: &[&str] = &[
        "ObjInstanceVersion",
        "ParentSequence",
        "ObjPosX",
        "ObjPosY",
        "DrawWidth",
        "DrawHeight",
        "MaxWidth",
        "ObjColor",
        "ObjName",
        "ObjComment",
        "InputLinks",
        "OutputLinks",
        "VariableLinks",
        "EventLinks",
        "SequenceObjects",
        "DefaultViewX",
        "DefaultViewY",
        "DefaultViewZoom",
        "bDrawFirst",
        "bDrawLast",
        "bOutputObjCommentToScreen",
        "bSuppressAutoComment",
        "bDeletable",
        "bEnabled",
        "Originator",
        "MaxTriggerCount",
        "ReTriggerDelay",
        "VarName",
    ];
    NAMES.iter().any(|n| n.eq_ignore_ascii_case(name))
}

fn chain_has(chain: &[String], name: &str) -> bool {
    chain.iter().any(|c| c == name)
}

/// Build the Kismet graph of `pkg` (whose own name is `own_name`).
///
/// `schema` supplies class chains and property declarations (a
/// [`PackageSet`] over the cooked folder); `defaults` supplies class default
/// values for properties an object does not store.
pub fn build_graph(
    pkg: &Package,
    own_name: &str,
    schema: &dyn Schema,
    defaults: &dyn ClassDefaults,
) -> KismetGraph {
    let mut b = Builder {
        pkg,
        own_name,
        schema,
        defaults,
        raw: Vec::new(),
        paths: Vec::new(),
        by_export: HashMap::new(),
        archetype: Vec::new(),
        remap_scope: Vec::new(),
        remap: HashMap::new(),
        class_defaults: HashMap::new(),
        effective: HashMap::new(),
        warnings: Vec::new(),
        dropped_warnings: 0,
        stats: GraphStats::default(),
    };
    collect(&mut b);
    assemble(&mut b)
}

/// Find and decode every Kismet export.
fn collect(b: &mut Builder<'_>) {
    let mut chains: HashMap<String, (Vec<String>, Option<NodeKind>)> = HashMap::new();
    let mut unresolved_classes = BTreeSet::new();
    for i in 0..b.pkg.exports.len() {
        let Some(e) = b.pkg.exports.get(i) else {
            continue;
        };
        if e.class_index.is_null() || in_class_default_object(b.pkg, i) {
            continue;
        }
        let class = match export_class_path(b.pkg, Some(b.own_name), i) {
            Ok(c) => c,
            Err(err) => {
                b.warn(format!("export {i}: class unresolvable: {err}"));
                continue;
            }
        };
        let key = class.to_ascii_lowercase();
        let (chain, kind) = match chains.get(&key) {
            Some(v) => v.clone(),
            None => {
                let chain = b.schema.class_chain(&class);
                let kind = classify_chain(&chain);
                let short = last_component(&class).to_ascii_lowercase();
                if kind.is_none()
                    && chain.len() <= 1
                    && (short.starts_with("seq") || short.starts_with("sequence"))
                {
                    unresolved_classes.insert(class.clone());
                }
                chains.insert(key, (chain.clone(), kind));
                (chain, kind)
            }
        };
        let Some(kind) = kind else {
            continue;
        };
        let (own, warnings, failed) = match decode_object(b.pkg, Some(b.own_name), i, b.schema) {
            Ok(d) => (d.properties, d.warnings, false),
            Err(err) => (Vec::new(), vec![format!("decode failed: {err}")], true),
        };
        let path = PackageIndex::from_export(i)
            .and_then(|pi| qualified_path(b.pkg, Some(b.own_name), pi).ok())
            .unwrap_or_else(|| format!("<export {i}>"));
        let id = b.raw.len();
        b.by_export.insert(i, id);
        b.paths.push(path);
        b.raw.push(Raw {
            export: i,
            class,
            chain,
            kind,
            own,
            warnings,
            failed,
        });
    }
    b.stats.kismet_exports = b.raw.len();
    b.stats.decode_failures = b.raw.iter().filter(|r| r.failed).count();
    b.stats.decode_warnings = b.raw.iter().map(|r| r.warnings.len()).sum();
    b.stats.unresolved_classes = unresolved_classes.len();
    for c in unresolved_classes {
        b.warn(format!(
            "class hierarchy of {c} unknown; not treated as Kismet"
        ));
    }

    // Archetypes inside this package that are themselves Kismet nodes.
    b.archetype = (0..b.raw.len())
        .map(|n| {
            let e = b.pkg.exports.get(b.raw.get(n)?.export)?;
            let a = e.archetype_index.export_index()?;
            b.by_export.get(&a).copied()
        })
        .collect();
    // Remap scope: the nearest sequence (self or outer chain) that has an
    // archetype node, i.e. the prefab instance the object belongs to.
    let mut scopes = Vec::with_capacity(b.raw.len());
    for n in 0..b.raw.len() {
        let mut cur = Some(n);
        let mut found = None;
        let mut steps = 0usize;
        while let Some(c) = cur {
            if steps > MAX_OUTER_DEPTH {
                break;
            }
            steps += 1;
            let is_seq = b.raw.get(c).is_some_and(|r| r.kind == NodeKind::Sequence);
            if is_seq && b.archetype.get(c).copied().flatten().is_some() {
                found = Some(c);
                break;
            }
            cur = b.outer_node(c);
        }
        scopes.push(found);
    }
    b.remap_scope = scopes;
    for n in 0..b.raw.len() {
        if let (Some(s), Some(a)) = (
            b.remap_scope.get(n).copied().flatten(),
            b.archetype.get(n).copied().flatten(),
        ) {
            b.remap.entry((s, a)).or_insert(n);
        }
    }
}

struct Ports {
    inputs: Vec<InputPort>,
    outputs: Vec<OutputPort>,
    variables: Vec<VariablePort>,
    events: Vec<EventPort>,
}

fn ports_of(eff: &[EffProp]) -> Ports {
    let arr = |name: &str| {
        eff_get(eff, name)
            .map(|e| items(&e.prop.value))
            .unwrap_or(&[])
    };
    let inputs = arr("InputLinks")
        .iter()
        .map(|v| InputPort {
            desc: as_str(field(v, "LinkDesc")).unwrap_or_default().to_owned(),
            activate_delay: as_float(field(v, "ActivateDelay")).unwrap_or(0.0),
            disabled: as_bool(field(v, "bDisabled")).unwrap_or(false),
        })
        .collect();
    let outputs = arr("OutputLinks")
        .iter()
        .map(|v| OutputPort {
            desc: as_str(field(v, "LinkDesc")).unwrap_or_default().to_owned(),
            activate_delay: as_float(field(v, "ActivateDelay")).unwrap_or(0.0),
            disabled: as_bool(field(v, "bDisabled")).unwrap_or(false),
            links: field(v, "Links").map(|l| items(l).len()).unwrap_or(0),
        })
        .collect();
    let variables = arr("VariableLinks")
        .iter()
        .map(|v| VariablePort {
            desc: as_str(field(v, "LinkDesc")).unwrap_or_default().to_owned(),
            expected_type: as_obj(field(v, "ExpectedType"))
                .filter(|o| o.index != 0)
                .map(|o| o.path.clone()),
            property_name: as_name(field(v, "PropertyName")).map(str::to_owned),
            link_var: as_name(field(v, "LinkVar")).map(str::to_owned),
            writeable: as_bool(field(v, "bWriteable")).unwrap_or(false),
            min_vars: as_int(field(v, "MinVars")),
            max_vars: as_int(field(v, "MaxVars")),
            links: field(v, "LinkedVariables")
                .map(|l| items(l).len())
                .unwrap_or(0),
        })
        .collect();
    let events = arr("EventLinks")
        .iter()
        .map(|v| EventPort {
            desc: as_str(field(v, "LinkDesc")).unwrap_or_default().to_owned(),
            expected_type: as_obj(field(v, "ExpectedType"))
                .filter(|o| o.index != 0)
                .map(|o| o.path.clone()),
            links: field(v, "LinkedEvents")
                .map(|l| items(l).len())
                .unwrap_or(0),
        })
        .collect();
    Ports {
        inputs,
        outputs,
        variables,
        events,
    }
}

#[derive(Default)]
struct LinkSink {
    edges: Vec<KismetEdge>,
    dangling: Vec<DanglingLink>,
}

fn dangling_for(t: &Target) -> Option<(DanglingReason, String)> {
    match t {
        Target::Node(_) => None,
        Target::Null => Some((DanglingReason::NullTarget, "None".to_owned())),
        Target::Import(p) => Some((DanglingReason::Import, p.clone())),
        Target::NotKismet(p) => Some((DanglingReason::NotKismet, p.clone())),
        Target::Bad(p) => Some((DanglingReason::BadIndex, p.clone())),
    }
}

fn assemble(b: &mut Builder<'_>) -> KismetGraph {
    let count = b.raw.len();
    let effs: Vec<Effective> = (0..count).map(|n| b.effective_of(n, 0)).collect();

    // Parents and membership.
    let mut parent: Vec<Option<usize>> = vec![None; count];
    let mut listed_in: Vec<Option<usize>> = vec![None; count];
    let mut members: Vec<Vec<usize>> = vec![Vec::new(); count];
    let mut sink = LinkSink::default();
    for n in 0..count {
        let eff = &effs[n];
        let kind = b.raw[n].kind;
        if let Some(e) = eff_get(eff, "ParentSequence")
            && e.origin != Origin::ClassDefault
            && let Some(r) = as_obj(Some(&e.prop.value))
        {
            match b.target(r) {
                Target::Node(p) if b.raw[p].kind == NodeKind::Sequence => parent[n] = Some(p),
                Target::Null => {}
                Target::Node(_) => sink.dangling.push(DanglingLink {
                    from: n,
                    field: "ParentSequence".to_owned(),
                    target: r.path.clone(),
                    reason: DanglingReason::WrongKind,
                }),
                t => {
                    if let Some((reason, target)) = dangling_for(&t) {
                        sink.dangling.push(DanglingLink {
                            from: n,
                            field: "ParentSequence".to_owned(),
                            target,
                            reason,
                        });
                    }
                }
            }
        }
        if kind == NodeKind::Sequence
            && let Some(e) = eff_get(eff, "SequenceObjects")
            && e.origin != Origin::ClassDefault
        {
            for (k, v) in items(&e.prop.value).iter().enumerate() {
                let Some(r) = as_obj(Some(v)) else {
                    continue;
                };
                match b.target(r) {
                    Target::Node(m) => {
                        members[n].push(m);
                        if listed_in[m].is_none() {
                            listed_in[m] = Some(n);
                        }
                    }
                    t => {
                        if let Some((reason, target)) = dangling_for(&t) {
                            sink.dangling.push(DanglingLink {
                                from: n,
                                field: format!("SequenceObjects[{k}]"),
                                target,
                                reason,
                            });
                        }
                    }
                }
            }
        }
    }
    for n in 0..count {
        match (parent[n], listed_in[n]) {
            (Some(p), Some(l)) if p != l => b.stats.parent_mismatches += 1,
            (Some(_), None) => b.stats.unlisted_members += 1,
            (None, Some(l)) => parent[n] = Some(l),
            (None, None) => {
                if let Some(o) = b.outer_node(n)
                    && b.raw[o].kind == NodeKind::Sequence
                {
                    parent[n] = Some(o);
                }
            }
            _ => {}
        }
    }

    // Roots, depths and scopes.
    let mut root: Vec<Option<usize>> = vec![None; count];
    let mut depth: Vec<usize> = vec![0; count];
    for n in 0..count {
        let mut cur = n;
        let mut d = 0usize;
        let mut ok = true;
        while let Some(p) = parent[cur] {
            if d >= MAX_SEQUENCE_DEPTH || p == n {
                ok = false;
                break;
            }
            cur = p;
            d += 1;
        }
        if ok {
            root[n] = Some(cur);
            depth[n] = d;
        } else {
            let path = b.paths[n].clone();
            b.warn(format!(
                "{path}: parent chain is cyclic or deeper than {MAX_SEQUENCE_DEPTH}"
            ));
        }
    }
    let root_scope = |b: &Builder<'_>, r: usize| -> NodeScope {
        if b.raw[r].kind != NodeKind::Sequence {
            return NodeScope::Detached;
        }
        let Some(e) = b.pkg.exports.get(b.raw[r].export) else {
            return NodeScope::Detached;
        };
        let mut cur = e.outer_index;
        let mut steps = 0usize;
        let mut first = true;
        while let Some(o) = cur.export_index() {
            if steps > MAX_OUTER_DEPTH {
                break;
            }
            steps += 1;
            let class = b.pkg.export_class_name(o).unwrap_or_default();
            if first && class == "Level" {
                return NodeScope::Level;
            }
            if class == "Prefab" {
                return NodeScope::Prefab;
            }
            first = false;
            cur = b
                .pkg
                .exports
                .get(o)
                .map(|x| x.outer_index)
                .unwrap_or_default();
        }
        NodeScope::Detached
    };
    let scope: Vec<NodeScope> = (0..count)
        .map(|n| match root[n] {
            Some(r) => root_scope(b, r),
            None => NodeScope::Detached,
        })
        .collect();

    // Nodes.
    let mut known = HashMap::new();
    let mut nodes = Vec::with_capacity(count);
    for n in 0..count {
        let eff = effs[n].clone();
        let raw = &b.raw[n];
        let ports = if raw.kind.is_op() {
            ports_of(&eff)
        } else {
            Ports {
                inputs: Vec::new(),
                outputs: Vec::new(),
                variables: Vec::new(),
                events: Vec::new(),
            }
        };
        for name in ["InputLinks", "OutputLinks", "VariableLinks", "EventLinks"] {
            if let Some(e) = eff_get(&eff, name)
                && e.origin == Origin::Archetype
            {
                b.stats.ports_from_archetype += 1;
            }
        }
        let get = |name: &str| eff_get(&eff, name).map(|e| &e.prop.value);
        let enabled = matches!(raw.kind, NodeKind::Event | NodeKind::Sequence)
            .then(|| as_bool(get("bEnabled")).unwrap_or(true));
        let event = (raw.kind == NodeKind::Event).then(|| {
            let originator = eff_get(&eff, "Originator")
                .filter(|e| e.origin != Origin::ClassDefault)
                .and_then(|e| as_obj(Some(&e.prop.value)))
                .filter(|o| o.index != 0);
            let originator_class = originator
                .and_then(|o| PackageIndex(o.index).export_index())
                .and_then(|i| b.pkg.export_class_name(i).ok());
            EventInfo {
                originator: originator.map(|o| o.path.clone()),
                originator_class,
                max_trigger_count: as_int(get("MaxTriggerCount")),
                retrigger_delay: as_float(get("ReTriggerDelay")),
                player_only: as_bool(get("bPlayerOnly")),
                client_side_only: as_bool(get("bClientSideOnly")),
                priority: as_int(get("Priority")),
            }
        });
        let variable = (raw.kind == NodeKind::Variable).then(|| {
            let value_property = VARIABLE_VALUES
                .iter()
                .find(|(c, _)| chain_has(&raw.chain, c))
                .map(|(_, p)| (*p).to_owned());
            let (value, value_origin) = match value_property.as_deref() {
                Some(p) => match eff_get(&eff, p) {
                    Some(e) => (Some(e.prop.value.clone()), Some(e.origin)),
                    None => {
                        let zero = b
                            .schema
                            .find_property(&raw.class, p)
                            .and_then(|d| zero_of(&d.ty));
                        let origin = zero.as_ref().map(|_| Origin::Zero);
                        (zero, origin)
                    }
                },
                None => (None, None),
            };
            VariableInfo {
                var_name: as_name(get("VarName")).map(str::to_owned),
                value_property,
                value,
                value_origin,
                find_var_name: as_name(get("FindVarName")).map(str::to_owned),
                label: as_str(get("VariableLabel")).map(str::to_owned),
            }
        });
        let params = if matches!(raw.kind, NodeKind::Frame | NodeKind::Sequence) {
            Vec::new()
        } else {
            params_of(b.schema, &raw.class, &eff, &mut known)
        };
        let export = b.pkg.exports.get(raw.export);
        let archetype = export
            .map(|e| e.archetype_index)
            .filter(|a| !a.is_null())
            .and_then(|a| qualified_path(b.pkg, Some(b.own_name), a).ok());
        let name = export
            .map(|e| b.pkg.fname(e.object_name))
            .unwrap_or_default();
        nodes.push(KismetNode {
            id: n,
            export_index: raw.export,
            path: b.paths[n].clone(),
            name,
            class: raw.class.clone(),
            kind: raw.kind,
            custom: raw
                .class
                .split('.')
                .next()
                .is_some_and(|p| p.eq_ignore_ascii_case("asamu")),
            scope: scope[n],
            parent: parent[n],
            archetype,
            obj_name: as_str(get("ObjName"))
                .filter(|s| !s.is_empty())
                .map(str::to_owned),
            comment: eff_get(&eff, "ObjComment")
                .filter(|e| e.origin != Origin::ClassDefault)
                .and_then(|e| as_str(Some(&e.prop.value)))
                .filter(|s| !s.is_empty())
                .map(str::to_owned),
            enabled,
            inputs: ports.inputs,
            outputs: ports.outputs,
            variables: ports.variables,
            events: ports.events,
            event,
            variable,
            params,
            warnings: raw.warnings.clone(),
        });
    }

    // Stored links.
    for n in 0..count {
        if !nodes[n].kind.is_op() {
            continue;
        }
        let eff = effs[n].clone();
        let resolvable =
            |name: &str| eff_get(&eff, name).filter(|e| e.origin != Origin::ClassDefault);
        let is_interp = chain_has(&b.raw[n].chain, "seqact_interp");
        let is_sequence = nodes[n].kind == NodeKind::Sequence;
        let push_edge = |sink: &mut LinkSink,
                         nodes: &[KismetNode],
                         kind: EdgeKind,
                         port: usize,
                         to: usize,
                         to_port: Option<usize>,
                         delay: Option<f32>| {
            let cross = matches!(
                kind,
                EdgeKind::Output | EdgeKind::Variable | EdgeKind::Matinee | EdgeKind::Event
            ) && nodes[n].parent != nodes[to].parent;
            sink.edges.push(KismetEdge {
                kind,
                from: n,
                from_port: Some(port),
                to,
                to_port,
                delay,
                derived: false,
                cross_sequence: cross,
            });
        };
        let scope_ok = |nodes: &[KismetNode], to: usize| nodes[to].scope == nodes[n].scope;

        if let Some(e) = resolvable("OutputLinks") {
            for (i, out) in items(&e.prop.value).iter().enumerate() {
                let delay = as_float(field(out, "ActivateDelay")).filter(|d| *d != 0.0);
                if let Some(links) = field(out, "Links") {
                    for (j, l) in items(links).iter().enumerate() {
                        let fname = format!("OutputLinks[{i}].Links[{j}]");
                        let Some(r) = as_obj(field(l, "LinkedOp")) else {
                            sink.dangling.push(DanglingLink {
                                from: n,
                                field: fname,
                                target: "None".to_owned(),
                                reason: DanglingReason::NullTarget,
                            });
                            continue;
                        };
                        let t = b.target(r);
                        let Target::Node(to) = t else {
                            if let Some((reason, target)) = dangling_for(&t) {
                                sink.dangling.push(DanglingLink {
                                    from: n,
                                    field: fname,
                                    target,
                                    reason,
                                });
                            }
                            continue;
                        };
                        let idx = as_int(field(l, "InputLinkIdx")).unwrap_or(0);
                        let reason = if !nodes[to].kind.is_op() {
                            Some(DanglingReason::WrongKind)
                        } else if !scope_ok(&nodes, to) {
                            Some(DanglingReason::OtherScope)
                        } else if usize::try_from(idx)
                            .ok()
                            .is_none_or(|k| k >= nodes[to].inputs.len())
                        {
                            Some(DanglingReason::InputIndexOutOfRange)
                        } else {
                            None
                        };
                        if let Some(reason) = reason {
                            sink.dangling.push(DanglingLink {
                                from: n,
                                field: format!("{fname} (InputLinkIdx {idx})"),
                                target: r.path.clone(),
                                reason,
                            });
                            continue;
                        }
                        let to_port = usize::try_from(idx).ok();
                        push_edge(&mut sink, &nodes, EdgeKind::Output, i, to, to_port, delay);
                    }
                }
                if is_sequence
                    && let Some(r) = as_obj(field(out, "LinkedOp")).filter(|r| r.index != 0)
                {
                    match b.target(r) {
                        Target::Node(to) if nodes[to].kind.is_op() => push_edge(
                            &mut sink,
                            &nodes,
                            EdgeKind::SubsequenceOutput,
                            i,
                            to,
                            None,
                            None,
                        ),
                        t => {
                            let (reason, target) = dangling_for(&t)
                                .unwrap_or((DanglingReason::WrongKind, r.path.clone()));
                            sink.dangling.push(DanglingLink {
                                from: n,
                                field: format!("OutputLinks[{i}].LinkedOp"),
                                target,
                                reason,
                            });
                        }
                    }
                }
            }
        }
        if is_sequence && let Some(e) = resolvable("InputLinks") {
            for (i, inp) in items(&e.prop.value).iter().enumerate() {
                if let Some(r) = as_obj(field(inp, "LinkedOp")).filter(|r| r.index != 0) {
                    match b.target(r) {
                        Target::Node(to) if nodes[to].kind.is_op() => push_edge(
                            &mut sink,
                            &nodes,
                            EdgeKind::SubsequenceInput,
                            i,
                            to,
                            None,
                            None,
                        ),
                        t => {
                            let (reason, target) = dangling_for(&t)
                                .unwrap_or((DanglingReason::WrongKind, r.path.clone()));
                            sink.dangling.push(DanglingLink {
                                from: n,
                                field: format!("InputLinks[{i}].LinkedOp"),
                                target,
                                reason,
                            });
                        }
                    }
                }
            }
        }
        if let Some(e) = resolvable("VariableLinks") {
            for (i, vl) in items(&e.prop.value).iter().enumerate() {
                let Some(linked) = field(vl, "LinkedVariables") else {
                    continue;
                };
                for (j, v) in items(linked).iter().enumerate() {
                    let fname = format!("VariableLinks[{i}].LinkedVariables[{j}]");
                    let Some(r) = as_obj(Some(v)) else {
                        continue;
                    };
                    let t = b.target(r);
                    match t {
                        Target::Node(to) if nodes[to].kind == NodeKind::Variable => {
                            if !scope_ok(&nodes, to) {
                                sink.dangling.push(DanglingLink {
                                    from: n,
                                    field: fname,
                                    target: r.path.clone(),
                                    reason: DanglingReason::OtherScope,
                                });
                                continue;
                            }
                            let kind = if is_interp && chain_has(&b.raw[to].chain, "interpdata") {
                                EdgeKind::Matinee
                            } else {
                                EdgeKind::Variable
                            };
                            push_edge(&mut sink, &nodes, kind, i, to, None, None);
                        }
                        Target::Node(_) => sink.dangling.push(DanglingLink {
                            from: n,
                            field: fname,
                            target: r.path.clone(),
                            reason: DanglingReason::WrongKind,
                        }),
                        t => {
                            if let Some((reason, target)) = dangling_for(&t) {
                                sink.dangling.push(DanglingLink {
                                    from: n,
                                    field: fname,
                                    target,
                                    reason,
                                });
                            }
                        }
                    }
                }
            }
        }
        if let Some(e) = resolvable("EventLinks") {
            for (i, el) in items(&e.prop.value).iter().enumerate() {
                let Some(linked) = field(el, "LinkedEvents") else {
                    continue;
                };
                for (j, v) in items(linked).iter().enumerate() {
                    let fname = format!("EventLinks[{i}].LinkedEvents[{j}]");
                    let Some(r) = as_obj(Some(v)) else {
                        continue;
                    };
                    match b.target(r) {
                        Target::Node(to) if nodes[to].kind == NodeKind::Event => {
                            if scope_ok(&nodes, to) {
                                push_edge(&mut sink, &nodes, EdgeKind::Event, i, to, None, None);
                            } else {
                                sink.dangling.push(DanglingLink {
                                    from: n,
                                    field: fname,
                                    target: r.path.clone(),
                                    reason: DanglingReason::OtherScope,
                                });
                            }
                        }
                        Target::Node(_) => sink.dangling.push(DanglingLink {
                            from: n,
                            field: fname,
                            target: r.path.clone(),
                            reason: DanglingReason::WrongKind,
                        }),
                        t => {
                            if let Some((reason, target)) = dangling_for(&t) {
                                sink.dangling.push(DanglingLink {
                                    from: n,
                                    field: fname,
                                    target,
                                    reason,
                                });
                            }
                        }
                    }
                }
            }
        }
    }

    // Derived links, matched by name within one tree (all level-scope nodes
    // form one tree; prefab archetypes and detached nodes group by root).
    let group = |n: usize| -> Option<usize> {
        match scope[n] {
            NodeScope::Level => Some(usize::MAX),
            _ => root[n],
        }
    };
    let mut unresolved = Vec::new();
    let mut remote_events: HashMap<(Option<usize>, String), Vec<usize>> = HashMap::new();
    let mut named_vars: HashMap<(Option<usize>, String), Vec<usize>> = HashMap::new();
    for n in 0..count {
        let chain = &b.raw[n].chain;
        let eff = &effs[n];
        if nodes[n].kind == NodeKind::Event
            && chain_has(chain, "seqevent_remoteevent")
            && let Some(name) = as_name(eff_get(eff, "EventName").map(|e| &e.prop.value))
        {
            remote_events
                .entry((group(n), name.to_ascii_lowercase()))
                .or_default()
                .push(n);
        }
        if nodes[n].kind == NodeKind::Variable
            && !chain_has(chain, "seqvar_named")
            && let Some(v) = nodes[n].variable.as_ref()
            && let Some(name) = v.var_name.as_deref()
        {
            named_vars
                .entry((group(n), name.to_ascii_lowercase()))
                .or_default()
                .push(n);
        }
    }
    let mut derived = 0usize;
    let mut capped = false;
    for n in 0..count {
        let chain = b.raw[n].chain.clone();
        let (kind, name, table) =
            if nodes[n].kind.is_op() && chain_has(&chain, "seqact_activateremoteevent") {
                let name = as_name(eff_get(&effs[n], "EventName").map(|e| &e.prop.value));
                (
                    EdgeKind::RemoteEvent,
                    name.map(str::to_owned),
                    &remote_events,
                )
            } else if nodes[n].kind == NodeKind::Variable && chain_has(&chain, "seqvar_named") {
                let name = nodes[n]
                    .variable
                    .as_ref()
                    .and_then(|v| v.find_var_name.clone());
                (EdgeKind::NamedVariable, name, &named_vars)
            } else {
                continue;
            };
        let Some(name) = name else {
            unresolved.push(UnresolvedLink {
                from: n,
                kind,
                name: "None".to_owned(),
            });
            continue;
        };
        match table.get(&(group(n), name.to_ascii_lowercase())) {
            Some(targets) => {
                for &to in targets {
                    if derived >= MAX_DERIVED_EDGES {
                        capped = true;
                        break;
                    }
                    derived += 1;
                    sink.edges.push(KismetEdge {
                        kind,
                        from: n,
                        from_port: None,
                        to,
                        to_port: None,
                        delay: None,
                        derived: true,
                        cross_sequence: nodes[n].parent != nodes[to].parent,
                    });
                }
            }
            None => unresolved.push(UnresolvedLink {
                from: n,
                kind,
                name,
            }),
        }
    }
    if capped {
        b.warn(format!(
            "more than {MAX_DERIVED_EDGES} derived edges; the rest were dropped"
        ));
    }

    let sequences = (0..count)
        .filter(|&n| nodes[n].kind == NodeKind::Sequence)
        .map(|n| {
            let chain = &b.raw[n].chain;
            let kind = if chain_has(chain, "prefabsequencecontainer") {
                SequenceKind::PrefabContainer
            } else if chain_has(chain, "prefabsequence") {
                if scope[n] == NodeScope::Level {
                    SequenceKind::PrefabInstance
                } else {
                    SequenceKind::PrefabArchetype
                }
            } else if parent[n].is_none() {
                SequenceKind::Root
            } else {
                SequenceKind::Sub
            };
            SequenceInfo {
                node: n,
                kind,
                depth: depth[n],
                members: std::mem::take(&mut members[n]),
            }
        })
        .collect();

    if b.dropped_warnings > 0 {
        let n = b.dropped_warnings;
        b.warnings.push(format!("{n} further warnings not listed"));
    }
    KismetGraph {
        format: GRAPH_FORMAT.to_owned(),
        version: GRAPH_VERSION,
        package: b.own_name.to_owned(),
        sequences,
        nodes,
        edges: sink.edges,
        dangling: sink.dangling,
        unresolved,
        stats: std::mem::take(&mut b.stats),
        warnings: std::mem::take(&mut b.warnings),
    }
}

// ------------------------------------------------------------------ summary

/// Sequence counts by kind.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct SequenceCounts {
    /// Root sequences.
    pub root: usize,
    /// Nested sequences.
    pub sub: usize,
    /// Prefab sequence containers.
    pub prefab_container: usize,
    /// Prefab instance sequences in the level.
    pub prefab_instance: usize,
    /// Prefab archetype sequences (not executed).
    pub prefab_archetype: usize,
    /// Deepest nesting among level sequences.
    pub max_depth: usize,
}

/// Link counts (level scope).
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct LinkCounts {
    /// Edges by kind.
    pub by_kind: BTreeMap<String, usize>,
    /// Stored edges.
    pub stored: usize,
    /// Derived edges.
    pub derived: usize,
    /// Edges between different parent sequences.
    pub cross_sequence: usize,
    /// Activation edges with a non-zero output delay.
    pub delayed: usize,
    /// Dangling links (whole package).
    pub dangling: usize,
    /// Remote-event actions without a matching event.
    pub unresolved_remote_events: usize,
    /// Named variables without a matching variable.
    pub unresolved_named_variables: usize,
}

/// Disabled elements (level scope).
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct DisabledCounts {
    /// Events with `bEnabled` false.
    pub events: usize,
    /// Sequences with `bEnabled` false.
    pub sequences: usize,
    /// Input links with `bDisabled`.
    pub input_ports: usize,
    /// Output links with `bDisabled`.
    pub output_ports: usize,
}

/// Gameplay-relevant usage (level scope, node counts).
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Features {
    /// `SeqAct_TriggerCheckpoint`.
    pub checkpoint_triggers: usize,
    /// `SeqAct_ToggleCheckpointEnable`.
    pub checkpoint_toggles: usize,
    /// `SeqAct_ToggleRestartFromCheckpointOption`.
    pub restart_option_toggles: usize,
    /// `SeqAct_ToggleGrapple`.
    pub grapple_toggles: usize,
    /// `SeqAct_SetMaxGrapples`.
    pub max_grapple_sets: usize,
    /// `SeqAct_ToggleRocketBoots`.
    pub rocket_boot_toggles: usize,
    /// `SeqAct_NarratorLine`.
    pub narrator_lines: usize,
    /// `SeqAct_StartTimeTrial`.
    pub time_trial_starts: usize,
    /// `SeqAct_EndTimeTrial`.
    pub time_trial_ends: usize,
    /// `SeqCond_IsTimeTrial`.
    pub time_trial_checks: usize,
    /// `SeqAct_ToggleStoryMode` / `SeqAct_ToggleSpawnInStoryMode`.
    pub story_mode_toggles: usize,
    /// `SeqAct_UnlockASAMUAchievement`.
    pub achievements: usize,
    /// `SeqAct_SetGameFinished`.
    pub game_finished: usize,
    /// `SeqAct_LevelStreaming` / `SeqAct_MultiLevelStreaming` / `SeqAct_LevelVisibility`.
    pub level_streaming_actions: usize,
    /// `SeqAct_ConsoleCommand`.
    pub console_commands: usize,
    /// `SeqAct_Interp`.
    pub matinee_actions: usize,
    /// `SeqAct_ActivateRemoteEvent`.
    pub remote_event_actions: usize,
    /// `SeqEvent_RemoteEvent`.
    pub remote_event_listeners: usize,
}

/// A trigger found upstream of a milestone.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct Trigger {
    /// Event class (short name).
    pub event: String,
    /// Originator class (short name), when known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub originator_class: Option<String>,
    /// True when reached through a remote event.
    pub via_remote_event: bool,
}

/// A gameplay milestone action with the events that can fire it.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Milestone {
    /// Node id.
    pub node: usize,
    /// Short class name.
    pub class: String,
    /// Object name of the parent sequence.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sequence: Option<String>,
    /// Relevant values (`Enable=true`, `Grapples=3`, console commands, levels).
    pub values: Vec<String>,
    /// Upstream events.
    pub triggers: Vec<Trigger>,
}

/// Publishable reduction of a graph: counts, class names, map names.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct KismetSummary {
    /// Package name.
    pub package: String,
    /// All Kismet objects in the package.
    pub kismet_objects: usize,
    /// Objects in the level's sequence tree.
    pub level_objects: usize,
    /// Objects of prefab archetypes.
    pub prefab_archetype_objects: usize,
    /// Objects outside any tree.
    pub detached_objects: usize,
    /// Level objects by kind.
    pub by_kind: BTreeMap<String, usize>,
    /// Level objects by qualified class.
    pub by_class: BTreeMap<String, usize>,
    /// Level objects of `asamu` classes.
    pub custom_classes: BTreeMap<String, usize>,
    /// Level events by qualified class.
    pub event_classes: BTreeMap<String, usize>,
    /// Sequence counts.
    pub sequences: SequenceCounts,
    /// Link counts.
    pub links: LinkCounts,
    /// Disabled elements.
    pub disabled: DisabledCounts,
    /// Gameplay usage.
    pub features: Features,
    /// Maps opened by console commands (`open <map>`), options stripped.
    pub map_transitions: Vec<String>,
    /// Levels named by Kismet streaming actions.
    pub streamed_levels: Vec<String>,
    /// Milestone actions and their triggers.
    pub milestones: Vec<Milestone>,
}

/// Milestone classes (short names, lower case).
const MILESTONES: &[&str] = &[
    "seqact_togglegrapple",
    "seqact_togglerocketboots",
    "seqact_setmaxgrapples",
    "seqact_togglevisiblegrapple",
    "seqact_triggercheckpoint",
    "seqact_togglecheckpointenable",
    "seqact_starttimetrial",
    "seqact_endtimetrial",
    "seqact_setgamefinished",
    "seqact_togglestorymode",
    "seqact_togglespawninstorymode",
    "seqact_setpawnsize",
    "seqact_playsuitonanimation",
    "seqact_showtitlelogo",
    "seqact_consolecommand",
    "seqact_levelstreaming",
    "seqact_multilevelstreaming",
    "seqact_levelvisibility",
];

/// Commands of a `SeqAct_ConsoleCommand` node.
pub fn console_commands(node: &KismetNode) -> Vec<String> {
    let mut out = Vec::new();
    if let Some(p) = node.param("Commands") {
        for v in items(&p.value) {
            if let Value::Str(s) = v
                && !s.trim().is_empty()
            {
                out.push(s.trim().to_owned());
            }
        }
    }
    if let Some(p) = node.param("Command")
        && let Value::Str(s) = &p.value
        && !s.trim().is_empty()
    {
        out.push(s.trim().to_owned());
    }
    out
}

/// Target map of an `open <map>[?options]` console command.
pub fn open_target(command: &str) -> Option<String> {
    let mut parts = command.split_whitespace();
    let verb = parts.next()?;
    if !verb.eq_ignore_ascii_case("open") && !verb.eq_ignore_ascii_case("travel") {
        return None;
    }
    let target = parts.next()?;
    let map = target.split('?').next().unwrap_or(target).trim();
    (!map.is_empty()).then(|| map.to_owned())
}

/// Level names of a streaming action node.
pub fn streaming_levels(node: &KismetNode) -> Vec<String> {
    let mut out = Vec::new();
    if let Some(p) = node.param("LevelName")
        && let Some(n) = as_name(Some(&p.value))
    {
        out.push(n.to_owned());
    }
    if let Some(p) = node.param("Levels") {
        for v in items(&p.value) {
            if let Some(n) = as_name(field(v, "LevelName")) {
                out.push(n.to_owned());
            }
        }
    }
    out
}

/// Edge indexes for the summary.
#[derive(Default)]
struct Adjacency {
    /// Target node -> (source node, kind) for activation, remote-event and
    /// sub-sequence-input edges.
    incoming: HashMap<usize, Vec<(usize, EdgeKind)>>,
    /// Source node -> (variable port, variable node) for variable edges.
    variables_out: HashMap<usize, Vec<(usize, usize)>>,
    /// (node, input port) pairs driven by some activation edge.
    driven_inputs: HashSet<(usize, usize)>,
}

impl KismetGraph {
    /// Node by id.
    pub fn node(&self, id: usize) -> Option<&KismetNode> {
        self.nodes.get(id)
    }

    /// Edge indexes used by the summary, built once (keeps the summary
    /// linear in the number of edges, whatever the number of milestones).
    fn adjacency(&self) -> Adjacency {
        let mut adj = Adjacency::default();
        for e in &self.edges {
            match e.kind {
                EdgeKind::Output | EdgeKind::RemoteEvent | EdgeKind::SubsequenceInput => {
                    adj.incoming.entry(e.to).or_default().push((e.from, e.kind));
                }
                EdgeKind::Variable => {
                    if let Some(port) = e.from_port {
                        adj.variables_out
                            .entry(e.from)
                            .or_default()
                            .push((port, e.to));
                    }
                }
                _ => {}
            }
            if e.kind == EdgeKind::Output
                && let Some(port) = e.to_port
            {
                adj.driven_inputs.insert((e.to, port));
            }
        }
        adj
    }

    /// Values of the variables linked to `node`'s variable port that feeds
    /// property `prop` (rendered).
    fn linked_values(&self, adj: &Adjacency, node: usize, prop: &str) -> Vec<String> {
        let Some(n) = self.nodes.get(node) else {
            return Vec::new();
        };
        let ports: HashSet<usize> = n
            .variables
            .iter()
            .enumerate()
            .filter(|(_, p)| {
                p.property_name
                    .as_deref()
                    .is_some_and(|x| x.eq_ignore_ascii_case(prop))
                    || p.desc.eq_ignore_ascii_case(prop)
            })
            .map(|(i, _)| i)
            .collect();
        adj.variables_out
            .get(&node)
            .map(Vec::as_slice)
            .unwrap_or_default()
            .iter()
            .filter(|(port, _)| ports.contains(port))
            .filter_map(|(_, to)| self.nodes.get(*to))
            .filter_map(|v| v.variable.as_ref().and_then(|x| x.value.as_ref()))
            .map(short_render)
            .collect()
    }

    /// Labels of `node`'s input links that some activation edge drives.
    fn used_inputs(&self, adj: &Adjacency, node: &KismetNode) -> Vec<String> {
        node.inputs
            .iter()
            .enumerate()
            .filter(|(i, _)| adj.driven_inputs.contains(&(node.id, *i)))
            .map(|(_, p)| format!("input={}", p.desc))
            .collect()
    }

    fn milestone_values(&self, adj: &Adjacency, n: &KismetNode) -> Vec<String> {
        let short = n.class_name().to_ascii_lowercase();
        let mut values = Vec::new();
        match short.as_str() {
            "seqact_consolecommand" => values.extend(console_commands(n)),
            "seqact_levelstreaming" | "seqact_multilevelstreaming" | "seqact_levelvisibility" => {
                for l in streaming_levels(n) {
                    values.push(format!("level={l}"));
                }
            }
            _ => {
                for p in &n.params {
                    if p.array_index != 0 {
                        continue;
                    }
                    let linked = self.linked_values(adj, n.id, &p.name);
                    let rendered = match &p.value {
                        Value::Int(_)
                        | Value::Float(_)
                        | Value::Bool(_)
                        | Value::Byte(_)
                        | Value::Enum(_) => Some(p.value.render()),
                        _ => None,
                    };
                    if !linked.is_empty() {
                        values.push(format!("{}<-{}", p.name, linked.join("|")));
                    } else if let Some(r) = rendered {
                        values.push(format!("{}={r}", p.name));
                    }
                }
            }
        }
        if n.inputs.len() > 1 {
            values.extend(self.used_inputs(adj, n));
        }
        values
    }

    /// Events from which `start` can be activated (walking activation,
    /// sub-sequence and remote-event edges backwards).
    pub fn triggers_of(&self, start: usize) -> Vec<Trigger> {
        self.triggers_with(&self.adjacency(), start)
    }

    fn triggers_with(&self, adj: &Adjacency, start: usize) -> Vec<Trigger> {
        let no_edges: &[(usize, EdgeKind)] = &[];
        let incoming = |n: usize| adj.incoming.get(&n).map(Vec::as_slice).unwrap_or(no_edges);
        let mut out = BTreeSet::new();
        let mut seen = HashSet::new();
        let mut queue = VecDeque::new();
        let mut steps = 0usize;
        queue.push_back((start, false));
        seen.insert(start);
        while let Some((cur, via_remote)) = queue.pop_front() {
            if seen.len() > MAX_TRACE_NODES || steps > MAX_TRACE_STEPS {
                break;
            }
            let Some(node) = self.nodes.get(cur) else {
                continue;
            };
            let edges = incoming(cur);
            steps = steps.saturating_add(edges.len());
            if cur != start && node.kind == NodeKind::Event {
                let mut relayed = false;
                for &(f, kind) in edges {
                    if kind == EdgeKind::RemoteEvent {
                        relayed = true;
                        if seen.insert(f) {
                            queue.push_back((f, true));
                        }
                    }
                }
                if !relayed {
                    out.insert(Trigger {
                        event: node.class_name().to_owned(),
                        originator_class: node
                            .event
                            .as_ref()
                            .and_then(|e| e.originator_class.clone()),
                        via_remote_event: via_remote,
                    });
                }
                continue;
            }
            for &(f, kind) in edges {
                if kind != EdgeKind::RemoteEvent && seen.insert(f) {
                    queue.push_back((f, via_remote));
                }
            }
        }
        out.into_iter().collect()
    }

    /// Publishable summary (level scope unless stated otherwise).
    pub fn summary(&self) -> KismetSummary {
        let level: Vec<&KismetNode> = self
            .nodes
            .iter()
            .filter(|n| n.scope == NodeScope::Level)
            .collect();
        let mut by_kind = BTreeMap::new();
        let mut by_class = BTreeMap::new();
        let mut custom_classes = BTreeMap::new();
        let mut event_classes = BTreeMap::new();
        let mut short_counts: HashMap<String, usize> = HashMap::new();
        let mut disabled = DisabledCounts::default();
        let mut map_transitions = Vec::new();
        let mut streamed_levels = Vec::new();
        for n in &level {
            *by_kind.entry(n.kind.name().to_owned()).or_insert(0) += 1;
            *by_class.entry(n.class.clone()).or_insert(0) += 1;
            if n.custom {
                *custom_classes.entry(n.class.clone()).or_insert(0) += 1;
            }
            if n.kind == NodeKind::Event {
                *event_classes.entry(n.class.clone()).or_insert(0) += 1;
                if n.enabled == Some(false) {
                    disabled.events += 1;
                }
            }
            if n.kind == NodeKind::Sequence && n.enabled == Some(false) {
                disabled.sequences += 1;
            }
            disabled.input_ports += n.inputs.iter().filter(|p| p.disabled).count();
            disabled.output_ports += n.outputs.iter().filter(|p| p.disabled).count();
            *short_counts
                .entry(n.class_name().to_ascii_lowercase())
                .or_insert(0) += 1;
            let short = n.class_name().to_ascii_lowercase();
            if short == "seqact_consolecommand" {
                for c in console_commands(n) {
                    if let Some(t) = open_target(&c)
                        && !map_transitions.contains(&t)
                    {
                        map_transitions.push(t);
                    }
                }
            }
            if matches!(
                short.as_str(),
                "seqact_levelstreaming" | "seqact_multilevelstreaming" | "seqact_levelvisibility"
            ) {
                for l in streaming_levels(n) {
                    if !streamed_levels.contains(&l) {
                        streamed_levels.push(l);
                    }
                }
            }
        }
        let c = |names: &[&str]| -> usize {
            names
                .iter()
                .map(|n| short_counts.get(*n).copied().unwrap_or(0))
                .sum()
        };
        let features = Features {
            checkpoint_triggers: c(&["seqact_triggercheckpoint"]),
            checkpoint_toggles: c(&["seqact_togglecheckpointenable"]),
            restart_option_toggles: c(&["seqact_togglerestartfromcheckpointoption"]),
            grapple_toggles: c(&["seqact_togglegrapple"]),
            max_grapple_sets: c(&["seqact_setmaxgrapples"]),
            rocket_boot_toggles: c(&["seqact_togglerocketboots"]),
            narrator_lines: c(&["seqact_narratorline"]),
            time_trial_starts: c(&["seqact_starttimetrial"]),
            time_trial_ends: c(&["seqact_endtimetrial"]),
            time_trial_checks: c(&["seqcond_istimetrial"]),
            story_mode_toggles: c(&["seqact_togglestorymode", "seqact_togglespawninstorymode"]),
            achievements: c(&["seqact_unlockasamuachievement"]),
            game_finished: c(&["seqact_setgamefinished"]),
            level_streaming_actions: c(&[
                "seqact_levelstreaming",
                "seqact_multilevelstreaming",
                "seqact_levelvisibility",
            ]),
            console_commands: c(&["seqact_consolecommand"]),
            matinee_actions: c(&["seqact_interp"]),
            remote_event_actions: c(&["seqact_activateremoteevent"]),
            remote_event_listeners: c(&["seqevent_remoteevent"]),
        };
        let mut sequences = SequenceCounts::default();
        for s in &self.sequences {
            let in_level = self
                .nodes
                .get(s.node)
                .is_some_and(|n| n.scope == NodeScope::Level);
            match s.kind {
                SequenceKind::Root => sequences.root += usize::from(in_level),
                SequenceKind::Sub => sequences.sub += usize::from(in_level),
                SequenceKind::PrefabContainer => {
                    sequences.prefab_container += usize::from(in_level)
                }
                SequenceKind::PrefabInstance => sequences.prefab_instance += 1,
                SequenceKind::PrefabArchetype => sequences.prefab_archetype += 1,
            }
            if in_level {
                sequences.max_depth = sequences.max_depth.max(s.depth);
            }
        }
        let mut links = LinkCounts {
            dangling: self.dangling.len(),
            ..LinkCounts::default()
        };
        for e in &self.edges {
            let in_level = self
                .nodes
                .get(e.from)
                .is_some_and(|n| n.scope == NodeScope::Level);
            if !in_level {
                continue;
            }
            *links.by_kind.entry(e.kind.name().to_owned()).or_insert(0) += 1;
            if e.derived {
                links.derived += 1;
            } else {
                links.stored += 1;
            }
            links.cross_sequence += usize::from(e.cross_sequence);
            links.delayed += usize::from(e.kind == EdgeKind::Output && e.delay.is_some());
        }
        for u in &self.unresolved {
            let in_level = self
                .nodes
                .get(u.from)
                .is_some_and(|n| n.scope == NodeScope::Level);
            if !in_level {
                continue;
            }
            match u.kind {
                EdgeKind::RemoteEvent => links.unresolved_remote_events += 1,
                EdgeKind::NamedVariable => links.unresolved_named_variables += 1,
                _ => {}
            }
        }
        let adj = self.adjacency();
        let milestones = level
            .iter()
            .filter(|n| MILESTONES.contains(&n.class_name().to_ascii_lowercase().as_str()))
            .map(|n| Milestone {
                node: n.id,
                class: n.class_name().to_owned(),
                sequence: n
                    .parent
                    .and_then(|p| self.nodes.get(p))
                    .map(|p| p.name.clone()),
                values: self.milestone_values(&adj, n),
                triggers: self.triggers_with(&adj, n.id),
            })
            .collect();
        KismetSummary {
            package: self.package.clone(),
            kismet_objects: self.nodes.len(),
            level_objects: level.len(),
            prefab_archetype_objects: self
                .nodes
                .iter()
                .filter(|n| n.scope == NodeScope::Prefab)
                .count(),
            detached_objects: self
                .nodes
                .iter()
                .filter(|n| n.scope == NodeScope::Detached)
                .count(),
            by_kind,
            by_class,
            custom_classes,
            event_classes,
            sequences,
            links,
            disabled,
            features,
            map_transitions,
            streamed_levels,
            milestones,
        }
    }

    /// Graphviz DOT rendering: one cluster per sequence (nested), nodes
    /// labelled with class and object name, edges styled by kind. Frames are
    /// omitted. Contains object names and paths: keep it local.
    pub fn to_dot(&self) -> String {
        let mut out = String::new();
        let _ = writeln!(out, "digraph \"{}\" {{", dot_escape(&self.package));
        out.push_str("  rankdir=LR;\n  compound=true;\n  node [fontsize=10, shape=box];\n");
        out.push_str("  edge [fontsize=8];\n");
        let mut children: BTreeMap<Option<usize>, Vec<usize>> = BTreeMap::new();
        for n in &self.nodes {
            if n.kind == NodeKind::Frame {
                continue;
            }
            children.entry(n.parent).or_default().push(n.id);
        }
        let mut emitted = HashSet::new();
        if let Some(top) = children.get(&None).cloned() {
            for id in top {
                self.dot_node(&mut out, id, &children, 1, &mut emitted);
            }
        }
        // Anything not reached (cyclic parents) is emitted at top level.
        for n in &self.nodes {
            if n.kind != NodeKind::Frame && !emitted.contains(&n.id) {
                self.dot_leaf(&mut out, n, 1);
                emitted.insert(n.id);
            }
        }
        for e in &self.edges {
            if !emitted.contains(&e.from) || !emitted.contains(&e.to) {
                continue;
            }
            let (style, color) = match e.kind {
                EdgeKind::Output => ("solid", "black"),
                EdgeKind::Variable => ("dashed", "blue"),
                EdgeKind::Matinee => ("dashed", "purple"),
                EdgeKind::Event => ("dotted", "darkgreen"),
                EdgeKind::SubsequenceInput | EdgeKind::SubsequenceOutput => ("solid", "gray50"),
                EdgeKind::RemoteEvent => ("bold", "red"),
                EdgeKind::NamedVariable => ("dotted", "blue"),
            };
            let mut label = String::new();
            if e.kind == EdgeKind::Output {
                let from = self
                    .nodes
                    .get(e.from)
                    .and_then(|n| e.from_port.and_then(|p| n.outputs.get(p)))
                    .map(|p| p.desc.as_str())
                    .unwrap_or("");
                let to = self
                    .nodes
                    .get(e.to)
                    .and_then(|n| e.to_port.and_then(|p| n.inputs.get(p)))
                    .map(|p| p.desc.as_str())
                    .unwrap_or("");
                label = format!("{from} > {to}");
                if let Some(d) = e.delay {
                    let _ = write!(label, " (+{d}s)");
                }
            }
            let _ = writeln!(
                out,
                "  n{} -> n{} [style={style}, color={color}, label=\"{}\"];",
                e.from,
                e.to,
                dot_escape(&label)
            );
        }
        out.push_str("}\n");
        out
    }

    fn dot_node(
        &self,
        out: &mut String,
        id: usize,
        children: &BTreeMap<Option<usize>, Vec<usize>>,
        depth: usize,
        emitted: &mut HashSet<usize>,
    ) {
        let Some(n) = self.nodes.get(id) else {
            return;
        };
        if !emitted.insert(id) {
            return;
        }
        let indent = "  ".repeat(depth.min(MAX_SEQUENCE_DEPTH));
        if n.kind == NodeKind::Sequence && depth <= MAX_SEQUENCE_DEPTH {
            let label = n.obj_name.as_deref().unwrap_or(&n.name);
            let _ = writeln!(out, "{indent}subgraph cluster_{id} {{");
            let _ = writeln!(
                out,
                "{indent}  label=\"{} ({})\";",
                dot_escape(label),
                dot_escape(n.class_name())
            );
            self.dot_leaf(out, n, depth + 1);
            if let Some(kids) = children.get(&Some(id)) {
                for &k in kids {
                    self.dot_node(out, k, children, depth + 1, emitted);
                }
            }
            let _ = writeln!(out, "{indent}}}");
        } else {
            self.dot_leaf(out, n, depth);
        }
    }

    fn dot_leaf(&self, out: &mut String, n: &KismetNode, depth: usize) {
        let indent = "  ".repeat(depth.min(MAX_SEQUENCE_DEPTH));
        let shape = match n.kind {
            NodeKind::Event => "house",
            NodeKind::Condition => "diamond",
            NodeKind::Variable => "ellipse",
            NodeKind::Sequence => "box3d",
            _ => "box",
        };
        let mut label = format!("{}\\n{}", dot_escape(n.class_name()), dot_escape(&n.name));
        if let Some(v) = n.variable.as_ref().and_then(|v| v.value.as_ref()) {
            let r = v.render();
            let r: String = r.chars().take(48).collect();
            let _ = write!(label, "\\n= {}", dot_escape(&r));
        }
        let color = if n.custom { ", color=darkorange" } else { "" };
        let _ = writeln!(
            out,
            "{indent}n{} [shape={shape}, label=\"{label}\"{color}];",
            n.id
        );
    }
}

/// Render a value with object references reduced to their object name.
fn short_render(v: &Value) -> String {
    match v {
        Value::Object(o) | Value::Interface(o) => last_component(&o.path).to_owned(),
        other => other.render(),
    }
}

fn dot_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' | '\r' => out.push_str("\\n"),
            c if c.is_control() => {}
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chain(names: &[&str]) -> Vec<String> {
        names.iter().map(|s| s.to_ascii_lowercase()).collect()
    }

    #[test]
    fn classification() {
        let c = |n: &[&str]| classify_chain(&chain(n));
        assert_eq!(
            c(&[
                "SeqAct_Interp",
                "SeqAct_Latent",
                "SequenceAction",
                "SequenceOp",
                "SequenceObject",
                "Object"
            ]),
            Some(NodeKind::Action)
        );
        assert_eq!(
            c(&["PrefabSequence", "Sequence", "SequenceOp", "SequenceObject"]),
            Some(NodeKind::Sequence)
        );
        assert_eq!(
            c(&["InterpData", "SequenceVariable", "SequenceObject"]),
            Some(NodeKind::Variable)
        );
        assert_eq!(
            c(&["SequenceFrameWrapped", "SequenceFrame", "SequenceObject"]),
            Some(NodeKind::Frame)
        );
        assert_eq!(c(&["Odd", "SequenceObject"]), Some(NodeKind::Object));
        assert_eq!(c(&["SeqAct_Interp"]), None);
        assert_eq!(c(&[]), None);
    }

    #[test]
    fn open_targets() {
        assert_eq!(
            open_target("open AG-IceCave"),
            Some("AG-IceCave".to_owned())
        );
        assert_eq!(
            open_target("OPEN Menu?game=Pkg.Game"),
            Some("Menu".to_owned())
        );
        assert_eq!(open_target("ToggleCrosshair false"), None);
        assert_eq!(open_target("open"), None);
        assert_eq!(open_target(""), None);
    }

    #[test]
    fn dot_escaping() {
        assert_eq!(dot_escape("a\"b\\c\nd\u{1}"), "a\\\"b\\\\c\\nd");
    }

    #[test]
    fn tagged_struct_merge() {
        let p = |n: &str, v: Value| Property {
            name: n.into(),
            type_name: "IntProperty".into(),
            array_index: 0,
            size: 4,
            struct_name: None,
            enum_name: None,
            value: v,
            offset: 0,
        };
        let mut a = Value::Struct {
            name: "S".into(),
            binary: false,
            fields: vec![p("X", Value::Int(1)), p("Y", Value::Int(2))],
        };
        let b = Value::Struct {
            name: "S".into(),
            binary: false,
            fields: vec![p("Y", Value::Int(5))],
        };
        merge_into(&mut a, &b, 0);
        assert_eq!(as_int(field(&a, "X")), Some(1));
        assert_eq!(as_int(field(&a, "Y")), Some(5));
        let mut bin = Value::Int(3);
        merge_into(&mut bin, &Value::Int(4), 0);
        assert_eq!(bin, Value::Int(4));
    }
}
