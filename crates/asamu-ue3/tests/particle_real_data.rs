//! Particle decoding against the user's own installed game (read-only).
//!
//! Reads `ASAMU_ORIGINAL_DIR` (the folder containing
//! `A Story About My Uncle.app`) or the default macOS Steam location and
//! skips cleanly when the data is absent. Only counts and structural facts
//! are asserted; nothing is copied or written.
//!
//! Acceptance test for `docs/reverse-engineering/PARTICLES.md`: `(T)` claims
//! there are asserted here. Run with `-- --nocapture` for the tables.

#![allow(clippy::unwrap_used)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use asamu_ue3::level::{self, ComponentKind, SceneOptions};
use asamu_ue3::matinee::InterpCurve;
use asamu_ue3::model::{LoadedPackage, PackageSet};
use asamu_ue3::particle::{
    Distribution, Param, ParticleCensus, ParticleDecoder, ParticleRole, RawDistribution,
    TwoVectors, curve_as, lookup_value,
};

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
                let name = p
                    .file_name()
                    .map(|n| n.to_string_lossy().to_ascii_lowercase())
                    .unwrap_or_default();
                p.is_file()
                    && !name.contains("shadercache")
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

/// Visit every package with a fresh set (keeps memory bounded).
fn for_each_package(dir: &Path, mut f: impl FnMut(&PackageSet, &Arc<LoadedPackage>)) {
    for path in packages(dir) {
        let set = PackageSet::new(&[dir.to_path_buf(), dir.join("Maps")]);
        let lp = set.open_file(&path).unwrap();
        f(&set, &lp);
    }
}

fn merge(into: &mut ParticleCensus, c: ParticleCensus) {
    into.packages += c.packages;
    for (k, v) in c.classes {
        let e = into.classes.entry(k).or_default();
        e.role = v.role;
        e.exports += v.exports;
        e.default_objects += v.default_objects;
        e.exact += v.exact;
        e.failed += v.failed;
    }
    for (k, v) in c.per_package {
        *into.per_package.entry(k).or_default() += v;
    }
    into.systems += c.systems;
    into.systems_failed += c.systems_failed;
    into.emitters += c.emitters;
    into.lod_levels += c.lod_levels;
    into.modules += c.modules;
    into.raw_distributions += c.raw_distributions;
    into.raw_without_object += c.raw_without_object;
    for (k, v) in c.distribution_kinds {
        *into.distribution_kinds.entry(k).or_default() += v;
    }
    for (k, v) in c.module_classes {
        *into.module_classes.entry(k).or_default() += v;
    }
    for (k, v) in c.emitter_kinds {
        *into.emitter_kinds.entry(k).or_default() += v;
    }
    into.payload_bytes += c.payload_bytes;
    into.failures.extend(c.failures);
    into.notes.extend(c.notes);
}

