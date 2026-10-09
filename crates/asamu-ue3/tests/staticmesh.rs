//! StaticMesh native-data decoder against synthetic payloads written byte by
//! byte in this file (no game data). See `docs/reverse-engineering/MESHES.md`.

#![allow(clippy::unwrap_used)]

use asamu_ue3::PackageIndex;
use asamu_ue3::bulkdata::BulkDataRecord;
use asamu_ue3::staticmesh::{
    BoxSphereBounds, CollisionTriangle, ColorBuffer, CompactKdopNode, FragmentRange, KdopBounds,
    KdopTree, LodModel, MeshSection, OptimizationSettings, PackedNormal, PositionBuffer,
    StaticMeshNative, ValidationContext, VertexBuffer, decode_static_mesh_native,
    encode_static_mesh_native, f32_to_half, half_to_f32, read_lod_model, validate_static_mesh,
    vertex_element_size,
};
use asamu_ue3::types::Guid;

/// Little-endian byte builder for hand-written payloads.
#[derive(Default)]
struct B(Vec<u8>);

impl B {
    fn u8(&mut self, v: u8) -> &mut Self {
        self.0.push(v);
        self
    }
    fn u16(&mut self, v: u16) -> &mut Self {
        self.0.extend_from_slice(&v.to_le_bytes());
        self
    }
    fn u32(&mut self, v: u32) -> &mut Self {
        self.0.extend_from_slice(&v.to_le_bytes());
        self
    }
    fn i32(&mut self, v: i32) -> &mut Self {
        self.0.extend_from_slice(&v.to_le_bytes());
        self
    }
    fn f32(&mut self, v: f32) -> &mut Self {
        self.u32(v.to_bits())
    }
    fn vec3(&mut self, v: [f32; 3]) -> &mut Self {
        self.f32(v[0]).f32(v[1]).f32(v[2])
    }
}

/// A unit quad (two triangles) in the XY plane facing +Z in UE3 terms.
fn quad_lod(num_uv: u32, full: bool, colors: bool) -> LodModel {
    let positions = vec![
        [0.0, 0.0, 0.0],
        [0.0, 10.0, 0.0],
        [10.0, 10.0, 0.0],
        [10.0, 0.0, 0.0],
    ];
    // Normal +Z (255 -> 1.0; 128 -> ~0), tangent +X, W = 255 (+1).
    let tz = PackedNormal([128, 128, 255, 255]);
    let tx = PackedNormal([255, 128, 128, 128]);
    let uvs: Vec<Vec<[f32; 2]>> = (0..num_uv)
        .map(|c| {
            let o = c as f32 * 0.25;
            vec![[o, 0.0], [o, 1.0], [o + 1.0, 1.0], [o + 1.0, 0.5]]
        })
        .collect();
    let elem = vertex_element_size(num_uv, full).unwrap() as u32;
    LodModel {
        raw_triangles: BulkDataRecord {
            flags: 0,
            element_count: 0,
            size_on_disk: 0,
            offset_in_file: 0,
            header_offset: 0,
        },
        sections: vec![
            MeshSection {
                material: PackageIndex(-3),
                enable_collision: 1,
                old_enable_collision: 1,
                enable_shadow_casting: 1,
                first_index: 0,
                num_triangles: 1,
                min_vertex_index: 0,
                max_vertex_index: 2,
                material_index: 0,
                fragments: vec![FragmentRange {
                    base_index: 0,
                    num_primitives: 1,
                }],
            },
            MeshSection {
                material: PackageIndex::NULL,
                enable_collision: 0,
                old_enable_collision: 0,
                enable_shadow_casting: 0,
                first_index: 3,
                num_triangles: 1,
                min_vertex_index: 0,
                max_vertex_index: 3,
                material_index: 1,
                fragments: vec![],
            },
        ],
        positions: PositionBuffer {
            stride: 12,
            num_vertices: 4,
            positions,
        },
        vertices: VertexBuffer {
            num_tex_coords: num_uv,
            stride: elem,
            num_vertices: 4,
            full_precision_uvs: full,
            tangent_x: vec![tx; 4],
            tangent_z: vec![tz; 4],
            uvs,
        },
        colors: if colors {
            ColorBuffer {
                stride: 4,
                num_vertices: 4,
                colors_bgra: vec![
                    [1, 2, 3, 4],
                    [5, 6, 7, 8],
                    [9, 10, 11, 12],
                    [13, 14, 15, 16],
                ],
            }
        } else {
            ColorBuffer {
                stride: 0,
                num_vertices: 0,
                colors_bgra: vec![],
            }
        },
        num_vertices: 4,
        // UE3 front faces wind so that cross(b - a, c - a) opposes the normal.
        indices: vec![0, 1, 2, 0, 2, 3],
        wireframe_indices: vec![],
        adjacency_indices: vec![0; 24],
    }
}

