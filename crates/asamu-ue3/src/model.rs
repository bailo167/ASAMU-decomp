//! Cross-package class model: a [`PackageSet`] opens packages by name from a
//! cooked folder, resolves qualified object paths (`Engine.Actor`,
//! `UTGame.UTPawn`, `Core.Object.Vector`), implements [`Schema`] for value
//! decoding, and builds [`ClassModel`]s and inherited class defaults.
//!
//! Qualified paths: an import's path already starts with its package; an
//! export is prefixed with its package file's name unless its outermost
//! object is itself a `Package` export (as in seek-free packages such as
//! `Startup.upk`, whose script packages `asamu` and `UTGame` are top-level
//! `Package` exports). Lookups are case-insensitive.

use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::Serialize;

use crate::compression::ReadOptions;
use crate::flags;
use crate::object::{DecodedObject, ObjResult, ObjectError, decode_object, qualified_path};
use crate::package::Package;
use crate::property::{Property, Value};
use crate::schema::NoSchema;
use crate::schema::{PropertyDef, PropertyType, Schema, StructDef, StructKind, last_component};
use crate::script::{
    PropertyData, PropertyKindData, ScriptBody, ScriptKind, ScriptObject, decode_script_object,
};
use crate::types::PackageIndex;

/// Longest super chain followed (guards against cycles).
pub const MAX_SUPER_DEPTH: usize = 64;
/// Longest children chain followed.
pub const MAX_CHILDREN: usize = 100_000;
/// Deepest nesting of array inner properties followed.
const MAX_INNER_DEPTH: usize = 8;

/// A package opened by a [`PackageSet`].
#[derive(Debug)]
pub struct LoadedPackage {
    /// The package's own name (file stem, e.g. `Engine`, `Startup`).
    pub name: String,
    /// File path.
    pub path: PathBuf,
    /// Parsed package.
    pub package: Package,
    by_path: HashMap<String, usize>,
    structs_by_name: HashMap<String, usize>,
}

impl LoadedPackage {
    /// Wrap a parsed package. `name` is its own name (file stem).
    pub fn new(name: impl Into<String>, path: impl Into<PathBuf>, package: Package) -> Self {
        let name = name.into();
        let mut by_path = HashMap::with_capacity(package.exports.len());
        let mut structs_by_name = HashMap::new();
        for i in 0..package.exports.len() {
            let Some(idx) = PackageIndex::from_export(i) else {
                continue;
            };
            if let Ok(p) = qualified_path(&package, Some(&name), idx) {
                by_path.entry(p.to_ascii_lowercase()).or_insert(i);
            }
            if ScriptKind::of_export(&package, i) == Some(ScriptKind::ScriptStruct)
                && let Some(e) = package.exports.get(i)
            {
                structs_by_name
                    .entry(package.fname(e.object_name).to_ascii_lowercase())
                    .or_insert(i);
            }
        }
        LoadedPackage {
            name,
            path: path.into(),
            package,
            by_path,
            structs_by_name,
        }
    }

    /// Export with this qualified path (case-insensitive).
    pub fn export_by_qualified(&self, path: &str) -> Option<usize> {
        self.by_path.get(&path.to_ascii_lowercase()).copied()
    }

    /// Find an export by qualified path, package-relative path, or (when
    /// unique among classes) bare class name.
    pub fn find(&self, path: &str) -> Option<usize> {
        if let Some(i) = self.export_by_qualified(path) {
            return Some(i);
        }
        let qualified = format!("{}.{path}", self.name);
        if let Some(i) = self.export_by_qualified(&qualified) {
            return Some(i);
        }
        if !path.contains('.') {
            let named = |i: &usize| {
                self.package
                    .exports
                    .get(*i)
                    .is_some_and(|e| self.package.fname(e.object_name).eq_ignore_ascii_case(path))
            };
            let is_class = |i: &usize| {
                self.package
                    .exports
                    .get(*i)
                    .is_some_and(|e| e.class_index.is_null())
            };
            // A unique class of that name first, then any unique export.
            for only_classes in [true, false] {
                let mut hits = (0..self.package.exports.len())
                    .filter(named)
                    .filter(|i| !only_classes || is_class(i));
                if let (Some(first), None) = (hits.next(), hits.next()) {
                    return Some(first);
                }
            }
        }
        None
    }

    /// Qualified path of export `i`.
    pub fn qualified(&self, i: usize) -> ObjResult<String> {
        let idx = PackageIndex::from_export(i)
            .ok_or_else(|| ObjectError::NotFound(format!("export {i}")))?;
        Ok(qualified_path(&self.package, Some(&self.name), idx)?)
    }

    /// Qualified path of any reference in this package (`None` for null).
    pub fn ref_path(&self, idx: PackageIndex) -> ObjResult<Option<String>> {
        if idx.is_null() {
            return Ok(None);
        }
        Ok(Some(qualified_path(&self.package, Some(&self.name), idx)?))
    }
}

