//! StaticMesh native-data decoding against the user's own installed game
//! (read-only).
//!
//! Reads `ASAMU_ORIGINAL_DIR` (the folder containing
//! `A Story About My Uncle.app`) or the default macOS Steam location and
//! skips cleanly when the data is absent. Only counts and structural facts are
//! asserted; nothing is copied or written.
//!
//! Acceptance test for `docs/reverse-engineering/MESHES.md`: every
//! `StaticMesh` export in every package decodes with its native tail consumed
//! exactly and passes every structural cross-check, and the decoded fields
//! have the documented meaning. `(T)` claims in MESHES.md are asserted here.
//!
//! Run with `-- --nocapture` to print the per-package coverage table and the
//! statistics quoted in MESHES.md.

#![allow(clippy::unwrap_used)]

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use asamu_ue3::model::{LoadedPackage, PackageSet};
use asamu_ue3::staticmesh::{
    LodModel, MeshCoverage, PackedNormal, StaticMesh, decode_static_mesh, is_static_mesh,
    mesh_coverage,
};

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
fn for_each_package(dir: &Path, mut f: impl FnMut(&PackageSet, &LoadedPackage)) {
    for path in packages(dir) {
        let set = PackageSet::new(&[dir.to_path_buf(), dir.join("Maps")]);
        let lp = set.open_file(&path).unwrap();
        f(&set, &lp);
    }
}

#[test]
fn every_static_mesh_decodes_exactly_and_validates() {
    let dir = require_data!();
    let mut covs: Vec<MeshCoverage> = Vec::new();
    for_each_package(&dir, |set, lp| covs.push(mesh_coverage(lp, set)));
    let (mut total, mut exact, mut valid, mut round_trip, mut bytes) = (0, 0, 0, 0, 0u64);
    let mut lods = BTreeMap::new();
    let mut with_meshes = 0;
    for c in &covs {
        if c.total == 0 {
            continue;
        }
        with_meshes += 1;
        eprintln!(
            "{:20} meshes {:4} exact {:4} valid {:4} rt {:4} lods {:?} sections {:4} verts {:7} tris {:7} \
             coll {:7} colors {} uv {:?} fp {} body {:3} src {} adj {:3} raw {:?} tail {}",
            c.package,
            c.total,
            c.exact,
            c.valid,
            c.round_trip,
            c.lod_histogram,
            c.sections,
            c.vertices,
            c.triangles,
            c.collision_triangles,
            c.with_colors,
            c.uv_channels,
            c.full_precision_uv_lods,
            c.with_body_setup,
            c.with_source_data,
            c.with_adjacency,
            c.raw_triangle_flags,
            c.native_bytes
        );
        for f in &c.failures {
            eprintln!("  FAIL {f}");
        }
        for f in &c.issues {
            eprintln!("  ISSUE {f}");
        }
        total += c.total;
        exact += c.exact;
        valid += c.valid;
        round_trip += c.round_trip;
        bytes += c.native_bytes;
        for (k, v) in &c.lod_histogram {
            *lods.entry(*k).or_insert(0) += v;
        }
    }
    eprintln!(
        "TOTAL packages {with_meshes} meshes {total} exact {exact} valid {valid} \
         round-trip {round_trip} native bytes {bytes} lods {lods:?}"
    );
    // (T) 1,512 StaticMesh exports in 14 packages; all consume exactly and validate.
    assert_eq!(with_meshes, 14);
    assert_eq!(total, 1512);
    assert_eq!(exact, total, "every StaticMesh must decode exactly");
    assert_eq!(valid, total, "every StaticMesh must pass the cross-checks");
    // (T) Re-encoding the decoded fields reproduces every native tail byte for byte.
    assert_eq!(round_trip, total, "every StaticMesh must round-trip");
    assert_eq!(lods.values().sum::<usize>(), total);
}

fn sub(a: [f32; 3], b: [f32; 3]) -> [f64; 3] {
    std::array::from_fn(|k| f64::from(a[k]) - f64::from(b[k]))
}
fn cross(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}
fn dot(a: [f64; 3], b: [f64; 3]) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}
fn len(a: [f64; 3]) -> f64 {
    dot(a, a).sqrt()
}
fn unpack3(n: PackedNormal) -> ([f64; 3], f64) {
    let u = n.unpack();
    (
        [f64::from(u[0]), f64::from(u[1]), f64::from(u[2])],
        f64::from(u[3]),
    )
}

