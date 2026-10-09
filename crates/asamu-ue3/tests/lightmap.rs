//! Synthetic fixtures for baked-lighting component data
//! (`asamu_ue3::lightmap`): known answers, exact round trips, and hostile
//! input (truncation, corruption, impossible counts). Every byte here is
//! written by this file; no game data is involved.

#![allow(clippy::unwrap_used)]

use asamu_ue3::bulkdata::BulkDataRecord;
use asamu_ue3::lightmap::*;
use asamu_ue3::property::{ObjRef, Property, Value};
use asamu_ue3::types::{Guid, PackageIndex};
use asamu_ue3::writer::Writer;

fn guid(n: u32) -> Guid {
    Guid {
        a: n,
        b: n.wrapping_mul(3),
        c: 0xDEAD_0000 | n,
        d: !n,
    }
}

fn samples(element_size: usize, n: usize, seed: u8) -> QuantizedSamples {
    let data: Vec<u8> = (0..element_size * n)
        .map(|i| seed.wrapping_add(u8::try_from(i % 251).unwrap()))
        .collect();
    QuantizedSamples {
        record: BulkDataRecord {
            flags: 0,
            element_count: i32::try_from(n).unwrap(),
            size_on_disk: i32::try_from(data.len()).unwrap(),
            offset_in_file: 0,
            header_offset: 0,
        },
        element_size,
        data,
    }
}

fn map_1d(n: usize) -> LightMap {
    LightMap::OneD(LightMap1D {
        light_guids: vec![guid(1), guid(2)],
        owner: PackageIndex(7),
        directional_samples: samples(DIRECTIONAL_SAMPLE_SIZE, n, 10),
        scale_vectors: [[1.0, 1.0, 1.0], [1.5, 0.5, 2.0], [2.0, 2.0, 2.0]],
        simple_samples: samples(SIMPLE_SAMPLE_SIZE, n, 99),
    })
}

fn map_2d() -> LightMap {
    LightMap::TwoD(LightMap2D {
        light_guids: vec![guid(5)],
        textures: [PackageIndex(3), PackageIndex(4), PackageIndex::NULL],
        scale_vectors: [
            [1.0, 1.0, 1.0],
            [1.540_928_6, 1.368_861_6, 1.423_704_1],
            [2.0, 2.0, 2.0],
        ],
        coordinate_scale: [0.265_625, 0.656_25],
        coordinate_bias: [0.25, 0.0],
    })
}

fn smc(instanced: bool) -> StaticMeshComponentNative {
    StaticMeshComponentNative {
        lods: vec![
            StaticMeshComponentLodInfo {
                shadow_maps: vec![PackageIndex(9)],
                shadow_vertex_buffers: vec![],
                light_map: map_2d(),
                override_vertex_colors: Some(ColorVertexBuffer {
                    stride: 4,
                    num_vertices: 3,
                    colors: vec![[1, 2, 3, 4], [5, 6, 7, 8], [9, 10, 11, 12]],
                }),
                painted_vertices: vec![PaintedVertex {
                    position: [1.0, -2.0, 3.5],
                    normal: [127, 128, 255, 0],
                    color: [10, 20, 30, 40],
                }],
            },
            StaticMeshComponentLodInfo {
                shadow_maps: vec![],
                shadow_vertex_buffers: vec![PackageIndex(11)],
                light_map: map_1d(5),
                override_vertex_colors: Some(ColorVertexBuffer {
                    stride: 4,
                    num_vertices: 0,
                    colors: vec![],
                }),
                painted_vertices: vec![],
            },
            StaticMeshComponentLodInfo {
                shadow_maps: vec![],
                shadow_vertex_buffers: vec![],
                light_map: LightMap::None,
                override_vertex_colors: None,
                painted_vertices: vec![],
            },
        ],
        instances: instanced.then(|| {
            vec![InstanceData {
                transform: core::array::from_fn(|i| i as f32),
                lightmap_uv_bias: [0.5, 0.25],
                shadowmap_uv_bias: [0.125, 0.0],
            }]
        }),
    }
}

fn model() -> ModelComponentNative {
    ModelComponentNative {
        model: PackageIndex(2),
        zone_index: 0,
        elements: vec![
            ModelElement {
                light_map: map_2d(),
                component: PackageIndex(6),
                material: PackageIndex(-3),
                nodes: vec![0, 1, 5],
                shadow_maps: vec![PackageIndex(8)],
                irrelevant_lights: vec![guid(9), guid(10)],
            },
            ModelElement {
                light_map: LightMap::None,
                component: PackageIndex(6),
                material: PackageIndex::NULL,
                nodes: vec![],
                shadow_maps: vec![],
                irrelevant_lights: vec![],
            },
        ],
        component_index: 4,
        nodes: vec![0, 1, 5, 7],
    }
}

