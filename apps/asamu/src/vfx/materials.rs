//! The opacity channels of `materials/materials.json` (`asamu-import
//! materials`), which the shared render-material model reduces to a single
//! alpha when the opacity comes from a second texture: the speed-line cone
//! (`ASAMUVelocityEffect.*`: opacity = a wind texture times a constant) and
//! the hard-landing decal (black colour, alpha from an impact texture) need
//! them. Pure parsing through the workspace's `serde_json::Value`.

use std::collections::HashMap;

use asamu_game::asamu_world::fixtures::Value;

/// A material's opacity channel.
#[derive(Clone, Debug, PartialEq)]
pub struct OpacitySpec {
    /// Constant (or texture multiplier), first component.
    pub value: f32,
    /// The opacity texture's object path.
    pub texture: Option<String>,
    /// The texture's channel selection (`rgb`, `a`, ...).
    pub channels: Option<String>,
    /// UV tiling of the texture.
    pub uv_scale: [f32; 2],
    /// The material's blend mode (`alpha_mode`).
    pub alpha_mode: String,
}

/// Parses every entry's opacity channel, keyed by lower-case material path.
///
/// # Errors
/// Not JSON or not an `asamu-materials` document.
pub fn parse_opacity(text: &str) -> Result<HashMap<String, OpacitySpec>, String> {
    let v: Value = text.parse().map_err(|e| format!("not JSON: {e}"))?;
    if v["format"].as_str() != Some("asamu-materials") {
        return Err("not an asamu-materials file".to_owned());
    }
    let entries = v["materials"].as_object().ok_or("no material table")?;
    let mut out = HashMap::with_capacity(entries.len());
    for (path, e) in entries {
        let o = &e["opacity"];
        if o.is_null() {
            continue;
        }
        let value = o["value"][0]
            .as_f64()
            .map(|f| f as f32)
            .filter(|f| f.is_finite())
            .unwrap_or(1.0);
        let tex = &o["texture"];
        let scale = |i: usize| {
            tex["uv"]["scale"][i]
                .as_f64()
                .map(|f| f as f32)
                .filter(|f| f.is_finite())
                .unwrap_or(1.0)
        };
        out.insert(
            path.to_ascii_lowercase(),
            OpacitySpec {
                value,
                texture: tex["texture"].as_str().map(str::to_owned),
                channels: tex["channels"].as_str().map(str::to_owned),
                uv_scale: [scale(0), scale(1)],
                alpha_mode: e["alpha_mode"].as_str().unwrap_or("opaque").to_owned(),
            },
        );
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_opacity_channels() {
        let text = r#"{"format":"asamu-materials","version":1,"materials":{
            "Pkg.Cone":{"alpha_mode":"blend","opacity":{"source":"expression","value":[0.5,0.5,0.5,0.5],
              "texture":{"texture":"Pkg.Wind","channels":"rgb","uv":{"channel":0,"scale":[5.0,5.0]}}}},
            "Pkg.Opaque":{"alpha_mode":"opaque"}
        }}"#;
        let m = parse_opacity(text).unwrap();
        assert_eq!(m.len(), 1);
        let c = &m["pkg.cone"];
        assert_eq!(c.value, 0.5);
        assert_eq!(c.texture.as_deref(), Some("Pkg.Wind"));
        assert_eq!(c.channels.as_deref(), Some("rgb"));
        assert_eq!(c.uv_scale, [5.0, 5.0]);
        assert_eq!(c.alpha_mode, "blend");
        assert!(parse_opacity("{}").is_err());
        assert!(parse_opacity("nope").is_err());
    }
}
