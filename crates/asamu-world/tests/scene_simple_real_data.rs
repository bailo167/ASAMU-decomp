//! The simple collision of the user's converted levels.
//!
//! Gated: runs only when `ASAMU_CONVERTED_DIR` points at the output of
//! `asamu-import` with a mesh manifest of version 2 (`asamu-import all
//! --only meshes` of the current importer). It converts nothing and never
//! touches the game install. CI has no such directory and skips; so does a
//! conversion from before the simple collision (version 1), with a line
//! saying so.
//!
//! What is compared with what:
//!
//! - the number of pawn-blocking mesh placements per map that are met at a
//!   simple body, at triangles, or not at all, against the counts the lead
//!   measured with a separate script (`docs/PARITY_FINDINGS.md` V6);
//! - the volumes that start switched off, against V14;
//! - three places where the first recordings of the original fix a contact:
//!   the Workshop floor (V18), the Workshop door (V11) and the landing at
//!   the start of AG-BeautifulCity (V20), and the flight through a volume
//!   that starts switched off (V13).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use asamu_assets::manifest::{MESH_MANIFEST_VERSION, MeshManifest};
use asamu_world::collision::{PrimitiveKind, QueryFilter};
use asamu_world::scene::{
    self, ActorJson, DirSource, EffectiveCollision, LoadOptions, LoadedMap, SceneFile, TraceRoute,
    actor_id, component_blocking, effective_collision, mesh_routes, switchable_volume,
};
use glam::Vec3;

/// The player's box.
const E: Vec3 = Vec3::new(21.0, 21.0, 44.0);
/// The box a move of the player sweeps: the original multiplies the X and Y
/// half sizes by 1.001 (`UWorld::MoveActor`, NATIVE_PHYSICS.md 9.2).
const MOVE_E: Vec3 = Vec3::new(21.021, 21.021, 44.0);

/// The converted directory and its mesh manifest, when the gate is open.
fn converted() -> Option<(PathBuf, MeshManifest)> {
    let Some(dir) = std::env::var_os("ASAMU_CONVERTED_DIR").map(PathBuf::from) else {
        eprintln!("ASAMU_CONVERTED_DIR not set: skipping");
        return None;
    };
    if !dir.join("levels").is_dir() {
        eprintln!("{} has no levels: skipping", dir.display());
        return None;
    }
    let path = dir.join("meshes").join("manifest.json");
    let Ok(data) = std::fs::read(&path) else {
        eprintln!("{} not readable: skipping", path.display());
        return None;
    };
    let manifest = MeshManifest::from_json(&path, &data).expect("mesh manifest");
    if manifest.version < MESH_MANIFEST_VERSION {
        eprintln!(
            "mesh manifest version {} has no simple collision: skipping (convert again)",
            manifest.version
        );
        return None;
    }
    Some((dir, manifest))
}

fn scene_file(dir: &Path, map: &str) -> Option<SceneFile> {
    let data = std::fs::read(dir.join("levels").join(format!("{map}.scene.json"))).ok()?;
    Some(serde_json::from_slice(&data).expect("scene file"))
}

fn load(dir: &Path, map: &str) -> LoadedMap {
    scene::load_map(&DirSource::new(dir), map, &LoadOptions::default()).expect("map loads")
}

/// (simple body, no pawn collision, triangles).
type Counts = [usize; 3];

