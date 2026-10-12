//! The game's collision world answers the pawn's box and the grapple's trace
//! the way the original's world check does (`asamu_game::world`;
//! `docs/PARITY_FINDINGS.md` P1 to P5, NATIVE_PHYSICS.md 10.2). No game
//! data: the rule on a list of contacts, and synthetic converted scenes.
//!
//! Every expected value is derived in a comment from the rules read in the
//! user's executable:
//!
//! - **Rule K**: a routine reports its hit short of the contact by 0.1 uu on
//!   a trace under 1 uu, else by a tenth of the trace's length, at most K uu;
//!   K = 1.0 for BSP (box traces), brushes and simple bodies, 4.0 for mesh
//!   triangles. A zero-extent trace on BSP: a flat 0.5 uu.
//! - **BSP first**: after a BSP hit reported at the distance `Dist` on a
//!   trace of length `L`, everything else is asked on the first
//!   `(5 + Dist) × (Dist / L) / (Dist + 0.0001)` of the trace (5 uu beyond
//!   the reported BSP hit) and applies its rule to that shorter length.

use asamu_game::world::{ReportedFirst, reported_first};
use asamu_game::{Game, TickReport};
use asamu_player::grapple_gun::ReleaseReason;
use asamu_player::ue3_movement::PawnPhysicsState;
use asamu_player::world::{PrimitiveKind as RoutineKind, TraceExtent};
use asamu_player::{CollisionWorld, FireOutcome, InputFrame, PlayerParams};
use asamu_world::collision::{InstanceRef, KindSet, PrimitiveKind as SceneKind, SceneHit};
use asamu_world::fixtures::{MeshFixtures, Place, SceneFixture, box_mesh};
use asamu_world::scene::{self, LoadOptions, MemorySource};
use glam::{DVec3, Vec3};

// ---------------------------------------------------------------------------
// The rule on a list of contacts.
// ---------------------------------------------------------------------------

/// Contacts along one trace: `(kind, distance from the start, uu)`.
struct Contacts {
    length: f32,
    contacts: Vec<(SceneKind, f32)>,
    /// What was asked: `(kinds, part of the trace)`.
    asked: Vec<(KindSet, f32)>,
}

impl Contacts {
    fn new(length: f32, contacts: &[(SceneKind, f32)]) -> Self {
        Self {
            length,
            contacts: contacts.to_vec(),
            asked: Vec::new(),
        }
    }

    /// The first contact among `kinds` on the first `part` of the trace,
    /// its time a fraction of that part (as the scene's queries answer).
    fn first_of(&mut self, kinds: KindSet, part: f32) -> Option<SceneHit> {
        self.asked.push((kinds, part));
        let reach = self.length * part;
        self.contacts
            .iter()
            .enumerate()
            .filter(|(_, (kind, distance))| kinds.contains(*kind) && *distance <= reach)
            .min_by(|a, b| a.1.1.total_cmp(&b.1.1))
            .map(|(i, (kind, distance))| SceneHit {
                t: f64::from(*distance) / f64::from(reach),
                normal: DVec3::Z,
                penetrating: false,
                instance: InstanceRef::Static(i as u32),
                triangle: 0,
                kind: *kind,
            })
    }

    fn check(&mut self, extent: TraceExtent) -> Option<ReportedFirst> {
        let length = self.length;
        reported_first(extent, length, |kinds, part| self.first_of(kinds, part))
    }
}

/// The part of a trace of `length` the other primitives are asked on after
/// a BSP hit reported `dist` uu from the start (the rule in the file docs).
fn checked(dist: f32, length: f32) -> f32 {
    (5.0 + dist) * (dist / length) / (dist + 1.0e-4)
}

fn close(a: f32, b: f32, tolerance: f32) -> bool {
    (a - b).abs() <= tolerance
}

