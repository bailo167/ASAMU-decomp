//! Loading NPC definitions from converted scenes (synthetic JSON written
//! here, hostile variants) and the real-data census (skips without
//! `ASAMU_CONVERTED_DIR`).

use asamu_world::npc::{
    COLLECTIBLE_TRIGGER_RADIUS, NpcHull, SkeletalIndex, SkinnedDrive, WormVolumeRole,
    load_npc_scene, load_npc_scene_for_map,
};
use asamu_world::scene::{DataSource, DirSource, LoadOptions, MemorySource, SubLevel};
use glam::Vec3;
use serde_json::{Value, json};

fn m(t: [f32; 3]) -> Value {
    json!([
        [1.0, 0.0, 0.0, 0.0],
        [0.0, 1.0, 0.0, 0.0],
        [0.0, 0.0, 1.0, 0.0],
        [t[0], t[1], t[2], 1.0]
    ])
}

fn actor(slot: usize, name: &str, class: &str, kind: &str, loc: [f32; 3], extra: Value) -> Value {
    let mut a = json!({
        "slot": slot, "name": name, "class": class, "kind": kind,
        "location": loc, "rotation": [0, 0, 0], "local_to_world": m(loc),
        "components": [], "params": {}, "matinee": []
    });
    if let (Some(dst), Some(src)) = (a.as_object_mut(), extra.as_object()) {
        for (k, v) in src {
            dst.insert(k.clone(), v.clone());
        }
    }
    a
}

fn box_hull(min: [f32; 3], max: [f32; 3]) -> Value {
    let v: Vec<[f32; 3]> = (0..8)
        .map(|i| {
            [
                if i & 1 == 0 { min[0] } else { max[0] },
                if i & 2 == 0 { min[1] } else { max[1] },
                if i & 4 == 0 { min[2] } else { max[2] },
            ]
        })
        .collect();
    json!({"vertices": v, "triangles": [], "planes": [
        [1.0, 0.0, 0.0, max[0]], [-1.0, 0.0, 0.0, -min[0]],
        [0.0, 1.0, 0.0, max[1]], [0.0, -1.0, 0.0, -min[1]],
        [0.0, 0.0, 1.0, max[2]], [0.0, 0.0, -1.0, -min[2]]
    ]})
}

fn scene_json(package: &str, actors: Vec<Value>, streaming: Vec<Value>) -> Vec<u8> {
    serde_json::to_vec(&json!({
        "format": "asamu-scene", "version": 1, "package": package,
        "world_info": {"kill_z": -10000.0},
        "streaming_levels": streaming, "actors": actors
    }))
    .expect("scene JSON")
}

