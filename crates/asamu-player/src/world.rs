//! Collision interface used by the simulation, and simple deterministic
//! implementations: [`BoxWorld`] (axis-aligned boxes + ground plane) for tests
//! and the graybox prototype, [`SlopeWorld`] (a `BoxWorld` plus inclined
//! solid half-spaces, for slope tests of the native-physics port) and
//! [`KindWorld`] (any world, answering as one [`PrimitiveKind`]).
//!
//! # Shapes: the pawn's box, and the older cylinder
//!
//! The original moves the player's pawn as an **axis-aligned box** whose half
//! sizes are the collision cylinder's radius and half height, `(21, 21, 44)`
//! (`docs/PARITY_FINDINGS.md` P4; NATIVE_PHYSICS.md 10.1): every move and
//! every extent trace of the pawn reaches the world's primitives with that
//! extent, and they treat it as a box. [`CollisionWorld::sweep_box`] is that
//! query, and the native-physics port ([`crate::ue3_movement`]) uses no other.
//!
//! [`CollisionWorld::sweep_capsule`] is the query from before that was known:
//! an **upright, flat-ended cylinder** (despite its name). It stays for the
//! placeholder movement model, the placeholder rope and the placement
//! helpers, which are ours and not the original's, and as the fallback of
//! [`CollisionWorld::sweep_box`] for a world that has not implemented the box
//! yet.
//!
//! # What a hit's time means: exact contact, and the original's pull-back
//!
//! Every query here reports the **exact first contact**. The original does
//! not: each of its collision routines moves the hit it reports back along
//! the trace by a distance that depends on the routine and on the trace's
//! length (P3; rule and constants read from the user's executable, see
//! [`pull_back`]). The pawn therefore never touches anything: it hovers a
//! true 3.15 or 4.95 uu above a floor and rests 0.1 uu from a wall. That
//! reduction is applied by **the caller** of [`CollisionWorld::sweep_box`]
//! and [`CollisionWorld::trace_line`], with [`TraceHit::reported_time`], from
//! the [`PrimitiveKind`] the world reports with the hit. A world never
//! applies it itself, or it would be applied twice.
//!
//! # Tunnelling
//!
//! Sweeps are **continuous and exact** for both shapes against axis-aligned
//! boxes. The box sweep is a separating-axis test over the three world axes.
//! The cylinder sweep is reduced to a ray cast against the Minkowski sum of
//! each box and the cylinder (a box with rounded vertical edges), decomposed
//! into two expanded slabs and four corner cylinders. There is no
//! substepping and no speed limit beyond `f32` precision, so the player
//! cannot tunnel through geometry at any speed.
//!
//! # Contacts
//!
//! A hit reports the fraction of the sweep travelled, the shape centre at
//! contact and the surface normal. If the sweep **starts** overlapping a box,
//! it reports a hit at `time = 0` with the normal of least penetration, but
//! only when moving *into* that normal, so an overlapping shape can always
//! move out. A box that starts exactly touching a surface is blocked at
//! `time = 0` when it moves into it and is free to slide along it or leave
//! (the convention of `asamu-world`'s box sweep; the original's own rules for
//! a start in contact are in NATIVE_PHYSICS.md 10.1 and are not reproduced:
//! with the pull-back a pawn is normally 0.1 uu or more from everything).
//!
//! Iteration order over boxes is the `Vec` order and ties keep the earlier
//! candidate, so results are deterministic.
//!
//! # Surfaces (what the gameplay script sees of the hit actor)
//!
//! Every hit carries the [`Surface`] of the primitive it hit: the UE3 actor
//! class the primitive stands for ([`ActorClass`]), the actor's `Tag`
//! ([`ActorTag`]) and an optional actor id. The grapple gun and the landing
//! handler read these (GRAPPLE.md §5/§12, ABILITIES.md §5). The tag
//! `NotGrappleAble` is the `grapple_able == false` flag that every primitive
//! already carries (an actor has a single tag in UE3, so a primitive that is
//! not grapple-able cannot also carry one of the other tags in the original;
//! this representation is a superset). [`CollisionWorld::actor_location`]
//! reports where a tracked actor currently is, so the grapple anchor can ride
//! moving targets (GRAPPLE.md G-AT-7/8).

use glam::{Vec2, Vec3};
use serde::{Deserialize, Serialize};

/// Separation (UU) that **our own** helpers keep between the cylinder and a
/// surface after a contact: the placeholder movement model, the placeholder
/// rope and the routines that place a pawn on a floor.
///
/// A numerical tolerance of those helpers, **not** a gameplay constant and
/// not the original's: the native-physics port does not use it. What the
/// original keeps between the pawn and the world is the pull-back of its
/// line checks ([`pull_back`]), which replaced this value there.
pub const CONTACT_SKIN: f32 = 0.05;

/// Displacements shorter than this (UU) are treated as no movement.
pub const MIN_MOVE: f32 = 1.0e-4;

/// The player's collision shape (an upright cylinder; see module docs).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct CollisionShape {
    /// Radius, UU.
    pub radius: f32,
    /// Half-height, UU.
    pub half_height: f32,
}

/// Which of the original's collision routines answers for a primitive. The
/// routine decides how far the hit it reports is moved back along the trace
/// ([`pull_back`]); nothing else depends on the kind. The same five kinds as
/// `asamu_world::collision::PrimitiveKind`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PrimitiveKind {
    /// Level BSP (`UModel::LineCheck`). Also what a world that knows no
    /// kinds reports: the test worlds here, and every surface of a world
    /// that has not implemented [`CollisionWorld::sweep_box`].
    #[default]
    Bsp,
    /// The brush of a volume, e.g. a blocking volume
    /// (`UBrushComponent::LineCheck`).
    BrushHull,
    /// The simple-collision body of a static mesh (the simple path of
    /// `UStaticMeshComponent::LineCheck`).
    SimpleHull,
    /// The triangles of a static mesh (the per-triangle path of the same
    /// routine).
    TriangleMesh,
    /// Another pawn's collision cylinder (`UCylinderComponent::LineCheck`).
    PawnCylinder,
}

/// Whether a trace carries an extent. The pawn's moves and its floor check
/// carry the pawn's box; the grapple's fire trace and the crosshair trace
/// carry none.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TraceExtent {
    /// A zero-extent (line) trace.
    Zero,
    /// A trace with a non-zero extent (the pawn's box).
    Box,
}

/// The part of a trace's length by which the original moves a reported hit
/// back, and the distance (UU) for a trace shorter than 1 uu.
/// NativeCode: the `f32` at 0x1016457B8 of the user's macOS executable, read
/// by `UModel::LineCheck @ 0x10092BD20` (at 0x10092BEEF),
/// `UBrushComponent::LineCheck @ 0x100759CB0` (at 0x100759D9A) and
/// `UStaticMeshComponent::LineCheck @ 0x100C58790` (at 0x100C58956 on the
/// simple path, at 0x100C59677 on the per-triangle path).
pub const PULL_BACK_FRACTION: f32 = 0.1;
/// The most (UU) a hit of BSP, of a brush or of a mesh's simple-collision
/// body is moved back. NativeCode: the `f32` at 0x101633D88, divided by the
/// trace's length at 0x10092BF09 (BSP, box path), 0x100759DB4 (brush) and
/// 0x100C58970 (static mesh, simple path).
pub const PULL_BACK_MAX: f32 = 1.0;
/// The most (UU) a hit of a static mesh's triangles is moved back.
/// NativeCode: the `f32` at 0x1016457CC, divided by the trace's length at
/// 0x100C5968E (`UStaticMeshComponent::LineCheck`, per-triangle path, which
/// both the zero-extent and the box check of the triangles end in).
pub const PULL_BACK_MAX_TRIANGLES: f32 = 4.0;
/// The distance (UU) a zero-extent hit of BSP is moved back, whatever the
/// trace's length. NativeCode: the `f32` −0.5 at 0x10163926C, divided by the
/// trace's length at 0x10092C0FD (`UModel::LineCheck`, zero-extent path).
pub const PULL_BACK_BSP_LINE: f32 = 0.5;
/// The part of a trace's length a hit of a pawn's cylinder is moved back.
/// NativeCode: the `f32` −0.001 at 0x1016BC044, added to the hit time at
/// 0x100537D69 (`UCylinderComponent::LineCheck @ 0x100537920`).
pub const PULL_BACK_PAWN_CYLINDER: f32 = 0.001;
/// How far (UU) beyond a BSP hit the original still looks for other
/// primitives ([`bsp_checked_fraction`]). NativeCode: the `f32` at
/// 0x101639278, read at 0x1008EBC91 (`UWorld::MultiLineCheck @ 0x1008EB640`).
pub const BSP_HIT_MARGIN: f32 = 5.0;
/// Added to the divisor of [`bsp_checked_fraction`]. NativeCode: the `f32`
/// at 0x10163FED8, read at 0x1008EBCA5.
pub const BSP_HIT_MARGIN_EPSILON: f32 = 1.0e-4;

/// The hit time the original reports for an exact first contact at `time`
/// (fraction of the trace, `[0, 1]`) on a trace `length` uu long.
///
/// Every collision routine of the original ends by moving its hit back
/// along the trace and clamping the time to `[0, 1]`
/// (`docs/PARITY_FINDINGS.md` P3; NATIVE_PHYSICS.md 10.2). Read in the
/// user's macOS executable (CONFIRMED; addresses at the constants above):
///
/// | kind | [`TraceExtent::Box`] | [`TraceExtent::Zero`] |
/// |---|---|---|
/// | [`PrimitiveKind::Bsp`] | rule K, K = 1.0 | 0.5 uu, whatever the length |
/// | [`PrimitiveKind::BrushHull`] | rule K, K = 1.0 | the same: one computation for both |
/// | [`PrimitiveKind::SimpleHull`] | rule K, K = 1.0 | the same: one computation for both |
/// | [`PrimitiveKind::TriangleMesh`] | rule K, K = 4.0 | the same: one computation for both |
/// | [`PrimitiveKind::PawnCylinder`] | 0.001 of the length | the same |
///
/// **Rule K**: the hit is moved back by 0.1 uu when the trace is shorter
/// than 1 uu, else by a tenth of the trace's length, but by no more than K
/// uu. In the original's arithmetic, which this function repeats operation
/// for operation in `f32`: the time is reduced by `0.1 / length` when that
/// exceeds 0.1, else by the smaller of 0.1 and `K / length`.
///
/// Measured on the first recordings (P3, V11, V16, V18): a flat BSP floor
/// (the pawn's centre at 72 + 44 + 2.15 + 1.0 on 1,817 records), simple and
/// per-triangle floors, a wall (rest gap 0.0999 uu) and 47 grapple anchors
/// (3.9967 uu short of the triangles). Not measured: the zero-extent rows
/// of BSP, brushes and simple bodies, and the pawn cylinder.
///
/// The original skips the reduction for a trace carrying the native flag
/// 0x80000; no trace of the player's movement or of the grapple carries it.
///
/// Robustness, not original behaviour: a NaN result (a NaN `time` or
/// `length`) is a hit at time 0 here, where the original's clamp would
/// report no hit.
#[must_use]
pub fn pull_back(kind: PrimitiveKind, extent: TraceExtent, length: f32, time: f32) -> f32 {
    let reduced = match (kind, extent) {
        (PrimitiveKind::PawnCylinder, _) => time + -PULL_BACK_PAWN_CYLINDER,
        (PrimitiveKind::Bsp, TraceExtent::Zero) => -PULL_BACK_BSP_LINE / length + time,
        (PrimitiveKind::TriangleMesh, _) => time - rule_k(length, PULL_BACK_MAX_TRIANGLES),
        (PrimitiveKind::Bsp | PrimitiveKind::BrushHull | PrimitiveKind::SimpleHull, _) => {
            time - rule_k(length, PULL_BACK_MAX)
        }
    };
    // The original: the smaller of the result and 1, replaced by 0 when the
    // result is negative.
    if reduced < 0.0 || reduced.is_nan() {
        0.0
    } else if reduced < 1.0 {
        reduced
    } else {
        1.0
    }
}