#[test]
fn every_particle_export_decodes_exactly() {
    let dir = require_data!();
    let mut total = ParticleCensus::default();
    for_each_package(&dir, |set, lp| {
        let dec = ParticleDecoder::new(set);
        let c = dec.census(std::slice::from_ref(lp));
        merge(&mut total, c);
    });
    println!("packages scanned: {}", total.packages);
    println!("particle exports per package:");
    for (k, v) in &total.per_package {
        println!("  {k:32} {v}");
    }
    println!("classes (exports / CDOs / exact / failed):");
    for (k, v) in &total.classes {
        println!(
            "  {k:44} {:?} {:6} {:4} {:6} {:4}",
            v.role, v.exports, v.default_objects, v.exact, v.failed
        );
    }
    println!(
        "systems {} (failed {}), emitters {}, LOD levels {}, modules {}, raw distributions {} \
         ({} without object), payload bytes {}",
        total.systems,
        total.systems_failed,
        total.emitters,
        total.lod_levels,
        total.modules,
        total.raw_distributions,
        total.raw_without_object,
        total.payload_bytes
    );
    println!("emitter kinds: {:?}", total.emitter_kinds);
    println!("distribution kinds: {:?}", total.distribution_kinds);
    println!("module classes:");
    for (k, v) in &total.module_classes {
        println!("  {k:44} {v}");
    }
    for f in total.failures.iter().take(40) {
        println!("FAIL {f}");
    }
    for n in total.notes.iter().take(40) {
        println!("NOTE {n}");
    }
    assert!(
        total.failures.is_empty(),
        "{} failures",
        total.failures.len()
    );
    let exports: usize = total.classes.values().map(|c| c.exports).sum();
    let exact: usize = total.classes.values().map(|c| c.exact).sum();
    println!("particle exports {exports}, exact {exact}");
    assert_eq!(exports, exact);
    let count = |n: &str| total.classes.get(n).map_or(0, |c| c.exports);
    assert_eq!(count("ParticleSystem"), 111);
    assert_eq!(count("ParticleSpriteEmitter"), 406);
    assert_eq!(count("ParticleLODLevel"), 686);
    assert_eq!(count("ParticleSystemComponent"), 198);
    assert_eq!(total.systems, 110);
    assert_eq!(total.systems_failed, 0);
    assert_eq!(total.emitters, 396);
    assert_eq!(total.lod_levels, 667);
    assert_eq!(total.modules, 6343);
    assert_eq!(total.raw_distributions, 8801);
    assert_eq!(total.raw_without_object, 0);
    // The other counts PARTICLES.md marks (T).
    assert_eq!(total.packages, 39);
    assert_eq!(exports, 10_978);
    assert_eq!(total.payload_bytes, 2_541_859);
    let per_package: Vec<(&str, usize)> = total
        .per_package
        .iter()
        .map(|(k, v)| (k.as_str(), *v))
        .collect();
    assert_eq!(
        per_package,
        vec![
            ("AG-BeautifulCity", 226),
            ("AG-Darkcave", 318),
            ("AG-Epilogue", 92),
            ("AG-IceCave", 312),
            ("AG-ParadiseCave", 422),
            ("AG-StarHaven", 328),
            ("AG-Workshop", 181),
            ("ASAMUFrontEndMap", 231),
            ("Core", 2),
            ("Engine", 381),
            ("Startup", 8303),
            ("TheCore", 149),
            ("UDKBase", 10),
            ("UTGameContent", 22),
            ("UnrealEd", 1),
        ]
    );
    assert_eq!(total.classes.len(), 156);
    let by_role = |r: ParticleRole| total.classes.values().filter(|c| c.role == Some(r)).count();
    assert_eq!(
        [
            by_role(ParticleRole::Module),
            by_role(ParticleRole::FloatDistribution),
            by_role(ParticleRole::VectorDistribution),
            by_role(ParticleRole::Component),
            by_role(ParticleRole::Emitter),
            by_role(ParticleRole::System),
            by_role(ParticleRole::LodLevel),
        ],
        [131, 9, 8, 4, 2, 1, 1]
    );
    assert_eq!(
        total
            .classes
            .values()
            .filter(|c| c.exports == c.default_objects)
            .count(),
        100,
        "classes that occur only as their class default object"
    );
    for (class, n) in [
        ("DistributionFloatConstant", 1955),
        ("DistributionVectorUniform", 1053),
        ("DistributionFloatUniform", 1042),
        ("DistributionVectorConstantCurve", 631),
        ("DistributionVectorConstant", 483),
        ("ParticleModuleSpawn", 474),
        ("DistributionFloatConstantCurve", 410),
        ("ParticleModuleRequired", 408),
        ("ParticleModuleSize", 406),
        ("ParticleModuleLifetime", 399),
        ("ParticleModuleColorOverLife", 315),
        ("ParticleModuleVelocity", 275),
        ("ParticleModuleSizeMultiplyLife", 245),
    ] {
        assert_eq!(count(class), n, "{class}");
    }
    let kinds = |m: &BTreeMap<String, usize>| -> Vec<(String, usize)> {
        m.iter().map(|(k, v)| (k.clone(), *v)).collect()
    };
    assert_eq!(
        kinds(&total.emitter_kinds),
        vec![
            ("beam".to_owned(), 26),
            ("mesh".to_owned(), 76),
            ("sprite".to_owned(), 294)
        ]
    );
    assert_eq!(
        kinds(&total.distribution_kinds),
        vec![
            ("float:constant".to_owned(), 3003),
            ("float:constant_curve".to_owned(), 574),
            ("float:parameter".to_owned(), 4),
            ("float:uniform".to_owned(), 1643),
            ("float:uniform_curve".to_owned(), 2),
            ("vector:constant".to_owned(), 929),
            ("vector:constant_curve".to_owned(), 963),
            ("vector:parameter".to_owned(), 90),
            ("vector:uniform".to_owned(), 1593),
        ]
    );
    for (class, n) in [
        ("ParticleModuleRequired", 667),
        ("ParticleModuleSpawn", 667),
        ("ParticleModuleLifetime", 667),
        ("ParticleModuleSize", 667),
        ("ParticleModuleColorOverLife", 463),
        ("ParticleModuleSizeMultiplyLife", 441),
        ("ParticleModuleVelocity", 414),
        ("ParticleModuleRotation", 382),
        ("ParticleModuleLocation", 356),
        ("ParticleModuleColor", 214),
        ("ParticleModuleRotationRate", 193),
        ("ParticleModuleAcceleration", 186),
        ("ParticleModuleTypeDataMesh", 147),
        ("ParticleModuleLocationPrimitiveSphere", 100),
        ("ParticleModuleSubUV", 86),
        ("ParticleModuleOrbit", 29),
        ("ParticleModuleSizeScaleByTime", 5),
        ("ParticleModuleAttractorPoint", 1),
    ] {
        assert_eq!(total.module_classes.get(class), Some(&n), "{class}");
    }
    // No shipped system reaches a module the runtime gives engine
    // behaviour without data to check it against.
    for class in ["ParticleModuleKillBox", "ParticleModuleKillHeight"] {
        assert_eq!(total.module_classes.get(class), None, "{class}");
    }
}

