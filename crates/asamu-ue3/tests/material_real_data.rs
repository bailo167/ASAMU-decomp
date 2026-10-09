//! Material decoding and approximation against the user's own installed game
//! (read-only).
//!
//! Reads `ASAMU_ORIGINAL_DIR` (the folder containing
//! `A Story About My Uncle.app`) or the default macOS Steam location and
//! skips cleanly when the data is absent. Only counts and structural facts are
//! asserted; nothing is copied or written.
//!
//! Acceptance test for `docs/reverse-engineering/MATERIALS.md`: `(T)` claims
//! there are asserted here. Run with `-- --nocapture` for the tables.

#![allow(clippy::unwrap_used)]

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use asamu_ue3::level::{self, SceneOptions};
use asamu_ue3::material::{
    ApproxStatus, MaterialClass, MaterialCoverage, MaterialDecoder, count_guid_occurrences,
    scan_package,
};
use asamu_ue3::model::{LoadedPackage, PackageSet};
use asamu_ue3::staticmesh::{decode_static_mesh, is_static_mesh};
use asamu_ue3::types::Guid;

const COOKED: &str = "A Story About My Uncle.app/Contents/Resources/ASAMU/CookedMac";

fn cooked_dir() -> Option<PathBuf> {
    let root = match std::env::var_os("ASAMU_ORIGINAL_DIR") {
        Some(r) => PathBuf::from(r),
        None => PathBuf::from(std::env::var_os("HOME")?)
            .join("Library/Application Support/Steam/steamapps/common/A Story About My Uncle"),
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

fn is_shader_cache(p: &Path) -> bool {
    p.file_name().is_some_and(|n| {
        n.to_string_lossy()
            .to_ascii_lowercase()
            .contains("shadercache")
    })
}

fn packages(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for d in [dir.to_path_buf(), dir.join("Maps")] {
        let Ok(rd) = std::fs::read_dir(&d) else {
            continue;
        };
        let mut v: Vec<PathBuf> = rd
            .flatten()
            .map(|e| e.path())
            .filter(|p| {
                p.is_file()
                    && !is_shader_cache(p)
                    && p.extension()
                        .map(|x| x.to_string_lossy().to_ascii_lowercase())
                        .is_some_and(|x| matches!(x.as_str(), "u" | "upk" | "asamu"))
            })
            .collect();
        v.sort();
        out.extend(v);
    }
    out
}

/// Visit every package with a fresh set (keeps memory bounded: maps are large).
fn for_each_package(dir: &Path, mut f: impl FnMut(&PackageSet, &Arc<LoadedPackage>)) {
    for path in packages(dir) {
        let set = PackageSet::new(&[dir.to_path_buf(), dir.join("Maps")]);
        let lp = set.open_file(&path).unwrap();
        f(&set, &lp);
    }
}

/// `Engine.EngineTypes.EBlendMode` in enum order (read from `Engine.u`).
const BLEND_MODES: [&str; 9] = [
    "BLEND_Opaque",
    "BLEND_Masked",
    "BLEND_Translucent",
    "BLEND_Additive",
    "BLEND_Modulate",
    "BLEND_ModulateAndAdd",
    "BLEND_SoftMasked",
    "BLEND_AlphaComposite",
    "BLEND_DitheredTranslucent",
];

fn bump<K: Ord>(m: &mut BTreeMap<K, usize>, k: K) {
    *m.entry(k).or_insert(0) += 1;
}

fn class_chain_has(set: &PackageSet, class: &str, base: &str) -> bool {
    class.eq_ignore_ascii_case(base)
        || set
            .super_chain(class)
            .iter()
            .any(|s| s.eq_ignore_ascii_case(base))
}

fn export_class(lp: &LoadedPackage, i: usize) -> String {
    asamu_ue3::object::export_class_path(&lp.package, Some(&lp.name), i).unwrap_or_default()
}

#[test]
fn every_material_decodes_exactly_and_round_trips() {
    let dir = require_data!();
    let mut covs: Vec<MaterialCoverage> = Vec::new();
    for_each_package(&dir, |set, lp| {
        let dec = MaterialDecoder::new(set);
        let c = scan_package(&dec, lp, &mut |_, _| {});
        if !c.classes.is_empty() || !c.expression_exports.is_empty() {
            covs.push(c);
        }
    });
    let mut classes: BTreeMap<String, (usize, usize, usize, usize)> = BTreeMap::new();
    let (mut exprs, mut exact, mut fails, mut native, mut perm) = (0, 0, 0, 0u64, 0);
    let mut masks = BTreeMap::new();
    let mut compile_errors = 0;
    let mut kinds: BTreeMap<String, usize> = BTreeMap::new();
    for c in &covs {
        eprintln!(
            "{:20} {:?} expressions {} exact {} native {}",
            c.package,
            c.classes
                .iter()
                .map(|(k, v)| (k.as_str(), v.total, v.decoded, v.round_trip))
                .collect::<Vec<_>>(),
            c.expression_exports.values().sum::<usize>(),
            c.expression_exact,
            c.native_bytes
        );
        for f in &c.failures {
            eprintln!("  FAIL {f}");
        }
        fails += c.failures.len() + c.expression_failures;
        for (k, v) in &c.classes {
            let e = classes.entry(k.clone()).or_default();
            e.0 += v.total;
            e.1 += v.default_objects;
            e.2 += v.decoded;
            e.3 += v.round_trip;
        }
        for (k, v) in &c.expression_exports {
            *kinds.entry(k.clone()).or_insert(0) += v;
        }
        exprs += c.expression_exports.values().sum::<usize>();
        exact += c.expression_exact;
        native += c.native_bytes;
        perm += c.static_permutation_instances;
        compile_errors += c.resources_with_compile_errors;
        for (k, v) in &c.quality_masks {
            *masks.entry(*k).or_insert(0) += v;
        }
    }
    eprintln!(
        "TOTAL {classes:?} expressions {exprs} exact {exact} native {native} masks {masks:?} \
         static-permutation instances {perm} kinds {}",
        kinds.len()
    );
    assert_eq!(fails, 0);
    let want = [
        ("DecalMaterial", 67, 1),
        ("Material", 1163, 1),
        ("MaterialFunction", 23, 1),
        ("MaterialInstanceConstant", 478, 1),
        ("MaterialInstanceTimeVarying", 6, 1),
        ("OtherInstance", 2, 2),
        ("OtherMaterial", 1, 1),
    ];
    for (k, total, cdos) in want {
        let got = classes.get(k).copied().unwrap_or_default();
        assert_eq!(got, (total, cdos, total, total), "{k}");
    }
    assert_eq!(classes.len(), want.len());
    assert_eq!(exprs, 17_449);
    assert_eq!(
        exact, exprs,
        "expression subobjects carry only tagged properties"
    );
    assert_eq!(native, 315_024);
    // Every material resource is the high-quality one alone.
    assert_eq!(masks, BTreeMap::from([(1u32, 1408usize)]));
    assert_eq!(compile_errors, 0);
    assert_eq!(perm, 180);
}

#[test]
fn native_resource_fields() {
    let dir = require_data!();
    let mut u32s: BTreeMap<[u32; 3], usize> = BTreeMap::new();
    let mut legacy: BTreeMap<u32, usize> = BTreeMap::new();
    let mut tex_coords: BTreeMap<u32, usize> = BTreeMap::new();
    let mut flags: BTreeMap<&'static str, usize> = BTreeMap::new();
    let mut transforms: BTreeMap<u32, usize> = BTreeMap::new();
    let (mut uniform, mut uniform_textures, mut lookups, mut lookup_in_range) = (0, 0, 0, 0);
    let (mut deps, mut deps_expr, mut dep_max_ok) = (0, 0, 0);
    let mut resources = 0;
    let (mut switches, mut cmasks, mut normals, mut terrain, mut overridden) = (0, 0, 0, 0, 0);
    let (mut sets_equal_bases, mut base_ok, mut base_bad) = (0, 0, 0);
    let mut lookup_scale: BTreeMap<String, usize> = BTreeMap::new();
    // (matching, total) for materials and for static-permutation instances.
    let mut blend_match = [(0usize, 0usize); 2];
    for_each_package(&dir, |set, lp| {
        let dec = MaterialDecoder::new(set);
        for i in 0..lp.package.exports.len() {
            let Some((_, class)) = dec.export_class(lp, i) else {
                continue;
            };
            if class == MaterialClass::MaterialFunction {
                continue;
            }
            let m = dec.decode(lp, i).unwrap();
            for q in &m.native.resources {
                resources += 1;
                let r = &q.resource;
                assert_eq!(q.quality, 0);
                bump(&mut u32s, r.resource_u32);
                // Compare with the (base) material's BlendMode and bIsMasked.
                let base_props = if m.class.is_instance() {
                    dec.resolve_chain(lp, i)
                        .base()
                        .map(|(_, b)| b.properties.clone())
                } else {
                    Some(m.properties.clone())
                };
                if let Some(bp) = base_props {
                    let blend = asamu_ue3::material::prop(&bp, "BlendMode")
                        .and_then(|v| match v {
                            asamu_ue3::Value::Enum(n) => {
                                BLEND_MODES.iter().position(|b| b.eq_ignore_ascii_case(n))
                            }
                            _ => None,
                        })
                        .unwrap_or(0);
                    let masked = matches!(
                        asamu_ue3::material::prop(&bp, "bIsMasked"),
                        Some(asamu_ue3::Value::Bool(true))
                    );
                    let ok =
                        r.resource_u32 == [u32::try_from(blend).unwrap(), 0, u32::from(masked)];
                    let slot = if m.class.is_instance() { 1 } else { 0 };
                    blend_match[slot].1 += 1;
                    if ok {
                        blend_match[slot].0 += 1;
                    }
                }
                bump(&mut legacy, r.legacy_u32);
                bump(&mut tex_coords, r.num_user_tex_coords);
                bump(&mut transforms, r.using_transforms);
                for (name, on) in [
                    ("scene_color", r.uses_scene_color),
                    ("scene_depth", r.uses_scene_depth),
                    ("dynamic_parameter", r.uses_dynamic_parameter),
                    ("lightmap_uvs", r.uses_lightmap_uvs),
                    ("vertex_position_offset", r.uses_vertex_position_offset),
                ] {
                    if on {
                        bump(&mut flags, name);
                    }
                }
                uniform += r.uniform_expression_textures.len();
                for t in &r.uniform_expression_textures {
                    let class = match t.export_index() {
                        Some(e) => export_class(lp, e),
                        None => t
                            .import_index()
                            .and_then(|ii| lp.package.import(ii).ok())
                            .map(|imp| lp.package.fname(imp.class_name))
                            .map(|c| format!("Engine.{c}"))
                            .unwrap_or_default(),
                    };
                    if class_chain_has(set, &class, "Engine.Texture") {
                        uniform_textures += 1;
                    }
                }
                for l in &r.texture_lookups {
                    lookups += 1;
                    if usize::try_from(l.texture_index)
                        .is_ok_and(|ti| ti < r.uniform_expression_textures.len())
                    {
                        lookup_in_range += 1;
                    }
                    bump(&mut lookup_scale, format!("{}x{}", l.u_scale, l.v_scale));
                }
                deps += r.texture_dependency_lengths.len();
                let mut max = 0;
                for d in &r.texture_dependency_lengths {
                    max = max.max(d.length);
                    if d.expression.export_index().is_some_and(|e| {
                        asamu_ue3::material::expression_kind(&export_class(lp, e)).is_some()
                    }) {
                        deps_expr += 1;
                    }
                }
                if r.texture_dependency_lengths.is_empty() || max == r.max_texture_dependency_length
                {
                    dep_max_ok += 1;
                }
                if let Some(s) = &q.static_parameters {
                    switches += s.static_switches.len();
                    cmasks += s.component_masks.len();
                    normals += s.normal_parameters.len();
                    terrain += s.terrain_layer_weights.len();
                    overridden += s.static_switches.iter().filter(|p| p.overridden).count()
                        + s.component_masks.iter().filter(|p| p.overridden).count();
                    let chain = dec.resolve_chain(lp, i);
                    match chain.base() {
                        Some((_, b)) => {
                            let base_id = b.native.resources.first().map(|x| x.resource.id);
                            if base_id == Some(s.base_material_id) {
                                base_ok += 1;
                            } else {
                                base_bad += 1;
                            }
                            if base_id == Some(r.id) {
                                sets_equal_bases += 1;
                            }
                        }
                        None => base_bad += 1,
                    }
                }
            }
        }
    });
    eprintln!("resources {resources}");
    eprintln!("resource u32 triples {u32s:?}");
    eprintln!(
        "legacy u32 {legacy:?}; resource u32 == [BlendMode, 0, bIsMasked] (materials, instances) {blend_match:?}"
    );
    eprintln!("NumUserTexCoords {tex_coords:?}");
    eprintln!("UsingTransforms {transforms:?}");
    eprintln!("flags {flags:?}");
    eprintln!("uniform textures {uniform} (texture class {uniform_textures})");
    eprintln!("lookups {lookups} in range {lookup_in_range} scales {lookup_scale:?}");
    eprintln!(
        "dependency entries {deps} (expressions {deps_expr}), max consistent {dep_max_ok}/{resources}"
    );
    eprintln!(
        "static sets: switches {switches} masks {cmasks} normal {normals} terrain {terrain} \
         overridden {overridden}; base id match {base_ok} mismatch {base_bad}; own id == base id {sets_equal_bases}"
    );
    assert_eq!(resources, 1408);
    assert_eq!(
        blend_match[0],
        (1228, 1228),
        "resource u32s are [blend mode, 0, masked]"
    );
    assert_eq!(blend_match[1], (169, 180));
    assert_eq!(legacy.len(), 1, "the discarded legacy u32 is one constant");
    assert_eq!(
        uniform_textures, uniform,
        "uniform expression textures are textures"
    );
    // TextureIndex is not always an index into UniformExpressionTextures
    // (TENTATIVE: it indexes the shader map's texture expressions).
    assert!(lookup_in_range <= lookups);
    assert_eq!(deps_expr, deps, "dependency keys are expression subobjects");
    assert_eq!(dep_max_ok, resources);
    assert_eq!(base_ok, 180);
    assert_eq!(base_bad, 0);
    assert_eq!(
        sets_equal_bases, 0,
        "a static permutation has its own resource Id"
    );
    assert_eq!(terrain, 0);
}

#[test]
fn approximations_cover_every_material() {
    let dir = require_data!();
    let mut status: BTreeMap<String, usize> = BTreeMap::new();
    let mut distinct: BTreeMap<String, (ApproxStatus, bool)> = BTreeMap::new();
    let mut differing = 0;
    let mut tex_refs = 0;
    let mut tex_ok = 0;
    let mut missing_tex = BTreeSet::new();
    let mut chains: BTreeMap<usize, usize> = BTreeMap::new();
    for_each_package(&dir, |set, lp| {
        let dec = MaterialDecoder::new(set);
        scan_package(&dec, lp, &mut |m, res| {
            let a = res.unwrap();
            bump(&mut status, format!("{:?}", a.status));
            bump(&mut chains, a.chain.len());
            let key = m.path.to_ascii_lowercase();
            match distinct.get(&key) {
                Some(prev) if *prev != (a.status, a.lossless) => differing += 1,
                Some(_) => {}
                None => {
                    distinct.insert(key, (a.status, a.lossless));
                }
            }
            for t in &a.textures {
                tex_refs += 1;
                // A few qualified paths are shared by two exports (a material
                // and a texture); look at every export with the path.
                let local: Vec<String> = (0..lp.package.exports.len())
                    .filter(|i| lp.qualified(*i).is_ok_and(|q| q.eq_ignore_ascii_case(t)))
                    .map(|i| export_class(lp, i))
                    .collect();
                let hit = local
                    .into_iter()
                    .find(|c| class_chain_has(set, c, "Engine.Texture"))
                    .or_else(|| set.locate(t).map(|(p, i)| export_class(&p, i)));
                if hit.is_some_and(|c| class_chain_has(set, &c, "Engine.Texture")) {
                    tex_ok += 1;
                } else {
                    missing_tex.insert(t.clone());
                }
            }
        });
    });
    let approximated = distinct
        .values()
        .filter(|(s, _)| *s == ApproxStatus::Approximated)
        .count();
    let lossless = distinct
        .values()
        .filter(|(s, l)| *s == ApproxStatus::Approximated && *l)
        .count();
    eprintln!(
        "status {status:?}; distinct {} approximated {approximated} lossless {lossless}; \
         chain lengths {chains:?}; texture refs {tex_refs} resolved {tex_ok}; missing {missing_tex:?}",
        distinct.len()
    );
    assert_eq!(differing, 0, "copies of a path approximate alike");
    assert_eq!(distinct.len(), 1100);
    assert_eq!(status.values().sum::<usize>(), 1710);
    assert_eq!(status.get("Approximated").copied(), Some(1695));
    assert_eq!(approximated, 1089);
    assert_eq!(lossless, 473);
    assert_eq!(tex_ok, tex_refs, "every bound texture is a texture export");
    assert_eq!(tex_refs, 3394);
    assert_eq!(
        chains,
        BTreeMap::from([(1, 1228), (2, 444), (3, 30), (4, 8)])
    );
}

#[test]
fn mesh_component_and_bsp_references_resolve() {
    let dir = require_data!();
    // (kind, package, path) of every reference; keys of every package.
    let mut refs: Vec<(&'static str, String, String)> = Vec::new();
    let mut keys = HashSet::new();
    for_each_package(&dir, |set, lp| {
        let dec = MaterialDecoder::new(set);
        scan_package(&dec, lp, &mut |m, res| {
            if res.is_ok() {
                keys.insert(m.path.to_ascii_lowercase());
            }
        });
        let mut add = |kind: &'static str, path: Option<String>| {
            if let Some(p) = path {
                refs.push((kind, lp.name.clone(), p));
            }
        };
        for i in 0..lp.package.exports.len() {
            if !is_static_mesh(&lp.package, i) {
                continue;
            }
            let mesh = decode_static_mesh(&lp.package, Some(&lp.name), i, set).unwrap();
            for lod in &mesh.native.lods {
                for s in &lod.sections {
                    add("section", lp.ref_path(s.material).ok().flatten());
                }
            }
        }
        for level_export in level::level_exports(&lp.package) {
            let scene =
                level::extract_scene(set, lp, level_export, &SceneOptions::default()).unwrap();
            for a in &scene.actors {
                for c in &a.components {
                    for m in &c.materials {
                        add("override", m.clone());
                    }
                }
            }
            if let Some(model) = scene.tail.model.export_index() {
                let g = level::extract_bsp(set, lp, model).unwrap();
                for s in &g.surfaces {
                    add("surface", s.material.clone());
                }
            }
        }
    });
    let mut counts: BTreeMap<&str, (usize, usize)> = BTreeMap::new();
    let mut unresolved = BTreeSet::new();
    for (kind, package, path) in &refs {
        let e = counts.entry(kind).or_default();
        e.0 += 1;
        if keys.contains(&path.to_ascii_lowercase()) {
            e.1 += 1;
        } else {
            unresolved.insert(format!("{package}: {path}"));
        }
    }
    eprintln!("references (total, resolved) {counts:?}; unresolved {unresolved:?}");
    assert!(unresolved.is_empty());
    assert_eq!(
        counts,
        BTreeMap::from([
            ("override", (9864, 9864)),
            ("section", (1983, 1983)),
            ("surface", (482, 482)),
        ])
    );
}

#[test]
fn resource_ids_key_the_shader_cache() {
    let dir = require_data!();
    let cache = dir.join("RefShaderCache-PC-OpenGL.upk");
    if !cache.is_file() {
        eprintln!("SKIP: no OpenGL shader cache");
        return;
    }
    let mut ids: Vec<Guid> = Vec::new();
    let mut seen = HashSet::new();
    let mut instance_ids = HashSet::new();
    let mut material_ids = HashSet::new();
    for_each_package(&dir, |set, lp| {
        let dec = MaterialDecoder::new(set);
        for i in 0..lp.package.exports.len() {
            if dec.export_class(lp, i).is_none() {
                continue;
            }
            let m = dec.decode(lp, i).unwrap();
            for q in &m.native.resources {
                if seen.insert(q.resource.id) {
                    ids.push(q.resource.id);
                }
                if m.class.is_instance() {
                    instance_ids.insert(q.resource.id);
                } else {
                    material_ids.insert(q.resource.id);
                }
            }
        }
    });
    let pkg = asamu_ue3::Package::open(&cache).unwrap();
    let hits = count_guid_occurrences(pkg.stream(), &ids);
    let found = hits.values().filter(|n| **n > 0).count();
    eprintln!(
        "distinct resource ids {} found in the OpenGL shader cache {found} ({} occurrences)",
        ids.len(),
        hits.values().sum::<usize>()
    );
    let missing: Vec<&Guid> = hits
        .iter()
        .filter(|(_, n)| **n == 0)
        .map(|(g, _)| g)
        .collect();
    eprintln!(
        "material ids {} instance ids {}; missing ids that belong to instances {}",
        material_ids.len(),
        instance_ids.len(),
        missing
            .iter()
            .filter(|g| instance_ids.contains(**g))
            .count()
    );
    assert_eq!(ids.len(), 825);
    assert_eq!(found, 778);
    assert!(
        missing
            .iter()
            .all(|g| instance_ids.contains(*g) && !material_ids.contains(*g)),
        "only static-permutation instance ids are missing"
    );
}

/// Facts re-derived by the verification pass (independent Python decoder)
/// and the behaviour its fixes rely on: parameter lists of every instance
/// copy, null texture overrides, and the effect of the single-channel tint,
/// constant-time rotation and `DepthBiasBlend` fixes on the approximations.
#[test]
fn parameter_lists_and_verification_fixes() {
    use asamu_ue3::property::Value;
    let dir = require_data!();
    let lists = [
        "ScalarParameterValues",
        "VectorParameterValues",
        "TextureParameterValues",
        "LinearColorParameterValues",
        "FontParameterValues",
    ];
    let mut elements: BTreeMap<&str, usize> = BTreeMap::new();
    let mut without_value = 0;
    let mut duplicate_names = 0;
    let mut null_textures = 0;
    let mut bound_null = 0;
    let mut depth_bias_blend = 0;
    let mut rotated: BTreeSet<String> = BTreeSet::new();
    let mut tinted: BTreeSet<String> = BTreeSet::new();
    for_each_package(&dir, |set, lp| {
        let dec = MaterialDecoder::new(set);
        scan_package(&dec, lp, &mut |m, res| {
            if m.class.is_instance() {
                for list in lists {
                    let Some(Value::Array(items)) = asamu_ue3::material::prop(&m.tagged, list)
                    else {
                        continue;
                    };
                    let mut names = BTreeSet::new();
                    for item in items {
                        let Value::Struct { fields, .. } = item else {
                            panic!("{}: {list} element is not a struct", m.path);
                        };
                        *elements.entry(list).or_insert(0) += 1;
                        let name = asamu_ue3::material::prop(fields, "ParameterName");
                        if let Some(Value::Name(n)) = name
                            && !names.insert(n.to_ascii_lowercase())
                        {
                            duplicate_names += 1;
                        }
                        let value = asamu_ue3::material::prop(fields, "ParameterValue");
                        if value.is_none() {
                            without_value += 1;
                        }
                        if list == "TextureParameterValues"
                            && matches!(value, Some(Value::Object(o)) if o.index == 0)
                        {
                            null_textures += 1;
                        }
                    }
                }
            }
            let a = res.unwrap();
            let channels = [
                Some(&a.base_color),
                Some(&a.emissive),
                Some(&a.specular),
                Some(&a.specular_power),
                a.normal.as_ref(),
                a.opacity.as_ref(),
            ];
            let present = channels
                .into_iter()
                .enumerate()
                .filter_map(|(i, c)| c.map(|c| (i, c)));
            for (i, ch) in present {
                let Some(t) = &ch.texture else {
                    continue;
                };
                if t.texture.is_none() {
                    bound_null += 1;
                }
                if t.uv.rotation_angle != 0.0 {
                    rotated.insert(m.path.to_ascii_lowercase());
                }
                // Colour channels (base, emissive, specular, normal; not the
                // scalar specular power / opacity) whose single texture
                // channel is tinted per component.
                if matches!(i, 0 | 1 | 2 | 4) && t.channels.len() == 1 {
                    let v = ch.value;
                    if v[0] != v[1] || v[1] != v[2] {
                        tinted.insert(m.path.to_ascii_lowercase());
                    }
                }
            }
            depth_bias_blend += usize::from(a.unsupported.contains_key("DepthBiasBlend"));
        });
    });
    eprintln!(
        "parameter elements {elements:?}; without ParameterValue {without_value}; duplicate \
         names {duplicate_names}; null texture overrides {null_textures}; channels bound to \
         a null texture {bound_null}; fixed UV rotations {}; tinted single channels {}",
        rotated.len(),
        tinted.len()
    );
    assert_eq!(
        elements,
        BTreeMap::from([
            ("ScalarParameterValues", 1158),
            ("TextureParameterValues", 767),
            ("VectorParameterValues", 279),
        ])
    );
    assert_eq!(without_value, 0, "array struct elements are stored in full");
    assert_eq!(duplicate_names, 0, "a name never repeats within one list");
    assert_eq!(null_textures, 6);
    assert_eq!(bound_null, 0, "a null override falls through");
    assert_eq!(depth_bias_blend, 0, "DepthBiasBlend is a texture sample");
    assert_eq!(rotated.len(), 3);
    assert_eq!(tinted.len(), 47);
}
