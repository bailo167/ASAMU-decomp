//! asamu-inspect — inspect UE3 v868 packages from a user's own ASAMU install.
//!
//! Every subcommand takes an explicit path. Output is human-readable text, or
//! JSON with `--json`. Nothing is written anywhere except by `decompress` and
//! `scripttext`, which require an explicit `--out` path outside the repository
//! (or under a git-ignored `research/` subdirectory such as `research/local/`).

mod kismet;
mod map;
mod objects;
mod safety;

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use asamu_ue3::{Package, Storage, Summary};
use clap::{Parser, Subcommand};
use serde::Serialize;

#[derive(Parser)]
#[command(
    name = "asamu-inspect",
    version,
    about = "Inspect UE3 (v868) packages from your own A Story About My Uncle install"
)]
struct Cli {
    /// Emit JSON instead of text.
    #[arg(long, global = true)]
    json: bool,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Package summary, compression layout, table counts and cross-check findings.
    Package {
        /// Package file (.u / .upk / .asamu).
        file: PathBuf,
    },
    /// Name table.
    Names {
        /// Package file.
        file: PathBuf,
    },
    /// Import table with resolved paths.
    Imports {
        /// Package file.
        file: PathBuf,
    },
    /// Export table (raw fields plus resolved names).
    Exports {
        /// Package file.
        file: PathBuf,
    },
    /// Exports as objects: full path, class and payload size.
    Objects {
        /// Package file.
        file: PathBuf,
        /// Only objects of this class name.
        #[arg(long)]
        class: Option<String>,
        /// Only objects whose path equals or lies under this dotted path.
        #[arg(long)]
        under: Option<String>,
        /// Only top-level objects (outer = null).
        #[arg(long)]
        top_level: bool,
    },
    /// Metadata-only map summary (class census and gameplay categories).
    Map {
        /// Map package (.asamu).
        file: PathBuf,
    },
    /// One row per package in a directory tree (summary data only by default).
    Census {
        /// Directory to scan recursively for .u / .upk / .asamu.
        dir: PathBuf,
        /// Also fully parse each package (decompress, tables, cross-checks).
        #[arg(long)]
        deep: bool,
    },
    /// Class model: super chain, flags, properties, functions, states, enums,
    /// consts and structs (resolves other packages in the same cooked folder).
    Class {
        /// Package file that contains the class (e.g. Startup.upk, Engine.u).
        file: PathBuf,
        /// Class path (e.g. asamu.ASAMUPawn, Actor) or bare class name.
        class: String,
    },
    /// Class default object values; with --inherited, merged across the super
    /// chain (child overrides parent).
    Defaults {
        /// Package file that contains the class.
        file: PathBuf,
        /// Class path or bare class name.
        class: String,
        /// Resolve inherited defaults across the super chain.
        #[arg(long)]
        inherited: bool,
    },
    /// Any object's prelude and tagged properties.
    Props {
        /// Package file.
        file: PathBuf,
        /// Object path (qualified or package-relative), or `#N` for export N (0-based).
        object: String,
    },
    /// Write a class's ScriptText (original, copyrighted UnrealScript source)
    /// to an explicit LOCAL path outside the repository or under a git-ignored
    /// research/ subdirectory. Never commit the result.
    Scripttext {
        /// Package file that contains the class.
        file: PathBuf,
        /// Class path or bare class name.
        class: String,
        /// Output path.
        #[arg(long)]
        out: PathBuf,
        /// Overwrite an existing output file.
        #[arg(long)]
        force: bool,
    },
    /// Exact-consumption coverage of script objects and class default objects
    /// for every package under a cooked folder (and its Maps/ subfolder).
    Coverage {
        /// Cooked folder (e.g. .../CookedMac).
        dir: PathBuf,
        /// Also decode the prelude and tagged properties of every other export.
        #[arg(long)]
        all_objects: bool,
    },
    /// Kismet graph of a map (or summaries of every map in a cooked/Maps
    /// folder): text summary, full graph with --json (summary with
    /// --summary), Graphviz with --dot. Full graphs are game data: keep them
    /// local (--out/--out-dir refuse the repo except git-ignored research/).
    Kismet {
        /// Map package (.asamu), or a folder of maps.
        path: PathBuf,
        /// Emit Graphviz DOT.
        #[arg(long)]
        dot: bool,
        /// With --json, emit the publishable summary instead of the full graph.
        #[arg(long)]
        summary: bool,
        /// Output file (one map).
        #[arg(long)]
        out: Option<PathBuf>,
        /// Output folder for per-map .kismet.json/.kismet.dot/.kismet-summary.json.
        #[arg(long)]
        out_dir: Option<PathBuf>,
        /// Overwrite existing output files.
        #[arg(long)]
        force: bool,
    },
    /// Write the uncompressed stream to an explicit path (never inside the repo
    /// except under a git-ignored research/ subdirectory such as research/local/).
    Decompress {
        /// Package file.
        file: PathBuf,
        /// Output path.
        #[arg(long)]
        out: PathBuf,
        /// Overwrite an existing output file.
        #[arg(long)]
        force: bool,
    },
}

