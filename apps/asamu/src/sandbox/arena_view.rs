//! Rendering of a hand-made arena: hides the stock graybox visuals while a
//! session shows a level of its own, and puts them back when the session
//! ends. Also the arena's marker labels (ruler marks, station names), drawn
//! as UI text at the markers' places on screen.
//!
//! The stock `spawn_level` builds its boxes once, for the graybox test
//! level. While a session runs on an arena, [`sync`] hides those
//! (`crate::LevelVisual`) and spawns cuboids for the running level; movers
//! get `crate::MoverVisual`, so the stock `sync_movers` moves them, and the
//! stock `draw_gizmos` already draws checkpoints, crystals and attractor
//! pads for any level. Colours are ours: they tell floors, ruler posts and
//! the tagged surfaces apart. Everything spawned here is removed, and the
//! stock visuals are shown again, as soon as no arena is being played.

use asamu_core::coords::ue_extents_to_bevy;
use asamu_core::glam as sim_glam;
use asamu_game::asamu_world::{Level, StaticBox, SurfaceTag};
use asamu_sandbox::arena::{ArenaMarker, arena_markers};
use bevy::ecs::schedule::common_conditions::any_with_component;
use bevy::prelude::*;
use bevy::ui::UiGlobalTransform;

use super::widgets::{FONT, KeepClear, LabUi, TEXT, clip, text};
use super::{Lab, LabSet, PanelState, Phase, Stage, VizSettings, lab_active};
use crate::{LevelVisual, MoverVisual, PlayerCamera, SCALE, Sim, bevy_vec, to_render};

/// How a box of an arena is drawn.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Look {
    /// A floor slab (top at or below the ground plane).
    Floor,
    /// Plain geometry the grapple does not take.
    Solid,
    /// A thin ruler post.
    Post,
    /// A surface the grapple takes.
    Grapple,
    /// `TopOnlyGrappleAble`.
    TopOnly,
    /// `BottomOnlyGrappleAble`.
    BottomOnly,
    /// `NotLandable`.
    NotLandable,
    /// A grapple point.
    Hook,
    /// A recharge crystal.
    Crystal,
    /// A glow flower.
    Flower,
    /// A story interactable.
    Interactable,
    /// A mover.
    Mover,
}

impl Look {
    const ALL: [Self; 12] = [
        Self::Floor,
        Self::Solid,
        Self::Post,
        Self::Grapple,
        Self::TopOnly,
        Self::BottomOnly,
        Self::NotLandable,
        Self::Hook,
        Self::Crystal,
        Self::Flower,
        Self::Interactable,
        Self::Mover,
    ];

    /// Base colour and emissive colour (ours; the hook, crystal, flower,
    /// interactable, mover and grapple surface follow the stock graybox).
    fn colors(self) -> (Color, LinearRgba) {
        let none = LinearRgba::BLACK;
        match self {
            Self::Floor => (Color::srgb(0.46, 0.49, 0.53), none),
            Self::Solid => (Color::srgb(0.74, 0.74, 0.72), none),
            Self::Post => (
                Color::srgb(0.98, 0.88, 0.30),
                LinearRgba::rgb(0.35, 0.28, 0.02),
            ),
            Self::Grapple => (Color::srgb(0.95, 0.55, 0.15), none),
            Self::TopOnly => (Color::srgb(0.98, 0.80, 0.30), none),
            Self::BottomOnly => (Color::srgb(0.82, 0.36, 0.22), none),
            Self::NotLandable => (Color::srgb(0.66, 0.38, 0.82), none),
            Self::Hook => (Color::srgb(1.0, 0.75, 0.2), LinearRgba::rgb(0.8, 0.4, 0.05)),
            Self::Crystal => (Color::srgb(0.3, 0.9, 1.0), LinearRgba::rgb(0.1, 0.5, 0.7)),
            Self::Flower => (Color::srgb(0.8, 0.4, 1.0), LinearRgba::rgb(0.4, 0.1, 0.6)),
            Self::Interactable => (Color::srgb(0.95, 0.9, 0.3), none),
            Self::Mover => (Color::srgb(0.85, 0.45, 0.2), none),
        }
    }
}

