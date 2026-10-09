//! Object-level subcommands: `class`, `defaults`, `props`, `scripttext` and
//! `coverage`.
//!
//! Output is printed locally from the user's own install. Names, types,
//! flags and default values are fine to print; `scripttext` writes the
//! shipped UnrealScript source to an explicit local file and refuses any
//! destination inside the repository except git-ignored `research/`
//! subdirectories (see `safety.rs`).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use asamu_ue3::coverage::{ObjectStats, PackageCoverage, package_coverage};
use asamu_ue3::model::{FunctionInfo, LoadedPackage, PackageSet};
use asamu_ue3::script::{self, ScriptBody, ScriptKind};
use asamu_ue3::{Package, Property, flags};
use serde::Serialize;

use crate::{print_json, safety};

fn open_set(file: &Path) -> Result<(PackageSet, std::sync::Arc<LoadedPackage>)> {
    PackageSet::for_file(file).with_context(|| format!("opening {}", file.display()))
}

/// Resolve a user-supplied object path to a qualified path in `lp`.
///
/// `#N` addresses export `N` (0-based, as printed in `exports` and in the
/// `export_index` JSON fields). Use it for the few objects whose qualified
/// path is not unique (same name and outer, different class; e.g. a material
/// and a texture in some maps), which a path lookup resolves to the first.
fn resolve(lp: &LoadedPackage, path: &str) -> Result<(usize, String)> {
    if let Some(n) = path.strip_prefix('#') {
        let i: usize = n
            .parse()
            .with_context(|| format!("bad export index {path:?}"))?;
        if i >= lp.package.exports.len() {
            bail!(
                "export index {i} out of range ({} exports in {})",
                lp.package.exports.len(),
                lp.name
            );
        }
        return Ok((i, lp.qualified(i)?));
    }
    let i = lp
        .find(path)
        .with_context(|| format!("no object {path:?} in {}", lp.name))?;
    Ok((i, lp.qualified(i)?))
}

// ---------------------------------------------------------------- class

fn signature(f: &FunctionInfo) -> String {
    let params: Vec<String> = f
        .params
        .iter()
        .map(|p| {
            let mut s = String::new();
            if p.optional {
                s.push_str("optional ");
            }
            if p.out {
                s.push_str("out ");
            }
            if p.coerce {
                s.push_str("coerce ");
            }
            s.push_str(&p.type_desc);
            s.push(' ');
            s.push_str(&p.name);
            if p.array_dim > 1 {
                s.push_str(&format!("[{}]", p.array_dim));
            }
            s
        })
        .collect();
    let native = if f.flags & flags::function::NATIVE != 0 {
        if f.native_index != 0 {
            format!("native({}) ", f.native_index)
        } else {
            "native ".to_owned()
        }
    } else {
        String::new()
    };
    format!(
        "{native}{}{}({})  [{}]{}",
        f.return_type
            .as_deref()
            .map(|t| format!("{t} "))
            .unwrap_or_default(),
        f.name,
        params.join(", "),
        f.flag_names.join(","),
        if f.bytecode_storage > 0 {
            format!(" bytecode {} B", f.bytecode_storage)
        } else {
            String::new()
        }
    )
}