/// Triangles of `lod`'s sections with collision enabled (`EnableCollision`,
/// or `OldEnableCollision` when `old`), as (indices, section).
fn collision_enabled_triangles(lod: &LodModel, old: bool) -> Vec<([u16; 3], u16)> {
    let mut out = Vec::new();
    for (si, sec) in lod.sections.iter().enumerate() {
        let flag = if old {
            sec.old_enable_collision
        } else {
            sec.enable_collision
        };
        if flag == 0 {
            continue;
        }
        let first = sec.first_index as usize;
        for t in 0..sec.num_triangles as usize {
            let i = first + 3 * t;
            out.push((
                [lod.indices[i], lod.indices[i + 1], lod.indices[i + 2]],
                si as u16,
            ));
        }
    }
    out
}

fn bump<K: Ord>(m: &mut BTreeMap<K, usize>, k: K) {
    *m.entry(k).or_insert(0) += 1;
}

#[derive(Default, Debug)]
struct Semantics {
    meshes: usize,
    lods: usize,
    vertices: u64,
    // Packed normals.
    normal_unit: u64,
    normal_zero: u64,
    tangent_unit: u64,
    orthogonal: u64,
    normal_w_byte: BTreeMap<u8, u64>,
    tangent_x_w_byte: BTreeMap<u8, u64>,
    // Winding and tangent frame.
    face_vs_normal_pos: u64,
    face_vs_normal_neg: u64,
    uv_tangent_pos: u64,
    uv_tangent_neg: u64,
    bitangent_sign_agree: u64,
    bitangent_sign_disagree: u64,
    // UVs.
    uv_values: u64,
    uv_in_range: u64,
    uv_non_finite: u64,
    uv_channels: BTreeMap<u32, usize>,
    full_precision_lods: usize,
    // Collision.
    collision_set_equal_render: usize,
    collision_multiset_equal_render: usize,
    collision_with_repeats: usize,
    collision_set_equal_old_flag: usize,
    meshes_without_collision: usize,
    kdop_nodes_pow2: usize,
    // Node-byte hypotheses (all rejected; printed for MESHES.md).
    kdop_nodes: u64,
    kdop_nodes_min_le_max: u64,
    kdop_nodes_sum_le_255: u64,
    kdop_nodes_pairs_ordered: u64,
    meshes_with_coarse_lod_outside_bounds: usize,
    kdop_root_contains_lod0: usize,
    // Index buffers.
    wireframe_empty_lods: usize,
    adjacency_lods: usize,
    // Sections.
    sections: usize,
    fragments_single_cover: usize,
    material_index_eq_section: usize,
    enable_collision: BTreeMap<(u32, u32), usize>,
    shadow: BTreeMap<u32, usize>,
    material_classes: BTreeMap<String, usize>,
    // Raw triangles.
    raw_triangles_empty: usize,
    // Colors.
    color_lods: usize,
    // Mesh-level fields.
    body_classes: BTreeMap<String, usize>,
    internal_version: BTreeMap<i32, usize>,
    lod_info_equals_lods: usize,
    streaming_factor_len: BTreeMap<usize, usize>,
    remove_degenerates: BTreeMap<u32, usize>,
    per_lod_flag: BTreeMap<u32, usize>,
    prealloc: BTreeMap<i32, usize>,
    opt_settings_len: BTreeMap<usize, usize>,
    simplified: BTreeMap<u32, usize>,
    proxy: BTreeMap<u32, usize>,
    source_data: usize,
    high_res_empty: usize,
    lighting_guid_zero: usize,
    vertex_position_version_max: i32,
    // Tagged lightmap properties.
    lightmap_index_values: BTreeMap<i32, usize>,
    lightmap_index_beyond_uvs: usize,
    lightmap_res_tagged: usize,
}

