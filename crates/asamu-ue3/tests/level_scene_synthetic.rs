//! Scene extraction (`level::extract_scene`) over synthetic map packages
//! written byte by byte here (no original game data), plus hostile-input
//! checks: repeated and foreign actor entries, archetype cycles, broken brush
//! references, shared brush models and archetypes (amplification bounded by
//! the scene budgets), and corruption of every byte of the package.
//!
//! The packages carry no script classes, so class chains are just the class
//! names and binary structs use the built-in `Vector`/`Rotator` layouts:
//! enough for actors, brushes (`Brush` → `Model` → `Polys`), transforms and
//! inherited values.

#![allow(clippy::unwrap_used)]

mod common;

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use asamu_ue3::level::{self, Scene, SceneOptions};
use asamu_ue3::model::PackageSet;
use asamu_ue3::{LoadedPackage, Package};
use common::{Export, Import, Synth, W};

const HAS_STACK: u64 = 0x0200_0000_0000_0000;
const MAP: &str = "SynthMap";

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

/// A tagged property value.
#[derive(Clone)]
enum T {
    Vector([f32; 3]),
    Obj(i32),
    Float(f32),
    Str(String),
}

fn tags(n: &mut Names, w: &mut W, props: &[(&str, T)]) {
    for (name, v) in props {
        n.fname(w, name);
        match v {
            T::Vector(p) => {
                n.fname(w, "StructProperty");
                w.i32(12);
                w.i32(0);
                n.fname(w, "Vector");
                for c in p {
                    w.bytes(&c.to_le_bytes());
                }
            }
            T::Obj(i) => {
                n.fname(w, "ObjectProperty");
                w.i32(4);
                w.i32(0);
                w.i32(*i);
            }
            T::Float(f) => {
                n.fname(w, "FloatProperty");
                w.i32(4);
                w.i32(0);
                w.bytes(&f.to_le_bytes());
            }
            T::Str(s) => {
                n.fname(w, "StrProperty");
                let mut body = W::default();
                body.fstring(s);
                w.i32(body.len() as i32);
                w.i32(0);
                w.bytes(&body.0);
            }
        }
    }
    n.fname(w, "None");
}

/// One export: class path, outer (package index), name, archetype, flags,
/// tagged properties, native tail.
#[derive(Clone)]
struct X {
    class: &'static str,
    outer: i32,
    name: String,
    archetype: i32,
    flags: u64,
    props: Vec<(&'static str, T)>,
    tail: Vec<u8>,
}

fn x(class: &'static str, outer: i32, name: &str) -> X {
    X {
        class,
        outer,
        name: name.to_owned(),
        archetype: 0,
        flags: 0x0007_0004_0000_0000,
        props: Vec::new(),
        tail: Vec::new(),
    }
}

/// Package index of export `i`.
fn e(i: usize) -> i32 {
    i as i32 + 1
}

fn build(exports: &[X]) -> Vec<u8> {
    let mut n = Names::default();
    for s in ["None", "Core", "Package", "Class"] {
        n.idx(s);
    }
    let mut imports: Vec<Import> = Vec::new();
    let mut classes: HashMap<String, i32> = HashMap::new();
    let mut packages: HashMap<String, i32> = HashMap::new();
    let mut out = Vec::new();
    for (i, s) in exports.iter().enumerate() {
        let class = match classes.get(s.class) {
            Some(&c) => c,
            None => {
                let (pkg, cls) = s.class.split_once('.').unwrap();
                let p = match packages.get(pkg) {
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
                    outer: p,
                    name: n.idx(cls),
                    number: 0,
                });
                let c = -(imports.len() as i32);
                classes.insert(s.class.to_owned(), c);
                c
            }
        };
        let mut w = W::default();
        if s.flags & HAS_STACK != 0 {
            // State frame: Node, StateNode, ProbeMask, LatentAction,
            // empty StateStack, CodeOffset (Node is non-null).
            w.i32(class);
            w.i32(class);
            w.u32(u32::MAX);
            w.u16(0);
            w.i32(0);
            w.i32(0);
        }
        w.i32(i as i32); // NetIndex
        tags(&mut n, &mut w, &s.props);
        w.bytes(&s.tail);
        let name = n.idx(&s.name);
        out.push(Export {
            class,
            super_: 0,
            outer: s.outer,
            name,
            number: 0,
            archetype: s.archetype,
            object_flags: s.flags,
            payload: w.0,
            export_flags: 0,
            net_counts: Vec::new(),
            guid: [0; 4],
            package_flags: 0,
        });
    }
    let mut synth = Synth::sample();
    synth.names = n.0.iter().map(|s| (s.clone(), 0u64)).collect();
    synth.imports = imports;
    synth.exports = out;
    synth.package_flags = 0x0002_0008;
    synth.texture_allocations = Vec::new();
    synth.additional_packages = Vec::new();
    synth.build().0
}