pub fn cmd_class(file: &Path, class_path: &str, json: bool) -> Result<()> {
    let (set, lp) = open_set(file)?;
    let (_, q) = resolve(&lp, class_path)?;
    let m = set
        .class_model(&q)
        .with_context(|| format!("building class model for {q}"))?;
    if json {
        return print_json(&m);
    }
    println!(
        "class        {}  ({} export {})",
        m.path, m.package_file, m.export_index
    );
    println!(
        "super chain  {}",
        if m.super_chain.is_empty() {
            "-".to_owned()
        } else {
            m.super_chain.join(" -> ")
        }
    );
    println!(
        "flags        {:#010x} [{}]",
        m.class_flags,
        m.class_flag_names.join(", ")
    );
    println!("within       {}", m.within.as_deref().unwrap_or("-"));
    println!("config       {}", m.config_name);
    if !m.native_header.is_empty() {
        println!("native hdr   {}", m.native_header);
    }
    for (label, v) in [
        ("hidecats", &m.hide_categories),
        ("dontsort", &m.dont_sort_categories),
        ("autoexpand", &m.auto_expand_categories),
        ("autocollapse", &m.auto_collapse_categories),
        ("classgroup", &m.class_groups),
    ] {
        if !v.is_empty() {
            println!("{label:<12} {}", v.join(", "));
        }
    }
    println!(
        "default obj  {}",
        m.default_object.as_deref().unwrap_or("-")
    );
    println!("script text  {}", m.script_text.as_deref().unwrap_or("-"));
    if m.replication_bytecode > 0 {
        println!(
            "class code   {} B (replication block)",
            m.replication_bytecode
        );
    }
    for i in &m.interfaces {
        println!("implements   {}", i.class);
    }
    for c in &m.components {
        println!("component    {} = {}", c.name, c.template);
    }
    println!("\nproperties ({}):", m.properties.len());
    for p in &m.properties {
        println!(
            "  {:<28} {:<28}{} [{}]{}{}",
            p.type_desc,
            p.name,
            if p.array_dim > 1 {
                format!("[{}]", p.array_dim)
            } else {
                String::new()
            },
            p.flag_names.join(","),
            if p.category != "None" {
                format!(" category {}", p.category)
            } else {
                String::new()
            },
            p.rep_offset
                .map(|r| format!(" rep {r}"))
                .unwrap_or_default()
        );
    }
    println!("\nfunctions ({}):", m.functions.len());
    for f in &m.functions {
        println!("  {}", signature(f));
    }
    println!("\nstates ({}):", m.states.len());
    for s in &m.states {
        println!(
            "  {}{} [{}] functions {}{}",
            s.name,
            s.super_state
                .as_deref()
                .map(|p| format!(" extends {p}"))
                .unwrap_or_default(),
            s.flag_names.join(","),
            s.functions.len(),
            if s.bytecode_storage > 0 {
                format!(", state code {} B", s.bytecode_storage)
            } else {
                String::new()
            }
        );
        for f in &s.functions {
            println!("      {}", signature(f));
        }
    }
    if !m.enums.is_empty() {
        println!("\nenums ({}):", m.enums.len());
        for e in &m.enums {
            println!("  {} {{{}}}", e.name, e.values.join(", "));
        }
    }
    if !m.consts.is_empty() {
        println!("\nconsts ({}):", m.consts.len());
        for c in &m.consts {
            println!("  {} = {}", c.name, c.value);
        }
    }
    if !m.structs.is_empty() {
        println!("\nstructs ({}):", m.structs.len());
        for s in &m.structs {
            println!(
                "  {}{} [{}] members {}",
                s.name,
                s.super_struct
                    .as_deref()
                    .map(|p| format!(" extends {p}"))
                    .unwrap_or_default(),
                s.flag_names.join(","),
                s.properties.len()
            );
            for p in &s.properties {
                println!("      {:<24} {}", p.type_desc, p.name);
            }
            for d in &s.defaults {
                println!("      default {} = {}", label(d), d.value.render());
            }
        }
    }
    Ok(())
}

fn label(p: &Property) -> String {
    if p.array_index > 0 {
        format!("{}[{}]", p.name, p.array_index)
    } else {
        p.name.clone()
    }
}

// ---------------------------------------------------------------- defaults

pub fn cmd_defaults(file: &Path, class_path: &str, inherited: bool, json: bool) -> Result<()> {
    let (set, lp) = open_set(file)?;
    let (_, q) = resolve(&lp, class_path)?;
    if inherited {
        let d = set
            .inherited_defaults(&q)
            .with_context(|| format!("resolving defaults of {q}"))?;
        if json {
            return print_json(&d);
        }
        println!(
            "defaults of {} merged across {} classes",
            d.class,
            d.sources.len()
        );
        for s in &d.sources {
            println!(
                "  source {:<40} {:<48} {} props{}",
                s.class,
                s.default_object,
                s.properties,
                if s.native_tail > 0 {
                    format!(", native tail {} B", s.native_tail)
                } else {
                    String::new()
                }
            );
        }
        for v in &d.values {
            let name = if v.array_index > 0 {
                format!("{}[{}]", v.name, v.array_index)
            } else {
                v.name.clone()
            };
            println!(
                "  {:<36} = {:<40} ({}, from {})",
                name,
                v.value.render(),
                v.type_name,
                v.source
            );
        }
        for w in &d.warnings {
            println!("  warning: {w}");
        }
        return Ok(());
    }
    let obj = set
        .class_defaults(&q)
        .with_context(|| format!("decoding default object of {q}"))?;
    if json {
        return print_json(&obj);
    }
    println!(
        "default object {} ({} tagged properties, native tail {} B)",
        obj.path,
        obj.properties.len(),
        obj.native_tail()
    );
    for p in &obj.properties {
        println!(
            "  {:<36} = {:<40} ({})",
            label(p),
            p.value.render(),
            p.type_name
        );
    }
    for w in &obj.warnings {
        println!("  warning: {w}");
    }
    Ok(())
}

