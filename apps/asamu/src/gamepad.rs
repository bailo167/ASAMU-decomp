//! Gamepad input: the original's controller bindings mapped onto the same
//! abstract actions the keyboard and mouse produce ([`InputFrame`]).
//!
//! # Evidence (original game)
//!
//! Bindings come from the shipped `ASAMU/Config/DefaultInput.ini`
//! `[Engine.PlayerInput]` (CONFIRMED, config; the same list is the
//! `Engine.PlayerInput` class default object, CONFIRMED, cdo):
//!
//! | Control | Command | Abstract action |
//! |---|---|---|
//! | `XboxTypeS_LeftX` / `_LeftY` | `GBA_StrafeLeft_Gamepad` (`Axis aStrafe Speed=1.0 DeadZone=0.4`) / `GBA_MoveForward_Gamepad` (`Axis aBaseY Speed=1.0 DeadZone=0.4`) | strafe / forward axes (analog) |
//! | `XboxTypeS_RightX` / `_RightY` | `GBA_TurnLeft_Gamepad` (`Axis aTurn Speed=1.0 DeadZone=0.2`) / `GBA_Look_Gamepad` (`Axis aLookup Speed=0.65 DeadZone=0.2`) | turn / look rates |
//! | `XboxTypeS_A` | `GBA_ReleaseableJump \| RocketBoostKeyDown` | jump (press, hold, release) — exactly the space bar's command |
//! | `XboxTypeS_RightTrigger` | `GBA_Fire` | fire (grapple) |
//! | `XboxTypeS_RightShoulder` | `GBA_PowerJump` | power jump |
//! | `XboxTypeS_LeftShoulder` | `GBA_Sprint` | sprint |
//! | `XboxTypeS_Back` | `GBA_QuickLoad` | quick restart from the checkpoint |
//! | `XboxTypeS_Start` | `\| onrelease ShowMenuXbox` | pause menu (on release) |
//! | `LeftTrigger`, `B`, `X`, `Y`, thumbstick clicks | (empty) | nothing |
//!
//! - The menu's controller layouts (`ASAMUSettingsManager.XboxStickLayout` /
//!   `XboxButtonLayout`, both 0 in `DefaultSettings.ini`) rebind four
//!   controls each from the class default arrays `XboxStickLayoutArray` /
//!   `XboxButtonLayoutArray` (CONFIRMED, cdo): stick layout 1 swaps the
//!   sticks (left X turns, left Y looks with `GBA_Look_Gamepad_Reverse`,
//!   right X strafes, right Y moves with `GBA_MoveForward_Gamepad_Reverse`);
//!   button layout 1 puts sprint on the right shoulder, power jump on the
//!   left shoulder, fire on the left trigger and nothing on the right
//!   trigger. [`GamepadLayout`] holds both (defaults 0).
//! - `GBA_Jump_Gamepad` (`SmartJump`, a `UTGame.UTConsolePlayerInput` exec)
//!   is defined but bound to no control in any layout: the A button sends
//!   the keyboard's releasable jump (CONFIRMED, config + cdo).
//! - There is no gamepad binding for `use` or for the time-trial restart
//!   (`GBA_TimeTrialRestart` is F8 only) (CONFIRMED, config).
//!
//! Axis arithmetic (CONFIRMED, native: `UInput::Exec` "AXIS" of the Mac
//! executable, read locally): an `Axis` command adds `Invert · Speed ·
//! value` to its axis, where a dead zone `0 < d < 1` first maps `|value|`
//! to `max(|value| − d, 0) / (1 − d)` keeping the sign. Held keys feed
//! their axis the value 1.0 every frame (`UInput::Tick`), so a full stick
//! deflection and a held W key produce the same `aBaseY` of 1.0: the
//! simulation's move axes take the stick value directly. The stick's
//! magnitude does not make the player slower: the stock walking move
//! normalizes the input direction and accelerates at the full `AccelRate`
//! (CONFIRMED, src: `PlayerController` `PlayerWalking.PlayerMove`), and the
//! simulation's original movement model likewise uses the direction only.
//! A deflection inside the dead zone is no input at all.
//!
//! Look rates (CONFIRMED, src: `ASAMUPlayerInput.PlayerInput`, read
//! locally; constants from config/cdo): with a gamepad the turn and look
//! axes are multiplied by `ControllerSensitivity` (1.0:
//! `[ASAMU.ASAMUControllerInput]` in `DefaultController.ini` and the
//! `ASAMUPlayerInput` class default), then by `100 · Δt` and by
//! `LookRightScale` (300) / `LookUpScale` (−250); the result is the
//! rotator change of the frame (65,536 units per turn).
//! `bEnableFOVScaling` is off (absent from the `PlayerInput` class
//! default, CONFIRMED cdo), so the FOV does not scale them. A full right
//! stick therefore turns 30,000 units/s (≈ 164.8°/s) and looks
//! 0.65 · 25,000 = 16,250 units/s (≈ 89.3°/s).
//!
//! Stick signs (CONFIRMED, native: the Mac build reads controllers through
//! SDL2's game-controller API (`HIDControllerInfo`) and maps each raw axis
//! linearly onto an output range): left X, right X and right Y map
//! `[-32768, 32767]` to `[-1, 1]` (SDL: right and **down** positive), left
//! Y maps it to `[1, -1]` (up positive). So pushing the right stick down
//! makes `aLookUp` negative after `LookUpScale` and looks down; pushing the
//! left stick up moves forward. Bevy reports up as positive on both
//! sticks, so the right Y axis is negated here. The trigger buttons count
//! as pressed above 10 % of their travel (native constant). Re-read in the
//! verification pass: `FMacViewport::ProcessInput` polls
//! `HIDControllerInfo::RefreshStates` (SDL game-controller axes and
//! buttons of up to four controllers), maps each axis linearly from its
//! raw range onto its output range, and presses the trigger key while the
//! raw value is strictly above minimum + 0.1 × range. The signs are also
//! what the shipped bindings need: the swapped stick layout uses the
//! `_Reverse` commands exactly where the two Y axes change roles.
//!
//! Rumble (`ASAMURumbleManager`, CONFIRMED src + cdo waveforms): hard
//! landing (35/35, linearly decreasing, 1.2 s), grapple attached (15/15,
//! sine 0→90°, 0.2 s, looping until released), power jump charging and
//! rocket boots charging (one shared waveform: 20/20, linearly increasing,
//! 0.9 s), power jump released (45/45, constant, 0.48 s) and cancelled
//! (25/25, linearly decreasing, 0.55 s), rocket boots boosting (45 left
//! sine 0→180°, 50 right linearly decreasing, 2.0 s, stopped by a landing
//! during the boost), gated by the `RumbleActive` setting (true by
//! default).
//!
//! How waveforms combine (CONFIRMED, src: the stock `ForceFeedbackManager`
//! and `PlayerController.ClientPlayForceFeedbackWaveform`): one waveform
//! plays at a time, a new one replaces the current one, a stop names its
//! waveform and does nothing unless that one is current, the waveform is
//! paused with the game, and nothing new plays unless the player's last
//! input came from a gamepad (`bUsingGamepad`). [`RumbleMixer`] keeps those
//! rules. All nine rumble calls of the shipped script are covered (the
//! controller's grapple states, the power jump's charge, release and two
//! cancels, the pawn's hard landing, the boots' charge, boost and landing);
//! the options menu stops everything when rumble is switched off, as the
//! mixer does. Ours: Bevy rumbles at a constant intensity, so each waveform
//! becomes its amplitude / 100 times the mean of its shape over the
//! duration; "last input came from a gamepad" is decided per frame from
//! the devices ([`next_in_use`]: gamepad input sets it, keyboard or mouse
//! input clears it — STRONG, the stock script describes the flag as
//! following the device of each input; the native code that sets it was
//! not examined); the power jump's charge rumble starts when the
//! simulation's power jump is seen charging (the `Charging` state's code
//! starts it in the original; the tick events carry no charge-start
//! event).
//!
//! # Integration
//!
//! Look input and the jump press go into the app's per-frame
//! [`PendingInput`] (like the mouse), so the camera turns between ticks.
//! The analog move axes and the held buttons are published in
//! [`GamepadActions`], which `main.rs`'s `fixed_tick` merges into each
//! tick's [`InputFrame`] with [`GamepadActions::merge_into`] (axes add and
//! clamp like the original's axis sums; buttons combine with OR). Start pauses and
//! resumes; Back is the checkpoint restart (the original's `QuickLoad`:
//! disabled in Workshop and Epilogue and while Kismet has switched the
//! HUD's restart-from-checkpoint option off, [`quick_load_allowed`]). Menu
//! navigation with the pad (the original's `[Scaleform.KeyMap]` maps the
//! D-pad and A/B to the menus) belongs to the UI.
//!
//! Every connected controller drives the one player (ours; the original
//! reads the local player's own controller): their sticks add up and are
//! clamped to a full deflection.

