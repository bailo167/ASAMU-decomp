//! Flag bit names for object, class, function, property, struct and state
//! flags as stored in UE3 v868 export payloads.
//!
//! The *positions* and *raw values* are read from the shipped packages
//! (CONFIRMED). The *names* follow UE3 engine conventions; their confidence is
//! recorded per group in `docs/reverse-engineering/OBJECT_FORMAT.md`. Names
//! whose meaning is corroborated by observed behaviour (for example the
//! parameter/return-value bits on function locals, or the struct flags that
//! decide binary serialization) are STRONG; the rest are TENTATIVE.

/// Conventional names of the bits set in `value`; unknown bits are reported
/// as one hexadecimal remainder.
pub fn describe(value: u64, names: &[(u64, &str)]) -> Vec<String> {
    let mut out = Vec::new();
    let mut known = 0u64;
    for &(bit, name) in names {
        known |= bit;
        if value & bit != 0 {
            out.push(name.to_owned());
        }
    }
    let unknown = value & !known;
    if unknown != 0 {
        out.push(format!("{unknown:#x}"));
    }
    out
}

/// `EObjectFlags` (64-bit, export table `ObjectFlags`).
pub mod object {
    /// `RF_ClassDefaultObject`: the object is a class default object (`Default__X`).
    pub const CLASS_DEFAULT_OBJECT: u64 = 0x0000_0000_0000_0200;
    /// `RF_ArchetypeObject`: the object is an archetype (template).
    pub const ARCHETYPE_OBJECT: u64 = 0x0000_0000_0000_0400;
    /// `RF_Transactional`.
    pub const TRANSACTIONAL: u64 = 0x0000_0001_0000_0000;
    /// `RF_Public`.
    pub const PUBLIC: u64 = 0x0000_0004_0000_0000;
    /// `RF_LoadForClient`.
    pub const LOAD_FOR_CLIENT: u64 = 0x0001_0000_0000_0000;
    /// `RF_LoadForServer`.
    pub const LOAD_FOR_SERVER: u64 = 0x0002_0000_0000_0000;
    /// `RF_LoadForEdit`.
    pub const LOAD_FOR_EDIT: u64 = 0x0004_0000_0000_0000;
    /// `RF_Standalone`.
    pub const STANDALONE: u64 = 0x0008_0000_0000_0000;
    /// `RF_NotForClient`.
    pub const NOT_FOR_CLIENT: u64 = 0x0010_0000_0000_0000;
    /// `RF_NotForServer`.
    pub const NOT_FOR_SERVER: u64 = 0x0020_0000_0000_0000;
    /// `RF_NotForEdit`.
    pub const NOT_FOR_EDIT: u64 = 0x0040_0000_0000_0000;
    /// `RF_HasStack`: the payload starts with a script state frame.
    /// STRONG: exactly the exports carrying this bit start with the
    /// `FStateFrame` layout documented in `object.rs`.
    pub const HAS_STACK: u64 = 0x0200_0000_0000_0000;
    /// `RF_Native`.
    pub const NATIVE: u64 = 0x0400_0000_0000_0000;

    /// (bit, conventional name) pairs.
    pub const NAMES: &[(u64, &str)] = &[
        (CLASS_DEFAULT_OBJECT, "ClassDefaultObject"),
        (ARCHETYPE_OBJECT, "ArchetypeObject"),
        (TRANSACTIONAL, "Transactional"),
        (PUBLIC, "Public"),
        (LOAD_FOR_CLIENT, "LoadForClient"),
        (LOAD_FOR_SERVER, "LoadForServer"),
        (LOAD_FOR_EDIT, "LoadForEdit"),
        (STANDALONE, "Standalone"),
        (NOT_FOR_CLIENT, "NotForClient"),
        (NOT_FOR_SERVER, "NotForServer"),
        (NOT_FOR_EDIT, "NotForEdit"),
        (HAS_STACK, "HasStack"),
        (NATIVE, "Native"),
    ];
}

