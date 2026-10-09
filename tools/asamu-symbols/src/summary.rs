//! The sanitized, deterministic summary (statistics only — never the raw
//! symbol list). All maps are `BTreeMap`s and all lists are sorted, so the JSON
//! is byte-for-byte reproducible for the same input binary.

use std::collections::BTreeMap;

use serde::Serialize;

use crate::analysis::{Analysis, DemangleStatus};
use crate::anchors::{self, AnchorReport};
use crate::classify::{Category, RULES};
use crate::keywords::{self, KeywordResult, PhysicsFn};
use crate::macho::{Scope, SymKind};
use crate::natives::{self, NativesReport};
use crate::registry::{self, ClassInfo, ModuleNatives};

/// Bump when the JSON layout changes.
pub const SCHEMA_VERSION: u32 = 1;
/// Published summaries must stay below this size.
pub const MAX_SUMMARY_BYTES: usize = 200 * 1024;

/// Classes whose full native details are included.
pub use crate::anchors::GAMEPLAY_CLASSES as CLASSES_OF_INTEREST;

/// Binary identity.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct BinaryInfo {
    /// Size in bytes.
    pub file_size: u64,
    /// `LC_UUID`.
    pub uuid: Option<String>,
    /// Dependent dylibs (basenames) in ordinal order.
    pub dylibs: Vec<String>,
}

/// Symbol-table totals.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Totals {
    /// `LC_SYMTAB.nsyms`.
    pub nlist_entries: u64,
    /// STABS debug entries (hidden by plain `nm`).
    pub stab_entries: u64,
    /// STABS entries by type.
    pub stab_by_type: BTreeMap<String, u64>,
    /// Regular entries (what plain `nm` lists).
    pub symbols: u64,
    /// Defined (not undefined).
    pub defined: u64,
    /// Undefined.
    pub undefined: u64,
    /// `nm` type letter census.
    pub nm_type_census: BTreeMap<String, u64>,
    /// Kind counts.
    pub kinds: BTreeMap<String, u64>,
    /// Scope counts.
    pub scopes: BTreeMap<String, u64>,
    /// `kind/scope` counts.
    pub kind_scope: BTreeMap<String, u64>,
    /// Symbols per `segment,section`.
    pub sections: BTreeMap<String, u64>,
    /// Symbols with `N_PEXT` set.
    pub private_extern: u64,
    /// Name-table read problems.
    pub invalid_name_offsets: u64,
    /// Names that were not UTF-8.
    pub non_utf8_names: u64,
}

/// Demangling statistics.
#[derive(Debug, Clone, Default, Serialize, PartialEq, Eq)]
pub struct DemangleStats {
    /// Raw names starting with `_` (Mach-O C-level prefix).
    pub leading_underscore: u64,
    /// Raw names without it (assembler-local labels, `start`, …).
    pub no_leading_underscore: u64,
    /// Itanium candidates (`__Z…`).
    pub itanium_candidates: u64,
    /// Demangled directly.
    pub ok: u64,
    /// Demangled after stripping a GCC suffix (`.b`, `.0`, …).
    pub ok_suffix_stripped: u64,
    /// Failed.
    pub failed: u64,
    /// Not Itanium.
    pub not_mangled: u64,
}

/// A rule row with its match count.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct RuleRow {
    /// Order (1-based).
    pub order: usize,
    /// Rule id.
    pub id: String,
    /// Tier.
    pub tier: String,
    /// Category.
    pub category: String,
    /// Description.
    pub description: String,
    /// Symbols classified by this rule.
    pub matched: u64,
}

