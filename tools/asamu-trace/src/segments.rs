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
//! - a sample is a *clean start* when it is quiet and the sample before it
//!   (if the trace has one, without a tick gap) is quiet too, so that
//!   anything a button release one tick earlier set off has shown.
//!
//! [`clean_runs`] lists the stretches of clean starts, [`events`] what the
//! player did, both for picking segments (`asamu-trace starts`).

use std::fmt::Write as _;

use anyhow::Result;
use asamu_core::glam::Vec3;
use asamu_player::trace::{TraceGrappleState, TraceSample};
use asamu_player::{PlayerParams, Trace};

use crate::state::{StateTimeline, story_mode_shown};

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
        Some(before) => before.tick.checked_add(1) == Some(s.tick) && is_quiet(before),
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
        && !(before.tick.checked_add(1) == Some(s.tick) && is_quiet(before))
    {
        why.push("the tick before it is not standing still");
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
        }
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
    }
    out
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
    let all = events(samples);
    let timeline = StateTimeline::from_notes(&trace.meta.notes)?;
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
             button down at 9, power-jump button up at 10, use at 10"
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
        assert_eq!(every.lines().count(), 3 + 1 + 15 + 2);
    }
}