/// Widest a box may be, in both horizontal directions, to count as a ruler
/// post (UU; ours).
const POST_WIDTH: f32 = 40.0;
/// Narrowest a slab may be to count as a floor (UU; ours).
const FLOOR_SPAN: f32 = 400.0;

fn box_look(b: &StaticBox) -> Look {
    match b.tag {
        SurfaceTag::TopOnlyGrappleAble => Look::TopOnly,
        SurfaceTag::BottomOnlyGrappleAble => Look::BottomOnly,
        SurfaceTag::NotLandable => Look::NotLandable,
        SurfaceTag::None | SurfaceTag::GrappleInteractable => {
            let size = b.max - b.min;
            if b.grapple_able {
                Look::Grapple
            } else if size.x.max(size.y) <= POST_WIDTH {
                Look::Post
            } else if b.max.z <= 0.0 && size.x.min(size.y) >= FLOOR_SPAN {
                Look::Floor
            } else {
                Look::Solid
            }
        }
    }
}

/// One cuboid to draw.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Visual {
    min: sim_glam::Vec3,
    max: sim_glam::Vec3,
    look: Look,
    /// Index into `Level::movers` for a mover.
    mover: Option<usize>,
}

/// The cuboids of `level`: the same elements the stock graybox draws.
fn visuals(level: &Level) -> Vec<Visual> {
    let fixed = |min, max, look| Visual {
        min,
        max,
        look,
        mover: None,
    };
    let cube = |centre: sim_glam::Vec3, half: f32, look| {
        let h = sim_glam::Vec3::splat(half);
        fixed(centre - h, centre + h, look)
    };
    let mut out = Vec::new();
    out.extend(
        level
            .static_boxes
            .iter()
            .map(|b| fixed(b.min, b.max, box_look(b))),
    );
    out.extend(
        level
            .grapple_points
            .iter()
            .map(|g| cube(g.position, g.half_extent, Look::Hook)),
    );
    out.extend(
        level
            .crystals
            .iter()
            .map(|c| cube(c.center, c.half_extent, Look::Crystal)),
    );
    out.extend(
        level
            .flowers
            .iter()
            .map(|f| cube(f.center, f.half_extent, Look::Flower)),
    );
    out.extend(
        level
            .interactables
            .iter()
            .map(|i| fixed(i.min, i.max, Look::Interactable)),
    );
    out.extend(level.movers.iter().enumerate().map(|(index, m)| Visual {
        min: m.min,
        max: m.max,
        look: Look::Mover,
        mover: Some(index),
    }));
    out
}

/// An entity this module spawned for an arena.
#[derive(Component)]
struct ArenaVisual;

/// What the arena view shows now.
#[derive(Resource, Debug, Default)]
pub(super) struct ArenaScene {
    /// Name of the level the Sandbox's own cuboids stand for.
    shown: Option<String>,
    /// The stock graybox visuals are hidden.
    stock_hidden: bool,
    /// The arena the markers belong to.
    markers_of: Option<String>,
    markers: Vec<ArenaMarker>,
}

impl ArenaScene {
    /// The markers of the arena being played (empty otherwise).
    pub(super) fn markers(&self) -> &[ArenaMarker] {
        &self.markers
    }

    /// Something of an arena is still in place.
    fn in_use(&self) -> bool {
        self.shown.is_some() || self.stock_hidden || self.markers_of.is_some()
    }
}

fn scene_in_use(scene: Res<ArenaScene>) -> bool {
    scene.in_use()
}

