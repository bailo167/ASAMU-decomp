//! Synthetic and hostile-input tests for `asamu_ue3::decal`: the
//! `DecalComponent` native layout (written byte by byte here), the decal
//! frame and projection math, and parameter extraction. No game data.

#![allow(clippy::unwrap_used)]

use asamu_ue3::decal::{
    DECAL_VERTEX_SIZE, DecalBox, DecalFrame, DecalMeshBuilder, MeshTriangles, WorldReceiver,
    clip_polygon, decal_is_mirrored, decal_params, decode_decal_component_native,
    encode_decal_component_native, face_normal, filter_passes, rotator_axes, unpack_normal,
    vertex_normals,
};
use asamu_ue3::lightmap::LightMap;
use asamu_ue3::property::{ObjRef, Property, Value};
use asamu_ue3::types::PackageIndex;

#[derive(Default)]
struct W(Vec<u8>);

impl W {
    fn i32(&mut self, v: i32) -> &mut Self {
        self.0.extend_from_slice(&v.to_le_bytes());
        self
    }
    fn u32(&mut self, v: u32) -> &mut Self {
        self.0.extend_from_slice(&v.to_le_bytes());
        self
    }
    fn u16(&mut self, v: u16) -> &mut Self {
        self.0.extend_from_slice(&v.to_le_bytes());
        self
    }
    fn f32(&mut self, v: f32) -> &mut Self {
        self.0.extend_from_slice(&v.to_le_bytes());
        self
    }
    fn bytes(&mut self, b: &[u8]) -> &mut Self {
        self.0.extend_from_slice(b);
        self
    }
}

/// A receiver with `nv` vertices, a fan of triangles and the given light
/// map type (0 none, 2 texture).
fn receiver(w: &mut W, component: i32, nv: usize, light_map: u32) {
    w.i32(component);
    w.i32(28).i32(nv as i32);
    for i in 0..nv {
        w.f32(i as f32).f32(2.0 * i as f32).f32(-1.0);
        w.bytes(&[127, 127, 255, 255]).bytes(&[255, 127, 127, 0]);
        w.f32(0.25).f32(0.75);
    }
    let tris: Vec<u16> = (1..nv.saturating_sub(1) as u16)
        .flat_map(|k| [0, k, k + 1])
        .collect();
    w.i32(2).i32(tris.len() as i32);
    for t in &tris {
        w.u16(*t);
    }
    w.u32((tris.len() / 3) as u32);
    w.u32(light_map);
    if light_map == 2 {
        w.i32(1).u32(1).u32(2).u32(3).u32(4); // one light GUID
        for k in 0..3 {
            w.i32(-(k + 1)).f32(1.0).f32(0.5).f32(0.25);
        }
        w.f32(0.5).f32(0.5).f32(0.25).f32(0.0);
    }
    w.i32(1).i32(7); // one ShadowMap1D reference
    w.i32(-1); // Data
    w.i32(-1); // InstanceIndex
}

fn sample() -> Vec<u8> {
    let mut w = W::default();
    w.i32(2);
    receiver(&mut w, 5, 4, 0);
    receiver(&mut w, 9, 3, 2);
    w.0
}

#[test]
fn native_layout_decodes_and_round_trips() {
    let bytes = sample();
    let n = decode_decal_component_native(&bytes, 0).unwrap();
    assert_eq!(n.receivers.len(), 2);
    let r = &n.receivers[0];
    assert_eq!(r.component, PackageIndex(5));
    assert_eq!(r.vertices.len(), 4);
    assert_eq!(r.vertices[2].position, [2.0, 4.0, -1.0]);
    assert_eq!(r.vertices[1].light_map_coordinate, [0.25, 0.75]);
    assert_eq!(r.indices, vec![0, 1, 2, 0, 2, 3]);
    assert_eq!(r.num_triangles, 2);
    assert_eq!(r.light_map, LightMap::None);
    assert_eq!(r.shadow_maps, vec![PackageIndex(7)]);
    assert_eq!((r.data, r.instance_index), (-1, -1));
    let r2 = &n.receivers[1];
    assert!(matches!(&r2.light_map, LightMap::TwoD(m) if m.light_guids.len() == 1));
    assert_eq!(
        encode_decal_component_native(&n, None).unwrap(),
        bytes,
        "byte-exact re-encode"
    );
    // The native data may start after a prefix (the tagged properties).
    let mut prefixed = vec![0xAA; 13];
    prefixed.extend_from_slice(&bytes);
    assert_eq!(decode_decal_component_native(&prefixed, 13).unwrap(), n);
    // No receivers at all (the 370 movable decals).
    let empty = 0i32.to_le_bytes();
    assert!(
        decode_decal_component_native(&empty, 0)
            .unwrap()
            .receivers
            .is_empty()
    );
}

