//! Runtime world representation: levels, checkpoints, grapple points,
//! grapple-reactive objects, the ability state a level sets, and levels
//! converted from the user's original install.
//!
//! This crate holds plain data (no Bevy, no UE3 parsing) plus the run-time
//! state machines of map-placed gameplay actors ([`objects`]: recharge
//! crystals, glow flowers, movers, interactables, attractor pads) and the
//! per-level ability state ([`abilities`]).
//!
//! Two kinds of level exist:
//!
//! - [`graybox_test_level`]: **hand-made test geometry, not original
//!   content** (axis-aligned boxes, [`Level`]).
//! - Converted levels: [`scene::load_map`] reads what `asamu-import levels`
//!   and `asamu-import meshes --collision` wrote to a user-local directory
//!   (never the repository) and builds triangle collision
//!   ([`collision`]: a two-level BVH over static-mesh kDOP triangles, BSP
//!   and blocking-volume hulls, with exact swept-cylinder and ray queries)
//!   and the gameplay actors with the original's rules ([`gameplay`]:
//!   player start, checkpoints, kill zones, triggers, falling rocks).
//!   [`rotation`] holds the UE3 transform math for actors that move;
//!   [`fixtures`] writes synthetic converted data for tests.
//!
//! Conventions: UE3 axes (X forward, Y right, Z up), distances in Unreal
//! units (UU), yaw in radians (UE3 convention, + turns right) unless a field
//! says it holds rotator units.

use std::collections::BTreeSet;

use glam::Vec3;
use serde::{Deserialize, Serialize};
use thiserror::Error;

pub mod abilities;
pub mod collision;
pub mod fixtures;
pub mod gameplay;
pub mod objects;
pub mod rotation;
pub mod scene;

pub use abilities::{
    KismetAbilityActions, LevelAbilities, ORIGINAL_ABILITY_ACTIONS, level_start_abilities,
};
pub use objects::{
    Attractor, GlowFlower, Interactable, Mover, MoverPath, ObjectEvent, RechargeCrystal,
    WorldEvent, WorldObjects,
};

/// UE3 actor `Tag` values the gameplay script tests (GRAPPLE.md §5,
/// ABILITIES.md §5). The tag `NotGrappleAble` is `grapple_able == false`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SurfaceTag {
    /// No tag the script tests.
    #[default]
    None,
    /// `TopOnlyGrappleAble`.
    TopOnlyGrappleAble,
    /// `BottomOnlyGrappleAble`.
    BottomOnlyGrappleAble,
    /// `grappleInteractable`.
    GrappleInteractable,
    /// `NotLandable`.
    NotLandable,
}

/// What a static box stands for in the original.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BoxKind {
    /// A `StaticMeshActor` (has a static-mesh component; the HUD crosshair
    /// reacts to it).
    #[default]
    StaticMesh,
    /// Level geometry without a static-mesh component (BSP).
    WorldGeometry,
}

/// What a collision primitive belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PrimitiveKind {
    /// BSP-like level geometry.
    WorldGeometry,
    /// A static mesh (static boxes and grapple points).
    StaticMesh,
    /// A mover (`InterpActor`).
    Mover,
    /// A recharge crystal.
    RechargeCrystal,
    /// A glow flower.
    GlowFlower,
    /// A story-mode interactable.
    Interactable,
}

/// One collision box with what the gameplay script sees of it.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct CollisionPrimitive {
    /// Minimum corner, UU (movers: at their current position).
    pub min: Vec3,
    /// Maximum corner, UU.
    pub max: Vec3,
    /// `false` = the tag `NotGrappleAble`.
    pub grapple_able: bool,
    /// The actor's tag.
    pub tag: SurfaceTag,
    /// What it belongs to.
    pub kind: PrimitiveKind,
    /// Actor id of tracked objects.
    pub actor: Option<u32>,
}

/// Where a level came from.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LevelOrigin {
    /// Hand-made test geometry; contains nothing from the original game.
    HandMadeGraybox,
    /// Converted locally from the user's original install (never committed).
    ConvertedFromOriginal {
        /// Original map name.
        map: String,
    },
}

