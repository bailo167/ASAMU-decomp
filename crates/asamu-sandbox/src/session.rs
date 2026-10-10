//! The Sandbox session.
//!
//! A [`Session`] owns the profile in use, the effective parameter set
//! (Classic plus the overlay), the time-control model, the save-state slots,
//! the rewind ring, the telemetry and an action log. It does not own the
//! game: the host keeps its `Game` and level script and lends them through
//! a [`SimCx`].
//!
//! How a host uses it, around its own unchanged tick:
//!
//! 1. [`Session::reconcile`] makes the game run the session's parameters
//!    whenever it does not (a freshly loaded level, a restored save state);
//! 2. [`Session::execute`] is the **single mutation entry point**: every
//!    hotkey and button becomes a [`Command`];
//! 3. [`Session::before_tick`] enforces the sticky rules before each tick;
//! 4. the host ticks the game exactly as Classic does;
//! 5. [`Session::after_tick`] observes the result.
//!
//! A pristine session (no override, default rules, no command, time control
//! never used) leaves a game bit-identical to one that never met a session;
//! the guard tests in `tests/classic_guard.rs` hold it to that.
//!
//! # What a command may touch
//!
//! Every simulation-changing command goes through an existing public API of
//! the game (`Game::set_params` by way of [`retune`], `set_max_grapples`,
//! `enable_rocket_boots`, `enter_story_mode`, `respawn`, `kill_player`,
//! `start_recording`, ...) or, for teleports and fly placement, through
//! `Game::player_mut` with the same recipe the app's debug teleport uses.
//! No command reimplements physics, the grapple or world logic. A command
//! that is refused changes nothing, neither in the session nor in the game.
//!
//! Every executed command is logged with the tick it ran before
//! ([`ActionLog`]); a Sandbox recording carries the log in its header.
//!
//! # Where a game is written
//!
//! Four entry points write the borrowed game, and nothing else in this
//! crate does: [`Session::execute`] (commands), [`Session::reconcile`] (the
//! session's parameter set, only when the game runs another),
//! [`Session::before_tick`] (the sticky rules, only where the game differs)
//! and [`Session::fly_move`] (the placement of fly mode, only while it is
//! on). [`Session::after_tick`] and every accessor only read. A session
//! never ticks the game.

use std::collections::BTreeMap;

use asamu_core::DEFAULT_TICK_RATE_HZ;
use asamu_core::rotator::wrap_radians;
use asamu_game::{Game, GameError, LevelScript, SetParamsError, TickReport};
use asamu_player::world::{CONTACT_SKIN, CollisionShape};
use asamu_player::{PlayerParams, grapple_gun, rocket_boots};
use asamu_world::rotation::{normalize_axis, units_to_radians};
use asamu_world::{Level, SpawnPoint};
use glam::Vec3;
use serde::{Deserialize, Serialize};

use crate::command::{Command, CommandError, SlotOp, TeleportTarget, TimeOp, Toggle};
use crate::inspect::{TeleportTargetInfo, format_value};
use crate::keys::{Catalog, Effect, effect_of};
use crate::overlay::{BASE_CLASSIC, Overlay, OverlayError, ParamSetLabel, param_set_label};
use crate::profile::{PROFILE_FORMAT, PROFILE_VERSION, Profile};
use crate::recording::SandboxRecording;
use crate::relatch::{RelatchReport, retune};
use crate::rules::{GrappleRule, Rules, Switch};
use crate::snapshot::{RewindRing, SimSnapshot, Slots};
use crate::telemetry::Telemetry;
use crate::time::{MAX_SPEED, MIN_SPEED, TimeControl};

/// Most actions an [`ActionLog`] keeps; later ones are counted as truncated.
pub const ACTION_LOG_CAP: usize = 4096;

/// Ticks between two rewind keyframes (ours): half a second at the default
/// tick rate.
pub const REWIND_INTERVAL_TICKS: u32 = 30;
/// Rewind keyframes kept (ours): one minute at the default tick rate.
pub const REWIND_KEYFRAMES: usize = 120;

/// Most bookmarks a session keeps (a bound, not a feature limit of note).
pub const MAX_MARKS: usize = 32;
/// Longest bookmark name, bytes.
pub const MAX_MARK_NAME: usize = 40;

/// Teleports and fly placement keep every coordinate within this distance
/// of the origin, UU (ours; far beyond any level, and small enough that the
/// simulation's arithmetic stays well-behaved there).
pub const PLACEMENT_LIMIT: f32 = 1.0e6;

/// The host's simulation, lent to the session for one call.
#[derive(Debug)]
pub struct SimCx<'a> {
    /// The running game.
    pub game: &'a mut Game,
    /// Its level script (`None`: hand-made level, or a map without Kismet).
    pub script: &'a mut Option<LevelScript>,
}

/// What a command did, for the host.
#[derive(Debug, Default)]
pub struct Outcome {
    /// One line for the notice area.
    pub message: String,
    /// The parameter set changed: the host refreshes its banner.
    pub params_changed: bool,
    /// The player or the whole simulation jumped (teleport, respawn, save
    /// state): the host snaps its render interpolation.
    pub discontinuity: bool,
    /// A recording finished: the host writes it to disk.
    pub recording: Option<SandboxRecording>,
}

impl Outcome {
    fn say(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            ..Self::default()
        }
    }

    fn jump(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            discontinuity: true,
            ..Self::default()
        }
    }
}

/// Why a session could not be created or reconciled.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum SessionError {
    /// The profile names a base other than [`BASE_CLASSIC`].
    #[error("unknown base {0:?} (supported: {BASE_CLASSIC:?})")]
    UnknownBase(String),
    /// The game does not run the script-layer pipeline the Classic base
    /// needs (a placeholder-parameter game).
    #[error("this game runs the placeholder parameters; the Sandbox needs the Classic set")]
    BaseMismatch,
    /// An override of the profile does not apply.
    #[error(transparent)]
    Overlay(#[from] OverlayError),
    /// The game refused the parameter set.
    #[error(transparent)]
    Params(#[from] SetParamsError),
}

/// One executed command and the tick it ran before.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LoggedAction {
    /// Tick count of the game when the command ran.
    pub tick: u64,
    /// The command.
    pub cmd: Command,
}

/// The session's tick-stamped command log, capped at [`ACTION_LOG_CAP`].
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ActionLog {
    actions: Vec<LoggedAction>,
    truncated: bool,
}

impl ActionLog {
    /// The logged actions, oldest first.
    #[must_use]
    pub fn actions(&self) -> &[LoggedAction] {
        &self.actions
    }

