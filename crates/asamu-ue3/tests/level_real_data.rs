//! Level, BSP and scene extraction over every shipped map, built from the
//! user's own install (read-only). Skips cleanly when the data is absent.
//!
//! Acceptance test for `docs/reverse-engineering/LEVEL_FORMAT.md`: every
//! `Level`, `Model`, `Polys` and `BrushComponent` export of every map decodes
//! with exact consumption, BSP models pass their structural checks, brush
//! polygons transformed by the recovered `LocalToWorld` land on the BSP
//! surfaces built from them, and the scenes contain the gameplay actor counts
//! recorded by the defaults and Kismet work (`DEFAULTS.md`, `ABILITIES.md`).
//! Only counts are asserted; nothing is written.

#![allow(clippy::unwrap_used)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use asamu_ue3::bsp;
use asamu_ue3::level::{self, ActorKind, ParamValue, Scene, SceneOptions};
use asamu_ue3::model::PackageSet;
use asamu_ue3::{LoadedPackage, PackageIndex};

const COOKED: &str = "A Story About My Uncle.app/Contents/Resources/ASAMU/CookedMac";

fn cooked_dir() -> Option<PathBuf> {
    let root = match std::env::var_os("ASAMU_ORIGINAL_DIR") {
        Some(r) => PathBuf::from(r),
        None => {
            let home = std::env::var_os("HOME")?;
            PathBuf::from(home)
                .join("Library/Application Support/Steam/steamapps/common/A Story About My Uncle")
        }
    };
    let dir = root.join(COOKED);
    dir.is_dir().then_some(dir)
}

macro_rules! require_data {
    () => {
        match cooked_dir() {
            Some(d) => d,
            None => {
                eprintln!(
                    "SKIP: original game data not found (set ASAMU_ORIGINAL_DIR to the folder \
                     containing 'A Story About My Uncle.app')"
                );
                return;
            }
        }
    };
}

fn maps(dir: &Path) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = std::fs::read_dir(dir.join("Maps"))
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.extension()
                .is_some_and(|x| x.eq_ignore_ascii_case("asamu"))
        })
        .collect();
    v.sort();
    v
}

fn stem(p: &Path) -> String {
    p.file_stem().unwrap().to_string_lossy().into_owned()
}

/// A fresh set per map keeps memory bounded.
fn open(dir: &Path, file: &Path) -> (PackageSet, Arc<LoadedPackage>) {
    let set = PackageSet::new(&[dir.to_path_buf(), dir.join("Maps")]);
    let lp = set.open_file(file).unwrap();
    (set, lp)
}

fn scene_of(set: &PackageSet, lp: &Arc<LoadedPackage>) -> Scene {
    let levels = level::level_exports(&lp.package);
    assert_eq!(levels.len(), 1, "{}: one Level export", lp.name);
    level::extract_scene(set, lp, levels[0], &SceneOptions::default()).unwrap()
}

#[test]
fn native_tails_consume_exactly() {
    let dir = require_data!();
    let mut totals: BTreeMap<String, (usize, usize)> = BTreeMap::new();
    for file in maps(&dir) {
        let (set, lp) = open(&dir, &file);
        let pkg = &lp.package;
        for i in 0..pkg.exports.len() {
            let class = pkg.export_class_name(i).unwrap();
            let own = Some(lp.name.as_str());
            let r = match class.as_str() {
                "Level" => level::decode_level(pkg, own, i, &set).map(|_| ()),
                "Model" => bsp::decode_model(pkg, own, i, &set).map(|_| ()),
                "Polys" => bsp::decode_polys(pkg, own, i, &set).map(|_| ()),
                "BrushComponent" => bsp::decode_brush_component(pkg, own, i, &set).map(|_| ()),
                _ => continue,
            };
            let t = totals.entry(class.clone()).or_default();
            t.1 += 1;
            match r {
                Ok(()) => t.0 += 1,
                Err(e) => eprintln!("{} export {i} ({class}): {e}", stem(&file)),
            }
        }
    }
    eprintln!("exact / total: {totals:?}");
    let expect = [
        ("BrushComponent", 1204),
        ("Level", 12),
        ("Model", 372),
        ("Polys", 372),
    ];
    for (k, n) in expect {
        assert_eq!(totals.get(k), Some(&(n, n)), "{k}");
    }
}

