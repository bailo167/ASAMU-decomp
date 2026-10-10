//! Decal extraction (`decal::extract_map_decals`) end to end over a
//! synthetic map package written byte by byte here (no original game data):
//! the run-time receiver query (colliding, accepting, visible static mesh
//! components of the decal's level), collision triangles only, mirrored
//! owners, cooked receivers, unlisted and repeated actors, and corruption of
//! every byte of the package.
//!
//! The package carries no script classes, so class chains are just the
//! class names, class defaults are unavailable (every value a decal needs is
//! set on the instance) and arrays of structs cannot be decoded: the
//! editor's `DecalReceivers` list is covered by the real-data tests only.

#![allow(clippy::unwrap_used)]

mod common;

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use asamu_ue3::bulkdata::BulkDataRecord;
use asamu_ue3::decal::{
    DecalComponentNative, DecalVertex, MapDecals, ReceiverSource, StaticReceiver,
    encode_decal_component_native, extract_map_decals, face_normal,
};
use asamu_ue3::level;
use asamu_ue3::lightmap::LightMap;
use asamu_ue3::model::PackageSet;
use asamu_ue3::staticmesh::{
    BoxSphereBounds, CollisionTriangle, ColorBuffer, CompactKdopNode, KdopBounds, KdopTree,
    LodModel, MeshSection, PackedNormal, PositionBuffer, StaticMeshNative, VertexBuffer,
    encode_static_mesh_native, vertex_element_size,
};
use asamu_ue3::types::Guid;
use asamu_ue3::{LoadedPackage, Package, PackageIndex};
use common::{Export, Import, Synth, W};

const HAS_STACK: u64 = 0x0200_0000_0000_0000;
const MAP: &str = "SynthDecals";

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
    Rotator([i32; 3]),
    Obj(i32),
    Float(f32),
    Bool(bool),
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
            T::Rotator(r) => {
                n.fname(w, "StructProperty");
                w.i32(12);
                w.i32(0);
                n.fname(w, "Rotator");
                for c in r {
                    w.i32(*c);
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
            T::Bool(b) => {
                n.fname(w, "BoolProperty");
                w.i32(0);
                w.i32(0);
                w.0.push(u8::from(*b));
            }
        }
    }
    n.fname(w, "None");
}

