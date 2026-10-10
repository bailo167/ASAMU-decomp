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
//!   related actors that exist (counted per class);
//! - the **atmosphere** ([`AtmosphereInfo`]): `WorldInfo`'s default
//!   post-process settings and the post-process volumes (settings, priority,
//!   convex hull planes) from the actors' `params` / `volume`, plus the
//!   additive `atmosphere` block of newer scenes (height-fog and fog-volume
//!   component values, fog materials, the default post-process chain). The
//!   values stay in UE3 names and units; their meaning and the rendering
//!   approximations are documented in
//!   `docs/reverse-engineering/POST_FOG_SKY.md`;
//! - the additive **foliage** block of newer scenes ([`FoliageInfo`],
//!   `docs/reverse-engineering/WATER_FOLIAGE.md`): every instanced static
//!   mesh component (painted foliage) becomes one [`MeshInstance`] per
//!   instance, and the water surfaces and SpeedTree placements are kept for
//!   the renderer.
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
    /// The additive atmosphere block, decoded separately so that a block
    /// of an unexpected shape (another importer version) does not stop the
    /// level from loading.
    #[serde(default)]
    atmosphere: Option<serde_json::Value>,
    /// The additive foliage block (instanced meshes, water, SpeedTree),
    /// decoded separately for the same reason.
    #[serde(default)]
    foliage: Option<serde_json::Value>,
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
    /// Only the player-start flags and the post-process parameters are
    /// read; every other parameter is skipped without being stored.
    #[serde(default)]
    params: ActorParams,
    /// World-space volume geometry (only the hull planes and bounds are
    /// read).
    #[serde(default)]
    volume: Option<RawVolume>,
}

#[derive(Debug, Default, Deserialize)]
struct RawVolume {
    #[serde(default)]
    hulls: Vec<RawHull>,
    #[serde(default)]
    bounds: Option<[[f64; 3]; 2]>,
}

#[derive(Debug, Default, Deserialize)]
struct RawHull {
    #[serde(default)]
    vertices: Vec<[f64; 3]>,
    #[serde(default)]
    planes: Vec<[f64; 4]>,
}

impl RawHull {
    /// The hull's planes, each oriented so that the centroid of its
    /// vertices is on the inside (negative) side. Non-finite planes and
    /// degenerate normals are dropped; a hull without a usable vertex
    /// centroid keeps the stored orientation (outward normals, which every
    /// shipped hull uses).
    #[allow(clippy::cast_possible_truncation)]
    fn oriented_planes(&self) -> Vec<[f32; 4]> {
        let n = self.vertices.len();
        let centroid = (n > 0)
            .then(|| {
                let mut c = [0.0f64; 3];
                for v in &self.vertices {
                    for (a, x) in c.iter_mut().zip(v) {
                        *a += x;
                    }
                }
                #[allow(clippy::cast_precision_loss)]
                c.map(|x| x / n as f64)
            })
            .filter(|c| c.iter().all(|x| x.is_finite()));
        self.planes
            .iter()
            .filter(|p| p.iter().all(|x| x.is_finite()))
            .filter(|p| p[0] * p[0] + p[1] * p[1] + p[2] * p[2] > 1e-12)
            .map(|p| {
                let flip =
                    centroid.is_some_and(|c| p[0] * c[0] + p[1] * c[1] + p[2] * c[2] - p[3] > 0.0);
                let q = if flip { p.map(|x| -x) } else { *p };
                q.map(|x| x as f32)
            })
            .filter(|p| p.iter().all(|x| x.is_finite()))
            .collect()
    }
}

/// The parameters read from an actor's `params`: `bEnabled` /
/// `bPrimaryStart` (player starts, post-process volumes) and the
/// post-process values of `WorldInfo` and `PostProcessVolume` (names
/// matched without regard to ASCII case, like the world crate's parameter
/// lookup; an exact-case key wins over a case-folded duplicate; flags that
/// are not booleans count as absent).
#[derive(Debug, Default, Clone, PartialEq)]
struct ActorParams {
    enabled: Option<bool>,
    primary: Option<bool>,
    /// `WorldInfo.DefaultPostProcessSettings`.
    default_post_process: Option<serde_json::Value>,
    /// `WorldInfo.bPersistPostProcessToNextLevel`.
    persist_post_process: Option<bool>,
    /// `PostProcessVolume.Settings`.
    settings: Option<serde_json::Value>,
    /// `PostProcessVolume.Priority`.
    priority: Option<f64>,
    /// `PostProcessVolume.bOverrideWorldPostProcessChain`.
    override_world_chain: Option<bool>,
}

/// Which [`ActorParams`] field a parameter name fills.
#[derive(Clone, Copy)]
enum ParamSlot {
    Enabled,
    Primary,
    DefaultPostProcess,
    Persist,
    Settings,
    Priority,
    OverrideChain,
}

impl ParamSlot {
    const NAMES: [(&'static str, ParamSlot); 7] = [
        ("bEnabled", ParamSlot::Enabled),
        ("bPrimaryStart", ParamSlot::Primary),
        ("DefaultPostProcessSettings", ParamSlot::DefaultPostProcess),
        ("bPersistPostProcessToNextLevel", ParamSlot::Persist),
        ("Settings", ParamSlot::Settings),
        ("Priority", ParamSlot::Priority),
        ("bOverrideWorldPostProcessChain", ParamSlot::OverrideChain),
    ];

    /// The slot for `key` and whether the key matched exactly.
    fn find(key: &str) -> Option<(ParamSlot, bool)> {
        Self::NAMES
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(key))
            .map(|(n, s)| (*s, *n == key))
    }
}

impl ActorParams {
    fn store(&mut self, slot: ParamSlot, exact: bool, v: serde_json::Value) {
        fn put<T>(dst: &mut Option<T>, exact: bool, v: Option<T>) {
            if (dst.is_none() || exact)
                && let Some(v) = v
            {
                *dst = Some(v);
            }
        }
        match slot {
            ParamSlot::Enabled => put(&mut self.enabled, exact, v.as_bool()),
            ParamSlot::Primary => put(&mut self.primary, exact, v.as_bool()),
            ParamSlot::Persist => put(&mut self.persist_post_process, exact, v.as_bool()),
            ParamSlot::OverrideChain => put(&mut self.override_world_chain, exact, v.as_bool()),
            ParamSlot::Priority => put(&mut self.priority, exact, v.as_f64()),
            ParamSlot::DefaultPostProcess => {
                put(
                    &mut self.default_post_process,
                    exact,
                    v.is_object().then_some(v),
                );
            }
            ParamSlot::Settings => put(&mut self.settings, exact, v.is_object().then_some(v)),
        }
    }
}

