//! `RB_BodySetup` decoding over synthetic data written byte by byte in
//! `bodysetup_common` (no original game data): every field of a hand-written
//! payload, the exact re-encode, the stored order of planes and matrices,
//! the permuted vertex data, the structural cross-checks, class default
//! objects, and the decoder and the static-mesh switches inside a synthetic
//! package.

#![allow(clippy::unwrap_used)]

mod bodysetup_common;
mod common;

use asamu_ue3::bodysetup::{
    BodyProperty, BodySetup, ElemBox, NameIndex, Plane, ScalarValue, body_setup_coverage,
    decode_body_setup, decode_body_setup_payload, encode_body_setup, is_affine, is_body_setup,
    permute_vertex_data, validate_body_setup,
};
use asamu_ue3::object::ObjectError;
use asamu_ue3::staticmesh::{
    BoxSphereBounds, KdopBounds, KdopTree, SimpleCollisionFlags, StaticMeshNative,
    decode_static_mesh, encode_static_mesh_native,
};
use asamu_ue3::types::{FName, Guid};
use asamu_ue3::{LoadedPackage, NoSchema, Package, PackageIndex};
use bodysetup_common::{
    B, BLOB, BOX_TM, E, IDENTITY, PYRAMID, PYRAMID_EDGES, PYRAMID_PLANES, PYRAMID_TRIS, SPHERE_TM,
    class_default_object, fixture, n, names,
};
use common::{Export, Import, Synth};

fn decode(bytes: &[u8]) -> BodySetup {
    decode_body_setup_payload(bytes, &names(), false).unwrap()
}

fn plane(p: [f32; 4]) -> Plane {
    Plane {
        x: p[0],
        y: p[1],
        z: p[2],
        w: p[3],
    }
}

