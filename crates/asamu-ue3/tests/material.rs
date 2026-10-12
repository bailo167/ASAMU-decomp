//! Material decoding and approximation on synthetic data written byte by
//! byte here (no original game data): native-tail known answers and round
//! trips, and the approximation of hand-built material graphs and instance
//! chains packed into synthetic v868 packages.

#![allow(clippy::unwrap_used)]

mod common;

use std::sync::Arc;

use asamu_ue3::Package;
use asamu_ue3::material::{
    ApproxMaterial, ApproxStatus, ChannelSource, MaterialChain, MaterialClass, MaterialDecoder,
    MaterialNative, NativeKind, ParameterValues, TextureStats, color_to_linear,
    decode_material_native, encode_material_native, expression_kind, roughness_from_specular_power,
};
use asamu_ue3::model::{LoadedPackage, PackageSet};
use asamu_ue3::property::{ObjRef, Property, Value};
use asamu_ue3::types::Guid;
use common::{Export, Import, Synth, W};

// ---------------------------------------------------------------------------
// Native tail writer (independent of the library encoder)
// ---------------------------------------------------------------------------

struct Res {
    id: [u32; 4],
    textures: Vec<i32>,
    lookups: Vec<(i32, i32, f32, f32)>,
    deps: Vec<(i32, i32)>,
    errors: Vec<&'static str>,
    flags: [u32; 5],
    extras: [u32; 3],
}

impl Res {
    fn simple(id: u32) -> Res {
        Res {
            id: [id, 2, 3, 4],
            textures: Vec::new(),
            lookups: Vec::new(),
            deps: Vec::new(),
            errors: Vec::new(),
            flags: [0; 5],
            extras: [0; 3],
        }
    }

    fn write(&self, w: &mut W) {
        w.i32(self.errors.len() as i32);
        for e in &self.errors {
            w.fstring(e);
        }
        w.i32(self.deps.len() as i32);
        for (e, l) in &self.deps {
            w.i32(*e);
            w.i32(*l);
        }
        w.i32(self.deps.iter().map(|d| d.1).max().unwrap_or(0)); // MaxTextureDependencyLength
        for v in self.id {
            w.u32(v);
        }
        w.u32(2); // NumUserTexCoords
        w.i32(self.textures.len() as i32);
        for t in &self.textures {
            w.i32(*t);
        }
        for f in self.flags {
            w.u32(f);
        }
        w.u32(1); // UsingTransforms
        w.i32(self.lookups.len() as i32);
        for (a, b, u, v) in &self.lookups {
            w.i32(*a);
            w.i32(*b);
            w.bytes(&u.to_le_bytes());
            w.bytes(&v.to_le_bytes());
        }
        w.u32(0x0108_2052); // discarded legacy value
        for v in self.extras {
            w.u32(v);
        }
    }
}

/// Static parameter set: (switch name, value, override) and (mask name,
/// rgba, override), names as name-table indices.
fn static_set(
    w: &mut W,
    base: [u32; 4],
    switches: &[(i32, bool, bool)],
    masks: &[(i32, [bool; 4], bool)],
) {
    for v in base {
        w.u32(v);
    }
    w.i32(switches.len() as i32);
    for (n, v, o) in switches {
        w.i32(*n);
        w.i32(0);
        w.u32(u32::from(*v));
        w.u32(u32::from(*o));
        for g in [9u32, 9, 9, 9] {
            w.u32(g);
        }
    }
    w.i32(masks.len() as i32);
    for (n, m, o) in masks {
        w.i32(*n);
        w.i32(0);
        for c in m {
            w.u32(u32::from(*c));
        }
        w.u32(u32::from(*o));
        for g in [8u32, 8, 8, 8] {
            w.u32(g);
        }
    }
    w.i32(1); // one normal parameter
    w.i32(*switches.first().map(|s| &s.0).unwrap_or(&0));
    w.i32(0);
    w.0.push(1); // CompressionSettings
    w.u32(1);
    for g in [7u32, 7, 7, 7] {
        w.u32(g);
    }
    w.i32(1); // one terrain layer weight parameter
    w.i32(*switches.first().map(|s| &s.0).unwrap_or(&0));
    w.i32(0);
    w.i32(3); // WeightmapIndex
    w.u32(0);
    for g in [6u32, 6, 6, 6] {
        w.u32(g);
    }
}

fn material_tail(resources: &[Res]) -> Vec<u8> {
    let mut w = W::default();
    let mask = match resources.len() {
        0 => 0,
        1 => 1,
        _ => 3,
    };
    w.u32(mask);
    for r in resources {
        r.write(&mut w);
    }
    w.0
}

#[test]
fn material_native_known_answer_and_round_trip() {
    let res = Res {
        id: [0xAABB_CCDD, 1, 2, 3],
        textures: vec![-3, 5],
        lookups: vec![(1, 0, 2.0, 2.0), (0, 1, 1.0, 1.0)],
        deps: vec![(7, 2), (8, 1)],
        errors: vec!["oops"],
        flags: [1, 0, 1, 0, 1],
        extras: [1, 0, 1],
    };
    let mut payload = vec![0xEEu8; 5]; // stand-in for prelude + tags
    let start = payload.len();
    payload.extend(material_tail(&[res]));
    let n = decode_material_native(&payload, start, NativeKind::Material).unwrap();
    assert_eq!(n.quality_mask, Some(1));
    assert_eq!(n.resources.len(), 1);
    let q = &n.resources[0];
    assert_eq!(q.quality, 0);
    assert!(q.static_parameters.is_none());
    let r = &q.resource;
    assert_eq!(r.compile_errors, vec!["oops".to_owned()]);
    assert_eq!(r.texture_dependency_lengths.len(), 2);
    assert_eq!(r.texture_dependency_lengths[0].expression.0, 7);
    assert_eq!(r.texture_dependency_lengths[0].length, 2);
    assert_eq!(r.max_texture_dependency_length, 2);
    assert_eq!(
        r.id,
        Guid {
            a: 0xAABB_CCDD,
            b: 1,
            c: 2,
            d: 3
        }
    );
    assert_eq!(r.num_user_tex_coords, 2);
    assert_eq!(
        r.uniform_expression_textures
            .iter()
            .map(|t| t.0)
            .collect::<Vec<_>>(),
        vec![-3, 5]
    );
    assert!(r.uses_scene_color && !r.uses_scene_depth && r.uses_dynamic_parameter);
    assert!(!r.uses_lightmap_uvs && r.uses_vertex_position_offset);
    assert_eq!(r.using_transforms, 1);
    assert_eq!(r.texture_lookups.len(), 2);
    assert_eq!(r.texture_lookups[0].tex_coord_index, 1);
    assert_eq!(r.texture_lookups[0].u_scale, 2.0);
    assert_eq!(r.legacy_u32, 0x0108_2052);
    assert_eq!(r.resource_u32, [1, 0, 1]);
    let enc = encode_material_native(&n, NativeKind::Material).unwrap();
    assert_eq!(enc, payload[start..]);
}

#[test]
fn two_quality_levels_and_instance_static_parameters() {
    let tail = material_tail(&[Res::simple(1), Res::simple(2)]);
    let n = decode_material_native(&tail, 0, NativeKind::Material).unwrap();
    assert_eq!(n.quality_mask, Some(3));
    assert_eq!(
        n.resources.iter().map(|q| q.quality).collect::<Vec<_>>(),
        vec![0, 1]
    );
    assert_eq!(n.resources[1].resource.id.a, 2);
    assert_eq!(
        encode_material_native(&n, NativeKind::Material).unwrap(),
        tail
    );

    let mut w = W::default();
    w.u32(1);
    Res::simple(5).write(&mut w);
    static_set(
        &mut w,
        [1, 2, 3, 4],
        &[(11, true, true), (12, false, false)],
        &[(13, [true, false, true, false], true)],
    );
    let kind = NativeKind::Instance {
        static_permutation: true,
    };
    let n = decode_material_native(&w.0, 0, kind).unwrap();
    let s = n.resources[0].static_parameters.as_ref().unwrap();
    assert_eq!(
        s.base_material_id,
        Guid {
            a: 1,
            b: 2,
            c: 3,
            d: 4
        }
    );
    assert_eq!(s.static_switches.len(), 2);
    assert_eq!(s.static_switches[0].name.index, 11);
    assert!(s.static_switches[0].value && s.static_switches[0].overridden);
    assert!(!s.static_switches[1].overridden);
    assert_eq!(s.component_masks[0].mask, [true, false, true, false]);
    assert_eq!(s.normal_parameters[0].compression_settings, 1);
    assert_eq!(s.terrain_layer_weights[0].weightmap_index, 3);
    assert_eq!(encode_material_native(&n, kind).unwrap(), w.0);
}

#[test]
fn empty_tails_and_rejections() {
    let none = NativeKind::Instance {
        static_permutation: false,
    };
    assert_eq!(
        decode_material_native(&[1, 2, 3], 3, none).unwrap(),
        MaterialNative::default()
    );
    assert!(decode_material_native(&[1, 2, 3], 2, none).is_err());
    assert!(decode_material_native(&[0; 4], 4, NativeKind::None).is_ok());
    assert!(decode_material_native(&[0; 8], 4, NativeKind::None).is_err());
    assert_eq!(
        encode_material_native(&MaterialNative::default(), none).unwrap(),
        Vec::<u8>::new()
    );

    // Unknown quality bits.
    let mut tail = material_tail(&[Res::simple(1)]);
    tail[0] = 4;
    assert!(decode_material_native(&tail, 0, NativeKind::Material).is_err());
    // A boolean holding 2.
    let mut res = Res::simple(1);
    res.flags[2] = 2;
    assert!(decode_material_native(&material_tail(&[res]), 0, NativeKind::Material).is_err());
    // Trailing byte.
    let mut tail = material_tail(&[Res::simple(1)]);
    tail.push(0);
    assert!(decode_material_native(&tail, 0, NativeKind::Material).is_err());
    // Mask 0: no resources, valid.
    let n = decode_material_native(&[0, 0, 0, 0], 0, NativeKind::Material).unwrap();
    assert_eq!(n.quality_mask, Some(0));
    // Inconsistent values do not encode.
    let mut bad =
        decode_material_native(&material_tail(&[Res::simple(1)]), 0, NativeKind::Material).unwrap();
    bad.quality_mask = Some(3);
    assert!(encode_material_native(&bad, NativeKind::Material).is_none());
    bad.quality_mask = Some(1);
    assert!(
        encode_material_native(
            &bad,
            NativeKind::Instance {
                static_permutation: true
            }
        )
        .is_none(),
        "an instance resource needs its static parameter set"
    );
}

