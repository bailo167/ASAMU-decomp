//! Shared helpers for integration tests.
#![allow(dead_code)]

use asamu_player::movement::place_on_floor;
use asamu_player::world::CONTACT_SKIN;
use asamu_player::{BoxWorld, InputFrame, PlayerParams, PlayerState, StepEvents, step};
use glam::Vec3;

/// Test tick length (60 Hz; a runtime choice, see `asamu_core::clock`).
pub const DT: f32 = 1.0 / 60.0;

/// A flat infinite floor at z = 0.
pub fn flat_world() -> BoxWorld {
    BoxWorld::new().with_ground(0.0, false)
}

/// Height of the collision centre when standing on a floor at `floor_z`.
pub fn standing_z(params: &PlayerParams, floor_z: f32) -> f32 {
    floor_z + params.movement.capsule_half_height.value + CONTACT_SKIN
}

/// A grounded state standing at `(x, y)` on whatever floor is below z = 10000.
pub fn standing(params: &PlayerParams, world: &BoxWorld, x: f32, y: f32, yaw: f32) -> PlayerState {
    let mut s = PlayerState::new(Vec3::new(x, y, 10_000.0), yaw);
    assert!(
        place_on_floor(&mut s, &params.movement, world, 20_000.0),
        "no floor below ({x}, {y})"
    );
    s
}

/// Steps once per input; returns the events of each tick.
pub fn run(
    state: &mut PlayerState,
    inputs: &[InputFrame],
    params: &PlayerParams,
    world: &BoxWorld,
) -> Vec<StepEvents> {
    inputs
        .iter()
        .map(|i| step(state, i, params, world, DT))
        .collect()
}

/// Forward input frame.
pub fn forward() -> InputFrame {
    InputFrame {
        move_forward: 1.0,
        ..InputFrame::default()
    }
}

/// Deterministic SplitMix64 PRNG (tests only; the simulation itself has no RNG).
pub struct SplitMix64(pub u64);

impl SplitMix64 {
    pub fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// Uniform in `[0, 1)`.
    pub fn unit(&mut self) -> f32 {
        (self.next_u64() >> 40) as f32 / (1u64 << 24) as f32
    }

    /// Uniform in `[lo, hi)`.
    pub fn range(&mut self, lo: f32, hi: f32) -> f32 {
        lo + (hi - lo) * self.unit()
    }

    /// `true` with probability `p`.
    pub fn chance(&mut self, p: f32) -> bool {
        self.unit() < p
    }
}

/// Yaw/pitch (radians, UE3 convention) that look from `from` towards `to`.
pub fn look_at(from: Vec3, to: Vec3) -> (f32, f32) {
    let d = to - from;
    let yaw = d.y.atan2(d.x);
    let pitch = d.z.atan2(d.truncate().length());
    (yaw, pitch)
}
