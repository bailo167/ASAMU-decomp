//! Level decals from `asamu-import decals` (`decals/<map>.decals.json`,
//! format `asamu-decals` version 1): parsing with validation, and the
//! render-space mesh data. Pure; `crate::vfx` loads and spawns them.
//!
//! Rendering choice (VFX_DECALS.md §10): **projected meshes**. The importer
//! clips each receiver's triangles against the decal box as the original's
//! cooker did (its output reproduces the cooked receivers of the 99 static
//! decals), so the runtime draws ordinary alpha-blended meshes lying on the
//! receivers — no depth prepass or clustered-decal support needed, and the
//! same result on every platform. Bevy's forward decals were rejected: they
//! are camera-facing quads faded by depth (not projections onto the
//! receivers) and need a depth prepass on the shared camera.
//!
//! The JSON is read through the workspace's `serde_json::Value` (re-exported
//! by `asamu_world::fixtures`; the app has no direct JSON dependency).

use std::collections::HashMap;

use asamu_core::glam::Vec3 as UeVec3;
use asamu_game::asamu_world::fixtures::Value;

/// `format` the parser accepts.
pub const FORMAT: &str = "asamu-decals";
/// `version` the parser accepts.
pub const VERSION: u64 = 1;
/// Most decals per file (hostile-input bound; the largest map has 163).
pub const MAX_DECALS: usize = 4096;
/// Most vertices per receiver mesh (hostile-input bound).
pub const MAX_VERTICES: usize = 1 << 20;
/// Most receiver meshes per decal (hostile-input bound; the importer stops
/// at the same number, the shipped maximum is a few dozen).
pub const MAX_RECEIVERS: usize = 4096;
/// Most vertices kept per file (hostile-input bound; the largest shipped
/// map has about 162,000). Receivers beyond it are dropped and counted.
pub const MAX_FILE_VERTICES: usize = 1 << 22;
/// Distance a level decal is lifted along its normal, UU (ours, against
/// z-fighting; the original uses a depth bias of −6e−5).
pub const LIFT_UU: f32 = 0.2;

/// Decal geometry on one receiver (UE space).
#[derive(Clone, Debug, PartialEq, Default)]
pub struct DecalGeometry {
    /// Receiving component path.
    pub component: Option<String>,
    /// Positions, UU.
    pub positions: Vec<UeVec3>,
    /// Unit normals.
    pub normals: Vec<UeVec3>,
    /// Texture coordinates.
    pub uvs: Vec<[f32; 2]>,
    /// Triangle indices.
    pub indices: Vec<u32>,
}

/// One placed decal actor.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct LevelDecal {
    /// Slot in `ULevel::Actors` (with the level index, the world actor id).
    pub slot: usize,
    /// Actor path.
    pub path: String,
    /// Hidden at level start (`bHidden` or `HiddenGame`).
    pub hidden: bool,
    /// Decal material path.
    pub material: Option<String>,
    /// `SortOrder`.
    pub sort_order: i32,
    /// Geometry per receiver.
    pub receivers: Vec<DecalGeometry>,
}

/// Folder of the decal masks inside `decals/` (the importer's).
pub const MASKS_DIR: &str = "masks";
/// Most masks read per file (hostile-input bound; the shipped decals use
/// 42 over all maps).
pub const MAX_MASKS: usize = 1024;

/// The decals of one map package.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct LevelDecalFile {
    /// Map package name.
    pub package: String,
    /// Decals.
    pub decals: Vec<LevelDecal>,
    /// Decal masks by lower-case material path: the file, relative to
    /// `decals/`, of a white image whose alpha is the material's opacity
    /// mask (for materials that keep their shape in an opacity texture
    /// only; VFX_DECALS.md §9).
    pub masks: HashMap<String, String>,
}

/// A mask file name as the importer writes it: `masks/<name>.dds`, one
/// plain file name below the masks folder.
#[must_use]
pub fn safe_mask_file(file: &str) -> Option<String> {
    let safe = asamu_assets::files::safe_relative_path(file).ok()?;
    let name = safe.strip_prefix(MASKS_DIR)?.strip_prefix('/')?;
    (!name.contains('/') && name.len() > 4 && name.to_ascii_lowercase().ends_with(".dds"))
        .then_some(safe)
}

