//! UE3 light components → physically based render lights (APPROXIMATION).
//!
//! UE3 lights have no physical units: a `PointLightComponent` has a unitless
//! `Brightness`, an sRGB `LightColor`, a `Radius` beyond which it contributes
//! nothing and a `FalloffExponent` shaping the attenuation inside it. Most of
//! the original's look comes from **baked lightmaps**, which the runtime does
//! not use yet, so dynamic lights in the recreation can only approximate it.
//!
//! The mapping below is ours, chosen so a level reads plausibly under Bevy's
//! default camera exposure; it is not recovered from the original and must
//! never feed gameplay:
//!
//! | UE3 | Render light | Mapping |
//! |---|---|---|
//! | point (`PointLightComponent`, `DominantPointLightComponent`) | point light | luminous power = `Brightness × K_point × Radius_m²`, range = `Radius_m` |
//! | spot (`SpotLightComponent`, `DominantSpotLightComponent`) | spot light | as point; inner/outer half-angles from `InnerConeAngle`/`OuterConeAngle` (degrees, clamped to 0..89.9°) |
//! | directional (`DirectionalLightComponent`, `DominantDirectionalLightComponent`) | directional light | illuminance = `Brightness × K_directional` |
//! | sky (`SkyLightComponent`) | ambient term | ambient brightness += `Brightness × K_sky` |
//!
//! `Radius_m` is the radius converted with the presentation scale. With
//! `K_point = 2500 lm/m²` a light of brightness 1 gives about 800 lux at half
//! its radius, comparable to Bevy's default 1,000,000 lm / 20 m point light.
//! The constants are [`LightMapping`] fields so the app can expose them.

use asamu_core::WorldScale;
use asamu_core::glam::Vec3;

use crate::scene::{SceneLight, UeLightKind};

/// Scale factors of the UE3 → render light mapping (see the module docs).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LightMapping {
    /// Lumens per unit `Brightness` per square metre of `Radius` (point and
    /// spot lights).
    pub point_lumens_per_brightness_m2: f32,
    /// Lux per unit `Brightness` (directional lights).
    pub directional_lux_per_brightness: f32,
    /// Ambient brightness per unit sky light `Brightness`.
    pub sky_ambient_per_brightness: f32,
    /// Ambient brightness always added (stands in for the baked indirect
    /// light of the original's lightmaps, which are not rendered yet).
    pub base_ambient: f32,
}

impl Default for LightMapping {
    fn default() -> Self {
        Self {
            point_lumens_per_brightness_m2: 2500.0,
            directional_lux_per_brightness: 10_000.0,
            sky_ambient_per_brightness: 300.0,
            base_ambient: 150.0,
        }
    }
}

/// What kind of render light a UE3 light becomes.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum RenderLightKind {
    /// Point light: luminous power in lumens, range in render units.
    Point {
        /// Luminous power (lm).
        lumens: f32,
        /// Range (render units).
        range: f32,
    },
    /// Spot light.
    Spot {
        /// Luminous power (lm).
        lumens: f32,
        /// Range (render units).
        range: f32,
        /// Inner half-angle (radians).
        inner: f32,
        /// Outer half-angle (radians).
        outer: f32,
    },
    /// Directional light: illuminance in lux.
    Directional {
        /// Illuminance (lx).
        lux: f32,
    },
    /// Ambient contribution.
    Ambient {
        /// Ambient brightness to add.
        brightness: f32,
    },
}

/// A render light in **UE3 coordinates** (the app converts location and
/// direction with `asamu_core::coords`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RenderLight {
    /// Kind and intensity.
    pub kind: RenderLightKind,
    /// Location (UE3, UU).
    pub location_ue: Vec3,
    /// Direction the light points along (UE3, unit).
    pub direction_ue: Vec3,
    /// sRGB colour bytes (`LightColor` R, G, B).
    pub color_srgb: [u8; 3],
    /// `CastShadows`.
    pub cast_shadows: bool,
}

