//! StaticMesh native-data decoder against hostile input: truncation at every
//! offset, trailing bytes, bit flips, extreme counts and a deterministic fuzz
//! loop over synthetic payloads (written in this file; no game data). The
//! decoder must never panic, hang or allocate beyond the input's size.

#![allow(clippy::unwrap_used)]

use asamu_ue3::PackageIndex;
use asamu_ue3::bulkdata::BulkDataRecord;
use asamu_ue3::staticmesh::{
    BoxSphereBounds, CollisionTriangle, ColorBuffer, CompactKdopNode, FragmentRange, KdopBounds,
    KdopTree, LodModel, MAX_LODS, MeshSection, PackedNormal, PositionBuffer, StaticMeshNative,
    ValidationContext, VertexBuffer, decode_static_mesh_native, encode_static_mesh_native,
    validate_static_mesh, vertex_element_size,
};
use asamu_ue3::types::Guid;

fn lod(num_uv: u32, colors: bool) -> LodModel {
    let n = 5usize;
    LodModel {
        raw_triangles: BulkDataRecord {
            flags: 0,
            element_count: 0,
            size_on_disk: 0,
            offset_in_file: 0,
            header_offset: 0,
        },
        sections: vec![MeshSection {
            material: PackageIndex(-1),
            enable_collision: 1,
            old_enable_collision: 1,
            enable_shadow_casting: 1,
            first_index: 0,
            num_triangles: 3,
            min_vertex_index: 0,
            max_vertex_index: 4,
            material_index: 0,
            fragments: vec![FragmentRange {
                base_index: 0,
                num_primitives: 3,
            }],
        }],
        positions: PositionBuffer {
            stride: 12,
            num_vertices: n as u32,
            positions: (0..n).map(|i| [i as f32, (i * 2) as f32, 1.0]).collect(),
        },
        vertices: VertexBuffer {
            num_tex_coords: num_uv,
            stride: vertex_element_size(num_uv, false).unwrap() as u32,
            num_vertices: n as u32,
            full_precision_uvs: false,
            tangent_x: vec![PackedNormal([255, 128, 128, 128]); n],
            tangent_z: vec![PackedNormal([128, 128, 255, 255]); n],
            uvs: (0..num_uv)
                .map(|_| (0..n).map(|i| [i as f32 * 0.5, 0.25]).collect())
                .collect(),
        },
        colors: ColorBuffer {
            stride: if colors { 4 } else { 0 },
            num_vertices: if colors { n as u32 } else { 0 },
            colors_bgra: if colors {
                vec![[9, 8, 7, 6]; n]
            } else {
                vec![]
            },
        },
        num_vertices: n as u32,
        indices: vec![0, 1, 2, 2, 3, 4, 4, 1, 0],
        wireframe_indices: vec![0, 1],
        adjacency_indices: vec![1; 36],
    }
}

fn sample() -> StaticMeshNative {
    StaticMeshNative {
        start: 0,
        bounds: BoxSphereBounds {
            origin: [2.0, 4.0, 1.0],
            box_extent: [2.0, 4.0, 0.0],
            sphere_radius: 5.0,
        },
        body_setup: PackageIndex(1),
        kdop: KdopTree {
            root_bounds: KdopBounds {
                min: [0.0; 3],
                max: [4.0, 8.0, 1.0],
            },
            nodes: vec![
                CompactKdopNode {
                    bytes: [0, 1, 2, 3, 4, 5]
                };
                2
            ],
            triangles: vec![
                CollisionTriangle {
                    vertices: [0, 1, 2],
                    material_index: 0,
                };
                3
            ],
        },
        internal_version: 18,
        source_data: Some(Box::new(lod(1, true))),
        optimization_settings: vec![],
        has_been_simplified: 0,
        is_mesh_proxy: 0,
        lods: vec![lod(2, false), lod(1, true)],
        lod_info_count: 2,
        thumbnail_angle: [0, 0, 0],
        thumbnail_distance: 10.0,
        high_res_source_mesh_name: "x".to_owned(),
        high_res_source_mesh_crc: 7,
        lighting_guid: Guid {
            a: 1,
            b: 1,
            c: 1,
            d: 1,
        },
        vertex_position_version: 0,
        cached_streaming_texture_factors: vec![0.5; 4],
        remove_degenerates: 1,
        per_lod_static_lighting_for_instancing: 0,
        console_prealloc_instance_count: 0,
    }
}

