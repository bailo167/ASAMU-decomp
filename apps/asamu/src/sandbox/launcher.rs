//! The stage launcher (`ui::Screen::Sandbox` before a session): the stages
//! this launch offers and the profile to start with.
//!
//! The stage list is launch-scoped, because the graybox and the converted
//! composition are different apps: a graybox launch offers the stock test
//! level and the built-in hand-made arenas, a converted launch offers its
//! converted maps. The launcher only sends `LabRequest::Start` and
//! `LabRequest::End`; `lifecycle` carries them out.

use asamu_sandbox::arena::builtin_arenas;
use bevy::ecs::schedule::common_conditions::any_with_component;
use bevy::prelude::*;

use super::panel::{ProfileList, builtin_profiles};
use super::widgets::{
    Act, BACKDROP, Btn, Cell, Item, LabUi, PAGE, PANEL_BG, PageList, Tone, ViewState, clip,
    spawn_items,
};
use super::{Lab, LabLaunch, LabRequest, LabSet, Phase, Stage, lab_launcher};
use crate::ui::UiLaunch;

/// A stage a session can start on.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct StageEntry {
    /// What to play on.
    pub stage: Stage,
    /// Short title.
    pub title: String,
    /// One line on what it is.
    pub summary: String,
}

/// The stages this launch offers.
pub(super) fn stages(launch: &UiLaunch) -> Vec<StageEntry> {
    if launch.converted.is_some() {
        return launch
            .levels
            .iter()
            .map(|name| StageEntry {
                stage: Stage::Map(name.clone()),
                title: name.clone(),
                summary: "converted map (local data from your own install)".to_owned(),
            })
            .collect();
    }
    let mut out = vec![StageEntry {
        stage: Stage::Graybox,
        title: "Graybox test level".to_owned(),
        summary: "the stock hand-made test level".to_owned(),
    }];
    out.extend(builtin_arenas().iter().map(|arena| StageEntry {
        stage: Stage::Arena(arena.id.to_owned()),
        title: format!("{} ({})", arena.title, arena.id),
        summary: arena.summary.to_owned(),
    }));
    out
}

/// The profile a `Start` request names: `None` stands for Classic.
pub(super) fn start_profile(name: &str) -> Option<String> {
    (name != "classic").then(|| name.to_owned())
}

/// `PAGE` entries of `list` from page `page` (clamped to the last page),
/// with the clamped page and the page count.
pub(super) fn page_of<T>(list: &[T], page: usize) -> (&[T], usize, usize) {
    let pages = list.len().div_ceil(PAGE).max(1);
    let page = page.min(pages - 1);
    let start = page * PAGE;
    let end = (start + PAGE).min(list.len());
    (list.get(start..end).unwrap_or(&[]), page, pages)
}

/// The "previous / next page" row of a list whose page `page` (as
/// [`page_of`] clamped it) is shown; nothing for a single page.
pub(super) fn page_row(list: PageList, page: usize, pages: usize, total: usize) -> Option<Item> {
    (pages > 1).then(|| {
        Item::Row(vec![
            Btn::new("< previous", Act::Page(list, page.saturating_sub(1)))
                .enabled(page > 0)
                .into(),
            Cell::label(
                format!("page {} of {pages} ({total} entries)", page + 1),
                0.0,
                Tone::Dim,
            ),
            Btn::new("next >", Act::Page(list, page + 1))
                .enabled(page + 1 < pages)
                .into(),
        ])
    })
}

/// What the launcher shows.
#[derive(Clone, Debug, PartialEq)]
struct LauncherData {
    stages: Vec<StageEntry>,
    page: usize,
    profiles: Vec<String>,
    selected: usize,
    converted: bool,
    from_cli: bool,
    notice: Option<String>,
}

/// The description of the profile `name`, when it is a built-in preset.
fn describe_profile(name: &str) -> String {
    builtin_profiles()
        .iter()
        .find(|(n, _)| n == name)
        .map_or_else(
            || "a profile you saved".to_owned(),
            |(_, description)| format!("built-in preset: {description}"),
        )
}