#[test]
fn a_single_kind_reports_its_first_contact_with_its_own_rule() {
    // A 28 uu floor check (box) that meets one surface 10 uu down.
    for (kind, short) in [
        // K = 1.0: a tenth of 28 uu is 2.8, capped at 1.0.
        (SceneKind::Bsp, 1.0_f32),
        (SceneKind::BrushHull, 1.0),
        (SceneKind::SimpleHull, 1.0),
        // K = 4.0: 2.8 uu.
        (SceneKind::TriangleMesh, 2.8),
        // A pawn cylinder: 0.001 of the trace.
        (SceneKind::PawnCylinder, 0.028),
    ] {
        let mut c = Contacts::new(28.0, &[(kind, 10.0)]);
        let r = c.check(TraceExtent::Box).unwrap();
        assert_eq!(r.hit.kind, kind);
        assert!(close(r.time, 10.0 / 28.0, 1e-7), "{kind:?}: {r:?}");
        assert_eq!(r.checked_fraction, 1.0, "{kind:?}: no BSP hit, whole trace");
        assert!(
            close(r.reported * 28.0, 10.0 - short, 1e-4),
            "{kind:?}: {r:?}"
        );
    }
    // Nothing on the trace: nothing reported, and only one question asked.
    let mut c = Contacts::new(28.0, &[(SceneKind::Bsp, 40.0)]);
    assert_eq!(c.check(TraceExtent::Box), None);
    assert_eq!(c.asked, vec![(KindSet::ALL, 1.0)]);
}

#[test]
fn triangles_over_a_bsp_floor_use_the_trace_shortened_by_the_bsp_hit() {
    // The 28 uu floor check from 4 uu above mesh triangles that lie 6 uu
    // above a BSP floor (the BSP 10 uu down). BSP is reported 1.0 uu short:
    // Dist = 9. Everything else is asked on the first 14/28 of the trace
    // (5 uu beyond), a 14 uu trace, so the triangles are reported a tenth of
    // 14 uu short, 1.4, not the 2.8 of the whole trace: at 2.6 uu.
    let mut c = Contacts::new(
        28.0,
        &[(SceneKind::TriangleMesh, 4.0), (SceneKind::Bsp, 10.0)],
    );
    let r = c.check(TraceExtent::Box).unwrap();
    let part = checked(9.0, 28.0);
    assert!(close(part, 0.5, 1e-5), "{part}");
    assert_eq!(r.hit.kind, SceneKind::TriangleMesh);
    assert!(close(r.time, 4.0 / 28.0, 1e-7), "{r:?}");
    assert!(close(r.checked_fraction, part, 1e-6), "{r:?}");
    assert!(
        close(r.reported * 28.0, 4.0 - 0.1 * 28.0 * part, 1e-4),
        "{r:?}"
    );
    assert!(close(r.reported * 28.0, 2.6, 1e-3), "{r:?}");
    // Asked: everything, then BSP alone. No third question: nothing else
    // could be reported before triangles touched first.
    assert_eq!(
        c.asked,
        vec![(KindSet::ALL, 1.0), (KindSet::only(SceneKind::Bsp), 1.0)]
    );
}