fn npc_test_map() -> MemorySource {
    let p = "AG-NpcTest.TheWorld.PersistentLevel.";
    let actors = vec![
        actor(
            0,
            "WorldInfo_0",
            "Engine.WorldInfo",
            "world_info",
            [0.0; 3],
            json!({}),
        ),
        actor(
            1,
            "PointLight_1",
            "Engine.PointLight",
            "light",
            [10.0, 20.0, 30.0],
            json!({}),
        ),
        actor(
            2,
            "PointLight_2",
            "Engine.PointLight",
            "light",
            [40.0, 50.0, 60.0],
            json!({}),
        ),
        actor(
            3,
            "ASAMUNPC_WormPawn_0",
            "asamu.ASAMUNPC_WormPawn",
            "asamu_other",
            [1000.0, 0.0, 200.0],
            json!({
                "params": {
                    "RandomLookAtTargets": [format!("{p}PointLight_1"), format!("{p}PointLight_2"), "Missing.Thing"],
                    "Radius": 100000.0, "Brightness": 25.0, "OuterConeAngle": 40.0, "InnerConeAngle": 15.0,
                    "LightColor": {"R": 255, "G": 247, "B": 247, "A": 255}
                },
                "components": [{"name": "SkeletalMeshComponent_0", "kind": "skeletal_mesh",
                                "local_to_world": m([1000.0, 0.0, 200.0]),
                                "skeletal_mesh": "Dark_Cave_worm.SkeletalMesh.Worm_SkeletalMesh"}]
            }),
        ),
        actor(
            4,
            "ASAMUWormScreamVolume_0",
            "asamu.ASAMUWormScreamVolume",
            "trigger_volume",
            [0.0; 3],
            json!({
                "volume": {"hulls": [box_hull([-500.0, -500.0, -100.0], [500.0, 500.0, 400.0])]}
            }),
        ),
        actor(
            5,
            "ASAMUCollectible_0",
            "asamu.ASAMUCollectible",
            "collectible",
            [100.0, 0.0, 0.0],
            json!({
                "params": {"CylinderComponent": format!("{p}ASAMUCollectible_0.Trigger")},
                "components": [{"name": "Trigger", "kind": "cylinder", "local_to_world": m([100.0, 0.0, 40.0]),
                                "cylinder": [50.0, 40.0]}]
            }),
        ),
        actor(
            6,
            "ASAMUCollectible_1",
            "asamu.ASAMUCollectible",
            "collectible",
            [300.0, 0.0, 0.0],
            json!({}),
        ),
        actor(
            7,
            "ASAMUInteractable_Actor_0",
            "asamu.ASAMUInteractable_Actor",
            "asamu_other",
            [0.0; 3],
            json!({
                "params": {"bParentInteractable": true, "bIsOptional": true, "MaxInteractTimes": 1,
                           "linkedInteractables": [format!("{p}ASAMUInteractable_Actor_1")]}
            }),
        ),
        actor(
            8,
            "ASAMUInteractable_Actor_1",
            "asamu.ASAMUInteractable_Actor",
            "asamu_other",
            [0.0; 3],
            json!({
                "params": {"linkedParentActor": format!("{p}ASAMUInteractable_Actor_0"), "MaxInteractTimes": 0,
                           "GlowMesh": null}
            }),
        ),
        actor(
            9,
            "ASAMUGlowFlower_0",
            "asamu.ASAMUGlowFlower",
            "asamu_other",
            [0.0; 3],
            json!({
                "params": {"glowDuration": 5.0, "EditorFlowerArray": [format!("{p}PointLight_1")]}
            }),
        ),
        actor(
            10,
            "ASAMUSoundMakingFoliage_0",
            "asamu.ASAMUSoundMakingFoliage",
            "static_mesh",
            [700.0, 0.0, 0.0],
            json!({
                "params": {"TouchSound": "MiscSounds.Foliage_Rustle_Cue", "CylinderComponent": "Trigger"},
                "components": [{"name": "Trigger", "kind": "cylinder", "local_to_world": m([700.0, 0.0, 40.0]),
                                "cylinder": [50.0, 40.0]}]
            }),
        ),
        actor(
            11,
            "SkeletalMeshActor_0",
            "Engine.SkeletalMeshActor",
            "other",
            [0.0, 900.0, 0.0],
            json!({
                "components": [{"name": "SkeletalMeshComponent_4", "kind": "skeletal_mesh",
                                "local_to_world": m([0.0, 900.0, -52.0]),
                                "skeletal_mesh": "Villagers.Meshes.Stray_Adult_01",
                                "animation": {"sequence": "StrayVillager_Talk_02", "looping": true,
                                              "playing": true, "start_time": 4.0}}]
            }),
        ),
        actor(
            12,
            "SkeletalMeshActorMAT_0",
            "Engine.SkeletalMeshActorMAT",
            "other",
            [0.0; 3],
            json!({
                "components": [{"name": "SkeletalMeshComponent_9", "kind": "skeletal_mesh",
                                "skeletal_mesh": "Maddie.Maddie"}],
                "matinee": [{"action": "SeqAct_Interp_3", "group": "Maddie"}]
            }),
        ),
        actor(
            13,
            "PlayerStart_0",
            "Engine.PlayerStart",
            "player_start",
            [5.0, 6.0, 7.0],
            json!({
                "components": [{"name": "CylinderComponent_0", "kind": "cylinder", "cylinder": [40.0, 80.0]}]
            }),
        ),
        actor(
            14,
            "ASAMUNPC_VillagerPawn_0",
            "asamu.ASAMUNPC_VillagerPawn",
            "asamu_other",
            [0.0; 3],
            json!({
                "params": {"bUseScriptedPath": true, "scriptedPath": [format!("{p}PlayerStart_0")]}
            }),
        ),
    ];
    let mut src = MemorySource::new();
    src.insert(
        "levels/AG-NpcTest.scene.json",
        scene_json(
            "AG-NpcTest",
            actors,
            vec![json!({"class": "LevelStreamingKismet", "package_name": "NpcSub", "offset": [0.0, 0.0, 1000.0]})],
        ),
    );
    src.insert(
        "levels/NpcSub.scene.json",
        scene_json(
            "NpcSub",
            vec![actor(
                3,
                "ASAMUCollectible_7",
                "asamu.ASAMUCollectible",
                "collectible",
                [0.0, 0.0, 0.0],
                json!({}),
            )],
            Vec::new(),
        ),
    );
    src
}

