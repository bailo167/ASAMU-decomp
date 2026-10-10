//! The in-play Sandbox HUD: the permanent watermark line, the session state,
//! the quick tuner, the speed, jump and swing read-outs, the save-state
//! slots and the latest outcome.
//!
//! Non-interactive text in the left half of the window (so it never reaches
//! the crosshair), placed under the developer read-out while that is shown
//! (F1). The watermark and the state line are always there while a session
//! runs; H hides the rest. While the inspector is open the HUD is hidden and
//! the panel's header is the watermark.
//!
//! The read-outs describe this recreation as it runs in the session. They
//! are not measurements of the original.

use asamu_sandbox::inspect::{PlayerView, SessionSummary, SlotView, TelemetryView, format_value};
use asamu_sandbox::keys::TuneValue;
use asamu_sandbox::overlay::ParamSetLabel;
use asamu_sandbox::session::Session;
use asamu_sandbox::telemetry::{JumpStats, SwingStats};
use bevy::ecs::schedule::common_conditions::any_with_component;
use bevy::prelude::*;
use bevy::window::{CursorGrabMode, CursorOptions, PrimaryWindow};

use super::widgets::{FONT, KeepClear, LabUi, STRIP_BG, Tone, ViewState, ascii, text};
use super::{Lab, LabSet, PanelState, Phase, inspection, lab_active};
use crate::Sim;

/// The watermark: what a screenshot of a session always says.
pub(super) const WATERMARK: &str =
    "SANDBOX - experimental - not the original's behaviour - saves off";

/// A speed factor as text (`0.25x`, `1x`).
pub(super) fn speed_factor(speed: f32) -> String {
    format!("{speed}x")
}

/// The latest jump as text.
pub(super) fn jump_text(jump: Option<JumpStats>) -> String {
    match jump {
        Some(j) => format!(
            "last jump: apex {:.0} uu, {:.2} s, {:.0} uu far, landing {:.0} uu/s",
            j.apex_height, j.airtime, j.distance, j.landing_velocity_z
        ),
        None => "last jump: -".to_owned(),
    }
}

/// The latest swing as text.
pub(super) fn swing_text(swing: Option<SwingStats>) -> String {
    match swing {
        Some(s) => format!(
            "last swing: from {:.0} uu, {:.2} s, peak {:.0}, release {:.0} uu/s",
            s.attach_distance, s.duration, s.peak_speed, s.release_speed
        ),
        None => "last swing: -".to_owned(),
    }
}

/// The quick tuner's selected parameter.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct QuickView {
    /// Parameter key.
    pub key: String,
    /// The value the session runs.
    pub current: TuneValue,
    /// The Classic value.
    pub classic: TuneValue,
    /// The session overrides it.
    pub overridden: bool,
    /// Its place in the pinned list, from 1.
    pub place: usize,
    /// Length of the pinned list.
    pub count: usize,
}

/// The quick tuner's selection, read from the session (`None`: nothing is
/// pinned, or the key is not a parameter).
pub(super) fn quick_view(session: &Session, panel: &PanelState) -> Option<QuickView> {
    let key = panel.pinned.get(panel.pinned_index)?;
    let info = session.catalog().get(key)?;
    let overridden = session.overlay().get(key);
    Some(QuickView {
        key: key.clone(),
        current: overridden.unwrap_or(&info.classic).clone(),
        classic: info.classic.clone(),
        overridden: overridden.is_some(),
        place: panel.pinned_index + 1,
        count: panel.pinned.len(),
    })
}

/// A line of the HUD.
#[derive(Component, Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum HudLine {
    /// The permanent watermark.
    Watermark,
    /// Profile, overrides, time state, recording.
    State,
    /// The quick tuner.
    Quick,
    /// Speed and height.
    Speed,
    /// The latest jump.
    Jump,
    /// The latest swing.
    Swing,
    /// Save-state slots and the rewind length.
    Slots,
    /// The latest outcome or refusal.
    Notice,
    /// Key help, first line.
    Keys,
    /// Key help, second line.
    MoreKeys,
}

impl HudLine {
    /// Every line, top to bottom.
    const ALL: [Self; 10] = [
        Self::Watermark,
        Self::State,
        Self::Quick,
        Self::Speed,
        Self::Jump,
        Self::Swing,
        Self::Slots,
        Self::Notice,
        Self::Keys,
        Self::MoreKeys,
    ];

