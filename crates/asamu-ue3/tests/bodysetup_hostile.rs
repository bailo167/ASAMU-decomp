//! Hostile input for the `RB_BodySetup` decoder, over the synthetic payload
//! of `bodysetup_common` (no original game data): truncation at every
//! offset, trailing bytes, impossible counts and sizes at every count and
//! size field, wrong tag and struct types, unknown, repeated and misordered
//! members, bad booleans, bad names, extreme values at every offset and
//! deterministic random mutations. Nothing may panic, hang or allocate
//! beyond what the input holds, and **every input the decoder accepts
//! re-encodes to exactly the same bytes**.

#![allow(clippy::unwrap_used)]

mod bodysetup_common;

use asamu_ue3::bodysetup::{
    NameIndex, decode_body_setup_payload, encode_body_setup, validate_body_setup,
};
use asamu_ue3::object::ObjectError;
use bodysetup_common::{B, E, IDENTITY, class_default_object, fixture, n, names, tagged};

fn try_decode(bytes: &[u8]) -> Result<asamu_ue3::bodysetup::BodySetup, ObjectError> {
    decode_body_setup_payload(bytes, &names(), false)
}

/// Decode; when accepted, the value must re-encode to the input and the
/// cross-checks must run without panicking. Returns whether it was accepted.
fn accepted_inputs_round_trip(bytes: &[u8], index: &NameIndex) -> bool {
    match try_decode(bytes) {
        Ok(b) => {
            let again = encode_body_setup(&b, index).expect("an accepted body encodes");
            assert!(again == bytes, "accepted input does not re-encode exactly");
            let _ = validate_body_setup(&b);
            true
        }
        Err(e) => {
            // Errors render without panicking.
            let _ = e.to_string();
            false
        }
    }
}

fn put(bytes: &mut [u8], off: usize, v: i32) {
    bytes[off..off + 4].copy_from_slice(&v.to_le_bytes());
}

fn get(bytes: &[u8], off: usize) -> i32 {
    i32::from_le_bytes(bytes[off..off + 4].try_into().unwrap())
}

#[test]
fn the_fixture_itself_is_accepted() {
    let fx = fixture();
    assert!(accepted_inputs_round_trip(
        &fx.bytes,
        &NameIndex::new(&names())
    ));
    // Element counts: the scale array, the four shape arrays, the six
    // arrays of the convex element and five in the native data. Tags: nine
    // of the body, five of the aggregate, 4 + 6 + 5 + 7 of the shapes.
    assert_eq!((fx.counts.len(), fx.sizes.len()), (16, 36));
    assert_eq!(fx.tags.len(), 36);
}

#[test]
fn every_truncation_is_rejected() {
    let fx = fixture();
    for len in 0..fx.bytes.len() {
        assert!(
            try_decode(&fx.bytes[..len]).is_err(),
            "accepted a payload cut to {len} of {} bytes",
            fx.bytes.len()
        );
        // The same cut read as a class default object: it ends inside a tag
        // or before the terminator, or leaves native bytes behind.
        assert!(
            decode_body_setup_payload(&fx.bytes[..len], &names(), true).is_err() || {
                // The only acceptable cut is exactly the end of the tagged data.
                len == 4 + tagged().bytes.len()
            }
        );
    }
    let cdo = class_default_object();
    for len in 0..cdo.bytes.len() {
        assert!(decode_body_setup_payload(&cdo.bytes[..len], &names(), true).is_err());
    }
    assert!(try_decode(&[]).is_err());
}

#[test]
fn trailing_bytes_are_rejected() {
    let fx = fixture();
    for extra in [vec![0u8], vec![0, 0, 0, 0], vec![0xff; 9]] {
        let mut bytes = fx.bytes.clone();
        bytes.extend_from_slice(&extra);
        assert!(matches!(
            try_decode(&bytes),
            Err(ObjectError::Malformed {
                what: "RB_BodySetup native data",
                ..
            })
        ));
    }
}

