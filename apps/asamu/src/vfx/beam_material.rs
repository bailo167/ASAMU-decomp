//! The grapple beam's material: `AdventureSuitEffects.GrappleBeam_inst`
//! (parent `MainBeam_Mat`), additive, unlit, two-sided (VFX_DECALS.md §2).
//!
//! What the original's material computes (CONFIRMED (content), read from the
//! material's expressions and described here in our own words): the sum of
//! three grey textures, times the colour parameter `GrappleBeamColor`, times
//! 20, as emissive light:
//!
//! - a thin soft line along the middle of the ribbon (the middle half of the
//!   texture's height stretched over the ribbon's width);
//! - a wider core over the whole width, fully white in its middle fifth,
//!   with faint side bands;
//! - a round soft spot stretched over the beam's whole length and panned
//!   along it at two beam lengths a second, hand to anchor: a pulse that
//!   runs up the beam twice a second.
//!
//! The particles' own colour (the system's colour-over-life modules) does
//! not enter: the material has no vertex-colour input.
//!
//! With the default colour (0.044, 0.191, 0.796) the light in the middle of
//! the beam is about (2.5, 11, 44) times display white: a white-hot line
//! that falls off through cyan to the beam's blue at the ribbon's edges.
//!
//! What is ours here, each an approximation and marked where it is made:
//!
//! - PLACEHOLDER (display mapping): the original adds that light to an HDR
//!   scene that its post-process blooms and tone maps. The beam is drawn
//!   with the hand, after the world's post-process, straight to the display
//!   range: each channel is `1 − exp(−light)`, which keeps the white core
//!   and the blue edge. No bloom halo is drawn around it.
//! - PLACEHOLDER (bright backgrounds): pure addition disappears on a white
//!   background, where the original's halo would still show. The beam also
//!   covers what is behind it a little, in proportion to its own brightness
//!   ([`COVERAGE`]), so its blue edge stays visible against snow and sky.
//! - Without the converted textures the ribbon gets a soft procedural line
//!   of about the same shape (PLACEHOLDER), never a flat band.

use bevy::asset::uuid_handle;
use bevy::prelude::*;
use bevy::render::render_resource::{AsBindGroup, ShaderType};
use bevy::shader::ShaderRef;

/// How much of the background the beam's brightest parts cover (ours; see
/// the module docs). 0 would be the original's pure addition.
pub const COVERAGE: f32 = 0.55;

/// Handle of the beam's fragment shader.
pub const BEAM_SHADER: Handle<Shader> = uuid_handle!("8c2f6b1a-3d4e-4f70-9b15-6a7c0d9e2f43");

/// The fragment shader. Inputs are the stock mesh vertex outputs (UV at
/// location 2, vertex colour at location 5: the beam's meshes always carry
/// both). `#{MATERIAL_BIND_GROUP}` is replaced with Bevy's material bind
/// group index when the shader is registered.
pub const BEAM_WGSL: &str = r"
struct BeamParams {
    // rgb: the light of a white texel (colour parameter times 20); a: coverage.
    light: vec4<f32>,
    // xy: UV scale of the thin line's sample; zw: its offset.
    thin_uv: vec4<f32>,
    // x: the pulse's offset along the beam; yzw: 1 where the thin line, the
    // core and the pulse texture are bound, else 0.
    pulse: vec4<f32>,
};