/// Rule K of [`pull_back`] as a reduction of the hit time.
fn rule_k(length: f32, cap: f32) -> f32 {
    let short = PULL_BACK_FRACTION / length;
    let capped = cap / length;
    if PULL_BACK_FRACTION < short {
        short
    } else if PULL_BACK_FRACTION < capped {
        PULL_BACK_FRACTION
    } else {
        capped
    }
}

/// The part of a trace the original still checks against everything that is
/// not BSP, once BSP has reported a hit at `bsp_time` (the time **after**
/// its own pull-back) on a trace `length` uu long. In `(0, 1]`, or 0 when
/// the BSP hit is at the very start (nothing else is checked then).
///
/// `UWorld::MultiLineCheck @ 0x1008EB640` asks the level's BSP first. On a
/// hit it shortens the trace to end [`BSP_HIT_MARGIN`] uu beyond the reported
/// BSP hit (never lengthening it) before it asks the other primitives, and
/// afterwards multiplies their hit times by the same factor (0x1008EBC3D to
/// 0x1008EBDA9; CONFIRMED, read in the user's macOS executable). Those
/// primitives therefore apply their own pull-back to the **shortened**
/// length: a static mesh's triangles 6 uu above a BSP floor, probed by the
/// 28 uu floor check from 4 uu above them, are reported
/// `0.1 × (4 + 6 − 1 + 5) = 1.4` uu short of the contact, not 2.8. A world
/// that mixes BSP with other kinds reports this factor in
/// [`TraceHit::checked_fraction`]. Not measured on a recording yet.
#[must_use]
pub fn bsp_checked_fraction(bsp_time: f32, length: f32) -> f32 {
    let distance = bsp_time * length;
    let fraction = (BSP_HIT_MARGIN + distance) * bsp_time / (distance + BSP_HIT_MARGIN_EPSILON);
    if fraction < 0.0 || fraction.is_nan() {
        0.0
    } else if fraction < 1.0 {
        fraction
    } else {
        1.0
    }
}

/// [`pull_back`] for a primitive whose own check saw only the first
/// `checked_fraction` of the trace ([`bsp_checked_fraction`]): the exact
/// contact at `time` of the whole trace is at `time / checked_fraction` of
/// the shortened one, is pulled back with the shortened length, and is
/// scaled back. With `checked_fraction` 1 (and for any value outside
/// `(0, 1)`) this is [`pull_back`] itself, bit for bit.
#[must_use]
pub fn reported_time(
    kind: PrimitiveKind,
    extent: TraceExtent,
    length: f32,
    time: f32,
    checked_fraction: f32,
) -> f32 {
    if checked_fraction > 0.0 && checked_fraction < 1.0 {
        pull_back(
            kind,
            extent,
            length * checked_fraction,
            time / checked_fraction,
        ) * checked_fraction
    } else {
        pull_back(kind, extent, length, time)
    }
}

/// UE3 actor `Tag` values that the gameplay script tests (GRAPPLE.md §2 and
/// §5, ABILITIES.md §5). A UE3 actor has exactly one tag; the tag
/// `NotGrappleAble` is represented by `grapple_able == false` (module docs).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActorTag {
    /// No tag the script tests.
    #[default]
    None,
    /// `TopOnlyGrappleAble` (`GrappleGun.topOnlyGrappleTag`): the grapple only
    /// attaches to faces whose normal Z is at least `TOP_GRAPPLE_ANGLE`
    /// (G-AC-4).
    TopOnlyGrappleAble,
    /// `BottomOnlyGrappleAble` (`GrappleGun.bottomOnlyGrappleTag`): only faces
    /// whose normal Z is at most `-BOTTOM_GRAPPLE_ANGLE` (G-AC-4).
    BottomOnlyGrappleAble,
    /// `grappleInteractable` (`GrappleGun.grappleInteractableTag`): the gun
    /// reports the actor as the Kismet originator of the grapple (G-AT-3, §14).
    GrappleInteractable,
    /// `NotLandable`: a landing on this actor skips the pawn's whole script
    /// landing handler (ABILITIES.md §5, G-CT-4).
    NotLandable,
}

/// The UE3 actor class a collision primitive stands for, as far as the
/// gameplay script distinguishes classes (GRAPPLE.md §1, §5, §6, §12).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "class", rename_all = "snake_case")]
pub enum ActorClass {
    /// Level geometry without a static-mesh component (BSP brushes; the hit
    /// actor is the level's `WorldInfo`).
    #[default]
    WorldGeometry,
    /// A `StaticMeshActor` (static, has a static-mesh component).
    StaticMesh,
    /// An `InterpActor` (mover): the grapple anchor follows it (G-AT-7).
    InterpActor,
    /// `ASAMURechargeCrystal` (`InterpActor`; grapple-able, release-instant,
    /// no decal). `charged` is the crystal's state when the surface was
    /// built (state `Charged`, G-WO-1).
    RechargeCrystal {
        /// The crystal is in its `Charged` state.
        charged: bool,
    },
    /// `ASAMUGlowFlower` (`InterpActor`; grapple-able, release-instant, no
    /// decal; G-WO-2).
    GlowFlower,
    /// `ASAMUFallingRock` (`InterpActor`; grapple-able; G-WO-3).
    FallingRock,
    /// `ASAMUFallingWhenGrappledRock` (`InterpActor`; grapple-able; G-WO-4).
    FallingWhenGrappledRock,
    /// `ASAMUFloatingRock` (`DynamicSMActor`; ordinary target, the anchor
    /// does **not** follow it; G-WO-5).
    FloatingRock,
    /// `ASAMUInteractable_Actor` (`DynamicSMActor`; story-mode interaction
    /// target, G-AC-0).
    Interactable,
}

impl ActorClass {
    /// The hit component is a static-mesh component (the crosshair rule of
    /// G-TG-2 ignores other components).
    #[must_use]
    pub fn has_static_mesh_component(self) -> bool {
        !matches!(self, Self::WorldGeometry)
    }

    /// The class implements `ASAMUGrappleAbleInterface` (its `Grappled` /
    /// `UnGrappled` handlers run, G-AT-3, G-RL-1).
    #[must_use]
    pub fn grapple_able_interface(self) -> bool {
        matches!(
            self,
            Self::RechargeCrystal { .. }
                | Self::GlowFlower
                | Self::FallingRock
                | Self::FallingWhenGrappledRock
        )
    }

    /// The class implements `ASAMUReleaseGrappleInstantInterface` (G-RL-3).
    #[must_use]
    pub fn release_instant(self) -> bool {
        matches!(self, Self::RechargeCrystal { .. } | Self::GlowFlower)
    }

    /// The class implements `ASAMUDoesNotAcceptGrappleDecal` (cosmetic).
    #[must_use]
    pub fn rejects_decal(self) -> bool {
        matches!(self, Self::RechargeCrystal { .. } | Self::GlowFlower)
    }

    /// The class derives from `InterpActor`, so the grapple anchor follows
    /// the actor (G-AT-7).
    #[must_use]
    pub fn is_interp_actor(self) -> bool {
        matches!(
            self,
            Self::InterpActor
                | Self::RechargeCrystal { .. }
                | Self::GlowFlower
                | Self::FallingRock
                | Self::FallingWhenGrappledRock
        )
    }
}

/// What the gameplay script sees of the actor a primitive belongs to (see
/// the module docs).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Surface {
    /// Id of the actor, when the world tracks it (movers, crystals,
    /// interactables); `None` for static geometry.
    #[serde(default)]
    pub actor: Option<u32>,
    /// The actor's class.
    #[serde(default)]
    pub class: ActorClass,
    /// The actor's tag.
    #[serde(default)]
    pub tag: ActorTag,
}

/// Where a tracked actor currently is ([`CollisionWorld::actor_location`]).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct ActorLocation {
    /// Actor id (as in [`Surface::actor`]).
    pub id: u32,
    /// Current location, UU.
    pub location: Vec3,
}

/// A sweep or ray contact.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Hit {
    /// Fraction of the sweep/ray travelled before contact, in `[0, 1]`.
    pub time: f32,
    /// Distance travelled before contact, UU.
    pub distance: f32,
    /// Sweeps: the shape centre at contact. Rays: the hit point.
    pub position: Vec3,
    /// Unit surface normal pointing towards the mover.
    pub normal: Vec3,
    /// Whether the surface accepts the grapple (`false` = the actor tag
    /// `NotGrappleAble`).
    pub grapple_able: bool,
    /// The sweep/ray started inside the surface.
    pub start_penetrating: bool,
    /// Class, tag and id of the hit actor.
    #[serde(default)]
    pub surface: Surface,
}

/// A contact together with what the caller needs to turn its exact time into
/// the time the original reports ([`TraceHit::reported_time`]).
///
/// Build it with [`TraceHit::new`]; the struct may gain fields.
#[derive(Clone, Copy, Debug, PartialEq)]
#[non_exhaustive]
pub struct TraceHit {
    /// The contact. Its `time`, `distance` and `position` are those of the
    /// **exact** first contact: no pull-back is applied.
    pub hit: Hit,
    /// Which of the original's collision routines the primitive stands for.
    pub kind: PrimitiveKind,
    /// The part of the trace's length the primitive's own check saw, in
    /// `(0, 1]`: 1, except for a primitive other than BSP on a trace on
    /// which BSP reported a hit too ([`bsp_checked_fraction`]).
    pub checked_fraction: f32,
}

impl TraceHit {
    /// A hit of a primitive of `kind` whose check saw the whole trace.
    #[must_use]
    pub fn new(hit: Hit, kind: PrimitiveKind) -> Self {
        Self {
            hit,
            kind,
            checked_fraction: 1.0,
        }
    }

    /// The same hit of a primitive whose check saw only the first
    /// `checked_fraction` of the trace.
    #[must_use]
    pub fn with_checked_fraction(mut self, checked_fraction: f32) -> Self {
        self.checked_fraction = checked_fraction;
        self
    }

    /// The hit time the original reports for this contact on a trace
    /// `length` uu long ([`reported_time`]).
    #[must_use]
    pub fn reported_time(&self, extent: TraceExtent, length: f32) -> f32 {
        reported_time(
            self.kind,
            extent,
            length,
            self.hit.time,
            self.checked_fraction,
        )
    }
}

/// Geometry queries the simulation needs. Implementations must be
/// deterministic (same inputs → same outputs, independent of call history).
///
/// [`Self::sweep_box`] and [`Self::trace_line`] are the queries of the
/// original's movement and traces; they have default implementations in
/// terms of the two older queries, so a world written before them keeps
/// working (as cylinders, and as [`PrimitiveKind::Bsp`]) until it implements
/// them.
pub trait CollisionWorld {
    /// Sweeps the centre of an upright cylinder from `start` to `end` and
    /// returns the first blocking contact, if any (see the module docs: not
    /// the original's shape).
    fn sweep_capsule(&self, start: Vec3, end: Vec3, shape: CollisionShape) -> Option<Hit>;