/// Keeps the rendered level in step with the session's stage.
#[allow(clippy::too_many_arguments, clippy::type_complexity)]
fn sync(
    mut commands: Commands,
    lab: Res<Lab>,
    sim: Option<Res<Sim>>,
    mut scene: ResMut<ArenaScene>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    own: Query<Entity, With<ArenaVisual>>,
    mut stock: Query<&mut Visibility, (With<LevelVisual>, Without<ArenaVisual>)>,
) {
    let arena = match (&lab.stage, lab.phase) {
        (Some(Stage::Arena(id)), Phase::Active) => Some(id.as_str()),
        _ => None,
    };
    // The markers are built once per arena, not per frame.
    if scene.markers_of.as_deref() != arena {
        scene.markers = arena.map(arena_markers).unwrap_or_default();
        scene.markers_of = arena.map(str::to_owned);
    }
    // The running hand-made level, while the stage is an arena.
    let level = arena
        .and(sim.as_deref())
        .filter(|sim| sim.game.scene_map().is_none())
        .map(|sim| sim.game.level());
    let name = level.map(|l| l.name.as_str());
    if scene.shown.as_deref() != name {
        for entity in &own {
            commands.entity(entity).despawn();
        }
        if let Some(level) = level {
            let looks: Vec<(Look, Handle<StandardMaterial>)> = Look::ALL
                .into_iter()
                .map(|look| {
                    let (base_color, emissive) = look.colors();
                    let material = materials.add(StandardMaterial {
                        base_color,
                        emissive,
                        perceptual_roughness: 0.9,
                        ..default()
                    });
                    (look, material)
                })
                .collect();
            for visual in visuals(level) {
                let Some((_, material)) = looks.iter().find(|(look, _)| *look == visual.look)
                else {
                    continue;
                };
                let size = bevy_vec(ue_extents_to_bevy(visual.max - visual.min, SCALE));
                let mut entity = commands.spawn((
                    ArenaVisual,
                    LabUi,
                    Mesh3d(meshes.add(Cuboid::new(size.x, size.y, size.z))),
                    MeshMaterial3d(material.clone()),
                    Transform::from_translation(to_render((visual.min + visual.max) * 0.5)),
                ));
                if let Some(index) = visual.mover {
                    // The stock `sync_movers` moves it.
                    entity.insert(MoverVisual(index));
                }
            }
        }
        scene.shown = name.map(str::to_owned);
    }
    // The stock graybox visuals are hidden exactly while ours are shown.
    let hide = scene.shown.is_some();
    if hide || scene.stock_hidden {
        let wanted = if hide {
            Visibility::Hidden
        } else {
            Visibility::Inherited
        };
        for mut visibility in &mut stock {
            if *visibility != wanted {
                *visibility = wanted;
            }
        }
        if scene.stock_hidden != hide {
            scene.stock_hidden = hide;
        }
    }
}

/// Labels drawn at once.
const LABELS: usize = 14;
/// Markers farther than this from the eye have no label (UU; ours).
const LABEL_RANGE: f32 = 3200.0;
/// Text size of a marker label.
const LABEL_FONT: f32 = FONT - 1.0;
/// Widest a label's text is drawn, pixels (longer texts wrap).
const LABEL_WIDTH: f32 = 232.0;
/// Where a label's box starts, relative to its marker's point on screen:
/// beside the point, not over it.
const LABEL_OFFSET: Vec2 = Vec2::new(8.0, -8.0);
/// Half the size of the area around the crosshair that labels keep clear,
/// pixels.
const CROSSHAIR_CLEAR: Vec2 = Vec2::new(40.0, 26.0);

/// Root of the marker labels.
#[derive(Component)]
struct LabelRoot;

/// A marker label's box (pool index).
#[derive(Component)]
struct MarkerLabel(usize);

/// A marker label's text (pool index).
#[derive(Component)]
struct MarkerText(usize);

fn spawn_labels(commands: &mut Commands) {
    commands
        .spawn((
            LabelRoot,
            LabUi,
            Node {
                position_type: PositionType::Absolute,
                width: Val::Percent(100.0),
                height: Val::Percent(100.0),
                ..default()
            },
            // Under the Sandbox HUD (60).
            GlobalZIndex(55),
            Pickable::IGNORE,
        ))
        .with_children(|root| {
            for index in 0..LABELS {
                root.spawn((
                    MarkerLabel(index),
                    Node {
                        position_type: PositionType::Absolute,
                        max_width: Val::Px(LABEL_WIDTH + 8.0),
                        padding: UiRect::axes(Val::Px(4.0), Val::Px(1.0)),
                        border_radius: BorderRadius::all(Val::Px(3.0)),
                        ..default()
                    },
                    BackgroundColor(Color::srgba(0.0, 0.0, 0.0, 0.62)),
                    Visibility::Hidden,
                    Pickable::IGNORE,
                    children![(
                        MarkerText(index),
                        text("", LABEL_FONT, TEXT),
                        Pickable::IGNORE
                    )],
                ));
            }
        });
}

