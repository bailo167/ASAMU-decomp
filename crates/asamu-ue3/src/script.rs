//! Script object payloads (v868): `Class`, `State`, `Function`,
//! `ScriptStruct`, `Enum`, `Const`, `TextBuffer` and every `*Property`.
//!
//! Every layout below is proven by exact consumption: for all 70,946 such
//! exports in the 12 `.u` packages and `Startup.upk` (the maps contain none)
//! the decoder consumes exactly `SerialSize` bytes (see
//! `docs/reverse-engineering/OBJECT_FORMAT.md`).
//!
//! ```text
//! UObject      i32 NetIndex | tagged properties ("None" only; absent for Class)
//! UField       i32 Next
//! UStruct      i32 SuperStruct | i32 ScriptText | i32 Children | i32 CppText
//!              | i32 Line | i32 TextPos | i32 ScriptBytecodeSize (in memory)
//!              | i32 ScriptStorageSize (on disk) | ScriptStorageSize bytes of bytecode
//! UFunction    UStruct | u16 iNative | u8 OperPrecedence | u32 FunctionFlags
//!              | u16 RepOffset (FUNC_Net only) | FName FriendlyName
//! UState       UStruct | u32 ProbeMask | u16 LabelTableOffset | u32 StateFlags
//!              | TMap<FName, i32 Function> FuncMap
//! UClass       UState | u32 ClassFlags | i32 ClassWithin | FName ClassConfigName
//!              | TMap<FName, i32 Component> ComponentNameToDefaultObjectMap
//!              | TArray<{i32 Class, i32 PointerProperty}> Interfaces
//!              | TArray<FName> DontSortCategories | TArray<FName> HideCategories
//!              | TArray<FName> AutoExpandCategories | TArray<FName> AutoCollapseCategories
//!              | u32 bForceScriptOrder | TArray<FName> ClassGroupNames
//!              | FString ClassHeaderFilename | FName DLLBindName | i32 ClassDefaultObject
//! UScriptStruct UStruct | u32 StructFlags | tagged struct defaults
//! UProperty    UField | i32 ArrayDim | u64 PropertyFlags | FName Category
//!              | i32 ArrayEnum | u16 RepOffset (CPF_Net only) | per-type fields
//! UEnum        UField | TArray<FName> Names
//! UConst       UField | FString Value
//! UTextBuffer  UObject | i32 Pos | i32 Top | FString Text
//! ```

use serde::Serialize;

use crate::flags;
use crate::object::{ObjResult, ObjectError, PreludeRules, qualified_path, read_prelude};
use crate::package::Package;
use crate::property::{self, Property, ValueContext};
use crate::reader::Reader;
use crate::schema::Schema;
use crate::types::{FName, PackageIndex};

/// Kind of a script object (its intrinsic `Core` class).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
pub enum ScriptKind {
    /// `Core.Class`.
    Class,
    /// `Core.State`.
    State,
    /// `Core.Function`.
    Function,
    /// `Core.ScriptStruct`.
    ScriptStruct,
    /// `Core.Enum`.
    Enum,
    /// `Core.Const`.
    Const,
    /// `Core.TextBuffer`.
    TextBuffer,
    /// `Core.ByteProperty`.
    ByteProperty,
    /// `Core.IntProperty`.
    IntProperty,
    /// `Core.FloatProperty`.
    FloatProperty,
    /// `Core.BoolProperty`.
    BoolProperty,
    /// `Core.StrProperty`.
    StrProperty,
    /// `Core.NameProperty`.
    NameProperty,
    /// `Core.ObjectProperty`.
    ObjectProperty,
    /// `Core.ClassProperty`.
    ClassProperty,
    /// `Core.ComponentProperty`.
    ComponentProperty,
    /// `Core.InterfaceProperty`.
    InterfaceProperty,
    /// `Core.StructProperty`.
    StructProperty,
    /// `Core.ArrayProperty`.
    ArrayProperty,
    /// `Core.MapProperty`.
    MapProperty,
    /// `Core.DelegateProperty`.
    DelegateProperty,
}

