//! `kismet` subcommand: Kismet (sequence) graphs of map packages.
//!
//! A full graph (object names, editor comments, actor references, values) is
//! original game data: it is printed locally or written to an explicit path
//! that `safety.rs` accepts (outside the repository, or under a git-ignored
//! `research/` subdirectory such as `research/local/kismet/`). Only the
//! summary (counts, class names, map names) is meant for publication.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use asamu_ue3::kismet::{self, KismetGraph, KismetSummary};
use asamu_ue3::model::PackageSet;
use serde::Serialize;

use crate::{print_json, safety};

/// Output options of the `kismet` subcommand.
pub struct KismetArgs<'a> {
    /// Map file, or a cooked folder / `Maps` folder (every `.asamu` in it).
    pub path: &'a Path,
    /// Emit Graphviz DOT instead of JSON/text.
    pub dot: bool,
    /// With `--json`, emit the summary instead of the full graph.
    pub summary: bool,
    /// Write the output for one map to this file.
    pub out: Option<&'a Path>,
    /// Write `<map>.kismet.json` and `<map>.kismet.dot` per map into this folder.
    pub out_dir: Option<&'a Path>,
    /// Overwrite existing output files.
    pub force: bool,
    /// Global `--json`.
    pub json: bool,
}

pub fn cmd_kismet(a: &KismetArgs<'_>) -> Result<()> {
    if a.path.is_dir() {
        return cmd_dir(a);
    }
    if a.out_dir.is_some() {
        bail!("--out-dir needs a folder of maps; use --out for one map");
    }
    let (set, lp) =
        PackageSet::for_file(a.path).with_context(|| format!("opening {}", a.path.display()))?;
    let graph = kismet::build_graph_for(&set, &lp);
    let text = if a.dot {
        graph.to_dot()
    } else if a.json && a.summary {
        serde_json::to_string_pretty(&graph.summary())? + "\n"
    } else if a.json {
        serde_json::to_string_pretty(&graph)? + "\n"
    } else {
        summary_text(&graph.summary(), &graph)
    };
    match a.out {
        Some(out) => {
            let target = safety::check_output_path(out, a.path, a.force)?;
            safety::write_output(&target, text.as_bytes(), a.force)?;
            eprintln!(
                "wrote {} bytes to {} (original game data: keep it local)",
                text.len(),
                target.display()
            );
        }
        None => print!("{text}"),
    }
    Ok(())
}

/// Maps directly inside `dir`, or inside `dir/Maps`.
fn maps_in(dir: &Path) -> Result<(Vec<PathBuf>, Vec<PathBuf>)> {
    let maps_dir = if dir.join("Maps").is_dir() {
        dir.join("Maps")
    } else {
        dir.to_path_buf()
    };
    let mut search = vec![maps_dir.clone()];
    if maps_dir != dir {
        search.push(dir.to_path_buf());
    } else if dir
        .file_name()
        .is_some_and(|n| n.to_string_lossy().eq_ignore_ascii_case("Maps"))
        && let Some(parent) = dir.parent()
    {
        search.push(parent.to_path_buf());
    }
    let mut maps: Vec<PathBuf> = std::fs::read_dir(&maps_dir)
        .with_context(|| format!("reading {}", maps_dir.display()))?
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.is_file()
                && p.extension()
                    .is_some_and(|x| x.to_string_lossy().eq_ignore_ascii_case("asamu"))
        })
        .collect();
    maps.sort();
    Ok((maps, search))
}

#[derive(Serialize)]
struct DirReport {
    maps: Vec<KismetSummary>,
}

