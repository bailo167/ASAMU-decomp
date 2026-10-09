//! Render-world side of the atmosphere: two full-screen passes in Bevy's
//! 3D post-processing chain.
//!
//! 1. **Height fog** (`height_fog.wgsl`), after the main pass and before
//!    Bevy's bloom: the original applies its fog to the HDR scene before
//!    bloom and tonemapping. Reads the view's depth buffer.
//! 2. **Uber blend** (`uber.wgsl`), after bloom and before Bevy's
//!    tonemapping (which the plugin turns off on these cameras): the
//!    original's tonemapper and colour-grading LUT.
//!
//! Both read one per-camera uniform ([`PostFxUniform`]) extracted from the
//! main-world [`PostFx`] component; the LUT is a main-world image. The fog
//! pass also writes a copy of the (fogged) scene before Bevy's bloom
//! ([`PreBloomTexture`]); the uber pass takes Bevy's bloom as the
//! difference and blends it the original's way (tint, scale, fade on bright
//! pixels).

use bevy::asset::Handle;
use bevy::core_pipeline::FullscreenShader;
use bevy::core_pipeline::schedule::{Core3d, Core3dSystems};
use bevy::core_pipeline::tonemapping::tonemapping;
use bevy::ecs::query::QueryItem;
use bevy::post_process::bloom::bloom;
use bevy::prelude::*;
use bevy::render::extract_component::{
    ComponentUniforms, DynamicUniformIndex, ExtractComponent, ExtractComponentPlugin,
    UniformComponentPlugin,
};
use bevy::render::render_asset::RenderAssets;
use bevy::render::render_resource::binding_types::{
    sampler, texture_2d, texture_2d_multisampled, uniform_buffer,
};
use bevy::render::render_resource::{
    AddressMode, BindGroupEntries, BindGroupLayoutDescriptor, BindGroupLayoutEntries,
    CachedRenderPipelineId, ColorTargetState, ColorWrites, FilterMode, FragmentState, LoadOp,
    Operations, PipelineCache, RenderPassColorAttachment, RenderPassDescriptor,
    RenderPipelineDescriptor, Sampler, SamplerBindingType, SamplerDescriptor, ShaderStages,
    ShaderType, SpecializedRenderPipeline, SpecializedRenderPipelines, StoreOp, TextureFormat,
    TextureSampleType,
};
use bevy::render::render_resource::{Extent3d, TextureDescriptor, TextureDimension, TextureUsages};
use bevy::render::renderer::{RenderContext, RenderDevice, ViewQuery};
use bevy::render::sync_component::SyncComponent;
use bevy::render::texture::{CachedTexture, GpuImage, TextureCache};
use bevy::render::view::{ExtractedView, Msaa, ViewDepthStencilTexture, ViewTarget};
use bevy::render::{GpuResourceAppExt, Render, RenderApp, RenderStartup, RenderSystems};
use bevy::shader::Shader;

/// The fog shader template (see [`fog_shader_source`]).
const HEIGHT_FOG_WGSL: &str = include_str!("height_fog.wgsl");
/// The uber blend shader.
const UBER_WGSL: &str = include_str!("uber.wgsl");

/// The fog shader for single- or multi-sampled depth.
#[must_use]
pub fn fog_shader_source(multisampled: bool) -> String {
    let ty = if multisampled {
        "texture_multisampled_2d<f32>"
    } else {
        "texture_2d<f32>"
    };
    HEIGHT_FOG_WGSL.replace("DEPTH_TEXTURE_TYPE", ty)
}

/// Per-camera values of both passes (layout shared with the WGSL `PostFx`
/// struct; see the shaders for each field).
#[derive(Component, ShaderType, Clone, Copy, Debug, Default, PartialEq)]
pub struct PostFxUniform {
    pub world_from_clip: Mat4,
    pub camera: Vec4,
    pub fog: Vec4,
    pub fog_opposite: Vec4,
    pub fog_inscatter: Vec4,
    pub fog_light: Vec4,
    pub tonemap: Vec4,
    pub tonemap2: Vec4,
    pub grade: Vec4,
    pub bloom: Vec4,
}

