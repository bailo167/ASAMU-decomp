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
//! | `base_color` texture × `value` | base colour texture and multiplier (`bias` is dropped); a texture that was not converted → the fallback palette colour × `value` |
//! | `opacity` | constant → base colour alpha; same texture as the displayed one → its alpha × value; another texture → [`UNREPRESENTED_OPACITY`] for translucent modes (placeholder), none for masked |
//! | `emissive` texture × `value` | emissive texture and colour (linear, may exceed 1) |
//! | `unlit` | unlit; the **emissive** channel becomes the displayed colour (UE3 unlit materials show `EmissiveColor`) |
//! | `alpha_mode` | `opaque` / `mask` (+ `alpha_cutoff`) / `blend` / `add` / `modulate`; `premultiplied` → blend |
//! | `normal` texture | normal map (used only when the app enables it) |
//! | `roughness`, `metallic`, `two_sided` | as is (clamped) |
//! | displayed texture's `uv` | UV channel, tiling and offset (panning and rotation are not animated) |
//!
//! Unknown fields are ignored; an entry that does not match the shape is
//! counted in [`MaterialManifest::unreadable`] and falls back.

use std::collections::{BTreeMap, HashMap};
use std::path::Path;

use serde::Deserialize;

use crate::error::{AssetError, AssetResult};
use crate::files::parse_json;
use crate::manifest::TextureManifest;
use crate::materials::{
    BlendMode, MaterialSource, RenderMaterial, TextureBinding, UvTransform, bind_texture,
    fallback_material,
};

/// `format` of `materials.json`.
pub const MATERIALS_FORMAT: &str = "asamu-materials";
/// `materials.json` versions this reader understands.
pub const MATERIALS_VERSION: u32 = 1;
/// Alpha used for a translucent material whose opacity comes from a texture
/// the renderer cannot combine (a different texture than the displayed one,
/// usually with depth or camera fades the approximation drops, as on the
/// light-beam effect meshes). A render-only PLACEHOLDER so such meshes stay
/// faint instead of opaque; not a recovered value.
pub const UNREPRESENTED_OPACITY: f32 = 0.1;

#[derive(Debug, Clone, Deserialize)]
struct RawUv {
    #[serde(default)]
    channel: i32,
    #[serde(default = "one2")]
    scale: [f32; 2],
    #[serde(default)]
    offset: [f32; 2],
}

fn one2() -> [f32; 2] {
    [1.0, 1.0]
}

#[derive(Debug, Clone, Deserialize)]
struct RawTexture {
    #[serde(default)]
    texture: Option<String>,
    #[serde(default)]
    uv: Option<RawUv>,
}

#[derive(Debug, Clone, Deserialize)]
struct RawChannel {
    #[serde(default)]
    value: [f32; 4],
    #[serde(default)]
    texture: Option<RawTexture>,
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
            if b.is_none() {
                missing.push(t.to_owned());
            }
            b
        };
        let color = |v: [f32; 4]| v.map(|c| finite_or(c, 0.0).max(0.0));
        // UE3 unlit materials display their emissive colour.
        let (shown, shown_texture) = if e.unlit {
            match &e.emissive {
                Some(c) => (color(c.value), bind(c, false)),
                None => ([0.0, 0.0, 0.0, 1.0], None),
            }
        } else {
            (color(e.base_color.value), bind(&e.base_color, false))
        };
        // A displayed texture that was not converted: tint the neutral
        // palette colour by the multiplier instead of showing the bare
        // multiplier (usually white).
        let shown_path = if e.unlit {
            e.emissive.as_ref().and_then(texture_path)
        } else {
            texture_path(&e.base_color)
        };
        let shown = if shown_texture.is_none() && shown_path.is_some() {
            let p = fallback_material(Some(key)).base_color;
            [shown[0] * p[0], shown[1] * p[1], shown[2] * p[2], shown[3]]
        } else {
            shown
        };
        m.base_color = [shown[0].min(1.0), shown[1].min(1.0), shown[2].min(1.0), 1.0];
        m.base_color_texture = shown_texture;
        m.emissive = [0.0; 3];
        if !e.unlit {
            if let Some(c) = &e.emissive {
                m.emissive_texture = bind(c, false);
                let v = color(c.value);
                m.emissive = [v[0], v[1], v[2]];
            }
            m.normal_texture = e.normal.as_ref().and_then(|c| bind(c, true));
        }
        // Opacity: a constant, or the alpha of the displayed texture.
        if let Some(o) = &e.opacity {
            let same_texture = |t: &str| {
                m.base_color_texture
                    .as_ref()
                    .is_some_and(|b| b.texture.eq_ignore_ascii_case(t))
            };
            let translucent = matches!(
                m.blend,
                BlendMode::Translucent | BlendMode::Additive | BlendMode::Modulate
            );
            match texture_path(o) {
                None => m.base_color[3] = clamp01(o.value[0]),
                Some(t) if same_texture(t) => m.base_color[3] = clamp01(o.value[0]),
                Some(_) if translucent => m.base_color[3] = UNREPRESENTED_OPACITY,
                Some(_) => {}
            }
        }
        // UV transform of the displayed texture.
        let shown_channel = if e.unlit {
            e.emissive.as_ref()
        } else {
            Some(&e.base_color)
        };
        if let Some(uv) = shown_channel
            .and_then(|c| c.texture.as_ref())
            .and_then(|t| t.uv.as_ref())
        {
            m.uv = UvTransform {
                channel: u32::try_from(uv.channel).unwrap_or(0).min(7),
                scale: uv.scale.map(|v| finite_or(v, 1.0)),
                offset: uv.offset.map(|v| finite_or(v, 0.0)),
            };
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
}
