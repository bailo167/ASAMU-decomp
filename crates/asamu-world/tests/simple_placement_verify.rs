//! Third look at the level's collision data: how a simple body is carried by
//! its placement, and whether the original's pawn can leave the places where
//! ours could not move.
//!
//! The synthetic part writes the component's matrix itself (no code of this
//! repository builds it) and uses a face that leans on both horizontal axes.
//! That matters: for a face that contains a world axis, the cross product of
//! one of its edges with that axis is the face's normal again, so a sweep
//! would still find the right contact with the face normals carried wrongly.
//! A face that leans both ways is found only when its normal is carried by
//! the inverse transpose of the placement.
//!
//! The gated part needs `ASAMU_CONVERTED_DIR` (mesh manifest version 2) and
//! `ASAMU_TRACE_RAW_DIR` (the raw recordings of the original,
//! `docs/TRACE_CAPTURE.md`); CI has neither and skips. Nothing is converted
//! and nothing is written.
//!
//! Every expected number is derived in the comment of its test from the
//! geometry written there, or is a recorded position of the original.

use std::path::{Path, PathBuf};

use asamu_world::collision::{PrimitiveKind, QueryFilter, SceneHit};
use asamu_world::fixtures::{MeshFixtures, Place, SceneFixture, box_mesh, json};
use asamu_world::scene::{self, DirSource, LoadOptions, LoadedMap, MemorySource};
use glam::{DVec3, Vec3};

const MAP: &str = "AG-Placed";
const MESH: &str = "Pkg.Meshes.Wedge";
/// The player's box.
const E: Vec3 = Vec3::new(21.0, 21.0, 44.0);
/// Where the wedge is placed.
const T: Vec3 = Vec3::new(1000.0, 2000.0, 300.0);

type Matrix = [[f32; 4]; 4];

/// The wedge in its mesh's space: the footprint `[-100, 100]²`, the bottom
/// `z = 0` and the top `z = 60 + 0.2 x + 0.1 y` (30, 70, 50 and 90 high at
/// its four corners), as eight vertices and twelve triangles.
fn wedge() -> (Vec<[f32; 3]>, Vec<[u32; 3]>) {
    let (mut v, t) = box_mesh(Vec3::new(-100.0, -100.0, 0.0), Vec3::new(100.0, 100.0, 1.0));
    for p in &mut v {
        if p[2] > 0.5 {
            p[2] = 60.0 + 20.0 * p[0].signum() + 10.0 * p[1].signum();
        }
    }
    (v, t)
}

/// A map with the wedge as the simple body of a mesh whose own triangles are
/// a thin slab under it (`z` from −10 to −5), placed by `matrix` (a
/// row-vector matrix: the images of the mesh's X, Y and Z axes, then the
/// origin).
fn source(matrix: &serde_json::Value) -> MemorySource {
    let mut s = SceneFixture::new(MAP, -50_000.0);
    let slot = s.static_mesh(MESH, Place::at(T));
    s.set_actor_value(slot, "local_to_world", matrix.clone());
    s.set_actor_value(
        slot,
        "components",
        json!([{
            "name": "StaticMeshComponent_0", "kind": "static_mesh", "local_to_world": matrix,
            "collide_actors": true, "block_actors": true, "block_zero_extent": true,
            "block_non_zero_extent": true, "static_mesh": MESH
        }]),
    );
    let mut m = MeshFixtures::new();
    let (v, t) = box_mesh(
        Vec3::new(-100.0, -100.0, -10.0),
        Vec3::new(100.0, 100.0, -5.0),
    );
    m.add(MESH, MAP, v, t);
    let (hv, ht) = wedge();
    m.add_convex(MESH, &hv, &ht);
    let mut src = MemorySource::new();
    s.write(&mut src);
    m.write_v2(&mut src, true);
    src
}

fn placed(matrix: Matrix) -> LoadedMap {
    let map = scene::load_map(&source(&json!(matrix)), MAP, &LoadOptions::default())
        .expect("the fixture loads");
    assert!(map.warnings.is_empty(), "{:?}", map.warnings);
    map
}

fn sweep(map: &LoadedMap, start: Vec3, end: Vec3) -> Option<SceneHit> {
    let hit = map
        .collision
        .sweep_box(&map.dynamic, start, end, E, QueryFilter::PAWN);
    let brute = map
        .collision
        .sweep_box_brute_force(&map.dynamic, start, end, E, QueryFilter::PAWN);
    assert_eq!(hit, brute);
    hit
}

