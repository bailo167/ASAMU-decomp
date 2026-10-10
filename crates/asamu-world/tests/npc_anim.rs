//! Skinned actors' animation as the simulation runs it (NPCS.md "Animation
//! notifies", "Look-at", "Pawn collision"): the importer's extras
//! (`matinee/<map>.actors.json`, `matinee/anim_notifies.json`) and the
//! skeletal manifest's notifies loaded into the NPC scene (synthetic files
//! written here), ambient nodes ticking and firing Kismet and sound
//! notifies, Matinee's `SetAnimPosition`, skeletal-control strengths,
//! look-at offsets and the pawns' collision cylinders.

#![allow(clippy::unwrap_used)]

use asamu_world::anim::NotifyKind;
use asamu_world::npc::{
    LookAtDriver, NPC_COLLISION_HALF_HEIGHT, NpcEvent, NpcOutput, NpcRuntime,
    WORM_LOOK_AT_CONTROLS, load_npc_scene,
};
use asamu_world::scene::{LoadOptions, MemorySource, SubLevel};
use glam::Vec3;
use serde_json::{Value, json};

const MAP: &str = "AG-AnimTest";

fn m(t: [f32; 3]) -> Value {
    json!([
        [1.0, 0.0, 0.0, 0.0],
        [0.0, 1.0, 0.0, 0.0],
        [0.0, 0.0, 1.0, 0.0],
        [t[0], t[1], t[2], 1.0]
    ])
}

fn skel(name: &str, mesh: &str, loc: [f32; 3]) -> Value {
    json!({"name": name, "kind": "skeletal_mesh", "skeletal_mesh": mesh, "local_to_world": m(loc)})
}