fn launcher_items(d: &LauncherData) -> Vec<Item> {
    let mut out = vec![
        Item::Title("Sandbox (experimental)".to_owned()),
        Item::text(
            "A lab on top of the recreation: live tuning, time control, save states, read-outs \
             and hand-made arenas.",
        ),
        Item::notice(
            "Not the original's behaviour and never evidence of parity. Saves, progress and \
             time-trial records are not touched: saves stay in memory.",
        ),
        Item::Rule,
    ];
    let profile = d.profiles.get(d.selected).map_or("classic", String::as_str);
    out.push(Item::Row(vec![
        Cell::label("profile", 64.0, Tone::Dim),
        Btn::new("<", Act::LauncherProfile(-1))
            .width(26.0)
            .enabled(d.selected > 0)
            .into(),
        Cell::label(
            profile,
            180.0,
            if profile == "classic" {
                Tone::Normal
            } else {
                Tone::Notice
            },
        ),
        Btn::new(">", Act::LauncherProfile(1))
            .width(26.0)
            .enabled(d.selected + 1 < d.profiles.len())
            .into(),
        Cell::label(
            format!("{} of {}", d.selected + 1, d.profiles.len().max(1)),
            0.0,
            Tone::Dim,
        ),
    ]));
    out.push(Item::dim(clip(&describe_profile(profile), 200)));
    out.push(Item::Rule);
    out.push(Item::dim(if d.converted {
        "Stage: a converted map of this launch (it loads as a story level; saves stay in memory)."
    } else {
        "Stage: hand-made levels, written by us (no original content)."
    }));
    let (shown, page, pages) = page_of(&d.stages, d.page);
    for entry in shown {
        out.push(Item::Row(vec![
            Btn::new(
                clip(&entry.title, 34),
                Act::Start {
                    stage: entry.stage.clone(),
                    profile: start_profile(profile),
                },
            )
            .width(270.0)
            .left()
            .into(),
        ]));
        // A hand-made stage says what it holds (a line of its own: it
        // wraps); the converted maps all read the same.
        if !d.converted {
            out.push(Item::dim(clip(&entry.summary, 200)));
        }
    }
    if d.stages.is_empty() {
        out.push(Item::Text(
            "No stage is available in this launch.".to_owned(),
            Tone::Warn,
        ));
    }
    out.extend(page_row(PageList::Stage, page, pages, d.stages.len()));
    out.push(Item::Rule);
    if d.from_cli {
        out.push(Item::dim(
            "This process was started with --sandbox: Classic is a separate launch.",
        ));
        out.push(Item::Row(vec![Btn::new("Quit", Act::Quit).into()]));
    } else {
        out.push(Item::Row(vec![
            Btn::new("Back to the main menu [Esc]", Act::End).into(),
        ]));
    }
    if let Some(notice) = &d.notice {
        out.push(Item::notice(clip(notice, 300)));
    }
    out
}

/// Root of the launcher's UI tree.
#[derive(Component)]
struct LauncherRoot;