/// Classifies the pawn-blocking static-mesh components of one scene by the
/// route of their mesh. `stored_flags` counts a component as blocking by the
/// actor's stored flags alone, as the script behind the findings' V6 did;
/// otherwise by the flags the loader derives (the collision type and the
/// classes that set it when play begins applied). Also returns, per
/// `(kind, collision type)`, the components the two rules disagree on.
fn classify(
    file: &SceneFile,
    manifest: &MeshManifest,
    stored_flags: bool,
    disagree: &mut BTreeMap<(String, String), usize>,
) -> Counts {
    let mut counts = [0usize; 3];
    for actor in &file.actors {
        let derived = effective_collision(actor);
        let stored = EffectiveCollision {
            collide: actor.collide_actors,
            block: actor.block_actors,
            blocks_weapons_only: false,
            component: None,
        };
        for comp in &actor.components {
            if comp.kind != "static_mesh" {
                continue;
            }
            let Some(path) = comp.static_mesh.as_deref() else {
                continue;
            };
            if manifest.get_for_package(path, &file.package).is_none() {
                continue;
            }
            let (by_derived, _) = component_blocking(actor, &derived, comp);
            let (by_stored, _) = component_blocking(actor, &stored, comp);
            if by_derived != by_stored {
                let key = (
                    actor.kind.clone(),
                    actor.collision_type.clone().unwrap_or_default(),
                );
                *disagree.entry(key).or_default() += 1;
            }
            if !(if stored_flags { by_stored } else { by_derived }) {
                continue;
            }
            let record = manifest.simple_collision_for_package(path, &file.package);
            let routes = mesh_routes(record);
            assert!(routes.recorded, "{path} has no simple-collision record");
            let swept = record.map_or(0, |r| r.convex + r.boxes);
            let class = match routes.pawn {
                TraceRoute::Body if swept > 0 => 0,
                TraceRoute::Body | TraceRoute::Nothing => 1,
                TraceRoute::Triangles => 2,
            };
            counts[class] += 1;
        }
    }
    counts
}

/// `docs/PARITY_FINDINGS.md` V6: pawn-blocking placements per map (the
/// persistent level only) as simple / no pawn collision / per-triangle.
const V6: [(&str, Counts); 8] = [
    ("AG-Workshop", [273, 755, 3]),
    ("AG-ParadiseCave", [1518, 222, 237]),
    ("AG-BeautifulCity", [2054, 1080, 142]),
    ("AG-Darkcave", [1301, 800, 108]),
    ("AG-StarHaven", [3112, 1828, 7]),
    ("AG-IceCave", [4090, 358, 138]),
    ("AG-Epilogue", [288, 516, 2]),
    ("TheCore", [44, 98, 0]),
];

#[test]
fn placements_by_how_the_pawn_meets_them() {
    let Some((dir, manifest)) = converted() else {
        return;
    };
    eprintln!(
        "{:<18} {:>22} {:>22} {:>22}",
        "map", "V6 (stored flags)", "ours, stored flags", "loader"
    );
    for (map, expected) in V6 {
        let Some(file) = scene_file(&dir, map) else {
            eprintln!("{map}: not converted, skipped");
            continue;
        };
        let mut disagree = BTreeMap::new();
        // By the stored flags our classification is the findings' own.
        let stored = classify(&file, &manifest, true, &mut disagree);
        assert_eq!(stored, expected, "{map}: simple / none / triangles");
        // The loader counts by the flags it derives, over the map and the
        // sub-levels it streams.
        let loaded = load(&dir, map);
        let mut derived = [0usize; 3];
        for level in &loaded.levels {
            let level_file = scene_file(&dir, &level.name).expect("sub-level scene");
            let mut ignore = BTreeMap::new();
            let c = classify(&level_file, &manifest, false, &mut ignore);
            for k in 0..3 {
                derived[k] += c[k];
            }
        }
        let s = loaded.stats;
        // The classes above are by the mesh. The loader also leaves out the
        // body of a placement too small for the original to look at it
        // (`scene::simple_body_checked`): those are counted with "none" and
        // on their own.
        assert_eq!(
            [
                s.pawn_simple + s.pawn_scale_skipped,
                s.pawn_none - s.pawn_scale_skipped,
                s.pawn_triangles
            ],
            derived,
            "{map}: the loader's counts"
        );
        assert_eq!(s.pawn_approximate, 0, "{map}: {:?}", loaded.warnings);
        assert_eq!(
            s.pawn_components,
            derived.iter().sum::<usize>(),
            "{map}: every pawn-blocking component is in one class"
        );
        let own = classify(&file, &manifest, false, &mut BTreeMap::new());
        eprintln!(
            "{map:<18} {:>22} {:>22} {:>22}   persistent level by derived flags {own:?}, \
             simple parts {}, too small for their body {}, levels {}",
            format!("{expected:?}"),
            format!("{stored:?}"),
            format!("{derived:?}"),
            s.simple_parts,
            s.pawn_scale_skipped,
            loaded.levels.len(),
        );
        // Where the two rules differ, the actor's flags are not its stored
        // ones: its class sets its collision type when play begins (falling
        // rocks, checkpoints) or it stores a collision type of its own.
        for ((kind, collision_type), n) in &disagree {
            eprintln!("    {n:>4} components of kind {kind} with {collision_type}");
            assert!(
                matches!(
                    kind.as_str(),
                    "falling_rock" | "falling_when_grappled_rock" | "checkpoint"
                ) || (collision_type != "COLLIDE_CustomDefault" && !collision_type.is_empty()),
                "{map}: {kind} {collision_type}"
            );
        }
        if disagree.is_empty() {
            assert_eq!(own, expected, "{map}");
        }
    }
}

