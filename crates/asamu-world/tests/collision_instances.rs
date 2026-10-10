//! Primitive kinds, collision flags, the enabled switch and convex solids
//! in scene queries (synthetic data only; expected values derived from the
//! geometry in the comments).

#![allow(clippy::unwrap_used)]

use std::collections::BTreeSet;

use asamu_world::SurfaceTag;
use asamu_world::collision::{
    Affine, CollisionClass, CollisionScene, CollisionSceneBuilder, Instance, InstanceInfo,
    InstanceOptions, InstanceRef, PrimitiveKind, QueryFilter,
};
use glam::{DVec3, Vec3};

const HALF: Vec3 = Vec3::new(21.0, 21.0, 44.0);
const PAWN: QueryFilter = QueryFilter::PAWN;
const TRACE: QueryFilter = QueryFilter::TRACE;

fn info(class: CollisionClass, actor: Option<u32>) -> InstanceInfo {
    InstanceInfo {
        actor,
        class,
        tag: SurfaceTag::None,
        grapple_able: true,
        blocks_pawn: true,
        blocks_traces: true,
        sublevel: 0,
    }
}

/// The box `[lo, hi]` as twelve triangles.
fn block(lo: Vec3, hi: Vec3) -> (Vec<Vec3>, Vec<[u32; 3]>) {
    let v = (0..8u32)
        .map(|i| {
            Vec3::new(
                if i & 1 == 0 { lo.x } else { hi.x },
                if i & 2 == 0 { lo.y } else { hi.y },
                if i & 4 == 0 { lo.z } else { hi.z },
            )
        })
        .collect();
    let t = vec![
        [0, 1, 3],
        [0, 3, 2],
        [4, 6, 7],
        [4, 7, 5],
        [0, 4, 5],
        [0, 5, 1],
        [2, 3, 7],
        [2, 7, 6],
        [0, 2, 6],
        [0, 6, 4],
        [1, 5, 7],
        [1, 7, 3],
    ];
    (v, t)
}

fn close(a: f64, b: f64) -> bool {
    (a - b).abs() < 1.0e-9
}

