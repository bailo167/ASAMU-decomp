//! The grapple's two one-shot particle effects: the flash at the hand when
//! the gun attaches and the impact at the anchor (VFX_DECALS.md §2).
//!
//! What the original does (CONFIRMED (src, cdo)):
//!
//! - **at the hand**: the gun's `grappleBeginEffect` component (template
//!   [`IGNITE_SYSTEM`], attached to the beam's socket) is activated on every
//!   attach. It is a plain particle component: unlike the beam it is not in
//!   the foreground depth group and has no field of view of its own, so it
//!   is drawn with the world, where the socket *is*, not where the hand mesh
//!   (drawn with its narrower field of view) shows it: about a third nearer
//!   to the middle of the screen than the drawn lens, and behind the hand.
//! - **at the anchor**: `ImpactParticleSystem` ([`CONNECT_SYSTEM`]) is
//!   spawned through the emitter pool at the hit location, turned to the hit
//!   normal and attached to the hit actor.
//!
//! Both systems are run here by the particle simulator the placed effects
//! use (`asamu_assets::particles`: spawn rates, lifetimes, size and colour
//! curves, in 1/60 s steps) and drawn as camera-facing sprites with their
//! converted textures, added to the picture.
//!
//! PLACEHOLDER, what is not drawn or not as the original has it:
//!
//! - the systems' **mesh emitters** (the impact's four emitters of a small
//!   sphere mesh that implodes) and their **distortion emitters** (a
//!   material that only refracts what is behind it) are left out: this
//!   renderer has neither particle meshes nor refraction;
//! - a material's flip-book texture is shown whole, and an opacity that
//!   comes from a second texture is the particle model's one mask
//!   (PARTICLES.md);
//! - the impact follows a moving anchor by translation only, and only while
//!   the gun stays attached to it;
//! - the flash at the hand is drawn by the hand's camera, behind the hand,
//!   on the pixels the world camera would put it on ([`PUSH_BACK`]): drawn by
//!   the world camera it would be fogged as if it were as far away as the
//!   wall behind it.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use asamu_assets::particles::{
    EmitterKind, EmitterRender, InstanceParams, ParticleLibrary, ParticleMaterial,
    ParticleMaterials, ScreenAlignment, SystemDef, SystemInstance,
};
use asamu_assets::{BlendMode, manifest::TextureManifest};
use asamu_core::coords::{bevy_dir_to_ue, bevy_pos_to_ue, ue_dir_to_bevy};
use asamu_core::glam as sim_glam;
use bevy::asset::uuid_handle;
use bevy::math::Affine3A;
use bevy::prelude::*;
use bevy::render::render_resource::{AsBindGroup, ShaderType};
use bevy::shader::ShaderRef;

use super::emitter::FovMap;
use crate::{SCALE, to_render};

/// Template of the gun's `grappleBeginEffect` component. CONFIRMED (cdo).
pub const IGNITE_SYSTEM: &str = "Sigge.Particles.Beam_Ignite_PS";
/// The gun's `ImpactParticleSystem`. CONFIRMED (cdo).
pub const CONNECT_SYSTEM: &str = "Sigge.Particles.Beam_Connect_PS";
/// Simulation step, s (the placed effects' step: a fixed-time system moves
/// as the original does at 60 frames a second).
pub const STEP: f32 = 1.0 / 60.0;
/// Most simulation steps per frame (a long frame is not caught up beyond).
const MAX_STEPS_PER_FRAME: u32 = 6;
/// Impacts alive at once (ours: the emitter pool has no such limit; an
/// impact lives a second and the gun refires no faster than that).
pub const MAX_IMPACTS: usize = 4;
/// How many times further from the eye than it is the hand's flash is
/// drawn, by the hand's camera (ours): far enough to be behind the hand,
/// and scaled about the eye so that every pixel stays where it was.
pub const PUSH_BACK: f32 = 64.0;

/// The two systems and the materials of their emitters, from the converted
/// data.
pub struct BurstData {
    ignite: Option<Arc<SystemDef>>,
    connect: Option<Arc<SystemDef>>,
    /// By lower-case material path.
    materials: HashMap<String, ParticleMaterial>,
}

impl BurstData {
    /// Reads the systems from a converted root. `Ok(None)` when particles
    /// are not converted or neither system is there.
    ///
    /// # Errors
    /// A particle or material file that exists but cannot be read.
    pub fn load(root: &Path, textures: Option<&TextureManifest>) -> Result<Option<Self>, String> {
        let Some(library) = ParticleLibrary::load(root).map_err(|e| e.to_string())? else {
            return Ok(None);
        };
        let (ignite, connect) = (
            library.get(IGNITE_SYSTEM).cloned(),
            library.get(CONNECT_SYSTEM).cloned(),
        );
        if ignite.is_none() && connect.is_none() {
            return Ok(None);
        }
        let all = ParticleMaterials::load(root).map_err(|e| e.to_string())?;
        let mut materials = HashMap::new();
        for system in [&ignite, &connect].into_iter().flatten() {
            for emitter in &system.emitters {
                for lod in &emitter.lods {
                    if let Some(path) = lod.required.material.as_deref() {
                        materials
                            .entry(path.to_ascii_lowercase())
                            .or_insert_with(|| all.get(path, textures));
                    }
                }
            }
        }
        Ok(Some(Self {
            ignite,
            connect,
            materials,
        }))
    }
}

