//! Synthetic tests for the `ULevel`, `UModel`, `UPolys` and `BrushComponent`
//! native-data parsers. Every byte is written here; no original game data is
//! involved. Includes hostile-input checks: truncation at every offset,
//! corrupted values at every offset, impossible counts and element sizes.

#![allow(clippy::unwrap_used)]

use asamu_ue3::Ue3Error;
use asamu_ue3::bsp::{self, poly_flags};
use asamu_ue3::level;
use asamu_ue3::object::ObjectError;
use asamu_ue3::reader::Reader;

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
    fn u8(&mut self, v: u8) -> &mut Self {
        self.0.push(v);
        self
    }
    fn u64(&mut self, v: u64) -> &mut Self {
        self.0.extend_from_slice(&v.to_le_bytes());
        self
    }
    fn f32(&mut self, v: f32) -> &mut Self {
        self.0.extend_from_slice(&v.to_le_bytes());
        self
    }
    fn vec3(&mut self, v: [f32; 3]) -> &mut Self {
        self.f32(v[0]).f32(v[1]).f32(v[2])
    }
    fn fstring(&mut self, s: &str) -> &mut Self {
        if s.is_empty() {
            return self.i32(0);
        }
        self.i32(i32::try_from(s.len() + 1).unwrap());
        self.0.extend_from_slice(s.as_bytes());
        self.u8(0)
    }
    fn lightmass(&mut self) -> &mut Self {
        self.u32(0)
            .u32(0)
            .f32(1.0)
            .u32(0)
            .f32(2.0)
            .f32(0.0)
            .f32(1.0)
            .f32(1.0)
            .f32(1.0)
    }
}

const QUAD: [[f32; 3]; 4] = [
    [0.0, 0.0, 0.0],
    [100.0, 0.0, 0.0],
    [100.0, 100.0, 0.0],
    [0.0, 100.0, 0.0],
];

/// A one-node BSP: a 100x100 quad on the plane z = 0, normal +Z.
fn model_tail(vert_size: usize, zones: i32) -> Vec<u8> {
    let mut w = W::default();
    // Bounds.
    w.vec3([50.0, 50.0, 0.0]).vec3([50.0, 50.0, 0.0]).f32(70.7);
    // Vectors: normal, texture U.
    w.i32(12).i32(2).vec3([0.0, 0.0, 1.0]).vec3([1.0, 0.0, 0.0]);
    // Points.
    w.i32(12).i32(4);
    for p in QUAD {
        w.vec3(p);
    }
    // Nodes: one 64-byte node.
    w.i32(64).i32(1);
    w.f32(0.0).f32(0.0).f32(1.0).f32(0.0); // plane X Y Z W
    w.i32(0).i32(0).i32(0); // vert pool, surf, vertex index
    w.u16(0).u16(0).i32(0); // component index / node index / element
    w.i32(-1).i32(-1).i32(-1).i32(-1); // back, front, coplanar, collision bound
    w.u8(1).u8(1).u8(4).u8(0); // zones, num vertices, flags
    w.i32(-1).i32(-1); // leaves
    // Surfs: owner + one 60-byte surface.
    w.i32(1).i32(1);
    w.i32(-3)
        .u32(0xE00)
        .i32(0)
        .i32(0)
        .i32(1)
        .i32(1)
        .i32(0)
        .i32(2);
    w.f32(0.0)
        .f32(0.0)
        .f32(1.0)
        .f32(0.0)
        .f32(32.0)
        .u32(1)
        .i32(0);
    // Verts.
    w.i32(i32::try_from(vert_size).unwrap()).i32(4);
    for k in 0..4 {
        w.i32(k).i32(0).f32(0.5).f32(0.5);
        if vert_size == bsp::VERT_SIZE {
            w.f32(0.25).f32(0.25);
        }
    }
    // NumSharedSides, NumZones, zones.
    w.i32(4).i32(zones);
    for _ in 0..zones.max(0) {
        w.i32(0).u64(u64::MAX).u64(1).f32(0.0);
    }
    // Polys, LeafHulls, Leaves, RootOutside, Linked, PortalNodes.
    w.i32(5);
    w.i32(4).i32(2).i32(7).i32(8);
    w.i32(4).i32(1).i32(0);
    w.u32(1).u32(0);
    w.i32(4).i32(0);
    // NumVertices + vertex buffer (one vertex).
    w.u32(4)
        .i32(36)
        .i32(1)
        .vec3([0.0; 3])
        .u32(0x7F7F_7FFF)
        .u32(0x7F7F_FF7F);
    w.f32(0.0).f32(1.0).f32(0.5).f32(0.5);
    // LightingGuid, LightmassSettings.
    w.u32(1).u32(2).u32(3).u32(4);
    w.i32(1).lightmass();
    w.0
}