    /// Number of logged actions.
    #[must_use]
    pub fn len(&self) -> usize {
        self.actions.len()
    }

    /// Nothing was logged.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.actions.is_empty()
    }

    /// Actions were dropped because the log was full.
    #[must_use]
    pub fn truncated(&self) -> bool {
        self.truncated
    }

    fn push(&mut self, tick: u64, cmd: Command) {
        if self.actions.len() < ACTION_LOG_CAP {
            self.actions.push(LoggedAction { tick, cmd });
        } else {
            self.truncated = true;
        }
    }
}

/// A bookmark: a place and a view on one level.
#[derive(Clone, Debug, PartialEq)]
struct Mark {
    level: String,
    position: Vec3,
    yaw: f32,
    pitch: f32,
}

/// Where a teleport puts the player.
#[derive(Clone, Debug, PartialEq)]
struct Destination {
    /// Collision-centre position, UU.
    position: Vec3,
    /// View yaw (`None`: keep).
    yaw: Option<f32>,
    /// View pitch (`None`: keep).
    pitch: Option<f32>,
    /// What to call it in the message.
    label: String,
}

/// A Sandbox session.
#[derive(Clone, Debug)]
pub struct Session {
    profile: Profile,
    params: PlayerParams,
    catalog: Catalog,
    /// Overrides and rules as the profile was loaded (for "modified since").
    loaded: (Overlay, Rules),
    time: TimeControl,
    slots: Slots,
    rewind: RewindRing,
    telemetry: Telemetry,
    log: ActionLog,
    flying: bool,
    /// Time was running when fly placement began (it resumes on leaving).
    fly_resume: bool,
    marks: BTreeMap<String, Mark>,
    /// Next entry of the teleport-target list `NextTarget` goes to.
    next_target: usize,
    /// The running recording saw a parameter set other than Classic.
    recording_ran_modified: bool,
}

impl Session {
    /// A session running `profile`. A time scale of the profile outside the
    /// supported range is brought into it.
    ///
    /// # Errors
    /// The profile's base is not [`BASE_CLASSIC`], or an override does not
    /// apply.
    pub fn new(profile: Profile) -> Result<Self, SessionError> {
        if profile.base != BASE_CLASSIC {
            return Err(SessionError::UnknownBase(profile.base));
        }
        let params = profile.overrides.apply()?;
        Ok(Self::with(profile, params))
    }

    /// The pristine session: the Classic set, default rules.
    #[must_use]
    pub fn classic() -> Self {
        Self::with(Profile::classic(), PlayerParams::asamu_original())
    }

    fn with(profile: Profile, params: PlayerParams) -> Self {
        let mut time = TimeControl::default();
        apply_time_scale(&mut time, profile.time_scale);
        Self {
            loaded: (profile.overrides.clone(), profile.rules),
            profile,
            params,
            catalog: Catalog::classic(),
            time,
            slots: Slots::default(),
            rewind: RewindRing::new(REWIND_INTERVAL_TICKS, REWIND_KEYFRAMES),
            telemetry: Telemetry::default(),
            log: ActionLog::default(),
            flying: false,
            fly_resume: false,
            marks: BTreeMap::new(),
            next_target: 0,
            recording_ran_modified: false,
        }
    }

    /// The profile in use (its overrides and rules are the session's).
    #[must_use]
    pub fn profile(&self) -> &Profile {
        &self.profile
    }

    /// The parameter overrides.
    #[must_use]
    pub fn overlay(&self) -> &Overlay {
        &self.profile.overrides
    }

    /// The sticky rules.
    #[must_use]
    pub fn rules(&self) -> &Rules {
        &self.profile.rules
    }

    /// The effective parameter set: the base with the overlay applied.
    #[must_use]
    pub fn params(&self) -> &PlayerParams {
        &self.params
    }

    /// What the effective set is (Classic, or modified).
    #[must_use]
    pub fn label(&self) -> ParamSetLabel {
        param_set_label(&self.params)
    }

    /// No override, default rules, no command executed and time control
    /// never used.
    #[must_use]
    pub fn is_pristine(&self) -> bool {
        self.profile.is_pristine()
            && self.log.is_empty()
            && !self.log.truncated()
            && !self.time.was_used()
    }

    /// The overrides or the rules differ from what the profile had when it
    /// was loaded.
    #[must_use]
    pub fn profile_modified(&self) -> bool {
        self.profile.overrides != self.loaded.0 || self.profile.rules != self.loaded.1
    }

    /// The parameter catalogue the session tunes against.
    #[must_use]
    pub fn catalog(&self) -> &Catalog {
        &self.catalog
    }

    /// A game on a hand-made level with the session's parameters:
    /// `Game::new(level, self.params().clone(), DEFAULT_TICK_RATE_HZ)`.
    ///
    /// # Errors
    /// The level is invalid.
    pub fn new_game(&self, level: Level) -> Result<Game, GameError> {
        Game::new(level, self.params.clone(), DEFAULT_TICK_RATE_HZ)
    }

    /// Makes the game run the session's parameter set if it does not
    /// already. `Ok(true)` means it wrote; a game that already runs the set
    /// is not touched at all.
    ///
    /// # Errors
    /// [`SessionError::BaseMismatch`] for a placeholder-parameter game, or
    /// the game refused the set.
    pub fn reconcile(&mut self, cx: &mut SimCx<'_>) -> Result<bool, SessionError> {
        if cx.game.params().pawn.is_none() {
            return Err(SessionError::BaseMismatch);
        }
        if *cx.game.params() == self.params {
            return Ok(false);
        }
        retune(cx.game, self.params.clone())?;
        Ok(true)
    }

    /// The single mutation entry point: runs `cmd` and logs it.
    ///
    /// # Errors
    /// The command was refused; the session and the game are unchanged.
    pub fn execute(&mut self, cmd: Command, cx: &mut SimCx<'_>) -> Result<Outcome, CommandError> {
        let tick = cx.game.clock().tick();
        let outcome = self.run(&cmd, cx)?;
        self.log.push(tick, cmd);
        if outcome.discontinuity {
            // What follows is a new attempt: its trail starts here.
            self.telemetry.archive_attempt();
        }
        Ok(outcome)
    }

    /// Frozen-world fly placement: moves the player by `delta` (UU) while
    /// [`Session::flying`], keeping it placed (zero velocity, airborne, a
    /// floor check forced for the first tick after). Not logged per frame:
    /// leaving fly placement logs where the player ended up. Does nothing
    /// outside fly placement or for a `delta` that is not finite.
    pub fn fly_move(&mut self, cx: &mut SimCx<'_>, delta: Vec3) {
        if !self.flying || !delta.is_finite() {
            return;
        }
        let next = (cx.game.player().position + delta)
            .clamp(Vec3::splat(-PLACEMENT_LIMIT), Vec3::splat(PLACEMENT_LIMIT));
        if next.is_finite() {
            hold_player_at(cx.game, next);
        }
    }

