//! Runtime world representation: levels, checkpoints, grapple points.
//!
//! This crate holds plain data (no Bevy, no UE3 parsing). Levels converted
//! from the user's original install will eventually be produced by the
//! importer into this form; today the only level is
//! [`graybox_test_level`], which is **hand-made test geometry, not original
//! content**.
//!
//! Conventions: UE3 axes (X forward, Y right, Z up), distances in Unreal
//! units (UU), yaw in radians (UE3 convention, + turns right).

use glam::Vec3;
use serde::{Deserialize, Serialize};
use thiserror::Error;

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
    /// Whether the grapple can attach to it.
    pub grapple_able: bool,
    /// Human-readable label (debugging / rendering).
    pub label: String,
}

impl StaticBox {
    /// A box from two corners in any order.
    #[must_use]
    pub fn new(a: Vec3, b: Vec3, grapple_able: bool, label: impl Into<String>) -> Self {
        Self {
            min: a.min(b),
            max: a.max(b),
            grapple_able,
            label: label.into(),
        }
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
        let mut ids: Vec<u32> = Vec::with_capacity(self.checkpoints.len());
        for c in &self.checkpoints {
            let what = format!("checkpoint {}", c.id);
            if ids.contains(&c.id) {
                return Err(LevelError::DuplicateCheckpoint(c.id));
            }
            ids.push(c.id);
            if !(finite3(c.min) && finite3(c.max)) {
                return Err(LevelError::NonFinite { what });
            }
            if !c.min.cmplt(c.max).all() {
                return Err(LevelError::Degenerate { what });
            }
            check_spawn(&c.spawn, what)?;
        }
        Ok(())
    }

    /// Index of the first checkpoint (in level order) whose volume contains `p`.
    #[must_use]
    pub fn checkpoint_index_at(&self, p: Vec3) -> Option<usize> {
        self.checkpoints.iter().position(|c| c.contains(p))
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
/// **Not original content.** Dimensions are in UU and were chosen by us so the
/// placeholder movement/grapple can cross it: a start platform with low steps
/// and a guard wall, a gap with one grapple hook leading to a lower platform
/// (checkpoint 1), a second gap under a grapple-able beam and two hooks leading
/// to a final platform (checkpoint 2), and an off-path non-grapple-able
/// pillar for "miss" feedback.
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