impl ScriptKind {
    /// Every kind, in a stable order.
    pub const ALL: [ScriptKind; 21] = [
        ScriptKind::Class,
        ScriptKind::State,
        ScriptKind::Function,
        ScriptKind::ScriptStruct,
        ScriptKind::Enum,
        ScriptKind::Const,
        ScriptKind::TextBuffer,
        ScriptKind::ByteProperty,
        ScriptKind::IntProperty,
        ScriptKind::FloatProperty,
        ScriptKind::BoolProperty,
        ScriptKind::StrProperty,
        ScriptKind::NameProperty,
        ScriptKind::ObjectProperty,
        ScriptKind::ClassProperty,
        ScriptKind::ComponentProperty,
        ScriptKind::InterfaceProperty,
        ScriptKind::StructProperty,
        ScriptKind::ArrayProperty,
        ScriptKind::MapProperty,
        ScriptKind::DelegateProperty,
    ];

    /// Class name (`Function`, `IntProperty`, ...).
    pub fn name(self) -> &'static str {
        match self {
            ScriptKind::Class => "Class",
            ScriptKind::State => "State",
            ScriptKind::Function => "Function",
            ScriptKind::ScriptStruct => "ScriptStruct",
            ScriptKind::Enum => "Enum",
            ScriptKind::Const => "Const",
            ScriptKind::TextBuffer => "TextBuffer",
            ScriptKind::ByteProperty => "ByteProperty",
            ScriptKind::IntProperty => "IntProperty",
            ScriptKind::FloatProperty => "FloatProperty",
            ScriptKind::BoolProperty => "BoolProperty",
            ScriptKind::StrProperty => "StrProperty",
            ScriptKind::NameProperty => "NameProperty",
            ScriptKind::ObjectProperty => "ObjectProperty",
            ScriptKind::ClassProperty => "ClassProperty",
            ScriptKind::ComponentProperty => "ComponentProperty",
            ScriptKind::InterfaceProperty => "InterfaceProperty",
            ScriptKind::StructProperty => "StructProperty",
            ScriptKind::ArrayProperty => "ArrayProperty",
            ScriptKind::MapProperty => "MapProperty",
            ScriptKind::DelegateProperty => "DelegateProperty",
        }
    }

    /// Kind for a class name (exact match).
    pub fn from_class_name(name: &str) -> Option<ScriptKind> {
        ScriptKind::ALL.into_iter().find(|k| k.name() == name)
    }

    /// True for the `*Property` kinds.
    pub fn is_property(self) -> bool {
        self.name().ends_with("Property")
    }

    /// True for the `UStruct` kinds (Class, State, Function, ScriptStruct).
    pub fn is_struct(self) -> bool {
        matches!(
            self,
            ScriptKind::Class | ScriptKind::State | ScriptKind::Function | ScriptKind::ScriptStruct
        )
    }

    /// Script kind of export `index`: its class must be one of the intrinsic
    /// `Core` classes above (`Class` exports have a null class index).
    pub fn of_export(pkg: &Package, index: usize) -> Option<ScriptKind> {
        let e = pkg.export(index).ok()?;
        if e.class_index.is_null() {
            return Some(ScriptKind::Class);
        }
        let name = pkg.export_class_name(index).ok()?;
        let kind = ScriptKind::from_class_name(&name)?;
        // The intrinsic classes are native-only: always imports from `Core`.
        match pkg.export_class_package(index).ok()? {
            Some(p) if p.eq_ignore_ascii_case("Core") => Some(kind),
            _ => None,
        }
    }
}

