//! Matinee data of every shipped map, decoded from the user's own install
//! (read-only). Skips cleanly when the data is absent.
//!
//! Acceptance test for `docs/reverse-engineering/MATINEE.md`:
//! - every `InterpData`, group and track of the 12 maps decodes, with no
//!   decoder warning, no unmodelled track class and no stored instance;
//! - every reachable track export is accounted for (one editor leftover);
//! - the per-map census matches the documented counts;
//! - recomputing the automatic tangents reproduces the stored ones;
//! - every relative move track bound to an actor starts at that actor's
//!   placement (initial transform + world-space key transform; a numerical
//!   round trip of our transform chain, not evidence about the engine);
//! - every shipped curve evaluates like an independent textbook Hermite /
//!   linear / constant evaluator in double precision, and the stored data
//!   never exercises the paths our runtime rules leave TENTATIVE or that
//!   the verification found to matter (a stored `Position`, jumping-forward
//!   events, `CurveTension`, a segment started by a `Constant` key).
//!
//! Only counts and class names are asserted or printed; nothing is written.

#![allow(clippy::unwrap_used)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use asamu_ue3::kismet::NodeScope;
use asamu_ue3::matinee::{
    self, AutoTangentValue, CurveMode, CurveValue, InterpCurve, MatineeCoverage, MatineeMap,
    MoveFrame, MoveInstance, MoveRotation, NoGroupActors, TrackData, normalize_axis,
    rotation_translation_matrix,
};
use asamu_ue3::model::{LoadedPackage, PackageSet};
use asamu_ue3::{Property, Value};

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

/// Every map, each decoded with a fresh package set (bounded memory).
fn each_map(dir: &Path, mut f: impl FnMut(&PackageSet, &LoadedPackage, &MatineeMap)) {
    for file in maps(dir) {
        let set = PackageSet::new(&[dir.to_path_buf(), dir.join("Maps")]);
        let lp = set.open_file(&file).unwrap();
        let m = matinee::extract_for(&set, &lp);
        f(&set, &lp, &m);
    }
}

/// (map, actions, InterpData, groups, tracks, group bindings, camera anims,
/// orphan tracks).
type CensusRow = (
    &'static str,
    usize,
    usize,
    usize,
    usize,
    usize,
    usize,
    usize,
);

const CENSUS: &[CensusRow] = &[
    ("AG-BeautifulCity", 17, 17, 21, 83, 19, 0, 0),
    ("AG-Darkcave", 6, 6, 11, 28, 8, 2, 0),
    ("AG-Epilogue", 3, 3, 6, 13, 3, 0, 0),
    ("AG-IceCave", 13, 13, 15, 18, 14, 0, 0),
    ("AG-ParadiseCave", 35, 35, 42, 97, 39, 0, 0),
    ("AG-StarHaven", 60, 57, 114, 397, 114, 1, 1),
    ("AG-Workshop", 19, 19, 22, 63, 18, 0, 0),
    ("ASAMUEntry", 0, 0, 0, 0, 0, 0, 0),
    ("ASAMUFrontEndMap", 1, 1, 1, 4, 1, 0, 0),
    ("ASAMULegal", 0, 0, 0, 0, 0, 0, 0),
    ("Freds_place", 0, 0, 0, 0, 0, 0, 0),
    ("TheCore", 7, 7, 10, 20, 8, 0, 0),
];