#[test]
fn helpers() {
    assert_eq!(
        expression_kind("Engine.MaterialExpressionTextureSample"),
        Some("TextureSample")
    );
    assert_eq!(expression_kind("Engine.Texture2D"), None);
    let c = color_to_linear([0, 128, 255, 51]); // B, G, R, A
    assert_eq!(c[0], 1.0);
    assert!((c[1] - (128.0f32 / 255.0).powf(2.2)).abs() < 1e-6);
    assert_eq!(c[2], 0.0);
    assert!((c[3] - 0.2).abs() < 1e-6);
    assert!((roughness_from_specular_power(15.0) - (2.0f32 / 17.0).sqrt().sqrt()).abs() < 1e-6);
    assert_eq!(roughness_from_specular_power(-5.0), 1.0);
    assert_eq!(roughness_from_specular_power(f32::NAN), 1.0);
    assert!(roughness_from_specular_power(1e9) < 0.02);
    assert_eq!(
        MaterialClass::classify("Engine.DecalMaterial", &[]),
        Some(MaterialClass::DecalMaterial)
    );
    assert_eq!(
        MaterialClass::classify(
            "My.Special",
            &[
                "Engine.MaterialInstanceConstant".into(),
                "Engine.MaterialInstance".into()
            ]
        ),
        Some(MaterialClass::OtherInstance)
    );
    assert_eq!(
        MaterialClass::classify("Engine.Texture2D", &["Engine.Texture".into()]),
        None
    );
}

// ---------------------------------------------------------------------------
// Synthetic packages with material graphs
// ---------------------------------------------------------------------------

/// Builds a v868 package whose exports carry hand-written payloads.
#[derive(Default)]
pub struct Builder {
    names: Vec<String>,
    imports: Vec<Import>,
    exports: Vec<Export>,
}

impl Builder {
    pub fn n(&mut self, s: &str) -> i32 {
        if let Some(i) = self.names.iter().position(|x| x == s) {
            return i as i32;
        }
        self.names.push(s.to_owned());
        (self.names.len() - 1) as i32
    }

    fn import(&mut self, class_package: &str, class: &str, outer: i32, name: &str) -> i32 {
        let imp = Import {
            class_package: self.n(class_package),
            class_name: self.n(class),
            outer,
            name: self.n(name),
            number: 0,
        };
        if let Some(i) = self.imports.iter().position(|x| {
            x.name == imp.name && x.outer == imp.outer && x.class_name == imp.class_name
        }) {
            return -(i as i32) - 1;
        }
        self.imports.push(imp);
        -(self.imports.len() as i32)
    }

    /// Import of class `Engine.<class>`.
    pub fn class(&mut self, class: &str) -> i32 {
        let engine = self.import("Core", "Package", 0, "Engine");
        self.import("Core", "Class", engine, class)
    }

    /// An object import `<package>.<name>` of class `Engine.<class>` (not
    /// present in this package).
    pub fn external(&mut self, package: &str, class: &str, name: &str) -> i32 {
        let p = self.import("Core", "Package", 0, package);
        self.import("Engine", class, p, name)
    }

    /// Add an export; returns its package index (export index + 1).
    pub fn export(&mut self, class: i32, outer: i32, name: &str, payload: Vec<u8>) -> i32 {
        let name = self.n(name);
        self.exports.push(Export {
            class,
            super_: 0,
            outer,
            name,
            number: 0,
            archetype: 0,
            object_flags: 0x0007_0004_0000_0000,
            payload,
            export_flags: 1,
            net_counts: Vec::new(),
            guid: [0; 4],
            package_flags: 0,
        });
        self.exports.len() as i32
    }

    /// Reserve an export slot to fill later (for forward references).
    pub fn reserve(&mut self, class: i32, outer: i32, name: &str) -> i32 {
        self.export(class, outer, name, Vec::new())
    }

    pub fn fill(&mut self, export: i32, payload: Vec<u8>) {
        self.exports[(export - 1) as usize].payload = payload;
    }

    pub fn package(&mut self) -> Package {
        self.n("None");
        // "None" must be index 0 for the tag terminator.
        assert_eq!(self.names[0], "None");
        let s = Synth {
            package_flags: 0x0000_0008,
            names: self
                .names
                .iter()
                .enumerate()
                .map(|(i, n)| (n.clone(), 0x0007_0010_0000_0000u64 + i as u64))
                .collect(),
            imports: self.imports.clone(),
            exports: self.exports.clone(),
            additional_packages: Vec::new(),
            texture_allocations: Vec::new(),
            guid: [1, 2, 3, 4],
            package_source: 0,
            engine_version: 12097,
            cooker_version: 136,
            depends: true,
            thumbnails: Vec::new(),
        };
        Package::from_bytes(s.build().0).unwrap()
    }

    pub fn new() -> Builder {
        let mut b = Builder::default();
        b.n("None");
        b
    }
}

/// Tagged-property writer.
pub struct Tags<'b> {
    pub b: &'b mut Builder,
    pub w: W,
}