/// An impact at an anchor.
struct Impact {
    instance: SystemInstance,
    /// Where it was spawned (UU).
    anchor: sim_glam::Vec3,
    /// It follows the gun's anchor (until the gun lets go or grapples
    /// again).
    following: bool,
    /// Its slot among the impacts' meshes.
    slot: usize,
}

/// Which mesh entity: the hand's flash or an impact slot, and the emitter.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum MeshKey {
    Ignite(usize),
    Impact(usize, usize),
}

/// The running effects.
#[derive(Resource, Default)]
pub struct Bursts {
    data: Option<BurstData>,
    ignite: Option<SystemInstance>,
    impacts: Vec<Impact>,
    accumulator: f32,
    spawned: u64,
    materials: HashMap<String, Handle<GlowMaterial>>,
    entities: HashMap<MeshKey, Entity>,
}

impl Bursts {
    /// Takes the converted systems (a new level, or the first load).
    pub fn set_data(&mut self, data: Option<BurstData>) {
        self.ignite = data.as_ref().and_then(|d| d.ignite.clone()).map(|def| {
            SystemInstance::new(def, sim_glam::Mat4::IDENTITY, InstanceParams::default(), 1)
        });
        self.impacts.clear();
        self.data = data;
    }

    /// A new game: nothing is burning.
    pub fn reset(&mut self) {
        if let Some(ignite) = &mut self.ignite {
            ignite.reset();
        }
        self.impacts.clear();
        self.accumulator = 0.0;
    }

    /// The gun attached at `anchor` (UU) on a surface with `normal`.
    pub fn attached(&mut self, anchor: sim_glam::Vec3, normal: sim_glam::Vec3) {
        if let Some(ignite) = &mut self.ignite {
            ignite.activate();
        }
        for impact in &mut self.impacts {
            impact.following = false;
        }
        let Some(def) = self.data.as_ref().and_then(|d| d.connect.clone()) else {
            return;
        };
        if self.impacts.len() >= MAX_IMPACTS {
            self.impacts.remove(0);
        }
        let slot = (0..MAX_IMPACTS)
            .find(|s| self.impacts.iter().all(|i| i.slot != *s))
            .unwrap_or(0);
        self.spawned = self.spawned.wrapping_add(1);
        let mut instance = SystemInstance::new(
            def,
            impact_transform(anchor, normal),
            InstanceParams::default(),
            0x1A7C ^ self.spawned,
        );
        instance.activate();
        self.impacts.push(Impact {
            instance,
            anchor,
            following: true,
            slot,
        });
    }

    /// Advances the effects by `dt` seconds of a running game. `anchor` is
    /// where the gun's anchor is now while it is attached (`None`: it let
    /// go); `viewer` the eye (UU), for the systems' level of detail.
    pub fn advance(&mut self, dt: f32, anchor: Option<sim_glam::Vec3>, viewer: sim_glam::Vec3) {
        for impact in &mut self.impacts {
            match anchor {
                Some(now) if impact.following => {
                    let mut t = impact.instance.transform;
                    t.w_axis = now.extend(1.0);
                    impact.instance.transform = t;
                    impact.anchor = now;
                }
                _ => impact.following = false,
            }
        }
        if !dt.is_finite() || dt <= 0.0 {
            return;
        }
        self.accumulator = (self.accumulator + dt).min(STEP * MAX_STEPS_PER_FRAME as f32);
        while self.accumulator >= STEP {
            self.accumulator -= STEP;
            if let Some(ignite) = &mut self.ignite {
                ignite.tick(STEP, Some(viewer));
            }
            for impact in &mut self.impacts {
                impact.instance.tick(STEP, Some(viewer));
            }
        }
        self.impacts.retain(|i| !i.instance.is_idle());
    }

    /// Live particles (hand, anchors).
    #[cfg(test)]
    #[must_use]
    pub fn particle_counts(&self) -> (usize, usize) {
        (
            self.ignite
                .as_ref()
                .map_or(0, SystemInstance::particle_count),
            self.impacts
                .iter()
                .map(|i| i.instance.particle_count())
                .sum(),
        )
    }
}

