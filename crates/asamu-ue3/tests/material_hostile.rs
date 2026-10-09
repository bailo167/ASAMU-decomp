//! Hostile-input tests for material decoding: truncated, mutated and noisy
//! native tails, and hostile material graphs (cycles, exponential fan-out,
//! deep and cyclic instance chains, mutated payloads) in synthetic packages.
//! Nothing may panic or hang; every accepted native tail must re-encode to the
//! same bytes.

#![allow(clippy::unwrap_used)]

mod common;

use asamu_ue3::Package;
use asamu_ue3::material::{
    ApproxStatus, MaterialDecoder, NativeKind, decode_material_native, encode_material_native,
};
use asamu_ue3::model::PackageSet;
use common::{Export, Import, Synth, W};

/// Deterministic xorshift generator.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % (n.max(1) as u64)) as usize
    }
}

fn resource(w: &mut W, id: u32) {
    w.i32(1);
    w.fstring("error text");
    w.i32(2);
    for (e, l) in [(3, 1), (4, 2)] {
        w.i32(e);
        w.i32(l);
    }
    w.i32(2);
    for v in [id, 1, 2, 3] {
        w.u32(v);
    }
    w.u32(1);
    w.i32(2);
    w.i32(-1);
    w.i32(5);
    for f in [0u32, 1, 0, 1, 0] {
        w.u32(f);
    }
    w.u32(0);
    w.i32(1);
    w.i32(0);
    w.i32(1);
    w.bytes(&1.0f32.to_le_bytes());
    w.bytes(&2.0f32.to_le_bytes());
    w.u32(7);
    for v in [2u32, 0, 0] {
        w.u32(v);
    }
}

fn static_set(w: &mut W) {
    for v in [1u32, 2, 3, 4] {
        w.u32(v);
    }
    w.i32(1);
    w.i32(1);
    w.i32(0);
    w.u32(1);
    w.u32(1);
    for g in [5u32; 4] {
        w.u32(g);
    }
    w.i32(1);
    w.i32(1);
    w.i32(0);
    for c in [1u32, 0, 0, 1, 1] {
        w.u32(c);
    }
    for g in [6u32; 4] {
        w.u32(g);
    }
    w.i32(1);
    w.i32(1);
    w.i32(0);
    w.0.push(3);
    w.u32(0);
    for g in [7u32; 4] {
        w.u32(g);
    }
    w.i32(1);
    w.i32(1);
    w.i32(0);
    w.i32(2);
    w.u32(1);
    for g in [8u32; 4] {
        w.u32(g);
    }
}

fn material_tail() -> Vec<u8> {
    let mut w = W::default();
    w.u32(3);
    resource(&mut w, 1);
    resource(&mut w, 2);
    w.0
}

fn instance_tail() -> Vec<u8> {
    let mut w = W::default();
    w.u32(1);
    resource(&mut w, 9);
    static_set(&mut w);
    w.0
}

const KINDS: [NativeKind; 2] = [
    NativeKind::Material,
    NativeKind::Instance {
        static_permutation: true,
    },
];

fn check_round_trip(data: &[u8], kind: NativeKind) {
    if let Ok(n) = decode_material_native(data, 0, kind) {
        let enc = encode_material_native(&n, kind).expect("accepted input must encode");
        assert_eq!(enc, data, "accepted input must re-encode to the same bytes");
    }
}

#[test]
fn fixtures_decode() {
    for (tail, kind) in [(material_tail(), KINDS[0]), (instance_tail(), KINDS[1])] {
        let n = decode_material_native(&tail, 0, kind).unwrap();
        assert_eq!(encode_material_native(&n, kind).unwrap(), tail);
    }
}

#[test]
fn every_truncation_is_rejected() {
    for (tail, kind) in [(material_tail(), KINDS[0]), (instance_tail(), KINDS[1])] {
        for cut in 0..tail.len() {
            assert!(
                decode_material_native(&tail[..cut], 0, kind).is_err(),
                "truncated at {cut} accepted"
            );
        }
        let mut longer = tail.clone();
        longer.push(0);
        assert!(decode_material_native(&longer, 0, kind).is_err());
    }
}