/// Recompute the decoded sample records the way the decoder reports them
/// (header offsets are payload positions) so fixtures compare equal.
fn strip_records(n: &mut LightingNative) {
    fn fix(m: &mut LightMap) {
        if let LightMap::OneD(m) = m {
            for s in [&mut m.directional_samples, &mut m.simple_samples] {
                s.record.header_offset = 0;
                s.record.offset_in_file = 0;
            }
        }
    }
    match n {
        LightingNative::StaticMesh(s) => s.lods.iter_mut().for_each(|l| fix(&mut l.light_map)),
        LightingNative::Model(m) => m.elements.iter_mut().for_each(|e| fix(&mut e.light_map)),
        LightingNative::SpeedTree(v) => v.iter_mut().for_each(fix),
        LightingNative::FluidSurface(m) => fix(m),
        LightingNative::ShadowMap1D(_) => {}
    }
}

fn fixtures() -> Vec<(LightingLayout, LightingNative)> {
    vec![
        (
            LightingLayout::StaticMeshComponent,
            LightingNative::StaticMesh(smc(false)),
        ),
        (
            LightingLayout::InstancedStaticMeshComponent,
            LightingNative::StaticMesh(smc(true)),
        ),
        (
            LightingLayout::ModelComponent,
            LightingNative::Model(model()),
        ),
        (
            LightingLayout::SpeedTreeComponent,
            LightingNative::SpeedTree(vec![
                LightMap::None,
                map_2d(),
                LightMap::None,
                map_1d(2),
                LightMap::None,
            ]),
        ),
        (
            LightingLayout::FluidSurfaceComponent,
            LightingNative::FluidSurface(map_1d(3)),
        ),
        (
            LightingLayout::ShadowMap1D,
            LightingNative::ShadowMap1D(ShadowMap1DNative {
                samples: vec![0.0, 0.5, 1.0],
                light_guid: guid(42),
            }),
        ),
    ]
}

/// A payload of `prefix` junk bytes (standing in for tagged properties)
/// followed by the encoded native data, with absolute sample offsets for an
/// export at stream offset `serial`.
fn payload(native: &LightingNative, prefix: usize, serial: i64) -> Vec<u8> {
    let mut data = vec![0xAB; prefix];
    let base = serial + i64::try_from(prefix).unwrap();
    data.extend(encode_lighting_native(native, Some(base)).unwrap());
    data
}

#[test]
fn every_layout_round_trips() {
    for (layout, native) in fixtures() {
        let data = payload(&native, 13, 1000);
        let decoded = decode_lighting_native(layout, &data, 13).unwrap();
        let mut a = decoded.clone();
        let mut b = native.clone();
        strip_records(&mut a);
        strip_records(&mut b);
        assert_eq!(a, b, "{layout:?}");
        // Re-encoding with the same base reproduces the bytes exactly.
        assert_eq!(
            encode_lighting_native(&decoded, Some(1013)).unwrap(),
            data[13..],
            "{layout:?}"
        );
        // Without a base the decoded offsets are kept: still exact.
        assert_eq!(
            encode_lighting_native(&decoded, None).unwrap(),
            data[13..],
            "{layout:?}"
        );
    }
}

#[test]
fn sample_records_carry_their_own_position() {
    let native = LightingNative::FluidSurface(map_1d(4));
    let data = payload(&native, 0, 5000);
    let LightingNative::FluidSurface(LightMap::OneD(m)) =
        decode_lighting_native(LightingLayout::FluidSurfaceComponent, &data, 0).unwrap()
    else {
        panic!("expected a 1D light map");
    };
    for s in [&m.directional_samples, &m.simple_samples] {
        assert!(s.record.inline_offset_matches(5000));
    }
    assert_eq!(m.directional_samples.len(), 4);
    assert_eq!(m.simple_samples.len(), 4);
    assert_eq!(m.simple_samples.color(0, 0), Some([99, 100, 101, 102]));
    assert_eq!(m.directional_samples.color(0, 1), Some([14, 15, 16, 17]));
    assert_eq!(m.directional_samples.color(0, 2), None);
    assert_eq!(m.simple_samples.color(4, 0), None);
}