#[test]
fn model_tail_parses_exactly() {
    let data = model_tail(bsp::VERT_SIZE, 1);
    let mut r = Reader::new(&data);
    let m = bsp::read_model(&mut r).unwrap();
    assert_eq!(r.remaining(), 0);
    assert_eq!(m.vectors.len(), 2);
    assert_eq!(m.points, QUAD.to_vec());
    assert_eq!(m.nodes.len(), 1);
    assert_eq!(m.nodes[0].num_vertices, 4);
    assert_eq!(m.nodes[0].plane, [0.0, 0.0, 1.0, 0.0]);
    assert_eq!(m.nodes[0].leaf, [-1, -1]);
    assert_eq!(m.surfs_owner.0, 1);
    assert_eq!(m.surfs[0].poly_flags, 0xE00);
    assert_eq!(m.surfs[0].actor.0, 2);
    assert_eq!(m.surfs[0].shadow_map_scale, 32.0);
    assert_eq!(m.vert_element_size, 24);
    assert_eq!(m.verts[3].point, 3);
    assert_eq!(m.verts[0].backface_shadow_uv, Some([0.25, 0.25]));
    assert_eq!(m.zones.len(), 1);
    assert_eq!(m.zones[0].connectivity, u64::MAX);
    assert_eq!(m.polys.0, 5);
    assert_eq!(m.leaf_hulls, vec![7, 8]);
    assert_eq!(m.leaves, vec![0]);
    assert_eq!((m.root_outside, m.linked), (1, 0));
    assert_eq!(m.num_vertices, 4);
    assert_eq!(m.vertex_buffer.len(), 1);
    assert_eq!(m.vertex_buffer[0].uv, [0.0, 1.0]);
    assert_eq!(m.lighting_guid.d, 4);
    assert_eq!(m.lightmass_settings.len(), 1);
    assert_eq!(m.lightmass_settings[0].emissive_light_falloff_exponent, 2.0);

    let c = m.check();
    assert_eq!(c.bad_references, 0, "{:?}", c.examples);
    assert_eq!(c.off_plane_points, 0);
    assert_eq!((c.winding_agrees, c.winding_opposes), (1, 0));
    assert_eq!(m.node_polygon(0).unwrap(), QUAD.to_vec());

    let mesh = m.triangulate(|_, s| bsp::is_visible_surface(s));
    assert_eq!(mesh.triangle_count(), 2);
    assert_eq!(mesh.tags, vec![0, 0]);
    assert_eq!(
        m.triangulate(|_, s| s.poly_flags & poly_flags::INVISIBLE != 0)
            .triangle_count(),
        0
    );
}

#[test]
fn model_short_verts_and_zone_limits() {
    let data = model_tail(bsp::VERT_SIZE_SHORT, 0);
    let m = bsp::read_model(&mut Reader::new(&data)).unwrap();
    assert_eq!(m.vert_element_size, 16);
    assert_eq!(m.verts[0].backface_shadow_uv, None);
    assert!(m.zones.is_empty());
    // More zones than the engine's fixed array: rejected before reading them.
    let data = model_tail(bsp::VERT_SIZE, 65);
    assert!(matches!(
        bsp::read_model(&mut Reader::new(&data)),
        Err(ObjectError::Malformed {
            what: "Model.NumZones",
            ..
        })
    ));
    let data = model_tail(bsp::VERT_SIZE, -1);
    assert!(bsp::read_model(&mut Reader::new(&data)).is_err());
}

#[test]
fn model_check_reports_bad_references_and_geometry() {
    let data = model_tail(bsp::VERT_SIZE, 1);
    let mut m = bsp::read_model(&mut Reader::new(&data)).unwrap();
    m.nodes[0].surf = 9;
    m.verts[2].point = 40;
    m.nodes[0].plane[3] = 5.0;
    let c = m.check();
    assert!(c.bad_references >= 2, "{c:?}");
    assert!(m.node_polygon(0).is_none(), "out-of-range point");
    assert!(m.triangulate(|_, _| true).indices.is_empty());
    m.verts[2].point = 2;
    let c = m.check();
    assert_eq!(c.off_plane_points, 4);
    m.nodes[0].vert_pool = 2; // 2 + 4 > 4 verts
    assert!(m.node_polygon(0).is_none());
    assert!(m.check().bad_references >= 2);
}