/// Main-world settings of the atmosphere passes on one camera (written by
/// the plugin every frame).
#[derive(Component, Clone, Debug, Default)]
pub struct PostFx {
    /// Uniform values except the camera matrix (filled at extraction).
    pub uniform: PostFxUniform,
    /// Run the fog pass.
    pub fog: bool,
    /// Run the uber pass (tonemapper + LUT).
    pub uber: bool,
    /// The colour-grading LUT (256 × 16, `Rgba8Unorm`).
    pub lut: Handle<Image>,
}

/// Render-world flags and LUT of a camera with [`PostFx`].
#[derive(Component, Clone, Debug)]
pub struct PostFxPasses {
    fog: bool,
    uber: bool,
    lut: Handle<Image>,
}

impl SyncComponent<RenderApp> for PostFx {
    type Target = (PostFxUniform, PostFxPasses);
}

impl ExtractComponent<RenderApp> for PostFx {
    type QueryData = (&'static PostFx, &'static GlobalTransform, &'static Camera);
    type QueryFilter = ();
    type Out = (PostFxUniform, PostFxPasses);

    fn extract_component(
        (fx, transform, camera): QueryItem<'_, '_, Self::QueryData>,
    ) -> Option<Self::Out> {
        let clip_from_world = camera.clip_from_view() * transform.to_matrix().inverse();
        let world_from_clip = clip_from_world.inverse();
        if !world_from_clip.is_finite() {
            return None;
        }
        let mut uniform = fx.uniform;
        uniform.world_from_clip = world_from_clip;
        uniform.camera = transform.translation().extend(uniform.camera.w);
        Some((
            uniform,
            PostFxPasses {
                fog: fx.fog,
                uber: fx.uber,
                lut: fx.lut.clone(),
            },
        ))
    }
}

/// Shader handles (main-world assets).
#[derive(Resource, Clone)]
struct PostFxShaders {
    fog_single: Handle<Shader>,
    fog_multi: Handle<Shader>,
    uber: Handle<Shader>,
}

/// Registers the render-world passes (no-op without a renderer).
pub struct PostFxRenderPlugin;

impl Plugin for PostFxRenderPlugin {
    fn build(&self, app: &mut App) {
        let Some(mut shaders) = app.world_mut().get_resource_mut::<Assets<Shader>>() else {
            return;
        };
        let handles = PostFxShaders {
            fog_single: shaders.add(Shader::from_wgsl(
                fog_shader_source(false),
                "asamu/post/height_fog.wgsl",
            )),
            fog_multi: shaders.add(Shader::from_wgsl(
                fog_shader_source(true),
                "asamu/post/height_fog_msaa.wgsl",
            )),
            uber: shaders.add(Shader::from_wgsl(UBER_WGSL, "asamu/post/uber.wgsl")),
        };
        app.add_plugins((
            ExtractComponentPlugin::<PostFx>::default(),
            UniformComponentPlugin::<PostFxUniform>::default(),
        ));
        let Some(render_app) = app.get_sub_app_mut(RenderApp) else {
            return;
        };
        render_app
            .insert_resource(handles)
            .init_gpu_resource::<SpecializedRenderPipelines<PostFxPipeline>>()
            .add_systems(RenderStartup, init_pipeline)
            .add_systems(
                Render,
                (
                    prepare_pipelines.in_set(RenderSystems::Prepare),
                    prepare_pre_bloom_textures.in_set(RenderSystems::PrepareResources),
                ),
            )
            .add_systems(
                Core3d,
                (
                    height_fog_pass
                        .in_set(Core3dSystems::PostProcess)
                        .before(bloom),
                    uber_pass
                        .in_set(Core3dSystems::PostProcess)
                        .after(bloom)
                        .before(tonemapping),
                ),
            );
    }
}

/// Pipelines and layouts of both passes.
#[derive(Resource)]
pub struct PostFxPipeline {
    fog_layout: BindGroupLayoutDescriptor,
    fog_layout_msaa: BindGroupLayoutDescriptor,
    uber_layout: BindGroupLayoutDescriptor,
    lut_sampler: Sampler,
    fullscreen: FullscreenShader,
    shaders: PostFxShaders,
}

fn init_pipeline(
    mut commands: Commands,
    device: Res<RenderDevice>,
    fullscreen: Res<FullscreenShader>,
    shaders: Res<PostFxShaders>,
) {
    let fog_entries = |msaa: bool| {
        BindGroupLayoutEntries::sequential(
            ShaderStages::FRAGMENT,
            (
                texture_2d(TextureSampleType::Float { filterable: false }),
                if msaa {
                    texture_2d_multisampled(TextureSampleType::Float { filterable: false })
                } else {
                    texture_2d(TextureSampleType::Float { filterable: false })
                },
                uniform_buffer::<PostFxUniform>(true),
            ),
        )
    };
    let uber_entries = BindGroupLayoutEntries::sequential(
        ShaderStages::FRAGMENT,
        (
            texture_2d(TextureSampleType::Float { filterable: false }),
            texture_2d(TextureSampleType::Float { filterable: false }),
            texture_2d(TextureSampleType::Float { filterable: true }),
            sampler(SamplerBindingType::Filtering),
            uniform_buffer::<PostFxUniform>(true),
        ),
    );
    commands.insert_resource(PostFxPipeline {
        fog_layout: BindGroupLayoutDescriptor::new("asamu_height_fog", &fog_entries(false)),
        fog_layout_msaa: BindGroupLayoutDescriptor::new(
            "asamu_height_fog_msaa",
            &fog_entries(true),
        ),
        uber_layout: BindGroupLayoutDescriptor::new("asamu_uber", &uber_entries),
        lut_sampler: device.create_sampler(&SamplerDescriptor {
            label: Some("asamu_lut_sampler"),
            address_mode_u: AddressMode::ClampToEdge,
            address_mode_v: AddressMode::ClampToEdge,
            address_mode_w: AddressMode::ClampToEdge,
            mag_filter: FilterMode::Linear,
            min_filter: FilterMode::Linear,
            ..default()
        }),
        fullscreen: fullscreen.clone(),
        shaders: shaders.clone(),
    });
}

/// Which pipeline to build.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct PostFxKey {
    format: TextureFormat,
    pass: Pass,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
enum Pass {
    Fog { msaa: bool },
    Uber,
}

impl SpecializedRenderPipeline for PostFxPipeline {
    type Key = PostFxKey;