#[test]
fn impossible_counts_are_rejected() {
    let fx = fixture();
    for &off in &fx.counts {
        let original = get(&fx.bytes, off);
        for bad in [
            -1,
            i32::MIN,
            i32::MAX,
            0x0100_0000,
            original + 1,
            original + 1000,
            original - 1,
        ] {
            if bad == original {
                continue;
            }
            let mut bytes = fx.bytes.clone();
            put(&mut bytes, off, bad);
            assert!(
                try_decode(&bytes).is_err(),
                "count at {off}: {original} -> {bad} was accepted"
            );
        }
    }
}

#[test]
fn impossible_tag_sizes_are_rejected() {
    let fx = fixture();
    for &off in &fx.sizes {
        let original = get(&fx.bytes, off);
        for bad in [
            -1,
            i32::MIN,
            i32::MAX,
            0,
            original + 1,
            original - 1,
            original + 4,
        ] {
            if bad == original {
                continue;
            }
            let mut bytes = fx.bytes.clone();
            put(&mut bytes, off, bad);
            assert!(
                try_decode(&bytes).is_err(),
                "tag size at {off}: {original} -> {bad} was accepted"
            );
        }
    }
}

/// Offset of the type name of the `k`-th tag called `name`.
fn type_at(fx: &B, name: &str, k: usize) -> usize {
    fx.tag_at(name, k) + 8
}

/// Offset of the struct name of the `k`-th struct tag called `name`.
fn struct_at(fx: &B, name: &str, k: usize) -> usize {
    fx.tag_at(name, k) + 24
}

#[test]
fn wrong_tag_types_are_rejected() {
    let fx = fixture();
    // (tag, occurrence, type written instead)
    let cases = [
        ("AggGeom", 0, "IntProperty"),
        ("AggGeom", 0, "ArrayProperty"),
        ("PreCachedPhysScale", 0, "IntProperty"),
        ("PreCachedPhysScale", 0, "StructProperty"),
        ("COMNudge", 0, "FloatProperty"),
        ("COMNudge", 0, "ArrayProperty"),
        ("SphereElems", 0, "StructProperty"),
        ("BoxElems", 0, "IntProperty"),
        ("SphylElems", 0, "FloatProperty"),
        ("ConvexElems", 0, "ObjectProperty"),
        ("TM", 0, "ArrayProperty"),
        ("TM", 1, "FloatProperty"),
        ("TM", 2, "IntProperty"),
        ("Radius", 0, "IntProperty"),
        ("Radius", 1, "ObjectProperty"),
        ("Length", 0, "IntProperty"),
        ("X", 0, "IntProperty"),
        ("Y", 0, "ArrayProperty"),
        ("Z", 0, "NameProperty"),
        ("bNoRBCollision", 0, "IntProperty"),
        ("bPerPolyShape", 2, "FloatProperty"),
        ("bSkipCloseAndParallelChecks", 0, "IntProperty"),
        ("VertexData", 0, "StructProperty"),
        ("VertexData", 0, "IntProperty"),
        ("PermutedVertexData", 0, "FloatProperty"),
        ("FaceTriData", 0, "IntProperty"),
        ("EdgeDirections", 0, "StructProperty"),
        ("FaceNormalDirections", 0, "ObjectProperty"),
        ("FacePlaneData", 0, "ByteProperty"),
        ("ElemBox", 0, "ArrayProperty"),
        ("ElemBox", 0, "IntProperty"),
        // A type this decoder does not read at all.
        ("MassScale", 0, "ArrayProperty"),
        ("MassScale", 0, "StructProperty"),
        ("BoneName", 0, "Padding"),
    ];
    for (tag, k, ty) in cases {
        let mut bytes = fx.bytes.clone();
        put(&mut bytes, type_at(&fx, tag, k), n(ty));
        assert!(
            try_decode(&bytes).is_err(),
            "{tag} #{k} as {ty} was accepted"
        );
    }
    // Struct tags with another struct's name.
    for (tag, k, name) in [
        ("AggGeom", 0, "Box"),
        ("AggGeom", 0, "Matrix"),
        ("COMNudge", 0, "Matrix"),
        ("TM", 0, "Box"),
        ("TM", 1, "Vector"),
        ("TM", 2, "KAggregateGeom"),
        ("ElemBox", 0, "Matrix"),
        ("ElemBox", 0, "Vector"),
    ] {
        let mut bytes = fx.bytes.clone();
        put(&mut bytes, struct_at(&fx, tag, k), n(name));
        assert!(
            try_decode(&bytes).is_err(),
            "{tag} #{k} as struct {name} was accepted"
        );
    }
    // A type name with an instance number is another name.
    let mut bytes = fx.bytes.clone();
    put(&mut bytes, type_at(&fx, "MassScale", 0) + 4, 1);
    assert!(try_decode(&bytes).is_err());
}

