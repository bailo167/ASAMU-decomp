//! UnrealScript bytecode (UE3 package version 868): token stream decoding,
//! structural validation and reference summaries.
//!
//! Every `UStruct` (`Function`, `State`, `Class`, `ScriptStruct`) stores
//! `ScriptStorageSize` bytes of bytecode after its header (see
//! [`crate::script::StructHeader`]). The bytes are a sequence of statements;
//! each statement is an expression tree written in prefix order: a one-byte
//! token followed by its operands, which are fixed-size fields and nested
//! expressions.
//!
//! Two sizes matter:
//!
//! - **storage** — the bytes on disk. An object reference is a 4-byte package
//!   index; a name is an 8-byte FName (`i32` index + `i32` number).
//! - **memory** — the layout after loading (`ScriptBytecodeSize`). Object
//!   references become native pointers, so every object operand is wider in
//!   memory ([`Layout::object_ref_memory`]). All code offsets stored in the
//!   bytecode (jump targets, skip sizes, label offsets, `RepOffset`,
//!   `LabelTableOffset`) count **memory** bytes.
//!
//! The decoder tracks both positions for every token, so absolute targets can
//! be checked against token boundaries and relative skips against the memory
//! size of the expressions they skip ([`Script::validate`]).
//!
//! The token numbering was taken from the fixed `GNatives` registrations in the
//! executable (`docs/reverse-engineering/BINARY_ANALYSIS.md` §6) and the
//! operand encodings were fixed empirically: with the rules below every
//! bytecode-carrying export of the shipped script packages decodes to exactly
//! `ScriptStorageSize` bytes, the memory total equals `ScriptBytecodeSize`, and
//! every jump target lands on a token boundary
//! (`docs/reverse-engineering/BYTECODE.md`).
//!
//! **Hygiene:** a decoded token stream is the original game's logic. It may be
//! inspected locally (`asamu-inspect disasm`), but listings must never be
//! committed or quoted. Reference summaries (which functions are called, which
//! constants appear) are fine for evidence work.
//!
//! Hostile-input discipline: every read is bounds-checked, offsets use checked
//! arithmetic, nesting is limited to [`MAX_DEPTH`], and every token consumes at
//! least one byte, so the number of nodes is bounded by the input length.

use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;
use thiserror::Error;

use crate::flags;
use crate::model::{LoadedPackage, MAX_CHILDREN, PackageSet};
use crate::object::{ObjResult, ObjectError, qualified_path};
use crate::package::Package;
use crate::script::{ScriptBody, ScriptKind, decode_script_object};
use crate::types::{FName, PackageIndex};

/// Deepest expression nesting accepted (real data needs far less; see
/// `BYTECODE.md`). Guards the recursive decoder against stack exhaustion.
pub const MAX_DEPTH: usize = 128;

/// Code offset of the entry that terminates a label table.
pub const LABEL_TABLE_END: u32 = 0xFFFF;

/// Largest number of failure descriptions kept by coverage reports.
const MAX_FAILURES: usize = 16;

/// Token numbers (`EExprToken`) of this build. Names follow the UE3
/// convention; the numbers are the `GNatives` slots the handlers are
/// registered at (CONFIRMED from the executable) or, for tokens the VM
/// handles inline, the values observed in the data.
pub mod token {
    /// Local variable (object operand: the property).
    pub const LOCAL_VARIABLE: u8 = 0x00;
    /// Member variable of `self` or of the context object.
    pub const INSTANCE_VARIABLE: u8 = 0x01;
    /// Member of the class default object.
    pub const DEFAULT_VARIABLE: u8 = 0x02;
    /// State-local variable.
    pub const STATE_VARIABLE: u8 = 0x03;
    /// Return from the function with a value expression.
    pub const RETURN: u8 = 0x04;
    /// `switch`.
    pub const SWITCH: u8 = 0x05;
    /// Unconditional jump (absolute memory offset).
    pub const JUMP: u8 = 0x06;
    /// Jump when the condition is false.
    pub const JUMP_IF_NOT: u8 = 0x07;
    /// Stop state code.
    pub const STOP: u8 = 0x08;
    /// `assert`.
    pub const ASSERT: u8 = 0x09;
    /// `case` / `default` of a switch.
    pub const CASE: u8 = 0x0A;
    /// No operation.
    pub const NOTHING: u8 = 0x0B;
    /// State label table.
    pub const LABEL_TABLE: u8 = 0x0C;
    /// `goto` a label (name expression).
    pub const GOTO_LABEL: u8 = 0x0D;
    /// Discard the return value of the following call.
    pub const EAT_RETURN_VALUE: u8 = 0x0E;
    /// Assignment.
    pub const LET: u8 = 0x0F;
    /// Dynamic array element.
    pub const DYN_ARRAY_ELEMENT: u8 = 0x10;
    /// `new`.
    pub const NEW: u8 = 0x11;
    /// `Class.static.` / `Class.default.` context.
    pub const CLASS_CONTEXT: u8 = 0x12;
    /// Metaclass cast (`class<T>(...)`).
    pub const META_CAST: u8 = 0x13;
    /// Boolean assignment.
    pub const LET_BOOL: u8 = 0x14;
    /// End of a default parameter value.
    pub const END_PARM_VALUE: u8 = 0x15;
    /// End of a call's parameter list.
    pub const END_FUNCTION_PARMS: u8 = 0x16;
    /// `self`.
    pub const SELF: u8 = 0x17;
    /// Skippable expression (short-circuit operand).
    pub const SKIP: u8 = 0x18;
    /// Object context (`a.b`).
    pub const CONTEXT: u8 = 0x19;
    /// Static array element.
    pub const ARRAY_ELEMENT: u8 = 0x1A;
    /// Call by name (virtual).
    pub const VIRTUAL_FUNCTION: u8 = 0x1B;
    /// Call of a bound function object (final).
    pub const FINAL_FUNCTION: u8 = 0x1C;
    /// `int` constant.
    pub const INT_CONST: u8 = 0x1D;
    /// `float` constant.
    pub const FLOAT_CONST: u8 = 0x1E;
    /// ANSI string constant.
    pub const STRING_CONST: u8 = 0x1F;
    /// Object constant.
    pub const OBJECT_CONST: u8 = 0x20;
    /// Name constant.
    pub const NAME_CONST: u8 = 0x21;
    /// Rotator constant.
    pub const ROTATION_CONST: u8 = 0x22;
    /// Vector constant.
    pub const VECTOR_CONST: u8 = 0x23;
    /// Byte constant.
    pub const BYTE_CONST: u8 = 0x24;
    /// Integer 0.
    pub const INT_ZERO: u8 = 0x25;
    /// Integer 1.
    pub const INT_ONE: u8 = 0x26;
    /// `true`.
    pub const TRUE: u8 = 0x27;
    /// `false`.
    pub const FALSE: u8 = 0x28;
    /// Native function parameter.
    pub const NATIVE_PARM: u8 = 0x29;
    /// `None` object.
    pub const NO_OBJECT: u8 = 0x2A;
    /// `int` constant stored in one byte.
    pub const INT_CONST_BYTE: u8 = 0x2C;
    /// Boolean variable (wraps the variable expression).
    pub const BOOL_VARIABLE: u8 = 0x2D;
    /// Checked object cast.
    pub const DYNAMIC_CAST: u8 = 0x2E;
    /// `foreach` over an iterator function.
    pub const ITERATOR: u8 = 0x2F;
    /// Leave a `foreach`.
    pub const ITERATOR_POP: u8 = 0x30;
    /// Next `foreach` iteration.
    pub const ITERATOR_NEXT: u8 = 0x31;
    /// Struct `==`.
    pub const STRUCT_CMP_EQ: u8 = 0x32;
    /// Struct `!=`.
    pub const STRUCT_CMP_NE: u8 = 0x33;
    /// UTF-16 string constant.
    pub const UNICODE_STRING_CONST: u8 = 0x34;
    /// Struct member access.
    pub const STRUCT_MEMBER: u8 = 0x35;
    /// Dynamic array `Length`.
    pub const DYN_ARRAY_LENGTH: u8 = 0x36;
    /// Call by name, skipping state overrides (`global.`).
    pub const GLOBAL_FUNCTION: u8 = 0x37;
    /// Primitive conversion (operand byte indexes `GCasts`).
    pub const PRIMITIVE_CAST: u8 = 0x38;
    /// Dynamic array `Insert(index, count)`.
    pub const DYN_ARRAY_INSERT: u8 = 0x39;
    /// Fallback return of a function that returns a value.
    pub const RETURN_NOTHING: u8 = 0x3A;
    /// Delegate `==` delegate.
    pub const EQUAL_EQUAL_DEL_DEL: u8 = 0x3B;
    /// Delegate `!=` delegate.
    pub const NOT_EQUAL_DEL_DEL: u8 = 0x3C;
    /// Delegate `==` function.
    pub const EQUAL_EQUAL_DEL_FUNC: u8 = 0x3D;
    /// Delegate `!=` function.
    pub const NOT_EQUAL_DEL_FUNC: u8 = 0x3E;
    /// `None` delegate.
    pub const EMPTY_DELEGATE: u8 = 0x3F;
    /// Dynamic array `Remove(index, count)`.
    pub const DYN_ARRAY_REMOVE: u8 = 0x40;
    /// Debugger information.
    pub const DEBUG_INFO: u8 = 0x41;
    /// Call through a delegate.
    pub const DELEGATE_FUNCTION: u8 = 0x42;
    /// Function assigned to a delegate.
    pub const DELEGATE_PROPERTY: u8 = 0x43;
    /// Delegate assignment.
    pub const LET_DELEGATE: u8 = 0x44;
    /// Ternary `?:`.
    pub const CONDITIONAL: u8 = 0x45;
    /// Dynamic array `Find(value)`.
    pub const DYN_ARRAY_FIND: u8 = 0x46;
    /// Dynamic array `Find(member, value)` over structs.
    pub const DYN_ARRAY_FIND_STRUCT: u8 = 0x47;
    /// `out` parameter.
    pub const LOCAL_OUT_VARIABLE: u8 = 0x48;
    /// Default value of an optional parameter.
    pub const DEFAULT_PARM_VALUE: u8 = 0x49;
    /// Omitted optional argument.
    pub const EMPTY_PARM_VALUE: u8 = 0x4A;
    /// Function used as a delegate value.
    pub const INSTANCE_DELEGATE: u8 = 0x4B;
    /// Call through an interface variable.
    pub const INTERFACE_CONTEXT: u8 = 0x51;
    /// Object to interface conversion.
    pub const INTERFACE_CAST: u8 = 0x52;
    /// Last token of every script.
    pub const END_OF_SCRIPT: u8 = 0x53;
    /// Dynamic array `Add(count)`.
    pub const DYN_ARRAY_ADD: u8 = 0x54;
    /// Dynamic array `AddItem(item)`.
    pub const DYN_ARRAY_ADD_ITEM: u8 = 0x55;
    /// Dynamic array `RemoveItem(item)`.
    pub const DYN_ARRAY_REMOVE_ITEM: u8 = 0x56;
    /// Dynamic array `InsertItem(index, item)`.
    pub const DYN_ARRAY_INSERT_ITEM: u8 = 0x57;
    /// `foreach` over a dynamic array.
    pub const DYN_ARRAY_ITERATOR: u8 = 0x58;
    /// Dynamic array `Sort(delegate)`.
    pub const DYN_ARRAY_SORT: u8 = 0x59;
    /// Jump unless running in the editor.
    pub const JUMP_IF_NOT_EDITOR_ONLY: u8 = 0x5A;
    /// First two-byte native token (`0x60..=0x6F`: index `(t - 0x60) << 8 | next`).
    pub const EXTENDED_NATIVE: u8 = 0x60;
    /// First one-byte native token (`0x70..=0xFF`: index = token).
    pub const FIRST_NATIVE: u8 = 0x70;
}

