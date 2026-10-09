//! AnimSet / AnimSequence decoding against the user's own installed game
//! (read-only). Skips cleanly when the data is absent.

#![allow(clippy::unwrap_used)]

use std::path::{Path, PathBuf};

use asamu_ue3::anim::{AnimCoverage, anim_coverage};
use asamu_ue3::model::{LoadedPackage, PackageSet};

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
fn every_anim_sequence_decodes_exactly() {
    let Some(dir) = cooked_dir() else {
        eprintln!("SKIP: original game data not found");
        return;
    };
    let mut covs: Vec<AnimCoverage> = Vec::new();
    for_each_package(&dir, |set, lp| covs.push(anim_coverage(lp, set)));
    for c in &covs {
        if c.sets + c.sequences == 0 {
            continue;
        }
        eprintln!("{c:#?}");
    }
    // (T) The counts of SKELETAL.md, summed over every package.
    let sum = |f: fn(&AnimCoverage) -> u64| covs.iter().map(f).sum::<u64>();
    fn n(v: usize) -> u64 {
        v as u64
    }
    let with_anims = covs.iter().filter(|c| c.sets + c.sequences > 0).count();
    assert_eq!(with_anims, 5, "packages with animation data");
    let failures: Vec<&String> = covs.iter().flat_map(|c| &c.failures).collect();
    assert!(failures.is_empty(), "{failures:?}");
    assert_eq!(sum(|c| n(c.sets)), 18);
    assert_eq!(sum(|c| n(c.sets_exact)), 18);
    let sequences = sum(|c| n(c.sequences));
    assert_eq!(sequences, 395);
    for (what, v) in [
        ("decoded exactly", sum(|c| n(c.sequences_exact))),
        ("stream re-encoded", sum(|c| n(c.stream_round_trip))),
        ("native tail re-encoded", sum(|c| n(c.native_round_trip))),
        (
            "track count = set bones",
            sum(|c| n(c.track_count_matches_set)),
        ),
        ("listed in their set", sum(|c| n(c.listed_in_set))),
        ("raw keys kept", sum(|c| n(c.with_raw_tracks))),
    ] {
        assert_eq!(v, sequences, "{what}");
    }
    assert_eq!(sum(|c| c.stream_bytes), 25_815_812);
    assert_eq!(sum(|c| n(c.identity_tracks)), 4_143);
    assert_eq!(sum(|c| n(c.unsupported_tracks)), 0);
    assert_eq!(sum(|c| c.non_finite_values), 0);
    assert_eq!(sum(|c| c.non_unit_rotation_keys), 0);
    assert_eq!(sum(|c| n(c.additive)), 0);
    assert_eq!(sum(|c| n(c.notifies)), 74);
    assert_eq!(sum(|c| n(c.with_notifies)), 29);
    // Every alignment byte is 0x55.
    let mut padding: BTreeMap<u8, usize> = BTreeMap::new();
    let mut encodings: BTreeMap<String, usize> = BTreeMap::new();
    let mut formats: BTreeMap<String, usize> = BTreeMap::new();
    let mut masks: BTreeMap<u8, usize> = BTreeMap::new();
    for c in &covs {
        for (k, v) in &c.padding_bytes {
            *padding.entry(*k).or_default() += v;
        }
        for (k, v) in &c.key_encodings {
            *encodings.entry(k.clone()).or_default() += v;
        }
        for (k, v) in &c.track_formats {
            *formats.entry(k.clone()).or_default() += v;
        }
        for (k, v) in &c.component_masks {
            *masks.entry(*k).or_default() += v;
        }
    }
    assert_eq!(padding, BTreeMap::from([(0x55, 37_150)]));
    let expect = |pairs: &[(&str, usize)]| -> BTreeMap<String, usize> {
        pairs.iter().map(|(k, v)| ((*k).to_owned(), *v)).collect()
    };
    assert_eq!(
        encodings,
        expect(&[
            ("AKF_ConstantKeyLerp", 7),
            ("AKF_PerTrackCompression", 312),
            ("AKF_VariableKeyLerp", 76),
        ])
    );
    // Export counts (a sequence shipped in two packages counts twice).
    assert_eq!(
        formats,
        expect(&[
            ("AKF_ConstantKeyLerp rotation ACF_Fixed48NoW", 62),
            ("AKF_ConstantKeyLerp rotation ACF_Float96NoW", 517),
            ("AKF_ConstantKeyLerp translation ACF_None", 579),
            ("AKF_PerTrackCompression rotation ACF_Fixed48NoW", 19_504),
            ("AKF_PerTrackCompression rotation ACF_Float96NoW", 636),
            ("AKF_PerTrackCompression translation ACF_Float96NoW", 18_146),
            (
                "AKF_PerTrackCompression translation ACF_IntervalFixed32NoW",
                475
            ),
            ("AKF_VariableKeyLerp rotation ACF_Float96NoW", 8_214),
            ("AKF_VariableKeyLerp translation ACF_None", 8_214),
        ])
    );
    assert_eq!(
        masks,
        BTreeMap::from([
            (0, 535),
            (1, 10_798),
            (2, 46),
            (3, 1_418),
            (4, 7_491),
            (5, 278),
            (6, 218),
            (7, 17_977),
        ])
    );
}

