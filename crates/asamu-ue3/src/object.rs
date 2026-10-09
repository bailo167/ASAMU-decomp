//! Export payload prelude (`UObject::Serialize` up to the tagged properties)
//! and generic object decoding for UE3 v868.
//!
//! Layout, proven by exact payload consumption across every shipped package
//! (see `docs/reverse-engineering/OBJECT_FORMAT.md`):
//!
//! ```text
//! [native prefix]      only DominantDirectional/DominantSpotLightComponent (not CDOs):
//!                      TArray<u16> (shadow map), serialized before UObject data
//! [FStateFrame]        only when ObjectFlags has RF_HasStack (0x0200000000000000):
//!                      i32 Node | i32 StateNode | u32 ProbeMask | u16 LatentAction
//!                      | TArray StateStack (always empty) | i32 CodeOffset (if Node != 0)
//! [component template] only for Component subclasses that are not a class default object:
//!                      i32 TemplateOwnerClass | FName TemplateName (only when an outer is a
//!                      class default object; absent for archetype subobjects)
//! i32 NetIndex
//! tagged properties    until the FName "None" (absent for Class objects)
//! [class-specific native data]
//! ```

use serde::Serialize;
use thiserror::Error;

use crate::error::Ue3Error;
use crate::flags;
use crate::package::{ObjectRef, Package};
use crate::property::{self, Property, ValueContext};
use crate::reader::Reader;
use crate::schema::Schema;
use crate::types::PackageIndex;

/// Errors from decoding an export payload.
#[derive(Debug, Error)]
pub enum ObjectError {
    /// A low-level read failed (truncation, bad count, bad name index...).
    #[error(transparent)]
    Ue3(#[from] Ue3Error),

    /// A field holds a value the format does not allow.
    #[error("malformed {what} at payload offset {offset}: {detail}")]
    Malformed {
        /// Field or structure description.
        what: &'static str,
        /// Payload offset of the field.
        offset: usize,
        /// What is wrong.
        detail: String,
    },

    /// A strict decoder did not consume exactly `SerialSize` bytes.
    #[error("export {export} ({kind}): decoder consumed {consumed} of {size} payload bytes")]
    SizeMismatch {
        /// Export index.
        export: usize,
        /// Decoded kind.
        kind: String,
        /// Bytes consumed.
        consumed: usize,
        /// Payload size.
        size: usize,
    },

    /// Nested values exceed the recursion limit.
    #[error("values nested deeper than {limit} levels at payload offset {offset}")]
    TooDeep {
        /// Depth limit.
        limit: usize,
        /// Payload offset.
        offset: usize,
    },

    /// The export is not of the kind the caller asked for.
    #[error("export {export} is a {found}, not a {expected}")]
    WrongKind {
        /// Export index.
        export: usize,
        /// Expected kind.
        expected: &'static str,
        /// Actual class name.
        found: String,
    },

    /// An object, class or package could not be found.
    #[error("not found: {0}")]
    NotFound(String),
}

/// Result alias for object decoding.
pub type ObjResult<T> = std::result::Result<T, ObjectError>;

/// `FStateFrame` of an object with `RF_HasStack`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct StateFrame {
    /// Function/state whose code is executing (`Node`).
    pub node: PackageIndex,
    /// Current state (`StateNode`).
    pub state_node: PackageIndex,
    /// Probe mask (`u32` in v868).
    pub probe_mask: u32,
    /// `u16` stored after the probe mask (UE3: `LatentAction`). Values vary per
    /// object and look like leftover data; meaning UNKNOWN.
    pub latent_action: u16,
    /// Number of pushed states (always 0 in the shipped packages).
    pub state_stack_len: usize,
    /// Code offset into `node`'s bytecode, present when `node` is non-null.
    pub code_offset: Option<i32>,
}

/// Component template data of a non-CDO `Component` subclass.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ComponentTemplate {
    /// `TemplateOwnerClass`: usually null; for subobjects of other subobjects
    /// (e.g. distributions inside particle modules) the owning object's class.
    pub owner_class: PackageIndex,
    /// `TemplateName`, present for components inside a class default object.
    pub template_name: Option<String>,
}

/// Bytes before the tagged properties.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ObjectPrelude {
    /// Element count of the leading `TArray<u16>` of dominant-light components.
    pub shadow_map_len: Option<usize>,
    /// State frame (`RF_HasStack`).
    pub state_frame: Option<StateFrame>,
    /// Component template data.
    pub component: Option<ComponentTemplate>,
    /// `NetIndex`.
    pub net_index: i32,
}

