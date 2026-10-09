//! The converted-level loader on synthetic data (no game data): collision
//! placement and flags, gameplay definitions, streaming, mesh variants and
//! the glTF fallback, and hostile inputs.

use asamu_world::SurfaceTag;
use asamu_world::collision::{CollisionClass, InstanceRef, QueryFilter, QueryKind};
use asamu_world::fixtures::{MeshFixtures, Place, SceneFixture, box_mesh};
use asamu_world::gameplay::{RockKind, VolumeKind};
use asamu_world::scene::{self, DataSource, LoadOptions, MemorySource, SceneError};
use glam::Vec3;
use serde_json::json;

const MAP: &str = "AG-Synthetic";

/// A 4000 × 4000 floor at z = 0 (BSP), a 200³ crate (static mesh), a
/// blocking volume, a kill zone below, a checkpoint with a marker, a trigger,
/// a crystal and a falling rock.
fn fixture(ucx: bool) -> MemorySource {
    let mut src = MemorySource::new();
    let mut s = SceneFixture::new(MAP, -5000.0).with_title("Synthetic");
    s.set_bsp(
        vec![
            [-2000.0, -2000.0, 0.0],
            [2000.0, -2000.0, 0.0],
            [2000.0, 2000.0, 0.0],
            [-2000.0, 2000.0, 0.0],
        ],
        vec![[0, 1, 2], [0, 2, 3]],
    );
    s.player_start(Vec3::new(0.0, 0.0, 80.0), 0);
    s.static_mesh("Pkg.Meshes.Crate", Place::at(Vec3::new(500.0, 0.0, 0.0)));
    s.blocking_volume(
        Vec3::new(-600.0, -100.0, 0.0),
        Vec3::new(-500.0, 100.0, 300.0),
    );
    s.kill_zone(
        Vec3::new(-3000.0, -3000.0, -3000.0),
        Vec3::new(3000.0, 3000.0, -1000.0),
    );
    let marker = s.marker(Place {
        location: Vec3::new(1000.0, 1000.0, 0.0),
        rotation: [0, 16_384, 0],
        scale: Vec3::ONE,
    });
    let marker_path = s.path(marker);
    s.checkpoint(
        Place::at(Vec3::new(1000.0, 1000.0, 40.0)),
        300.0,
        100.0,
        1,
        json!({"spawnPointActor": marker_path, "spawnPointOffset": {"X": -300.0, "Y": 0.0, "Z": 0.0}}),
    );
    s.trigger(Vec3::new(-1000.0, 0.0, 40.0), 100.0, 50.0);
    s.mesh_actor(
        "asamu.ASAMURechargeCrystal",
        "recharge_crystal",
        "Pkg.Meshes.Crystal",
        Place::at(Vec3::new(0.0, 1500.0, 500.0)),
        "COLLIDE_BlockAll",
        None,
        json!({"RechargeDelay": 4.0, "bShouldRecharge": true}),
    );
    s.mesh_actor(
        "asamu.ASAMUFallingRock",
        "falling_rock",
        "Pkg.Meshes.Crystal",
        Place::at(Vec3::new(0.0, -1500.0, 800.0)),
        "COLLIDE_CustomDefault",
        Some("NotGrappleAble"),
        json!({"fallDistance": 500.0, "fallingLowerRate": 400.0, "fallingHigherRate": 400.0}),
    );
    s.write(&mut src);
    let mut meshes = MeshFixtures::new();
    let (v, t) = box_mesh(
        Vec3::new(-100.0, -100.0, 0.0),
        Vec3::new(100.0, 100.0, 200.0),
    );
    meshes.add("Pkg.Meshes.Crate", MAP, v, t);
    let (v, t) = box_mesh(Vec3::splat(-50.0), Vec3::splat(50.0));
    meshes.add("Pkg.Meshes.Crystal", MAP, v, t);
    meshes.write(&mut src, ucx);
    src
}

