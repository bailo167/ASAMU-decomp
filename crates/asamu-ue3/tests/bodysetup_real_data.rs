//! `RB_BodySetup` decoding and the static meshes' simple-collision switches
//! against the user's own installed game (read-only).
//!
//! Reads `ASAMU_ORIGINAL_DIR` (the folder containing
//! `A Story About My Uncle.app`) or the default macOS Steam location and
//! skips cleanly when the data is absent. Only counts and structural facts
//! are asserted; nothing is copied or written.
//!
//! Acceptance test for the "Simple collision" section of
//! `docs/reverse-engineering/MESHES.md`: every `RB_BodySetup` export of every
//! package decodes with every payload byte consumed, re-encodes byte for
//! byte, agrees with the schema-driven generic decoder value for value and
//! passes the structural cross-checks; the geometric meaning of the fields
//! and the counts quoted in the document are asserted here.
//!
//! Run with `-- --nocapture` to print the tables.

#![allow(clippy::unwrap_used)]

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use asamu_ue3::bodysetup::{
    AggGeom, BodyProperty, BodySetup, BodySetupCoverage, Matrix, Plane, ScalarValue,
    body_setup_coverage, decode_body_setup, is_affine, is_body_setup,
};
use asamu_ue3::model::{LoadedPackage, PackageSet};
use asamu_ue3::staticmesh::{SimpleCollisionFlags, decode_static_mesh, is_static_mesh};
use asamu_ue3::{Property, Value};

const COOKED: &str = "A Story About My Uncle.app/Contents/Resources/ASAMU/CookedMac";

fn cooked_dir() -> Option<PathBuf> {
    let root = match std::env::var_os("ASAMU_ORIGINAL_DIR") {
        Some(r) => PathBuf::from(r),
        None => {
            let home = std::env::var_os("HOME")?;
            PathBuf::from(home)
                .join("Library/Application Support/Steam/steamapps/common/A Story About My Uncle")
        }
    };
    let dir = root.join(COOKED);
    dir.is_dir().then_some(dir)
}

macro_rules! require_data {
    () => {
        match cooked_dir() {
            Some(d) => d,
            None => {
                eprintln!(
                    "SKIP: original game data not found (set ASAMU_ORIGINAL_DIR to the folder \
                     containing 'A Story About My Uncle.app')"
                );
                return;
            }
        }
    };
}

fn packages(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for d in [dir.to_path_buf(), dir.join("Maps")] {
        let Ok(rd) = std::fs::read_dir(&d) else {
            continue;
        };
        let mut v: Vec<PathBuf> = rd
            .flatten()
            .map(|e| e.path())
            .filter(|p| {
                p.is_file()
                    && p.extension()
                        .map(|x| x.to_string_lossy().to_ascii_lowercase())
                        .is_some_and(|x| matches!(x.as_str(), "u" | "upk" | "asamu"))
            })
            .collect();
        v.sort();
        out.extend(v);
    }
    out
}

/// Visit every package with a fresh set (keeps memory bounded: maps are large).
fn for_each_package(dir: &Path, mut f: impl FnMut(&PackageSet, &LoadedPackage)) {
    for path in packages(dir) {
        let set = PackageSet::new(&[dir.to_path_buf(), dir.join("Maps")]);
        let lp = set.open_file(&path).unwrap();
        f(&set, &lp);
    }
}