fn add(t: &mut MatineeCoverage, c: &MatineeCoverage) {
    t.actions += c.actions;
    t.level_actions += c.level_actions;
    t.actions_with_data += c.actions_with_data;
    t.interp_data += c.interp_data;
    t.level_interp_data += c.level_interp_data;
    t.unused_interp_data += c.unused_interp_data;
    t.camera_anims += c.camera_anims;
    t.folder_groups += c.folder_groups;
    t.tracks_total += c.tracks_total;
    t.tracks_decoded += c.tracks_decoded;
    t.tracks_unknown += c.tracks_unknown;
    t.tracks_disabled += c.tracks_disabled;
    t.orphan_tracks += c.orphan_tracks;
    t.orphan_groups += c.orphan_groups;
    t.stored_instances += c.stored_instances;
    t.quat_interpolation += c.quat_interpolation;
    t.raw_actor_tm += c.raw_actor_tm;
    t.split_move_tracks += c.split_move_tracks;
    t.axis_order_mismatches += c.axis_order_mismatches;
    t.lookup_group_keys += c.lookup_group_keys;
    t.lookup_length_mismatches += c.lookup_length_mismatches;
    t.bindings += c.bindings;
    t.property_links += c.property_links;
    t.bindings_without_group += c.bindings_without_group;
    t.unbound_groups += c.unbound_groups;
    t.archetype_merges += c.archetype_merges;
    t.unremapped_refs += c.unremapped_refs;
    t.decode_failures += c.decode_failures;
    t.warnings += c.warnings;
    let sum = |a: &mut BTreeMap<String, usize>, b: &BTreeMap<String, usize>| {
        for (k, v) in b {
            *a.entry(k.clone()).or_default() += v;
        }
    };
    sum(&mut t.groups, &c.groups);
    sum(&mut t.curve_modes, &c.curve_modes);
    sum(&mut t.curve_methods, &c.curve_methods);
    sum(&mut t.move_frames, &c.move_frames);
    sum(&mut t.rot_modes, &c.rot_modes);
    sum(&mut t.bound_classes, &c.bound_classes);
    for (k, v) in &c.tracks {
        let e = t.tracks.entry(k.clone()).or_default();
        e.count += v.count;
        e.decoded += v.decoded;
        e.curve_keys += v.curve_keys;
        e.discrete_keys += v.discrete_keys;
    }
}

#[test]
fn every_map_decodes_completely_and_matches_the_census() {
    let dir = require_data!();
    let mut total = MatineeCoverage::default();
    let mut seen = Vec::new();
    each_map(&dir, |_, _, m| {
        let c = &m.coverage;
        println!(
            "{:<18} actions {:>3} (level {:>3})  data {:>3}  groups {:>3}  tracks {:>3}  \
             bindings {:>3}  property links {}  camera anims {}  orphans {}",
            m.package,
            c.actions,
            c.level_actions,
            c.interp_data,
            c.groups.values().sum::<usize>(),
            c.tracks_total,
            c.bindings,
            c.property_links,
            c.camera_anims,
            c.orphan_tracks + c.orphan_groups
        );
        assert!(m.warnings.is_empty(), "{}: {:?}", m.package, m.warnings);
        assert_eq!(c.warnings, 0, "{}", m.package);
        assert_eq!(c.decode_failures, 0, "{}", m.package);
        assert_eq!(c.tracks_unknown, 0, "{}", m.package);
        assert_eq!(c.tracks_decoded, c.tracks_total, "{}", m.package);
        assert_eq!(
            c.stored_instances, 0,
            "{}: instances are transient",
            m.package
        );
        assert_eq!(c.unremapped_refs, 0, "{}", m.package);
        assert_eq!(c.orphan_groups, 0, "{}", m.package);
        assert_eq!(c.actions_with_data, c.actions, "{}", m.package);
        assert_eq!(c.unused_interp_data, 0, "{}", m.package);
        let row = CENSUS
            .iter()
            .find(|r| r.0.eq_ignore_ascii_case(&m.package))
            .unwrap_or_else(|| panic!("{} not in the census", m.package));
        assert_eq!(
            (
                c.actions,
                c.interp_data,
                c.groups.values().sum::<usize>(),
                c.tracks_total,
                c.bindings,
                c.camera_anims,
                c.orphan_tracks
            ),
            (row.1, row.2, row.3, row.4, row.5, row.6, row.7),
            "{}",
            m.package
        );
        seen.push(m.package.clone());
        add(&mut total, c);
    });
    assert_eq!(seen.len(), CENSUS.len());
    println!("{total:#?}");

    // Totals (the census in MATINEE.md).
    assert_eq!(total.actions, 161);
    assert_eq!(total.level_actions, 158);
    assert_eq!(total.interp_data, 158);
    assert_eq!(total.level_interp_data, 155);
    assert_eq!(total.tracks_total, 723);
    assert_eq!(total.orphan_tracks, 1);
    assert_eq!(total.groups.get("InterpGroup"), Some(&221));
    assert_eq!(total.groups.get("InterpGroupDirector"), Some(&18));
    assert_eq!(total.groups.get("InterpGroupCamera"), Some(&3));
    assert_eq!(total.folder_groups, 0);
    let count = |k: &str| total.tracks.get(k).map_or(0, |t| t.count);
    assert_eq!(count("InterpTrackMove"), 172);
    assert_eq!(count("InterpTrackMoveAxis"), 354);
    assert_eq!(count("InterpTrackAnimControl"), 49);
    assert_eq!(count("InterpTrackEvent"), 35);
    assert_eq!(count("InterpTrackDirector"), 15);
    assert_eq!(count("InterpTrackFade"), 14);
    assert_eq!(count("InterpTrackFloatProp"), 23);
    assert_eq!(count("InterpTrackSkelControlStrength"), 17);
    assert_eq!(count("InterpTrackSound"), 17);
    assert_eq!(count("InterpTrackVectorProp"), 12);
    assert_eq!(count("InterpTrackColorProp"), 7);
    assert_eq!(count("InterpTrackSkelControlScale"), 4);
    assert_eq!(count("InterpTrackVisibility"), 2);
    assert_eq!(count("InterpTrackToggle"), 1);
    assert_eq!(count("InterpTrackParticleReplay"), 1);
    // Every curve keeps the default method.
    assert_eq!(total.curve_methods.len(), 1);
    assert!(
        total
            .curve_methods
            .contains_key("IMT_UseFixedTangentEvalAndNewAutoTangents")
    );
    // Move-track features the runtime needs.
    assert_eq!(
        total.rot_modes.get("IMR_Keyframed").copied(),
        Some(count("InterpTrackMove"))
    );
    assert_eq!(total.quat_interpolation, 1);
    assert_eq!(total.raw_actor_tm, 0);
    assert_eq!(total.lookup_group_keys, 0);
    assert_eq!(total.lookup_length_mismatches, 0);
    assert_eq!(total.axis_order_mismatches, 0);
    assert_eq!(total.split_move_tracks, 59);
    // Bindings.
    assert_eq!(total.bindings_without_group, 0);
    assert_eq!(total.property_links, 2);
    assert_eq!(total.bound_classes.get("InterpActor"), Some(&237));
}

