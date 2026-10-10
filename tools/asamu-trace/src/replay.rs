//! Replays a trace's inputs through our simulation and records our trace.
//!
//! The replay builds an [`asamu_game::Game`] on the graybox level or on a
//! level converted by `asamu-import` (optionally with the map's Kismet,
//! [`asamu_game::load_level_with_kismet`]), puts the pawn in the trace's
//! state at the start tick (position, velocity, yaw, pitch, walking or
//! falling from the sample; the recorded script state from the `state:`
//! note, see "Start state" below) and then feeds every later sample's
//! input, one tick each. It stops at the first tick gap, and (for an
//! original recording) before the first event that inputs cannot reproduce
//! ("Validity" below). The result is a runtime trace aligned tick for tick
//! with the input trace, ready for [`crate::compare`].
//!
//! # Start state
//!
//! What the original's script had latched at the start tick decides its
//! next move, so the replay sets it from the recording ([`crate::state`]),
//! and only from the recording:
//!
//! | Our state | From |
//! |---|---|
//! | `GroundSpeed`, `AirControl` (0.3 before the first normal landing, the landed value after), `JumpZ`, `AirSpeed` | the recorded values |
//! | sprint applied | the pawn's `bSprinting` |
//! | story mode | `GroundSpeed` when it is the story speed (on) or the walking or sprint speed (off): the pawn's own script writes no other value (ABILITIES.md A-WK-3). Any other value (a console `SetSpeed`, as in AG-Workshop) shows nothing: the replay then leaves it as it is and says so; [`ReplayOptions::story_mode`] states it |
//! | grapple capacity, used count, fire latch | the gun's `iMaxGrapples`, `iTimesGrappled`, `bCanGrapple` |
//! | rocket boots enabled | the boots' `bEnabled` |
//! | eye height | the pawn's `EyeHeight` (when the `state:` note has it) |
//! | the pawn has a base | a walking start whose recorded `Base` is not null: the first floor check then keeps the pawn where it stands if its floor is inside the native hover band, as the original's does for a based pawn (NATIVE_PHYSICS.md 3.3); without a base it re-seats the pawn in any case |
//! | button levels of the tick before (for press and release edges) | the start sample's own input |
//! | FOV | the start sample's FOV |
//!
//! Not recorded, so left as our simulation starts them: "sprint after
//! landing", the pawn's and the power jump's state code (jump-release
//! damping, a charging power jump, a running zoom), zoom availability, the
//! gun's timers and attachment, the boots' boost, the walk bob, the floor
//! normal. All of these are at rest when the pawn stands still with no
//! button held, so a replay of an original recording starts only at such a
//! tick ([`crate::segments`], [`StartPolicy`]): a start in the air, while
//! moving or while the grapple is attached is refused (the error names the
//! standing-still ticks before and after it), moved forward to the next
//! standing-still tick, or forced. A trace converted before the `state:`
//! note existed has an `init:` note for its first sample only.
//!
//! ## What the start notes say (PARITY_FINDINGS.md N2)
//!
//! The recorded position is the original's, on the original's collision.
//! Ours differs (other shapes, other rest heights), and a replay used to
//! say nothing when that already decides the first tick. Whenever the start
//! state is written, the notes now carry:
//!
//! - `start: our floor is … uu below the pawn …`: the distance our own
//!   floor check will measure, what it hits (the actor's name on a
//!   converted level) and whether that is the recorded base actor (by name;
//!   world geometry against the original's `WorldInfo`);
//! - `warning: the start position overlaps our collision …`: with what, and
//!   how far above the recorded height a pawn lowered onto that place comes
//!   to rest. Our pawn usually cannot move from such a start;
//! - `start: in the first tick …`: how far our pawn and the original moved
//!   vertically in the first tick while walking, as a warning when the two
//!   differ by more than the width of the native hover band
//!   (`MAX_FLOOR_DIST − MIN_FLOOR_DIST`): the two stand on different floor
//!   heights (our floor check re-seated the pawn on our floor, or left it
//!   inside our geometry), and every vertical number carries that offset;
//! - `warning: our pawn did not move …` when it never left an overlapping
//!   start while the original travelled.
//!
//! # Validity ([`EventPolicy`], PARITY_FINDINGS.md N3 and N6)
//!
//! A recording contains events that no replay of the inputs reproduces
//! ([`crate::segments`]): respawn teleports and level-script state changes
//! (story mode, a console speed, the grapple capacity, the rocket boots).
//! For an original recording the replay, by default, **stops before the
//! first such tick** and says so (`validity:` note,
//! [`ReplayResult::stopped_at_event`]): the trace it returns is the valid
//! part. [`EventPolicy::Inject`] instead takes the recorded change at that
//! tick and runs on, under an `injected:` note: a level-script change is
//! written into our state before our tick (the record after the frame shows
//! it, so the frame ran with it; whether the script acted before or after
//! the pawn's physics of that frame is not recorded: TENTATIVE. One
//! instance agrees with "before": the original leaves story mode in DC1
//! seg3 at tick 5019 and walks 299.0 uu/s at the end of that frame, ours
//! with the change written before the tick 299.1), a teleport replaces our
//! position, velocity, view and physics mode after our tick.
//! [`EventPolicy::Ignore`] runs through as replays did before, with a
//! warning. Traces of our own runtime are not checked (our simulation
//! reproduces its own respawns).
//!
//! # One-step replays ([`ReplayOptions::one_step`], N8)
//!
//! A free-running replay measures accumulated drift: one early difference
//! (a pawn standing 1 uu lower) hides everything after it. A one-step
//! replay restarts **every** tick from the input trace's previous sample,
//! so sample `k` of our trace is one tick of our rules applied to the
//! original's state `k − 1`.
//!
//! Resynchronised before each tick, from sample `k − 1` and the `state:`
//! note at `k − 1`: position, velocity, yaw and pitch; the physics mode
//! (walking, falling, or attached with the recorded anchor: an attachment
//! we do not have is created on world geometry, one the original does not
//! have is released); `GroundSpeed` with the story mode it shows,
//! `AirControl`, `JumpZ`, `AirSpeed`, the sprint flag; the gun's used
//! count, capacity, fire latch and released flag; rocket boots enabled; the
//! eye height.
//!
//! **Not** resynchronised, because no record shows them (they continue from
//! our own previous tick): the pawn's and the power jump's state code and
//! timers (jump-release damping, a charging power jump, a running zoom),
//! "sprint after landing", the move-input lock, the gun's weapon state and
//! timers, the boots' boost, the walk bob, the FOV (the recorded one is the
//! cached view FOV), the floor normal and the "has a base" flag while the
//! physics mode agrees (set from the recorded base when the mode has to be
//! changed), the level's objects and Kismet. Button levels are not touched
//! either: both sides get the same inputs. A tick with a teleport or a
//! level-script state change is left out (its sample is missing from our
//! trace; the next tick starts from the recording after it) unless
//! [`EventPolicy::Ignore`] is given.
//!
//! When nothing differs, the resynchronisation writes back what is already
//! there: a one-step replay of a recording made by our own simulation is
//! that recording, bit for bit (tested).
//!
//! **A tick that starts inside our collision measures something else.** The
//! recording's positions are the original's, on the original's collision.
//! Where ours has geometry the original does not have there (a prop that
//! does not block the original's pawn, a hull of another shape, a volume
//! that is switched off), the resynchronised pawn starts the tick stuck in
//! or pushed out of it. That is a collision difference, not one tick of our
//! movement rules, so every such tick is counted and listed (a `one-step:
//! warning:` note with the count and the first ticks,
//! [`ReplayResult::resync_overlaps`] with all of them): read the one-step
//! numbers of the other ticks apart from these. The test is the same as for
//! the start position (the pawn's shape at the recorded position against
//! our collision as it was when the replay started: on the per-sample path,
//! level objects that moved since are not followed). Measured on the
//! recordings of 2026-10-10 (CONFIRMED, `tests/findings_comparisons.rs`):
//! 185 of the 3,000 ticks of the Workshop walk start that way (from tick
//! 1798; our pawn stands still in some of them and reports falling), and 5
//! of 150 in the flight through a blocking volume that is switched off in
//! the original (PARITY_FINDINGS.md P5), where the largest one-step
//! horizontal error is 18.2 uu with them and 6.1 uu without.
//!
//! The overlap test does **not** separate every collision difference: a
//! walking tick that starts above or below our floor (the original stands
//! on its own floor) is moved onto ours by our floor check, up to the
//! check's reach in one tick, and a move may meet a step of ours where the
//! original has a ramp. The largest one-step errors of the Workshop walk
//! (11.7 uu and 678 uu/s horizontally, 22.3 uu vertically, on the stairs)
//! are of that kind and are not overlap ticks. Until the collision agrees,
//! one-step numbers of walking ticks are numbers about the collision too.
//!
//! # What else the notes compare (N7, N10)
//!
//! - `attaches:` the grapple's attaches of both sides, counted the same way:
//!   the attached flag rose or the used-grapple counter rose (an attach
//!   released inside its frame shows in the counter only).
//! - `state check:` our `GroundSpeed`, `AirControl`, sprint flag, used
//!   grapples, capacity and eye height after every tick against the
//!   recording's (canonical samples do not carry them).
//!
//! # Time step ([`Stepping`])
//!
//! - **Fixed** — a trace with a fixed tick rate (`tick_rate` in its meta: a
//!   benchmark-mode recording, or one of our own runtime traces) is replayed
//!   with one fixed tick per sample at that rate through
//!   `Game::tick` / `LevelScript::tick`. [`ReplayOptions::tick_rate`]
//!   overrides the rate for any trace.
//! - **Per sample** — a trace without a fixed rate (`tick_rate: null`: the
//!   original without benchmark mode, every frame with its own
//!   `DeltaSeconds`) is replayed with each sample's own frame length,
//!   `time[k] − time[k−1]` ([`crate::timestep`]: for converted recordings
//!   that is the recorded `f32` bit for bit).
//!   [`ReplayOptions::variable_dt`] forces this for a fixed-rate trace too.
//!   `Game::tick` has no `dt` argument, so these replays run on
//!   [`crate::stepper::VariableStepper`]: all of `Game::tick` on the graybox,
//!   but on a converted level only the player and the level objects (no
//!   touch volumes, checkpoints, deaths, falling rocks, NPCs or Kismet — the
//!   replay's notes say so, and `kismet: true` is refused). Our samples
//!   carry the input samples' times. A frame longer than
//!   `asamu_player::MAX_STEP_DT` is simulated as that bound and a frame
//!   length that is not positive as a tick in which no time passes; both are
//!   counted in the notes and in [`VariableSteps`].
//!
//! If the game's own spawn state already equals sample 0 (a runtime trace
//! that started at the spawn point), the spawn state is kept unchanged, so
//! replaying a runtime recording reproduces it exactly.
//!
//! Limits: a forced start while the grapple is attached does not recreate
//! the attachment (a one-step replay does). With `--kismet` the map's Kismet
//! always starts from level start, also for a segment replay
//! (`start_tick`), and its level-start actions may override the start
//! state; the notes say so.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use asamu_core::DEFAULT_TICK_RATE_HZ;
use asamu_core::glam::Vec3;
use asamu_core::rotator::wrap_radians;
use asamu_game::{Game, GameWorld, LevelScript, load_level_with_kismet};
use asamu_player::grapple_gun::{Attachment, ReleaseReason};
use asamu_player::pawn::{PawnStateName, SprintState};
use asamu_player::trace::{TraceGrappleState, TraceSample, TraceSource};
use asamu_player::ue3_movement::{MAX_FLOOR_DIST, MIN_FLOOR_DIST, STEP_FUDGE, TARGET_FLOOR_DIST};
use asamu_player::world::{ActorClass, CollisionShape, CollisionWorld, Surface};
use asamu_player::{
    InputFrame, MAX_STEP_DT, MovementModelKind, PawnPhysicsState, PlayerParams, PlayerState, Trace,
    grapple_gun, pawn, rocket_boots,
};
use serde::Deserialize;

use crate::compare::ONE_STEP_NOTE_PREFIX;
use crate::convert::InitState;
use crate::segments::{
    EventKind, MAX_LISTED, is_clean_start, next_clean_start, not_clean_reasons,
    previous_clean_start, recorded_events, tick_displacement,
};
use crate::state::{StateTimeline, TickState, TimelineCursor, story_mode_shown};
use crate::stepper::VariableStepper;
use crate::timestep::{FrameLength, StepStats, finite};

/// Which level the replay runs on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReplayLevel {
    /// The hand-made graybox level (`asamu_world::graybox_test_level`).
    Graybox,
    /// A level converted by `asamu-import`.
    Converted {
        /// The converted output directory.
        dir: PathBuf,
        /// Map name (e.g. `AG-Workshop`).
        map: String,
        /// Run the map's Kismet ([`load_level_with_kismet`]); fixed 60 Hz
        /// replays only.
        kismet: bool,
    },
}

/// What a replay of an original recording does when its start tick is not
/// a clean start ([`crate::segments`]). Traces of our own runtime start
/// anywhere.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum StartPolicy {
    /// An error that names the standing-still ticks before and after.
    #[default]
    Refuse,
    /// Start at the next clean start instead.
    Snap,
    /// Start there all the same (the notes carry a warning).
    Force,
}