#[test]
fn the_contact_reported_first_need_not_be_the_one_touched_first() {
    // A 28 uu box trace. BSP 10 uu down is reported at 9.0; the rest is
    // asked on 14 uu of trace, where triangles are reported 1.4 uu short.
    // Triangles 10.3 uu down (touched after the BSP) are reported at 8.9,
    // before the BSP; at 10.5 they are reported at 9.1, after it.
    let part = checked(9.0, 28.0);
    let mut c = Contacts::new(
        28.0,
        &[(SceneKind::Bsp, 10.0), (SceneKind::TriangleMesh, 10.3)],
    );
    let r = c.check(TraceExtent::Box).unwrap();
    assert_eq!(r.hit.kind, SceneKind::TriangleMesh);
    assert!(close(r.time * 28.0, 10.3, 1e-4), "{r:?}");
    assert!(
        close(r.reported * 28.0, 10.3 - 0.1 * 28.0 * part, 1e-4),
        "{r:?}"
    );
    assert!(r.reported * 28.0 < 9.0);
    let mut c = Contacts::new(
        28.0,
        &[(SceneKind::Bsp, 10.0), (SceneKind::TriangleMesh, 10.5)],
    );
    let r = c.check(TraceExtent::Box).unwrap();
    assert_eq!(r.hit.kind, SceneKind::Bsp);
    assert!(close(r.reported * 28.0, 9.0, 1e-4), "{r:?}");
    assert_eq!(r.checked_fraction, 1.0, "BSP sees the whole trace");

    // A simple body is never reported before a BSP surface it lies behind:
    // on the shortened trace it is moved back by at most the BSP's 1.0 uu.
    // Level with the BSP both are reported at 9.0, and the BSP is kept.
    let mut c = Contacts::new(
        28.0,
        &[(SceneKind::SimpleHull, 10.0), (SceneKind::Bsp, 10.0)],
    );
    let r = c.check(TraceExtent::Box).unwrap();
    assert_eq!(r.hit.kind, SceneKind::Bsp);

    // The shortening hides what an unshortened trace would report first. On
    // a 100 uu trace with BSP 10 uu down (reported at 9, the rest asked on
    // 14 uu), triangles 12.5 uu down are reported 1.4 short at 11.1: after
    // the BSP. On the whole trace they would be moved back 4.0, to 8.5.
    let mut c = Contacts::new(
        100.0,
        &[(SceneKind::Bsp, 10.0), (SceneKind::TriangleMesh, 12.5)],
    );
    let r = c.check(TraceExtent::Box).unwrap();
    assert_eq!(r.hit.kind, SceneKind::Bsp);
    assert!(close(r.reported * 100.0, 9.0, 1e-3), "{r:?}");
    // ... and what lies beyond those 14 uu is not asked for at all.
    assert!(
        c.asked
            .iter()
            .skip(1)
            .all(|(_, part)| close(*part, 0.14, 1e-4)),
        "{:?}",
        c.asked
    );
}

#[test]
fn a_bsp_hit_reported_at_the_start_ends_the_check() {
    // A 20 uu box trace with BSP 0.9 uu away: reported 1.0 uu short, at
    // the very start (time 0). Nothing else is asked then, not even a
    // simple body touched before the BSP.
    let mut c = Contacts::new(20.0, &[(SceneKind::SimpleHull, 0.5), (SceneKind::Bsp, 0.9)]);
    let r = c.check(TraceExtent::Box).unwrap();
    assert_eq!(r.hit.kind, SceneKind::Bsp);
    assert_eq!(r.reported, 0.0);
    assert!(close(r.time * 20.0, 0.9, 1e-5), "{r:?}");
    assert_eq!(
        c.asked,
        vec![(KindSet::ALL, 1.0), (KindSet::only(SceneKind::Bsp), 1.0)]
    );
}

#[test]
fn only_what_could_be_reported_earlier_is_asked_for() {
    // A 7 uu move stopped by a simple body: a tenth of 7 uu is under both
    // caps, so triangles touched later cannot be reported earlier. Asked:
    // everything, then BSP (for the shortening) and nothing more.
    let mut c = Contacts::new(7.0, &[(SceneKind::SimpleHull, 3.0)]);
    let r = c.check(TraceExtent::Box).unwrap();
    assert!(close(r.reported * 7.0, 3.0 - 0.7, 1e-5), "{r:?}");
    assert_eq!(c.asked.len(), 2, "{:?}", c.asked);
    // A 28 uu trace stopped by a simple body 10 uu away (reported at 9.0):
    // triangles within 1.8 uu behind it would be reported first (2.8 short),
    // so they are asked for; 11 uu away they are found and win (8.2).
    let mut c = Contacts::new(
        28.0,
        &[
            (SceneKind::SimpleHull, 10.0),
            (SceneKind::TriangleMesh, 11.0),
        ],
    );
    let r = c.check(TraceExtent::Box).unwrap();
    assert_eq!(r.hit.kind, SceneKind::TriangleMesh);
    assert!(close(r.reported * 28.0, 11.0 - 2.8, 1e-4), "{r:?}");
    assert_eq!(c.asked.len(), 3, "{:?}", c.asked);
    assert_eq!(c.asked[2].0, KindSet::only(SceneKind::TriangleMesh));
}

