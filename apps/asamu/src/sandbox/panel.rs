//! The inspector panel (`ui::Screen::Sandbox` while a session runs): tabs for
//! tuning, rules and actions, time and save states, visualisers, profiles and
//! the stage.
//!
//! The panel is described as plain data ([`panel_items`], pure and
//! unit-tested) from a [`PanelData`] gathered through the session's
//! read-only `Inspection`, and spawned again whenever that data changes.
//! Everything in it that moves by itself is a `Live` text, written in place
//! every frame ([`live_text`]). Its buttons carry `widgets::Act`s: the panel
//! changes nothing itself.
//!
//! The header is the Sandbox watermark while the panel covers the HUD.
//! Plain `bevy_ui`: steppers instead of sliders, pages instead of scroll
//! areas, name lists instead of text input.

use std::sync::OnceLock;

use asamu_sandbox::command::{Command, SlotOp, TeleportTarget, TimeOp, Toggle};
use asamu_sandbox::inspect::{
    AimView, ParamRow, PlayerView, SessionSummary, SlotView, TelemetryView, TeleportTargetInfo,
    WorldView, format_value,
};
use asamu_sandbox::keys::{Effect, ValueKind};
use asamu_sandbox::profile::Profile;
use asamu_sandbox::rules::{GrappleRule, Rules, Switch};
use asamu_sandbox::session::Session;
use asamu_sandbox::time::SPEED_STEPS;
use bevy::ecs::schedule::common_conditions::any_with_component;
use bevy::prelude::*;

use super::hud::{jump_text, speed_factor, swing_text};
use super::launcher::{StageEntry, page_of, page_row, stages, start_profile};
use super::widgets::{
    Act, Btn, Cell, Item, LabUi, Live, MARK_NAMES, PANEL_BG, PageList, Tone, ViewState, VizFlag,
    clip, spawn_items,
};
use super::{
    Lab, LabSet, PanelState, PanelTab, Phase, Stage, VizSettings, inspection, lab_active, storage,
};
use crate::Sim;
use crate::ui::UiLaunch;

/// The built-in presets as `(name, description)`, built once per process
/// (building them applies their overrides, which is not free).
pub(super) fn builtin_profiles() -> &'static [(String, String)] {
    static BUILT: OnceLock<Vec<(String, String)>> = OnceLock::new();
    BUILT.get_or_init(|| {
        Profile::builtin()
            .into_iter()
            .map(|p| (p.name, p.description))
            .collect()
    })
}

/// Frames between two looks at the profile directory.
const PROFILE_REFRESH_FRAMES: u32 = 30;

/// The names of the profiles a session can load, for the launcher and the
/// Profiles tab: the built-in presets, then the user's saved ones. Read
/// through `storage::profile_names` (which looks at the profile directory),
/// at most twice a second.
#[derive(Resource, Debug, Default)]
pub(super) struct ProfileList {
    names: Vec<String>,
    age: u32,
    filled: bool,
}

impl ProfileList {
    /// The names, built-in presets first.
    pub(super) fn names(&self) -> &[String] {
        &self.names
    }

    /// Brings the list up to date when it is due.
    pub(super) fn refresh(&mut self, lab: &Lab) {
        if self.filled && self.age < PROFILE_REFRESH_FRAMES {
            self.age += 1;
            return;
        }
        let names = storage::profile_names(lab);
        if names != self.names {
            self.names = names;
        }
        self.age = 0;
        self.filled = true;
    }

    /// Looks again on the next refresh (after a profile was saved).
    pub(super) fn expire(&mut self) {
        self.filled = false;
    }
}

/// Everything the panel shows that changes only in answer to a click.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct PanelData {
    profile: String,
    profile_modified: bool,
    overrides: usize,
    level: String,
    scripted: bool,
    rewind_available: bool,
    frozen: bool,
    speed: f32,
    rules: Rules,
    tab: PanelTab,
    group: usize,
    row: usize,
    freeze_while_open: bool,
    pinned: Vec<String>,
    viz: VizSettings,
    hud: bool,
    teleport_page: usize,
    stage_page: usize,
    profile_page: usize,
    /// Parameter groups with their number of overrides.
    groups: Vec<(String, usize)>,
    /// The rows of the selected group.
    rows: Vec<ParamRow>,
    targets: Vec<TeleportTargetInfo>,
    marks: Vec<String>,
    slots: Vec<SlotView>,
    profiles: Vec<String>,
    stages: Vec<StageEntry>,
    stage: Option<Stage>,
    can_save: bool,
    notice: Option<String>,
    message: Option<String>,
}

/// Gathers the panel's data. Read-only.
#[allow(clippy::too_many_arguments)]
fn gather(
    sim: &Sim,
    lab: &Lab,
    session: &Session,
    panel: &PanelState,
    viz: &VizSettings,
    view: &ViewState,
    launch: &UiLaunch,
    profiles: &[String],
) -> Option<PanelData> {
    let inspection = inspection(sim, lab)?;
    let summary = inspection.summary();
    let names = session.catalog().groups();
    let group = panel.group.min(names.len().saturating_sub(1));
    let groups: Vec<(String, usize)> = names
        .iter()
        .map(|name| {
            let overridden = session
                .overlay()
                .iter()
                .filter(|(key, _)| key.split_once('.').is_some_and(|(head, _)| head == *name))
                .count();
            ((*name).to_owned(), overridden)
        })
        .collect();
    let rows = names
        .get(group)
        .map(|name| inspection.params(name))
        .unwrap_or_default();
    Some(PanelData {
        profile: summary.profile,
        profile_modified: summary.profile_modified,
        overrides: summary.overrides,
        marks: session
            .marks_on(&summary.level)
            .into_iter()
            .map(str::to_owned)
            .collect(),
        level: summary.level,
        scripted: summary.scripted,
        rewind_available: summary.rewind_available,
        frozen: summary.frozen,
        speed: summary.speed,
        rules: summary.rules,
        tab: panel.tab,
        group,
        row: panel.row.min(rows.len().saturating_sub(1)),
        freeze_while_open: panel.freeze_while_open,
        pinned: panel.pinned.clone(),
        viz: *viz,
        hud: view.hud,
        teleport_page: view.teleport_page,
        stage_page: view.stage_page,
        profile_page: view.profile_page,
        groups,
        rows,
        targets: inspection.teleport_targets(),
        slots: inspection.slots(),
        profiles: profiles.to_vec(),
        stages: stages(launch),
        stage: lab.stage.clone(),
        can_save: lab.dirs.is_some(),
        notice: lab.notice.clone(),
        message: view.message.clone(),
    })
}

