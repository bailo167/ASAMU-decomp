//! Sandbox hotkeys: keys become `LabRequest`s, and the debug keys Classic
//! reads (F2, F3, F4, F6, F7, R, F9) are consumed while a session runs, so
//! they act as logged commands instead.
//!
//! [`hotkeys`] runs in `LabSet::Input`, before `cursor_and_pause` sees the
//! keys of the frame, and only while a session is active: in a Classic
//! process the keys are never touched. The Sandbox's own keys work in play
//! (no screen open, mouse captured); the inspector key also works with the
//! mouse free, and closes the inspector again.
//!
//! | Key | Action |
//! |---|---|
//! | `` ` `` or F5 | the tools: open or close the inspector (Esc closes it too); opening it also switches the read-outs and drawings on |
//! | `[` / `]` | previous / next pinned parameter (quick tuner) |
//! | `-` / `=` | step the selected parameter (Left Alt x10, Left Ctrl x0.1) |
//! | Backspace | reset the selected parameter (Left Alt: reset all) |
//! | P / O | freeze / single step (Left Alt: ten steps) |
//! | `,` / `.` / `/` | slower / faster / back to 1x |
//! | 1-4, K, L | select a save-state slot, save, load |
//! | Z | rewind one keyframe (hand-made levels) |
//! | T / G | teleport to the crosshair hit / next teleport target |
//! | B / V | set the "quick" bookmark / return to it |
//! | N | freeze-and-fly placement |
//! | H | read-outs and drawings on or off (off is the clean screen a session starts with) |
//! | F9 | Sandbox recording |
//! | F2, F3, F4, F6 | story mode, grapple capacity, rocket boots, attractor pads: as today, as logged commands |
//! | F7, R | quick load / respawn: as today, as a logged command |
//!
//! None of these is read by the app otherwise (W A S D, arrows, Space, Left
//! Shift, Left Ctrl, E, Q, Enter, Esc, Tab, F1, F8, F10 and F12 stay what
//! they are).
//!
//! For unattended runs (layout checks, screenshots) the environment variable
//! [`VIEW_ENV`] plays a short list of steps in a running session, standing
//! in for the inspector key, the tab buttons and the forward key. It cannot
//! start a session and does nothing without one.

use asamu_core::glam as sim_glam;
use asamu_sandbox::command::{Command, SlotOp, TeleportTarget, TimeOp, Toggle};
use asamu_sandbox::playground::playground;
use asamu_sandbox::snapshot::SLOT_COUNT;
use bevy::prelude::*;
use bevy::window::{CursorGrabMode, CursorOptions, PrimaryWindow};

use super::widgets::{Modifiers, QUICK_MARK, ViewState};
use super::{Lab, LabRequest, LabSet, PanelState, PanelTab, VizSettings, lab_active};
use crate::ui::{Screen, UiState};
use crate::{PendingInput, Sim};

/// The keys `cursor_and_pause` gives a debug meaning in Classic. While a
/// session runs they are taken away from it (their "just pressed" state is
/// cleared) and act through the session instead.
pub(super) const CONSUMED: [KeyCode; 7] = [
    KeyCode::F2,
    KeyCode::F3,
    KeyCode::F4,
    KeyCode::F6,
    KeyCode::F7,
    KeyCode::KeyR,
    KeyCode::F9,
];

/// The keys that select a save-state slot.
const SLOT_KEYS: [KeyCode; SLOT_COUNT] = [
    KeyCode::Digit1,
    KeyCode::Digit2,
    KeyCode::Digit3,
    KeyCode::Digit4,
];

/// Where the keys of this frame act.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mode {
    /// No screen open, mouse captured: every Sandbox key works.
    Play,
    /// No screen open, mouse free: only the inspector key.
    Free,
    /// The inspector is open: the keys that close it.
    Inspector,
    /// Another screen (pause menu, settings, loading): nothing.
    Other,
}

/// What a key asks for.
#[derive(Clone, Debug, PartialEq)]
enum KeyAct {
    /// A command for the session.
    Do(Command),
    /// Open the inspector.
    OpenInspector,
    /// Close the inspector.
    CloseInspector,
    /// Move the quick tuner's selection.
    Pinned(i32),
    /// HUD read-outs on or off.
    Hud,
}

