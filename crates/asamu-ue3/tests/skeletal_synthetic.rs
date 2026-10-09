//! Synthetic and hostile-input tests for the SkeletalMesh native decoder.
//!
//! Every fixture is written byte by byte here (no game data). The hand-made
//! payload follows the documented v868 layout field by field, independently
//! of the library's encoder, which must reproduce it exactly.

#![allow(clippy::unwrap_used)]

use asamu_ue3::FName;
use asamu_ue3::bulkdata::BulkDataRecord;
use asamu_ue3::skeletal::{
    GpuSkinVertex, GpuSkinVertexBuffer, MeshBone, MultiSizeIndices, PerPolyBoneCollision,
    RigidSkinVertex, SkelChunk, SkelLodModel, SkelSection, SkeletalMeshNative, SoftSkinVertex,
    ValidationContext, VertexInfluence, VertexInfluences, decode_skeletal_mesh_native,
    encode_skeletal_mesh_native, gpu_vertex_size, skeleton_depth, validate_skeletal_mesh,
};
use asamu_ue3::staticmesh::{BoxSphereBounds, KdopBounds, PackedNormal};
use asamu_ue3::types::PackageIndex;

/// Little-endian byte sink.
#[derive(Default)]
struct W(Vec<u8>);

impl W {
    fn u8(&mut self, v: u8) {
        self.0.push(v);
    }
    fn u16(&mut self, v: u16) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    fn u32(&mut self, v: u32) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    fn i32(&mut self, v: i32) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    fn f32(&mut self, v: f32) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    fn vec3(&mut self, v: [f32; 3]) {
        for c in v {
            self.f32(c);
        }
    }
    fn bytes(&mut self, b: &[u8]) {
        self.0.extend_from_slice(b);
    }
}

