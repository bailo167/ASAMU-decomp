//! Rendering view of a converted level scene (`levels/<map>.scene.json`).
//!
//! The scene JSON (`format` `asamu-scene`, version 1) is written by
//! `asamu-import levels` from `asamu_ue3::level::Scene` and documented in
//! `docs/reverse-engineering/LEVEL_FORMAT.md`. This module parses it on its
//! own (the runtime does not depend on the UE3 parser) and keeps only what
//! rendering and level start-up need:
//!
//! - every visible **static mesh component** with its world matrix and
//!   per-slot material overrides (static mesh actors, `InterpActor`s, ASAMU
//!   actors with meshes, ...);
//! - **lights** (point, spot, directional, sky) from light components;
//! - **player starts**, `WorldInfo` (title, `KillZ`), and the fog and sky
//!   related actors that exist (presence only: their parameters live on
//!   components the scene does not export yet).
//!
//! Positions stay in UE3 world space (UU, left-handed, X forward, Y right, Z
//! up); conversion to render space happens in [`crate::transform`].
//!
//! Visibility rules (render side; UE3 semantics, TENTATIVE for ASAMU): a mesh
//! component is drawn unless its actor has `bHidden` or the component has
//! `HiddenGame`. Light actors are `bHidden` by class default, but their light
//! components still light the level, so `bHidden` does not apply to lights.

use std::collections::BTreeMap;
use std::path::Path;

use asamu_core::glam::{Mat4, Vec3};
use serde::Deserialize;

use crate::error::{AssetError, AssetResult};
use crate::files::parse_json;
use crate::transform::{UeRowMatrix, light_location_direction, ue_row_matrix_to_mat4};

/// `format` of a scene file.
pub const SCENE_FORMAT: &str = "asamu-scene";
/// Scene versions this reader understands.
pub const SCENE_VERSION: u32 = 1;

// ------------------------------------------------------------ raw JSON

#[derive(Debug, Deserialize)]
struct RawScene {
    format: String,
    version: u32,
    #[serde(default)]
    package: String,
    #[serde(default)]
    world_info: Option<RawWorldInfo>,
    #[serde(default)]
    streaming_levels: Vec<RawStreaming>,
    #[serde(default)]
    actors: Vec<RawActor>,
}

#[derive(Debug, Deserialize)]
struct RawWorldInfo {
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    kill_z: Option<f32>,
    #[serde(default)]
    default_gravity_z: Option<f32>,
}

/// Lenient float readers: values are parsed as `f64` and narrowed to `f32`, so an
/// out-of-range number becomes `±inf` (and is filtered later) instead of failing the
/// whole scene. This keeps parsing identical whether or not `serde_json`'s
/// `float_roundtrip` feature is unified into the build (it rejects `f32` overflow).
mod lenient {
    use serde::{Deserialize, Deserializer};

    #[allow(clippy::cast_possible_truncation)]
    pub fn f<'de, D: Deserializer<'de>>(d: D) -> Result<f32, D::Error> {
        Ok(f64::deserialize(d)? as f32)
    }

    #[allow(clippy::cast_possible_truncation)]
    pub fn v3<'de, D: Deserializer<'de>>(d: D) -> Result<[f32; 3], D::Error> {
        Ok(<[f64; 3]>::deserialize(d)?.map(|x| x as f32))
    }

    #[allow(clippy::cast_possible_truncation)]
    pub fn m4<'de, D: Deserializer<'de>>(d: D) -> Result<[[f32; 4]; 4], D::Error> {
        Ok(<[[f64; 4]; 4]>::deserialize(d)?.map(|r| r.map(|x| x as f32)))
    }
}

#[derive(Debug, Deserialize)]
struct RawStreaming {
    #[serde(default)]
    class: String,
    #[serde(default)]
    package_name: Option<String>,
    #[serde(default, deserialize_with = "lenient::v3")]
    offset: [f32; 3],
}

#[derive(Debug, Deserialize)]
struct RawActor {
    #[serde(default)]
    slot: usize,
    #[serde(default)]
    name: String,
    #[serde(default)]
    class: String,
    #[serde(default)]
    kind: String,
    #[serde(default, deserialize_with = "lenient::v3")]
    location: [f32; 3],
    #[serde(default)]
    rotation: [i32; 3],
    #[serde(default)]
    hidden: bool,
    #[serde(default)]
    block_actors: bool,
    #[serde(default)]
    is_static: bool,
    #[serde(default)]
    components: Vec<RawComponent>,
    #[serde(default)]
    matinee: Vec<serde::de::IgnoredAny>,
    /// Only the player-start flags are read; every other parameter is
    /// skipped without being stored.
    #[serde(default)]
    params: StartFlags,
}