fn main() {
    let cli = Cli::parse();
    if let Err(e) = run(&cli) {
        eprintln!("asamu-inspect: {e:#}");
        std::process::exit(1);
    }
}

fn run(cli: &Cli) -> Result<()> {
    match &cli.cmd {
        Cmd::Package { file } => cmd_package(file, cli.json),
        Cmd::Names { file } => cmd_names(file, cli.json),
        Cmd::Imports { file } => cmd_imports(file, cli.json),
        Cmd::Exports { file } => cmd_exports(file, cli.json),
        Cmd::Objects {
            file,
            class,
            under,
            top_level,
        } => cmd_objects(
            file,
            class.as_deref(),
            under.as_deref(),
            *top_level,
            cli.json,
        ),
        Cmd::Map { file } => cmd_map(file, cli.json),
        Cmd::Census { dir, deep } => cmd_census(dir, *deep, cli.json),
        Cmd::Decompress { file, out, force } => cmd_decompress(file, out, *force, cli.json),
        Cmd::Class { file, class } => objects::cmd_class(file, class, cli.json),
        Cmd::Defaults {
            file,
            class,
            inherited,
        } => objects::cmd_defaults(file, class, *inherited, cli.json),
        Cmd::Props { file, object } => objects::cmd_props(file, object, cli.json),
        Cmd::Scripttext {
            file,
            class,
            out,
            force,
        } => objects::cmd_scripttext(file, class, out, *force, cli.json),
        Cmd::Coverage { dir, all_objects } => objects::cmd_coverage(dir, *all_objects, cli.json),
        Cmd::Kismet {
            path,
            dot,
            summary,
            out,
            out_dir,
            force,
        } => kismet::cmd_kismet(&kismet::KismetArgs {
            path,
            dot: *dot,
            summary: *summary,
            out: out.as_deref(),
            out_dir: out_dir.as_deref(),
            force: *force,
            json: cli.json,
        }),
    }
}

fn open(file: &Path) -> Result<Package> {
    Package::open(file).with_context(|| format!("parsing {}", file.display()))
}

pub(crate) fn print_json<T: Serialize>(v: &T) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(v)?);
    Ok(())
}

fn file_label(file: &Path) -> String {
    file.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| file.display().to_string())
}

// ---------------------------------------------------------------- package

#[derive(Serialize)]
struct CompressionReport {
    method: String,
    chunk_count: usize,
    block_sizes: BTreeSet<u32>,
    block_count: Option<usize>,
    compressed_bytes: u64,
    uncompressed_bytes: u64,
}

#[derive(Serialize)]
struct TablesReport {
    stream_len: usize,
    names: usize,
    imports: usize,
    exports: usize,
    extents: asamu_ue3::TableExtents,
    depends_parsed: bool,
    top_level_exports: usize,
}

#[derive(Serialize)]
struct PackageReport<'a> {
    file: String,
    file_size: u64,
    summary: &'a Summary,
    package_flag_names: Vec<String>,
    compression: CompressionReport,
    tables: Option<TablesReport>,
    issues: Vec<asamu_ue3::Issue>,
    parse_error: Option<String>,
}

