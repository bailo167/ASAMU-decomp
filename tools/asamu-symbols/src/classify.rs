//! Ordered, documented classification rules.
//!
//! Every symbol gets exactly one [`Category`] from the **first** rule in
//! [`RULES`] that matches. Rules come in four tiers, in this order:
//!
//! 1. **name** — specific prefixes / namespaces observed in the data
//!    (e.g. `Scaleform::`, `SDL_`, `inflate*`, `ASAMU`).
//! 2. **provenance** — the STABS debug map says which compilation unit or static
//!    archive defined the symbol (sanitized component label).
//! 3. **import** — undefined symbols are attributed to the dylib their
//!    two-level-namespace library ordinal names.
//! 4. **convention** — weak fallbacks (UE3 type-prefix convention, static
//!    initialisers without provenance).
//!
//! Anything left is `unknown`. The rule table is rendered verbatim into the
//! JSON/Markdown output so the documentation cannot drift from the code.

use serde::Serialize;

use crate::macho::SymKind;
use crate::provenance::Component;

/// Symbol categories.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
pub enum Category {
    /// Game-specific (ASAMU package / name).
    #[serde(rename = "asamu")]
    Asamu,
    /// UDKBase (UDK game layer; there is no UTGame in this build).
    #[serde(rename = "udk")]
    Udk,
    /// Unreal Engine 3 core/engine/framework/platform modules.
    #[serde(rename = "ue3")]
    Ue3,
    /// Scaleform GFx 4 (static archives).
    #[serde(rename = "scaleform")]
    Scaleform,
    /// NVIDIA PhysX 2.8.x (static archives).
    #[serde(rename = "physx")]
    Physx,
    /// FaceFX SDK (not linked in this build; kept so absence is explicit).
    #[serde(rename = "facefx")]
    FaceFx,
    /// OpenAL API (imported from the bundled `openal.dylib`).
    #[serde(rename = "audio")]
    Audio,
    /// libogg / libvorbis (statically compiled).
    #[serde(rename = "ogg-vorbis")]
    OggVorbis,
    /// zlib (statically compiled).
    #[serde(rename = "zlib")]
    Zlib,
    /// LZO / lzopro (statically compiled).
    #[serde(rename = "lzo")]
    Lzo,
    /// SDL2 API (imported from the bundled `libSDL2-2.0.0.dylib`).
    #[serde(rename = "sdl")]
    Sdl,
    /// Steamworks API (imported from `libsteam_api.dylib`, plus SDK header inlines).
    #[serde(rename = "steam")]
    Steam,
    /// C++ runtime / STL.
    #[serde(rename = "cxx-runtime")]
    CxxRuntime,
    /// Objective-C runtime / Cocoa classes.
    #[serde(rename = "objc")]
    Objc,
    /// OS libraries and frameworks (libSystem, OpenGL, CoreFoundation, ...).
    #[serde(rename = "platform")]
    Platform,
    /// Compiler/linker generated labels and startup symbols.
    #[serde(rename = "compiler")]
    Compiler,
    /// No rule matched.
    #[serde(rename = "unknown")]
    Unknown,
}

impl Category {
    /// All categories in output order.
    pub const ALL: [Category; 17] = [
        Category::Asamu,
        Category::Udk,
        Category::Ue3,
        Category::Scaleform,
        Category::Physx,
        Category::FaceFx,
        Category::Audio,
        Category::OggVorbis,
        Category::Zlib,
        Category::Lzo,
        Category::Sdl,
        Category::Steam,
        Category::CxxRuntime,
        Category::Objc,
        Category::Platform,
        Category::Compiler,
        Category::Unknown,
    ];

    /// Stable id.
    pub fn id(self) -> &'static str {
        match self {
            Category::Asamu => "asamu",
            Category::Udk => "udk",
            Category::Ue3 => "ue3",
            Category::Scaleform => "scaleform",
            Category::Physx => "physx",
            Category::FaceFx => "facefx",
            Category::Audio => "audio",
            Category::OggVorbis => "ogg-vorbis",
            Category::Zlib => "zlib",
            Category::Lzo => "lzo",
            Category::Sdl => "sdl",
            Category::Steam => "steam",
            Category::CxxRuntime => "cxx-runtime",
            Category::Objc => "objc",
            Category::Platform => "platform",
            Category::Compiler => "compiler",
            Category::Unknown => "unknown",
        }
    }

    /// Parse an id.
    pub fn from_id(id: &str) -> Option<Category> {
        Category::ALL.iter().copied().find(|c| c.id() == id)
    }

    /// True for the gameplay-relevant layers (ASAMU, UDK, UE3).
    pub fn is_game_layer(self) -> bool {
        matches!(self, Category::Asamu | Category::Udk | Category::Ue3)
    }
}