/// System-level settings the runtime relies on: no shipped system opts out
/// of the spawn count limit, none has an activation delay, and the LOD
/// methods are the default (automatic) or `DirectSet`.
#[test]
fn system_settings_census() {
    let dir = require_data!();
    let mut seen = std::collections::HashSet::new();
    let mut lod_methods: BTreeMap<String, usize> = BTreeMap::new();
    let mut warmups = Vec::new();
    let mut fixed = 0usize;
    let mut lod_distance_lens: BTreeMap<usize, usize> = BTreeMap::new();
    let mut max_lod_distance = 0.0f32;
    for_each_package(&dir, |set, lp| {
        let dec = ParticleDecoder::new(set);
        for i in asamu_ue3::particle::system_exports(&lp.package) {
            let s = dec.system(lp, i).unwrap();
            assert!(
                !matches!(
                    s.params.get("bSkipSpawnCountCheck"),
                    Some(Param::Bool(true))
                ),
                "{} skips the spawn count check",
                s.path
            );
            for k in ["Delay", "DelayLow", "bUseDelayRange", "WarmupTickRate"] {
                assert!(
                    !s.params.contains_key(k),
                    "{} stores {k}: {:?}",
                    s.path,
                    s.params.get(k)
                );
            }
            if !seen.insert(s.path.to_ascii_lowercase()) {
                continue;
            }
            let method = s
                .params
                .get("LODMethod")
                .and_then(Param::as_text)
                .unwrap_or("(default)")
                .to_owned();
            *lod_methods.entry(method).or_default() += 1;
            if let Some(w) = s.params.get("WarmupTime").and_then(Param::as_f32) {
                warmups.push(w);
            }
            if s.params.get("SystemUpdateMode").and_then(Param::as_text) == Some("EPSUM_FixedTime")
            {
                fixed += 1;
            }
            if let Some(Param::List(d)) = s.params.get("LODDistances") {
                *lod_distance_lens.entry(d.len()).or_default() += 1;
                for x in d {
                    max_lod_distance = max_lod_distance.max(x.as_f32().unwrap_or(0.0));
                }
            }
        }
    });
    warmups.sort_by(f32::total_cmp);
    println!(
        "{} distinct systems; LOD methods {lod_methods:?}; warm-ups {warmups:?}; fixed-time {fixed}; \
         LODDistances lengths {lod_distance_lens:?}, largest {max_lod_distance}",
        seen.len()
    );
    assert_eq!(seen.len(), 100);
    assert_eq!(
        lod_methods.into_iter().collect::<Vec<_>>(),
        vec![
            ("(default)".to_owned(), 95),
            ("PARTICLESYSTEMLODMETHOD_DirectSet".to_owned(), 5)
        ]
    );
    assert_eq!(warmups, vec![5.0, 5.0, 6.0]);
    assert_eq!(fixed, 1);
    assert_eq!(
        lod_distance_lens.into_iter().collect::<Vec<_>>(),
        vec![(1, 37), (2, 63)]
    );
    assert_eq!(max_lod_distance, 2500.0);
}

