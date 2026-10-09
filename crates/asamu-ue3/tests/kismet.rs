//! Kismet graph recovery against synthetic map packages written byte by byte
//! here (no original game data), plus hostile-input checks.
//!
//! The synthetic map mirrors the shipped layout: `TheWorld.PersistentLevel`
//! (a `Level`) holds `Main_Sequence`, whose ops store their links as tagged
//! properties (`InputLinks`, `OutputLinks`, `VariableLinks`, `EventLinks`,
//! `SequenceObjects`, `ParentSequence`). A prefab archetype lives under a
//! top-level package export and is instanced in the level.

#![allow(clippy::unwrap_used)]

mod common;

use std::collections::HashMap;
use std::sync::Arc;

use asamu_ue3::kismet::{
    ClassDefaults, DanglingReason, EdgeKind, KismetGraph, NoClassDefaults, NodeKind, NodeScope,
    Origin, SequenceKind, build_graph, classify_chain, open_target,
};
use asamu_ue3::schema::{PropertyDef, PropertyType, Schema, StructDef, StructKind};
use asamu_ue3::{ObjRef, Package, Property, Value};
use common::{Export, Import, Synth, W};

// ------------------------------------------------------------------ values

#[derive(Clone, Debug)]
enum V {
    Int(i32),
    Float(f32),
    Bool(bool),
    Str(&'static str),
    Name(&'static str),
    /// Package index (export i => i + 1, import j => -(j + 1)).
    Obj(i32),
    Arr(Vec<V>),
    Struct(&'static str, Vec<(&'static str, V)>),
}

#[derive(Default)]
struct Names(Vec<String>);

impl Names {
    fn idx(&mut self, s: &str) -> i32 {
        if let Some(i) = self.0.iter().position(|x| x == s) {
            return i as i32;
        }
        self.0.push(s.to_owned());
        (self.0.len() - 1) as i32
    }
    fn fname(&mut self, w: &mut W, s: &str) {
        let i = self.idx(s);
        w.i32(i);
        w.i32(0);
    }
}

fn type_name(v: &V) -> &'static str {
    match v {
        V::Int(_) => "IntProperty",
        V::Float(_) => "FloatProperty",
        V::Bool(_) => "BoolProperty",
        V::Str(_) => "StrProperty",
        V::Name(_) => "NameProperty",
        V::Obj(_) => "ObjectProperty",
        V::Arr(_) => "ArrayProperty",
        V::Struct(..) => "StructProperty",
    }
}

/// Item encoding (array elements): structs are nested tagged streams.
fn item(n: &mut Names, w: &mut W, v: &V) {
    match v {
        V::Int(i) => w.i32(*i),
        V::Float(f) => w.bytes(&f.to_le_bytes()),
        V::Bool(b) => w.bytes(&[u8::from(*b)]),
        V::Str(s) => w.fstring(s),
        V::Name(s) => n.fname(w, s),
        V::Obj(i) => w.i32(*i),
        V::Arr(items) => {
            w.i32(items.len() as i32);
            for i in items {
                item(n, w, i);
            }
        }
        V::Struct(_, fields) => tagged(n, w, fields),
    }
}

/// One tag plus its value.
fn tag(n: &mut Names, w: &mut W, name: &str, v: &V) {
    let mut body = W::default();
    if let V::Struct(_, fields) = v {
        tagged(n, &mut body, fields);
    } else if !matches!(v, V::Bool(_)) {
        item(n, &mut body, v);
    }
    n.fname(w, name);
    n.fname(w, type_name(v));
    w.i32(body.len() as i32);
    w.i32(0);
    match v {
        V::Struct(s, _) => n.fname(w, s),
        V::Bool(b) => w.bytes(&[u8::from(*b)]),
        _ => {}
    }
    w.bytes(&body.0);
}

fn tagged(n: &mut Names, w: &mut W, fields: &[(&str, V)]) {
    for (name, v) in fields {
        tag(n, w, name, v);
    }
    n.fname(w, "None");
}

// ------------------------------------------------------------------ links

fn input(desc: &'static str) -> V {
    V::Struct(
        "SeqOpInputLink",
        vec![("LinkDesc", V::Str(desc)), ("ActivateDelay", V::Float(0.0))],
    )
}

fn output(desc: &'static str, links: &[(usize, i32)], delay: f32) -> V {
    V::Struct(
        "SeqOpOutputLink",
        vec![
            (
                "Links",
                V::Arr(
                    links
                        .iter()
                        .map(|&(op, idx)| {
                            V::Struct(
                                "SeqOpOutputInputLink",
                                vec![("LinkedOp", V::Obj(e(op))), ("InputLinkIdx", V::Int(idx))],
                            )
                        })
                        .collect(),
                ),
            ),
            ("LinkDesc", V::Str(desc)),
            ("ActivateDelay", V::Float(delay)),
        ],
    )
}

fn var_link(desc: &'static str, prop: &'static str, vars: &[i32]) -> V {
    V::Struct(
        "SeqVarLink",
        vec![
            (
                "LinkedVariables",
                V::Arr(vars.iter().map(|&i| V::Obj(i)).collect()),
            ),
            ("LinkDesc", V::Str(desc)),
            ("PropertyName", V::Name(prop)),
            ("MinVars", V::Int(1)),
            ("MaxVars", V::Int(255)),
        ],
    )
}

fn event_link(events: &[usize]) -> V {
    V::Struct(
        "SeqEventLink",
        vec![
            (
                "LinkedEvents",
                V::Arr(events.iter().map(|&i| V::Obj(e(i))).collect()),
            ),
            ("LinkDesc", V::Str("Event")),
        ],
    )
}

/// Package index of 0-based export `i`.
fn e(i: usize) -> i32 {
    i as i32 + 1
}

// ------------------------------------------------------------------ spec