#[test]
fn bsp_models_are_consistent() {
    let dir = require_data!();
    let (mut models, mut refs, mut polygons, mut agree) = (0usize, 0usize, 0usize, 0usize);
    let (mut level_points, mut level_off, mut level_max) = (0usize, 0usize, 0.0f64);
    for file in maps(&dir) {
        let (set, lp) = open(&dir, &file);
        let pkg = &lp.package;
        let level_model = level::level_exports(pkg)
            .first()
            .and_then(|&l| level::decode_level(pkg, Some(&lp.name), l, &set).ok())
            .and_then(|(_, t)| t.model.export_index());
        for i in 0..pkg.exports.len() {
            if pkg.export_class_name(i).unwrap() != "Model" {
                continue;
            }
            let (obj, m) = bsp::decode_model(pkg, Some(&lp.name), i, &set).unwrap();
            let c = m.check();
            models += 1;
            refs += c.references;
            assert_eq!(c.bad_references, 0, "{}: {:?}", obj.path, c.examples);
            assert_eq!(c.non_unit_planes, 0, "{}", obj.path);
            assert_eq!(m.vert_element_size, bsp::VERT_SIZE, "{}", obj.path);
            polygons += c.winding_agrees + c.winding_opposes;
            agree += c.winding_agrees;
            if Some(i) == level_model {
                assert!(m.nodes.iter().all(|n| n.node_flags == 0), "{}", obj.path);
                level_points += c.plane_points;
                level_off += c.off_plane_points;
                level_max = level_max.max(c.max_plane_distance);
            }
        }
    }
    eprintln!(
        "{models} models, {refs} references in range; {agree} of {polygons} polygons wind with \
         their node normal; level BSPs: {level_off} of {level_points} points off-plane, max \
         {level_max:.3} UU"
    );
    assert_eq!(models, 372);
    assert_eq!(refs, 41_108);
    assert_eq!((agree, polygons), (2_693, 2_693));
    // Level BSPs: points sit on their node planes up to vertex snapping.
    assert_eq!((level_off, level_points), (83, 5_274));
    assert!(level_max > 6.6 && level_max < 6.7, "{level_max}");
}

/// Per-map size of the level BSP (LEVEL_FORMAT.md table): nodes, surfaces,
/// triangles; every level surface has `PolyFlags` 0xE00.
#[test]
fn level_bsp_sizes() {
    let dir = require_data!();
    let expect: BTreeMap<&str, (usize, usize, usize)> = [
        ("AG-BeautifulCity", (6, 6, 12)),
        ("AG-Darkcave", (178, 49, 486)),
        ("AG-Epilogue", (307, 134, 904)),
        ("AG-IceCave", (12, 12, 24)),
        ("AG-ParadiseCave", (6, 6, 12)),
        ("AG-StarHaven", (0, 0, 0)),
        ("AG-Workshop", (297, 130, 877)),
        ("ASAMUEntry", (12, 12, 24)),
        ("ASAMUFrontEndMap", (252, 121, 747)),
        ("ASAMULegal", (12, 12, 24)),
        ("Freds_place", (0, 0, 0)),
        ("TheCore", (0, 0, 0)),
    ]
    .into_iter()
    .collect();
    let mut surfaces = 0;
    for file in maps(&dir) {
        let (set, lp) = open(&dir, &file);
        let pkg = &lp.package;
        let levels = level::level_exports(pkg);
        let (_, t) = level::decode_level(pkg, Some(&lp.name), levels[0], &set).unwrap();
        let g = level::extract_bsp(&set, &lp, t.model.export_index().unwrap()).unwrap();
        let name = stem(&file);
        let got = (g.counts.nodes, g.counts.surfs, g.collision.triangle_count());
        assert_eq!(Some(&got), expect.get(name.as_str()), "{name}");
        assert_eq!(g.visible, g.collision, "{name}");
        assert!(g.surfaces.iter().all(|s| s.poly_flags == 0xE00), "{name}");
        surfaces += g.surfaces.len();
    }
    assert_eq!(surfaces, 482);
}