// ---------------------------------------------------------------------------
// Semantics: compressed keys against the raw (editor) keys kept in the
// packages, the time mapping, and the quaternion convention against the
// reference skeleton.
// ---------------------------------------------------------------------------

use std::collections::BTreeMap;

use asamu_ue3::anim::{
    AnimSequence, AnimSetInfo, CompressedTrack, TrackKind, decode_anim_sequence, decode_anim_set,
    is_anim_sequence, is_anim_set, sample_rotation, sample_translation,
};
use asamu_ue3::skeletal::{bone_names, decode_skeletal_mesh, is_skeletal_mesh};

#[derive(Default, Debug, Clone, Copy)]
struct Err {
    count: u64,
    sum: f64,
    max: f64,
}

impl Err {
    fn add(&mut self, e: f64) {
        self.count += 1;
        self.sum += e;
        self.max = self.max.max(e);
    }
}

fn angle_deg(a: [f32; 4], b: [f32; 4]) -> f64 {
    let d: f64 = (0..4).map(|k| f64::from(a[k]) * f64::from(b[k])).sum();
    2.0 * d.abs().min(1.0).acos().to_degrees()
}

fn dist(a: [f32; 3], b: [f32; 3]) -> f64 {
    (0..3)
        .map(|k| (f64::from(a[k]) - f64::from(b[k])).powi(2))
        .sum::<f64>()
        .sqrt()
}

/// "frames" (key -> frame table), "every" (one key per frame), "even"
/// (fewer evenly spaced keys) or "const" (one key).
fn timing(t: &CompressedTrack, num_frames: i32) -> &'static str {
    if t.has_frame_table {
        "frames"
    } else if t.num_keys <= 1 {
        "const"
    } else if i32::try_from(t.num_keys).ok() == Some(num_frames) {
        "every"
    } else {
        "even"
    }
}

type Rigid = ([f64; 3], [f64; 4]);

struct MeshData {
    names: Vec<String>,
    parents: Vec<usize>,
    reference: Vec<Rigid>,
    /// LOD 0 vertices with two or more influences: position and the
    /// reference-skeleton bones influencing it.
    blended: Vec<([f64; 3], Vec<usize>)>,
}

fn cross(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}
fn rotate(q: [f64; 4], v: [f64; 3]) -> [f64; 3] {
    let u = [q[0], q[1], q[2]];
    let t = cross(u, v).map(|c| 2.0 * c);
    let c2 = cross(u, t);
    std::array::from_fn(|k| v[k] + q[3] * t[k] + c2[k])
}
fn qmul(a: [f64; 4], b: [f64; 4]) -> [f64; 4] {
    [
        a[3] * b[0] + a[0] * b[3] + a[1] * b[2] - a[2] * b[1],
        a[3] * b[1] - a[0] * b[2] + a[1] * b[3] + a[2] * b[0],
        a[3] * b[2] + a[0] * b[1] - a[1] * b[0] + a[2] * b[3],
        a[3] * b[3] - a[0] * b[0] - a[1] * b[1] - a[2] * b[2],
    ]
}
fn compose(locals: &[Rigid], parents: &[usize]) -> Vec<Rigid> {
    let mut g: Vec<Rigid> = Vec::with_capacity(locals.len());
    for (i, (t, q)) in locals.iter().enumerate() {
        if i == 0 {
            g.push((*t, *q));
            continue;
        }
        let (pt, pq) = g[parents[i]];
        let rt = rotate(pq, *t);
        g.push(([pt[0] + rt[0], pt[1] + rt[1], pt[2] + rt[2]], qmul(pq, *q)));
    }
    g
}
/// Skin `v` with bone pose `g` against bind pose `r` (`g * r^-1 * v`).
fn skin(g: Rigid, r: Rigid, v: [f64; 3]) -> [f64; 3] {
    let inv = [-r.1[0], -r.1[1], -r.1[2], r.1[3]];
    let l = rotate(inv, [v[0] - r.0[0], v[1] - r.0[1], v[2] - r.0[2]]);
    let w = rotate(g.1, l);
    [w[0] + g.0[0], w[1] + g.0[1], w[2] + g.0[2]]
}

