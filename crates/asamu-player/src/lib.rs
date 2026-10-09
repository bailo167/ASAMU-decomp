//! Deterministic, render-free player simulation: movement, grapple, camera.
//!
//! Two locomotion models sit behind the swappable [`movement::MovementModel`]
//! trait: our own [`movement::PlaceholderMovement`] (used by the raw
//! [`sim::step`]) and [`ue3_movement::Ue3PawnMovement`], a port of the
//! original's native UE3/UDK pawn walking/falling physics written from
//! `docs/reverse-engineering/NATIVE_PHYSICS.md`. On top of it,
//! [`pawn`] reimplements the ASAMU pawn/controller script layer (jump and
//! its release damping, sprint, story mode and zoom, landing, eye height and
//! bob, power jump) from `docs/reverse-engineering/ABILITIES.md`.
//! [`PlayerParams::asamu_original`] holds the original's values with
//! provenance and enables the script layer with the original grapple gun
//! ([`grapple_gun`], `docs/reverse-engineering/GRAPPLE.md`) and rocket boots
//! ([`rocket_boots`]); [`PlayerParams::placeholder`] (the [`Default`]) keeps
//! the graybox placeholders, the raw physics and the placeholder rope
//! grapple ([`grapple`]) for debugging. See `docs/PARITY.md`.
//!
//! - [`input`]: [`InputFrame`], the per-tick logical input.
//! - [`params`]: [`PlayerParams`] built from `asamu_core::Param` values with
//!   provenance; [`PlayerParams::provenance_report`].
//! - [`world`]: the [`CollisionWorld`] trait and the simple [`BoxWorld`].
//! - [`movement`]: the locomotion trait, the placeholder model and
//!   [`MovementModelKind`] (runtime model selection).
//! - [`ue3_movement`]: the native-physics port ([`Ue3PawnMovement`]).
//! - [`pawn`]: the ASAMU script layer ([`PawnScript`]).
//! - [`grapple_gun`]: the original grapple gun (targeting, acceptance,
//!   attach, pull, release, budget).
//! - [`rocket_boots`]: the original rocket boots.
//! - [`events`]: Kismet-visible simulation events ([`events::SimEvent`]).
//! - [`grapple`]: the placeholder rope grapple of the debug configuration.
//! - [`sim`]: [`PlayerState`] and the pure [`step`] function (deterministic
//!   across platforms by construction; hostile `dt`/state/parameters are
//!   contained, see the module docs).
//! - [`trace`]: the JSON Lines parity trace format and [`trace::compare`].
//!
//! Units: UE3 axes (X forward, Y right, Z up), distances in Unreal units.

pub mod events;
pub mod grapple;
pub mod grapple_gun;
pub mod input;
pub mod movement;
pub mod params;
pub mod pawn;
pub mod rocket_boots;
pub mod sim;
pub mod trace;
pub mod ue3_movement;
pub mod world;

pub use events::{EventLog, SimEvent};
pub use grapple::{Aim, GrappleEvent, GrappleState, RopeOutcome};
pub use grapple_gun::{FireOutcome, GunEvents, GunState, ReleaseReason};
pub use input::InputFrame;
pub use movement::{
    ClassDefaults, Landing, MovementModel, MovementModelKind, PawnHooks, PlaceholderMovement,
};
pub use params::{
    CameraParams, GrappleGunParams, GrappleParams, MovementParams, PawnParams, PlayerParams,
    ReleaseMode, RocketBootsParams, RopeMode,
};
pub use pawn::{
    LandCue, LandingHandler, LandingOutcome, PawnScript, PawnStateName, PowerJumpEvent,
    PowerJumpStateName,
};
pub use rocket_boots::{BootsEvent, BootsStateName, RocketBoots};
pub use sim::{
    MAX_STEP_DT, PendingStep, PlayerState, StepEvents, begin_step, finish_step, step, step_with,
};
pub use trace::{Trace, TraceDiff, TraceMeta, TraceSample, compare};
pub use ue3_movement::{PawnPhysicsState, Ue3PawnMovement};
pub use world::{
    ActorClass, ActorTag, BoxWorld, CollisionShape, CollisionWorld, HalfSpace, Hit, SlopeWorld,
    Surface,
};
