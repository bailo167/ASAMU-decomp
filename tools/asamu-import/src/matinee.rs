//! `asamu-import matinee`: per-map Matinee (`InterpData`) export.
//!
//! For every map package (or the ones named with `--map`) that holds any
//! Matinee data, this writes into `<out>/matinee/` (user-local; never the
//! repository or the game install):
//!
//! - `<map>.matinee.json` — [`asamu_ue3::matinee::MatineeMap`]: every
//!   `SeqAct_Interp` with its effective settings (play rate, looping,
//!   rewind, forced start), its `InterpData`, its group bindings (the
//!   actors each group drives, by object path and class) and property
//!   links; every `InterpData` with its groups and tracks (move curves and
//!   split axis sub-tracks, event keys, director cuts, sounds, property /
//!   material / skeletal-control curves, fades, anim keys, toggles,
//!   visibility keys, ...); `CameraAnim` assets; publishable coverage
//!   counts.
//! - `manifest.json` — per-map counts.
//! - `<map>.actors.json` ([`ActorsFile`], format [`ACTORS_FORMAT`]) — what
//!   the runtime needs to play the skeletal actors Matinee and Kismet talk
//!   to: each placed skeletal mesh component's own `AnimNodeSequence`
//!   (`AnimSeqName`, `bLooping`, `bPlaying`, `CurrentTime`, `Rate`, so the
//!   ambient animations and their notifies run as in the original) and the
//!   `SkelControlLookAt` controls of the components' instanced anim trees
//!   (control name, the bone its control list names, axes, limits, blend
//!   times) that `SeqAct_SetLookAtTarget` actors aim at the player.
//! - `camera_anims.json` ([`CameraAnimsFile`], format
//!   [`CAMERA_ANIMS_FORMAT`]) — the `CameraAnim` assets of `Startup.upk`
//!   (`ASAMUCameraAnimations`, `Zeth_CameraStuffs`, ...: the grapple,
//!   landing, power-jump, rocket-boots and worm-growl camera animations the
//!   gameplay script plays), decoded like the maps' own camera animations.
//! - `anim_notifies.json` ([`AnimNotifiesFile`], format
//!   [`ANIM_NOTIFIES_FORMAT`]) — every `AnimNotify` object of `Startup.upk`
//!   and of the converted maps by path: `AnimNotify_Kismet`'s `NotifyName`
//!   (which `SeqEvent_AnimNotify` it fires) and `AnimNotify_Sound`'s cue and
//!   options. The skeletal manifest lists each sequence's notifies by
//!   object path only.
//!
//! Curve keys are written as stored (`in`, `out`, `arrive`, `leave`,
//! `mode`); evaluate them with `asamu_ue3::matinee::InterpCurve::eval`,
//! which follows the original engine's arithmetic.
//!
//! All of this is derived from copyrighted game data: keep it local and do
//! not redistribute it.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result, bail};
use asamu_ue3::level::{as_f32, member, prop};
use asamu_ue3::matinee::{self, CameraAnimInfo, MatineeCoverage, MatineeMap};
use asamu_ue3::model::{LoadedPackage, PackageSet};
use asamu_ue3::{Property, Value};
use serde::Serialize;

use crate::levels::{effective_props, prepare_dir, select_maps};
use crate::safety;

/// `format` of `manifest.json`.
pub const MANIFEST_FORMAT: &str = "asamu-matinee-manifest";
/// `format` of `camera_anims.json`.
pub const CAMERA_ANIMS_FORMAT: &str = "asamu-camera-anims";
/// `format` of `anim_notifies.json`.
pub const ANIM_NOTIFIES_FORMAT: &str = "asamu-anim-notifies";
/// `format` of `<map>.actors.json`.
pub const ACTORS_FORMAT: &str = "asamu-matinee-actors";
/// `version` of the three files above.
pub const EXTRA_VERSION: u32 = 1;
/// Notes kept per file (the rest are counted).
const MAX_EXTRA_WARNINGS: usize = 64;

#[derive(clap::Args, Debug)]
pub struct Args {
    /// Map to convert (file stem, case-insensitive, e.g. AG-IceCave). Repeat
    /// for several; default: every map in the cooked Maps folder.
    #[arg(long = "map")]
    maps: Vec<String>,
    /// Pretty-print JSON.
    #[arg(long)]
    pretty: bool,
    /// Overwrite existing output files.
    #[arg(long)]
    force: bool,
    /// Also write maps without any Matinee data (an empty document).
    #[arg(long)]
    include_empty: bool,
}

/// One map in `manifest.json` (counts only).
#[derive(Debug, Serialize)]
pub struct ManifestEntry {
    /// Map package name.
    pub map: String,
    /// File written (none when the map has no Matinee data).
    pub file: Option<String>,
    /// Coverage counts.
    pub coverage: MatineeCoverage,
}