/// `UStruct` fields.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct StructHeader {
    /// `SuperStruct` (equals the export table's `SuperIndex` in every shipped package).
    pub super_struct: PackageIndex,
    /// `ScriptText` (`TextBuffer` with the source; classes only in practice).
    pub script_text: PackageIndex,
    /// `Children`: first field of the children chain.
    pub children: PackageIndex,
    /// `CppText` (always null in the shipped packages).
    pub cpp_text: PackageIndex,
    /// `Line` (source line; -1 for classes without script code).
    pub line: i32,
    /// `TextPos` (source position).
    pub text_pos: i32,
    /// `ScriptBytecodeSize` (in-memory bytecode size).
    pub bytecode_size: i32,
    /// `ScriptStorageSize` (bytecode bytes on disk).
    pub storage_size: usize,
    /// Payload offset of the bytecode.
    pub bytecode_offset: usize,
}

/// `UFunction` fields.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FunctionData {
    /// `iNative` (native function index; 0 when not natively indexed).
    pub native_index: u16,
    /// `OperPrecedence`.
    pub operator_precedence: u8,
    /// `FunctionFlags`.
    pub function_flags: u32,
    /// `RepOffset`, present with `FUNC_Net`.
    pub rep_offset: Option<u16>,
    /// `FriendlyName` (operator symbol or the function name).
    pub friendly_name: String,
}

/// `UState` fields.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct StateData {
    /// `ProbeMask`.
    pub probe_mask: u32,
    /// `LabelTableOffset` (0xFFFF when the state has no labels).
    pub label_table_offset: u16,
    /// `StateFlags`.
    pub state_flags: u32,
    /// `FuncMap`: function name to function object.
    pub func_map: Vec<(String, PackageIndex)>,
}

/// `UClass` fields.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ClassData {
    /// `ClassFlags`.
    pub class_flags: u32,
    /// `ClassWithin`.
    pub within: PackageIndex,
    /// `ClassConfigName` (`None` when not configurable).
    pub config_name: String,
    /// `ComponentNameToDefaultObjectMap`.
    pub components: Vec<(String, PackageIndex)>,
    /// `Interfaces`: (interface class, pointer property).
    pub interfaces: Vec<(PackageIndex, PackageIndex)>,
    /// `DontSortCategories`.
    pub dont_sort_categories: Vec<String>,
    /// `HideCategories`.
    pub hide_categories: Vec<String>,
    /// `AutoExpandCategories`.
    pub auto_expand_categories: Vec<String>,
    /// `AutoCollapseCategories`.
    pub auto_collapse_categories: Vec<String>,
    /// `bForceScriptOrder` (raw `u32`).
    pub force_script_order: u32,
    /// `ClassGroupNames`.
    pub class_groups: Vec<String>,
    /// `ClassHeaderFilename` (native header group, empty for script classes).
    pub native_header: String,
    /// `DLLBindName` (`None` everywhere).
    pub dll_bind_name: String,
    /// `ClassDefaultObject`.
    pub default_object: PackageIndex,
}

/// Per-type fields of a `UProperty`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub enum PropertyKindData {
    /// `ByteProperty`: `Enum`.
    Byte {
        /// Enum (null for plain bytes).
        enum_: PackageIndex,
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
    /// `ObjectProperty`: `PropertyClass`.
    Object {
        /// Property class.
        class: PackageIndex,
    },
    /// `ClassProperty`: `PropertyClass`, `MetaClass`.
    Class {
        /// Property class.
        class: PackageIndex,
        /// Meta class.
        meta_class: PackageIndex,
    },
    /// `ComponentProperty`: `PropertyClass`.
    Component {
        /// Component class.
        class: PackageIndex,
    },
    /// `InterfaceProperty`: `InterfaceClass`.
    Interface {
        /// Interface class.
        class: PackageIndex,
    },
    /// `StructProperty`: `Struct`.
    Struct {
        /// Script struct.
        struct_: PackageIndex,
    },
    /// `ArrayProperty`: `Inner`.
    Array {
        /// Element property.
        inner: PackageIndex,
    },
    /// `MapProperty`: `Key`, `Value`.
    Map {
        /// Key property.
        key: PackageIndex,
        /// Value property.
        value: PackageIndex,
    },
    /// `DelegateProperty`: `Function`, `SourceDelegate`.
    Delegate {
        /// Signature function.
        function: PackageIndex,
        /// Source delegate property.
        source: PackageIndex,
    },
}

