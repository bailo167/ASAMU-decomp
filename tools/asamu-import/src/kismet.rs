//! `asamu-import kismet`: per-map Kismet runtime graphs.
//!
//! For every map package (or the ones named with `--map`) this writes into
//! `<out>/kismet/` (user-local; never the repository or the game install):
//!
//! - `<map>.kismet.json` — format `asamu-kismet-runtime` v1, read by
//!   `crates/asamu-kismet` (`asamu_kismet::graph`):
//!   - `nodes`: every Kismet object of the package in export order (ids are
//!     the `asamu_ue3::kismet` node ids, so the Matinee export's `node`
//!     fields match). Level-scope nodes carry their class, kind, parent
//!     sequence, `bEnabled`, input/output links (with their activation
//!     targets), variable links (with `PropertyName` and the linked
//!     variables; named variables are resolved to the variables they find),
//!     event links, sequence members, event settings, variable values,
//!     effective class-specific properties and the class flags
//!     `bAutoActivateOutputLinks` / `bLatentExecution` / derives from
//!     `SeqAct_Latent` (from the merged class defaults). Prefab-archetype and
//!     detached nodes are written as inert `other` nodes.
//!   - `actors`: every actor the graph references (event originators,
//!     object variables and properties, Matinee bindings), every actor
//!     attached (`Base`, at any depth) to a Matinee-bound actor (the
//!     runtime carries these passengers with their base), and their bases,
//!     with level package, `ULevel::Actors` slot, placement and draw scale.
//!   - `sounds`: for each sound cue a sound or narrator action names, the
//!     cue's `Duration` and the `Duration` of its first wave node (depth
//!     first from `FirstNode`, as the engine's `FindFirstWaveNode`; voice
//!     waves are found in the map's `<map>_LOC_INT` companion).
//!   - `probes`: move-track samples computed with `asamu_ue3::matinee` for
//!     every bound actor of every level `SeqAct_Interp` (initial transform at
//!     position 0), so the runtime's Matinee port can be checked bit for bit.
//! - `manifest.json` — per-map counts and the class census.
//!
//! Values are encoded as documented in `asamu_kismet::value`. Everything here
//! is derived from copyrighted game data: keep it local and do not
//! redistribute it.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result, bail};
use asamu_ue3::kismet::{self, EdgeKind, KismetGraph, NodeKind, NodeScope};
use asamu_ue3::level::{self, SceneActor, SceneOptions};
use asamu_ue3::matinee::{self, MatineeMap, MoveInstance, MoveRotation, NoGroupActors, TrackData};
use asamu_ue3::model::{LoadedPackage, PackageSet};
use asamu_ue3::property::Value;
use asamu_ue3::schema::Schema;
use asamu_ue3::sound::SoundDecoder;
use serde::Serialize;
use serde_json::{Map, Value as J, json};

use crate::levels::{prepare_dir, select_maps};
use crate::safety;

/// `format` of a runtime graph.
pub const RUNTIME_FORMAT: &str = "asamu-kismet-runtime";
/// `version` of a runtime graph.
pub const RUNTIME_VERSION: u32 = 1;
/// `format` of `manifest.json`.
pub const MANIFEST_FORMAT: &str = "asamu-kismet-manifest";
/// Deepest value nesting written.
const MAX_DEPTH: usize = 32;
/// Samples per probe (evenly spaced over the `InterpData` length).
const PROBE_SAMPLES: usize = 5;

#[derive(clap::Args, Debug)]
pub struct Args {
    /// Map to convert (file stem, case-insensitive). Repeat for several;
    /// default: every map in the cooked Maps folder.
    #[arg(long = "map")]
    maps: Vec<String>,
    /// Pretty-print JSON.
    #[arg(long)]
    pretty: bool,
    /// Overwrite existing output files.
    #[arg(long)]
    force: bool,
}