/// The box dropped at `T + (u, v)` from 400 above `T` to 400 below it: the
/// height of its centre above `T` where it stops, and the hit.
fn drop_at(map: &LoadedMap, u: f32, v: f32) -> (f64, SceneHit) {
    let start = T + Vec3::new(u, v, 400.0);
    let end = T + Vec3::new(u, v, -400.0);
    let hit = sweep(map, start, end).expect("the wedge is under the box");
    (hit.point(start, end).z - f64::from(T.z), hit)
}

/// A trace along z through `T` (`up`: from 400 below to 400 above, else the
/// other way): the height above `T` where it ends, and the hit.
fn trace(map: &LoadedMap, up: bool, filter: QueryFilter) -> Option<(f64, SceneHit)> {
    let (a, b) = if up { (-400.0, 400.0) } else { (400.0, -400.0) };
    let start = T + Vec3::new(0.0, 0.0, a);
    let end = T + Vec3::new(0.0, 0.0, b);
    let hit = map.collision.raycast(&map.dynamic, start, end, filter);
    let brute = map
        .collision
        .raycast_brute_force(&map.dynamic, start, end, filter);
    assert_eq!(hit, brute);
    hit.map(|h| (h.point(start, end).z - f64::from(T.z), h))
}

fn close(a: f64, b: f64) -> bool {
    (a - b).abs() < 1.0e-4
}