#[test]
fn synthetic_map_loads_with_collision_and_gameplay() {
    for ucx in [true, false] {
        let src = fixture(ucx);
        let map = scene::load_map(&src, "ag-synthetic", &LoadOptions::default()).unwrap();
        assert_eq!(map.map, MAP);
        assert_eq!(map.world.title.as_deref(), Some("Synthetic"));
        assert_eq!(map.world.kill_z, -5000.0);
        assert_eq!(map.stats.meshes_loaded, 2, "{:?}", map.warnings);
        assert_eq!(map.stats.bsp_triangles, 2);
        assert_eq!(map.stats.blocking_hulls, 1);
        assert_eq!(map.dynamic.len(), 1, "the rock is dynamic");
        assert!(map.warnings.is_empty(), "{:?}", map.warnings);
        let c = &map.collision;
        // Landing on the BSP floor.
        let hit = c
            .sweep_cylinder(
                &map.dynamic,
                Vec3::new(0.0, 0.0, 300.0),
                Vec3::new(0.0, 0.0, -100.0),
                21.0,
                44.0,
                QueryFilter::PAWN,
            )
            .unwrap();
        assert!((hit.t - 256.0 / 400.0).abs() < 1e-9);
        let inst = c.instance(&map.dynamic, hit.instance).unwrap();
        assert_eq!(inst.info.class, CollisionClass::WorldGeometry);
        // Walking into the crate (static mesh at x = 400..600).
        let hit = c
            .sweep_cylinder(
                &map.dynamic,
                Vec3::new(0.0, 0.0, 50.0),
                Vec3::new(1000.0, 0.0, 50.0),
                21.0,
                44.0,
                QueryFilter::PAWN,
            )
            .unwrap();
        assert!((hit.t - 379.0 / 1000.0).abs() < 1e-9, "{hit:?}");
        assert_eq!(
            c.instance(&map.dynamic, hit.instance).unwrap().info.class,
            CollisionClass::StaticMesh
        );
        // The blocking volume blocks the pawn but not traces.
        let pawn = c
            .sweep_cylinder(
                &map.dynamic,
                Vec3::new(0.0, 0.0, 50.0),
                Vec3::new(-1000.0, 0.0, 50.0),
                21.0,
                44.0,
                QueryFilter::PAWN,
            )
            .unwrap();
        assert!((pawn.t - 479.0 / 1000.0).abs() < 1e-9, "{pawn:?}");
        assert!(
            c.raycast(
                &map.dynamic,
                Vec3::new(0.0, 0.0, 50.0),
                Vec3::new(-700.0, 0.0, 50.0),
                QueryFilter::TRACE
            )
            .is_none()
        );
        // The crystal (collision type BlockAll, actor flags clear) blocks both.
        let ray = c
            .raycast(
                &map.dynamic,
                Vec3::new(0.0, 1000.0, 500.0),
                Vec3::new(0.0, 2000.0, 500.0),
                QueryFilter::TRACE,
            )
            .unwrap();
        assert_eq!(
            c.instance(&map.dynamic, ray.instance).unwrap().info.class,
            CollisionClass::RechargeCrystal
        );
        // The rock is forced to BlockAll, dynamic, tagged NotGrappleAble.
        let ray = c
            .raycast(
                &map.dynamic,
                Vec3::new(0.0, -1000.0, 800.0),
                Vec3::new(0.0, -2000.0, 800.0),
                QueryFilter::TRACE,
            )
            .unwrap();
        assert!(matches!(ray.instance, InstanceRef::Dynamic(0)));
        let info = c.instance(&map.dynamic, ray.instance).unwrap().info;
        assert_eq!(info.class, CollisionClass::FallingRock);
        assert!(!info.grapple_able);
        assert_eq!(info.tag, SurfaceTag::None);
        // Gameplay definitions.
        let a = &map.actors;
        assert_eq!(
            a.player_start().unwrap().location,
            Vec3::new(0.0, 0.0, 80.0)
        );
        assert_eq!(a.checkpoints.len(), 1);
        let cp = &a.checkpoints[0];
        assert_eq!((cp.index, cp.radius, cp.half_height), (1, 300.0, 100.0));
        // Spawn = marker + offset rotated by the marker's yaw (a quarter
        // turn: −300 along X becomes −300 along Y); rotation = the marker's.
        assert!(
            (cp.spawn_location - Vec3::new(1000.0, 700.0, 0.0)).length() < 1e-3,
            "{}",
            cp.spawn_location
        );
        assert_eq!(cp.spawn_rotation, [0, 16_384, 0]);
        assert_eq!(a.volumes.len(), 1);
        assert_eq!(a.volumes[0].kind, VolumeKind::KillZone);
        assert_eq!(a.triggers.len(), 1);
        assert_eq!(a.crystals.len(), 1);
        assert_eq!(a.crystals[0].recharge_delay, 4.0);
        assert!((a.crystals[0].half_extent - 50.0).abs() < 1e-3);
        assert_eq!(a.rocks.len(), 1);
        assert_eq!(a.rocks[0].kind, RockKind::Falling);
        assert_eq!(a.rocks[0].params.fall_distance, 500.0);
        assert_eq!(a.rocks[0].body, Some(0));
    }
}