#[test]
fn unknown_repeated_and_misordered_members_are_rejected() {
    let fx = fixture();
    // (tag, occurrence, name written instead)
    let cases = [
        // Not members of the struct they are in.
        ("Radius", 0, "Length"),
        ("Radius", 0, "MassScale"),
        ("X", 0, "Radius"),
        ("Length", 0, "X"),
        ("VertexData", 0, "TM"),
        ("ElemBox", 0, "Box"),
        ("SphereElems", 0, "VertexData"),
        ("bSkipCloseAndParallelChecks", 0, "bNoRBCollision"),
        // Repeated members.
        ("PermutedVertexData", 0, "VertexData"),
        ("Y", 0, "X"),
        ("BoxElems", 0, "SphereElems"),
        ("bPerPolyShape", 0, "bNoRBCollision"),
        // Out of declaration order.
        ("VertexData", 0, "EdgeDirections"),
        ("X", 0, "Z"),
        ("SphereElems", 0, "ConvexElems"),
        // Repeated tags of the body itself.
        ("MassScale", 0, "bNoCollision"),
        ("bNoCollision", 0, "BoneName"),
    ];
    for (tag, k, name) in cases {
        let mut bytes = fx.bytes.clone();
        put(&mut bytes, fx.tag_at(tag, k), n(name));
        assert!(
            try_decode(&bytes).is_err(),
            "{tag} #{k} renamed {name} was accepted"
        );
    }
    // A member name with an instance number is another name.
    let mut bytes = fx.bytes.clone();
    put(&mut bytes, fx.tag_at("Radius", 0) + 4, 1);
    assert!(try_decode(&bytes).is_err());

    // Two aggregates, two scale arrays, two nudges.
    for (name, ty, extra) in [
        ("AggGeom", "StructProperty", Some("KAggregateGeom")),
        ("PreCachedPhysScale", "ArrayProperty", None),
        ("COMNudge", "StructProperty", Some("Vector")),
    ] {
        let mut value = B::default();
        match name {
            "AggGeom" => {
                value.name("None");
            }
            "PreCachedPhysScale" => {
                value.count(0);
            }
            _ => {
                value.floats(&[0.0; 3]);
            }
        }
        let mut b = B::default();
        b.i32(0);
        for _ in 0..2 {
            let e = extra.map_or(E::No, E::Struct);
            b.tag(name, ty, e, &value);
        }
        b.name("None");
        b.count(0);
        let err = try_decode(&b.bytes).unwrap_err().to_string();
        assert!(err.contains("tagged twice"), "{name}: {err}");
        // Once is fine.
        let mut once = B::default();
        once.i32(0);
        once.tag(name, ty, extra.map_or(E::No, E::Struct), &value);
        once.name("None");
        once.count(0);
        assert!(try_decode(&once.bytes).is_ok(), "{name}");
    }

    // A sphere with its radius before its matrix.
    let mut s = B::default();
    s.count(1)
        .float_tag("Radius", 1.0)
        .tm_tag(&IDENTITY)
        .name("None");
    let mut g = B::default();
    g.tag("SphereElems", "ArrayProperty", E::No, &s);
    g.name("None");
    let mut b = B::default();
    b.i32(0);
    b.tag("AggGeom", "StructProperty", E::Struct("KAggregateGeom"), &g);
    b.name("None");
    b.count(0);
    let err = try_decode(&b.bytes).unwrap_err().to_string();
    assert!(err.contains("out of declaration order"), "{err}");
}