use std::time::Duration;

use asamu_game::GameState;
use asamu_game::save::ChapterId;
use asamu_player::InputFrame;
use bevy::ecs::system::SystemParam;
use bevy::input::gamepad::{Gamepad, GamepadButton, GamepadRumbleIntensity, GamepadRumbleRequest};
use bevy::input::mouse::AccumulatedMouseMotion;
use bevy::prelude::*;
use bevy::window::{CursorGrabMode, CursorOptions, PrimaryWindow};

use crate::{PendingInput, Sim};

/// `GBA_StrafeLeft_Gamepad` / `GBA_MoveForward_Gamepad` `DeadZone=0.4`
/// (`DefaultInput.ini`, CONFIRMED config).
pub const MOVE_DEAD_ZONE: f32 = 0.4;
/// `GBA_TurnLeft_Gamepad` / `GBA_Look_Gamepad` `DeadZone=0.2`
/// (`DefaultInput.ini`, CONFIRMED config).
pub const LOOK_DEAD_ZONE: f32 = 0.2;
/// `GBA_TurnLeft_Gamepad` `Speed=1.0` (CONFIRMED config).
pub const TURN_SPEED: f32 = 1.0;
/// `GBA_Look_Gamepad` `Speed=0.65` (`_Reverse`: −0.65) (CONFIRMED config).
pub const LOOK_SPEED: f32 = 0.65;
/// `[Engine.PlayerInput] LookRightScale=300` (CONFIRMED config, cdo).
pub const LOOK_RIGHT_SCALE: f32 = 300.0;
/// `[Engine.PlayerInput] LookUpScale=-250` (CONFIRMED config, cdo).
pub const LOOK_UP_SCALE: f32 = -250.0;
/// `[ASAMU.ASAMUControllerInput] ControllerSensitivity=1.0`
/// (`DefaultController.ini`; also the `ASAMUPlayerInput` class default)
/// (CONFIRMED config, cdo).
pub const CONTROLLER_SENSITIVITY: f32 = 1.0;
/// The `100 · Δt` time scale of `PlayerInput` (CONFIRMED src).
pub const INPUT_TIME_SCALE: f32 = 100.0;
/// Rotator units per turn (UE3).
pub const ROTATOR_UNITS_PER_TURN: f32 = 65_536.0;
/// A trigger counts as pressed above this fraction of its travel (the Mac
/// viewport's trigger-button rule, CONFIRMED native constant 0.1).
pub const TRIGGER_THRESHOLD: f32 = 0.1;
/// Longest frame time one frame of stick look is scaled by (ours: a frame
/// that took longer, e.g. while a level loads, must not swing the view by
/// seconds of turning at once).
pub const MAX_LOOK_FRAME_SECONDS: f32 = 0.1;

/// The controller layouts of the options menu (`XboxStickLayout`,
/// `XboxButtonLayout`; 0 = the default bindings) and the controller
/// sensitivity.
#[derive(Resource, Debug, Clone, Copy, PartialEq)]
pub struct GamepadLayout {
    /// 0: move on the left stick, look on the right; 1: swapped.
    pub stick: u8,
    /// 0: RB power jump, RT fire, LB sprint; 1: RB sprint, LB power jump,
    /// LT fire.
    pub button: u8,
    /// `ControllerSensitivity`.
    pub sensitivity: f32,
    /// `RumbleActive`.
    pub rumble: bool,
}

impl Default for GamepadLayout {
    fn default() -> Self {
        Self {
            stick: 0,
            button: 0,
            sensitivity: CONTROLLER_SENSITIVITY,
            rumble: true,
        }
    }
}

/// One frame of controller state in the original's conventions (see the
/// module docs): sticks in `[-1, 1]` with left Y up-positive and right Y
/// down-positive (SDL / the Mac build), buttons as pressed flags, triggers
/// as travel in `[0, 1]`.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct PadState {
    /// `XboxTypeS_LeftX` (right positive).
    pub left_x: f32,
    /// `XboxTypeS_LeftY` (up positive).
    pub left_y: f32,
    /// `XboxTypeS_RightX` (right positive).
    pub right_x: f32,
    /// `XboxTypeS_RightY` (down positive).
    pub right_y: f32,
    /// `XboxTypeS_A`.
    pub a: bool,
    /// `XboxTypeS_LeftShoulder`.
    pub left_shoulder: bool,
    /// `XboxTypeS_RightShoulder`.
    pub right_shoulder: bool,
    /// `XboxTypeS_LeftTriggerAxis`.
    pub left_trigger: f32,
    /// `XboxTypeS_RightTriggerAxis`.
    pub right_trigger: f32,
}

impl PadState {
    /// Sums two controllers (axes add and are clamped to a full
    /// deflection, buttons combine; ours: every connected controller drives
    /// the one player). Non-finite axis values read as 0.
    #[must_use]
    pub fn combine(self, o: Self) -> Self {
        let add = |a: f32, b: f32| stick(stick(a) + stick(b));
        let travel = |a: f32, b: f32| stick(a).max(stick(b)).max(0.0);
        Self {
            left_x: add(self.left_x, o.left_x),
            left_y: add(self.left_y, o.left_y),
            right_x: add(self.right_x, o.right_x),
            right_y: add(self.right_y, o.right_y),
            a: self.a || o.a,
            left_shoulder: self.left_shoulder || o.left_shoulder,
            right_shoulder: self.right_shoulder || o.right_shoulder,
            left_trigger: travel(self.left_trigger, o.left_trigger),
            right_trigger: travel(self.right_trigger, o.right_trigger),
        }
    }

    /// Any control the bindings read is in use under `layout`: a stick past
    /// its binding's dead zone, a trigger past its threshold, or a held
    /// button.
    #[must_use]
    pub fn active(&self, layout: &GamepadLayout) -> bool {
        let axes = pad_axes(self, layout.stick);
        self.a
            || self.left_shoulder
            || self.right_shoulder
            || self.left_trigger > TRIGGER_THRESHOLD
            || self.right_trigger > TRIGGER_THRESHOLD
            || [axes.strafe, axes.forward, axes.turn, axes.look]
                .iter()
                .any(|v| *v != 0.0)
    }
}

/// A raw stick value as the original's input code sees it: within
/// `[-1, 1]`, non-finite values read as 0.
#[must_use]
pub fn stick(v: f32) -> f32 {
    if v.is_finite() {
        v.clamp(-1.0, 1.0)
    } else {
        0.0
    }
}

/// Whether the player's last input came from a gamepad (the original's
/// `PlayerInput.bUsingGamepad`, which gates rumble). Ours: decided per
/// frame by [`next_in_use`].
#[derive(Resource, Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct GamepadInUse(pub bool);

/// The next value of [`GamepadInUse`]: gamepad input this frame turns it
/// on, otherwise keyboard or mouse input turns it off, otherwise it stays.
#[must_use]
pub fn next_in_use(current: bool, pad_input: bool, other_input: bool) -> bool {
    if pad_input {
        true
    } else if other_input {
        false
    } else {
        current
    }
}

/// The original's dead zone (`UInput::Exec` "AXIS", CONFIRMED native): for
/// `0 < d < 1`, `|v|` becomes `max(|v| − d, 0) / (1 − d)` with the sign
/// kept; any other `d` leaves `v` unchanged. Non-finite input reads as 0.
#[must_use]
pub fn dead_zone(v: f32, d: f32) -> f32 {
    if !v.is_finite() {
        return 0.0;
    }
    if d <= 0.0 || d >= 1.0 || d.is_nan() {
        return v;
    }
    let m = (v.abs() - d).max(0.0) / (1.0 - d);
    if v > 0.0 { m } else { -m }
}

/// The four analog axes before the per-frame time scaling
/// (`aStrafe`, `aBaseY`, `aTurn`, `aLookUp` as the bindings fill them).
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct PadAxes {
    /// `aStrafe` (+ right).
    pub strafe: f32,
    /// `aBaseY` (+ forward).
    pub forward: f32,
    /// `aTurn` (+ right).
    pub turn: f32,
    /// `aLookUp` before `LookUpScale` (+ down: the scale is negative).
    pub look: f32,
}