fn analyse(s: &mut Semantics, lp: &LoadedPackage, m: &StaticMesh, unique: &mut BTreeSet<String>) {
    let n = &m.native;
    let pkg = &lp.package;
    s.meshes += 1;
    unique.insert(m.object.path.to_ascii_lowercase());
    bump(&mut s.internal_version, n.internal_version);
    if usize::try_from(n.lod_info_count).ok() == Some(n.lods.len()) {
        s.lod_info_equals_lods += 1;
    }
    s.vertex_position_version_max = s.vertex_position_version_max.max(n.vertex_position_version);
    bump(
        &mut s.streaming_factor_len,
        n.cached_streaming_texture_factors.len(),
    );
    bump(&mut s.remove_degenerates, n.remove_degenerates);
    bump(
        &mut s.per_lod_flag,
        n.per_lod_static_lighting_for_instancing,
    );
    bump(&mut s.prealloc, n.console_prealloc_instance_count);
    bump(&mut s.opt_settings_len, n.optimization_settings.len());
    bump(&mut s.simplified, n.has_been_simplified);
    bump(&mut s.proxy, n.is_mesh_proxy);
    if n.source_data.is_some() {
        s.source_data += 1;
    }
    if n.high_res_source_mesh_name.is_empty() {
        s.high_res_empty += 1;
    }
    if n.lighting_guid.is_zero() {
        s.lighting_guid_zero += 1;
    }
    if !n.body_setup.is_null() {
        bump(
            &mut s.body_classes,
            pkg.class_name(n.body_setup).unwrap_or_default(),
        );
    }
    if let Some(v) = m.light_map_coordinate_index() {
        bump(&mut s.lightmap_index_values, v);
        if n.lods
            .first()
            .is_some_and(|l| u32::try_from(v).map_or(true, |v| v >= l.vertices.num_tex_coords))
        {
            s.lightmap_index_beyond_uvs += 1;
        }
    }
    if m.light_map_resolution().is_some() {
        s.lightmap_res_tagged += 1;
    }
    if n.kdop.nodes.is_empty() || n.kdop.nodes.len().is_power_of_two() {
        s.kdop_nodes_pow2 += 1;
    }
    for node in &n.kdop.nodes {
        let b = node.bytes;
        s.kdop_nodes += 1;
        if (0..3).all(|k| b[k] <= b[k + 3]) {
            s.kdop_nodes_min_le_max += 1;
        }
        if (0..3).all(|k| u16::from(b[k]) + u16::from(b[k + 3]) <= 255) {
            s.kdop_nodes_sum_le_255 += 1;
        }
        if b[0] <= b[1] && b[2] <= b[3] && b[4] <= b[5] {
            s.kdop_nodes_pairs_ordered += 1;
        }
    }
    // Coarser LODs against LOD 0's bounds (same tolerance as the validator).
    let bo = n.bounds;
    let outside = n.lods.iter().skip(1).any(|lod| {
        lod.positions.positions.iter().any(|p| {
            (0..3).any(|k| {
                let tol = 1e-2 + 1e-5 * (bo.origin[k].abs() + bo.box_extent[k].abs());
                p[k] < bo.origin[k] - bo.box_extent[k] - tol
                    || p[k] > bo.origin[k] + bo.box_extent[k] + tol
            })
        })
    });
    if outside {
        s.meshes_with_coarse_lod_outside_bounds += 1;
    }
    let l0 = &n.lods[0];
    // Collision triangles = LOD 0 triangles of collision-enabled sections.
    let mut render = collision_enabled_triangles(l0, false);
    let mut by_old = collision_enabled_triangles(l0, true);
    by_old.sort_unstable();
    by_old.dedup();
    let mut coll: Vec<([u16; 3], u16)> = n
        .kdop
        .triangles
        .iter()
        .map(|t| (t.vertices, t.material_index))
        .collect();
    render.sort_unstable();
    coll.sort_unstable();
    if render == coll {
        s.collision_multiset_equal_render += 1;
    }
    let coll_len = coll.len();
    render.dedup();
    coll.dedup();
    if render == coll {
        s.collision_set_equal_render += 1;
    }
    if by_old == coll {
        s.collision_set_equal_old_flag += 1;
    }
    if coll.len() != coll_len {
        s.collision_with_repeats += 1;
    }
    if n.kdop.triangles.is_empty() {
        s.meshes_without_collision += 1;
    } else {
        let mut lo = [f32::MAX; 3];
        let mut hi = [f32::MIN; 3];
        for p in &l0.positions.positions {
            for k in 0..3 {
                lo[k] = lo[k].min(p[k]);
                hi[k] = hi[k].max(p[k]);
            }
        }
        let rb = n.kdop.root_bounds;
        if (0..3).all(|k| rb.min[k] <= lo[k] + 1.0 && rb.max[k] >= hi[k] - 1.0) {
            s.kdop_root_contains_lod0 += 1;
        }
    }
    for lod in &n.lods {
        analyse_lod(s, pkg, lod);
    }
}