#[test]
fn actions_link_their_data_and_groups() {
    let dir = require_data!();
    each_map(&dir, |_, _, m| {
        for a in &m.actions {
            let d = m.data(a.interp_data.as_ref().unwrap()).unwrap();
            assert!(d.used_by.contains(&a.path));
            assert!(d.length > 0.0, "{}", d.path);
            for b in &a.bindings {
                let g = d
                    .groups
                    .iter()
                    .find(|g| Some(&g.name) == b.group.as_ref())
                    .unwrap_or_else(|| panic!("{}: link {} has no group", a.path, b.label));
                assert_ne!(g.kind, matinee::GroupKind::Director);
            }
            // The standard inputs, in order.
            assert_eq!(
                a.inputs,
                ["Play", "Reverse", "Stop", "Pause", "Change Dir"],
                "{}",
                a.path
            );
            assert!(a.settings.play_rate > 0.0);
            if a.scope == NodeScope::Prefab {
                assert!(d.scope == NodeScope::Prefab);
            }
        }
    });
}

fn check_auto<T: AutoTangentValue>(c: &InterpCurve<T>, tension: f32, stats: &mut [usize; 3]) {
    let mut re = c.clone();
    re.auto_set_tangents(tension);
    for (a, b) in c.points.iter().zip(&re.points) {
        if !a.mode.is_auto() {
            continue;
        }
        let exact = a.arrive_tangent == b.arrive_tangent && a.leave_tangent == b.leave_tangent;
        let close = (0..T::DIM).all(|k| {
            let s = a.arrive_tangent.get(k).abs().max(1.0);
            (a.arrive_tangent.get(k) - b.arrive_tangent.get(k)).abs() <= 1e-5 * s
                && (a.leave_tangent.get(k) - b.leave_tangent.get(k)).abs() <= 1e-5 * s
        });
        stats[0] += usize::from(exact);
        stats[1] += usize::from(!exact && close);
        stats[2] += usize::from(!close);
    }
}

