//! Particle drawing: one mesh per emitter, rebuilt every frame from the
//! simulator's sprites as camera-facing (or velocity- / axis-aligned) quads
//! in render space, with one unlit material per particle material.

use asamu_assets::BlendMode;
use asamu_assets::particles::{
    EmitterRender, ScreenAlignment, Sprite, SubUvMethod, sub_image_rect,
};
use asamu_core::coords::{ue_dir_to_bevy, ue_pos_to_bevy};
use asamu_core::glam as sim_glam;
use bevy::asset::RenderAssetUsages;
use bevy::camera::visibility::NoFrustumCulling;
use bevy::light::{NotShadowCaster, NotShadowReceiver};
use bevy::mesh::{Indices, PrimitiveTopology};
use bevy::prelude::*;

use super::material::{SpriteMaterial, sprite_material};
use super::{ParticleRuntime, Phase, visible};
use crate::converted::RenderLevels;
use crate::kismet::Presentation;
use crate::{PlayerCamera, Sim};

/// Marker on a particle mesh entity (effect index, emitter index).
#[derive(Component, Debug, Clone, Copy, PartialEq, Eq)]
pub struct ParticleMesh(pub usize, pub usize);

/// Render units per UE unit.
fn render_scale() -> f32 {
    let one = ue_pos_to_bevy(sim_glam::Vec3::new(0.0, 1.0, 0.0), crate::SCALE);
    one.length()
}

fn to_vec3(v: sim_glam::Vec3) -> Vec3 {
    Vec3::new(v.x, v.y, v.z)
}

/// The camera frame used to orient sprites (render space).
#[derive(Debug, Clone, Copy)]
pub struct View {
    /// Camera position.
    pub eye: Vec3,
    /// Camera right.
    pub right: Vec3,
    /// Camera up.
    pub up: Vec3,
    /// Camera forward.
    pub forward: Vec3,
}

/// Quad corners of one sprite in render space (counter-clockwise from the
/// lower left as seen by the camera), or `None` for a degenerate sprite.
pub fn corners(s: &Sprite, r: &EmitterRender, view: &View, scale: f32) -> Option<[Vec3; 4]> {
    let center = to_vec3(ue_pos_to_bevy(s.position, crate::SCALE));
    if !center.is_finite() {
        return None;
    }
    let (w, h) = match r.alignment {
        ScreenAlignment::Rectangle | ScreenAlignment::Velocity => (s.size[0], s.size[1]),
        _ => (s.size[0], s.size[0]),
    };
    let (w, h) = (w.abs() * scale * 0.5, h.abs() * scale * 0.5);
    if !(w.is_finite() && h.is_finite()) || (w == 0.0 && h == 0.0) {
        return None;
    }
    let (right, up) = match (r.alignment, r.axis_lock.as_deref()) {
        (_, Some(lock)) if lock != "EPAL_NONE" => axis_frame(lock, center, view),
        (ScreenAlignment::Velocity, _) => {
            let v = to_vec3(ue_dir_to_bevy(s.velocity));
            let up = v.normalize_or_zero();
            if up == Vec3::ZERO {
                (view.right, view.up)
            } else {
                let to_eye = (view.eye - center).normalize_or_zero();
                let right = up.cross(to_eye).normalize_or_zero();
                if right == Vec3::ZERO {
                    (view.right, view.up)
                } else {
                    (right, up)
                }
            }
        }
        _ => {
            let (sin, cos) = s.rotation.sin_cos();
            (
                view.right * cos + view.up * sin,
                view.up * cos - view.right * sin,
            )
        }
    };
    let (rx, uy) = (right * w, up * h);
    Some([
        center - rx - uy,
        center + rx - uy,
        center + rx + uy,
        center - rx + uy,
    ])
}