#[test]
fn synthetic_scene_loads_every_npc_kind() {
    let src = npc_test_map();
    let s = load_npc_scene_for_map(&src, "ag-npctest", &LoadOptions::default()).unwrap();
    assert!(s.warnings.is_empty(), "{:?}", s.warnings);
    assert!(s.validate().is_empty(), "{:?}", s.validate());

    let w = &s.worms[0];
    assert_eq!(w.id, 3);
    assert_eq!(w.location, Vec3::new(1000.0, 0.0, 200.0));
    assert_eq!(
        w.look_targets,
        vec![Vec3::new(10.0, 20.0, 30.0), Vec3::new(40.0, 50.0, 60.0)]
    );
    assert_eq!(w.light.radius, 100_000.0);
    assert_eq!(w.light.brightness, 25.0);
    assert_eq!(w.light.color, [255, 247, 247, 255]);
    assert_eq!((w.light.outer_cone, w.light.inner_cone), (40.0, 15.0));
    assert_eq!(
        w.meshes[0].mesh,
        "Dark_Cave_worm.SkeletalMesh.Worm_SkeletalMesh"
    );

    let v = &s.worm_volumes[0];
    assert_eq!(v.role, WormVolumeRole::Scream);
    assert!(v.encompasses(Vec3::new(0.0, 0.0, 0.0)));
    assert!(v.encompasses(Vec3::new(499.0, -499.0, 399.0)));
    assert!(!v.encompasses(Vec3::new(501.0, 0.0, 0.0)));

    assert_eq!(s.collectibles.len(), 3);
    let c0 = &s.collectibles[0];
    assert_eq!(c0.trigger.center, Vec3::new(100.0, 0.0, 40.0));
    // Without a component: the class template (50 × 40, +40 above).
    let c1 = &s.collectibles[1];
    assert_eq!(c1.trigger.radius, COLLECTIBLE_TRIGGER_RADIUS);
    assert_eq!(c1.trigger.center, Vec3::new(300.0, 0.0, 40.0));
    // The streamed level: index 1, offset applied.
    let sub = &s.collectibles[2];
    assert_eq!(sub.id, 1 << 16 | 3);
    assert_eq!(sub.level, "NpcSub");
    assert_eq!(sub.location, Vec3::new(0.0, 0.0, 1000.0));

    let parent = &s.story_items[0];
    assert!(parent.parent && parent.optional);
    assert_eq!(parent.linked_children, vec![8]);
    assert!(parent.has_glow);
    let child = &s.story_items[1];
    assert_eq!(child.linked_parent, Some(7));
    assert_eq!(child.max_interact_times, 0);
    assert!(!child.has_glow, "GlowMesh overridden with null");

    let f = &s.flowers[0];
    assert_eq!(f.glow_duration, 5.0);
    assert_eq!(f.fade_time, 1.0, "class default");
    assert_eq!(f.lights, vec![1]);

    let fol = &s.foliage[0];
    assert_eq!(fol.trigger.center, Vec3::new(700.0, 0.0, 40.0));
    assert_eq!(fol.sound.as_deref(), Some("MiscSounds.Foliage_Rustle_Cue"));

    assert_eq!(s.skinned.len(), 2);
    let amb = &s.skinned[0];
    assert_eq!(amb.drive, SkinnedDrive::Ambient);
    let hint = amb.components[0].animation.as_ref().unwrap();
    assert_eq!(hint.sequence, "StrayVillager_Talk_02");
    assert!(hint.looping && hint.playing);
    assert_eq!(hint.start_time, 4.0);
    assert_eq!(hint.rate, 1.0);
    assert_eq!(s.skinned[1].drive, SkinnedDrive::Matinee);

    assert_eq!(s.nav_points[0].location, Vec3::new(5.0, 6.0, 7.0));
    assert_eq!(s.nav_points[0].radius, 40.0);
    let vil = &s.villagers[0];
    assert!(vil.use_scripted_path);
    assert_eq!(vil.scripted_path.len(), 1);
    assert_eq!(vil.scripted_path[0].radius, 40.0);

    assert_eq!(
        s.actor_by_name("AG-NpcTest.TheWorld.PersistentLevel.ASAMUNPC_WormPawn_0"),
        Some(3)
    );
    assert_eq!(s.actor_by_name("asamunpc_wormpawn_0"), Some(3));
}