/// `bEnabled` / `bPrimaryStart` from an actor's `params` (names matched
/// without regard to ASCII case, like the world crate's parameter lookup;
/// values that are not booleans count as absent).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct StartFlags {
    enabled: Option<bool>,
    primary: Option<bool>,
}

impl<'de> Deserialize<'de> for StartFlags {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct Visitor;
        impl<'de> serde::de::Visitor<'de> for Visitor {
            type Value = StartFlags;
            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("an actor parameter map")
            }
            fn visit_unit<E: serde::de::Error>(self) -> Result<StartFlags, E> {
                Ok(StartFlags::default())
            }
            fn visit_map<A: serde::de::MapAccess<'de>>(
                self,
                mut map: A,
            ) -> Result<StartFlags, A::Error> {
                let mut out = StartFlags::default();
                while let Some(key) = map.next_key::<std::borrow::Cow<'de, str>>()? {
                    let slot = if key.eq_ignore_ascii_case("bEnabled") {
                        &mut out.enabled
                    } else if key.eq_ignore_ascii_case("bPrimaryStart") {
                        &mut out.primary
                    } else {
                        map.next_value::<serde::de::IgnoredAny>()?;
                        continue;
                    };
                    let v: serde_json::Value = map.next_value()?;
                    // An exact-case key wins over a case-folded duplicate.
                    if slot.is_none() || key == "bEnabled" || key == "bPrimaryStart" {
                        *slot = v.as_bool().or(*slot);
                    }
                }
                Ok(out)
            }
        }
        d.deserialize_any(Visitor)
    }
}

#[derive(Debug, Deserialize)]
struct RawComponent {
    #[serde(default)]
    name: String,
    #[serde(default)]
    kind: String,
    #[serde(deserialize_with = "lenient::m4")]
    local_to_world: UeRowMatrix,
    #[serde(default)]
    hidden_game: bool,
    #[serde(default)]
    block_actors: bool,
    #[serde(default)]
    static_mesh: Option<String>,
    #[serde(default)]
    materials: Vec<Option<String>>,
    #[serde(default)]
    light: Option<RawLight>,
    #[serde(default)]
    cylinder: Option<[f32; 2]>,
}

#[derive(Debug, Deserialize)]
struct RawLight {
    #[serde(default)]
    light_type: String,
    #[serde(default, deserialize_with = "lenient::f")]
    brightness: f32,
    #[serde(default)]
    color: [u8; 4],
    #[serde(default)]
    radius: Option<f32>,
    #[serde(default)]
    falloff_exponent: Option<f32>,
    #[serde(default)]
    inner_cone_angle: Option<f32>,
    #[serde(default)]
    outer_cone_angle: Option<f32>,
    #[serde(default = "yes")]
    enabled: bool,
    #[serde(default)]
    cast_shadows: bool,
}

fn yes() -> bool {
    true
}

// ------------------------------------------------------------ model

/// A streaming sub-level (`WorldInfo.StreamingLevels`).
#[derive(Debug, Clone, PartialEq)]
pub struct StreamingInfo {
    /// Sub-level package name.
    pub package: String,
    /// `LevelStreaming*` class (`LevelStreamingAlwaysLoaded`,
    /// `LevelStreamingKismet`, ...).
    pub class: String,
    /// `Offset` (UE3, UU).
    pub offset: Vec3,
}

impl StreamingInfo {
    /// Loaded together with the persistent level (UE3
    /// `LevelStreamingAlwaysLoaded`); the others are streamed in by Kismet.
    #[must_use]
    pub fn always_loaded(&self) -> bool {
        self.class.to_ascii_lowercase().contains("alwaysloaded")
    }
}

/// One static mesh component to draw.
#[derive(Debug, Clone, PartialEq)]
pub struct MeshInstance {
    /// Level the actor belongs to: 0 = the persistent level, then the merged
    /// sub-levels in merge order.
    pub level: usize,
    /// Index of the actor in `ULevel::Actors`.
    pub actor_slot: usize,
    /// Actor object name.
    pub actor_name: String,
    /// Actor class path (`Engine.StaticMeshActor`, ...).
    pub actor_class: String,
    /// Actor gameplay kind from the scene (`static_mesh`, `interp_actor`, ...).
    pub actor_kind: String,
    /// Component object name.
    pub component_name: String,
    /// Static mesh object path.
    pub mesh: String,
    /// Material overrides per slot (`None` keeps the mesh's material).
    pub material_overrides: Vec<Option<String>>,
    /// Component world matrix in UE3 space (column-vector convention).
    pub ue_local_to_world: Mat4,
    /// The actor is static (`bStatic`): it never moves.
    pub is_static: bool,
    /// The actor is driven by Matinee (moves at run time once Matinee runs).
    pub matinee_driven: bool,
    /// The component blocks actors (a collision hint; gameplay collision is
    /// the world crate's business).
    pub blocks: bool,
}