// Column widths of the Tune tab, pixels.
const KEY_W: f32 = 306.0;
const BIG_STEP_W: f32 = 30.0;
const STEP_W: f32 = 26.0;
const VALUE_W: f32 = 100.0;
const CLASSIC_W: f32 = 96.0;
const UNIT_W: f32 = 136.0;
const EFFECT_W: f32 = 90.0;
/// Width of the label that starts a row of choices.
const LEAD_W: f32 = 110.0;

fn effect_short(effect: Effect) -> (&'static str, Tone) {
    match effect {
        Effect::Live => ("live", Tone::Good),
        Effect::Latched => ("latched", Tone::Good),
        Effect::SpawnOnly => ("spawn only", Tone::Notice),
        Effect::Inert(_) => ("inert", Tone::Warn),
        Effect::Unclassified => ("", Tone::Dim),
    }
}

fn effect_long(effect: Effect) -> String {
    match effect {
        Effect::Live => "live: read every tick, so the next tick runs the new value".to_owned(),
        Effect::Latched => {
            "latched: the pawn runs on a copy of it; a change brings the copy up to date at once"
                .to_owned()
        }
        Effect::SpawnOnly => "spawn only: read when the gun and the boots spawn; it applies to \
                              sessions started with it, not to the running game"
            .to_owned(),
        Effect::Inert(reason) => format!("inert: {reason}"),
        Effect::Unclassified => {
            "not classified yet: a change may or may not reach the running pawn at once".to_owned()
        }
    }
}

fn overrides_text(count: usize) -> String {
    match count {
        0 => "Classic parameter set, unchanged".to_owned(),
        1 => "1 override".to_owned(),
        n => format!("{n} overrides"),
    }
}

fn header(d: &PanelData, out: &mut Vec<Item>) {
    out.push(Item::Row(vec![
        Cell::Title("SANDBOX (experimental) - NOT the original's behaviour - saves off".to_owned()),
        Cell::Fill,
        Btn::new("close [`]", Act::Close).into(),
    ]));
    let mut tabs: Vec<Cell> = [
        (PanelTab::Tune, "Tune"),
        (PanelTab::Rules, "Rules & actions"),
        (PanelTab::Time, "Time & states"),
        (PanelTab::View, "View"),
        (PanelTab::Profiles, "Profiles"),
        (PanelTab::Stage, "Stage"),
    ]
    .into_iter()
    .map(|(tab, label)| Btn::new(label, Act::Tab(tab)).selected(d.tab == tab).into())
    .collect();
    tabs.push(Cell::Fill);
    tabs.push(Cell::label(
        format!(
            "profile {}{} | {}",
            d.profile,
            if d.profile_modified { "*" } else { "" },
            overrides_text(d.overrides)
        ),
        0.0,
        if d.overrides > 0 {
            Tone::Notice
        } else {
            Tone::Dim
        },
    ));
    out.push(Item::Row(tabs));
    out.push(Item::Rule);
}

fn param_row(index: usize, row: &ParamRow, d: &PanelData) -> Item {
    let numeric = matches!(row.kind, ValueKind::Float | ValueKind::Int);
    let nudge = |steps: i32, scale: f64| Act::Nudge {
        key: row.key.clone(),
        steps,
        scale,
    };
    let (down, up) = if numeric { ("-", "+") } else { ("<", ">") };
    let value = format_value(&row.current);
    let (effect, effect_tone) = effect_short(row.effect);
    Item::Row(vec![
        Btn::new(row.key.clone(), Act::Row(index))
            .width(KEY_W)
            .left()
            .selected(index == d.row)
            .into(),
        Btn::new("--", nudge(-1, 10.0))
            .width(BIG_STEP_W)
            .enabled(numeric)
            .into(),
        Btn::new(down, nudge(-1, 1.0)).width(STEP_W).into(),
        Cell::label(
            if row.overridden {
                format!("{value} *")
            } else {
                value
            },
            VALUE_W,
            if row.overridden {
                Tone::Notice
            } else {
                Tone::Normal
            },
        ),
        Btn::new(up, nudge(1, 1.0)).width(STEP_W).into(),
        Btn::new("++", nudge(1, 10.0))
            .width(BIG_STEP_W)
            .enabled(numeric)
            .into(),
        Cell::label(
            if row.overridden {
                format_value(&row.classic)
            } else {
                "=".to_owned()
            },
            CLASSIC_W,
            Tone::Dim,
        ),
        Cell::label(row.unit, UNIT_W, Tone::Dim),
        Cell::label(effect, EFFECT_W, effect_tone),
        Btn::new(
            "reset",
            Act::Do(Command::ResetParam {
                key: row.key.clone(),
            }),
        )
        .width(50.0)
        .enabled(row.overridden)
        .into(),
        Btn::new("pin", Act::Pin(row.key.clone()))
            .width(40.0)
            .selected(d.pinned.contains(&row.key))
            .into(),
    ])
}

fn tune(d: &PanelData, out: &mut Vec<Item>) {
    let mut groups = vec![Cell::label("group", 44.0, Tone::Dim)];
    for (index, (name, overridden)) in d.groups.iter().enumerate() {
        let label = if *overridden > 0 {
            format!("{name} ({overridden})")
        } else {
            name.clone()
        };
        groups.push(
            Btn::new(label, Act::Group(index))
                .selected(index == d.group)
                .into(),
        );
    }
    groups.push(Cell::Fill);
    let reset_group: Vec<Command> = d
        .rows
        .iter()
        .filter(|row| row.overridden)
        .map(|row| Command::ResetParam {
            key: row.key.clone(),
        })
        .collect();
    groups.push(
        Btn::new("reset group", Act::DoAll(reset_group.clone()))
            .enabled(!reset_group.is_empty())
            .into(),
    );
    groups.push(
        Btn::new("reset all", Act::Do(Command::ResetAllParams))
            .enabled(d.overrides > 0)
            .into(),
    );
    out.push(Item::Row(groups));
    let gap = BIG_STEP_W + STEP_W + 5.0;
    out.push(Item::Row(vec![
        Cell::label("parameter", KEY_W, Tone::Dim),
        Cell::label("", gap, Tone::Dim),
        Cell::label("value", VALUE_W, Tone::Dim),
        Cell::label("", gap, Tone::Dim),
        Cell::label("classic", CLASSIC_W, Tone::Dim),
        Cell::label("unit", UNIT_W, Tone::Dim),
        Cell::label("effect", EFFECT_W, Tone::Dim),
    ]));
    for (index, row) in d.rows.iter().enumerate() {
        out.push(param_row(index, row, d));
    }
    out.push(Item::Rule);
    match d.rows.get(d.row) {
        Some(row) => {
            out.push(Item::text(clip(
                &format!("{}: {}", row.key, row.description),
                124,
            )));
            out.push(Item::dim(clip(
                &format!(
                    "Classic {} {} | {}",
                    format_value(&row.classic),
                    row.unit,
                    row.provenance
                ),
                124,
            )));
            out.push(Item::dim(clip(&effect_long(row.effect), 124)));
        }
        None => out.push(Item::dim("No parameter in this group.")),
    }
    out.push(Item::dim(
        "-- and ++ are ten steps; hold Left Alt for x10, Left Ctrl for x0.1. A refused value \
         says why below. Steps are ours.",
    ));
}