fn source() -> MemorySource {
    let mut src = MemorySource::new();
    let actors = vec![
        json!({"slot": 1, "name": "SkeletalMeshActor_61", "class": "Engine.SkeletalMeshActor",
               "kind": "other", "location": [100.0, 0.0, 0.0], "rotation": [0, 0, 0],
               "components": [skel("SkeletalMeshComponent_4", "Villagers.Meshes.Villager", [100.0, 0.0, 0.0])],
               "params": {}, "matinee": []}),
        json!({"slot": 2, "name": "SkeletalMeshActorMATWithFollowCollision_1",
               "class": "asamu.SkeletalMeshActorMATWithFollowCollision",
               "kind": "other", "location": [300.0, 0.0, 0.0], "rotation": [0, 0, 0],
               "components": [skel("SkeletalMeshComponent_0", "Maddie.Maddie", [300.0, 0.0, 0.0])],
               "params": {"headLookAtControlNames": ["LookAtController"],
                          "eyesLookAtControlNames": ["EyesLookAt"]},
               "matinee": [{"action": "x"}]}),
        json!({"slot": 3, "name": "ASAMUNPC_WormPawn_0", "class": "asamu.ASAMUNPC_WormPawn",
               "kind": "other", "location": [0.0, 5000.0, 500.0], "rotation": [0, 0, 0],
               "components": [
                   {"name": "CylinderComponent_0", "kind": "cylinder", "cylinder": [200.0, 500.0],
                    "local_to_world": m([0.0, 5000.0, 500.0])},
                   skel("SkeletalMeshComponent_0", "Dark_Cave_worm.SkeletalMesh.Worm", [0.0, 5000.0, 500.0])
               ],
               "params": {}, "matinee": []}),
        json!({"slot": 4, "name": "ASAMUNPC_MaddiePawn_0", "class": "asamu.ASAMUNPC_MaddiePawn",
               "kind": "other", "location": [0.0, -900.0, 78.0], "rotation": [0, 0, 0],
               "components": [], "params": {}, "matinee": []}),
    ];
    src.insert(
        format!("levels/{MAP}.scene.json"),
        serde_json::to_vec(&json!({
            "format": "asamu-scene", "version": 1, "package": MAP,
            "world_info": {"kill_z": -10000.0}, "streaming_levels": [], "actors": actors
        }))
        .unwrap(),
    );
    let p = format!("{MAP}.TheWorld.PersistentLevel.");
    src.insert(
        format!("matinee/{MAP}.actors.json"),
        serde_json::to_vec(&json!({
            "format": "asamu-matinee-actors", "version": 1, "package": MAP,
            "anim_nodes": [
                {"actor": format!("{p}SkeletalMeshActor_61"), "component": "SkeletalMeshComponent_4",
                 "sequence": "StrayVillager_Talk_02", "looping": true, "playing": true,
                 "start_time": 1.0, "rate": 1.0},
                {"actor": format!("{p}Missing_0"), "component": "C", "sequence": "X",
                 "looping": true, "playing": true, "start_time": 0.0, "rate": 1.0}
            ],
            "look_at_controls": [
                {"actor": format!("{p}SkeletalMeshActorMATWithFollowCollision_1"),
                 "component": "SkeletalMeshComponent_0", "control": "LookAtController", "bone": "head",
                 "look_at_axis": "AXIS_X", "up_axis": "AXIS_Z", "invert_look_at_axis": false,
                 "invert_up_axis": false, "enable_limit": true, "limit_based_on_ref_pose": false,
                 "max_angle": 45.0, "outer_max_angle": 90.0, "dead_zone_angle": 0.0,
                 "allow_rotation": [false, false, true], "allow_rotation_space": "BCS_BoneSpace",
                 "target_interp_speed": 10.0, "control_strength": 0.25, "blend_in_time": 0.33,
                 "blend_out_time": 0.33},
                {"actor": format!("{p}ASAMUNPC_WormPawn_0"), "component": "SkeletalMeshComponent_0",
                 "control": "HeadLookat", "bone": "head", "control_strength": 1.0}
            ]
        }))
        .unwrap(),
    );
    src.insert(
        "skeletal/manifest.json",
        serde_json::to_vec(&json!({
            "version": 1,
            "meshes": {
                "Villagers.Meshes.Villager": {
                    "package": "Startup", "scale": 1.0,
                    "lods": [{"lod": 0, "gltf": "Startup/V.gltf"}],
                    "anim_sets": [{"path": "Villagers.AnimSets.Set", "sequences": [
                        {"animation": 0, "name": "Set/StrayVillager_Talk_02",
                         "sequence_name": "StrayVillager_Talk_02", "length": 10.0, "rate_scale": 1.0,
                         "notifies": [
                             [0.0, "Villagers.AnimSets.Set.Seq_0.AnimNotify_Kismet_16", "c", 0.0],
                             [6.4, "Villagers.AnimSets.Set.Seq_0.AnimNotify_Kismet_20", "c", 0.0],
                             [6.4, "Villagers.AnimSets.Set.Seq_0.AnimNotify_Sound_0", "", 0.0],
                             [8.0, "Villagers.AnimSets.Set.Seq_0.AnimNotify_Footstep_0", "", 0.0]
                         ]}
                    ]}]
                },
                "Maddie.Maddie": {
                    "package": "Startup", "scale": 1.0,
                    "lods": [{"lod": 0, "gltf": "Startup/M.gltf"}],
                    "anim_sets": [{"path": "Maddie.Set", "sequences": [
                        {"animation": 0, "name": "Maddie.Set/maddie_meetingstrays",
                         "sequence_name": "maddie_meetingstrays", "length": 20.0, "rate_scale": 1.0,
                         "notifies": [[6.43, "Maddie.Set.Seq_1.AnimNotify_Kismet_0", "", 0.0],
                                      [10.0, "Maddie.Set.Seq_1.AnimNotify_Sound_0", "Maddie_1", 0.0]]}
                    ]}]
                }
            }
        }))
        .unwrap(),
    );
    src.insert(
        "matinee/anim_notifies.json",
        serde_json::to_vec(&json!({
            "format": "asamu-anim-notifies", "version": 1,
            "notifies": [
                {"path": "Villagers.AnimSets.Set.Seq_0.AnimNotify_Kismet_16", "class": "Engine.AnimNotify_Kismet",
                 "notify_name": "Talk_Standing_1", "follow_actor": false, "ignore_if_actor_hidden": false,
                 "volume": 1.0, "pitch": 1.0, "percent_to_play": 1.0},
                {"path": "Villagers.AnimSets.Set.Seq_0.AnimNotify_Kismet_20", "class": "Engine.AnimNotify_Kismet",
                 "notify_name": "Talk_Standing_1_2", "follow_actor": false, "ignore_if_actor_hidden": false,
                 "volume": 1.0, "pitch": 1.0, "percent_to_play": 1.0},
                {"path": "Villagers.AnimSets.Set.Seq_0.AnimNotify_Sound_0", "class": "Engine.AnimNotify_Sound",
                 "sound_cue": "Sounds.Talk_Cue", "follow_actor": true, "ignore_if_actor_hidden": false,
                 "volume": 0.5, "pitch": 1.0, "percent_to_play": 1.0},
                {"path": "Maddie.Set.Seq_1.AnimNotify_Kismet_0", "class": "Engine.AnimNotify_Kismet",
                 "notify_name": "Maddie_Line", "follow_actor": false, "ignore_if_actor_hidden": false,
                 "volume": 1.0, "pitch": 1.0, "percent_to_play": 1.0},
                {"path": "Maddie.Set.Seq_1.AnimNotify_Sound_0", "class": "Engine.AnimNotify_Sound",
                 "sound_cue": "Maddie.Voice_1", "follow_actor": true, "ignore_if_actor_hidden": false,
                 "volume": 1.0, "pitch": 1.0, "percent_to_play": 1.0}
            ]
        }))
        .unwrap(),
    );
    src
}

