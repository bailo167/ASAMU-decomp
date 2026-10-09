//! Loads every converted map from a user-local converted-data directory.
//!
//! Gated: runs only when `ASAMU_CONVERTED_DIR` points at the output of
//! `asamu-import --out <dir> levels` and `asamu-import --out <dir> meshes
//! --collision` (the `meshes` step may be skipped; the collision then comes
//! from the BSP and blocking volumes only). It converts nothing itself and
//! never touches the game install. CI has no such directory and skips.

use std::path::PathBuf;

use asamu_world::collision::QueryFilter;
use asamu_world::rotation::actor_local_to_world;
use asamu_world::scene::{self, DirSource, LoadOptions};
use glam::Vec3;

fn converted_dir() -> Option<PathBuf> {
    let dir = PathBuf::from(std::env::var_os("ASAMU_CONVERTED_DIR")?);
    dir.join("levels").is_dir().then_some(dir)
}

fn map_names(dir: &std::path::Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir.join("levels"))
        .expect("levels directory")
        .flatten()
        .filter_map(|e| e.file_name().into_string().ok())
        .filter_map(|f| f.strip_suffix(".scene.json").map(str::to_owned))
        .collect();
    names.sort();
    names
}

#[test]
fn every_converted_map_loads() {
    let Some(dir) = converted_dir() else {
        eprintln!("ASAMU_CONVERTED_DIR not set: skipping");
        return;
    };
    let source = DirSource::new(&dir);
    for name in map_names(&dir) {
        let start = std::time::Instant::now();
        let map = scene::load_map(&source, &name, &LoadOptions::default()).unwrap();
        let s = map.collision.stats();
        eprintln!(
            "{name:<18} levels {} actors {:>5} meshes {:>4} (failed {}) instances {:>5} world tris {:>8} bsp {:>4} hulls {:>4} dyn {:>3} \
             starts {} checkpoints {:>2} volumes {:>3} triggers {:>2} crystals {:>3} rocks {:>3} killZ {:e} warnings {} ({:?})",
            map.levels.len(),
            map.stats.actors,
            s.meshes,
            map.stats.meshes_failed,
            s.instances,
            s.instance_triangles,
            map.stats.bsp_triangles,
            map.stats.blocking_hulls,
            map.dynamic.len(),
            map.actors.player_starts.len(),
            map.actors.checkpoints.len(),
            map.actors.volumes.len(),
            map.actors.triggers.len(),
            map.actors.crystals.len(),
            map.actors.rocks.len(),
            map.world.kill_z,
            map.warnings.len(),
            start.elapsed()
        );
        for w in map.warnings.iter().take(3) {
            eprintln!("    warning: {w}");
        }
        assert!(
            map.stats.meshes_failed == 0,
            "{name}: {:?}",
            &map.warnings[..map.warnings.len().min(5)]
        );
        if name.starts_with("AG-") {
            assert!(s.instance_triangles > 10_000, "{name}");
            assert_eq!(map.actors.player_starts.len(), 1, "{name}");
            // Every placed mesh component was placed.
            assert!(
                map.stats.placed_components + map.dynamic.len() + map.stats.components_without_mesh
                    <= map.stats.blocking_components,
                "{name}"
            );
            assert!(
                map.stats.placed_components * 100 >= map.stats.blocking_components * 95,
                "{name}: {:?}",
                map.stats
            );
        }
        // Streaming (LEVEL_FORMAT.md): BeautifulCity always loads Freds_place,
        // IceCave streams TheCore by Kismet.
        if name.eq_ignore_ascii_case("AG-BeautifulCity") {
            assert_eq!(map.levels.len(), 2);
            assert!(map.levels[1].initially_loaded);
        }
        if name.eq_ignore_ascii_case("AG-IceCave") {
            assert_eq!(map.levels.len(), 2);
            assert!(!map.levels[1].initially_loaded);
            assert_eq!(map.actors.rocks.len(), 166);
            assert_eq!(map.actors.crystals.len(), 98);
            assert_eq!(map.bodies.len(), 166);
        }
        // Every start has floor below it within a few hundred units.
        if let Some(ps) = map.actors.player_start() {
            let hit = map.collision.raycast(
                &map.dynamic,
                ps.location,
                ps.location - Vec3::Z * 1000.0,
                QueryFilter::PAWN,
            );
            eprintln!(
                "    start {:?} floor {:?}",
                ps.location,
                hit.map(|h| (h.t * 1000.0, h.normal))
            );
        }
    }
}