/// `docs/PARITY_FINDINGS.md` V14: `DynamicBlockingVolume` actors per level
/// as (all, starting with `bEnabled = false`). 24 and 15 in all.
const V14: [(&str, usize, usize); 8] = [
    ("AG-Workshop", 0, 0),
    ("AG-ParadiseCave", 0, 0),
    ("AG-BeautifulCity", 7, 2),
    ("AG-Darkcave", 6, 6),
    ("AG-StarHaven", 7, 3),
    ("AG-IceCave", 3, 3),
    ("AG-Epilogue", 0, 0),
    ("TheCore", 1, 1),
];

fn dynamic_blocking_volumes(file: &SceneFile) -> Vec<&ActorJson> {
    file.actors
        .iter()
        .filter(|a| a.class.eq_ignore_ascii_case("Engine.DynamicBlockingVolume"))
        .collect()
}

#[test]
fn volumes_start_switched_as_their_flag_says() {
    let Some((dir, _)) = converted() else {
        return;
    };
    let (mut all, mut off) = (0usize, 0usize);
    for (map, expected_all, expected_off) in V14 {
        let Some(file) = scene_file(&dir, map) else {
            eprintln!("{map}: not converted, skipped");
            continue;
        };
        // The scene data carries the flag for every one of them.
        let volumes = dynamic_blocking_volumes(&file);
        let disabled = volumes
            .iter()
            .filter(|a| switchable_volume(a) == Some(false))
            .count();
        assert!(volumes.iter().all(|a| switchable_volume(a).is_some()));
        assert_eq!(
            (volumes.len(), disabled),
            (expected_all, expected_off),
            "{map}"
        );
        all += volumes.len();
        off += disabled;
        // The loader places them all and starts each as its flag says.
        let loaded = load(&dir, map);
        let (mut placed, mut placed_off) = (0usize, 0usize);
        for (index, level) in loaded.levels.iter().enumerate() {
            let level_file = scene_file(&dir, &level.name).expect("sub-level scene");
            for a in dynamic_blocking_volumes(&level_file) {
                let id = actor_id(index as u8, a.slot).unwrap();
                assert_eq!(
                    loaded.actor_collision(id),
                    switchable_volume(a),
                    "{map}: {}",
                    a.name
                );
                placed += 1;
                placed_off += usize::from(switchable_volume(a) == Some(false));
            }
        }
        assert_eq!(
            (
                loaded.stats.switchable_volumes,
                loaded.stats.disabled_volumes
            ),
            (placed, placed_off),
            "{map}"
        );
        assert_eq!(loaded.actors_without_collision().len(), placed_off, "{map}");
        eprintln!(
            "{map:<18} dynamic blocking volumes {expected_all}, disabled {expected_off}; \
             with sub-levels {placed}, disabled {placed_off}"
        );
    }
    if all > 0 {
        assert_eq!((all, off), (24, 15));
    }
}