#[test]
fn bit_flips_and_extreme_values_never_panic() {
    for (tail, kind) in [(material_tail(), KINDS[0]), (instance_tail(), KINDS[1])] {
        for at in 0..tail.len() {
            for bit in 0..8 {
                let mut t = tail.clone();
                t[at] ^= 1 << bit;
                check_round_trip(&t, kind);
            }
            for v in [i32::MAX, i32::MIN, -1, 0x4000_0000] {
                let mut t = tail.clone();
                let end = (at + 4).min(t.len());
                let bytes = v.to_le_bytes();
                t[at..end].copy_from_slice(&bytes[..end - at]);
                check_round_trip(&t, kind);
            }
        }
    }
}

#[test]
fn random_mutations_and_noise_never_panic() {
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    let bases = [(material_tail(), KINDS[0]), (instance_tail(), KINDS[1])];
    for _ in 0..4000 {
        let (base, kind) = &bases[rng.below(2)];
        let mut t = base.clone();
        for _ in 0..1 + rng.below(6) {
            match rng.below(4) {
                0 => {
                    let i = rng.below(t.len());
                    t[i] = rng.next() as u8;
                }
                1 => {
                    let i = rng.below(t.len());
                    t.insert(i, rng.next() as u8);
                }
                2 if t.len() > 1 => {
                    let i = rng.below(t.len());
                    t.remove(i);
                }
                _ => {
                    let i = rng.below(t.len());
                    let end = (i + 4).min(t.len());
                    let v = (rng.next() as i32).to_le_bytes();
                    t[i..end].copy_from_slice(&v[..end - i]);
                }
            }
        }
        check_round_trip(&t, *kind);
    }
    for len in 0..200 {
        let noise: Vec<u8> = (0..len).map(|_| rng.next() as u8).collect();
        for kind in KINDS {
            check_round_trip(&noise, kind);
        }
    }
}

#[test]
fn huge_counts_are_refused_before_allocating() {
    // CompileErrors count = i32::MAX right after the mask.
    let mut w = W::default();
    w.u32(1);
    w.i32(i32::MAX);
    assert!(decode_material_native(&w.0, 0, NativeKind::Material).is_err());
    // TextureDependencyLengthMap count far beyond the data.
    let mut w = W::default();
    w.u32(1);
    w.i32(0);
    w.i32(0x1000_0000);
    w.bytes(&[0; 64]);
    assert!(decode_material_native(&w.0, 0, NativeKind::Material).is_err());
    // Negative count.
    let mut w = W::default();
    w.u32(1);
    w.i32(-5);
    assert!(decode_material_native(&w.0, 0, NativeKind::Material).is_err());
}

// ---------------------------------------------------------------------------
// Hostile graphs
// ---------------------------------------------------------------------------

#[derive(Default)]
struct B {
    names: Vec<String>,
    imports: Vec<Import>,
    exports: Vec<Export>,
}

impl B {
    fn new() -> B {
        let mut b = B::default();
        b.n("None");
        b
    }

    fn n(&mut self, s: &str) -> i32 {
        if let Some(i) = self.names.iter().position(|x| x == s) {
            return i as i32;
        }
        self.names.push(s.to_owned());
        (self.names.len() - 1) as i32
    }

    fn class(&mut self, class: &str) -> i32 {
        let engine = Import {
            class_package: self.n("Core"),
            class_name: self.n("Package"),
            outer: 0,
            name: self.n("Engine"),
            number: 0,
        };
        let ei = match self
            .imports
            .iter()
            .position(|i| i.name == engine.name && i.outer == 0)
        {
            Some(i) => -(i as i32) - 1,
            None => {
                self.imports.push(engine);
                -(self.imports.len() as i32)
            }
        };
        let c = Import {
            class_package: self.n("Core"),
            class_name: self.n("Class"),
            outer: ei,
            name: self.n(class),
            number: 0,
        };
        match self
            .imports
            .iter()
            .position(|i| i.name == c.name && i.outer == ei)
        {
            Some(i) => -(i as i32) - 1,
            None => {
                self.imports.push(c);
                -(self.imports.len() as i32)
            }
        }
    }

