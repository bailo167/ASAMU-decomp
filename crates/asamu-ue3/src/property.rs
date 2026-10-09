//! Tagged properties (`FPropertyTag` streams) and property values, v868.
//!
//! ```text
//! tag   : FName Name                      ("None" ends the stream)
//!         FName Type                      (IntProperty, StructProperty, ...)
//!         i32   Size                      (bytes of the value that follows the tag)
//!         i32   ArrayIndex                (element of a static array)
//!         StructProperty: FName StructName
//!         BoolProperty:   u8 Value        (Size is 0)
//!         ByteProperty:   FName EnumName  ("None" for plain bytes)
//! value : Size bytes
//! ```
//!
//! Value encodings (CONFIRMED by exact consumption of every tag in the script
//! packages, see `docs/reverse-engineering/OBJECT_FORMAT.md`): Int/Float 4,
//! Name 8, Str FString, Object/Class/Component/Interface 4 (package index),
//! Delegate 12 (object + function FName), Byte 1 or 8 (an enum value is stored
//! as the enumerator's FName when the tag carries an EnumName), Array
//! `i32 count` + elements, Struct either a nested tagged stream or, for
//! immutable structs, the members back to back in binary form.
//!
//! Array elements and binary struct members use the item encoding
//! (`SerializeItem`): the same as above except that bools take one byte,
//! plain bytes one byte, enum-typed bytes an 8-byte FName (the enumerator's
//! name), and structs recurse with the same binary/tagged rule.

use std::sync::Arc;

use serde::Serialize;

use crate::object::{ObjResult, ObjectError, qualified_path};
use crate::package::Package;
use crate::reader::Reader;
use crate::schema::{PropertyDef, PropertyType, Schema, StructDef};
use crate::types::{FName, PackageIndex};

/// Deepest nesting of struct/array values followed.
pub const MAX_VALUE_DEPTH: usize = 32;
/// Most warnings kept per decode (the rest are counted).
pub const MAX_WARNINGS: usize = 256;
/// Work units (tags, array elements, binary struct members) allowed per
/// payload byte. Real data needs at most about one unit per byte (the
/// densest shipped object, a byte array, has 0.999 decoded values per byte).
pub const WORK_PER_BYTE: usize = 8;
/// Work units always allowed, whatever the payload size.
pub const MIN_WORK_BUDGET: usize = 1 << 16;
/// Work budget of a fresh [`ValueContext`] (callers that know the payload
/// size should use [`work_budget_for`]).
pub const DEFAULT_WORK_BUDGET: usize = 1 << 22;
/// Largest element count reserved up front for a decoded array; longer
/// arrays grow as elements actually decode. A serialized count is only
/// checked against the remaining bytes, and one decoded [`Value`] is far
/// larger than one serialized byte.
pub const MAX_PREALLOC: usize = 4096;

/// Work budget for decoding a payload of `len` bytes.
///
/// The budget bounds the total number of decoded values, so a hostile schema
/// (zero-sized binary structs with a huge `ArrayDim`, or nested structs that
/// fan out exponentially) cannot make the decoder hang or exhaust memory.
pub fn work_budget_for(len: usize) -> usize {
    len.saturating_mul(WORK_PER_BYTE)
        .saturating_add(MIN_WORK_BUDGET)
}

/// Object reference value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ObjRef {
    /// Raw package index.
    pub index: i32,
    /// Resolved qualified path (`None` for null).
    pub path: String,
}

/// A decoded property value.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub enum Value {
    /// `IntProperty`.
    Int(i32),
    /// `FloatProperty`.
    Float(f32),
    /// `BoolProperty`.
    Bool(bool),
    /// `ByteProperty` without an enum (or an element of a byte array).
    Byte(u8),
    /// Enum-typed byte, stored as the enumerator's name.
    Enum(String),
    /// `NameProperty`.
    Name(String),
    /// `StrProperty`.
    Str(String),
    /// `ObjectProperty` / `ClassProperty` / `ComponentProperty`.
    Object(ObjRef),
    /// `InterfaceProperty`.
    Interface(ObjRef),
    /// `DelegateProperty`.
    Delegate {
        /// Bound object.
        object: ObjRef,
        /// Function name.
        function: String,
    },
    /// Dynamic array with decoded elements.
    Array(Vec<Value>),
    /// Dynamic array whose element type is unknown.
    RawArray {
        /// Element count.
        count: usize,
        /// Bytes of element data.
        bytes: usize,
    },
    /// Struct value.
    Struct {
        /// Struct name.
        name: String,
        /// True when stored in binary form (immutable struct).
        binary: bool,
        /// Members (tags of a tagged struct; members in order for a binary one).
        fields: Vec<Property>,
    },
    /// Bytes that could not be interpreted.
    Raw {
        /// Size in bytes.
        bytes: usize,
        /// Why the value is raw.
        reason: String,
    },
}