/// One export: class path, outer (package index), name, flags, tagged
/// properties, native tail.
#[derive(Clone)]
struct X {
    class: &'static str,
    outer: i32,
    name: String,
    flags: u64,
    props: Vec<(&'static str, T)>,
    tail: Vec<u8>,
}

fn x(class: &'static str, outer: i32, name: &str) -> X {
    X {
        class,
        outer,
        name: name.to_owned(),
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
            // State frame: Node, StateNode, ProbeMask, LatentAction, empty
            // StateStack, CodeOffset (Node is non-null).
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
            archetype: 0,
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

/// `ULevel` tail with the given actor list; everything else empty.
fn level_tail(owner: i32, actors: &[i32]) -> Vec<u8> {
    let mut w = W::default();
    w.i32(owner);
    w.i32(actors.len() as i32);
    le(&mut w, actors);
    // URL: 4 empty strings, no options, port, valid.
    le(&mut w, &[0, 0, 0, 0, 0, 7777, 1]);
    w.i32(0); // Model
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

// ---------------------------------------------------------------- the mesh

/// Local positions of the wall mesh: a 100 × 100 quad on `x = 0` (vertices
/// 0–3, collision enabled) and a second quad of the same size on `x = 5`
/// (vertices 4–7, a section without collision).
fn wall_positions() -> Vec<[f32; 3]> {
    let quad = |x: f32| {
        [
            [x, -50.0, -50.0],
            [x, -50.0, 50.0],
            [x, 50.0, 50.0],
            [x, 50.0, -50.0],
        ]
    };
    quad(0.0).into_iter().chain(quad(5.0)).collect()
}

/// The wall's index triples, wound so that the outward normal
/// ([`face_normal`]) is `−X`.
fn wall_triangles(positions: &[[f32; 3]]) -> Vec<[u16; 3]> {
    let mut out = Vec::new();
    for base in [0u16, 4] {
        for t in [[0u16, 1, 2], [0, 2, 3]] {
            let mut t = t.map(|i| i + base);
            let tri = t.map(|i| positions[usize::from(i)]);
            if face_normal(tri)[0] > 0.0 {
                t.swap(1, 2);
            }
            out.push(t);
        }
    }
    out
}

fn section(first_index: u32, min: u32, max: u32, collision: u32, index: i32) -> MeshSection {
    MeshSection {
        material: PackageIndex::NULL,
        enable_collision: collision,
        old_enable_collision: collision,
        enable_shadow_casting: 1,
        first_index,
        num_triangles: 2,
        min_vertex_index: min,
        max_vertex_index: max,
        material_index: index,
        fragments: vec![],
    }
}

/// `UStaticMesh` native data of the wall: LOD 0 holds both quads, the kDOP
/// tree only the first (the collision-enabled section's triangles).
fn wall_mesh_tail() -> Vec<u8> {
    let positions = wall_positions();
    let tris = wall_triangles(&positions);
    let count = positions.len() as u32;
    let lod = LodModel {
        raw_triangles: BulkDataRecord {
            flags: 0,
            element_count: 0,
            size_on_disk: 0,
            offset_in_file: 0,
            header_offset: 0,
        },
        sections: vec![section(0, 0, 3, 1, 0), section(6, 4, 7, 0, 1)],
        positions: PositionBuffer {
            stride: 12,
            num_vertices: count,
            positions: positions.clone(),
        },
        vertices: VertexBuffer {
            num_tex_coords: 1,
            stride: vertex_element_size(1, false).unwrap() as u32,
            num_vertices: count,
            full_precision_uvs: false,
            tangent_x: vec![PackedNormal([128, 255, 128, 128]); positions.len()],
            tangent_z: vec![PackedNormal([0, 128, 128, 255]); positions.len()],
            uvs: vec![vec![[0.0, 0.0]; positions.len()]],
        },
        colors: ColorBuffer {
            stride: 0,
            num_vertices: 0,
            colors_bgra: vec![],
        },
        num_vertices: count,
        indices: tris.iter().flatten().copied().collect(),
        wireframe_indices: vec![],
        adjacency_indices: vec![],
    };
    let native = StaticMeshNative {
        start: 0,
        bounds: BoxSphereBounds {
            origin: [2.5, 0.0, 0.0],
            box_extent: [2.5, 50.0, 50.0],
            sphere_radius: 71.0,
        },
        body_setup: PackageIndex::NULL,
        kdop: KdopTree {
            root_bounds: KdopBounds {
                min: [0.0, -50.0, -50.0],
                max: [0.0, 50.0, 50.0],
            },
            nodes: vec![CompactKdopNode { bytes: [0; 6] }],
            // The first quad's triangles; the first one twice (two shipped
            // meshes repeat triangles: a repeat must not double the decal).
            triangles: [tris[0], tris[1], tris[0]]
                .into_iter()
                .map(|vertices| CollisionTriangle {
                    vertices,
                    material_index: 0,
                })
                .collect(),
        },
        internal_version: 18,
        source_data: None,
        optimization_settings: vec![],
        has_been_simplified: 0,
        is_mesh_proxy: 0,
        lods: vec![lod],
        lod_info_count: 1,
        thumbnail_angle: [0, 0, 0],
        thumbnail_distance: 0.0,
        high_res_source_mesh_name: String::new(),
        high_res_source_mesh_crc: 0,
        lighting_guid: Guid {
            a: 1,
            b: 2,
            c: 3,
            d: 4,
        },
        vertex_position_version: 1,
        cached_streaming_texture_factors: vec![],
        remove_degenerates: 0,
        per_lod_static_lighting_for_instancing: 0,
        console_prealloc_instance_count: 0,
    };
    encode_static_mesh_native(&native).unwrap()
}

// ---------------------------------------------------------------- the map

const WORLD: usize = 0;
const LEVEL: usize = 1;
const MESH: usize = 2;
/// First wall actor; wall `k` is the actor `WALLS + 2k`, its component the
/// export after it.
const WALLS: usize = 3;
const WALL_NAMES: [&str; 6] = ["Front", "Ghost", "Hidden", "Refuses", "Behind", "Other"];
const FRONT: usize = 0;
const GHOST: usize = 1;
const HIDDEN: usize = 2;
const REFUSES: usize = 3;
const OTHER_LEVEL: usize = 5;
/// First decal actor; decal `k` is the actor `DECALS + 2k`, its component
/// the export after it.
const DECALS: usize = WALLS + 2 * WALL_NAMES.len();
const STRAIGHT: usize = 0;
const MIRRORED: usize = 1;
const COOKED: usize = 2;
const DYNAMIC: usize = 3;
const UNLISTED: usize = 4;
const DECAL_COUNT: usize = 5;
const LEVEL_B: usize = DECALS + 2 * DECAL_COUNT;

fn wall_actor(k: usize) -> usize {
    WALLS + 2 * k
}

fn wall_component(k: usize) -> usize {
    WALLS + 2 * k + 1
}

fn decal_actor(k: usize) -> usize {
    DECALS + 2 * k
}

fn decal_component(k: usize) -> usize {
    DECALS + 2 * k + 1
}

/// The cooked receiver of the `COOKED` decal: one triangle in the front
/// wall's local space, partly outside the decal box.
fn cooked_tail() -> Vec<u8> {
    let vertex = |position| DecalVertex {
        position,
        tangent_x: [127, 255, 127, 127],
        tangent_z: [0, 127, 127, 255],
        light_map_coordinate: [0.0, 0.0],
    };
    // Wound like the wall itself: outward normal −X in its local space.
    let corners = [[0.0, -10.0, -10.0], [0.0, -10.0, 10.0], [0.0, 60.0, -10.0]];
    let indices = if face_normal(corners)[0] < 0.0 {
        vec![0, 1, 2]
    } else {
        vec![0, 2, 1]
    };
    let native = DecalComponentNative {
        receivers: vec![StaticReceiver {
            component: PackageIndex(e(wall_component(FRONT))),
            vertices: corners.map(vertex).to_vec(),
            indices,
            num_triangles: 1,
            light_map: LightMap::None,
            shadow_maps: vec![],
            data: 0,
            instance_index: 0,
        }],
    };
    encode_decal_component_native(&native, None).unwrap()
}

/// A map with one wall mesh placed six times and five decals at the origin
/// looking along `+X` (box 40 × 40, 0–50 UU deep):
///
/// | wall | where | what the decals see |
/// |---|---|---|
/// | Front | `x = 10`, facing the origin | the receiver |
/// | Ghost | `x = 20`, its actor does not collide | not in the collision hash |
/// | Hidden | `x = 30`, `bHidden` | hidden receivers take no decal |
/// | Refuses | `x = 40`, accepts no decals | refused |
/// | Behind | `x = −10`, turned to face the origin | only the mirrored decal |
/// | Other | `x = 15`, listed by a second level | another level's receiver |
fn decal_map(actors: &[i32]) -> Vec<X> {
    let mut v = vec![
        x("Engine.World", 0, "TheWorld"),
        x("Engine.Level", e(WORLD), "PersistentLevel"),
        x("Engine.StaticMesh", 0, "Wall"),
    ];
    v[MESH].tail = wall_mesh_tail();
    let placement: [([f32; 3], [i32; 3]); 6] = [
        ([10.0, 0.0, 0.0], [0, 0, 0]),
        ([20.0, 0.0, 0.0], [0, 0, 0]),
        ([30.0, 0.0, 0.0], [0, 0, 0]),
        ([40.0, 0.0, 0.0], [0, 0, 0]),
        ([-10.0, 0.0, 0.0], [0, 32768, 0]),
        ([15.0, 0.0, 0.0], [0, 0, 0]),
    ];
    for (k, name) in WALL_NAMES.iter().enumerate() {
        let (location, rotation) = placement[k];
        let mut actor = x("Engine.StaticMeshActor", e(LEVEL), name);
        actor.flags |= HAS_STACK;
        actor.props = vec![
            ("Location", T::Vector(location)),
            ("Rotation", T::Rotator(rotation)),
            ("bCollideActors", T::Bool(k != GHOST)),
            ("bHidden", T::Bool(k == HIDDEN)),
        ];
        let mut comp = x(
            "Engine.StaticMeshComponent",
            e(v.len()),
            &format!("{name}Mesh"),
        );
        comp.props = vec![
            ("StaticMesh", T::Obj(e(MESH))),
            ("CollideActors", T::Bool(true)),
            ("bAcceptsStaticDecals", T::Bool(k != REFUSES)),
            ("bAcceptsDynamicDecals", T::Bool(k != REFUSES)),
        ];
        // `LODData` (the component's native tail) is not read here.
        comp.tail = vec![0, 0, 0, 0];
        v.push(actor);
        v.push(comp);
    }
    for k in 0..DECAL_COUNT {
        let mut actor = x("Engine.DecalActorMovable", e(LEVEL), &format!("Decal_{k}"));
        actor.flags |= HAS_STACK;
        actor.props = vec![("Location", T::Vector([0.0, 0.0, 0.0]))];
        if k == MIRRORED {
            actor
                .props
                .push(("DrawScale3D", T::Vector([1.0, -1.0, 1.0])));
        }
        let mut comp = x("Engine.DecalComponent", e(v.len()), &format!("Decal_{k}_C"));
        comp.props = vec![
            ("Width", T::Float(40.0)),
            ("Height", T::Float(40.0)),
            ("TileX", T::Float(1.0)),
            ("TileY", T::Float(1.0)),
            ("NearPlane", T::Float(0.0)),
            ("FarPlane", T::Float(50.0)),
            ("bStaticDecal", T::Bool(k != DYNAMIC)),
            ("bProjectOnStaticMeshes", T::Bool(true)),
            ("BackfaceAngle", T::Float(0.001)),
        ];
        comp.tail = if k == COOKED {
            cooked_tail()
        } else {
            0i32.to_le_bytes().to_vec()
        };
        v.push(actor);
        v.push(comp);
    }
    // A second level that lists the "Other" wall.
    let mut level_b = x("Engine.Level", e(WORLD), "LevelB");
    level_b.tail = level_tail(e(LEVEL_B), &[e(wall_actor(OTHER_LEVEL))]);
    v.push(level_b);
    v[LEVEL].tail = level_tail(e(LEVEL), actors);
    v
}

/// The persistent level's default actor list: every wall but "Other", every
/// decal but the unlisted one.
fn default_actors() -> Vec<i32> {
    let mut a: Vec<i32> = (0..WALL_NAMES.len())
        .filter(|k| *k != OTHER_LEVEL)
        .map(|k| e(wall_actor(k)))
        .collect();
    a.extend(
        (0..DECAL_COUNT)
            .filter(|k| *k != UNLISTED)
            .map(|k| e(decal_actor(k))),
    );
    a
}

fn open(bytes: Vec<u8>) -> (PackageSet, Arc<LoadedPackage>) {
    let set = PackageSet::new::<&Path>(&[]);
    let pkg = Package::from_bytes(bytes).unwrap();
    let lp = set.insert_package(MAP, pkg);
    (set, lp)
}

fn extract(exports: &[X]) -> MapDecals {
    let (set, lp) = open(build(exports));
    extract_map_decals(&set, &lp).unwrap()
}

fn area(positions: &[[f32; 3]], triangles: &[[u32; 3]]) -> f32 {
    triangles
        .iter()
        .map(|t| {
            let [a, b, c] = t.map(|i| positions[i as usize]);
            let u = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
            let v = [c[0] - a[0], c[1] - a[1], c[2] - a[2]];
            let n = [
                u[1] * v[2] - u[2] * v[1],
                u[2] * v[0] - u[0] * v[2],
                u[0] * v[1] - u[1] * v[0],
            ];
            0.5 * (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt()
        })
        .sum()
}

fn path_of(name: &str) -> String {
    format!("{MAP}.TheWorld.PersistentLevel.{name}.{name}Mesh")
}

// ------------------------------------------------------------------ tests

#[test]
fn the_fixture_winds_the_wall_towards_minus_x() {
    let p = wall_positions();
    for t in wall_triangles(&p) {
        let n = face_normal(t.map(|i| p[usize::from(i)]));
        assert!(n[0] < -0.99, "{n:?}");
    }
}

#[test]
fn receivers_come_from_the_run_time_query() {
    let m = extract(&decal_map(&default_actors()));
    let s = &m.stats;
    assert_eq!(s.components, DECAL_COUNT);
    assert_eq!(s.components_exact, DECAL_COUNT);
    assert_eq!(s.actors, DECAL_COUNT - 1, "{:?}", m.warnings);
    assert_eq!(s.orphan_components, 1, "the unlisted actor's decal");
    assert_eq!(s.actors_with_component, s.actors);
    assert_eq!(s.mirrored, 1);
    assert_eq!(s.with_static_receivers, 1);
    assert_eq!(s.unresolved_receivers, 0);
    assert_eq!(m.decals.len(), DECAL_COUNT - 1);
    // Slots follow the actor list: four walls first.
    assert_eq!(
        m.decals.iter().map(|d| d.slot).collect::<Vec<_>>(),
        vec![5, 6, 7, 8]
    );

    // The straight decal: the front wall only, its collision quad only
    // (40 × 40 of it, one polygon per collision triangle, the repeated
    // triangle once), not the section without collision 5 UU behind it.
    let d = &m.decals[STRAIGHT];
    assert!(!d.mirrored && d.params.static_decal);
    assert_eq!(d.receivers.len(), 1, "{:?}", d.receivers);
    let r = &d.receivers[0];
    assert_eq!(r.component.as_deref(), Some(path_of("Front").as_str()));
    assert_eq!(r.source, ReceiverSource::Projected);
    assert!(!r.listed, "found by the query, not an editor list");
    assert!((area(&r.positions, &r.triangles) - 1600.0).abs() < 0.5);
    assert!(r.positions.iter().all(|p| (p[0] - 10.0).abs() < 1e-3));
    assert_eq!(r.outside, 0);
    // Texture coordinates span the unit square; normals face the decal.
    for uv in &r.uvs {
        assert!((-1e-4..=1.0 + 1e-4).contains(&uv[0]) && (-1e-4..=1.0 + 1e-4).contains(&uv[1]));
    }
    assert!(r.uvs.iter().any(|uv| uv[0] < 0.01) && r.uvs.iter().any(|uv| uv[0] > 0.99));
    assert!(r.normals.iter().all(|n| n[0] < -0.99));

    // The mirrored decal projects along −X: the wall behind, nothing else.
    let d = &m.decals[MIRRORED];
    assert!(d.mirrored);
    assert_eq!(d.frame.direction, [-1.0, 0.0, 0.0]);
    assert_eq!(d.receivers.len(), 1);
    let r = &d.receivers[0];
    assert_eq!(r.component.as_deref(), Some(path_of("Behind").as_str()));
    assert!((area(&r.positions, &r.triangles) - 1600.0).abs() < 0.5);
    assert!(r.positions.iter().all(|p| (p[0] + 10.0).abs() < 1e-3));
    assert!(r.normals.iter().all(|n| n[0] > 0.99));

    // The cooked decal keeps the cooker's receiver (clipped to the box) and
    // asks nothing else.
    let d = &m.decals[COOKED];
    assert_eq!((d.static_receivers, d.unclipped_receivers), (1, 1));
    assert_eq!(d.receivers.len(), 1);
    let r = &d.receivers[0];
    assert_eq!(r.source, ReceiverSource::Cooked);
    assert!(r.listed);
    assert_eq!(r.component.as_deref(), Some(path_of("Front").as_str()));
    // Local (0, −10..60, ±10) at the wall (x = 10): y is cut at 20.
    assert!(r.positions.iter().all(|p| (p[0] - 10.0).abs() < 1e-3));
    assert!(r.positions.iter().all(|p| p[1] <= 20.0 + 1e-3));
    assert!(r.positions.iter().any(|p| (p[1] - 20.0).abs() < 1e-3));
    assert_eq!(r.outside, 0);
    assert!(r.normals.iter().all(|n| n[0] < -0.99));

    // A dynamic (not static, not movable) decal attaches through the
    // dynamic flag: the same front wall.
    let d = &m.decals[DYNAMIC];
    assert!(!d.params.static_decal && !d.mirrored);
    assert_eq!(d.receivers.len(), 1);
    assert_eq!(
        d.receivers[0].component.as_deref(),
        Some(path_of("Front").as_str())
    );

    assert_eq!(s.with_geometry, 4);
    assert_eq!(s.world_receivers, 3);
    assert_eq!(s.receiver_meshes, 4);
    // Three projected quads and the clipped cooked triangle: a handful of
    // triangles (a polygon clipped through a box corner repeats the corner).
    assert!((8..=16).contains(&s.triangles), "{}", s.triangles);
    // Every vertex has a unit normal, also the repeated corners.
    for r in m.decals.iter().flat_map(|d| &d.receivers) {
        for n in &r.normals {
            let len = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt();
            assert!((len - 1.0).abs() < 1e-4, "{n:?}");
        }
    }
}

#[test]
fn repeated_and_foreign_actor_entries_are_taken_once() {
    // Each decal actor three times, null entries, an import, an export that
    // is no actor, an index beyond the table.
    let mut actors = default_actors();
    let d0 = e(decal_actor(STRAIGHT));
    actors.extend([d0, 0, d0, -1, e(MESH), 9999, e(decal_actor(MIRRORED))]);
    let once = extract(&decal_map(&default_actors()));
    let m = extract(&decal_map(&actors));
    assert_eq!(m.stats.actors, DECAL_COUNT - 1);
    assert_eq!(m.decals.len(), DECAL_COUNT - 1);
    assert_eq!(m.stats.triangles, once.stats.triangles);
    assert_eq!(m.decals, once.decals);
    // The second level listing a decal of the first gives it no second
    // life either.
    let mut exports = decal_map(&default_actors());
    exports[LEVEL_B].tail = level_tail(
        e(LEVEL_B),
        &[e(wall_actor(OTHER_LEVEL)), e(decal_actor(STRAIGHT))],
    );
    let m = extract(&exports);
    assert_eq!(m.decals.len(), DECAL_COUNT - 1);
}

#[test]
fn receivers_of_another_level_are_not_queried() {
    // The "Other" wall (x = 15, in front of the decals) is listed by the
    // second level only: no decal of the persistent level lands on it. Once
    // the persistent level lists it too it is the nearest receiver's
    // neighbour and takes the decals as well.
    let m = extract(&decal_map(&default_actors()));
    let other = path_of("Other");
    assert!(
        m.decals
            .iter()
            .flat_map(|d| &d.receivers)
            .all(|r| r.component.as_deref() != Some(other.as_str()))
    );
    let mut actors = default_actors();
    actors.insert(0, e(wall_actor(OTHER_LEVEL)));
    let mut exports = decal_map(&actors);
    exports[LEVEL_B].tail = level_tail(e(LEVEL_B), &[]);
    let m = extract(&exports);
    let d = &m.decals[STRAIGHT];
    let names: Vec<&str> = d
        .receivers
        .iter()
        .filter_map(|r| r.component.as_deref())
        .collect();
    assert_eq!(names, vec![path_of("Front"), other.clone()]);
    // Still not on the ghost, the hidden wall or the one that refuses.
    for skipped in ["Ghost", "Hidden", "Refuses"] {
        assert!(!names.contains(&path_of(skipped).as_str()));
    }
}

#[test]
fn hidden_receivers_take_a_decal_only_when_asked() {
    let mut exports = decal_map(&default_actors());
    exports[decal_component(STRAIGHT)]
        .props
        .push(("bProjectOnHidden", T::Bool(true)));
    let m = extract(&exports);
    let names: Vec<&str> = m.decals[STRAIGHT]
        .receivers
        .iter()
        .filter_map(|r| r.component.as_deref())
        .collect();
    assert_eq!(names, vec![path_of("Front"), path_of("Hidden")]);
    // `bProjectOnStaticMeshes` off: nothing at all.
    let mut exports = decal_map(&default_actors());
    for p in &mut exports[decal_component(STRAIGHT)].props {
        if p.0 == "bProjectOnStaticMeshes" {
            p.1 = T::Bool(false);
        }
    }
    let m = extract(&exports);
    assert!(m.decals[STRAIGHT].receivers.is_empty());
    assert_eq!(m.stats.with_geometry, 3);
}

#[test]
fn a_mirrored_receiver_takes_the_decal_on_its_outward_face() {
    // The front wall mirrored across its own plane's Y axis: it still
    // stands at x = 10 and still faces the origin, but its triangles reach
    // world space with the opposite winding.
    let plain = extract(&decal_map(&default_actors()));
    let mut exports = decal_map(&default_actors());
    exports[wall_actor(FRONT)]
        .props
        .push(("DrawScale3D", T::Vector([1.0, -1.0, 1.0])));
    let m = extract(&exports);
    let d = &m.decals[STRAIGHT];
    assert_eq!(d.receivers.len(), 1, "{:?}", d.receivers);
    let r = &d.receivers[0];
    assert_eq!(r.component.as_deref(), Some(path_of("Front").as_str()));
    assert!((area(&r.positions, &r.triangles) - 1600.0).abs() < 0.5);
    // The normals still point at the decal (the decal is lifted off the
    // surface along them).
    assert!(r.normals.iter().all(|n| n[0] < -0.99), "{:?}", r.normals);
    // The cooked receiver on the same wall is rewound too.
    let cooked = &m.decals[COOKED].receivers[0];
    assert!(!cooked.triangles.is_empty());
    assert!(cooked.normals.iter().all(|n| n[0] < -0.99));
    assert_eq!(
        plain.decals[COOKED].receivers[0].normals[0],
        cooked.normals[0]
    );
    // Mirrored across X instead, the wall turns its back: nothing.
    let mut exports = decal_map(&default_actors());
    exports[wall_actor(FRONT)]
        .props
        .push(("DrawScale3D", T::Vector([-1.0, 1.0, 1.0])));
    let m = extract(&exports);
    assert!(m.decals[STRAIGHT].receivers.is_empty());
}

#[test]
fn broken_references_are_survived() {
    // A wall whose mesh reference points at the level, a cooked receiver
    // whose component is an import, and a decal whose owner is the mesh.
    let mut exports = decal_map(&default_actors());
    for p in &mut exports[wall_component(FRONT)].props {
        if p.0 == "StaticMesh" {
            p.1 = T::Obj(e(LEVEL));
        }
    }
    let m = extract(&exports);
    assert!(m.decals[STRAIGHT].receivers.is_empty());
    // The cooked receiver still resolves its component's transform.
    assert_eq!(m.decals[COOKED].receivers.len(), 1);

    let mut exports = decal_map(&default_actors());
    let native = DecalComponentNative {
        receivers: vec![StaticReceiver {
            component: PackageIndex(-1),
            vertices: vec![],
            indices: vec![0, 1, 2, 7, 8, 9],
            num_triangles: 2,
            light_map: LightMap::None,
            shadow_maps: vec![],
            data: 0,
            instance_index: 0,
        }],
    };
    exports[decal_component(COOKED)].tail = encode_decal_component_native(&native, None).unwrap();
    let m = extract(&exports);
    let d = &m.decals[COOKED];
    assert_eq!(d.static_receivers, 1);
    assert!(d.receivers.is_empty());
    assert_eq!(d.unresolved_receivers.len(), 1);

    // Non-finite and absurd decal values give no geometry and no panic.
    for bad in [f32::NAN, f32::INFINITY, -1.0e30, 0.0] {
        let mut exports = decal_map(&default_actors());
        for p in &mut exports[decal_component(STRAIGHT)].props {
            if p.0 == "Width" || p.0 == "FarPlane" {
                p.1 = T::Float(bad);
            }
        }
        let m = extract(&exports);
        let d = &m.decals[STRAIGHT];
        assert!(
            d.receivers
                .iter()
                .flat_map(|r| &r.positions)
                .flatten()
                .all(|c| c.is_finite()),
            "{bad}"
        );
    }
}

/// Corrupting any byte of the synthetic map never panics, whatever the
/// extraction then finds, and never yields more geometry than the fixture
/// can hold.
#[test]
fn corrupted_maps_never_panic() {
    let data = build(&decal_map(&default_actors()));
    let mut parsed = 0usize;
    let mut extracted = 0usize;
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
            if let Ok(m) = extract_map_decals(&set, &lp) {
                extracted += 1;
                assert!(m.decals.len() <= DECAL_COUNT);
                assert!(m.stats.triangles <= 64, "{}", m.stats.triangles);
                for r in m.decals.iter().flat_map(|d| &d.receivers) {
                    assert_eq!(r.positions.len(), r.normals.len());
                    assert_eq!(r.positions.len(), r.uvs.len());
                    assert!(
                        r.triangles
                            .iter()
                            .flatten()
                            .all(|i| (*i as usize) < r.positions.len())
                    );
                }
            }
            let _ = level::level_exports(&lp.package);
        }
    }
    assert!(
        parsed > data.len(),
        "most corruptions still parse ({parsed})"
    );
    assert!(extracted > data.len() / 2, "{extracted}");
}
