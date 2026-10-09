//! Compressed package bodies: chunk headers, block tables and the splice into a
//! single uncompressed stream.
//!
//! Each summary chunk entry points at an `FCompressedChunkInfo` header in the
//! file:
//!
//! ```text
//! i32 Tag (0x9E2A83C1) | i32 BlockSize | i32 CompressedSize | i32 UncompressedSize
//! ceil(UncompressedSize / BlockSize) x { i32 CompressedSize, i32 UncompressedSize }
//! compressed blocks, back to back
//! ```
//!
//! The uncompressed stream is rebuilt as: the summary re-serialized without its
//! chunk table (see [`Summary::to_uncompressed_bytes`]) followed by every
//! chunk's output at its `UncompressedOffset`.

use serde::Serialize;

use crate::error::{Result, Ue3Error};
use crate::issue::Issue;
use crate::lzo;
use crate::reader::Reader;
use crate::summary::{CompressedChunk, CompressionMethod, PACKAGE_TAG, Summary};

/// Largest accepted chunk `BlockSize`. Shipped packages use 128 KiB.
pub const MAX_BLOCK_SIZE: u32 = 16 * 1024 * 1024;

/// Largest accepted ratio of a block's uncompressed size to its compressed size.
/// LZO1X cannot expand by more than ~255x (a run-length match costs at least one
/// byte per 255 output bytes), so anything beyond this is malformed.
pub const MAX_EXPANSION_RATIO: u64 = 256;

/// Default cap on the size of the rebuilt uncompressed stream (2 GiB; package
/// offsets are `i32`, so a valid stream never exceeds this).
pub const DEFAULT_MAX_STREAM_SIZE: u64 = 1 << 31;

/// Largest tolerated gap between the re-serialized summary and the first chunk's
/// uncompressed offset (zero-filled, reported as a warning). Shipped packages
/// have no gap; a larger gap is treated as malformed.
pub const MAX_SUMMARY_GAP: u64 = 4096;

/// Size of the fixed part of a chunk header.
pub const CHUNK_HEADER_SIZE: usize = 16;

/// Size of one block-table entry.
pub const BLOCK_ENTRY_SIZE: usize = 8;

/// Upper bound on decompression worker threads, whatever [`ReadOptions::threads`]
/// asks for (a hostile package can declare thousands of tiny chunks).
pub const MAX_THREADS: usize = 64;

/// Options controlling decompression.
#[derive(Debug, Clone)]
pub struct ReadOptions {
    /// Reject packages whose uncompressed stream would exceed this many bytes.
    /// Enforced for compressed packages (declared stream length, before
    /// allocating) and for uncompressed ones (the file is the stream);
    /// [`crate::Package::open_with`] also refuses larger files before reading them.
    pub max_stream_size: u64,
    /// Worker threads for chunk decompression (`0` = use available parallelism,
    /// max 8; explicit values are capped at [`MAX_THREADS`]).
    pub threads: usize,
}

impl Default for ReadOptions {
    fn default() -> Self {
        ReadOptions {
            max_stream_size: DEFAULT_MAX_STREAM_SIZE,
            threads: 0,
        }
    }
}

/// One compressed block as described by a chunk's block table.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct BlockInfo {
    /// Compressed size in the file.
    pub compressed_size: u32,
    /// Size after decompression.
    pub uncompressed_size: u32,
    /// File offset of the compressed bytes.
    pub file_offset: u64,
}

/// A parsed and validated compressed chunk.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ChunkInfo {
    /// Index in the summary chunk table.
    pub index: usize,
    /// The summary entry.
    pub entry: CompressedChunk,
    /// Header tag.
    pub tag: u32,
    /// Header block size.
    pub block_size: u32,
    /// Header total compressed size (sum of block compressed sizes).
    pub compressed_size: u32,
    /// Header total uncompressed size.
    pub uncompressed_size: u32,
    /// Bytes taken by the header plus block table.
    pub header_size: u32,
    /// Block table.
    pub blocks: Vec<BlockInfo>,
}

/// Result of rebuilding the uncompressed stream of a compressed package.
#[derive(Debug, Clone)]
pub struct Decompressed {
    /// The uncompressed stream.
    pub stream: Vec<u8>,
    /// Validated chunk layouts.
    pub chunks: Vec<ChunkInfo>,
    /// Non-fatal findings.
    pub issues: Vec<Issue>,
}