#[test]
fn trailing_bytes_and_wrong_element_sizes_are_errors() {
    let mut bytes = sample();
    bytes.push(0);
    assert!(decode_decal_component_native(&bytes, 0).is_err());
    // Vertex element size other than 28.
    let mut w = W::default();
    w.i32(1).i32(1).i32(DECAL_VERTEX_SIZE as i32 + 4).i32(0);
    assert!(decode_decal_component_native(&w.0, 0).is_err());
    // Index element size other than 2.
    let mut w = W::default();
    w.i32(1).i32(1).i32(28).i32(0).i32(4).i32(0);
    assert!(decode_decal_component_native(&w.0, 0).is_err());
    // Negative and impossible counts.
    let mut w = W::default();
    w.i32(-1);
    assert!(decode_decal_component_native(&w.0, 0).is_err());
    let mut w = W::default();
    w.i32(i32::MAX);
    assert!(decode_decal_component_native(&w.0, 0).is_err());
    // Start beyond the data.
    assert!(decode_decal_component_native(&sample(), 10_000).is_err());
}

#[test]
fn truncation_at_every_offset_is_an_error() {
    let bytes = sample();
    for cut in 0..bytes.len() {
        assert!(
            decode_decal_component_native(&bytes[..cut], 0).is_err(),
            "cut at {cut}"
        );
    }
}

#[test]
fn corrupted_inputs_never_panic_and_accepted_ones_reencode() {
    let bytes = sample();
    let mut accepted = 0usize;
    for i in 0..bytes.len() {
        for v in [0u8, 1, 0x7F, 0x80, 0xFF] {
            let mut b = bytes.clone();
            b[i] = v;
            if let Ok(n) = decode_decal_component_native(&b, 0) {
                accepted += 1;
                assert_eq!(encode_decal_component_native(&n, None).unwrap(), b);
            }
        }
        for bit in 0..8 {
            let mut b = bytes.clone();
            b[i] ^= 1 << bit;
            if let Ok(n) = decode_decal_component_native(&b, 0) {
                assert_eq!(encode_decal_component_native(&n, None).unwrap(), b);
            }
        }
    }
    assert!(accepted > 0);
    // Pseudo-random multi-byte noise.
    let mut x = 0x1234_5678_u32;
    for _ in 0..2000 {
        let mut b = bytes.clone();
        for _ in 0..4 {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            let at = (x as usize) % b.len();
            b[at] = (x >> 8) as u8;
        }
        let _ = decode_decal_component_native(&b, 0);
    }
}

fn dot(a: [f32; 3], b: [f32; 3]) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

#[test]
fn frame_axes_follow_the_rotator_and_the_decal_rotation() {
    // Yaw 90°: forward +Y, the width axis (rotator Y) −X, height +Z.
    let f = DecalFrame::new([0.0; 3], [0, 16384, 0], 0.0);
    let close = |a: [f32; 3], b: [f32; 3]| a.iter().zip(b).all(|(x, y)| (x - y).abs() < 1e-5);
    assert!(close(f.direction, [0.0, 1.0, 0.0]), "{:?}", f.direction);
    assert!(close(f.width_axis, [-1.0, 0.0, 0.0]), "{:?}", f.width_axis);
    assert!(close(f.height_axis, [0.0, 0.0, 1.0]), "{:?}", f.height_axis);
    assert!(close(f.tangent(), [1.0, 0.0, 0.0]));
    // DecalRotation 90°: width becomes the old height axis.
    let r = DecalFrame::new([0.0; 3], [0, 16384, 0], 90.0);
    assert!(close(r.width_axis, f.height_axis));
    assert!(close(r.height_axis, [1.0, 0.0, 0.0]));
    // Orthonormal for an arbitrary orientation.
    let g = DecalFrame::new([1.0, 2.0, 3.0], [-3927, 36805, -821], 33.0);
    for (a, b) in [
        (g.direction, g.width_axis),
        (g.direction, g.height_axis),
        (g.width_axis, g.height_axis),
    ] {
        assert!(dot(a, b).abs() < 1e-5);
    }
    let axes = rotator_axes([-3927, 36805, -821]);
    assert!(close(g.direction, axes[0]));
}