#[test]
fn loading_by_levels_matches_loading_by_map() {
    let src = npc_test_map();
    let levels = vec![
        SubLevel {
            name: "AG-NpcTest".into(),
            streaming_class: None,
            initially_loaded: true,
            offset: Vec3::ZERO,
            actors: 0,
        },
        SubLevel {
            name: "NpcSub".into(),
            streaming_class: Some("LevelStreamingKismet".into()),
            initially_loaded: false,
            offset: Vec3::new(0.0, 0.0, 1000.0),
            actors: 0,
        },
    ];
    let a = load_npc_scene(&src, &levels, &LoadOptions::default());
    let b = load_npc_scene_for_map(&src, "AG-NpcTest", &LoadOptions::default()).unwrap();
    assert_eq!(a, b);
    // A missing level is a warning.
    let mut more = levels.clone();
    more.push(SubLevel {
        name: "Nowhere".into(),
        ..levels[1].clone()
    });
    let c = load_npc_scene(&src, &more, &LoadOptions::default());
    assert_eq!(c.warnings.len(), 1);
}

#[test]
fn hostile_scenes_are_errors_or_warnings_never_panics() {
    let opts = LoadOptions::default();
    let mut src = MemorySource::new();
    assert!(load_npc_scene_for_map(&src, "x", &opts).is_err(), "missing");
    src.insert("levels/x.scene.json", b"{not json".to_vec());
    assert!(
        load_npc_scene_for_map(&src, "x", &opts).is_err(),
        "malformed"
    );
    src.insert(
        "levels/x.scene.json",
        serde_json::to_vec(&json!({"format": "other", "version": 1, "package": "x"})).unwrap(),
    );
    assert!(load_npc_scene_for_map(&src, "x", &opts).is_err(), "format");
    // Odd values: non-finite location (serialized as null → rejected by the
    // array type → JSON error), a slot too large, degenerate hulls, NaN
    // cylinder sizes, references to nothing.
    let actors = vec![
        actor(
            70_000,
            "ASAMUCollectible_9",
            "asamu.ASAMUCollectible",
            "collectible",
            [0.0; 3],
            json!({}),
        ),
        actor(
            1,
            "ASAMUWormScreamVolume_0",
            "asamu.ASAMUWormScreamVolume",
            "trigger_volume",
            [0.0; 3],
            json!({
                "volume": {"hulls": [{"vertices": [[0.0, 0.0, 0.0]], "triangles": [[0, 9, 99]], "planes": []}]}
            }),
        ),
        actor(
            2,
            "ASAMUCollectible_2",
            "asamu.ASAMUCollectible",
            "collectible",
            [1e30, 0.0, 0.0],
            json!({
                "components": [{"name": "Trigger", "kind": "cylinder", "cylinder": [-5.0, 1.0]}]
            }),
        ),
        actor(
            3,
            "ASAMUInteractable_Actor_0",
            "asamu.ASAMUInteractable_Actor",
            "asamu_other",
            [0.0; 3],
            json!({
                "params": {"MaxInteractTimes": 99_999_999_999i64, "linkedParentActor": 5, "linkedInteractables": "nope"}
            }),
        ),
        actor(
            4,
            "ASAMUNPC_WormPawn_0",
            "asamu.ASAMUNPC_WormPawn",
            "asamu_other",
            [0.0; 3],
            json!({
                "params": {"RandomLookAtTargets": [1, null, {}], "Radius": "big", "LightColor": {"R": 999}}
            }),
        ),
    ];
    src.insert(
        "levels/x.scene.json",
        scene_json("x", actors, vec![json!({"class": "LevelStreamingKismet"})]),
    );
    let s = load_npc_scene_for_map(&src, "x", &opts).unwrap();
    assert!(
        s.warnings.iter().any(|w| w.contains("slot")),
        "{:?}",
        s.warnings
    );
    assert!(
        s.warnings.iter().any(|w| w.contains("hulls")),
        "{:?}",
        s.warnings
    );
    assert_eq!(s.collectibles.len(), 1);
    assert_eq!(
        s.collectibles[0].trigger.radius, COLLECTIBLE_TRIGGER_RADIUS,
        "bad cylinder ignored"
    );
    assert_eq!(
        s.story_items[0].max_interact_times, 1,
        "out-of-range falls back to the default"
    );
    assert_eq!(s.story_items[0].linked_parent, None);
    assert!(s.worms[0].look_targets.is_empty());
    assert_eq!(
        s.worms[0].light.color[0], 255,
        "out-of-range channel keeps the default"
    );
    // Size limits.
    let small = LoadOptions {
        max_file_bytes: 10,
        ..LoadOptions::default()
    };
    assert!(load_npc_scene_for_map(&src, "x", &small).is_err());
    let few = LoadOptions {
        max_actors: 2,
        ..LoadOptions::default()
    };
    assert!(load_npc_scene_for_map(&src, "x", &few).is_err());
}

