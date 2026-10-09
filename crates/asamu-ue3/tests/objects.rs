//! Object payload decoding against a synthetic script package written byte by
//! byte in `objects_common` (no original game data).

#![allow(clippy::unwrap_used)]

mod common;
mod objects_common;

use asamu_ue3::coverage::{check_tag_order, package_coverage};
use asamu_ue3::flags;
use asamu_ue3::object::{ObjectError, decode_object};
use asamu_ue3::property::{NATIVE_LAYOUTS, native_layout, tag_type_matches};
use asamu_ue3::schema::{NoSchema, PropertyType, Schema};
use asamu_ue3::script::{self, PropertyKindData, ScriptBody, ScriptKind, decode_script_object};
use asamu_ue3::{Package, PackageIndex, PackageSet, Value};
use objects_common::{build, core, e, ex};

fn pkg() -> Package {
    Package::from_bytes(build().bytes).unwrap()
}

fn set() -> PackageSet {
    let set = PackageSet::new::<&str>(&[]);
    set.insert_package("TestPkg", pkg());
    set
}

#[test]
fn script_objects_consume_exactly() {
    let p = pkg();
    let cov = script::script_coverage(&p, Some("TestPkg"));
    let total: usize = cov.values().map(|c| c.total).sum();
    let exact: usize = cov.values().map(|c| c.exact).sum();
    assert_eq!(total, 20, "{cov:?}");
    assert_eq!(exact, total, "{cov:?}");
    assert_eq!(cov[&ScriptKind::Class].total, 3);
    assert_eq!(cov[&ScriptKind::Function].total, 1);
    assert_eq!(cov[&ScriptKind::State].total, 1);
    assert_eq!(cov[&ScriptKind::TextBuffer].total, 1);
}

#[test]
fn class_payload_fields() {
    let p = pkg();
    let o = decode_script_object(&p, Some("TestPkg"), ex::DERIVED, &NoSchema).unwrap();
    assert_eq!(o.kind, ScriptKind::Class);
    assert_eq!(o.net_index, 3);
    assert_eq!(o.next, Some(PackageIndex(0)));
    let ScriptBody::Class {
        structure,
        state,
        class,
    } = &o.body
    else {
        panic!("not a class: {o:?}");
    };
    assert_eq!(structure.super_struct, PackageIndex(e(ex::BASE)));
    assert_eq!(structure.script_text, PackageIndex(e(ex::SCRIPT_TEXT)));
    assert_eq!(structure.children, PackageIndex(e(ex::HEALTH)));
    assert_eq!(structure.storage_size, 2);
    assert_eq!(structure.bytecode_size, 2);
    assert_eq!(structure.line, -1);
    assert_eq!(state.label_table_offset, 0xFFFF);
    assert_eq!(class.class_flags, 0x12);
    assert_eq!(class.within, PackageIndex(core("Object")));
    assert_eq!(class.config_name, "Game");
    assert_eq!(class.hide_categories, vec!["Object"]);
    assert!(class.dont_sort_categories.is_empty());
    assert_eq!(class.dll_bind_name, "None");
    assert_eq!(class.default_object, PackageIndex(e(ex::DEFAULT_DERIVED)));
}

