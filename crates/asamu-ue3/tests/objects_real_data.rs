//! Object decoding against the user's own installed game (read-only).
//!
//! Reads `ASAMU_ORIGINAL_DIR` (the folder containing
//! `A Story About My Uncle.app`) or the default macOS Steam location and
//! skips cleanly when the data is absent. Only counts, names and structural
//! facts are asserted; nothing is copied or written.
//!
//! This is the acceptance test for `docs/reverse-engineering/OBJECT_FORMAT.md`:
//! every script object in every package consumes exactly `SerialSize` bytes,
//! every class default object decodes exactly, and every other export's
//! prelude and tagged properties decode.

#![allow(clippy::unwrap_used)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use asamu_ue3::coverage::{ObjectStats, package_coverage};
use asamu_ue3::model::{LoadedPackage, PackageSet};
use asamu_ue3::property::{NATIVE_LAYOUTS, NativeMember};
use asamu_ue3::schema::{PropertyType, Schema};
use asamu_ue3::script::ScriptKind;
use asamu_ue3::{Package, Value, flags};

const COOKED: &str = "A Story About My Uncle.app/Contents/Resources/ASAMU/CookedMac";

fn cooked_dir() -> Option<PathBuf> {
    let root = match std::env::var_os("ASAMU_ORIGINAL_DIR") {
        Some(r) => PathBuf::from(r),
        None => {
            let home = std::env::var_os("HOME")?;
            PathBuf::from(home)
                .join("Library/Application Support/Steam/steamapps/common/A Story About My Uncle")
        }
    };
    let dir = root.join(COOKED);
    dir.is_dir().then_some(dir)
}

macro_rules! require_data {
    () => {
        match cooked_dir() {
            Some(d) => d,
            None => {
                eprintln!(
                    "SKIP: original game data not found (set ASAMU_ORIGINAL_DIR to the folder \
                     containing 'A Story About My Uncle.app')"
                );
                return;
            }
        }
    };
}

fn packages(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for d in [dir.to_path_buf(), dir.join("Maps")] {
        let Ok(rd) = std::fs::read_dir(&d) else {
            continue;
        };
        let mut v: Vec<PathBuf> = rd
            .flatten()
            .map(|e| e.path())
            .filter(|p| {
                p.is_file()
                    && p.extension()
                        .map(|x| x.to_string_lossy().to_ascii_lowercase())
                        .is_some_and(|x| matches!(x.as_str(), "u" | "upk" | "asamu"))
            })
            .collect();
        v.sort();
        out.extend(v);
    }
    out
}

fn set_for(dir: &Path) -> PackageSet {
    PackageSet::new(&[dir.to_path_buf(), dir.join("Maps")])
}

