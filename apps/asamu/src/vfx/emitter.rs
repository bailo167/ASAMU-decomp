//! Where the grapple beam leaves the hand, and how a beam between a point
//! on the hand and a point in the world is drawn when the two are seen
//! through different fields of view.
//!
//! # What the original does
//!
//! The gun's beam component (`MyBeam`, a `UDKParticleSystemComponent`) is
//! attached to the hand mesh's socket `GrappleSocket` (`BeamSocketName`),
//! drawn in the foreground depth group, and given the hand mesh's own field
//! of view, 70° (CONFIRMED (cdo, src); VFX_DECALS.md §2). The hand mesh is
//! drawn with that field of view while the world keeps the player's: its
//! scene proxy passes the mesh through the hand's projection and back
//! through the inverse of the view's (`FUDKSkeletalMeshSceneProxy::
//! GetWorldMatrices`, CONFIRMED (native)). The beam component does the same
//! to its own transform (`UUDKParticleSystemComponent::
//! SetTransformedToWorld`: the owning player's view point and field of view,
//! the component's field of view, the near clipping plane; CONFIRMED
//! (native)). With two perspective projections that differ in their field of
//! view only, that product is, in view space, a scale of the two axes across
//! the view by `tan(view / 2) / tan(hand / 2)` and nothing along it.
//!
//! So the beam's source is the socket **where the drawn hand shows it**,
//! expressed for the world's projection; its target (`GrappleBeamEnd`, an
//! absolute world point) is not touched; and the beam between them is an
//! ordinary world-space beam, drawn after the world and its depth, together
//! with the hand: the world never hides it, the hand does where the hand is
//! nearer.
//!
//! # What this does
//!
//! The same picture from the other side. The hand is drawn by an overlay
//! camera with the hand's field of view that clears depth
//! (`crate::npc::hands`), which is this renderer's foreground depth group.
//! The beam is computed in the world's view space ("view space" below) with
//! its source at the socket mapped there ([`FovMap::hand_to_view`]), and
//! every vertex is then mapped into the hand camera's space
//! ([`FovMap::view_to_hand`]) and drawn by the hand's camera. The source
//! vertex therefore **is** the socket, in the pass that draws the hand, at
//! any window size, field of view and zoom and in every frame of the hand's
//! animation; and a world point keeps the pixel the world camera gives it
//! (both mappings are exact; the tests at the end pin them with the
//! renderer's own projection matrices).

#[cfg(test)]
use bevy::math::{Mat4, Vec4Swizzles};
use bevy::math::{Vec2, Vec3};
use bevy::prelude::Component;

/// Name of the socket's node in the converted hand mesh: the gun's
/// `BeamSocketName`, `GrappleSocket` (CONFIRMED (cdo)), as the skeletal
/// importer names a socket's node (`SOCKET_<Name>`, SKELETAL.md).
pub(crate) const SOCKET_NODE: &str = "SOCKET_GrappleSocket";

/// The node of the hand mesh the beam leaves from ([`SOCKET_NODE`]); put on
/// it by `crate::npc::hands` when the hand's scene has spawned.
#[derive(Component, Debug, Default)]
pub(crate) struct BeamSocket;

/// The camera that draws the hand, with what the beam needs to know about
/// the hand this frame. Put on the overlay camera by `crate::npc::hands`.
#[derive(Component, Debug, Default)]
pub(crate) struct BeamView {
    /// The hand is on screen this frame.
    pub hand_shown: bool,
}

/// Half extents of a perspective view at unit depth: `(tan(fov / 2) ·
/// aspect, tan(fov / 2))` for a vertical field of view `fov` (radians).
#[must_use]
pub(crate) fn half_extents(vertical_fov: f32, aspect: f32) -> Vec2 {
    let t = (vertical_fov * 0.5).tan();
    Vec2::new(t * aspect, t)
}

/// The mapping between the hand camera's space and the world camera's
/// (both camera-local: x right, y up, −z forward, the eye at the origin).
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct FovMap {
    /// View half extents over hand half extents, per axis.
    scale: Vec2,
}

impl FovMap {
    /// The identity (one camera draws everything).
    pub(crate) const IDENTITY: Self = Self { scale: Vec2::ONE };

    /// The map for a world view and a hand view with these half extents
    /// ([`half_extents`]). Degenerate input gives the identity.
    #[must_use]
    pub(crate) fn new(view: Vec2, hand: Vec2) -> Self {
        let scale = view / hand;
        if scale.is_finite() && scale.x > 1e-4 && scale.y > 1e-4 {
            Self { scale }
        } else {
            Self::IDENTITY
        }
    }

    /// A point of the hand camera's space as the world camera would have to
    /// see it to put it on the same pixel (the original's remapping of the
    /// beam component's transform).
    #[must_use]
    pub(crate) fn hand_to_view(self, p: Vec3) -> Vec3 {
        Vec3::new(p.x * self.scale.x, p.y * self.scale.y, p.z)
    }