// ------------------------------------------------------------ native tails

fn le(w: &mut W, vals: &[i32]) {
    for v in vals {
        w.i32(*v);
    }
}

fn vec3(w: &mut W, p: [f32; 3]) {
    for c in p {
        w.bytes(&c.to_le_bytes());
    }
}

fn lightmass(w: &mut W) {
    for _ in 0..9 {
        w.u32(0);
    }
}

/// `ULevel` tail with the given actor list and model; everything else empty.
fn level_tail(owner: i32, actors: &[i32], model: i32) -> Vec<u8> {
    let mut w = W::default();
    w.i32(owner);
    w.i32(actors.len() as i32);
    le(&mut w, actors);
    // URL: 4 empty strings, no options, port, valid.
    le(&mut w, &[0, 0, 0, 0, 0, 7777, 1]);
    w.i32(model);
    le(&mut w, &[0, 0]); // ModelComponents, GameSequences
    le(&mut w, &[0, 0]); // texture maps
    w.i32(0); // skipped block
    le(&mut w, &[1, 0]); // CachedPhysBSPData
    le(&mut w, &[0, 0, 0, 0]); // SM map, SM store, per-tri map, per-tri store
    le(&mut w, &[0, 0]); // versions
    w.i32(0); // ForceStreamTextures
    le(&mut w, &[0, 0]); // convex BSP data + version
    le(&mut w, &[0; 6]); // nav/cover/pylon
    le(&mut w, &[0, 0, 0, 0]); // cover refs, links, pairs, cross-level actors
    w.u32(0); // light volume not initialized
    // Visibility: 2-float origin, 4 sizes, no buckets.
    w.bytes(&0f32.to_le_bytes());
    w.bytes(&0f32.to_le_bytes());
    le(&mut w, &[0, 0, 0, 0, 0]);
    // Distance field: max distance, box (min, max, valid), size, no voxels.
    w.bytes(&0f32.to_le_bytes());
    vec3(&mut w, [0.0; 3]);
    vec3(&mut w, [0.0; 3]);
    w.0.push(0);
    le(&mut w, &[0, 0, 0, 0]);
    w.0
}

/// Brush `UModel` tail: no BSP nodes, only the `Polys` reference.
fn brush_model_tail(polys: i32) -> Vec<u8> {
    let mut w = W::default();
    for _ in 0..7 {
        w.bytes(&0f32.to_le_bytes()); // bounds
    }
    le(&mut w, &[12, 0, 12, 0, 64, 0]); // Vectors, Points, Nodes
    w.i32(0); // Surfs owner
    w.i32(0); // Surfs
    le(&mut w, &[24, 0]); // Verts
    le(&mut w, &[0, 0]); // NumSharedSides, NumZones
    w.i32(polys);
    le(&mut w, &[4, 0, 4, 0]); // LeafHulls, Leaves
    le(&mut w, &[0, 0]); // RootOutside, Linked
    le(&mut w, &[4, 0]); // PortalNodes
    w.u32(0); // NumVertices
    le(&mut w, &[36, 0]); // VertexBuffer
    le(&mut w, &[0, 0, 0, 0]); // LightingGuid
    w.i32(0); // lightmass settings
    w.0
}

/// `UPolys` tail with one polygon per entry of `polys` (brush-local space).
fn polys_tail(owner: i32, polys: &[Vec<[f32; 3]>]) -> Vec<u8> {
    let mut w = W::default();
    w.i32(polys.len() as i32);
    w.i32(polys.len() as i32);
    w.i32(owner);
    for p in polys {
        vec3(&mut w, [0.0; 3]);
        vec3(&mut w, [0.0, 0.0, 1.0]);
        vec3(&mut w, [1.0, 0.0, 0.0]);
        vec3(&mut w, [0.0, 1.0, 0.0]);
        w.i32(p.len() as i32);
        for v in p {
            vec3(&mut w, *v);
        }
        w.u32(0); // PolyFlags
        w.i32(0); // Actor
        le(&mut w, &[0, 0]); // ItemName
        le(&mut w, &[0, -1, -1]); // Material, iLink, iBrushPoly
        w.bytes(&32f32.to_le_bytes());
        w.u32(1);
        lightmass(&mut w);
        le(&mut w, &[0, 0]); // RulesetVariation
    }
    w.0
}