fn exercise(bytes: &[u8]) {
    // Decoding must not panic; when it succeeds, validation must not either.
    if let Ok(n) = decode_static_mesh_native(bytes, 0) {
        let _ = validate_static_mesh(
            &n,
            &ValidationContext {
                payload_stream_offset: Some(0),
                imports: 4,
                exports: 4,
            },
        );
        let _ = encode_static_mesh_native(&n);
    }
}

#[test]
fn every_truncation_is_rejected() {
    let bytes = encode_static_mesh_native(&sample()).unwrap();
    assert!(decode_static_mesh_native(&bytes, 0).is_ok());
    for cut in 0..bytes.len() {
        assert!(
            decode_static_mesh_native(&bytes[..cut], 0).is_err(),
            "truncation at {cut} of {} accepted",
            bytes.len()
        );
    }
}

#[test]
fn trailing_bytes_are_rejected() {
    let mut bytes = encode_static_mesh_native(&sample()).unwrap();
    bytes.push(0);
    assert!(decode_static_mesh_native(&bytes, 0).is_err());
}

#[test]
fn bit_flips_and_extreme_values_never_panic() {
    let bytes = encode_static_mesh_native(&sample()).unwrap();
    for i in 0..bytes.len() {
        for bit in [0x01u8, 0x80] {
            let mut b = bytes.clone();
            b[i] ^= bit;
            exercise(&b);
        }
    }
    for i in 0..bytes.len().saturating_sub(3) {
        for v in [i32::MAX, i32::MIN, -1, 0x7fff, 65_536, 1 << 24] {
            let mut b = bytes.clone();
            b[i..i + 4].copy_from_slice(&v.to_le_bytes());
            exercise(&b);
        }
    }
}

/// Individual fields patched to values the format does not allow. Offsets come
/// from the documented layout (checked by `layout_size_model_matches_the_encoder`).
#[test]
fn malformed_fields_are_rejected() {
    let good = encode_static_mesh_native(&sample()).unwrap();
    let at = find_vertex_header(&good, &sample());
    // Unsupported UV channel counts (the encoder refuses them, so patch bytes).
    for bad in [0u32, 5, u32::MAX] {
        let mut bytes = good.clone();
        bytes[at..at + 4].copy_from_slice(&bad.to_le_bytes());
        assert!(decode_static_mesh_native(&bytes, 0).is_err(), "{bad} UVs");
    }
    // bUseFullPrecisionUVs must be 0 or 1.
    let mut bytes = good.clone();
    bytes[at + 12..at + 16].copy_from_slice(&2u32.to_le_bytes());
    assert!(decode_static_mesh_native(&bytes, 0).is_err());
    // Bulk element size must match the vertex format.
    let mut bytes = good.clone();
    bytes[at + 16..at + 20].copy_from_slice(&13i32.to_le_bytes());
    assert!(decode_static_mesh_native(&bytes, 0).is_err());
    // A huge vertex count is rejected before allocating.
    let mut bytes = good.clone();
    bytes[at + 20..at + 24].copy_from_slice(&i32::MAX.to_le_bytes());
    assert!(decode_static_mesh_native(&bytes, 0).is_err());
    // Platform data flag on a section.
    let mut n = sample();
    n.lods.truncate(1);
    n.source_data = None;
    let bytes = encode_static_mesh_native(&n).unwrap();
    let flag_at = find_platform_flag(&bytes, &n);
    let mut b = bytes.clone();
    b[flag_at] = 1;
    assert!(decode_static_mesh_native(&b, 0).is_err());
    // bHasSourceData must be 0 or 1.
    let src_at = 28 + 4 + 24 + 8 + 6 * 2 + 8 + 8 * 3 + 4;
    let mut b = bytes.clone();
    assert_eq!(b[src_at..src_at + 4], [0, 0, 0, 0]);
    b[src_at..src_at + 4].copy_from_slice(&7u32.to_le_bytes());
    assert!(decode_static_mesh_native(&b, 0).is_err());
    // Negative inline raw-triangle size.
    let mut n2 = n.clone();
    n2.lods[0].raw_triangles.size_on_disk = 0;
    let bytes = encode_static_mesh_native(&n2).unwrap();
    let lod_at = src_at + 4 + 4 + 8 + 4;
    let mut b = bytes.clone();
    b[lod_at + 8..lod_at + 12].copy_from_slice(&(-5i32).to_le_bytes());
    assert!(decode_static_mesh_native(&b, 0).is_err());
    // LOD count above the sanity limit (enough bytes for the count check).
    let count_at = src_at + 4 + 4 + 8;
    let mut b = bytes.clone();
    b[count_at..count_at + 4].copy_from_slice(&((MAX_LODS + 1) as i32).to_le_bytes());
    b.resize(b.len() + MAX_LODS * 64, 0);
    assert!(decode_static_mesh_native(&b, 0).is_err());
}