#[test]
fn auto_tangents_reproduce_the_stored_tangents() {
    let dir = require_data!();
    let mut f = [0usize; 3];
    let mut v = [0usize; 3];
    each_map(&dir, |_, _, m| {
        let groups = m
            .interp_data
            .iter()
            .flat_map(|d| d.groups.iter())
            .chain(m.camera_anims.iter().filter_map(|c| c.group.as_ref()));
        for g in groups {
            for t in &g.tracks {
                match &t.data {
                    TrackData::Move(mv) => {
                        check_auto(&mv.pos, mv.lin_curve_tension, &mut v);
                        check_auto(&mv.euler, mv.ang_curve_tension, &mut v);
                        for a in &mv.axes {
                            check_auto(&a.curve, 0.0, &mut f);
                        }
                    }
                    TrackData::FloatProperty(c)
                    | TrackData::SkelControlStrength(c)
                    | TrackData::SkelControlScale(c) => check_auto(&c.curve, 0.0, &mut f),
                    TrackData::Fade(c) => check_auto(&c.curve, 0.0, &mut f),
                    TrackData::AnimControl(c) => check_auto(&c.weight, 0.0, &mut f),
                    TrackData::VectorProperty(c) => check_auto(&c.curve, 0.0, &mut v),
                    _ => {}
                }
            }
        }
    });
    println!("float auto keys [exact, within 1e-5, off] = {f:?}; vector = {v:?}");
    let (ft, vt) = (f.iter().sum::<usize>(), v.iter().sum::<usize>());
    assert!(ft > 1000 && vt > 800, "{f:?} {v:?}");
    // Bit-exact for the overwhelming majority; a few clamped keys differ in
    // the last bits (the editor that baked them was another build).
    assert!(f[0] * 100 >= ft * 99, "{f:?}");
    assert!(v[0] * 100 >= vt * 95, "{v:?}");
    assert!(f[2] <= 2 && v[2] == 0, "{f:?} {v:?}");
}

fn member_f32(props: &[Property], name: &str, m: &str) -> f32 {
    match props.iter().find(|p| p.name == name).map(|p| &p.value) {
        Some(Value::Struct { fields, .. }) => fields
            .iter()
            .find(|p| p.name == m)
            .and_then(|p| match p.value {
                Value::Float(x) => Some(x),
                _ => None,
            })
            .unwrap_or(0.0),
        _ => 0.0,
    }
}

fn member_i32(props: &[Property], name: &str, m: &str) -> i32 {
    match props.iter().find(|p| p.name == name).map(|p| &p.value) {
        Some(Value::Struct { fields, .. }) => fields
            .iter()
            .find(|p| p.name == m)
            .and_then(|p| match p.value {
                Value::Int(x) => Some(x),
                _ => None,
            })
            .unwrap_or(0),
        _ => 0,
    }
}

fn placement(props: &[Property]) -> ([f32; 3], [i32; 3]) {
    (
        [
            member_f32(props, "Location", "X"),
            member_f32(props, "Location", "Y"),
            member_f32(props, "Location", "Z"),
        ],
        [
            member_i32(props, "Rotation", "Pitch"),
            member_i32(props, "Rotation", "Yaw"),
            member_i32(props, "Rotation", "Roll"),
        ],
    )
}

