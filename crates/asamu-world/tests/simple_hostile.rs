//! Hostile simple-collision data: shapes documents and manifest records that
//! are broken, absurd or enormous. Converted files are untrusted input like
//! any other; the loader must answer every one of them with a map that
//! loads, a warning where collision was lost or replaced, and queries that
//! end with finite answers. Nothing here may panic or run away.
//!
//! All data is written by these tests; no game data is involved.

use asamu_world::collision::{PrimitiveKind, QueryFilter};
use asamu_world::fixtures::{MeshFixtures, Place, SceneFixture, box_mesh, convex_element, json};
use asamu_world::scene::{self, DataSource, LoadOptions, LoadedMap, MemorySource};
use glam::Vec3;

const MAP: &str = "AG-Hostile";
const MESH: &str = "Pkg.Meshes.Block";
const SHAPES: &str = "meshes/AG-Hostile/Pkg_Meshes_Block.collision.json";
const MANIFEST: &str = "meshes/manifest.json";
/// The player's box.
const E: Vec3 = Vec3::new(21.0, 21.0, 44.0);

/// A block `[-100, 100]² × [0, 100]` whose body is its lower half, placed
/// at the origin and (scaled by 3) at x = 2000.
fn fixture() -> (SceneFixture, MeshFixtures) {
    let mut s = SceneFixture::new(MAP, -50_000.0);
    s.static_mesh(MESH, Place::at(Vec3::ZERO));
    s.static_mesh(
        MESH,
        Place {
            location: Vec3::new(2000.0, 0.0, 0.0),
            rotation: [0; 3],
            scale: Vec3::splat(3.0),
        },
    );
    let mut m = MeshFixtures::new();
    let (v, t) = box_mesh(
        Vec3::new(-100.0, -100.0, 0.0),
        Vec3::new(100.0, 100.0, 100.0),
    );
    m.add(MESH, MAP, v, t);
    let (hv, ht) = box_mesh(
        Vec3::new(-100.0, -100.0, 0.0),
        Vec3::new(100.0, 100.0, 50.0),
    );
    m.add_convex(MESH, &hv, &ht);
    (s, m)
}

fn source(s: &SceneFixture, m: &MeshFixtures) -> MemorySource {
    let mut src = MemorySource::new();
    s.write(&mut src);
    m.write_v2(&mut src, true);
    src
}

fn load(src: &MemorySource) -> LoadedMap {
    scene::load_map(src, MAP, &LoadOptions::default()).expect("the map still loads")
}

/// Height of the box's centre where a drop at `(x, 0)` from 900 to −900
/// ends, with the kind of what stopped it; the hierarchy and the reference
/// must agree.
fn drop_at(map: &LoadedMap, x: f32) -> Option<(f64, PrimitiveKind)> {
    let start = Vec3::new(x, 0.0, 900.0);
    let end = Vec3::new(x, 0.0, -900.0);
    let hit = map
        .collision
        .sweep_box(&map.dynamic, start, end, E, QueryFilter::PAWN);
    let brute = map
        .collision
        .sweep_box_brute_force(&map.dynamic, start, end, E, QueryFilter::PAWN);
    assert_eq!(hit, brute);
    hit.map(|h| {
        assert!(h.t.is_finite() && (0.0..=1.0).contains(&h.t));
        assert!(h.normal.is_finite());
        (h.point(start, end).z, h.kind)
    })
}

/// Sweeps, traces and overlap tests through and around everything: each one
/// must end, and every answer must be finite.
fn storm(map: &LoadedMap) {
    for i in 0..24 {
        for j in 0..24 {
            let a = Vec3::new(-600.0 + 130.0 * i as f32, -300.0 + 25.0 * j as f32, 700.0);
            let b = Vec3::new(2600.0 - 120.0 * j as f32, 280.0 - 23.0 * i as f32, -150.0);
            for filter in [QueryFilter::PAWN, QueryFilter::TRACE, QueryFilter::LINE] {
                if let Some(h) = map.collision.sweep_box(&map.dynamic, a, b, E, filter) {
                    assert!(h.t.is_finite() && (0.0..=1.0).contains(&h.t));
                    assert!(h.normal.is_finite());
                }
                if let Some(h) = map.collision.raycast(&map.dynamic, a, b, filter) {
                    assert!(h.t.is_finite() && (0.0..=1.0).contains(&h.t));
                }
                let _ = map
                    .collision
                    .overlaps_box(&map.dynamic, a.lerp(b, 0.37), E, filter);
            }
            let _ = map
                .collision
                .sweep_cylinder(&map.dynamic, a, b, 21.0, 44.0, QueryFilter::PAWN);
        }
    }
}