@group(#{MATERIAL_BIND_GROUP}) @binding(0) var<uniform> params: BeamParams;
@group(#{MATERIAL_BIND_GROUP}) @binding(1) var thin_texture: texture_2d<f32>;
@group(#{MATERIAL_BIND_GROUP}) @binding(2) var thin_sampler: sampler;
@group(#{MATERIAL_BIND_GROUP}) @binding(3) var core_texture: texture_2d<f32>;
@group(#{MATERIAL_BIND_GROUP}) @binding(4) var core_sampler: sampler;
@group(#{MATERIAL_BIND_GROUP}) @binding(5) var pulse_texture: texture_2d<f32>;
@group(#{MATERIAL_BIND_GROUP}) @binding(6) var pulse_sampler: sampler;

struct BeamInput {
    @location(2) uv: vec2<f32>,
    @location(5) color: vec4<f32>,
};

@fragment
fn fragment(in: BeamInput) -> @location(0) vec4<f32> {
    let thin = textureSample(thin_texture, thin_sampler, in.uv * params.thin_uv.xy + params.thin_uv.zw).rgb;
    let core = textureSample(core_texture, core_sampler, in.uv).rgb;
    let pulse = textureSample(pulse_texture, pulse_sampler, vec2<f32>(in.uv.x + params.pulse.x, in.uv.y)).rgb;
    var sum = thin * params.pulse.y + core * params.pulse.z + pulse * params.pulse.w;
    // No converted texture at all: a soft line of our own instead of a flat band.
    let bound = params.pulse.y + params.pulse.z + params.pulse.w;
    if bound < 0.5 {
        let d = (in.uv.y - 0.5) * 2.0;
        sum = vec3<f32>(exp(-d * d * 9.0) + 0.9 * exp(-d * d * 60.0));
    }
    let light = params.light.rgb * sum * in.color.rgb;
    // Display mapping (ours): saturate each channel softly.
    let rgb = vec3<f32>(1.0) - exp(-light);
    let cover = clamp(params.light.a * max(rgb.r, max(rgb.g, rgb.b)), 0.0, 1.0);
    // Premultiplied blending: adds `rgb`, keeps `1 - cover` of what is behind.
    return vec4<f32>(rgb, cover);
}
";

/// Uniforms of [`BeamMaterial`] (layout mirrored by `BeamParams` in the
/// shader).
#[derive(ShaderType, Clone, Copy, Debug, PartialEq)]
pub struct BeamParams {
    /// The light of a white texel, linear RGB (the colour parameter times
    /// the material's factor); alpha is the coverage.
    pub light: Vec4,
    /// UV scale (xy) and offset (zw) of the thin line's sample.
    pub thin_uv: Vec4,
    /// The pulse's offset along the beam (x) and which of the three textures
    /// are bound (y thin, z core, w pulse: 1 or 0).
    pub pulse: Vec4,
}

impl BeamParams {
    /// The uniforms for a beam colour (`GrappleBeamColor`, linear).
    #[must_use]
    pub fn for_color(color: [f32; 4]) -> Self {
        let k = super::beam::EMISSIVE_SCALE;
        let [su, sv] = super::beam::THIN_UV_SCALE;
        let [ou, ov] = super::beam::THIN_UV_OFFSET;
        Self {
            light: Vec4::new(color[0] * k, color[1] * k, color[2] * k, COVERAGE),
            thin_uv: Vec4::new(su, sv, ou, ov),
            pulse: Vec4::ZERO,
        }
    }

    /// Sets the beam's colour, keeping the rest.
    pub fn set_color(&mut self, color: [f32; 4]) {
        let k = super::beam::EMISSIVE_SCALE;
        self.light = Vec4::new(color[0] * k, color[1] * k, color[2] * k, self.light.w);
    }
}

/// The pulse's texture offset `seconds` after the beam's material started:
/// the panner adds `time · speed` to the coordinate along the beam; kept in
/// `[0, 1)` (the texture repeats).
#[must_use]
pub fn pulse_offset(seconds: f32) -> f32 {
    if !seconds.is_finite() {
        return 0.0;
    }
    let offset = (seconds * super::beam::PULSE_PAN_SPEED).rem_euclid(1.0);
    // `rem_euclid` rounds a tiny negative remainder up to 1.
    if offset < 1.0 { offset } else { 0.0 }
}

/// The beam's material.
#[derive(Asset, TypePath, AsBindGroup, Clone, Debug)]
pub struct BeamMaterial {
    /// Uniforms.
    #[uniform(0)]
    pub params: BeamParams,
    /// The thin line (white when absent; its flag in the uniforms is 0).
    #[texture(1)]
    #[sampler(2)]
    pub thin: Option<Handle<Image>>,
    /// The wide core.
    #[texture(3)]
    #[sampler(4)]
    pub core: Option<Handle<Image>>,
    /// The travelling pulse.
    #[texture(5)]
    #[sampler(6)]
    pub pulse: Option<Handle<Image>>,
}

impl BeamMaterial {
    /// A beam of `color` without textures (the procedural line).
    #[must_use]
    pub fn new(color: [f32; 4]) -> Self {
        Self {
            params: BeamParams::for_color(color),
            thin: None,
            core: None,
            pulse: None,
        }
    }

    /// Binds the converted textures (each may be missing) and sets their
    /// flags.
    pub fn set_textures(
        &mut self,
        thin: Option<Handle<Image>>,
        core: Option<Handle<Image>>,
        pulse: Option<Handle<Image>>,
    ) {
        let flag = |t: &Option<Handle<Image>>| if t.is_some() { 1.0 } else { 0.0 };
        self.params.pulse.y = flag(&thin);
        self.params.pulse.z = flag(&core);
        self.params.pulse.w = flag(&pulse);
        self.thin = thin;
        self.core = core;
        self.pulse = pulse;
    }
}

impl Material for BeamMaterial {
    fn fragment_shader() -> ShaderRef {
        ShaderRef::Handle(BEAM_SHADER)
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

/// The shader text with the material bind group index filled in.
#[must_use]
pub fn beam_shader_source() -> String {
    BEAM_WGSL.replace(
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
        &BEAM_SHADER,
        Shader::from_wgsl(beam_shader_source(), "asamu/vfx/beam.wgsl"),
    ) {
        warn!("vfx: beam shader not registered: {e}");
        return;
    }
    app.add_plugins(MaterialPlugin::<BeamMaterial>::default());
}

/// What the shader's display mapping makes of `light` (the same formula,
/// for the tests): the colour added and the share of the background covered.
#[cfg(test)]
fn display(light: [f32; 3], coverage: f32) -> ([f32; 3], f32) {
    let rgb = light.map(|c| 1.0 - (-c).exp());
    let peak = rgb[0].max(rgb[1]).max(rgb[2]);
    (rgb, (coverage * peak).clamp(0.0, 1.0))
}

#[cfg(test)]
mod tests {
    use super::super::beam::{DEFAULT_BEAM_COLOR, EMISSIVE_SCALE, MIDAS_BEAM_COLOR};
    use super::*;

    #[test]
    fn uniforms_follow_the_material() {
        let p = BeamParams::for_color(DEFAULT_BEAM_COLOR);
        assert!((p.light.z - DEFAULT_BEAM_COLOR[2] * EMISSIVE_SCALE).abs() < 1e-6);
        assert_eq!(p.light.w, COVERAGE);
        assert_eq!(p.thin_uv, Vec4::new(1.0, 0.5, 0.0, 0.25));
        assert_eq!(p.pulse, Vec4::ZERO);
        let mut m = BeamMaterial::new(DEFAULT_BEAM_COLOR);
        m.set_textures(Some(Handle::default()), None, Some(Handle::default()));
        assert_eq!(m.params.pulse.yzw(), Vec3::new(1.0, 0.0, 1.0));
        m.params.pulse.x = 0.25;
        m.params.set_color(MIDAS_BEAM_COLOR);
        assert!((m.params.light.x - 20.0).abs() < 1e-5);
        assert_eq!(m.params.light.w, COVERAGE);
        assert_eq!(m.params.pulse, Vec4::new(0.25, 1.0, 0.0, 1.0));
        assert_eq!(m.alpha_mode(), AlphaMode::Premultiplied);
    }

    /// The pulse runs from the hand to the anchor twice a second: its
    /// texture offset falls by two lengths a second and stays in `[0, 1)`.
    #[test]
    fn the_pulse_runs_up_the_beam_twice_a_second() {
        assert_eq!(pulse_offset(0.0), 0.0);
        assert!((pulse_offset(0.1) - 0.8).abs() < 1e-5);
        assert!((pulse_offset(0.25) - 0.5).abs() < 1e-5);
        assert!(pulse_offset(0.5).abs() < 1e-5);
        assert_eq!(pulse_offset(1.0e-9), 0.0);
        for t in [0.0f32, 0.013, 1.7, 99.99, 1.0e6] {
            let o = pulse_offset(t);
            assert!((0.0..1.0).contains(&o), "{t}: {o}");
        }
        assert_eq!(pulse_offset(f32::NAN), 0.0);
        assert_eq!(pulse_offset(f32::INFINITY), 0.0);
    }

    /// The beam's picture by numbers: a white-hot middle, a blue edge, and
    /// an edge that still shows on a white background.
    #[test]
    fn the_beam_has_a_white_core_and_a_blue_edge_on_any_background() {
        let color = DEFAULT_BEAM_COLOR;
        let light = |texels: f32| {
            [
                color[0] * EMISSIVE_SCALE * texels,
                color[1] * EMISSIVE_SCALE * texels,
                color[2] * EMISSIVE_SCALE * texels,
            ]
        };
        // The middle: the three textures add up to more than 2.
        let (core, core_cover) = display(light(2.4), COVERAGE);
        assert!(core.iter().all(|c| *c > 0.8), "{core:?}");
        // The edge: a twentieth of a white texel.
        let (edge, edge_cover) = display(light(0.05), COVERAGE);
        assert!(edge[2] > 0.5 && edge[0] < 0.1 && edge[1] < 0.25, "{edge:?}");
        // On black the beam is what it adds.
        let on = |background: f32, (rgb, cover): ([f32; 3], f32)| {
            rgb.map(|c| (c + background * (1.0 - cover)).min(1.0))
        };
        assert_eq!(on(0.0, (edge, edge_cover)), edge);
        // On white the edge is still clearly blue (red well below blue),
        // where pure addition would leave white.
        let on_white = on(1.0, (edge, edge_cover));
        assert!(on_white[2] > 0.99 && on_white[0] < 0.8, "{on_white:?}");
        assert_eq!(on(1.0, (edge, 0.0)), [1.0, 1.0, 1.0]);
        // The core stays the brightest thing on any background.
        let core_on_white = on(1.0, (core, core_cover));
        assert!(core_on_white.iter().all(|c| *c > 0.95), "{core_on_white:?}");
    }

    #[test]
    fn the_shader_names_its_bindings() {
        let src = beam_shader_source();
        assert!(!src.contains("#{"));
        for needle in [
            "thin_texture",
            "core_texture",
            "pulse_texture",
            "@location(2) uv",
            "@location(5) color",
        ] {
            assert!(src.contains(needle), "{needle}");
        }
    }
}