/// What a replay of an original recording does at a tick with an event the
/// inputs cannot reproduce: a teleport or a level-script state change
/// ([`EventKind::ends_validity`]; see the module docs, "Validity").
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum EventPolicy {
    /// The replay ends before the tick (a one-step replay leaves the tick
    /// out and goes on).
    #[default]
    Stop,
    /// The recorded change is taken over at the tick, under an `injected:`
    /// note, and the replay goes on (a one-step replay leaves the tick out:
    /// it takes everything from the recording anyway).
    Inject,
    /// The tick is simulated like any other; the notes carry a warning.
    Ignore,
}

/// Replay options.
#[derive(Clone, Debug, PartialEq)]
pub struct ReplayOptions {
    /// Level.
    pub level: ReplayLevel,
    /// Fixed tick rate, Hz: overrides whatever the trace says (also for a
    /// trace without a fixed rate, which is then replayed with ticks of
    /// equal length instead of its own frame lengths).
    pub tick_rate: Option<f64>,
    /// Step with each sample's own frame length even when the trace has a
    /// fixed tick rate ([`Stepping::PerSample`]). Excludes `tick_rate`.
    pub variable_dt: bool,
    /// Apply the recorded start state (the trace's `state:` or `init:`
    /// note, the start sample's button levels and FOV). Off, a one-step
    /// replay does not write the recorded script state on later ticks
    /// either (position, velocity, view and physics mode only).
    pub use_init: bool,
    /// Story mode at the start, when the recording does not show it or is
    /// to be overruled. (A one-step replay takes the story mode from the
    /// recorded `GroundSpeed` on every tick where that shows it.)
    pub story_mode: Option<bool>,
    /// What to do with a start tick that is not a clean start.
    pub start: StartPolicy,
    /// Grapple capacity override (`SetMaxGrapples`).
    pub max_grapples: Option<i32>,
    /// Use the placeholder parameters and model (debugging).
    pub placeholder: bool,
    /// Start from the sample with this tick instead of the first one
    /// (segment replays: separates local error from accumulated drift).
    pub start_tick: Option<u64>,
    /// Replay at most this many ticks.
    pub max_ticks: Option<u64>,
    /// Restart every tick from the input trace's previous sample (see the
    /// module docs, "One-step replays").
    pub one_step: bool,
    /// What to do at a teleport or a level-script state change of an
    /// original recording.
    pub events: EventPolicy,
}

impl Default for ReplayOptions {
    fn default() -> Self {
        Self {
            level: ReplayLevel::Graybox,
            tick_rate: None,
            variable_dt: false,
            use_init: true,
            story_mode: None,
            start: StartPolicy::Refuse,
            max_grapples: None,
            placeholder: false,
            start_tick: None,
            max_ticks: None,
            one_step: false,
            events: EventPolicy::Stop,
        }
    }
}

/// How a replay advanced time.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Stepping {
    /// One fixed tick per sample at this rate (Hz), through `Game::tick`.
    Fixed(f64),
    /// Each sample's own frame length, through
    /// [`crate::stepper::VariableStepper`].
    PerSample,
}

impl Stepping {
    /// The stepping [`replay`] uses for `trace` under `opts`: the
    /// `tick_rate` override, else per-sample lengths when asked for, else
    /// the trace's fixed rate, else (no fixed rate) per-sample lengths.
    ///
    /// # Errors
    /// Both `tick_rate` and `variable_dt` are set, or the rate is invalid.
    pub fn choose(trace: &Trace, opts: &ReplayOptions) -> Result<Self> {
        let stepping = match (opts.tick_rate, opts.variable_dt, trace.meta.tick_rate) {
            (Some(_), true, _) => {
                bail!("a fixed tick rate and per-sample frame lengths exclude each other")
            }
            (Some(r), false, _) => Self::Fixed(r),
            (None, true, _) | (None, false, None) => Self::PerSample,
            (None, false, Some(r)) => Self::Fixed(f64::from(r)),
        };
        if let Self::Fixed(rate) = stepping
            && !(rate.is_finite() && rate > 0.0)
        {
            bail!("invalid tick rate {rate}");
        }
        Ok(stepping)
    }
}

/// Frame lengths of a per-sample replay ([`Stepping::PerSample`]).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct VariableSteps {
    /// The frame lengths of the replayed ticks, as the input trace has them
    /// (seconds; before clamping).
    pub lengths: StepStats,
    /// Ticks longer than `MAX_STEP_DT`, simulated as `MAX_STEP_DT`.
    pub clamped: usize,
    /// The first such tick.
    pub first_clamped: Option<u64>,
    /// Ticks whose frame length is not positive: no time passed in them.
    pub no_op: usize,
    /// The first such tick.
    pub first_no_op: Option<u64>,
}

/// What a replay did at an event the inputs cannot reproduce.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EventAction {
    /// The replay ended before the tick.
    Stopped,
    /// The recorded change was taken over.
    Injected,
    /// The tick was not simulated (one-step replays).
    LeftOut,
    /// The tick was simulated like any other.
    Ignored,
}

/// An event of the input trace inside the replayed ticks that inputs cannot
/// reproduce, and what the replay did there.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CrossedEvent {
    /// The tick.
    pub tick: u64,
    /// What happened in the recording.
    pub kind: EventKind,
    /// What the replay did.
    pub action: EventAction,
}

/// The grapple's attaches inside the replayed ticks, counted the same way
/// on both sides: the attached flag rose, or the gun's used-grapple counter
/// rose (an attach that is released inside its frame).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Attaches {
    /// Ticks of the input trace's attaches (the counter is read from its
    /// `state:` note; without one, attached samples only).
    pub original: Vec<u64>,
    /// Ticks of our attaches.
    pub ours: Vec<u64>,
}

/// Result of [`replay`].
#[derive(Clone, Debug, PartialEq)]
pub struct ReplayResult {
    /// Our trace, with the input trace's tick numbers.
    pub trace: Trace,
    /// The tick the replay started from (after [`StartPolicy::Snap`]).
    pub start_tick: u64,
    /// The input trace's tick after which a gap stopped the replay.
    pub stopped_at_gap: Option<u64>,
    /// Respawns during the replay.
    pub respawns: u32,
    /// How time advanced.
    pub stepping: Stepping,
    /// Frame lengths of a per-sample replay (`None` for a fixed one).
    pub variable: Option<VariableSteps>,
    /// Every tick restarted from the input trace's previous sample.
    pub one_step: bool,
    /// The tick of the input trace before which the replay ended because
    /// inputs cannot reproduce what happened in it ([`EventPolicy::Stop`]).
    pub stopped_at_event: Option<u64>,
    /// The events of that kind inside the replayed ticks.
    pub events: Vec<CrossedEvent>,
    /// The grapple's attaches of both sides (original recordings only).
    pub attaches: Option<Attaches>,
    /// The pawn's shape at the start position overlaps our collision.
    pub start_overlaps: bool,
    /// One-step replays: the ticks that started inside our collision (the
    /// input trace's previous sample, which the tick was restarted from,
    /// overlaps it; see the module docs). Their errors measure the
    /// collision difference, not one tick of our movement rules. Empty for
    /// a free-running replay.
    pub resync_overlaps: Vec<u64>,
}

fn params(placeholder: bool) -> PlayerParams {
    if placeholder {
        PlayerParams::placeholder()
    } else {
        PlayerParams::asamu_original()
    }
}

fn build(opts: &ReplayOptions, rate: f64) -> Result<(Game, Option<LevelScript>, String)> {
    let p = params(opts.placeholder);
    let model = if opts.placeholder {
        MovementModelKind::Placeholder
    } else {
        MovementModelKind::Ue3Pawn
    };
    Ok(match &opts.level {
        ReplayLevel::Graybox => {
            let level = Game::graybox()?.level().clone();
            let g = Game::new(level, p, rate)?.with_movement_model(model);
            (g, None, "graybox".to_owned())
        }
        ReplayLevel::Converted { dir, map, kismet } => {
            if *kismet {
                if (rate - DEFAULT_TICK_RATE_HZ).abs() > 1e-9 {
                    bail!(
                        "--kismet replays run at {DEFAULT_TICK_RATE_HZ} Hz only; the trace needs {rate} Hz"
                    );
                }
                if opts.placeholder {
                    bail!("--kismet needs the original parameters (not --placeholder)");
                }
                let (g, script) = load_level_with_kismet(dir, map)
                    .with_context(|| format!("loading {map} with Kismet from {}", dir.display()))?;
                let desc = if script.is_some() {
                    format!("converted {map} with Kismet")
                } else {
                    format!("converted {map} (no Kismet export: replayed without Kismet)")
                };
                (g, script, desc)
            } else {
                let g = Game::load_level(dir, map)
                    .with_context(|| format!("loading {map} from {}", dir.display()))?;
                let same = (rate - DEFAULT_TICK_RATE_HZ).abs() <= 1e-9 && !opts.placeholder;
                let g = if same {
                    g
                } else {
                    let m = g
                        .scene_map()
                        .cloned()
                        .context("converted game without a map")?;
                    Game::from_loaded_map(m, p, rate)?.with_movement_model(model)
                };
                (g, None, format!("converted {map}"))
            }
        }
    })
}

/// The frame lengths of `samples[1..]` up to the first tick gap.
fn lengths_until_gap(samples: &[TraceSample]) -> Vec<f64> {
    samples
        .windows(2)
        .map_while(|w| match w {
            [a, b] if a.tick.checked_add(1) == Some(b.tick) => Some(b.time - a.time),
            _ => None,
        })
        .collect()
}

/// A yaw already in [−π, π) is taken as it is (wrapping it again can move it
/// by one bit).
fn in_range_yaw(yaw: f32) -> f32 {
    if (-core::f32::consts::PI..core::f32::consts::PI).contains(&yaw) {
        yaw
    } else {
        wrap_radians(yaw)
    }
}

/// `items` joined with `, `, at most [`MAX_LISTED`] of them, then how many
/// more there are.
fn bounded(items: &[String]) -> String {
    let shown = items.len().min(MAX_LISTED);
    let mut text = items.get(..shown).unwrap_or(items).join(", ");
    if items.len() > shown {
        text.push_str(&format!(" and {} more", items.len() - shown));
    }
    text
}

/// Names of a converted level's actors by actor id (`level << 16 | slot`),
/// read from the level's scene files when first asked for.
struct ActorNames {
    /// The converted data directory and the names of the loaded levels (by
    /// level index); `None` on a hand-made level.
    source: Option<(PathBuf, Vec<String>)>,
    /// Slot → name per level index (`None`: the file could not be read).
    levels: BTreeMap<usize, Option<BTreeMap<usize, String>>>,
}

/// The part of a scene file this module reads.
#[derive(Deserialize)]
struct SceneNames {
    #[serde(default)]
    actors: Vec<SceneActorName>,
}

#[derive(Deserialize)]
struct SceneActorName {
    slot: usize,
    name: String,
}

impl ActorNames {
    fn new(level: &ReplayLevel, game: &Game) -> Self {
        let source = match (level, game.scene_map()) {
            (ReplayLevel::Converted { dir, .. }, Some(map)) => Some((
                dir.clone(),
                map.levels.iter().map(|l| l.name.clone()).collect(),
            )),
            _ => None,
        };
        Self {
            source,
            levels: BTreeMap::new(),
        }
    }

    fn read(dir: &Path, level: &str) -> Option<BTreeMap<usize, String>> {
        const SUFFIX: &str = ".scene.json";
        let folder = dir.join("levels");
        let file = std::fs::read_dir(&folder)
            .ok()?
            .filter_map(Result::ok)
            .filter_map(|e| e.file_name().into_string().ok())
            .find(|n| {
                n.strip_suffix(SUFFIX)
                    .is_some_and(|stem| stem.eq_ignore_ascii_case(level))
            })?;
        let text = std::fs::read_to_string(folder.join(file)).ok()?;
        let scene: SceneNames = serde_json::from_str(&text).ok()?;
        Some(scene.actors.into_iter().map(|a| (a.slot, a.name)).collect())
    }

    /// The object name of actor `id`, if the level's scene file has it.
    fn name(&mut self, id: u32) -> Option<String> {
        let (dir, names) = self.source.as_ref()?;
        let level = usize::try_from(id >> 16).ok()?;
        let slot = usize::try_from(id & 0xFFFF).ok()?;
        let level_name = names.get(level)?;
        self.levels
            .entry(level)
            .or_insert_with(|| Self::read(dir, level_name))
            .as_ref()?
            .get(&slot)
            .cloned()
    }
}

/// What a query hit, for a note: the actor's name when the level has one.
struct SurfaceText {
    /// E.g. `StaticMeshActor_12 (StaticMesh)`.
    text: String,
    /// The actor's object name.
    name: Option<String>,
    /// Level geometry without an actor (BSP).
    world_geometry: bool,
}

fn surface_text(surface: &Surface, names: &mut ActorNames) -> SurfaceText {
    let name = surface.actor.and_then(|id| names.name(id));
    let world_geometry = surface.actor.is_none() && surface.class == ActorClass::WorldGeometry;
    let text = match (&name, surface.actor) {
        (Some(n), _) => format!("{n} ({:?})", surface.class),
        (None, Some(id)) => format!("actor {id} ({:?})", surface.class),
        (None, None) if world_geometry => "world geometry (no actor)".to_owned(),
        (None, None) => format!("a surface without an actor ({:?})", surface.class),
    };
    SurfaceText {
        text,
        name,
        world_geometry,
    }
}

