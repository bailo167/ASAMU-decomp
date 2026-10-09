//! Package file summary (`FPackageFileSummary`) for file version 868.
//!
//! Layout (little-endian), confirmed against every package shipped with the
//! game (see `docs/reverse-engineering/PACKAGE_ANALYSIS.md`):
//!
//! ```text
//! u32 Tag (0x9E2A83C1) | u16 FileVersion (868) | u16 LicenseeVersion (0)
//! i32 TotalHeaderSize | FString FolderName | u32 PackageFlags
//! i32 NameCount, NameOffset | i32 ExportCount, ExportOffset | i32 ImportCount, ImportOffset
//! i32 DependsOffset | i32 ImportExportGuidsOffset, ImportGuidsCount, ExportGuidsCount
//! i32 ThumbnailTableOffset | Guid | TArray<{i32 ExportCount, NameCount, NetObjectCount}> Generations
//! i32 EngineVersion | i32 CookerVersion | u32 CompressionFlags
//! TArray<{i32 UncompressedOffset, UncompressedSize, CompressedOffset, CompressedSize}> CompressedChunks
//! u32 PackageSource | TArray<FString> AdditionalPackagesToCook
//! TArray<{i32 SizeX, SizeY, NumMips; u32 Format, TexCreateFlags; TArray<i32> ExportIndices}> TextureAllocations
//! ```

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use serde::Serialize;

use crate::error::{Result, Ue3Error};
use crate::reader::Reader;
use crate::types::Guid;
use crate::writer::Writer;

/// Package tag as read little-endian (bytes `C1 83 2A 9E`).
pub const PACKAGE_TAG: u32 = 0x9E2A_83C1;
/// Byte-swapped tag, as a big-endian (console) package would present it.
pub const PACKAGE_TAG_SWAPPED: u32 = 0xC183_2A9E;
/// The only file version this reader understands.
pub const SUPPORTED_FILE_VERSION: u16 = 868;
/// The only licensee version this reader understands.
pub const SUPPORTED_LICENSEE_VERSION: u16 = 0;
/// Most bytes [`Summary::read_from_path`] reads while looking for the end of
/// the summary. Shipped summaries are at most 9,005 bytes; this only stops a
/// hostile count from making the prefix reader pull in a whole huge file.
pub const MAX_SUMMARY_PREFIX: u64 = 64 * 1024 * 1024;