/// `EClassFlags` (`UClass::ClassFlags`).
pub mod class {
    /// `CLASS_Abstract`.
    pub const ABSTRACT: u32 = 0x0000_0001;
    /// `CLASS_Compiled`.
    pub const COMPILED: u32 = 0x0000_0002;
    /// `CLASS_Config`.
    pub const CONFIG: u32 = 0x0000_0004;
    /// `CLASS_Transient`.
    pub const TRANSIENT: u32 = 0x0000_0008;
    /// `CLASS_Parsed`.
    pub const PARSED: u32 = 0x0000_0010;
    /// `CLASS_Localized`.
    pub const LOCALIZED: u32 = 0x0000_0020;
    /// `CLASS_SafeReplace`.
    pub const SAFE_REPLACE: u32 = 0x0000_0040;
    /// `CLASS_Native`.
    pub const NATIVE: u32 = 0x0000_0080;
    /// `CLASS_NoExport`.
    pub const NO_EXPORT: u32 = 0x0000_0100;
    /// `CLASS_Placeable`.
    pub const PLACEABLE: u32 = 0x0000_0200;
    /// `CLASS_PerObjectConfig`.
    pub const PER_OBJECT_CONFIG: u32 = 0x0000_0400;
    /// `CLASS_NativeReplication`.
    pub const NATIVE_REPLICATION: u32 = 0x0000_0800;
    /// `CLASS_EditInlineNew`.
    pub const EDIT_INLINE_NEW: u32 = 0x0000_1000;
    /// `CLASS_CollapseCategories`.
    pub const COLLAPSE_CATEGORIES: u32 = 0x0000_2000;
    /// `CLASS_Interface`. STRONG: set on `Core.Interface` and the classes that
    /// extend it.
    pub const INTERFACE: u32 = 0x0000_4000;
    /// `CLASS_HasInstancedProps`.
    pub const HAS_INSTANCED_PROPS: u32 = 0x0020_0000;
    /// `CLASS_NeedsDefProps`.
    pub const NEEDS_DEF_PROPS: u32 = 0x0040_0000;
    /// `CLASS_HasComponents`.
    pub const HAS_COMPONENTS: u32 = 0x0080_0000;
    /// `CLASS_Hidden`.
    pub const HIDDEN: u32 = 0x0100_0000;
    /// `CLASS_Deprecated`.
    pub const DEPRECATED: u32 = 0x0200_0000;
    /// `CLASS_HideDropDown`.
    pub const HIDE_DROP_DOWN: u32 = 0x0400_0000;
    /// `CLASS_Exported`.
    pub const EXPORTED: u32 = 0x0800_0000;
    /// `CLASS_Intrinsic`.
    pub const INTRINSIC: u32 = 0x1000_0000;
    /// `CLASS_NativeOnly`.
    pub const NATIVE_ONLY: u32 = 0x2000_0000;
    /// `CLASS_PerObjectLocalized`.
    pub const PER_OBJECT_LOCALIZED: u32 = 0x4000_0000;
    /// `CLASS_HasCrossLevelRefs`.
    pub const HAS_CROSS_LEVEL_REFS: u32 = 0x8000_0000;

    /// (bit, conventional name) pairs.
    pub const NAMES: &[(u64, &str)] = &[
        (ABSTRACT as u64, "Abstract"),
        (COMPILED as u64, "Compiled"),
        (CONFIG as u64, "Config"),
        (TRANSIENT as u64, "Transient"),
        (PARSED as u64, "Parsed"),
        (LOCALIZED as u64, "Localized"),
        (SAFE_REPLACE as u64, "SafeReplace"),
        (NATIVE as u64, "Native"),
        (NO_EXPORT as u64, "NoExport"),
        (PLACEABLE as u64, "Placeable"),
        (PER_OBJECT_CONFIG as u64, "PerObjectConfig"),
        (NATIVE_REPLICATION as u64, "NativeReplication"),
        (EDIT_INLINE_NEW as u64, "EditInlineNew"),
        (COLLAPSE_CATEGORIES as u64, "CollapseCategories"),
        (INTERFACE as u64, "Interface"),
        (HAS_INSTANCED_PROPS as u64, "HasInstancedProps"),
        (NEEDS_DEF_PROPS as u64, "NeedsDefProps"),
        (HAS_COMPONENTS as u64, "HasComponents"),
        (HIDDEN as u64, "Hidden"),
        (DEPRECATED as u64, "Deprecated"),
        (HIDE_DROP_DOWN as u64, "HideDropDown"),
        (EXPORTED as u64, "Exported"),
        (INTRINSIC as u64, "Intrinsic"),
        (NATIVE_ONLY as u64, "NativeOnly"),
        (PER_OBJECT_LOCALIZED as u64, "PerObjectLocalized"),
        (HAS_CROSS_LEVEL_REFS as u64, "HasCrossLevelRefs"),
    ];
}

