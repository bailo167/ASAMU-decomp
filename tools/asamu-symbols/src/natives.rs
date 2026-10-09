//! Decoding of UE3 native function tables and `IMPLEMENT_FUNCTION` pointers.
//!
//! * `G<pkg><Class>Natives` is an array of `FNativeFunctionLookup`
//!   `{ const ANSICHAR* Name; Native Pointer; }` terminated by a NULL name; the
//!   name string is `<Class>exec<Func>`.
//!   `Native` is a pointer-to-member-function, which on x86_64 Itanium is 16
//!   bytes (`ptr`, `adj`), so each entry is 24 bytes.
//! * `int<Class>exec<Func>` is one `Native` (16 bytes) per `IMPLEMENT_FUNCTION`.
//!
//! A pointer-to-member with an odd `ptr` is virtual (`ptr - 1` is the vtable
//! offset) and is reported as such instead of being resolved.
//!
//! The pointers are resolved back to symbols, which shows which C++ thunk each
//! script native really calls (subclasses that re-declare an inherited native
//! point at the parent's thunk). Every read is bounds-checked against retained
//! section bytes, tables are capped by the next symbol address and by
//! [`MAX_TABLE_ENTRIES`], and any inconsistency ends the table instead of
//! guessing.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use serde::Serialize;

use crate::analysis::Analysis;
use crate::demangle::strip_macho_underscore;
use crate::macho::SymKind;
use crate::registry::{Registry, exec_thunk, split_natives_table};

/// Hard cap on entries decoded from one table.
pub const MAX_TABLE_ENTRIES: u64 = 4096;
const ENTRY_SIZE: u64 = 24;
const MAX_NAME: usize = 256;

/// Result of decoding.
#[derive(Debug, Clone, Default, Serialize, PartialEq, Eq)]
pub struct NativesReport {
    /// Tables decoded.
    pub tables_decoded: u32,
    /// Tables that could not be decoded (bad pointer, no terminator, ...).
    pub tables_failed: u32,
    /// Total entries in decoded tables.
    pub table_entries: u32,
    /// Entries whose pointer resolved to an exec thunk symbol.
    pub entries_resolved: u32,
    /// Entries whose name string (`<Class>exec<Func>`) has the same `exec*`
    /// leaf as the resolved thunk.
    pub entries_name_matches_symbol: u32,
    /// Entries resolving to a thunk of a different (ancestor) class.
    pub entries_inherited: u32,
    /// Entries that did not resolve to a symbol.
    pub entries_unresolved: u32,
    /// Entries per class (decoded tables only). Folded into
    /// `classes_of_interest` by the summary and then omitted from the JSON.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub table_entries_per_class: BTreeMap<String, u32>,
    /// `int…exec…` pointers decoded.
    pub int_decoded: u32,
    /// `int…exec…` pointers resolved to an exec thunk.
    pub int_resolved: u32,
    /// `int…exec…` pointers that are *virtual* pointers-to-member (odd value =
    /// vtable offset + 1), as `Class.execFunc` → vtable offset (hex).
    pub int_virtual: BTreeMap<String, String>,
    /// `int<Class>exec<Func>` → `Owner::exec<Func>` where Owner ≠ Class
    /// (`Class.execFunc` → `Owner`), sorted.
    pub int_inherited: BTreeMap<String, String>,
    /// Table entries (`<Class>exec<Func>`) that also have an `int…` registration.
    pub table_entries_with_int: u32,
    /// Table entries without an `int…` registration.
    pub table_entries_without_int: u32,
    /// `int…` registrations without a table entry.
    pub int_without_table_entry: u32,
    /// [`Self::int_without_table_entry`] split by class (`<Class>` of
    /// `int<Class>exec<Func>`).
    pub int_without_table_entry_by_class: BTreeMap<String, u32>,
    /// How many of [`Self::int_without_table_entry`] are `execPoll*` natives
    /// (UE3 latent-action poll functions, by name).
    pub int_without_table_entry_latent_poll: u32,
    /// [`Self::table_entries_without_int`] split by class.
    pub table_entries_without_int_by_class: BTreeMap<String, u32>,
    /// Exec thunk symbols (`<Class>::exec<Func>`) named by neither an `int…`
    /// registration nor a table entry of their own class.
    pub exec_thunks_unregistered: u32,
}

