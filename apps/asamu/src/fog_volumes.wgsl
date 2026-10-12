// ASAMU-decomp: UE3 fog volumes, evaluated per pixel inside the fog pass.
//
// Our own shader. The rules are the original's, described in our words in
// docs/reverse-engineering/POST_FOG_SKY.md (section 3); the reference
// implementation and its tests are in fog_volumes.rs (`FogVolume::integral`,
// and `gpu_transmittance`, which this file transliterates). The fog pass's
// shader is prefixed with this text.

struct FogVolume {
    // Render space to the volume's box, whose faces are at +-1.
    local_from_world: mat4x4<f32>,
    // Linear half-space volumes: dot(xyz, p) + w is the height (UU) of the
    // render-space point p above the plane.
    plane: vec4<f32>,
    // x: 0 constant density, 1 linear half-space; y: density per UU, or the
    // density gained per UU below the plane; z: start distance (UU);
    // w: maximum distance (UU).
    params: vec4<f32>,
    // rgb: fog colour (render radiance).
    color: vec4<f32>,
}

// No fog is integrated beyond this view depth (UU), whatever the volume's
// own maximum distance.
const FOG_VOLUME_HARD_MAX_DISTANCE: f32 = 65535.0;

// Density of a linear half-space volume integrated along the straight line
// from the eye to the point `len` UU along the ray. `eye_h` is the eye's
// height above the plane (UU), `dir_h` the height gained per UU of ray.
fn fog_halfspace_line_integral(eye_h: f32, dir_h: f32, len: f32, gain: f32) -> f32 {
    let point_h = eye_h + dir_h * len;
    var k = 0.0;
    if (eye_h <= 0.0) {
        k = 1.0;
    }
    let c = min((1.0 - 2.0 * k) * point_h, 0.0);
    let g = k * (point_h + eye_h) - c * c / (abs(eye_h - point_h) + 0.0001);
    return -0.5 * gain * len * g;
}

// Transmittance of what is seen through the volume along one view ray.
// `camera` and the unit `dir` are in render space; `cos_view` is the cosine
// between the ray and the view axis; `scene_distance` is the length of the
// ray to the drawn surface (UU).
fn fog_volume_transmittance(
    v: FogVolume,
    camera: vec3<f32>,
    dir: vec3<f32>,
    cos_view: f32,
    uu_per_unit: f32,
    scene_distance: f32,
) -> f32 {
    // Where the ray is inside the box (slab test; distances in render units).
    let o = (v.local_from_world * vec4<f32>(camera, 1.0)).xyz;
    var d = (v.local_from_world * vec4<f32>(dir, 0.0)).xyz;
    d = select(d, vec3<f32>(1.0e-20), abs(d) < vec3<f32>(1.0e-20));
    let a = (vec3<f32>(-1.0) - o) / d;
    let b = (vec3<f32>(1.0) - o) / d;
    let near3 = min(a, b);
    let far3 = max(a, b);
    let t_in = max(max(near3.x, near3.y), max(near3.z, 0.0));
    let t_out = min(far3.x, min(far3.y, far3.z));
    if (t_out <= t_in) {
        return 1.0;
    }
    // The original measures along the view axis: the depth of the volume's
    // faces, never beyond the drawn surface or the maximum distance.
    let cap = min(min(scene_distance * cos_view, v.params.w), FOG_VOLUME_HARD_MAX_DISTANCE);
    let z_in = min(t_in * uu_per_unit * cos_view, cap);
    let z_out = min(t_out * uu_per_unit * cos_view, cap);
    var integral = 0.0;
    if (v.params.x < 0.5) {
        let start = v.params.z;
        integral = v.params.y * (max(z_out - start, 0.0) - max(z_in - start, 0.0));
    } else {
        let eye_h = dot(v.plane.xyz, camera) + v.plane.w;
        let dir_h = dot(v.plane.xyz, dir) / uu_per_unit;
        integral = fog_halfspace_line_integral(eye_h, dir_h, z_out / cos_view, v.params.y)
            - fog_halfspace_line_integral(eye_h, dir_h, z_in / cos_view, v.params.y);
    }
    return clamp(exp2(-integral), 0.0, 1.0);
}