/// Orientation for `ParticleModuleOrientationAxisLock` (TENTATIVE: locked
/// axes face along the UE axis; the rotate variants turn about it towards
/// the camera).
fn axis_frame(lock: &str, center: Vec3, view: &View) -> (Vec3, Vec3) {
    let axis = |a: sim_glam::Vec3| to_vec3(ue_dir_to_bevy(a));
    let (x, y, z) = (
        axis(sim_glam::Vec3::X),
        axis(sim_glam::Vec3::Y),
        axis(sim_glam::Vec3::Z),
    );
    let face = |n: Vec3, right: Vec3| (right, n.cross(right).normalize_or_zero());
    let rotate_about = |a: Vec3| {
        let to_eye = (view.eye - center).normalize_or_zero();
        let right = a.cross(to_eye).normalize_or_zero();
        if right == Vec3::ZERO {
            (view.right, view.up)
        } else {
            (right, a)
        }
    };
    match lock {
        "EPAL_X" | "EPAL_NEGATIVE_X" => face(x, y),
        "EPAL_Y" | "EPAL_NEGATIVE_Y" => face(y, x),
        "EPAL_Z" | "EPAL_NEGATIVE_Z" => face(z, y),
        "EPAL_ROTATE_X" => rotate_about(x),
        "EPAL_ROTATE_Y" => rotate_about(y),
        "EPAL_ROTATE_Z" => rotate_about(z),
        _ => (view.right, view.up),
    }
}

/// Mesh buffers of one emitter.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Buffers {
    /// Positions.
    pub positions: Vec<[f32; 3]>,
    /// UVs.
    pub uvs: Vec<[f32; 2]>,
    /// Linear RGBA colours.
    pub colors: Vec<[f32; 4]>,
    /// Normals (towards the camera).
    pub normals: Vec<[f32; 3]>,
    /// Indices.
    pub indices: Vec<u32>,
}

/// Build the quads of `r` (sorted back to front when `sort`).
pub fn build(r: &EmitterRender, view: &View, sort: bool) -> Buffers {
    let scale = render_scale();
    let mut order: Vec<usize> = (0..r.sprites.len()).collect();
    if sort {
        let dist = |i: &usize| {
            r.sprites.get(*i).map_or(0.0, |s| {
                to_vec3(ue_pos_to_bevy(s.position, crate::SCALE)).distance_squared(view.eye)
            })
        };
        order.sort_by(|a, b| dist(b).total_cmp(&dist(a)));
    }
    let mut b = Buffers::default();
    for i in order {
        let Some(s) = r.sprites.get(i) else { continue };
        let Some(c) = corners(s, r, view, scale) else {
            continue;
        };
        let image = if r.sub_uv == SubUvMethod::LinearBlend && s.blend >= 0.5 {
            s.next_image
        } else {
            s.sub_image
        };
        let [u0, v0, u1, v1] = sub_image_rect(image, r.sub_images);
        let Ok(base) = u32::try_from(b.positions.len()) else {
            break;
        };
        let color = s
            .color
            .map(|x| if x.is_finite() { x.max(0.0) } else { 0.0 });
        let n = -view.forward;
        for (p, uv) in c.iter().zip([[u0, v1], [u1, v1], [u1, v0], [u0, v0]]) {
            b.positions.push(p.to_array());
            b.uvs.push(uv);
            b.colors.push(color);
            b.normals.push(n.to_array());
        }
        b.indices
            .extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
    }
    // Beams: one camera-facing strip per segment.
    for seg in &r.beams {
        let start = to_vec3(ue_pos_to_bevy(seg.start, crate::SCALE));
        let end = to_vec3(ue_pos_to_bevy(seg.end, crate::SCALE));
        let along = end - start;
        let mid = (start + end) * 0.5;
        let side =
            along.cross(view.eye - mid).normalize_or_zero() * (seg.width.abs() * scale * 0.5);
        if !(start.is_finite() && end.is_finite()) || side == Vec3::ZERO {
            continue;
        }
        let Ok(base) = u32::try_from(b.positions.len()) else {
            break;
        };
        let color = seg
            .color
            .map(|x| if x.is_finite() { x.max(0.0) } else { 0.0 });
        let n = -view.forward;
        let quad = [start - side, end - side, end + side, start + side];
        for (p, uv) in quad
            .iter()
            .zip([[0.0, 1.0], [1.0, 1.0], [1.0, 0.0], [0.0, 0.0]])
        {
            b.positions.push(p.to_array());
            b.uvs.push(uv);
            b.colors.push(color);
            b.normals.push(n.to_array());
        }
        b.indices
            .extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
    }
    b
}