/// One map in `manifest.json`.
#[derive(Debug, Serialize)]
pub struct ManifestEntry {
    /// Map package.
    pub map: String,
    /// File written.
    pub file: String,
    /// Nodes written (all scopes).
    pub nodes: usize,
    /// Level-scope nodes.
    pub level_nodes: usize,
    /// Actors referenced.
    pub actors: usize,
    /// Sound cues with durations.
    pub sounds: usize,
    /// Matinee probes.
    pub probes: usize,
    /// Level-scope classes (short name → count).
    pub classes: BTreeMap<String, usize>,
    /// Graph warnings and dangling links.
    pub warnings: usize,
}

/// `manifest.json`.
#[derive(Debug, Serialize)]
pub struct Manifest {
    /// [`MANIFEST_FORMAT`].
    pub format: &'static str,
    /// [`RUNTIME_VERSION`].
    pub version: u32,
    /// Maps.
    pub maps: Vec<ManifestEntry>,
}

fn install_dirs(ctx: &crate::Ctx) -> Result<(PathBuf, PathBuf)> {
    let install = match &ctx.original {
        Some(p) => asamu_locate::from_original_dir(p)?,
        None => asamu_locate::locate()?,
    };
    Ok((install.cooked_dir, install.root))
}

fn to_json<T: Serialize>(v: &T, pretty: bool) -> Result<Vec<u8>> {
    Ok(if pretty {
        serde_json::to_vec_pretty(v)?
    } else {
        serde_json::to_vec(v)?
    })
}

fn write_file(dir: &Path, name: &str, data: &[u8], input: &Path, force: bool) -> Result<PathBuf> {
    let target = safety::check_output_path(&dir.join(name), input, force)?;
    safety::write_output(&target, data, force)?;
    Ok(target)
}

/// JSON encoding of a property value (see `asamu_kismet::value`).
pub fn value_json(v: &Value) -> J {
    value_json_depth(v, 0)
}

fn value_json_depth(v: &Value, depth: usize) -> J {
    if depth > MAX_DEPTH {
        return J::Null;
    }
    match v {
        Value::Int(i) => json!(i),
        Value::Float(f) => {
            // Keep a fraction marker so integral floats stay floats.
            serde_json::Number::from_f64(f64::from(*f)).map_or(J::Null, J::Number)
        }
        Value::Bool(b) => json!(b),
        Value::Byte(b) => json!(b),
        Value::Enum(s) | Value::Name(s) | Value::Str(s) => json!(s),
        Value::Object(o) | Value::Interface(o) => {
            if o.index == 0 {
                json!({"$obj": null})
            } else {
                json!({"$obj": o.path})
            }
        }
        Value::Delegate { .. } | Value::RawArray { .. } | Value::Raw { .. } => J::Null,
        Value::Array(items) => J::Array(
            items
                .iter()
                .map(|i| value_json_depth(i, depth + 1))
                .collect(),
        ),
        Value::Struct { name, fields, .. } => {
            let mut m = Map::new();
            m.insert("$struct".to_owned(), json!(name));
            for f in fields {
                let key = if f.array_index == 0 {
                    f.name.clone()
                } else {
                    format!("{}[{}]", f.name, f.array_index)
                };
                m.insert(key, value_json_depth(&f.value, depth + 1));
            }
            J::Object(m)
        }
    }
}

/// Every object path a value references.
fn collect_paths(v: &Value, out: &mut BTreeSet<String>, depth: usize) {
    if depth > MAX_DEPTH {
        return;
    }
    match v {
        Value::Object(o) | Value::Interface(o) if o.index != 0 => {
            out.insert(o.path.clone());
        }
        Value::Array(items) => {
            for i in items {
                collect_paths(i, out, depth + 1);
            }
        }
        Value::Struct { fields, .. } => {
            for f in fields {
                collect_paths(&f.value, out, depth + 1);
            }
        }
        _ => {}
    }
}

/// Class flags from the merged class defaults.
#[derive(Debug, Clone, Copy, Default)]
struct ClassFlags {
    auto_activate: bool,
    latent: bool,
    latent_base: bool,
}