#[test]
fn every_script_object_cdo_and_object_decodes() {
    let dir = require_data!();
    let files = packages(&dir);
    assert_eq!(files.len(), 42);
    let set = set_for(&dir);
    let mut kinds: BTreeMap<ScriptKind, (usize, usize)> = BTreeMap::new();
    let mut cdo = ObjectStats::default();
    let mut objects = ObjectStats::default();
    let mut unresolved = Vec::new();
    for f in &files {
        let name = f.file_name().unwrap().to_string_lossy().into_owned();
        let lower = name.to_ascii_lowercase();
        let cov = if lower.ends_with(".u") || lower == "startup.upk" {
            let lp = set.open_file(f).unwrap();
            package_coverage(&set, &lp, true)
        } else {
            let stem = f.file_stem().unwrap().to_string_lossy().into_owned();
            let lp = LoadedPackage::new(stem, f, Package::open(f).unwrap());
            package_coverage(&set, &lp, true)
        };
        for (k, c) in &cov.script {
            assert_eq!(c.exact, c.total, "{name} {}: {:?}", k.name(), c.failures);
            let e = kinds.entry(*k).or_default();
            e.0 += c.exact;
            e.1 += c.total;
        }
        // Maps define no script objects.
        if lower.ends_with(".asamu") {
            assert!(cov.script.is_empty(), "{name}");
        }
        if lower == "startup.upk" {
            assert_eq!(
                cov.cdo.total, 583,
                "asamu (172) + UTGame (411) class default objects"
            );
        }
        cdo.absorb(&cov.cdo);
        let o = cov.objects.unwrap();
        for c in &o.unknown_classes {
            if !unresolved.contains(c) {
                unresolved.push(c.clone());
            }
        }
        objects.absorb(&o);
    }

    let expected: &[(ScriptKind, usize)] = &[
        (ScriptKind::Class, 2521),
        (ScriptKind::State, 211),
        (ScriptKind::Function, 12511),
        (ScriptKind::ScriptStruct, 848),
        (ScriptKind::Enum, 379),
        (ScriptKind::Const, 614),
        (ScriptKind::TextBuffer, 2521),
        (ScriptKind::ByteProperty, 1985),
        (ScriptKind::IntProperty, 5896),
        (ScriptKind::FloatProperty, 7009),
        (ScriptKind::BoolProperty, 8349),
        (ScriptKind::StrProperty, 4745),
        (ScriptKind::NameProperty, 1674),
        (ScriptKind::ObjectProperty, 9750),
        (ScriptKind::ClassProperty, 690),
        (ScriptKind::ComponentProperty, 630),
        (ScriptKind::InterfaceProperty, 113),
        (ScriptKind::StructProperty, 7349),
        (ScriptKind::ArrayProperty, 2339),
        (ScriptKind::MapProperty, 45),
        (ScriptKind::DelegateProperty, 767),
    ];
    for &(k, n) in expected {
        assert_eq!(kinds.get(&k).copied(), Some((n, n)), "{}", k.name());
    }
    let total: usize = kinds.values().map(|v| v.1).sum();
    assert_eq!(total, 70_946);

    // Class default objects: every one decodes exactly, fully typed, in
    // property-link order, with only declared properties.
    assert_eq!(cdo.total, 2521);
    assert_eq!(cdo.exact, 2521, "{:?}", cdo.failures);
    assert_eq!(cdo.with_raw_values, 0);
    assert_eq!(cdo.with_warnings, 0, "{:?}", cdo.warning_samples);
    assert_eq!(cdo.order_violations, 0);
    assert_eq!(cdo.undeclared_tags, 0);
    assert_eq!(cdo.unknown_class, 0);

    // Every other non-script export: prelude + tagged properties decode.
    assert_eq!(objects.total, 129_218);
    assert_eq!(objects.decoded, objects.total, "{:?}", objects.failures);
    assert_eq!(objects.exact + objects.native_tail, objects.total);
    assert_eq!(objects.with_raw_values, 0);
    assert_eq!(objects.with_warnings, 0, "{:?}", objects.warning_samples);
    assert_eq!(objects.order_violations, 0);
    assert_eq!(objects.undeclared_tags, 0);
    unresolved.sort();
    assert_eq!(
        unresolved,
        vec![
            "Engine.Level",
            "Engine.LightMapTexture2D",
            "Engine.StaticMesh"
        ],
        "only native-only classes lack a script definition"
    );

    // Class and component properties are tagged as ObjectProperty.
    for t in [&cdo.tag_types, &objects.tag_types] {
        for bad in ["ClassProperty", "ComponentProperty", "MapProperty"] {
            assert!(!t.contains_key(bad), "{bad} tag found");
        }
    }
}