/// Rule tier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Tier {
    /// Name pattern.
    Name,
    /// Debug-map provenance.
    Provenance,
    /// Import dylib.
    Import,
    /// Weak convention fallback.
    Convention,
}

/// The facts a rule may look at.
#[derive(Debug, Clone, Copy)]
pub struct View<'a> {
    /// Raw string-table name.
    pub raw: &'a str,
    /// Name with one Mach-O underscore removed.
    pub name: &'a str,
    /// Demangled text, if the name is an Itanium name that demangled.
    pub full: Option<&'a str>,
    /// Qualified demangled name without parameters/return type.
    pub qualified: Option<&'a str>,
    /// Symbol kind.
    pub kind: SymKind,
    /// Debug-map component, if attributed.
    pub component: Option<&'a Component>,
    /// Import dylib basename, for undefined symbols.
    pub dylib: Option<&'a str>,
}

/// Prefixes `cpp_demangle` puts in front of special names.
const SPECIAL_PREFIXES: [&str; 10] = [
    "typeinfo name for ",
    "typeinfo for ",
    "typeinfo fn for ",
    "guard variable for ",
    "construction vtable for ",
    "TLS init function for ",
    "TLS wrapper function for ",
    "non-transaction clone for ",
    "transaction clone for ",
    "java resource ",
];

/// Strip `cpp_demangle`'s special-name decoration and return the entity it is
/// about: `{vtable(X)}` → `X`, `{virtual override thunk({offset(-8)}, X::f)}` →
/// `X::f`, `typeinfo for X` → `X`, `reference temporary #0 for X` → `X`,
/// `construction vtable for X-in-Y` → `X`.
pub fn special_subject(s: &str) -> &str {
    for wrap in ["{vtable(", "{vtt("] {
        if let Some(inner) = s.strip_prefix(wrap).and_then(|r| r.strip_suffix(")}")) {
            return inner;
        }
    }
    if let Some(inner) = s
        .strip_prefix("{virtual override thunk(")
        .and_then(|r| r.strip_suffix(")}"))
    {
        // Last top-level ", " separates the offsets from the target.
        let bytes = inner.as_bytes();
        let mut depth: i32 = 0;
        let mut last = None;
        for (i, b) in bytes.iter().enumerate() {
            match b {
                b'(' | b'{' | b'<' => depth += 1,
                b')' | b'}' | b'>' => depth -= 1,
                b',' if depth == 0 => last = Some(i),
                _ => {}
            }
        }
        return match last {
            Some(i) => inner.get(i + 1..).unwrap_or(inner).trim_start(),
            None => inner,
        };
    }
    if let Some(rest) = s.strip_prefix("reference temporary #")
        && let Some(pos) = rest.find(" for ")
    {
        return rest.get(pos + 5..).unwrap_or(rest);
    }
    for p in SPECIAL_PREFIXES {
        if let Some(rest) = s.strip_prefix(p) {
            if p.starts_with("construction vtable") {
                return rest.split("-in-").next().unwrap_or(rest);
            }
            return rest;
        }
    }
    s
}

/// True if the demangled text is a special name (vtable, typeinfo, thunk, guard, …).
pub fn is_special_name(s: &str) -> bool {
    special_subject(s).len() != s.len()
}

impl<'a> View<'a> {
    /// The C++ qualified name with special-name decoration removed, or the plain name.
    pub fn subject(&self) -> &'a str {
        special_subject(self.qualified.unwrap_or(self.name))
    }

    /// Leading identifier of [`Self::subject`] (up to `::`, `<`, `(`, space).
    pub fn head(&self) -> &'a str {
        let s = self.subject();
        let end = s
            .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
            .unwrap_or(s.len());
        s.get(..end).unwrap_or("")
    }
}

/// One classification rule.
#[derive(Clone, Copy)]
pub struct Rule {
    /// Stable id (`tier.what`).
    pub id: &'static str,
    /// Tier.
    pub tier: Tier,
    /// Resulting category.
    pub category: Category,
    /// Human-readable description (rendered into docs).
    pub description: &'static str,
    /// Predicate.
    pub test: fn(&View<'_>) -> bool,
}

impl std::fmt::Debug for Rule {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Rule").field("id", &self.id).finish()
    }
}

fn starts_upper_after(s: &str, prefix: &str) -> bool {
    s.strip_prefix(prefix)
        .and_then(|r| r.chars().next())
        .map(|c| c.is_ascii_uppercase())
        .unwrap_or(false)
}

fn any_prefix(s: &str, prefixes: &[&str]) -> bool {
    prefixes.iter().any(|p| s.starts_with(p))
}

// ---- name tier --------------------------------------------------------------

