//! UnrealScript bytecode decoder against hand-written token streams.
//!
//! Every byte below is written by this test; nothing comes from the original
//! game. The encodings mirror the v868 layout documented in
//! `docs/reverse-engineering/BYTECODE.md`.

#![allow(clippy::unwrap_used)]

mod common;
mod objects_common;

use asamu_ue3::Package;
use asamu_ue3::bytecode::{
    self, BytecodeError, ExprKind, LABEL_TABLE_END, Layout, MAX_DEPTH, NativeTable, OperandRole,
    Script, decode, token, token_name,
};
use asamu_ue3::model::PackageSet;

/// Little-endian token stream builder.
#[derive(Default, Clone)]
struct B(Vec<u8>);

impl B {
    fn t(mut self, x: u8) -> Self {
        self.0.push(x);
        self
    }
    fn obj(mut self, i: i32) -> Self {
        self.0.extend_from_slice(&i.to_le_bytes());
        self
    }
    fn name(mut self, i: i32, n: i32) -> Self {
        self.0.extend_from_slice(&i.to_le_bytes());
        self.0.extend_from_slice(&n.to_le_bytes());
        self
    }
    fn u16(mut self, v: u16) -> Self {
        self.0.extend_from_slice(&v.to_le_bytes());
        self
    }
    fn u32(mut self, v: u32) -> Self {
        self.0.extend_from_slice(&v.to_le_bytes());
        self
    }
    fn i32(mut self, v: i32) -> Self {
        self.0.extend_from_slice(&v.to_le_bytes());
        self
    }
    fn f32(mut self, v: f32) -> Self {
        self.0.extend_from_slice(&v.to_le_bytes());
        self
    }
    fn raw(mut self, b: &[u8]) -> Self {
        self.0.extend_from_slice(b);
        self
    }
    fn cat(mut self, o: &B) -> Self {
        self.0.extend_from_slice(&o.0);
        self
    }
    fn len(&self) -> usize {
        self.0.len()
    }
}

fn b() -> B {
    B::default()
}

/// `InstanceVariable <obj>`: 5 storage / 9 memory bytes.
fn ivar(i: i32) -> B {
    b().t(token::INSTANCE_VARIABLE).obj(i)
}

/// `LocalVariable <obj>`.
fn lvar(i: i32) -> B {
    b().t(token::LOCAL_VARIABLE).obj(i)
}

fn one(bytes: &B) -> Script {
    let s = decode(&bytes.0, Layout::SHIPPED)
        .unwrap_or_else(|e| panic!("decode failed: {e} for {:02x?}", bytes.0));
    assert_eq!(s.storage_size, bytes.len());
    assert_eq!(s.statements.len(), 1, "expected one statement: {s:#?}");
    s
}

/// One case of the encoding table: bytes, token, storage and memory size.
struct Case {
    bytes: B,
    token: u8,
    storage: usize,
    memory: usize,
    check: fn(&ExprKind) -> bool,
}