fn bad(chunk: usize, reason: impl Into<String>) -> Ue3Error {
    Ue3Error::BadChunk {
        chunk,
        reason: reason.into(),
    }
}

/// Parse and validate the header and block table of chunk `index`.
pub fn parse_chunk(file: &[u8], index: usize, entry: &CompressedChunk) -> Result<ChunkInfo> {
    let start = u64::from(entry.compressed_offset);
    let size = u64::from(entry.compressed_size);
    let end = start + size; // both < 2^31, cannot overflow u64
    let file_len = file.len() as u64;
    if end > file_len {
        return Err(bad(
            index,
            format!("compressed range {start}+{size} exceeds file length {file_len}"),
        ));
    }
    // start/end <= file.len(), so they fit in usize.
    let region = &file[start as usize..end as usize];
    // Reader errors carry offsets relative to the chunk region; wrap them so
    // the message names the chunk and its file offset.
    let ctx = |e: Ue3Error| {
        bad(
            index,
            format!("header/block table at file offset {start}: {e}"),
        )
    };
    let mut r = Reader::new(region);
    let tag = r.read_u32().map_err(ctx)?;
    if tag != PACKAGE_TAG {
        return Err(bad(index, format!("chunk header tag {tag:#010x}")));
    }
    let block_size = r.read_non_negative("Chunk.BlockSize").map_err(ctx)?;
    let compressed_size = r.read_non_negative("Chunk.CompressedSize").map_err(ctx)?;
    let uncompressed_size = r.read_non_negative("Chunk.UncompressedSize").map_err(ctx)?;
    if block_size == 0 || block_size > MAX_BLOCK_SIZE {
        return Err(bad(index, format!("block size {block_size}")));
    }
    if uncompressed_size != entry.uncompressed_size {
        return Err(bad(
            index,
            format!(
                "header uncompressed size {uncompressed_size} != summary entry {}",
                entry.uncompressed_size
            ),
        ));
    }
    let block_count = usize::try_from(uncompressed_size.div_ceil(block_size))
        .map_err(|_| bad(index, "block count does not fit in usize"))?;
    let at = r.position();
    r.check_count("Chunk.Blocks", at, block_count, BLOCK_ENTRY_SIZE)
        .map_err(ctx)?;
    let mut raw = Vec::with_capacity(block_count);
    for _ in 0..block_count {
        let c = r.read_non_negative("Block.CompressedSize").map_err(ctx)?;
        let u = r.read_non_negative("Block.UncompressedSize").map_err(ctx)?;
        raw.push((c, u));
    }
    let header_size = r.position(); // <= region.len() <= i32::MAX
    let header_size_u32 =
        u32::try_from(header_size).map_err(|_| bad(index, "header size overflow"))?;

    let mut blocks = Vec::with_capacity(block_count);
    let mut sum_c: u64 = 0;
    let mut sum_u: u64 = 0;
    let data_start = start + header_size as u64;
    for (bi, &(c, u)) in raw.iter().enumerate() {
        if u > block_size {
            return Err(bad(
                index,
                format!("block {bi} uncompressed size {u} exceeds block size {block_size}"),
            ));
        }
        if u64::from(u) > u64::from(c) * MAX_EXPANSION_RATIO + MAX_EXPANSION_RATIO {
            return Err(bad(
                index,
                format!("block {bi} claims impossible expansion {c} -> {u}"),
            ));
        }
        blocks.push(BlockInfo {
            compressed_size: c,
            uncompressed_size: u,
            file_offset: data_start + sum_c,
        });
        sum_c += u64::from(c);
        sum_u += u64::from(u);
    }
    if sum_u != u64::from(uncompressed_size) {
        return Err(bad(
            index,
            format!("block uncompressed sizes sum to {sum_u}, header says {uncompressed_size}"),
        ));
    }
    if sum_c != u64::from(compressed_size) {
        return Err(bad(
            index,
            format!("block compressed sizes sum to {sum_c}, header says {compressed_size}"),
        ));
    }
    let needed = header_size as u64 + sum_c;
    if needed != size {
        return Err(bad(
            index,
            format!("header + blocks occupy {needed} bytes, summary entry says {size}"),
        ));
    }
    Ok(ChunkInfo {
        index,
        entry: *entry,
        tag,
        block_size,
        compressed_size,
        uncompressed_size,
        header_size: header_size_u32,
        blocks,
    })
}

