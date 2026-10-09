//! Deterministic, render-free player simulation: movement, grapple, camera.
//!
//! Two locomotion models sit behind the swappable [`movement::MovementModel`]
//! trait: our own [`movement::PlaceholderMovement`] (the default of
//! [`sim::step`]) and [`ue3_movement::Ue3PawnMovement`], a port of the
//! original's native UE3/UDK pawn walking/falling physics written from
//! `docs/reverse-engineering/NATIVE_PHYSICS.md`. Parameter defaults in
//! [`params`] are documented PLACEHOLDERS except `movement.world_gravity_z`
//! (recovered from the original's config). See `docs/PARITY.md`.
//!
//! - [`input`]: [`InputFrame`], the per-tick logical input.
//! - [`params`]: [`PlayerParams`] built from `asamu_core::Param` values with
//!   provenance; [`PlayerParams::provenance_report`].
//! - [`world`]: the [`CollisionWorld`] trait and the simple [`BoxWorld`].
//! - [`movement`]: the locomotion trait, the placeholder model and
//!   [`MovementModelKind`] (runtime model selection).
//! - [`ue3_movement`]: the native-physics port ([`Ue3PawnMovement`]).
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
pub mod ue3_movement;
pub mod world;

pub use grapple::{Aim, GrappleEvent, GrappleState, RopeOutcome};
pub use input::InputFrame;
pub use movement::{MovementModel, MovementModelKind, PlaceholderMovement};
pub use params::{
    CameraParams, GrappleParams, MovementParams, PlayerParams, ReleaseMode, RopeMode,
};
pub use sim::{MAX_STEP_DT, PlayerState, StepEvents, step, step_with};
pub use trace::{Trace, TraceDiff, TraceMeta, TraceSample, compare};
pub use ue3_movement::{PawnPhysicsState, Ue3PawnMovement};
pub use world::{BoxWorld, CollisionShape, CollisionWorld, HalfSpace, Hit, SlopeWorld};
