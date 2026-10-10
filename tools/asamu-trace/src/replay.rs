//! Replays a trace's inputs through our simulation and records our trace.
//!
//! The replay builds an [`asamu_game::Game`] on the graybox level or on a
//! level converted by `asamu-import` (optionally with the map's Kismet,
//! [`asamu_game::load_level_with_kismet`]), puts the pawn in the trace's
//! state at the start tick (position, velocity, yaw, pitch, walking or
//! falling from the sample; the recorded script state from the `state:`
//! note, see "Start state" below) and then feeds every later sample's
//! input, one tick each. It stops at the first tick gap. The result is a
//! runtime trace aligned tick for tick with the input trace, ready for
//! [`crate::compare`].
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
//! | button levels of the tick before (for press and release edges) | the start sample's own input |
//! | FOV | the start sample's FOV |
//!
//! Not recorded, so left as our simulation starts them: "sprint after
//! landing", the pawn's and the power jump's state code (jump-release
//! damping, a charging power jump, a running zoom), zoom availability, the
//! gun's timers and attachment, the boots' boost, the eye height's
//! smoothing (the raw record has `EyeHeight`; it changes on most frames and
//! is not in the timeline), the floor the pawn is based on. All of these
//! are at rest when the pawn stands still with no button held, so a replay
//! of an original recording starts only at such a tick
//! ([`crate::segments`], [`StartPolicy`]): a start in the air, while moving
//! or while the grapple is attached is refused (the error names the
//! standing-still ticks before and after it), moved forward to the next
//! standing-still tick, or forced. A trace converted before the `state:`
//! note existed has an `init:` note for its first sample only.
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
//! the attachment. With `--kismet` the map's Kismet always starts from level
//! start, also for a segment replay (`start_tick`), and its level-start
//! actions may override the start state; the notes say so.

use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use asamu_core::DEFAULT_TICK_RATE_HZ;
use asamu_core::rotator::wrap_radians;
use asamu_game::{Game, LevelScript, load_level_with_kismet};
use asamu_player::pawn::SprintState;
use asamu_player::trace::{TraceGrappleState, TraceSample, TraceSource};
use asamu_player::{
    InputFrame, MAX_STEP_DT, MovementModelKind, PawnPhysicsState, PlayerParams, Trace, grapple_gun,
    pawn, rocket_boots,
};

use crate::convert::InitState;
use crate::segments::{is_clean_start, next_clean_start, not_clean_reasons, previous_clean_start};
use crate::state::{StateTimeline, TickState, story_mode_shown};
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
    /// note, the start sample's button levels and FOV).
    pub use_init: bool,
    /// Story mode at the start, when the recording does not show it or is
    /// to be overruled.
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

