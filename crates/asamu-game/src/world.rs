//! The collision world a [`crate::Game`] hands to the player simulation.
//!
//! [`GameWorld`] combines the axis-aligned boxes of hand-made levels
//! ([`BoxWorld`]) with the triangle collision of a converted level
//! ([`SceneCollision`]: the static [`asamu_world::collision::CollisionScene`]
//! shared through an `Arc`, plus the current dynamic instances, the loaded
//! sub-levels and the state the gameplay script reads from hit actors —
//! crystal charge, moving actors' locations). It implements
//! [`CollisionWorld`]: the earliest contact of either part wins (ties keep
//! the box), sweeps test instances that block pawns, rays those that block
//! zero-extent traces.

use std::sync::Arc;

use asamu_player::BoxWorld;
use asamu_player::world::{
    ActorClass, ActorTag, CollisionShape, CollisionWorld, Hit, MIN_MOVE, Surface,
};
use asamu_world::SurfaceTag;
use asamu_world::collision::{CollisionClass, Instance, QueryFilter, QueryKind, SceneHit};
use asamu_world::scene::LoadedMap;
use glam::Vec3;

/// Triangle collision of a converted level with its run-time state.
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
        let t = (h.t as f32).clamp(0.0, 1.0);
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

    /// Swept player cylinder against the scene.
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

    /// Zero-extent trace against the scene.
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

    /// `true` if the shape centred at `position` overlaps any solid.
    #[must_use]
    pub fn overlaps(&self, position: Vec3, shape: CollisionShape) -> bool {
        self.boxes.overlaps(position, shape)
            || self
                .scene
                .as_ref()
                .is_some_and(|s| s.overlaps(position, shape))
    }
}

fn earliest(a: Option<Hit>, b: Option<Hit>) -> Option<Hit> {
    match (a, b) {
        (Some(x), Some(y)) => Some(if y.time < x.time { y } else { x }),
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
}
