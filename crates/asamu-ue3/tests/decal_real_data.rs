//! Decal decoding against the user's own installed game (read-only).
//!
//! Reads `ASAMU_ORIGINAL_DIR` (the folder containing
//! `A Story About My Uncle.app`) or the default macOS Steam location and
//! skips cleanly when the data is absent. Only counts and structural facts
//! are asserted; nothing is copied or written.
//!
//! Acceptance test for `docs/reverse-engineering/VFX_DECALS.md`: `(T)`
//! claims there are asserted here. Run with `-- --nocapture` for the table.

#![allow(clippy::unwrap_used)]

use std::path::{Path, PathBuf};

use std::collections::{HashMap, HashSet};

use asamu_ue3::decal::{
    DecalBox, DecalProjector, DecalStats, ReceiverSource, STATIC_MESH_COMPONENT_CLASS,
    decal_is_mirrored, decode_component, encode_decal_component_native, extract_map_decals,
    transform_mirrors,
};
use asamu_ue3::level::{decode_level, level_exports};
use asamu_ue3::model::PackageSet;
use asamu_ue3::object::export_class_path;

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

fn maps(dir: &Path) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = std::fs::read_dir(dir.join("Maps"))
        .map(|rd| {
            rd.flatten()
                .map(|e| e.path())
                .filter(|p| {
                    p.extension()
                        .is_some_and(|x| x.eq_ignore_ascii_case("asamu"))
                })
                .collect()
        })
        .unwrap_or_default();
    v.sort();
    v
}

fn dist(a: [f32; 3], b: [f32; 3]) -> f32 {
    ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2) + (a[2] - b[2]).powi(2)).sqrt()
}

/// (T) Every `DecalComponent` in every map decodes with its native data
/// consumed exactly and re-encodes byte for byte; per-map census and
/// projection coverage.
#[test]
fn every_decal_component_decodes_and_reencodes_exactly() {
    let Some(dir) = cooked_dir() else {
        eprintln!("SKIP: original game data not found (set ASAMU_ORIGINAL_DIR)");
        return;
    };
    let set = PackageSet::new(&[dir.join("Maps"), dir.clone()]);
    let mut total = DecalStats::default();
    let mut reencoded = 0usize;
    let mut maps_with_decals = 0usize;
    eprintln!(
        "{:<18} {:>5} {:>6} {:>4} {:>6} {:>6} {:>6} {:>6} {:>5} {:>4} {:>4}",
        "map", "comps", "actors", "stat", "geom", "meshes", "tris", "verts", "unres", "mov", "hid"
    );
    for path in maps(&dir) {
        let lp = set.open_file(&path).unwrap();
        for i in 0..lp.package.exports.len() {
            if lp.package.export_class_name(i).ok().as_deref() != Some("DecalComponent")
                || asamu_ue3::object::in_class_default_object(&lp.package, i)
            {
                continue;
            }
            let (obj, native) = decode_component(&set, &lp, i).unwrap();
            let data = lp.package.export_data(i).unwrap();
            let base = i64::from(lp.package.export(i).unwrap().serial_offset)
                + i64::try_from(obj.properties_end).unwrap();
            let bytes = encode_decal_component_native(&native, Some(base)).unwrap();
            assert_eq!(
                bytes.as_slice(),
                &data[obj.properties_end..],
                "{}: re-encode differs",
                obj.path
            );
            reencoded += 1;
        }
        let m = extract_map_decals(&set, &lp).unwrap();
        // The extraction is deterministic (same decals, same order, same
        // geometry on a second run over a fresh package set).
        if m.stats.actors > 0 && m.stats.actors < 80 {
            let again_set = PackageSet::new(&[dir.join("Maps"), dir.clone()]);
            let again_lp = again_set.open_file(&path).unwrap();
            assert_eq!(extract_map_decals(&again_set, &again_lp).unwrap(), m);
        }
        let s = &m.stats;
        eprintln!(
            "{:<18} {:>5} {:>6} {:>4} {:>6} {:>6} {:>6} {:>6} {:>5} {:>4} {:>4}",
            m.package,
            s.components,
            s.actors,
            s.with_static_receivers,
            s.with_geometry,
            s.receiver_meshes,
            s.triangles,
            s.vertices,
            s.unresolved_receivers,
            s.movable,
            s.hidden
        );
        for w in m.warnings.iter().take(3) {
            eprintln!("  warning: {w}");
        }
        assert_eq!(s.components, s.components_exact, "{}", m.package);
        assert_eq!(s.actors, s.actors_with_component, "{}", m.package);
        // Every projected vertex lies inside its decal box.
        for d in &m.decals {
            for r in &d.receivers {
                if r.source == ReceiverSource::Projected {
                    assert_eq!(r.outside, 0, "{}", d.path);
                }
                assert_eq!(r.positions.len(), r.uvs.len());
                assert_eq!(r.positions.len(), r.normals.len());
            }
        }
        if s.actors > 0 {
            maps_with_decals += 1;
        }
        total.add(s);
    }
    eprintln!("total: {total:#?}\nre-encoded {reencoded}; maps with decals {maps_with_decals}");
    assert_eq!(total.components, reencoded);
    assert_eq!(total.components, 469);
    assert_eq!(total.orphan_components, 0);
    assert_eq!(total.actors, 469);
    assert_eq!(total.decal_actors, 371);
    assert_eq!(total.movable, 370);
    assert_eq!(total.with_static_receivers, 99);
    assert_eq!(total.static_receivers, 210);
    assert_eq!(total.hidden, 5);
    assert_eq!(total.without_material, 0);
    assert_eq!(total.vertices_outside, 0);
    assert_eq!(maps_with_decals, 7);
    // One decal sits on a mirrored owner; one editor receiver is a BSP
    // model component (front end); every listed receiver resolves.
    assert_eq!(total.mirrored, 1);
    assert_eq!(total.bsp_receivers, 1);
    assert_eq!(total.unresolved_receivers, 0);
    assert_eq!(total.hidden_receivers, 0);
    // 463 of the 469 decals land on something; 197 receiver meshes are on
    // components outside the editor's receiver lists.
    assert_eq!(total.with_geometry, 463);
    assert_eq!(total.world_receivers, 197);
    assert_eq!(total.receiver_meshes, 1150);
    assert_eq!(total.triangles, 94_588);
}