/// Provenance summary.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Provenance {
    /// Debug-map units.
    pub units: u64,
    /// Units per component.
    pub units_per_component: BTreeMap<String, u32>,
    /// Regular symbols attributed to a unit.
    pub symbols_with_unit: u64,
    /// Symbols per origin (`ue3:Engine`, `dylib:OpenGL`, `none`, …).
    pub symbols_per_origin: BTreeMap<String, u64>,
    /// Address / name conflicts while building the map.
    pub address_conflicts: u64,
    /// GSYM name conflicts.
    pub name_conflicts: u64,
    /// External library version hints.
    pub external_versions: BTreeMap<String, String>,
    /// PhysX / Scaleform SDK marker directories.
    pub sdk_markers: Vec<String>,
    /// Compilation-unit file names per UE3 module (Engine listed by count only).
    pub ue3_module_units: BTreeMap<String, Vec<String>>,
    /// Compilation units per UE3 module.
    pub ue3_module_unit_counts: BTreeMap<String, u64>,
}

/// Native registration summary.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Natives {
    /// `AutoInitializeRegistrants<Pkg>` packages.
    pub registrant_packages: Vec<String>,
    /// `AutoGenerateNames<Pkg>` packages.
    pub generate_names_packages: Vec<String>,
    /// Natives-table package prefixes → tables.
    pub natives_table_prefixes: BTreeMap<String, u32>,
    /// Natives tables.
    pub natives_tables: u32,
    /// Native classes (`PrivateStaticClass`).
    pub native_classes: u32,
    /// Exec thunks.
    pub exec_thunks: u32,
    /// `int…exec…` registrations.
    pub int_registrations: u32,
    /// Unmatched `int…exec…` registrations.
    pub int_registrations_unmatched: u32,
    /// `__GLOBAL__sub_I_*` initialisers.
    pub static_initializers: u32,
    /// FName globals per prefix.
    pub fname_globals: BTreeMap<String, u32>,
    /// Per-module rollup with class → exec-thunk counts.
    pub modules: BTreeMap<String, ModuleNatives>,
    /// Decoded tables and pointers.
    pub decoded: NativesReport,
    /// Full details for selected gameplay classes.
    pub classes_of_interest: BTreeMap<String, ClassDetail>,
}

/// Class detail for `classes_of_interest`.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ClassDetail {
    /// Registry info.
    #[serde(flatten)]
    pub info: ClassInfo,
    /// Entries in the decoded natives table.
    pub natives_table_entries: Option<u32>,
}

/// One ASAMU-category symbol (complete list).
#[derive(Debug, Clone, Serialize, PartialEq, Eq, PartialOrd, Ord)]
pub struct AsamuSymbol {
    /// Address (hex, or `undefined`).
    pub address: String,
    /// `nm` letter.
    pub nm_type: String,
    /// Kind.
    pub kind: String,
    /// Demangled (or plain) name.
    pub name: String,
    /// Classification rule.
    pub rule: String,
    /// Sanitized unit.
    pub unit: Option<String>,
}

/// Middleware presence probe.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Probe {
    /// Library.
    pub library: String,
    /// What was searched.
    pub evidence_searched: String,
    /// Debug-map units from the library's own sources/archives.
    pub library_units: u64,
    /// Imports bound to the library's dylib.
    pub dylib_imports: u64,
    /// Name hits in UE3 integration code (any category).
    pub integration_name_hits: u64,
    /// Verdict.
    pub verdict: String,
}

/// The whole summary.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Summary {
    /// Schema version.
    pub schema: u32,
    /// Producing tool.
    pub tool: String,
    /// Binary identity.
    pub binary: BinaryInfo,
    /// Totals.
    pub totals: Totals,
    /// Demangling.
    pub demangle: DemangleStats,
    /// Category counts (all categories, including zero).
    pub categories: BTreeMap<String, u64>,
    /// Category → kind → count.
    pub category_by_kind: BTreeMap<String, BTreeMap<String, u64>>,
    /// Category → origin → count (rule cross-check).
    pub category_by_origin: BTreeMap<String, BTreeMap<String, u64>>,
    /// Ordered rule table with match counts.
    pub rules: Vec<RuleRow>,
    /// Provenance.
    pub provenance: Provenance,
    /// Middleware probes.
    pub middleware: Vec<Probe>,
    /// Native registration.
    pub natives: Natives,
    /// Complete ASAMU-category symbol list.
    pub asamu_symbols: Vec<AsamuSymbol>,
    /// Keyword search.
    pub keywords: BTreeMap<String, KeywordResult>,
    /// Movement-physics functions (game layer).
    pub physics_functions: Vec<PhysicsFn>,
    /// Ghidra anchors.
    pub anchors: AnchorReport,
}