#[test]
fn planes_contains_local_and_uv_agree() {
    let f = DecalFrame::new([10.0, 0.0, 0.0], [0, 0, 0], 0.0);
    // Forward +X, width −… rotator Y = +Y, height +Z.
    let (w, h, near, far) = (40.0, 20.0, -5.0, 50.0);
    let planes = f.planes(w, h, near, far);
    let inside = |p: [f32; 3]| {
        planes
            .iter()
            .all(|pl| dot([pl[0], pl[1], pl[2]], p) - pl[3] <= 1e-4)
    };
    for p in [
        [10.0, 0.0, 0.0],
        [60.0, 20.0, 10.0],
        [5.0, -20.0, -10.0],
        [61.0, 0.0, 0.0],
        [10.0, 21.0, 0.0],
        [4.0, 0.0, 0.0],
    ] {
        assert_eq!(inside(p), f.contains(p, w, h, near, far, 0.0), "{p:?}");
    }
    assert_eq!(f.local([12.0, 3.0, 4.0]), [3.0, 4.0, 2.0]);
    // Shader: −(decal matrix · (p − L)) + offset + 0.5, i.e. u along +W
    // (here +Y), v along −H (here −Z), centred on 0.5.
    assert_eq!(
        f.uv([10.0, 0.0, 0.0], w, h, [1.0, 1.0], [0.0, 0.0]),
        [0.5, 0.5]
    );
    assert_eq!(
        f.uv([10.0, 20.0, 0.0], w, h, [1.0, 1.0], [0.0, 0.0]),
        [1.0, 0.5]
    );
    assert_eq!(
        f.uv([10.0, 0.0, 10.0], w, h, [1.0, 1.0], [0.0, 0.0]),
        [0.5, 0.0]
    );
    assert_eq!(
        f.uv([10.0, 20.0, 0.0], w, h, [2.0, 1.0], [0.1, 0.0]),
        [1.6, 0.5]
    );
    // Degenerate sizes do not divide by zero.
    assert_eq!(
        f.uv([10.0, 5.0, 5.0], 0.0, 0.0, [1.0, 1.0], [0.0, 0.0]),
        [0.5, 0.5]
    );
}