#[test]
fn binary_struct_layouts() {
    let dir = require_data!();
    let set = set_for(&dir);
    // The built-in fallback layouts equal the schema's serialization order.
    for (name, layout) in NATIVE_LAYOUTS {
        let path = format!("Core.Object.{name}");
        let def = set.struct_def(&path).unwrap();
        assert!(def.is_binary(true), "{name}");
        let link = set.property_link(&path);
        assert_eq!(link.len(), layout.len(), "{name}");
        for (d, (member, kind)) in link.iter().zip(layout.iter()) {
            assert_eq!(&d.name, member, "{name}");
            let ok = match (kind, &d.ty) {
                (NativeMember::F32, PropertyType::Float) => true,
                (NativeMember::I32, PropertyType::Int) => true,
                (NativeMember::U8, PropertyType::Byte { enum_path: None }) => true,
                (NativeMember::Vector, PropertyType::Struct { struct_path }) => {
                    struct_path == "Core.Object.Vector"
                }
                (NativeMember::Plane, PropertyType::Struct { struct_path }) => {
                    struct_path == "Core.Object.Plane"
                }
                _ => false,
            };
            assert!(ok, "{name}.{}: {:?}", d.name, d.ty);
        }
    }

    // Only one binary struct declares a native member, and no package stores
    // a value of it, so whether binary serialization skips CPF_Native members
    // stays UNKNOWN (transient members are included: see CoverSlot).
    let mut binary = 0;
    let mut with_native = Vec::new();
    for f in packages(&dir) {
        let lower = f
            .file_name()
            .unwrap()
            .to_string_lossy()
            .to_ascii_lowercase();
        if !(lower.ends_with(".u") || lower == "startup.upk") {
            continue;
        }
        let lp = set.open_file(&f).unwrap();
        for i in 0..lp.package.exports.len() {
            if ScriptKind::of_export(&lp.package, i) != Some(ScriptKind::ScriptStruct) {
                continue;
            }
            let def = set.struct_def(&lp.qualified(i).unwrap()).unwrap();
            if def.is_binary(true) {
                binary += 1;
                if set
                    .property_link(&def.path)
                    .iter()
                    .any(|p| p.flags & flags::property::NATIVE != 0)
                {
                    with_native.push(def.path.clone());
                }
            }
        }
    }
    assert_eq!(
        binary, 31,
        "14 Immutable + 1 native Immutable + 16 ImmutableWhenCooked"
    );
    assert_eq!(with_native, vec!["Engine.Pylon.PolyReference"]);
    for f in packages(&dir) {
        let p = Package::open(&f).unwrap();
        let uses = p
            .names
            .iter()
            .any(|n| n.name.eq_ignore_ascii_case("PolyReference"));
        let name = f.file_name().unwrap().to_string_lossy().into_owned();
        assert_eq!(uses, name == "Engine.u", "{name}");
    }

    // An identity matrix: each plane stores W before X, Y, Z (own members first).
    let d = set.class_defaults("Engine.EdCoordSystem").unwrap();
    let m = d.properties.iter().find(|p| p.name == "M").unwrap();
    let Value::Struct { fields, binary, .. } = &m.value else {
        panic!()
    };
    assert!(*binary);
    let plane = |i: usize| -> Vec<(String, f32)> {
        let Value::Struct { fields: f, .. } = &fields[i].value else {
            panic!()
        };
        f.iter()
            .map(|x| match x.value {
                Value::Float(v) => (x.name.clone(), v),
                _ => panic!(),
            })
            .collect()
    };
    let get = |i: usize, n: &str| plane(i).into_iter().find(|(k, _)| k == n).unwrap().1;
    assert_eq!(plane(0)[0].0, "W");
    assert_eq!(
        (get(0, "X"), get(1, "Y"), get(2, "Z"), get(3, "W")),
        (1.0, 1.0, 1.0, 1.0)
    );
    assert_eq!((get(0, "W"), get(0, "Y"), get(3, "X")), (0.0, 0.0, 0.0));
}