    fn specialize(&self, key: Self::Key) -> RenderPipelineDescriptor {
        let target = |format| {
            Some(ColorTargetState {
                format,
                blend: None,
                write_mask: ColorWrites::ALL,
            })
        };
        let targets = match key.pass {
            Pass::Fog { .. } => vec![target(key.format), target(PRE_BLOOM_FORMAT)],
            Pass::Uber => vec![target(key.format)],
        };
        let (label, layout, shader) = match key.pass {
            Pass::Fog { msaa: false } => (
                "asamu_height_fog",
                self.fog_layout.clone(),
                self.shaders.fog_single.clone(),
            ),
            Pass::Fog { msaa: true } => (
                "asamu_height_fog_msaa",
                self.fog_layout_msaa.clone(),
                self.shaders.fog_multi.clone(),
            ),
            Pass::Uber => (
                "asamu_uber",
                self.uber_layout.clone(),
                self.shaders.uber.clone(),
            ),
        };
        RenderPipelineDescriptor {
            label: Some(label.into()),
            layout: vec![layout],
            vertex: self.fullscreen.to_vertex_state(),
            fragment: Some(FragmentState {
                shader,
                targets,
                ..default()
            }),
            ..default()
        }
    }
}

/// Pipeline ids of a view.
#[derive(Component)]
pub struct PostFxPipelineIds {
    fog: CachedRenderPipelineId,
    uber: CachedRenderPipelineId,
}

fn prepare_pipelines(
    mut commands: Commands,
    cache: Res<PipelineCache>,
    mut pipelines: ResMut<SpecializedRenderPipelines<PostFxPipeline>>,
    pipeline: Option<Res<PostFxPipeline>>,
    views: Query<(Entity, &ExtractedView, &Msaa), With<PostFxUniform>>,
) {
    let Some(pipeline) = pipeline else {
        return;
    };
    for (entity, view, msaa) in &views {
        let fog = pipelines.specialize(
            &cache,
            &pipeline,
            PostFxKey {
                format: view.target_format,
                pass: Pass::Fog {
                    msaa: msaa.samples() > 1,
                },
            },
        );
        let uber = pipelines.specialize(
            &cache,
            &pipeline,
            PostFxKey {
                format: view.target_format,
                pass: Pass::Uber,
            },
        );
        commands
            .entity(entity)
            .insert(PostFxPipelineIds { fog, uber });
    }
}

/// Format of the pre-bloom copy (HDR, like Bevy's HDR view targets).
const PRE_BLOOM_FORMAT: TextureFormat = TextureFormat::Rgba16Float;

/// The fogged scene before Bevy's bloom, per view.
#[derive(Component)]
pub struct PreBloomTexture(CachedTexture);

fn prepare_pre_bloom_textures(
    mut commands: Commands,
    device: Res<RenderDevice>,
    mut cache: ResMut<TextureCache>,
    views: Query<(Entity, &ViewTarget), With<PostFxPasses>>,
) {
    for (entity, target) in &views {
        let size = target.main_texture().size();
        let texture = cache.get(
            &device,
            TextureDescriptor {
                label: Some("asamu_pre_bloom"),
                size: Extent3d {
                    width: size.width,
                    height: size.height,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: TextureDimension::D2,
                format: PRE_BLOOM_FORMAT,
                usage: TextureUsages::RENDER_ATTACHMENT | TextureUsages::TEXTURE_BINDING,
                view_formats: &[],
            },
        );
        commands.entity(entity).insert(PreBloomTexture(texture));
    }
}

fn run_fullscreen(
    ctx: &mut RenderContext,
    label: &'static str,
    destinations: &[&bevy::render::render_resource::TextureView],
    pipeline: &bevy::render::render_resource::RenderPipeline,
    bind_group: &bevy::render::render_resource::BindGroup,
    offset: u32,
) {
    let attachments: Vec<Option<RenderPassColorAttachment>> = destinations
        .iter()
        .map(|view| {
            Some(RenderPassColorAttachment {
                view,
                depth_slice: None,
                resolve_target: None,
                ops: Operations {
                    load: LoadOp::Clear(Default::default()),
                    store: StoreOp::Store,
                },
            })
        })
        .collect();
    let mut pass = ctx.begin_tracked_render_pass(RenderPassDescriptor {
        label: Some(label),
        color_attachments: &attachments,
        depth_stencil_attachment: None,
        timestamp_writes: None,
        occlusion_query_set: None,
        multiview_mask: None,
    });
    pass.set_render_pipeline(pipeline);
    pass.set_bind_group(0, bind_group, &[offset]);
    pass.draw(0..3, 0..1);
}

#[allow(clippy::too_many_arguments, clippy::type_complexity)]
fn height_fog_pass(
    view: ViewQuery<(
        &ViewTarget,
        &ViewDepthStencilTexture,
        &PostFxPasses,
        &PostFxPipelineIds,
        &DynamicUniformIndex<PostFxUniform>,
        &Msaa,
        &PreBloomTexture,
    )>,
    pipeline: Option<Res<PostFxPipeline>>,
    cache: Res<PipelineCache>,
    uniforms: Res<ComponentUniforms<PostFxUniform>>,
    mut ctx: RenderContext,
) {
    let (target, depth, passes, ids, index, msaa, pre_bloom) = view.into_inner();
    if !passes.fog && !passes.uber {
        return;
    }
    let (Some(pipeline), Some(render_pipeline), Some(binding)) = (
        pipeline,
        cache.get_render_pipeline(ids.fog),
        uniforms.uniforms().binding(),
    ) else {
        return;
    };
    let Some(depth_view) = depth.attachment.depth_stencil_views().depth_only_view() else {
        return;
    };
    let layout = if msaa.samples() > 1 {
        &pipeline.fog_layout_msaa
    } else {
        &pipeline.fog_layout
    };
    let post = target.post_process_write();
    let bind_group = ctx.render_device().create_bind_group(
        Some("asamu_height_fog"),
        &cache.get_bind_group_layout(layout),
        &BindGroupEntries::sequential((post.source, depth_view, binding)),
    );
    run_fullscreen(
        &mut ctx,
        "asamu_height_fog",
        &[post.destination, &pre_bloom.0.default_view],
        render_pipeline,
        &bind_group,
        index.index(),
    );
}

/// True when the fog pass can run this frame (its pipeline is compiled and
/// the depth buffer has a depth-only view). The uber pass depends on the
/// pre-bloom copy that pass writes: without it the copy would be stale (or
/// empty, and the whole scene would count as bloom).
fn fog_pass_ready(
    cache: &PipelineCache,
    ids: &PostFxPipelineIds,
    depth: &ViewDepthStencilTexture,
) -> bool {
    cache.get_render_pipeline(ids.fog).is_some()
        && depth
            .attachment
            .depth_stencil_views()
            .depth_only_view()
            .is_some()
}

#[allow(clippy::too_many_arguments, clippy::type_complexity)]
fn uber_pass(
    view: ViewQuery<(
        &ViewTarget,
        &ViewDepthStencilTexture,
        &PostFxPasses,
        &PostFxPipelineIds,
        &DynamicUniformIndex<PostFxUniform>,
        &PreBloomTexture,
    )>,
    pipeline: Option<Res<PostFxPipeline>>,
    cache: Res<PipelineCache>,
    uniforms: Res<ComponentUniforms<PostFxUniform>>,
    images: Res<RenderAssets<GpuImage>>,
    mut ctx: RenderContext,
) {
    let (target, depth, passes, ids, index, pre_bloom) = view.into_inner();
    if !passes.uber || !fog_pass_ready(&cache, ids, depth) {
        return;
    }
    let (Some(pipeline), Some(render_pipeline), Some(binding), Some(lut)) = (
        pipeline,
        cache.get_render_pipeline(ids.uber),
        uniforms.uniforms().binding(),
        images.get(&passes.lut),
    ) else {
        return;
    };
    let post = target.post_process_write();
    let bind_group = ctx.render_device().create_bind_group(
        Some("asamu_uber"),
        &cache.get_bind_group_layout(&pipeline.uber_layout),
        &BindGroupEntries::sequential((
            post.source,
            &pre_bloom.0.default_view,
            &lut.texture_view,
            &pipeline.lut_sampler,
            binding,
        )),
    );
    run_fullscreen(
        &mut ctx,
        "asamu_uber",
        &[post.destination],
        render_pipeline,
        &bind_group,
        index.index(),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fog_shader_variants_differ_only_in_the_depth_type() {
        let single = fog_shader_source(false);
        let multi = fog_shader_source(true);
        assert!(!single.contains("DEPTH_TEXTURE_TYPE") && !multi.contains("DEPTH_TEXTURE_TYPE"));
        assert!(single.contains("var depth_texture: texture_2d<f32>;"));
        assert!(multi.contains("var depth_texture: texture_multisampled_2d<f32>;"));
        assert_eq!(
            single.replace("var depth_texture: texture_2d<f32>;", "X"),
            multi.replace("var depth_texture: texture_multisampled_2d<f32>;", "X")
        );
    }

    #[test]
    fn shaders_declare_the_same_uniform_layout() {
        let fields = |src: &str| {
            let start = src.find("struct PostFx {").unwrap();
            let end = start + src[start..].find('}').unwrap();
            src[start..end]
                .lines()
                .map(str::trim)
                .filter(|l| !l.starts_with("//") && l.contains(':'))
                .map(str::to_owned)
                .collect::<Vec<_>>()
        };
        let fog = fields(HEIGHT_FOG_WGSL);
        assert_eq!(fog, fields(UBER_WGSL));
        // Ten fields, as in `PostFxUniform`.
        assert_eq!(fog.len(), 10);
        assert_eq!(fog[0], "world_from_clip: mat4x4<f32>,");
        assert_eq!(fog[9], "bloom: vec4<f32>,");
        assert_eq!(PostFxUniform::min_size().get(), 64 + 9 * 16);
    }
}