const QUAD: [[f32; 3]; 4] = [
    [0.0, 0.0, 0.0],
    [10.0, 0.0, 0.0],
    [10.0, 10.0, 0.0],
    [0.0, 10.0, 0.0],
];

// ------------------------------------------------------------ fixtures

/// Indices of the base map's exports.
const WORLD: usize = 0;
const LEVEL: usize = 1;
const WORLD_INFO: usize = 2;
const BRUSH_A: usize = 3;
const MODEL: usize = 4;
const POLYS: usize = 5;
const BRUSH_B: usize = 6;
const STRAY: usize = 7;

/// TheWorld.PersistentLevel with a WorldInfo and two brushes sharing one
/// brush model, plus an actor-class export the list does not mention.
/// `actors` lists the `ULevel::Actors` entries.
fn base_map(actors: &[i32]) -> Vec<X> {
    let mut v = vec![
        x("Engine.World", 0, "TheWorld"),
        x("Engine.Level", e(WORLD), "PersistentLevel"),
        x("Engine.WorldInfo", e(LEVEL), "WorldInfo_0"),
        x("Engine.Brush", e(LEVEL), "Brush_0"),
        x("Engine.Model", e(BRUSH_A), "Model_0"),
        x("Engine.Polys", e(MODEL), "Polys_0"),
        x("Engine.Brush", e(LEVEL), "Brush_1"),
        x("Engine.Actor", e(LEVEL), "Stray_0"),
    ];
    v[LEVEL].tail = level_tail(e(LEVEL), actors, 0);
    v[WORLD_INFO].flags |= HAS_STACK;
    v[WORLD_INFO].props = vec![("KillZ", T::Float(-500.0))];
    v[BRUSH_A].flags |= HAS_STACK;
    v[BRUSH_A].props = vec![
        ("Location", T::Vector([100.0, 0.0, 0.0])),
        ("Brush", T::Obj(e(MODEL))),
    ];
    v[MODEL].tail = brush_model_tail(e(POLYS));
    v[POLYS].tail = polys_tail(e(MODEL), &[QUAD.to_vec()]);
    v[BRUSH_B].flags |= HAS_STACK;
    v[BRUSH_B].props = vec![
        ("Location", T::Vector([0.0, 200.0, 0.0])),
        ("DrawScale3D", T::Vector([2.0, 2.0, 2.0])),
        ("Brush", T::Obj(e(MODEL))),
    ];
    v
}

fn open(bytes: Vec<u8>) -> (PackageSet, Arc<LoadedPackage>) {
    let set = PackageSet::new::<&Path>(&[]);
    let pkg = Package::from_bytes(bytes).unwrap();
    let lp = set.insert_package(MAP, pkg);
    (set, lp)
}

fn scene(exports: &[X], opts: &SceneOptions) -> Scene {
    let (set, lp) = open(build(exports));
    level::extract_scene(&set, &lp, LEVEL, opts).unwrap()
}

// ------------------------------------------------------------ tests

#[test]
fn synthetic_map_scene() {
    let s = scene(
        &base_map(&[e(WORLD_INFO), e(BRUSH_A), 0, e(BRUSH_B)]),
        &SceneOptions::default(),
    );
    let st = &s.stats;
    assert_eq!(
        (st.actor_slots, st.null_slots, st.actors, st.duplicate_slots),
        (4, 1, 3, 0)
    );
    assert_eq!(st.unlisted_actors, 1, "Stray_0 is not listed");
    assert_eq!(st.foreign_actors, 0);
    assert_eq!(st.decode_failures, 0, "{:?}", s.warnings);
    assert_eq!(st.volumes_with_geometry, 2);
    assert_eq!(st.budget_skips, 0);
    // Two brushes share the model: 4 vertices + 4 (triangle bound) each.
    assert_eq!(st.geometry_elements, 16);
    let wi = s.world_info.as_ref().unwrap();
    assert_eq!(wi.kill_z, -500.0);
    let a = &s.actors[1];
    assert_eq!(
        (a.slot, a.export_index, a.name.as_str()),
        (1, BRUSH_A, "Brush_0")
    );
    assert_eq!(a.location, [100.0, 0.0, 0.0]);
    let polys = a.volume.as_ref().unwrap().polys.as_ref().unwrap();
    assert_eq!(polys.triangle_count(), 2);
    assert_eq!(polys.positions[2], [110.0, 10.0, 0.0]);
    // DrawScale3D 2 then translation.
    let b = &s.actors[2];
    let polys = b.volume.as_ref().unwrap().polys.as_ref().unwrap();
    assert_eq!(polys.positions[2], [20.0, 220.0, 0.0]);
    assert_eq!(
        b.volume.as_ref().unwrap().bounds,
        Some(([0.0, 200.0, 0.0], [20.0, 220.0, 0.0]))
    );
    assert!(a.instance.contains_key("Brush"));
}

