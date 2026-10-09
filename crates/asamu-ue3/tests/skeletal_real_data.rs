//! SkeletalMesh native-data decoding against the user's own installed game
//! (read-only).
//!
//! Reads `ASAMU_ORIGINAL_DIR` (the folder containing
//! `A Story About My Uncle.app`) or the default macOS Steam location and
//! skips cleanly when the data is absent. Only counts and structural facts are
//! asserted; nothing is copied or written.
//!
//! Acceptance test for `docs/reverse-engineering/SKELETAL.md`: every
//! `SkeletalMesh` export in every package decodes with its native tail
//! consumed exactly, re-encodes byte for byte and passes every structural
//! cross-check; the decoded fields have the documented meaning. `(T)` claims
//! in SKELETAL.md are asserted here.
//!
//! Run with `-- --nocapture` to print the statistics quoted in SKELETAL.md.

#![allow(clippy::unwrap_used)]

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use asamu_ue3::model::{LoadedPackage, PackageSet};
use asamu_ue3::skeletal::{
    SkeletalCoverage, SkeletalMesh, bone_names, decode_skeletal_mesh, is_skeletal_mesh,
    skeletal_coverage,
};
use asamu_ue3::staticmesh::{PackedNormal, f32_to_half, half_to_f32};
use asamu_ue3::{PackageIndex, Value, decode_object};

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

fn for_each_package(dir: &Path, mut f: impl FnMut(&PackageSet, &LoadedPackage)) {
    for path in packages(dir) {
        let set = PackageSet::new(&[dir.to_path_buf(), dir.join("Maps")]);
        let lp = set.open_file(&path).unwrap();
        f(&set, &lp);
    }
}