    /// The inverse: a point of the world camera's space as the hand camera
    /// has to draw it to put it on the pixel the world camera gives it.
    #[must_use]
    pub(crate) fn view_to_hand(self, p: Vec3) -> Vec3 {
        Vec3::new(p.x / self.scale.x, p.y / self.scale.y, p.z)
    }

    /// A direction of the hand camera's space in the world camera's
    /// (the linear part of [`Self::hand_to_view`]), normalised; zero for a
    /// zero direction.
    #[must_use]
    pub(crate) fn hand_dir_to_view(self, d: Vec3) -> Vec3 {
        self.hand_to_view(d).normalize_or_zero()
    }
}

/// The pixel a camera-local point lands on, for a projection matrix
/// (`clip_from_view`) and a viewport size in pixels; `None` at or behind the
/// eye. (For the tests that pin the mappings with the renderer's own
/// projection matrices.)
#[cfg(test)]
#[must_use]
pub(crate) fn pixel_of(clip_from_view: Mat4, p: Vec3, viewport: Vec2) -> Option<Vec2> {
    let clip = clip_from_view * p.extend(1.0);
    if clip.w.is_nan() || clip.w <= 1e-6 {
        return None;
    }
    let ndc = clip.xy() / clip.w;
    Some(Vec2::new(
        (ndc.x * 0.5 + 0.5) * viewport.x,
        (0.5 - ndc.y * 0.5) * viewport.y,
    ))
}

/// The size of one pixel at `depth` in front of a camera whose view is
/// `half_height` tall at unit depth (the y of [`half_extents`]) and
/// `viewport_height` pixels high.
#[must_use]
pub(crate) fn pixel_size(depth: f32, half_height: f32, viewport_height: f32) -> f32 {
    if viewport_height > 0.0 {
        2.0 * depth.max(0.0) * half_height / viewport_height
    } else {
        0.0
    }
}

#[cfg(test)]
mod tests {
    use bevy::camera::{CameraProjection, PerspectiveProjection};

    use super::*;

    /// Bevy's vertical field of view for a horizontal one (UE3's
    /// convention), as `sync_camera` and `update_hands` set it.
    fn vertical(horizontal_degrees: f32, aspect: f32) -> f32 {
        2.0 * ((horizontal_degrees.to_radians() * 0.5).tan() / aspect).atan()
    }

    fn projection(horizontal_degrees: f32, aspect: f32, near: f32) -> PerspectiveProjection {
        PerspectiveProjection {
            fov: vertical(horizontal_degrees, aspect),
            aspect_ratio: aspect,
            near,
            far: 1000.0,
            ..Default::default()
        }
    }

    /// Windows of several shapes, the world's field of view at its default,
    /// zoomed in (story mode) and widened by the user's setting.
    const WINDOWS: [(f32, f32); 5] = [
        (1280.0, 720.0),
        (1920.0, 1080.0),
        (1600.0, 1200.0),
        (2560.0, 1080.0),
        (800.0, 1280.0),
    ];
    const VIEW_FOVS: [f32; 5] = [90.0, 30.0, 60.0, 110.0, 70.0];
    const HAND_FOV: f32 = 70.0;

    /// Places on the hand: the socket's rest place as the converted mesh has
    /// it in front of the eye (metres, camera-local), and others across the
    /// lower right of the view while the hand animates.
    const SOCKETS: [Vec3; 5] = [
        Vec3::new(0.21, -0.19, -0.45),
        Vec3::new(0.05, -0.02, -0.30),
        Vec3::new(0.35, -0.30, -0.62),
        Vec3::new(-0.10, -0.25, -0.20),
        Vec3::new(0.00, 0.00, -0.50),
    ];

    /// The beam's start, mapped into the world's view, is on the pixel the
    /// hand camera draws the socket on; and it is the socket again when it
    /// is drawn by the hand camera.
    #[test]
    fn the_source_projects_onto_the_socket_at_any_window_and_fov() {
        let mut worst = 0.0f32;
        for (w, h) in WINDOWS {
            let viewport = Vec2::new(w, h);
            let aspect = w / h;
            let hand = projection(HAND_FOV, aspect, 0.01);
            for fov in VIEW_FOVS {
                let view = projection(fov, aspect, 0.05);
                let map = FovMap::new(
                    half_extents(view.fov, view.aspect_ratio),
                    half_extents(hand.fov, hand.aspect_ratio),
                );
                for socket in SOCKETS {
                    let on_hand = pixel_of(hand.get_clip_from_view(), socket, viewport)
                        .expect("in front of the eye");
                    let source = map.hand_to_view(socket);
                    let in_view = pixel_of(view.get_clip_from_view(), source, viewport)
                        .expect("in front of the eye");
                    let error = on_hand.distance(in_view);
                    worst = worst.max(error);
                    assert!(
                        error < 0.05,
                        "{w}x{h} fov {fov}: socket {socket} at {on_hand}, source at {in_view}"
                    );
                    // Depth is kept, so the hand hides what is behind it.
                    assert_eq!(source.z, socket.z);
                    let back = map.view_to_hand(source);
                    assert!(back.distance(socket) < 1e-6, "{back} vs {socket}");
                    let drawn = pixel_of(hand.get_clip_from_view(), back, viewport)
                        .expect("in front of the eye");
                    assert!(drawn.distance(on_hand) < 0.05);
                }
            }
        }
        eprintln!("worst source error: {worst} pixels");
    }