fn poly(w: &mut W, verts: &[[f32; 3]]) {
    w.vec3([0.0; 3])
        .vec3([0.0, 0.0, 1.0])
        .vec3([1.0, 0.0, 0.0])
        .vec3([0.0, 1.0, 0.0]);
    w.i32(i32::try_from(verts.len()).unwrap());
    for v in verts {
        w.vec3(*v);
    }
    w.u32(0x20)
        .i32(3)
        .i32(4)
        .i32(0)
        .i32(-1)
        .i32(-1)
        .i32(0)
        .f32(32.0)
        .u32(1);
    w.lightmass();
    w.i32(6).i32(0);
}

fn polys_tail() -> Vec<u8> {
    let mut w = W::default();
    w.i32(2).i32(4).i32(1);
    poly(&mut w, &QUAD);
    poly(&mut w, &QUAD[..3]);
    w.0
}

#[test]
fn polys_tail_parses_exactly() {
    let data = polys_tail();
    let mut r = Reader::new(&data);
    let p = bsp::read_polys(&mut r).unwrap();
    assert_eq!(r.remaining(), 0);
    assert_eq!(p.max, 4);
    assert_eq!(p.owner.0, 1);
    assert_eq!(p.polys.len(), 2);
    assert_eq!(p.polys[0].vertices, QUAD.to_vec());
    assert_eq!(p.polys[1].vertices.len(), 3);
    assert_eq!(p.polys[0].poly_flags, poly_flags::SEMISOLID);
    assert_eq!(p.polys[0].item_name.index, 4);
    assert_eq!(p.polys[0].shadow_map_scale, 32.0);
    assert_eq!(p.polys[0].ruleset_variation.index, 6);
    // Max below Num.
    let mut bad = data.clone();
    bad[4..8].copy_from_slice(&1i32.to_le_bytes());
    assert!(bsp::read_polys(&mut Reader::new(&bad)).is_err());
}

#[test]
fn brush_component_tail() {
    let mut w = W::default();
    w.i32(2).i32(1).i32(3).u8(1).u8(2).u8(3).i32(1).i32(0);
    let data = w.0;
    let mut r = Reader::new(&data);
    assert_eq!(bsp::read_brush_component_tail(&mut r).unwrap(), vec![3, 0]);
    assert_eq!(r.remaining(), 0);
    // Element size other than 1.
    let mut w = W::default();
    w.i32(1).i32(2).i32(1).u16(0);
    assert!(bsp::read_brush_component_tail(&mut Reader::new(&w.0)).is_err());
}

fn level_tail() -> Vec<u8> {
    level_tail_with(true)
}