/// The intact fixture: the box rests on the body's top, 50 + 44 at the
/// origin and 3 × 50 + 44 on the copy scaled by 3; a bullet ends on the
/// triangles at 100 and 300.
#[test]
fn the_intact_fixture_is_met_at_its_body() {
    let (s, m) = fixture();
    let map = load(&source(&s, &m));
    assert!(map.warnings.is_empty(), "{:?}", map.warnings);
    assert_eq!(drop_at(&map, 0.0), Some((94.0, PrimitiveKind::SimpleHull)));
    assert_eq!(
        drop_at(&map, 2000.0),
        Some((194.0, PrimitiveKind::SimpleHull))
    );
    storm(&map);
}

/// Text that is not a shapes document at all: `NaN`, `Infinity` and `null`
/// where a number belongs, numbers too large for an `f32`, a truncated
/// file, an empty one, another top-level type, and deep nesting. JSON has
/// no `NaN`; a converter that printed one wrote a file that does not parse.
/// In every case the mesh keeps its triangles for the pawn (approximate),
/// one warning names the mesh, and the rest of the level is unharmed.
#[test]
fn unreadable_shapes_documents_fall_back_to_the_triangles() {
    let (s, m) = fixture();
    let good = source(&s, &m);
    let original = String::from_utf8(good.read(SHAPES, 1 << 20).unwrap()).unwrap();
    // The fixture writes the top face's height as 50.0 in its vertices.
    assert!(original.contains("50.0"));
    let nested = format!("{}1{}", "[".repeat(5000), "]".repeat(5000));
    let cases: Vec<(&str, String)> = vec![
        ("NaN for a number", original.replacen("50.0", "NaN", 1)),
        (
            "Infinity for a number",
            original.replacen("50.0", "Infinity", 1),
        ),
        (
            "-Infinity for a number",
            original.replacen("50.0", "-Infinity", 1),
        ),
        ("null for a number", original.replacen("50.0", "null", 1)),
        (
            "a string for a number",
            original.replacen("50.0", "\"50\"", 1),
        ),
        ("a number beyond f32", original.replacen("50.0", "1e39", 1)),
        ("a number beyond f64", original.replacen("50.0", "1e999", 1)),
        ("half a file", original[..original.len() / 2].to_owned()),
        ("an empty file", String::new()),
        ("an array", "[]".to_owned()),
        ("a number", "7".to_owned()),
        ("deep nesting", nested),
        (
            "a later version",
            original.replacen("\"version\":1", "\"version\":2", 1),
        ),
    ];
    for (what, text) in cases {
        let mut src = good.clone();
        assert_ne!(text, original, "{what}: the case changes nothing");
        src.insert(SHAPES, text.into_bytes());
        let map = load(&src);
        assert_eq!(map.warnings.len(), 1, "{what}: {:?}", map.warnings);
        assert!(
            map.warnings[0].contains(MESH) && map.warnings[0].contains("approximate"),
            "{what}: {:?}",
            map.warnings
        );
        assert_eq!(
            (map.stats.pawn_approximate, map.stats.pawn_simple),
            (2, 0),
            "{what}"
        );
        // The triangles: 100 + 44 and 300 + 44.
        assert_eq!(
            drop_at(&map, 0.0),
            Some((144.0, PrimitiveKind::TriangleMesh)),
            "{what}"
        );
        assert_eq!(
            drop_at(&map, 2000.0),
            Some((344.0, PrimitiveKind::TriangleMesh)),
            "{what}"
        );
        storm(&map);
    }
}