    /// Shown even when the read-outs are hidden (H).
    fn permanent(self) -> bool {
        matches!(self, Self::Watermark | Self::State)
    }

    fn tone(self) -> Tone {
        match self {
            Self::Watermark | Self::Notice => Tone::Notice,
            Self::State | Self::Quick | Self::Speed | Self::Jump | Self::Swing | Self::Slots => {
                Tone::Normal
            }
            Self::Keys | Self::MoreKeys => Tone::Hint,
        }
    }
}

/// What the HUD is written from.
#[derive(Clone, Debug)]
pub(super) struct HudData {
    /// The session at a glance.
    pub summary: SessionSummary,
    /// The player.
    pub player: PlayerView,
    /// The read-outs.
    pub telemetry: TelemetryView,
    /// The save-state slots.
    pub slots: Vec<SlotView>,
    /// The quick tuner's selection.
    pub quick: Option<QuickView>,
    /// The latest outcome or refusal.
    pub notice: Option<String>,
    /// The mouse is captured (the hotkeys work).
    pub grabbed: bool,
}

/// The text of a HUD line (`None`: the line is not shown).
pub(super) fn hud_line(line: HudLine, d: &HudData) -> Option<String> {
    let s = &d.summary;
    Some(match line {
        HudLine::Watermark => WATERMARK.to_owned(),
        HudLine::State => {
            let set = match s.label {
                ParamSetLabel::Classic => "Classic parameter set, unchanged".to_owned(),
                ParamSetLabel::Placeholder => "placeholder parameter set".to_owned(),
                ParamSetLabel::Modified { overrides: 1 } => "1 override".to_owned(),
                ParamSetLabel::Modified { overrides } => format!("{overrides} overrides"),
            };
            let mut out = format!(
                "profile {}{} ({set}) | time {}",
                s.profile,
                if s.profile_modified { "*" } else { "" },
                speed_factor(s.speed)
            );
            if s.frozen {
                out.push_str(" FROZEN");
            }
            if s.pending_steps > 0 {
                out.push_str(&format!(" ({} steps pending)", s.pending_steps));
            }
            if !s.rules.is_default() {
                out.push_str(" | rules on");
            }
            if s.recording {
                out.push_str(" | REC");
            }
            if s.flying {
                out.push_str(" | FLY placement");
            }
            out
        }
        HudLine::Quick => match &d.quick {
            Some(q) => format!(
                "quick < {} = {}{} > {}/{}",
                q.key,
                format_value(&q.current),
                if q.overridden {
                    format!(" * (classic {})", format_value(&q.classic))
                } else {
                    " (classic)".to_owned()
                },
                q.place,
                q.count
            ),
            None => "quick: nothing pinned (pin parameters in the inspector)".to_owned(),
        },
        HudLine::Speed => format!(
            "speed {:.0} uu/s (peak {:.0}) | height {:.0} uu | {}",
            d.player.speed, d.telemetry.peak_speed, d.player.position.z, d.player.physics
        ),
        HudLine::Jump => jump_text(d.telemetry.last_jump),
        HudLine::Swing => swing_text(d.telemetry.last_swing),
        HudLine::Slots => {
            let mut out = "slots".to_owned();
            let mut selected = 0;
            for slot in &d.slots {
                if slot.selected {
                    selected = slot.slot + 1;
                }
                out.push_str(&match slot.tick {
                    Some(tick) => format!(" {}:t{tick}", slot.slot + 1),
                    None => format!(" {}:-", slot.slot + 1),
                });
            }
            out.push_str(&format!(" (sel {selected})"));
            if s.rewind_available {
                out.push_str(&format!(" | rewind {:.0} s", s.rewind_seconds));
            }
            out
        }
        HudLine::Notice => return d.notice.as_ref().map(|n| format!("> {n}")),
        HudLine::Keys => {
            let keys = if d.grabbed {
                "` inspector | [ ] pick  - = step  Backspace reset | P freeze | O step"
            } else {
                "click to capture the mouse: the Sandbox keys work in play | ` inspector"
            };
            keys.to_owned()
        }
        HudLine::MoreKeys => {
            if !d.grabbed {
                return None;
            }
            // F9 is named because the developer read-out above still calls
            // it "record trace": in a session it makes a Sandbox recording.
            ", . / speed | 1-4 K L slots | Z rewind | T G B V teleport | N fly | \
             F9 sandbox recording | H hide"
                .to_owned()
        }
    })
}

