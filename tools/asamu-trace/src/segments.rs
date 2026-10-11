//! Where a replay of a recording may start, and what happens in between.
//!
//! A replay starts from one sample of the original: position, velocity, view
//! and walking/falling from the sample, the recorded script state from the
//! `state:` note ([`crate::state`]). What no record shows starts fresh in
//! our simulation: the pawn's and the power jump's latent state code (jump
//! release damping, a charging power jump, a running zoom), the gun's
//! timers and attachment, the rocket boots' boost. All of that is at rest
//! when the pawn **stands still with no button held**, so that is where a
//! replay of an original recording starts:
//!
//! - a sample is *quiet* when it is walking (`grounded`), its velocity is
//!   exactly zero, the grapple is not attached and its input has no move, no
//!   jump (held or pressed), no grapple button and no power-jump button (the
//!   sprint button may be held: flag, speed and button level are all
//!   recorded);
//! - a sample is a *clean start* when it is quiet, the sample before it
//!   (if the trace has one, without a tick gap) is quiet too, so that
//!   anything a button release one tick earlier set off has shown, and the
//!   pawn was not teleported in its tick.
//!
//! [`clean_runs`] lists the stretches of clean starts, [`events`] what the
//! player did, both for picking segments (`asamu-trace starts`).
//!
//! # Events a replay cannot follow
//!
//! Two kinds of event in a recording come from outside the player's
//! simulation, so a replay that only feeds inputs cannot reproduce them
//! ([`EventKind::ends_validity`]; [`crate::replay`] ends a replay's validity
//! at the first of them, or takes them from the recording under a labelled
//! option):
//!
//! - **A teleport** ([`is_teleport`]): the pawn moved farther in one tick
//!   than [`TELEPORT_SPEED`] × the tick's frame length. That speed is the
//!   3-D clamp of the falling physics (`ASAMUPawn.fTerminalVelocity`, class
//!   default 10,000 uu/s; NATIVE_PHYSICS.md 4), so no fall covers more. In
//!   the recordings of 2026-10-10 the rule finds five ticks, each a respawn
//!   after a death (10,035 to 34,860 uu in one tick; the largest
//!   displacement per time anywhere else is 6,776 uu/s, in the fall before
//!   one of them): CONFIRMED on those 45,237 records. The rule cannot find
//!   a script move that is shorter than that limit (167 uu in a frame of
//!   1/60 s, more in a longer one): a respawn that close to the place of
//!   death would pass as a move. Whether those recordings hold such a case
//!   cannot be told from the samples (the five respawns found moved the
//!   pawn 10,035 uu or more); a recorder field for the death or the respawn
//!   would close the gap.
//! - **A level-script state change**, read from the `state:` note: the
//!   recorded `GroundSpeed` changes in a way the pawn's own script does not
//!   make (it writes the walking and the sprint speed, ABILITIES.md A-WK-3:
//!   to or from the story speed is story mode entered or left, any other
//!   value is a console `SetSpeed`), the grapple capacity `iMaxGrapples`
//!   changes, or the rocket boots are switched. In the same recordings: 7
//!   ticks (6 story-mode changes, 1 capacity change).
//!
//! # Attaches
//!
//! [`EventKind::GrappleAttached`] is the attached flag rising from one
//! sample to the next. An attach that is released inside the same frame
//! never shows in a sample; it does show in the gun's used-grapple counter
//! (`iTimesGrappled` in the `state:` note), which every attach raises
//! (GRAPPLE.md G-AT-1). [`recorded_events`] reports such a tick as
//! [`EventKind::GrappleAttachedWithinFrame`], so that the attaches of a
//! recording are counted from the counter: 134 in the recordings of
//! 2026-10-10, 15 of them inside one frame.

use std::fmt::Write as _;

use anyhow::Result;
use asamu_core::glam::Vec3;
use asamu_player::trace::{TraceGrappleState, TraceSample};
use asamu_player::{PawnParams, PlayerParams, Trace};

use crate::state::{StateTimeline, TickState, story_mode_shown};

/// Speed no physics of the pawn exceeds, uu/s: a longer move in one tick is
/// a teleport ([`is_teleport`]). Source: `ASAMUPawn.fTerminalVelocity`
/// (class default; `PlayerParams::asamu_original().movement.terminal_velocity`,
/// which a test compares it with), the 3-D speed clamp of the falling
/// physics.
pub const TELEPORT_SPEED: f64 = 10_000.0;

/// Distance of two positions in `f64` (the Python converter computes it the
/// same way).
fn distance(a: Vec3, b: Vec3) -> f64 {
    let (dx, dy, dz) = (
        f64::from(a.x) - f64::from(b.x),
        f64::from(a.y) - f64::from(b.y),
        f64::from(a.z) - f64::from(b.z),
    );
    (dx * dx + dy * dy + dz * dz).sqrt()
}

/// `true` if the pawn was teleported in the tick of `s`: `before` is the
/// sample of the tick before it and the pawn moved more than
/// [`TELEPORT_SPEED`] × the frame length (`s.time − before.time`; in a tick
/// in which no time passes, any move).
#[must_use]
pub fn is_teleport(before: &TraceSample, s: &TraceSample) -> bool {
    if before.tick.checked_add(1) != Some(s.tick) {
        return false;
    }
    let limit = TELEPORT_SPEED * (s.time - before.time);
    distance(s.position, before.position) > limit.max(0.0)
}

