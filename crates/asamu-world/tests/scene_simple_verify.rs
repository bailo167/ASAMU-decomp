//! Second look at the level's collision data (the verification pass of the
//! work that made converted levels collide the pawn at simple bodies).
//!
//! Gated like `scene_simple_real_data.rs`: `ASAMU_CONVERTED_DIR` must point
//! at the output of `asamu-import`; the checks against the recordings also
//! need `ASAMU_TRACE_RAW_DIR` (the raw recordings of the original,
//! `docs/TRACE_CAPTURE.md`). Nothing is converted and nothing is written;
//! CI has neither directory and skips.
//!
//! Where the expected values come from:
//!
//! - the counts per map were made with a separate script over the converted
//!   JSON (no code of this repository), by the rules written down at
//!   [`RECOUNT`];
//! - the four places are where `docs/PARITY_FINDINGS.md` V8 found the
//!   original's pawn standing inside a prop of our old collision; their
//!   coordinates are the recorded pawn location at the named tick;
//! - the gap under a standing pawn is the original's floor rule
//!   (NATIVE_PHYSICS.md 3.3 with the pull-back of P3).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use asamu_world::collision::{InstanceRef, PrimitiveKind, QueryFilter};
use asamu_world::scene::{self, DirSource, LoadOptions, LoadedMap, SceneFile};
use glam::Vec3;

/// The player's box.
const E: Vec3 = Vec3::new(21.0, 21.0, 44.0);
/// The box a move of the player sweeps (`scene_simple_real_data.rs`).
const MOVE_E: Vec3 = Vec3::new(21.021, 21.021, 44.0);

fn converted_dir() -> Option<PathBuf> {
    let Some(dir) = std::env::var_os("ASAMU_CONVERTED_DIR").map(PathBuf::from) else {
        eprintln!("ASAMU_CONVERTED_DIR not set: skipping");
        return None;
    };
    if !dir.join("levels").is_dir() {
        eprintln!("{} has no levels: skipping", dir.display());
        return None;
    }
    Some(dir)
}

/// `version` of the mesh manifest in `dir` (0 when there is none).
fn manifest_version(dir: &Path) -> u64 {
    std::fs::read(dir.join("meshes").join("manifest.json"))
        .ok()
        .and_then(|d| serde_json::from_slice::<serde_json::Value>(&d).ok())
        .and_then(|v| v.get("version").and_then(serde_json::Value::as_u64))
        .unwrap_or(0)
}

fn scene_file(dir: &Path, map: &str) -> Option<SceneFile> {
    let data = std::fs::read(dir.join("levels").join(format!("{map}.scene.json"))).ok()?;
    Some(serde_json::from_slice(&data).expect("scene file"))
}

fn load(dir: &Path, map: &str) -> LoadedMap {
    scene::load_map(&DirSource::new(dir), map, &LoadOptions::default()).expect("map loads")
}

/// A loaded map with the names of its actors by actor id.
type Loaded = (LoadedMap, BTreeMap<u32, String>);

/// Names of the actors of a loaded map by actor id.
fn actor_names(dir: &Path, map: &LoadedMap) -> BTreeMap<u32, String> {
    let mut names = BTreeMap::new();
    for (index, level) in map.levels.iter().enumerate() {
        let Some(file) = scene_file(dir, &level.name) else {
            continue;
        };
        for a in &file.actors {
            if let Some(id) = scene::actor_id(index as u8, a.slot) {
                names.insert(id, a.name.clone());
            }
        }
    }
    names
}