#[test]
fn bad_booleans_indices_and_names_are_rejected() {
    let fx = fixture();
    // The boolean byte follows the 24-byte tag header.
    for (tag, k) in [
        ("bNoCollision", 0),
        ("bNoRBCollision", 1),
        ("bSkipCloseAndParallelChecks", 0),
    ] {
        for v in [2u8, 0x80, 0xff] {
            let mut bytes = fx.bytes.clone();
            bytes[fx.tag_at(tag, k) + 24] = v;
            let err = try_decode(&bytes).unwrap_err().to_string();
            assert!(err.contains("not 0 or 1"), "{tag}: {err}");
        }
    }
    // Static array indices.
    for (tag, k) in [
        ("MassScale", 0),
        ("AggGeom", 0),
        ("VertexData", 0),
        ("TM", 1),
    ] {
        for v in [1, -1, i32::MAX] {
            let mut bytes = fx.bytes.clone();
            put(&mut bytes, fx.tag_at(tag, k) + 20, v);
            assert!(try_decode(&bytes).is_err(), "{tag} array index {v}");
        }
    }
    // Name indices outside the table and negative instance numbers, in tag
    // names, type names, struct names, enum names and name values.
    let table = names().len() as i32;
    let name_offsets = [
        fx.tag_at("MassScale", 0),
        fx.tag_at("MassScale", 0) + 8,
        fx.tag_at("AggGeom", 0) + 24,
        fx.tag_at("SleepFamily", 0) + 24,
        fx.tag_at("SleepFamily", 0) + 32,
        fx.tag_at("BoneName", 0) + 24,
        fx.tag_at("Radius", 0),
        fx.tag_at("TM", 0) + 24,
    ];
    for off in name_offsets {
        for bad in [table, -1, i32::MIN, i32::MAX] {
            let mut bytes = fx.bytes.clone();
            put(&mut bytes, off, bad);
            assert!(try_decode(&bytes).is_err(), "name index {bad} at {off}");
        }
        for bad in [-1, i32::MIN] {
            let mut bytes = fx.bytes.clone();
            put(&mut bytes, off + 4, bad);
            assert!(try_decode(&bytes).is_err(), "name number {bad} at {off}");
        }
    }
    // An empty name table resolves nothing.
    let empty: Vec<String> = Vec::new();
    assert!(decode_body_setup_payload(&fx.bytes, &empty, false).is_err());
}

#[test]
fn pre_cooked_data_headers_are_checked() {
    let tags = tagged();
    let body = |native: &dyn Fn(&mut B)| {
        let mut b = B::default();
        b.i32(0);
        b.append(&tags);
        native(&mut b);
        b.bytes
    };
    // The plain form decodes.
    let ok = body(&|b| {
        b.count(1).count(1).i32(1).count(2).u8(1).u8(2);
    });
    assert_eq!(try_decode(&ok).unwrap().cached_bytes(), 2);
    // Any element size other than one byte is refused.
    for size in [0, 2, 4, -1, i32::MAX] {
        let bad = body(&|b| {
            b.count(1).count(1).i32(size).count(2).u8(1).u8(2);
        });
        let err = try_decode(&bad).unwrap_err().to_string();
        assert!(err.contains("bulk element size"), "{size}: {err}");
    }
    // Counts beyond the remaining bytes are refused before anything is read.
    for (outer, inner, len) in [
        (i32::MAX, 0, 0),
        (1, i32::MAX, 0),
        (1, 1, i32::MAX),
        (-1, 0, 0),
        (1, -1, 0),
        (1, 1, -1),
        (2, 1, 0),
        (1, 2, 0),
        (1, 1, 1),
    ] {
        let bad = body(&|b| {
            b.count(outer).count(inner).i32(1).count(len);
        });
        assert!(try_decode(&bad).is_err(), "{outer} {inner} {len}");
    }
    // No native data at all.
    assert!(try_decode(&body(&|_| {})).is_err());
}

