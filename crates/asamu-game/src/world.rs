//! The collision world a [`crate::Game`] hands to the player simulation.
//!
//! [`GameWorld`] combines the axis-aligned boxes of hand-made levels
//! ([`BoxWorld`]) with the collision of a converted level
//! ([`SceneCollision`]: the static [`asamu_world::collision::CollisionScene`]
//! shared through an `Arc`, plus the current dynamic instances, the loaded
//! sub-levels and the state the gameplay script reads from hit actors —
//! crystal charge, moving actors' locations). It implements
//! [`CollisionWorld`].
//!
//! # The original's world check
//!
//! The queries of the original's movement and traces are
//! [`CollisionWorld::sweep_box`] (the pawn's moves and its floor check: an
//! axis-aligned box, `docs/PARITY_FINDINGS.md` P4) and
//! [`CollisionWorld::trace_line`] (the grapple's fire trace: a zero-extent
//! bullet trace). A converted level answers them the way the original's
//! world check does ([`SceneCollision::sweep_box`],
//! [`SceneCollision::trace_line`]):
//!
//! - **What is asked.** The box meets what the level loader placed for the
//!   pawn: each mesh's simple body, its triangles when the mesh's box switch
//!   is off, nothing for a mesh without a body, the BSP, the hulls of
//!   blocking volumes that are switched on, pawn cylinders
//!   ([`asamu_world::scene`], P1, P2, P5). The bullet trace meets the
//!   triangles of every mesh, whatever its switches.
//! - **Which contact is reported.** Not simply the first one touched. Every
//!   collision routine of the original moves the hit it reports back along
//!   the trace by its own amount ([`asamu_player::world::pull_back`], P3),
//!   and the world check asks the level's BSP first and everything else
//!   only on a trace shortened to end 5 uu beyond the BSP hit
//!   ([`asamu_player::world::bsp_checked_fraction`], NATIVE_PHYSICS.md
//!   10.2). The contact returned is the one whose **reported** time is the
//!   earliest ([`reported_first`]), with the kind of its primitive and the
//!   part of the trace its routine saw, so that the caller's
//!   [`TraceHit::reported_time`] gives the original's hit time. The hit's
//!   own time and position stay those of the exact contact: the pull-back
//!   is applied once, by the caller.
//!
//! [`CollisionWorld::sweep_capsule`] (an upright cylinder, exact first
//! contact) and [`CollisionWorld::raycast`] (exact first contact of what
//! blocks bullet traces) remain for our own helpers: the placeholder
//! movement model and rope, placement, the base-mover probe, effects. They
//! are not the original's queries.
//!
//! Boxes and scene together (only hand-made levels have boxes): the box
//! world answers as BSP and the earlier reported time wins; a tie keeps the
//! box.

use std::sync::Arc;

use asamu_player::BoxWorld;
use asamu_player::world::{
    ActorClass, ActorTag, CollisionShape, CollisionWorld, Hit, MIN_MOVE,
    PrimitiveKind as RoutineKind, Surface, TraceExtent, TraceHit, bsp_checked_fraction, pull_back,
    reported_time,
};
use asamu_world::SurfaceTag;
use asamu_world::collision::{
    CollisionClass, Instance, KindSet, PrimitiveKind as SceneKind, QueryFilter, QueryKind, SceneHit,
};
use asamu_world::scene::LoadedMap;
use glam::Vec3;

/// The routine of the original a scene primitive's kind stands for (the two
/// crates name the same five kinds).
#[must_use]
pub fn routine_kind(kind: SceneKind) -> RoutineKind {
    match kind {
        SceneKind::Bsp => RoutineKind::Bsp,
        SceneKind::BrushHull => RoutineKind::BrushHull,
        SceneKind::SimpleHull => RoutineKind::SimpleHull,
        SceneKind::TriangleMesh => RoutineKind::TriangleMesh,
        SceneKind::PawnCylinder => RoutineKind::PawnCylinder,
    }
}