/// The axes of `pad` under stick layout `stick` (see [`GamepadLayout`]).
#[must_use]
pub fn pad_axes(pad: &PadState, stick: u8) -> PadAxes {
    let mv = |v: f32| dead_zone(v, MOVE_DEAD_ZONE);
    let lk = |v: f32| dead_zone(v, LOOK_DEAD_ZONE);
    if stick == 1 {
        // LeftX: TurnLeft_Gamepad; LeftY: Look_Gamepad_Reverse;
        // RightX: StrafeLeft_Gamepad; RightY: MoveForward_Gamepad_Reverse.
        PadAxes {
            strafe: mv(pad.right_x),
            forward: -mv(pad.right_y),
            turn: TURN_SPEED * lk(pad.left_x),
            look: -LOOK_SPEED * lk(pad.left_y),
        }
    } else {
        PadAxes {
            strafe: mv(pad.left_x),
            forward: mv(pad.left_y),
            turn: TURN_SPEED * lk(pad.right_x),
            look: LOOK_SPEED * lk(pad.right_y),
        }
    }
}

/// The view change of one frame of `dt` seconds, in radians (yaw + right,
/// pitch + up, the [`InputFrame`] conventions): `axis · sensitivity · 100 ·
/// dt · scale` rotator units (see the module docs). `dt` counts up to
/// [`MAX_LOOK_FRAME_SECONDS`].
#[must_use]
pub fn look_radians(axes: &PadAxes, dt: f32, sensitivity: f32) -> (f32, f32) {
    if !dt.is_finite() || dt <= 0.0 || !sensitivity.is_finite() {
        return (0.0, 0.0);
    }
    let to_rad = std::f32::consts::TAU / ROTATOR_UNITS_PER_TURN;
    let k = sensitivity * INPUT_TIME_SCALE * dt.min(MAX_LOOK_FRAME_SECONDS);
    let yaw = axes.turn * k * LOOK_RIGHT_SCALE * to_rad;
    let pitch = axes.look * k * LOOK_UP_SCALE * to_rad;
    (yaw, pitch)
}

/// Held buttons as abstract actions under button layout `button`.
#[must_use]
pub fn pad_buttons(pad: &PadState, button: u8) -> (bool, bool, bool, bool) {
    let rt = pad.right_trigger > TRIGGER_THRESHOLD;
    let lt = pad.left_trigger > TRIGGER_THRESHOLD;
    let (grapple, sprint, power) = if button == 1 {
        (lt, pad.right_shoulder, pad.left_shoulder)
    } else {
        (rt, pad.left_shoulder, pad.right_shoulder)
    };
    (pad.a, grapple, sprint, power)
}

/// The gamepad's per-tick actions, merged by `main.rs` into each
/// [`InputFrame`] (see the module docs).
#[derive(Resource, Debug, Clone, Copy, Default, PartialEq)]
pub struct GamepadActions {
    /// Forward axis (`aBaseY`, dead zone applied).
    pub move_forward: f32,
    /// Strafe axis (`aStrafe`, dead zone applied).
    pub move_right: f32,
    /// A held (jump; `ReleaseJump` on release).
    pub jump_held: bool,
    /// Fire held (grapple).
    pub grapple_held: bool,
    /// Sprint held.
    pub sprint_held: bool,
    /// Power jump held.
    pub power_jump_held: bool,
}

impl GamepadActions {
    /// The actions of `pad` under `layout`.
    #[must_use]
    pub fn from_pad(pad: &PadState, layout: &GamepadLayout) -> Self {
        let axes = pad_axes(pad, layout.stick);
        let (jump_held, grapple_held, sprint_held, power_jump_held) =
            pad_buttons(pad, layout.button);
        Self {
            move_forward: axes.forward,
            move_right: axes.strafe,
            jump_held,
            grapple_held,
            sprint_held,
            power_jump_held,
        }
    }

    /// Adds these actions to a tick's input (axes add and clamp to
    /// `[-1, 1]`, as the original sums every binding into one axis;
    /// buttons combine with OR). Called by `fixed_tick` before the
    /// cinematic-mode filter, so that it blocks the gamepad too.
    pub fn merge_into(&self, input: &mut InputFrame) {
        let add = |a: f32, b: f32| {
            let s = a + if b.is_finite() { b } else { 0.0 };
            if s.is_finite() {
                s.clamp(-1.0, 1.0)
            } else {
                0.0
            }
        };
        input.move_forward = add(input.move_forward, self.move_forward);
        input.move_right = add(input.move_right, self.move_right);
        input.jump_held |= self.jump_held;
        input.grapple_held |= self.grapple_held;
        input.sprint_held |= self.sprint_held;
        input.power_jump_held |= self.power_jump_held;
    }
}

/// Button edges of a frame (any connected controller).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct PadEdges {
    jump_pressed: bool,
    start_released: bool,
    back_pressed: bool,
    /// Start or Back is down (they count as gamepad input too).
    menu_buttons_held: bool,
}

/// Gamepad input per DefaultInput.ini.
pub struct GamepadPlugin;

impl Plugin for GamepadPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<GamepadLayout>()
            .init_resource::<GamepadActions>()
            .init_resource::<GamepadInUse>()
            .add_systems(
                RunFixedMainLoop,
                read_gamepads.in_set(RunFixedMainLoopSystems::BeforeFixedMainLoop),
            )
            .add_systems(Update, rumble);
    }
}

/// Reads one controller in the original's conventions.
fn pad_state(g: &Gamepad) -> PadState {
    let axis = |a: GamepadAxis| stick(g.get_unclamped(a).unwrap_or(0.0));
    let trigger = |b: GamepadButton| {
        g.get_unclamped(b)
            .filter(|v| v.is_finite())
            .map_or(if g.pressed(b) { 1.0 } else { 0.0 }, |v| v.clamp(0.0, 1.0))
    };
    PadState {
        left_x: axis(GamepadAxis::LeftStickX),
        left_y: axis(GamepadAxis::LeftStickY),
        right_x: axis(GamepadAxis::RightStickX),
        // Bevy: up positive; the original: down positive.
        right_y: -axis(GamepadAxis::RightStickY),
        a: g.pressed(GamepadButton::South),
        left_shoulder: g.pressed(GamepadButton::LeftTrigger),
        right_shoulder: g.pressed(GamepadButton::RightTrigger),
        left_trigger: trigger(GamepadButton::LeftTrigger2),
        right_trigger: trigger(GamepadButton::RightTrigger2),
    }
}

/// The keyboard and mouse of a frame, for [`next_in_use`].
#[derive(SystemParam)]
struct OtherDevices<'w> {
    keys: Option<Res<'w, ButtonInput<KeyCode>>>,
    mouse: Option<Res<'w, ButtonInput<MouseButton>>>,
    motion: Option<Res<'w, AccumulatedMouseMotion>>,
}

impl OtherDevices<'_> {
    /// A key or mouse button went down, or the mouse moved, this frame.
    fn active(&self) -> bool {
        self.keys
            .as_ref()
            .is_some_and(|k| k.get_just_pressed().next().is_some())
            || self
                .mouse
                .as_ref()
                .is_some_and(|m| m.get_just_pressed().next().is_some())
            || self.motion.as_ref().is_some_and(|m| m.delta != Vec2::ZERO)
    }
}