    /// Before every tick: enforces the sticky rules (the default rules write
    /// nothing). A tick never runs in fly placement: if the host unfroze
    /// time behind the session's back, fly placement is over, and the log
    /// says where it left the player.
    pub fn before_tick(&mut self, cx: &mut SimCx<'_>) {
        if self.flying {
            self.leave_fly(cx.game);
        }
        self.profile.rules.enforce(cx.game);
    }

    /// After every tick: telemetry, and rewind keyframes on hand-made levels
    /// (not while recording). Never changes the game.
    pub fn after_tick(&mut self, game: &Game, script: Option<&LevelScript>, report: &TickReport) {
        self.telemetry.observe(game, report);
        if game.scene_map().is_some() {
            // The clone cost on converted maps is unmeasured: no keyframes.
            self.rewind.clear();
        } else if !game.is_recording() {
            self.rewind.observe(game, script);
        }
    }

    /// Session end: stops a running recording and returns it.
    pub fn finish(&mut self, cx: &mut SimCx<'_>) -> Option<SandboxRecording> {
        let trace = cx.game.stop_recording()?;
        Some(self.wrap_recording(trace))
    }

    /// The time-control model.
    #[must_use]
    pub fn time(&self) -> &TimeControl {
        &self.time
    }

    /// The time-control model, for the host's own bookkeeping (freezing
    /// while a panel is open). Commands go through [`Session::execute`].
    pub fn time_mut(&mut self) -> &mut TimeControl {
        &mut self.time
    }

    /// Frozen-world fly placement is on.
    #[must_use]
    pub fn flying(&self) -> bool {
        self.flying
    }

    /// The save-state slots.
    #[must_use]
    pub fn slots(&self) -> &Slots {
        &self.slots
    }

    /// The rewind keyframes.
    #[must_use]
    pub fn rewind(&self) -> &RewindRing {
        &self.rewind
    }

    /// The read-outs.
    #[must_use]
    pub fn telemetry(&self) -> &Telemetry {
        &self.telemetry
    }

    /// The command log.
    #[must_use]
    pub fn log(&self) -> &ActionLog {
        &self.log
    }

    /// Names of the bookmarks set on the level named `level`, sorted.
    #[must_use]
    pub fn marks_on(&self, level: &str) -> Vec<&str> {
        self.marks
            .iter()
            .filter(|(_, mark)| mark.level == level)
            .map(|(name, _)| name.as_str())
            .collect()
    }

    /// A recording that is running now saw a parameter set other than
    /// Classic at some point (so it is not a Classic-parameter recording
    /// even if the set is Classic again when it ends).
    #[must_use]
    pub fn recording_ran_modified(&self) -> bool {
        self.recording_ran_modified
    }

    /// Places the player can be teleported to in `game`: the start, the
    /// checkpoints (converted maps: every player start and the checkpoints'
    /// respawn points) and the session's bookmarks on this level. This is
    /// the list `TeleportTarget::NextTarget` walks through.
    #[must_use]
    pub fn teleport_targets(&self, game: &Game) -> Vec<TeleportTargetInfo> {
        let mut out = vec![TeleportTargetInfo {
            label: "start".to_owned(),
            target: TeleportTarget::Start,
        }];
        match game.scene_map() {
            Some(map) => {
                let primary = map.actors.player_start().map(|start| start.id);
                for start in &map.actors.player_starts {
                    if Some(start.id) == primary {
                        continue;
                    }
                    out.push(TeleportTargetInfo {
                        label: format!("player start {}", start.name),
                        target: TeleportTarget::Position {
                            position: start.location.to_array(),
                            yaw: Some(units_to_radians(start.rotation[1])),
                            pitch: Some(0.0),
                        },
                    });
                }
                for checkpoint in &map.actors.checkpoints {
                    out.push(TeleportTargetInfo {
                        label: format!("checkpoint {} ({})", checkpoint.index, checkpoint.name),
                        target: TeleportTarget::Checkpoint { id: checkpoint.id },
                    });
                }
            }
            None => {
                for checkpoint in &game.level().checkpoints {
                    out.push(TeleportTargetInfo {
                        label: format!("checkpoint {}", checkpoint.id),
                        target: TeleportTarget::Checkpoint { id: checkpoint.id },
                    });
                }
            }
        }
        for name in self.marks_on(&game.level().name) {
            out.push(TeleportTargetInfo {
                label: format!("bookmark {name}"),
                target: TeleportTarget::Mark {
                    name: name.to_owned(),
                },
            });
        }
        out
    }

    /// The host's banner line for `game` under this session. It always says
    /// that this is the Sandbox and never calls a modified set original.
    #[must_use]
    pub fn banner(&self, game: &Game) -> String {
        let set = match param_set_label(game.params()) {
            ParamSetLabel::Classic => {
                "Classic parameter set, unchanged (the recreation's; not verified against the \
                 original)"
                    .to_owned()
            }
            ParamSetLabel::Placeholder => "placeholder parameter set".to_owned(),
            ParamSetLabel::Modified { overrides } => {
                let listed: Vec<String> = self
                    .overlay()
                    .iter()
                    .take(6)
                    .map(|(key, value)| format!("{key} = {}", format_value(value)))
                    .collect();
                let more = self.overlay().len().saturating_sub(listed.len());
                format!(
                    "MODIFIED parameters ({overrides} overrides; NOT the original's values): {}{}",
                    listed.join(", "),
                    if more > 0 {
                        format!(", and {more} more")
                    } else {
                        String::new()
                    }
                )
            }
        };
        let rules = if self.rules().is_default() {
            "rules: the level's".to_owned()
        } else {
            format!("rules: {}", describe_rules(self.rules()))
        };
        format!(
            "ASAMU-decomp SANDBOX (experimental; not verified against the original; saves off) | \
             profile {}{} | {}\n{set}\n{rules} | movement model: {}",
            self.profile.name,
            if self.profile_modified() { "*" } else { "" },
            game.level().name,
            game.movement_model().name(),
        )
    }