#[test]
fn relative_move_tracks_start_at_their_actors() {
    let dir = require_data!();
    let (mut checked, mut based) = (0usize, 0usize);
    each_map(&dir, |set, lp, m| {
        for a in m.actions.iter().filter(|a| a.scope == NodeScope::Level) {
            let d = m.data(a.interp_data.as_ref().unwrap()).unwrap();
            for b in &a.bindings {
                let Some(g) = d.groups.iter().find(|g| Some(&g.name) == b.group.as_ref()) else {
                    continue;
                };
                for t in &b.targets {
                    let Some(idx) = t.object.as_ref().and_then(|o| lp.export_by_qualified(o))
                    else {
                        continue;
                    };
                    let props = set.decode(lp, idx).unwrap().properties;
                    let (loc, rot) = placement(&props);
                    let base = match props.iter().find(|p| p.name == "Base").map(|p| &p.value) {
                        Some(Value::Object(o)) if o.index > 0 => {
                            let bp = set.decode(lp, (o.index - 1) as usize).unwrap().properties;
                            let (bl, br) = placement(&bp);
                            Some(rotation_translation_matrix(br, bl))
                        }
                        _ => None,
                    };
                    for tr in &g.tracks {
                        let TrackData::Move(mv) = &tr.data else {
                            continue;
                        };
                        if mv.move_frame != MoveFrame::RelativeToInitial || !mv.is_active() {
                            continue;
                        }
                        let inst = match &base {
                            Some(bm) => {
                                based += 1;
                                MoveInstance::with_base(mv, loc, rot, bm, 0.0, &NoGroupActors)
                            }
                            None => MoveInstance::new(mv, loc, rot, 0.0, &NoGroupActors),
                        };
                        let s = mv.sample(0.0, &inst, &NoGroupActors).unwrap();
                        let tol = loc.iter().fold(0.05f32, |acc, x| acc.max(x.abs() * 4e-7));
                        for k in 0..3 {
                            assert!(
                                (s.location[k] - loc[k]).abs() <= tol,
                                "{}: {:?} vs {loc:?}",
                                m.package,
                                s.location
                            );
                        }
                        let MoveRotation::Set(r) = s.rotation else {
                            panic!("keyframed rotation expected")
                        };
                        for k in 0..3 {
                            // Table trigonometry works in 4-unit steps.
                            assert!(
                                normalize_axis(r[k].wrapping_sub(rot[k])).abs() <= 4,
                                "{}: {r:?} vs {rot:?}",
                                m.package
                            );
                        }
                        checked += 1;
                    }
                }
            }
        }
    });
    println!("relative move tracks checked {checked} (attached to a base: {based})");
    assert!(checked >= 250, "{checked}");
    assert!(based > 0);
}

/// Reference evaluation written independently of `InterpCurve::eval`
/// (double precision, textbook Hermite basis): first/last key outside the
/// range, the segment's start key decides constant / linear / cubic, and
/// cubic tangents are scaled by the segment length (the default method,
/// which every shipped curve uses).
fn reference<T: CurveValue>(c: &InterpCurve<T>, t: f32, k: usize) -> f64 {
    let p = &c.points;
    let out = |i: usize| f64::from(p[i].out_val.get(k));
    let n = p.len();
    if n == 0 {
        return 0.0;
    }
    if n == 1 || t <= p[0].in_val {
        return out(0);
    }
    if t >= p[n - 1].in_val {
        return out(n - 1);
    }
    let i = (1..n).find(|&i| t < p[i].in_val).unwrap();
    let (a, b) = (&p[i - 1], &p[i]);
    let d = f64::from(b.in_val) - f64::from(a.in_val);
    if d <= 0.0 || a.mode == CurveMode::Constant {
        return out(i - 1);
    }
    let x = (f64::from(t) - f64::from(a.in_val)) / d;
    let (p0, p1) = (out(i - 1), out(i));
    if a.mode == CurveMode::Linear {
        return p0 + x * (p1 - p0);
    }
    let m0 = f64::from(a.leave_tangent.get(k)) * d;
    let m1 = f64::from(b.arrive_tangent.get(k)) * d;
    let (x2, x3) = (x * x, x * x * x);
    (2.0 * x3 - 3.0 * x2 + 1.0) * p0
        + (x3 - 2.0 * x2 + x) * m0
        + (-2.0 * x3 + 3.0 * x2) * p1
        + (x3 - x2) * m1
}

#[derive(Default)]
struct CurveStats {
    samples: usize,
    max_error: f64,
    constant_segments: usize,
}