/// `UProperty` fields.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PropertyData {
    /// `ArrayDim`.
    pub array_dim: i32,
    /// `PropertyFlags`.
    pub flags: u64,
    /// `Category`.
    pub category: String,
    /// `ArrayEnum`.
    pub array_enum: PackageIndex,
    /// `RepOffset`, present with `CPF_Net`.
    pub rep_offset: Option<u16>,
    /// Per-type fields.
    pub kind: PropertyKindData,
}

/// Kind-specific payload body.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub enum ScriptBody {
    /// `UClass`.
    Class {
        /// `UStruct` part.
        structure: StructHeader,
        /// `UState` part.
        state: StateData,
        /// `UClass` part.
        class: Box<ClassData>,
    },
    /// `UState`.
    State {
        /// `UStruct` part.
        structure: StructHeader,
        /// `UState` part.
        state: StateData,
    },
    /// `UFunction`.
    Function {
        /// `UStruct` part.
        structure: StructHeader,
        /// `UFunction` part.
        function: FunctionData,
    },
    /// `UScriptStruct`.
    ScriptStruct {
        /// `UStruct` part.
        structure: StructHeader,
        /// `StructFlags`.
        struct_flags: u32,
        /// Struct defaults (tagged).
        defaults: Vec<Property>,
    },
    /// `UProperty` and subclasses.
    Property(PropertyData),
    /// `UEnum`.
    Enum {
        /// Enumerator names in value order.
        names: Vec<String>,
    },
    /// `UConst`.
    Const {
        /// Value text.
        value: String,
    },
    /// `UTextBuffer`. The text itself is not kept here; see [`text_buffer_text`].
    TextBuffer {
        /// `Pos`.
        pos: i32,
        /// `Top`.
        top: i32,
        /// Payload offset of the `Text` FString.
        text_offset: usize,
        /// Length of the text in characters.
        text_chars: usize,
    },
}

/// A decoded script object.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ScriptObject {
    /// Export index.
    pub export_index: usize,
    /// Kind.
    pub kind: ScriptKind,
    /// `NetIndex`.
    pub net_index: i32,
    /// Tagged properties before the field data (only `None` in practice).
    pub tagged: Vec<Property>,
    /// `UField::Next` (all kinds except `TextBuffer`).
    pub next: Option<PackageIndex>,
    /// Kind-specific body.
    pub body: ScriptBody,
    /// Payload size (= bytes consumed).
    pub size: usize,
}

impl ScriptObject {
    /// `UStruct` header for the struct kinds.
    pub fn structure(&self) -> Option<&StructHeader> {
        match &self.body {
            ScriptBody::Class { structure, .. }
            | ScriptBody::State { structure, .. }
            | ScriptBody::Function { structure, .. }
            | ScriptBody::ScriptStruct { structure, .. } => Some(structure),
            _ => None,
        }
    }

    /// Property fields for the property kinds.
    pub fn property(&self) -> Option<&PropertyData> {
        match &self.body {
            ScriptBody::Property(p) => Some(p),
            _ => None,
        }
    }
}

fn name(r: &mut Reader<'_>, pkg: &Package, what: &'static str) -> ObjResult<String> {
    let at = r.position();
    let n: FName = r.read_fname()?;
    pkg.try_fname(n).map_err(|e| ObjectError::Malformed {
        what,
        offset: at,
        detail: e.to_string(),
    })
}