/// `manifest.json`.
#[derive(Debug, Serialize)]
pub struct Manifest {
    /// [`MANIFEST_FORMAT`].
    pub format: &'static str,
    /// [`matinee::MATINEE_VERSION`] of the map files.
    pub matinee_version: u32,
    /// Maps, in file-name order.
    pub maps: Vec<ManifestEntry>,
}

/// Cooked package folder and root of the install.
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

/// True when the map holds anything worth a file.
pub fn has_content(m: &MatineeMap) -> bool {
    !m.actions.is_empty() || !m.interp_data.is_empty() || !m.camera_anims.is_empty()
}

/// `camera_anims.json`: the `CameraAnim` assets of a non-map package.
#[derive(Debug, Serialize)]
pub struct CameraAnimsFile {
    /// [`CAMERA_ANIMS_FORMAT`].
    pub format: &'static str,
    /// [`EXTRA_VERSION`].
    pub version: u32,
    /// Package the assets come from.
    pub package: String,
    /// The assets (same shape as a map file's `camera_anims`).
    pub camera_anims: Vec<CameraAnimInfo>,
}

/// One `AnimNotify` object (effective values: own over class defaults).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct AnimNotifyInfo {
    /// Object path (as the skeletal manifest's notify entries name it).
    pub path: String,
    /// Qualified class (`Engine.AnimNotify_Kismet`, ...).
    pub class: String,
    /// `AnimNotify_Kismet.NotifyName` (`None` when unset or `None`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub notify_name: Option<String>,
    /// `AnimNotify_Sound.SoundCue`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sound_cue: Option<String>,
    /// `AnimNotify_Sound.bFollowActor`.
    pub follow_actor: bool,
    /// `AnimNotify_Sound.bIgnoreIfActorHidden`.
    pub ignore_if_actor_hidden: bool,
    /// `AnimNotify_Sound.BoneName` (`None` when unset).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bone: Option<String>,
    /// `AnimNotify_Sound.VolumeMultiplier`.
    pub volume: f32,
    /// `AnimNotify_Sound.PitchMultiplier`.
    pub pitch: f32,
    /// `AnimNotify_Sound.PercentToPlay`.
    pub percent_to_play: f32,
}

/// `anim_notifies.json`.
#[derive(Debug, Serialize)]
pub struct AnimNotifiesFile {
    /// [`ANIM_NOTIFIES_FORMAT`].
    pub format: &'static str,
    /// [`EXTRA_VERSION`].
    pub version: u32,
    /// Notifies by path (sorted).
    pub notifies: Vec<AnimNotifyInfo>,
}

/// The `AnimNodeSequence` a placed skeletal mesh component plays on its own
/// (the component's `Animations` object when it is a sequence node, not a
/// tree).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct AnimNodeInfo {
    /// Owning actor's object path.
    pub actor: String,
    /// Component object name.
    pub component: String,
    /// `AnimSeqName` (`None` when unset).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sequence: Option<String>,
    /// `bLooping`.
    pub looping: bool,
    /// `bPlaying`.
    pub playing: bool,
    /// `CurrentTime` (the start position), s.
    pub start_time: f32,
    /// `Rate`.
    pub rate: f32,
}

/// A `SkelControlLookAt` of a placed component's instanced anim tree.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct LookAtControlInfo {
    /// Owning actor's object path.
    pub actor: String,
    /// Component object name.
    pub component: String,
    /// `ControlName`.
    pub control: String,
    /// The bone of the tree's `SkelControlLists` entry the control is in.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bone: Option<String>,
    /// `LookAtAxis` (`AXIS_X`, ...).
    pub look_at_axis: String,
    /// `UpAxis`.
    pub up_axis: String,
    /// `bInvertLookAtAxis`.
    pub invert_look_at_axis: bool,
    /// `bInvertUpAxis`.
    pub invert_up_axis: bool,
    /// `bEnableLimit`.
    pub enable_limit: bool,
    /// `bLimitBasedOnRefPose`.
    pub limit_based_on_ref_pose: bool,
    /// `MaxAngle`, degrees.
    pub max_angle: f32,
    /// `OuterMaxAngle`, degrees.
    pub outer_max_angle: f32,
    /// `DeadZoneAngle`, degrees.
    pub dead_zone_angle: f32,
    /// `bAllowRotationX/Y/Z`.
    pub allow_rotation: [bool; 3],
    /// `AllowRotationSpace` (`BCS_BoneSpace`, ...).
    pub allow_rotation_space: String,
    /// `TargetLocationInterpSpeed`.
    pub target_interp_speed: f32,
    /// `ControlStrength` at level start.
    pub control_strength: f32,
    /// `BlendInTime`, s.
    pub blend_in_time: f32,
    /// `BlendOutTime`, s.
    pub blend_out_time: f32,
}