/// Offset of LOD 0's vertex-buffer header (`NumTexCoords`) in the encoding
/// of `n`, computed from the documented layout.
fn find_vertex_header(bytes: &[u8], n: &StaticMeshNative) -> usize {
    let mut at = 28 + 4 + 24 + 8 + 6 * n.kdop.nodes.len() + 8 + 8 * n.kdop.triangles.len() + 4;
    at += 4;
    if let Some(src) = &n.source_data {
        at += lod_size(src);
    }
    at += 4 + 24 * n.optimization_settings.len() + 8 + 4;
    let l = &n.lods[0];
    at += 16 + raw_inline(l);
    at += sections_size(l);
    at += 8 + 8 + 12 * l.positions.positions.len();
    assert_eq!(
        u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap()),
        l.vertices.num_tex_coords
    );
    at
}

fn find_platform_flag(bytes: &[u8], n: &StaticMeshNative) -> usize {
    let at = 28 + 4 + 24 + 8 + 6 * n.kdop.nodes.len() + 8 + 8 * n.kdop.triangles.len() + 4 + 4;
    let at = at + 4 + 8 + 4 + 16 + 4;
    let s = &n.lods[0].sections[0];
    let flag = at + 36 + 4 + 8 * s.fragments.len();
    assert_eq!(bytes[flag], 0);
    flag
}

fn raw_inline(l: &LodModel) -> usize {
    if l.raw_triangles.flags & 1 == 0 {
        l.raw_triangles.size_on_disk.max(0) as usize
    } else {
        0
    }
}

fn sections_size(l: &LodModel) -> usize {
    4 + l
        .sections
        .iter()
        .map(|s| 36 + 4 + 8 * s.fragments.len() + 1)
        .sum::<usize>()
}

fn lod_size(l: &LodModel) -> usize {
    let vb = &l.vertices;
    let elem = vertex_element_size(vb.num_tex_coords, vb.full_precision_uvs).unwrap();
    16 + raw_inline(l)
        + sections_size(l)
        + 8
        + 8
        + 12 * l.positions.positions.len()
        + 16
        + 8
        + elem * vb.tangent_x.len()
        + 8
        + if l.colors.num_vertices != 0 {
            8 + 4 * l.colors.colors_bgra.len()
        } else {
            0
        }
        + 4
        + 3 * 8
        + 2 * (l.indices.len() + l.wireframe_indices.len() + l.adjacency_indices.len())
}

#[test]
fn layout_size_model_matches_the_encoder() {
    let n = sample();
    let bytes = encode_static_mesh_native(&n).unwrap();
    let head = 28 + 4 + 24 + 8 + 6 * n.kdop.nodes.len() + 8 + 8 * n.kdop.triangles.len() + 4;
    let src = 4 + n.source_data.as_deref().map_or(0, lod_size);
    let mid = 4 + 8 + 4 + n.lods.iter().map(lod_size).sum::<usize>();
    let tail = 4 + 12 + 4 + (4 + 2) + 4 + 16 + 4 + 4 + 16 + 12;
    assert_eq!(bytes.len(), head + src + mid + tail);
}