/// A contact chosen by [`reported_first`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ReportedFirst {
    /// The contact (its `t` belongs to the part of the trace it was found
    /// on; use [`Self::time`]).
    pub hit: SceneHit,
    /// Exact time of the contact as a fraction of the whole trace.
    pub time: f32,
    /// The part of the trace the primitive's routine saw: 1 for BSP and
    /// when BSP reported no hit, else
    /// [`asamu_player::world::bsp_checked_fraction`].
    pub checked_fraction: f32,
    /// The hit time the original reports for it (the time the choice was
    /// made on).
    pub reported: f32,
}

/// The sets of kinds whose routines move a hit back by the same rule, each
/// with one kind to compute that rule with: brushes and simple bodies
/// (rule K, K = 1.0), mesh triangles (K = 4.0), pawn cylinders.
const ROUTINE_CLASSES: [(KindSet, RoutineKind); 3] = [
    (
        KindSet::only(SceneKind::BrushHull).with(SceneKind::SimpleHull),
        RoutineKind::BrushHull,
    ),
    (
        KindSet::only(SceneKind::TriangleMesh),
        RoutineKind::TriangleMesh,
    ),
    (
        KindSet::only(SceneKind::PawnCylinder),
        RoutineKind::PawnCylinder,
    ),
];

/// The contact the original's world check reports first on a trace
/// `length` uu long, given `first_of(kinds, part)`: the exact first contact
/// among the primitives of `kinds` on the first `part` (in `(0, 1]`) of the
/// trace, with its time as a fraction of that part.
///
/// The original (`UWorld::MultiLineCheck`, NATIVE_PHYSICS.md 10.2,
/// CONFIRMED in the user's executable; not measured on a recording where
/// kinds are mixed):
///
/// 1. asks the level's BSP. Its routine reports the hit moved back
///    ([`pull_back`]);
/// 2. after a BSP hit, shortens the trace to end 5 uu beyond that reported
///    hit ([`bsp_checked_fraction`]; nothing else is asked when the BSP hit
///    is reported at the very start);
/// 3. asks the other primitives on that trace. Each routine moves its own
///    hit back by its own rule, computed with the shortened length
///    ([`reported_time`]);
/// 4. takes the earliest of the reported hits.
///
/// So the contact reported need not be the first one touched: mesh
/// triangles are moved back by up to 4 uu, everything else by at most 1.
/// On equal reported times BSP is kept, then the contact touched first
/// (ours: the original's order among equal times was not read).
///
/// To find it, the first contact of all is taken first; a routine class
/// (kinds sharing one rule) is asked separately only when a contact of it
/// at that same time would be reported before the best so far. A class's
/// first contact is also its first reported one, because one rule on one
/// length keeps the order of times.
pub fn reported_first<F>(extent: TraceExtent, length: f32, mut first_of: F) -> Option<ReportedFirst>
where
    F: FnMut(KindSet, f32) -> Option<SceneHit>,
{
    let on_whole = |h: &SceneHit| (h.t as f32).clamp(0.0, 1.0);
    let first = first_of(KindSet::ALL, 1.0)?;
    let first_time = on_whole(&first);
    let bsp = if first.kind == SceneKind::Bsp {
        Some(first)
    } else {
        first_of(KindSet::only(SceneKind::Bsp), 1.0)
    };
    let mut best: Option<ReportedFirst> = None;
    let mut fraction = 1.0;
    if let Some(hit) = bsp {
        let time = on_whole(&hit);
        let reported = pull_back(RoutineKind::Bsp, extent, length, time);
        best = Some(ReportedFirst {
            hit,
            time,
            checked_fraction: 1.0,
            reported,
        });
        fraction = bsp_checked_fraction(reported, length);
        if fraction <= 0.0 {
            return best;
        }
    }
    let consider = |best: &mut Option<ReportedFirst>, hit: SceneHit, time: f32| {
        let reported = reported_time(routine_kind(hit.kind), extent, length, time, fraction);
        if best.is_none_or(|b| reported < b.reported) {
            *best = Some(ReportedFirst {
                hit,
                time,
                checked_fraction: fraction,
                reported,
            });
        }
    };
    if first.kind != SceneKind::Bsp && first_time <= fraction {
        consider(&mut best, first, first_time);
    }
    for (kinds, rule) in ROUTINE_CLASSES {
        if kinds.contains(first.kind) {
            // `first` is this class's first contact.
            continue;
        }
        let bound = reported_time(rule, extent, length, first_time, fraction);
        if best.is_some_and(|b| bound >= b.reported) {
            continue;
        }
        if let Some(hit) = first_of(kinds, fraction) {
            let time = ((hit.t * f64::from(fraction)) as f32).clamp(0.0, 1.0);
            consider(&mut best, hit, time);
        }
    }
    best
}

