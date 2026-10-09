//! Synthetic checks of the public flag-naming API (no game data needed).

use asamu_ue3::flags::{self, export, name, object, package_source};
use asamu_ue3::package_flags;

#[test]
fn every_named_package_flag_is_a_distinct_single_bit() {
    let mut seen = 0u32;
    for &(bit, label) in package_flags::NAMES {
        assert_eq!(bit.count_ones(), 1, "{label}");
        assert_eq!(seen & bit, 0, "{label} named twice");
        seen |= bit;
    }
    // The nine bits that occur in shipped summaries all have names.
    assert_eq!(0x22AA_000D & !seen, 0);
    assert_eq!(
        package_flags::describe(package_flags::SERVER_SIDE_ONLY | 0x8000_0000),
        vec!["ServerSideOnly", "0x80000000"]
    );
}

#[test]
fn object_export_and_name_tables_have_unique_single_bits() {
    for names in [object::NAMES, export::NAMES, name::NAMES] {
        let mut seen = 0u64;
        for &(bit, label) in names {
            assert_eq!(bit.count_ones(), 1, "{label}");
            assert_eq!(seen & bit, 0, "{label} named twice");
            seen |= bit;
        }
    }
    // The save mark of name entries decomposes into named bits only.
    assert_eq!(
        flags::describe(name::SAVED, name::NAMES),
        vec!["TagExp", "LoadForClient", "LoadForServer", "LoadForEdit"]
    );
}

#[test]
fn load_context_rule() {
    // A typical placed actor (client + server + edit) is created in game; an
    // editor-only helper (edit only) is not.
    let placed = object::LOAD_FOR_CLIENT | object::LOAD_FOR_SERVER | object::LOAD_FOR_EDIT;
    assert!(object::loaded_in_game(placed));
    assert!(!object::loaded_in_game(object::LOAD_FOR_EDIT));
    assert!(!object::loaded_in_game(0));
    assert_eq!(placed & !object::LOAD_KEEP_MASK, 0);
}

#[test]
fn package_source_crc_properties() {
    use package_source::{crc32, package_source_crc};
    // Each character is fed as a little-endian 16-bit unit of its upper-case
    // form, so the result equals the plain CRC over those bytes.
    let units: Vec<u8> = "AB-1_X".bytes().flat_map(|b| [b, 0]).collect();
    assert_eq!(package_source_crc("ab-1_x"), Some(crc32(units)));
    assert_eq!(package_source_crc("\u{442}"), None);
    // Different names give different checksums (no accidental constant).
    let a = package_source_crc("Startup");
    let b = package_source_crc("Startup_LOC_INT");
    assert!(a.is_some() && b.is_some() && a != b);
}
