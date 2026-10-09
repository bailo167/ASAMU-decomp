//! A parsed package: summary, uncompressed stream, tables, cross-checks and
//! object-reference resolution.

use std::collections::BTreeMap;
use std::path::Path;

use serde::Serialize;

use crate::compression::{self, ChunkInfo, ReadOptions};
use crate::error::{Result, Ue3Error};
use crate::issue::{Issue, Severity};
use crate::reader::Reader;
use crate::summary::{CompressionMethod, Summary};
use crate::tables::{self, ExportEntry, ImportEntry, NameEntry, ThumbnailEntry};
use crate::types::{FName, IndexKind, PackageIndex};

/// Longest outer chain followed before giving up (guards against cycles).
pub const MAX_OUTER_DEPTH: usize = 256;

/// Most per-export findings recorded individually before summarizing.
const MAX_LISTED_EXPORT_ISSUES: usize = 32;

/// How the package body is stored on disk.
#[derive(Debug, Clone, Serialize)]
pub enum Storage {
    /// The file is the uncompressed stream.
    Uncompressed,
    /// The body is stored in compressed chunks; the stream was rebuilt in memory.
    Compressed {
        /// Compression method.
        method: CompressionMethod,
        /// Validated chunk layouts.
        chunks: Vec<ChunkInfo>,
    },
}

/// Byte ranges `[start, end)` of the header tables in the uncompressed stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize)]
pub struct TableExtents {
    /// Name table.
    pub names: (usize, usize),
    /// Import table.
    pub imports: (usize, usize),
    /// Export table.
    pub exports: (usize, usize),
    /// Depends map (when parsed).
    pub depends: Option<(usize, usize)>,
    /// Thumbnail records (first record start to last record end), when present.
    pub thumbnail_data: Option<(usize, usize)>,
    /// Thumbnail table, when present.
    pub thumbnail_table: Option<(usize, usize)>,
}