fn rule_button(label: &str, rules: Rules, current: Rules) -> Cell {
    Btn::new(label, Act::Do(Command::SetRules { rules }))
        .selected(rules == current)
        .into()
}

fn rules(d: &PanelData, out: &mut Vec<Item>) {
    let r = d.rules;
    out.push(Item::dim(
        "Sticky rules: enforced before every tick, over whatever the level sets. Ours.",
    ));
    let mut grapples = vec![
        Cell::label("grapples", LEAD_W, Tone::Normal),
        rule_button(
            "the level's",
            Rules {
                grapples: GrappleRule::Level,
                ..r
            },
            r,
        ),
    ];
    for n in 0..=3 {
        grapples.push(rule_button(
            &n.to_string(),
            Rules {
                grapples: GrappleRule::Fixed(n),
                ..r
            },
            r,
        ));
    }
    grapples.push(rule_button(
        "unlimited",
        Rules {
            grapples: GrappleRule::Unlimited,
            ..r
        },
        r,
    ));
    out.push(Item::Row(grapples));
    out.push(Item::Row(vec![
        Cell::label("rocket boots", LEAD_W, Tone::Normal),
        rule_button(
            "the level's",
            Rules {
                rocket_boots: Switch::Level,
                ..r
            },
            r,
        ),
        rule_button(
            "on",
            Rules {
                rocket_boots: Switch::On,
                ..r
            },
            r,
        ),
        rule_button(
            "off",
            Rules {
                rocket_boots: Switch::Off,
                ..r
            },
            r,
        ),
    ]));
    out.push(Item::Row(vec![
        Cell::label("auto refill", LEAD_W, Tone::Normal),
        Btn::new(
            if r.auto_refill { "on" } else { "off" },
            Act::Do(Command::SetRules {
                rules: Rules {
                    auto_refill: !r.auto_refill,
                    ..r
                },
            }),
        )
        .selected(r.auto_refill)
        .width(44.0)
        .into(),
        Cell::label(
            "refill the grapples whenever they are used up, without a landing",
            0.0,
            Tone::Dim,
        ),
    ]));
    out.push(Item::Rule);
    out.push(Item::dim(
        "Actions (one-off; the level may change the state again). A rule above pins its ability.",
    ));
    let act = |label: &str, command: Command| -> Cell { Btn::new(label, Act::Do(command)).into() };
    out.push(Item::Row(vec![
        act("story mode [F2]", Command::StoryMode { on: Toggle::Toggle }),
        act("cycle grapples [F3]", Command::CycleGrapples),
        act(
            "rocket boots [F4]",
            Command::RocketBoots { on: Toggle::Toggle },
        ),
        act("attractor pads [F6]", Command::ActivateAttractors),
        act("refill grapples", Command::RefillGrapples),
        act("re-arm boots", Command::ResetBoots),
    ]));
    out.push(Item::Row(vec![
        act("respawn", Command::Respawn),
        act("quick load [F7/R]", Command::Kill),
        act(
            "teleport to the crosshair [T]",
            Command::Teleport {
                to: TeleportTarget::AimPoint,
            },
        ),
        act("fly placement [N]", Command::Fly { on: Toggle::Toggle }),
    ]));
    out.push(Item::Live(Live::Abilities));
    out.push(Item::Rule);
    out.push(Item::dim(
        "Teleport to (refused while the grapple is attached; G steps through this list):",
    ));
    let (shown, page, pages) = page_of(&d.targets, d.teleport_page);
    for chunk in shown.chunks(5) {
        out.push(Item::Row(
            chunk
                .iter()
                .map(|target| {
                    Btn::new(
                        clip(&target.label, 22),
                        Act::Do(Command::Teleport {
                            to: target.target.clone(),
                        }),
                    )
                    .width(186.0)
                    .into()
                })
                .collect(),
        ));
    }
    out.extend(page_row(PageList::Teleport, page, pages, d.targets.len()));
    let mut marks = vec![Cell::label("set bookmark", LEAD_W, Tone::Normal)];
    for name in MARK_NAMES {
        marks.push(
            Btn::new(
                name,
                Act::Do(Command::SetMark {
                    name: name.to_owned(),
                }),
            )
            .selected(d.marks.iter().any(|m| m == name))
            .into(),
        );
    }
    marks.push(Cell::label(
        "here, with the view (B sets \"quick\", V returns to it)",
        0.0,
        Tone::Dim,
    ));
    out.push(Item::Row(marks));
}