/// What the loader must count per map, sub-levels included, as
/// (pawn-blocking components met at a simple body, at nothing, at
/// triangles; simple-body shapes placed; components whose body is left out
/// because the placement is too small).
///
/// Recounted with a script of the verifier's own (Python over the scene
/// files, the mesh manifest and nothing else) by these rules:
///
/// - a static-mesh component blocks the pawn when the actor collides and
///   blocks and the component has `CollideActors`, `BlockActors` and
///   `BlockNonZeroExtent`. The actor's flags are its stored ones, except
///   that a stored `CollisionType` other than `COLLIDE_CustomDefault` sets
///   them and those of the collision component, and that both falling-rock
///   classes are `COLLIDE_BlockAll` (the loader's documented rule);
/// - its mesh is met at the simple body when the record's box switch is on
///   and the body has a convex element or a box, at nothing when the switch
///   is on without such a body, at the triangles when the switch is stored
///   `false`;
/// - a component whose matrix has rows whose lengths multiply to 1e-4 or
///   less is met at nothing where it would have been met at its body (the
///   original's static-mesh check, `scene::simple_body_checked`): 4 in
///   AG-ParadiseCave (two lanterns, two poles) and 3 in AG-IceCave (a rock
///   at scale 0.0116), with products of 1.5e-6 to 7.4e-5; the next smallest
///   product of a placement with a body is 2.9e-4;
/// - a shape is placed for every convex element and box of a component
///   whose pawn route or (when it blocks zero-extent traces) line route is
///   the body.
///
/// By the stored flags alone, and without the rule for small placements,
/// the same script gives the findings' V6 on all eight maps. The other
/// differences from V6 here are the sub-levels (Freds_place 6 / 52 / 0 in
/// AG-BeautifulCity, TheCore 44 / 98 / 0 in AG-IceCave), 166 falling rocks
/// (simple) and 98 recharge crystals (97 nothing, 1 simple) in AG-IceCave,
/// 15 glow flowers (nothing) in AG-Darkcave, and 7 crystals more and 2
/// sound-foliage actors fewer (nothing) in AG-StarHaven.
const RECOUNT: [(&str, [usize; 3], usize, usize); 8] = [
    ("AG-Workshop", [273, 755, 3], 1101, 0),
    ("AG-ParadiseCave", [1514, 226, 237], 17_615, 4),
    ("AG-BeautifulCity", [2060, 1132, 142], 7978, 0),
    ("AG-Darkcave", [1301, 815, 108], 12_188, 0),
    ("AG-StarHaven", [3112, 1833, 7], 29_331, 0),
    ("AG-IceCave", [4298, 556, 138], 20_441, 3),
    ("AG-Epilogue", [288, 516, 2], 1116, 0),
    ("TheCore", [44, 98, 0], 591, 0),
];

#[test]
fn the_loader_counts_what_an_independent_recount_counts() {
    let Some(dir) = converted_dir() else {
        return;
    };
    if manifest_version(&dir) < 2 {
        eprintln!("mesh manifest without simple collision: skipping");
        return;
    }
    for (map, expected, parts, small) in RECOUNT {
        if scene_file(&dir, map).is_none() {
            eprintln!("{map}: not converted, skipped");
            continue;
        }
        let loaded = load(&dir, map);
        let s = loaded.stats;
        eprintln!(
            "{map:<18} loader {:?} shapes {} too small {}; recount {expected:?} shapes {parts} too small {small}",
            [s.pawn_simple, s.pawn_none, s.pawn_triangles],
            s.simple_parts,
            s.pawn_scale_skipped
        );
        assert_eq!(
            [s.pawn_simple, s.pawn_none, s.pawn_triangles],
            expected,
            "{map}"
        );
        assert_eq!(s.simple_parts, parts, "{map}: shapes placed");
        assert_eq!(s.pawn_scale_skipped, small, "{map}: too small for a body");
        assert_eq!(s.pawn_approximate, 0, "{map}");
        assert_eq!(
            s.pawn_components,
            expected.iter().sum::<usize>(),
            "{map}: every pawn-blocking component is in one class"
        );
    }
}

/// A place where the original's pawn stood still in a recording.
struct Stand {
    /// Segment and tick of the canonical trace (raw record = segment base +
    /// tick, bases in the findings), and what V8 found in the pawn there.
    tick: &'static str,
    map: &'static str,
    /// The recorded pawn location.
    at: [f32; 3],
    /// The recorded base actor.
    base: &'static str,
    /// What the base actor's mesh sends the pawn to (its record in the mesh
    /// manifest, read with the verifier's script).
    kind: PrimitiveKind,
    /// The gap the original's floor rule leaves under the pawn's box there:
    /// lowest and highest.
    gap: (f64, f64),
}

/// The walking floor check keeps what it measures between 1.9 and 2.4 uu
/// (NATIVE_PHYSICS.md 3.3), and it measures the true gap less the pull-back
/// of the hit (P3): 1.0 on a simple body, 0.1 × 28 = 2.8 on triangles (the
/// check is 28 uu long and the cap for triangles is 4.0).
const SIMPLE_BAND: (f64, f64) = (1.9 + 1.0, 2.4 + 1.0);
const TRIANGLE_BAND: (f64, f64) = (1.9 + 2.8, 2.4 + 2.8);