/// Hand-written native tail: two bones, one LOD with one section, one chunk
/// holding one rigid and one soft source vertex, two GPU vertices with two
/// half UV channels, three indices, a name map, empty per-poly data, one
/// bone-break name, one clothing slot, four streaming factors, no source
/// data. Returned with the prefix offset (8 junk bytes stand in for the
/// tagged properties).
fn hand_made() -> (Vec<u8>, usize) {
    let mut w = W::default();
    w.bytes(&[0xAA; 8]);
    let start = w.0.len();
    // Bounds.
    w.vec3([1.0, 2.0, 3.0]);
    w.vec3([10.0, 20.0, 30.0]);
    w.f32(40.0);
    // Materials.
    w.i32(2);
    w.i32(-1);
    w.i32(0);
    // Origin, RotOrigin.
    w.vec3([0.5, 0.0, -0.5]);
    w.i32(0);
    w.i32(-16384);
    w.i32(0);
    // RefSkeleton: root + child.
    w.i32(2);
    for (name, children, parent, pos) in [(3, 1, 0, [0.0, 0.0, 0.0]), (4, 0, 0, [0.0, 0.0, 10.0])] {
        w.i32(name);
        w.i32(0);
        w.u32(0); // flags
        w.vec3([0.0, 0.0, 0.0]);
        w.f32(1.0); // quat w
        w.vec3(pos);
        w.i32(children);
        w.i32(parent);
        w.bytes(&[1, 2, 3, 255]);
    }
    w.i32(2); // SkeletalDepth
    // LODs.
    w.i32(1);
    // Sections.
    w.i32(1);
    w.u16(0);
    w.u16(0);
    w.u32(0);
    w.u32(1);
    w.u8(0);
    // Index container.
    w.u32(0);
    w.u8(2);
    w.i32(2);
    w.i32(3);
    w.u16(0);
    w.u16(1);
    w.u16(1);
    // ActiveBoneIndices.
    w.i32(2);
    w.u16(0);
    w.u16(1);
    // Chunks.
    w.i32(1);
    w.u32(0); // BaseVertexIndex
    w.i32(1); // rigid vertices
    w.vec3([0.0, 0.0, 0.0]);
    w.bytes(&[255, 128, 128, 0, 128, 255, 128, 0, 128, 128, 255, 255]);
    for _ in 0..8 {
        w.f32(0.25);
    }
    w.bytes(&[9, 9, 9, 9]);
    w.u8(0);
    w.i32(1); // soft vertices
    w.vec3([0.0, 0.0, 10.0]);
    w.bytes(&[255, 128, 128, 0, 128, 255, 128, 0, 128, 128, 255, 255]);
    for _ in 0..8 {
        w.f32(0.5);
    }
    w.bytes(&[9, 9, 9, 9]);
    w.bytes(&[1, 0, 0, 0]);
    w.bytes(&[200, 55, 0, 0]);
    w.i32(2); // bone map
    w.u16(0);
    w.u16(1);
    w.i32(1);
    w.i32(1);
    w.i32(2);
    // Size, NumVertices, RequiredBones.
    w.u32(0);
    w.u32(2);
    w.i32(2);
    w.u8(0);
    w.u8(1);
    // RawPointIndices: inline, uncompressed, two i32.
    w.u32(0);
    w.i32(2);
    w.i32(8);
    w.i32(1234);
    w.i32(7);
    w.i32(8);
    // NumTexCoords.
    w.u32(2);
    // Vertex buffer.
    w.u32(2);
    w.u32(0);
    w.u32(1); // bUsePackedPosition (stored only)
    w.vec3([1.0, 1.0, 1.0]);
    w.vec3([0.0, 0.0, 0.0]);
    w.i32(36);
    w.i32(2);
    for (p, bones, weights) in [
        ([0.0, 0.0, 0.0], [0u8, 0, 0, 0], [255u8, 0, 0, 0]),
        ([0.0, 0.0, 10.0], [1, 0, 0, 0], [200, 55, 0, 0]),
    ] {
        w.bytes(&[255, 128, 128, 0]);
        w.bytes(&[128, 128, 255, 255]);
        w.bytes(&bones);
        w.bytes(&weights);
        w.vec3(p);
        w.u16(0x3800); // 0.5
        w.u16(0x3c00); // 1.0
        w.u16(0x0000);
        w.u16(0xbc00); // -1.0
    }
    // Vertex influences (none).
    w.i32(0);
    // Adjacency container (empty, 16-bit).
    w.u32(0);
    w.u8(2);
    w.i32(2);
    w.i32(0);
    // NameIndexMap.
    w.i32(2);
    w.i32(3);
    w.i32(0);
    w.i32(0);
    w.i32(4);
    w.i32(0);
    w.i32(1);
    // PerPolyBoneKDOPs, BoneBreakNames, BoneBreakOptions, ClothingAssets.
    w.i32(0);
    w.i32(1);
    w.i32(5);
    w.bytes(b"Neck\0");
    w.i32(1);
    w.u8(2);
    w.i32(1);
    w.i32(0);
    // Streaming factors.
    w.i32(4);
    for f in [1.0, 2.0, 3.0, 4.0] {
        w.f32(f);
    }
    // bHaveSourceData.
    w.u32(0);
    (w.0, start)
}