fn cmd_package(file: &Path, json: bool) -> Result<()> {
    let (summary, file_size) =
        Summary::read_from_path(file).with_context(|| format!("reading {}", file.display()))?;
    let full = Package::open(file);
    let (tables, issues, parse_error, block_info) = match &full {
        Ok(p) => {
            let blocks = match &p.storage {
                Storage::Compressed { chunks, .. } => Some((
                    chunks.iter().map(|c| c.block_size).collect::<BTreeSet<_>>(),
                    chunks.iter().map(|c| c.blocks.len()).sum::<usize>(),
                )),
                Storage::Uncompressed => None,
            };
            (
                Some(TablesReport {
                    stream_len: p.stream().len(),
                    names: p.names.len(),
                    imports: p.imports.len(),
                    exports: p.exports.len(),
                    extents: p.extents,
                    depends_parsed: p.depends.is_some(),
                    top_level_exports: p.top_level_exports().count(),
                }),
                p.issues.clone(),
                None,
                blocks,
            )
        }
        Err(e) => (None, Vec::new(), Some(e.to_string()), None),
    };
    let report = PackageReport {
        file: file_label(file),
        file_size,
        package_flag_names: summary.package_flag_names(),
        compression: CompressionReport {
            method: summary.compression().name().to_owned(),
            chunk_count: summary.compressed_chunks.len(),
            block_sizes: block_info.as_ref().map(|b| b.0.clone()).unwrap_or_default(),
            block_count: block_info.as_ref().map(|b| b.1),
            compressed_bytes: summary.total_compressed_chunk_size(),
            uncompressed_bytes: summary.total_uncompressed_chunk_size(),
        },
        summary: &summary,
        tables,
        issues,
        parse_error,
    };
    if json {
        return print_json(&report);
    }
    let s = report.summary;
    println!("file                 {}", report.file);
    println!("file size            {}", report.file_size);
    println!(
        "version              {}/{} (engine {}, cooker {})",
        s.file_version, s.licensee_version, s.engine_version, s.cooker_version
    );
    println!("folder               {:?}", s.folder_name);
    println!(
        "package flags        {:#010x} [{}]",
        s.package_flags,
        report.package_flag_names.join(", ")
    );
    println!("guid                 {}", s.guid);
    println!("total header size    {}", s.total_header_size);
    println!("names                {} @ {}", s.name_count, s.name_offset);
    println!(
        "imports              {} @ {}",
        s.import_count, s.import_offset
    );
    println!(
        "exports              {} @ {}",
        s.export_count, s.export_offset
    );
    println!("depends offset       {}", s.depends_offset);
    println!(
        "import/export guids  @ {} (imports {}, exports {})",
        s.import_export_guids_offset, s.import_guids_count, s.export_guids_count
    );
    println!("thumbnail table      {}", s.thumbnail_table_offset);
    for (i, g) in s.generations.iter().enumerate() {
        println!(
            "generation {i:<9} exports {} names {} net objects {}",
            g.export_count, g.name_count, g.net_object_count
        );
    }
    println!("package source       {:#010x}", s.package_source);
    println!(
        "additional packages  {}",
        if s.additional_packages_to_cook.is_empty() {
            "-".to_owned()
        } else {
            s.additional_packages_to_cook.join(", ")
        }
    );
    println!("texture allocations  {}", s.texture_allocations.len());
    let c = &report.compression;
    println!(
        "compression          {} (flags {:#x}), {} chunks{}",
        c.method,
        s.compression_flags,
        c.chunk_count,
        match c.block_count {
            Some(n) => format!(
                ", {n} blocks, block size {:?}, {} -> {} bytes",
                c.block_sizes, c.compressed_bytes, c.uncompressed_bytes
            ),
            None => String::new(),
        }
    );
    println!("summary size         {} bytes", s.serialized_size);
    if let Some(t) = &report.tables {
        println!("stream length        {}", t.stream_len);
        println!(
            "tables parsed        names {} imports {} exports {} (top-level {}), depends {}",
            t.names,
            t.imports,
            t.exports,
            t.top_level_exports,
            if t.depends_parsed { "yes" } else { "no" }
        );
    }
    if let Some(e) = &report.parse_error {
        println!("parse error          {e}");
    }
    if report.issues.is_empty() {
        println!("issues               none");
    } else {
        for i in &report.issues {
            println!("issue                {i}");
        }
    }
    Ok(())
}