/// How our floor's actor compares with the recorded base actor.
fn base_comparison(recorded: Option<&str>, ours: &SurfaceText) -> String {
    match (recorded, &ours.name) {
        (None, _) => "the recording names no base actor at this tick".to_owned(),
        (Some(r), Some(n)) if r == n => format!(
            "the recorded base actor has the same name ({r}; names are not unique across \
             streamed levels)"
        ),
        (Some(r), Some(_)) => format!("the recorded base actor is {r}: another actor"),
        (Some(r), None) if ours.world_geometry && r.starts_with("WorldInfo") => {
            format!("the recorded base is {r}: world geometry on both sides")
        }
        (Some(r), None) if ours.world_geometry => {
            format!("the recorded base actor is {r}: an actor, where ours is world geometry")
        }
        (Some(r), None) => {
            format!("the recorded base actor is {r} (ours has no actor name here: not comparable)")
        }
    }
}

/// Heights a start that overlaps our collision is probed from, UU above the
/// recorded position (a search ladder of the harness, not a game value).
const OVERLAP_PROBE_LIFTS: [f32; 9] = [1.0, 2.0, 4.0, 8.0, 16.0, 32.0, 64.0, 128.0, 256.0];

/// The notes about the start position on our collision (see the module
/// docs, "What the start notes say"). Returns whether the pawn's shape
/// overlaps our collision there.
fn start_position_notes(
    world: &GameWorld,
    params: &PlayerParams,
    s0: &TraceSample,
    recorded_base: Option<&str>,
    names: &mut ActorNames,
    notes: &mut Vec<String>,
) -> bool {
    let shape = CollisionShape {
        radius: params.movement.capsule_radius.value,
        half_height: params.movement.capsule_half_height.value,
    };
    let at = s0.position;
    let overlaps = world.overlaps(at, shape);
    if overlaps {
        let free = OVERLAP_PROBE_LIFTS
            .into_iter()
            .find(|lift| !world.overlaps(at + Vec3::Z * *lift, shape));
        let rest = match free {
            None => format!(
                "no free place for it up to {} uu above",
                OVERLAP_PROBE_LIFTS[OVERLAP_PROBE_LIFTS.len() - 1]
            ),
            Some(lift) => match world.sweep_capsule(at + Vec3::Z * lift, at, shape) {
                Some(hit) => format!(
                    "lowered from {lift} uu above, it comes to rest {:.3} uu above the recorded \
                     height, on {}",
                    f64::from(hit.position.z) - f64::from(at.z),
                    surface_text(&hit.surface, names).text
                ),
                None => format!(
                    "lowered from {lift} uu above, it reaches the recorded height without a \
                     contact (a grazing overlap)"
                ),
            },
        };
        notes.push(format!(
            "warning: the start position overlaps our collision: the pawn's shape at the \
             recorded position ({}, {}, {}) is inside our geometry; {rest}. Our pawn may not \
             move from here; the original stood there on its own collision",
            at.x, at.y, at.z
        ));
    }
    if s0.grounded && !overlaps {
        let probe = params.movement.step_height.value + STEP_FUDGE;
        let base = recorded_base.map_or_else(
            || "the recording names no base actor at this tick".to_owned(),
            |r| format!("the recorded base actor is {r}"),
        );
        match world.sweep_capsule(at, at - Vec3::Z * probe, shape) {
            Some(hit) => {
                let ours = surface_text(&hit.surface, names);
                notes.push(format!(
                    "start: our floor is {:.3} uu below the pawn at the recorded position (our \
                     floor check rests a pawn {TARGET_FLOOR_DIST} uu above its floor and leaves \
                     a based one alone between {MIN_FLOOR_DIST} and {MAX_FLOOR_DIST}): {}; {}",
                    hit.distance,
                    ours.text,
                    base_comparison(recorded_base, &ours)
                ));
            }
            None => notes.push(format!(
                "start: no floor of ours within {probe} uu below the recorded position ({base}): \
                 our pawn starts to fall"
            )),
        }
    }
    overlaps
}

/// The trace's recorded script state, read tick by tick.
struct Recorded<'a> {
    cursor: Option<TimelineCursor<'a>>,
}

impl Recorded<'_> {
    /// The recorded state at `tick` (ticks must not decrease).
    fn at(&mut self, tick: u64) -> Result<Option<TickState>> {
        match &mut self.cursor {
            Some(c) => Ok(c.advance(tick)?.cloned()),
            None => Ok(None),
        }
    }
}

/// The first tick at which one of our values differed from the recording's.
#[derive(Clone, Copy, Debug, PartialEq)]
struct FirstDifference {
    tick: u64,
    ours: f64,
    recorded: f64,
}

/// One compared value of the `state check:` notes.
#[derive(Clone, Copy, Debug, Default)]
struct CheckedValue {
    compared: usize,
    differing: usize,
    first: Option<FirstDifference>,
    largest: f64,
    largest_at: Option<FirstDifference>,
}

impl CheckedValue {
    fn add(&mut self, tick: u64, ours: f64, recorded: f64) {
        self.compared += 1;
        let difference = (ours - recorded).abs();
        if ours != recorded {
            self.differing += 1;
            let d = FirstDifference {
                tick,
                ours,
                recorded,
            };
            self.first.get_or_insert(d);
            if self.largest_at.is_none() || difference > self.largest {
                self.largest = difference;
                self.largest_at = Some(d);
            }
        }
    }
}

/// Our script state against the recording's, tick by tick (see the module
/// docs, "What else the notes compare").
#[derive(Clone, Copy, Debug, Default)]
struct StateCheck {
    ground_speed: CheckedValue,
    air_control: CheckedValue,
    sprinting: CheckedValue,
    times_grappled: CheckedValue,
    max_grapples: CheckedValue,
    eye_height: CheckedValue,
}

impl StateCheck {
    fn add(&mut self, tick: u64, ours: &PlayerState, recorded: &TickState) {
        if !ours.script.started {
            return;
        }
        let s = &ours.script;
        if let Some(v) = recorded.ground_speed {
            self.ground_speed
                .add(tick, f64::from(s.ground_speed), f64::from(v));
        }
        if let Some(v) = recorded.air_control {
            self.air_control
                .add(tick, f64::from(s.air_control), f64::from(v));
        }
        if let Some(v) = recorded.sprinting {
            self.sprinting.add(
                tick,
                f64::from(u8::from(s.sprint.active)),
                f64::from(u8::from(v)),
            );
        }
        if let Some(v) = recorded.times_grappled {
            self.times_grappled
                .add(tick, f64::from(s.gun.times_grappled), f64::from(v));
        }
        if let Some(v) = recorded.max_grapples {
            self.max_grapples
                .add(tick, f64::from(s.gun.max_grapples), f64::from(v));
        }
        if let Some(v) = recorded.eye_height {
            self.eye_height
                .add(tick, f64::from(s.eye_height), f64::from(v));
        }
    }

    fn notes(&self, notes: &mut Vec<String>) {
        let exact = [
            ("GroundSpeed", &self.ground_speed),
            ("AirControl", &self.air_control),
            ("the sprint flag", &self.sprinting),
            ("the used-grapple count", &self.times_grappled),
            ("the grapple capacity", &self.max_grapples),
        ];
        let agreeing: Vec<&str> = exact
            .iter()
            .filter(|(_, c)| c.compared > 0 && c.differing == 0)
            .map(|(name, _)| *name)
            .collect();
        if let Some(ticks) = exact.iter().map(|(_, c)| c.compared).max()
            && !agreeing.is_empty()
        {
            notes.push(format!(
                "state check: after each of {ticks} tick(s) our {} equal(s) the recording's",
                agreeing.join(", ")
            ));
        }
        for (name, c) in exact {
            if let Some(d) = c.first {
                notes.push(format!(
                    "state check: {name} differs from the recording's after {} of {} tick(s) \
                     (first at tick {}: ours {}, recorded {})",
                    c.differing, c.compared, d.tick, d.ours, d.recorded
                ));
            }
        }
        let e = &self.eye_height;
        if e.compared > 0 {
            match e.largest_at {
                None => notes.push(format!(
                    "state check: our eye height equals the recorded EyeHeight after each of {} \
                     tick(s)",
                    e.compared
                )),
                Some(d) => notes.push(format!(
                    "state check: our eye height differs from the recorded EyeHeight after {} \
                     of {} tick(s); largest difference {:.4} uu at tick {} (ours {}, recorded {})",
                    e.differing, e.compared, e.largest, d.tick, d.ours, d.recorded
                )),
            }
        }
    }
}

/// What the resynchronisation of a one-step replay had to change beyond
/// values (see [`resync`]).
#[derive(Clone, Copy, Debug, Default)]
struct ResyncCounts {
    ticks: usize,
    mode: usize,
    first_mode: Option<u64>,
    attached: usize,
    first_attached: Option<u64>,
    released: usize,
    first_released: Option<u64>,
    anchor: usize,
    first_anchor: Option<u64>,
}

/// Writes the recorded script state `st` into the player: only values, and
/// only where they differ in kind (story mode entered or left, boots
/// switched), so that a state that already agrees is left bit for bit.
fn write_recorded_state(p: &mut PlayerState, params: &PlayerParams, st: &TickState) {
    if p.script.started
        && let Some(pawn_params) = params.pawn.as_ref()
    {
        if let Some(shown) = st
            .ground_speed
            .and_then(|g| story_mode_shown(g, pawn_params))
        {
            match (shown, p.script.is_story()) {
                (true, false) => {
                    pawn::enter_story_mode(p, params);
                }
                (false, true) => pawn::exit_story_mode(p, params),
                _ => {}
            }
        }
        let s = &mut p.script;
        if let Some(v) = st.ground_speed {
            s.ground_speed = v;
        }
        if let Some(v) = st.sprinting {
            s.sprint.active = v;
        }
        if let Some(v) = st.air_control {
            s.air_control = v;
        }
        if let Some(v) = st.jump_z {
            s.jump_z = v;
        }
        if let Some(v) = st.air_speed {
            s.air_speed = v;
        }
        if let Some(v) = st.eye_height {
            s.eye_height = v;
        }
    }
    if let Some(n) = st.max_grapples
        && p.script.gun.max_grapples != n
    {
        grapple_gun::set_max_grapples(p, n);
    }
    if let Some(n) = st.times_grappled {
        p.script.gun.times_grappled = n;
    }
    if let Some(on) = st.can_grapple {
        grapple_gun::enable_grapple(p, on);
    }
    if let Some(on) = st.released {
        p.script.gun.released = on;
    }
    if let Some(on) = st.boots_enabled
        && p.script.boots.enabled != on
    {
        rocket_boots::enable_rocket_boots(p, on);
    }
}

/// Puts the player into the state of the input trace's sample `o` (the
/// state the original's next tick starts from) with the recorded script
/// state `st` of that tick: the one-step resynchronisation (see the module
/// docs for what it covers and what it cannot).
fn resync(
    p: &mut PlayerState,
    params: &PlayerParams,
    o: &TraceSample,
    st: Option<&TickState>,
    counts: &mut ResyncCounts,
) {
    counts.ticks += 1;
    let yaw = in_range_yaw(o.yaw);
    p.position = o.position;
    p.velocity = o.velocity;
    p.yaw = yaw;
    p.pitch = o.pitch;
    if p.script.started {
        p.script.pov_yaw = yaw;
        p.script.pov_pitch = o.pitch;
    }
    let ours_attached = p.script.gun.attached.is_some();
    match (o.grapple_state, o.grapple_anchor) {
        (TraceGrappleState::Attached, Some(anchor)) if p.script.gun.spawned => {
            if !ours_attached {
                // What an attach leaves (GRAPPLE.md §6), without its events,
                // budget and timers: the target is unknown, so it is world
                // geometry that carries no anchor.
                let g = &mut p.script.gun;
                g.grapple_location = anchor;
                g.attached = Some(Attachment::default());
                g.follow = None;
                g.has_grappled = true;
                p.pawn.flying = true;
                p.grounded = false;
                p.pawn.based = false;
                p.script.release_gap = false;
                p.script.code.goto(PawnStateName::Shooting);
                counts.attached += 1;
                counts.first_attached.get_or_insert(o.tick);
            } else if p.script.gun.grapple_location != anchor {
                p.script.gun.grapple_location = anchor;
                p.script.gun.follow = None;
                counts.anchor += 1;
                counts.first_anchor.get_or_insert(o.tick);
            }
        }
        _ => {
            if ours_attached {
                // The common release (physics Falling, pawn `Release`, the
                // controller's release gap); its handler calls are dropped.
                grapple_gun::release_from_outside(p, ReleaseReason::External);
                counts.released += 1;
                counts.first_released.get_or_insert(o.tick);
            }
            if p.grounded != o.grounded || p.pawn.flying {
                p.grounded = o.grounded;
                p.pawn = PawnPhysicsState {
                    force_floor_check: o.grounded,
                    based: o.grounded && st.is_some_and(|s| s.base.is_some()),
                    ..PawnPhysicsState::default()
                };
                counts.mode += 1;
                counts.first_mode.get_or_insert(o.tick);
            }
        }
    }
    if let Some(st) = st {
        write_recorded_state(p, params, st);
    }
}

/// The simulation of a replay: `Game::tick` (optionally with Kismet) at a
/// fixed rate, or the per-sample stepper.
struct Run {
    game: Game,
    script: Option<LevelScript>,
    /// The per-sample stepper ([`Stepping::PerSample`]).
    sim: Option<VariableStepper>,
    /// Input ticks of the fixed path's recorded samples, in order.
    ticks: Vec<u64>,
    /// Fixed path: samples to overwrite after the recording stopped
    /// (index into the recording, state to copy).
    patches: Vec<(usize, TraceSample)>,
    /// Per-sample path: our samples.
    samples: Vec<TraceSample>,
    variable: VariableSteps,
    lengths: Vec<f64>,
    /// Time of the start sample.
    t0: f64,
}