/// `docs/PARITY_FINDINGS.md` V8: the four starts at which our pawn could not
/// move, because the original's pawn stands inside the triangles of a prop
/// that has no simple body, which the original's pawn does not collide with.
///
/// The second one is the rest after the landing at the start of AG-Darkcave:
/// the pawn has not moved since. The recording has it falling at 271.682
/// uu/s one record earlier and landing in a frame of 0.016616 s, so the
/// fall's step was (271.682 + 1040 × 0.016616 / 2) × 0.016616 = 4.658 uu
/// (gravity −520, doubled: findings section 3). The hit is reported 0.1 of
/// the step before the contact, and the first floor check lifts the pawn by
/// the 2.15 of the walking hover: 0.4658 + 2.15 = 2.6158 above the contact.
/// One step of an `f32` at that height is 0.0005; the recorded height and
/// our contact carry one each, and one more is allowed for the step.
const STUCK_STARTS: [Stand; 4] = [
    Stand {
        tick: "DC1 seg3 tick 742 (rockpile_01)",
        map: "AG-BeautifulCity",
        at: [-58475.074, 44980.46, 53982.2],
        base: "StaticMeshActor_99",
        // Village_Cave.Village_Entrance_Cave: a body of 114 convex elements.
        kind: PrimitiveKind::SimpleHull,
        gap: SIMPLE_BAND,
    },
    Stand {
        tick: "PLAY2 seg6 tick 95 (redrock_04)",
        map: "AG-Darkcave",
        // 93182.59375 as recorded.
        at: [93_182.59, 2750.8962, 4482.1963],
        base: "StaticMeshActor_5",
        // zeth_darkcave.darkcavepart1: box switch stored false.
        kind: PrimitiveKind::TriangleMesh,
        gap: (2.6158 - 0.0015, 2.6158 + 0.0015),
    },
    Stand {
        tick: "PLAY2 seg1 tick 3950 (candle_1)",
        map: "AG-BeautifulCity",
        at: [-34271.137, 18138.502, 50599.668],
        base: "StaticMeshActor_88",
        // Village_Shacks.modular_Foundation_1: a body of one convex element.
        kind: PrimitiveKind::SimpleHull,
        gap: SIMPLE_BAND,
    },
    Stand {
        tick: "PLAY2 seg0 tick 1868 (six props)",
        map: "AG-BeautifulCity",
        at: [-51311.96, 25011.02, 45778.13],
        base: "StaticMeshActor_721",
        // TexturingBCityPlatform.CityPillarRevampCake: box switch stored false.
        kind: PrimitiveKind::TriangleMesh,
        gap: TRIANGLE_BAND,
    },
];