    /// The session's current overrides, rules and time scale as a profile
    /// named `name` (the name is checked when the profile is saved).
    #[must_use]
    pub fn to_profile(&self, name: &str) -> Profile {
        let speed = self.time.speed();
        Profile {
            format: PROFILE_FORMAT.to_owned(),
            version: PROFILE_VERSION,
            name: name.to_owned(),
            description: if name == self.profile.name && !self.profile_modified() {
                self.profile.description.clone()
            } else {
                format!(
                    "Saved from a Sandbox session that started from the profile {:?}. Values are \
                     ours, not the original's.",
                    self.profile.name
                )
            },
            base: BASE_CLASSIC.to_owned(),
            overrides: self.profile.overrides.clone(),
            rules: self.profile.rules,
            time_scale: (speed != 1.0).then_some(speed),
            extensions: self.profile.extensions.clone(),
        }
    }

    // -----------------------------------------------------------------------
    // Commands.
    // -----------------------------------------------------------------------

    /// Runs `cmd`. Every arm checks before it writes: an error leaves the
    /// session and the game as they were.
    fn run(&mut self, cmd: &Command, cx: &mut SimCx<'_>) -> Result<Outcome, CommandError> {
        match cmd {
            Command::SetParam { key, value } => {
                let mut overlay = self.profile.overrides.clone();
                overlay.set(key, value.clone())?;
                self.tune(overlay, key, cx)
            }
            Command::NudgeParam { key, steps, scale } => {
                let mut overlay = self.profile.overrides.clone();
                overlay.nudge(key, *steps, *scale)?;
                self.tune(overlay, key, cx)
            }
            Command::ResetParam { key } => {
                if self.catalog.get(key).is_none() {
                    return Err(OverlayError::UnknownKey {
                        key: key.clone(),
                        suggestion: self.catalog.suggest(key).map(str::to_owned),
                    }
                    .into());
                }
                let mut overlay = self.profile.overrides.clone();
                if !overlay.clear(key) {
                    return Ok(Outcome::say(format!("{key} already is the Classic value")));
                }
                self.tune(overlay, key, cx)
            }
            Command::ResetAllParams => {
                let count = self.profile.overrides.len();
                self.commit(Overlay::default(), cx)?;
                Ok(Outcome {
                    message: format!("{count} overrides removed: the Classic parameter set"),
                    params_changed: count > 0,
                    ..Outcome::default()
                })
            }
            Command::LoadProfile { profile } => self.load_profile(profile, cx),
            Command::SetRules { rules } => {
                self.profile.rules = *rules;
                rules.enforce(cx.game);
                Ok(Outcome::say(format!("rules: {}", describe_rules(rules))))
            }
            Command::SetMaxGrapples { n } => {
                self.require_free_grapple_rule(cx.game)?;
                cx.game.set_max_grapples(*n);
                Ok(Outcome::say(describe_capacity(cx.game)))
            }
            Command::CycleGrapples => {
                self.require_free_grapple_rule(cx.game)?;
                // 0, 1, 2, 3, unlimited, and round again.
                let next = match cx.game.player().script.gun.max_grapples {
                    0 => 1,
                    1 => 2,
                    2 => 3,
                    3 => -1,
                    _ => 0,
                };
                cx.game.set_max_grapples(next);
                Ok(Outcome::say(describe_capacity(cx.game)))
            }
            Command::RocketBoots { on } => {
                require_script(cx.game)?;
                if self.profile.rules.rocket_boots != Switch::Level {
                    return Err(refused(
                        "the rocket-boots rule pins them; change the rule instead",
                    ));
                }
                let enable = on.resolve(cx.game.player().script.boots.enabled);
                cx.game.enable_rocket_boots(enable);
                Ok(Outcome::say(format!(
                    "rocket boots {}",
                    if enable { "enabled" } else { "disabled" }
                )))
            }
            Command::StoryMode { on } => {
                require_script(cx.game)?;
                let now = cx.game.in_story_mode();
                let want = on.resolve(now);
                if want && !now {
                    cx.game.enter_story_mode();
                } else if !want && now {
                    cx.game.exit_story_mode();
                }
                Ok(Outcome::say(format!(
                    "story mode {}",
                    if cx.game.in_story_mode() { "on" } else { "off" }
                )))
            }
            Command::RefillGrapples => {
                require_script(cx.game)?;
                grapple_gun::reset_grapple_amount(cx.game.player_mut());
                Ok(Outcome::say("grapples refilled"))
            }
            Command::ResetBoots => {
                require_script(cx.game)?;
                let boosting =
                    cx.game.player().script.boots.state == rocket_boots::BootsStateName::Boosting;
                rocket_boots::reset_boots(&mut cx.game.player_mut().script.boots);
                Ok(Outcome::say(if boosting {
                    "rocket boots re-armed (the boost was cut; move input returns at the next \
                     landing)"
                } else {
                    "rocket boots re-armed"
                }))
            }
            Command::ActivateAttractors => {
                let pads: Vec<u32> = cx.game.level().attractors.iter().map(|a| a.id).collect();
                if pads.is_empty() {
                    return Err(refused("this level has no attractor pad"));
                }
                for id in &pads {
                    cx.game.activate_attractor(*id);
                }
                Ok(Outcome::say(format!(
                    "{} attractor pads activated",
                    pads.len()
                )))
            }
            Command::Respawn => {
                cx.game.respawn();
                Ok(Outcome::jump("respawned at the active checkpoint"))
            }
            Command::Kill => {
                let scene = cx.game.scene_map().is_some();
                cx.game.kill_player();
                Ok(if scene {
                    Outcome::say("death sequence started")
                } else {
                    Outcome::jump("respawned at the active checkpoint")
                })
            }
            Command::Teleport { to } => self.teleport(to, cx),
            Command::SetMark { name } => self.set_mark(name, cx.game),
            Command::Fly { on } => self.fly(*on, cx),
            Command::Time { op } => self.time_op(*op, cx),
            Command::Slot { op } => self.slot_op(*op, cx),
            Command::Rewind => self.rewind_one(cx),
            Command::Record { on } => {
                let recording = cx.game.is_recording();
                match (on.resolve(recording), recording) {
                    (true, false) => {
                        cx.game.start_recording();
                        self.recording_ran_modified = false;
                        Ok(Outcome::say(
                            "sandbox recording started (never a parity trace)",
                        ))
                    }
                    (false, true) => {
                        let recording = cx.game.stop_recording().map(|t| self.wrap_recording(t));
                        Ok(Outcome {
                            message: "sandbox recording finished".to_owned(),
                            recording,
                            ..Outcome::default()
                        })
                    }
                    (true, true) => Ok(Outcome::say("already recording")),
                    (false, false) => Ok(Outcome::say("not recording")),
                }
            }
        }
    }