#[test]
fn streaming_levels_and_mesh_variants() {
    let mut src = MemorySource::new();
    let mut main = SceneFixture::new("AG-Main", -1.0e7);
    main.player_start(Vec3::new(0.0, 0.0, 100.0), 0);
    main.stream("sub_always", "LevelStreamingAlwaysLoaded");
    main.stream("sub_kismet", "LevelStreamingKismet");
    main.stream("missing_level", "LevelStreamingKismet");
    main.static_mesh("Pkg.M", Place::at(Vec3::ZERO));
    main.write(&mut src);
    let mut a = SceneFixture::new("Sub_Always", -1.0e7);
    a.static_mesh("Pkg.M", Place::at(Vec3::new(1000.0, 0.0, 0.0)));
    a.write(&mut src);
    let mut k = SceneFixture::new("Sub_Kismet", -1.0e7);
    k.static_mesh("Pkg.M", Place::at(Vec3::new(2000.0, 0.0, 0.0)));
    k.write(&mut src);
    let mut meshes = MeshFixtures::new();
    let (v, t) = box_mesh(Vec3::splat(-10.0), Vec3::splat(10.0));
    meshes.add("Pkg.M", "AG-Main", v, t);
    meshes.differs_in("Pkg.M", "Sub_Kismet");
    let (v, t) = box_mesh(Vec3::splat(-50.0), Vec3::splat(50.0));
    meshes.add("Pkg.M@Sub_Kismet", "Sub_Kismet", v, t);
    meshes.write(&mut src, true);
    let map = scene::load_map(&src, "AG-Main", &LoadOptions::default()).unwrap();
    assert_eq!(map.levels.len(), 3);
    assert!(map.levels[1].initially_loaded);
    assert!(!map.levels[2].initially_loaded);
    assert_eq!(map.initial_level_mask(), 0b011);
    assert_eq!(map.level_index("sub_kismet"), Some(2));
    assert!(
        map.warnings.iter().any(|w| w.contains("missing_level")),
        "{:?}",
        map.warnings
    );
    // The Kismet level's copy of the mesh is the differing variant (50 uu).
    let probe = |mask: u64| {
        map.collision.raycast(
            &map.dynamic,
            Vec3::new(2000.0, 0.0, 500.0),
            Vec3::new(2000.0, 0.0, -500.0),
            QueryFilter {
                kind: QueryKind::Trace,
                sublevels: mask,
            },
        )
    };
    assert!(probe(0b011).is_none(), "not streamed in yet");
    let hit = probe(0b111).unwrap();
    assert!((hit.t * 1000.0 - 450.0).abs() < 1e-6, "{hit:?}");
    assert_eq!(
        map.collision
            .instance(&map.dynamic, hit.instance)
            .unwrap()
            .info
            .sublevel,
        2
    );
}