/// A wall across each of five lanes (y = 0, 1000, … 4000), met by walking
/// 1000 uu in +x at z = 150:
///
/// 0. BSP: triangles of the world itself, face at x = 500.
/// 1. A blocking volume's brush, face at x = 500.
/// 2. A static mesh with a simple body: the body (face at x = 480) blocks
///    the pawn only, the mesh's triangles (face at x = 500) traces only.
/// 3. A static mesh collided by its triangles, face at x = 500.
/// 4. A pawn's cylinder (radius 34) at x = 550, blocking the pawn only.
///
/// A box or a cylinder of half width 21 meets a face at x = F with its
/// centre at F − 21: 479 of 1000, or 459 for the simple body. Against the
/// pawn cylinder both count as a radius-21 cylinder (the original adds the
/// trace extent to the cylinder): centre at 550 − 34 − 21 = 495. A ray meets
/// the faces at 500 of 1000.
#[test]
fn kinds_and_flags_are_reported() {
    let mut b = CollisionSceneBuilder::new();
    let wall = |lane: f32, x0: f32| {
        block(
            Vec3::new(x0, lane - 100.0, 0.0),
            Vec3::new(600.0, lane + 100.0, 300.0),
        )
    };
    let (v, t) = wall(0.0, 500.0);
    let bsp = b.add_mesh(v, t).unwrap();
    b.add_static(
        bsp,
        Affine::IDENTITY,
        info(CollisionClass::WorldGeometry, None),
    )
    .unwrap();
    let (v, t) = wall(1000.0, 500.0);
    let brush = b.add_convex_mesh(v, t).unwrap();
    b.add_static(
        brush,
        Affine::IDENTITY,
        info(CollisionClass::BlockingVolume, Some(1)),
    )
    .unwrap();
    let (v, t) = wall(2000.0, 480.0);
    let axes = [Vec3::X, Vec3::Y, Vec3::Z];
    let body = b.add_convex_hull(v, t, &axes, &axes).unwrap();
    let mut body_info = info(CollisionClass::StaticMesh, Some(2));
    body_info.blocks_traces = false;
    b.add_static_with(
        body,
        Affine::IDENTITY,
        body_info,
        InstanceOptions::of_kind(PrimitiveKind::SimpleHull),
    )
    .unwrap();
    let (v, t) = wall(2000.0, 500.0);
    let triangles = b.add_mesh(v, t).unwrap();
    let mut triangle_info = info(CollisionClass::StaticMesh, Some(2));
    triangle_info.blocks_pawn = false;
    b.add_static(triangles, Affine::IDENTITY, triangle_info)
        .unwrap();
    let (v, t) = wall(3000.0, 500.0);
    let mesh = b.add_mesh(v, t).unwrap();
    b.add_static(
        mesh,
        Affine::IDENTITY,
        info(CollisionClass::StaticMesh, Some(3)),
    )
    .unwrap();
    let mut scene = b.build();
    let cyl = scene.add_cylinder_mesh(34.0, 78.0).unwrap();
    let mut pawn_info = info(CollisionClass::StaticMesh, Some(4));
    pawn_info.blocks_traces = false;
    let dynamic = vec![
        scene
            .dynamic_instance(
                cyl,
                Affine::from_translation(DVec3::new(550.0, 4000.0, 150.0)),
                pawn_info,
            )
            .unwrap(),
    ];
    let kinds: Vec<PrimitiveKind> = scene.statics().iter().map(|i| i.kind).collect();
    assert_eq!(
        kinds,
        [
            PrimitiveKind::Bsp,
            PrimitiveKind::BrushHull,
            PrimitiveKind::SimpleHull,
            PrimitiveKind::TriangleMesh,
            PrimitiveKind::TriangleMesh
        ]
    );
    assert_eq!(dynamic[0].kind, PrimitiveKind::PawnCylinder);
    assert!(scene.statics().iter().all(|i| i.enabled) && dynamic[0].enabled);

    // (lane, pawn contact, pawn kind, trace contact and kind)
    let expected = [
        (
            0.0f32,
            0.479,
            PrimitiveKind::Bsp,
            Some((0.5, PrimitiveKind::Bsp)),
        ),
        (
            1000.0,
            0.479,
            PrimitiveKind::BrushHull,
            Some((0.5, PrimitiveKind::BrushHull)),
        ),
        (
            2000.0,
            0.459,
            PrimitiveKind::SimpleHull,
            Some((0.5, PrimitiveKind::TriangleMesh)),
        ),
        (
            3000.0,
            0.479,
            PrimitiveKind::TriangleMesh,
            Some((0.5, PrimitiveKind::TriangleMesh)),
        ),
        (4000.0, 0.495, PrimitiveKind::PawnCylinder, None),
    ];
    for (lane, t, kind, trace) in expected {
        let s = Vec3::new(0.0, lane, 150.0);
        let e = Vec3::new(1000.0, lane, 150.0);
        let hit = scene.sweep_box(&dynamic, s, e, HALF, PAWN).unwrap();
        assert!(close(hit.t, t), "box, lane {lane}: {hit:?}");
        assert_eq!((hit.kind, hit.normal), (kind, DVec3::NEG_X), "lane {lane}");
        assert_eq!(
            scene.sweep_box_brute_force(&dynamic, s, e, HALF, PAWN),
            Some(hit)
        );
        let inst = scene.instance(&dynamic, hit.instance).unwrap();
        assert!(inst.info.blocks_pawn && inst.kind == kind);
        let hit = scene
            .sweep_cylinder(&dynamic, s, e, 21.0, 44.0, PAWN)
            .unwrap();
        assert!(close(hit.t, t), "cylinder, lane {lane}: {hit:?}");
        assert_eq!(hit.kind, kind, "lane {lane}");
        let ray = scene.raycast(&dynamic, s, e, TRACE);
        assert_eq!(ray, scene.raycast_brute_force(&dynamic, s, e, TRACE));
        match (ray, trace) {
            (Some(hit), Some((t, kind))) => {
                assert!(close(hit.t, t), "ray, lane {lane}: {hit:?}");
                assert_eq!(hit.kind, kind, "lane {lane}");
                let inst = scene.instance(&dynamic, hit.instance).unwrap();
                assert!(inst.info.blocks_traces);
            }
            (None, None) => {}
            (got, want) => panic!("ray, lane {lane}: {got:?}, expected {want:?}"),
        }
        // A box swept as a trace sees what traces see.
        let as_trace = scene.sweep_box(&dynamic, s, e, HALF, TRACE);
        assert_eq!(as_trace.map(|h| h.kind), trace.map(|(_, k)| k));
    }
    // The simple body differs from the triangles: 20 uu in front of them.
    let inside_body_only = Vec3::new(459.5, 2000.0, 150.0);
    assert!(scene.overlaps_box(&dynamic, inside_body_only, HALF, PAWN));
    assert!(!scene.overlaps_box(&dynamic, inside_body_only, HALF, TRACE));
}