/// UE3 `EPackageFlags` bit values.
///
/// The names follow UE3 engine conventions. Confidence per bit is recorded in
/// PACKAGE_ANALYSIS.md ("Package flags"): it combines correlations across the
/// 42 shipped packages with the code of the original executable that tests or
/// sets each bit (loader, saver, net and download code; local Ghidra reading,
/// nothing copied). "CONFIRMED meaning" = the executable's behaviour for the
/// bit is read directly and agrees with the data; the UE3 *name* itself is
/// then STRONG. Only nine bits occur in shipped summaries.
pub mod package_flags {
    /// `PKG_AllowDownload`. Observed on 32 packages. The original code sets it
    /// on every newly created package and clears it on the GUID-cache package
    /// (`GuidCache.upk` is one of the 10 without it). Meaning (download
    /// permission) TENTATIVE.
    pub const ALLOW_DOWNLOAD: u32 = 0x0000_0001;
    /// `PKG_ClientOptional`. STRONG: the download code lets a client skip a
    /// package file with this bit. Never observed.
    pub const CLIENT_OPTIONAL: u32 = 0x0000_0002;
    /// `PKG_ServerSideOnly`. CONFIRMED meaning: objects whose outermost package
    /// has the bit get no net index, and a forced-export package with no net
    /// objects receives it at load. Data: set on all 17 packages whose
    /// generation `NetObjectCount` is 0 and on the 3 shader caches (whose save
    /// path sets it explicitly); 8,365 of 8,365 checked objects of such
    /// packages store `NetIndex = -1`.
    pub const SERVER_SIDE_ONLY: u32 = 0x0000_0004;
    /// `PKG_Cooked`. Set on every shipped package except the three
    /// `RefShaderCache-*` packages. STRONG: the loader switches its archive to
    /// cooked-data mode when the bit is set, and many cooked-only paths test it.
    pub const COOKED: u32 = 0x0000_0008;
    /// `PKG_Unsecure`. TENTATIVE; never observed.
    pub const UNSECURE: u32 = 0x0000_0010;
    /// `PKG_SavedWithNewerVersion`. STRONG: the loader sets it (warning once)
    /// when a package's engine version is newer than the running engine's.
    /// Never observed.
    pub const SAVED_WITH_NEWER_VERSION: u32 = 0x0000_0020;
    /// `PKG_Need`. TENTATIVE; never observed.
    pub const NEED: u32 = 0x0000_8000;
    /// `PKG_Compiling`. TENTATIVE: import verification stops early for a
    /// package with this bit. Never observed.
    pub const COMPILING: u32 = 0x0001_0000;
    /// `PKG_ContainsMap`. CONFIRMED meaning: the world and level serializers
    /// set it on their outermost package when saving; set on exactly the 12
    /// `.asamu` map packages.
    pub const CONTAINS_MAP: u32 = 0x0002_0000;
    /// `PKG_Trash`. STRONG: the loader never copies it from a file and sets it
    /// when the file path contains `__Trashcan`. Never observed.
    pub const TRASH: u32 = 0x0004_0000;
    /// `PKG_DisallowLazyLoading`. STRONG: the loader disables lazy loading for
    /// a package with this bit (except cooked packages in the editor).
    /// Observed on 37.
    pub const DISALLOW_LAZY_LOADING: u32 = 0x0008_0000;
    /// `PKG_PlayInEditor`. TENTATIVE (tested by level-streaming and
    /// dirty-marking code); never observed.
    pub const PLAY_IN_EDITOR: u32 = 0x0010_0000;
    /// `PKG_ContainsScript`. Corroborated: set on exactly the `.u` script
    /// packages and on the `asamu`/`UTGame` package exports in `Startup.upk`;
    /// the code never marks such packages dirty. STRONG.
    pub const CONTAINS_SCRIPT: u32 = 0x0020_0000;
    /// `PKG_ContainsDebugInfo`. TENTATIVE; never observed.
    pub const CONTAINS_DEBUG_INFO: u32 = 0x0040_0000;
    /// `PKG_RequireImportsAlreadyLoaded`. STRONG: the loader skips import
    /// verification for a package with this bit, and the editor clears it on
    /// load. Observed on 35.
    pub const REQUIRE_IMPORTS_ALREADY_LOADED: u32 = 0x0080_0000;
    /// `PKG_SelfContainedLighting`. TENTATIVE; never observed.
    pub const SELF_CONTAINED_LIGHTING: u32 = 0x0100_0000;
    /// `PKG_StoreCompressed`. CONFIRMED: set exactly when CompressionFlags != 0,
    /// and the loader installs the compressed-chunk map only when it is set.
    pub const STORE_COMPRESSED: u32 = 0x0200_0000;
    /// `PKG_StoreFullyCompressed`. TENTATIVE (tested by the saver); never
    /// observed.
    pub const STORE_FULLY_COMPRESSED: u32 = 0x0400_0000;
    /// `PKG_ContainsInlinedShaders`. TENTATIVE; never observed.
    pub const CONTAINS_INLINED_SHADERS: u32 = 0x0800_0000;
    /// `PKG_ContainsFaceFXData`. CONFIRMED meaning: the loader sets it in
    /// memory when an export's class is `FaceFXAsset` or `FaceFXAnimSet`.
    /// Never stored in a shipped summary.
    pub const CONTAINS_FACEFX_DATA: u32 = 0x1000_0000;
    /// `PKG_NoExportAllowed`. CONFIRMED meaning: the loader sets it on a
    /// package whose summary `PackageSource` equals the checksum of its base
    /// file name (true for all 42 shipped packages; see
    /// [`crate::flags::package_source`]). Stored in 27 summaries.
    pub const NO_EXPORT_ALLOWED: u32 = 0x2000_0000;
    /// `PKG_StrippedSource`. TENTATIVE; never observed.
    pub const STRIPPED_SOURCE: u32 = 0x4000_0000;