#[test]
fn clipping_and_backfaces() {
    // Decal at the origin projecting along +X onto the plane x = 10.
    let mut params = decal_params(&[]);
    params.width = 20.0;
    params.height = 20.0;
    params.near_plane = 0.0;
    params.far_plane = 50.0;
    let f = DecalFrame::new([0.0; 3], [0, 0, 0], 0.0);
    let bx = DecalBox::new(f, &params);
    // A triangle on x = 10 facing the decal (normal −X in UE3 winding).
    let mut tri = [[10.0, -5.0, -5.0], [10.0, 0.0, 5.0], [10.0, 5.0, -5.0]];
    if dot(face_normal(tri), [1.0, 0.0, 0.0]) > 0.0 {
        tri.swap(1, 2);
    }
    assert!(dot(face_normal(tri), [-1.0, 0.0, 0.0]) > 0.99);
    let poly = bx.clip_triangle(tri).unwrap();
    assert_eq!(poly.len(), 3, "fully inside: unchanged");
    // The reversed triangle faces away: skipped unless backfaces project.
    let back = [tri[0], tri[2], tri[1]];
    assert!(bx.clip_triangle(back).is_none());
    let mut both = bx;
    both.backfaces = true;
    assert!(both.clip_triangle(back).is_some());
    // A large triangle is clipped to the box (square of side 20).
    let mut big = [
        [10.0, -100.0, -100.0],
        [10.0, 0.0, 100.0],
        [10.0, 100.0, -100.0],
    ];
    if dot(face_normal(big), [1.0, 0.0, 0.0]) > 0.0 {
        big.swap(1, 2);
    }
    let poly = bx.clip_triangle(big).unwrap();
    assert!(
        poly.iter()
            .all(|p| p[1].abs() <= 10.0 + 1e-4 && p[2].abs() <= 10.0 + 1e-4)
    );
    assert!(poly.len() >= 4);
    // bNoClip keeps the whole triangle when it touches the box.
    let mut nc = bx;
    nc.no_clip = true;
    assert_eq!(nc.clip_triangle(big).unwrap().len(), 3);
    // Beyond the far plane: nothing.
    let far = tri.map(|p| [p[0] + 100.0, p[1], p[2]]);
    assert!(bx.clip_triangle(far).is_none());
    // A single plane clip of a square.
    let square = [
        [0.0, 0.0, 0.0],
        [2.0, 0.0, 0.0],
        [2.0, 2.0, 0.0],
        [0.0, 2.0, 0.0],
    ];
    let half = clip_polygon(&square, [1.0, 0.0, 0.0, 1.0]);
    assert_eq!(half.len(), 4);
    assert!(half.iter().all(|p| p[0] <= 1.0 + 1e-6));
}

#[test]
fn mesh_builder_fans_and_normals() {
    let mut b = DecalMeshBuilder::default();
    let poly = [
        [0.0, 0.0, 0.0],
        [1.0, 0.0, 0.0],
        [1.0, 1.0, 0.0],
        [0.0, 1.0, 0.0],
    ];
    b.add_polygon(&poly, [0.0, 0.0, 1.0]);
    assert_eq!(b.positions.len(), 4);
    assert_eq!(b.triangles, vec![[0, 1, 2], [0, 2, 3]]);
    b.add_polygon(&poly[..2], [0.0, 0.0, 1.0]);
    assert_eq!(b.positions.len(), 4, "degenerate polygons are skipped");
    let n = vertex_normals(&b.positions, &b.triangles);
    // UE3 winding: (c − a) × (b − a) for (0,0)-(1,0)-(1,1) points −Z.
    assert!(n.iter().all(|v| (v[2] + 1.0).abs() < 1e-6), "{n:?}");
    assert_eq!(vertex_normals(&b.positions, &[[0, 1, 99]]).len(), 4);
    let up = unpack_normal([127, 127, 255, 0]);
    assert!(up[2] > 0.99);
}

fn prop(name: &str, value: Value) -> Property {
    Property {
        name: name.to_owned(),
        type_name: String::new(),
        array_index: 0,
        size: 0,
        struct_name: None,
        enum_name: None,
        value,
        offset: 0,
    }
}

fn vector(x: f32, y: f32, z: f32) -> Value {
    Value::Struct {
        name: "Vector".into(),
        binary: true,
        fields: vec![
            prop("X", Value::Float(x)),
            prop("Y", Value::Float(y)),
            prop("Z", Value::Float(z)),
        ],
    }
}