#[test]
fn handwritten_payload_decodes_field_by_field() {
    let fx = fixture();
    let b = decode(&fx.bytes);
    assert_eq!(b.net_index, 7);
    assert_eq!(b.properties.len(), 9);

    // Scalar tags, in stream order.
    let scalars: Vec<(&str, &ScalarValue)> =
        b.scalars().map(|s| (s.name.as_str(), &s.value)).collect();
    assert_eq!(scalars.len(), 6);
    assert_eq!(scalars[0].0, "SleepFamily");
    assert_eq!(
        *scalars[0].1,
        ScalarValue::Enum {
            enum_name: "ESleepFamily".to_owned(),
            raw_enum: FName {
                index: n("ESleepFamily"),
                number: 0
            },
            value: "SF_Sensitive".to_owned(),
            raw_value: FName {
                index: n("SF_Sensitive"),
                number: 0
            },
        }
    );
    // Instance number 3 displays as `_2`.
    assert_eq!(
        *b.scalar("bonename").unwrap(),
        ScalarValue::Name {
            text: "Pelvis_2".to_owned(),
            raw: FName {
                index: n("Pelvis"),
                number: 3
            }
        }
    );
    assert_eq!(b.bool_property("bNoCollision"), Some(true));
    assert_eq!(b.bool_property("MassScale"), None);
    assert_eq!(
        *b.scalar("PhysMaterial").unwrap(),
        ScalarValue::Object(PackageIndex(-5))
    );
    assert_eq!(*b.scalar("MassScale").unwrap(), ScalarValue::Float(2.5));
    assert_eq!(
        *b.scalar("PreCachedPhysDataVersion").unwrap(),
        ScalarValue::Int(12345)
    );
    assert!(b.scalar("Nope").is_none());
    assert_eq!(
        b.pre_cached_phys_scale().unwrap(),
        [[1.0, 1.0, 1.0], [2.0, 2.0, 0.5]]
    );
    assert!(
        b.properties
            .iter()
            .any(|p| *p == BodyProperty::ComNudge([0.25, -0.5, 1.0]))
    );

    let g = b.agg_geom().unwrap();
    assert_eq!(g.element_count(), 4);
    assert_eq!(b.element_count(), 4);
    assert_eq!(g.skip_close_and_parallel_checks, Some(true));

    let s = &g.spheres()[0];
    assert_eq!(s.tm, Some(SPHERE_TM));
    assert_eq!(s.radius, Some(4.0));
    assert_eq!(
        (s.no_rb_collision, s.per_poly_shape),
        (Some(false), Some(true))
    );

    let bx = &g.boxes()[0];
    assert_eq!(bx.tm, Some(BOX_TM));
    assert_eq!((bx.x, bx.y, bx.z), (Some(2.0), Some(4.0), Some(6.0)));
    assert_eq!(
        (bx.no_rb_collision, bx.per_poly_shape),
        (Some(true), Some(false))
    );

    let sp = &g.sphyls()[0];
    assert_eq!(sp.tm, Some(IDENTITY));
    assert_eq!((sp.radius, sp.length), (Some(1.5), Some(8.0)));
    assert_eq!(
        (sp.no_rb_collision, sp.per_poly_shape),
        (Some(false), Some(false))
    );

    let c = &g.convex()[0];
    assert_eq!(c.vertex_data.as_deref().unwrap(), PYRAMID);
    assert_eq!(c.face_tri_data.as_deref().unwrap(), PYRAMID_TRIS);
    assert_eq!(c.edge_directions.as_deref().unwrap(), PYRAMID_EDGES);
    let planes: Vec<Plane> = PYRAMID_PLANES.iter().map(|p| plane(*p)).collect();
    assert_eq!(c.face_plane_data.as_deref().unwrap(), planes);
    let normals: Vec<[f32; 3]> = PYRAMID_PLANES.iter().map(|p| [p[0], p[1], p[2]]).collect();
    assert_eq!(c.face_normal_directions.as_deref().unwrap(), normals);
    assert_eq!(
        c.elem_box,
        Some(ElemBox {
            min: [-1.0, -1.0, 0.5],
            max: [1.0, 1.0, 2.5],
            is_valid: 1
        })
    );
    // Vertices 0..3 by coordinate, then the apex four times.
    assert_eq!(
        c.permuted_vertex_data.as_deref().unwrap(),
        [
            plane([-1.0, 1.0, 1.0, -1.0]),
            plane([-1.0, -1.0, 1.0, 1.0]),
            plane([0.5, 0.5, 0.5, 0.5]),
            plane([0.0, 0.0, 0.0, 0.0]),
            plane([0.0, 0.0, 0.0, 0.0]),
            plane([2.5, 2.5, 2.5, 2.5]),
        ]
    );

    // Native data: one entry per scale, one blob per convex element.
    let cached = b.cached_data();
    assert_eq!(cached.len(), 2);
    assert_eq!(cached[0].elements.len(), 1);
    assert_eq!(cached[0].elements[0].0, BLOB);
    assert_eq!(cached[1].elements.len(), 1);
    assert!(cached[1].elements[0].0.is_empty());
    assert_eq!(b.cached_bytes(), 3);
    assert_eq!(
        b.native_start,
        fx.bytes.len() - (4 + 4 + 4 + 4 + 3 + 4 + 4 + 4)
    );

    // The fixture is consistent.
    assert_eq!(validate_body_setup(&b), Vec::<String>::new());
}

#[test]
fn encoder_reproduces_the_handwritten_payload() {
    let fx = fixture();
    let b = decode(&fx.bytes);
    let index = NameIndex::new(&names());
    assert_eq!(encode_body_setup(&b, &index).unwrap(), fx.bytes);
    // Decoding what was encoded gives the same value again.
    assert_eq!(decode(&encode_body_setup(&b, &index).unwrap()), b);
    // A name table as a slice works too, and a table without the needed
    // names cannot encode.
    let slice: &[&str] = bodysetup_common::NAMES;
    assert_eq!(
        decode_body_setup_payload(&fx.bytes, &slice, false).unwrap(),
        b
    );
    let short: Vec<&str> = vec!["None", "AggGeom"];
    assert!(encode_body_setup(&b, &NameIndex::new(&short)).is_none());
    // Names compare without regard to case.
    let upper: Vec<String> = names().iter().map(|s| s.to_ascii_uppercase()).collect();
    assert_eq!(
        decode_body_setup_payload(&fx.bytes, &upper, false)
            .unwrap()
            .element_count(),
        4
    );
}