fn sample_native(num_uv: u32, full: bool, colors: bool, lods: usize) -> StaticMeshNative {
    StaticMeshNative {
        start: 0,
        bounds: BoxSphereBounds {
            origin: [5.0, 5.0, 0.0],
            box_extent: [5.0, 5.0, 0.0],
            sphere_radius: 7.5,
        },
        body_setup: PackageIndex(2),
        kdop: KdopTree {
            root_bounds: KdopBounds {
                min: [0.0, 0.0, 0.0],
                max: [10.0, 10.0, 0.0],
            },
            nodes: vec![CompactKdopNode {
                bytes: [1, 2, 3, 4, 5, 6],
            }],
            triangles: vec![CollisionTriangle {
                vertices: [0, 1, 2],
                material_index: 0,
            }],
        },
        internal_version: 18,
        source_data: None,
        optimization_settings: vec![],
        has_been_simplified: 0,
        is_mesh_proxy: 0,
        lods: (0..lods).map(|_| quad_lod(num_uv, full, colors)).collect(),
        lod_info_count: lods as u32,
        thumbnail_angle: [1, -2, 3],
        thumbnail_distance: 128.0,
        high_res_source_mesh_name: String::new(),
        high_res_source_mesh_crc: 0,
        lighting_guid: Guid {
            a: 1,
            b: 2,
            c: 3,
            d: 4,
        },
        vertex_position_version: 1,
        cached_streaming_texture_factors: vec![1.0, 2.0, 0.0, 0.0],
        remove_degenerates: 1,
        per_lod_static_lighting_for_instancing: 0,
        console_prealloc_instance_count: 0,
    }
}

/// Zero the decoder-recorded header positions so values compare by content.
fn strip_offsets(mut n: StaticMeshNative) -> StaticMeshNative {
    for lod in n.lods.iter_mut().chain(n.source_data.as_deref_mut()) {
        lod.raw_triangles.header_offset = 0;
    }
    n
}

fn ctx() -> ValidationContext {
    ValidationContext {
        payload_stream_offset: None,
        imports: 8,
        exports: 8,
    }
}

/// Hand-written bytes for the documented layout (independent of the encoder).
fn handwritten() -> Vec<u8> {
    let mut b = B::default();
    // Bounds, BodySetup.
    b.vec3([1.0, 2.0, 3.0]).vec3([4.0, 5.0, 6.0]).f32(7.0);
    b.i32(-1);
    // kDOP: root bounds, 1 node, 1 triangle.
    b.vec3([0.0, 0.0, 0.0]).vec3([1.0, 1.0, 1.0]);
    b.i32(6).i32(1).u8(10).u8(20).u8(30).u8(40).u8(50).u8(60);
    b.i32(8).i32(1).u16(0).u16(1).u16(2).u16(0);
    // InternalVersion, no source data, one optimisation-settings record.
    b.i32(18).u32(0);
    b.i32(1)
        .u8(1)
        .f32(0.5)
        .f32(0.25)
        .u8(2)
        .u8(3)
        .u8(4)
        .u32(1)
        .f32(60.0)
        .f32(0.1);
    b.u32(0).u32(0);
    // One LOD.
    b.i32(1);
    b.u32(0).i32(0).i32(0).i32(1234); // raw triangles record (empty)
    b.i32(1); // one section
    b.i32(-7)
        .u32(1)
        .u32(1)
        .u32(1)
        .u32(0)
        .u32(1)
        .u32(0)
        .u32(2)
        .i32(0);
    b.i32(1).i32(0).i32(1); // one fragment
    b.u8(0); // no platform data
    b.u32(12).u32(3).i32(12).i32(3); // positions
    b.vec3([0.0, 0.0, 0.0])
        .vec3([0.0, 1.0, 0.0])
        .vec3([1.0, 1.0, 0.0]);
    b.u32(1).u32(12).u32(3).u32(0).i32(12).i32(3); // 1 half UV channel
    for v in 0..3u16 {
        b.u8(255).u8(128).u8(128).u8(128); // TangentX
        b.u8(128).u8(128).u8(255).u8(0); // TangentZ, W = 0 (-1)
        b.u16(0x3c00 * (v & 1)).u16(0x3800); // (0|1, 0.5)
    }
    b.u32(4).u32(3).i32(4).i32(3); // colors
    b.u8(1)
        .u8(2)
        .u8(3)
        .u8(4)
        .u8(5)
        .u8(6)
        .u8(7)
        .u8(8)
        .u8(9)
        .u8(10)
        .u8(11)
        .u8(12);
    b.u32(3); // NumVertices
    b.i32(2).i32(3).u16(0).u16(1).u16(2); // indices
    b.i32(2).i32(0); // wireframe
    b.i32(2).i32(0); // adjacency
    // Trailing fields.
    b.u32(1); // LOD info count
    b.i32(0).i32(16384).i32(0).f32(300.0);
    b.i32(0); // empty FString
    b.u32(0xDEADBEEF);
    b.u32(9).u32(8).u32(7).u32(6);
    b.i32(5);
    b.i32(4).f32(1.0).f32(0.0).f32(0.0).f32(0.0);
    b.u32(1).u32(0).i32(0);
    b.0
}