/// How far the pawn moved in the tick of `s`, UU.
#[must_use]
pub fn tick_displacement(before: &TraceSample, s: &TraceSample) -> f64 {
    distance(s.position, before.position)
}

/// `true` if `s` stands still with nothing held (see the module docs).
#[must_use]
pub fn is_quiet(s: &TraceSample) -> bool {
    not_quiet_reasons(s).is_empty()
}

/// Why `s` is not quiet (empty when it is).
#[must_use]
pub fn not_quiet_reasons(s: &TraceSample) -> Vec<&'static str> {
    let mut why = Vec::new();
    if s.grapple_state == TraceGrappleState::Attached {
        why.push("the grapple is attached");
    } else if !s.grounded {
        why.push("the pawn is in the air");
    }
    if s.velocity != Vec3::ZERO {
        why.push("the pawn is moving");
    }
    if s.input.move_forward != 0.0 || s.input.move_right != 0.0 {
        why.push("a move input is held");
    }
    if s.input.jump_held || s.input.jump_pressed {
        why.push("the jump button is held");
    }
    if s.input.grapple_held {
        why.push("the grapple button is held");
    }
    if s.input.power_jump_held {
        why.push("the power-jump button is held");
    }
    why
}

/// `true` if `samples[index]` is a clean start (see the module docs).
#[must_use]
pub fn is_clean_start(samples: &[TraceSample], index: usize) -> bool {
    let Some(s) = samples.get(index) else {
        return false;
    };
    if !is_quiet(s) {
        return false;
    }
    match index.checked_sub(1).and_then(|i| samples.get(i)) {
        None => true,
        Some(before) => {
            before.tick.checked_add(1) == Some(s.tick)
                && is_quiet(before)
                && !is_teleport(before, s)
        }
    }
}

/// Why `samples[index]` is not a clean start (empty when it is one).
#[must_use]
pub fn not_clean_reasons(samples: &[TraceSample], index: usize) -> Vec<&'static str> {
    let Some(s) = samples.get(index) else {
        return vec!["no such sample"];
    };
    let mut why = not_quiet_reasons(s);
    if why.is_empty()
        && let Some(before) = index.checked_sub(1).and_then(|i| samples.get(i))
    {
        if !(before.tick.checked_add(1) == Some(s.tick) && is_quiet(before)) {
            why.push("the tick before it is not standing still");
        } else if is_teleport(before, s) {
            why.push("the pawn was teleported in this tick");
        }
    }
    why
}

/// Index of the first clean start at or after `from`.
#[must_use]
pub fn next_clean_start(samples: &[TraceSample], from: usize) -> Option<usize> {
    (from..samples.len()).find(|i| is_clean_start(samples, *i))
}

/// Index of the last clean start at or before `from`.
#[must_use]
pub fn previous_clean_start(samples: &[TraceSample], from: usize) -> Option<usize> {
    (0..=from.min(samples.len().checked_sub(1)?))
        .rev()
        .find(|i| is_clean_start(samples, *i))
}

/// Maximal stretches of consecutive clean starts, as inclusive index ranges.
#[must_use]
pub fn clean_runs(samples: &[TraceSample]) -> Vec<(usize, usize)> {
    let mut runs: Vec<(usize, usize)> = Vec::new();
    for i in 0..samples.len() {
        if !is_clean_start(samples, i) {
            continue;
        }
        match runs.last_mut() {
            Some(last) if last.1 + 1 == i => last.1 = i,
            _ => runs.push((i, i)),
        }
    }
    runs
}

/// Something the player did, or that happened to the pawn, at one tick.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EventKind {
    /// A move input begins (after a tick without one).
    MoveBegins,
    /// The move input ends.
    MoveEnds,
    /// The sprint button goes down.
    SprintDown,
    /// The sprint button goes up.
    SprintUp,
    /// A jump press.
    Jump,
    /// The power-jump button goes down.
    PowerJumpDown,
    /// The power-jump button goes up.
    PowerJumpUp,
    /// `use` pressed.
    Use,
    /// The grapple button goes down.
    GrappleButtonDown,
    /// The grapple button goes up.
    GrappleButtonUp,
    /// The grapple attached.
    GrappleAttached,
    /// The grapple let go in a tick whose input has the button up.
    GrappleReleasedByButton,
    /// The grapple let go while the button was still held (the gun's own
    /// release: closer than its release distance, an instant release, or
    /// something done to the pawn).
    GrappleReleasedHeld,
    /// Walking → not walking.
    LeftGround,
    /// Not walking → walking.
    Landed,
    /// The pawn moved farther than any physics allows ([`is_teleport`]): a
    /// respawn or another script move.
    Teleport,
    /// The recorded `GroundSpeed` became the story speed.
    StoryModeOn,
    /// The recorded `GroundSpeed` went from the story speed to the walking
    /// or the sprint speed.
    StoryModeOff,
    /// The recorded `GroundSpeed` changed to or from a value the pawn's own
    /// script never writes (a console `SetSpeed`).
    GroundSpeedSet,
    /// The gun's capacity `iMaxGrapples` changed.
    MaxGrapplesChanged,
    /// The rocket boots were enabled.
    BootsEnabled,
    /// The rocket boots were disabled.
    BootsDisabled,
    /// The used-grapple counter rose without the attached flag rising: an
    /// attach that was released inside the same frame.
    GrappleAttachedWithinFrame,
}