#[test]
fn planes_and_matrices_are_stored_w_first() {
    // One convex element with a single plane (1, 2, 3 | 4) and one sphere
    // whose matrix rows are (1..4), (5..8), (9..12), (13..16).
    let mut e = B::default();
    e.float_array_tag("FacePlaneData", 1, &[4.0, 1.0, 2.0, 3.0]);
    e.name("None");
    let mut convex = B::default();
    convex.count(1).append(&e);
    let mut s = B::default();
    let mut tm = B::default();
    tm.floats(&[
        4.0, 1.0, 2.0, 3.0, 8.0, 5.0, 6.0, 7.0, 12.0, 9.0, 10.0, 11.0, 16.0, 13.0, 14.0, 15.0,
    ]);
    s.count(1)
        .tag("TM", "StructProperty", E::Struct("Matrix"), &tm)
        .name("None");
    let mut g = B::default();
    g.tag("SphereElems", "ArrayProperty", E::No, &s);
    g.tag("ConvexElems", "ArrayProperty", E::No, &convex);
    g.name("None");
    let mut b = B::default();
    b.i32(0);
    b.tag("AggGeom", "StructProperty", E::Struct("KAggregateGeom"), &g);
    b.name("None");
    b.count(0);
    let body = decode(&b.bytes);
    let geom = body.agg_geom().unwrap();
    assert_eq!(
        geom.convex()[0].face_plane_data.as_deref().unwrap(),
        [plane([1.0, 2.0, 3.0, 4.0])]
    );
    let m = geom.spheres()[0].tm.unwrap();
    assert_eq!(
        m,
        [
            [1.0, 2.0, 3.0, 4.0],
            [5.0, 6.0, 7.0, 8.0],
            [9.0, 10.0, 11.0, 12.0],
            [13.0, 14.0, 15.0, 16.0]
        ]
    );
    assert!(!is_affine(&m));
    assert!(is_affine(&SPHERE_TM) && is_affine(&BOX_TM));
    // Absent members stay absent (and are reported by the cross-checks).
    assert!(geom.convex()[0].vertex_data.is_none());
    assert!(geom.spheres()[0].radius.is_none());
    assert!(geom.box_elems.is_none() && geom.boxes().is_empty());
    let issues = validate_body_setup(&body);
    assert!(
        issues
            .iter()
            .any(|i| i.contains("convex 0: a member is absent"))
    );
    assert!(
        issues
            .iter()
            .any(|i| i.contains("sphere 0: TM is not affine"))
    );
    assert!(
        issues
            .iter()
            .any(|i| i.contains("sphere 0: Radius is absent"))
    );
    // Partial bodies re-encode exactly as well.
    assert_eq!(
        encode_body_setup(&body, &NameIndex::new(&names())).unwrap(),
        b.bytes
    );
}