/// `<map>.actors.json`.
#[derive(Debug, Serialize)]
pub struct ActorsFile {
    /// [`ACTORS_FORMAT`].
    pub format: &'static str,
    /// [`EXTRA_VERSION`].
    pub version: u32,
    /// Map package.
    pub package: String,
    /// Components' own sequence nodes, in export order.
    pub anim_nodes: Vec<AnimNodeInfo>,
    /// Look-at controls, in export order.
    pub look_at_controls: Vec<LookAtControlInfo>,
    /// Decoder notes (capped).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}

impl ActorsFile {
    /// True when the file would carry anything.
    #[must_use]
    pub fn has_content(&self) -> bool {
        !self.anim_nodes.is_empty() || !self.look_at_controls.is_empty()
    }
}

fn note(warnings: &mut Vec<String>, message: String) {
    if warnings.len() < MAX_EXTRA_WARNINGS {
        warnings.push(message);
    }
}

fn value_name(v: Option<&Value>) -> Option<String> {
    match v? {
        Value::Name(s) | Value::Str(s) | Value::Enum(s) if !s.is_empty() && s != "None" => {
            Some(s.clone())
        }
        _ => None,
    }
}

fn value_bool(v: Option<&Value>, default: bool) -> bool {
    match v {
        Some(Value::Bool(b)) => *b,
        _ => default,
    }
}

fn value_f32(v: Option<&Value>, default: f32) -> f32 {
    v.and_then(as_f32)
        .filter(|f| f.is_finite())
        .unwrap_or(default)
}

fn value_obj(v: Option<&Value>) -> Option<String> {
    match v? {
        Value::Object(o) if o.index != 0 => Some(o.path.clone()),
        _ => None,
    }
}

/// Class path of export `i` and whether its super chain contains `base`
/// (qualified, case-insensitive).
fn class_is(set: &PackageSet, lp: &LoadedPackage, i: usize, base: &str) -> Option<String> {
    let class = asamu_ue3::object::export_class_path(&lp.package, Some(&lp.name), i).ok()?;
    let chain = set.super_chain(&class);
    let is = class.eq_ignore_ascii_case(base) || chain.iter().any(|c| c.eq_ignore_ascii_case(base));
    is.then_some(class)
}

/// Every `AnimNotify` object of `lp` (sorted by path into `out`).
pub fn anim_notifies_of(
    set: &PackageSet,
    lp: &Arc<LoadedPackage>,
    out: &mut BTreeMap<String, AnimNotifyInfo>,
) {
    for i in 0..lp.package.exports.len() {
        let Some(class) = class_is(set, lp, i, "Engine.AnimNotify") else {
            continue;
        };
        let Ok(path) = lp.qualified(i) else { continue };
        let props: Vec<Property> = effective_props(set, lp, i, 0).unwrap_or_default();
        let get = |n: &str| prop(&props, n);
        out.insert(
            path.to_ascii_lowercase(),
            AnimNotifyInfo {
                path,
                class,
                notify_name: value_name(get("NotifyName")),
                sound_cue: value_obj(get("SoundCue")),
                follow_actor: value_bool(get("bFollowActor"), false),
                ignore_if_actor_hidden: value_bool(get("bIgnoreIfActorHidden"), false),
                bone: value_name(get("BoneName")),
                volume: value_f32(get("VolumeMultiplier"), 1.0),
                pitch: value_f32(get("PitchMultiplier"), 1.0),
                percent_to_play: value_f32(get("PercentToPlay"), 1.0),
            },
        );
    }
}

/// `Pkg.TheWorld.PersistentLevel.<Actor>.<Component>` → (actor path,
/// component name) for components of placed actors.
fn placed_component(path: &str) -> Option<(String, String)> {
    let parts: Vec<&str> = path.split('.').collect();
    match parts.as_slice() {
        [pkg, world, level, actor, component]
            if world.eq_ignore_ascii_case("TheWorld")
                && level.eq_ignore_ascii_case("PersistentLevel") =>
        {
            Some((
                format!("{pkg}.{world}.{level}.{actor}"),
                (*component).to_owned(),
            ))
        }
        _ => None,
    }
}

/// The bone of every control in an anim tree's `SkelControlLists`
/// (control path lower case → bone), following each list's
/// `ControlHead` → `NextControl` chain (bounded).
fn control_bones(set: &PackageSet, tree_props: &[Property]) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    let Some(Value::Array(lists)) = prop(tree_props, "SkelControlLists") else {
        return out;
    };
    for list in lists {
        let Some(bone) = value_name(member(list, "BoneName")) else {
            continue;
        };
        let mut next = value_obj(member(list, "ControlHead"));
        let mut steps = 0;
        while let Some(path) = next.take() {
            steps += 1;
            if steps > 64 || out.contains_key(&path.to_ascii_lowercase()) {
                break;
            }
            out.insert(path.to_ascii_lowercase(), bone.clone());
            if let Some((clp, ci)) = set.locate(&path)
                && let Ok(o) = set.decode(&clp, ci)
            {
                next = value_obj(prop(&o.properties, "NextControl"));
            }
        }
    }
    out
}