#[test]
fn a_line_trace_reports_bsp_half_a_unit_short_and_triangles_four() {
    // The 16 384 uu fire trace. BSP 1000 uu away: 999.5. Triangles: a tenth
    // of the trace is far above the cap: 4.0 short, 996.0.
    let mut c = Contacts::new(16_384.0, &[(SceneKind::Bsp, 1000.0)]);
    let r = c.check(TraceExtent::Zero).unwrap();
    assert!(close(r.reported * 16_384.0, 999.5, 2e-3), "{r:?}");
    let mut c = Contacts::new(16_384.0, &[(SceneKind::TriangleMesh, 1000.0)]);
    let r = c.check(TraceExtent::Zero).unwrap();
    assert!(close(r.reported * 16_384.0, 996.0, 2e-3), "{r:?}");
    // Triangles 2 uu behind a BSP face: the BSP is reported at 999.5, the
    // rest asked on 1004.5 uu of trace (the triangles, 1002 uu away, are on
    // it) and moved back 4.0: 998.0, before the BSP.
    let mut c = Contacts::new(
        16_384.0,
        &[(SceneKind::Bsp, 1000.0), (SceneKind::TriangleMesh, 1002.0)],
    );
    let r = c.check(TraceExtent::Zero).unwrap();
    assert_eq!(r.hit.kind, SceneKind::TriangleMesh);
    assert!(close(r.reported * 16_384.0, 998.0, 2e-3), "{r:?}");
    assert!(close(r.checked_fraction * 16_384.0, 1004.5, 2e-2), "{r:?}");
    // 6 uu behind: beyond the 5 uu looked at. The BSP is reported.
    let mut c = Contacts::new(
        16_384.0,
        &[(SceneKind::Bsp, 1000.0), (SceneKind::TriangleMesh, 1006.0)],
    );
    assert_eq!(c.check(TraceExtent::Zero).unwrap().hit.kind, SceneKind::Bsp);
}

// ---------------------------------------------------------------------------
// A converted scene: what the box and the trace meet.
// ---------------------------------------------------------------------------

/// Half sizes of the pawn's box.
const BOX: Vec3 = Vec3::new(21.0, 21.0, 44.0);

/// Slots of the fixture's actors that tests refer to.
struct Mixed {
    src: MemorySource,
    volume: u32,
}

/// A BSP floor at z = 0 with, 1000 uu apart along X:
///
/// - at 0: a rug, two triangles 0.2 uu above the floor, a mesh without a
///   simple body (the original's pawn walks through such a mesh, P1);
/// - at 1000: a block 200 × 200 × 100 with a simple body of the same shape
///   (the pawn meets the body, bullet traces the triangles, P2);
/// - at 2000: the same block as a mesh whose box switch is stored `false`
///   (the pawn meets its triangles);
/// - at 3000: a sheet of such triangles 6 uu above the floor;
/// - at 4000: a blocking volume that starts switched off (P5);
/// - at 6000: a wall 2000 uu high with a simple body.
fn mixed() -> Mixed {
    let mut src = MemorySource::new();
    let mut s = SceneFixture::new("AG-Mixed", -50_000.0);
    let half = 20_000.0;
    s.set_bsp(
        vec![
            [-half, -half, 0.0],
            [half, -half, 0.0],
            [half, half, 0.0],
            [-half, half, 0.0],
        ],
        vec![[0, 1, 2], [0, 2, 3]],
    );
    s.player_start(Vec3::new(-2000.0, 0.0, 80.0), 0);
    s.static_mesh("Pkg.Rug", Place::at(Vec3::new(0.0, 0.0, 0.2)));
    s.static_mesh("Pkg.Block", Place::at(Vec3::new(1000.0, 0.0, 0.0)));
    s.static_mesh("Pkg.Slab", Place::at(Vec3::new(2000.0, 0.0, 0.0)));
    s.static_mesh("Pkg.Sheet", Place::at(Vec3::new(3000.0, 0.0, 6.0)));
    let volume = s.dynamic_blocking_volume(
        Vec3::new(3900.0, -200.0, 0.0),
        Vec3::new(4000.0, 200.0, 400.0),
        false,
    );
    s.static_mesh("Pkg.Wall", Place::at(Vec3::new(6000.0, 0.0, 0.0)));
    s.write(&mut src);
    let mut meshes = MeshFixtures::new();
    let quad = |z: f32| {
        (
            vec![
                [-100.0, -100.0, z],
                [100.0, -100.0, z],
                [100.0, 100.0, z],
                [-100.0, 100.0, z],
            ],
            vec![[0, 1, 2], [0, 2, 3]],
        )
    };
    let (v, t) = quad(0.0);
    meshes.add("Pkg.Rug", "AG-Mixed", v, t);
    let (lo, hi) = (
        Vec3::new(-100.0, -100.0, 0.0),
        Vec3::new(100.0, 100.0, 100.0),
    );
    meshes.add_solid_box("Pkg.Block", "AG-Mixed", lo, hi);
    let (v, t) = box_mesh(lo, hi);
    meshes.add("Pkg.Slab", "AG-Mixed", v, t);
    meshes.set_switches("Pkg.Slab", false, false);
    let (v, t) = quad(0.0);
    meshes.add("Pkg.Sheet", "AG-Mixed", v, t);
    meshes.set_switches("Pkg.Sheet", false, false);
    meshes.add_solid_box(
        "Pkg.Wall",
        "AG-Mixed",
        Vec3::new(0.0, -500.0, 0.0),
        Vec3::new(100.0, 500.0, 2000.0),
    );
    meshes.write_v2(&mut src, true);
    Mixed {
        src,
        volume: scene::actor_id(0, volume).expect("slot fits"),
    }
}