/// A resolved package index.
#[derive(Debug, Clone, Copy)]
pub enum ObjectRef<'a> {
    /// Null reference.
    Null,
    /// An import, with its zero-based index.
    Import(usize, &'a ImportEntry),
    /// An export, with its zero-based index.
    Export(usize, &'a ExportEntry),
}

/// A fully parsed package.
#[derive(Debug, Clone)]
pub struct Package {
    /// File summary as stored on disk (including the chunk table).
    pub summary: Summary,
    /// Storage mode.
    pub storage: Storage,
    /// Name table.
    pub names: Vec<NameEntry>,
    /// Import table.
    pub imports: Vec<ImportEntry>,
    /// Export table.
    pub exports: Vec<ExportEntry>,
    /// Depends map (one list per export), when present and parseable.
    pub depends: Option<Vec<Vec<PackageIndex>>>,
    /// Editor thumbnails, when the package has a thumbnail table.
    pub thumbnails: Option<Vec<ThumbnailEntry>>,
    /// Table byte ranges in the stream.
    pub extents: TableExtents,
    /// Cross-check findings.
    pub issues: Vec<Issue>,
    /// Size of the file on disk.
    pub file_size: u64,
    stream: Vec<u8>,
}

impl Package {
    /// Read and parse a package file.
    pub fn open(path: impl AsRef<Path>) -> Result<Package> {
        Package::open_with(path, &ReadOptions::default())
    }

    /// Read and parse a package file with explicit options.
    ///
    /// Files larger than [`ReadOptions::max_stream_size`] are refused before
    /// anything is read: an uncompressed package is its own stream, and every
    /// offset in a compressed one is an `i32`.
    pub fn open_with(path: impl AsRef<Path>, opts: &ReadOptions) -> Result<Package> {
        let path = path.as_ref();
        let io = |source| Ue3Error::Io {
            path: path.to_path_buf(),
            source,
        };
        let len = std::fs::metadata(path).map_err(io)?.len();
        if len > opts.max_stream_size {
            return Err(Ue3Error::TooLarge {
                what: "package file",
                size: len,
                limit: opts.max_stream_size,
            });
        }
        let bytes = std::fs::read(path).map_err(io)?;
        Package::from_bytes_with(bytes, opts)
    }

    /// Parse a package from its file bytes.
    pub fn from_bytes(file: Vec<u8>) -> Result<Package> {
        Package::from_bytes_with(file, &ReadOptions::default())
    }

    /// Parse a package from its file bytes with explicit options.
    pub fn from_bytes_with(file: Vec<u8>, opts: &ReadOptions) -> Result<Package> {
        let summary = Summary::parse(&file)?;
        let file_size = file.len() as u64;
        let mut issues = Vec::new();
        let (storage, stream) = if summary.is_compressed() {
            let d = compression::decompress_package(&file, &summary, opts)?;
            issues.extend(d.issues);
            (
                Storage::Compressed {
                    method: summary.compression(),
                    chunks: d.chunks,
                },
                d.stream,
            )
        } else {
            // The file is the stream: apply the same cap as for a rebuilt one.
            if file_size > opts.max_stream_size {
                return Err(Ue3Error::TooLarge {
                    what: "uncompressed stream",
                    size: file_size,
                    limit: opts.max_stream_size,
                });
            }
            (Storage::Uncompressed, file)
        };
        Package::from_stream(summary, storage, stream, file_size, issues)
    }

    fn from_stream(
        summary: Summary,
        storage: Storage,
        stream: Vec<u8>,
        file_size: u64,
        mut issues: Vec<Issue>,
    ) -> Result<Package> {
        let len = stream.len();
        let table = |what: &str, offset: u32| -> Result<Reader<'_>> {
            Reader::at(&stream, offset as usize).map_err(|_| Ue3Error::OutOfBounds {
                what: what.to_owned(),
                offset: u64::from(offset),
                size: 0,
                len,
            })
        };

        let mut r = table("name table", summary.name_offset)?;
        let names = tables::read_names(&mut r, summary.name_count as usize)?;
        let names_ext = (summary.name_offset as usize, r.position());

        let mut r = table("import table", summary.import_offset)?;
        let imports = tables::read_imports(&mut r, summary.import_count as usize)?;
        let imports_ext = (summary.import_offset as usize, r.position());

        let mut r = table("export table", summary.export_offset)?;
        let exports = tables::read_exports(&mut r, summary.export_count as usize)?;
        let exports_ext = (summary.export_offset as usize, r.position());

        let mut depends = None;
        let mut depends_ext = None;
        if summary.depends_offset != 0 && !exports.is_empty() {
            match table("depends map", summary.depends_offset)
                .and_then(|mut r| tables::read_depends(&mut r, exports.len()).map(|d| (d, r)))
            {
                Ok((d, r)) => {
                    depends_ext = Some((summary.depends_offset as usize, r.position()));
                    depends = Some(d);
                }
                Err(e) => issues.push(Issue::warning(format!("depends map not parsed: {e}"))),
            }
        }

        let mut thumbnails = None;
        let mut thumbnail_table = None;
        let mut thumbnail_data = None;
        if summary.thumbnail_table_offset != 0 {
            match table("thumbnail table", summary.thumbnail_table_offset)
                .and_then(|mut r| tables::read_thumbnails(&mut r, &stream).map(|t| (t, r)))
            {
                Ok((t, r)) => {
                    thumbnail_table = Some((summary.thumbnail_table_offset as usize, r.position()));
                    let start = t.iter().map(|e| e.offset as usize).min();
                    let end = t.iter().map(|e| e.end).max();
                    thumbnail_data = start.zip(end);
                    thumbnails = Some(t);
                }
                Err(e) => issues.push(Issue::warning(format!("thumbnail table not parsed: {e}"))),
            }
        }

        let mut pkg = Package {
            summary,
            storage,
            names,
            imports,
            exports,
            depends,
            thumbnails,
            extents: TableExtents {
                names: names_ext,
                imports: imports_ext,
                exports: exports_ext,
                depends: depends_ext,
                thumbnail_data,
                thumbnail_table,
            },
            issues,
            file_size,
            stream,
        };
        pkg.validate_references()?;
        pkg.cross_check();
        Ok(pkg)
    }

    /// Hard structural checks: every FName and package index in the tables must
    /// resolve.
    fn validate_references(&self) -> Result<()> {
        for (i, imp) in self.imports.iter().enumerate() {
            for n in [imp.class_package, imp.class_name, imp.object_name] {
                self.check_name(n, || format!("import {i}"))?;
            }
            self.check_index(imp.outer_index, || format!("import {i} outer"))?;
        }
        for (i, exp) in self.exports.iter().enumerate() {
            self.check_name(exp.object_name, || format!("export {i}"))?;
            for (idx, what) in [
                (exp.class_index, "class"),
                (exp.super_index, "super"),
                (exp.outer_index, "outer"),
                (exp.archetype_index, "archetype"),
            ] {
                self.check_index(idx, || format!("export {i} {what}"))?;
            }
        }
        Ok(())
    }

    fn check_name(&self, n: FName, context: impl FnOnce() -> String) -> Result<()> {
        let ok = usize::try_from(n.index).is_ok_and(|i| i < self.names.len());
        if ok {
            Ok(())
        } else {
            Err(Ue3Error::BadNameIndex {
                index: n.index,
                count: self.names.len(),
                context: context(),
            })
        }
    }

    fn check_index(&self, idx: PackageIndex, context: impl FnOnce() -> String) -> Result<()> {
        let ok = match idx.kind() {
            IndexKind::Null => true,
            IndexKind::Import(i) => i < self.imports.len(),
            IndexKind::Export(i) => i < self.exports.len(),
        };
        if ok {
            Ok(())
        } else {
            Err(Ue3Error::BadPackageIndex {
                index: idx.0,
                imports: self.imports.len(),
                exports: self.exports.len(),
                context: context(),
            })
        }
    }

    /// Soft cross-checks recorded in [`Package::issues`].
    fn cross_check(&mut self) {
        let mut issues = Vec::new();
        let s = &self.summary;
        let len = self.stream.len();

        if s.total_header_size as usize > len {
            issues.push(Issue::error(format!(
                "TotalHeaderSize {} beyond stream length {len}",
                s.total_header_size
            )));
        }
        for (what, off) in [
            ("ImportExportGuidsOffset", s.import_export_guids_offset),
            ("DependsOffset", s.depends_offset),
            ("ThumbnailTableOffset", s.thumbnail_table_offset),
        ] {
            if off as usize > len {
                issues.push(Issue::warning(format!(
                    "{what} {off} beyond stream length {len}"
                )));
            }
        }
        if s.import_guids_count != 0 || s.export_guids_count != 0 {
            issues.push(Issue::warning(format!(
                "import/export GUID records present ({} / {}); layout not parsed",
                s.import_guids_count, s.export_guids_count
            )));
        }
        match s.latest_generation() {
            None => issues.push(Issue::warning("generation table is empty")),
            Some(g) => {
                if i64::from(g.export_count) != i64::from(s.export_count)
                    || i64::from(g.name_count) != i64::from(s.name_count)
                {
                    issues.push(Issue::warning(format!(
                        "latest generation counts (exports {}, names {}) differ from tables (exports {}, names {})",
                        g.export_count, g.name_count, s.export_count, s.name_count
                    )));
                }
            }
        }

        let header_end = s.total_header_size as usize;
        // The stream's own summary has no chunk table (16 bytes per chunk shorter).
        let summary_len = s
            .serialized_size
            .saturating_sub(s.compressed_chunks.len().saturating_mul(16));
        let mut regions = vec![
            ("summary", (0, summary_len)),
            ("name table", self.extents.names),
            ("import table", self.extents.imports),
            ("export table", self.extents.exports),
        ];
        if let Some(d) = self.extents.depends {
            regions.push(("depends map", d));
        }
        if let Some(d) = self.extents.thumbnail_data {
            regions.push(("thumbnail data", d));
        }
        if let Some(d) = self.extents.thumbnail_table {
            regions.push(("thumbnail table", d));
        }
        for (i, &(a_name, a)) in regions.iter().enumerate() {
            if a.1 > header_end {
                issues.push(Issue::warning(format!(
                    "{a_name} ends at {} beyond TotalHeaderSize {header_end}",
                    a.1
                )));
            }
            for &(b_name, b) in regions.iter().skip(i + 1) {
                if a.0 < b.1 && b.0 < a.1 {
                    issues.push(Issue::warning(format!(
                        "{a_name} [{}, {}) overlaps {b_name} [{}, {})",
                        a.0, a.1, b.0, b.1
                    )));
                }
            }
        }

        let mut listed = 0usize;
        let mut unlisted = 0usize;
        for (i, e) in self.exports.iter().enumerate() {
            let problem = match serial_range(e, len) {
                Err(msg) => Some(Issue::error(format!("export {i}: {msg}"))),
                Ok(Some((start, _))) if start < header_end => Some(Issue::warning(format!(
                    "export {i}: serial offset {start} inside the header (TotalHeaderSize {header_end})"
                ))),
                Ok(_) => None,
            };
            if let Some(p) = problem {
                if listed < MAX_LISTED_EXPORT_ISSUES {
                    issues.push(p);
                    listed += 1;
                } else {
                    unlisted += 1;
                }
            }
        }
        if unlisted > 0 {
            issues.push(Issue::error(format!(
                "{unlisted} further export serial-range findings not listed"
            )));
        }
        issues.extend(payload_tiling(&self.exports, header_end, len));
        self.issues.extend(issues);
    }

    /// The uncompressed stream (for uncompressed packages, the file itself).
    pub fn stream(&self) -> &[u8] {
        &self.stream
    }

    /// Consume the package and return the uncompressed stream.
    pub fn into_stream(self) -> Vec<u8> {
        self.stream
    }

    /// True when the body was stored compressed.
    pub fn is_compressed(&self) -> bool {
        matches!(self.storage, Storage::Compressed { .. })
    }

    /// True if any cross-check reported an error-level finding.
    pub fn has_errors(&self) -> bool {
        self.issues.iter().any(|i| i.severity == Severity::Error)
    }

    /// Name-table string at `index`.
    pub fn name(&self, index: i32) -> Option<&str> {
        usize::try_from(index)
            .ok()
            .and_then(|i| self.names.get(i))
            .map(|n| n.name.as_str())
    }

    /// Display an FName (`Name` or `Name_{number-1}`), failing on a bad index.
    pub fn try_fname(&self, n: FName) -> Result<String> {
        self.check_name(n, || "FName lookup".to_owned())?;
        Ok(n.display_with(self.name(n.index).unwrap_or_default()))
    }

    /// Display an FName; a bad index displays as `<bad name #N>`.
    pub fn fname(&self, n: FName) -> String {
        match self.name(n.index) {
            Some(base) => n.display_with(base),
            None => format!("<bad name #{}>", n.index),
        }
    }

    /// Resolve a package index against the tables.
    pub fn resolve(&self, idx: PackageIndex) -> Result<ObjectRef<'_>> {
        let err = || Ue3Error::BadPackageIndex {
            index: idx.0,
            imports: self.imports.len(),
            exports: self.exports.len(),
            context: "resolve".to_owned(),
        };
        Ok(match idx.kind() {
            IndexKind::Null => ObjectRef::Null,
            IndexKind::Import(i) => ObjectRef::Import(i, self.imports.get(i).ok_or_else(err)?),
            IndexKind::Export(i) => ObjectRef::Export(i, self.exports.get(i).ok_or_else(err)?),
        })
    }

    /// Object name of a reference (`None` for null).
    pub fn object_name(&self, idx: PackageIndex) -> Result<String> {
        Ok(match self.resolve(idx)? {
            ObjectRef::Null => "None".to_owned(),
            ObjectRef::Import(_, imp) => self.fname(imp.object_name),
            ObjectRef::Export(_, exp) => self.fname(exp.object_name),
        })
    }

    /// Outer of a reference (null for null).
    pub fn outer_of(&self, idx: PackageIndex) -> Result<PackageIndex> {
        Ok(match self.resolve(idx)? {
            ObjectRef::Null => PackageIndex::NULL,
            ObjectRef::Import(_, imp) => imp.outer_index,
            ObjectRef::Export(_, exp) => exp.outer_index,
        })
    }

    /// The reference followed by its outers, innermost first, ending at the
    /// outermost object (null excluded). Fails on cycles / excessive depth.
    pub fn outer_chain(&self, idx: PackageIndex) -> Result<Vec<PackageIndex>> {
        let mut chain = Vec::new();
        let mut cur = idx;
        while !cur.is_null() {
            if chain.len() >= MAX_OUTER_DEPTH {
                return Err(Ue3Error::OuterChainTooDeep {
                    start: idx.0,
                    limit: MAX_OUTER_DEPTH,
                });
            }
            chain.push(cur);
            cur = self.outer_of(cur)?;
        }
        Ok(chain)
    }

    /// Full dotted path (`Outermost.Outer.Name`) of a reference; `None` for null.
    pub fn object_path(&self, idx: PackageIndex) -> Result<String> {
        if idx.is_null() {
            return Ok("None".to_owned());
        }
        let chain = self.outer_chain(idx)?;
        let mut parts = Vec::with_capacity(chain.len());
        for p in chain.iter().rev() {
            parts.push(self.object_name(*p)?);
        }
        Ok(parts.join("."))
    }

    /// Full path of export `i`.
    pub fn export_path(&self, i: usize) -> Result<String> {
        self.object_path(self.export_ref(i)?)
    }

    /// Full path of import `i`.
    pub fn import_path(&self, i: usize) -> Result<String> {
        self.object_path(self.import_ref(i)?)
    }

    /// Package index of export `i`.
    pub fn export_ref(&self, i: usize) -> Result<PackageIndex> {
        if i >= self.exports.len() {
            return Err(self.bad_ref_err(i64::try_from(i).unwrap_or(i64::MAX), "export"));
        }
        PackageIndex::from_export(i).ok_or_else(|| self.bad_ref_err(i64::MAX, "export"))
    }

    /// Package index of import `i`.
    pub fn import_ref(&self, i: usize) -> Result<PackageIndex> {
        if i >= self.imports.len() {
            return Err(self.bad_ref_err(i64::try_from(i).unwrap_or(i64::MAX), "import"));
        }
        PackageIndex::from_import(i).ok_or_else(|| self.bad_ref_err(i64::MAX, "import"))
    }

    fn bad_ref_err(&self, i: i64, what: &str) -> Ue3Error {
        Ue3Error::BadPackageIndex {
            index: i32::try_from(i).unwrap_or(i32::MAX),
            imports: self.imports.len(),
            exports: self.exports.len(),
            context: format!("{what} table index"),
        }
    }

    /// Name of the outermost object of a reference (e.g. `Engine` for
    /// `Engine.PlayerStart`), or `None` for null.
    pub fn root_name(&self, idx: PackageIndex) -> Result<Option<String>> {
        let chain = self.outer_chain(idx)?;
        match chain.last() {
            Some(root) => Ok(Some(self.object_name(*root)?)),
            None => Ok(None),
        }
    }

    /// Class name of the object a reference points to: the import's
    /// `ClassName`, or the export's class (`Class` when `ClassIndex == 0`).
    pub fn class_name(&self, idx: PackageIndex) -> Result<String> {
        Ok(match self.resolve(idx)? {
            ObjectRef::Null => "None".to_owned(),
            ObjectRef::Import(_, imp) => self.fname(imp.class_name),
            ObjectRef::Export(i, _) => self.export_class_name(i)?,
        })
    }

    /// Class name of export `i` (`Class` when `ClassIndex == 0`).
    pub fn export_class_name(&self, i: usize) -> Result<String> {
        let exp = self.export(i)?;
        if exp.class_index.is_null() {
            return Ok("Class".to_owned());
        }
        self.object_name(exp.class_index)
    }

    /// Outermost package of export `i`'s class: e.g. `Engine` for an imported
    /// `Engine.PlayerStart`, `Core` when `ClassIndex == 0`. `None` when the
    /// class is a top-level export of this package itself (the package name is
    /// then the file name, which the bytes do not record).
    pub fn export_class_package(&self, i: usize) -> Result<Option<String>> {
        let exp = self.export(i)?;
        if exp.class_index.is_null() {
            return Ok(Some("Core".to_owned()));
        }
        match self.resolve(exp.class_index)? {
            ObjectRef::Export(_, class_exp) if class_exp.outer_index.is_null() => Ok(None),
            _ => {
                let chain = self.outer_chain(exp.class_index)?;
                // The class itself is chain[0]; its package is the outermost entry.
                if chain.len() < 2 {
                    return Ok(None);
                }
                match chain.last() {
                    Some(root) => Ok(Some(self.object_name(*root)?)),
                    None => Ok(None),
                }
            }
        }
    }

    /// Export entry `i`.
    pub fn export(&self, i: usize) -> Result<&ExportEntry> {
        self.exports
            .get(i)
            .ok_or_else(|| self.bad_ref_err(i64::try_from(i).unwrap_or(i64::MAX), "export"))
    }

    /// Import entry `i`.
    pub fn import(&self, i: usize) -> Result<&ImportEntry> {
        self.imports
            .get(i)
            .ok_or_else(|| self.bad_ref_err(i64::try_from(i).unwrap_or(i64::MAX), "import"))
    }

    /// True when `ancestor` appears in the outer chain of `idx` (excluding `idx` itself).
    pub fn is_inside(&self, idx: PackageIndex, ancestor: PackageIndex) -> Result<bool> {
        Ok(self
            .outer_chain(idx)?
            .iter()
            .skip(1)
            .any(|p| *p == ancestor))
    }

    /// Zero-based indices of exports whose outer is null.
    pub fn top_level_exports(&self) -> impl Iterator<Item = usize> + '_ {
        self.exports
            .iter()
            .enumerate()
            .filter(|(_, e)| e.outer_index.is_null())
            .map(|(i, _)| i)
    }

    /// Zero-based indices of exports with the given display name.
    pub fn find_exports(&self, name: &str) -> Vec<usize> {
        self.exports
            .iter()
            .enumerate()
            .filter(|(_, e)| self.fname(e.object_name) == name)
            .map(|(i, _)| i)
            .collect()
    }

    /// Serialized payload of export `i`, bounds-checked against the stream.
    pub fn export_data(&self, i: usize) -> Result<&[u8]> {
        let exp = self.export(i)?;
        match serial_range(exp, self.stream.len()) {
            Ok(Some((start, end))) => Ok(&self.stream[start..end]),
            Ok(None) => Ok(&[]),
            Err(msg) => Err(Ue3Error::OutOfBounds {
                what: format!("export {i} payload ({msg})"),
                offset: u64::try_from(exp.serial_offset).unwrap_or(0),
                size: u64::try_from(exp.serial_size).unwrap_or(0),
                len: self.stream.len(),
            }),
        }
    }

    /// Number of exports per class name.
    pub fn class_census(&self) -> Result<BTreeMap<String, usize>> {
        let mut out = BTreeMap::new();
        for i in 0..self.exports.len() {
            *out.entry(self.export_class_name(i)?).or_insert(0) += 1;
        }
        Ok(out)
    }
}