/// Kind of a UE3 light component (from its class name).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum UeLightKind {
    /// `PointLightComponent` and subclasses.
    Point,
    /// `SpotLightComponent` and subclasses.
    Spot,
    /// `DirectionalLightComponent` and subclasses.
    Directional,
    /// `SkyLightComponent`.
    Sky,
    /// Anything else.
    Other,
}

impl UeLightKind {
    /// Classifies a light component class name.
    #[must_use]
    pub fn from_class(class: &str) -> Self {
        let c = class.to_ascii_lowercase();
        if c.contains("skylight") {
            Self::Sky
        } else if c.contains("spotlight") {
            Self::Spot
        } else if c.contains("directionallight") {
            Self::Directional
        } else if c.contains("pointlight") {
            Self::Point
        } else {
            Self::Other
        }
    }
}

/// One light component.
#[derive(Debug, Clone, PartialEq)]
pub struct SceneLight {
    /// Level of the owning actor (see [`MeshInstance::level`]).
    pub level: usize,
    /// Index of the owning actor.
    pub actor_slot: usize,
    /// Owning actor name.
    pub actor_name: String,
    /// Light component class (`PointLightComponent`, ...).
    pub light_class: String,
    /// Kind.
    pub kind: UeLightKind,
    /// World location (UE3, UU).
    pub location_ue: Vec3,
    /// Direction the light points along (UE3, unit).
    pub direction_ue: Vec3,
    /// `Brightness`.
    pub brightness: f32,
    /// `LightColor` R, G, B (sRGB bytes).
    pub color_srgb: [u8; 3],
    /// `Radius` (UU).
    pub radius: Option<f32>,
    /// `FalloffExponent`.
    pub falloff_exponent: Option<f32>,
    /// `InnerConeAngle` (degrees).
    pub inner_cone_angle: Option<f32>,
    /// `OuterConeAngle` (degrees).
    pub outer_cone_angle: Option<f32>,
    /// `bEnabled`.
    pub enabled: bool,
    /// `CastShadows`.
    pub cast_shadows: bool,
}

/// A player start.
#[derive(Debug, Clone, PartialEq)]
pub struct PlayerStartInfo {
    /// Actor name.
    pub name: String,
    /// Location (UE3, UU): the centre of the start's collision cylinder.
    pub location_ue: Vec3,
    /// `Rotation` (pitch, yaw, roll; 65536 per turn).
    pub rotation: [i32; 3],
    /// Collision cylinder (radius, half height), when present.
    pub cylinder: Option<[f32; 2]>,
    /// `bEnabled` (absent = true, the class default).
    pub enabled: bool,
    /// `bPrimaryStart` (absent = true, the class default).
    pub primary: bool,
}

/// Counts over the parsed scene.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LevelSceneStats {
    /// Actors in the file.
    pub actors: usize,
    /// Static mesh components seen.
    pub mesh_components: usize,
    /// Of those, drawn (in [`LevelScene::meshes`]).
    pub drawn: usize,
    /// Skipped: actor `bHidden` or component `HiddenGame`.
    pub hidden: usize,
    /// Skipped: no static mesh assigned.
    pub no_mesh: usize,
    /// Skipped: non-finite world matrix.
    pub bad_transform: usize,
    /// Light components.
    pub lights: usize,
    /// Actors per class name (only the ones the renderer approximates:
    /// fog, sky and post-process related classes).
    pub atmosphere_actors: BTreeMap<String, usize>,
}

/// The rendering view of one level.
#[derive(Debug, Clone, PartialEq)]
pub struct LevelScene {
    /// Map package name.
    pub package: String,
    /// `WorldInfo.Title`.
    pub title: Option<String>,
    /// `WorldInfo.KillZ`.
    pub kill_z: Option<f32>,
    /// `WorldInfo.DefaultGravityZ`.
    pub default_gravity_z: Option<f32>,
    /// Streaming sub-levels.
    pub streaming_levels: Vec<StreamingInfo>,
    /// Sub-levels merged into this scene ([`Self::merge_sublevel`]).
    pub merged_levels: Vec<String>,
    /// Static mesh components to draw.
    pub meshes: Vec<MeshInstance>,
    /// Light components.
    pub lights: Vec<SceneLight>,
    /// Player starts in actor order.
    pub player_starts: Vec<PlayerStartInfo>,
    /// Counts.
    pub stats: LevelSceneStats,
}