// ---------------------------------------------------------------- props

pub fn cmd_props(file: &Path, object_path: &str, json: bool) -> Result<()> {
    let (set, lp) = open_set(file)?;
    let (i, _) = resolve(&lp, object_path)?;
    let obj = set
        .decode(&lp, i)
        .with_context(|| format!("decoding {object_path}"))?;
    if json {
        return print_json(&obj);
    }
    println!("object       {} (export {})", obj.path, obj.export_index);
    println!("class        {}", obj.class);
    println!(
        "payload      {} B, tagged properties end at {}, native tail {} B",
        obj.payload_size,
        obj.properties_end,
        obj.native_tail()
    );
    println!("net index    {}", obj.prelude.net_index);
    if let Some(sf) = &obj.prelude.state_frame {
        println!(
            "state frame  node {} state {} probe {:#010x} latent {:#06x} code offset {:?}",
            sf.node, sf.state_node, sf.probe_mask, sf.latent_action, sf.code_offset
        );
    }
    if let Some(c) = &obj.prelude.component {
        println!(
            "component    owner class {} template name {}",
            c.owner_class,
            c.template_name.as_deref().unwrap_or("-")
        );
    }
    if let Some(n) = obj.prelude.shadow_map_len {
        println!("shadow map   {n} u16 entries");
    }
    for p in &obj.properties {
        println!(
            "  {:<36} = {:<40} ({})",
            label(p),
            p.value.render(),
            p.type_name
        );
    }
    for w in &obj.warnings {
        println!("  warning: {w}");
    }
    Ok(())
}

// ---------------------------------------------------------------- scripttext

#[derive(Serialize)]
struct ScriptTextReport {
    class: String,
    text_buffer: String,
    output: String,
    chars: usize,
}

pub fn cmd_scripttext(
    file: &Path,
    class_path: &str,
    out: &Path,
    force: bool,
    json: bool,
) -> Result<()> {
    let target = safety::check_output_path(out, file, force)?;
    let (set, lp) = open_set(file)?;
    let (_, q) = resolve(&lp, class_path)?;
    let (clp, obj) = set.script_object(&q)?;
    let ScriptBody::Class { structure, .. } = &obj.body else {
        bail!("{q} is a {}, not a class", obj.kind.name());
    };
    let Some(tb) = structure.script_text.export_index() else {
        bail!("{q} has no ScriptText buffer");
    };
    let text = script::text_buffer_text(&clp.package, tb)?;
    eprintln!(
        "WARNING: this is the original, copyrighted UnrealScript source shipped with the game. \
         Read it locally only; never commit, quote or paraphrase it into the repository."
    );
    safety::write_output(&target, text.as_bytes(), force)?;
    let report = ScriptTextReport {
        class: q,
        text_buffer: clp.qualified(tb)?,
        output: target.display().to_string(),
        chars: text.chars().count(),
    };
    if json {
        return print_json(&report);
    }
    println!(
        "wrote {} characters of {} to {}",
        report.chars, report.text_buffer, report.output
    );
    Ok(())
}

// ---------------------------------------------------------------- coverage