#[test]
fn handwritten_payload_decodes_field_by_field() {
    let bytes = handwritten();
    let n = decode_static_mesh_native(&bytes, 0).unwrap();
    assert_eq!(n.bounds.origin, [1.0, 2.0, 3.0]);
    assert_eq!(n.bounds.box_extent, [4.0, 5.0, 6.0]);
    assert_eq!(n.bounds.sphere_radius, 7.0);
    assert_eq!(n.body_setup, PackageIndex(-1));
    assert_eq!(n.kdop.nodes[0].bytes, [10, 20, 30, 40, 50, 60]);
    assert_eq!(n.kdop.triangles[0].vertices, [0, 1, 2]);
    assert_eq!(n.internal_version, 18);
    assert!(n.source_data.is_none());
    let o = n.optimization_settings[0];
    assert_eq!(
        (
            o.reduction_method,
            o.silhouette_importance,
            o.shading_importance
        ),
        (1, 2, 4)
    );
    assert_eq!(
        (
            o.num_triangles_percentage,
            o.normals_threshold,
            o.welding_threshold
        ),
        (0.5, 60.0, 0.1)
    );
    let lod = &n.lods[0];
    assert_eq!(lod.raw_triangles.offset_in_file, 1234);
    assert_eq!(lod.sections[0].material, PackageIndex(-7));
    assert_eq!(lod.sections[0].max_vertex_index, 2);
    assert_eq!(lod.sections[0].fragments.len(), 1);
    assert_eq!(lod.positions.positions[2], [1.0, 1.0, 0.0]);
    assert_eq!(lod.vertices.uvs[0][1], [1.0, 0.5]);
    assert_eq!(lod.vertices.uvs[0][0], [0.0, 0.5]);
    assert_eq!(lod.vertices.tangent_z[0].unpack()[2], 1.0);
    assert_eq!(lod.vertices.tangent_z[0].unpack()[3], -1.0);
    assert_eq!(lod.colors.colors_bgra[2], [9, 10, 11, 12]);
    assert_eq!(lod.indices, vec![0, 1, 2]);
    assert!(lod.wireframe_indices.is_empty());
    assert_eq!(n.lod_info_count, 1);
    assert_eq!(n.thumbnail_angle, [0, 16384, 0]);
    assert_eq!(n.thumbnail_distance, 300.0);
    assert_eq!(n.high_res_source_mesh_crc, 0xDEADBEEF);
    assert_eq!(n.lighting_guid.a, 9);
    assert_eq!(n.vertex_position_version, 5);
    assert_eq!(n.cached_streaming_texture_factors, vec![1.0, 0.0, 0.0, 0.0]);
    assert_eq!(n.remove_degenerates, 1);
    // The encoder reproduces the hand-written bytes exactly.
    assert_eq!(encode_static_mesh_native(&n).unwrap(), bytes);
}

#[test]
fn decoding_starts_at_the_given_offset() {
    let mut bytes = vec![0xAA; 13];
    bytes.extend(handwritten());
    let n = decode_static_mesh_native(&bytes, 13).unwrap();
    assert_eq!(n.start, 13);
    assert_eq!(n.lods.len(), 1);
    assert!(decode_static_mesh_native(&bytes, 12).is_err());
    assert!(decode_static_mesh_native(&bytes, bytes.len() + 1).is_err());
}