fn game(m: &Mixed) -> Game {
    let loaded = scene::load_map(&m.src, "AG-Mixed", &LoadOptions::default()).expect("loads");
    assert!(loaded.warnings.is_empty(), "{:?}", loaded.warnings);
    let mut g =
        Game::from_loaded_map(loaded, PlayerParams::asamu_original(), 60.0).expect("game starts");
    g.start();
    g
}

fn idle() -> InputFrame {
    InputFrame::default()
}

#[test]
fn the_box_meets_bodies_and_the_bullet_trace_triangles() {
    let m = mixed();
    let g = game(&m);
    let w = g.world();
    // A 100 uu box trace straight down: the bottom starts 56 uu above the
    // surface it will meet, so the exact contact is at 0.56 of the trace.
    let down = |x: f32, top: f32| {
        let start = Vec3::new(x, 0.0, top + 100.0);
        w.sweep_box(start, start - Vec3::Z * 100.0, BOX)
            .expect("a floor")
    };
    // Over the rug: the BSP floor. The rug has no body and is not there for
    // the pawn. Reported 1.0 uu short (a tenth of 100 uu, capped at 1.0).
    let hit = down(0.0, 0.0);
    assert_eq!(hit.kind, RoutineKind::Bsp);
    assert!(close(hit.hit.time, 0.56, 1e-6), "{hit:?}");
    assert!(close(
        hit.reported_time(TraceExtent::Box, 100.0),
        0.55,
        1e-6
    ));
    assert_eq!(hit.hit.normal, Vec3::Z);
    // On the block: its simple body. The trace ends 56 uu above the BSP, so
    // BSP reports nothing and the body's routine sees the whole trace.
    let hit = down(1000.0, 100.0);
    assert_eq!(hit.kind, RoutineKind::SimpleHull);
    assert!(close(hit.hit.time, 0.56, 1e-6), "{hit:?}");
    assert_eq!(hit.checked_fraction, 1.0);
    assert!(close(
        hit.reported_time(TraceExtent::Box, 100.0),
        0.55,
        1e-6
    ));
    // On the block whose box switch is off: its triangles, reported 4.0 uu
    // short (a tenth of 100 uu, capped at 4.0).
    let hit = down(2000.0, 100.0);
    assert_eq!(hit.kind, RoutineKind::TriangleMesh);
    assert!(close(hit.hit.time, 0.56, 1e-6), "{hit:?}");
    assert!(close(
        hit.reported_time(TraceExtent::Box, 100.0),
        0.52,
        1e-6
    ));

    // The bullet trace, 1000 uu straight down from z = 500.
    let line = |x: f32| {
        w.trace_line(Vec3::new(x, 0.0, 500.0), Vec3::NEG_Z, 1000.0)
            .expect("a surface")
    };
    // At the rug: BSP 500 uu down is reported at 499.5, the rest asked on
    // 504.5 uu of trace; the rug's triangles, 499.8 uu down, are moved back
    // 4.0 to 495.8 and are what the trace reports, though the pawn's box
    // never meets them.
    let hit = line(0.0);
    assert_eq!(hit.kind, RoutineKind::TriangleMesh);
    assert!(close(hit.hit.time * 1000.0, 499.8, 1e-2), "{hit:?}");
    assert!(close(hit.hit.position.z, 0.2, 1e-2), "{hit:?}");
    assert!(close(hit.checked_fraction, checked(499.5, 1000.0), 1e-5));
    assert!(
        close(
            hit.reported_time(TraceExtent::Zero, 1000.0) * 1000.0,
            495.8,
            1e-2
        ),
        "{hit:?}"
    );
    // At the block: the triangles of its top (400 uu down), 4.0 uu short;
    // never the simple body.
    let hit = line(1000.0);
    assert_eq!(hit.kind, RoutineKind::TriangleMesh);
    assert!(
        close(
            hit.reported_time(TraceExtent::Zero, 1000.0) * 1000.0,
            396.0,
            1e-2
        ),
        "{hit:?}"
    );
    // Away from every mesh: the BSP, a flat 0.5 uu short.
    let hit = line(-1000.0);
    assert_eq!(hit.kind, RoutineKind::Bsp);
    assert!(
        close(
            hit.reported_time(TraceExtent::Zero, 1000.0) * 1000.0,
            499.5,
            1e-2
        ),
        "{hit:?}"
    );
    // The overlap test is the box's: over the rug the pawn's box is free
    // 1 uu above the floor, and inside the block's body it is not.
    assert!(!w.overlaps_box(Vec3::new(0.0, 0.0, 45.0), BOX));
    assert!(w.overlaps_box(Vec3::new(1000.0, 0.0, 100.0), BOX));
}

