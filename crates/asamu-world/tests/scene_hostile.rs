//! Hostile converted data (synthetic only): inputs crafted to amplify memory,
//! overflow offsets or flip hull orientations must be refused or contained,
//! never panic, and never turn solid geometry into something passable.

use asamu_world::SurfaceTag;
use asamu_world::collision::{
    Affine, CollisionClass, CollisionSceneBuilder, InstanceInfo, QueryFilter,
};
use asamu_world::fixtures::{MeshFixtures, Place, SceneFixture, box_mesh};
use asamu_world::gameplay::{DeathCause, RockDef, RockKind, RockParams, SceneActors, SceneRuntime};
use asamu_world::scene::{self, DataSource, LoadOptions, MemorySource};
use glam::Vec3;
use serde_json::{Value, json};

const MAP: &str = "AG-Hostile";
const MESH: &str = "Pkg.Meshes.Box";

/// A map placing one box mesh, written by the fixtures.
fn base(place: Place) -> MemorySource {
    let mut src = MemorySource::new();
    let mut s = SceneFixture::new(MAP, -5000.0);
    s.player_start(Vec3::new(0.0, 0.0, 80.0), 0);
    s.static_mesh(MESH, place);
    s.write(&mut src);
    let mut meshes = MeshFixtures::new();
    let (v, t) = box_mesh(Vec3::splat(-50.0), Vec3::splat(50.0));
    meshes.add(MESH, MAP, v, t);
    meshes.write(&mut src, true);
    src
}

fn read_json(src: &MemorySource, path: &str) -> Value {
    serde_json::from_slice(&src.read(path, u64::MAX).expect("file")).expect("json")
}

/// The glTF path of the box mesh.
fn gltf_path(src: &MemorySource) -> (String, String) {
    let m = read_json(src, "meshes/manifest.json");
    let lod = &m["meshes"][MESH]["lods"][0];
    (
        format!("meshes/{}", lod["gltf"].as_str().expect("gltf")),
        format!("meshes/{}", lod["bin"].as_str().expect("bin")),
    )
}

#[test]
fn distinct_position_accessors_cannot_amplify_the_vertex_count() {
    // 64 collision primitives, each with its own POSITION accessor over the
    // same 8 vertices and no triangles: without a vertex cap the loader
    // would copy the vertices 64 times (any number of times, from a tiny
    // file).
    let mut src = base(Place::at(Vec3::new(500.0, 0.0, 0.0)));
    let (gltf, _) = gltf_path(&src);
    let mut g = read_json(&src, &gltf);
    let mut accessors = vec![
        json!({"bufferView": 0, "componentType": 5126, "count": 8, "type": "VEC3"}),
        json!({"bufferView": 1, "componentType": 5123, "count": 0, "type": "SCALAR"}),
    ];
    let mut prims = Vec::new();
    for k in 0..64 {
        accessors.push(json!({"bufferView": 0, "componentType": 5126, "count": 8, "type": "VEC3"}));
        prims.push(json!({"attributes": {"POSITION": k + 2}, "indices": 1, "mode": 4}));
    }
    g["accessors"] = json!(accessors);
    g["meshes"][1]["primitives"] = json!(prims);
    src.insert(gltf.clone(), g.to_string().into_bytes());
    let opts = LoadOptions {
        max_mesh_triangles: 10,
        ..LoadOptions::default()
    };
    let map = scene::load_map(&src, MAP, &opts).expect("map loads");
    assert_eq!(map.stats.meshes_failed, 1, "{:?}", map.warnings);
    assert!(
        map.warnings.iter().any(|w| w.contains("too many vertices")),
        "{:?}",
        map.warnings
    );
    // With room for them the same file is merely a mesh without triangles.
    let map = scene::load_map(&src, MAP, &LoadOptions::default()).expect("map loads");
    assert_eq!(map.stats.meshes_failed, 0, "{:?}", map.warnings);
    assert_eq!(map.stats.meshes_without_collision, 1);
}

#[test]
fn oversized_index_accessors_are_refused_before_reading() {
    let mut src = base(Place::at(Vec3::new(500.0, 0.0, 0.0)));
    let (gltf, bin) = gltf_path(&src);
    let bin_len = src.read(&bin, u64::MAX).expect("bin").len();
    let mut g = read_json(&src, &gltf);
    // One byte per index over the whole buffer.
    g["bufferViews"] = json!([
        g["bufferViews"][0].clone(),
        {"buffer": 0, "byteOffset": 0, "byteLength": bin_len},
    ]);
    g["accessors"][1] =
        json!({"bufferView": 1, "componentType": 5121, "count": bin_len, "type": "SCALAR"});
    src.insert(gltf, g.to_string().into_bytes());
    let opts = LoadOptions {
        max_mesh_triangles: 10,
        ..LoadOptions::default()
    };
    let map = scene::load_map(&src, MAP, &opts).expect("map loads");
    assert_eq!(map.stats.meshes_failed, 1);
    assert!(
        map.warnings
            .iter()
            .any(|w| w.contains("too many triangles")),
        "{:?}",
        map.warnings
    );
}