    /// Commits `overlay` and reports on `key`.
    fn tune(
        &mut self,
        overlay: Overlay,
        key: &str,
        cx: &mut SimCx<'_>,
    ) -> Result<Outcome, CommandError> {
        let report = self.commit(overlay, cx)?;
        let info = self.catalog.get(key);
        let classic = info.map(|i| &i.classic);
        let current = self.profile.overrides.get(key).or(classic);
        let mut message = match (current, self.profile.overrides.get(key).is_some()) {
            (Some(value), true) => format!(
                "{key} = {}{}",
                format_value(value),
                classic.map_or_else(String::new, |c| format!(" (Classic {})", format_value(c)))
            ),
            (Some(value), false) => format!("{key} = {} (the Classic value)", format_value(value)),
            (None, _) => format!("{key} changed"),
        };
        let relatched = !report.relatched.is_empty();
        match effect_of(key) {
            Effect::Live | Effect::Unclassified => {}
            Effect::Latched => message.push_str(if relatched {
                "; the running pawn's copy was updated"
            } else {
                "; latched: the running pawn's copy changes when it is next written"
            }),
            Effect::SpawnOnly => message.push_str("; takes effect at the next spawn"),
            Effect::Inert(reason) => {
                message.push_str("; no effect in this pipeline: ");
                message.push_str(reason);
            }
        }
        Ok(Outcome {
            message,
            params_changed: true,
            ..Outcome::default()
        })
    }

    /// Makes `overlay` the session's and the game's: applies it to the
    /// Classic set, hands the result to the game (which validates it), then
    /// stores it. On error nothing changed.
    fn commit(
        &mut self,
        overlay: Overlay,
        cx: &mut SimCx<'_>,
    ) -> Result<RelatchReport, CommandError> {
        let params = overlay.apply()?;
        let report = retune(cx.game, params.clone())?;
        self.note_parameter_change(cx.game, &params);
        self.profile.overrides = overlay;
        self.params = params;
        Ok(report)
    }

    /// Keeps track of whether a running recording saw a set other than
    /// Classic (before or after this change).
    fn note_parameter_change(&mut self, game: &Game, next: &PlayerParams) {
        let classic = PlayerParams::asamu_original();
        if !game.is_recording() {
            self.recording_ran_modified = false;
        } else if self.params != classic || *next != classic {
            self.recording_ran_modified = true;
        }
    }

    fn load_profile(
        &mut self,
        profile: &Profile,
        cx: &mut SimCx<'_>,
    ) -> Result<Outcome, CommandError> {
        if profile.base != BASE_CLASSIC {
            return Err(refused(format!(
                "unknown base {:?} (supported: {BASE_CLASSIC:?})",
                profile.base
            )));
        }
        let params = profile.overrides.apply()?;
        retune(cx.game, params.clone())?;
        self.note_parameter_change(cx.game, &params);
        self.params = params;
        self.profile = profile.clone();
        self.loaded = (profile.overrides.clone(), profile.rules);
        // The profile's speed replaces the session's; a freeze stays.
        match profile.time_scale {
            Some(_) => apply_time_scale(&mut self.time, profile.time_scale),
            None => apply_time_scale(&mut self.time, Some(1.0)),
        }
        self.profile.rules.enforce(cx.game);
        Ok(Outcome {
            message: format!(
                "profile {}: {} overrides, rules: {}, speed {}x",
                profile.name,
                profile.overrides.len(),
                describe_rules(&profile.rules),
                self.time.speed()
            ),
            params_changed: true,
            ..Outcome::default()
        })
    }

    fn require_free_grapple_rule(&self, game: &Game) -> Result<(), CommandError> {
        require_script(game)?;
        if self.profile.rules.grapples == GrappleRule::Level {
            Ok(())
        } else {
            Err(refused(
                "the grapple rule pins the capacity; change the rule instead",
            ))
        }
    }

    fn teleport(
        &mut self,
        to: &TeleportTarget,
        cx: &mut SimCx<'_>,
    ) -> Result<Outcome, CommandError> {
        // `NextTarget` is one of the listed targets; the cursor only moves
        // once the teleport went through.
        let (destination, advance) = match to {
            TeleportTarget::NextTarget => {
                let targets = self.teleport_targets(cx.game);
                let index = self.next_target % targets.len().max(1);
                let Some(next) = targets.get(index) else {
                    return Err(refused("this level has no teleport target"));
                };
                (
                    self.destination(&next.target, cx.game)?,
                    Some(index.saturating_add(1)),
                )
            }
            other => (self.destination(other, cx.game)?, None),
        };
        // Whatever the target was (a spot found beside a surface or above a
        // spawn point with a tuned cylinder, a bookmark set far out), the
        // placement limit holds.
        let place = destination.position;
        if !place.is_finite() || place.abs().max_element() > PLACEMENT_LIMIT {
            return Err(refused(format!(
                "{} is more than {PLACEMENT_LIMIT} uu from the origin",
                destination.label
            )));
        }
        require_free_player(cx.game, "teleport")?;
        place_player(cx.game, &destination);
        if let Some(next) = advance {
            self.next_target = next;
        }
        Ok(Outcome::jump(format!(
            "teleported to {}",
            destination.label
        )))
    }