fn names(r: &mut Reader<'_>, pkg: &Package, what: &'static str) -> ObjResult<Vec<String>> {
    let n = r.read_count(what, 8)?;
    let mut out = Vec::with_capacity(n.min(property::MAX_PREALLOC));
    for _ in 0..n {
        out.push(name(r, pkg, what)?);
    }
    Ok(out)
}

fn read_struct_header(r: &mut Reader<'_>) -> ObjResult<StructHeader> {
    let super_struct = r.read_package_index()?;
    let script_text = r.read_package_index()?;
    let children = r.read_package_index()?;
    let cpp_text = r.read_package_index()?;
    let line = r.read_i32()?;
    let text_pos = r.read_i32()?;
    let bytecode_size = r.read_i32()?;
    let storage_size = r.read_non_negative("ScriptStorageSize")? as usize;
    let bytecode_offset = r.position();
    r.skip(storage_size)?;
    Ok(StructHeader {
        super_struct,
        script_text,
        children,
        cpp_text,
        line,
        text_pos,
        bytecode_size,
        storage_size,
        bytecode_offset,
    })
}

fn read_state(r: &mut Reader<'_>, pkg: &Package) -> ObjResult<StateData> {
    let probe_mask = r.read_u32()?;
    let label_table_offset = r.read_u16()?;
    let state_flags = r.read_u32()?;
    let n = r.read_count("State.FuncMap", 12)?;
    let mut func_map = Vec::with_capacity(n.min(property::MAX_PREALLOC));
    for _ in 0..n {
        let k = name(r, pkg, "State.FuncMap key")?;
        func_map.push((k, r.read_package_index()?));
    }
    Ok(StateData {
        probe_mask,
        label_table_offset,
        state_flags,
        func_map,
    })
}

fn read_class(r: &mut Reader<'_>, pkg: &Package) -> ObjResult<ClassData> {
    let class_flags = r.read_u32()?;
    let within = r.read_package_index()?;
    let config_name = name(r, pkg, "Class.ConfigName")?;
    let n = r.read_count("Class.ComponentNameToDefaultObjectMap", 12)?;
    let mut components = Vec::with_capacity(n.min(property::MAX_PREALLOC));
    for _ in 0..n {
        let k = name(r, pkg, "Class.ComponentNameToDefaultObjectMap key")?;
        components.push((k, r.read_package_index()?));
    }
    let interfaces = r.read_tarray("Class.Interfaces", 8, |r| {
        Ok((r.read_package_index()?, r.read_package_index()?))
    })?;
    let dont_sort_categories = names(r, pkg, "Class.DontSortCategories")?;
    let hide_categories = names(r, pkg, "Class.HideCategories")?;
    let auto_expand_categories = names(r, pkg, "Class.AutoExpandCategories")?;
    let auto_collapse_categories = names(r, pkg, "Class.AutoCollapseCategories")?;
    let force_script_order = r.read_u32()?;
    let class_groups = names(r, pkg, "Class.ClassGroupNames")?;
    let native_header = r.read_fstring()?;
    let dll_bind_name = name(r, pkg, "Class.DLLBindName")?;
    let default_object = r.read_package_index()?;
    Ok(ClassData {
        class_flags,
        within,
        config_name,
        components,
        interfaces,
        dont_sort_categories,
        hide_categories,
        auto_expand_categories,
        auto_collapse_categories,
        force_script_order,
        class_groups,
        native_header,
        dll_bind_name,
        default_object,
    })
}