fn time(d: &PanelData, out: &mut Vec<Item>) {
    let op = |label: &str, op: TimeOp| -> Btn { Btn::new(label, Act::Do(Command::Time { op })) };
    out.push(Item::Row(vec![
        Cell::label("time", LEAD_W, Tone::Normal),
        op(
            if d.frozen {
                "unfreeze [P]"
            } else {
                "freeze [P]"
            },
            TimeOp::Freeze(Toggle::Toggle),
        )
        .selected(d.frozen)
        .into(),
        op("step 1 [O]", TimeOp::Step { n: 1 }).into(),
        op("step 10", TimeOp::Step { n: 10 }).into(),
        op("step 60", TimeOp::Step { n: 60 }).into(),
        Cell::label(
            "a step runs one tick with the keys held now",
            0.0,
            Tone::Dim,
        ),
    ]));
    let mut speeds = vec![Cell::label("speed", LEAD_W, Tone::Normal)];
    for step in SPEED_STEPS {
        speeds.push(
            op(&speed_factor(step), TimeOp::Scale { value: step })
                .selected(step == d.speed)
                .width(52.0)
                .into(),
        );
    }
    speeds.push(Cell::label(
        "[,] slower  [.] faster  [/] 1x; the tick itself is never changed",
        0.0,
        Tone::Dim,
    ));
    out.push(Item::Row(speeds));
    if d.freeze_while_open {
        out.push(Item::dim(
            "The open inspector holds time (switch at the bottom). Freeze and speed above are \
             the session's own and show when it closes; steps run now.",
        ));
    }
    out.push(Item::Rule);
    out.push(Item::dim(
        "Save states: in memory, gone when the session ends. [1-4] select, [K] save, [L] load.",
    ));
    if d.scripted {
        out.push(Item::notice(
            "This level runs a level script: a save state restores the simulation only (sounds, \
             effects and the script-driven interface are not part of it).",
        ));
    }
    for slot in &d.slots {
        let state = match (slot.tick, &slot.label) {
            (Some(tick), Some(label)) => format!("tick {tick} - {}", clip(label, 40)),
            (Some(tick), None) => format!("tick {tick}"),
            (None, _) => "empty".to_owned(),
        };
        let slot_op = |label: &str, op: SlotOp| Btn::new(label, Act::Do(Command::Slot { op }));
        out.push(Item::Row(vec![
            slot_op(
                &format!("slot {}", slot.slot + 1),
                SlotOp::Select { slot: slot.slot },
            )
            .selected(slot.selected)
            .width(LEAD_W)
            .into(),
            slot_op("save", SlotOp::Save { slot: slot.slot }).into(),
            slot_op("load", SlotOp::Load { slot: slot.slot })
                .enabled(slot.loadable)
                .into(),
            slot_op("clear", SlotOp::Clear { slot: slot.slot })
                .enabled(slot.tick.is_some())
                .into(),
            Cell::label(
                state,
                0.0,
                if slot.tick.is_some() {
                    Tone::Normal
                } else {
                    Tone::Dim
                },
            ),
        ]));
    }
    out.push(Item::Rule);
    out.push(Item::Row(vec![
        Cell::label("rewind", LEAD_W, Tone::Normal),
        Btn::new("back one keyframe [Z]", Act::Do(Command::Rewind))
            .enabled(d.rewind_available)
            .into(),
        Cell::Live(Live::Rewind),
    ]));
    out.push(Item::Row(vec![
        Cell::label("recording", LEAD_W, Tone::Normal),
        Btn::new(
            "start / stop [F9]",
            Act::Do(Command::Record { on: Toggle::Toggle }),
        )
        .into(),
        Cell::Live(Live::Recording),
    ]));
    out.push(Item::dim(
        "A Sandbox recording is its own file kind, tagged as not a parity run; the parity tools \
         refuse it.",
    ));
}

fn switch(on: bool, act: Act) -> Cell {
    Btn::new(if on { "on" } else { "off" }, act)
        .selected(on)
        .width(44.0)
        .into()
}

fn view(d: &PanelData, out: &mut Vec<Item>) {
    out.push(Item::dim(
        "Visualisers: drawn by the Sandbox over the game; they change nothing.",
    ));
    for flag in VizFlag::ALL {
        let (name, description) = flag.describe();
        out.push(Item::Row(vec![
            switch(flag.get(&d.viz), Act::Viz(flag)),
            Cell::label(name, 170.0, Tone::Normal),
            Cell::label(description, 0.0, Tone::Dim),
        ]));
    }
    out.push(Item::Row(vec![
        switch(d.hud, Act::Hud),
        Cell::label("HUD read-outs", 170.0, Tone::Normal),
        Cell::label(
            "the lines under the watermark [H]; the watermark itself stays",
            0.0,
            Tone::Dim,
        ),
    ]));
    out.push(Item::Rule);
    out.push(Item::dim(
        "Quick tuner, usable in play ([ ] select, - = step, Backspace reset). Click to unpin; \
         pin more on the Tune tab.",
    ));
    for chunk in d.pinned.chunks(3) {
        out.push(Item::Row(
            chunk
                .iter()
                .map(|key| {
                    Btn::new(key.clone(), Act::Pin(key.clone()))
                        .width(KEY_W)
                        .left()
                        .into()
                })
                .collect(),
        ));
    }
    if d.pinned.is_empty() {
        out.push(Item::dim("(nothing pinned)"));
    }
    out.push(Item::Rule);
    out.push(Item::Live(Live::Player));
    out.push(Item::Live(Live::Latched));
    out.push(Item::Live(Live::Aim));
    out.push(Item::Live(Live::Telemetry));
}

/// The first of `custom-1`, `custom-2`, ... that no profile uses yet.
fn free_profile_name(taken: &[String]) -> String {
    (1..=taken.len() + 1)
        .map(|n| format!("custom-{n}"))
        .find(|name| !taken.contains(name))
        .unwrap_or_else(|| "custom-1".to_owned())
}

fn profiles(d: &PanelData, out: &mut Vec<Item>) {
    out.push(Item::dim(
        "A profile is a set of overrides, rules and a time scale on top of the Classic set. The \
         built-in presets are ours, not modes of the original.",
    ));
    out.push(Item::text(format!(
        "in use: {}{} ({})",
        d.profile,
        if d.profile_modified {
            "*, changed since it was loaded"
        } else {
            ""
        },
        overrides_text(d.overrides)
    )));
    let builtin = builtin_profiles();
    let (shown, page, pages) = page_of(&d.profiles, d.profile_page);
    for name in shown {
        let description = builtin
            .iter()
            .find(|(n, _)| n == name)
            .map_or("saved by you", |(_, description)| description.as_str());
        out.push(Item::Row(vec![
            Btn::new("load", Act::LoadProfile(name.clone()))
                .width(50.0)
                .into(),
            Cell::label(
                name.clone(),
                150.0,
                if *name == d.profile {
                    Tone::Notice
                } else {
                    Tone::Normal
                },
            ),
            Cell::label(clip(description, 96), 0.0, Tone::Dim),
        ]));
    }
    out.extend(page_row(PageList::Profile, page, pages, d.profiles.len()));
    out.push(Item::Rule);
    if d.can_save {
        let mut row: Vec<Cell> = vec![Cell::label("save the tuning", 0.0, Tone::Normal)];
        let fresh = free_profile_name(&d.profiles);
        row.push(Btn::new(format!("as {fresh}"), Act::SaveProfile(fresh)).into());
        // Over the profile in use, when it is one of the user's own.
        let own = d.profiles.contains(&d.profile) && !builtin.iter().any(|(n, _)| *n == d.profile);
        if own {
            row.push(
                Btn::new(
                    format!("over {}", d.profile),
                    Act::SaveProfile(d.profile.clone()),
                )
                .into(),
            );
        }
        out.push(Item::Row(row));
        out.push(Item::dim(
            "Profiles are saved in the Sandbox's own directory, never among the saves.",
        ));
    } else {
        out.push(Item::dim(
            "No user data directory: profiles cannot be saved in this run.",
        ));
    }
}