#[test]
fn permuted_vertex_data_follows_the_rule() {
    let v = |i: u8| [f32::from(i), f32::from(i) + 0.25, f32::from(i) + 0.5];
    let verts = |k: u8| (0..k).map(v).collect::<Vec<_>>();
    assert!(permute_vertex_data(&[]).is_empty());
    // A full group: component i of each plane is vertex i.
    assert_eq!(
        permute_vertex_data(&verts(4)),
        [
            plane([0.0, 1.0, 2.0, 3.0]),
            plane([0.25, 1.25, 2.25, 3.25]),
            plane([0.5, 1.5, 2.5, 3.5]),
        ]
    );
    // Five, six and seven vertices: the second group repeats its first
    // vertex (vertex 4) in the missing places.
    for (k, xs) in [
        (5u8, [4.0, 4.0, 4.0, 4.0]),
        (6, [4.0, 5.0, 4.0, 4.0]),
        (7, [4.0, 5.0, 6.0, 4.0]),
    ] {
        let p = permute_vertex_data(&verts(k));
        assert_eq!(p.len(), 6, "{k}");
        assert_eq!(p[3], plane(xs), "{k}");
        assert_eq!(p[4], plane(xs.map(|x| x + 0.25)), "{k}");
        assert_eq!(p[5], plane(xs.map(|x| x + 0.5)), "{k}");
    }
    // One to three vertices: a single padded group.
    assert_eq!(permute_vertex_data(&verts(1))[0], plane([0.0; 4]));
    assert_eq!(
        permute_vertex_data(&verts(3))[0],
        plane([0.0, 1.0, 2.0, 0.0])
    );
    assert_eq!(permute_vertex_data(&verts(9)).len(), 9);
}

#[test]
fn validation_reports_inconsistencies() {
    let base = decode(&fixture().bytes);
    let check = |edit: &dyn Fn(&mut BodySetup), needle: &str| {
        let mut b = base.clone();
        edit(&mut b);
        let issues = validate_body_setup(&b);
        assert!(
            issues.iter().any(|i| i.contains(needle)),
            "expected {needle:?} in {issues:?}"
        );
    };
    fn geom(b: &mut BodySetup) -> &mut asamu_ue3::bodysetup::AggGeom {
        b.properties
            .iter_mut()
            .find_map(|p| match p {
                BodyProperty::AggGeom(g) => Some(g),
                _ => None,
            })
            .unwrap()
    }
    fn convex(b: &mut BodySetup) -> &mut asamu_ue3::bodysetup::ConvexElem {
        &mut geom(b).convex_elems.as_mut().unwrap()[0]
    }
    check(
        &|b| convex(b).permuted_vertex_data.as_mut().unwrap()[3].w = 9.0,
        "PermutedVertexData is not the permutation",
    );
    check(
        &|b| {
            convex(b).permuted_vertex_data.as_mut().unwrap().pop();
        },
        "PermutedVertexData is not the permutation",
    );
    check(
        &|b| convex(b).elem_box.as_mut().unwrap().max[2] = 3.0,
        "ElemBox is not the vertices' bounding box",
    );
    check(
        &|b| convex(b).elem_box.as_mut().unwrap().is_valid = 0,
        "ElemBox.IsValid is 0",
    );
    check(
        &|b| convex(b).face_tri_data.as_mut().unwrap()[4] = 5,
        "FaceTriData index 5 outside the 5 vertices",
    );
    check(
        &|b| convex(b).face_tri_data.as_mut().unwrap()[0] = -1,
        "FaceTriData index -1",
    );
    check(
        &|b| {
            convex(b).face_tri_data.as_mut().unwrap().pop();
        },
        "not whole triangles",
    );
    check(
        &|b| convex(b).vertex_data.as_mut().unwrap()[1][0] = f32::NAN,
        "a value is not finite",
    );
    check(
        &|b| convex(b).face_plane_data.as_mut().unwrap()[0].w = f32::INFINITY,
        "a value is not finite",
    );
    check(&|b| convex(b).edge_directions = None, "a member is absent");
    check(
        &|b| {
            geom(b).sphere_elems.as_mut().unwrap()[0]
                .tm
                .as_mut()
                .unwrap()[3][3] = 0.0
        },
        "sphere 0: TM is not affine",
    );
    check(
        &|b| geom(b).box_elems.as_mut().unwrap()[0].tm.as_mut().unwrap()[0][0] = f32::NAN,
        "box 0: TM is not finite",
    );
    check(
        &|b| geom(b).sphyl_elems.as_mut().unwrap()[0].tm = None,
        "sphyl 0: TM is absent",
    );
    check(
        &|b| geom(b).sphere_elems.as_mut().unwrap()[0].radius = Some(-1.0),
        "sphere 0: Radius is -1",
    );
    check(
        &|b| geom(b).box_elems.as_mut().unwrap()[0].y = None,
        "box 0: Y is absent",
    );
    check(
        &|b| geom(b).sphyl_elems.as_mut().unwrap()[0].length = Some(f32::NAN),
        "sphyl 0: Length is NaN",
    );
    check(
        &|b| {
            b.pre_cached_phys_data.as_mut().unwrap().pop();
        },
        "1 pre-cooked entries for 2 pre-cooked scales",
    );
    check(
        &|b| b.pre_cached_phys_data.as_mut().unwrap()[1].elements.clear(),
        "pre-cooked entry 1 has 0 blobs for 1 convex elements",
    );
    check(
        &|b| {
            for p in &mut b.properties {
                if let BodyProperty::PreCachedPhysScale(v) = p {
                    v[0][1] = f32::NAN;
                }
            }
        },
        "PreCachedPhysScale has a non-finite value",
    );
}