    fn export(&mut self, class: i32, outer: i32, name: &str) -> i32 {
        let name = self.n(name);
        self.exports.push(Export {
            class,
            super_: 0,
            outer,
            name,
            number: 0,
            archetype: 0,
            object_flags: 0x0007_0004_0000_0000,
            payload: Vec::new(),
            export_flags: 1,
            net_counts: Vec::new(),
            guid: [0; 4],
            package_flags: 0,
        });
        self.exports.len() as i32
    }

    fn set(&mut self, e: i32, payload: Vec<u8>) {
        self.exports[(e - 1) as usize].payload = payload;
    }

    fn package(&self) -> Option<Package> {
        let s = Synth {
            package_flags: 8,
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
        Package::from_bytes(s.build().0).ok()
    }

    /// Tag header.
    fn tag(&mut self, w: &mut W, name: &str, ty: &str, size: usize) {
        let (n, t) = (self.n(name), self.n(ty));
        for v in [n, 0, t, 0, size as i32, 0] {
            w.i32(v);
        }
    }

    /// `name = ExpressionInput(Expression = expr)`.
    fn input(&mut self, w: &mut W, name: &str, expr: i32) {
        let mut body = W::default();
        self.tag(&mut body, "Expression", "ObjectProperty", 4);
        body.i32(expr);
        body.i32(0);
        body.i32(0);
        self.tag(w, name, "StructProperty", body.len());
        let s = self.n("ExpressionInput");
        w.i32(s);
        w.i32(0);
        w.bytes(&body.0);
    }

    fn object(&mut self, w: &mut W, name: &str, v: i32) {
        self.tag(w, name, "ObjectProperty", 4);
        w.i32(v);
    }
}

fn end(w: &mut W) {
    w.i32(0);
    w.i32(0);
}

fn material_payload(b: &mut B, diffuse: i32) -> Vec<u8> {
    let mut w = W::default();
    w.i32(0);
    b.input(&mut w, "DiffuseColor", diffuse);
    end(&mut w);
    w.u32(0); // quality mask 0: no resources
    w.0
}

fn approximate_all(pkg: Package) -> Vec<Option<ApproxStatus>> {
    let set = PackageSet::new::<&str>(&[]);
    let lp = set.insert_package("Hostile", pkg);
    let dec = MaterialDecoder::new(&set);
    (0..lp.package.exports.len())
        .map(|i| dec.approximate(&lp, i).ok().map(|a| a.status))
        .collect()
}

#[test]
fn expression_cycles_and_fan_out_terminate() {
    let mut b = B::new();
    let mc = b.class("Material");
    let mul = b.class("MaterialExpressionMultiply");
    let m = b.export(mc, 0, "M");
    // Self-referencing multiply.
    let selfish = b.export(mul, m, "Self");
    let mut w = W::default();
    w.i32(0);
    b.input(&mut w, "A", selfish);
    b.input(&mut w, "B", selfish);
    end(&mut w);
    b.set(selfish, w.0);
    // A 40-level diamond chain: 2^40 paths without a work limit.
    let mut prev = selfish;
    for i in 0..40 {
        let e = b.export(mul, m, &format!("D{i}"));
        let mut w = W::default();
        w.i32(0);
        b.input(&mut w, "A", prev);
        b.input(&mut w, "B", prev);
        end(&mut w);
        b.set(e, w.0);
        prev = e;
    }
    let p = material_payload(&mut b, prev);
    b.set(m, p);
    let start = std::time::Instant::now();
    let statuses = approximate_all(b.package().unwrap());
    assert!(start.elapsed().as_secs() < 30, "graph walk must be bounded");
    assert_eq!(statuses[0], Some(ApproxStatus::Fallback));
}

#[test]
fn links_to_non_expressions_and_long_instance_chains() {
    let mut b = B::new();
    let mc = b.class("Material");
    let tc = b.class("Texture2D");
    let mic = b.class("MaterialInstanceConstant");
    let m = b.export(mc, 0, "M");
    let t = b.export(tc, 0, "T");
    let mut w = W::default();
    w.i32(0);
    end(&mut w);
    b.set(t, w.0);
    let p = material_payload(&mut b, t); // diffuse "expression" is a texture
    b.set(m, p);
    // 20 instances in a row; the chain limit is 16.
    let mut parent = m;
    for i in 0..20 {
        let e = b.export(mic, 0, &format!("MI{i}"));
        let mut w = W::default();
        w.i32(0);
        b.object(&mut w, "Parent", parent);
        end(&mut w);
        b.set(e, w.0);
        parent = e;
    }
    let statuses = approximate_all(b.package().unwrap());
    assert_eq!(statuses[0], Some(ApproxStatus::Fallback), "material");
    assert_eq!(statuses[1], None, "a texture is not a material");
    assert_eq!(
        statuses[2],
        Some(ApproxStatus::Fallback),
        "short chain to a broken base"
    );
    assert_eq!(statuses[21], Some(ApproxStatus::Fallback), "chain too long");
}

#[test]
fn mutated_material_payloads_never_panic() {
    let mut rng = Rng(0x1234_5678_9ABC_DEF1);
    let mut b = B::new();
    let mc = b.class("Material");
    let ts = b.class("MaterialExpressionTextureSample");
    let tx = b.class("Texture2D");
    let m = b.export(mc, 0, "M");
    let t = b.export(tx, 0, "T");
    let e = b.export(ts, m, "TS");
    let mut w = W::default();
    w.i32(0);
    end(&mut w);
    b.set(t, w.0);
    let mut w = W::default();
    w.i32(0);
    b.object(&mut w, "Texture", t);
    end(&mut w);
    b.set(e, w.0.clone());
    let base = material_payload(&mut b, e);
    b.set(m, base.clone());
    let expr_base = w.0;
    assert_eq!(
        approximate_all(b.package().unwrap())[0],
        Some(ApproxStatus::Approximated)
    );
    for _ in 0..1500 {
        let (target, src) = if rng.below(2) == 0 {
            (m, &base)
        } else {
            (e, &expr_base)
        };
        let mut p = src.clone();
        for _ in 0..1 + rng.below(4) {
            let i = rng.below(p.len());
            match rng.below(3) {
                0 => p[i] = rng.next() as u8,
                1 => p[i] ^= 1 << rng.below(8),
                _ => {
                    let end = (i + 4).min(p.len());
                    let v = (rng.next() as i32).to_le_bytes();
                    p[i..end].copy_from_slice(&v[..end - i]);
                }
            }
        }
        let saved = b.exports[(target - 1) as usize].payload.clone();
        b.set(target, p);
        if let Some(pkg) = b.package() {
            let _ = approximate_all(pkg);
        }
        b.set(target, saved);
    }
}

// ---------------------------------------------------------------------------
// Added by the verification pass
// ---------------------------------------------------------------------------

/// Class names come from the package's name table, which a hostile file can
/// fill with any (Latin-1) bytes: a multi-byte character straddling the
/// `MaterialExpression` prefix must not panic.
#[test]
fn expression_kind_never_splits_a_character() {
    let prefix = "MaterialExpression";
    for cut in 0..=prefix.len() {
        for c in ['\u{e9}', '\u{c3}', '\u{263a}', '\u{1f600}'] {
            let name = format!("{}{c}{}", &prefix[..cut], &prefix[cut..]);
            for path in [
                name.clone(),
                format!("Engine.{name}"),
                format!("{c}.{name}"),
            ] {
                let kind = asamu_ue3::material::expression_kind(&path);
                // Only an intact prefix is an expression class.
                assert_eq!(kind.is_some(), cut == prefix.len(), "{path}");
            }
        }
    }
    assert_eq!(
        asamu_ue3::material::expression_kind("Engine.materialexpression\u{e9}"),
        Some("\u{e9}")
    );
    assert_eq!(asamu_ue3::material::expression_kind(""), None);
}

/// The same through a package: an expression class whose name puts a
/// two-byte character across the prefix boundary is scanned and walked
/// without panicking (it is simply not an expression).
#[test]
fn hostile_expression_class_names_in_a_package() {
    let mut b = B::new();
    let mc = b.class("Material");
    // Written as UTF-8 by the test writer and read back as Latin-1: two
    // two-byte characters starting at byte 17.
    let odd = b.class("MaterialExpressio\u{e9}Multiply");
    let m = b.export(mc, 0, "M");
    let e = b.export(odd, m, "Odd");
    let mut w = W::default();
    w.i32(0);
    end(&mut w);
    b.set(e, w.0);
    let p = material_payload(&mut b, e);
    b.set(m, p);
    let pkg = b.package().unwrap();
    let set = PackageSet::new::<&str>(&[]);
    let lp = set.insert_package("Hostile", pkg);
    let dec = MaterialDecoder::new(&set);
    let cov = asamu_ue3::material::material_coverage(&dec, &lp);
    assert_eq!(cov.expression_exports.len(), 0);
    let a = dec.approximate(&lp, 0).unwrap();
    assert_eq!(a.status, ApproxStatus::Fallback);
    assert_eq!(a.unsupported.get("missing-expression"), Some(&1));
}

/// A minimal resource whose `CompileErrors` holds the raw string `s`.
fn resource_with_error(s: &[u8]) -> Vec<u8> {
    let mut w = W::default();
    w.u32(1); // quality mask
    w.i32(1); // one compile error
    w.bytes(s);
    w.i32(0); // TextureDependencyLengthMap
    w.i32(0); // MaxTextureDependencyLength
    for v in [1u32, 2, 3, 4] {
        w.u32(v);
    }
    w.u32(1); // NumUserTexCoords
    w.i32(0); // UniformExpressionTextures
    for _ in 0..5 {
        w.u32(0);
    }
    w.u32(0); // UsingTransforms
    w.i32(0); // TextureLookups
    w.u32(7);
    for v in [0u32, 0, 0] {
        w.u32(v);
    }
    w.0
}

fn utf16(units: &[u16]) -> Vec<u8> {
    let mut w = W::default();
    w.i32(-(units.len() as i32) - 1);
    for u in units {
        w.u16(*u);
    }
    w.u16(0);
    w.0
}

/// Encodings the build's string writer never produces are refused, so that
/// every accepted tail re-encodes to the same bytes.
#[test]
fn non_canonical_compile_error_strings_are_refused() {
    let k = NativeKind::Material;
    // UTF-16 for characters that fit in one byte (the writer would use
    // Latin-1): refused.
    let latin_in_utf16 = resource_with_error(&utf16(&[0x61, 0xE9]));
    assert!(decode_material_native(&latin_in_utf16, 0, k).is_err());
    // Empty strings stored with a terminator: refused.
    let mut w = W::default();
    w.i32(1);
    w.0.push(0);
    assert!(decode_material_native(&resource_with_error(&w.0), 0, k).is_err());
    assert!(decode_material_native(&resource_with_error(&utf16(&[])), 0, k).is_err());
    // Canonical forms decode and round-trip byte for byte.
    let wide = resource_with_error(&utf16(&[0x61, 0x263A]));
    let n = decode_material_native(&wide, 0, k).unwrap();
    assert_eq!(n.resources[0].resource.compile_errors, vec!["a\u{263a}"]);
    check_round_trip(&wide, k);
    let mut w = W::default();
    w.i32(3);
    w.bytes(&[b'a', 0xE9, 0]);
    let latin = resource_with_error(&w.0);
    let n = decode_material_native(&latin, 0, k).unwrap();
    assert_eq!(n.resources[0].resource.compile_errors, vec!["a\u{e9}"]);
    check_round_trip(&latin, k);
    let mut w = W::default();
    w.i32(0);
    check_round_trip(&resource_with_error(&w.0), k);
}

/// Random strings in the compile-error slot: whatever is accepted
/// re-encodes exactly.
#[test]
fn random_compile_error_strings_round_trip_when_accepted() {
    let mut rng = Rng(0x0BAD_5EED_1234_5678);
    let mut accepted = 0;
    for _ in 0..4000 {
        let len = rng.below(6) as i32 - 3;
        let mut w = W::default();
        w.i32(len);
        let body = if len >= 0 {
            len as usize
        } else {
            2 * (-len) as usize
        };
        for _ in 0..body {
            // Mostly small values so that terminators and one-byte
            // characters are common.
            let v = match rng.below(4) {
                0 => 0,
                1 => rng.below(0x100) as u8,
                _ => b'a' + rng.below(3) as u8,
            };
            w.0.push(v);
        }
        let data = resource_with_error(&w.0);
        if decode_material_native(&data, 0, NativeKind::Material).is_ok() {
            accepted += 1;
        }
        check_round_trip(&data, NativeKind::Material);
    }
    assert!(accepted > 0);
}