#[derive(Clone)]
struct ExportSpec {
    class: &'static str,
    outer: i32,
    name: &'static str,
    archetype: i32,
    props: Vec<(&'static str, V)>,
    raw: Option<Vec<u8>>,
}

#[derive(Clone)]
struct Spec {
    exports: Vec<ExportSpec>,
}

/// Export indices of the base map.
mod ex {
    pub const WORLD: usize = 0;
    pub const LEVEL: usize = 1;
    pub const MAIN: usize = 2;
    pub const LOADED: usize = 3;
    pub const SET_MAX: usize = 4;
    pub const VAR_INT: usize = 5;
    pub const INTERP: usize = 6;
    pub const INTERP_DATA: usize = 7;
    pub const ACTIVATE_REMOTE: usize = 8;
    pub const SUB: usize = 9;
    pub const REMOTE: usize = 10;
    pub const CONSOLE: usize = 11;
    pub const NAMED: usize = 12;
    pub const TOGGLE: usize = 13;
    pub const FRAME: usize = 14;
    pub const TRIGGER: usize = 15;
    pub const TOUCH: usize = 16;
    pub const STREAM: usize = 17;
    pub const PREFAB_PKG: usize = 18;
    pub const PREFAB: usize = 19;
    pub const ARCH_SEQ: usize = 20;
    pub const ARCH_LOADED: usize = 21;
    pub const ARCH_INTERP: usize = 22;
    pub const INST_SEQ: usize = 23;
    pub const INST_LOADED: usize = 24;
    pub const INST_INTERP: usize = 25;
}

fn x(
    class: &'static str,
    outer: usize,
    name: &'static str,
    props: Vec<(&'static str, V)>,
) -> ExportSpec {
    ExportSpec {
        class,
        outer: if outer == usize::MAX { 0 } else { e(outer) },
        name,
        archetype: 0,
        props,
        raw: None,
    }
}

const TOP: usize = usize::MAX;

fn parent(i: usize) -> (&'static str, V) {
    ("ParentSequence", V::Obj(e(i)))
}

fn base() -> Spec {
    use ex::*;
    let exports = vec![
        x("Engine.World", TOP, "TheWorld", vec![]),
        x("Engine.Level", WORLD, "PersistentLevel", vec![]),
        x(
            "Engine.Sequence",
            LEVEL,
            "Main_Sequence",
            vec![(
                "SequenceObjects",
                V::Arr(
                    [
                        LOADED,
                        SET_MAX,
                        VAR_INT,
                        INTERP,
                        INTERP_DATA,
                        ACTIVATE_REMOTE,
                        SUB,
                        TOGGLE,
                        FRAME,
                        TOUCH,
                        STREAM,
                        INST_SEQ,
                    ]
                    .iter()
                    .map(|&i| V::Obj(e(i)))
                    .collect(),
                ),
            )],
        ),
        x(
            "Engine.SeqEvent_LevelLoaded",
            MAIN,
            "SeqEvent_LevelLoaded_0",
            vec![
                (
                    "OutputLinks",
                    V::Arr(vec![
                        output("Loaded and Visible", &[(SET_MAX, 0)], 0.5),
                        output(
                            "Beginning of Level",
                            &[(INTERP, 0), (ACTIVATE_REMOTE, 0)],
                            0.0,
                        ),
                    ]),
                ),
                parent(MAIN),
                ("ObjComment", V::Str("start \"here\"")),
            ],
        ),
        x(
            "asamu.SeqAct_SetMaxGrapples",
            MAIN,
            "SeqAct_SetMaxGrapples_0",
            vec![
                ("InputLinks", V::Arr(vec![input("In")])),
                (
                    "OutputLinks",
                    V::Arr(vec![output("Out", &[(TOGGLE, 2)], 0.0)]),
                ),
                (
                    "VariableLinks",
                    V::Arr(vec![var_link("Grapples", "Grapples", &[e(VAR_INT)])]),
                ),
                parent(MAIN),
            ],
        ),
        x(
            "Engine.SeqVar_Int",
            MAIN,
            "SeqVar_Int_0",
            vec![
                ("IntValue", V::Int(3)),
                ("VarName", V::Name("Counter")),
                parent(MAIN),
            ],
        ),
        x(
            "Engine.SeqAct_Interp",
            MAIN,
            "SeqAct_Interp_0",
            vec![
                ("InputLinks", V::Arr(vec![input("Play"), input("Reverse")])),
                (
                    "OutputLinks",
                    V::Arr(vec![
                        output("Completed", &[], 0.0),
                        output("Reversed", &[], 0.0),
                    ]),
                ),
                (
                    "VariableLinks",
                    V::Arr(vec![var_link("Data", "None", &[e(INTERP_DATA)])]),
                ),
                parent(MAIN),
            ],
        ),
        x(
            "Engine.InterpData",
            MAIN,
            "InterpData_0",
            vec![parent(MAIN)],
        ),
        x(
            "Engine.SeqAct_ActivateRemoteEvent",
            MAIN,
            "SeqAct_ActivateRemoteEvent_0",
            vec![
                ("EventName", V::Name("GoSub")),
                ("InputLinks", V::Arr(vec![input("In")])),
                ("OutputLinks", V::Arr(vec![output("Out", &[], 0.0)])),
                parent(MAIN),
            ],
        ),
        x(
            "Engine.Sequence",
            MAIN,
            "Sub",
            vec![
                (
                    "SequenceObjects",
                    V::Arr(vec![
                        V::Obj(e(REMOTE)),
                        V::Obj(e(CONSOLE)),
                        V::Obj(e(NAMED)),
                    ]),
                ),
                parent(MAIN),
            ],
        ),
        x(
            "Engine.SeqEvent_RemoteEvent",
            SUB,
            "SeqEvent_RemoteEvent_0",
            vec![
                ("EventName", V::Name("GoSub")),
                (
                    "OutputLinks",
                    V::Arr(vec![output("Out", &[(CONSOLE, 0)], 0.0)]),
                ),
                parent(SUB),
            ],
        ),
        x(
            "Engine.SeqAct_ConsoleCommand",
            SUB,
            "SeqAct_ConsoleCommand_0",
            vec![
                (
                    "Commands",
                    V::Arr(vec![
                        V::Str("open AG-Next?game=Pkg.Game"),
                        V::Str("ToggleCrosshair false"),
                    ]),
                ),
                ("InputLinks", V::Arr(vec![input("In")])),
                ("OutputLinks", V::Arr(vec![output("Out", &[], 0.0)])),
                parent(SUB),
            ],
        ),
        x(
            "Engine.SeqVar_Named",
            SUB,
            "SeqVar_Named_0",
            vec![("FindVarName", V::Name("Counter")), parent(SUB)],
        ),
        // No InputLinks stored: the class defaults supply three.
        x(
            "Engine.SeqAct_Toggle",
            MAIN,
            "SeqAct_Toggle_0",
            vec![
                ("OutputLinks", V::Arr(vec![output("Out", &[], 0.0)])),
                ("EventLinks", V::Arr(vec![event_link(&[LOADED])])),
                parent(MAIN),
            ],
        ),
        // No ParentSequence: the parent comes from Main_Sequence's list.
        x(
            "Engine.SequenceFrame",
            MAIN,
            "SequenceFrame_0",
            vec![("ObjComment", V::Str("box"))],
        ),
        x("Engine.Trigger", LEVEL, "Trigger_0", vec![]),
        x(
            "Engine.SeqEvent_Touch",
            MAIN,
            "SeqEvent_Touch_0",
            vec![
                ("Originator", V::Obj(e(TRIGGER))),
                ("MaxTriggerCount", V::Int(0)),
                ("ReTriggerDelay", V::Float(0.25)),
                ("bEnabled", V::Bool(false)),
                (
                    "OutputLinks",
                    V::Arr(vec![output("Touched", &[(STREAM, 0)], 0.0)]),
                ),
                parent(MAIN),
            ],
        ),
        x(
            "Engine.SeqAct_MultiLevelStreaming",
            MAIN,
            "SeqAct_MultiLevelStreaming_0",
            vec![
                (
                    "Levels",
                    V::Arr(vec![V::Struct(
                        "LevelStreamingInfo",
                        vec![("Level", V::Obj(0)), ("LevelName", V::Name("nextlevel"))],
                    )]),
                ),
                ("InputLinks", V::Arr(vec![input("Load"), input("Unload")])),
                ("OutputLinks", V::Arr(vec![output("Finished", &[], 0.0)])),
                parent(MAIN),
            ],
        ),
        x("Core.Package", TOP, "PrefabPkg", vec![]),
        x("Engine.Prefab", PREFAB_PKG, "MyPrefab", vec![]),
        x(
            "Engine.PrefabSequence",
            PREFAB,
            "PrefabSequence_0",
            vec![(
                "SequenceObjects",
                V::Arr(vec![V::Obj(e(ARCH_LOADED)), V::Obj(e(ARCH_INTERP))]),
            )],
        ),
        x(
            "Engine.SeqEvent_LevelLoaded",
            ARCH_SEQ,
            "SeqEvent_LevelLoaded_9",
            vec![
                (
                    "OutputLinks",
                    V::Arr(vec![output("Loaded and Visible", &[(ARCH_INTERP, 0)], 0.0)]),
                ),
                parent(ARCH_SEQ),
            ],
        ),
        x(
            "Engine.SeqAct_Interp",
            ARCH_SEQ,
            "SeqAct_Interp_9",
            vec![
                ("InputLinks", V::Arr(vec![input("Play")])),
                parent(ARCH_SEQ),
            ],
        ),
        ExportSpec {
            archetype: e(ARCH_SEQ),
            ..x(
                "Engine.PrefabSequence",
                MAIN,
                "Inst_Seq",
                vec![
                    (
                        "SequenceObjects",
                        V::Arr(vec![V::Obj(e(INST_LOADED)), V::Obj(e(INST_INTERP))]),
                    ),
                    parent(MAIN),
                ],
            )
        },
        // Stores no OutputLinks: inherited from the archetype, remapped.
        ExportSpec {
            archetype: e(ARCH_LOADED),
            ..x(
                "Engine.SeqEvent_LevelLoaded",
                INST_SEQ,
                "SeqEvent_LevelLoaded_1",
                vec![parent(INST_SEQ)],
            )
        },
        // Stores no InputLinks: inherited from the archetype.
        ExportSpec {
            archetype: e(ARCH_INTERP),
            ..x(
                "Engine.SeqAct_Interp",
                INST_SEQ,
                "SeqAct_Interp_1",
                vec![parent(INST_SEQ)],
            )
        },
    ];
    Spec { exports }
}

/// Write the spec as an uncompressed v868 package.
fn build_bytes(spec: &Spec) -> Vec<u8> {
    let mut n = Names::default();
    for s in ["None", "Core", "Package", "Class"] {
        n.idx(s);
    }
    let mut imports: Vec<Import> = Vec::new();
    let mut packages: HashMap<String, i32> = HashMap::new();
    let mut classes: HashMap<String, i32> = HashMap::new();
    let mut import_of = |n: &mut Names, imports: &mut Vec<Import>, class: &str| -> i32 {
        if let Some(&i) = classes.get(class) {
            return i;
        }
        let (pkg, cls) = class.split_once('.').unwrap();
        let pkg_idx = match packages.get(pkg) {
            Some(&p) => p,
            None => {
                imports.push(Import {
                    class_package: n.idx("Core"),
                    class_name: n.idx("Package"),
                    outer: 0,
                    name: n.idx(pkg),
                    number: 0,
                });
                let p = -(imports.len() as i32);
                packages.insert(pkg.to_owned(), p);
                p
            }
        };
        imports.push(Import {
            class_package: n.idx("Core"),
            class_name: n.idx("Class"),
            outer: pkg_idx,
            name: n.idx(cls),
            number: 0,
        });
        let i = -(imports.len() as i32);
        classes.insert(class.to_owned(), i);
        i
    };
    let mut exports = Vec::new();
    for (i, s) in spec.exports.iter().enumerate() {
        let class = import_of(&mut n, &mut imports, s.class);
        let payload = match &s.raw {
            Some(r) => r.clone(),
            None => {
                let mut w = W::default();
                w.i32(i as i32); // NetIndex
                tagged(&mut n, &mut w, &s.props);
                w.0
            }
        };
        let name = n.idx(s.name);
        exports.push(Export {
            class,
            super_: 0,
            outer: s.outer,
            name,
            number: 0,
            archetype: s.archetype,
            object_flags: 0x0007_0004_0000_0000,
            payload,
            export_flags: 0,
            net_counts: Vec::new(),
            guid: [0; 4],
            package_flags: 0,
        });
    }
    let mut synth = Synth::sample();
    synth.names = n.0.iter().map(|s| (s.clone(), 0u64)).collect();
    synth.imports = imports;
    synth.exports = exports;
    synth.package_flags = 0x0002_0008;
    synth.texture_allocations = Vec::new();
    synth.additional_packages = Vec::new();
    synth.build().0
}

fn build_pkg(spec: &Spec) -> Package {
    Package::from_bytes(build_bytes(spec)).unwrap()
}

// ------------------------------------------------------------------ schema

struct TestSchema {
    defs: HashMap<String, Arc<PropertyDef>>,
    structs: HashMap<String, Arc<StructDef>>,
    links: HashMap<String, Vec<Arc<PropertyDef>>>,
}

fn def(owner: &str, name: &str, ty: PropertyType) -> PropertyDef {
    PropertyDef {
        name: name.to_owned(),
        path: format!("{owner}.{name}"),
        array_dim: 1,
        flags: 0,
        category: "None".to_owned(),
        array_enum: None,
        rep_offset: None,
        ty,
    }
}

fn array_of(owner: &str, name: &str, inner: PropertyType) -> PropertyType {
    PropertyType::Array {
        inner: Box::new(def(owner, name, inner)),
    }
}

fn obj(class: &str) -> PropertyType {
    PropertyType::Object {
        class: class.to_owned(),
    }
}

fn strukt(path: &str) -> PropertyType {
    PropertyType::Struct {
        struct_path: path.to_owned(),
    }
}

const OP: &str = "Engine.SequenceOp";

impl TestSchema {
    fn new() -> TestSchema {
        let class = PropertyType::Class {
            class: "Core.Class".into(),
            meta_class: "Engine.SequenceObject".into(),
        };
        let base = vec![
            def(
                OP,
                "InputLinks",
                array_of(OP, "InputLinks", strukt("Engine.SequenceOp.SeqOpInputLink")),
            ),
            def(
                OP,
                "OutputLinks",
                array_of(
                    OP,
                    "OutputLinks",
                    strukt("Engine.SequenceOp.SeqOpOutputLink"),
                ),
            ),
            def(
                OP,
                "VariableLinks",
                array_of(OP, "VariableLinks", strukt("Engine.SequenceOp.SeqVarLink")),
            ),
            def(
                OP,
                "EventLinks",
                array_of(OP, "EventLinks", strukt("Engine.SequenceOp.SeqEventLink")),
            ),
            def(
                "Engine.Sequence",
                "SequenceObjects",
                array_of(
                    "Engine.Sequence",
                    "SequenceObjects",
                    obj("Engine.SequenceObject"),
                ),
            ),
            def(
                "Engine.SequenceObject",
                "ParentSequence",
                obj("Engine.Sequence"),
            ),
            def("Engine.SequenceObject", "ObjComment", PropertyType::Str),
            def("Engine.SequenceEvent", "Originator", obj("Engine.Actor")),
            def("Engine.SequenceEvent", "MaxTriggerCount", PropertyType::Int),
            def(
                "Engine.SequenceEvent",
                "ReTriggerDelay",
                PropertyType::Float,
            ),
            def("Engine.SequenceEvent", "bEnabled", PropertyType::Bool),
            def("Engine.SequenceVariable", "VarName", PropertyType::Name),
        ];
        let members = vec![
            def(OP, "LinkDesc", PropertyType::Str),
            def(OP, "ActivateDelay", PropertyType::Float),
            def(OP, "bDisabled", PropertyType::Bool),
            def(OP, "LinkedOp", obj("Engine.SequenceOp")),
            def(OP, "InputLinkIdx", PropertyType::Int),
            def(
                OP,
                "Links",
                array_of(
                    OP,
                    "Links",
                    strukt("Engine.SequenceOp.SeqOpOutputInputLink"),
                ),
            ),
            def(OP, "ExpectedType", class),
            def(
                OP,
                "LinkedVariables",
                array_of(OP, "LinkedVariables", obj("Engine.SequenceVariable")),
            ),
            def(
                OP,
                "LinkedEvents",
                array_of(OP, "LinkedEvents", obj("Engine.SequenceEvent")),
            ),
            def(OP, "LinkVar", PropertyType::Name),
            def(OP, "PropertyName", PropertyType::Name),
            def(OP, "bWriteable", PropertyType::Bool),
            def(OP, "MinVars", PropertyType::Int),
            def(OP, "MaxVars", PropertyType::Int),
            def(
                "Engine.SeqAct_MultiLevelStreaming",
                "Level",
                obj("Engine.LevelStreaming"),
            ),
            def(
                "Engine.SeqAct_MultiLevelStreaming",
                "LevelName",
                PropertyType::Name,
            ),
        ];
        let specific: Vec<(&str, Vec<PropertyDef>)> = vec![
            (
                "asamu.SeqAct_SetMaxGrapples",
                vec![def(
                    "asamu.SeqAct_SetMaxGrapples",
                    "Grapples",
                    PropertyType::Int,
                )],
            ),
            (
                "Engine.SeqAct_ConsoleCommand",
                vec![def(
                    "Engine.SeqAct_ConsoleCommand",
                    "Commands",
                    array_of(
                        "Engine.SeqAct_ConsoleCommand",
                        "Commands",
                        PropertyType::Str,
                    ),
                )],
            ),
            (
                "Engine.SeqAct_MultiLevelStreaming",
                vec![def(
                    "Engine.SeqAct_MultiLevelStreaming",
                    "Levels",
                    array_of(
                        "Engine.SeqAct_MultiLevelStreaming",
                        "Levels",
                        strukt("Engine.SeqAct_MultiLevelStreaming.LevelStreamingInfo"),
                    ),
                )],
            ),
            (
                "Engine.SeqAct_ActivateRemoteEvent",
                vec![def(
                    "Engine.SeqAct_ActivateRemoteEvent",
                    "EventName",
                    PropertyType::Name,
                )],
            ),
            (
                "Engine.SeqEvent_RemoteEvent",
                vec![def(
                    "Engine.SeqEvent_RemoteEvent",
                    "EventName",
                    PropertyType::Name,
                )],
            ),
            (
                "Engine.SeqVar_Int",
                vec![def("Engine.SeqVar_Int", "IntValue", PropertyType::Int)],
            ),
            (
                "Engine.SeqVar_Named",
                vec![def(
                    "Engine.SeqVar_Named",
                    "FindVarName",
                    PropertyType::Name,
                )],
            ),
        ];
        let mut defs = HashMap::new();
        for d in base
            .iter()
            .chain(members.iter())
            .chain(specific.iter().flat_map(|(_, v)| v.iter()))
        {
            defs.insert(d.name.to_ascii_lowercase(), Arc::new(d.clone()));
        }
        let mut links = HashMap::new();
        for (class, own) in &specific {
            let mut v: Vec<Arc<PropertyDef>> = own.iter().cloned().map(Arc::new).collect();
            v.extend(base.iter().cloned().map(Arc::new));
            links.insert(class.to_ascii_lowercase(), v);
        }
        let mk = |path: &str, members: &[&str]| -> (String, Arc<StructDef>) {
            let properties = members
                .iter()
                .map(|m| defs.get(&m.to_ascii_lowercase()).unwrap().clone())
                .collect();
            let name = path.rsplit('.').next().unwrap().to_owned();
            (
                path.to_ascii_lowercase(),
                Arc::new(StructDef {
                    path: path.to_owned(),
                    name,
                    kind: StructKind::ScriptStruct,
                    super_path: None,
                    struct_flags: 0,
                    properties,
                }),
            )
        };
        let structs = [
            mk(
                "Engine.SequenceOp.SeqOpInputLink",
                &["LinkDesc", "ActivateDelay", "bDisabled", "LinkedOp"],
            ),
            mk(
                "Engine.SequenceOp.SeqOpOutputLink",
                &[
                    "Links",
                    "LinkDesc",
                    "ActivateDelay",
                    "bDisabled",
                    "LinkedOp",
                ],
            ),
            mk(
                "Engine.SequenceOp.SeqOpOutputInputLink",
                &["LinkedOp", "InputLinkIdx"],
            ),
            mk(
                "Engine.SequenceOp.SeqVarLink",
                &[
                    "ExpectedType",
                    "LinkedVariables",
                    "LinkDesc",
                    "LinkVar",
                    "PropertyName",
                    "bWriteable",
                    "MinVars",
                    "MaxVars",
                ],
            ),
            mk(
                "Engine.SequenceOp.SeqEventLink",
                &["ExpectedType", "LinkedEvents", "LinkDesc"],
            ),
            mk(
                "Engine.SeqAct_MultiLevelStreaming.LevelStreamingInfo",
                &["Level", "LevelName"],
            ),
        ]
        .into_iter()
        .collect();
        TestSchema {
            defs,
            structs,
            links,
        }
    }
}

fn chain_of(class_path: &str) -> Vec<&'static str> {
    let short = class_path
        .rsplit('.')
        .next()
        .unwrap_or("")
        .to_ascii_lowercase();
    let op = ["sequenceop", "sequenceobject", "object"];
    let with = |own: &[&'static str], tail: &[&'static str]| -> Vec<&'static str> {
        own.iter().chain(tail).copied().collect()
    };
    match short.as_str() {
        "sequence" => with(&["sequence"], &op),
        "prefabsequence" => with(&["prefabsequence", "sequence"], &op),
        "seqevent_levelloaded" | "seqevent_remoteevent" | "seqevent_touch" => {
            with(&["x", "sequenceevent"], &op)
        }
        "seqact_setmaxgrapples"
        | "seqact_activateremoteevent"
        | "seqact_consolecommand"
        | "seqact_toggle"
        | "seqact_multilevelstreaming" => with(&["x", "sequenceaction"], &op),
        "seqact_interp" => with(&["seqact_interp", "seqact_latent", "sequenceaction"], &op),
        "interpdata" => vec!["interpdata", "sequencevariable", "sequenceobject", "object"],
        "seqvar_int" => vec!["seqvar_int", "sequencevariable", "sequenceobject", "object"],
        "seqvar_named" => vec![
            "seqvar_named",
            "sequencevariable",
            "sequenceobject",
            "object",
        ],
        "sequenceframe" => vec!["sequenceframe", "sequenceobject", "object"],
        _ => Vec::new(),
    }
    .into_iter()
    .map(|c| if c == "x" { "" } else { c })
    .collect()
}

