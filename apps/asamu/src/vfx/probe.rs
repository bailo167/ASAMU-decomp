//! Developer-only measurement of where the grapple beam's two ends are on
//! screen (off unless the environment variable `ASAMU_VFX_PROBE` is set).
//!
//! The beam's start has to be the hand's socket as the hand's camera draws
//! it, and its end the anchor as the world's camera draws it
//! ([`super::emitter`]). This checks both in a running app, from the things
//! the renderer is given rather than from the beam's own arithmetic:
//!
//! - **one line in the log per frame the beam is drawn** (`vfx probe:`), with
//!   the pixel the hand camera's own projection puts the socket node on, the
//!   pixel it puts the middle of the first row of each ribbon's mesh on (the
//!   mesh as it is handed to the renderer this frame), the pixel the world
//!   camera puts the gun's anchor on, the pixel the hand camera puts the
//!   middle of each ribbon's last row on, and the two distances in pixels
//!   (the largest over the four ribbons). While the anchor rides an actor
//!   the level moves, also the pixel of the point of that actor's **drawn**
//!   meshes that was under the anchor at the attach (the actor's placement
//!   as `kismet::sync_actor_render` uses it, moved rigidly) and its distance
//!   from the beam's end: the beam stays on the rock that is drawn;
//! - **dots in the picture**, so that a window read-back can be measured
//!   without the log: a magenta ball that is a child of the socket node (it
//!   is where the hand is drawn, by construction), a small yellow ball at
//!   the middle of the first ribbon's first row (brought towards the eye
//!   along its line of sight, so that it is in front), a red ball at the
//!   anchor drawn by the world's camera, and a small yellow ball at the
//!   middle of the first ribbon's last row drawn by the hand's camera. Where
//!   the beam is right each yellow dot sits in the middle of its larger dot.
//!
//! `ASAMU_VFX_PROBE=nohand` also hides the hand's meshes (the skeleton still
//! animates): the socket is on the palm, behind the hand as seen from the
//! eye, so only then are the two dots at the hand in the picture.
//! `ASAMU_VFX_PROBE=log` draws no dots.
//!
//! Nothing here runs in normal play, and nothing here writes the simulation.

use bevy::camera::visibility::{NoFrustumCulling, RenderLayers};
use bevy::light::{NotShadowCaster, NotShadowReceiver};
use bevy::math::{Affine3A, DVec2};
use bevy::prelude::*;

use super::{BeamRibbon, BeamSocket, BeamView, VfxState, beam_anchor, bursts, shape_beam};
use crate::{FIRST_PERSON_LAYER, PlayerCamera, Sim, to_render};

/// Radius of the large dots (socket, anchor) on screen, pixels.
const LARGE_DOT_PIXELS: f32 = 7.0;
/// Radius of the small dots (the beam's two ends) on screen, pixels.
const SMALL_DOT_PIXELS: f32 = 3.0;
/// How far towards the eye the dot of the beam's start is brought (a factor
/// on its distance; any factor keeps its pixel).
const START_DOT_PULL: f32 = 0.6;

/// What the probe does.
#[derive(Resource, Clone, Copy, Debug, PartialEq, Eq)]
struct Probe {
    /// Draw the dots.
    dots: bool,
    /// Hide the hand's meshes.
    hide_hand: bool,
}

impl Probe {
    /// The probe a value of `ASAMU_VFX_PROBE` asks for; `None` for an unset,
    /// empty or `0` value.
    fn parse(value: Option<&str>) -> Option<Self> {
        let value = value?.trim();
        match value.to_ascii_lowercase().as_str() {
            "" | "0" | "off" => None,
            "log" => Some(Self {
                dots: false,
                hide_hand: false,
            }),
            "nohand" => Some(Self {
                dots: true,
                hide_hand: true,
            }),
            _ => Some(Self {
                dots: true,
                hide_hand: false,
            }),
        }
    }
}

/// One of the probe's dots.
#[derive(Component, Clone, Copy, Debug, PartialEq, Eq)]
enum ProbeDot {
    /// On the socket node (its child).
    Socket,
    /// The middle of the first row of the first ribbon.
    BeamStart,
    /// The gun's anchor, drawn by the world's camera.
    Anchor,
    /// The middle of the last row of the first ribbon.
    BeamEnd,
}