#[test]
fn the_floor_check_over_triangles_above_bsp_is_shortened_by_the_bsp_hit() {
    let m = mixed();
    let g = game(&m);
    // The 28 uu floor check from 4 uu above the sheet (6 uu above the BSP):
    // BSP 10 uu down is reported at 9.0, the rest asked on 14 uu, the
    // sheet's triangles reported 1.4 uu short at 2.6 uu.
    let start = Vec3::new(3000.0, 0.0, 6.0 + 4.0 + 44.0);
    let hit = g
        .world()
        .sweep_box(start, start - Vec3::Z * 28.0, BOX)
        .unwrap();
    assert_eq!(hit.kind, RoutineKind::TriangleMesh);
    assert!(close(hit.hit.time * 28.0, 4.0, 1e-4), "{hit:?}");
    assert!(close(hit.checked_fraction, checked(9.0, 28.0), 1e-5));
    assert!(
        close(hit.reported_time(TraceExtent::Box, 28.0) * 28.0, 2.6, 1e-3),
        "{hit:?}"
    );
    // The same check over the block of triangles, whose top is 100 uu above
    // the BSP (out of the trace's reach): 2.8 uu short, at 1.2 uu.
    let start = Vec3::new(2000.0, 0.0, 100.0 + 4.0 + 44.0);
    let hit = g
        .world()
        .sweep_box(start, start - Vec3::Z * 28.0, BOX)
        .unwrap();
    assert_eq!(hit.kind, RoutineKind::TriangleMesh);
    assert_eq!(hit.checked_fraction, 1.0);
    assert!(
        close(hit.reported_time(TraceExtent::Box, 28.0) * 28.0, 1.2, 1e-3),
        "{hit:?}"
    );
}

/// Stands a walking pawn, based on its floor, with its bottom `gap` uu
/// above a floor whose top is at `top`, with the floor check forced, and
/// runs one idle tick.
fn stand(g: &mut Game, x: f32, top: f32, gap: f32) -> f32 {
    let p = g.player_mut();
    p.position = Vec3::new(x, 0.0, top + 44.0 + gap);
    p.velocity = Vec3::ZERO;
    p.grounded = true;
    p.pawn = PawnPhysicsState {
        floor: Vec3::Z,
        based: true,
        force_floor_check: true,
        ..PawnPhysicsState::default()
    };
    g.tick(&idle()).expect("playing");
    assert!(g.player().grounded, "still walking at {x}");
    g.player().position.z - 44.0 - top
}