/// Lower-case property name to definition, for one struct and its supers.
type PropertyLookup = HashMap<String, Arc<PropertyDef>>;

/// Packages of one cooked folder, opened on demand and cached.
#[derive(Debug)]
pub struct PackageSet {
    files: BTreeMap<String, PathBuf>,
    startup: Vec<String>,
    opts: ReadOptions,
    loaded: RefCell<HashMap<String, Arc<LoadedPackage>>>,
    failed: RefCell<HashMap<String, String>>,
    locate_cache: RefCell<HashMap<String, Option<(String, usize)>>>,
    defs: RefCell<HashMap<String, Option<Arc<StructDef>>>>,
    links: RefCell<HashMap<String, Arc<Vec<Arc<PropertyDef>>>>>,
    lookups: RefCell<HashMap<String, Arc<PropertyLookup>>>,
    enums: RefCell<HashMap<String, Option<Arc<Vec<String>>>>>,
    struct_names: RefCell<HashMap<String, Option<Arc<StructDef>>>>,
}

fn is_package_file(p: &Path) -> bool {
    let ext = p
        .extension()
        .map(|x| x.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    matches!(ext.as_str(), "u" | "upk" | "asamu")
}

fn stem_lower(p: &Path) -> Option<String> {
    p.file_stem()
        .map(|s| s.to_string_lossy().to_ascii_lowercase())
}

impl PackageSet {
    /// A set over the package files directly inside `dirs` (not recursive).
    /// Earlier directories win on name clashes. Missing directories are skipped.
    pub fn new<P: AsRef<Path>>(dirs: &[P]) -> PackageSet {
        let mut files = BTreeMap::new();
        for d in dirs {
            let Ok(rd) = std::fs::read_dir(d.as_ref()) else {
                continue;
            };
            let mut entries: Vec<PathBuf> = rd.flatten().map(|e| e.path()).collect();
            entries.sort();
            for p in entries {
                if p.is_file()
                    && is_package_file(&p)
                    && let Some(s) = stem_lower(&p)
                {
                    files.entry(s).or_insert(p);
                }
            }
        }
        // Seek-free startup packages hold script packages without .u files
        // (asamu, UTGame); search them when a package has no file of its own.
        let startup = files
            .keys()
            .filter(|k| k.starts_with("startup") && !k.contains("_loc_"))
            .cloned()
            .collect();
        PackageSet {
            files,
            startup,
            opts: ReadOptions::default(),
            loaded: RefCell::default(),
            failed: RefCell::default(),
            locate_cache: RefCell::default(),
            defs: RefCell::default(),
            links: RefCell::default(),
            lookups: RefCell::default(),
            enums: RefCell::default(),
            struct_names: RefCell::default(),
        }
    }

    /// A set for `file`: its folder, plus the parent folder when the file
    /// lies in a `Maps` subfolder of a cooked folder. `file` is opened.
    pub fn for_file(file: &Path) -> ObjResult<(PackageSet, Arc<LoadedPackage>)> {
        let mut dirs = Vec::new();
        if let Some(dir) = file.parent() {
            let dir = if dir.as_os_str().is_empty() {
                Path::new(".")
            } else {
                dir
            };
            dirs.push(dir.to_path_buf());
            if dir
                .file_name()
                .is_some_and(|n| n.to_string_lossy().eq_ignore_ascii_case("Maps"))
                && let Some(parent) = dir.parent()
            {
                dirs.push(parent.to_path_buf());
            }
        }
        let set = PackageSet::new(&dirs);
        let pkg = set.open_file(file)?;
        Ok((set, pkg))
    }

    /// Lower-case names of the package files known to the set.
    pub fn file_names(&self) -> impl Iterator<Item = &str> {
        self.files.keys().map(String::as_str)
    }

    /// Open (or return the cached) package at `path`, registering it under
    /// its file stem.
    pub fn open_file(&self, path: &Path) -> ObjResult<Arc<LoadedPackage>> {
        let key =
            stem_lower(path).ok_or_else(|| ObjectError::NotFound(path.display().to_string()))?;
        if let Some(p) = self.loaded.borrow().get(&key) {
            return Ok(p.clone());
        }
        let package = Package::open_with(path, &self.opts)?;
        let name = path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        let lp = Arc::new(LoadedPackage::new(name, path, package));
        self.loaded.borrow_mut().insert(key, lp.clone());
        Ok(lp)
    }

    /// Register an already parsed package under `name` (its own name).
    pub fn insert_package(&self, name: &str, package: Package) -> Arc<LoadedPackage> {
        let lp = Arc::new(LoadedPackage::new(name, PathBuf::from(name), package));
        self.loaded
            .borrow_mut()
            .insert(name.to_ascii_lowercase(), lp.clone());
        self.locate_cache.borrow_mut().clear();
        lp
    }

    /// The package whose own name is `name` (opening its file if needed).
    pub fn package(&self, name: &str) -> Option<Arc<LoadedPackage>> {
        let key = name.to_ascii_lowercase();
        if let Some(p) = self.loaded.borrow().get(&key) {
            return Some(p.clone());
        }
        if self.failed.borrow().contains_key(&key) {
            return None;
        }
        let path = self.files.get(&key)?.clone();
        match self.open_file(&path) {
            Ok(p) => Some(p),
            Err(e) => {
                self.failed.borrow_mut().insert(key, e.to_string());
                None
            }
        }
    }

    /// Locate the export with qualified path `path`.
    pub fn locate(&self, path: &str) -> Option<(Arc<LoadedPackage>, usize)> {
        let key = path.to_ascii_lowercase();
        if let Some(hit) = self.locate_cache.borrow().get(&key) {
            return hit
                .as_ref()
                .and_then(|(p, i)| self.package(p).map(|lp| (lp, *i)));
        }
        let found = self.locate_uncached(path);
        self.locate_cache
            .borrow_mut()
            .insert(key, found.as_ref().map(|(lp, i)| (lp.name.clone(), *i)));
        found
    }

    fn locate_uncached(&self, path: &str) -> Option<(Arc<LoadedPackage>, usize)> {
        let first = path.split('.').next().unwrap_or(path);
        if let Some(lp) = self.package(first)
            && let Some(i) = lp.export_by_qualified(path)
        {
            return Some((lp, i));
        }
        for s in self.startup.clone() {
            if let Some(lp) = self.package(&s)
                && let Some(i) = lp.export_by_qualified(path)
            {
                return Some((lp, i));
            }
        }
        let loaded: Vec<Arc<LoadedPackage>> = self.loaded.borrow().values().cloned().collect();
        loaded
            .into_iter()
            .find_map(|lp| lp.export_by_qualified(path).map(|i| (lp, i)))
    }

    /// Strictly decode the script object at qualified `path`.
    pub fn script_object(&self, path: &str) -> ObjResult<(Arc<LoadedPackage>, ScriptObject)> {
        let (lp, i) = self
            .locate(path)
            .ok_or_else(|| ObjectError::NotFound(path.to_owned()))?;
        let obj = decode_script_object(&lp.package, Some(&lp.name), i, &NoSchema)?;
        Ok((lp, obj))
    }

    /// Decode export `index` of `lp` (prelude + tagged properties) with this
    /// set as schema.
    pub fn decode(&self, lp: &LoadedPackage, index: usize) -> ObjResult<DecodedObject> {
        decode_object(&lp.package, Some(&lp.name), index, self)
    }

    fn build_struct_def(&self, path: &str) -> ObjResult<StructDef> {
        let (lp, obj) = self.script_object(path)?;
        let (structure, kind, struct_flags) = match &obj.body {
            ScriptBody::Class { structure, .. } => (structure, StructKind::Class, 0),
            ScriptBody::State { structure, .. } => (structure, StructKind::State, 0),
            ScriptBody::Function { structure, .. } => (structure, StructKind::Function, 0),
            ScriptBody::ScriptStruct {
                structure,
                struct_flags,
                ..
            } => (structure, StructKind::ScriptStruct, *struct_flags),
            _ => {
                return Err(ObjectError::WrongKind {
                    export: obj.export_index,
                    expected: "struct",
                    found: obj.kind.name().to_owned(),
                });
            }
        };
        let mut properties = Vec::new();
        for (ci, child) in children(&lp, structure.children)? {
            if let ScriptBody::Property(data) = &child.body {
                properties.push(Arc::new(property_def(&lp, ci, data, 0)?));
            }
        }
        Ok(StructDef {
            path: lp.qualified(obj.export_index)?,
            name: lp
                .package
                .fname(lp.package.export(obj.export_index)?.object_name),
            kind,
            super_path: lp.ref_path(structure.super_struct)?,
            struct_flags,
            properties,
        })
    }

    /// Build the [`ClassModel`] for the class at qualified `path`.
    pub fn class_model(&self, path: &str) -> ObjResult<ClassModel> {
        let (lp, obj) = self.script_object(path)?;
        let ScriptBody::Class {
            structure,
            state,
            class,
        } = &obj.body
        else {
            return Err(ObjectError::WrongKind {
                export: obj.export_index,
                expected: "Class",
                found: obj.kind.name().to_owned(),
            });
        };
        let qpath = lp.qualified(obj.export_index)?;
        let mut model = ClassModel {
            path: qpath.clone(),
            package_file: lp.name.clone(),
            export_index: obj.export_index,
            super_chain: self.super_chain(&qpath),
            class_flags: class.class_flags,
            class_flag_names: flags::describe(u64::from(class.class_flags), flags::class::NAMES),
            within: lp.ref_path(class.within)?,
            config_name: class.config_name.clone(),
            native_header: class.native_header.clone(),
            hide_categories: class.hide_categories.clone(),
            dont_sort_categories: class.dont_sort_categories.clone(),
            auto_expand_categories: class.auto_expand_categories.clone(),
            auto_collapse_categories: class.auto_collapse_categories.clone(),
            class_groups: class.class_groups.clone(),
            force_script_order: class.force_script_order != 0,
            interfaces: Vec::new(),
            components: Vec::new(),
            default_object: lp.ref_path(class.default_object)?,
            script_text: lp.ref_path(structure.script_text)?,
            replication_bytecode: structure.storage_size,
            probe_mask: state.probe_mask,
            properties: Vec::new(),
            functions: Vec::new(),
            states: Vec::new(),
            enums: Vec::new(),
            consts: Vec::new(),
            structs: Vec::new(),
        };
        for (iface, ptr) in &class.interfaces {
            model.interfaces.push(InterfaceInfo {
                class: lp.ref_path(*iface)?.unwrap_or_default(),
                pointer_property: lp.ref_path(*ptr)?,
            });
        }
        for (name, comp) in &class.components {
            model.components.push(ComponentInfo {
                name: name.clone(),
                template: lp.ref_path(*comp)?.unwrap_or_default(),
            });
        }
        for (ci, child) in children(&lp, structure.children)? {
            let name = lp.package.fname(lp.package.export(ci)?.object_name);
            match &child.body {
                ScriptBody::Property(data) => {
                    model
                        .properties
                        .push(PropertyInfo::from_def(&property_def(&lp, ci, data, 0)?));
                }
                ScriptBody::Function { .. } => {
                    model.functions.push(function_info(&lp, ci, &child)?)
                }
                ScriptBody::State {
                    structure: s,
                    state: st,
                } => {
                    let mut functions = Vec::new();
                    for (fi, f) in children(&lp, s.children)? {
                        if matches!(f.body, ScriptBody::Function { .. }) {
                            functions.push(function_info(&lp, fi, &f)?);
                        }
                    }
                    // Functions are prepended to the chain: restore declaration order.
                    functions.reverse();
                    model.states.push(StateInfo {
                        name,
                        flags: st.state_flags,
                        flag_names: flags::describe(u64::from(st.state_flags), flags::state::NAMES),
                        super_state: lp.ref_path(s.super_struct)?,
                        probe_mask: st.probe_mask,
                        label_table_offset: st.label_table_offset,
                        bytecode_storage: s.storage_size,
                        functions,
                    });
                }
                ScriptBody::Enum { names } => model.enums.push(EnumInfo {
                    name,
                    values: names.clone(),
                }),
                ScriptBody::Const { value } => model.consts.push(ConstInfo {
                    name,
                    value: value.clone(),
                }),
                ScriptBody::ScriptStruct {
                    structure: s,
                    struct_flags,
                    ..
                } => {
                    let mut properties = Vec::new();
                    for (pi, p) in children(&lp, s.children)? {
                        if let ScriptBody::Property(data) = &p.body {
                            properties
                                .push(PropertyInfo::from_def(&property_def(&lp, pi, data, 0)?));
                        }
                    }
                    let defaults = self.struct_defaults(&lp, ci)?;
                    model.structs.push(StructInfo {
                        name,
                        flags: *struct_flags,
                        flag_names: flags::describe(
                            u64::from(*struct_flags),
                            flags::structure::NAMES,
                        ),
                        super_struct: lp.ref_path(s.super_struct)?,
                        properties,
                        defaults,
                    });
                }
                _ => {}
            }
        }
        // The children chain lists properties in declaration order but every
        // other field (function, state, enum, const, struct) in reverse
        // declaration order (STRONG: compared against the class sources).
        // Report everything in declaration order.
        model.functions.reverse();
        model.states.reverse();
        model.enums.reverse();
        model.consts.reverse();
        model.structs.reverse();
        Ok(model)
    }

    fn struct_defaults(&self, lp: &LoadedPackage, index: usize) -> ObjResult<Vec<Property>> {
        let obj = decode_script_object(&lp.package, Some(&lp.name), index, self)?;
        match obj.body {
            ScriptBody::ScriptStruct { defaults, .. } => Ok(defaults),
            _ => Ok(Vec::new()),
        }
    }

    /// Qualified paths of the super classes of `path`, nearest first.
    pub fn super_chain(&self, path: &str) -> Vec<String> {
        let mut out = Vec::new();
        let mut cur = self.struct_def(path).and_then(|d| d.super_path.clone());
        while let Some(p) = cur {
            if out.len() >= MAX_SUPER_DEPTH
                || out.iter().any(|o: &String| o.eq_ignore_ascii_case(&p))
            {
                break;
            }
            cur = self.struct_def(&p).and_then(|d| d.super_path.clone());
            out.push(p);
        }
        out
    }

    /// Class default object of the class at `class_path`: (package, export index).
    pub fn default_object(&self, class_path: &str) -> ObjResult<(Arc<LoadedPackage>, usize)> {
        let (lp, obj) = self.script_object(class_path)?;
        let ScriptBody::Class { class, .. } = &obj.body else {
            return Err(ObjectError::WrongKind {
                export: obj.export_index,
                expected: "Class",
                found: obj.kind.name().to_owned(),
            });
        };
        let i = class
            .default_object
            .export_index()
            .ok_or_else(|| ObjectError::NotFound(format!("default object of {class_path}")))?;
        Ok((lp, i))
    }

    /// Decoded class default object of `class_path`.
    pub fn class_defaults(&self, class_path: &str) -> ObjResult<DecodedObject> {
        let (lp, i) = self.default_object(class_path)?;
        self.decode(&lp, i)
    }

    /// Class defaults resolved across the super chain: each class's default
    /// object only stores values that differ from its super class's defaults,
    /// so the values are merged root-first (child overrides parent; tagged
    /// struct values merge member-wise).
    pub fn inherited_defaults(&self, class_path: &str) -> ObjResult<InheritedDefaults> {
        let mut chain = self.super_chain(class_path);
        chain.reverse();
        let own = self
            .struct_def(class_path)
            .map(|d| d.path.clone())
            .unwrap_or_else(|| class_path.to_owned());
        chain.push(own.clone());
        let mut values: Vec<ResolvedDefault> = Vec::new();
        let mut sources = Vec::new();
        let mut warnings = Vec::new();
        for class in &chain {
            match self.class_defaults(class) {
                Ok(obj) => {
                    merge_defaults(&mut values, &obj.properties, class);
                    sources.push(DefaultsSource {
                        class: class.clone(),
                        default_object: obj.path.clone(),
                        properties: obj.properties.len(),
                        native_tail: obj.native_tail(),
                    });
                    warnings.extend(obj.warnings.into_iter().map(|w| format!("{class}: {w}")));
                }
                Err(e) => warnings.push(format!("{class}: defaults unavailable: {e}")),
            }
        }
        Ok(InheritedDefaults {
            class: own,
            sources,
            values,
            warnings,
        })
    }
}

fn children(lp: &LoadedPackage, first: PackageIndex) -> ObjResult<Vec<(usize, ScriptObject)>> {
    let mut out = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let mut cur = first;
    while let Some(i) = cur.export_index() {
        if out.len() >= MAX_CHILDREN || !seen.insert(i) {
            return Err(ObjectError::Malformed {
                what: "children chain",
                offset: 0,
                detail: format!("cycle or excessive length at export {i}"),
            });
        }
        let obj = decode_script_object(&lp.package, Some(&lp.name), i, &NoSchema)?;
        cur = obj.next.unwrap_or_default();
        out.push((i, obj));
    }
    Ok(out)
}

fn property_def(
    lp: &LoadedPackage,
    index: usize,
    data: &PropertyData,
    depth: usize,
) -> ObjResult<PropertyDef> {
    let p = |idx: PackageIndex| -> ObjResult<String> {
        Ok(lp.ref_path(idx)?.unwrap_or_else(|| "None".to_owned()))
    };
    let sub = |idx: PackageIndex| -> ObjResult<Option<Box<PropertyDef>>> {
        let Some(i) = idx.export_index() else {
            return Ok(None);
        };
        if depth >= MAX_INNER_DEPTH {
            return Err(ObjectError::TooDeep {
                limit: MAX_INNER_DEPTH,
                offset: 0,
            });
        }
        let obj = decode_script_object(&lp.package, Some(&lp.name), i, &NoSchema)?;
        match &obj.body {
            ScriptBody::Property(d) => Ok(Some(Box::new(property_def(lp, i, d, depth + 1)?))),
            _ => Err(ObjectError::WrongKind {
                export: i,
                expected: "property",
                found: obj.kind.name().to_owned(),
            }),
        }
    };
    let ty = match &data.kind {
        PropertyKindData::Byte { enum_ } => PropertyType::Byte {
            enum_path: lp.ref_path(*enum_)?,
        },
        PropertyKindData::Int => PropertyType::Int,
        PropertyKindData::Float => PropertyType::Float,
        PropertyKindData::Bool => PropertyType::Bool,
        PropertyKindData::Str => PropertyType::Str,
        PropertyKindData::Name => PropertyType::Name,
        PropertyKindData::Object { class } => PropertyType::Object { class: p(*class)? },
        PropertyKindData::Class { class, meta_class } => PropertyType::Class {
            class: p(*class)?,
            meta_class: p(*meta_class)?,
        },
        PropertyKindData::Component { class } => PropertyType::Component { class: p(*class)? },
        PropertyKindData::Interface { class } => PropertyType::Interface { class: p(*class)? },
        PropertyKindData::Struct { struct_ } => PropertyType::Struct {
            struct_path: p(*struct_)?,
        },
        PropertyKindData::Array { inner } => match sub(*inner)? {
            Some(inner) => PropertyType::Array { inner },
            None => {
                return Err(ObjectError::Malformed {
                    what: "ArrayProperty.Inner",
                    offset: 0,
                    detail: format!("export {index}: inner property is not an export"),
                });
            }
        },
        PropertyKindData::Map { key, value } => PropertyType::Map {
            key: sub(*key)?,
            value: sub(*value)?,
        },
        PropertyKindData::Delegate { function, source } => PropertyType::Delegate {
            function: p(*function)?,
            source: lp.ref_path(*source)?,
        },
    };
    Ok(PropertyDef {
        name: lp.package.fname(lp.package.export(index)?.object_name),
        path: lp.qualified(index)?,
        array_dim: data.array_dim,
        flags: data.flags,
        category: data.category.clone(),
        array_enum: lp.ref_path(data.array_enum)?,
        rep_offset: data.rep_offset,
        ty,
    })
}

fn function_info(lp: &LoadedPackage, index: usize, obj: &ScriptObject) -> ObjResult<FunctionInfo> {
    let ScriptBody::Function {
        structure,
        function,
    } = &obj.body
    else {
        return Err(ObjectError::WrongKind {
            export: index,
            expected: "Function",
            found: obj.kind.name().to_owned(),
        });
    };
    let mut params = Vec::new();
    let mut locals = Vec::new();
    let mut return_type = None;
    for (ci, child) in children(lp, structure.children)? {
        let ScriptBody::Property(data) = &child.body else {
            continue;
        };
        let def = property_def(lp, ci, data, 0)?;
        if def.has(flags::property::RETURN_PARM) {
            return_type = Some(def.ty.describe());
        } else if def.has(flags::property::PARM) {
            params.push(ParamInfo::from_def(&def));
        } else {
            locals.push(ParamInfo::from_def(&def));
        }
    }
    Ok(FunctionInfo {
        name: lp.package.fname(lp.package.export(index)?.object_name),
        flags: function.function_flags,
        flag_names: flags::describe(u64::from(function.function_flags), flags::function::NAMES),
        native_index: function.native_index,
        operator_precedence: function.operator_precedence,
        friendly_name: function.friendly_name.clone(),
        rep_offset: function.rep_offset,
        super_function: lp.ref_path(structure.super_struct)?,
        params,
        return_type,
        locals,
        bytecode_storage: structure.storage_size,
    })
}

fn key_of(p: &Property) -> (String, i32) {
    (p.name.to_ascii_lowercase(), p.array_index)
}

fn merge_props(into: &mut Vec<Property>, from: &[Property]) {
    for p in from {
        match into.iter_mut().find(|q| key_of(q) == key_of(p)) {
            Some(q) => merge_value(&mut q.value, &p.value),
            None => into.push(p.clone()),
        }
    }
}

fn merge_value(into: &mut Value, from: &Value) {
    match (into, from) {
        (
            Value::Struct {
                binary: false,
                fields: a,
                ..
            },
            Value::Struct {
                binary: false,
                fields: b,
                ..
            },
        ) => merge_props(a, b),
        (slot, v) => *slot = v.clone(),
    }
}

fn merge_defaults(values: &mut Vec<ResolvedDefault>, props: &[Property], source: &str) {
    for p in props {
        let key = key_of(p);
        match values
            .iter_mut()
            .find(|v| (v.name.to_ascii_lowercase(), v.array_index) == key)
        {
            Some(v) => {
                merge_value(&mut v.value, &p.value);
                v.source = source.to_owned();
            }
            None => values.push(ResolvedDefault {
                name: p.name.clone(),
                array_index: p.array_index,
                type_name: p.type_name.clone(),
                value: p.value.clone(),
                source: source.to_owned(),
            }),
        }
    }
}

impl Schema for PackageSet {
    fn struct_def(&self, path: &str) -> Option<Arc<StructDef>> {
        let key = path.to_ascii_lowercase();
        if let Some(d) = self.defs.borrow().get(&key) {
            return d.clone();
        }
        let d = self.build_struct_def(path).ok().map(Arc::new);
        self.defs.borrow_mut().insert(key, d.clone());
        d
    }

    fn struct_by_name(&self, name: &str) -> Option<Arc<StructDef>> {
        let key = name.to_ascii_lowercase();
        if let Some(d) = self.struct_names.borrow().get(&key) {
            return d.clone();
        }
        // Core.Object first (the immutable math structs), then every loaded package.
        let mut found = self.struct_def(&format!("Core.Object.{name}"));
        if found.is_none() {
            let loaded: Vec<Arc<LoadedPackage>> = self.loaded.borrow().values().cloned().collect();
            found = loaded.iter().find_map(|lp| {
                let i = *lp.structs_by_name.get(&key)?;
                let path = lp.qualified(i).ok()?;
                self.struct_def(&path)
            });
        }
        self.struct_names.borrow_mut().insert(key, found.clone());
        found
    }

    fn find_property(&self, owner: &str, name: &str) -> Option<Arc<PropertyDef>> {
        let key = owner.to_ascii_lowercase();
        let cached = self.lookups.borrow().get(&key).cloned();
        let map = match cached {
            Some(m) => m,
            None => {
                let mut m = HashMap::new();
                for d in self.property_link(owner) {
                    // The link lists the most derived declarations first; they win.
                    m.entry(d.name.to_ascii_lowercase()).or_insert(d);
                }
                let m = Arc::new(m);
                self.lookups.borrow_mut().insert(key, m.clone());
                m
            }
        };
        map.get(&name.to_ascii_lowercase()).cloned()
    }

    fn property_link(&self, owner: &str) -> Vec<Arc<PropertyDef>> {
        let key = owner.to_ascii_lowercase();
        if let Some(l) = self.links.borrow().get(&key) {
            return l.as_ref().clone();
        }
        let mut chain = vec![owner.to_owned()];
        chain.extend(self.super_chain(owner));
        let mut out = Vec::new();
        for s in &chain {
            if let Some(d) = self.struct_def(s) {
                out.extend(d.properties.iter().cloned());
            }
        }
        self.links.borrow_mut().insert(key, Arc::new(out.clone()));
        out
    }

    fn enum_names(&self, path: &str) -> Option<Arc<Vec<String>>> {
        let key = path.to_ascii_lowercase();
        if let Some(e) = self.enums.borrow().get(&key) {
            return e.clone();
        }
        let e = match self.script_object(path) {
            Ok((
                _,
                ScriptObject {
                    body: ScriptBody::Enum { names },
                    ..
                },
            )) => Some(Arc::new(names)),
            _ => None,
        };
        self.enums.borrow_mut().insert(key, e.clone());
        e
    }

    fn class_chain(&self, class_path: &str) -> Vec<String> {
        let mut out = vec![last_component(class_path).to_ascii_lowercase()];
        out.extend(
            self.super_chain(class_path)
                .iter()
                .map(|p| last_component(p).to_ascii_lowercase()),
        );
        out
    }
}

/// One declared property, for display.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PropertyInfo {
    /// Name.
    pub name: String,
    /// UnrealScript-style type.
    pub type_desc: String,
    /// Property class (`IntProperty`, ...).
    pub kind: String,
    /// `ArrayDim`.
    pub array_dim: i32,
    /// Raw flags.
    pub flags: u64,
    /// Flag names.
    pub flag_names: Vec<String>,
    /// Category.
    pub category: String,
    /// `RepOffset` (replicated properties).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rep_offset: Option<u16>,
    /// Full type with references.
    pub ty: PropertyType,
}