/// What the keys just pressed ask for in `mode`. `pinned` is the quick
/// tuner's selected parameter, `slot` the selected save-state slot.
fn key_acts(
    keys: &ButtonInput<KeyCode>,
    mode: Mode,
    pinned: Option<&str>,
    slot: usize,
) -> Vec<KeyAct> {
    let mut out = Vec::new();
    let down = |key: KeyCode| keys.just_pressed(key);
    let inspector = down(KeyCode::Backquote) || down(KeyCode::F5);
    match mode {
        Mode::Other => return out,
        Mode::Inspector => {
            if inspector || down(KeyCode::Escape) {
                out.push(KeyAct::CloseInspector);
            }
            return out;
        }
        Mode::Free => {
            if inspector {
                out.push(KeyAct::OpenInspector);
            }
            return out;
        }
        Mode::Play => {}
    }
    if inspector {
        out.push(KeyAct::OpenInspector);
    }
    let modifiers = Modifiers::held(keys);
    let mut run = |command: Command| out.push(KeyAct::Do(command));

    // The quick tuner.
    if let Some(key) = pinned {
        for (code, steps) in [(KeyCode::Minus, -1), (KeyCode::Equal, 1)] {
            if down(code) {
                run(Command::NudgeParam {
                    key: key.to_owned(),
                    steps,
                    scale: modifiers.scale(),
                });
            }
        }
    }
    if down(KeyCode::Backspace) {
        if modifiers.coarse {
            run(Command::ResetAllParams);
        } else if let Some(key) = pinned {
            run(Command::ResetParam {
                key: key.to_owned(),
            });
        }
    }

    // Time.
    let time = |op: TimeOp| Command::Time { op };
    if down(KeyCode::KeyP) {
        run(time(TimeOp::Freeze(Toggle::Toggle)));
    }
    if down(KeyCode::KeyO) {
        run(time(TimeOp::Step {
            n: if modifiers.coarse { 10 } else { 1 },
        }));
    }
    if down(KeyCode::Comma) {
        run(time(TimeOp::Slower));
    }
    if down(KeyCode::Period) {
        run(time(TimeOp::Faster));
    }
    if down(KeyCode::Slash) {
        run(time(TimeOp::Scale { value: 1.0 }));
    }

    // Save states.
    for (index, code) in SLOT_KEYS.into_iter().enumerate() {
        if down(code) {
            run(Command::Slot {
                op: SlotOp::Select { slot: index },
            });
        }
    }
    if down(KeyCode::KeyK) {
        run(Command::Slot {
            op: SlotOp::Save { slot },
        });
    }
    if down(KeyCode::KeyL) {
        run(Command::Slot {
            op: SlotOp::Load { slot },
        });
    }
    if down(KeyCode::KeyZ) {
        run(Command::Rewind);
    }

    // Placement.
    if down(KeyCode::KeyT) {
        run(Command::Teleport {
            to: TeleportTarget::AimPoint,
        });
    }
    if down(KeyCode::KeyG) {
        run(Command::Teleport {
            to: TeleportTarget::NextTarget,
        });
    }
    if down(KeyCode::KeyB) {
        run(Command::SetMark {
            name: QUICK_MARK.to_owned(),
        });
    }
    if down(KeyCode::KeyV) {
        run(Command::Teleport {
            to: TeleportTarget::Mark {
                name: QUICK_MARK.to_owned(),
            },
        });
    }
    if down(KeyCode::KeyN) {
        run(Command::Fly { on: Toggle::Toggle });
    }

    // The keys Classic gives a debug meaning: the same effects, through the
    // session, so they are logged (and refused where a rule pins the
    // ability).
    if down(KeyCode::F9) {
        run(Command::Record { on: Toggle::Toggle });
    }
    if down(KeyCode::F2) {
        run(Command::StoryMode { on: Toggle::Toggle });
    }
    if down(KeyCode::F3) {
        run(Command::CycleGrapples);
    }
    if down(KeyCode::F4) {
        run(Command::RocketBoots { on: Toggle::Toggle });
    }
    if down(KeyCode::F6) {
        run(Command::ActivateAttractors);
    }
    // Quick load on a converted level, respawn on a hand-made one: what R
    // and F7 do today.
    if down(KeyCode::F7) || down(KeyCode::KeyR) {
        run(Command::Kill);
    }

    // The view's own keys.
    if down(KeyCode::BracketLeft) {
        out.push(KeyAct::Pinned(-1));
    }
    if down(KeyCode::BracketRight) {
        out.push(KeyAct::Pinned(1));
    }
    if down(KeyCode::KeyH) {
        out.push(KeyAct::Hud);
    }
    out
}

/// The pinned index `steps` further on, wrapping around `count` entries.
fn wrapped(index: usize, steps: i32, count: usize) -> usize {
    if count == 0 {
        return 0;
    }
    let count = count as i64;
    (index as i64 + i64::from(steps)).rem_euclid(count) as usize
}

/// Environment variable for unattended looks at the Sandbox (layout checks,
/// screenshots), in the way `ASAMU_MENU_ACTION` drives the Classic menu: a
/// comma-separated list of steps, played once the first session of the
/// process is running. A step lasts [`VIEW_FRAMES`] frames, or the seconds
/// of real time written after an `@` (`at:wheel@4`).
///
/// | Step | What it does |
/// |---|---|
/// | `tune`, `rules`, `time`, `view`, `profiles`, `stage` | the inspector on that tab (`tune:<group index>` picks a parameter group) |
/// | `play` | the inspector closed |
/// | `show` | the inspector closed, the read-outs and every visualiser switched on |
/// | `walk` | the inspector closed, forward held for the step |
/// | `respawn` | the inspector closed, one `respawn` command |
/// | `at:<station>` | the inspector closed, the pawn placed at a station of the playground (`hub`, `near`, `medium`, `far`, `crystals`, `runway`, `ferry`, `yard`, `carousel`, `wheel`, `tower`) |
/// | `pull:<piece>` | the inspector closed, the view turned to a piece of the playground (`near hook 1`, `carousel hook 1`, ...) and the grapple held for the step |
///
/// It stands in for the inspector key, the tab buttons, the W key, the
/// mouse button and a few commands, all inside a session: it does nothing
/// without a session and cannot start one. A developer's tool: the two
/// playground steps place the pawn and turn its view through the session's
/// own teleport command, which is logged like any other.
pub(super) const VIEW_ENV: &str = "ASAMU_LAB_VIEW";