/// The transform of an impact (UE space, column vectors): at `anchor`, its
/// X axis along the hit normal, no roll (the original turns the effect with
/// the rotator of the normal).
#[must_use]
pub fn impact_transform(anchor: sim_glam::Vec3, normal: sim_glam::Vec3) -> sim_glam::Mat4 {
    let x = normal.normalize_or_zero();
    let x = if x == sim_glam::Vec3::ZERO {
        sim_glam::Vec3::X
    } else {
        x
    };
    let flat = sim_glam::Vec3::new(-x.y, x.x, 0.0).normalize_or_zero();
    let y = if flat == sim_glam::Vec3::ZERO {
        sim_glam::Vec3::Y
    } else {
        flat
    };
    let z = x.cross(y);
    sim_glam::Mat4::from_cols(
        x.extend(0.0),
        y.extend(0.0),
        z.extend(0.0),
        anchor.extend(1.0),
    )
}

/// The transform of the hand's flash (UE space): the socket's place and
/// axes, from the socket node's render-space position and its forward,
/// right and up directions (the node's forward axis is the socket's X).
#[must_use]
pub fn socket_transform(position: Vec3, forward: Vec3, right: Vec3, up: Vec3) -> sim_glam::Mat4 {
    let ue = |v: Vec3| sim_glam::Vec3::new(v.x, v.y, v.z);
    let dir = |v: Vec3| bevy_dir_to_ue(ue(v)).normalize_or_zero();
    sim_glam::Mat4::from_cols(
        dir(forward).extend(0.0),
        dir(right).extend(0.0),
        dir(up).extend(0.0),
        bevy_pos_to_ue(ue(position), SCALE).extend(1.0),
    )
}

/// How sprites are placed for the camera that draws them.
#[derive(Clone, Copy, Debug)]
pub struct QuadView {
    /// The eye (render space).
    pub eye: Vec3,
    /// The camera's right.
    pub right: Vec3,
    /// The camera's up.
    pub up: Vec3,
    /// When the hand's camera draws the sprites: the world camera's space
    /// from the world, the mapping to the hand camera's, and the world from
    /// the hand camera's ([`PUSH_BACK`] is applied between the two).
    pub pushed: Option<(Affine3A, FovMap, Affine3A)>,
}

impl QuadView {
    fn place(&self, p: Vec3) -> Vec3 {
        match &self.pushed {
            Some((view_from_world, map, world_from_hand)) => world_from_hand.transform_point3(
                map.view_to_hand(view_from_world.transform_point3(p)) * PUSH_BACK,
            ),
            None => p,
        }
    }
}

/// Mesh buffers of one emitter's sprites.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Quads {
    /// Positions (render space).
    pub positions: Vec<[f32; 3]>,
    /// Texture coordinates.
    pub uvs: Vec<[f32; 2]>,
    /// Colours (linear RGBA).
    pub colors: Vec<[f32; 4]>,
    /// Triangles.
    pub indices: Vec<u32>,
}

/// The sprites of `r` as quads facing the camera of `view`: a sprite's size
/// is its full extent; square emitters use the first size for both sides;
/// velocity-aligned sprites lie along their velocity; the others are turned
/// by their rotation about the view axis. Degenerate sprites are left out.
#[must_use]
pub fn quads(r: &EmitterRender, view: &QuadView) -> Quads {
    let mut q = Quads::default();
    let unit = SCALE.bevy_units_per_uu;
    for s in &r.sprites {
        let center = to_render(s.position);
        let (w, h) = match r.alignment {
            ScreenAlignment::Rectangle | ScreenAlignment::Velocity => (s.size[0], s.size[1]),
            _ => (s.size[0], s.size[0]),
        };
        let (w, h) = (w.abs() * unit * 0.5, h.abs() * unit * 0.5);
        if !center.is_finite() || !(w.is_finite() && h.is_finite()) || (w == 0.0 && h == 0.0) {
            continue;
        }
        let turned = || {
            let (sin, cos) = s.rotation.sin_cos();
            (
                view.right * cos + view.up * sin,
                view.up * cos - view.right * sin,
            )
        };
        let (right, up) = if r.alignment == ScreenAlignment::Velocity {
            let v = ue_dir_to_bevy(s.velocity);
            let up = Vec3::new(v.x, v.y, v.z).normalize_or_zero();
            let right = up
                .cross((view.eye - center).normalize_or_zero())
                .normalize_or_zero();
            if up == Vec3::ZERO || right == Vec3::ZERO {
                (view.right, view.up)
            } else {
                (right, up)
            }
        } else {
            turned()
        };
        let (rx, uy) = (right * w, up * h);
        let corners = [
            center - rx - uy,
            center + rx - uy,
            center + rx + uy,
            center - rx + uy,
        ]
        .map(|c| view.place(c));
        if corners.iter().any(|c| !c.is_finite()) {
            continue;
        }
        let base = q.positions.len() as u32;
        q.positions.extend(corners.map(|c| c.to_array()));
        q.uvs
            .extend([[0.0, 1.0], [1.0, 1.0], [1.0, 0.0], [0.0, 0.0]]);
        q.colors.extend([s.color; 4]);
        q.indices
            .extend([base, base + 1, base + 2, base, base + 2, base + 3]);
    }
    q
}