#[derive(Serialize)]
struct CoverageRow {
    file: String,
    #[serde(flatten)]
    coverage: Option<PackageCoverage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

fn collect(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for d in [dir.to_path_buf(), dir.join("Maps")] {
        let Ok(rd) = std::fs::read_dir(&d) else {
            continue;
        };
        let mut files: Vec<PathBuf> = rd
            .flatten()
            .map(|e| e.path())
            .filter(|p| {
                p.is_file()
                    && p.extension()
                        .map(|x| x.to_string_lossy().to_ascii_lowercase())
                        .is_some_and(|x| matches!(x.as_str(), "u" | "upk" | "asamu"))
            })
            .collect();
        files.sort();
        out.extend(files);
    }
    out
}

fn is_script_package(p: &Path) -> bool {
    let name = p
        .file_name()
        .map(|n| n.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    name.ends_with(".u") || name == "startup.upk"
}

pub fn cmd_coverage(dir: &Path, all_objects: bool, json: bool) -> Result<()> {
    let files = collect(dir);
    if files.is_empty() {
        bail!("no packages under {}", dir.display());
    }
    let set = PackageSet::new(&[dir.to_path_buf(), dir.join("Maps")]);
    let mut rows = Vec::new();
    for f in &files {
        let label = f
            .strip_prefix(dir)
            .unwrap_or(f)
            .to_string_lossy()
            .into_owned();
        // Script packages stay cached in the set (they are the schema); other
        // packages are decoded standalone and dropped.
        let res = if is_script_package(f) {
            set.open_file(f)
                .map(|lp| package_coverage(&set, &lp, all_objects))
                .map_err(|e| e.to_string())
        } else {
            Package::open(f)
                .map(|p| {
                    let name = f
                        .file_stem()
                        .map(|s| s.to_string_lossy().into_owned())
                        .unwrap_or_default();
                    package_coverage(&set, &LoadedPackage::new(name, f, p), all_objects)
                })
                .map_err(|e| e.to_string())
        };
        rows.push(match res {
            Ok(c) => CoverageRow {
                file: label,
                coverage: Some(c),
                error: None,
            },
            Err(e) => CoverageRow {
                file: label,
                coverage: None,
                error: Some(e),
            },
        });
    }
    if json {
        return print_json(&rows);
    }
    let mut kinds: BTreeMap<ScriptKind, (usize, usize)> = BTreeMap::new();
    let mut cdo = ObjectStats::default();
    let mut objects = ObjectStats::default();
    for r in &rows {
        let Some(c) = &r.coverage else {
            println!("{:<40} ERROR {}", r.file, r.error.as_deref().unwrap_or(""));
            continue;
        };
        let st: usize = c.script.values().map(|k| k.total).sum();
        let se: usize = c.script.values().map(|k| k.exact).sum();
        print!(
            "{:<40} script {:>6}/{:<6} cdo {:>4}/{:<4}",
            r.file, se, st, c.cdo.exact, c.cdo.total
        );
        if let Some(o) = &c.objects {
            print!(
                "  objects {:>6}/{:<6} decoded, {:>6} exact",
                o.decoded, o.total, o.exact
            );
            objects.absorb(o);
        }
        println!();
        for (k, v) in &c.script {
            let e = kinds.entry(*k).or_default();
            e.0 += v.exact;
            e.1 += v.total;
            for f in &v.failures {
                println!("    {} failure {f}", k.name());
            }
        }
        cdo.absorb(&c.cdo);
    }
    println!("\nscript objects per kind (exact / total):");
    for (k, (e, t)) in &kinds {
        println!("  {:<20} {e:>6} / {t:<6}", k.name());
    }
    let (te, tt) = kinds.values().fold((0, 0), |a, v| (a.0 + v.0, a.1 + v.1));
    println!("  {:<20} {te:>6} / {tt:<6}", "TOTAL");
    print_stats("class default objects", &cdo);
    if all_objects {
        print_stats("other objects", &objects);
    }
    Ok(())
}

fn print_stats(label: &str, s: &ObjectStats) {
    println!(
        "\n{label}: {} total, {} decoded, {} consume exactly, {} with native tail, \
         {} with raw values, {} with warnings, {} tag-order violations, {} undeclared tags, \
         {} of an unresolved class",
        s.total,
        s.decoded,
        s.exact,
        s.native_tail,
        s.with_raw_values,
        s.with_warnings,
        s.order_violations,
        s.undeclared_tags,
        s.unknown_class
    );
    if !s.unknown_classes.is_empty() {
        println!("  unresolved classes: {}", s.unknown_classes.join(", "));
    }
    println!(
        "  tag types: {}",
        s.tag_types
            .iter()
            .map(|(k, v)| format!("{k} {v}"))
            .collect::<Vec<_>>()
            .join(", ")
    );
    for f in &s.failures {
        println!("  failure {f}");
    }
    for w in &s.warning_samples {
        println!("  warning {w}");
    }
}