#[test]
fn every_body_setup_decodes_exactly_and_round_trips() {
    let dir = require_data!();
    let mut covs: Vec<BodySetupCoverage> = Vec::new();
    let mut scanned = 0;
    for_each_package(&dir, |_, lp| {
        scanned += 1;
        covs.push(body_setup_coverage(lp));
    });
    let mut t = BodySetupCoverage::default();
    let mut with_bodies = 0;
    for c in &covs {
        if c.total == 0 {
            continue;
        }
        with_bodies += 1;
        eprintln!(
            "{:20} bodies {:3} exact {:3} rt {:3} valid {:3} bytes {:8} owners {:?} agg {:3} empty {} \
             convex {:4} box {:2} sphere {} sphyl {} verts {:5} planes {:5} cooked {:2} blobs {:3} ({} B)",
            c.package,
            c.total,
            c.exact,
            c.round_trip,
            c.valid,
            c.payload_bytes,
            c.owners,
            c.with_agg_geom,
            c.empty_agg_geom,
            c.convex,
            c.boxes,
            c.spheres,
            c.sphyls,
            c.convex_vertices,
            c.convex_planes,
            c.with_cached_data,
            c.cached_blobs,
            c.cached_bytes
        );
        for f in &c.failures {
            eprintln!("  FAIL {f}");
        }
        for f in &c.issues {
            eprintln!("  ISSUE {f}");
        }
        t.total += c.total;
        t.exact += c.exact;
        t.round_trip += c.round_trip;
        t.valid += c.valid;
        t.payload_bytes += c.payload_bytes;
        t.with_agg_geom += c.with_agg_geom;
        t.empty_agg_geom += c.empty_agg_geom;
        t.convex += c.convex;
        t.boxes += c.boxes;
        t.spheres += c.spheres;
        t.sphyls += c.sphyls;
        t.convex_vertices += c.convex_vertices;
        t.convex_planes += c.convex_planes;
        t.with_cached_data += c.with_cached_data;
        t.cached_blobs += c.cached_blobs;
        t.cached_bytes += c.cached_bytes;
        for (k, v) in &c.owners {
            *t.owners.entry(k.clone()).or_insert(0) += v;
        }
        for (k, v) in &c.scalar_tags {
            *t.scalar_tags.entry(k.clone()).or_insert(0) += v;
        }
    }
    eprintln!(
        "TOTAL packages {scanned} (with bodies {with_bodies}) bodies {} exact {} rt {} valid {} bytes {} \
         owners {:?} agg {} empty {} convex {} box {} sphere {} sphyl {} verts {} planes {} cooked {} \
         blobs {} ({} B) scalars {:?}",
        t.total,
        t.exact,
        t.round_trip,
        t.valid,
        t.payload_bytes,
        t.owners,
        t.with_agg_geom,
        t.empty_agg_geom,
        t.convex,
        t.boxes,
        t.spheres,
        t.sphyls,
        t.convex_vertices,
        t.convex_planes,
        t.with_cached_data,
        t.cached_blobs,
        t.cached_bytes,
        t.scalar_tags
    );
    // MESHES.md, "Simple collision": every export, exactly, three ways.
    assert_eq!((scanned, with_bodies), (42, 13));
    assert_eq!(t.total, 543);
    assert_eq!((t.exact, t.round_trip, t.valid), (543, 543, 543));
    assert_eq!(t.payload_bytes, 3_750_487);
    let owners: Vec<(&str, usize)> = t.owners.iter().map(|(k, v)| (k.as_str(), *v)).collect();
    assert_eq!(
        owners,
        [("<none>", 1), ("PhysicsAsset", 34), ("StaticMesh", 508)]
    );
    // Every body but the class default object has an aggregate, none empty.
    assert_eq!((t.with_agg_geom, t.empty_agg_geom), (542, 0));
    assert_eq!((t.convex, t.boxes, t.spheres, t.sphyls), (3446, 25, 5, 6));
    assert_eq!((t.convex_vertices, t.convex_planes), (32_524, 33_625));
    assert_eq!(
        (t.with_cached_data, t.cached_blobs, t.cached_bytes),
        (12, 13, 35_885)
    );
    let scalars: Vec<(&str, usize)> = t
        .scalar_tags
        .iter()
        .map(|(k, v)| (k.as_str(), *v))
        .collect();
    assert_eq!(
        scalars,
        [
            ("BoneName", 34),
            ("MassScale", 1),
            ("PhysMaterial", 16),
            ("PreCachedPhysDataVersion", 542),
            ("bBlockNonZeroExtent", 9),
            ("bBlockZeroExtent", 9),
            ("bConsiderForBounds", 1),
            ("bNoCollision", 1),
        ]
    );
}

// ---------------------------------------------------------------------------
// Independent re-decode: the schema-driven generic decoder
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
enum Leaf {
    I(i32),
    F(u32),
    B(bool),
    U(u8),
    S(String),
    Count(usize),
}

type Flat = Vec<(String, Leaf)>;

fn flat_generic(prefix: &str, props: &[Property], out: &mut Flat) {
    for p in props {
        assert_eq!(p.array_index, 0);
        flat_value(&format!("{prefix}/{}", p.name), &p.value, out);
    }
}

fn flat_value(key: &str, v: &Value, out: &mut Flat) {
    match v {
        Value::Int(v) => out.push((key.to_owned(), Leaf::I(*v))),
        Value::Float(v) => out.push((key.to_owned(), Leaf::F(v.to_bits()))),
        Value::Bool(v) => out.push((key.to_owned(), Leaf::B(*v))),
        Value::Byte(v) => out.push((key.to_owned(), Leaf::U(*v))),
        Value::Enum(s) | Value::Name(s) => out.push((key.to_owned(), Leaf::S(s.clone()))),
        Value::Object(o) => out.push((key.to_owned(), Leaf::I(o.index))),
        Value::Array(items) => {
            out.push((format!("{key}#"), Leaf::Count(items.len())));
            for (i, it) in items.iter().enumerate() {
                flat_value(&format!("{key}[{i}]"), it, out);
            }
        }
        Value::Struct { fields, .. } => flat_generic(key, fields, out),
        other => panic!("{key}: the generic decoder kept {other:?}"),
    }
}

fn f(out: &mut Flat, key: String, v: f32) {
    out.push((key, Leaf::F(v.to_bits())));
}

fn flat_vec(out: &mut Flat, key: &str, v: [f32; 3]) {
    for (n, c) in ["X", "Y", "Z"].iter().zip(v) {
        f(out, format!("{key}/{n}"), c);
    }
}

fn flat_plane(out: &mut Flat, key: &str, p: Plane) {
    for (n, c) in ["X", "Y", "Z", "W"].iter().zip([p.x, p.y, p.z, p.w]) {
        f(out, format!("{key}/{n}"), c);
    }
}

fn flat_matrix(out: &mut Flat, key: &str, m: Option<&Matrix>) {
    let Some(m) = m else { return };
    for (n, row) in ["XPlane", "YPlane", "ZPlane", "WPlane"].iter().zip(m) {
        flat_plane(
            out,
            &format!("{key}/{n}"),
            Plane {
                x: row[0],
                y: row[1],
                z: row[2],
                w: row[3],
            },
        );
    }
}