#[test]
fn huge_counts_do_not_allocate() {
    // A convex array that claims i32::MAX elements in a 12-byte value, and
    // the same for every other array kind: each is refused by the count
    // check, before any element is read or reserved.
    let array = |name: &str, count: i32| {
        let mut v = B::default();
        v.count(count).name("None");
        let mut g = B::default();
        g.tag(name, "ArrayProperty", E::No, &v);
        g.name("None");
        let mut b = B::default();
        b.i32(0);
        b.tag("AggGeom", "StructProperty", E::Struct("KAggregateGeom"), &g);
        b.name("None");
        b.count(0);
        b.bytes
    };
    for name in ["SphereElems", "BoxElems", "SphylElems", "ConvexElems"] {
        assert!(try_decode(&array(name, 1)).is_ok(), "{name}");
        for count in [2, 1000, i32::MAX, -1] {
            assert!(try_decode(&array(name, count)).is_err(), "{name} {count}");
        }
    }
    for name in [
        "VertexData",
        "PermutedVertexData",
        "FaceTriData",
        "EdgeDirections",
        "FaceNormalDirections",
        "FacePlaneData",
    ] {
        for count in [1, 1000, i32::MAX, -1] {
            let mut items = B::default();
            items.count(count);
            let mut e = B::default();
            e.tag(name, "ArrayProperty", E::No, &items);
            e.name("None");
            let mut v = B::default();
            v.count(1).append(&e);
            let mut g = B::default();
            g.tag("ConvexElems", "ArrayProperty", E::No, &v);
            g.name("None");
            let mut b = B::default();
            b.i32(0);
            b.tag("AggGeom", "StructProperty", E::Struct("KAggregateGeom"), &g);
            b.name("None");
            b.count(0);
            assert!(try_decode(&b.bytes).is_err(), "{name} {count}");
        }
    }
}

#[test]
fn extreme_values_at_every_offset_never_panic() {
    let fx = fixture();
    let index = NameIndex::new(&names());
    let mut accepted = 0usize;
    let mut tried = 0usize;
    for off in 0..fx.bytes.len().saturating_sub(3) {
        for v in [0, 1, -1, i32::MIN, i32::MAX, 0x0100_0000, 0x7fc0_0001] {
            let mut bytes = fx.bytes.clone();
            put(&mut bytes, off, v);
            tried += 1;
            if accepted_inputs_round_trip(&bytes, &index) {
                accepted += 1;
            }
        }
    }
    for off in 0..fx.bytes.len() {
        for bit in 0..8 {
            let mut bytes = fx.bytes.clone();
            bytes[off] ^= 1 << bit;
            tried += 1;
            if accepted_inputs_round_trip(&bytes, &index) {
                accepted += 1;
            }
        }
    }
    // Float, index and blob bytes may change freely; structure may not.
    assert!(accepted > 0 && accepted < tried, "{accepted} of {tried}");
}

/// Deterministic generator (64-bit LCG, high bits).
struct Lcg(u64);

impl Lcg {
    fn next(&mut self) -> u32 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        (self.0 >> 33) as u32
    }
    fn below(&mut self, n: usize) -> usize {
        self.next() as usize % n
    }
}

#[test]
fn random_mutations_never_panic_and_accepted_inputs_round_trip() {
    let fx = fixture();
    let index = NameIndex::new(&names());
    let mut rng = Lcg(0x5eed_b0d7_5e70_0001);
    let mut accepted = 0usize;
    for _ in 0..20_000 {
        let mut bytes = fx.bytes.clone();
        for _ in 0..1 + rng.below(4) {
            let off = rng.below(bytes.len());
            match rng.below(4) {
                0 => bytes[off] = rng.next() as u8,
                1 => bytes[off] = bytes[off].wrapping_add(1),
                2 => bytes[off] = 0,
                _ => bytes[off] = 0xff,
            }
        }
        match rng.below(8) {
            0 => {
                let keep = rng.below(bytes.len() + 1);
                bytes.truncate(keep);
            }
            1 => bytes.push(rng.next() as u8),
            _ => {}
        }
        if accepted_inputs_round_trip(&bytes, &index) {
            accepted += 1;
        }
        // As a class default object as well.
        if let Ok(b) = decode_body_setup_payload(&bytes, &names(), true) {
            assert!(encode_body_setup(&b, &index).unwrap() == bytes);
        }
    }
    assert!(accepted > 0);
    // Pure noise of every small length.
    for len in 0..512 {
        let noise: Vec<u8> = (0..len).map(|_| rng.next() as u8).collect();
        accepted_inputs_round_trip(&noise, &index);
        let _ = decode_body_setup_payload(&noise, &names(), true);
    }
}