impl Schema for TestSchema {
    fn struct_def(&self, path: &str) -> Option<Arc<StructDef>> {
        self.structs.get(&path.to_ascii_lowercase()).cloned()
    }
    fn struct_by_name(&self, name: &str) -> Option<Arc<StructDef>> {
        self.structs
            .values()
            .find(|s| s.name.eq_ignore_ascii_case(name))
            .cloned()
    }
    fn find_property(&self, _owner: &str, name: &str) -> Option<Arc<PropertyDef>> {
        self.defs.get(&name.to_ascii_lowercase()).cloned()
    }
    fn property_link(&self, owner: &str) -> Vec<Arc<PropertyDef>> {
        self.links
            .get(&owner.to_ascii_lowercase())
            .cloned()
            .unwrap_or_default()
    }
    fn enum_names(&self, _path: &str) -> Option<Arc<Vec<String>>> {
        None
    }
    fn class_chain(&self, class_path: &str) -> Vec<String> {
        let short = class_path
            .rsplit('.')
            .next()
            .unwrap_or("")
            .to_ascii_lowercase();
        let chain = chain_of(class_path);
        if chain.is_empty() {
            return vec![short];
        }
        chain
            .into_iter()
            .map(|c| {
                if c.is_empty() {
                    short.clone()
                } else {
                    c.to_owned()
                }
            })
            .collect()
    }
}