/// Every frame before the fixed ticks: the gamepad actions, look and jump
/// presses into [`PendingInput`], Start (pause / resume) and Back (quick
/// restart).
#[allow(clippy::too_many_arguments)]
fn read_gamepads(
    gamepads: Query<&Gamepad>,
    layout: Res<GamepadLayout>,
    time: Res<Time<Real>>,
    others: OtherDevices,
    mut in_use: ResMut<GamepadInUse>,
    mut actions: ResMut<GamepadActions>,
    mut sim: Option<ResMut<Sim>>,
    mut pending: Option<ResMut<PendingInput>>,
    mut ui: Option<ResMut<crate::ui::UiState>>,
    play: Option<Res<crate::ui::Play>>,
    presentation: Option<Res<crate::kismet::Presentation>>,
    mut cursor: Query<&mut CursorOptions, With<PrimaryWindow>>,
) {
    let mut pad = PadState::default();
    let mut edges = PadEdges::default();
    for g in &gamepads {
        pad = pad.combine(pad_state(g));
        edges.jump_pressed |= g.just_pressed(GamepadButton::South);
        edges.start_released |= g.just_released(GamepadButton::Start);
        edges.back_pressed |= g.just_pressed(GamepadButton::Select);
        edges.menu_buttons_held |=
            g.pressed(GamepadButton::Start) || g.pressed(GamepadButton::Select);
    }
    let pad_input = pad.active(&layout) || edges.menu_buttons_held || edges.start_released;
    let next = next_in_use(in_use.0, pad_input, others.active());
    if in_use.0 != next {
        in_use.0 = next;
    }
    let screen = ui.as_ref().map(|s| s.screen);
    let menu_open = screen.is_some_and(|s| s != crate::ui::Screen::None);
    let Some(sim) = sim.as_mut() else {
        *actions = GamepadActions::default();
        return;
    };
    // Start in the pause menu resumes (the menu's own Resume does the
    // same: play on, capture the mouse, drop stale input).
    if edges.start_released && screen == Some(crate::ui::Screen::Pause) {
        if sim.game.state() == GameState::Paused {
            sim.game.resume();
        }
        if let Some(ui) = ui.as_mut() {
            ui.close();
        }
        set_grab(&mut cursor, true);
        if let Some(p) = pending.as_mut() {
            **p = PendingInput::default();
        }
        *actions = GamepadActions::default();
        return;
    }
    if menu_open || sim.game.state() != GameState::Playing {
        *actions = GamepadActions::default();
        return;
    }
    // Start while playing: the pause menu (`ShowMenuXbox`), unless Kismet
    // disabled it (`SeqAct_DisablePauseMenu`).
    if edges.start_released && presentation.as_ref().is_none_or(|p| p.pause_menu) {
        sim.game.pause();
        set_grab(&mut cursor, false);
        if let Some(p) = pending.as_mut() {
            **p = PendingInput::default();
        }
        *actions = GamepadActions::default();
        return;
    }
    // Back: `QuickLoad`.
    if edges.back_pressed
        && quick_load_allowed(
            play.as_ref().and_then(|p| p.chapter),
            presentation.as_ref().and_then(|p| p.restart_option),
        )
    {
        if sim.game.scene_map().is_some() {
            sim.game.kill_player();
        } else {
            sim.game.respawn();
            sim.snap_interpolation();
        }
    }
    *actions = GamepadActions::from_pad(&pad, &layout);
    if let Some(p) = pending.as_mut() {
        let (yaw, pitch) = look_radians(
            &pad_axes(&pad, layout.stick),
            time.delta_secs(),
            layout.sensitivity,
        );
        p.look_yaw += yaw;
        p.look_pitch += pitch;
        if edges.jump_pressed {
            p.jump = true;
        }
    }
}

/// `ASAMUPawn.QuickLoad`'s rule (CONFIRMED src): not in Workshop (level 1)
/// or Epilogue (level 7), and not while the HUD's
/// `bRestartFromCheckpointEnabled` is off. `restart_option` is what Kismet
/// last set with `SeqAct_ToggleRestartFromCheckpointOption` (`None`: never
/// set; the HUD's class default is on, CONFIRMED cdo).
#[must_use]
pub fn quick_load_allowed(chapter: Option<ChapterId>, restart_option: Option<bool>) -> bool {
    restart_option != Some(false)
        && !matches!(chapter, Some(ChapterId::Workshop | ChapterId::Epilogue))
}

fn set_grab(cursor: &mut Query<&mut CursorOptions, With<PrimaryWindow>>, grab: bool) {
    for mut c in cursor.iter_mut() {
        if grab {
            c.grab_mode = CursorGrabMode::Locked;
            c.visible = false;
        } else {
            c.grab_mode = CursorGrabMode::None;
            c.visible = true;
        }
    }
}

/// UE3 `EWaveformFunction` shapes used by the rumble waveforms.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WaveShape {
    /// `WF_Constant`.
    Constant,
    /// `WF_LinearIncreasing`.
    LinearIncreasing,
    /// `WF_LinearDecreasing`.
    LinearDecreasing,
    /// `WF_Sin0to90`.
    Sin0To90,
    /// `WF_Sin0to180`.
    Sin0To180,
}

impl WaveShape {
    /// Mean of the shape over its duration (1 = constant at the amplitude).
    #[must_use]
    pub fn mean(self) -> f32 {
        match self {
            Self::Constant => 1.0,
            Self::LinearIncreasing | Self::LinearDecreasing => 0.5,
            // ∫₀^{π/2} sin / (π/2) = 2/π; ∫₀^π sin / π = 2/π.
            Self::Sin0To90 | Self::Sin0To180 => std::f32::consts::FRAC_2_PI,
        }
    }
}

/// One `ForceFeedbackWaveform` of `ASAMURumbleManager` (single sample;
/// amplitudes 0..100) (CONFIRMED cdo).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Waveform {
    /// `LeftAmplitude`.
    pub left: f32,
    /// `RightAmplitude`.
    pub right: f32,
    /// `LeftFunction`.
    pub left_shape: WaveShape,
    /// `RightFunction`.
    pub right_shape: WaveShape,
    /// `Duration`, s.
    pub duration: f32,
}

impl Waveform {
    /// Constant-intensity approximation (strong motor = left).
    #[must_use]
    pub fn intensity(&self) -> GamepadRumbleIntensity {
        GamepadRumbleIntensity {
            strong_motor: (self.left / 100.0 * self.left_shape.mean()).clamp(0.0, 1.0),
            weak_motor: (self.right / 100.0 * self.right_shape.mean()).clamp(0.0, 1.0),
        }
    }
}

/// `Feedback_HardLanding`.
pub const RUMBLE_HARD_LANDING: Waveform = Waveform {
    left: 35.0,
    right: 35.0,
    left_shape: WaveShape::LinearDecreasing,
    right_shape: WaveShape::LinearDecreasing,
    duration: 1.2,
};
/// `Feedback_Grapple` (`bIsLooping`).
pub const RUMBLE_GRAPPLE: Waveform = Waveform {
    left: 15.0,
    right: 15.0,
    left_shape: WaveShape::Sin0To90,
    right_shape: WaveShape::Sin0To90,
    duration: 0.2,
};
/// `Feedback_PowerJumpRelease`.
pub const RUMBLE_POWER_JUMP_RELEASE: Waveform = Waveform {
    left: 45.0,
    right: 45.0,
    left_shape: WaveShape::Constant,
    right_shape: WaveShape::Constant,
    duration: 0.48,
};
/// `Feedback_PowerJumpCancel`.
pub const RUMBLE_POWER_JUMP_CANCEL: Waveform = Waveform {
    left: 25.0,
    right: 25.0,
    left_shape: WaveShape::LinearDecreasing,
    right_shape: WaveShape::LinearDecreasing,
    duration: 0.55,
};
/// `Feedback_PowerJumpCharge` (also the rocket boots' charge).
pub const RUMBLE_POWER_JUMP_CHARGE: Waveform = Waveform {
    left: 20.0,
    right: 20.0,
    left_shape: WaveShape::LinearIncreasing,
    right_shape: WaveShape::LinearIncreasing,
    duration: 0.9,
};
/// `Feedback_RocketBootsActive`.
pub const RUMBLE_ROCKET_BOOTS_ACTIVE: Waveform = Waveform {
    left: 45.0,
    right: 50.0,
    left_shape: WaveShape::Sin0To180,
    right_shape: WaveShape::LinearDecreasing,
    duration: 2.0,
};
/// How long the looping grapple rumble is requested from the motors at a
/// time (ours: it is renewed while the waveform is current and stopped at
/// the release).
pub const GRAPPLE_RUMBLE_SECONDS: f32 = 30.0;

/// The waveform objects of `ASAMURumbleManager` the game plays. Stops name
/// a waveform, so the rocket boots' charge (which plays the power jump's
/// charge waveform) is the same kind as the power jump's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RumbleKind {
    /// `Feedback_PowerJumpCharge` (`RUMBLE_POWERJUMPCHARGE`,
    /// `RUMBLE_ROCKETBOOTSCHARGE`).
    PowerJumpCharge,
    /// `Feedback_PowerJumpRelease`.
    PowerJumpRelease,
    /// `Feedback_PowerJumpCancel`.
    PowerJumpCancel,
    /// `Feedback_Grapple` (looping).
    Grapple,
    /// `Feedback_RocketBootsActive`.
    RocketBootsActive,
    /// `Feedback_HardLanding`.
    HardLanding,
}