impl Run {
    fn player(&self) -> &PlayerState {
        match &self.sim {
            Some(s) => s.player(),
            None => self.game.player(),
        }
    }

    fn player_mut(&mut self) -> &mut PlayerState {
        match &mut self.sim {
            Some(s) => s.player_mut(),
            None => self.game.player_mut(),
        }
    }

    fn fov(&self) -> f32 {
        match &self.sim {
            Some(s) => s.fov(),
            None => self.game.fov(),
        }
    }

    fn respawn_count(&self) -> u32 {
        match &self.sim {
            Some(s) => s.respawn_count(),
            None => self.game.respawn_count(),
        }
    }

    /// One tick with the input of `s` (the sample after `prev`), recorded
    /// as tick `s.tick`.
    fn tick(&mut self, prev: &TraceSample, s: &TraceSample, notes: &mut Vec<String>) -> Result<()> {
        match &mut self.sim {
            None => {
                let ticked = match self.script.as_mut() {
                    Some(sc) => sc.tick(&mut self.game, &s.input).is_some(),
                    None => self.game.tick(&s.input).is_some(),
                };
                if !ticked {
                    bail!("the game did not tick at input tick {}", s.tick);
                }
                self.ticks.push(s.tick);
            }
            Some(sim) => {
                let seconds = s.time - prev.time;
                let length = FrameLength::of(seconds);
                match length {
                    FrameLength::Step(_) => {}
                    FrameLength::Clamped => {
                        self.variable.clamped += 1;
                        self.variable.first_clamped.get_or_insert(s.tick);
                    }
                    FrameLength::NoOp => {
                        self.variable.no_op += 1;
                        self.variable.first_no_op.get_or_insert(s.tick);
                    }
                }
                self.lengths.push(seconds);
                let report = sim.tick(&s.input, length.dt(), finite(s.time - self.t0));
                if report.respawned {
                    notes.push(format!("respawn (kill_z) at tick {}", s.tick));
                }
                self.samples.push(TraceSample::capture(
                    s.tick,
                    s.time,
                    &s.input,
                    sim.player(),
                    sim.fov(),
                ));
            }
        }
        Ok(())
    }

    /// Records the player's present state as the sample of the tick that
    /// just ran (after an injection changed it).
    fn recapture(&mut self, s: &TraceSample) {
        let sample = TraceSample::capture(s.tick, s.time, &s.input, self.player(), self.fov());
        match &self.sim {
            // The game's recording gets it when it stops; index 0 is the
            // start sample.
            None => self
                .patches
                .push((self.ticks.len().saturating_sub(1), sample)),
            Some(_) => {
                if let Some(last) = self.samples.last_mut() {
                    *last = sample;
                }
            }
        }
    }
}

/// Replays `original` (see the module docs).
///
/// # Errors
/// Empty trace, contradictory or invalid stepping options, a trace without a
/// fixed rate whose sample times never advance, a per-sample replay with
/// Kismet, an invalid `state:` note, level loading or game construction
/// errors.
pub fn replay(original: &Trace, opts: &ReplayOptions) -> Result<ReplayResult> {
    let mut start = match opts.start_tick {
        None => 0,
        Some(t) => original
            .samples
            .iter()
            .position(|s| s.tick == t)
            .with_context(|| format!("the trace has no sample at tick {t}"))?,
    };
    let mut start_notes = Vec::new();
    let all = original.samples.as_slice();
    let is_original = original.meta.source == TraceSource::Original;
    if is_original
        && let Some(asked) = all.get(start)
        && !is_clean_start(all, start)
    {
        let why = not_clean_reasons(all, start).join(", ");
        let tick_of = |i: Option<usize>| {
            i.and_then(|i| all.get(i))
                .map_or_else(|| "none".to_owned(), |s| s.tick.to_string())
        };
        match opts.start {
            StartPolicy::Refuse => bail!(
                "tick {} of this recording is not a standing-still start ({why}); the script \
                 state that no record shows (jump damping, a charging power jump, the grapple's \
                 attachment and timers) would start fresh. Standing-still starts: {} before it, \
                 {} after it. Start from one of them (--from-tick), let the replay move to the \
                 next one (--start snap), or start here all the same (--start force)",
                asked.tick,
                tick_of(previous_clean_start(all, start)),
                tick_of(next_clean_start(all, start))
            ),
            StartPolicy::Snap => {
                let next = next_clean_start(all, start).with_context(|| {
                    format!("no standing-still start at or after tick {}", asked.tick)
                })?;
                start_notes.push(format!(
                    "start: moved from tick {} ({why}) to the next standing-still tick {}",
                    asked.tick,
                    tick_of(Some(next))
                ));
                start = next;
            }
            StartPolicy::Force => start_notes.push(format!(
                "warning: forced start at tick {}, which is not a standing-still start ({why}); \
                 script state that no record shows starts fresh",
                asked.tick
            )),
        }
    }
    let end = match opts.max_ticks {
        None => original.samples.len(),
        Some(n) => usize::try_from(n)
            .ok()
            .and_then(|n| start.checked_add(n)?.checked_add(1))
            .map_or(original.samples.len(), |e| e.min(original.samples.len())),
    };
    let samples = original.samples.get(start..end).unwrap_or(&[]);
    let Some(s0) = samples.first() else {
        bail!("the trace has no samples");
    };
    let stepping = Stepping::choose(original, opts)?;
    let source = match original.meta.source {
        TraceSource::Original => "original",
        TraceSource::Runtime => "runtime",
    };
    let (mut game, script, mut notes) = match stepping {
        Stepping::Fixed(rate) => {
            let (game, script, level_desc) = build(opts, rate)?;
            let note =
                format!("asamu-trace replay of a {source} trace on {level_desc} at {rate} Hz");
            (game, script, vec![note])
        }
        Stepping::PerSample => {
            if matches!(opts.level, ReplayLevel::Converted { kismet: true, .. }) {
                bail!(
                    "a replay with per-sample frame lengths cannot run the map's Kismet: \
                     asamu-game has no tick that takes a frame length. Replay without --kismet \
                     (player and level objects only), or pass --tick-rate {DEFAULT_TICK_RATE_HZ} \
                     to replay with ticks of equal length"
                );
            }
            let lengths = lengths_until_gap(samples);
            if !lengths.is_empty()
                && lengths
                    .iter()
                    .all(|d| FrameLength::of(*d) == FrameLength::NoOp)
            {
                bail!(
                    "the trace has no fixed tick rate and its sample times never advance; \
                     pass --tick-rate"
                );
            }
            // The clock's rate is not used: the stepper takes over below.
            let (game, script, level_desc) = build(opts, DEFAULT_TICK_RATE_HZ)?;
            let note = format!(
                "asamu-trace replay of a {source} trace on {level_desc} with each sample's own \
                 frame length: {} ticks, {}",
                lengths.len(),
                StepStats::of_lengths(lengths).describe()
            );
            (game, script, vec![note])
        }
    };
    if let Some(l) = &original.meta.level {
        notes.push(format!("input trace level: {l}"));
    }
    notes.append(&mut start_notes);
    if script.is_some() && start > 0 {
        notes.push(format!(
            "note: Kismet starts from level start, not from the state at tick {}",
            s0.tick
        ));
    }

    // The recorded script state. Without the start state (`use_init` off) a
    // broken note is not an error, as before it was read for anything else.
    let timeline = match StateTimeline::from_notes(&original.meta.notes) {
        Ok(t) => t,
        Err(e) if !opts.use_init => {
            notes.push(format!(
                "warning: the trace's state: note is not readable ({e:#}); events and the \
                 recorded state are not used"
            ));
            None
        }
        Err(e) => return Err(e),
    };
    // What happens in the input trace inside the replayed ticks.
    let recorded_events = if is_original {
        recorded_events(all, timeline.as_ref())?
    } else {
        Vec::new()
    };
    let inside = |index: usize| index > start && index < end;
    let mut ends: BTreeMap<u64, Vec<EventKind>> = BTreeMap::new();
    for e in &recorded_events {
        if inside(e.index) && e.kind.ends_validity() {
            ends.entry(e.tick).or_default().push(e.kind);
        }
    }
    let mut attaches = is_original.then(|| Attaches {
        original: recorded_events
            .iter()
            .filter(|e| inside(e.index) && e.kind.is_attach())
            .map(|e| e.tick)
            .collect(),
        ours: Vec::new(),
    });
    let mut recorded = Recorded {
        cursor: timeline.as_ref().map(StateTimeline::cursor),
    };
    let first_tick = original.samples.first().map_or(s0.tick, |f| f.tick);
    let state_at_start = match recorded.at(s0.tick)? {
        Some(st) => Some(st),
        None => InitState::from_notes(&original.meta.notes)
            .filter(|_| timeline.is_none())
            .map(|init| {
                if opts.use_init && s0.tick != first_tick {
                    notes.push(
                        "note: the init: state belongs to the first sample, not the start tick"
                            .to_owned(),
                    );
                }
                TickState::from(&init)
            }),
    };

    // Initial state.
    let wrote_state = {
        let p = game.player_mut();
        let yaw = in_range_yaw(s0.yaw);
        let same = p.position == s0.position
            && p.velocity == s0.velocity
            && p.yaw == yaw
            && p.pitch == s0.pitch
            && p.grounded == s0.grounded;
        if same {
            notes.push("initial state: the spawn state equals sample 0 (kept)".to_owned());
        } else {
            p.position = s0.position;
            p.velocity = s0.velocity;
            p.yaw = yaw;
            p.pitch = s0.pitch;
            p.grounded = s0.grounded;
            p.pawn = PawnPhysicsState {
                force_floor_check: s0.grounded,
                ..PawnPhysicsState::default()
            };
            if p.script.started {
                p.script.pov_yaw = yaw;
                p.script.pov_pitch = s0.pitch;
            }
            notes.push(format!(
                "initial state: sample {} (position, velocity, view, {})",
                s0.tick,
                if s0.grounded { "walking" } else { "falling" }
            ));
        }
        if s0.grapple_state == TraceGrappleState::Attached && !opts.one_step {
            notes.push(
                "warning: the trace starts attached; the attachment is not recreated".to_owned(),
            );
        }
        !same
    };
    if opts.use_init {
        if is_original && s0.tick == first_tick {
            notes.push(
                "note: the buttons held before the first record are unknown (taken as released)"
                    .to_owned(),
            );
        }
        apply_start_state(
            &mut game,
            &state_at_start.clone().unwrap_or_default(),
            s0,
            opts.story_mode,
            wrote_state,
            &mut notes,
        );
    }
    if let Some(n) = opts.max_grapples {
        grapple_gun::set_max_grapples(game.player_mut(), n);
        notes.push(format!("max_grapples override {n}"));
    }
    let run_params = game.params().clone();
    let mut names = ActorNames::new(&opts.level, &game);
    let start_overlaps = wrote_state
        && start_position_notes(
            game.world(),
            &run_params,
            s0,
            state_at_start.as_ref().and_then(|s| s.base.as_deref()),
            &mut names,
            &mut notes,
        );

    game.start();
    game.start_recording();
    let mut run = Run {
        sim: None,
        game,
        script,
        ticks: vec![s0.tick],
        patches: Vec::new(),
        samples: Vec::new(),
        variable: VariableSteps {
            lengths: StepStats::default(),
            clamped: 0,
            first_clamped: None,
            no_op: 0,
            first_no_op: None,
        },
        lengths: Vec::new(),
        t0: s0.time,
    };
    if stepping == Stepping::PerSample {
        let sim = VariableStepper::from_game(&run.game);
        notes.push(
            "stepping: per-sample frame lengths on asamu-trace's stepper (a rebuild of Game::tick \
             from the public simulation API; asamu-game has no tick that takes a frame length)"
                .to_owned(),
        );
        if sim.is_converted() {
            notes.push(
                "note: on a converted level this stepper simulates the player and the level \
                 objects (recharge crystals, glow flowers, interactables, attractor pads) only; \
                 touch volumes, checkpoints, kill zones, deaths and respawns, falling rocks, \
                 level streaming, NPCs and Kismet are not simulated"
                    .to_owned(),
            );
        }
        run.samples.push(TraceSample::capture(
            s0.tick,
            s0.time,
            &InputFrame::default(),
            sim.player(),
            sim.fov(),
        ));
        run.sim = Some(sim);
    }
    let respawns_before = run.respawn_count();

    // The ticks.
    let mut stopped_at_gap = None;
    let mut stopped_at_event = None;
    let mut crossed: Vec<CrossedEvent> = Vec::new();
    let mut check = StateCheck::default();
    let mut counts = ResyncCounts::default();
    let mut resync_overlaps: Vec<u64> = Vec::new();
    let pawn_shape = CollisionShape {
        radius: run_params.movement.capsule_radius.value,
        half_height: run_params.movement.capsule_half_height.value,
    };
    let mut state_prev = state_at_start;
    let mut prev = s0;
    let mut simulated = 0_usize;
    let mut original_travel = 0.0_f64;
    for s in samples.iter().skip(1) {
        if prev.tick.checked_add(1) != Some(s.tick) {
            stopped_at_gap = Some(prev.tick);
            break;
        }
        let state_now = recorded.at(s.tick)?;
        let kinds = ends.get(&s.tick).map_or(&[][..], Vec::as_slice);
        let mut inject = false;
        if !kinds.is_empty() {
            let action = match (opts.events, opts.one_step) {
                (EventPolicy::Ignore, _) => EventAction::Ignored,
                (_, true) => EventAction::LeftOut,
                (EventPolicy::Stop, false) => EventAction::Stopped,
                (EventPolicy::Inject, false) => EventAction::Injected,
            };
            crossed.extend(kinds.iter().map(|kind| CrossedEvent {
                tick: s.tick,
                kind: *kind,
                action,
            }));
            match action {
                EventAction::Stopped => {
                    stopped_at_event = Some(s.tick);
                    break;
                }
                EventAction::LeftOut => {
                    state_prev = state_now;
                    prev = s;
                    continue;
                }
                EventAction::Injected => inject = true,
                EventAction::Ignored => {}
            }
        }
        if opts.one_step {
            // The recording's position on our collision: a tick that starts
            // inside it is not one tick of our movement rules.
            if run.game.world().overlaps(prev.position, pawn_shape) {
                resync_overlaps.push(s.tick);
            }
            // Without the recorded start state (`use_init` off) the recorded
            // script state is not written on later ticks either.
            resync(
                run.player_mut(),
                &run_params,
                prev,
                state_prev.as_ref().filter(|_| opts.use_init),
                &mut counts,
            );
        }
        if inject
            && kinds.iter().any(|k| k.is_level_state())
            && let Some(st) = &state_now
        {
            // The level script's part of the recorded state, before our tick.
            let level_state = TickState {
                ground_speed: st.ground_speed,
                max_grapples: st.max_grapples,
                boots_enabled: st.boots_enabled,
                ..TickState::default()
            };
            write_recorded_state(run.player_mut(), &run_params, &level_state);
        }
        let before = {
            let p = run.player();
            (p.is_grapple_attached(), p.script.gun.times_grappled)
        };
        run.tick(prev, s, &mut notes)?;
        simulated += 1;
        original_travel += tick_displacement(prev, s);
        // Counted before an injected teleport rewrites the state.
        if let Some(a) = &mut attaches {
            let p = run.player();
            if (p.is_grapple_attached() && !before.0) || p.script.gun.times_grappled > before.1 {
                a.ours.push(s.tick);
            }
        }
        if inject && kinds.contains(&EventKind::Teleport) {
            // Where the recording's pawn is after its teleport.
            let mut unused = ResyncCounts::default();
            resync(
                run.player_mut(),
                &run_params,
                s,
                state_now.as_ref(),
                &mut unused,
            );
            let p = run.player_mut();
            p.pawn = PawnPhysicsState {
                force_floor_check: s.grounded,
                based: s.grounded && state_now.as_ref().is_some_and(|st| st.base.is_some()),
                flying: p.pawn.flying,
                ..PawnPhysicsState::default()
            };
            run.recapture(s);
        }
        if simulated == 1 && wrote_state {
            first_tick_note(s0, s, run.player(), &mut notes);
        }
        if is_original && let Some(st) = &state_now {
            check.add(s.tick, run.player(), st);
        }
        state_prev = state_now;
        prev = s;
    }
    let respawns = run.respawn_count().saturating_sub(respawns_before);

    // Our trace.
    let mut trace = run
        .game
        .stop_recording()
        .context("recording was not running")?;
    let variable = match &run.sim {
        None => {
            // Our ticks count from 0; give them the input trace's numbers.
            for (s, tick) in trace.samples.iter_mut().zip(&run.ticks) {
                s.tick = *tick;
            }
            for (index, patch) in &run.patches {
                if let Some(s) = trace.samples.get_mut(*index) {
                    *s = TraceSample {
                        tick: s.tick,
                        time: s.time,
                        ..*patch
                    };
                }
            }
            None
        }
        Some(_) => {
            // The game's recording gives the meta line (level, model and
            // parameter notes); the samples come from the stepper.
            trace.meta.tick_rate = None;
            trace.samples = std::mem::take(&mut run.samples);
            for n in &mut trace.meta.notes {
                if n == "recorded by asamu-game" {
                    *n = "recorded by asamu-trace (per-sample stepper)".to_owned();
                }
            }
            let mut variable = run.variable;
            variable.lengths = StepStats::of_lengths(std::mem::take(&mut run.lengths));
            if let Some(t) = variable.first_clamped {
                notes.push(format!(
                    "warning: {} tick(s) longer than {MAX_STEP_DT} s were simulated as \
                     {MAX_STEP_DT} s (our step bound; first at tick {t})",
                    variable.clamped
                ));
            }
            if let Some(t) = variable.first_no_op {
                notes.push(format!(
                    "warning: {} tick(s) have a frame length that is not positive; no time \
                     passed in them (first at tick {t})",
                    variable.no_op
                ));
            }
            Some(variable)
        }
    };
    // The start sample is the input trace's own: its input is the one that
    // led to the start state (not simulated here), so that a comparison
    // counts no input mismatch at the start tick.
    if let Some(first) = trace.samples.first_mut() {
        first.input = s0.input;
    }
    if let Some(t) = stopped_at_gap {
        notes.push(format!("stopped at the tick gap after input tick {t}"));
    }
    if respawns > 0 {
        notes.push(format!("{respawns} respawn(s) during the replay"));
    }
    if opts.one_step {
        let state = match (timeline.is_some(), opts.use_init) {
            (_, false) => Some("the recorded state is switched off"),
            (false, true) => Some("the trace has no state: note"),
            (true, true) => None,
        };
        one_step_notes(&counts, state, &mut notes);
        if let Some(first) = resync_overlaps.first() {
            let ticks: Vec<String> = resync_overlaps.iter().map(u64::to_string).collect();
            notes.push(format!(
                "{ONE_STEP_NOTE_PREFIX} warning: {} of {} tick(s) start inside our collision (the \
                 pawn's shape at the recording's previous sample overlaps our geometry; first \
                 tick {first}): our pawn starts them stuck in or pushed out of geometry the \
                 original does not have there, so their errors measure the collision difference, \
                 not one tick of our movement rules. Tick(s) {}",
                resync_overlaps.len(),
                counts.ticks,
                bounded(&ticks)
            ));
        }
    }
    event_notes(
        &crossed,
        stopped_at_event,
        simulated,
        samples.len().saturating_sub(1),
        &mut notes,
    );
    if let Some(a) = &attaches
        && (!a.original.is_empty() || !a.ours.is_empty())
    {
        let list = |ticks: &[u64]| {
            if ticks.is_empty() {
                String::new()
            } else {
                let ticks: Vec<String> = ticks.iter().map(u64::to_string).collect();
                format!(" (tick(s) {})", bounded(&ticks))
            }
        };
        // A tick the replay did not simulate has no attach of ours.
        let simulated_ticks: Vec<u64> = trace.samples.iter().map(|s| s.tick).collect();
        let comparable: Vec<u64> = a
            .original
            .iter()
            .copied()
            .filter(|t| simulated_ticks.binary_search(t).is_ok())
            .collect();
        notes.push(format!(
            "attaches: the original attached the grapple {} time(s) in the replayed ticks{}, \
             ours {} time(s){}; counted from the attached flag and the used-grapple counter; {}",
            comparable.len(),
            list(&comparable),
            a.ours.len(),
            list(&a.ours),
            if comparable == a.ours {
                "the same ticks"
            } else {
                "not the same ticks"
            }
        ));
    }
    if is_original {
        check.notes(&mut notes);
    }
    if start_overlaps
        && simulated > 0
        && trace.samples.iter().all(|s| s.position == s0.position)
        && original_travel > 0.0
        && !opts.one_step
    {
        notes.push(format!(
            "warning: our pawn did not move in {simulated} tick(s) while the original travelled \
             {original_travel:.1} uu: the start position overlaps our collision"
        ));
    }
    trace.meta.notes.extend(notes);
    trace.validate()?;
    if let Some(a) = &mut attaches {
        let ticks: Vec<u64> = trace.samples.iter().map(|s| s.tick).collect();
        a.original.retain(|t| ticks.binary_search(t).is_ok());
    }
    Ok(ReplayResult {
        trace,
        start_tick: s0.tick,
        stopped_at_gap,
        respawns,
        stepping,
        variable,
        one_step: opts.one_step,
        stopped_at_event,
        events: crossed,
        attaches,
        start_overlaps,
        resync_overlaps,
    })
}