/// Class defaults: `SeqAct_Toggle` has three inputs and a variable link whose
/// reference must never be resolved (it points into another package);
/// `SequenceEvent`s default to `MaxTriggerCount` 1.
struct TestDefaults;

fn prop(name: &str, type_name: &str, value: Value) -> Property {
    Property {
        name: name.into(),
        type_name: type_name.into(),
        array_index: 0,
        size: 0,
        struct_name: None,
        enum_name: None,
        value,
        offset: 0,
    }
}

fn link_struct(fields: Vec<Property>) -> Value {
    Value::Struct {
        name: "SeqOpInputLink".into(),
        binary: false,
        fields,
    }
}

impl ClassDefaults for TestDefaults {
    fn class_defaults(&self, class_path: &str) -> Vec<Property> {
        let short = class_path
            .rsplit('.')
            .next()
            .unwrap_or("")
            .to_ascii_lowercase();
        let mut out = Vec::new();
        if short.starts_with("seqevent_") {
            out.push(prop("MaxTriggerCount", "IntProperty", Value::Int(1)));
        }
        if short == "seqact_toggle" {
            let input =
                |d: &str| link_struct(vec![prop("LinkDesc", "StrProperty", Value::Str(d.into()))]);
            out.push(prop(
                "InputLinks",
                "ArrayProperty",
                Value::Array(vec![input("Turn On"), input("Turn Off"), input("Toggle")]),
            ));
            out.push(prop(
                "VariableLinks",
                "ArrayProperty",
                Value::Array(vec![link_struct(vec![prop(
                    "LinkedVariables",
                    "ArrayProperty",
                    Value::Array(vec![Value::Object(ObjRef {
                        index: e(ex::VAR_INT),
                        path: "Engine.Default__Something.Elsewhere".into(),
                    })]),
                )])]),
            ));
        }
        out
    }
}