/// The markers within [`LABEL_RANGE`] of `eye`, nearest first.
fn nearest(markers: &[ArenaMarker], eye: sim_glam::Vec3) -> Vec<&ArenaMarker> {
    let mut near: Vec<(f32, &ArenaMarker)> = markers
        .iter()
        .map(|marker| (marker.position.distance(eye), marker))
        .filter(|(distance, _)| *distance <= LABEL_RANGE)
        .collect();
    near.sort_by(|a, b| a.0.total_cmp(&b.0));
    near.into_iter().map(|(_, marker)| marker).collect()
}

/// The box (two corners) the label `text` takes for a marker drawn at `at`:
/// beside the marker's point, moved back into the `viewport` where it would
/// leave it. An estimate from the font's fixed advance of 0.6 em, on the
/// generous side for wrapped texts.
fn label_box(at: Vec2, text: &str, viewport: Vec2) -> (Vec2, Vec2) {
    let advance = LABEL_FONT * 0.6;
    let full = text.chars().count() as f32 * advance;
    let lines = (full / (LABEL_WIDTH * 0.85)).ceil().max(1.0);
    let size = Vec2::new(full.min(LABEL_WIDTH) + 10.0, lines * LABEL_FONT * 1.3 + 4.0);
    let room = (viewport - size).max(Vec2::ZERO);
    let min = (at + LABEL_OFFSET).clamp(Vec2::ZERO, room);
    (min, min + size)
}

fn boxes_overlap(a: (Vec2, Vec2), b: (Vec2, Vec2)) -> bool {
    a.0.x < b.1.x && b.0.x < a.1.x && a.0.y < b.1.y && b.0.y < a.1.y
}

/// Chooses the labels to draw from `candidates` (a marker's point on screen
/// and its text, nearest marker first) and says where each one goes (the
/// top-left corner of its box). A label is left out when its marker's point
/// is outside the `viewport`, or when its box would cover the crosshair, one
/// of the `blocked` boxes (other text on screen) or a label already chosen.
/// At most [`LABELS`].
fn declutter<'a>(
    candidates: impl IntoIterator<Item = (Vec2, &'a str)>,
    viewport: Vec2,
    blocked: &[(Vec2, Vec2)],
) -> Vec<(Vec2, &'a str)> {
    let centre = viewport * 0.5;
    let crosshair = (centre - CROSSHAIR_CLEAR, centre + CROSSHAIR_CLEAR);
    let mut taken: Vec<(Vec2, Vec2)> = vec![crosshair];
    taken.extend_from_slice(blocked);
    let mut chosen = Vec::new();
    for (at, text) in candidates {
        if chosen.len() >= LABELS {
            break;
        }
        let inside = at.x >= 0.0 && at.y >= 0.0 && at.x <= viewport.x && at.y <= viewport.y;
        if !inside || !at.is_finite() {
            continue;
        }
        let area = label_box(at, text, viewport);
        if taken.iter().any(|other| boxes_overlap(area, *other)) {
            continue;
        }
        taken.push(area);
        chosen.push((area.0, text));
    }
    chosen
}