fn text_index(a: &Analysis) -> HashMap<u64, usize> {
    let mut map: HashMap<u64, usize> = HashMap::new();
    for (i, e) in a.entries.iter().enumerate() {
        let Some(sym) = a.symbol(e) else { continue };
        if sym.kind != SymKind::Text {
            continue;
        }
        let is_thunk = match (e.qualified.as_deref(), e.full.as_deref()) {
            (Some(q), Some(f)) => exec_thunk(q, f).is_some(),
            _ => false,
        };
        match map.get(&sym.value) {
            Some(&prev) => {
                let prev_thunk = a
                    .entries
                    .get(prev)
                    .and_then(|p| match (p.qualified.as_deref(), p.full.as_deref()) {
                        (Some(q), Some(f)) => exec_thunk(q, f),
                        _ => None,
                    })
                    .is_some();
                if is_thunk && !prev_thunk {
                    map.insert(sym.value, i);
                }
            }
            None => {
                map.insert(sym.value, i);
            }
        }
    }
    map
}

/// `<Class>` part of a `<Class>exec<Func>` key (empty if there is no `exec`).
fn class_of_key(key: &str) -> &str {
    key.find("exec").and_then(|p| key.get(..p)).unwrap_or("")
}

/// Resolve a code pointer to (`Owner`, `execLeaf`) if it is an exec thunk.
fn resolve<'a>(a: &'a Analysis, idx: &HashMap<u64, usize>, ptr: u64) -> Option<(&'a str, &'a str)> {
    let i = *idx.get(&ptr)?;
    let e = a.entries.get(i)?;
    exec_thunk(e.qualified.as_deref()?, e.full.as_deref()?)
}

