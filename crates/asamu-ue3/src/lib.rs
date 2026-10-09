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

pub mod compression;
pub mod error;
pub mod issue;
pub mod lzo;
pub mod package;
pub mod reader;
pub mod summary;
pub mod tables;
pub mod types;
pub mod writer;

pub use compression::{BlockInfo, ChunkInfo, ReadOptions};
pub use error::{Result, Ue3Error};
pub use issue::{Issue, Severity};
pub use package::{MAX_OUTER_DEPTH, ObjectRef, Package, Storage, TableExtents};
pub use summary::{
    CompressedChunk, CompressionMethod, GenerationInfo, PACKAGE_TAG, Summary, TextureAllocation,
    package_flags,
};
pub use tables::{ExportEntry, ImportEntry, NameEntry, ThumbnailEntry};
pub use types::{FName, Guid, IndexKind, PackageIndex};
