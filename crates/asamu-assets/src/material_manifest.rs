//! Runtime view of `materials/materials.json` (format `asamu-materials`,
//! version 1), written by `asamu-import materials` from
//! `asamu_ue3::material::ApproxMaterial` (schema in
//! `docs/reverse-engineering/MATERIALS.md`).
//!
//! Each entry is an approximate PBR reduction of a UE3 material graph:
//! channels (`base_color`, `normal`, `emissive`, `opacity`, ...) carry a
//! linear RGBA `value`, a `bias` and an optional bound texture (object path,
//! channels read, UV transform), where the channel equals `texture.channels ·
//! value + bias` when a texture is bound and `value` otherwise. This reader
//! maps that onto [`RenderMaterial`]:
//!
//! | Entry | Render material |
//! |---|---|
//! | `status: fallback` | [`fallback_material`] (the importer could not resolve the graph) |
//! | `base_color` texture × `value` + `bias` | base colour texture and multiplier; the `bias` is folded into the multiplier through the texture's mean (`texture_stats`), so the average colour is kept; without the mean it is dropped |
//! | displayed texture in `runtime_textures` | [`SCREEN_OFF`]: a render target or movie has no picture until the game draws one (PLACEHOLDER) |
//! | displayed texture not converted otherwise | the fallback palette colour × multiplier |
//! | `opacity` constant | base colour alpha |
//! | `opacity` = the displayed texture's alpha | its alpha × value |
//! | `opacity` from another texture or channel, or from `gradients` | a [`ComposedTexture`]: the renderer builds one picture with the colour in RGB and the opacity in alpha (a cut-out whose shape is in a separate mask, a beam that fades along a falloff texture, a fade along a mesh's own coordinates) |
//! | `opacity` texture with another tiling than the displayed one, or not converted | [`UNREPRESENTED_OPACITY`] for translucent modes (PLACEHOLDER), none for masked |
//! | `opacity` unresolved (translucent modes) | [`UNREPRESENTED_OPACITY`] (PLACEHOLDER) |
//! | `emissive` texture × `value` | emissive texture and colour (linear, may exceed 1) |
//! | `unlit` | unlit; the **emissive** channel becomes the displayed colour (UE3 unlit materials show `EmissiveColor`) |
//! | `alpha_mode` | `opaque` / `mask` (+ `alpha_cutoff`) / `blend` / `add` / `modulate`; `premultiplied` → blend |
//! | `normal` texture | normal map (used only when the app enables it) |
//! | `roughness`, `metallic`, `two_sided` | as is (clamped) |
//! | displayed texture's `uv` | UV channel, tiling and offset (panning and rotation are not animated) |
//!
//! Unknown fields are ignored; an entry that does not match the shape is
//! counted in [`MaterialManifest::unreadable`] and falls back. The optional
//! fields (`dropped`, `gradients`, `texture_stats`, `runtime_textures`) are
//! absent from files written before they existed, which read as before.

use std::collections::{BTreeMap, HashMap};
use std::path::Path;

use serde::Deserialize;

use crate::error::{AssetError, AssetResult};
use crate::files::parse_json;
use crate::manifest::TextureManifest;
use crate::materials::{
    BlendMode, ComposedAlpha, ComposedRgb, ComposedTexture, GREY_CHROMA, Gradient, GradientFactor,
    MAX_GRADIENT_FACTORS, MaterialSource, RenderMaterial, TextureBinding, UvTransform,
    bind_texture, fallback_material,
};

/// `format` of `materials.json`.
pub const MATERIALS_FORMAT: &str = "asamu-materials";
/// `materials.json` versions this reader understands.
pub const MATERIALS_VERSION: u32 = 1;
/// Alpha used for a translucent material whose opacity the renderer cannot
/// take from the data: the reduction could not resolve it (it is built from
/// scene depth, the view angle or time alone), or it comes from a texture
/// that cannot be composed with the displayed one. A render-only
/// PLACEHOLDER so such meshes stay faint instead of solid; not a recovered
/// value.
pub const UNREPRESENTED_OPACITY: f32 = 0.1;
/// Base colour (linear) of a surface whose displayed texture is filled
/// while the game runs (a render target, a movie): a switched-off screen.
/// A render-only PLACEHOLDER; the original draws a picture into it.
pub const SCREEN_OFF: [f32; 3] = [0.012, 0.012, 0.014];
/// Smallest texture mean (linear, 0..1) a channel's `bias` is divided by
/// when it is folded into the multiplier (a nearly black texture cannot
/// carry an added colour as a factor).
pub const MIN_FOLD_MEAN: f32 = 0.02;
/// Largest multiplier the fold may produce per unit of the original one
/// (ours, against a near-black texture turning an added colour into a
/// blown-out factor).
pub const MAX_FOLD_GAIN: f32 = 8.0;

#[derive(Debug, Clone, Deserialize, PartialEq)]
struct RawUv {
    #[serde(default)]
    channel: i32,
    #[serde(default = "one2")]
    scale: [f32; 2],
    #[serde(default)]
    offset: [f32; 2],
    #[serde(default)]
    rotation_angle: f32,
}

impl Default for RawUv {
    fn default() -> Self {
        Self {
            channel: 0,
            scale: one2(),
            offset: [0.0; 2],
            rotation_angle: 0.0,
        }
    }
}

fn one2() -> [f32; 2] {
    [1.0, 1.0]
}

fn yes() -> bool {
    true
}

#[derive(Debug, Clone, Deserialize)]
struct RawTexture {
    #[serde(default)]
    texture: Option<String>,
    #[serde(default)]
    channels: Option<String>,
    #[serde(default)]
    uv: Option<RawUv>,
}

#[derive(Debug, Clone, Deserialize)]
struct RawChannel {
    #[serde(default)]
    value: [f32; 4],
    #[serde(default)]
    bias: [f32; 4],
    #[serde(default)]
    texture: Option<RawTexture>,
    #[serde(default = "yes")]
    resolved: bool,
}

#[derive(Debug, Clone, Deserialize)]
struct RawGradientFactor {
    #[serde(default)]
    uv_channel: i32,
    #[serde(default)]
    component: String,
    #[serde(default)]
    scale: f32,
    #[serde(default)]
    offset: f32,
    #[serde(default)]
    power: f32,
}

#[derive(Debug, Clone, Deserialize)]
struct RawGradient {
    #[serde(default)]
    factors: Vec<RawGradientFactor>,
}

#[derive(Debug, Clone, Deserialize)]
struct RawStats {
    #[serde(default)]
    mean: [f32; 4],
    #[serde(default)]
    chroma: Option<f32>,
}