/// Collision of a converted level with its run-time state.
#[derive(Clone, Debug)]
pub struct SceneCollision {
    /// The loaded map (static collision and gameplay definitions).
    pub map: Arc<LoadedMap>,
    /// Current dynamic instances (same order as `map.dynamic`).
    pub dynamic: Vec<Instance>,
    /// Loaded levels (bit per level index).
    pub level_mask: u64,
    /// `(crystal id, charged)`, sorted by id.
    pub crystals: Vec<(u32, bool)>,
    /// `(actor id, location)` of moving actors, sorted by id.
    pub actor_locations: Vec<(u32, Vec3)>,
}

impl SceneCollision {
    /// The state at level start (crystals charged, actors at rest).
    #[must_use]
    pub fn new(map: Arc<LoadedMap>) -> Self {
        let dynamic = map.dynamic.clone();
        let level_mask = map.initial_level_mask();
        let mut crystals: Vec<(u32, bool)> =
            map.actors.crystals.iter().map(|c| (c.id, true)).collect();
        crystals.sort_unstable_by_key(|c| c.0);
        let mut actor_locations: Vec<(u32, Vec3)> = map
            .actors
            .rocks
            .iter()
            .map(|r| (r.id, r.location))
            .collect();
        actor_locations.sort_unstable_by_key(|a| a.0);
        Self {
            map,
            dynamic,
            level_mask,
            crystals,
            actor_locations,
        }
    }

    fn filter(&self, kind: QueryKind) -> QueryFilter {
        QueryFilter {
            kind,
            sublevels: self.level_mask,
        }
    }

    fn surface(&self, hit: &SceneHit) -> (bool, Surface) {
        let Some(inst) = self.map.collision.instance(&self.dynamic, hit.instance) else {
            return (true, Surface::default());
        };
        let info = inst.info;
        let class = match info.class {
            CollisionClass::WorldGeometry | CollisionClass::BlockingVolume => {
                ActorClass::WorldGeometry
            }
            CollisionClass::StaticMesh => ActorClass::StaticMesh,
            CollisionClass::InterpActor => ActorClass::InterpActor,
            CollisionClass::RechargeCrystal => ActorClass::RechargeCrystal {
                charged: info
                    .actor
                    .and_then(|id| self.crystals.binary_search_by_key(&id, |c| c.0).ok())
                    .and_then(|i| self.crystals.get(i))
                    .is_none_or(|c| c.1),
            },
            CollisionClass::GlowFlower => ActorClass::GlowFlower,
            CollisionClass::FallingRock => ActorClass::FallingRock,
            CollisionClass::FallingWhenGrappledRock => ActorClass::FallingWhenGrappledRock,
            CollisionClass::Interactable => ActorClass::Interactable,
        };
        let tag = match info.tag {
            SurfaceTag::None => ActorTag::None,
            SurfaceTag::TopOnlyGrappleAble => ActorTag::TopOnlyGrappleAble,
            SurfaceTag::BottomOnlyGrappleAble => ActorTag::BottomOnlyGrappleAble,
            SurfaceTag::GrappleInteractable => ActorTag::GrappleInteractable,
            SurfaceTag::NotLandable => ActorTag::NotLandable,
        };
        (
            info.grapple_able,
            Surface {
                actor: info.actor,
                class,
                tag,
            },
        )
    }

    fn to_hit(&self, h: &SceneHit, start: Vec3, d: Vec3, length: f32) -> Hit {
        self.to_hit_at(h, (h.t as f32).clamp(0.0, 1.0), start, d, length)
    }