#[test]
fn standing_heights_follow_the_kind_of_the_floor() {
    let m = mixed();
    let mut g = game(&m);
    // The walking floor check leaves a *reported* floor distance between
    // 1.9 and 2.4 uu alone and raises a smaller one to 2.15
    // (NATIVE_PHYSICS.md 3.3). Its 28 uu trace (`MaxStepHeight` 26 + 2)
    // reports BSP and simple bodies 1.0 uu nearer, triangles 2.8 uu nearer.
    // BSP, from 2.5 uu: reported 1.5, raised by 0.65 to a true 3.15.
    assert!(close(stand(&mut g, -1000.0, 0.0, 2.5), 3.15, 1e-3));
    // Over the rug the floor is the BSP all the same.
    assert!(close(stand(&mut g, 0.0, 0.0, 2.5), 3.15, 1e-3));
    // On the simple body: the same.
    assert!(close(stand(&mut g, 1000.0, 100.0, 2.5), 3.15, 1e-3));
    // On mesh triangles, from 3.0 uu: reported 0.2, raised by 1.95 to 4.95.
    assert!(close(stand(&mut g, 2000.0, 100.0, 3.0), 4.95, 1e-3));
    // Inside the band nothing moves: 3.0 uu above BSP is reported 2.0.
    assert!(close(stand(&mut g, -1000.0, 0.0, 3.0), 3.0, 1e-4));
}

#[test]
fn walking_into_a_simple_body_stops_a_tenth_of_a_unit_from_it() {
    let m = mixed();
    let mut g = game(&m);
    // On the BSP floor 50 uu before the block's face at x = 900, facing +X.
    let p = g.player_mut();
    p.position = Vec3::new(850.0 - 21.0, 0.0, 44.0 + 3.15);
    p.velocity = Vec3::ZERO;
    p.grounded = true;
    p.pawn = PawnPhysicsState::default();
    let forward = InputFrame {
        move_forward: 1.0,
        ..InputFrame::default()
    };
    for _ in 0..120 {
        g.tick(&forward).unwrap();
    }
    // A move sweeps the box 1.001 times as wide (21.021 uu, the contact
    // plane measured at the Workshop door, V11), and a blocked pawn keeps
    // pushing with moves under 1 uu, each reported 0.1 uu short: it rests
    // 21.021 + 0.1 uu from the face.
    let x = g.player().position.x;
    assert!(close(x, 900.0 - 21.0 * 1.001 - 0.1, 2e-3), "{x}");
    assert!(g.player().grounded);
}

#[test]
fn a_blocking_volume_blocks_only_while_it_is_switched_on() {
    let m = mixed();
    let mut g = game(&m);
    let (from, to) = (Vec3::new(3800.0, 0.0, 100.0), Vec3::new(4100.0, 0.0, 100.0));
    // Starts off (`bEnabled` false): the box passes, and stands inside.
    assert_eq!(g.actor_collision(m.volume), Some(false));
    assert!(g.world().sweep_box(from, to, BOX).is_none());
    assert!(!g.world().overlaps_box(Vec3::new(3950.0, 0.0, 100.0), BOX));
    // Switched on: stopped at the face x = 3900, the centre 21 uu before
    // it, 79 uu into the 300 uu trace; a brush, reported 1.0 uu short.
    assert!(g.set_actor_collision(m.volume, true));
    assert_eq!(g.actor_collision(m.volume), Some(true));
    let hit = g.world().sweep_box(from, to, BOX).expect("blocked");
    assert_eq!(hit.kind, RoutineKind::BrushHull);
    assert!(close(hit.hit.time * 300.0, 79.0, 1e-3), "{hit:?}");
    assert!(close(
        hit.reported_time(TraceExtent::Box, 300.0) * 300.0,
        78.0,
        1e-3
    ));
    assert!(g.world().overlaps_box(Vec3::new(3950.0, 0.0, 100.0), BOX));
    // The volume never stops the bullet trace.
    assert!(g.world().trace_line(from, Vec3::X, 300.0).is_none());
    // Off again.
    assert!(g.set_actor_collision(m.volume, false));
    assert!(g.world().sweep_box(from, to, BOX).is_none());
    // An actor without switchable collision: nothing to switch.
    assert!(!g.set_actor_collision(0xFFFF, true));
    assert_eq!(g.actor_collision(0xFFFF), None);
}