#[test]
fn every_truncation_of_a_scene_is_handled() {
    let src = npc_test_map();
    let full = src.read("levels/AG-NpcTest.scene.json", u64::MAX).unwrap();
    for cut in (0..full.len()).step_by(37) {
        let mut s = MemorySource::new();
        s.insert("levels/t.scene.json", full[..cut].to_vec());
        let _ = load_npc_scene_for_map(&s, "t", &LoadOptions::default());
    }
}

#[test]
fn hull_from_json_orients_planes_outward() {
    let h: asamu_world::scene::HullJson =
        serde_json::from_value(box_hull([0.0, 0.0, 0.0], [10.0, 10.0, 10.0])).unwrap();
    let hull = NpcHull::from_json(&h, Vec3::new(100.0, 0.0, 0.0)).unwrap();
    assert!(hull.contains(Vec3::new(105.0, 5.0, 5.0)));
    assert!(!hull.contains(Vec3::new(5.0, 5.0, 5.0)));
    // Flipped planes are re-oriented.
    let mut flipped = h.clone();
    for p in &mut flipped.planes {
        *p = [-p[0], -p[1], -p[2], -p[3]];
    }
    let hull = NpcHull::from_json(&flipped, Vec3::ZERO).unwrap();
    assert!(hull.contains(Vec3::splat(5.0)));
    // No planes: rebuilt from triangles.
    let tris: asamu_world::scene::HullJson = serde_json::from_value(json!({
        "vertices": [[0,0,0],[10,0,0],[0,10,0],[0,0,10]],
        "triangles": [[0,2,1],[0,1,3],[0,3,2],[1,2,3]], "planes": []
    }))
    .unwrap();
    let hull = NpcHull::from_json(&tris, Vec3::ZERO).unwrap();
    assert!(hull.contains(Vec3::splat(1.0)));
    assert!(!hull.contains(Vec3::splat(6.0)));
}