fn stage_title(stage: Option<&Stage>) -> String {
    match stage {
        Some(Stage::Graybox) => "graybox test level".to_owned(),
        Some(Stage::Arena(id)) => format!("arena {id}"),
        Some(Stage::Map(name)) => format!("map {name}"),
        None => "(none)".to_owned(),
    }
}

fn stage(d: &PanelData, out: &mut Vec<Item>) {
    out.push(Item::text(format!(
        "on: {} | level: {}",
        stage_title(d.stage.as_ref()),
        clip(&d.level, 80)
    )));
    out.push(Item::Live(Live::World));
    out.push(Item::Rule);
    out.push(Item::notice(format!(
        "Starting a stage ends this session and starts a new one with the profile {}. Tuning \
         that is not saved as a profile is not carried over.",
        d.profile
    )));
    let (shown, page, pages) = page_of(&d.stages, d.stage_page);
    for entry in shown {
        out.push(Item::Row(vec![
            Btn::new(
                clip(&entry.title, 34),
                Act::Start {
                    stage: entry.stage.clone(),
                    profile: start_profile(&d.profile),
                },
            )
            .width(270.0)
            .left()
            .selected(Some(&entry.stage) == d.stage.as_ref())
            .into(),
            Cell::label(clip(&entry.summary, 86), 0.0, Tone::Dim),
        ]));
    }
    out.extend(page_row(PageList::Stage, page, pages, d.stages.len()));
    out.push(Item::Rule);
    out.push(Item::Row(vec![
        Btn::new("end the session", Act::End).into(),
        Cell::label(
            "back to the launcher or the main menu; nothing of it is kept",
            0.0,
            Tone::Dim,
        ),
    ]));
}

fn footer(d: &PanelData, out: &mut Vec<Item>) {
    out.push(Item::Rule);
    match (&d.message, &d.notice) {
        (Some(message), _) => out.push(Item::Text(clip(message, 240), Tone::Warn)),
        (None, Some(notice)) => out.push(Item::notice(clip(&format!("last: {notice}"), 240))),
        (None, None) => out.push(Item::dim("last: -")),
    }
    out.push(Item::Row(vec![
        switch(d.freeze_while_open, Act::FreezeWhileOpen),
        Cell::label("freeze while open", 0.0, Tone::Normal),
        Cell::label("|", 0.0, Tone::Dim),
        Cell::Live(Live::Clock),
    ]));
}

/// The panel as a list of items.
pub(super) fn panel_items(d: &PanelData) -> Vec<Item> {
    let mut out = Vec::new();
    header(d, &mut out);
    match d.tab {
        PanelTab::Tune => tune(d, &mut out),
        PanelTab::Rules => rules(d, &mut out),
        PanelTab::Time => time(d, &mut out),
        PanelTab::View => view(d, &mut out),
        PanelTab::Profiles => profiles(d, &mut out),
        PanelTab::Stage => stage(d, &mut out),
    }
    footer(d, &mut out);
    out
}

/// What the [`Live`] texts are written from.
#[derive(Clone, Debug)]
pub(super) struct LiveData {
    /// The session at a glance.
    pub summary: SessionSummary,
    /// The player.
    pub player: PlayerView,
    /// What the crosshair points at.
    pub aim: Option<AimView>,
    /// The read-outs.
    pub telemetry: TelemetryView,
    /// The level.
    pub world: WorldView,
    /// The open inspector holds the clock ("freeze while open"), whatever
    /// the session's own time model says.
    pub held: bool,
}

/// The current text of a [`Live`] line.
pub(super) fn live_text(which: Live, d: &LiveData) -> String {
    let p = &d.player;
    match which {
        Live::Clock => {
            let s = &d.summary;
            let mut out = format!(
                "tick {} | time {} at {}",
                s.tick,
                if s.frozen {
                    "FROZEN"
                } else if d.held {
                    "held while the inspector is open, then running"
                } else {
                    "running"
                },
                speed_factor(s.speed)
            );
            if s.pending_steps > 0 {
                out.push_str(&format!(" | {} steps pending", s.pending_steps));
            }
            if s.recording {
                out.push_str(" | REC");
            }
            if s.flying {
                out.push_str(" | FLY placement");
            }
            out
        }
        Live::Player => format!(
            "player: ({:.0}, {:.0}, {:.0}) uu | speed {:.0} uu/s (horizontal {:.0}) | {} | yaw \
             {:.0} pitch {:.0} deg | respawns {}{}",
            p.position.x,
            p.position.y,
            p.position.z,
            p.speed,
            p.horizontal_speed,
            p.physics,
            p.yaw.to_degrees(),
            p.pitch.to_degrees(),
            p.respawns,
            if p.dying { " | death sequence" } else { "" },
        ),
        Live::Latched => format!(
            "run-time values: ground speed {:.0} | air control {:.2} | jump {:.0} | air speed \
             {:.0} | FOV {:.1} | cylinder {:.0}/{:.0}",
            p.ground_speed,
            p.air_control,
            p.jump_z,
            p.air_speed,
            p.fov,
            p.capsule_radius,
            p.capsule_half_height,
        ),
        Live::Abilities => {
            let grapples = if p.unlimited_grapples {
                "unlimited".to_owned()
            } else {
                format!("{} of {} left", p.grapples_left, p.max_grapples)
            };
            format!(
                "now: grapples {grapples}{} | rocket boots {} ({}) | story mode {}",
                if p.grapple_attached {
                    " (attached)"
                } else {
                    ""
                },
                if p.boots_enabled { "on" } else { "off" },
                p.boots_state,
                if p.story_mode { "on" } else { "off" },
            )
        }
        Live::Aim => match &d.aim {
            Some(aim) if aim.hit && aim.acceptable => format!(
                "aim: can be grappled, {:.0} uu away (reach {:.0})",
                aim.distance, aim.max_distance
            ),
            Some(aim) if aim.hit => format!(
                "aim: hit at {:.0} uu, no grapple from here (reach {:.0})",
                aim.distance, aim.max_distance
            ),
            Some(aim) => format!("aim: nothing hit (reach {:.0} uu)", aim.max_distance),
            None => "aim: no grapple gun".to_owned(),
        },
        Live::Telemetry => format!(
            "peak {:.0} uu/s | {} | {}",
            d.telemetry.peak_speed,
            jump_text(d.telemetry.last_jump),
            swing_text(d.telemetry.last_swing),
        ),
        Live::Rewind => {
            let s = &d.summary;
            if s.rewind_available {
                format!(
                    "{} keyframes, {:.0} s back (hand-made levels; not while recording)",
                    s.rewind_keyframes, s.rewind_seconds
                )
            } else {
                "hand-made levels only".to_owned()
            }
        }
        Live::Recording => {
            let state = if d.summary.recording {
                "recording"
            } else {
                "not recording"
            };
            state.to_owned()
        }
        Live::World => {
            let w = &d.world;
            let mut out = format!(
                "{} | checkpoints {}{}",
                if w.hand_made {
                    "hand-made by us"
                } else {
                    "converted map"
                },
                w.checkpoints,
                w.active_checkpoint
                    .map(|id| format!(" (active {id})"))
                    .unwrap_or_default(),
            );
            for (label, value) in &w.details {
                out.push_str(&format!(" | {label}: {value}"));
            }
            clip(&out, 480)
        }
    }
}