/// Places the marker labels where their markers are on screen.
#[allow(clippy::too_many_arguments, clippy::type_complexity)]
fn labels(
    mut commands: Commands,
    lab: Res<Lab>,
    sim: Option<Res<Sim>>,
    viz: Res<VizSettings>,
    panel: Res<PanelState>,
    scene: Res<ArenaScene>,
    camera: Query<(&Camera, &Transform), With<PlayerCamera>>,
    roots: Query<Entity, With<LabelRoot>>,
    other_text: Query<
        (&ComputedNode, &UiGlobalTransform, &InheritedVisibility),
        Or<(
            With<KeepClear>,
            With<crate::hud::HudInfoPanel>,
            With<crate::hud::AbilityPanel>,
        )>,
    >,
    mut boxes: Query<(&MarkerLabel, &mut Node, &mut Visibility), Without<KeepClear>>,
    mut texts: Query<(&MarkerText, &mut Text)>,
) {
    let wanted =
        lab.phase == Phase::Active && viz.markers && !panel.open && !scene.markers().is_empty();
    let (Some(sim), true) = (sim.as_deref(), wanted) else {
        for root in &roots {
            commands.entity(root).despawn();
        }
        return;
    };
    if roots.is_empty() {
        // Spawned now, placed from the next frame on.
        spawn_labels(&mut commands);
        return;
    }
    let Some((camera, transform)) = camera.iter().next() else {
        return;
    };
    // The camera was placed for this frame already (`LabSet::Draw` runs
    // after `sync_camera`); its global transform is only propagated later.
    let view = GlobalTransform::from(*transform);
    let viewport = camera.logical_viewport_size().unwrap_or(Vec2::ZERO);
    // The text already on screen (the read-out, the ability panel, the
    // Sandbox HUD, the graphs), as laid out last frame, in logical pixels.
    let blocked: Vec<(Vec2, Vec2)> = other_text
        .iter()
        .filter(|(_, _, visible)| visible.get())
        .map(|(node, transform, _)| {
            let centre = transform.translation * node.inverse_scale_factor;
            let half = node.size * node.inverse_scale_factor * 0.5;
            (centre - half, centre + half)
        })
        .collect();
    let near = nearest(scene.markers(), sim.curr_eye);
    let placed = declutter(
        near.iter().filter_map(|marker| {
            let at = camera
                .world_to_viewport(&view, to_render(marker.position))
                .ok()?;
            Some((at, marker.text.as_str()))
        }),
        viewport,
        &blocked,
    );
    for (label, mut node, mut visibility) in &mut boxes {
        let (wanted, at) = match placed.get(label.0) {
            Some((corner, _)) => (Visibility::Inherited, *corner),
            None => (Visibility::Hidden, Vec2::ZERO),
        };
        if *visibility != wanted {
            *visibility = wanted;
        }
        if wanted == Visibility::Inherited {
            let (left, top) = (Val::Px(at.x), Val::Px(at.y));
            if node.left != left {
                node.left = left;
            }
            if node.top != top {
                node.top = top;
            }
        }
    }
    for (label, mut text) in &mut texts {
        let content = placed
            .get(label.0)
            .map(|(_, text)| super::widgets::ascii(&clip(text, 96)))
            .unwrap_or_default();
        if text.0 != content {
            text.0 = content;
        }
    }
}

pub(super) fn build(app: &mut App) {
    app.init_resource::<ArenaScene>().add_systems(
        Update,
        (
            sync.run_if(lab_active.or_else(scene_in_use)),
            labels.run_if(lab_active.or_else(any_with_component::<LabelRoot>)),
        )
            .chain()
            .in_set(LabSet::Draw),
    );
}

#[cfg(test)]
mod tests {
    use asamu_game::Game;
    use asamu_sandbox::arena::{GRAPPLE_LAB, MOVEMENT_LAB, build_arena, builtin_arenas};
    use asamu_sandbox::session::Session;

    use super::*;

    #[test]
    fn every_element_of_an_arena_is_drawn_once() {
        for arena in builtin_arenas() {
            let level = build_arena(arena.id).unwrap();
            let all = visuals(&level);
            assert_eq!(
                all.len(),
                level.static_boxes.len()
                    + level.grapple_points.len()
                    + level.crystals.len()
                    + level.flowers.len()
                    + level.interactables.len()
                    + level.movers.len(),
                "{}",
                arena.id
            );
            // Movers are tagged with their index, so the stock system moves
            // them; nothing else is.
            let movers: Vec<usize> = all.iter().filter_map(|v| v.mover).collect();
            assert_eq!(movers, (0..level.movers.len()).collect::<Vec<_>>());
            for visual in &all {
                assert_eq!(visual.mover.is_some(), visual.look == Look::Mover);
                let size = visual.max - visual.min;
                assert!(size.min_element() > 0.0, "{visual:?}");
                assert!(Look::ALL.contains(&visual.look));
            }
        }
    }