fn skeletal_manifest() -> MemorySource {
    let mut src = MemorySource::new();
    src.insert(
        "skeletal/manifest.json",
        serde_json::to_vec(&json!({
            "version": 1,
            "meshes": {
                "Villagers.Meshes.Stray_Adult_01": {
                    "package": "Startup", "scale": 1.0,
                    "lods": [{"lod": 0, "gltf": "Startup/Villagers/Meshes/Stray_Adult_01.gltf", "bin": "x.bin"}],
                    "anim_sets": [{"path": "Villagers.AnimSets.AnimSet_Villager_Adult_01", "sequences": [
                        {"animation": 0, "name": "Villagers.AnimSets.AnimSet_Villager_Adult_01/StrayVillager_Talk_02",
                         "sequence_name": "StrayVillager_Talk_02", "length": 8.0, "rate_scale": 1.0},
                        {"animation": 1, "name": "Villagers.AnimSets.AnimSet_Villager_Adult_01/StrayVillager_Idle_01",
                         "sequence_name": "StrayVillager_Idle_01", "length": 6.0}
                    ]}],
                    "sockets": [{"name": "Hat", "bone": "Head"}]
                },
                "AG-StarHaven.Chars.Captain": {
                    "package": "AG-StarHaven",
                    "lods": [{"lod": 0, "gltf": "AG-StarHaven/Chars/Captain.gltf"}]
                },
                "Evil.Mesh": {"lods": [{"lod": 0, "gltf": "../../etc/passwd"}]},
                "Evil.Absolute": {"lods": [{"lod": 0, "gltf": "/etc/passwd"}]},
                "Evil.Drive": {"lods": [{"lod": 0, "gltf": "C:/Windows/x.gltf"}]},
                "Evil.Backslash": {"lods": [{"lod": 0, "gltf": "a\\..\\..\\x.gltf"}]},
                "Evil.Label": {"lods": [{"lod": 0, "gltf": "a/x.gltf#Scene0"}]},
                "Evil.Dot": {"lods": [{"lod": 0, "gltf": "a/./x.gltf"}]},
                "Evil.Empty": {"lods": [{"lod": 0, "gltf": ""}]},
                "Evil.Control": {"lods": [{"lod": 0, "gltf": "a/x\u{1}.gltf"}]},
                "Evil.NoLods": {"lods": []}
            }
        }))
        .expect("manifest JSON"),
    );
    src
}

#[test]
fn skeletal_index_resolves_meshes_and_animations() {
    let idx = SkeletalIndex::load(&skeletal_manifest(), &LoadOptions::default()).unwrap();
    assert_eq!(
        idx.meshes.keys().cloned().collect::<Vec<_>>(),
        vec![
            "ag-starhaven.chars.captain".to_owned(),
            "villagers.meshes.stray_adult_01".to_owned()
        ],
        "unsafe or missing paths refused"
    );
    let m = idx.get("villagers.meshes.stray_adult_01").unwrap();
    assert_eq!(
        m.gltf,
        "skeletal/Startup/Villagers/Meshes/Stray_Adult_01.gltf"
    );
    assert_eq!(m.animation("strayvillager_talk_02").unwrap().index, 0);
    assert_eq!(
        m.idle_animation().unwrap().sequence,
        "StrayVillager_Idle_01"
    );
    assert_eq!(m.animations[1].rate_scale, 1.0, "default rate scale");
    assert_eq!(m.sockets, vec![("Hat".to_owned(), "Head".to_owned())]);
    // Package prefix either way.
    assert!(idx.get("Chars.Captain").is_some());
    assert!(idx.get("Startup.Villagers.Meshes.Stray_Adult_01").is_some());
    assert!(idx.get("Nope.Nope").is_none());
    assert!(SkeletalIndex::load(&MemorySource::new(), &LoadOptions::default()).is_err());
}

/// Real data: the converted AG-Darkcave (set `ASAMU_CONVERTED_DIR` to a
/// directory written by `asamu-import levels`); skips otherwise.
#[test]
fn real_darkcave_npc_census() {
    let Some(dir) = std::env::var_os("ASAMU_CONVERTED_DIR") else {
        eprintln!("skipped: ASAMU_CONVERTED_DIR not set");
        return;
    };
    let src = DirSource::new(std::path::PathBuf::from(dir));
    let Ok(s) = load_npc_scene_for_map(&src, "AG-Darkcave", &LoadOptions::default()) else {
        eprintln!("skipped: AG-Darkcave not converted");
        return;
    };
    eprintln!(
        "AG-Darkcave: worms {}, worm volumes {}, collectibles {}, story items {}, flowers {}, foliage {}, skinned {}, warnings {}",
        s.worms.len(),
        s.worm_volumes.len(),
        s.collectibles.len(),
        s.story_items.len(),
        s.flowers.len(),
        s.foliage.len(),
        s.skinned.len(),
        s.warnings.len()
    );
    assert_eq!(s.worms.len(), 1);
    assert_eq!(s.worm_volumes.len(), 1);
    assert_eq!(s.worm_volumes[0].role, WormVolumeRole::Scream);
    assert!(!s.worm_volumes[0].hulls.is_empty());
    assert_eq!(s.collectibles.len(), 5);
    assert_eq!(s.story_items.len(), 3);
    assert_eq!(s.flowers.len(), 15);
    assert_eq!(s.foliage.len(), 406);
    assert!(s.maddies.is_empty() && s.villagers.is_empty());
    assert!(s.validate().is_empty(), "{:?}", s.validate());
    let w = &s.worms[0];
    assert_eq!(w.light.radius, 100_000.0);
    assert_eq!(w.light.brightness, 25.0);
    // `RandomLookAtTargets`: six point lights of the level (stored only).
    assert_eq!(w.look_targets.len(), 6);
}