fn class_flags(set: &PackageSet, class: &str) -> ClassFlags {
    let mut f = ClassFlags::default();
    if let Ok(d) = set.inherited_defaults(class) {
        for v in &d.values {
            let b = matches!(v.value, Value::Bool(true));
            if v.name.eq_ignore_ascii_case("bAutoActivateOutputLinks") {
                f.auto_activate = b;
            } else if v.name.eq_ignore_ascii_case("bLatentExecution") {
                f.latent = b;
            }
        }
    }
    f.latent_base = set.class_chain(class).iter().any(|c| c == "seqact_latent");
    f
}

/// The runtime nodes of a graph (see the module docs).
fn nodes_json(g: &KismetGraph, set: &PackageSet) -> (Vec<J>, BTreeSet<String>, BTreeSet<String>) {
    let n = g.nodes.len();
    let mut outputs: Vec<Vec<Vec<J>>> = g
        .nodes
        .iter()
        .map(|x| vec![Vec::new(); x.outputs.len()])
        .collect();
    let mut var_links: Vec<Vec<Vec<usize>>> = g
        .nodes
        .iter()
        .map(|x| vec![Vec::new(); x.variables.len()])
        .collect();
    let mut event_links: Vec<Vec<Vec<usize>>> = g
        .nodes
        .iter()
        .map(|x| vec![Vec::new(); x.events.len()])
        .collect();
    let mut named: HashMap<usize, Vec<usize>> = HashMap::new();
    for e in &g.edges {
        match e.kind {
            EdgeKind::Output => {
                if let (Some(fp), Some(tp)) = (e.from_port, e.to_port)
                    && let Some(slot) = outputs.get_mut(e.from).and_then(|o| o.get_mut(fp))
                {
                    slot.push(json!({"op": e.to, "input": tp}));
                }
            }
            EdgeKind::Variable | EdgeKind::Matinee => {
                if let Some(fp) = e.from_port
                    && let Some(slot) = var_links.get_mut(e.from).and_then(|o| o.get_mut(fp))
                {
                    slot.push(e.to);
                }
            }
            EdgeKind::Event => {
                if let Some(fp) = e.from_port
                    && let Some(slot) = event_links.get_mut(e.from).and_then(|o| o.get_mut(fp))
                {
                    slot.push(e.to);
                }
            }
            EdgeKind::NamedVariable => named.entry(e.from).or_default().push(e.to),
            _ => {}
        }
    }
    let mut flags_cache: HashMap<String, ClassFlags> = HashMap::new();
    let mut actor_paths = BTreeSet::new();
    let mut sound_paths = BTreeSet::new();
    let mut out = Vec::with_capacity(n);
    for x in &g.nodes {
        if x.scope != NodeScope::Level {
            out.push(json!({"id": x.id, "path": x.path, "class": x.class, "kind": "other"}));
            continue;
        }
        let flags = *flags_cache
            .entry(x.class.clone())
            .or_insert_with(|| class_flags(set, &x.class));
        let inputs: Vec<J> = x
            .inputs
            .iter()
            .map(|i| json!({"desc": i.desc, "delay": i.activate_delay, "disabled": i.disabled}))
            .collect();
        let outs: Vec<J> = x
            .outputs
            .iter()
            .enumerate()
            .map(|(i, o)| {
                json!({
                    "desc": o.desc, "delay": o.activate_delay, "disabled": o.disabled,
                    "links": outputs.get(x.id).and_then(|v| v.get(i)).cloned().unwrap_or_default(),
                })
            })
            .collect();
        let vars: Vec<J> = x
            .variables
            .iter()
            .enumerate()
            .map(|(i, v)| {
                let mut ids: Vec<usize> = Vec::new();
                for t in var_links
                    .get(x.id)
                    .and_then(|l| l.get(i))
                    .cloned()
                    .unwrap_or_default()
                {
                    match named.get(&t) {
                        Some(found) => ids.extend(found.iter().copied()),
                        None => ids.push(t),
                    }
                }
                json!({
                    "desc": v.desc, "property": v.property_name, "writeable": v.writeable,
                    "vars": ids,
                })
            })
            .collect();
        let evs: Vec<J> = x
            .events
            .iter()
            .enumerate()
            .map(|(i, e)| {
                json!({"desc": e.desc,
                       "events": event_links.get(x.id).and_then(|l| l.get(i)).cloned().unwrap_or_default()})
            })
            .collect();
        let mut params = Map::new();
        for p in &x.params {
            if p.array_index != 0 {
                continue;
            }
            collect_paths(&p.value, &mut actor_paths, 0);
            let short = x.class_name();
            if (short == "SeqAct_PlaySound" && p.name.eq_ignore_ascii_case("PlaySound"))
                || (short == "SeqAct_NarratorLine" && p.name.eq_ignore_ascii_case("Cue"))
            {
                collect_paths(&p.value, &mut sound_paths, 0);
            }
            params.insert(p.name.clone(), value_json(&p.value));
        }
        let mut node = Map::new();
        node.insert("id".into(), json!(x.id));
        node.insert("path".into(), json!(x.path));
        node.insert("class".into(), json!(x.class));
        node.insert("kind".into(), json!(x.kind.name()));
        node.insert("parent".into(), json!(x.parent));
        if let Some(o) = &x.obj_name {
            node.insert("obj_name".into(), json!(o));
        }
        if let Some(e) = x.enabled {
            node.insert("enabled".into(), json!(e));
        }
        node.insert("inputs".into(), J::Array(inputs));
        node.insert("outputs".into(), J::Array(outs));
        node.insert("variables".into(), J::Array(vars));
        node.insert("event_links".into(), J::Array(evs));
        if x.kind == NodeKind::Sequence
            && let Some(s) = g.sequences.iter().find(|s| s.node == x.id)
        {
            node.insert("members".into(), json!(s.members));
        }
        if let Some(ev) = &x.event {
            if let Some(o) = &ev.originator {
                actor_paths.insert(o.clone());
            }
            node.insert(
                "event".into(),
                json!({
                    "originator": ev.originator,
                    "max_trigger_count": ev.max_trigger_count.unwrap_or(0),
                    "retrigger_delay": ev.retrigger_delay.unwrap_or(0.0),
                    "player_only": ev.player_only.unwrap_or(false),
                    "priority": ev.priority.unwrap_or(0),
                }),
            );
        }
        if let Some(v) = &x.variable {
            if let Some(val) = &v.value {
                collect_paths(val, &mut actor_paths, 0);
            }
            node.insert(
                "var".into(),
                json!({
                    "var_name": v.var_name,
                    "value": v.value.as_ref().map_or(J::Null, value_json),
                    "find_var_name": v.find_var_name,
                }),
            );
        }
        node.insert("params".into(), J::Object(params));
        node.insert("auto_activate_outputs".into(), json!(flags.auto_activate));
        node.insert("latent".into(), json!(flags.latent));
        node.insert("latent_base".into(), json!(flags.latent_base));
        out.push(J::Object(node));
    }
    (out, actor_paths, sound_paths)
}