    /// (bit, conventional UE3 name) pairs, low bit first.
    pub const NAMES: &[(u32, &str)] = &[
        (ALLOW_DOWNLOAD, "AllowDownload"),
        (CLIENT_OPTIONAL, "ClientOptional"),
        (SERVER_SIDE_ONLY, "ServerSideOnly"),
        (COOKED, "Cooked"),
        (UNSECURE, "Unsecure"),
        (SAVED_WITH_NEWER_VERSION, "SavedWithNewerVersion"),
        (NEED, "Need"),
        (COMPILING, "Compiling"),
        (CONTAINS_MAP, "ContainsMap"),
        (TRASH, "Trash"),
        (DISALLOW_LAZY_LOADING, "DisallowLazyLoading"),
        (PLAY_IN_EDITOR, "PlayInEditor"),
        (CONTAINS_SCRIPT, "ContainsScript"),
        (CONTAINS_DEBUG_INFO, "ContainsDebugInfo"),
        (
            REQUIRE_IMPORTS_ALREADY_LOADED,
            "RequireImportsAlreadyLoaded",
        ),
        (SELF_CONTAINED_LIGHTING, "SelfContainedLighting"),
        (STORE_COMPRESSED, "StoreCompressed"),
        (STORE_FULLY_COMPRESSED, "StoreFullyCompressed"),
        (CONTAINS_INLINED_SHADERS, "ContainsInlinedShaders"),
        (CONTAINS_FACEFX_DATA, "ContainsFaceFXData"),
        (NO_EXPORT_ALLOWED, "NoExportAllowed"),
        (STRIPPED_SOURCE, "StrippedSource"),
    ];

    /// Conventional names of the set bits; unknown bits as `0x........`.
    pub fn describe(flags: u32) -> Vec<String> {
        let mut out = Vec::new();
        let mut known = 0u32;
        for &(bit, name) in NAMES {
            known |= bit;
            if flags & bit != 0 {
                out.push(name.to_owned());
            }
        }
        let unknown = flags & !known;
        if unknown != 0 {
            out.push(format!("{unknown:#010x}"));
        }
        out
    }
}

/// Compression method named by `CompressionFlags` (UE3 `ECompressionFlags`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum CompressionMethod {
    /// `0`: stored uncompressed.
    None,
    /// `1`: `COMPRESS_ZLIB` (not supported; never observed in this game).
    Zlib,
    /// `2`: `COMPRESS_LZO`.
    Lzo,
    /// `4`: `COMPRESS_LZX` (not supported; never observed in this game).
    Lzx,
    /// Anything else (including bias flags combined with a method).
    Unknown(u32),
}

impl CompressionMethod {
    /// Decode raw `CompressionFlags`.
    pub fn from_flags(flags: u32) -> Self {
        match flags {
            0 => CompressionMethod::None,
            1 => CompressionMethod::Zlib,
            2 => CompressionMethod::Lzo,
            4 => CompressionMethod::Lzx,
            other => CompressionMethod::Unknown(other),
        }
    }

    /// Short lower-case name.
    pub fn name(self) -> &'static str {
        match self {
            CompressionMethod::None => "none",
            CompressionMethod::Zlib => "zlib",
            CompressionMethod::Lzo => "lzo",
            CompressionMethod::Lzx => "lzx",
            CompressionMethod::Unknown(_) => "unknown",
        }
    }
}

/// One entry of the summary's generation table.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct GenerationInfo {
    /// Export count at that generation.
    pub export_count: i32,
    /// Name count at that generation.
    pub name_count: i32,
    /// Net object count at that generation.
    pub net_object_count: i32,
}

