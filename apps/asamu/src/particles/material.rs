//! The particle sprite material: an unlit shader that multiplies the
//! displayed texture by the material colour (HDR strength kept) and the
//! vertex colour, and takes its opacity from one channel of a second
//! texture (UE3 particle materials often keep the opacity mask in another
//! texture or in a colour channel), blended the way the UE3 blend mode asks.
//!
//! Ours, an approximation of the original material graphs: one colour
//! texture, one opacity mask, constant factors; no distortion, no depth
//! fade, no lighting, no fog.

use asamu_assets::BlendMode;
use asamu_assets::particles::ParticleMaterial;
use bevy::asset::{RenderAssetUsages, uuid_handle};
use bevy::image::{
    ImageAddressMode, ImageFilterMode, ImageLoaderSettings, ImageSampler, ImageSamplerDescriptor,
};
use bevy::prelude::*;
use bevy::render::render_resource::{AsBindGroup, ShaderType};
use bevy::shader::ShaderRef;

use crate::converted::SOURCE;

/// Handle of the sprite fragment shader.
pub const SPRITE_SHADER: Handle<Shader> = uuid_handle!("5d0a1f7e-6c3b-4b7e-9a52-1f3c8e2d4a61");

/// The fragment shader. Inputs are the stock mesh vertex outputs (UV at
/// location 2, vertex colour at location 5; particle meshes always carry
/// both). `#{MATERIAL_BIND_GROUP}` is replaced with Bevy's material bind
/// group index when the shader is registered (plain WGSL gets no shader
/// defs here).
pub const SPRITE_WGSL: &str = r"
struct SpriteParams {
    color: vec4<f32>,
    mask_channels: vec4<f32>,
    mask_bias: f32,
    cutoff: f32,
    mode: u32,
    pad: u32,
};

