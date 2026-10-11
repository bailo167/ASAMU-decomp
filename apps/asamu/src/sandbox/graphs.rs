//! Speed and height graphs over the session's telemetry (rows of UI bars; no
//! chart widget and no second camera).
//!
//! Two small graphs at the bottom right (above the grapple lamps): the
//! history ring of the session's telemetry, [`PER_BAR`] ticks per bar, the
//! latest tick at the right edge. Speed is drawn from zero to the highest
//! value in the window, height between the lowest and the highest. They show
//! this recreation in the session, not the original.

use asamu_sandbox::telemetry::TELEMETRY_SAMPLES;
use bevy::ecs::schedule::common_conditions::any_with_component;
use bevy::prelude::*;

use super::widgets::{FONT, KeepClear, LabUi, STRIP_BG, TEXT_HINT, text};
use super::{Lab, LabSet, PanelState, Phase, VizSettings, lab_active};

/// Bars per graph.
const BARS: usize = 60;
/// Ticks one bar stands for.
const PER_BAR: usize = TELEMETRY_SAMPLES / BARS;
/// Height of a graph, pixels.
const GRAPH_HEIGHT: f32 = 38.0;
/// Width of a bar, pixels.
const BAR_WIDTH: f32 = 3.0;

/// A graph.
#[derive(Component, Clone, Copy, Debug, PartialEq, Eq)]
enum Graph {
    /// Speed per tick.
    Speed,
    /// Height (world Z of the collision centre) per tick.
    Height,
}

/// A bar of a graph (0 = oldest).
#[derive(Component, Clone, Copy, Debug)]
struct GraphBar {
    graph: Graph,
    index: usize,
}

/// Root of the graphs.
#[derive(Component)]
struct GraphRoot;

/// `samples` (oldest first) as `bars` buckets of `per_bar` samples each,
/// right-aligned: the latest sample is in the last bucket. A bucket holds
/// the highest (`peak`) or the mean value of its samples, or `None` when no
/// sample falls in it. Samples older than the window are left out.
fn buckets(samples: &[f32], bars: usize, per_bar: usize, peak: bool) -> Vec<Option<f32>> {
    let mut sums = vec![(0.0_f32, 0_u32, f32::NEG_INFINITY); bars];
    let per_bar = per_bar.max(1);
    for (age, value) in samples.iter().rev().enumerate() {
        let back = age / per_bar;
        if back >= bars {
            break;
        }
        if !value.is_finite() {
            continue;
        }
        if let Some(slot) = sums.get_mut(bars - 1 - back) {
            slot.0 += value;
            slot.1 += 1;
            slot.2 = slot.2.max(*value);
        }
    }
    sums.into_iter()
        .map(|(sum, count, highest)| {
            (count > 0).then(|| if peak { highest } else { sum / count as f32 })
        })
        .collect()
}

/// The lowest and highest value of the filled buckets.
fn range(values: &[Option<f32>]) -> Option<(f32, f32)> {
    values.iter().flatten().fold(None, |acc, v| match acc {
        None => Some((*v, *v)),
        Some((lo, hi)) => Some((lo.min(*v), hi.max(*v))),
    })
}

/// Bar heights in percent of the graph, from `floor` to `ceiling` (an empty
/// bucket has no bar; a filled one is at least a sliver).
fn heights(values: &[Option<f32>], floor: f32, ceiling: f32) -> Vec<f32> {
    let span = (ceiling - floor).max(1.0);
    values
        .iter()
        .map(|v| match v {
            Some(v) => (((v - floor) / span) * 100.0).clamp(3.0, 100.0),
            None => 0.0,
        })
        .collect()
}

/// A graph's caption and bar heights.
fn graph(graph: Graph, samples: &[f32]) -> (String, Vec<f32>) {
    match graph {
        Graph::Speed => {
            let values = buckets(samples, BARS, PER_BAR, true);
            let top = range(&values).map_or(0.0, |(_, hi)| hi);
            (
                format!("speed, 0 to {top:.0} uu/s (latest {TELEMETRY_SAMPLES} ticks)"),
                heights(&values, 0.0, top),
            )
        }
        Graph::Height => {
            let values = buckets(samples, BARS, PER_BAR, false);
            let (lo, hi) = range(&values).unwrap_or((0.0, 0.0));
            (
                format!("height, {lo:.0} to {hi:.0} uu"),
                heights(&values, lo, hi),
            )
        }
    }
}