/// Root of the HUD.
#[derive(Component)]
struct HudRoot;

/// Distance of the HUD from the window's top-left corner, pixels.
const MARGIN: f32 = 10.0;

fn spawn(commands: &mut Commands) {
    commands
        .spawn((
            HudRoot,
            LabUi,
            KeepClear,
            Node {
                position_type: PositionType::Absolute,
                left: Val::Px(MARGIN),
                top: Val::Px(MARGIN),
                // The left half only: never under the crosshair. A definite
                // width, so a line that wraps is measured as wrapped.
                width: Val::Percent(48.0),
                flex_direction: FlexDirection::Column,
                padding: UiRect::axes(Val::Px(8.0), Val::Px(5.0)),
                border_radius: BorderRadius::all(Val::Px(4.0)),
                ..default()
            },
            BackgroundColor(STRIP_BG),
            GlobalZIndex(60),
            Pickable::IGNORE,
        ))
        .with_children(|root| {
            for line in HudLine::ALL {
                let size = if line == HudLine::Watermark {
                    FONT + 1.0
                } else {
                    FONT
                };
                root.spawn((line, text("", size, line.tone().color()), Pickable::IGNORE));
            }
        });
}

/// Shows the HUD while a session runs and removes it afterwards.
#[allow(clippy::too_many_arguments, clippy::type_complexity)]
fn sync(
    mut commands: Commands,
    lab: Res<Lab>,
    sim: Option<Res<Sim>>,
    panel: Res<PanelState>,
    view: Res<ViewState>,
    debug: Option<Res<crate::hud::DebugInfo>>,
    cursor: Query<&CursorOptions, With<PrimaryWindow>>,
    info: Query<&ComputedNode, With<crate::hud::HudInfoPanel>>,
    mut roots: Query<(Entity, &mut Node, &mut Visibility), (With<HudRoot>, Without<HudLine>)>,
    mut lines: Query<(&HudLine, &mut Text, &mut Node), Without<HudRoot>>,
) {
    let active = lab.phase == Phase::Active;
    let shown = sim
        .as_deref()
        .filter(|_| active)
        .and_then(|sim| Some((inspection(sim, &lab)?, lab.session.as_ref()?)));
    let Some((inspection, session)) = shown else {
        for (root, ..) in &roots {
            commands.entity(root).despawn();
        }
        return;
    };
    let Ok((_, mut root, mut visibility)) = roots.single_mut() else {
        // Spawned now, written from the next frame on.
        if roots.is_empty() {
            spawn(&mut commands);
        }
        return;
    };
    // The panel's header is the watermark while the inspector is open.
    let wanted = if panel.open {
        Visibility::Hidden
    } else {
        Visibility::Inherited
    };
    if *visibility != wanted {
        *visibility = wanted;
    }
    // Under the developer read-out while it is shown.
    let read_out = debug.is_none_or(|d| d.0);
    let top = info
        .iter()
        .next()
        .filter(|_| read_out)
        .map_or(MARGIN, |node| {
            MARGIN + (node.size.y * node.inverse_scale_factor).ceil() + 6.0
        });
    let top = Val::Px(top);
    if root.top != top {
        root.top = top;
    }
    let data = HudData {
        summary: inspection.summary(),
        player: inspection.player(),
        telemetry: inspection.telemetry(),
        slots: inspection.slots(),
        quick: quick_view(session, &panel),
        notice: lab.notice.clone(),
        grabbed: cursor
            .iter()
            .next()
            .is_some_and(|c| c.grab_mode != CursorGrabMode::None),
    };
    for (line, mut text, mut node) in &mut lines {
        let content = hud_line(*line, &data).filter(|_| view.hud || line.permanent());
        let display = if content.is_some() {
            Display::Flex
        } else {
            Display::None
        };
        if node.display != display {
            node.display = display;
        }
        let content = ascii(&content.unwrap_or_default());
        if text.0 != content {
            text.0 = content;
        }
    }
}

