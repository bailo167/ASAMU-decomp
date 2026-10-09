//! Shared math, units, coordinate conventions and configuration types.
//!
//! Nothing in this crate knows about Steam, file paths or the original game
//! files. It is the vocabulary shared by the importer and the runtime.
//!
//! Modules:
//!
//! - [`units`]: Unreal units (UU), the unit the simulation runs in, and the
//!   *presentation-only* UU ↔ metre convention.
//! - [`coords`]: UE3 (left-handed, X forward, Y right, Z up) ↔ Bevy
//!   (right-handed, Y up, −Z forward) conversion.
//! - [`rotator`]: UE3 rotator units (65536 per turn) ↔ radians.
//! - [`det_math`]: deterministic `sin`/`cos` (pure Rust, IEEE basic ops only)
//!   so the simulation is bit-identical across platforms.
//! - [`clock`]: a fixed-step simulation clock. The original game's tick model
//!   is **UNKNOWN**; the tick rate is a runtime choice.
//! - [`provenance`]: [`Provenance`] and [`Param`], used to attach a source to
//!   every gameplay number.
//! - [`exact_f32`]: serde helpers that make `f32` values round-trip
//!   bit-exactly through JSON.

pub mod clock;
pub mod coords;
pub mod det_math;
pub mod exact_f32;
pub mod provenance;
pub mod rotator;
pub mod units;

pub use clock::{ClockError, DEFAULT_TICK_RATE_HZ, FixedClock};
pub use coords::WorldScale;
pub use glam;
pub use provenance::{Param, Provenance};
pub use rotator::Rotator;
pub use units::Uu;