/// Maps one scene light. Returns `None` for lights with no effect (disabled,
/// zero or negative brightness, unknown class, non-finite values).
#[must_use]
pub fn map_light(
    light: &SceneLight,
    mapping: &LightMapping,
    scale: WorldScale,
) -> Option<RenderLight> {
    if !light.enabled || !(light.brightness.is_finite() && light.brightness > 0.0) {
        return None;
    }
    if !(light.location_ue.is_finite() && light.direction_ue.is_finite()) {
        return None;
    }
    let radius_render = |r: Option<f32>| {
        r.filter(|r| r.is_finite() && *r > 0.0)
            .map(|r| r * scale.bevy_units_per_uu)
    };
    let kind = match light.kind {
        UeLightKind::Point => {
            let range = radius_render(light.radius)?;
            RenderLightKind::Point {
                lumens: light.brightness * mapping.point_lumens_per_brightness_m2 * range * range,
                range,
            }
        }
        UeLightKind::Spot => {
            let range = radius_render(light.radius)?;
            let max = 89.9_f32.to_radians();
            // Missing angles take the `Engine.SpotLightComponent` class
            // defaults (CONFIRMED: `asamu-inspect defaults --inherited
            // Engine.u SpotLightComponent` gives OuterConeAngle 44.0 and no
            // InnerConeAngle, i.e. 0). The importer writes merged values, so
            // this only applies to hand-written or damaged scenes.
            let outer = light
                .outer_cone_angle
                .filter(|a| a.is_finite())
                .unwrap_or(44.0)
                .to_radians()
                .clamp(0.0, max);
            let inner = light
                .inner_cone_angle
                .filter(|a| a.is_finite())
                .unwrap_or(0.0)
                .to_radians()
                .clamp(0.0, outer);
            RenderLightKind::Spot {
                lumens: light.brightness * mapping.point_lumens_per_brightness_m2 * range * range,
                range,
                inner,
                outer,
            }
        }
        UeLightKind::Directional => RenderLightKind::Directional {
            lux: light.brightness * mapping.directional_lux_per_brightness,
        },
        UeLightKind::Sky => RenderLightKind::Ambient {
            brightness: light.brightness * mapping.sky_ambient_per_brightness,
        },
        UeLightKind::Other => return None,
    };
    // Large but finite inputs (or mapping constants) can overflow `f32`; an
    // infinite intensity would poison the renderer's lighting, so such a
    // light is dropped like any other light without a usable effect.
    let finite = match kind {
        RenderLightKind::Point { lumens, range } => lumens.is_finite() && range.is_finite(),
        RenderLightKind::Spot {
            lumens,
            range,
            inner,
            outer,
        } => lumens.is_finite() && range.is_finite() && inner.is_finite() && outer.is_finite(),
        RenderLightKind::Directional { lux } => lux.is_finite(),
        RenderLightKind::Ambient { brightness } => brightness.is_finite(),
    };
    if !finite {
        return None;
    }
    Some(RenderLight {
        kind,
        location_ue: light.location_ue,
        direction_ue: light.direction_ue,
        color_srgb: light.color_srgb,
        cast_shadows: light.cast_shadows,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn light(kind: UeLightKind) -> SceneLight {
        SceneLight {
            level: 0,
            actor_slot: 3,
            actor_name: "L".to_owned(),
            light_class: "PointLightComponent".to_owned(),
            kind,
            location_ue: Vec3::new(100.0, 0.0, 50.0),
            direction_ue: Vec3::X,
            brightness: 2.0,
            color_srgb: [255, 128, 0],
            radius: Some(500.0),
            falloff_exponent: Some(2.0),
            inner_cone_angle: Some(20.0),
            outer_cone_angle: Some(120.0),
            enabled: true,
            cast_shadows: true,
        }
    }

    #[test]
    fn point_light_scales_with_brightness_and_radius() {
        let m = LightMapping::default();
        let s = WorldScale::PRESENTATION_METRES;
        let r = map_light(&light(UeLightKind::Point), &m, s).unwrap();
        let RenderLightKind::Point { lumens, range } = r.kind else {
            panic!("{r:?}");
        };
        assert!((range - 10.0).abs() < 1e-5, "500 uu = 10 m");
        assert!((lumens - 2.0 * 2500.0 * 100.0).abs() < 1e-2);
        assert_eq!(r.color_srgb, [255, 128, 0]);
    }

    #[test]
    fn spot_angles_are_clamped() {
        let r = map_light(
            &light(UeLightKind::Spot),
            &LightMapping::default(),
            WorldScale::IDENTITY,
        )
        .unwrap();
        let RenderLightKind::Spot { inner, outer, .. } = r.kind else {
            panic!("{r:?}");
        };
        assert!((outer - 89.9_f32.to_radians()).abs() < 1e-6);
        assert!((inner - 20.0_f32.to_radians()).abs() < 1e-6);
        let mut l = light(UeLightKind::Spot);
        l.inner_cone_angle = Some(80.0);
        l.outer_cone_angle = Some(30.0);
        let r = map_light(&l, &LightMapping::default(), WorldScale::IDENTITY).unwrap();
        let RenderLightKind::Spot { inner, outer, .. } = r.kind else {
            panic!("{r:?}");
        };
        assert!(inner <= outer);
    }

    #[test]
    fn lights_without_effect_are_dropped() {
        let m = LightMapping::default();
        let s = WorldScale::IDENTITY;
        let mut l = light(UeLightKind::Point);
        l.enabled = false;
        assert!(map_light(&l, &m, s).is_none());
        let mut l = light(UeLightKind::Point);
        l.brightness = 0.0;
        assert!(map_light(&l, &m, s).is_none());
        let mut l = light(UeLightKind::Point);
        l.brightness = f32::NAN;
        assert!(map_light(&l, &m, s).is_none());
        let mut l = light(UeLightKind::Point);
        l.radius = None;
        assert!(map_light(&l, &m, s).is_none());
        let mut l = light(UeLightKind::Point);
        l.location_ue = Vec3::splat(f32::INFINITY);
        assert!(map_light(&l, &m, s).is_none());
        assert!(map_light(&light(UeLightKind::Other), &m, s).is_none());
    }

    #[test]
    fn overflowing_intensities_are_dropped() {
        let s = WorldScale::IDENTITY;
        for kind in [
            UeLightKind::Point,
            UeLightKind::Spot,
            UeLightKind::Directional,
            UeLightKind::Sky,
        ] {
            let mut l = light(kind);
            l.brightness = f32::MAX;
            l.radius = Some(f32::MAX);
            assert!(
                map_light(&l, &LightMapping::default(), s).is_none(),
                "{kind:?}"
            );
            // A finite light with an overflowing mapping constant too.
            let huge = LightMapping {
                point_lumens_per_brightness_m2: f32::MAX,
                directional_lux_per_brightness: f32::MAX,
                sky_ambient_per_brightness: f32::MAX,
                base_ambient: 0.0,
            };
            assert!(map_light(&light(kind), &huge, s).is_none(), "{kind:?}");
            // Non-finite cone angles fall back to the defaults.
            let mut l = light(kind);
            l.inner_cone_angle = Some(f32::NAN);
            l.outer_cone_angle = Some(f32::INFINITY);
            assert!(
                map_light(&l, &LightMapping::default(), s).is_some(),
                "{kind:?}"
            );
        }
    }

    #[test]
    fn directional_and_sky() {
        let m = LightMapping::default();
        let d = map_light(&light(UeLightKind::Directional), &m, WorldScale::IDENTITY).unwrap();
        assert_eq!(d.kind, RenderLightKind::Directional { lux: 20_000.0 });
        let a = map_light(&light(UeLightKind::Sky), &m, WorldScale::IDENTITY).unwrap();
        assert_eq!(a.kind, RenderLightKind::Ambient { brightness: 600.0 });
    }
}