    #[test]
    fn looks_tell_floors_posts_and_tagged_surfaces_apart() {
        let looks = |id: &str| -> Vec<Look> {
            visuals(&build_arena(id).unwrap())
                .into_iter()
                .map(|v| v.look)
                .collect()
        };
        let movement = looks(MOVEMENT_LAB);
        for look in [Look::Floor, Look::Post, Look::Solid] {
            assert!(movement.contains(&look), "{look:?}");
        }
        let grapple = looks(GRAPPLE_LAB);
        for look in [
            Look::Floor,
            Look::Hook,
            Look::Grapple,
            Look::TopOnly,
            Look::BottomOnly,
            Look::NotLandable,
            Look::Solid,
            Look::Crystal,
            Look::Mover,
        ] {
            assert!(grapple.contains(&look), "{look:?}");
        }
        // Every look has its own colour.
        let mut colors: Vec<[u8; 3]> = Look::ALL
            .iter()
            .map(|look| {
                let c = look.colors().0.to_srgba();
                [c.red, c.green, c.blue].map(|v| (v * 255.0).round() as u8)
            })
            .collect();
        colors.sort_unstable();
        colors.dedup();
        assert_eq!(colors.len(), Look::ALL.len());
    }

    #[test]
    fn labels_go_to_the_nearest_markers_only() {
        let markers = arena_markers(MOVEMENT_LAB);
        assert!(markers.len() > LABELS, "the pool is smaller than the arena");
        assert!(
            markers.iter().all(|m| m.text.is_ascii()),
            "labels are ASCII"
        );
        let eye = sim_glam::Vec3::new(-300.0, 0.0, 80.0);
        let near = nearest(&markers, eye);
        assert!(!near.is_empty() && near.len() < markers.len());
        let distances: Vec<f32> = near.iter().map(|m| m.position.distance(eye)).collect();
        assert!(distances.windows(2).all(|w| w[0] <= w[1]));
        assert!(distances.iter().all(|d| *d <= LABEL_RANGE));
        // Far from everything: no label.
        assert!(nearest(&markers, sim_glam::Vec3::splat(1.0e6)).is_empty());
        assert!(nearest(&[], eye).is_empty());
    }