/// Frames a step of [`VIEW_ENV`] lasts when it names no time.
const VIEW_FRAMES: u32 = 180;
/// Longest a step may ask to last, seconds.
const VIEW_SECONDS_MAX: f32 = 600.0;

/// A step of [`VIEW_ENV`].
#[derive(Clone, Debug, PartialEq)]
enum View {
    /// The inspector on a tab (and, for Tune, a parameter group).
    Tab(PanelTab, Option<usize>),
    /// The inspector closed.
    Play,
    /// The inspector closed, the read-outs and every visualiser on.
    Show,
    /// The inspector closed, forward held.
    Walk,
    /// The inspector closed, one respawn.
    Respawn,
    /// The inspector closed, the pawn at a station of the playground.
    At(String),
    /// The inspector closed, the view on a piece of the playground and the
    /// grapple held.
    Pull(String),
}

/// A step and how long it lasts (`None`: [`VIEW_FRAMES`] frames).
type TimedView = (View, Option<f32>);

/// Parses a [`VIEW_ENV`] value (`None`: not understood, nothing is played).
fn parse_views(value: &str) -> Option<Vec<TimedView>> {
    value.split(',').map(parse_view).collect()
}

fn parse_view(value: &str) -> Option<TimedView> {
    let value = value.trim();
    let (value, seconds) = match value.rsplit_once('@') {
        Some((step, seconds)) => {
            let seconds = seconds
                .trim()
                .parse::<f32>()
                .ok()
                .filter(|s| s.is_finite() && *s > 0.0 && *s <= VIEW_SECONDS_MAX)?;
            (step.trim(), Some(seconds))
        }
        None => (value, None),
    };
    let named = |name: &str| {
        let name = name.trim();
        (!name.is_empty() && name.len() <= 64).then(|| name.to_owned())
    };
    let view = match value {
        "play" => View::Play,
        "show" => View::Show,
        "walk" => View::Walk,
        "respawn" => View::Respawn,
        _ => {
            if let Some(station) = value.strip_prefix("at:") {
                View::At(named(station)?)
            } else if let Some(piece) = value.strip_prefix("pull:") {
                View::Pull(named(piece)?)
            } else {
                let (name, group) = match value.split_once(':') {
                    Some((name, group)) => (name, Some(group.trim().parse::<usize>().ok()?)),
                    None => (value, None),
                };
                let tab = match name.trim() {
                    "tune" => PanelTab::Tune,
                    "rules" => PanelTab::Rules,
                    "time" => PanelTab::Time,
                    "view" => PanelTab::View,
                    "profiles" => PanelTab::Profiles,
                    "stage" => PanelTab::Stage,
                    _ => return None,
                };
                // Only the Tune tab has groups.
                if group.is_some() && tab != PanelTab::Tune {
                    return None;
                }
                View::Tab(tab, group)
            }
        }
    };
    Some((view, seconds))
}

/// The unattended steps still to play.
#[derive(Default)]
struct Tour {
    read: bool,
    views: Vec<TimedView>,
    next: usize,
    frames: u32,
    seconds: f32,
    /// The tour holds the forward key.
    walking: bool,
    /// The tour holds the grapple button.
    pulling: bool,
}

impl Tour {
    /// The step this frame belongs to, and whether this is its first frame.
    /// `delta`: the frame's real time, seconds.
    fn step(&mut self, delta: f32) -> Option<(View, bool)> {
        if !self.read {
            self.read = true;
            if let Ok(value) = std::env::var(VIEW_ENV) {
                match parse_views(&value) {
                    Some(views) => {
                        info!("{VIEW_ENV}: playing {views:?}");
                        self.views = views;
                    }
                    None => warn!("{VIEW_ENV} {value:?} not understood"),
                }
            }
        }
        let (view, length) = self.views.get(self.next)?.clone();
        let first = self.frames == 0;
        self.frames = self.frames.saturating_add(1);
        if delta.is_finite() && delta > 0.0 {
            self.seconds += delta;
        }
        let over = match length {
            Some(seconds) => self.seconds >= seconds,
            None => self.frames >= VIEW_FRAMES,
        };
        if over {
            self.frames = 0;
            self.seconds = 0.0;
            self.next += 1;
        }
        Some((view, first))
    }
}

/// Where the piece `name` of the playground is now: where the level script
/// has it, if it moves.
fn piece_now(sim: &Sim, name: &str) -> Option<sim_glam::Vec3> {
    let piece = playground().piece(name)?;
    let moved = piece
        .actor
        .zip(sim.script.as_ref())
        .and_then(|(actor, script)| script.mover_location(actor));
    Some(moved.unwrap_or(piece.location))
}

/// The teleport that leaves the pawn where it is and turns its view to
/// `target`.
fn look_at(sim: &Sim, target: sim_glam::Vec3) -> Command {
    let d = target - sim.game.eye_position();
    Command::Teleport {
        to: TeleportTarget::Position {
            position: sim.game.player().position.to_array(),
            yaw: Some(d.y.atan2(d.x)),
            pitch: Some((d.z / d.length().max(1.0)).clamp(-1.0, 1.0).asin()),
        },
    }
}