#[test]
fn bsp_offsets_near_the_address_limit_do_not_overflow() {
    let mut src = base(Place::at(Vec3::new(500.0, 0.0, 0.0)));
    let mut s = SceneFixture::new(MAP, -5000.0);
    s.player_start(Vec3::new(0.0, 0.0, 80.0), 0);
    s.set_bsp(
        vec![[0.0, 0.0, 0.0], [100.0, 0.0, 0.0], [0.0, 100.0, 0.0]],
        vec![[0, 1, 2]],
    );
    s.write(&mut src);
    let path = format!("levels/{MAP}.bsp.json");
    for (span, offset) in [
        ("positions", u64::MAX - 2),
        ("positions", u64::MAX - 6),
        ("triangles", u64::MAX - 2),
        ("triangles", u64::MAX - 10),
        ("positions", u64::MAX),
    ] {
        let mut index = read_json(&src, &path);
        index["meshes"]["collision"][span]["offset"] = json!(offset);
        let mut bad = src.clone();
        bad.insert(path.clone(), index.to_string().into_bytes());
        // Must not panic (debug builds check overflow); the BSP is dropped
        // with a warning.
        let map = scene::load_map(&bad, MAP, &LoadOptions::default()).expect("map loads");
        assert_eq!(map.stats.bsp_triangles, 0);
        assert!(
            map.warnings.iter().any(|w| w.contains("BSP")),
            "{:?}",
            map.warnings
        );
    }
    // Huge counts are refused before any allocation.
    let mut index = read_json(&src, &path);
    index["meshes"]["collision"]["positions"]["count"] = json!(u64::MAX);
    src.insert(path, index.to_string().into_bytes());
    let map = scene::load_map(&src, MAP, &LoadOptions::default()).expect("map loads");
    assert_eq!(map.stats.bsp_triangles, 0);
    // 64-bit targets refuse the count; 32-bit ones cannot parse it.
    let refused = if usize::BITS == 64 {
        "too many BSP"
    } else {
        "BSP"
    };
    assert!(
        map.warnings.iter().any(|w| w.contains(refused)),
        "{:?}",
        map.warnings
    );
}

fn info() -> InstanceInfo {
    InstanceInfo {
        actor: None,
        class: CollisionClass::BlockingVolume,
        tag: SurfaceTag::None,
        grapple_able: true,
        blocks_pawn: true,
        blocks_traces: true,
        sublevel: 0,
    }
}

#[test]
fn stray_hull_vertices_do_not_flip_a_blocking_volume() {
    // A 400³ hull plus vertices no triangle uses: one far away, one
    // infinite (what an out-of-range number in the JSON parses to).
    for stray in [
        Vec3::new(1.0e30, 0.0, 0.0),
        Vec3::new(f32::INFINITY, 0.0, 0.0),
    ] {
        let (v, t) = box_mesh(Vec3::splat(-200.0), Vec3::splat(200.0));
        let mut v: Vec<Vec3> = v.into_iter().map(Vec3::from_array).collect();
        v.push(stray);
        let mut b = CollisionSceneBuilder::new();
        let m = b.add_convex_mesh(v, t).expect("hull");
        b.add_static(m, Affine::IDENTITY, info()).expect("instance");
        let s = b.build();
        for dir in [Vec3::X, -Vec3::X, Vec3::Y, Vec3::Z] {
            let from = dir * 600.0;
            let hit = s
                .sweep_cylinder(&[], from, Vec3::ZERO, 21.0, 44.0, QueryFilter::PAWN)
                .unwrap_or_else(|| panic!("passed through the hull from {dir} (stray {stray})"));
            assert!(hit.normal.dot(dir.as_dvec3()) > 0.99, "{hit:?}");
            assert!(
                s.sweep_cylinder(&[], Vec3::ZERO, from, 21.0, 44.0, QueryFilter::PAWN)
                    .is_none(),
                "trapped inside towards {dir}"
            );
        }
        assert!(s.overlaps_cylinder(&[], Vec3::ZERO, 21.0, 44.0, QueryFilter::PAWN));
        assert!(!s.overlaps_cylinder(&[], Vec3::X * 300.0, 21.0, 44.0, QueryFilter::PAWN));
    }
}