/// `EFunctionFlags` (`UFunction::FunctionFlags`).
pub mod function {
    /// `FUNC_Final`.
    pub const FINAL: u32 = 0x0000_0001;
    /// `FUNC_Defined`.
    pub const DEFINED: u32 = 0x0000_0002;
    /// `FUNC_Iterator`.
    pub const ITERATOR: u32 = 0x0000_0004;
    /// `FUNC_Latent`.
    pub const LATENT: u32 = 0x0000_0008;
    /// `FUNC_PreOperator`.
    pub const PRE_OPERATOR: u32 = 0x0000_0010;
    /// `FUNC_Singular`.
    pub const SINGULAR: u32 = 0x0000_0020;
    /// `FUNC_Net`. CONFIRMED as the presence bit of the serialized `RepOffset`
    /// (exact payload consumption depends on it).
    pub const NET: u32 = 0x0000_0040;
    /// `FUNC_NetReliable`.
    pub const NET_RELIABLE: u32 = 0x0000_0080;
    /// `FUNC_Simulated`.
    pub const SIMULATED: u32 = 0x0000_0100;
    /// `FUNC_Exec`.
    pub const EXEC: u32 = 0x0000_0200;
    /// `FUNC_Native`.
    pub const NATIVE: u32 = 0x0000_0400;
    /// `FUNC_Event`.
    pub const EVENT: u32 = 0x0000_0800;
    /// `FUNC_Operator`.
    pub const OPERATOR: u32 = 0x0000_1000;
    /// `FUNC_Static`.
    pub const STATIC: u32 = 0x0000_2000;
    /// `FUNC_HasOptionalParms`.
    pub const HAS_OPTIONAL_PARMS: u32 = 0x0000_4000;
    /// `FUNC_Const`.
    pub const CONST: u32 = 0x0000_8000;
    /// `FUNC_Public`.
    pub const PUBLIC: u32 = 0x0002_0000;
    /// `FUNC_Private`.
    pub const PRIVATE: u32 = 0x0004_0000;
    /// `FUNC_Protected`.
    pub const PROTECTED: u32 = 0x0008_0000;
    /// `FUNC_Delegate`.
    pub const DELEGATE: u32 = 0x0010_0000;
    /// `FUNC_NetServer`.
    pub const NET_SERVER: u32 = 0x0020_0000;
    /// `FUNC_HasOutParms`.
    pub const HAS_OUT_PARMS: u32 = 0x0040_0000;
    /// `FUNC_HasDefaults`.
    pub const HAS_DEFAULTS: u32 = 0x0080_0000;
    /// `FUNC_NetClient`.
    pub const NET_CLIENT: u32 = 0x0100_0000;
    /// `FUNC_DLLImport`.
    pub const DLL_IMPORT: u32 = 0x0200_0000;