impl<'b> Tags<'b> {
    pub fn new(b: &'b mut Builder) -> Tags<'b> {
        let mut w = W::default();
        w.i32(0); // NetIndex
        Tags { b, w }
    }

    /// A bare tagged stream (struct contents), without NetIndex.
    pub fn inner(b: &'b mut Builder) -> Tags<'b> {
        Tags { b, w: W::default() }
    }

    fn header(&mut self, name: &str, ty: &str, size: usize) {
        let (n, t) = (self.b.n(name), self.b.n(ty));
        self.w.i32(n);
        self.w.i32(0);
        self.w.i32(t);
        self.w.i32(0);
        self.w.i32(size as i32);
        self.w.i32(0);
    }

    pub fn float(mut self, name: &str, v: f32) -> Self {
        self.header(name, "FloatProperty", 4);
        self.w.bytes(&v.to_le_bytes());
        self
    }

    pub fn int(mut self, name: &str, v: i32) -> Self {
        self.header(name, "IntProperty", 4);
        self.w.i32(v);
        self
    }

    pub fn boolean(mut self, name: &str, v: bool) -> Self {
        self.header(name, "BoolProperty", 0);
        self.w.0.push(u8::from(v));
        self
    }

    pub fn object(mut self, name: &str, v: i32) -> Self {
        self.header(name, "ObjectProperty", 4);
        self.w.i32(v);
        self
    }

    pub fn name_value(mut self, name: &str, v: &str) -> Self {
        self.header(name, "NameProperty", 8);
        let i = self.b.n(v);
        self.w.i32(i);
        self.w.i32(0);
        self
    }

    pub fn enumeration(mut self, name: &str, enum_name: &str, v: &str) -> Self {
        self.header(name, "ByteProperty", 8);
        let (e, x) = (self.b.n(enum_name), self.b.n(v));
        self.w.i32(e);
        self.w.i32(0);
        self.w.i32(x);
        self.w.i32(0);
        self
    }

    pub fn structure(mut self, name: &str, struct_name: &str, body: Vec<u8>) -> Self {
        self.header(name, "StructProperty", body.len());
        let s = self.b.n(struct_name);
        self.w.i32(s);
        self.w.i32(0);
        self.w.bytes(&body);
        self
    }

    /// End the stream with `None`.
    pub fn end(mut self) -> Vec<u8> {
        self.w.i32(0);
        self.w.i32(0);
        self.w.0
    }
}

/// An `ExpressionInput` / `MaterialInput` struct body.
pub fn link(b: &mut Builder, expr: i32, output: i32, mask: Option<[bool; 4]>) -> Vec<u8> {
    let mut t = Tags::inner(b).object("Expression", expr);
    if output != 0 {
        t = t.int("OutputIndex", output);
    }
    if let Some(m) = mask {
        t = t.int("Mask", 1);
        for (c, on) in ["MaskR", "MaskG", "MaskB", "MaskA"].iter().zip(m) {
            if on {
                t = t.int(c, 1);
            }
        }
    }
    t.end()
}

/// A `ColorMaterialInput` with `UseConstant` and an optional expression.
pub fn color_constant(b: &mut Builder, bgra: [u8; 4], expr: i32) -> Vec<u8> {
    let mut t = Tags::inner(b).boolean("UseConstant", true);
    t = t.structure("Constant", "Color", bgra.to_vec());
    if expr != 0 {
        t = t.object("Expression", expr);
    }
    t.end()
}

fn linear_color(r: f32, g: f32, b: f32, a: f32) -> Vec<u8> {
    let mut w = W::default();
    for v in [r, g, b, a] {
        w.bytes(&v.to_le_bytes());
    }
    w.0
}

/// Register `pkg` in a fresh set and return both.
pub fn load(pkg: Package) -> (PackageSet, Arc<LoadedPackage>) {
    let set = PackageSet::new::<&str>(&[]);
    let lp = set.insert_package("SynthMats", pkg);
    (set, lp)
}

const RGBA: Option<[bool; 4]> = None;
const RGB: Option<[bool; 4]> = Some([true, true, true, false]);
const ALPHA: Option<[bool; 4]> = Some([false, false, false, true]);

fn texture(b: &mut Builder, outer: i32, name: &str) -> i32 {
    let class = b.class("Texture2D");
    let payload = Tags::new(b).end();
    b.export(class, outer, name, payload)
}

/// A complete lit, masked material with a panning, tiled base texture.
#[test]
fn textured_masked_material() {
    let mut b = Builder::new();
    let core = b.import("Core", "Package", 0, "Core");
    let package_class = b.import("Core", "Class", core, "Package");
    let root_payload = Tags::new(&mut b).end();
    let root = b.export(package_class, 0, "Mats", root_payload);
    let t_diff = texture(&mut b, root, "T_Diffuse");
    let t_norm = texture(&mut b, root, "T_Normal");
    let material_class = b.class("Material");
    let m = b.reserve(material_class, root, "M_Test");

    let c_tc = b.class("MaterialExpressionTextureCoordinate");
    let tc_payload = Tags::new(&mut b)
        .int("CoordinateIndex", 1)
        .float("UTiling", 4.0)
        .float("VTiling", 2.0)
        .end();
    let tc = b.export(c_tc, m, "MaterialExpressionTextureCoordinate_0", tc_payload);

    let c_pan = b.class("MaterialExpressionPanner");
    let coord = link(&mut b, tc, 0, RGBA);
    let pan_payload = Tags::new(&mut b)
        .structure("Coordinate", "ExpressionInput", coord)
        .float("SpeedX", 0.1)
        .float("SpeedY", 0.2)
        .end();
    let pan = b.export(c_pan, m, "MaterialExpressionPanner_0", pan_payload);

    let c_ts = b.class("MaterialExpressionTextureSample");
    let coords = link(&mut b, pan, 0, RGBA);
    let ts_payload = Tags::new(&mut b)
        .object("Texture", t_diff)
        .structure("Coordinates", "ExpressionInput", coords)
        .end();
    let ts = b.export(c_ts, m, "MaterialExpressionTextureSample_0", ts_payload);
    let tn_payload = Tags::new(&mut b).object("Texture", t_norm).end();
    let tn = b.export(c_ts, m, "MaterialExpressionTextureSample_1", tn_payload);

    let c_c3 = b.class("MaterialExpressionConstant3Vector");
    let c3_payload = Tags::new(&mut b)
        .float("R", 0.5)
        .float("G", 0.25)
        .float("B", 1.0)
        .end();
    let c3 = b.export(c_c3, m, "MaterialExpressionConstant3Vector_0", c3_payload);

    let c_mul = b.class("MaterialExpressionMultiply");
    let a = link(&mut b, ts, 0, RGB);
    let bb = link(&mut b, c3, 0, RGBA);
    let mul_payload = Tags::new(&mut b)
        .structure("A", "ExpressionInput", a)
        .structure("B", "ExpressionInput", bb)
        .end();
    let mul = b.export(c_mul, m, "MaterialExpressionMultiply_0", mul_payload);

    let c_k = b.class("MaterialExpressionConstant");
    let k_payload = Tags::new(&mut b).float("R", 32.0).end();
    let k = b.export(c_k, m, "MaterialExpressionConstant_0", k_payload);

    let diffuse = link(&mut b, mul, 0, RGBA);
    let normal = link(&mut b, tn, 0, RGB);
    let emissive = color_constant(&mut b, [0, 128, 255, 255], 0);
    let spec_power = link(&mut b, k, 0, RGBA);
    let opacity_mask = link(&mut b, ts, 4, ALPHA);
    let m_payload = Tags::new(&mut b)
        .structure("DiffuseColor", "ColorMaterialInput", diffuse)
        .structure("SpecularPower", "ScalarMaterialInput", spec_power)
        .structure("Normal", "VectorMaterialInput", normal)
        .structure("EmissiveColor", "ColorMaterialInput", emissive)
        .structure("OpacityMask", "ScalarMaterialInput", opacity_mask)
        .float("OpacityMaskClipValue", 0.5)
        .enumeration("BlendMode", "EBlendMode", "BLEND_Masked")
        .boolean("TwoSided", true)
        .end();
    let mut payload = m_payload;
    payload.extend(material_tail(&[Res::simple(77)]));
    b.fill(m, payload);

    let (set, lp) = load(b.package());
    let dec = MaterialDecoder::new(&set);
    let index = (m - 1) as usize;
    let obj = dec.decode(&lp, index).unwrap();
    assert_eq!(obj.class, MaterialClass::Material);
    assert_eq!(obj.path, "Mats.M_Test");
    assert_eq!(obj.native.resources.len(), 1);
    let a = dec.approximate(&lp, index).unwrap();
    assert_eq!(a.status, ApproxStatus::Approximated);
    assert!(a.lossless, "{:?}", a.notes);
    assert_eq!(a.chain, vec!["Mats.M_Test".to_owned()]);
    assert_eq!(
        a.resource_id.as_deref(),
        Some("0000004D000000020000000300000004")
    );
    assert_eq!(a.alpha_mode, "mask");
    assert_eq!(a.alpha_cutoff, Some(0.5));
    assert!(a.two_sided && !a.unlit && !a.decal);
    let tex = a.base_color.texture.as_ref().unwrap();
    assert_eq!(tex.texture.as_deref(), Some("Mats.T_Diffuse"));
    assert_eq!(tex.channels, "rgb");
    assert_eq!(tex.uv.channel, 1);
    assert_eq!(tex.uv.scale, [4.0, 2.0]);
    assert_eq!(tex.uv.panning, [0.1, 0.2]);
    assert_eq!(a.base_color.value, [0.5, 0.25, 1.0, 1.0]);
    assert_eq!(a.base_color.source, ChannelSource::Expression);
    let n = a.normal.as_ref().unwrap();
    assert_eq!(
        n.texture.as_ref().unwrap().texture.as_deref(),
        Some("Mats.T_Normal")
    );
    assert_eq!(a.emissive.source, ChannelSource::Constant);
    assert_eq!(a.emissive.value, color_to_linear([0, 128, 255, 255]));
    assert_eq!(a.specular.source, ChannelSource::Default);
    assert_eq!(a.specular.value, [0.0, 0.0, 0.0, 1.0]);
    assert_eq!(a.specular_power.value[0], 32.0);
    assert_eq!(a.roughness, roughness_from_specular_power(32.0));
    let op = a.opacity.as_ref().unwrap();
    assert_eq!(op.texture.as_ref().unwrap().channels, "a");
    assert_eq!(op.texture.as_ref().unwrap().uv.scale, [4.0, 2.0]);
    assert_eq!(
        a.textures,
        vec!["Mats.T_Diffuse".to_owned(), "Mats.T_Normal".to_owned()]
    );
    assert_eq!(a.expressions.get("TextureSample"), Some(&3));
}

/// Builds a material whose diffuse input is the expression `diffuse` (built
/// by the closure inside the material), plus extra base tags.
fn one_input_material(
    b: &mut Builder,
    build: impl FnOnce(&mut Builder, i32) -> (i32, Option<[bool; 4]>),
    lit: bool,
) -> (i32, i32) {
    let core = b.import("Core", "Package", 0, "Core");
    let package_class = b.import("Core", "Class", core, "Package");
    let root_payload = Tags::new(b).end();
    let root = b.export(package_class, 0, "Mats", root_payload);
    let material_class = b.class("Material");
    let m = b.reserve(material_class, root, "M_One");
    let (expr, mask) = build(b, m);
    let diffuse = link(b, expr, 0, mask);
    let mut t = Tags::new(b).structure("DiffuseColor", "ColorMaterialInput", diffuse);
    if !lit {
        t = t.enumeration("LightingModel", "EMaterialLightingModel", "MLM_Unlit");
    }
    let mut payload = t.end();
    payload.extend(material_tail(&[Res::simple(1)]));
    b.fill(m, payload);
    (root, m)
}

fn constant(b: &mut Builder, outer: i32, name: &str, v: f32) -> i32 {
    let c = b.class("MaterialExpressionConstant");
    let p = Tags::new(b).float("R", v).end();
    b.export(c, outer, name, p)
}

fn binary(
    b: &mut Builder,
    outer: i32,
    class: &str,
    name: &str,
    inputs: &[(&str, i32, Option<[bool; 4]>)],
) -> i32 {
    let c = b.class(class);
    let mut bodies = Vec::new();
    for (n, e, m) in inputs {
        bodies.push((n.to_string(), link(b, *e, 0, *m)));
    }
    let mut t = Tags::new(b);
    for (n, body) in bodies {
        t = t.structure(&n, "ExpressionInput", body);
    }
    let p = t.end();
    b.export(c, outer, name, p)
}

fn approx_of(b: &mut Builder, m: i32) -> asamu_ue3::material::ApproxMaterial {
    let (set, lp) = load(b.package());
    let dec = MaterialDecoder::new(&set);
    dec.approximate(&lp, (m - 1) as usize).unwrap()
}

#[test]
fn constant_folding_lerp_one_minus_and_masks() {
    // Lerp(0.2, 1.0, 0.25) = 0.4
    let mut b = Builder::new();
    let (_, m) = one_input_material(
        &mut b,
        |b, m| {
            let x = constant(b, m, "C0", 0.2);
            let y = constant(b, m, "C1", 1.0);
            let w = constant(b, m, "C2", 0.25);
            let l = binary(
                b,
                m,
                "MaterialExpressionLinearInterpolate",
                "L",
                &[("A", x, None), ("B", y, None), ("Alpha", w, None)],
            );
            (l, None)
        },
        true,
    );
    let a = approx_of(&mut b, m);
    assert!((a.base_color.value[0] - 0.4).abs() < 1e-6);
    assert!(a.lossless);

    // OneMinus(texture.G) -> value -1, bias 1, channel g.
    let mut b = Builder::new();
    let (_, m) = one_input_material(
        &mut b,
        |b, m| {
            let t = texture(b, m, "T");
            let c = b.class("MaterialExpressionTextureSample");
            let p = Tags::new(b).object("Texture", t).end();
            let ts = b.export(c, m, "TS", p);
            let om = binary(
                b,
                m,
                "MaterialExpressionOneMinus",
                "OM",
                &[("Input", ts, Some([false, true, false, false]))],
            );
            (om, None)
        },
        true,
    );
    let a = approx_of(&mut b, m);
    let tex = a.base_color.texture.as_ref().unwrap();
    assert_eq!(tex.channels, "g");
    assert_eq!(a.base_color.value, [-1.0; 4]);
    assert_eq!(a.base_color.bias, [1.0; 4]);

    // ComponentMask(B, A) of a texture -> channels "ba".
    let mut b = Builder::new();
    let (_, m) = one_input_material(
        &mut b,
        |b, m| {
            let t = texture(b, m, "T");
            let c = b.class("MaterialExpressionTextureSample");
            let p = Tags::new(b).object("Texture", t).end();
            let ts = b.export(c, m, "TS", p);
            let cm_class = b.class("MaterialExpressionComponentMask");
            let input = link(b, ts, 0, None);
            let p = Tags::new(b)
                .structure("Input", "ExpressionInput", input)
                .boolean("B", true)
                .boolean("A", true)
                .end();
            (b.export(cm_class, m, "CM", p), None)
        },
        true,
    );
    let a = approx_of(&mut b, m);
    assert_eq!(a.base_color.texture.as_ref().unwrap().channels, "ba");
}

#[test]
fn vector_parameter_default_and_texture_coordinate_math() {
    // Diffuse = VectorParameter default (0.2, 0.4, 0.6, 1).
    let mut b = Builder::new();
    let (_, m) = one_input_material(
        &mut b,
        |b, m| {
            let c = b.class("MaterialExpressionVectorParameter");
            let p = Tags::new(b)
                .name_value("ParameterName", "Tint")
                .structure(
                    "DefaultValue",
                    "LinearColor",
                    linear_color(0.2, 0.4, 0.6, 1.0),
                )
                .end();
            (b.export(c, m, "VP", p), None)
        },
        true,
    );
    let a = approx_of(&mut b, m);
    assert_eq!(a.base_color.value, [0.2, 0.4, 0.6, 1.0]);
    assert!(a.lossless);

    // TextureSample(coords = 3 - TexCoord * 2): scale -2, offset 3.
    let mut b = Builder::new();
    let (_, m) = one_input_material(
        &mut b,
        |b, m| {
            let t = texture(b, m, "T");
            let tc_class = b.class("MaterialExpressionTextureCoordinate");
            let p = Tags::new(b).end();
            let tc = b.export(tc_class, m, "TC", p);
            let two = constant(b, m, "Two", 2.0);
            let three = constant(b, m, "Three", 3.0);
            let mul = binary(
                b,
                m,
                "MaterialExpressionMultiply",
                "Mul",
                &[("A", tc, None), ("B", two, None)],
            );
            let sub = binary(
                b,
                m,
                "MaterialExpressionSubtract",
                "Sub",
                &[("A", three, None), ("B", mul, None)],
            );
            let c = b.class("MaterialExpressionTextureSample");
            let coords = link(b, sub, 0, None);
            let p = Tags::new(b)
                .object("Texture", t)
                .structure("Coordinates", "ExpressionInput", coords)
                .end();
            (b.export(c, m, "TS", p), RGB)
        },
        true,
    );
    let a = approx_of(&mut b, m);
    let uv = a.base_color.texture.as_ref().unwrap().uv;
    assert_eq!(uv.scale, [-2.0, -2.0]);
    assert_eq!(uv.offset, [3.0, 3.0]);
    assert!(a.lossless, "{:?}", a.notes);
}

#[test]
fn use_constant_wins_over_a_connected_expression() {
    let mut b = Builder::new();
    let core = b.import("Core", "Package", 0, "Core");
    let package_class = b.import("Core", "Class", core, "Package");
    let root_payload = Tags::new(&mut b).end();
    let root = b.export(package_class, 0, "Mats", root_payload);
    let material_class = b.class("Material");
    let m = b.reserve(material_class, root, "M");
    let k = constant(&mut b, m, "K", 0.9);
    let body = color_constant(&mut b, [255, 0, 0, 255], k); // blue constant + expression
    let mut payload = Tags::new(&mut b)
        .structure("DiffuseColor", "ColorMaterialInput", body)
        .end();
    payload.extend(material_tail(&[Res::simple(1)]));
    b.fill(m, payload);
    let a = approx_of(&mut b, m);
    assert_eq!(a.base_color.source, ChannelSource::Constant);
    assert_eq!(a.base_color.value, [0.0, 0.0, 1.0, 1.0]);
}

#[test]
fn unsupported_expressions_are_reported() {
    // Diffuse = Fresnel: unresolved -> fallback (lit material).
    let mut b = Builder::new();
    let (_, m) = one_input_material(
        &mut b,
        |b, m| {
            let c = b.class("MaterialExpressionFresnel");
            let p = Tags::new(b).float("Exponent", 3.0).end();
            (b.export(c, m, "F", p), None)
        },
        true,
    );
    let a = approx_of(&mut b, m);
    assert_eq!(a.status, ApproxStatus::Fallback);
    assert!(!a.base_color.resolved);
    assert_eq!(a.unsupported.get("Fresnel"), Some(&1));

    // Unlit material: the main channel is emissive (default black), so an
    // unresolved diffuse does not make it a fallback.
    let mut b = Builder::new();
    let (_, m) = one_input_material(
        &mut b,
        |b, m| {
            let c = b.class("MaterialExpressionFresnel");
            let p = Tags::new(b).end();
            (b.export(c, m, "F", p), None)
        },
        false,
    );
    let a = approx_of(&mut b, m);
    assert!(a.unlit);
    assert_eq!(a.status, ApproxStatus::Approximated);

    // Texture * Fresnel keeps the texture with a note.
    let mut b = Builder::new();
    let (_, m) = one_input_material(
        &mut b,
        |b, m| {
            let t = texture(b, m, "T");
            let c = b.class("MaterialExpressionTextureSample");
            let p = Tags::new(b).object("Texture", t).end();
            let ts = b.export(c, m, "TS", p);
            let fc = b.class("MaterialExpressionFresnel");
            let p = Tags::new(b).end();
            let f = b.export(fc, m, "F", p);
            (
                binary(
                    b,
                    m,
                    "MaterialExpressionMultiply",
                    "Mul",
                    &[("A", ts, RGB), ("B", f, None)],
                ),
                None,
            )
        },
        true,
    );
    let a = approx_of(&mut b, m);
    assert_eq!(a.status, ApproxStatus::Approximated);
    assert!(!a.lossless);
    assert!(a.base_color.texture.is_some());
    assert!(
        a.notes.iter().any(|n| n.contains("Fresnel")),
        "{:?}",
        a.notes
    );
}

/// Base material with a static switch; instance 1 overrides it through its
/// native static parameter set; instance 2 inherits from instance 1.
#[test]
fn instance_chain_and_static_switch_override() {
    let mut b = Builder::new();
    let core = b.import("Core", "Package", 0, "Core");
    let package_class = b.import("Core", "Class", core, "Package");
    let root_payload = Tags::new(&mut b).end();
    let root = b.export(package_class, 0, "Mats", root_payload);
    let material_class = b.class("Material");
    let mic_class = b.class("MaterialInstanceConstant");
    let m = b.reserve(material_class, root, "M_Base");
    let red = {
        let c = b.class("MaterialExpressionConstant3Vector");
        let p = Tags::new(&mut b).float("R", 1.0).end();
        b.export(c, m, "Red", p)
    };
    let blue = {
        let c = b.class("MaterialExpressionConstant3Vector");
        let p = Tags::new(&mut b).float("B", 1.0).end();
        b.export(c, m, "Blue", p)
    };
    let sw_class = b.class("MaterialExpressionStaticSwitchParameter");
    let la = link(&mut b, red, 0, None);
    let lb = link(&mut b, blue, 0, None);
    let sw_payload = Tags::new(&mut b)
        .structure("A", "ExpressionInput", la)
        .structure("B", "ExpressionInput", lb)
        .name_value("ParameterName", "UseRed")
        .end();
    let sw = b.export(sw_class, m, "Switch", sw_payload);
    let diffuse = link(&mut b, sw, 0, None);
    let mut payload = Tags::new(&mut b)
        .structure("DiffuseColor", "ColorMaterialInput", diffuse)
        .end();
    payload.extend(material_tail(&[Res::simple(10)]));
    b.fill(m, payload);

    let use_red = b.n("UseRed");
    let mut mic1 = Tags::new(&mut b)
        .object("Parent", m)
        .boolean("bHasStaticPermutationResource", true)
        .end();
    let mut w = W::default();
    w.u32(1);
    Res::simple(20).write(&mut w);
    static_set(&mut w, [10, 2, 3, 4], &[(use_red, true, true)], &[]);
    mic1.extend(w.0);
    let i1 = b.export(mic_class, root, "MI_Red", mic1);
    let mic2 = Tags::new(&mut b).object("Parent", i1).end();
    let i2 = b.export(mic_class, root, "MI_Child", mic2);

    let (set, lp) = load(b.package());
    let dec = MaterialDecoder::new(&set);
    let base = dec.approximate(&lp, (m - 1) as usize).unwrap();
    assert_eq!(
        base.base_color.value,
        [0.0, 0.0, 1.0, 1.0],
        "default: switch off -> B"
    );
    let child = dec.approximate(&lp, (i2 - 1) as usize).unwrap();
    assert_eq!(
        child.chain,
        vec![
            "Mats.MI_Child".to_owned(),
            "Mats.MI_Red".to_owned(),
            "Mats.M_Base".to_owned()
        ]
    );
    assert_eq!(child.base_material.as_deref(), Some("Mats.M_Base"));
    assert_eq!(child.class, MaterialClass::MaterialInstanceConstant);
    assert_eq!(child.base_color.value, [1.0, 0.0, 0.0, 1.0]);
    assert_eq!(child.parameters.switches.get("usered"), Some(&true));
    assert_eq!(
        child.resource_id.as_deref(),
        Some("00000014000000020000000300000004")
    );
    let obj = dec.decode(&lp, (i1 - 1) as usize).unwrap();
    assert_eq!(obj.static_parameters.switches.get("usered"), Some(&true));
}

#[test]
fn broken_and_cyclic_chains_fall_back() {
    let mut b = Builder::new();
    let core = b.import("Core", "Package", 0, "Core");
    let package_class = b.import("Core", "Class", core, "Package");
    let root_payload = Tags::new(&mut b).end();
    let root = b.export(package_class, 0, "Mats", root_payload);
    let mic_class = b.class("MaterialInstanceConstant");
    let missing = b.external("Elsewhere", "Material", "M_Gone");
    let p = Tags::new(&mut b).object("Parent", missing).end();
    let orphan = b.export(mic_class, root, "MI_Orphan", p);
    let selfish = b.reserve(mic_class, root, "MI_Loop");
    let p = Tags::new(&mut b).object("Parent", selfish).end();
    b.fill(selfish, p);
    let no_parent_payload = Tags::new(&mut b).end();
    let no_parent = b.export(mic_class, root, "MI_NoParent", no_parent_payload);

    let (set, lp) = load(b.package());
    let dec = MaterialDecoder::new(&set);
    for e in [orphan, selfish, no_parent] {
        let a = dec.approximate(&lp, (e - 1) as usize).unwrap();
        assert_eq!(a.status, ApproxStatus::Fallback);
        assert!(a.base_material.is_none());
        assert!(
            a.notes.iter().any(|n| n.starts_with("fallback")),
            "{:?}",
            a.notes
        );
        assert_eq!(a.base_color.value, [0.5, 0.5, 0.5, 1.0]);
    }
    let chain = dec.resolve_chain(&lp, (selfish - 1) as usize);
    assert_eq!(chain.links.len(), 1);
    assert!(chain.error.as_deref().unwrap_or("").contains("cycle"));
}

#[test]
fn parameter_values_nearest_instance_wins() {
    let mut b = Builder::new();
    let mic_class = b.class("MaterialInstanceConstant");
    let mitv_class = b.class("MaterialInstanceTimeVarying");
    let _a_payload = Tags::new(&mut b).end();
    let _a = b.export(mic_class, 0, "A", _a_payload);
    let _b2_payload = Tags::new(&mut b).end();
    let _b2 = b.export(mitv_class, 0, "B", _b2_payload);
    let (set, lp) = load(b.package());
    let dec = MaterialDecoder::new(&set);
    let mut leaf = dec.decode(&lp, 0).unwrap();
    let mut parent = dec.decode(&lp, 1).unwrap();
    let prop = |name: &str, value: Value| Property {
        name: name.to_owned(),
        type_name: String::new(),
        array_index: 0,
        size: 0,
        struct_name: None,
        enum_name: None,
        value,
        offset: 0,
    };
    let s = |fields: Vec<Property>| Value::Struct {
        name: String::new(),
        binary: false,
        fields,
    };
    let scalar = |n: &str, v: f32| {
        s(vec![
            prop("ParameterName", Value::Name(n.into())),
            prop("ParameterValue", Value::Float(v)),
        ])
    };
    let tex = |n: &str, t: &str| {
        s(vec![
            prop("ParameterName", Value::Name(n.into())),
            prop(
                "ParameterValue",
                Value::Object(ObjRef {
                    index: 3,
                    path: t.into(),
                }),
            ),
        ])
    };
    leaf.properties.push(prop(
        "ScalarParameterValues",
        Value::Array(vec![scalar("Tiling", 2.0)]),
    ));
    leaf.properties.push(prop(
        "TextureParameterValues",
        Value::Array(vec![tex("Diffuse", "P.T_Leaf")]),
    ));
    // The time-varying parent: curve's first key wins over ParameterValue.
    let curve = s(vec![prop(
        "Points",
        Value::Array(vec![s(vec![
            prop("InVal", Value::Float(0.0)),
            prop("OutVal", Value::Float(7.0)),
        ])]),
    )]);
    let tv = s(vec![
        prop("ParameterName", Value::Name("Glow".into())),
        prop("ParameterValue", Value::Float(1.0)),
        prop("ParameterValueCurve", curve),
    ]);
    parent.properties.push(prop(
        "ScalarParameterValues",
        Value::Array(vec![scalar("Tiling", 9.0), tv]),
    ));
    parent.properties.push(prop(
        "TextureParameterValues",
        Value::Array(vec![tex("Diffuse", "P.T_Parent")]),
    ));
    let chain = MaterialChain {
        links: vec![(lp.clone(), leaf), (lp.clone(), parent)],
        error: None,
    };
    let p = ParameterValues::from_chain(&chain);
    assert_eq!(p.scalars.get("tiling"), Some(&2.0));
    assert_eq!(p.scalars.get("glow"), Some(&7.0));
    assert_eq!(
        p.textures.get("diffuse"),
        Some(&Some("P.T_Leaf".to_owned()))
    );
    assert_eq!(p.time_varying, 1);
}

// ---------------------------------------------------------------------------
// Added by the verification pass
// ---------------------------------------------------------------------------

fn constant3(b: &mut Builder, outer: i32, name: &str, v: [f32; 3]) -> i32 {
    let c = b.class("MaterialExpressionConstant3Vector");
    let p = Tags::new(b)
        .float("R", v[0])
        .float("G", v[1])
        .float("B", v[2])
        .end();
    b.export(c, outer, name, p)
}

fn sample(b: &mut Builder, outer: i32, name: &str) -> i32 {
    let t = texture(b, outer, &format!("{name}_Tex"));
    let c = b.class("MaterialExpressionTextureSample");
    let p = Tags::new(b).object("Texture", t).end();
    b.export(c, outer, name, p)
}

/// One texture channel times a colour is a colour (UE3 broadcasts
/// `float1 * float3`): the channel keeps the single texture channel and the
/// colour as per-component multiplier. Before the verification pass the
/// colour collapsed to its red component.
#[test]
fn single_channel_texture_tinted_by_a_colour_keeps_the_colour() {
    // Diffuse = T.a * (0.5, 0.25, 1.0) + 0.125
    let mut b = Builder::new();
    let (_, m) = one_input_material(
        &mut b,
        |b, m| {
            let ts = sample(b, m, "TS");
            let tint = constant3(b, m, "Tint", [0.5, 0.25, 1.0]);
            let mul = binary(
                b,
                m,
                "MaterialExpressionMultiply",
                "Mul",
                &[("A", ts, ALPHA), ("B", tint, None)],
            );
            let k = constant(b, m, "K", 0.125);
            let add = binary(
                b,
                m,
                "MaterialExpressionAdd",
                "Add",
                &[("A", mul, None), ("B", k, None)],
            );
            (add, None)
        },
        true,
    );
    let a = approx_of(&mut b, m);
    assert!(a.lossless, "{:?}", a.notes);
    let tex = a.base_color.texture.as_ref().unwrap();
    assert_eq!(tex.channels, "a");
    assert_eq!(a.base_color.value, [0.5, 0.25, 1.0, 1.0]);
    assert_eq!(a.base_color.bias, [0.125, 0.125, 0.125, 0.0]);

    // Constant first, then a mask that picks the green multiplier:
    // ComponentMask(G)((0.5, 0.25, 1.0) * T.r) = 0.25 * T.r.
    let mut b = Builder::new();
    let (_, m) = one_input_material(
        &mut b,
        |b, m| {
            let ts = sample(b, m, "TS");
            let tint = constant3(b, m, "Tint", [0.5, 0.25, 1.0]);
            let mul = binary(
                b,
                m,
                "MaterialExpressionMultiply",
                "Mul",
                &[
                    ("A", tint, None),
                    ("B", ts, Some([true, false, false, false])),
                ],
            );
            let cm_class = b.class("MaterialExpressionComponentMask");
            let input = link(b, mul, 0, None);
            let p = Tags::new(b)
                .structure("Input", "ExpressionInput", input)
                .boolean("G", true)
                .end();
            (b.export(cm_class, m, "CM", p), None)
        },
        true,
    );
    let a = approx_of(&mut b, m);
    let tex = a.base_color.texture.as_ref().unwrap();
    assert_eq!(tex.channels, "r");
    assert_eq!(a.base_color.value, [0.25; 4]);
    assert!(a.lossless, "{:?}", a.notes);

    // A plain one-channel term is still splatted (unchanged behaviour).
    let mut b = Builder::new();
    let (_, m) = one_input_material(
        &mut b,
        |b, m| {
            let ts = sample(b, m, "TS");
            let k = constant(b, m, "K", 2.0);
            let mul = binary(
                b,
                m,
                "MaterialExpressionMultiply",
                "Mul",
                &[("A", ts, Some([false, true, false, false])), ("B", k, None)],
            );
            (mul, None)
        },
        true,
    );
    let a = approx_of(&mut b, m);
    assert_eq!(a.base_color.value, [2.0; 4]);
    assert_eq!(a.base_color.texture.as_ref().unwrap().channels, "g");
}

/// `Panner` / `Rotator` with a constant time input: a fixed offset and a
/// fixed angle (before the verification pass both were silently dropped and
/// the material still claimed to be lossless).
#[test]
fn panner_and_rotator_with_a_constant_time() {
    let mut b = Builder::new();
    let (_, m) = one_input_material(
        &mut b,
        |b, m| {
            let t = texture(b, m, "T");
            let tc_class = b.class("MaterialExpressionTextureCoordinate");
            let p = Tags::new(b).end();
            let tc = b.export(tc_class, m, "TC", p);
            let two = constant(b, m, "Two", 2.0);
            let pan_class = b.class("MaterialExpressionPanner");
            let coord = link(b, tc, 0, None);
            let time = link(b, two, 0, None);
            let p = Tags::new(b)
                .structure("Coordinate", "ExpressionInput", coord)
                .structure("Time", "ExpressionInput", time)
                .float("SpeedX", 0.25)
                .float("SpeedY", 0.5)
                .end();
            let pan = b.export(pan_class, m, "Pan", p);
            let three = constant(b, m, "Three", 3.0);
            let rot_class = b.class("MaterialExpressionRotator");
            let coord = link(b, pan, 0, None);
            let time = link(b, three, 0, None);
            let p = Tags::new(b)
                .structure("Coordinate", "ExpressionInput", coord)
                .structure("Time", "ExpressionInput", time)
                .float("Speed", 0.5)
                .float("CenterX", 0.25)
                .end();
            let rot = b.export(rot_class, m, "Rot", p);
            let ts_class = b.class("MaterialExpressionTextureSample");
            let coords = link(b, rot, 0, None);
            let p = Tags::new(b)
                .object("Texture", t)
                .structure("Coordinates", "ExpressionInput", coords)
                .end();
            (b.export(ts_class, m, "TS", p), RGB)
        },
        true,
    );
    let a = approx_of(&mut b, m);
    assert!(a.lossless, "{:?}", a.notes);
    let uv = a.base_color.texture.as_ref().unwrap().uv;
    assert_eq!(uv.offset, [0.5, 1.0], "speed * constant time");
    assert_eq!(uv.panning, [0.0, 0.0]);
    assert_eq!(uv.rotation, 0.0);
    assert_eq!(uv.rotation_angle, 1.5, "speed * constant time");
    assert_eq!(uv.rotation_center, [0.25, 0.5]);
    assert!(uv.rotated());

    // Panning after a rotation cannot be expressed by the transform order
    // (pan, then rotate): kept, with a note.
    let mut b = Builder::new();
    let (_, m) = one_input_material(
        &mut b,
        |b, m| {
            let t = texture(b, m, "T");
            let rot_class = b.class("MaterialExpressionRotator");
            let p = Tags::new(b).float("Speed", 1.0).end();
            let rot = b.export(rot_class, m, "Rot", p);
            let pan_class = b.class("MaterialExpressionPanner");
            let coord = link(b, rot, 0, None);
            let p = Tags::new(b)
                .structure("Coordinate", "ExpressionInput", coord)
                .float("SpeedX", 0.5)
                .end();
            let pan = b.export(pan_class, m, "Pan", p);
            let ts_class = b.class("MaterialExpressionTextureSample");
            let coords = link(b, pan, 0, None);
            let p = Tags::new(b)
                .object("Texture", t)
                .structure("Coordinates", "ExpressionInput", coords)
                .end();
            (b.export(ts_class, m, "TS", p), RGB)
        },
        true,
    );
    let a = approx_of(&mut b, m);
    let uv = a.base_color.texture.as_ref().unwrap().uv;
    assert_eq!(uv.rotation, 1.0);
    assert_eq!(uv.panning, [0.5, 0.0]);
    assert!(!a.lossless);
    assert!(
        a.notes.iter().any(|n| n.contains("after a Rotator")),
        "{:?}",
        a.notes
    );
}

/// `MaterialExpressionDepthBiasBlend` extends `TextureSample` (Engine.u
/// class model): its texture is used, the depth fade noted.
#[test]
fn depth_bias_blend_is_a_texture_sample() {
    let mut b = Builder::new();
    let (_, m) = one_input_material(
        &mut b,
        |b, m| {
            let t = texture(b, m, "T");
            let c = b.class("MaterialExpressionDepthBiasBlend");
            let p = Tags::new(b)
                .object("Texture", t)
                .float("BiasScale", 2.0)
                .end();
            (b.export(c, m, "DBB", p), RGB)
        },
        true,
    );
    let a = approx_of(&mut b, m);
    assert_eq!(a.status, ApproxStatus::Approximated);
    let tex = a.base_color.texture.as_ref().unwrap();
    assert_eq!(tex.texture.as_deref(), Some("Mats.M_One.T"));
    assert!(a.unsupported.is_empty(), "{:?}", a.unsupported);
    assert!(a.notes.iter().any(|n| n.contains("DepthBiasBlend")));
}

/// A scalar input (opacity mask, opacity, specular power) reads the first
/// component of a wider value.
#[test]
fn scalar_inputs_read_the_first_component() {
    let mut b = Builder::new();
    let core = b.import("Core", "Package", 0, "Core");
    let package_class = b.import("Core", "Class", core, "Package");
    let root_payload = Tags::new(&mut b).end();
    let root = b.export(package_class, 0, "Mats", root_payload);
    let material_class = b.class("Material");
    let m = b.reserve(material_class, root, "M");
    let v = constant3(&mut b, m, "V", [0.25, 0.5, 0.75]);
    let ts = sample(&mut b, m, "TS");
    let tint = constant3(&mut b, m, "Tint", [3.0, 5.0, 7.0]);
    let tinted = binary(
        &mut b,
        m,
        "MaterialExpressionMultiply",
        "Mul",
        &[("A", ts, ALPHA), ("B", tint, None)],
    );
    let mask = link(&mut b, v, 0, None);
    let power = link(&mut b, tinted, 0, None);
    let mut payload = Tags::new(&mut b)
        .structure("OpacityMask", "ScalarMaterialInput", mask)
        .structure("SpecularPower", "ScalarMaterialInput", power)
        .enumeration("BlendMode", "EBlendMode", "BLEND_Masked")
        .end();
    payload.extend(material_tail(&[Res::simple(1)]));
    b.fill(m, payload);
    let a = approx_of(&mut b, m);
    assert_eq!(a.opacity.as_ref().unwrap().value, [0.25; 4]);
    assert_eq!(a.specular_power.value, [3.0; 4]);
    assert_eq!(a.specular_power.texture.as_ref().unwrap().channels, "a");
    assert_eq!(a.roughness, roughness_from_specular_power(3.0));
}

/// Texture parameters: a null override does not clear the parameter; the
/// parent instance's texture (or the expression's own) is used instead, as
/// the executable's `FMaterialInstanceConstantResource::GetTextureValue`
/// does.
#[test]
fn null_texture_overrides_fall_through_to_the_parent() {
    let mut b = Builder::new();
    let mic_class = b.class("MaterialInstanceConstant");
    for name in ["Leaf", "Middle", "Root"] {
        let p = Tags::new(&mut b).end();
        b.export(mic_class, 0, name, p);
    }
    let (set, lp) = load(b.package());
    let dec = MaterialDecoder::new(&set);
    let mut leaf = dec.decode(&lp, 0).unwrap();
    let mut middle = dec.decode(&lp, 1).unwrap();
    let mut root = dec.decode(&lp, 2).unwrap();
    let prop = |name: &str, value: Value| Property {
        name: name.to_owned(),
        type_name: String::new(),
        array_index: 0,
        size: 0,
        struct_name: None,
        enum_name: None,
        value,
        offset: 0,
    };
    let tex = |n: &str, t: Option<&str>| Value::Struct {
        name: String::new(),
        binary: false,
        fields: vec![
            prop("ParameterName", Value::Name(n.into())),
            prop(
                "ParameterValue",
                Value::Object(ObjRef {
                    index: if t.is_some() { 3 } else { 0 },
                    path: t.unwrap_or("None").into(),
                }),
            ),
        ],
    };
    let list = |items: Vec<Value>| prop("TextureParameterValues", Value::Array(items));
    // Leaf clears Diffuse and Detail; Middle clears Diffuse too; Root sets
    // Diffuse. Detail is set nowhere else; Mask is set by Leaf (the first of
    // two entries of the same name counts).
    leaf.properties.push(list(vec![
        tex("Diffuse", None),
        tex("Detail", None),
        tex("Mask", Some("P.T_Mask")),
        tex("Mask", Some("P.T_Other")),
    ]));
    middle.properties.push(list(vec![
        tex("Diffuse", None),
        tex("Mask", Some("P.T_M2")),
    ]));
    root.properties
        .push(list(vec![tex("Diffuse", Some("P.T_Root"))]));
    let chain = MaterialChain {
        links: vec![(lp.clone(), leaf), (lp.clone(), middle), (lp.clone(), root)],
        error: None,
    };
    let p = ParameterValues::from_chain(&chain);
    assert_eq!(
        p.textures.get("diffuse"),
        Some(&Some("P.T_Root".to_owned()))
    );
    assert_eq!(p.textures.get("detail"), Some(&None));
    assert_eq!(p.textures.get("mask"), Some(&Some("P.T_Mask".to_owned())));
}

// ---------------------------------------------------------------------------
// Added after the first play session: what a channel lost, lengths, gradients,
// coordinates put together from components, vertex colour, texture statistics
// ---------------------------------------------------------------------------

/// A material with the given inputs (`DiffuseColor`, `EmissiveColor`,
/// `Opacity`, ...), each one expression built inside the material.
fn material_with(
    b: &mut Builder,
    build: impl FnOnce(&mut Builder, i32) -> Vec<(&'static str, i32, Option<[bool; 4]>)>,
    blend: &str,
    unlit: bool,
) -> i32 {
    let core = b.import("Core", "Package", 0, "Core");
    let package_class = b.import("Core", "Class", core, "Package");
    let root_payload = Tags::new(b).end();
    let root = b.export(package_class, 0, "Mats", root_payload);
    let material_class = b.class("Material");
    let m = b.reserve(material_class, root, "M_One");
    let inputs = build(b, m);
    let mut bodies = Vec::new();
    for (name, expr, mask) in inputs {
        let kind = match name {
            "Opacity" | "OpacityMask" | "SpecularPower" => "ScalarMaterialInput",
            "Normal" => "VectorMaterialInput",
            _ => "ColorMaterialInput",
        };
        bodies.push((name, kind, link(b, expr, 0, mask)));
    }
    let mut t = Tags::new(b);
    for (name, kind, body) in bodies {
        t = t.structure(name, kind, body);
    }
    t = t.enumeration("BlendMode", "EBlendMode", blend);
    if unlit {
        t = t.enumeration("LightingModel", "EMaterialLightingModel", "MLM_Unlit");
    }
    let mut payload = t.end();
    payload.extend(material_tail(&[Res::simple(1)]));
    b.fill(m, payload);
    m
}

/// An expression without properties (`PixelDepth`, `VertexColor`, ...).
fn bare(b: &mut Builder, outer: i32, kind: &str, name: &str) -> i32 {
    let c = b.class(&format!("MaterialExpression{kind}"));
    let p = Tags::new(b).end();
    b.export(c, outer, name, p)
}

fn coordinates(b: &mut Builder, outer: i32, name: &str, tiling: [f32; 2]) -> i32 {
    let c = b.class("MaterialExpressionTextureCoordinate");
    let p = Tags::new(b)
        .float("UTiling", tiling[0])
        .float("VTiling", tiling[1])
        .end();
    b.export(c, outer, name, p)
}

/// `ComponentMask` of `input` keeping red (`false`) or green (`true`).
fn component(b: &mut Builder, outer: i32, name: &str, input: i32, green: bool) -> i32 {
    let c = b.class("MaterialExpressionComponentMask");
    let body = link(b, input, 0, None);
    let p = Tags::new(b)
        .structure("Input", "ExpressionInput", body)
        .boolean("R", !green)
        .boolean("G", green)
        .end();
    b.export(c, outer, name, p)
}

/// `ConstantClamp` of `input` to 0..1.
fn clamp01(b: &mut Builder, outer: i32, name: &str, input: i32) -> i32 {
    let c = b.class("MaterialExpressionConstantClamp");
    let body = link(b, input, 0, None);
    let p = Tags::new(b)
        .structure("Input", "ExpressionInput", body)
        .float("Min", 0.0)
        .float("Max", 1.0)
        .end();
    b.export(c, outer, name, p)
}

/// A texture sample of `<name>_Tex` at the coordinates `coords` (0: the
/// mesh's own).
fn sample_at(b: &mut Builder, outer: i32, name: &str, coords: i32) -> i32 {
    let t = texture(b, outer, &format!("{name}_Tex"));
    let c = b.class("MaterialExpressionTextureSample");
    let mut tags = Tags::new(b).object("Texture", t);
    if coords != 0 {
        let body = link(tags.b, coords, 0, None);
        tags = tags.structure("Coordinates", "ExpressionInput", body);
    }
    let p = tags.end();
    b.export(c, outer, name, p)
}

fn approx_with(
    b: &mut Builder,
    m: i32,
    prepare: impl FnOnce(&MaterialDecoder<'_>),
) -> ApproxMaterial {
    let (set, lp) = load(b.package());
    let dec = MaterialDecoder::new(&set);
    prepare(&dec);
    dec.approximate(&lp, (m - 1) as usize).unwrap()
}

fn stats(mean: [f32; 3], chroma: f32, uniform: bool) -> TextureStats {
    TextureStats {
        mean: [mean[0], mean[1], mean[2], 1.0],
        chroma,
        uniform,
    }
}

fn bound(a: &ApproxMaterial) -> Option<&str> {
    a.base_color.texture.as_ref()?.texture.as_deref()
}

/// A depth scaled by a constant is still a depth: the constant says nothing
/// without it and must not end up as a blend weight or as an opacity. (The
/// distance blend between a near and a far tiling used to reduce to the far
/// texture times 0.0005, and a depth fade to an opacity of 0.00125.)
#[test]
fn a_length_keeps_the_constants_combined_with_it() {
    let mut b = Builder::new();
    let m = material_with(
        &mut b,
        |b, m| {
            let near = sample(b, m, "Near");
            let far = sample(b, m, "Far");
            let depth = bare(b, m, "PixelDepth", "Depth");
            let k = constant(b, m, "K", 0.0005);
            let scaled = binary(
                b,
                m,
                "MaterialExpressionMultiply",
                "Scaled",
                &[("A", depth, None), ("B", k, None)],
            );
            let alpha = clamp01(b, m, "Alpha", scaled);
            let blend = binary(
                b,
                m,
                "MaterialExpressionLinearInterpolate",
                "Blend",
                &[("A", near, RGB), ("B", far, RGB), ("Alpha", alpha, None)],
            );
            // Opacity = texture alpha * clamp(depth * 0.00125) * 0.4
            let mask = sample(b, m, "Mask");
            let depth2 = bare(b, m, "PixelDepth", "Depth2");
            let k2 = constant(b, m, "K2", 0.00125);
            let fade = binary(
                b,
                m,
                "MaterialExpressionMultiply",
                "Fade",
                &[("A", k2, None), ("B", depth2, None)],
            );
            let fade = clamp01(b, m, "FadeClamp", fade);
            let strength = constant(b, m, "Strength", 0.4);
            let faded = binary(
                b,
                m,
                "MaterialExpressionMultiply",
                "Faded",
                &[("A", mask, ALPHA), ("B", fade, None)],
            );
            let opacity = binary(
                b,
                m,
                "MaterialExpressionMultiply",
                "Opacity",
                &[("A", faded, None), ("B", strength, None)],
            );
            // Specular: one texture at a near (8) and a far (2) tiling,
            // blended by the same kind of clamped depth: the far one.
            let tile = texture(b, m, "Tile_Tex");
            let ts = b.class("MaterialExpressionTextureSample");
            let mut sides = Vec::new();
            for (name, tiling) in [("TileNear", 8.0), ("TileFar", 2.0)] {
                let tc = coordinates(b, m, &format!("{name}TC"), [tiling, tiling]);
                let body = link(b, tc, 0, None);
                let p = Tags::new(b)
                    .object("Texture", tile)
                    .structure("Coordinates", "ExpressionInput", body)
                    .end();
                sides.push(b.export(ts, m, name, p));
            }
            let depth3 = bare(b, m, "PixelDepth", "Depth3");
            let k3 = constant(b, m, "K3", 0.0005);
            let far = binary(
                b,
                m,
                "MaterialExpressionMultiply",
                "Far3",
                &[("A", depth3, None), ("B", k3, None)],
            );
            let far = clamp01(b, m, "Far3Clamp", far);
            let tiles = binary(
                b,
                m,
                "MaterialExpressionLinearInterpolate",
                "Tiles",
                &[
                    ("A", sides[0], RGB),
                    ("B", sides[1], RGB),
                    ("Alpha", far, None),
                ],
            );
            vec![
                ("DiffuseColor", blend, None),
                ("Opacity", opacity, None),
                ("SpecularColor", tiles, None),
            ]
        },
        "BLEND_Translucent",
        false,
    );
    let a = approx_of(&mut b, m);
    assert_eq!(bound(&a), Some("Mats.M_One.Near_Tex"), "{:?}", a.notes);
    assert_eq!(a.base_color.value, [1.0, 1.0, 1.0, 1.0]);
    assert_eq!(a.dropped.get("base_color"), Some(&vec!["depth", "texture"]));
    assert_eq!(a.dropped_textures, vec!["Mats.M_One.Far_Tex".to_owned()]);
    assert_eq!(a.specular.texture.as_ref().unwrap().uv.scale, [2.0, 2.0]);
    assert_eq!(a.dropped.get("specular"), Some(&vec!["depth"]));
    let o = a.opacity.as_ref().unwrap();
    assert_eq!(
        o.texture.as_ref().unwrap().texture.as_deref(),
        Some("Mats.M_One.Mask_Tex")
    );
    assert!((o.value[0] - 0.4).abs() < 1e-6, "{:?}", o.value);
    assert_eq!(a.dropped.get("opacity"), Some(&vec!["depth"]));
    assert_eq!(a.unsupported.get("PixelDepth"), Some(&3));
    assert!(!a.lossless);
}

/// An opacity that is nothing but a comparison of depths (a glow at depth
/// edges) reduces to the lerp's representable side, and says what it lost.
#[test]
fn an_opacity_built_from_depth_alone() {
    let mut b = Builder::new();
    let m = material_with(
        &mut b,
        |b, m| {
            let colour = constant3(b, m, "Colour", [0.25, 1.5, 4.0]);
            let zero = constant(b, m, "Zero", 0.0);
            let scene = bare(b, m, "SceneDepth", "Scene");
            let pixel = bare(b, m, "PixelDepth", "Pixel");
            let edge = binary(
                b,
                m,
                "MaterialExpressionSubtract",
                "Edge",
                &[("A", scene, None), ("B", pixel, None)],
            );
            let pixel2 = bare(b, m, "PixelDepth", "Pixel2");
            let k = constant(b, m, "K", 0.0015);
            let near = binary(
                b,
                m,
                "MaterialExpressionMultiply",
                "Near",
                &[("A", pixel2, None), ("B", k, None)],
            );
            let near = clamp01(b, m, "NearClamp", near);
            let near = binary(
                b,
                m,
                "MaterialExpressionOneMinus",
                "Inv",
                &[("Input", near, None)],
            );
            let opacity = binary(
                b,
                m,
                "MaterialExpressionLinearInterpolate",
                "Lerp",
                &[("A", zero, None), ("B", edge, None), ("Alpha", near, None)],
            );
            vec![("EmissiveColor", colour, None), ("Opacity", opacity, None)]
        },
        "BLEND_Translucent",
        true,
    );
    let a = approx_of(&mut b, m);
    assert_eq!(a.status, ApproxStatus::Approximated);
    let o = a.opacity.as_ref().unwrap();
    assert!(o.resolved);
    assert_eq!(o.value[0], 0.0, "{:?}", a.notes);
    assert_eq!(a.dropped.get("opacity"), Some(&vec!["depth"]));
    assert!(a.dropped.get("emissive").is_none());
    assert_eq!(a.emissive.value, [0.25, 1.5, 4.0, 1.0]);

    // The depth alone as the opacity: unresolved, and the reason is kept.
    let mut b = Builder::new();
    let m = material_with(
        &mut b,
        |b, m| {
            let scene = bare(b, m, "SceneDepth", "Scene");
            let k = constant(b, m, "K", 0.01);
            let scaled = binary(
                b,
                m,
                "MaterialExpressionMultiply",
                "Scaled",
                &[("A", scene, None), ("B", k, None)],
            );
            vec![("Opacity", scaled, None)]
        },
        "BLEND_Additive",
        false,
    );
    let a = approx_of(&mut b, m);
    let o = a.opacity.as_ref().unwrap();
    assert!(!o.resolved);
    assert_eq!(o.value[0], 1.0, "the engine default, not 0.01");
    assert_eq!(a.dropped.get("opacity"), Some(&vec!["depth"]));
}

/// A channel computed from one texture coordinate (a fade along a mesh) is
/// a gradient: the product of powers of `scale * coordinate + offset`.
#[test]
fn a_function_of_one_coordinate_is_a_gradient() {
    let mut b = Builder::new();
    let m = material_with(
        &mut b,
        |b, m| {
            let colour = constant3(b, m, "Colour", [0.125, 0.5, 1.0]);
            let tc = coordinates(b, m, "TC", [1.0, 1.0]);
            let v = component(b, m, "V", tc, true);
            let five = constant(b, m, "Five", 5.0);
            let fade = binary(
                b,
                m,
                "MaterialExpressionPower",
                "Pow",
                &[("Base", v, None), ("Exponent", five, None)],
            );
            let emissive = binary(
                b,
                m,
                "MaterialExpressionMultiply",
                "Glow",
                &[("A", colour, None), ("B", fade, None)],
            );
            // Opacity = (1 - u * 2) * v^5 * 0.5, u from coordinates tiled 2 x 1.
            let tc2 = coordinates(b, m, "TC2", [2.0, 1.0]);
            let u = component(b, m, "U", tc2, false);
            let inv = binary(
                b,
                m,
                "MaterialExpressionOneMinus",
                "Inv",
                &[("Input", u, None)],
            );
            let half = constant(b, m, "Half", 0.5);
            let both = binary(
                b,
                m,
                "MaterialExpressionMultiply",
                "Both",
                &[("A", inv, None), ("B", fade, None)],
            );
            let opacity = binary(
                b,
                m,
                "MaterialExpressionMultiply",
                "Opacity",
                &[("A", both, None), ("B", half, None)],
            );
            vec![
                ("EmissiveColor", emissive, None),
                ("Opacity", opacity, None),
            ]
        },
        "BLEND_Translucent",
        false,
    );
    let a = approx_of(&mut b, m);
    assert!(a.emissive.resolved && a.emissive.texture.is_none());
    assert_eq!(a.emissive.value, [0.125, 0.5, 1.0, 1.0]);
    let g = a.gradients.get("emissive").unwrap();
    assert_eq!(g.factors.len(), 1);
    let f = g.factors[0];
    assert_eq!(
        (f.uv_channel, f.component, f.scale, f.offset, f.power),
        (0, "v", 1.0, 0.0, 5.0)
    );
    let o = a.opacity.as_ref().unwrap();
    assert!(o.resolved);
    assert_eq!(o.value[0], 0.5);
    let g = a.gradients.get("opacity").unwrap();
    assert_eq!(g.factors.len(), 2);
    let (u, v) = (g.factors[0], g.factors[1]);
    assert_eq!(
        (u.component, u.scale, u.offset, u.power),
        ("u", -2.0, 1.0, 1.0)
    );
    assert_eq!(
        (v.component, v.scale, v.offset, v.power),
        ("v", 1.0, 0.0, 5.0)
    );
    // At (u, v) = (0.25, 1): (1 - 0.5) * 1 = 0.5.
    assert!((g.eval(|_| [0.25, 1.0]) - 0.5).abs() < 1e-6);
    assert!(a.dropped.is_empty(), "{:?}", a.dropped);
    assert!(a.lossless, "{:?}", a.notes);

    // Both coordinates at once are coordinates, not a value.
    let mut b = Builder::new();
    let m = material_with(
        &mut b,
        |b, m| {
            let tc = coordinates(b, m, "TC", [1.0, 1.0]);
            vec![("EmissiveColor", tc, None)]
        },
        "BLEND_Opaque",
        true,
    );
    let a = approx_of(&mut b, m);
    assert!(!a.emissive.resolved);
    assert_eq!(a.status, ApproxStatus::Fallback);
    assert_eq!(a.dropped.get("emissive"), Some(&vec!["coordinates"]));
    assert!(a.gradients.is_empty());
}

/// Coordinates put together from components (`AppendVector` of a value
/// along u and one along v, or a constant) are coordinates again, and a
/// component of a view vector scaled and offset stands for its offset.
#[test]
fn coordinates_from_components() {
    let mut b = Builder::new();
    let m = material_with(
        &mut b,
        |b, m| {
            let tc = coordinates(b, m, "TC", [1.0, 1.0]);
            let u = component(b, m, "U", tc, false);
            let three = constant(b, m, "Three", 3.0);
            let wide = binary(
                b,
                m,
                "MaterialExpressionMultiply",
                "Wide",
                &[("A", u, None), ("B", three, None)],
            );
            let v = component(b, m, "V", tc, true);
            let uv = binary(
                b,
                m,
                "MaterialExpressionAppendVector",
                "UV",
                &[("A", wide, None), ("B", v, None)],
            );
            let diffuse = sample_at(b, m, "D", uv);
            // Opacity: a falloff sampled at (0.5 + view.x / 2.5, v).
            let view = bare(b, m, "ReflectionVector", "View");
            let x = component(b, m, "X", view, false);
            let k = constant(b, m, "K", 2.5);
            let scaled = binary(
                b,
                m,
                "MaterialExpressionDivide",
                "Scaled",
                &[("A", x, None), ("B", k, None)],
            );
            let half = constant(b, m, "Half", 0.5);
            let across = binary(
                b,
                m,
                "MaterialExpressionAdd",
                "Across",
                &[("A", scaled, None), ("B", half, None)],
            );
            let v2 = component(b, m, "V2", tc, true);
            let uv2 = binary(
                b,
                m,
                "MaterialExpressionAppendVector",
                "UV2",
                &[("A", across, None), ("B", v2, None)],
            );
            let falloff = sample_at(b, m, "F", uv2);
            vec![("DiffuseColor", diffuse, RGB), ("Opacity", falloff, None)]
        },
        "BLEND_Translucent",
        false,
    );
    let a = approx_of(&mut b, m);
    let t = a.base_color.texture.as_ref().unwrap();
    assert_eq!(t.uv.scale, [3.0, 1.0]);
    assert_eq!(t.uv.offset, [0.0, 0.0]);
    assert!(a.dropped.get("base_color").is_none(), "{:?}", a.notes);
    let o = a.opacity.as_ref().unwrap().texture.as_ref().unwrap();
    assert_eq!(o.texture.as_deref(), Some("Mats.M_One.F_Tex"));
    assert_eq!(o.uv.scale, [0.0, 1.0], "{:?}", a.notes);
    assert_eq!(o.uv.offset, [0.5, 0.0]);
    assert_eq!(a.dropped.get("opacity"), Some(&vec!["view"]));
}

/// Vertex colour is not exported: it counts as white (an unpainted mesh),
/// so a blend it saturates is decided, and a blend by a texture is not.
#[test]
fn vertex_colour_counts_as_white() {
    let build = |alpha_from_texture: bool| {
        let mut b = Builder::new();
        let m = material_with(
            &mut b,
            |b, m| {
                let moss = sample(b, m, "Moss");
                let rock = sample(b, m, "Rock");
                let mask = sample(b, m, "Mask");
                let alpha = if alpha_from_texture {
                    mask
                } else {
                    // vertex red + (1 - mask red), clamped: 1 and above.
                    let paint = bare(b, m, "VertexColor", "Paint");
                    let inv = binary(
                        b,
                        m,
                        "MaterialExpressionOneMinus",
                        "Inv",
                        &[("Input", mask, RED)],
                    );
                    let sum = binary(
                        b,
                        m,
                        "MaterialExpressionAdd",
                        "Sum",
                        &[("A", paint, RED), ("B", inv, None)],
                    );
                    clamp01(b, m, "Clamp", sum)
                };
                let blend = binary(
                    b,
                    m,
                    "MaterialExpressionLinearInterpolate",
                    "Blend",
                    &[("A", moss, RGB), ("B", rock, RGB), ("Alpha", alpha, RED)],
                );
                vec![("DiffuseColor", blend, None)]
            },
            "BLEND_Opaque",
            false,
        );
        approx_of(&mut b, m)
    };
    let painted = build(false);
    assert_eq!(
        bound(&painted),
        Some("Mats.M_One.Rock_Tex"),
        "{:?}",
        painted.notes
    );
    assert!(
        painted
            .dropped
            .get("base_color")
            .is_some_and(|d| d.contains(&"vertex_color"))
    );
    // A mask texture varies: input A stands for the blend, as before.
    let masked = build(true);
    assert_eq!(bound(&masked), Some("Mats.M_One.Moss_Tex"));
    assert_eq!(masked.dropped.get("base_color"), Some(&vec!["texture"]));
}

const RED: Option<[bool; 4]> = Some([true, false, false, false]);

/// What the textures hold decides between two of them: in a product the
/// coloured one is shown and the grey one (an occlusion map) stands in by
/// its mean; a texture of one colour is that colour; a lookup that has no
/// place on the surface (view-dependent coordinates, a cube map) is its
/// mean; an object filled at run time is named.
#[test]
fn texture_statistics_decide_between_textures() {
    let mut b = Builder::new();
    let m = material_with(
        &mut b,
        |b, m| {
            // Occlusion (unique, tiling 1) * rock tile (tiling 13) * white.
            let ao = sample(b, m, "AO");
            let tc = coordinates(b, m, "TC", [13.0, 13.0]);
            let tile = sample_at(b, m, "Tile", tc);
            let white = sample(b, m, "White");
            let shaded = binary(
                b,
                m,
                "MaterialExpressionMultiply",
                "Shaded",
                &[("A", ao, RGB), ("B", tile, RGB)],
            );
            let diffuse = binary(
                b,
                m,
                "MaterialExpressionMultiply",
                "Diffuse",
                &[("A", white, RGB), ("B", shaded, None)],
            );
            // Emissive = colour + reflection lookup * 0.5.
            let view = bare(b, m, "ReflectionVector", "View");
            let refl = sample_at(b, m, "Refl", view);
            let half = constant(b, m, "Half", 0.5);
            let dim = binary(
                b,
                m,
                "MaterialExpressionMultiply",
                "Dim",
                &[("A", refl, RGB), ("B", half, None)],
            );
            let colour = constant3(b, m, "Colour", [0.1, 0.2, 0.3]);
            let emissive = binary(
                b,
                m,
                "MaterialExpressionAdd",
                "Emissive",
                &[("A", colour, None), ("B", dim, None)],
            );
            // Specular = a cube map sampled by a plain texture sample.
            let cube = sample(b, m, "Cube");
            // Opacity mask = occlusion * tile again: no mean is folded in.
            let ao2 = sample(b, m, "AO2");
            let tc2 = coordinates(b, m, "TC2", [13.0, 13.0]);
            let tile2 = sample_at(b, m, "Tile2", tc2);
            let mask = binary(
                b,
                m,
                "MaterialExpressionMultiply",
                "Mask",
                &[("A", ao2, RGB), ("B", tile2, RGB)],
            );
            // Normal: a render target.
            let screen = sample(b, m, "Screen");
            vec![
                ("DiffuseColor", diffuse, None),
                ("EmissiveColor", emissive, None),
                ("SpecularColor", cube, RGB),
                ("Normal", screen, RGB),
                ("OpacityMask", mask, None),
            ]
        },
        "BLEND_Masked",
        false,
    );
    let prepare = |dec: &MaterialDecoder<'_>| {
        dec.assume_texture(
            "Mats.M_One.AO_Tex",
            Some(stats([0.5, 0.5, 0.5], 0.01, false)),
            false,
            false,
        );
        dec.assume_texture(
            "Mats.M_One.Tile_Tex",
            Some(stats([0.2, 0.15, 0.1], 0.1, false)),
            false,
            false,
        );
        dec.assume_texture(
            "Mats.M_One.White_Tex",
            Some(stats([1.0, 1.0, 1.0], 0.0, true)),
            false,
            false,
        );
        dec.assume_texture(
            "Mats.M_One.Refl_Tex",
            Some(stats([0.2, 0.4, 0.6], 0.2, false)),
            false,
            false,
        );
        dec.assume_texture(
            "Mats.M_One.Cube_Tex",
            Some(stats([0.25, 0.25, 0.25], 0.0, false)),
            false,
            true,
        );
        dec.assume_texture("Mats.M_One.Screen_Tex", None, true, false);
        dec.assume_texture(
            "Mats.M_One.AO2_Tex",
            Some(stats([0.5, 0.5, 0.5], 0.01, false)),
            false,
            false,
        );
        dec.assume_texture(
            "Mats.M_One.Tile2_Tex",
            Some(stats([0.2, 0.15, 0.1], 0.1, false)),
            false,
            false,
        );
    };
    let a = approx_with(&mut b, m, prepare);
    assert_eq!(bound(&a), Some("Mats.M_One.Tile_Tex"), "{:?}", a.notes);
    let t = a.base_color.texture.as_ref().unwrap();
    assert_eq!(t.uv.scale, [13.0, 13.0]);
    assert_eq!(a.base_color.value, [0.5, 0.5, 0.5, 1.0]);
    assert_eq!(a.dropped.get("base_color"), Some(&vec!["texture"]));
    assert_eq!(
        a.dropped_textures,
        vec![
            "Mats.M_One.AO2_Tex".to_owned(),
            "Mats.M_One.AO_Tex".to_owned()
        ]
    );
    assert_eq!(
        a.texture_stats.get("Mats.M_One.Tile_Tex").map(|s| s.mean),
        Some([0.2, 0.15, 0.1, 1.0])
    );
    // The white texture is a constant: not bound, not dropped.
    assert!(!a.textures.contains(&"Mats.M_One.White_Tex".to_owned()));
    // The reflection lookup stands in by its mean: 0.1 + 0.2 * 0.5, ...
    assert!(a.emissive.texture.is_none());
    for (got, want) in a.emissive.value.iter().zip([0.2, 0.4, 0.6]) {
        assert!((got - want).abs() < 1e-6, "{:?}", a.emissive.value);
    }
    assert_eq!(a.dropped.get("emissive"), Some(&vec!["view"]));
    // The cube map too, whatever samples it.
    assert!(a.specular.texture.is_none());
    assert_eq!(a.specular.value[0], 0.25);
    // An opacity keeps the same texture but not the other's mean: a mean
    // could push a whole cut-out under its clip value.
    let o = a.opacity.as_ref().unwrap();
    assert_eq!(
        o.texture.as_ref().unwrap().texture.as_deref(),
        Some("Mats.M_One.Tile2_Tex")
    );
    assert_eq!(o.value, [1.0, 1.0, 1.0, 1.0]);
    // The render target stays bound and is named as filled at run time.
    assert_eq!(a.runtime_textures, vec!["Mats.M_One.Screen_Tex".to_owned()]);

    // Without statistics the old rule holds: the coarser tiling is kept.
    let b2 = &mut b;
    let old = approx_of(b2, m);
    assert_eq!(bound(&old), Some("Mats.M_One.White_Tex"), "{:?}", old.notes);
    assert!(old.texture_stats.is_empty() && old.runtime_textures.is_empty());
}
