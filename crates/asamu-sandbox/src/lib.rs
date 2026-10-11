//! Sandbox mode for the ASAMU recreation (render-free model; no Bevy).
//!
//! The Sandbox is **ours**: a place to experiment with the movement and
//! game systems (live parameter tuning, cheats, time control, save states,
//! hand-made lab arenas). Nothing in this crate is original game content and
//! nothing a Sandbox session produces is evidence of parity with the
//! original. Classic behaviour stays the faithful target and is not changed
//! by this crate:
//!
//! - it sits **above** the simulation crates (`asamu-core` ← `asamu-player`
//!   ← `asamu-world` ← `asamu-kismet` ← `asamu-game` ← this crate) and no
//!   Classic crate or parity tool depends on it;
//! - it adds no dependency feature to the build graph;
//! - the Classic parameter set ([`asamu_player::PlayerParams::asamu_original`])
//!   is never edited: a session runs a *copy* with overrides applied
//!   ([`overlay::Overlay`]), every override carries placeholder provenance
//!   with the [`overlay::OVERRIDE_NOTE_PREFIX`] note, and the only door into
//!   a running [`asamu_game::Game`] is `Game::set_params`
//!   (`asamu-game/src/tooling.rs`), which Classic code never calls;
//! - a session never calls `Game::tick` in a different order: the host keeps
//!   ticking the game exactly as Classic does, and the session only acts
//!   between ticks ([`session::Session::before_tick`],
//!   [`session::Session::execute`]);
//! - recordings made in a session are their own container
//!   ([`recording::SandboxRecording`]) that the parity trace reader rejects.
//!
//! # Modules
//!
//! | Module | Contents |
//! |---|---|
//! | [`keys`] | The tunable-parameter catalogue: one key per row of the Classic provenance report |
//! | [`overlay`] | Overrides on the Classic set, and labels for a parameter set |
//! | [`relatch`] | Bringing a running pawn's latched run-time values up to a new set |
//! | [`rules`] | Sticky cheats that are not parameters (grapple budget, boots) |
//! | [`profile`] | The versioned profile file and the Sandbox's own directories |
//! | [`command`] | Every mutation a session can perform, as serialisable data |
//! | [`session`] | The session: profile, effective parameters, commands, hooks |
//! | [`time`] | The time-control model (freeze, single step, speed) |
//! | [`snapshot`] | In-memory save states and the keyframe rewind ring |
//! | [`telemetry`] | Trail, speed and height history, jump and swing statistics |
//! | [`predict`] | A predicted arc on a copy of the player |
//! | [`recording`] | The Sandbox recording container |
//! | [`arena`] | Built-in hand-made lab arenas |
//! | [`inspect`] | Read-only views of a session for a user interface |
//!
//! The app-side plugin lives in `apps/asamu/src/sandbox.rs` behind that
//! crate's `sandbox` Cargo feature. The feature exists on the app only:
//! deliberately, nothing in this crate or anywhere else under `crates/` or
//! `tools/` is compiled conditionally on it (a boundary test scans for the
//! attribute, which is why it is not spelled out here).

pub mod arena;
pub mod command;
pub mod inspect;
pub mod keys;
pub mod overlay;
pub mod predict;
pub mod profile;
pub mod recording;
pub mod relatch;
pub mod rules;
pub mod session;
pub mod snapshot;
pub mod telemetry;
pub mod time;