    /// (bit, conventional name) pairs.
    pub const NAMES: &[(u64, &str)] = &[
        (FINAL as u64, "Final"),
        (DEFINED as u64, "Defined"),
        (ITERATOR as u64, "Iterator"),
        (LATENT as u64, "Latent"),
        (PRE_OPERATOR as u64, "PreOperator"),
        (SINGULAR as u64, "Singular"),
        (NET as u64, "Net"),
        (NET_RELIABLE as u64, "NetReliable"),
        (SIMULATED as u64, "Simulated"),
        (EXEC as u64, "Exec"),
        (NATIVE as u64, "Native"),
        (EVENT as u64, "Event"),
        (OPERATOR as u64, "Operator"),
        (STATIC as u64, "Static"),
        (HAS_OPTIONAL_PARMS as u64, "HasOptionalParms"),
        (CONST as u64, "Const"),
        (PUBLIC as u64, "Public"),
        (PRIVATE as u64, "Private"),
        (PROTECTED as u64, "Protected"),
        (DELEGATE as u64, "Delegate"),
        (NET_SERVER as u64, "NetServer"),
        (HAS_OUT_PARMS as u64, "HasOutParms"),
        (HAS_DEFAULTS as u64, "HasDefaults"),
        (NET_CLIENT as u64, "NetClient"),
        (DLL_IMPORT as u64, "DLLImport"),
    ];
}

/// `EPropertyFlags` (64-bit, `UProperty::PropertyFlags`).
pub mod property {
    /// `CPF_Edit`.
    pub const EDIT: u64 = 0x0000_0000_0000_0001;
    /// `CPF_Const`.
    pub const CONST: u64 = 0x0000_0000_0000_0002;
    /// `CPF_Input`.
    pub const INPUT: u64 = 0x0000_0000_0000_0004;
    /// `CPF_ExportObject`.
    pub const EXPORT_OBJECT: u64 = 0x0000_0000_0000_0008;
    /// `CPF_OptionalParm`.
    pub const OPTIONAL_PARM: u64 = 0x0000_0000_0000_0010;
    /// `CPF_Net`. CONFIRMED as the presence bit of the serialized `RepOffset`.
    pub const NET: u64 = 0x0000_0000_0000_0020;
    /// `CPF_EditFixedSize`.
    pub const EDIT_FIXED_SIZE: u64 = 0x0000_0000_0000_0040;
    /// `CPF_Parm`. STRONG: set on every function local that is a parameter.
    pub const PARM: u64 = 0x0000_0000_0000_0080;
    /// `CPF_OutParm`.
    pub const OUT_PARM: u64 = 0x0000_0000_0000_0100;
    /// `CPF_SkipParm`.
    pub const SKIP_PARM: u64 = 0x0000_0000_0000_0200;
    /// `CPF_ReturnParm`. STRONG: set on exactly the `ReturnValue` locals.
    pub const RETURN_PARM: u64 = 0x0000_0000_0000_0400;
    /// `CPF_CoerceParm`.
    pub const COERCE_PARM: u64 = 0x0000_0000_0000_0800;
    /// `CPF_Native`.
    pub const NATIVE: u64 = 0x0000_0000_0000_1000;
    /// `CPF_Transient`.
    pub const TRANSIENT: u64 = 0x0000_0000_0000_2000;
    /// `CPF_Config`.
    pub const CONFIG: u64 = 0x0000_0000_0000_4000;
    /// `CPF_Localized`.
    pub const LOCALIZED: u64 = 0x0000_0000_0000_8000;
    /// `CPF_EditConst`.
    pub const EDIT_CONST: u64 = 0x0000_0000_0002_0000;
    /// `CPF_GlobalConfig`.
    pub const GLOBAL_CONFIG: u64 = 0x0000_0000_0004_0000;
    /// `CPF_Component`.
    pub const COMPONENT: u64 = 0x0000_0000_0008_0000;
    /// `CPF_AlwaysInit`.
    pub const ALWAYS_INIT: u64 = 0x0000_0000_0010_0000;
    /// `CPF_DuplicateTransient`.
    pub const DUPLICATE_TRANSIENT: u64 = 0x0000_0000_0020_0000;
    /// `CPF_NeedCtorLink`.
    pub const NEED_CTOR_LINK: u64 = 0x0000_0000_0040_0000;
    /// `CPF_NoExport`.
    pub const NO_EXPORT: u64 = 0x0000_0000_0080_0000;
    /// `CPF_NoImport`.
    pub const NO_IMPORT: u64 = 0x0000_0000_0100_0000;
    /// `CPF_NoClear`.
    pub const NO_CLEAR: u64 = 0x0000_0000_0200_0000;
    /// `CPF_EditInline`.
    pub const EDIT_INLINE: u64 = 0x0000_0000_0400_0000;
    /// `CPF_EditInlineUse`.
    pub const EDIT_INLINE_USE: u64 = 0x0000_0000_1000_0000;
    /// `CPF_Deprecated`.
    pub const DEPRECATED: u64 = 0x0000_0000_2000_0000;
    /// `CPF_DataBinding`.
    pub const DATA_BINDING: u64 = 0x0000_0000_4000_0000;
    /// `CPF_SerializeText`.
    pub const SERIALIZE_TEXT: u64 = 0x0000_0000_8000_0000;
    /// `CPF_RepNotify`.
    pub const REP_NOTIFY: u64 = 0x0000_0001_0000_0000;
    /// `CPF_Interp`.
    pub const INTERP: u64 = 0x0000_0002_0000_0000;
    /// `CPF_NonTransactional`.
    pub const NON_TRANSACTIONAL: u64 = 0x0000_0004_0000_0000;
    /// `CPF_EditorOnly`.
    pub const EDITOR_ONLY: u64 = 0x0000_0008_0000_0000;
    /// `CPF_NotForConsole`.
    pub const NOT_FOR_CONSOLE: u64 = 0x0000_0010_0000_0000;
    /// `CPF_RepRetry`.
    pub const REP_RETRY: u64 = 0x0000_0020_0000_0000;
    /// `CPF_PrivateWrite`.
    pub const PRIVATE_WRITE: u64 = 0x0000_0040_0000_0000;
    /// `CPF_ProtectedWrite`.
    pub const PROTECTED_WRITE: u64 = 0x0000_0080_0000_0000;
    /// `CPF_ArchetypeProperty`.
    pub const ARCHETYPE_PROPERTY: u64 = 0x0000_0100_0000_0000;
    /// `CPF_EditHide`.
    pub const EDIT_HIDE: u64 = 0x0000_0200_0000_0000;
    /// `CPF_EditTextBox`.
    pub const EDIT_TEXT_BOX: u64 = 0x0000_0400_0000_0000;
    /// `CPF_CrossLevelPassive`.
    pub const CROSS_LEVEL_PASSIVE: u64 = 0x0000_1000_0000_0000;
    /// `CPF_CrossLevelActive`.
    pub const CROSS_LEVEL_ACTIVE: u64 = 0x0000_2000_0000_0000;