/// Keys to requests, before `cursor_and_pause` reads them.
#[allow(clippy::too_many_arguments)]
fn hotkeys(
    mut keys: ResMut<ButtonInput<KeyCode>>,
    mut mouse: Option<ResMut<ButtonInput<MouseButton>>>,
    mut pending: Option<ResMut<PendingInput>>,
    state: Option<Res<UiState>>,
    cursor: Query<&CursorOptions, With<PrimaryWindow>>,
    lab: Res<Lab>,
    sim: Option<Res<Sim>>,
    real: Option<Res<Time<Real>>>,
    mut panel: ResMut<PanelState>,
    mut view: ResMut<ViewState>,
    mut viz: ResMut<VizSettings>,
    mut requests: MessageWriter<LabRequest>,
    mut tour: Local<Tour>,
) {
    // Unattended looks at the Sandbox (`VIEW_ENV`; nothing without it).
    let step = tour.step(real.as_ref().map_or(0.0, |real| real.delta_secs()));
    if let Some((shown, true)) = &step {
        match shown {
            View::Tab(tab, group) => {
                panel.tab = *tab;
                if let Some(group) = group {
                    panel.group = *group;
                }
                if !panel.open {
                    requests.write(LabRequest::OpenInspector);
                }
                // As the tools key does.
                if !view.hud {
                    view.hud = true;
                }
            }
            View::Play | View::Show | View::Walk | View::Respawn | View::At(_) | View::Pull(_) => {
                if panel.open {
                    requests.write(LabRequest::CloseInspector);
                }
            }
        }
        match shown {
            View::Show => {
                if !view.hud {
                    view.hud = true;
                }
                viz.set_if_neq(VizSettings {
                    velocity: true,
                    cylinder: true,
                    aim: true,
                    trail: true,
                    attempts: true,
                    prediction: true,
                    markers: true,
                    graphs: true,
                });
            }
            View::Respawn => {
                requests.write(LabRequest::Do(Command::Respawn));
            }
            View::At(id) => match playground().station(id) {
                Some(station) => {
                    requests.write(LabRequest::Do(Command::Teleport {
                        to: station.teleport(),
                    }));
                }
                None => warn!("{VIEW_ENV}: the playground has no station {id:?}"),
            },
            View::Pull(name) => {
                match sim
                    .as_deref()
                    .and_then(|sim| Some((sim, piece_now(sim, name)?)))
                {
                    Some((sim, target)) => {
                        requests.write(LabRequest::Do(look_at(sim, target)));
                    }
                    None => warn!("{VIEW_ENV}: the playground has no piece {name:?}"),
                }
            }
            View::Tab(..) | View::Play | View::Walk => {}
        }
    }
    // Forward is held exactly for the frames of a `walk` step.
    let walk = matches!(step, Some((View::Walk, _)));
    if walk != tour.walking {
        tour.walking = walk;
        if walk {
            keys.press(KeyCode::KeyW);
        } else {
            keys.release(KeyCode::KeyW);
        }
    }
    // The grapple button is held for a `pull` step, from its second frame
    // on (the first one turns the view, and lets go of an earlier pull). It
    // is never "just pressed": `cursor_and_pause` would take that for the
    // click that captures the mouse.
    let pull = matches!(step, Some((View::Pull(_), false)));
    if (pull || tour.pulling)
        && let Some(mouse) = mouse.as_mut()
    {
        if pull {
            mouse.press(MouseButton::Left);
            mouse.clear_just_pressed(MouseButton::Left);
            if let Some(pending) = pending.as_mut()
                && !pending.grapple_armed
            {
                pending.grapple_armed = true;
            }
        } else {
            mouse.release(MouseButton::Left);
        }
        tour.pulling = pull;
    }

    let screen = state.as_ref().map_or(Screen::None, |s| s.screen);
    let grabbed = cursor
        .iter()
        .next()
        .is_some_and(|c| c.grab_mode != CursorGrabMode::None);
    let mode = match screen {
        Screen::None if grabbed => Mode::Play,
        Screen::None => Mode::Free,
        Screen::Sandbox if panel.open => Mode::Inspector,
        _ => Mode::Other,
    };
    let pinned = panel.pinned.get(panel.pinned_index).cloned();
    let slot = lab
        .session
        .as_ref()
        .map_or(0, |session| session.slots().selected());
    let acts = key_acts(&keys, mode, pinned.as_deref(), slot);
    // A key answers with the runtime's notice: what the view itself last
    // said (a profile that did not load) is out of date then.
    if !acts.is_empty() && view.message.is_some() {
        view.message = None;
    }
    for act in acts {
        match act {
            KeyAct::Do(command) => {
                requests.write(LabRequest::Do(command));
            }
            KeyAct::OpenInspector => {
                requests.write(LabRequest::OpenInspector);
                // The tools were asked for: the read-outs and the drawings
                // come with the inspector (H puts the clean screen back).
                if !view.hud {
                    view.hud = true;
                }
            }
            KeyAct::CloseInspector => {
                requests.write(LabRequest::CloseInspector);
            }
            KeyAct::Pinned(steps) => {
                let next = wrapped(panel.pinned_index, steps, panel.pinned.len());
                if panel.pinned_index != next {
                    panel.pinned_index = next;
                }
            }
            KeyAct::Hud => view.hud = !view.hud,
        }
    }

    // Classic's handler does not see its debug keys in a session ...
    for key in CONSUMED {
        if keys.just_pressed(key) {
            keys.clear_just_pressed(key);
        }
    }
    // ... nor the Esc that closed the inspector (it would pause the game in
    // the same frame, once the inspector's screen is gone).
    if mode == Mode::Inspector && keys.just_pressed(KeyCode::Escape) {
        keys.clear_just_pressed(KeyCode::Escape);
    }
}

