//! A synthetic script package with hand-written v868 export payloads, used by
//! the object decoder tests. Every byte is written here; nothing comes from
//! the original game.

#![allow(dead_code, clippy::unwrap_used)]

use crate::common::{Export, Import, Synth, W};

/// Names of the synthetic package, in name-table order.
pub const NAMES: &[&str] = &[
    "None",
    "Core",
    "Package",
    "Class",
    "Function",
    "IntProperty",
    "FloatProperty",
    "BoolProperty",
    "StrProperty",
    "NameProperty",
    "ObjectProperty",
    "StructProperty",
    "ArrayProperty",
    "ByteProperty",
    "Enum",
    "Const",
    "TextBuffer",
    "ScriptStruct",
    "State",
    "Object",
    "Component",
    "Base",
    "BaseSpeed",
    "Default__Base",
    "Derived",
    "Health",
    "Points",
    "Where",
    "MyVec",
    "A",
    "B",
    "Mode",
    "EMode",
    "M_A",
    "M_B",
    "DoIt",
    "X",
    "ReturnValue",
    "ScriptText",
    "Default__Derived",
    "Idle",
    "MaxThing",
    "Title",
    "Partner",
    "MyComp",
    "Default__MyComp",
    "Comp0",
    "Size",
    "TheLevel",
    "Instance",
    "Inner",
    "Game",
];

/// Name-table index of `s`.
pub fn n(s: &str) -> i32 {
    NAMES.iter().position(|x| *x == s).unwrap() as i32
}

/// Import package index of `Core.<class>` (see [`IMPORT_CLASSES`]).
pub fn core(class: &str) -> i32 {
    let i = IMPORT_CLASSES.iter().position(|x| *x == class).unwrap();
    -(i as i32 + 2)
}

/// Core classes imported as `Core.<name>` (import 0 is the `Core` package).
pub const IMPORT_CLASSES: &[&str] = &[
    "Class",
    "Function",
    "IntProperty",
    "FloatProperty",
    "BoolProperty",
    "StrProperty",
    "NameProperty",
    "ObjectProperty",
    "StructProperty",
    "ArrayProperty",
    "ByteProperty",
    "Enum",
    "Const",
    "TextBuffer",
    "ScriptStruct",
    "State",
    "Object",
    "Component",
];

/// Little-endian payload writer with UE3 helpers.
#[derive(Default)]
pub struct P(pub W);

impl P {
    pub fn i32(&mut self, v: i32) -> &mut Self {
        self.0.i32(v);
        self
    }
    pub fn u32(&mut self, v: u32) -> &mut Self {
        self.0.u32(v);
        self
    }
    pub fn u16(&mut self, v: u16) -> &mut Self {
        self.0.u16(v);
        self
    }
    pub fn u8(&mut self, v: u8) -> &mut Self {
        self.0.bytes(&[v]);
        self
    }
    pub fn u64(&mut self, v: u64) -> &mut Self {
        self.0.u64(v);
        self
    }
    pub fn f32(&mut self, v: f32) -> &mut Self {
        self.0.bytes(&v.to_le_bytes());
        self
    }
    pub fn name(&mut self, s: &str) -> &mut Self {
        self.i32(n(s)).i32(0)
    }
    pub fn fstring(&mut self, s: &str) -> &mut Self {
        self.0.fstring(s);
        self
    }
    pub fn none(&mut self) -> &mut Self {
        self.name("None")
    }
    /// Tag header.
    pub fn tag(&mut self, name: &str, ty: &str, size: i32, index: i32) -> &mut Self {
        self.name(name).name(ty).i32(size).i32(index)
    }
    /// `UStruct` part with the given bytecode.
    pub fn structure(&mut self, sup: i32, text: i32, children: i32, code: &[u8]) -> &mut Self {
        self.i32(sup)
            .i32(text)
            .i32(children)
            .i32(0)
            .i32(-1)
            .i32(-1)
            .i32(code.len() as i32)
            .i32(code.len() as i32);
        self.0.bytes(code);
        self
    }
    /// `UObject` + `UField` + `UProperty` common part.
    pub fn property(&mut self, next: i32, dim: i32, flags: u64, category: &str) -> &mut Self {
        self.i32(0)
            .none()
            .i32(next)
            .i32(dim)
            .u64(flags)
            .name(category)
            .i32(0);
        if flags & 0x20 != 0 {
            self.u16(7);
        }
        self
    }
    pub fn bytes(&self) -> Vec<u8> {
        self.0.0.clone()
    }
}