#[test]
fn deterministic_fuzz_never_panics() {
    let base = encode_static_mesh_native(&sample()).unwrap();
    let mut state = 0x9E37_79B9_7F4A_7C15u64;
    let mut next = move || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    for _ in 0..3000 {
        let mut b = base.clone();
        let edits = 1 + (next() % 6) as usize;
        for _ in 0..edits {
            let i = (next() % b.len() as u64) as usize;
            match next() % 4 {
                0 => b[i] = next() as u8,
                1 => {
                    let cut = i.max(1);
                    b.truncate(cut);
                }
                2 => {
                    if i + 4 <= b.len() {
                        b[i..i + 4].copy_from_slice(&(next() as i32).to_le_bytes());
                    }
                }
                _ => b.insert(i, next() as u8),
            }
            if b.is_empty() {
                break;
            }
        }
        exercise(&b);
    }
    // Pure noise.
    for len in [0usize, 1, 7, 64, 333, 4096] {
        let b: Vec<u8> = (0..len).map(|_| next() as u8).collect();
        exercise(&b);
    }
}

// ---------------------------------------------------------------------------
// Adversarial review additions (verify-meshes)
// ---------------------------------------------------------------------------

/// Offset just past the `HighResSourceMeshName` FString: the fields after it
/// have fixed sizes except the streaming-factor array.
fn high_res_name_end(bytes: &[u8], n: &StaticMeshNative) -> usize {
    bytes.len() - (4 + 16 + 4 + 4 + 4 * n.cached_streaming_texture_factors.len() + 12)
}

fn canonical_fstring(s: &str) -> Vec<u8> {
    let mut w = asamu_ue3::writer::Writer::new();
    assert!(w.fstring(s));
    w.into_bytes()
}

/// Every payload the decoder accepts re-encodes to exactly the same bytes,
/// except for the two places where the decoder normalises: inline
/// raw-triangle payload bytes (not kept) and a non-canonical FString
/// encoding of `HighResSourceMeshName` (UTF-16 for a Latin-1 string, or a
/// lone NUL for an empty one). Before the NaN-payload fix in `f32_to_half`
/// this failed on mutated UV halves.
#[test]
fn accepted_inputs_reencode_exactly() {
    let base = encode_static_mesh_native(&sample()).unwrap();
    let mut state = 0x2545_F491_4F6C_DD1Du64;
    let mut next = move || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    let (mut accepted, mut compared) = (0usize, 0usize);
    for _ in 0..20_000 {
        let mut b = base.clone();
        for _ in 0..1 + next() % 3 {
            let i = (next() % b.len() as u64) as usize;
            if next() % 2 == 0 {
                b[i] = next() as u8;
            } else {
                b[i] ^= 1 << (next() % 8);
            }
        }
        let Ok(n) = decode_static_mesh_native(&b, 0) else {
            continue;
        };
        accepted += 1;
        let inline_raw = n
            .lods
            .iter()
            .chain(n.source_data.as_deref())
            .any(|l| l.raw_triangles.has_inline_bytes() && l.raw_triangles.stored_len() > 0);
        let name = canonical_fstring(&n.high_res_source_mesh_name);
        let end = high_res_name_end(&b, &n);
        let canonical_name = end >= name.len() && b[end - name.len()..end] == name[..];
        if inline_raw || !canonical_name {
            continue;
        }
        compared += 1;
        let again = encode_static_mesh_native(&n).unwrap();
        assert_eq!(again, b, "accepted input does not re-encode exactly");
    }
    assert!(accepted > 2_000, "only {accepted} mutated inputs decoded");
    assert!(compared > 2_000, "only {compared} inputs compared");
}

/// UV halves with every special encoding (NaN payloads, infinities,
/// subnormals, negative zero) decode and re-encode bit for bit.
#[test]
fn special_uv_halves_round_trip() {
    let n = sample();
    let good = encode_static_mesh_native(&n).unwrap();
    let at = find_vertex_header(&good, &n);
    // Vertex data starts after NumTexCoords, Stride, NumVertices, bFull,
    // ElementSize, Count; the first UV half follows the two packed normals.
    let uv0 = at + 24 + 8;
    for h in [
        0x7c01u16, 0xfe55, 0x7c00, 0xfc00, 0x0001, 0x8000, 0x7bff, 0x7fff,
    ] {
        let mut b = good.clone();
        b[uv0..uv0 + 2].copy_from_slice(&h.to_le_bytes());
        b[uv0 + 2..uv0 + 4].copy_from_slice(&h.rotate_left(3).to_le_bytes());
        let d = decode_static_mesh_native(&b, 0).unwrap();
        assert_eq!(encode_static_mesh_native(&d).unwrap(), b, "half {h:#06x}");
        let _ = validate_static_mesh(&d, &ctx4());
    }
}