impl<'de> Deserialize<'de> for ActorParams {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct Visitor;
        impl<'de> serde::de::Visitor<'de> for Visitor {
            type Value = ActorParams;
            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("an actor parameter map")
            }
            fn visit_unit<E: serde::de::Error>(self) -> Result<ActorParams, E> {
                Ok(ActorParams::default())
            }
            fn visit_map<A: serde::de::MapAccess<'de>>(
                self,
                mut map: A,
            ) -> Result<ActorParams, A::Error> {
                let mut out = ActorParams::default();
                while let Some(key) = map.next_key::<std::borrow::Cow<'de, str>>()? {
                    let Some((slot, exact)) = ParamSlot::find(&key) else {
                        map.next_value::<serde::de::IgnoredAny>()?;
                        continue;
                    };
                    let v: serde_json::Value = map.next_value()?;
                    out.store(slot, exact, v);
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
    /// Component class path (instanced static mesh components are drawn
    /// once per instance from the foliage block).
    #[serde(default)]
    class: String,
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

/// The importer's additive `atmosphere` block (`tools/asamu-import`
/// `levels.rs`, version 1).
#[derive(Debug, Default, Deserialize)]
struct RawAtmosphere {
    #[serde(default)]
    version: u32,
    #[serde(default)]
    height_fogs: Vec<RawFogComponent>,
    #[serde(default)]
    fog_volumes: Vec<RawFogVolume>,
    #[serde(default)]
    post_process_chain: Option<RawChain>,
}

#[derive(Debug, Default, Deserialize)]
struct RawFogComponent {
    #[serde(default)]
    slot: usize,
    #[serde(default)]
    actor: String,
    #[serde(default)]
    actor_class: String,
    #[serde(default, deserialize_with = "lenient::v3")]
    location: [f32; 3],
    #[serde(default)]
    class: String,
    #[serde(default)]
    params: serde_json::Map<String, serde_json::Value>,
}

#[derive(Debug, Default, Deserialize)]
struct RawFogVolume {
    #[serde(flatten)]
    density: RawFogComponent,
    #[serde(default)]
    material: Option<RawMaterialParams>,
    #[serde(default)]
    mesh: Option<String>,
    #[serde(default)]
    mesh_local_to_world: Option<[[f64; 4]; 4]>,
}

#[derive(Debug, Default, Deserialize)]
struct RawMaterialParams {
    #[serde(default)]
    path: String,
    #[serde(default)]
    vectors: BTreeMap<String, [f64; 4]>,
    #[serde(default)]
    scalars: BTreeMap<String, f64>,
}

#[derive(Debug, Default, Deserialize)]
struct RawChain {
    #[serde(default)]
    name: String,
    #[serde(default)]
    source: String,
    #[serde(default)]
    effects: Vec<RawEffect>,
}

#[derive(Debug, Default, Deserialize)]
struct RawEffect {
    #[serde(default)]
    name: String,
    #[serde(default)]
    class: String,
    #[serde(default)]
    params: serde_json::Map<String, serde_json::Value>,
}

/// The importer's additive `foliage` block (`tools/asamu-import`
/// `levels.rs`, version 1; `docs/reverse-engineering/WATER_FOLIAGE.md`).
/// Only what rendering needs is read; the foliage actors' bookkeeping
/// (`foliage_actors`) is skipped.
#[derive(Debug, Default, Deserialize)]
struct RawFoliage {
    #[serde(default)]
    version: u32,
    #[serde(default)]
    instanced_meshes: Vec<RawInstancedMesh>,
    #[serde(default)]
    fluid_surfaces: Vec<RawFluidSurface>,
    #[serde(default)]
    speedtrees: Vec<RawSpeedTree>,
}

#[derive(Debug, Default, Deserialize)]
struct RawInstancedMesh {
    #[serde(default)]
    slot: usize,
    #[serde(default)]
    component: String,
    #[serde(default)]
    instances: Vec<RawInstance>,
}

#[derive(Debug, Deserialize)]
struct RawInstance {
    #[serde(deserialize_with = "lenient::m4")]
    local_to_world: UeRowMatrix,
}

#[derive(Debug, Deserialize)]
struct RawFluidSurface {
    #[serde(default)]
    slot: usize,
    #[serde(default)]
    actor: String,
    #[serde(default)]
    component: String,
    #[serde(deserialize_with = "lenient::m4")]
    local_to_world: UeRowMatrix,
    #[serde(default)]
    hidden: bool,
    #[serde(default)]
    params: serde_json::Map<String, serde_json::Value>,
    #[serde(default)]
    material: Option<RawMaterialParams>,
}

#[derive(Debug, Deserialize)]
struct RawSpeedTree {
    #[serde(default)]
    slot: usize,
    #[serde(default)]
    actor: String,
    #[serde(default)]
    component: String,
    #[serde(deserialize_with = "lenient::m4")]
    local_to_world: UeRowMatrix,
    #[serde(default)]
    hidden: bool,
    #[serde(default)]
    speedtree: Option<String>,
}

// ------------------------------------------------------------ atmosphere model

/// UE3 property values in the scene's `params` encoding (numbers, booleans,
/// strings, lists and struct maps keyed by member name), looked up by name
/// without regard to ASCII case (an exact-case key wins). Numbers are read
/// as `f64` and narrowed; non-finite results count as absent.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct UeProps(pub BTreeMap<String, serde_json::Value>);

#[allow(clippy::cast_possible_truncation)]
fn finite_f32(v: &serde_json::Value) -> Option<f32> {
    v.as_f64().map(|x| x as f32).filter(|x| x.is_finite())
}

impl UeProps {
    /// Wraps a JSON object (anything else gives an empty set).
    #[must_use]
    pub fn from_value(v: &serde_json::Value) -> Self {
        match v {
            serde_json::Value::Object(m) => Self::from_map(m),
            _ => Self::default(),
        }
    }

    fn from_map(m: &serde_json::Map<String, serde_json::Value>) -> Self {
        Self(m.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
    }

    /// Parses a JSON object written in the `params` encoding (`None` for
    /// malformed text or a non-object).
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        match serde_json::from_str::<serde_json::Value>(text).ok()? {
            serde_json::Value::Object(m) => Some(Self::from_map(&m)),
            _ => None,
        }
    }

    /// Number of values.
    #[must_use]
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// No values.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// The raw value named `name`.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&serde_json::Value> {
        self.0.get(name).or_else(|| {
            self.0
                .iter()
                .find(|(k, _)| k.eq_ignore_ascii_case(name))
                .map(|(_, v)| v)
        })
    }

    /// A float (or integer) value.
    #[must_use]
    pub fn f32(&self, name: &str) -> Option<f32> {
        self.get(name).and_then(finite_f32)
    }

    /// A boolean value.
    #[must_use]
    pub fn bool(&self, name: &str) -> Option<bool> {
        self.get(name).and_then(serde_json::Value::as_bool)
    }

    /// A string, name, enumerator or object path (`None` for a null object
    /// reference).
    #[must_use]
    pub fn text(&self, name: &str) -> Option<&str> {
        self.get(name).and_then(serde_json::Value::as_str)
    }

    /// A struct value.
    #[must_use]
    pub fn sub(&self, name: &str) -> Option<UeProps> {
        match self.get(name)? {
            serde_json::Value::Object(m) => Some(Self::from_map(m)),
            _ => None,
        }
    }

    /// Members `names` of struct `name` as floats; missing members are 0
    /// (the value UE3 leaves unstored), a member of the wrong type makes
    /// the whole value absent.
    fn members<const N: usize>(&self, name: &str, names: [&str; N]) -> Option<[f32; N]> {
        let s = self.sub(name)?;
        let mut out = [0.0; N];
        for (o, n) in out.iter_mut().zip(names) {
            if let Some(v) = s.get(n) {
                *o = finite_f32(v)?;
            }
        }
        Some(out)
    }

    /// A `Vector` (`X`, `Y`, `Z`).
    #[must_use]
    pub fn vec3(&self, name: &str) -> Option<Vec3> {
        self.members(name, ["X", "Y", "Z"]).map(Vec3::from_array)
    }

    /// A `Plane` (`X`, `Y`, `Z`, `W`).
    #[must_use]
    pub fn plane(&self, name: &str) -> Option<[f32; 4]> {
        self.members(name, ["X", "Y", "Z", "W"])
    }

    /// A `Color` (bytes `R`, `G`, `B`, `A`; sRGB-encoded in UE3).
    #[must_use]
    pub fn color8(&self, name: &str) -> Option<[u8; 4]> {
        let c = self.members(name, ["R", "G", "B", "A"])?;
        let byte = |x: f32| ((0.0..=255.0).contains(&x) && x.fract() == 0.0).then_some(x);
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        Some([
            byte(c[0])? as u8,
            byte(c[1])? as u8,
            byte(c[2])? as u8,
            byte(c[3])? as u8,
        ])
    }

    /// A `LinearColor` (floats `R`, `G`, `B`, `A`).
    #[must_use]
    pub fn linear_color(&self, name: &str) -> Option<[f32; 4]> {
        self.members(name, ["R", "G", "B", "A"])
    }
}

/// A post-process volume (`Engine.PostProcessVolume` and subclasses).
#[derive(Debug, Clone, PartialEq)]
pub struct PostProcessVolumeInfo {
    /// Level of the actor (see [`MeshInstance::level`]).
    pub level: usize,
    /// Actor slot.
    pub actor_slot: usize,
    /// Actor name.
    pub actor_name: String,
    /// `Priority` (absent = 0, the class default).
    pub priority: f32,
    /// `bEnabled` (absent = true: the class default of
    /// `Engine.PostProcessVolume`. No shipped volume stores its own
    /// `bEnabled`; the importer writes the effective value).
    pub enabled: bool,
    /// `bOverrideWorldPostProcessChain`.
    pub override_world_chain: bool,
    /// `Settings` (effective values: the volume's own over the class
    /// defaults, with the `bOverride_*` flags).
    pub settings: UeProps,
    /// Convex hulls of the volume as planes `(X, Y, Z, W)` in UE3 world
    /// space (`X·p = W` on the plane; the inside is where every plane gives
    /// `X·p − W ≤ 0`, the importer's convention for brush hulls).
    pub hulls: Vec<Vec<[f32; 4]>>,
    /// World bounds of the geometry (UE3), when known.
    pub bounds: Option<(Vec3, Vec3)>,
}