impl RumbleKind {
    /// The waveform's class-default values.
    #[must_use]
    pub const fn waveform(self) -> Waveform {
        match self {
            Self::PowerJumpCharge => RUMBLE_POWER_JUMP_CHARGE,
            Self::PowerJumpRelease => RUMBLE_POWER_JUMP_RELEASE,
            Self::PowerJumpCancel => RUMBLE_POWER_JUMP_CANCEL,
            Self::Grapple => RUMBLE_GRAPPLE,
            Self::RocketBootsActive => RUMBLE_ROCKET_BOOTS_ACTIVE,
            Self::HardLanding => RUMBLE_HARD_LANDING,
        }
    }

    /// `bIsLooping` (only the grapple's, CONFIRMED cdo).
    #[must_use]
    pub const fn looping(self) -> bool {
        matches!(self, Self::Grapple)
    }

    /// Seconds one motor request for this waveform lasts.
    #[must_use]
    pub const fn request_seconds(self) -> f32 {
        if self.looping() {
            GRAPPLE_RUMBLE_SECONDS
        } else {
            self.waveform().duration
        }
    }
}

/// What a tick asks of the rumble manager.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RumbleCue {
    /// `RumbleManager.Rumble`: play the waveform (it replaces the current
    /// one).
    Play(RumbleKind),
    /// `RumbleManager.StopRumble`: stop the waveform if it is the current
    /// one.
    Stop(RumbleKind),
}

/// The rumble cues of one tick's player events (see the module docs). The
/// order inside a tick is ours; with stops that name their waveform the
/// outcome does not depend on it for the pairs that can coincide (a
/// landing that ends a boost, a release followed by a new attach).
#[must_use]
pub fn rumble_cues(e: &asamu_player::StepEvents) -> Vec<RumbleCue> {
    use asamu_player::pawn::PowerJumpEvent;
    use asamu_player::rocket_boots::BootsEvent;
    let mut out = Vec::new();
    // The controller's `ReleaseGrapple` / `Grappling` states.
    if e.gun.released.is_some() {
        out.push(RumbleCue::Stop(RumbleKind::Grapple));
    }
    if e.gun.attached.is_some() {
        out.push(RumbleCue::Play(RumbleKind::Grapple));
    }
    match e.power_jump {
        Some(PowerJumpEvent::Fired { .. }) => {
            out.push(RumbleCue::Play(RumbleKind::PowerJumpRelease));
        }
        Some(PowerJumpEvent::Canceled) => out.push(RumbleCue::Play(RumbleKind::PowerJumpCancel)),
        _ => {}
    }
    match e.boots {
        Some(BootsEvent::Started) => out.push(RumbleCue::Play(RumbleKind::PowerJumpCharge)),
        Some(BootsEvent::BoostBegan) => out.push(RumbleCue::Play(RumbleKind::RocketBootsActive)),
        // `Boosting.PlayerLanded`: stops the boost waveform (a landing
        // during the charge leaves the charge waveform to run out).
        Some(BootsEvent::Canceled) => out.push(RumbleCue::Stop(RumbleKind::RocketBootsActive)),
        _ => {}
    }
    if e.landing.is_some_and(|l| l.hard) {
        out.push(RumbleCue::Play(RumbleKind::HardLanding));
    }
    out
}

/// What the motors are told.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum MotorCommand {
    /// Stop every rumble.
    Stop,
    /// Rumble with `kind`'s intensity for `seconds`.
    Start {
        /// The waveform.
        kind: RumbleKind,
        /// How long.
        seconds: f32,
    },
}

/// The stock `ForceFeedbackManager`'s rules over Bevy's additive rumble
/// requests (see the module docs): one waveform at a time, a new one
/// replaces the current one, a stop only stops the waveform it names, and
/// the waveform pauses with the game.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct RumbleMixer {
    /// The current waveform and the seconds left of it (looping: of its
    /// current motor request).
    current: Option<(RumbleKind, f32)>,
    paused: bool,
}

impl RumbleMixer {
    /// The waveform that is current (playing or paused).
    #[must_use]
    pub fn current(&self) -> Option<RumbleKind> {
        self.current.map(|(k, _)| k)
    }

    /// Applies a cue.
    pub fn apply(&mut self, cue: RumbleCue) -> Vec<MotorCommand> {
        let mut out = Vec::new();
        match cue {
            RumbleCue::Play(kind) => {
                let seconds = kind.request_seconds();
                if self.current.replace((kind, seconds)).is_some() && !self.paused {
                    out.push(MotorCommand::Stop);
                }
                if !self.paused {
                    out.push(MotorCommand::Start { kind, seconds });
                }
            }
            RumbleCue::Stop(kind) => {
                if self.current() == Some(kind) {
                    self.current = None;
                    if !self.paused {
                        out.push(MotorCommand::Stop);
                    }
                }
            }
        }
        out
    }

    /// `dt` seconds of play: a finished waveform ends (its motor request
    /// ran out by itself), a looping one gets a new request.
    pub fn advance(&mut self, dt: f32) -> Vec<MotorCommand> {
        let mut out = Vec::new();
        if self.paused || !dt.is_finite() || dt <= 0.0 {
            return out;
        }
        if let Some((kind, left)) = self.current.as_mut() {
            *left -= dt;
            if *left <= 0.0 {
                if kind.looping() {
                    *left = kind.request_seconds();
                    out.push(MotorCommand::Start {
                        kind: *kind,
                        seconds: *left,
                    });
                } else {
                    self.current = None;
                }
            }
        }
        out
    }

    /// Pauses (motors off, the waveform kept) or resumes (the rest of the
    /// waveform plays).
    pub fn set_paused(&mut self, paused: bool) -> Vec<MotorCommand> {
        if self.paused == paused {
            return Vec::new();
        }
        self.paused = paused;
        match self.current {
            None => Vec::new(),
            Some(_) if paused => vec![MotorCommand::Stop],
            Some((kind, seconds)) => vec![MotorCommand::Start { kind, seconds }],
        }
    }

    /// Drops everything (a new game, no game, rumble switched off).
    pub fn reset(&mut self) -> Vec<MotorCommand> {
        let playing = self.current.is_some() && !self.paused;
        *self = Self::default();
        if playing {
            vec![MotorCommand::Stop]
        } else {
            Vec::new()
        }
    }
}

/// Rumble state of the app (the mixer and whether the power jump was seen
/// charging last frame).
#[derive(Debug, Default)]
struct RumbleState {
    mixer: RumbleMixer,
    charging: bool,
}