/// At each of the four starts the pawn's box, where the original's pawn
/// stood, overlaps nothing that blocks the pawn; it does overlap what blocks
/// bullets (the triangles of every mesh, which is what the pawn collided
/// with before); and the floor under it is the recorded base actor, of the
/// kind its mesh's switch says, at the gap the original's floor rule leaves.
#[test]
fn the_four_stuck_starts_are_free_and_stand_where_the_rule_says() {
    let Some(dir) = converted_dir() else {
        return;
    };
    if manifest_version(&dir) < 2 {
        eprintln!("mesh manifest without simple collision: skipping");
        return;
    }
    let mut maps: BTreeMap<&str, Loaded> = BTreeMap::new();
    for stand in &STUCK_STARTS {
        if scene_file(&dir, stand.map).is_none() {
            eprintln!("{}: not converted, skipped", stand.map);
            continue;
        }
        let (map, names) = maps.entry(stand.map).or_insert_with(|| {
            let m = load(&dir, stand.map);
            let n = actor_names(&dir, &m);
            (m, n)
        });
        let p = Vec3::from_array(stand.at);
        let mask = map.initial_level_mask();
        let pawn = QueryFilter {
            sublevels: mask,
            ..QueryFilter::PAWN
        };
        let bullets = QueryFilter {
            sublevels: mask,
            ..QueryFilter::TRACE
        };
        for e in [E, MOVE_E] {
            assert!(
                !map.collision.overlaps_box(&map.dynamic, p, e, pawn),
                "{}: the pawn's box overlaps pawn collision",
                stand.tick
            );
        }
        assert!(
            map.collision.overlaps_box(&map.dynamic, p, E, bullets),
            "{}: nothing of the old collision is in the box",
            stand.tick
        );
        // The floor check's reach is 28 uu.
        let down = p - Vec3::Z * 28.0;
        let hit = map
            .collision
            .sweep_box(&map.dynamic, p, down, E, pawn)
            .unwrap_or_else(|| panic!("{}: no floor within 28 uu", stand.tick));
        assert_eq!(
            hit,
            map.collision
                .sweep_box_brute_force(&map.dynamic, p, down, E, pawn)
                .expect("the reference finds it too"),
            "{}",
            stand.tick
        );
        let inst = match hit.instance {
            InstanceRef::Static(i) => map.collision.statics().get(i as usize),
            InstanceRef::Dynamic(i) => map.dynamic.get(i as usize),
        };
        let name = inst
            .and_then(|i| i.info.actor)
            .and_then(|a| names.get(&a).cloned())
            .unwrap_or_default();
        let gap = hit.t * 28.0;
        eprintln!(
            "{:<36} free; floor {:?} of {name}, gap {gap:.4} (rule {:.4} to {:.4}), normal {}",
            stand.tick, hit.kind, stand.gap.0, stand.gap.1, hit.normal
        );
        assert!(!hit.penetrating, "{}", stand.tick);
        assert_eq!(name, stand.base, "{}", stand.tick);
        assert_eq!(hit.kind, stand.kind, "{}", stand.tick);
        assert!(
            (stand.gap.0..=stand.gap.1).contains(&gap),
            "{}: gap {gap}",
            stand.tick
        );
    }
}

/// The records of one raw recording as (map, pawn location, physics mode).
fn recording(dir: &Path, scenario: &str) -> Option<Vec<(String, Vec3, i64)>> {
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
        let physics = v["player"]["physics"].as_i64().unwrap_or(-1);
        out.push((map, Vec3::new(c(0), c(1), c(2)), physics));
    }
    Some(out)
}