impl EventKind {
    /// Short name for listings.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::MoveBegins => "move",
            Self::MoveEnds => "move ends",
            Self::SprintDown => "sprint",
            Self::SprintUp => "sprint ends",
            Self::Jump => "jump",
            Self::PowerJumpDown => "power-jump button down",
            Self::PowerJumpUp => "power-jump button up",
            Self::Use => "use",
            Self::GrappleButtonDown => "grapple button down",
            Self::GrappleButtonUp => "grapple button up",
            Self::GrappleAttached => "grapple attached",
            Self::GrappleReleasedByButton => "grapple released by the button",
            Self::GrappleReleasedHeld => "grapple released with the button held",
            Self::LeftGround => "left the ground",
            Self::Landed => "landed",
            Self::Teleport => "teleport",
            Self::StoryModeOn => "story mode on",
            Self::StoryModeOff => "story mode off",
            Self::GroundSpeedSet => "GroundSpeed set (a console speed)",
            Self::MaxGrapplesChanged => "grapple capacity changed",
            Self::BootsEnabled => "rocket boots enabled",
            Self::BootsDisabled => "rocket boots disabled",
            Self::GrappleAttachedWithinFrame => "grapple attached and released within the frame",
        }
    }

    /// `true` for a change of the level script's making (story mode, a
    /// console speed, the grapple capacity, the rocket boots).
    #[must_use]
    pub fn is_level_state(self) -> bool {
        matches!(
            self,
            Self::StoryModeOn
                | Self::StoryModeOff
                | Self::GroundSpeedSet
                | Self::MaxGrapplesChanged
                | Self::BootsEnabled
                | Self::BootsDisabled
        )
    }

    /// `true` for an event that a replay of the inputs cannot reproduce: a
    /// teleport or a level-script state change (see the module docs).
    #[must_use]
    pub fn ends_validity(self) -> bool {
        self == Self::Teleport || self.is_level_state()
    }

    /// `true` for an attach of the grapple, seen in a sample or only in the
    /// used-grapple counter.
    #[must_use]
    pub fn is_attach(self) -> bool {
        matches!(
            self,
            Self::GrappleAttached | Self::GrappleAttachedWithinFrame
        )
    }
}

/// One event.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Event {
    /// Index of the sample whose input or state shows it.
    pub index: usize,
    /// Its tick.
    pub tick: u64,
    /// What.
    pub kind: EventKind,
}

/// The events of `samples`, in order (each from a sample and the one before
/// it; nothing is reported across a tick gap).
#[must_use]
pub fn events(samples: &[TraceSample]) -> Vec<Event> {
    let mut out = Vec::new();
    for (i, w) in samples.windows(2).enumerate() {
        let [a, b] = w else { continue };
        if a.tick.checked_add(1) != Some(b.tick) {
            continue;
        }
        let index = i + 1;
        let mut push = |kind| {
            out.push(Event {
                index,
                tick: b.tick,
                kind,
            });
        };
        let moving = |s: &TraceSample| s.input.move_forward != 0.0 || s.input.move_right != 0.0;
        match (moving(a), moving(b)) {
            (false, true) => push(EventKind::MoveBegins),
            (true, false) => push(EventKind::MoveEnds),
            _ => {}
        }
        match (a.input.sprint_held, b.input.sprint_held) {
            (false, true) => push(EventKind::SprintDown),
            (true, false) => push(EventKind::SprintUp),
            _ => {}
        }
        if b.input.jump_pressed {
            push(EventKind::Jump);
        }
        match (a.input.power_jump_held, b.input.power_jump_held) {
            (false, true) => push(EventKind::PowerJumpDown),
            (true, false) => push(EventKind::PowerJumpUp),
            _ => {}
        }
        if b.input.use_pressed {
            push(EventKind::Use);
        }
        match (a.input.grapple_held, b.input.grapple_held) {
            (false, true) => push(EventKind::GrappleButtonDown),
            (true, false) => push(EventKind::GrappleButtonUp),
            _ => {}
        }
        let attached = |s: &TraceSample| s.grapple_state == TraceGrappleState::Attached;
        match (attached(a), attached(b)) {
            (false, true) => push(EventKind::GrappleAttached),
            (true, false) if b.input.grapple_held => push(EventKind::GrappleReleasedHeld),
            (true, false) => push(EventKind::GrappleReleasedByButton),
            _ => {}
        }
        match (a.grounded, b.grounded) {
            (true, false) => push(EventKind::LeftGround),
            (false, true) => push(EventKind::Landed),
            _ => {}
        }
        if is_teleport(a, b) {
            push(EventKind::Teleport);
        }
    }
    out
}

/// The three values the pawn's own script and story mode give `GroundSpeed`
/// (ABILITIES.md A-WK-3), computed from `pawn` as the script does.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GroundSpeeds {
    /// `fMoveSpeed`.
    pub walk: f32,
    /// Walking speed × the sprint multiplier.
    pub sprint: f32,
    /// Walking speed × the story multiplier.
    pub story: f32,
}