/// Check that the export payloads tile `[header_end, stream_len)` exactly (no
/// gaps, no overlaps), as they do in every shipped package. Deviations are
/// reported as warnings.
fn payload_tiling(exports: &[ExportEntry], header_end: usize, stream_len: usize) -> Vec<Issue> {
    let mut ranges: Vec<(usize, usize)> = exports
        .iter()
        .filter_map(|e| serial_range(e, stream_len).ok().flatten())
        .collect();
    if ranges.is_empty() {
        return Vec::new();
    }
    ranges.sort_unstable();
    let mut out = Vec::new();
    let (mut gaps, mut gap_bytes, mut overlaps) = (0usize, 0usize, 0usize);
    let mut pos = header_end;
    for &(start, end) in &ranges {
        if start > pos {
            gaps += 1;
            gap_bytes += start - pos;
        } else if start < pos {
            overlaps += 1;
        }
        pos = pos.max(end);
    }
    if gaps > 0 {
        out.push(Issue::warning(format!(
            "export payloads leave {gaps} gaps ({gap_bytes} bytes) after TotalHeaderSize"
        )));
    }
    if overlaps > 0 {
        out.push(Issue::warning(format!(
            "{overlaps} export payloads overlap a preceding payload or the header"
        )));
    }
    if pos != stream_len {
        out.push(Issue::warning(format!(
            "export payloads end at {pos} but the stream is {stream_len} bytes"
        )));
    }
    out
}

/// Validated `[start, end)` payload range of an export; `Ok(None)` when it has
/// no payload.
fn serial_range(
    e: &ExportEntry,
    len: usize,
) -> std::result::Result<Option<(usize, usize)>, String> {
    if e.serial_size == 0 {
        return Ok(None);
    }
    let size = usize::try_from(e.serial_size)
        .map_err(|_| format!("negative serial size {}", e.serial_size))?;
    let start = usize::try_from(e.serial_offset)
        .map_err(|_| format!("negative serial offset {}", e.serial_offset))?;
    match start.checked_add(size) {
        Some(end) if end <= len => Ok(Some((start, end))),
        _ => Err(format!(
            "serial range {start}+{size} beyond stream length {len}"
        )),
    }
}