/// Validate the chunk table of a compressed package and parse every chunk header.
/// Returns the chunk layouts and the re-serialized uncompressed summary.
pub fn plan(file: &[u8], summary: &Summary) -> Result<(Vec<ChunkInfo>, Vec<u8>, Vec<Issue>)> {
    let flags = summary.compression_flags;
    match summary.compression() {
        CompressionMethod::Lzo => {}
        CompressionMethod::None => {
            return Err(bad(0, "chunk table present but CompressionFlags is 0"));
        }
        other => {
            return Err(Ue3Error::UnsupportedCompression {
                flags,
                method: other.name(),
            });
        }
    }
    if summary.compressed_chunks.is_empty() {
        return Err(bad(0, "CompressionFlags set but the chunk table is empty"));
    }
    let header = summary.to_uncompressed_bytes()?;
    let mut issues = Vec::new();
    let mut chunks = Vec::with_capacity(summary.compressed_chunks.len());
    let mut prev_u_end: Option<u64> = None;
    let mut prev_c_end = summary.serialized_size as u64;
    for (i, entry) in summary.compressed_chunks.iter().enumerate() {
        let u_off = u64::from(entry.uncompressed_offset);
        let u_end = u_off + u64::from(entry.uncompressed_size);
        match prev_u_end {
            None => {
                let hl = header.len() as u64;
                if u_off < hl {
                    return Err(bad(
                        i,
                        format!(
                            "first chunk starts at {u_off}, inside the {hl}-byte uncompressed summary"
                        ),
                    ));
                }
                if u_off > hl + MAX_SUMMARY_GAP {
                    return Err(bad(
                        i,
                        format!(
                            "first chunk starts at {u_off}, far beyond the {hl}-byte uncompressed summary"
                        ),
                    ));
                }
                if u_off > hl {
                    issues.push(Issue::warning(format!(
                        "first chunk starts at {u_off} but the uncompressed summary is {hl} bytes; gap zero-filled"
                    )));
                }
            }
            Some(p) if p != u_off => {
                return Err(bad(
                    i,
                    format!("not contiguous: starts at {u_off}, previous chunk ends at {p}"),
                ));
            }
            Some(_) => {}
        }
        let c_off = u64::from(entry.compressed_offset);
        if c_off < prev_c_end {
            return Err(bad(
                i,
                format!(
                    "compressed data at {c_off} overlaps the summary or previous chunk (ends {prev_c_end})"
                ),
            ));
        }
        if c_off > prev_c_end {
            issues.push(Issue::warning(format!(
                "chunk {i}: {} unused file bytes before compressed offset {c_off}",
                c_off - prev_c_end
            )));
        }
        chunks.push(parse_chunk(file, i, entry)?);
        prev_u_end = Some(u_end);
        prev_c_end = c_off + u64::from(entry.compressed_size);
    }
    let file_len = file.len() as u64;
    if prev_c_end < file_len {
        issues.push(Issue::warning(format!(
            "{} trailing file bytes after the last chunk",
            file_len - prev_c_end
        )));
    }
    Ok((chunks, header, issues))
}

/// Decompress one chunk into `out` (exactly `chunk.uncompressed_size` bytes).
pub fn decompress_chunk(file: &[u8], chunk: &ChunkInfo, out: &mut [u8]) -> Result<()> {
    if out.len() as u64 != u64::from(chunk.uncompressed_size) {
        return Err(bad(chunk.index, "output region has the wrong size"));
    }
    let mut pos = 0usize;
    for (bi, b) in chunk.blocks.iter().enumerate() {
        let src_end = b.file_offset + u64::from(b.compressed_size);
        let src = usize::try_from(b.file_offset)
            .ok()
            .zip(usize::try_from(src_end).ok())
            .and_then(|(s, e)| file.get(s..e))
            .ok_or_else(|| bad(chunk.index, format!("block {bi} outside the file")))?;
        let want = usize::try_from(b.uncompressed_size)
            .map_err(|_| bad(chunk.index, "block size does not fit in usize"))?;
        let data = lzo::decompress(src, want).map_err(|source| Ue3Error::Lzo {
            chunk: chunk.index,
            block: bi,
            source,
        })?;
        if data.len() != want {
            return Err(bad(
                chunk.index,
                format!(
                    "block {bi} decompressed to {} bytes, expected {want}",
                    data.len()
                ),
            ));
        }
        let dst = pos
            .checked_add(want)
            .and_then(|end| out.get_mut(pos..end))
            .ok_or_else(|| bad(chunk.index, format!("block {bi} overruns the chunk output")))?;
        dst.copy_from_slice(&data);
        pos += want;
    }
    if pos != out.len() {
        return Err(bad(chunk.index, "blocks did not fill the chunk output"));
    }
    Ok(())
}