/// The `SkelControlLookAt` controls of the anim tree at `tree_path` (its
/// subobjects in whichever package holds it), with the bone each control's
/// list names: `(control path, effective properties, bone)`.
fn tree_look_at_controls(
    set: &PackageSet,
    tree_path: &str,
) -> Vec<(String, Vec<Property>, Option<String>)> {
    let Some((tlp, ti)) = set.locate(tree_path) else {
        return Vec::new();
    };
    let bones = effective_props(set, &tlp, ti, 0)
        .map(|p| control_bones(set, &p))
        .unwrap_or_default();
    let prefix = format!("{}.", tree_path.to_ascii_lowercase());
    let mut out = Vec::new();
    for i in 0..tlp.package.exports.len() {
        let Ok(path) = tlp.qualified(i) else { continue };
        if !path.to_ascii_lowercase().starts_with(&prefix)
            || class_is(set, &tlp, i, "Engine.SkelControlLookAt").is_none()
        {
            continue;
        }
        let props = effective_props(set, &tlp, i, 0).unwrap_or_default();
        let bone = bones.get(&path.to_ascii_lowercase()).cloned();
        out.push((path, props, bone));
    }
    out
}

fn look_at_info(
    actor: &str,
    component: &str,
    props: &[Property],
    bone: Option<String>,
) -> Option<LookAtControlInfo> {
    let get = |n: &str| prop(props, n);
    Some(LookAtControlInfo {
        actor: actor.to_owned(),
        component: component.to_owned(),
        control: value_name(get("ControlName"))?,
        bone,
        look_at_axis: value_name(get("LookAtAxis")).unwrap_or_else(|| "AXIS_X".into()),
        up_axis: value_name(get("UpAxis")).unwrap_or_else(|| "AXIS_Z".into()),
        invert_look_at_axis: value_bool(get("bInvertLookAtAxis"), false),
        invert_up_axis: value_bool(get("bInvertUpAxis"), false),
        enable_limit: value_bool(get("bEnableLimit"), false),
        limit_based_on_ref_pose: value_bool(get("bLimitBasedOnRefPose"), true),
        max_angle: value_f32(get("MaxAngle"), 0.0),
        outer_max_angle: value_f32(get("OuterMaxAngle"), 0.0),
        dead_zone_angle: value_f32(get("DeadZoneAngle"), 0.0),
        allow_rotation: [
            value_bool(get("bAllowRotationX"), true),
            value_bool(get("bAllowRotationY"), true),
            value_bool(get("bAllowRotationZ"), true),
        ],
        allow_rotation_space: value_name(get("AllowRotationSpace"))
            .unwrap_or_else(|| "BCS_BoneSpace".into()),
        target_interp_speed: value_f32(get("TargetLocationInterpSpeed"), 0.0),
        control_strength: value_f32(get("ControlStrength"), 1.0),
        blend_in_time: value_f32(get("BlendInTime"), 0.0),
        blend_out_time: value_f32(get("BlendOutTime"), 0.0),
    })
}

/// The ambient sequence nodes and look-at controls of a map's placed
/// skeletal actors. A component's `Animations` is either its own sequence
/// node (exported as an [`AnimNodeInfo`]) or an anim tree, instanced in the
/// map or inherited from the component's archetype; the tree's
/// `SkelControlLookAt` controls are exported per component.
pub fn actors_of_map(set: &PackageSet, lp: &Arc<LoadedPackage>) -> ActorsFile {
    let mut file = ActorsFile {
        format: ACTORS_FORMAT,
        version: EXTRA_VERSION,
        package: lp.name.clone(),
        anim_nodes: Vec::new(),
        look_at_controls: Vec::new(),
        warnings: Vec::new(),
    };
    type Controls = Vec<(String, Vec<Property>, Option<String>)>;
    let mut trees: BTreeMap<String, Controls> = BTreeMap::new();
    for i in 0..lp.package.exports.len() {
        if class_is(set, lp, i, "Engine.SkeletalMeshComponent").is_none() {
            continue;
        }
        let Ok(path) = lp.qualified(i) else { continue };
        let Some((actor, component)) = placed_component(&path) else {
            continue;
        };
        let props = match effective_props(set, lp, i, 0) {
            Ok(p) => p,
            Err(e) => {
                note(&mut file.warnings, format!("{path}: {e}"));
                continue;
            }
        };
        // The component's own sequence node (ambient actors).
        if let Some(anim) = value_obj(prop(&props, "Animations"))
            && let Some((alp, ai)) = set.locate(&anim)
            && class_is(set, &alp, ai, "Engine.AnimNodeSequence").is_some()
        {
            let node = effective_props(set, &alp, ai, 0).unwrap_or_default();
            let get = |n: &str| prop(&node, n);
            file.anim_nodes.push(AnimNodeInfo {
                actor: actor.clone(),
                component: component.clone(),
                sequence: value_name(get("AnimSeqName")),
                looping: value_bool(get("bLooping"), false),
                playing: value_bool(get("bPlaying"), false),
                start_time: value_f32(get("CurrentTime"), 0.0),
                rate: value_f32(get("Rate"), 1.0),
            });
        }
        // The anim tree the component instances at run time
        // (`AnimTreeTemplate`, own or from its archetype) and its look-at
        // controls.
        if let Some(tree) = value_obj(prop(&props, "AnimTreeTemplate")) {
            let controls = trees
                .entry(tree.to_ascii_lowercase())
                .or_insert_with(|| tree_look_at_controls(set, &tree));
            for (_, cprops, bone) in controls.iter() {
                if let Some(info) = look_at_info(&actor, &component, cprops, bone.clone()) {
                    file.look_at_controls.push(info);
                }
            }
        }
    }
    file
}