/// UStruct `UClass` tail (`UState` + `UClass` fields) with a CDO reference.
fn class_tail(p: &mut P, cdo: i32) {
    // UState
    p.u32(0).u16(0xFFFF).u32(0).i32(0);
    // UClass
    p.u32(0x0000_0012).i32(core("Object")).name("Game");
    p.i32(0); // component map
    p.i32(0); // interfaces
    p.i32(0); // DontSortCategories
    p.i32(1).name("Object"); // HideCategories
    p.i32(0).i32(0); // AutoExpand, AutoCollapse
    p.u32(0); // bForceScriptOrder
    p.i32(0); // ClassGroupNames
    p.fstring(""); // ClassHeaderFilename
    p.none(); // DLLBindName
    p.i32(cdo);
}

pub struct Built {
    pub synth: Synth,
    pub bytes: Vec<u8>,
}

/// Export indices (0-based) of the synthetic objects.
pub mod ex {
    pub const BASE: usize = 0;
    pub const BASE_SPEED: usize = 1;
    pub const DEFAULT_BASE: usize = 2;
    pub const DERIVED: usize = 3;
    pub const HEALTH: usize = 4;
    pub const POINTS: usize = 5;
    pub const POINTS_INNER: usize = 6;
    pub const WHERE: usize = 7;
    pub const MYVEC: usize = 8;
    pub const MYVEC_A: usize = 9;
    pub const MYVEC_B: usize = 10;
    pub const MODE: usize = 11;
    pub const EMODE: usize = 12;
    pub const DOIT: usize = 13;
    pub const DOIT_X: usize = 14;
    pub const DOIT_RET: usize = 15;
    pub const SCRIPT_TEXT: usize = 16;
    pub const DEFAULT_DERIVED: usize = 17;
    pub const IDLE: usize = 18;
    pub const MAX_THING: usize = 19;
    pub const TITLE: usize = 20;
    pub const MYCOMP: usize = 21;
    pub const DEFAULT_MYCOMP: usize = 22;
    pub const COMP_TEMPLATE: usize = 23;
    pub const INSTANCE: usize = 24;
    pub const COMP_INSTANCE: usize = 25;
}

/// Package index of 0-based export `i`.
pub fn e(i: usize) -> i32 {
    i as i32 + 1
}