/// Rebuild the full uncompressed stream of a compressed package.
pub fn decompress_package(
    file: &[u8],
    summary: &Summary,
    opts: &ReadOptions,
) -> Result<Decompressed> {
    let (chunks, header, issues) = plan(file, summary)?;
    let stream_len = summary.uncompressed_stream_len().unwrap_or(0);
    if stream_len > opts.max_stream_size {
        return Err(Ue3Error::TooLarge {
            what: "uncompressed stream",
            size: stream_len,
            limit: opts.max_stream_size,
        });
    }
    let stream_len = usize::try_from(stream_len).map_err(|_| Ue3Error::TooLarge {
        what: "uncompressed stream",
        size: stream_len,
        limit: usize::MAX as u64,
    })?;
    let first_off = chunks
        .first()
        .map(|c| c.entry.uncompressed_offset as usize)
        .unwrap_or(stream_len);

    let mut stream = vec![0u8; stream_len];
    stream
        .get_mut(..header.len())
        .ok_or_else(|| bad(0, "uncompressed summary longer than the stream"))?
        .copy_from_slice(&header);

    // Split the chunk area into one disjoint output region per chunk.
    let mut regions: Vec<(&ChunkInfo, &mut [u8])> = Vec::with_capacity(chunks.len());
    let (_, mut rest) = stream
        .split_at_mut_checked(first_off)
        .ok_or_else(|| bad(0, "first chunk offset beyond the stream"))?;
    for c in &chunks {
        let n = c.entry.uncompressed_size as usize;
        let (region, tail) = rest
            .split_at_mut_checked(n)
            .ok_or_else(|| bad(c.index, "chunk output beyond the stream"))?;
        regions.push((c, region));
        rest = tail;
    }

    let threads = match opts.threads {
        0 => std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(1)
            .min(8),
        n => n.min(MAX_THREADS),
    }
    .clamp(1, regions.len().max(1));

    if threads <= 1 {
        for (c, region) in regions {
            decompress_chunk(file, c, region)?;
        }
    } else {
        let mut buckets: Vec<Vec<(&ChunkInfo, &mut [u8])>> =
            (0..threads).map(|_| Vec::new()).collect();
        for (i, item) in regions.into_iter().enumerate() {
            if let Some(b) = buckets.get_mut(i % threads) {
                b.push(item);
            }
        }
        let mut first_err: Option<(usize, Ue3Error)> = None;
        std::thread::scope(|s| {
            let mut handles = Vec::with_capacity(buckets.len());
            for bucket in buckets {
                // `Builder::spawn_scoped` reports thread-creation failure as an
                // error instead of panicking like `Scope::spawn`.
                let spawned = std::thread::Builder::new().spawn_scoped(
                    s,
                    move || -> std::result::Result<(), (usize, Ue3Error)> {
                        for (c, region) in bucket {
                            decompress_chunk(file, c, region).map_err(|e| (c.index, e))?;
                        }
                        Ok(())
                    },
                );
                match spawned {
                    Ok(h) => handles.push(h),
                    Err(e) => {
                        first_err = Some((
                            usize::MAX,
                            Ue3Error::Worker(format!("could not start a worker thread: {e}")),
                        ));
                        break;
                    }
                }
            }
            for h in handles {
                let res = h.join().unwrap_or_else(|_| {
                    Err((
                        usize::MAX,
                        Ue3Error::Worker("decompression worker panicked".to_owned()),
                    ))
                });
                if let Err((idx, e)) = res {
                    let replace = first_err.as_ref().is_none_or(|(cur, _)| idx < *cur);
                    if replace {
                        first_err = Some((idx, e));
                    }
                }
            }
        });
        if let Some((_, e)) = first_err {
            return Err(e);
        }
    }
    Ok(Decompressed {
        stream,
        chunks,
        issues,
    })
}