/// One entry of the summary's compressed-chunk table (`FCompressedChunk`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct CompressedChunk {
    /// Offset of the chunk's output in the uncompressed stream.
    pub uncompressed_offset: u32,
    /// Size of the chunk's output.
    pub uncompressed_size: u32,
    /// File offset of the chunk header.
    pub compressed_offset: u32,
    /// Bytes the chunk occupies in the file (header + block table + blocks).
    pub compressed_size: u32,
}

/// One entry of the summary's texture-allocation table (`FTextureType`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TextureAllocation {
    /// Width.
    pub size_x: i32,
    /// Height.
    pub size_y: i32,
    /// Mip count.
    pub num_mips: i32,
    /// Pixel format (raw `EPixelFormat`).
    pub format: u32,
    /// Texture creation flags.
    pub tex_create_flags: u32,
    /// Export indices that share this allocation.
    pub export_indices: Vec<i32>,
}

/// Parsed package summary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Summary {
    /// Package tag (always [`PACKAGE_TAG`] after a successful parse).
    pub tag: u32,
    /// File version (868).
    pub file_version: u16,
    /// Licensee version (0).
    pub licensee_version: u16,
    /// Offset in the uncompressed stream where the header ends and export data begins.
    pub total_header_size: u32,
    /// Folder name (`"None"` in every shipped package).
    pub folder_name: String,
    /// Raw package flags (see [`package_flags`]).
    pub package_flags: u32,
    /// Name-table entry count.
    pub name_count: u32,
    /// Name-table offset (uncompressed stream).
    pub name_offset: u32,
    /// Export-table entry count.
    pub export_count: u32,
    /// Export-table offset (uncompressed stream).
    pub export_offset: u32,
    /// Import-table entry count.
    pub import_count: u32,
    /// Import-table offset (uncompressed stream).
    pub import_offset: u32,
    /// Depends-map offset (uncompressed stream).
    pub depends_offset: u32,
    /// Import/export GUID data offset (uncompressed stream).
    pub import_export_guids_offset: u32,
    /// Number of import GUID records.
    pub import_guids_count: u32,
    /// Number of export GUID records.
    pub export_guids_count: u32,
    /// Thumbnail table offset (0 = none).
    pub thumbnail_table_offset: u32,
    /// Package GUID.
    pub guid: Guid,
    /// Generation table.
    pub generations: Vec<GenerationInfo>,
    /// Engine version (12097 in every shipped package).
    pub engine_version: i32,
    /// Cooker version.
    pub cooker_version: i32,
    /// Raw compression flags.
    pub compression_flags: u32,
    /// Compressed chunk table (empty for uncompressed packages).
    pub compressed_chunks: Vec<CompressedChunk>,
    /// Package source checksum: the CRC of the package's base file name
    /// (see [`crate::flags::package_source`]).
    pub package_source: u32,
    /// Additional packages to cook (map packages naming their sublevels).
    pub additional_packages_to_cook: Vec<String>,
    /// Texture allocation table.
    pub texture_allocations: Vec<TextureAllocation>,
    /// Number of bytes the summary occupies at the start of the file.
    pub serialized_size: usize,
}