    /// Casts a ray from `origin` along `direction` (normalized internally) up
    /// to `max_distance` and returns the first contact, if any.
    fn raycast(&self, origin: Vec3, direction: Vec3, max_distance: f32) -> Option<Hit>;

    /// Current location of the tracked actor `id` (movers), if the world
    /// knows it. The default knows no actors.
    fn actor_location(&self, id: u32) -> Option<Vec3> {
        let _ = id;
        None
    }

    /// Sweeps the centre of the axis-aligned box with the half sizes
    /// `half_extents` from `start` to `end`: the original's pawn move and
    /// extent trace (`(21, 21, 44)` for the player's traces; a move widens X
    /// and Y by 1.001).
    ///
    /// Returns the contact the original's world check would report first,
    /// with its **exact** time (the caller applies the pull-back:
    /// [`TraceHit::reported_time`] with [`TraceExtent::Box`]) and the kind of
    /// the primitive. In a world of one kind that is simply the first
    /// contact. Where kinds are mixed it is the contact whose *reported*
    /// time is the earliest, which need not be the first one touched: on a
    /// 28 uu floor check a mesh's triangles up to 1.8 uu below the top of a
    /// blocking volume are reported before it (2.8 uu back against 1.0).
    ///
    /// The default sweeps the cylinder of radius `half_extents.x` and half
    /// height `half_extents.z` with [`Self::sweep_capsule`] and reports
    /// [`PrimitiveKind::Bsp`].
    fn sweep_box(&self, start: Vec3, end: Vec3, half_extents: Vec3) -> Option<TraceHit> {
        let shape = CollisionShape {
            radius: half_extents.x,
            half_height: half_extents.z,
        };
        self.sweep_capsule(start, end, shape)
            .map(|hit| TraceHit::new(hit, PrimitiveKind::default()))
    }

    /// [`Self::raycast`] with the kind of the primitive that was hit: the
    /// original's zero-extent trace (the grapple's fire trace, the
    /// crosshair). The time is the exact contact's; the caller applies
    /// [`TraceHit::reported_time`] with [`TraceExtent::Zero`] and
    /// `max_distance`. Which contact is to be returned where kinds are
    /// mixed: as for [`Self::sweep_box`].
    ///
    /// The default is [`Self::raycast`] reporting [`PrimitiveKind::Bsp`].
    fn trace_line(&self, origin: Vec3, direction: Vec3, max_distance: f32) -> Option<TraceHit> {
        self.raycast(origin, direction, max_distance)
            .map(|hit| TraceHit::new(hit, PrimitiveKind::default()))
    }
}

/// An axis-aligned bounding box.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Aabb {
    /// Minimum corner.
    pub min: Vec3,
    /// Maximum corner.
    pub max: Vec3,
}

impl Aabb {
    /// Builds a box from two opposite corners in any order.
    #[must_use]
    pub fn from_corners(a: Vec3, b: Vec3) -> Self {
        Self {
            min: a.min(b),
            max: a.max(b),
        }
    }

    /// Builds a box from its centre and half-extents (absolute values used).
    #[must_use]
    pub fn from_center_half_extents(center: Vec3, half: Vec3) -> Self {
        let half = half.abs();
        Self {
            min: center - half,
            max: center + half,
        }
    }

    /// Centre point.
    #[must_use]
    pub fn center(&self) -> Vec3 {
        (self.min + self.max) * 0.5
    }

    /// Half-extents.
    #[must_use]
    pub fn half_extents(&self) -> Vec3 {
        (self.max - self.min) * 0.5
    }

    /// `true` if `p` is inside or on the boundary.
    #[must_use]
    pub fn contains(&self, p: Vec3) -> bool {
        p.cmpge(self.min).all() && p.cmple(self.max).all()
    }
}

/// A solid axis-aligned box.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct SolidBox {
    /// Extent of the box.
    pub bounds: Aabb,
    /// Whether the grapple can attach to this box.
    pub grapple_able: bool,
    /// The actor the box belongs to.
    #[serde(default)]
    pub surface: Surface,
}

/// An infinite horizontal ground plane (solid below `z`).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct GroundPlane {
    /// Height of the surface, UU.
    pub z: f32,
    /// Whether the grapple can attach to the ground.
    pub grapple_able: bool,
}

/// A world made of an optional ground plane and axis-aligned boxes.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct BoxWorld {
    /// Optional infinite ground.
    pub ground: Option<GroundPlane>,
    /// Solid boxes.
    pub boxes: Vec<SolidBox>,
    /// Current locations of tracked actors (movers), for
    /// [`CollisionWorld::actor_location`].
    #[serde(default)]
    pub actors: Vec<ActorLocation>,
}

/// A candidate contact from one primitive.
#[derive(Clone, Copy, Debug)]
struct Candidate {
    t: f32,
    normal: Vec3,
    /// Penetration depth when the query starts inside (0 otherwise).
    depth: f32,
    inside: bool,
}

fn axis_unit(axis: usize, positive: bool) -> Vec3 {
    let mut v = Vec3::ZERO;
    v[axis] = if positive { 1.0 } else { -1.0 };
    v
}

/// Ray (origin + t·d, t ∈ [0, 1]) against the box `[lo, hi]`.
fn ray_vs_box(origin: Vec3, d: Vec3, lo: Vec3, hi: Vec3) -> Option<Candidate> {
    let mut t_enter = f32::NEG_INFINITY;
    let mut t_exit = f32::INFINITY;
    let mut n_enter = Vec3::ZERO;
    for axis in 0..3 {
        let o = origin[axis];
        let dd = d[axis];
        let (l, h) = (lo[axis], hi[axis]);
        if dd == 0.0 {
            if o < l || o > h {
                return None;
            }
            continue;
        }
        let inv = 1.0 / dd;
        let mut t0 = (l - o) * inv;
        let mut t1 = (h - o) * inv;
        // Entering through the low face means the outward normal is -axis.
        let mut n = axis_unit(axis, false);
        if t0 > t1 {
            core::mem::swap(&mut t0, &mut t1);
            n = axis_unit(axis, true);
        }
        if t0 > t_enter {
            t_enter = t0;
            n_enter = n;
        }
        if t1 < t_exit {
            t_exit = t1;
        }
        if t_enter > t_exit {
            return None;
        }
    }
    if t_exit < 0.0 || t_enter > 1.0 {
        return None;
    }
    if t_enter >= 0.0 {
        return Some(Candidate {
            t: t_enter,
            normal: n_enter,
            depth: 0.0,
            inside: false,
        });
    }
    // Starts inside (or exactly on a face with zero motion along it): report
    // the face of least penetration.
    let mut best_depth = f32::INFINITY;
    let mut best_normal = Vec3::Z;
    for axis in 0..3 {
        let o = origin[axis];
        let to_low = o - lo[axis];
        let to_high = hi[axis] - o;
        if to_high < best_depth {
            best_depth = to_high;
            best_normal = axis_unit(axis, true);
        }
        if to_low < best_depth {
            best_depth = to_low;
            best_normal = axis_unit(axis, false);
        }
    }
    Some(Candidate {
        t: 0.0,
        normal: best_normal,
        depth: best_depth.max(0.0),
        inside: true,
    })
}

/// Ray (origin + t·d, t ∈ [0, 1]) against a finite vertical cylinder.
fn ray_vs_vertical_cylinder(
    origin: Vec3,
    d: Vec3,
    center: Vec2,
    radius: f32,
    z_lo: f32,
    z_hi: f32,
) -> Option<Candidate> {
    let rel = origin.truncate() - center;
    let r2 = radius * radius;
    let dist2 = rel.length_squared();
    let in_disc = dist2 <= r2;
    let in_z = origin.z >= z_lo && origin.z <= z_hi;
    if in_disc && in_z {
        let dist = dist2.sqrt();
        let radial_depth = radius - dist;
        let low_depth = origin.z - z_lo;
        let high_depth = z_hi - origin.z;
        let radial_normal = if dist > 0.0 {
            (rel / dist).extend(0.0)
        } else {
            // Exactly on the axis: push against the horizontal motion.
            let back = -d.truncate();
            if back.length_squared() > 0.0 {
                back.normalize().extend(0.0)
            } else {
                Vec3::X
            }
        };
        let mut best = (radial_depth, radial_normal);
        if high_depth < best.0 {
            best = (high_depth, Vec3::Z);
        }
        if low_depth < best.0 {
            best = (low_depth, Vec3::NEG_Z);
        }
        return Some(Candidate {
            t: 0.0,
            normal: best.1,
            depth: best.0.max(0.0),
            inside: true,
        });
    }

    let mut best: Option<Candidate> = None;
    let mut consider = |c: Candidate| {
        if best.is_none_or(|b| c.t < b.t) {
            best = Some(c);
        }
    };

    // Side wall.
    let dxy = d.truncate();
    let a = dxy.length_squared();
    if a > 0.0 && !in_disc {
        let b = rel.dot(dxy);
        let c = dist2 - r2;
        if b < 0.0 {
            let disc = b * b - a * c;
            if disc >= 0.0 {
                let t = (-b - disc.sqrt()) / a;
                if (0.0..=1.0).contains(&t) {
                    let z = origin.z + d.z * t;
                    if z >= z_lo && z <= z_hi {
                        let p = rel + dxy * t;
                        let n = p.normalize_or_zero();
                        if n != Vec2::ZERO {
                            consider(Candidate {
                                t,
                                normal: n.extend(0.0),
                                depth: 0.0,
                                inside: false,
                            });
                        }
                    }
                }
            }
        }
    }

    // Caps.
    if d.z < 0.0 && origin.z >= z_hi {
        let t = (z_hi - origin.z) / d.z;
        if t <= 1.0 && (rel + dxy * t).length_squared() <= r2 {
            consider(Candidate {
                t: t.max(0.0),
                normal: Vec3::Z,
                depth: 0.0,
                inside: false,
            });
        }
    }
    if d.z > 0.0 && origin.z <= z_lo {
        let t = (z_lo - origin.z) / d.z;
        if t <= 1.0 && (rel + dxy * t).length_squared() <= r2 {
            consider(Candidate {
                t: t.max(0.0),
                normal: Vec3::NEG_Z,
                depth: 0.0,
                inside: false,
            });
        }
    }
    best
}

/// Swept upright cylinder (centre path origin + t·d) against one box: a ray
/// against the Minkowski sum (box ⊕ cylinder).
fn sweep_vs_box(origin: Vec3, d: Vec3, b: &Aabb, shape: CollisionShape) -> Option<Candidate> {
    let r = shape.radius;
    let h = shape.half_height;
    let z_lo = b.min.z - h;
    let z_hi = b.max.z + h;
    let parts = [
        ray_vs_box(
            origin,
            d,
            Vec3::new(b.min.x - r, b.min.y, z_lo),
            Vec3::new(b.max.x + r, b.max.y, z_hi),
        ),
        ray_vs_box(
            origin,
            d,
            Vec3::new(b.min.x, b.min.y - r, z_lo),
            Vec3::new(b.max.x, b.max.y + r, z_hi),
        ),
        ray_vs_vertical_cylinder(origin, d, Vec2::new(b.min.x, b.min.y), r, z_lo, z_hi),
        ray_vs_vertical_cylinder(origin, d, Vec2::new(b.max.x, b.min.y), r, z_lo, z_hi),
        ray_vs_vertical_cylinder(origin, d, Vec2::new(b.min.x, b.max.y), r, z_lo, z_hi),
        ray_vs_vertical_cylinder(origin, d, Vec2::new(b.max.x, b.max.y), r, z_lo, z_hi),
    ];
    let mut inside: Option<Candidate> = None;
    let mut entry: Option<Candidate> = None;
    for c in parts.into_iter().flatten() {
        if c.inside {
            // Least penetration over all parts that contain the origin.
            if inside.is_none_or(|i| c.depth < i.depth) {
                inside = Some(c);
            }
        } else if entry.is_none_or(|e| c.t < e.t) {
            entry = Some(c);
        }
    }
    inside.or(entry)
}