fn ctx_no_offset() -> ValidationContext {
    ValidationContext {
        payload_stream_offset: None,
        imports: 4,
        exports: 4,
    }
}

fn ctx4() -> ValidationContext {
    ValidationContext {
        payload_stream_offset: Some(0),
        imports: 4,
        exports: 4,
    }
}

/// Every bulk array insists on its element size: kDOP nodes (6), collision
/// triangles (8), positions (12), tangent/UV vertices (format-dependent),
/// colors (4) and the three index buffers (2).
#[test]
fn every_bulk_header_requires_its_element_size() {
    let mut n = sample();
    n.source_data = None;
    let good = encode_static_mesh_native(&n).unwrap();
    assert!(decode_static_mesh_native(&good, 0).is_ok());
    let nodes_hdr = 28 + 4 + 24;
    let tris_hdr = nodes_hdr + 8 + 6 * n.kdop.nodes.len();
    let at = find_vertex_header(&good, &n);
    let l = &n.lods[0];
    let nv = l.positions.positions.len();
    let pos_hdr = at - 12 * nv - 8;
    let elem = vertex_element_size(l.vertices.num_tex_coords, false).unwrap();
    let color_hdr = at + 24 + elem * nv; // stride, NumVertices, then the bulk header if any
    assert_eq!(
        l.colors.num_vertices, 0,
        "LOD 0 of the sample has no colors"
    );
    let index_hdr = color_hdr + 8 + 4;
    let wire_hdr = index_hdr + 8 + 2 * l.indices.len();
    let adj_hdr = wire_hdr + 8 + 2 * l.wireframe_indices.len();
    for (name, off, expected) in [
        ("kDOP nodes", nodes_hdr, 6i32),
        ("kDOP triangles", tris_hdr, 8),
        ("positions", pos_hdr, 12),
        ("vertices", at + 16, elem as i32),
        ("indices", index_hdr, 2),
        ("wireframe", wire_hdr, 2),
        ("adjacency", adj_hdr, 2),
    ] {
        assert_eq!(
            i32::from_le_bytes(good[off..off + 4].try_into().unwrap()),
            expected,
            "{name} header offset"
        );
        for bad in [0, -1, expected - 1, expected + 1, i32::MAX, i32::MIN] {
            let mut b = good.clone();
            b[off..off + 4].copy_from_slice(&bad.to_le_bytes());
            assert!(
                decode_static_mesh_native(&b, 0).is_err(),
                "{name}: element size {bad} accepted"
            );
        }
        // A count that does not fit the remaining bytes is refused.
        for bad in [i32::MAX, -1, (good.len() as i32) / expected + 1] {
            let mut b = good.clone();
            b[off + 4..off + 8].copy_from_slice(&bad.to_le_bytes());
            assert!(
                decode_static_mesh_native(&b, 0).is_err(),
                "{name}: count {bad} accepted"
            );
        }
    }
    // Colors: LOD 1 has them; patch its element size.
    let mut with_colors = n.clone();
    with_colors.lods.truncate(1);
    with_colors.lods[0] = lod(1, true);
    let good = encode_static_mesh_native(&with_colors).unwrap();
    let at = find_vertex_header(&good, &with_colors);
    let elem = vertex_element_size(1, false).unwrap();
    let color_bulk = at + 24 + elem * with_colors.lods[0].positions.positions.len() + 8;
    assert_eq!(&good[color_bulk..color_bulk + 4], &4i32.to_le_bytes());
    for bad in [0i32, 3, 5, -4] {
        let mut b = good.clone();
        b[color_bulk..color_bulk + 4].copy_from_slice(&bad.to_le_bytes());
        assert!(
            decode_static_mesh_native(&b, 0).is_err(),
            "color size {bad}"
        );
    }
}