fn cmd_dir(a: &KismetArgs<'_>) -> Result<()> {
    if a.out.is_some() {
        bail!("--out writes one map; use --out-dir with a folder");
    }
    let (maps, search) = maps_in(a.path)?;
    if maps.is_empty() {
        bail!("no .asamu maps in {}", a.path.display());
    }
    let mut summaries = Vec::new();
    for m in &maps {
        // A fresh set per map keeps memory bounded (maps are large).
        let set = PackageSet::new(&search);
        let lp = set
            .open_file(m)
            .with_context(|| format!("opening {}", m.display()))?;
        let graph = kismet::build_graph_for(&set, &lp);
        if let Some(dir) = a.out_dir {
            write_map_outputs(dir, m, &graph, a.force)?;
        }
        summaries.push((graph.summary(), graph.dangling.len(), graph.warnings.len()));
    }
    if a.json {
        return print_json(&DirReport {
            maps: summaries.into_iter().map(|(s, _, _)| s).collect(),
        });
    }
    println!(
        "{:<24} {:>6} {:>6} {:>5} {:>5} {:>6} {:>6} {:>5} {:>4} {:>4} {:>4} {:>4}  transitions",
        "map", "objs", "level", "seqs", "evts", "acts", "links", "dang", "chk", "grp", "rkt", "nar"
    );
    for (s, dangling, _) in &summaries {
        let seqs = s.sequences.root + s.sequences.sub + s.sequences.prefab_instance;
        let mut transitions = s.map_transitions.clone();
        transitions.extend(s.streamed_levels.iter().map(|l| format!("stream:{l}")));
        println!(
            "{:<24} {:>6} {:>6} {:>5} {:>5} {:>6} {:>6} {:>5} {:>4} {:>4} {:>4} {:>4}  {}",
            s.package,
            s.kismet_objects,
            s.level_objects,
            seqs,
            s.by_kind.get("event").copied().unwrap_or(0),
            s.by_kind.get("action").copied().unwrap_or(0),
            s.links.stored,
            dangling,
            s.features.checkpoint_triggers,
            s.features.grapple_toggles + s.features.max_grapple_sets,
            s.features.rocket_boot_toggles,
            s.features.narrator_lines,
            if transitions.is_empty() {
                "-".to_owned()
            } else {
                transitions.join(", ")
            }
        );
    }
    if let Some(dir) = a.out_dir {
        eprintln!(
            "wrote per-map graphs to {} (original game data: keep it local)",
            dir.display()
        );
    }
    Ok(())
}

fn write_map_outputs(dir: &Path, map: &Path, graph: &KismetGraph, force: bool) -> Result<()> {
    let stem = map
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| graph.package.clone());
    let json = serde_json::to_string_pretty(graph)? + "\n";
    let dot = graph.to_dot();
    let summary = serde_json::to_string_pretty(&graph.summary())? + "\n";
    for (name, data) in [
        (format!("{stem}.kismet.json"), json),
        (format!("{stem}.kismet.dot"), dot),
        (format!("{stem}.kismet-summary.json"), summary),
    ] {
        let target = safety::check_output_path(&dir.join(name), map, force)?;
        safety::write_output(&target, data.as_bytes(), force)?;
    }
    Ok(())
}

