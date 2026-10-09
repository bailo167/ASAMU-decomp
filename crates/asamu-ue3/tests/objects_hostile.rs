//! Hostile-input tests for the object decoders: truncation, bit flips,
//! extreme values, reference cycles, deep nesting and random tag streams.
//! Nothing may panic or hang; malformed script objects must be rejected.

#![allow(clippy::unwrap_used)]

mod common;
mod objects_common;

use asamu_ue3::object::decode_object;
use asamu_ue3::property::{ValueContext, read_tagged};
use asamu_ue3::reader::Reader;
use asamu_ue3::schema::NoSchema;
use asamu_ue3::script::{ScriptKind, decode_script_object};
use asamu_ue3::{Package, PackageSet, Value};
use objects_common::{P, build_with, e, ex, payloads};

/// Decode everything a consumer might touch; must never panic.
fn exercise(pkg: Package) {
    let set = PackageSet::new::<&str>(&[]);
    let lp = set.insert_package("TestPkg", pkg);
    for i in 0..lp.package.exports.len() {
        let _ = decode_script_object(&lp.package, Some("TestPkg"), i, &NoSchema);
        let _ = decode_object(&lp.package, Some("TestPkg"), i, &NoSchema);
        let _ = set.decode(&lp, i);
    }
    let _ = set.class_model("TestPkg.Derived");
    let _ = set.class_model("TestPkg.Base");
    let _ = set.inherited_defaults("TestPkg.Derived");
    let _ = asamu_ue3::coverage::package_coverage(&set, &lp, true);
}

fn with_payload(i: usize, payload: Vec<u8>) -> Option<Package> {
    let mut all = payloads();
    all[i] = payload;
    Package::from_bytes(build_with(&all).bytes).ok()
}

#[test]
fn truncated_script_payloads_are_rejected() {
    let base = payloads();
    let reference = Package::from_bytes(build_with(&base).bytes).unwrap();
    for (i, full) in base.iter().enumerate() {
        let script = ScriptKind::of_export(&reference, i).is_some();
        for len in 0..full.len() {
            let Some(pkg) = with_payload(i, full[..len].to_vec()) else {
                continue;
            };
            if script {
                assert!(
                    decode_script_object(&pkg, Some("TestPkg"), i, &NoSchema).is_err(),
                    "export {i} truncated to {len} decoded"
                );
            }
            exercise(pkg);
        }
    }
}

#[test]
fn truncated_tagged_streams_are_rejected() {
    // Every export whose tagged stream ends at its payload end must fail when cut.
    for i in [
        ex::DEFAULT_BASE,
        ex::DEFAULT_DERIVED,
        ex::COMP_TEMPLATE,
        ex::COMP_INSTANCE,
    ] {
        let full = payloads()[i].clone();
        for len in 0..full.len() {
            let pkg = with_payload(i, full[..len].to_vec()).unwrap();
            let set = PackageSet::new::<&str>(&[]);
            let lp = set.insert_package("TestPkg", pkg);
            assert!(
                set.decode(&lp, i).is_err(),
                "export {i} cut to {len} decoded"
            );
        }
    }
}

#[test]
fn bit_flips_never_panic() {
    let base = payloads();
    for (i, full) in base.iter().enumerate() {
        for pos in 0..full.len() {
            for bit in [0u8, 3, 7] {
                let mut m = full.clone();
                m[pos] ^= 1 << bit;
                if let Some(pkg) = with_payload(i, m) {
                    exercise(pkg);
                }
            }
        }
    }
}

#[test]
fn extreme_values_never_panic() {
    let base = payloads();
    for (i, full) in base.iter().enumerate() {
        for pos in (0..full.len().saturating_sub(3)).step_by(2) {
            for v in [i32::MIN, -1, i32::MAX, 0x7FFF_FFF0, 0x0001_0000] {
                let mut m = full.clone();
                m[pos..pos + 4].copy_from_slice(&v.to_le_bytes());
                if let Some(pkg) = with_payload(i, m) {
                    exercise(pkg);
                }
            }
        }
    }
}