impl Summary {
    /// Parse the summary from the start of `data` (the file bytes, or a prefix of them).
    pub fn parse(data: &[u8]) -> Result<Summary> {
        let mut r = Reader::new(data);
        let tag = r.read_u32()?;
        if tag == PACKAGE_TAG_SWAPPED {
            return Err(Ue3Error::BigEndian);
        }
        if tag != PACKAGE_TAG {
            return Err(Ue3Error::BadTag { found: tag });
        }
        let file_version = r.read_u16()?;
        let licensee_version = r.read_u16()?;
        if file_version != SUPPORTED_FILE_VERSION || licensee_version != SUPPORTED_LICENSEE_VERSION
        {
            return Err(Ue3Error::UnsupportedVersion {
                file_version,
                licensee_version,
            });
        }
        let total_header_size = r.read_non_negative("TotalHeaderSize")?;
        let folder_name = r.read_fstring()?;
        let package_flags = r.read_u32()?;
        let name_count = r.read_non_negative("NameCount")?;
        let name_offset = r.read_non_negative("NameOffset")?;
        let export_count = r.read_non_negative("ExportCount")?;
        let export_offset = r.read_non_negative("ExportOffset")?;
        let import_count = r.read_non_negative("ImportCount")?;
        let import_offset = r.read_non_negative("ImportOffset")?;
        let depends_offset = r.read_non_negative("DependsOffset")?;
        let import_export_guids_offset = r.read_non_negative("ImportExportGuidsOffset")?;
        let import_guids_count = r.read_non_negative("ImportGuidsCount")?;
        let export_guids_count = r.read_non_negative("ExportGuidsCount")?;
        let thumbnail_table_offset = r.read_non_negative("ThumbnailTableOffset")?;
        let guid = r.read_guid()?;
        let generations = r.read_tarray("Generations", 12, |r| {
            Ok(GenerationInfo {
                export_count: r.read_i32()?,
                name_count: r.read_i32()?,
                net_object_count: r.read_i32()?,
            })
        })?;
        let engine_version = r.read_i32()?;
        let cooker_version = r.read_i32()?;
        let compression_flags = r.read_u32()?;
        let compressed_chunks = r.read_tarray("CompressedChunks", 16, |r| {
            Ok(CompressedChunk {
                uncompressed_offset: r.read_non_negative("Chunk.UncompressedOffset")?,
                uncompressed_size: r.read_non_negative("Chunk.UncompressedSize")?,
                compressed_offset: r.read_non_negative("Chunk.CompressedOffset")?,
                compressed_size: r.read_non_negative("Chunk.CompressedSize")?,
            })
        })?;
        let package_source = r.read_u32()?;
        let additional_packages_to_cook =
            r.read_tarray("AdditionalPackagesToCook", 4, |r| r.read_fstring())?;
        let texture_allocations = r.read_tarray("TextureAllocations", 24, |r| {
            Ok(TextureAllocation {
                size_x: r.read_i32()?,
                size_y: r.read_i32()?,
                num_mips: r.read_i32()?,
                format: r.read_u32()?,
                tex_create_flags: r.read_u32()?,
                export_indices: r
                    .read_tarray("TextureAllocation.ExportIndices", 4, |r| r.read_i32())?,
            })
        })?;
        Ok(Summary {
            tag,
            file_version,
            licensee_version,
            total_header_size,
            folder_name,
            package_flags,
            name_count,
            name_offset,
            export_count,
            export_offset,
            import_count,
            import_offset,
            depends_offset,
            import_export_guids_offset,
            import_guids_count,
            export_guids_count,
            thumbnail_table_offset,
            guid,
            generations,
            engine_version,
            cooker_version,
            compression_flags,
            compressed_chunks,
            package_source,
            additional_packages_to_cook,
            texture_allocations,
            serialized_size: r.position(),
        })
    }

    /// Read only the summary of a package file, reading a growing prefix of the
    /// file (at most [`MAX_SUMMARY_PREFIX`] bytes) instead of the whole thing.
    /// Returns the summary and the file size.
    pub fn read_from_path(path: &Path) -> Result<(Summary, u64)> {
        Summary::read_from_path_limited(path, MAX_SUMMARY_PREFIX)
    }