fn summary_text(s: &KismetSummary, g: &KismetGraph) -> String {
    use std::fmt::Write as _;
    let mut o = String::new();
    let _ = writeln!(o, "package              {}", s.package);
    let _ = writeln!(
        o,
        "kismet objects       {} (level {}, prefab archetype {}, detached {})",
        s.kismet_objects, s.level_objects, s.prefab_archetype_objects, s.detached_objects
    );
    let _ = writeln!(
        o,
        "sequences            root {} sub {} prefab containers {} prefab instances {} prefab archetypes {} (max depth {})",
        s.sequences.root,
        s.sequences.sub,
        s.sequences.prefab_container,
        s.sequences.prefab_instance,
        s.sequences.prefab_archetype,
        s.sequences.max_depth
    );
    let kinds: Vec<String> = s.by_kind.iter().map(|(k, v)| format!("{k} {v}")).collect();
    let _ = writeln!(o, "by kind              {}", kinds.join(", "));
    let links: Vec<String> = s
        .links
        .by_kind
        .iter()
        .map(|(k, v)| format!("{k} {v}"))
        .collect();
    let _ = writeln!(
        o,
        "links                stored {} derived {} ({}); cross-sequence {}, delayed {}",
        s.links.stored,
        s.links.derived,
        links.join(", "),
        s.links.cross_sequence,
        s.links.delayed
    );
    let _ = writeln!(
        o,
        "unresolved           remote events {} named variables {}; dangling {}",
        s.links.unresolved_remote_events, s.links.unresolved_named_variables, s.links.dangling
    );
    let _ = writeln!(
        o,
        "disabled             events {} sequences {} inputs {} outputs {}",
        s.disabled.events, s.disabled.sequences, s.disabled.input_ports, s.disabled.output_ports
    );
    let st = &g.stats;
    let _ = writeln!(
        o,
        "construction         decode failures {} warnings {}; ports from archetype {}; parent mismatches {}; unlisted members {}; unresolved classes {}",
        st.decode_failures,
        st.decode_warnings,
        st.ports_from_archetype,
        st.parent_mismatches,
        st.unlisted_members,
        st.unresolved_classes
    );
    let f = &s.features;
    let _ = writeln!(
        o,
        "features             checkpoints {} (toggles {}, restart option {}), grapple toggles {}, max-grapple sets {}, rocket-boot toggles {}, narrator lines {}",
        f.checkpoint_triggers,
        f.checkpoint_toggles,
        f.restart_option_toggles,
        f.grapple_toggles,
        f.max_grapple_sets,
        f.rocket_boot_toggles,
        f.narrator_lines
    );
    let _ = writeln!(
        o,
        "                     time trial start {} end {} checks {}, story-mode toggles {}, achievements {}, game finished {}",
        f.time_trial_starts,
        f.time_trial_ends,
        f.time_trial_checks,
        f.story_mode_toggles,
        f.achievements,
        f.game_finished
    );
    let _ = writeln!(
        o,
        "                     matinee {}, console commands {}, level streaming {}, remote events {} -> {}",
        f.matinee_actions,
        f.console_commands,
        f.level_streaming_actions,
        f.remote_event_actions,
        f.remote_event_listeners
    );
    let _ = writeln!(
        o,
        "map transitions      {}",
        dash(&s.map_transitions.join(", "))
    );
    let _ = writeln!(
        o,
        "streamed levels      {}",
        dash(&s.streamed_levels.join(", "))
    );
    let _ = writeln!(o, "\nmilestones:");
    if s.milestones.is_empty() {
        let _ = writeln!(o, "  -");
    }
    for m in &s.milestones {
        let triggers: Vec<String> = m
            .triggers
            .iter()
            .map(|t| {
                let mut x = t.event.clone();
                if let Some(c) = &t.originator_class {
                    x.push_str(&format!("({c})"));
                }
                if t.via_remote_event {
                    x.push_str("[remote]");
                }
                x
            })
            .collect();
        let _ = writeln!(
            o,
            "  #{:<5} {:<40} in {:<24} {}  <= {}",
            m.node,
            m.class,
            m.sequence.as_deref().unwrap_or("-"),
            dash(&m.values.join("; ")),
            dash(&triggers.join(", "))
        );
    }
    let _ = writeln!(o, "\nevent classes:");
    for (c, n) in &s.event_classes {
        let _ = writeln!(o, "  {n:>5}  {c}");
    }
    let _ = writeln!(o, "\nclasses:");
    for (c, n) in &s.by_class {
        let marker = if s.custom_classes.contains_key(c) {
            "*"
        } else {
            " "
        };
        let _ = writeln!(o, "  {n:>5} {marker} {c}");
    }
    if !g.dangling.is_empty() {
        let _ = writeln!(o, "\ndangling links:");
        for d in &g.dangling {
            let from = g.node(d.from).map(|n| n.path.as_str()).unwrap_or("?");
            let _ = writeln!(o, "  {from} {} -> {} ({:?})", d.field, d.target, d.reason);
        }
    }
    if !g.warnings.is_empty() {
        let _ = writeln!(o, "\nwarnings:");
        for w in &g.warnings {
            let _ = writeln!(o, "  {w}");
        }
    }
    o
}

fn dash(s: &str) -> &str {
    if s.is_empty() { "-" } else { s }
}

#[cfg(test)]
mod tests {
    use asamu_ue3::Value;
    use asamu_ue3::kismet::{
        EdgeKind, GRAPH_FORMAT, GRAPH_VERSION, GraphStats, InputPort, KismetEdge, KismetGraph,
        KismetNode, NodeKind, NodeScope, Origin, OutputPort, Param, SequenceInfo, SequenceKind,
    };

    fn node(id: usize, kind: NodeKind, class: &str, name: &str) -> KismetNode {
        KismetNode {
            id,
            export_index: id + 2,
            path: format!("M.{name}"),
            name: name.to_owned(),
            class: class.to_owned(),
            kind,
            custom: class.starts_with("asamu."),
            scope: NodeScope::Level,
            parent: None,
            archetype: None,
            obj_name: None,
            comment: None,
            enabled: None,
            inputs: Vec::new(),
            outputs: Vec::new(),
            variables: Vec::new(),
            events: Vec::new(),
            event: None,
            variable: None,
            params: Vec::new(),
            warnings: Vec::new(),
        }
    }