fn flat_opt_f(out: &mut Flat, key: String, v: Option<f32>) {
    if let Some(v) = v {
        f(out, key, v);
    }
}

fn flat_opt_b(out: &mut Flat, key: String, v: Option<bool>) {
    if let Some(v) = v {
        out.push((key, Leaf::B(v)));
    }
}

fn flat_array<T>(
    out: &mut Flat,
    key: &str,
    items: Option<&Vec<T>>,
    mut each: impl FnMut(&mut Flat, &str, &T),
) {
    let Some(items) = items else { return };
    out.push((format!("{key}#"), Leaf::Count(items.len())));
    for (i, it) in items.iter().enumerate() {
        each(out, &format!("{key}[{i}]"), it);
    }
}

fn flat_agg(out: &mut Flat, key: &str, g: &AggGeom) {
    flat_array(
        out,
        &format!("{key}/SphereElems"),
        g.sphere_elems.as_ref(),
        |out, k, e| {
            flat_matrix(out, &format!("{k}/TM"), e.tm.as_ref());
            flat_opt_f(out, format!("{k}/Radius"), e.radius);
            flat_opt_b(out, format!("{k}/bNoRBCollision"), e.no_rb_collision);
            flat_opt_b(out, format!("{k}/bPerPolyShape"), e.per_poly_shape);
        },
    );
    flat_array(
        out,
        &format!("{key}/BoxElems"),
        g.box_elems.as_ref(),
        |out, k, e| {
            flat_matrix(out, &format!("{k}/TM"), e.tm.as_ref());
            flat_opt_f(out, format!("{k}/X"), e.x);
            flat_opt_f(out, format!("{k}/Y"), e.y);
            flat_opt_f(out, format!("{k}/Z"), e.z);
            flat_opt_b(out, format!("{k}/bNoRBCollision"), e.no_rb_collision);
            flat_opt_b(out, format!("{k}/bPerPolyShape"), e.per_poly_shape);
        },
    );
    flat_array(
        out,
        &format!("{key}/SphylElems"),
        g.sphyl_elems.as_ref(),
        |out, k, e| {
            flat_matrix(out, &format!("{k}/TM"), e.tm.as_ref());
            flat_opt_f(out, format!("{k}/Radius"), e.radius);
            flat_opt_f(out, format!("{k}/Length"), e.length);
            flat_opt_b(out, format!("{k}/bNoRBCollision"), e.no_rb_collision);
            flat_opt_b(out, format!("{k}/bPerPolyShape"), e.per_poly_shape);
        },
    );
    flat_array(
        out,
        &format!("{key}/ConvexElems"),
        g.convex_elems.as_ref(),
        |out, k, e| {
            let vectors = |out: &mut Flat, k: &str, v: &[f32; 3]| flat_vec(out, k, *v);
            let planes = |out: &mut Flat, k: &str, p: &Plane| flat_plane(out, k, *p);
            flat_array(
                out,
                &format!("{k}/VertexData"),
                e.vertex_data.as_ref(),
                vectors,
            );
            flat_array(
                out,
                &format!("{k}/PermutedVertexData"),
                e.permuted_vertex_data.as_ref(),
                planes,
            );
            flat_array(
                out,
                &format!("{k}/FaceTriData"),
                e.face_tri_data.as_ref(),
                |out, k, i| out.push((k.to_owned(), Leaf::I(*i))),
            );
            flat_array(
                out,
                &format!("{k}/EdgeDirections"),
                e.edge_directions.as_ref(),
                vectors,
            );
            flat_array(
                out,
                &format!("{k}/FaceNormalDirections"),
                e.face_normal_directions.as_ref(),
                vectors,
            );
            flat_array(
                out,
                &format!("{k}/FacePlaneData"),
                e.face_plane_data.as_ref(),
                planes,
            );
            if let Some(b) = &e.elem_box {
                flat_vec(out, &format!("{k}/ElemBox/Min"), b.min);
                flat_vec(out, &format!("{k}/ElemBox/Max"), b.max);
                out.push((format!("{k}/ElemBox/IsValid"), Leaf::U(b.is_valid)));
            }
        },
    );
    flat_opt_b(
        out,
        format!("{key}/bSkipCloseAndParallelChecks"),
        g.skip_close_and_parallel_checks,
    );
}

fn flat_typed(b: &BodySetup) -> Flat {
    let mut out = Flat::new();
    for p in &b.properties {
        match p {
            BodyProperty::Scalar(s) => {
                let key = format!("/{}", s.name);
                let leaf = match &s.value {
                    ScalarValue::Bool(v) => Leaf::B(*v),
                    ScalarValue::Int(v) => Leaf::I(*v),
                    ScalarValue::Float(v) => Leaf::F(v.to_bits()),
                    ScalarValue::Name { text, .. } => Leaf::S(text.clone()),
                    ScalarValue::Object(o) => Leaf::I(o.0),
                    ScalarValue::Byte { value, .. } => Leaf::U(*value),
                    ScalarValue::Enum { value, .. } => Leaf::S(value.clone()),
                };
                out.push((key, leaf));
            }
            BodyProperty::PreCachedPhysScale(v) => {
                flat_array(&mut out, "/PreCachedPhysScale", Some(v), |out, k, s| {
                    flat_vec(out, k, *s);
                });
            }
            BodyProperty::ComNudge(v) => flat_vec(&mut out, "/COMNudge", *v),
            BodyProperty::AggGeom(g) => flat_agg(&mut out, "/AggGeom", g),
        }
    }
    out
}