/// Plays the tick events' rumble on every connected controller through
/// the [`RumbleMixer`].
#[allow(clippy::too_many_arguments)]
fn rumble(
    mut ticks: MessageReader<crate::ui::GameTick>,
    layout: Res<GamepadLayout>,
    in_use: Res<GamepadInUse>,
    time: Res<Time<Real>>,
    sim: Option<Res<Sim>>,
    gamepads: Query<Entity, With<Gamepad>>,
    mut state: Local<RumbleState>,
    mut requests: MessageWriter<GamepadRumbleRequest>,
) {
    use asamu_player::pawn::PowerJumpStateName;
    let mut commands = Vec::new();
    match sim {
        Some(sim) if layout.rumble => {
            if sim.is_added() {
                // A new game starts silent.
                commands.extend(state.mixer.reset());
                state.charging = false;
            }
            let playing = sim.game.state() == GameState::Playing;
            commands.extend(state.mixer.set_paused(!playing));
            commands.extend(state.mixer.advance(time.delta_secs()));
            // `Charging`'s state code starts the charge rumble.
            let charging =
                sim.game.player().script.power_jump.state == PowerJumpStateName::Charging;
            let mut cues = Vec::new();
            if charging && !state.charging {
                cues.push(RumbleCue::Play(RumbleKind::PowerJumpCharge));
            }
            state.charging = charging;
            cues.extend(ticks.read().flat_map(|t| rumble_cues(&t.0.events)));
            for cue in cues {
                // Nothing new plays unless a gamepad is in use; stops
                // always apply.
                if matches!(cue, RumbleCue::Play(_)) && !in_use.0 {
                    continue;
                }
                commands.extend(state.mixer.apply(cue));
            }
        }
        _ => {
            ticks.clear();
            commands.extend(state.mixer.reset());
            state.charging = false;
        }
    }
    for gamepad in &gamepads {
        for command in &commands {
            match *command {
                MotorCommand::Stop => {
                    requests.write(GamepadRumbleRequest::Stop { gamepad });
                }
                MotorCommand::Start { kind, seconds } => {
                    requests.write(GamepadRumbleRequest::Add {
                        duration: Duration::from_secs_f32(seconds.clamp(0.0, 60.0)),
                        intensity: kind.waveform().intensity(),
                        gamepad,
                    });
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dead_zone_rescales_like_the_original() {
        assert_eq!(dead_zone(0.3, 0.4), 0.0);
        assert_eq!(dead_zone(-0.4, 0.4), 0.0);
        assert!((dead_zone(0.7, 0.4) - 0.5).abs() < 1e-6);
        assert!((dead_zone(-0.7, 0.4) + 0.5).abs() < 1e-6);
        assert_eq!(dead_zone(1.0, 0.4), 1.0);
        assert_eq!(dead_zone(-1.0, 0.2), -1.0);
        // No dead zone (0 or out of range) leaves the value.
        assert_eq!(dead_zone(0.05, 0.0), 0.05);
        assert_eq!(dead_zone(0.05, 1.0), 0.05);
        assert_eq!(dead_zone(f32::NAN, 0.2), 0.0);
    }

    #[test]
    fn default_layout_moves_on_the_left_stick_and_looks_on_the_right() {
        let pad = PadState {
            left_x: 1.0,
            left_y: 0.7,
            right_x: -0.6,
            right_y: 1.0,
            ..PadState::default()
        };
        let a = pad_axes(&pad, 0);
        assert_eq!(a.strafe, 1.0);
        assert!((a.forward - 0.5).abs() < 1e-6);
        assert!((a.turn + 0.5).abs() < 1e-6);
        assert!((a.look - LOOK_SPEED).abs() < 1e-6);
        // Swapped sticks: the reverse bindings keep "up = forward / look up".
        let b = pad_axes(
            &PadState {
                left_x: -0.6,
                left_y: -1.0,
                right_x: 1.0,
                right_y: -0.7,
                ..PadState::default()
            },
            1,
        );
        assert_eq!(b.strafe, 1.0);
        assert!(
            (b.forward - 0.5).abs() < 1e-6,
            "right stick up moves forward"
        );
        assert!((b.turn + 0.5).abs() < 1e-6);
        assert!(
            (b.look - LOOK_SPEED).abs() < 1e-6,
            "left stick down looks down"
        );
    }

    #[test]
    fn look_rates_follow_player_input_scaling() {
        // Full right turn for one second at 60 frames: 30,000 units.
        let axes = PadAxes {
            turn: 1.0,
            look: LOOK_SPEED,
            ..PadAxes::default()
        };
        let (mut yaw, mut pitch) = (0.0f32, 0.0f32);
        for _ in 0..60 {
            let (y, p) = look_radians(&axes, 1.0 / 60.0, CONTROLLER_SENSITIVITY);
            yaw += y;
            pitch += p;
        }
        let units = |r: f32| r / std::f32::consts::TAU * ROTATOR_UNITS_PER_TURN;
        assert!((units(yaw) - 30_000.0).abs() < 1.0, "{}", units(yaw));
        // Stick down (positive look axis) pitches down at 16,250 units/s.
        assert!((units(pitch) + 16_250.0).abs() < 1.0, "{}", units(pitch));
        let (y2, _) = look_radians(&axes, 1.0 / 60.0, 2.0);
        assert!((y2 - 2.0 * look_radians(&axes, 1.0 / 60.0, 1.0).0).abs() < 1e-7);
        assert_eq!(look_radians(&axes, 0.0, 1.0), (0.0, 0.0));
        assert_eq!(look_radians(&axes, f32::NAN, 1.0), (0.0, 0.0));
    }

    #[test]
    fn button_layouts_follow_the_settings_arrays() {
        let pad = PadState {
            a: true,
            right_trigger: 0.11,
            left_shoulder: true,
            ..PadState::default()
        };
        assert_eq!(pad_buttons(&pad, 0), (true, true, true, false));
        // Layout 1: LB is the power jump, the right trigger does nothing.
        assert_eq!(pad_buttons(&pad, 1), (true, false, false, true));
        let lt = PadState {
            left_trigger: 0.5,
            right_shoulder: true,
            ..PadState::default()
        };
        assert_eq!(pad_buttons(&lt, 1), (false, true, true, false));
        assert_eq!(pad_buttons(&lt, 0), (false, false, false, true));
        // At or below 10 % of the travel a trigger is not pressed.
        let light = PadState {
            right_trigger: TRIGGER_THRESHOLD,
            ..PadState::default()
        };
        assert!(!pad_buttons(&light, 0).1);
    }

    #[test]
    fn actions_merge_into_the_keyboard_frame() {
        let pad = PadState {
            left_y: 1.0,
            left_x: -0.7,
            a: true,
            right_trigger: 1.0,
            ..PadState::default()
        };
        let actions = GamepadActions::from_pad(&pad, &GamepadLayout::default());
        let mut frame = InputFrame {
            move_forward: 1.0,
            sprint_held: true,
            ..InputFrame::default()
        };
        actions.merge_into(&mut frame);
        assert_eq!(frame.move_forward, 1.0, "sums clamp to the axis range");
        assert!((frame.move_right + 0.5).abs() < 1e-6);
        assert!(frame.jump_held && frame.grapple_held && frame.sprint_held);
        assert!(!frame.power_jump_held);
        assert!(
            !frame.jump_pressed,
            "the press is latched through PendingInput"
        );
        // The default actions change nothing.
        let mut f2 = InputFrame::default();
        GamepadActions::default().merge_into(&mut f2);
        assert_eq!(f2, InputFrame::default());
        let bad = GamepadActions {
            move_forward: f32::NAN,
            ..GamepadActions::default()
        };
        bad.merge_into(&mut f2);
        assert_eq!(f2.move_forward, 0.0);
    }

    #[test]
    fn controllers_combine() {
        let a = PadState {
            left_y: 0.5,
            a: true,
            right_trigger: 0.2,
            ..PadState::default()
        };
        let b = PadState {
            left_y: 0.25,
            right_shoulder: true,
            right_trigger: 0.9,
            ..PadState::default()
        };
        let c = a.combine(b);
        assert_eq!(c.left_y, 0.75);
        assert!(c.a && c.right_shoulder);
        assert_eq!(c.right_trigger, 0.9);
    }

    #[test]
    fn quick_load_follows_the_chapter_and_the_hud_option() {
        assert!(!quick_load_allowed(Some(ChapterId::Workshop), None));
        assert!(!quick_load_allowed(Some(ChapterId::Epilogue), Some(true)));
        assert!(quick_load_allowed(Some(ChapterId::IceCave), None));
        assert!(quick_load_allowed(Some(ChapterId::IceCave), Some(true)));
        assert!(quick_load_allowed(None, None));
        // Kismet switched the HUD's restart-from-checkpoint option off.
        assert!(!quick_load_allowed(Some(ChapterId::IceCave), Some(false)));
        assert!(!quick_load_allowed(None, Some(false)));
    }

    #[test]
    fn waveforms_become_constant_intensities() {
        let i = RUMBLE_POWER_JUMP_RELEASE.intensity();
        assert!((i.strong_motor - 0.45).abs() < 1e-6);
        assert!((i.weak_motor - 0.45).abs() < 1e-6);
        let h = RUMBLE_HARD_LANDING.intensity();
        assert!((h.strong_motor - 0.175).abs() < 1e-6);
        let b = RUMBLE_ROCKET_BOOTS_ACTIVE.intensity();
        assert!((b.strong_motor - 0.45 * std::f32::consts::FRAC_2_PI).abs() < 1e-6);
        assert!((b.weak_motor - 0.25).abs() < 1e-6);
    }

    #[test]
    fn tick_events_map_to_rumble_cues() {
        use asamu_player::pawn::PowerJumpEvent;
        use asamu_player::rocket_boots::BootsEvent;
        let mut r = asamu_player::StepEvents::default();
        assert!(rumble_cues(&r).is_empty());
        r.power_jump = Some(PowerJumpEvent::Fired {
            leap: false,
            jumped: true,
        });
        r.boots = Some(BootsEvent::Canceled);
        assert_eq!(
            rumble_cues(&r),
            vec![
                RumbleCue::Play(RumbleKind::PowerJumpRelease),
                RumbleCue::Stop(RumbleKind::RocketBootsActive)
            ]
        );
        r.power_jump = Some(PowerJumpEvent::Canceled);
        r.boots = Some(BootsEvent::BoostBegan);
        assert_eq!(
            rumble_cues(&r),
            vec![
                RumbleCue::Play(RumbleKind::PowerJumpCancel),
                RumbleCue::Play(RumbleKind::RocketBootsActive)
            ]
        );
        // The boots' charge plays the power jump's charge waveform; the
        // exhausted press and the normal end of a boost rumble nothing.
        r.power_jump = None;
        r.boots = Some(BootsEvent::Started);
        assert_eq!(
            rumble_cues(&r),
            vec![RumbleCue::Play(RumbleKind::PowerJumpCharge)]
        );
        for quiet in [BootsEvent::Exhausted, BootsEvent::Finished] {
            r.boots = Some(quiet);
            assert!(rumble_cues(&r).is_empty(), "{quiet:?}");
        }
    }

    #[test]
    fn waveform_kinds_carry_the_class_defaults() {
        let all = [
            RumbleKind::PowerJumpCharge,
            RumbleKind::PowerJumpRelease,
            RumbleKind::PowerJumpCancel,
            RumbleKind::Grapple,
            RumbleKind::RocketBootsActive,
            RumbleKind::HardLanding,
        ];
        for k in all {
            let w = k.waveform();
            assert!(w.duration > 0.0 && w.left > 0.0 && w.right > 0.0, "{k:?}");
            let i = w.intensity();
            assert!(i.strong_motor > 0.0 && i.strong_motor <= 1.0, "{k:?}");
            assert!(i.weak_motor > 0.0 && i.weak_motor <= 1.0, "{k:?}");
            assert_eq!(k.looping(), k == RumbleKind::Grapple);
            let expected = if k.looping() {
                GRAPPLE_RUMBLE_SECONDS
            } else {
                w.duration
            };
            assert_eq!(k.request_seconds(), expected);
        }
        assert_eq!(RumbleKind::HardLanding.waveform().duration, 1.2);
        assert_eq!(RumbleKind::Grapple.waveform().duration, 0.2);
        assert_eq!(RumbleKind::PowerJumpCharge.waveform().duration, 0.9);
        assert_eq!(RumbleKind::PowerJumpRelease.waveform().duration, 0.48);
        assert_eq!(RumbleKind::PowerJumpCancel.waveform().duration, 0.55);
        assert_eq!(RumbleKind::RocketBootsActive.waveform().duration, 2.0);
    }

    #[test]
    fn one_waveform_plays_at_a_time_and_stops_name_their_waveform() {
        use MotorCommand::{Start, Stop};
        use RumbleKind::{Grapple, HardLanding, PowerJumpCharge, RocketBootsActive};
        let mut m = RumbleMixer::default();
        assert_eq!(m.current(), None);
        // Nothing current: a stop does nothing.
        assert!(m.apply(RumbleCue::Stop(Grapple)).is_empty());
        // The first waveform just starts.
        assert_eq!(
            m.apply(RumbleCue::Play(HardLanding)),
            vec![Start {
                kind: HardLanding,
                seconds: 1.2
            }]
        );
        // A new waveform replaces the current one (never both).
        assert_eq!(
            m.apply(RumbleCue::Play(Grapple)),
            vec![
                Stop,
                Start {
                    kind: Grapple,
                    seconds: GRAPPLE_RUMBLE_SECONDS
                }
            ]
        );
        assert_eq!(m.current(), Some(Grapple));
        // A stop for another waveform leaves it (a landing that ends a
        // boost while the grapple rumbles).
        assert!(m.apply(RumbleCue::Stop(RocketBootsActive)).is_empty());
        assert_eq!(m.current(), Some(Grapple));
        // Its own stop ends it.
        assert_eq!(m.apply(RumbleCue::Stop(Grapple)), vec![Stop]);
        assert_eq!(m.current(), None);
        // A hard landing that also ends a boost: either order leaves the
        // landing waveform playing.
        for cues in [
            [
                RumbleCue::Stop(RocketBootsActive),
                RumbleCue::Play(HardLanding),
            ],
            [
                RumbleCue::Play(HardLanding),
                RumbleCue::Stop(RocketBootsActive),
            ],
        ] {
            let mut m = RumbleMixer::default();
            m.apply(RumbleCue::Play(RocketBootsActive));
            for c in cues {
                m.apply(c);
            }
            assert_eq!(m.current(), Some(HardLanding));
        }
        // The boots' landing during their charge does not stop the charge
        // waveform.
        let mut m = RumbleMixer::default();
        m.apply(RumbleCue::Play(PowerJumpCharge));
        assert!(m.apply(RumbleCue::Stop(RocketBootsActive)).is_empty());
        assert_eq!(m.current(), Some(PowerJumpCharge));
    }

    #[test]
    fn waveforms_run_out_loop_and_pause_with_the_game() {
        use MotorCommand::{Start, Stop};
        use RumbleKind::{Grapple, PowerJumpRelease};
        let mut m = RumbleMixer::default();
        m.apply(RumbleCue::Play(PowerJumpRelease));
        assert!(m.advance(0.4).is_empty());
        assert_eq!(m.current(), Some(PowerJumpRelease));
        // Paused: the motors stop, the waveform and its rest are kept and
        // no time passes.
        assert_eq!(m.set_paused(true), vec![Stop]);
        assert!(m.set_paused(true).is_empty());
        assert!(m.advance(10.0).is_empty());
        assert_eq!(m.current(), Some(PowerJumpRelease));
        // A cue while paused changes the waveform without touching the
        // motors.
        assert!(m.apply(RumbleCue::Stop(Grapple)).is_empty());
        let resumed = m.set_paused(false);
        assert_eq!(resumed.len(), 1);
        let Start { kind, seconds } = resumed[0] else {
            panic!("resumes");
        };
        assert_eq!(kind, PowerJumpRelease);
        assert!((seconds - 0.08).abs() < 1e-6, "{seconds}");
        // It runs out by itself.
        assert!(m.advance(0.1).is_empty());
        assert_eq!(m.current(), None);
        assert!(m.advance(1.0).is_empty());
        // The looping grapple waveform is renewed until it is stopped.
        m.apply(RumbleCue::Play(Grapple));
        assert!(m.advance(GRAPPLE_RUMBLE_SECONDS - 1.0).is_empty());
        assert_eq!(
            m.advance(2.0),
            vec![Start {
                kind: Grapple,
                seconds: GRAPPLE_RUMBLE_SECONDS
            }]
        );
        assert_eq!(m.current(), Some(Grapple));
        // Played while paused: silent until the game resumes.
        let mut p = RumbleMixer::default();
        assert!(p.set_paused(true).is_empty());
        assert!(p.apply(RumbleCue::Play(Grapple)).is_empty());
        assert_eq!(
            p.set_paused(false),
            vec![Start {
                kind: Grapple,
                seconds: GRAPPLE_RUMBLE_SECONDS
            }]
        );
        // A reset silences a playing waveform, once.
        assert_eq!(p.reset(), vec![Stop]);
        assert!(p.reset().is_empty());
        assert_eq!(p, RumbleMixer::default());
        // Bad frame times change nothing.
        m.advance(f32::NAN);
        m.advance(-1.0);
        assert_eq!(m.current(), Some(Grapple));
    }

    #[test]
    fn the_last_device_used_decides_whether_a_gamepad_is_in_use() {
        assert!(next_in_use(false, true, false));
        assert!(next_in_use(false, true, true), "the pad wins a tie");
        assert!(!next_in_use(true, false, true));
        assert!(next_in_use(true, false, false), "no input keeps it");
        assert!(!next_in_use(false, false, false));
        // What counts as gamepad input: a stick past its dead zone, a
        // trigger past its threshold, a button.
        let layout = GamepadLayout::default();
        assert!(!PadState::default().active(&layout));
        let rest = PadState {
            left_x: 0.39,
            right_y: -0.19,
            right_trigger: TRIGGER_THRESHOLD,
            ..PadState::default()
        };
        assert!(!rest.active(&layout), "inside every dead zone");
        for pad in [
            PadState {
                left_y: 0.41,
                ..PadState::default()
            },
            PadState {
                right_x: -0.21,
                ..PadState::default()
            },
            PadState {
                left_trigger: 0.2,
                ..PadState::default()
            },
            PadState {
                a: true,
                ..PadState::default()
            },
            PadState {
                right_shoulder: true,
                ..PadState::default()
            },
        ] {
            assert!(pad.active(&layout), "{pad:?}");
        }
    }

    #[test]
    fn combined_sticks_stay_within_a_full_deflection() {
        let full = PadState {
            left_x: 1.0,
            left_y: -1.0,
            right_x: 1.0,
            right_y: 1.0,
            right_trigger: 1.0,
            ..PadState::default()
        };
        let c = full.combine(full);
        assert_eq!(
            (c.left_x, c.left_y, c.right_x, c.right_y),
            (1.0, -1.0, 1.0, 1.0)
        );
        assert_eq!(c.right_trigger, 1.0);
        // Two full sticks turn no faster than one.
        assert_eq!(pad_axes(&c, 0), pad_axes(&full, 0));
        // Damaged values read as centred sticks and released triggers.
        let bad = PadState {
            left_x: f32::NAN,
            left_y: f32::INFINITY,
            right_x: -7.0,
            left_trigger: f32::NAN,
            right_trigger: -3.0,
            ..PadState::default()
        };
        let c = PadState::default().combine(bad);
        assert_eq!((c.left_x, c.left_y, c.right_x), (0.0, 0.0, -1.0));
        assert_eq!((c.left_trigger, c.right_trigger), (0.0, 0.0));
        assert_eq!(stick(f32::NEG_INFINITY), 0.0);
        let a = GamepadActions::from_pad(&c, &GamepadLayout::default());
        assert!(a.move_forward.is_finite() && a.move_right.is_finite());
    }

    #[test]
    fn a_long_frame_does_not_swing_the_view() {
        let axes = PadAxes {
            turn: 1.0,
            look: LOOK_SPEED,
            ..PadAxes::default()
        };
        let capped = look_radians(&axes, MAX_LOOK_FRAME_SECONDS, 1.0);
        assert_eq!(look_radians(&axes, 5.0, 1.0), capped);
        assert_eq!(look_radians(&axes, f32::INFINITY, 1.0), (0.0, 0.0));
        // Under the cap the turn is proportional to the frame time.
        let half = look_radians(&axes, MAX_LOOK_FRAME_SECONDS / 2.0, 1.0);
        assert!((half.0 * 2.0 - capped.0).abs() < 1e-6);
        // A full turn of the stick for the capped frame: 3,000 units.
        let units = capped.0 / std::f32::consts::TAU * ROTATOR_UNITS_PER_TURN;
        assert!((units - 3_000.0).abs() < 0.5, "{units}");
    }

    /// A rumble request as the test compares it.
    #[derive(Debug, Clone, Copy, PartialEq)]
    enum Sent {
        Stop(Entity),
        Add {
            gamepad: Entity,
            seconds: f32,
            strong: f32,
        },
    }

    /// Rumble requests of the frames so far, in order.
    #[derive(Resource, Default)]
    struct Requests(Vec<Sent>);

    fn collect(mut reader: MessageReader<GamepadRumbleRequest>, mut out: ResMut<Requests>) {
        for r in reader.read() {
            out.0.push(match r {
                GamepadRumbleRequest::Stop { gamepad } => Sent::Stop(*gamepad),
                GamepadRumbleRequest::Add {
                    duration,
                    intensity,
                    gamepad,
                } => Sent::Add {
                    gamepad: *gamepad,
                    seconds: duration.as_secs_f32(),
                    strong: intensity.strong_motor,
                },
            });
        }
    }

    fn tick_with(events: asamu_player::StepEvents) -> crate::ui::GameTick {
        crate::ui::GameTick(asamu_game::TickReport {
            tick: 1,
            events,
            respawned: false,
            checkpoint_activated: None,
            world: Default::default(),
            died: None,
        })
    }

    /// The plugin as registered, without a window, a device layer or the
    /// rest of the app: its systems' parameters and schedules are valid, a
    /// connected pad drives the actions and the pending input, and the
    /// rumble follows the game (in use, paused, new game).
    #[test]
    fn the_plugin_runs_headless_and_a_pad_drives_actions_and_rumble() {
        use asamu_player::pawn::PowerJumpEvent;
        let mut app = App::new();
        app.add_plugins(MinimalPlugins)
            .add_message::<crate::ui::GameTick>()
            .add_message::<GamepadRumbleRequest>()
            .init_resource::<PendingInput>()
            .init_resource::<Requests>()
            .add_plugins(GamepadPlugin)
            .add_systems(Update, collect.after(rumble));
        // No game and no pad: nothing to do.
        app.update();
        assert_eq!(
            *app.world().resource::<GamepadActions>(),
            GamepadActions::default()
        );
        assert!(!app.world().resource::<GamepadInUse>().0);

        let mut sim = Sim::new(asamu_game::Game::graybox().unwrap(), String::new());
        sim.game.start();
        app.insert_resource(sim);
        let mut pad = Gamepad::default();
        pad.analog_mut().set(GamepadAxis::LeftStickY, 1.0);
        pad.analog_mut().set(GamepadAxis::RightStickX, 1.0);
        pad.analog_mut().set(GamepadButton::RightTrigger2, 0.8);
        pad.digital_mut().press(GamepadButton::South);
        let id = app.world_mut().spawn(pad).id();
        std::thread::sleep(Duration::from_millis(2));
        app.update();
        let a = *app.world().resource::<GamepadActions>();
        assert_eq!(a.move_forward, 1.0);
        assert_eq!(a.move_right, 0.0);
        assert!(a.jump_held && a.grapple_held && !a.sprint_held && !a.power_jump_held);
        assert!(app.world().resource::<GamepadInUse>().0);
        let p = app.world().resource::<PendingInput>();
        assert!(p.jump, "the press is latched for the next tick");
        assert!(p.look_yaw > 0.0 && p.look_pitch == 0.0, "{}", p.look_yaw);

        // A power jump fires: its waveform starts on the pad.
        app.world_mut()
            .write_message(tick_with(asamu_player::StepEvents {
                power_jump: Some(PowerJumpEvent::Fired {
                    leap: false,
                    jumped: true,
                }),
                ..Default::default()
            }));
        app.update();
        let sent = std::mem::take(&mut app.world_mut().resource_mut::<Requests>().0);
        assert_eq!(sent.len(), 1, "{sent:?}");
        let Sent::Add {
            gamepad,
            seconds,
            strong,
        } = sent[0]
        else {
            panic!("{sent:?}");
        };
        assert_eq!(gamepad, id);
        assert!((seconds - 0.48).abs() < 1e-6, "{seconds}");
        assert!((strong - 0.45).abs() < 1e-6, "{strong}");
        // The game pauses: the motors stop, and the paused pad publishes
        // no actions.
        app.world_mut().resource_mut::<Sim>().game.pause();
        app.update();
        let sent = std::mem::take(&mut app.world_mut().resource_mut::<Requests>().0);
        assert_eq!(sent, vec![Sent::Stop(id)]);
        assert_eq!(
            *app.world().resource::<GamepadActions>(),
            GamepadActions::default()
        );
        // With the keyboard in use nothing new plays.
        app.world_mut().resource_mut::<Sim>().game.resume();
        app.world_mut().entity_mut(id).insert(Gamepad::default());
        app.world_mut().resource_mut::<GamepadInUse>().0 = false;
        std::thread::sleep(Duration::from_millis(600));
        app.update();
        app.world_mut().resource_mut::<Requests>().0.clear();
        app.world_mut()
            .write_message(tick_with(asamu_player::StepEvents {
                power_jump: Some(PowerJumpEvent::Canceled),
                ..Default::default()
            }));
        app.update();
        assert!(app.world().resource::<Requests>().0.is_empty());
        assert_eq!(
            *app.world().resource::<GamepadActions>(),
            GamepadActions::default(),
            "a centred pad publishes nothing"
        );
        // Rumble switched off, or the game gone: silent and reset.
        app.world_mut().resource_mut::<GamepadInUse>().0 = true;
        app.world_mut().resource_mut::<GamepadLayout>().rumble = false;
        app.world_mut()
            .write_message(tick_with(asamu_player::StepEvents {
                power_jump: Some(PowerJumpEvent::Canceled),
                ..Default::default()
            }));
        app.update();
        assert!(app.world().resource::<Requests>().0.is_empty());
        app.world_mut().remove_resource::<Sim>();
        app.update();
        assert!(app.world().resource::<Requests>().0.is_empty());
    }
}