/// The raw-triangle bulk-data record: unknown flag bits, two compression
/// bits, negative counts and inline sizes beyond the payload are refused;
/// an `Unused` record is accepted and skipped by validation.
#[test]
fn raw_triangle_records_are_checked() {
    let mut n = sample();
    n.source_data = None;
    let good = encode_static_mesh_native(&n).unwrap();
    // LOD 0's record follows bHasSourceData, the empty optimisation array,
    // two u32 and the LOD count.
    let rec = 28
        + 4
        + 24
        + 8
        + 6 * n.kdop.nodes.len()
        + 8
        + 8 * n.kdop.triangles.len()
        + 4
        + 4
        + 4
        + 8
        + 4;
    assert_eq!(&good[rec..rec + 16], &[0u8; 16]);
    let patch = |flags: u32, count: i32, size: i32, offset: i32| {
        let mut b = good.clone();
        b[rec..rec + 4].copy_from_slice(&flags.to_le_bytes());
        b[rec + 4..rec + 8].copy_from_slice(&count.to_le_bytes());
        b[rec + 8..rec + 12].copy_from_slice(&size.to_le_bytes());
        b[rec + 12..rec + 16].copy_from_slice(&offset.to_le_bytes());
        b
    };
    for (flags, count, size, offset) in [
        (0x40u32, 0, 0, 0),  // unknown bit
        (0x100, 0, 0, 0),    // unknown bit
        (0x12, 0, 0, 0),     // zlib + LZO
        (0, -1, 0, 0),       // negative count
        (0, 0, -1, 0),       // negative inline size
        (0, 0, i32::MAX, 0), // inline size beyond the payload
        (0x01, 1, -1, 0),    // separate file, negative size
        (0x01, 1, 16, -1),   // separate file, negative offset
    ] {
        assert!(
            decode_static_mesh_native(&patch(flags, count, size, offset), 0).is_err(),
            "record {flags:#x} {count} {size} {offset} accepted"
        );
    }
    // Unused (stripped) record: decodes, and validation does not apply the
    // inline-size rule to it.
    let b = patch(0x20, 5, 0, 0);
    let d = decode_static_mesh_native(&b, 0).unwrap();
    assert!(validate_static_mesh(&d, &ctx_no_offset()).is_empty());
    // Inline record with a payload: skipped, and validation checks its size.
    let mut m = n.clone();
    m.lods[0].raw_triangles.size_on_disk = 10;
    m.lods[0].raw_triangles.element_count = 1;
    let b = encode_static_mesh_native(&m).unwrap();
    let d = decode_static_mesh_native(&b, 0).unwrap();
    assert_eq!(d.lods[0].raw_triangles.size_on_disk, 10);
    assert!(
        validate_static_mesh(&d, &ctx4())
            .iter()
            .any(|i| i.contains("SizeOnDisk"))
    );
}

/// Array counts larger than the remaining bytes are refused before any
/// allocation: sections, fragments, LODs, optimisation settings, streaming
/// factors and the LOD info count.
#[test]
fn oversized_counts_are_refused() {
    let mut n = sample();
    n.source_data = None;
    let good = encode_static_mesh_native(&n).unwrap();
    let head = 28 + 4 + 24 + 8 + 6 * n.kdop.nodes.len() + 8 + 8 * n.kdop.triangles.len() + 4 + 4;
    let opt = head;
    let lods = head + 4 + 8;
    let sections = lods + 4 + 16;
    let fragments = sections + 4 + 36;
    let factors = good.len() - 12 - 4 * n.cached_streaming_texture_factors.len() - 4;
    // From the end: 3 x u32, factors, VertexPositionVersionNumber, LightingGuid,
    // CRC, the FString "x", ThumbnailDistance, ThumbnailAngle, then the count.
    let lod_info = good.len()
        - (12
            + 4 * n.cached_streaming_texture_factors.len()
            + 4
            + 4
            + 16
            + 4
            + (4 + 2)
            + 4
            + 12
            + 4);
    for (name, off, real) in [
        ("optimisation settings", opt, 0i32),
        ("LODs", lods, n.lods.len() as i32),
        ("sections", sections, 1),
        ("fragments", fragments, 1),
        ("streaming factors", factors, 4),
    ] {
        assert_eq!(
            i32::from_le_bytes(good[off..off + 4].try_into().unwrap()),
            real,
            "{name} offset"
        );
        for bad in [i32::MAX, i32::MIN, -1, 1 << 20] {
            let mut b = good.clone();
            b[off..off + 4].copy_from_slice(&bad.to_le_bytes());
            assert!(
                decode_static_mesh_native(&b, 0).is_err(),
                "{name}: count {bad} accepted"
            );
        }
    }
    // The LOD info elements carry no data on load, so only a negative count
    // is a parse error; any other mismatch is a validation issue.
    assert_eq!(
        i32::from_le_bytes(good[lod_info..lod_info + 4].try_into().unwrap()),
        2
    );
    for bad in [i32::MIN, -1] {
        let mut b = good.clone();
        b[lod_info..lod_info + 4].copy_from_slice(&bad.to_le_bytes());
        assert!(decode_static_mesh_native(&b, 0).is_err(), "LOD info {bad}");
    }
    let mut b = good.clone();
    b[lod_info..lod_info + 4].copy_from_slice(&i32::MAX.to_le_bytes());
    let d = decode_static_mesh_native(&b, 0).unwrap();
    assert!(
        validate_static_mesh(&d, &ctx_no_offset())
            .iter()
            .any(|i| i.contains("LOD info count"))
    );
    // A start offset past the payload is an error, not a panic.
    assert!(decode_static_mesh_native(&good, good.len() + 1).is_err());
    assert!(decode_static_mesh_native(&[], 0).is_err());
}