#[test]
fn hand_made_payload_decodes_field_by_field_and_re_encodes() {
    let (data, start) = hand_made();
    let n = decode_skeletal_mesh_native(&data, start, false).unwrap();
    assert_eq!(n.bounds.origin, [1.0, 2.0, 3.0]);
    assert_eq!(n.bounds.sphere_radius, 40.0);
    assert_eq!(n.materials, vec![PackageIndex(-1), PackageIndex(0)]);
    assert_eq!(n.origin, [0.5, 0.0, -0.5]);
    assert_eq!(n.rot_origin, [0, -16384, 0]);
    assert_eq!(n.ref_skeleton.len(), 2);
    assert_eq!(
        n.ref_skeleton[1].name,
        FName {
            index: 4,
            number: 0
        }
    );
    assert_eq!(n.ref_skeleton[1].position, [0.0, 0.0, 10.0]);
    assert_eq!(n.ref_skeleton[0].num_children, 1);
    assert_eq!(n.ref_skeleton[1].bone_color, [1, 2, 3, 255]);
    assert_eq!(n.skeletal_depth, 2);
    let lod = &n.lods[0];
    assert_eq!(lod.sections[0].num_triangles, 1);
    assert_eq!(lod.indices.indices, vec![0, 1, 1]);
    assert_eq!(lod.chunks[0].rigid_vertices.len(), 1);
    assert_eq!(
        lod.chunks[0].soft_vertices[0].influence_weights,
        [200, 55, 0, 0]
    );
    assert_eq!(lod.chunks[0].max_bone_influences, 2);
    assert_eq!(lod.raw_point_indices, vec![7, 8]);
    assert_eq!(lod.num_tex_coords, 2);
    let vb = &lod.vertex_buffer;
    assert!(vb.use_packed_position);
    assert_eq!(vb.vertices.len(), 2);
    assert_eq!(vb.vertices[1].position, [0.0, 0.0, 10.0]);
    assert_eq!(vb.vertices[0].uvs[0], [0.5, 1.0]);
    assert_eq!(vb.vertices[0].uvs[1], [0.0, -1.0]);
    assert_eq!(vb.vertices[1].influence_bones, [1, 0, 0, 0]);
    assert!(lod.colors.is_none());
    assert_eq!(n.name_index_map.len(), 2);
    assert_eq!(n.bone_break_names, vec!["Neck".to_owned()]);
    assert_eq!(n.bone_break_options, vec![2]);
    assert_eq!(n.clothing_assets, vec![PackageIndex(0)]);
    assert_eq!(n.cached_streaming_texture_factors, vec![1.0, 2.0, 3.0, 4.0]);
    assert!(n.source_data.is_none());
    // The encoder reproduces the hand-made bytes exactly.
    assert_eq!(
        encode_skeletal_mesh_native(&n, false).unwrap(),
        data[start..]
    );
    // Structure is consistent (raw point offset checked against a stream
    // position: payload at 1234 - (start + 8 header bytes...)).
    let ctx = ValidationContext {
        payload_stream_offset: None,
        imports: 1,
        exports: 1,
    };
    let names = vec!["Root".to_owned(), "Child".to_owned()];
    assert_eq!(
        validate_skeletal_mesh(&n, Some(&names), &ctx),
        Vec::<String>::new()
    );
    assert_eq!(skeleton_depth(&n.ref_skeleton), Some(2));
}

#[test]
fn colors_select_the_color_buffer() {
    let (data, start) = hand_made();
    // Decoding the same bytes as if the mesh had vertex colors reads the
    // vertex-influence count as a color bulk header and fails.
    assert!(decode_skeletal_mesh_native(&data, start, true).is_err());
}

fn packed(b: [u8; 4]) -> PackedNormal {
    PackedNormal(b)
}