/// The `CameraAnim` assets of a non-map package file.
pub fn camera_anims_of(file: &Path, cooked: &Path) -> Result<CameraAnimsFile> {
    let set = PackageSet::new(&[cooked.to_path_buf(), cooked.join("Maps")]);
    let lp = set
        .open_file(file)
        .with_context(|| format!("opening {}", file.display()))?;
    let m = matinee::extract_for(&set, &lp);
    Ok(CameraAnimsFile {
        format: CAMERA_ANIMS_FORMAT,
        version: EXTRA_VERSION,
        package: lp.name.clone(),
        camera_anims: m.camera_anims,
    })
}

/// The Matinee data and the extras of one map (one package set).
pub fn convert_map_with_extras(
    file: &Path,
    cooked: &Path,
    notifies: &mut BTreeMap<String, AnimNotifyInfo>,
) -> Result<(MatineeMap, ActorsFile)> {
    let set = PackageSet::new(&[cooked.to_path_buf(), cooked.join("Maps")]);
    let lp = set
        .open_file(file)
        .with_context(|| format!("opening {}", file.display()))?;
    let m = matinee::extract_for(&set, &lp);
    let actors = actors_of_map(&set, &lp);
    anim_notifies_of(&set, &lp, notifies);
    Ok((m, actors))
}

/// Decode one map (a fresh package set per map keeps memory bounded).
#[cfg(test)]
pub fn convert_map(file: &Path, cooked: &Path) -> Result<MatineeMap> {
    let set = PackageSet::new(&[cooked.to_path_buf(), cooked.join("Maps")]);
    let lp = set
        .open_file(file)
        .with_context(|| format!("opening {}", file.display()))?;
    Ok(matinee::extract_for(&set, &lp))
}

/// One summary line per map (counts only).
pub fn summary_line(m: &MatineeMap) -> String {
    let c = &m.coverage;
    format!(
        "{:<20} actions {:>3}  data {:>3}  groups {:>4}  tracks {:>4} (decoded {:>4}, unknown {})  \
         bindings {:>3}  camera anims {}  warnings {}",
        m.package,
        c.actions,
        c.interp_data,
        c.groups.values().sum::<usize>(),
        c.tracks_total,
        c.tracks_decoded,
        c.tracks_unknown,
        c.bindings,
        c.camera_anims,
        c.warnings
    )
}