/// A manifest that names a shapes file that is not there, a folder, a path
/// out of the data, an absurd name, or counts that the file does not have.
#[test]
fn manifest_records_that_lie_do_not_take_the_collision_away() {
    let (s, m) = fixture();
    let good = source(&s, &m);
    let manifest: serde_json::Value =
        serde_json::from_slice(&good.read(MANIFEST, 1 << 20).unwrap()).unwrap();
    let long = "a/".repeat(4000) + "x.collision.json";
    for (what, shapes) in [
        ("a missing file", json!("AG-Hostile/nowhere.collision.json")),
        ("a folder", json!("AG-Hostile")),
        ("the manifest itself", json!("manifest.json")),
        ("a parent path", json!("../levels/AG-Hostile.scene.json")),
        ("an absolute path", json!("/etc/passwd")),
        ("a drive path", json!("C:\\Windows\\win.ini")),
        ("a device name", json!("AG-Hostile/NUL")),
        ("an empty name", json!("")),
        ("a very long path", json!(long)),
        (
            "a name with a NUL",
            json!("AG-Hostile/a\u{0}b.collision.json"),
        ),
    ] {
        let mut edited = manifest.clone();
        edited["meshes"][MESH]["simple_collision"]["shapes"] = shapes;
        let mut src = good.clone();
        src.insert(MANIFEST, edited.to_string().into_bytes());
        let map = load(&src);
        assert!(
            map.warnings
                .iter()
                .any(|w| w.contains(MESH) && w.contains("approximate")),
            "{what}: {:?}",
            map.warnings
        );
        assert_eq!(map.stats.pawn_simple, 0, "{what}");
        assert_eq!(
            drop_at(&map, 0.0),
            Some((144.0, PrimitiveKind::TriangleMesh)),
            "{what}"
        );
        storm(&map);
    }
    // Counts in the record that the file does not bear out: the file is
    // what is used.
    let mut edited = manifest.clone();
    edited["meshes"][MESH]["simple_collision"]["convex"] = json!(4_000_000_000u64);
    edited["meshes"][MESH]["simple_collision"]["boxes"] = json!(4_000_000_000u64);
    let mut src = good.clone();
    src.insert(MANIFEST, edited.to_string().into_bytes());
    let map = load(&src);
    assert_eq!(map.stats.simple_parts, 2);
    assert_eq!(drop_at(&map, 0.0), Some((94.0, PrimitiveKind::SimpleHull)));
    // A record that says there is a body where the manifest's file has no
    // shape at all: the pawn meets nothing, as with a body without shapes.
    let mut src = good.clone();
    src.insert(
        SHAPES,
        json!({"format": "asamu-simple-collision", "version": 1})
            .to_string()
            .into_bytes(),
    );
    let map = load(&src);
    assert_eq!((map.stats.pawn_simple, map.stats.pawn_none), (0, 2));
    assert_eq!(drop_at(&map, 0.0), None);
}

/// One element of the fixture's body replaced by `element`.
fn with_element(element: serde_json::Value) -> LoadedMap {
    let (s, _) = fixture();
    let mut m = MeshFixtures::new();
    let (v, t) = box_mesh(
        Vec3::new(-100.0, -100.0, 0.0),
        Vec3::new(100.0, 100.0, 100.0),
    );
    m.add(MESH, MAP, v, t);
    m.add_convex_raw(MESH, element);
    load(&source(&s, &m))
}