#[test]
fn encode_decode_round_trips_every_vertex_format() {
    for num_uv in 1..=4 {
        for full in [false, true] {
            for colors in [false, true] {
                for lods in [1, 3] {
                    let n = sample_native(num_uv, full, colors, lods);
                    let bytes = encode_static_mesh_native(&n).unwrap();
                    let back = decode_static_mesh_native(&bytes, 0).unwrap();
                    // The recorded header offsets point at the records.
                    let at = back.lods[0].raw_triangles.header_offset;
                    assert_eq!(bytes[at..at + 4], [0, 0, 0, 0]);
                    let back = strip_offsets(back);
                    assert_eq!(back, n, "uv {num_uv} full {full} colors {colors}");
                    assert!(
                        validate_static_mesh(&back, &ctx()).is_empty(),
                        "{:?}",
                        validate_static_mesh(&back, &ctx())
                    );
                    // Vertex element size is 8 + 4/8 bytes per channel.
                    let elem = vertex_element_size(num_uv, full).unwrap();
                    assert_eq!(elem, 8 + num_uv as usize * if full { 8 } else { 4 });
                }
            }
        }
    }
}

#[test]
fn source_data_and_optimization_settings_round_trip() {
    let mut n = sample_native(2, false, false, 1);
    n.source_data = Some(Box::new(quad_lod(1, true, true)));
    n.optimization_settings = vec![OptimizationSettings {
        reduction_method: 2,
        num_triangles_percentage: 50.0,
        max_deviation_percentage: 1.5,
        silhouette_importance: 3,
        texture_importance: 3,
        shading_importance: 1,
        recalc_normals: 1,
        normals_threshold: 60.0,
        welding_threshold: 0.0,
    }];
    n.high_res_source_mesh_name = "Hi\u{e9}".to_owned();
    let bytes = encode_static_mesh_native(&n).unwrap();
    assert_eq!(
        strip_offsets(decode_static_mesh_native(&bytes, 0).unwrap()),
        n
    );
}

#[test]
fn inline_raw_triangle_payload_is_skipped() {
    let mut n = sample_native(1, false, false, 1);
    n.lods[0].raw_triangles.element_count = 1;
    n.lods[0].raw_triangles.size_on_disk = 0x174;
    let bytes = encode_static_mesh_native(&n).unwrap();
    let back = decode_static_mesh_native(&bytes, 0).unwrap();
    assert_eq!(back.lods[0].raw_triangles.size_on_disk, 0x174);
    assert_eq!(back.lods[0].positions, n.lods[0].positions);
    // Separate-file records carry no inline bytes.
    let mut n = sample_native(1, false, false, 1);
    n.lods[0].raw_triangles.flags = 1;
    n.lods[0].raw_triangles.size_on_disk = 100;
    let bytes = encode_static_mesh_native(&n).unwrap();
    assert_eq!(
        decode_static_mesh_native(&bytes, 0).unwrap().lods[0]
            .raw_triangles
            .size_on_disk,
        100
    );
}

#[test]
fn lod_model_reads_standalone() {
    let lod = quad_lod(2, false, true);
    let mut n = sample_native(2, false, true, 1);
    n.lods = vec![lod.clone()];
    let bytes = encode_static_mesh_native(&n).unwrap();
    // The LOD starts after bounds (28), body setup (4), kDOP (24 + 8 + 6 + 8 + 8),
    // internal version (4), source flag (4), settings count (4), two u32 (8) and
    // the LOD count (4).
    let at = 28 + 4 + 24 + 8 + 6 + 8 + 8 + 4 + 4 + 4 + 8 + 4;
    let mut r = asamu_ue3::reader::Reader::at(&bytes, at).unwrap();
    let mut got = read_lod_model(&mut r).unwrap();
    assert_eq!(got.raw_triangles.header_offset, at);
    got.raw_triangles.header_offset = 0;
    assert_eq!(got, lod);
}

#[test]
fn half_floats_round_trip_and_round_to_even() {
    // Bit-exact for all 65,536 halves, NaN payloads included (the encoder
    // must reproduce any accepted vertex buffer byte for byte).
    for h in 0..=u16::MAX {
        let f = half_to_f32(h);
        assert_eq!(f32_to_half(f), h, "half {h:#06x} -> {f}");
    }
    // A NaN whose payload only has low f32 bits stays a (quiet) NaN.
    assert_eq!(f32_to_half(f32::from_bits(0x7f80_0001)), 0x7e00);
    assert_eq!(f32_to_half(f32::from_bits(0xff80_0001)), 0xfe00);
    assert_eq!(half_to_f32(0x3c00), 1.0);
    assert_eq!(half_to_f32(0xc000), -2.0);
    assert_eq!(half_to_f32(0x3800), 0.5);
    assert_eq!(half_to_f32(0x7bff), 65504.0);
    assert_eq!(half_to_f32(0x0001), 2f32.powi(-24));
    assert_eq!(half_to_f32(0x7c00), f32::INFINITY);
    assert_eq!(f32_to_half(1.0 + 2f32.powi(-11)), 0x3c00); // tie -> even
    assert_eq!(f32_to_half(1.0 + 3.0 * 2f32.powi(-11)), 0x3c02); // tie -> even (up)
    assert_eq!(f32_to_half(1.0e6), 0x7c00);
    assert_eq!(f32_to_half(-1.0e-10), 0x8000);
    assert_eq!(f32_to_half(65520.0), 0x7c00); // rounds up past the largest finite half
}