fn volume_scene(options: InstanceOptions) -> (CollisionScene, u32) {
    let mut b = CollisionSceneBuilder::new();
    // A floor at z = 0 that stays on.
    let floor = b
        .add_mesh(
            vec![
                Vec3::new(-4000.0, -4000.0, 0.0),
                Vec3::new(4000.0, -4000.0, 0.0),
                Vec3::new(0.0, 4000.0, 0.0),
            ],
            vec![[0, 1, 2]],
        )
        .unwrap();
    b.add_static(
        floor,
        Affine::IDENTITY,
        info(CollisionClass::StaticMesh, None),
    )
    .unwrap();
    // A blocking volume: x 500..600, y −100..100, z 50..350.
    let (v, t) = block(
        Vec3::new(500.0, -100.0, 50.0),
        Vec3::new(600.0, 100.0, 350.0),
    );
    let hull = b.add_convex_mesh(v, t).unwrap();
    let index = b
        .add_static_with(
            hull,
            Affine::IDENTITY,
            info(CollisionClass::BlockingVolume, Some(42)),
            options,
        )
        .unwrap();
    (b.build(), index)
}

/// Whether the volume blocks: a box and a cylinder walking through it, a
/// ray through it, and overlaps inside it. Blocking, the walkers stop with
/// their centre at 500 − 21 = 479 of 1000 and the ray at 500 (compared
/// below as 0.5 − 0.021).
fn blocks(scene: &CollisionScene, dynamic: &[Instance]) -> bool {
    let s = Vec3::new(0.0, 0.0, 200.0);
    let e = Vec3::new(1000.0, 0.0, 200.0);
    let inside = Vec3::new(550.0, 0.0, 200.0);
    let results = [
        scene.sweep_box(dynamic, s, e, HALF, PAWN).map(|h| h.t),
        scene
            .sweep_box_brute_force(dynamic, s, e, HALF, PAWN)
            .map(|h| h.t),
        scene
            .sweep_cylinder(dynamic, s, e, 21.0, 44.0, PAWN)
            .map(|h| h.t),
        scene
            .sweep_cylinder_brute_force(dynamic, s, e, 21.0, 44.0, PAWN)
            .map(|h| h.t),
        scene.raycast(dynamic, s, e, TRACE).map(|h| h.t - 0.021),
        scene
            .raycast_brute_force(dynamic, s, e, TRACE)
            .map(|h| h.t - 0.021),
    ];
    let overlaps = [
        scene.overlaps_box(dynamic, inside, HALF, PAWN),
        scene.overlaps_cylinder(dynamic, inside, 21.0, 44.0, PAWN),
    ];
    let on = results[0].is_some();
    for r in results {
        assert_eq!(r.is_some(), on, "{results:?}");
        if let Some(t) = r {
            assert!(close(t, 0.479), "{results:?}");
        }
    }
    assert_eq!(overlaps, [on, on]);
    // The floor is there either way: a box dropped beside the volume lands
    // with its centre at z = 44, after 156 of 200.
    let hit = scene
        .sweep_box(
            dynamic,
            Vec3::new(0.0, 0.0, 200.0),
            Vec3::new(0.0, 0.0, 0.0),
            HALF,
            PAWN,
        )
        .unwrap();
    assert!(close(hit.t, 0.78));
    on
}