/// V13: in the recording, a grapple flight in AG-BeautifulCity passes
/// through `DynamicBlockingVolume_6`, which starts with `bEnabled = false`:
/// the original's pawn centre is 15.0, 3.1 and 11.6 uu from its surface on
/// three consecutive ticks, with nothing else within 120 uu. The findings'
/// fix plan names the point (4353, −595, 37937) inside it. The pawn's box
/// there overlaps nothing and moves freely; with the volume switched on it
/// is inside the volume.
#[test]
fn the_disabled_volume_of_the_recorded_flight_is_passed() {
    let Some((dir, _)) = converted() else {
        return;
    };
    let Some(file) = scene_file(&dir, "AG-BeautifulCity") else {
        return;
    };
    let mut map = load(&dir, "AG-BeautifulCity");
    let volume = file
        .actors
        .iter()
        .find(|a| a.name == "DynamicBlockingVolume_6")
        .expect("the volume");
    assert_eq!(switchable_volume(volume), Some(false));
    let id = actor_id(0, volume.slot).unwrap();
    let mask = map.initial_level_mask();
    let filter = QueryFilter {
        sublevels: mask,
        ..QueryFilter::PAWN
    };
    let p = Vec3::new(4353.0, -595.0, 37937.0);
    assert!(!map.collision.overlaps_box(&map.dynamic, p, E, filter));
    for d in [
        Vec3::new(60.0, 0.0, 0.0),
        Vec3::new(-60.0, 0.0, 0.0),
        Vec3::new(0.0, 60.0, 0.0),
        Vec3::new(0.0, -60.0, 0.0),
        Vec3::new(0.0, 0.0, 60.0),
        Vec3::new(0.0, 0.0, -60.0),
    ] {
        assert_eq!(
            map.collision.sweep_box(&map.dynamic, p, p + d, E, filter),
            None,
            "{d}"
        );
    }
    let hulls = map.set_actor_collision(id, true);
    assert!(hulls > 0);
    assert!(map.collision.overlaps_box(&map.dynamic, p, E, filter));
    eprintln!("DynamicBlockingVolume_6: {hulls} hulls, free while disabled");
}

/// V18: in AG-Workshop the original's pawn stands at z = 119.15 on 1,817
/// records; 119.15 = floor + 44 (half height) + 2.15 (the walking hover) +
/// 1.0 (the pull-back of a hit on BSP), so the box's first contact is at
/// 116.0 with the floor at 72. At (131, −339), the place the fix plan
/// names, a rug lies on that floor (a mesh without a simple body: two
/// triangles at z = 72.2, P1): the pawn's box must not notice it.
#[test]
fn the_workshop_floor_is_met_at_the_recorded_height() {
    let Some((dir, _)) = converted() else {
        return;
    };
    if scene_file(&dir, "AG-Workshop").is_none() {
        return;
    }
    let map = load(&dir, "AG-Workshop");
    let start = Vec3::new(131.0, -339.0, 200.0);
    let end = Vec3::new(131.0, -339.0, 0.0);
    let hit = map
        .collision
        .sweep_box(&map.dynamic, start, end, E, QueryFilter::PAWN)
        .expect("a floor");
    let z = hit.point(start, end).z;
    eprintln!(
        "Workshop floor at (131, -339): box centre {z:.4}, {:?}",
        hit.kind
    );
    assert!((z - 116.0).abs() < 0.01, "{z}");
    assert_eq!(hit.kind, PrimitiveKind::Bsp);
    // A bullet trace does end on the rug, above the floor.
    let trace = map
        .collision
        .raycast(&map.dynamic, start, end, QueryFilter::TRACE)
        .expect("something below");
    eprintln!(
        "    bullet trace ends at z {:.4} on {:?}",
        trace.point(start, end).z,
        trace.kind
    );
    assert!(trace.point(start, end).z >= 72.0 - 1.0e-3);
}