/// The dots' mesh and materials.
#[derive(Resource)]
struct ProbeAssets {
    ball: Handle<Mesh>,
    magenta: Handle<StandardMaterial>,
    yellow: Handle<StandardMaterial>,
    red: Handle<StandardMaterial>,
}

/// The point of a moved actor the anchor was put on.
#[derive(Clone, Copy, Debug, PartialEq)]
struct RockPoint {
    actor: u32,
    /// The actor's placement at the attach (UE location and rotator).
    placement: (asamu_core::glam::Vec3, [i32; 3]),
    /// The anchor at the attach, render space.
    anchor: Vec3,
}

/// Adds the probe when the environment asks for it.
pub(super) fn add(app: &mut App) {
    let Some(probe) = Probe::parse(std::env::var("ASAMU_VFX_PROBE").ok().as_deref()) else {
        return;
    };
    info!("vfx probe: {probe:?}");
    app.insert_resource(probe)
        .add_systems(Startup, setup)
        .add_systems(Update, (mark_socket, hide_hand))
        .add_systems(
            PostUpdate,
            measure
                .after(shape_beam)
                .after(bevy::camera::CameraUpdateSystems),
        );
}

fn flat(color: Color) -> StandardMaterial {
    StandardMaterial {
        base_color: color,
        unlit: true,
        fog_enabled: false,
        ..default()
    }
}

fn setup(
    mut commands: Commands,
    probe: Res<Probe>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    let assets = ProbeAssets {
        ball: meshes.add(Sphere::new(1.0)),
        magenta: materials.add(flat(Color::linear_rgb(1.0, 0.0, 1.0))),
        yellow: materials.add(flat(Color::linear_rgb(1.0, 1.0, 0.0))),
        red: materials.add(flat(Color::linear_rgb(1.0, 0.0, 0.0))),
    };
    if probe.dots {
        for (dot, material, layer) in [
            (ProbeDot::BeamStart, &assets.yellow, FIRST_PERSON_LAYER),
            (ProbeDot::Anchor, &assets.red, 0),
            (ProbeDot::BeamEnd, &assets.yellow, FIRST_PERSON_LAYER),
        ] {
            commands.spawn((
                dot,
                Mesh3d(assets.ball.clone()),
                MeshMaterial3d(material.clone()),
                RenderLayers::layer(layer),
                Transform::IDENTITY,
                Visibility::Hidden,
                NotShadowCaster,
                NotShadowReceiver,
                NoFrustumCulling,
            ));
        }
    }
    commands.insert_resource(assets);
}

/// Gives the socket node its dot when the hand's scene has spawned.
fn mark_socket(
    mut commands: Commands,
    probe: Res<Probe>,
    assets: Option<Res<ProbeAssets>>,
    sockets: Query<Entity, Added<BeamSocket>>,
) {
    let Some(assets) = assets else {
        return;
    };
    if !probe.dots {
        return;
    }
    for socket in &sockets {
        commands.entity(socket).with_child((
            ProbeDot::Socket,
            Mesh3d(assets.ball.clone()),
            MeshMaterial3d(assets.magenta.clone()),
            RenderLayers::layer(FIRST_PERSON_LAYER),
            Transform::IDENTITY,
            Visibility::Hidden,
            NotShadowCaster,
            NotShadowReceiver,
            NoFrustumCulling,
        ));
    }
}

/// `nohand`: the hand's meshes are not drawn (everything else of the hand's
/// layer is: the beam, the flash, the dots).
#[allow(clippy::type_complexity)]
fn hide_hand(
    probe: Res<Probe>,
    mut meshes: Query<
        (&RenderLayers, &mut Visibility),
        (
            With<Mesh3d>,
            Without<BeamRibbon>,
            Without<ProbeDot>,
            Without<bursts::BurstMesh>,
        ),
    >,
) {
    if !probe.hide_hand {
        return;
    }
    let overlay = RenderLayers::layer(FIRST_PERSON_LAYER);
    for (layers, mut visibility) in &mut meshes {
        if *layers == overlay && *visibility != Visibility::Hidden {
            *visibility = Visibility::Hidden;
        }
    }
}