/// UE3 name of a token byte (`"Native"` for `>= 0x60`, `"Unknown"` for gaps).
pub fn token_name(t: u8) -> &'static str {
    use token::*;
    match t {
        LOCAL_VARIABLE => "LocalVariable",
        INSTANCE_VARIABLE => "InstanceVariable",
        DEFAULT_VARIABLE => "DefaultVariable",
        STATE_VARIABLE => "StateVariable",
        RETURN => "Return",
        SWITCH => "Switch",
        JUMP => "Jump",
        JUMP_IF_NOT => "JumpIfNot",
        STOP => "Stop",
        ASSERT => "Assert",
        CASE => "Case",
        NOTHING => "Nothing",
        LABEL_TABLE => "LabelTable",
        GOTO_LABEL => "GotoLabel",
        EAT_RETURN_VALUE => "EatReturnValue",
        LET => "Let",
        DYN_ARRAY_ELEMENT => "DynArrayElement",
        NEW => "New",
        CLASS_CONTEXT => "ClassContext",
        META_CAST => "MetaCast",
        LET_BOOL => "LetBool",
        END_PARM_VALUE => "EndParmValue",
        END_FUNCTION_PARMS => "EndFunctionParms",
        SELF => "Self",
        SKIP => "Skip",
        CONTEXT => "Context",
        ARRAY_ELEMENT => "ArrayElement",
        VIRTUAL_FUNCTION => "VirtualFunction",
        FINAL_FUNCTION => "FinalFunction",
        INT_CONST => "IntConst",
        FLOAT_CONST => "FloatConst",
        STRING_CONST => "StringConst",
        OBJECT_CONST => "ObjectConst",
        NAME_CONST => "NameConst",
        ROTATION_CONST => "RotationConst",
        VECTOR_CONST => "VectorConst",
        BYTE_CONST => "ByteConst",
        INT_ZERO => "IntZero",
        INT_ONE => "IntOne",
        TRUE => "True",
        FALSE => "False",
        NATIVE_PARM => "NativeParm",
        NO_OBJECT => "NoObject",
        INT_CONST_BYTE => "IntConstByte",
        BOOL_VARIABLE => "BoolVariable",
        DYNAMIC_CAST => "DynamicCast",
        ITERATOR => "Iterator",
        ITERATOR_POP => "IteratorPop",
        ITERATOR_NEXT => "IteratorNext",
        STRUCT_CMP_EQ => "StructCmpEq",
        STRUCT_CMP_NE => "StructCmpNe",
        UNICODE_STRING_CONST => "UnicodeStringConst",
        STRUCT_MEMBER => "StructMember",
        DYN_ARRAY_LENGTH => "DynArrayLength",
        GLOBAL_FUNCTION => "GlobalFunction",
        PRIMITIVE_CAST => "PrimitiveCast",
        DYN_ARRAY_INSERT => "DynArrayInsert",
        RETURN_NOTHING => "ReturnNothing",
        EQUAL_EQUAL_DEL_DEL => "EqualEqual_DelDel",
        NOT_EQUAL_DEL_DEL => "NotEqual_DelDel",
        EQUAL_EQUAL_DEL_FUNC => "EqualEqual_DelFunc",
        NOT_EQUAL_DEL_FUNC => "NotEqual_DelFunc",
        EMPTY_DELEGATE => "EmptyDelegate",
        DYN_ARRAY_REMOVE => "DynArrayRemove",
        DEBUG_INFO => "DebugInfo",
        DELEGATE_FUNCTION => "DelegateFunction",
        DELEGATE_PROPERTY => "DelegateProperty",
        LET_DELEGATE => "LetDelegate",
        CONDITIONAL => "Conditional",
        DYN_ARRAY_FIND => "DynArrayFind",
        DYN_ARRAY_FIND_STRUCT => "DynArrayFindStruct",
        LOCAL_OUT_VARIABLE => "LocalOutVariable",
        DEFAULT_PARM_VALUE => "DefaultParmValue",
        EMPTY_PARM_VALUE => "EmptyParmValue",
        INSTANCE_DELEGATE => "InstanceDelegate",
        INTERFACE_CONTEXT => "InterfaceContext",
        INTERFACE_CAST => "InterfaceCast",
        END_OF_SCRIPT => "EndOfScript",
        DYN_ARRAY_ADD => "DynArrayAdd",
        DYN_ARRAY_ADD_ITEM => "DynArrayAddItem",
        DYN_ARRAY_REMOVE_ITEM => "DynArrayRemoveItem",
        DYN_ARRAY_INSERT_ITEM => "DynArrayInsertItem",
        DYN_ARRAY_ITERATOR => "DynArrayIterator",
        DYN_ARRAY_SORT => "DynArraySort",
        JUMP_IF_NOT_EDITOR_ONLY => "JumpIfNotEditorOnly",
        t if t >= EXTENDED_NATIVE => "Native",
        _ => "Unknown",
    }
}

/// Memory widths of the operand kinds that differ between storage and memory.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Layout {
    /// Bytes an object reference occupies in memory (storage: 4).
    pub object_ref_memory: usize,
    /// Bytes an FName occupies in memory (storage: 8).
    pub name_memory: usize,
}

impl Layout {
    /// The layout of the shipped packages: 8-byte object pointers (a 64-bit
    /// build), 8-byte names. CONFIRMED: the memory totals equal
    /// `ScriptBytecodeSize` for every bytecode-carrying export.
    pub const SHIPPED: Layout = Layout {
        object_ref_memory: 8,
        name_memory: 8,
    };
    /// Storage widths (memory offsets equal storage offsets).
    pub const STORAGE: Layout = Layout {
        object_ref_memory: 4,
        name_memory: 8,
    };
}

impl Default for Layout {
    fn default() -> Self {
        Layout::SHIPPED
    }
}

/// Errors from decoding a token stream.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum BytecodeError {
    /// An operand runs past the end of the bytecode.
    #[error("bytecode truncated at offset {offset}: need {needed} more bytes, {available} left")]
    Truncated {
        /// Storage offset of the read.
        offset: usize,
        /// Bytes needed.
        needed: usize,
        /// Bytes left.
        available: usize,
    },
    /// A token byte this build does not define.
    #[error("unknown token {token:#04x} at offset {offset}")]
    UnknownToken {
        /// The byte.
        token: u8,
        /// Storage offset.
        offset: usize,
    },
    /// A terminator token where an expression is required.
    #[error("unexpected {name} ({token:#04x}) at offset {offset}")]
    UnexpectedTerminator {
        /// The byte.
        token: u8,
        /// Its name.
        name: &'static str,
        /// Storage offset.
        offset: usize,
    },
    /// A structural rule of a token's operands is violated.
    #[error("malformed {what} at offset {offset}")]
    Malformed {
        /// Description.
        what: &'static str,
        /// Storage offset.
        offset: usize,
    },
    /// Expressions nest deeper than [`MAX_DEPTH`].
    #[error("expressions nested deeper than {limit} at offset {offset}")]
    TooDeep {
        /// The limit.
        limit: usize,
        /// Storage offset.
        offset: usize,
    },
    /// Offset arithmetic overflowed.
    #[error("offset overflow at {offset}")]
    Overflow {
        /// Storage offset.
        offset: usize,
    },
}

/// One entry of a state label table.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Label {
    /// Label name.
    pub name: FName,
    /// Memory offset of the labelled code.
    pub offset: u32,
}

/// Operands shared by `Context` and `ClassContext`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ContextExpr {
    /// Object (or class) expression.
    pub object: Box<Expr>,
    /// Memory bytes to skip when the object is `None`.
    pub skip: u16,
    /// Property holding the r-value (zeroed when the context is `None`).
    pub rvalue_property: PackageIndex,
    /// Size/type byte of the r-value.
    pub rvalue_size: u8,
    /// Expression evaluated in the object's context.
    pub expr: Box<Expr>,
}

/// A decoded expression (one token and its operands).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Expr {
    /// Token byte.
    pub token: u8,
    /// Storage offset of the token byte.
    pub offset: usize,
    /// Storage bytes of the whole expression.
    pub size: usize,
    /// Memory offset of the token byte.
    pub mem_offset: usize,
    /// Memory bytes of the whole expression.
    pub mem_size: usize,
    /// Operands.
    pub kind: ExprKind,
}