fn collect<'a>(p: &'a Param, out: &mut Vec<&'a RawDistribution>) {
    match p {
        Param::Distribution(d) => out.push(d),
        Param::List(l) => l.iter().for_each(|x| collect(x, out)),
        Param::Struct(m) => m.values().for_each(|x| collect(x, out)),
        _ => {}
    }
}

fn system_distributions(s: &asamu_ue3::particle::ParticleSystem) -> Vec<&RawDistribution> {
    let mut out = Vec::new();
    for v in s.params.values() {
        collect(v, &mut out);
    }
    for e in &s.emitters {
        for v in e.params.values() {
            collect(v, &mut out);
        }
        for l in &e.lods {
            for m in l
                .required
                .iter()
                .chain(&l.spawn)
                .chain(&l.type_data)
                .chain(&l.event_generator)
                .chain(&l.modules)
            {
                for v in m.params.values() {
                    collect(v, &mut out);
                }
            }
        }
    }
    out
}

fn lock(mut v: Vec<f32>, locked: u8) -> Vec<f32> {
    if v.len() == 3 {
        match locked {
            1 => v[1] = v[0],
            2 => v[2] = v[0],
            3 => v[2] = v[1],
            4 => {
                v[1] = v[0];
                v[2] = v[0];
            }
            _ => {}
        }
    }
    v
}

/// Reference value of a non-random distribution at `t` (matinee curve
/// evaluator, CONFIRMED engine-exact in MATINEE.md).
fn reference(d: &Distribution, t: f32) -> Option<Vec<f32>> {
    match d {
        Distribution::Constant { value, locked_axes } => Some(lock(value.clone(), *locked_axes)),
        Distribution::ConstantCurve { curve, locked_axes } => {
            let v = match curve.dim {
                1 => vec![curve_as::<f32>(curve).eval(t, 0.0)],
                3 => curve_as::<[f32; 3]>(curve).eval(t, [0.0; 3]).to_vec(),
                _ => return None,
            };
            Some(lock(v, *locked_axes))
        }
        _ => None,
    }
}

/// Reference `[min.., max..]` entry of a random distribution at `t`.
fn reference_range(d: &Distribution, t: f32) -> Option<(Vec<f32>, Vec<f32>)> {
    match d {
        Distribution::Uniform {
            min, max, mirror, ..
        } => {
            let mut lo = min.clone();
            if lo.len() == 3 {
                for i in 0..3 {
                    lo[i] = match mirror[i] {
                        0 => max[i],
                        2 => -max[i],
                        _ => min[i],
                    };
                }
            }
            let (lo, hi) = match d {
                Distribution::Uniform { locked_axes, .. } => {
                    (lock(lo, *locked_axes), lock(max.clone(), *locked_axes))
                }
                _ => (lo, max.clone()),
            };
            Some((lo, hi))
        }
        Distribution::UniformCurve { curve, .. } => match curve.dim {
            2 => {
                let v = curve_as::<[f32; 2]>(curve).eval(t, [0.0; 2]);
                Some((vec![v[0]], vec![v[1]]))
            }
            6 => {
                let c: InterpCurve<TwoVectors> = curve_as(curve);
                let v = c.eval(t, TwoVectors([0.0; 6])).0;
                Some((v[3..6].to_vec(), v[0..3].to_vec()))
            }
            _ => None,
        },
        _ => None,
    }
}