#[test]
fn many_scalar_tags_stay_linear_and_exact() {
    // 100,000 distinct scalar tags (`Padding_0` ...): accepted, kept in
    // order with their stored names, and re-encoded exactly. One repeat
    // anywhere is refused.
    const N: i32 = 100_000;
    let build = |repeat: Option<i32>| {
        let mut b = B::default();
        b.i32(0);
        for k in 0..N {
            let number = if repeat == Some(k) { 1 } else { k + 1 };
            b.name_num("Padding", number).name("IntProperty");
            b.i32(4).i32(0).i32(k);
        }
        b.name("None");
        b.count(0);
        b.bytes
    };
    let bytes = build(None);
    let body = try_decode(&bytes).unwrap();
    assert_eq!(body.properties.len(), N as usize);
    assert_eq!(body.scalars().last().unwrap().name, "Padding_99999");
    assert!(encode_body_setup(&body, &NameIndex::new(&names())).unwrap() == bytes);
    for k in [1, N / 2, N - 1] {
        let err = try_decode(&build(Some(k))).unwrap_err().to_string();
        assert!(err.contains("Padding_0 is tagged twice"), "{err}");
    }
}

#[test]
fn duplicate_names_decode_alike_and_encode_to_the_first_entry() {
    // The exact re-encode of an accepted input holds for a name table whose
    // names are distinct without regard to case, which is what a package's
    // name table is (the shipped packages have no such duplicate among
    // 77,358 names). Names compare without regard to case, so a table that
    // repeats one in another spelling gives two spellings of the same
    // payload: both decode to the same fields, and the encoder writes the
    // first entry (`NameIndex`), never an index it was not given.
    let fx = fixture();
    let mut table = names();
    let first_extra = i32::try_from(table.len()).unwrap();
    table.extend(["tm", "NONE", "structproperty", "MATRIX", "boolproperty"].map(str::to_owned));
    let index = NameIndex::new(&table);
    let canonical = decode_body_setup_payload(&fx.bytes, &table, false).unwrap();
    assert!(encode_body_setup(&canonical, &index).unwrap() == fx.bytes);

    // (field offset, name it holds, index of the other spelling)
    let tm = fx.tag_at("TM", 0);
    let flag = fx.tag_at("bNoRBCollision", 0);
    // The terminator of the body's tagged properties: `NetIndex`, then the
    // tagged stream, whose last eight bytes are the `None` name.
    let last_none = 4 + tagged().bytes.len() - 8;
    let patches = [
        (tm, "TM", first_extra),
        (tm + 8, "StructProperty", first_extra + 2),
        (tm + 24, "Matrix", first_extra + 3),
        (flag + 8, "BoolProperty", first_extra + 4),
        (last_none, "None", first_extra + 1),
    ];
    let mut all = fx.bytes.clone();
    for (off, name, other) in patches {
        assert_eq!(get(&fx.bytes, off), n(name), "{name} at {off}");
        let mut one = fx.bytes.clone();
        put(&mut one, off, other);
        put(&mut all, off, other);
        // With the plain table the index is out of range: refused.
        assert!(try_decode(&one).is_err(), "{name}");
        let b = decode_body_setup_payload(&one, &table, false).unwrap();
        assert!(
            b == canonical,
            "{name}: another spelling decodes differently"
        );
        assert!(encode_body_setup(&b, &index).unwrap() == fx.bytes, "{name}");
    }
    let b = decode_body_setup_payload(&all, &table, false).unwrap();
    assert!(b == canonical);
    assert!(encode_body_setup(&b, &index).unwrap() == fx.bytes);
    // An instance number makes it another name, in either spelling.
    for (off, name, other) in patches {
        for idx in [n(name), other] {
            let mut one = fx.bytes.clone();
            put(&mut one, off, idx);
            put(&mut one, off + 4, 1);
            assert!(
                decode_body_setup_payload(&one, &table, false).is_err(),
                "{name}_0 accepted"
            );
        }
    }
}