#[test]
fn typed_decoder_agrees_with_the_generic_decoder() {
    let dir = require_data!();
    let (mut total, mut values) = (0usize, 0usize);
    for_each_package(&dir, |set, lp| {
        let pkg = &lp.package;
        for i in 0..pkg.exports.len() {
            if !is_body_setup(pkg, i) {
                continue;
            }
            total += 1;
            let typed = decode_body_setup(pkg, i).unwrap();
            let generic = set.decode(lp, i).unwrap();
            assert!(
                generic.warnings.is_empty(),
                "{}: {:?}",
                generic.path,
                generic.warnings
            );
            assert_eq!(generic.class, "Engine.RB_BodySetup");
            assert_eq!(generic.prelude.net_index, typed.net_index);
            assert!(generic.prelude.state_frame.is_none() && generic.prelude.component.is_none());
            assert_eq!(
                generic.properties_end, typed.native_start,
                "{}",
                generic.path
            );
            let mut a = Flat::new();
            flat_generic("", &generic.properties, &mut a);
            let mut b = flat_typed(&typed);
            a.sort();
            b.sort();
            values += a.len();
            assert!(a == b, "{}: typed and generic decodes differ", generic.path);
        }
    });
    eprintln!("generic comparison: {total} bodies, {values} values equal");
    assert_eq!((total, values), (543, 756_768));
}

// ---------------------------------------------------------------------------
// Meaning of the fields
// ---------------------------------------------------------------------------

fn dot(a: [f32; 3], b: [f32; 3]) -> f64 {
    f64::from(a[0]) * f64::from(b[0])
        + f64::from(a[1]) * f64::from(b[1])
        + f64::from(a[2]) * f64::from(b[2])
}

fn len(a: [f32; 3]) -> f64 {
    dot(a, a).sqrt()
}

#[derive(Default, Debug)]
struct GeomStats {
    convex: usize,
    planes: usize,
    unit_normals: usize,
    planes_with_3_on: usize,
    tris: usize,
    tris_on_a_plane: usize,
    tris_outward: usize,
    tris_inward: usize,
    edge_dirs: usize,
    unit_edge_dirs: usize,
    normal_dirs: usize,
    normal_dirs_matching_a_plane: usize,
    planes_matching_a_normal_dir: usize,
    edge_dirs_matching_an_edge: usize,
    empty_edge_dirs: usize,
    tms: usize,
    affine: usize,
    identity: usize,
    orthonormal: usize,
    min_verts: usize,
    max_verts: usize,
    /// Planes by how far the element's farthest vertex lies outside them (uu).
    planes_outside_over_0_01: usize,
    planes_outside_over_1: usize,
    convex_with_a_vertex_outside: usize,
    max_outside_uu: f64,
    closed_surfaces: usize,
    /// Convex elements by `vertices % 4` (the size of the last permuted group).
    last_group: [usize; 4],
    /// Boxes, spheres and capsules, and those with either flag set.
    other_shapes: usize,
    flagged_shapes: usize,
}

fn tm_stats(s: &mut GeomStats, m: &Matrix) {
    s.tms += 1;
    if is_affine(m) {
        s.affine += 1;
    }
    let rows: Vec<[f32; 3]> = m.iter().take(3).map(|r| [r[0], r[1], r[2]]).collect();
    let ortho = (0..3).all(|i| (len(rows[i]) - 1.0).abs() < 1e-4)
        && dot(rows[0], rows[1]).abs() < 1e-4
        && dot(rows[0], rows[2]).abs() < 1e-4
        && dot(rows[1], rows[2]).abs() < 1e-4;
    if ortho {
        s.orthonormal += 1;
    }
    let identity = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];
    if rows == identity {
        s.identity += 1;
    }
}