/// The `start: in the first tick …` note (see the module docs): how far our pawn
/// and the original moved vertically in the first tick while walking.
fn first_tick_note(
    s0: &TraceSample,
    s1: &TraceSample,
    ours: &PlayerState,
    notes: &mut Vec<String>,
) {
    if !(s0.grounded && s1.grounded && ours.grounded) {
        return;
    }
    let (start, theirs, mine) = (
        f64::from(s0.position.z),
        f64::from(s1.position.z),
        f64::from(ours.position.z),
    );
    let (dz_ours, dz_original) = (mine - start, theirs - start);
    let difference = dz_ours - dz_original;
    let band = f64::from(MAX_FLOOR_DIST) - f64::from(MIN_FLOOR_DIST);
    if difference.abs() > band {
        notes.push(format!(
            "warning: in the first tick our pawn moves {dz_ours:+.4} uu vertically and the \
             original {dz_original:+.4} uu while both walk: {difference:+.4} uu apart, more than \
             the width of the native hover band ({band:.1} uu), so the two stand on different \
             floor heights from the start. Every vertical number of this replay carries that \
             offset"
        ));
    } else {
        notes.push(format!(
            "start: in the first tick our pawn moves {dz_ours:+.4} uu vertically and the \
             original {dz_original:+.4} uu while both walk"
        ));
    }
}

/// The notes of a one-step replay. `no_state`: why the recorded script
/// state was not written, if it was not.
fn one_step_notes(counts: &ResyncCounts, no_state: Option<&str>, notes: &mut Vec<String>) {
    let state = match no_state {
        None => "the recorded script state (GroundSpeed with the story mode it shows, \
                 AirControl, JumpZ, AirSpeed, the sprint flag, the gun's used count, capacity, \
                 fire latch and released flag, rocket boots enabled, the eye height when \
                 recorded)"
            .to_owned(),
        Some(why) => format!("no script state ({why})"),
    };
    notes.push(format!(
        "{ONE_STEP_NOTE_PREFIX} each of {} tick(s) starts from the input trace's previous sample \
         (position, velocity, view, walking, falling or attached at the recorded anchor) and \
         {state}; the errors are those of one tick, not accumulated drift",
        counts.ticks
    ));
    notes.push(format!(
        "{ONE_STEP_NOTE_PREFIX} not resynchronised (no record shows them; kept from our own \
         previous tick): the pawn's and the power jump's state code and timers, sprint after \
         landing, the move-input lock, the gun's weapon state and timers, the boots' boost, the \
         walk bob, the FOV, the floor normal and the base flag while the physics mode agrees, \
         level objects and Kismet"
    ));
    let mut changed = Vec::new();
    for (n, first, what) in [
        (
            counts.mode,
            counts.first_mode,
            "the physics mode was set to the recording's (walking or falling)",
        ),
        (
            counts.attached,
            counts.first_attached,
            "an attachment of the original that we did not have was created",
        ),
        (
            counts.released,
            counts.first_released,
            "an attachment of ours that the original did not have was released",
        ),
        (
            counts.anchor,
            counts.first_anchor,
            "our anchor was moved to the recorded one",
        ),
    ] {
        if let Some(t) = first {
            changed.push(format!("{what} before {n} tick(s) (first after tick {t})"));
        }
    }
    if !changed.is_empty() {
        notes.push(format!("{ONE_STEP_NOTE_PREFIX} {}", changed.join("; ")));
    }
}