/// Median over blended vertices of the largest distance between the
/// positions their influencing bones skin them to (0 when the pose is
/// continuous at the joints).
fn median_spread(mesh: &MeshData, pose: &[Rigid]) -> f64 {
    let bind = compose(&mesh.reference, &mesh.parents);
    let posed = compose(pose, &mesh.parents);
    let mut spreads: Vec<f64> = mesh
        .blended
        .iter()
        .map(|(p, bones)| {
            let ps: Vec<[f64; 3]> = bones.iter().map(|&b| skin(posed[b], bind[b], *p)).collect();
            let mut mx = 0.0f64;
            for a in &ps {
                for b in &ps {
                    mx = mx.max(
                        ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2) + (a[2] - b[2]).powi(2))
                            .sqrt(),
                    );
                }
            }
            mx
        })
        .collect();
    spreads.sort_by(f64::total_cmp);
    spreads.get(spreads.len() / 2).copied().unwrap_or(0.0)
}

/// Frame-0 pose of `seq` on `mesh`. `engine_rule`: negate W of every
/// non-root bone's rotation (what the engine's pose code does).
fn frame0_pose(
    mesh: &MeshData,
    set: &AnimSetInfo,
    seq: &AnimSequence,
    engine_rule: bool,
) -> Vec<Rigid> {
    mesh.names
        .iter()
        .enumerate()
        .map(|(bi, name)| {
            let reference = mesh.reference[bi];
            let Some(ti) = set
                .track_bone_names
                .iter()
                .position(|t| t.eq_ignore_ascii_case(name))
            else {
                return reference;
            };
            let track = &seq.tracks[ti];
            let mut q = track
                .rotation
                .as_ref()
                .map_or([0.0, 0.0, 0.0, 1.0], |r| r.rotation_key(0))
                .map(f64::from);
            if engine_rule && bi != 0 {
                q[3] = -q[3];
            }
            let t = if set.uses_anim_translation(ti, bi == 0) {
                track
                    .translation
                    .as_ref()
                    .map_or([0.0; 3], |t| t.translation_key(0))
                    .map(f64::from)
            } else {
                reference.0
            };
            (t, q)
        })
        .collect()
}