/// Which optional prelude parts an export carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PreludeRules {
    /// `RF_HasStack` is set: a state frame follows.
    pub has_stack: bool,
    /// The object is a component (not a CDO): template owner class follows.
    pub component: bool,
    /// The component lies inside a class default object: the template name follows.
    pub template: bool,
    /// Leading `TArray<u16>` of dominant-light components.
    pub shadow_map_prefix: bool,
}

impl PreludeRules {
    /// Derive the rules for export `index` from its flags, its outer chain and
    /// its class chain (lower-case names, nearest first; see
    /// [`Schema::class_chain`]).
    pub fn for_export(pkg: &Package, index: usize, class_chain: &[String]) -> PreludeRules {
        let Ok(e) = pkg.export(index) else {
            return PreludeRules::default();
        };
        let cdo = e.object_flags & flags::object::CLASS_DEFAULT_OBJECT != 0;
        let is = |n: &str| class_chain.iter().any(|c| c == n);
        let component = !cdo && is("component");
        PreludeRules {
            has_stack: e.object_flags & flags::object::HAS_STACK != 0,
            component,
            template: component && in_class_default_object(pkg, index),
            shadow_map_prefix: !cdo
                && (is("dominantdirectionallightcomponent") || is("dominantspotlightcomponent")),
        }
    }
}

/// True when export `index` or one of its export outers is a class default
/// object (`RF_ClassDefaultObject`).
///
/// This decides whether a component carries a `TemplateName`: CONFIRMED by
/// exact consumption, components under archetypes (`RF_ArchetypeObject`, e.g.
/// prefab archetypes in maps) do not carry one.
pub fn in_class_default_object(pkg: &Package, index: usize) -> bool {
    let mask = flags::object::CLASS_DEFAULT_OBJECT;
    let mut cur = Some(index);
    let mut steps = 0usize;
    while let Some(i) = cur {
        let Ok(e) = pkg.export(i) else { return false };
        if e.object_flags & mask != 0 {
            return true;
        }
        steps += 1;
        if steps > crate::package::MAX_OUTER_DEPTH {
            return false;
        }
        cur = e.outer_index.export_index();
    }
    false
}

/// Read the prelude described by `rules`.
pub fn read_prelude(
    r: &mut Reader<'_>,
    pkg: &Package,
    rules: &PreludeRules,
) -> ObjResult<ObjectPrelude> {
    let mut shadow_map_len = None;
    if rules.shadow_map_prefix {
        let n = r.read_count("DominantLightShadowMap", 2)?;
        r.skip(n.saturating_mul(2))?;
        shadow_map_len = Some(n);
    }
    let mut state_frame = None;
    if rules.has_stack {
        let node = r.read_package_index()?;
        let state_node = r.read_package_index()?;
        let probe_mask = r.read_u32()?;
        let latent_action = r.read_u16()?;
        let at = r.position();
        let stack = r.read_count("StateFrame.StateStack", 8)?;
        if stack != 0 {
            return Err(ObjectError::Malformed {
                what: "StateFrame.StateStack",
                offset: at,
                detail: format!(
                    "{stack} pushed states; non-empty state stacks never occur in the shipped \
                     packages, so their element layout is unverified"
                ),
            });
        }
        let code_offset = if node.is_null() {
            None
        } else {
            Some(r.read_i32()?)
        };
        state_frame = Some(StateFrame {
            node,
            state_node,
            probe_mask,
            latent_action,
            state_stack_len: stack,
            code_offset,
        });
    }
    let mut component = None;
    if rules.component {
        let owner_class = r.read_package_index()?;
        let template_name = if rules.template {
            let at = r.position();
            let n = r.read_fname()?;
            Some(pkg.try_fname(n).map_err(|e| ObjectError::Malformed {
                what: "Component.TemplateName",
                offset: at,
                detail: e.to_string(),
            })?)
        } else {
            None
        };
        component = Some(ComponentTemplate {
            owner_class,
            template_name,
        });
    }
    let net_index = r.read_i32()?;
    Ok(ObjectPrelude {
        shadow_map_len,
        state_frame,
        component,
        net_index,
    })
}