fn close(a: &[f32], b: &[f32]) -> bool {
    a.len() == b.len()
        && a.iter().zip(b).all(|(x, y)| {
            let tol = 1e-4_f32 * x.abs().max(y.abs()).max(1.0);
            (x - y).abs() <= tol
        })
}

/// With every tangent unscaled (`IMT_UseBrokenTangentEval`).
fn broken(d: &Distribution) -> Distribution {
    let mut d = d.clone();
    match &mut d {
        Distribution::ConstantCurve { curve, .. } | Distribution::UniformCurve { curve, .. } => {
            curve.broken_tangents = true;
        }
        _ => {}
    }
    d
}

fn legacy(d: &Distribution) -> bool {
    matches!(d, Distribution::ConstantCurve { curve, .. } | Distribution::UniformCurve { curve, .. } if curve.legacy_method)
}

/// Every entry of the table agrees with `value` evaluated at the entry's
/// sample time (`None`: nothing to compare for this op/kind).
fn table_agrees(d: &RawDistribution, value: &Distribution) -> Option<bool> {
    let dim = if d.dist == "vector" { 3 } else { 1 };
    let chunk = usize::from(d.baked.chunk);
    if chunk == 0 || d.table.len() < chunk + 2 {
        return None;
    }
    let entries = (d.table.len() - 2) / chunk;
    let mut compared = false;
    for e in 0..entries {
        let t = if d.baked.time_scale != 0.0 {
            d.baked.start_time + e as f32 / d.baked.time_scale
        } else {
            d.baked.start_time
        };
        let entry = &d.table[2 + e * chunk..2 + (e + 1) * chunk];
        let v = lookup_value(
            &d.table,
            d.baked.chunk,
            d.baked.time_scale,
            d.baked.start_time,
            t,
            chunk,
        )
        .unwrap();
        assert!(close(&v, entry), "lookup read at {t}");
        match d.baked.op {
            1 => {
                // A sample time that lands on a key of a discontinuous
                // (constant) segment may round to either side of it.
                let eps = if d.baked.time_scale != 0.0 {
                    1e-3 / d.baked.time_scale
                } else {
                    0.0
                };
                let near = [t, t - eps, t + eps]
                    .iter()
                    .filter_map(|&x| reference(value, x))
                    .collect::<Vec<_>>();
                if !near.is_empty() {
                    compared = true;
                    if !near.iter().any(|r| close(&entry[..dim], r)) {
                        return Some(false);
                    }
                }
            }
            2 => {
                if let Some((lo, hi)) = reference_range(value, t) {
                    compared = true;
                    if !(close(&entry[..dim], &lo) && close(&entry[dim..2 * dim], &hi)) {
                        return Some(false);
                    }
                }
            }
            _ => {}
        }
    }
    compared.then_some(true)
}