/// One entry of `materials.json` (the fields the renderer uses).
#[derive(Debug, Clone, Deserialize)]
pub struct MaterialEntry {
    #[serde(default)]
    status: String,
    #[serde(default)]
    alpha_mode: String,
    #[serde(default)]
    alpha_cutoff: Option<f32>,
    #[serde(default)]
    two_sided: bool,
    #[serde(default)]
    unlit: bool,
    base_color: RawChannel,
    #[serde(default)]
    normal: Option<RawChannel>,
    #[serde(default)]
    emissive: Option<RawChannel>,
    #[serde(default)]
    opacity: Option<RawChannel>,
    #[serde(default)]
    roughness: Option<f32>,
    #[serde(default)]
    metallic: Option<f32>,
    #[serde(default)]
    gradients: BTreeMap<String, RawGradient>,
    #[serde(default)]
    texture_stats: BTreeMap<String, RawStats>,
    #[serde(default)]
    runtime_textures: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct RawFile {
    format: String,
    version: u32,
    #[serde(default)]
    materials: BTreeMap<String, serde_json::Value>,
}

/// `materials/materials.json`.
#[derive(Debug, Clone, Default)]
pub struct MaterialManifest {
    /// Format version.
    pub version: u32,
    entries: BTreeMap<String, MaterialEntry>,
    folded: HashMap<String, String>,
    /// Entries that did not match the expected shape.
    pub unreadable: usize,
}

fn clamp01(v: f32) -> f32 {
    if v.is_finite() {
        v.clamp(0.0, 1.0)
    } else {
        0.0
    }
}

fn finite_or(v: f32, d: f32) -> f32 {
    if v.is_finite() { v } else { d }
}

fn texture_path(c: &RawChannel) -> Option<&str> {
    c.texture.as_ref()?.texture.as_deref()
}

/// Index of the texture channel that feeds component `i` of a channel
/// reading `channels` (`rgb`, `a`, ...): a single channel feeds every
/// component.
fn channel_index(channels: Option<&str>, i: usize) -> usize {
    let text = channels.unwrap_or("rgba");
    let pick = if text.len() == 1 { 0 } else { i };
    match text.as_bytes().get(pick) {
        Some(b'r') => 0,
        Some(b'g') => 1,
        Some(b'b') => 2,
        Some(b'a') => 3,
        _ => i.min(3),
    }
}

fn uv_of(t: &RawTexture) -> RawUv {
    t.uv.clone().unwrap_or_default()
}

fn uv_transform(uv: &RawUv) -> UvTransform {
    UvTransform {
        channel: u32::try_from(uv.channel).unwrap_or(0).min(7),
        scale: uv.scale.map(|v| finite_or(v, 1.0)),
        offset: uv.offset.map(|v| finite_or(v, 0.0)),
    }
}

impl MaterialEntry {
    fn stats(&self, texture: &str) -> Option<&RawStats> {
        self.texture_stats
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(texture))
            .map(|(_, s)| s)
    }

    /// The mean of a bound texture, when the file has it.
    fn mean(&self, texture: &str) -> Option<[f32; 4]> {
        self.stats(texture).map(|s| s.mean)
    }

    /// The single texture channel a colour channel reads, when showing the
    /// texture's own colour in its place would be wrong: the alpha channel
    /// always, a colour channel when the texture is known to carry colour
    /// (a grey texture's channels are alike; without statistics the
    /// texture is shown as before).
    fn lone_channel(&self, t: &RawTexture) -> Option<usize> {
        let channels = t.channels.as_deref()?;
        if channels.len() != 1 {
            return None;
        }
        let index = channel_index(Some(channels), 0);
        let coloured = t
            .texture
            .as_deref()
            .and_then(|p| self.stats(p))
            .and_then(|s| s.chroma)
            .is_some_and(|c| c.is_finite() && c >= GREY_CHROMA);
        (index == 3 || coloured).then_some(index)
    }

    /// The colour multiplier of a channel: its `value`, plus its `bias`
    /// folded in through the bound texture's mean (`texture * value + bias`
    /// and `texture * (value + bias / mean)` agree on average). Without a
    /// texture, or without its mean, or under a nearly black texture, the
    /// bias is left out as before.
    fn multiplier(&self, c: &RawChannel) -> [f32; 4] {
        let clean = |v: f32| finite_or(v, 0.0).max(0.0);
        let mut out = c.value.map(clean);
        let Some(t) = c.texture.as_ref() else {
            return out;
        };
        let Some(mean) = t.texture.as_deref().and_then(|p| self.mean(p)) else {
            return out;
        };
        for (i, slot) in out.iter_mut().enumerate().take(3) {
            let m = finite_or(mean[channel_index(t.channels.as_deref(), i)], 0.0);
            let b = finite_or(c.bias[i], 0.0);
            if b != 0.0 && m >= MIN_FOLD_MEAN {
                let v = finite_or(c.value[i], 0.0);
                let cap = v.abs().max(1.0) * MAX_FOLD_GAIN;
                *slot = (v + b / m).clamp(0.0, cap);
            }
        }
        out
    }

    fn is_runtime(&self, texture: &str) -> bool {
        self.runtime_textures
            .iter()
            .any(|t| t.eq_ignore_ascii_case(texture))
    }

    /// The gradient of channel `key`, with the UV channel it reads. `None`
    /// when the file has none, or one this renderer cannot draw (factors of
    /// several UV channels, too many factors, values that are not finite).
    fn gradient(&self, key: &str) -> Option<(Gradient, i32)> {
        let raw = self.gradients.get(key)?;
        let first = raw.factors.first()?;
        if raw.factors.len() > MAX_GRADIENT_FACTORS {
            return None;
        }
        let mut factors = Vec::with_capacity(raw.factors.len());
        for f in &raw.factors {
            let v = match f.component.as_str() {
                "u" => false,
                "v" => true,
                _ => return None,
            };
            if f.uv_channel != first.uv_channel
                || !(f.scale.is_finite() && f.offset.is_finite() && f.power.is_finite())
                || f.power < 0.0
            {
                return None;
            }
            factors.push(GradientFactor {
                v,
                scale: f.scale,
                offset: f.offset,
                power: f.power,
            });
        }
        Some((Gradient { factors }, first.uv_channel))
    }
}

impl MaterialManifest {
    /// Parses `materials.json`.
    ///
    /// # Errors
    /// Malformed JSON or a wrong format/version (individual entries of the
    /// wrong shape are counted in [`Self::unreadable`] instead).
    pub fn from_json(path: &Path, data: &[u8]) -> AssetResult<Self> {
        let raw: RawFile = parse_json(path, data)?;
        if raw.format != MATERIALS_FORMAT || raw.version != MATERIALS_VERSION {
            return Err(AssetError::Format {
                path: path.to_path_buf(),
                expected: format!("{MATERIALS_FORMAT} version {MATERIALS_VERSION}"),
                found: format!("{} version {}", raw.format, raw.version),
            });
        }
        let mut entries = BTreeMap::new();
        let mut unreadable = 0;
        for (k, v) in raw.materials {
            match serde_json::from_value::<MaterialEntry>(v) {
                Ok(e) => {
                    entries.insert(k, e);
                }
                Err(_) => unreadable += 1,
            }
        }
        let folded = entries
            .keys()
            .map(|k| (k.to_ascii_lowercase(), k.clone()))
            .collect();
        Ok(Self {
            version: raw.version,
            entries,
            folded,
            unreadable,
        })
    }