fn graph_of(spec: &Spec) -> KismetGraph {
    let pkg = build_pkg(spec);
    build_graph(&pkg, "TestMap", &TestSchema::new(), &TestDefaults)
}

fn node_of(g: &KismetGraph, export: usize) -> usize {
    g.nodes
        .iter()
        .find(|n| n.export_index == export)
        .map(|n| n.id)
        .unwrap_or_else(|| panic!("export {export} is not a node"))
}

fn edges(g: &KismetGraph, kind: EdgeKind) -> Vec<(usize, usize)> {
    g.edges
        .iter()
        .filter(|e| e.kind == kind)
        .map(|e| (g.nodes[e.from].export_index, g.nodes[e.to].export_index))
        .collect()
}

// ------------------------------------------------------------------ tests

#[test]
fn nodes_kinds_and_sequences() {
    use ex::*;
    let g = graph_of(&base());
    assert!(g.warnings.is_empty(), "{:?}", g.warnings);
    assert_eq!(g.stats.decode_failures, 0);
    assert_eq!(g.stats.decode_warnings, 0, "{:?}", g.nodes);
    // Every export except World, Level, Trigger, PrefabPkg and Prefab.
    assert_eq!(g.nodes.len(), 21);
    assert_eq!(g.stats.kismet_exports, 21);
    let kind = |x: usize| g.nodes[node_of(&g, x)].kind;
    assert_eq!(kind(MAIN), NodeKind::Sequence);
    assert_eq!(kind(LOADED), NodeKind::Event);
    assert_eq!(kind(SET_MAX), NodeKind::Action);
    assert_eq!(kind(INTERP_DATA), NodeKind::Variable);
    assert_eq!(kind(FRAME), NodeKind::Frame);
    assert_eq!(kind(INST_SEQ), NodeKind::Sequence);

    let n = |x: usize| &g.nodes[node_of(&g, x)];
    assert!(n(SET_MAX).custom);
    assert!(!n(INTERP).custom);
    assert_eq!(
        n(SET_MAX).path,
        "TestMap.TheWorld.PersistentLevel.Main_Sequence.SeqAct_SetMaxGrapples_0"
    );
    assert_eq!(
        n(FRAME).parent,
        Some(node_of(&g, MAIN)),
        "parent from SequenceObjects"
    );
    assert_eq!(n(CONSOLE).parent, Some(node_of(&g, SUB)));
    assert_eq!(n(LOADED).comment.as_deref(), Some("start \"here\""));

    // Scopes: the prefab archetype is not part of the level.
    assert_eq!(n(ARCH_LOADED).scope, NodeScope::Prefab);
    assert_eq!(n(INST_LOADED).scope, NodeScope::Level);
    assert_eq!(n(CONSOLE).scope, NodeScope::Level);

    let seq = |x: usize| {
        g.sequences
            .iter()
            .find(|s| s.node == node_of(&g, x))
            .unwrap()
            .clone()
    };
    assert_eq!(seq(MAIN).kind, SequenceKind::Root);
    assert_eq!(seq(MAIN).depth, 0);
    assert_eq!(seq(MAIN).members.len(), 12);
    assert_eq!(seq(SUB).kind, SequenceKind::Sub);
    assert_eq!(seq(SUB).depth, 1);
    assert_eq!(seq(ARCH_SEQ).kind, SequenceKind::PrefabArchetype);
    assert_eq!(seq(INST_SEQ).kind, SequenceKind::PrefabInstance);
    assert_eq!(g.stats.parent_mismatches, 0);
    assert_eq!(g.stats.unlisted_members, 0);
}