/// Default payloads, by export.
pub fn payloads() -> Vec<Vec<u8>> {
    use ex::*;
    let mut v = Vec::new();
    // 0 Base: class, super Core.Object, children BaseSpeed.
    let mut p = P::default();
    p.i32(0)
        .i32(0)
        .structure(core("Object"), 0, e(BASE_SPEED), &[]);
    class_tail(&mut p, e(DEFAULT_BASE));
    v.push(p.bytes());
    // 1 Base.BaseSpeed: float.
    let mut p = P::default();
    p.property(0, 1, 0x1, "Base");
    v.push(p.bytes());
    // 2 Default__Base: BaseSpeed = 1.5.
    let mut p = P::default();
    p.i32(2)
        .tag("BaseSpeed", "FloatProperty", 4, 0)
        .f32(1.5)
        .none();
    v.push(p.bytes());
    // 3 Derived: class, super Base; children chain:
    //   Health -> Points -> Where -> Mode -> Title -> DoIt -> MyVec -> EMode -> Idle -> MaxThing
    let mut p = P::default();
    p.i32(3)
        .i32(0)
        .structure(e(BASE), e(SCRIPT_TEXT), e(HEALTH), &[0x0B, 0x53]);
    class_tail(&mut p, e(DEFAULT_DERIVED));
    v.push(p.bytes());
    // 4 Health: int, replicated (CPF_Net -> RepOffset).
    let mut p = P::default();
    p.property(e(POINTS), 1, 0x21, "None");
    v.push(p.bytes());
    // 5 Points: array<int>.
    let mut p = P::default();
    p.property(e(WHERE), 1, 0x40_0000, "None")
        .i32(e(POINTS_INNER));
    v.push(p.bytes());
    // 6 Points.Points: inner int.
    let mut p = P::default();
    p.property(0, 1, 0, "None");
    v.push(p.bytes());
    // 7 Where: struct MyVec.
    let mut p = P::default();
    p.property(e(MODE), 1, 0, "None").i32(e(MYVEC));
    v.push(p.bytes());
    // 8 MyVec: script struct, immutable (binary), members A (float), B (int); defaults A=0.5.
    let mut p = P::default();
    p.i32(8)
        .none()
        .i32(e(EMODE))
        .structure(0, 0, e(MYVEC_A), &[])
        .u32(0x30)
        .tag("A", "FloatProperty", 4, 0)
        .f32(0.5)
        .none();
    v.push(p.bytes());
    // 9 MyVec.A float, 10 MyVec.B int.
    let mut p = P::default();
    p.property(e(MYVEC_B), 1, 0, "None");
    v.push(p.bytes());
    let mut p = P::default();
    p.property(0, 1, 0, "None");
    v.push(p.bytes());
    // 11 Mode: byte EMode.
    let mut p = P::default();
    p.property(e(TITLE), 1, 0x1, "Derived").i32(e(EMODE));
    v.push(p.bytes());
    // 12 EMode: enum {M_A, M_B}.
    let mut p = P::default();
    p.i32(12).none().i32(e(IDLE)).i32(2).name("M_A").name("M_B");
    v.push(p.bytes());
    // 13 DoIt: function (Net -> RepOffset), children X -> ReturnValue.
    let mut p = P::default();
    p.i32(13)
        .none()
        .i32(e(MYVEC))
        .structure(0, 0, e(DOIT_X), &[0x04, 0x0B, 0x53])
        .u16(129)
        .u8(0)
        .u32(0x0000_2441 | 0x40)
        .u16(3)
        .name("DoIt");
    v.push(p.bytes());
    // 14 X: int parm; 15 ReturnValue: bool return parm.
    let mut p = P::default();
    p.property(e(DOIT_RET), 1, 0x80, "None");
    v.push(p.bytes());
    let mut p = P::default();
    p.property(0, 1, 0x580, "None");
    v.push(p.bytes());
    // 16 ScriptText: text buffer.
    let mut p = P::default();
    p.i32(16).none().i32(3).i32(9).fstring("synthetic text");
    v.push(p.bytes());
    // 17 Default__Derived: Health=7, Points=[1,2,3], Where=(A=2.5,B=-1) binary,
    //    Mode=M_B, Title="hi there", BaseSpeed=3.0 (inherited property).
    let mut p = P::default();
    p.i32(17)
        .tag("Health", "IntProperty", 4, 0)
        .i32(7)
        .tag("Points", "ArrayProperty", 16, 0)
        .i32(3)
        .i32(1)
        .i32(2)
        .i32(3)
        .tag("Where", "StructProperty", 8, 0)
        .name("MyVec")
        .f32(2.5)
        .i32(-1)
        .tag("Mode", "ByteProperty", 8, 0)
        .name("EMode")
        .name("M_B")
        .tag("Title", "StrProperty", 13, 0)
        .fstring("hi there")
        .tag("BaseSpeed", "FloatProperty", 4, 0)
        .f32(3.0)
        .none();
    v.push(p.bytes());
    // 18 Idle: state.
    let mut p = P::default();
    p.i32(18)
        .none()
        .i32(e(MAX_THING))
        .structure(0, 0, 0, &[0x0B])
        .u32(0xFFFF_FFFF)
        .u16(0xFFFF)
        .u32(0x2)
        .i32(1)
        .name("DoIt")
        .i32(e(DOIT));
    v.push(p.bytes());
    // 19 MaxThing: const.
    let mut p = P::default();
    p.i32(19).none().i32(0).fstring("42");
    v.push(p.bytes());
    // 20 Title: string, next DoIt.
    let mut p = P::default();
    p.property(e(DOIT), 1, 0x40_0001, "Derived");
    v.push(p.bytes());
    // 21 MyComp: component class (super Core.Component), children Size.
    let mut p = P::default();
    p.i32(21).i32(0).structure(core("Component"), 0, 0, &[]);
    class_tail(&mut p, e(DEFAULT_MYCOMP));
    v.push(p.bytes());
    // 22 Default__MyComp: CDO of a component class (no template data).
    let mut p = P::default();
    p.i32(22).none();
    v.push(p.bytes());
    // 23 Default__Derived.Comp0: component inside a CDO (owner + template name).
    let mut p = P::default();
    p.i32(0).name("Comp0").i32(23).none();
    v.push(p.bytes());
    // 24 TheLevel.Instance: Derived instance with RF_HasStack (state frame).
    let mut p = P::default();
    p.i32(e(DERIVED))
        .i32(e(IDLE))
        .u32(0xFFFF_FFFF)
        .u16(0x30)
        .i32(0)
        .i32(-1)
        .i32(24)
        .tag("Health", "IntProperty", 4, 0)
        .i32(99)
        .none()
        .u32(0xDEAD_BEEF); // native tail
    v.push(p.bytes());
    // 25 TheLevel.Instance.Comp0: component outside a CDO (owner only).
    let mut p = P::default();
    p.i32(0).i32(25).none();
    v.push(p.bytes());
    v
}