/// One tick with the fire button pressed, from rest in the air `distance`
/// uu before the wall's face (x = 6000), the centre at z = 1000.
fn fire_at_the_wall(m: &Mixed, distance: f32) -> (Game, TickReport) {
    let mut g = game(m);
    g.set_max_grapples(3);
    let p = g.player_mut();
    p.position = Vec3::new(6000.0 - distance, 0.0, 1000.0);
    p.velocity = Vec3::ZERO;
    p.grounded = false;
    p.pawn = PawnPhysicsState::default();
    let fire = InputFrame {
        grapple_held: true,
        ..InputFrame::default()
    };
    let r = g.tick(&fire).expect("playing");
    (g, r)
}

#[test]
fn the_anchor_is_four_units_before_the_triangles_and_decides_the_release() {
    let m = mixed();
    // The view is level (the start's yaw 0, no look input), the eye 38 uu
    // above the centre. The fire trace meets the wall's triangles at
    // (6000, 0, 1038) and reports them 4.0 uu short along the trace (a
    // tenth of 16 384 uu, capped at 4.0; measured 3.9967 uu at 47 recorded
    // anchors, V16): the anchor is (5996, 0, 1038).
    //
    // 198.393 uu before the wall the surface point is √(198.393² + 38²) =
    // 202 uu from the centre, outside the 200 uu release distance, but the
    // anchor is √(194.393² + 38²) = 198.07 uu away: the gun's first tick
    // releases at once (P3: the original's presses whose first measured
    // distance was 197.0 to 199.9 released in the press frame).
    let (g, r) = fire_at_the_wall(&m, 198.393);
    assert_eq!(r.events.gun.fire, Some(FireOutcome::Attached));
    let anchor = r.events.gun.attached.expect("attached").anchor;
    assert!(
        (anchor - Vec3::new(5996.0, 0.0, 1038.0)).length() < 0.01,
        "{anchor}"
    );
    assert!(close(g.player().script.gun.distance, 198.07, 0.01));
    let released = r.events.gun.released.expect("released in the press tick");
    assert_eq!(released.reason, ReleaseReason::Proximity);
    assert!(!g.player().is_grapple_attached());
    // 201.447 uu before the wall the surface point is 205 uu away and the
    // anchor √(197.447² + 38²) = 201.07 uu: it stays attached.
    let (g, r) = fire_at_the_wall(&m, 201.447);
    assert_eq!(r.events.gun.fire, Some(FireOutcome::Attached));
    assert!(r.events.gun.released.is_none());
    assert!(g.player().is_grapple_attached());
    assert!(close(g.player().script.gun.distance, 201.07, 0.01));
}

#[test]
fn the_crosshair_measures_to_the_reported_hit() {
    // G-TG-2: every gun tick a second bullet trace, `fMaxDistance` + 1000 =
    // 6000 uu long, decides the HUD crosshair; outside story mode it is
    // positive when the hit location is closer than `fMaxDistance` (5000 uu)
    // to the eye, grapples remain and the hit is an acceptable static mesh.
    // The hit location is the reported one: on the wall's triangles a tenth
    // of the 6000 uu trace is far above the cap, so 4.0 uu before the
    // surface (the same rule as the anchor's, G-AT-4).
    //
    // The view is level and the eye straight above the centre, so the trace
    // meets the wall's face (x = 6000) `distance` uu from the eye.
    let m = mixed();
    let crosshair_at = |distance: f32| {
        let mut g = game(&m);
        g.set_max_grapples(3);
        let p = g.player_mut();
        p.position = Vec3::new(6000.0 - distance, 0.0, 1000.0);
        p.velocity = Vec3::ZERO;
        p.grounded = false;
        p.pawn = PawnPhysicsState::default();
        g.tick(&idle()).expect("playing");
        g.crosshair()
    };
    // Well inside the range.
    assert!(crosshair_at(4000.0));
    // The surface 5002 uu away is reported at 4998: in range (the exact
    // contact, 5002, would not be).
    assert!(crosshair_at(5002.0));
    // The surface 5006 uu away is reported at 5002: out of range.
    assert!(!crosshair_at(5006.0));
}