    /// Where `target` is in `game`.
    fn destination(
        &self,
        target: &TeleportTarget,
        game: &Game,
    ) -> Result<Destination, CommandError> {
        match target {
            TeleportTarget::Position {
                position,
                yaw,
                pitch,
            } => {
                let position = Vec3::from_array(*position);
                let finite = position.is_finite()
                    && yaw.is_none_or(f32::is_finite)
                    && pitch.is_none_or(f32::is_finite);
                if !finite {
                    return Err(refused("the teleport position is not finite"));
                }
                if position.abs().max_element() > PLACEMENT_LIMIT {
                    return Err(refused(format!(
                        "the teleport position is more than {PLACEMENT_LIMIT} uu from the origin"
                    )));
                }
                Ok(Destination {
                    position,
                    yaw: *yaw,
                    pitch: *pitch,
                    label: format!("({:.0}, {:.0}, {:.0})", position.x, position.y, position.z),
                })
            }
            TeleportTarget::AimPoint => {
                let Some(aim) = game.gun_aim() else {
                    return Err(refused("there is no crosshair without the grapple gun"));
                };
                let Some(hit) = aim.impact.hit else {
                    return Err(refused("the crosshair is not on a surface"));
                };
                // Back off the surface by the cylinder's extent along the
                // normal, so the pawn is placed against it and not inside.
                let shape = shape_of(game);
                let normal = hit.normal.normalize_or_zero();
                let extent = shape.radius * normal.truncate().length()
                    + shape.half_height * normal.z.abs()
                    + 4.0 * CONTACT_SKIN;
                let position = aim.impact.location + normal * extent;
                if !position.is_finite() || game.world().overlaps(position, shape) {
                    return Err(refused("there is no room for the player at the crosshair"));
                }
                Ok(Destination {
                    position,
                    yaw: None,
                    pitch: None,
                    label: "the crosshair".to_owned(),
                })
            }
            TeleportTarget::Start => match game.scene_map() {
                Some(map) => {
                    let Some(start) = map.actors.player_start() else {
                        return Err(refused("this map has no player start"));
                    };
                    Ok(Destination {
                        position: free_spot(game, start.location),
                        yaw: Some(units_to_radians(start.rotation[1])),
                        pitch: Some(0.0),
                        label: "the start".to_owned(),
                    })
                }
                None => Ok(spawn_destination(
                    game,
                    &game.level().player_start,
                    "the start",
                )),
            },
            TeleportTarget::Checkpoint { id } => match game.scene_map() {
                Some(map) => {
                    let Some(checkpoint) = map.actors.checkpoints.iter().find(|c| c.id == *id)
                    else {
                        return Err(refused(format!("this map has no checkpoint {id}")));
                    };
                    Ok(Destination {
                        position: free_spot(game, checkpoint.spawn_location),
                        yaw: Some(units_to_radians(checkpoint.spawn_rotation[1])),
                        pitch: Some(units_to_radians(normalize_axis(
                            checkpoint.spawn_rotation[0],
                        ))),
                        label: format!("checkpoint {} ({})", checkpoint.index, checkpoint.name),
                    })
                }
                None => {
                    let Some(checkpoint) = game.level().checkpoints.iter().find(|c| c.id == *id)
                    else {
                        return Err(refused(format!("this level has no checkpoint {id}")));
                    };
                    Ok(spawn_destination(
                        game,
                        &checkpoint.spawn,
                        &format!("checkpoint {id}"),
                    ))
                }
            },
            TeleportTarget::NextTarget => Err(refused("there is no next teleport target")),
            TeleportTarget::Mark { name } => {
                let Some(mark) = self.marks.get(name) else {
                    return Err(refused(format!("there is no bookmark {name:?}")));
                };
                if mark.level != game.level().name {
                    return Err(refused(format!(
                        "the bookmark {name:?} was set on {:?}",
                        mark.level
                    )));
                }
                Ok(Destination {
                    position: mark.position,
                    yaw: Some(mark.yaw),
                    pitch: Some(mark.pitch),
                    label: format!("bookmark {name}"),
                })
            }
        }
    }

    fn set_mark(&mut self, name: &str, game: &Game) -> Result<Outcome, CommandError> {
        let plain =
            !name.is_empty() && name.len() <= MAX_MARK_NAME && !name.chars().any(char::is_control);
        if !plain {
            return Err(refused(format!(
                "a bookmark name has 1 to {MAX_MARK_NAME} bytes and no control characters"
            )));
        }
        if !self.marks.contains_key(name) && self.marks.len() >= MAX_MARKS {
            return Err(refused(format!("at most {MAX_MARKS} bookmarks")));
        }
        let player = game.player();
        if !player.position.is_finite() {
            return Err(refused("the player's position is not finite"));
        }
        self.marks.insert(
            name.to_owned(),
            Mark {
                level: game.level().name.clone(),
                position: player.position,
                yaw: player.yaw,
                pitch: player.pitch,
            },
        );
        Ok(Outcome::say(format!("bookmark {name} set")))
    }

    fn fly(&mut self, on: Toggle, cx: &mut SimCx<'_>) -> Result<Outcome, CommandError> {
        match (on.resolve(self.flying), self.flying) {
            (true, false) => {
                require_free_player(cx.game, "fly")?;
                self.fly_resume = !self.time.frozen();
                self.time.set_frozen(true);
                self.flying = true;
                // From here on the pawn is held as a teleport leaves it, so
                // whenever fly placement ends it simply falls from there.
                let here = cx.game.player().position;
                hold_player_at(cx.game, here);
                Ok(Outcome::say(
                    "fly placement on (time frozen; this is placement, not flight physics)",
                ))
            }
            (false, true) => {
                self.leave_fly(cx.game);
                if self.fly_resume {
                    self.time.set_frozen(false);
                }
                Ok(Outcome::jump("fly placement off"))
            }
            (true, true) => Ok(Outcome::say("fly placement already on")),
            (false, false) => Ok(Outcome::say("fly placement already off")),
        }
    }

    /// Ends fly placement. The game is not touched (the pawn has been held
    /// in the placed state all along); one teleport is logged saying where
    /// the player ended up, and a new attempt starts in the read-outs.
    fn leave_fly(&mut self, game: &Game) {
        self.flying = false;
        let player = game.player();
        self.log.push(
            game.clock().tick(),
            Command::Teleport {
                to: TeleportTarget::Position {
                    position: player.position.to_array(),
                    yaw: Some(player.yaw),
                    pitch: Some(player.pitch),
                },
            },
        );
        self.telemetry.archive_attempt();
    }

    fn time_op(&mut self, op: TimeOp, cx: &mut SimCx<'_>) -> Result<Outcome, CommandError> {
        // Check first: only a speed can be refused.
        if let TimeOp::Scale { value } = op {
            let mut probe = self.time.clone();
            probe.set_speed(value)?;
        }
        // Time never runs in fly placement: leaving comes first.
        let runs = match op {
            TimeOp::Freeze(toggle) => !toggle.resolve(self.time.frozen()),
            TimeOp::Step { n } => n > 0,
            TimeOp::Reset => true,
            TimeOp::Slower | TimeOp::Faster | TimeOp::Scale { .. } => false,
        };
        let left_fly = self.flying && runs;
        if left_fly {
            self.leave_fly(cx.game);
        }
        match op {
            TimeOp::Freeze(toggle) => {
                let frozen = toggle.resolve(self.time.frozen());
                self.time.set_frozen(frozen);
            }
            TimeOp::Step { n } => self.time.request_steps(n),
            TimeOp::Slower => self.time.slower(),
            TimeOp::Faster => self.time.faster(),
            TimeOp::Scale { value } => self.time.set_speed(value)?,
            TimeOp::Reset => self.time.reset(),
        }
        let message = match op {
            // What is queued, which is what will run: requests add up and
            // are bounded.
            TimeOp::Step { .. } => format!("stepping: {} ticks queued", self.time.pending_steps()),
            _ if self.time.frozen() => format!("time frozen (speed {}x)", self.time.speed()),
            _ => format!("time running at {}x", self.time.speed()),
        };
        Ok(Outcome {
            message,
            discontinuity: left_fly,
            ..Outcome::default()
        })
    }