#[test]
fn children_and_super_cycles_terminate() {
    // MaxThing.Next -> Health closes the class's children chain into a loop.
    let mut all = payloads();
    let mut p = P::default();
    p.i32(19).none().i32(e(ex::HEALTH)).fstring("42");
    all[ex::MAX_THING] = p.bytes();
    let pkg = Package::from_bytes(build_with(&all).bytes).unwrap();
    let set = PackageSet::new::<&str>(&[]);
    set.insert_package("TestPkg", pkg);
    assert!(set.class_model("TestPkg.Derived").is_err());
    exercise(Package::from_bytes(build_with(&all).bytes).unwrap());

    // Base.SuperStruct -> Derived makes the super chain a loop.
    let mut all = payloads();
    let mut b = all[ex::BASE].clone();
    b[8..12].copy_from_slice(&e(ex::DERIVED).to_le_bytes());
    all[ex::BASE] = b;
    let pkg = Package::from_bytes(build_with(&all).bytes).unwrap();
    let set = PackageSet::new::<&str>(&[]);
    set.insert_package("TestPkg", pkg);
    let chain = set.super_chain("TestPkg.Derived");
    assert!(chain.len() <= 2, "{chain:?}");
    let _ = set.inherited_defaults("TestPkg.Derived");
}

/// A struct tag nested `depth` levels deep (unknown struct, so the decoder
/// probes it as a tagged stream).
fn nested_struct_tag(depth: usize) -> Vec<u8> {
    let mut inner = P::default();
    inner.none();
    let mut value = inner.bytes();
    for _ in 0..depth {
        let mut t = P::default();
        t.tag("Where", "StructProperty", value.len() as i32, 0)
            .name("Partner");
        let mut bytes = t.bytes();
        bytes.extend_from_slice(&value);
        let mut end = P::default();
        end.none();
        bytes.extend_from_slice(&end.bytes());
        value = bytes;
    }
    value
}

#[test]
fn deep_nesting_is_bounded() {
    let pkg = Package::from_bytes(build_with(&payloads()).bytes).unwrap();
    for depth in [1usize, 31, 40, 200] {
        let stream = nested_struct_tag(depth);
        let mut r = Reader::new(&stream);
        let mut ctx = ValueContext::new(&pkg, None, &NoSchema);
        let props = read_tagged(&mut r, &mut ctx, None, 0).unwrap();
        assert_eq!(r.remaining(), 0);
        assert_eq!(props.len(), 1);
        // Too-deep values are kept raw (with a warning) instead of recursing.
        if depth > 40 {
            assert!(props[0].value.has_raw());
            assert!(!ctx.warnings().is_empty());
        }
    }
}

#[test]
fn hostile_tags() {
    let pkg = Package::from_bytes(build_with(&payloads()).bytes).unwrap();
    let run = |bytes: &[u8]| {
        let mut r = Reader::new(bytes);
        let mut ctx = ValueContext::new(&pkg, None, &NoSchema);
        read_tagged(&mut r, &mut ctx, Some("TestPkg.Derived"), 0)
    };
    // Negative size, size past the end, negative array index, bad name index.
    let mut p = P::default();
    p.tag("Health", "IntProperty", -4, 0).i32(1).none();
    assert!(run(&p.bytes()).is_err());
    let mut p = P::default();
    p.tag("Health", "IntProperty", 400, 0).i32(1).none();
    assert!(run(&p.bytes()).is_err());
    let mut p = P::default();
    p.tag("Health", "IntProperty", 4, -1).i32(1).none();
    assert!(run(&p.bytes()).is_err());
    let mut p = P::default();
    p.i32(99_999).i32(0);
    assert!(run(&p.bytes()).is_err());
    // Missing terminator.
    let mut p = P::default();
    p.tag("Health", "IntProperty", 4, 0).i32(1);
    assert!(run(&p.bytes()).is_err());
    // Wrong value size for the type: kept raw, stream continues.
    let mut p = P::default();
    p.tag("Health", "IntProperty", 2, 0).u16(1).none();
    let props = run(&p.bytes()).unwrap();
    assert!(matches!(props[0].value, Value::Raw { bytes: 2, .. }));
    // Array with a huge element count: rejected inside the value, kept raw.
    let mut p = P::default();
    p.tag("Points", "ArrayProperty", 8, 0)
        .i32(i32::MAX)
        .i32(0)
        .none();
    let props = run(&p.bytes()).unwrap();
    assert!(props[0].value.has_raw());
    // Unknown property type: kept raw.
    let mut p = P::default();
    p.tag("Health", "Partner", 4, 0).i32(1).none();
    let props = run(&p.bytes()).unwrap();
    assert!(props[0].value.has_raw());
}