#[test]
fn known_layout_of_a_2d_light_map() {
    // Hand-written bytes: type 2, one GUID, three (texture, scale) pairs,
    // coordinate scale and bias.
    let mut w = Writer::new();
    w.u32(2);
    w.i32(1);
    w.guid(guid(1));
    for (t, s) in [(5, 1.0_f32), (6, 2.0), (0, 3.0)] {
        w.i32(t);
        for _ in 0..3 {
            w.u32(s.to_bits());
        }
    }
    for v in [0.5_f32, 0.25, 0.125, 0.0625] {
        w.u32(v.to_bits());
    }
    let bytes = w.into_bytes();
    assert_eq!(bytes.len(), 4 + 4 + 16 + 3 * 16 + 16);
    let m = decode_fluid_surface_component_native(&bytes, 0).unwrap();
    let LightMap::TwoD(t) = m else {
        panic!("expected 2D")
    };
    assert_eq!(
        t.textures,
        [PackageIndex(5), PackageIndex(6), PackageIndex::NULL]
    );
    assert_eq!(t.scale_vectors[1], [2.0; 3]);
    assert_eq!(t.uv_rect(), [0.125, 0.0625, 0.625, 0.3125]);
    assert_eq!(t.light_guids, vec![guid(1)]);
}

#[test]
fn empty_static_mesh_component_is_four_bytes() {
    let n = decode_static_mesh_component_native(&[0, 0, 0, 0], 0, false).unwrap();
    assert!(n.lods.is_empty() && n.instances.is_none());
    assert!(decode_static_mesh_component_native(&[0, 0, 0, 0], 0, true).is_err());
}

#[test]
fn truncation_at_every_offset_is_an_error() {
    for (layout, native) in fixtures() {
        let data = payload(&native, 3, 0);
        for cut in 3..data.len() {
            assert!(
                decode_lighting_native(layout, &data[..cut], 3).is_err(),
                "{layout:?} cut at {cut}"
            );
        }
        // Trailing bytes are refused too.
        let mut longer = data.clone();
        longer.push(0);
        assert!(decode_lighting_native(layout, &longer, 3).is_err());
    }
}

#[test]
fn corruption_never_panics() {
    for (layout, native) in fixtures() {
        let data = payload(&native, 0, 0);
        for i in 0..data.len() {
            for v in [0x00, 0xFF, 0x7F, 0x80, data[i] ^ 0x01] {
                let mut d = data.clone();
                d[i] = v;
                let _ = decode_lighting_native(layout, &d, 0);
            }
        }
        for i in (0..data.len().saturating_sub(3)).step_by(1) {
            for v in [i32::MAX, i32::MIN, -1, 0x4000_0000] {
                let mut d = data.clone();
                d[i..i + 4].copy_from_slice(&v.to_le_bytes());
                let _ = decode_lighting_native(layout, &d, 0);
            }
        }
    }
}

#[test]
fn malformed_values_are_refused() {
    // Unknown light map type.
    assert!(decode_fluid_surface_component_native(&3u32.to_le_bytes(), 0).is_err());
    // A huge GUID count cannot allocate.
    let mut w = Writer::new();
    w.u32(2);
    w.i32(0x1000_0000);
    assert!(decode_fluid_surface_component_native(&w.into_bytes(), 0).is_err());
    // Vertex colour flag other than 0/1.
    let mut w = Writer::new();
    w.i32(1); // one LOD
    w.i32(0);
    w.i32(0);
    w.u32(0); // no light map
    w.u8(2);
    w.i32(0);
    assert!(decode_static_mesh_component_native(&w.into_bytes(), 0, false).is_err());
    // Wrong bulk element size for the instance array.
    let mut w = Writer::new();
    w.i32(0);
    w.i32(64);
    w.i32(0);
    assert!(decode_static_mesh_component_native(&w.into_bytes(), 0, true).is_err());
}

fn sample_bytes(flags: u32, count: i32, size: i32, payload: &[u8]) -> Vec<u8> {
    let mut w = Writer::new();
    w.u32(1); // 1D
    w.i32(0); // no GUIDs
    w.i32(0); // owner
    w.u32(flags);
    w.i32(count);
    w.i32(size);
    w.i32(0);
    w.bytes(payload);
    w.into_bytes()
}

#[test]
fn only_inline_uncompressed_samples_are_accepted() {
    // Separate-file, compressed and unused records are refused.
    for flags in [0x01, 0x10, 0x21, 0x02] {
        let d = sample_bytes(flags, 1, 8, &[0; 8]);
        assert!(
            decode_fluid_surface_component_native(&d, 0).is_err(),
            "{flags:#x}"
        );
    }
    // Size on disk must equal count x element size.
    let d = sample_bytes(0, 2, 8, &[0; 8]);
    assert!(decode_fluid_surface_component_native(&d, 0).is_err());
}