/// Token-specific operands.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub enum ExprKind {
    /// `LocalVariable`.
    LocalVariable { property: PackageIndex },
    /// `InstanceVariable`.
    InstanceVariable { property: PackageIndex },
    /// `DefaultVariable`.
    DefaultVariable { property: PackageIndex },
    /// `StateVariable`.
    StateVariable { property: PackageIndex },
    /// `LocalOutVariable`.
    LocalOutVariable { property: PackageIndex },
    /// `NativeParm`.
    NativeParm { property: PackageIndex },
    /// `Return`.
    Return { value: Box<Expr> },
    /// `ReturnNothing`: the return-value property.
    ReturnNothing { property: PackageIndex },
    /// `Switch`.
    Switch {
        property: PackageIndex,
        value_size: u8,
        value: Box<Expr>,
    },
    /// `Case`; `next` is `None` for `default:` (0xFFFF).
    Case {
        next: Option<u16>,
        value: Option<Box<Expr>>,
    },
    /// `Jump`.
    Jump { target: u16 },
    /// `JumpIfNot`.
    JumpIfNot { target: u16, condition: Box<Expr> },
    /// `JumpIfNotEditorOnly`.
    JumpIfNotEditorOnly { target: u16 },
    /// `Stop`.
    Stop,
    /// `Assert`.
    Assert {
        line: u16,
        debug_only: u8,
        condition: Box<Expr>,
    },
    /// `Nothing`.
    Nothing,
    /// `LabelTable`: entries, then the terminator's name (`None`).
    LabelTable {
        labels: Vec<Label>,
        terminator: FName,
    },
    /// `GotoLabel`.
    GotoLabel { label: Box<Expr> },
    /// `EatReturnValue`: the discarded return property, then the call.
    EatReturnValue {
        property: PackageIndex,
        value: Box<Expr>,
    },
    /// `Let`.
    Let { target: Box<Expr>, value: Box<Expr> },
    /// `LetBool`.
    LetBool { target: Box<Expr>, value: Box<Expr> },
    /// `LetDelegate`.
    LetDelegate { target: Box<Expr>, value: Box<Expr> },
    /// `DynArrayElement`.
    DynArrayElement { index: Box<Expr>, array: Box<Expr> },
    /// `ArrayElement`.
    ArrayElement { index: Box<Expr>, array: Box<Expr> },
    /// `New`.
    New {
        outer: Box<Expr>,
        name: Box<Expr>,
        flags: Box<Expr>,
        class: Box<Expr>,
        template: Box<Expr>,
    },
    /// `Context`.
    Context(ContextExpr),
    /// `ClassContext`.
    ClassContext(ContextExpr),
    /// `InterfaceContext`.
    InterfaceContext { value: Box<Expr> },
    /// `MetaCast`.
    MetaCast {
        class: PackageIndex,
        value: Box<Expr>,
    },
    /// `DynamicCast`.
    DynamicCast {
        class: PackageIndex,
        value: Box<Expr>,
    },
    /// `InterfaceCast`.
    InterfaceCast {
        class: PackageIndex,
        value: Box<Expr>,
    },
    /// `PrimitiveCast`: `cast` indexes `GCasts`.
    PrimitiveCast { cast: u8, value: Box<Expr> },
    /// `EndParmValue`.
    EndParmValue,
    /// `EndFunctionParms`.
    EndFunctionParms,
    /// `Self`.
    SelfRef,
    /// `Skip`.
    Skip { skip: u16, value: Box<Expr> },
    /// `VirtualFunction`.
    VirtualFunction { name: FName, args: Vec<Expr> },
    /// `GlobalFunction`.
    GlobalFunction { name: FName, args: Vec<Expr> },
    /// `FinalFunction`.
    FinalFunction {
        function: PackageIndex,
        args: Vec<Expr>,
    },
    /// `DelegateFunction`.
    DelegateFunction {
        local: u8,
        property: PackageIndex,
        name: FName,
        args: Vec<Expr>,
    },
    /// Native call by index (`0x60..=0xFF` tokens).
    NativeFunction { index: u16, args: Vec<Expr> },
    /// Delegate comparisons (`0x3B..=0x3E`).
    DelegateCompare { args: Vec<Expr> },
    /// `IntConst`.
    IntConst { value: i32 },
    /// `FloatConst`.
    FloatConst { value: f32 },
    /// `ByteConst`.
    ByteConst { value: u8 },
    /// `IntConstByte`.
    IntConstByte { value: u8 },
    /// `StringConst` (Latin-1).
    StringConst { value: String },
    /// `UnicodeStringConst`.
    UnicodeStringConst { value: String },
    /// `ObjectConst`.
    ObjectConst { object: PackageIndex },
    /// `NameConst`.
    NameConst { name: FName },
    /// `RotationConst`.
    RotationConst { pitch: i32, yaw: i32, roll: i32 },
    /// `VectorConst`.
    VectorConst { x: f32, y: f32, z: f32 },
    /// `IntZero`.
    IntZero,
    /// `IntOne`.
    IntOne,
    /// `True`.
    True,
    /// `False`.
    False,
    /// `NoObject`.
    NoObject,
    /// `EmptyDelegate`.
    EmptyDelegate,
    /// `EmptyParmValue`.
    EmptyParmValue,
    /// `EndOfScript`.
    EndOfScript,
    /// `BoolVariable`.
    BoolVariable { value: Box<Expr> },
    /// `Iterator`.
    Iterator { iterator: Box<Expr>, end: u16 },
    /// `IteratorPop`.
    IteratorPop,
    /// `IteratorNext`.
    IteratorNext,
    /// `StructCmpEq` / `StructCmpNe`.
    StructCmp {
        struct_: PackageIndex,
        left: Box<Expr>,
        right: Box<Expr>,
    },
    /// `StructMember`.
    StructMember {
        property: PackageIndex,
        struct_: PackageIndex,
        copy: u8,
        modified: u8,
        value: Box<Expr>,
    },
    /// `DynArrayLength`.
    DynArrayLength { array: Box<Expr> },
    /// `DynArrayInsert`.
    DynArrayInsert {
        array: Box<Expr>,
        index: Box<Expr>,
        count: Box<Expr>,
    },
    /// `DynArrayRemove`.
    DynArrayRemove {
        array: Box<Expr>,
        index: Box<Expr>,
        count: Box<Expr>,
    },
    /// `DynArrayAdd`.
    DynArrayAdd { array: Box<Expr>, count: Box<Expr> },
    /// `DynArrayAddItem`.
    DynArrayAddItem {
        array: Box<Expr>,
        skip: u16,
        item: Box<Expr>,
    },
    /// `DynArrayRemoveItem`.
    DynArrayRemoveItem {
        array: Box<Expr>,
        skip: u16,
        item: Box<Expr>,
    },
    /// `DynArrayInsertItem`.
    DynArrayInsertItem {
        array: Box<Expr>,
        skip: u16,
        index: Box<Expr>,
        item: Box<Expr>,
    },
    /// `DynArrayFind`.
    DynArrayFind {
        array: Box<Expr>,
        skip: u16,
        value: Box<Expr>,
    },
    /// `DynArrayFindStruct`.
    DynArrayFindStruct {
        array: Box<Expr>,
        skip: u16,
        member: Box<Expr>,
        value: Box<Expr>,
    },
    /// `DynArraySort`.
    DynArraySort {
        array: Box<Expr>,
        skip: u16,
        comparator: Box<Expr>,
    },
    /// `DynArrayIterator`.
    DynArrayIterator {
        array: Box<Expr>,
        item: Box<Expr>,
        has_index: u8,
        index: Box<Expr>,
        end: u16,
    },
    /// `DebugInfo`.
    DebugInfo {
        version: i32,
        line: i32,
        pos: i32,
        opcode: u8,
    },
    /// `DelegateProperty`.
    DelegateProperty {
        function: FName,
        property: PackageIndex,
    },
    /// `InstanceDelegate`.
    InstanceDelegate { function: FName },
    /// `Conditional`.
    Conditional {
        condition: Box<Expr>,
        skip_true: u16,
        if_true: Box<Expr>,
        skip_false: u16,
        if_false: Box<Expr>,
    },
    /// `DefaultParmValue`: `size` memory bytes of value plus the
    /// terminating `EndParmValue` byte.
    DefaultParmValue { size: u16, value: Box<Expr> },
}

impl Expr {
    /// Storage offset just past the expression.
    pub fn end(&self) -> usize {
        self.offset.saturating_add(self.size)
    }

    /// Memory offset just past the expression.
    pub fn mem_end(&self) -> usize {
        self.mem_offset.saturating_add(self.mem_size)
    }

    /// Direct sub-expressions in stream order.
    pub fn children(&self) -> Vec<&Expr> {
        use ExprKind as K;
        match &self.kind {
            K::Return { value }
            | K::Switch { value, .. }
            | K::EatReturnValue { value, .. }
            | K::InterfaceContext { value }
            | K::MetaCast { value, .. }
            | K::DynamicCast { value, .. }
            | K::InterfaceCast { value, .. }
            | K::PrimitiveCast { value, .. }
            | K::Skip { value, .. }
            | K::BoolVariable { value }
            | K::StructMember { value, .. }
            | K::DefaultParmValue { value, .. } => vec![value],
            K::JumpIfNot { condition, .. } | K::Assert { condition, .. } => vec![condition],
            K::Case { value, .. } => value.iter().map(|b| &**b).collect(),
            K::GotoLabel { label } => vec![label],
            K::Let { target, value }
            | K::LetBool { target, value }
            | K::LetDelegate { target, value } => {
                vec![target, value]
            }
            K::DynArrayElement { index, array } | K::ArrayElement { index, array } => {
                vec![index, array]
            }
            K::New {
                outer,
                name,
                flags,
                class,
                template,
            } => vec![outer, name, flags, class, template],
            K::Context(c) | K::ClassContext(c) => vec![&c.object, &c.expr],
            K::VirtualFunction { args, .. }
            | K::GlobalFunction { args, .. }
            | K::FinalFunction { args, .. }
            | K::DelegateFunction { args, .. }
            | K::NativeFunction { args, .. }
            | K::DelegateCompare { args } => args.iter().collect(),
            K::Iterator { iterator, .. } => vec![iterator],
            K::StructCmp { left, right, .. } => vec![left, right],
            K::DynArrayLength { array } => vec![array],
            K::DynArrayInsert {
                array,
                index,
                count,
            }
            | K::DynArrayRemove {
                array,
                index,
                count,
            } => vec![array, index, count],
            K::DynArrayAdd { array, count } => vec![array, count],
            K::DynArrayAddItem { array, item, .. } | K::DynArrayRemoveItem { array, item, .. } => {
                vec![array, item]
            }
            K::DynArrayInsertItem {
                array, index, item, ..
            } => vec![array, index, item],
            K::DynArrayFind { array, value, .. } => vec![array, value],
            K::DynArrayFindStruct {
                array,
                member,
                value,
                ..
            } => vec![array, member, value],
            K::DynArraySort {
                array, comparator, ..
            } => vec![array, comparator],
            K::DynArrayIterator {
                array, item, index, ..
            } => vec![array, item, index],
            K::Conditional {
                condition,
                if_true,
                if_false,
                ..
            } => vec![condition, if_true, if_false],
            K::LocalVariable { .. }
            | K::InstanceVariable { .. }
            | K::DefaultVariable { .. }
            | K::StateVariable { .. }
            | K::LocalOutVariable { .. }
            | K::NativeParm { .. }
            | K::ReturnNothing { .. }
            | K::Jump { .. }
            | K::JumpIfNotEditorOnly { .. }
            | K::Stop
            | K::Nothing
            | K::LabelTable { .. }
            | K::EndParmValue
            | K::EndFunctionParms
            | K::SelfRef
            | K::IntConst { .. }
            | K::FloatConst { .. }
            | K::ByteConst { .. }
            | K::IntConstByte { .. }
            | K::StringConst { .. }
            | K::UnicodeStringConst { .. }
            | K::ObjectConst { .. }
            | K::NameConst { .. }
            | K::RotationConst { .. }
            | K::VectorConst { .. }
            | K::IntZero
            | K::IntOne
            | K::True
            | K::False
            | K::NoObject
            | K::EmptyDelegate
            | K::EmptyParmValue
            | K::EndOfScript
            | K::IteratorPop
            | K::IteratorNext
            | K::DebugInfo { .. }
            | K::DelegateProperty { .. }
            | K::InstanceDelegate { .. } => Vec::new(),
        }
    }

    /// Visit this expression and every sub-expression in stream (prefix)
    /// order; `depth` is 0 for `self`.
    pub fn walk<'e>(&'e self, f: &mut dyn FnMut(&'e Expr, usize)) {
        // Iterative to keep stack use flat; children pushed in reverse.
        let mut stack: Vec<(&'e Expr, usize)> = vec![(self, 0)];
        while let Some((e, d)) = stack.pop() {
            f(e, d);
            let kids = e.children();
            for k in kids.into_iter().rev() {
                stack.push((k, d.saturating_add(1)));
            }
        }
    }
}

/// A decoded token stream.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Script {
    /// Top-level statements in stream order.
    pub statements: Vec<Expr>,
    /// Storage bytes decoded (= input length on success).
    pub storage_size: usize,
    /// Memory bytes under the layout used.
    pub memory_size: usize,
    /// Layout used for memory offsets.
    pub layout: Layout,
}

/// Bytecode bounds-checked cursor that tracks storage and memory offsets.
struct Cursor<'a> {
    data: &'a [u8],
    pos: usize,
    mem: usize,
    layout: Layout,
}

type BResult<T> = std::result::Result<T, BytecodeError>;

impl<'a> Cursor<'a> {
    fn take(&mut self, n: usize, mem_n: usize) -> BResult<&'a [u8]> {
        let available = self.data.len().saturating_sub(self.pos);
        if n > available {
            return Err(BytecodeError::Truncated {
                offset: self.pos,
                needed: n,
                available,
            });
        }
        let end = self.pos + n; // n <= len - pos
        let out = &self.data[self.pos..end];
        self.pos = end;
        self.mem = self
            .mem
            .checked_add(mem_n)
            .ok_or(BytecodeError::Overflow { offset: self.pos })?;
        Ok(out)
    }

    fn array<const N: usize>(&mut self) -> BResult<[u8; N]> {
        let b = self.take(N, N)?;
        let mut out = [0u8; N];
        out.copy_from_slice(b);
        Ok(out)
    }

    fn peek(&self) -> BResult<u8> {
        self.data
            .get(self.pos)
            .copied()
            .ok_or(BytecodeError::Truncated {
                offset: self.pos,
                needed: 1,
                available: 0,
            })
    }

    fn u8(&mut self) -> BResult<u8> {
        Ok(self.array::<1>()?[0])
    }

    fn u16(&mut self) -> BResult<u16> {
        Ok(u16::from_le_bytes(self.array()?))
    }

    fn i32(&mut self) -> BResult<i32> {
        Ok(i32::from_le_bytes(self.array()?))
    }

    fn u32(&mut self) -> BResult<u32> {
        Ok(u32::from_le_bytes(self.array()?))
    }

    fn f32(&mut self) -> BResult<f32> {
        Ok(f32::from_le_bytes(self.array()?))
    }

    fn object(&mut self) -> BResult<PackageIndex> {
        let b = self.take(4, self.layout.object_ref_memory)?;
        let mut a = [0u8; 4];
        a.copy_from_slice(b);
        Ok(PackageIndex(i32::from_le_bytes(a)))
    }

    fn name(&mut self) -> BResult<FName> {
        let b = self.take(8, self.layout.name_memory)?;
        let mut i = [0u8; 4];
        let mut n = [0u8; 4];
        i.copy_from_slice(&b[..4]);
        n.copy_from_slice(&b[4..8]);
        Ok(FName {
            index: i32::from_le_bytes(i),
            number: i32::from_le_bytes(n),
        })
    }

    fn ansi_string(&mut self) -> BResult<String> {
        let start = self.pos;
        let rest = self.data.get(start..).unwrap_or(&[]);
        let len = rest
            .iter()
            .position(|&b| b == 0)
            .ok_or(BytecodeError::Malformed {
                what: "unterminated StringConst",
                offset: start,
            })?;
        let total = len
            .checked_add(1)
            .ok_or(BytecodeError::Overflow { offset: start })?;
        let b = self.take(total, total)?;
        Ok(b[..len].iter().map(|&c| char::from(c)).collect())
    }

    fn unicode_string(&mut self) -> BResult<String> {
        let start = self.pos;
        let mut units = Vec::new();
        loop {
            let u = self.u16().map_err(|_| BytecodeError::Malformed {
                what: "unterminated UnicodeStringConst",
                offset: start,
            })?;
            if u == 0 {
                break;
            }
            units.push(u);
        }
        Ok(String::from_utf16_lossy(&units))
    }
}