pub(super) fn build(app: &mut App) {
    app.add_systems(
        RunFixedMainLoop,
        hotkeys.in_set(LabSet::Input).run_if(lab_active),
    );
}

#[cfg(test)]
mod tests {
    use asamu_sandbox::session::Session;

    use super::super::Phase;
    use super::*;

    fn pressed(codes: &[KeyCode]) -> ButtonInput<KeyCode> {
        let mut keys = ButtonInput::default();
        for code in codes {
            keys.press(*code);
        }
        keys
    }

    /// The commands `codes` produce in play (pinned: `gun.max_distance`,
    /// selected slot 2).
    fn commands(codes: &[KeyCode]) -> Vec<Command> {
        key_acts(&pressed(codes), Mode::Play, Some("gun.max_distance"), 2)
            .into_iter()
            .filter_map(|act| match act {
                KeyAct::Do(command) => Some(command),
                _ => None,
            })
            .collect()
    }

    fn one(codes: &[KeyCode]) -> Command {
        let mut all = commands(codes);
        assert_eq!(all.len(), 1, "{codes:?}: {all:?}");
        all.remove(0)
    }

    #[test]
    fn every_hotkey_produces_its_request() {
        let key = || "gun.max_distance".to_owned();
        let nudge = |steps, scale| Command::NudgeParam {
            key: key(),
            steps,
            scale,
        };
        let time = |op| Command::Time { op };
        let slot = |op| Command::Slot { op };
        let teleport = |to| Command::Teleport { to };
        let quick = || QUICK_MARK.to_owned();
        let table: Vec<(&[KeyCode], Command)> = vec![
            (&[KeyCode::Minus], nudge(-1, 1.0)),
            (&[KeyCode::Equal], nudge(1, 1.0)),
            (&[KeyCode::AltLeft, KeyCode::Equal], nudge(1, 10.0)),
            (&[KeyCode::ControlLeft, KeyCode::Minus], nudge(-1, 0.1)),
            (&[KeyCode::Backspace], Command::ResetParam { key: key() }),
            (
                &[KeyCode::AltLeft, KeyCode::Backspace],
                Command::ResetAllParams,
            ),
            (&[KeyCode::KeyP], time(TimeOp::Freeze(Toggle::Toggle))),
            (&[KeyCode::KeyO], time(TimeOp::Step { n: 1 })),
            (
                &[KeyCode::AltLeft, KeyCode::KeyO],
                time(TimeOp::Step { n: 10 }),
            ),
            (&[KeyCode::Comma], time(TimeOp::Slower)),
            (&[KeyCode::Period], time(TimeOp::Faster)),
            (&[KeyCode::Slash], time(TimeOp::Scale { value: 1.0 })),
            (&[KeyCode::Digit1], slot(SlotOp::Select { slot: 0 })),
            (&[KeyCode::Digit2], slot(SlotOp::Select { slot: 1 })),
            (&[KeyCode::Digit3], slot(SlotOp::Select { slot: 2 })),
            (&[KeyCode::Digit4], slot(SlotOp::Select { slot: 3 })),
            // K and L act on the selected slot (2 in this table).
            (&[KeyCode::KeyK], slot(SlotOp::Save { slot: 2 })),
            (&[KeyCode::KeyL], slot(SlotOp::Load { slot: 2 })),
            (&[KeyCode::KeyZ], Command::Rewind),
            (&[KeyCode::KeyT], teleport(TeleportTarget::AimPoint)),
            (&[KeyCode::KeyG], teleport(TeleportTarget::NextTarget)),
            (&[KeyCode::KeyB], Command::SetMark { name: quick() }),
            (
                &[KeyCode::KeyV],
                teleport(TeleportTarget::Mark { name: quick() }),
            ),
            (&[KeyCode::KeyN], Command::Fly { on: Toggle::Toggle }),
            (&[KeyCode::F9], Command::Record { on: Toggle::Toggle }),
            (&[KeyCode::F2], Command::StoryMode { on: Toggle::Toggle }),
            (&[KeyCode::F3], Command::CycleGrapples),
            (&[KeyCode::F4], Command::RocketBoots { on: Toggle::Toggle }),
            (&[KeyCode::F6], Command::ActivateAttractors),
            (&[KeyCode::F7], Command::Kill),
            (&[KeyCode::KeyR], Command::Kill),
        ];
        for (codes, expected) in table {
            assert_eq!(one(codes), expected, "{codes:?}");
        }
        // The view's own keys and the inspector key are not commands.
        let acts = |codes: &[KeyCode]| key_acts(&pressed(codes), Mode::Play, None, 0);
        assert_eq!(acts(&[KeyCode::Backquote]), [KeyAct::OpenInspector]);
        assert_eq!(acts(&[KeyCode::F5]), [KeyAct::OpenInspector]);
        assert_eq!(acts(&[KeyCode::BracketLeft]), [KeyAct::Pinned(-1)]);
        assert_eq!(acts(&[KeyCode::BracketRight]), [KeyAct::Pinned(1)]);
        assert_eq!(acts(&[KeyCode::KeyH]), [KeyAct::Hud]);
        // Nothing pinned: the tuner keys do nothing (reset-all still works).
        assert!(acts(&[KeyCode::Minus, KeyCode::Equal, KeyCode::Backspace]).is_empty());
        assert_eq!(
            acts(&[KeyCode::AltLeft, KeyCode::Backspace]),
            [KeyAct::Do(Command::ResetAllParams)]
        );
    }