fn levels() -> Vec<SubLevel> {
    vec![SubLevel {
        name: MAP.into(),
        streaming_class: None,
        initially_loaded: true,
        offset: Vec3::ZERO,
        actors: 4,
    }]
}

#[test]
fn extras_load_into_the_npc_scene() {
    let scene = load_npc_scene(&source(), &levels(), &LoadOptions::default());
    assert!(scene.warnings.is_empty(), "{:?}", scene.warnings);
    // The villager's own node came from the actors file.
    let villager = scene
        .skinned
        .iter()
        .find(|s| s.name == "SkeletalMeshActor_61")
        .unwrap();
    let hint = villager.components[0].animation.as_ref().unwrap();
    assert_eq!(hint.sequence, "StrayVillager_Talk_02");
    assert_eq!(hint.start_time, 1.0);
    assert!(hint.looping && hint.playing);
    // Notifies resolved through anim_notifies.json; unknown classes are
    // kept as "other".
    let info = scene
        .sequence_info("Villagers.Meshes.Villager", "straYvillager_talk_02")
        .unwrap();
    assert_eq!(info.notifies.len(), 4);
    assert_eq!(
        info.notifies[0].kind,
        NotifyKind::Kismet {
            name: "Talk_Standing_1".into()
        }
    );
    assert!(matches!(info.notifies[2].kind, NotifyKind::Sound { volume, .. } if volume == 0.5));
    assert!(matches!(info.notifies[3].kind, NotifyKind::Other { .. }));
    // Look-at setups: the actor's control names and its tree's control; the
    // worm's four controls by name.
    let maddie = scene.skinned_index(2).map(|_| 2).unwrap();
    let look = scene.look_at_of(maddie).unwrap();
    assert_eq!(look.driver, LookAtDriver::Player);
    assert_eq!(look.head, vec!["LookAtController".to_owned()]);
    assert_eq!(look.eyes, vec!["EyesLookAt".to_owned()]);
    assert_eq!(look.controls.len(), 1);
    assert_eq!(look.controls[0].bone.as_deref(), Some("head"));
    let worm = scene.look_at_of(3).unwrap();
    assert_eq!(worm.driver, LookAtDriver::WormAim);
    assert_eq!(worm.head.len(), WORM_LOOK_AT_CONTROLS.len());
    assert_eq!(worm.controls[0].control, "HeadLookat");
    // Pawn collision: the worm's placed cylinder, Maddie's template one.
    let worm_c = scene.pawn_collision.iter().find(|c| c.id == 3).unwrap();
    assert_eq!((worm_c.radius, worm_c.half_height), (200.0, 500.0));
    let maddie_c = scene.pawn_collision.iter().find(|c| c.id == 4).unwrap();
    assert_eq!(maddie_c.half_height, NPC_COLLISION_HALF_HEIGHT);
}