pub(super) fn build(app: &mut App) {
    app.add_systems(
        Update,
        sync.run_if(lab_active.or_else(any_with_component::<HudRoot>))
            .in_set(LabSet::Draw),
    );
}

#[cfg(test)]
mod tests {
    use asamu_game::Game;
    use asamu_sandbox::command::{Command, SlotOp, TimeOp, Toggle};
    use asamu_sandbox::session::SimCx;

    use super::super::Stage;
    use super::*;

    fn lab_and_sim() -> (Lab, Sim) {
        let mut lab = Lab::new(true);
        lab.phase = Phase::Active;
        lab.session = Some(Session::classic());
        lab.stage = Some(Stage::Graybox);
        lab.dirs = None;
        let mut game = Game::graybox().unwrap();
        game.start();
        (lab, Sim::new(game, String::new()))
    }

    fn data(lab: &Lab, sim: &Sim, panel: &PanelState) -> HudData {
        let inspection = inspection(sim, lab).unwrap();
        HudData {
            summary: inspection.summary(),
            player: inspection.player(),
            telemetry: inspection.telemetry(),
            slots: inspection.slots(),
            quick: quick_view(lab.session.as_ref().unwrap(), panel),
            notice: lab.notice.clone(),
            grabbed: true,
        }
    }

    fn run(lab: &mut Lab, sim: &mut Sim, command: Command) {
        let Sim { game, script, .. } = sim;
        lab.session
            .as_mut()
            .unwrap()
            .execute(command, &mut SimCx { game, script })
            .unwrap();
    }

    #[test]
    fn the_watermark_says_what_a_session_is() {
        assert!(WATERMARK.contains("SANDBOX"));
        assert!(WATERMARK.contains("experimental"));
        assert!(WATERMARK.contains("not the original's behaviour"));
        assert!(WATERMARK.contains("saves off"));
        assert!(WATERMARK.is_ascii());
        // It fits one line of the HUD's half window at the default size.
        assert!(WATERMARK.len() <= 70);
    }

    #[test]
    fn a_pristine_session_reads_classic_and_every_line_is_ascii() {
        let (lab, sim) = lab_and_sim();
        let d = data(&lab, &sim, &PanelState::default());
        assert_eq!(hud_line(HudLine::Watermark, &d).unwrap(), WATERMARK);
        let state = hud_line(HudLine::State, &d).unwrap();
        assert_eq!(
            state,
            "profile classic (Classic parameter set, unchanged) | time 1x"
        );
        assert_eq!(
            hud_line(HudLine::Slots, &d).unwrap(),
            "slots 1:- 2:- 3:- 4:- (sel 1) | rewind 0 s"
        );
        assert_eq!(hud_line(HudLine::Jump, &d).unwrap(), "last jump: -");
        assert_eq!(hud_line(HudLine::Swing, &d).unwrap(), "last swing: -");
        assert_eq!(hud_line(HudLine::Notice, &d), None);
        for line in HudLine::ALL {
            if let Some(text) = hud_line(line, &d) {
                assert!(text.is_ascii(), "{line:?}: {text}");
                assert!(!text.is_empty(), "{line:?}");
            }
        }
        // The quick tuner starts on the first pinned key, at its Classic
        // value.
        let quick = hud_line(HudLine::Quick, &d).unwrap();
        assert!(
            quick.starts_with("quick < movement.custom_gravity_scaling = "),
            "{quick}"
        );
        assert!(quick.ends_with("(classic) > 1/7"), "{quick}");
    }