#[test]
fn params_read_the_effective_properties() {
    let receiver = Value::Struct {
        name: "DecalReceiver".into(),
        binary: false,
        fields: vec![prop(
            "Component",
            Value::Object(ObjRef {
                index: 3,
                path: "Map.TheWorld.PersistentLevel.A.StaticMeshComponent_0".into(),
            }),
        )],
    };
    let props = vec![
        prop(
            "DecalMaterial",
            Value::Object(ObjRef {
                index: -2,
                path: "Pkg.M_Decal".into(),
            }),
        ),
        prop("Width", Value::Float(412.5)),
        prop("Height", Value::Float(100.0)),
        prop("FarPlane", Value::Float(512.5)),
        prop("DecalRotation", Value::Float(15.0)),
        prop("bMovableDecal", Value::Bool(true)),
        prop("SortOrder", Value::Int(3)),
        prop("HitNormal", vector(0.0, 0.0, 1.0)),
        prop(
            "DecalTransform",
            Value::Enum("DecalTransform_OwnerAbsolute".into()),
        ),
        prop("DecalReceivers", Value::Array(vec![receiver])),
        prop(
            "BlendRange",
            Value::Struct {
                name: "Vector2D".into(),
                binary: true,
                fields: vec![
                    prop("X", Value::Float(89.5)),
                    prop("Y", Value::Float(180.0)),
                ],
            },
        ),
    ];
    let p = decal_params(&props);
    assert_eq!(p.material.as_deref(), Some("Pkg.M_Decal"));
    assert_eq!((p.width, p.height, p.far_plane), (412.5, 100.0, 512.5));
    assert_eq!(p.rotation_degrees, 15.0);
    assert!(p.movable_decal && !p.static_decal);
    assert_eq!(p.sort_order, 3);
    assert_eq!(p.hit_normal, [0.0, 0.0, 1.0]);
    assert_eq!(
        p.decal_transform.as_deref(),
        Some("DecalTransform_OwnerAbsolute")
    );
    assert_eq!(p.receivers.len(), 1);
    assert_eq!(p.blend_range, [89.5, 180.0]);
    // Nothing set: native zeroes.
    let z = decal_params(&[]);
    assert_eq!((z.width, z.material.clone()), (0.0, None));
}

/// A triangle on the plane `x = at` whose outward normal ([`face_normal`])
/// points along `−X` (`towards_origin`) or `+X`.
fn wall(at: f32, towards_minus_x: bool) -> [[f32; 3]; 3] {
    let mut tri = [[at, -5.0, -5.0], [at, 0.0, 5.0], [at, 5.0, -5.0]];
    let n = face_normal(tri);
    if (n[0] < 0.0) != towards_minus_x {
        tri.swap(1, 2);
    }
    tri
}

#[test]
fn mirrored_owners_project_along_the_reversed_direction() {
    // The native rule: a static decal whose owner's DrawScale3D has a
    // negative product.
    assert!(decal_is_mirrored(true, [1.0, -1.0, 1.0]));
    assert!(decal_is_mirrored(true, [-2.0, -1.0, -0.5]));
    assert!(!decal_is_mirrored(true, [1.0, 1.0, 1.0]));
    assert!(!decal_is_mirrored(true, [-1.0, -1.0, 1.0]));
    assert!(!decal_is_mirrored(false, [1.0, -1.0, 1.0]), "not static");
    assert!(!decal_is_mirrored(true, [0.0, -1.0, 1.0]));
    assert!(!decal_is_mirrored(true, [f32::NAN, 1.0, 1.0]));

    let mut params = decal_params(&[]);
    params.width = 20.0;
    params.height = 20.0;
    params.near_plane = 0.0;
    params.far_plane = 50.0;
    params.backface_angle = 0.001;
    let f = DecalFrame::new([0.0; 3], [0, 0, 0], 0.0);
    let m = f.mirrored();
    assert_eq!(m.direction, [-1.0, 0.0, 0.0]);
    assert_eq!((m.width_axis, m.height_axis), (f.width_axis, f.height_axis));
    assert_eq!(m.hit_normal(), [1.0, 0.0, 0.0]);
    assert_eq!(f.hit_normal(), [-1.0, 0.0, 0.0]);
    assert_eq!(m.mirrored(), f);
    // The box lies on the other side.
    assert!(f.contains([10.0, 0.0, 0.0], 20.0, 20.0, 0.0, 50.0, 0.0));
    assert!(!m.contains([10.0, 0.0, 0.0], 20.0, 20.0, 0.0, 50.0, 0.0));
    assert!(m.contains([-10.0, 0.0, 0.0], 20.0, 20.0, 0.0, 50.0, 0.0));
    // Texture coordinates do not change: W and H stay.
    let p = [-10.0, 4.0, -3.0];
    assert_eq!(
        m.uv(p, 20.0, 20.0, [1.0, 1.0], [0.0, 0.0]),
        f.uv(p, 20.0, 20.0, [1.0, 1.0], [0.0, 0.0])
    );
    // A wall behind the decal that faces it takes the mirrored decal only;
    // a wall in front that faces it takes the straight one only.
    let (straight, mirrored) = (DecalBox::new(f, &params), DecalBox::new(m, &params));
    let behind = wall(-10.0, false);
    let front = wall(10.0, true);
    assert!(straight.clip_triangle(front).is_some());
    assert!(mirrored.clip_triangle(front).is_none());
    assert!(mirrored.clip_triangle(behind).is_some());
    assert!(straight.clip_triangle(behind).is_none());
    // The far side of the wall behind (facing away from the origin) takes
    // neither.
    let behind_far_side = wall(-10.0, true);
    assert!(mirrored.clip_triangle(behind_far_side).is_none());
    assert!(straight.clip_triangle(behind_far_side).is_none());
}