#[test]
fn function_state_enum_const_textbuffer() {
    let p = pkg();
    let d = |i| decode_script_object(&p, Some("TestPkg"), i, &NoSchema).unwrap();
    let ScriptBody::Function {
        structure,
        function,
    } = d(ex::DOIT).body
    else {
        panic!()
    };
    assert_eq!(structure.children, PackageIndex(e(ex::DOIT_X)));
    assert_eq!(structure.storage_size, 3);
    assert_eq!(function.native_index, 129);
    assert_eq!(
        function.function_flags & flags::function::NET,
        flags::function::NET
    );
    assert_eq!(function.rep_offset, Some(3));
    assert_eq!(function.friendly_name, "DoIt");

    let ScriptBody::State { state, .. } = d(ex::IDLE).body else {
        panic!()
    };
    assert_eq!(state.probe_mask, 0xFFFF_FFFF);
    assert_eq!(state.state_flags, flags::state::AUTO);
    assert_eq!(
        state.func_map,
        vec![("DoIt".to_owned(), PackageIndex(e(ex::DOIT)))]
    );

    let ScriptBody::Enum { names } = d(ex::EMODE).body else {
        panic!()
    };
    assert_eq!(names, vec!["M_A", "M_B"]);

    let ScriptBody::Const { value } = d(ex::MAX_THING).body else {
        panic!()
    };
    assert_eq!(value, "42");

    let t = d(ex::SCRIPT_TEXT);
    assert_eq!(t.next, None);
    let ScriptBody::TextBuffer {
        pos,
        top,
        text_chars,
        ..
    } = t.body
    else {
        panic!()
    };
    assert_eq!((pos, top, text_chars), (3, 9, 14));
    assert_eq!(
        script::text_buffer_text(&p, ex::SCRIPT_TEXT).unwrap(),
        "synthetic text"
    );
    assert!(matches!(
        script::text_buffer_text(&p, ex::DOIT),
        Err(ObjectError::WrongKind { .. })
    ));
}

#[test]
fn property_payload_fields() {
    let p = pkg();
    let prop = |i| {
        let o = decode_script_object(&p, Some("TestPkg"), i, &NoSchema).unwrap();
        o.property().unwrap().clone()
    };
    let h = prop(ex::HEALTH);
    assert_eq!(h.array_dim, 1);
    assert_eq!(h.flags, 0x21);
    assert_eq!(h.rep_offset, Some(7));
    assert_eq!(h.kind, PropertyKindData::Int);
    assert_eq!(
        prop(ex::POINTS).kind,
        PropertyKindData::Array {
            inner: PackageIndex(e(ex::POINTS_INNER))
        }
    );
    assert_eq!(
        prop(ex::WHERE).kind,
        PropertyKindData::Struct {
            struct_: PackageIndex(e(ex::MYVEC))
        }
    );
    let m = prop(ex::MODE);
    assert_eq!(m.category, "Derived");
    assert_eq!(
        m.kind,
        PropertyKindData::Byte {
            enum_: PackageIndex(e(ex::EMODE))
        }
    );
    let ScriptBody::ScriptStruct {
        struct_flags,
        defaults,
        ..
    } = decode_script_object(&p, Some("TestPkg"), ex::MYVEC, &NoSchema)
        .unwrap()
        .body
    else {
        panic!()
    };
    assert_eq!(struct_flags, 0x30);
    assert_eq!(defaults.len(), 1);
    assert_eq!(defaults[0].value, Value::Float(0.5));
}

#[test]
fn non_script_exports_are_rejected_by_the_strict_decoder() {
    let p = pkg();
    for i in [ex::DEFAULT_DERIVED, ex::INSTANCE, ex::COMP_TEMPLATE] {
        assert!(ScriptKind::of_export(&p, i).is_none());
        assert!(matches!(
            decode_script_object(&p, None, i, &NoSchema),
            Err(ObjectError::WrongKind { .. })
        ));
    }
}