#[test]
fn packed_normals_unpack_to_unit_range() {
    assert_eq!(PackedNormal([255, 0, 128, 255]).unpack()[0], 1.0);
    assert_eq!(PackedNormal([255, 0, 128, 255]).unpack()[1], -1.0);
    let mid = PackedNormal([255, 0, 128, 255]).unpack()[2];
    assert!(mid > 0.0 && mid < 0.01);
}

#[test]
fn validation_reports_inconsistencies() {
    let ok = sample_native(2, false, true, 1);
    assert!(validate_static_mesh(&ok, &ctx()).is_empty());
    let check = |f: &dyn Fn(&mut StaticMeshNative), needle: &str| {
        let mut n = ok.clone();
        f(&mut n);
        let issues = validate_static_mesh(&n, &ctx());
        assert!(
            issues.iter().any(|i| i.contains(needle)),
            "expected '{needle}' in {issues:?}"
        );
    };
    check(&|n| n.lods[0].indices[1] = 9, "index 9 >= 4 vertices");
    check(&|n| n.lods[0].sections[1].num_triangles = 5, "exceed");
    check(&|n| n.lods[0].sections[0].max_vertex_index = 1, "outside");
    check(
        &|n| n.lods[0].positions.positions[0][2] = 100.0,
        "bounds box",
    );
    check(&|n| n.lods[0].positions.stride = 16, "position stride");
    check(&|n| n.lods[0].vertices.stride = 99, "vertex stride");
    check(&|n| n.lods[0].num_vertices = 3, "NumVertices 3");
    check(&|n| n.lods[0].colors.num_vertices = 5, "color buffer");
    check(
        &|n| n.lods[0].adjacency_indices.pop().map(|_| ()).unwrap(),
        "adjacency",
    );
    check(&|n| n.lods[0].wireframe_indices = vec![0], "odd");
    check(
        &|n| n.kdop.triangles[0].vertices[2] = 4,
        "collision triangle 0",
    );
    check(
        &|n| n.kdop.triangles[0].material_index = 2,
        "material index",
    );
    check(&|n| n.body_setup = PackageIndex(100), "BodySetup");
    check(
        &|n| n.lods[0].sections[0].material = PackageIndex(-100),
        "material reference",
    );
    check(&|n| n.lods.clear(), "no LOD models");
    check(
        &|n| n.lods[0].raw_triangles.element_count = 2,
        "raw triangles: SizeOnDisk",
    );
    // Inline OffsetInFile must equal the payload's stream position.
    let mut n = ok.clone();
    n.lods[0].raw_triangles.header_offset = 100;
    n.lods[0].raw_triangles.offset_in_file = 1000 + 116;
    let c = ValidationContext {
        payload_stream_offset: Some(1000),
        ..ctx()
    };
    assert!(validate_static_mesh(&n, &c).is_empty());
    n.lods[0].raw_triangles.offset_in_file = 7;
    assert!(
        validate_static_mesh(&n, &c)
            .iter()
            .any(|i| i.contains("OffsetInFile"))
    );
    // Coarser LODs may exceed the bounds (they are LOD 0's).
    let mut n = sample_native(1, false, false, 2);
    n.lods[1].positions.positions[0][2] = 1.0;
    assert!(validate_static_mesh(&n, &ctx()).is_empty());
}

#[test]
fn encoder_rejects_inconsistent_vertex_buffers() {
    let mut n = sample_native(2, false, false, 1);
    n.lods[0].vertices.uvs.pop();
    assert!(encode_static_mesh_native(&n).is_none());
    let mut n = sample_native(1, false, false, 1);
    n.lods[0].vertices.num_tex_coords = 0;
    assert!(encode_static_mesh_native(&n).is_none());
    let mut n = sample_native(1, false, false, 1);
    n.lods[0].colors.colors_bgra.push([0; 4]);
    assert!(encode_static_mesh_native(&n).is_none());
}
