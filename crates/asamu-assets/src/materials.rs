//! Render materials for converted levels.
//!
//! The renderer needs, per UE3 material path, an approximate PBR description:
//! base colour (texture and/or factor), optional normal and emissive maps,
//! blend mode, two-sidedness and an unlit flag. That description comes from
//! `materials/materials.json`, written by `asamu-import materials` (see
//! [`crate::material_manifest`]). When the file is absent,
//! or a material is missing from it, the renderer falls back to
//! [`fallback_material`]: an untextured, neutral colour per material, plus a
//! few **name heuristics** (render-only, not RE claims) so translucent
//! effect meshes do not render as opaque blocks.

use crate::manifest::{AddressMode, TextureManifest};

/// How a material blends with what is behind it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum BlendMode {
    /// Opaque.
    Opaque,
    /// Alpha-tested with a cutoff (UE3 `BLEND_Masked`, `OpacityMaskClipValue`).
    Masked {
        /// Alpha below this is discarded.
        cutoff: f32,
    },
    /// Alpha-blended (UE3 `BLEND_Translucent`).
    Translucent,
    /// Additive (UE3 `BLEND_Additive`).
    Additive,
    /// Multiplicative (UE3 `BLEND_Modulate`).
    Modulate,
}

/// A texture bound to a material input, resolved to a converted file.
#[derive(Debug, Clone, PartialEq)]
pub struct TextureBinding {
    /// Texture object path.
    pub texture: String,
    /// DDS file relative to `textures/` (validated).
    pub file: String,
    /// Sample as sRGB colour.
    pub srgb: bool,
    /// Address modes (U, V).
    pub address: [AddressMode; 2],
    /// Texture size in texels (`SizeX`, `SizeY`; 0 when unknown).
    pub size: [u32; 2],
}

/// Texture coordinate transform applied to every texture of a material.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct UvTransform {
    /// UV channel (`TEXCOORD_n`).
    pub channel: u32,
    /// Tiling.
    pub scale: [f32; 2],
    /// Offset (after tiling).
    pub offset: [f32; 2],
}

impl Default for UvTransform {
    fn default() -> Self {
        Self {
            channel: 0,
            scale: [1.0, 1.0],
            offset: [0.0, 0.0],
        }
    }
}

/// Where a [`RenderMaterial`] came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MaterialSource {
    /// `materials/materials.json`.
    Converted,
    /// [`fallback_material`] (no converted description).
    Fallback,
}

/// An approximate PBR material for the renderer.
#[derive(Debug, Clone, PartialEq)]
pub struct RenderMaterial {
    /// UE3 material path (`None` for an unassigned slot).
    pub path: Option<String>,
    /// Origin of this description.
    pub source: MaterialSource,
    /// Linear RGBA multiplier of the base colour (alpha = opacity).
    pub base_color: [f32; 4],
    /// Base colour texture.
    pub base_color_texture: Option<TextureBinding>,
    /// Tangent-space normal map.
    pub normal_texture: Option<TextureBinding>,
    /// Linear emissive colour (multiplies the emissive texture when present).
    pub emissive: [f32; 3],
    /// Emissive texture.
    pub emissive_texture: Option<TextureBinding>,
    /// Perceptual roughness 0..1.
    pub roughness: f32,
    /// Metallic 0..1.
    pub metallic: f32,
    /// Blend mode.
    pub blend: BlendMode,
    /// Render both faces.
    pub two_sided: bool,
    /// Ignore lighting.
    pub unlit: bool,
    /// UV transform.
    pub uv: UvTransform,
    /// Texture paths the description names that were not converted.
    pub missing_textures: Vec<String>,
}

