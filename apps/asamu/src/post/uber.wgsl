// ASAMU-decomp: the UE3 uber post-process blend as a full-screen pass:
// the customizable tonemapper, then the colour-grading LUT (display-gamma
// space), then decoding to linear for Bevy's sRGB output. The bloom Bevy
// added is recovered as the difference to the pre-bloom copy the fog pass
// wrote, then tinted, scaled and faded on bright pixels the original's way.
//
// Our own shader. The maths follows docs/reverse-engineering/POST_FOG_SKY.md;
// the reference implementations and tests are `Tonemapper::map`,
// `Lut::sample` and `bloom_blend` in post/ue3.rs.

struct PostFx {
    world_from_clip: mat4x4<f32>,
    camera: vec4<f32>,
    fog: vec4<f32>,
    fog_opposite: vec4<f32>,
    fog_inscatter: vec4<f32>,
    fog_light: vec4<f32>,
    // A, B, crossover, scale.
    tonemap: vec4<f32>,
    // x: toe factor; y: kind (0 off, 1 filmic, 2 customizable); z: 1 when
    // the LUT applies.
    tonemap2: vec4<f32>,
    // x: UE3 scene colour per render radiance unit.
    grade: vec4<f32>,
    // rgb: Bloom_Tint (linear) × Bloom_Scale; a: Bloom_ScreenBlendThreshold.
    bloom: vec4<f32>,
}

@group(0) @binding(0) var screen_texture: texture_2d<f32>;
@group(0) @binding(1) var pre_bloom_texture: texture_2d<f32>;
@group(0) @binding(2) var lut_texture: texture_2d<f32>;
@group(0) @binding(3) var lut_sampler: sampler;
@group(0) @binding(4) var<uniform> fx: PostFx;

fn tonemap(x_in: vec3<f32>) -> vec3<f32> {
    let x = max(x_in, vec3<f32>(0.0));
    let kind = fx.tonemap2.y;
    if (kind < 0.5) {
        return clamp(pow(x, vec3<f32>(1.0 / 2.2)), vec3<f32>(0.0), vec3<f32>(1.0));
    }
    let curve = fx.tonemap.y * x / max(abs(x + vec3<f32>(fx.tonemap.x)), vec3<f32>(1e-12));
    if (kind < 1.5) {
        return clamp(curve, vec3<f32>(0.0), vec3<f32>(1.0));
    }
    let toe = pow(x * fx.tonemap.w, vec3<f32>(1.0 / 2.2));
    let k = clamp((x - vec3<f32>(fx.tonemap.z)) * 10000.0, vec3<f32>(0.0), vec3<f32>(1.0));
    let m = mix(toe, curve, k);
    return clamp(mix(m, curve, vec3<f32>(fx.tonemap2.x)), vec3<f32>(0.0), vec3<f32>(1.0));
}

fn grade_lut(c: vec3<f32>) -> vec3<f32> {
    let slice = floor(c.b * 14.9999);
    let f = c.b * 15.0 - slice;
    let u = (slice * 16.0 + c.r * 15.0 + 0.5) / 256.0;
    let v = (c.g * 15.0 + 0.5) / 16.0;
    let a = textureSampleLevel(lut_texture, lut_sampler, vec2<f32>(u, v), 0.0).rgb;
    let b = textureSampleLevel(lut_texture, lut_sampler, vec2<f32>(u + 16.0 / 256.0, v), 0.0).rgb;
    return mix(a, b, vec3<f32>(f));
}

fn srgb_to_linear(c: vec3<f32>) -> vec3<f32> {
    let lo = c / 12.92;
    let hi = pow((c + vec3<f32>(0.055)) / 1.055, vec3<f32>(2.4));
    return select(hi, lo, c <= vec3<f32>(0.04045));
}

@fragment
fn fragment(@builtin(position) position: vec4<f32>, @location(0) uv: vec2<f32>) -> @location(0) vec4<f32> {
    let coord = vec2<i32>(position.xy);
    let scene = textureLoad(screen_texture, coord, 0);
    let pre = textureLoad(pre_bloom_texture, coord, 0).rgb * fx.grade.x;
    let bloom = max(scene.rgb * fx.grade.x - pre, vec3<f32>(0.0));
    let lum = dot(min(pre, vec3<f32>(65503.0)), vec3<f32>(0.3, 0.59, 0.11));
    let fade = clamp(exp2(-3.0 * lum) * fx.bloom.a, 0.0, 1.0);
    var g = tonemap(pre + bloom * fx.bloom.rgb * fade);
    if (fx.tonemap2.z > 0.5) {
        g = grade_lut(g);
    }
    return vec4<f32>(srgb_to_linear(clamp(g, vec3<f32>(0.0), vec3<f32>(1.0))), scene.a);
}