/// One tagged property (or one member of a binary struct).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Property {
    /// Property name.
    pub name: String,
    /// Property type name (`IntProperty`, ...).
    pub type_name: String,
    /// Static array index.
    pub array_index: i32,
    /// Serialized value size in bytes.
    pub size: usize,
    /// Struct name (struct tags only).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub struct_name: Option<String>,
    /// Enum name (byte tags with an enum only).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enum_name: Option<String>,
    /// Decoded value.
    pub value: Value,
    /// Payload offset of the tag (or of the member, for binary structs).
    pub offset: usize,
}

/// Decoding context: package, schema, warning sink.
pub struct ValueContext<'a> {
    /// Package the bytes come from.
    pub pkg: &'a Package,
    /// The package's own name (qualifies export paths).
    pub own_name: Option<&'a str>,
    /// Type information.
    pub schema: &'a dyn Schema,
    /// True when the package is cooked (`ImmutableWhenCooked` structs are binary).
    pub cooked: bool,
    warnings: Vec<String>,
    dropped: usize,
    budget: usize,
    exhausted: bool,
}

impl<'a> ValueContext<'a> {
    /// Context for `pkg`, with [`DEFAULT_WORK_BUDGET`].
    pub fn new(pkg: &'a Package, own_name: Option<&'a str>, schema: &'a dyn Schema) -> Self {
        ValueContext {
            pkg,
            own_name,
            schema,
            cooked: pkg.summary.is_cooked(),
            warnings: Vec::new(),
            dropped: 0,
            budget: DEFAULT_WORK_BUDGET,
            exhausted: false,
        }
    }

    /// Replace the remaining work budget (see [`work_budget_for`]).
    pub fn set_work_budget(&mut self, units: usize) {
        self.budget = units;
        self.exhausted = false;
    }

    /// Work units left.
    pub fn remaining_work(&self) -> usize {
        self.budget
    }

    /// True once the work budget ran out; every later decode fails.
    pub fn budget_exhausted(&self) -> bool {
        self.exhausted
    }

    /// Spend one work unit.
    fn charge(&mut self, offset: usize) -> ObjResult<()> {
        match self.budget.checked_sub(1) {
            Some(left) if !self.exhausted => {
                self.budget = left;
                Ok(())
            }
            _ => {
                self.exhausted = true;
                Err(ObjectError::Malformed {
                    what: "property values",
                    offset,
                    detail: "decoding work budget exhausted (more values than the payload \
                             size allows)"
                        .to_owned(),
                })
            }
        }
    }

    /// Record a warning.
    pub fn warn(&mut self, offset: usize, msg: impl Into<String>) {
        if self.warnings.len() < MAX_WARNINGS {
            self.warnings.push(format!("@{offset}: {}", msg.into()));
        } else {
            self.dropped = self.dropped.saturating_add(1);
        }
    }

    /// Warnings recorded so far.
    pub fn warnings(&self) -> &[String] {
        &self.warnings
    }

    /// Consume the context and return its warnings.
    pub fn into_warnings(mut self) -> Vec<String> {
        if self.dropped > 0 {
            self.warnings
                .push(format!("{} further warnings not listed", self.dropped));
        }
        self.warnings
    }

    fn name(&self, n: FName, what: &'static str, offset: usize) -> ObjResult<String> {
        self.pkg.try_fname(n).map_err(|e| ObjectError::Malformed {
            what,
            offset,
            detail: e.to_string(),
        })
    }

    fn obj_ref(&mut self, idx: PackageIndex, offset: usize) -> ObjRef {
        match qualified_path(self.pkg, self.own_name, idx) {
            Ok(path) => ObjRef { index: idx.0, path },
            Err(e) => {
                self.warn(
                    offset,
                    format!("unresolvable object reference {}: {e}", idx.0),
                );
                ObjRef {
                    index: idx.0,
                    path: format!("<bad index {}>", idx.0),
                }
            }
        }
    }
}