    #[test]
    fn labels_do_not_cover_each_other_or_the_crosshair() {
        let viewport = Vec2::new(1280.0, 720.0);
        let at = |x: f32, y: f32| Vec2::new(x, y);
        // The first of two labels at nearly the same place wins (it is the
        // nearer marker); one well away from both is drawn too.
        let chosen = declutter(
            [
                (at(100.0, 100.0), "1 s walking (440 uu)"),
                (at(110.0, 104.0), "2 s walking (880 uu)"),
                (at(100.0, 300.0), "riser 1.00 x Classic MaxStepHeight"),
            ],
            viewport,
            &[],
        );
        assert_eq!(
            chosen.iter().map(|(_, text)| *text).collect::<Vec<_>>(),
            ["1 s walking (440 uu)", "riser 1.00 x Classic MaxStepHeight"]
        );
        // Nothing lands on the crosshair, and nothing off screen is drawn.
        let chosen = declutter(
            [
                (viewport * 0.5 - at(20.0, 0.0), "over the crosshair"),
                (at(-5.0, 100.0), "left of the window"),
                (at(100.0, 900.0), "below the window"),
                (at(f32::NAN, 0.0), "nowhere"),
            ],
            viewport,
            &[],
        );
        assert!(chosen.is_empty(), "{chosen:?}");
        // Nor on other text on screen (a HUD box), but beside it.
        let hud = (at(0.0, 0.0), at(600.0, 300.0));
        let chosen = declutter(
            [
                (at(300.0, 150.0), "inside the HUD"),
                (at(560.0, 150.0), "reaching into the HUD"),
                (at(700.0, 150.0), "beside the HUD"),
            ],
            viewport,
            &[hud],
        );
        assert_eq!(
            chosen.iter().map(|(_, text)| *text).collect::<Vec<_>>(),
            ["beside the HUD"]
        );
        let chosen = declutter([(at(300.0, 150.0), "inside the HUD")], viewport, &[hud]);
        assert!(chosen.is_empty(), "{chosen:?}");
        // Never more than the pool holds, however many fit.
        let column: Vec<(Vec2, &str)> = (0..40)
            .map(|i| (at(20.0, 10.0 + 17.0 * i as f32), "x"))
            .collect();
        assert_eq!(declutter(column, viewport, &[]).len(), LABELS);
        // A long text takes more lines, so its box is taller.
        let short = label_box(at(50.0, 50.0), "short", viewport);
        let long = label_box(at(50.0, 50.0), &"long ".repeat(30), viewport);
        assert!(long.1.y - long.0.y > short.1.y - short.0.y);
        assert!(long.1.x - long.0.x <= LABEL_WIDTH + 10.0);
        // A label goes beside its marker's point, and stays in the window
        // for a marker at its edge.
        assert_eq!(short.0, at(50.0, 50.0) + LABEL_OFFSET);
        let edge = label_box(
            viewport - at(4.0, 2.0),
            "+0.30 x Classic jump apex",
            viewport,
        );
        assert!(edge.1.x <= viewport.x && edge.1.y <= viewport.y, "{edge:?}");
        assert!(edge.0.x >= 0.0 && edge.0.y >= 0.0);
        let corner = label_box(Vec2::ZERO, "x", viewport);
        assert!(corner.0.x >= 0.0 && corner.0.y >= 0.0, "{corner:?}");
        let placed = declutter([(at(1270.0, 400.0), "at the right edge")], viewport, &[]);
        assert_eq!(placed.len(), 1);
        assert!(placed[0].0.x < 1270.0 - 100.0, "{placed:?}");
    }

    /// An app with what [`sync`] needs, a stock visual standing in for the
    /// graybox, and a session on `stage`.
    fn app_on(stage: Stage, game: Game) -> (App, Entity) {
        let mut lab = Lab::new(true);
        lab.phase = Phase::Active;
        lab.session = Some(Session::classic());
        lab.stage = Some(stage);
        lab.dirs = None;
        let mut app = App::new();
        app.insert_resource(lab)
            .insert_resource(Sim::new(game, String::new()))
            .init_resource::<ArenaScene>()
            .init_resource::<Assets<Mesh>>()
            .init_resource::<Assets<StandardMaterial>>()
            .add_systems(Update, sync);
        let stock = app
            .world_mut()
            .spawn((LevelVisual, Visibility::Inherited))
            .id();
        (app, stock)
    }

    fn own_visuals(app: &mut App) -> usize {
        let world = app.world_mut();
        world
            .query_filtered::<Entity, With<ArenaVisual>>()
            .iter(world)
            .count()
    }

    #[test]
    fn an_arena_replaces_the_stock_visuals_and_gives_them_back() {
        let level = build_arena(GRAPPLE_LAB).unwrap();
        let expected = visuals(&level).len();
        let movers = level.movers.len();
        let game = Session::classic().new_game(level).unwrap();
        let (mut app, stock) = app_on(Stage::Arena(GRAPPLE_LAB.to_owned()), game);
        app.update();
        assert_eq!(own_visuals(&mut app), expected);
        assert_eq!(
            app.world().get::<Visibility>(stock),
            Some(&Visibility::Hidden)
        );
        let world = app.world_mut();
        assert_eq!(
            world
                .query_filtered::<&MoverVisual, With<ArenaVisual>>()
                .iter(world)
                .count(),
            movers
        );
        assert!(!app.world().resource::<ArenaScene>().markers().is_empty());
        // A second frame builds nothing again.
        app.update();
        assert_eq!(own_visuals(&mut app), expected);
        // Session over: ours are gone, the stock ones are back.
        app.world_mut().resource_mut::<Lab>().phase = Phase::Idle;
        app.update();
        assert_eq!(own_visuals(&mut app), 0);
        assert_eq!(
            app.world().get::<Visibility>(stock),
            Some(&Visibility::Inherited)
        );
        let scene = app.world().resource::<ArenaScene>();
        assert!(!scene.in_use());
        assert!(scene.markers().is_empty());
        let world = app.world_mut();
        assert_eq!(world.query::<&LabUi>().iter(world).count(), 0);
    }