/// Most `Base` links followed from one actor.
const MAX_BASE_CHAIN: usize = 16;

/// The actors (lower-case paths, keys of `bases`: path → base path) attached
/// through `Base`, at any depth up to [`MAX_BASE_CHAIN`], to an actor in
/// `bound`. Cycles and self-bases end the walk.
fn passengers(
    bases: &BTreeMap<String, Option<String>>,
    bound: &BTreeSet<String>,
) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for (path, first) in bases {
        let mut base = first.as_ref();
        let mut steps = 0;
        while let Some(b) = base {
            steps += 1;
            if steps > MAX_BASE_CHAIN || b == path {
                break;
            }
            if bound.contains(b) {
                out.insert(path.clone());
                break;
            }
            base = bases.get(b).and_then(Option::as_ref);
        }
    }
    out
}

fn actor_json(scene_level: &str, package: &str, a: &SceneActor) -> J {
    json!({
        "path": format!("{scene_level}.{}", a.name),
        "name": a.name,
        "class": a.class,
        "kind": a.kind,
        "package": package,
        "slot": a.slot,
        "location": a.location,
        "rotation": a.rotation,
        "draw_scale": a.draw_scale,
        "draw_scale3d": a.draw_scale3d,
        "pre_pivot": a.pre_pivot,
        "base": a.base,
        "hidden": a.hidden,
    })
}

