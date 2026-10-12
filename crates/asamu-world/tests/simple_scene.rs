//! The level's collision data as the original uses it, on synthetic
//! converted data (no game data): which meshes stop the pawn at all
//! (`docs/PARITY_FINDINGS.md` P1), at what shape (P2), volumes that start
//! switched off (P5), and the scale-dependent rule of the swept box through
//! the scene loader.
//!
//! Expected contacts are worked out in each test's comment from the
//! geometry the test writes and from the original's rules; none was taken
//! from a run of this code.

use asamu_assets::manifest::SimpleCollisionEntry;
use asamu_world::collision::{CollisionClass, InstanceRef, PrimitiveKind, QueryFilter, SceneHit};
use asamu_world::fixtures::{MeshFixtures, Place, SceneFixture, box_mesh, convex_element, json};
use asamu_world::scene::{
    self, LoadOptions, LoadedMap, MemorySource, TraceRoute, actor_id, mesh_routes,
};
use glam::{DVec3, Vec3};

const MAP: &str = "AG-Simple";
/// The player's box.
const E: Vec3 = Vec3::new(21.0, 21.0, 44.0);

fn load(scene: &SceneFixture, meshes: &MeshFixtures) -> LoadedMap {
    let mut src = MemorySource::new();
    scene.write(&mut src);
    meshes.write_v2(&mut src, true);
    scene::load_map(&src, MAP, &LoadOptions::default()).unwrap()
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

/// Height of the box's centre where a drop from `(x, y, 400)` to
/// `(x, y, -400)` is stopped, with the hit.
fn drop_at(map: &LoadedMap, x: f32, y: f32) -> Option<(f64, SceneHit)> {
    let start = Vec3::new(x, y, 400.0);
    let end = Vec3::new(x, y, -400.0);
    sweep(map, start, end).map(|h| (h.point(start, end).z, h))
}

/// Height at which a trace straight down from `(x, y, 400)` to
/// `(x, y, -400)` ends, with the hit.
fn trace_down(map: &LoadedMap, x: f32, y: f32, filter: QueryFilter) -> Option<(f64, SceneHit)> {
    let start = Vec3::new(x, y, 400.0);
    let end = Vec3::new(x, y, -400.0);
    let hit = map.collision.raycast(&map.dynamic, start, end, filter);
    let brute = map
        .collision
        .raycast_brute_force(&map.dynamic, start, end, filter);
    assert_eq!(hit, brute);
    hit.map(|h| (h.point(start, end).z, h))
}

fn close(a: f64, b: f64) -> bool {
    (a - b).abs() < 1.0e-4
}

/// A ramp as a closed solid: the box `[0, 10]³` cut by the plane
/// `x + z = 10`, so that it is 10 high at x = 0 and ends at x = 10.
fn ramp() -> (Vec<[f32; 3]>, Vec<[u32; 3]>) {
    let v = vec![
        [0.0, 0.0, 0.0],
        [10.0, 0.0, 0.0],
        [0.0, 10.0, 0.0],
        [10.0, 10.0, 0.0],
        [0.0, 0.0, 10.0],
        [0.0, 10.0, 10.0],
    ];
    let t = vec![
        [0, 1, 3],
        [0, 3, 2],
        [0, 2, 5],
        [0, 5, 4],
        [1, 3, 5],
        [1, 5, 4],
        [0, 1, 4],
        [2, 3, 5],
    ];
    (v, t)
}

/// The four rows of the table in MESHES.md ("A swept trace therefore sees
/// three kinds of mesh"), for both switches, and a mesh without a record.
#[test]
fn the_switches_route_the_traces() {
    let record = |use_box: bool, use_line: bool, body: bool| SimpleCollisionEntry {
        use_simple_box_collision: use_box,
        use_simple_line_collision: use_line,
        use_simple_rigid_body_collision: true,
        has_body_setup: body,
        shapes: body.then(|| "x.collision.json".to_owned()),
        convex: usize::from(body),
        boxes: 0,
        spheres: 0,
        sphyls: 0,
    };
    use TraceRoute::{Body, Nothing, Triangles};
    for (use_box, use_line, body, pawn, line) in [
        // Body setup, switches on (not stored): the simple shapes.
        (true, true, true, Body, Body),
        // No body setup, switches on: nothing.
        (true, true, false, Nothing, Nothing),
        // Switch stored false: the triangles, with or without a body.
        (false, false, true, Triangles, Triangles),
        (false, false, false, Triangles, Triangles),
        // The two switches are independent.
        (true, false, true, Body, Triangles),
        (false, true, true, Triangles, Body),
        (true, false, false, Nothing, Triangles),
        (false, true, false, Triangles, Nothing),
    ] {
        let r = mesh_routes(Some(&record(use_box, use_line, body)));
        assert_eq!((r.pawn, r.line, r.recorded), (pawn, line, true));
    }
    // Converted data without the record (mesh manifest version 1).
    let r = mesh_routes(None);
    assert_eq!((r.pawn, r.line, r.recorded), (Triangles, Triangles, false));
}

/// P1: a mesh whose box switch is on (the default) and that has no simple
/// body does not stop the pawn at all, and still stops bullet traces at its
/// triangles. A rug: `[-100, 100]² × [0, 10]`.
#[test]
fn a_mesh_without_a_body_is_passed_by_the_pawn_and_stops_bullets() {
    let mut s = SceneFixture::new(MAP, -5000.0);
    let slot = s.static_mesh("Pkg.Meshes.Rug", Place::at(Vec3::ZERO));
    let mut m = MeshFixtures::new();
    let (v, t) = box_mesh(
        Vec3::new(-100.0, -100.0, 0.0),
        Vec3::new(100.0, 100.0, 10.0),
    );
    m.add("Pkg.Meshes.Rug", MAP, v, t);
    let map = load(&s, &m);
    assert!(map.warnings.is_empty(), "{:?}", map.warnings);
    assert_eq!(
        (
            map.stats.blocking_components,
            map.stats.pawn_components,
            map.stats.pawn_none,
            map.stats.pawn_simple,
            map.stats.pawn_triangles,
            map.stats.pawn_approximate,
            map.stats.placed_components,
        ),
        (1, 1, 1, 0, 0, 0, 1)
    );
    // One instance: the triangles, for bullet traces only.
    let statics = map.collision.statics();
    assert_eq!(statics.len(), 1);
    let inst = &statics[0];
    assert_eq!(inst.kind, PrimitiveKind::TriangleMesh);
    assert_eq!(inst.info.actor, actor_id(0, slot));
    assert_eq!(
        (
            inst.info.blocks_pawn,
            inst.info.blocks_traces,
            inst.blocks_lines
        ),
        (false, true, false)
    );
    // The pawn's box falls through it, the legacy cylinder too, and a pawn
    // may stand inside it.
    assert_eq!(drop_at(&map, 0.0, 0.0), None);
    assert_eq!(
        map.collision.sweep_cylinder(
            &map.dynamic,
            Vec3::new(0.0, 0.0, 400.0),
            Vec3::new(0.0, 0.0, -400.0),
            21.0,
            44.0,
            QueryFilter::PAWN
        ),
        None
    );
    assert!(!map.collision.overlaps_box(
        &map.dynamic,
        Vec3::new(0.0, 0.0, 5.0),
        E,
        QueryFilter::PAWN
    ));
    // A bullet trace ends on its top, z = 10.
    let (z, hit) = trace_down(&map, 30.0, -40.0, QueryFilter::TRACE).unwrap();
    assert!(close(z, 10.0), "{z}");
    assert_eq!(hit.kind, PrimitiveKind::TriangleMesh);
    assert_eq!(hit.normal, DVec3::Z);
    assert_eq!(hit.instance, InstanceRef::Static(0));
    // A line trace that is no bullet trace goes to the body, which is not
    // there.
    assert_eq!(trace_down(&map, 30.0, -40.0, QueryFilter::LINE), None);
}

/// P2: a mesh with a simple body is collided, for the pawn, at the body and
/// not at its triangles.
///
/// The mesh's triangles are the block `[-100, 100]² × [0, 100]`. Its body is
/// one convex element, a ramp over the same square that rises from z = 0 at
/// x = −100 to z = 100 at x = 100: the plane `z = (x + 100) / 2`, gradient
/// 1/2 along x.
///
/// - The box dropped at (0, 0): a box on a plane of gradient (a, b) rests
///   with its centre `44 + 21 (|a| + |b|)` above the plane at its centre
///   (its lowest edge is the one that touches). Here 50 + 44 + 10.5 = 104.5,
///   on the plane's normal (−1, 0, 2)/√5. On the triangles it would stop at
///   100 + 44 = 144.
/// - A bullet trace down at (0, 0) ends on the triangles at z = 100.
/// - A line trace that is no bullet trace ends on the ramp at z = 50.
#[test]
fn a_mesh_with_a_body_is_met_at_the_body() {
    let mut s = SceneFixture::new(MAP, -5000.0);
    s.static_mesh("Pkg.Meshes.Stairs", Place::at(Vec3::ZERO));
    let mut m = MeshFixtures::new();
    let (v, t) = box_mesh(
        Vec3::new(-100.0, -100.0, 0.0),
        Vec3::new(100.0, 100.0, 100.0),
    );
    m.add("Pkg.Meshes.Stairs", MAP, v, t);
    let wedge_v = [
        [-100.0, -100.0, 0.0],
        [100.0, -100.0, 0.0],
        [-100.0, 100.0, 0.0],
        [100.0, 100.0, 0.0],
        [100.0, -100.0, 100.0],
        [100.0, 100.0, 100.0],
    ];
    let wedge_t = [
        [0, 1, 3],
        [0, 3, 2],
        [1, 3, 5],
        [1, 5, 4],
        [0, 2, 5],
        [0, 5, 4],
        [0, 1, 4],
        [2, 3, 5],
    ];
    m.add_convex("Pkg.Meshes.Stairs", &wedge_v, &wedge_t);
    let map = load(&s, &m);
    assert!(map.warnings.is_empty(), "{:?}", map.warnings);
    assert_eq!(
        (
            map.stats.pawn_simple,
            map.stats.pawn_none,
            map.stats.pawn_triangles,
            map.stats.simple_parts
        ),
        (1, 0, 0, 1)
    );
    // Two instances of the one actor: the triangles for bullet traces, the
    // body for the pawn and for line traces.
    let statics = map.collision.statics();
    assert_eq!(statics.len(), 2);
    assert_eq!(
        (
            statics[0].kind,
            statics[0].info.blocks_pawn,
            statics[0].info.blocks_traces,
            statics[0].blocks_lines
        ),
        (PrimitiveKind::TriangleMesh, false, true, false)
    );
    assert_eq!(
        (
            statics[1].kind,
            statics[1].info.blocks_pawn,
            statics[1].info.blocks_traces,
            statics[1].blocks_lines
        ),
        (PrimitiveKind::SimpleHull, true, false, true)
    );
    assert_eq!(statics[0].info.actor, statics[1].info.actor);
    assert!(statics[1].native.is_some());

    let (z, hit) = drop_at(&map, 0.0, 0.0).unwrap();
    assert!(close(z, 104.5), "{z}");
    assert_eq!(hit.kind, PrimitiveKind::SimpleHull);
    let n = DVec3::new(-1.0, 0.0, 2.0) / 5.0f64.sqrt();
    assert!((hit.normal - n).length() < 1.0e-6, "{}", hit.normal);
    assert!(!hit.penetrating);
    // 30 further up the ramp the plane is 15 higher.
    let (z, _) = drop_at(&map, 30.0, 0.0).unwrap();
    assert!(close(z, 119.5), "{z}");

    let (z, hit) = trace_down(&map, 0.0, 0.0, QueryFilter::TRACE).unwrap();
    assert!(close(z, 100.0), "{z}");
    assert_eq!(hit.kind, PrimitiveKind::TriangleMesh);
    let (z, hit) = trace_down(&map, 0.0, 0.0, QueryFilter::LINE).unwrap();
    assert!(close(z, 50.0), "{z}");
    assert_eq!(hit.kind, PrimitiveKind::SimpleHull);
    // A pawn trace with no extent sees what the pawn's box sees.
    let (z, hit) = trace_down(&map, 0.0, 0.0, QueryFilter::PAWN).unwrap();
    assert!(close(z, 50.0), "{z}");
    assert_eq!(hit.kind, PrimitiveKind::SimpleHull);
}

/// The same mesh with its switches stored `false` is met at its triangles by
/// everything: the pawn stops at 100 + 44 = 144 on the block's top, and the
/// body is not placed at all. With only the box switch `false`, the pawn
/// uses the triangles and line traces the body; with only the line switch
/// `false`, the other way round.
#[test]
fn a_stored_false_switch_sends_the_trace_to_the_triangles() {
    let build = |use_box: bool, use_line: bool| {
        let mut s = SceneFixture::new(MAP, -5000.0);
        s.static_mesh("Pkg.Meshes.Stairs", Place::at(Vec3::ZERO));
        let mut m = MeshFixtures::new();
        let (v, t) = box_mesh(
            Vec3::new(-100.0, -100.0, 0.0),
            Vec3::new(100.0, 100.0, 100.0),
        );
        m.add("Pkg.Meshes.Stairs", MAP, v, t);
        // The body: the lower half of the block, top at z = 50.
        let (hv, ht) = box_mesh(
            Vec3::new(-100.0, -100.0, 0.0),
            Vec3::new(100.0, 100.0, 50.0),
        );
        m.add_convex("Pkg.Meshes.Stairs", &hv, &ht);
        m.set_switches("Pkg.Meshes.Stairs", use_box, use_line);
        load(&s, &m)
    };
    // (box switch, line switch) → (pawn stops at, line trace ends at).
    for (use_box, use_line, pawn_z, line_z) in [
        (false, false, 144.0, 100.0),
        (false, true, 144.0, 50.0),
        (true, false, 94.0, 100.0),
        (true, true, 94.0, 50.0),
    ] {
        let map = build(use_box, use_line);
        assert!(map.warnings.is_empty(), "{:?}", map.warnings);
        let (z, hit) = drop_at(&map, 0.0, 0.0).unwrap();
        assert!(close(z, pawn_z), "{use_box} {use_line}: {z}");
        let kind = if use_box {
            PrimitiveKind::SimpleHull
        } else {
            PrimitiveKind::TriangleMesh
        };
        assert_eq!(hit.kind, kind);
        assert_eq!(hit.normal, DVec3::Z);
        let (z, _) = trace_down(&map, 0.0, 0.0, QueryFilter::LINE).unwrap();
        assert!(close(z, line_z), "{use_box} {use_line}: {z}");
        // Bullet traces: the triangles, always.
        let (z, hit) = trace_down(&map, 0.0, 0.0, QueryFilter::TRACE).unwrap();
        assert!(close(z, 100.0), "{z}");
        assert_eq!(hit.kind, PrimitiveKind::TriangleMesh);
        assert_eq!(
            (map.stats.pawn_simple, map.stats.pawn_triangles),
            (usize::from(use_box), usize::from(!use_box))
        );
        // The body is placed only where something goes to it.
        assert_eq!(
            map.collision.statics().len(),
            if use_box || use_line { 2 } else { 1 }
        );
    }
}

/// A body is placed by its component's transform, non-uniform scale and
/// mirroring included.
///
/// The ramp ([`ramp`]) is 10 high at x = 0 and ends at x = 10 (the plane
/// `x + z = 10`). Placed with the scale (−8, 9, 2) a local point (x, y, z)
/// is at (−8x, 9y, 2z): the solid spans x ∈ [−80, 0], y ∈ [0, 90],
/// z ∈ [0, 20], and its sloping face is `−x/8 + z/2 = 10`, that is
/// `z = 20 + x/4`: it rises towards +x, because the placement mirrors x.
///
/// - The box dropped at (−50, 45) covers x ∈ [−71, −29], inside the slope.
///   The plane is at 20 − 12.5 = 7.5 under its centre and its gradient is
///   1/4, so the centre rests at 7.5 + 44 + 21/4 = 56.75, on the plane's
///   normal (−1, 0, 4)/√17.
/// - Without the mirroring the solid would be at x ∈ [0, 80]: a drop at
///   (50, 45) must find nothing.
/// - The mesh's own triangles are a slab that the pawn must not notice.
///
/// Turned by a yaw of 16384 units (90°: the mesh's +X goes to the world's
/// +Y and its +Y to the world's −X), a local point is at (−9y, −8x, 2z): the
/// solid spans x ∈ [−90, 0], y ∈ [−80, 0], and the slope is `z = 20 + y/4`.
/// The box dropped at (−45, −50) rests at 56.75 again, on (0, −1, 4)/√17.
///
/// No pair of these scales multiplies to more than 100 (72 at most), so the
/// box's face axes are all tested and the rule changes nothing here.
#[test]
fn a_body_follows_scale_mirroring_and_rotation() {
    let (hv, ht) = ramp();
    let mut m = MeshFixtures::new();
    let (v, t) = box_mesh(Vec3::new(0.0, 0.0, 0.0), Vec3::new(10.0, 10.0, 1.0));
    m.add("Pkg.Meshes.Ramp", MAP, v, t);
    m.add_convex("Pkg.Meshes.Ramp", &hv, &ht);
    let scale = Vec3::new(-8.0, 9.0, 2.0);

    let mut s = SceneFixture::new(MAP, -5000.0);
    s.static_mesh(
        "Pkg.Meshes.Ramp",
        Place {
            location: Vec3::ZERO,
            rotation: [0; 3],
            scale,
        },
    );
    let map = load(&s, &m);
    assert!(map.warnings.is_empty(), "{:?}", map.warnings);
    let hull = &map.collision.statics()[1];
    assert_eq!(hull.kind, PrimitiveKind::SimpleHull);
    assert!(hull.to_world.determinant() < 0.0, "the placement mirrors");
    assert!(!hull.native.unwrap().skips_face_axes());
    let (z, hit) = drop_at(&map, -50.0, 45.0).unwrap();
    assert!(close(z, 56.75), "{z}");
    let n = DVec3::new(-1.0, 0.0, 4.0) / 17.0f64.sqrt();
    assert!((hit.normal - n).length() < 1.0e-6, "{}", hit.normal);
    assert_eq!(hit.kind, PrimitiveKind::SimpleHull);
    assert_eq!(drop_at(&map, 50.0, 45.0), None);
    // On the flat part behind the top (x just below 0 is the top edge, 20
    // high): a box at (10, 45) covers x ∈ [−11, 31] and rests on that edge.
    let (z, _) = drop_at(&map, 10.0, 45.0).unwrap();
    assert!(close(z, 64.0), "{z}");
    // Walking in from the high side at half height: the back face is the
    // plane x = 0, met with the centre at x = 21.
    let start = Vec3::new(200.0, 45.0, 10.0);
    let end = Vec3::new(-200.0, 45.0, 10.0);
    let hit = sweep(&map, start, end).unwrap();
    assert!(close(hit.point(start, end).x, 21.0));
    assert_eq!(hit.normal, DVec3::X);

    let mut s = SceneFixture::new(MAP, -5000.0);
    s.static_mesh(
        "Pkg.Meshes.Ramp",
        Place {
            location: Vec3::ZERO,
            rotation: [0, 16_384, 0],
            scale,
        },
    );
    let map = load(&s, &m);
    let (z, hit) = drop_at(&map, -45.0, -50.0).unwrap();
    assert!(close(z, 56.75), "{z}");
    let n = DVec3::new(0.0, -1.0, 4.0) / 17.0f64.sqrt();
    assert!((hit.normal - n).length() < 1.0e-5, "{}", hit.normal);
    assert_eq!(drop_at(&map, -45.0, 50.0), None);
    assert_eq!(drop_at(&map, 45.0, -50.0), None);
}

/// The scale-dependent rule through the loader (NATIVE_PHYSICS.md 10.1; the
/// derivations are in `simple_native_rule.rs`). An octahedron with its
/// corners one unit from the centre, as a mesh with itself as its body.
///
/// - Placed at scale 128 the box dropped on its top corner stops with its
///   centre at 128 + 86 = 214: the box's face axes and every edge axis are
///   left out, and the four face directions (±1, ±1, 1)/√3 remain, on which
///   the box reaches (21 + 21 + 44)/√3. The exact contact is 128 + 44 = 172.
/// - Placed at scale 20 it stops at 20 + 44 = 64: the box's face axes are
///   left out, but the octahedron's horizontal edges crossed with the box's
///   edges give the vertical axis back.
/// - Placed at scale 5 nothing is left out: 5 + 44 = 49.
///
/// A mesh whose switch sends the pawn to its triangles keeps the box's face
/// axes at any scale: the same octahedron with its box switch stored `false`
/// stops the box at 128 + 44 = 172.
#[test]
fn the_scale_rule_applies_to_placed_bodies() {
    let v: Vec<[f32; 3]> = vec![
        [1.0, 0.0, 0.0],
        [-1.0, 0.0, 0.0],
        [0.0, 1.0, 0.0],
        [0.0, -1.0, 0.0],
        [0.0, 0.0, 1.0],
        [0.0, 0.0, -1.0],
    ];
    let t: Vec<[u32; 3]> = vec![
        [0, 2, 4],
        [2, 1, 4],
        [1, 3, 4],
        [3, 0, 4],
        [2, 0, 5],
        [1, 2, 5],
        [3, 1, 5],
        [0, 3, 5],
    ];
    // What the fixture writes for it: four face directions, six edge
    // directions, eight planes.
    let element = convex_element(&v, &t);
    assert_eq!(
        element["face_normal_directions"].as_array().unwrap().len(),
        4
    );
    assert_eq!(element["edge_directions"].as_array().unwrap().len(), 6);
    assert_eq!(element["planes"].as_array().unwrap().len(), 8);
    let mut m = MeshFixtures::new();
    m.add("Pkg.Meshes.Rock", MAP, v.clone(), t.clone());
    m.add_convex("Pkg.Meshes.Rock", &v, &t);
    m.add("Pkg.Meshes.RockTri", MAP, v.clone(), t.clone());
    m.add_convex("Pkg.Meshes.RockTri", &v, &t);
    m.set_switches("Pkg.Meshes.RockTri", false, true);
    let place = |mesh: &str, scale: f32| {
        let mut s = SceneFixture::new(MAP, -5000.0);
        s.static_mesh(
            mesh,
            Place {
                location: Vec3::ZERO,
                rotation: [0; 3],
                scale: Vec3::splat(scale),
            },
        );
        load(&s, &m)
    };
    for (scale, z_expected, wide) in [(128.0, 214.0, true), (20.0, 64.0, true), (5.0, 49.0, false)]
    {
        let map = place("Pkg.Meshes.Rock", scale);
        let hull = &map.collision.statics()[1];
        assert_eq!(hull.native.unwrap().skips_face_axes(), wide, "{scale}");
        let (z, hit) = drop_at(&map, 0.0, 0.0).unwrap();
        assert!((z - z_expected).abs() < 1.0e-4, "scale {scale}: {z}");
        assert_eq!(hit.kind, PrimitiveKind::SimpleHull);
    }
    let map = place("Pkg.Meshes.RockTri", 128.0);
    let (z, hit) = drop_at(&map, 0.0, 0.0).unwrap();
    assert!(close(z, 172.0), "{z}");
    assert_eq!(hit.kind, PrimitiveKind::TriangleMesh);
    assert_eq!(hit.normal, DVec3::Z);
}

/// P5: a `DynamicBlockingVolume` whose `bEnabled` is false starts with its
/// collision switched off; switching the actor's collision on makes it
/// block, and off again lets the pawn through.
///
/// The volume is the box x ∈ [200, 300], y ∈ [−100, 100], z ∈ [0, 300]. A
/// pawn walking along +x at y = 0, z = 100 meets its face x = 200 with its
/// centre at 200 − 21 = 179.
#[test]
fn a_disabled_volume_is_passed_until_it_is_switched_on() {
    let mut s = SceneFixture::new(MAP, -5000.0);
    let lo = Vec3::new(200.0, -100.0, 0.0);
    let hi = Vec3::new(300.0, 100.0, 300.0);
    let disabled = s.dynamic_blocking_volume(lo, hi, false);
    // Further along: one that starts enabled, one that is enabled although
    // its stored actor flag says it does not collide (the class sets that
    // flag from `bEnabled` when play begins), and a plain blocking volume.
    let step = Vec3::new(1000.0, 0.0, 0.0);
    let enabled = s.dynamic_blocking_volume(lo + step, hi + step, true);
    let flagless = s.dynamic_blocking_volume(lo + step * 2.0, hi + step * 2.0, true);
    s.set_actor_value(flagless, "collide_actors", json!(false));
    let plain = s.blocking_volume(lo + step * 3.0, hi + step * 3.0);
    let mut map = load(&s, &MeshFixtures::new());
    assert!(map.warnings.is_empty(), "{:?}", map.warnings);
    assert_eq!(
        (
            map.stats.blocking_hulls,
            map.stats.switchable_volumes,
            map.stats.disabled_volumes
        ),
        (4, 3, 1)
    );
    let id = |slot: usize| actor_id(0, slot).unwrap();
    assert_eq!(map.actor_collision(id(disabled)), Some(false));
    assert_eq!(map.actor_collision(id(enabled)), Some(true));
    assert_eq!(map.actor_collision(id(flagless)), Some(true));
    assert_eq!(map.actor_collision(id(plain)), Some(true));
    assert_eq!(map.actor_collision(9999), None);
    assert_eq!(
        map.actors_without_collision()
            .into_iter()
            .collect::<Vec<_>>(),
        vec![id(disabled)]
    );

    // Where a walk along +x from `from` is stopped.
    let walk = |map: &LoadedMap, from: f32| {
        let start = Vec3::new(from, 0.0, 100.0);
        let end = Vec3::new(from + 500.0, 0.0, 100.0);
        sweep(map, start, end).map(|h| (h.point(start, end).x, h))
    };
    assert_eq!(walk(&map, 0.0), None);
    assert!(!map.collision.overlaps_box(
        &map.dynamic,
        Vec3::new(250.0, 0.0, 100.0),
        E,
        QueryFilter::PAWN
    ));
    for (k, slot) in [(1.0, enabled), (2.0, flagless), (3.0, plain)] {
        let (x, hit) = walk(&map, 1000.0 * k).unwrap();
        assert!(close(x, 1000.0 * f64::from(k) + 179.0), "{x}");
        assert_eq!(hit.kind, PrimitiveKind::BrushHull);
        assert_eq!(hit.normal, DVec3::NEG_X);
        let inst = map.collision.instance(&map.dynamic, hit.instance).unwrap();
        assert_eq!(inst.info.actor, Some(id(slot)));
        assert_eq!(inst.info.class, CollisionClass::BlockingVolume);
        // Volumes do not stop traces.
        assert!(!inst.info.blocks_traces && !inst.blocks_lines);
    }

    // Level script switches the actor on.
    assert_eq!(map.set_actor_collision(id(disabled), true), 1);
    assert_eq!(map.actor_collision(id(disabled)), Some(true));
    assert!(map.actors_without_collision().is_empty());
    let (x, hit) = walk(&map, 0.0).unwrap();
    assert!(close(x, 179.0), "{x}");
    assert_eq!(hit.kind, PrimitiveKind::BrushHull);
    assert!(map.collision.overlaps_box(
        &map.dynamic,
        Vec3::new(250.0, 0.0, 100.0),
        E,
        QueryFilter::PAWN
    ));
    // And off again; an enabled one can be switched off as well.
    assert_eq!(map.set_actor_collision(id(disabled), false), 1);
    assert_eq!(walk(&map, 0.0), None);
    assert_eq!(map.set_actor_collision(id(enabled), false), 1);
    assert_eq!(walk(&map, 1000.0), None);
    // An actor without collision has nothing to switch.
    assert_eq!(map.set_actor_collision(9999, true), 0);

    // A game layer that moved the volume's instances to its own dynamic
    // list (actors that level script moves) switches them through the free
    // function; the switch survives the move.
    let ids = std::collections::BTreeSet::from([id(disabled)]);
    let mut dynamic = map.collision.take_actor_statics(&ids);
    assert_eq!(dynamic.len(), 1);
    assert!(!dynamic[0].enabled);
    assert_eq!(
        scene::actor_collision(&map.collision, &dynamic, id(disabled)),
        Some(false)
    );
    let start = Vec3::new(0.0, 0.0, 100.0);
    let end = Vec3::new(500.0, 0.0, 100.0);
    let through = |c: &asamu_world::collision::CollisionScene, d: &[_]| {
        c.sweep_box(d, start, end, E, QueryFilter::PAWN)
    };
    assert_eq!(through(&map.collision, &dynamic), None);
    assert_eq!(
        scene::set_actor_collision(&mut map.collision, &mut dynamic, id(disabled), true),
        1
    );
    let hit = through(&map.collision, &dynamic).unwrap();
    assert!(close(hit.point(start, end).x, 179.0));
    assert_eq!(hit.instance, InstanceRef::Dynamic(0));
}

/// A body made of a sphere only never stops the pawn (the original's sphere
/// and capsule checks answer "no hit" to any trace with an extent), and the
/// loader says when line traces would need a ray against one. With the line
/// switch stored `false`, as on the one such mesh of the shipped game, line
/// traces use the triangles and nothing is missing.
#[test]
fn spheres_and_capsules_never_stop_the_pawn() {
    let build = |use_line: bool| {
        let mut s = SceneFixture::new(MAP, -5000.0);
        s.static_mesh("Pkg.Meshes.Ball", Place::at(Vec3::ZERO));
        let mut m = MeshFixtures::new();
        let (v, t) = box_mesh(Vec3::splat(-160.0), Vec3::splat(160.0));
        m.add("Pkg.Meshes.Ball", MAP, v, t);
        m.add_sphere_shape("Pkg.Meshes.Ball", Vec3::ZERO, 160.0);
        m.add_sphyl_shape("Pkg.Meshes.Ball", Vec3::ZERO, 50.0, 400.0);
        m.set_switches("Pkg.Meshes.Ball", true, use_line);
        load(&s, &m)
    };
    let map = build(false);
    assert!(map.warnings.is_empty(), "{:?}", map.warnings);
    assert_eq!((map.stats.pawn_none, map.stats.pawn_simple), (1, 0));
    assert_eq!(drop_at(&map, 0.0, 0.0), None);
    let (z, _) = trace_down(&map, 0.0, 0.0, QueryFilter::TRACE).unwrap();
    assert!(close(z, 160.0));
    let (z, _) = trace_down(&map, 0.0, 0.0, QueryFilter::LINE).unwrap();
    assert!(close(z, 160.0));

    let map = build(true);
    assert_eq!(drop_at(&map, 0.0, 0.0), None);
    assert_eq!(trace_down(&map, 0.0, 0.0, QueryFilter::LINE), None);
    assert_eq!(map.warnings.len(), 1, "{:?}", map.warnings);
    assert!(map.warnings[0].contains("not traced"), "{:?}", map.warnings);
}

/// A box element is placed by its own matrix inside the mesh: a box of full
/// size (40, 60, 20) centred on (0, 0, 100) of a mesh placed at (500, 0, 0)
/// has its top at z = 110, and the pawn's box rests on it at 154.
#[test]
fn a_box_element_is_placed_by_its_own_matrix() {
    let mut s = SceneFixture::new(MAP, -5000.0);
    s.static_mesh("Pkg.Meshes.Shelf", Place::at(Vec3::new(500.0, 0.0, 0.0)));
    let mut m = MeshFixtures::new();
    let (v, t) = box_mesh(Vec3::splat(-5.0), Vec3::splat(5.0));
    m.add("Pkg.Meshes.Shelf", MAP, v, t);
    m.add_box_shape(
        "Pkg.Meshes.Shelf",
        Vec3::new(0.0, 0.0, 100.0),
        Vec3::new(40.0, 60.0, 20.0),
    );
    let map = load(&s, &m);
    assert!(map.warnings.is_empty(), "{:?}", map.warnings);
    let (z, hit) = drop_at(&map, 500.0, 0.0).unwrap();
    assert!(close(z, 154.0), "{z}");
    assert_eq!(hit.kind, PrimitiveKind::SimpleHull);
    // The box's own routine was not read: it is swept as the exact solid.
    assert!(map.collision.statics()[1].native.is_none());
    // 20 + 21 from the centre along x the pawn's box no longer covers it.
    assert!(drop_at(&map, 500.0 + 40.9, 0.0).is_some());
    assert_eq!(drop_at(&map, 500.0 + 41.1, 0.0), None);
}

/// Elements that are not convex, that store odd lists or that reach far
/// outside their mesh are swept as what their lists say, and nothing fails.
///
/// The element has the eight corners of the cube `[0, 10]³` and a ninth
/// vertex at (5, 5, 5000), far above; it stores the three axes as its face
/// and edge directions and the cube's twelve triangles. For the box sweep
/// its solid is what those directions cut out around all nine vertices: the
/// column `[0, 10]² × [0, 5000]`, although no triangle and no plane says so.
/// A box dropped on it rests at 5000 + 44; a box that walks in from the side
/// at z = 2500 is stopped 21 before the column; next to the column there is
/// nothing.
#[test]
fn elements_that_are_not_convex_are_swept_as_their_lists_say() {
    let (cv, ct) = box_mesh(Vec3::ZERO, Vec3::splat(10.0));
    let mut vertices = cv.clone();
    vertices.push([5.0, 5.0, 5000.0]);
    let element = json!({
        "vertices": vertices,
        "planes": [[0.0, 0.0, 1.0, 10.0], [0.0, 0.0, -1.0, 0.0]],
        "face_triangles": ct,
        "edge_directions": [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
        "face_normal_directions": [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
        "bounds_min": [0.0, 0.0, 0.0],
        "bounds_max": [10.0, 10.0, 5000.0],
    });
    let mut s = SceneFixture::new(MAP, -50_000.0);
    s.static_mesh("Pkg.Meshes.Odd", Place::at(Vec3::ZERO));
    let mut m = MeshFixtures::new();
    m.add("Pkg.Meshes.Odd", MAP, cv, ct.clone());
    m.add_convex_raw("Pkg.Meshes.Odd", element);
    // A second element without any triangle and one with nothing at all.
    m.add_convex_raw(
        "Pkg.Meshes.Odd",
        json!({
            "vertices": [[-500.0, 0.0, 0.0], [-490.0, 0.0, 0.0], [-500.0, 10.0, 0.0], [-500.0, 0.0, 10.0]],
            "planes": [], "face_triangles": [], "edge_directions": [],
            "face_normal_directions": [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
            "bounds_min": [-500.0, 0.0, 0.0], "bounds_max": [-490.0, 10.0, 10.0],
        }),
    );
    m.add_convex_raw(
        "Pkg.Meshes.Odd",
        json!({
            "vertices": [], "planes": [], "face_triangles": [], "edge_directions": [],
            "face_normal_directions": [], "bounds_min": [0.0, 0.0, 0.0], "bounds_max": [0.0, 0.0, 0.0],
        }),
    );
    let map = load(&s, &m);
    // The empty element is left out, with a note.
    assert_eq!(map.warnings.len(), 1, "{:?}", map.warnings);
    assert!(map.warnings[0].contains("left out"), "{:?}", map.warnings);
    assert_eq!(map.stats.simple_parts, 2);
    let start = Vec3::new(5.0, 5.0, 9000.0);
    let end = Vec3::new(5.0, 5.0, 0.0);
    let hit = sweep(&map, start, end).unwrap();
    assert!((hit.point(start, end).z - 5044.0).abs() < 1.0e-3);
    assert_eq!(hit.normal, DVec3::Z);
    let start = Vec3::new(300.0, 5.0, 2500.0);
    let end = Vec3::new(-300.0, 5.0, 2500.0);
    let hit = sweep(&map, start, end).unwrap();
    assert!(close(hit.point(start, end).x, 31.0));
    assert_eq!(hit.normal, DVec3::X);
    let start = Vec3::new(40.0, 5.0, 9000.0);
    assert_eq!(sweep(&map, start, Vec3::new(40.0, 5.0, 0.0)), None);
    // The element without triangles is the solid of its four vertices along
    // its three directions, the cube [−500, −490] × [0, 10]²: the box rests
    // on it at 10 + 44.
    let start = Vec3::new(-495.0, 5.0, 400.0);
    let end = Vec3::new(-495.0, 5.0, 0.0);
    let hit = sweep(&map, start, end).unwrap();
    assert!(close(hit.point(start, end).z, 54.0));
    // A storm of sweeps through and around all of it: every one ends.
    let mut n = 0u32;
    for i in 0..40 {
        for j in 0..40 {
            let a = Vec3::new(-700.0 + 35.0 * i as f32, -80.0 + 7.0 * j as f32, 6000.0);
            let b = Vec3::new(600.0 - 31.0 * j as f32, 90.0 - 5.0 * i as f32, -200.0);
            if let Some(h) = sweep(&map, a, b) {
                assert!(h.t.is_finite() && (0.0..=1.0).contains(&h.t));
                n += 1;
            }
            let _ = map.collision.raycast(&map.dynamic, a, b, QueryFilter::LINE);
            let _ = map
                .collision
                .overlaps_box(&map.dynamic, a.lerp(b, 0.5), E, QueryFilter::PAWN);
        }
    }
    assert!(n > 0);
}

/// A moving actor (a falling rock) has its body as dynamic instances beside
/// its triangles, and both follow the actor.
#[test]
fn a_falling_rock_carries_its_body() {
    let mut s = SceneFixture::new(MAP, -5000.0);
    let rock = s.mesh_actor(
        "asamu.ASAMUFallingRock",
        "falling_rock",
        "Pkg.Meshes.Rock",
        Place::at(Vec3::new(0.0, 0.0, 200.0)),
        "COLLIDE_CustomDefault",
        None,
        json!({"fallDistance": 500.0, "fallingLowerRate": 400.0, "fallingHigherRate": 400.0}),
    );
    let mut m = MeshFixtures::new();
    // Triangles: a block 100 high. Body: its lower half.
    let (v, t) = box_mesh(Vec3::new(-50.0, -50.0, 0.0), Vec3::new(50.0, 50.0, 100.0));
    m.add("Pkg.Meshes.Rock", MAP, v, t);
    let (hv, ht) = box_mesh(Vec3::new(-50.0, -50.0, 0.0), Vec3::new(50.0, 50.0, 50.0));
    m.add_convex("Pkg.Meshes.Rock", &hv, &ht);
    let mut map = load(&s, &m);
    assert!(map.warnings.is_empty(), "{:?}", map.warnings);
    assert!(map.collision.statics().is_empty());
    assert_eq!(map.dynamic.len(), 2);
    assert_eq!(map.bodies.len(), 1);
    assert_eq!(map.bodies[0].actor, actor_id(0, rock).unwrap());
    assert_eq!(map.bodies[0].parts.len(), 2);
    assert_eq!(map.actors.rocks.len(), 1);
    assert_eq!(map.actors.rocks[0].body, Some(0));
    assert_eq!(
        (map.dynamic[0].kind, map.dynamic[1].kind),
        (PrimitiveKind::TriangleMesh, PrimitiveKind::SimpleHull)
    );
    assert!(map.dynamic[1].native.is_some() && map.dynamic[1].blocks_lines);
    // The pawn lands on the body's top (200 + 50 + 44), a bullet on the
    // triangles' (200 + 100).
    let (z, hit) = drop_at(&map, 0.0, 0.0).unwrap();
    assert!(close(z, 294.0), "{z}");
    assert_eq!(hit.instance, InstanceRef::Dynamic(1));
    let (z, _) = trace_down(&map, 0.0, 0.0, QueryFilter::TRACE).unwrap();
    assert!(close(z, 300.0), "{z}");
    // The rock 150 lower: both parts moved.
    let body = map.bodies[0].clone();
    let actor = scene::body_transform(&body, Vec3::new(0.0, 0.0, 50.0), [0; 3]);
    for (index, relative) in &body.parts {
        let to_world = relative.then(&actor);
        assert!(
            map.collision
                .place_dynamic(&mut map.dynamic[*index], to_world)
        );
    }
    let (z, _) = drop_at(&map, 0.0, 0.0).unwrap();
    assert!(close(z, 144.0), "{z}");
    let (z, _) = trace_down(&map, 0.0, 0.0, QueryFilter::TRACE).unwrap();
    assert!(close(z, 150.0), "{z}");
    assert!(map.dynamic[1].native.is_some() && map.dynamic[1].blocks_lines);
    assert!(!map.dynamic[0].blocks_lines);
}

/// A conversion without simple-collision data (mesh manifest version 1)
/// still loads and behaves as before: every blocking mesh stops the pawn at
/// its triangles. The loader says that this is approximate, once.
#[test]
fn a_version_1_manifest_collides_with_triangles_and_says_so() {
    let mut s = SceneFixture::new(MAP, -5000.0);
    s.static_mesh("Pkg.Meshes.Rug", Place::at(Vec3::ZERO));
    s.static_mesh("Pkg.Meshes.Stairs", Place::at(Vec3::new(1000.0, 0.0, 0.0)));
    let mut m = MeshFixtures::new();
    let (v, t) = box_mesh(
        Vec3::new(-100.0, -100.0, 0.0),
        Vec3::new(100.0, 100.0, 10.0),
    );
    m.add("Pkg.Meshes.Rug", MAP, v, t);
    let (v, t) = box_mesh(
        Vec3::new(-100.0, -100.0, 0.0),
        Vec3::new(100.0, 100.0, 100.0),
    );
    m.add("Pkg.Meshes.Stairs", MAP, v, t);
    // Declared, but a version 1 manifest cannot carry it.
    let (hv, ht) = box_mesh(
        Vec3::new(-100.0, -100.0, 0.0),
        Vec3::new(100.0, 100.0, 50.0),
    );
    m.add_convex("Pkg.Meshes.Stairs", &hv, &ht);
    let mut src = MemorySource::new();
    s.write(&mut src);
    m.write(&mut src, true);
    let map = scene::load_map(&src, MAP, &LoadOptions::default()).unwrap();
    assert_eq!(map.warnings.len(), 1, "{:?}", map.warnings);
    let w = &map.warnings[0];
    assert!(w.contains("version 1") && w.contains("approximate"), "{w}");
    assert_eq!(
        (
            map.stats.pawn_components,
            map.stats.pawn_approximate,
            map.stats.pawn_simple,
            map.stats.pawn_none,
            map.stats.pawn_triangles
        ),
        (2, 2, 0, 0, 0)
    );
    let (z, hit) = drop_at(&map, 0.0, 0.0).unwrap();
    assert!(close(z, 54.0), "{z}");
    assert_eq!(hit.kind, PrimitiveKind::TriangleMesh);
    let (z, hit) = drop_at(&map, 1000.0, 0.0).unwrap();
    assert!(close(z, 144.0), "{z}");
    assert_eq!(hit.kind, PrimitiveKind::TriangleMesh);
    for inst in map.collision.statics() {
        assert!(inst.info.blocks_pawn && inst.info.blocks_traces && inst.blocks_lines);
    }
    // The same data as version 2: the rug is passed, the stairs are met at
    // their body.
    let map = load(&s, &m);
    assert!(map.warnings.is_empty(), "{:?}", map.warnings);
    assert_eq!(drop_at(&map, 0.0, 0.0), None);
    let (z, _) = drop_at(&map, 1000.0, 0.0).unwrap();
    assert!(close(z, 94.0), "{z}");
}

/// Shapes that cannot be read do not take the mesh's collision away: the
/// pawn then collides with the triangles (approximate) and a warning says
/// which mesh. Nothing panics on any of these.
#[test]
fn unusable_shapes_fall_back_to_the_triangles() {
    let mut s = SceneFixture::new(MAP, -5000.0);
    s.static_mesh("Pkg.Meshes.Stairs", Place::at(Vec3::ZERO));
    s.static_mesh("Pkg.Meshes.Stairs", Place::at(Vec3::new(1000.0, 0.0, 0.0)));
    let mut m = MeshFixtures::new();
    let (v, t) = box_mesh(
        Vec3::new(-100.0, -100.0, 0.0),
        Vec3::new(100.0, 100.0, 100.0),
    );
    m.add("Pkg.Meshes.Stairs", MAP, v, t);
    let (hv, ht) = box_mesh(
        Vec3::new(-100.0, -100.0, 0.0),
        Vec3::new(100.0, 100.0, 50.0),
    );
    m.add_convex("Pkg.Meshes.Stairs", &hv, &ht);
    let mut good = MemorySource::new();
    s.write(&mut good);
    m.write_v2(&mut good, true);
    let shapes = "meshes/AG-Simple/Pkg_Meshes_Stairs.collision.json";
    let original = scene::DataSource::read(&good, shapes, 1 << 20).unwrap();
    let manifest_path = "meshes/manifest.json";
    let manifest: serde_json::Value =
        serde_json::from_slice(&scene::DataSource::read(&good, manifest_path, 1 << 20).unwrap())
            .unwrap();

    let check = |src: &MemorySource, what: &str| {
        let map = scene::load_map(src, MAP, &LoadOptions::default()).unwrap();
        assert_eq!(map.warnings.len(), 1, "{what}: {:?}", map.warnings);
        assert!(
            map.warnings[0].contains("Pkg.Meshes.Stairs")
                && map.warnings[0].contains("approximate"),
            "{what}: {:?}",
            map.warnings
        );
        assert_eq!(
            (map.stats.pawn_approximate, map.stats.pawn_simple),
            (2, 0),
            "{what}"
        );
        for x in [0.0, 1000.0] {
            let (z, hit) = drop_at(&map, x, 0.0).unwrap();
            assert!(close(z, 144.0), "{what}: {z}");
            assert_eq!(hit.kind, PrimitiveKind::TriangleMesh);
        }
    };
    // The file is there but is not what it should be.
    let mut src = MemorySource::new();
    s.write(&mut src);
    m.write_v2(&mut src, true);
    let mut doc: serde_json::Value = serde_json::from_slice(&original).unwrap();
    src.insert(shapes, b"{ not json".to_vec());
    check(&src, "broken JSON");
    doc["format"] = json!("something-else");
    src.insert(shapes, doc.to_string().into_bytes());
    check(&src, "another format");
    doc["format"] = json!("asamu-simple-collision");
    doc["convex"][0]["face_triangles"][0][0] = json!(99);
    src.insert(shapes, doc.to_string().into_bytes());
    check(&src, "an index past the vertices");
    doc["convex"][0]["face_triangles"][0][0] = json!(0);
    doc["convex"][0]["vertices"][1][2] = json!(1.0e39);
    src.insert(shapes, doc.to_string().into_bytes());
    check(&src, "a vertex beyond f32");
    // The manifest names a file outside the data, or none.
    for (shapes_value, what) in [
        (json!("../../secret.json"), "an unsafe path"),
        (json!("C:/x.json"), "a drive path"),
        (json!("AG-Simple/missing.collision.json"), "a missing file"),
        (serde_json::Value::Null, "no file"),
    ] {
        let mut edited = manifest.clone();
        edited["meshes"]["Pkg.Meshes.Stairs"]["simple_collision"]["shapes"] = shapes_value;
        let mut src = MemorySource::new();
        s.write(&mut src);
        m.write_v2(&mut src, true);
        src.insert(manifest_path, edited.to_string().into_bytes());
        check(&src, what);
    }
    // An intact file after all that.
    let map = scene::load_map(&good, MAP, &LoadOptions::default()).unwrap();
    assert!(map.warnings.is_empty());
    let (z, _) = drop_at(&map, 0.0, 0.0).unwrap();
    assert!(close(z, 94.0));
    // A triangle budget too small for the body: the same fallback.
    let opts = LoadOptions {
        max_total_triangles: 12,
        ..LoadOptions::default()
    };
    let map = scene::load_map(&good, MAP, &opts).unwrap();
    assert!(!map.warnings.is_empty());
    assert_eq!(map.stats.pawn_simple, 0);
    // A record of the wrong shape makes the whole manifest's records
    // unreadable: the map still loads, approximately.
    let mut edited = manifest.clone();
    edited["meshes"]["Pkg.Meshes.Stairs"]["simple_collision"] = json!({"has_body_setup": "yes"});
    let mut src = MemorySource::new();
    s.write(&mut src);
    m.write_v2(&mut src, true);
    src.insert(manifest_path, edited.to_string().into_bytes());
    let map = scene::load_map(&src, MAP, &LoadOptions::default()).unwrap();
    assert!(map.warnings.iter().any(|w| w.contains("approximate")));
    assert_eq!(map.stats.pawn_approximate, 2);
}

/// The sizes of gameplay actors keep coming from what is drawn, not from a
/// body that reaches further: a crystal whose mesh is a cube of half size 50
/// with a body four times as large keeps the half extent 50.
#[test]
fn gameplay_sizes_do_not_follow_the_body() {
    let mut s = SceneFixture::new(MAP, -5000.0);
    s.mesh_actor(
        "asamu.ASAMURechargeCrystal",
        "recharge_crystal",
        "Pkg.Meshes.Crystal",
        Place::at(Vec3::new(0.0, 0.0, 500.0)),
        "COLLIDE_BlockAll",
        None,
        json!({"RechargeDelay": 4.0, "bShouldRecharge": true}),
    );
    let mut m = MeshFixtures::new();
    let (v, t) = box_mesh(Vec3::splat(-50.0), Vec3::splat(50.0));
    m.add("Pkg.Meshes.Crystal", MAP, v, t);
    let (hv, ht) = box_mesh(Vec3::splat(-200.0), Vec3::splat(200.0));
    m.add_convex("Pkg.Meshes.Crystal", &hv, &ht);
    let map = load(&s, &m);
    assert_eq!(map.actors.crystals.len(), 1);
    assert_eq!(map.actors.crystals[0].half_extent, 50.0);
    // The pawn does meet the body: 500 + 200 + 44.
    let start = Vec3::new(0.0, 0.0, 2000.0);
    let end = Vec3::new(0.0, 0.0, 0.0);
    let hit = sweep(&map, start, end).unwrap();
    assert!(close(hit.point(start, end).z, 744.0));
}