    #[test]
    fn nothing_of_the_view_outlives_a_session() {
        use super::super::control::testing::{Rig, frames, lab, rig, send};
        use super::super::{LabRequest, graphs, hud, input, launcher, panel, widgets};

        // The runtime with every part of the view except the gizmo
        // visualisers (lines drawn anew each frame: they leave nothing).
        let mut app = rig(Rig::default());
        app.init_resource::<VizSettings>()
            .init_resource::<Assets<Mesh>>()
            .init_resource::<Assets<StandardMaterial>>();
        input::build(&mut app);
        panel::build(&mut app);
        widgets::build(&mut app);
        launcher::build(&mut app);
        hud::build(&mut app);
        graphs::build(&mut app);
        build(&mut app);
        let stock = app
            .world_mut()
            .spawn((LevelVisual, Visibility::Inherited))
            .id();
        let view_entities = |app: &mut App| {
            let world = app.world_mut();
            world.query::<&LabUi>().iter(world).count()
        };
        frames(&mut app, 3);
        assert_eq!(view_entities(&mut app), 0, "Classic: the view is idle");

        // The launcher, then a session on an arena with the inspector open.
        send(&mut app, LabRequest::OpenLauncher);
        frames(&mut app, 3);
        assert_eq!(lab(&app).phase, Phase::Launcher);
        assert_eq!(view_entities(&mut app), 1, "the launcher");
        send(
            &mut app,
            LabRequest::Start {
                stage: Stage::Arena(GRAPPLE_LAB.to_owned()),
                profile: Some("moon".to_owned()),
            },
        );
        frames(&mut app, 4);
        assert_eq!(lab(&app).phase, Phase::Active, "{:?}", lab(&app).notice);
        let level = build_arena(GRAPPLE_LAB).unwrap();
        // The arena's cuboids, the HUD, the graphs and the marker labels.
        assert_eq!(view_entities(&mut app), visuals(&level).len() + 3);
        assert_eq!(
            app.world().get::<Visibility>(stock),
            Some(&Visibility::Hidden)
        );
        send(&mut app, LabRequest::OpenInspector);
        frames(&mut app, 3);
        assert!(app.world().resource::<PanelState>().open);
        // The inspector's panel comes; the graphs and the labels make room.
        assert_eq!(view_entities(&mut app), visuals(&level).len() + 2);

        // The session ends with the inspector still open.
        send(&mut app, LabRequest::End);
        frames(&mut app, 3);
        assert_eq!(lab(&app).phase, Phase::Idle);
        assert!(lab(&app).session.is_none());
        assert_eq!(view_entities(&mut app), 0, "something outlived the session");
        assert_eq!(
            app.world().get::<Visibility>(stock),
            Some(&Visibility::Inherited)
        );
        assert!(!app.world().resource::<ArenaScene>().in_use());
        assert!(!app.world().resource::<PanelState>().open);
    }

    #[test]
    fn the_graybox_stage_keeps_the_stock_visuals() {
        let (mut app, stock) = app_on(Stage::Graybox, Game::graybox().unwrap());
        app.update();
        app.update();
        assert_eq!(own_visuals(&mut app), 0);
        assert_eq!(
            app.world().get::<Visibility>(stock),
            Some(&Visibility::Inherited)
        );
        assert!(!app.world().resource::<ArenaScene>().in_use());
    }
}