/// Duration of the first wave node reached depth first from the cue's
/// `FirstNode`.
fn first_wave_duration(g: &asamu_ue3::sound::CueGraph) -> Option<f32> {
    let by_path: HashMap<&str, &asamu_ue3::sound::CueNode> =
        g.nodes.iter().map(|n| (n.path.as_str(), n)).collect();
    let mut stack: Vec<(&str, usize)> = g
        .cue
        .first_node
        .as_deref()
        .map(|p| (p, 0))
        .into_iter()
        .collect();
    let mut visited = BTreeSet::new();
    while let Some((p, depth)) = stack.pop() {
        if depth > 64 || !visited.insert(p) {
            continue;
        }
        let node = by_path.get(p)?;
        if node.kind.is_some_and(|k| k.is_wave()) {
            return node.wave.as_ref().and_then(|w| w.duration);
        }
        for c in node.children.iter().rev().flatten() {
            stack.push((c.as_str(), depth + 1));
        }
    }
    None
}

fn sounds_json(set: &PackageSet, lp: &LoadedPackage, paths: &BTreeSet<String>) -> Map<String, J> {
    let dec = SoundDecoder::new(set);
    dec.register_package(&lp.name);
    // Voice and narrator waves live in the map's localized companion
    // (`<map>_LOC_INT`); without it the first wave of those cues does not
    // resolve and its duration is lost. The default (INT) language's
    // durations are used.
    let loc = format!("{}_LOC_INT", lp.name);
    if set.package(&loc).is_some() {
        dec.register_package(&loc);
    }
    let mut out = Map::new();
    for p in paths {
        let Some((pkg, index)) = dec.locate_from(lp, p) else {
            continue;
        };
        let owner: &LoadedPackage = pkg.as_deref().unwrap_or(lp);
        let duration = dec.decode_cue(owner, index).ok().and_then(|c| c.duration);
        let first = dec
            .cue_graph(owner, index)
            .ok()
            .and_then(|g| first_wave_duration(&g));
        out.insert(
            p.clone(),
            json!({"duration": duration, "first_wave_duration": first}),
        );
    }
    out
}

/// Move-track probes for every bound actor of every level action.
fn probes_json(
    m: &MatineeMap,
    placements: &HashMap<String, (SceneActorPlacement, Option<String>)>,
) -> Vec<J> {
    let mut out = Vec::new();
    for a in &m.actions {
        if a.scope != NodeScope::Level {
            continue;
        }
        let Some(data) = a.interp_data.as_deref().and_then(|p| m.data(p)) else {
            continue;
        };
        for b in &a.bindings {
            let Some(gname) = b.group.as_deref() else {
                continue;
            };
            let Some(group) = data
                .groups
                .iter()
                .find(|g| g.name.eq_ignore_ascii_case(gname))
            else {
                continue;
            };
            for t in &b.targets {
                let Some(obj) = t.object.as_deref() else {
                    continue;
                };
                let Some((place, base)) = placements.get(&obj.to_ascii_lowercase()) else {
                    continue;
                };
                let base_place = base
                    .as_deref()
                    .and_then(|bp| placements.get(&bp.to_ascii_lowercase()))
                    .map(|(p, _)| *p);
                for track in &group.tracks {
                    let TrackData::Move(mt) = &track.data else {
                        continue;
                    };
                    if track.disabled || !mt.is_active() {
                        continue;
                    }
                    let inst = match base_place {
                        Some(bp) => MoveInstance::with_base(
                            mt,
                            place.location,
                            place.rotation,
                            &matinee::rotation_translation_matrix(bp.rotation, bp.location),
                            0.0,
                            &NoGroupActors,
                        ),
                        None => MoveInstance::new(
                            mt,
                            place.location,
                            place.rotation,
                            0.0,
                            &NoGroupActors,
                        ),
                    };
                    let mut samples = Vec::new();
                    for k in 0..PROBE_SAMPLES {
                        let t = if PROBE_SAMPLES > 1 {
                            data.length * k as f32 / (PROBE_SAMPLES - 1) as f32
                        } else {
                            0.0
                        };
                        if let Some(s) = mt.sample(t, &inst, &NoGroupActors) {
                            let rot = match s.rotation {
                                MoveRotation::Set(r) => r,
                                _ => place.rotation,
                            };
                            samples.push(json!({"t": t, "location": s.location, "rotation": rot}));
                        }
                    }
                    out.push(json!({
                        "action": a.node, "group": group.name, "actor": obj,
                        "position": 0.0, "samples": samples,
                    }));
                }
            }
        }
    }
    out
}