fn bump(map: &mut BTreeMap<String, u64>, key: impl Into<String>) {
    let c = map.entry(key.into()).or_insert(0);
    *c = c.saturating_add(1);
}

fn probes(
    a: &Analysis,
    origin_counts: &BTreeMap<String, u64>,
    units: &BTreeMap<String, u32>,
) -> Vec<Probe> {
    let unit_sum = |pred: &dyn Fn(&str) -> bool| -> u64 {
        units
            .iter()
            .filter(|(k, _)| pred(k))
            .map(|(_, v)| u64::from(*v))
            .sum()
    };
    let origin_sum = |pred: &dyn Fn(&str) -> bool| -> u64 {
        origin_counts
            .iter()
            .filter(|(k, _)| pred(k))
            .map(|(_, v)| *v)
            .sum()
    };
    let displays: Vec<&str> = a.entries.iter().map(|e| a.display(e)).collect();
    // Case-sensitive: any include needle, no exclude needle.
    let name_hits = |include: &[&str], exclude: &[&str]| -> u64 {
        let count = displays
            .iter()
            .filter(|d| include.iter().any(|n| d.contains(n)))
            .filter(|d| !exclude.iter().any(|n| d.contains(n)))
            .count();
        u64::try_from(count).unwrap_or(u64::MAX)
    };
    let verdict = |units: u64, imports: u64, hits: u64| -> String {
        if units > 0 {
            "statically linked (library object code present)".into()
        } else if imports > 0 {
            "dynamically linked (imports bound to bundled dylib)".into()
        } else if hits > 0 {
            "not linked; UE3 integration/stub code only".into()
        } else {
            "absent".into()
        }
    };
    let mut out = Vec::new();
    let mut push = |library: &str, searched: &str, units: u64, imports: u64, hits: u64| {
        out.push(Probe {
            library: library.into(),
            evidence_searched: searched.into(),
            library_units: units,
            dylib_imports: imports,
            integration_name_hits: hits,
            verdict: verdict(units, imports, hits),
        });
    };
    push(
        "PhysX 2.8.4",
        "debug-map archives libPhysX*.a/libLowLevel.a; name `PhysX`",
        unit_sum(&|k| k.starts_with("physx:")),
        0,
        name_hits(&["PhysX"], &[]),
    );
    push(
        "Scaleform GFx 4",
        "debug-map archives libgfx*.a; namespace `Scaleform::`",
        unit_sum(&|k| k.starts_with("scaleform:")),
        0,
        name_hits(&["Scaleform::"], &[]),
    );
    push(
        "zlib",
        "debug-map External/zlib",
        unit_sum(&|k| k == "external:zlib"),
        0,
        name_hits(&["inflate", "deflate"], &[]),
    );
    push(
        "LZO (lzopro)",
        "debug-map External/lzopro",
        unit_sum(&|k| k.starts_with("external:lzo")),
        0,
        name_hits(&["lzo"], &[]),
    );
    push(
        "libogg / libvorbis",
        "debug-map External/libogg-*, External/libvorbis-*",
        unit_sum(&|k| k == "external:libogg" || k == "external:libvorbis"),
        0,
        name_hits(&["vorbis", "ogg_"], &[]),
    );
    push(
        "OpenAL",
        "imports bound to openal.dylib",
        0,
        origin_sum(&|k| k.to_ascii_lowercase().starts_with("dylib:openal")),
        name_hits(&["ALAudio"], &[]),
    );
    push(
        "SDL2",
        "imports bound to libSDL2-2.0.0.dylib",
        0,
        origin_sum(&|k| k.starts_with("dylib:libSDL2")),
        name_hits(&["SDL"], &[]),
    );
    push(
        "Steamworks",
        "imports bound to libsteam_api.dylib",
        0,
        origin_sum(&|k| k.starts_with("dylib:libsteam_api")),
        name_hits(&["Steamworks"], &[]),
    );
    push(
        "FaceFX",
        "namespace `OC3Ent::` (SDK) vs `FaceFX` names (UE3 glue)",
        0,
        0,
        name_hits(&["FaceFX", "OC3Ent"], &[]),
    );
    push("Bink", "name `Bink`", 0, 0, name_hits(&["Bink"], &[]));
    push(
        "SpeedTree",
        "name `SpeedTree`",
        0,
        0,
        name_hits(&["SpeedTree"], &[]),
    );
    push(
        "NVIDIA APEX",
        "names containing `Apex` (excluding `JumpApex`)",
        0,
        0,
        name_hits(&["Apex"], &["JumpApex"]),
    );
    push(
        "Recast/Detour",
        "names `Recast`/`Detour`",
        0,
        0,
        name_hits(&["Recast", "Detour"], &[]),
    );
    push(
        "Trioviz (stereo 3D)",
        "name `Trioviz`",
        0,
        0,
        name_hits(&["Trioviz"], &[]),
    );
    push("Havok", "name `Havok`", 0, 0, name_hits(&["Havok"], &[]));
    push(
        "Wwise",
        "namespace `AK::`, name `Wwise`",
        0,
        0,
        name_hits(&["AK::", "Wwise"], &[]),
    );
    push("FMOD", "name `FMOD`", 0, 0, name_hits(&["FMOD"], &[]));
    out
}