#[test]
fn face_test_follows_the_backface_angle() {
    let mut params = decal_params(&[]);
    params.width = 1000.0;
    params.height = 1000.0;
    params.near_plane = -500.0;
    params.far_plane = 500.0;
    params.backface_angle = 0.001;
    let f = DecalFrame::new([0.0; 3], [0, 0, 0], 0.0);
    let bx = DecalBox::new(f, &params);
    assert_eq!(bx.backface_angle, 0.001);
    // Facing: cosine 1.
    assert!(bx.faces(wall(10.0, true)));
    assert!(!bx.faces(wall(10.0, false)));
    // Grazing triangles: outward normal (−c, 0, ±√(1−c²)).
    let grazing = |c: f32| {
        let s = (1.0 - c * c).sqrt();
        // Two edges in the plane with that normal: (0,1,0) and (s,0,c).
        let mut tri = [[0.0, 0.0, 0.0], [0.0, 10.0, 0.0], [10.0 * s, 0.0, 10.0 * c]];
        if face_normal(tri)[0] > 0.0 {
            tri.swap(1, 2);
        }
        tri
    };
    assert!(bx.faces(grazing(0.01)));
    assert!(!bx.faces(grazing(0.0005)), "below BackfaceAngle");
    // bProjectOnBackfaces: back faces count, edge-on ones still do not.
    let mut both = bx;
    both.backfaces = true;
    assert!(both.faces(wall(10.0, false)));
    let mut edge = grazing(0.0005);
    assert!(!both.faces(edge));
    edge.swap(1, 2);
    assert!(!both.faces(edge));
    // A degenerate triangle has no normal: never a receiver.
    let degenerate = [[0.0; 3], [1.0, 0.0, 0.0], [2.0, 0.0, 0.0]];
    assert!(!bx.faces(degenerate) && !both.faces(degenerate));
    assert!(bx.clip_triangle(degenerate).is_none());
    // A negative angle with backfaces keeps every face (cooked receivers).
    let all = DecalBox {
        backfaces: true,
        backface_angle: -1.0,
        ..bx
    };
    assert!(all.faces(wall(10.0, false)) && all.faces(wall(10.0, true)));
    assert!(all.faces(grazing(0.0005)));
    // A non-finite angle in the properties falls back to zero.
    params.backface_angle = f32::NAN;
    assert_eq!(DecalBox::new(f, &params).backface_angle, 0.0);
    // Non-finite geometry is refused, not propagated.
    let nan = [[f32::NAN, 0.0, 0.0], [10.0, 0.0, 5.0], [10.0, 5.0, -5.0]];
    assert!(bx.clip_triangle(nan).is_none());
    let inf = [
        [10.0, f32::INFINITY, 0.0],
        [10.0, 0.0, 5.0],
        [10.0, 5.0, -5.0],
    ];
    assert!(
        bx.clip_triangle(inf)
            .is_none_or(|p| p.iter().flatten().all(|c| c.is_finite()))
    );
}