// ---------------------------------------------------------------- names

#[derive(Serialize)]
struct NameRow<'a> {
    index: usize,
    name: &'a str,
    flags: String,
}

fn cmd_names(file: &Path, json: bool) -> Result<()> {
    let p = open(file)?;
    let rows: Vec<NameRow<'_>> = p
        .names
        .iter()
        .enumerate()
        .map(|(index, n)| NameRow {
            index,
            name: &n.name,
            flags: format!("{:#018x}", n.flags),
        })
        .collect();
    if json {
        return print_json(&rows);
    }
    for r in rows {
        println!("{:>6}  {}  {}", r.index, r.flags, r.name);
    }
    Ok(())
}

// ---------------------------------------------------------------- imports

#[derive(Serialize)]
struct ImportRow {
    index: usize,
    package_index: i32,
    class_package: String,
    class_name: String,
    outer: String,
    object_name: String,
    path: String,
}

fn cmd_imports(file: &Path, json: bool) -> Result<()> {
    let p = open(file)?;
    let mut rows = Vec::with_capacity(p.imports.len());
    for (i, imp) in p.imports.iter().enumerate() {
        rows.push(ImportRow {
            index: i,
            package_index: p.import_ref(i)?.0,
            class_package: p.fname(imp.class_package),
            class_name: p.fname(imp.class_name),
            outer: p.object_path(imp.outer_index)?,
            object_name: p.fname(imp.object_name),
            path: p.import_path(i)?,
        });
    }
    if json {
        return print_json(&rows);
    }
    for r in rows {
        println!(
            "{:>6}  {:>7}  {}.{}  {}",
            r.index, r.package_index, r.class_package, r.class_name, r.path
        );
    }
    Ok(())
}

// ---------------------------------------------------------------- exports

#[derive(Serialize)]
struct ExportRow {
    index: usize,
    package_index: i32,
    path: String,
    class: String,
    class_index: i32,
    super_path: String,
    super_index: i32,
    outer_index: i32,
    archetype: String,
    archetype_index: i32,
    object_flags: String,
    serial_offset: i32,
    serial_size: i32,
    export_flags: String,
    generation_net_object_count: Vec<i32>,
    package_guid: String,
    package_flags: String,
}

fn cmd_exports(file: &Path, json: bool) -> Result<()> {
    let p = open(file)?;
    let mut rows = Vec::with_capacity(p.exports.len());
    for (i, e) in p.exports.iter().enumerate() {
        rows.push(ExportRow {
            index: i,
            package_index: p.export_ref(i)?.0,
            path: p.export_path(i)?,
            class: p.export_class_name(i)?,
            class_index: e.class_index.0,
            super_path: p.object_path(e.super_index)?,
            super_index: e.super_index.0,
            outer_index: e.outer_index.0,
            archetype: p.object_path(e.archetype_index)?,
            archetype_index: e.archetype_index.0,
            object_flags: format!("{:#018x}", e.object_flags),
            serial_offset: e.serial_offset,
            serial_size: e.serial_size,
            export_flags: format!("{:#010x}", e.export_flags),
            generation_net_object_count: e.generation_net_object_count.clone(),
            package_guid: e.package_guid.to_string(),
            package_flags: format!("{:#010x}", e.package_flags),
        });
    }
    if json {
        return print_json(&rows);
    }
    for r in rows {
        println!(
            "{:>6}  {:<28} off {:>10} size {:>9}  flags {} ef {}  {}",
            r.index,
            r.class,
            r.serial_offset,
            r.serial_size,
            r.object_flags,
            r.export_flags,
            r.path
        );
    }
    Ok(())
}

// ---------------------------------------------------------------- objects

#[derive(Serialize)]
struct ObjectRow {
    index: usize,
    path: String,
    class: String,
    class_package: String,
    serial_size: i32,
}