fn rich_lod(full_precision: bool, wide: bool) -> SkelLodModel {
    let vertex = |p: [f32; 3]| GpuSkinVertex {
        tangent_x: packed([255, 128, 128, 0]),
        tangent_z: packed([128, 128, 255, 255]),
        influence_bones: [0, 1, 0, 0],
        influence_weights: [128, 127, 0, 0],
        position: p,
        uvs: [[0.125, 0.75], [1.5, -2.0], [3.0, 0.0], [0.0; 2]],
    };
    let chunk = SkelChunk {
        base_vertex_index: 0,
        rigid_vertices: vec![RigidSkinVertex {
            position: [1.0, 2.0, 3.0],
            tangents: [packed([1, 2, 3, 4]); 3],
            uvs: [[0.5; 2]; 4],
            color: [5, 6, 7, 8],
            bone: 1,
        }],
        soft_vertices: vec![
            SoftSkinVertex {
                position: [4.0, 5.0, 6.0],
                tangents: [packed([9, 10, 11, 12]); 3],
                uvs: [[0.25; 2]; 4],
                color: [1, 1, 1, 1],
                influence_bones: [0, 1, 0, 0],
                influence_weights: [128, 127, 0, 0],
            };
            2
        ],
        bone_map: vec![0, 1],
        num_rigid_vertices: 1,
        num_soft_vertices: 2,
        max_bone_influences: 2,
    };
    SkelLodModel {
        sections: vec![SkelSection {
            material_index: 0,
            chunk_index: 0,
            base_index: 0,
            num_triangles: 1,
            triangle_sorting: 3,
        }],
        indices: MultiSizeIndices {
            needs_cpu_access: 1,
            data_type_size: if wide { 4 } else { 2 },
            indices: vec![0, 1, 2],
        },
        active_bone_indices: vec![0, 1],
        chunks: vec![chunk.clone()],
        size: 0,
        num_vertices: 3,
        required_bones: vec![0, 1],
        raw_point_indices_record: BulkDataRecord {
            flags: 0,
            element_count: 3,
            size_on_disk: 12,
            offset_in_file: 0,
            header_offset: 0,
        },
        raw_point_indices: vec![0, 1, 2],
        num_tex_coords: 3,
        vertex_buffer: GpuSkinVertexBuffer {
            num_tex_coords: 3,
            use_full_precision_uvs: full_precision,
            use_packed_position: false,
            mesh_extension: [1.0; 3],
            mesh_origin: [0.0; 3],
            vertices: vec![
                vertex([0.0; 3]),
                vertex([1.0, 0.0, 0.0]),
                vertex([0.0, 1.0, 0.0]),
            ],
        },
        colors: Some(vec![[1, 2, 3, 4], [5, 6, 7, 8], [9, 10, 11, 12]]),
        vertex_influences: vec![VertexInfluences {
            influences: vec![VertexInfluence {
                weights: [255, 0, 0, 0],
                bones: [1, 0, 0, 0],
            }],
            mapping: vec![([0, 1], vec![0, 2])],
            sections: vec![SkelSection {
                material_index: 0,
                chunk_index: 0,
                base_index: 0,
                num_triangles: 1,
                triangle_sorting: 0,
            }],
            chunks: vec![chunk],
            required_bones: vec![0, 1],
            usage: 1,
        }],
        adjacency: MultiSizeIndices {
            needs_cpu_access: 0,
            data_type_size: 2,
            indices: (0..12).map(|i| i % 3).collect(),
        },
    }
}

fn rich_mesh() -> SkeletalMeshNative {
    let bone = |idx: i32, parent: i32, children: i32| MeshBone {
        name: FName {
            index: idx,
            number: 0,
        },
        flags: 0,
        orientation: [0.0, 0.0, 0.0, 1.0],
        position: [0.0, 0.0, idx as f32],
        num_children: children,
        parent_index: parent,
        bone_color: [0; 4],
    };
    SkeletalMeshNative {
        start: 0,
        bounds: BoxSphereBounds {
            origin: [0.0; 3],
            box_extent: [10.0; 3],
            sphere_radius: 20.0,
        },
        materials: vec![PackageIndex(0)],
        origin: [0.0; 3],
        rot_origin: [0; 3],
        ref_skeleton: vec![bone(0, 0, 1), bone(1, 0, 0)],
        skeletal_depth: 2,
        lods: vec![rich_lod(false, false), rich_lod(true, true)],
        name_index_map: vec![
            (
                FName {
                    index: 0,
                    number: 0,
                },
                0,
            ),
            (
                FName {
                    index: 1,
                    number: 0,
                },
                1,
            ),
        ],
        per_poly_bone_kdops: vec![PerPolyBoneCollision {
            root_bounds: KdopBounds {
                min: [-1.0; 3],
                max: [1.0; 3],
            },
            nodes: vec![[1, 2, 3, 4, 5, 6]],
            triangles: vec![[0, 1, 2, 0]],
            vertices: vec![[0.0; 3], [1.0; 3], [2.0; 3]],
        }],
        bone_break_names: vec!["A".to_owned(), "Ünïcode".to_owned(), "日本".to_owned()],
        bone_break_options: vec![0, 1],
        clothing_assets: vec![PackageIndex(0), PackageIndex(1)],
        cached_streaming_texture_factors: vec![0.5],
        source_data: Some(Box::new(rich_lod(false, false))),
    }
}