/// Replays `original` (see the module docs).
///
/// # Errors
/// Empty trace, contradictory or invalid stepping options, a trace without a
/// fixed rate whose sample times never advance, a per-sample replay with
/// Kismet, level loading or game construction errors.
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
    if original.meta.source == TraceSource::Original
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
    let (mut game, mut script, mut notes) = match stepping {
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

    // Initial state.
    {
        let p = game.player_mut();
        // A yaw already in [−π, π) is taken as it is (wrapping it again can
        // move it by one bit).
        let yaw = if (-core::f32::consts::PI..core::f32::consts::PI).contains(&s0.yaw) {
            s0.yaw
        } else {
            wrap_radians(s0.yaw)
        };
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
        if s0.grapple_state == TraceGrappleState::Attached {
            notes.push(
                "warning: the trace starts attached; the attachment is not recreated".to_owned(),
            );
        }
    }
    if opts.use_init {
        let first_tick = original.samples.first().map_or(s0.tick, |f| f.tick);
        let recorded = match StateTimeline::from_notes(&original.meta.notes)? {
            Some(timeline) => timeline.at(s0.tick)?,
            None => InitState::from_notes(&original.meta.notes).map(|init| {
                if s0.tick != first_tick {
                    notes.push(
                        "note: the init: state belongs to the first sample, not the start tick"
                            .to_owned(),
                    );
                }
                TickState::from(&init)
            }),
        };
        if original.meta.source == TraceSource::Original && s0.tick == first_tick {
            notes.push(
                "note: the buttons held before the first record are unknown (taken as released)"
                    .to_owned(),
            );
        }
        apply_start_state(
            &mut game,
            &recorded.unwrap_or_default(),
            s0,
            opts.story_mode,
            &mut notes,
        );
    }
    if let Some(n) = opts.max_grapples {
        grapple_gun::set_max_grapples(game.player_mut(), n);
        notes.push(format!("max_grapples override {n}"));
    }

    game.start();
    game.start_recording();
    let (mut trace, stopped_at_gap, respawns, variable) = match stepping {
        Stepping::Fixed(_) => {
            let respawns_before = game.respawn_count();
            let mut stopped_at_gap = None;
            let mut prev_tick = s0.tick;
            for s in samples.iter().skip(1) {
                if prev_tick.checked_add(1) != Some(s.tick) {
                    stopped_at_gap = Some(prev_tick);
                    break;
                }
                let ticked = match script.as_mut() {
                    Some(sc) => sc.tick(&mut game, &s.input).is_some(),
                    None => game.tick(&s.input).is_some(),
                };
                if !ticked {
                    bail!("the game did not tick at input tick {}", s.tick);
                }
                prev_tick = s.tick;
            }
            let respawns = game.respawn_count().saturating_sub(respawns_before);
            let mut trace = game.stop_recording().context("recording was not running")?;
            // Our ticks count from 0; give them the input trace's numbers.
            let base = s0.tick;
            for s in &mut trace.samples {
                s.tick = s.tick.checked_add(base).context("tick numbers overflow")?;
            }
            (trace, stopped_at_gap, respawns, None)
        }
        Stepping::PerSample => {
            // The game's recording gives the meta line (level, model and
            // parameter notes); the samples come from the stepper.
            let mut trace = game.stop_recording().context("recording was not running")?;
            trace.meta.tick_rate = None;
            trace.samples.clear();
            for n in &mut trace.meta.notes {
                if n == "recorded by asamu-game" {
                    *n = "recorded by asamu-trace (per-sample stepper)".to_owned();
                }
            }
            let (stopped_at_gap, respawns, variable) =
                run_per_sample(&game, samples, &mut trace, &mut notes);
            (trace, stopped_at_gap, respawns, Some(variable))
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
    trace.meta.notes.extend(notes);
    trace.validate()?;
    Ok(ReplayResult {
        trace,
        start_tick: s0.tick,
        stopped_at_gap,
        respawns,
        stepping,
        variable,
    })
}

fn on_off(on: bool) -> &'static str {
    if on { "on" } else { "off" }
}

/// Puts the script state `recorded` (the original's at the start tick), the
/// button levels of the start sample `s0` and its FOV into the game's player
/// (see the module docs, "Start state"). Writes what it set to `notes`.
fn apply_start_state(
    game: &mut Game,
    recorded: &TickState,
    s0: &TraceSample,
    story_mode: Option<bool>,
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
    if !applied.is_empty() {
        notes.push(format!(
            "start state applied (tick {}): {}",
            s0.tick,
            applied.join(", ")
        ));
    }
}

/// The per-sample loop: steps a [`VariableStepper`] snapshot of `game` with
/// every sample's input and frame length, writes our samples (with the input
/// samples' ticks and times) to `trace` and the stepping notes to `notes`.
/// Returns the gap that stopped it, the respawn count and the frame-length
/// summary.
fn run_per_sample(
    game: &Game,
    samples: &[TraceSample],
    trace: &mut Trace,
    notes: &mut Vec<String>,
) -> (Option<u64>, u32, VariableSteps) {
    let mut sim = VariableStepper::from_game(game);
    notes.push(
        "stepping: per-sample frame lengths on asamu-trace's stepper (a rebuild of Game::tick \
         from the public simulation API; asamu-game has no tick that takes a frame length)"
            .to_owned(),
    );
    if sim.is_converted() {
        notes.push(
            "note: on a converted level this stepper simulates the player and the level objects \
             (recharge crystals, glow flowers, interactables, attractor pads) only; touch \
             volumes, checkpoints, kill zones, deaths and respawns, falling rocks, level \
             streaming, NPCs and Kismet are not simulated"
                .to_owned(),
        );
    }
    let respawns_before = sim.respawn_count();
    let mut variable = VariableSteps {
        lengths: StepStats::default(),
        clamped: 0,
        first_clamped: None,
        no_op: 0,
        first_no_op: None,
    };
    let mut lengths = Vec::new();
    let mut stopped_at_gap = None;
    let Some(s0) = samples.first() else {
        return (None, 0, variable);
    };
    trace.samples.push(TraceSample::capture(
        s0.tick,
        s0.time,
        &InputFrame::default(),
        sim.player(),
        sim.fov(),
    ));
    let mut prev = s0;
    for s in samples.iter().skip(1) {
        if prev.tick.checked_add(1) != Some(s.tick) {
            stopped_at_gap = Some(prev.tick);
            break;
        }
        let seconds = s.time - prev.time;
        let length = FrameLength::of(seconds);
        match length {
            FrameLength::Step(_) => {}
            FrameLength::Clamped => {
                variable.clamped += 1;
                variable.first_clamped.get_or_insert(s.tick);
            }
            FrameLength::NoOp => {
                variable.no_op += 1;
                variable.first_no_op.get_or_insert(s.tick);
            }
        }
        lengths.push(seconds);
        let report = sim.tick(&s.input, length.dt(), finite(s.time - s0.time));
        if report.respawned {
            notes.push(format!("respawn (kill_z) at tick {}", s.tick));
        }
        trace.samples.push(TraceSample::capture(
            s.tick,
            s.time,
            &s.input,
            sim.player(),
            sim.fov(),
        ));
        prev = s;
    }
    variable.lengths = StepStats::of_lengths(lengths);
    if let Some(t) = variable.first_clamped {
        notes.push(format!(
            "warning: {} tick(s) longer than {MAX_STEP_DT} s were simulated as {MAX_STEP_DT} s \
             (our step bound; first at tick {t})",
            variable.clamped
        ));
    }
    if let Some(t) = variable.first_no_op {
        notes.push(format!(
            "warning: {} tick(s) have a frame length that is not positive; no time passed in \
             them (first at tick {t})",
            variable.no_op
        ));
    }
    let respawns = sim.respawn_count().saturating_sub(respawns_before);
    (stopped_at_gap, respawns, variable)
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
