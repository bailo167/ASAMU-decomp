//! Deterministic, render-free player simulation: movement, grapple, camera.
//!
//! **No gameplay constant recovered from the original game is used here yet.**
//! Every parameter default in [`params`] is a documented PLACEHOLDER, and the
//! locomotion model in [`movement`] is our own placeholder model behind the
//! swappable [`movement::MovementModel`] trait. See
//! `docs/reverse-engineering/GAMEPLAY_LEADS.md` and `docs/PARITY.md`.
//!
//! - [`input`]: [`InputFrame`], the per-tick logical input.
//! - [`params`]: [`PlayerParams`] built from `asamu_core::Param` values with
//!   provenance; [`PlayerParams::provenance_report`].
//! - [`world`]: the [`CollisionWorld`] trait and the simple [`BoxWorld`].
//! - [`movement`]: locomotion models.
//! - [`grapple`]: grapple acquisition, attach/release, pull, rope constraint.
//! - [`sim`]: [`PlayerState`] and the pure [`step`] function (deterministic
//!   across platforms by construction; hostile `dt`/state/parameters are
//!   contained, see the module docs).
//! - [`trace`]: the JSON Lines parity trace format and [`trace::compare`].
//!
//! Units: UE3 axes (X forward, Y right, Z up), distances in Unreal units.

pub mod grapple;
pub mod input;
pub mod movement;
pub mod params;
pub mod sim;
pub mod trace;
pub mod world;

pub use grapple::{Aim, GrappleEvent, GrappleState, RopeOutcome};
pub use input::InputFrame;
pub use movement::{MovementModel, PlaceholderMovement};
pub use params::{
    CameraParams, GrappleParams, MovementParams, PlayerParams, ReleaseMode, RopeMode,
};
pub use sim::{MAX_STEP_DT, PlayerState, StepEvents, step, step_with};
pub use trace::{Trace, TraceDiff, TraceMeta, TraceSample, compare};
pub use world::{BoxWorld, CollisionShape, CollisionWorld, Hit};