/// A blocking volume that starts switched off (finding P5) is passed
/// through by every query and blocks again when it is switched on, by
/// instance, by actor, or through the field of a dynamic instance.
#[test]
fn a_disabled_instance_is_passed_through_and_blocks_again() {
    let (mut scene, index) = volume_scene(InstanceOptions {
        kind: None,
        enabled: false,
    });
    assert_eq!(
        scene.statics()[index as usize].kind,
        PrimitiveKind::BrushHull
    );
    assert!(!scene.statics()[index as usize].enabled);
    assert!(!blocks(&scene, &[]));
    // By actor.
    assert_eq!(scene.set_actor_enabled(42, true), 1);
    assert!(blocks(&scene, &[]));
    assert_eq!(scene.set_actor_enabled(42, false), 1);
    assert!(!blocks(&scene, &[]));
    assert_eq!(scene.set_actor_enabled(7, true), 0);
    assert!(!blocks(&scene, &[]));
    // By static index, and through a hit's reference.
    assert!(scene.set_static_enabled(index, true));
    assert!(blocks(&scene, &[]));
    let hit = scene
        .sweep_box(
            &[],
            Vec3::new(0.0, 0.0, 200.0),
            Vec3::new(1000.0, 0.0, 200.0),
            HALF,
            PAWN,
        )
        .unwrap();
    assert_eq!(hit.instance, InstanceRef::Static(index));
    assert!(scene.set_enabled(&mut [], hit.instance, false));
    assert!(!blocks(&scene, &[]));
    assert!(!scene.set_static_enabled(99, true));
    assert!(!scene.set_enabled(&mut [], InstanceRef::Dynamic(0), true));
    assert!(!blocks(&scene, &[]));
    // Taken out as a dynamic instance it keeps its kind and its switch.
    let mut dynamic = scene.take_actor_statics(&BTreeSet::from([42]));
    assert_eq!(dynamic.len(), 1);
    assert_eq!(
        (dynamic[0].kind, dynamic[0].enabled),
        (PrimitiveKind::BrushHull, false)
    );
    assert!(!blocks(&scene, &dynamic));
    dynamic[0].enabled = true;
    assert!(blocks(&scene, &dynamic));
    assert!(scene.set_enabled(&mut dynamic, InstanceRef::Dynamic(0), false));
    assert!(!blocks(&scene, &dynamic));
    // Moving it keeps both; switched on at the new place it blocks there.
    let away = Affine::from_translation(DVec3::new(0.0, 3000.0, 0.0));
    let mut moved = dynamic[0].clone();
    assert!(scene.place_dynamic(&mut moved, away));
    assert_eq!(
        (moved.kind, moved.enabled),
        (PrimitiveKind::BrushHull, false)
    );
    moved.enabled = true;
    assert!(!blocks(&scene, std::slice::from_ref(&moved)));
    let hit = scene
        .sweep_box(
            std::slice::from_ref(&moved),
            Vec3::new(0.0, 3000.0, 200.0),
            Vec3::new(1000.0, 3000.0, 200.0),
            HALF,
            PAWN,
        )
        .unwrap();
    assert!(close(hit.t, 0.479));
    assert_eq!(hit.instance, InstanceRef::Dynamic(0));
    // Built switched on, it blocks from the start.
    let (scene, _) = volume_scene(InstanceOptions::default());
    assert!(blocks(&scene, &[]));
}

/// A box that starts inside a convex solid (centre at x = 150 in the cube
/// |x|, |y|, |z| ≤ 200) is a penetrating start. The way out with the least
/// travel is +X: 200 − (150 − 21) = 71, against 221 through ±Y and 244
/// through ±Z, so the normal is +X and only motion against it (deeper in)
/// is blocked.
#[test]
fn a_box_inside_a_solid_is_blocked_only_deeper_in() {
    let mut b = CollisionSceneBuilder::new();
    let (v, t) = block(Vec3::splat(-200.0), Vec3::splat(200.0));
    let m = b.add_convex_mesh(v, t).unwrap();
    assert!(b.mesh(m).unwrap().hull().is_some());
    b.add_static(
        m,
        Affine::IDENTITY,
        info(CollisionClass::BlockingVolume, None),
    )
    .unwrap();
    let scene = b.build();
    let s = Vec3::new(150.0, 0.0, 0.0);
    assert!(scene.overlaps_box(&[], s, HALF, PAWN));
    let hit = scene
        .sweep_box(&[], s, s - Vec3::X * 10.0, HALF, PAWN)
        .unwrap();
    assert_eq!((hit.t, hit.normal, hit.penetrating), (0.0, DVec3::X, true));
    let hit = scene
        .sweep_box(&[], s, s + Vec3::new(-1.0, 30.0, 5.0), HALF, PAWN)
        .unwrap();
    assert!(hit.penetrating && hit.normal == DVec3::X);
    for free in [Vec3::X * 300.0, Vec3::Y * 30.0, Vec3::new(1.0, -30.0, 9.0)] {
        assert!(
            scene.sweep_box(&[], s, s + free, HALF, PAWN).is_none(),
            "{free}"
        );
    }
    // From outside it is solid: walking in −x from x = 500, the centre
    // stops at 221, after 279 of 500.
    let out = Vec3::new(500.0, 0.0, 0.0);
    let hit = scene.sweep_box(&[], out, Vec3::ZERO, HALF, PAWN).unwrap();
    assert!(close(hit.t, 279.0 / 500.0) && hit.normal == DVec3::X && !hit.penetrating);
}