fn cases() -> Vec<Case> {
    use token::*;
    let zero = || b().t(INT_ZERO);
    let mut v = vec![
        Case {
            bytes: lvar(-3),
            token: LOCAL_VARIABLE,
            storage: 5,
            memory: 9,
            check: |k| matches!(k, ExprKind::LocalVariable { property } if property.0 == -3),
        },
        Case {
            bytes: ivar(7),
            token: INSTANCE_VARIABLE,
            storage: 5,
            memory: 9,
            check: |k| matches!(k, ExprKind::InstanceVariable { property } if property.0 == 7),
        },
        Case {
            bytes: b().t(DEFAULT_VARIABLE).obj(1),
            token: DEFAULT_VARIABLE,
            storage: 5,
            memory: 9,
            check: |k| matches!(k, ExprKind::DefaultVariable { .. }),
        },
        Case {
            bytes: b().t(STATE_VARIABLE).obj(1),
            token: STATE_VARIABLE,
            storage: 5,
            memory: 9,
            check: |k| matches!(k, ExprKind::StateVariable { .. }),
        },
        Case {
            bytes: b().t(LOCAL_OUT_VARIABLE).obj(1),
            token: LOCAL_OUT_VARIABLE,
            storage: 5,
            memory: 9,
            check: |k| matches!(k, ExprKind::LocalOutVariable { .. }),
        },
        Case {
            bytes: b().t(NATIVE_PARM).obj(1),
            token: NATIVE_PARM,
            storage: 5,
            memory: 9,
            check: |k| matches!(k, ExprKind::NativeParm { .. }),
        },
        Case {
            bytes: b().t(RETURN).cat(&zero()),
            token: RETURN,
            storage: 2,
            memory: 2,
            check: |k| matches!(k, ExprKind::Return { value } if value.token == INT_ZERO),
        },
        Case {
            bytes: b().t(RETURN_NOTHING).obj(4),
            token: RETURN_NOTHING,
            storage: 5,
            memory: 9,
            check: |k| matches!(k, ExprKind::ReturnNothing { .. }),
        },
        Case {
            bytes: b().t(SWITCH).obj(2).t(4).cat(&zero()),
            token: SWITCH,
            storage: 7,
            memory: 11,
            check: |k| matches!(k, ExprKind::Switch { value_size: 4, .. }),
        },
        Case {
            bytes: b().t(CASE).u16(5).cat(&zero()),
            token: CASE,
            storage: 4,
            memory: 4,
            check: |k| {
                matches!(
                    k,
                    ExprKind::Case {
                        next: Some(5),
                        value: Some(_)
                    }
                )
            },
        },
        Case {
            bytes: b().t(CASE).u16(0xFFFF),
            token: CASE,
            storage: 3,
            memory: 3,
            check: |k| {
                matches!(
                    k,
                    ExprKind::Case {
                        next: None,
                        value: None
                    }
                )
            },
        },
        Case {
            bytes: b().t(JUMP).u16(0x1234),
            token: JUMP,
            storage: 3,
            memory: 3,
            check: |k| matches!(k, ExprKind::Jump { target: 0x1234 }),
        },
        Case {
            bytes: b().t(JUMP_IF_NOT).u16(9).t(TRUE),
            token: JUMP_IF_NOT,
            storage: 4,
            memory: 4,
            check: |k| matches!(k, ExprKind::JumpIfNot { target: 9, .. }),
        },
        Case {
            bytes: b().t(JUMP_IF_NOT_EDITOR_ONLY).u16(3),
            token: JUMP_IF_NOT_EDITOR_ONLY,
            storage: 3,
            memory: 3,
            check: |k| matches!(k, ExprKind::JumpIfNotEditorOnly { target: 3 }),
        },
        Case {
            bytes: b().t(ASSERT).u16(77).t(1).t(TRUE),
            token: ASSERT,
            storage: 5,
            memory: 5,
            check: |k| {
                matches!(
                    k,
                    ExprKind::Assert {
                        line: 77,
                        debug_only: 1,
                        ..
                    }
                )
            },
        },
        Case {
            bytes: b()
                .t(LABEL_TABLE)
                .name(5, 0)
                .u32(0)
                .name(6, 2)
                .u32(40)
                .name(1, 0)
                .u32(LABEL_TABLE_END),
            token: LABEL_TABLE,
            storage: 37,
            memory: 37,
            check: |k| {
                matches!(k, ExprKind::LabelTable { labels, terminator }
                    if labels.len() == 2 && labels[1].offset == 40 && labels[1].name.number == 2
                        && terminator.index == 1)
            },
        },
        Case {
            bytes: b().t(GOTO_LABEL).t(NAME_CONST).name(5, 0),
            token: GOTO_LABEL,
            storage: 10,
            memory: 10,
            check: |k| matches!(k, ExprKind::GotoLabel { .. }),
        },
        Case {
            bytes: b()
                .t(EAT_RETURN_VALUE)
                .obj(3)
                .t(FINAL_FUNCTION)
                .obj(4)
                .t(END_FUNCTION_PARMS),
            token: EAT_RETURN_VALUE,
            storage: 11,
            memory: 19,
            check: |k| matches!(k, ExprKind::EatReturnValue { value, .. } if value.token == FINAL_FUNCTION),
        },
        Case {
            bytes: b().t(LET).cat(&lvar(1)).cat(&zero()),
            token: LET,
            storage: 7,
            memory: 11,
            check: |k| matches!(k, ExprKind::Let { .. }),
        },
        Case {
            bytes: b().t(LET_BOOL).cat(&lvar(1)).t(TRUE),
            token: LET_BOOL,
            storage: 7,
            memory: 11,
            check: |k| matches!(k, ExprKind::LetBool { .. }),
        },
        Case {
            bytes: b().t(LET_DELEGATE).cat(&lvar(1)).t(EMPTY_DELEGATE),
            token: LET_DELEGATE,
            storage: 7,
            memory: 11,
            check: |k| matches!(k, ExprKind::LetDelegate { .. }),
        },
        Case {
            bytes: b().t(DYN_ARRAY_ELEMENT).cat(&zero()).cat(&lvar(1)),
            token: DYN_ARRAY_ELEMENT,
            storage: 7,
            memory: 11,
            check: |k| matches!(k, ExprKind::DynArrayElement { index, .. } if index.token == INT_ZERO),
        },
        Case {
            bytes: b().t(ARRAY_ELEMENT).cat(&zero()).cat(&lvar(1)),
            token: ARRAY_ELEMENT,
            storage: 7,
            memory: 11,
            check: |k| matches!(k, ExprKind::ArrayElement { .. }),
        },
        Case {
            bytes: b()
                .t(NEW)
                .t(NO_OBJECT)
                .t(NO_OBJECT)
                .t(NO_OBJECT)
                .t(OBJECT_CONST)
                .obj(9)
                .t(NO_OBJECT),
            token: NEW,
            storage: 10,
            memory: 14,
            check: |k| matches!(k, ExprKind::New { class, .. } if class.token == OBJECT_CONST),
        },
        Case {
            bytes: b()
                .t(CONTEXT)
                .cat(&ivar(1))
                .u16(9)
                .obj(2)
                .t(0)
                .cat(&ivar(2)),
            token: CONTEXT,
            storage: 18,
            memory: 30,
            check: |k| {
                matches!(k, ExprKind::Context(c) if c.skip == 9 && c.rvalue_property.0 == 2
                    && c.rvalue_size == 0 && c.expr.token == INSTANCE_VARIABLE)
            },
        },
        Case {
            bytes: b()
                .t(CLASS_CONTEXT)
                .t(OBJECT_CONST)
                .obj(1)
                .u16(9)
                .obj(0)
                .t(0)
                .cat(&b().t(DEFAULT_VARIABLE).obj(2)),
            token: CLASS_CONTEXT,
            storage: 18,
            memory: 30,
            check: |k| matches!(k, ExprKind::ClassContext(_)),
        },
        Case {
            bytes: b().t(INTERFACE_CONTEXT).cat(&ivar(1)),
            token: INTERFACE_CONTEXT,
            storage: 6,
            memory: 10,
            check: |k| matches!(k, ExprKind::InterfaceContext { .. }),
        },
        Case {
            bytes: b().t(META_CAST).obj(1).t(NO_OBJECT),
            token: META_CAST,
            storage: 6,
            memory: 10,
            check: |k| matches!(k, ExprKind::MetaCast { .. }),
        },
        Case {
            bytes: b().t(DYNAMIC_CAST).obj(1).t(NO_OBJECT),
            token: DYNAMIC_CAST,
            storage: 6,
            memory: 10,
            check: |k| matches!(k, ExprKind::DynamicCast { .. }),
        },
        Case {
            bytes: b().t(INTERFACE_CAST).obj(1).t(NO_OBJECT),
            token: INTERFACE_CAST,
            storage: 6,
            memory: 10,
            check: |k| matches!(k, ExprKind::InterfaceCast { .. }),
        },
        Case {
            bytes: b().t(PRIMITIVE_CAST).t(0x3A).cat(&zero()),
            token: PRIMITIVE_CAST,
            storage: 3,
            memory: 3,
            check: |k| matches!(k, ExprKind::PrimitiveCast { cast: 0x3A, .. }),
        },
        Case {
            bytes: b().t(SKIP).u16(2).cat(&zero()),
            token: SKIP,
            storage: 4,
            memory: 4,
            check: |k| matches!(k, ExprKind::Skip { skip: 2, .. }),
        },
        Case {
            bytes: b()
                .t(VIRTUAL_FUNCTION)
                .name(3, 0)
                .cat(&zero())
                .t(END_FUNCTION_PARMS),
            token: VIRTUAL_FUNCTION,
            storage: 11,
            memory: 11,
            check: |k| matches!(k, ExprKind::VirtualFunction { args, .. } if args.len() == 1),
        },
        Case {
            bytes: b().t(GLOBAL_FUNCTION).name(3, 0).t(END_FUNCTION_PARMS),
            token: GLOBAL_FUNCTION,
            storage: 10,
            memory: 10,
            check: |k| matches!(k, ExprKind::GlobalFunction { args, .. } if args.is_empty()),
        },
        Case {
            bytes: b()
                .t(FINAL_FUNCTION)
                .obj(5)
                .cat(&zero())
                .t(END_FUNCTION_PARMS),
            token: FINAL_FUNCTION,
            storage: 7,
            memory: 11,
            check: |k| matches!(k, ExprKind::FinalFunction { function, args } if function.0 == 5 && args.len() == 1),
        },
        Case {
            bytes: b()
                .t(DELEGATE_FUNCTION)
                .t(1)
                .obj(2)
                .name(3, 0)
                .t(END_FUNCTION_PARMS),
            token: DELEGATE_FUNCTION,
            storage: 15,
            memory: 19,
            check: |k| matches!(k, ExprKind::DelegateFunction { local: 1, .. }),
        },
        Case {
            bytes: b().t(0x70).cat(&zero()).t(END_FUNCTION_PARMS),
            token: 0x70,
            storage: 3,
            memory: 3,
            check: |k| matches!(k, ExprKind::NativeFunction { index: 0x70, args } if args.len() == 1),
        },
        Case {
            bytes: b().t(0xFF).t(END_FUNCTION_PARMS),
            token: 0xFF,
            storage: 2,
            memory: 2,
            check: |k| matches!(k, ExprKind::NativeFunction { index: 0xFF, .. }),
        },
        Case {
            bytes: b().t(0x61).t(0x15).t(END_FUNCTION_PARMS),
            token: 0x61,
            storage: 3,
            memory: 3,
            check: |k| matches!(k, ExprKind::NativeFunction { index: 0x115, .. }),
        },
        Case {
            bytes: b().t(0x6F).t(0xFF).t(END_FUNCTION_PARMS),
            token: 0x6F,
            storage: 3,
            memory: 3,
            check: |k| matches!(k, ExprKind::NativeFunction { index: 0xFFF, .. }),
        },
        Case {
            bytes: b()
                .t(EQUAL_EQUAL_DEL_DEL)
                .cat(&ivar(1))
                .cat(&ivar(2))
                .t(END_FUNCTION_PARMS),
            token: EQUAL_EQUAL_DEL_DEL,
            storage: 12,
            memory: 20,
            check: |k| matches!(k, ExprKind::DelegateCompare { args } if args.len() == 2),
        },
        Case {
            bytes: b().t(INT_CONST).i32(-123456),
            token: INT_CONST,
            storage: 5,
            memory: 5,
            check: |k| matches!(k, ExprKind::IntConst { value: -123456 }),
        },
        Case {
            bytes: b().t(FLOAT_CONST).f32(0.25),
            token: FLOAT_CONST,
            storage: 5,
            memory: 5,
            check: |k| matches!(k, ExprKind::FloatConst { value } if *value == 0.25),
        },
        Case {
            bytes: b().t(BYTE_CONST).t(200),
            token: BYTE_CONST,
            storage: 2,
            memory: 2,
            check: |k| matches!(k, ExprKind::ByteConst { value: 200 }),
        },
        Case {
            bytes: b().t(INT_CONST_BYTE).t(42),
            token: INT_CONST_BYTE,
            storage: 2,
            memory: 2,
            check: |k| matches!(k, ExprKind::IntConstByte { value: 42 }),
        },
        Case {
            bytes: b().t(STRING_CONST).raw(b"abc\0"),
            token: STRING_CONST,
            storage: 5,
            memory: 5,
            check: |k| matches!(k, ExprKind::StringConst { value } if value == "abc"),
        },
        Case {
            bytes: b().t(UNICODE_STRING_CONST).u16(0x68).u16(0xE9).u16(0),
            token: UNICODE_STRING_CONST,
            storage: 7,
            memory: 7,
            check: |k| matches!(k, ExprKind::UnicodeStringConst { value } if value == "h\u{e9}"),
        },
        Case {
            bytes: b().t(OBJECT_CONST).obj(-8),
            token: OBJECT_CONST,
            storage: 5,
            memory: 9,
            check: |k| matches!(k, ExprKind::ObjectConst { object } if object.0 == -8),
        },
        Case {
            bytes: b().t(NAME_CONST).name(4, 1),
            token: NAME_CONST,
            storage: 9,
            memory: 9,
            check: |k| matches!(k, ExprKind::NameConst { name } if name.index == 4 && name.number == 1),
        },
        Case {
            bytes: b().t(ROTATION_CONST).i32(1).i32(-2).i32(3),
            token: ROTATION_CONST,
            storage: 13,
            memory: 13,
            check: |k| {
                matches!(
                    k,
                    ExprKind::RotationConst {
                        pitch: 1,
                        yaw: -2,
                        roll: 3
                    }
                )
            },
        },
        Case {
            bytes: b().t(VECTOR_CONST).f32(1.0).f32(2.0).f32(3.0),
            token: VECTOR_CONST,
            storage: 13,
            memory: 13,
            check: |k| matches!(k, ExprKind::VectorConst { y, .. } if *y == 2.0),
        },
        Case {
            bytes: b().t(BOOL_VARIABLE).cat(&ivar(3)),
            token: BOOL_VARIABLE,
            storage: 6,
            memory: 10,
            check: |k| matches!(k, ExprKind::BoolVariable { .. }),
        },
        Case {
            bytes: b()
                .t(ITERATOR)
                .t(FINAL_FUNCTION)
                .obj(5)
                .t(END_FUNCTION_PARMS)
                .u16(77),
            token: ITERATOR,
            storage: 9,
            memory: 13,
            check: |k| matches!(k, ExprKind::Iterator { end: 77, .. }),
        },
        Case {
            bytes: b().t(STRUCT_CMP_EQ).obj(1).cat(&ivar(2)).cat(&ivar(3)),
            token: STRUCT_CMP_EQ,
            storage: 15,
            memory: 27,
            check: |k| matches!(k, ExprKind::StructCmp { .. }),
        },
        Case {
            bytes: b().t(STRUCT_CMP_NE).obj(1).t(NO_OBJECT).t(NO_OBJECT),
            token: STRUCT_CMP_NE,
            storage: 7,
            memory: 11,
            check: |k| matches!(k, ExprKind::StructCmp { .. }),
        },
        Case {
            bytes: b().t(STRUCT_MEMBER).obj(1).obj(2).t(0).t(1).cat(&lvar(3)),
            token: STRUCT_MEMBER,
            storage: 16,
            memory: 28,
            check: |k| {
                matches!(
                    k,
                    ExprKind::StructMember {
                        copy: 0,
                        modified: 1,
                        ..
                    }
                )
            },
        },
        Case {
            bytes: b().t(DYN_ARRAY_LENGTH).cat(&ivar(1)),
            token: DYN_ARRAY_LENGTH,
            storage: 6,
            memory: 10,
            check: |k| matches!(k, ExprKind::DynArrayLength { .. }),
        },
        Case {
            bytes: b()
                .t(DYN_ARRAY_INSERT)
                .cat(&ivar(1))
                .cat(&zero())
                .t(INT_ONE)
                .t(END_FUNCTION_PARMS),
            token: DYN_ARRAY_INSERT,
            storage: 9,
            memory: 13,
            check: |k| matches!(k, ExprKind::DynArrayInsert { .. }),
        },
        Case {
            bytes: b()
                .t(DYN_ARRAY_REMOVE)
                .cat(&ivar(1))
                .cat(&zero())
                .t(INT_ONE)
                .t(END_FUNCTION_PARMS),
            token: DYN_ARRAY_REMOVE,
            storage: 9,
            memory: 13,
            check: |k| matches!(k, ExprKind::DynArrayRemove { .. }),
        },
        Case {
            bytes: b()
                .t(DYN_ARRAY_ADD)
                .cat(&ivar(1))
                .t(INT_ONE)
                .t(END_FUNCTION_PARMS),
            token: DYN_ARRAY_ADD,
            storage: 8,
            memory: 12,
            check: |k| matches!(k, ExprKind::DynArrayAdd { .. }),
        },
        Case {
            bytes: b()
                .t(DYN_ARRAY_ADD_ITEM)
                .cat(&ivar(1))
                .u16(2)
                .cat(&zero())
                .t(END_FUNCTION_PARMS),
            token: DYN_ARRAY_ADD_ITEM,
            storage: 10,
            memory: 14,
            check: |k| matches!(k, ExprKind::DynArrayAddItem { skip: 2, .. }),
        },
        Case {
            bytes: b()
                .t(DYN_ARRAY_REMOVE_ITEM)
                .cat(&ivar(1))
                .u16(2)
                .cat(&zero())
                .t(END_FUNCTION_PARMS),
            token: DYN_ARRAY_REMOVE_ITEM,
            storage: 10,
            memory: 14,
            check: |k| matches!(k, ExprKind::DynArrayRemoveItem { .. }),
        },
        Case {
            bytes: b()
                .t(DYN_ARRAY_INSERT_ITEM)
                .cat(&ivar(1))
                .u16(3)
                .cat(&zero())
                .t(INT_ONE)
                .t(END_FUNCTION_PARMS),
            token: DYN_ARRAY_INSERT_ITEM,
            storage: 11,
            memory: 15,
            check: |k| matches!(k, ExprKind::DynArrayInsertItem { skip: 3, .. }),
        },
        Case {
            bytes: b()
                .t(DYN_ARRAY_FIND)
                .cat(&ivar(1))
                .u16(2)
                .cat(&zero())
                .t(END_FUNCTION_PARMS),
            token: DYN_ARRAY_FIND,
            storage: 10,
            memory: 14,
            check: |k| matches!(k, ExprKind::DynArrayFind { .. }),
        },
        Case {
            bytes: b()
                .t(DYN_ARRAY_FIND_STRUCT)
                .cat(&ivar(1))
                .u16(11)
                .t(NAME_CONST)
                .name(2, 0)
                .cat(&zero())
                .t(END_FUNCTION_PARMS),
            token: DYN_ARRAY_FIND_STRUCT,
            storage: 19,
            memory: 23,
            check: |k| matches!(k, ExprKind::DynArrayFindStruct { member, .. } if member.token == NAME_CONST),
        },
        Case {
            bytes: b()
                .t(DYN_ARRAY_SORT)
                .cat(&ivar(1))
                .u16(18)
                .t(DELEGATE_PROPERTY)
                .name(2, 0)
                .obj(0)
                .t(END_FUNCTION_PARMS),
            token: DYN_ARRAY_SORT,
            storage: 22,
            memory: 30,
            check: |k| matches!(k, ExprKind::DynArraySort { .. }),
        },
        Case {
            bytes: b()
                .t(DYN_ARRAY_ITERATOR)
                .cat(&ivar(1))
                .cat(&lvar(2))
                .t(0)
                .t(EMPTY_PARM_VALUE)
                .u16(99),
            token: DYN_ARRAY_ITERATOR,
            storage: 15,
            memory: 23,
            check: |k| {
                matches!(
                    k,
                    ExprKind::DynArrayIterator {
                        has_index: 0,
                        end: 99,
                        ..
                    }
                )
            },
        },
        Case {
            bytes: b().t(DEBUG_INFO).i32(100).i32(12).i32(3).t(0x0F),
            token: DEBUG_INFO,
            storage: 14,
            memory: 14,
            check: |k| {
                matches!(
                    k,
                    ExprKind::DebugInfo {
                        version: 100,
                        line: 12,
                        pos: 3,
                        opcode: 0x0F
                    }
                )
            },
        },
        Case {
            bytes: b().t(DELEGATE_PROPERTY).name(2, 0).obj(3),
            token: DELEGATE_PROPERTY,
            storage: 13,
            memory: 17,
            check: |k| matches!(k, ExprKind::DelegateProperty { property, .. } if property.0 == 3),
        },
        Case {
            bytes: b().t(INSTANCE_DELEGATE).name(2, 0),
            token: INSTANCE_DELEGATE,
            storage: 9,
            memory: 9,
            check: |k| matches!(k, ExprKind::InstanceDelegate { .. }),
        },
        Case {
            bytes: b()
                .t(CONDITIONAL)
                .t(TRUE)
                .u16(1)
                .cat(&zero())
                .u16(1)
                .t(INT_ONE),
            token: CONDITIONAL,
            storage: 8,
            memory: 8,
            check: |k| {
                matches!(
                    k,
                    ExprKind::Conditional {
                        skip_true: 1,
                        skip_false: 1,
                        ..
                    }
                )
            },
        },
        Case {
            bytes: b()
                .t(DEFAULT_PARM_VALUE)
                .u16(2)
                .cat(&zero())
                .t(END_PARM_VALUE),
            token: DEFAULT_PARM_VALUE,
            storage: 5,
            memory: 5,
            check: |k| matches!(k, ExprKind::DefaultParmValue { size: 2, .. }),
        },
    ];
    for t in [NOT_EQUAL_DEL_DEL, EQUAL_EQUAL_DEL_FUNC, NOT_EQUAL_DEL_FUNC] {
        v.push(Case {
            bytes: b()
                .t(t)
                .cat(&ivar(1))
                .t(INSTANCE_DELEGATE)
                .name(2, 0)
                .t(END_FUNCTION_PARMS),
            token: t,
            storage: 16,
            memory: 20,
            check: |k| matches!(k, ExprKind::DelegateCompare { args } if args.len() == 2),
        });
    }
    for (t, check) in [
        (
            STOP,
            (|k: &ExprKind| matches!(k, ExprKind::Stop)) as fn(&ExprKind) -> bool,
        ),
        (NOTHING, |k| matches!(k, ExprKind::Nothing)),
        (END_OF_SCRIPT, |k| matches!(k, ExprKind::EndOfScript)),
        (INT_ZERO, |k| matches!(k, ExprKind::IntZero)),
        (INT_ONE, |k| matches!(k, ExprKind::IntOne)),
        (TRUE, |k| matches!(k, ExprKind::True)),
        (FALSE, |k| matches!(k, ExprKind::False)),
        (NO_OBJECT, |k| matches!(k, ExprKind::NoObject)),
        (SELF, |k| matches!(k, ExprKind::SelfRef)),
        (EMPTY_DELEGATE, |k| matches!(k, ExprKind::EmptyDelegate)),
        (EMPTY_PARM_VALUE, |k| matches!(k, ExprKind::EmptyParmValue)),
        (ITERATOR_POP, |k| matches!(k, ExprKind::IteratorPop)),
        (ITERATOR_NEXT, |k| matches!(k, ExprKind::IteratorNext)),
        (END_FUNCTION_PARMS, |k| {
            matches!(k, ExprKind::EndFunctionParms)
        }),
        (END_PARM_VALUE, |k| matches!(k, ExprKind::EndParmValue)),
    ] {
        v.push(Case {
            bytes: b().t(t),
            token: t,
            storage: 1,
            memory: 1,
            check,
        });
    }
    v
}