/// Root of the panel's UI tree.
#[derive(Component)]
struct PanelRoot;

/// Shows the panel while the inspector is open, rebuilt when its data
/// changes, and removes it afterwards.
#[allow(clippy::too_many_arguments)]
fn sync(
    mut commands: Commands,
    lab: Res<Lab>,
    sim: Option<Res<Sim>>,
    panel: Res<PanelState>,
    viz: Res<VizSettings>,
    view: Res<ViewState>,
    launch: Res<UiLaunch>,
    mut profiles: ResMut<ProfileList>,
    mut shown: Local<Option<PanelData>>,
    roots: Query<Entity, With<PanelRoot>>,
) {
    let data = (|| {
        if lab.phase != Phase::Active || !panel.open {
            return None;
        }
        let session = lab.session.as_ref()?;
        let sim = sim.as_deref()?;
        if panel.tab == PanelTab::Profiles {
            profiles.refresh(&lab);
        }
        gather(
            sim,
            &lab,
            session,
            &panel,
            &viz,
            &view,
            &launch,
            profiles.names(),
        )
    })();
    let Some(data) = data else {
        for root in &roots {
            commands.entity(root).despawn();
        }
        *shown = None;
        return;
    };
    if shown.as_ref() == Some(&data) && !roots.is_empty() {
        return;
    }
    // A profile may just have been saved: look at the directory again.
    if shown.as_ref().is_some_and(|old| old.notice != data.notice) {
        profiles.expire();
    }
    for root in &roots {
        commands.entity(root).despawn();
    }
    let items = panel_items(&data);
    commands
        .spawn((
            PanelRoot,
            LabUi,
            Node {
                position_type: PositionType::Absolute,
                width: Val::Percent(100.0),
                height: Val::Percent(100.0),
                padding: UiRect::top(Val::Px(4.0)),
                align_items: AlignItems::FlexStart,
                justify_content: JustifyContent::Center,
                ..default()
            },
            // Under the Classic menu (100), over the notices.
            GlobalZIndex(95),
        ))
        .with_children(|root| {
            root.spawn((
                Node {
                    flex_direction: FlexDirection::Column,
                    row_gap: Val::Px(2.0),
                    padding: UiRect::axes(Val::Px(10.0), Val::Px(7.0)),
                    // The window's width: nothing of the read-out it covers
                    // shows beside it.
                    width: Val::Percent(99.0),
                    max_width: Val::Px(1500.0),
                    border_radius: BorderRadius::all(Val::Px(8.0)),
                    ..default()
                },
                BackgroundColor(PANEL_BG),
            ))
            .with_children(|panel| spawn_items(panel, &items));
        });
    *shown = Some(data);
}

/// Writes the [`Live`] texts.
fn update_live(
    lab: Res<Lab>,
    sim: Option<Res<Sim>>,
    panel: Res<PanelState>,
    mut texts: Query<(&Live, &mut Text)>,
) {
    let Some(sim) = sim.as_deref() else {
        return;
    };
    let Some(inspection) = inspection(sim, &lab) else {
        return;
    };
    let data = LiveData {
        summary: inspection.summary(),
        player: inspection.player(),
        aim: inspection.aim(),
        telemetry: inspection.telemetry(),
        world: inspection.world(),
        held: panel.open && panel.freeze_while_open,
    };
    for (which, mut text) in &mut texts {
        let line = super::widgets::ascii(&live_text(*which, &data));
        if text.0 != line {
            text.0 = line;
        }
    }
}

pub(super) fn build(app: &mut App) {
    app.init_resource::<ProfileList>().add_systems(
        Update,
        (
            sync.run_if(lab_active.or_else(any_with_component::<PanelRoot>)),
            update_live.run_if(lab_active.and_then(any_with_component::<Live>)),
        )
            .chain()
            .in_set(LabSet::Draw),
    );
}

#[cfg(test)]
mod tests {
    use asamu_game::Game;
    use asamu_sandbox::inspect::Inspection;
    use asamu_sandbox::keys::TuneValue;
    use asamu_sandbox::session::SimCx;

    use super::super::widgets::{buttons, texts};
    use super::*;

    /// A lab with a running classic session on the stock graybox.
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

    fn data_for(lab: &Lab, sim: &Sim, panel: &PanelState) -> PanelData {
        let names: Vec<String> = builtin_profiles().iter().map(|(n, _)| n.clone()).collect();
        gather(
            sim,
            lab,
            lab.session.as_ref().unwrap(),
            panel,
            &VizSettings::default(),
            &ViewState::default(),
            &UiLaunch::default(),
            &names,
        )
        .unwrap()
    }