    /// The contact `h` at `t` of the trace `start + d·t`.
    fn to_hit_at(&self, h: &SceneHit, t: f32, start: Vec3, d: Vec3, length: f32) -> Hit {
        let normal = h.normal.as_vec3().normalize_or(Vec3::Z);
        let (grapple_able, surface) = self.surface(h);
        Hit {
            time: t,
            distance: t * length,
            position: start + d * t,
            normal,
            grapple_able,
            start_penetrating: h.penetrating,
            surface,
        }
    }

    /// The original's extent trace of the pawn: the axis-aligned box with
    /// the half sizes `half_extents` swept from `start` to `end` against
    /// what blocks the pawn, answered as the original's world check answers
    /// (module docs; [`reported_first`]). The hit's time is the exact
    /// contact's; [`TraceHit::reported_time`] with [`TraceExtent::Box`] and
    /// the trace's length gives the original's.
    #[must_use]
    pub fn sweep_box(&self, start: Vec3, end: Vec3, half_extents: Vec3) -> Option<TraceHit> {
        let d = end - start;
        let length = d.length();
        if !(length.is_finite() && start.is_finite() && half_extents.is_finite()) {
            return None;
        }
        let filter = self.filter(QueryKind::Pawn);
        let chosen = reported_first(TraceExtent::Box, length, |kinds, part| {
            // The whole trace ends at `end` itself, not at a recomputed point.
            let to = if part < 1.0 { start + d * part } else { end };
            self.map.collision.sweep_box_kinds(
                &self.dynamic,
                start,
                to,
                half_extents,
                filter,
                kinds,
            )
        })?;
        Some(self.trace_hit(&chosen, start, d, length))
    }

    /// The original's zero-extent bullet trace (the grapple's fire trace)
    /// from `origin` along `direction` for `max_distance` uu, against what
    /// blocks bullet traces (a mesh's triangles, never its simple body),
    /// answered as the original's world check answers ([`reported_first`]).
    /// The hit's time and position are the exact contact's;
    /// [`TraceHit::reported_time`] with [`TraceExtent::Zero`] and
    /// `max_distance` gives the original's.
    #[must_use]
    pub fn trace_line(&self, origin: Vec3, direction: Vec3, max_distance: f32) -> Option<TraceHit> {
        if !(origin.is_finite() && direction.is_finite() && max_distance.is_finite())
            || max_distance <= 0.0
        {
            return None;
        }
        let dir = direction.normalize_or_zero();
        if dir == Vec3::ZERO {
            return None;
        }
        let d = dir * max_distance;
        let filter = self.filter(QueryKind::Trace);
        let end = origin + d;
        let chosen = reported_first(TraceExtent::Zero, max_distance, |kinds, part| {
            let to = if part < 1.0 { origin + d * part } else { end };
            self.map
                .collision
                .raycast_kinds(&self.dynamic, origin, to, filter, kinds)
        })?;
        Some(self.trace_hit(&chosen, origin, d, max_distance))
    }

    fn trace_hit(&self, chosen: &ReportedFirst, start: Vec3, d: Vec3, length: f32) -> TraceHit {
        let hit = self.to_hit_at(&chosen.hit, chosen.time, start, d, length);
        TraceHit::new(hit, routine_kind(chosen.hit.kind))
            .with_checked_fraction(chosen.checked_fraction)
    }

    /// `true` when the axis-aligned box at `position` overlaps what blocks
    /// the pawn.
    #[must_use]
    pub fn overlaps_box(&self, position: Vec3, half_extents: Vec3) -> bool {
        self.map.collision.overlaps_box(
            &self.dynamic,
            position,
            half_extents,
            self.filter(QueryKind::Pawn),
        )
    }

    /// `true` when a zero-extent line check that is not a bullet trace hits
    /// something between `start` and `end` (a mesh answers such a check
    /// with its simple body or its triangles by its line switch;
    /// `asamu_world::collision::QueryKind::Line`).
    #[must_use]
    pub fn line_blocked(&self, start: Vec3, end: Vec3) -> bool {
        self.map
            .collision
            .raycast(&self.dynamic, start, end, self.filter(QueryKind::Line))
            .is_some()
    }