#[test]
fn every_token_encoding_decodes_with_both_sizes() {
    let cases = cases();
    let mut tokens = std::collections::BTreeSet::new();
    for c in &cases {
        let s = one(&c.bytes);
        let e = &s.statements[0];
        assert_eq!(e.token, c.token, "{}", token_name(c.token));
        assert_eq!(e.size, c.storage, "storage size of {}", token_name(c.token));
        assert_eq!(
            e.mem_size,
            c.memory,
            "memory size of {}",
            token_name(c.token)
        );
        assert_eq!(s.memory_size, c.memory);
        assert!(
            (c.check)(&e.kind),
            "operands of {}: {:#?}",
            token_name(c.token),
            e.kind
        );
        // With storage widths memory equals storage.
        let st = decode(&c.bytes.0, Layout::STORAGE).unwrap();
        assert_eq!(st.memory_size, c.storage, "{}", token_name(c.token));
        // Children lie inside their parent, in order, in both coordinate systems.
        e.walk(&mut |x, _| {
            let mut pos = x.offset + 1;
            let mut mem = x.mem_offset + 1;
            for k in x.children() {
                assert!(k.offset >= pos && k.end() <= x.end());
                assert!(k.mem_offset >= mem && k.mem_end() <= x.mem_end());
                pos = k.end();
                mem = k.mem_end();
            }
        });
        if c.token < token::EXTENDED_NATIVE {
            tokens.insert(c.token);
        }
    }
    // Every named expression token below 0x60 has a case.
    for t in 0u8..0x60 {
        if token_name(t) != "Unknown" {
            assert!(
                tokens.contains(&t),
                "no encoding case for {}",
                token_name(t)
            );
        }
    }
}