/// The baked `FRawDistribution` tables agree with our decoding of the
/// distribution objects evaluated by the engine-exact curve evaluator.
#[test]
fn baked_lookup_tables_agree_with_decoded_distributions() {
    let dir = require_data!();
    // kind -> [agree, legacy curves whose table matches unscaled tangents, other]
    let mut stats: BTreeMap<String, [usize; 3]> = BTreeMap::new();
    let mut range_ok = [0usize; 2];
    let mut range_outside: BTreeMap<String, usize> = BTreeMap::new();
    let mut mismatches: Vec<String> = Vec::new();
    let mut ops: BTreeMap<(String, u8), usize> = BTreeMap::new();
    let mut legacy_curves = [0usize; 2];
    let mut legacy_packages: BTreeMap<String, usize> = BTreeMap::new();
    let mut seen = std::collections::HashSet::new();
    for_each_package(&dir, |set, lp| {
        let dec = ParticleDecoder::new(set);
        for i in asamu_ue3::particle::system_exports(&lp.package) {
            let Ok(s) = dec.system(lp, i) else { continue };
            for d in system_distributions(&s) {
                let key = format!("{}:{:?}", s.path, d.object);
                if !seen.insert(key) {
                    continue;
                }
                *ops.entry((d.class.clone().unwrap_or_default(), d.baked.op))
                    .or_default() += 1;
                if let Some([lo, hi]) = d.baked.range {
                    let values = &d.table[2..];
                    let tol = |x: f32| 1e-3 * x.abs().max(1.0);
                    let inside = values
                        .iter()
                        .all(|v| *v >= lo - tol(lo) && *v <= hi + tol(hi));
                    range_ok[usize::from(inside)] += 1;
                    if !inside {
                        let what = format!(
                            "{} op {} {}",
                            d.class.clone().unwrap_or_default(),
                            d.baked.op,
                            if legacy(&d.value) {
                                "legacy"
                            } else {
                                "current"
                            }
                        );
                        *range_outside.entry(what).or_default() += 1;
                    }
                }
                if legacy(&d.value) {
                    legacy_curves[0] += 1;
                    let pkg = s.path.split('.').next().unwrap_or("").to_owned();
                    *legacy_packages.entry(pkg).or_default() += 1;
                } else if matches!(
                    d.value,
                    Distribution::ConstantCurve { .. } | Distribution::UniformCurve { .. }
                ) {
                    legacy_curves[1] += 1;
                }
                let Some(ok) = table_agrees(d, &d.value) else {
                    continue;
                };
                let kind = match &d.value {
                    Distribution::Constant { .. } => "constant",
                    Distribution::Uniform { .. } => "uniform",
                    Distribution::ConstantCurve { .. } => "constant_curve",
                    Distribution::UniformCurve { .. } => "uniform_curve",
                    _ => "other",
                };
                let slot = stats.entry(format!("{}:{kind}", d.dist)).or_default();
                if ok {
                    slot[0] += 1;
                } else if legacy(&d.value) && table_agrees(d, &broken(&d.value)) == Some(true) {
                    slot[1] += 1;
                } else {
                    slot[2] += 1;
                    if mismatches.len() < 20 {
                        mismatches.push(format!(
                            "{} {:?} {:?} baked {:?} table {:?}",
                            s.path,
                            d.object,
                            d.value,
                            d.baked,
                            &d.table[..d.table.len().min(16)]
                        ));
                    }
                }
            }
        }
    });
    println!(
        "baked tables (kind: [agree, legacy curve baked with unscaled tangents, other]): {stats:?}"
    );
    println!("range header contains every entry: [no, yes] = {range_ok:?}");
    println!("tables with entries outside their range header: {range_outside:?}");
    println!("ops by distribution class: {ops:?}");
    println!(
        "curves [legacy InterpMethod, other] = {legacy_curves:?}; legacy by package {legacy_packages:?}"
    );
    for m in &mismatches {
        println!("MISMATCH {m}");
    }
    let agree: usize = stats.values().map(|s| s[0]).sum();
    let legacy_explained: usize = stats.values().map(|s| s[1]).sum();
    let other: usize = stats.values().map(|s| s[2]).sum();
    println!("agree {agree}, legacy unscaled {legacy_explained}, other {other}");
    assert_eq!(agree, 3916, "agreeing tables");
    assert_eq!(
        legacy_explained, 179,
        "legacy tables explained by unscaled tangents"
    );
    // The rest are uniform tables older than their objects' current values
    // (the game evaluates the objects; PARTICLES.md lists them).
    assert_eq!(other, 6, "tables that disagree");
    assert_eq!(legacy_curves, [381, 439]);
    assert_eq!(range_ok, [99, 4549]);
    assert_eq!(
        stats.into_iter().collect::<Vec<_>>(),
        vec![
            ("float:constant".to_owned(), [1700, 0, 0]),
            ("float:constant_curve".to_owned(), [256, 45, 0]),
            ("float:uniform".to_owned(), [492, 0, 1]),
            ("float:uniform_curve".to_owned(), [1, 0, 0]),
            ("vector:constant".to_owned(), [425, 0, 0]),
            ("vector:constant_curve".to_owned(), [345, 134, 0]),
            ("vector:uniform".to_owned(), [697, 0, 5]),
        ]
    );
    assert_eq!(
        range_outside.into_iter().collect::<Vec<_>>(),
        vec![
            ("DistributionFloatUniform op 2 current".to_owned(), 56),
            ("DistributionVectorUniform op 2 current".to_owned(), 43),
        ]
    );
    assert_eq!(
        legacy_packages
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        vec![
            "Envy_Effects",
            "Envy_Level_Effects_2",
            "FX_VehicleExplosions",
            "FoliageDemo",
            "Pickups",
            "T_FX",
            "VH_All",
            "WP_LinkGun",
            "WP_RocketLauncher",
            "WP_ShockRifle",
            "WP_Translocator",
        ]
    );
    assert_eq!(legacy_packages.get("FoliageDemo"), Some(&4));
}

