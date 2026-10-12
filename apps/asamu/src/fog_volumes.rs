//! Fog volumes of the original levels (`FogVolume*DensityInfo` actors).
//!
//! In the original a fog volume is not a surface: the actor's mesh only
//! bounds a region, and what is seen through that region is blended towards
//! the fog colour by the density integrated along the view ray between the
//! mesh's front and back faces. This module holds that rule as the original
//! evaluates it (read from its shaders and code; described in our words in
//! `docs/reverse-engineering/POST_FOG_SKY.md`, section 3):
//!
//! - [`FogVolume::from_info`]: a volume from the scene's `atmosphere` block
//!   (density function and its parameters, colour, distances, shape);
//! - `FogVolume::integral` / `FogVolume::transmittance`: the reference
//!   maths in UE3 space (compiled for the tests only);
//! - [`pack`]: the volumes of a level as the fog pass's uniform data
//!   (render space), and `gpu_transmittance` (tests only), the same
//!   evaluation as the shader (`fog_volumes.wgsl`) performs it.
//!
//! The volumes are drawn inside the height-fog pass (`post/render.rs`),
//! after the height fog, which is the original's order (height fog before
//! translucency; fog volumes are translucent primitives).
//!
//! Approximations (ours):
//! - the shape is the box of the volume mesh's bounds. Every shipped volume
//!   uses `EngineMeshes.Cube`, for which that is exact;
//! - the original averages four neighbouring depth samples at the volume's
//!   silhouette; we use the pixel's own depth;
//! - translucent surfaces inside a volume get the fog of what is behind
//!   them (the original fogs translucency per vertex);
//! - spherical and cone density volumes are not drawn (no shipped map has
//!   one); volumes shaped by other actors (`FogVolumeActors`) are not
//!   supported (no shipped volume uses them).

use asamu_assets::manifest::BoundsEntry;
use asamu_assets::scene::FogVolumeInfo;
use asamu_core::coords::bevy_pos_to_ue;
use asamu_core::glam as sim_glam;
#[cfg(test)]
use bevy::math::Vec3;
use bevy::math::{Mat4, Vec4};
use bevy::render::render_resource::ShaderType;

/// The shader text of the per-pixel evaluation (prefixed to the fog pass's
/// shader).
pub const WGSL: &str = include_str!("fog_volumes.wgsl");

/// Fog volumes the fog pass evaluates per pixel. AG-Darkcave has the most
/// (seven); when a level has more, the nearest ones are kept.
pub const MAX_GPU_VOLUMES: usize = 8;

#[cfg(test)]
/// View depth (UU) beyond which the original integrates no fog volume,
/// whatever `MaxDistance` says (a constant of its integral shaders).
pub const HARD_MAX_DISTANCE: f32 = 65535.0;

/// The original scales `PlaneDistanceFactor` by this (its half-space shader
/// multiplies by −0.00005, i.e. −½ · 10⁻⁴).
pub const HALFSPACE_FACTOR_SCALE: f32 = 1.0e-4;

#[cfg(test)]
/// Added to the ray's height change in the half-space integral (a constant
/// of the original's shader that keeps a level ray finite).
const HALFSPACE_EPSILON: f32 = 1.0e-4;

/// Class defaults of `Engine.FogVolumeDensityComponent` and its subclasses,
/// used when a value is missing (the importer writes effective values).
mod defaults {
    pub const DENSITY: f32 = 0.0005;
    pub const PLANE_DISTANCE_FACTOR: f32 = 0.1;
    pub const MAX_DISTANCE: f32 = 65535.0;
    pub const START_DISTANCE: f32 = 0.0;
    pub const SIMPLE_LIGHT_COLOR: [f32; 4] = [0.5, 0.5, 0.7, 1.0];
}

/// The fog material's colour parameter (`UFogVolumeDensityComponent::Attach`
/// writes `SimpleLightColor` into it).
const COLOR_PARAMETER: &str = "EmissiveColor";

/// Density function of a volume.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum FogDensity {
    /// The same density everywhere inside (`Density`, per UU of view depth).
    Constant {
        /// `Density`.
        density: f32,
    },
    /// No fog above a plane; below it the density grows with the depth
    /// under the plane.
    LinearHalfspace {
        /// `HalfspacePlane` as stored (`X`, `Y`, `Z`, `W`): the height of a
        /// UE3 point `p` above the plane is `XYZ·p + W`.
        plane: [f32; 4],
        /// `PlaneDistanceFactor`: the density per UU of depth under the
        /// plane is this times [`HALFSPACE_FACTOR_SCALE`].
        factor: f32,
    },
}

/// One enabled fog volume (UE3 units and axes).
#[derive(Debug, Clone, PartialEq)]
pub struct FogVolume {
    /// Actor name (for the log).
    pub actor: String,
    /// Density function.
    pub density: FogDensity,
    /// Fog colour (linear, UE3 scene units).
    pub color: sim_glam::Vec3,
    /// `StartDistance` (UU of view depth; constant-density volumes only).
    pub start_distance: f32,
    /// `MaxDistance` (UU of view depth).
    pub max_distance: f32,
    /// UE3 world space to the volume's box, whose faces are at ±1.
    pub local_from_ue: sim_glam::Mat4,
    /// Centre of the box (UE3).
    pub center_ue: sim_glam::Vec3,
}

/// The colour of a volume: its fog material's `EmissiveColor` value; for a
/// volume on the engine's base material (no value in the scene) the
/// component's `SimpleLightColor`, which is what the engine writes into
/// that parameter.
fn volume_color(info: &FogVolumeInfo) -> sim_glam::Vec3 {
    let c = info
        .material_vectors
        .get(COLOR_PARAMETER)
        .copied()
        .or_else(|| info.density.params.linear_color("SimpleLightColor"))
        .unwrap_or(defaults::SIMPLE_LIGHT_COLOR);
    sim_glam::Vec3::new(c[0], c[1], c[2]).max(sim_glam::Vec3::ZERO)
}