#[test]
fn level_tails_are_consistent() {
    let dir = require_data!();
    let mut ranges = Vec::new();
    let (mut dynamic_keys, mut force_stream) = (0usize, 0usize);
    for file in maps(&dir) {
        let (set, lp) = open(&dir, &file);
        let pkg = &lp.package;
        let levels = level::level_exports(pkg);
        let (_, t) = level::decode_level(pkg, Some(&lp.name), levels[0], &set).unwrap();
        let class = |idx: PackageIndex| {
            idx.export_index()
                .map(|i| pkg.export_class_name(i).unwrap())
                .unwrap_or_default()
        };
        let name = stem(&file);
        // The engine keeps WorldInfo first and the builder brush second.
        assert_eq!(class(t.actors[0]), "WorldInfo", "{name}");
        assert_eq!(class(t.actors[1]), "Brush", "{name}");
        assert_eq!(t.actors_owner.export_index(), Some(levels[0]), "{name}");
        assert_eq!(class(t.model), "Model", "{name}");
        assert!(
            t.model_components
                .iter()
                .all(|&c| class(c) == "ModelComponent")
        );
        assert_eq!(t.game_sequences.len(), 1, "{name}");
        // ASAMUEntry has no Kismet: its single entry is null.
        let expect = if name == "ASAMUEntry" { "" } else { "Sequence" };
        assert_eq!(class(t.game_sequences[0]), expect, "{name}");
        assert_eq!(t.skipped_block_bytes, 16, "{name}");
        assert_eq!(
            t.cached_phys_sm_map, t.cached_phys_sm_store.entries,
            "{name}"
        );
        assert_eq!(
            t.cached_phys_per_tri_map, t.cached_phys_per_tri_store,
            "{name}"
        );
        assert_eq!(t.cached_phys_sm_version, 34_079_889, "{name}");
        // Never exercised by the shipped data (see LEVEL_FORMAT.md).
        assert_eq!(t.visibility.buckets, 0, "{name}");
        assert_eq!(t.distance_field.voxels, 0, "{name}");
        assert!(t.cross_level_actors.is_empty() && t.cover_link_refs.is_empty());
        let has_light_volume = !matches!(name.as_str(), "ASAMUEntry" | "ASAMULegal");
        assert_eq!(t.light_volume.is_some(), has_light_volume, "{name}");
        // The f32 after the light-volume bounds is 0 everywhere.
        assert!(t.light_volume.is_none_or(|v| v.spacing == 0.0), "{name}");
        // Freds_place has no BSP and a zero BSP physics version.
        let bsp_version = if name == "Freds_place" { 0 } else { 34_079_889 };
        assert_eq!(t.cached_phys_bsp_version, bsp_version, "{name}");
        ranges.push((
            t.actors.len(),
            t.model_components.len(),
            t.texture_to_instances.entries,
        ));
        dynamic_keys = dynamic_keys.max(t.dynamic_texture_instances.entries);
        force_stream = force_stream.max(t.force_stream_textures);
        assert_eq!(t.dynamic_texture_instances.values, 0, "{name}");
        assert_eq!(
            (
                t.cached_phys_convex_bsp,
                t.cached_phys_convex_bsp_version,
                t.cross_level_cover_guid_refs,
                t.cover_index_pairs
            ),
            (0, 0, 0, 0),
            "{name}"
        );
    }
    assert_eq!((dynamic_keys, force_stream), (4_080, 14));
    let span = |k: fn(&(usize, usize, usize)) -> usize| {
        let v: Vec<usize> = ranges.iter().map(k).collect();
        (v.iter().min().copied(), v.iter().max().copied())
    };
    assert_eq!(span(|r| r.0), (Some(4), Some(7_552)), "actor slots");
    assert_eq!(span(|r| r.1), (Some(0), Some(52)), "model components");
    assert_eq!(span(|r| r.2), (Some(2), Some(1_300)), "streamed textures");
}