/// Elements whose lists are empty, degenerate or inconsistent. The solid of
/// an element is what its stored directions cut out around its vertices, so
/// the planes are never needed, and an element without a vertex is nothing.
#[test]
fn degenerate_elements_are_swept_or_left_out() {
    let (hv, ht) = box_mesh(
        Vec3::new(-100.0, -100.0, 0.0),
        Vec3::new(100.0, 100.0, 50.0),
    );
    let whole = convex_element(&hv, &ht);

    // No planes: the same solid (50 + 44).
    let mut e = whole.clone();
    e["planes"] = json!([]);
    let map = with_element(e);
    assert!(map.warnings.is_empty(), "{:?}", map.warnings);
    assert_eq!(drop_at(&map, 0.0), Some((94.0, PrimitiveKind::SimpleHull)));
    storm(&map);

    // Planes that are nonsense (zero normals, a plane far away): the same.
    let mut e = whole.clone();
    e["planes"] = json!([
        [0.0, 0.0, 0.0, 0.0],
        [0.0, 0.0, 1.0, -9.0e9],
        [1.0e30, 0.0, 0.0, 1.0]
    ]);
    let map = with_element(e);
    assert_eq!(drop_at(&map, 0.0), Some((94.0, PrimitiveKind::SimpleHull)));

    // No vertex: nothing is placed, a warning says so, and the pawn meets
    // nothing of this mesh (its body has no shape).
    let mut e = whole.clone();
    e["vertices"] = json!([]);
    e["face_triangles"] = json!([]);
    let map = with_element(e);
    assert_eq!(map.stats.simple_parts, 0);
    assert!(
        map.warnings.iter().any(|w| w.contains("left out")),
        "{:?}",
        map.warnings
    );
    assert_eq!(drop_at(&map, 0.0), None);
    storm(&map);

    // Vertices but triangles that name none of them: refused as a document.
    let mut e = whole.clone();
    e["vertices"] = json!([]);
    let map = with_element(e);
    assert!(
        map.warnings.iter().any(|w| w.contains("approximate")),
        "{:?}",
        map.warnings
    );
    assert_eq!(
        drop_at(&map, 0.0),
        Some((144.0, PrimitiveKind::TriangleMesh))
    );

    // Every vertex the same point, a single vertex, collinear vertices,
    // directions of length zero, directions that are not unit, no direction
    // at all, bounds that are inside out or far off: whatever is made of
    // them, every query ends.
    let point = [[5.0f32, 5.0, 5.0]; 8];
    let line: Vec<[f32; 3]> = (0..8).map(|i| [i as f32 * 10.0, 0.0, 20.0]).collect();
    let odd: Vec<serde_json::Value> = vec![
        json!({"vertices": point, "face_triangles": ht, "planes": [],
               "edge_directions": [[1.0, 0.0, 0.0]], "face_normal_directions": [[0.0, 0.0, 1.0]],
               "bounds_min": [5.0, 5.0, 5.0], "bounds_max": [5.0, 5.0, 5.0]}),
        json!({"vertices": [[1.0, 2.0, 3.0]], "face_triangles": [[0, 0, 0]], "planes": [],
               "edge_directions": [], "face_normal_directions": [[0.0, 0.0, 1.0]],
               "bounds_min": [1.0, 2.0, 3.0], "bounds_max": [1.0, 2.0, 3.0]}),
        json!({"vertices": line, "face_triangles": [[0, 1, 2], [2, 3, 4]], "planes": [],
               "edge_directions": [[1.0, 0.0, 0.0]], "face_normal_directions": [[0.0, 1.0, 0.0]],
               "bounds_min": [0.0, 0.0, 20.0], "bounds_max": [70.0, 0.0, 20.0]}),
        json!({"vertices": hv, "face_triangles": ht, "planes": [],
               "edge_directions": [[0.0, 0.0, 0.0], [0.0, 0.0, 0.0]],
               "face_normal_directions": [[0.0, 0.0, 0.0]],
               "bounds_min": [-100.0, -100.0, 0.0], "bounds_max": [100.0, 100.0, 50.0]}),
        json!({"vertices": hv, "face_triangles": ht, "planes": [],
               "edge_directions": [[1.0e30, 0.0, 0.0], [0.0, 1.0e-30, 0.0], [0.0, 0.0, 3.0]],
               "face_normal_directions": [[2.0e30, 0.0, 0.0], [0.0, 5.0e-30, 0.0], [0.0, 0.0, 7.0]],
               "bounds_min": [-100.0, -100.0, 0.0], "bounds_max": [100.0, 100.0, 50.0]}),
        json!({"vertices": hv, "face_triangles": ht, "planes": [],
               "edge_directions": [], "face_normal_directions": [],
               "bounds_min": [-100.0, -100.0, 0.0], "bounds_max": [100.0, 100.0, 50.0]}),
        json!({"vertices": hv, "face_triangles": [], "planes": [],
               "edge_directions": [], "face_normal_directions": [],
               "bounds_min": [-100.0, -100.0, 0.0], "bounds_max": [100.0, 100.0, 50.0]}),
        json!({"vertices": hv, "face_triangles": ht, "planes": [],
               "edge_directions": [[1.0, 0.0, 0.0]], "face_normal_directions": [[0.0, 0.0, 1.0]],
               "bounds_min": [9.0e9, 9.0e9, 9.0e9], "bounds_max": [-9.0e9, -9.0e9, -9.0e9]}),
    ];
    // What each must be, from its lists (the block's copy at x = 2000 is
    // scaled by 3):
    // 0: one point at height 5 with the direction Z: 5 + 44 and 15 + 44.
    // 1: one point at height 3, likewise: 47 and 53.
    // 2: a segment at height 20 through the box's footprint: 64 and 104.
    // 3, 5: no usable direction, so the directions of the surface, which is
    //    the lower half of the block: 94 and 194.
    // 4: the three axes, whatever their length: the same.
    // 6: neither a direction nor a surface: left out, with a warning.
    // 7: bounds that are inside out are not what the solid is made of.
    let expected: [Option<(f64, f64)>; 8] = [
        Some((49.0, 59.0)),
        Some((47.0, 53.0)),
        Some((64.0, 104.0)),
        Some((94.0, 194.0)),
        Some((94.0, 194.0)),
        Some((94.0, 194.0)),
        None,
        Some((94.0, 194.0)),
    ];
    for (k, (e, expected)) in odd.into_iter().zip(expected).enumerate() {
        let map = with_element(e);
        let near = drop_at(&map, 0.0);
        let far = drop_at(&map, 2000.0);
        storm(&map);
        match expected {
            Some((a, b)) => {
                assert!(map.warnings.is_empty(), "{k}: {:?}", map.warnings);
                assert_eq!(near, Some((a, PrimitiveKind::SimpleHull)), "{k}");
                assert_eq!(far, Some((b, PrimitiveKind::SimpleHull)), "{k}");
            }
            None => {
                assert_eq!(map.stats.simple_parts, 0, "{k}");
                assert!(
                    map.warnings.iter().any(|w| w.contains("left out")),
                    "{k}: {:?}",
                    map.warnings
                );
                assert_eq!((near, far), (None, None), "{k}");
            }
        }
    }
}