#[test]
fn unknown_tokens_are_rejected() {
    for t in 0u8..0x60 {
        if token_name(t) == "Unknown" {
            let r = decode(&[t, 0, 0, 0, 0, 0, 0, 0, 0], Layout::SHIPPED);
            assert!(
                matches!(r, Err(BytecodeError::UnknownToken { token, offset: 0 }) if token == t),
                "{t:#04x}: {r:?}"
            );
        }
    }
    // The gaps of this build: 0x2B, 0x4C-0x50, 0x5B-0x5F.
    for t in [
        0x2B, 0x4C, 0x4D, 0x4E, 0x4F, 0x50, 0x5B, 0x5C, 0x5D, 0x5E, 0x5F,
    ] {
        assert_eq!(token_name(t), "Unknown");
    }
    assert_eq!(token_name(0x60), "Native");
    assert_eq!(token_name(0xFF), "Native");
}

#[test]
fn memory_offsets_follow_object_width() {
    // Let(LocalVariable, FinalFunction(ObjectConst, IntZero)); Jump 0; EndOfScript
    let code = b()
        .t(token::LET)
        .cat(&lvar(1))
        .t(token::FINAL_FUNCTION)
        .obj(2)
        .t(token::OBJECT_CONST)
        .obj(3)
        .t(token::INT_ZERO)
        .t(token::END_FUNCTION_PARMS)
        .t(token::JUMP)
        .u16(0)
        .t(token::END_OF_SCRIPT);
    let s = decode(&code.0, Layout::SHIPPED).unwrap();
    assert_eq!(s.storage_size, 22);
    assert_eq!(s.memory_size, 34);
    let offs: Vec<(usize, usize)> = s
        .statements
        .iter()
        .map(|e| (e.offset, e.mem_offset))
        .collect();
    assert_eq!(offs, vec![(0, 0), (18, 30), (21, 33)]);
    let ExprKind::Let { value, .. } = &s.statements[0].kind else {
        panic!()
    };
    let ExprKind::FinalFunction { args, .. } = &value.kind else {
        panic!()
    };
    assert_eq!((value.offset, value.mem_offset), (6, 10));
    assert_eq!((args[0].offset, args[0].mem_offset), (11, 19));
    assert_eq!((args[1].offset, args[1].mem_offset), (16, 28));
    assert_eq!(s.storage_offset_of(30), Some(18));
    assert_eq!(s.storage_offset_of(34), Some(22));
    assert_eq!(s.storage_offset_of(31), None);
    assert!(s.ends_with_end_of_script());
    let v = s.validate();
    assert!(v.is_clean());
    assert_eq!(v.targets, 1);
    assert_eq!(v.targets_on_statements, 1);
    assert_eq!(v.tokens, 7);
    assert_eq!(v.max_depth, 2);
    // A 32-bit layout moves everything after the first object reference.
    let narrow = Layout {
        object_ref_memory: 4,
        name_memory: 8,
    };
    assert_eq!(decode(&code.0, narrow).unwrap().memory_size, 22);
}

