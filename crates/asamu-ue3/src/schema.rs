//! Type information needed to decode property *values*: property definitions
//! (from `*Property` exports) and struct/class layouts (from `Class`,
//! `ScriptStruct`, `Function` and `State` exports).
//!
//! The tagged-property stream is self-delimiting (every tag carries its size),
//! so it can always be walked without type information. Decoding the *bytes*
//! of an array, or of a struct stored in binary form, needs the declaring
//! property's definition; [`Schema`] provides it. [`crate::model::PackageSet`]
//! implements it across packages; [`NoSchema`] provides nothing.

use std::sync::Arc;

use serde::Serialize;

use crate::flags;

/// Which kind of `UStruct` a [`StructDef`] describes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum StructKind {
    /// `Core.Class`.
    Class,
    /// `Core.ScriptStruct`.
    ScriptStruct,
    /// `Core.Function`.
    Function,
    /// `Core.State`.
    State,
}

/// Type of a declared property, with its references resolved to qualified
/// object paths (`Package.Outer.Name`).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub enum PropertyType {
    /// `ByteProperty`, optionally typed by an enum.
    Byte {
        /// Qualified path of the `Enum`, when the byte is an enum.
        enum_path: Option<String>,
    },
    /// `IntProperty`.
    Int,
    /// `FloatProperty`.
    Float,
    /// `BoolProperty`.
    Bool,
    /// `StrProperty`.
    Str,
    /// `NameProperty`.
    Name,
    /// `ObjectProperty`.
    Object {
        /// Qualified path of the property class.
        class: String,
    },
    /// `ClassProperty` (`class<MetaClass>`).
    Class {
        /// Qualified path of the property class (normally `Core.Class`).
        class: String,
        /// Qualified path of the meta class.
        meta_class: String,
    },
    /// `ComponentProperty`.
    Component {
        /// Qualified path of the component class.
        class: String,
    },
    /// `InterfaceProperty`.
    Interface {
        /// Qualified path of the interface class.
        class: String,
    },
    /// `StructProperty`.
    Struct {
        /// Qualified path of the `ScriptStruct`.
        struct_path: String,
    },
    /// `ArrayProperty` (dynamic array).
    Array {
        /// Element property (an export nested under the array property).
        inner: Box<PropertyDef>,
    },
    /// `MapProperty`.
    Map {
        /// Key property.
        key: Option<Box<PropertyDef>>,
        /// Value property.
        value: Option<Box<PropertyDef>>,
    },
    /// `DelegateProperty`.
    Delegate {
        /// Qualified path of the delegate signature function.
        function: String,
        /// Qualified path of the source delegate, when serialized as non-null.
        source: Option<String>,
    },
}

impl PropertyType {
    /// UE3 class name of the property (`IntProperty`, ...).
    pub fn class_name(&self) -> &'static str {
        match self {
            PropertyType::Byte { .. } => "ByteProperty",
            PropertyType::Int => "IntProperty",
            PropertyType::Float => "FloatProperty",
            PropertyType::Bool => "BoolProperty",
            PropertyType::Str => "StrProperty",
            PropertyType::Name => "NameProperty",
            PropertyType::Object { .. } => "ObjectProperty",
            PropertyType::Class { .. } => "ClassProperty",
            PropertyType::Component { .. } => "ComponentProperty",
            PropertyType::Interface { .. } => "InterfaceProperty",
            PropertyType::Struct { .. } => "StructProperty",
            PropertyType::Array { .. } => "ArrayProperty",
            PropertyType::Map { .. } => "MapProperty",
            PropertyType::Delegate { .. } => "DelegateProperty",
        }
    }

    /// UnrealScript-style type text (`int`, `array<Vector>`, `class<Actor>`, ...).
    pub fn describe(&self) -> String {
        match self {
            PropertyType::Byte { enum_path: Some(e) } => last_component(e).to_owned(),
            PropertyType::Byte { enum_path: None } => "byte".to_owned(),
            PropertyType::Int => "int".to_owned(),
            PropertyType::Float => "float".to_owned(),
            PropertyType::Bool => "bool".to_owned(),
            PropertyType::Str => "string".to_owned(),
            PropertyType::Name => "name".to_owned(),
            PropertyType::Object { class } | PropertyType::Component { class } => {
                last_component(class).to_owned()
            }
            PropertyType::Class { meta_class, .. } => {
                format!("class<{}>", last_component(meta_class))
            }
            PropertyType::Interface { class } => format!("interface {}", last_component(class)),
            PropertyType::Struct { struct_path } => last_component(struct_path).to_owned(),
            PropertyType::Array { inner } => format!("array<{}>", inner.ty.describe()),
            PropertyType::Map { key, value } => format!(
                "map{{{}, {}}}",
                key.as_ref().map_or("?".to_owned(), |k| k.ty.describe()),
                value.as_ref().map_or("?".to_owned(), |v| v.ty.describe())
            ),
            PropertyType::Delegate { function, .. } => {
                format!("delegate<{}>", last_component(function))
            }
        }
    }
}

/// Last dotted component of a path.
pub fn last_component(path: &str) -> &str {
    path.rsplit('.').next().unwrap_or(path)
}

/// One declared property (a `*Property` export).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PropertyDef {
    /// Property name.
    pub name: String,
    /// Qualified path of the property object.
    pub path: String,
    /// `ArrayDim` (static array size; 1 for scalars).
    pub array_dim: i32,
    /// Raw `PropertyFlags`.
    pub flags: u64,
    /// Editor category name (`None` when absent).
    pub category: String,
    /// Qualified path of the `ArrayEnum` (static array sized by an enum), if any.
    pub array_enum: Option<String>,
    /// Replication offset, present when the property has `CPF_Net`.
    pub rep_offset: Option<u16>,
    /// Type and type references.
    pub ty: PropertyType,
}