/// The pixel of normalised device coordinates in a viewport of `size`
/// pixels (x right, y down, the origin at the top left).
fn pixel_of_ndc(ndc: DVec2, size: DVec2) -> DVec2 {
    DVec2::new((ndc.x * 0.5 + 0.5) * size.x, (0.5 - ndc.y * 0.5) * size.y)
}

/// The pixel a camera with this transform and projection matrix
/// (`clip_from_view`) draws a world point on, in a viewport of `size`
/// pixels; `None` at or behind the eye.
///
/// In double precision: the levels are kilometres from the origin, where a
/// single-precision world coordinate is up to half a millimetre coarse, a
/// whole pixel at the hand's distance in a window 1920 wide. The numbers that go in (the
/// transform, the projection, the mesh's vertices) are the single-precision
/// ones the renderer is given; nothing is lost on top of them here.
fn project(
    world_from_view: Affine3A,
    clip_from_view: Mat4,
    world: Vec3,
    size: Vec2,
) -> Option<Vec2> {
    let view_from_world = Mat4::from(world_from_view).as_dmat4().inverse();
    let clip = clip_from_view.as_dmat4() * view_from_world * world.as_dvec3().extend(1.0);
    if !clip.is_finite() || clip.w <= 1.0e-9 {
        return None;
    }
    let ndc = DVec2::new(clip.x, clip.y) / clip.w;
    Some(pixel_of_ndc(ndc, size.as_dvec2()).as_vec2())
}

/// The pixel a camera draws a world point on, with the camera's own
/// projection matrix; `None` behind the eye or without a viewport.
fn pixel(camera: &Camera, transform: &GlobalTransform, world: Vec3) -> Option<Vec2> {
    let size = camera.physical_viewport_size()?.as_vec2();
    project(transform.affine(), camera.clip_from_view(), world, size)
}

/// The world radius of a ball that is `pixels` in radius on screen at
/// `world`, for a camera; `None` behind the eye or without a viewport.
fn radius_at(
    camera: &Camera,
    transform: &GlobalTransform,
    world: Vec3,
    pixels: f32,
) -> Option<f32> {
    // Far enough aside to be many single-precision steps of a world
    // coordinate away, near enough to stay on screen.
    const ASIDE: f32 = 0.05;
    let centre = pixel(camera, transform, world)?;
    let aside = pixel(
        camera,
        transform,
        world + transform.right().as_vec3() * ASIDE,
    )?;
    let per_unit = centre.distance(aside) / ASIDE;
    (per_unit.is_finite() && per_unit > 0.0).then(|| pixels / per_unit)
}

/// The middles of the first and the last row of a ribbon's mesh (world
/// space); `None` for a mesh without a ribbon in it.
fn row_middles(positions: &[[f32; 3]]) -> Option<(Vec3, Vec3)> {
    let n = positions.len();
    if n < 4 || !n.is_multiple_of(2) {
        return None;
    }
    let at = |k: usize| Vec3::from_array(positions[k]);
    Some(((at(0) + at(1)) * 0.5, (at(n - 2) + at(n - 1)) * 0.5))
}

/// The actor's placement as the renderer moves its meshes by.
fn placement(sim: &Sim, actor: u32) -> Option<(asamu_core::glam::Vec3, [i32; 3])> {
    let script = sim.script.as_ref()?;
    script
        .moved_actors()
        .into_iter()
        .find(|(id, _, _)| *id == actor)
        .map(|(_, location, rotation)| (location, rotation))
        .or_else(|| script.actor_placement(actor))
}

fn show(dot: (&mut Transform, &mut GlobalTransform, &mut Visibility), at: Option<(Vec3, f32)>) {
    let (transform, global, visibility) = dot;
    match at {
        Some((position, radius)) if position.is_finite() && radius.is_finite() => {
            *transform = Transform::from_translation(position).with_scale(Vec3::splat(radius));
            *global = GlobalTransform::from(*transform);
            *visibility = Visibility::Inherited;
        }
        _ => *visibility = Visibility::Hidden,
    }
}