/// Build the summary.
pub fn build(a: &Analysis) -> Summary {
    let img = &a.image;
    let mut nm_census = BTreeMap::new();
    let mut kinds = BTreeMap::new();
    let mut scopes = BTreeMap::new();
    let mut kind_scope = BTreeMap::new();
    let mut sections = BTreeMap::new();
    let mut pext = 0u64;
    let mut dm = DemangleStats::default();
    let mut categories: BTreeMap<String, u64> = Category::ALL
        .iter()
        .map(|c| (c.id().to_string(), 0))
        .collect();
    let mut cat_kind: BTreeMap<String, BTreeMap<String, u64>> = BTreeMap::new();
    let mut cat_origin: BTreeMap<String, BTreeMap<String, u64>> = BTreeMap::new();
    let mut rule_counts: BTreeMap<&str, u64> = BTreeMap::new();
    let mut origin_counts: BTreeMap<String, u64> = BTreeMap::new();
    let mut asamu = Vec::new();
    let mut defined = 0u64;

    for e in &a.entries {
        let Some(sym) = a.symbol(e) else { continue };
        bump(&mut nm_census, sym.nm_type.to_string());
        bump(&mut kinds, sym.kind.id());
        let scope = match sym.scope {
            Scope::Global => "global",
            Scope::Local => "local",
        };
        bump(&mut scopes, scope);
        bump(&mut kind_scope, format!("{}/{scope}", sym.kind.id()));
        if let Some(s) = img.section_of(sym) {
            bump(&mut sections, format!("{},{}", s.segment, s.name));
        }
        if sym.private_extern {
            pext = pext.saturating_add(1);
        }
        if sym.kind != SymKind::Undefined {
            defined = defined.saturating_add(1);
        }
        if sym.raw.starts_with('_') {
            dm.leading_underscore = dm.leading_underscore.saturating_add(1);
        } else {
            dm.no_leading_underscore = dm.no_leading_underscore.saturating_add(1);
        }
        match e.demangle {
            DemangleStatus::NotMangled => dm.not_mangled = dm.not_mangled.saturating_add(1),
            DemangleStatus::Ok => dm.ok = dm.ok.saturating_add(1),
            DemangleStatus::OkSuffixStripped => {
                dm.ok_suffix_stripped = dm.ok_suffix_stripped.saturating_add(1)
            }
            DemangleStatus::Failed => dm.failed = dm.failed.saturating_add(1),
        }
        let cat = e.category.id();
        bump(&mut categories, cat);
        bump(cat_kind.entry(cat.to_string()).or_default(), sym.kind.id());
        let origin = a.origin_id(e);
        bump(
            cat_origin.entry(cat.to_string()).or_default(),
            origin.clone(),
        );
        bump(&mut origin_counts, origin);
        let rc = rule_counts.entry(e.rule).or_insert(0);
        *rc = rc.saturating_add(1);
        if e.category == Category::Asamu {
            asamu.push(AsamuSymbol {
                address: if sym.kind == SymKind::Undefined {
                    "undefined".into()
                } else {
                    format!("0x{:x}", sym.value)
                },
                nm_type: sym.nm_type.to_string(),
                kind: sym.kind.id().into(),
                name: a.display(e).to_string(),
                rule: e.rule.to_string(),
                unit: a.unit(e).map(|u| u.rel_path.clone()),
            });
        }
    }
    dm.itanium_candidates = dm
        .ok
        .saturating_add(dm.ok_suffix_stripped)
        .saturating_add(dm.failed);
    asamu.sort();

    let mut rules: Vec<RuleRow> = RULES
        .iter()
        .enumerate()
        .map(|(i, r)| RuleRow {
            order: i + 1,
            id: r.id.to_string(),
            tier: format!("{:?}", r.tier).to_ascii_lowercase(),
            category: r.category.id().to_string(),
            description: r.description.to_string(),
            matched: rule_counts.get(r.id).copied().unwrap_or(0),
        })
        .collect();
    rules.push(RuleRow {
        order: RULES.len() + 1,
        id: "none".into(),
        tier: "fallback".into(),
        category: Category::Unknown.id().into(),
        description: "no rule matched".into(),
        matched: rule_counts.get("none").copied().unwrap_or(0),
    });

    let reg = registry::build(a);
    let decoded = natives::decode(a, &reg);
    let mut classes_of_interest = BTreeMap::new();
    for name in CLASSES_OF_INTEREST {
        if let Some(info) = reg.classes.get(*name) {
            classes_of_interest.insert(
                (*name).to_string(),
                ClassDetail {
                    info: info.clone(),
                    natives_table_entries: decoded.table_entries_per_class.get(*name).copied(),
                },
            );
        }
    }

    let mut ue3_module_units = BTreeMap::new();
    let mut ue3_module_unit_counts = BTreeMap::new();
    for (module, files) in &reg.ue3_module_units {
        ue3_module_unit_counts.insert(module.clone(), u64::try_from(files.len()).unwrap_or(0));
        if module != "Engine" {
            ue3_module_units.insert(module.clone(), files.clone());
        }
    }

    let middleware = probes(a, &origin_counts, &reg.units_per_component);

    let mut decoded_slim = decoded.clone();
    // Per-class table sizes are folded into classes_of_interest; keep totals only.
    decoded_slim.table_entries_per_class.clear();

    Summary {
        schema: SCHEMA_VERSION,
        tool: "asamu-symbols".into(),
        binary: BinaryInfo {
            file_size: img.file_size,
            uuid: img.uuid.clone(),
            dylibs: img.dylibs.clone(),
        },
        totals: Totals {
            nlist_entries: img.stats.nlist_total,
            stab_entries: img.stats.stab_entries,
            stab_by_type: img.stats.stab_by_type.clone(),
            symbols: u64::try_from(img.symbols.len()).unwrap_or(u64::MAX),
            defined,
            undefined: kinds.get("undefined").copied().unwrap_or(0),
            nm_type_census: nm_census,
            kinds,
            scopes,
            kind_scope,
            sections,
            private_extern: pext,
            invalid_name_offsets: img.stats.invalid_name_offsets,
            non_utf8_names: img.stats.non_utf8_names,
        },
        demangle: dm,
        categories,
        category_by_kind: cat_kind,
        category_by_origin: cat_origin,
        rules,
        provenance: Provenance {
            units: img.stats.debug_map_units,
            units_per_component: reg.units_per_component.clone(),
            symbols_with_unit: img.stats.symbols_with_unit,
            symbols_per_origin: origin_counts,
            address_conflicts: img.stats.debug_map_address_conflicts,
            name_conflicts: img.stats.debug_map_name_conflicts,
            external_versions: reg.external_versions.clone(),
            sdk_markers: reg.sdk_markers.iter().cloned().collect(),
            ue3_module_units,
            ue3_module_unit_counts,
        },
        middleware,
        natives: Natives {
            registrant_packages: reg.registrant_packages.iter().cloned().collect(),
            generate_names_packages: reg.generate_names_packages.iter().cloned().collect(),
            natives_table_prefixes: reg.natives_table_prefixes.clone(),
            natives_tables: reg.natives_tables,
            native_classes: reg.native_classes,
            exec_thunks: reg.exec_thunks,
            int_registrations: reg.int_registrations,
            int_registrations_unmatched: reg.int_registrations_unmatched,
            static_initializers: reg.static_initializers,
            fname_globals: reg.fname_globals.clone(),
            modules: reg.modules.clone(),
            decoded: decoded_slim,
            classes_of_interest,
        },
        asamu_symbols: asamu,
        keywords: keywords::search(a),
        physics_functions: keywords::physics_functions(a),
        anchors: anchors::resolve(a),
    }
}