/// Keeps a penetrating candidate only when the motion goes into its normal.
fn blocking(c: Candidate, d: Vec3) -> Option<Candidate> {
    if c.inside && d.dot(c.normal) >= 0.0 {
        None
    } else {
        Some(c)
    }
}

/// The order in which the original tests the pawn box's own face axes: Z,
/// X, Y, an equal entry keeping the earlier axis (NATIVE_PHYSICS.md 10.1).
const BOX_FACE_ORDER: [usize; 3] = [2, 0, 1];

/// Swept axis-aligned box (centre path `origin + t·d`, `t ∈ [0, 1]`, half
/// sizes `e`) against one box: a separating-axis test over the three world
/// axes, with the start rules of the module docs.
fn box_vs_box(origin: Vec3, d: Vec3, b: &Aabb, e: Vec3) -> Option<Candidate> {
    let mut entry = f32::NEG_INFINITY;
    let mut normal = Vec3::ZERO;
    let mut separated = false;
    let mut exit = f32::INFINITY;
    // Least overlap over the axes that overlap at the start.
    let mut depth = f32::INFINITY;
    let mut depth_normal = Vec3::ZERO;
    for axis in BOX_FACE_ORDER {
        // The obstacle's range relative to the box centre at the start.
        let lo = b.min[axis] - origin[axis];
        let hi = b.max[axis] - origin[axis];
        let r = e[axis];
        let v = d[axis];
        // Signed gaps: the box below the obstacle's range, and above it.
        let below = lo - r;
        let above = -(hi + r);
        let (gap, positive, closing) = if below >= above {
            (below, false, v)
        } else {
            (above, true, -v)
        };
        // Written so that a NaN from hostile input ends the sweep.
        if !(gap.is_finite() && closing.is_finite()) {
            return None;
        }
        let axis_exit = if gap >= 0.0 {
            // Separated or touching at the start: the motion must close
            // this axis for the boxes ever to overlap.
            if closing <= 0.0 {
                return None;
            }
            let axis_entry = gap / closing;
            if axis_entry > entry {
                entry = axis_entry;
                normal = axis_unit(axis, positive);
            }
            separated = true;
            (gap + (hi - lo) + 2.0 * r) / closing
        } else {
            if -gap < depth {
                depth = -gap;
                depth_normal = axis_unit(axis, positive);
            }
            if v > 0.0 {
                (hi + r) / v
            } else if v < 0.0 {
                (lo - r) / v
            } else {
                f32::INFINITY
            }
        };
        if axis_exit < exit {
            exit = axis_exit;
        }
        if entry > exit || entry > 1.0 {
            return None;
        }
    }
    if separated {
        Some(Candidate {
            t: entry.max(0.0),
            normal,
            depth: 0.0,
            inside: false,
        })
    } else {
        // Overlapping on every axis at the start.
        depth.is_finite().then_some(Candidate {
            t: 0.0,
            normal: depth_normal,
            depth,
            inside: true,
        })
    }
}

/// A swept shape (either kind) against the ground plane, whose surface for
/// the shape's centre is at `surface`.
fn sweep_vs_ground(start_z: f32, dz: f32, surface: f32) -> Option<Candidate> {
    if start_z < surface {
        (dz < 0.0).then_some(Candidate {
            t: 0.0,
            normal: Vec3::Z,
            depth: surface - start_z,
            inside: true,
        })
    } else if dz < 0.0 {
        let t = (surface - start_z) / dz;
        (t <= 1.0).then_some(Candidate {
            t: t.max(0.0),
            normal: Vec3::Z,
            depth: 0.0,
            inside: false,
        })
    } else {
        None
    }
}

/// `true` when a box sweep with these inputs can be answered.
fn valid_box_query(start: Vec3, d: Vec3, half_extents: Vec3) -> bool {
    start.is_finite()
        && d.is_finite()
        && d != Vec3::ZERO
        && half_extents.is_finite()
        && half_extents.cmpgt(Vec3::ZERO).all()
}

impl BoxWorld {
    /// An empty world (nothing to collide with).
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds a ground plane at `z` (builder style).
    #[must_use]
    pub fn with_ground(mut self, z: f32, grapple_able: bool) -> Self {
        self.ground = Some(GroundPlane { z, grapple_able });
        self
    }

    /// Adds a box from two corners (builder style).
    #[must_use]
    pub fn with_box(mut self, a: Vec3, b: Vec3, grapple_able: bool) -> Self {
        self.boxes.push(SolidBox {
            bounds: Aabb::from_corners(a, b),
            grapple_able,
            surface: Surface::default(),
        });
        self
    }

    /// Adds a box belonging to the actor described by `surface` (builder
    /// style).
    #[must_use]
    pub fn with_surface_box(
        mut self,
        a: Vec3,
        b: Vec3,
        grapple_able: bool,
        surface: Surface,
    ) -> Self {
        self.boxes.push(SolidBox {
            bounds: Aabb::from_corners(a, b),
            grapple_able,
            surface,
        });
        self
    }

    /// Records (or updates) the location of the tracked actor `id`.
    pub fn set_actor_location(&mut self, id: u32, location: Vec3) {
        match self.actors.iter_mut().find(|a| a.id == id) {
            Some(a) => a.location = location,
            None => self.actors.push(ActorLocation { id, location }),
        }
    }

    /// `true` if the shape centred at `position` overlaps any solid
    /// (touching counts as not overlapping).
    #[must_use]
    pub fn overlaps(&self, position: Vec3, shape: CollisionShape) -> bool {
        if let Some(g) = self.ground
            && position.z < g.z + shape.half_height
        {
            return true;
        }
        self.boxes.iter().any(|b| {
            sweep_vs_box(position, Vec3::ZERO, &b.bounds, shape)
                .is_some_and(|c| c.inside && c.depth > 0.0)
        })
    }

    /// `true` if the axis-aligned box with the half sizes `half_extents`
    /// centred at `position` overlaps any solid (touching counts as not
    /// overlapping).
    #[must_use]
    pub fn overlaps_box(&self, position: Vec3, half_extents: Vec3) -> bool {
        if let Some(g) = self.ground
            && position.z < g.z + half_extents.z
        {
            return true;
        }
        self.boxes.iter().any(|b| {
            let lo = b.bounds.min - half_extents;
            let hi = b.bounds.max + half_extents;
            position.cmpgt(lo).all() && position.cmplt(hi).all()
        })
    }

    /// The earliest contact of the swept box with the ground and the boxes
    /// (ties keep the ground, then the earlier box).
    fn box_candidate(
        &self,
        start: Vec3,
        d: Vec3,
        half_extents: Vec3,
    ) -> Option<(Candidate, (bool, Surface))> {
        let mut best: Option<(Candidate, (bool, Surface))> = None;
        let mut consider = |c: Candidate, info: (bool, Surface)| {
            if best.is_none_or(|(b, _)| c.t < b.t) {
                best = Some((c, info));
            }
        };
        if let Some(g) = self.ground
            && let Some(c) = sweep_vs_ground(start.z, d.z, g.z + half_extents.z)
        {
            consider(c, (g.grapple_able, Surface::default()));
        }
        for b in &self.boxes {
            if let Some(c) =
                box_vs_box(start, d, &b.bounds, half_extents).and_then(|c| blocking(c, d))
            {
                consider(c, (b.grapple_able, b.surface));
            }
        }
        best
    }
}

/// A solid half-space: every point `p` with `normal · p < offset` is solid.
/// `normal` is the unit surface normal pointing out of the solid. Used for
/// sloped floors and walls in tests ([`SlopeWorld`]).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct HalfSpace {
    /// Unit outward normal.
    pub normal: Vec3,
    /// Plane offset: the surface is `normal · p = offset`.
    pub offset: f32,
    /// Whether the grapple can attach to it.
    pub grapple_able: bool,
}

impl HalfSpace {
    /// The half-space bounded by the plane through `point` with outward
    /// `normal` (normalized here; a zero/non-finite normal yields `None`).
    #[must_use]
    pub fn through_point(normal: Vec3, point: Vec3, grapple_able: bool) -> Option<Self> {
        let n = normal.normalize_or_zero();
        (n != Vec3::ZERO && point.is_finite()).then(|| Self {
            normal: n,
            offset: n.dot(point),
            grapple_able,
        })
    }

    /// A ramp rising along +X whose surface passes through `point` and whose
    /// normal has exactly the Z component `normal_z` (`(0, 1]`): normal
    /// `(-sqrt(1 - z²), 0, z)`. `None` for `normal_z` outside `(0, 1]`.
    #[must_use]
    pub fn ramp_x(normal_z: f32, point: Vec3, grapple_able: bool) -> Option<Self> {
        if !(normal_z > 0.0 && normal_z <= 1.0) || !point.is_finite() {
            return None;
        }
        let normal = Vec3::new(-(1.0 - normal_z * normal_z).sqrt(), 0.0, normal_z);
        Some(Self {
            normal,
            offset: normal.dot(point),
            grapple_able,
        })
    }

    /// Distance from the plane to the centre of an upright cylinder resting
    /// on it (support distance of the cylinder along `-normal`).
    fn support(&self, shape: CollisionShape) -> f32 {
        let n = self.normal;
        shape.radius * (n.x * n.x + n.y * n.y).sqrt() + shape.half_height * n.z.abs()
    }

    /// Distance from the plane to the centre of an axis-aligned box resting
    /// on it with its lowest corner (support distance of the box along
    /// `-normal`): `Σ half_extents[i] · |normal[i]|`.
    fn support_box(&self, half_extents: Vec3) -> f32 {
        half_extents.dot(self.normal.abs())
    }

    /// Ray `origin + t·d` (t ∈ [0, 1]) against the plane pushed out by
    /// `support`.
    fn cast(&self, origin: Vec3, d: Vec3, support: f32) -> Option<Candidate> {
        let n = self.normal;
        let f0 = n.dot(origin) - self.offset - support;
        if f0 < 0.0 {
            return Some(Candidate {
                t: 0.0,
                normal: n,
                depth: -f0,
                inside: true,
            });
        }
        let dn = n.dot(d);
        if dn >= 0.0 {
            return None;
        }
        let t = -f0 / dn;
        (t <= 1.0).then_some(Candidate {
            t: t.max(0.0),
            normal: n,
            depth: 0.0,
            inside: false,
        })
    }
}

/// A [`BoxWorld`] plus solid half-spaces (exact sweeps of the box and of the
/// upright cylinder against inclined planes). The solid is the union of all
/// primitives; ties keep the box-world hit, then the earlier half-space.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SlopeWorld {
    /// Boxes and optional ground plane.
    pub boxes: BoxWorld,
    /// Inclined solids.
    pub half_spaces: Vec<HalfSpace>,
}

impl SlopeWorld {
    /// Wraps a box world (no half-spaces yet).
    #[must_use]
    pub fn new(boxes: BoxWorld) -> Self {
        Self {
            boxes,
            half_spaces: Vec::new(),
        }
    }

    /// Adds a half-space (builder style).
    #[must_use]
    pub fn with_half_space(mut self, h: HalfSpace) -> Self {
        self.half_spaces.push(h);
        self
    }