/// Coordinates at the end of what an `f32` holds, placed at a scale that
/// takes them past it, and placements that are themselves absurd. Bounds
/// that overflow must not poison the hierarchy for everything else: the
/// intact copy of the block beside it keeps working.
#[test]
fn enormous_coordinates_do_not_poison_the_scene() {
    for big in [3.0e38f32, 1.0e30, 1.0e19] {
        let (hv, ht) = box_mesh(Vec3::splat(-big), Vec3::splat(big));
        let element = convex_element(&hv, &ht);
        let mut s = SceneFixture::new(MAP, -50_000.0);
        s.static_mesh("Pkg.Meshes.Huge", Place::at(Vec3::new(0.0, 5.0e6, 0.0)));
        s.static_mesh(
            "Pkg.Meshes.Huge",
            Place {
                location: Vec3::new(0.0, -5.0e6, 0.0),
                rotation: [1234, 5678, 9012],
                scale: Vec3::new(1.0e6, -1.0e-6, 1.0e3),
            },
        );
        s.static_mesh(MESH, Place::at(Vec3::ZERO));
        let mut m = MeshFixtures::new();
        let (v, t) = box_mesh(Vec3::splat(-1.0), Vec3::splat(1.0));
        m.add("Pkg.Meshes.Huge", MAP, v, t);
        m.add_convex_raw("Pkg.Meshes.Huge", element);
        let (v, t) = box_mesh(
            Vec3::new(-100.0, -100.0, 0.0),
            Vec3::new(100.0, 100.0, 100.0),
        );
        m.add(MESH, MAP, v, t);
        let (hv, ht) = box_mesh(
            Vec3::new(-100.0, -100.0, 0.0),
            Vec3::new(100.0, 100.0, 50.0),
        );
        m.add_convex(MESH, &hv, &ht);
        let map = load(&source(&s, &m));
        eprintln!(
            "coordinates of {big:e}: parts {}, warnings {:?}",
            map.stats.simple_parts, map.warnings
        );
        storm(&map);
        // A solid that reaches that far holds the whole test area: whatever
        // the box meets first, the queries above ended. Far outside the
        // overflowing shapes' say nothing is asserted; the bounds of every
        // instance must be usable numbers.
        for inst in map.collision.statics() {
            assert!(
                !inst.bounds.min.is_nan() && !inst.bounds.max.is_nan(),
                "{big:e}: {:?}",
                inst.bounds
            );
        }
    }
}