#[test]
fn ports_events_variables_and_params() {
    use ex::*;
    let g = graph_of(&base());
    let n = |x: usize| &g.nodes[node_of(&g, x)];
    let loaded = n(LOADED);
    assert_eq!(loaded.outputs.len(), 2);
    assert_eq!(loaded.outputs[0].desc, "Loaded and Visible");
    assert_eq!(loaded.outputs[0].activate_delay, 0.5);
    assert_eq!(loaded.outputs[1].links, 2);
    assert_eq!(loaded.enabled, Some(true));
    assert_eq!(
        loaded.event.as_ref().unwrap().max_trigger_count,
        Some(1),
        "class default"
    );

    let touch = n(TOUCH).event.clone().unwrap();
    assert_eq!(touch.max_trigger_count, Some(0));
    assert_eq!(touch.retrigger_delay, Some(0.25));
    assert_eq!(
        touch.originator.as_deref(),
        Some("TestMap.TheWorld.PersistentLevel.Trigger_0")
    );
    assert_eq!(touch.originator_class.as_deref(), Some("Trigger"));
    assert_eq!(n(TOUCH).enabled, Some(false));

    // Inputs from the class defaults.
    let toggle = n(TOGGLE);
    let descs: Vec<&str> = toggle.inputs.iter().map(|p| p.desc.as_str()).collect();
    assert_eq!(descs, ["Turn On", "Turn Off", "Toggle"]);

    let set_max = n(SET_MAX);
    assert_eq!(set_max.variables.len(), 1);
    assert_eq!(
        set_max.variables[0].property_name.as_deref(),
        Some("Grapples")
    );
    assert_eq!(set_max.variables[0].min_vars, Some(1));
    let grapples = set_max.param("Grapples").unwrap();
    assert_eq!(
        (grapples.value.clone(), grapples.origin),
        (Value::Int(0), Origin::Zero)
    );
    // Base-class properties are not params.
    assert!(set_max.param("InputLinks").is_none());

    let var = n(VAR_INT).variable.clone().unwrap();
    assert_eq!(var.value, Some(Value::Int(3)));
    assert_eq!(var.value_origin, Some(Origin::Own));
    assert_eq!(var.var_name.as_deref(), Some("Counter"));
    let named = n(NAMED).variable.clone().unwrap();
    assert_eq!(named.find_var_name.as_deref(), Some("Counter"));
}

#[test]
fn typed_edges() {
    use ex::*;
    let g = graph_of(&base());
    assert!(g.dangling.is_empty(), "{:?}", g.dangling);
    assert!(g.unresolved.is_empty(), "{:?}", g.unresolved);
    let mut out = edges(&g, EdgeKind::Output);
    out.sort();
    assert_eq!(
        out,
        vec![
            (LOADED, SET_MAX),
            (LOADED, INTERP),
            (LOADED, ACTIVATE_REMOTE),
            (SET_MAX, TOGGLE),
            (REMOTE, CONSOLE),
            (TOUCH, STREAM),
            (ARCH_LOADED, ARCH_INTERP),
            (INST_LOADED, INST_INTERP),
        ]
    );
    let first = g
        .edges
        .iter()
        .find(|e| e.kind == EdgeKind::Output && g.nodes[e.to].export_index == SET_MAX)
        .unwrap();
    assert_eq!(
        (first.from_port, first.to_port, first.delay),
        (Some(0), Some(0), Some(0.5))
    );
    let to_toggle = g
        .edges
        .iter()
        .find(|e| e.kind == EdgeKind::Output && g.nodes[e.to].export_index == TOGGLE)
        .unwrap();
    assert_eq!(
        to_toggle.to_port,
        Some(2),
        "index valid through class-default inputs"
    );
    assert_eq!(edges(&g, EdgeKind::Variable), vec![(SET_MAX, VAR_INT)]);
    assert_eq!(edges(&g, EdgeKind::Matinee), vec![(INTERP, INTERP_DATA)]);
    assert_eq!(edges(&g, EdgeKind::Event), vec![(TOGGLE, LOADED)]);
    assert_eq!(
        edges(&g, EdgeKind::RemoteEvent),
        vec![(ACTIVATE_REMOTE, REMOTE)]
    );
    assert_eq!(edges(&g, EdgeKind::NamedVariable), vec![(NAMED, VAR_INT)]);
    let remote = g
        .edges
        .iter()
        .find(|e| e.kind == EdgeKind::RemoteEvent)
        .unwrap();
    assert!(remote.derived && remote.cross_sequence);
    assert!(
        g.edges
            .iter()
            .filter(|e| !e.derived)
            .all(|e| !e.cross_sequence)
    );
    // The class-default variable link of SeqAct_Toggle points into another
    // package and must not become an edge.
    assert!(
        !g.edges
            .iter()
            .any(|e| g.nodes[e.from].export_index == TOGGLE && e.kind == EdgeKind::Variable)
    );
}