fn read_property(r: &mut Reader<'_>, pkg: &Package, kind: ScriptKind) -> ObjResult<PropertyData> {
    let array_dim = r.read_i32()?;
    let flags = r.read_u64()?;
    let category = name(r, pkg, "Property.Category")?;
    let array_enum = r.read_package_index()?;
    let rep_offset = if flags & flags::property::NET != 0 {
        Some(r.read_u16()?)
    } else {
        None
    };
    let kind = match kind {
        ScriptKind::ByteProperty => PropertyKindData::Byte {
            enum_: r.read_package_index()?,
        },
        ScriptKind::IntProperty => PropertyKindData::Int,
        ScriptKind::FloatProperty => PropertyKindData::Float,
        ScriptKind::BoolProperty => PropertyKindData::Bool,
        ScriptKind::StrProperty => PropertyKindData::Str,
        ScriptKind::NameProperty => PropertyKindData::Name,
        ScriptKind::ObjectProperty => PropertyKindData::Object {
            class: r.read_package_index()?,
        },
        ScriptKind::ClassProperty => PropertyKindData::Class {
            class: r.read_package_index()?,
            meta_class: r.read_package_index()?,
        },
        ScriptKind::ComponentProperty => PropertyKindData::Component {
            class: r.read_package_index()?,
        },
        ScriptKind::InterfaceProperty => PropertyKindData::Interface {
            class: r.read_package_index()?,
        },
        ScriptKind::StructProperty => PropertyKindData::Struct {
            struct_: r.read_package_index()?,
        },
        ScriptKind::ArrayProperty => PropertyKindData::Array {
            inner: r.read_package_index()?,
        },
        ScriptKind::MapProperty => PropertyKindData::Map {
            key: r.read_package_index()?,
            value: r.read_package_index()?,
        },
        ScriptKind::DelegateProperty => PropertyKindData::Delegate {
            function: r.read_package_index()?,
            source: r.read_package_index()?,
        },
        other => {
            return Err(ObjectError::WrongKind {
                export: 0,
                expected: "property",
                found: other.name().to_owned(),
            });
        }
    };
    Ok(PropertyData {
        array_dim,
        flags,
        category,
        array_enum,
        rep_offset,
        kind,
    })
}

/// Decode script object `index` strictly: the decoder must consume exactly
/// `SerialSize` bytes, otherwise [`ObjectError::SizeMismatch`] is returned.
///
/// `own_name` qualifies export paths in struct defaults; `schema` supplies
/// types for struct default values (use [`crate::schema::NoSchema`] if none).
pub fn decode_script_object(
    pkg: &Package,
    own_name: Option<&str>,
    index: usize,
    schema: &dyn Schema,
) -> ObjResult<ScriptObject> {
    let kind = ScriptKind::of_export(pkg, index).ok_or_else(|| ObjectError::WrongKind {
        export: index,
        expected: "script object",
        found: pkg
            .export_class_name(index)
            .unwrap_or_else(|_| "<unknown>".to_owned()),
    })?;
    let data = pkg.export_data(index)?;
    let mut r = Reader::new(data);
    let prelude = read_prelude(&mut r, pkg, &PreludeRules::default())?;
    let mut ctx = ValueContext::new(pkg, own_name, schema);
    ctx.set_work_budget(property::work_budget_for(data.len()));
    let tagged = if kind == ScriptKind::Class {
        Vec::new()
    } else {
        property::read_tagged(&mut r, &mut ctx, None, 0)?
    };
    let next = if kind == ScriptKind::TextBuffer {
        None
    } else {
        Some(r.read_package_index()?)
    };
    let body = match kind {
        ScriptKind::Class => {
            let structure = read_struct_header(&mut r)?;
            let state = read_state(&mut r, pkg)?;
            let class = Box::new(read_class(&mut r, pkg)?);
            ScriptBody::Class {
                structure,
                state,
                class,
            }
        }
        ScriptKind::State => {
            let structure = read_struct_header(&mut r)?;
            let state = read_state(&mut r, pkg)?;
            ScriptBody::State { structure, state }
        }
        ScriptKind::Function => {
            let structure = read_struct_header(&mut r)?;
            let native_index = r.read_u16()?;
            let operator_precedence = r.read_u8()?;
            let function_flags = r.read_u32()?;
            let rep_offset = if function_flags & flags::function::NET != 0 {
                Some(r.read_u16()?)
            } else {
                None
            };
            let friendly_name = name(&mut r, pkg, "Function.FriendlyName")?;
            ScriptBody::Function {
                structure,
                function: FunctionData {
                    native_index,
                    operator_precedence,
                    function_flags,
                    rep_offset,
                    friendly_name,
                },
            }
        }
        ScriptKind::ScriptStruct => {
            let structure = read_struct_header(&mut r)?;
            let struct_flags = r.read_u32()?;
            let owner = qualified_path(
                pkg,
                own_name,
                PackageIndex::from_export(index).unwrap_or_default(),
            )?;
            let defaults = property::read_tagged(&mut r, &mut ctx, Some(&owner), 0)?;
            ScriptBody::ScriptStruct {
                structure,
                struct_flags,
                defaults,
            }
        }
        ScriptKind::Enum => ScriptBody::Enum {
            names: names(&mut r, pkg, "Enum.Names")?,
        },
        ScriptKind::Const => ScriptBody::Const {
            value: r.read_fstring()?,
        },
        ScriptKind::TextBuffer => {
            let pos = r.read_i32()?;
            let top = r.read_i32()?;
            let text_offset = r.position();
            let text = r.read_fstring()?;
            ScriptBody::TextBuffer {
                pos,
                top,
                text_offset,
                text_chars: text.chars().count(),
            }
        }
        k => ScriptBody::Property(read_property(&mut r, pkg, k).map_err(|e| match e {
            ObjectError::WrongKind {
                expected, found, ..
            } => ObjectError::WrongKind {
                export: index,
                expected,
                found,
            },
            e => e,
        })?),
    };
    if r.position() != data.len() {
        return Err(ObjectError::SizeMismatch {
            export: index,
            kind: kind.name().to_owned(),
            consumed: r.position(),
            size: data.len(),
        });
    }
    Ok(ScriptObject {
        export_index: index,
        kind,
        net_index: prelude.net_index,
        tagged,
        next,
        body,
        size: data.len(),
    })
}