impl GroundSpeeds {
    /// The speeds of `pawn`.
    #[must_use]
    pub fn of(pawn: &PawnParams) -> Self {
        let walk = pawn.move_speed.value;
        Self {
            walk,
            sprint: walk * pawn.sprint_speed_multiplier.value,
            story: walk * pawn.story_speed_multiplier.value,
        }
    }

    /// The original's: 440, 880 and 264 uu/s (class defaults; the Python
    /// converter has the same three numbers, and a test compares them).
    #[must_use]
    pub fn original() -> Self {
        PlayerParams::asamu_original().pawn.as_ref().map_or(
            Self {
                walk: 0.0,
                sprint: 0.0,
                story: 0.0,
            },
            Self::of,
        )
    }

    /// What a change of the recorded `GroundSpeed` from `old` to `new` is:
    /// nothing between the walking and the sprint speed (the pawn's own
    /// sprint), story mode on or off, or a console speed.
    #[must_use]
    pub fn change(&self, old: f32, new: f32) -> Option<EventKind> {
        let own = |g: f32| g == self.walk || g == self.sprint;
        if old == new || (own(old) && own(new)) {
            None
        } else if new == self.story {
            Some(EventKind::StoryModeOn)
        } else if old == self.story && own(new) {
            Some(EventKind::StoryModeOff)
        } else {
            Some(EventKind::GroundSpeedSet)
        }
    }
}

/// The level-script state changes and the counter-only attach between two
/// consecutive recorded states (`attached_rose`: the attached flag rose in
/// this tick's sample).
fn state_change_events(
    speeds: &GroundSpeeds,
    before: &TickState,
    after: &TickState,
    attached_rose: bool,
) -> Vec<EventKind> {
    let mut kinds = Vec::new();
    if let (Some(old), Some(new)) = (before.ground_speed, after.ground_speed)
        && let Some(kind) = speeds.change(old, new)
    {
        kinds.push(kind);
    }
    if let (Some(old), Some(new)) = (before.max_grapples, after.max_grapples)
        && old != new
    {
        kinds.push(EventKind::MaxGrapplesChanged);
    }
    match (before.boots_enabled, after.boots_enabled) {
        (Some(false), Some(true)) => kinds.push(EventKind::BootsEnabled),
        (Some(true), Some(false)) => kinds.push(EventKind::BootsDisabled),
        _ => {}
    }
    if let (Some(old), Some(new)) = (before.times_grappled, after.times_grappled)
        && new > old
        && !attached_rose
    {
        kinds.push(EventKind::GrappleAttachedWithinFrame);
    }
    kinds
}

/// [`events`] plus what only the recorded script state shows
/// (`timeline`, the trace's `state:` note): level-script state changes and
/// attaches that were released inside their frame (see the module docs).
/// In tick order; at one tick the events of the samples come first.
///
/// # Errors
/// An invalid `state:` note.
pub fn recorded_events(
    samples: &[TraceSample],
    timeline: Option<&StateTimeline>,
) -> Result<Vec<Event>> {
    let from_samples = events(samples);
    let Some(timeline) = timeline else {
        return Ok(from_samples);
    };
    let speeds = GroundSpeeds::original();
    let mut from_state: Vec<Event> = Vec::new();
    timeline.for_each_change(|tick, before, after| {
        let Ok(index) = samples.binary_search_by_key(&tick, |s| s.tick) else {
            return;
        };
        let attached = |i: usize| {
            samples
                .get(i)
                .is_some_and(|s| s.grapple_state == TraceGrappleState::Attached)
        };
        let attached_rose = attached(index) && index.checked_sub(1).is_some_and(|i| !attached(i));
        for kind in state_change_events(&speeds, before, after, attached_rose) {
            from_state.push(Event { index, tick, kind });
        }
    })?;
    // A stable merge by sample index.
    let mut out = Vec::with_capacity(from_samples.len() + from_state.len());
    let mut state = from_state.into_iter().peekable();
    for e in from_samples {
        while let Some(s) = state.next_if(|s| s.index < e.index) {
            out.push(s);
        }
        out.push(e);
    }
    out.extend(state);
    Ok(out)
}

/// Most ticks an `event:` note lists.
pub const MAX_LISTED: usize = 20;
/// Prefix of the converter's notes about [`recorded_events`].
pub const EVENT_NOTE_PREFIX: &str = "event: ";

/// `items` joined with `, `, at most [`MAX_LISTED`] of them, then how many
/// more there are.
fn bounded_list(items: &[String]) -> String {
    let shown = items.len().min(MAX_LISTED);
    let mut text = items.get(..shown).unwrap_or(items).join(", ");
    if items.len() > shown {
        let _ = write!(text, " and {} more", items.len() - shown);
    }
    text
}