#[test]
fn class_model() {
    let set = set();
    let m = set.class_model("TestPkg.Derived").unwrap();
    assert_eq!(m.super_chain, vec!["TestPkg.Base", "Core.Object"]);
    assert_eq!(m.config_name, "Game");
    assert_eq!(
        m.default_object.as_deref(),
        Some("TestPkg.Default__Derived")
    );
    assert_eq!(m.script_text.as_deref(), Some("TestPkg.Derived.ScriptText"));
    let names: Vec<&str> = m.properties.iter().map(|p| p.name.as_str()).collect();
    assert_eq!(names, vec!["Health", "Points", "Where", "Mode", "Title"]);
    assert_eq!(m.properties[1].type_desc, "array<int>");
    assert_eq!(m.properties[2].type_desc, "MyVec");
    assert_eq!(m.properties[3].type_desc, "EMode");
    assert_eq!(m.properties[4].type_desc, "string");
    assert_eq!(m.properties[0].rep_offset, Some(7));
    assert_eq!(m.functions.len(), 1);
    let f = &m.functions[0];
    assert_eq!(f.name, "DoIt");
    assert_eq!(f.native_index, 129);
    assert_eq!(f.params.len(), 1);
    assert_eq!(f.params[0].name, "X");
    assert_eq!(f.params[0].type_desc, "int");
    assert_eq!(f.return_type.as_deref(), Some("bool"));
    assert!(f.flag_names.contains(&"Native".to_owned()));
    assert_eq!(m.states.len(), 1);
    assert_eq!(m.states[0].name, "Idle");
    assert_eq!(m.enums[0].values, vec!["M_A", "M_B"]);
    assert_eq!(m.consts[0].value, "42");
    assert_eq!(m.structs[0].name, "MyVec");
    assert_eq!(m.structs[0].flag_names, vec!["Atomic", "Immutable"]);
    assert_eq!(m.structs[0].defaults[0].name, "A");

    // Schema view.
    let link: Vec<String> = set
        .property_link("TestPkg.Derived")
        .iter()
        .map(|d| d.name.clone())
        .collect();
    assert_eq!(
        link,
        vec!["Health", "Points", "Where", "Mode", "Title", "BaseSpeed"]
    );
    let base = set.find_property("TestPkg.Derived", "basespeed").unwrap();
    assert_eq!(base.ty, PropertyType::Float);
    assert_eq!(
        set.class_chain("TestPkg.Derived"),
        vec!["derived", "base", "object"]
    );
    assert_eq!(
        set.enum_names("TestPkg.Derived.EMode").unwrap().as_ref(),
        &vec!["M_A".to_owned(), "M_B".to_owned()]
    );
    assert!(set.class_model("TestPkg.Derived.Health").is_err());
    assert!(set.class_model("TestPkg.Nope").is_err());
}

#[test]
fn class_defaults_and_inheritance() {
    let set = set();
    let d = set.class_defaults("TestPkg.Derived").unwrap();
    assert_eq!(d.native_tail(), 0);
    assert!(d.warnings.is_empty(), "{:?}", d.warnings);
    let get = |n: &str| {
        d.properties
            .iter()
            .find(|p| p.name == n)
            .unwrap()
            .value
            .clone()
    };
    assert_eq!(get("Health"), Value::Int(7));
    assert_eq!(
        get("Points"),
        Value::Array(vec![Value::Int(1), Value::Int(2), Value::Int(3)])
    );
    let Value::Struct {
        name,
        binary,
        fields,
    } = get("Where")
    else {
        panic!()
    };
    assert_eq!((name.as_str(), binary), ("MyVec", true));
    assert_eq!(fields[0].value, Value::Float(2.5));
    assert_eq!(fields[1].value, Value::Int(-1));
    assert_eq!(get("Mode"), Value::Enum("M_B".into()));
    assert_eq!(get("Title"), Value::Str("hi there".into()));
    let (violation, undeclared) = check_tag_order(&set, &d.class, &d.properties);
    assert!(!violation);
    assert_eq!(undeclared, 0);

    let inh = set.inherited_defaults("TestPkg.Derived").unwrap();
    let classes: Vec<&str> = inh.sources.iter().map(|s| s.class.as_str()).collect();
    assert_eq!(classes, vec!["TestPkg.Base", "TestPkg.Derived"]);
    let speed = inh.values.iter().find(|v| v.name == "BaseSpeed").unwrap();
    assert_eq!(speed.value, Value::Float(3.0));
    assert_eq!(speed.source, "TestPkg.Derived");
    // Core.Object has no default object in the set: noted, not fatal.
    assert!(
        inh.warnings.iter().all(|w| !w.contains("TestPkg")),
        "{:?}",
        inh.warnings
    );

    let base = set.inherited_defaults("TestPkg.Base").unwrap();
    let speed = base.values.iter().find(|v| v.name == "BaseSpeed").unwrap();
    assert_eq!(speed.value, Value::Float(1.5));
    assert_eq!(speed.source, "TestPkg.Base");
}