struct Decoder<'a> {
    c: Cursor<'a>,
}

impl Decoder<'_> {
    /// Parse one expression that must not be a terminator.
    fn operand(&mut self, depth: usize) -> BResult<Box<Expr>> {
        let t = self.c.peek()?;
        if t == token::END_FUNCTION_PARMS || t == token::END_PARM_VALUE {
            return Err(BytecodeError::UnexpectedTerminator {
                token: t,
                name: token_name(t),
                offset: self.c.pos,
            });
        }
        Ok(Box::new(self.expr(depth)?))
    }

    /// Parse call arguments up to and including `EndFunctionParms`.
    fn args(&mut self, depth: usize) -> BResult<Vec<Expr>> {
        let mut out = Vec::new();
        loop {
            if self.c.peek()? == token::END_FUNCTION_PARMS {
                self.c.u8()?;
                return Ok(out);
            }
            out.push(*self.operand(depth)?);
        }
    }

    /// Consume the `EndFunctionParms` byte that closes the dynamic-array
    /// operations (CONFIRMED: present after every one of them).
    fn end_parms(&mut self, what: &'static str) -> BResult<()> {
        let at = self.c.pos;
        if self.c.u8()? != token::END_FUNCTION_PARMS {
            return Err(BytecodeError::Malformed { what, offset: at });
        }
        Ok(())
    }

    fn context(&mut self, depth: usize) -> BResult<ContextExpr> {
        let object = self.operand(depth)?;
        let skip = self.c.u16()?;
        let rvalue_property = self.c.object()?;
        let rvalue_size = self.c.u8()?;
        let expr = self.operand(depth)?;
        Ok(ContextExpr {
            object,
            skip,
            rvalue_property,
            rvalue_size,
            expr,
        })
    }

    fn expr(&mut self, depth: usize) -> BResult<Expr> {
        let offset = self.c.pos;
        let mem_offset = self.c.mem;
        if depth >= MAX_DEPTH {
            return Err(BytecodeError::TooDeep {
                limit: MAX_DEPTH,
                offset,
            });
        }
        let d = depth + 1; // depth < MAX_DEPTH
        let t = self.c.u8()?;
        use ExprKind as K;
        use token::*;
        let kind = if t >= FIRST_NATIVE {
            K::NativeFunction {
                index: u16::from(t),
                args: self.args(d)?,
            }
        } else if t >= EXTENDED_NATIVE {
            let low = self.c.u8()?;
            K::NativeFunction {
                index: (u16::from(t - EXTENDED_NATIVE) << 8) | u16::from(low),
                args: self.args(d)?,
            }
        } else {
            match t {
                LOCAL_VARIABLE => K::LocalVariable {
                    property: self.c.object()?,
                },
                INSTANCE_VARIABLE => K::InstanceVariable {
                    property: self.c.object()?,
                },
                DEFAULT_VARIABLE => K::DefaultVariable {
                    property: self.c.object()?,
                },
                STATE_VARIABLE => K::StateVariable {
                    property: self.c.object()?,
                },
                LOCAL_OUT_VARIABLE => K::LocalOutVariable {
                    property: self.c.object()?,
                },
                NATIVE_PARM => K::NativeParm {
                    property: self.c.object()?,
                },
                RETURN => K::Return {
                    value: self.operand(d)?,
                },
                RETURN_NOTHING => K::ReturnNothing {
                    property: self.c.object()?,
                },
                SWITCH => K::Switch {
                    property: self.c.object()?,
                    value_size: self.c.u8()?,
                    value: self.operand(d)?,
                },
                CASE => {
                    let next = self.c.u16()?;
                    if next == 0xFFFF {
                        K::Case {
                            next: None,
                            value: None,
                        }
                    } else {
                        K::Case {
                            next: Some(next),
                            value: Some(self.operand(d)?),
                        }
                    }
                }
                JUMP => K::Jump {
                    target: self.c.u16()?,
                },
                JUMP_IF_NOT => K::JumpIfNot {
                    target: self.c.u16()?,
                    condition: self.operand(d)?,
                },
                JUMP_IF_NOT_EDITOR_ONLY => K::JumpIfNotEditorOnly {
                    target: self.c.u16()?,
                },
                STOP => K::Stop,
                ASSERT => K::Assert {
                    line: self.c.u16()?,
                    debug_only: self.c.u8()?,
                    condition: self.operand(d)?,
                },
                NOTHING => K::Nothing,
                LABEL_TABLE => {
                    let mut labels = Vec::new();
                    // Entries until the terminator, whose offset is 0xFFFF
                    // (its name is `None`; checked by the coverage report).
                    let terminator = loop {
                        let name = self.c.name()?;
                        let offset = self.c.u32()?;
                        if offset == LABEL_TABLE_END {
                            break name;
                        }
                        labels.push(Label { name, offset });
                    };
                    K::LabelTable { labels, terminator }
                }
                GOTO_LABEL => K::GotoLabel {
                    label: self.operand(d)?,
                },
                EAT_RETURN_VALUE => K::EatReturnValue {
                    property: self.c.object()?,
                    value: self.operand(d)?,
                },
                LET => K::Let {
                    target: self.operand(d)?,
                    value: self.operand(d)?,
                },
                LET_BOOL => K::LetBool {
                    target: self.operand(d)?,
                    value: self.operand(d)?,
                },
                LET_DELEGATE => K::LetDelegate {
                    target: self.operand(d)?,
                    value: self.operand(d)?,
                },
                DYN_ARRAY_ELEMENT => K::DynArrayElement {
                    index: self.operand(d)?,
                    array: self.operand(d)?,
                },
                ARRAY_ELEMENT => K::ArrayElement {
                    index: self.operand(d)?,
                    array: self.operand(d)?,
                },
                NEW => K::New {
                    outer: self.operand(d)?,
                    name: self.operand(d)?,
                    flags: self.operand(d)?,
                    class: self.operand(d)?,
                    template: self.operand(d)?,
                },
                CONTEXT => K::Context(self.context(d)?),
                CLASS_CONTEXT => K::ClassContext(self.context(d)?),
                INTERFACE_CONTEXT => K::InterfaceContext {
                    value: self.operand(d)?,
                },
                META_CAST => K::MetaCast {
                    class: self.c.object()?,
                    value: self.operand(d)?,
                },
                DYNAMIC_CAST => K::DynamicCast {
                    class: self.c.object()?,
                    value: self.operand(d)?,
                },
                INTERFACE_CAST => K::InterfaceCast {
                    class: self.c.object()?,
                    value: self.operand(d)?,
                },
                PRIMITIVE_CAST => K::PrimitiveCast {
                    cast: self.c.u8()?,
                    value: self.operand(d)?,
                },
                END_PARM_VALUE => K::EndParmValue,
                END_FUNCTION_PARMS => K::EndFunctionParms,
                SELF => K::SelfRef,
                SKIP => K::Skip {
                    skip: self.c.u16()?,
                    value: self.operand(d)?,
                },
                VIRTUAL_FUNCTION => K::VirtualFunction {
                    name: self.c.name()?,
                    args: self.args(d)?,
                },
                GLOBAL_FUNCTION => K::GlobalFunction {
                    name: self.c.name()?,
                    args: self.args(d)?,
                },
                FINAL_FUNCTION => K::FinalFunction {
                    function: self.c.object()?,
                    args: self.args(d)?,
                },
                DELEGATE_FUNCTION => K::DelegateFunction {
                    local: self.c.u8()?,
                    property: self.c.object()?,
                    name: self.c.name()?,
                    args: self.args(d)?,
                },
                EQUAL_EQUAL_DEL_DEL | NOT_EQUAL_DEL_DEL | EQUAL_EQUAL_DEL_FUNC
                | NOT_EQUAL_DEL_FUNC => K::DelegateCompare {
                    args: self.args(d)?,
                },
                INT_CONST => K::IntConst {
                    value: self.c.i32()?,
                },
                FLOAT_CONST => K::FloatConst {
                    value: self.c.f32()?,
                },
                BYTE_CONST => K::ByteConst {
                    value: self.c.u8()?,
                },
                INT_CONST_BYTE => K::IntConstByte {
                    value: self.c.u8()?,
                },
                STRING_CONST => K::StringConst {
                    value: self.c.ansi_string()?,
                },
                UNICODE_STRING_CONST => K::UnicodeStringConst {
                    value: self.c.unicode_string()?,
                },
                OBJECT_CONST => K::ObjectConst {
                    object: self.c.object()?,
                },
                NAME_CONST => K::NameConst {
                    name: self.c.name()?,
                },
                ROTATION_CONST => K::RotationConst {
                    pitch: self.c.i32()?,
                    yaw: self.c.i32()?,
                    roll: self.c.i32()?,
                },
                VECTOR_CONST => K::VectorConst {
                    x: self.c.f32()?,
                    y: self.c.f32()?,
                    z: self.c.f32()?,
                },
                INT_ZERO => K::IntZero,
                INT_ONE => K::IntOne,
                TRUE => K::True,
                FALSE => K::False,
                NO_OBJECT => K::NoObject,
                EMPTY_DELEGATE => K::EmptyDelegate,
                EMPTY_PARM_VALUE => K::EmptyParmValue,
                END_OF_SCRIPT => K::EndOfScript,
                BOOL_VARIABLE => K::BoolVariable {
                    value: self.operand(d)?,
                },
                ITERATOR => K::Iterator {
                    iterator: self.operand(d)?,
                    end: self.c.u16()?,
                },
                ITERATOR_POP => K::IteratorPop,
                ITERATOR_NEXT => K::IteratorNext,
                STRUCT_CMP_EQ | STRUCT_CMP_NE => K::StructCmp {
                    struct_: self.c.object()?,
                    left: self.operand(d)?,
                    right: self.operand(d)?,
                },
                STRUCT_MEMBER => K::StructMember {
                    property: self.c.object()?,
                    struct_: self.c.object()?,
                    copy: self.c.u8()?,
                    modified: self.c.u8()?,
                    value: self.operand(d)?,
                },
                DYN_ARRAY_LENGTH => K::DynArrayLength {
                    array: self.operand(d)?,
                },
                DYN_ARRAY_INSERT => {
                    let k = K::DynArrayInsert {
                        array: self.operand(d)?,
                        index: self.operand(d)?,
                        count: self.operand(d)?,
                    };
                    self.end_parms("DynArrayInsert without EndFunctionParms")?;
                    k
                }
                DYN_ARRAY_REMOVE => {
                    let k = K::DynArrayRemove {
                        array: self.operand(d)?,
                        index: self.operand(d)?,
                        count: self.operand(d)?,
                    };
                    self.end_parms("DynArrayRemove without EndFunctionParms")?;
                    k
                }
                DYN_ARRAY_ADD => {
                    let k = K::DynArrayAdd {
                        array: self.operand(d)?,
                        count: self.operand(d)?,
                    };
                    self.end_parms("DynArrayAdd without EndFunctionParms")?;
                    k
                }
                DYN_ARRAY_ADD_ITEM => {
                    let k = K::DynArrayAddItem {
                        array: self.operand(d)?,
                        skip: self.c.u16()?,
                        item: self.operand(d)?,
                    };
                    self.end_parms("DynArrayAddItem without EndFunctionParms")?;
                    k
                }
                DYN_ARRAY_REMOVE_ITEM => {
                    let k = K::DynArrayRemoveItem {
                        array: self.operand(d)?,
                        skip: self.c.u16()?,
                        item: self.operand(d)?,
                    };
                    self.end_parms("DynArrayRemoveItem without EndFunctionParms")?;
                    k
                }
                DYN_ARRAY_INSERT_ITEM => {
                    let k = K::DynArrayInsertItem {
                        array: self.operand(d)?,
                        skip: self.c.u16()?,
                        index: self.operand(d)?,
                        item: self.operand(d)?,
                    };
                    self.end_parms("DynArrayInsertItem without EndFunctionParms")?;
                    k
                }
                DYN_ARRAY_FIND => {
                    let k = K::DynArrayFind {
                        array: self.operand(d)?,
                        skip: self.c.u16()?,
                        value: self.operand(d)?,
                    };
                    self.end_parms("DynArrayFind without EndFunctionParms")?;
                    k
                }
                DYN_ARRAY_FIND_STRUCT => {
                    let k = K::DynArrayFindStruct {
                        array: self.operand(d)?,
                        skip: self.c.u16()?,
                        member: self.operand(d)?,
                        value: self.operand(d)?,
                    };
                    self.end_parms("DynArrayFindStruct without EndFunctionParms")?;
                    k
                }
                DYN_ARRAY_SORT => {
                    let k = K::DynArraySort {
                        array: self.operand(d)?,
                        skip: self.c.u16()?,
                        comparator: self.operand(d)?,
                    };
                    self.end_parms("DynArraySort without EndFunctionParms")?;
                    k
                }
                DYN_ARRAY_ITERATOR => K::DynArrayIterator {
                    array: self.operand(d)?,
                    item: self.operand(d)?,
                    has_index: self.c.u8()?,
                    index: self.operand(d)?,
                    end: self.c.u16()?,
                },
                DEBUG_INFO => K::DebugInfo {
                    version: self.c.i32()?,
                    line: self.c.i32()?,
                    pos: self.c.i32()?,
                    opcode: self.c.u8()?,
                },
                DELEGATE_PROPERTY => K::DelegateProperty {
                    function: self.c.name()?,
                    property: self.c.object()?,
                },
                INSTANCE_DELEGATE => K::InstanceDelegate {
                    function: self.c.name()?,
                },
                CONDITIONAL => K::Conditional {
                    condition: self.operand(d)?,
                    skip_true: self.c.u16()?,
                    if_true: self.operand(d)?,
                    skip_false: self.c.u16()?,
                    if_false: self.operand(d)?,
                },
                DEFAULT_PARM_VALUE => {
                    let size = self.c.u16()?;
                    let value = self.operand(d)?;
                    let end_at = self.c.pos;
                    if self.c.u8()? != END_PARM_VALUE {
                        return Err(BytecodeError::Malformed {
                            what: "DefaultParmValue without EndParmValue",
                            offset: end_at,
                        });
                    }
                    K::DefaultParmValue { size, value }
                }
                _ => return Err(BytecodeError::UnknownToken { token: t, offset }),
            }
        };
        Ok(Expr {
            token: t,
            offset,
            size: self.c.pos - offset, // pos only grows
            mem_offset,
            mem_size: self.c.mem - mem_offset,
            kind,
        })
    }
}