    #[test]
    fn the_keys_the_game_and_the_app_use_stay_theirs() {
        // Movement, look and use, the pause key, tab navigation, and the
        // app's view keys: F1 read-out, F8 time-trial restart, F10 level
        // gizmos, F12 frame recording.
        let theirs = [
            KeyCode::KeyW,
            KeyCode::KeyA,
            KeyCode::KeyS,
            KeyCode::KeyD,
            KeyCode::ArrowUp,
            KeyCode::ArrowDown,
            KeyCode::ArrowLeft,
            KeyCode::ArrowRight,
            KeyCode::Space,
            KeyCode::ShiftLeft,
            KeyCode::ControlLeft,
            KeyCode::KeyE,
            KeyCode::KeyQ,
            KeyCode::Enter,
            KeyCode::Escape,
            KeyCode::Tab,
            KeyCode::F1,
            KeyCode::F8,
            KeyCode::F10,
            KeyCode::F12,
        ];
        for code in theirs {
            let acts = key_acts(&pressed(&[code]), Mode::Play, Some("gun.max_distance"), 0);
            assert!(acts.is_empty(), "{code:?}: {acts:?}");
            assert!(!CONSUMED.contains(&code), "{code:?}");
        }
    }

    #[test]
    fn outside_play_only_the_inspector_keys_act() {
        let everything = [
            KeyCode::Minus,
            KeyCode::KeyP,
            KeyCode::KeyT,
            KeyCode::KeyK,
            KeyCode::F9,
            KeyCode::F3,
            KeyCode::KeyR,
            KeyCode::KeyH,
            KeyCode::BracketLeft,
        ];
        for mode in [Mode::Free, Mode::Inspector, Mode::Other] {
            let acts = key_acts(&pressed(&everything), mode, Some("gun.max_distance"), 0);
            assert!(acts.is_empty(), "{mode:?}: {acts:?}");
        }
        let acts = |codes: &[KeyCode], mode| key_acts(&pressed(codes), mode, None, 0);
        assert_eq!(
            acts(&[KeyCode::Backquote], Mode::Free),
            [KeyAct::OpenInspector]
        );
        for code in [KeyCode::Backquote, KeyCode::F5, KeyCode::Escape] {
            assert_eq!(
                acts(&[code], Mode::Inspector),
                [KeyAct::CloseInspector],
                "{code:?}"
            );
        }
        // Esc with the mouse free is the game's own (resume, pause menu).
        assert!(acts(&[KeyCode::Escape], Mode::Free).is_empty());
        assert!(acts(&[KeyCode::Backquote, KeyCode::Escape], Mode::Other).is_empty());
    }

    #[test]
    fn the_quick_tuner_selection_wraps() {
        assert_eq!(wrapped(0, -1, 7), 6);
        assert_eq!(wrapped(6, 1, 7), 0);
        assert_eq!(wrapped(3, 1, 7), 4);
        assert_eq!(wrapped(0, 1, 0), 0);
        assert_eq!(wrapped(9, 1, 3), 1);
    }