    /// `true` if the shape centred at `position` overlaps any solid
    /// (touching counts as not overlapping).
    #[must_use]
    pub fn overlaps(&self, position: Vec3, shape: CollisionShape) -> bool {
        self.boxes.overlaps(position, shape)
            || self
                .half_spaces
                .iter()
                .any(|h| h.normal.dot(position) - h.offset - h.support(shape) < 0.0)
    }
}

impl SlopeWorld {
    /// `true` if the axis-aligned box with the half sizes `half_extents`
    /// centred at `position` overlaps any solid (touching counts as not
    /// overlapping).
    #[must_use]
    pub fn overlaps_box(&self, position: Vec3, half_extents: Vec3) -> bool {
        self.boxes.overlaps_box(position, half_extents)
            || self
                .half_spaces
                .iter()
                .any(|h| h.normal.dot(position) - h.offset - h.support_box(half_extents) < 0.0)
    }
}

impl CollisionWorld for SlopeWorld {
    fn actor_location(&self, id: u32) -> Option<Vec3> {
        self.boxes.actor_location(id)
    }

    /// The box against the boxes and the half-spaces: a box rests on an
    /// inclined plane with its lowest corner, `Σ half_extents[i]·|normal[i]|`
    /// from the plane, and the contact normal is the plane's (a box vertex
    /// or edge on a face reports the face's normal, NATIVE_PHYSICS.md 10.1).
    /// Every primitive is [`PrimitiveKind::Bsp`].
    fn sweep_box(&self, start: Vec3, end: Vec3, half_extents: Vec3) -> Option<TraceHit> {
        let d = end - start;
        if !valid_box_query(start, d, half_extents) {
            return None;
        }
        let length = d.length();
        let mut best = self
            .boxes
            .box_candidate(start, d, half_extents)
            .map(|(c, info)| make_hit(start, d, length, c, info));
        for h in &self.half_spaces {
            if let Some(c) = h
                .cast(start, d, h.support_box(half_extents))
                .and_then(|c| blocking(c, d))
            {
                let hit = make_hit(start, d, length, c, (h.grapple_able, Surface::default()));
                if best.is_none_or(|b| hit.time < b.time) {
                    best = Some(hit);
                }
            }
        }
        best.map(|hit| TraceHit::new(hit, PrimitiveKind::Bsp))
    }

    fn sweep_capsule(&self, start: Vec3, end: Vec3, shape: CollisionShape) -> Option<Hit> {
        let d = end - start;
        let length = d.length();
        if !(length.is_finite() && start.is_finite()) || length < MIN_MOVE {
            return None;
        }
        let mut best = self.boxes.sweep_capsule(start, end, shape);
        for h in &self.half_spaces {
            if let Some(c) = h
                .cast(start, d, h.support(shape))
                .and_then(|c| blocking(c, d))
            {
                let hit = make_hit(start, d, length, c, (h.grapple_able, Surface::default()));
                if best.is_none_or(|b| hit.time < b.time) {
                    best = Some(hit);
                }
            }
        }
        best
    }

    fn raycast(&self, origin: Vec3, direction: Vec3, max_distance: f32) -> Option<Hit> {
        let mut best = self.boxes.raycast(origin, direction, max_distance);
        let dir = direction.normalize_or_zero();
        if !(origin.is_finite() && max_distance.is_finite())
            || max_distance <= 0.0
            || dir == Vec3::ZERO
        {
            return best;
        }
        let d = dir * max_distance;
        for h in &self.half_spaces {
            if let Some(c) = h.cast(origin, d, 0.0) {
                let hit = make_hit(
                    origin,
                    d,
                    max_distance,
                    c,
                    (h.grapple_able, Surface::default()),
                );
                if best.is_none_or(|b| hit.time < b.time) {
                    best = Some(hit);
                }
            }
        }
        best
    }
}

fn make_hit(
    origin: Vec3,
    d: Vec3,
    length: f32,
    c: Candidate,
    (grapple_able, surface): (bool, Surface),
) -> Hit {
    let t = c.t.clamp(0.0, 1.0);
    Hit {
        time: t,
        distance: t * length,
        position: origin + d * t,
        normal: c.normal,
        grapple_able,
        start_penetrating: c.inside,
        surface,
    }
}

impl CollisionWorld for BoxWorld {
    fn actor_location(&self, id: u32) -> Option<Vec3> {
        self.actors.iter().find(|a| a.id == id).map(|a| a.location)
    }

    /// The box against the ground and the boxes, exactly: a box meets the
    /// face of another at its half size from it whatever the sideways
    /// offset, and its corners are square. The normal is the world axis that
    /// separated the two last; of two axes closing at the same time the
    /// earlier in the original's order Z, X, Y. Every primitive is
    /// [`PrimitiveKind::Bsp`].
    fn sweep_box(&self, start: Vec3, end: Vec3, half_extents: Vec3) -> Option<TraceHit> {
        let d = end - start;
        if !valid_box_query(start, d, half_extents) {
            return None;
        }
        self.box_candidate(start, d, half_extents).map(|(c, info)| {
            TraceHit::new(make_hit(start, d, d.length(), c, info), PrimitiveKind::Bsp)
        })
    }

    fn sweep_capsule(&self, start: Vec3, end: Vec3, shape: CollisionShape) -> Option<Hit> {
        let d = end - start;
        let length = d.length();
        if !(length.is_finite() && start.is_finite()) || length < MIN_MOVE {
            return None;
        }
        let mut best: Option<(Candidate, (bool, Surface))> = None;
        let mut consider = |c: Candidate, info: (bool, Surface)| {
            if best.is_none_or(|(b, _)| c.t < b.t) {
                best = Some((c, info));
            }
        };
        if let Some(g) = self.ground {
            let surface = g.z + shape.half_height;
            if start.z < surface {
                if d.z < 0.0 {
                    consider(
                        Candidate {
                            t: 0.0,
                            normal: Vec3::Z,
                            depth: surface - start.z,
                            inside: true,
                        },
                        (g.grapple_able, Surface::default()),
                    );
                }
            } else if d.z < 0.0 {
                let t = (surface - start.z) / d.z;
                if t <= 1.0 {
                    consider(
                        Candidate {
                            t: t.max(0.0),
                            normal: Vec3::Z,
                            depth: 0.0,
                            inside: false,
                        },
                        (g.grapple_able, Surface::default()),
                    );
                }
            }
        }
        for b in &self.boxes {
            if let Some(c) = sweep_vs_box(start, d, &b.bounds, shape).and_then(|c| blocking(c, d)) {
                consider(c, (b.grapple_able, b.surface));
            }
        }
        best.map(|(c, info)| make_hit(start, d, length, c, info))
    }

    fn raycast(&self, origin: Vec3, direction: Vec3, max_distance: f32) -> Option<Hit> {
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
        let mut best: Option<(Candidate, (bool, Surface))> = None;
        let mut consider = |c: Candidate, info: (bool, Surface)| {
            if best.is_none_or(|(b, _)| c.t < b.t) {
                best = Some((c, info));
            }
        };
        if let Some(g) = self.ground {
            if origin.z < g.z {
                consider(
                    Candidate {
                        t: 0.0,
                        normal: Vec3::Z,
                        depth: g.z - origin.z,
                        inside: true,
                    },
                    (g.grapple_able, Surface::default()),
                );
            } else if d.z < 0.0 {
                let t = (g.z - origin.z) / d.z;
                if t <= 1.0 {
                    consider(
                        Candidate {
                            t: t.max(0.0),
                            normal: Vec3::Z,
                            depth: 0.0,
                            inside: false,
                        },
                        (g.grapple_able, Surface::default()),
                    );
                }
            }
        }
        for b in &self.boxes {
            // A ray starting inside a solid is blocked immediately.
            if let Some(c) = ray_vs_box(origin, d, b.bounds.min, b.bounds.max) {
                consider(c, (b.grapple_able, b.surface));
            }
        }
        best.map(|(c, info)| make_hit(origin, d, max_distance, c, info))
    }
}

/// A world all of whose primitives answer as one [`PrimitiveKind`]: the
/// same geometry as BSP, as a mesh's simple-collision body or as a mesh's
/// triangles. For tests of what depends on the kind (the pull-back).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct KindWorld<W> {
    /// The geometry.
    pub world: W,
    /// What every hit of it is reported as.
    pub kind: PrimitiveKind,
}

impl<W> KindWorld<W> {
    /// `world` with every primitive answering as `kind`.
    #[must_use]
    pub fn new(world: W, kind: PrimitiveKind) -> Self {
        Self { world, kind }
    }
}

impl<W: CollisionWorld> CollisionWorld for KindWorld<W> {
    fn sweep_capsule(&self, start: Vec3, end: Vec3, shape: CollisionShape) -> Option<Hit> {
        self.world.sweep_capsule(start, end, shape)
    }

    fn raycast(&self, origin: Vec3, direction: Vec3, max_distance: f32) -> Option<Hit> {
        self.world.raycast(origin, direction, max_distance)
    }

    fn actor_location(&self, id: u32) -> Option<Vec3> {
        self.world.actor_location(id)
    }

    fn sweep_box(&self, start: Vec3, end: Vec3, half_extents: Vec3) -> Option<TraceHit> {
        self.world
            .sweep_box(start, end, half_extents)
            .map(|h| TraceHit::new(h.hit, self.kind))
    }