/// Decode a whole token stream (the `ScriptStorageSize` bytes of a struct).
///
/// Statements are decoded until the input is exhausted; a successful result
/// has consumed exactly `bytes.len()` bytes.
pub fn decode(bytes: &[u8], layout: Layout) -> Result<Script, BytecodeError> {
    let mut dec = Decoder {
        c: Cursor {
            data: bytes,
            pos: 0,
            mem: 0,
            layout,
        },
    };
    let mut statements = Vec::new();
    while dec.c.pos < bytes.len() {
        statements.push(dec.expr(0)?);
    }
    Ok(Script {
        statements,
        storage_size: dec.c.pos,
        memory_size: dec.c.mem,
        layout,
    })
}

/// An absolute code offset that does not land on a token boundary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BadTarget {
    /// Storage offset of the token holding the target.
    pub at: usize,
    /// Token byte.
    pub token: u8,
    /// Memory offset it points at.
    pub target: usize,
}

/// A relative skip whose value differs from the memory size it should cover.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BadSkip {
    /// Storage offset of the token holding the skip.
    pub at: usize,
    /// Token byte.
    pub token: u8,
    /// Stored skip value.
    pub skip: usize,
    /// Memory size of the expression(s) it covers.
    pub expected: usize,
}

/// Structural checks over a decoded script.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Validation {
    /// Tokens (expression nodes) in the script.
    pub tokens: usize,
    /// Deepest nesting (0 = statements only).
    pub max_depth: usize,
    /// Absolute targets checked (jumps, cases, iterator ends, labels).
    pub targets: usize,
    /// Absolute targets that miss a token boundary.
    pub bad_targets: Vec<BadTarget>,
    /// Relative skips checked.
    pub skips: usize,
    /// Relative skips that disagree with the size they cover.
    pub bad_skips: Vec<BadSkip>,
    /// Absolute targets that point at a top-level statement start.
    pub targets_on_statements: usize,
    /// Contexts producing a `foreach` iterator/array (skip = past the loop).
    pub loop_context_skips: usize,
    /// Storage offsets of contexts whose skip equals the memory size of the
    /// object expression rather than the context expression.
    pub object_size_context_skips: Vec<usize>,
}

impl Validation {
    /// True when every target and skip checked out.
    pub fn is_clean(&self) -> bool {
        self.bad_targets.is_empty() && self.bad_skips.is_empty()
    }
}

impl Script {
    /// Every expression in stream order with its depth.
    pub fn walk<'e>(&'e self, f: &mut dyn FnMut(&'e Expr, usize)) {
        for s in &self.statements {
            s.walk(f);
        }
    }

    /// Memory offsets of every token start (plus the end of the script).
    pub fn token_boundaries(&self) -> BTreeSet<usize> {
        let mut out = BTreeSet::new();
        self.walk(&mut |e, _| {
            out.insert(e.mem_offset);
        });
        out.insert(self.memory_size);
        out
    }

    /// Map a memory offset that starts a token to its storage offset.
    pub fn storage_offset_of(&self, mem: usize) -> Option<usize> {
        if mem == self.memory_size {
            return Some(self.storage_size);
        }
        let mut found = None;
        self.walk(&mut |e, _| {
            if found.is_none() && e.mem_offset == mem {
                found = Some(e.offset);
            }
        });
        found
    }

    /// True when the last statement is `EndOfScript`.
    pub fn ends_with_end_of_script(&self) -> bool {
        self.statements
            .last()
            .is_some_and(|s| s.token == token::END_OF_SCRIPT)
    }

    /// Check absolute targets against token boundaries and relative skips
    /// against the memory size of the expressions they skip.
    ///
    /// Rules (CONFIRMED over all shipped bytecode, `BYTECODE.md`):
    /// - `Jump`, `JumpIfNot`, `JumpIfNotEditorOnly`, `Case` (non-default),
    ///   `Iterator`, `DynArrayIterator` and label-table entries hold absolute
    ///   memory offsets of token starts.
    /// - `Context`/`ClassContext`: skip = memory size of the context
    ///   expression, except (a) the context that produces a `foreach`
    ///   iterator or array, whose skip lands one byte past the loop's end
    ///   offset, and (b) contexts whose skip equals the memory size of the
    ///   object expression instead (reported in
    ///   [`Validation::object_size_context_skips`]; in the shipped data these
    ///   are exactly contexts on the `Outer` variable).
    /// - `Skip`: memory size of its expression plus the `EndFunctionParms`
    ///   byte of the call it is the last argument of.
    /// - `Conditional`: each skip = memory size of the branch after it.
    /// - `DynArrayAddItem`/`RemoveItem`/`InsertItem`/`Find`/`FindStruct`/
    ///   `Sort`: memory size of the operands after the skip plus the closing
    ///   `EndFunctionParms` byte.
    /// - `DefaultParmValue`: memory size of the value plus the
    ///   `EndParmValue` byte.
    pub fn validate(&self) -> Validation {
        let bounds = self.token_boundaries();
        let stmts: BTreeSet<usize> = self.statements.iter().map(|s| s.mem_offset).collect();
        // Contexts that produce a foreach iterator/array: storage offset -> loop end.
        let mut loop_contexts: BTreeMap<usize, usize> = BTreeMap::new();
        // Skip expressions that are the last argument of a call.
        let mut last_arg_skips: BTreeSet<usize> = BTreeSet::new();
        self.walk(&mut |e, _| match &e.kind {
            ExprKind::Iterator { iterator: x, end }
            | ExprKind::DynArrayIterator { array: x, end, .. } => {
                if matches!(x.kind, ExprKind::Context(_) | ExprKind::ClassContext(_)) {
                    loop_contexts.insert(x.offset, usize::from(*end));
                }
            }
            ExprKind::VirtualFunction { args, .. }
            | ExprKind::GlobalFunction { args, .. }
            | ExprKind::FinalFunction { args, .. }
            | ExprKind::DelegateFunction { args, .. }
            | ExprKind::NativeFunction { args, .. }
            | ExprKind::DelegateCompare { args } => {
                if let Some(last) = args.last()
                    && last.token == token::SKIP
                {
                    last_arg_skips.insert(last.offset);
                }
            }
            _ => {}
        });
        let mut v = Validation::default();
        let check_target = |v: &mut Validation, e: &Expr, target: usize| {
            v.targets += 1;
            if stmts.contains(&target) {
                v.targets_on_statements += 1;
            }
            if !bounds.contains(&target) {
                v.bad_targets.push(BadTarget {
                    at: e.offset,
                    token: e.token,
                    target,
                });
            }
        };
        let check_skip = |v: &mut Validation, e: &Expr, skip: u16, expected: usize| {
            v.skips += 1;
            if usize::from(skip) != expected {
                v.bad_skips.push(BadSkip {
                    at: e.offset,
                    token: e.token,
                    skip: usize::from(skip),
                    expected,
                });
            }
        };
        self.walk(&mut |e, depth| {
            use ExprKind as K;
            v.tokens += 1;
            v.max_depth = v.max_depth.max(depth);
            match &e.kind {
                K::Jump { target }
                | K::JumpIfNot { target, .. }
                | K::JumpIfNotEditorOnly { target } => {
                    check_target(&mut v, e, usize::from(*target));
                }
                K::Case {
                    next: Some(next), ..
                } => check_target(&mut v, e, usize::from(*next)),
                K::Iterator { end, .. } | K::DynArrayIterator { end, .. } => {
                    check_target(&mut v, e, usize::from(*end));
                }
                K::LabelTable { labels, .. } => {
                    for l in labels {
                        let t = usize::try_from(l.offset).unwrap_or(usize::MAX);
                        check_target(&mut v, e, t);
                    }
                }
                K::Context(c) | K::ClassContext(c) => {
                    let loop_end = loop_contexts.get(&e.offset);
                    let expected = match loop_end {
                        Some(end) => end.saturating_add(1).saturating_sub(c.expr.mem_offset),
                        None => c.expr.mem_size,
                    };
                    let skip = usize::from(c.skip);
                    if skip != expected && skip == c.object.mem_size {
                        v.skips += 1;
                        v.object_size_context_skips.push(e.offset);
                    } else {
                        if loop_end.is_some() {
                            v.loop_context_skips += 1;
                        }
                        check_skip(&mut v, e, c.skip, expected);
                    }
                }
                K::Skip { skip, value } => {
                    let expected = if last_arg_skips.contains(&e.offset) {
                        value.mem_size.saturating_add(1)
                    } else {
                        // Not the last argument: no rule observed; always reported.
                        usize::MAX
                    };
                    check_skip(&mut v, e, *skip, expected);
                }
                K::Conditional {
                    skip_true,
                    if_true,
                    skip_false,
                    if_false,
                    ..
                } => {
                    check_skip(&mut v, e, *skip_true, if_true.mem_size);
                    check_skip(&mut v, e, *skip_false, if_false.mem_size);
                }
                K::DynArrayAddItem { skip, item, .. }
                | K::DynArrayRemoveItem { skip, item, .. } => {
                    check_skip(
                        &mut v,
                        e,
                        *skip,
                        e.mem_end().saturating_sub(item.mem_offset),
                    );
                }
                K::DynArrayInsertItem { skip, index, .. } => {
                    check_skip(
                        &mut v,
                        e,
                        *skip,
                        e.mem_end().saturating_sub(index.mem_offset),
                    );
                }
                K::DynArrayFind { skip, value, .. } => {
                    check_skip(
                        &mut v,
                        e,
                        *skip,
                        e.mem_end().saturating_sub(value.mem_offset),
                    );
                }
                K::DynArrayFindStruct { skip, member, .. } => {
                    check_skip(
                        &mut v,
                        e,
                        *skip,
                        e.mem_end().saturating_sub(member.mem_offset),
                    );
                }
                K::DynArraySort {
                    skip, comparator, ..
                } => check_skip(
                    &mut v,
                    e,
                    *skip,
                    e.mem_end().saturating_sub(comparator.mem_offset),
                ),
                K::DefaultParmValue { size, value } => {
                    check_skip(&mut v, e, *size, value.mem_size.saturating_add(1));
                }
                _ => {}
            }
        });
        v
    }