/// A fog component (height fog or fog volume density) from the atmosphere
/// block.
#[derive(Debug, Clone, PartialEq)]
pub struct FogComponentInfo {
    /// Level of the actor.
    pub level: usize,
    /// Actor slot.
    pub actor_slot: usize,
    /// Actor name.
    pub actor_name: String,
    /// Actor class path.
    pub actor_class: String,
    /// Actor location (UE3, UU).
    pub location_ue: Vec3,
    /// Component class path.
    pub class: String,
    /// Effective component values.
    pub params: UeProps,
}

impl FogComponentInfo {
    /// The class name without its package (`ExponentialHeightFogComponent`).
    #[must_use]
    pub fn class_name(&self) -> &str {
        self.class.rsplit('.').next().unwrap_or(&self.class)
    }
}

/// A fog volume: its density component, fog material parameters and shape.
#[derive(Debug, Clone, PartialEq)]
pub struct FogVolumeInfo {
    /// The density component.
    pub density: FogComponentInfo,
    /// Fog material path.
    pub material: Option<String>,
    /// The material's own vector parameters (`EmissiveColor`, ...).
    pub material_vectors: BTreeMap<String, [f32; 4]>,
    /// The material's own scalar parameters.
    pub material_scalars: BTreeMap<String, f32>,
    /// Static mesh that gives the volume's shape.
    pub mesh: Option<String>,
    /// Its world matrix (UE3 space, column-vector convention).
    pub ue_mesh_to_world: Option<Mat4>,
}

/// One effect of a post-process chain.
#[derive(Debug, Clone, PartialEq)]
pub struct PostProcessEffectInfo {
    /// Effect object name.
    pub name: String,
    /// Effect class path.
    pub class: String,
    /// Effective values.
    pub params: UeProps,
}

/// The engine's default post-process chain.
#[derive(Debug, Clone, PartialEq)]
pub struct PostProcessChainInfo {
    /// Chain object path.
    pub name: String,
    /// Where the importer found the name.
    pub source: String,
    /// Effects in chain order.
    pub effects: Vec<PostProcessEffectInfo>,
}

/// Fog, sky and post-process data of a level (UE3 names and units).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AtmosphereInfo {
    /// Version of the scene's `atmosphere` block (`None`: the scene predates
    /// it, or the block did not decode; only the actor-derived fields are
    /// filled).
    pub block_version: Option<u32>,
    /// Why the scene's `atmosphere` block was ignored (it is present but has
    /// an unexpected shape).
    pub block_error: Option<String>,
    /// `WorldInfo.DefaultPostProcessSettings` of the persistent level.
    pub world_post_process: Option<UeProps>,
    /// `WorldInfo.bPersistPostProcessToNextLevel`.
    pub persist_post_process: Option<bool>,
    /// Post-process volumes in actor order (persistent level first).
    pub post_process_volumes: Vec<PostProcessVolumeInfo>,
    /// Height-fog components.
    pub height_fogs: Vec<FogComponentInfo>,
    /// Fog volumes.
    pub fog_volumes: Vec<FogVolumeInfo>,
    /// The default post-process chain.
    pub post_process_chain: Option<PostProcessChainInfo>,
}

fn fog_component(raw: RawFogComponent, level: usize) -> FogComponentInfo {
    FogComponentInfo {
        level,
        actor_slot: raw.slot,
        actor_name: raw.actor,
        actor_class: raw.actor_class,
        location_ue: if finite3(raw.location) {
            Vec3::from_array(raw.location)
        } else {
            Vec3::ZERO
        },
        class: raw.class,
        params: UeProps::from_map(&raw.params),
    }
}

#[allow(clippy::cast_possible_truncation)]
fn narrow4(v: [f64; 4]) -> Option<[f32; 4]> {
    let out = v.map(|x| x as f32);
    out.iter().all(|x| x.is_finite()).then_some(out)
}

impl AtmosphereInfo {
    fn apply_block(&mut self, raw: RawAtmosphere) {
        self.block_version = Some(raw.version);
        self.height_fogs = raw
            .height_fogs
            .into_iter()
            .map(|f| fog_component(f, 0))
            .collect();
        self.fog_volumes = raw
            .fog_volumes
            .into_iter()
            .map(|v| {
                #[allow(clippy::cast_possible_truncation)]
                let m = v
                    .mesh_local_to_world
                    .map(|m| m.map(|r| r.map(|x| x as f32)))
                    .map(|m| ue_row_matrix_to_mat4(&m))
                    .filter(Mat4::is_finite);
                let (material, vectors, scalars) = match v.material {
                    Some(mp) => (
                        Some(mp.path).filter(|p| !p.is_empty()),
                        mp.vectors
                            .into_iter()
                            .filter_map(|(k, c)| Some((k, narrow4(c)?)))
                            .collect(),
                        #[allow(clippy::cast_possible_truncation)]
                        mp.scalars
                            .into_iter()
                            .map(|(k, x)| (k, x as f32))
                            .filter(|(_, x)| x.is_finite())
                            .collect(),
                    ),
                    None => (None, BTreeMap::new(), BTreeMap::new()),
                };
                FogVolumeInfo {
                    density: fog_component(v.density, 0),
                    material,
                    material_vectors: vectors,
                    material_scalars: scalars,
                    mesh: v.mesh.filter(|m| !m.is_empty()),
                    ue_mesh_to_world: m,
                }
            })
            .collect();
        self.post_process_chain = raw.post_process_chain.map(|c| PostProcessChainInfo {
            name: c.name,
            source: c.source,
            effects: c
                .effects
                .into_iter()
                .map(|e| PostProcessEffectInfo {
                    name: e.name,
                    class: e.class,
                    params: UeProps::from_map(&e.params),
                })
                .collect(),
        });
    }

    /// The post-process volumes sorted the way UE3 consults them: highest
    /// `Priority` first; volumes of equal priority keep their registration
    /// (actor) order. (UE3 inserts each volume before the first one of
    /// strictly lower priority: `APostProcessVolume::UpdateComponentsInternal`,
    /// see `POST_FOG_SKY.md`.)
    #[must_use]
    pub fn volumes_by_priority(&self) -> Vec<&PostProcessVolumeInfo> {
        let mut v: Vec<&PostProcessVolumeInfo> = self.post_process_volumes.iter().collect();
        v.sort_by(|a, b| b.priority.total_cmp(&a.priority));
        v
    }
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
    /// For an instanced static mesh component (foliage): the index of this
    /// instance in the component's `PerInstanceSMData`. Such draws carry the
    /// component name with `#<index>` appended in [`Self::component_name`],
    /// so that per-component lookups keyed by the plain name (light maps:
    /// an instanced component's light map needs each instance's own UV
    /// bias, which those lookups do not apply) do not match them.
    pub instance: Option<u32>,
}

/// A water surface: one `FluidSurfaceComponent` (`FluidSurfaceActor`) from
/// the scene's foliage block. The surface is the rectangle of
/// [`Self::width`] along the component's local X axis and [`Self::height`]
/// along its local Y axis, centred on the component origin
/// (`UFluidSurfaceComponent::UpdateBounds`, CONFIRMED from the Mac
/// executable; WATER_FOLIAGE.md).
#[derive(Debug, Clone, PartialEq)]
pub struct WaterSurfaceInfo {
    /// Level of the actor (see [`MeshInstance::level`]).
    pub level: usize,
    /// Actor slot.
    pub actor_slot: usize,
    /// Actor name.
    pub actor_name: String,
    /// Component name.
    pub component_name: String,
    /// Component world matrix (UE3 space, column-vector convention).
    pub ue_local_to_world: Mat4,
    /// `FluidWidth` (UU, along local X).
    pub width: f32,
    /// `FluidHeight` (UU, along local Y).
    pub height: f32,
    /// `GridSpacing` (UU between simulation grid vertices).
    pub grid_spacing: Option<f32>,
    /// `FluidMaterial` path.
    pub material: Option<String>,
    /// The material's own vector parameters.
    pub material_vectors: BTreeMap<String, [f32; 4]>,
    /// The material's own scalar parameters.
    pub material_scalars: BTreeMap<String, f32>,
    /// Effective component values (UE3 names).
    pub params: UeProps,
    /// Actor `bHidden` or component `HiddenGame`.
    pub hidden: bool,
}