#[test]
fn every_skeletal_mesh_decodes_exactly_and_validates() {
    let dir = require_data!();
    let mut covs: Vec<SkeletalCoverage> = Vec::new();
    for_each_package(&dir, |set, lp| covs.push(skeletal_coverage(lp, set)));
    let (mut total, mut exact, mut valid, mut round_trip, mut bytes) = (0, 0, 0, 0, 0u64);
    let mut with_meshes = 0;
    let mut lods = BTreeMap::new();
    let (mut bones, mut sections, mut chunks, mut vertices, mut triangles) = (0, 0, 0, 0u64, 0u64);
    for c in &covs {
        if c.total == 0 {
            continue;
        }
        with_meshes += 1;
        eprintln!(
            "{:18} meshes {:2} exact {:2} valid {:2} rt {:2} lods {:?} bones {:4} sections {:3} chunks {:3} \
             verts {:6} tris {:6} uv {:?} idx {:?} adj {} srcv {} colors {} packedflag {} infl {} perpoly {} \
             src {} tail {}",
            c.package,
            c.total,
            c.exact,
            c.valid,
            c.round_trip,
            c.lod_histogram,
            c.bones,
            c.sections,
            c.chunks,
            c.vertices,
            c.triangles,
            c.uv_channels,
            c.index_sizes,
            c.with_adjacency,
            c.chunks_with_source_vertices,
            c.with_colors,
            c.packed_position_flag_lods,
            c.with_vertex_influences,
            c.with_per_poly,
            c.with_source_data,
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
        bones += c.bones;
        sections += c.sections;
        chunks += c.chunks;
        vertices += c.vertices;
        triangles += c.triangles;
        for (k, v) in &c.lod_histogram {
            *lods.entry(*k).or_insert(0) += v;
        }
    }
    eprintln!(
        "TOTAL packages {with_meshes} meshes {total} exact {exact} valid {valid} round-trip {round_trip} \
         native bytes {bytes} lods {lods:?} bones {bones} sections {sections} chunks {chunks} \
         vertices {vertices} triangles {triangles}"
    );
    // (T) 28 SkeletalMesh exports in 5 packages; all consume exactly, validate
    // and re-encode byte for byte.
    assert_eq!(with_meshes, 5);
    assert_eq!(total, 28);
    assert_eq!(exact, total, "every SkeletalMesh must decode exactly");
    assert_eq!(
        valid, total,
        "every SkeletalMesh must pass the cross-checks"
    );
    assert_eq!(round_trip, total, "every SkeletalMesh must round-trip");
    assert_eq!(lods.values().sum::<usize>(), total);
    let c: BTreeMap<&str, (usize, u64)> = covs
        .iter()
        .filter(|c| c.total > 0)
        .map(|c| (c.package.as_str(), (c.total, c.native_bytes)))
        .collect();
    assert_eq!(c.get("Startup").map(|v| v.0), Some(18));
    assert_eq!(c.get("AG-StarHaven").map(|v| v.0), Some(6));
    assert_eq!(c.get("AG-BeautifulCity").map(|v| v.0), Some(2));
    assert_eq!(c.get("AG-Darkcave").map(|v| v.0), Some(1));
    assert_eq!(c.get("TheCore").map(|v| v.0), Some(1));
    // (T) No mesh has vertex colors, alternative influences, per-poly
    // collision or source data; every index buffer is 16-bit.
    for c in covs.iter().filter(|c| c.total > 0) {
        assert_eq!(c.with_colors, 0);
        assert_eq!(c.with_vertex_influences, 0);
        assert_eq!(c.with_per_poly, 0);
        assert_eq!(c.with_source_data, 0);
        assert_eq!(c.full_precision_uv_lods, 0);
        assert!(c.index_sizes.keys().all(|&k| k == 2));
    }
}

/// Position, influence bones, influence weights, UV 0 and normal of a
/// source (rigid or soft) vertex.
type SourceVertex = ([f32; 3], [u8; 4], [u8; 4], [f32; 2], PackedNormal);

fn bump<K: Ord>(m: &mut BTreeMap<K, u64>, k: K) {
    *m.entry(k).or_insert(0) += 1;
}

fn unpack(n: PackedNormal) -> [f64; 4] {
    n.unpack().map(f64::from)
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

/// Rotate `v` by unit quaternion `q` (`q v q*`).
fn rotate(q: [f64; 4], v: [f64; 3]) -> [f64; 3] {
    let u = [q[0], q[1], q[2]];
    let w = q[3];
    let t = cross(u, v).map(|c| 2.0 * c);
    let c2 = cross(u, t);
    std::array::from_fn(|k| v[k] + w * t[k] + c2[k])
}
fn qmul(a: [f64; 4], b: [f64; 4]) -> [f64; 4] {
    [
        a[3] * b[0] + a[0] * b[3] + a[1] * b[2] - a[2] * b[1],
        a[3] * b[1] - a[0] * b[2] + a[1] * b[3] + a[2] * b[0],
        a[3] * b[2] + a[0] * b[1] - a[1] * b[0] + a[2] * b[3],
        a[3] * b[3] - a[0] * b[0] - a[1] * b[1] - a[2] * b[2],
    ]
}

/// Component-space bone positions/rotations of the reference pose, with the
/// stored quaternions (`conj = false`) or their conjugates.
fn component_space(m: &SkeletalMesh, conj: bool) -> Vec<([f64; 3], [f64; 4])> {
    let mut out: Vec<([f64; 3], [f64; 4])> = Vec::new();
    for (i, b) in m.native.ref_skeleton.iter().enumerate() {
        let mut q = b.orientation.map(f64::from);
        if conj {
            q = [-q[0], -q[1], -q[2], q[3]];
        }
        let t = b.position.map(f64::from);
        if i == 0 {
            out.push((t, q));
        } else {
            let (pt, pq) = out[b.parent_index as usize];
            let rt = rotate(pq, t);
            out.push(([pt[0] + rt[0], pt[1] + rt[1], pt[2] + rt[2]], qmul(pq, q)));
        }
    }
    out
}

/// `x` converted to a half by truncating the mantissa (subnormals flush to
/// zero, overflow saturates), then widened back.
fn half_truncated(x: f32) -> f32 {
    let bits = x.to_bits();
    let sign = (bits >> 16) & 0x8000;
    let exp = ((bits >> 23) & 0xff) as i32 - 127 + 15;
    let h = if exp <= 0 {
        sign
    } else if exp >= 31 {
        sign | 0x7bff
    } else {
        sign | ((exp as u32) << 10) | ((bits >> 13) & 0x3ff)
    };
    half_to_f32(h as u16)
}

fn median(v: &mut [f64]) -> f64 {
    if v.is_empty() {
        return 0.0;
    }
    v.sort_by(f64::total_cmp);
    v[v.len() / 2]
}

#[derive(Default, Debug)]
struct Semantics {
    meshes: usize,
    unique_paths: BTreeSet<String>,
    vertices: u64,
    weights_sum_255: u64,
    alt_bytes_sum_255: u64,
    weights_descending: u64,
    influence_count: BTreeMap<usize, u64>,
    max_influences_field: BTreeMap<i32, u64>,
    max_influences_exceeded: u64,
    normal_unit: u64,
    tangent_unit: u64,
    orthogonal: u64,
    normal_w_byte: BTreeMap<u8, u64>,
    tangent_w_byte: BTreeMap<u8, u64>,
    face_against_normal: u64,
    face_along_normal: u64,
    source_vertex_chunks: u64,
    gpu_matches_source_position: u64,
    gpu_matches_source_influences: u64,
    gpu_matches_source_uv0: u64,
    uv0_round_nearest: u64,
    uv0_truncated: u64,
    gpu_matches_source_normal: u64,
    source_vertices: u64,
    rigid_vertices: u64,
    raw_point_count_eq_vertices: u64,
    raw_point_empty: u64,
    raw_point_flags: BTreeMap<String, u64>,
    lod_size_zero: u64,
    lod_size_40_per_vertex: u64,
    lods: u64,
    active_eq_required: u64,
    required_sorted: u64,
    bone_flags: BTreeMap<u32, u64>,
    origin_zero: u64,
    rot_origin: BTreeMap<String, u64>,
    name_index_map_full: u64,
    name_index_map_empty: u64,
    triangle_sorting: BTreeMap<u8, u64>,
    section_material_eq_index: u64,
    sections: u64,
    sections_eq_chunks: u64,
    num_tex_coords: BTreeMap<u32, u64>,
    clothing_assets_len: BTreeMap<usize, u64>,
    streaming_factors_len: BTreeMap<usize, u64>,
    bone_break: u64,
    bounds_tight: u64,
    // Quaternion convention: median distance of a vertex to its dominant bone.
    median_dist_stored: Vec<f64>,
    median_dist_conj: Vec<f64>,
    // Sockets.
    sockets: u64,
    sockets_bone_found: u64,
}

fn analyse(s: &mut Semantics, lp: &LoadedPackage, set: &PackageSet, m: &SkeletalMesh) {
    let n = &m.native;
    let pkg = &lp.package;
    s.meshes += 1;
    s.unique_paths.insert(m.object.path.to_ascii_lowercase());
    let names = bone_names(pkg, n);
    for b in &n.ref_skeleton {
        bump(&mut s.bone_flags, b.flags);
    }
    if n.origin == [0.0; 3] {
        s.origin_zero += 1;
    }
    bump(&mut s.rot_origin, format!("{:?}", n.rot_origin));
    if n.name_index_map.len() == n.ref_skeleton.len() {
        s.name_index_map_full += 1;
    }
    if n.name_index_map.is_empty() {
        s.name_index_map_empty += 1;
    }
    bump(&mut s.clothing_assets_len, n.clothing_assets.len());
    bump(
        &mut s.streaming_factors_len,
        n.cached_streaming_texture_factors.len(),
    );
    if !n.bone_break_names.is_empty() || !n.bone_break_options.is_empty() {
        s.bone_break += 1;
    }
    // Bounds vs LOD 0 vertex box.
    if let Some(lod0) = n.lods.first() {
        let mut lo = [f32::INFINITY; 3];
        let mut hi = [f32::NEG_INFINITY; 3];
        for v in &lod0.vertex_buffer.vertices {
            for k in 0..3 {
                lo[k] = lo[k].min(v.position[k]);
                hi[k] = hi[k].max(v.position[k]);
            }
        }
        let b = &n.bounds;
        let tight = (0..3).all(|k| {
            (b.origin[k] - b.box_extent[k] - lo[k]).abs() < 0.5
                && (b.origin[k] + b.box_extent[k] - hi[k]).abs() < 0.5
        });
        if tight {
            s.bounds_tight += 1;
        }
    }
    // Quaternion convention.
    for (conj, out) in [
        (false, &mut s.median_dist_stored),
        (true, &mut s.median_dist_conj),
    ] {
        let cs = component_space(m, conj);
        let mut d = Vec::new();
        if let Some(lod0) = n.lods.first() {
            for c in &lod0.chunks {
                let lo = c.base_vertex_index as usize;
                let hi = lo + c.vertex_count().unwrap() as usize;
                for v in &lod0.vertex_buffer.vertices[lo..hi] {
                    let k = (0..4).max_by_key(|&k| v.influence_weights[k]).unwrap();
                    let bone = c.bone_map[v.influence_bones[k] as usize] as usize;
                    let p = v.position.map(f64::from);
                    let bp = cs[bone].0;
                    d.push(len([p[0] - bp[0], p[1] - bp[1], p[2] - bp[2]]));
                }
            }
        }
        out.push(median(&mut d));
    }
    // Sockets.
    for sref in m.socket_refs() {
        s.sockets += 1;
        let Some(e) = PackageIndex(sref.index).export_index() else {
            continue;
        };
        let Ok(obj) = decode_object(pkg, Some(&lp.name), e, set) else {
            continue;
        };
        let bone = obj.properties.iter().find_map(|p| match &p.value {
            Value::Name(b) if p.name.eq_ignore_ascii_case("BoneName") => Some(b.clone()),
            _ => None,
        });
        if bone.is_some_and(|b| names.iter().any(|n| n.eq_ignore_ascii_case(&b))) {
            s.sockets_bone_found += 1;
        }
    }
    for lod in &n.lods {
        s.lods += 1;
        let vb = &lod.vertex_buffer;
        bump(&mut s.num_tex_coords, lod.num_tex_coords);
        if lod.size == 0 {
            s.lod_size_zero += 1;
        } else if u64::from(lod.size) == u64::from(lod.num_vertices) * 40 {
            s.lod_size_40_per_vertex += 1;
        }
        let rec = &lod.raw_point_indices_record;
        bump(&mut s.raw_point_flags, format!("{:#x}", rec.flags));
        if u32::try_from(lod.raw_point_indices.len()).ok() == Some(lod.num_vertices) {
            s.raw_point_count_eq_vertices += 1;
        } else if lod.raw_point_indices.is_empty() && rec.element_count == 0 {
            s.raw_point_empty += 1;
        }
        let active: Vec<u16> = lod.active_bone_indices.clone();
        let required: Vec<u16> = lod.required_bones.iter().map(|&b| u16::from(b)).collect();
        if active == required {
            s.active_eq_required += 1;
        }
        if required.windows(2).all(|w| w[0] < w[1]) {
            s.required_sorted += 1;
        }
        if lod.sections.len() == lod.chunks.len() {
            s.sections_eq_chunks += 1;
        }
        for (si, sec) in lod.sections.iter().enumerate() {
            s.sections += 1;
            bump(&mut s.triangle_sorting, sec.triangle_sorting);
            if usize::from(sec.material_index) == si {
                s.section_material_eq_index += 1;
            }
        }
        for c in &lod.chunks {
            bump(&mut s.max_influences_field, c.max_bone_influences);
            let lo = c.base_vertex_index as usize;
            let hi = lo + c.vertex_count().unwrap() as usize;
            let gpu = &vb.vertices[lo..hi];
            for v in gpu {
                let used = v.influence_weights.iter().filter(|&&w| w > 0).count();
                if used > c.max_bone_influences as usize {
                    s.max_influences_exceeded += 1;
                }
            }
            if !c.rigid_vertices.is_empty() || !c.soft_vertices.is_empty() {
                s.source_vertex_chunks += 1;
                s.rigid_vertices += c.rigid_vertices.len() as u64;
                // GPU vertex order: rigid vertices first, then soft.
                let src: Vec<SourceVertex> = c
                    .rigid_vertices
                    .iter()
                    .map(|r| {
                        (
                            r.position,
                            [r.bone, 0, 0, 0],
                            [255, 0, 0, 0],
                            r.uvs[0],
                            r.tangents[2],
                        )
                    })
                    .chain(c.soft_vertices.iter().map(|sv| {
                        (
                            sv.position,
                            sv.influence_bones,
                            sv.influence_weights,
                            sv.uvs[0],
                            sv.tangents[2],
                        )
                    }))
                    .collect();
                for (g, (p, bones, weights, uv, tz)) in gpu.iter().zip(&src) {
                    s.source_vertices += 1;
                    if g.position == *p {
                        s.gpu_matches_source_position += 1;
                    }
                    let gw = g.influence_weights;
                    let same_infl = (0..4).all(|k| {
                        gw[k] == weights[k] && (gw[k] == 0 || g.influence_bones[k] == bones[k])
                    });
                    if same_infl {
                        s.gpu_matches_source_influences += 1;
                    }
                    let as_half = |x: f32| half_to_f32(f32_to_half(x));
                    if g.uvs[0][0] == as_half(uv[0]) && g.uvs[0][1] == as_half(uv[1]) {
                        s.uv0_round_nearest += 1;
                    }
                    if g.uvs[0][0] == half_truncated(uv[0]) && g.uvs[0][1] == half_truncated(uv[1])
                    {
                        s.uv0_truncated += 1;
                    }
                    if (g.uvs[0][0] - uv[0]).abs() < 1e-3 && (g.uvs[0][1] - uv[1]).abs() < 1e-3 {
                        s.gpu_matches_source_uv0 += 1;
                    }
                    if g.tangent_z.0[..3] == tz.0[..3] {
                        s.gpu_matches_source_normal += 1;
                    }
                }
            }
        }
        for v in &vb.vertices {
            s.vertices += 1;
            let w = v.influence_weights;
            if w.iter().map(|&x| u32::from(x)).sum::<u32>() == 255 {
                s.weights_sum_255 += 1;
            }
            if v.influence_bones.iter().map(|&x| u32::from(x)).sum::<u32>() == 255 {
                s.alt_bytes_sum_255 += 1;
            }
            if w.windows(2).all(|p| p[0] >= p[1]) {
                s.weights_descending += 1;
            }
            *s.influence_count
                .entry(w.iter().filter(|&&x| x > 0).count())
                .or_insert(0) += 1;
            let z = unpack(v.tangent_z);
            let x = unpack(v.tangent_x);
            let zl = len([z[0], z[1], z[2]]);
            let xl = len([x[0], x[1], x[2]]);
            if (zl - 1.0).abs() < 0.02 {
                s.normal_unit += 1;
            }
            if (xl - 1.0).abs() < 0.02 {
                s.tangent_unit += 1;
            }
            if dot([z[0], z[1], z[2]], [x[0], x[1], x[2]]).abs() < 0.05 {
                s.orthogonal += 1;
            }
            bump(&mut s.normal_w_byte, v.tangent_z.0[3]);
            bump(&mut s.tangent_w_byte, v.tangent_x.0[3]);
        }
        // Winding against the stored normals.
        for t in lod.indices.indices.as_chunks::<3>().0 {
            let [a, b, c] = t.map(|i| &vb.vertices[i as usize]);
            let face = cross(sub(b.position, a.position), sub(c.position, a.position));
            let fl = len(face);
            if fl < 1e-6 {
                continue;
            }
            let ns: [f64; 3] = std::array::from_fn(|k| {
                [a, b, c]
                    .iter()
                    .map(|v| unpack(v.tangent_z)[k])
                    .sum::<f64>()
            });
            let nl = len(ns);
            if nl < 1e-6 {
                continue;
            }
            let cos = dot(face, ns) / (fl * nl);
            if cos < -0.5 {
                s.face_against_normal += 1;
            } else if cos > 0.5 {
                s.face_along_normal += 1;
            }
        }
    }
}

#[test]
fn skeletal_fields_have_the_documented_meaning() {
    let dir = require_data!();
    let mut s = Semantics::default();
    for_each_package(&dir, |set, lp| {
        for i in 0..lp.package.exports.len() {
            if !is_skeletal_mesh(&lp.package, i) {
                continue;
            }
            let m = decode_skeletal_mesh(&lp.package, Some(&lp.name), i, set).unwrap();
            analyse(&mut s, lp, set, &m);
        }
    });
    let stored = median(&mut s.median_dist_stored.clone());
    let conj = median(&mut s.median_dist_conj.clone());
    let pairs: Vec<(f64, f64)> = s
        .median_dist_stored
        .iter()
        .copied()
        .zip(s.median_dist_conj.iter().copied())
        .collect();
    let better = pairs.iter().filter(|(a, b)| a + 1e-9 < *b).count();
    let not_worse = pairs.iter().filter(|(a, b)| *a <= b + 1e-9).count();
    eprintln!("{s:#?}");
    eprintln!(
        "quaternion convention: median vertex->dominant-bone distance stored {stored:.2} uu, \
         conjugated {conj:.2} uu; stored closer on {better} of {} meshes",
        s.meshes
    );
    // (T) 28 exports, 27 distinct object paths.
    assert_eq!(s.meshes, 28);
    assert_eq!(s.unique_paths.len(), 27);
    // (T) Influence bytes: InfluenceBones[4] then InfluenceWeights[4]; the
    // weights of every vertex sum to 255 and are stored in descending order.
    assert_eq!(s.weights_sum_255, s.vertices);
    assert_eq!(s.weights_descending, s.vertices);
    assert_eq!(s.max_influences_exceeded, 0);
    // (T) Every chunk keeps its source vertices; the GPU vertices of a chunk
    // are its rigid vertices followed by its soft vertices (same positions,
    // influences and normals; UV 0 is the source float UV converted to a
    // half by truncating the mantissa, not by rounding).
    assert_eq!(s.source_vertices, s.vertices);
    assert_eq!(s.gpu_matches_source_position, s.source_vertices);
    assert_eq!(s.gpu_matches_source_influences, s.source_vertices);
    assert_eq!(s.gpu_matches_source_normal, s.source_vertices);
    assert_eq!(s.uv0_truncated, s.source_vertices);
    assert!(s.uv0_round_nearest < s.source_vertices);
    // (T) TangentZ.W is a pure sign byte; winding is clockwise w.r.t. the
    // stored normals, as for static meshes.
    assert!(s.normal_w_byte.keys().all(|&b| b == 0 || b == 255));
    assert!(s.face_against_normal > 50 * s.face_along_normal.max(1));
    // (T) RawPointIndices: one per vertex, stored inline (one stock UDK LOD
    // stores none).
    assert_eq!(s.raw_point_count_eq_vertices + s.raw_point_empty, s.lods);
    assert_eq!(s.raw_point_empty, 1);
    // (T) LOD `Size` is 0 except on one stock UDK LOD, where it is
    // NumVertices x 40 (not that LOD's 32-byte vertex stride; meaning
    // UNKNOWN); sections and chunks pair one to one.
    assert_eq!(s.lod_size_zero + s.lod_size_40_per_vertex, s.lods);
    assert_eq!(s.lod_size_40_per_vertex, 1);
    assert_eq!(s.sections_eq_chunks, s.lods);
    // (T) The stored quaternions (not their conjugates) place vertices next
    // to their dominant bone: never farther, strictly closer on 20 meshes
    // (on the other 8 the two conventions coincide).
    assert_eq!(not_worse, s.meshes);
    assert_eq!(better, 20);
    assert!(stored < conj);
    // (T) Every socket names an existing bone.
    assert_eq!(s.sockets_bone_found, s.sockets);
    assert_eq!(
        s.name_index_map_full + s.name_index_map_empty,
        s.meshes as u64
    );
}