    /// Calls, natives and constants referenced by the script.
    pub fn references(&self) -> References {
        let mut r = References::default();
        self.walk(&mut |e, _| {
            use ExprKind as K;
            match &e.kind {
                K::FinalFunction { function, .. } => {
                    *r.final_functions.entry(function.0).or_default() += 1;
                }
                K::VirtualFunction { name, .. } => {
                    *r.virtual_functions.entry(fname_key(*name)).or_default() += 1;
                }
                K::GlobalFunction { name, .. } => {
                    *r.global_functions.entry(fname_key(*name)).or_default() += 1;
                }
                K::DelegateFunction { name, .. } => {
                    *r.delegate_functions.entry(fname_key(*name)).or_default() += 1;
                }
                K::NativeFunction { index, .. } => {
                    *r.natives.entry(*index).or_default() += 1;
                }
                K::FloatConst { value } => {
                    *r.floats.entry(value.to_bits()).or_default() += 1;
                }
                K::VectorConst { x, y, z } => {
                    for f in [x, y, z] {
                        *r.floats.entry(f.to_bits()).or_default() += 1;
                    }
                }
                K::IntConst { value } => *r.ints.entry(*value).or_default() += 1,
                K::IntConstByte { value } | K::ByteConst { value } => {
                    *r.ints.entry(i32::from(*value)).or_default() += 1;
                }
                K::IntZero => *r.ints.entry(0).or_default() += 1,
                K::IntOne => *r.ints.entry(1).or_default() += 1,
                K::RotationConst { pitch, yaw, roll } => {
                    for i in [pitch, yaw, roll] {
                        *r.ints.entry(*i).or_default() += 1;
                    }
                }
                K::NameConst { name } => {
                    *r.names.entry(fname_key(*name)).or_default() += 1;
                }
                _ => {}
            }
        });
        r
    }
}

/// Role of an object operand, used to type-check operands against the
/// package tables (see [`Script::object_operands`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
pub enum OperandRole {
    /// `LocalVariable` property.
    LocalVariable,
    /// `InstanceVariable` property.
    InstanceVariable,
    /// `DefaultVariable` property.
    DefaultVariable,
    /// `StateVariable` property.
    StateVariable,
    /// `LocalOutVariable` property.
    LocalOutVariable,
    /// `NativeParm` property.
    NativeParm,
    /// `ReturnNothing` property.
    ReturnNothing,
    /// `EatReturnValue` property.
    EatReturnValue,
    /// `Switch` property.
    SwitchProperty,
    /// `Context`/`ClassContext` r-value property.
    ContextRValue,
    /// `StructMember` property.
    StructMemberProperty,
    /// `StructMember` struct.
    StructMemberStruct,
    /// `StructCmpEq`/`Ne` struct.
    StructCmpStruct,
    /// `FinalFunction` function.
    FinalFunction,
    /// `MetaCast` class.
    MetaCastClass,
    /// `DynamicCast` class.
    DynamicCastClass,
    /// `InterfaceCast` class.
    InterfaceCastClass,
    /// `DelegateFunction` property.
    DelegateFunctionProperty,
    /// `DelegateProperty` property.
    DelegatePropertyProperty,
    /// `ObjectConst` object.
    ObjectConst,
}

impl OperandRole {
    /// Whether an object of class `class_name` (`"None"` for null) is
    /// acceptable for this role. The rules were confirmed over every operand
    /// of the shipped bytecode (`BYTECODE.md`).
    pub fn accepts(self, class_name: &str) -> bool {
        use OperandRole as R;
        let property = class_name.ends_with("Property");
        let null = class_name == "None";
        match self {
            R::LocalVariable
            | R::InstanceVariable
            | R::DefaultVariable
            | R::StateVariable
            | R::LocalOutVariable
            | R::NativeParm
            | R::ReturnNothing
            | R::EatReturnValue
            | R::StructMemberProperty => property,
            R::SwitchProperty => property || null,
            R::ContextRValue => property || null || class_name == "Const",
            R::DelegateFunctionProperty | R::DelegatePropertyProperty => {
                class_name == "DelegateProperty" || null
            }
            R::StructMemberStruct | R::StructCmpStruct => class_name == "ScriptStruct",
            R::FinalFunction => class_name == "Function",
            R::MetaCastClass | R::DynamicCastClass | R::InterfaceCastClass => class_name == "Class",
            R::ObjectConst => true,
        }
    }
}

/// An object operand of a token.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct ObjectOperand {
    /// Storage offset of the token.
    pub at: usize,
    /// Role.
    pub role: OperandRole,
    /// The reference.
    pub object: PackageIndex,
}

impl Script {
    /// Every object operand in stream order.
    pub fn object_operands(&self) -> Vec<ObjectOperand> {
        let mut out = Vec::new();
        self.walk(&mut |e, _| {
            use ExprKind as K;
            use OperandRole as R;
            let mut push = |role, object| {
                out.push(ObjectOperand {
                    at: e.offset,
                    role,
                    object,
                });
            };
            match &e.kind {
                K::LocalVariable { property } => push(R::LocalVariable, *property),
                K::InstanceVariable { property } => push(R::InstanceVariable, *property),
                K::DefaultVariable { property } => push(R::DefaultVariable, *property),
                K::StateVariable { property } => push(R::StateVariable, *property),
                K::LocalOutVariable { property } => push(R::LocalOutVariable, *property),
                K::NativeParm { property } => push(R::NativeParm, *property),
                K::ReturnNothing { property } => push(R::ReturnNothing, *property),
                K::EatReturnValue { property, .. } => push(R::EatReturnValue, *property),
                K::Switch { property, .. } => push(R::SwitchProperty, *property),
                K::Context(c) | K::ClassContext(c) => push(R::ContextRValue, c.rvalue_property),
                K::StructMember {
                    property, struct_, ..
                } => {
                    push(R::StructMemberProperty, *property);
                    push(R::StructMemberStruct, *struct_);
                }
                K::StructCmp { struct_, .. } => push(R::StructCmpStruct, *struct_),
                K::FinalFunction { function, .. } => push(R::FinalFunction, *function),
                K::MetaCast { class, .. } => push(R::MetaCastClass, *class),
                K::DynamicCast { class, .. } => push(R::DynamicCastClass, *class),
                K::InterfaceCast { class, .. } => push(R::InterfaceCastClass, *class),
                K::DelegateFunction { property, .. } => {
                    push(R::DelegateFunctionProperty, *property)
                }
                K::DelegateProperty { property, .. } => {
                    push(R::DelegatePropertyProperty, *property)
                }
                K::ObjectConst { object } => push(R::ObjectConst, *object),
                _ => {}
            }
        });
        out
    }
}

fn fname_key(n: FName) -> (i32, i32) {
    (n.index, n.number)
}

/// What a script references, with occurrence counts. Keys are raw
/// (package index, FName `(index, number)`, native index, `f32` bits); the
/// caller resolves them against the package.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct References {
    /// `FinalFunction` targets (package index).
    pub final_functions: BTreeMap<i32, usize>,
    /// `VirtualFunction` names.
    pub virtual_functions: BTreeMap<(i32, i32), usize>,
    /// `GlobalFunction` names.
    pub global_functions: BTreeMap<(i32, i32), usize>,
    /// `DelegateFunction` names.
    pub delegate_functions: BTreeMap<(i32, i32), usize>,
    /// Native indices called by token.
    pub natives: BTreeMap<u16, usize>,
    /// Float constants (bit patterns), including vector components.
    pub floats: BTreeMap<u32, usize>,
    /// Integer constants (int, byte, rotator components, 0/1 tokens).
    pub ints: BTreeMap<i32, usize>,
    /// Name constants.
    pub names: BTreeMap<(i32, i32), usize>,
}

// ---------------------------------------------------------------- packages

/// The bytecode of one struct export.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExportBytecode<'a> {
    /// Export index.
    pub export_index: usize,
    /// Kind (`Function`, `State`, `Class` or `ScriptStruct`).
    pub kind: ScriptKind,
    /// The `ScriptStorageSize` bytes.
    pub bytes: &'a [u8],
    /// Declared `ScriptBytecodeSize` (memory size).
    pub memory_size: i32,
    /// `LabelTableOffset` for states and classes (`0xFFFF` = none).
    pub label_table_offset: Option<u16>,
    /// `iNative` and `FunctionFlags` for functions.
    pub function: Option<(u16, u32)>,
}

/// Locate the bytecode of struct export `index` (decodes the script object).
pub fn export_bytecode(pkg: &Package, index: usize) -> ObjResult<ExportBytecode<'_>> {
    let obj = decode_script_object(pkg, None, index, &crate::schema::NoSchema)?;
    let Some(h) = obj.structure() else {
        return Err(ObjectError::WrongKind {
            export: index,
            expected: "struct (Function, State, Class or ScriptStruct)",
            found: obj.kind.name().to_owned(),
        });
    };
    let data = pkg.export_data(index)?;
    let end = h
        .bytecode_offset
        .checked_add(h.storage_size)
        .ok_or_else(|| ObjectError::Malformed {
            what: "bytecode extent",
            offset: h.bytecode_offset,
            detail: "overflow".to_owned(),
        })?;
    let bytes = data
        .get(h.bytecode_offset..end)
        .ok_or_else(|| ObjectError::Malformed {
            what: "bytecode extent",
            offset: h.bytecode_offset,
            detail: format!("{end} beyond payload of {}", data.len()),
        })?;
    let (label_table_offset, function) = match &obj.body {
        ScriptBody::Class { state, .. } | ScriptBody::State { state, .. } => {
            (Some(state.label_table_offset), None)
        }
        ScriptBody::Function { function, .. } => {
            (None, Some((function.native_index, function.function_flags)))
        }
        _ => (None, None),
    };
    Ok(ExportBytecode {
        export_index: index,
        kind: obj.kind,
        bytes,
        memory_size: h.bytecode_size,
        label_table_offset,
        function,
    })
}

