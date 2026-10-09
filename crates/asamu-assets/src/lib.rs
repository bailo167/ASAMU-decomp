//! Runtime asset representation for data converted locally from a user's
//! legitimate original installation. This crate never ships original assets
//! and never parses UE3 packages: it reads the user-local output of
//! `asamu-import` (`textures/`, `meshes/`, `levels/`, `materials/`) and turns
//! it into render-agnostic plans the Bevy app instantiates.
//!
//! Modules:
//!
//! - [`files`]: bounded reads and validation of paths stored in manifests
//!   (converted data is treated as untrusted input; nothing here panics).
//! - [`manifest`]: texture and mesh manifests (`asamu-import textures` /
//!   `meshes`).
//! - [`material_manifest`]: `materials/materials.json` (`asamu-import
//!   materials`), and [`materials`]: the render material model with a
//!   graceful fallback when no converted description exists.
//! - [`scene`]: the rendering view of `levels/<map>.scene.json` (static mesh
//!   components, lights, player starts, `WorldInfo`).
//! - [`transform`]: UE3 row-vector matrices → render transforms via
//!   `asamu_core::coords`.
//! - [`lighting`]: UE3 lights → physically based lights (a documented
//!   approximation).
//! - [`bsp`]: the level BSP (walls and floors built from CSG brushes) as
//!   flat-shaded render meshes.
//! - [`level`]: [`ConvertedDir`] and [`LevelPlan`], the per-level render plan
//!   with shared mesh primitives and materials.
//!
//! # Scale
//!
//! Rendering uses [`asamu_core::WorldScale::PRESENTATION_METRES`] (50 UU per
//! render unit, i.e. render units are presentation metres), the same scale as
//! the graybox app. It is a presentation convention, not a recovered fact;
//! the simulation stays in UU and never sees render coordinates.

pub mod bsp;
pub mod error;
pub mod files;
pub mod level;
pub mod lighting;
pub mod lightmaps;
pub mod manifest;
pub mod material_manifest;
pub mod materials;
pub mod scene;
pub mod transform;

pub use error::{AssetError, AssetResult};
pub use level::{ConvertedDir, Draw, LevelPlan, Manifests, PlanOptions, PlanStats, PrimitiveAsset};
pub use lighting::{LightMapping, RenderLight, RenderLightKind};
pub use materials::{BlendMode, MaterialSource, RenderMaterial, TextureBinding};
pub use scene::LevelScene;
pub use transform::RenderTransform;