    fn slot_op(&mut self, op: SlotOp, cx: &mut SimCx<'_>) -> Result<Outcome, CommandError> {
        match op {
            SlotOp::Select { slot } => {
                self.slots.select(slot)?;
                Ok(Outcome::say(format!("slot {} selected", slot + 1)))
            }
            SlotOp::Save { slot } => {
                let tick = cx.game.clock().tick();
                let snapshot =
                    SimSnapshot::capture(cx.game, cx.script.as_ref(), format!("tick {tick}"));
                let scripted = snapshot.scripted();
                self.slots.save(slot, snapshot)?;
                Ok(Outcome::say(format!(
                    "slot {} saved at tick {tick}{}",
                    slot + 1,
                    if scripted {
                        " (simulation only: audio, effects and scripted interface are not saved)"
                    } else {
                        ""
                    }
                )))
            }
            SlotOp::Load { slot } => {
                let saved = self.slots.filled(slot)?;
                let tick = saved.tick();
                let (game, script) = saved.instantiate(cx.game)?;
                let mut outcome = self.install(game, script, cx)?;
                outcome.message = format!("slot {} loaded (tick {tick})", slot + 1);
                Ok(outcome)
            }
            SlotOp::Clear { slot } => {
                let had = self.slots.clear(slot)?;
                Ok(Outcome::say(format!(
                    "slot {} {}",
                    slot + 1,
                    if had { "cleared" } else { "was empty" }
                )))
            }
        }
    }

    fn rewind_one(&mut self, cx: &mut SimCx<'_>) -> Result<Outcome, CommandError> {
        if cx.game.scene_map().is_some() {
            return Err(refused(
                "rewind is available on hand-made levels only (use a save-state slot here)",
            ));
        }
        let now = cx.game.clock().tick();
        let Some(keyframe) = self.rewind.rewind_from(now) else {
            return Err(refused("there is no keyframe to rewind to yet"));
        };
        let tick = keyframe.tick();
        let (game, script) = keyframe.instantiate(cx.game)?;
        let mut outcome = self.install(game, script, cx)?;
        outcome.message = format!(
            "rewound to tick {tick} ({} ticks back)",
            now.saturating_sub(tick)
        );
        Ok(outcome)
    }

    /// Puts a save state's simulation (fresh clones, see
    /// `SimSnapshot::instantiate`) in place of the running one, under the
    /// session's current parameters and rules. A running recording is
    /// finished first and handed to the host. On error nothing changed.
    fn install(
        &mut self,
        mut game: Game,
        script: Option<LevelScript>,
        cx: &mut SimCx<'_>,
    ) -> Result<Outcome, CommandError> {
        // The replacement is retuned aside, so a refusal leaves the running
        // simulation alone.
        retune(&mut game, self.params.clone())?;
        let recording = cx.game.stop_recording().map(|t| self.wrap_recording(t));
        *cx.game = game;
        *cx.script = script;
        self.profile.rules.enforce(cx.game);
        Ok(Outcome {
            message: String::new(),
            params_changed: false,
            discontinuity: true,
            recording,
        })
    }

    fn wrap_recording(&mut self, trace: asamu_player::Trace) -> SandboxRecording {
        let recording = SandboxRecording::finish(trace, self);
        self.recording_ran_modified = false;
        recording
    }
}

// ---------------------------------------------------------------------------
// Helpers.
// ---------------------------------------------------------------------------

fn refused(why: impl Into<String>) -> CommandError {
    CommandError::Refused(why.into())
}

/// Brings a profile's time scale into the supported range and applies it; a
/// scale that is not finite is ignored.
fn apply_time_scale(time: &mut TimeControl, scale: Option<f32>) {
    if let Some(scale) = scale.filter(|s| s.is_finite()) {
        // In range after the clamp, so this cannot be refused.
        let _ = time.set_speed(scale.clamp(MIN_SPEED, MAX_SPEED));
    }
}

/// Ability commands act on the script layer's gun and boots.
fn require_script(game: &Game) -> Result<(), CommandError> {
    if game.uses_original_params() && game.player().script.started {
        Ok(())
    } else {
        Err(refused(
            "this game runs without the script layer (placeholder parameters)",
        ))
    }
}

/// Moving the player by hand is refused while the grapple is attached (the
/// grappled object would never hear of the release) and during the death
/// sequence (its reset teleports anyway).
fn require_free_player(game: &Game, what: &str) -> Result<(), CommandError> {
    if game.player().is_grapple_attached() {
        return Err(refused(format!(
            "cannot {what} while the grapple is attached: release it first"
        )));
    }
    if game.is_dying() {
        return Err(refused(format!("cannot {what} during the death sequence")));
    }
    Ok(())
}

fn shape_of(game: &Game) -> CollisionShape {
    let movement = &game.params().movement;
    CollisionShape {
        radius: movement.capsule_radius.value,
        half_height: movement.capsule_half_height.value,
    }
}

/// The collision centre of a pawn spawned at a hand-made level's `spawn`
/// (the floor point under its feet), as the game's own spawn places it.
fn spawn_destination(game: &Game, spawn: &SpawnPoint, label: &str) -> Destination {
    let lift = game.params().movement.capsule_half_height.value + CONTACT_SKIN;
    Destination {
        position: spawn.feet + Vec3::Z * lift,
        yaw: Some(spawn.yaw),
        pitch: Some(0.0),
        label: label.to_owned(),
    }
}

/// Most places [`free_spot`] tries above a blocked one (ours). The collision
/// cylinder is a tunable parameter with no upper limit, so the search is
/// bounded by a count and not by the cylinder's size: each try is an overlap
/// query against the whole scene.
const FREE_SPOT_TRIES: u8 = 64;
/// The smallest step between two tries of [`free_spot`], UU (ours).
const FREE_SPOT_STEP: f32 = 4.0;

/// `position` itself when the player fits there, else the first free spot
/// straight above it (up to twice the cylinder's size, in steps of
/// [`FREE_SPOT_STEP`] or larger ones for a cylinder so big that
/// [`FREE_SPOT_TRIES`] of those would not cover the distance), else
/// `position` unchanged. A placement aid for spawn points of converted
/// maps, built on the world's own overlap query.
fn free_spot(game: &Game, position: Vec3) -> Vec3 {
    let shape = shape_of(game);
    let world = game.world();
    if !position.is_finite() || !world.overlaps(position, shape) {
        return position;
    }
    let reach = 2.0 * (shape.half_height + shape.radius);
    let step = (reach / f32::from(FREE_SPOT_TRIES)).max(FREE_SPOT_STEP);
    if !step.is_finite() {
        return position;
    }
    for n in 1..=FREE_SPOT_TRIES {
        let lift = step * f32::from(n);
        if lift > reach {
            break;
        }
        let up = position + Vec3::Z * lift;
        if up.is_finite() && !world.overlaps(up, shape) {
            return up;
        }
    }
    position
}