/// True for actor classes that carry fog, sky or atmosphere settings the
/// renderer can only approximate (counted in
/// [`LevelSceneStats::atmosphere_actors`]).
fn is_atmosphere_class(class: &str) -> bool {
    let c = class.to_ascii_lowercase();
    ["fog", "sky", "postprocess", "lightmass"]
        .iter()
        .any(|k| c.contains(k))
}

fn finite3(v: [f32; 3]) -> bool {
    v.iter().all(|c| c.is_finite())
}

impl LevelScene {
    /// Parses a scene JSON document read from `path`.
    ///
    /// # Errors
    /// Malformed JSON, a wrong `format`, or an unsupported `version`.
    pub fn from_json(path: &Path, data: &[u8]) -> AssetResult<Self> {
        let raw: RawScene = parse_json(path, data)?;
        if raw.format != SCENE_FORMAT {
            return Err(AssetError::Format {
                path: path.to_path_buf(),
                expected: format!("format {SCENE_FORMAT:?}"),
                found: format!("format {:?}", raw.format),
            });
        }
        if raw.version != SCENE_VERSION {
            return Err(AssetError::Format {
                path: path.to_path_buf(),
                expected: format!("version {SCENE_VERSION}"),
                found: format!("version {}", raw.version),
            });
        }
        Ok(Self::from_raw(raw))
    }

    fn from_raw(raw: RawScene) -> Self {
        let mut stats = LevelSceneStats {
            actors: raw.actors.len(),
            ..LevelSceneStats::default()
        };
        let mut meshes = Vec::new();
        let mut lights = Vec::new();
        let mut player_starts = Vec::new();
        for actor in raw.actors {
            if is_atmosphere_class(&actor.class) {
                let short = actor
                    .class
                    .rsplit('.')
                    .next()
                    .unwrap_or(&actor.class)
                    .to_owned();
                *stats.atmosphere_actors.entry(short).or_default() += 1;
            }
            if actor.kind == "player_start" && finite3(actor.location) {
                player_starts.push(PlayerStartInfo {
                    name: actor.name.clone(),
                    location_ue: Vec3::from_array(actor.location),
                    rotation: actor.rotation,
                    cylinder: actor
                        .components
                        .iter()
                        .find_map(|c| c.cylinder)
                        .filter(|c| finite3([c[0], c[1], 0.0])),
                    enabled: actor.params.enabled.unwrap_or(true),
                    primary: actor.params.primary.unwrap_or(true),
                });
            }
            let matinee_driven = !actor.matinee.is_empty();
            for comp in &actor.components {
                if let Some(l) = &comp.light {
                    stats.lights += 1;
                    let (location_ue, direction_ue) =
                        light_location_direction(&comp.local_to_world);
                    lights.push(SceneLight {
                        level: 0,
                        actor_slot: actor.slot,
                        actor_name: actor.name.clone(),
                        light_class: l.light_type.clone(),
                        kind: UeLightKind::from_class(&l.light_type),
                        location_ue,
                        direction_ue,
                        brightness: l.brightness,
                        color_srgb: [l.color[0], l.color[1], l.color[2]],
                        radius: l.radius,
                        falloff_exponent: l.falloff_exponent,
                        inner_cone_angle: l.inner_cone_angle,
                        outer_cone_angle: l.outer_cone_angle,
                        enabled: l.enabled,
                        cast_shadows: l.cast_shadows,
                    });
                }
                if comp.kind != "static_mesh" {
                    continue;
                }
                stats.mesh_components += 1;
                if actor.hidden || comp.hidden_game {
                    stats.hidden += 1;
                    continue;
                }
                let Some(mesh) = comp.static_mesh.clone().filter(|m| !m.is_empty()) else {
                    stats.no_mesh += 1;
                    continue;
                };
                let m = ue_row_matrix_to_mat4(&comp.local_to_world);
                if !m.is_finite() {
                    stats.bad_transform += 1;
                    continue;
                }
                stats.drawn += 1;
                meshes.push(MeshInstance {
                    level: 0,
                    actor_slot: actor.slot,
                    actor_name: actor.name.clone(),
                    actor_class: actor.class.clone(),
                    actor_kind: actor.kind.clone(),
                    component_name: comp.name.clone(),
                    mesh,
                    material_overrides: comp.materials.clone(),
                    ue_local_to_world: m,
                    is_static: actor.is_static,
                    matinee_driven,
                    blocks: actor.block_actors && comp.block_actors,
                });
            }
        }
        let world = raw.world_info;
        Self {
            package: raw.package,
            title: world.as_ref().and_then(|w| w.title.clone()),
            kill_z: world.as_ref().and_then(|w| w.kill_z),
            default_gravity_z: world.as_ref().and_then(|w| w.default_gravity_z),
            streaming_levels: raw
                .streaming_levels
                .into_iter()
                .filter_map(|s| {
                    let package = s.package_name.filter(|p| !p.is_empty())?;
                    Some(StreamingInfo {
                        package,
                        class: s.class,
                        offset: if finite3(s.offset) {
                            Vec3::from_array(s.offset)
                        } else {
                            Vec3::ZERO
                        },
                    })
                })
                .collect(),
            merged_levels: Vec::new(),
            meshes,
            lights,
            player_starts,
            stats,
        }
    }