#[test]
fn prefab_instances_inherit_from_their_archetype() {
    use ex::*;
    let g = graph_of(&base());
    assert_eq!(g.stats.ports_from_archetype, 2);
    let inst = &g.nodes[node_of(&g, INST_LOADED)];
    assert_eq!(inst.outputs.len(), 1);
    assert_eq!(inst.outputs[0].desc, "Loaded and Visible");
    assert!(
        inst.archetype
            .as_deref()
            .unwrap()
            .ends_with("PrefabSequence_0.SeqEvent_LevelLoaded_9")
    );
    let interp = &g.nodes[node_of(&g, INST_INTERP)];
    assert_eq!(interp.inputs.len(), 1);
    // The inherited link targets the instance's own op, not the archetype's.
    assert!(edges(&g, EdgeKind::Output).contains(&(INST_LOADED, INST_INTERP)));
    assert!(!edges(&g, EdgeKind::Output).contains(&(INST_LOADED, ARCH_INTERP)));
}

#[test]
fn summary_features_and_milestones() {
    use ex::*;
    let g = graph_of(&base());
    let s = g.summary();
    assert_eq!(s.kismet_objects, 21);
    assert_eq!(s.prefab_archetype_objects, 3);
    assert_eq!(s.level_objects, 18);
    assert_eq!(s.sequences.root, 1);
    assert_eq!(s.sequences.sub, 1);
    assert_eq!(s.sequences.prefab_instance, 1);
    assert_eq!(s.sequences.prefab_archetype, 1);
    assert_eq!(s.sequences.max_depth, 1);
    assert_eq!(s.map_transitions, vec!["AG-Next"]);
    assert_eq!(s.streamed_levels, vec!["nextlevel"]);
    assert_eq!(s.features.max_grapple_sets, 1);
    assert_eq!(s.features.console_commands, 1);
    assert_eq!(s.features.level_streaming_actions, 1);
    assert_eq!(s.features.matinee_actions, 2);
    assert_eq!(
        s.custom_classes.get("asamu.SeqAct_SetMaxGrapples"),
        Some(&1)
    );
    assert_eq!(s.event_classes.get("Engine.SeqEvent_LevelLoaded"), Some(&2));
    assert_eq!(s.disabled.events, 1);
    // Level-scope edges only (the archetype's own edge is excluded).
    assert_eq!(s.links.by_kind.get("output"), Some(&7));
    assert_eq!(s.links.derived, 2);
    assert_eq!(s.links.dangling, 0);

    let m = |class: &str| s.milestones.iter().find(|m| m.class == class).unwrap();
    let set_max = m("SeqAct_SetMaxGrapples");
    assert_eq!(set_max.values, vec!["Grapples<-3"]);
    assert_eq!(set_max.triggers.len(), 1);
    assert_eq!(set_max.triggers[0].event, "SeqEvent_LevelLoaded");
    let console = m("SeqAct_ConsoleCommand");
    assert_eq!(
        console.values,
        vec!["open AG-Next?game=Pkg.Game", "ToggleCrosshair false"]
    );
    assert_eq!(console.sequence.as_deref(), Some("Sub"));
    assert!(
        console.triggers[0].via_remote_event,
        "{:?}",
        console.triggers
    );
    let stream = m("SeqAct_MultiLevelStreaming");
    assert_eq!(stream.values, vec!["level=nextlevel", "input=Load"]);
    assert_eq!(
        stream.triggers[0].originator_class.as_deref(),
        Some("Trigger")
    );
    let _ = (TOUCH, STREAM);
}

#[test]
fn dangling_links_are_reported() {
    use ex::*;
    let mut spec = base();
    // SetMaxGrapples links to: an import, a non-Kismet export, an input index
    // out of range, a null op; its variable link names an op.
    spec.exports[SET_MAX].props = vec![
        ("InputLinks", V::Arr(vec![input("In")])),
        (
            "OutputLinks",
            V::Arr(vec![V::Struct(
                "SeqOpOutputLink",
                vec![(
                    "Links",
                    V::Arr(vec![
                        V::Struct(
                            "SeqOpOutputInputLink",
                            vec![("LinkedOp", V::Obj(-2)), ("InputLinkIdx", V::Int(0))],
                        ),
                        V::Struct(
                            "SeqOpOutputInputLink",
                            vec![
                                ("LinkedOp", V::Obj(e(TRIGGER))),
                                ("InputLinkIdx", V::Int(0)),
                            ],
                        ),
                        V::Struct(
                            "SeqOpOutputInputLink",
                            vec![("LinkedOp", V::Obj(e(TOGGLE))), ("InputLinkIdx", V::Int(7))],
                        ),
                        V::Struct(
                            "SeqOpOutputInputLink",
                            vec![("LinkedOp", V::Obj(0)), ("InputLinkIdx", V::Int(0))],
                        ),
                        V::Struct(
                            "SeqOpOutputInputLink",
                            vec![
                                ("LinkedOp", V::Obj(e(VAR_INT))),
                                ("InputLinkIdx", V::Int(0)),
                            ],
                        ),
                        V::Struct(
                            "SeqOpOutputInputLink",
                            vec![
                                ("LinkedOp", V::Obj(e(ARCH_INTERP))),
                                ("InputLinkIdx", V::Int(0)),
                            ],
                        ),
                    ]),
                )],
            )]),
        ),
        (
            "VariableLinks",
            V::Arr(vec![var_link("Grapples", "Grapples", &[e(INTERP), 9999])]),
        ),
        parent(MAIN),
    ];
    let g = graph_of(&spec);
    let reasons: Vec<DanglingReason> = g.dangling.iter().map(|d| d.reason).collect();
    assert_eq!(
        reasons,
        vec![
            DanglingReason::Import,
            DanglingReason::NotKismet,
            DanglingReason::InputIndexOutOfRange,
            DanglingReason::NullTarget,
            DanglingReason::WrongKind,
            DanglingReason::OtherScope,
            DanglingReason::WrongKind,
            DanglingReason::BadIndex,
        ],
        "{:#?}",
        g.dangling
    );
    assert_eq!(
        g.dangling[2].field,
        "OutputLinks[0].Links[2] (InputLinkIdx 7)"
    );
    assert!(g.dangling[1].target.ends_with("Trigger_0"));
    assert!(
        !edges(&g, EdgeKind::Output)
            .iter()
            .any(|&(f, _)| f == SET_MAX)
    );
    assert_eq!(g.summary().links.dangling, 8);
}