#[test]
fn class_default_objects_have_no_native_data() {
    let cdo = class_default_object();
    let b = decode_body_setup_payload(&cdo.bytes, &names(), true).unwrap();
    assert_eq!(b.net_index, -1);
    assert!(b.pre_cached_phys_data.is_none() && b.cached_data().is_empty());
    assert!(b.agg_geom().is_none());
    assert_eq!(b.element_count(), 0);
    assert_eq!(b.bool_property("bNoCollision"), Some(false));
    assert_eq!(b.native_start, cdo.bytes.len());
    assert_eq!(validate_body_setup(&b), Vec::<String>::new());
    assert_eq!(
        encode_body_setup(&b, &NameIndex::new(&names())).unwrap(),
        cdo.bytes
    );
    // Read as an ordinary object the native data is missing ...
    assert!(decode_body_setup_payload(&cdo.bytes, &names(), false).is_err());
    // ... and an ordinary object read as a class default object has bytes left.
    assert!(matches!(
        decode_body_setup_payload(&fixture().bytes, &names(), true),
        Err(ObjectError::Malformed {
            what: "RB_BodySetup native data",
            ..
        })
    ));
}

// ---------------------------------------------------------------------------
// Inside a package
// ---------------------------------------------------------------------------

const CDO: u64 = 0x0000_0000_0000_0200;
const HAS_STACK: u64 = 0x0200_0000_0000_0000;

fn empty_mesh_tail(body_setup: i32) -> Vec<u8> {
    let native = StaticMeshNative {
        start: 0,
        bounds: BoxSphereBounds {
            origin: [0.0; 3],
            box_extent: [1.0, 1.0, 2.5],
            sphere_radius: 3.0,
        },
        body_setup: PackageIndex(body_setup),
        kdop: KdopTree {
            root_bounds: KdopBounds {
                min: [0.0; 3],
                max: [0.0; 3],
            },
            nodes: vec![],
            triangles: vec![],
        },
        internal_version: 18,
        source_data: None,
        optimization_settings: vec![],
        has_been_simplified: 0,
        is_mesh_proxy: 0,
        lods: vec![],
        lod_info_count: 0,
        thumbnail_angle: [0; 3],
        thumbnail_distance: 0.0,
        high_res_source_mesh_name: String::new(),
        high_res_source_mesh_crc: 0,
        lighting_guid: Guid {
            a: 1,
            b: 2,
            c: 3,
            d: 4,
        },
        vertex_position_version: 0,
        cached_streaming_texture_factors: vec![],
        remove_degenerates: 0,
        per_lod_static_lighting_for_instancing: 0,
        console_prealloc_instance_count: 0,
    };
    encode_static_mesh_native(&native).unwrap()
}