#[test]
fn gameplay_class_models_and_defaults() {
    let dir = require_data!();
    let set = set_for(&dir);

    let pawn = set.class_model("asamu.ASAMUPawn").unwrap();
    assert_eq!(
        pawn.super_chain,
        vec![
            "UTGame.UTPawn",
            "UDKBase.UDKPawn",
            "GameFramework.GamePawn",
            "Engine.Pawn",
            "Engine.Actor",
            "Core.Object"
        ]
    );
    assert_eq!(
        pawn.default_object.as_deref(),
        Some("asamu.Default__ASAMUPawn")
    );
    assert!(pawn.script_text.is_some());
    assert!(pawn.functions.iter().any(|f| f.name == "DoJump"));

    let pc = set.class_model("asamu.ASAMUPlayerController").unwrap();
    assert_eq!(
        pc.super_chain.first().map(String::as_str),
        Some("UDKBase.UDKPlayerController")
    );
    let states: Vec<&str> = pc.states.iter().map(|s| s.name.as_str()).collect();
    for s in ["Grappling", "ReleaseGrapple", "PlayerFlying"] {
        assert!(states.contains(&s), "{states:?}");
    }

    let gun = set.class_model("asamu.GrappleGun").unwrap();
    assert_eq!(
        gun.super_chain.first().map(String::as_str),
        Some("UDKBase.UDKWeapon")
    );

    // Native operator: parameters in declaration order, flags and index.
    let object = set.class_model("Core.Object").unwrap();
    let not = object
        .functions
        .iter()
        .find(|f| f.name == "Not_PreBool")
        .unwrap();
    assert_eq!(not.native_index, 129);
    assert_eq!(not.return_type.as_deref(), Some("bool"));
    assert_eq!(not.params.len(), 1);
    assert!(not.flag_names.contains(&"PreOperator".to_owned()));
    // Functions are reported in declaration order (operators come first in
    // Object's declarations; the children chain lists them last).
    let pos = |n: &str| object.functions.iter().position(|f| f.name == n).unwrap();
    assert!(pos("Not_PreBool") < pos("VSize"));

    // Inherited defaults resolve across four packages.
    let d = set.inherited_defaults("asamu.ASAMUPawn").unwrap();
    let sources: Vec<&str> = d.sources.iter().map(|s| s.class.as_str()).collect();
    assert_eq!(
        sources,
        vec![
            "Core.Object",
            "Engine.Actor",
            "Engine.Pawn",
            "GameFramework.GamePawn",
            "UDKBase.UDKPawn",
            "UTGame.UTPawn",
            "asamu.ASAMUPawn"
        ]
    );
    assert!(d.warnings.is_empty(), "{:?}", d.warnings);
    let v = |n: &str| d.values.iter().find(|v| v.name == n).unwrap();
    // A value overridden by ASAMUPawn and one inherited from UTPawn.
    assert_eq!(v("JumpZ").source, "asamu.ASAMUPawn");
    assert_eq!(v("JumpZ").value, Value::Float(1000.0));
    assert_eq!(v("GroundSpeed").source, "UTGame.UTPawn");
    assert_eq!(v("GroundSpeed").value, Value::Float(440.0));
    assert_eq!(v("CustomGravityScaling").source, "UDKBase.UDKPawn");
}

/// Planes of a decoded binary `Matrix` value, each as its four floats in
/// serialized order.
fn matrix_planes(v: &Value) -> Option<Vec<[f32; 4]>> {
    let Value::Struct {
        name,
        binary: true,
        fields,
    } = v
    else {
        return None;
    };
    if name != "Matrix" {
        return None;
    }
    let mut out = Vec::new();
    for plane in fields {
        let Value::Struct { fields: f, .. } = &plane.value else {
            return None;
        };
        let mut xs = [0f32; 4];
        for (slot, m) in xs.iter_mut().zip(f) {
            let Value::Float(x) = m.value else {
                return None;
            };
            *slot = x;
        }
        out.push(xs);
    }
    (out.len() == 4).then_some(out)
}

#[derive(Default)]
struct Facts {
    matrices: usize,
    affine_own_first: usize,
    affine_super_first: usize,
    enum_none_tags: usize,
}

/// Walk a value: `tag` is true when `v` is the value of a tag (top level or a
/// member of a tagged struct), false for array items and binary members.
fn walk(v: &Value, tag: bool, facts: &mut Facts) {
    if tag && matches!(v, Value::Enum(n) if n == "None") {
        facts.enum_none_tags += 1;
    }
    if let Some(planes) = matrix_planes(v) {
        facts.matrices += 1;
        // Serialized order W, X, Y, Z: the affine W column is element 0 when
        // own members come first, element 3 if the super struct's came first.
        let col = |k: usize| planes.iter().map(|p| p[k]).collect::<Vec<_>>();
        if col(0) == [0.0, 0.0, 0.0, 1.0] {
            facts.affine_own_first += 1;
        }
        if col(3) == [0.0, 0.0, 0.0, 1.0] {
            facts.affine_super_first += 1;
        }
    }
    match v {
        Value::Array(items) => items.iter().for_each(|i| walk(i, false, facts)),
        Value::Struct { binary, fields, .. } => {
            for f in fields {
                walk(&f.value, !binary, facts);
            }
        }
        _ => {}
    }
}