fn r_asamu(v: &View<'_>) -> bool {
    v.raw.to_ascii_lowercase().contains("asamu")
}

fn r_objc(v: &View<'_>) -> bool {
    any_prefix(
        v.raw,
        &[
            "_OBJC_", "_objc_", "l_OBJC_", "L_OBJC_", "-[", "+[", "__OBJC",
        ],
    )
}

fn r_compiler_label(v: &View<'_>) -> bool {
    any_prefix(
        v.raw,
        &[
            "GCC_except_table",
            "ltmp",
            "___tcf_",
            "___cxx_global_var_init",
        ],
    ) || matches!(
        v.name,
        "dyld_stub_binder"
            | "start"
            | "pvars"
            | "_mh_execute_header"
            | "__dso_handle"
            | "NXArgc"
            | "NXArgv"
            | "environ"
            | "__progname"
    )
}

fn r_cxx(v: &View<'_>) -> bool {
    let head = v.head();
    if matches!(head, "std" | "__gnu_cxx" | "__cxxabiv1") {
        return true;
    }
    if v.qualified.is_some() {
        let subj = v.subject();
        if subj.starts_with("operator new") || subj.starts_with("operator delete") {
            return true;
        }
    }
    any_prefix(v.raw, &["___cxa_", "___gxx_", "__Unwind_"])
}

fn r_scaleform(v: &View<'_>) -> bool {
    v.head() == "Scaleform"
}

fn r_physx(v: &View<'_>) -> bool {
    let head = v.head();
    if matches!(
        head,
        "Opcode" | "IceCore" | "IceMaths" | "Meshmerizer" | "HullLib"
    ) {
        return true;
    }
    let physx_named = ["Nx", "Np", "Pxd", "Pxs", "Pxc", "Pxn"]
        .iter()
        .any(|p| starts_upper_after(head, p))
        || head.starts_with("NX_");
    if !physx_named {
        return false;
    }
    // PhysX SDK header code instantiated in a UE3 unit is always a member,
    // vtable or typeinfo of an `Nx…` type (`NxD6JointDesc::setToDefault`,
    // `{vtable(NxForceFieldKernelRadial)}`). A plain variable or function that a
    // UE3 unit itself defines with an `Nx` prefix (the file-static `NxDumpIndex`
    // in `Engine/Src/UnPhysLevel.cpp`) is UE3 code and falls through to
    // provenance.
    if matches!(v.component, Some(Component::Ue3Module(_))) {
        let special = is_special_name(v.qualified.unwrap_or(v.name));
        let after_head = v.subject().get(head.len()..).unwrap_or("");
        let member = after_head.starts_with("::") || after_head.starts_with('<');
        if !special && !member {
            return false;
        }
    }
    true
}

fn r_sdl(v: &View<'_>) -> bool {
    v.name.starts_with("SDL_")
}

fn r_steam(v: &View<'_>) -> bool {
    if v.kind == SymKind::Undefined && v.name.starts_with("Steam") {
        return true;
    }
    let head = v.head();
    matches!(
        head,
        "CSteamID" | "CGameID" | "CCallbackBase" | "CCallback" | "CCallResult"
    ) || starts_upper_after(head, "ISteam")
}

fn r_openal(v: &View<'_>) -> bool {
    starts_upper_after(v.name, "al") || starts_upper_after(v.name, "alc")
}

fn r_zlib(v: &View<'_>) -> bool {
    let n = v.name;
    any_prefix(
        n,
        &[
            "inflate",
            "deflate",
            "zcalloc",
            "zcfree",
            "zError",
            "zlibVersion",
            "_tr_",
            "z_errmsg",
            "adler32",
            "crc32",
            "get_crc_table",
            "compress",
            "uncompress",
            "_length_code",
            "_dist_code",
        ],
    )
}

fn r_lzo(v: &View<'_>) -> bool {
    any_prefix(v.name, &["lzo", "_lzo", "__lzo"])
}

fn r_ogg_vorbis(v: &View<'_>) -> bool {
    any_prefix(
        v.name,
        &["ogg", "vorbis", "ov_", "_vorbis", "_ogg", "__vorbis"],
    )
}

fn r_facefx(v: &View<'_>) -> bool {
    v.head() == "OC3Ent"
}

fn r_udk(v: &View<'_>) -> bool {
    let head = v.head();
    let type_like = ["UDK", "AUDK", "UUDK", "FUDK"]
        .iter()
        .any(|p| starts_upper_after(head, p));
    let subj = v.subject();
    type_like
        || any_prefix(
            v.name,
            &[
                "UDKBASE_",
                "GUDKBase",
                "intAUDK",
                "intUUDK",
                "_GLOBAL__sub_I_UDK",
            ],
        )
        || subj.starts_with("AutoInitializeRegistrantsUDKBase")
        || subj.starts_with("AutoGenerateNamesUDKBase")
}