/// A static axis-aligned box (graybox geometry).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct StaticBox {
    /// Minimum corner, UU.
    pub min: Vec3,
    /// Maximum corner, UU.
    pub max: Vec3,
    /// Whether the grapple can attach to it (`false` = tag
    /// `NotGrappleAble`).
    pub grapple_able: bool,
    /// Human-readable label (debugging / rendering).
    pub label: String,
    /// The actor's tag (besides `NotGrappleAble`).
    #[serde(default)]
    pub tag: SurfaceTag,
    /// What the box stands for.
    #[serde(default)]
    pub kind: BoxKind,
}

impl StaticBox {
    /// A static-mesh box from two corners in any order, without a tag.
    #[must_use]
    pub fn new(a: Vec3, b: Vec3, grapple_able: bool, label: impl Into<String>) -> Self {
        Self {
            min: a.min(b),
            max: a.max(b),
            grapple_able,
            label: label.into(),
            tag: SurfaceTag::None,
            kind: BoxKind::StaticMesh,
        }
    }

    /// The same box with `tag` (builder style).
    #[must_use]
    pub fn with_tag(mut self, tag: SurfaceTag) -> Self {
        self.tag = tag;
        self
    }
}

/// A dedicated grapple target (rendered and collided as a small cube).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct GrapplePoint {
    /// Centre, UU.
    pub position: Vec3,
    /// Half the cube's edge length, UU.
    pub half_extent: f32,
}

/// Where the player appears.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct SpawnPoint {
    /// Floor contact point under the player's feet, UU. The runtime places
    /// the collision shape on top of it.
    pub feet: Vec3,
    /// Initial view yaw, radians.
    pub yaw: f32,
}

/// A checkpoint: entering its volume makes `spawn` the respawn point.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Checkpoint {
    /// Unique id within the level.
    pub id: u32,
    /// Trigger volume minimum corner, UU.
    pub min: Vec3,
    /// Trigger volume maximum corner, UU.
    pub max: Vec3,
    /// Respawn point once activated.
    pub spawn: SpawnPoint,
}

impl Checkpoint {
    /// `true` if `p` lies inside the trigger volume (boundary inclusive).
    #[must_use]
    pub fn contains(&self, p: Vec3) -> bool {
        p.cmpge(self.min).all() && p.cmple(self.max).all()
    }
}

/// A playable level.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Level {
    /// Display name.
    pub name: String,
    /// Provenance of the level data.
    pub origin: LevelOrigin,
    /// Static collision/render boxes.
    pub static_boxes: Vec<StaticBox>,
    /// Grapple targets.
    pub grapple_points: Vec<GrapplePoint>,
    /// Initial spawn.
    pub player_start: SpawnPoint,
    /// Checkpoints in progression order.
    pub checkpoints: Vec<Checkpoint>,
    /// Falling below this Z kills/respawns the player, UU. (Stock UE3 maps
    /// carry a per-level kill height in their world settings; that ASAMU's
    /// maps use it for falls is TENTATIVE. For converted levels the value
    /// will come from the map; for the graybox it is hand-picked.)
    pub kill_z: f32,
    /// Recharge crystals.
    #[serde(default)]
    pub crystals: Vec<RechargeCrystal>,
    /// Glow flowers.
    #[serde(default)]
    pub flowers: Vec<GlowFlower>,
    /// Movers.
    #[serde(default)]
    pub movers: Vec<Mover>,
    /// Story-mode interactables.
    #[serde(default)]
    pub interactables: Vec<Interactable>,
    /// Attractor pads.
    #[serde(default)]
    pub attractors: Vec<Attractor>,
    /// Ability state applied at level start (the original's Kismet
    /// actions; see [`abilities`]).
    #[serde(default)]
    pub abilities: LevelAbilities,
}

