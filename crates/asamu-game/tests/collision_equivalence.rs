//! The triangle collision world against the box world: boxes triangulated
//! into 12 triangles each must give the same contacts as `BoxWorld`'s exact
//! cylinder-vs-box sweep (an independent implementation: Minkowski slabs and
//! rounded vertical edges) and the same ray hits.

use std::sync::Arc;

use asamu_game::{GameWorld, SceneCollision};
use asamu_player::BoxWorld;
use asamu_player::world::{CollisionShape, CollisionWorld};
use asamu_world::fixtures::{MeshFixtures, Place, SceneFixture, box_mesh};
use asamu_world::scene::{self, LoadOptions, MemorySource};
use glam::Vec3;

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
    fn range(&mut self, lo: f32, hi: f32) -> f32 {
        lo + (hi - lo) * ((self.next() >> 40) as f32 / (1u64 << 24) as f32)
    }
}

const SHAPE: CollisionShape = CollisionShape {
    radius: 21.0,
    half_height: 44.0,
};

/// The same boxes as a `BoxWorld` and as a converted scene of static meshes.
fn worlds(seed: u64) -> (BoxWorld, GameWorld) {
    let mut rng = Rng(seed);
    let mut boxes = BoxWorld::new();
    let mut src = MemorySource::new();
    let mut scene = SceneFixture::new("AG-Boxes", -1.0e6);
    scene.player_start(Vec3::new(0.0, 0.0, 5000.0), 0);
    let mut meshes = MeshFixtures::new();
    for i in 0..40 {
        // Integer-ish sizes keep the f32 geometry exact in both worlds.
        let half = Vec3::new(
            rng.range(10.0, 300.0).round(),
            rng.range(10.0, 300.0).round(),
            rng.range(10.0, 200.0).round(),
        );
        let center = Vec3::new(
            rng.range(-1500.0, 1500.0).round(),
            rng.range(-1500.0, 1500.0).round(),
            rng.range(-300.0, 300.0).round(),
        );
        boxes = boxes.with_box(center - half, center + half, true);
        let path = format!("Pkg.Box{i}");
        let (v, t) = box_mesh(-half, half);
        meshes.add(&path, "AG-Boxes", v, t);
        scene.static_mesh(&path, Place::at(center));
    }
    scene.write(&mut src);
    meshes.write(&mut src, true);
    let map = scene::load_map(&src, "AG-Boxes", &LoadOptions::default()).expect("fixture loads");
    assert_eq!(map.collision.stats().instance_triangles, 40 * 12);
    let world = GameWorld {
        boxes: BoxWorld::new(),
        scene: Some(SceneCollision::new(Arc::new(map))),
    };
    (boxes, world)
}

#[test]
fn sweeps_and_rays_match_the_box_world() {
    let mut compared = 0;
    let mut normal_mismatches = 0;
    for seed in 1..=4 {
        let (boxes, tris) = worlds(seed);
        let mut rng = Rng(seed * 1000);
        for _ in 0..3000 {
            let start = Vec3::new(
                rng.range(-2000.0, 2000.0),
                rng.range(-2000.0, 2000.0),
                rng.range(-500.0, 500.0),
            );
            let end = start
                + match rng.next() % 4 {
                    0 => Vec3::new(0.0, 0.0, rng.range(-800.0, 800.0)),
                    1 => Vec3::new(rng.range(-800.0, 800.0), rng.range(-800.0, 800.0), 0.0),
                    _ => Vec3::new(
                        rng.range(-800.0, 800.0),
                        rng.range(-800.0, 800.0),
                        rng.range(-800.0, 800.0),
                    ),
                };
            // Clearly separated starts only (the two worlds treat touching
            // and penetrating starts with different tolerances).
            let margin = CollisionShape {
                radius: SHAPE.radius + 0.1,
                half_height: SHAPE.half_height + 0.1,
            };
            if boxes.overlaps(start, margin) {
                continue;
            }
            let a = boxes.sweep_capsule(start, end, SHAPE);
            let b = tris.sweep_capsule(start, end, SHAPE);
            let len = (end - start).length();
            match (a, b) {
                (None, None) => {}
                (Some(a), Some(b)) => {
                    compared += 1;
                    // BoxWorld solves in f32 (about 1e-3 uu of root error at
                    // these coordinates); the triangle world in f64.
                    let dt = (a.time - b.time).abs() * len;
                    assert!(dt < 1e-2, "{start} -> {end}: box {a:?} tri {b:?}");
                    if a.normal.dot(b.normal) < 0.999 {
                        normal_mismatches += 1;
                    }
                }
                (a, b) => {
                    // Only grazes within the tolerance may differ.
                    let h = a.or(b).unwrap();
                    let p = start + (end - start) * h.time;
                    let shrunk = CollisionShape {
                        radius: SHAPE.radius - 0.01,
                        half_height: SHAPE.half_height - 0.01,
                    };
                    let further = p + (end - start).normalize_or_zero() * 0.05;
                    assert!(
                        !boxes.overlaps(further, shrunk),
                        "{start} -> {end}: box {a:?} tri {b:?}"
                    );
                }
            }
            // Rays (zero extent).
            let dir = end - start;
            let ra = boxes.raycast(start, dir, len);
            let rb = tris.raycast(start, dir, len);
            match (ra, rb) {
                (Some(x), Some(y)) => {
                    assert!(
                        (x.distance - y.distance).abs() < 1e-2,
                        "ray {start} {dir}: {x:?} {y:?}"
                    );
                    assert!(x.normal.dot(y.normal) > 0.999, "ray normal {x:?} {y:?}");
                }
                (None, None) => {}
                (x, y) => panic!("ray {start} {dir}: {x:?} vs {y:?}"),
            }
        }
    }
    assert!(compared > 1500, "{compared}");
    // Normals may differ only where several features touch at once.
    assert!(
        normal_mismatches * 100 <= compared,
        "{normal_mismatches} of {compared}"
    );
}