#[derive(Debug, Clone, Copy)]
struct SceneActorPlacement {
    location: [f32; 3],
    rotation: [i32; 3],
}

/// Builds the runtime graph document of one map.
pub fn convert_map(file: &Path, cooked: &Path) -> Result<(String, J, ManifestEntry)> {
    let set = PackageSet::new(&[cooked.to_path_buf(), cooked.join("Maps")]);
    let lp: Arc<LoadedPackage> = set
        .open_file(file)
        .with_context(|| format!("opening {}", file.display()))?;
    let g = kismet::build_graph_for(&set, &lp);
    let (nodes, mut actor_paths, sound_paths) = nodes_json(&g, &set);
    let m = matinee::extract_for(&set, &lp);
    for a in &m.actions {
        for b in &a.bindings {
            for t in &b.targets {
                if let Some(o) = &t.object {
                    actor_paths.insert(o.clone());
                }
            }
        }
    }
    // Scene placements of the referenced actors (and their bases).
    let mut actors_out = Vec::new();
    let mut placements: HashMap<String, (SceneActorPlacement, Option<String>)> = HashMap::new();
    if let Some(&level_export) = level::level_exports(&lp.package).first() {
        let opts = SceneOptions {
            volume_geometry: false,
            ..SceneOptions::default()
        };
        if let Ok(scene) = level::extract_scene(&set, &lp, level_export, &opts) {
            let by_path: BTreeMap<String, &SceneActor> = scene
                .actors
                .iter()
                .map(|a| {
                    (
                        format!("{}.{}", scene.level, a.name).to_ascii_lowercase(),
                        a,
                    )
                })
                .collect();
            let mut wanted: BTreeSet<String> =
                actor_paths.iter().map(|p| p.to_ascii_lowercase()).collect();
            // Passengers: actors attached (through `Base`, at any depth) to
            // an actor a Matinee group moves. UE3 carries them with their
            // base, so the runtime needs their placement and base.
            let bound: BTreeSet<String> = m
                .actions
                .iter()
                .flat_map(|a| a.bindings.iter())
                .flat_map(|b| b.targets.iter())
                .filter_map(|t| t.object.as_deref())
                .map(str::to_ascii_lowercase)
                .collect();
            let bases: BTreeMap<String, Option<String>> = by_path
                .iter()
                .map(|(p, a)| (p.clone(), a.base.as_deref().map(str::to_ascii_lowercase)))
                .collect();
            wanted.extend(passengers(&bases, &bound));
            let mut done: BTreeSet<String> = BTreeSet::new();
            let mut guard = 0usize;
            while let Some(p) = wanted.pop_first() {
                guard += 1;
                if guard > 1 << 20 || !done.insert(p.clone()) {
                    continue;
                }
                let Some(a) = by_path.get(&p) else { continue };
                if let Some(b) = &a.base {
                    wanted.insert(b.to_ascii_lowercase());
                }
                placements.insert(
                    p.clone(),
                    (
                        SceneActorPlacement {
                            location: a.location,
                            rotation: a.rotation,
                        },
                        a.base.clone(),
                    ),
                );
                actors_out.push(actor_json(&scene.level, &lp.name, a));
            }
        }
    }
    let sounds = sounds_json(&set, &lp, &sound_paths);
    let probes = probes_json(&m, &placements);
    let mut classes: BTreeMap<String, usize> = BTreeMap::new();
    for x in g.nodes.iter().filter(|x| x.scope == NodeScope::Level) {
        *classes.entry(x.class_name().to_owned()).or_insert(0) += 1;
    }
    let entry = ManifestEntry {
        map: g.package.clone(),
        file: format!("{}.kismet.json", g.package),
        nodes: g.nodes.len(),
        level_nodes: g
            .nodes
            .iter()
            .filter(|x| x.scope == NodeScope::Level)
            .count(),
        actors: actors_out.len(),
        sounds: sounds.len(),
        probes: probes.len(),
        classes,
        warnings: g.warnings.len() + g.dangling.len(),
    };
    let doc = json!({
        "format": RUNTIME_FORMAT,
        "version": RUNTIME_VERSION,
        "package": g.package,
        "nodes": nodes,
        "actors": actors_out,
        "sounds": sounds,
        "probes": probes,
    });
    Ok((g.package.clone(), doc, entry))
}