pub fn run(ctx: &crate::Ctx, args: Args) -> Result<()> {
    let (cooked, install_root) = install_dirs(ctx)?;
    let maps_dir = cooked.join("Maps");
    let maps = select_maps(&maps_dir, &args.maps)?;
    if maps.is_empty() {
        bail!("no map packages in {}", maps_dir.display());
    }
    let out_dir = prepare_dir(&ctx.out.join("matinee"), &maps_dir, &install_root)?;
    eprintln!(
        "writing Matinee data derived from your own install to {} (do not redistribute)",
        out_dir.display()
    );
    let mut manifest = Manifest {
        format: MANIFEST_FORMAT,
        matinee_version: matinee::MATINEE_VERSION,
        maps: Vec::new(),
    };
    let mut notifies: BTreeMap<String, AnimNotifyInfo> = BTreeMap::new();
    for file in &maps {
        let (m, actors) = convert_map_with_extras(file, &cooked, &mut notifies)?;
        println!("{}", summary_line(&m));
        let file_name = if has_content(&m) || args.include_empty {
            let name = format!("{}.matinee.json", m.package);
            write_file(
                &out_dir,
                &name,
                &to_json(&m, args.pretty)?,
                file,
                args.force,
            )?;
            Some(name)
        } else {
            None
        };
        if actors.has_content() {
            println!(
                "{:<20} skeletal anim nodes {:>3}  look-at controls {:>3}",
                actors.package,
                actors.anim_nodes.len(),
                actors.look_at_controls.len()
            );
            write_file(
                &out_dir,
                &format!("{}.actors.json", actors.package),
                &to_json(&actors, args.pretty)?,
                file,
                args.force,
            )?;
        }
        manifest.maps.push(ManifestEntry {
            map: m.package.clone(),
            file: file_name,
            coverage: m.coverage,
        });
    }
    // The gameplay camera animations and the shared anim sets' notifies live
    // in `Startup.upk` (CONFIRMED by an export census: 25 `CameraAnim`
    // assets; the villagers' `AnimNotify_Kismet` objects).
    let startup = cooked.join("Startup.upk");
    if startup.is_file() {
        let anims = camera_anims_of(&startup, &cooked)?;
        println!(
            "{:<20} camera anims {}",
            anims.package,
            anims.camera_anims.len()
        );
        write_file(
            &out_dir,
            "camera_anims.json",
            &to_json(&anims, args.pretty)?,
            &startup,
            true,
        )?;
        let set = PackageSet::new(&[cooked.clone(), maps_dir.clone()]);
        let lp = set
            .open_file(&startup)
            .with_context(|| format!("opening {}", startup.display()))?;
        anim_notifies_of(&set, &lp, &mut notifies);
    }
    let notifies_file = AnimNotifiesFile {
        format: ANIM_NOTIFIES_FORMAT,
        version: EXTRA_VERSION,
        notifies: notifies.into_values().collect(),
    };
    println!("anim notifies {}", notifies_file.notifies.len());
    write_file(
        &out_dir,
        "anim_notifies.json",
        &to_json(&notifies_file, args.pretty)?,
        &maps_dir,
        true,
    )?;
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
    use asamu_ue3::matinee::{MATINEE_FORMAT, MATINEE_VERSION};

    fn empty(name: &str) -> MatineeMap {
        MatineeMap {
            format: MATINEE_FORMAT.to_owned(),
            version: MATINEE_VERSION,
            package: name.to_owned(),
            actions: Vec::new(),
            interp_data: Vec::new(),
            camera_anims: Vec::new(),
            coverage: MatineeCoverage::default(),
            orphans: Vec::new(),
            warnings: Vec::new(),
        }
    }

    #[test]
    fn empty_maps_have_no_content_and_summarise() {
        let m = empty("ASAMULegal");
        assert!(!has_content(&m));
        let line = summary_line(&m);
        assert!(line.starts_with("ASAMULegal"));
        assert!(line.contains("tracks    0"));
    }

    #[test]
    fn manifest_json_shape_is_stable() {
        let manifest = Manifest {
            format: MANIFEST_FORMAT,
            matinee_version: MATINEE_VERSION,
            maps: vec![ManifestEntry {
                map: "AG-Test".to_owned(),
                file: None,
                coverage: MatineeCoverage::default(),
            }],
        };
        let v: serde_json::Value =
            serde_json::from_slice(&to_json(&manifest, false).unwrap()).unwrap();
        assert_eq!(v["format"], MANIFEST_FORMAT);
        assert_eq!(v["matinee_version"], MATINEE_VERSION);
        assert_eq!(v["maps"][0]["map"], "AG-Test");
        assert!(v["maps"][0]["file"].is_null());
        assert_eq!(v["maps"][0]["coverage"]["tracks_total"], 0);
        let doc: serde_json::Value =
            serde_json::from_slice(&to_json(&empty("AG-Test"), true).unwrap()).unwrap();
        assert_eq!(doc["format"], MATINEE_FORMAT);
        assert_eq!(doc["version"], MATINEE_VERSION);
        assert!(doc["actions"].as_array().unwrap().is_empty());
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
        assert!(prepare_dir(&root.join("matinee-test-out"), &input, &install).is_err());
        assert!(!root.join("matinee-test-out").exists());
        let ok = prepare_dir(&tmp.path().join("conv").join("matinee"), &input, &install).unwrap();
        let f = write_file(&ok, "m.matinee.json", b"{}", &input, false).unwrap();
        assert!(write_file(&ok, "m.matinee.json", b"{}", &input, false).is_err());
        assert!(write_file(&ok, "m.matinee.json", b"[]", &input, true).is_ok());
        assert_eq!(std::fs::read(f).unwrap(), b"[]");
    }

    /// The writer never lands in the game install or follows a symlink:
    /// an output folder inside the install, a symlinked output folder that
    /// leads into it, and a symlink planted at a target file are refused
    /// (the last even with `--force`), and the input stays untouched.
    #[cfg(unix)]
    #[test]
    fn output_refuses_the_install_and_symlinks() {
        let tmp = tempfile::tempdir().unwrap();
        let install = tmp.path().join("Game");
        std::fs::create_dir_all(install.join("Maps")).unwrap();
        let input = install.join("Maps").join("AG-Test.asamu");
        std::fs::write(&input, b"x").unwrap();
        assert!(prepare_dir(&install.join("out").join("matinee"), &input, &install).is_err());
        assert!(!install.join("out").exists());
        let conv = tmp.path().join("conv");
        std::fs::create_dir_all(&conv).unwrap();
        std::os::unix::fs::symlink(&install, conv.join("matinee")).unwrap();
        assert!(prepare_dir(&conv.join("matinee"), &input, &install).is_err());
        let ok = prepare_dir(&tmp.path().join("conv2").join("matinee"), &input, &install).unwrap();
        std::os::unix::fs::symlink(&input, ok.join("AG-Test.matinee.json")).unwrap();
        assert!(write_file(&ok, "AG-Test.matinee.json", b"{}", &input, true).is_err());
        assert!(write_file(&ok, "AG-Test.matinee.json", b"{}", &input, false).is_err());
        assert_eq!(std::fs::read(&input).unwrap(), b"x");
        // The manifest is rewritten in place (force) without following links.
        let m = write_file(&ok, "manifest.json", b"{}", &input, true).unwrap();
        assert!(write_file(&ok, "manifest.json", b"[]", &input, true).is_ok());
        assert_eq!(std::fs::read(m).unwrap(), b"[]");
    }

    /// Gated on the user's install (skips without it): one map converts to
    /// JSON whose actions all name a decoded `InterpData`, converting twice
    /// gives identical bytes, and the run writes only into the temporary
    /// output directory.
    #[test]
    fn real_map_converts_to_consistent_json() {
        let Ok(install) = asamu_locate::locate() else {
            eprintln!("SKIP: original game data not found");
            return;
        };
        let cooked = install.cooked_dir.clone();
        let Ok(maps) = select_maps(&cooked.join("Maps"), &["AG-IceCave".to_owned()]) else {
            eprintln!("SKIP: AG-IceCave not found");
            return;
        };
        let m = convert_map(&maps[0], &cooked).unwrap();
        assert!(has_content(&m));
        let tmp = tempfile::tempdir().unwrap();
        let dir = prepare_dir(&tmp.path().join("matinee"), &maps[0], &install.root).unwrap();
        let bytes = to_json(&m, false).unwrap();
        // Deterministic: a second, independent conversion is byte-identical.
        let again = convert_map(&maps[0], &cooked).unwrap();
        assert_eq!(to_json(&again, false).unwrap(), bytes);
        let f = write_file(&dir, "AG-IceCave.matinee.json", &bytes, &maps[0], false).unwrap();
        let doc: serde_json::Value = serde_json::from_slice(&std::fs::read(f).unwrap()).unwrap();
        assert_eq!(doc["format"], matinee::MATINEE_FORMAT);
        let data: Vec<&str> = doc["interp_data"]
            .as_array()
            .unwrap()
            .iter()
            .map(|d| d["path"].as_str().unwrap())
            .collect();
        let actions = doc["actions"].as_array().unwrap();
        assert_eq!(actions.len(), m.coverage.actions);
        for a in actions {
            assert!(data.contains(&a["interp_data"].as_str().unwrap()));
        }
        // Curve keys carry the documented field names.
        let first_point =
            &doc["interp_data"][0]["groups"][0]["tracks"][0]["data"]["pos"]["points"][0];
        for k in ["in", "out", "arrive", "leave", "mode"] {
            assert!(!first_point[k].is_null(), "{k}");
        }
    }

    #[test]
    fn placed_components_and_values_decode() {
        assert_eq!(
            placed_component("M.TheWorld.PersistentLevel.Actor_1.Comp_0"),
            Some(("M.TheWorld.PersistentLevel.Actor_1".into(), "Comp_0".into()))
        );
        assert!(placed_component("M.TheWorld.PersistentLevel.Actor_1").is_none());
        assert!(placed_component("Pkg.Group.Actor.Comp").is_none());
        assert!(placed_component("M.TheWorld.PersistentLevel.A.B.C").is_none());
        assert_eq!(value_name(Some(&Value::Name("X".into()))), Some("X".into()));
        assert_eq!(value_name(Some(&Value::Name("None".into()))), None);
        assert_eq!(value_name(None), None);
        assert!(value_bool(Some(&Value::Bool(true)), false));
        assert!(value_bool(None, true));
        assert_eq!(value_f32(Some(&Value::Float(f32::NAN)), 2.0), 2.0);
        assert_eq!(value_f32(Some(&Value::Int(3)), 2.0), 3.0);
        let mut w = Vec::new();
        for i in 0..(MAX_EXTRA_WARNINGS + 5) {
            note(&mut w, format!("{i}"));
        }
        assert_eq!(w.len(), MAX_EXTRA_WARNINGS);
    }

    #[test]
    fn extra_files_have_a_stable_shape() {
        let actors = ActorsFile {
            format: ACTORS_FORMAT,
            version: EXTRA_VERSION,
            package: "AG-Test".into(),
            anim_nodes: vec![AnimNodeInfo {
                actor: "AG-Test.TheWorld.PersistentLevel.A".into(),
                component: "C".into(),
                sequence: Some("Idle".into()),
                looping: true,
                playing: true,
                start_time: 1.0,
                rate: 1.0,
            }],
            look_at_controls: Vec::new(),
            warnings: Vec::new(),
        };
        assert!(actors.has_content());
        let v: serde_json::Value =
            serde_json::from_slice(&to_json(&actors, false).unwrap()).unwrap();
        assert_eq!(v["format"], ACTORS_FORMAT);
        assert_eq!(v["anim_nodes"][0]["sequence"], "Idle");
        assert_eq!(v["anim_nodes"][0]["start_time"], 1.0);
        assert!(v.get("warnings").is_none());
        let notifies = AnimNotifiesFile {
            format: ANIM_NOTIFIES_FORMAT,
            version: EXTRA_VERSION,
            notifies: vec![AnimNotifyInfo {
                path: "P.S.N".into(),
                class: "Engine.AnimNotify_Kismet".into(),
                notify_name: Some("Talk".into()),
                sound_cue: None,
                follow_actor: false,
                ignore_if_actor_hidden: false,
                bone: None,
                volume: 1.0,
                pitch: 1.0,
                percent_to_play: 1.0,
            }],
        };
        let v: serde_json::Value =
            serde_json::from_slice(&to_json(&notifies, false).unwrap()).unwrap();
        assert_eq!(v["notifies"][0]["notify_name"], "Talk");
        assert!(v["notifies"][0].get("sound_cue").is_none());
    }

    /// Gated on the user's install (skips without it): the gameplay camera
    /// animations the class defaults and script name are all in
    /// `Startup.upk` with a decoded camera group, and BeautifulCity's
    /// talking villagers carry their own sequence nodes while the anim
    /// notifies that fire their Kismet events are named.
    #[test]
    fn real_startup_camera_anims_and_actor_extras() {
        let Ok(install) = asamu_locate::locate() else {
            eprintln!("SKIP: original game data not found");
            return;
        };
        let cooked = install.cooked_dir.clone();
        let startup = cooked.join("Startup.upk");
        if !startup.is_file() {
            eprintln!("SKIP: Startup.upk not found");
            return;
        }
        let anims = camera_anims_of(&startup, &cooked).unwrap();
        assert_eq!(anims.camera_anims.len(), 25);
        for want in [
            "ASAMUCameraAnimations.Grapple.GrappleBegin",
            "ASAMUCameraAnimations.Grapple.GrappleLoop",
            "ASAMUCameraAnimations.NormalLand",
            "ASAMUCameraAnimations.HardLanding",
            "ASAMUCameraAnimations.PowerJumpBob",
            "ASAMUCameraAnimations.PowerLeapBob",
            "ASAMUCameraAnimations.rocketBoots.RocketBootsBegin",
            "ASAMUCameraAnimations.rocketBoots.RocketBootsBoosting",
            "Zeth_CameraStuffs.PowerJumpChargeCameraAnim",
            "Zeth_CameraStuffs.MonsterGrowl",
        ] {
            let a = anims
                .camera_anims
                .iter()
                .find(|a| a.path == want)
                .unwrap_or_else(|| panic!("{want}"));
            assert!(a.length >= 0.0, "{want}");
            assert!(a.group.is_some(), "{want}");
        }
        // The grapple animations carry no tracks (post-process settings
        // only); the landing and boots ones move the camera.
        let moves = |p: &str| {
            anims.camera_anims.iter().any(|a| {
                a.path == p
                    && a.group.as_ref().is_some_and(|g| {
                        g.tracks
                            .iter()
                            .any(|t| matches!(t.data, matinee::TrackData::Move(_)))
                    })
            })
        };
        assert!(!moves("ASAMUCameraAnimations.Grapple.GrappleBegin"));
        assert!(moves("ASAMUCameraAnimations.HardLanding"));
        assert!(moves(
            "ASAMUCameraAnimations.rocketBoots.RocketBootsBoosting"
        ));
        let Ok(maps) = select_maps(&cooked.join("Maps"), &["AG-BeautifulCity".to_owned()]) else {
            eprintln!("SKIP: AG-BeautifulCity not found");
            return;
        };
        let mut notifies = BTreeMap::new();
        let (_, actors) = convert_map_with_extras(&maps[0], &cooked, &mut notifies).unwrap();
        assert!(actors.anim_nodes.len() >= 60, "{}", actors.anim_nodes.len());
        assert!(actors.anim_nodes.iter().all(|n| n.rate.is_finite()));
        assert!(actors.look_at_controls.iter().any(|c| c.bone.is_some()));
        let set = PackageSet::new(&[cooked.clone(), cooked.join("Maps")]);
        let lp = set.open_file(&startup).unwrap();
        anim_notifies_of(&set, &lp, &mut notifies);
        let kismet: Vec<&AnimNotifyInfo> = notifies
            .values()
            .filter(|n| n.class == "Engine.AnimNotify_Kismet")
            .collect();
        assert!(!kismet.is_empty());
        assert!(kismet.iter().all(|n| n.notify_name.is_some()));
    }
}
