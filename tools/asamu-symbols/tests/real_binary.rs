//! Gated checks against the real executable. They SKIP (pass with a note)
//! when the original game is not installed (CI has no game data).

use asamu_symbols::analysis::Analysis;
use asamu_symbols::{locate, macho, summary};

fn load() -> Option<Analysis> {
    let Some((path, source)) = locate::find_original_binary() else {
        eprintln!(
            "SKIP: original executable not found (set ASAMU_ORIGINAL_DIR to the folder \
             containing 'A Story About My Uncle.app')"
        );
        return None;
    };
    eprintln!("using executable via {}", source.label());
    let data = std::fs::read(&path).expect("read executable");
    let image = macho::parse(&data).expect("parse executable");
    Some(Analysis::new(image))
}

#[test]
fn real_binary_totals_match_nm() {
    let Some(a) = load() else { return };
    let s = summary::build(&a);
    let t = &s.totals;
    // `nm "$B" | wc -l`, `nm -U | wc -l`, `nm -u | wc -l`, `nm -a | wc -l`.
    assert_eq!(t.symbols, 135_093);
    assert_eq!(t.defined, 134_643);
    assert_eq!(t.undefined, 450);
    assert_eq!(t.nlist_entries, 535_281);
    assert_eq!(t.stab_entries, 400_188);
    // `nm "$B" | awk '{print $(NF-1)}' | sort | uniq -c`.
    let census = |k: &str| t.nm_type_census.get(k).copied().unwrap_or(0);
    assert_eq!(census("T"), 62_276);
    assert_eq!(census("t"), 23_631);
    assert_eq!(census("s"), 21_156);
    assert_eq!(census("b"), 11_214);
    assert_eq!(census("S"), 9_458);
    assert_eq!(census("D"), 4_850);
    assert_eq!(census("d"), 2_058);
    assert_eq!(census("U"), 450);
    // Text kind equals T + t.
    assert_eq!(t.kinds.get("text").copied(), Some(62_276 + 23_631));
}

#[test]
fn real_binary_registration_and_keywords() {
    let Some(a) = load() else { return };
    let s = summary::build(&a);
    let n = &s.natives;
    assert!(n.registrant_packages.iter().any(|p| p == "ASAMU"));
    assert!(n.registrant_packages.iter().any(|p| p == "UDKBase"));
    assert!(!n.registrant_packages.iter().any(|p| p == "UTGame"));
    assert_eq!(n.native_classes, 1_535);
    assert_eq!(n.natives_tables, 324);
    assert_eq!(n.decoded.tables_failed, 0);
    assert_eq!(n.decoded.entries_unresolved, 0);
    // Exactly one ASAMU native class, with 13 exec thunks.
    let asamu = n.modules.get("ASAMU").expect("ASAMU module");
    assert_eq!(asamu.classes, 1);
    assert_eq!(asamu.exec_thunks, 13);
    // No grapple symbols anywhere, even as a loose substring.
    let g = s.keywords.get("Grapple").expect("Grapple keyword");
    assert_eq!(g.substring_symbols, 0);
    assert_eq!(
        s.keywords.get("ASAMU").map(|k| k.substring_symbols),
        Some(53)
    );
    // Every curated anchor resolves.
    assert!(
        s.anchors.missing.is_empty(),
        "missing: {:?}",
        s.anchors.missing
    );
    assert_eq!(s.categories.get("unknown").copied(), Some(0));
}

/// Numbers re-derived independently with Apple's `nm`/`nm -ap`/`c++filt` and a
/// separate STABS walk (see SYMBOL_ANALYSIS.md, "Independent verification").
#[test]
fn real_binary_independent_cross_checks() {
    let Some(a) = load() else { return };
    let s = summary::build(&a);
    // `nm -g | wc -l` = 77,034 (`LC_DYSYMTAB.nextdefsym` 76,584 + 450 undefined).
    assert_eq!(s.totals.scopes.get("global").copied(), Some(77_034));
    // `nm -m | grep -c 'private external'`.
    assert_eq!(s.totals.private_extern, 23_027);
    // Debug map: 1,330 N_OSO units; 9 defined symbols carry no N_FUN/N_STSYM/N_GSYM
    // (7 crt/linker symbols + 2 Scaleform function-local statics).
    assert_eq!(s.provenance.units, 1_330);
    assert_eq!(s.provenance.symbols_with_unit, 134_634);
    assert_eq!(
        s.provenance.symbols_per_origin.get("none").copied(),
        Some(9)
    );
    let n = &s.natives;
    // `c++filt` count of `<Class>::exec<Func>(FFrame&, void*)` text symbols.
    assert_eq!(n.exec_thunks, 2_505);
    // `nm | grep -c ' _int[AU].*exec'`.
    assert_eq!(n.int_registrations, 2_510);
    let d = &n.decoded;
    assert_eq!(d.table_entries, 2_468);
    assert_eq!(d.entries_inherited, 106);
    assert_eq!(d.int_virtual.len(), 2);
    assert_eq!(d.table_entries_with_int, 2_368);
    assert_eq!(d.table_entries_without_int, 100);
    assert_eq!(d.int_without_table_entry, 142);
    assert_eq!(d.int_without_table_entry_latent_poll, 8);
    assert_eq!(
        d.int_without_table_entry_by_class.get("UObject").copied(),
        Some(134)
    );
    assert_eq!(d.exec_thunks_unregistered, 1);
    // Per-class exec thunks (c++filt) for a few classes.
    for (class, thunks) in [
        ("AActor", 122),
        ("APawn", 49),
        ("AController", 38),
        ("APlayerController", 42),
        ("UObject", 396),
        ("AUDKPawn", 2),
        ("UASAMUSystemSettingsManager", 13),
    ] {
        let got = n.classes_of_interest.get(class).map(|c| c.info.exec_thunks);
        assert_eq!(got, Some(thunks), "{class}");
    }
    // ASAMU: 53 name matches + 7 debug-map-only symbols.
    assert_eq!(s.categories.get("asamu").copied(), Some(60));
    assert_eq!(s.asamu_symbols.len(), 60);
    // PhysX names compiled into Engine units: SDK header code only (81); the
    // UE3 file-static `NxDumpIndex` is UE3.
    assert_eq!(
        s.category_by_origin
            .get("physx")
            .and_then(|m| m.get("ue3:Engine"))
            .copied(),
        Some(81)
    );
    // Gameplay vocabulary absent even as loose substrings.
    for kw in [
        "Grapple",
        "Tether",
        "Checkpoint",
        "Collectible",
        "Narrative",
    ] {
        let k = s.keywords.get(kw).expect("keyword");
        assert_eq!(k.substring_symbols, 0, "{kw}");
    }
    assert!(
        s.anchors
            .script_events_missing
            .iter()
            .any(|e| e == "ASAMU_*")
    );
}

#[test]
fn committed_summary_is_current() {
    let Some(a) = load() else { return };
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../docs/reverse-engineering/data/symbols/summary.json");
    let Ok(committed) = std::fs::read_to_string(&path) else {
        eprintln!("SKIP: no committed summary at {}", path.display());
        return;
    };
    let fresh = summary::to_json(&summary::build(&a)).expect("json");
    assert!(fresh.len() < summary::MAX_SUMMARY_BYTES);
    assert!(
        fresh == committed,
        "summary.json is stale; regenerate with: cargo run -p asamu-symbols -- --write-summary docs/reverse-engineering/data/symbols/summary.json"
    );
}