fn check_curve<T: CurveValue>(c: &InterpCurve<T>, default: T, st: &mut CurveStats) {
    let p = &c.points;
    let mut times = Vec::new();
    for w in p.windows(2) {
        if w[0].mode == CurveMode::Constant {
            st.constant_segments += 1;
        }
        for f in [0.0f32, 0.137, 0.5, 0.731, 0.999] {
            times.push(w[0].in_val + f * (w[1].in_val - w[0].in_val));
        }
    }
    if let (Some(a), Some(b)) = (p.first(), p.last()) {
        times.push(a.in_val - 1.0);
        times.push(b.in_val + 1.0);
    }
    for t in times {
        let got = c.eval(t, default);
        for k in 0..T::DIM {
            // Relative to the curve's own scale (values and tangents).
            let scale = p
                .iter()
                .flat_map(|q| {
                    [
                        q.out_val.get(k),
                        q.arrive_tangent.get(k),
                        q.leave_tangent.get(k),
                    ]
                })
                .fold(1.0f64, |m, v| m.max(4.0 * f64::from(v).abs()));
            let err = (f64::from(got.get(k)) - reference(c, t, k)).abs() / scale;
            st.max_error = st.max_error.max(err);
            st.samples += 1;
        }
    }
}

#[test]
fn shipped_curves_and_flags_behind_the_runtime_rules() {
    let dir = require_data!();
    let mut st = CurveStats::default();
    let (mut positions, mut jump_events, mut tensions) = (0usize, 0usize, 0usize);
    let (mut relative, mut first_key_moves) = (0usize, 0usize);
    each_map(&dir, |set, lp, m| {
        for a in &m.actions {
            let props = set.decode(lp, a.export_index).unwrap().properties;
            positions += usize::from(props.iter().any(|p| p.name == "Position"));
        }
        let groups = m
            .interp_data
            .iter()
            .flat_map(|d| d.groups.iter())
            .chain(m.camera_anims.iter().filter_map(|c| c.group.as_ref()));
        for g in groups {
            for t in &g.tracks {
                let raw = set.decode(lp, t.export_index).unwrap().properties;
                tensions += usize::from(raw.iter().any(|p| p.name == "CurveTension"));
                match &t.data {
                    TrackData::Move(mv) => {
                        check_curve(&mv.pos, [0.0; 3], &mut st);
                        check_curve(&mv.euler, [0.0; 3], &mut st);
                        for ax in &mv.axes {
                            check_curve(&ax.curve, 0.0, &mut st);
                        }
                        if mv.move_frame == MoveFrame::RelativeToInitial {
                            relative += 1;
                            let a = NoGroupActors;
                            let (p0, r0) = mv.key_transform(0.0, &a);
                            first_key_moves += usize::from(p0 != [0.0; 3] || r0 != [0; 3]);
                        }
                    }
                    TrackData::FloatProperty(c)
                    | TrackData::SkelControlStrength(c)
                    | TrackData::SkelControlScale(c) => check_curve(&c.curve, 0.0, &mut st),
                    TrackData::Fade(f) => check_curve(&f.curve, 0.0, &mut st),
                    TrackData::AnimControl(c) => check_curve(&c.weight, 0.0, &mut st),
                    TrackData::VectorProperty(c) | TrackData::ColorProperty(c) => {
                        check_curve(&c.curve, [0.0; 3], &mut st)
                    }
                    TrackData::Sound(s) => check_curve(&s.curve, [0.0; 3], &mut st),
                    TrackData::Event(e) => jump_events += usize::from(e.fire_jumping_forwards),
                    _ => {}
                }
            }
        }
    });
    println!(
        "curve samples {} (max scaled error {:e}); relative move tracks {relative}, \
         {first_key_moves} with a non-identity time-0 key",
        st.samples, st.max_error
    );
    assert!(st.samples > 10_000, "{}", st.samples);
    assert!(st.max_error < 1e-6, "{:e}", st.max_error);
    // No segment starts at a Constant key (every Constant key ends its curve).
    assert_eq!(st.constant_segments, 0);
    // Initialisation always sees Position 0 at level start (no action stores
    // one); restarted actions use their stopped position.
    assert_eq!(positions, 0);
    // No event track fires on forward jumps (the reverse-loop wrap rule has
    // no audible effect on the shipped maps).
    assert_eq!(jump_events, 0);
    // Tension 0 for every float/vector-base track (CurveTension never set).
    assert_eq!(tensions, 0);
    // The time-0 key is factored out of the initial transform for these.
    assert!(
        relative >= 160 && first_key_moves > 0,
        "{relative} {first_key_moves}"
    );
}