#[test]
fn every_optional_part_round_trips() {
    let n = rich_mesh();
    let bytes = encode_skeletal_mesh_native(&n, true).unwrap();
    let back = decode_skeletal_mesh_native(&bytes, 0, true).unwrap();
    assert_eq!(back.lods[1].vertex_buffer.vertices[0].uvs[1], [1.5, -2.0]);
    assert_eq!(back.lods[1].indices.data_type_size, 4);
    assert_eq!(back.per_poly_bone_kdops, n.per_poly_bone_kdops);
    assert_eq!(back.lods[0].vertex_influences, n.lods[0].vertex_influences);
    assert_eq!(back.bone_break_names, n.bone_break_names);
    assert_eq!(
        back.source_data.as_ref().map(|l| &l.vertex_buffer),
        n.source_data.as_ref().map(|l| &l.vertex_buffer)
    );
    // Fields equal up to the record's header offset (positional).
    assert_eq!(encode_skeletal_mesh_native(&back, true).unwrap(), bytes);
    let ctx = ValidationContext {
        payload_stream_offset: None,
        imports: 1,
        exports: 2,
    };
    let issues = validate_skeletal_mesh(&back, None, &ctx);
    assert!(issues.is_empty(), "{issues:?}");
    // Without the colors flag the encoder refuses (colors present).
    assert!(encode_skeletal_mesh_native(&n, false).is_none());
}

#[test]
fn gpu_vertex_sizes() {
    assert_eq!(gpu_vertex_size(1, false), Some(32));
    assert_eq!(gpu_vertex_size(2, false), Some(36));
    assert_eq!(gpu_vertex_size(3, false), Some(40));
    assert_eq!(gpu_vertex_size(1, true), Some(36));
    assert_eq!(gpu_vertex_size(4, true), Some(60));
    assert_eq!(gpu_vertex_size(0, false), None);
    assert_eq!(gpu_vertex_size(5, false), None);
}

#[test]
fn validation_reports_each_inconsistency() {
    let ctx = ValidationContext {
        payload_stream_offset: None,
        imports: 1,
        exports: 2,
    };
    let check = |f: &dyn Fn(&mut SkeletalMeshNative), needle: &str| {
        let mut n = rich_mesh();
        f(&mut n);
        let issues = validate_skeletal_mesh(&n, None, &ctx);
        assert!(
            issues.iter().any(|m| m.contains(needle)),
            "expected '{needle}' in {issues:?}"
        );
    };
    check(
        &|n| n.materials[0] = PackageIndex(99),
        "material 0 reference",
    );
    check(
        &|n| n.ref_skeleton[1].parent_index = 1,
        "not an earlier bone",
    );
    check(&|n| n.ref_skeleton[0].num_children = 3, "NumChildren");
    check(
        &|n| n.ref_skeleton[0].orientation = [0.0, 0.0, 0.0, 2.0],
        "orientation length",
    );
    check(&|n| n.skeletal_depth = 5, "SkeletalDepth");
    check(&|n| n.name_index_map[0].1 = 1, "NameIndexMap");
    check(&|n| n.lods[0].indices.indices[2] = 7, "index 7");
    check(&|n| n.lods[0].sections[0].chunk_index = 4, "chunk 4");
    check(&|n| n.lods[0].sections[0].num_triangles = 9, "exceed");
    check(
        &|n| n.lods[0].chunks[0].base_vertex_index = 1,
        "starts at vertex 1",
    );
    check(&|n| n.lods[0].chunks[0].bone_map[1] = 9, "bone map entry 9");
    check(
        &|n| n.lods[0].chunks[0].max_bone_influences = 0,
        "MaxBoneInfluences",
    );
    check(
        &|n| n.lods[0].vertex_buffer.vertices[0].influence_bones[1] = 5,
        "outside its bone map",
    );
    check(&|n| n.lods[0].num_vertices = 4, "NumVertices 4");
    check(&|n| n.lods[0].colors = Some(vec![[0; 4]]), "1 colors");
    check(
        &|n| n.lods[0].adjacency.indices.pop().map_or((), |_| ()),
        "adjacency index count",
    );
    check(&|n| n.lods[0].active_bone_indices.push(7), "active bone 7");
    check(&|n| n.lods[0].required_bones.push(7), "required bone 7");
    check(&|n| n.lods[0].num_tex_coords = 1, "NumTexCoords");
    check(
        &|n| n.lods[0].vertex_buffer.vertices[0].position = [99.0; 3],
        "outside the bounds",
    );
    check(
        &|n| n.per_poly_bone_kdops[0].triangles[0][1] = 9,
        "per-poly collision",
    );
    check(&|n| n.ref_skeleton.clear(), "empty reference skeleton");
    check(&|n| n.origin[1] = f32::NAN, "Origin not finite");
    check(
        &|n| n.ref_skeleton[1].position[0] = f32::INFINITY,
        "position not finite",
    );
}