    fn tab(tab: PanelTab) -> PanelState {
        PanelState {
            tab,
            open: true,
            ..PanelState::default()
        }
    }

    const TABS: [PanelTab; 6] = [
        PanelTab::Tune,
        PanelTab::Rules,
        PanelTab::Time,
        PanelTab::View,
        PanelTab::Profiles,
        PanelTab::Stage,
    ];

    #[test]
    fn every_tab_carries_the_watermark_a_way_out_and_ascii_text() {
        let (lab, sim) = lab_and_sim();
        for which in TABS {
            let items = panel_items(&data_for(&lab, &sim, &tab(which)));
            let all = texts(&items).join("\n");
            assert!(all.contains("SANDBOX"), "{which:?}");
            assert!(all.contains("NOT the original's behaviour"), "{which:?}");
            assert!(all.contains("saves off"), "{which:?}");
            assert!(all.is_ascii(), "{which:?}: {all}");
            let all_buttons = buttons(&items);
            assert!(all_buttons.iter().any(|b| b.act == Act::Close), "{which:?}");
            // Every tab can be reached from every tab.
            for other in TABS {
                assert!(
                    all_buttons
                        .iter()
                        .any(|b| b.act == Act::Tab(other) && b.selected == (other == which)),
                    "{which:?} -> {other:?}"
                );
            }
            assert!(
                all_buttons.iter().any(|b| b.act == Act::FreezeWhileOpen),
                "{which:?}"
            );
        }
    }

    #[test]
    fn the_tune_tab_lists_every_key_of_the_group_with_steppers() {
        let (lab, sim) = lab_and_sim();
        let session = lab.session.as_ref().unwrap();
        let groups = session.catalog().groups();
        let mut seen = 0;
        for (index, group) in groups.iter().enumerate() {
            let panel = PanelState {
                group: index,
                ..tab(PanelTab::Tune)
            };
            let data = data_for(&lab, &sim, &panel);
            let items = panel_items(&data);
            let all_buttons = buttons(&items);
            let keys: Vec<&str> = session
                .catalog()
                .iter()
                .filter(|info| info.key.starts_with(&format!("{group}.")))
                .map(|info| info.key.as_str())
                .collect();
            assert!(!keys.is_empty(), "{group}");
            assert!(keys.len() <= 24, "{group}: the page would not fit");
            for key in &keys {
                for steps in [-1, 1] {
                    assert!(
                        all_buttons.iter().any(|b| b.act
                            == Act::Nudge {
                                key: (*key).to_owned(),
                                steps,
                                scale: 1.0
                            }
                            && b.enabled),
                        "{key} {steps}"
                    );
                }
                assert!(
                    all_buttons
                        .iter()
                        .any(|b| b.act == Act::Pin((*key).to_owned())),
                    "{key}"
                );
                // Nothing is overridden: no reset is offered.
                assert!(all_buttons.iter().any(|b| b.act
                    == Act::Do(Command::ResetParam {
                        key: (*key).to_owned()
                    })
                    && !b.enabled));
            }
            seen += keys.len();
        }
        assert_eq!(seen, session.catalog().iter().count());
    }

    #[test]
    fn an_override_is_marked_and_can_be_reset() {
        let (mut lab, mut sim) = lab_and_sim();
        let key = "movement.custom_gravity_scaling";
        {
            let Sim { game, script, .. } = &mut sim;
            let session = lab.session.as_mut().unwrap();
            session
                .execute(
                    Command::SetParam {
                        key: key.to_owned(),
                        value: TuneValue::Float(0.5),
                    },
                    &mut SimCx { game, script },
                )
                .unwrap();
        }
        let data = data_for(&lab, &sim, &tab(PanelTab::Tune));
        assert_eq!(data.overrides, 1);
        assert_eq!(data.groups[0], ("movement".to_owned(), 1));
        let items = panel_items(&data);
        let all = texts(&items);
        assert!(all.contains(&"0.5 *"), "{all:?}");
        assert!(all.iter().any(|t| t.contains("1 override")));
        let all_buttons = buttons(&items);
        let reset = Act::Do(Command::ResetParam {
            key: key.to_owned(),
        });
        assert!(all_buttons.iter().any(|b| b.act == reset && b.enabled));
        assert!(all_buttons.iter().any(|b| b.act
            == Act::DoAll(vec![Command::ResetParam {
                key: key.to_owned()
            }])
            && b.enabled));
        assert!(
            all_buttons
                .iter()
                .any(|b| b.act == Act::Do(Command::ResetAllParams) && b.enabled)
        );
    }

    #[test]
    fn rules_and_time_buttons_show_the_state_they_would_set() {
        let (lab, sim) = lab_and_sim();
        let items = panel_items(&data_for(&lab, &sim, &tab(PanelTab::Rules)));
        let all_buttons = buttons(&items);
        // The default rules are the selected choices.
        let selected_rules: Vec<&Act> = all_buttons
            .iter()
            .filter(|b| b.selected && matches!(b.act, Act::Do(Command::SetRules { .. })))
            .map(|b| &b.act)
            .collect();
        assert_eq!(selected_rules.len(), 2, "{selected_rules:?}");
        assert!(selected_rules.iter().all(|a| **a
            == Act::Do(Command::SetRules {
                rules: Rules::default()
            })));
        assert!(all_buttons.iter().any(|b| b.act
            == Act::Do(Command::SetRules {
                rules: Rules {
                    grapples: GrappleRule::Unlimited,
                    ..Rules::default()
                }
            })));
        // The teleport list starts with the level's start.
        assert!(all_buttons.iter().any(|b| b.act
            == Act::Do(Command::Teleport {
                to: TeleportTarget::Start
            })));

        let items = panel_items(&data_for(&lab, &sim, &tab(PanelTab::Time)));
        let all_buttons = buttons(&items);
        let speeds: Vec<&&Btn> = all_buttons
            .iter()
            .filter(|b| {
                matches!(
                    b.act,
                    Act::Do(Command::Time {
                        op: TimeOp::Scale { .. }
                    })
                )
            })
            .collect();
        assert_eq!(speeds.len(), SPEED_STEPS.len());
        assert_eq!(speeds.iter().filter(|b| b.selected).count(), 1);
        assert!(speeds.iter().any(|b| b.selected && b.label == "1x"));
        // Four slots; an empty one cannot be loaded or cleared.
        for slot in 0..4 {
            assert!(all_buttons.iter().any(|b| b.act
                == Act::Do(Command::Slot {
                    op: SlotOp::Save { slot }
                })
                && b.enabled));
            assert!(all_buttons.iter().any(|b| b.act
                == Act::Do(Command::Slot {
                    op: SlotOp::Load { slot }
                })
                && !b.enabled));
        }
        // Hand-made level: the rewind is offered.
        assert!(
            all_buttons
                .iter()
                .any(|b| b.act == Act::Do(Command::Rewind) && b.enabled)
        );
    }