/// Measures this frame's beam and places the dots.
#[allow(clippy::too_many_arguments, clippy::type_complexity)]
fn measure(
    sim: Option<Res<Sim>>,
    time: Res<Time>,
    state: Res<VfxState>,
    meshes: Res<Assets<Mesh>>,
    view: Query<(&Camera, &GlobalTransform), With<PlayerCamera>>,
    hands: Query<(&Camera, &GlobalTransform, &BeamView)>,
    sockets: Query<&GlobalTransform, With<BeamSocket>>,
    ribbons: Query<(&BeamRibbon, &Mesh3d, &RenderLayers)>,
    mut dots: Query<
        (
            &ProbeDot,
            &mut Transform,
            &mut GlobalTransform,
            &mut Visibility,
        ),
        (
            Without<BeamSocket>,
            Without<PlayerCamera>,
            Without<BeamView>,
        ),
    >,
    mut frame: Local<u64>,
    mut rock: Local<Option<RockPoint>>,
    mut was_on: Local<bool>,
) {
    *frame += 1;
    let hand = hands.iter().next();
    let on = state.beam_on;
    if on != *was_on {
        info!(
            "vfx probe: f={} t={:.3} the beam goes {} (hand shown: {:?})",
            *frame,
            time.elapsed_secs(),
            if on { "on" } else { "off" },
            hand.map(|h| h.2.hand_shown),
        );
        *was_on = on;
    }
    let world_view = view.iter().next();
    let anchor = sim.as_ref().and_then(|s| beam_anchor(s)).map(to_render);
    let socket = sockets.iter().next();
    let measured = (|| {
        if !on {
            return None;
        }
        let (view_camera, view_tf) = world_view?;
        let anchor = anchor?;
        // The camera that draws the ribbons: the hand's when they are on its
        // layer, else the world's.
        let on_overlay = ribbons
            .iter()
            .next()
            .is_some_and(|(_, _, layers)| *layers == RenderLayers::layer(FIRST_PERSON_LAYER));
        let (draw_camera, draw_tf) = match hand {
            Some((camera, tf, _)) if on_overlay => (camera, tf),
            _ => (view_camera, view_tf),
        };
        let socket_pixel = match (hand, socket) {
            (Some((camera, tf, _)), Some(socket)) => pixel(camera, tf, socket.translation()),
            _ => None,
        };
        let anchor_pixel = pixel(view_camera, view_tf, anchor);
        let mut first_strand = None;
        let mut worst_start = None::<f32>;
        let mut worst_end = None::<f32>;
        for (ribbon, mesh, _) in &ribbons {
            let Some((start, end)) = meshes
                .get(&mesh.0)
                .and_then(|m| {
                    m.try_attribute_option(Mesh::ATTRIBUTE_POSITION)
                        .ok()
                        .flatten()
                })
                .and_then(|v| v.as_float3())
                .and_then(row_middles)
            else {
                continue;
            };
            let start_pixel = pixel(draw_camera, draw_tf, start);
            let end_pixel = pixel(draw_camera, draw_tf, end);
            if let (Some(a), Some(b)) = (socket_pixel, start_pixel) {
                worst_start = Some(worst_start.map_or(a.distance(b), |w| w.max(a.distance(b))));
            }
            if let (Some(a), Some(b)) = (anchor_pixel, end_pixel) {
                worst_end = Some(worst_end.map_or(a.distance(b), |w| w.max(a.distance(b))));
            }
            if ribbon.index == 0 {
                first_strand = Some((start, end, start_pixel, end_pixel));
            }
        }
        let (start, end, start_pixel, end_pixel) = first_strand?;
        Some((
            (view_camera, view_tf, draw_camera, draw_tf),
            (start, end, anchor),
            (socket_pixel, start_pixel, anchor_pixel, end_pixel),
            (worst_start, worst_end),
        ))
    })();

    // The point of the drawn rock under the anchor.
    let follow = sim
        .as_ref()
        .and_then(|s| s.game.player().script.gun.follow.map(|f| (f.actor, s)));
    let rock_now = match (on, follow, anchor) {
        (true, Some((actor, sim)), Some(anchor)) => {
            let now = placement(sim, actor);
            if rock.is_none_or(|r| r.actor != actor)
                && let Some(placement) = now
            {
                *rock = Some(RockPoint {
                    actor,
                    placement,
                    anchor,
                });
            }
            match (*rock, now) {
                (Some(r), Some(now)) => {
                    Some(crate::kismet::render_delta(r.placement, now).transform_point3(r.anchor))
                }
                _ => None,
            }
        }
        _ => {
            *rock = None;
            None
        }
    };

    let fmt =
        |p: Option<Vec2>| p.map_or_else(|| "-".to_owned(), |p| format!("{:.2},{:.2}", p.x, p.y));
    let num = |d: Option<f32>| d.map_or_else(|| "-".to_owned(), |d| format!("{d:.3}"));
    if let Some((cameras, _, pixels, worst)) = &measured {
        let (view_camera, view_tf, _, _) = *cameras;
        let (socket_pixel, start_pixel, anchor_pixel, end_pixel) = *pixels;
        let rock_pixel = rock_now.and_then(|p| pixel(view_camera, view_tf, p));
        let rock_error = match (rock_pixel, end_pixel) {
            (Some(a), Some(b)) => Some(a.distance(b)),
            _ => None,
        };
        let size = view_camera
            .physical_viewport_size()
            .map_or_else(|| "-".to_owned(), |s| format!("{}x{}", s.x, s.y));
        info!(
            "vfx probe: f={} t={:.3} vp={} socket={} start={} d_start={} anchor={} end={} d_end={} \
             rock={} d_rock={}",
            *frame,
            time.elapsed_secs(),
            size,
            fmt(socket_pixel),
            fmt(start_pixel),
            num(worst.0),
            fmt(anchor_pixel),
            fmt(end_pixel),
            num(worst.1),
            fmt(rock_pixel),
            num(rock_error),
        );
    }

    // The dots.
    let socket_scale = socket.map(|s| s.compute_transform().scale.max_element());
    for (dot, mut transform, mut global, mut visibility) in &mut dots {
        let Some((cameras, points, _, _)) = &measured else {
            if *visibility != Visibility::Hidden {
                *visibility = Visibility::Hidden;
            }
            continue;
        };
        let (view_camera, view_tf, draw_camera, draw_tf) = *cameras;
        let (start, end, anchor) = *points;
        match dot {
            ProbeDot::Socket => {
                // A child of the socket node: only its size is set here (the
                // parent's scale taken out), one frame late.
                let radius = match (hand, socket, socket_scale) {
                    (Some((camera, tf, _)), Some(socket), Some(scale)) if scale > 0.0 => {
                        radius_at(camera, tf, socket.translation(), LARGE_DOT_PIXELS)
                            .map(|r| r / scale)
                    }
                    _ => None,
                };
                match radius {
                    Some(r) if r.is_finite() => {
                        transform.translation = Vec3::ZERO;
                        transform.scale = Vec3::splat(r);
                        *visibility = Visibility::Inherited;
                    }
                    _ => *visibility = Visibility::Hidden,
                }
            }
            ProbeDot::BeamStart => {
                // Towards the drawing camera's eye along the line of sight.
                let eye = draw_tf.translation();
                let pulled = eye + (start - eye) * START_DOT_PULL;
                let at =
                    radius_at(draw_camera, draw_tf, pulled, SMALL_DOT_PIXELS).map(|r| (pulled, r));
                show((&mut *transform, &mut *global, &mut *visibility), at);
            }
            ProbeDot::Anchor => {
                let at =
                    radius_at(view_camera, view_tf, anchor, LARGE_DOT_PIXELS).map(|r| (anchor, r));
                show((&mut *transform, &mut *global, &mut *visibility), at);
            }
            ProbeDot::BeamEnd => {
                let at = radius_at(draw_camera, draw_tf, end, SMALL_DOT_PIXELS).map(|r| (end, r));
                show((&mut *transform, &mut *global, &mut *visibility), at);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_probe_is_off_unless_asked_for() {
        for off in [
            None,
            Some(""),
            Some(" "),
            Some("0"),
            Some("off"),
            Some("OFF"),
        ] {
            assert_eq!(Probe::parse(off), None, "{off:?}");
        }
        assert_eq!(
            Probe::parse(Some("1")),
            Some(Probe {
                dots: true,
                hide_hand: false
            })
        );
        assert_eq!(
            Probe::parse(Some(" NoHand ")),
            Some(Probe {
                dots: true,
                hide_hand: true
            })
        );
        assert_eq!(
            Probe::parse(Some("log")),
            Some(Probe {
                dots: false,
                hide_hand: false
            })
        );
    }

    #[test]
    fn pixels_of_device_coordinates() {
        let size = DVec2::new(1280.0, 720.0);
        assert_eq!(pixel_of_ndc(DVec2::ZERO, size), DVec2::new(640.0, 360.0));
        assert_eq!(pixel_of_ndc(DVec2::new(-1.0, 1.0), size), DVec2::ZERO);
        assert_eq!(pixel_of_ndc(DVec2::new(1.0, -1.0), size), size);
    }

    /// A point straight ahead is in the middle of the picture, one at the
    /// edge of a 90° view on the edge, one behind the eye nowhere: also for
    /// a camera two kilometres from the origin, where single precision would
    /// lose most of a pixel.
    #[test]
    fn a_camera_far_from_the_origin_projects_exactly() {
        use bevy::camera::{CameraProjection, PerspectiveProjection};
        let size = Vec2::new(1920.0, 1080.0);
        let projection = PerspectiveProjection {
            fov: 2.0 * (9.0f32 / 16.0).atan(),
            aspect_ratio: 16.0 / 9.0,
            near: 0.01,
            ..Default::default()
        };
        let clip_from_view = projection.get_clip_from_view();
        let camera = Affine3A::from_rotation_translation(
            Quat::from_euler(EulerRot::YXZ, 0.7, -0.3, 0.0),
            Vec3::new(2105.5, 1108.6, 978.9),
        );
        let at =
            |local: Vec3| project(camera, clip_from_view, camera.transform_point3(local), size);
        // The world points are rounded to single precision (a quarter of a
        // millimetre at this place); at 30 m that is far below a pixel.
        let middle = at(Vec3::new(0.0, 0.0, -30.0)).unwrap();
        assert!(middle.distance(Vec2::new(960.0, 540.0)) < 0.05, "{middle}");
        let edge = at(Vec3::new(30.0, 0.0, -30.0)).unwrap();
        assert!(
            (edge.x - 1920.0).abs() < 0.05 && (edge.y - 540.0).abs() < 0.05,
            "{edge}"
        );
        assert_eq!(at(Vec3::new(0.0, 0.0, 5.0)), None);
        // Near the origin the hand's distance is exact too.
        let near = project(
            Affine3A::IDENTITY,
            clip_from_view,
            Vec3::new(0.33, 0.0, -0.33),
            size,
        );
        assert!(
            near.is_some_and(|p| (p.x - 1920.0).abs() < 1.0e-3),
            "{near:?}"
        );
    }

    #[test]
    fn the_middles_of_a_ribbons_first_and_last_row() {
        let ribbon = [
            [0.0, 0.0, 0.0],
            [2.0, 0.0, 0.0],
            [0.0, 5.0, 0.0],
            [2.0, 5.0, 0.0],
            [0.0, 9.0, 4.0],
            [2.0, 9.0, 6.0],
        ];
        assert_eq!(
            row_middles(&ribbon),
            Some((Vec3::new(1.0, 0.0, 0.0), Vec3::new(1.0, 9.0, 5.0)))
        );
        // The mesh before the first beam (one degenerate triangle) and odd
        // input have no rows.
        assert_eq!(row_middles(&[[0.0; 3]; 3]), None);
        assert_eq!(row_middles(&[]), None);
        assert_eq!(row_middles(&ribbon[..5]), None);
    }
}