/// The wedge placed with the scale (−2, 3, 0.5), which mirrors, and turned
/// about z: a quarter turn, and a turn whose cosine and sine are 0.8 and 0.6.
///
/// **Quarter turn.** The mesh's X axis goes to (0, −2, 0), its Y axis to
/// (−3, 0, 0), its Z axis to (0, 0, 0.5): a mesh point (x, y, z) is at
/// `T + (−3y, −2x, z/2)`. With (u, v) the offset from `T`, x = −v/2 and
/// y = −u/3, so the top `z = 60 + 0.2x + 0.1y` is the world plane
/// `30 − v/20 − u/60` above `T`: gradient (−1/60, −1/20), normal
/// (1/60, 1/20, 1) normalised.
///
/// **General turn.** X goes to −2·(0.8, 0.6, 0) = (−1.6, −1.2, 0), Y to
/// 3·(−0.6, 0.8, 0) = (−1.8, 2.4, 0). Solving u = −1.6x − 1.8y,
/// v = −1.2x + 2.4y (determinant −6) gives x = −0.4u − 0.3v and
/// y = −0.2u + (4/15)v, so the top is `30 + 0.1x + 0.05y =
/// 30 − u/20 − v/60` above `T`: gradient (−1/20, −1/60), normal
/// (1/20, 1/60, 1) normalised.
///
/// A box resting on a plane of gradient (a, b) has its centre
/// 44 + 21(|a| + |b|) above the plane at its centre (its lowest corner
/// touches). Both gradients give 44 + 21·(1/20 + 1/60) = 45.4:
///
/// - dropped at `T`: 30 + 45.4 = 75.4;
/// - dropped at (u, v) = (60, −30): the plane is at 30 + 1.5 − 1 = 30.5
///   (quarter turn) and 30 − 3 + 0.5 = 27.5 (general turn), so 75.9 and
///   72.9. The box's footprint stays well inside the wedge's.
///
/// What would go wrong: with the normals carried like vectors instead of by
/// the inverse transpose, the top's direction is no axis any more and the
/// box stops on the next best one, an edge of the top crossed with a world
/// axis. Under the quarter turn that is the top's edge along the mesh's X,
/// (0, −2, 0.1) in the world, crossed with the world's X: the direction
/// (0, 0.05, 1), on which the wedge reaches 35 above `T` (at u = −300) and
/// the box 0.05·21 + 44 = 45.05 below its centre: 80.05 instead of 75.4
/// (tried: with that change made, this test stops at 80.05). Without the
/// mirroring the quarter turn's plane would be `30 + v/20 − u/60`, which is
/// 27.5 at (60, −30) instead of 30.5.
///
/// No two of the scales multiply to more than 100, so the box's own face
/// axes are all tested (the scale rule changes nothing here).
#[test]
fn a_face_that_leans_both_ways_follows_a_mirrored_placement() {
    let quarter: Matrix = [
        [0.0, -2.0, 0.0, 0.0],
        [-3.0, 0.0, 0.0, 0.0],
        [0.0, 0.0, 0.5, 0.0],
        [T.x, T.y, T.z, 1.0],
    ];
    let general: Matrix = [
        [-1.6, -1.2, 0.0, 0.0],
        [-1.8, 2.4, 0.0, 0.0],
        [0.0, 0.0, 0.5, 0.0],
        [T.x, T.y, T.z, 1.0],
    ];
    let cases = [
        (
            "quarter turn",
            quarter,
            DVec3::new(1.0 / 60.0, 1.0 / 20.0, 1.0),
            75.9,
        ),
        (
            "general turn",
            general,
            DVec3::new(1.0 / 20.0, 1.0 / 60.0, 1.0),
            72.9,
        ),
    ];
    for (what, matrix, normal, off_centre) in cases {
        let map = placed(matrix);
        let hulls: Vec<_> = map
            .collision
            .statics()
            .iter()
            .filter(|i| i.kind == PrimitiveKind::SimpleHull)
            .collect();
        assert_eq!(hulls.len(), 1, "{what}");
        assert!(hulls[0].to_world.determinant() < 0.0, "{what}: mirrored");
        let rule = hulls[0]
            .native
            .expect("a body is under the original's rule");
        assert!(!rule.skips_face_axes(), "{what}");
        let normal = normal.normalize();

        let (z, hit) = drop_at(&map, 0.0, 0.0);
        assert!(close(z, 75.4), "{what}: {z}");
        assert_eq!(hit.kind, PrimitiveKind::SimpleHull, "{what}");
        assert!(
            (hit.normal - normal).length() < 1.0e-5,
            "{what}: {}",
            hit.normal
        );
        let (z, hit) = drop_at(&map, 60.0, -30.0);
        assert!(close(z, off_centre), "{what}: {z}");
        assert!(
            (hit.normal - normal).length() < 1.0e-5,
            "{what}: {}",
            hit.normal
        );

        // Line traces go to the body, whose faces are seen from outside
        // whichever way the placement turned their winding: from above the
        // top (30 above T, the same normal), from below the bottom (at T's
        // height, facing down).
        let (z, hit) = trace(&map, false, QueryFilter::LINE).expect("the top");
        assert!(close(z, 30.0), "{what}: {z}");
        assert_eq!(hit.kind, PrimitiveKind::SimpleHull, "{what}");
        assert!(
            (hit.normal - normal).length() < 1.0e-5,
            "{what}: {}",
            hit.normal
        );
        let (z, hit) = trace(&map, true, QueryFilter::LINE).expect("the bottom");
        assert!(close(z, 0.0), "{what}: {z}");
        assert!(
            (hit.normal - DVec3::NEG_Z).length() < 1.0e-5,
            "{what}: {}",
            hit.normal
        );
        // Bullet traces go to the triangles, the slab from −10 to −5 at half
        // scale: its top at −2.5 from above (through the body), its bottom
        // at −5 from below.
        let (z, hit) = trace(&map, false, QueryFilter::TRACE).expect("the slab");
        assert!(close(z, -2.5), "{what}: {z}");
        assert_eq!(hit.kind, PrimitiveKind::TriangleMesh, "{what}");
        let (z, _) = trace(&map, true, QueryFilter::TRACE).expect("the slab");
        assert!(close(z, -5.0), "{what}: {z}");
    }

    // Under the general turn the wedge's face x = 100 is the world plane
    // −0.8u − 0.6v = 200 (from x = −0.4u − 0.3v), facing n = (−0.8, −0.6, 0).
    // The box reaches 21·(0.8 + 0.6) = 29.4 along n, so walking in against n
    // from 400 out, 10 above T's height, its centre stops 229.4 from T: at
    // (u, v) = (−183.52, −137.64), after 170.6 of 400.
    let map = placed(general);
    let n = Vec3::new(-0.8, -0.6, 0.0);
    let start = T + n * 400.0 + Vec3::Z * 10.0;
    let end = T + Vec3::Z * 10.0;
    let hit = sweep(&map, start, end).expect("the side face");
    assert!((hit.t - 170.6 / 400.0).abs() < 1.0e-6, "{}", hit.t);
    let p = hit.point(start, end) - T.as_dvec3();
    assert!(close(p.x, -183.52) && close(p.y, -137.64), "{p}");
    assert!(
        (hit.normal - n.as_dvec3()).length() < 1.0e-5,
        "{}",
        hit.normal
    );
}