#[test]
fn shapes_have_the_documented_meaning() {
    let dir = require_data!();
    let mut s = GeomStats {
        min_verts: usize::MAX,
        ..GeomStats::default()
    };
    // (radius, mesh half extent, mesh sphere radius, translation) of bodies
    // that are a single sphere.
    let mut sphere_meshes: Vec<(f32, [f32; 3], f32, [f32; 3])> = Vec::new();
    let mut sphere_mesh_switches = Vec::new();
    for_each_package(&dir, |set, lp| {
        let pkg = &lp.package;
        for i in 0..pkg.exports.len() {
            if !is_body_setup(pkg, i) {
                continue;
            }
            let b = decode_body_setup(pkg, i).unwrap();
            let Some(g) = b.agg_geom() else { continue };
            let mut flags = |a: Option<bool>, b: Option<bool>| {
                s.other_shapes += 1;
                if a.unwrap() || b.unwrap() {
                    s.flagged_shapes += 1;
                }
            };
            for e in g.spheres() {
                flags(e.no_rb_collision, e.per_poly_shape);
            }
            for e in g.sphyls() {
                flags(e.no_rb_collision, e.per_poly_shape);
            }
            for e in g.boxes() {
                flags(e.no_rb_collision, e.per_poly_shape);
            }
            for e in g.spheres() {
                tm_stats(&mut s, e.tm.as_ref().unwrap());
            }
            for e in g.sphyls() {
                tm_stats(&mut s, e.tm.as_ref().unwrap());
            }
            for e in g.boxes() {
                tm_stats(&mut s, e.tm.as_ref().unwrap());
            }
            // A mesh whose whole body is one sphere: compare it with the
            // mesh bounds.
            let owner = pkg.export(i).unwrap().outer_index.export_index();
            if g.element_count() == 1
                && g.spheres().len() == 1
                && let Some(o) = owner
                && is_static_mesh(pkg, o)
            {
                let e = &g.spheres()[0];
                let m = e.tm.unwrap();
                let mesh = decode_static_mesh(pkg, Some(&lp.name), o, set).unwrap();
                sphere_mesh_switches.push(mesh.simple_collision_flags());
                sphere_meshes.push((
                    e.radius.unwrap(),
                    mesh.native.bounds.box_extent,
                    mesh.native.bounds.sphere_radius,
                    [m[3][0], m[3][1], m[3][2]],
                ));
            }
            for c in g.convex() {
                s.convex += 1;
                let verts = c.vertex_data.as_ref().unwrap();
                let planes = c.face_plane_data.as_ref().unwrap();
                let tris = c.face_tri_data.as_ref().unwrap();
                let edges = c.edge_directions.as_ref().unwrap();
                let normals = c.face_normal_directions.as_ref().unwrap();
                s.min_verts = s.min_verts.min(verts.len());
                s.max_verts = s.max_verts.max(verts.len());
                s.last_group[verts.len() % 4] += 1;
                let scale = verts.iter().map(|v| len(*v)).fold(1.0f64, f64::max);
                let tol = 2e-4 * scale;
                let mut worst = 0.0f64;
                // A closed triangulated surface without holes has 2V - 4 triangles.
                if tris.len() / 3 + 4 == 2 * verts.len() {
                    s.closed_surfaces += 1;
                }
                for p in planes {
                    s.planes += 1;
                    let n = [p.x, p.y, p.z];
                    if (len(n) - 1.0).abs() < 1e-3 {
                        s.unit_normals += 1;
                    }
                    let mut on = 0;
                    let mut outside = 0.0f64;
                    for v in verts {
                        let d = dot(n, *v) - f64::from(p.w);
                        outside = outside.max(d);
                        if d.abs() < tol {
                            on += 1;
                        }
                    }
                    worst = worst.max(outside);
                    if outside > 0.01 {
                        s.planes_outside_over_0_01 += 1;
                    }
                    if outside > 1.0 {
                        s.planes_outside_over_1 += 1;
                    }
                    if on >= 3 {
                        s.planes_with_3_on += 1;
                    }
                    if normals
                        .iter()
                        .any(|d| (dot(*d, n).abs() - 1.0).abs() < 1e-3)
                    {
                        s.planes_matching_a_normal_dir += 1;
                    }
                }
                let mut hull_edges: Vec<[f32; 3]> = Vec::new();
                for t in tris.as_chunks::<3>().0 {
                    s.tris += 1;
                    let v: Vec<[f32; 3]> = t
                        .iter()
                        .map(|&i| verts[usize::try_from(i).unwrap()])
                        .collect();
                    let on_plane = planes.iter().find(|p| {
                        v.iter()
                            .all(|x| (dot([p.x, p.y, p.z], *x) - f64::from(p.w)).abs() < tol)
                    });
                    if let Some(p) = on_plane {
                        s.tris_on_a_plane += 1;
                        let e1 = [v[1][0] - v[0][0], v[1][1] - v[0][1], v[1][2] - v[0][2]];
                        let e2 = [v[2][0] - v[0][0], v[2][1] - v[0][1], v[2][2] - v[0][2]];
                        let cross = [
                            e1[1] * e2[2] - e1[2] * e2[1],
                            e1[2] * e2[0] - e1[0] * e2[2],
                            e1[0] * e2[1] - e1[1] * e2[0],
                        ];
                        let d = dot(cross, [p.x, p.y, p.z]);
                        if d > 0.0 {
                            s.tris_outward += 1;
                        } else if d < 0.0 {
                            s.tris_inward += 1;
                        }
                    }
                    for k in 0..3 {
                        let (a, b) = (v[k], v[(k + 1) % 3]);
                        hull_edges.push([b[0] - a[0], b[1] - a[1], b[2] - a[2]]);
                    }
                }
                if edges.is_empty() {
                    s.empty_edge_dirs += 1;
                }
                for d in edges {
                    s.edge_dirs += 1;
                    if (len(*d) - 1.0).abs() < 1e-3 {
                        s.unit_edge_dirs += 1;
                    }
                    if hull_edges.iter().any(|e| {
                        let l = len(*e);
                        l > 0.0 && (dot(*e, *d).abs() / l - 1.0).abs() < 1e-3
                    }) {
                        s.edge_dirs_matching_an_edge += 1;
                    }
                }
                for d in normals {
                    s.normal_dirs += 1;
                    if planes
                        .iter()
                        .any(|p| (dot(*d, [p.x, p.y, p.z]).abs() - 1.0).abs() < 1e-3)
                    {
                        s.normal_dirs_matching_a_plane += 1;
                    }
                }
                s.max_outside_uu = s.max_outside_uu.max(worst);
                if worst > 0.01 {
                    s.convex_with_a_vertex_outside += 1;
                }
            }
        }
    });
    eprintln!("{s:#?}");
    eprintln!("single-sphere mesh bodies: {sphere_meshes:?}");

    assert_eq!(s.convex, 3446);
    assert_eq!((s.min_verts, s.max_verts), (4, 79));
    // Vertices in the last permuted group: a full one on 1,859 elements,
    // then 1, 2 and 3 (padded by repeating the group's first vertex; the
    // rule itself is checked bit for bit by `validate_body_setup`).
    assert_eq!(s.last_group, [1859, 640, 593, 354]);
    // No box, sphere or capsule has `bNoRBCollision` or `bPerPolyShape` set.
    assert_eq!((s.other_shapes, s.flagged_shapes), (36, 0));
    // Planes: unit normals, each through at least three of the element's
    // vertices (within 2e-4 of the element's size).
    assert_eq!(
        (s.planes, s.unit_normals, s.planes_with_3_on),
        (33_625, 33_625, 33_625)
    );
    // FaceTriData: a closed surface on all but one element; each triangle
    // lies in one of the element's planes on 50,427 of 51,262, and is wound
    // clockwise seen from outside (cross product against the plane normal).
    assert_eq!(s.closed_surfaces, 3445);
    assert_eq!((s.tris, s.tris_on_a_plane), (51_262, 50_427));
    assert_eq!((s.tris_inward, s.tris_outward), (50_305, 122));
    // The direction lists: unit vectors, every edge direction parallel to a
    // triangle edge, every normal direction parallel to a plane normal and
    // every plane normal listed (up to sign).
    assert_eq!(
        (s.edge_dirs, s.unit_edge_dirs, s.edge_dirs_matching_an_edge),
        (43_867, 43_867, 43_867)
    );
    assert_eq!(s.empty_edge_dirs, 2);
    assert_eq!(
        (
            s.normal_dirs,
            s.normal_dirs_matching_a_plane,
            s.planes_matching_a_normal_dir
        ),
        (28_200, 28_200, 33_625)
    );
    // The elements are NOT all convex with respect to their own planes: on
    // 584 of them a vertex lies more than 0.01 uu outside a plane.
    assert_eq!(s.convex_with_a_vertex_outside, 584);
    assert_eq!(
        (s.planes_outside_over_0_01, s.planes_outside_over_1),
        (1364, 125)
    );
    assert!(s.max_outside_uu > 160.0 && s.max_outside_uu < 161.0);
    // Transforms: affine, orthonormal axes (1e-4), 16 of 36 unrotated.
    assert_eq!(
        (s.tms, s.affine, s.orthonormal, s.identity),
        (36, 36, 36, 16)
    );
    // The one mesh whose body is a sphere: the radius is the mesh's half
    // extent and the sphere sits at the mesh origin.
    assert_eq!(sphere_meshes.len(), 1);
    for (radius, extent, sphere_radius, origin) in &sphere_meshes {
        assert_eq!(*radius, 160.0);
        assert_eq!(*extent, [160.0; 3]);
        assert!((sphere_radius - 160.0).abs() < 1e-3);
        assert!(origin.iter().all(|c| c.abs() < 1e-3));
    }
    // Its line switch is stored off, its box switch is at the default.
    assert_eq!(
        sphere_mesh_switches,
        [SimpleCollisionFlags {
            line: false,
            box_: true,
            rigid_body: true
        }]
    );
}

