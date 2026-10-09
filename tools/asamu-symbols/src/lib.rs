//! asamu-symbols: sanitized analysis of the unstripped original Mac executable.
//!
//! The library parses the Mach-O symbol table with the `object` crate (no `nm`
//! subprocess), demangles Itanium names, classifies symbols with ordered,
//! documented rules, attributes them to compilation units through the STABS
//! debug map, recovers the UE3 native registration data and produces
//! **statistics only** (never the raw symbol list) for publication.

pub mod analysis;
pub mod anchors;
pub mod classify;
pub mod demangle;
pub mod keywords;
pub mod locate;
pub mod macho;
pub mod markdown;
pub mod natives;
pub mod provenance;
pub mod registry;
pub mod summary;