impl PropertyInfo {
    /// From a definition.
    pub fn from_def(d: &PropertyDef) -> PropertyInfo {
        PropertyInfo {
            name: d.name.clone(),
            type_desc: d.ty.describe(),
            kind: d.ty.class_name().to_owned(),
            array_dim: d.array_dim,
            flags: d.flags,
            flag_names: d.flag_names(),
            category: d.category.clone(),
            rep_offset: d.rep_offset,
            ty: d.ty.clone(),
        }
    }
}

/// A function parameter or local.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ParamInfo {
    /// Name.
    pub name: String,
    /// UnrealScript-style type.
    pub type_desc: String,
    /// Raw property flags.
    pub flags: u64,
    /// `out`.
    pub out: bool,
    /// `optional`.
    pub optional: bool,
    /// `coerce`.
    pub coerce: bool,
    /// `ArrayDim`.
    pub array_dim: i32,
}

impl ParamInfo {
    fn from_def(d: &PropertyDef) -> ParamInfo {
        ParamInfo {
            name: d.name.clone(),
            type_desc: d.ty.describe(),
            flags: d.flags,
            out: d.has(flags::property::OUT_PARM),
            optional: d.has(flags::property::OPTIONAL_PARM),
            coerce: d.has(flags::property::COERCE_PARM),
            array_dim: d.array_dim,
        }
    }
}

