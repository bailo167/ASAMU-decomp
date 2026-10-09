//! Baked-lighting component data against the user's own installed game
//! (read-only).
//!
//! Reads `ASAMU_ORIGINAL_DIR` (the folder containing
//! `A Story About My Uncle.app`) or the default macOS Steam location and
//! skips cleanly when the data is absent. Only counts and structural facts are
//! asserted; nothing is copied or written.
//!
//! Acceptance test for `docs/reverse-engineering/LIGHTMAPS.md`: `(T)` claims
//! there are asserted here. Run with `-- --nocapture` for the tables.

#![allow(clippy::unwrap_used)]

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use asamu_ue3::lightmap::{
    LayoutCoverage, LightMap, LightingLayout, LightingNative, decode_lighting, guid_of,
    lighting_coverage, lighting_layout, shadow_map_2d_info,
};
use asamu_ue3::model::{LoadedPackage, PackageSet};
use asamu_ue3::property::Value;
use asamu_ue3::staticmesh::decode_static_mesh;

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

fn packages(dir: &Path, maps_only: bool) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let dirs = if maps_only {
        vec![dir.join("Maps")]
    } else {
        vec![dir.to_path_buf(), dir.join("Maps")]
    };
    for d in dirs {
        let Ok(rd) = std::fs::read_dir(&d) else {
            continue;
        };
        let mut v: Vec<PathBuf> = rd
            .flatten()
            .map(|e| e.path())
            .filter(|p| {
                p.is_file()
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

fn for_each_package(dir: &Path, maps_only: bool, mut f: impl FnMut(&PackageSet, &LoadedPackage)) {
    for path in packages(dir, maps_only) {
        let set = PackageSet::new(&[dir.to_path_buf(), dir.join("Maps")]);
        let lp = set.open_file(&path).unwrap();
        f(&set, &lp);
    }
}

/// `(T)`: every lighting export of every package decodes exactly, re-encodes
/// byte for byte and passes the structural checks.
#[test]
fn every_lighting_export_decodes_exactly() {
    let dir = require_data!();
    let mut totals: BTreeMap<LightingLayout, LayoutCoverage> = BTreeMap::new();
    let mut maps: BTreeMap<&'static str, usize> = BTreeMap::new();
    let mut subclasses: BTreeMap<String, (usize, usize)> = BTreeMap::new();
    let (mut sm2d, mut sm1d, mut colors, mut painted, mut inst, mut guids, mut elems) =
        (0, 0, 0, 0, 0, 0, 0);
    let (mut tex_ok, mut simple_tex, mut rect_ok) = (0, 0, 0);
    for_each_package(&dir, false, |set, lp| {
        let c = lighting_coverage(lp, set);
        if c.layouts.is_empty() && c.subclasses.is_empty() {
            return;
        }
        eprintln!(
            "{:20} {:?} light maps {:?}",
            c.package, c.layouts, c.light_maps
        );
        for f in c.failures.iter().chain(&c.issues) {
            eprintln!("    {f}");
        }
        for (l, v) in &c.layouts {
            let t = totals.entry(*l).or_default();
            t.total += v.total;
            t.exact += v.exact;
            t.round_trip += v.round_trip;
            t.valid += v.valid;
            t.native_bytes += v.native_bytes;
        }
        for (k, v) in &c.light_maps {
            *maps.entry(k).or_default() += v;
        }
        for (k, v) in &c.subclasses {
            let e = subclasses.entry(k.clone()).or_insert((0, 0));
            e.0 += v.0;
            e.1 += v.1;
        }
        sm2d += c.shadow_map_2d_refs;
        sm1d += c.shadow_map_1d_refs;
        colors += c.override_color_lods;
        painted += c.painted_vertices;
        inst += c.instances;
        guids += c.light_guid_refs;
        elems += c.model_elements;
        tex_ok += c.light_map_2d_textures_ok;
        simple_tex += c.light_map_2d_simple_textures;
        rect_ok += c.light_map_2d_rect_ok;
    });
    eprintln!("totals {totals:?}");
    eprintln!(
        "light maps {maps:?}; shadow maps 2D {sm2d} 1D {sm1d}; override-colour LODs {colors}; \
         painted vertices {painted}; instances {inst}; light GUID refs {guids}; model elements \
         {elems}; 2D directional texture pairs ok {tex_ok}; 2D simple textures {simple_tex}; 2D \
         rects inside [0,1] {rect_ok}; subclasses {subclasses:?}"
    );
    let get = |l| totals.get(&l).cloned().unwrap_or_default();
    let expect = [
        (LightingLayout::StaticMeshComponent, 24_628),
        (LightingLayout::InstancedStaticMeshComponent, 102),
        (LightingLayout::ModelComponent, 161),
        (LightingLayout::SpeedTreeComponent, 4),
        (LightingLayout::FluidSurfaceComponent, 2),
        (LightingLayout::ShadowMap1D, 20),
    ];
    for (layout, n) in expect {
        let c = get(layout);
        assert_eq!(c.total, n, "{layout:?} exports");
        assert_eq!(c.exact, n, "{layout:?} exact");
        assert_eq!(c.round_trip, n, "{layout:?} round trip");
        assert_eq!(c.valid, n, "{layout:?} valid");
    }
    assert_eq!(subclasses.get("UTGibStaticMeshComponent"), Some(&(2, 2)));
    assert_eq!(maps.get("1d"), Some(&784));
    assert_eq!(maps.get("2d"), Some(&24_575));
    assert_eq!(maps.get("none"), Some(&326));
    // Textures 0 and 1 are always two distinct `LightMapTexture2D` exports
    // of the same package; the third (simple) slot is never filled.
    assert_eq!(tex_ok, 24_575);
    assert_eq!(simple_tex, 0, "a simple coefficient texture is present");
    assert_eq!(sm2d, 5_364);
    assert_eq!(sm1d, 20);
    assert_eq!(inst, 4_543);
    assert_eq!(colors, 161);
    assert_eq!(elems, 223);
    assert_eq!(rect_ok, 24_573);
}

fn last_name(path: &str) -> &str {
    path.rsplit('.').next().unwrap_or(path)
}

fn prefix(name: &str) -> &str {
    name.trim_end_matches(|c: char| c.is_ascii_digit() || c == '_')
}

/// `(T)`: the two stored coefficient textures of every 2D light map are a
/// `NormalizedAverageColor` / `DirectionalMaxComponent` pair of the same
/// atlas, and the third (simple) slot is always null.
#[test]
fn coefficient_textures_pair_up() {
    let dir = require_data!();
    let mut names: BTreeMap<String, usize> = BTreeMap::new();
    let (mut pairs, mut checked) = (0, 0);
    let mut s0_dev = 0.0_f32;
    let mut s1_max = 0.0_f32;
    // 2D light maps whose rectangle leaves [0, 1], by component class.
    let mut outside: BTreeMap<&'static str, usize> = BTreeMap::new();
    for_each_package(&dir, true, |set, lp| {
        let pkg = &lp.package;
        for i in 0..pkg.exports.len() {
            let class = pkg.export_class_name(i).unwrap_or_default();
            if class == "LightMapTexture2D" || class == "ShadowMapTexture2D" {
                let p = pkg.export_path(i).unwrap();
                *names
                    .entry(format!("{class} {}", prefix(last_name(&p))))
                    .or_default() += 1;
            }
            if lighting_layout(pkg, i).is_none() {
                continue;
            }
            let d = decode_lighting(pkg, Some(&lp.name), i, set).unwrap();
            for m in d.native.light_maps() {
                let LightMap::TwoD(t) = m else { continue };
                checked += 1;
                if !t
                    .uv_rect()
                    .iter()
                    .all(|v| v.is_finite() && (-1e-6..=1.0 + 1e-6).contains(v))
                {
                    *outside.entry(d.layout.class_name()).or_default() += 1;
                }
                assert!(t.textures[2].is_null());
                let a = pkg.object_path(t.textures[0]).unwrap();
                let b = pkg.object_path(t.textures[1]).unwrap();
                let (na, nb) = (last_name(&a), last_name(&b));
                assert_eq!(prefix(na), "NormalizedAverageColor");
                assert_eq!(prefix(nb), "DirectionalMaxComponent");
                if na.trim_start_matches("NormalizedAverageColor")
                    == nb.trim_start_matches("DirectionalMaxComponent")
                {
                    pairs += 1;
                }
                for v in t.scale_vectors[0] {
                    if v.is_finite() && t.scale_vectors[1].iter().any(|s| *s > 0.0) {
                        s0_dev = s0_dev.max((v - 1.0).abs());
                    }
                }
                for v in t.scale_vectors[1] {
                    s1_max = s1_max.max(v);
                }
            }
        }
    });
    eprintln!(
        "{names:?}; 2D light maps {checked}, same-atlas pairs {pairs}; lit coefficient-0 scale \
         max |s-1| {s0_dev}; coefficient-1 scale max {s1_max}; rectangles outside [0,1] \
         {outside:?}"
    );
    // Only instanced components (foliage), whose per-instance
    // `LightmapUVBias` completes the offset, leave the unit square.
    assert_eq!(
        outside,
        BTreeMap::from([("InstancedStaticMeshComponent", 2)])
    );
    assert_eq!(
        names.get("LightMapTexture2D NormalizedAverageColor"),
        Some(&2_721)
    );
    assert_eq!(
        names.get("LightMapTexture2D DirectionalMaxComponent"),
        Some(&2_721)
    );
    assert_eq!(
        names.get("ShadowMapTexture2D ShadowMapTexture2D"),
        Some(&219)
    );
    assert_eq!(names.len(), 3);
    assert_eq!(checked, 24_575);
    assert_eq!(pairs, checked);
}

fn guid_prop(props: &[asamu_ue3::Property], name: &str) -> Option<String> {
    props
        .iter()
        .find(|p| p.name == name)
        .and_then(|p| guid_of(&p.value))
}

/// `(T)`: light maps list the baked lights by the light component's
/// `LightmapGuid`; shadow maps name their light by its `LightGuid`. Streamed
/// sub-levels (`TheCore`, `Freds_place`) are lit by lights of the level that
/// streams them, so GUIDs are matched over all maps.
#[test]
fn light_guids_match_light_components() {
    let dir = require_data!();
    let mut lightmap_guids = BTreeSet::new();
    let mut light_guids = BTreeSet::new();
    let mut lm_refs: Vec<String> = Vec::new();
    let mut sm_refs: Vec<String> = Vec::new();
    let (mut lm_local, mut lights) = (0, 0);
    // (LightmapGuid, LightGuid) of every light component.
    let mut components: Vec<(Option<String>, Option<String>)> = Vec::new();
    for_each_package(&dir, true, |set, lp| {
        let pkg = &lp.package;
        let mut local = BTreeSet::new();
        for i in 0..pkg.exports.len() {
            let class = pkg.export_class_name(i).unwrap_or_default();
            if !class.ends_with("LightComponent") {
                continue;
            }
            let o = set.decode(lp, i).unwrap();
            if o.path.contains("Default__") {
                continue;
            }
            lights += 1;
            let lmg = guid_prop(&o.properties, "LightmapGuid");
            let lg = guid_prop(&o.properties, "LightGuid");
            if let Some(g) = &lmg {
                local.insert(g.clone());
                lightmap_guids.insert(g.clone());
            }
            if let Some(g) = &lg {
                light_guids.insert(g.clone());
            }
            components.push((lmg, lg));
        }
        for i in 0..pkg.exports.len() {
            let class = pkg.export_class_name(i).unwrap_or_default();
            if class == "ShadowMap2D" {
                let o = set.decode(lp, i).unwrap();
                if let Some(g) = shadow_map_2d_info(&o.properties).light_guid {
                    sm_refs.push(g);
                }
            }
            if lighting_layout(pkg, i).is_none() {
                continue;
            }
            let d = decode_lighting(pkg, Some(&lp.name), i, set).unwrap();
            if let LightingNative::ShadowMap1D(s) = &d.native {
                sm_refs.push(s.light_guid.to_string());
            }
            for m in d.native.light_maps() {
                for g in m.light_guids() {
                    let g = g.to_string();
                    lm_local += usize::from(local.contains(&g));
                    lm_refs.push(g);
                }
            }
        }
    });
    let lm_matched = lm_refs
        .iter()
        .filter(|g| lightmap_guids.contains(*g))
        .count();
    let lm_as_light_guid = lm_refs.iter().filter(|g| light_guids.contains(*g)).count();
    let sm_matched = sm_refs.iter().filter(|g| light_guids.contains(*g)).count();
    let used: BTreeSet<&String> = lm_refs.iter().collect();
    let baked = lightmap_guids.iter().filter(|g| used.contains(g)).count();
    let used_sm: BTreeSet<&String> = sm_refs.iter().collect();
    let shadowed = light_guids.iter().filter(|g| used_sm.contains(g)).count();
    // Per light component (a few components share their GUIDs with a copy
    // of the same light in another map).
    let baked_components = components
        .iter()
        .filter(|(g, _)| g.as_ref().is_some_and(|g| used.contains(g)))
        .count();
    let shadowed_components = components
        .iter()
        .filter(|(_, g)| g.as_ref().is_some_and(|g| used_sm.contains(g)))
        .count();
    let both = components
        .iter()
        .filter(|(a, b)| {
            a.as_ref().is_some_and(|g| used.contains(g))
                && b.as_ref().is_some_and(|g| used_sm.contains(g))
        })
        .count();
    eprintln!(
        "light components {lights}; light-map GUID refs {} (a LightmapGuid: {lm_matched}, in the \
         same package: {lm_local}, a LightGuid: {lm_as_light_guid}); shadow maps {} (a LightGuid: \
         {sm_matched}); distinct LightmapGuids baked into some light map {baked}, distinct \
         LightGuids shadow-mapped {shadowed}; light components baked {baked_components}, \
         shadow-mapped {shadowed_components}, both {both}",
        lm_refs.len(),
        sm_refs.len()
    );
    assert_eq!(lights, 3_437);
    assert_eq!(lm_refs.len(), 153_534);
    assert_eq!(lm_refs.len() - lm_matched, 20);
    assert_eq!(lm_as_light_guid, 0);
    assert_eq!(sm_refs.len(), 5_346);
    assert_eq!(sm_refs.len() - sm_matched, 1);
    assert_eq!(lightmap_guids.len(), 3_415);
    assert_eq!(baked, 3_220);
    assert_eq!(shadowed, 40);
    assert_eq!(baked_components, 3_242);
    assert_eq!(shadowed_components, 40);
    // A light is either baked into light maps or casts static shadow maps,
    // never both (what the runtime relies on when it stops baked lights from
    // lighting light-mapped surfaces).
    assert_eq!(both, 0);
}

/// `(T)`: a vertex light map has one sample per vertex of the matching LOD of
/// the component's static mesh, and its owner is the component itself.
#[test]
fn vertex_light_maps_match_mesh_vertices() {
    let dir = require_data!();
    let (mut checked, mut agree, mut own, mut resolvable) = (0, 0, 0, 0);
    for_each_package(&dir, true, |set, lp| {
        let pkg = &lp.package;
        for i in 0..pkg.exports.len() {
            if lighting_layout(pkg, i).is_none() {
                continue;
            }
            let d = decode_lighting(pkg, Some(&lp.name), i, set).unwrap();
            let LightingNative::StaticMesh(s) = &d.native else {
                continue;
            };
            let mesh_index = d.object.properties.iter().find_map(|p| match &p.value {
                Value::Object(o) if p.name == "StaticMesh" && o.index != 0 => Some(o.index),
                _ => None,
            });
            for (lod, info) in s.lods.iter().enumerate() {
                let LightMap::OneD(m) = &info.light_map else {
                    continue;
                };
                checked += 1;
                if m.owner.export_index() == Some(i) {
                    own += 1;
                }
                let Some(mi) = mesh_index.and_then(|x| asamu_ue3::PackageIndex(x).export_index())
                else {
                    continue;
                };
                let Ok(mesh) = decode_static_mesh(pkg, Some(&lp.name), mi, set) else {
                    continue;
                };
                resolvable += 1;
                let verts = mesh
                    .native
                    .lods
                    .get(lod)
                    .map(|l| l.positions.positions.len());
                if verts == Some(m.simple_samples.len()) {
                    agree += 1;
                }
            }
        }
    });
    eprintln!(
        "vertex light maps {checked}: owner is the component {own}; mesh in the same package \
         {resolvable}, sample count = mesh LOD vertex count {agree}"
    );
    assert_eq!(checked, 784);
    assert_eq!(own, checked);
    assert_eq!(resolvable, 682);
    assert_eq!(agree, resolvable);
}

/// `(T)`: `ShadowMap2D` has no native data: every export ends right after
/// its tagged properties.
#[test]
fn shadow_map_2d_is_tags_only() {
    let dir = require_data!();
    let (mut total, mut tags_only, mut with_texture) = (0, 0, 0);
    for_each_package(&dir, true, |set, lp| {
        let pkg = &lp.package;
        for i in 0..pkg.exports.len() {
            if pkg.export_class_name(i).unwrap_or_default() != "ShadowMap2D" {
                continue;
            }
            let o = set.decode(lp, i).unwrap();
            if o.path.contains("Default__") {
                continue;
            }
            total += 1;
            tags_only += usize::from(o.native_tail() == 0);
            with_texture += usize::from(shadow_map_2d_info(&o.properties).texture.is_some());
        }
    });
    eprintln!("ShadowMap2D exports {total}: tags only {tags_only}, with a texture {with_texture}");
    assert_eq!(total, 5_326);
    assert_eq!(tags_only, total);
}

/// Log-ratio statistics of `simple × scale2` against a combination of the
/// two directional coefficients, per colour channel, over every vertex
/// light map (which keeps both forms).
#[derive(Debug, Default)]
struct Fit {
    n: usize,
    sum: f64,
    sum_sq: f64,
}

impl Fit {
    fn add(&mut self, predicted: f64, actual: f64) {
        // Channels that are (nearly) black in either form carry no ratio.
        if predicted > 1e-3 && actual > 1e-3 {
            let l = (actual / predicted).ln();
            self.n += 1;
            self.sum += l;
            self.sum_sq += l * l;
        }
    }
    /// Geometric mean of `actual / predicted`.
    fn ratio(&self) -> f64 {
        (self.sum / self.n as f64).exp()
    }
    /// Standard deviation of the log ratio.
    fn spread(&self) -> f64 {
        let m = self.sum / self.n as f64;
        (self.sum_sq / self.n as f64 - m * m).max(0.0).sqrt()
    }
}

/// Reproduces the `LIGHTMAPS.md` evidence for the (TENTATIVE) combination
/// rule `coefficient0 × scale0 × mean_i(coefficient1_i × scale1_i)`: in the
/// vertex light maps, which keep the simple coefficient next to the two
/// directional ones, the mean reproduces the simple value without bias,
/// while a sum or a maximum of the three max components does not. Asserted
/// loosely; the printed numbers are the evidence.
#[test]
fn simple_coefficient_matches_the_mean_combination() {
    let dir = require_data!();
    let lin = |v: u8| f64::from(v) / 255.0;
    let srgb = |v: u8| f64::from(asamu_ue3::lightmap::srgb_to_linear(v));
    // [transfer][combination]: transfer 0 = bytes as linear, 1 = sRGB;
    // combination 0 = mean, 1 = sum, 2 = max.
    let mut fits: [[Fit; 3]; 2] = Default::default();
    let mut samples = 0usize;
    for_each_package(&dir, true, |set, lp| {
        let pkg = &lp.package;
        for i in 0..pkg.exports.len() {
            if lighting_layout(pkg, i).is_none() {
                continue;
            }
            let d = decode_lighting(pkg, Some(&lp.name), i, set).unwrap();
            for m in d.native.light_maps() {
                let LightMap::OneD(m) = m else { continue };
                let s = m.scale_vectors.map(|v| v.map(f64::from));
                for k in 0..m.simple_samples.len() {
                    let (Some(c0), Some(c1), Some(simple)) = (
                        m.directional_samples.color(k, 0),
                        m.directional_samples.color(k, 1),
                        m.simple_samples.color(k, 0),
                    ) else {
                        continue;
                    };
                    samples += 1;
                    for (t, conv) in [lin, srgb].iter().enumerate() {
                        // FColor bytes are B, G, R, A; channel ch = R, G, B.
                        let max: Vec<f64> = (0..3).map(|ch| conv(c1[2 - ch]) * s[1][ch]).collect();
                        let combined = [
                            max.iter().sum::<f64>() / 3.0,
                            max.iter().sum::<f64>(),
                            max.iter().copied().fold(0.0, f64::max),
                        ];
                        for (c, agg) in combined.iter().enumerate() {
                            for ch in 0..3 {
                                let predicted = conv(c0[2 - ch]) * s[0][ch] * agg;
                                let actual = conv(simple[2 - ch]) * s[2][ch];
                                fits[t][c].add(predicted, actual);
                            }
                        }
                    }
                }
            }
        }
    });
    for (t, name) in ["linear bytes", "sRGB"].iter().enumerate() {
        for (c, comb) in ["mean", "sum", "max"].iter().enumerate() {
            let f = &fits[t][c];
            eprintln!(
                "{name:12} {comb:4}: {} channel samples, geometric mean ratio {:.3}, log-ratio \
                 sd {:.3}",
                f.n,
                f.ratio(),
                f.spread()
            );
        }
    }
    eprintln!("vertex light map samples {samples}");
    assert!(samples > 200_000);
    for f in &fits {
        assert!((f[0].ratio() - 1.0).abs() < 0.2, "mean: {}", f[0].ratio());
        assert!(f[1].ratio() < 0.5, "sum: {}", f[1].ratio());
        assert!(f[2].ratio() < 0.9, "max: {}", f[2].ratio());
        assert!(f[0].spread() < 0.45 && f[0].spread() <= f[2].spread());
    }
    // Bytes taken as linear fit the vertex samples at least as well as sRGB.
    assert!(fits[0][0].spread() <= fits[1][0].spread());
}