@group(#{MATERIAL_BIND_GROUP}) @binding(0) var<uniform> params: SpriteParams;
@group(#{MATERIAL_BIND_GROUP}) @binding(1) var color_texture: texture_2d<f32>;
@group(#{MATERIAL_BIND_GROUP}) @binding(2) var color_sampler: sampler;
@group(#{MATERIAL_BIND_GROUP}) @binding(3) var mask_texture: texture_2d<f32>;
@group(#{MATERIAL_BIND_GROUP}) @binding(4) var mask_sampler: sampler;

struct SpriteInput {
    @location(2) uv: vec2<f32>,
    @location(5) color: vec4<f32>,
};

@fragment
fn fragment(in: SpriteInput) -> @location(0) vec4<f32> {
    let texel = textureSample(color_texture, color_sampler, in.uv);
    let mask_texel = textureSample(mask_texture, mask_sampler, in.uv);
    let mask = clamp(dot(mask_texel, params.mask_channels) + params.mask_bias, 0.0, 1.0);
    let rgb = params.color.rgb * texel.rgb * in.color.rgb;
    let alpha = clamp(params.color.a * mask * in.color.a, 0.0, 1.0);
    // 1 mask, 2 alpha blend, 3 additive, 4 modulate; else opaque.
    if params.mode == 1u {
        if alpha < params.cutoff {
            discard;
        }
        return vec4<f32>(rgb, 1.0);
    }
    if params.mode == 2u {
        return vec4<f32>(rgb, alpha);
    }
    if params.mode == 3u {
        // Premultiplied blending with alpha 0 adds `rgb * alpha`.
        return vec4<f32>(rgb * alpha, 0.0);
    }
    if params.mode == 4u {
        return vec4<f32>(rgb * alpha, alpha);
    }
    return vec4<f32>(rgb, 1.0);
}
";

/// Uniforms of [`SpriteMaterial`] (layout mirrored by `SpriteParams` in the
/// shader).
#[derive(ShaderType, Clone, Copy, Debug, PartialEq)]
pub struct SpriteParams {
    /// Linear colour multiplier; alpha is the constant opacity factor.
    pub color: Vec4,
    /// Channel weights of the opacity mask.
    pub mask_channels: Vec4,
    /// Added to the mask (1 when there is no mask texture).
    pub mask_bias: f32,
    /// Alpha cutoff (mask mode).
    pub cutoff: f32,
    /// 0 opaque, 1 mask, 2 alpha blend, 3 additive, 4 modulate.
    pub mode: u32,
    /// Padding.
    pub pad: u32,
}

/// The particle sprite material.
#[derive(Asset, TypePath, AsBindGroup, Clone, Debug)]
pub struct SpriteMaterial {
    /// Uniforms.
    #[uniform(0)]
    pub params: SpriteParams,
    /// Displayed texture (white when absent).
    #[texture(1)]
    #[sampler(2)]
    pub color_texture: Option<Handle<Image>>,
    /// Opacity mask texture (white when absent).
    #[texture(3)]
    #[sampler(4)]
    pub mask_texture: Option<Handle<Image>>,
    /// Blend mode.
    pub alpha_mode: AlphaMode,
}

impl Material for SpriteMaterial {
    fn fragment_shader() -> ShaderRef {
        ShaderRef::Handle(SPRITE_SHADER)
    }

    fn alpha_mode(&self) -> AlphaMode {
        self.alpha_mode
    }

    fn enable_prepass() -> bool {
        false
    }

    fn enable_shadows() -> bool {
        false
    }
}

/// Shader mode and Bevy alpha mode of a UE3 blend mode.
pub fn modes(blend: BlendMode) -> (u32, f32, AlphaMode) {
    match blend {
        BlendMode::Opaque => (0, 0.0, AlphaMode::Opaque),
        BlendMode::Masked { cutoff } => (1, cutoff, AlphaMode::Mask(cutoff)),
        BlendMode::Translucent => (2, 0.0, AlphaMode::Blend),
        BlendMode::Additive => (3, 0.0, AlphaMode::Add),
        BlendMode::Modulate => (4, 0.0, AlphaMode::Multiply),
    }
}

/// The uniforms of a particle material.
pub fn params(m: &ParticleMaterial) -> SpriteParams {
    let (mode, cutoff, _) = modes(m.blend);
    let [r, g, b] = m.color;
    // A mask texture that could not be bound leaves the mask at 1.
    let (channels, bias) = if m.opacity_texture.is_some() {
        (m.opacity_channels, m.opacity_bias)
    } else {
        ([0.0; 4], 1.0)
    };
    SpriteParams {
        color: Vec4::new(r, g, b, m.opacity),
        mask_channels: Vec4::from_array(channels),
        mask_bias: bias,
        cutoff,
        mode,
        pad: 0,
    }
}

fn address(a: asamu_assets::manifest::AddressMode) -> ImageAddressMode {
    match a {
        asamu_assets::manifest::AddressMode::Wrap => ImageAddressMode::Repeat,
        asamu_assets::manifest::AddressMode::Clamp => ImageAddressMode::ClampToEdge,
        asamu_assets::manifest::AddressMode::Mirror => ImageAddressMode::MirrorRepeat,
    }
}

/// Image handles by `(converted file, sRGB)`.
pub type ImageCache = std::collections::HashMap<(String, bool), Handle<Image>>;

fn image(
    t: &asamu_assets::TextureBinding,
    server: &AssetServer,
    images: &mut ImageCache,
) -> Handle<Image> {
    images
        .entry((t.file.clone(), t.srgb))
        .or_insert_with(|| {
            let srgb = t.srgb;
            let descriptor = ImageSamplerDescriptor {
                address_mode_u: address(t.address[0]),
                address_mode_v: address(t.address[1]),
                mag_filter: ImageFilterMode::Linear,
                min_filter: ImageFilterMode::Linear,
                mipmap_filter: ImageFilterMode::Linear,
                ..default()
            };
            server
                .load_builder()
                .with_settings(move |s: &mut ImageLoaderSettings| {
                    s.is_srgb = srgb;
                    s.sampler = ImageSampler::Descriptor(descriptor.clone());
                    s.asset_usage = RenderAssetUsages::RENDER_WORLD;
                })
                .load(format!("{SOURCE}://textures/{}", t.file))
        })
        .clone()
}

/// The Bevy material of a particle material.
pub fn sprite_material(
    m: &ParticleMaterial,
    server: &AssetServer,
    images: &mut ImageCache,
) -> SpriteMaterial {
    SpriteMaterial {
        params: params(m),
        color_texture: m.texture.as_ref().map(|t| image(t, server, images)),
        mask_texture: m.opacity_texture.as_ref().map(|t| image(t, server, images)),
        alpha_mode: modes(m.blend).2,
    }
}

/// The shader text with the material bind group index filled in.
pub fn sprite_shader_source() -> String {
    SPRITE_WGSL.replace(
        "#{MATERIAL_BIND_GROUP}",
        &bevy::pbr::MATERIAL_BIND_GROUP_INDEX.to_string(),
    )
}

/// Registers the material and its shader (no-op without a renderer).
pub fn register(app: &mut App) {
    let Some(mut shaders) = app.world_mut().get_resource_mut::<Assets<Shader>>() else {
        return;
    };
    if let Err(e) = shaders.insert(
        &SPRITE_SHADER,
        Shader::from_wgsl(sprite_shader_source(), "asamu/particles/sprite.wgsl"),
    ) {
        warn!("particles: sprite shader not registered: {e}");
        return;
    }
    app.add_plugins(MaterialPlugin::<SpriteMaterial>::default());
}

#[cfg(test)]
mod tests {
    use super::*;

    fn material(blend: BlendMode) -> ParticleMaterial {
        ParticleMaterial {
            path: "Fx.M".to_owned(),
            blend,
            color: [10.0, 5.0, 2.5],
            opacity: 0.4,
            texture: None,
            opacity_texture: None,
            opacity_channels: [0.0, 0.0, 1.0, 0.0],
            opacity_bias: 0.0,
            converted: true,
        }
    }

    #[test]
    fn blend_modes_map_to_shader_modes() {
        assert_eq!(modes(BlendMode::Additive), (3, 0.0, AlphaMode::Add));
        assert_eq!(modes(BlendMode::Translucent), (2, 0.0, AlphaMode::Blend));
        assert_eq!(modes(BlendMode::Modulate), (4, 0.0, AlphaMode::Multiply));
        assert_eq!(
            modes(BlendMode::Masked { cutoff: 0.33 }),
            (1, 0.33, AlphaMode::Mask(0.33))
        );
        assert_eq!(modes(BlendMode::Opaque).0, 0);
    }

    #[test]
    fn params_keep_hdr_colour_and_neutral_mask_without_a_texture() {
        let p = params(&material(BlendMode::Additive));
        assert_eq!(p.color, Vec4::new(10.0, 5.0, 2.5, 0.4));
        assert_eq!(p.mode, 3);
        // No mask texture bound: the mask must evaluate to 1.
        assert_eq!((p.mask_channels, p.mask_bias), (Vec4::ZERO, 1.0));
    }

    /// The shader text declares the bindings and layout the material binds.
    #[test]
    fn shader_matches_the_bind_group() {
        for needle in [
            "@binding(0) var<uniform> params: SpriteParams",
            "@binding(1) var color_texture",
            "@binding(2) var color_sampler",
            "@binding(3) var mask_texture",
            "@binding(4) var mask_sampler",
            "@location(2) uv: vec2<f32>",
            "@location(5) color: vec4<f32>",
        ] {
            assert!(SPRITE_WGSL.contains(needle), "{needle}");
        }
        assert!(
            !sprite_shader_source().contains('#'),
            "no unresolved shader def"
        );
        assert!(sprite_shader_source().contains("@group(3) @binding(0)"));
        // Uniform member order mirrors `SpriteParams`.
        let order = [
            "color: vec4",
            "mask_channels: vec4",
            "mask_bias: f32",
            "cutoff: f32",
            "mode: u32",
        ];
        let mut at = 0;
        for o in order {
            let i = SPRITE_WGSL[at..].find(o).unwrap_or_else(|| panic!("{o}"));
            at += i;
        }
    }
}