fn cmd_objects(
    file: &Path,
    class: Option<&str>,
    under: Option<&str>,
    top_level: bool,
    json: bool,
) -> Result<()> {
    let p = open(file)?;
    let mut rows = Vec::new();
    for (i, e) in p.exports.iter().enumerate() {
        if top_level && !e.outer_index.is_null() {
            continue;
        }
        let cls = p.export_class_name(i)?;
        if class.is_some_and(|c| c != cls) {
            continue;
        }
        let path = p.export_path(i)?;
        if let Some(prefix) = under {
            let inside = path == prefix
                || path
                    .strip_prefix(prefix)
                    .is_some_and(|rest| rest.starts_with('.'));
            if !inside {
                continue;
            }
        }
        rows.push(ObjectRow {
            index: i,
            path,
            class: cls,
            class_package: p
                .export_class_package(i)?
                .unwrap_or_else(|| "<this package>".to_owned()),
            serial_size: e.serial_size,
        });
    }
    if json {
        return print_json(&rows);
    }
    for r in rows {
        println!(
            "{:>6}  {:<32} {:>9}  {}",
            r.index, r.class, r.serial_size, r.path
        );
    }
    Ok(())
}

// ---------------------------------------------------------------- map

fn cmd_map(file: &Path, json: bool) -> Result<()> {
    let p = open(file)?;
    let s = map::summarize(&file_label(file), &p)?;
    if json {
        return print_json(&s);
    }
    println!("file                 {}", s.file);
    println!("ContainsMap flag     {}", s.contains_map_flag);
    println!(
        "names/imports/exports {}/{}/{} ({} distinct export classes)",
        s.name_count, s.import_count, s.export_count, s.distinct_classes
    );
    println!("worlds               {}", list_or_dash(&s.worlds));
    println!("levels               {}", list_or_dash(&s.levels));
    println!("PersistentLevel      {}", s.has_persistent_level);
    println!(
        "additional packages  {}",
        list_or_dash(&s.additional_packages)
    );
    println!("\ncategories (heuristic by class name; may overlap):");
    for (name, cat) in &s.categories {
        println!("  {name:<20} {}", cat.total);
        for (cls, n) in &cat.classes {
            println!("      {n:>6}  {cls}");
        }
    }
    println!("\nnon-stock classes (package not a stock engine package):");
    if s.non_stock_classes.is_empty() {
        println!("  -");
    }
    for r in &s.non_stock_classes {
        println!("  {:>6}  {}.{}", r.count, r.package, r.class);
    }
    println!("\nclass census:");
    for r in &s.classes {
        println!("  {:>6}  {}.{}", r.count, r.package, r.class);
    }
    Ok(())
}

fn list_or_dash(v: &[String]) -> String {
    if v.is_empty() {
        "-".to_owned()
    } else {
        v.join(", ")
    }
}

// ---------------------------------------------------------------- census

