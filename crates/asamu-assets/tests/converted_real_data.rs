//! Checks against user-local converted data (never committed).
//!
//! Set `ASAMU_CONVERTED_DIR` to an `asamu-import --out` directory (with at
//! least `levels/`; `meshes/`, `textures/` and `materials/` are used when
//! present). Without it the test is skipped, so CI (which has no game data)
//! passes.
//!
//! Besides structural invariants, every plan is compared with values
//! recomputed **independently** from the raw files (`serde_json::Value` and
//! the BSP binary read here, not through the crate's parsers): which
//! components are drawn, each draw's render placement of the mesh's local
//! origin and axes (row-vector UE3 matrix → `(y, z, −x)` axes at 50 UU per
//! render unit), mirroring, the player start the simulation's rule picks,
//! the light count, and the BSP triangle count and texture rule (1/128).
//!
//! ```sh
//! ASAMU_CONVERTED_DIR=research/local/render/converted \
//!   cargo test -p asamu-assets --test converted_real_data -- --nocapture
//! ```

use std::collections::HashMap;
use std::path::PathBuf;

use asamu_assets::{ConvertedDir, LevelPlan, Manifests, PlanOptions};
use asamu_core::glam::{Mat4, Vec3};
use serde_json::Value;

fn converted_dir() -> Option<ConvertedDir> {
    let dir = PathBuf::from(std::env::var_os("ASAMU_CONVERTED_DIR")?);
    dir.join("levels").is_dir().then(|| ConvertedDir::new(dir))
}

fn read_json(path: &std::path::Path) -> Value {
    let data = std::fs::read(path).expect("read");
    serde_json::from_slice(&data).expect("json")
}

fn f(v: &Value) -> f32 {
    v.as_f64().map_or(f32::NAN, |x| x as f32)
}

/// A raw row-vector matrix from the scene JSON.
fn raw_matrix(v: &Value) -> Option<[[f32; 4]; 4]> {
    let rows = v.as_array()?;
    let mut m = [[0.0f32; 4]; 4];
    for (i, row) in rows.iter().take(4).enumerate() {
        for (j, x) in row.as_array()?.iter().take(4).enumerate() {
            m[i][j] = f(x);
        }
    }
    Some(m)
}

/// `p · M` (UE3 row-vector convention), as written independently of the
/// crate's matrix conversion.
fn ue_point(m: &[[f32; 4]; 4], p: [f32; 3]) -> [f32; 3] {
    let mut out = [0.0f32; 3];
    for (j, o) in out.iter_mut().enumerate() {
        *o = p[0] * m[0][j] + p[1] * m[1][j] + p[2] * m[2][j] + m[3][j];
    }
    out
}

fn det3(m: &[[f32; 4]; 4]) -> f32 {
    m[0][0] * (m[1][1] * m[2][2] - m[1][2] * m[2][1])
        - m[0][1] * (m[1][0] * m[2][2] - m[1][2] * m[2][0])
        + m[0][2] * (m[1][0] * m[2][1] - m[1][1] * m[2][0])
}

/// UE3 → render: axes `(y, z, −x)`, 50 UU per render unit.
fn render(p: [f32; 3]) -> Vec3 {
    Vec3::new(p[1], p[2], -p[0]) / 50.0
}

fn flag(params: &Value, name: &str) -> bool {
    params.get(name).and_then(Value::as_bool).unwrap_or(true)
}