/// Level validation errors.
#[derive(Debug, Clone, PartialEq, Error)]
pub enum LevelError {
    /// A number is NaN or infinite.
    #[error("{what}: non-finite value")]
    NonFinite {
        /// Which element.
        what: String,
    },
    /// A box or volume has zero or negative size.
    #[error("{what}: min must be < max on every axis")]
    Degenerate {
        /// Which element.
        what: String,
    },
    /// A grapple point has a non-positive size.
    #[error("grapple point {index}: half_extent must be > 0")]
    BadGrapplePoint {
        /// Index in `grapple_points`.
        index: usize,
    },
    /// Two checkpoints share an id.
    #[error("duplicate checkpoint id {0}")]
    DuplicateCheckpoint(u32),
    /// A spawn point is below `kill_z`.
    #[error("{what}: spawn is below kill_z")]
    SpawnBelowKillZ {
        /// Which spawn.
        what: String,
    },
    /// Two level objects share an actor id.
    #[error("duplicate object id {0}")]
    DuplicateObjectId(u32),
    /// A level object has an invalid value.
    #[error("object {id}: {what}")]
    BadObject {
        /// Actor id.
        id: u32,
        /// What is wrong.
        what: &'static str,
    },
}

fn finite3(v: Vec3) -> bool {
    v.is_finite()
}

impl Level {
    /// Checks the level for malformed data.
    ///
    /// # Errors
    /// The first [`LevelError`] found.
    pub fn validate(&self) -> Result<(), LevelError> {
        if !self.kill_z.is_finite() {
            return Err(LevelError::NonFinite {
                what: "kill_z".into(),
            });
        }
        for (i, b) in self.static_boxes.iter().enumerate() {
            let what = format!("static box {i} ({})", b.label);
            if !(finite3(b.min) && finite3(b.max)) {
                return Err(LevelError::NonFinite { what });
            }
            if !b.min.cmplt(b.max).all() {
                return Err(LevelError::Degenerate { what });
            }
        }
        for (index, g) in self.grapple_points.iter().enumerate() {
            if !(finite3(g.position) && g.half_extent.is_finite()) {
                return Err(LevelError::NonFinite {
                    what: format!("grapple point {index}"),
                });
            }
            if g.half_extent <= 0.0 {
                return Err(LevelError::BadGrapplePoint { index });
            }
        }
        let check_spawn = |s: &SpawnPoint, what: String| -> Result<(), LevelError> {
            if !(finite3(s.feet) && s.yaw.is_finite()) {
                return Err(LevelError::NonFinite { what });
            }
            if s.feet.z < self.kill_z {
                return Err(LevelError::SpawnBelowKillZ { what });
            }
            Ok(())
        };
        check_spawn(&self.player_start, "player_start".into())?;
        // Sets, not lists: converted levels hold thousands of objects.
        let mut ids: BTreeSet<u32> = BTreeSet::new();
        for c in &self.checkpoints {
            let what = format!("checkpoint {}", c.id);
            if !ids.insert(c.id) {
                return Err(LevelError::DuplicateCheckpoint(c.id));
            }
            if !(finite3(c.min) && finite3(c.max)) {
                return Err(LevelError::NonFinite { what });
            }
            if !c.min.cmplt(c.max).all() {
                return Err(LevelError::Degenerate { what });
            }
            check_spawn(&c.spawn, what)?;
        }
        self.validate_objects()
    }

    fn validate_objects(&self) -> Result<(), LevelError> {
        let bad = |id: u32, what: &'static str| Err(LevelError::BadObject { id, what });
        let mut ids: BTreeSet<u32> = BTreeSet::new();
        let mut claim = |id: u32| -> Result<(), LevelError> {
            if !ids.insert(id) {
                return Err(LevelError::DuplicateObjectId(id));
            }
            Ok(())
        };
        for c in &self.crystals {
            claim(c.id)?;
            if !(finite3(c.center) && c.half_extent.is_finite() && c.half_extent > 0.0) {
                return bad(c.id, "crystal needs a finite centre and half_extent > 0");
            }
            if !(c.recharge_delay.is_finite() && c.recharge_delay >= 0.0) {
                return bad(c.id, "recharge_delay must be finite and >= 0");
            }
        }
        for f in &self.flowers {
            claim(f.id)?;
            if !(finite3(f.center) && f.half_extent.is_finite() && f.half_extent > 0.0) {
                return bad(f.id, "flower needs a finite centre and half_extent > 0");
            }
        }
        for m in &self.movers {
            claim(m.id)?;
            if !(finite3(m.min) && finite3(m.max) && m.min.cmplt(m.max).all()) {
                return bad(m.id, "mover box must be finite with min < max");
            }
            let MoverPath::PingPong { offset, period } = m.path;
            if !(finite3(offset) && period.is_finite() && period > 0.0) {
                return bad(m.id, "mover path needs a finite offset and period > 0");
            }
        }
        for i in &self.interactables {
            claim(i.id)?;
            if !(finite3(i.min) && finite3(i.max) && i.min.cmplt(i.max).all()) {
                return bad(i.id, "interactable box must be finite with min < max");
            }
        }
        for a in &self.attractors {
            claim(a.id)?;
            let finite = finite3(a.position)
                && a.range.is_finite()
                && a.strength.is_finite()
                && a.velocity_base_amount.is_finite()
                && a.attract_duration.is_finite();
            if !(finite && a.attract_duration > 0.0) {
                return bad(
                    a.id,
                    "attractor values must be finite with attract_duration > 0",
                );
            }
        }
        Ok(())
    }