fn spawn(commands: &mut Commands) {
    commands
        .spawn((
            GraphRoot,
            LabUi,
            KeepClear,
            Node {
                position_type: PositionType::Absolute,
                right: Val::Px(12.0),
                // Above the grapple lamps in the corner.
                bottom: Val::Px(54.0),
                flex_direction: FlexDirection::Column,
                row_gap: Val::Px(2.0),
                padding: UiRect::axes(Val::Px(8.0), Val::Px(5.0)),
                border_radius: BorderRadius::all(Val::Px(4.0)),
                ..default()
            },
            BackgroundColor(STRIP_BG),
            GlobalZIndex(60),
            Pickable::IGNORE,
        ))
        .with_children(|root| {
            for (graph, color) in [
                (Graph::Speed, Color::srgb(0.45, 0.85, 1.0)),
                (Graph::Height, Color::srgb(0.60, 0.95, 0.55)),
            ] {
                root.spawn((graph, text("", FONT - 1.0, TEXT_HINT), Pickable::IGNORE));
                root.spawn((
                    Node {
                        height: Val::Px(GRAPH_HEIGHT),
                        flex_direction: FlexDirection::Row,
                        align_items: AlignItems::FlexEnd,
                        column_gap: Val::Px(1.0),
                        ..default()
                    },
                    Pickable::IGNORE,
                ))
                .with_children(|bars| {
                    for index in 0..BARS {
                        bars.spawn((
                            GraphBar { graph, index },
                            Node {
                                width: Val::Px(BAR_WIDTH),
                                height: Val::Percent(0.0),
                                ..default()
                            },
                            BackgroundColor(color),
                            Pickable::IGNORE,
                        ));
                    }
                });
            }
        });
}

/// Shows the graphs while a session runs with them switched on (and the
/// inspector closed), and removes them otherwise.
#[allow(clippy::type_complexity)]
fn sync(
    mut commands: Commands,
    lab: Res<Lab>,
    viz: Res<VizSettings>,
    panel: Res<PanelState>,
    roots: Query<Entity, With<GraphRoot>>,
    mut captions: Query<(&Graph, &mut Text)>,
    mut bars: Query<(&GraphBar, &mut Node)>,
) {
    let session = lab
        .session
        .as_ref()
        .filter(|_| lab.phase == Phase::Active && viz.graphs && !panel.open);
    let Some(session) = session else {
        for root in &roots {
            commands.entity(root).despawn();
        }
        return;
    };
    if roots.is_empty() {
        // Spawned now, written from the next frame on.
        spawn(&mut commands);
        return;
    }
    let telemetry = session.telemetry();
    let speed: Vec<f32> = telemetry.speed_history().collect();
    let height: Vec<f32> = telemetry.height_history().collect();
    let (speed_caption, speed_bars) = graph(Graph::Speed, &speed);
    let (height_caption, height_bars) = graph(Graph::Height, &height);
    for (which, mut text) in &mut captions {
        let caption = match which {
            Graph::Speed => &speed_caption,
            Graph::Height => &height_caption,
        };
        if text.0 != *caption {
            text.0.clone_from(caption);
        }
    }
    for (bar, mut node) in &mut bars {
        let values = match bar.graph {
            Graph::Speed => &speed_bars,
            Graph::Height => &height_bars,
        };
        let height = Val::Percent(values.get(bar.index).copied().unwrap_or(0.0));
        if node.height != height {
            node.height = height;
        }
    }
}

pub(super) fn build(app: &mut App) {
    app.add_systems(
        Update,
        sync.run_if(lab_active.or_else(any_with_component::<GraphRoot>))
            .in_set(LabSet::Draw),
    );
}

#[cfg(test)]
mod tests {
    use asamu_sandbox::session::Session;

    use super::*;

