//! Markdown rendering of a [`Summary`] (tables only; same sanitized content as
//! the JSON).

use std::fmt::Write as _;

use crate::summary::Summary;

fn esc(s: &str) -> String {
    s.replace('|', "\\|")
}

fn pct(n: u64, total: u64) -> String {
    if total == 0 {
        return "0.0%".into();
    }
    // Integer arithmetic to stay deterministic: tenths of a percent.
    let tenths = n.saturating_mul(1000) / total;
    format!("{}.{}%", tenths / 10, tenths % 10)
}

/// Render the summary as Markdown.
pub fn render(s: &Summary) -> String {
    let mut o = String::new();
    let t = &s.totals;
    let _ = writeln!(o, "## Symbol table totals\n");
    let _ = writeln!(o, "| Measure | Count |\n|---|---:|");
    let _ = writeln!(
        o,
        "| `nlist` entries (`LC_SYMTAB.nsyms`) | {} |",
        t.nlist_entries
    );
    let _ = writeln!(
        o,
        "| STABS debug-map entries (hidden by plain `nm`) | {} |",
        t.stab_entries
    );
    let _ = writeln!(o, "| Regular symbols (plain `nm`) | {} |", t.symbols);
    let _ = writeln!(o, "| Defined | {} |", t.defined);
    let _ = writeln!(o, "| Undefined | {} |", t.undefined);
    let _ = writeln!(o, "| Private extern (`N_PEXT`) | {} |", t.private_extern);
    let _ = writeln!(o);
    let _ = writeln!(o, "| `nm` type | Count |\n|---|---:|");
    let mut census: Vec<(&String, &u64)> = t.nm_type_census.iter().collect();
    census.sort_by(|a, b| b.1.cmp(a.1).then(a.0.cmp(b.0)));
    for (k, v) in census {
        let _ = writeln!(o, "| `{k}` | {v} |");
    }
    let _ = writeln!(o);
    let _ = writeln!(o, "| Kind / scope | Count |\n|---|---:|");
    for (k, v) in &t.kind_scope {
        let _ = writeln!(o, "| {k} | {v} |");
    }
    let _ = writeln!(o);
    let _ = writeln!(o, "| STABS type | Count |\n|---|---:|");
    for (k, v) in &t.stab_by_type {
        let _ = writeln!(o, "| `N_{k}` | {v} |");
    }

    let d = &s.demangle;
    let _ = writeln!(o, "\n## Demangling\n");
    let _ = writeln!(o, "| Measure | Count |\n|---|---:|");
    let _ = writeln!(
        o,
        "| Raw names with Mach-O leading `_` | {} |",
        d.leading_underscore
    );
    let _ = writeln!(o, "| Raw names without it | {} |", d.no_leading_underscore);
    let _ = writeln!(
        o,
        "| Itanium candidates (`__Z…`) | {} |",
        d.itanium_candidates
    );
    let _ = writeln!(o, "| Demangled directly | {} |", d.ok);
    let _ = writeln!(
        o,
        "| Demangled after stripping a GCC `.suffix` | {} |",
        d.ok_suffix_stripped
    );
    let _ = writeln!(o, "| Failed | {} |", d.failed);
    let _ = writeln!(o, "| Not mangled (C / labels) | {} |", d.not_mangled);

    let _ = writeln!(o, "\n## Categories\n");
    let _ = writeln!(o, "| Category | Symbols | Share |\n|---|---:|---:|");
    for (k, v) in &s.categories {
        let _ = writeln!(o, "| {k} | {v} | {} |", pct(*v, t.symbols));
    }

    let _ = writeln!(o, "\n## Classification rules (ordered, first match wins)\n");
    let _ = writeln!(
        o,
        "| # | Rule | Tier | Category | Matched | Description |\n|---:|---|---|---|---:|---|"
    );
    for r in &s.rules {
        let _ = writeln!(
            o,
            "| {} | `{}` | {} | {} | {} | {} |",
            r.order,
            r.id,
            r.tier,
            r.category,
            r.matched,
            esc(&r.description)
        );
    }

    let p = &s.provenance;
    let _ = writeln!(o, "\n## Provenance (STABS debug map)\n");
    let _ = writeln!(
        o,
        "{} compilation units; {} of {} symbols attributed; {} address / {} name conflicts.\n",
        p.units, p.symbols_with_unit, t.symbols, p.address_conflicts, p.name_conflicts
    );
    let _ = writeln!(o, "| Component | Units |\n|---|---:|");
    for (k, v) in &p.units_per_component {
        let _ = writeln!(o, "| `{k}` | {v} |");
    }
    if !p.external_versions.is_empty() {
        let _ = writeln!(
            o,
            "\nExternal library versions (from directory names): {}",
            {
                let v: Vec<String> = p
                    .external_versions
                    .iter()
                    .map(|(k, v)| format!("{k} {v}"))
                    .collect();
                v.join(", ")
            }
        );
    }
    if !p.sdk_markers.is_empty() {
        let _ = writeln!(o, "\nSDK marker directories: {}", p.sdk_markers.join(", "));
    }

    let _ = writeln!(o, "\n## Middleware\n");
    let _ = writeln!(
        o,
        "| Library | Library units | Dylib imports | Name hits | Verdict |\n|---|---:|---:|---:|---|"
    );
    for m in &s.middleware {
        let _ = writeln!(
            o,
            "| {} | {} | {} | {} | {} |",
            m.library, m.library_units, m.dylib_imports, m.integration_name_hits, m.verdict
        );
    }

    let n = &s.natives;
    let _ = writeln!(o, "\n## Native registration\n");
    let _ = writeln!(
        o,
        "- `AutoInitializeRegistrants<Pkg>`: {}",
        n.registrant_packages.join(", ")
    );
    let _ = writeln!(
        o,
        "- `AutoGenerateNames<Pkg>`: {}",
        n.generate_names_packages.join(", ")
    );
    let _ = writeln!(
        o,
        "- Native classes (`PrivateStaticClass`): {}; exec thunks: {}; `int…exec…` registrations: {} ({} unmatched); `G…Natives` tables: {}; static initialisers: {}",
        n.native_classes,
        n.exec_thunks,
        n.int_registrations,
        n.int_registrations_unmatched,
        n.natives_tables,
        n.static_initializers
    );
    let dd = &n.decoded;
    let _ = writeln!(
        o,
        "- Decoded tables: {} ok / {} failed, {} entries ({} resolved to exec thunks, {} name/symbol matches, {} inherited, {} unresolved); `int` pointers: {} decoded, {} resolved, {} virtual, {} point at an ancestor's thunk",
        dd.tables_decoded,
        dd.tables_failed,
        dd.table_entries,
        dd.entries_resolved,
        dd.entries_name_matches_symbol,
        dd.entries_inherited,
        dd.entries_unresolved,
        dd.int_decoded,
        dd.int_resolved,
        dd.int_virtual.len(),
        dd.int_inherited.len()
    );
    let _ = writeln!(
        o,
        "- Overlap: {} table entries also have an `int` registration, {} do not; {} `int` registrations have no table entry; {} exec thunks are named by neither",
        dd.table_entries_with_int,
        dd.table_entries_without_int,
        dd.int_without_table_entry,
        dd.exec_thunks_unregistered
    );
    let by_class = |m: &std::collections::BTreeMap<String, u32>| -> String {
        let mut v: Vec<(&String, &u32)> = m.iter().collect();
        v.sort_by(|a, b| b.1.cmp(a.1).then(a.0.cmp(b.0)));
        let parts: Vec<String> = v.iter().map(|(k, n)| format!("{k} {n}")).collect();
        parts.join(", ")
    };
    let _ = writeln!(
        o,
        "- `int` registrations without a table entry by class: {} ({} are `execPoll*` latent natives); table entries without an `int` registration by class: {}",
        by_class(&dd.int_without_table_entry_by_class),
        dd.int_without_table_entry_latent_poll,
        by_class(&dd.table_entries_without_int_by_class)
    );
    let _ = writeln!(
        o,
        "\n| Module | Native classes | With exec thunks | Exec thunks |\n|---|---:|---:|---:|"
    );
    for (k, m) in &n.modules {
        let _ = writeln!(
            o,
            "| {k} | {} | {} | {} |",
            m.classes, m.classes_with_natives, m.exec_thunks
        );
    }
    let _ = writeln!(o, "\n| FName prefix | Globals |\n|---|---:|");
    for (k, v) in &n.fname_globals {
        let _ = writeln!(o, "| `{k}` | {v} |");
    }
    let _ = writeln!(
        o,
        "\n| Class of interest | Module | Exec thunks | `int` regs | Table entries | Table pkg |\n|---|---|---:|---:|---:|---|"
    );
    for (k, c) in &n.classes_of_interest {
        let _ = writeln!(
            o,
            "| {k} | {} | {} | {} | {} | {} |",
            c.info.module.as_deref().unwrap_or("?"),
            c.info.exec_thunks,
            c.info.int_registrations,
            c.natives_table_entries
                .map(|v| v.to_string())
                .unwrap_or_else(|| "–".into()),
            c.info.natives_table_pkg.as_deref().unwrap_or("–")
        );
    }

    let _ = writeln!(o, "\n## ASAMU-category symbols (complete)\n");
    let _ = writeln!(
        o,
        "| Address | nm | Name | Rule | Unit |\n|---|---|---|---|---|"
    );
    for a in &s.asamu_symbols {
        let _ = writeln!(
            o,
            "| `{}` | {} | `{}` | {} | {} |",
            a.address,
            a.nm_type,
            esc(&a.name),
            a.rule,
            a.unit.as_deref().unwrap_or("")
        );
    }

    let _ = writeln!(o, "\n## Keywords\n");
    let _ = writeln!(
        o,
        "| Keyword | Mode | Symbols | Substring | Game-layer names | Most relevant (first 6) |\n|---|---|---:|---:|---:|---|"
    );
    for (k, r) in &s.keywords {
        let top: Vec<String> = r
            .top_names
            .iter()
            .take(6)
            .map(|n| format!("`{}`", esc(n)))
            .collect();
        let _ = writeln!(
            o,
            "| {k}{} | {} | {} | {} | {} | {} |",
            if r.requested { "" } else { " ⁺" },
            r.mode,
            r.symbols,
            r.substring_symbols,
            r.game_layer_distinct_names,
            top.join(", ")
        );
    }

    let _ = writeln!(o, "\n## Movement-physics functions\n");
    let _ = writeln!(o, "| Function | Address | Unit |\n|---|---|---|");
    for f in &s.physics_functions {
        let _ = writeln!(
            o,
            "| `{}` | `{}` | {} |",
            esc(&f.name),
            f.address,
            f.unit.as_deref().unwrap_or("")
        );
    }

    let an = &s.anchors;
    let _ = writeln!(o, "\n## Ghidra anchors\n");
    let _ = writeln!(
        o,
        "| Topic | Symbol | Address | Unit | Why |\n|---|---|---|---|---|"
    );
    for r in &an.resolved {
        let _ = writeln!(
            o,
            "| {} | `{}` | `{}` | {} | {} |",
            r.topic,
            esc(&r.symbol),
            r.address,
            r.unit.as_deref().unwrap_or(""),
            esc(&r.why)
        );
    }
    if !an.missing.is_empty() {
        let _ = writeln!(o, "\nNot present: {}", an.missing.join(", "));
    }
    let _ = writeln!(
        o,
        "\nScript events present: {}",
        an.script_events_present.join(", ")
    );
    if !an.script_events_missing.is_empty() {
        let _ = writeln!(
            o,
            "\nScript events absent: {}",
            an.script_events_missing.join(", ")
        );
    }
    o
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percent_is_integer_based() {
        assert_eq!(pct(1, 3), "33.3%");
        assert_eq!(pct(0, 0), "0.0%");
        assert_eq!(pct(5, 5), "100.0%");
    }

    #[test]
    fn escaping() {
        assert_eq!(esc("a|b<c>"), "a\\|b<c>");
    }
}