/// A script function bound to a fixed native index (`iNative != 0`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct NativeInfo {
    /// The index.
    pub index: u16,
    /// Package file the function was found in.
    pub package: String,
    /// Function path (`Class.Function`) within that package.
    pub path: String,
    /// Qualified path (package-prefixed, as used by [`PackageSet::locate`]).
    pub qualified: String,
    /// `FriendlyName` (the operator symbol for operators).
    pub friendly_name: String,
    /// `FunctionFlags`.
    pub function_flags: u32,
}

impl NativeInfo {
    /// True for operators (`FUNC_Operator`).
    pub fn is_operator(&self) -> bool {
        self.function_flags & flags::function::OPERATOR != 0
    }
}

/// Native index to script function, built from the decoded `iNative` values
/// of the native script packages (indices are global: `GNatives`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct NativeTable {
    entries: BTreeMap<u16, NativeInfo>,
    /// Indices claimed by two different functions (never in shipped data).
    pub conflicts: Vec<(u16, String, String)>,
}

impl NativeTable {
    /// Add every function of `pkg` with a non-zero `iNative`; returns how
    /// many entries were new.
    pub fn add_package(&mut self, pkg: &Package, package_name: &str) -> usize {
        let mut added = 0;
        for i in 0..pkg.exports.len() {
            if ScriptKind::of_export(pkg, i) != Some(ScriptKind::Function) {
                continue;
            }
            let Ok(obj) = decode_script_object(pkg, None, i, &crate::schema::NoSchema) else {
                continue;
            };
            let ScriptBody::Function { function, .. } = &obj.body else {
                continue;
            };
            if function.native_index == 0 {
                continue;
            }
            let path = pkg.export_path(i).unwrap_or_else(|_| format!("#{i}"));
            let qualified = PackageIndex::from_export(i)
                .and_then(|idx| qualified_path(pkg, Some(package_name), idx).ok())
                .unwrap_or_else(|| path.clone());
            let info = NativeInfo {
                index: function.native_index,
                package: package_name.to_owned(),
                path,
                qualified,
                friendly_name: function.friendly_name.clone(),
                function_flags: function.function_flags,
            };
            match self.entries.get(&info.index) {
                Some(existing) if existing.path != info.path => {
                    self.conflicts
                        .push((info.index, existing.path.clone(), info.path.clone()));
                }
                Some(_) => {}
                None => {
                    self.entries.insert(info.index, info);
                    added += 1;
                }
            }
        }
        added
    }

    /// The function bound to `index`.
    pub fn get(&self, index: u16) -> Option<&NativeInfo> {
        self.entries.get(&index)
    }

    /// Number of entries.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// True when empty.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// All entries in index order.
    pub fn iter(&self) -> impl Iterator<Item = &NativeInfo> {
        self.entries.values()
    }
}

/// Number of parameters (`CPF_Parm` without `CPF_ReturnParm`) of function
/// export `index`, from its children chain.
pub fn function_parameter_count(pkg: &Package, index: usize) -> ObjResult<usize> {
    let obj = decode_script_object(pkg, None, index, &crate::schema::NoSchema)?;
    let Some(h) = obj.structure() else {
        return Err(ObjectError::WrongKind {
            export: index,
            expected: "Function",
            found: obj.kind.name().to_owned(),
        });
    };
    let mut count = 0;
    let mut cur = h.children;
    let mut steps = 0usize;
    while let Some(ci) = cur.export_index() {
        steps += 1;
        if steps > MAX_CHILDREN.min(pkg.exports.len().saturating_add(1)) {
            return Err(ObjectError::Malformed {
                what: "children chain",
                offset: 0,
                detail: "cycle or excessive length".to_owned(),
            });
        }
        let child = decode_script_object(pkg, None, ci, &crate::schema::NoSchema)?;
        if let Some(p) = child.property()
            && p.flags & flags::property::PARM != 0
            && p.flags & flags::property::RETURN_PARM == 0
        {
            count += 1;
        }
        cur = child.next.unwrap_or_default();
    }
    Ok(count)
}

/// Argument counts of calls compared with the callee's parameter count.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct ArityCheck {
    /// `FinalFunction` calls.
    pub final_calls: usize,
    /// ... with exactly one argument expression per parameter.
    pub final_ok: usize,
    /// ... whose callee could not be resolved.
    pub final_unresolved: usize,
    /// Native calls by token.
    pub native_calls: usize,
    /// ... with exactly one argument expression per parameter.
    pub native_ok: usize,
    /// ... whose index has no script function.
    pub native_unresolved: usize,
    /// First mismatches.
    pub failures: Vec<String>,
}

impl ArityCheck {
    /// Merge another report.
    pub fn absorb(&mut self, o: &ArityCheck) {
        self.final_calls += o.final_calls;
        self.final_ok += o.final_ok;
        self.final_unresolved += o.final_unresolved;
        self.native_calls += o.native_calls;
        self.native_ok += o.native_ok;
        self.native_unresolved += o.native_unresolved;
        for f in &o.failures {
            if self.failures.len() < MAX_FAILURES {
                self.failures.push(f.clone());
            }
        }
    }
}

/// Check every `FinalFunction` and native call in `lp` against the callee's
/// parameter count (resolving imports through `set`, natives through
/// `natives`). Omitted optional arguments are explicit `EmptyParmValue`
/// tokens, so the counts must match exactly.
pub fn check_call_arity(
    set: &PackageSet,
    lp: &LoadedPackage,
    natives: &NativeTable,
    layout: Layout,
) -> ArityCheck {
    let mut out = ArityCheck::default();
    let mut cache: BTreeMap<String, Option<usize>> = BTreeMap::new();
    let mut params_of = |qualified: &str| -> Option<usize> {
        let key = qualified.to_ascii_lowercase();
        if let Some(c) = cache.get(&key) {
            return *c;
        }
        let r = set
            .locate(qualified)
            .and_then(|(p, i)| function_parameter_count(&p.package, i).ok());
        cache.insert(key, r);
        r
    };
    let pkg = &lp.package;
    for i in 0..pkg.exports.len() {
        let Some(kind) = ScriptKind::of_export(pkg, i) else {
            continue;
        };
        if !kind.is_struct() {
            continue;
        }
        let Ok(bc) = export_bytecode(pkg, i) else {
            continue;
        };
        let Ok(script) = decode(bc.bytes, layout) else {
            continue;
        };
        let mut fail = |msg: String| {
            if out.failures.len() < MAX_FAILURES {
                let path = pkg.export_path(i).unwrap_or_default();
                out.failures.push(format!("{} #{i} {path}: {msg}", lp.name));
            }
        };
        let mut calls: Vec<(bool, String, usize, usize)> = Vec::new();
        let mut unresolved_natives = 0usize;
        script.walk(&mut |e, _| match &e.kind {
            ExprKind::FinalFunction { function, args } => {
                let q = lp.ref_path(*function).ok().flatten().unwrap_or_default();
                calls.push((true, q, args.len(), e.offset));
            }
            ExprKind::NativeFunction { index, args } => match natives.get(*index) {
                Some(n) => calls.push((false, n.qualified.clone(), args.len(), e.offset)),
                None => unresolved_natives += 1,
            },
            _ => {}
        });
        out.native_calls += unresolved_natives;
        out.native_unresolved += unresolved_natives;
        for (is_final, q, nargs, at) in calls {
            if is_final {
                out.final_calls += 1;
            } else {
                out.native_calls += 1;
            }
            match params_of(&q) {
                Some(n) if n == nargs => {
                    if is_final {
                        out.final_ok += 1;
                    } else {
                        out.native_ok += 1;
                    }
                }
                Some(n) => fail(format!(
                    "call of {q} at {at}: {nargs} arguments, {n} parameters"
                )),
                None => {
                    if is_final {
                        out.final_unresolved += 1;
                    } else {
                        out.native_unresolved += 1;
                    }
                }
            }
        }
    }
    out
}

/// Per-kind bytecode coverage.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct KindBytecode {
    /// Exports of the kind.
    pub total: usize,
    /// Exports with `ScriptStorageSize > 0`.
    pub with_bytecode: usize,
    /// Of those: decoded consuming exactly `ScriptStorageSize`.
    pub exact: usize,
    /// Of those: memory total equal to `ScriptBytecodeSize`.
    pub memory_match: usize,
    /// Of those: every target and skip valid.
    pub clean: usize,
    /// Of those: last statement is `EndOfScript`.
    pub end_of_script: usize,
}

/// Bytecode coverage over packages.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct BytecodeCoverage {
    /// Per kind.
    pub kinds: BTreeMap<String, KindBytecode>,
    /// Storage bytes decoded.
    pub storage_bytes: usize,
    /// Memory bytes decoded.
    pub memory_bytes: usize,
    /// Expression nodes.
    pub tokens: usize,
    /// Token histogram by token name (natives grouped as `Native`).
    pub token_counts: BTreeMap<String, usize>,
    /// Native calls by token form (`one-byte`, `two-byte`).
    pub native_forms: BTreeMap<String, usize>,
    /// Absolute targets checked / bad.
    pub targets: usize,
    /// Absolute targets that miss a token boundary.
    pub bad_targets: usize,
    /// Absolute targets on top-level statement starts.
    pub targets_on_statements: usize,
    /// Relative skips checked.
    pub skips: usize,
    /// Relative skips that disagree.
    pub bad_skips: usize,
    /// Deepest nesting seen.
    pub max_depth: usize,
    /// States/classes with a `LabelTableOffset` (not 0xFFFF).
    pub label_table_offsets: usize,
    /// ... that point at the entries right after a `LabelTable` token.
    pub label_table_offsets_ok: usize,
    /// Contexts whose skip covers a `foreach` loop.
    pub loop_context_skips: usize,
    /// Contexts whose skip equals the object expression's memory size.
    pub object_size_context_skips: usize,
    /// ... of which the object is the `Outer` instance variable.
    pub object_size_context_skips_on_outer: usize,
    /// Label tables / terminators named `None`.
    pub label_tables: usize,
    /// Label-table terminators whose name is `None`.
    pub label_terminators_none: usize,
    /// Top-level statements that are bare terminators or literals (a sign of
    /// a mis-parse; 0 in the shipped data).
    pub suspicious_statements: usize,
    /// Object operands type-checked against the package tables.
    pub operands: usize,
    /// Object operands whose class does not fit their role.
    pub operand_violations: usize,
    /// Function-scoped operands (`LocalVariable`, `LocalOutVariable`,
    /// `NativeParm`, `ReturnNothing`) checked for belonging to the function.
    pub local_operands: usize,
    /// ... whose property's outer is the function being decoded.
    pub local_operands_ok: usize,
    /// Contexts over a plain variable, checked for `rvalue_property` = variable.
    pub variable_contexts: usize,
    /// ... where the r-value property is that variable.
    pub variable_contexts_ok: usize,
    /// Properties with a `RepOffset` (CPF_Net) whose class has bytecode.
    pub rep_offsets: usize,
    /// ... that land on a statement start of the class bytecode.
    pub rep_offsets_ok: usize,
    /// First failures (`package export: error`).
    pub failures: Vec<String>,
}

impl BytecodeCoverage {
    /// Totals over all kinds.
    pub fn total(&self) -> KindBytecode {
        let mut t = KindBytecode::default();
        for k in self.kinds.values() {
            t.total += k.total;
            t.with_bytecode += k.with_bytecode;
            t.exact += k.exact;
            t.memory_match += k.memory_match;
            t.clean += k.clean;
            t.end_of_script += k.end_of_script;
        }
        t
    }