#[test]
fn random_tag_streams_never_panic() {
    let pkg = Package::from_bytes(build_with(&payloads()).bytes).unwrap();
    let set = PackageSet::new::<&str>(&[]);
    set.insert_package("TestPkg", pkg.clone());
    let mut seed = 0x1234_5678_9ABC_DEF0u64;
    let mut next = || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    let names = objects_common::NAMES.len() as u64;
    for _ in 0..3000 {
        let len = (next() % 96) as usize;
        let mut bytes = Vec::with_capacity(len);
        while bytes.len() < len {
            // Bias towards valid name indices and small sizes.
            let v = match next() % 4 {
                0 => (next() % names) as i32,
                1 => (next() % 16) as i32,
                2 => 0,
                _ => next() as i32,
            };
            bytes.extend_from_slice(&v.to_le_bytes());
        }
        let mut r = Reader::new(&bytes);
        let mut ctx = ValueContext::new(&pkg, None, &set);
        let _ = read_tagged(&mut r, &mut ctx, Some("TestPkg.Derived"), 0);
    }
}

/// A hand-written schema of binary structs, for fan-out attacks that a real
/// package schema could also express (`ArrayDim` and struct nesting come from
/// the package being decoded).
struct FanoutSchema {
    structs: std::collections::HashMap<String, std::sync::Arc<asamu_ue3::StructDef>>,
}

impl FanoutSchema {
    /// `Test.Owner` declares `Where` of struct `Test.L0`; `Test.L{i}` has one
    /// member of struct `Test.L{i+1}` with `ArrayDim` `dim`, and the last
    /// level is an empty binary struct (zero bytes per value).
    fn new(levels: usize, dim: i32) -> FanoutSchema {
        use asamu_ue3::schema::StructKind;
        use asamu_ue3::{PropertyDef, PropertyType, StructDef};
        use std::sync::Arc;
        let def = |name: &str, ty: PropertyType, array_dim: i32| {
            Arc::new(PropertyDef {
                name: name.to_owned(),
                path: format!("Test.{name}"),
                array_dim,
                flags: 0,
                category: "None".to_owned(),
                array_enum: None,
                rep_offset: None,
                ty,
            })
        };
        let mut structs = std::collections::HashMap::new();
        let owner = StructDef {
            path: "Test.Owner".to_owned(),
            name: "Owner".to_owned(),
            kind: StructKind::Class,
            super_path: None,
            struct_flags: 0,
            properties: vec![def(
                "Where",
                PropertyType::Struct {
                    struct_path: "Test.L0".to_owned(),
                },
                1,
            )],
        };
        structs.insert("test.owner".to_owned(), Arc::new(owner));
        for i in 0..=levels {
            let properties = if i < levels {
                vec![def(
                    "Inner",
                    PropertyType::Struct {
                        struct_path: format!("Test.L{}", i + 1),
                    },
                    dim,
                )]
            } else {
                Vec::new()
            };
            let s = StructDef {
                path: format!("Test.L{i}"),
                name: format!("L{i}"),
                kind: StructKind::ScriptStruct,
                super_path: None,
                struct_flags: 0x30, // Atomic | Immutable: binary
                properties,
            };
            structs.insert(format!("test.l{i}"), Arc::new(s));
        }
        FanoutSchema { structs }
    }
}