/// Every recorded position of the original's pawn against the new world: a
/// measurement, printed, with no threshold. The original's pawn is never
/// inside what it collides with, so a recorded position at which the pawn's
/// box overlaps our pawn collision is a place where our collision data (or
/// the state of the level: movers, falling rocks and switched volumes are
/// where they start) is not the original's. The same count against what
/// blocks bullets (the triangles of every mesh) is what the pawn collided
/// with before.
///
/// For every record at which the pawn walks and is free, the gap under its
/// box is measured too and compared with what the original's floor rule
/// leaves ([`SIMPLE_BAND`], [`TRIANGLE_BAND`]); records outside the band are
/// rests after a landing (2.15 plus a tenth of the fall's step until the
/// pawn moves), moving platforms and whatever is left of P11 and P12.
///
/// Asserted: at the records of the four starts the box is free, and in
/// AG-Workshop every walking record stands in the band.
#[test]
fn recorded_positions_against_the_new_world() {
    let Some(dir) = converted_dir() else {
        return;
    };
    if manifest_version(&dir) < 2 {
        eprintln!("mesh manifest without simple collision: skipping");
        return;
    }
    let Some(raw) = std::env::var_os("ASAMU_TRACE_RAW_DIR").map(PathBuf::from) else {
        eprintln!("ASAMU_TRACE_RAW_DIR not set: skipping");
        return;
    };
    // (recording, raw record) of the four starts.
    let starts = [
        ("DC1", 8194 + 742),
        ("PLAY2", 25_512 + 95),
        ("PLAY2", 3138 + 3950),
        ("PLAY2", 1868),
    ];
    let mut maps: BTreeMap<String, Option<Loaded>> = BTreeMap::new();
    for scenario in ["WS1", "DC1", "PLAY2"] {
        let Some(records) = recording(&raw, scenario) else {
            eprintln!("{scenario}: no recording, skipped");
            continue;
        };
        // Per map: records, overlapping pawn collision, overlapping bullet
        // geometry; and who is overlapped.
        let mut per_map: BTreeMap<String, [usize; 3]> = BTreeMap::new();
        // (map, kind, actor) → records, first record, last record.
        let mut blockers: BTreeMap<(String, String, String), (usize, usize, usize)> =
            BTreeMap::new();
        let mut floors: BTreeMap<(String, String), Vec<f64>> = BTreeMap::new();
        let mut no_floor: BTreeMap<String, usize> = BTreeMap::new();
        for (index, (name, p, physics)) in records.iter().enumerate() {
            let entry = maps.entry(name.clone()).or_insert_with(|| {
                scene_file(&dir, name)?;
                let m = load(&dir, name);
                let n = actor_names(&dir, &m);
                Some((m, n))
            });
            let Some((map, names)) = entry.as_mut() else {
                continue;
            };
            if !p.is_finite() {
                continue;
            }
            let mask = map.initial_level_mask();
            let pawn = QueryFilter {
                sublevels: mask,
                ..QueryFilter::PAWN
            };
            let bullets = QueryFilter {
                sublevels: mask,
                ..QueryFilter::TRACE
            };
            let counts = per_map.entry(name.clone()).or_default();
            counts[0] += 1;
            let inside = map.collision.overlaps_box(&map.dynamic, *p, E, pawn);
            if inside {
                counts[1] += 1;
                // Who: switch off every instance whose bounds reach the box,
                // then each one on alone.
                let near: Vec<u32> = map
                    .collision
                    .statics()
                    .iter()
                    .enumerate()
                    .filter(|(_, i)| {
                        i.enabled
                            && i.info.blocks_pawn
                            && !(p.cmplt(i.bounds.min - E).any() || p.cmpgt(i.bounds.max + E).any())
                    })
                    .map(|(k, _)| k as u32)
                    .collect();
                for &k in &near {
                    map.collision.set_static_enabled(k, false);
                }
                let mut who_here: std::collections::BTreeSet<(String, String)> =
                    std::collections::BTreeSet::new();
                for &k in &near {
                    map.collision.set_static_enabled(k, true);
                    if map.collision.overlaps_box(&map.dynamic, *p, E, pawn) {
                        let inst = &map.collision.statics()[k as usize];
                        let who = inst
                            .info
                            .actor
                            .and_then(|a| names.get(&a).cloned())
                            .unwrap_or_else(|| "(level geometry)".to_owned());
                        who_here.insert((format!("{:?}", inst.kind), who));
                    }
                    map.collision.set_static_enabled(k, false);
                }
                for &k in &near {
                    map.collision.set_static_enabled(k, true);
                }
                let found = !who_here.is_empty();
                for (kind, who) in who_here {
                    let e = blockers
                        .entry((name.clone(), kind, who))
                        .or_insert((0usize, index, index));
                    e.0 += 1;
                    e.2 = index;
                }
                if !found {
                    let e = blockers
                        .entry((
                            name.clone(),
                            "Dynamic".to_owned(),
                            "(a moving actor)".to_owned(),
                        ))
                        .or_insert((0usize, index, index));
                    e.0 += 1;
                    e.2 = index;
                }
            }
            if map.collision.overlaps_box(&map.dynamic, *p, E, bullets) {
                counts[2] += 1;
            }
            // A walking pawn (physics mode 1) that is free: the gap under
            // its box, by the kind of the floor.
            if *physics == 1 && !inside {
                let down = *p - Vec3::Z * 28.0;
                let kind = match map.collision.sweep_box(&map.dynamic, *p, down, E, pawn) {
                    Some(h) => {
                        let g = floors
                            .entry((name.clone(), format!("{:?}", h.kind)))
                            .or_default();
                        g.push(h.t * 28.0);
                        Some(h.kind)
                    }
                    None => None,
                };
                if kind.is_none() {
                    *no_floor.entry(name.clone()).or_default() += 1;
                }
            }
            if starts.contains(&(scenario, index)) {
                assert!(!inside, "{scenario} record {index}: the start is not free");
            }
        }
        for (name, [n, inside, old]) in &per_map {
            eprintln!(
                "{scenario:<6} {name:<18} records {n:>6}: box overlaps pawn collision on {inside:>5}, \
                 the triangles of every mesh on {old:>5}"
            );
        }
        for ((name, kind), gaps) in &mut floors {
            gaps.sort_by(f64::total_cmp);
            let band = if kind == "TriangleMesh" {
                TRIANGLE_BAND
            } else {
                SIMPLE_BAND
            };
            let within = |slack: f64| {
                gaps.iter()
                    .filter(|g| (band.0 - slack..=band.1 + slack).contains(*g))
                    .count()
            };
            let q = |f: f64| gaps[((gaps.len() - 1) as f64 * f) as usize];
            // The Workshop is flat floor and one staircase (a ramp in its
            // simple body), walked in story mode without a jump: every
            // walking record must be in the band.
            if name.eq_ignore_ascii_case("AG-Workshop") {
                assert_eq!(within(0.0), gaps.len(), "{name}: {kind}");
            }
            eprintln!(
                "        walking on {kind:<12} in {name:<18}: {:>6} records, gap in the rule's band \
                 {:.1} to {:.1} on {:>6}, within 0.5 of it on {:>6}; gap 5% {:.3}, median {:.3}, 95% {:.3}",
                gaps.len(),
                band.0,
                band.1,
                within(0.0),
                within(0.5),
                q(0.05),
                q(0.5),
                q(0.95)
            );
        }
        for (name, n) in &no_floor {
            eprintln!("        walking with no floor within 28 uu in {name}: {n} records");
        }
        let mut top: Vec<_> = blockers.into_iter().collect();
        top.sort_by(|a, b| b.1.0.cmp(&a.1.0).then(a.0.cmp(&b.0)));
        for ((name, kind, who), (n, first, last)) in &top {
            eprintln!(
                "        overlapped: {n:>5} records ({first} to {last}), {name} {kind} {who}"
            );
        }
    }
}