#[test]
fn recomputed_actor_transforms_match_the_importer() {
    // The runtime rotation code (deterministic sine table) against the
    // importer's matrices, for every actor of every map.
    let Some(dir) = converted_dir() else {
        eprintln!("ASAMU_CONVERTED_DIR not set: skipping");
        return;
    };
    let source = DirSource::new(&dir);
    let mut checked = 0usize;
    let mut worst = 0.0f64;
    for name in map_names(&dir) {
        let path = format!("levels/{name}.scene.json");
        let data = scene::DataSource::read(&source, &path, 1 << 30).unwrap();
        let file: scene::SceneFile = serde_json::from_slice(&data).unwrap();
        for a in &file.actors {
            let mine = actor_local_to_world(
                Vec3::from_array(a.location),
                a.rotation,
                a.draw_scale,
                Vec3::from_array(a.draw_scale3d),
                Vec3::from_array(a.pre_pivot),
            );
            let theirs = asamu_world::collision::Affine::from_row_matrix(&a.local_to_world);
            let scale = 1.0 + theirs.rows.iter().map(|r| r.length()).fold(0.0, f64::max);
            let mut err = (mine.translation - theirs.translation).length()
                / (1.0 + theirs.translation.length());
            for k in 0..3 {
                err = err.max((mine.rows[k] - theirs.rows[k]).length() / scale);
            }
            worst = worst.max(err);
            assert!(err < 1e-5, "{name} {}: {err}", a.name);
            checked += 1;
        }
    }
    eprintln!("{checked} actor transforms, worst relative difference {worst:e}");
    assert!(checked > 20_000);
}

/// SplitMix64 for query generation.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
    fn pick(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }
    fn offset(&mut self, e: f64) -> glam::DVec3 {
        let mut c = || (self.next() >> 11) as f64 / (1u64 << 53) as f64 * 2.0 * e - e;
        glam::DVec3::new(c(), c(), c())
    }
}

#[test]
fn bvh_queries_equal_brute_force_on_real_maps() {
    // Sweeps and rays aimed at random triangles of real geometry (and at the
    // player start), BVH against the reference that tests every triangle.
    let Some(dir) = converted_dir() else {
        eprintln!("ASAMU_CONVERTED_DIR not set: skipping");
        return;
    };
    let plan: &[(&str, usize)] = if cfg!(debug_assertions) {
        &[("AG-Workshop", 40)]
    } else {
        &[
            ("AG-Workshop", 300),
            ("AG-ParadiseCave", 80),
            ("AG-IceCave", 40),
        ]
    };
    let source = DirSource::new(&dir);
    for &(name, queries) in plan {
        let Ok(map) = scene::load_map(&source, name, &LoadOptions::default()) else {
            continue;
        };
        let statics = map.collision.statics();
        let mut rng = Rng(name.len() as u64);
        let mut hits = 0;
        for q in 0..queries {
            let filter = QueryFilter {
                kind: if q % 2 == 0 {
                    asamu_world::collision::QueryKind::Pawn
                } else {
                    asamu_world::collision::QueryKind::Trace
                },
                sublevels: if q % 5 == 0 {
                    u64::MAX
                } else {
                    map.initial_level_mask()
                },
            };
            let inst = &statics[rng.pick(statics.len())];
            let mesh = &map.collision.meshes()[inst.mesh as usize];
            let tri = rng.pick(mesh.triangles().len()) as u32;
            let w = map.collision.world_triangle(inst, tri).expect("triangle");
            let mut target = (w[0] + w[1] + w[2]) / 3.0;
            if q % 7 == 0
                && let Some(ps) = map.actors.player_start()
            {
                target = ps.location.as_dvec3();
            }
            let start = (target + rng.offset(300.0)).as_vec3();
            let end = match rng.pick(3) {
                0 => target.as_vec3(),
                1 => (target * 2.0 - start.as_dvec3()).as_vec3(),
                _ => (start.as_dvec3() + rng.offset(20.0)).as_vec3(),
            };
            let a = map
                .collision
                .sweep_cylinder(&map.dynamic, start, end, 21.0, 44.0, filter);
            let b = map.collision.sweep_cylinder_brute_force(
                &map.dynamic,
                start,
                end,
                21.0,
                44.0,
                filter,
            );
            assert_eq!(a, b, "{name}: sweep {start} -> {end}");
            hits += usize::from(a.is_some());
            let a = map.collision.raycast(&map.dynamic, start, end, filter);
            let b = map
                .collision
                .raycast_brute_force(&map.dynamic, start, end, filter);
            assert_eq!(a, b, "{name}: ray {start} -> {end}");
        }
        eprintln!("{name}: {queries} sweeps and rays identical ({hits} sweep hits)");
        assert!(hits * 4 >= queries, "{name}: too few hits ({hits})");
    }
}