#[derive(Serialize)]
struct CensusRow {
    file: String,
    size: u64,
    package_flags: String,
    compression: String,
    chunks: usize,
    names: u32,
    imports: u32,
    exports: u32,
    engine_version: i32,
    cooker_version: i32,
    #[serde(skip_serializing_if = "Option::is_none")]
    deep: Option<DeepRow>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

#[derive(Serialize)]
struct DeepRow {
    ok: bool,
    stream_len: usize,
    warnings: usize,
    errors: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

fn collect_packages(dir: &Path, out: &mut Vec<PathBuf>, depth: usize) -> Result<()> {
    if depth > 16 {
        return Ok(());
    }
    let mut entries: Vec<_> = std::fs::read_dir(dir)
        .with_context(|| format!("reading directory {}", dir.display()))?
        .filter_map(|e| e.ok())
        .collect();
    entries.sort_by_key(|e| e.file_name());
    for e in entries {
        let path = e.path();
        let Ok(ft) = e.file_type() else { continue };
        if ft.is_dir() {
            collect_packages(&path, out, depth + 1)?;
        } else if ft.is_file() {
            let ext = path
                .extension()
                .map(|x| x.to_string_lossy().to_ascii_lowercase())
                .unwrap_or_default();
            if matches!(ext.as_str(), "u" | "upk" | "asamu") {
                out.push(path);
            }
        }
    }
    Ok(())
}

fn cmd_census(dir: &Path, deep: bool, json: bool) -> Result<()> {
    let mut files = Vec::new();
    collect_packages(dir, &mut files, 0)?;
    let mut rows = Vec::with_capacity(files.len());
    for f in &files {
        let rel = f
            .strip_prefix(dir)
            .unwrap_or(f)
            .to_string_lossy()
            .into_owned();
        match Summary::read_from_path(f) {
            Ok((s, size)) => {
                let deep_row = deep.then(|| match Package::open(f) {
                    Ok(p) => DeepRow {
                        ok: true,
                        stream_len: p.stream().len(),
                        warnings: p
                            .issues
                            .iter()
                            .filter(|i| i.severity == asamu_ue3::Severity::Warning)
                            .count(),
                        errors: p
                            .issues
                            .iter()
                            .filter(|i| i.severity == asamu_ue3::Severity::Error)
                            .count(),
                        error: None,
                    },
                    Err(e) => DeepRow {
                        ok: false,
                        stream_len: 0,
                        warnings: 0,
                        errors: 0,
                        error: Some(e.to_string()),
                    },
                });
                rows.push(CensusRow {
                    file: rel,
                    size,
                    package_flags: format!("{:#010x}", s.package_flags),
                    compression: s.compression().name().to_owned(),
                    chunks: s.compressed_chunks.len(),
                    names: s.name_count,
                    imports: s.import_count,
                    exports: s.export_count,
                    engine_version: s.engine_version,
                    cooker_version: s.cooker_version,
                    deep: deep_row,
                    error: None,
                });
            }
            Err(e) => rows.push(CensusRow {
                file: rel,
                size: std::fs::metadata(f).map(|m| m.len()).unwrap_or(0),
                package_flags: String::new(),
                compression: String::new(),
                chunks: 0,
                names: 0,
                imports: 0,
                exports: 0,
                engine_version: 0,
                cooker_version: 0,
                deep: None,
                error: Some(e.to_string()),
            }),
        }
    }
    if json {
        return print_json(&rows);
    }
    println!(
        "{:<44} {:>11} {:>10} {:>5} {:>6} {:>7} {:>7} {:>7}{}",
        "file",
        "size",
        "flags",
        "comp",
        "chunks",
        "names",
        "imports",
        "exports",
        if deep { "  deep" } else { "" }
    );
    for r in &rows {
        if let Some(e) = &r.error {
            println!("{:<44} {:>11}  ERROR {e}", r.file, r.size);
            continue;
        }
        let deep_col = match &r.deep {
            None => String::new(),
            Some(d) if d.ok => format!(
                "  ok stream {} warn {} err {}",
                d.stream_len, d.warnings, d.errors
            ),
            Some(d) => format!("  FAIL {}", d.error.as_deref().unwrap_or("")),
        };
        println!(
            "{:<44} {:>11} {:>10} {:>5} {:>6} {:>7} {:>7} {:>7}{}",
            r.file,
            r.size,
            r.package_flags,
            r.compression,
            r.chunks,
            r.names,
            r.imports,
            r.exports,
            deep_col
        );
    }
    println!("{} packages", rows.len());
    Ok(())
}

// ---------------------------------------------------------------- decompress

#[derive(Serialize)]
struct DecompressReport {
    input: String,
    output: String,
    compressed: bool,
    file_size: u64,
    stream_len: usize,
}

fn cmd_decompress(file: &Path, out: &Path, force: bool, json: bool) -> Result<()> {
    let target = safety::check_output_path(out, file, force)?;
    let p = open(file)?;
    let report = DecompressReport {
        input: file_label(file),
        output: target.display().to_string(),
        compressed: p.is_compressed(),
        file_size: p.file_size,
        stream_len: p.stream().len(),
    };
    safety::write_output(&target, p.stream(), force)?;
    if json {
        return print_json(&report);
    }
    println!(
        "wrote {} bytes ({}) to {}",
        report.stream_len,
        if report.compressed {
            "decompressed"
        } else {
            "already uncompressed; copied"
        },
        report.output
    );
    Ok(())
}
