// ASAMU-decomp: UE3 exponential height fog as a full-screen pass.
//
// Our own shader. The maths follows docs/reverse-engineering/POST_FOG_SKY.md
// (the original's fog pass, described there in our words); the reference
// implementation and its tests are `HeightFog::evaluate` in post/ue3.rs.
// The depth binding's type is filled in by the plugin (single- or
// multi-sampled depth; `textureLoad(.., 0)` reads mip 0 or sample 0). With
// the fog off the pass only copies the scene (see `FogOutput`).

struct PostFx {
    world_from_clip: mat4x4<f32>,
    // xyz: camera position (render space); w: UU per render unit.
    camera: vec4<f32>,
    // x: density at the camera height (per UU); y: height falloff (per UU);
    // z: directional colour exponent; w: start distance (UU).
    fog: vec4<f32>,
    // rgb: opposite-light colour (render radiance); a: least transmittance.
    fog_opposite: vec4<f32>,
    // rgb: inscattering colour (render radiance); a: 1 when the fog is on.
    fog_inscatter: vec4<f32>,
    // xyz: unit direction towards the light (render space); w: ray length
    // used where nothing was drawn (UU).
    fog_light: vec4<f32>,
    tonemap: vec4<f32>,
    tonemap2: vec4<f32>,
    grade: vec4<f32>,
    bloom: vec4<f32>,
}

// The fogged scene, twice: the post-processing chain's next texture and a
// copy the uber pass uses to separate Bevy's bloom from the scene.
struct FogOutput {
    @location(0) color: vec4<f32>,
    @location(1) pre_bloom: vec4<f32>,
}

@group(0) @binding(0) var screen_texture: texture_2d<f32>;
@group(0) @binding(1) var depth_texture: DEPTH_TEXTURE_TYPE;
@group(0) @binding(2) var<uniform> fx: PostFx;

@fragment
fn fragment(@builtin(position) position: vec4<f32>, @location(0) uv: vec2<f32>) -> FogOutput {
    let coord = vec2<i32>(position.xy);
    let scene = textureLoad(screen_texture, coord, 0);
    if (fx.fog_inscatter.a < 0.5) {
        return FogOutput(scene, scene);
    }
    let depth = textureLoad(depth_texture, coord, 0).x;
    let ndc = vec2<f32>(uv.x * 2.0 - 1.0, 1.0 - uv.y * 2.0);
    let near_h = fx.world_from_clip * vec4<f32>(ndc, 1.0, 1.0);
    let ray = near_h.xyz / near_h.w - fx.camera.xyz;
    let dir = ray / max(length(ray), 1e-12);
    // Reverse-Z: depth 0 is the infinite far plane (nothing drawn).
    var dist = fx.fog_light.w;
    if (depth > 0.0) {
        let h = fx.world_from_clip * vec4<f32>(ndc, depth, 1.0);
        dist = length(h.xyz / h.w - fx.camera.xyz) * fx.camera.w;
    }
    // Render +Y is UE3 +Z.
    var rise = dir.y * dist;
    if (abs(rise) <= 0.01) {
        rise = 0.01;
    }
    let kz = fx.fog.y * rise;
    var line_integral = 0.6931472;
    if (abs(kz) > 1e-6) {
        line_integral = (1.0 - exp2(clamp(-kz, -126.0, 126.0))) / kz;
    }
    let integral = fx.fog.x * line_integral * max(dist - fx.fog.w, 0.0);
    let transmittance = max(clamp(exp2(-integral), 0.0, 1.0), fx.fog_opposite.a);
    let w = pow(abs(0.5 - 0.499 * dot(fx.fog_light.xyz, dir)), fx.fog.z);
    let color = mix(fx.fog_inscatter.rgb, fx.fog_opposite.rgb, w);
    let fogged = vec4<f32>(scene.rgb * transmittance + color * (1.0 - transmittance), scene.a);
    return FogOutput(fogged, fogged);
}