/// Whether an emitter's render data is drawn here: sprites of a sprite
/// emitter whose material adds light (see the module docs for what is left
/// out).
#[must_use]
pub fn drawn(r: &EmitterRender, material: Option<&ParticleMaterial>) -> bool {
    r.kind == EmitterKind::Sprite
        && !r.sprites.is_empty()
        && material.is_some_and(|m| {
            m.converted
                && matches!(m.blend, BlendMode::Additive)
                && (m.texture.is_some() || m.color.iter().any(|c| *c > 0.0))
        })
}

// ---------------------------------------------------------------------------
// Material.
// ---------------------------------------------------------------------------

/// Handle of the glow sprites' fragment shader.
pub const GLOW_SHADER: Handle<Shader> = uuid_handle!("2b7d9e04-51c6-4a3f-8e2d-c9a1f05b7d18");

/// The fragment shader: the displayed texture times the material's colour
/// and the particle's, times an opacity mask from a second texture's
/// channels, added to the picture. Inputs as the beam's shader.
pub const GLOW_WGSL: &str = r"
struct GlowParams {
    color: vec4<f32>,
    mask_channels: vec4<f32>,
    // x: added to the mask.
    mask_bias: vec4<f32>,
};

@group(#{MATERIAL_BIND_GROUP}) @binding(0) var<uniform> params: GlowParams;
@group(#{MATERIAL_BIND_GROUP}) @binding(1) var color_texture: texture_2d<f32>;
@group(#{MATERIAL_BIND_GROUP}) @binding(2) var color_sampler: sampler;
@group(#{MATERIAL_BIND_GROUP}) @binding(3) var mask_texture: texture_2d<f32>;
@group(#{MATERIAL_BIND_GROUP}) @binding(4) var mask_sampler: sampler;

struct GlowInput {
    @location(2) uv: vec2<f32>,
    @location(5) color: vec4<f32>,
};

@fragment
fn fragment(in: GlowInput) -> @location(0) vec4<f32> {
    let texel = textureSample(color_texture, color_sampler, in.uv);
    let mask_texel = textureSample(mask_texture, mask_sampler, in.uv);
    let mask = clamp(dot(mask_texel, params.mask_channels) + params.mask_bias.x, 0.0, 1.0);
    let alpha = clamp(params.color.a * mask * in.color.a, 0.0, 1.0);
    let rgb = params.color.rgb * texel.rgb * in.color.rgb * alpha;
    // Premultiplied blending with alpha 0 adds `rgb`.
    return vec4<f32>(rgb, 0.0);
}
";

/// Uniforms of [`GlowMaterial`].
#[derive(ShaderType, Clone, Copy, Debug, PartialEq)]
pub struct GlowParams {
    /// Linear colour multiplier; alpha is the constant opacity factor.
    pub color: Vec4,
    /// Channel weights of the opacity mask.
    pub mask_channels: Vec4,
    /// x: added to the mask (1 when there is no mask texture).
    pub mask_bias: Vec4,
}

impl GlowParams {
    /// The uniforms of a particle material.
    #[must_use]
    pub fn of(m: &ParticleMaterial) -> Self {
        let [r, g, b] = m.color;
        // A mask texture that could not be bound leaves the mask at 1.
        let (channels, bias) = if m.opacity_texture.is_some() {
            (m.opacity_channels, m.opacity_bias)
        } else {
            ([0.0; 4], 1.0)
        };
        Self {
            color: Vec4::new(r, g, b, m.opacity),
            mask_channels: Vec4::from_array(channels),
            mask_bias: Vec4::new(bias, 0.0, 0.0, 0.0),
        }
    }
}

/// The glow sprites' material.
#[derive(Asset, TypePath, AsBindGroup, Clone, Debug)]
pub struct GlowMaterial {
    /// Uniforms.
    #[uniform(0)]
    pub params: GlowParams,
    /// Displayed texture (white when absent).
    #[texture(1)]
    #[sampler(2)]
    pub color_texture: Option<Handle<Image>>,
    /// Opacity mask texture (white when absent).
    #[texture(3)]
    #[sampler(4)]
    pub mask_texture: Option<Handle<Image>>,
}

impl Material for GlowMaterial {
    fn fragment_shader() -> ShaderRef {
        ShaderRef::Handle(GLOW_SHADER)
    }

    fn alpha_mode(&self) -> AlphaMode {
        AlphaMode::Premultiplied
    }

    fn enable_prepass() -> bool {
        false
    }

    fn enable_shadows() -> bool {
        false
    }
}

/// Registers the material and its shader (no-op without a renderer).
pub fn register(app: &mut App) {
    let Some(mut shaders) = app.world_mut().get_resource_mut::<Assets<Shader>>() else {
        return;
    };
    let source = GLOW_WGSL.replace(
        "#{MATERIAL_BIND_GROUP}",
        &bevy::pbr::MATERIAL_BIND_GROUP_INDEX.to_string(),
    );
    if let Err(e) = shaders.insert(
        &GLOW_SHADER,
        Shader::from_wgsl(source, "asamu/vfx/glow.wgsl"),
    ) {
        warn!("vfx: glow shader not registered: {e}");
        return;
    }
    app.add_plugins(MaterialPlugin::<GlowMaterial>::default());
}

// ---------------------------------------------------------------------------
// Drawing.
// ---------------------------------------------------------------------------

/// A mesh of glow sprites.
#[derive(Component)]
pub struct BurstMesh;

fn fill(mesh: &mut Mesh, q: Quads) {
    let n = q.positions.len();
    mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, q.positions);
    mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, vec![[0.0f32, 1.0, 0.0]; n]);
    mesh.insert_attribute(Mesh::ATTRIBUTE_UV_0, q.uvs);
    mesh.insert_attribute(Mesh::ATTRIBUTE_COLOR, q.colors);
    mesh.insert_indices(bevy::mesh::Indices::U32(q.indices));
}

/// What [`draw`] needs from the frame's cameras and hand.
pub struct DrawFrame {
    /// The world camera's sprites frame.
    pub world: QuadView,
    /// The hand's flash: the socket's transform (UE space) and the view its
    /// sprites are drawn with, with the layer of the camera that draws them;
    /// `None` when it is not drawn this frame.
    pub hand: Option<(sim_glam::Mat4, QuadView, usize)>,
}

/// Rebuilds the sprite meshes of the running effects.
#[allow(clippy::too_many_arguments)]
pub fn draw(
    commands: &mut Commands,
    server: &AssetServer,
    images: &mut HashMap<String, Handle<Image>>,
    bursts: &mut Bursts,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<GlowMaterial>,
    existing: &mut Query<
        (
            &Mesh3d,
            &mut Visibility,
            &mut bevy::camera::visibility::RenderLayers,
        ),
        With<BurstMesh>,
    >,
    frame: &DrawFrame,
) {
    use bevy::camera::visibility::{NoFrustumCulling, RenderLayers};
    let Bursts {
        data,
        ignite,
        impacts,
        materials: handles,
        entities,
        ..
    } = bursts;
    let Some(data) = data.as_ref() else {
        return;
    };
    for entity in entities.values() {
        if let Ok((_, mut v, _)) = existing.get_mut(*entity)
            && *v != Visibility::Hidden
        {
            *v = Visibility::Hidden;
        }
    }
    let mut jobs: Vec<(MeshKey, EmitterRender, QuadView, usize)> = Vec::new();
    if let (Some(ignite), Some((transform, view, layer))) = (ignite.as_mut(), frame.hand.as_ref()) {
        ignite.transform = *transform;
        for r in ignite.render() {
            jobs.push((MeshKey::Ignite(r.emitter), r, *view, *layer));
        }
    }
    for impact in impacts.iter() {
        for r in impact.instance.render() {
            jobs.push((MeshKey::Impact(impact.slot, r.emitter), r, frame.world, 0));
        }
    }
    for (key, r, view, layer) in jobs {
        let path = r.material.as_deref().unwrap_or("").to_ascii_lowercase();
        let material = data.materials.get(&path);
        if !drawn(&r, material) {
            continue;
        }
        let Some(material) = material else { continue };
        let q = quads(&r, &view);
        if q.indices.is_empty() {
            continue;
        }
        let layer = RenderLayers::layer(layer);
        if let Some(entity) = entities.get(&key)
            && let Ok((mesh, mut visibility, mut layers)) = existing.get_mut(*entity)
        {
            if let Some(mut mesh) = meshes.get_mut(&mesh.0) {
                fill(&mut mesh, q);
            }
            *visibility = Visibility::Inherited;
            if *layers != layer {
                *layers = layer;
            }
            continue;
        }
        let handle = handles
            .entry(path)
            .or_insert_with(|| {
                materials.add(GlowMaterial {
                    params: GlowParams::of(material),
                    color_texture: material
                        .texture
                        .as_ref()
                        .map(|t| super::load_image(server, images, t)),
                    mask_texture: material
                        .opacity_texture
                        .as_ref()
                        .map(|t| super::load_image(server, images, t)),
                })
            })
            .clone();
        let mut mesh = Mesh::new(
            bevy::mesh::PrimitiveTopology::TriangleList,
            bevy::asset::RenderAssetUsages::default(),
        );
        fill(&mut mesh, q);
        let entity = commands
            .spawn((
                BurstMesh,
                Mesh3d(meshes.add(mesh)),
                MeshMaterial3d(handle),
                layer,
                Transform::IDENTITY,
                Visibility::Inherited,
                NoFrustumCulling,
                bevy::light::NotShadowCaster,
                bevy::light::NotShadowReceiver,
            ))
            .id();
        entities.insert(key, entity);
    }
}

#[cfg(test)]
mod tests {
    use asamu_assets::particles::{Sprite, SubUvMethod};

    use super::*;

    fn sprite(position: sim_glam::Vec3, size: [f32; 2]) -> Sprite {
        Sprite {
            position,
            size,
            rotation: 0.0,
            color: [1.0, 0.5, 0.25, 0.8],
            velocity: sim_glam::Vec3::ZERO,
            sub_image: 0,
            next_image: 0,
            blend: 0.0,
        }
    }

    fn render(
        kind: EmitterKind,
        alignment: ScreenAlignment,
        sprites: Vec<Sprite>,
    ) -> EmitterRender {
        EmitterRender {
            emitter: 0,
            kind,
            material: Some("Fx.Glow".to_owned()),
            alignment,
            sub_images: [1, 1],
            sub_uv: SubUvMethod::None,
            axis_lock: None,
            sorted: false,
            sprites,
            beams: Vec::new(),
        }
    }

    fn material(blend: BlendMode, color: [f32; 3], converted: bool) -> ParticleMaterial {
        ParticleMaterial {
            path: "Fx.Glow".to_owned(),
            blend,
            color,
            opacity: 1.0,
            texture: None,
            opacity_texture: None,
            opacity_channels: [0.0; 4],
            opacity_bias: 1.0,
            converted,
        }
    }

    const VIEW: QuadView = QuadView {
        eye: Vec3::ZERO,
        right: Vec3::X,
        up: Vec3::Y,
        pushed: None,
    };

    #[test]
    fn a_sprite_is_a_quad_of_its_full_size_facing_the_camera() {
        let at = sim_glam::Vec3::new(500.0, 0.0, 0.0);
        let r = render(
            EmitterKind::Sprite,
            ScreenAlignment::Square,
            vec![sprite(at, [20.0, 7.0])],
        );
        let q = quads(&r, &VIEW);
        assert_eq!((q.positions.len(), q.indices.len()), (4, 6));
        let unit = SCALE.bevy_units_per_uu;
        let c: Vec<Vec3> = q.positions.iter().map(|p| Vec3::from_array(*p)).collect();
        // Square: the first size on both sides, 20 UU across and high.
        assert!((c[0].distance(c[1]) - 20.0 * unit).abs() < 1e-5);
        assert!((c[1].distance(c[2]) - 20.0 * unit).abs() < 1e-5);
        let centre = (c[0] + c[2]) * 0.5;
        assert!(centre.distance(to_render(at)) < 1e-5);
        assert!(q.colors.iter().all(|k| *k == [1.0, 0.5, 0.25, 0.8]));
        // Rectangle: both sizes.
        let r = render(
            EmitterKind::Sprite,
            ScreenAlignment::Rectangle,
            vec![sprite(at, [20.0, 7.0])],
        );
        let q = quads(&r, &VIEW);
        let c: Vec<Vec3> = q.positions.iter().map(|p| Vec3::from_array(*p)).collect();
        assert!((c[1].distance(c[2]) - 7.0 * unit).abs() < 1e-5);
        // Degenerate sprites are left out.
        let r = render(
            EmitterKind::Sprite,
            ScreenAlignment::Square,
            vec![
                sprite(at, [0.0, 0.0]),
                sprite(sim_glam::Vec3::NAN, [5.0, 5.0]),
            ],
        );
        assert!(quads(&r, &VIEW).indices.is_empty());
    }

    #[test]
    fn a_velocity_aligned_sprite_lies_along_its_velocity() {
        let mut s = sprite(sim_glam::Vec3::new(300.0, 0.0, 0.0), [3.0, 10.0]);
        s.velocity = sim_glam::Vec3::new(0.0, 0.0, 150.0);
        let r = render(EmitterKind::Sprite, ScreenAlignment::Velocity, vec![s]);
        let q = quads(&r, &VIEW);
        let c: Vec<Vec3> = q.positions.iter().map(|p| Vec3::from_array(*p)).collect();
        let long = (c[3] - c[0]).normalize();
        // UE up is render up.
        assert!(long.dot(Vec3::Y).abs() > 0.999, "{long}");
        assert!((c[3].distance(c[0]) - 10.0 * SCALE.bevy_units_per_uu).abs() < 1e-5);
    }

    /// The hand's flash drawn by the hand's camera: every corner stays on
    /// the pixel the world camera gives it, and is far behind the hand.
    #[test]
    fn a_pushed_sprite_keeps_its_pixels_and_goes_behind_the_hand() {
        use bevy::camera::{CameraProjection, PerspectiveProjection};

        use super::super::emitter::{half_extents, pixel_of};
        let aspect = 16.0 / 9.0;
        let vertical = |across: f32| 2.0 * ((across.to_radians() * 0.5).tan() / aspect).atan();
        let world = PerspectiveProjection {
            fov: vertical(90.0),
            aspect_ratio: aspect,
            ..Default::default()
        };
        let hand = PerspectiveProjection {
            fov: vertical(70.0),
            aspect_ratio: aspect,
            ..Default::default()
        };
        let viewport = Vec2::new(1280.0, 720.0);
        let map = FovMap::new(
            half_extents(world.fov, aspect),
            half_extents(hand.fov, aspect),
        );
        // The camera at the origin looking along −Z: UE +X is render −Z.
        let at = sim_glam::Vec3::new(45.0, 21.0, -19.0);
        let r = render(
            EmitterKind::Sprite,
            ScreenAlignment::Square,
            vec![sprite(at, [8.0, 8.0])],
        );
        let plain = quads(&r, &VIEW);
        let pushed = quads(
            &r,
            &QuadView {
                pushed: Some((Affine3A::IDENTITY, map, Affine3A::IDENTITY)),
                ..VIEW
            },
        );
        for (a, b) in plain.positions.iter().zip(&pushed.positions) {
            let (a, b) = (Vec3::from_array(*a), Vec3::from_array(*b));
            let in_world = pixel_of(world.get_clip_from_view(), a, viewport).unwrap();
            let by_hand = pixel_of(hand.get_clip_from_view(), b, viewport).unwrap();
            assert!(in_world.distance(by_hand) < 0.05, "{in_world} {by_hand}");
            assert!((b.z / a.z - PUSH_BACK).abs() < 1e-3);
        }
    }

    #[test]
    fn only_glowing_sprite_emitters_are_drawn() {
        let one = vec![sprite(sim_glam::Vec3::X, [1.0, 1.0])];
        let glow = material(BlendMode::Additive, [1.0, 1.0, 1.0], true);
        let sprites = render(EmitterKind::Sprite, ScreenAlignment::Square, one.clone());
        assert!(drawn(&sprites, Some(&glow)));
        assert!(!drawn(&sprites, None));
        // A mesh emitter, an empty emitter.
        assert!(!drawn(
            &render(
                EmitterKind::Mesh,
                ScreenAlignment::TypeSpecific,
                one.clone()
            ),
            Some(&glow)
        ));
        assert!(!drawn(
            &render(EmitterKind::Sprite, ScreenAlignment::Square, Vec::new()),
            Some(&glow)
        ));
        // A material that adds no light (the distortion emitters'), one that
        // is not converted, one that is not additive.
        assert!(!drawn(
            &sprites,
            Some(&material(BlendMode::Additive, [0.0; 3], true))
        ));
        assert!(!drawn(
            &sprites,
            Some(&material(BlendMode::Additive, [1.0; 3], false))
        ));
        assert!(!drawn(
            &sprites,
            Some(&material(BlendMode::Translucent, [1.0; 3], true))
        ));
    }

    #[test]
    fn transforms_are_orthonormal_frames_at_their_place() {
        let anchor = sim_glam::Vec3::new(100.0, -200.0, 300.0);
        for normal in [
            sim_glam::Vec3::new(0.0, 0.0, 1.0),
            sim_glam::Vec3::new(1.0, 1.0, 0.2),
            sim_glam::Vec3::new(-0.3, 0.0, -1.0),
            sim_glam::Vec3::ZERO,
            sim_glam::Vec3::NAN,
        ] {
            let t = impact_transform(anchor, normal);
            assert_eq!(t.transform_point3(sim_glam::Vec3::ZERO), anchor);
            let (x, y, z) = (
                t.x_axis.truncate(),
                t.y_axis.truncate(),
                t.z_axis.truncate(),
            );
            for (a, b) in [(x, y), (y, z), (x, z)] {
                assert!(a.dot(b).abs() < 1e-5, "{normal}: {a} {b}");
                assert!((a.length() - 1.0).abs() < 1e-5 && (b.length() - 1.0).abs() < 1e-5);
            }
            if normal.is_finite() && normal != sim_glam::Vec3::ZERO {
                assert!(x.dot(normal.normalize()) > 0.999);
            }
        }
        // The socket: render forward (−Z) is UE +X, right is +Y, up is +Z.
        let t = socket_transform(Vec3::new(1.0, 2.0, -3.0), Vec3::NEG_Z, Vec3::X, Vec3::Y);
        assert!((t.x_axis.truncate() - sim_glam::Vec3::X).length() < 1e-6);
        assert!((t.y_axis.truncate() - sim_glam::Vec3::Y).length() < 1e-6);
        assert!((t.z_axis.truncate() - sim_glam::Vec3::Z).length() < 1e-6);
        let back = to_render(t.transform_point3(sim_glam::Vec3::ZERO));
        assert!(back.distance(Vec3::new(1.0, 2.0, -3.0)) < 1e-5);
    }

    #[test]
    fn nothing_burns_without_converted_systems() {
        let mut b = Bursts::default();
        b.set_data(None);
        b.attached(sim_glam::Vec3::ZERO, sim_glam::Vec3::Z);
        b.advance(0.1, Some(sim_glam::Vec3::X), sim_glam::Vec3::ZERO);
        b.advance(f32::NAN, None, sim_glam::Vec3::ZERO);
        assert_eq!(b.particle_counts(), (0, 0));
        b.reset();
        assert!(b.impacts.is_empty() && b.ignite.is_none());
    }

    /// Real-data check (skipped unless `ASAMU_CONVERTED_DIR` names a
    /// conversion with particles): both systems are there with glowing
    /// sprite emitters; an attach lights the hand's flash and an impact; the
    /// impact rides a moving anchor while the gun holds it and stays behind
    /// after the release; and both burn out by themselves.
    #[test]
    fn the_converted_systems_flash_and_burn_out() {
        let Some(root) = std::env::var_os("ASAMU_CONVERTED_DIR").map(std::path::PathBuf::from)
        else {
            eprintln!("skipped: ASAMU_CONVERTED_DIR is not set");
            return;
        };
        let dir = asamu_assets::ConvertedDir::new(root.clone());
        let manifests = dir.load_manifests().expect("manifests");
        let Some(data) = BurstData::load(&root, manifests.textures.as_ref()).expect("readable")
        else {
            eprintln!("skipped: the grapple's particle systems are not converted");
            return;
        };
        assert!(data.ignite.is_some() && data.connect.is_some());
        let mut b = Bursts::default();
        b.set_data(Some(data));
        let anchor = sim_glam::Vec3::new(1000.0, 0.0, 200.0);
        let eye = sim_glam::Vec3::ZERO;
        b.attached(anchor, sim_glam::Vec3::new(-1.0, 0.0, 0.0));
        let mut peak = (0usize, 0usize);
        let mut glowing = (0usize, 0usize);
        let moved = anchor + sim_glam::Vec3::new(0.0, 50.0, 0.0);
        for step in 0..12 {
            b.advance(STEP, Some(if step < 6 { anchor } else { moved }), eye);
            let (hand, impact) = b.particle_counts();
            peak = (peak.0.max(hand), peak.1.max(impact));
            let data = b.data.as_ref().unwrap();
            let lit = |renders: Vec<EmitterRender>| {
                renders
                    .iter()
                    .filter(|r| {
                        let key = r.material.as_deref().unwrap_or("").to_ascii_lowercase();
                        drawn(r, data.materials.get(&key))
                    })
                    .count()
            };
            glowing.0 = glowing.0.max(lit(b.ignite.as_ref().unwrap().render()));
            glowing.1 = glowing.1.max(lit(b.impacts[0].instance.render()));
        }
        eprintln!(
            "peak particles: hand {}, impact {}; glowing emitters: {glowing:?}",
            peak.0, peak.1
        );
        assert!(peak.0 > 0 && peak.1 > 0, "{peak:?}");
        assert!(glowing.0 >= 3 && glowing.1 >= 2, "{glowing:?}");
        // The impact rode the anchor.
        assert_eq!(b.impacts[0].anchor, moved);
        assert_eq!(
            b.impacts[0]
                .instance
                .transform
                .transform_point3(sim_glam::Vec3::ZERO),
            moved
        );
        // Released: it stays where it is and burns out; so does the flash.
        for _ in 0..180 {
            b.advance(STEP, None, eye);
        }
        assert_eq!(b.particle_counts(), (0, 0));
        assert!(b.impacts.is_empty(), "a finished impact is dropped");
        // Refiring keeps at most MAX_IMPACTS.
        for i in 0..MAX_IMPACTS + 2 {
            b.attached(anchor + sim_glam::Vec3::X * i as f32, sim_glam::Vec3::Z);
        }
        assert_eq!(b.impacts.len(), MAX_IMPACTS);
        assert_eq!(b.impacts.iter().filter(|i| i.following).count(), 1);
        let mut slots: Vec<usize> = b.impacts.iter().map(|i| i.slot).collect();
        slots.sort_unstable();
        slots.dedup();
        assert_eq!(
            slots.len(),
            MAX_IMPACTS,
            "every live impact has its own meshes"
        );
    }
}