/// Deterministic pseudo-random numbers (no external crates).
struct Lcg(u64);

impl Lcg {
    fn next(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        self.0 >> 33
    }
}

#[test]
fn truncation_and_trailing_bytes_are_rejected() {
    let (data, start) = hand_made();
    for cut in start..data.len() {
        assert!(
            decode_skeletal_mesh_native(&data[..cut], start, false).is_err(),
            "truncated at {cut} accepted"
        );
    }
    let mut longer = data.clone();
    longer.push(0);
    assert!(decode_skeletal_mesh_native(&longer, start, false).is_err());
    let bytes = encode_skeletal_mesh_native(&rich_mesh(), true).unwrap();
    for cut in (0..bytes.len()).step_by(7) {
        assert!(decode_skeletal_mesh_native(&bytes[..cut], 0, true).is_err());
    }
}

#[test]
fn mutations_never_panic_and_accepted_inputs_re_encode() {
    let (data, start) = hand_made();
    let rich = encode_skeletal_mesh_native(&rich_mesh(), true).unwrap();
    let mut rng = Lcg(0x5eed);
    let mut accepted = 0;
    for round in 0..6000 {
        let (base, s, colors) = if round % 2 == 0 {
            (&data, start, false)
        } else {
            (&rich, 0, true)
        };
        let mut m = base.clone();
        let flips = 1 + rng.next() % 4;
        for _ in 0..flips {
            let at = s + (rng.next() as usize) % (m.len() - s);
            match rng.next() % 3 {
                0 => m[at] ^= 1 << (rng.next() % 8),
                1 => m[at] = rng.next() as u8,
                _ => {
                    // Extreme i32 at a 4-byte position.
                    let at = at.min(m.len().saturating_sub(4));
                    let v = [i32::MIN, i32::MAX, -1, 0x7fff_fff0][(rng.next() % 4) as usize];
                    m[at..at + 4].copy_from_slice(&v.to_le_bytes());
                }
            }
        }
        if let Ok(n) = decode_skeletal_mesh_native(&m, s, colors) {
            accepted += 1;
            let _ = validate_skeletal_mesh(&n, None, &ValidationContext::default());
            // Only non-canonical FStrings may change on re-encoding.
            if n.bone_break_names
                .iter()
                .all(|b| b.is_ascii() && !b.is_empty())
            {
                assert_eq!(
                    encode_skeletal_mesh_native(&n, colors).as_deref(),
                    Some(&m[s..]),
                    "round {round}"
                );
            }
        }
    }
    assert!(accepted > 100, "only {accepted} mutations decoded");
    // Pure noise.
    for len in [0usize, 1, 3, 28, 64, 200, 1000] {
        let noise: Vec<u8> = (0..len).map(|_| rng.next() as u8).collect();
        let _ = decode_skeletal_mesh_native(&noise, 0, false);
        let _ = decode_skeletal_mesh_native(&noise, 0, true);
    }
}

#[test]
fn bounded_counts_are_refused_before_allocation() {
    let (mut data, start) = hand_made();
    // Material count far beyond the payload.
    data[start + 28..start + 32].copy_from_slice(&0x0fff_ffffi32.to_le_bytes());
    assert!(decode_skeletal_mesh_native(&data, start, false).is_err());
    // Negative bone count.
    let (mut data, start) = hand_made();
    let bones_at = start + 28 + 4 + 8 + 12 + 12;
    data[bones_at..bones_at + 4].copy_from_slice(&(-1i32).to_le_bytes());
    assert!(decode_skeletal_mesh_native(&data, start, false).is_err());
}