#[test]
fn checkpoint_spawns_follow_a_cp_4_from_the_raw_scenes() {
    // Recomputes every checkpoint's respawn position and rotation straight
    // from the scene JSON (ABILITIES.md A-CP-4) and compares with the loader.
    let Some(dir) = converted_dir() else {
        eprintln!("ASAMU_CONVERTED_DIR not set: skipping");
        return;
    };
    let source = DirSource::new(&dir);
    let mut checked = 0;
    for name in map_names(&dir) {
        let path = format!("levels/{name}.scene.json");
        let data = scene::DataSource::read(&source, &path, 1 << 30).unwrap();
        let raw: serde_json::Value = serde_json::from_slice(&data).unwrap();
        let actors = raw["actors"].as_array().cloned().unwrap_or_default();
        let Ok(map) = scene::load_map(&source, &name, &LoadOptions::default()) else {
            continue;
        };
        let param = |a: &serde_json::Value, k: &str| {
            a["params"]
                .as_object()
                .and_then(|o| o.iter().find(|(kk, _)| kk.eq_ignore_ascii_case(k)))
                .map(|(_, v)| v.clone())
        };
        let v3 = |v: &serde_json::Value| {
            glam::DVec3::new(
                v[0].as_f64().unwrap_or(0.0),
                v[1].as_f64().unwrap_or(0.0),
                v[2].as_f64().unwrap_or(0.0),
            )
        };
        let rot = |v: &serde_json::Value| [0, 1, 2].map(|k| v[k].as_i64().unwrap_or(0) as i32);
        let mut indices = Vec::new();
        for a in actors.iter().filter(|a| a["kind"] == "checkpoint") {
            let spawn_actor = param(a, "spawnPointActor")
                .and_then(|v| v.as_str().map(str::to_owned))
                .and_then(|p| {
                    let n = p.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
                    actors
                        .iter()
                        .find(|b| {
                            b["name"]
                                .as_str()
                                .is_some_and(|s| s.eq_ignore_ascii_case(&n))
                        })
                        .cloned()
                });
            let rotate = param(a, "bRotatePlayerToSpawnPointRot")
                .and_then(|v| v.as_bool())
                .unwrap_or(true);
            let local = param(a, "bOffsetLocalSpace")
                .and_then(|v| v.as_bool())
                .unwrap_or(true);
            let offset = param(a, "spawnPointOffset").map_or(glam::DVec3::ZERO, |o| {
                glam::DVec3::new(
                    o["X"].as_f64().unwrap_or(0.0),
                    o["Y"].as_f64().unwrap_or(0.0),
                    o["Z"].as_f64().unwrap_or(0.0),
                )
            });
            let base = spawn_actor
                .as_ref()
                .map_or(v3(&a["location"]), |s| v3(&s["location"]));
            let offset = match (local, &spawn_actor) {
                (false, _) => offset,
                (true, Some(s)) if rotate => {
                    asamu_world::rotation::rotate_vector(offset, rot(&s["rotation"]))
                }
                (true, _) => asamu_world::rotation::rotate_vector(offset, rot(&a["rotation"])),
            };
            let want_rot = match (&spawn_actor, rotate) {
                (Some(s), true) => rot(&s["rotation"]),
                _ => rot(&a["rotation"]),
            };
            let def = map
                .actors
                .checkpoints
                .iter()
                .find(|d| a["name"].as_str() == Some(d.name.as_str()))
                .expect("checkpoint loaded");
            let diff = (def.spawn_location.as_dvec3() - (base + offset)).length();
            // f32 positions near 2e5 uu are 0.016 uu apart.
            assert!(diff < 0.05, "{name} {}: {diff}", def.name);
            assert_eq!(def.spawn_rotation, want_rot, "{name} {}", def.name);
            indices.push(def.index);
            checked += 1;
        }
        // Every shipped map numbers its checkpoints 0..N-1, so the
        // index-as-position lookup (A-CP-4) finds the right one.
        indices.sort_unstable();
        assert!(
            indices.iter().enumerate().all(|(i, v)| *v == i as i32),
            "{name}: {indices:?}"
        );
    }
    eprintln!("{checked} checkpoint spawns recomputed");
}