    /// The beam's far end, drawn by the hand camera, is on the pixel the
    /// world camera puts the anchor on: near, far, off to the side and at
    /// the edge of the view.
    #[test]
    fn the_anchor_keeps_its_pixel_when_the_hand_camera_draws_it() {
        let anchors = [
            Vec3::new(0.0, 0.0, -3.0),
            Vec3::new(1.5, 2.0, -12.0),
            Vec3::new(-20.0, 6.0, -49.0),
            Vec3::new(3.0, -1.0, -3.2),
            Vec3::new(0.4, 9.0, -30.0),
        ];
        for (w, h) in WINDOWS {
            let viewport = Vec2::new(w, h);
            let aspect = w / h;
            let hand = projection(HAND_FOV, aspect, 0.01);
            for fov in VIEW_FOVS {
                let view = projection(fov, aspect, 0.05);
                let map = FovMap::new(
                    half_extents(view.fov, view.aspect_ratio),
                    half_extents(hand.fov, hand.aspect_ratio),
                );
                for anchor in anchors {
                    let in_world = pixel_of(view.get_clip_from_view(), anchor, viewport)
                        .expect("in front of the eye");
                    let drawn = pixel_of(
                        hand.get_clip_from_view(),
                        map.view_to_hand(anchor),
                        viewport,
                    )
                    .expect("in front of the eye");
                    assert!(
                        in_world.distance(drawn) < 0.25,
                        "{w}x{h} fov {fov}: anchor {anchor} at {in_world}, drawn at {drawn}"
                    );
                }
            }
        }
    }

    /// A straight line of the world's view is a straight line of the hand's
    /// (the mapping is linear), so a beam computed in one and drawn in the
    /// other does not bend.
    #[test]
    fn straight_stays_straight() {
        let map = FovMap::new(half_extents(1.0, 1.6), half_extents(0.7, 1.6));
        let a = Vec3::new(0.2, -0.2, -0.4);
        let b = Vec3::new(-3.0, 4.0, -25.0);
        let middle = map.view_to_hand(a.lerp(b, 0.37));
        let line = map.view_to_hand(a).lerp(map.view_to_hand(b), 0.37);
        assert!(middle.distance(line) < 1e-5);
        // Directions map with the same scale and come out normalised.
        let d = map.hand_dir_to_view(Vec3::new(0.3, 0.1, -1.0));
        assert!((d.length() - 1.0).abs() < 1e-6);
        assert_eq!(map.hand_dir_to_view(Vec3::ZERO), Vec3::ZERO);
    }

    #[test]
    fn equal_views_and_bad_input_are_the_identity() {
        let e = half_extents(1.2, 1.5);
        assert_eq!(FovMap::new(e, e), FovMap::IDENTITY);
        for bad in [Vec2::ZERO, Vec2::new(f32::NAN, 1.0), Vec2::new(1.0, -1.0)] {
            assert_eq!(FovMap::new(e, bad), FovMap::IDENTITY, "{bad}");
            assert_eq!(FovMap::new(bad, e), FovMap::IDENTITY, "{bad}");
        }
        let p = Vec3::new(1.0, 2.0, -3.0);
        assert_eq!(FovMap::IDENTITY.hand_to_view(p), p);
        assert_eq!(FovMap::IDENTITY.view_to_hand(p), p);
    }

    #[test]
    fn pixels() {
        let p = projection(90.0, 16.0 / 9.0, 0.05);
        let viewport = Vec2::new(1280.0, 720.0);
        let centre = pixel_of(p.get_clip_from_view(), Vec3::new(0.0, 0.0, -5.0), viewport);
        assert!(centre.is_some_and(|c| c.distance(Vec2::new(640.0, 360.0)) < 1e-3));
        // 90° across: the right edge is at x = depth.
        let edge = pixel_of(p.get_clip_from_view(), Vec3::new(5.0, 0.0, -5.0), viewport);
        assert!(edge.is_some_and(|c| (c.x - 1280.0).abs() < 1e-2));
        assert_eq!(
            pixel_of(p.get_clip_from_view(), Vec3::new(0.0, 0.0, 1.0), viewport),
            None
        );
        // One pixel at 5 m: the view is 2 · 5 · tan(vfov / 2) tall.
        let half = half_extents(p.fov, p.aspect_ratio);
        let size = pixel_size(5.0, half.y, 720.0);
        assert!((size * 720.0 - 2.0 * 5.0 * half.y).abs() < 1e-5);
        assert_eq!(pixel_size(5.0, half.y, 0.0), 0.0);
        assert_eq!(pixel_size(-1.0, half.y, 720.0), 0.0);
    }
}