fn analyse_lod(s: &mut Semantics, pkg: &asamu_ue3::Package, lod: &LodModel) {
    s.lods += 1;
    if lod.colors.num_vertices != 0 {
        s.color_lods += 1;
    }
    if lod.raw_triangles.element_count == 0 && lod.raw_triangles.size_on_disk == 0 {
        s.raw_triangles_empty += 1;
    }
    if lod.wireframe_indices.is_empty() {
        s.wireframe_empty_lods += 1;
    }
    if !lod.adjacency_indices.is_empty() {
        s.adjacency_lods += 1;
    }
    bump(&mut s.uv_channels, lod.vertices.num_tex_coords);
    if lod.vertices.full_precision_uvs {
        s.full_precision_lods += 1;
    }
    for (si, sec) in lod.sections.iter().enumerate() {
        s.sections += 1;
        let class = if sec.material.is_null() {
            "(null)".to_owned()
        } else {
            pkg.class_name(sec.material).unwrap_or_default()
        };
        bump(&mut s.material_classes, class);
        bump(
            &mut s.enable_collision,
            (sec.enable_collision, sec.old_enable_collision),
        );
        bump(&mut s.shadow, sec.enable_shadow_casting);
        if sec.fragments.len() == 1
            && sec.fragments[0].base_index as u32 == sec.first_index
            && sec.fragments[0].num_primitives as u32 == sec.num_triangles
        {
            s.fragments_single_cover += 1;
        }
        if usize::try_from(sec.material_index).ok() == Some(si) {
            s.material_index_eq_section += 1;
        }
    }
    let vb = &lod.vertices;
    for v in 0..vb.tangent_z.len() {
        s.vertices += 1;
        let (z, _) = unpack3(vb.tangent_z[v]);
        let (x, _) = unpack3(vb.tangent_x[v]);
        if len(z) < 0.5 {
            s.normal_zero += 1;
        }
        if (len(z) - 1.0).abs() < 0.02 {
            s.normal_unit += 1;
        }
        if (len(x) - 1.0).abs() < 0.02 {
            s.tangent_unit += 1;
        }
        if dot(x, z).abs() < 0.05 {
            s.orthogonal += 1;
        }
        *s.normal_w_byte.entry(vb.tangent_z[v].0[3]).or_insert(0) += 1;
        *s.tangent_x_w_byte.entry(vb.tangent_x[v].0[3]).or_insert(0) += 1;
        for ch in &vb.uvs {
            for c in ch[v] {
                s.uv_values += 1;
                if !c.is_finite() {
                    s.uv_non_finite += 1;
                } else if c.abs() <= 64.0 {
                    s.uv_in_range += 1;
                }
            }
        }
    }
    let pos = &lod.positions.positions;
    let uv0 = vb.uvs.first();
    for t in lod.indices.as_chunks::<3>().0 {
        let (ia, ib, ic) = (t[0] as usize, t[1] as usize, t[2] as usize);
        let e1 = sub(pos[ib], pos[ia]);
        let e2 = sub(pos[ic], pos[ia]);
        let face = cross(e1, e2);
        if len(face) < 1e-6 {
            continue;
        }
        let mut vn = [0.0; 3];
        for &i in &[ia, ib, ic] {
            let (z, _) = unpack3(vb.tangent_z[i]);
            for k in 0..3 {
                vn[k] += z[k];
            }
        }
        let d = dot(face, vn) / len(face) / len(vn).max(1e-9);
        if d > 0.5 {
            s.face_vs_normal_pos += 1;
        } else if d < -0.5 {
            s.face_vs_normal_neg += 1;
        }
        let Some(uv) = uv0 else { continue };
        let (a, b, c) = (uv[ia], uv[ib], uv[ic]);
        let du1 = f64::from(b[0] - a[0]);
        let dv1 = f64::from(b[1] - a[1]);
        let du2 = f64::from(c[0] - a[0]);
        let dv2 = f64::from(c[1] - a[1]);
        let det = du1 * dv2 - du2 * dv1;
        if det.abs() < 1e-8 {
            continue;
        }
        let r = 1.0 / det;
        let tan: [f64; 3] = std::array::from_fn(|k| (e1[k] * dv2 - e2[k] * dv1) * r);
        let bit: [f64; 3] = std::array::from_fn(|k| (e2[k] * du1 - e1[k] * du2) * r);
        for &i in &[ia, ib, ic] {
            let (x, _) = unpack3(vb.tangent_x[i]);
            let (z, w) = unpack3(vb.tangent_z[i]);
            let dt = dot(tan, x) / len(tan).max(1e-12);
            if dt > 0.5 {
                s.uv_tangent_pos += 1;
            } else if dt < -0.5 {
                s.uv_tangent_neg += 1;
            }
            let y = cross(z, x).map(|c| c * w.signum());
            let db = dot(bit, y) / len(bit).max(1e-12);
            if db > 0.5 {
                s.bitangent_sign_agree += 1;
            } else if db < -0.5 {
                s.bitangent_sign_disagree += 1;
            }
        }
    }
}