/// A package whose name table starts with the fixture's names:
/// exports 0 `Mesh` (StaticMesh), 1 `Mesh.Body` (RB_BodySetup, the fixture),
/// 2 `Default__RB_BodySetup` (class default object), 3 `Stacked`
/// (RB_BodySetup flagged as having a state frame), 4 `Plain` (StaticMesh
/// without any tag).
fn package() -> Package {
    let mut all: Vec<String> = names();
    let mut idx = |s: &str| -> i32 {
        if let Some(i) = all.iter().position(|x| x == s) {
            return i as i32;
        }
        all.push(s.to_owned());
        (all.len() - 1) as i32
    };
    let (core, package, class, engine) = (idx("Core"), idx("Package"), idx("Class"), idx("Engine"));
    let imp = |class_name, outer, name| Import {
        class_package: core,
        class_name,
        outer,
        name,
        number: 0,
    };
    let imports = vec![
        imp(package, 0, engine),             // -1 Engine
        imp(class, -1, idx("RB_BodySetup")), // -2 Engine.RB_BodySetup
        imp(class, -1, idx("StaticMesh")),   // -3 Engine.StaticMesh
    ];
    // The mesh: BodySetup = export 2 (index 1), UseSimpleBoxCollision = false.
    let name_ref = |w: &mut Vec<u8>, i: i32| {
        w.extend_from_slice(&i.to_le_bytes());
        w.extend_from_slice(&0i32.to_le_bytes());
    };
    let (body_tag, object_property) = (idx("BodySetup"), n("ObjectProperty"));
    let (box_tag, bool_property) = (idx("UseSimpleBoxCollision"), n("BoolProperty"));
    let mut mesh = Vec::new();
    mesh.extend_from_slice(&0i32.to_le_bytes()); // NetIndex
    name_ref(&mut mesh, body_tag);
    name_ref(&mut mesh, object_property);
    mesh.extend_from_slice(&4i32.to_le_bytes());
    mesh.extend_from_slice(&0i32.to_le_bytes());
    mesh.extend_from_slice(&2i32.to_le_bytes());
    name_ref(&mut mesh, box_tag);
    name_ref(&mut mesh, bool_property);
    mesh.extend_from_slice(&0i32.to_le_bytes());
    mesh.extend_from_slice(&0i32.to_le_bytes());
    mesh.push(0);
    name_ref(&mut mesh, n("None"));
    mesh.extend_from_slice(&empty_mesh_tail(2));
    let mut plain = Vec::new();
    plain.extend_from_slice(&0i32.to_le_bytes());
    name_ref(&mut plain, n("None"));
    plain.extend_from_slice(&empty_mesh_tail(0));

    let exp = |class, outer, name, flags, payload: Vec<u8>| Export {
        class,
        super_: 0,
        outer,
        name,
        number: 0,
        archetype: 0,
        object_flags: flags,
        payload,
        export_flags: 0,
        net_counts: Vec::new(),
        guid: [0; 4],
        package_flags: 0,
    };
    let flags = 0x0007_0000_0000_0000u64;
    let exports = vec![
        exp(-3, 0, idx("Mesh"), flags, mesh),
        exp(-2, 1, idx("Body"), flags, fixture().bytes),
        exp(
            -2,
            0,
            idx("Default__RB_BodySetup"),
            flags | CDO,
            class_default_object().bytes,
        ),
        exp(-2, 0, idx("Stacked"), flags | HAS_STACK, fixture().bytes),
        exp(-3, 0, idx("Plain"), flags, plain),
    ];
    let mut synth = Synth::sample();
    synth.names = all.iter().map(|s| (s.clone(), 0u64)).collect();
    synth.imports = imports;
    synth.exports = exports;
    synth.package_flags = 0x0002_0008;
    synth.texture_allocations = Vec::new();
    synth.additional_packages = Vec::new();
    Package::from_bytes(synth.build().0).unwrap()
}