/// A SpeedTree placement (`SpeedTreeComponent`).
#[derive(Debug, Clone, PartialEq)]
pub struct SpeedTreeInfo {
    /// Level of the actor.
    pub level: usize,
    /// Actor slot.
    pub actor_slot: usize,
    /// Actor name.
    pub actor_name: String,
    /// Component name.
    pub component_name: String,
    /// Component world matrix (UE3 space, column-vector convention).
    pub ue_local_to_world: Mat4,
    /// The tree asset (`None`: the component names none; true of all four
    /// shipped placements, all in AG-Darkcave).
    pub speedtree: Option<String>,
    /// Actor `bHidden` or component `HiddenGame`.
    pub hidden: bool,
}

/// Water and SpeedTree data of a level (the scene's foliage block; the
/// block's instanced meshes become [`LevelScene::meshes`]).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct FoliageInfo {
    /// Version of the scene's foliage block (`None`: the scene predates it
    /// or the block did not decode).
    pub block_version: Option<u32>,
    /// Why the block was ignored (present with an unexpected shape).
    pub block_error: Option<String>,
    /// Water surfaces in actor order.
    pub water: Vec<WaterSurfaceInfo>,
    /// SpeedTree placements in actor order.
    pub speedtrees: Vec<SpeedTreeInfo>,
    /// Entries of the block left out because it holds more than the reader
    /// takes ([`MAX_INSTANCED_DRAWS`], [`MAX_FOLIAGE_PLACEMENTS`]); 0 for
    /// every scene the importer writes.
    pub over_limit: usize,
}

/// Instances taken from one scene's foliage block, over all of its
/// instanced components: the importer's own bound per map
/// (`MAX_INSTANCES_PER_MAP` in `tools/asamu-import` `levels.rs`; the shipped
/// maps hold at most 4,531). A damaged or hand-edited scene cannot make the
/// reader build more draws than this.
pub const MAX_INSTANCED_DRAWS: usize = 262_144;

/// Water surfaces, and SpeedTree placements, taken from one scene's foliage
/// block (the importer's bound on both together, `MAX_PLACEMENTS`; the
/// shipped maps hold at most two and four).
pub const MAX_FOLIAGE_PLACEMENTS: usize = 1_024;

/// True for `Engine.InstancedStaticMeshComponent` (and classes named like
/// it).
fn is_instanced_class(class: &str) -> bool {
    class
        .rsplit('.')
        .next()
        .is_some_and(|c| c.eq_ignore_ascii_case("InstancedStaticMeshComponent"))
}

/// A material path with its own vector and scalar parameters.
type MaterialMaps = (
    Option<String>,
    BTreeMap<String, [f32; 4]>,
    BTreeMap<String, f32>,
);

#[allow(clippy::cast_possible_truncation)]
fn material_maps(m: Option<RawMaterialParams>) -> MaterialMaps {
    match m {
        Some(mp) => (
            Some(mp.path).filter(|p| !p.is_empty()),
            mp.vectors
                .into_iter()
                .filter_map(|(k, c)| Some((k, narrow4(c)?)))
                .collect(),
            mp.scalars
                .into_iter()
                .map(|(k, x)| (k, x as f32))
                .filter(|(_, x)| x.is_finite())
                .collect(),
        ),
        None => (None, BTreeMap::new(), BTreeMap::new()),
    }
}

/// The decoded foliage block: instance matrices per (actor slot, component
/// name), water and SpeedTree placements.
#[derive(Debug, Default)]
struct FoliageBlock {
    info: FoliageInfo,
    instances: BTreeMap<(usize, String), Vec<Mat4>>,
}

impl FoliageBlock {
    fn decode(value: Option<serde_json::Value>) -> Self {
        Self::decode_bounded(value, MAX_INSTANCED_DRAWS, MAX_FOLIAGE_PLACEMENTS)
    }