#[test]
fn repeated_foreign_and_invalid_actor_entries() {
    // Brush_0 three times, an import, an out-of-range export, a model (not
    // inside the level) and the stray actor.
    let s = scene(
        &base_map(&[
            e(WORLD_INFO),
            e(BRUSH_A),
            e(BRUSH_A),
            -1,
            9999,
            e(BRUSH_A),
            e(MODEL),
        ]),
        &SceneOptions::default(),
    );
    let st = &s.stats;
    assert_eq!(st.duplicate_slots, 2);
    assert_eq!(st.actors, 3, "WorldInfo, Brush_0 once, Model_0");
    assert_eq!(st.foreign_actors, 1, "Model_0 lives under Brush_0");
    let listed: Vec<usize> = s.actors.iter().map(|a| a.export_index).collect();
    assert_eq!(listed, vec![WORLD_INFO, BRUSH_A, MODEL]);
    let warn = s.warnings.join("\n");
    assert!(warn.contains("listed again"), "{warn}");
    assert!(warn.contains("is not an export"), "{warn}");
    assert!(warn.contains("out of range"), "{warn}");
}

#[test]
fn broken_brush_references_are_warnings() {
    let mut m = base_map(&[e(WORLD_INFO), e(BRUSH_A), e(BRUSH_B)]);
    // Brush_0's model is the brush itself; Brush_1's model points its
    // Polys reference at the model (not a Polys export).
    m[BRUSH_A].props[1] = ("Brush", T::Obj(e(BRUSH_A)));
    m.push(x("Engine.Model", e(BRUSH_B), "Model_1"));
    let model_1 = m.len() - 1;
    m[model_1].tail = brush_model_tail(e(model_1));
    m[BRUSH_B].props[2] = ("Brush", T::Obj(e(model_1)));
    let s = scene(&m, &SceneOptions::default());
    assert_eq!(s.stats.decode_failures, 2, "{:?}", s.warnings);
    assert_eq!(s.stats.volumes_with_geometry, 0);
    assert!(
        s.actors
            .iter()
            .all(|a| a.volume.as_ref().is_none_or(|v| v.polys.is_none()))
    );
}

#[test]
fn repeated_stored_values_resolve_like_the_engine() {
    // A tag stored twice: the later value wins, as when the engine applies
    // the tags in order.
    let mut m = base_map(&[e(WORLD_INFO), e(BRUSH_A)]);
    m[BRUSH_A]
        .props
        .push(("location", T::Vector([5.0, 6.0, 7.0])));
    let s = scene(&m, &SceneOptions::default());
    assert_eq!(s.actors[1].location, [5.0, 6.0, 7.0]);
    let polys = s.actors[1].volume.as_ref().unwrap().polys.as_ref().unwrap();
    assert_eq!(polys.positions[0], [5.0, 6.0, 7.0]);
}

#[test]
fn archetype_cycles_terminate() {
    let mut m = base_map(&[e(WORLD_INFO), e(BRUSH_A), e(BRUSH_B)]);
    m[BRUSH_A].archetype = e(BRUSH_B);
    m[BRUSH_B].archetype = e(BRUSH_A);
    m[BRUSH_A].props.push(("Tag", T::Str("A".to_owned())));
    let s = scene(&m, &SceneOptions::default());
    assert_eq!(s.stats.actors, 3);
    assert!(
        s.warnings
            .iter()
            .any(|w| w.contains("archetype chain longer")),
        "{:?}",
        s.warnings
    );
    // Brush_1 inherits Brush_0's values through the cycle.
    assert_eq!(s.actors[2].location, [0.0, 200.0, 0.0]);
}