/// Enumerator orders the decoder relies on, read from `Core.u` / `Engine.u`.
#[test]
fn enum_orders_match_the_script_packages() {
    let dir = require_data!();
    let set = PackageSet::new(&[dir.clone(), dir.join("Maps")]);
    let enums = |class: &str, name: &str| -> Vec<String> {
        let m = set.class_model(class).unwrap();
        m.enums
            .iter()
            .find(|e| e.name == name)
            .map(|e| e.values.clone())
            .unwrap_or_default()
    };
    let check = |got: Vec<String>, want: &[&str]| {
        assert!(got.len() > want.len(), "{got:?}");
        for (g, w) in got.iter().zip(want) {
            assert_eq!(g, w);
        }
    };
    check(
        enums("Core.DistributionVector", "EDistributionVectorLockFlags"),
        asamu_ue3::particle::LOCK_FLAG_NAMES,
    );
    check(
        enums("Core.DistributionVector", "EDistributionVectorMirrorFlags"),
        asamu_ue3::particle::MIRROR_FLAG_NAMES,
    );
    check(
        enums(
            "Engine.DistributionFloatParameterBase",
            "DistributionParamMode",
        ),
        asamu_ue3::particle::PARAM_MODE_NAMES,
    );
}

/// Placements: every particle system component of every map's placed actors,
/// with its effective template resolved to a decodable system.
#[test]
fn map_particle_components_resolve() {
    let dir = require_data!();
    let maps = packages(&dir)
        .into_iter()
        .filter(|p| {
            p.extension()
                .is_some_and(|e| e.eq_ignore_ascii_case("asamu"))
        })
        .collect::<Vec<_>>();
    let mut total = 0usize;
    let mut by_class: BTreeMap<String, usize> = BTreeMap::new();
    let mut auto = [0usize; 2];
    let mut templates_ok = 0usize;
    let mut no_template = 0usize;
    for path in maps {
        let set = PackageSet::new(&[dir.clone(), dir.join("Maps")]);
        let lp = set.open_file(&path).unwrap();
        let dec = ParticleDecoder::new(&set);
        let mut n = 0usize;
        for lvl in level::level_exports(&lp.package) {
            let scene = level::extract_scene(&set, &lp, lvl, &SceneOptions::default()).unwrap();
            for a in &scene.actors {
                for c in &a.components {
                    if c.kind != ComponentKind::ParticleSystem {
                        continue;
                    }
                    n += 1;
                    *by_class.entry(a.class.clone()).or_default() += 1;
                    let info = dec.component(&lp, c.export_index).unwrap();
                    auto[usize::from(info.auto_activate)] += 1;
                    match &info.template {
                        Some(t) => {
                            let (tlp, ti) = set.locate(t).unwrap_or_else(|| panic!("{t}"));
                            let s = dec.system(&tlp, ti).unwrap();
                            assert!(!s.emitters.is_empty() || s.skipped_emitters == 0);
                            templates_ok += 1;
                        }
                        None => no_template += 1,
                    }
                }
            }
        }
        println!("{:28} {n} particle components", lp.name);
        total += n;
    }
    println!(
        "total {total}; by actor class {by_class:?}; bAutoActivate [false, true] = {auto:?}; templates resolved {templates_ok}, none {no_template}"
    );
    assert_eq!(total, 162);
    assert_eq!(templates_ok + no_template, total);
    assert_eq!(no_template, 0);
    assert_eq!(
        by_class.into_iter().collect::<Vec<_>>(),
        vec![("Engine.Emitter".to_owned(), 162)]
    );
    assert_eq!(auto, [6, 156]);
}