fn level_tail_with(light_volume: bool) -> Vec<u8> {
    let mut w = W::default();
    // Actors owner + actors (one null).
    w.i32(9).i32(3).i32(2).i32(3).i32(0);
    // URL.
    w.fstring("unreal").fstring("").fstring("Map").fstring("");
    w.i32(1).fstring("game=X").i32(7777).i32(1);
    // Model, ModelComponents, GameSequences.
    w.i32(4).i32(1).i32(5).i32(1).i32(6);
    // TextureToInstancesMap: 1 texture with 2 instances.
    w.i32(1).i32(-1).i32(2);
    for _ in 0..2 {
        w.vec3([1.0; 3]).f32(2.0).f32(3.0);
    }
    // DynamicTextureInstances: 1 component with 1 instance.
    w.i32(1)
        .i32(7)
        .i32(1)
        .vec3([0.0; 3])
        .f32(1.0)
        .f32(1.0)
        .i32(-1)
        .u32(1)
        .f32(1.0);
    // Skipped block.
    w.i32(4).u32(0xDEAD_BEEF);
    // CachedPhysBSPData.
    w.i32(1).i32(3).u8(1).u8(2).u8(3);
    // CachedPhysSMDataMap + store.
    w.i32(1).i32(-2).vec3([1.0; 3]).i32(0);
    w.i32(1).i32(2).i32(1).i32(1).u8(9).i32(1).i32(0);
    // PerTri map + store.
    w.i32(1).i32(-2).vec3([1.0; 3]).i32(0);
    w.i32(1).i32(1).i32(2).u8(1).u8(2);
    // Versions, ForceStreamTextures.
    w.i32(11).i32(12).i32(1).i32(-1).u32(1);
    // CachedPhysConvexBSPData + version.
    w.i32(1).i32(1).i32(1).u8(5).i32(13);
    // Nav/cover/pylon heads and tails.
    for k in 0..6 {
        w.i32(k);
    }
    // CrossLevelCoverGuidRefs, CoverLinkRefs, CoverIndexPairs, CrossLevelActors.
    w.i32(1).u32(1).u32(2).u32(3).u32(4).i32(0);
    w.i32(1).i32(3);
    w.i32(2).i32(0).u8(1).i32(1).u8(2);
    w.i32(0);
    // Light volume: initialized, box, f32, 2 samples of 33 bytes.
    if light_volume {
        w.u32(1)
            .vec3([-1.0; 3])
            .vec3([1.0; 3])
            .u8(1)
            .f32(0.0)
            .i32(2);
        w.0.extend_from_slice(&[0u8; 66]);
    } else {
        w.u32(0);
    }
    // Visibility: origin, 4 sizes, 1 bucket { size, 1 cell, 1 chunk of 3 bytes }.
    w.f32(1.0).f32(2.0).i32(1).i32(2).i32(3).i32(1);
    w.i32(1).i32(8).i32(1).vec3([0.0; 3]).u16(0).u16(0);
    w.i32(1).u32(1).i32(16).i32(3).u8(1).u8(2).u8(3);
    // Distance field: max, box, size, 2 voxels.
    w.f32(100.0)
        .vec3([0.0; 3])
        .vec3([1.0; 3])
        .u8(1)
        .i32(1)
        .i32(1)
        .i32(2)
        .i32(2);
    w.u32(0).u32(0);
    w.0
}

#[test]
fn level_tail_parses_exactly() {
    let data = level_tail();
    let mut r = Reader::new(&data);
    let t = level::read_level_tail(&mut r).unwrap();
    assert_eq!(r.remaining(), 0);
    assert_eq!(t.actors_owner.0, 9);
    assert_eq!(
        t.actors.iter().map(|a| a.0).collect::<Vec<_>>(),
        vec![2, 3, 0]
    );
    assert_eq!(t.url.protocol, "unreal");
    assert_eq!(t.url.map, "Map");
    assert_eq!(t.url.options, vec!["game=X".to_owned()]);
    assert_eq!((t.url.port, t.url.valid), (7777, 1));
    assert_eq!(t.model.0, 4);
    assert_eq!(t.model_components.len(), 1);
    assert_eq!(t.game_sequences[0].0, 6);
    assert_eq!(
        t.texture_to_instances,
        level::MapCount {
            entries: 1,
            values: 2
        }
    );
    assert_eq!(
        t.dynamic_texture_instances,
        level::MapCount {
            entries: 1,
            values: 1
        }
    );
    assert_eq!(t.skipped_block_bytes, 4);
    assert_eq!(t.cached_phys_bsp_bytes, 3);
    assert_eq!(t.cached_phys_sm_map, 1);
    assert_eq!(
        t.cached_phys_sm_store,
        level::MapCount {
            entries: 1,
            values: 2
        }
    );
    assert_eq!(
        (t.cached_phys_per_tri_map, t.cached_phys_per_tri_store),
        (1, 1)
    );
    assert_eq!(
        (t.cached_phys_bsp_version, t.cached_phys_sm_version),
        (11, 12)
    );
    assert_eq!(t.force_stream_textures, 1);
    assert_eq!(
        (t.cached_phys_convex_bsp, t.cached_phys_convex_bsp_version),
        (1, 13)
    );
    assert_eq!(t.nav_cover_pylon.map(|p| p.0), [0, 1, 2, 3, 4, 5]);
    assert_eq!(t.cross_level_cover_guid_refs, 1);
    assert_eq!(t.cover_link_refs.len(), 1);
    assert_eq!(t.cover_index_pairs, 2);
    assert!(t.cross_level_actors.is_empty());
    let lv = t.light_volume.unwrap();
    assert_eq!((lv.samples, lv.bounds_valid), (2, 1));
    assert_eq!(t.visibility.sizes, [1, 2, 3, 1]);
    assert_eq!(
        (
            t.visibility.buckets,
            t.visibility.cells,
            t.visibility.chunks,
            t.visibility.chunk_bytes
        ),
        (1, 1, 1, 3)
    );
    assert_eq!(t.distance_field.size, [1, 1, 2]);
    assert_eq!(t.distance_field.voxels, 2);
}