/// Many brushes share one brush model: the model is decoded once and the
/// world-space copies stop at the geometry budget.
#[test]
fn shared_brush_models_stay_within_the_geometry_budget() {
    let mut m = base_map(&[]);
    m.truncate(STRAY);
    let mut actors = vec![e(WORLD_INFO)];
    for k in 0..300 {
        let mut b = m[BRUSH_B].clone();
        b.name = format!("Brush_{}", k + 10);
        m.push(b);
        actors.push(e(m.len() - 1));
    }
    m[LEVEL].tail = level_tail(e(LEVEL), &actors, 0);
    let opts = SceneOptions {
        max_geometry: 8 * 100,
        ..SceneOptions::default()
    };
    let s = scene(&m, &opts);
    assert_eq!(s.stats.actors, 301);
    assert_eq!(s.stats.volumes_with_geometry, 100);
    assert_eq!(s.stats.budget_skips, 200);
    assert_eq!(s.stats.geometry_elements, 800);
    assert!(s.stats.geometry_elements <= opts.max_geometry);
    assert!(s.warnings.len() <= level::MAX_SCENE_WARNINGS + 1);
    assert!(
        s.warnings.iter().any(|w| w.contains("geometry budget")),
        "{:?}",
        s.warnings.iter().take(3).collect::<Vec<_>>()
    );
    // Default budget: every brush gets its geometry.
    let s = scene(&m, &SceneOptions::default());
    assert_eq!(s.stats.volumes_with_geometry, 300);
    assert_eq!(s.stats.budget_skips, 0);
}

/// Many actors inherit from one archetype with a large value: merged copies
/// stop at the merged-value budget and fall back to the stored values.
#[test]
fn shared_archetypes_stay_within_the_merged_value_budget() {
    let mut m = base_map(&[]);
    m.truncate(STRAY);
    let mut arch = x("Engine.Brush", 0, "BigArchetype");
    arch.props = vec![("Tag", T::Str("x".repeat(64 * 1024)))];
    m.push(arch);
    let arch_idx = m.len() - 1;
    let mut actors = vec![e(WORLD_INFO)];
    for k in 0..200 {
        let mut b = x("Engine.Brush", e(LEVEL), &format!("Inst_{k}"));
        b.flags |= HAS_STACK;
        b.archetype = e(arch_idx);
        b.props = vec![("Location", T::Vector([k as f32, 0.0, 0.0]))];
        m.push(b);
        actors.push(e(m.len() - 1));
    }
    m[LEVEL].tail = level_tail(e(LEVEL), &actors, 0);
    let big = SceneOptions::default();
    let s = scene(&m, &big);
    assert_eq!(s.stats.budget_skips, 0, "{:?}", s.warnings);
    let unbounded = s.stats.merged_weight;
    assert!(unbounded > 200 * 2000, "{unbounded}");
    assert_eq!(s.actors[5].tag.as_deref().map(str::len), Some(64 * 1024));

    let opts = SceneOptions {
        max_merged_weight: unbounded / 4,
        ..SceneOptions::default()
    };
    let s = scene(&m, &opts);
    assert!(s.stats.merged_weight <= opts.max_merged_weight);
    assert!(s.stats.budget_skips > 100, "{}", s.stats.budget_skips);
    // Every actor still has its own values; the late ones lost the
    // inherited string.
    assert_eq!(s.stats.actors, 201);
    assert_eq!(s.actors[200].location, [199.0, 0.0, 0.0]);
    assert_eq!(s.actors[200].tag, None);
    assert!(s.actors[1].tag.is_some());
}

/// Corrupting any byte of a synthetic map never panics, whatever the scene
/// extraction then finds.
#[test]
fn corrupted_maps_never_panic() {
    let data = build(&base_map(&[e(WORLD_INFO), e(BRUSH_A), 0, e(BRUSH_B)]));
    let mut parsed = 0usize;
    for at in 0..data.len() {
        for v in [0x00u8, 0xFF, 0x80, 0x7F, 0x01] {
            if data[at] == v {
                continue;
            }
            let mut d = data.clone();
            d[at] = v;
            let Ok(pkg) = Package::from_bytes(d) else {
                continue;
            };
            parsed += 1;
            let set = PackageSet::new::<&Path>(&[]);
            let lp = set.insert_package(MAP, pkg);
            for l in level::level_exports(&lp.package) {
                if let Ok(s) = level::extract_scene(&set, &lp, l, &SceneOptions::default()) {
                    assert!(s.stats.actors <= s.stats.actor_slots);
                }
            }
            for i in 0..lp.package.exports.len() {
                if lp.package.export_class_name(i).is_ok_and(|c| c == "Model") {
                    let _ = level::extract_bsp(&set, &lp, i);
                }
            }
        }
    }
    assert!(
        parsed > data.len(),
        "most corruptions still parse ({parsed})"
    );
}