/// Shows the launcher while `Phase::Launcher`, rebuilt when what it shows
/// changes, and removes it afterwards.
#[allow(clippy::too_many_arguments)]
fn sync(
    mut commands: Commands,
    lab: Res<Lab>,
    launch: Res<UiLaunch>,
    cli: Res<LabLaunch>,
    mut view: ResMut<ViewState>,
    mut profiles: ResMut<ProfileList>,
    mut shown: Local<Option<LauncherData>>,
    roots: Query<Entity, With<LauncherRoot>>,
) {
    if lab.phase != Phase::Launcher {
        for root in &roots {
            commands.entity(root).despawn();
        }
        *shown = None;
        return;
    }
    profiles.refresh(&lab);
    let names = profiles.names();
    // The first selection: `--sandbox-profile`, else Classic.
    let selected = match view.launcher_profile {
        Some(index) => index.min(names.len().saturating_sub(1)),
        None => {
            let wanted = cli.args.profile.as_deref().unwrap_or("classic");
            names.iter().position(|n| n == wanted).unwrap_or(0)
        }
    };
    if view.launcher_profile != Some(selected) {
        view.launcher_profile = Some(selected);
    }
    let all = stages(&launch);
    let (_, page, _) = page_of(&all, view.stage_page);
    if view.stage_page != page {
        view.stage_page = page;
    }
    let data = LauncherData {
        stages: all,
        page,
        profiles: names.to_vec(),
        selected,
        converted: launch.converted.is_some(),
        from_cli: lab.from_cli,
        notice: view.message.clone().or_else(|| lab.notice.clone()),
    };
    if shown.as_ref() == Some(&data) && !roots.is_empty() {
        return;
    }
    for root in &roots {
        commands.entity(root).despawn();
    }
    let items = launcher_items(&data);
    commands
        .spawn((
            LauncherRoot,
            LabUi,
            Node {
                position_type: PositionType::Absolute,
                width: Val::Percent(100.0),
                height: Val::Percent(100.0),
                align_items: AlignItems::Center,
                justify_content: JustifyContent::Center,
                ..default()
            },
            BackgroundColor(BACKDROP),
            // Under the Classic menu (100), over the notices.
            GlobalZIndex(96),
        ))
        .with_children(|root| {
            root.spawn((
                Node {
                    flex_direction: FlexDirection::Column,
                    row_gap: Val::Px(6.0),
                    padding: UiRect::all(Val::Px(20.0)),
                    width: Val::Px(900.0),
                    max_width: Val::Percent(96.0),
                    border_radius: BorderRadius::all(Val::Px(10.0)),
                    ..default()
                },
                BackgroundColor(PANEL_BG),
            ))
            .with_children(|panel| spawn_items(panel, &items));
        });
    *shown = Some(data);
}

/// Esc leaves the launcher (in a `--sandbox` process there is nothing to go
/// back to).
fn keys(keys: Res<ButtonInput<KeyCode>>, lab: Res<Lab>, mut requests: MessageWriter<LabRequest>) {
    if keys.just_pressed(KeyCode::Escape) && !lab.from_cli {
        requests.write(LabRequest::End);
    }
}

pub(super) fn build(app: &mut App) {
    app.add_systems(
        Update,
        (
            keys.run_if(lab_launcher),
            sync.run_if(lab_launcher.or_else(any_with_component::<LauncherRoot>)),
        )
            .in_set(LabSet::Draw),
    );
}

#[cfg(test)]
mod tests {
    use asamu_assets::ConvertedDir;

    use super::super::widgets::{buttons, texts};
    use super::*;

    fn data(launch: &UiLaunch, from_cli: bool) -> LauncherData {
        LauncherData {
            stages: stages(launch),
            page: 0,
            profiles: vec!["classic".to_owned(), "moon".to_owned()],
            selected: 0,
            converted: launch.converted.is_some(),
            from_cli,
            notice: None,
        }
    }

    #[test]
    fn a_graybox_launch_offers_the_hand_made_stages() {
        let all = stages(&UiLaunch::default());
        assert_eq!(all[0].stage, Stage::Graybox);
        let arenas: Vec<&Stage> = all[1..].iter().map(|s| &s.stage).collect();
        assert_eq!(
            arenas,
            [
                &Stage::Arena("movement-lab".to_owned()),
                &Stage::Arena("grapple-lab".to_owned())
            ]
        );
        assert_eq!(all.len(), 1 + builtin_arenas().len());
    }

    #[test]
    fn a_converted_launch_offers_its_maps_and_no_arena() {
        let launch = UiLaunch {
            converted: Some(ConvertedDir::new(std::path::PathBuf::from("conv"))),
            levels: vec!["AG-Workshop".to_owned(), "AG-IceCave".to_owned()],
            ..UiLaunch::default()
        };
        let all = stages(&launch);
        assert_eq!(
            all.iter().map(|s| s.stage.clone()).collect::<Vec<_>>(),
            [
                Stage::Map("AG-Workshop".to_owned()),
                Stage::Map("AG-IceCave".to_owned())
            ]
        );
    }