/// Validation never panics or overflows on extreme decoded values and
/// reports each inconsistency.
#[test]
fn validation_survives_extreme_values() {
    let base = sample();
    let ctx = ctx4();
    let check = |f: &dyn Fn(&mut StaticMeshNative), needle: &str| {
        let mut n = base.clone();
        f(&mut n);
        let issues = validate_static_mesh(&n, &ctx);
        assert!(
            issues.iter().any(|i| i.contains(needle)),
            "expected '{needle}' in {issues:?}"
        );
        // The same values survive the encoder and decoder where representable.
        if let Some(bytes) = encode_static_mesh_native(&n)
            && let Ok(d) = decode_static_mesh_native(&bytes, 0)
        {
            let _ = validate_static_mesh(&d, &ctx);
        }
    };
    check(
        &|n| {
            n.lods[0].sections[0].first_index = u32::MAX;
            n.lods[0].sections[0].num_triangles = u32::MAX;
        },
        "exceed",
    );
    check(
        &|n| {
            for s in &mut n.lods[0].sections {
                s.num_triangles = u32::MAX;
            }
        },
        "exceed",
    );
    check(
        &|n| {
            n.lods[0].sections[0].min_vertex_index = u32::MAX;
            n.lods[0].sections[0].max_vertex_index = 0;
        },
        "outside",
    );
    check(&|n| n.body_setup = PackageIndex(i32::MIN), "BodySetup");
    check(&|n| n.body_setup = PackageIndex(i32::MAX), "BodySetup");
    check(
        &|n| n.lods[0].sections[0].material = PackageIndex(i32::MIN),
        "material reference",
    );
    check(
        &|n| n.kdop.triangles[0].material_index = u16::MAX,
        "material index",
    );
    check(
        &|n| n.kdop.triangles[0].vertices = [u16::MAX; 3],
        "collision triangle 0",
    );
    check(
        &|n| n.lods[0].positions.positions[0] = [f32::NAN, 0.0, 0.0],
        "bounds box",
    );
    check(
        &|n| n.lods[0].positions.positions[0] = [f32::INFINITY, 0.0, 0.0],
        "bounds box",
    );
    check(&|n| n.bounds.origin = [f32::NAN; 3], "bounds box");
    check(
        &|n| {
            n.lods[0].positions.positions.clear();
            n.lods[0].positions.num_vertices = 0;
        },
        "index",
    );
    check(
        &|n| n.lods[0].adjacency_indices = vec![0; 5],
        "adjacency index count",
    );
    check(
        &|n| n.lods[0].vertices.num_vertices = u32::MAX,
        "vertex buffer",
    );
    check(&|n| n.lods[0].vertices.stride = u32::MAX, "vertex stride");
    check(
        &|n| n.lods[0].raw_triangles.element_count = i32::MAX,
        "raw triangles",
    );
    check(&|n| n.lod_info_count = u32::MAX, "LOD info count");
    check(&|n| n.lod_info_count = 0, "LOD info count");
}