fn eq(a: &str, b: &str) -> bool {
    a.eq_ignore_ascii_case(b)
}

fn read_name(r: &mut Reader<'_>, ctx: &ValueContext<'_>, what: &'static str) -> ObjResult<String> {
    let at = r.position();
    let n = r.read_fname()?;
    ctx.name(n, what, at)
}

/// Read a tagged property stream up to and including its `None` terminator.
///
/// `owner` is the qualified path of the class/struct whose properties the tags
/// name; it is used to look up declarations for arrays and structs.
pub fn read_tagged(
    r: &mut Reader<'_>,
    ctx: &mut ValueContext<'_>,
    owner: Option<&str>,
    depth: usize,
) -> ObjResult<Vec<Property>> {
    if depth > MAX_VALUE_DEPTH {
        return Err(ObjectError::TooDeep {
            limit: MAX_VALUE_DEPTH,
            offset: r.position(),
        });
    }
    let mut out = Vec::new();
    loop {
        let offset = r.position();
        let name = read_name(r, ctx, "property tag name")?;
        if eq(&name, "None") {
            return Ok(out);
        }
        ctx.charge(offset)?;
        let type_name = read_name(r, ctx, "property tag type")?;
        let size_at = r.position();
        let size = r.read_i32()?;
        let size = usize::try_from(size).map_err(|_| ObjectError::Malformed {
            what: "property tag size",
            offset: size_at,
            detail: format!("negative size {size}"),
        })?;
        let idx_at = r.position();
        let array_index = r.read_i32()?;
        if array_index < 0 {
            return Err(ObjectError::Malformed {
                what: "property tag array index",
                offset: idx_at,
                detail: format!("negative index {array_index}"),
            });
        }
        let mut struct_name = None;
        let mut enum_name = None;
        let mut bool_value = None;
        match type_name.as_str() {
            "StructProperty" => struct_name = Some(read_name(r, ctx, "struct tag name")?),
            "BoolProperty" => bool_value = Some(r.read_u8()?),
            "ByteProperty" => enum_name = Some(read_name(r, ctx, "byte tag enum name")?),
            _ => {}
        }
        let start = r.position();
        let end =
            start
                .checked_add(size)
                .filter(|e| *e <= r.len())
                .ok_or(ObjectError::Malformed {
                    what: "property tag size",
                    offset: size_at,
                    detail: format!(
                        "value of {size} bytes runs past the end ({} left)",
                        r.remaining()
                    ),
                })?;

        let def = owner.and_then(|o| ctx.schema.find_property(o, &name));
        if def.is_none()
            && let Some(o) = owner
            && ctx.schema.struct_def(o).is_some()
        {
            ctx.warn(
                offset,
                format!("{name}: no such property declared on {o} or its supers"),
            );
        }
        let def = match def {
            Some(d) if !tag_type_matches(&type_name, &d.ty) => {
                ctx.warn(
                    offset,
                    format!(
                        "{name}: tag type {type_name} disagrees with declaration {}",
                        d.ty.class_name()
                    ),
                );
                None
            }
            d => d,
        };

        let mut vr = Reader::at(&r.data()[..end], start)?;
        let value = match decode_tag_value(
            &mut vr,
            ctx,
            &type_name,
            TagExtras {
                struct_name: struct_name.as_deref(),
                enum_name: enum_name.as_deref(),
                bool_value,
            },
            def.as_deref(),
            depth,
        ) {
            Ok(v) if vr.position() == end => v,
            Ok(_) => {
                ctx.warn(
                    start,
                    format!(
                        "{name} ({type_name}): decoded {} of {size} bytes; kept raw",
                        vr.position().saturating_sub(start)
                    ),
                );
                Value::Raw {
                    bytes: size,
                    reason: "decoded size disagrees with tag size".to_owned(),
                }
            }
            // A spent work budget is fatal: keeping the value raw would let
            // the rest of the stream continue without a budget.
            Err(e) if ctx.budget_exhausted() => return Err(e),
            Err(e) => {
                ctx.warn(start, format!("{name} ({type_name}): {e}; kept raw"));
                Value::Raw {
                    bytes: size,
                    reason: e.to_string(),
                }
            }
        };
        r.seek(end)?;
        out.push(Property {
            name,
            type_name,
            array_index,
            size,
            struct_name,
            enum_name: enum_name.filter(|e| !eq(e, "None")),
            value,
            offset,
        });
    }
}