/// Placements no level should have: matrices without an inverse (a flat
/// one, a zero row, all zero), one beyond what an `f32` holds, and one that
/// is not a matrix. Whatever the loader makes of each (a map with or
/// without that body, or an error for a scene that cannot be read), nothing
/// panics, every query ends with a finite answer and the hierarchy agrees
/// with the reference.
#[test]
fn placements_without_an_inverse_do_not_break_a_body() {
    let row = |x: f64, y: f64, z: f64| json!([x, y, z, 0.0]);
    let origin = json!([T.x, T.y, T.z, 1.0]);
    let cases = [
        (
            "flat",
            json!([
                row(1.0, 0.0, 0.0),
                row(0.0, 1.0, 0.0),
                row(1.0, 1.0, 0.0),
                origin
            ]),
        ),
        (
            "a zero row",
            json!([
                row(2.0, 0.0, 0.0),
                row(0.0, 0.0, 0.0),
                row(0.0, 0.0, 3.0),
                origin
            ]),
        ),
        (
            "all zero",
            json!([
                row(0.0, 0.0, 0.0),
                row(0.0, 0.0, 0.0),
                row(0.0, 0.0, 0.0),
                origin
            ]),
        ),
        (
            "beyond f32",
            json!([
                row(1.0e39, 0.0, 0.0),
                row(0.0, 1.0, 0.0),
                row(0.0, 0.0, 1.0),
                origin
            ]),
        ),
        (
            "the largest f32",
            json!([
                row(3.0e38, 3.0e38, 3.0e38),
                row(-3.0e38, 3.0e38, 0.0),
                row(0.0, 0.0, 3.0e38),
                origin
            ]),
        ),
        ("not a matrix", json!([[1.0, 0.0], [0.0, 1.0]])),
    ];
    for (what, matrix) in cases {
        let Ok(map) = scene::load_map(&source(&matrix), MAP, &LoadOptions::default()) else {
            // A scene that does not read is refused as a whole.
            continue;
        };
        for inst in map.collision.statics() {
            assert!(
                !inst.bounds.min.is_nan() && !inst.bounds.max.is_nan(),
                "{what}: {:?}",
                inst.bounds
            );
        }
        for i in 0..12 {
            for j in 0..12 {
                let a = T + Vec3::new(-330.0 + 60.0 * i as f32, -310.0 + 55.0 * j as f32, 250.0);
                let b = T + Vec3::new(300.0 - 50.0 * j as f32, 290.0 - 45.0 * i as f32, -120.0);
                for filter in [QueryFilter::PAWN, QueryFilter::TRACE, QueryFilter::LINE] {
                    let hit = map.collision.sweep_box(&map.dynamic, a, b, E, filter);
                    assert_eq!(
                        hit,
                        map.collision
                            .sweep_box_brute_force(&map.dynamic, a, b, E, filter),
                        "{what}"
                    );
                    if let Some(h) = hit {
                        assert!(h.t.is_finite() && (0.0..=1.0).contains(&h.t), "{what}");
                        assert!(h.normal.is_finite(), "{what}");
                    }
                    if let Some(h) = map.collision.raycast(&map.dynamic, a, b, filter) {
                        assert!(h.t.is_finite() && (0.0..=1.0).contains(&h.t), "{what}");
                    }
                    let _ = map
                        .collision
                        .overlaps_box(&map.dynamic, a.lerp(b, 0.4), E, filter);
                }
            }
        }
    }
}

// ------------------------------------------------------------ gated part

fn converted_dir() -> Option<PathBuf> {
    let Some(dir) = std::env::var_os("ASAMU_CONVERTED_DIR").map(PathBuf::from) else {
        eprintln!("ASAMU_CONVERTED_DIR not set: skipping");
        return None;
    };
    let version = std::fs::read(dir.join("meshes").join("manifest.json"))
        .ok()
        .and_then(|d| serde_json::from_slice::<serde_json::Value>(&d).ok())
        .and_then(|v| v.get("version").and_then(serde_json::Value::as_u64))
        .unwrap_or(0);
    if version < 2 {
        eprintln!("mesh manifest version {version} has no simple collision: skipping");
        return None;
    }
    Some(dir)
}

/// The pawn locations of one raw recording by record index, with the map
/// each was recorded in.
fn recording(dir: &Path, scenario: &str) -> Option<Vec<(String, Vec3)>> {
    let needle = format!("-{scenario}-");
    let path = std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .find(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.contains(&needle) && n.ends_with(".raw.jsonl"))
        })?;
    let text = std::fs::read_to_string(path).ok()?;
    let mut out = Vec::new();
    for line in text.lines() {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        if v.get("frame").is_none() {
            continue;
        }
        let map = v["world"]["map"].as_str().unwrap_or_default().to_owned();
        let l = &v["player"]["location"];
        let c = |i: usize| l[i].as_f64().unwrap_or(f64::NAN) as f32;
        out.push((map, Vec3::new(c(0), c(1), c(2))));
    }
    Some(out)
}