    /// [`Summary::read_from_path`] with an explicit prefix limit: a summary that
    /// needs more than `max_prefix` bytes of a longer file fails with
    /// [`Ue3Error::TooLarge`].
    pub fn read_from_path_limited(path: &Path, max_prefix: u64) -> Result<(Summary, u64)> {
        let io = |source| Ue3Error::Io {
            path: path.to_path_buf(),
            source,
        };
        let mut file = File::open(path).map_err(io)?;
        let file_len = file.metadata().map_err(io)?.len();
        let cap = file_len.min(max_prefix);
        let mut want: u64 = 64 * 1024;
        loop {
            let take = want.min(cap);
            file.seek(SeekFrom::Start(0)).map_err(io)?;
            let mut buf = Vec::with_capacity(usize::try_from(take).unwrap_or(0));
            (&mut file).take(take).read_to_end(&mut buf).map_err(io)?;
            match Summary::parse(&buf) {
                Ok(s) => return Ok((s, file_len)),
                Err(Ue3Error::UnexpectedEof { .. } | Ue3Error::CountTooLarge { .. })
                    if take < cap =>
                {
                    want = want.saturating_mul(4);
                }
                Err(Ue3Error::UnexpectedEof { .. } | Ue3Error::CountTooLarge { .. })
                    if take < file_len =>
                {
                    // More of the file exists, but the summary would need more
                    // than the prefix limit.
                    return Err(Ue3Error::TooLarge {
                        what: "package summary",
                        size: take.saturating_add(1),
                        limit: max_prefix,
                    });
                }
                Err(e) => return Err(e),
            }
        }
    }

    /// Decoded compression method.
    pub fn compression(&self) -> CompressionMethod {
        CompressionMethod::from_flags(self.compression_flags)
    }

    /// True when the package body is stored in compressed chunks.
    pub fn is_compressed(&self) -> bool {
        self.compression_flags != 0 || !self.compressed_chunks.is_empty()
    }

    /// `PKG_ContainsMap` is set (corroborated against file extensions; see docs).
    pub fn contains_map(&self) -> bool {
        self.package_flags & package_flags::CONTAINS_MAP != 0
    }

    /// `PKG_ContainsScript` is set (corroborated against file extensions; see docs).
    pub fn contains_script(&self) -> bool {
        self.package_flags & package_flags::CONTAINS_SCRIPT != 0
    }

    /// `PKG_Cooked` is set.
    pub fn is_cooked(&self) -> bool {
        self.package_flags & package_flags::COOKED != 0
    }

    /// Conventional names of the set package flags.
    pub fn package_flag_names(&self) -> Vec<String> {
        package_flags::describe(self.package_flags)
    }

    /// Sum of the chunks' uncompressed sizes.
    pub fn total_uncompressed_chunk_size(&self) -> u64 {
        self.compressed_chunks
            .iter()
            .map(|c| u64::from(c.uncompressed_size))
            .sum()
    }

    /// Sum of the chunks' on-disk sizes.
    pub fn total_compressed_chunk_size(&self) -> u64 {
        self.compressed_chunks
            .iter()
            .map(|c| u64::from(c.compressed_size))
            .sum()
    }

    /// Length of the uncompressed stream implied by the chunk table
    /// (end of the last chunk), or `None` for uncompressed packages.
    pub fn uncompressed_stream_len(&self) -> Option<u64> {
        self.compressed_chunks
            .iter()
            .map(|c| u64::from(c.uncompressed_offset) + u64::from(c.uncompressed_size))
            .max()
    }

    /// Generation entry matching the current tables (the last one).
    pub fn latest_generation(&self) -> Option<&GenerationInfo> {
        self.generations.last()
    }