#[test]
fn frame_bounds_cover_the_box() {
    let f = DecalFrame::new([10.0, 20.0, 30.0], [0, 0, 0], 0.0);
    let (lo, hi) = f.bounds(40.0, 20.0, -5.0, 50.0);
    assert_eq!(lo, [5.0, 0.0, 20.0]);
    assert_eq!(hi, [60.0, 40.0, 40.0]);
    // Mirrored: the same box on the other side of the origin.
    let (lo, hi) = f.mirrored().bounds(40.0, 20.0, -5.0, 50.0);
    assert_eq!(lo, [-40.0, 0.0, 20.0]);
    assert_eq!(hi, [15.0, 40.0, 40.0]);
    // An arbitrary orientation: every point inside lies in the bounds.
    let g = DecalFrame::new([1.0, 2.0, 3.0], [-3927, 36805, -821], 33.0);
    let (w, h, near, far) = (123.0, 45.0, 10.0, 80.0);
    let (lo, hi) = g.bounds(w, h, near, far);
    for a in [-0.5f32, 0.0, 0.5] {
        for b in [-0.5f32, 0.25, 0.5] {
            for d in [near, 40.0, far] {
                let p: Vec<f32> = (0..3)
                    .map(|k| {
                        g.origin[k]
                            + g.width_axis[k] * a * w
                            + g.height_axis[k] * b * h
                            + g.direction[k] * d
                    })
                    .collect();
                assert!(g.contains([p[0], p[1], p[2]], w, h, near, far, 1e-2));
                for k in 0..3 {
                    assert!(p[k] >= lo[k] - 1e-3 && p[k] <= hi[k] + 1e-3, "{p:?}");
                }
            }
        }
    }
}

#[test]
fn mesh_triangles_drop_repeats_and_bad_indices() {
    let positions = vec![
        [0.0, 0.0, 0.0],
        [2.0, 0.0, 0.0],
        [0.0, 3.0, 0.0],
        [0.0, 0.0, 9.0],
    ];
    let m = MeshTriangles::new(
        positions.clone(),
        [[0, 1, 2], [0, 1, 2], [0, 1, 99], [u32::MAX, 0, 1]],
    );
    assert_eq!(
        m.indices,
        vec![0, 1, 2],
        "one triangle: repeat and bad dropped"
    );
    // Bounds cover the used vertices only (vertex 3 is unused).
    assert_eq!(m.bounds, Some(([0.0, 0.0, 0.0], [2.0, 3.0, 0.0])));
    // The same corners in another order are another triangle (winding).
    let m = MeshTriangles::new(positions.clone(), [[0, 1, 2], [0, 2, 1]]);
    assert_eq!(m.indices.len(), 6);
    // No collision triangles: nothing to receive a decal, no bounds.
    let m = MeshTriangles::new(positions, Vec::<[u32; 3]>::new());
    assert!(m.indices.is_empty() && m.bounds.is_none());
    // A triangle with a non-finite corner is dropped (and cannot widen the
    // bounds).
    let m = MeshTriangles::new(
        vec![[f32::NAN, 0.0, 0.0], [1.0; 3], [2.0; 3], [3.0; 3]],
        [[0, 1, 2], [1, 2, 3]],
    );
    assert_eq!(m.indices, vec![1, 2, 3]);
    assert_eq!(m.bounds, Some(([1.0; 3], [3.0; 3])));
    let m = MeshTriangles::new(vec![[f32::INFINITY; 3], [1.0; 3], [2.0; 3]], [[0, 1, 2]]);
    assert!(m.indices.is_empty() && m.bounds.is_none());
}