/// The notes about the events the inputs cannot reproduce.
fn event_notes(
    crossed: &[CrossedEvent],
    stopped_at: Option<u64>,
    simulated: usize,
    asked: usize,
    notes: &mut Vec<String>,
) {
    let list = |action: EventAction| -> Vec<String> {
        crossed
            .iter()
            .filter(|e| e.action == action)
            .map(|e| format!("{} {}", e.tick, e.kind.name()))
            .collect()
    };
    if let Some(tick) = stopped_at {
        notes.push(format!(
            "validity: stopped before tick {tick} ({}): a replay of the inputs cannot reproduce \
             it. {simulated} of {asked} tick(s) replayed; --level-events inject takes the \
             recorded change and runs on, ignore runs through",
            list(EventAction::Stopped)
                .iter()
                .map(|e| e.split_once(' ').map_or(e.as_str(), |(_, kind)| kind))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    let left_out = list(EventAction::LeftOut);
    if !left_out.is_empty() {
        notes.push(format!(
            "validity: {} event(s) the inputs cannot reproduce; their ticks are left out of this \
             one-step replay (not simulated; the tick after starts from the recording): {}",
            left_out.len(),
            bounded(&left_out)
        ));
    }
    let injected = list(EventAction::Injected);
    if !injected.is_empty() {
        notes.push(format!(
            "injected: {} recorded change(s) the inputs cannot reproduce were taken from the \
             recording (--level-events inject): a level-script state change is written before \
             our tick, a teleport replaces our position, velocity, view and physics mode after \
             it: {}",
            injected.len(),
            bounded(&injected)
        ));
    }
    let ignored = list(EventAction::Ignored);
    if let Some(first) = crossed.iter().find(|e| e.action == EventAction::Ignored) {
        notes.push(format!(
            "warning: the replay ran through {} event(s) the inputs cannot reproduce \
             (--level-events ignore): {}. From tick {} on the comparison is not valid",
            ignored.len(),
            bounded(&ignored),
            first.tick
        ));
    }
}

fn on_off(on: bool) -> &'static str {
    if on { "on" } else { "off" }
}

/// Puts the script state `recorded` (the original's at the start tick), the
/// button levels of the start sample `s0` and its FOV into the game's player
/// (see the module docs, "Start state"). Writes what it set to `notes`.
/// `wrote_state`: the start sample replaced the spawn state.
fn apply_start_state(
    game: &mut Game,
    recorded: &TickState,
    s0: &TraceSample,
    story_mode: Option<bool>,
    wrote_state: bool,
    notes: &mut Vec<String>,
) {
    let params = game.params().clone();
    let p = game.player_mut();
    let mut applied = Vec::new();
    if p.script.started
        && let Some(pawn_params) = params.pawn.as_ref()
    {
        let walk = pawn_params.move_speed.value;
        let story_speed = walk * pawn_params.story_speed_multiplier.value;
        let sprint_speed = walk * pawn_params.sprint_speed_multiplier.value;
        let shown = recorded
            .ground_speed
            .and_then(|g| story_mode_shown(g, pawn_params));
        let was = p.script.is_story();
        match story_mode.or(shown) {
            Some(true) if !was => {
                pawn::enter_story_mode(p, &params);
            }
            Some(false) if was => pawn::exit_story_mode(p, &params),
            _ => {}
        }
        match (story_mode, shown, recorded.ground_speed) {
            (Some(on), _, _) => applied.push(format!("story mode {} (--story-mode)", on_off(on))),
            (None, Some(true), Some(g)) => {
                applied.push(format!(
                    "story mode on (GroundSpeed {g} is the story speed)"
                ));
            }
            (None, Some(false), Some(g)) => applied.push(format!(
                "story mode off (GroundSpeed {g} is the walking or the sprint speed)"
            )),
            (None, None, Some(g)) => notes.push(format!(
                "warning: the recording does not show whether the pawn is in story mode \
                 (GroundSpeed {g} is none of the story, walking and sprint speeds \
                 {story_speed}, {walk}, {sprint_speed}); left {}; state it with --story-mode",
                on_off(was)
            )),
            _ => {}
        }
        let s = &mut p.script;
        if let Some(v) = recorded.ground_speed {
            s.ground_speed = v;
            applied.push(format!("ground_speed {v}"));
        }
        if let Some(v) = recorded.sprinting {
            s.sprint = SprintState {
                active: v,
                armed: false,
            };
            applied.push(format!("sprinting {v}"));
        }
        if let Some(v) = recorded.air_control {
            s.air_control = v;
            applied.push(format!("air_control {v}"));
        }
        if let Some(v) = recorded.jump_z {
            s.jump_z = v;
            applied.push(format!("jump_z {v}"));
        }
        if let Some(v) = recorded.air_speed {
            s.air_speed = v;
            applied.push(format!("air_speed {v}"));
        }
        let i = &s0.input;
        s.jump_was_held = i.jump_held;
        s.sprint_was_held = i.sprint_held;
        s.power_jump_was_held = i.power_jump_held;
        let held: Vec<&str> = [
            (i.jump_held, "jump"),
            (i.sprint_held, "sprint"),
            (i.power_jump_held, "power jump"),
            (i.grapple_held, "grapple"),
        ]
        .into_iter()
        .filter_map(|(on, name)| on.then_some(name))
        .collect();
        if !held.is_empty() {
            applied.push(format!("buttons held: {}", held.join(" + ")));
        }
        if s.fov != s0.fov {
            s.fov = s0.fov;
            applied.push(format!("fov {}", s0.fov));
        }
    }
    p.grapple_was_held = s0.input.grapple_held;
    if let Some(n) = recorded.max_grapples {
        grapple_gun::set_max_grapples(p, n);
        applied.push(format!("max_grapples {n}"));
    }
    if let Some(n) = recorded.times_grappled {
        p.script.gun.times_grappled = n;
        applied.push(format!("times_grappled {n}"));
    }
    if let Some(on) = recorded.can_grapple {
        grapple_gun::enable_grapple(p, on);
        applied.push(format!("can_grapple {on}"));
    }
    if let Some(on) = recorded.boots_enabled {
        rocket_boots::enable_rocket_boots(p, on);
        applied.push(format!("rocket_boots {on}"));
    }
    if p.script.started
        && let Some(v) = recorded.eye_height
    {
        p.script.eye_height = v;
        applied.push(format!("eye_height {v}"));
    }
    if wrote_state
        && s0.grounded
        && let Some(base) = &recorded.base
    {
        // The original's pawn has a base: its floor check leaves it where it
        // stands inside the hover band (NATIVE_PHYSICS.md 3.3).
        p.pawn.based = true;
        applied.push(format!("based (recorded base {base})"));
    }
    if !applied.is_empty() {
        notes.push(format!(
            "start state applied (tick {}): {}",
            s0.tick,
            applied.join(", ")
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use asamu_player::InputFrame;
    use asamu_player::trace::compare;

    fn scripted_inputs(n: usize) -> Vec<InputFrame> {
        (0..n)
            .map(|i| InputFrame {
                move_forward: if i < 50 { 1.0 } else { 0.0 },
                move_right: if (20..40).contains(&i) { -1.0 } else { 0.0 },
                look_yaw_delta: if i % 7 == 0 { 0.01 } else { 0.0 },
                look_pitch_delta: if i % 11 == 0 { -0.005 } else { 0.0 },
                jump_pressed: i == 30,
                jump_held: (30..45).contains(&i),
                sprint_held: (5..25).contains(&i),
                grapple_held: (60..80).contains(&i),
                ..InputFrame::default()
            })
            .collect()
    }

    /// A runtime recording on the graybox.
    fn runtime_trace(n: usize) -> Trace {
        let mut g = Game::graybox().unwrap();
        g.start();
        g.start_recording();
        for i in scripted_inputs(n) {
            g.tick(&i).unwrap();
        }
        g.stop_recording().unwrap()
    }

    #[test]
    fn replay_reproduces_a_runtime_recording_exactly() {
        let t = runtime_trace(120);
        let r = replay(&t, &ReplayOptions::default()).unwrap();
        assert_eq!(r.stopped_at_gap, None);
        assert_eq!(r.trace.samples.len(), t.samples.len());
        let d = compare(&t, &r.trace);
        assert!(d.is_exact(), "{d:?}");
        assert!(r.trace.meta.notes.iter().any(|n| n.contains("kept")));
    }

    #[test]
    fn replay_is_deterministic_and_stops_at_gaps() {
        let mut t = runtime_trace(90);
        // Teleport the start so the replay overwrites the spawn state.
        for s in &mut t.samples {
            s.position.z += 120.0;
            s.tick += 1000;
        }
        t.samples[0].grounded = false;
        let a = replay(&t, &ReplayOptions::default()).unwrap();
        let b = replay(&t, &ReplayOptions::default()).unwrap();
        assert_eq!(
            a.trace.to_jsonl_string().unwrap(),
            b.trace.to_jsonl_string().unwrap()
        );
        assert_eq!(a.trace.samples[0].tick, 1000);
        assert_eq!(a.trace.samples[0].position, t.samples[0].position);
        assert!(!a.trace.samples[0].grounded);
        assert!(
            a.trace.samples[1].position.z < t.samples[0].position.z,
            "falls"
        );

        let mid = replay(
            &t,
            &ReplayOptions {
                start_tick: Some(1050),
                max_ticks: Some(20),
                ..ReplayOptions::default()
            },
        )
        .unwrap();
        assert_eq!(mid.trace.samples.len(), 21);
        assert_eq!(mid.trace.samples[0].tick, 1050);
        assert_eq!(mid.trace.samples[0].position, t.samples[50].position);
        assert!(
            replay(
                &t,
                &ReplayOptions {
                    start_tick: Some(5),
                    ..ReplayOptions::default()
                }
            )
            .is_err()
        );

        t.samples.remove(40);
        let g = replay(&t, &ReplayOptions::default()).unwrap();
        assert_eq!(g.stopped_at_gap, Some(1039));
        assert_eq!(g.trace.samples.len(), 40);
        assert_eq!(g.trace.samples.last().unwrap().tick, 1039);
    }

    #[test]
    fn init_note_and_options() {
        let mut t = runtime_trace(10);
        t.meta.notes.push(
            "init: {\"air_control\":0.3,\"base\":null,\"ground_speed\":440.0,\"jump_z\":1000.0,\
             \"max_grapples\":3,\"physics\":1,\"rocket_boots\":true,\"sprinting\":false,\
             \"times_grappled\":1}"
                .to_owned(),
        );
        let r = replay(&t, &ReplayOptions::default()).unwrap();
        assert!(r.trace.meta.notes.iter().any(|n| n
            == "start state applied (tick 0): story mode off (GroundSpeed 440 is the \
                        walking or the sprint speed), ground_speed 440, sprinting false, \
                        air_control 0.3, jump_z 1000, max_grapples 3, times_grappled 1, \
                        rocket_boots true"));
        let no = replay(
            &t,
            &ReplayOptions {
                use_init: false,
                max_grapples: Some(-1),
                ..ReplayOptions::default()
            },
        )
        .unwrap();
        assert!(
            !no.trace
                .meta
                .notes
                .iter()
                .any(|n| n.starts_with("start state applied"))
        );
        assert!(
            no.trace
                .meta
                .notes
                .iter()
                .any(|n| n == "max_grapples override -1")
        );

        // A trace without a fixed rate: --tick-rate still replays it with
        // fixed ticks; without it, its own frame lengths are used.
        let mut v = t.clone();
        v.meta.tick_rate = None;
        let r30 = replay(
            &v,
            &ReplayOptions {
                tick_rate: Some(30.0),
                ..ReplayOptions::default()
            },
        )
        .unwrap();
        assert_eq!(r30.trace.meta.tick_rate, Some(30.0));
        assert_eq!(r30.stepping, Stepping::Fixed(30.0));
        assert_eq!(r30.variable, None);
        let own = replay(&v, &ReplayOptions::default()).unwrap();
        assert_eq!(own.stepping, Stepping::PerSample);
        assert_eq!(own.trace.meta.tick_rate, None);
        for bad in [0.0, -60.0, f64::NAN, f64::INFINITY] {
            let o = ReplayOptions {
                tick_rate: Some(bad),
                ..ReplayOptions::default()
            };
            assert!(replay(&t, &o).is_err(), "{bad}");
        }
        let p = replay(
            &t,
            &ReplayOptions {
                placeholder: true,
                ..ReplayOptions::default()
            },
        )
        .unwrap();
        assert!(p.trace.meta.notes.iter().any(|n| n.contains("placeholder")));
        let empty = Trace::new(t.meta.clone());
        assert!(replay(&empty, &ReplayOptions::default()).is_err());
        let k = ReplayOptions {
            level: ReplayLevel::Converted {
                dir: PathBuf::from("/nonexistent"),
                map: "AG-Workshop".into(),
                kismet: true,
            },
            tick_rate: Some(30.0),
            ..ReplayOptions::default()
        };
        assert!(replay(&t, &k).is_err());
    }

    /// A trace of the original standing at the graybox spawn for
    /// `inputs.len()` ticks, with the given notes. (Only the inputs and the
    /// first sample matter to a replay.)
    fn standing_original(notes: &[&str], inputs: &[InputFrame]) -> Trace {
        let g = Game::graybox().unwrap();
        let mut meta = asamu_player::TraceMeta::runtime(Some("graybox".into()), Some(60.0));
        meta.source = TraceSource::Original;
        meta.notes = notes.iter().map(|n| (*n).to_owned()).collect();
        let mut t = Trace::new(meta);
        let neutral = InputFrame::default();
        t.samples
            .push(TraceSample::capture(0, 0.0, &neutral, g.player(), g.fov()));
        for (i, input) in inputs.iter().enumerate() {
            let tick = i as u64 + 1;
            t.samples.push(TraceSample::capture(
                tick,
                tick as f64 / 60.0,
                input,
                g.player(),
                g.fov(),
            ));
        }
        t
    }

    fn state_note(ground_speed: f32, air_control: f32) -> String {
        format!(
            "state: {{\"changes\":[[0,{{\"air_control\":{air_control:?},\"air_speed\":2000.0,\
             \"boots_enabled\":false,\"can_grapple\":true,\"ground_speed\":{ground_speed:?},\
             \"jump_z\":1000.0,\"max_grapples\":2,\"physics\":1,\"sprinting\":false,\
             \"times_grappled\":0}}]],\"v\":1}}"
        )
    }

    /// Forward from tick 3, one jump press at tick 40.
    fn walk_and_jump(n: usize) -> Vec<InputFrame> {
        (1..=n)
            .map(|i| InputFrame {
                move_forward: if i >= 3 { 1.0 } else { 0.0 },
                jump_pressed: i == 40,
                jump_held: (40..50).contains(&i),
                ..InputFrame::default()
            })
            .collect()
    }

    /// The highest horizontal speed while walking.
    fn top_speed(t: &Trace) -> f32 {
        t.samples
            .iter()
            .filter(|s| s.grounded)
            .map(|s| s.velocity.truncate().length())
            .fold(0.0, f32::max)
    }

    #[test]
    fn start_state_comes_from_the_recording() {
        let has =
            |r: &ReplayResult, text: &str| r.trace.meta.notes.iter().any(|n| n.contains(text));
        // Walking speed, landed air control: a jump leaves the ground.
        let t = standing_original(&[&state_note(440.0, 0.35)], &walk_and_jump(90));
        let r = replay(&t, &ReplayOptions::default()).unwrap();
        assert_eq!(r.start_tick, 0);
        assert!(
            has(
                &r,
                "start state applied (tick 0): story mode off (GroundSpeed 440 is the walking or \
                 the sprint speed), ground_speed 440, sprinting false, air_control 0.35, jump_z \
                 1000, air_speed 2000, max_grapples 2, times_grappled 0, can_grapple true, \
                 rocket_boots false"
            ),
            "{:#?}",
            r.trace.meta.notes
        );
        assert!(has(
            &r,
            "the buttons held before the first record are unknown"
        ));
        assert!(r.trace.samples.iter().any(|s| !s.grounded), "the jump");
        assert!(
            (top_speed(&r.trace) - 440.0).abs() < 1.0,
            "{}",
            top_speed(&r.trace)
        );

        // The story speed shows story mode: the jump does nothing, the walk
        // is slower.
        let t = standing_original(&[&state_note(264.0, 0.3)], &walk_and_jump(90));
        let r = replay(&t, &ReplayOptions::default()).unwrap();
        assert!(has(
            &r,
            "story mode on (GroundSpeed 264 is the story speed)"
        ));
        assert!(r.trace.samples.iter().all(|s| s.grounded));
        assert!(
            (top_speed(&r.trace) - 264.0).abs() < 1.0,
            "{}",
            top_speed(&r.trace)
        );

        // A console speed shows nothing about story mode: said, and left as
        // the simulation starts (off) unless stated.
        let t = standing_original(&[&state_note(132.0, 0.3)], &walk_and_jump(90));
        let r = replay(&t, &ReplayOptions::default()).unwrap();
        assert!(
            has(
                &r,
                "warning: the recording does not show whether the pawn is in story mode \
                 (GroundSpeed 132 is none of the story, walking and sprint speeds 264, 440, \
                 880); left off; state it with --story-mode"
            ),
            "{:#?}",
            r.trace.meta.notes
        );
        assert!(r.trace.samples.iter().any(|s| !s.grounded));
        assert!(top_speed(&r.trace) < 133.0);
        let stated = ReplayOptions {
            story_mode: Some(true),
            ..ReplayOptions::default()
        };
        let r = replay(&t, &stated).unwrap();
        assert!(has(&r, "story mode on (--story-mode), ground_speed 132"));
        assert!(!has(&r, "does not show"));
        assert!(r.trace.samples.iter().all(|s| s.grounded));
        assert!(
            (top_speed(&r.trace) - 132.0).abs() < 1.0,
            "{}",
            top_speed(&r.trace)
        );
        // Stated the other way, it overrules what the speed shows.
        let t264 = standing_original(&[&state_note(264.0, 0.3)], &walk_and_jump(90));
        let off = ReplayOptions {
            story_mode: Some(false),
            ..ReplayOptions::default()
        };
        let r = replay(&t264, &off).unwrap();
        assert!(has(&r, "story mode off (--story-mode), ground_speed 264"));
        assert!(r.trace.samples.iter().any(|s| !s.grounded));

        // Without the recorded state the replay starts as the level does.
        let none = ReplayOptions {
            use_init: false,
            ..ReplayOptions::default()
        };
        let r = replay(&t264, &none).unwrap();
        assert!(!has(&r, "start state applied"));
        assert!((top_speed(&r.trace) - 440.0).abs() < 1.0);
        // A broken state: note is an error, not a silent fresh start.
        let broken = standing_original(&["state: {\"v\":7,\"changes\":[]}"], &walk_and_jump(5));
        assert!(replay(&broken, &ReplayOptions::default()).is_err());
    }

    #[test]
    fn replays_of_the_original_start_standing_still() {
        // Standing for 20 ticks, walking 21..=40 (sprint held from 15),
        // standing again from 41.
        let inputs: Vec<InputFrame> = (1..=60)
            .map(|i| InputFrame {
                move_forward: if (21..=40).contains(&i) { 1.0 } else { 0.0 },
                sprint_held: i >= 15,
                ..InputFrame::default()
            })
            .collect();
        let t = standing_original(&[&state_note(440.0, 0.35)], &inputs);
        let from = |tick: u64, start: StartPolicy| {
            replay(
                &t,
                &ReplayOptions {
                    start_tick: Some(tick),
                    start,
                    ..ReplayOptions::default()
                },
            )
        };
        // Inside a standing stretch: fine, with the held sprint button known.
        let r = from(18, StartPolicy::Refuse).unwrap();
        assert_eq!(r.start_tick, 18);
        assert_eq!(r.trace.samples[0].tick, 18);
        assert!(
            r.trace
                .meta
                .notes
                .iter()
                .any(|n| n.starts_with("start state applied (tick 18):")
                    && n.contains("buttons held: sprint")),
            "{:#?}",
            r.trace.meta.notes
        );
        assert!(
            !r.trace
                .meta
                .notes
                .iter()
                .any(|n| n.contains("before the first record"))
        );
        // While walking: refused, with the ticks to use instead.
        let e = from(30, StartPolicy::Refuse).unwrap_err().to_string();
        assert!(
            e.contains(
                "tick 30 of this recording is not a standing-still start (a move input is held)"
            ) && e.contains("Standing-still starts: 20 before it, 42 after it")
                && e.contains("--start snap"),
            "{e}"
        );
        // The first tick after the walk is quiet, but the tick before it is not.
        let e = from(41, StartPolicy::Refuse).unwrap_err().to_string();
        assert!(
            e.contains("the tick before it is not standing still"),
            "{e}"
        );
        let r = from(30, StartPolicy::Snap).unwrap();
        assert_eq!(r.start_tick, 42);
        assert_eq!(r.trace.samples.len(), 19);
        assert!(
            r.trace.meta.notes.iter().any(|n| n
                == "start: moved from tick 30 (a move input is held) to the next \
                    standing-still tick 42"),
            "{:#?}",
            r.trace.meta.notes
        );
        let r = from(30, StartPolicy::Force).unwrap();
        assert_eq!(r.start_tick, 30);
        assert!(
            r.trace
                .meta
                .notes
                .iter()
                .any(|n| n.starts_with("warning: forced start at tick 30"))
        );
        // Nothing to move to.
        let mut moving = t.clone();
        for s in &mut moving.samples[50..] {
            s.input.move_right = 1.0;
        }
        let e = replay(
            &moving,
            &ReplayOptions {
                start_tick: Some(55),
                start: StartPolicy::Snap,
                ..ReplayOptions::default()
            },
        )
        .unwrap_err();
        assert!(
            e.to_string()
                .contains("no standing-still start at or after tick 55")
        );
        // Our own traces start anywhere, as before.
        let mut ours = t.clone();
        ours.meta.source = TraceSource::Runtime;
        let r = replay(
            &ours,
            &ReplayOptions {
                start_tick: Some(30),
                ..ReplayOptions::default()
            },
        )
        .unwrap();
        assert_eq!(r.start_tick, 30);
    }

    /// Scripted inputs with the given frame lengths, recorded on the
    /// stepper: a variable-rate trace (`tick_rate: null`) as `convert`
    /// writes it (time = the f64 sum of the f32 lengths). Each tick is
    /// stepped with the length its two sample times give back, which is the
    /// given `f32` unless it is too small to change the sum.
    fn variable_trace(lengths: &[f32]) -> Trace {
        let game = Game::graybox().unwrap();
        let mut sim = VariableStepper::from_game(&game);
        let mut meta = asamu_player::TraceMeta::runtime(Some("graybox".into()), None);
        meta.notes.push("made by the replay tests".into());
        let mut t = Trace::new(meta);
        t.samples.push(TraceSample::capture(
            0,
            0.0,
            &InputFrame::default(),
            sim.player(),
            sim.fov(),
        ));
        let mut time = 0.0_f64;
        for (i, (input, dt)) in scripted_inputs(lengths.len())
            .iter()
            .zip(lengths)
            .enumerate()
        {
            let next = time + f64::from(*dt);
            sim.tick(input, FrameLength::of(next - time).dt(), next);
            time = next;
            t.samples.push(TraceSample::capture(
                i as u64 + 1,
                time,
                input,
                sim.player(),
                sim.fov(),
            ));
        }
        t
    }

    /// Frame lengths between 12 and 21 ms, never two alike in a row.
    fn uneven(n: usize) -> Vec<f32> {
        (0..n)
            .map(|i| 0.012 + 0.0011 * ((i * 7) % 9) as f32)
            .collect()
    }

    #[test]
    fn fixed_rate_traces_keep_the_fixed_path() {
        let t = runtime_trace(120);
        let r = replay(&t, &ReplayOptions::default()).unwrap();
        assert_eq!(r.stepping, Stepping::Fixed(60.0));
        assert_eq!(r.variable, None);
        assert_eq!(r.trace.meta.tick_rate, Some(60.0));
        // Sample for sample (times included) what the game itself recorded.
        assert_eq!(r.trace.samples, t.samples);
        let notes = &r.trace.meta.notes;
        assert!(
            notes
                .iter()
                .any(|n| n == "asamu-trace replay of a runtime trace on graybox at 60 Hz"),
            "{notes:#?}"
        );
        assert!(notes.iter().any(|n| n == "recorded by asamu-game"));
        assert!(!notes.iter().any(|n| n.starts_with("stepping:")));
    }

    /// A trace whose frames all have the same length replays identically
    /// with per-sample lengths and with the fixed tick: forced for a
    /// fixed-rate trace, and by default once the rate is removed.
    #[test]
    fn equal_lengths_replay_like_the_fixed_tick() {
        let mut t = runtime_trace(400);
        // Start elsewhere, falling, so the replay writes the initial state.
        for s in &mut t.samples {
            s.position.z += 80.0;
        }
        t.samples[0].grounded = false;
        let fixed = replay(&t, &ReplayOptions::default()).unwrap();
        let forced = replay(
            &t,
            &ReplayOptions {
                variable_dt: true,
                ..ReplayOptions::default()
            },
        )
        .unwrap();
        let mut v = t.clone();
        v.meta.tick_rate = None;
        let own = replay(&v, &ReplayOptions::default()).unwrap();
        for r in [&forced, &own] {
            assert_eq!(r.stepping, Stepping::PerSample);
            assert_eq!(r.trace.meta.tick_rate, None);
            assert_eq!(r.trace.samples, fixed.trace.samples);
            let var = r.variable.unwrap();
            assert_eq!((var.clamped, var.no_op), (0, 0));
            assert_eq!(var.lengths.steps, 400);
            assert!(var.lengths.uniform());
            assert!((var.lengths.mean - 1.0 / 60.0).abs() < 1e-9);
        }
        assert!(
            fixed
                .trace
                .samples
                .iter()
                .any(|s| s.position != fixed.trace.samples[0].position)
        );
        // Segment replays agree too.
        let seg = |variable_dt| {
            replay(
                &t,
                &ReplayOptions {
                    variable_dt,
                    start_tick: Some(150),
                    max_ticks: Some(100),
                    ..ReplayOptions::default()
                },
            )
            .unwrap()
        };
        let (a, b) = (seg(false), seg(true));
        assert_eq!(a.trace.samples.len(), 101);
        for (x, y) in a.trace.samples.iter().zip(&b.trace.samples) {
            // The fixed path's clock restarts at 0; ours keeps the input's
            // times.
            assert_eq!(
                TraceSample { time: 0.0, ..*x },
                TraceSample { time: 0.0, ..*y }
            );
        }
        assert_eq!(b.trace.samples[0].time, t.samples[150].time);
        // The placeholder model too.
        let p = |variable_dt| {
            replay(
                &t,
                &ReplayOptions {
                    variable_dt,
                    placeholder: true,
                    ..ReplayOptions::default()
                },
            )
            .unwrap()
        };
        assert_eq!(p(true).trace.samples, p(false).trace.samples);
        // Both at once is refused.
        assert!(
            replay(
                &t,
                &ReplayOptions {
                    variable_dt: true,
                    tick_rate: Some(60.0),
                    ..ReplayOptions::default()
                }
            )
            .is_err()
        );
    }

    #[test]
    fn variable_rate_trace_replays_with_its_own_frame_lengths() {
        let lengths = uneven(300);
        let t = variable_trace(&lengths);
        assert_eq!(t.meta.tick_rate, None);
        let r = replay(&t, &ReplayOptions::default()).unwrap();
        assert_eq!(r.stepping, Stepping::PerSample);
        assert_eq!(r.stopped_at_gap, None);
        // Exactly the recording: every tick used the recorded f32 length.
        assert_eq!(r.trace.samples, t.samples);
        assert!(compare(&t, &r.trace).is_exact());
        let var = r.variable.unwrap();
        assert_eq!(var.lengths.steps, 300);
        assert!((var.lengths.min - 0.012).abs() < 1e-6, "{var:?}");
        assert!((var.lengths.max - 0.0208).abs() < 1e-6, "{var:?}");
        assert!(!var.lengths.uniform());
        let notes = &r.trace.meta.notes;
        assert!(
            notes.iter().any(|n| n.starts_with(
                "asamu-trace replay of a runtime trace on graybox with each sample's own frame \
                 length: 300 ticks, 0.012000..0.020800 s (mean 0.016400 s)"
            )),
            "{notes:#?}"
        );
        assert!(notes.iter().any(|n| n.starts_with("stepping: per-sample")));
        assert!(
            notes
                .iter()
                .any(|n| n == "recorded by asamu-trace (per-sample stepper)")
        );
        assert!(!notes.iter().any(|n| n == "recorded by asamu-game"));
        assert!(notes.iter().any(|n| n == "movement model: ue3_pawn"));
        assert!(!notes.iter().any(|n| n.contains("converted level")));
        assert!(!notes.iter().any(|n| n.starts_with("warning:")));

        // Deterministic, also through the file format.
        let again = replay(&t, &ReplayOptions::default()).unwrap();
        let text = r.trace.to_jsonl_string().unwrap();
        assert_eq!(text, again.trace.to_jsonl_string().unwrap());
        let reread = Trace::from_jsonl_str(&t.to_jsonl_string().unwrap()).unwrap();
        assert_eq!(
            replay(&reread, &ReplayOptions::default())
                .unwrap()
                .trace
                .to_jsonl_string()
                .unwrap(),
            text
        );
        // A replay of our replay is the same run again.
        let twice = replay(&r.trace, &ReplayOptions::default()).unwrap();
        assert_eq!(twice.trace.samples, r.trace.samples);

        // The frame lengths matter: fixed 60 Hz ticks give another run.
        let fixed = replay(
            &t,
            &ReplayOptions {
                tick_rate: Some(60.0),
                ..ReplayOptions::default()
            },
        )
        .unwrap();
        assert_eq!(fixed.stepping, Stepping::Fixed(60.0));
        assert_eq!(fixed.trace.meta.tick_rate, Some(60.0));
        assert!(!compare(&t, &fixed.trace).is_exact());

        // Segments, gaps and tick numbers work as in the fixed path.
        let mut shifted = t.clone();
        for s in &mut shifted.samples {
            s.tick += 5000;
            s.time += 83.25;
        }
        let seg = replay(
            &shifted,
            &ReplayOptions {
                start_tick: Some(5100),
                max_ticks: Some(50),
                ..ReplayOptions::default()
            },
        )
        .unwrap();
        assert_eq!(seg.trace.samples.len(), 51);
        assert_eq!(seg.trace.samples[0].tick, 5100);
        assert_eq!(seg.trace.samples[0].position, t.samples[100].position);
        assert_eq!(seg.trace.samples[50].time, shifted.samples[150].time);
        assert_eq!(seg.variable.unwrap().lengths.steps, 50);
        shifted.samples.remove(40);
        let gap = replay(&shifted, &ReplayOptions::default()).unwrap();
        assert_eq!(gap.stopped_at_gap, Some(5039));
        assert_eq!(gap.trace.samples.len(), 40);
        assert_eq!(gap.variable.unwrap().lengths.steps, 39);
        // One sample: nothing to step.
        let mut one = t.clone();
        one.samples.truncate(1);
        let r1 = replay(&one, &ReplayOptions::default()).unwrap();
        assert_eq!(r1.trace.samples.len(), 1);
        assert_eq!(r1.variable.unwrap().lengths.steps, 0);
    }

    #[test]
    fn frame_length_extremes() {
        // Long frames: simulated as MAX_STEP_DT, counted and noted.
        let mut lengths = uneven(60);
        lengths[20] = 0.4;
        lengths[45] = 3.0;
        let mut t = variable_trace(&lengths);
        let r = replay(&t, &ReplayOptions::default()).unwrap();
        // The recording's stepper clamped the same way.
        assert_eq!(r.trace.samples, t.samples);
        let var = r.variable.unwrap();
        assert_eq!((var.clamped, var.first_clamped), (2, Some(21)));
        assert_eq!(var.no_op, 0);
        assert_eq!(var.lengths.max, f64::from(3.0_f32));
        let notes = &r.trace.meta.notes;
        assert!(
            notes.iter().any(|n| n
                == "warning: 2 tick(s) longer than 0.25 s were simulated as 0.25 s (our step \
                    bound; first at tick 21)"),
            "{notes:#?}"
        );

        // Times that stand still or run backwards: ticks without time.
        let n = t.samples.len();
        t.samples[10].time = t.samples[9].time;
        t.samples[30].time = t.samples[29].time - 1.0;
        let r = replay(&t, &ReplayOptions::default()).unwrap();
        assert_eq!(r.trace.samples.len(), n);
        let var = r.variable.unwrap();
        // Tick 10 (zero), tick 30 (negative); ticks 11 and 31 get longer.
        assert_eq!((var.no_op, var.first_no_op), (2, Some(10)));
        assert_eq!(r.trace.samples[10].position, r.trace.samples[9].position);
        assert_eq!(r.trace.samples[10].yaw, r.trace.samples[9].yaw);
        assert_eq!(r.trace.samples[10].time, t.samples[10].time);
        assert!(
            r.trace
                .meta
                .notes
                .iter()
                .any(|n| n
                    .starts_with("warning: 2 tick(s) have a frame length that is not positive")),
            "{:#?}",
            r.trace.meta.notes
        );
        let again = replay(&t, &ReplayOptions::default()).unwrap();
        assert_eq!(again.trace.samples, r.trace.samples);

        // Tiny frames (down to one too small to change the time sum, which
        // therefore is a tick without time) and huge time values: finite,
        // deterministic, no panic.
        let tiny: Vec<f32> = (0..80)
            .map(|i| [1e-6_f32, 3e-4, 1e-9, 5e-4, 1e-38][i % 5])
            .collect();
        let mut t = variable_trace(&tiny);
        let r = replay(&t, &ReplayOptions::default()).unwrap();
        assert_eq!(r.trace.samples, t.samples);
        let var = r.variable.unwrap();
        assert_eq!((var.no_op, var.first_no_op, var.clamped), (16, Some(5), 0));
        assert!((var.lengths.max - 5e-4).abs() < 1e-9, "{var:?}");
        assert!(r.trace.samples.iter().all(|s| s.position.is_finite()));
        assert!(
            r.trace
                .samples
                .iter()
                .any(|s| s.position != r.trace.samples[0].position),
            "tiny frames still move the pawn"
        );
        t.samples[40].time = 1e-320;
        t.samples[41].time = 1e308;
        t.samples[42].time = -1e308;
        t.samples[43].time = 1e308;
        let r = replay(&t, &ReplayOptions::default()).unwrap();
        assert_eq!(r.trace.samples.len(), 81);
        assert!(r.trace.samples.iter().all(|s| s.position.is_finite()));
        let var = r.variable.unwrap();
        assert!(var.clamped >= 2 && var.no_op >= 2, "{var:?}");
        assert!(var.lengths.max.is_finite() && var.lengths.min.is_finite());
        r.trace.to_jsonl_string().unwrap();

        // No time information at all: an error that names the way out.
        let mut frozen = variable_trace(&uneven(20));
        for s in &mut frozen.samples {
            s.time = 0.0;
        }
        let e = replay(&frozen, &ReplayOptions::default()).unwrap_err();
        assert!(e.to_string().contains("--tick-rate"), "{e}");
        assert!(
            replay(
                &frozen,
                &ReplayOptions {
                    tick_rate: Some(60.0),
                    ..ReplayOptions::default()
                }
            )
            .is_ok()
        );
    }

    #[test]
    fn base_actor_comparison_and_event_notes() {
        let named = |name: Option<&str>, world_geometry| SurfaceText {
            text: String::new(),
            name: name.map(str::to_owned),
            world_geometry,
        };
        let cmp = |recorded, ours: &SurfaceText| base_comparison(recorded, ours);
        assert!(cmp(None, &named(Some("A_1"), false)).contains("names no base actor"));
        assert!(cmp(Some("A_1"), &named(Some("A_1"), false)).contains("the same name (A_1"));
        assert!(cmp(Some("A_1"), &named(Some("A_2"), false)).ends_with("is A_1: another actor"));
        assert!(
            cmp(Some("WorldInfo_6"), &named(None, true)).ends_with("world geometry on both sides")
        );
        assert!(
            cmp(Some("StaticMeshActor_3"), &named(None, true))
                .ends_with("an actor, where ours is world geometry")
        );
        assert!(cmp(Some("A_1"), &named(None, false)).ends_with("not comparable)"));
        // A hand-made level has no actor names.
        let game = Game::graybox().unwrap();
        let mut names = ActorNames::new(&ReplayLevel::Graybox, &game);
        assert_eq!(names.name(7), None);
        let world = surface_text(&Surface::default(), &mut names);
        assert!(world.world_geometry && world.text == "world geometry (no actor)");
        // A converted directory that does not exist: no names, no panic.
        let mut missing = ActorNames {
            source: Some((PathBuf::from("/nonexistent"), vec!["AG-Workshop".into()])),
            levels: BTreeMap::new(),
        };
        assert_eq!(missing.name(3), None);
        assert_eq!(
            missing.name(u32::MAX),
            None,
            "a level the map does not have"
        );

        // The notes about events, by what the replay did.
        let event = |tick, kind, action| CrossedEvent { tick, kind, action };
        let mut notes = Vec::new();
        event_notes(
            &[
                event(40, EventKind::StoryModeOn, EventAction::Stopped),
                event(40, EventKind::Teleport, EventAction::Stopped),
            ],
            Some(40),
            39,
            120,
            &mut notes,
        );
        assert_eq!(
            notes,
            [
                "validity: stopped before tick 40 (story mode on, teleport): a replay of the \
              inputs cannot reproduce it. 39 of 120 tick(s) replayed; --level-events inject \
              takes the recorded change and runs on, ignore runs through"
            ]
        );
        notes.clear();
        let many: Vec<CrossedEvent> = (0..30)
            .map(|i| event(100 + i, EventKind::Teleport, EventAction::LeftOut))
            .collect();
        event_notes(&many, None, 0, 0, &mut notes);
        assert_eq!(notes.len(), 1);
        assert!(
            notes[0].starts_with("validity: 30 event(s) the inputs cannot reproduce")
                && notes[0].ends_with("119 teleport and 10 more"),
            "{notes:#?}"
        );
        notes.clear();
        event_notes(&[], None, 5, 5, &mut notes);
        assert!(notes.is_empty());
        assert_eq!(bounded(&[]), "");
    }

    /// A state: note that cannot be read is an error when the start state
    /// is asked for, and a warning when it is not.
    #[test]
    fn unreadable_state_note() {
        let broken = standing_original(&["state: {\"v\":7,\"changes\":[]}"], &walk_and_jump(5));
        assert!(replay(&broken, &ReplayOptions::default()).is_err());
        let r = replay(
            &broken,
            &ReplayOptions {
                use_init: false,
                ..ReplayOptions::default()
            },
        )
        .unwrap();
        assert_eq!(r.trace.samples.len(), 6);
        assert!(
            r.trace
                .meta
                .notes
                .iter()
                .any(|n| n.starts_with("warning: the trace's state: note is not readable")),
            "{:#?}",
            r.trace.meta.notes
        );
        // A one-step replay of a trace without any state note says what it
        // resynchronises.
        let bare = standing_original(&[], &walk_and_jump(30));
        let r = replay(
            &bare,
            &ReplayOptions {
                one_step: true,
                ..ReplayOptions::default()
            },
        )
        .unwrap();
        assert!(r.one_step);
        assert!(
            r.trace
                .meta
                .notes
                .iter()
                .any(|n| n.contains("and no script state (the trace has no state: note)")),
            "{:#?}",
            r.trace.meta.notes
        );
        // Without the recorded start state the script state is not written
        // on later ticks either.
        let stated = standing_original(&[&state_note(264.0, 0.3)], &walk_and_jump(30));
        let off = replay(
            &stated,
            &ReplayOptions {
                one_step: true,
                use_init: false,
                ..ReplayOptions::default()
            },
        )
        .unwrap();
        assert!(
            off.trace
                .meta
                .notes
                .iter()
                .any(|n| n.contains("no script state (the recorded state is switched off)")),
            "{:#?}",
            off.trace.meta.notes
        );
        assert!(
            off.trace.meta.notes.iter().any(|n| n.starts_with(
                "state check: GroundSpeed differs from the recording's after 30 of 30 tick(s) \
                 (first at tick 1: ours 440, recorded 264)"
            )),
            "{:#?}",
            off.trace.meta.notes
        );
        let on = replay(
            &stated,
            &ReplayOptions {
                one_step: true,
                ..ReplayOptions::default()
            },
        )
        .unwrap();
        assert!(
            on.trace
                .meta
                .notes
                .iter()
                .any(|n| n.contains("our GroundSpeed, AirControl")),
            "{:#?}",
            on.trace.meta.notes
        );
        // Every sample of this "original" stands at the spawn point while
        // its inputs walk: each one-step tick starts there again.
        let start = bare.samples[0].position;
        assert!(
            r.trace
                .samples
                .iter()
                .all(|s| (s.position - start).length() < 10.0)
        );
    }

    #[test]
    fn per_sample_replays_refuse_kismet() {
        let t = variable_trace(&uneven(10));
        let k = ReplayOptions {
            level: ReplayLevel::Converted {
                dir: PathBuf::from("/nonexistent"),
                map: "AG-Workshop".into(),
                kismet: true,
            },
            ..ReplayOptions::default()
        };
        let e = replay(&t, &k).unwrap_err();
        assert!(e.to_string().contains("cannot run the map's Kismet"), "{e}");
        // Without Kismet it gets as far as loading the level.
        let c = ReplayOptions {
            level: ReplayLevel::Converted {
                dir: PathBuf::from("/nonexistent"),
                map: "AG-Workshop".into(),
                kismet: false,
            },
            ..ReplayOptions::default()
        };
        let e = replay(&t, &c).unwrap_err();
        assert!(format!("{e:#}").contains("loading AG-Workshop"), "{e:#}");
    }
}