/// A function.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct FunctionInfo {
    /// Name.
    pub name: String,
    /// `FunctionFlags`.
    pub flags: u32,
    /// Flag names.
    pub flag_names: Vec<String>,
    /// `iNative`.
    pub native_index: u16,
    /// `OperPrecedence`.
    pub operator_precedence: u8,
    /// `FriendlyName`.
    pub friendly_name: String,
    /// `RepOffset`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rep_offset: Option<u16>,
    /// Overridden function (`SuperStruct`).
    pub super_function: Option<String>,
    /// Parameters in declaration order.
    pub params: Vec<ParamInfo>,
    /// Return type, if any.
    pub return_type: Option<String>,
    /// Local variables.
    pub locals: Vec<ParamInfo>,
    /// Bytecode bytes on disk.
    pub bytecode_storage: usize,
}

/// A state.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct StateInfo {
    /// Name.
    pub name: String,
    /// `StateFlags`.
    pub flags: u32,
    /// Flag names.
    pub flag_names: Vec<String>,
    /// Super state.
    pub super_state: Option<String>,
    /// `ProbeMask`.
    pub probe_mask: u32,
    /// `LabelTableOffset`.
    pub label_table_offset: u16,
    /// Bytecode bytes on disk (state code).
    pub bytecode_storage: usize,
    /// Functions declared in the state.
    pub functions: Vec<FunctionInfo>,
}