/// True when a tag's type name is the one written for a property of type
/// `ty`. Class and component properties are subclasses of the object
/// property and are tagged as `ObjectProperty` (CONFIRMED: no
/// `ClassProperty`/`ComponentProperty` tag occurs in the shipped packages).
pub fn tag_type_matches(type_name: &str, ty: &PropertyType) -> bool {
    match ty {
        PropertyType::Object { .. }
        | PropertyType::Class { .. }
        | PropertyType::Component { .. } => type_name == "ObjectProperty",
        other => other.class_name() == type_name,
    }
}

#[derive(Clone, Copy)]
struct TagExtras<'s> {
    struct_name: Option<&'s str>,
    enum_name: Option<&'s str>,
    bool_value: Option<u8>,
}

fn decode_tag_value(
    r: &mut Reader<'_>,
    ctx: &mut ValueContext<'_>,
    type_name: &str,
    extras: TagExtras<'_>,
    def: Option<&PropertyDef>,
    depth: usize,
) -> ObjResult<Value> {
    let size = r.remaining();
    let at = r.position();
    let expect = |n: usize| -> ObjResult<()> {
        if size == n {
            Ok(())
        } else {
            Err(ObjectError::Malformed {
                what: "property value",
                offset: at,
                detail: format!("{type_name} with size {size}, expected {n}"),
            })
        }
    };
    Ok(match type_name {
        "IntProperty" => {
            expect(4)?;
            Value::Int(r.read_i32()?)
        }
        "FloatProperty" => {
            expect(4)?;
            Value::Float(r.read_f32()?)
        }
        "BoolProperty" => {
            expect(0)?;
            Value::Bool(extras.bool_value.unwrap_or(0) != 0)
        }
        "ByteProperty" => match extras.enum_name {
            Some(e) if !eq(e, "None") && size == 8 => Value::Enum(read_name(r, ctx, "enum value")?),
            _ => {
                expect(1)?;
                Value::Byte(r.read_u8()?)
            }
        },
        "NameProperty" => {
            expect(8)?;
            Value::Name(read_name(r, ctx, "name value")?)
        }
        "StrProperty" => Value::Str(r.read_fstring()?),
        "ObjectProperty" | "ClassProperty" | "ComponentProperty" => {
            expect(4)?;
            let idx = r.read_package_index()?;
            Value::Object(ctx.obj_ref(idx, at))
        }
        "InterfaceProperty" => {
            expect(4)?;
            let idx = r.read_package_index()?;
            Value::Interface(ctx.obj_ref(idx, at))
        }
        "DelegateProperty" => {
            expect(12)?;
            let idx = r.read_package_index()?;
            let object = ctx.obj_ref(idx, at);
            let function = read_name(r, ctx, "delegate function")?;
            Value::Delegate { object, function }
        }
        "ArrayProperty" => {
            let inner = match def.map(|d| &d.ty) {
                Some(PropertyType::Array { inner }) => Some(inner.as_ref()),
                _ => None,
            };
            decode_array(r, ctx, inner, depth)?
        }
        "StructProperty" => {
            let name = extras.struct_name.unwrap_or("None");
            let sdef = match def.map(|d| &d.ty) {
                Some(PropertyType::Struct { struct_path }) => ctx.schema.struct_def(struct_path),
                _ => None,
            }
            .or_else(|| ctx.schema.struct_by_name(name));
            decode_struct(r, ctx, name, sdef, true, depth)?
        }
        other => {
            ctx.warn(at, format!("unsupported property type {other}; kept raw"));
            r.skip(size)?;
            Value::Raw {
                bytes: size,
                reason: format!("unsupported type {other}"),
            }
        }
    })
}

/// Smallest serialized size of one item of `ty`, used to reject impossible
/// element counts. A true lower bound except for structs: a binary struct
/// without members would take 0 bytes, but none exists in the shipped data,
/// so arrays of structs are limited to one element per remaining byte.
fn item_min_size(ty: &PropertyType) -> usize {
    match ty {
        PropertyType::Byte { enum_path: Some(_) } => 8,
        PropertyType::Byte { enum_path: None } | PropertyType::Bool => 1,
        PropertyType::Name => 8,
        PropertyType::Delegate { .. } => 12,
        PropertyType::Struct { .. } => 1,
        _ => 4,
    }
}