    #[test]
    fn profiles_and_stages_are_listed_with_what_they_do() {
        let (mut lab, sim) = lab_and_sim();
        let items = panel_items(&data_for(&lab, &sim, &tab(PanelTab::Profiles)));
        let all_buttons = buttons(&items);
        for (name, _) in builtin_profiles() {
            assert!(
                all_buttons
                    .iter()
                    .any(|b| b.act == Act::LoadProfile(name.clone())),
                "{name}"
            );
        }
        // No directory: nothing can be saved, and the tab says so.
        assert!(
            !all_buttons
                .iter()
                .any(|b| matches!(b.act, Act::SaveProfile(_)))
        );
        lab.dirs = Some(asamu_sandbox::profile::SandboxDirs {
            root: std::env::temp_dir().join("asamu-sandbox-panel-test-not-created"),
        });
        let items = panel_items(&data_for(&lab, &sim, &tab(PanelTab::Profiles)));
        assert!(
            buttons(&items)
                .iter()
                .any(|b| b.act == Act::SaveProfile("custom-1".to_owned()))
        );

        let items = panel_items(&data_for(&lab, &sim, &tab(PanelTab::Stage)));
        let all_buttons = buttons(&items);
        assert!(all_buttons.iter().any(|b| b.act
            == Act::Start {
                stage: Stage::Arena("grapple-lab".to_owned()),
                profile: None
            }));
        assert!(all_buttons.iter().any(|b| b.act == Act::End));
        // The running stage is the marked one.
        assert!(all_buttons.iter().any(|b| b.selected
            && b.act
                == Act::Start {
                    stage: Stage::Graybox,
                    profile: None
                }));
    }

    #[test]
    fn free_profile_names_skip_the_taken_ones() {
        assert_eq!(free_profile_name(&[]), "custom-1");
        let taken = ["custom-1".to_owned(), "custom-3".to_owned()];
        assert_eq!(free_profile_name(&taken), "custom-2");
        let taken = ["custom-1".to_owned(), "custom-2".to_owned()];
        assert_eq!(free_profile_name(&taken), "custom-3");
    }

    #[test]
    fn the_static_data_does_not_move_with_the_simulation() {
        // The tree is rebuilt when the data changes; a tick must not change
        // it, or a click could fall between a press and its release.
        let (lab, mut sim) = lab_and_sim();
        for which in TABS {
            let before = data_for(&lab, &sim, &tab(which));
            for _ in 0..30 {
                let _ = sim.game.tick(&asamu_player::InputFrame {
                    move_forward: 1.0,
                    ..Default::default()
                });
            }
            assert_eq!(before, data_for(&lab, &sim, &tab(which)), "{which:?}");
        }
    }

    #[test]
    fn live_lines_are_ascii_and_follow_the_simulation() {
        let (lab, mut sim) = lab_and_sim();
        let read = |sim: &Sim| {
            let inspection: Inspection<'_> = inspection(sim, &lab).unwrap();
            LiveData {
                summary: inspection.summary(),
                player: inspection.player(),
                aim: inspection.aim(),
                telemetry: inspection.telemetry(),
                world: inspection.world(),
                held: false,
            }
        };
        let all = [
            Live::Clock,
            Live::Player,
            Live::Latched,
            Live::Abilities,
            Live::Aim,
            Live::Telemetry,
            Live::Rewind,
            Live::Recording,
            Live::World,
        ];
        let before = read(&sim);
        for which in all {
            let line = live_text(which, &before);
            assert!(!line.is_empty() && line.is_ascii(), "{which:?}: {line}");
        }
        assert!(live_text(Live::Clock, &before).starts_with("tick 0 | time running at 1x"));
        let held = LiveData {
            held: true,
            ..before.clone()
        };
        assert!(live_text(Live::Clock, &held).contains("held while the inspector is open"));
        assert_eq!(live_text(Live::Recording, &before), "not recording");
        for _ in 0..10 {
            let _ = sim.game.tick(&asamu_player::InputFrame::default());
        }
        assert!(live_text(Live::Clock, &read(&sim)).starts_with("tick 10 "));
    }

    #[test]
    fn the_panel_appears_and_goes_with_the_inspector_and_the_session() {
        let (lab, sim) = lab_and_sim();
        let mut app = App::new();
        app.insert_resource(lab)
            .insert_resource(sim)
            .init_resource::<PanelState>()
            .init_resource::<VizSettings>()
            .init_resource::<ViewState>()
            .init_resource::<UiLaunch>()
            .init_resource::<ProfileList>()
            .add_systems(Update, (sync, update_live).chain());
        let count = |app: &mut App| {
            let world = app.world_mut();
            (
                world
                    .query_filtered::<Entity, With<PanelRoot>>()
                    .iter(world)
                    .count(),
                world.query::<&Live>().iter(world).count(),
            )
        };
        app.update();
        assert_eq!(count(&mut app), (0, 0), "closed: nothing is drawn");
        app.world_mut().resource_mut::<PanelState>().open = true;
        app.update();
        app.update();
        let (roots, live) = count(&mut app);
        assert_eq!(roots, 1);
        assert!(live >= 1);
        // The live lines were written.
        let world = app.world_mut();
        let clock = world
            .query::<(&Live, &Text)>()
            .iter(world)
            .find(|(which, _)| **which == Live::Clock)
            .map(|(_, text)| text.0.clone())
            .unwrap();
        assert!(clock.starts_with("tick 0"), "{clock}");
        // Another tab: one tree, not two.
        app.world_mut().resource_mut::<PanelState>().tab = PanelTab::View;
        app.update();
        app.update();
        assert_eq!(count(&mut app).0, 1);
        // The session ends with the inspector still flagged open.
        app.world_mut().resource_mut::<Lab>().phase = Phase::Idle;
        app.update();
        app.update();
        assert_eq!(count(&mut app), (0, 0));
        let world = app.world_mut();
        assert_eq!(world.query::<&LabUi>().iter(world).count(), 0);
    }
}