impl FogVolume {
    /// A volume from the scene's `atmosphere` block and the bounds of its
    /// mesh. `Ok(None)`: the volume draws nothing in the original (disabled,
    /// or a density that is not positive). `Err`: we cannot draw it (says
    /// why).
    pub fn from_info(
        info: &FogVolumeInfo,
        bounds: Option<&BoundsEntry>,
    ) -> Result<Option<Self>, String> {
        let p = &info.density.params;
        // A disabled component registers nothing with the scene.
        if !p.bool("bEnabled").unwrap_or(true) {
            return Ok(None);
        }
        let class = info.density.class_name();
        let density = if class.eq_ignore_ascii_case("FogVolumeConstantDensityComponent") {
            FogDensity::Constant {
                density: p.f32("Density").unwrap_or(defaults::DENSITY),
            }
        } else if class.eq_ignore_ascii_case("FogVolumeLinearHalfspaceDensityComponent") {
            let plane = p
                .plane("HalfspacePlane")
                .ok_or_else(|| "no HalfspacePlane value".to_owned())?;
            FogDensity::LinearHalfspace {
                plane,
                factor: p
                    .f32("PlaneDistanceFactor")
                    .unwrap_or(defaults::PLANE_DISTANCE_FACTOR),
            }
        } else {
            return Err(format!("density function of {class} is not drawn"));
        };
        // The original creates no scene info for a density that is not
        // positive.
        let amount = match density {
            FogDensity::Constant { density } => density,
            FogDensity::LinearHalfspace { plane, factor } => {
                if !plane.iter().all(|x| x.is_finite()) {
                    return Err("HalfspacePlane is not finite".to_owned());
                }
                factor
            }
        };
        if !amount.is_finite() {
            return Err("density is not finite".to_owned());
        }
        if amount <= 0.0 {
            return Ok(None);
        }
        let mesh_to_world = info
            .ue_mesh_to_world
            .ok_or_else(|| "no volume mesh in the scene".to_owned())?;
        let bounds = bounds.ok_or_else(|| {
            format!(
                "bounds of the volume mesh {} are unknown",
                info.mesh.as_deref().unwrap_or("(none)")
            )
        })?;
        let origin = sim_glam::Vec3::from_array(bounds.origin);
        let extent = sim_glam::Vec3::from_array(bounds.box_extent);
        if !origin.is_finite() || !extent.is_finite() || extent.min_element() <= 0.0 {
            return Err("the volume mesh has no box".to_owned());
        }
        let ue_from_local = mesh_to_world
            * sim_glam::Mat4::from_translation(origin)
            * sim_glam::Mat4::from_scale(extent);
        let det = ue_from_local.determinant();
        if !ue_from_local.is_finite() || !det.is_finite() || det.abs() < f32::MIN_POSITIVE {
            return Err("the volume's transform cannot be inverted".to_owned());
        }
        let local_from_ue = ue_from_local.inverse();
        if !local_from_ue.is_finite() {
            return Err("the volume's transform cannot be inverted".to_owned());
        }
        let max_distance = p.f32("MaxDistance").unwrap_or(defaults::MAX_DISTANCE);
        let start_distance = p.f32("StartDistance").unwrap_or(defaults::START_DISTANCE);
        if !max_distance.is_finite() || !start_distance.is_finite() {
            return Err("distances are not finite".to_owned());
        }
        Ok(Some(Self {
            actor: info.density.actor_name.clone(),
            density,
            color: volume_color(info),
            start_distance,
            max_distance,
            local_from_ue,
            center_ue: ue_from_local.transform_point3(sim_glam::Vec3::ZERO),
        }))
    }

    #[cfg(test)]
    /// Where a ray is inside the box: entry and exit distances along the
    /// unit direction `dir` (UU; the entry is 0 when the origin is inside).
    #[must_use]
    pub fn ray_span(&self, origin: sim_glam::Vec3, dir: sim_glam::Vec3) -> Option<(f32, f32)> {
        let o = self.local_from_ue.transform_point3(origin);
        let d = self.local_from_ue.transform_vector3(dir);
        slab_span(o.to_array(), d.to_array())
    }

    #[cfg(test)]
    /// The density integral of one view ray, the way the original
    /// accumulates it: the volume's back faces add, its front faces
    /// subtract, each measured at the face's view depth but never beyond
    /// the drawn surface, `MaxDistance` or [`HARD_MAX_DISTANCE`].
    ///
    /// `camera` is the eye, `forward` the unit view axis, `dir` the unit
    /// direction of the pixel's ray and `scene_distance` the length of that
    /// ray to the drawn surface (all UE3).
    #[must_use]
    pub fn integral(
        &self,
        camera: sim_glam::Vec3,
        forward: sim_glam::Vec3,
        dir: sim_glam::Vec3,
        scene_distance: f32,
    ) -> f32 {
        let Some((t_in, t_out)) = self.ray_span(camera, dir) else {
            return 0.0;
        };
        let cos = dir.dot(forward).max(MIN_VIEW_COSINE);
        let cap = (scene_distance * cos)
            .min(self.max_distance)
            .min(HARD_MAX_DISTANCE);
        let z_in = (t_in * cos).min(cap);
        let z_out = (t_out * cos).min(cap);
        match self.density {
            FogDensity::Constant { density } => {
                let s = self.start_distance;
                density * ((z_out - s).max(0.0) - (z_in - s).max(0.0))
            }
            FogDensity::LinearHalfspace { plane, factor } => {
                let n = sim_glam::Vec3::new(plane[0], plane[1], plane[2]);
                let eye_h = n.dot(camera) + plane[3];
                let dir_h = n.dot(dir);
                let gain = factor * HALFSPACE_FACTOR_SCALE;
                halfspace_line_integral(eye_h, dir_h, z_out / cos, gain)
                    - halfspace_line_integral(eye_h, dir_h, z_in / cos, gain)
            }
        }
    }

    #[cfg(test)]
    /// Share of the scene that stays visible through the volume along one
    /// view ray: `2^(−integral)`. The result on screen is
    /// `scene · t + colour · (1 − t)`.
    #[must_use]
    pub fn transmittance(
        &self,
        camera: sim_glam::Vec3,
        forward: sim_glam::Vec3,
        dir: sim_glam::Vec3,
        scene_distance: f32,
    ) -> f32 {
        transmittance_of(self.integral(camera, forward, dir, scene_distance))
    }

    /// How near the volume is to `point`: 0 inside the box, else the
    /// distance to its centre (only used to choose which volumes to keep).
    fn distance_to(&self, point: sim_glam::Vec3) -> f32 {
        let l = self.local_from_ue.transform_point3(point).abs();
        if l.max_element() <= 1.0 {
            0.0
        } else {
            (point - self.center_ue).length()
        }
    }
}