    /// Number of readable entries.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// True when there are no readable entries.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    fn entry(&self, path: &str) -> Option<(&str, &MaterialEntry)> {
        if let Some((k, v)) = self.entries.get_key_value(path) {
            return Some((k.as_str(), v));
        }
        let k = self.folded.get(&path.to_ascii_lowercase())?;
        self.entries.get_key_value(k).map(|(k, v)| (k.as_str(), v))
    }

    /// The render material for `path`; `None` when the file has no entry.
    #[must_use]
    pub fn render_material(
        &self,
        path: &str,
        textures: Option<&TextureManifest>,
    ) -> Option<RenderMaterial> {
        let (key, e) = self.entry(path)?;
        if e.status == "fallback" {
            return Some(fallback_material(Some(key)));
        }
        let mut m = fallback_material(Some(key));
        m.source = MaterialSource::Converted;
        m.two_sided = e.two_sided;
        m.unlit = e.unlit;
        m.roughness = e.roughness.map_or(0.8, clamp01);
        m.metallic = e.metallic.map_or(0.0, clamp01);
        m.blend = match e.alpha_mode.as_str() {
            "mask" => BlendMode::Masked {
                cutoff: e.alpha_cutoff.map_or(1.0 / 3.0, clamp01),
            },
            "blend" | "premultiplied" => BlendMode::Translucent,
            "add" => BlendMode::Additive,
            "modulate" => BlendMode::Modulate,
            _ => BlendMode::Opaque,
        };
        let mut missing = Vec::new();
        let mut bind = |c: &RawChannel, linear: bool| -> Option<TextureBinding> {
            let t = texture_path(c)?;
            let b = bind_texture(textures, t, linear);
            if b.is_none() && !e.is_runtime(t) {
                missing.push(t.to_owned());
            }
            b
        };
        // UE3 unlit materials display their emissive colour.
        let shown_channel = if e.unlit {
            e.emissive.as_ref()
        } else {
            Some(&e.base_color)
        };
        let shown_key = if e.unlit { "emissive" } else { "base_color" };
        let (shown, shown_texture) = match shown_channel {
            Some(c) => (e.multiplier(c), bind(c, false)),
            None => ([0.0, 0.0, 0.0, 1.0], None),
        };
        let shown_path = shown_channel.and_then(texture_path);
        let shown = match shown_path {
            // A screen the game draws into while it runs: switched off.
            Some(p) if shown_texture.is_none() && e.is_runtime(p) => {
                [SCREEN_OFF[0], SCREEN_OFF[1], SCREEN_OFF[2], shown[3]]
            }
            // A displayed texture that was not converted: tint the neutral
            // palette colour by the multiplier instead of showing the bare
            // multiplier (usually white).
            Some(_) if shown_texture.is_none() => {
                let p = fallback_material(Some(key)).base_color;
                [shown[0] * p[0], shown[1] * p[1], shown[2] * p[2], shown[3]]
            }
            _ => shown,
        };
        m.base_color = [shown[0].min(1.0), shown[1].min(1.0), shown[2].min(1.0), 1.0];
        m.base_color_texture = shown_texture;
        m.emissive = [0.0; 3];
        if !e.unlit {
            if let Some(c) = &e.emissive {
                m.emissive_texture = bind(c, false);
                let v = e.multiplier(c);
                m.emissive = [v[0], v[1], v[2]];
                // The glow's shape is one channel of the texture: a picture
                // of that channel, not the texture's colour.
                let channel = c.texture.as_ref().and_then(|t| e.lone_channel(t));
                if let (Some(channel), Some(texture)) = (channel, m.emissive_texture.clone()) {
                    m.emissive_composed = Some(ComposedTexture {
                        rgb: ComposedRgb::Channel { texture, channel },
                        alpha: ComposedAlpha::One,
                        emissive: true,
                    });
                }
            }
            m.normal_texture = e.normal.as_ref().and_then(|c| bind(c, true));
        }
        // UV transform of the displayed texture.
        let shown_uv = shown_channel
            .and_then(|c| c.texture.as_ref())
            .map(uv_of)
            .unwrap_or_default();
        m.uv = uv_transform(&shown_uv);

        // The colour of a composed picture: the displayed texture, the
        // displayed channel's gradient, or white. A lit material whose base
        // colour is black shows its emissive channel alone: that channel's
        // gradient is then the picture's colour (and its emissive texture).
        let shown_gradient = e.gradient(shown_key);
        let black_base = !e.unlit
            && m.base_color_texture.is_none()
            && m.base_color[..3].iter().all(|c| *c <= 1.0e-4);
        let emissive_gradient = if black_base && m.emissive_texture.is_none() {
            e.gradient("emissive")
        } else {
            None
        };
        let translucent = matches!(
            m.blend,
            BlendMode::Translucent | BlendMode::Additive | BlendMode::Modulate
        );
        let mut rgb = ComposedRgb::White;
        let mut picture_uv: Option<UvTransform> = None;
        let mut emissive_picture = false;
        if let Some((g, channel)) = shown_gradient.or_else(|| {
            emissive_picture = emissive_gradient.is_some();
            emissive_gradient
        }) {
            rgb = ComposedRgb::Gradient(g);
            picture_uv = Some(UvTransform {
                channel: u32::try_from(channel).unwrap_or(0).min(7),
                ..UvTransform::default()
            });
        } else if let Some(t) = &m.base_color_texture {
            // An unlit colour read from one channel is that channel in grey.
            let lone = shown_channel
                .and_then(|c| c.texture.as_ref())
                .and_then(|raw| e.lone_channel(raw));
            rgb = match lone {
                Some(channel) => ComposedRgb::Channel {
                    texture: t.clone(),
                    channel,
                },
                None => ComposedRgb::Texture(t.clone()),
            };
            picture_uv = Some(m.uv);
        }

        // Opacity.
        let mut alpha = ComposedAlpha::One;
        if let Some(o) = &e.opacity {
            let value = finite_or(o.value[0], 1.0);
            let bias = finite_or(o.bias[0], 0.0);
            let shown_name = m.base_color_texture.as_ref().map(|b| b.texture.clone());
            let same_as_shown = |t: &RawTexture| {
                matches!(
                    (t.texture.as_deref(), shown_name.as_deref()),
                    (Some(p), Some(s)) if s.eq_ignore_ascii_case(p)
                )
            };
            let opacity_gradient = e.gradient("opacity");
            match (&o.texture, opacity_gradient) {
                (_, Some((g, channel))) => {
                    // A gradient lives in the mesh's plain coordinates: it
                    // composes with a gradient colour or with white, and
                    // with a texture only when that is not tiled or moved.
                    let plain = UvTransform {
                        channel: u32::try_from(channel).unwrap_or(0).min(7),
                        ..UvTransform::default()
                    };
                    if picture_uv.is_some_and(|uv| uv != plain) {
                        rgb = ComposedRgb::White;
                    }
                    picture_uv = Some(plain);
                    alpha = ComposedAlpha::Gradient(g);
                    m.base_color[3] = clamp01(value);
                }
                (None, None) if !o.resolved && translucent => {
                    m.base_color[3] = UNREPRESENTED_OPACITY;
                }
                (None, None) => m.base_color[3] = clamp01(value),
                // The displayed texture's own alpha: the texture carries it
                // (unless a picture of one of its channels replaces it).
                (Some(t), None)
                    if same_as_shown(t)
                        && bias == 0.0
                        && channel_index(t.channels.as_deref(), 0) == 3
                        && !matches!(rgb, ComposedRgb::Channel { .. }) =>
                {
                    m.base_color[3] = clamp01(value);
                }
                (Some(t), None) => {
                    let uv = uv_transform(&uv_of(t));
                    let bound = t
                        .texture
                        .as_deref()
                        .and_then(|p| bind_texture(textures, p, false));
                    let fits = match &rgb {
                        ComposedRgb::White => true,
                        ComposedRgb::Texture(_) | ComposedRgb::Channel { .. } => {
                            picture_uv == Some(uv)
                        }
                        ComposedRgb::Gradient(_) => false,
                    };
                    match bound {
                        Some(texture) if fits => {
                            // A plain factor stays on the material (full
                            // precision); anything else goes into the
                            // picture.
                            let (scale, bias, factor) =
                                if bias == 0.0 && (0.0..=1.0).contains(&value) {
                                    (1.0, 0.0, value)
                                } else {
                                    (value, bias, 1.0)
                                };
                            alpha = ComposedAlpha::Texture {
                                texture,
                                channel: channel_index(t.channels.as_deref(), 0),
                                scale,
                                bias,
                            };
                            picture_uv = Some(uv);
                            m.base_color[3] = factor;
                        }
                        _ if translucent => m.base_color[3] = UNREPRESENTED_OPACITY,
                        _ => {}
                    }
                }
            }
        }
        if !matches!(alpha, ComposedAlpha::One)
            || matches!(rgb, ComposedRgb::Gradient(_) | ComposedRgb::Channel { .. })
        {
            if let Some(uv) = picture_uv {
                m.uv = uv;
            }
            m.composed = Some(ComposedTexture {
                emissive: emissive_picture && matches!(rgb, ComposedRgb::Gradient(_)),
                rgb,
                alpha,
            });
        }
        m.missing_textures = missing;
        Some(m)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::TextureEntry;

    fn textures() -> TextureManifest {
        let mut t = BTreeMap::new();
        for name in ["Pkg.T_D", "Pkg.T_E"] {
            t.insert(
                name.to_owned(),
                TextureEntry {
                    package: "Map".to_owned(),
                    class: "Texture2D".to_owned(),
                    file: format!("Map/{name}.dds"),
                    format: "PF_DXT1".to_owned(),
                    size: [256, 128],
                    mips: 1,
                    cube: false,
                    srgb: Some(true),
                    address: None,
                    lod_group: None,
                    compression_settings: None,
                },
            );
        }
        TextureManifest::from_entries(1, t)
    }

    /// A hand-written document in the importer's schema (no game data).
    const DOC: &str = r#"{
      "format": "asamu-materials", "version": 1, "notice": "n", "conventions": "c",
      "materials": {
        "Pkg.M_Lit": {
          "package": "Map", "export_index": 3, "class": "MaterialInstanceConstant",
          "chain": ["Pkg.M_Lit", "Pkg.M_Base"], "base_material": "Pkg.M_Base", "status": "approximated",
          "lossless": false, "blend_mode": "BLEND_Masked", "alpha_mode": "mask", "alpha_cutoff": 0.5,
          "two_sided": true, "unlit": false, "lighting_model": "MLM_Phong", "decal": false,
          "base_color": {"source": "expression", "value": [0.5, 0.5, 0.5, 1], "bias": [0, 0, 0, 0],
                         "texture": {"texture": "Pkg.T_D", "sampler": "2d", "channels": "rgb",
                                     "uv": {"channel": 0, "scale": [4, 2], "offset": [0.5, 0],
                                            "panning": [0, 0], "rotation": 0, "rotation_center": [0.5, 0.5]}},
                         "vertex_color": false, "resolved": true},
          "normal": {"source": "expression", "value": [1, 1, 1, 1], "bias": [0, 0, 0, 0],
                     "texture": {"texture": "Pkg.T_Missing", "sampler": "normal", "channels": "rgb",
                                 "uv": {"channel": 0, "scale": [1, 1], "offset": [0, 0], "panning": [0, 0],
                                        "rotation": 0, "rotation_center": [0.5, 0.5]}},
                     "vertex_color": false, "resolved": true},
          "emissive": {"source": "default", "value": [0, 0, 0, 0], "bias": [0, 0, 0, 0], "vertex_color": false, "resolved": true},
          "specular": {"source": "default", "value": [0, 0, 0, 0], "bias": [0, 0, 0, 0], "vertex_color": false, "resolved": true},
          "specular_power": {"source": "default", "value": [15, 15, 15, 15], "bias": [0, 0, 0, 0], "vertex_color": false, "resolved": true},
          "specular_level": 0.0, "roughness": 0.6, "metallic": 0.0,
          "opacity": {"source": "expression", "value": [1, 1, 1, 1], "bias": [0, 0, 0, 0],
                      "texture": {"texture": "Pkg.T_D", "sampler": "2d", "channels": "a",
                                  "uv": {"channel": 0, "scale": [4, 2], "offset": [0.5, 0], "panning": [0, 0],
                                         "rotation": 0, "rotation_center": [0.5, 0.5]}},
                      "vertex_color": false, "resolved": true},
          "parameters": {}, "textures": ["Pkg.T_D"], "expressions": {}, "notes": []
        },
        "Pkg.M_Glow": {
          "status": "approximated", "alpha_mode": "add", "two_sided": false, "unlit": true,
          "base_color": {"value": [0, 0, 0, 1]},
          "emissive": {"value": [2, 1, 0.5, 1], "texture": {"texture": "Pkg.T_E", "channels": "rgb"}},
          "opacity": {"value": [0.25, 0, 0, 0]}
        },
        "Pkg.M_Beam": {
          "status": "approximated", "alpha_mode": "blend", "unlit": true,
          "base_color": {"value": [0, 0, 0, 1]},
          "emissive": {"value": [0.9, 0.5, 0.25, 1]},
          "opacity": {"value": [0.003, 0, 0, 0], "texture": {"texture": "Pkg.T_Falloff", "channels": "rgb"}}
        },
        "Pkg.M_Broken": {"status": "fallback", "alpha_mode": "opaque", "base_color": {"value": [0, 0, 0, 1]}},
        "Pkg.M_Bad": {"status": "approximated", "base_color": "red"}
      },
      "coverage": {}
    }"#;

    #[test]
    fn entries_map_onto_render_materials() {
        let m = MaterialManifest::from_json(Path::new("materials.json"), DOC.as_bytes()).unwrap();
        assert_eq!(m.len(), 4);
        assert_eq!(m.unreadable, 1);
        let t = textures();

        let lit = m.render_material("pkg.m_lit", Some(&t)).unwrap();
        assert_eq!(lit.source, MaterialSource::Converted);
        let tex = lit.base_color_texture.as_ref().unwrap();
        assert_eq!(tex.file, "Map/Pkg.T_D.dds");
        assert_eq!(tex.size, [256, 128]);
        assert_eq!(lit.base_color, [0.5, 0.5, 0.5, 1.0]);
        assert_eq!(lit.blend, BlendMode::Masked { cutoff: 0.5 });
        assert!(lit.two_sided && !lit.unlit);
        assert_eq!(lit.roughness, 0.6);
        assert!(lit.normal_texture.is_none());
        assert_eq!(lit.missing_textures, vec!["Pkg.T_Missing".to_owned()]);
        assert_eq!(lit.uv.scale, [4.0, 2.0]);
        assert_eq!(lit.uv.offset, [0.5, 0.0]);

        let glow = m.render_material("Pkg.M_Glow", Some(&t)).unwrap();
        assert!(glow.unlit);
        assert_eq!(glow.blend, BlendMode::Additive);
        // The emissive channel is what an unlit material shows (clamped).
        assert_eq!(glow.base_color, [1.0, 1.0, 0.5, 0.25]);
        assert_eq!(glow.base_color_texture.as_ref().unwrap().texture, "Pkg.T_E");
        assert_eq!(glow.emissive, [0.0; 3]);

        let beam = m.render_material("Pkg.M_Beam", Some(&t)).unwrap();
        assert_eq!(beam.blend, BlendMode::Translucent);
        assert_eq!(beam.base_color, [0.9, 0.5, 0.25, UNREPRESENTED_OPACITY]);

        let broken = m.render_material("Pkg.M_Broken", Some(&t)).unwrap();
        assert_eq!(broken.source, MaterialSource::Fallback);
        assert!(m.render_material("Pkg.M_Bad", Some(&t)).is_none());
        assert!(m.render_material("Pkg.Nope", Some(&t)).is_none());
    }

    #[test]
    fn hostile_documents() {
        for bad in [
            &b""[..],
            b"null",
            b"{\"format\": \"asamu-materials\", \"version\": 2, \"materials\": {}}",
            b"{\"format\": \"other\", \"version\": 1, \"materials\": {}}",
            b"{\"format\": \"asamu-materials\", \"version\": 1, \"materials\": [1]}",
        ] {
            assert!(MaterialManifest::from_json(Path::new("m"), bad).is_err());
        }
        let ok = MaterialManifest::from_json(
            Path::new("m"),
            br#"{"format": "asamu-materials", "version": 1, "materials": {
                "a": 5, "b": null,
                "c": {"status": "approximated", "alpha_cutoff": 1e39,  "alpha_mode": "mask",
                      "base_color": {"value": [1e39, -1, 0.5, 1],
                                     "texture": {"texture": "T", "uv": {"channel": -4, "scale": [1e39, 2]}}}}}}"#,
        )
        .unwrap();
        assert_eq!(ok.unreadable, 2);
        let c = ok.render_material("c", None).unwrap();
        assert_eq!(c.blend, BlendMode::Masked { cutoff: 0.0 });
        let p = fallback_material(Some("c")).base_color;
        assert_eq!(c.base_color, [0.0, 0.0, 0.5 * p[2], 1.0]);
        assert_eq!(c.missing_textures, vec!["T".to_owned()]);
        assert_eq!(c.uv.channel, 0);
        assert_eq!(c.uv.scale, [1.0, 2.0]);
    }

    /// Entries in the importer's schema with the fields added after the
    /// first play session (hand-written; no game data).
    const PLAY: &str = r#"{
      "format": "asamu-materials", "version": 1,
      "materials": {
        "Pkg.M_Painting": {
          "status": "approximated", "alpha_mode": "mask", "alpha_cutoff": 0.3333,
          "base_color": {"source": "default", "value": [0, 0, 0, 1]},
          "emissive": {"value": [0.02, 8, 10, 1]},
          "opacity": {"value": [1, 1, 1, 1],
                      "texture": {"texture": "Pkg.T_E", "channels": "a",
                                  "uv": {"channel": 0, "scale": [2, 2], "offset": [0, 0]}}}
        },
        "Pkg.M_Cog": {
          "status": "approximated", "alpha_mode": "mask",
          "base_color": {"value": [1, 1, 1, 1], "texture": {"texture": "Pkg.T_D", "channels": "rgb"}},
          "opacity": {"value": [1, 1, 1, 1], "texture": {"texture": "Pkg.T_E", "channels": "rgb"}}
        },
        "Pkg.M_CogTiled": {
          "status": "approximated", "alpha_mode": "mask",
          "base_color": {"value": [1, 1, 1, 1],
                         "texture": {"texture": "Pkg.T_D", "channels": "rgb", "uv": {"scale": [4, 4]}}},
          "opacity": {"value": [1, 1, 1, 1], "texture": {"texture": "Pkg.T_E", "channels": "rgb"}}
        },
        "Pkg.M_SameGreen": {
          "status": "approximated", "alpha_mode": "blend",
          "base_color": {"value": [1, 1, 1, 1], "texture": {"texture": "Pkg.T_D", "channels": "rgb"}},
          "opacity": {"value": [0.5, 0.5, 0.5, 0.5], "texture": {"texture": "pkg.t_d", "channels": "g"}}
        },
        "Pkg.M_Beam": {
          "status": "approximated", "alpha_mode": "blend", "unlit": true,
          "base_color": {"value": [0, 0, 0, 1]},
          "emissive": {"value": [1, 0.25, 0.125, 1]},
          "opacity": {"value": [0.08, 0.08, 0.08, 0.08],
                      "texture": {"texture": "Pkg.T_E", "channels": "g",
                                  "uv": {"channel": 0, "scale": [0, 1], "offset": [0.5, 0]}}},
          "dropped": {"opacity": ["depth", "view", "texture"]}
        },
        "Pkg.M_BeamBias": {
          "status": "approximated", "alpha_mode": "add", "unlit": true,
          "base_color": {"value": [0, 0, 0, 1]},
          "emissive": {"value": [1, 1, 1, 1]},
          "opacity": {"value": [2, 2, 2, 2], "bias": [0.125, 0.125, 0.125, 0.125],
                      "texture": {"texture": "Pkg.T_E", "channels": "rgb"}}
        },
        "Pkg.M_Checkpoint": {
          "status": "approximated", "alpha_mode": "blend",
          "base_color": {"source": "default", "value": [0, 0, 0, 1]},
          "emissive": {"value": [0.125, 0.5, 1, 1]},
          "opacity": {"value": [1, 1, 1, 1]},
          "gradients": {
            "emissive": {"factors": [{"uv_channel": 0, "component": "v", "scale": 1, "offset": 0, "power": 5}]},
            "opacity": {"factors": [{"uv_channel": 0, "component": "v", "scale": 1, "offset": 0, "power": 5}]}
          }
        },
        "Pkg.M_FadeOverTexture": {
          "status": "approximated", "alpha_mode": "blend",
          "base_color": {"value": [1, 1, 1, 1], "texture": {"texture": "Pkg.T_D", "channels": "rgb"}},
          "opacity": {"value": [0.5, 0.5, 0.5, 0.5]},
          "gradients": {"opacity": {"factors": [{"uv_channel": 0, "component": "u", "scale": -1, "offset": 1, "power": 1}]}}
        },
        "Pkg.M_FadeOverTiles": {
          "status": "approximated", "alpha_mode": "blend",
          "base_color": {"value": [1, 1, 1, 1],
                         "texture": {"texture": "Pkg.T_D", "channels": "rgb", "uv": {"scale": [2, 2]}}},
          "opacity": {"value": [1, 1, 1, 1]},
          "gradients": {"opacity": {"factors": [{"uv_channel": 0, "component": "u", "scale": 1, "offset": 0, "power": 1}]}}
        },
        "Pkg.M_Outline": {
          "status": "approximated", "alpha_mode": "blend", "unlit": true,
          "base_color": {"value": [0, 0, 0, 1]},
          "emissive": {"value": [0.25, 1.5, 4, 1]},
          "opacity": {"value": [0, 0, 0, 0], "resolved": true},
          "dropped": {"opacity": ["depth"]}
        },
        "Pkg.M_Unresolved": {
          "status": "approximated", "alpha_mode": "add",
          "base_color": {"value": [0.5, 0.5, 0.5, 1]},
          "opacity": {"value": [1, 1, 1, 1], "resolved": false}
        },
        "Pkg.M_UnresolvedMask": {
          "status": "approximated", "alpha_mode": "mask",
          "base_color": {"value": [0.5, 0.5, 0.5, 1]},
          "opacity": {"value": [1, 1, 1, 1], "resolved": false}
        },
        "Pkg.M_Monitor": {
          "status": "approximated", "alpha_mode": "opaque",
          "base_color": {"value": [1, 1, 1, 1], "texture": {"texture": "Pkg.T_Screen", "channels": "rgb"}},
          "runtime_textures": ["pkg.t_screen"]
        },
        "Pkg.M_Ground": {
          "status": "approximated", "alpha_mode": "opaque",
          "base_color": {"value": [0.25, 0.25, 0.25, 1], "bias": [0.125, 0.125, 0.125, 0],
                         "texture": {"texture": "Pkg.T_D", "channels": "rgb", "uv": {"scale": [13, 13]}}},
          "texture_stats": {"pkg.t_d": {"mean": [0.25, 0.5, 0.0625, 1], "chroma": 0.1, "uniform": false}}
        },
        "Pkg.M_GroundGrey": {
          "status": "approximated", "alpha_mode": "opaque",
          "base_color": {"value": [0.25, 0.25, 0.25, 1], "bias": [0.125, 0.125, 0.125, 0],
                         "texture": {"texture": "Pkg.T_D", "channels": "a"}},
          "texture_stats": {"Pkg.T_D": {"mean": [0.25, 0.5, 0.0625, 0.5]}}
        },
        "Pkg.M_GroundDark": {
          "status": "approximated", "alpha_mode": "opaque",
          "base_color": {"value": [0.25, 0.25, 0.25, 1], "bias": [0.125, 0.125, 0.125, 0],
                         "texture": {"texture": "Pkg.T_D", "channels": "rgb"}},
          "texture_stats": {"Pkg.T_D": {"mean": [0.001, 0.001, 0.001, 1]}}
        },
        "Pkg.M_Lamp": {
          "status": "approximated", "alpha_mode": "opaque",
          "base_color": {"value": [1, 1, 1, 1], "texture": {"texture": "Pkg.T_D", "channels": "rgb"}},
          "emissive": {"value": [11.5, 6.5, 1.25, 1], "texture": {"texture": "Pkg.T_D", "channels": "a"}}
        },
        "Pkg.M_LampRed": {
          "status": "approximated", "alpha_mode": "opaque",
          "base_color": {"value": [1, 1, 1, 1], "texture": {"texture": "Pkg.T_D", "channels": "rgb"}},
          "emissive": {"value": [2, 2, 2, 1], "texture": {"texture": "Pkg.T_E", "channels": "r"}},
          "texture_stats": {"Pkg.T_E": {"mean": [0.5, 0.5, 0.5, 1], "chroma": 0.25}}
        },
        "Pkg.M_LampGrey": {
          "status": "approximated", "alpha_mode": "opaque",
          "base_color": {"value": [1, 1, 1, 1], "texture": {"texture": "Pkg.T_D", "channels": "rgb"}},
          "emissive": {"value": [2, 2, 2, 1], "texture": {"texture": "Pkg.T_E", "channels": "r"}},
          "texture_stats": {"Pkg.T_E": {"mean": [0.5, 0.5, 0.5, 1], "chroma": 0.001}}
        },
        "Pkg.M_GlowGreen": {
          "status": "approximated", "alpha_mode": "add", "unlit": true,
          "base_color": {"value": [0, 0, 0, 1]},
          "emissive": {"value": [0.5, 1, 0.25, 1], "texture": {"texture": "Pkg.T_E", "channels": "g"}},
          "opacity": {"value": [1, 1, 1, 1], "texture": {"texture": "Pkg.T_E", "channels": "a"}},
          "texture_stats": {"Pkg.T_E": {"mean": [0.5, 0.25, 0.125, 1], "chroma": 0.3}}
        },
        "Pkg.M_BadGradient": {
          "status": "approximated", "alpha_mode": "blend",
          "base_color": {"value": [0.5, 0.5, 0.5, 1]},
          "opacity": {"value": [0.75, 0.75, 0.75, 0.75]},
          "gradients": {"opacity": {"factors": [{"uv_channel": 0, "component": "w", "scale": 1, "offset": 0, "power": 1}]}}
        }
      }
    }"#;

    fn play() -> MaterialManifest {
        let m = MaterialManifest::from_json(Path::new("materials.json"), PLAY.as_bytes()).unwrap();
        assert_eq!(m.unreadable, 0);
        m
    }

    #[test]
    fn opacity_from_another_texture_or_channel_is_composed() {
        let (m, t) = (play(), textures());
        // A glowing drawing: black base, constant emissive, the shape in the
        // alpha channel of a texture that is not displayed.
        let painting = m.render_material("Pkg.M_Painting", Some(&t)).unwrap();
        assert_eq!(painting.blend, BlendMode::Masked { cutoff: 0.3333 });
        assert_eq!(painting.base_color, [0.0, 0.0, 0.0, 1.0]);
        assert_eq!(painting.emissive, [0.02, 8.0, 10.0]);
        assert!(painting.base_color_texture.is_none());
        let c = painting.composed.as_ref().unwrap();
        assert_eq!(c.rgb, ComposedRgb::White);
        assert!(!c.emissive);
        match &c.alpha {
            ComposedAlpha::Texture {
                texture,
                channel,
                scale,
                bias,
            } => {
                assert_eq!(texture.texture, "Pkg.T_E");
                assert_eq!((*channel, *scale, *bias), (3, 1.0, 0.0));
            }
            other => panic!("{other:?}"),
        }
        // The picture is placed as the mask is.
        assert_eq!(painting.uv.scale, [2.0, 2.0]);

        // A textured cut-out whose mask is the red channel of another
        // texture with the same coordinates: both go into one picture.
        let cog = m.render_material("Pkg.M_Cog", Some(&t)).unwrap();
        let c = cog.composed.as_ref().unwrap();
        assert!(matches!(&c.rgb, ComposedRgb::Texture(b) if b.texture == "Pkg.T_D"));
        assert!(matches!(
            &c.alpha,
            ComposedAlpha::Texture { texture, channel: 0, .. } if texture.texture == "Pkg.T_E"
        ));
        assert_eq!(cog.base_color_texture.as_ref().unwrap().texture, "Pkg.T_D");
        // Tiled differently, the two cannot share a picture: the texture
        // stays and the cut-out is lost, as before.
        let tiled = m.render_material("Pkg.M_CogTiled", Some(&t)).unwrap();
        assert!(tiled.composed.is_none());
        assert_eq!(tiled.base_color[3], 1.0);
        assert_eq!(tiled.uv.scale, [4.0, 4.0]);
        // Without the mask texture converted: the same.
        let unbound = m.render_material("Pkg.M_Cog", None).unwrap();
        assert!(unbound.composed.is_none());

        // The opacity in another channel of the displayed texture; a plain
        // factor stays on the material.
        let green = m.render_material("Pkg.M_SameGreen", Some(&t)).unwrap();
        let c = green.composed.as_ref().unwrap();
        assert!(matches!(
            &c.alpha,
            ComposedAlpha::Texture { channel: 1, scale, bias, .. } if *scale == 1.0 && *bias == 0.0
        ));
        assert_eq!(green.base_color[3], 0.5);

        // A beam: constant colour, fading along a falloff texture sampled
        // on one line (U fixed at 0.5).
        let beam = m.render_material("Pkg.M_Beam", Some(&t)).unwrap();
        assert!(beam.unlit);
        assert_eq!(beam.base_color, [1.0, 0.25, 0.125, 0.08]);
        assert_eq!(beam.composed.as_ref().unwrap().rgb, ComposedRgb::White);
        assert_eq!(beam.uv.scale, [0.0, 1.0]);
        assert_eq!(beam.uv.offset, [0.5, 0.0]);
        // A factor above 1 or a bias goes into the picture.
        let bias = m.render_material("Pkg.M_BeamBias", Some(&t)).unwrap();
        assert_eq!(bias.blend, BlendMode::Additive);
        assert_eq!(bias.base_color[3], 1.0);
        assert!(matches!(
            &bias.composed.as_ref().unwrap().alpha,
            ComposedAlpha::Texture { channel: 0, scale, bias, .. } if *scale == 2.0 && *bias == 0.125
        ));
    }

    #[test]
    fn gradients_become_pictures() {
        let (m, t) = (play(), textures());
        // Colour and opacity both fade along v; the base colour is black, so
        // the picture is the emissive texture too.
        let cp = m.render_material("Pkg.M_Checkpoint", Some(&t)).unwrap();
        assert_eq!(cp.blend, BlendMode::Translucent);
        assert_eq!(cp.base_color, [0.0, 0.0, 0.0, 1.0]);
        assert_eq!(cp.emissive, [0.125, 0.5, 1.0]);
        let c = cp.composed.as_ref().unwrap();
        assert!(c.emissive);
        let (ComposedRgb::Gradient(rgb), ComposedAlpha::Gradient(alpha)) = (&c.rgb, &c.alpha)
        else {
            panic!("{c:?}");
        };
        assert_eq!(rgb, alpha);
        assert_eq!(rgb.factors.len(), 1);
        assert!(rgb.factors[0].v);
        assert_eq!(rgb.factors[0].power, 5.0);
        assert_eq!(cp.uv, UvTransform::default());

        // A fade over a texture that is not tiled keeps the texture ...
        let over = m
            .render_material("Pkg.M_FadeOverTexture", Some(&t))
            .unwrap();
        let c = over.composed.as_ref().unwrap();
        assert!(matches!(&c.rgb, ComposedRgb::Texture(_)));
        assert!(matches!(&c.alpha, ComposedAlpha::Gradient(g) if !g.factors[0].v));
        assert_eq!(over.base_color[3], 0.5);
        assert!(!c.emissive);
        // ... over a tiled one the fade wins (it is in the mesh's plain
        // coordinates) and the colour is the material's constant.
        let tiles = m.render_material("Pkg.M_FadeOverTiles", Some(&t)).unwrap();
        let c = tiles.composed.as_ref().unwrap();
        assert_eq!(c.rgb, ComposedRgb::White);
        assert_eq!(tiles.uv, UvTransform::default());

        // A gradient this reader cannot draw is left out.
        let bad = m.render_material("Pkg.M_BadGradient", Some(&t)).unwrap();
        assert!(bad.composed.is_none());
        assert_eq!(bad.base_color[3], 0.75);
        let doc = |factor: &str| {
            format!(
                r#"{{"format": "asamu-materials", "version": 1, "materials": {{"g": {{
                    "status": "approximated", "alpha_mode": "blend",
                    "base_color": {{"value": [1, 1, 1, 1]}}, "opacity": {{"value": [1, 1, 1, 1]}},
                    "gradients": {{"opacity": {{"factors": [{factor}]}}}}}}}}}}"#
            )
        };
        let plain = r#"{"uv_channel": 0, "component": "u", "scale": 1, "offset": 0, "power": 1}"#;
        for (factors, drawn) in [
            (plain.to_owned(), true),
            ([plain; MAX_GRADIENT_FACTORS].join(","), true),
            ([plain; MAX_GRADIENT_FACTORS + 1].join(","), false),
            (String::new(), false),
            (plain.replace("\"scale\": 1", "\"scale\": 1e39"), false),
            (plain.replace("\"power\": 1", "\"power\": -2"), false),
            (
                format!(
                    "{plain},{}",
                    plain.replace("\"uv_channel\": 0", "\"uv_channel\": 1")
                ),
                false,
            ),
        ] {
            let m = MaterialManifest::from_json(Path::new("m"), doc(&factors).as_bytes()).unwrap();
            let r = m.render_material("g", None).unwrap();
            assert_eq!(r.composed.is_some(), drawn, "{factors}");
        }
    }

    #[test]
    fn opacity_that_was_not_recovered_stays_faint_or_absent() {
        let (m, t) = (play(), textures());
        // An outline built from scene depth alone reduces to opacity 0: the
        // shell over an object is not drawn.
        let outline = m.render_material("Pkg.M_Outline", Some(&t)).unwrap();
        assert_eq!(outline.base_color, [0.25, 1.0, 1.0, 0.0]);
        assert!(outline.composed.is_none());
        // Unresolved: the placeholder for translucent modes, nothing for a mask.
        let add = m.render_material("Pkg.M_Unresolved", Some(&t)).unwrap();
        assert_eq!(add.base_color[3], UNREPRESENTED_OPACITY);
        let mask = m.render_material("Pkg.M_UnresolvedMask", Some(&t)).unwrap();
        assert_eq!(mask.base_color[3], 1.0);
    }

    #[test]
    fn a_screen_filled_at_run_time_is_switched_off() {
        let (m, t) = (play(), textures());
        let screen = m.render_material("Pkg.M_Monitor", Some(&t)).unwrap();
        assert_eq!(
            screen.base_color,
            [SCREEN_OFF[0], SCREEN_OFF[1], SCREEN_OFF[2], 1.0]
        );
        assert!(screen.base_color_texture.is_none());
        assert!(screen.missing_textures.is_empty());
        // A texture that is simply not converted still shows the palette.
        let doc = PLAY.replace(r#""runtime_textures": ["pkg.t_screen"]"#, r#""notes": []"#);
        let m = MaterialManifest::from_json(Path::new("m"), doc.as_bytes()).unwrap();
        let plain = m.render_material("Pkg.M_Monitor", Some(&t)).unwrap();
        let p = fallback_material(Some("Pkg.M_Monitor")).base_color;
        assert_eq!(plain.base_color, p);
        assert_eq!(plain.missing_textures, vec!["Pkg.T_Screen".to_owned()]);
    }

    #[test]
    fn a_colour_read_from_one_channel_is_a_picture_of_that_channel() {
        let (m, t) = (play(), textures());
        // A lamp whose glow is shaped by the alpha channel of its surface
        // texture: the emissive texture is that channel, not the colours.
        let lamp = m.render_material("Pkg.M_Lamp", Some(&t)).unwrap();
        assert_eq!(lamp.emissive, [11.5, 6.5, 1.25]);
        assert_eq!(lamp.emissive_texture.as_ref().unwrap().texture, "Pkg.T_D");
        let c = lamp.emissive_composed.as_ref().unwrap();
        assert!(c.emissive && c.alpha == ComposedAlpha::One);
        assert!(matches!(
            &c.rgb,
            ComposedRgb::Channel { texture, channel: 3 } if texture.texture == "Pkg.T_D"
        ));
        assert!(lamp.composed.is_none(), "the base colour is untouched");
        // One colour channel of a coloured texture: the same.
        let red = m.render_material("Pkg.M_LampRed", Some(&t)).unwrap();
        assert!(matches!(
            &red.emissive_composed.as_ref().unwrap().rgb,
            ComposedRgb::Channel { channel: 0, .. }
        ));
        // Of a grey texture (or one without statistics): the texture
        // itself says the same, as before.
        let grey = m.render_material("Pkg.M_LampGrey", Some(&t)).unwrap();
        assert!(grey.emissive_composed.is_none());
        let doc = PLAY.replace("texture_stats", "other");
        let plain = MaterialManifest::from_json(Path::new("m"), doc.as_bytes()).unwrap();
        let unknown = plain.render_material("Pkg.M_LampRed", Some(&t)).unwrap();
        assert!(unknown.emissive_composed.is_none());
        assert!(
            plain
                .render_material("Pkg.M_Lamp", Some(&t))
                .unwrap()
                .emissive_composed
                .is_some(),
            "alpha needs no statistics"
        );
        // Without the texture converted there is nothing to compose.
        assert!(
            m.render_material("Pkg.M_Lamp", None)
                .unwrap()
                .emissive_composed
                .is_none()
        );

        // An unlit glow: green channel as the colour, alpha as the opacity,
        // of one texture.
        let glow = m.render_material("Pkg.M_GlowGreen", Some(&t)).unwrap();
        let c = glow.composed.as_ref().unwrap();
        assert!(matches!(&c.rgb, ComposedRgb::Channel { channel: 1, .. }));
        assert!(matches!(
            &c.alpha,
            ComposedAlpha::Texture { channel: 3, .. }
        ));
        assert_eq!(glow.base_color, [0.5, 1.0, 0.25, 1.0]);
        assert!(glow.emissive_composed.is_none());
    }

    #[test]
    fn an_added_colour_is_folded_into_the_multiplier() {
        let (m, t) = (play(), textures());
        // texture * 0.25 + 0.125 with a texture mean of (0.25, 0.5, 0.0625):
        // 0.25 + 0.125 / mean per channel.
        let ground = m.render_material("Pkg.M_Ground", Some(&t)).unwrap();
        assert_eq!(ground.base_color, [0.75, 0.5, 1.0, 1.0], "2.25 is clamped");
        assert_eq!(ground.uv.scale, [13.0, 13.0]);
        // One channel feeds every component.
        let grey = m.render_material("Pkg.M_GroundGrey", Some(&t)).unwrap();
        assert_eq!(grey.base_color, [0.5, 0.5, 0.5, 1.0]);
        // A nearly black texture cannot carry the added colour.
        let dark = m.render_material("Pkg.M_GroundDark", Some(&t)).unwrap();
        assert_eq!(dark.base_color, [0.25, 0.25, 0.25, 1.0]);
        // Without statistics the bias is left out, as before.
        let doc = PLAY.replace("texture_stats", "other");
        let m = MaterialManifest::from_json(Path::new("m"), doc.as_bytes()).unwrap();
        let plain = m.render_material("Pkg.M_Ground", Some(&t)).unwrap();
        assert_eq!(plain.base_color, [0.25, 0.25, 0.25, 1.0]);
    }
}