    /// Re-serialize this summary as it appears at the start of the
    /// *uncompressed* stream: `CompressionFlags = 0` and an empty chunk table,
    /// every other field unchanged. For shipped compressed packages the result
    /// is exactly `chunk[0].UncompressedOffset` bytes long (verified by the
    /// real-data tests).
    pub fn to_uncompressed_bytes(&self) -> Result<Vec<u8>> {
        let mut w = Writer::new();
        w.u32(self.tag);
        w.u16(self.file_version);
        w.u16(self.licensee_version);
        w.i32(to_i32("TotalHeaderSize", self.total_header_size)?);
        if !w.fstring(&self.folder_name) {
            return Err(too_long("FolderName"));
        }
        w.u32(self.package_flags);
        for (what, v) in [
            ("NameCount", self.name_count),
            ("NameOffset", self.name_offset),
            ("ExportCount", self.export_count),
            ("ExportOffset", self.export_offset),
            ("ImportCount", self.import_count),
            ("ImportOffset", self.import_offset),
            ("DependsOffset", self.depends_offset),
            ("ImportExportGuidsOffset", self.import_export_guids_offset),
            ("ImportGuidsCount", self.import_guids_count),
            ("ExportGuidsCount", self.export_guids_count),
            ("ThumbnailTableOffset", self.thumbnail_table_offset),
        ] {
            w.i32(to_i32(what, v)?);
        }
        w.guid(self.guid);
        w.i32(len_i32("Generations", self.generations.len())?);
        for g in &self.generations {
            w.i32(g.export_count);
            w.i32(g.name_count);
            w.i32(g.net_object_count);
        }
        w.i32(self.engine_version);
        w.i32(self.cooker_version);
        w.u32(0); // CompressionFlags: the stream is uncompressed.
        w.i32(0); // CompressedChunks: empty.
        w.u32(self.package_source);
        w.i32(len_i32(
            "AdditionalPackagesToCook",
            self.additional_packages_to_cook.len(),
        )?);
        for s in &self.additional_packages_to_cook {
            if !w.fstring(s) {
                return Err(too_long("AdditionalPackagesToCook"));
            }
        }
        w.i32(len_i32(
            "TextureAllocations",
            self.texture_allocations.len(),
        )?);
        for t in &self.texture_allocations {
            w.i32(t.size_x);
            w.i32(t.size_y);
            w.i32(t.num_mips);
            w.u32(t.format);
            w.u32(t.tex_create_flags);
            w.i32(len_i32("ExportIndices", t.export_indices.len())?);
            for &i in &t.export_indices {
                w.i32(i);
            }
        }
        Ok(w.into_bytes())
    }
}

fn to_i32(what: &'static str, v: u32) -> Result<i32> {
    i32::try_from(v).map_err(|_| Ue3Error::InvalidValue {
        what,
        offset: 0,
        value: i64::from(v),
    })
}

fn len_i32(what: &'static str, n: usize) -> Result<i32> {
    i32::try_from(n).map_err(|_| Ue3Error::InvalidValue {
        what,
        offset: 0,
        value: i64::try_from(n).unwrap_or(i64::MAX),
    })
}

fn too_long(what: &'static str) -> Ue3Error {
    Ue3Error::InvalidValue {
        what,
        offset: 0,
        value: -1,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flag_description() {
        let names = package_flags::describe(0x228A_0009);
        assert_eq!(
            names,
            vec![
                "AllowDownload",
                "Cooked",
                "ContainsMap",
                "DisallowLazyLoading",
                "RequireImportsAlreadyLoaded",
                "StoreCompressed",
                "NoExportAllowed"
            ]
        );
        assert_eq!(package_flags::describe(0x8000_0000), vec!["0x80000000"]);
    }

    #[test]
    fn compression_methods() {
        assert_eq!(CompressionMethod::from_flags(0), CompressionMethod::None);
        assert_eq!(CompressionMethod::from_flags(2), CompressionMethod::Lzo);
        assert_eq!(
            CompressionMethod::from_flags(0x12),
            CompressionMethod::Unknown(0x12)
        );
    }

    #[test]
    fn rejects_bad_tags_and_versions() {
        assert!(matches!(
            Summary::parse(&[0, 0, 0, 0]),
            Err(Ue3Error::BadTag { found: 0 })
        ));
        assert!(matches!(
            Summary::parse(&[0x9E, 0x2A, 0x83, 0xC1]),
            Err(Ue3Error::BigEndian)
        ));
        let mut v = PACKAGE_TAG.to_le_bytes().to_vec();
        v.extend_from_slice(&869u16.to_le_bytes());
        v.extend_from_slice(&0u16.to_le_bytes());
        assert!(matches!(
            Summary::parse(&v),
            Err(Ue3Error::UnsupportedVersion {
                file_version: 869,
                ..
            })
        ));
    }
}