/// A box element (half extents 50 × 30 × 20) turned a quarter about Z and
/// placed at (1000, 0, 100) occupies x 970..1030, y −50..50, z 80..120.
/// A box dropped on it lands with its centre at 120 + 44 = 164 (136 of 300
/// from z = 300); walking in +x from x = 800 at z = 100 it stops at
/// 970 − 21 = 949 (149 of 300).
#[test]
fn a_box_element_is_a_solid_wherever_it_is_placed() {
    let mut b = CollisionSceneBuilder::new();
    assert!(b.add_box_mesh(Vec3::new(50.0, 0.0, 20.0)).is_none());
    assert!(b.add_box_mesh(Vec3::new(50.0, f32::NAN, 20.0)).is_none());
    let m = b.add_box_mesh(Vec3::new(50.0, 30.0, 20.0)).unwrap();
    let hull = b.mesh(m).unwrap().hull().unwrap();
    assert_eq!(
        (
            hull.vertices().len(),
            hull.face_directions().len(),
            hull.edge_directions().len()
        ),
        (8, 3, 3)
    );
    // Local x → world y, local y → world −x.
    let quarter = Affine {
        rows: [DVec3::Y, DVec3::NEG_X, DVec3::Z],
        translation: DVec3::new(1000.0, 0.0, 100.0),
    };
    b.add_static_with(
        m,
        quarter,
        info(CollisionClass::StaticMesh, Some(5)),
        InstanceOptions::of_kind(PrimitiveKind::SimpleHull),
    )
    .unwrap();
    let scene = b.build();
    let s = Vec3::new(1000.0, 40.0, 300.0);
    let hit = scene
        .sweep_box(&[], s, s - Vec3::Z * 300.0, HALF, PAWN)
        .unwrap();
    assert!(close(hit.t, 136.0 / 300.0), "{hit:?}");
    assert_eq!(
        (hit.normal, hit.kind),
        (DVec3::Z, PrimitiveKind::SimpleHull)
    );
    // At y = 72 the box's side (y = 51) is past the element's edge at 50.
    let s = Vec3::new(1000.0, 72.0, 300.0);
    assert!(
        scene
            .sweep_box(&[], s, s - Vec3::Z * 300.0, HALF, PAWN)
            .is_none()
    );
    let s = Vec3::new(800.0, 0.0, 100.0);
    let hit = scene
        .sweep_box(&[], s, s + Vec3::X * 300.0, HALF, PAWN)
        .unwrap();
    assert!(close(hit.t, 149.0 / 300.0), "{hit:?}");
    assert_eq!(hit.normal, DVec3::NEG_X);
}

/// A mesh passed as convex that is not a closed solid (here one square,
/// wound so that it faces up) has no solid: the box meets its triangles,
/// one-sided, as the cylinder does. From above it lands with its centre at
/// z = 44 (156 of 400 from z = 200); from below it passes.
#[test]
fn an_open_convex_mesh_stays_one_sided_for_the_box() {
    let mut b = CollisionSceneBuilder::new();
    let m = b
        .add_convex_mesh(
            vec![
                Vec3::new(-500.0, -500.0, 0.0),
                Vec3::new(500.0, -500.0, 0.0),
                Vec3::new(500.0, 500.0, 0.0),
                Vec3::new(-500.0, 500.0, 0.0),
            ],
            vec![[0, 1, 2], [0, 2, 3]],
        )
        .unwrap();
    assert!(b.mesh(m).unwrap().is_convex() && b.mesh(m).unwrap().hull().is_none());
    b.add_static(
        m,
        Affine::IDENTITY,
        info(CollisionClass::BlockingVolume, None),
    )
    .unwrap();
    let scene = b.build();
    let up = Vec3::new(0.0, 0.0, 200.0);
    let down = Vec3::new(0.0, 0.0, -200.0);
    let hit = scene.sweep_box(&[], up, down, HALF, PAWN).unwrap();
    assert!(close(hit.t, 156.0 / 400.0) && hit.normal == DVec3::Z);
    assert_eq!(
        scene
            .sweep_cylinder(&[], up, down, 21.0, 44.0, PAWN)
            .map(|h| h.t),
        Some(hit.t)
    );
    assert!(scene.sweep_box(&[], down, up, HALF, PAWN).is_none());
    assert!(
        scene
            .sweep_cylinder(&[], down, up, 21.0, 44.0, PAWN)
            .is_none()
    );
}