/// Qualified path of a reference: imports and exports under a top-level
/// `Package` export already start with their package name; other exports are
/// prefixed with `own_name` (the package file's name) when given.
pub fn qualified_path(
    pkg: &Package,
    own_name: Option<&str>,
    idx: PackageIndex,
) -> Result<String, Ue3Error> {
    let path = pkg.object_path(idx)?;
    let Some(i) = idx.export_index() else {
        return Ok(path);
    };
    let Some(own) = own_name else {
        return Ok(path);
    };
    let chain = pkg.outer_chain(PackageIndex::from_export(i).unwrap_or(idx))?;
    let root = chain.last().copied().unwrap_or(idx);
    let root_is_package = match pkg.resolve(root)? {
        ObjectRef::Export(ri, _) => pkg.export_class_name(ri)? == "Package",
        _ => false,
    };
    Ok(if root_is_package {
        path
    } else {
        format!("{own}.{path}")
    })
}

/// Qualified path of the class of export `index` (`Core.Class` when the
/// export is itself a class).
pub fn export_class_path(
    pkg: &Package,
    own_name: Option<&str>,
    index: usize,
) -> Result<String, Ue3Error> {
    let e = pkg.export(index)?;
    if e.class_index.is_null() {
        return Ok("Core.Class".to_owned());
    }
    qualified_path(pkg, own_name, e.class_index)
}

/// A generically decoded export: prelude plus tagged properties.
#[derive(Debug, Clone, Serialize)]
pub struct DecodedObject {
    /// Export index.
    pub export_index: usize,
    /// Qualified object path.
    pub path: String,
    /// Qualified class path.
    pub class: String,
    /// Prelude.
    pub prelude: ObjectPrelude,
    /// Tagged properties (empty for `Class` objects, which have none).
    pub properties: Vec<Property>,
    /// Payload offset just after the terminating `None`.
    pub properties_end: usize,
    /// `SerialSize`.
    pub payload_size: usize,
    /// Non-fatal decoding notes (raw values, unknown types, inferred prelude).
    pub warnings: Vec<String>,
}

impl DecodedObject {
    /// Bytes after the tagged properties (class-specific native data).
    pub fn native_tail(&self) -> usize {
        self.payload_size.saturating_sub(self.properties_end)
    }
}

fn is_class_object(pkg: &Package, index: usize) -> bool {
    pkg.export(index).is_ok_and(|e| e.class_index.is_null())
}

/// Decode the prelude and tagged properties of export `index`.
///
/// `own_name` is the package's own name (file stem) used to qualify export
/// paths; `schema` supplies class chains and property definitions. When the
/// class chain is unknown, component preludes are inferred by probing the
/// candidate layouts (recorded as a warning).
pub fn decode_object(
    pkg: &Package,
    own_name: Option<&str>,
    index: usize,
    schema: &dyn Schema,
) -> ObjResult<DecodedObject> {
    let data = pkg.export_data(index)?;
    let path = qualified_path(
        pkg,
        own_name,
        PackageIndex::from_export(index).unwrap_or_default(),
    )?;
    let class = export_class_path(pkg, own_name, index)?;
    let chain = schema.class_chain(&class);
    let mut warnings = Vec::new();
    let class_object = is_class_object(pkg, index);

    let rules = PreludeRules::for_export(pkg, index, &chain);
    let candidates: Vec<PreludeRules> = if chain.is_empty() && !class_object {
        let base = PreludeRules {
            component: false,
            template: false,
            ..rules
        };
        vec![
            base,
            PreludeRules {
                component: true,
                template: true,
                ..base
            },
            PreludeRules {
                component: true,
                template: false,
                ..base
            },
        ]
    } else {
        vec![rules]
    };

    let mut last_err = None;
    for rules in &candidates {
        let mut r = Reader::new(data);
        let prelude = match read_prelude(&mut r, pkg, rules) {
            Ok(p) => p,
            Err(e) => {
                last_err = Some(e);
                continue;
            }
        };
        let mut ctx = ValueContext::new(pkg, own_name, schema);
        ctx.set_work_budget(property::work_budget_for(data.len()));
        let props = if class_object {
            Vec::new()
        } else {
            match property::read_tagged(&mut r, &mut ctx, Some(&class), 0) {
                Ok(p) => p,
                Err(e) => {
                    last_err = Some(e);
                    continue;
                }
            }
        };
        if candidates.len() > 1 {
            warnings.push(format!(
                "class hierarchy of {class} unknown; prelude inferred by probing (component: {}, template: {})",
                rules.component, rules.template
            ));
        }
        warnings.extend(ctx.into_warnings());
        return Ok(DecodedObject {
            export_index: index,
            path,
            class,
            prelude,
            properties: props,
            properties_end: r.position(),
            payload_size: data.len(),
            warnings,
        });
    }
    Err(last_err.unwrap_or_else(|| ObjectError::NotFound(format!("export {index}"))))
}