#[test]
fn every_converted_level_builds_a_plan() {
    let Some(dir) = converted_dir() else {
        eprintln!("skipped: ASAMU_CONVERTED_DIR is not set to a converted directory");
        return;
    };
    let levels = dir.list_levels();
    assert!(
        !levels.is_empty(),
        "no scene files in {:?}",
        dir.levels_dir()
    );
    let manifests = dir.load_manifests().expect("manifests");
    for level in levels {
        let plan = LevelPlan::load(&dir, &level, &PlanOptions::default()).expect("plan");
        let s = &plan.stats;
        eprintln!(
            "{level}: instances {} (drawn {}), draws {}, primitives {}, materials {} \
             (converted {}, fallback {}), textures {} (missing {}), missing meshes {}, \
             bad transforms {}, mirrored draws {}, max shear {:.2e}, bsp triangles {} ({} dropped, \
             {} meshes), lights {} (+{} skipped), \
             ambient {:.0}, player starts {}, atmosphere {:?}",
            s.instances,
            s.instances_drawn,
            s.draws,
            s.primitives,
            s.materials,
            s.materials_converted,
            s.materials_fallback,
            s.textures,
            s.textures_missing,
            s.missing_meshes.len(),
            s.bad_transforms,
            s.mirrored_draws,
            s.max_shear,
            s.bsp_triangles,
            s.bsp_dropped,
            plan.bsp.len(),
            s.lights_mapped,
            s.lights_skipped,
            plan.ambient,
            plan.scene.player_starts.len(),
            plan.scene.stats.atmosphere_actors,
        );
        assert_eq!(s.bsp_dropped, 0, "{level}");
        for b in &plan.bsp {
            assert!(b.material < plan.materials.len());
            assert_eq!(b.mesh.positions.len(), b.mesh.indices.len());
            assert!(b.mesh.uvs.iter().flatten().all(|c| c.is_finite()));
        }
        // Structural invariants.
        for d in &plan.draws {
            assert!(d.primitive < plan.primitives.len());
            assert!(d.material < plan.materials.len());
            assert!(d.transform.translation.is_finite());
            assert!(d.transform.scale.is_finite());
        }
        assert_eq!(s.bad_transforms, 0, "{level}");
        if manifests.meshes.is_some() {
            // With the level's meshes converted, every instance resolves.
            if s.missing_meshes.is_empty() {
                assert_eq!(s.instances, s.instances_drawn, "{level}");
            } else {
                eprintln!(
                    "  missing: {:?}",
                    s.missing_meshes.keys().take(10).collect::<Vec<_>>()
                );
            }
        }
        independent_checks(&dir, &level, &plan, &manifests);
    }
}