#[test]
fn errors_and_warnings() {
    let src = MemorySource::new();
    assert_eq!(
        scene::load_map(&src, "AG-Nothing", &LoadOptions::default()).unwrap_err(),
        SceneError::MissingMap("AG-Nothing".to_owned())
    );
    // Wrong format.
    let mut src = MemorySource::new();
    src.insert(
        "levels/X.scene.json",
        br#"{"format":"other","version":1,"package":"X"}"#.to_vec(),
    );
    assert!(matches!(
        scene::load_map(&src, "X", &LoadOptions::default()),
        Err(SceneError::Format { .. })
    ));
    src.insert(
        "levels/X.scene.json",
        br#"{"format":"asamu-scene","version":2,"package":"X"}"#.to_vec(),
    );
    assert!(matches!(
        scene::load_map(&src, "X", &LoadOptions::default()),
        Err(SceneError::Format { .. })
    ));
    src.insert("levels/X.scene.json", b"{not json".to_vec());
    assert!(matches!(
        scene::load_map(&src, "X", &LoadOptions::default()),
        Err(SceneError::Json { .. })
    ));
    // Too many actors.
    let mut s = SceneFixture::new("Y", 0.0);
    for i in 0..10 {
        s.player_start(Vec3::splat(i as f32), 0);
    }
    let mut src = MemorySource::new();
    s.write(&mut src);
    let opts = LoadOptions {
        max_actors: 5,
        ..LoadOptions::default()
    };
    assert!(matches!(
        scene::load_map(&src, "Y", &opts),
        Err(SceneError::Format { .. })
    ));
    // No mesh manifest: a warning, collision from the rest.
    let map = scene::load_map(&src, "Y", &LoadOptions::default()).unwrap();
    assert!(
        map.warnings
            .iter()
            .any(|w| w.contains("no static-mesh collision"))
    );
}

#[test]
fn hostile_mesh_and_bsp_data_never_panic() {
    let base = fixture(true);
    let files: Vec<String> = [
        "meshes/manifest.json",
        "levels/AG-Synthetic.bsp.json",
        "levels/AG-Synthetic.bsp.bin",
    ]
    .into_iter()
    .map(str::to_owned)
    .chain(
        base.list("meshes/AG-Synthetic")
            .unwrap()
            .into_iter()
            .map(|f| format!("meshes/AG-Synthetic/{f}")),
    )
    .collect();
    assert!(files.len() >= 7, "{files:?}");
    let mut loads = 0;
    for file in &files {
        let original = base.read(file, u64::MAX).unwrap();
        let positions: Vec<usize> = (0..original.len())
            .step_by((original.len() / 64).max(1))
            .collect();
        for &pos in &positions {
            for value in [0x00u8, 0xFF, b'9', b'-', b'"'] {
                let mut data = original.clone();
                data[pos] = value;
                let mut src = base.clone();
                src.insert(file.clone(), data);
                // Must not panic; errors and warnings are fine.
                let _ = scene::load_map(&src, MAP, &LoadOptions::default());
                loads += 1;
            }
        }
        // Truncations.
        for cut in [0, original.len() / 3, original.len().saturating_sub(1)] {
            let mut src = base.clone();
            src.insert(file.clone(), original[..cut].to_vec());
            let _ = scene::load_map(&src, MAP, &LoadOptions::default());
        }
    }
    assert!(loads > 500);
    // Out-of-range indices and unsafe paths in the manifest are reported.
    let mut src = base.clone();
    let mut manifest: serde_json::Value =
        serde_json::from_slice(&base.read("meshes/manifest.json", u64::MAX).unwrap()).unwrap();
    manifest["meshes"]["Pkg.Meshes.Crate"]["lods"][0]["gltf"] = json!("../../etc/passwd");
    src.insert("meshes/manifest.json", manifest.to_string().into_bytes());
    let map = scene::load_map(&src, MAP, &LoadOptions::default()).unwrap();
    assert_eq!(map.stats.meshes_failed, 1);
    assert!(
        map.warnings.iter().any(|w| w.contains("unsafe")),
        "{:?}",
        map.warnings
    );
    // A BSP whose spans exceed the binary.
    let mut src = base.clone();
    let mut index: serde_json::Value =
        serde_json::from_slice(&base.read("levels/AG-Synthetic.bsp.json", u64::MAX).unwrap())
            .unwrap();
    index["meshes"]["collision"]["triangles"]["count"] = json!(1_000_000);
    src.insert(
        "levels/AG-Synthetic.bsp.json",
        index.to_string().into_bytes(),
    );
    let map = scene::load_map(&src, MAP, &LoadOptions::default()).unwrap();
    assert_eq!(map.stats.bsp_triangles, 0);
    assert!(map.warnings.iter().any(|w| w.contains("BSP")));
}