/// Remove `//` and `/* */` comments (string literals are not special-cased;
/// good enough to count keywords).
fn strip_comments(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'/' && b.get(i + 1) == Some(&b'/') {
            while i < b.len() && b[i] != b'\n' {
                i += 1;
            }
        } else if b[i] == b'/' && b.get(i + 1) == Some(&b'*') {
            i += 2;
            while i < b.len() && !(b[i] == b'*' && b.get(i + 1) == Some(&b'/')) {
                i += 1;
            }
            i += 2;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// True when `text` has `keyword {` with `keyword` not preceded by an
/// identifier character (case-insensitive).
fn has_block(text: &str, keyword: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    let mut from = 0;
    while let Some(p) = lower[from..].find(keyword) {
        let at = from + p;
        let before_ok = lower[..at]
            .chars()
            .next_back()
            .is_none_or(|c| !(c.is_ascii_alphanumeric() || c == '_'));
        let after = lower[at + keyword.len()..].trim_start();
        if before_ok && after.starts_with('{') {
            return true;
        }
        from = at + keyword.len();
    }
    false
}

/// Facts about decoded *values* that consumers rely on (see "Value semantics"
/// and the binary-struct section of OBJECT_FORMAT.md).
#[test]
fn value_semantics_facts() {
    let dir = require_data!();
    let set = set_for(&dir);
    let mut facts = Facts::default();
    let mut top_native = 0usize;
    let mut top_transient = 0usize;
    let mut buffers = 0usize;
    let mut with_defaultproperties = 0usize;
    let mut with_structdefaultproperties = 0usize;
    for f in packages(&dir) {
        let lower = f
            .file_name()
            .unwrap()
            .to_string_lossy()
            .to_ascii_lowercase();
        let script = lower.ends_with(".u") || lower == "startup.upk";
        let lp = if script {
            set.open_file(&f).unwrap()
        } else {
            let stem = f.file_stem().unwrap().to_string_lossy().into_owned();
            std::sync::Arc::new(LoadedPackage::new(stem, &f, Package::open(&f).unwrap()))
        };
        for i in 0..lp.package.exports.len() {
            match ScriptKind::of_export(&lp.package, i) {
                Some(ScriptKind::TextBuffer) => {
                    // Read in memory only; never printed or written.
                    let text = asamu_ue3::script::text_buffer_text(&lp.package, i).unwrap();
                    let code = strip_comments(&text);
                    buffers += 1;
                    with_defaultproperties += usize::from(has_block(&code, "defaultproperties"));
                    with_structdefaultproperties +=
                        usize::from(has_block(&code, "structdefaultproperties"));
                    continue;
                }
                Some(_) => continue,
                None => {}
            }
            let o = set.decode(&lp, i).unwrap();
            for p in &o.properties {
                walk(&p.value, true, &mut facts);
                if let Some(d) = set.find_property(&o.class, &p.name) {
                    top_native += usize::from(d.flags & flags::property::NATIVE != 0);
                    top_transient += usize::from(d.flags & flags::property::TRANSIENT != 0);
                }
            }
        }
    }
    // Cooking strips every class defaultproperties block from ScriptText;
    // struct defaults blocks survive. "defaultproperties {" never matches
    // inside "structdefaultproperties {" (identifier character before it).
    assert_eq!(buffers, 2521);
    assert_eq!(with_defaultproperties, 0);
    assert_eq!(with_structdefaultproperties, 61);
    // Plane members are serialized own-first (W, then Vector's X, Y, Z).
    assert_eq!(facts.matrices, 3480);
    assert_eq!(facts.affine_own_first, 3475);
    assert_eq!(facts.affine_super_first, 0);
    // Enum values whose name is None (not an enumerator).
    assert_eq!(facts.enum_none_tags, 853);
    // Tagged streams carry transient but never native properties.
    assert_eq!(top_native, 0);
    assert!(top_transient > 0);
}