#[cfg(test)]
/// Rays more than about 89.99° off the view axis do not occur on screen;
/// the floor keeps the division finite.
const MIN_VIEW_COSINE: f32 = 1.0e-4;

#[cfg(test)]
fn transmittance_of(integral: f32) -> f32 {
    if integral.is_nan() {
        return 1.0;
    }
    (-integral).exp2().clamp(0.0, 1.0)
}

#[cfg(test)]
/// Entry and exit of a ray through the box with faces at ±1 (`o` origin,
/// `d` direction, any length): `None` when the ray misses it or leaves it
/// behind the origin.
fn slab_span(o: [f32; 3], d: [f32; 3]) -> Option<(f32, f32)> {
    let mut t_in = 0.0_f32;
    let mut t_out = f32::INFINITY;
    for axis in 0..3 {
        // A ray along a face: a tiny component stands in for zero (as in
        // the shader), which keeps the test free of divisions by zero.
        let di = if d[axis].abs() < 1.0e-20 {
            1.0e-20
        } else {
            d[axis]
        };
        let a = (-1.0 - o[axis]) / di;
        let b = (1.0 - o[axis]) / di;
        t_in = t_in.max(a.min(b));
        t_out = t_out.min(a.max(b));
    }
    (t_in.is_finite() && t_out.is_finite() && t_out > t_in).then_some((t_in, t_out))
}

#[cfg(test)]
/// Density of a linear half-space volume integrated along the straight line
/// from the eye to the point `len` UU along the ray. `eye_h` is the eye's
/// height above the plane, `dir_h` the height gained per UU of ray and
/// `gain` the density per UU of depth under the plane.
///
/// The closed form the original's shader uses (it is exact but for the
/// epsilon in the divisor).
#[must_use]
pub fn halfspace_line_integral(eye_h: f32, dir_h: f32, len: f32, gain: f32) -> f32 {
    let point_h = eye_h + dir_h * len;
    let k = if eye_h <= 0.0 { 1.0 } else { 0.0 };
    let c = ((1.0 - 2.0 * k) * point_h).min(0.0);
    let g = k * (point_h + eye_h) - c * c / ((eye_h - point_h).abs() + HALFSPACE_EPSILON);
    -0.5 * gain * len * g
}

/// Builds the volumes of a level. `bounds_of` gives the bounds of a volume
/// mesh; what cannot be drawn is described in `notes`.
pub fn from_scene<'a>(
    infos: &[FogVolumeInfo],
    bounds_of: impl Fn(&str) -> Option<&'a BoundsEntry>,
    notes: &mut Vec<String>,
) -> Vec<FogVolume> {
    let mut out = Vec::new();
    for info in infos {
        let bounds = info.mesh.as_deref().and_then(&bounds_of);
        match FogVolume::from_info(info, bounds) {
            Ok(Some(v)) => out.push(v),
            Ok(None) => {}
            Err(e) => notes.push(format!(
                "fog volume {} is not drawn: {e}",
                info.density.actor_name
            )),
        }
        if info.density.params.get("FogVolumeActors").is_some() {
            notes.push(format!(
                "fog volume {}: FogVolumeActors is not supported (only the actor's own mesh \
                 shapes the volume)",
                info.density.actor_name
            ));
        }
    }
    out
}

/// One volume as the fog pass's shader reads it (layout shared with the
/// WGSL `FogVolume` struct).
#[derive(ShaderType, Clone, Copy, Debug, Default, PartialEq)]
pub struct FogVolumeGpu {
    /// Render space to the box with faces at ±1.
    pub local_from_world: Mat4,
    /// Half-space plane in render space: `xyz · p + w` is the height (UU)
    /// of a render-space point above the plane.
    pub plane: Vec4,
    /// `x`: 0 constant density, 1 linear half-space; `y`: density per UU, or
    /// the density gained per UU under the plane; `z`: start distance;
    /// `w`: maximum distance.
    pub params: Vec4,
    /// Fog colour (render radiance) in `rgb`.
    pub color: Vec4,
}

fn to_mat4(m: sim_glam::Mat4) -> Mat4 {
    Mat4::from_cols_array(&m.to_cols_array())
}

/// The affine map from render space to UE3 space.
fn ue_from_render() -> sim_glam::Mat4 {
    let at = |v: sim_glam::Vec3| bevy_pos_to_ue(v, crate::SCALE);
    let o = at(sim_glam::Vec3::ZERO);
    sim_glam::Mat4::from_cols(
        (at(sim_glam::Vec3::X) - o).extend(0.0),
        (at(sim_glam::Vec3::Y) - o).extend(0.0),
        (at(sim_glam::Vec3::Z) - o).extend(0.0),
        o.extend(1.0),
    )
}

impl FogVolume {
    /// The volume for the shader; `scale` converts UE3 scene units to
    /// render radiance (`post::render_per_ue`).
    #[must_use]
    pub fn to_gpu(&self, scale: f32) -> FogVolumeGpu {
        let ue_from_render = ue_from_render();
        let (kind, amount, plane) = match self.density {
            FogDensity::Constant { density } => (0.0, density, Vec4::ZERO),
            FogDensity::LinearHalfspace { plane, factor } => {
                // height(p) = n · (A p + o) + W for the render point p.
                let n = sim_glam::Vec3::new(plane[0], plane[1], plane[2]);
                let col = |i: usize| ue_from_render.col(i).truncate();
                (
                    1.0,
                    factor * HALFSPACE_FACTOR_SCALE,
                    Vec4::new(
                        n.dot(col(0)),
                        n.dot(col(1)),
                        n.dot(col(2)),
                        n.dot(col(3)) + plane[3],
                    ),
                )
            }
        };
        let c = self.color * scale;
        FogVolumeGpu {
            local_from_world: to_mat4(self.local_from_ue * ue_from_render),
            plane,
            params: Vec4::new(kind, amount, self.start_distance, self.max_distance),
            color: Vec4::new(c.x, c.y, c.z, 0.0),
        }
    }
}