impl asamu_ue3::Schema for FanoutSchema {
    fn struct_def(&self, path: &str) -> Option<std::sync::Arc<asamu_ue3::StructDef>> {
        self.structs.get(&path.to_ascii_lowercase()).cloned()
    }
    fn struct_by_name(&self, name: &str) -> Option<std::sync::Arc<asamu_ue3::StructDef>> {
        self.struct_def(&format!("Test.{name}"))
    }
    fn find_property(
        &self,
        owner: &str,
        name: &str,
    ) -> Option<std::sync::Arc<asamu_ue3::PropertyDef>> {
        self.struct_def(owner)?
            .properties
            .iter()
            .find(|p| p.name.eq_ignore_ascii_case(name))
            .cloned()
    }
    fn property_link(&self, owner: &str) -> Vec<std::sync::Arc<asamu_ue3::PropertyDef>> {
        self.struct_def(owner)
            .map(|s| s.properties.clone())
            .unwrap_or_default()
    }
    fn enum_names(&self, _path: &str) -> Option<std::sync::Arc<Vec<String>>> {
        None
    }
    fn class_chain(&self, _class_path: &str) -> Vec<String> {
        Vec::new()
    }
}

/// `Where` (struct `L0`) with an empty value: the schema decides how much
/// work decoding those zero bytes takes.
fn zero_byte_struct_tag() -> Vec<u8> {
    let mut p = P::default();
    p.tag("Where", "StructProperty", 0, 0).name("MyVec").none();
    p.bytes()
}

#[test]
fn zero_sized_members_with_huge_array_dim_terminate() {
    // L0 { L1 Inner[i32::MAX] }, L1 empty: 2^31 zero-byte members.
    let pkg = Package::from_bytes(build_with(&payloads()).bytes).unwrap();
    let schema = FanoutSchema::new(1, i32::MAX);
    let stream = zero_byte_struct_tag();
    let start = std::time::Instant::now();
    let mut r = Reader::new(&stream);
    let mut ctx = ValueContext::new(&pkg, None, &schema);
    ctx.set_work_budget(asamu_ue3::property::work_budget_for(stream.len()));
    let res = read_tagged(&mut r, &mut ctx, Some("Test.Owner"), 0);
    assert!(res.is_err(), "fan-out must exhaust the work budget");
    assert!(ctx.budget_exhausted());
    assert!(start.elapsed() < std::time::Duration::from_secs(10));
}

#[test]
fn exponential_struct_fanout_terminates() {
    // 30 levels of 16-way fan-out: 16^30 leaves without a budget.
    let pkg = Package::from_bytes(build_with(&payloads()).bytes).unwrap();
    let schema = FanoutSchema::new(30, 16);
    let stream = zero_byte_struct_tag();
    let start = std::time::Instant::now();
    // Default budget (no payload size known).
    let mut r = Reader::new(&stream);
    let mut ctx = ValueContext::new(&pkg, None, &schema);
    assert!(read_tagged(&mut r, &mut ctx, Some("Test.Owner"), 0).is_err());
    assert!(ctx.budget_exhausted());
    // Small explicit budget: fails fast, and stays failed.
    let mut r = Reader::new(&stream);
    let mut ctx = ValueContext::new(&pkg, None, &schema);
    ctx.set_work_budget(1000);
    assert!(read_tagged(&mut r, &mut ctx, Some("Test.Owner"), 0).is_err());
    assert_eq!(ctx.remaining_work(), 0);
    assert!(start.elapsed() < std::time::Duration::from_secs(20));
}

#[test]
fn modest_fanout_within_budget_decodes() {
    // 3 levels of 4-way fan-out over zero-byte leaves: 4 + 16 + 64 members.
    let pkg = Package::from_bytes(build_with(&payloads()).bytes).unwrap();
    let schema = FanoutSchema::new(3, 4);
    let stream = zero_byte_struct_tag();
    let mut r = Reader::new(&stream);
    let mut ctx = ValueContext::new(&pkg, None, &schema);
    ctx.set_work_budget(asamu_ue3::property::work_budget_for(stream.len()));
    let props = read_tagged(&mut r, &mut ctx, Some("Test.Owner"), 0).unwrap();
    assert_eq!(r.remaining(), 0);
    let Value::Struct { binary, fields, .. } = &props[0].value else {
        panic!("{:?}", props[0].value);
    };
    assert!(binary);
    assert_eq!(fields.len(), 4);
    assert!(!ctx.budget_exhausted());
}