/// Real data: the story maps' NPC census against the map census of NPCS.md §1
/// and SAVE.md 6.3 (5 collectibles in each collectible map, 25 in total; 11
/// distinct optional story keys, found by interacting with every story item
/// through the runtime). Set `ASAMU_CONVERTED_DIR` to `asamu-import levels`
/// output (with the streamed sub-levels); maps not converted are skipped and
/// the totals are only checked when all six are present. With
/// `skeletal/manifest.json` present, every skinned component must resolve.
#[test]
fn real_story_map_census() {
    use asamu_world::npc::{NpcEvent, NpcOutput, NpcRuntime};
    use std::collections::BTreeSet;
    let Some(dir) = std::env::var_os("ASAMU_CONVERTED_DIR") else {
        eprintln!("skipped: ASAMU_CONVERTED_DIR not set");
        return;
    };
    let src = DirSource::new(std::path::PathBuf::from(dir));
    let skeletal = SkeletalIndex::load(&src, &LoadOptions::default()).ok();
    // (map, collectibles, story items, foliage, glow flowers, worms)
    let expected = [
        ("AG-Workshop", 0, 6, 0, 0, 0),
        ("AG-ParadiseCave", 5, 12, 1018, 0, 0),
        ("AG-BeautifulCity", 5, 4, 0, 0, 0),
        ("AG-Darkcave", 5, 3, 406, 15, 1),
        ("AG-StarHaven", 5, 17, 410, 0, 0),
        ("AG-IceCave", 5, 22, 0, 0, 0),
    ];
    let mut converted = 0;
    let mut collectibles = 0;
    let mut keys: BTreeSet<(String, Option<String>)> = BTreeSet::new();
    for (map, coll, items, foliage, flowers, worms) in expected {
        let Ok(s) = load_npc_scene_for_map(&src, map, &LoadOptions::default()) else {
            eprintln!("skipped: {map} not converted");
            continue;
        };
        converted += 1;
        collectibles += s.collectibles.len();
        let got = (
            s.collectibles.len(),
            s.story_items.len(),
            s.foliage.len(),
            s.flowers.len(),
            s.worms.len(),
        );
        eprintln!(
            "{map}: {got:?}, skinned actors {}, warnings {:?}",
            s.skinned.len(),
            s.warnings
        );
        assert_eq!(got, (coll, items, foliage, flowers, worms), "{map}");
        assert!(s.maddies.is_empty() && s.villagers.is_empty(), "{map}");
        assert!(s.validate().is_empty(), "{map}: {:?}", s.validate());
        assert!(
            s.collectibles
                .iter()
                .all(|c| c.trigger.radius == COLLECTIBLE_TRIGGER_RADIUS)
        );
        let mut rt = NpcRuntime::new(&s, 1, false);
        for item in &s.story_items {
            let mut out = NpcOutput::default();
            rt.interact_with(&s, item.id, &mut out);
            for e in out.events {
                if let NpcEvent::StoryItemRegistered { item } = e {
                    let name = item
                        .and_then(|id| s.story_items.iter().find(|d| d.id == id))
                        .map(|d| d.name.clone());
                    keys.insert((map.to_owned(), name));
                }
            }
        }
        if let Some(index) = &skeletal {
            for a in &s.skinned {
                for c in &a.components {
                    assert!(
                        index.get(&c.mesh).is_some(),
                        "{map}: {} not converted",
                        c.mesh
                    );
                }
            }
        }
    }
    eprintln!("optional story keys: {keys:?}");
    if converted == expected.len() {
        assert_eq!(collectibles, 25);
        assert_eq!(keys.len(), 11, "TOTAL_INTERACTABLES_COUNT");
        assert_eq!(
            keys.iter().filter(|k| k.1.is_none()).count(),
            5,
            "stand-alone optional items collapse to one `<level>None` key per level"
        );
    }
}