    /// Index of the first checkpoint (in level order) whose volume contains `p`.
    #[must_use]
    pub fn checkpoint_index_at(&self, p: Vec3) -> Option<usize> {
        self.checkpoints.iter().position(|c| c.contains(p))
    }

    /// Every collision box with its surface, in a stable order: static
    /// boxes, grapple points, movers (at their current offset in `objects`,
    /// or at rest), crystals, flowers, interactables.
    #[must_use]
    pub fn collision_primitives(&self, objects: Option<&WorldObjects>) -> Vec<CollisionPrimitive> {
        let mut out = Vec::new();
        for b in &self.static_boxes {
            out.push(CollisionPrimitive {
                min: b.min,
                max: b.max,
                grapple_able: b.grapple_able,
                tag: b.tag,
                kind: match b.kind {
                    BoxKind::StaticMesh => PrimitiveKind::StaticMesh,
                    BoxKind::WorldGeometry => PrimitiveKind::WorldGeometry,
                },
                actor: None,
            });
        }
        for g in &self.grapple_points {
            let h = Vec3::splat(g.half_extent);
            out.push(CollisionPrimitive {
                min: g.position - h,
                max: g.position + h,
                grapple_able: true,
                tag: SurfaceTag::None,
                kind: PrimitiveKind::StaticMesh,
                actor: None,
            });
        }
        for (i, m) in self.movers.iter().enumerate() {
            let offset = objects
                .and_then(|o| o.mover_offsets.get(i))
                .copied()
                .unwrap_or(Vec3::ZERO);
            out.push(CollisionPrimitive {
                min: m.min + offset,
                max: m.max + offset,
                grapple_able: true,
                tag: SurfaceTag::None,
                kind: PrimitiveKind::Mover,
                actor: Some(m.id),
            });
        }
        let cube = |c: Vec3, h: f32| (c - Vec3::splat(h), c + Vec3::splat(h));
        for c in &self.crystals {
            let (min, max) = cube(c.center, c.half_extent);
            out.push(CollisionPrimitive {
                min,
                max,
                grapple_able: true,
                tag: SurfaceTag::None,
                kind: PrimitiveKind::RechargeCrystal,
                actor: Some(c.id),
            });
        }
        for f in &self.flowers {
            let (min, max) = cube(f.center, f.half_extent);
            out.push(CollisionPrimitive {
                min,
                max,
                grapple_able: true,
                tag: SurfaceTag::None,
                kind: PrimitiveKind::GlowFlower,
                actor: Some(f.id),
            });
        }
        for i in &self.interactables {
            out.push(CollisionPrimitive {
                min: i.min,
                max: i.max,
                grapple_able: true,
                tag: SurfaceTag::None,
                kind: PrimitiveKind::Interactable,
                actor: Some(i.id),
            });
        }
        out
    }

    /// Current location of the mover `id` (rest centre plus its offset in
    /// `objects`).
    #[must_use]
    pub fn mover_location(&self, objects: &WorldObjects, id: u32) -> Option<Vec3> {
        let i = self.movers.iter().position(|m| m.id == id)?;
        let m = self.movers.get(i)?;
        let offset = objects.mover_offsets.get(i).copied().unwrap_or(Vec3::ZERO);
        Some((m.min + m.max) * 0.5 + offset)
    }