/// Semantic checks of the decoded fields (vertex formats, winding, tangent
/// convention, collision/render relationship, constant fields). Prints the
/// statistics quoted in MESHES.md and asserts the claims marked (T) there.
#[test]
fn decoded_fields_have_the_documented_meaning() {
    let dir = require_data!();
    let mut s = Semantics::default();
    let mut unique = BTreeSet::new();
    for_each_package(&dir, |set, lp| {
        for i in 0..lp.package.exports.len() {
            if !is_static_mesh(&lp.package, i) {
                continue;
            }
            let m = decode_static_mesh(&lp.package, Some(&lp.name), i, set).unwrap();
            analyse(&mut s, lp, &m, &mut unique);
        }
    });
    eprintln!("{s:#?}");
    eprintln!("distinct mesh paths: {}", unique.len());
    let n = s.meshes;
    assert_eq!(n, 1512);
    assert_eq!(unique.len(), 882);
    // Packed normals: unit TangentX/TangentZ, orthogonal; Z.W is a pure sign.
    assert!(s.normal_unit + s.normal_zero >= s.vertices * 99 / 100);
    assert!(s.tangent_unit >= s.vertices * 99 / 100);
    assert!(s.orthogonal >= s.vertices * 97 / 100);
    assert_eq!(
        s.normal_w_byte.keys().copied().collect::<Vec<_>>(),
        vec![0, 255]
    );
    assert!(s.tangent_x_w_byte.keys().all(|&b| b == 127 || b == 128));
    // Winding: cross(b - a, c - a) points away from the vertex normal.
    assert!(s.face_vs_normal_neg > 1000 * s.face_vs_normal_pos);
    // Tangent frame: TangentX follows dP/du, bitangent = cross(Z, X) * sign(Z.W).
    assert!(s.uv_tangent_pos > 100 * s.uv_tangent_neg);
    assert!(s.bitangent_sign_agree > 100 * s.bitangent_sign_disagree);
    // UVs: half floats only, finite, almost all within +-64.
    assert_eq!(s.uv_non_finite, 0);
    assert_eq!(s.full_precision_lods, 0);
    assert!(s.uv_in_range * 100_000 >= s.uv_values * 99_999);
    // Collision: kDOP triangles are LOD 0's collision-enabled triangles.
    assert_eq!(s.collision_set_equal_render, n);
    assert_eq!(s.collision_multiset_equal_render, n - 2);
    assert_eq!(s.collision_with_repeats, 2);
    // Selecting by OldEnableCollision fails exactly on the meshes with a (0, 1) section.
    assert_eq!(s.collision_set_equal_old_flag, n - 4);
    assert_eq!(s.enable_collision.get(&(0, 1)), Some(&4));
    assert_eq!(s.kdop_nodes_pow2, n);
    // Index buffers: wireframe always empty in the cooked data.
    assert_eq!(s.wireframe_empty_lods, s.lods);
    // Sections: one fragment covering the section; MaterialIndex = position.
    assert_eq!(s.fragments_single_cover, s.sections);
    assert_eq!(s.material_index_eq_section, s.sections);
    // Raw triangles are stripped (empty inline record) in every LOD.
    assert_eq!(s.raw_triangles_empty, s.lods);
    // Constant mesh-level fields.
    assert_eq!(s.internal_version.get(&18), Some(&n));
    assert_eq!(s.lod_info_equals_lods, n);
    assert_eq!(s.streaming_factor_len.get(&4), Some(&n));
    assert_eq!(s.opt_settings_len.get(&0), Some(&n));
    assert_eq!(s.simplified.get(&0), Some(&n));
    assert_eq!(s.proxy.get(&0), Some(&n));
    assert_eq!(s.per_lod_flag.get(&0), Some(&n));
    assert_eq!(s.prealloc.get(&0), Some(&n));
    assert_eq!(s.source_data, 0);
    assert_eq!(s.high_res_empty, n);
    assert_eq!(s.lighting_guid_zero, 0);
    // References resolve to the expected classes.
    assert!(s.body_classes.keys().all(|k| k == "RB_BodySetup"));
    assert!(s.material_classes.keys().all(|k| matches!(
        k.as_str(),
        "(null)" | "Material" | "MaterialInstanceConstant"
    )));
}