/// V11: walking along −x at y = −351.1 in AG-Workshop the original's pawn
/// is stopped by the door (`workshop_door_01`, a mesh with a simple body).
/// The recording fixes the plane on which the swept box's centre first
/// touches it twice over (NATIVE_PHYSICS.md 3.2): the pawn rests at
/// x = 303.0848 after a move shorter than 1 uu, which leaves the flat
/// pull-back of 0.1, and one tick earlier it is at 303.1922 after a move
/// that leaves 0.2074. Both put the plane at x = 302.9848.
///
/// A move sweeps the pawn's box with its X and Y half sizes multiplied by
/// 1.001 (`UWorld::MoveActor`, NATIVE_PHYSICS.md 9.2): 21.021. The door's
/// body is one convex element whose face is at x = 281.96387, so the plane
/// is at 281.96387 + 21.021 = 302.98487.
#[test]
fn the_workshop_door_is_met_on_the_recorded_plane() {
    let Some((dir, _)) = converted() else {
        return;
    };
    if scene_file(&dir, "AG-Workshop").is_none() {
        return;
    }
    let map = load(&dir, "AG-Workshop");
    let start = Vec3::new(330.0, -351.1, 119.15);
    let end = Vec3::new(250.0, -351.1, 119.15);
    let hit = map
        .collision
        .sweep_box(&map.dynamic, start, end, MOVE_E, QueryFilter::PAWN)
        .expect("the door");
    let x = hit.point(start, end).x;
    eprintln!(
        "Workshop door: box centre stopped at x {x:.5} ({:?}, normal {}); recorded plane 302.9848",
        hit.kind, hit.normal
    );
    assert_eq!(hit.kind, PrimitiveKind::SimpleHull);
    assert!((x - 302.9848).abs() < 0.002, "{x}");
    assert_eq!(hit.normal, glam::DVec3::X);
}

/// V20: at the start of AG-BeautifulCity (on `Village_Entrance_Cave`, a
/// mesh with a simple body) the original's pawn falls from the PlayerStart.
/// The recording (DC1 segment 1) has it at z = 54994.4492 on tick 6, still
/// falling at 273.07 uu/s, and at rest at z = 54996.5977 from tick 7 on.
///
/// - It reached 54994.4492 in a free step, so the first contact of its box
///   is below that.
/// - On tick 7 the fall's step is 4.82 uu (273.07 + 17.75 uu/s over the
///   0.016566 s of a tick there). A hit is reported short of the contact by
///   0.1 of the move (0.482), so the pawn lands at the contact + 0.482, or
///   where it was if it is nearer than that. The floor check then reports a
///   distance of 0 (its own pull-back is 1.0) and the pawn is lifted by the
///   2.15 of the walking hover: 54996.5977 − 2.15 = 54994.4477 is where it
///   landed, which is where it was. So the contact is no more than 0.482
///   below 54994.4477: above 54993.965.
///
/// The box's first contact below the start must lie in
/// [54993.965, 54994.4492]. On the mesh's triangles ours came to rest 12.43
/// uu lower.
#[test]
fn the_beautiful_city_start_lands_at_the_recorded_height() {
    let Some((dir, _)) = converted() else {
        return;
    };
    if scene_file(&dir, "AG-BeautifulCity").is_none() {
        return;
    }
    let map = load(&dir, "AG-BeautifulCity");
    let ps = map.actors.player_start().expect("a start").location;
    let end = ps - Vec3::Z * 300.0;
    let filter = QueryFilter {
        sublevels: map.initial_level_mask(),
        ..QueryFilter::PAWN
    };
    let hit = map
        .collision
        .sweep_box(&map.dynamic, ps, end, MOVE_E, filter)
        .expect("ground below the start");
    let z = hit.point(ps, end).z;
    eprintln!(
        "BeautifulCity start {ps}: box centre stopped at z {z:.4} ({:?}); recorded window 54993.965 to 54994.4492",
        hit.kind
    );
    assert_eq!(hit.kind, PrimitiveKind::SimpleHull);
    // The recorded heights are f32 near 55,000: one step is 0.0039.
    assert!((54993.965 - 0.004..=54994.4492 + 0.004).contains(&z), "{z}");
}