/// The converter's `event:` notes for the events of a run: teleports,
/// level-script state changes and the attaches by the used-grapple counter
/// (how many, and where; the Python converter writes the same lines).
#[must_use]
pub fn event_notes(all: &[Event]) -> Vec<String> {
    let mut notes = Vec::new();
    let ticks = |keep: &dyn Fn(EventKind) -> bool| -> Vec<String> {
        all.iter()
            .filter(|e| keep(e.kind))
            .map(|e| e.tick.to_string())
            .collect()
    };
    let teleports = ticks(&|k| k == EventKind::Teleport);
    if !teleports.is_empty() {
        notes.push(format!(
            "{EVENT_NOTE_PREFIX}{} teleport(s) (the pawn moved more than {TELEPORT_SPEED} uu/s x \
             the frame length in one tick: a respawn or another script move): tick(s) {}",
            teleports.len(),
            bounded_list(&teleports)
        ));
    }
    let level: Vec<String> = all
        .iter()
        .filter(|e| e.kind.is_level_state())
        .map(|e| format!("{} {}", e.tick, e.kind.name()))
        .collect();
    if !level.is_empty() {
        notes.push(format!(
            "{EVENT_NOTE_PREFIX}{} level-script state change(s) (story mode, a console speed, \
             the grapple capacity or the rocket boots: changes the pawn's own rules do not \
             make): {}",
            level.len(),
            bounded_list(&level)
        ));
    }
    let attaches = all.iter().filter(|e| e.kind.is_attach()).count();
    let within = ticks(&|k| k == EventKind::GrappleAttachedWithinFrame);
    if !within.is_empty() {
        notes.push(format!(
            "{EVENT_NOTE_PREFIX}{attaches} grapple attach(es), {} of them inside one frame (the \
             used-grapple counter iTimesGrappled rose, no sample is attached): tick(s) {}",
            within.len(),
            bounded_list(&within)
        ));
    } else if attaches > 0 {
        notes.push(format!(
            "{EVENT_NOTE_PREFIX}{attaches} grapple attach(es), each with an attached sample"
        ));
    }
    notes
}

/// The events of `samples[from..until]`: with `every_event` one per line,
/// else one line with, per kind, how often and where first.
fn activity(out: &mut String, all: &[Event], from: usize, until: usize, every_event: bool) {
    let inside: Vec<&Event> = all
        .iter()
        .filter(|e| e.index >= from && e.index < until)
        .collect();
    if inside.is_empty() {
        let _ = writeln!(out, " no event");
        return;
    }
    if every_event {
        let _ = writeln!(out);
        for e in inside {
            let _ = writeln!(out, "        {} {}", e.tick, e.kind.name());
        }
        return;
    }
    let mut kinds: Vec<(EventKind, usize, u64)> = Vec::new();
    for e in inside {
        match kinds.iter_mut().find(|k| k.0 == e.kind) {
            Some(k) => k.1 += 1,
            None => kinds.push((e.kind, 1, e.tick)),
        }
    }
    let text: Vec<String> = kinds
        .iter()
        .map(|(kind, n, tick)| {
            if *n == 1 {
                format!("{} at {tick}", kind.name())
            } else {
                format!("{} x{n} (first at {tick})", kind.name())
            }
        })
        .collect();
    let _ = writeln!(out, " {}", text.join(", "));
}

/// The line(s) for the samples between two stretches, `samples[from..until]`.
fn between(
    out: &mut String,
    samples: &[TraceSample],
    all: &[Event],
    from: usize,
    until: usize,
    every_event: bool,
) {
    let (Some(first), Some(last)) = (
        samples.get(from),
        until.checked_sub(1).and_then(|i| samples.get(i)),
    ) else {
        return;
    };
    if from >= until {
        return;
    }
    let _ = write!(out, "    {}..={}:", first.tick, last.tick);
    activity(out, all, from, until, every_event);
}