#[test]
fn validation_accepts_correct_targets_and_skips() {
    use token::*;
    // Build statement by statement, computing memory offsets.
    let s0 = b().t(SWITCH).obj(1).t(4).cat(&lvar(1)); // m0, 19
    let jump_end = |t: u16| b().t(JUMP).u16(t); // m23, 3
    let case_default = b().t(CASE).u16(0xFFFF); // m26, 3
    // m29: JumpIfNot(target, AndAnd(BoolVariable ivar, Skip(size+1, BoolVariable ivar)))
    // native 0x82 args: BoolVariable (10) , Skip(4+10=... ) end
    let cond = b()
        .t(0x82)
        .t(BOOL_VARIABLE)
        .cat(&ivar(1))
        .t(SKIP)
        .u16(11) // BoolVariable ivar (10 mem) + EndFunctionParms (1)
        .t(BOOL_VARIABLE)
        .cat(&ivar(2))
        .t(END_FUNCTION_PARMS);
    // 1 + 10 + 3 + 10 + 1 = 25 memory bytes
    let jin = |t: u16| b().t(JUMP_IF_NOT).u16(t).cat(&cond); // m29, 28
    // m57: Context(ivar 1).ivar 2 statement as a call: Context(ivar1, skip=11, VirtualFunction)
    let ctx_call = b()
        .t(CONTEXT)
        .cat(&ivar(1))
        .u16(11)
        .obj(0)
        .t(0)
        .t(VIRTUAL_FUNCTION)
        .name(3, 0)
        .t(INT_ZERO)
        .t(END_FUNCTION_PARMS); // 1 + 9 + 2 + 8 + 1 + 11 = 32 → ends m89
    // m89: Let(lvar, Conditional(True, 1, IntZero, 1, IntOne)) = 1 + 9 + 8 = 18 → m107
    let cond_let = b()
        .t(LET)
        .cat(&lvar(1))
        .t(CONDITIONAL)
        .t(TRUE)
        .u16(1)
        .t(INT_ZERO)
        .u16(1)
        .t(INT_ONE);
    // m107: DynArrayAddItem(ivar, skip=2, IntZero) EndFunctionParms = 1 + 9 + 2 + 1 + 1 = 14 → m121
    let add_item = b()
        .t(DYN_ARRAY_ADD_ITEM)
        .cat(&ivar(4))
        .u16(2)
        .t(INT_ZERO)
        .t(END_FUNCTION_PARMS);
    // m121: Iterator(Context(ivar, skip→past loop, FinalFunction ...), end=m?)
    //   Context: 1 + 9 + 2 + 8 + 1 + FinalFunction(1 + 8 + 9 + 1 = 19) = 40; Iterator = 1 + 40 + 2 = 43 → m164
    //   body m164: IteratorNext (1) → m165: IteratorPop (1) → m166
    //   loop end offset = m165 (the IteratorPop); context skip lands at m166.
    //   Context expr starts at m121 + 1 + 1 + 9 + 2 + 8 + 1 = m143 → skip = 166 - 143 = 23
    let iter = b()
        .t(ITERATOR)
        .t(CONTEXT)
        .cat(&ivar(5))
        .u16(23)
        .obj(0)
        .t(0)
        .t(FINAL_FUNCTION)
        .obj(6)
        .cat(&lvar(7))
        .t(END_FUNCTION_PARMS)
        .u16(165)
        .t(ITERATOR_NEXT)
        .t(ITERATOR_POP);
    // m166: Context on an object whose skip equals the object's size (the `Outer` quirk).
    let outer_ctx = b()
        .t(CONTEXT)
        .cat(&ivar(8))
        .u16(9)
        .obj(0)
        .t(0)
        .t(VIRTUAL_FUNCTION)
        .name(3, 0)
        .t(END_FUNCTION_PARMS); // 1 + 9 + 2 + 8 + 1 + 10 = 31 → m197
    // m197: DefaultParmValue(size 2, IntZero, EndParmValue) = 5 → m202
    let dpv = b()
        .t(DEFAULT_PARM_VALUE)
        .u16(2)
        .t(INT_ZERO)
        .t(END_PARM_VALUE);
    // m202: Stop, m203: LabelTable (Begin=m0, Loop=m29; 37 bytes), m240: Return(Nothing),
    // m242: EndOfScript
    let labels = b()
        .t(STOP)
        .t(LABEL_TABLE)
        .name(5, 0)
        .u32(0)
        .name(6, 0)
        .u32(29)
        .name(0, 0)
        .u32(LABEL_TABLE_END);
    let tail = b().t(RETURN).t(NOTHING).t(END_OF_SCRIPT);
    let code = |end: u16, case_next: u16| {
        b().cat(&s0)
            .cat(&b().t(CASE).u16(case_next).t(INT_ZERO))
            .cat(&jump_end(end))
            .cat(&case_default)
            .cat(&jin(end))
            .cat(&ctx_call)
            .cat(&cond_let)
            .cat(&add_item)
            .cat(&iter)
            .cat(&outer_ctx)
            .cat(&dpv)
            .cat(&labels)
            .cat(&tail)
    };
    let good = code(57, 26);
    let s = decode(&good.0, Layout::SHIPPED).unwrap();
    assert_eq!(s.storage_size, good.len());
    let starts: Vec<usize> = s.statements.iter().map(|e| e.mem_offset).collect();
    assert_eq!(
        starts,
        vec![
            0, 19, 23, 26, 29, 57, 89, 107, 121, 164, 165, 166, 197, 202, 203, 240, 242
        ]
    );
    let v = s.validate();
    assert!(v.is_clean(), "{v:#?}");
    // Jump, JumpIfNot, Case, Iterator end, two labels.
    assert_eq!(v.targets, 6);
    assert_eq!(v.targets_on_statements, 6);
    // Skip, ctx_call, Conditional x2, AddItem, iterator context, outer ctx, DefaultParmValue.
    assert_eq!(v.skips, 8);
    assert_eq!(v.loop_context_skips, 1);
    assert_eq!(v.object_size_context_skips.len(), 1);
    assert!(s.ends_with_end_of_script());
    // Label table entries.
    let lt = s
        .statements
        .iter()
        .find(|e| e.token == token::LABEL_TABLE)
        .unwrap();
    assert_eq!(lt.mem_offset, 203);
    let ExprKind::LabelTable { labels, terminator } = &lt.kind else {
        panic!()
    };
    assert_eq!(labels.len(), 2);
    assert_eq!(terminator.index, 0);

    // A jump into the middle of a token's operands is reported (m58 starts the
    // context's object expression, m59 is inside its object reference).
    let inner = decode(&code(58, 26).0, Layout::SHIPPED).unwrap().validate();
    assert!(inner.bad_targets.is_empty());
    assert_eq!(inner.targets_on_statements, 4);
    let bad = code(59, 26);
    let v = decode(&bad.0, Layout::SHIPPED).unwrap().validate();
    assert_eq!(v.bad_targets.len(), 2, "{v:#?}");
    assert!(v.bad_targets.iter().all(|t| t.target == 59));
    let bad = code(57, 27);
    let v = decode(&bad.0, Layout::SHIPPED).unwrap().validate();
    assert_eq!(v.bad_targets.len(), 1);
    assert_eq!(v.bad_targets[0].token, token::CASE);
}