#[test]
fn class_default_object_stores_the_four_class_defaults() {
    let dir = require_data!();
    let mut found = 0;
    for_each_package(&dir, |_, lp| {
        let pkg = &lp.package;
        for i in 0..pkg.exports.len() {
            if !is_body_setup(pkg, i) || pkg.export(i).unwrap().object_flags & 0x200 == 0 {
                continue;
            }
            found += 1;
            assert_eq!(lp.name, "Engine");
            let b = decode_body_setup(pkg, i).unwrap();
            // No native data and no aggregate.
            assert!(b.pre_cached_phys_data.is_none() && b.agg_geom().is_none());
            let scalars: Vec<(&str, &ScalarValue)> =
                b.scalars().map(|s| (s.name.as_str(), &s.value)).collect();
            assert_eq!(
                scalars,
                [
                    ("bBlockZeroExtent", &ScalarValue::Bool(true)),
                    ("bBlockNonZeroExtent", &ScalarValue::Bool(true)),
                    ("bConsiderForBounds", &ScalarValue::Bool(true)),
                    ("MassScale", &ScalarValue::Float(1.0)),
                ]
            );
        }
    });
    assert_eq!(found, 1);
}

// ---------------------------------------------------------------------------
// Static meshes: the switches and the body reference
// ---------------------------------------------------------------------------