/// An enum.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct EnumInfo {
    /// Name.
    pub name: String,
    /// Enumerators.
    pub values: Vec<String>,
}

/// A constant.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ConstInfo {
    /// Name.
    pub name: String,
    /// Value text.
    pub value: String,
}

/// A script struct declared in the class.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct StructInfo {
    /// Name.
    pub name: String,
    /// `StructFlags`.
    pub flags: u32,
    /// Flag names.
    pub flag_names: Vec<String>,
    /// Super struct.
    pub super_struct: Option<String>,
    /// Members.
    pub properties: Vec<PropertyInfo>,
    /// Struct defaults.
    pub defaults: Vec<Property>,
}

/// An implemented interface.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct InterfaceInfo {
    /// Interface class.
    pub class: String,
    /// Pointer property.
    pub pointer_property: Option<String>,
}

/// A default subobject component.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ComponentInfo {
    /// Component name.
    pub name: String,
    /// Template object.
    pub template: String,
}

/// Structure of one class.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ClassModel {
    /// Qualified path.
    pub path: String,
    /// Package file that defines it.
    pub package_file: String,
    /// Export index there.
    pub export_index: usize,
    /// Super classes, nearest first.
    pub super_chain: Vec<String>,
    /// `ClassFlags`.
    pub class_flags: u32,
    /// Flag names.
    pub class_flag_names: Vec<String>,
    /// `ClassWithin`.
    pub within: Option<String>,
    /// `ClassConfigName`.
    pub config_name: String,
    /// `ClassHeaderFilename`.
    pub native_header: String,
    /// `HideCategories`.
    pub hide_categories: Vec<String>,
    /// `DontSortCategories`.
    pub dont_sort_categories: Vec<String>,
    /// `AutoExpandCategories`.
    pub auto_expand_categories: Vec<String>,
    /// `AutoCollapseCategories`.
    pub auto_collapse_categories: Vec<String>,
    /// `ClassGroupNames`.
    pub class_groups: Vec<String>,
    /// `bForceScriptOrder`.
    pub force_script_order: bool,
    /// Implemented interfaces.
    pub interfaces: Vec<InterfaceInfo>,
    /// Default subobject components.
    pub components: Vec<ComponentInfo>,
    /// Class default object.
    pub default_object: Option<String>,
    /// `ScriptText` buffer (source; never commit its contents).
    pub script_text: Option<String>,
    /// Class bytecode bytes on disk (replication conditions).
    pub replication_bytecode: usize,
    /// `ProbeMask`.
    pub probe_mask: u32,
    /// Properties declared by this class, in declaration order.
    pub properties: Vec<PropertyInfo>,
    /// Functions declared by this class, in declaration order.
    pub functions: Vec<FunctionInfo>,
    /// States declared by this class, in declaration order.
    pub states: Vec<StateInfo>,
    /// Enums declared by this class, in declaration order.
    pub enums: Vec<EnumInfo>,
    /// Constants declared by this class, in declaration order.
    pub consts: Vec<ConstInfo>,
    /// Structs declared by this class, in declaration order.
    pub structs: Vec<StructInfo>,
}

/// One resolved default value.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ResolvedDefault {
    /// Property name.
    pub name: String,
    /// Static array index.
    pub array_index: i32,
    /// Property type name.
    pub type_name: String,
    /// Value.
    pub value: Value,
    /// Class whose default object last set (part of) the value.
    pub source: String,
}

/// One class default object that contributed to [`InheritedDefaults`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DefaultsSource {
    /// Class.
    pub class: String,
    /// Its default object.
    pub default_object: String,
    /// Tagged properties it stores.
    pub properties: usize,
    /// Bytes of class-specific native data after its tagged properties.
    pub native_tail: usize,
}

/// Defaults of a class merged across its super chain.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct InheritedDefaults {
    /// Class.
    pub class: String,
    /// Default objects consulted, root first.
    pub sources: Vec<DefaultsSource>,
    /// Merged values, in first-set order.
    pub values: Vec<ResolvedDefault>,
    /// Decoding notes.
    pub warnings: Vec<String>,
}