fn fill(mesh: &mut Mesh, b: Buffers) {
    mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, b.positions);
    mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, b.normals);
    mesh.insert_attribute(Mesh::ATTRIBUTE_UV_0, b.uvs);
    mesh.insert_attribute(Mesh::ATTRIBUTE_COLOR, b.colors);
    mesh.insert_indices(Indices::U32(b.indices));
}

fn mesh_of(b: Buffers) -> Mesh {
    let mut mesh = Mesh::new(
        PrimitiveTopology::TriangleList,
        RenderAssetUsages::default(),
    );
    fill(&mut mesh, b);
    mesh
}

/// Rebuild every emitter mesh from the simulation.
#[allow(clippy::too_many_arguments)]
pub(super) fn draw(
    mut commands: Commands,
    server: Res<AssetServer>,
    sim: Option<Res<Sim>>,
    pres: Option<Res<Presentation>>,
    levels: Option<Res<RenderLevels>>,
    camera: Query<&GlobalTransform, With<PlayerCamera>>,
    mut rt: ResMut<ParticleRuntime>,
    mut meshes: ResMut<Assets<Mesh>>,
    materials: Option<ResMut<Assets<SpriteMaterial>>>,
    mut entities: Query<(&Mesh3d, &mut Visibility), With<ParticleMesh>>,
) {
    if !matches!(rt.phase, Phase::Ready) {
        return;
    }
    // Without a renderer the sprite material is not registered.
    let Some(mut materials) = materials else {
        return;
    };
    let Some(cam) = camera.iter().next() else {
        return;
    };
    let view = View {
        eye: cam.translation(),
        right: cam.right().as_vec3(),
        up: cam.up().as_vec3(),
        forward: cam.forward().as_vec3(),
    };
    let rt = &mut *rt;
    for (ei, effect) in rt.effects.iter_mut().enumerate() {
        let shown = visible(effect, sim.as_deref(), pres.as_deref(), levels.as_deref());
        let renders = if shown {
            effect.instance.render()
        } else {
            Vec::new()
        };
        // Hide every emitter mesh first; the ones with sprites show below.
        for ent in effect.entities.iter().flatten() {
            if let Ok((_, mut v)) = entities.get_mut(*ent) {
                *v = Visibility::Hidden;
            }
        }
        for r in renders {
            if r.sprites.is_empty() && r.beams.is_empty() {
                continue;
            }
            let Some(slot) = effect.entities.get_mut(r.emitter) else {
                continue;
            };
            let key = r.material.as_deref().unwrap_or("").to_ascii_lowercase();
            let render_material = rt.materials.get(&key);
            let blend = render_material.map(|m| m.blend);
            let sort = r.sorted || matches!(blend, Some(BlendMode::Translucent));
            let buffers = build(&r, &view, sort);
            if buffers.indices.is_empty() {
                continue;
            }
            let entity = match slot {
                Some(e) => *e,
                None => {
                    let handle = rt
                        .handles
                        .entry(key.clone())
                        .or_insert_with(|| {
                            let m = render_material.cloned().unwrap_or_else(|| {
                                asamu_assets::particles::ParticleMaterials::default()
                                    .get(r.material.as_deref().unwrap_or(""), None)
                            });
                            materials.add(sprite_material(&m, &server, &mut rt.images))
                        })
                        .clone();
                    let e = commands
                        .spawn((
                            Mesh3d(meshes.add(mesh_of(buffers))),
                            MeshMaterial3d(handle),
                            Transform::IDENTITY,
                            Visibility::Visible,
                            NoFrustumCulling,
                            NotShadowCaster,
                            NotShadowReceiver,
                            ParticleMesh(ei, r.emitter),
                        ))
                        .id();
                    *slot = Some(e);
                    continue;
                }
            };
            let Ok((mesh3d, mut vis)) = entities.get_mut(entity) else {
                continue;
            };
            let Some(mut mesh) = meshes.get_mut(&mesh3d.0) else {
                continue;
            };
            fill(&mut mesh, buffers);
            *vis = Visibility::Visible;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use asamu_assets::particles::EmitterKind;

    fn view() -> View {
        View {
            eye: Vec3::new(0.0, 0.0, 10.0),
            right: Vec3::X,
            up: Vec3::Y,
            forward: Vec3::NEG_Z,
        }
    }

    fn sprite(size: [f32; 2], rotation: f32) -> Sprite {
        Sprite {
            position: sim_glam::Vec3::ZERO,
            size,
            rotation,
            color: [1.0, 0.5, 0.25, 0.5],
            velocity: sim_glam::Vec3::new(0.0, 0.0, 100.0),
            sub_image: 3,
            next_image: 0,
            blend: 0.0,
        }
    }

    fn emitter(alignment: ScreenAlignment, sprites: Vec<Sprite>) -> EmitterRender {
        EmitterRender {
            emitter: 0,
            kind: EmitterKind::Sprite,
            material: None,
            alignment,
            sub_images: [2, 2],
            sub_uv: SubUvMethod::Linear,
            axis_lock: None,
            sorted: false,
            sprites,
            beams: Vec::new(),
        }
    }

    #[test]
    fn square_sprites_face_the_camera_at_full_width() {
        let scale = render_scale();
        let r = emitter(ScreenAlignment::Square, vec![sprite([100.0, 7.0], 0.0)]);
        let c = corners(&r.sprites[0], &r, &view(), scale).unwrap();
        // Square: width and height both Size.X (100 UU → 2 render units).
        let w = c[1] - c[0];
        let h = c[3] - c[0];
        assert!((w.length() - 100.0 * scale).abs() < 1e-5);
        assert!((h.length() - 100.0 * scale).abs() < 1e-5);
        assert!(w.dot(Vec3::Y).abs() < 1e-6, "width along the camera right");
        // A quarter turn swaps the axes.
        let r2 = emitter(
            ScreenAlignment::Rectangle,
            vec![sprite([100.0, 20.0], std::f32::consts::FRAC_PI_2)],
        );
        let c2 = corners(&r2.sprites[0], &r2, &view(), scale).unwrap();
        let w2 = c2[1] - c2[0];
        assert!(w2.dot(Vec3::X).abs() < 1e-5, "rotated width runs along up");
        assert!((w2.length() - 100.0 * scale).abs() < 1e-5);
    }

    #[test]
    fn velocity_sprites_stretch_along_the_motion() {
        let scale = render_scale();
        let r = emitter(ScreenAlignment::Velocity, vec![sprite([10.0, 80.0], 0.0)]);
        let c = corners(&r.sprites[0], &r, &view(), scale).unwrap();
        let up = c[3] - c[0];
        // UE +Z is render +Y.
        assert!(up.normalize().dot(Vec3::Y) > 0.999);
        assert!((up.length() - 80.0 * scale).abs() < 1e-5);
    }

    #[test]
    fn buffers_hold_one_quad_per_sprite_with_sub_image_uvs() {
        let r = emitter(
            ScreenAlignment::Square,
            vec![sprite([10.0, 10.0], 0.0), sprite([0.0, 0.0], 0.0)],
        );
        let b = build(&r, &view(), true);
        assert_eq!(b.positions.len(), 4, "the zero-size sprite is skipped");
        assert_eq!(b.indices, vec![0, 1, 2, 0, 2, 3]);
        // Sub image 3 of 2 × 2: the lower right quarter.
        assert_eq!(b.uvs[0], [0.5, 1.0]);
        assert_eq!(b.uvs[2], [1.0, 0.5]);
        assert_eq!(b.colors[0], [1.0, 0.5, 0.25, 0.5]);
    }

    #[test]
    fn beams_draw_as_strips_between_their_ends() {
        use asamu_assets::particles::BeamSegment;
        let mut r = emitter(ScreenAlignment::Square, Vec::new());
        r.beams.push(BeamSegment {
            start: sim_glam::Vec3::ZERO,
            end: sim_glam::Vec3::new(0.0, 500.0, 0.0),
            width: 50.0,
            color: [0.0, 1.0, 1.0, 1.0],
        });
        let b = build(&r, &view(), false);
        assert_eq!(b.positions.len(), 4);
        let scale = render_scale();
        // UE +Y is render +X: the strip runs 10 render units along X, 1 wide.
        let p: Vec<Vec3> = b.positions.iter().map(|p| Vec3::from_array(*p)).collect();
        assert!(((p[1] - p[0]).length() - 500.0 * scale).abs() < 1e-4);
        assert!(((p[3] - p[0]).length() - 50.0 * scale).abs() < 1e-4);
        assert!((p[1] - p[0]).normalize().dot(Vec3::X).abs() > 0.999);
    }
}