/// The level's volumes for the fog pass, seen from `camera_ue`: how many
/// are set, and their data in the order they are blended (farthest centre
/// first, like the original's translucency sort; TENTATIVE for volumes that
/// overlap, which no shipped map shows from inside both). More than
/// [`MAX_GPU_VOLUMES`]: the ones nearest to the camera are kept.
#[must_use]
pub fn pack(
    volumes: &[FogVolume],
    camera_ue: sim_glam::Vec3,
    scale: f32,
) -> (usize, [FogVolumeGpu; MAX_GPU_VOLUMES]) {
    let mut order: Vec<&FogVolume> = volumes.iter().collect();
    if order.len() > MAX_GPU_VOLUMES {
        order.sort_by(|a, b| {
            a.distance_to(camera_ue)
                .total_cmp(&b.distance_to(camera_ue))
        });
        order.truncate(MAX_GPU_VOLUMES);
    }
    let far = |v: &FogVolume| (v.center_ue - camera_ue).length_squared();
    order.sort_by(|a, b| far(b).total_cmp(&far(a)));
    let mut out = [FogVolumeGpu::default(); MAX_GPU_VOLUMES];
    for (slot, v) in out.iter_mut().zip(&order) {
        *slot = v.to_gpu(scale);
    }
    (order.len(), out)
}

#[cfg(test)]
/// The shader's evaluation, line for line (`fog_volume_transmittance` in
/// `fog_volumes.wgsl`): `camera` and the unit `dir` in render space,
/// `cos_view` the cosine between the ray and the view axis,
/// `scene_distance` in UU.
#[must_use]
pub fn gpu_transmittance(
    v: &FogVolumeGpu,
    camera: Vec3,
    dir: Vec3,
    cos_view: f32,
    uu_per_unit: f32,
    scene_distance: f32,
) -> f32 {
    let o = v.local_from_world.transform_point3(camera);
    let d = v.local_from_world.transform_vector3(dir);
    let Some((t_in, t_out)) = slab_span(o.to_array(), d.to_array()) else {
        return 1.0;
    };
    let cap = (scene_distance * cos_view)
        .min(v.params.w)
        .min(HARD_MAX_DISTANCE);
    let z_in = (t_in * uu_per_unit * cos_view).min(cap);
    let z_out = (t_out * uu_per_unit * cos_view).min(cap);
    let integral = if v.params.x < 0.5 {
        let start = v.params.z;
        v.params.y * ((z_out - start).max(0.0) - (z_in - start).max(0.0))
    } else {
        let eye_h = v.plane.truncate().dot(camera) + v.plane.w;
        let dir_h = v.plane.truncate().dot(dir) / uu_per_unit;
        halfspace_line_integral(eye_h, dir_h, z_out / cos_view, v.params.y)
            - halfspace_line_integral(eye_h, dir_h, z_in / cos_view, v.params.y)
    };
    transmittance_of(integral)
}

#[cfg(test)]
mod tests {
    use super::*;
    use asamu_assets::scene::{FogComponentInfo, UeProps};
    use asamu_core::coords::{ue_dir_to_bevy, ue_pos_to_bevy};
    use sim_glam::Vec3 as V;
    use std::collections::BTreeMap;

    /// `EngineMeshes.Cube`-like bounds: a box of ±128 UU about the origin.
    const CUBE: BoundsEntry = BoundsEntry {
        origin: [0.0, 0.0, 0.0],
        box_extent: [128.0, 128.0, 128.0],
        sphere_radius: 221.7,
    };

    fn info(class: &str, params: &str, scale: [f32; 3], at: [f32; 3]) -> FogVolumeInfo {
        FogVolumeInfo {
            density: FogComponentInfo {
                level: 0,
                actor_slot: 1,
                actor_name: "Fog_1".to_owned(),
                actor_class: "Engine.FogVolumeConstantDensityInfo".to_owned(),
                location_ue: V::from_array(at),
                class: format!("Engine.{class}"),
                params: UeProps::parse(params).unwrap(),
            },
            material: Some("Map.FogMI".to_owned()),
            material_vectors: BTreeMap::new(),
            material_scalars: BTreeMap::new(),
            mesh: Some("EngineMeshes.Cube".to_owned()),
            ue_mesh_to_world: Some(
                sim_glam::Mat4::from_translation(V::from_array(at))
                    * sim_glam::Mat4::from_scale(V::from_array(scale)),
            ),
        }
    }