#[test]
fn world_receiver_acceptance_overlap_and_filter() {
    let receiver = |accepts_static, accepts_dynamic| WorldReceiver {
        export: 1,
        path: "Map.TheWorld.PersistentLevel.StaticMeshActor_3.StaticMeshComponent_0".into(),
        level: 0,
        min: [0.0, 0.0, 0.0],
        max: [10.0, 10.0, 10.0],
        accepts_static,
        accepts_dynamic,
        hidden: false,
    };
    let decal = |static_decal, movable_decal| {
        let mut p = decal_params(&[]);
        p.static_decal = static_decal;
        p.movable_decal = movable_decal;
        p
    };
    // (accepts static, accepts dynamic) × (static, movable) → attached.
    for (rs, rd, ds, dm, want) in [
        (true, false, true, false, true), // placed static decal on a static mesh
        (true, false, false, false, false), // dynamic decal needs the dynamic flag
        (true, false, false, true, true), // movable counts for either flag
        (false, true, true, false, false), // static decal, dynamic-only receiver
        (false, true, true, true, true),  // the movable decal actors
        (false, true, false, false, true), // run-time (gameplay) decal
        (false, false, true, true, false),
        (true, true, true, true, true),
    ] {
        assert_eq!(
            receiver(rs, rd).accepts(&decal(ds, dm)),
            want,
            "receiver ({rs}, {rd}) decal ({ds}, {dm})"
        );
    }
    let r = receiver(true, true);
    assert!(r.overlaps([5.0, 5.0, 5.0], [20.0, 20.0, 20.0]));
    assert!(
        r.overlaps([10.0, 10.0, 10.0], [20.0, 20.0, 20.0]),
        "touching"
    );
    assert!(!r.overlaps([10.1, 0.0, 0.0], [20.0, 20.0, 20.0]));
    assert!(!r.overlaps([0.0, 0.0, -5.0], [10.0, 10.0, -0.1]));
    assert!(!r.overlaps([f32::NAN; 3], [f32::NAN; 3]));
    // Actor filter.
    let owner = "Map.TheWorld.PersistentLevel.StaticMeshActor_3";
    let mut p = decal_params(&[]);
    assert!(filter_passes(&p, Some(owner)) && filter_passes(&p, None));
    p.filter = vec!["map.theworld.persistentlevel.staticmeshactor_3".into()];
    p.filter_mode = Some("FM_Ignore".into());
    assert!(!filter_passes(&p, Some(owner)));
    assert!(filter_passes(
        &p,
        Some("Map.TheWorld.PersistentLevel.Other")
    ));
    assert!(filter_passes(&p, None));
    p.filter_mode = Some("FM_Affect".into());
    assert!(filter_passes(&p, Some(owner)));
    assert!(!filter_passes(
        &p,
        Some("Map.TheWorld.PersistentLevel.Other")
    ));
    assert!(!filter_passes(&p, None));
    p.filter_mode = Some("FM_None".into());
    assert!(filter_passes(&p, Some(owner)));
}

#[test]
fn clipping_never_panics_or_grows_without_bound_on_wild_input() {
    // Pseudo-random boxes and triangles, including huge, tiny and
    // non-finite values: clipping returns a small polygon or nothing.
    let mut x = 0x9E37_79B9_u32;
    let mut next = move || {
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        let pick = x % 16;
        let v = (x >> 8) as f32 / 16_777_216.0 * 2.0 - 1.0;
        match pick {
            0 => f32::NAN,
            1 => f32::INFINITY,
            2 => f32::NEG_INFINITY,
            3 => v * 1e30,
            4 => v * 1e-30,
            5 => 0.0,
            _ => v * 500.0,
        }
    };
    let mut kept = 0usize;
    for round in 0..4000 {
        let mut params = decal_params(&[]);
        params.width = next().abs();
        params.height = next().abs();
        params.near_plane = next();
        params.far_plane = next();
        params.backface_angle = if round % 7 == 0 { next() } else { 0.001 };
        params.project_on_backfaces = round % 3 == 0;
        params.no_clip = round % 11 == 0;
        let rot = [
            (next() * 100.0) as i32,
            (next() * 100.0) as i32,
            (next() * 100.0) as i32,
        ];
        let f = DecalFrame::new([next(), next(), next()], rot, next());
        let bx = DecalBox::new(f, &params);
        let tri = [
            [next(), next(), next()],
            [next(), next(), next()],
            [next(), next(), next()],
        ];
        if let Some(poly) = bx.clip_triangle(tri) {
            // A triangle clipped by six planes has at most nine corners.
            assert!((3..=9).contains(&poly.len()), "{}", poly.len());
            kept += 1;
        }
        let mut b = DecalMeshBuilder::default();
        b.project(&bx, [tri]);
        assert!(b.positions.len() <= 9 && b.triangles.len() <= 7);
        let _ = f.bounds(
            params.width,
            params.height,
            params.near_plane,
            params.far_plane,
        );
        let _ = f.uv(
            tri[0],
            params.width,
            params.height,
            [next(), next()],
            [next(), next()],
        );
    }
    assert!(kept > 0, "some rounds must clip to something");
}