fn decode_array(
    r: &mut Reader<'_>,
    ctx: &mut ValueContext<'_>,
    inner: Option<&PropertyDef>,
    depth: usize,
) -> ObjResult<Value> {
    let Some(inner) = inner else {
        let at = r.position();
        let count = r.read_count("array element count", 1)?;
        let bytes = r.remaining();
        if count > 0 {
            ctx.warn(
                at,
                format!("array of {count} elements with unknown element type; kept raw"),
            );
        }
        r.skip(bytes)?;
        return Ok(Value::RawArray { count, bytes });
    };
    let count = r.read_count("array element count", item_min_size(&inner.ty))?;
    let mut items = Vec::with_capacity(count.min(MAX_PREALLOC));
    for _ in 0..count {
        items.push(decode_item(r, ctx, inner, depth + 1)?);
    }
    Ok(Value::Array(items))
}

/// Decode one item (array element or binary struct member) of `def`'s type.
pub fn decode_item(
    r: &mut Reader<'_>,
    ctx: &mut ValueContext<'_>,
    def: &PropertyDef,
    depth: usize,
) -> ObjResult<Value> {
    if depth > MAX_VALUE_DEPTH {
        return Err(ObjectError::TooDeep {
            limit: MAX_VALUE_DEPTH,
            offset: r.position(),
        });
    }
    let at = r.position();
    ctx.charge(at)?;
    Ok(match &def.ty {
        // An enum-typed byte is serialized as the enumerator's FName; a plain
        // byte as one byte (CONFIRMED by exact consumption of arrays of enums).
        PropertyType::Byte { enum_path: Some(_) } => Value::Enum(read_name(r, ctx, "enum item")?),
        PropertyType::Byte { enum_path: None } => Value::Byte(r.read_u8()?),
        PropertyType::Int => Value::Int(r.read_i32()?),
        PropertyType::Float => Value::Float(r.read_f32()?),
        PropertyType::Bool => Value::Bool(r.read_u8()? != 0),
        PropertyType::Str => Value::Str(r.read_fstring()?),
        PropertyType::Name => Value::Name(read_name(r, ctx, "name item")?),
        PropertyType::Object { .. }
        | PropertyType::Class { .. }
        | PropertyType::Component { .. } => {
            let idx = r.read_package_index()?;
            Value::Object(ctx.obj_ref(idx, at))
        }
        PropertyType::Interface { .. } => {
            let idx = r.read_package_index()?;
            Value::Interface(ctx.obj_ref(idx, at))
        }
        PropertyType::Delegate { .. } => {
            let idx = r.read_package_index()?;
            let object = ctx.obj_ref(idx, at);
            let function = read_name(r, ctx, "delegate item function")?;
            Value::Delegate { object, function }
        }
        PropertyType::Struct { struct_path } => {
            let sdef = ctx.schema.struct_def(struct_path);
            let name = crate::schema::last_component(struct_path).to_owned();
            decode_struct(r, ctx, &name, sdef, false, depth)?
        }
        PropertyType::Array { inner } => {
            let count = r.read_count("nested array element count", item_min_size(&inner.ty))?;
            let mut items = Vec::with_capacity(count.min(MAX_PREALLOC));
            for _ in 0..count {
                items.push(decode_item(r, ctx, inner, depth + 1)?);
            }
            Value::Array(items)
        }
        PropertyType::Map { .. } => {
            return Err(ObjectError::Malformed {
                what: "map item",
                offset: at,
                detail: "MapProperty values are not serialized in this format".to_owned(),
            });
        }
    })
}

/// True when a struct member is written by binary struct serialization.
///
/// Every member is written, `CPF_Transient` ones included (CONFIRMED: the
/// `CoverSlot` values in `Default__CoverLink` contain their transient members
/// and decode exactly only with them). No binary struct in the shipped
/// packages declares a `CPF_Native` member, so whether those would be skipped
/// is UNKNOWN; they are not skipped here.
pub fn serialized_in_binary(_def: &PropertyDef) -> bool {
    true
}