/// Text of a `TextBuffer` export.
///
/// **Hygiene:** a class's `ScriptText` buffer holds the original, copyrighted
/// UnrealScript source. It may be read locally for understanding but must
/// never be committed, quoted or paraphrased into the repository.
pub fn text_buffer_text(pkg: &Package, index: usize) -> ObjResult<String> {
    let obj = decode_script_object(pkg, None, index, &crate::schema::NoSchema)?;
    let ScriptBody::TextBuffer { text_offset, .. } = obj.body else {
        return Err(ObjectError::WrongKind {
            export: index,
            expected: "TextBuffer",
            found: obj.kind.name().to_owned(),
        });
    };
    let data = pkg.export_data(index)?;
    let mut r = Reader::at(data, text_offset)?;
    Ok(r.read_fstring()?)
}

/// Exact-consumption statistics for one kind.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct KindCoverage {
    /// Exports of this kind.
    pub total: usize,
    /// Exports decoded with exact consumption.
    pub exact: usize,
    /// First few failures (`export index: error`).
    pub failures: Vec<String>,
}

/// Decode every script object of `pkg` strictly and count exact consumption
/// per kind.
pub fn script_coverage(
    pkg: &Package,
    own_name: Option<&str>,
) -> std::collections::BTreeMap<ScriptKind, KindCoverage> {
    let mut out: std::collections::BTreeMap<ScriptKind, KindCoverage> = Default::default();
    for i in 0..pkg.exports.len() {
        let Some(kind) = ScriptKind::of_export(pkg, i) else {
            continue;
        };
        let c = out.entry(kind).or_default();
        c.total += 1;
        match decode_script_object(pkg, own_name, i, &crate::schema::NoSchema) {
            Ok(_) => c.exact += 1,
            Err(e) => {
                if c.failures.len() < 8 {
                    c.failures.push(format!("{i}: {e}"));
                }
            }
        }
    }
    out
}