#[test]
fn missing_or_bad_extras_are_tolerated() {
    let mut src = source();
    src.insert(format!("matinee/{MAP}.actors.json"), b"{ not json".to_vec());
    src.insert("matinee/anim_notifies.json", br#"{"notifies": 5}"#.to_vec());
    let scene = load_npc_scene(&src, &levels(), &LoadOptions::default());
    assert_eq!(scene.warnings.len(), 2, "{:?}", scene.warnings);
    // Without names the notifies are "other"; nothing fires to Kismet.
    let info = scene
        .sequence_info("Villagers.Meshes.Villager", "StrayVillager_Talk_02")
        .unwrap();
    assert!(
        info.notifies
            .iter()
            .all(|n| matches!(n.kind, NotifyKind::Other { .. }))
    );
    // A Kismet notify without a name (unset, empty or "None") does nothing.
    let mut src = source();
    src.insert(
        "matinee/anim_notifies.json",
        serde_json::to_vec(&json!({
            "format": "asamu-anim-notifies", "version": 1,
            "notifies": [
                {"path": "Villagers.AnimSets.Set.Seq_0.AnimNotify_Kismet_16",
                 "class": "Engine.AnimNotify_Kismet", "notify_name": "None"},
                {"path": "Villagers.AnimSets.Set.Seq_0.AnimNotify_Kismet_20",
                 "class": "Engine.AnimNotify_Kismet", "notify_name": ""},
                {"path": "Villagers.AnimSets.Set.Seq_0.AnimNotify_Sound_0",
                 "class": "Engine.AnimNotify_Kismet"}
            ]
        }))
        .unwrap(),
    );
    let scene = load_npc_scene(&src, &levels(), &LoadOptions::default());
    let info = scene
        .sequence_info("Villagers.Meshes.Villager", "StrayVillager_Talk_02")
        .unwrap();
    assert!(
        info.notifies
            .iter()
            .all(|n| matches!(n.kind, NotifyKind::Other { .. })),
        "{:?}",
        info.notifies
    );
    // A different format string is refused.
    let mut src = source();
    src.insert(
        format!("matinee/{MAP}.actors.json"),
        br#"{"format": "something-else", "anim_nodes": []}"#.to_vec(),
    );
    let scene = load_npc_scene(&src, &levels(), &LoadOptions::default());
    assert_eq!(scene.warnings.len(), 1);
    assert!(
        scene.skinned[0].components[0].animation.is_none(),
        "no hint from a refused file"
    );
}

#[test]
fn ambient_animation_fires_kismet_and_sound_notifies_every_loop() {
    let scene = load_npc_scene(&source(), &levels(), &LoadOptions::default());
    let mut rt = NpcRuntime::new(&scene, 7, false);
    let mut out = NpcOutput::default();
    let mut times: Vec<(f32, String)> = Vec::new();
    for t in 0..(25 * 60) {
        rt.tick_actors(&scene, Vec3::new(0.0, 0.0, -10_000.0), 1.0 / 60.0, &mut out);
        for e in out.events.drain(..) {
            let label = match e {
                NpcEvent::AnimNotify { actor: 1, name } => name,
                NpcEvent::AnimSound {
                    actor: 1,
                    cue,
                    volume,
                    ..
                } => {
                    assert_eq!(volume, 0.5);
                    cue
                }
                _ => continue,
            };
            times.push(((t + 1) as f32 / 60.0, label));
        }
    }
    // Start 1.0: 6.4 → 5.4 s (Kismet + sound), 10/0 → 9.0 s, 16.4 → 15.4 s,
    // 20 → 19.0 s, 26.4 → 25.4 (past the run).
    let labels: Vec<&str> = times.iter().map(|(_, l)| l.as_str()).collect();
    assert_eq!(
        labels,
        vec![
            "Talk_Standing_1_2",
            "Sounds.Talk_Cue",
            "Talk_Standing_1",
            "Talk_Standing_1_2",
            "Sounds.Talk_Cue",
            "Talk_Standing_1"
        ]
    );
    assert!((times[0].0 - 5.4).abs() < 0.02, "{}", times[0].0);
    assert!((times[2].0 - 9.0).abs() < 0.02, "{}", times[2].0);
    // Deterministic: a second runtime gives the same node state.
    let mut rt2 = NpcRuntime::new(&scene, 7, false);
    let mut out2 = NpcOutput::default();
    for _ in 0..(25 * 60) {
        rt2.tick_actors(
            &scene,
            Vec3::new(0.0, 0.0, -10_000.0),
            1.0 / 60.0,
            &mut out2,
        );
    }
    assert_eq!(rt.skinned, rt2.skinned);
}

#[test]
fn matinee_positions_controls_and_look_at() {
    let scene = load_npc_scene(&source(), &levels(), &LoadOptions::default());
    let mut rt = NpcRuntime::new(&scene, 7, false);
    let mut out = NpcOutput::default();
    // A switch fires nothing; moving forwards past 6.43 returns the Kismet
    // name (for the Matinee update) and past 10 plays the voice line.
    assert_eq!(
        rt.set_anim_position(
            &scene,
            2,
            "maddie_meetingstrays",
            1.0,
            true,
            false,
            &mut out
        ),
        Some(Vec::new())
    );
    assert_eq!(
        rt.set_anim_position(
            &scene,
            2,
            "maddie_meetingstrays",
            7.0,
            true,
            false,
            &mut out
        ),
        Some(vec!["Maddie_Line".to_owned()])
    );
    assert!(out.events.is_empty(), "Kismet notifies are not events here");
    rt.set_anim_position(
        &scene,
        2,
        "maddie_meetingstrays",
        11.0,
        true,
        false,
        &mut out,
    );
    assert_eq!(
        out.events,
        vec![NpcEvent::AnimSound {
            actor: 2,
            cue: "Maddie.Voice_1".into(),
            volume: 1.0,
            pitch: 1.0,
            bone: None
        }]
    );
    // Not firing (jumps, backwards): silent.
    out.events.clear();
    assert_eq!(
        rt.set_anim_position(
            &scene,
            2,
            "maddie_meetingstrays",
            15.0,
            false,
            false,
            &mut out
        ),
        Some(Vec::new())
    );
    assert!(out.events.is_empty());
    let state = &rt.skinned[1];
    assert_eq!(state.current().unwrap().position, 15.0);
    assert_eq!(
        rt.anim_sequence_length(&scene, 2, "maddie_meetingstrays"),
        Some(20.0)
    );
    assert_eq!(rt.anim_sequence_length(&scene, 2, "nothing"), None);
    assert!(
        rt.set_anim_position(&scene, 999, "x", 0.0, true, false, &mut out)
            .is_none()
    );
    // Control strengths: the control's own at first, Matinee's afterwards.
    assert_eq!(rt.control_strength(&scene, 2, "LookAtController"), 0.25);
    assert_eq!(rt.control_strength(&scene, 2, "EyesLookAt"), 1.0);
    assert!(rt.set_skel_control_strength(&scene, 2, "LOOKATCONTROLLER", 0.75));
    assert_eq!(rt.control_strength(&scene, 2, "LookAtController"), 0.75);
    assert!(!rt.set_skel_control_strength(&scene, 999, "X", 1.0));
    // Look-at offsets (the stored target is kept but not used).
    assert!(rt.set_look_at(
        &scene,
        2,
        Some(3),
        Vec3::new(0.0, 0.0, 25.0),
        Vec3::new(0.0, 0.0, f32::NAN)
    ));
    assert_eq!(rt.skinned[1].look_at.head_offset.z, 25.0);
    assert_eq!(rt.skinned[1].look_at.eyes_offset, Vec3::ZERO);
    assert_eq!(rt.skinned[1].look_at.target, Some(3));
}

/// A plain `SkeletalMeshActor` has no slot node: Matinee's
/// `SetAnimPosition` drives the component's own sequence node, which keeps
/// ticking afterwards; without an own node nothing happens.
#[test]
fn matinee_drives_a_plain_skeletal_actors_own_node() {
    let scene = load_npc_scene(&source(), &levels(), &LoadOptions::default());
    let mut rt = NpcRuntime::new(&scene, 7, false);
    let mut out = NpcOutput::default();
    // The villager (actor 1) plays its own loop from 1.0; Matinee moves it to
    // 7.0 with notifies: the Kismet notify at 6.4 is returned, its sound
    // becomes an event.
    let names = rt
        .set_anim_position(
            &scene,
            1,
            "StrayVillager_Talk_02",
            7.0,
            true,
            false,
            &mut out,
        )
        .unwrap();
    assert_eq!(names, vec!["Talk_Standing_1_2".to_owned()]);
    assert!(matches!(
        out.events.as_slice(),
        [NpcEvent::AnimSound { actor: 1, cue, .. }] if cue == "Sounds.Talk_Cue"
    ));
    let state = &rt.skinned[0];
    assert!(state.matinee.is_none(), "no slot node on a plain actor");
    let node = state.current().unwrap();
    assert_eq!(node.position, 7.0);
    assert!(
        node.playing && !node.looping,
        "playing as placed, looping as told"
    );
    // The node goes on ticking from there.
    out.events.clear();
    rt.tick_actors(&scene, Vec3::new(0.0, 0.0, -10_000.0), 0.5, &mut out);
    assert!((rt.skinned[0].current().unwrap().position - 7.5).abs() < 1e-5);
    // The same call on the look-at actor (a `...MAT` class) uses a slot node
    // and leaves no trace on an own node.
    rt.set_anim_position(
        &scene,
        2,
        "maddie_meetingstrays",
        1.0,
        true,
        false,
        &mut out,
    );
    assert!(rt.skinned[1].matinee.is_some() && rt.skinned[1].ambient.is_none());
    // A plain actor without an own sequence node ignores the call.
    let mut src = source();
    src.insert(
        format!("matinee/{MAP}.actors.json"),
        serde_json::to_vec(&json!({
            "format": "asamu-matinee-actors", "version": 1, "package": MAP,
            "anim_nodes": [], "look_at_controls": []
        }))
        .unwrap(),
    );
    let bare = load_npc_scene(&src, &levels(), &LoadOptions::default());
    let mut rt = NpcRuntime::new(&bare, 7, false);
    let mut out = NpcOutput::default();
    assert_eq!(
        rt.set_anim_position(
            &bare,
            1,
            "StrayVillager_Talk_02",
            7.0,
            true,
            false,
            &mut out
        ),
        Some(Vec::new())
    );
    assert!(out.events.is_empty());
    assert!(rt.skinned[0].current().is_none());
}

#[test]
fn pawn_cylinders_stand_where_the_pawns_are() {
    let scene = load_npc_scene(&source(), &levels(), &LoadOptions::default());
    let rt = NpcRuntime::new(&scene, 7, false);
    let cyl = rt.pawn_cylinders(&scene);
    assert_eq!(cyl.len(), 2);
    let worm = cyl.iter().find(|(id, _)| *id == 3).unwrap().1;
    assert_eq!(worm.center, Vec3::new(0.0, 5000.0, 500.0));
    assert_eq!(worm.radius, 200.0);
}