pub fn run(ctx: &crate::Ctx, args: Args) -> Result<()> {
    let (cooked, install_root) = install_dirs(ctx)?;
    let maps_dir = cooked.join("Maps");
    let maps = select_maps(&maps_dir, &args.maps)?;
    if maps.is_empty() {
        bail!("no map packages in {}", maps_dir.display());
    }
    let out_dir = prepare_dir(&ctx.out.join("kismet"), &maps_dir, &install_root)?;
    eprintln!(
        "writing Kismet graphs derived from your own install to {} (do not redistribute)",
        out_dir.display()
    );
    let mut manifest = Manifest {
        format: MANIFEST_FORMAT,
        version: RUNTIME_VERSION,
        maps: Vec::new(),
    };
    for file in &maps {
        let (package, doc, entry) = convert_map(file, &cooked)?;
        println!(
            "{:<20} nodes {:>5} (level {:>5})  actors {:>4}  sounds {:>3}  probes {:>4}  warnings {}",
            package,
            entry.nodes,
            entry.level_nodes,
            entry.actors,
            entry.sounds,
            entry.probes,
            entry.warnings
        );
        write_file(
            &out_dir,
            &entry.file,
            &to_json(&doc, args.pretty)?,
            file,
            args.force,
        )?;
        manifest.maps.push(entry);
    }
    write_file(
        &out_dir,
        "manifest.json",
        &to_json(&manifest, true)?,
        &maps_dir,
        true,
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use asamu_ue3::property::{ObjRef, Property};

    fn prop(name: &str, value: Value) -> Property {
        Property {
            name: name.to_owned(),
            type_name: String::new(),
            array_index: 0,
            size: 0,
            struct_name: None,
            enum_name: None,
            value,
            offset: 0,
        }
    }

    #[test]
    fn values_encode_as_the_runtime_expects() {
        let v = Value::Struct {
            name: "Vector".into(),
            binary: true,
            fields: vec![prop("X", Value::Float(1.0)), prop("Y", Value::Float(2.5))],
        };
        let j = value_json(&v);
        assert_eq!(j["$struct"], "Vector");
        assert_eq!(j["X"].as_f64(), Some(1.0));
        assert!(j["X"].is_f64(), "integral floats stay floats");
        let o = value_json(&Value::Object(ObjRef {
            index: 5,
            path: "P.A".into(),
        }));
        assert_eq!(o, json!({"$obj": "P.A"}));
        let null = value_json(&Value::Object(ObjRef {
            index: 0,
            path: "None".into(),
        }));
        assert_eq!(null, json!({"$obj": null}));
        assert_eq!(value_json(&Value::Int(7)), json!(7));
        assert_eq!(
            value_json(&Value::Enum("CIM_Linear".into())),
            json!("CIM_Linear")
        );
        let mut paths = BTreeSet::new();
        collect_paths(
            &Value::Array(vec![Value::Object(ObjRef {
                index: 3,
                path: "P.B".into(),
            })]),
            &mut paths,
            0,
        );
        assert!(paths.contains("P.B"));
        // Deep nesting is cut off without panicking.
        let mut deep = Value::Int(1);
        for _ in 0..(MAX_DEPTH + 4) {
            deep = Value::Array(vec![deep]);
        }
        let _ = value_json(&deep);
    }

    #[test]
    fn passengers_follow_base_chains_to_matinee_bound_actors() {
        let b = |pairs: &[(&str, Option<&str>)]| -> BTreeMap<String, Option<String>> {
            pairs
                .iter()
                .map(|(p, b)| ((*p).to_owned(), b.map(str::to_owned)))
                .collect()
        };
        let bases = b(&[
            ("ship", None),
            ("crate", Some("ship")),
            ("cup", Some("crate")),
            ("rock", Some("ground")),
            ("ground", None),
            ("loop_a", Some("loop_b")),
            ("loop_b", Some("loop_a")),
            ("selfie", Some("selfie")),
            ("orphan", Some("missing")),
        ]);
        let bound: BTreeSet<String> = ["ship".to_owned()].into();
        let got: Vec<String> = passengers(&bases, &bound).into_iter().collect();
        assert_eq!(got, vec!["crate".to_owned(), "cup".to_owned()]);
        // A bound actor attached to another bound one is listed too (it is
        // carried when its base moves).
        let bound: BTreeSet<String> = ["ship".to_owned(), "crate".to_owned()].into();
        assert!(passengers(&bases, &bound).contains("crate"));
    }

    #[test]
    fn manifest_shape_is_stable() {
        let m = Manifest {
            format: MANIFEST_FORMAT,
            version: RUNTIME_VERSION,
            maps: vec![ManifestEntry {
                map: "AG-Test".into(),
                file: "AG-Test.kismet.json".into(),
                nodes: 3,
                level_nodes: 2,
                actors: 1,
                sounds: 0,
                probes: 0,
                classes: BTreeMap::new(),
                warnings: 0,
            }],
        };
        let v: J = serde_json::from_slice(&to_json(&m, false).unwrap()).unwrap();
        assert_eq!(v["format"], MANIFEST_FORMAT);
        assert_eq!(v["maps"][0]["level_nodes"], 2);
    }

    #[test]
    fn output_is_refused_inside_the_repository() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(Path::parent)
            .unwrap()
            .to_path_buf();
        let input = root.join("Cargo.toml");
        let tmp = tempfile::tempdir().unwrap();
        let install = tmp.path().join("no-install");
        assert!(prepare_dir(&root.join("kismet-test-out"), &input, &install).is_err());
        assert!(!root.join("kismet-test-out").exists());
    }

    /// Gated on the user's install (skips without it): one map converts to
    /// a runtime graph whose node ids are dense, whose output links target
    /// existing inputs, and which is byte-identical when converted twice.
    #[test]
    fn real_map_converts_to_a_consistent_runtime_graph() {
        let Ok(install) = asamu_locate::locate() else {
            eprintln!("SKIP: original game data not found");
            return;
        };
        let cooked = install.cooked_dir.clone();
        let Ok(maps) = select_maps(&cooked.join("Maps"), &["AG-Epilogue".to_owned()]) else {
            eprintln!("SKIP: AG-Epilogue not found");
            return;
        };
        let (pkg, doc, entry) = convert_map(&maps[0], &cooked).unwrap();
        assert_eq!(pkg, "AG-Epilogue");
        let nodes = doc["nodes"].as_array().unwrap();
        assert_eq!(nodes.len(), entry.nodes);
        for (i, n) in nodes.iter().enumerate() {
            assert_eq!(n["id"].as_u64(), Some(i as u64));
            for o in n["outputs"].as_array().into_iter().flatten() {
                for l in o["links"].as_array().unwrap() {
                    let t = &nodes[l["op"].as_u64().unwrap() as usize];
                    let inputs = t["inputs"].as_array().unwrap();
                    assert!((l["input"].as_u64().unwrap() as usize) < inputs.len());
                }
            }
        }
        let again = convert_map(&maps[0], &cooked).unwrap().1;
        assert_eq!(
            serde_json::to_vec(&doc).unwrap(),
            serde_json::to_vec(&again).unwrap()
        );
    }
}