#[test]
fn validation_reports_wrong_skips() {
    use token::*;
    // Context skip off by one.
    let c = b()
        .t(CONTEXT)
        .cat(&ivar(1))
        .u16(10)
        .obj(0)
        .t(0)
        .cat(&ivar(2));
    let v = one(&c).validate();
    assert_eq!(v.bad_skips.len(), 1);
    assert_eq!(v.bad_skips[0].expected, 9);
    assert_eq!(v.bad_skips[0].skip, 10);
    // Skip that is not the last argument has no rule: always reported.
    let s = b()
        .t(0x82)
        .t(SKIP)
        .u16(2)
        .t(TRUE)
        .t(TRUE)
        .t(END_FUNCTION_PARMS);
    assert_eq!(one(&s).validate().bad_skips.len(), 1);
    // Skip as last argument must cover its expression plus EndFunctionParms.
    let s = b()
        .t(0x82)
        .t(TRUE)
        .t(SKIP)
        .u16(1)
        .t(TRUE)
        .t(END_FUNCTION_PARMS);
    let v = one(&s).validate();
    assert_eq!(v.bad_skips.len(), 1);
    assert_eq!(v.bad_skips[0].expected, 2);
    // Conditional with swapped skips.
    let s = b()
        .t(CONDITIONAL)
        .t(TRUE)
        .u16(5)
        .cat(&ivar(1))
        .u16(9)
        .t(INT_ZERO);
    assert_eq!(one(&s).validate().bad_skips.len(), 2);
    // DefaultParmValue that forgets the EndParmValue byte in its size.
    let s = b()
        .t(DEFAULT_PARM_VALUE)
        .u16(1)
        .t(INT_ZERO)
        .t(END_PARM_VALUE);
    assert_eq!(one(&s).validate().bad_skips.len(), 1);
    // DynArrayFind whose skip omits the closing EndFunctionParms.
    let s = b()
        .t(DYN_ARRAY_FIND)
        .cat(&ivar(1))
        .u16(1)
        .t(INT_ZERO)
        .t(END_FUNCTION_PARMS);
    assert_eq!(one(&s).validate().bad_skips.len(), 1);
}

#[test]
fn structural_errors_are_reported() {
    use token::*;
    // Terminators where an operand is required.
    for code in [
        b().t(RETURN).t(END_FUNCTION_PARMS),
        b().t(LET).cat(&lvar(1)).t(END_PARM_VALUE),
        b().t(JUMP_IF_NOT).u16(0).t(END_FUNCTION_PARMS),
    ] {
        assert!(matches!(
            decode(&code.0, Layout::SHIPPED),
            Err(BytecodeError::UnexpectedTerminator { .. })
        ));
    }
    // DefaultParmValue must end with EndParmValue.
    let r = decode(
        &b().t(DEFAULT_PARM_VALUE).u16(1).t(INT_ZERO).t(NOTHING).0,
        Layout::SHIPPED,
    );
    assert!(
        matches!(r, Err(BytecodeError::Malformed { offset: 4, .. })),
        "{r:?}"
    );
    // Dynamic-array operations must end with EndFunctionParms.
    let r = decode(
        &b().t(DYN_ARRAY_ADD).cat(&ivar(1)).t(INT_ONE).t(NOTHING).0,
        Layout::SHIPPED,
    );
    assert!(
        matches!(r, Err(BytecodeError::Malformed { offset: 7, .. })),
        "{r:?}"
    );
    // Unterminated strings.
    assert!(matches!(
        decode(&b().t(STRING_CONST).raw(b"abc").0, Layout::SHIPPED),
        Err(BytecodeError::Malformed { .. })
    ));
    assert!(matches!(
        decode(
            &b().t(UNICODE_STRING_CONST).u16(0x41).t(0).0,
            Layout::SHIPPED
        ),
        Err(BytecodeError::Malformed { .. })
    ));
    // Argument list without EndFunctionParms.
    assert!(matches!(
        decode(
            &b().t(VIRTUAL_FUNCTION).name(1, 0).t(INT_ZERO).0,
            Layout::SHIPPED
        ),
        Err(BytecodeError::Truncated { .. })
    ));
    // Label table without terminator.
    assert!(matches!(
        decode(&b().t(LABEL_TABLE).name(1, 0).u32(0).0, Layout::SHIPPED),
        Err(BytecodeError::Truncated { .. })
    ));
    // Empty input is an empty script.
    let s = decode(&[], Layout::SHIPPED).unwrap();
    assert!(s.statements.is_empty());
    assert!(!s.ends_with_end_of_script());
    assert!(s.validate().is_clean());
}