    /// The axis-aligned boxes for collision: static boxes followed by one cube
    /// per grapple point (all grapple points are grapple-able), as
    /// `(min, max, grapple_able)`, in a stable order.
    #[must_use]
    pub fn collision_boxes(&self) -> Vec<(Vec3, Vec3, bool)> {
        let statics = self
            .static_boxes
            .iter()
            .map(|b| (b.min, b.max, b.grapple_able));
        let points = self.grapple_points.iter().map(|g| {
            let h = Vec3::splat(g.half_extent);
            (g.position - h, g.position + h, true)
        });
        statics.chain(points).collect()
    }
}

/// A small hand-made graybox level for the first gameplay slice.
///
/// **Not original content.** Dimensions are in UU and were chosen by us: a
/// start platform with low steps and a guard wall, a gap with one grapple
/// hook leading to a lower platform (checkpoint 1), a second gap under a
/// grapple-able beam and two hooks leading to a final platform (checkpoint
/// 2), and an off-path non-grapple-able pillar for "miss" feedback. North of
/// the start platform (off the main path) a playground holds one of each
/// grapple-reactive object: a recharge crystal, a glow flower, a moving
/// block on our own back-and-forth path, a `NotLandable` pad, a story-mode
/// interactable on the platform and an (inactive until activated)
/// attractor pad hovering over the start platform. Abilities:
/// [`LevelAbilities::graybox_test`] (everything on, 3 grapples) — a test
/// configuration.
#[must_use]
pub fn graybox_test_level() -> Level {
    let v = Vec3::new;
    Level {
        name: "graybox-test (hand-made, not original content)".into(),
        origin: LevelOrigin::HandMadeGraybox,
        static_boxes: vec![
            StaticBox::new(
                v(-600.0, -400.0, -100.0),
                v(600.0, 400.0, 0.0),
                false,
                "start platform",
            ),
            StaticBox::new(
                v(-600.0, -420.0, 0.0),
                v(600.0, -400.0, 200.0),
                false,
                "guard wall",
            ),
            StaticBox::new(v(0.0, 200.0, 0.0), v(100.0, 400.0, 12.0), false, "step 1"),
            StaticBox::new(v(100.0, 200.0, 0.0), v(200.0, 400.0, 24.0), false, "step 2"),
            StaticBox::new(v(200.0, 200.0, 0.0), v(600.0, 400.0, 36.0), false, "step 3"),
            StaticBox::new(
                v(1500.0, 700.0, -600.0),
                v(1600.0, 800.0, 1400.0),
                false,
                "pillar (not grapple-able)",
            ),
            StaticBox::new(
                v(2400.0, -600.0, -400.0),
                v(3600.0, 600.0, -300.0),
                false,
                "platform 2",
            ),
            StaticBox::new(
                v(4200.0, -300.0, 500.0),
                v(4800.0, 300.0, 560.0),
                true,
                "grapple beam",
            ),
            StaticBox::new(
                v(5400.0, -800.0, -300.0),
                v(7000.0, 800.0, -200.0),
                false,
                "final platform",
            ),
            StaticBox::new(
                v(900.0, 1000.0, -120.0),
                v(1100.0, 1200.0, -100.0),
                true,
                "NotLandable pad",
            )
            .with_tag(SurfaceTag::NotLandable),
        ],
        grapple_points: vec![
            GrapplePoint {
                position: v(1750.0, 0.0, 900.0),
                half_extent: 50.0,
            },
            GrapplePoint {
                position: v(4000.0, -350.0, 700.0),
                half_extent: 40.0,
            },
            GrapplePoint {
                position: v(5000.0, 350.0, 750.0),
                half_extent: 40.0,
            },
        ],
        player_start: SpawnPoint {
            feet: v(-400.0, 0.0, 0.0),
            yaw: 0.0,
        },
        checkpoints: vec![
            Checkpoint {
                id: 1,
                min: v(2400.0, -600.0, -300.0),
                max: v(3600.0, 600.0, 0.0),
                spawn: SpawnPoint {
                    feet: v(2600.0, 0.0, -300.0),
                    yaw: 0.0,
                },
            },
            Checkpoint {
                id: 2,
                min: v(5400.0, -800.0, -200.0),
                max: v(7000.0, 800.0, 100.0),
                spawn: SpawnPoint {
                    feet: v(5600.0, 0.0, -200.0),
                    yaw: 0.0,
                },
            },
        ],
        kill_z: -1500.0,
        crystals: vec![RechargeCrystal {
            id: 101,
            center: v(300.0, 1300.0, 350.0),
            half_extent: 30.0,
            recharge_delay: objects::CRYSTAL_DEFAULT_RECHARGE_DELAY,
            should_recharge: true,
            parent_crystal: false,
            linked_parent: None,
        }],
        flowers: vec![GlowFlower {
            id: 201,
            center: v(-300.0, 1300.0, 300.0),
            half_extent: 25.0,
        }],
        movers: vec![Mover {
            id: 301,
            min: v(-60.0, 2000.0, 540.0),
            max: v(60.0, 2120.0, 660.0),
            path: MoverPath::PingPong {
                offset: v(800.0, 0.0, 0.0),
                period: 4.0,
            },
            label: "moving block (our test path)".into(),
        }],
        interactables: vec![Interactable {
            id: 401,
            min: v(-580.0, 280.0, 0.0),
            max: v(-520.0, 340.0, 60.0),
            max_interact_times: objects::INTERACTABLE_DEFAULT_MAX_INTERACT_TIMES,
            label: "story interactable".into(),
        }],
        attractors: vec![Attractor::as_placed(501, v(0.0, 0.0, 150.0))],
        abilities: LevelAbilities::graybox_test(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn graybox_level_is_valid_and_labelled_as_hand_made() {
        let level = graybox_test_level();
        assert_eq!(level.validate(), Ok(()));
        assert_eq!(level.origin, LevelOrigin::HandMadeGraybox);
        assert!(level.name.contains("not original content"));
        assert!(level.static_boxes.iter().any(|b| b.grapple_able));
        assert!(level.static_boxes.iter().any(|b| !b.grapple_able));
        assert!(!level.grapple_points.is_empty());
        assert!(level.player_start.feet.z > level.kill_z);
        assert_eq!(
            level.collision_boxes().len(),
            level.static_boxes.len() + level.grapple_points.len()
        );
    }

    #[test]
    fn checkpoints_lookup() {
        let level = graybox_test_level();
        assert_eq!(
            level.checkpoint_index_at(Vec3::new(2600.0, 0.0, -250.0)),
            Some(0)
        );
        assert_eq!(
            level.checkpoint_index_at(Vec3::new(6000.0, 0.0, -150.0)),
            Some(1)
        );
        assert_eq!(level.checkpoint_index_at(Vec3::new(0.0, 0.0, 45.0)), None);
    }

    #[test]
    fn validation_catches_bad_data() {
        let mut l = graybox_test_level();
        l.static_boxes[0].max.x = l.static_boxes[0].min.x;
        assert!(matches!(l.validate(), Err(LevelError::Degenerate { .. })));

        let mut l = graybox_test_level();
        l.kill_z = f32::NAN;
        assert!(matches!(l.validate(), Err(LevelError::NonFinite { .. })));

        let mut l = graybox_test_level();
        l.grapple_points[0].half_extent = 0.0;
        assert_eq!(l.validate(), Err(LevelError::BadGrapplePoint { index: 0 }));

        let mut l = graybox_test_level();
        l.checkpoints[1].id = l.checkpoints[0].id;
        assert_eq!(l.validate(), Err(LevelError::DuplicateCheckpoint(1)));

        let mut l = graybox_test_level();
        l.player_start.feet.z = l.kill_z - 1.0;
        assert!(matches!(
            l.validate(),
            Err(LevelError::SpawnBelowKillZ { .. })
        ));
    }

    #[test]
    fn static_box_orders_corners() {
        let b = StaticBox::new(
            Vec3::new(1.0, 2.0, 3.0),
            Vec3::new(-1.0, 5.0, 0.0),
            true,
            "x",
        );
        assert_eq!(b.min, Vec3::new(-1.0, 2.0, 0.0));
        assert_eq!(b.max, Vec3::new(1.0, 5.0, 3.0));
    }

    #[test]
    fn serde_round_trip() {
        let level = graybox_test_level();
        let json = serde_json::to_string(&level).unwrap();
        let back: Level = serde_json::from_str(&json).unwrap();
        assert_eq!(back, level);
    }
}