/// `docs/PARITY_FINDINGS.md` V14 per map (all, starting disabled).
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

/// A conversion from before the simple collision (mesh manifest version 1)
/// still loads every map: the pawn collides with the triangles of every
/// blocking mesh as it did, one warning says that this is approximate and
/// how to mend it, and nothing is counted as met at a simple body. The
/// volumes' switch does not need the mesh data and is applied all the same.
///
/// Runs when `ASAMU_CONVERTED_DIR` points at such a conversion.
#[test]
fn a_conversion_without_simple_collision_loads_and_says_so() {
    let Some(dir) = converted_dir() else {
        return;
    };
    let version = manifest_version(&dir);
    if version != 1 {
        eprintln!("mesh manifest version {version}: not a version 1 conversion, skipping");
        return;
    }
    for (map, _, _) in V14 {
        if scene_file(&dir, map).is_none() {
            eprintln!("{map}: not converted, skipped");
            continue;
        }
        let loaded = load(&dir, map);
        let s = loaded.stats;
        let notices: Vec<&String> = loaded
            .warnings
            .iter()
            .filter(|w| w.contains("simple-collision"))
            .collect();
        eprintln!(
            "{map:<18} pawn-blocking components {} (approximate {}), simple {}, none {}, triangles {}, \
             shapes {}, volumes with a switch {} (off {}); notice: {:?}",
            s.pawn_components,
            s.pawn_approximate,
            s.pawn_simple,
            s.pawn_none,
            s.pawn_triangles,
            s.simple_parts,
            s.switchable_volumes,
            s.disabled_volumes,
            notices
        );
        assert_eq!(notices.len(), 1, "{map}: {:?}", loaded.warnings);
        assert!(
            notices[0].contains("version 1")
                && notices[0].contains("approximate")
                && notices[0].contains("asamu-import"),
            "{map}: {}",
            notices[0]
        );
        assert!(s.pawn_components > 0, "{map}");
        assert_eq!(s.pawn_approximate, s.pawn_components, "{map}");
        assert_eq!(
            (s.pawn_simple, s.pawn_none, s.pawn_triangles, s.simple_parts),
            (0, 0, 0, 0),
            "{map}"
        );
        // Every pawn-blocking instance is a mesh's triangles, BSP or a
        // volume, as before.
        assert!(
            loaded
                .collision
                .statics()
                .iter()
                .chain(&loaded.dynamic)
                .all(|i| i.kind != PrimitiveKind::SimpleHull),
            "{map}"
        );
    }
    // The switch of the volumes, per map as in V14 (sub-levels are loaded
    // with their map: TheCore's one is counted with AG-IceCave too).
    for (map, all, off) in V14 {
        if scene_file(&dir, map).is_none() {
            continue;
        }
        let loaded = load(&dir, map);
        let extra = usize::from(map == "AG-IceCave");
        assert_eq!(
            (
                loaded.stats.switchable_volumes,
                loaded.stats.disabled_volumes
            ),
            (all + extra, off + extra),
            "{map}"
        );
    }
}