/// The listing of `asamu-trace starts`: every stretch of clean starts with
/// the recorded script state at its first tick and the events inside it (a
/// sprint button, `use`), and what happens between the stretches (per kind
/// of event: how often and where first; with `every_event` each event on
/// its own line).
///
/// # Errors
/// An invalid `state:` note.
pub fn describe(trace: &Trace, every_event: bool) -> Result<String> {
    let samples = trace.samples.as_slice();
    let runs = clean_runs(samples);
    let timeline = StateTimeline::from_notes(&trace.meta.notes)?;
    let all = recorded_events(samples, timeline.as_ref())?;
    let params = PlayerParams::asamu_original();
    let mut out = String::new();
    let _ = writeln!(
        out,
        "{} samples, ticks {}..={}; {} standing-still stretch(es) (a replay starts at a tick \
         inside one), {} event(s) of input or state",
        samples.len(),
        samples.first().map_or(0, |s| s.tick),
        samples.last().map_or(0, |s| s.tick),
        runs.len(),
        all.len()
    );
    let mut cursor = 0;
    for (a, b) in &runs {
        between(&mut out, samples, &all, cursor, *a, every_event);
        let (Some(first), Some(last)) = (samples.get(*a), samples.get(*b)) else {
            continue;
        };
        let _ = write!(
            out,
            "standing still {}..={} ({} ticks)",
            first.tick,
            last.tick,
            b - a + 1
        );
        if let Some(t) = &timeline
            && let Some(st) = t.at(first.tick)?
        {
            if let Some(g) = st.ground_speed {
                let story = match params.pawn.as_ref().and_then(|p| story_mode_shown(g, p)) {
                    Some(true) => "story mode on",
                    Some(false) => "story mode off",
                    None => "story mode not shown",
                };
                let _ = write!(out, ": GroundSpeed {g} ({story})");
            }
            if let Some(v) = st.air_control {
                let _ = write!(out, ", AirControl {v}");
            }
            if let (Some(used), Some(max)) = (st.times_grappled, st.max_grapples) {
                let _ = write!(out, ", grapples used {used} of {max}");
            }
            if st.boots_enabled == Some(true) {
                let _ = write!(out, ", rocket boots");
            }
            if let Some(base) = &st.base {
                let _ = write!(out, ", on {base}");
            }
        }
        let _ = writeln!(out);
        if all.iter().any(|e| e.index > *a && e.index <= *b) {
            let _ = write!(out, "    while standing:");
            activity(&mut out, &all, a + 1, b + 1, every_event);
        }
        cursor = b + 1;
    }
    between(&mut out, samples, &all, cursor, samples.len(), every_event);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use asamu_player::InputFrame;

    fn standing(tick: u64) -> TraceSample {
        TraceSample {
            tick,
            time: tick as f64 / 60.0,
            input: InputFrame::default(),
            position: Vec3::new(1.0, 2.0, 3.0),
            velocity: Vec3::ZERO,
            yaw: 0.0,
            pitch: 0.0,
            fov: 90.0,
            grapple_state: TraceGrappleState::Idle,
            grapple_anchor: None,
            rope_length: None,
            grounded: true,
        }
    }

    #[test]
    fn quiet_and_clean() {
        let mut s: Vec<TraceSample> = (0..12).map(standing).collect();
        assert!((0..12).all(|i| is_clean_start(&s, i)));
        assert_eq!(clean_runs(&s), [(0, 11)]);
        // Looking around and a held sprint button stay quiet.
        s[1].input.look_yaw_delta = 0.3;
        s[1].input.sprint_held = true;
        assert!(is_quiet(&s[1]));
        // Everything else does not, and the tick after it is not clean.
        s[3].input.move_right = -0.2;
        s[5].velocity.z = -1e-30;
        s[7].input.power_jump_held = true;
        s[9].grounded = false;
        assert_eq!(clean_runs(&s), [(0, 2), (11, 11)]);
        assert_eq!(not_clean_reasons(&s, 3), ["a move input is held"]);
        assert_eq!(
            not_clean_reasons(&s, 4),
            ["the tick before it is not standing still"]
        );
        assert_eq!(not_clean_reasons(&s, 5), ["the pawn is moving"]);
        assert_eq!(not_clean_reasons(&s, 9), ["the pawn is in the air"]);
        assert_eq!(not_clean_reasons(&s, 99), ["no such sample"]);
        assert!(not_clean_reasons(&s, 0).is_empty());
        assert_eq!(next_clean_start(&s, 3), Some(11));
        assert_eq!(next_clean_start(&s, 0), Some(0));
        assert_eq!(previous_clean_start(&s, 10), Some(2));
        assert_eq!(previous_clean_start(&s, 500), Some(11));
        assert_eq!(next_clean_start(&s, 12), None);
        assert_eq!(previous_clean_start(&[], 0), None);
        let mut t = standing(0);
        t.grapple_state = TraceGrappleState::Attached;
        t.grounded = false;
        t.input.grapple_held = true;
        t.input.jump_pressed = true;
        assert_eq!(
            not_quiet_reasons(&t),
            [
                "the grapple is attached",
                "the jump button is held",
                "the grapple button is held"
            ]
        );
        // A tick gap: the sample after it has no known tick before it.
        let mut g: Vec<TraceSample> = (0..6).map(standing).collect();
        for x in &mut g[3..] {
            x.tick += 10;
        }
        assert_eq!(clean_runs(&g), [(0, 2), (4, 5)]);
    }

    #[test]
    fn events_in_order() {
        let mut s: Vec<TraceSample> = (0..12).map(standing).collect();
        for x in &mut s[2..6] {
            x.input.move_forward = 1.0;
        }
        s[3].input.sprint_held = true;
        s[4].input.jump_pressed = true;
        for x in &mut s[4..8] {
            x.grounded = false;
        }
        for x in &mut s[5..7] {
            x.grapple_state = TraceGrappleState::Attached;
            x.input.grapple_held = true;
        }
        s[7].input.grapple_held = true;
        s[9].input.power_jump_held = true;
        s[10].input.use_pressed = true;
        let got: Vec<(u64, EventKind)> = events(&s).iter().map(|e| (e.tick, e.kind)).collect();
        use EventKind::*;
        assert_eq!(
            got,
            [
                (2, MoveBegins),
                (3, SprintDown),
                (4, SprintUp),
                (4, Jump),
                (4, LeftGround),
                (5, GrappleButtonDown),
                (5, GrappleAttached),
                (6, MoveEnds),
                (7, GrappleReleasedHeld),
                (8, GrappleButtonUp),
                (8, Landed),
                (9, PowerJumpDown),
                (10, PowerJumpUp),
                (10, Use),
            ]
        );
        s[7].input.grapple_held = false;
        assert!(
            events(&s)
                .iter()
                .any(|e| e.tick == 7 && e.kind == GrappleReleasedByButton)
        );
        assert_eq!(
            GrappleReleasedHeld.name(),
            "grapple released with the button held"
        );
        assert!(events(&s[..1]).is_empty());

        // The listing: two stretches, the activity between them, the state
        // of the timeline at each stretch's first tick.
        let mut t = Trace::new(asamu_player::TraceMeta::runtime(None, Some(60.0)));
        t.meta.notes.push(
            "state: {\"changes\":[[0,{\"ground_speed\":264.0,\"air_control\":0.3,\
             \"times_grappled\":0,\"max_grapples\":2,\"base\":\"Floor_1\"}],\
             [9,{\"ground_speed\":132.0,\"times_grappled\":1,\"base\":null}]],\"v\":1}"
                .to_owned(),
        );
        t.samples = s;
        t.samples[1].input.sprint_held = true;
        let text = describe(&t, false).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 5, "{text}");
        assert!(lines[0].starts_with("12 samples, ticks 0..=11; 2 standing-still stretch(es)"));
        assert_eq!(
            lines[1],
            "standing still 0..=1 (2 ticks): GroundSpeed 264 (story mode on), AirControl 0.3, \
             grapples used 0 of 2, on Floor_1"
        );
        assert_eq!(lines[2], "    while standing: sprint at 1");
        assert_eq!(
            lines[3],
            "    2..=10: move at 2, sprint ends x2 (first at 2), sprint at 3, jump at 4, left the ground \
             at 4, grapple button down at 5, grapple attached at 5, move ends at 6, grapple \
             button up at 7, grapple released by the button at 7, landed at 8, power-jump \
             button down at 9, GroundSpeed set (a console speed) at 9, grapple attached and \
             released within the frame at 9, power-jump button up at 10, use at 10"
        );
        assert_eq!(
            lines[4],
            "standing still 11..=11 (1 ticks): GroundSpeed 132 (story mode not shown), \
             AirControl 0.3, grapples used 1 of 2"
        );
        let every = describe(&t, true).unwrap();
        assert!(
            every.contains("\n        7 grapple released by the button\n"),
            "{every}"
        );
        // The timeline's own events (a console speed and a counted attach at
        // tick 9) are listed with the samples' events.
        assert!(
            every.contains("\n        9 grapple attached and released within the frame\n"),
            "{every}"
        );
        assert_eq!(every.lines().count(), 3 + 1 + 17 + 2);
    }

    /// 1/64 s (exact in binary): [`TELEPORT_SPEED`] allows 156.25 uu.
    const TICK: f64 = 0.015_625;

    fn at(tick: u64, x: f32) -> TraceSample {
        let mut s = standing(tick);
        s.time = tick as f64 * TICK;
        s.position = Vec3::new(x, 2.0, 3.0);
        s
    }

    #[test]
    fn teleports() {
        assert_eq!(
            TELEPORT_SPEED,
            f64::from(
                PlayerParams::asamu_original()
                    .movement
                    .terminal_velocity
                    .value
            ),
            "the teleport rule's speed is the falling physics' clamp"
        );
        // 156 uu in a tick is a fast fall, 156.5 uu is not physics.
        assert!(!is_teleport(&at(0, 0.0), &at(1, 156.0)));
        assert!(is_teleport(&at(0, 0.0), &at(1, 156.5)));
        assert!(is_teleport(&at(0, 0.0), &at(1, -20_000.0)));
        assert_eq!(tick_displacement(&at(0, 1.0), &at(1, 4.0)), 3.0);
        // Not across a tick gap (the frames in between are unknown).
        assert!(!is_teleport(&at(0, 0.0), &at(2, 9000.0)));
        // A tick in which no time passes, or time runs back: any move.
        let mut frozen = at(1, 0.5);
        frozen.time = 0.0;
        assert!(is_teleport(&at(0, 0.0), &frozen));
        frozen.time = -1.0;
        assert!(is_teleport(&at(0, 0.0), &frozen));
        frozen.position.x = 0.0;
        assert!(!is_teleport(&at(0, 0.0), &frozen));
        // Huge times and positions: no panic, a finite answer.
        let mut far = at(1, f32::MAX);
        far.time = 1e308;
        let mut origin = at(0, -f32::MAX);
        origin.time = -1e308;
        assert!(!is_teleport(&origin, &far), "an infinite frame length");
        far.time = origin.time;
        assert!(is_teleport(&origin, &far));

        // In a listing: an event, and the standing stretch does not span it.
        let mut s: Vec<TraceSample> = (0..10).map(|t| at(t, 0.0)).collect();
        for x in &mut s[5..] {
            x.position.x = 5000.0;
        }
        let got: Vec<(u64, EventKind)> = events(&s).iter().map(|e| (e.tick, e.kind)).collect();
        assert_eq!(got, [(5, EventKind::Teleport)]);
        assert!(EventKind::Teleport.ends_validity());
        assert!(!EventKind::Teleport.is_level_state());
        assert_eq!(clean_runs(&s), [(0, 4), (6, 9)]);
        assert_eq!(
            not_clean_reasons(&s, 5),
            ["the pawn was teleported in this tick"]
        );
        assert!(is_clean_start(&s, 6), "the tick after it is a start again");
    }

    fn timeline(json: &str) -> StateTimeline {
        StateTimeline::from_notes(&[format!("state: {json}")])
            .unwrap()
            .unwrap()
    }

    #[test]
    fn level_state_changes_and_counted_attaches() {
        let speeds = GroundSpeeds::original();
        assert_eq!(
            (speeds.walk, speeds.sprint, speeds.story),
            (440.0, 880.0, 264.0),
            "the Python converter has these three numbers"
        );
        use EventKind::*;
        for (old, new, want) in [
            (440.0, 880.0, None),
            (880.0, 440.0, None),
            (440.0, 440.0, None),
            (132.0, 132.0, None),
            (440.0, 264.0, Some(StoryModeOn)),
            (880.0, 264.0, Some(StoryModeOn)),
            (132.0, 264.0, Some(StoryModeOn)),
            (264.0, 440.0, Some(StoryModeOff)),
            (264.0, 880.0, Some(StoryModeOff)),
            (264.0, 132.0, Some(GroundSpeedSet)),
            (440.0, 132.0, Some(GroundSpeedSet)),
            (132.0, 440.0, Some(GroundSpeedSet)),
            (66.0, 132.0, Some(GroundSpeedSet)),
        ] {
            assert_eq!(speeds.change(old, new), want, "{old} -> {new}");
        }
        // Ten standing samples; the grapple is attached on ticks 4 and 5.
        let mut s: Vec<TraceSample> = (0..10).map(|t| at(t, 0.0)).collect();
        for x in &mut s[4..6] {
            x.grapple_state = TraceGrappleState::Attached;
            x.grapple_anchor = Some(Vec3::X);
            x.grounded = false;
        }
        let t = timeline(
            "{\"changes\":[[0,{\"ground_speed\":440.0,\"max_grapples\":2,\"times_grappled\":0,\
             \"boots_enabled\":true}],[1,{\"ground_speed\":880.0}],[2,{\"ground_speed\":264.0}],\
             [3,{\"times_grappled\":1}],[4,{\"ground_speed\":440.0,\"times_grappled\":2}],\
             [5,{\"ground_speed\":132.0,\"times_grappled\":0}],[6,{\"max_grapples\":3}],\
             [7,{\"boots_enabled\":false}],[8,{\"boots_enabled\":true}],\
             [9,{\"max_grapples\":null}],[40,{\"ground_speed\":264.0}]],\"v\":1}",
        );
        let all = recorded_events(&s, Some(&t)).unwrap();
        let got: Vec<(u64, EventKind)> = all.iter().map(|e| (e.tick, e.kind)).collect();
        assert_eq!(
            got,
            [
                (2, StoryModeOn),
                (3, GrappleAttachedWithinFrame),
                (4, GrappleAttached),
                (4, LeftGround),
                (4, StoryModeOff),
                (5, GroundSpeedSet),
                (6, GrappleReleasedByButton),
                (6, Landed),
                (6, MaxGrapplesChanged),
                (7, BootsDisabled),
                (8, BootsEnabled),
            ],
            "the sprint at tick 1, the refill at 5, a gun that goes away at 9 and a tick the \
             trace does not have are no events"
        );
        assert!(all.iter().all(|e| s[e.index].tick == e.tick));
        let ends: Vec<u64> = all
            .iter()
            .filter(|e| e.kind.ends_validity())
            .map(|e| e.tick)
            .collect();
        assert_eq!(ends, [2, 4, 5, 6, 7, 8]);
        assert_eq!(all.iter().filter(|e| e.kind.is_attach()).count(), 2);
        assert_eq!(
            event_notes(&all),
            [
                "event: 6 level-script state change(s) (story mode, a console speed, the grapple \
                 capacity or the rocket boots: changes the pawn's own rules do not make): 2 story \
                 mode on, 4 story mode off, 5 GroundSpeed set (a console speed), 6 grapple \
                 capacity changed, 7 rocket boots disabled, 8 rocket boots enabled",
                "event: 2 grapple attach(es), 1 of them inside one frame (the used-grapple \
                 counter iTimesGrappled rose, no sample is attached): tick(s) 3",
            ]
        );
        // Without a timeline: the samples' events only.
        let plain = recorded_events(&s, None).unwrap();
        assert_eq!(plain, events(&s));
        assert_eq!(
            event_notes(&plain),
            ["event: 1 grapple attach(es), each with an attached sample"]
        );
        assert!(event_notes(&[]).is_empty());
        // A long list is cut after MAX_LISTED ticks.
        let mut jumpy: Vec<TraceSample> = (0..30).map(|t| at(t, 400.0 * t as f32)).collect();
        jumpy[0].position.x = 0.0;
        let notes = event_notes(&events(&jumpy));
        assert_eq!(notes.len(), 1, "{notes:#?}");
        assert!(
            notes[0].starts_with(
                "event: 29 teleport(s) (the pawn moved more than 10000 uu/s x the frame length \
                 in one tick: a respawn or another script move): tick(s) 1, 2, "
            ) && notes[0].ends_with("19, 20 and 9 more"),
            "{notes:#?}"
        );
        // A broken timeline is an error.
        let bad = timeline(
            "{\"changes\":[[0,{\"ground_speed\":1.0}],[1,{\"ground_speed\":\"x\"}]],\"v\":1}",
        );
        assert!(recorded_events(&s, Some(&bad)).is_err());
    }
}