#[test]
fn level_tail_without_light_volume() {
    let data = level_tail_with(false);
    let mut r = Reader::new(&data);
    let t = level::read_level_tail(&mut r).unwrap();
    assert_eq!(r.remaining(), 0);
    assert!(t.light_volume.is_none());
    assert_eq!(t.distance_field.voxels, 2);
}

/// Truncation at every offset fails cleanly; corrupting any 4-byte field
/// never panics.
fn hostile<T>(data: &[u8], parse: impl Fn(&mut Reader<'_>) -> Result<T, ObjectError>) {
    for cut in 0..data.len() {
        let mut r = Reader::new(&data[..cut]);
        assert!(parse(&mut r).is_err(), "truncated at {cut} parsed");
    }
    for at in 0..=data.len().saturating_sub(4) {
        for v in [i32::MAX, i32::MIN, -1, 0x4000_0000, 65] {
            let mut d = data.to_vec();
            d[at..at + 4].copy_from_slice(&v.to_le_bytes());
            let mut r = Reader::new(&d);
            let _ = parse(&mut r);
        }
        let mut d = data.to_vec();
        d[at] ^= 0xFF;
        let _ = parse(&mut Reader::new(&d));
    }
}

#[test]
fn hostile_model_tail() {
    hostile(&model_tail(bsp::VERT_SIZE, 1), |r| {
        let m = bsp::read_model(r)?;
        let _ = m.check();
        let _ = m.triangulate(|_, _| true);
        Ok(m)
    });
}

#[test]
fn hostile_polys_tail() {
    hostile(&polys_tail(), bsp::read_polys);
}

#[test]
fn hostile_level_tail() {
    hostile(&level_tail(), level::read_level_tail);
    hostile(&level_tail_with(false), level::read_level_tail);
}

#[test]
fn hostile_brush_component_tail() {
    let mut w = W::default();
    w.i32(2)
        .i32(1)
        .i32(3)
        .u8(1)
        .u8(2)
        .u8(3)
        .i32(1)
        .i32(2)
        .u8(4)
        .u8(5);
    hostile(&w.0, bsp::read_brush_component_tail);
}

#[test]
fn hostile_short_vert_model_tail() {
    hostile(&model_tail(bsp::VERT_SIZE_SHORT, 0), |r| {
        let m = bsp::read_model(r)?;
        let _ = m.check();
        Ok(m)
    });
}

#[test]
fn bulk_element_decoders_must_consume_exactly_the_element_size() {
    // Element size 12 but the decoder reads 8 bytes: rejected, not skipped.
    let mut w = W::default();
    w.i32(12).i32(2).vec3([1.0; 3]).vec3([2.0; 3]);
    let r = bsp::read_bulk(&mut Reader::new(&w.0), "t", &[12], |r, _| {
        Ok([r.read_f32()?, r.read_f32()?])
    });
    assert!(matches!(r, Err(ObjectError::Malformed { what: "t", .. })));
    // Zero elements with an allowed size; negative counts and sizes.
    let mut w = W::default();
    w.i32(12).i32(0);
    let (size, v) =
        bsp::read_bulk(&mut Reader::new(&w.0), "t", &[12], |r, _| bsp::read_vec3(r)).unwrap();
    assert_eq!((size, v.len()), (12, 0));
    for (size, count) in [(-12, 1), (12, -1), (0, 5), (i32::MIN, 0)] {
        let mut w = W::default();
        w.i32(size).i32(count);
        assert!(
            bsp::read_bulk(&mut Reader::new(&w.0), "t", &[12], |r, _| bsp::read_vec3(r)).is_err(),
            "{size} {count}"
        );
    }
    // Polys: Num at the limit of the remaining bytes is rejected before any
    // allocation; Max below Num and negative Num too.
    for (num, max) in [(i32::MAX, i32::MAX), (-1, 0), (1, 0), (1, -5)] {
        let mut w = W::default();
        w.i32(num).i32(max).i32(0);
        w.0.extend_from_slice(&[0u8; 200]);
        assert!(
            bsp::read_polys(&mut Reader::new(&w.0)).is_err(),
            "{num} {max}"
        );
    }
}

/// Independent composition of the transform from elementary matrices
/// (row-vector convention, `p' = p · M`): `T(−PrePivot) · S · Roll(X) ·
/// Pitch(Y) · Yaw(Z) · T(Location)`; the library computes the closed form
/// read from the native code.
fn elementary(rot: [i32; 3], scale: [f64; 3], pre: [f64; 3], loc: [f64; 3]) -> [[f64; 4]; 4] {
    let mul = |a: &[[f64; 4]; 4], b: &[[f64; 4]; 4]| {
        let mut o = [[0.0; 4]; 4];
        for i in 0..4 {
            for j in 0..4 {
                o[i][j] = (0..4).map(|k| a[i][k] * b[k][j]).sum();
            }
        }
        o
    };
    let id = || {
        let mut m = [[0.0; 4]; 4];
        for (k, row) in m.iter_mut().enumerate() {
            row[k] = 1.0;
        }
        m
    };
    let translate = |t: [f64; 3]| {
        let mut m = id();
        m[3] = [t[0], t[1], t[2], 1.0];
        m
    };
    let (sp, cp) = level::rotator_sin_cos(rot[0]);
    let (sy, cy) = level::rotator_sin_cos(rot[1]);
    let (sr, cr) = level::rotator_sin_cos(rot[2]);
    let mut s = id();
    for k in 0..3 {
        s[k][k] = scale[k];
    }
    let mut roll = id();
    roll[1] = [0.0, cr, -sr, 0.0];
    roll[2] = [0.0, sr, cr, 0.0];
    let mut pitch = id();
    pitch[0] = [cp, 0.0, sp, 0.0];
    pitch[2] = [-sp, 0.0, cp, 0.0];
    let mut yaw = id();
    yaw[0] = [cy, sy, 0.0, 0.0];
    yaw[1] = [-sy, cy, 0.0, 0.0];
    let m = mul(&translate([-pre[0], -pre[1], -pre[2]]), &s);
    let m = mul(&m, &roll);
    let m = mul(&m, &pitch);
    let m = mul(&m, &yaw);
    mul(&m, &translate(loc))
}

fn close(a: &level::Mat4, b: &[[f64; 4]; 4], what: &str) {
    for i in 0..4 {
        for j in 0..4 {
            let d = (f64::from(a[i][j]) - b[i][j]).abs();
            assert!(
                d <= 1e-4 * b[i][j].abs().max(1.0),
                "{what}: [{i}][{j}] {} vs {}",
                a[i][j],
                b[i][j]
            );
        }
    }
}

#[test]
fn actor_transform_equals_the_elementary_composition() {
    let rotations = [
        [0, 0, 0],
        [16384, 0, 0],
        [0, 16384, 0],
        [0, 0, 16384],
        [1234, -5678, 9012],
        [-32768, 70000, -1],
        [3, 7, 11],
        [i32::MAX, i32::MIN, 65535],
    ];
    let scales = [
        (1.0, [1.0, 1.0, 1.0]),
        (2.5, [1.0, 2.0, 0.5]),
        (1.0, [-1.0, 1.0, 1.0]),
        (-0.5, [3.0, -2.0, 1.0]),
    ];
    for rot in rotations {
        for (ds, s3) in scales {
            let pre = [3.0f32, -4.0, 5.5];
            let loc = [100.0f32, -2000.0, 37.25];
            let m = level::actor_local_to_world(loc, rot, ds, s3, pre);
            let s = [
                f64::from(s3[0] * ds),
                f64::from(s3[1] * ds),
                f64::from(s3[2] * ds),
            ];
            let want = elementary(rot, s, pre.map(f64::from), loc.map(f64::from));
            close(&m, &want, &format!("{rot:?} {ds} {s3:?}"));
        }
    }
    // Angles are quantized to 4 units, and whole turns wrap.
    let a = level::actor_local_to_world([0.0; 3], [5, 9, 13], 1.0, [1.0; 3], [0.0; 3]);
    let b = level::actor_local_to_world([0.0; 3], [4, 8, 12], 1.0, [1.0; 3], [0.0; 3]);
    let c = level::actor_local_to_world([0.0; 3], [65540, -65528, 12], 1.0, [1.0; 3], [0.0; 3]);
    assert_eq!(a, b);
    assert_eq!(b, c);
}

#[test]
fn component_transform_flags_against_independent_results() {
    let parent = level::actor_local_to_world(
        [10.0, 20.0, 30.0],
        [0, 16384, 0],
        2.0,
        [1.0, 1.0, 3.0],
        [0.0; 3],
    );
    let base = level::ComponentTransform {
        translation: [1.0, 0.0, 0.0],
        rotation: [0, 0, 0],
        scale: 1.0,
        scale3d: [1.0; 3],
        absolute_translation: false,
        absolute_rotation: false,
        absolute_scale: false,
    };
    let at = |c: &level::ComponentTransform, p: [f32; 3]| {
        level::transform_point(&level::component_local_to_world(c, &parent), p)
    };
    let near = |a: [f32; 3], b: [f32; 3]| (0..3).all(|k| (a[k] - b[k]).abs() < 1e-3);
    // Relative: local (1,0,0)+(1,0,0) → yaw 90° → (0, 2·2, 0) + location.
    assert!(near(at(&base, [1.0, 0.0, 0.0]), [10.0, 24.0, 30.0]));
    // Absolute translation drops the parent location only.
    let c = level::ComponentTransform {
        absolute_translation: true,
        ..base
    };
    assert!(near(at(&c, [1.0, 0.0, 0.0]), [0.0, 4.0, 0.0]));
    // Absolute scale: parent axes normalized (rotation kept, scale 1).
    let c = level::ComponentTransform {
        absolute_scale: true,
        ..base
    };
    assert!(near(at(&c, [1.0, 0.0, 0.0]), [10.0, 22.0, 30.0]));
    assert!(near(at(&c, [0.0, 0.0, 1.0]), [10.0, 21.0, 31.0]));
    // Absolute rotation: parent axis lengths kept on the diagonal.
    let c = level::ComponentTransform {
        absolute_rotation: true,
        ..base
    };
    assert!(near(at(&c, [1.0, 0.0, 0.0]), [14.0, 20.0, 30.0]));
    assert!(near(at(&c, [0.0, 0.0, 1.0]), [12.0, 20.0, 36.0]));
    // All three: the component's own transform in world space.
    let c = level::ComponentTransform {
        absolute_translation: true,
        absolute_rotation: true,
        absolute_scale: true,
        rotation: [0, 16384, 0],
        scale: 3.0,
        ..base
    };
    assert!(near(at(&c, [1.0, 0.0, 0.0]), [1.0, 3.0, 0.0]));
    // Degenerate parents (zero scale, NaN) never panic.
    let zero = level::actor_local_to_world([0.0; 3], [0; 3], 0.0, [1.0; 3], [0.0; 3]);
    let nan = level::actor_local_to_world([f32::NAN; 3], [0; 3], f32::NAN, [1.0; 3], [0.0; 3]);
    for p in [zero, nan] {
        let c = level::ComponentTransform {
            absolute_rotation: true,
            absolute_scale: true,
            ..base
        };
        let _ = level::component_local_to_world(&c, &p);
    }
}

#[test]
fn impossible_counts_are_rejected_before_allocating() {
    // Bulk array claiming 2^30 nodes in 8 bytes.
    let mut w = W::default();
    w.vec3([0.0; 3]).vec3([0.0; 3]).f32(0.0);
    w.i32(12).i32(0).i32(12).i32(0).i32(64).i32(1 << 30);
    let err = bsp::read_model(&mut Reader::new(&w.0)).unwrap_err();
    assert!(
        matches!(err, ObjectError::Ue3(Ue3Error::CountTooLarge { .. })),
        "{err}"
    );
    // Actor array claiming i32::MAX entries.
    let mut w = W::default();
    w.i32(1).i32(i32::MAX);
    assert!(level::read_level_tail(&mut Reader::new(&w.0)).is_err());
    // Negative skipped-block size.
    let mut data = level_tail();
    let pos = data
        .windows(8)
        .position(|x| x == [4, 0, 0, 0, 0xEF, 0xBE, 0xAD, 0xDE])
        .unwrap();
    data[pos..pos + 4].copy_from_slice(&(-4i32).to_le_bytes());
    assert!(level::read_level_tail(&mut Reader::new(&data)).is_err());
}