    /// Merge another report into this one.
    pub fn absorb(&mut self, o: &BytecodeCoverage) {
        for (k, v) in &o.kinds {
            let e = self.kinds.entry(k.clone()).or_default();
            e.total += v.total;
            e.with_bytecode += v.with_bytecode;
            e.exact += v.exact;
            e.memory_match += v.memory_match;
            e.clean += v.clean;
            e.end_of_script += v.end_of_script;
        }
        self.storage_bytes += o.storage_bytes;
        self.memory_bytes += o.memory_bytes;
        self.tokens += o.tokens;
        for (k, v) in &o.token_counts {
            *self.token_counts.entry(k.clone()).or_default() += v;
        }
        for (k, v) in &o.native_forms {
            *self.native_forms.entry(k.clone()).or_default() += v;
        }
        self.targets += o.targets;
        self.bad_targets += o.bad_targets;
        self.targets_on_statements += o.targets_on_statements;
        self.skips += o.skips;
        self.bad_skips += o.bad_skips;
        self.max_depth = self.max_depth.max(o.max_depth);
        self.loop_context_skips += o.loop_context_skips;
        self.object_size_context_skips += o.object_size_context_skips;
        self.object_size_context_skips_on_outer += o.object_size_context_skips_on_outer;
        self.label_tables += o.label_tables;
        self.label_terminators_none += o.label_terminators_none;
        self.suspicious_statements += o.suspicious_statements;
        self.operands += o.operands;
        self.operand_violations += o.operand_violations;
        self.local_operands += o.local_operands;
        self.local_operands_ok += o.local_operands_ok;
        self.variable_contexts += o.variable_contexts;
        self.variable_contexts_ok += o.variable_contexts_ok;
        self.label_table_offsets += o.label_table_offsets;
        self.label_table_offsets_ok += o.label_table_offsets_ok;
        self.rep_offsets += o.rep_offsets;
        self.rep_offsets_ok += o.rep_offsets_ok;
        for f in &o.failures {
            if self.failures.len() < MAX_FAILURES {
                self.failures.push(f.clone());
            }
        }
    }

    fn fail(&mut self, msg: String) {
        if self.failures.len() < MAX_FAILURES {
            self.failures.push(msg);
        }
    }
}

/// Decode the bytecode of every struct export of `pkg` and validate it.
pub fn package_bytecode_coverage(pkg: &Package, label: &str, layout: Layout) -> BytecodeCoverage {
    let mut cov = BytecodeCoverage::default();
    // Class export index -> statement starts of its bytecode (memory).
    let mut class_statements: BTreeMap<usize, BTreeSet<usize>> = BTreeMap::new();
    for i in 0..pkg.exports.len() {
        let Some(kind) = ScriptKind::of_export(pkg, i) else {
            continue;
        };
        if !kind.is_struct() {
            continue;
        }
        let k = cov.kinds.entry(kind.name().to_owned()).or_default();
        k.total += 1;
        let bc = match export_bytecode(pkg, i) {
            Ok(b) => b,
            Err(e) => {
                cov.fail(format!("{label} #{i}: {e}"));
                continue;
            }
        };
        if bc.bytes.is_empty() {
            continue;
        }
        k.with_bytecode += 1;
        let script = match decode(bc.bytes, layout) {
            Ok(s) => s,
            Err(e) => {
                let path = pkg.export_path(i).unwrap_or_default();
                cov.fail(format!("{label} #{i} {path}: {e}"));
                continue;
            }
        };
        let k = cov.kinds.entry(kind.name().to_owned()).or_default();
        k.exact += 1;
        let memory_ok = i64::try_from(script.memory_size).ok() == Some(i64::from(bc.memory_size));
        if memory_ok {
            k.memory_match += 1;
        }
        if script.ends_with_end_of_script() {
            k.end_of_script += 1;
        }
        let v = script.validate();
        if v.is_clean() {
            k.clean += 1;
        }
        if !memory_ok {
            let path = pkg.export_path(i).unwrap_or_default();
            cov.fail(format!(
                "{label} #{i} {path}: memory size {} != ScriptBytecodeSize {}",
                script.memory_size, bc.memory_size
            ));
        }
        if let Some(b) = v.bad_targets.first() {
            let path = pkg.export_path(i).unwrap_or_default();
            cov.fail(format!(
                "{label} #{i} {path}: {} bad targets (first: {} at {} -> {})",
                v.bad_targets.len(),
                token_name(b.token),
                b.at,
                b.target
            ));
        }
        if let Some(b) = v.bad_skips.first() {
            let path = pkg.export_path(i).unwrap_or_default();
            cov.fail(format!(
                "{label} #{i} {path}: {} bad skips (first: {} at {}: {} != {})",
                v.bad_skips.len(),
                token_name(b.token),
                b.at,
                b.skip,
                b.expected
            ));
        }
        cov.storage_bytes += script.storage_size;
        cov.memory_bytes += script.memory_size;
        cov.tokens += v.tokens;
        cov.targets += v.targets;
        cov.bad_targets += v.bad_targets.len();
        cov.targets_on_statements += v.targets_on_statements;
        cov.skips += v.skips;
        cov.bad_skips += v.bad_skips.len();
        cov.max_depth = cov.max_depth.max(v.max_depth);
        script.walk(&mut |e, _| {
            *cov.token_counts
                .entry(token_name(e.token).to_owned())
                .or_default() += 1;
            if e.token >= token::FIRST_NATIVE {
                *cov.native_forms.entry("one-byte".to_owned()).or_default() += 1;
            } else if e.token >= token::EXTENDED_NATIVE {
                *cov.native_forms.entry("two-byte".to_owned()).or_default() += 1;
            }
        });
        cov.loop_context_skips += v.loop_context_skips;
        for at in &v.object_size_context_skips {
            cov.object_size_context_skips += 1;
            let mut on_outer = false;
            script.walk(&mut |e, _| {
                if e.offset == *at
                    && let ExprKind::Context(c) | ExprKind::ClassContext(c) = &e.kind
                    && let ExprKind::InstanceVariable { property } = &c.object.kind
                    && pkg.object_name(*property).is_ok_and(|n| n == "Outer")
                {
                    on_outer = true;
                }
            });
            if on_outer {
                cov.object_size_context_skips_on_outer += 1;
            }
        }
        for st in &script.statements {
            use token::*;
            if matches!(
                st.token,
                END_FUNCTION_PARMS
                    | END_PARM_VALUE
                    | INT_CONST
                    | FLOAT_CONST
                    | STRING_CONST
                    | NAME_CONST
                    | INT_ZERO
                    | INT_ONE
                    | INT_CONST_BYTE
                    | BYTE_CONST
                    | TRUE
                    | FALSE
                    | NO_OBJECT
                    | EMPTY_PARM_VALUE
            ) {
                cov.suspicious_statements += 1;
                let path = pkg.export_path(i).unwrap_or_default();
                cov.fail(format!(
                    "{label} #{i} {path}: top-level {} at {}",
                    token_name(st.token),
                    st.offset
                ));
            }
        }
        let me = PackageIndex::from_export(i).unwrap_or_default();
        for o in script.object_operands() {
            cov.operands += 1;
            let class = pkg
                .class_name(o.object)
                .unwrap_or_else(|_| "<bad index>".to_owned());
            if !o.role.accepts(&class) {
                cov.operand_violations += 1;
                let path = pkg.export_path(i).unwrap_or_default();
                cov.fail(format!(
                    "{label} #{i} {path}: {:?} operand at {} is a {class}",
                    o.role, o.at
                ));
            }
            if matches!(
                o.role,
                OperandRole::LocalVariable
                    | OperandRole::LocalOutVariable
                    | OperandRole::NativeParm
                    | OperandRole::ReturnNothing
            ) {
                cov.local_operands += 1;
                if pkg.outer_of(o.object).ok() == Some(me) {
                    cov.local_operands_ok += 1;
                }
            }
        }
        script.walk(&mut |e, _| match &e.kind {
            ExprKind::Context(c) | ExprKind::ClassContext(c) => {
                if let ExprKind::InstanceVariable { property }
                | ExprKind::DefaultVariable { property }
                | ExprKind::LocalVariable { property } = &c.expr.kind
                {
                    cov.variable_contexts += 1;
                    if *property == c.rvalue_property {
                        cov.variable_contexts_ok += 1;
                    }
                }
            }
            ExprKind::LabelTable { terminator, .. } => {
                cov.label_tables += 1;
                if pkg.try_fname(*terminator).is_ok_and(|n| n == "None") {
                    cov.label_terminators_none += 1;
                }
            }
            _ => {}
        });
        if let Some(lto) = bc.label_table_offset
            && lto != 0xFFFF
        {
            cov.label_table_offsets += 1;
            let mut ok = false;
            script.walk(&mut |e, _| {
                if e.token == token::LABEL_TABLE
                    && e.mem_offset.checked_add(1) == Some(usize::from(lto))
                {
                    ok = true;
                }
            });
            if ok {
                cov.label_table_offsets_ok += 1;
            } else {
                let path = pkg.export_path(i).unwrap_or_default();
                cov.fail(format!(
                    "{label} #{i} {path}: LabelTableOffset {lto} is not a label table"
                ));
            }
        }
        if kind == ScriptKind::Class {
            class_statements.insert(i, script.statements.iter().map(|s| s.mem_offset).collect());
        }
    }
    // Replicated properties: RepOffset points into the owning class's bytecode.
    for i in 0..pkg.exports.len() {
        let Some(kind) = ScriptKind::of_export(pkg, i) else {
            continue;
        };
        if !kind.is_property() {
            continue;
        }
        let Ok(obj) = decode_script_object(pkg, None, i, &crate::schema::NoSchema) else {
            continue;
        };
        let Some(rep) = obj.property().and_then(|p| p.rep_offset) else {
            continue;
        };
        let Some(outer) = pkg
            .exports
            .get(i)
            .and_then(|e| e.outer_index.export_index())
        else {
            continue;
        };
        let Some(starts) = class_statements.get(&outer) else {
            continue;
        };
        cov.rep_offsets += 1;
        if starts.contains(&usize::from(rep)) {
            cov.rep_offsets_ok += 1;
        } else {
            let path = pkg.export_path(i).unwrap_or_default();
            cov.fail(format!(
                "{label} #{i} {path}: RepOffset {rep} is not a statement of the class bytecode"
            ));
        }
    }
    cov
}

#[cfg(test)]
mod tests {
    use super::*;

    fn obj(v: i32) -> [u8; 4] {
        v.to_le_bytes()
    }

    #[test]
    fn decodes_let_with_native_call() {
        // Let(LocalVariable #1, Add_IntInt(IntConst 5, IntOne)) ; EndOfScript
        let mut b = vec![token::LET, token::LOCAL_VARIABLE];
        b.extend_from_slice(&obj(1));
        b.push(0x92); // one-byte native
        b.push(token::INT_CONST);
        b.extend_from_slice(&5i32.to_le_bytes());
        b.push(token::INT_ONE);
        b.push(token::END_FUNCTION_PARMS);
        b.push(token::END_OF_SCRIPT);
        let s = decode(&b, Layout::SHIPPED).unwrap();
        assert_eq!(s.storage_size, b.len());
        assert_eq!(s.memory_size, b.len() + 4);
        assert_eq!(s.statements.len(), 2);
        assert!(s.ends_with_end_of_script());
        let ExprKind::Let { value, .. } = &s.statements[0].kind else {
            panic!("not a Let");
        };
        assert!(matches!(
            value.kind,
            ExprKind::NativeFunction { index: 0x92, .. }
        ));
        assert_eq!(value.mem_offset, 10);
        assert!(s.validate().is_clean());
    }

    #[test]
    fn extended_native_index() {
        let b = [0x61, 0x15, token::END_FUNCTION_PARMS];
        let s = decode(&b, Layout::SHIPPED).unwrap();
        assert!(matches!(
            s.statements[0].kind,
            ExprKind::NativeFunction { index: 0x115, .. }
        ));
    }

    #[test]
    fn terminator_as_operand_is_rejected() {
        let b = [token::RETURN, token::END_FUNCTION_PARMS];
        assert!(matches!(
            decode(&b, Layout::SHIPPED),
            Err(BytecodeError::UnexpectedTerminator { .. })
        ));
    }

    #[test]
    fn deep_nesting_is_rejected() {
        let mut b = vec![token::RETURN; MAX_DEPTH + 4];
        b.push(token::INT_ZERO);
        assert!(matches!(
            decode(&b, Layout::SHIPPED),
            Err(BytecodeError::TooDeep { .. })
        ));
    }
}