#[test]
fn preludes() {
    let set = set();
    let lp = set.package("TestPkg").unwrap();

    let inst = set.decode(&lp, ex::INSTANCE).unwrap();
    let sf = inst.prelude.state_frame.clone().unwrap();
    assert_eq!(sf.node, PackageIndex(e(ex::DERIVED)));
    assert_eq!(sf.state_node, PackageIndex(e(ex::IDLE)));
    assert_eq!(sf.probe_mask, 0xFFFF_FFFF);
    assert_eq!(sf.latent_action, 0x30);
    assert_eq!(sf.code_offset, Some(-1));
    assert_eq!(inst.prelude.net_index, 24);
    assert_eq!(inst.properties.len(), 1);
    assert_eq!(inst.native_tail(), 4);

    let t = set.decode(&lp, ex::COMP_TEMPLATE).unwrap();
    let c = t.prelude.component.clone().unwrap();
    assert_eq!(c.template_name.as_deref(), Some("Comp0"));
    assert_eq!(t.prelude.net_index, 23);
    assert_eq!(t.native_tail(), 0);

    let i = set.decode(&lp, ex::COMP_INSTANCE).unwrap();
    assert_eq!(i.prelude.component.clone().unwrap().template_name, None);
    assert_eq!(i.prelude.net_index, 25);

    let cdo = set.decode(&lp, ex::DEFAULT_MYCOMP).unwrap();
    assert!(cdo.prelude.component.is_none());
    assert_eq!(cdo.prelude.net_index, 22);

    // Class objects have no tagged properties.
    let class = set.decode(&lp, ex::DERIVED).unwrap();
    assert!(class.properties.is_empty());
    assert_eq!(class.properties_end, 4);

    let cov = package_coverage(&set, &lp, true);
    assert_eq!(cov.cdo.total, 3);
    assert_eq!(cov.cdo.exact, 3);
    assert_eq!(cov.cdo.order_violations, 0);
    let o = cov.objects.unwrap();
    assert_eq!(o.total, 3);
    assert_eq!(o.decoded, 3);
    assert_eq!(o.native_tail, 1);
}

#[test]
fn no_schema_keeps_values_raw_and_probes_preludes() {
    let p = pkg();
    let d = decode_object(&p, Some("TestPkg"), ex::DEFAULT_DERIVED, &NoSchema).unwrap();
    let get = |n: &str| {
        d.properties
            .iter()
            .find(|q| q.name == n)
            .unwrap()
            .value
            .clone()
    };
    assert_eq!(get("Health"), Value::Int(7));
    assert!(matches!(
        get("Points"),
        Value::RawArray {
            count: 3,
            bytes: 12
        }
    ));
    assert!(matches!(get("Where"), Value::Raw { bytes: 8, .. }));
    assert_eq!(get("Mode"), Value::Enum("M_B".into()));
    assert!(!d.warnings.is_empty());

    // Unknown hierarchy: the component template prelude is found by probing.
    let t = decode_object(&p, Some("TestPkg"), ex::COMP_TEMPLATE, &NoSchema).unwrap();
    assert_eq!(
        t.prelude
            .component
            .clone()
            .unwrap()
            .template_name
            .as_deref(),
        Some("Comp0")
    );
    assert!(t.warnings.iter().any(|w| w.contains("probing")));
}

#[test]
fn tag_types_and_native_layouts() {
    let class = PropertyType::Class {
        class: "Core.Class".into(),
        meta_class: "Engine.Actor".into(),
    };
    assert!(tag_type_matches("ObjectProperty", &class));
    assert!(!tag_type_matches("ClassProperty", &class));
    assert!(tag_type_matches("IntProperty", &PropertyType::Int));
    assert!(!tag_type_matches("FloatProperty", &PropertyType::Int));
    let plane = native_layout("plane").unwrap();
    assert_eq!(plane[0].0, "W");
    assert_eq!(NATIVE_LAYOUTS.len(), 14);
    assert!(native_layout("NotAStruct").is_none());
}

#[test]
fn value_rendering() {
    let v = Value::Struct {
        name: "Vector".into(),
        binary: true,
        fields: Vec::new(),
    };
    assert_eq!(v.render(), "()");
    assert_eq!(Value::Name("Foo".into()).render(), "'Foo'");
    assert_eq!(
        Value::Array(vec![Value::Int(1), Value::Bool(true)]).render(),
        "[1, true]"
    );
    assert!(Value::RawArray { count: 1, bytes: 4 }.has_raw());
    assert!(!Value::Int(0).has_raw());
}