/// Decode all tables and int pointers.
pub fn decode(a: &Analysis, reg: &Registry) -> NativesReport {
    let mut rep = NativesReport::default();
    let idx = text_index(a);

    // Sorted addresses of defined non-text symbols, to bound tables.
    let mut data_addrs: Vec<u64> = a
        .image
        .symbols
        .iter()
        .filter(|s| matches!(s.kind, SymKind::Data | SymKind::Bss))
        .map(|s| s.value)
        .collect();
    data_addrs.sort_unstable();
    data_addrs.dedup();

    let mut packages: Vec<String> = reg
        .registrant_packages
        .iter()
        .chain(reg.generate_names_packages.iter())
        .cloned()
        .collect();
    packages.extend(reg.modules.keys().cloned());
    packages.sort();
    packages.dedup();

    let mut int_keys: BTreeSet<String> = BTreeSet::new();
    let mut table_keys: BTreeSet<String> = BTreeSet::new();
    let mut thunk_keys: BTreeSet<String> = BTreeSet::new();
    for e in &a.entries {
        let Some(sym) = a.symbol(e) else { continue };
        if sym.kind == SymKind::Text
            && let (Some(q), Some(f)) = (e.qualified.as_deref(), e.full.as_deref())
            && let Some((owner, leaf)) = exec_thunk(q, f)
        {
            thunk_keys.insert(format!("{owner}{leaf}"));
        }
        if sym.kind != SymKind::Data || e.full.is_some() {
            continue;
        }
        let name = strip_macho_underscore(&sym.raw);

        if let Some(rest) = name.strip_prefix("int")
            && rest.contains("exec")
            && (rest.starts_with('U') || rest.starts_with('A'))
        {
            let Some(ptr) = a.image.read_u64(sym.value) else {
                continue;
            };
            rep.int_decoded = rep.int_decoded.saturating_add(1);
            int_keys.insert(rest.to_string());
            if ptr & 1 == 1 {
                let (class, leaf) = match rest.find("exec") {
                    Some(p) => (rest.get(..p).unwrap_or(""), rest.get(p..).unwrap_or("")),
                    None => ("", rest),
                };
                rep.int_virtual.insert(
                    format!("{class}.{leaf}"),
                    format!("0x{:x}", ptr.saturating_sub(1)),
                );
            }
            if let Some((owner, leaf)) = resolve(a, &idx, ptr) {
                rep.int_resolved = rep.int_resolved.saturating_add(1);
                let expected = format!("{owner}{leaf}");
                if rest != expected {
                    // rest = <Class><leaf>; recover Class by stripping the leaf.
                    if let Some(class) = rest.strip_suffix(leaf) {
                        rep.int_inherited
                            .insert(format!("{class}.{leaf}"), owner.to_string());
                    }
                }
            }
            continue;
        }

        if !(name.starts_with('G') && name.ends_with("Natives")) {
            continue;
        }
        let Some((_pkg, class)) = split_natives_table(name, &packages) else {
            continue;
        };
        // Upper bound: next data symbol address, capped.
        let pos = data_addrs.partition_point(|&x| x <= sym.value);
        let bound = data_addrs.get(pos).copied().unwrap_or(u64::MAX);
        let max_by_bound = bound.saturating_sub(sym.value) / ENTRY_SIZE;
        let max_entries = max_by_bound.min(MAX_TABLE_ENTRIES);
        let mut count: u32 = 0;
        let mut names: Vec<String> = Vec::new();
        let (mut resolved, mut matched, mut inherited, mut unresolved) = (0u32, 0u32, 0u32, 0u32);
        let mut ok = false;
        for i in 0..max_entries {
            let Some(base) = i
                .checked_mul(ENTRY_SIZE)
                .and_then(|off| sym.value.checked_add(off))
            else {
                break;
            };
            let Some(name_ptr) = a.image.read_u64(base) else {
                break;
            };
            if name_ptr == 0 {
                ok = true;
                break;
            }
            let (Some(fn_ptr), Some(adj)) = (
                base.checked_add(8).and_then(|p| a.image.read_u64(p)),
                base.checked_add(16).and_then(|p| a.image.read_u64(p)),
            ) else {
                break;
            };
            let Some(entry_name) = a.image.read_cstr(name_ptr, MAX_NAME) else {
                break;
            };
            if adj != 0 || !entry_name.contains("exec") {
                break;
            }
            // MAP_NATIVE stores "<Class>exec<Func>".
            let entry_leaf = entry_name
                .strip_prefix(class)
                .unwrap_or(entry_name.as_str());
            count = count.saturating_add(1);
            names.push(entry_name.clone());
            match resolve(a, &idx, fn_ptr) {
                Some((owner, leaf)) => {
                    resolved = resolved.saturating_add(1);
                    if leaf == entry_leaf {
                        matched = matched.saturating_add(1);
                    }
                    if owner != class {
                        inherited = inherited.saturating_add(1);
                    }
                }
                None => unresolved = unresolved.saturating_add(1),
            }
        }
        if ok {
            rep.tables_decoded = rep.tables_decoded.saturating_add(1);
            rep.table_entries = rep.table_entries.saturating_add(count);
            rep.entries_resolved = rep.entries_resolved.saturating_add(resolved);
            rep.entries_name_matches_symbol =
                rep.entries_name_matches_symbol.saturating_add(matched);
            rep.entries_inherited = rep.entries_inherited.saturating_add(inherited);
            rep.entries_unresolved = rep.entries_unresolved.saturating_add(unresolved);
            rep.table_entries_per_class.insert(class.to_string(), count);
            table_keys.extend(names);
        } else {
            rep.tables_failed = rep.tables_failed.saturating_add(1);
        }
    }
    let n = |x: usize| u32::try_from(x).unwrap_or(u32::MAX);
    rep.table_entries_with_int = n(table_keys.intersection(&int_keys).count());
    rep.table_entries_without_int = n(table_keys.difference(&int_keys).count());
    rep.int_without_table_entry = n(int_keys.difference(&table_keys).count());
    for key in int_keys.difference(&table_keys) {
        let c = rep
            .int_without_table_entry_by_class
            .entry(class_of_key(key).to_string())
            .or_insert(0);
        *c = c.saturating_add(1);
        if key.contains("execPoll") {
            rep.int_without_table_entry_latent_poll =
                rep.int_without_table_entry_latent_poll.saturating_add(1);
        }
    }
    for key in table_keys.difference(&int_keys) {
        let c = rep
            .table_entries_without_int_by_class
            .entry(class_of_key(key).to_string())
            .or_insert(0);
        *c = c.saturating_add(1);
    }
    rep.exec_thunks_unregistered = n(thunk_keys
        .iter()
        .filter(|k| !int_keys.contains(*k) && !table_keys.contains(*k))
        .count());
    rep
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn class_of_key_splits_at_first_exec() {
        assert_eq!(class_of_key("UObjectexecBoolToByte"), "UObject");
        assert_eq!(class_of_key("AControllerexecPollMoveTo"), "AController");
        assert_eq!(class_of_key("execFoo"), "");
        assert_eq!(class_of_key("NoMarker"), "");
        assert_eq!(class_of_key(""), "");
    }

    #[test]
    fn decode_on_synthetic_image_reports_nothing() {
        let bytes = crate::macho::tests::build_test_macho();
        let image = crate::macho::parse(&bytes).expect("parse");
        let a = Analysis::new(image);
        let reg = crate::registry::build(&a);
        let rep = decode(&a, &reg);
        assert_eq!(rep, NativesReport::default());
    }
}