    /// Appends a streaming sub-level's meshes, lights and player starts,
    /// moved by `offset` (the streaming `Offset`; zero in every shipped map,
    /// `LEVEL_FORMAT.md`). Returns the level index given to its contents.
    pub fn merge_sublevel(&mut self, sub: LevelScene, offset: Vec3) -> usize {
        let level = self.merged_levels.len() + 1;
        let shift = asamu_core::glam::Mat4::from_translation(offset);
        for mut m in sub.meshes {
            m.level = level;
            m.ue_local_to_world = shift * m.ue_local_to_world;
            self.meshes.push(m);
        }
        for mut l in sub.lights {
            l.level = level;
            l.location_ue += offset;
            self.lights.push(l);
        }
        self.stats.actors += sub.stats.actors;
        self.stats.mesh_components += sub.stats.mesh_components;
        self.stats.drawn += sub.stats.drawn;
        self.stats.hidden += sub.stats.hidden;
        self.stats.no_mesh += sub.stats.no_mesh;
        self.stats.bad_transform += sub.stats.bad_transform;
        self.stats.lights += sub.stats.lights;
        for (k, v) in sub.stats.atmosphere_actors {
            *self.stats.atmosphere_actors.entry(k).or_default() += v;
        }
        self.merged_levels.push(sub.package);
        level
    }

    /// The player start the game spawns at, by the same rule as
    /// `asamu_world::gameplay::SceneActors::player_start` (so the render
    /// camera and the simulation agree): the first enabled primary start,
    /// else the first enabled one, else the first. UE3 rates starts at run
    /// time (`FindPlayerStart`); this fixed rule is TENTATIVE for maps with
    /// several starts. Each of the 10 shipped maps that has a start has
    /// exactly one (`tests/converted_real_data.rs` prints the counts).
    #[must_use]
    pub fn player_start(&self) -> Option<&PlayerStartInfo> {
        let starts = &self.player_starts;
        starts
            .iter()
            .find(|p| p.enabled && p.primary)
            .or_else(|| starts.iter().find(|p| p.enabled))
            .or_else(|| starts.first())
    }