fn area(positions: &[[f32; 3]], triangles: &[[u32; 3]]) -> f64 {
    triangles
        .iter()
        .map(|t| {
            let [a, b, c] = t.map(|i| positions[i as usize]);
            let u = [b[0] - a[0], b[1] - a[1], b[2] - a[2]].map(f64::from);
            let v = [c[0] - a[0], c[1] - a[1], c[2] - a[2]].map(f64::from);
            let n = [
                u[1] * v[2] - u[2] * v[1],
                u[2] * v[0] - u[0] * v[2],
                u[0] * v[1] - u[1] * v[0],
            ];
            0.5 * (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt()
        })
        .sum()
}

/// (T) The projection used for the movable decals reproduces the cooker's
/// static receivers: over all 99 static decals (210 cooked receivers), the
/// area our projection puts on each receiving component equals the cooked
/// area (both clipped to the decal box) within 2 % on at least 205
/// receivers and within 0.5 % in total; every cooked receiver is in the
/// decal's editor receiver list, and the listed components the cooker left
/// out receive next to nothing from us either.
#[test]
fn projection_matches_the_cooked_receivers_by_area() {
    let Some(dir) = cooked_dir() else {
        eprintln!("SKIP: original game data not found (set ASAMU_ORIGINAL_DIR)");
        return;
    };
    let set = PackageSet::new(&[dir.join("Maps"), dir.clone()]);
    let (mut n, mut within2, mut listed) = (0usize, 0usize, 0usize);
    let (mut cooked_area, mut our_area, mut extra_area) = (0f64, 0f64, 0f64);
    let (mut unclipped, mut vertex_lit) = (0usize, 0usize);
    for path in maps(&dir) {
        let lp = set.open_file(&path).unwrap();
        let m = extract_map_decals(&set, &lp).unwrap();
        unclipped += m.stats.unclipped_receivers;
        let mut proj = DecalProjector::new(&set);
        for d in m.decals.iter().filter(|d| d.static_receivers > 0) {
            let bx = DecalBox::new(d.frame, &d.params);
            // Decal by decal, as many receivers are stored unclipped as
            // carry a vertex light map.
            assert_eq!(
                d.unclipped_receivers,
                d.receivers.iter().filter(|r| r.has_light_map).count(),
                "{}",
                d.path
            );
            for r in &d.receivers {
                let c = r.component.as_deref().unwrap();
                assert_eq!(r.source, ReceiverSource::Cooked);
                assert!(r.listed);
                if r.has_light_map {
                    vertex_lit += 1;
                }
                if d.params.receivers.iter().any(|x| x == c) {
                    listed += 1;
                }
                let ci = lp.export_by_qualified(c).unwrap();
                let ours = proj.project(&lp, ci, &bx).unwrap();
                let (ac, ao) = (
                    area(&r.positions, &r.triangles),
                    area(&ours.positions, &ours.triangles),
                );
                n += 1;
                cooked_area += ac;
                our_area += ao;
                if (ao - ac).abs() <= 0.02 * ac.max(1.0) {
                    within2 += 1;
                }
            }
            for c in &d.params.receivers {
                if d.receivers
                    .iter()
                    .any(|r| r.component.as_deref() == Some(c))
                {
                    continue;
                }
                if let Some(b) = lp
                    .export_by_qualified(c)
                    .and_then(|ci| proj.project(&lp, ci, &bx))
                {
                    extra_area += area(&b.positions, &b.triangles);
                }
            }
        }
    }
    eprintln!(
        "cooked receivers {n} ({unclipped} stored unclipped), area within 2% on {within2}; \
         cooked area {cooked_area:.0} UU², ours {our_area:.0} UU², on uncooked listed \
         components {extra_area:.0} UU²; in the editor list {listed}"
    );
    assert_eq!(n, 210);
    assert_eq!(listed, n);
    // The receivers stored unclipped are as many as those with a vertex
    // light map (20 and 20).
    assert_eq!(unclipped, 20);
    assert_eq!(vertex_lit, 20);
    assert!(within2 >= 205, "{within2}");
    assert!((our_area - cooked_area).abs() <= 0.005 * cooked_area);
    assert!(extra_area <= 0.01 * cooked_area, "{extra_area}");
}

/// (T) On the one static `DecalActor` the match is vertex-exact: projecting
/// its box onto its editor receivers gives, on 3 of the 4 cooked receivers,
/// the same vertex and triangle counts with every cooked vertex within
/// 0.1 UU of one of ours (the cooked vertices are in the receiver's local
/// space); the fourth differs by two clipped polygons.
#[test]
fn projection_reproduces_the_static_decal_actor_exactly() {
    let Some(dir) = cooked_dir() else {
        eprintln!("SKIP: original game data not found (set ASAMU_ORIGINAL_DIR)");
        return;
    };
    let set = PackageSet::new(&[dir.join("Maps"), dir.clone()]);
    let lp = set
        .open_file(&dir.join("Maps").join("AG-Darkcave.asamu"))
        .unwrap();
    let m = extract_map_decals(&set, &lp).unwrap();
    let d = m
        .decals
        .iter()
        .find(|d| d.decal_actor && d.static_receivers > 0)
        .unwrap();
    let bx = DecalBox::new(d.frame, &d.params);
    let mut proj = DecalProjector::new(&set);
    let comp = lp
        .export_by_qualified(d.component.as_deref().unwrap())
        .unwrap();
    let (_, native) = decode_component(&set, &lp, comp).unwrap();
    assert_eq!(native.receivers.len(), 4);
    let mut exact = 0usize;
    let (mut cooked_tris, mut matched_tris) = (0usize, 0usize);
    for rec in &native.receivers {
        let ci = rec.component.export_index().unwrap();
        let world = proj.component_world_matrix(&lp, ci).unwrap();
        let cooked: Vec<[f32; 3]> = rec
            .vertices
            .iter()
            .map(|v| asamu_ue3::decal::transform(&world, v.position))
            .collect();
        let ours = proj.project(&lp, ci, &bx).unwrap();
        let near = |p: [f32; 3]| {
            ours.positions
                .iter()
                .map(|q| dist(p, *q))
                .fold(f32::MAX, f32::min)
        };
        let worst = cooked.iter().map(|p| near(*p)).fold(0.0f32, f32::max);
        let tris = rec.indices.len() / 3;
        cooked_tris += tris;
        matched_tris += rec
            .indices
            .as_chunks::<3>()
            .0
            .iter()
            .filter(|t| t.iter().all(|&i| near(cooked[usize::from(i)]) < 0.1))
            .count();
        eprintln!(
            "receiver {}: ours {} v / {} t, cooked {} v / {} t, worst {worst:.3} UU",
            ci,
            ours.positions.len(),
            ours.triangles.len(),
            cooked.len(),
            tris
        );
        if ours.positions.len() == cooked.len() && ours.triangles.len() == tris && worst < 0.1 {
            exact += 1;
        }
    }
    eprintln!("exact receivers {exact}; matched cooked triangles {matched_tris}/{cooked_tris}");
    assert_eq!(exact, 3);
    assert_eq!(cooked_tris, 121);
    assert!(matched_tris >= 117, "{matched_tris}");
}

fn dot(a: [f32; 3], b: [f32; 3]) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

/// (T) The frame agrees with the hit frame the editor stored on every one
/// of the 469 components: `HitLocation` is the frame's origin, `HitNormal`
/// the reversed projection direction, `HitTangent` the reversed width axis
/// and `HitBinormal` the height axis. The serialized
/// `bFlipBackfaceDirection` equals the native rule (static decal on an
/// owner whose `DrawScale3D` has a negative product) on all of them; it is
/// set on exactly one decal, whose stored normal therefore points along its
/// orientation and whose receivers lie only in the mirrored box.
#[test]
fn frames_match_the_stored_hit_frames_and_the_mirror_rule() {
    let Some(dir) = cooked_dir() else {
        eprintln!("SKIP: original game data not found (set ASAMU_ORIGINAL_DIR)");
        return;
    };
    let set = PackageSet::new(&[dir.join("Maps"), dir.clone()]);
    let (mut n, mut mirrored) = (0usize, Vec::new());
    let (mut worst_axis, mut worst_origin) = (0f32, 0f32);
    for path in maps(&dir) {
        let lp = set.open_file(&path).unwrap();
        let m = extract_map_decals(&set, &lp).unwrap();
        let mut proj = DecalProjector::new(&set);
        for d in &m.decals {
            n += 1;
            let p = &d.params;
            assert_eq!(
                p.flip_backface_direction,
                decal_is_mirrored(p.static_decal, d.draw_scale3d),
                "{}",
                d.path
            );
            assert_eq!(d.mirrored, p.flip_backface_direction, "{}", d.path);
            assert!(p.static_decal, "{}: every placed decal is static", d.path);
            let f = &d.frame;
            for (stored, ours) in [
                (p.hit_normal, f.hit_normal()),
                (p.hit_tangent, f.tangent()),
                (p.hit_binormal, f.height_axis),
            ] {
                let e = (0..3)
                    .map(|k| (stored[k] - ours[k]).abs())
                    .fold(0f32, f32::max);
                worst_axis = worst_axis.max(e);
                assert!(e < 2e-3, "{}: {stored:?} vs {ours:?}", d.path);
            }
            let e = dist(p.hit_location, f.origin);
            worst_origin = worst_origin.max(e);
            assert!(e < 0.05, "{}: origin {e}", d.path);
            if d.mirrored {
                // Stored normal along the orientation's forward axis.
                let forward = asamu_ue3::decal::rotator_axes(d.rotation)[0];
                assert!(dot(p.hit_normal, forward) > 0.999, "{}", d.path);
                // Nothing in the unmirrored box, the decal's surface in the
                // mirrored one.
                let straight = DecalBox::new(f.mirrored(), p);
                let (mut tris_straight, mut area) = (0usize, 0f64);
                for rc in &p.receivers {
                    let ci = lp.export_by_qualified(rc).unwrap();
                    tris_straight += proj.project(&lp, ci, &straight).unwrap().triangles.len();
                }
                for r in &d.receivers {
                    area += self::area(&r.positions, &r.triangles);
                }
                assert_eq!(tris_straight, 0, "{}", d.path);
                assert!(
                    area > 0.9 * f64::from(p.width * p.height),
                    "{}: {area} of {}",
                    d.path,
                    p.width * p.height
                );
                mirrored.push((m.package.clone(), d.name.clone(), d.triangle_count()));
            }
        }
    }
    eprintln!(
        "{n} decals: worst stored axis difference {worst_axis:.6}, origin {worst_origin:.4} UU; mirrored {mirrored:?}"
    );
    assert_eq!(n, 469);
    assert_eq!(mirrored.len(), 1);
    assert_eq!(mirrored[0].0, "AG-BeautifulCity");
}

/// (T) Evidence for the run-time receiver query
/// ([`DecalProjector::world_receivers`]):
///
/// - every static mesh component in the editor's receiver lists (2,455) is
///   one the query can return: exactly a `StaticMeshComponent`, colliding,
///   its owner colliding and listed in the level, accepting static decals,
///   not hidden;
/// - about one placed static mesh component in eight does not collide, so
///   the lists are not a random sample;
/// - the components the query adds (unlisted, but in the decal's box and
///   facing it) all belong to actors that come *after* the decal's actor in
///   `ULevel::Actors`, while the listed receivers all come before it: the
///   editor's list is what was already attached when the decal attached at
///   map load, not a different rule.
#[test]
fn editor_receiver_lists_are_a_prefix_of_the_run_time_query() {
    let Some(dir) = cooked_dir() else {
        eprintln!("SKIP: original game data not found (set ASAMU_ORIGINAL_DIR)");
        return;
    };
    let set = PackageSet::new(&[dir.join("Maps"), dir.clone()]);
    let (mut listed, mut listed_candidates, mut listed_other_class) = (0usize, 0usize, 0usize);
    let (mut candidates, mut placed) = (0usize, 0usize);
    let (mut unlisted_after, mut unlisted_before) = (0usize, 0usize);
    let (mut listed_after, mut listed_before) = (0usize, 0usize);
    for path in maps(&dir) {
        let lp = set.open_file(&path).unwrap();
        let m = extract_map_decals(&set, &lp).unwrap();
        if m.decals.is_empty() {
            continue;
        }
        let pkg = &lp.package;
        let mut level_of_actor = HashMap::new();
        let mut slot_of = HashMap::new();
        for level in level_exports(pkg) {
            let (_, tail) = decode_level(pkg, Some(&lp.name), level, &set).unwrap();
            for (slot, a) in tail.actors.iter().enumerate() {
                if let Some(ai) = a.export_index() {
                    level_of_actor.entry(ai).or_insert(level);
                    slot_of.entry(ai).or_insert(slot);
                }
            }
        }
        let mut proj = DecalProjector::new(&set);
        let world = proj.world_receivers(&lp, &level_of_actor);
        candidates += world.len();
        let by_path: HashMap<&str, &asamu_ue3::decal::WorldReceiver> =
            world.iter().map(|w| (w.path.as_str(), w)).collect();
        for i in 0..pkg.exports.len() {
            if export_class_path(pkg, Some(&lp.name), i)
                .is_ok_and(|c| c.eq_ignore_ascii_case(STATIC_MESH_COMPONENT_CLASS))
                && !asamu_ue3::object::in_class_default_object(pkg, i)
                && pkg
                    .export(i)
                    .ok()
                    .and_then(|e| e.outer_index.export_index())
                    .is_some_and(|o| level_of_actor.contains_key(&o))
            {
                placed += 1;
            }
        }
        let slot_of_component = |path: &str| {
            lp.export_by_qualified(path)
                .and_then(|ci| pkg.export(ci).ok()?.outer_index.export_index())
                .and_then(|o| slot_of.get(&o).copied())
        };
        for d in &m.decals {
            let in_list: HashSet<&str> = d.params.receivers.iter().map(String::as_str).collect();
            for rc in &d.params.receivers {
                let ci = lp.export_by_qualified(rc).unwrap();
                let class = export_class_path(pkg, Some(&lp.name), ci).unwrap();
                if !class.eq_ignore_ascii_case(STATIC_MESH_COMPONENT_CLASS) {
                    listed_other_class += 1;
                    continue;
                }
                listed += 1;
                if let Some(w) = by_path.get(rc.as_str()) {
                    assert!(w.accepts(&d.params) && !w.hidden, "{rc}");
                    listed_candidates += 1;
                }
            }
            if d.static_receivers > 0 {
                continue;
            }
            for r in d.receivers.iter().filter(|r| !r.triangles.is_empty()) {
                let c = r.component.as_deref().unwrap();
                if r.component_class.as_deref() != Some(STATIC_MESH_COMPONENT_CLASS) {
                    continue;
                }
                assert_eq!(r.listed, in_list.contains(c), "{c}");
                let after = slot_of_component(c).is_some_and(|s| s > d.slot);
                match (r.listed, after) {
                    (true, true) => listed_after += 1,
                    (true, false) => listed_before += 1,
                    (false, true) => unlisted_after += 1,
                    (false, false) => unlisted_before += 1,
                }
            }
        }
    }
    eprintln!(
        "listed static mesh receivers {listed} ({listed_candidates} are query candidates; other \
         classes {listed_other_class}); placed static mesh components {placed}, query candidates \
         {candidates}; receivers of movable decals with geometry: listed {listed_before} before / \
         {listed_after} after their decal in the actor list, unlisted {unlisted_before} before / \
         {unlisted_after} after"
    );
    assert_eq!(listed, 2455);
    assert_eq!(listed_candidates, listed);
    assert_eq!(listed_other_class, 1);
    // Not every placed component collides (about 12 % do not).
    assert!(candidates * 100 < placed * 93, "{candidates} of {placed}");
    assert_eq!((listed_before, listed_after), (756, 0));
    assert_eq!((unlisted_before, unlisted_after), (0, 197));
}

/// (T) Mirrored receivers (a negative scale on the component or its actor:
/// the transform's determinant is negative). No cooked receiver is one, so
/// the cooked data cannot show how they are handled; five receivers of
/// three movable decals are. On each of them the decal's box holds far more area
/// on the faces whose *winding* normal points away from the decal than on
/// the ones whose winding normal faces it: the winding of a mirrored mesh is
/// reversed in world space, and the projection has to test the surface's
/// real outward normal (as the native code does in the receiver's local
/// space).
#[test]
fn mirrored_receivers_take_the_decal_on_their_outward_faces() {
    let Some(dir) = cooked_dir() else {
        eprintln!("SKIP: original game data not found (set ASAMU_ORIGINAL_DIR)");
        return;
    };
    let set = PackageSet::new(&[dir.join("Maps"), dir.clone()]);
    let (mut cooked, mut cooked_mirrored) = (0usize, 0usize);
    let (mut mirrored, mut outward, mut winding) = (0usize, 0f64, 0f64);
    for path in maps(&dir) {
        let lp = set.open_file(&path).unwrap();
        let m = extract_map_decals(&set, &lp).unwrap();
        let mut proj = DecalProjector::new(&set);
        for d in &m.decals {
            for r in d.receivers.iter().filter(|r| !r.triangles.is_empty()) {
                let c = r.component.as_deref().unwrap();
                if r.component_class.as_deref() != Some(STATIC_MESH_COMPONENT_CLASS) {
                    continue;
                }
                let ci = lp.export_by_qualified(c).unwrap();
                let world = proj.component_world_matrix(&lp, ci).unwrap();
                let is_mirrored = transform_mirrors(&world);
                if r.source == ReceiverSource::Cooked {
                    cooked += 1;
                    cooked_mirrored += usize::from(is_mirrored);
                    continue;
                }
                if !is_mirrored {
                    continue;
                }
                mirrored += 1;
                // What we extracted (outward faces) against the same box on
                // the triangles as their corners are stored (winding).
                let bx = DecalBox::new(d.frame, &d.params);
                let ours = area(&r.positions, &r.triangles);
                let mut raw = asamu_ue3::decal::DecalMeshBuilder::default();
                raw.project(
                    &bx,
                    proj.component_triangles(&lp, ci)
                        .unwrap()
                        .into_iter()
                        .map(|t| [t[0], t[2], t[1]]),
                );
                let theirs = area(&raw.positions, &raw.triangles);
                eprintln!(
                    "{} {}: on outward faces {ours:.0} UU², by stored winding {theirs:.0} UU²",
                    m.package, d.name
                );
                outward += ours;
                winding += theirs;
            }
        }
    }
    eprintln!(
        "cooked receivers with geometry {cooked} ({cooked_mirrored} mirrored); mirrored receivers \
         of movable decals {mirrored}: {outward:.0} UU² on outward faces, {winding:.0} UU² by winding"
    );
    assert_eq!(cooked_mirrored, 0);
    assert_eq!(mirrored, 5);
    assert!(outward > 10.0 * winding, "{outward} vs {winding}");
}