/// Serialize deterministically (pretty JSON + trailing newline).
pub fn to_json(s: &Summary) -> Result<String, serde_json::Error> {
    let mut out = serde_json::to_string_pretty(s)?;
    out.push('\n');
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::macho;

    fn synthetic_summary() -> (Summary, String) {
        let bytes = crate::macho::tests::build_test_macho();
        let image = macho::parse(&bytes).expect("parse");
        let analysis = Analysis::new(image);
        let s = build(&analysis);
        let json = to_json(&s).expect("json");
        (s, json)
    }

    #[test]
    fn json_is_deterministic_and_sorted() {
        let (_, a) = synthetic_summary();
        let (_, b) = synthetic_summary();
        assert_eq!(a, b);
        assert!(a.ends_with('\n'));
        let v: serde_json::Value = serde_json::from_str(&a).expect("valid json");
        // Every category is present (zero counts included) and keys are sorted.
        let cats = v["categories"].as_object().expect("object");
        assert_eq!(cats.len(), Category::ALL.len());
        let keys: Vec<&String> = cats.keys().collect();
        let mut sorted = keys.clone();
        sorted.sort();
        assert_eq!(keys, sorted);
        // Textual order in the file matches sorted order too.
        let asamu = a.find("\"asamu\": 0").expect("asamu key");
        let unknown = a.find("\"unknown\":").expect("unknown key");
        assert!(asamu < unknown);
    }

    #[test]
    fn summary_contains_statistics_not_symbol_lists() {
        let (s, json) = synthetic_summary();
        assert_eq!(s.totals.symbols, 3);
        assert_eq!(s.totals.undefined, 1);
        assert_eq!(s.totals.stab_entries, 4);
        // Ordinary symbol names never appear in the published summary.
        assert!(!json.contains("local_fn"));
        assert!(!json.contains("GlobalFn"));
        assert!(!json.contains("imported"));
        // Build-machine paths from the debug map never appear either.
        assert!(!json.contains("/b/"));
        assert!(json.len() < MAX_SUMMARY_BYTES);
    }

    #[test]
    fn rule_table_covers_every_symbol() {
        let (s, _) = synthetic_summary();
        let matched: u64 = s.rules.iter().map(|r| r.matched).sum();
        assert_eq!(matched, s.totals.symbols);
        let cats: u64 = s.categories.values().sum();
        assert_eq!(cats, s.totals.symbols);
    }
}