fn r_ue3_header_statics(v: &View<'_>) -> bool {
    v.head() == "GlobalVectorConstants"
        || matches!(
            v.subject(),
            "SSE_ONE" | "SSE_XYZ_MASK" | "SSE_SIGN_MASK" | "DebugUtilColor"
        )
}

/// Uppercase package prefixes of `AutoGenerateNames` FName globals (plus the
/// engine-wide key-name tables).
pub const FNAME_PREFIXES: [&str; 10] = [
    "CORE_",
    "ENGINE_",
    "GAMEFRAMEWORK_",
    "IPDRV_",
    "GFXUI_",
    "ONLINESUBSYSTEMSTEAMWORKS_",
    "UDKBASE_",
    "ASAMU_",
    "KEY_",
    "UIKEY_",
];

fn r_ue3_registration(v: &View<'_>) -> bool {
    let subj = v.subject();
    if subj.starts_with("AutoInitializeRegistrants") || subj.starts_with("AutoGenerateNames") {
        return true;
    }
    let n = v.name;
    if let Some(rest) = n.strip_prefix("int")
        && (rest.starts_with('U') || rest.starts_with('A'))
        && rest.contains("exec")
    {
        return true;
    }
    if n.starts_with('G') && n.ends_with("Natives") && n.len() > "GNatives".len() {
        return true;
    }
    FNAME_PREFIXES.iter().any(|p| n.starts_with(p))
}

// ---- provenance tier --------------------------------------------------------

fn r_prov_asamu(v: &View<'_>) -> bool {
    matches!(v.component, Some(Component::Ue3Module(m)) if m == "ASAMU")
}
fn r_prov_udk(v: &View<'_>) -> bool {
    matches!(v.component, Some(Component::Ue3Module(m)) if m == "UDKBase")
}
fn r_prov_ue3(v: &View<'_>) -> bool {
    matches!(v.component, Some(Component::Ue3Module(_)))
}
fn r_prov_physx(v: &View<'_>) -> bool {
    matches!(v.component, Some(Component::Physx(_)))
}
fn r_prov_scaleform(v: &View<'_>) -> bool {
    matches!(v.component, Some(Component::Scaleform(_)))
}
fn r_prov_zlib(v: &View<'_>) -> bool {
    matches!(v.component, Some(Component::External(l)) if l == "zlib")
}
fn r_prov_lzo(v: &View<'_>) -> bool {
    matches!(v.component, Some(Component::External(l)) if l.starts_with("lzo"))
}
fn r_prov_oggvorbis(v: &View<'_>) -> bool {
    matches!(v.component, Some(Component::External(l)) if l == "libogg" || l == "libvorbis")
}

// ---- import tier ------------------------------------------------------------

fn r_imp_sdl(v: &View<'_>) -> bool {
    v.dylib.map(|d| d.starts_with("libSDL2")).unwrap_or(false)
}
fn r_imp_steam(v: &View<'_>) -> bool {
    v.dylib
        .map(|d| d.starts_with("libsteam_api"))
        .unwrap_or(false)
}
fn r_imp_openal(v: &View<'_>) -> bool {
    v.dylib
        .map(|d| d.to_ascii_lowercase().starts_with("openal"))
        .unwrap_or(false)
}
fn r_imp_objc(v: &View<'_>) -> bool {
    v.dylib.map(|d| d.starts_with("libobjc")).unwrap_or(false)
}
fn r_imp_cxx(v: &View<'_>) -> bool {
    v.dylib
        .map(|d| d.starts_with("libstdc++") || d.starts_with("libc++"))
        .unwrap_or(false)
}
fn r_imp_platform(v: &View<'_>) -> bool {
    v.kind == SymKind::Undefined
}

// ---- convention tier --------------------------------------------------------

fn r_static_init(v: &View<'_>) -> bool {
    v.name.starts_with("_GLOBAL__sub_I_")
}

fn r_ue3_convention(v: &View<'_>) -> bool {
    let head = v.head();
    let mut chars = head.chars();
    match (chars.next(), chars.next(), chars.next()) {
        (Some(p), Some(u), Some(l)) => {
            matches!(p, 'U' | 'A' | 'F' | 'T' | 'E' | 'I')
                && u.is_ascii_uppercase()
                && (l.is_ascii_lowercase() || l.is_ascii_uppercase())
        }
        _ => false,
    }
}