impl LevelDecalFile {
    /// Triangles over all decals.
    #[must_use]
    pub fn triangles(&self) -> usize {
        self.decals
            .iter()
            .flat_map(|d| &d.receivers)
            .map(|r| r.indices.len() / 3)
            .sum()
    }
}

fn floats(v: &Value, what: &str) -> Result<Vec<f32>, String> {
    let arr = v
        .as_array()
        .ok_or_else(|| format!("{what}: not an array"))?;
    arr.iter()
        .map(|x| {
            x.as_f64()
                .map(|f| f as f32)
                .filter(|f| f.is_finite())
                .ok_or_else(|| format!("{what}: not a finite number"))
        })
        .collect()
}

fn vec3s(v: &Value, what: &str) -> Result<Vec<UeVec3>, String> {
    let f = floats(v, what)?;
    if !f.len().is_multiple_of(3) {
        return Err(format!("{what}: length {} is not a multiple of 3", f.len()));
    }
    Ok(f.as_chunks::<3>()
        .0
        .iter()
        .map(|c| UeVec3::new(c[0], c[1], c[2]))
        .collect())
}

fn parse_receiver(v: &Value) -> Result<DecalGeometry, String> {
    let positions = vec3s(&v["positions"], "positions")?;
    if positions.len() > MAX_VERTICES {
        return Err(format!(
            "{} vertices exceed {MAX_VERTICES}",
            positions.len()
        ));
    }
    let normals = vec3s(&v["normals"], "normals")?;
    let uv_flat = floats(&v["uvs"], "uvs")?;
    let indices: Vec<u32> = v["indices"]
        .as_array()
        .ok_or("indices: not an array")?
        .iter()
        .map(|x| {
            x.as_u64()
                .and_then(|i| u32::try_from(i).ok())
                .ok_or_else(|| "indices: not an index".to_owned())
        })
        .collect::<Result<_, _>>()?;
    if normals.len() != positions.len() || uv_flat.len() != positions.len() * 2 {
        return Err("normals/uvs do not match the positions".to_owned());
    }
    if !indices.len().is_multiple_of(3) || indices.iter().any(|&i| i as usize >= positions.len()) {
        return Err("bad triangle indices".to_owned());
    }
    Ok(DecalGeometry {
        component: v["component"].as_str().map(str::to_owned),
        positions,
        normals,
        uvs: uv_flat.as_chunks::<2>().0.to_vec(),
        indices,
    })
}

/// Parses and validates a decal file. A malformed receiver drops only that
/// receiver (counted in the second value), as do receivers beyond
/// [`MAX_RECEIVERS`] per decal or [`MAX_FILE_VERTICES`] per file; a wrong
/// format, version or structure fails. Unknown fields are ignored.
///
/// # Errors
/// Not JSON, another format or version, or no decal list.
pub fn parse(text: &str) -> Result<(LevelDecalFile, usize), String> {
    let v: Value = text.parse().map_err(|e| format!("not JSON: {e}"))?;
    if v["format"].as_str() != Some(FORMAT) {
        return Err(format!("not an {FORMAT} file"));
    }
    if v["version"].as_u64() != Some(VERSION) {
        return Err(format!("unsupported version {}", v["version"]));
    }
    let list = v["decals"].as_array().ok_or("no decal list")?;
    if list.len() > MAX_DECALS {
        return Err(format!("{} decals exceed {MAX_DECALS}", list.len()));
    }
    let mut dropped = 0usize;
    let mut vertices = 0usize;
    let mut decals = Vec::with_capacity(list.len());
    for d in list {
        let Some(slot) = d["slot"].as_u64().and_then(|s| usize::try_from(s).ok()) else {
            dropped += 1;
            continue;
        };
        let mut receivers = Vec::new();
        for r in d["receivers"].as_array().map(Vec::as_slice).unwrap_or(&[]) {
            if receivers.len() >= MAX_RECEIVERS {
                dropped += 1;
                continue;
            }
            match parse_receiver(r) {
                Ok(g) if !g.indices.is_empty() => {
                    if vertices.saturating_add(g.positions.len()) > MAX_FILE_VERTICES {
                        dropped += 1;
                        continue;
                    }
                    vertices += g.positions.len();
                    receivers.push(g);
                }
                Ok(_) => {}
                Err(_) => dropped += 1,
            }
        }
        decals.push(LevelDecal {
            slot,
            path: d["path"].as_str().unwrap_or_default().to_owned(),
            hidden: d["hidden"].as_bool().unwrap_or(false),
            material: d["material"].as_str().map(str::to_owned),
            sort_order: d["sort_order"]
                .as_i64()
                .and_then(|s| i32::try_from(s).ok())
                .unwrap_or(0),
            receivers,
        });
    }
    // Masks: entries with an unsafe or foreign file name are dropped.
    let mut masks = HashMap::new();
    if let Some(table) = v["masks"].as_object() {
        for (material, entry) in table.iter().take(MAX_MASKS) {
            match entry["file"].as_str().and_then(safe_mask_file) {
                Some(file) => {
                    masks.insert(material.to_ascii_lowercase(), file);
                }
                None => dropped += 1,
            }
        }
    }
    Ok((
        LevelDecalFile {
            package: v["package"].as_str().unwrap_or_default().to_owned(),
            decals,
            masks,
        },
        dropped,
    ))
}