/// FNV-1a 64 (palette selection only).
fn fnv1a(s: &str) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in s.bytes() {
        h ^= u64::from(b.to_ascii_lowercase());
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// sRGB → linear for one channel.
#[must_use]
pub fn srgb_to_linear(c: f32) -> f32 {
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

/// A muted colour derived from the material path, so different materials
/// stay distinguishable without textures. Linear RGB.
fn palette(path: &str) -> [f32; 3] {
    let h = fnv1a(path);
    // Hue 0..1, low saturation, mid value (sRGB space), then linearised.
    let hue = (h % 360) as f32 / 360.0;
    let sat = 0.12 + ((h >> 16) % 100) as f32 / 1000.0;
    let val = 0.55 + ((h >> 32) % 200) as f32 / 1000.0;
    let i = (hue * 6.0).floor();
    let f = hue * 6.0 - i;
    let p = val * (1.0 - sat);
    let q = val * (1.0 - f * sat);
    let t = val * (1.0 - (1.0 - f) * sat);
    let (r, g, b) = match i as u32 % 6 {
        0 => (val, t, p),
        1 => (q, val, p),
        2 => (p, val, t),
        3 => (p, q, val),
        4 => (t, p, val),
        _ => (val, p, q),
    };
    [srgb_to_linear(r), srgb_to_linear(g), srgb_to_linear(b)]
}

/// The fallback description of a material without a converted entry.
///
/// Untextured, opaque, rough, in a neutral colour derived from the path.
/// Name heuristics (render-only approximations, applied to the lower-case
/// path; **not** reverse-engineering claims):
///
/// | Path contains | Result |
/// |---|---|
/// | `volumetric`, `lightbeam`, `light_beam`, `godray`, `lightshaft` | additive, unlit, faint warm colour |
/// | `glass` | translucent, 25 % opacity, two-sided |
/// | `water` | translucent, 60 % opacity |
/// | `sky` | unlit |
#[must_use]
pub fn fallback_material(path: Option<&str>) -> RenderMaterial {
    let mut m = RenderMaterial {
        path: path.map(str::to_owned),
        source: MaterialSource::Fallback,
        base_color: [0.6, 0.6, 0.6, 1.0],
        base_color_texture: None,
        normal_texture: None,
        emissive: [0.0; 3],
        emissive_texture: None,
        roughness: 0.9,
        metallic: 0.0,
        blend: BlendMode::Opaque,
        two_sided: false,
        unlit: false,
        uv: UvTransform::default(),
        missing_textures: Vec::new(),
    };
    let Some(path) = path else {
        return m;
    };
    let [r, g, b] = palette(path);
    m.base_color = [r, g, b, 1.0];
    let lower = path.to_ascii_lowercase();
    let has = |keys: &[&str]| keys.iter().any(|k| lower.contains(k));
    if has(&[
        "volumetric",
        "lightbeam",
        "light_beam",
        "godray",
        "lightshaft",
    ]) {
        m.blend = BlendMode::Additive;
        m.unlit = true;
        m.two_sided = true;
        m.base_color = [0.08, 0.07, 0.05, 1.0];
    } else if has(&["glass"]) {
        m.blend = BlendMode::Translucent;
        m.two_sided = true;
        m.base_color = [0.7, 0.75, 0.8, 0.25];
        m.roughness = 0.1;
    } else if has(&["water"]) {
        m.blend = BlendMode::Translucent;
        m.base_color = [0.2, 0.3, 0.35, 0.6];
        m.roughness = 0.1;
    } else if has(&["sky"]) {
        m.unlit = true;
    }
    m
}

/// Resolves a texture object path through the texture manifest. `None` when
/// the texture was not converted or its manifest path is unsafe.
#[must_use]
pub fn bind_texture(
    textures: Option<&TextureManifest>,
    path: &str,
    force_linear: bool,
) -> Option<TextureBinding> {
    let (key, entry) = textures?.get(path)?;
    if entry.cube {
        return None;
    }
    let file = entry.safe_file().ok()?;
    Some(TextureBinding {
        texture: key.to_owned(),
        file,
        srgb: !force_linear && entry.is_srgb(),
        address: entry.address_modes(),
        size: entry.size,
    })
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::manifest::TextureEntry;

    #[test]
    fn fallback_is_deterministic_and_distinguishes_materials() {
        let a = fallback_material(Some("Pkg.M_Wood"));
        let b = fallback_material(Some("Pkg.M_Stone"));
        assert_eq!(
            a,
            fallback_material(Some("pkg.m_wood")).with_path("Pkg.M_Wood")
        );
        assert_ne!(a.base_color, b.base_color);
        assert_eq!(a.blend, BlendMode::Opaque);
        assert_eq!(a.source, MaterialSource::Fallback);
        for c in &a.base_color[..3] {
            assert!((0.0..=1.0).contains(c));
        }
        let none = fallback_material(None);
        assert!(none.path.is_none());
    }

    impl RenderMaterial {
        fn with_path(mut self, p: &str) -> Self {
            self.path = Some(p.to_owned());
            self
        }
    }

    #[test]
    fn fallback_heuristics() {
        let beam = fallback_material(Some(
            "EngineVolumetrics.LightBeam.Materials.M_EV_LightBeam_Simple_01",
        ));
        assert_eq!(beam.blend, BlendMode::Additive);
        assert!(beam.unlit);
        let glass = fallback_material(Some("Props.M_WindowGlass"));
        assert_eq!(glass.blend, BlendMode::Translucent);
        assert!(fallback_material(Some("Env.M_SkyDome")).unlit);
    }

    #[test]
    fn texture_binding_goes_through_the_manifest() {
        let mut t = BTreeMap::new();
        let entry = |file: &str, cube: bool| TextureEntry {
            package: "Map".to_owned(),
            class: "Texture2D".to_owned(),
            file: file.to_owned(),
            format: "PF_DXT1".to_owned(),
            size: [4, 4],
            mips: 1,
            cube,
            srgb: Some(true),
            address: Some(["TA_Clamp".to_owned(), "TA_Wrap".to_owned()]),
            lod_group: None,
            compression_settings: None,
        };
        t.insert("Pkg.T_D".to_owned(), entry("Map/Pkg/T_D.dds", false));
        t.insert("Pkg.T_Cube".to_owned(), entry("Map/Pkg/T_Cube.dds", true));
        t.insert("Pkg.T_Bad".to_owned(), entry("../T_Bad.dds", false));
        let m = TextureManifest::from_entries(1, t);
        let b = bind_texture(Some(&m), "pkg.t_d", false).unwrap();
        assert_eq!(b.texture, "Pkg.T_D");
        assert_eq!(b.file, "Map/Pkg/T_D.dds");
        assert!(b.srgb);
        assert_eq!(b.address, [AddressMode::Clamp, AddressMode::Wrap]);
        assert!(!bind_texture(Some(&m), "Pkg.T_D", true).unwrap().srgb);
        assert!(bind_texture(Some(&m), "Pkg.T_Cube", false).is_none());
        assert!(bind_texture(Some(&m), "Pkg.T_Bad", false).is_none());
        assert!(bind_texture(Some(&m), "Pkg.Missing", false).is_none());
        assert!(bind_texture(None, "Pkg.T_D", false).is_none());
    }

    #[test]
    fn srgb_conversion_endpoints() {
        assert_eq!(srgb_to_linear(0.0), 0.0);
        assert!((srgb_to_linear(1.0) - 1.0).abs() < 1e-6);
        assert!((srgb_to_linear(0.5) - 0.214).abs() < 1e-3);
    }
}