#[test]
fn body_setups_decode_inside_a_package() {
    let pkg = package();
    assert_eq!(
        (0..5).map(|i| is_body_setup(&pkg, i)).collect::<Vec<_>>(),
        [false, true, true, true, false]
    );
    let body = decode_body_setup(&pkg, 1).unwrap();
    assert_eq!(body, decode(&fixture().bytes));
    assert_eq!(
        encode_body_setup(&body, &NameIndex::new(&pkg)).unwrap(),
        pkg.export_data(1).unwrap()
    );
    // The class default object is recognised by its flag.
    let cdo = decode_body_setup(&pkg, 2).unwrap();
    assert!(cdo.pre_cached_phys_data.is_none());
    // Other classes, state frames and bad indices are refused.
    assert!(matches!(
        decode_body_setup(&pkg, 0),
        Err(ObjectError::WrongKind { export: 0, .. })
    ));
    assert!(matches!(
        decode_body_setup(&pkg, 3),
        Err(ObjectError::Malformed {
            what: "RB_BodySetup prelude",
            ..
        })
    ));
    assert!(decode_body_setup(&pkg, 99).is_err());
    assert!(!is_body_setup(&pkg, 99));

    let lp = LoadedPackage::new("Synth", "Synth.upk", pkg);
    let cov = body_setup_coverage(&lp);
    assert_eq!(
        (cov.total, cov.exact, cov.round_trip, cov.valid),
        (3, 2, 2, 2)
    );
    assert_eq!(cov.failures.len(), 1);
    assert!(cov.failures[0].starts_with("3: "));
    assert_eq!(cov.owners.get("StaticMesh"), Some(&1));
    assert_eq!(cov.owners.get("<none>"), Some(&1));
    assert_eq!(
        (
            cov.with_agg_geom,
            cov.empty_agg_geom,
            cov.convex,
            cov.boxes,
            cov.spheres,
            cov.sphyls
        ),
        (1, 0, 1, 1, 1, 1)
    );
    assert_eq!((cov.convex_vertices, cov.convex_planes), (5, 5));
    assert_eq!(
        (cov.with_cached_data, cov.cached_blobs, cov.cached_bytes),
        (1, 2, 3)
    );
    assert_eq!(cov.scalar_tags.get("bNoCollision"), Some(&2));
    assert_eq!(cov.scalar_tags.get("BoneName"), Some(&1));
}

#[test]
fn static_mesh_switches_default_to_true() {
    let pkg = package();
    let mesh = decode_static_mesh(&pkg, Some("Synth"), 0, &NoSchema).unwrap();
    assert_eq!(mesh.stored_use_simple_box_collision(), Some(false));
    assert_eq!(mesh.stored_use_simple_line_collision(), None);
    assert_eq!(mesh.stored_use_simple_rigid_body_collision(), None);
    assert_eq!(
        mesh.simple_collision_flags(),
        SimpleCollisionFlags {
            line: true,
            box_: false,
            rigid_body: true
        }
    );
    // The tagged reference and the native one name the same export.
    assert_eq!(mesh.tagged_body_setup(), Some(PackageIndex(2)));
    assert_eq!(mesh.native.body_setup, PackageIndex(2));
    assert!(is_body_setup(
        &pkg,
        mesh.native.body_setup.export_index().unwrap()
    ));

    // No tag at all: every switch is at its native default and there is no body.
    let plain = decode_static_mesh(&pkg, Some("Synth"), 4, &NoSchema).unwrap();
    assert_eq!(plain.stored_use_simple_box_collision(), None);
    assert_eq!(
        plain.simple_collision_flags(),
        SimpleCollisionFlags::default()
    );
    assert_eq!(
        SimpleCollisionFlags::default(),
        SimpleCollisionFlags {
            line: true,
            box_: true,
            rigid_body: true
        }
    );
    assert_eq!(plain.tagged_body_setup(), None);
    assert!(plain.native.body_setup.is_null());
}