/// Counts far beyond what the original has (its largest element has 79
/// vertices, 83 face directions and 148 edge directions; the largest body
/// 191 elements): an element with thousands of vertices and directions, a
/// body of thousands of elements, and a triangle budget that ends in the
/// middle of a body. Each loads in bounded time and memory and answers
/// queries.
#[test]
fn enormous_counts_are_bounded() {
    // A cloud of 5,000 vertices inside the lower half of the block, with
    // 3,000 face directions and 9,000 edge directions (most of them
    // repeats) and no surface: more than a solid may hold.
    let mut vertices: Vec<[f32; 3]> = Vec::new();
    for i in 0..5000u32 {
        let f = |k: u32| ((i.wrapping_mul(k) % 2001) as f32 / 10.0) - 100.0;
        vertices.push([f(7919), f(104_729), (i % 501) as f32 / 10.0]);
    }
    let dirs: Vec<[f32; 3]> = (0..9000u32)
        .map(|i| {
            let a = i as f32 * 0.001;
            [a.cos(), a.sin(), ((i % 7) as f32 - 3.0) * 0.1]
        })
        .collect();
    let element = json!({
        "vertices": vertices, "planes": [], "face_triangles": [],
        "edge_directions": dirs, "face_normal_directions": dirs[..3000],
        "bounds_min": [-100.0, -100.0, 0.0], "bounds_max": [100.0, 100.0, 50.0],
    });
    let map = with_element(element);
    // Neither a solid (too many vertices) nor a surface: left out, and the
    // loader says so.
    assert_eq!(map.stats.simple_parts, 0);
    assert!(
        map.warnings.iter().any(|w| w.contains("left out")),
        "{:?}",
        map.warnings
    );
    assert_eq!(drop_at(&map, 0.0), None);
    storm(&map);

    // A body of 6,000 small elements in a row along x.
    let (s, _) = fixture();
    let mut m = MeshFixtures::new();
    let (v, t) = box_mesh(
        Vec3::new(-100.0, -100.0, 0.0),
        Vec3::new(100.0, 100.0, 100.0),
    );
    m.add(MESH, MAP, v, t);
    for i in 0..6000 {
        let x = -100.0 + i as f32 * (200.0 / 6000.0);
        let (hv, ht) = box_mesh(
            Vec3::new(x, -100.0, 0.0),
            Vec3::new(x + 200.0 / 6000.0, 100.0, 50.0),
        );
        m.add_convex(MESH, &hv, &ht);
    }
    let src = source(&s, &m);
    let map = load(&src);
    assert!(map.warnings.is_empty(), "{:?}", map.warnings);
    // Two placements of 6,000 shapes.
    assert_eq!(map.stats.simple_parts, 12_000);
    // Together they are the lower half again: 50 + 44 (the heights are
    // exact; the box rests on many elements at once).
    assert_eq!(drop_at(&map, 0.0), Some((94.0, PrimitiveKind::SimpleHull)));
    storm(&map);

    // The budget ends inside that body: the mesh falls back to its
    // triangles (if those still fit) or loses its collision with a warning;
    // never half a body.
    for budget in [0usize, 11, 12, 13, 500, 20_000, 120_000] {
        let opts = LoadOptions {
            max_total_triangles: budget,
            ..LoadOptions::default()
        };
        let map = scene::load_map(&src, MAP, &opts).expect("the map still loads");
        let parts = map.stats.simple_parts;
        assert!(
            parts == 0 || parts == 12_000,
            "budget {budget}: half a body ({parts} shapes)"
        );
        if parts == 0 {
            assert!(!map.warnings.is_empty(), "budget {budget}");
        }
        let _ = drop_at(&map, 0.0);
    }
}