    #[test]
    fn tuning_time_control_and_slots_show_up() {
        let (mut lab, mut sim) = lab_and_sim();
        run(
            &mut lab,
            &mut sim,
            Command::NudgeParam {
                key: "movement.custom_gravity_scaling".to_owned(),
                steps: -1,
                scale: 1.0,
            },
        );
        run(
            &mut lab,
            &mut sim,
            Command::Time {
                op: TimeOp::Scale { value: 0.5 },
            },
        );
        run(
            &mut lab,
            &mut sim,
            Command::Time {
                op: TimeOp::Freeze(Toggle::On),
            },
        );
        run(
            &mut lab,
            &mut sim,
            Command::Slot {
                op: SlotOp::Save { slot: 1 },
            },
        );
        run(
            &mut lab,
            &mut sim,
            Command::Slot {
                op: SlotOp::Select { slot: 1 },
            },
        );
        let d = data(&lab, &sim, &PanelState::default());
        let state = hud_line(HudLine::State, &d).unwrap();
        assert!(
            state.starts_with("profile classic* (1 override) | time 0.5x FROZEN"),
            "{state}"
        );
        let quick = hud_line(HudLine::Quick, &d).unwrap();
        assert!(quick.contains(" * (classic "), "{quick}");
        let slots = hud_line(HudLine::Slots, &d).unwrap();
        assert!(
            slots.starts_with("slots 1:- 2:t0 3:- 4:- (sel 2)"),
            "{slots}"
        );
    }

    #[test]
    fn an_empty_quick_tuner_and_a_free_mouse_say_what_to_do() {
        let (mut lab, sim) = lab_and_sim();
        lab.notice = Some("teleport refused: the grapple is attached".to_owned());
        let panel = PanelState {
            pinned: Vec::new(),
            ..PanelState::default()
        };
        let mut d = data(&lab, &sim, &panel);
        assert!(
            hud_line(HudLine::Quick, &d)
                .unwrap()
                .contains("nothing pinned")
        );
        assert_eq!(
            hud_line(HudLine::Notice, &d).unwrap(),
            "> teleport refused: the grapple is attached"
        );
        let more = hud_line(HudLine::MoreKeys, &d).expect("shown while the mouse is captured");
        assert!(more.contains("F9 sandbox recording"), "{more}");
        d.grabbed = false;
        assert!(
            hud_line(HudLine::Keys, &d)
                .unwrap()
                .starts_with("click to capture the mouse")
        );
        assert_eq!(hud_line(HudLine::MoreKeys, &d), None);
    }

    #[test]
    fn the_watermark_is_there_whenever_a_session_runs_and_gone_after() {
        let (lab, sim) = lab_and_sim();
        let mut app = App::new();
        app.insert_resource(lab)
            .insert_resource(sim)
            .init_resource::<PanelState>()
            .init_resource::<ViewState>()
            .add_systems(Update, sync);
        let lines = |app: &mut App| -> Vec<(HudLine, String, Display)> {
            let world = app.world_mut();
            world
                .query::<(&HudLine, &Text, &Node)>()
                .iter(world)
                .map(|(line, text, node)| (*line, text.0.clone(), node.display))
                .collect()
        };
        let root_visibility = |app: &mut App| {
            let world = app.world_mut();
            world
                .query_filtered::<&Visibility, With<HudRoot>>()
                .iter(world)
                .next()
                .copied()
        };
        // Spawned on the first frame, written on the second.
        app.update();
        app.update();
        let shown = lines(&mut app);
        assert_eq!(shown.len(), HudLine::ALL.len());
        let watermark = |shown: &[(HudLine, String, Display)]| {
            shown
                .iter()
                .find(|(line, ..)| *line == HudLine::Watermark)
                .cloned()
                .unwrap()
        };
        assert_eq!(watermark(&shown).1, WATERMARK);
        assert_eq!(watermark(&shown).2, Display::Flex);
        assert_eq!(root_visibility(&mut app), Some(Visibility::Inherited));
        // H hides the read-outs, never the watermark or the state line.
        app.world_mut().resource_mut::<ViewState>().hud = false;
        app.update();
        let shown = lines(&mut app);
        for (line, _, display) in &shown {
            assert_eq!(
                *display == Display::Flex,
                line.permanent(),
                "{line:?}: {display:?}"
            );
        }
        assert_eq!(watermark(&shown).1, WATERMARK);
        // The inspector covers the HUD; its header is the watermark then.
        app.world_mut().resource_mut::<PanelState>().open = true;
        app.update();
        assert_eq!(root_visibility(&mut app), Some(Visibility::Hidden));
        // Session over: nothing is left.
        app.world_mut().resource_mut::<Lab>().phase = Phase::Idle;
        app.update();
        assert!(lines(&mut app).is_empty());
        let world = app.world_mut();
        assert_eq!(world.query::<&LabUi>().iter(world).count(), 0);
    }
}