/// How much of the data the scale-dependent rule touches, and that on the
/// real levels the hierarchy and the reference without one agree on sweeps
/// around the placements it touches most.
#[test]
fn the_scale_rule_on_the_real_levels() {
    let Some((dir, _)) = converted() else {
        return;
    };
    for (map, _) in V6 {
        if scene_file(&dir, map).is_none() {
            continue;
        }
        let loaded = load(&dir, map);
        let statics = loaded.collision.statics();
        let pawn_hulls: Vec<_> = statics
            .iter()
            .filter(|i| i.kind == PrimitiveKind::SimpleHull && i.info.blocks_pawn)
            .collect();
        let wide: Vec<_> = pawn_hulls
            .iter()
            .filter(|i| i.native.is_some_and(|r| r.skips_face_axes()))
            .collect();
        let wide_actors: std::collections::BTreeSet<_> =
            wide.iter().filter_map(|i| i.info.actor).collect();
        // Pawn-blocking triangle meshes at a scale at which the original
        // would leave out a face axis of the box (two scales multiplying to
        // more than 10,000), which is not reproduced.
        let big_triangles = statics
            .iter()
            .filter(|i| i.kind == PrimitiveKind::TriangleMesh && i.info.blocks_pawn)
            .filter(|i| {
                let s = [
                    i.to_world.rows[0].length(),
                    i.to_world.rows[1].length(),
                    i.to_world.rows[2].length(),
                ];
                s[0] * s[1] > 1.0e4 || s[1] * s[2] > 1.0e4 || s[0] * s[2] > 1.0e4
            })
            .count();
        let volumes: Vec<_> = statics
            .iter()
            .filter(|i| i.kind == PrimitiveKind::BrushHull)
            .collect();
        let wide_volumes = volumes
            .iter()
            .filter(|i| i.native.is_some_and(|r| r.skips_face_axes()))
            .count();
        let ruled = statics.iter().filter(|i| i.native.is_some()).count();
        eprintln!(
            "{map:<18} pawn-blocking body shapes {:>6}, leaving out a face axis {:>5} (of {:>4} actors); \
             pawn-blocking triangle meshes beyond the triangle rule's face threshold {big_triangles}; \
             volume hulls {} (leaving out a face axis {wide_volumes}); instances under the rule {ruled} of {}",
            pawn_hulls.len(),
            wide.len(),
            wide_actors.len(),
            volumes.len(),
            statics.len(),
        );
        // Sweeps down onto, and sideways into, a few of the widest.
        let filter = QueryFilter::PAWN;
        let step = (wide.len() / 6).max(1);
        let mut checked = 0usize;
        for inst in wide.iter().step_by(step).take(6) {
            let c = (inst.bounds.min + inst.bounds.max) * 0.5;
            let h = (inst.bounds.max - inst.bounds.min) * 0.5;
            for (a, b) in [
                (c + Vec3::Z * (h.z + 200.0), c - Vec3::Z * 20.0),
                (c + Vec3::X * (h.x + 150.0) + Vec3::Y * 30.0, c),
            ] {
                let fast = loaded.collision.sweep_box(&loaded.dynamic, a, b, E, filter);
                let slow = loaded
                    .collision
                    .sweep_box_brute_force(&loaded.dynamic, a, b, E, filter);
                assert_eq!(fast, slow, "{map}: {a} -> {b}");
                checked += 1;
            }
        }
        eprintln!("    {checked} sweeps agree with the reference");
    }
}