/// Recomputes plan values from the raw files (see the module docs).
fn independent_checks(dir: &ConvertedDir, level: &str, plan: &LevelPlan, manifests: &Manifests) {
    // glTF file → the manifest's glTF units per UU.
    let scales: HashMap<String, f32> = manifests
        .meshes
        .iter()
        .flat_map(|m| m.meshes.values())
        .flat_map(|e| e.lods.iter().map(move |l| (l.gltf.clone(), e.scale)))
        .collect();
    let scene_path = dir.scene_path(level).expect("scene path");
    let raw = read_json(&scene_path);
    let actors = raw["actors"].as_array().cloned().unwrap_or_default();

    // Drawn static mesh components of the persistent level, in order.
    let mut drawn = Vec::new();
    let mut lights = 0usize;
    for a in &actors {
        let hidden = a["hidden"].as_bool().unwrap_or(false);
        for c in a["components"].as_array().into_iter().flatten() {
            if !c["light"].is_null() {
                lights += 1;
            }
            if c["kind"] != "static_mesh" || hidden || c["hidden_game"].as_bool() == Some(true) {
                continue;
            }
            if c["static_mesh"].as_str().is_none_or(str::is_empty) {
                continue;
            }
            let Some(m) = raw_matrix(&c["local_to_world"]) else {
                continue;
            };
            if m.iter().flatten().all(|x| x.is_finite()) {
                drawn.push((a["slot"].as_u64().unwrap_or(u64::MAX), m));
            }
        }
    }
    // Index among the persistent level's instances, per plan instance.
    let mut own_index = Vec::with_capacity(plan.scene.meshes.len());
    let mut own = 0usize;
    for m in &plan.scene.meshes {
        own_index.push((m.level == 0).then_some(own));
        own += usize::from(m.level == 0);
    }
    assert_eq!(own, drawn.len(), "{level}: drawn components");
    if plan.scene.merged_levels.is_empty() {
        assert_eq!(lights, plan.scene.stats.lights, "{level}: lights");
    }

    // Each draw places the mesh's local origin and unit axes where the raw
    // matrix puts them (glTF local = (y, z, −x) · mesh scale of UE3 local).
    let mut checked = 0usize;
    let mut sheared = 0usize;
    let mut worst_sheared = 0.0f32;
    for d in &plan.draws {
        let inst = &plan.scene.meshes[d.instance];
        let Some(k) = own_index[d.instance] else {
            continue;
        };
        let (slot, m) = &drawn[k];
        assert_eq!(*slot, inst.actor_slot as u64, "{level}");
        assert_eq!(d.transform.mirrored, det3(m) < 0.0, "{level}: mirroring");
        let t = d.transform;
        let r = Mat4::from_scale_rotation_translation(t.scale, t.rotation, t.translation);
        let mesh_scale = scales
            .get(&plan.primitives[d.primitive].gltf)
            .copied()
            .unwrap_or(1.0);
        // The local origin, to a tolerance relative to its distance.
        let image = |local: [f32; 3]| {
            let gltf = Vec3::new(local[1], local[2], -local[0]) * mesh_scale;
            (r.transform_point3(gltf), render(ue_point(m, local)))
        };
        let (got0, want0) = image([0.0; 3]);
        let tol = 1e-5 * want0.abs().max_element() + 1e-4;
        assert!(
            (got0 - want0).abs().max_element() <= tol,
            "{level}: slot {slot} origin: {got0} vs {want0}"
        );
        // The local unit axes (as differences, relative to their length,
        // allowing for the `f32` rounding of far-away positions). A sheared
        // matrix cannot be expressed as translation, rotation and scale;
        // those draws are counted, not asserted.
        let rounding = 1e-6 * want0.abs().max_element();
        let mut worst = 0.0f32;
        for local in [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]] {
            let (got, want) = image(local);
            let (got, want) = (got - got0, want - want0);
            let err = ((got - want).length() - rounding).max(0.0);
            worst = worst.max(err / want.length().max(1e-6));
        }
        if t.shear < 1e-4 {
            assert!(worst < 1e-3, "{level}: slot {slot}: axis error {worst}");
        } else {
            sheared += 1;
            worst_sheared = worst_sheared.max(worst);
        }
        checked += 1;
    }
    eprintln!(
        "  independently checked {checked} draws ({sheared} sheared, worst axis error \
         {worst_sheared:.2e} relative)"
    );

    // The player start the simulation uses (first enabled primary, first
    // enabled, first).
    let starts: Vec<&Value> = actors
        .iter()
        .filter(|a| a["kind"] == "player_start")
        .collect();
    let chosen = starts
        .iter()
        .find(|a| flag(&a["params"], "bEnabled") && flag(&a["params"], "bPrimaryStart"))
        .or_else(|| starts.iter().find(|a| flag(&a["params"], "bEnabled")))
        .or_else(|| starts.first());
    match (chosen, plan.start()) {
        (Some(a), Some((loc, yaw, _))) => {
            let l = &a["location"];
            assert_eq!(loc, Vec3::new(f(&l[0]), f(&l[1]), f(&l[2])), "{level}");
            let units = a["rotation"][1].as_i64().unwrap_or(0) as f32;
            let expected = units * std::f32::consts::TAU / 65536.0;
            assert!((yaw - expected).abs() < 1e-4, "{level}: yaw");
        }
        (None, None) => {}
        (c, p) => panic!("{level}: start {:?} vs plan {p:?}", c.map(|a| &a["name"])),
    }

    // BSP: triangle count and the 1/128 texture rule on the first triangle.
    let Some(bsp_path) = dir.bsp_path(level) else {
        return;
    };
    if !plan.scene.merged_levels.is_empty() {
        return;
    }
    let bsp = read_json(&bsp_path);
    let visible = &bsp["meshes"]["visible"];
    let count = |k: &str| visible[k]["count"].as_u64().unwrap_or(0) as usize;
    assert_eq!(plan.stats.bsp_triangles, count("triangles"), "{level}");
    let Some(bin_name) = bsp["bin"].as_str() else {
        return;
    };
    let bin = std::fs::read(bsp_path.with_file_name(bin_name)).expect("bsp bin");
    if count("triangles") == 0 {
        return;
    }
    let off = |k: &str| visible[k]["offset"].as_u64().unwrap_or(0) as usize;
    let word = |o: usize| u32::from_le_bytes(bin[o..o + 4].try_into().expect("4 bytes"));
    let float = |o: usize| f32::from_bits(word(o));
    let surface = &bsp["surfaces"][word(off("surfaces")) as usize];
    let v3 = |v: &Value| [f(&v[0]), f(&v[1]), f(&v[2])];
    let (base, tu, tv) = (
        v3(&surface["base"]),
        v3(&surface["texture_u"]),
        v3(&surface["texture_v"]),
    );
    let index = word(off("triangles")) as usize;
    let p = [0, 4, 8].map(|k| float(off("positions") + index * 12 + k));
    let dot = |a: [f32; 3]| (0..3).map(|i| (p[i] - base[i]) * a[i]).sum::<f32>() / 128.0;
    let want_pos = render(p);
    let want_uv = [dot(tu), dot(tv)];
    let pos_tol = 1e-4 + 1e-6 * want_pos.abs().max_element();
    let found = plan.bsp.iter().any(|b| {
        b.mesh.positions.iter().zip(&b.mesh.uvs).any(|(q, uv)| {
            (Vec3::from_array(*q) - want_pos).abs().max_element() <= pos_tol
                && (uv[0] - want_uv[0]).abs() < 1e-3
                && (uv[1] - want_uv[1]).abs() < 1e-3
        })
    });
    assert!(
        found,
        "{level}: BSP vertex {p:?} with uv {want_uv:?} not in the plan"
    );
}
