//! Defensive reader for the Unreal Engine 3 package generation used by
//! *A Story About My Uncle* (package file version 868, licensee version 0).
//!
//! Every serialized offset, size and count is treated as hostile input: reads
//! are bounds-checked, arithmetic is checked, counts are validated against the
//! remaining bytes before allocating, and malformed data yields a
//! [`Ue3Error`] instead of a panic.
//!
//! ```no_run
//! # fn main() -> Result<(), asamu_ue3::Ue3Error> {
//! let pkg = asamu_ue3::Package::open("Core.u")?;
//! for i in 0..pkg.exports.len() {
//!     println!("{} ({})", pkg.export_path(i)?, pkg.export_class_name(i)?);
//! }
//! # Ok(())
//! # }
//! ```
//!
//! Module map:
//! - [`reader`]: bounds-checked little-endian cursor.
//! - [`summary`]: `FPackageFileSummary` (v868) and package flags.
//! - [`compression`]: compressed chunk headers, block tables, stream rebuild.
//! - [`tables`]: name / import / export / depends tables.
//! - [`package`]: [`Package`], cross-checks and object resolution.
//! - [`lzo`]: LZO1X block decompressor.
//! - [`object`]: export payload prelude (state frame, component template,
//!   NetIndex) and generic object decoding.
//! - [`property`]: tagged properties and property values.
//! - [`script`]: script object payloads (Class, State, Function, ScriptStruct,
//!   Enum, Const, TextBuffer, `*Property`).
//! - [`schema`]: property/struct definitions used to decode values.
//! - [`model`]: cross-package class model and inherited class defaults.
//! - [`flags`]: object/class/function/property/struct/state flag names.
//! - [`coverage`]: exact-consumption coverage over real packages.
//! - [`kismet`]: Kismet (sequence) graphs of map packages.
//! - [`bodysetup`]: `RB_BodySetup` simple collision shapes (convex hulls, boxes,
//!   spheres, capsules).

pub mod anim;
pub mod bodysetup;
pub mod bsp;
pub mod bulkdata;
pub mod bytecode;
pub mod compression;
pub mod coverage;
pub mod decal;
pub mod error;
pub mod flags;
pub mod issue;
pub mod kismet;
pub mod level;
pub mod lightmap;
pub mod lzo;
pub mod material;
pub mod matinee;
pub mod model;
pub mod object;
pub mod package;
pub mod particle;
pub mod property;
pub mod reader;
pub mod schema;
pub mod script;
pub mod skeletal;
pub mod sound;
pub mod staticmesh;
pub mod summary;
pub mod tables;
pub mod texture;
pub mod types;
pub mod writer;

pub use compression::{BlockInfo, ChunkInfo, ReadOptions};
pub use error::{Result, Ue3Error};
pub use issue::{Issue, Severity};
pub use model::{ClassModel, InheritedDefaults, LoadedPackage, PackageSet};
pub use object::{DecodedObject, ObjectError, ObjectPrelude, decode_object};
pub use package::{MAX_OUTER_DEPTH, ObjectRef, Package, Storage, TableExtents};
pub use property::{ObjRef, Property, Value};
pub use schema::{NoSchema, PropertyDef, PropertyType, Schema, StructDef};
pub use script::{ScriptBody, ScriptKind, ScriptObject, decode_script_object};
pub use summary::{
    CompressedChunk, CompressionMethod, GenerationInfo, PACKAGE_TAG, Summary, TextureAllocation,
    package_flags,
};
pub use tables::{ExportEntry, ImportEntry, NameEntry, ThumbnailEntry};
pub use types::{FName, Guid, IndexKind, PackageIndex};