#[test]
fn stray_hull_vertices_do_not_disable_a_kill_zone() {
    let mut src = MemorySource::new();
    let mut s = SceneFixture::new(MAP, -50_000.0);
    s.player_start(Vec3::new(0.0, 0.0, 80.0), 0);
    let kz = s.kill_zone(
        Vec3::new(-500.0, -500.0, -2000.0),
        Vec3::new(500.0, 500.0, -1000.0),
    );
    s.write(&mut src);
    let path = format!("levels/{MAP}.scene.json");
    let mut scene_json = read_json(&src, &path);
    let hull = &mut scene_json["actors"][kz]["volume"]["hulls"][0];
    if let Some(v) = hull["vertices"].as_array_mut() {
        v.push(json!([1.0e30, 0.0, 0.0]));
    }
    src.insert(path, scene_json.to_string().into_bytes());
    let map = scene::load_map(&src, MAP, &LoadOptions::default()).expect("map");
    assert_eq!(map.actors.volumes.len(), 1);
    let mut rt = SceneRuntime::new(&map.actors, map.initial_level_mask(), 1);
    let mut events = Vec::new();
    let out = rt.update_touches(
        &map.actors,
        map.world.kill_z,
        Vec3::new(0.0, 0.0, -500.0),
        Vec3::new(0.0, 0.0, -1500.0),
        21.0,
        44.0,
        &mut events,
    );
    assert_eq!(out.death.map(|d| d.0), Some(DeathCause::KillZone));
    // And beside it nothing happens.
    let mut rt = SceneRuntime::new(&map.actors, map.initial_level_mask(), 1);
    let out = rt.update_touches(
        &map.actors,
        map.world.kill_z,
        Vec3::new(2000.0, 0.0, -500.0),
        Vec3::new(2000.0, 0.0, -1500.0),
        21.0,
        44.0,
        &mut events,
    );
    assert!(out.death.is_none());
}

#[test]
fn baked_copies_of_singular_placements_count_against_the_budget() {
    // 50 placements of the 12-triangle box with a zero X scale: each is
    // baked into its own world-space copy.
    let mut src = MemorySource::new();
    let mut s = SceneFixture::new(MAP, -5000.0);
    s.player_start(Vec3::new(0.0, 0.0, 80.0), 0);
    for i in 0..50 {
        s.static_mesh(
            MESH,
            Place {
                location: Vec3::new(300.0 * i as f32, 1000.0, 0.0),
                rotation: [0; 3],
                scale: Vec3::new(0.0, 1.0, 1.0),
            },
        );
    }
    s.write(&mut src);
    let mut meshes = MeshFixtures::new();
    let (v, t) = box_mesh(Vec3::splat(-50.0), Vec3::splat(50.0));
    meshes.add(MESH, MAP, v, t);
    meshes.write(&mut src, true);
    let opts = LoadOptions {
        max_total_triangles: 12 * 4,
        ..LoadOptions::default()
    };
    let map = scene::load_map(&src, MAP, &opts).expect("map");
    // The mesh itself plus three copies fit.
    assert_eq!(map.stats.placed_components, 3, "{:?}", map.stats);
    assert!(map.collision.stats().mesh_triangles <= 12 * 4);
    assert!(map.warnings.iter().any(|w| w.contains("budget")));
    // Unlimited: all placed, as flat copies that still collide.
    let map = scene::load_map(&src, MAP, &LoadOptions::default()).expect("map");
    assert_eq!(map.stats.placed_components, 50);
    let hit = map.collision.raycast(
        &map.dynamic,
        Vec3::new(-100.0, 1000.0, 0.0),
        Vec3::new(100.0, 1000.0, 0.0),
        QueryFilter::TRACE,
    );
    assert!(hit.is_some());
}

#[test]
fn hostile_rock_parameters_keep_rocks_finite() {
    let params = RockParams {
        respawn_at_start: true,
        falling_lower_rate: -f32::MAX,
        falling_higher_rate: f32::MAX,
        fall_distance: f32::MAX,
        accel_rate: f32::MAX,
        decel_rate: f32::MAX,
        update_rate: 1.0e30,
        should_rotate: true,
        rotation_lower_rate: Vec3::splat(-f32::MAX),
        rotation_higher_rate: Vec3::splat(f32::MAX),
        rotation_accel_rate: f32::MAX,
        reach_max_speed_time: f32::MIN_POSITIVE,
    };
    let rock = |id: u32, kind: RockKind| RockDef {
        id,
        name: format!("rock{id}"),
        kind,
        location: Vec3::new(0.0, 0.0, 1000.0),
        rotation: [0; 3],
        params,
        body: None,
    };
    let actors = SceneActors {
        rocks: vec![
            rock(1, RockKind::Falling),
            rock(2, RockKind::FallingWhenGrappled),
        ],
        ..SceneActors::default()
    };
    let mut rt = SceneRuntime::new(&actors, 1, 7);
    rt.set_falling_rocks_active(&actors, true);
    let mut ev = Vec::new();
    rt.rock_grappled(&actors, 2, &mut ev);
    let scene = asamu_world::collision::CollisionScene::empty();
    for i in 0..400 {
        rt.tick_rocks(&actors, &[], &scene, &mut [], 1.0 / 60.0);
        if i == 100 {
            rt.rock_grappled(&actors, 1, &mut ev);
        }
        if i == 200 {
            rt.rock_ungrappled(&actors, 1);
            rt.reset_grapple_rocks(&actors);
        }
        for s in &rt.rocks {
            assert!(s.location.is_finite() && s.fall_rate.is_finite(), "{s:?}");
        }
    }
}