    /// Swept upright cylinder against what blocks the pawn: the exact first
    /// contact (our helpers' query, not the original's; module docs).
    #[must_use]
    pub fn sweep(&self, start: Vec3, end: Vec3, shape: CollisionShape) -> Option<Hit> {
        let d = end - start;
        let length = d.length();
        if !(length.is_finite() && start.is_finite()) || length < MIN_MOVE {
            return None;
        }
        let h = self.map.collision.sweep_cylinder(
            &self.dynamic,
            start,
            end,
            shape.radius,
            shape.half_height,
            self.filter(QueryKind::Pawn),
        )?;
        Some(self.to_hit(&h, start, d, length))
    }

    /// Zero-extent trace against what blocks bullet traces: the exact first
    /// contact (our helpers' query; the original's is [`Self::trace_line`]).
    #[must_use]
    pub fn ray(&self, origin: Vec3, direction: Vec3, max_distance: f32) -> Option<Hit> {
        if !(origin.is_finite() && direction.is_finite() && max_distance.is_finite())
            || max_distance <= 0.0
        {
            return None;
        }
        let dir = direction.normalize_or_zero();
        if dir == Vec3::ZERO {
            return None;
        }
        let d = dir * max_distance;
        let h = self.map.collision.raycast(
            &self.dynamic,
            origin,
            origin + d,
            self.filter(QueryKind::Trace),
        )?;
        Some(self.to_hit(&h, origin, d, max_distance))
    }

    /// `true` when the player cylinder at `position` overlaps the scene.
    #[must_use]
    pub fn overlaps(&self, position: Vec3, shape: CollisionShape) -> bool {
        self.map.collision.overlaps_cylinder(
            &self.dynamic,
            position,
            shape.radius,
            shape.half_height,
            self.filter(QueryKind::Pawn),
        )
    }

    /// Switches the collision of the actor `actor` on or off for every
    /// query (what a level script's toggle does to a blocking volume, P5):
    /// each of its **dynamic** instances. Returns how many there are.
    ///
    /// The static instances are shared and cannot change during play, so an
    /// actor whose collision can be switched is given dynamic instances
    /// before play: [`crate::Game::from_loaded_map`] does that for every
    /// actor that starts switched off, and [`crate::load_level_with_kismet`]
    /// for every actor a toggle action of the level script targets. For any
    /// other actor this returns 0 and changes nothing.
    pub fn set_actor_collision(&mut self, actor: u32, enabled: bool) -> usize {
        let mut n = 0;
        for inst in &mut self.dynamic {
            if inst.info.actor == Some(actor) {
                inst.enabled = enabled;
                n += 1;
            }
        }
        n
    }

    /// Whether the collision of the actor `actor` is switched on (`None`
    /// for an actor without collision here).
    #[must_use]
    pub fn actor_collision(&self, actor: u32) -> Option<bool> {
        asamu_world::scene::actor_collision(&self.map.collision, &self.dynamic, actor)
    }

    /// `true` when the actor's collision can be switched during play (it has
    /// dynamic instances; see [`Self::set_actor_collision`]).
    #[must_use]
    pub fn actor_switchable(&self, actor: u32) -> bool {
        self.dynamic.iter().any(|i| i.info.actor == Some(actor))
    }

    /// Current location of a moving actor.
    #[must_use]
    pub fn actor_location(&self, id: u32) -> Option<Vec3> {
        let i = self
            .actor_locations
            .binary_search_by_key(&id, |a| a.0)
            .ok()?;
        self.actor_locations.get(i).map(|a| a.1)
    }
}

/// Boxes plus an optional converted scene (see the module docs).
#[derive(Clone, Debug, Default)]
pub struct GameWorld {
    /// Hand-made boxes (graybox levels and their objects).
    pub boxes: BoxWorld,
    /// Converted level collision.
    pub scene: Option<SceneCollision>,
}