/// (map, actor slots, null slots, components, brushes/volumes with geometry,
/// KillZ, [player starts, checkpoints, kill zones, dynamic kill zones,
/// falling rocks, falling-when-grappled rocks, recharge crystals, attractor
/// pads, collectibles]).
type Row = (&'static str, usize, usize, usize, usize, f32, [usize; 9]);

const MAPS: &[Row] = &[
    (
        "AG-BeautifulCity",
        4870,
        80,
        6058,
        238,
        -1e10,
        [1, 12, 7, 0, 0, 0, 0, 0, 5],
    ),
    (
        "AG-Darkcave",
        3498,
        10,
        4882,
        111,
        -1e10,
        [1, 17, 12, 0, 0, 0, 0, 0, 5],
    ),
    (
        "AG-Epilogue",
        1005,
        20,
        1133,
        47,
        1.0,
        [1, 1, 0, 0, 0, 0, 0, 0, 0],
    ),
    (
        "AG-IceCave",
        6587,
        20,
        8410,
        269,
        -1e7,
        [1, 28, 33, 3, 134, 32, 98, 0, 5],
    ),
    (
        "AG-ParadiseCave",
        4383,
        55,
        6367,
        153,
        -1e9,
        [1, 24, 26, 0, 0, 0, 0, 0, 5],
    ),
    (
        "AG-StarHaven",
        7552,
        100,
        9802,
        286,
        -1e7,
        [1, 25, 13, 0, 0, 0, 7, 0, 5],
    ),
    (
        "AG-Workshop",
        1257,
        28,
        1442,
        46,
        1.0,
        [1, 1, 0, 0, 0, 0, 0, 1, 0],
    ),
    (
        "ASAMUEntry",
        4,
        0,
        3,
        2,
        -262_143.0,
        [1, 0, 0, 0, 0, 0, 0, 0, 0],
    ),
    (
        "ASAMUFrontEndMap",
        1165,
        5,
        1363,
        45,
        1.0,
        [1, 1, 0, 0, 0, 0, 0, 1, 0],
    ),
    (
        "ASAMULegal",
        4,
        0,
        3,
        2,
        -262_143.0,
        [1, 0, 0, 0, 0, 0, 0, 0, 0],
    ),
    (
        "Freds_place",
        73,
        0,
        94,
        0,
        -262_143.0,
        [0, 0, 0, 0, 0, 0, 0, 0, 0],
    ),
    (
        "TheCore",
        205,
        1,
        404,
        4,
        -262_143.0,
        [0, 0, 0, 0, 0, 0, 0, 1, 0],
    ),
];

const GAMEPLAY: [ActorKind; 9] = [
    ActorKind::PlayerStart,
    ActorKind::Checkpoint,
    ActorKind::KillZone,
    ActorKind::DynamicKillZone,
    ActorKind::FallingRock,
    ActorKind::FallingWhenGrappledRock,
    ActorKind::RechargeCrystal,
    ActorKind::TelePadAttractor,
    ActorKind::Collectible,
];

#[test]
fn scenes_extract_for_every_map() {
    let dir = require_data!();
    let files = maps(&dir);
    assert_eq!(files.len(), MAPS.len());
    let mut totals = [0usize; 9];
    let (mut cp_index, mut cp_offset, mut cp_kismet, mut rock_distance) = (0, 0, 0, 0);
    let mut matinee_actors = 0;
    let (mut sm_actors, mut sm_components, mut overrides, mut lights, mut with_archetype) =
        (0usize, 0usize, 0usize, 0usize, 0usize);
    let mut max_merged = 0usize;
    for (file, row) in files.iter().zip(MAPS) {
        let (set, lp) = open(&dir, file);
        let scene = scene_of(&set, &lp);
        let (name, slots, nulls, components, volumes, kill_z, counts) = *row;
        assert_eq!(stem(file), name);
        let s = &scene.stats;
        assert_eq!(s.actor_slots, slots, "{name}");
        assert_eq!(s.null_slots, nulls, "{name}");
        assert_eq!(s.actors, slots - nulls, "{name}");
        assert_eq!(s.components, components, "{name}");
        assert_eq!(s.volumes_with_geometry, volumes, "{name}");
        assert_eq!(
            s.unlisted_actors, 0,
            "{name}: actors outside ULevel::Actors"
        );
        assert_eq!(
            s.foreign_actors, 0,
            "{name}: listed actors outside the level"
        );
        assert_eq!(s.decode_failures, 0, "{name}");
        assert_eq!((s.duplicate_slots, s.budget_skips), (0, 0), "{name}");
        // The scene budgets leave ample room for the shipped maps.
        assert!(
            s.merged_weight * 4 < level::MAX_SCENE_MERGED_WEIGHT,
            "{name}: {}",
            s.merged_weight
        );
        assert!(
            s.geometry_elements * 100 < level::MAX_SCENE_GEOMETRY,
            "{name}"
        );
        max_merged = max_merged.max(s.merged_weight);
        sm_actors += s.kinds.get(&ActorKind::StaticMesh).copied().unwrap_or(0);
        lights += s
            .component_kinds
            .get(&level::ComponentKind::Light)
            .copied()
            .unwrap_or(0);
        // Exactly the exports with a state frame (RF_HasStack) are actors.
        let with_stack = lp
            .package
            .exports
            .iter()
            .filter(|e| e.object_flags & asamu_ue3::flags::object::HAS_STACK != 0)
            .count();
        assert_eq!(with_stack, s.actors, "{name}");
        assert!(scene.actors.iter().all(|a| {
            lp.package.exports[a.export_index].object_flags & asamu_ue3::flags::object::HAS_STACK
                != 0
        }));
        assert!(scene.warnings.is_empty(), "{name}: {:?}", scene.warnings);
        let wi = scene.world_info.as_ref().unwrap();
        assert_eq!(wi.kill_z, kill_z, "{name}");
        assert_eq!(wi.default_gravity_z, -520.0, "{name}");
        assert_eq!(
            wi.global_gravity_z, 0.0,
            "{name}: gravity is never overridden"
        );
        for (k, (kind, want)) in GAMEPLAY.iter().zip(counts).enumerate() {
            let got = s.kinds.get(kind).copied().unwrap_or(0);
            assert_eq!(got, want, "{name}: {kind:?}");
            totals[k] += got;
        }
        let streaming: Vec<String> = scene
            .streaming_levels
            .iter()
            .filter_map(|l| l.package_name.clone())
            .collect();
        let expect_streaming: &[&str] = match name {
            "AG-BeautifulCity" => &["freds_place"],
            "AG-IceCave" => &["thecore"],
            _ => &[],
        };
        assert_eq!(streaming, expect_streaming, "{name}");
        for l in &scene.streaming_levels {
            let class = if name == "AG-IceCave" {
                "LevelStreamingKismet"
            } else {
                "LevelStreamingAlwaysLoaded"
            };
            assert_eq!((l.class.as_str(), l.offset), (class, [0.0; 3]), "{name}");
        }
        for a in &scene.actors {
            match a.kind {
                ActorKind::Checkpoint => {
                    cp_index += usize::from(a.instance.contains_key("checkpointIndex"));
                    cp_offset += usize::from(a.instance.contains_key("spawnPointOffset"));
                    cp_kismet += usize::from(
                        a.instance.get("bTriggeredFromKismet") == Some(&ParamValue::Bool(true)),
                    );
                    assert!(a.params.contains_key("bEnabled"), "{}", a.name);
                }
                ActorKind::FallingRock => {
                    rock_distance += usize::from(a.instance.contains_key("fallDistance"));
                }
                ActorKind::PlayerStart => {
                    let cyl = a.components.iter().find_map(|c| c.cylinder);
                    assert_eq!(cyl, Some([40.0, 80.0]), "{name}: player start cylinder");
                }
                _ => {}
            }
            matinee_actors += usize::from(!a.matinee.is_empty());
            with_archetype += usize::from(a.archetype.is_some());
            for c in &a.components {
                if c.kind == level::ComponentKind::StaticMesh {
                    sm_components += 1;
                    overrides += usize::from(c.materials.iter().any(Option::is_some));
                }
            }
        }
    }
    eprintln!("largest merged-value weight {max_merged}");
    // LEVEL_FORMAT.md "Scene extraction".
    assert_eq!(sm_actors, 21_283);
    assert_eq!((sm_components, overrides), (24_631, 9_470));
    assert_eq!(lights, 3_437);
    assert_eq!(with_archetype, 80);
    // DEFAULTS.md section 7 and ABILITIES.md (map census): 10 player starts,
    // 109 checkpoints, 91 + 3 kill zones, 134 + 32 rocks, 105 crystals,
    // 3 attractor pads.
    assert_eq!(totals, [10, 109, 91, 3, 134, 32, 105, 3, 25]);
    assert_eq!((cp_index, cp_offset, cp_kismet), (101, 97, 8));
    assert_eq!(rock_distance, 134);
    assert_eq!(matinee_actors, 281);
}

fn sub(a: [f32; 3], b: [f32; 3]) -> [f64; 3] {
    [
        f64::from(a[0]) - f64::from(b[0]),
        f64::from(a[1]) - f64::from(b[1]),
        f64::from(a[2]) - f64::from(b[2]),
    ]
}

/// The BSP surfaces record their source brush and polygon. Transforming that
/// polygon with the actor's `LocalToWorld` must reproduce the surface plane.
#[test]
fn bsp_surfaces_match_transformed_brush_polygons() {
    let dir = require_data!();
    let (mut checked, mut matched, mut reversed) = (0usize, 0usize, 0usize);
    let (mut whole, mut partial) = (0usize, 0usize);
    for file in maps(&dir) {
        let (set, lp) = open(&dir, &file);
        let scene = scene_of(&set, &lp);
        let Some(model) = scene.tail.model.export_index() else {
            continue;
        };
        let g = level::extract_bsp(&set, &lp, model).unwrap();
        assert_eq!(g.visible.triangle_count(), g.collision.triangle_count());
        let by_path: BTreeMap<String, &level::SceneActor> = scene
            .actors
            .iter()
            .map(|a| {
                (
                    format!("{}.{}", scene.level, a.name).to_ascii_lowercase(),
                    a,
                )
            })
            .collect();
        let mut per_brush: BTreeMap<String, (usize, usize)> = BTreeMap::new();
        for s in &g.surfaces {
            assert_eq!(
                s.poly_flags, 0xE00,
                "every shipped BSP surface has PolyFlags 0xE00"
            );
            let (Some(actor), Ok(k)) = (&s.actor, u32::try_from(s.brush_poly)) else {
                continue;
            };
            let a = by_path.get(&actor.to_ascii_lowercase()).unwrap();
            let mesh = a.volume.as_ref().and_then(|v| v.polys.as_ref()).unwrap();
            // The polygon is the fan of the triangles tagged with its index:
            // (a, b, c), (a, c, d), ... → a, b, c, d, ...
            let fan: Vec<[u32; 3]> = mesh
                .indices
                .iter()
                .zip(&mesh.tags)
                .filter(|(_, t)| **t == k)
                .map(|(i, _)| *i)
                .collect();
            let mut poly: Vec<[f32; 3]> = vec![
                mesh.positions[fan[0][0] as usize],
                mesh.positions[fan[0][1] as usize],
            ];
            poly.extend(fan.iter().map(|t| mesh.positions[t[2] as usize]));
            let p0 = poly[0];
            let w = bsp::polygon_normal(&poly);
            let n = sub(w, [0.0; 3]);
            let len = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt();
            assert!(len > 1e-6);
            let n = [n[0] / len, n[1] / len, n[2] / len];
            let w = n[0] * f64::from(p0[0]) + n[1] * f64::from(p0[1]) + n[2] * f64::from(p0[2]);
            let cos = n[0] * f64::from(s.plane[0])
                + n[1] * f64::from(s.plane[1])
                + n[2] * f64::from(s.plane[2]);
            checked += 1;
            // Subtractive brushes carve the BSP: their surfaces face the
            // other way from the brush polygon.
            let subtract =
                a.instance.get("CsgOper") == Some(&ParamValue::Text("CSG_Subtract".to_owned()));
            assert_eq!(cos < 0.0, subtract, "{}", a.name);
            reversed += usize::from(cos < 0.0);
            let e = per_brush.entry(a.name.clone()).or_default();
            e.1 += 1;
            if cos.abs() > 0.999 && (w * cos.signum() - f64::from(s.plane[3])).abs() < 1.0 {
                matched += 1;
                e.0 += 1;
            }
        }
        whole += per_brush.values().filter(|(m, n)| m == n).count();
        partial += per_brush.values().filter(|(m, n)| m != n).count();
    }
    eprintln!(
        "{matched} of {checked} brush-sourced BSP surfaces match their transformed polygon; \
         {whole} brushes match fully, {partial} partly"
    );
    assert_eq!((checked, matched), (482, 437));
    assert_eq!(reversed, 201);
    assert_eq!((whole, partial), (80, 8));
}

/// Volumes whose cooked data has both the brush polygons and the collision
/// hulls: both decode into the same world-space box.
#[test]
fn volume_hulls_agree_with_brush_polygons() {
    let dir = require_data!();
    let (mut both, mut agree, mut hull_tris, mut hulls) = (0usize, 0usize, 0usize, 0usize);
    let (mut volumes, mut rotated, mut pivoted, mut scaled) = (0usize, 0usize, 0usize, 0usize);
    let mut mirrored = 0usize;
    // (actors, with hulls, with brush polygons) for blocking volumes, CSG
    // brushes and every other brush/volume.
    let mut table = [[0usize; 3]; 3];
    let mut outliers: Vec<f32> = Vec::new();
    for file in maps(&dir) {
        let (set, lp) = open(&dir, &file);
        let scene = scene_of(&set, &lp);
        for a in &scene.actors {
            let Some(v) = &a.volume else { continue };
            volumes += 1;
            rotated += usize::from(a.rotation != [0; 3]);
            pivoted += usize::from(a.pre_pivot != [0.0; 3]);
            scaled += usize::from(a.draw_scale != 1.0 || a.draw_scale3d != [1.0; 3]);
            let det = a.draw_scale * a.draw_scale3d.iter().product::<f32>();
            mirrored += usize::from(det < 0.0);
            let row = match a.kind {
                ActorKind::BlockingVolume => 0,
                ActorKind::Brush => 1,
                _ => 2,
            };
            table[row][0] += 1;
            table[row][1] += usize::from(!v.hulls.is_empty());
            table[row][2] += usize::from(v.polys.as_ref().is_some_and(|m| !m.positions.is_empty()));
            hulls += v.hulls.len();
            hull_tris += v.hulls.iter().filter(|h| !h.triangles.is_empty()).count();
            let mut hb: Option<([f32; 3], [f32; 3])> = None;
            for p in v.hulls.iter().flat_map(|h| h.vertices.iter()) {
                hb = Some(match hb {
                    None => (*p, *p),
                    Some((lo, hi)) => (
                        [lo[0].min(p[0]), lo[1].min(p[1]), lo[2].min(p[2])],
                        [hi[0].max(p[0]), hi[1].max(p[1]), hi[2].max(p[2])],
                    ),
                });
            }
            let pb = v.polys.as_ref().and_then(|m| m.bounds());
            if let (Some(h), Some(p)) = (hb, pb) {
                both += 1;
                let d = (0..3)
                    .map(|k| (h.0[k] - p.0[k]).abs().max((h.1[k] - p.1[k]).abs()))
                    .fold(0.0f32, f32::max);
                agree += usize::from(d < 1.0);
                if d >= 1.0 {
                    outliers.push(d);
                }
            }
        }
    }
    eprintln!("{hulls} hulls ({hull_tris} with triangles); {agree} of {both} agree");
    assert_eq!((both, agree), (198, 194));
    // The 4 that disagree are off by 9 UU to 21,000 UU.
    outliers.sort_by(f32::total_cmp);
    assert_eq!(outliers.len(), 4);
    assert!(outliers[0] > 8.0 && outliers[3] < 21_000.0, "{outliers:?}");
    // No brush or volume is rotated, pre-pivoted or mirrored; a few are
    // scaled.
    assert_eq!((volumes, rotated, pivoted, scaled), (1204, 0, 0, 6));
    assert_eq!(mirrored, 0);
    assert_eq!(hull_tris, hulls);
    assert_eq!(hulls, 1_323);
    // Blocking volumes keep only hulls, CSG brushes only polygons (all but
    // Freds_place's builder brush, whose Polys is empty), the rest both
    // (LEVEL_FORMAT.md "Brushes and volumes").
    assert_eq!(table, [[844, 844, 0], [162, 0, 161], [198, 198, 198]]);
}