/// The teleport recipe (the one the app's debug teleport uses): position,
/// view, zero velocity, airborne with a forced floor check, and the cached
/// point of view following the view.
fn place_player(game: &mut Game, to: &Destination) {
    let max_pitch = game.params().camera.max_pitch_degrees.value.to_radians();
    let player = game.player_mut();
    player.position = to.position;
    if let Some(yaw) = to.yaw {
        // A yaw already in range is kept bit for bit (a bookmark returns
        // the exact view); only one outside it is wrapped.
        player.yaw = if (-std::f32::consts::PI..std::f32::consts::PI).contains(&yaw) {
            yaw
        } else {
            wrap_radians(yaw)
        };
    }
    if let Some(pitch) = to.pitch {
        player.pitch = if max_pitch.is_finite() && max_pitch > 0.0 {
            pitch.clamp(-max_pitch, max_pitch)
        } else {
            pitch
        };
    }
    player.velocity = Vec3::ZERO;
    player.grounded = false;
    player.pawn.force_floor_check = true;
    player.script.pov_yaw = player.yaw;
    player.script.pov_pitch = player.pitch;
}

/// Holds the player at `position` in the placed state of the teleport
/// recipe, with its view unchanged (fly placement).
fn hold_player_at(game: &mut Game, position: Vec3) {
    place_player(
        game,
        &Destination {
            position,
            yaw: None,
            pitch: None,
            label: String::new(),
        },
    );
}

fn describe_capacity(game: &Game) -> String {
    let capacity = game.player().script.gun.max_grapples;
    if capacity >= grapple_gun::UNLIMITED_GRAPPLES {
        "grapple capacity unlimited".to_owned()
    } else {
        format!("grapple capacity {capacity}")
    }
}

fn describe_rules(rules: &Rules) -> String {
    let grapples = match rules.grapples {
        GrappleRule::Level => "grapples as the level sets them".to_owned(),
        // A negative count is the game's own spelling of "unlimited".
        GrappleRule::Fixed(n) if n >= 0 => format!("{n} grapples"),
        GrappleRule::Fixed(_) | GrappleRule::Unlimited => "unlimited grapples".to_owned(),
    };
    let boots = match rules.rocket_boots {
        Switch::Level => "boots as the level sets them",
        Switch::On => "boots on",
        Switch::Off => "boots off",
    };
    format!(
        "{grapples}, {boots}{}",
        if rules.auto_refill {
            ", grapples refill by themselves"
        } else {
            ""
        }
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_classic_session_is_pristine_and_runs_the_classic_set() {
        let session = Session::classic();
        assert!(session.is_pristine());
        assert_eq!(session.label(), ParamSetLabel::Classic);
        assert_eq!(*session.params(), PlayerParams::asamu_original());
        assert!(session.overlay().is_empty());
        assert!(session.rules().is_default());
        assert!(session.time().is_default());
        assert!(session.log().is_empty());
        assert!(!session.flying());
        assert!(!session.profile_modified());
        assert!(!session.recording_ran_modified());
        // The same through a profile.
        let from_profile = Session::new(Profile::classic()).unwrap();
        assert_eq!(*from_profile.params(), PlayerParams::asamu_original());
        assert!(from_profile.is_pristine());
    }

    #[test]
    fn an_unknown_base_is_refused() {
        let profile = Profile {
            base: "placeholder".to_owned(),
            ..Profile::classic()
        };
        assert_eq!(
            Session::new(profile).err(),
            Some(SessionError::UnknownBase("placeholder".to_owned()))
        );
    }

    #[test]
    fn reconcile_leaves_a_classic_game_alone_and_refuses_a_placeholder_game() {
        let mut session = Session::classic();
        let mut game = Game::graybox().unwrap();
        let before = game.clone();
        let mut script = None;
        let mut cx = SimCx {
            game: &mut game,
            script: &mut script,
        };
        assert_eq!(session.reconcile(&mut cx), Ok(false));
        session.before_tick(&mut cx);
        assert_eq!(game.params(), before.params());
        assert_eq!(game.player(), before.player());

        let mut placeholder = Game::graybox_placeholder().unwrap();
        let mut cx = SimCx {
            game: &mut placeholder,
            script: &mut script,
        };
        assert_eq!(session.reconcile(&mut cx), Err(SessionError::BaseMismatch));
    }

    #[test]
    fn a_classic_session_builds_the_same_game_as_the_graybox_constructor() {
        let ours = Session::classic()
            .new_game(asamu_world::graybox_test_level())
            .unwrap();
        let classic = Game::graybox().unwrap();
        assert_eq!(ours.params(), classic.params());
        assert_eq!(ours.player(), classic.player());
        assert_eq!(ours.level(), classic.level());
        assert_eq!(ours.clock().tick_rate_hz(), classic.clock().tick_rate_hz());
    }

    #[test]
    fn the_banner_always_says_sandbox() {
        let session = Session::classic();
        let game = Game::graybox().unwrap();
        let banner = session.banner(&game);
        assert!(banner.contains("SANDBOX"), "{banner}");
        assert!(
            banner.contains("not verified against the original"),
            "{banner}"
        );
        assert!(banner.contains("saves off"), "{banner}");
        assert!(!banner.contains("ORIGINAL values"), "{banner}");
    }

    #[test]
    fn the_action_log_is_capped_and_says_so() {
        let mut log = ActionLog::default();
        for tick in 0..(ACTION_LOG_CAP as u64 + 5) {
            log.push(tick, Command::Respawn);
        }
        assert_eq!(log.len(), ACTION_LOG_CAP);
        assert!(log.truncated());
        assert_eq!(
            log.actions().last().map(|a| a.tick),
            Some(ACTION_LOG_CAP as u64 - 1)
        );
    }

    #[test]
    fn a_profile_time_scale_is_brought_into_range() {
        for (given, expected) in [
            (Some(0.25), 0.25),
            (Some(1000.0), MAX_SPEED),
            (Some(-3.0), MIN_SPEED),
            (Some(f32::NAN), 1.0),
            (None, 1.0),
        ] {
            let profile = Profile {
                time_scale: given,
                ..Profile::classic()
            };
            let session = Session::new(profile).unwrap();
            assert_eq!(session.time().speed(), expected, "{given:?}");
        }
    }
}