    /// [`Self::decode`] taking at most `max_draws` instances over all
    /// components and `max_placements` water surfaces and as many SpeedTree
    /// placements; what is left out is counted in
    /// [`FoliageInfo::over_limit`].
    fn decode_bounded(
        value: Option<serde_json::Value>,
        max_draws: usize,
        max_placements: usize,
    ) -> Self {
        let mut out = Self::default();
        let raw = match value
            .filter(|v| !v.is_null())
            .map(serde_json::from_value::<RawFoliage>)
        {
            Some(Ok(raw)) => raw,
            Some(Err(e)) => {
                out.info.block_error = Some(e.to_string());
                return out;
            }
            None => return out,
        };
        out.info.block_version = Some(raw.version);
        let mut budget = max_draws;
        for im in raw.instanced_meshes {
            // A component listed twice keeps its first entry.
            let key = (im.slot, im.component);
            if out.instances.contains_key(&key) {
                out.info.over_limit = out.info.over_limit.saturating_add(im.instances.len());
                continue;
            }
            let take = im.instances.len().min(budget);
            budget -= take;
            out.info.over_limit = out
                .info
                .over_limit
                .saturating_add(im.instances.len() - take);
            let list = im
                .instances
                .iter()
                .take(take)
                .map(|i| ue_row_matrix_to_mat4(&i.local_to_world))
                .collect();
            out.instances.insert(key, list);
        }
        for f in raw.fluid_surfaces {
            let m = ue_row_matrix_to_mat4(&f.local_to_world);
            let params = UeProps::from_map(&f.params);
            let (Some(width), Some(height)) = (params.f32("FluidWidth"), params.f32("FluidHeight"))
            else {
                continue;
            };
            if !m.is_finite() || width <= 0.0 || height <= 0.0 {
                continue;
            }
            if out.info.water.len() >= max_placements {
                out.info.over_limit = out.info.over_limit.saturating_add(1);
                continue;
            }
            let (material, material_vectors, material_scalars) = material_maps(f.material);
            out.info.water.push(WaterSurfaceInfo {
                level: 0,
                actor_slot: f.slot,
                actor_name: f.actor,
                component_name: f.component,
                ue_local_to_world: m,
                width,
                height,
                grid_spacing: params.f32("GridSpacing").filter(|g| *g > 0.0),
                material: material.or_else(|| params.text("FluidMaterial").map(str::to_owned)),
                material_vectors,
                material_scalars,
                params,
                hidden: f.hidden,
            });
        }
        for t in raw.speedtrees {
            let m = ue_row_matrix_to_mat4(&t.local_to_world);
            if !m.is_finite() {
                continue;
            }
            if out.info.speedtrees.len() >= max_placements {
                out.info.over_limit = out.info.over_limit.saturating_add(1);
                continue;
            }
            out.info.speedtrees.push(SpeedTreeInfo {
                level: 0,
                actor_slot: t.slot,
                actor_name: t.actor,
                component_name: t.component,
                ue_local_to_world: m,
                speedtree: t.speedtree.filter(|p| !p.is_empty()),
                hidden: t.hidden,
            });
        }
        out
    }
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
    /// Instanced static mesh components seen (drawn once per instance).
    pub instanced_components: usize,
    /// Instances drawn from them (included in [`Self::drawn`]).
    pub instances: usize,
    /// Instanced components skipped because the scene has no instance data
    /// for them (a scene converted before the foliage block existed).
    pub instanced_without_data: usize,
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
    /// Fog, sky and post-process data.
    pub atmosphere: AtmosphereInfo,
    /// Water surfaces and SpeedTree placements.
    pub foliage: FoliageInfo,
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

/// True for `Engine.PostProcessVolume` (and classes named like it).
fn is_post_process_volume(class: &str) -> bool {
    class
        .rsplit('.')
        .next()
        .is_some_and(|c| c.eq_ignore_ascii_case("PostProcessVolume"))
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

    fn from_raw(mut raw: RawScene) -> Self {
        let FoliageBlock {
            info: foliage,
            instances: mut instanced,
        } = FoliageBlock::decode(raw.foliage.take());
        let mut stats = LevelSceneStats {
            actors: raw.actors.len(),
            ..LevelSceneStats::default()
        };
        let mut meshes = Vec::new();
        let mut lights = Vec::new();
        let mut player_starts = Vec::new();
        let mut atmosphere = AtmosphereInfo::default();
        for mut actor in raw.actors {
            if actor.kind == "world_info" && atmosphere.world_post_process.is_none() {
                atmosphere.world_post_process = actor
                    .params
                    .default_post_process
                    .take()
                    .map(|v| UeProps::from_value(&v));
                atmosphere.persist_post_process = actor.params.persist_post_process;
            }
            if is_post_process_volume(&actor.class) {
                let volume = actor.volume.take().unwrap_or_default();
                #[allow(clippy::cast_possible_truncation)]
                let bounds = volume.bounds.and_then(|[lo, hi]| {
                    let lo = lo.map(|x| x as f32);
                    let hi = hi.map(|x| x as f32);
                    (finite3(lo) && finite3(hi))
                        .then(|| (Vec3::from_array(lo), Vec3::from_array(hi)))
                });
                #[allow(clippy::cast_possible_truncation)]
                atmosphere.post_process_volumes.push(PostProcessVolumeInfo {
                    level: 0,
                    actor_slot: actor.slot,
                    actor_name: actor.name.clone(),
                    priority: actor
                        .params
                        .priority
                        .map(|p| p as f32)
                        .filter(|p| p.is_finite())
                        .unwrap_or(0.0),
                    enabled: actor.params.enabled.unwrap_or(true),
                    override_world_chain: actor.params.override_world_chain.unwrap_or(false),
                    settings: actor
                        .params
                        .settings
                        .take()
                        .map(|v| UeProps::from_value(&v))
                        .unwrap_or_default(),
                    hulls: volume
                        .hulls
                        .iter()
                        .map(RawHull::oriented_planes)
                        .filter(|h| !h.is_empty())
                        .collect(),
                    bounds,
                });
            }
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
                // Each instance list is drawn by one component only: a
                // scene that repeats a component cannot multiply the draws.
                let list = instanced.remove(&(actor.slot, comp.name.clone()));
                if is_instanced_class(&comp.class) || list.is_some() {
                    stats.instanced_components += 1;
                    let Some(list) = list else {
                        stats.instanced_without_data += 1;
                        continue;
                    };
                    for (i, m) in list.iter().enumerate() {
                        let Ok(index) = u32::try_from(i) else {
                            break;
                        };
                        if !m.is_finite() {
                            stats.bad_transform += 1;
                            continue;
                        }
                        stats.drawn += 1;
                        stats.instances += 1;
                        meshes.push(MeshInstance {
                            level: 0,
                            actor_slot: actor.slot,
                            actor_name: actor.name.clone(),
                            actor_class: actor.class.clone(),
                            actor_kind: actor.kind.clone(),
                            component_name: format!("{}#{index}", comp.name),
                            mesh: mesh.clone(),
                            material_overrides: comp.materials.clone(),
                            ue_local_to_world: *m,
                            is_static: actor.is_static,
                            matinee_driven,
                            blocks: actor.block_actors && comp.block_actors,
                            instance: Some(index),
                        });
                    }
                    continue;
                }
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
                    instance: None,
                });
            }
        }
        match raw
            .atmosphere
            .filter(|v| !v.is_null())
            .map(serde_json::from_value::<RawAtmosphere>)
        {
            Some(Ok(block)) => atmosphere.apply_block(block),
            Some(Err(e)) => atmosphere.block_error = Some(e.to_string()),
            None => {}
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
            atmosphere,
            foliage,
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
        // Atmosphere: the persistent level's WorldInfo settings and chain
        // stay; the sub-level's volumes and fog components join (their
        // geometry moved by the streaming offset).
        let shift3 = |v: Vec3| v + offset;
        for mut v in sub.atmosphere.post_process_volumes {
            v.level = level;
            for hull in &mut v.hulls {
                for p in hull.iter_mut() {
                    // n·(x − o) = w  ⇔  n·x = w + n·o
                    p[3] += p[0] * offset.x + p[1] * offset.y + p[2] * offset.z;
                }
            }
            v.bounds = v.bounds.map(|(lo, hi)| (shift3(lo), shift3(hi)));
            self.atmosphere.post_process_volumes.push(v);
        }
        for mut f in sub.atmosphere.height_fogs {
            f.level = level;
            f.location_ue = shift3(f.location_ue);
            self.atmosphere.height_fogs.push(f);
        }
        for mut f in sub.atmosphere.fog_volumes {
            f.density.level = level;
            f.density.location_ue = shift3(f.density.location_ue);
            f.ue_mesh_to_world = f.ue_mesh_to_world.map(|m| shift * m);
            self.atmosphere.fog_volumes.push(f);
        }
        for mut w in sub.foliage.water {
            w.level = level;
            w.ue_local_to_world = shift * w.ue_local_to_world;
            self.foliage.water.push(w);
        }
        for mut t in sub.foliage.speedtrees {
            t.level = level;
            t.ue_local_to_world = shift * t.ue_local_to_world;
            self.foliage.speedtrees.push(t);
        }
        self.foliage.over_limit = self
            .foliage
            .over_limit
            .saturating_add(sub.foliage.over_limit);
        self.stats.instanced_components += sub.stats.instanced_components;
        self.stats.instances += sub.stats.instances;
        self.stats.instanced_without_data += sub.stats.instanced_without_data;
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

    /// A synthetic scene with post-process actors and an `atmosphere` block
    /// (hand-written; the values are made up, not game data).
    fn atmosphere_scene_json() -> String {
        let ident = "[[1,0,0,0],[0,1,0,0],[0,0,1,0],[0,0,0,1]]";
        format!(
            r#"{{
  "format": "asamu-scene", "version": 1, "package": "AtmoMap",
  "actors": [
    {{"slot": 0, "name": "WorldInfo_0", "class": "Engine.WorldInfo", "kind": "world_info", "components": [],
      "params": {{"DefaultPostProcessSettings": {{"Bloom_Scale": 0.8, "bOverride_Bloom_Scale": true,
                   "Scene_HighLights": {{"X": 1.0, "Y": 0.5}}, "ColorGrading_LookupTable": "Pkg.lut.LUT_A",
                   "Bloom_Tint": {{"R": 255, "G": 128, "B": 0, "A": 0}}}},
                 "bPersistPostProcessToNextLevel": false, "Title": "x"}}}},
    {{"slot": 1, "name": "PostProcessVolume_0", "class": "Engine.PostProcessVolume", "kind": "volume",
      "components": [{{"name": "BC", "kind": "brush", "local_to_world": {ident}}}],
      "params": {{"bEnabled": true, "Settings": {{"Bloom_Scale": 0.3}}, "Brush": "x"}},
      "volume": {{"hulls": [{{"vertices": [[-10,-10,-10],[10,10,10]],
                               "planes": [[1,0,0,10],[-1,0,0,-10],[0,1,0,10],[0,-1,0,10],[0,0,1,10],[0,0,-1,10],[0,0,0,1]]}}],
                  "polys": null, "bounds": [[-10,-10,-10],[10,10,10]]}}}},
    {{"slot": 2, "name": "PostProcessVolume_1", "class": "Engine.PostProcessVolume", "kind": "volume",
      "params": {{"bEnabled": false, "Priority": 2.5, "bOverrideWorldPostProcessChain": true, "Settings": 7}},
      "volume": null}},
    {{"slot": 3, "name": "ExponentialHeightFog_0", "class": "Engine.ExponentialHeightFog", "kind": "other", "hidden": true,
      "components": [{{"name": "F", "kind": "other", "local_to_world": {ident}}}]}}
  ],
  "atmosphere": {{
    "version": 1,
    "height_fogs": [{{"slot": 3, "actor": "ExponentialHeightFog_0", "actor_class": "Engine.ExponentialHeightFog",
                      "location": [1, 2, 3], "component": "F", "class": "Engine.ExponentialHeightFogComponent",
                      "params": {{"FogDensity": 0.5, "FogHeight": -100.0, "bEnabled": true,
                                  "OppositeLightColor": {{"R": 10, "G": 20, "B": 30, "A": 255}}}}}}],
    "fog_volumes": [{{"slot": 4, "actor": "FogVolumeConstantDensityInfo_0", "actor_class": "Engine.FogVolumeConstantDensityInfo",
                      "location": [0, 0, 0], "component": "D", "class": "Engine.FogVolumeConstantDensityComponent",
                      "params": {{"Density": 0.001, "HalfspacePlane": {{"X": 0, "Y": 0, "Z": 1, "W": -5}}}},
                      "material": {{"path": "Map.FogMI", "class": "Engine.MaterialInstanceConstant", "parent": null,
                                    "vectors": {{"EmissiveColor": [0.1, 0.2, 0.3, 1.0]}}, "scalars": {{"S": 2.0}}}},
                      "mesh": "EngineMeshes.Cube", "mesh_local_to_world": [[2,0,0,0],[0,2,0,0],[0,0,2,0],[5,6,7,1]]}}],
    "post_process_chain": {{"name": "FX.Chain", "source": "DefaultEngine.ini",
                            "effects": [{{"name": "Uber", "class": "Engine.UberPostProcessEffect",
                                          "params": {{"TonemapperType": "Tonemapper_Customizable", "bShowInGame": true}}}}]}},
    "warnings": []
  }}
}}"#
        )
    }

    #[test]
    fn atmosphere_parses_from_actors_and_block() {
        let json = atmosphere_scene_json();
        let s = LevelScene::from_json(Path::new("AtmoMap.scene.json"), json.as_bytes()).unwrap();
        let a = &s.atmosphere;
        assert_eq!(a.block_version, Some(1));
        let w = a.world_post_process.as_ref().unwrap();
        assert_eq!(w.f32("bloom_scale"), Some(0.8));
        assert_eq!(w.bool("bOverride_Bloom_Scale"), Some(true));
        // Missing struct members are 0 (unstored UE3 values).
        assert_eq!(w.vec3("Scene_HighLights"), Some(Vec3::new(1.0, 0.5, 0.0)));
        assert_eq!(w.color8("Bloom_Tint"), Some([255, 128, 0, 0]));
        assert_eq!(w.text("ColorGrading_LookupTable"), Some("Pkg.lut.LUT_A"));
        assert_eq!(a.persist_post_process, Some(false));
        assert_eq!(a.post_process_volumes.len(), 2);
        let v0 = &a.post_process_volumes[0];
        assert!(v0.enabled && !v0.override_world_chain);
        assert_eq!(v0.priority, 0.0);
        assert_eq!(v0.settings.f32("Bloom_Scale"), Some(0.3));
        // One hull; the zero-normal plane is dropped and the plane whose
        // normal points inward (relative to the centroid) is flipped.
        assert_eq!(v0.hulls.len(), 1);
        assert_eq!(v0.hulls[0].len(), 6);
        assert_eq!(v0.hulls[0][1], [1.0, 0.0, 0.0, 10.0]);
        assert_eq!(v0.bounds, Some((Vec3::splat(-10.0), Vec3::splat(10.0))));
        let v1 = &a.post_process_volumes[1];
        assert!(!v1.enabled && v1.override_world_chain);
        assert_eq!(v1.priority, 2.5);
        assert!(v1.settings.is_empty(), "a non-struct Settings is ignored");
        assert!(v1.hulls.is_empty() && v1.bounds.is_none());
        let order: Vec<&str> = a
            .volumes_by_priority()
            .iter()
            .map(|v| v.actor_name.as_str())
            .collect();
        assert_eq!(order, ["PostProcessVolume_1", "PostProcessVolume_0"]);
        // Block contents.
        assert_eq!(a.height_fogs.len(), 1);
        let f = &a.height_fogs[0];
        assert_eq!(f.class_name(), "ExponentialHeightFogComponent");
        assert_eq!(f.location_ue, Vec3::new(1.0, 2.0, 3.0));
        assert_eq!(f.params.f32("FogDensity"), Some(0.5));
        assert_eq!(
            f.params.color8("OppositeLightColor"),
            Some([10, 20, 30, 255])
        );
        let fv = &a.fog_volumes[0];
        assert_eq!(fv.material.as_deref(), Some("Map.FogMI"));
        assert_eq!(fv.material_vectors["EmissiveColor"], [0.1, 0.2, 0.3, 1.0]);
        assert_eq!(fv.material_scalars["S"], 2.0);
        assert_eq!(
            fv.density.params.plane("HalfspacePlane"),
            Some([0.0, 0.0, 1.0, -5.0])
        );
        let m = fv.ue_mesh_to_world.unwrap();
        assert_eq!(m.transform_point3(Vec3::ONE), Vec3::new(7.0, 8.0, 9.0));
        let chain = a.post_process_chain.as_ref().unwrap();
        assert_eq!(chain.name, "FX.Chain");
        assert_eq!(
            chain.effects[0].params.text("TonemapperType"),
            Some("Tonemapper_Customizable")
        );
        // Player starts and the rest of the scene are unaffected.
        assert!(s.player_starts.is_empty());
        assert_eq!(
            s.stats.atmosphere_actors.get("ExponentialHeightFog"),
            Some(&1)
        );
    }

    #[test]
    fn scenes_without_the_block_still_read_post_process_actors() {
        let json = atmosphere_scene_json();
        let cut = json.find("\"atmosphere\"").unwrap();
        // Drop the block (and the comma before it).
        let head = json[..cut].trim_end().trim_end_matches(',');
        let old = format!("{head}\n}}");
        let s = LevelScene::from_json(Path::new("x"), old.as_bytes()).unwrap();
        assert_eq!(s.atmosphere.block_version, None);
        assert!(s.atmosphere.world_post_process.is_some());
        assert_eq!(s.atmosphere.post_process_volumes.len(), 2);
        assert!(s.atmosphere.height_fogs.is_empty());
        assert!(s.atmosphere.post_process_chain.is_none());
        // The original synthetic scene: no post-process data at all.
        let plain = synthetic_scene_json();
        let s = LevelScene::from_json(Path::new("x"), plain.as_bytes()).unwrap();
        assert_eq!(s.atmosphere, AtmosphereInfo::default());
    }

    /// Absent volume parameters take the class defaults of
    /// `Engine.PostProcessVolume`: enabled, priority 0, the world chain
    /// kept. (No shipped volume stores its own `bEnabled`.)
    #[test]
    fn post_process_volumes_default_to_the_class_defaults() {
        let json = r#"{"format": "asamu-scene", "version": 1, "package": "P",
            "actors": [{"slot": 0, "name": "PostProcessVolume_0", "class": "Engine.PostProcessVolume",
                        "kind": "volume", "params": {"Settings": {"Bloom_Scale": 0.5}}},
                       {"slot": 1, "name": "PostProcessVolume_1", "class": "Engine.PostProcessVolume",
                        "kind": "volume", "params": {"bEnabled": "yes", "Priority": "high"}},
                       {"slot": 2, "name": "PostProcessVolume_2", "class": "Engine.PostProcessVolume",
                        "kind": "volume"}]}"#;
        let s = LevelScene::from_json(Path::new("x"), json.as_bytes()).unwrap();
        let v = &s.atmosphere.post_process_volumes;
        assert_eq!(v.len(), 3);
        for p in v {
            assert!(p.enabled, "{}", p.actor_name);
            assert_eq!(p.priority, 0.0);
            assert!(!p.override_world_chain);
        }
        assert_eq!(v[0].settings.f32("Bloom_Scale"), Some(0.5));
        assert!(v[2].settings.is_empty() && v[2].hulls.is_empty());
    }

    #[test]
    fn ue_props_lookups_are_lenient_and_typed() {
        let p = UeProps::from_value(&serde_json::json!({
            "A": 1, "a": 2, "b": "x", "C": {"R": 1.5, "G": 2, "B": 3, "A": 4},
            "Bad": {"X": "no"}, "Big": 1e300, "Byte": {"R": 256, "G": 0, "B": 0, "A": 0}
        }));
        assert_eq!(p.f32("A"), Some(1.0), "the exact-case key wins");
        assert_eq!(p.f32("B"), None);
        assert_eq!(p.text("B"), Some("x"));
        assert_eq!(p.linear_color("c"), Some([1.5, 2.0, 3.0, 4.0]));
        assert_eq!(p.color8("C"), None, "1.5 is not a byte");
        assert_eq!(p.color8("Byte"), None, "256 is not a byte");
        assert_eq!(p.vec3("Bad"), None);
        assert_eq!(p.f32("Big"), None, "overflows f32");
        assert_eq!(p.sub("Missing"), None);
        assert!(UeProps::from_value(&serde_json::json!([1, 2])).is_empty());
        assert_eq!(
            UeProps::parse(r#"{"Bloom_Scale": 0.5}"#).and_then(|p| p.f32("bloom_scale")),
            Some(0.5)
        );
        assert!(UeProps::parse("[1]").is_none());
        assert!(UeProps::parse("{").is_none());
    }

    #[test]
    fn sublevel_atmosphere_merges_with_the_offset() {
        let json = atmosphere_scene_json();
        let mut a = LevelScene::from_json(Path::new("a"), json.as_bytes()).unwrap();
        let mut b = LevelScene::from_json(Path::new("b"), json.as_bytes()).unwrap();
        b.package = "Sub".to_owned();
        b.atmosphere.world_post_process = None;
        let level = a.merge_sublevel(b, Vec3::new(100.0, 0.0, 0.0));
        let at = &a.atmosphere;
        assert!(
            at.world_post_process.is_some(),
            "the persistent level's stays"
        );
        assert_eq!(at.post_process_volumes.len(), 4);
        let moved = &at.post_process_volumes[2];
        assert_eq!(moved.level, level);
        // x ≤ 10 moved by +100 → x ≤ 110.
        assert_eq!(moved.hulls[0][1], [1.0, 0.0, 0.0, 110.0]);
        assert_eq!(moved.bounds.unwrap().1, Vec3::new(110.0, 10.0, 10.0));
        assert_eq!(at.height_fogs[1].location_ue, Vec3::new(101.0, 2.0, 3.0));
        let m = at.fog_volumes[1].ue_mesh_to_world.unwrap();
        assert_eq!(m.transform_point3(Vec3::ZERO), Vec3::new(105.0, 6.0, 7.0));
    }

    #[test]
    fn hostile_atmosphere_blocks_never_panic() {
        let json = atmosphere_scene_json();
        let bytes = json.as_bytes();
        let start = json.find("\"atmosphere\"").unwrap();
        for cut in (start..bytes.len()).step_by(5) {
            let _ = LevelScene::from_json(Path::new("x"), &bytes[..cut]);
        }
        for i in (0..bytes.len()).step_by(4) {
            for v in *b"\"9{]-n" {
                let mut b = bytes.to_vec();
                b[i] = v;
                let _ = LevelScene::from_json(Path::new("x"), &b);
            }
        }
        // A block of the wrong shape is ignored (with the reason); the
        // level and its actor-derived post-process data still load.
        let bad = json.replacen("\"height_fogs\": [", "\"height_fogs\": [7, ", 1);
        let s = LevelScene::from_json(Path::new("x"), bad.as_bytes()).unwrap();
        assert_eq!(s.atmosphere.block_version, None);
        assert!(s.atmosphere.block_error.is_some());
        assert!(s.atmosphere.height_fogs.is_empty() && s.atmosphere.fog_volumes.is_empty());
        assert_eq!(s.atmosphere.post_process_volumes.len(), 2);
        assert!(s.atmosphere.world_post_process.is_some());
        let not_object = json.replacen("\"atmosphere\": {", "\"atmosphere\": \"?\", \"x\": {", 1);
        let s = LevelScene::from_json(Path::new("x"), not_object.as_bytes()).unwrap();
        assert!(s.atmosphere.block_error.is_some());
        let null = format!(
            "{}\"atmosphere\": null}}",
            &json[..json.find("\"atmosphere\"").unwrap()]
        );
        let s = LevelScene::from_json(Path::new("x"), null.as_bytes()).unwrap();
        assert_eq!(s.atmosphere.block_error, None);
        assert_eq!(s.atmosphere.block_version, None);
        let inf = json.replacen("[5,6,7,1]", "[5e39,6,7,1]", 1);
        let s = LevelScene::from_json(Path::new("x"), inf.as_bytes()).unwrap();
        assert!(s.atmosphere.fog_volumes[0].ue_mesh_to_world.is_none());
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

    /// A synthetic scene with a foliage actor (one instanced component of
    /// two instances), a water surface and a SpeedTree placement, in the
    /// importer's foliage-block format (hand-written; no game data).
    fn foliage_scene_json(block: &str) -> String {
        let ident = "[[1,0,0,0],[0,1,0,0],[0,0,1,0],[0,0,0,1]]";
        format!(
            r#"{{
  "format": "asamu-scene", "version": 1, "package": "FoliageMap",
  "actors": [
    {{"slot": 1, "name": "InstancedFoliageActor_0", "class": "Engine.InstancedFoliageActor", "kind": "other",
      "is_static": true,
      "components": [{{"name": "ISMC_0", "class": "Engine.InstancedStaticMeshComponent", "kind": "static_mesh",
                       "local_to_world": {ident}, "static_mesh": "Pkg.Meshes.Rock", "materials": [null]}},
                     {{"name": "ISMC_1", "class": "Engine.InstancedStaticMeshComponent", "kind": "static_mesh",
                       "local_to_world": {ident}, "static_mesh": "Pkg.Meshes.Pebble"}}]}},
    {{"slot": 2, "name": "FluidSurfaceActor_0", "class": "Engine.FluidSurfaceActor", "kind": "other",
      "components": [{{"name": "Fluid", "class": "Engine.FluidSurfaceComponent", "kind": "other_primitive",
                       "local_to_world": [[1,0,0,0],[0,1,0,0],[0,0,1,0],[10,20,30,1]]}}]}}
  ]{block}
}}"#
        )
    }

    const FOLIAGE_BLOCK: &str = r#",
  "atmosphere": null,
  "foliage": {"version": 1,
    "instanced_meshes": [{"slot": 1, "actor": "InstancedFoliageActor_0", "component": "ISMC_0",
      "instances": [{"local_to_world": [[1,0,0,0],[0,1,0,0],[0,0,1,0],[100,0,0,1]], "lightmap_uv_bias": [0,0], "shadowmap_uv_bias": [0,0]},
                    {"local_to_world": [[0,2,0,0],[-2,0,0,0],[0,0,2,0],[0,50,0,1]], "lightmap_uv_bias": [0.5,0], "shadowmap_uv_bias": [0,0]},
                    {"local_to_world": [[1e39,0,0,0],[0,1,0,0],[0,0,1,0],[0,0,0,1]]}]}],
    "foliage_actors": [{"slot": 1, "actor": "InstancedFoliageActor_0", "meshes": []}],
    "fluid_surfaces": [{"slot": 2, "actor": "FluidSurfaceActor_0", "component": "Fluid",
      "local_to_world": [[1,0,0,0],[0,1,0,0],[0,0,1,0],[10,20,30,1]], "hidden": false,
      "params": {"FluidWidth": 2000.0, "FluidHeight": 1000.0, "GridSpacing": 10.0, "FluidMaterial": "Pkg.M_Water"},
      "material": {"path": "Pkg.M_Water", "vectors": {"Tint": [0.1, 0.2, 0.3, 1.0]}, "scalars": {"Opacity": 0.5}}},
                       {"slot": 3, "actor": "Broken", "component": "F", "local_to_world": [[1,0,0,0],[0,1,0,0],[0,0,1,0],[0,0,0,1]],
      "params": {"FluidWidth": -5.0, "FluidHeight": 1.0}}],
    "speedtrees": [{"slot": 4, "actor": "SpeedTreeActor_0", "component": "ST",
      "local_to_world": [[1,0,0,0],[0,1,0,0],[0,0,1,0],[7,8,9,1]], "hidden": false, "speedtree": null}],
    "warnings": []}"#;

    #[test]
    fn instanced_components_draw_once_per_instance() {
        let json = foliage_scene_json(FOLIAGE_BLOCK);
        let s = LevelScene::from_json(Path::new("x"), json.as_bytes()).unwrap();
        assert_eq!(s.foliage.block_version, Some(1));
        assert_eq!(s.foliage.block_error, None);
        // ISMC_0: two finite instances drawn (the third is not finite);
        // ISMC_1 has no instance data and is not drawn at its component
        // transform.
        assert_eq!(s.stats.instanced_components, 2);
        assert_eq!(s.stats.instances, 2);
        assert_eq!(s.stats.instanced_without_data, 1);
        assert_eq!(s.stats.bad_transform, 1);
        assert_eq!(s.meshes.len(), 2);
        assert_eq!(s.stats.drawn, 2);
        let a = &s.meshes[0];
        assert_eq!(a.component_name, "ISMC_0#0");
        assert_eq!(a.instance, Some(0));
        assert_eq!(a.mesh, "Pkg.Meshes.Rock");
        assert_eq!(
            a.ue_local_to_world.w_axis.truncate(),
            Vec3::new(100.0, 0.0, 0.0)
        );
        let b = &s.meshes[1];
        assert_eq!(b.component_name, "ISMC_0#1");
        assert_eq!(b.instance, Some(1));
        // Row-vector rows become columns: local X maps to +2 Y.
        assert_eq!(
            b.ue_local_to_world.transform_vector3(Vec3::X),
            Vec3::new(0.0, 2.0, 0.0)
        );
        assert_eq!(
            b.ue_local_to_world.w_axis.truncate(),
            Vec3::new(0.0, 50.0, 0.0)
        );
        assert!(b.is_static);
    }

    #[test]
    fn water_and_speedtree_placements_parse() {
        let json = foliage_scene_json(FOLIAGE_BLOCK);
        let s = LevelScene::from_json(Path::new("x"), json.as_bytes()).unwrap();
        // The surface with a negative width is dropped.
        assert_eq!(s.foliage.water.len(), 1);
        let w = &s.foliage.water[0];
        assert_eq!(w.actor_name, "FluidSurfaceActor_0");
        assert_eq!((w.width, w.height), (2000.0, 1000.0));
        assert_eq!(w.grid_spacing, Some(10.0));
        assert_eq!(w.material.as_deref(), Some("Pkg.M_Water"));
        assert_eq!(w.material_vectors["Tint"], [0.1, 0.2, 0.3, 1.0]);
        assert_eq!(w.material_scalars["Opacity"], 0.5);
        assert_eq!(
            w.ue_local_to_world.w_axis.truncate(),
            Vec3::new(10.0, 20.0, 30.0)
        );
        assert_eq!(s.foliage.speedtrees.len(), 1);
        assert_eq!(s.foliage.speedtrees[0].speedtree, None);
        // A sub-level's water and trees move with the streaming offset.
        let mut host =
            LevelScene::from_json(Path::new("x"), foliage_scene_json("").as_bytes()).unwrap();
        assert!(host.foliage.water.is_empty());
        let level = host.merge_sublevel(s, Vec3::new(0.0, 0.0, 1000.0));
        assert_eq!(host.foliage.water[0].level, level);
        assert_eq!(
            host.foliage.water[0].ue_local_to_world.w_axis.truncate(),
            Vec3::new(10.0, 20.0, 1030.0)
        );
        assert_eq!(host.foliage.speedtrees[0].level, level);
        assert_eq!(host.stats.instances, 2);
        assert_eq!(host.stats.instanced_components, 4);
        assert_eq!(host.stats.instanced_without_data, 3);
    }

    #[test]
    fn scenes_without_or_with_a_broken_foliage_block_still_load() {
        let s = LevelScene::from_json(Path::new("x"), foliage_scene_json("").as_bytes()).unwrap();
        assert_eq!(s.foliage.block_version, None);
        assert_eq!(s.foliage.block_error, None);
        assert!(s.meshes.is_empty(), "no instance data: nothing drawn");
        assert_eq!(s.stats.instanced_without_data, 2);
        let broken = foliage_scene_json(r#", "foliage": {"instanced_meshes": 7}"#);
        let s = LevelScene::from_json(Path::new("x"), broken.as_bytes()).unwrap();
        assert!(s.foliage.block_error.is_some());
        assert!(s.foliage.water.is_empty());
        let null = foliage_scene_json(r#", "foliage": null"#);
        let s = LevelScene::from_json(Path::new("x"), null.as_bytes()).unwrap();
        assert_eq!(s.foliage.block_error, None);
        // Fuzz-style: truncations and byte flips never panic.
        let json = foliage_scene_json(FOLIAGE_BLOCK);
        let bytes = json.as_bytes();
        for cut in (0..bytes.len()).step_by(11) {
            let _ = LevelScene::from_json(Path::new("x"), &bytes[..cut]);
        }
        for i in (0..bytes.len()).step_by(5) {
            for v in [b'"', b'9', b'{', b']', 0xff] {
                let mut b = bytes.to_vec();
                b[i] = v;
                let _ = LevelScene::from_json(Path::new("x"), &b);
            }
        }
    }

    #[test]
    fn foliage_block_bounds_hold_against_damaged_scenes() {
        let ident = "[[1,0,0,0],[0,1,0,0],[0,0,1,0],[0,0,0,1]]";
        let instances = |n: usize| {
            (0..n)
                .map(|_| format!(r#"{{"local_to_world": {ident}}}"#))
                .collect::<Vec<_>>()
                .join(",")
        };
        let water = |slot: usize| {
            format!(
                r#"{{"slot": {slot}, "actor": "W", "component": "F", "local_to_world": {ident},
                     "params": {{"FluidWidth": 10.0, "FluidHeight": 10.0}}}}"#
            )
        };
        let tree = |slot: usize| {
            format!(
                r#"{{"slot": {slot}, "actor": "T", "component": "S", "local_to_world": {ident}}}"#
            )
        };
        let block: serde_json::Value = serde_json::from_str(&format!(
            r#"{{"version": 1,
                 "instanced_meshes": [
                   {{"slot": 1, "component": "A", "instances": [{}]}},
                   {{"slot": 1, "component": "A", "instances": [{}]}},
                   {{"slot": 1, "component": "B", "instances": [{}]}},
                   {{"slot": 2, "component": "A", "instances": [{}]}}],
                 "fluid_surfaces": [{}, {}, {}],
                 "speedtrees": [{}, {}, {}]}}"#,
            instances(3),
            instances(50),
            instances(4),
            instances(2),
            water(1),
            water(2),
            water(3),
            tree(1),
            tree(2),
            tree(3),
        ))
        .unwrap();
        // Five draws in all: A takes 3, the repeated A is ignored, B gets the
        // remaining 2 of its 4, the last component none.
        let b = FoliageBlock::decode_bounded(Some(block.clone()), 5, 2);
        assert_eq!(b.instances[&(1, "A".to_owned())].len(), 3);
        assert_eq!(b.instances[&(1, "B".to_owned())].len(), 2);
        assert!(b.instances[&(2, "A".to_owned())].is_empty());
        assert_eq!(b.info.water.len(), 2);
        assert_eq!(b.info.speedtrees.len(), 2);
        // 50 (repeat) + 2 (B) + 2 (last) + 1 water + 1 tree.
        assert_eq!(b.info.over_limit, 56);
        // The reader's own bounds take all of it.
        let all = FoliageBlock::decode(Some(block));
        assert_eq!(all.instances.values().map(Vec::len).sum::<usize>(), 9);
        assert_eq!(all.info.over_limit, 50, "only the repeated component");
        assert_eq!((all.info.water.len(), all.info.speedtrees.len()), (3, 3));
        // The bounds hold every shipped map (4,531 instances; 2 and 4
        // placements).
        const { assert!(MAX_INSTANCED_DRAWS >= 4_531 && MAX_FOLIAGE_PLACEMENTS >= 4) };
    }

    #[test]
    fn a_repeated_instanced_component_is_drawn_once() {
        // Two actors claim slot 1 with the same component name: the instance
        // list is used by the first only, the second counts as a component
        // without data (never a second copy of every instance).
        let ident = "[[1,0,0,0],[0,1,0,0],[0,0,1,0],[0,0,0,1]]";
        let actor = format!(
            r#"{{"slot": 1, "name": "InstancedFoliageActor_0", "class": "Engine.InstancedFoliageActor",
                 "kind": "other", "is_static": true,
                 "components": [{{"name": "ISMC_0", "class": "Engine.InstancedStaticMeshComponent",
                                  "kind": "static_mesh", "local_to_world": {ident},
                                  "static_mesh": "Pkg.Meshes.Rock"}}]}}"#
        );
        let json = format!(
            r#"{{"format": "asamu-scene", "version": 1, "package": "M",
                 "actors": [{actor}, {actor}, {actor}],
                 "foliage": {{"version": 1, "instanced_meshes": [{{"slot": 1, "component": "ISMC_0",
                     "instances": [{{"local_to_world": {ident}}}, {{"local_to_world": {ident}}}]}}]}}}}"#
        );
        let s = LevelScene::from_json(Path::new("x"), json.as_bytes()).unwrap();
        assert_eq!(s.meshes.len(), 2);
        assert_eq!(s.stats.instances, 2);
        assert_eq!(s.stats.instanced_components, 3);
        assert_eq!(s.stats.instanced_without_data, 2);
        assert_eq!(s.foliage.over_limit, 0);
        // Instance draws keep the component's index and never match the
        // plain component name (per-component lookups such as light maps).
        assert!(s.meshes.iter().all(|m| m.component_name != "ISMC_0"));
        assert_eq!(
            s.meshes.iter().map(|m| m.instance).collect::<Vec<_>>(),
            [Some(0), Some(1)]
        );
    }
}