/// Build the synthetic package with the given payloads.
pub fn build_with(payloads: &[Vec<u8>]) -> Built {
    use ex::*;
    let names = NAMES.iter().map(|s| (s.to_string(), 0u64)).collect();
    let mut imports = vec![Import {
        class_package: n("Core"),
        class_name: n("Package"),
        outer: 0,
        name: n("Core"),
        number: 0,
    }];
    for c in IMPORT_CLASSES {
        imports.push(Import {
            class_package: n("Core"),
            class_name: n("Class"),
            outer: -1,
            name: n(c),
            number: 0,
        });
    }
    // (class, super, outer, name, flags)
    let cdo = 0x0007_0004_0000_0200u64;
    let plain = 0x0007_0004_0000_0000u64;
    let stack = 0x0207_0001_0000_0000u64;
    let table: Vec<(i32, i32, i32, &str, u64)> = vec![
        (0, core("Object"), 0, "Base", plain),
        (core("FloatProperty"), 0, e(BASE), "BaseSpeed", plain),
        (e(BASE), 0, 0, "Default__Base", cdo),
        (0, e(BASE), 0, "Derived", plain),
        (core("IntProperty"), 0, e(DERIVED), "Health", plain),
        (core("ArrayProperty"), 0, e(DERIVED), "Points", plain),
        (core("IntProperty"), 0, e(POINTS), "Points", plain),
        (core("StructProperty"), 0, e(DERIVED), "Where", plain),
        (core("ScriptStruct"), 0, e(DERIVED), "MyVec", plain),
        (core("FloatProperty"), 0, e(MYVEC), "A", plain),
        (core("IntProperty"), 0, e(MYVEC), "B", plain),
        (core("ByteProperty"), 0, e(DERIVED), "Mode", plain),
        (core("Enum"), 0, e(DERIVED), "EMode", plain),
        (core("Function"), 0, e(DERIVED), "DoIt", plain),
        (core("IntProperty"), 0, e(DOIT), "X", plain),
        (core("BoolProperty"), 0, e(DOIT), "ReturnValue", plain),
        (core("TextBuffer"), 0, e(DERIVED), "ScriptText", plain),
        (e(DERIVED), 0, 0, "Default__Derived", cdo),
        (core("State"), 0, e(DERIVED), "Idle", plain),
        (core("Const"), 0, e(DERIVED), "MaxThing", plain),
        (core("StrProperty"), 0, e(DERIVED), "Title", plain),
        (0, core("Component"), 0, "MyComp", plain),
        (e(MYCOMP), 0, 0, "Default__MyComp", cdo),
        (e(MYCOMP), 0, e(DEFAULT_DERIVED), "Comp0", plain),
        (e(DERIVED), 0, 0, "Instance", stack),
        (e(MYCOMP), 0, e(INSTANCE), "Comp0", plain),
    ];
    assert_eq!(table.len(), payloads.len());
    let exports = table
        .iter()
        .zip(payloads)
        .map(|(&(class, super_, outer, name, flags), payload)| Export {
            class,
            super_,
            outer,
            name: n(name),
            number: 0,
            archetype: 0,
            object_flags: flags,
            payload: payload.clone(),
            export_flags: 0,
            net_counts: Vec::new(),
            guid: [0; 4],
            package_flags: 0,
        })
        .collect();
    let mut synth = Synth::sample();
    synth.names = names;
    synth.imports = imports;
    synth.exports = exports;
    synth.package_flags = 0x0020_0008; // ContainsScript | Cooked
    synth.texture_allocations = Vec::new();
    synth.additional_packages = Vec::new();
    let (bytes, _) = synth.build();
    Built { synth, bytes }
}

/// The default synthetic package.
pub fn build() -> Built {
    build_with(&payloads())
}