/// Decode a struct value. `bounded` is true when `r` ends exactly at the end
/// of the value (tag level), which allows probing when the struct is unknown.
fn decode_struct(
    r: &mut Reader<'_>,
    ctx: &mut ValueContext<'_>,
    name: &str,
    sdef: Option<Arc<StructDef>>,
    bounded: bool,
    depth: usize,
) -> ObjResult<Value> {
    if let Some(sdef) = sdef {
        if sdef.is_binary(ctx.cooked) {
            let fields = decode_binary_members(r, ctx, &sdef, depth)?;
            return Ok(Value::Struct {
                name: sdef.name.clone(),
                binary: true,
                fields,
            });
        }
        let fields = read_tagged(r, ctx, Some(&sdef.path), depth + 1)?;
        return Ok(Value::Struct {
            name: sdef.name.clone(),
            binary: false,
            fields,
        });
    }
    // Unknown struct: built-in layouts of the immutable Core structs first.
    if let Some(layout) = native_layout(name) {
        let start = r.position();
        let fields = decode_native_layout(r, layout)?;
        if !bounded || r.remaining() == 0 {
            return Ok(Value::Struct {
                name: name.to_owned(),
                binary: true,
                fields,
            });
        }
        r.seek(start)?;
    }
    if bounded {
        let start = r.position();
        let mut probe = r.clone();
        let mut sub = ValueContext::new(ctx.pkg, ctx.own_name, ctx.schema);
        // The probe spends the caller's budget, so nested probes cannot
        // multiply it.
        sub.set_work_budget(ctx.budget);
        let probed = read_tagged(&mut probe, &mut sub, None, depth + 1);
        ctx.budget = sub.budget;
        if sub.exhausted {
            ctx.exhausted = true;
            return Err(probed.err().unwrap_or(ObjectError::Malformed {
                what: "property values",
                offset: start,
                detail: "decoding work budget exhausted".to_owned(),
            }));
        }
        if let Ok(fields) = probed
            && probe.remaining() == 0
        {
            for w in sub.into_warnings() {
                ctx.warn(start, w);
            }
            r.seek(probe.position())?;
            return Ok(Value::Struct {
                name: name.to_owned(),
                binary: false,
                fields,
            });
        }
        let bytes = r.remaining();
        ctx.warn(
            start,
            format!("struct {name}: definition unknown; kept raw"),
        );
        r.skip(bytes)?;
        return Ok(Value::Raw {
            bytes,
            reason: format!("unknown struct {name}"),
        });
    }
    Err(ObjectError::NotFound(format!(
        "definition of struct {name}"
    )))
}

fn decode_binary_members(
    r: &mut Reader<'_>,
    ctx: &mut ValueContext<'_>,
    sdef: &StructDef,
    depth: usize,
) -> ObjResult<Vec<Property>> {
    let link = ctx.schema.property_link(&sdef.path);
    let mut fields = Vec::new();
    for def in link.iter().filter(|d| serialized_in_binary(d)) {
        let dim = def.array_dim.max(1);
        for i in 0..dim {
            let offset = r.position();
            let value = decode_item(r, ctx, def, depth + 1)?;
            fields.push(Property {
                name: def.name.clone(),
                type_name: def.ty.class_name().to_owned(),
                array_index: i,
                size: r.position().saturating_sub(offset),
                struct_name: match &def.ty {
                    PropertyType::Struct { struct_path } => {
                        Some(crate::schema::last_component(struct_path).to_owned())
                    }
                    _ => None,
                },
                enum_name: None,
                value,
                offset,
            });
        }
    }
    Ok(fields)
}

/// Member type in a built-in layout.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NativeMember {
    /// `float`.
    F32,
    /// `int`.
    I32,
    /// `byte`.
    U8,
    /// Nested `Vector` (3 floats).
    Vector,
    /// Nested `Plane` (4 floats).
    Plane,
}

/// Built-in member layouts of the immutable `Core.Object` structs, used when
/// no schema is available. Member names and order are the binary
/// serialization order derived from the script declarations in `Core.u` (a
/// real-data test checks they agree with the schema).
pub const NATIVE_LAYOUTS: &[(&str, &[(&str, NativeMember)])] = {
    use NativeMember::*;
    &[
        ("Vector", &[("X", F32), ("Y", F32), ("Z", F32)]),
        ("Rotator", &[("Pitch", I32), ("Yaw", I32), ("Roll", I32)]),
        ("Color", &[("B", U8), ("G", U8), ("R", U8), ("A", U8)]),
        (
            "LinearColor",
            &[("R", F32), ("G", F32), ("B", F32), ("A", F32)],
        ),
        ("Guid", &[("A", I32), ("B", I32), ("C", I32), ("D", I32)]),
        ("Vector2D", &[("X", F32), ("Y", F32)]),
        ("Vector4", &[("X", F32), ("Y", F32), ("Z", F32), ("W", F32)]),
        // Plane extends Vector: its own member W precedes the inherited X, Y, Z.
        ("Plane", &[("W", F32), ("X", F32), ("Y", F32), ("Z", F32)]),
        ("Quat", &[("X", F32), ("Y", F32), ("Z", F32), ("W", F32)]),
        ("IntPoint", &[("X", I32), ("Y", I32)]),
        (
            "PackedNormal",
            &[("X", U8), ("Y", U8), ("Z", U8), ("W", U8)],
        ),
        ("Box", &[("Min", Vector), ("Max", Vector), ("IsValid", U8)]),
        ("TwoVectors", &[("v1", Vector), ("v2", Vector)]),
        (
            "Matrix",
            &[
                ("XPlane", Plane),
                ("YPlane", Plane),
                ("ZPlane", Plane),
                ("WPlane", Plane),
            ],
        ),
    ]
};