    /// The JSON export is a stable, documented format: field names, order
    /// and enum spellings are part of it (bump `GRAPH_VERSION` to change).
    #[test]
    fn graph_json_format_is_stable() {
        let mut seq = node(0, NodeKind::Sequence, "Engine.Sequence", "S");
        seq.enabled = Some(true);
        let mut act = node(1, NodeKind::Action, "asamu.SeqAct_X", "S.A");
        act.parent = Some(0);
        act.inputs.push(InputPort {
            desc: "In".into(),
            activate_delay: 0.0,
            disabled: false,
        });
        act.outputs.push(OutputPort {
            desc: "Out".into(),
            activate_delay: 0.25,
            disabled: false,
            links: 1,
        });
        act.params.push(Param {
            name: "Enable".into(),
            array_index: 0,
            value: Value::Bool(true),
            origin: Origin::ClassDefault,
        });
        let g = KismetGraph {
            format: GRAPH_FORMAT.into(),
            version: GRAPH_VERSION,
            package: "M".into(),
            sequences: vec![SequenceInfo {
                node: 0,
                kind: SequenceKind::Root,
                depth: 0,
                members: vec![1],
            }],
            nodes: vec![seq, act],
            edges: vec![KismetEdge {
                kind: EdgeKind::Output,
                from: 1,
                from_port: Some(0),
                to: 1,
                to_port: Some(0),
                delay: Some(0.25),
                derived: false,
                cross_sequence: false,
            }],
            dangling: Vec::new(),
            unresolved: Vec::new(),
            stats: GraphStats::default(),
            warnings: Vec::new(),
        };
        let json = serde_json::to_string(&g).unwrap_or_default();
        let expected = concat!(
            r#"{"format":"asamu-kismet-graph","version":1,"package":"M","#,
            r#""sequences":[{"node":0,"kind":"root","depth":0,"members":[1]}],"#,
            r#""nodes":[{"id":0,"export_index":2,"path":"M.S","name":"S","class":"Engine.Sequence","#,
            r#""kind":"sequence","custom":false,"scope":"level","enabled":true},"#,
            r#"{"id":1,"export_index":3,"path":"M.S.A","name":"S.A","class":"asamu.SeqAct_X","#,
            r#""kind":"action","custom":true,"scope":"level","parent":0,"#,
            r#""inputs":[{"desc":"In","activate_delay":0.0,"disabled":false}],"#,
            r#""outputs":[{"desc":"Out","activate_delay":0.25,"disabled":false,"links":1}],"#,
            r#""params":[{"name":"Enable","array_index":0,"value":{"Bool":true},"origin":"class_default"}]}],"#,
            r#""edges":[{"kind":"output","from":1,"from_port":0,"to":1,"to_port":0,"delay":0.25,"#,
            r#""derived":false,"cross_sequence":false}],"dangling":[],"unresolved":[],"#,
            r#""stats":{"kismet_exports":0,"decode_failures":0,"decode_warnings":0,"#,
            r#""ports_from_archetype":0,"parent_mismatches":0,"unlisted_members":0,"#,
            r#""unresolved_classes":0},"warnings":[]}"#
        );
        assert_eq!(json, expected);
    }

    fn cooked_dir() -> Option<std::path::PathBuf> {
        let root = match std::env::var_os("ASAMU_ORIGINAL_DIR") {
            Some(r) => std::path::PathBuf::from(r),
            None => std::path::PathBuf::from(std::env::var_os("HOME")?)
                .join("Library/Application Support/Steam/steamapps/common/A Story About My Uncle"),
        };
        let dir = root.join("A Story About My Uncle.app/Contents/Resources/ASAMU/CookedMac");
        dir.is_dir().then_some(dir)
    }

    /// Real data (skipped when absent): the JSON of a map is byte-identical
    /// across builds and parses back.
    #[test]
    fn real_map_json_is_deterministic() {
        let Some(dir) = cooked_dir() else {
            eprintln!("SKIP: original game data not found");
            return;
        };
        let map = dir.join("Maps").join("AG-Workshop.asamu");
        let render = || {
            let (set, lp) = asamu_ue3::model::PackageSet::for_file(&map).ok()?;
            let g = asamu_ue3::kismet::build_graph_for(&set, &lp);
            Some((
                serde_json::to_string(&g).ok()?,
                serde_json::to_string(&g.summary()).ok()?,
                g.to_dot(),
            ))
        };
        let a = render();
        assert!(a.is_some());
        assert_eq!(a, render());
        let (json, summary, dot) = a.unwrap_or_default();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap_or_default();
        assert_eq!(v["format"], "asamu-kismet-graph");
        assert_eq!(v["nodes"].as_array().map(Vec::len), Some(257));
        let s: serde_json::Value = serde_json::from_str(&summary).unwrap_or_default();
        assert_eq!(s["map_transitions"][0], "AG-ParadiseCave");
        assert!(dot.starts_with("digraph \"AG-Workshop\""));
    }
}