/// The four places of `docs/PARITY_FINDINGS.md` V8 where our pawn "could
/// not move" because it stood inside a prop the original does not collide
/// the pawn with, as (scenario, raw record index, map, what stood there).
/// The record index is the segment's base plus the tick (bases: DC1 seg3
/// 8194; PLAY2 seg6 25512, seg1 3138, seg0 0).
const STARTS: [(&str, usize, &str, &str); 4] = [
    ("DC1", 8194 + 742, "AG-BeautifulCity", "rockpile_01"),
    ("PLAY2", 25_512 + 95, "AG-Darkcave", "redrock_04"),
    ("PLAY2", 3138 + 3950, "AG-BeautifulCity", "candle_1"),
    ("PLAY2", 1868, "AG-BeautifulCity", "six props"),
];

/// How many records after each start are followed.
const FOLLOWED: usize = 60;

/// The original's own path out of each of the four places, swept through
/// the new world: the pawn's box from every recorded position to the next,
/// for 60 records. The original went that way, so nothing that blocks the
/// pawn may be in it.
///
/// Asserted: the box is free at the start; the records are the ones the
/// findings name (V20: 60 ticks after DC1 seg3 tick 742 the original is
/// 258.6 uu from where it stood); and the first move the original makes
/// from each place is not stopped where it starts, which is what "cannot
/// move" was. The number of moves that meet something is printed as a
/// measurement, without a threshold: the straight line between two records
/// is not always the way the original went within the tick (it slides along
/// walls and steps up), so a later move may legitimately meet something.
/// Measured on the first recordings: none of the 181 moves does.
#[test]
fn the_original_leaves_the_four_stuck_starts_through_the_new_world() {
    let Some(dir) = converted_dir() else {
        return;
    };
    let Some(raw) = std::env::var_os("ASAMU_TRACE_RAW_DIR").map(PathBuf::from) else {
        eprintln!("ASAMU_TRACE_RAW_DIR not set: skipping");
        return;
    };
    for (scenario, index, map_name, what) in STARTS {
        let Some(records) = recording(&raw, scenario) else {
            eprintln!("{scenario}: no raw recording, skipped");
            continue;
        };
        let path: Vec<Vec3> = records
            .get(index..=index + FOLLOWED)
            .expect("the recording has the records")
            .iter()
            .map(|(m, p)| {
                assert!(m.eq_ignore_ascii_case(map_name), "{scenario} {index}: {m}");
                *p
            })
            .collect();
        let travelled: f64 = path
            .windows(2)
            .map(|w| f64::from((w[1] - w[0]).length()))
            .sum();
        if (scenario, index) == ("DC1", 8936) {
            let away = f64::from((path[FOLLOWED] - path[0]).length());
            assert!((away - 258.6).abs() < 0.1, "{away}");
        }
        let map = scene::load_map(&DirSource::new(&dir), map_name, &LoadOptions::default())
            .expect("map loads");
        let pawn = QueryFilter {
            sublevels: map.initial_level_mask(),
            ..QueryFilter::PAWN
        };
        assert!(
            !map.collision.overlaps_box(&map.dynamic, path[0], E, pawn),
            "{scenario} {index} ({what}): the box overlaps pawn collision"
        );
        let mut moves = 0usize;
        let mut met: Vec<(usize, f64, PrimitiveKind, bool)> = Vec::new();
        for (k, w) in path.windows(2).enumerate() {
            if w[0] == w[1] {
                continue;
            }
            moves += 1;
            let hit = map.collision.sweep_box(&map.dynamic, w[0], w[1], E, pawn);
            assert_eq!(
                hit,
                map.collision
                    .sweep_box_brute_force(&map.dynamic, w[0], w[1], E, pawn),
                "{scenario} {index} + {k}"
            );
            if let Some(h) = hit {
                let length = f64::from((w[1] - w[0]).length());
                if moves == 1 {
                    assert!(
                        !h.penetrating && h.t > 0.0,
                        "{scenario} {index} ({what}): the first move is stopped where it starts \
                         ({h:?})"
                    );
                }
                met.push((k, (1.0 - h.t) * length, h.kind, h.penetrating));
            }
        }
        assert!(moves > 0, "{scenario} {index}: the original does not move");
        eprintln!(
            "{scenario:<6} record {index:>5} ({what:<12}) {map_name:<17} travels {travelled:8.3} uu in \
             {moves} moves; moves that meet pawn collision: {}",
            met.len()
        );
        for (k, short, kind, penetrating) in &met {
            eprintln!(
                "        move {k:>2}: stopped {short:.4} uu before its recorded end by {kind:?}{}",
                if *penetrating { " (penetrating)" } else { "" }
            );
        }
    }
}