/// The ordered rule table. First match wins.
pub const RULES: &[Rule] = &[
    Rule {
        id: "name.asamu",
        tier: Tier::Name,
        category: Category::Asamu,
        description: "raw name contains `asamu` (case-insensitive): UASAMUSystemSettingsManager, AutoInitializeRegistrantsASAMU, Gasamu…Natives, ASAMU*.cpp initialisers",
        test: r_asamu,
    },
    Rule {
        id: "name.objc",
        tier: Tier::Name,
        category: Category::Objc,
        description: "Objective-C runtime names: `_OBJC_*`, `_objc_*`, `-[…]`/`+[…]`",
        test: r_objc,
    },
    Rule {
        id: "name.compiler-label",
        tier: Tier::Name,
        category: Category::Compiler,
        description: "compiler/linker labels and crt1 startup symbols: `GCC_except_table*`, `ltmp*`, `___tcf_*`, `___cxx_global_var_init*`, `start`, `pvars`, `dyld_stub_binder`, `__mh_execute_header`, `_NXArgc`/`_NXArgv`/`_environ`/`___progname`",
        test: r_compiler_label,
    },
    Rule {
        id: "name.cxx-runtime",
        tier: Tier::Name,
        category: Category::CxxRuntime,
        description: "C++ runtime/STL: demangled owner `std::`/`__gnu_cxx::`/`__cxxabiv1::`, global `operator new/delete`, `___cxa_*`, `___gxx_*`, `__Unwind_*`",
        test: r_cxx,
    },
    Rule {
        id: "name.scaleform",
        tier: Tier::Name,
        category: Category::Scaleform,
        description: "demangled namespace `Scaleform::` (Scaleform GFx 4)",
        test: r_scaleform,
    },
    Rule {
        id: "name.physx",
        tier: Tier::Name,
        category: Category::Physx,
        description: "PhysX 2.8 naming: leading type `Nx*`/`Np*`/`Pxd*`/`Pxs*`/`Pxc*`/`Pxn*`/`NX_*`, namespaces `Opcode::`/`IceCore::`/`IceMaths::`/`Meshmerizer::`/`HullLib::`; inside a UE3 unit only members/vtables/typeinfo of such types (SDK header code), not plain UE3 variables such as `NxDumpIndex`",
        test: r_physx,
    },
    Rule {
        id: "name.facefx",
        tier: Tier::Name,
        category: Category::FaceFx,
        description: "FaceFX SDK namespace `OC3Ent::` (UE3's own UFaceFX* integration classes are UE3, not SDK)",
        test: r_facefx,
    },
    Rule {
        id: "name.sdl",
        tier: Tier::Name,
        category: Category::Sdl,
        description: "C names `SDL_*`",
        test: r_sdl,
    },
    Rule {
        id: "name.steam",
        tier: Tier::Name,
        category: Category::Steam,
        description: "undefined `Steam*` flat API imports; Steamworks SDK header types `CSteamID`, `CGameID`, `CCallback*`, `CCallResult`, `ISteam*`",
        test: r_steam,
    },
    Rule {
        id: "name.openal",
        tier: Tier::Name,
        category: Category::Audio,
        description: "OpenAL C API `al[A-Z]*` / `alc[A-Z]*`",
        test: r_openal,
    },
    Rule {
        id: "name.zlib",
        tier: Tier::Name,
        category: Category::Zlib,
        description: "zlib C API/internals: `inflate*`, `deflate*`, `crc32`, `adler32`, `_tr_*`, `zcalloc`, `zcfree`, `z_errmsg`, `compress*`, `uncompress`",
        test: r_zlib,
    },
    Rule {
        id: "name.lzo",
        tier: Tier::Name,
        category: Category::Lzo,
        description: "LZO / lzopro C names `lzo*`, `_lzo*`, `__lzo*`",
        test: r_lzo,
    },
    Rule {
        id: "name.ogg-vorbis",
        tier: Tier::Name,
        category: Category::OggVorbis,
        description: "libogg/libvorbis C names `ogg*`, `vorbis*`, `ov_*`, `_vorbis*`, `_ogg*`",
        test: r_ogg_vorbis,
    },
    Rule {
        id: "name.udk",
        tier: Tier::Name,
        category: Category::Udk,
        description: "UDK layer names: leading type `UDK*`/`AUDK*`/`UUDK*`/`FUDK*`, `GUDKBase…Natives`, `UDKBASE_*` FNames, `intAUDK…`/`intUUDK…` natives, `AutoInitializeRegistrantsUDKBase`, `__GLOBAL__sub_I_UDK*`",
        test: r_udk,
    },
    Rule {
        id: "name.ue3-header-statics",
        tier: Tier::Name,
        category: Category::Ue3,
        description: "UE3 header-defined statics duplicated into every compilation unit: `GlobalVectorConstants::*`, `SSE_ONE`, `SSE_XYZ_MASK`, `SSE_SIGN_MASK`, `DebugUtilColor`",
        test: r_ue3_header_statics,
    },
    Rule {
        id: "provenance.asamu",
        tier: Tier::Provenance,
        category: Category::Asamu,
        description: "debug map: defined in a compilation unit under `Development/Src/ASAMU/`",
        test: r_prov_asamu,
    },
    Rule {
        id: "provenance.udk",
        tier: Tier::Provenance,
        category: Category::Udk,
        description: "debug map: defined in a compilation unit under `Development/Src/UDKBase/`",
        test: r_prov_udk,
    },
    Rule {
        id: "provenance.ue3",
        tier: Tier::Provenance,
        category: Category::Ue3,
        description: "debug map: defined in any other `Development/Src/<Module>/` unit (Core, Engine, GameFramework, IpDrv, GFxUI, OnlineSubsystemSteamworks, OpenGLDrv, Mac, Launch, ALAudio)",
        test: r_prov_ue3,
    },
    Rule {
        id: "provenance.physx",
        tier: Tier::Provenance,
        category: Category::Physx,
        description: "debug map: object from a PhysX static archive (`libPhysXCore.a`, `libLowLevel.a`, `libPhysXExtensions.a`, `libPhysXCooking.a`) or PhysX SDK source",
        test: r_prov_physx,
    },
    Rule {
        id: "provenance.scaleform",
        tier: Tier::Provenance,
        category: Category::Scaleform,
        description: "debug map: object from a Scaleform archive (`libgfx*.a`) or Scaleform source",
        test: r_prov_scaleform,
    },
    Rule {
        id: "provenance.zlib",
        tier: Tier::Provenance,
        category: Category::Zlib,
        description: "debug map: unit under `External/zlib/`",
        test: r_prov_zlib,
    },
    Rule {
        id: "provenance.lzo",
        tier: Tier::Provenance,
        category: Category::Lzo,
        description: "debug map: unit under `External/lzo*/`",
        test: r_prov_lzo,
    },
    Rule {
        id: "provenance.ogg-vorbis",
        tier: Tier::Provenance,
        category: Category::OggVorbis,
        description: "debug map: unit under `External/libogg-*/` or `External/libvorbis-*/`",
        test: r_prov_oggvorbis,
    },
    Rule {
        id: "import.sdl",
        tier: Tier::Import,
        category: Category::Sdl,
        description: "undefined, bound to `libSDL2-2.0.0.dylib`",
        test: r_imp_sdl,
    },
    Rule {
        id: "import.steam",
        tier: Tier::Import,
        category: Category::Steam,
        description: "undefined, bound to `libsteam_api.dylib`",
        test: r_imp_steam,
    },
    Rule {
        id: "import.openal",
        tier: Tier::Import,
        category: Category::Audio,
        description: "undefined, bound to `openal.dylib`",
        test: r_imp_openal,
    },
    Rule {
        id: "import.objc",
        tier: Tier::Import,
        category: Category::Objc,
        description: "undefined, bound to `libobjc`",
        test: r_imp_objc,
    },
    Rule {
        id: "import.cxx-runtime",
        tier: Tier::Import,
        category: Category::CxxRuntime,
        description: "undefined, bound to `libstdc++`",
        test: r_imp_cxx,
    },
    Rule {
        id: "import.platform",
        tier: Tier::Import,
        category: Category::Platform,
        description: "any other undefined symbol (on this binary: libSystem, OpenGL, CoreFoundation, CoreServices, Foundation, AppKit)",
        test: r_imp_platform,
    },
    Rule {
        id: "convention.static-init",
        tier: Tier::Convention,
        category: Category::Compiler,
        description: "`__GLOBAL__sub_I_<file>` static initialiser without debug-map provenance",
        test: r_static_init,
    },
    Rule {
        id: "convention.ue3-registration",
        tier: Tier::Convention,
        category: Category::Ue3,
        description: "UE3 native registration names without provenance: `AutoInitializeRegistrants*`, `AutoGenerateNames*`, `int<Class>exec<Func>` native pointers, `G<pkg><Class>Natives` tables, `<PKG>_<Name>` FName globals",
        test: r_ue3_registration,
    },
    Rule {
        id: "convention.ue3-type-prefix",
        tier: Tier::Convention,
        category: Category::Ue3,
        description: "leading type follows the UE3 prefix convention `[UAFTEI][A-Z]…` (weak; only reached without provenance)",
        test: r_ue3_convention,
    },
];