#[test]
fn compressed_keys_follow_the_raw_keys() {
    let Some(dir) = cooked_dir() else {
        eprintln!("SKIP: original game data not found");
        return;
    };
    let mut rot_err: BTreeMap<String, Err> = BTreeMap::new();
    let mut trans_err: BTreeMap<String, Err> = BTreeMap::new();
    let mut identity_rot_err = Err::default();
    let mut identity_trans_err = Err::default();
    let mut frame_rates: BTreeMap<i64, u64> = BTreeMap::new();
    let mut raw_key_counts: BTreeMap<&'static str, u64> = BTreeMap::new();
    let mut meshes: BTreeMap<String, MeshData> = BTreeMap::new();
    let mut posed: Vec<(AnimSetInfo, AnimSequence)> = Vec::new();
    let mut sequences = 0u64;
    let mut frame_table_monotonic = 0u64;
    let mut frame_table_final_duplicate = 0u64;
    let mut final_duplicates_differ = 0u64;
    let mut frame_tables = 0u64;
    let mut last_key_at_end = 0u64;
    let mut multi_key_tracks = 0u64;
    let mut notify_classes: BTreeMap<String, u64> = BTreeMap::new();
    for_each_package(&dir, |set, lp| {
        let pkg = &lp.package;
        let mut sets: BTreeMap<usize, AnimSetInfo> = BTreeMap::new();
        for i in 0..pkg.exports.len() {
            if is_skeletal_mesh(pkg, i) {
                let m = decode_skeletal_mesh(pkg, Some(&lp.name), i, set).unwrap();
                let n = &m.native;
                let lod = &n.lods[0];
                let mut blended = Vec::new();
                for c in &lod.chunks {
                    let lo = c.base_vertex_index as usize;
                    let hi = lo + c.vertex_count().unwrap() as usize;
                    for v in &lod.vertex_buffer.vertices[lo..hi] {
                        let bones: Vec<usize> = (0..4)
                            .filter(|&k| v.influence_weights[k] > 0)
                            .map(|k| usize::from(c.bone_map[usize::from(v.influence_bones[k])]))
                            .collect();
                        if bones.len() >= 2 {
                            blended.push((v.position.map(f64::from), bones));
                        }
                    }
                }
                meshes.entry(m.object.path.clone()).or_insert(MeshData {
                    names: bone_names(pkg, n),
                    parents: n
                        .ref_skeleton
                        .iter()
                        .map(|b| b.parent_index as usize)
                        .collect(),
                    reference: n
                        .ref_skeleton
                        .iter()
                        .map(|b| (b.position.map(f64::from), b.orientation.map(f64::from)))
                        .collect(),
                    blended,
                });
            }
            if is_anim_set(pkg, i) {
                sets.insert(i, decode_anim_set(pkg, Some(&lp.name), i, set).unwrap());
            }
        }
        for i in 0..pkg.exports.len() {
            if !is_anim_sequence(pkg, i) {
                continue;
            }
            let seq: AnimSequence = decode_anim_sequence(pkg, Some(&lp.name), i, set).unwrap();
            sequences += 1;
            let info = &seq.info;
            for n in &info.notifies {
                let class = n
                    .notify
                    .as_ref()
                    .and_then(|p| {
                        let idx = pkg.find_exports(p.rsplit('.').next().unwrap_or(""));
                        idx.iter()
                            .find(|&&e| pkg.export_path(e).ok().is_some_and(|q| p.ends_with(&q)))
                            .and_then(|&e| pkg.export_class_name(e).ok())
                    })
                    .unwrap_or_else(|| "?".to_owned());
                *notify_classes.entry(class).or_insert(0) += 1;
            }
            if info.num_frames > 1 && info.sequence_length > 0.0 {
                let fps =
                    (f64::from(info.num_frames - 1) / f64::from(info.sequence_length)).round();
                *frame_rates.entry(fps as i64).or_insert(0) += 1;
            }
            let set_info = pkg
                .export(i)
                .ok()
                .and_then(|e| e.outer_index.export_index())
                .and_then(|o| sets.get(&o))
                .cloned()
                .unwrap();
            let nf = info.num_frames;
            let len = info.sequence_length;
            for (ti, (track, raw)) in seq.tracks.iter().zip(&seq.native.raw_tracks).enumerate() {
                for part in [&track.translation, &track.rotation].into_iter().flatten() {
                    if part.has_frame_table {
                        frame_tables += 1;
                        let f = &part.frames;
                        if f.windows(2).all(|w| w[0] < w[1]) {
                            frame_table_monotonic += 1;
                        } else if f.windows(2).all(|w| w[0] <= w[1])
                            && f.windows(2).filter(|w| w[0] == w[1]).count() == 1
                            && f.len() >= 2
                            && f[f.len() - 1] == f[f.len() - 2]
                            && i32::from(f[f.len() - 1]) == nf - 1
                        {
                            frame_table_final_duplicate += 1;
                            let n = part.num_keys;
                            let differ = match part.kind {
                                TrackKind::Rotation => {
                                    angle_deg(part.rotation_key(n - 2), part.rotation_key(n - 1))
                                        > 1e-3
                                }
                                TrackKind::Translation => {
                                    dist(part.translation_key(n - 2), part.translation_key(n - 1))
                                        > 1e-4
                                }
                            };
                            if differ {
                                final_duplicates_differ += 1;
                            }
                        }
                    }
                    if part.num_keys > 1 {
                        multi_key_tracks += 1;
                        let t = part.key_time(part.num_keys - 1, len, nf);
                        if (t - len).abs() <= 1e-4 * len.max(1.0) {
                            last_key_at_end += 1;
                        }
                    }
                }
                *raw_key_counts
                    .entry(match raw.rot_keys.len() {
                        1 => "rot 1",
                        n if i32::try_from(n).ok() == Some(nf) => "rot NumFrames",
                        _ => "rot other",
                    })
                    .or_insert(0) += 1;
                *raw_key_counts
                    .entry(match raw.pos_keys.len() {
                        1 => "pos 1",
                        n if i32::try_from(n).ok() == Some(nf) => "pos NumFrames",
                        _ => "pos other",
                    })
                    .or_insert(0) += 1;
                let uses_translation = set_info.uses_anim_translation(ti, false);
                let frames = nf.max(1) as usize;
                for f in 0..frames {
                    let time = if frames > 1 {
                        f as f32 / (frames - 1) as f32 * len
                    } else {
                        0.0
                    };
                    let rq = raw.rot_keys[f.min(raw.rot_keys.len() - 1)];
                    let rp = raw.pos_keys[f.min(raw.pos_keys.len() - 1)];
                    match &track.rotation {
                        Some(r) => {
                            let key =
                                format!("{:?} {} {}", r.codec, r.format.name(), timing(r, nf));
                            rot_err
                                .entry(key)
                                .or_default()
                                .add(angle_deg(sample_rotation(r, time, len, nf), rq));
                        }
                        None => identity_rot_err.add(angle_deg([0.0, 0.0, 0.0, 1.0], rq)),
                    }
                    match &track.translation {
                        Some(t) => {
                            let key = format!(
                                "{:?} {} {} {}",
                                t.codec,
                                t.format.name(),
                                timing(t, nf),
                                if uses_translation { "anim" } else { "mesh" }
                            );
                            trans_err
                                .entry(key)
                                .or_default()
                                .add(dist(sample_translation(t, time, len, nf), rp));
                        }
                        None => identity_trans_err.add(dist([0.0; 3], rp)),
                    }
                }
            }
            posed.push((set_info, seq));
        }
    });
    eprintln!("sequences {sequences}");
    eprintln!("frame rates (NumFrames-1)/SequenceLength: {frame_rates:?}");
    eprintln!("raw key counts: {raw_key_counts:?}");
    eprintln!(
        "frame tables {frame_tables}, strictly increasing {frame_table_monotonic}, \
         increasing with a duplicated final frame {frame_table_final_duplicate} \
         (the two keys differ on {final_duplicates_differ})"
    );
    eprintln!("multi-key tracks {multi_key_tracks}, last key at SequenceLength {last_key_at_end}");
    eprintln!("notify classes {notify_classes:?}");
    for (k, e) in &rot_err {
        eprintln!(
            "rotation    {k:40} samples {:8} mean {:.4} deg max {:.4} deg",
            e.count,
            e.sum / e.count as f64,
            e.max
        );
    }
    for (k, e) in &trans_err {
        eprintln!(
            "translation {k:46} samples {:8} mean {:.4} uu max {:.4} uu",
            e.count,
            e.sum / e.count as f64,
            e.max
        );
    }
    eprintln!(
        "identity rotation tracks: samples {} max {:.4} deg; identity translation tracks: samples {} max {:.4} uu",
        identity_rot_err.count,
        identity_rot_err.max,
        identity_trans_err.count,
        identity_trans_err.max
    );
    // Quaternion convention: skin LOD 0 of the best-matching mesh with the
    // frame-0 pose; a correct pose is continuous at the joints.
    let (mut paired, mut engine_better, mut engine_not_worse) = (0usize, 0usize, 0usize);
    let (mut spread_engine, mut spread_plain) = (Vec::new(), Vec::new());
    // Second, independent view of the same rule: the frame-0 local rotation
    // of each tracked bone against its reference rotation, with and without
    // the W negation, for the root and for the other bones.
    let (mut nonroot_neg, mut nonroot_plain) = (Vec::new(), Vec::new());
    let (mut root_neg, mut root_plain) = (Vec::new(), Vec::new());
    for (set, seq) in &posed {
        let Some(mesh) = meshes.values().max_by_key(|m| {
            set.track_bone_names
                .iter()
                .filter(|t| m.names.iter().any(|n| n.eq_ignore_ascii_case(t)))
                .count()
        }) else {
            continue;
        };
        let found = set
            .track_bone_names
            .iter()
            .filter(|t| mesh.names.iter().any(|n| n.eq_ignore_ascii_case(t)))
            .count();
        if found * 10 < set.track_bone_names.len() * 9 || mesh.blended.is_empty() {
            continue;
        }
        paired += 1;
        for (bi, name) in mesh.names.iter().enumerate() {
            let Some(ti) = set
                .track_bone_names
                .iter()
                .position(|t| t.eq_ignore_ascii_case(name))
            else {
                continue;
            };
            let Some(r) = seq.tracks[ti].rotation.as_ref() else {
                continue;
            };
            let k = r.rotation_key(0);
            let reference = mesh.reference[bi].1.map(|c| c as f32);
            let neg = angle_deg([k[0], k[1], k[2], -k[3]], reference);
            let plain = angle_deg(k, reference);
            if bi == 0 {
                root_neg.push(neg);
                root_plain.push(plain);
            } else {
                nonroot_neg.push(neg);
                nonroot_plain.push(plain);
            }
        }
        let a = median_spread(mesh, &frame0_pose(mesh, set, seq, true));
        let b = median_spread(mesh, &frame0_pose(mesh, set, seq, false));
        if a + 1e-6 < b {
            engine_better += 1;
        }
        if a <= b + 1e-6 {
            engine_not_worse += 1;
        } else {
            eprintln!(
                "  engine rule worse: {} {} ({} tracks, {found} on the mesh): {a:.3} vs {b:.3}",
                set.path,
                seq.info.sequence_name,
                set.track_bone_names.len()
            );
        }
        spread_engine.push(a);
        spread_plain.push(b);
    }
    spread_engine.sort_by(f64::total_cmp);
    spread_plain.sort_by(f64::total_cmp);
    for v in [
        &mut nonroot_neg,
        &mut nonroot_plain,
        &mut root_neg,
        &mut root_plain,
    ] {
        v.sort_by(f64::total_cmp);
    }
    let med = |v: &[f64]| v.get(v.len() / 2).copied().unwrap_or(0.0);
    eprintln!(
        "frame-0 local rotation vs reference rotation (median): non-root bones ({}) {:.2} deg with \
         the W negation, {:.2} deg without; root ({}) {:.2} deg with, {:.2} deg without",
        nonroot_neg.len(),
        med(&nonroot_neg),
        med(&nonroot_plain),
        root_neg.len(),
        med(&root_neg),
        med(&root_plain)
    );
    eprintln!(
        "frame-0 skinning continuity over {paired} sequences: median spread with the engine's \
         non-root W negation {:.3} uu, without {:.3} uu; engine rule better on {engine_better}, \
         not worse on {engine_not_worse}",
        med(&spread_engine),
        med(&spread_plain)
    );
    // (T) 395 sequences; frame tables increase strictly, except that some
    // repeat the last frame (NumFrames - 1) for their final two keys.
    assert_eq!(sequences, 395);
    assert_eq!(
        frame_table_monotonic + frame_table_final_duplicate,
        frame_tables
    );
    // (T) Raw keys are kept for every track: one key or NumFrames keys.
    assert_eq!(raw_key_counts.get("rot other"), None);
    assert_eq!(raw_key_counts.get("pos other"), None);
    // (T) Decompressed keys follow the raw keys: small mean errors in every
    // format; large maxima only where keys were thinned to fewer evenly
    // spaced keys (the engine's even mapping shifts them in time).
    for (k, e) in &rot_err {
        assert!(e.sum / (e.count as f64) < 0.25, "{k}: mean rotation error");
        if !k.ends_with(" even") {
            assert!(e.max < 6.0, "{k}: max rotation error {} deg", e.max);
        }
    }
    for (k, e) in &trans_err {
        assert!(
            e.sum / (e.count as f64) < 0.05,
            "{k}: mean translation error"
        );
        assert!(e.max < 1.5, "{k}: max translation error {} uu", e.max);
    }
    assert!(identity_rot_err.max < 1.0);
    assert!(identity_trans_err.max < 1e-3);
    // (T) The engine negates W of every non-root animated rotation; with that
    // rule the frame-0 pose is continuous at the joints.
    assert!(paired >= 380, "{paired} sequences paired with a mesh");
    assert!(
        engine_better * 100 >= paired * 99,
        "engine rule better on {engine_better} of {paired}"
    );
    assert!(med(&spread_engine) < med(&spread_plain));
    // (T) Exactly three sequences are not better with the rule: two worse
    // (both on the five-bone Maddie_book prop) and one tie.
    assert_eq!(paired - engine_better, 3);
    assert_eq!(paired - engine_not_worse, 2);
    // (T) Per bone: non-root rotations sit much closer to the reference pose
    // with the negation; the root's only without it (the root exception).
    assert!(med(&nonroot_neg) * 2.0 < med(&nonroot_plain));
    assert!(med(&root_plain) * 10.0 < med(&root_neg));
    // (T) Duplicated final frames: the two keys differ on most such tracks,
    // which is why the exporter keeps both (see SKELETAL.md).
    assert_eq!(frame_table_final_duplicate, 515);
    assert!(final_duplicates_differ > 300, "{final_duplicates_differ}");
}