impl PropertyDef {
    /// Conventional names of the set property flags.
    pub fn flag_names(&self) -> Vec<String> {
        flags::describe(self.flags, flags::property::NAMES)
    }

    /// True when the flag bits in `mask` are all set.
    pub fn has(&self, mask: u64) -> bool {
        self.flags & mask == mask
    }
}

/// Layout of a class, script struct, function or state: its own properties
/// in declaration (children-chain) order plus the super struct's path.
#[derive(Debug, Clone, PartialEq)]
pub struct StructDef {
    /// Qualified path.
    pub path: String,
    /// Object name.
    pub name: String,
    /// Kind of struct.
    pub kind: StructKind,
    /// Qualified path of the super struct, if any.
    pub super_path: Option<String>,
    /// `StructFlags` for script structs (0 for the other kinds).
    pub struct_flags: u32,
    /// Properties declared directly on this struct, in children-chain order.
    pub properties: Vec<Arc<PropertyDef>>,
}

impl StructDef {
    /// True when values of this struct are stored in binary form rather than
    /// as tagged properties: `STRUCT_Immutable`, or `STRUCT_ImmutableWhenCooked`
    /// in a cooked package.
    pub fn is_binary(&self, cooked: bool) -> bool {
        self.kind == StructKind::ScriptStruct
            && (self.struct_flags & flags::structure::IMMUTABLE != 0
                || (cooked && self.struct_flags & flags::structure::IMMUTABLE_WHEN_COOKED != 0))
    }
}

/// Source of type information for value decoding.
pub trait Schema {
    /// Definition of the struct/class/function/state at `path` (qualified,
    /// compared case-insensitively).
    fn struct_def(&self, path: &str) -> Option<Arc<StructDef>>;

    /// Definition of the script struct called `name` (the tag only records the
    /// struct's name). Used when no declaring property is known.
    fn struct_by_name(&self, name: &str) -> Option<Arc<StructDef>>;

    /// Property `name` declared on `owner` or one of its super structs.
    fn find_property(&self, owner: &str, name: &str) -> Option<Arc<PropertyDef>>;

    /// Every property of `owner` in serialization order (UE3 `PropertyLink`):
    /// the struct's own properties in declaration order, then its super
    /// struct's link. CONFIRMED: tagged properties of class default objects
    /// appear in this order, and binary `Plane` values (a `Vector` subclass)
    /// store `W` before `X, Y, Z`.
    fn property_link(&self, owner: &str) -> Vec<Arc<PropertyDef>>;

    /// Enumerator names of the enum at `path`.
    fn enum_names(&self, path: &str) -> Option<Arc<Vec<String>>>;

    /// Lower-case class names from `class_path` up to the root, nearest first
    /// (empty when the class is unknown).
    fn class_chain(&self, class_path: &str) -> Vec<String>;
}

/// A [`Schema`] that knows nothing: values needing type information are kept
/// raw (with a warning) and components are not recognized.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoSchema;

impl Schema for NoSchema {
    fn struct_def(&self, _path: &str) -> Option<Arc<StructDef>> {
        None
    }
    fn struct_by_name(&self, _name: &str) -> Option<Arc<StructDef>> {
        None
    }
    fn find_property(&self, _owner: &str, _name: &str) -> Option<Arc<PropertyDef>> {
        None
    }
    fn property_link(&self, _owner: &str) -> Vec<Arc<PropertyDef>> {
        Vec::new()
    }
    fn enum_names(&self, _path: &str) -> Option<Arc<Vec<String>>> {
        None
    }
    fn class_chain(&self, _class_path: &str) -> Vec<String> {
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn def(name: &str, ty: PropertyType) -> PropertyDef {
        PropertyDef {
            name: name.to_owned(),
            path: format!("Pkg.Owner.{name}"),
            array_dim: 1,
            flags: 0,
            category: "None".to_owned(),
            array_enum: None,
            rep_offset: None,
            ty,
        }
    }

    #[test]
    fn type_descriptions() {
        assert_eq!(PropertyType::Int.describe(), "int");
        assert_eq!(
            PropertyType::Byte {
                enum_path: Some("Engine.Actor.EPhysics".into())
            }
            .describe(),
            "EPhysics"
        );
        let inner = def(
            "Inner",
            PropertyType::Struct {
                struct_path: "Core.Object.Vector".into(),
            },
        );
        assert_eq!(
            PropertyType::Array {
                inner: Box::new(inner)
            }
            .describe(),
            "array<Vector>"
        );
        assert_eq!(
            PropertyType::Class {
                class: "Core.Class".into(),
                meta_class: "Engine.Actor".into()
            }
            .describe(),
            "class<Actor>"
        );
        assert_eq!(last_component("NoDots"), "NoDots");
    }

    #[test]
    fn binary_struct_rule() {
        let mut s = StructDef {
            path: "Core.Object.Vector".into(),
            name: "Vector".into(),
            kind: StructKind::ScriptStruct,
            super_path: None,
            struct_flags: 0x30,
            properties: Vec::new(),
        };
        assert!(s.is_binary(false));
        s.struct_flags = 0x181;
        assert!(s.is_binary(true));
        assert!(!s.is_binary(false));
        s.struct_flags = 0x1;
        assert!(!s.is_binary(true));
        s.kind = StructKind::Class;
        s.struct_flags = 0x30;
        assert!(!s.is_binary(true));
    }
}