/// Classify a symbol: returns the category and the id of the rule that matched
/// (`"none"` for unknown).
pub fn classify(v: &View<'_>) -> (Category, &'static str) {
    for rule in RULES {
        if (rule.test)(v) {
            return (rule.category, rule.id);
        }
    }
    (Category::Unknown, "none")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::demangle::{Demangled, demangle, strip_macho_underscore};

    struct Owned {
        raw: String,
        full: Option<String>,
        qualified: Option<String>,
    }

    fn owned(raw: &str) -> Owned {
        let (full, qualified) = match demangle(raw) {
            Demangled::Ok {
                full, qualified, ..
            } => (Some(full), Some(qualified)),
            _ => (None, None),
        };
        Owned {
            raw: raw.to_string(),
            full,
            qualified,
        }
    }

    fn cat_with(
        raw: &str,
        kind: SymKind,
        component: Option<&Component>,
        dylib: Option<&str>,
    ) -> (Category, &'static str) {
        let o = owned(raw);
        let v = View {
            raw: &o.raw,
            name: strip_macho_underscore(&o.raw),
            full: o.full.as_deref(),
            qualified: o.qualified.as_deref(),
            kind,
            component,
            dylib,
        };
        classify(&v)
    }

    fn cat(raw: &str) -> Category {
        cat_with(raw, SymKind::Text, None, None).0
    }

    #[test]
    fn asamu_names() {
        assert_eq!(
            cat("__ZN27UASAMUSystemSettingsManager11SetLanguageERK7FString"),
            Category::Asamu
        );
        assert_eq!(
            cat("_GasamuUASAMUSystemSettingsManagerNatives"),
            Category::Asamu
        );
        assert_eq!(cat("__GLOBAL__sub_I_ASAMU.cpp"), Category::Asamu);
    }

    #[test]
    fn middleware_names() {
        assert_eq!(
            cat("__ZN9Scaleform3GFx9MovieImpl7AdvanceEfjb"),
            Category::Scaleform
        );
        assert_eq!(cat("__ZN7NpScene8simulateEf"), Category::Physx);
        assert_eq!(cat("__ZN6Opcode8Collider9InitQueryEv"), Category::Physx);
        assert_eq!(cat("_SDL_PollEvent"), Category::Sdl);
        assert_eq!(
            cat_with("_SteamAPI_Init", SymKind::Undefined, None, None).0,
            Category::Steam
        );
        assert_eq!(cat("_alSourcePlay"), Category::Audio);
        assert_eq!(cat("_alcOpenDevice"), Category::Audio);
        assert_eq!(cat("_inflateInit2_"), Category::Zlib);
        assert_eq!(cat("_lzo1x_decompress_safe"), Category::Lzo);
        assert_eq!(cat("_ov_open_callbacks"), Category::OggVorbis);
        assert_eq!(cat("_oggpack_read"), Category::OggVorbis);
    }

    #[test]
    fn runtime_and_compiler_names() {
        assert_eq!(
            cat("__ZNSt6vectorIiSaIiEE9push_backERKi"),
            Category::CxxRuntime
        );
        assert_eq!(cat("__Znwm"), Category::CxxRuntime);
        assert_eq!(cat("___cxa_guard_acquire"), Category::CxxRuntime);
        assert_eq!(cat("GCC_except_table42"), Category::Compiler);
        assert_eq!(cat("_OBJC_CLASS_$_NSAutoreleasePool"), Category::Objc);
        assert_eq!(cat("_objc_msgSend"), Category::Objc);
    }

    #[test]
    fn ue3_and_udk_names() {
        assert_eq!(cat("__ZN5APawn11physWalkingEfi"), Category::Ue3);
        assert_eq!(cat("_intAActorexecTrace"), Category::Ue3);
        assert_eq!(
            classify_id("_intAActorexecTrace"),
            "convention.ue3-registration"
        );
        assert_eq!(cat("start"), Category::Compiler);
        assert_eq!(cat("__mh_execute_header"), Category::Compiler);
        assert_eq!(cat("_GEngineAActorNatives"), Category::Ue3);
        assert_eq!(cat("_ENGINE_Landed"), Category::Ue3);
        assert_eq!(
            cat("__ZN8AUDKPawn13execSetPuppetER6FFramePv"),
            Category::Udk
        );
        assert_eq!(cat("_UDKBASE_StoppedFalling"), Category::Udk);
        assert_eq!(cat("__ZN21GlobalVectorConstants9Float0001E"), Category::Ue3);
    }

    #[test]
    fn rule_order_name_beats_provenance() {
        let engine = Component::Ue3Module("Engine".into());
        // A Scaleform name compiled into a UE3 unit is still Scaleform.
        let (c, id) = cat_with(
            "__ZN9Scaleform6String6AppendEPKcm",
            SymKind::Text,
            Some(&engine),
            None,
        );
        assert_eq!((c, id), (Category::Scaleform, "name.scaleform"));
        // A generic PhysX name is only recognised through provenance.
        let px = Component::Physx("libPhysXCore.a".into());
        let (c, id) = cat_with("__ZN5Shape7getBodyEv", SymKind::Text, Some(&px), None);
        assert_eq!((c, id), (Category::Physx, "provenance.physx"));
        // ASAMU-module unit with no ASAMU in the name.
        let asamu = Component::Ue3Module("ASAMU".into());
        let (c, id) = cat_with(
            "__GLOBAL__sub_I_NullSurveys.cpp",
            SymKind::Text,
            Some(&asamu),
            None,
        );
        assert_eq!((c, id), (Category::Asamu, "provenance.asamu"));
        // Header statics in the ASAMU unit stay UE3.
        let (c, _) = cat_with("_SSE_ONE", SymKind::Data, Some(&asamu), None);
        assert_eq!(c, Category::Ue3);
    }

    #[test]
    fn physx_names_inside_ue3_units() {
        let engine = Component::Ue3Module("Engine".into());
        // PhysX SDK header code compiled into a UE3 unit stays PhysX: members,
        // vtables, template members.
        let (c, id) = cat_with(
            "__ZN13NxD6JointDesc12setToDefaultEv",
            SymKind::Text,
            Some(&engine),
            None,
        );
        assert_eq!((c, id), (Category::Physx, "name.physx"));
        let (c, _) = cat_with(
            "__ZTV24NxForceFieldKernelRadial",
            SymKind::Data,
            Some(&engine),
            None,
        );
        assert_eq!(c, Category::Physx);
        // A plain `Nx`-prefixed variable defined by UE3 itself is UE3.
        let (c, id) = cat_with("__ZL11NxDumpIndex", SymKind::Data, Some(&engine), None);
        assert_eq!((c, id), (Category::Ue3, "provenance.ue3"));
        // The same plain name inside a PhysX archive (or without provenance) is PhysX.
        let px = Component::Physx("libPhysXCore.a".into());
        let (c, _) = cat_with("__ZL11NxDumpIndex", SymKind::Data, Some(&px), None);
        assert_eq!(c, Category::Physx);
        assert_eq!(cat("__ZL11NxDumpIndex"), Category::Physx);
    }

    #[test]
    fn imports_by_dylib() {
        let (c, id) = cat_with("_glClear", SymKind::Undefined, None, Some("OpenGL"));
        assert_eq!((c, id), (Category::Platform, "import.platform"));
        let (c, _) = cat_with(
            "_SDL_Init",
            SymKind::Undefined,
            None,
            Some("libSDL2-2.0.0.dylib"),
        );
        assert_eq!(c, Category::Sdl);
    }

    #[test]
    fn fallbacks() {
        assert_eq!(cat("__GLOBAL__sub_I_Foo.cpp"), Category::Compiler);
        assert_eq!(cat("__ZN9UFooThing3BarEv"), Category::Ue3);
        assert_eq!(cat("_some_c_function"), Category::Unknown);
        assert_eq!(classify_id("_some_c_function"), "none");
    }

    fn classify_id(raw: &str) -> &'static str {
        cat_with(raw, SymKind::Text, None, None).1
    }

    #[test]
    fn special_names_are_unwrapped() {
        assert_eq!(special_subject("{vtable(UFoo)}"), "UFoo");
        assert_eq!(special_subject("{vtt(UFoo)}"), "UFoo");
        assert_eq!(
            special_subject("{virtual override thunk({offset(-8)}, UFoo::Bar())}"),
            "UFoo::Bar()"
        );
        assert_eq!(
            special_subject("typeinfo for std::exception"),
            "std::exception"
        );
        assert_eq!(special_subject("guard variable for gX"), "gX");
        assert_eq!(special_subject("reference temporary #0 for gY"), "gY");
        assert_eq!(special_subject("construction vtable for A-in-B"), "A");
        assert_eq!(special_subject("APawn::physWalking"), "APawn::physWalking");
        assert_eq!(special_subject("{vtable("), "{vtable(");
        assert!(is_special_name("{vtable(UFoo)}"));
        assert!(!is_special_name("UFoo::Bar"));
        // A vtable of a Scaleform class is Scaleform.
        assert_eq!(cat("__ZTVN9Scaleform3GFx9MovieImplE"), Category::Scaleform);
        assert_eq!(cat("__ZTV27UASAMUSystemSettingsManager"), Category::Asamu);
    }

    #[test]
    fn rule_ids_are_unique_and_categories_round_trip() {
        let mut ids: Vec<&str> = RULES.iter().map(|r| r.id).collect();
        ids.sort_unstable();
        let before = ids.len();
        ids.dedup();
        assert_eq!(before, ids.len());
        for c in Category::ALL {
            assert_eq!(Category::from_id(c.id()), Some(c));
        }
    }
}