#[test]
fn nesting_limit_is_enforced_without_stack_overflow() {
    use token::*;
    // Exactly MAX_DEPTH levels decode; one more is rejected.
    let mut ok = vec![RETURN; MAX_DEPTH - 1];
    ok.push(INT_ZERO);
    let s = decode(&ok, Layout::SHIPPED).unwrap();
    assert_eq!(s.validate().max_depth, MAX_DEPTH - 1);
    let mut deep = vec![RETURN; MAX_DEPTH];
    deep.push(INT_ZERO);
    assert!(matches!(
        decode(&deep, Layout::SHIPPED),
        Err(BytecodeError::TooDeep { .. })
    ));
    // Far deeper hostile nesting in several shapes.
    for t in [RETURN, BOOL_VARIABLE, 0x81, LET, GOTO_LABEL] {
        let v = vec![t; 100_000];
        assert!(decode(&v, Layout::SHIPPED).is_err());
    }
}

/// Deterministic pseudo-random bytes (xorshift).
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
}

fn exercise(s: &Script) {
    let v = s.validate();
    let _ = s.references();
    let _ = s.object_operands();
    let _ = s.token_boundaries();
    assert!(v.tokens >= s.statements.len());
    let mut n = 0usize;
    s.walk(&mut |e, _| {
        n += 1;
        assert!(e.size >= 1 && e.mem_size >= e.size);
        assert!(e.end() <= s.storage_size && e.mem_end() <= s.memory_size);
    });
    assert_eq!(n, v.tokens);
    for st in s.statements.iter().take(8) {
        assert_eq!(s.storage_offset_of(st.mem_offset), Some(st.offset));
    }
}

#[test]
fn hostile_truncation_and_bit_flips_never_panic() {
    let mut all = b();
    for c in cases() {
        all = all.cat(&c.bytes);
    }
    all = all.t(token::END_OF_SCRIPT);
    let full = decode(&all.0, Layout::SHIPPED).unwrap();
    exercise(&full);
    // Every prefix: either an error or a script that consumed the prefix exactly.
    for cut in 0..all.len() {
        if let Ok(s) = decode(&all.0[..cut], Layout::SHIPPED) {
            assert_eq!(s.storage_size, cut);
            exercise(&s);
        }
    }
    // Every byte replaced by a set of hostile values.
    for i in 0..all.len() {
        for v in [0x00u8, 0x16, 0x15, 0x2B, 0x5F, 0x60, 0x6F, 0x70, 0xFF] {
            let mut m = all.0.clone();
            m[i] = v;
            if let Ok(s) = decode(&m, Layout::SHIPPED) {
                assert_eq!(s.storage_size, m.len());
                exercise(&s);
            }
        }
    }
}

#[test]
fn random_streams_never_panic() {
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    let mut ok = 0;
    for round in 0..20_000 {
        let len = (rng.next() % 96) as usize;
        let mut v = Vec::with_capacity(len);
        for _ in 0..len {
            // Bias towards valid tokens and small operands.
            let r = rng.next();
            v.push(if round % 2 == 0 {
                (r % 0x5B) as u8
            } else {
                r as u8
            });
        }
        if let Ok(s) = decode(&v, Layout::SHIPPED) {
            ok += 1;
            assert_eq!(s.storage_size, v.len());
            exercise(&s);
        }
    }
    assert!(ok > 0);
}

#[test]
fn references_count_calls_and_constants() {
    use token::*;
    let code = b()
        .t(FINAL_FUNCTION)
        .obj(-5)
        .t(VIRTUAL_FUNCTION)
        .name(3, 0)
        .t(FLOAT_CONST)
        .f32(2.5)
        .t(END_FUNCTION_PARMS)
        .t(0x61)
        .t(0x15)
        .t(INT_CONST)
        .i32(-7)
        .t(VECTOR_CONST)
        .f32(2.5)
        .f32(0.0)
        .f32(1.0)
        .t(END_FUNCTION_PARMS)
        .t(INT_CONST_BYTE)
        .t(9)
        .t(NAME_CONST)
        .name(4, 0)
        .t(END_FUNCTION_PARMS)
        .t(GLOBAL_FUNCTION)
        .name(3, 0)
        .t(INT_ONE)
        .t(END_FUNCTION_PARMS)
        .t(END_OF_SCRIPT);
    let s = decode(&code.0, Layout::SHIPPED).unwrap();
    let r = s.references();
    assert_eq!(r.final_functions.get(&-5), Some(&1));
    assert_eq!(r.virtual_functions.get(&(3, 0)), Some(&1));
    assert_eq!(r.global_functions.get(&(3, 0)), Some(&1));
    assert_eq!(r.natives.get(&0x115), Some(&1));
    assert_eq!(r.floats.get(&2.5f32.to_bits()), Some(&2));
    assert_eq!(r.floats.get(&1.0f32.to_bits()), Some(&1));
    assert_eq!(r.ints.get(&-7), Some(&1));
    assert_eq!(r.ints.get(&9), Some(&1));
    assert_eq!(r.ints.get(&1), Some(&1));
    assert_eq!(r.names.get(&(4, 0)), Some(&1));
    let roles: Vec<OperandRole> = s.object_operands().iter().map(|o| o.role).collect();
    assert_eq!(roles, vec![OperandRole::FinalFunction]);
}

#[test]
fn operand_roles_accept_the_expected_classes() {
    use OperandRole as R;
    assert!(R::LocalVariable.accepts("IntProperty"));
    assert!(!R::LocalVariable.accepts("Function"));
    assert!(!R::LocalVariable.accepts("None"));
    assert!(R::ContextRValue.accepts("None"));
    assert!(R::ContextRValue.accepts("Const"));
    assert!(R::SwitchProperty.accepts("None"));
    assert!(!R::SwitchProperty.accepts("Const"));
    assert!(R::FinalFunction.accepts("Function"));
    assert!(!R::FinalFunction.accepts("State"));
    assert!(R::DynamicCastClass.accepts("Class"));
    assert!(R::StructMemberStruct.accepts("ScriptStruct"));
    assert!(R::DelegateFunctionProperty.accepts("DelegateProperty"));
    assert!(!R::DelegateFunctionProperty.accepts("ObjectProperty"));
    assert!(R::ObjectConst.accepts("Texture2D"));
}

// ---------------------------------------------------------------- package level

