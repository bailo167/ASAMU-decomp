//! Collision interface used by the simulation, and simple deterministic
//! implementations: [`BoxWorld`] (axis-aligned boxes + ground plane) for tests
//! and the graybox prototype, and [`SlopeWorld`] (a `BoxWorld` plus inclined
//! solid half-spaces, for slope tests of the native-physics port).
//!
//! # Shape: an upright cylinder standing in for the "capsule"
//!
//! The interface is called [`CollisionWorld::sweep_capsule`] and the
//! parameters `capsule_radius`/`capsule_half_height`, but [`BoxWorld`] sweeps
//! an **upright, flat-ended cylinder** of that radius and half-height. A flat
//! bottom makes standing on box edges and ground contact simple and exact. Stock
//! UE3 pawns collide as an axis-aligned cylinder, so this is also the closer
//! stand-in for the original (that ASAMU's pawn does the same is TENTATIVE).
//!
//! # Tunnelling
//!
//! Sweeps are **continuous and exact** for this shape against axis-aligned
//! boxes: the cylinder sweep is reduced to a ray cast against the Minkowski sum
//! of each box and the cylinder (a box with rounded vertical edges), decomposed
//! into two expanded slabs and four corner cylinders. There is no substepping
//! and no speed limit beyond `f32` precision, so the player cannot tunnel
//! through geometry at any speed.
//!
//! # Contacts
//!
//! A hit reports the fraction of the sweep travelled, the shape centre at
//! contact and the surface normal. If the sweep **starts** overlapping a box,
//! it reports a hit at `time = 0` with the normal of least penetration — but
//! only when moving *into* that normal, so an overlapping shape can always move
//! out. Callers keep a small separation ([`CONTACT_SKIN`]) after contacts.
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

/// Separation (UU) kept between the shape and surfaces after a contact.
///
/// A numerical tolerance of this implementation, **not** a gameplay constant.
/// It does show up in traces (a standing player's centre is
/// `floor + half_height + CONTACT_SKIN`); how far the original keeps the pawn
/// above the floor is a separate, parity-relevant quantity to be taken from
/// evidence, not from this value.
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

/// Geometry queries the simulation needs. Implementations must be
/// deterministic (same inputs → same outputs, independent of call history).
pub trait CollisionWorld {
    /// Sweeps the shape's centre from `start` to `end` and returns the first
    /// blocking contact, if any.
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

/// A [`BoxWorld`] plus solid half-spaces (exact upright-cylinder sweeps
/// against inclined planes). The solid is the union of all primitives; ties
/// keep the box-world hit, then the earlier half-space.
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

impl CollisionWorld for SlopeWorld {
    fn actor_location(&self, id: u32) -> Option<Vec3> {
        self.boxes.actor_location(id)
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