#[test]
fn unresolved_name_matches_are_reported() {
    use ex::*;
    let mut spec = base();
    spec.exports[REMOTE].props[0] = ("EventName", V::Name("SomethingElse"));
    spec.exports[NAMED].props[0] = ("FindVarName", V::Name("Nobody"));
    let g = graph_of(&spec);
    let kinds: Vec<EdgeKind> = g.unresolved.iter().map(|u| u.kind).collect();
    assert_eq!(kinds, vec![EdgeKind::RemoteEvent, EdgeKind::NamedVariable]);
    assert_eq!(g.unresolved[0].name, "GoSub");
    let s = g.summary();
    assert_eq!(
        (
            s.links.unresolved_remote_events,
            s.links.unresolved_named_variables
        ),
        (1, 1)
    );
}

#[test]
fn graphs_are_deterministic_and_render_dot() {
    let a = graph_of(&base());
    let b = graph_of(&base());
    assert_eq!(a, b);
    let dot = a.to_dot();
    assert!(dot.starts_with("digraph \"TestMap\" {"));
    assert!(dot.trim_end().ends_with('}'));
    let main = node_of(&a, ex::MAIN);
    let sub = node_of(&a, ex::SUB);
    assert!(dot.contains(&format!("subgraph cluster_{main} {{")));
    assert!(dot.contains(&format!("subgraph cluster_{sub} {{")));
    let from = node_of(&a, ex::LOADED);
    let to = node_of(&a, ex::SET_MAX);
    assert!(dot.contains(&format!(
        "n{from} -> n{to} [style=solid, color=black, label=\"Loaded and Visible > In (+0.5s)\"];"
    )));
    assert!(dot.contains("style=bold, color=red"), "remote-event edge");
    assert!(dot.contains("color=darkorange"), "custom class highlighted");
    assert!(!dot.contains("SequenceFrame_0"), "frames are omitted");
    // Balanced braces.
    assert_eq!(dot.matches('{').count(), dot.matches('}').count());
}

#[test]
fn works_without_class_defaults_or_schema_knowledge() {
    let pkg = build_pkg(&base());
    let g = build_graph(&pkg, "TestMap", &TestSchema::new(), &NoClassDefaults);
    // Without defaults SeqAct_Toggle has no inputs, so the link into input 2
    // is out of range.
    assert_eq!(g.dangling.len(), 1);
    assert_eq!(g.dangling[0].reason, DanglingReason::InputIndexOutOfRange);
    // Without a schema nothing is classified as Kismet.
    let none = build_graph(&pkg, "TestMap", &asamu_ue3::NoSchema, &NoClassDefaults);
    assert!(none.nodes.is_empty());
}

#[test]
fn helpers() {
    assert_eq!(
        classify_chain(&[
            "seqact_x".into(),
            "sequenceaction".into(),
            "sequenceop".into(),
            "sequenceobject".into()
        ]),
        Some(NodeKind::Action)
    );
    assert_eq!(open_target("open AG-IceCave"), Some("AG-IceCave".into()));
    assert_eq!(open_target("TRAVEL Foo?x"), Some("Foo".into()));
    assert_eq!(open_target("setspeed 0.3"), None);
}

// ------------------------------------------------------------------ hostile

#[test]
fn cyclic_parents_and_archetypes_terminate() {
    use ex::*;
    let mut spec = base();
    // Main_Sequence and Sub claim each other as parent.
    spec.exports[MAIN].props.push(parent(SUB));
    // Two ops are each other's archetype.
    spec.exports[INTERP].archetype = e(ACTIVATE_REMOTE);
    spec.exports[ACTIVATE_REMOTE].archetype = e(INTERP);
    let g = graph_of(&spec);
    assert!(
        g.warnings.iter().any(|w| w.contains("cyclic")),
        "{:?}",
        g.warnings
    );
    let _ = g.summary();
    let _ = g.to_dot();
}

#[test]
fn corrupted_payloads_never_panic() {
    let spec = base();
    let full = build_bytes(&spec);
    let pkg = Package::from_bytes(full).unwrap();
    let schema = TestSchema::new();
    for i in 0..spec.exports.len() {
        let data = pkg.export_data(i).unwrap().to_vec();
        for cut in [0, 1, 4, 7, 12, data.len() / 2, data.len().saturating_sub(1)] {
            let mut s = spec.clone();
            s.exports[i].raw = Some(data[..cut.min(data.len())].to_vec());
            let p = build_pkg(&s);
            let g = build_graph(&p, "TestMap", &schema, &TestDefaults);
            let _ = g.summary();
            let _ = g.to_dot();
        }
        for pos in (0..data.len()).step_by(3) {
            let mut bad = data.clone();
            bad[pos] ^= 0xA5;
            let mut s = spec.clone();
            s.exports[i].raw = Some(bad);
            let p = build_pkg(&s);
            let g = build_graph(&p, "TestMap", &schema, &TestDefaults);
            let _ = g.summary();
        }
    }
    // A truncated Kismet payload is a decode failure, not a missing node.
    let mut s = spec.clone();
    s.exports[ex::LOADED].raw = Some(vec![1, 2]);
    let g = build_graph(&build_pkg(&s), "TestMap", &schema, &TestDefaults);
    assert_eq!(g.stats.decode_failures, 1);
    assert_eq!(g.nodes.len(), 21);
}

#[test]
fn random_package_mutations_never_panic() {
    let bytes = build_bytes(&base());
    let schema = TestSchema::new();
    let mut seed: u64 = 0x9E37_79B9_7F4A_7C15;
    let mut next = || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    for _ in 0..400 {
        let mut b = bytes.clone();
        let flips = 1 + (next() % 8) as usize;
        for _ in 0..flips {
            let pos = (next() as usize) % b.len();
            b[pos] = next() as u8;
        }
        if let Ok(p) = Package::from_bytes(b) {
            let g = build_graph(&p, "TestMap", &schema, &TestDefaults);
            let _ = g.summary();
            let _ = g.to_dot();
        }
    }
}