impl GameWorld {
    /// A world of boxes only.
    #[must_use]
    pub fn from_boxes(boxes: BoxWorld) -> Self {
        Self { boxes, scene: None }
    }

    /// `true` if the upright cylinder centred at `position` overlaps any
    /// solid (our helpers' shape; the pawn's is [`Self::overlaps_box`]).
    #[must_use]
    pub fn overlaps(&self, position: Vec3, shape: CollisionShape) -> bool {
        self.boxes.overlaps(position, shape)
            || self
                .scene
                .as_ref()
                .is_some_and(|s| s.overlaps(position, shape))
    }

    /// `true` when a zero-extent line check that is not a bullet trace hits
    /// something between `start` and `end`.
    #[must_use]
    pub fn line_blocked(&self, start: Vec3, end: Vec3) -> bool {
        let d = end - start;
        let length = d.length();
        if !(length.is_finite() && start.is_finite()) || length <= 0.0 {
            return false;
        }
        self.boxes.raycast(start, d, length).is_some()
            || self
                .scene
                .as_ref()
                .is_some_and(|s| s.line_blocked(start, end))
    }

    /// `true` if the axis-aligned box centred at `position` overlaps
    /// anything that blocks the pawn.
    #[must_use]
    pub fn overlaps_box(&self, position: Vec3, half_extents: Vec3) -> bool {
        self.boxes.overlaps_box(position, half_extents)
            || self
                .scene
                .as_ref()
                .is_some_and(|s| s.overlaps_box(position, half_extents))
    }
}

fn earliest(a: Option<Hit>, b: Option<Hit>) -> Option<Hit> {
    match (a, b) {
        (Some(x), Some(y)) => Some(if y.time < x.time { y } else { x }),
        (x, None) => x,
        (None, y) => y,
    }
}

/// The hit of `a` (the boxes) or `b` (the scene) the original would report
/// first on a trace `length` uu long; a tie keeps `a`.
fn earliest_reported(
    a: Option<TraceHit>,
    b: Option<TraceHit>,
    extent: TraceExtent,
    length: f32,
) -> Option<TraceHit> {
    match (a, b) {
        (Some(x), Some(y)) => Some(
            if y.reported_time(extent, length) < x.reported_time(extent, length) {
                y
            } else {
                x
            },
        ),
        (x, None) => x,
        (None, y) => y,
    }
}

impl CollisionWorld for GameWorld {
    fn sweep_capsule(&self, start: Vec3, end: Vec3, shape: CollisionShape) -> Option<Hit> {
        let boxes = self.boxes.sweep_capsule(start, end, shape);
        let scene = self.scene.as_ref().and_then(|s| s.sweep(start, end, shape));
        earliest(boxes, scene)
    }

    fn raycast(&self, origin: Vec3, direction: Vec3, max_distance: f32) -> Option<Hit> {
        let boxes = self.boxes.raycast(origin, direction, max_distance);
        let scene = self
            .scene
            .as_ref()
            .and_then(|s| s.ray(origin, direction, max_distance));
        earliest(boxes, scene)
    }

    fn actor_location(&self, id: u32) -> Option<Vec3> {
        self.boxes
            .actor_location(id)
            .or_else(|| self.scene.as_ref().and_then(|s| s.actor_location(id)))
    }

    fn sweep_box(&self, start: Vec3, end: Vec3, half_extents: Vec3) -> Option<TraceHit> {
        let boxes = self.boxes.sweep_box(start, end, half_extents);
        let scene = self
            .scene
            .as_ref()
            .and_then(|s| s.sweep_box(start, end, half_extents));
        earliest_reported(boxes, scene, TraceExtent::Box, (end - start).length())
    }

    fn trace_line(&self, origin: Vec3, direction: Vec3, max_distance: f32) -> Option<TraceHit> {
        let boxes = self.boxes.trace_line(origin, direction, max_distance);
        let scene = self
            .scene
            .as_ref()
            .and_then(|s| s.trace_line(origin, direction, max_distance));
        earliest_reported(boxes, scene, TraceExtent::Zero, max_distance)
    }
}