    fn trace_line(&self, origin: Vec3, direction: Vec3, max_distance: f32) -> Option<TraceHit> {
        self.world
            .trace_line(origin, direction, max_distance)
            .map(|h| TraceHit::new(h.hit, self.kind))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SHAPE: CollisionShape = CollisionShape {
        radius: 20.0,
        half_height: 45.0,
    };

    fn unit_box_world() -> BoxWorld {
        // A 200×200×100 box centred on the origin.
        BoxWorld::new().with_box(
            Vec3::new(-100.0, -100.0, -50.0),
            Vec3::new(100.0, 100.0, 50.0),
            true,
        )
    }

    #[test]
    fn sweep_hits_faces_with_axis_normals() {
        let w = unit_box_world();
        // From +X side moving -X.
        let h = w
            .sweep_capsule(Vec3::new(300.0, 0.0, 0.0), Vec3::new(0.0, 0.0, 0.0), SHAPE)
            .unwrap();
        assert_eq!(h.normal, Vec3::X);
        assert!((h.position.x - 120.0).abs() < 1e-4);
        assert!((h.time - (180.0 / 300.0)).abs() < 1e-6);
        assert!(!h.start_penetrating);
        // From above moving down: lands with bottom on top face.
        let h = w
            .sweep_capsule(
                Vec3::new(10.0, 10.0, 500.0),
                Vec3::new(10.0, 10.0, 0.0),
                SHAPE,
            )
            .unwrap();
        assert_eq!(h.normal, Vec3::Z);
        assert!((h.position.z - 95.0).abs() < 1e-4);
        // From below moving up.
        let h = w
            .sweep_capsule(Vec3::new(0.0, 0.0, -500.0), Vec3::new(0.0, 0.0, 0.0), SHAPE)
            .unwrap();
        assert_eq!(h.normal, Vec3::NEG_Z);
        assert!((h.position.z + 95.0).abs() < 1e-4);
    }

    #[test]
    fn sweep_rounds_vertical_edges() {
        let w = unit_box_world();
        // Diagonal approach towards the (+x,+y) vertical edge.
        let start = Vec3::new(300.0, 300.0, 0.0);
        let h = w.sweep_capsule(start, Vec3::ZERO, SHAPE).unwrap();
        let expected = Vec3::new(1.0, 1.0, 0.0).normalize();
        assert!((h.normal - expected).length() < 1e-4, "{:?}", h.normal);
        // Contact centre is exactly `radius` from the edge.
        let edge = Vec2::new(100.0, 100.0);
        assert!(((h.position.truncate() - edge).length() - 20.0).abs() < 1e-3);
        // Passing the edge diagonally just outside the rounded corner misses,
        // although it would hit a square-cornered (box) approximation.
        let miss = w.sweep_capsule(
            Vec3::new(300.0, 116.0, 0.0),
            Vec3::new(116.0, 300.0, 0.0),
            SHAPE,
        );
        assert!(miss.is_none(), "{miss:?}");
    }

    #[test]
    fn sweep_misses_and_short_moves() {
        let w = unit_box_world();
        assert!(
            w.sweep_capsule(
                Vec3::new(300.0, 0.0, 0.0),
                Vec3::new(130.0, 0.0, 0.0),
                SHAPE
            )
            .is_none()
        );
        assert!(
            w.sweep_capsule(
                Vec3::new(300.0, 0.0, 0.0),
                Vec3::new(300.0, 0.0, 0.0),
                SHAPE
            )
            .is_none()
        );
        // Passing above the box (bottom clears the top).
        assert!(
            w.sweep_capsule(
                Vec3::new(-300.0, 0.0, 96.0),
                Vec3::new(300.0, 0.0, 96.0),
                SHAPE
            )
            .is_none()
        );
    }

    #[test]
    fn no_tunnelling_at_extreme_speed() {
        let w = BoxWorld::new().with_box(
            Vec3::new(0.0, -50.0, -50.0),
            Vec3::new(1.0, 50.0, 50.0),
            false,
        );
        // A 1 UU thick wall and a 1e6 UU move in one sweep.
        let h = w
            .sweep_capsule(
                Vec3::new(-500.0, 0.0, 0.0),
                Vec3::new(1.0e6, 0.0, 0.0),
                SHAPE,
            )
            .unwrap();
        assert_eq!(h.normal, Vec3::NEG_X);
        assert!((h.position.x + 20.0).abs() < 0.1);
    }

    #[test]
    fn penetrating_start_only_blocks_inward_motion() {
        let w = unit_box_world();
        // Centre 1 UU inside the top contact surface.
        let start = Vec3::new(0.0, 0.0, 94.0);
        let down = w
            .sweep_capsule(start, start - Vec3::Z * 10.0, SHAPE)
            .unwrap();
        assert!(down.start_penetrating);
        assert_eq!(down.time, 0.0);
        assert_eq!(down.normal, Vec3::Z);
        assert!(
            w.sweep_capsule(start, start + Vec3::Z * 10.0, SHAPE)
                .is_none()
        );
        // Horizontal motion along the surface is not blocked by the shallow overlap.
        assert!(
            w.sweep_capsule(start, start + Vec3::X * 10.0, SHAPE)
                .is_none()
        );
        assert!(w.overlaps(start, SHAPE));
        assert!(!w.overlaps(Vec3::new(0.0, 0.0, 95.0 + CONTACT_SKIN), SHAPE));
    }

    #[test]
    fn touching_contact_blocks_at_time_zero() {
        let w = unit_box_world();
        let start = Vec3::new(0.0, 0.0, 95.0);
        let h = w
            .sweep_capsule(start, start - Vec3::Z * 5.0, SHAPE)
            .unwrap();
        assert_eq!(h.time, 0.0);
        assert_eq!(h.normal, Vec3::Z);
    }

    #[test]
    fn ground_plane_sweep_and_ray() {
        let w = BoxWorld::new().with_ground(0.0, false);
        let h = w
            .sweep_capsule(
                Vec3::new(0.0, 0.0, 100.0),
                Vec3::new(0.0, 0.0, -100.0),
                SHAPE,
            )
            .unwrap();
        assert_eq!(h.normal, Vec3::Z);
        assert!((h.position.z - 45.0).abs() < 1e-4);
        assert!(!h.grapple_able);
        let r = w
            .raycast(Vec3::new(0.0, 0.0, 10.0), Vec3::new(1.0, 0.0, -1.0), 100.0)
            .unwrap();
        assert!((r.position - Vec3::new(10.0, 0.0, 0.0)).length() < 1e-4);
        assert!((r.distance - 200.0_f32.sqrt()).abs() < 1e-3);
        assert!(
            w.raycast(Vec3::new(0.0, 0.0, 10.0), Vec3::new(1.0, 0.0, -1.0), 5.0)
                .is_none()
        );
        assert!(
            w.raycast(Vec3::new(0.0, 0.0, 10.0), Vec3::Z, 1000.0)
                .is_none()
        );
        assert!(w.overlaps(Vec3::new(0.0, 0.0, 44.0), SHAPE));
    }

    #[test]
    fn raycast_reports_grapple_flag_and_nearest_box() {
        let w = BoxWorld::new()
            .with_box(
                Vec3::new(500.0, -10.0, -10.0),
                Vec3::new(520.0, 10.0, 10.0),
                true,
            )
            .with_box(
                Vec3::new(200.0, -10.0, -10.0),
                Vec3::new(220.0, 10.0, 10.0),
                false,
            );
        let h = w.raycast(Vec3::ZERO, Vec3::X, 1000.0).unwrap();
        assert!(!h.grapple_able, "nearer non-grapple box occludes");
        assert!((h.distance - 200.0).abs() < 1e-3);
        assert_eq!(h.normal, Vec3::NEG_X);
        let h = w
            .raycast(Vec3::new(300.0, 0.0, 0.0), Vec3::X, 1000.0)
            .unwrap();
        assert!(h.grapple_able);
        assert!((h.position.x - 500.0).abs() < 1e-3);
        // Inside a box: blocked immediately.
        let h = w
            .raycast(Vec3::new(210.0, 0.0, 0.0), Vec3::X, 1000.0)
            .unwrap();
        assert_eq!(h.time, 0.0);
        assert!(h.start_penetrating);
        // Degenerate inputs.
        assert!(w.raycast(Vec3::ZERO, Vec3::ZERO, 10.0).is_none());
        assert!(w.raycast(Vec3::ZERO, Vec3::X, 0.0).is_none());
        assert!(
            w.raycast(Vec3::ZERO, Vec3::new(f32::NAN, 0.0, 0.0), 10.0)
                .is_none()
        );
    }

    #[test]
    fn aabb_helpers() {
        let b = Aabb::from_corners(Vec3::new(1.0, 5.0, -2.0), Vec3::new(-1.0, 2.0, 2.0));
        assert_eq!(b.min, Vec3::new(-1.0, 2.0, -2.0));
        assert_eq!(b.max, Vec3::new(1.0, 5.0, 2.0));
        assert_eq!(b.center(), Vec3::new(0.0, 3.5, 0.0));
        assert_eq!(b.half_extents(), Vec3::new(1.0, 1.5, 2.0));
        assert!(b.contains(Vec3::new(0.0, 2.0, 0.0)));
        assert!(!b.contains(Vec3::new(0.0, 1.9, 0.0)));
        let c = Aabb::from_center_half_extents(Vec3::ONE, Vec3::new(-1.0, 1.0, 1.0));
        assert_eq!(c.min, Vec3::ZERO);
    }

    #[test]
    fn half_space_sweeps_land_on_the_slope_with_its_normal() {
        let ramp = HalfSpace::ramp_x(0.8, Vec3::ZERO, false).unwrap();
        assert_eq!(ramp.normal.z, 0.8);
        assert!((ramp.normal.length() - 1.0).abs() < 1e-6);
        let w = SlopeWorld::new(BoxWorld::new()).with_half_space(ramp);
        // Dropping straight down onto the slope at x = 100: the surface is at
        // z = 100·tanθ (tanθ = 0.6/0.8 = 0.75); the cylinder rests on its rim.
        let x = 100.0;
        let h = w
            .sweep_capsule(Vec3::new(x, 0.0, 1000.0), Vec3::new(x, 0.0, -1000.0), SHAPE)
            .unwrap();
        assert_eq!(h.normal, ramp.normal);
        assert!(!h.start_penetrating);
        let rest = SHAPE.radius * 0.6 + SHAPE.half_height * 0.8;
        let expected_z = x * 0.75 + rest / 0.8;
        assert!(
            (h.position.z - expected_z).abs() < 1e-2,
            "{h:?} vs {expected_z}"
        );
        assert!(!w.overlaps(h.position + Vec3::Z * CONTACT_SKIN, SHAPE));
        assert!(w.overlaps(h.position - Vec3::Z, SHAPE));
        // Moving away from the plane is never blocked, even from inside.
        let inside = h.position - Vec3::Z;
        assert!(
            w.sweep_capsule(inside, inside + Vec3::Z * 5.0, SHAPE)
                .is_none()
        );
        let blocked = w
            .sweep_capsule(inside, inside - Vec3::Z * 5.0, SHAPE)
            .unwrap();
        assert!(blocked.start_penetrating && blocked.time == 0.0);
        // Rays.
        let r = w
            .raycast(Vec3::new(0.0, 0.0, 100.0), Vec3::NEG_Z, 1000.0)
            .unwrap();
        assert!((r.position.z).abs() < 1e-3, "{r:?}");
        assert!(
            w.raycast(Vec3::new(0.0, 0.0, 100.0), Vec3::Z, 1000.0)
                .is_none()
        );
        assert!(HalfSpace::ramp_x(0.0, Vec3::ZERO, false).is_none());
        assert!(HalfSpace::ramp_x(1.5, Vec3::ZERO, false).is_none());
        assert!(HalfSpace::through_point(Vec3::ZERO, Vec3::ZERO, false).is_none());
    }

    #[test]
    fn slope_world_takes_the_earliest_primitive() {
        let w = SlopeWorld::new(BoxWorld::new().with_ground(0.0, false)).with_half_space(
            HalfSpace::through_point(Vec3::new(-1.0, 0.0, 0.0), Vec3::new(500.0, 0.0, 0.0), true)
                .unwrap(),
        );
        // Horizontal sweep hits the vertical half-space wall (x = 500).
        let h = w
            .sweep_capsule(
                Vec3::new(0.0, 0.0, 50.0),
                Vec3::new(1000.0, 0.0, 50.0),
                SHAPE,
            )
            .unwrap();
        assert_eq!(h.normal, Vec3::NEG_X);
        assert!((h.position.x - 480.0).abs() < 1e-3);
        assert!(h.grapple_able);
        // Downward sweep hits the ground first.
        let h = w
            .sweep_capsule(
                Vec3::new(0.0, 0.0, 100.0),
                Vec3::new(0.0, 0.0, -100.0),
                SHAPE,
            )
            .unwrap();
        assert_eq!(h.normal, Vec3::Z);
        assert!(!h.grapple_able);
    }

    // -----------------------------------------------------------------
    // The original's pull-back. Expected values are written out from the
    // rule read in the executable (see `pull_back`), not from this code.
    // -----------------------------------------------------------------

    #[test]
    fn pull_back_follows_the_rule_of_each_routine() {
        use PrimitiveKind::{BrushHull, Bsp, PawnCylinder, SimpleHull, TriangleMesh};
        // A trace with the pawn's extent, and a line.
        let (boxed, line) = (TraceExtent::Box, TraceExtent::Zero);
        let close = |got: f32, want: f64, what: &str| {
            assert!(
                (f64::from(got) - want).abs() < 1e-6,
                "{what}: {got} vs {want}"
            );
        };
        // The 28 uu floor check (MaxStepHeight 26 + 2), contact half way
        // down, at 14 uu. BSP, a brush and a simple body: a tenth of the
        // trace is 2.8 uu, more than the 1.0 uu cap, so 13 uu is reported.
        for kind in [Bsp, BrushHull, SimpleHull] {
            close(
                pull_back(kind, boxed, 28.0, 0.5),
                13.0 / 28.0,
                "floor check",
            );
        }
        // A mesh's triangles: 2.8 uu is under the 4.0 uu cap: 11.2 uu.
        close(
            pull_back(TriangleMesh, boxed, 28.0, 0.5),
            11.2 / 28.0,
            "triangles",
        );
        // A 7 uu move stopped at its very end: a tenth, 0.7 uu, whatever
        // the kind (the caps are not reached).
        for kind in [Bsp, BrushHull, SimpleHull, TriangleMesh] {
            close(pull_back(kind, boxed, 7.0, 1.0), 6.3 / 7.0, "7 uu move");
            // Shorter than 1 uu: 0.1 uu, not a tenth.
            close(pull_back(kind, boxed, 0.5, 1.0), 0.4 / 0.5, "0.5 uu move");
            // Exactly 1 uu: both readings give 0.1 uu.
            close(pull_back(kind, boxed, 1.0, 1.0), 0.9, "1 uu move");
        }
        // The caps are reached at 10 uu and at 40 uu.
        close(pull_back(Bsp, boxed, 10.0, 1.0), 9.0 / 10.0, "10 uu");
        close(
            pull_back(Bsp, boxed, 100.0, 1.0),
            99.0 / 100.0,
            "100 uu, K 1",
        );
        close(
            pull_back(TriangleMesh, boxed, 40.0, 1.0),
            36.0 / 40.0,
            "40 uu",
        );
        close(
            pull_back(TriangleMesh, boxed, 100.0, 1.0),
            96.0 / 100.0,
            "100 uu, K 4",
        );
        // The grapple's fire trace is 16,384 uu long (GRAPPLE.md section 4):
        // its anchor on a mesh's triangles is 4.0 uu short of them (measured
        // 3.9967 on 47 anchors, PARITY_FINDINGS V16), on a brush or a simple
        // body 1.0 uu, on BSP 0.5 uu.
        let far = 16384.0_f32;
        let at = 2000.0 / 16384.0_f32;
        close(
            pull_back(TriangleMesh, line, far, at),
            1996.0 / 16384.0,
            "anchor",
        );
        close(
            pull_back(BrushHull, line, far, at),
            1999.0 / 16384.0,
            "brush",
        );
        close(
            pull_back(SimpleHull, line, far, at),
            1999.0 / 16384.0,
            "body",
        );
        close(pull_back(Bsp, line, far, at), 1999.5 / 16384.0, "BSP line");
        // BSP's zero-extent rule is a flat 0.5 uu at any length, also under
        // 1 uu and under 5 uu where rule K would give 0.1 uu and a tenth.
        close(pull_back(Bsp, line, 4.0, 1.0), 3.5 / 4.0, "short BSP line");
        close(
            pull_back(Bsp, line, 0.8, 1.0),
            0.3 / 0.8,
            "shorter BSP line",
        );
        // A pawn's cylinder: a thousandth of the trace, with or without
        // extent.
        close(
            pull_back(PawnCylinder, boxed, 500.0, 0.5),
            0.499,
            "cylinder",
        );
        close(pull_back(PawnCylinder, line, 0.5, 0.5), 0.499, "cylinder");
        // Never before the start of the trace: a contact nearer than the
        // pull-back is reported at time 0.
        for kind in [Bsp, BrushHull, SimpleHull, TriangleMesh, PawnCylinder] {
            for extent in [boxed, line] {
                assert_eq!(pull_back(kind, extent, 28.0, 0.0), 0.0);
                assert_eq!(pull_back(kind, extent, 28.0, f32::NAN), 0.0);
                let t = pull_back(kind, extent, 28.0, 1.0);
                assert!((0.0..1.0).contains(&t), "{kind:?} {extent:?}: {t}");
                // A trace of no length (0.1 / 0 is infinite) or of unknown
                // length: at the start. The cylinder's rule has no length.
                if kind != PawnCylinder {
                    assert_eq!(pull_back(kind, extent, 0.0, 0.3), 0.0);
                    assert_eq!(pull_back(kind, extent, f32::NAN, 0.3), 0.0);
                }
            }
        }
        assert_eq!(pull_back(Bsp, boxed, 28.0, 0.9 / 28.0), 0.0, "0.9 uu away");
        assert_eq!(pull_back(TriangleMesh, boxed, 28.0, 2.7 / 28.0), 0.0);
        // The arithmetic is the original's, operation for operation: for a
        // trace between 1 and 10 uu the time is reduced by the float 0.1
        // itself.
        assert_eq!(
            pull_back(Bsp, boxed, 2.266, 0.75).to_bits(),
            (0.75_f32 - 0.1_f32).to_bits()
        );
        assert_eq!(
            pull_back(Bsp, boxed, 0.5, 0.75).to_bits(),
            (0.75_f32 - 0.1_f32 / 0.5_f32).to_bits()
        );
        assert_eq!(
            pull_back(TriangleMesh, boxed, 64.0, 0.75).to_bits(),
            (0.75_f32 - 4.0_f32 / 64.0_f32).to_bits()
        );
    }

    #[test]
    fn a_trace_shortened_by_a_bsp_hit_is_pulled_back_with_the_shortened_length() {
        // The 28 uu floor check from 4 uu above a mesh's triangles that lie
        // 6 uu above a BSP floor. BSP: contact at 10 uu, reported 1.0 uu
        // short, at 9 uu. The original then looks 5 uu further and no more:
        // the other primitives see a trace of 14 uu, half the check.
        let bsp_time = pull_back(PrimitiveKind::Bsp, TraceExtent::Box, 28.0, 10.0 / 28.0);
        assert!((bsp_time - 9.0 / 28.0).abs() < 1e-6);
        let fraction = bsp_checked_fraction(bsp_time, 28.0);
        // (5 + 9)·(9/28) / (9 + 0.0001) = 0.5 less 5.6e-6.
        assert!(
            (f64::from(fraction) - 0.5 * 9.0 / 9.0001).abs() < 1e-6,
            "{fraction}"
        );
        // The triangles, 4 uu down: on the 14 uu trace a tenth is 1.4 uu
        // (on the whole 28 uu it would be 2.8), so they are reported at
        // 2.6 uu.
        let raw = 4.0 / 28.0;
        let mesh = reported_time(
            PrimitiveKind::TriangleMesh,
            TraceExtent::Box,
            28.0,
            raw,
            fraction,
        );
        assert!((mesh * 28.0 - 2.6).abs() < 1e-4, "{}", mesh * 28.0);
        let whole = reported_time(
            PrimitiveKind::TriangleMesh,
            TraceExtent::Box,
            28.0,
            raw,
            1.0,
        );
        assert!((whole * 28.0 - 1.2).abs() < 1e-5);
        assert_eq!(
            whole.to_bits(),
            pull_back(PrimitiveKind::TriangleMesh, TraceExtent::Box, 28.0, raw).to_bits()
        );
        // No BSP hit, or one at the end of the trace: the whole trace.
        assert_eq!(bsp_checked_fraction(1.0, 28.0), 1.0);
        // A BSP hit less than 5 uu before the end shortens nothing:
        // (5 + 24)·(24/28)/24 > 1.
        assert_eq!(bsp_checked_fraction(24.0 / 28.0, 28.0), 1.0);
        // A BSP hit reported at the start: nothing else is checked.
        assert_eq!(bsp_checked_fraction(0.0, 28.0), 0.0);
        // The hit a world reports carries the fraction.
        let hit = Hit {
            time: raw,
            distance: 4.0,
            position: Vec3::ZERO,
            normal: Vec3::Z,
            grapple_able: false,
            start_penetrating: false,
            surface: Surface::default(),
        };
        let traced = TraceHit::new(hit, PrimitiveKind::TriangleMesh);
        assert_eq!(traced.checked_fraction, 1.0);
        assert_eq!(
            traced.reported_time(TraceExtent::Box, 28.0).to_bits(),
            whole.to_bits()
        );
        assert_eq!(
            traced
                .with_checked_fraction(fraction)
                .reported_time(TraceExtent::Box, 28.0)
                .to_bits(),
            mesh.to_bits()
        );
    }

    // -----------------------------------------------------------------
    // The pawn's box.
    // -----------------------------------------------------------------

    /// The player's box (DEFAULTS.md: radius 21, half height 44).
    const PAWN: Vec3 = Vec3::new(21.0, 21.0, 44.0);

    #[test]
    fn a_box_meets_a_wall_face_at_its_half_width_whatever_the_sideways_offset() {
        // P4, the Workshop door: stopped with the centre 21 uu from the face
        // at a sideways offset where a cylinder would pass the corner.
        let w = BoxWorld::new().with_box(
            Vec3::new(-50.0, -1000.0, -100.0),
            Vec3::new(0.0, 0.0, 300.0),
            false,
        );
        // The wall ends at y = 0; the centre passes at y = 0, 9, 20 and 20.9
        // beyond its end. The box (half width 21) still overlaps it sideways.
        for y in [0.0_f32, 9.0, 20.0, 20.9] {
            let h = w
                .sweep_box(Vec3::new(200.0, y, 50.0), Vec3::new(0.0, y, 50.0), PAWN)
                .expect("the box does not pass");
            assert_eq!(h.hit.normal, Vec3::X, "y {y}");
            assert!((h.hit.position.x - 21.0).abs() < 1e-4, "y {y}: {:?}", h.hit);
            assert!((h.hit.time - 179.0 / 200.0).abs() < 1e-6);
            assert_eq!(h.kind, PrimitiveKind::Bsp);
            assert_eq!(h.checked_fraction, 1.0);
            assert!(!h.hit.start_penetrating);
        }
        // 21.5 uu beyond the end: clear.
        assert!(
            w.sweep_box(
                Vec3::new(200.0, 21.5, 50.0),
                Vec3::new(0.0, 21.5, 50.0),
                PAWN
            )
            .is_none()
        );
        // The cylinder of the same size passes the corner 9 uu beyond the
        // end later: at sqrt(21² − 9²) = 18.97 from the face, not at 21.
        let shape = CollisionShape {
            radius: 21.0,
            half_height: 44.0,
        };
        let c = w
            .sweep_capsule(
                Vec3::new(200.0, 9.0, 50.0),
                Vec3::new(0.0, 9.0, 50.0),
                shape,
            )
            .unwrap();
        assert!((c.position.x - (21.0_f32 * 21.0 - 81.0).sqrt()).abs() < 1e-3);
    }

    #[test]
    fn box_corners_are_square() {
        let w = unit_box_world();
        let e = Vec3::new(20.0, 20.0, 45.0);
        // The box and the obstacle overlap sideways where the centre is
        // within (120, 120) of the origin. The path x + y = 416 of
        // `sweep_rounds_vertical_edges` never is (that needs x + y ≤ 240):
        // it misses the square corner as it misses the round one.
        assert!(
            w.sweep_box(
                Vec3::new(300.0, 116.0, 0.0),
                Vec3::new(116.0, 300.0, 0.0),
                e
            )
            .is_none()
        );
        // The path x + y = 238 passes through (119, 119), 1 uu inside the
        // square corner, and is stopped; the cylinder passes (its rounded
        // corner reaches 100 + 20/√2 = 114.1 on the diagonal).
        let (from, to) = (Vec3::new(238.0, 0.0, 0.0), Vec3::new(0.0, 238.0, 0.0));
        let h = w.sweep_box(from, to, e).expect("square corner");
        // Y overlaps from the start (y = 0); X is the last axis to close, at
        // x = 120: t = 118/238, where y = 118 is still within 120.
        assert!((h.hit.time - 118.0 / 238.0).abs() < 1e-6, "{:?}", h.hit);
        assert_eq!(h.hit.normal, Vec3::X);
        assert!(
            w.sweep_capsule(
                from,
                to,
                CollisionShape {
                    radius: 20.0,
                    half_height: 45.0
                }
            )
            .is_none()
        );
        // Head-on into the vertical edge along the diagonal: both axes close
        // at the same time and the earlier in the order Z, X, Y names the
        // normal: X (a cylinder reports the diagonal).
        let h = w
            .sweep_box(Vec3::new(300.0, 300.0, 0.0), Vec3::ZERO, e)
            .unwrap();
        assert_eq!(h.hit.normal, Vec3::X);
        assert!((h.hit.position - Vec3::new(120.0, 120.0, 0.0)).length() < 1e-3);
        // From above onto the top edge, all three closing together: Z.
        let h = w
            .sweep_box(
                Vec3::new(320.0, 320.0, 295.0),
                Vec3::new(20.0, 20.0, -5.0),
                e,
            )
            .unwrap();
        assert_eq!(h.hit.normal, Vec3::Z);
        assert!((h.hit.position - Vec3::new(120.0, 120.0, 95.0)).length() < 1e-3);
    }

    #[test]
    fn box_faces_ground_and_start_rules() {
        let w = unit_box_world().with_ground(-500.0, true);
        let e = Vec3::new(20.0, 30.0, 45.0);
        // The three faces, each at the box's own half size on that axis.
        let h = w
            .sweep_box(Vec3::new(0.0, 300.0, 0.0), Vec3::ZERO, e)
            .unwrap();
        assert_eq!(h.hit.normal, Vec3::Y);
        assert!((h.hit.position.y - 130.0).abs() < 1e-4);
        assert!((h.hit.distance - 170.0).abs() < 1e-3);
        let h = w
            .sweep_box(Vec3::new(0.0, 0.0, -400.0), Vec3::ZERO, e)
            .unwrap();
        assert_eq!(h.hit.normal, Vec3::NEG_Z);
        assert!((h.hit.position.z + 95.0).abs() < 1e-4);
        // The ground plane.
        let h = w
            .sweep_box(
                Vec3::new(900.0, 0.0, 0.0),
                Vec3::new(900.0, 0.0, -1000.0),
                e,
            )
            .unwrap();
        assert_eq!(h.hit.normal, Vec3::Z);
        assert!((h.hit.position.z + 455.0).abs() < 1e-4);
        assert!(h.hit.grapple_able);
        // Touching the top face exactly: blocked at time 0 when moving into
        // it, free to slide along it and to leave.
        let on = Vec3::new(0.0, 0.0, 95.0);
        let h = w.sweep_box(on, on - Vec3::Z * 5.0, e).unwrap();
        assert_eq!((h.hit.time, h.hit.normal), (0.0, Vec3::Z));
        assert!(!h.hit.start_penetrating);
        assert!(w.sweep_box(on, on + Vec3::X * 500.0, e).is_none());
        assert!(w.sweep_box(on, on + Vec3::Z * 5.0, e).is_none());
        assert!(!w.overlaps_box(on, e));
        // Overlapping by 1 uu: only motion into the face of least overlap is
        // blocked.
        let inside = Vec3::new(0.0, 0.0, 94.0);
        assert!(w.overlaps_box(inside, e));
        let h = w.sweep_box(inside, inside - Vec3::Z, e).unwrap();
        assert!(h.hit.start_penetrating && h.hit.time == 0.0 && h.hit.normal == Vec3::Z);
        assert!(w.sweep_box(inside, inside + Vec3::Z, e).is_none());
        assert!(w.sweep_box(inside, inside + Vec3::X * 10.0, e).is_none());
        // Below the ground.
        assert!(w.overlaps_box(Vec3::new(900.0, 0.0, -456.0), e));
        // No tunnelling, and degenerate queries are no hit.
        let h = w
            .sweep_box(Vec3::new(1.0e6, 0.0, 0.0), Vec3::new(-1.0e6, 0.0, 0.0), e)
            .unwrap();
        assert!((h.hit.position.x - 120.0).abs() < 0.5);
        assert!(w.sweep_box(on, on, e).is_none());
        assert!(w.sweep_box(on, Vec3::new(f32::NAN, 0.0, 0.0), e).is_none());
        assert!(
            w.sweep_box(on, Vec3::ZERO, Vec3::new(20.0, 0.0, 45.0))
                .is_none()
        );
    }

    #[test]
    fn a_box_rests_on_a_slope_with_its_lowest_corner() {
        // A plane rising 0.3 per uu along +X and 0.4 per uu along +Y through
        // the origin: z = 0.3·x + 0.4·y, normal (−0.3, −0.4, 1) normalised.
        // The box's lowest corner is the one at (−21, −21) from its centre,
        // where the plane is 21·(0.3 + 0.4) = 14.7 uu lower than under the
        // centre, so the centre rests 44 + 14.7 above the plane (P4; the
        // rule of `asamu-world`'s box sweep). The cylinder's lowest rim
        // point is only 21·sqrt(0.3² + 0.4²) = 10.5 uu lower.
        let plane =
            HalfSpace::through_point(Vec3::new(-0.3, -0.4, 1.0), Vec3::ZERO, false).unwrap();
        let w = SlopeWorld::new(BoxWorld::new()).with_half_space(plane);
        for (x, y) in [(0.0_f32, 0.0_f32), (100.0, -40.0), (-250.0, 300.0)] {
            let top = Vec3::new(x, y, 1000.0);
            let surface = 0.3 * x + 0.4 * y;
            let h = w.sweep_box(top, top - Vec3::Z * 2000.0, PAWN).unwrap();
            assert!(
                (h.hit.position.z - (surface + 44.0 + 14.7)).abs() < 2e-3,
                "{:?}",
                h.hit
            );
            assert_eq!(h.hit.normal, plane.normal);
            assert_eq!(h.kind, PrimitiveKind::Bsp);
            assert!(!w.overlaps_box(h.hit.position + Vec3::Z * 0.01, PAWN));
            assert!(w.overlaps_box(h.hit.position - Vec3::Z * 0.01, PAWN));
            let c = w
                .sweep_capsule(
                    top,
                    top - Vec3::Z * 2000.0,
                    CollisionShape {
                        radius: 21.0,
                        half_height: 44.0,
                    },
                )
                .unwrap();
            assert!((c.position.z - (surface + 44.0 + 10.5)).abs() < 2e-3);
        }
        // On a ramp along one axis the two shapes rest at the same height.
        let ramp = HalfSpace::ramp_x(0.8, Vec3::ZERO, false).unwrap();
        let w = SlopeWorld::new(BoxWorld::new()).with_half_space(ramp);
        let top = Vec3::new(10.0, 5.0, 1000.0);
        let h = w.sweep_box(top, top - Vec3::Z * 2000.0, PAWN).unwrap();
        // Gradient 0.6/0.8 = 0.75: 44 + 21·0.75 above the plane.
        assert!((h.hit.position.z - (7.5 + 44.0 + 15.75)).abs() < 2e-3);
    }

    /// A world from before the box: only the two older queries.
    struct OldWorld(BoxWorld);

    impl CollisionWorld for OldWorld {
        fn sweep_capsule(&self, start: Vec3, end: Vec3, shape: CollisionShape) -> Option<Hit> {
            self.0.sweep_capsule(start, end, shape)
        }

        fn raycast(&self, origin: Vec3, direction: Vec3, max_distance: f32) -> Option<Hit> {
            self.0.raycast(origin, direction, max_distance)
        }
    }

    #[test]
    fn a_world_without_the_box_falls_back_to_its_cylinder_as_bsp() {
        let boxes = unit_box_world();
        let old = OldWorld(boxes.clone());
        let (from, to) = (Vec3::new(300.0, 300.0, 0.0), Vec3::ZERO);
        let e = Vec3::new(20.0, 20.0, 45.0);
        let fallback = old.sweep_box(from, to, e).unwrap();
        let cylinder = boxes
            .sweep_capsule(
                from,
                to,
                CollisionShape {
                    radius: 20.0,
                    half_height: 45.0,
                },
            )
            .unwrap();
        assert_eq!(fallback.hit, cylinder);
        assert_eq!(fallback.kind, PrimitiveKind::Bsp);
        assert_eq!(fallback.checked_fraction, 1.0);
        // The world that implements the box answers with the box.
        assert_ne!(boxes.sweep_box(from, to, e).unwrap().hit, cylinder);
        let ray = old.trace_line(Vec3::new(300.0, 0.0, 0.0), Vec3::NEG_X, 1000.0);
        let ray = ray.unwrap();
        assert_eq!(
            Some(ray.hit),
            boxes.raycast(Vec3::new(300.0, 0.0, 0.0), Vec3::NEG_X, 1000.0)
        );
        assert_eq!(ray.kind, PrimitiveKind::Bsp);
    }

    #[test]
    fn kind_world_changes_the_kind_and_nothing_else() {
        let boxes = unit_box_world();
        let (from, to) = (Vec3::new(300.0, 10.0, 0.0), Vec3::ZERO);
        let e = Vec3::new(20.0, 20.0, 45.0);
        let plain = boxes.sweep_box(from, to, e).unwrap();
        for kind in [
            PrimitiveKind::Bsp,
            PrimitiveKind::BrushHull,
            PrimitiveKind::SimpleHull,
            PrimitiveKind::TriangleMesh,
            PrimitiveKind::PawnCylinder,
        ] {
            let w = KindWorld::new(boxes.clone(), kind);
            let h = w.sweep_box(from, to, e).unwrap();
            assert_eq!(h.hit, plain.hit);
            assert_eq!(h.kind, kind);
            let r = w
                .trace_line(Vec3::new(300.0, 0.0, 0.0), Vec3::NEG_X, 1000.0)
                .unwrap();
            assert_eq!(r.kind, kind);
            assert!((r.hit.distance - 200.0).abs() < 1e-3);
            assert_eq!(
                w.sweep_capsule(
                    from,
                    to,
                    CollisionShape {
                        radius: 20.0,
                        half_height: 45.0
                    }
                ),
                boxes.sweep_capsule(
                    from,
                    to,
                    CollisionShape {
                        radius: 20.0,
                        half_height: 45.0
                    }
                )
            );
        }
        assert_eq!(PrimitiveKind::default(), PrimitiveKind::Bsp);
    }

    /// Exhaustive-ish check that the cylinder sweep never ends inside a box:
    /// sweeps from many outside starts towards many targets.
    #[test]
    fn swept_contacts_never_end_inside() {
        let w = unit_box_world();
        let mut checked = 0;
        for i in 0..24 {
            let a = i as f32 / 24.0 * core::f32::consts::TAU;
            for zi in -3..=3 {
                let start = Vec3::new(a.cos() * 400.0, a.sin() * 400.0, zi as f32 * 60.0);
                for j in 0..12 {
                    let b = j as f32 / 12.0 * core::f32::consts::TAU;
                    let end = Vec3::new(b.cos() * 50.0, b.sin() * 50.0, (j as f32 - 6.0) * 30.0);
                    if let Some(h) = w.sweep_capsule(start, end, SHAPE) {
                        let rest = h.position + h.normal * CONTACT_SKIN;
                        assert!(!w.overlaps(rest, SHAPE), "{start} -> {end}: {h:?}");
                        checked += 1;
                    }
                }
            }
        }
        assert!(checked > 100);
    }
}