    /// (bit, conventional name) pairs.
    pub const NAMES: &[(u64, &str)] = &[
        (EDIT, "Edit"),
        (CONST, "Const"),
        (INPUT, "Input"),
        (EXPORT_OBJECT, "ExportObject"),
        (OPTIONAL_PARM, "OptionalParm"),
        (NET, "Net"),
        (EDIT_FIXED_SIZE, "EditFixedSize"),
        (PARM, "Parm"),
        (OUT_PARM, "OutParm"),
        (SKIP_PARM, "SkipParm"),
        (RETURN_PARM, "ReturnParm"),
        (COERCE_PARM, "CoerceParm"),
        (NATIVE, "Native"),
        (TRANSIENT, "Transient"),
        (CONFIG, "Config"),
        (LOCALIZED, "Localized"),
        (EDIT_CONST, "EditConst"),
        (GLOBAL_CONFIG, "GlobalConfig"),
        (COMPONENT, "Component"),
        (ALWAYS_INIT, "AlwaysInit"),
        (DUPLICATE_TRANSIENT, "DuplicateTransient"),
        (NEED_CTOR_LINK, "NeedCtorLink"),
        (NO_EXPORT, "NoExport"),
        (NO_IMPORT, "NoImport"),
        (NO_CLEAR, "NoClear"),
        (EDIT_INLINE, "EditInline"),
        (EDIT_INLINE_USE, "EditInlineUse"),
        (DEPRECATED, "Deprecated"),
        (DATA_BINDING, "DataBinding"),
        (SERIALIZE_TEXT, "SerializeText"),
        (REP_NOTIFY, "RepNotify"),
        (INTERP, "Interp"),
        (NON_TRANSACTIONAL, "NonTransactional"),
        (EDITOR_ONLY, "EditorOnly"),
        (NOT_FOR_CONSOLE, "NotForConsole"),
        (REP_RETRY, "RepRetry"),
        (PRIVATE_WRITE, "PrivateWrite"),
        (PROTECTED_WRITE, "ProtectedWrite"),
        (ARCHETYPE_PROPERTY, "ArchetypeProperty"),
        (EDIT_HIDE, "EditHide"),
        (EDIT_TEXT_BOX, "EditTextBox"),
        (CROSS_LEVEL_PASSIVE, "CrossLevelPassive"),
        (CROSS_LEVEL_ACTIVE, "CrossLevelActive"),
    ];
}