    /// Axis-aligned UE3 bounds of the drawn mesh instances' origins and of
    /// the lights, `None` for an empty scene. A cheap stand-in for the level
    /// extent (camera far plane, fly-camera speed).
    #[must_use]
    pub fn origin_bounds(&self) -> Option<(Vec3, Vec3)> {
        let points = self
            .meshes
            .iter()
            .map(|m| m.ue_local_to_world.w_axis.truncate())
            .chain(self.lights.iter().map(|l| l.location_ue))
            .chain(self.player_starts.iter().map(|p| p.location_ue))
            .filter(|p| p.is_finite());
        let mut out: Option<(Vec3, Vec3)> = None;
        for p in points {
            out = Some(match out {
                None => (p, p),
                Some((lo, hi)) => (lo.min(p), hi.max(p)),
            });
        }
        out
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A small synthetic scene in the importer's format (hand-written; no
    /// game data).
    pub(crate) fn synthetic_scene_json() -> String {
        let ident = "[[1,0,0,0],[0,1,0,0],[0,0,1,0],[0,0,0,1]]";
        let moved = "[[2,0,0,0],[0,2,0,0],[0,0,2,0],[100,200,300,1]]";
        format!(
            r#"{{
  "format": "asamu-scene", "version": 1, "package": "TestMap", "level": "TestMap.TheWorld.PersistentLevel",
  "level_export": 1, "coordinates": "UE3", "tail": {{"anything": true}},
  "world_info": {{"object": "W", "title": "Test", "kill_z": -1000.0, "soft_kill_z": false,
                  "default_gravity_z": -520.0, "global_gravity_z": 0.0, "default_game_type": null}},
  "streaming_levels": [{{"object": "S", "class": "LevelStreamingKismet", "package_name": "Sub", "offset": [0,0,0], "params": {{}}}}],
  "bsp_model": null,
  "actors": [
    {{"slot": 0, "name": "WorldInfo_0", "class": "Engine.WorldInfo", "kind": "world_info", "components": []}},
    {{"slot": 2, "name": "StaticMeshActor_1", "class": "Engine.StaticMeshActor", "kind": "static_mesh",
      "location": [100,200,300], "rotation": [0,0,0], "hidden": false, "block_actors": true, "is_static": true,
      "components": [{{"name": "SMC_1", "kind": "static_mesh", "local_to_world": {moved}, "hidden_game": false,
                       "block_actors": true, "static_mesh": "Pkg.Meshes.Box", "materials": [null, "Pkg.M_Override"]}}],
      "params": {{}}, "instance": {{"Location": {{"X": 100}}}}, "volume": null, "matinee": []}},
    {{"slot": 3, "name": "CameraActor_0", "class": "Engine.CameraActor", "kind": "other",
      "components": [{{"name": "Cam", "kind": "static_mesh", "local_to_world": {ident}, "hidden_game": true,
                       "static_mesh": "EditorMeshes.MatineeCam_SM"}}]}},
    {{"slot": 4, "name": "Fog_0", "class": "Engine.FogVolumeConstantDensityInfo", "kind": "other", "hidden": true,
      "components": [{{"name": "Auto", "kind": "static_mesh", "local_to_world": {ident}, "static_mesh": "EngineMeshes.Cube"}}]}},
    {{"slot": 5, "name": "InterpActor_0", "class": "Engine.InterpActor", "kind": "interp_actor",
      "components": [{{"name": "SMC_2", "kind": "static_mesh", "local_to_world": {ident}, "static_mesh": "Pkg.Meshes.Door"}},
                     {{"name": "SMC_3", "kind": "static_mesh", "local_to_world": {ident}, "static_mesh": null}}],
      "matinee": [{{"action": "A", "interp_data": null, "group": "Door"}}]}},
    {{"slot": 6, "name": "PointLight_0", "class": "Engine.PointLight", "kind": "light", "hidden": true,
      "components": [{{"name": "PLC", "kind": "light", "local_to_world": [[0.5,0,0,0],[0,0.5,0,0],[0,0,0.5,0],[10,20,30,1]],
                       "light": {{"light_type": "PointLightComponent", "brightness": 1.5, "color": [255,200,100,0],
                                  "radius": 256.0, "falloff_exponent": 2.0, "inner_cone_angle": null,
                                  "outer_cone_angle": null, "enabled": true, "cast_shadows": true}}}}]}},
    {{"slot": 7, "name": "DominantDirectionalLight_0", "class": "Engine.DominantDirectionalLight", "kind": "light",
      "components": [{{"name": "DLC", "kind": "light", "local_to_world": [[0,0,-1,0],[0,1,0,0],[1,0,0,0],[0,0,0,1]],
                       "light": {{"light_type": "DominantDirectionalLightComponent", "brightness": 1.0,
                                  "color": [255,255,255,0], "enabled": true, "cast_shadows": true}}}}]}},
    {{"slot": 8, "name": "PlayerStart_0", "class": "Engine.PlayerStart", "kind": "player_start",
      "location": [-50, 25, 90], "rotation": [0, 16384, 0],
      "components": [{{"name": "Cyl", "kind": "cylinder", "local_to_world": {ident}, "cylinder": [40, 80]}}]}},
    {{"slot": 9, "name": "Bad_0", "class": "Engine.StaticMeshActor", "kind": "static_mesh",
      "components": [{{"name": "SMC_9", "kind": "static_mesh", "local_to_world": [[1e39,0,0,0],[0,1,0,0],[0,0,1,0],[0,0,0,1]],
                       "static_mesh": "Pkg.Meshes.Box"}}]}}
  ],
  "stats": {{}}, "warnings": []
}}"#
        )
    }

    #[test]
    fn synthetic_scene_parses_into_the_render_view() {
        let json = synthetic_scene_json();
        let s = LevelScene::from_json(Path::new("TestMap.scene.json"), json.as_bytes()).unwrap();
        assert_eq!(s.package, "TestMap");
        assert_eq!(s.title.as_deref(), Some("Test"));
        assert_eq!(s.kill_z, Some(-1000.0));
        assert_eq!(s.streaming_levels.len(), 1);
        assert_eq!(s.streaming_levels[0].package, "Sub");
        assert!(!s.streaming_levels[0].always_loaded());
        // Drawn: the static mesh actor and the interp actor's door.
        assert_eq!(s.meshes.len(), 2);
        let box_ = &s.meshes[0];
        assert_eq!(box_.mesh, "Pkg.Meshes.Box");
        assert_eq!(
            box_.material_overrides,
            vec![None, Some("Pkg.M_Override".to_owned())]
        );
        assert!(box_.is_static && box_.blocks && !box_.matinee_driven);
        assert_eq!(
            box_.ue_local_to_world.transform_point3(Vec3::X),
            Vec3::new(102.0, 200.0, 300.0)
        );
        let door = &s.meshes[1];
        assert!(door.matinee_driven);
        assert_eq!(door.actor_kind, "interp_actor");
        // 1e39 overflows f32 to infinity: refused, counted.
        assert_eq!(s.stats.bad_transform, 1);
        assert_eq!(
            s.stats.hidden, 2,
            "camera mesh (HiddenGame) and fog volume (bHidden)"
        );
        assert_eq!(s.stats.no_mesh, 1);
        assert_eq!(s.stats.mesh_components, 6);
        assert_eq!(s.stats.drawn, 2);
        // Lights: the hidden light actor still lights.
        assert_eq!(s.lights.len(), 2);
        let p = &s.lights[0];
        assert_eq!(p.kind, UeLightKind::Point);
        assert_eq!(p.location_ue, Vec3::new(10.0, 20.0, 30.0));
        assert_eq!(p.direction_ue, Vec3::X);
        assert_eq!(p.color_srgb, [255, 200, 100]);
        let d = &s.lights[1];
        assert_eq!(d.kind, UeLightKind::Directional);
        assert_eq!(d.direction_ue, Vec3::NEG_Z);
        assert!(d.radius.is_none());
        // Player start.
        assert_eq!(s.player_starts.len(), 1);
        assert_eq!(s.player_starts[0].location_ue, Vec3::new(-50.0, 25.0, 90.0));
        assert_eq!(s.player_starts[0].cylinder, Some([40.0, 80.0]));
        assert_eq!(
            s.stats
                .atmosphere_actors
                .get("FogVolumeConstantDensityInfo"),
            Some(&1)
        );
        let (lo, hi) = s.origin_bounds().unwrap();
        assert!(lo.x <= -50.0 && hi.z >= 300.0);
    }

    #[test]
    fn sublevels_merge_with_their_offset() {
        let json = synthetic_scene_json();
        let mut a = LevelScene::from_json(Path::new("a"), json.as_bytes()).unwrap();
        let mut b = LevelScene::from_json(Path::new("b"), json.as_bytes()).unwrap();
        b.package = "Sub".to_owned();
        let (n_meshes, n_lights) = (a.meshes.len(), a.lights.len());
        let level = a.merge_sublevel(b, Vec3::new(1000.0, 0.0, 0.0));
        assert_eq!(level, 1);
        assert_eq!(a.meshes.len(), 2 * n_meshes);
        assert_eq!(a.lights.len(), 2 * n_lights);
        assert_eq!(a.merged_levels, vec!["Sub".to_owned()]);
        let moved = &a.meshes[n_meshes];
        assert_eq!(moved.level, 1);
        assert_eq!(
            moved.ue_local_to_world.w_axis.truncate(),
            Vec3::new(1100.0, 200.0, 300.0)
        );
        assert_eq!(
            a.lights[n_lights].location_ue,
            Vec3::new(1010.0, 20.0, 30.0)
        );
        // Player starts stay those of the persistent level.
        assert_eq!(a.player_starts.len(), 1);
        let always = StreamingInfo {
            package: "P".to_owned(),
            class: "LevelStreamingAlwaysLoaded".to_owned(),
            offset: Vec3::ZERO,
        };
        assert!(always.always_loaded());
    }

    #[test]
    fn wrong_format_or_version_is_refused() {
        let json = synthetic_scene_json();
        let other = json.replacen("asamu-scene", "asamu-bsp", 1);
        assert!(matches!(
            LevelScene::from_json(Path::new("x"), other.as_bytes()),
            Err(AssetError::Format { .. })
        ));
        let newer = json.replacen("\"version\": 1", "\"version\": 2", 1);
        assert!(matches!(
            LevelScene::from_json(Path::new("x"), newer.as_bytes()),
            Err(AssetError::Format { .. })
        ));
    }

    /// Inserts two player starts before the synthetic one (slot 8, no
    /// params: enabled and primary by default).
    fn scene_with_starts(first: &str, second: &str, third_params: &str) -> LevelScene {
        let json = synthetic_scene_json();
        let extra = format!(
            r#"{{"slot": 10, "name": "PlayerStart_A", "class": "Engine.PlayerStart", "kind": "player_start",
                 "location": [1, 2, 3], "params": {first}}},
               {{"slot": 11, "name": "PlayerStart_B", "class": "Engine.PlayerStart", "kind": "player_start",
                 "location": [4, 5, 6], "params": {second}}},
               {{"slot": 8, "params": {third_params},"#
        );
        let json = json.replacen(r#"{"slot": 8,"#, &extra, 1);
        LevelScene::from_json(Path::new("x"), json.as_bytes()).unwrap()
    }

    #[test]
    fn player_start_follows_the_simulation_rule() {
        // A disabled primary start and an enabled secondary one come first:
        // the enabled primary start (the third) is chosen.
        let s = scene_with_starts(
            r#"{"bEnabled": false, "bPrimaryStart": true}"#,
            r#"{"benabled": true, "BPRIMARYSTART": false, "Other": [1, {"bEnabled": false}]}"#,
            "{}",
        );
        assert_eq!(s.player_starts.len(), 3);
        assert!(!s.player_starts[0].enabled && s.player_starts[0].primary);
        assert!(s.player_starts[1].enabled && !s.player_starts[1].primary);
        assert_eq!(s.player_start().unwrap().name, "PlayerStart_0");
        // No enabled primary start: the first enabled one.
        let s = scene_with_starts(
            r#"{"bEnabled": false}"#,
            r#"{"bPrimaryStart": false}"#,
            r#"{"bPrimaryStart": false}"#,
        );
        assert_eq!(s.player_start().unwrap().name, "PlayerStart_B");
        // None enabled: the first. Non-boolean values count as absent.
        let s = scene_with_starts(
            r#"{"bEnabled": false}"#,
            r#"{"bEnabled": false}"#,
            r#"{"bEnabled": false, "bPrimaryStart": "yes"}"#,
        );
        assert!(s.player_starts[2].primary);
        assert_eq!(s.player_start().unwrap().name, "PlayerStart_A");
        // An exact-case key wins over a case-folded duplicate.
        let s = scene_with_starts(
            r#"{"benabled": true, "bEnabled": false}"#,
            r#"{"bEnabled": false, "BENABLED": true}"#,
            "null",
        );
        assert!(!s.player_starts[0].enabled && !s.player_starts[1].enabled);
        assert_eq!(s.player_start().unwrap().name, "PlayerStart_0");
        // A params value of the wrong shape is a parse error, not a panic.
        let json =
            synthetic_scene_json().replacen(r#"{"slot": 8,"#, r#"{"slot": 8, "params": 5,"#, 1);
        assert!(LevelScene::from_json(Path::new("x"), json.as_bytes()).is_err());
    }

    #[test]
    fn light_classes_classify() {
        assert_eq!(
            UeLightKind::from_class("DominantSpotLightComponent"),
            UeLightKind::Spot
        );
        assert_eq!(
            UeLightKind::from_class("DominantPointLightComponent"),
            UeLightKind::Point
        );
        assert_eq!(
            UeLightKind::from_class("SkyLightComponent"),
            UeLightKind::Sky
        );
        assert_eq!(
            UeLightKind::from_class("SphericalHarmonicLightComponent"),
            UeLightKind::Other
        );
    }

    #[test]
    fn truncated_and_corrupted_scenes_never_panic() {
        let json = synthetic_scene_json();
        let bytes = json.as_bytes();
        for cut in (0..bytes.len()).step_by(7) {
            let _ = LevelScene::from_json(Path::new("x"), &bytes[..cut]);
        }
        for i in (0..bytes.len()).step_by(3) {
            for v in [b'"', b'9', b'{', b']', b'-', 0xff] {
                let mut b = bytes.to_vec();
                b[i] = v;
                let _ = LevelScene::from_json(Path::new("x"), &b);
            }
        }
        assert!(LevelScene::from_json(Path::new("x"), b"").is_err());
    }
}