    #[test]
    fn stage_buttons_start_with_the_selected_profile() {
        let launch = UiLaunch::default();
        let mut d = data(&launch, false);
        let items = launcher_items(&d);
        let starts: Vec<&Act> = buttons(&items)
            .into_iter()
            .map(|b| &b.act)
            .filter(|a| matches!(a, Act::Start { .. }))
            .collect();
        assert_eq!(starts.len(), d.stages.len());
        // Classic is spelled as "no profile".
        assert!(
            starts
                .iter()
                .all(|a| matches!(a, Act::Start { profile: None, .. }))
        );
        d.selected = 1;
        let items = launcher_items(&d);
        assert!(buttons(&items).iter().any(|b| matches!(
            &b.act,
            Act::Start { stage: Stage::Arena(id), profile: Some(p) }
                if id == "movement-lab" && p == "moon"
        )));
        // The stepper stops at both ends.
        let stepper = |d: &LauncherData, delta: i32| {
            let items = launcher_items(d);
            buttons(&items)
                .into_iter()
                .find(|b| b.act == Act::LauncherProfile(delta))
                .map(|b| b.enabled)
        };
        assert_eq!(stepper(&d, 1), Some(false));
        assert_eq!(stepper(&d, -1), Some(true));
        d.selected = 0;
        assert_eq!(stepper(&d, -1), Some(false));
    }

    #[test]
    fn the_launcher_says_what_the_sandbox_is_and_offers_a_way_back() {
        let launch = UiLaunch::default();
        let items = launcher_items(&data(&launch, false));
        let all = texts(&items).join("\n");
        assert!(all.contains("Not the original's behaviour"));
        assert!(all.contains("saves stay in memory"));
        assert!(all.is_ascii(), "{all}");
        assert!(buttons(&items).iter().any(|b| b.act == Act::End));
        assert!(!buttons(&items).iter().any(|b| b.act == Act::Quit));
        // A `--sandbox` process has no Classic menu to go back to: its
        // launcher offers Quit instead.
        let items = launcher_items(&data(&launch, true));
        assert!(!buttons(&items).iter().any(|b| b.act == Act::End));
        assert!(buttons(&items).iter().any(|b| b.act == Act::Quit));
    }

    #[test]
    fn long_lists_are_paged() {
        let list: Vec<usize> = (0..PAGE * 2 + 3).collect();
        let (first, page, pages) = page_of(&list, 0);
        assert_eq!((first.len(), page, pages), (PAGE, 0, 3));
        let (last, page, _) = page_of(&list, 99);
        assert_eq!((last.len(), page), (3, 2));
        assert_eq!(page_of::<usize>(&[], 4), (&[][..], 0, 1));
        assert!(page_row(PageList::Stage, 0, 1, 3).is_none());
        // The buttons name the page they show, from the page that is shown:
        // a stale page number in the view cannot strand them.
        let row = [page_row(PageList::Stage, 1, 3, 23).unwrap()];
        let turns: Vec<(&Act, bool)> = buttons(&row).iter().map(|b| (&b.act, b.enabled)).collect();
        assert_eq!(
            turns,
            [
                (&Act::Page(PageList::Stage, 0), true),
                (&Act::Page(PageList::Stage, 2), true)
            ]
        );
        let row = [page_row(PageList::Stage, 2, 3, 23).unwrap()];
        assert!(buttons(&row).iter().any(|b| !b.enabled));
    }

    #[test]
    fn the_launcher_appears_and_goes_with_its_phase() {
        let mut app = App::new();
        app.insert_resource(Lab::new(true))
            .insert_resource(LabLaunch::default())
            .init_resource::<UiLaunch>()
            .init_resource::<ViewState>()
            .init_resource::<ProfileList>()
            .add_systems(Update, sync);
        app.world_mut().resource_mut::<Lab>().dirs = None;
        let roots = |app: &mut App| {
            let world = app.world_mut();
            world
                .query_filtered::<Entity, With<LauncherRoot>>()
                .iter(world)
                .count()
        };
        app.update();
        assert_eq!(roots(&mut app), 0, "idle: nothing is drawn");
        app.world_mut().resource_mut::<Lab>().phase = Phase::Launcher;
        app.update();
        app.update();
        assert_eq!(roots(&mut app), 1);
        assert_eq!(
            app.world().resource::<ViewState>().launcher_profile,
            Some(0)
        );
        app.world_mut().resource_mut::<Lab>().phase = Phase::Active;
        app.update();
        assert_eq!(
            roots(&mut app),
            0,
            "the launcher does not outlive its phase"
        );
        let world = app.world_mut();
        assert_eq!(world.query::<&LabUi>().iter(world).count(), 0);
    }
}