    /// A constant-density box of ±1280 UU about the origin.
    fn constant(density: f32, extra: &str) -> FogVolume {
        let i = info(
            "FogVolumeConstantDensityComponent",
            &format!(r#"{{"Density": {density}, "bEnabled": true{extra}}}"#),
            [10.0, 10.0, 10.0],
            [0.0, 0.0, 0.0],
        );
        FogVolume::from_info(&i, Some(&CUBE)).unwrap().unwrap()
    }

    /// A half-space box of ±1280 UU about (0, 0, `z`), plane at that height
    /// with the fog below (the stored form: normal +Z, `W = −z`).
    fn halfspace(factor: f32, z: f32) -> FogVolume {
        let i = info(
            "FogVolumeLinearHalfspaceDensityComponent",
            &format!(
                r#"{{"PlaneDistanceFactor": {factor},
                     "HalfspacePlane": {{"X": 0.0, "Y": 0.0, "Z": 1.0, "W": {}}}}}"#,
                -z
            ),
            [10.0, 10.0, 10.0],
            [0.0, 0.0, z],
        );
        FogVolume::from_info(&i, Some(&CUBE)).unwrap().unwrap()
    }

    fn close(a: f32, b: f32, tol: f32) -> bool {
        (a - b).abs() <= tol * a.abs().max(b.abs()).max(1.0e-6)
    }

    #[test]
    fn a_volume_is_read_from_the_scene_values() {
        let v = constant(
            3.0e-6,
            r#", "MaxDistance": 50000.0, "StartDistance": 100.0"#,
        );
        assert_eq!(v.density, FogDensity::Constant { density: 3.0e-6 });
        assert_eq!((v.start_distance, v.max_distance), (100.0, 50000.0));
        assert_eq!(v.center_ue, V::ZERO);
        // The box: ±128 UU of the mesh times the scale of 10.
        let corner = v
            .local_from_ue
            .transform_point3(V::new(1280.0, -1280.0, 640.0));
        assert!((corner - V::new(1.0, -1.0, 0.5)).length() < 1.0e-5);
        // Missing values are the class defaults.
        let i = info(
            "FogVolumeConstantDensityComponent",
            "{}",
            [1.0, 1.0, 1.0],
            [5.0, 6.0, 7.0],
        );
        let d = FogVolume::from_info(&i, Some(&CUBE)).unwrap().unwrap();
        assert_eq!(d.density, FogDensity::Constant { density: 0.0005 });
        assert_eq!((d.start_distance, d.max_distance), (0.0, 65535.0));
        assert_eq!(d.color, V::new(0.5, 0.5, 0.7));
        assert_eq!(d.center_ue, V::new(5.0, 6.0, 7.0));
        // Bounds that are not centred move the box.
        let off = BoundsEntry {
            origin: [10.0, 0.0, 0.0],
            box_extent: [1.0, 2.0, 3.0],
            sphere_radius: 0.0,
        };
        let o = FogVolume::from_info(&i, Some(&off)).unwrap().unwrap();
        assert_eq!(o.center_ue, V::new(15.0, 6.0, 7.0));
    }

    #[test]
    fn the_colour_is_the_fog_materials_then_the_components() {
        let mut i = info(
            "FogVolumeConstantDensityComponent",
            r#"{"SimpleLightColor": {"R": 0.1, "G": 0.2, "B": 0.3, "A": 1.0}}"#,
            [1.0, 1.0, 1.0],
            [0.0, 0.0, 0.0],
        );
        let c = |i: &FogVolumeInfo| FogVolume::from_info(i, Some(&CUBE)).unwrap().unwrap().color;
        assert_eq!(c(&i), V::new(0.1, 0.2, 0.3));
        i.material_vectors
            .insert("EmissiveColor".to_owned(), [0.4, 0.7, 0.5, 1.0]);
        assert_eq!(c(&i), V::new(0.4, 0.7, 0.5));
        // Another parameter is not the colour; a negative value is no light.
        i.material_vectors.clear();
        i.material_vectors
            .insert("Other".to_owned(), [9.0, 9.0, 9.0, 1.0]);
        assert_eq!(c(&i), V::new(0.1, 0.2, 0.3));
        i.material_vectors
            .insert("EmissiveColor".to_owned(), [-1.0, 0.5, 0.0, 1.0]);
        assert_eq!(c(&i), V::new(0.0, 0.5, 0.0));
    }

    #[test]
    fn volumes_that_draw_nothing_or_cannot_be_drawn() {
        let make =
            |class: &str, params: &str| info(class, params, [1.0, 1.0, 1.0], [0.0, 0.0, 0.0]);
        let constant = "FogVolumeConstantDensityComponent";
        let half = "FogVolumeLinearHalfspaceDensityComponent";
        // Disabled, or no density: nothing, as in the original.
        for (class, params) in [
            (constant, r#"{"bEnabled": false, "Density": 1.0}"#),
            (constant, r#"{"Density": 0.0}"#),
            (constant, r#"{"Density": -0.001}"#),
            (
                half,
                r#"{"PlaneDistanceFactor": 0.0, "HalfspacePlane": {"Z": 1.0, "W": 0.0}}"#,
            ),
        ] {
            assert_eq!(
                FogVolume::from_info(&make(class, params), Some(&CUBE)),
                Ok(None),
                "{params}"
            );
        }
        // Not drawable: said why.
        let err =
            |i: &FogVolumeInfo, b: Option<&BoundsEntry>| FogVolume::from_info(i, b).unwrap_err();
        assert!(
            err(
                &make("FogVolumeSphericalDensityComponent", "{}"),
                Some(&CUBE)
            )
            .contains("not drawn")
        );
        assert!(
            err(&make("FogVolumeConeDensityComponent", "{}"), Some(&CUBE)).contains("not drawn")
        );
        assert!(err(&make(half, "{}"), Some(&CUBE)).contains("HalfspacePlane"));
        assert!(err(&make(constant, "{}"), None).contains("bounds"));
        let mut no_mesh = make(constant, "{}");
        no_mesh.ue_mesh_to_world = None;
        assert!(err(&no_mesh, Some(&CUBE)).contains("no volume mesh"));
        let mut flat = make(constant, "{}");
        flat.ue_mesh_to_world = Some(sim_glam::Mat4::from_scale(V::new(1.0, 0.0, 1.0)));
        assert!(err(&flat, Some(&CUBE)).contains("inverted"));
        let empty = BoundsEntry {
            origin: [0.0; 3],
            box_extent: [1.0, 0.0, 1.0],
            sphere_radius: 0.0,
        };
        assert!(err(&make(constant, "{}"), Some(&empty)).contains("no box"));
        // A disabled volume is nothing even when it could not be drawn.
        assert_eq!(
            FogVolume::from_info(
                &make(
                    "FogVolumeSphericalDensityComponent",
                    r#"{"bEnabled": false}"#
                ),
                None
            ),
            Ok(None)
        );
        // `from_scene` keeps the drawable ones and reports the others.
        let mut notes = Vec::new();
        let infos = [
            make(constant, "{}"),
            make("FogVolumeConeDensityComponent", "{}"),
            make(constant, r#"{"bEnabled": false}"#),
            make(
                constant,
                r#"{"FogVolumeActors": ["Map.StaticMeshActor_3"]}"#,
            ),
        ];
        let out = from_scene(&infos, |_| Some(&CUBE), &mut notes);
        assert_eq!(out.len(), 2);
        assert_eq!(notes.len(), 2, "{notes:?}");
        assert!(notes[0].contains("Fog_1") && notes[0].contains("not drawn"));
        assert!(notes[1].contains("FogVolumeActors"));
        let none = from_scene(&infos, |_| None, &mut notes);
        assert!(none.is_empty());
    }

    #[test]
    fn constant_density_from_inside_is_an_exponential_of_the_view_depth() {
        let v = constant(1.0e-3, "");
        let eye = V::new(-280.0, 0.0, 0.0);
        // Looking along +X: the far face is 1560 UU away.
        let t = |scene: f32| v.transmittance(eye, V::X, V::X, scene);
        assert!(close(t(1.0e9), (-1.56_f32).exp2(), 1.0e-5));
        // A surface inside the volume ends the fog there.
        assert!(close(t(500.0), (-0.5_f32).exp2(), 1.0e-5));
        assert!(close(t(1.0), (-0.001_f32).exp2(), 1.0e-6));
        assert_eq!(t(0.0), 1.0);
        // The same thing as a distance fog of that density: halving every
        // 1000 UU.
        assert!(close(t(1000.0), 0.5, 1.0e-5));
    }

    #[test]
    fn constant_density_measures_view_depth_not_ray_length() {
        let v = constant(1.0e-3, "");
        // 45° off the view axis towards +Y: the ray leaves through the +X
        // face (or +Y, the box is a cube) after 1280·√2 UU, at a view depth
        // of 1280 UU, and that depth is what counts.
        let dir = V::new(1.0, 1.0, 0.0).normalize();
        let i = v.integral(V::ZERO, V::X, dir, 1.0e9);
        assert!(close(i, 1.28, 1.0e-5), "{i}");
        // A surface 1000 UU along that ray is at depth 707.
        let i = v.integral(V::ZERO, V::X, dir, 1000.0);
        assert!(close(i, 1.0e-3 * 1000.0 * dir.x, 1.0e-5), "{i}");
    }

    #[test]
    fn from_outside_the_front_faces_subtract() {
        let v = constant(1.0e-3, "");
        let eye = V::new(-5000.0, 0.0, 0.0);
        let i = |scene: f32| v.integral(eye, V::X, V::X, scene);
        // Through the whole box: 2560 UU of fog.
        assert!(close(i(1.0e9), 2.56, 1.0e-5));
        // A surface in front of the volume: none. Inside it: from the
        // front face (3720 UU away) to the surface.
        assert_eq!(i(3000.0), 0.0);
        assert!(close(i(4720.0), 1.0, 1.0e-4));
        // A ray that misses the box, and a box behind the eye.
        assert_eq!(v.integral(eye, V::X, V::new(0.0, 1.0, 0.0), 1.0e9), 0.0);
        assert_eq!(v.integral(V::new(5000.0, 0.0, 0.0), V::X, V::X, 1.0e9), 0.0);
        assert_eq!(v.ray_span(V::new(5000.0, 0.0, 0.0), V::X), None);
        let (t_in, t_out) = v.ray_span(V::ZERO, V::X).unwrap();
        assert_eq!(t_in, 0.0);
        assert!(close(t_out, 1280.0, 1.0e-5));
    }

    #[test]
    fn start_and_maximum_distances_bound_the_integral() {
        // Start distance: no fog nearer than that depth.
        let v = constant(1.0e-3, r#", "StartDistance": 1000.0"#);
        let i = |scene: f32| v.integral(V::new(-1280.0, 0.0, 0.0), V::X, V::X, scene);
        assert_eq!(i(900.0), 0.0);
        assert!(close(i(1500.0), 0.5, 1.0e-5));
        assert!(close(i(1.0e9), 1.56, 1.0e-5));
        // Maximum distance: none beyond it.
        let v = constant(1.0e-3, r#", "MaxDistance": 400.0"#);
        assert!(close(v.integral(V::ZERO, V::X, V::X, 1.0e9), 0.4, 1.0e-5));
        // The shader's own limit of 65535 UU holds whatever the volume says.
        let big = info(
            "FogVolumeConstantDensityComponent",
            r#"{"Density": 1.0e-5, "MaxDistance": 650000.0}"#,
            [2000.0, 2000.0, 2000.0],
            [0.0, 0.0, 0.0],
        );
        let big = FogVolume::from_info(&big, Some(&CUBE)).unwrap().unwrap();
        assert!(close(
            big.integral(V::ZERO, V::X, V::X, 1.0e9),
            0.65535,
            1.0e-5
        ));
    }

    /// Numeric integral of the half-space density along a ray.
    fn halfspace_numeric(eye_h: f32, dir_h: f32, len: f32, gain: f32) -> f32 {
        let n = 20_000;
        let step = f64::from(len) / f64::from(n);
        let mut sum = 0.0_f64;
        for s in 0..n {
            let t = (f64::from(s) + 0.5) * step;
            let h = f64::from(eye_h) + f64::from(dir_h) * t;
            sum += f64::from(gain) * (-h).max(0.0) * step;
        }
        #[allow(clippy::cast_possible_truncation)]
        let out = sum as f32;
        out
    }

    #[test]
    fn the_halfspace_closed_form_is_the_integral_of_the_density() {
        let gain = 0.1 * HALFSPACE_FACTOR_SCALE;
        for (eye_h, dir_h, len) in [
            (500.0, -0.5, 3000.0),  // above, looking down into the fog
            (500.0, 0.5, 3000.0),   // above, looking up: no fog
            (500.0, -0.1, 1000.0),  // above, ends above the plane
            (-400.0, -0.3, 2000.0), // below, going deeper
            (-400.0, 0.8, 2000.0),  // below, leaving through the plane
            (-400.0, 0.1, 1000.0),  // below, stays below
            (-400.0, 0.0, 1500.0),  // level, below
            (0.0, -1.0, 100.0),     // on the plane, straight down
        ] {
            let closed = halfspace_line_integral(eye_h, dir_h, len, gain);
            let numeric = halfspace_numeric(eye_h, dir_h, len, gain);
            assert!(
                close(closed, numeric, 2.0e-3),
                "{eye_h} {dir_h} {len}: {closed} vs {numeric}"
            );
            assert!(closed >= -1.0e-6);
        }
        assert_eq!(halfspace_line_integral(500.0, 0.5, 3000.0, gain), 0.0);
        assert_eq!(halfspace_line_integral(500.0, -0.5, 0.0, gain), 0.0);
    }

    #[test]
    fn a_halfspace_volume_fogs_below_its_plane_only() {
        // Plane at z = 1000; the box reaches from −280 to 2280.
        let v = halfspace(0.1, 1000.0);
        let gain = 0.1 * HALFSPACE_FACTOR_SCALE;
        // From above the plane looking level: nothing.
        let above = V::new(0.0, 0.0, 1500.0);
        assert_eq!(v.integral(above, V::X, V::X, 1.0e9), 0.0);
        // From 500 UU under the plane looking level: density gain·500 over
        // the 1280 UU to the box's side.
        let below = V::new(0.0, 0.0, 500.0);
        let i = v.integral(below, V::X, V::X, 1.0e9);
        assert!(close(i, gain * 500.0 * 1280.0, 1.0e-3), "{i}");
        // Straight down from above, a floor 1200 UU below the eye (700 UU
        // under the plane): ½ · gain · 700².
        let i = v.integral(above, V::NEG_Z, V::NEG_Z, 1200.0);
        assert!(close(i, 0.5 * gain * 700.0 * 700.0, 1.0e-3), "{i}");
        // The box ends the fog: straight down without a floor, the bottom
        // face is 1280 UU under the plane.
        let i = v.integral(above, V::NEG_Z, V::NEG_Z, 1.0e9);
        assert!(close(i, 0.5 * gain * 1280.0 * 1280.0, 1.0e-3), "{i}");
        // From outside the box only the part inside it counts.
        let outside = V::new(-5000.0, 0.0, 500.0);
        let i = v.integral(outside, V::X, V::X, 1.0e9);
        assert!(close(i, gain * 500.0 * 2560.0, 1.0e-3), "{i}");
        // The stored plane is evaluated as `XYZ·p + W`: the same numbers
        // with the normal flipped put the fog on the other side.
        let flipped = FogVolume {
            density: FogDensity::LinearHalfspace {
                plane: [0.0, 0.0, -1.0, 1000.0],
                factor: 0.1,
            },
            ..v.clone()
        };
        assert_eq!(flipped.integral(below, V::X, V::X, 1.0e9), 0.0);
        assert!(flipped.integral(above, V::X, V::X, 1.0e9) > 0.0);
    }

    #[test]
    fn transmittance_is_two_to_the_minus_integral() {
        assert_eq!(transmittance_of(0.0), 1.0);
        assert_eq!(transmittance_of(1.0), 0.5);
        assert_eq!(transmittance_of(3.0), 0.125);
        assert_eq!(transmittance_of(-0.5), 1.0, "never brighter than the scene");
        assert_eq!(transmittance_of(1.0e9), 0.0);
        assert_eq!(transmittance_of(f32::NAN), 1.0);
        assert_eq!(transmittance_of(f32::INFINITY), 0.0);
    }

    /// The values of AG-ParadiseCave's volume (density 3·10⁻⁶ in a box far
    /// larger than any view): how little it adds.
    #[test]
    fn a_thin_level_wide_volume_is_subtle() {
        let i = info(
            "FogVolumeConstantDensityComponent",
            r#"{"Density": 3.0e-6}"#,
            [900.0, 800.0, 220.0],
            [0.0, 0.0, 0.0],
        );
        let v = FogVolume::from_info(&i, Some(&CUBE)).unwrap().unwrap();
        let opacity = |d: f32| 1.0 - v.transmittance(V::ZERO, V::X, V::X, d);
        assert!(opacity(100.0) < 0.0003, "the hand's distance: nothing");
        assert!(close(opacity(5000.0), 0.01034, 1.0e-3));
        assert!(close(opacity(20000.0), 0.04074, 1.0e-3));
        // The most it ever adds: 65535 UU of it.
        assert!(close(opacity(1.0e9), 0.1274, 1.0e-3));
    }

    fn render_point(p: V) -> Vec3 {
        Vec3::from_array(ue_pos_to_bevy(p, crate::SCALE).to_array())
    }

    fn render_dir(d: V) -> Vec3 {
        Vec3::from_array(ue_dir_to_bevy(d).to_array()).normalize()
    }

    #[test]
    fn the_shader_form_agrees_with_the_reference() {
        let uu = 1.0 / crate::SCALE.bevy_units_per_uu;
        let mut c = constant(4.0e-4, r#", "StartDistance": 300.0, "MaxDistance": 9000.0"#);
        // A rotated, shifted, non-uniform box.
        let m = sim_glam::Mat4::from_translation(V::new(4000.0, -2500.0, 800.0))
            * sim_glam::Mat4::from_rotation_z(0.6)
            * sim_glam::Mat4::from_scale(V::new(1500.0, 900.0, 400.0));
        c.local_from_ue = m.inverse();
        c.center_ue = m.transform_point3(V::ZERO);
        let mut h = halfspace(0.03, 700.0);
        h.local_from_ue = c.local_from_ue;
        h.center_ue = c.center_ue;
        let forward = V::new(0.9, -0.3, -0.1).normalize();
        let eyes = [
            V::new(0.0, 0.0, 600.0),
            V::new(4000.0, -2500.0, 900.0),
            V::new(4200.0, -2600.0, 500.0),
            V::new(9000.0, -6000.0, 2000.0),
        ];
        let dirs = [
            forward,
            V::new(1.0, -0.4, -0.3).normalize(),
            V::new(0.7, -0.6, 0.2).normalize(),
            V::new(0.8, 0.1, -0.5).normalize(),
            V::new(1.0, 0.0, 0.0),
        ];
        let mut inside = 0;
        for v in [&c, &h] {
            let g = v.to_gpu(2.0);
            assert_eq!(
                g.color.truncate(),
                Vec3::from_array((v.color * 2.0).to_array())
            );
            for eye in eyes {
                for dir in dirs {
                    for scene in [50.0, 1200.0, 4000.0, 1.0e9] {
                        let cos = dir.dot(forward);
                        if cos <= 0.0 {
                            continue;
                        }
                        let want = v.transmittance(eye, forward, dir, scene);
                        let got = gpu_transmittance(
                            &g,
                            render_point(eye),
                            render_dir(dir),
                            cos,
                            uu,
                            scene,
                        );
                        assert!(
                            (want - got).abs() < 2.0e-3,
                            "{:?} eye {eye} dir {dir} scene {scene}: {want} vs {got}",
                            v.density
                        );
                        if want < 0.999 {
                            inside += 1;
                        }
                    }
                }
            }
        }
        assert!(inside > 20, "the cases exercise the fog ({inside})");
    }

    #[test]
    fn the_uniform_holds_the_nearest_volumes_far_to_near() {
        let at = |x: f32| {
            let i = info(
                "FogVolumeConstantDensityComponent",
                r#"{"Density": 0.001}"#,
                [1.0, 1.0, 1.0],
                [x, 0.0, 0.0],
            );
            FogVolume::from_info(&i, Some(&CUBE)).unwrap().unwrap()
        };
        let volumes: Vec<FogVolume> = (0..11_u8).map(|n| at(f32::from(n) * 1000.0)).collect();
        // Nothing to draw.
        let (n, data) = pack(&[], V::ZERO, 1.0);
        assert_eq!(n, 0);
        assert!(data.iter().all(|v| *v == FogVolumeGpu::default()));
        // Three: all, the farthest first.
        let (n, data) = pack(&volumes[..3], V::new(-500.0, 0.0, 0.0), 1.0);
        assert_eq!(n, 3);
        let centre = |g: &FogVolumeGpu| {
            let p = g.local_from_world.inverse().transform_point3(Vec3::ZERO);
            bevy_pos_to_ue(V::from_array(p.to_array()), crate::SCALE).x
        };
        let xs: Vec<f32> = data[..n].iter().map(|g| centre(g).round()).collect();
        assert_eq!(xs, [2000.0, 1000.0, 0.0]);
        assert_eq!(data[3], FogVolumeGpu::default());
        // Eleven, seen from inside the one at 5000: the eight nearest.
        let (n, data) = pack(&volumes, V::new(5010.0, 0.0, 0.0), 1.0);
        assert_eq!(n, MAX_GPU_VOLUMES);
        let mut xs: Vec<f32> = data.iter().map(|g| centre(g).round()).collect();
        assert_eq!(
            xs[MAX_GPU_VOLUMES - 1],
            5000.0,
            "the one around the eye last"
        );
        xs.sort_by(f32::total_cmp);
        assert_eq!(
            xs,
            [
                2000.0, 3000.0, 4000.0, 5000.0, 6000.0, 7000.0, 8000.0, 9000.0
            ]
        );
    }

    #[test]
    fn the_shader_declares_the_uniforms_layout() {
        let start = WGSL.find("struct FogVolume {").unwrap();
        let end = start + WGSL[start..].find('}').unwrap();
        let fields: Vec<&str> = WGSL[start..end]
            .lines()
            .map(str::trim)
            .filter(|l| !l.starts_with("//") && l.contains(':'))
            .collect();
        assert_eq!(
            fields,
            [
                "local_from_world: mat4x4<f32>,",
                "plane: vec4<f32>,",
                "params: vec4<f32>,",
                "color: vec4<f32>,",
            ]
        );
        assert_eq!(FogVolumeGpu::min_size().get(), 64 + 3 * 16);
        assert!(WGSL.contains("const FOG_VOLUME_HARD_MAX_DISTANCE: f32 = 65535.0;"));
        assert!(WGSL.contains("fn fog_volume_transmittance("));
    }

    /// The shipped levels (skips without converted data): every fog volume
    /// is read, the kinds and numbers are the ones the document lists, and
    /// the colour rule agrees with what the engine writes at run time.
    #[test]
    fn the_real_levels_fog_volumes() {
        let Some(dir) = std::env::var_os("ASAMU_CONVERTED_DIR") else {
            eprintln!("skipped: ASAMU_CONVERTED_DIR is not set");
            return;
        };
        let dir = asamu_assets::ConvertedDir::new(std::path::PathBuf::from(dir));
        let Ok(manifests) = dir.load_manifests() else {
            eprintln!("skipped: no converted manifests");
            return;
        };
        let Some(meshes) = manifests.meshes.as_ref() else {
            eprintln!("skipped: no mesh manifest");
            return;
        };
        // (map, constant, half-space, drawn)
        let expected = [
            ("AG-Workshop", 2, 0, 2),
            ("AG-ParadiseCave", 1, 0, 1),
            ("AG-BeautifulCity", 1, 0, 1),
            ("AG-Darkcave", 0, 7, 7),
            ("AG-StarHaven", 1, 0, 0),
            ("AG-IceCave", 1, 4, 5),
            ("AG-Epilogue", 2, 0, 2),
            ("TheCore", 0, 0, 0),
        ];
        for (map, constant, half, drawn) in expected {
            let Ok(scene) = dir.load_scene(map) else {
                eprintln!("skipped: {map} is not converted");
                continue;
            };
            let infos = &scene.atmosphere.fog_volumes;
            let count = |name: &str| {
                infos
                    .iter()
                    .filter(|i| i.density.class_name() == name)
                    .count()
            };
            assert_eq!(
                count("FogVolumeConstantDensityComponent"),
                constant,
                "{map}"
            );
            assert_eq!(
                count("FogVolumeLinearHalfspaceDensityComponent"),
                half,
                "{map}"
            );
            let mut notes = Vec::new();
            let volumes = from_scene(
                infos,
                |mesh| {
                    meshes
                        .get_for_package(mesh, &scene.package)
                        .and_then(|(_, e)| e.bounds_ue.as_ref())
                },
                &mut notes,
            );
            assert!(notes.is_empty(), "{map}: {notes:?}");
            assert_eq!(volumes.len(), drawn, "{map}");
            // No fog volume mesh is a drawn surface.
            assert_eq!(scene.stats.fog_volume_meshes, infos.len(), "{map}");
            assert!(
                scene
                    .meshes
                    .iter()
                    .all(|m| { !asamu_assets::scene::is_fog_volume_actor_class(&m.actor_class) })
            );
            for i in infos {
                // Every shipped volume is a cube shaped by its own mesh.
                assert_eq!(i.mesh.as_deref(), Some("EngineMeshes.Cube"), "{map}");
                assert!(i.density.params.get("FogVolumeActors").is_none());
                // The engine writes `SimpleLightColor` into the material's
                // colour parameter when a volume attaches, in actor order:
                // the last volume on a material decides. The cooked value
                // is that one already.
                let Some(material) = i.material_vectors.get("EmissiveColor") else {
                    continue;
                };
                let last = infos
                    .iter()
                    .rev()
                    .find(|o| o.material == i.material)
                    .and_then(|o| o.density.params.linear_color("SimpleLightColor"));
                let last = last.unwrap_or(defaults::SIMPLE_LIGHT_COLOR);
                for c in 0..3 {
                    assert!(
                        (material[c] - last[c]).abs() < 1.0e-6,
                        "{map} {}: {material:?} vs {last:?}",
                        i.density.actor_name
                    );
                }
            }
            // The horizontal planes: `W` is minus the actor's height.
            for i in infos {
                if let Some(p) = i.density.params.plane("HalfspacePlane")
                    && i.density.class_name() == "FogVolumeLinearHalfspaceDensityComponent"
                {
                    assert!((p[2].abs() - 1.0).abs() < 1.0e-5, "{map}: {p:?}");
                    assert!(
                        (p[3] + i.density.location_ue.z).abs() < 0.01,
                        "{map}: {p:?} at {}",
                        i.density.location_ue.z
                    );
                }
            }
            if map == "AG-ParadiseCave" {
                let v = &volumes[0];
                assert_eq!(v.density, FogDensity::Constant { density: 3.0e-6 });
                assert_eq!((v.start_distance, v.max_distance), (0.0, 65535.0));
                // Inside it at the first checkpoint.
                let eye = V::new(147_241.0, -154_469.0, 95_875.0);
                assert_eq!(v.ray_span(eye, V::X).map(|s| s.0), Some(0.0));
                let far = 1.0 - v.transmittance(eye, V::X, V::X, 1.0e9);
                assert!(far < 0.13, "at most 12.7 % of its colour: {far}");
            }
        }
    }
}