#[test]
fn static_mesh_switches_and_bodies() {
    let dir = require_data!();
    #[derive(Default, Debug)]
    struct Counts {
        meshes: usize,
        stored: BTreeMap<String, usize>,
        with_body: usize,
        tagged_body: usize,
        body_shapes: BTreeMap<String, usize>,
    }
    let mut all = Counts::default();
    // First copy of each object path: (has body, stored box, stored line, stored rb, shapes).
    type Row = (bool, Option<bool>, Option<bool>, Option<bool>, usize);
    let mut unique: BTreeMap<String, Row> = BTreeMap::new();
    let mut differing: BTreeSet<String> = BTreeSet::new();
    for_each_package(&dir, |set, lp| {
        let pkg = &lp.package;
        for i in 0..pkg.exports.len() {
            if !is_static_mesh(pkg, i) {
                continue;
            }
            let mesh = decode_static_mesh(pkg, Some(&lp.name), i, set).unwrap();
            all.meshes += 1;
            let stored = [
                (
                    "UseSimpleLineCollision",
                    mesh.stored_use_simple_line_collision(),
                ),
                (
                    "UseSimpleBoxCollision",
                    mesh.stored_use_simple_box_collision(),
                ),
                (
                    "UseSimpleRigidBodyCollision",
                    mesh.stored_use_simple_rigid_body_collision(),
                ),
            ];
            for (name, v) in stored {
                if let Some(v) = v {
                    *all.stored.entry(format!("{name}={v}")).or_insert(0) += 1;
                }
            }
            let body = mesh.native.body_setup;
            // The tagged reference and the native one are the same object.
            assert_eq!(
                mesh.tagged_body_setup().unwrap_or_default(),
                body,
                "{}",
                mesh.object.path
            );
            if mesh.tagged_body_setup().is_some() {
                all.tagged_body += 1;
            }
            let mut shapes = 0;
            if !body.is_null() {
                all.with_body += 1;
                let b = body.export_index().expect("body setup is an export");
                assert!(is_body_setup(pkg, b));
                // The body setup is the mesh's own subobject.
                assert_eq!(pkg.export(b).unwrap().outer_index.export_index(), Some(i));
                let decoded = decode_body_setup(pkg, b).unwrap();
                let g = decoded.agg_geom().expect("mesh body has an AggGeom");
                shapes = g.element_count();
                let kind = format!(
                    "convex {} box {} sphere {} sphyl {}",
                    usize::from(!g.convex().is_empty()),
                    usize::from(!g.boxes().is_empty()),
                    usize::from(!g.spheres().is_empty()),
                    usize::from(!g.sphyls().is_empty())
                );
                *all.body_shapes.entry(kind).or_insert(0) += 1;
            }
            let row: Row = (
                !body.is_null(),
                mesh.stored_use_simple_box_collision(),
                mesh.stored_use_simple_line_collision(),
                mesh.stored_use_simple_rigid_body_collision(),
                shapes,
            );
            match unique.get(&mesh.object.path) {
                None => {
                    unique.insert(mesh.object.path.clone(), row);
                }
                Some(first) if *first != row => {
                    differing.insert(mesh.object.path.clone());
                }
                Some(_) => {}
            }
        }
    });
    eprintln!("{all:#?}");
    let mut classes: BTreeMap<String, usize> = BTreeMap::new();
    for (has_body, bx, line, rb, shapes) in unique.values() {
        let key = format!(
            "body {} shapes>0 {} box {:?} line {:?} rb {:?}",
            has_body,
            *shapes > 0,
            bx,
            line,
            rb
        );
        *classes.entry(key).or_insert(0) += 1;
    }
    eprintln!(
        "unique paths {} differing copies {}",
        unique.len(),
        differing.len()
    );
    for (k, v) in &classes {
        eprintln!("  {v:4} {k}");
    }

    // Exports: the switches are only ever stored as false.
    assert_eq!(all.meshes, 1512);
    let stored: Vec<(&str, usize)> = all.stored.iter().map(|(k, v)| (k.as_str(), *v)).collect();
    assert_eq!(
        stored,
        [
            ("UseSimpleBoxCollision=false", 80),
            ("UseSimpleLineCollision=false", 69),
            ("UseSimpleRigidBodyCollision=false", 66),
        ]
    );
    // 508 meshes have a body; it is tagged and stored natively alike, is the
    // mesh's own subobject, and holds convex elements (507) or a sphere (1).
    assert_eq!((all.with_body, all.tagged_body), (508, 508));
    let kinds: Vec<(&str, usize)> = all
        .body_shapes
        .iter()
        .map(|(k, v)| (k.as_str(), *v))
        .collect();
    assert_eq!(
        kinds,
        [
            ("convex 0 box 0 sphere 1 sphyl 0", 1),
            ("convex 1 box 0 sphere 0 sphyl 0", 507),
        ]
    );

    // Distinct object paths: every copy of a path agrees.
    assert_eq!((unique.len(), differing.len()), (882, 0));
    let count = |f: &dyn Fn(&Row) -> bool| unique.values().filter(|r| f(r)).count();
    // The numbers of PARITY_FINDINGS.md (V5).
    assert_eq!(count(&|r| r.0), 316, "meshes with a body");
    assert_eq!(count(&|r| r.0 && r.4 == 0), 0, "bodies without any shape");
    assert_eq!(
        count(&|r| !r.0 && r.1.is_none()),
        515,
        "no body, box switch at its default"
    );
    assert_eq!(
        count(&|r| r.0 && r.1.is_none()),
        297,
        "body, box switch at its default"
    );
    assert_eq!(
        count(&|r| r.1 == Some(false)),
        70,
        "box switch stored false"
    );
    assert_eq!(
        count(&|r| r.1 == Some(false) && r.0),
        19,
        "... of which with a body"
    );
    assert_eq!(count(&|r| r.1 == Some(true)), 0);
    assert_eq!(
        count(&|r| r.2 == Some(false)),
        65,
        "line switch stored false"
    );
    assert_eq!(
        count(&|r| r.2 == Some(false) && r.0),
        19,
        "... of which with a body"
    );
    assert_eq!(
        count(&|r| !r.0 && r.2.is_none()),
        520,
        "no body, line switch at its default"
    );
    assert_eq!(count(&|r| r.2 == Some(true)), 0);
    assert_eq!(
        count(&|r| r.3 == Some(false)),
        57,
        "rigid-body switch stored false"
    );
    assert_eq!(count(&|r| r.3 == Some(true)), 0);
}