/// `EStructFlags` (`UScriptStruct::StructFlags`).
pub mod structure {
    /// `STRUCT_Native`.
    pub const NATIVE: u32 = 0x0000_0001;
    /// `STRUCT_Export`.
    pub const EXPORT: u32 = 0x0000_0002;
    /// `STRUCT_HasComponents`.
    pub const HAS_COMPONENTS: u32 = 0x0000_0004;
    /// `STRUCT_Transient`.
    pub const TRANSIENT: u32 = 0x0000_0008;
    /// `STRUCT_Atomic`.
    pub const ATOMIC: u32 = 0x0000_0010;
    /// `STRUCT_Immutable`. STRONG: tagged struct values of exactly the
    /// structs with this bit are stored in binary form.
    pub const IMMUTABLE: u32 = 0x0000_0020;
    /// `STRUCT_StrictConfig`.
    pub const STRICT_CONFIG: u32 = 0x0000_0040;
    /// `STRUCT_ImmutableWhenCooked`: binary form in cooked packages.
    pub const IMMUTABLE_WHEN_COOKED: u32 = 0x0000_0080;
    /// `STRUCT_AtomicWhenCooked`.
    pub const ATOMIC_WHEN_COOKED: u32 = 0x0000_0100;

    /// (bit, conventional name) pairs.
    pub const NAMES: &[(u64, &str)] = &[
        (NATIVE as u64, "Native"),
        (EXPORT as u64, "Export"),
        (HAS_COMPONENTS as u64, "HasComponents"),
        (TRANSIENT as u64, "Transient"),
        (ATOMIC as u64, "Atomic"),
        (IMMUTABLE as u64, "Immutable"),
        (STRICT_CONFIG as u64, "StrictConfig"),
        (IMMUTABLE_WHEN_COOKED as u64, "ImmutableWhenCooked"),
        (ATOMIC_WHEN_COOKED as u64, "AtomicWhenCooked"),
    ];
}

/// `EStateFlags` (`UState::StateFlags`).
pub mod state {
    /// `STATE_Editable`.
    pub const EDITABLE: u32 = 0x0000_0001;
    /// `STATE_Auto`.
    pub const AUTO: u32 = 0x0000_0002;
    /// `STATE_Simulated`.
    pub const SIMULATED: u32 = 0x0000_0004;
    /// `STATE_HasLocals`.
    pub const HAS_LOCALS: u32 = 0x0000_0008;

    /// (bit, conventional name) pairs.
    pub const NAMES: &[(u64, &str)] = &[
        (EDITABLE as u64, "Editable"),
        (AUTO as u64, "Auto"),
        (SIMULATED as u64, "Simulated"),
        (HAS_LOCALS as u64, "HasLocals"),
    ];
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn describe_reports_known_and_unknown_bits() {
        assert_eq!(
            describe(0x580, property::NAMES),
            vec!["Parm", "OutParm", "ReturnParm"]
        );
        assert_eq!(
            describe(0x23411, function::NAMES),
            vec![
                "Final",
                "PreOperator",
                "Native",
                "Operator",
                "Static",
                "Public"
            ]
        );
        assert_eq!(
            describe(0x30, structure::NAMES),
            vec!["Atomic", "Immutable"]
        );
        assert_eq!(
            describe(0x8000_0000_0000_0000, object::NAMES),
            vec!["0x8000000000000000"]
        );
        assert!(describe(0, class::NAMES).is_empty());
    }
}