#[test]
fn shadow_map_2d_properties() {
    let f = |name: &str, v: f32| Property {
        name: name.to_owned(),
        type_name: "FloatProperty".to_owned(),
        array_index: 0,
        size: 4,
        struct_name: None,
        enum_name: None,
        value: Value::Float(v),
        offset: 0,
    };
    let i = |name: &str, v: i32| Property {
        value: Value::Int(v),
        type_name: "IntProperty".to_owned(),
        ..f(name, 0.0)
    };
    let st = |name: &str, s: &str, fields: Vec<Property>| Property {
        type_name: "StructProperty".to_owned(),
        struct_name: Some(s.to_owned()),
        value: Value::Struct {
            name: s.to_owned(),
            binary: false,
            fields,
        },
        ..f(name, 0.0)
    };
    let props = vec![
        Property {
            type_name: "ObjectProperty".to_owned(),
            value: Value::Object(ObjRef {
                index: 5,
                path: "Map.ShadowMapTexture2D_3".to_owned(),
            }),
            ..f("Texture", 0.0)
        },
        st(
            "CoordinateScale",
            "Vector2D",
            vec![f("X", 0.5), f("Y", 0.25)],
        ),
        st("CoordinateBias", "Vector2D", vec![f("X", 0.125)]),
        st(
            "LightGuid",
            "Guid",
            vec![i("A", 1), i("B", 2), i("C", 3), i("D", -1)],
        ),
        Property {
            type_name: "BoolProperty".to_owned(),
            value: Value::Bool(true),
            ..f("bIsShadowFactorTexture", 0.0)
        },
    ];
    let info = shadow_map_2d_info(&props);
    assert_eq!(info.texture.as_deref(), Some("Map.ShadowMapTexture2D_3"));
    assert_eq!(info.coordinate_scale, [0.5, 0.25]);
    assert_eq!(info.coordinate_bias, [0.125, 0.0]);
    assert_eq!(
        info.light_guid.as_deref(),
        Some("000000010000000200000003FFFFFFFF")
    );
    assert!(info.is_shadow_factor_texture);
    let empty = shadow_map_2d_info(&[]);
    assert_eq!(empty.texture, None);
    assert_eq!(empty.light_guid, None);
}

#[test]
fn srgb_transfer_function() {
    assert_eq!(srgb_to_linear(0), 0.0);
    assert!((srgb_to_linear(255) - 1.0).abs() < 1e-6);
    // 10 / 255 is in the linear segment.
    assert!((srgb_to_linear(10) - 10.0 / 255.0 / 12.92).abs() < 1e-7);
    // Middle grey (188) is about 0.5 linear.
    assert!((srgb_to_linear(188) - 0.502_886).abs() < 1e-4);
    let mut prev = -1.0;
    for v in 0..=255u8 {
        let l = srgb_to_linear(v);
        assert!(l > prev);
        prev = l;
    }
}

#[test]
fn directional_irradiance_combines_colour_and_mean_intensity() {
    let scale = [[1.0; 3], [2.0, 4.0, 6.0], [1.0; 3]];
    // White colour, full max components: mean of the scaled components = 4.
    let e = directional_irradiance([255, 255, 255], [255, 255, 255], &scale);
    for c in e {
        assert!((c - 4.0).abs() < 1e-5);
    }
    // Black colour or zero components give zero.
    assert_eq!(
        directional_irradiance([0, 0, 0], [255, 255, 255], &scale),
        [0.0; 3]
    );
    assert_eq!(
        directional_irradiance([255, 255, 255], [0, 0, 0], &scale),
        [0.0; 3]
    );
    // Pure red colour keeps only red.
    let e = directional_irradiance([255, 0, 0], [255, 0, 0], &scale);
    assert!((e[0] - 2.0 / 3.0).abs() < 1e-6 && e[1] == 0.0 && e[2] == 0.0);
}

#[test]
fn vertex_irradiance_reads_bgra_simple_samples() {
    let LightMap::OneD(mut m) = map_1d(1) else {
        panic!()
    };
    m.simple_samples.data = vec![0, 128, 255, 77]; // B, G, R, A
    m.scale_vectors[SIMPLE_LIGHTMAP_COEF_INDEX] = [2.0, 1.0, 0.5];
    let e = vertex_irradiance(&m, 0).unwrap();
    assert!((e[0] - 2.0).abs() < 1e-6);
    assert!((e[1] - srgb_to_linear(128)).abs() < 1e-6);
    assert_eq!(e[2], 0.0);
    assert!(vertex_irradiance(&m, 1).is_none());
}