/// Built-in layout for an immutable Core struct name.
pub fn native_layout(name: &str) -> Option<&'static [(&'static str, NativeMember)]> {
    NATIVE_LAYOUTS
        .iter()
        .find(|(n, _)| eq(n, name))
        .map(|(_, l)| *l)
}

fn decode_native_layout(
    r: &mut Reader<'_>,
    layout: &[(&str, NativeMember)],
) -> ObjResult<Vec<Property>> {
    let mut out = Vec::with_capacity(layout.len());
    for &(name, m) in layout {
        let offset = r.position();
        let (type_name, struct_name, value) = match m {
            NativeMember::F32 => ("FloatProperty", None, Value::Float(r.read_f32()?)),
            NativeMember::I32 => ("IntProperty", None, Value::Int(r.read_i32()?)),
            NativeMember::U8 => ("ByteProperty", None, Value::Byte(r.read_u8()?)),
            NativeMember::Vector => {
                let fields = decode_native_layout(r, native_layout("Vector").unwrap_or(&[]))?;
                (
                    "StructProperty",
                    Some("Vector".to_owned()),
                    Value::Struct {
                        name: "Vector".to_owned(),
                        binary: true,
                        fields,
                    },
                )
            }
            NativeMember::Plane => {
                let fields = decode_native_layout(r, native_layout("Plane").unwrap_or(&[]))?;
                (
                    "StructProperty",
                    Some("Plane".to_owned()),
                    Value::Struct {
                        name: "Plane".to_owned(),
                        binary: true,
                        fields,
                    },
                )
            }
        };
        out.push(Property {
            name: name.to_owned(),
            type_name: type_name.to_owned(),
            array_index: 0,
            size: r.position().saturating_sub(offset),
            struct_name,
            enum_name: None,
            value,
            offset,
        });
    }
    Ok(out)
}

impl Value {
    /// Compact one-line rendering for text output.
    pub fn render(&self) -> String {
        match self {
            Value::Int(v) => v.to_string(),
            Value::Float(v) => format!("{v:?}"),
            Value::Bool(v) => v.to_string(),
            Value::Byte(v) => v.to_string(),
            Value::Enum(n) => n.clone(),
            Value::Name(n) => format!("'{n}'"),
            Value::Str(s) => format!("{s:?}"),
            Value::Object(o) | Value::Interface(o) => o.path.clone(),
            Value::Delegate { object, function } => format!("{}.{function}", object.path),
            Value::Array(items) => format!(
                "[{}]",
                items
                    .iter()
                    .map(Value::render)
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            Value::RawArray { count, bytes } => format!("<array of {count}, {bytes} raw bytes>"),
            Value::Struct { fields, .. } => format!(
                "({})",
                fields
                    .iter()
                    .map(|f| {
                        if f.array_index > 0 {
                            format!("{}[{}]={}", f.name, f.array_index, f.value.render())
                        } else {
                            format!("{}={}", f.name, f.value.render())
                        }
                    })
                    .collect::<Vec<_>>()
                    .join(",")
            ),
            Value::Raw { bytes, reason } => format!("<{bytes} raw bytes: {reason}>"),
        }
    }

    /// True when this value or a nested one is raw (undecoded).
    pub fn has_raw(&self) -> bool {
        match self {
            Value::Raw { .. } | Value::RawArray { .. } => true,
            Value::Array(items) => items.iter().any(Value::has_raw),
            Value::Struct { fields, .. } => fields.iter().any(|f| f.value.has_raw()),
            _ => false,
        }
    }
}
