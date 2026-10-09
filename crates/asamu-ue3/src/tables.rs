//! Name, import and export tables (file version 868).
//!
//! ```text
//! Name   : FString Name | u64 Flags
//! Import : FName ClassPackage | FName ClassName | i32 OuterIndex | FName ObjectName      (28 bytes)
//! Export : i32 ClassIndex | i32 SuperIndex | i32 OuterIndex | FName ObjectName
//!          | i32 ArchetypeIndex | u64 ObjectFlags | i32 SerialSize | i32 SerialOffset
//!          | u32 ExportFlags | TArray<i32> GenerationNetObjectCount | Guid PackageGuid
//!          | u32 PackageFlags                                        (68 bytes + 4 per net count)
//! ```

use serde::Serialize;

use crate::error::Result;
use crate::reader::Reader;
use crate::types::{FName, Guid, PackageIndex};

/// Smallest serialized name entry: empty FString (4) + flags (8).
pub const NAME_ENTRY_MIN_SIZE: usize = 12;
/// Serialized import entry size.
pub const IMPORT_ENTRY_SIZE: usize = 28;
/// Smallest serialized export entry (empty `GenerationNetObjectCount`).
pub const EXPORT_ENTRY_MIN_SIZE: usize = 68;

/// One name-table entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct NameEntry {
    /// The name string.
    pub name: String,
    /// Raw `EObjectFlags` stored with the name.
    pub flags: u64,
}

/// One import-table entry (`FObjectImport`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct ImportEntry {
    /// Package of the imported object's class (e.g. `Core`).
    pub class_package: FName,
    /// Class of the imported object (e.g. `Class`, `Package`, `Texture2D`).
    pub class_name: FName,
    /// Outer object.
    pub outer_index: PackageIndex,
    /// Object name.
    pub object_name: FName,
}

/// One export-table entry (`FObjectExport`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ExportEntry {
    /// Class of the object (`0` = `Core.Class`, i.e. the object is a class).
    pub class_index: PackageIndex,
    /// Super struct (for classes/structs/functions).
    pub super_index: PackageIndex,
    /// Outer object.
    pub outer_index: PackageIndex,
    /// Object name.
    pub object_name: FName,
    /// Archetype object.
    pub archetype_index: PackageIndex,
    /// Raw `EObjectFlags`.
    pub object_flags: u64,
    /// Size of the serialized object payload.
    pub serial_size: i32,
    /// Offset of the payload in the uncompressed stream.
    pub serial_offset: i32,
    /// Raw `EExportFlags`.
    pub export_flags: u32,
    /// Per-generation net object counts.
    pub generation_net_object_count: Vec<i32>,
    /// Package GUID (meaningful for `Package` exports).
    pub package_guid: Guid,
    /// Package flags (meaningful for `Package` exports).
    pub package_flags: u32,
}

/// Read `count` name entries starting at the reader's position.
pub fn read_names(r: &mut Reader<'_>, count: usize) -> Result<Vec<NameEntry>> {
    let at = r.position();
    r.check_count("NameTable", at, count, NAME_ENTRY_MIN_SIZE)?;
    let mut out = Vec::with_capacity(count);
    for _ in 0..count {
        out.push(NameEntry {
            name: r.read_fstring()?,
            flags: r.read_u64()?,
        });
    }
    Ok(out)
}

/// Read one import entry.
pub fn read_import(r: &mut Reader<'_>) -> Result<ImportEntry> {
    Ok(ImportEntry {
        class_package: r.read_fname()?,
        class_name: r.read_fname()?,
        outer_index: r.read_package_index()?,
        object_name: r.read_fname()?,
    })
}

/// Read `count` import entries starting at the reader's position.
pub fn read_imports(r: &mut Reader<'_>, count: usize) -> Result<Vec<ImportEntry>> {
    let at = r.position();
    r.check_count("ImportTable", at, count, IMPORT_ENTRY_SIZE)?;
    let mut out = Vec::with_capacity(count);
    for _ in 0..count {
        out.push(read_import(r)?);
    }
    Ok(out)
}

/// Read one export entry.
pub fn read_export(r: &mut Reader<'_>) -> Result<ExportEntry> {
    Ok(ExportEntry {
        class_index: r.read_package_index()?,
        super_index: r.read_package_index()?,
        outer_index: r.read_package_index()?,
        object_name: r.read_fname()?,
        archetype_index: r.read_package_index()?,
        object_flags: r.read_u64()?,
        serial_size: r.read_i32()?,
        serial_offset: r.read_i32()?,
        export_flags: r.read_u32()?,
        generation_net_object_count: r
            .read_tarray("Export.GenerationNetObjectCount", 4, |r| r.read_i32())?,
        package_guid: r.read_guid()?,
        package_flags: r.read_u32()?,
    })
}

/// Read `count` export entries starting at the reader's position.
pub fn read_exports(r: &mut Reader<'_>, count: usize) -> Result<Vec<ExportEntry>> {
    let at = r.position();
    r.check_count("ExportTable", at, count, EXPORT_ENTRY_MIN_SIZE)?;
    let mut out = Vec::with_capacity(count);
    for _ in 0..count {
        out.push(read_export(r)?);
    }
    Ok(out)
}

/// Read the depends map: one `TArray<i32>` per export.
pub fn read_depends(r: &mut Reader<'_>, export_count: usize) -> Result<Vec<Vec<PackageIndex>>> {
    let at = r.position();
    r.check_count("DependsMap", at, export_count, 4)?;
    let mut out = Vec::with_capacity(export_count);
    for _ in 0..export_count {
        out.push(r.read_tarray("DependsMap entry", 4, |r| r.read_package_index())?);
    }
    Ok(out)
}

/// One editor thumbnail: a table entry plus the record it points at.
///
/// Table entry (at `ThumbnailTableOffset`, after an `i32` count):
/// `FString ObjectClassName | FString ObjectPathWithoutPackageName | i32 FileOffset`.
/// Record (at `FileOffset`): `i32 Width | i32 Height | TArray<u8> CompressedImageData`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ThumbnailEntry {
    /// Class name of the thumbnailed object.
    pub class_name: String,
    /// Object path without the package name.
    pub object_path: String,
    /// Offset of the thumbnail record in the stream.
    pub offset: u32,
    /// Image width.
    pub width: i32,
    /// Image height.
    pub height: i32,
    /// Size of the compressed image data. In the shipped packages every
    /// non-empty record holds PNG data (13 of 21 records); the other 8 are
    /// empty (`0 x 0`, size 0).
    pub data_size: u32,
    /// End offset of the record in the stream.
    pub end: usize,
}

/// Read the thumbnail table at the reader's position, then each record it
/// references in `stream`.
pub fn read_thumbnails(r: &mut Reader<'_>, stream: &[u8]) -> Result<Vec<ThumbnailEntry>> {
    let table = r.read_tarray("ThumbnailTable", 12, |r| {
        Ok((
            r.read_fstring()?,
            r.read_fstring()?,
            r.read_non_negative("Thumbnail.FileOffset")?,
        ))
    })?;
    let mut out = Vec::with_capacity(table.len());
    for (class_name, object_path, offset) in table {
        let mut rec = Reader::at(stream, offset as usize)?;
        let width = rec.read_i32()?;
        let height = rec.read_i32()?;
        let size = rec.read_count("Thumbnail.CompressedImageData", 1)?;
        rec.skip(size)?;
        out.push(ThumbnailEntry {
            class_name,
            object_path,
            offset,
            width,
            height,
            data_size: u32::try_from(size).unwrap_or(u32::MAX),
            end: rec.position(),
        });
    }
    Ok(out)
}