/// The shapes are in the mesh's own local space: every body's shapes are
/// compared with the bounds stored in its mesh (`Bounds`: origin and box
/// extent, which is the render vertices' box). The excess is how far a
/// convex vertex (or a sphere's extreme point) lies outside that box,
/// relative to the box's largest edge.
///
/// The counts were first measured by a decoder written separately in Python
/// (local, not committed) and are asserted here against this crate's
/// decoders. Simple collision is authored by hand, so it is loose on some
/// meshes; what the numbers establish is the frame (a swapped axis, a
/// mirrored axis or a scale would move most hulls out of their boxes).
#[test]
fn shapes_sit_in_their_meshes_bounds() {
    let dir = require_data!();
    // First copy of each object path -> relative excess (every copy of a
    // path has the same shapes: `static_mesh_switches_and_bodies` and the
    // importer's gated test).
    let mut excess: BTreeMap<String, (f64, bool)> = BTreeMap::new();
    for_each_package(&dir, |set, lp| {
        let pkg = &lp.package;
        for i in 0..pkg.exports.len() {
            if !is_static_mesh(pkg, i) {
                continue;
            }
            let mesh = decode_static_mesh(pkg, Some(&lp.name), i, set).unwrap();
            let Some(b) = mesh.native.body_setup.export_index() else {
                continue;
            };
            if excess.contains_key(&mesh.object.path) {
                continue;
            }
            let body = decode_body_setup(pkg, b).unwrap();
            let g = body.agg_geom().unwrap();
            // No mesh body has a box or a capsule.
            assert!(g.boxes().is_empty() && g.sphyls().is_empty());
            let mut points: Vec<[f64; 3]> = Vec::new();
            for c in g.convex() {
                points.extend(
                    c.vertex_data
                        .as_ref()
                        .unwrap()
                        .iter()
                        .map(|v| v.map(f64::from)),
                );
            }
            for s in g.spheres() {
                let m = s.tm.unwrap();
                let r = f64::from(s.radius.unwrap());
                let c = [m[3][0], m[3][1], m[3][2]].map(f64::from);
                points.push(c.map(|x| x - r));
                points.push(c.map(|x| x + r));
            }
            assert!(!points.is_empty(), "{}", mesh.object.path);
            let origin = mesh.native.bounds.origin.map(f64::from);
            let extent = mesh.native.bounds.box_extent.map(f64::from);
            let mut worst = f64::NEG_INFINITY;
            for p in &points {
                for k in 0..3 {
                    worst = worst
                        .max((origin[k] - extent[k]) - p[k])
                        .max(p[k] - (origin[k] + extent[k]));
                }
            }
            let size = 2.0 * extent.iter().copied().fold(0.0f64, f64::max);
            assert!(size > 0.0, "{}", mesh.object.path);
            let box_on = mesh.simple_collision_flags().box_;
            excess.insert(mesh.object.path.clone(), (worst / size, box_on));
        }
    });
    let count = |f: &dyn Fn(f64, bool) -> bool| excess.values().filter(|(e, b)| f(*e, *b)).count();
    let mut sorted: Vec<f64> = excess.values().map(|(e, _)| *e).collect();
    sorted.sort_by(f64::total_cmp);
    eprintln!(
        "mesh bodies {} | excess <= 1% {} | 1-5% {} | > 5% {} | > 50% {} | median {:.5} | \
         box switch on: {} bodies, {} over 1%, {} over 5%",
        excess.len(),
        count(&|e, _| e <= 0.01),
        count(&|e, _| e > 0.01 && e <= 0.05),
        count(&|e, _| e > 0.05),
        count(&|e, _| e > 0.5),
        sorted[sorted.len() / 2],
        count(&|_, b| b),
        count(&|e, b| b && e > 0.01),
        count(&|e, b| b && e > 0.05),
    );
    for (path, (e, b)) in &excess {
        if *e > 0.05 {
            eprintln!("  {e:9.3} box switch {b:5} {path}");
        }
    }
    assert_eq!(excess.len(), 316);
    assert_eq!(count(&|e, _| e <= 0.01), 243);
    assert_eq!(count(&|e, _| e > 0.01 && e <= 0.05), 60);
    assert_eq!(count(&|e, _| e > 0.05), 13);
    // One mesh is a collision-only asset: a 21 uu placeholder whose hulls
    // span thousands of units. Every other body stays within 40% of its
    // mesh's size.
    assert_eq!(count(&|e, _| e > 0.5), 1);
    assert_eq!(count(&|e, _| e > 0.4), 1);
    // Half of the bodies leave their box by less than 0.02% of its size.
    assert!(sorted[sorted.len() / 2] < 2e-4);
    // The 297 bodies a swept trace uses (box switch on).
    assert_eq!(
        (
            count(&|_, b| b),
            count(&|e, b| b && e > 0.01),
            count(&|e, b| b && e > 0.05)
        ),
        (297, 67, 12)
    );
}