    #[test]
    fn the_latest_sample_is_in_the_last_bucket() {
        // Seven samples, three per bar, four bars: the oldest bar is empty.
        let samples = [1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0];
        assert_eq!(
            buckets(&samples, 4, 3, true),
            [None, Some(1.0), Some(4.0), Some(7.0)]
        );
        assert_eq!(
            buckets(&samples, 4, 3, false),
            [None, Some(1.0), Some(3.0), Some(6.0)]
        );
        // More samples than the window: the oldest are left out.
        assert_eq!(buckets(&samples, 2, 2, true), [Some(5.0), Some(7.0)]);
        assert_eq!(buckets(&[], 3, 2, true), [None, None, None]);
        // A non-finite sample is skipped, not drawn.
        assert_eq!(buckets(&[f32::NAN, 2.0], 1, 2, true), [Some(2.0)]);
        assert_eq!(buckets(&[1.0], 1, 0, true), [Some(1.0)]);
    }

    #[test]
    fn bars_scale_to_the_window_and_stay_in_the_graph() {
        let values = [None, Some(0.0), Some(50.0), Some(100.0)];
        assert_eq!(range(&values), Some((0.0, 100.0)));
        assert_eq!(range(&[None, None]), None);
        assert_eq!(heights(&values, 0.0, 100.0), [0.0, 3.0, 50.0, 100.0]);
        // A flat line still draws, and nothing leaves the graph.
        let flat = [Some(400.0), Some(400.0)];
        let (lo, hi) = range(&flat).unwrap();
        for h in heights(&flat, lo, hi) {
            assert!((0.0..=100.0).contains(&h));
        }
        for h in heights(&[Some(1.0e9), Some(-1.0e9)], 0.0, 10.0) {
            assert!((0.0..=100.0).contains(&h));
        }
    }

    #[test]
    fn captions_are_ascii_and_every_bar_has_a_height() {
        let ramp: Vec<f32> = (0..TELEMETRY_SAMPLES).map(|i| i as f32).collect();
        for which in [Graph::Speed, Graph::Height] {
            let (caption, bars) = graph(which, &ramp);
            assert!(caption.is_ascii(), "{caption}");
            assert_eq!(bars.len(), BARS);
            assert!(bars.iter().all(|h| (0.0..=100.0).contains(h)));
            // A rising ramp draws rising bars.
            assert!(bars.windows(2).all(|w| w[0] <= w[1]), "{bars:?}");
            let (_, empty) = graph(which, &[]);
            assert!(empty.iter().all(|h| *h == 0.0));
        }
        assert_eq!(BARS * PER_BAR, TELEMETRY_SAMPLES);
    }

    #[test]
    fn the_graphs_follow_their_switch_and_the_session() {
        let mut lab = Lab::new(true);
        lab.phase = Phase::Active;
        lab.session = Some(Session::classic());
        lab.dirs = None;
        let mut app = App::new();
        app.insert_resource(lab)
            .init_resource::<VizSettings>()
            .init_resource::<PanelState>()
            .add_systems(Update, sync);
        let count = |app: &mut App| {
            let world = app.world_mut();
            (
                world
                    .query_filtered::<Entity, With<GraphRoot>>()
                    .iter(world)
                    .count(),
                world.query::<&GraphBar>().iter(world).count(),
            )
        };
        app.update();
        app.update();
        assert_eq!(count(&mut app), (1, 2 * BARS));
        // Switched off, covered by the inspector, or the session over: gone.
        app.world_mut().resource_mut::<VizSettings>().graphs = false;
        app.update();
        assert_eq!(count(&mut app), (0, 0));
        app.world_mut().resource_mut::<VizSettings>().graphs = true;
        app.update();
        assert_eq!(count(&mut app).0, 1);
        app.world_mut().resource_mut::<PanelState>().open = true;
        app.update();
        assert_eq!(count(&mut app), (0, 0));
        app.world_mut().resource_mut::<PanelState>().open = false;
        app.update();
        assert_eq!(count(&mut app).0, 1);
        app.world_mut().resource_mut::<Lab>().phase = Phase::Idle;
        app.update();
        assert_eq!(count(&mut app), (0, 0));
        let world = app.world_mut();
        assert_eq!(world.query::<&LabUi>().iter(world).count(), 0);
    }
}