    #[test]
    fn the_unattended_steps_parse() {
        let tab = |tab| (View::Tab(tab, None), None);
        assert_eq!(parse_views("tune"), Some(vec![tab(PanelTab::Tune)]));
        assert_eq!(
            parse_views(" tune:3 , rules,time, view ,profiles,stage,play,show,walk,respawn"),
            Some(vec![
                (View::Tab(PanelTab::Tune, Some(3)), None),
                tab(PanelTab::Rules),
                tab(PanelTab::Time),
                tab(PanelTab::View),
                tab(PanelTab::Profiles),
                tab(PanelTab::Stage),
                (View::Play, None),
                (View::Show, None),
                (View::Walk, None),
                (View::Respawn, None),
            ])
        );
        // The playground's steps, and a step with its own length.
        assert_eq!(
            parse_views("at:wheel@4, pull: carousel hook 1 @2.5,play@0.5,stage@3"),
            Some(vec![
                (View::At("wheel".to_owned()), Some(4.0)),
                (View::Pull("carousel hook 1".to_owned()), Some(2.5)),
                (View::Play, Some(0.5)),
                (View::Tab(PanelTab::Stage, None), Some(3.0)),
            ])
        );
        for bad in [
            "",
            "tune,",
            "nothing",
            "rules:1",
            "tune:x",
            "tune:-1",
            "play:2",
            "at:",
            "pull:  ",
            "play@",
            "play@0",
            "play@-1",
            "play@nan",
            "play@inf",
            "play@9999",
        ] {
            assert_eq!(parse_views(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn a_tour_plays_each_step_for_its_frames_and_then_ends() {
        let mut tour = Tour {
            read: true,
            views: vec![(View::Tab(PanelTab::Time, None), None), (View::Walk, None)],
            ..Tour::default()
        };
        let mut firsts = Vec::new();
        let mut walk_frames = 0;
        for frame in 0..3 * VIEW_FRAMES {
            match tour.step(0.0) {
                Some((view, first)) => {
                    if first {
                        firsts.push((frame, view.clone()));
                    }
                    if view == View::Walk {
                        walk_frames += 1;
                    }
                }
                None => assert!(frame >= 2 * VIEW_FRAMES),
            }
        }
        assert_eq!(
            firsts,
            vec![
                (0, View::Tab(PanelTab::Time, None)),
                (VIEW_FRAMES, View::Walk)
            ]
        );
        assert_eq!(walk_frames, VIEW_FRAMES);
        // Nothing asked for: nothing is played.
        let mut none = Tour {
            read: true,
            ..Tour::default()
        };
        assert!((0..10).all(|_| none.step(0.0).is_none()));
        // A step with a length lasts that long in real time, whatever the
        // frame rate: half a second is 5 frames of 0.1 s, or 50 of 0.01 s.
        for (delta, frames) in [(0.1f32, 5), (0.01, 50)] {
            let mut timed = Tour {
                read: true,
                views: vec![(View::Play, Some(0.5)), (View::Show, None)],
                ..Tour::default()
            };
            let mut played = 0;
            while matches!(timed.step(delta), Some((View::Play, _))) {
                played += 1;
                assert!(played < 1000);
            }
            assert!((played as i32 - frames).abs() <= 1, "{delta}: {played}");
        }
    }

    /// An app with the hotkey layer under its real run condition and a
    /// captured mouse.
    fn app(phase: Phase) -> App {
        let mut lab = Lab::new(true);
        lab.phase = phase;
        lab.session = (phase == Phase::Active).then(Session::classic);
        lab.dirs = None;
        let mut app = App::new();
        app.insert_resource(lab)
            .init_resource::<ButtonInput<KeyCode>>()
            .init_resource::<PanelState>()
            .init_resource::<ViewState>()
            .init_resource::<VizSettings>()
            .add_message::<LabRequest>()
            .add_systems(Update, hotkeys.run_if(lab_active));
        app.world_mut().spawn((
            PrimaryWindow,
            CursorOptions {
                grab_mode: CursorGrabMode::Locked,
                ..default()
            },
        ));
        app
    }

    fn press(app: &mut App, codes: &[KeyCode]) {
        let mut keys = app.world_mut().resource_mut::<ButtonInput<KeyCode>>();
        for code in codes {
            keys.press(*code);
        }
    }

    fn written(app: &App) -> Vec<LabRequest> {
        app.world()
            .resource::<Messages<LabRequest>>()
            .iter_current_update_messages()
            .cloned()
            .collect()
    }

    #[test]
    fn f9_and_the_debug_keys_are_consumed_in_a_session() {
        // In a session: Classic's handler, which runs after this layer,
        // finds none of its debug keys "just pressed" ...
        let mut session = app(Phase::Active);
        press(&mut session, &CONSUMED);
        press(&mut session, &[KeyCode::F1, KeyCode::F10, KeyCode::Escape]);
        session.update();
        let keys = session.world().resource::<ButtonInput<KeyCode>>();
        for key in CONSUMED {
            assert!(!keys.just_pressed(key), "{key:?} was left for Classic");
            assert!(keys.pressed(key), "{key:?} is still held");
        }
        // ... the view-only keys and the pause key are left alone ...
        for key in [KeyCode::F1, KeyCode::F10, KeyCode::Escape] {
            assert!(keys.just_pressed(key), "{key:?}");
        }
        // ... and each consumed key became a logged command instead (F7 and
        // R both ask for the quick load).
        let commands: Vec<Command> = written(&session)
            .into_iter()
            .filter_map(|request| match request {
                LabRequest::Do(command) => Some(command),
                _ => None,
            })
            .collect();
        assert_eq!(commands.len(), CONSUMED.len() - 1, "{commands:?}");
        for expected in [
            Command::Record { on: Toggle::Toggle },
            Command::StoryMode { on: Toggle::Toggle },
            Command::CycleGrapples,
            Command::RocketBoots { on: Toggle::Toggle },
            Command::ActivateAttractors,
            Command::Kill,
        ] {
            assert!(commands.contains(&expected), "{expected:?}");
        }

        // Idle (Classic): the layer does not run, the keys are untouched and
        // nothing is asked of the Sandbox.
        let mut classic = app(Phase::Idle);
        press(&mut classic, &CONSUMED);
        classic.update();
        let keys = classic.world().resource::<ButtonInput<KeyCode>>();
        for key in CONSUMED {
            assert!(keys.just_pressed(key), "{key:?} was taken from Classic");
        }
        assert!(written(&classic).is_empty());
        // The same in the launcher: there is no session yet.
        let mut launcher = app(Phase::Launcher);
        press(&mut launcher, &CONSUMED);
        launcher.update();
        let keys = launcher.world().resource::<ButtonInput<KeyCode>>();
        assert!(CONSUMED.iter().all(|key| keys.just_pressed(*key)));
    }

    #[test]
    fn the_debug_keys_are_consumed_even_when_they_do_nothing() {
        // The inspector is open: the keys are not commands, and they still
        // do not reach Classic's handler when the screen closes this frame.
        let mut app = app(Phase::Active);
        let mut state = UiState::default();
        state.screen = Screen::Sandbox;
        app.insert_resource(state);
        app.world_mut().resource_mut::<PanelState>().open = true;
        press(&mut app, &CONSUMED);
        press(&mut app, &[KeyCode::Escape]);
        app.update();
        let keys = app.world().resource::<ButtonInput<KeyCode>>();
        for key in CONSUMED {
            assert!(!keys.just_pressed(key), "{key:?}");
        }
        // Esc closed the inspector and is not left to pause the game.
        assert!(!keys.just_pressed(KeyCode::Escape));
        let requests = written(&app);
        assert!(
            matches!(requests.as_slice(), [LabRequest::CloseInspector]),
            "{requests:?}"
        );
    }

    #[test]
    fn the_inspector_key_opens_and_closes_through_the_runtime() {
        use asamu_game::GameState;

        use super::super::control::testing::{Rig, frames, lab, rig, sim};

        // A `--sandbox --no-menu` process: a session on the graybox, with
        // the runtime (`lifecycle`, `control`) and this hotkey layer.
        let mut app = rig(Rig {
            from_cli: true,
            no_menu: true,
            ..Rig::default()
        });
        app.init_resource::<ViewState>()
            .init_resource::<VizSettings>();
        super::build(&mut app);
        frames(&mut app, 4);
        assert_eq!(lab(&app).phase, Phase::Active);
        assert!(sim(&app).game.clock().tick() > 0, "the session plays");

        // The rig has no input plugin: a key is "just pressed" for one frame
        // by hand.
        let tap = |app: &mut App, code: KeyCode| {
            app.world_mut()
                .resource_mut::<ButtonInput<KeyCode>>()
                .press(code);
            frames(app, 1);
            let just_pressed = app
                .world()
                .resource::<ButtonInput<KeyCode>>()
                .just_pressed(code);
            let mut keys = app.world_mut().resource_mut::<ButtonInput<KeyCode>>();
            keys.release(code);
            keys.clear();
            just_pressed
        };
        let screen = |app: &App| app.world().resource::<UiState>().screen;
        let open = |app: &App| app.world().resource::<PanelState>().open;

        // The inspector key: the runtime opens the panel on its own screen.
        tap(&mut app, KeyCode::Backquote);
        assert!(open(&app));
        assert_eq!(screen(&app), Screen::Sandbox);
        // It freezes while open (the default): the player does not move.
        frames(&mut app, 2);
        let (held_tick, held_at) = {
            let game = &sim(&app).game;
            (game.clock().tick(), game.player().position)
        };
        // Keys that are commands in play do nothing here.
        tap(&mut app, KeyCode::KeyT);
        tap(&mut app, KeyCode::KeyP);
        frames(&mut app, 5);
        let game = &sim(&app).game;
        assert_eq!(game.clock().tick(), held_tick);
        assert_eq!(game.player().position, held_at);
        let session = lab(&app).session.as_ref().unwrap();
        assert!(session.log().is_empty(), "{:?}", session.log().actions());
        assert!(!session.time().frozen(), "the hold is not the session's");

        // Esc closes it, and is not left for the game to take as "pause".
        let left = tap(&mut app, KeyCode::Escape);
        assert!(!left, "Esc was left just pressed");
        assert!(!open(&app));
        assert_eq!(screen(&app), Screen::None);
        frames(&mut app, 5);
        let game = &sim(&app).game;
        assert_eq!(game.state(), GameState::Playing);
        assert!(game.clock().tick() > held_tick, "time runs again");
    }

    #[test]
    fn the_view_keys_change_only_view_state() {
        let mut app = app(Phase::Active);
        let pinned = app.world().resource::<PanelState>().pinned.len();
        press(&mut app, &[KeyCode::BracketLeft, KeyCode::KeyH]);
        app.update();
        let panel = app.world().resource::<PanelState>();
        assert_eq!(panel.pinned_index, pinned - 1);
        assert!(!panel.open, "only the runtime opens the inspector");
        // H switches the read-outs and drawings on (a session starts with
        // the clean screen) ...
        assert!(app.world().resource::<ViewState>().hud);
        assert!(written(&app).is_empty());
        // ... and off again.
        app.world_mut()
            .resource_mut::<ButtonInput<KeyCode>>()
            .clear();
        app.world_mut()
            .resource_mut::<ButtonInput<KeyCode>>()
            .release(KeyCode::KeyH);
        press(&mut app, &[KeyCode::KeyH]);
        app.update();
        assert!(!app.world().resource::<ViewState>().hud);
    }
}