/// Render-space mesh data of a decal geometry: positions lifted by
/// [`LIFT_UU`] along their normals and converted with `to_render`, normals
/// with `dir`.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct RenderGeometry {
    /// Positions.
    pub positions: Vec<[f32; 3]>,
    /// Normals.
    pub normals: Vec<[f32; 3]>,
    /// Texture coordinates.
    pub uvs: Vec<[f32; 2]>,
    /// Indices.
    pub indices: Vec<u32>,
}

/// Converts `g` with the given position and direction mappings.
#[must_use]
pub fn render_geometry(
    g: &DecalGeometry,
    pos: impl Fn(UeVec3) -> [f32; 3],
    dir: impl Fn(UeVec3) -> [f32; 3],
) -> RenderGeometry {
    RenderGeometry {
        positions: g
            .positions
            .iter()
            .zip(&g.normals)
            .map(|(p, n)| pos(*p + *n * LIFT_UU))
            .collect(),
        normals: g.normals.iter().map(|n| dir(*n)).collect(),
        uvs: g.uvs.clone(),
        indices: g.indices.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> String {
        r#"{"format":"asamu-decals","version":1,"package":"Map","decals":[
            {"slot":4,"path":"Map.TheWorld.PersistentLevel.DecalActorMovable_0","hidden":true,
             "material":"Pkg.M_Decal","sort_order":2,"receivers":[
               {"component":"Map.C","positions":[0,0,0, 10,0,0, 0,10,0],"normals":[0,0,1, 0,0,1, 0,0,1],
                "uvs":[0,0, 1,0, 0,1],"indices":[0,1,2]},
               {"component":"Map.Bad","positions":[0,0,0],"normals":[0,0,1],"uvs":[0,0],"indices":[0,1,2]},
               {"component":"Map.Empty","positions":[],"normals":[],"uvs":[],"indices":[]}
             ]},
            {"path":"no slot"}
        ]}"#
        .to_owned()
    }

    #[test]
    fn parses_and_validates() {
        let (f, dropped) = parse(&sample()).unwrap();
        assert_eq!(f.package, "Map");
        assert_eq!(f.decals.len(), 1);
        assert_eq!(dropped, 2, "bad receiver + decal without slot");
        let d = &f.decals[0];
        assert_eq!((d.slot, d.hidden, d.sort_order), (4, true, 2));
        assert_eq!(d.material.as_deref(), Some("Pkg.M_Decal"));
        assert_eq!(d.receivers.len(), 1);
        assert_eq!(d.receivers[0].positions[1], UeVec3::new(10.0, 0.0, 0.0));
        assert_eq!(d.receivers[0].uvs[2], [0.0, 1.0]);
        assert_eq!(f.triangles(), 1);
    }

    #[test]
    fn rejects_other_documents() {
        assert!(parse("not json").is_err());
        assert!(parse(r#"{"format":"asamu-scene","version":1,"decals":[]}"#).is_err());
        assert!(parse(r#"{"format":"asamu-decals","version":2,"decals":[]}"#).is_err());
        assert!(parse(r#"{"format":"asamu-decals","version":1}"#).is_err());
        // Non-finite numbers cannot appear in JSON; strings in place of
        // numbers drop the receiver.
        let bad = sample().replace("10,0,0", "\"x\",0,0");
        let (f, dropped) = parse(&bad).unwrap();
        assert!(f.decals[0].receivers.is_empty());
        assert_eq!(dropped, 3);
    }

    #[test]
    fn masks_are_read_with_safe_file_names_only() {
        let text = r#"{"format":"asamu-decals","version":1,"package":"M","decals":[],"masks":{
            "Pkg.Mat_A":{"file":"masks/m_Pkg_Mat_A-0011aabb.dds","channel":"r"},
            "Pkg.Up":{"file":"masks/../../secret.dds"},
            "Pkg.Abs":{"file":"/etc/masks/x.dds"},
            "Pkg.Deep":{"file":"masks/sub/x.dds"},
            "Pkg.Other":{"file":"textures/x.dds"},
            "Pkg.Ext":{"file":"masks/x.png"},
            "Pkg.NoFile":{"channel":"a"},
            "Pkg.Url":{"file":"masks/http://x.dds"}
        }}"#;
        let (f, dropped) = parse(text).unwrap();
        assert_eq!(f.masks.len(), 1);
        assert_eq!(
            f.masks.get("pkg.mat_a").map(String::as_str),
            Some("masks/m_Pkg_Mat_A-0011aabb.dds")
        );
        assert_eq!(dropped, 7);
        assert_eq!(safe_mask_file("masks/.dds"), None);
        assert_eq!(
            safe_mask_file("masks/a.DDS").as_deref(),
            Some("masks/a.DDS")
        );
        assert_eq!(safe_mask_file("masks"), None);
        assert_eq!(safe_mask_file("masksx/a.dds"), None);
        // Files written before masks existed have none.
        let (f, _) = parse(&sample()).unwrap();
        assert!(f.masks.is_empty());
    }

    #[test]
    fn budgets_drop_receivers_instead_of_growing() {
        let receiver = r#"{"positions":[0,0,0, 1,0,0, 0,1,0],"normals":[0,0,1, 0,0,1, 0,0,1],
            "uvs":[0,0, 1,0, 0,1],"indices":[0,1,2],"listed":false}"#;
        let many = vec![receiver; MAX_RECEIVERS + 5].join(",");
        let text = format!(
            r#"{{"format":"asamu-decals","version":1,"package":"M","decals":[
                {{"slot":1,"mirrored":true,"receivers":[{many}]}}]}}"#
        );
        let (f, dropped) = parse(&text).unwrap();
        assert_eq!(f.decals[0].receivers.len(), MAX_RECEIVERS);
        assert_eq!(dropped, 5);
        // Too many decals is a refusal, not a truncation.
        let decals = vec![r#"{"slot":1}"#; MAX_DECALS + 1].join(",");
        let text = format!(r#"{{"format":"asamu-decals","version":1,"decals":[{decals}]}}"#);
        assert!(parse(&text).is_err());
        // Indices must stay inside the receiver, also the largest ones.
        let bad = receiver.replace("[0,1,2]", "[0,1,4294967295]");
        let text = format!(
            r#"{{"format":"asamu-decals","version":1,"decals":[{{"slot":1,"receivers":[{bad}]}}]}}"#
        );
        let (f, dropped) = parse(&text).unwrap();
        assert!(f.decals[0].receivers.is_empty());
        assert_eq!(dropped, 1);
        // Numbers too large for an f32 are not finite: the receiver goes.
        let huge = receiver.replace("1,0,0", "1e39,0,0");
        let text = format!(
            r#"{{"format":"asamu-decals","version":1,"decals":[{{"slot":1,"receivers":[{huge}]}}]}}"#
        );
        let (f, dropped) = parse(&text).unwrap();
        assert!(f.decals[0].receivers.is_empty());
        assert_eq!(dropped, 1);
    }

    #[test]
    fn render_geometry_lifts_along_the_normal() {
        let (f, _) = parse(&sample()).unwrap();
        let g = &f.decals[0].receivers[0];
        let r = render_geometry(g, |p| p.to_array(), |n| n.to_array());
        assert_eq!(r.positions[0], [0.0, 0.0, LIFT_UU]);
        assert_eq!(r.indices, vec![0, 1, 2]);
        assert_eq!(r.normals.len(), 3);
    }
}