/// The synthetic script package with bytecode in a function, a state and a class.
fn package_with_bytecode() -> (objects_common::Built, Vec<u8>) {
    use objects_common::{P, core, e, ex, n};
    use token::*;
    let mut payloads = objects_common::payloads();
    // DoIt: Let(X, FinalFunction DoIt(LocalVariable X)); native 129 (X) as a
    // statement; Return(Nothing); EndOfScript.
    let code = b()
        .t(LET)
        .cat(&lvar(e(ex::DOIT_X)))
        .t(FINAL_FUNCTION)
        .obj(e(ex::DOIT))
        .cat(&lvar(e(ex::DOIT_X)))
        .t(END_FUNCTION_PARMS)
        .t(0x81)
        .cat(&lvar(e(ex::DOIT_X)))
        .t(END_FUNCTION_PARMS)
        .t(RETURN)
        .t(NOTHING)
        .t(END_OF_SCRIPT);
    let memory = decode(&code.0, Layout::SHIPPED).unwrap().memory_size as i32;
    let mut p = P::default();
    p.i32(13)
        .none()
        .i32(e(ex::MYVEC))
        .i32(0)
        .i32(0)
        .i32(e(ex::DOIT_X))
        .i32(0)
        .i32(-1)
        .i32(-1)
        .i32(memory)
        .i32(code.len() as i32);
    p.0.bytes(&code.0);
    p.u16(0x81)
        .u8(0)
        .u32(0x0000_2441 | 0x40)
        .u16(3)
        .name("DoIt");
    payloads[ex::DOIT] = p.bytes();
    // Idle: Stop; LabelTable(Idle=0); EndOfScript; LabelTableOffset = 2.
    let state_code = b()
        .t(STOP)
        .t(LABEL_TABLE)
        .name(n("Idle"), 0)
        .u32(0)
        .name(n("None"), 0)
        .u32(LABEL_TABLE_END)
        .t(END_OF_SCRIPT);
    let mut p = P::default();
    p.i32(18)
        .none()
        .i32(e(ex::MAX_THING))
        .structure(0, 0, 0, &state_code.0)
        .u32(0xFFFF_FFFF)
        .u16(2)
        .u32(0x2)
        .i32(1)
        .name("DoIt")
        .i32(e(ex::DOIT));
    payloads[ex::IDLE] = p.bytes();
    // Derived: replication condition at m7 (Health has RepOffset 7).
    let class_code = b()
        .raw(&[NOTHING; 7])
        .t(BOOL_VARIABLE)
        .cat(&ivar(e(ex::HEALTH)))
        .t(END_OF_SCRIPT);
    let class_memory = decode(&class_code.0, Layout::SHIPPED).unwrap().memory_size as i32;
    let mut p = P::default();
    p.i32(3)
        .i32(0)
        .i32(e(ex::BASE))
        .i32(e(ex::SCRIPT_TEXT))
        .i32(e(ex::HEALTH))
        .i32(0)
        .i32(-1)
        .i32(-1)
        .i32(class_memory)
        .i32(class_code.len() as i32);
    p.0.bytes(&class_code.0);
    // UState + UClass tail (as objects_common's class_tail).
    p.u32(0).u16(0xFFFF).u32(0).i32(0);
    p.u32(0x0000_0012).i32(core("Object")).name("Game");
    p.i32(0).i32(0).i32(0);
    p.i32(1).name("Object");
    p.i32(0).i32(0).u32(0).i32(0);
    p.fstring("");
    p.none();
    p.i32(e(ex::DEFAULT_DERIVED));
    payloads[ex::DERIVED] = p.bytes();
    let built = objects_common::build_with(&payloads);
    (built, code.0)
}

#[test]
fn package_coverage_on_synthetic_package() {
    use objects_common::ex;
    let (built, code) = package_with_bytecode();
    let pkg = Package::from_bytes(built.bytes.clone()).unwrap();
    let bc = bytecode::export_bytecode(&pkg, ex::DOIT).unwrap();
    assert_eq!(bc.bytes, &code[..]);
    assert_eq!(bc.function, Some((0x81, 0x0000_2441 | 0x40)));
    assert!(bytecode::export_bytecode(&pkg, ex::HEALTH).is_err());
    let cov = bytecode::package_bytecode_coverage(&pkg, "Synth", Layout::SHIPPED);
    let t = cov.total();
    assert_eq!(t.with_bytecode, 3, "{cov:#?}");
    assert_eq!(t.exact, 3);
    assert_eq!(t.memory_match, 3);
    assert_eq!(t.clean, 3);
    assert_eq!(t.end_of_script, 3);
    assert_eq!(cov.label_table_offsets, 1);
    assert_eq!(cov.label_table_offsets_ok, 1);
    assert_eq!(cov.label_terminators_none, 1);
    assert_eq!(cov.rep_offsets, 1);
    assert_eq!(cov.rep_offsets_ok, 1);
    assert_eq!(cov.operand_violations, 0, "{:?}", cov.failures);
    assert_eq!(cov.local_operands, 3);
    assert_eq!(cov.local_operands_ok, 3);
    assert_eq!(cov.suspicious_statements, 0);
    assert!(cov.failures.is_empty(), "{:?}", cov.failures);
    // Storage widths do not reproduce ScriptBytecodeSize.
    let narrow = bytecode::package_bytecode_coverage(&pkg, "Synth", Layout::STORAGE);
    assert_eq!(
        narrow.total().memory_match,
        1,
        "only the label-only state has no object operand"
    );

    // Natives and arity through a package set on disk.
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("Synth.u");
    std::fs::write(&file, &built.bytes).unwrap();
    let (set, lp) = PackageSet::for_file(&file).unwrap();
    let mut natives = NativeTable::default();
    assert_eq!(natives.add_package(&lp.package, &lp.name), 1);
    assert_eq!(natives.add_package(&lp.package, &lp.name), 0);
    let info = natives.get(0x81).unwrap();
    assert_eq!(info.qualified, "Synth.Derived.DoIt");
    assert!(!info.is_operator());
    assert!(natives.conflicts.is_empty());
    assert_eq!(
        bytecode::function_parameter_count(&lp.package, ex::DOIT).unwrap(),
        1
    );
    let arity = bytecode::check_call_arity(&set, &lp, &natives, Layout::SHIPPED);
    assert_eq!(arity.final_calls, 1);
    assert_eq!(arity.final_ok, 1);
    assert_eq!(arity.native_calls, 1);
    assert_eq!(arity.native_ok, 1, "{arity:#?}");
    assert!(arity.failures.is_empty());
    // An unknown native index is reported as unresolved.
    let empty = NativeTable::default();
    let arity = bytecode::check_call_arity(&set, &lp, &empty, Layout::SHIPPED);
    assert_eq!(arity.native_unresolved, 1);
}

#[test]
fn package_coverage_reports_corrupt_bytecode() {
    use objects_common::{P, e, ex};
    let mut payloads = objects_common::payloads();
    // DoIt with an unknown token and a wrong memory size.
    let code = [token::NOTHING, 0x2B, token::END_OF_SCRIPT];
    let mut p = P::default();
    p.i32(13)
        .none()
        .i32(e(ex::MYVEC))
        .structure(0, 0, e(ex::DOIT_X), &code)
        .u16(0)
        .u8(0)
        .u32(0x40)
        .u16(3)
        .name("DoIt");
    payloads[ex::DOIT] = p.bytes();
    let built = objects_common::build_with(&payloads);
    let pkg = Package::from_bytes(built.bytes).unwrap();
    let cov = bytecode::package_bytecode_coverage(&pkg, "Synth", Layout::SHIPPED);
    let f = cov.kinds.get("Function").unwrap();
    assert_eq!(f.with_bytecode, 1);
    assert_eq!(f.exact, 0);
    assert!(
        cov.failures
            .iter()
            .any(|m| m.contains("unknown token 0x2b")),
        "{:?}",
        cov.failures
    );
}
