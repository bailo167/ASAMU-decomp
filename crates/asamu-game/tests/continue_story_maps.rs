//! Continue in every story map, measured against the game that played on.
//! Skips without `ASAMU_CONVERTED_DIR` (a conversion with simple collision,
//! Kismet and Matinee); ignored by default (minutes of simulation; run it
//! optimized: `cargo test -p asamu-game --release --test continue_story_maps
//! -- --ignored --nocapture`).
//!
//! `continue_from_checkpoint.rs` shows the rule on one save of
//! AG-ParadiseCave. Here the rule is attacked on all seven chapters with
//! saves the level's own play produced:
//!
//! - **The walk** (ours, test tooling): a fresh start, the opening, then the
//!   level's checkpoints in index order. Before each checkpoint the pawn is
//!   put into every Kismet touch trigger that lies nearest to the checkpoint
//!   before it (the story exits excepted) and the game runs on for some
//!   seconds, so cutscenes start, doors open, abilities are given, and the
//!   pawn dies where it falls. Every checkpoint that becomes the latest
//!   writes the save the app writes ([`Game::capture_snapshot`],
//!   [`LevelScript::save_into`]), and the running game is kept as it was at
//!   that tick.
//! - **The comparison**: the level is started again from each save, as
//!   Continue starts it, next to the kept game that simply plays on. What
//!   the continued game shows in its first 15 s, and where its script and
//!   its world differ from the game that played on, is printed; the rules
//!   below are asserted.
//! - **The way out**: from a continued game the smoke harness's story chain
//!   ([`asamu_game::smoke::chain_step_from`]) must still reach the next map.
//! - Two cases by themselves: AG-IceCave's trigger that streams `TheCore`
//!   in (where it lies among the checkpoints), and AG-BeautifulCity's first
//!   checkpoint, which the first-encounter cutscene triggers by an event
//!   key while its Matinee is still playing.
//!
//! With `ASAMU_CONTINUE_SAVE_OUT=<folder>` the first and a late save of each
//! walk, and the cutscene's save of AG-BeautifulCity, are also written as
//! save folders of ours ([`write_save`]) for a look at the same Continue in
//! the app.
//!
//! What the comparison prints and does not assert is what the original's
//! rule gives as far as it was read (KISMET_RUNTIME.md §12), listed there
//! under "What a continued game does not have": loops the level-loaded
//! events start again, actors that one-time script had hidden, story mode,
//! and the event keys of a Matinee that was playing at the save, which fire
//! again up to the saved position.
//!
//! Asserted (KISMET_RUNTIME.md §12):
//!
//! 1. The opening that a fresh start shows does not show again: a continued
//!    game takes the view from the player (a director cut, cinematic mode, a
//!    view target, a Matinee fade) only where the game that played on does
//!    so in the same stretch, or where the level's own level-loaded chain
//!    does (which runs on every load in the original too).
//! 2. No narrator line of the fresh start's first 25 s is spoken again.
//! 3. The saved events, variables and Matinees are the continued game's
//!    after its first tick, except what that tick's level-loaded chain and
//!    running Matinees changed in both.
//! 4. Abilities, collectibles, the latest checkpoint and the pawn's place
//!    (the checkpoint's spawn point) are the saved ones.
//! 5. A death in the continued game respawns at the saved checkpoint and
//!    shows nothing of the opening.
//! 6. Nothing becomes non-finite, and the script reports no new error.

#![allow(clippy::unwrap_used)]

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use asamu_game::asamu_kismet::{OpClass, Output, ScriptState, ToggleMode};
use asamu_game::asamu_world::WorldEvent;
use asamu_game::kismet_host::ScriptRestore;
use asamu_game::save::{ChapterId, SaveSession, SaveStore, Snapshot, checkpoint_saved};
use asamu_game::smoke;
use asamu_game::{Game, LevelScript, ScriptedTick, load_level_with_kismet};
use asamu_player::InputFrame;
use glam::Vec3;

/// Ticks a fresh start runs before the walk begins (25 s: every level's
/// opening is over or well under way).
const OPENING: u64 = 1_500;
/// Ticks the game runs on after the pawn was put into a trigger (5 s).
const SETTLE: u64 = 300;
/// Ticks a continued game and the game that played on are compared over.
const WINDOW: u64 = 900;
/// Most saves compared per map (first, last and evenly between).
const MAX_SAVES: usize = 9;

fn converted_dir() -> Option<PathBuf> {
    let d = PathBuf::from(std::env::var_os("ASAMU_CONVERTED_DIR")?);
    (d.join("levels").is_dir() && d.join("kismet").is_dir() && d.join("matinee").is_dir())
        .then_some(d)
}

/// With `ASAMU_CONTINUE_SAVE_OUT=<folder>`: writes `snapshot` as a save
/// folder of ours (`<folder>/<name>/saves`: `snapshot.json`, `general.json`,
/// `progression.json`), so that the app can be started with
/// `ASAMU_SAVE_DIR=<folder>/<name>` and `ASAMU_MENU_ACTION=continue` to look
/// at the same Continue on screen. A developer's aid; the folder is
/// user-local data and never part of the repository.
fn write_save(map: &str, name: &str, snapshot: &Snapshot) {
    let Some(out) = std::env::var_os("ASAMU_CONTINUE_SAVE_OUT") else {
        return;
    };
    let dir = PathBuf::from(out).join(name).join("saves");
    let mut on_disk = SaveSession::open(SaveStore::new(&dir));
    on_disk.new_game().unwrap();
    on_disk.begin_level(map, None).unwrap();
    on_disk.on_checkpoint_saved(snapshot.clone()).unwrap();
    eprintln!("the save was written to {}", dir.display());
}

/// What a stretch of ticks showed and did.
#[derive(Debug, Default, Clone)]
struct Shown {
    /// Kinds of taking the view from the player, with the ticks (from 1).
    view: BTreeMap<&'static str, Vec<u64>>,
    /// Narrator lines started.
    narrator: Vec<String>,
    /// Tutorial prompts shown.
    tutorials: Vec<i32>,
    /// Level transitions and movies.
    transitions: Vec<String>,
    /// Every output kind, counted.
    kinds: BTreeMap<String, u32>,
    /// Actors hidden (true) or shown (false) by an action, last state.
    hidden: BTreeMap<String, bool>,
    /// Actors toggled on (true) or off (false) by an action, last state.
    toggled: BTreeMap<String, bool>,
    /// Actors destroyed.
    destroyed: BTreeSet<String>,
    /// Deaths and respawns.
    deaths: u32,
    respawns: u32,
    /// Non-finite values met.
    problems: Vec<String>,
}

impl Shown {
    fn note(&mut self, tick: u64, t: &ScriptedTick) {
        for o in &t.outputs {
            *self.kinds.entry(smoke::output_kind(o)).or_default() += 1;
            let view = match o {
                Output::MatineeCut { group: Some(_), .. } => Some("cut"),
                Output::CinematicMode { mode, .. } if *mode != ToggleMode::Off => Some("cinematic"),
                Output::CameraTarget { target: Some(_) } => Some("view target"),
                Output::MatineeFade { amount, .. } if *amount > 0.0 => Some("matinee fade"),
                Output::CameraFade { opacity, .. } if *opacity > 0.0 => Some("camera fade"),
                _ => None,
            };
            if let Some(kind) = view {
                self.view.entry(kind).or_default().push(tick);
            }
            match o {
                Output::NarratorLine { id, .. } => self.narrator.push(id.clone()),
                Output::TutorialShow { id, .. } => self.tutorials.push(*id),
                Output::LevelTransition { map, .. } => self.transitions.push(format!("open {map}")),
                Output::OpenMovie { movie, .. } => {
                    self.transitions.push(format!("movie {movie:?}"));
                }
                Output::ActorHidden { actor, mode } => {
                    let now = match mode {
                        ToggleMode::On => true,
                        ToggleMode::Off => false,
                        ToggleMode::Toggle => !self.hidden.get(actor).copied().unwrap_or(false),
                    };
                    self.hidden.insert(actor.clone(), now);
                }
                Output::ActorToggled { actor, mode } => {
                    let now = match mode {
                        ToggleMode::On => true,
                        ToggleMode::Off => false,
                        ToggleMode::Toggle => !self.toggled.get(actor).copied().unwrap_or(true),
                    };
                    self.toggled.insert(actor.clone(), now);
                }
                Output::ActorDestroyed { actor } => {
                    self.destroyed.insert(actor.clone());
                }
                _ => {}
            }
        }
        for e in t.report.world.iter() {
            match e {
                WorldEvent::PlayerDied { .. } => self.deaths += 1,
                WorldEvent::PlayerRespawned => self.respawns += 1,
                _ => {}
            }
        }
    }

    /// The kinds of taking the view, with how often and the first tick.
    fn view_line(&self) -> String {
        if self.view.is_empty() {
            return "the player's view throughout".to_owned();
        }
        self.view
            .iter()
            .map(|(k, t)| format!("{k} x{} from tick {}", t.len(), t[0]))
            .collect::<Vec<_>>()
            .join(", ")
    }
}

/// One tick; `None` when the game stopped.
fn step(
    game: &mut Game,
    script: &mut LevelScript,
    input: &InputFrame,
    shown: &mut Shown,
    tick: u64,
) -> Option<ScriptedTick> {
    let t = script.tick(game, input)?;
    shown.note(tick, &t);
    if shown.problems.len() < 8 {
        shown.problems.extend(smoke::non_finite(game, Some(script)));
    }
    Some(t)
}

/// A save the walk wrote, with the game as it was at that tick.
struct Save {
    index: i32,
    /// Tick of the walk.
    tick: u64,
    /// The checkpoint did not register by touch and was triggered as a
    /// `SeqAct_TriggerCheckpoint` would.
    forced: bool,
    snapshot: Snapshot,
    game: Game,
    script: LevelScript,
    /// Hidden and toggled actors up to the save, from the walk's outputs.
    hidden: BTreeMap<String, bool>,
    toggled: BTreeMap<String, bool>,
    /// Narrator lines and tutorial prompts up to the save.
    narrator: Vec<String>,
    tutorials: Vec<i32>,
}

/// The walk's result.
struct Walk {
    /// What the first [`OPENING`] ticks of the fresh start showed.
    opening: Shown,
    saves: Vec<Save>,
    /// The story exit the walk ran into, if any.
    transition: Option<String>,
    ticks: u64,
    touched: usize,
    deaths: u32,
    checkpoints: usize,
    /// Checkpoints after the first that wrote no save.
    unsaved: Vec<String>,
}

/// Where to put the pawn to touch actor `id`: a trigger's centre, or the
/// centre of a volume's first hull.
fn trigger_point(game: &Game, id: u32) -> Option<Vec3> {
    let map = game.scene_map()?;
    if let Some(t) = map.actors.triggers.iter().find(|t| t.id == id) {
        return Some(t.location);
    }
    let v = map.actors.volumes.iter().find(|v| v.id == id)?;
    let h = v.hulls.first()?;
    Some((h.min + h.max) * 0.5)
}

fn place(game: &mut Game, at: Vec3) {
    let player = game.player_mut();
    player.position = at;
    player.velocity = Vec3::ZERO;
    player.pawn.force_floor_check = true;
}

/// The level started as the app starts it from `snapshot`.
fn start_from(
    dir: &Path,
    map: &str,
    snapshot: &Snapshot,
) -> (Game, LevelScript, i32, ScriptRestore) {
    let (mut game, script) = load_level_with_kismet(dir, map).unwrap();
    let mut script = script.unwrap();
    let index = game.apply_snapshot(snapshot).checkpoint_index;
    let restore = script.start_from_save(Some(snapshot), Some(index));
    game.start();
    (game, script, index, restore)
}

/// Plays `map` from a fresh start through its checkpoints (module docs).
fn walk(dir: &Path, map: &str) -> Walk {
    let fresh = Snapshot::fresh();
    let (mut game, mut script, index, restore) = start_from(dir, map, &fresh);
    assert_eq!(index, -1, "{map}");
    assert_eq!(restore, ScriptRestore::Nothing, "{map}");
    let mut out = Walk {
        opening: Shown::default(),
        saves: Vec::new(),
        transition: None,
        ticks: 0,
        touched: 0,
        deaths: 0,
        checkpoints: 0,
        unsaved: Vec::new(),
    };
    let mut all = Shown::default();
    let mut previous = fresh;
    let idle = InputFrame::default();

    // One tick of the walk; `false` when the walk is over (the game stopped
    // or the level's exit was taken).
    let tick = |game: &mut Game,
                script: &mut LevelScript,
                out: &mut Walk,
                all: &mut Shown,
                previous: &mut Snapshot,
                forced: bool|
     -> bool {
        out.ticks += 1;
        let Some(t) = step(game, script, &idle, all, out.ticks) else {
            return false;
        };
        if out.ticks <= OPENING {
            out.opening.note(out.ticks, &t);
        }
        if let Some(index) = checkpoint_saved(&t.report) {
            let mut snapshot = game.capture_snapshot(Some(previous));
            script.save_into(&mut snapshot);
            *previous = snapshot.clone();
            out.saves.push(Save {
                index,
                tick: out.ticks,
                forced,
                snapshot,
                game: game.clone(),
                script: script.clone(),
                hidden: all.hidden.clone(),
                toggled: all.toggled.clone(),
                narrator: all.narrator.clone(),
                tutorials: all.tutorials.clone(),
            });
        }
        if let Some(t) = all.transitions.iter().find(|t| t.starts_with("open ")) {
            out.transition = Some(t.clone());
            return false;
        }
        true
    };

    for _ in 0..OPENING {
        if !tick(
            &mut game,
            &mut script,
            &mut out,
            &mut all,
            &mut previous,
            false,
        ) {
            out.deaths = all.deaths;
            return out;
        }
    }

    let mut checkpoints = game.scene_map().unwrap().actors.checkpoints.clone();
    checkpoints.sort_by_key(|c| c.index);
    out.checkpoints = checkpoints.len();

    // The touch triggers, each with the checkpoint it lies nearest to.
    let graph = script.runtime().graph().clone();
    let exits: BTreeSet<usize> = smoke::exit_triggers(&graph)
        .into_iter()
        .map(|t| t.event)
        .collect();
    let mut triggers: Vec<(usize, f32, u32, Vec3)> = Vec::new();
    let mut seen_actors = BTreeSet::new();
    for n in &graph.nodes {
        if n.class != OpClass::Touch || exits.contains(&n.id) {
            continue;
        }
        let Some(id) = n
            .event
            .as_ref()
            .and_then(|e| e.originator.as_deref())
            .and_then(|p| graph.actor_by_path(p))
            .and_then(|r| script.world_id(r))
        else {
            continue;
        };
        if !seen_actors.insert(id) {
            continue;
        }
        let Some(point) = trigger_point(&game, id) else {
            continue;
        };
        let nearest = checkpoints
            .iter()
            .enumerate()
            .map(|(i, c)| (i, c.location.distance(point)))
            .min_by(|a, b| a.1.total_cmp(&b.1));
        if let Some((i, d)) = nearest {
            triggers.push((i, d, id, point));
        }
    }
    triggers.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.total_cmp(&b.1)));

    'walk: for (i, c) in checkpoints.iter().enumerate() {
        if i > 0 {
            // The checkpoint itself.
            place(&mut game, c.location);
            if !tick(
                &mut game,
                &mut script,
                &mut out,
                &mut all,
                &mut previous,
                false,
            ) {
                break 'walk;
            }
            if game.latest_checkpoint_index().is_none_or(|l| l < c.index)
                && game.trigger_checkpoint(c.id)
            {
                if !tick(
                    &mut game,
                    &mut script,
                    &mut out,
                    &mut all,
                    &mut previous,
                    true,
                ) {
                    break 'walk;
                }
            }
            for _ in 0..SETTLE {
                if !tick(
                    &mut game,
                    &mut script,
                    &mut out,
                    &mut all,
                    &mut previous,
                    false,
                ) {
                    break 'walk;
                }
            }
        }
        // The triggers around it.
        for (_, _, _, point) in triggers.iter().filter(|t| t.0 == i) {
            place(&mut game, *point);
            out.touched += 1;
            for _ in 0..SETTLE {
                if !tick(
                    &mut game,
                    &mut script,
                    &mut out,
                    &mut all,
                    &mut previous,
                    false,
                ) {
                    break 'walk;
                }
            }
        }
    }
    out.deaths = all.deaths;
    // The checkpoints that wrote no save, and why.
    for c in checkpoints.iter().skip(1) {
        if !out.saves.iter().any(|s| s.index == c.index) {
            out.unsaved.push(format!(
                "{} (index {}, level {}, {}{})",
                c.name,
                c.index,
                c.id >> 16,
                if c.enabled { "enabled" } else { "disabled" },
                if c.triggered_from_kismet {
                    ", triggered from Kismet"
                } else {
                    ""
                }
            ));
        }
    }
    out
}

/// Where two script states differ: paths, by kind of difference.
#[derive(Debug, Default)]
struct StateDiff {
    enabled: Vec<String>,
    counts: Vec<String>,
    variables: Vec<String>,
    playing: Vec<String>,
    positions: Vec<String>,
}

impl StateDiff {
    fn line(&self) -> String {
        format!(
            "bEnabled {}, trigger counts {}, variables {}, playing {}, positions {}",
            self.enabled.len(),
            self.counts.len(),
            self.variables.len(),
            self.playing.len(),
            self.positions.len()
        )
    }
}

fn short(path: &str) -> &str {
    path.split_once("Main_Sequence.").map_or(path, |p| p.1)
}

fn diff(a: &ScriptState, b: &ScriptState) -> StateDiff {
    let mut d = StateDiff::default();
    for (path, x) in &a.events {
        let Some(y) = b.events.get(path) else {
            continue;
        };
        if x.enabled != y.enabled {
            d.enabled
                .push(format!("{} {}->{}", short(path), x.enabled, y.enabled));
        }
        if x.trigger_count != y.trigger_count {
            d.counts.push(format!(
                "{} {}->{}",
                short(path),
                x.trigger_count,
                y.trigger_count
            ));
        }
    }
    for (path, x) in &a.variables {
        if let Some(y) = b.variables.get(path)
            && x != y
        {
            d.variables.push(format!("{} {x:?}->{y:?}", short(path)));
        }
    }
    for (path, x) in &a.matinees {
        let Some(y) = b.matinees.get(path) else {
            continue;
        };
        if x.playing != y.playing {
            d.playing
                .push(format!("{} {}->{}", short(path), x.playing, y.playing));
        } else if !x.playing && (x.position - y.position).abs() > 0.01 {
            d.positions
                .push(format!("{} {}->{}", short(path), x.position, y.position));
        }
    }
    d
}

/// Movers of `a` that are somewhere else in `b` (more than 2 uu, or turned
/// by more than 64 rotator units), with the distance.
fn mover_diff(a: &LevelScript, b: &LevelScript) -> Vec<(u32, f32)> {
    let mut out = Vec::new();
    for id in a.mover_ids() {
        let (Some((la, ra)), Some((lb, rb))) = (a.mover_transform(id), b.mover_transform(id))
        else {
            continue;
        };
        let turned = (0..3).any(|i| {
            let d = (ra[i] - rb[i]).rem_euclid(65_536);
            d.min(65_536 - d) > 64
        });
        let moved = la.distance(lb);
        if moved > 2.0 || turned {
            out.push((id, moved));
        }
    }
    out
}

fn abilities(game: &Game) -> (i32, bool, bool, bool) {
    let s = &game.player().script;
    (
        s.gun.max_grapples,
        s.boots.enabled,
        s.gun.can_grapple,
        game.in_story_mode(),
    )
}

fn collected(game: &Game) -> Vec<bool> {
    game.npcs()
        .map(|n| {
            n.runtime()
                .collectibles
                .iter()
                .map(|c| c.collected)
                .collect()
        })
        .unwrap_or_default()
}

/// The world's switches: volumes and triggers that collide, checkpoints
/// enabled, levels streamed.
fn switches(game: &Game) -> (Vec<bool>, Vec<bool>, Vec<bool>, u64) {
    let r = game.scene_runtime().unwrap();
    (
        r.volume_enabled.clone(),
        r.trigger_enabled.clone(),
        r.checkpoints.enabled.clone(),
        r.level_mask,
    )
}

fn count_diff(a: &[bool], b: &[bool]) -> usize {
    a.iter().zip(b).filter(|(x, y)| x != y).count()
}

/// The saves to compare: at most [`MAX_SAVES`], the first and the last
/// always.
fn pick(n: usize) -> Vec<usize> {
    if n <= MAX_SAVES {
        return (0..n).collect();
    }
    let mut out: Vec<usize> = (0..MAX_SAVES)
        .map(|i| i * (n - 1) / (MAX_SAVES - 1))
        .collect();
    out.dedup();
    out
}

/// Compares the level continued from `save` with the game that played on;
/// returns the rule violations found.
fn compare(dir: &Path, map: &str, walk: &Walk, save: &Save) -> Vec<String> {
    let mut wrong = Vec::new();
    let tag = format!("{map} checkpoint {}", save.index);
    let saved = save.script.runtime().capture_state();
    let (mut cg, mut cs, index, restore) = start_from(dir, map, &save.snapshot);
    if index != save.index {
        wrong.push(format!("{tag}: the load reports checkpoint {index}"));
    }
    if !matches!(restore, ScriptRestore::Saved { .. }) {
        wrong.push(format!("{tag}: the script was given {restore:?}"));
    }
    // Before the first tick: the pawn at the checkpoint's spawn point, the
    // saved abilities and collectibles.
    // (The reset moves the pawn to the nearest free spot where the spawn
    // point's cylinder does not fit: some tens of units at most.)
    let spawn = cg.scene_respawn_point().map(|p| p.0);
    let at = cg.player().position;
    if spawn.is_none_or(|s| s.distance(at) > 64.0) {
        wrong.push(format!(
            "{tag}: the pawn starts at {at:?}, spawn point {spawn:?}"
        ));
    }
    if cg.latest_checkpoint_index() != Some(save.index) {
        wrong.push(format!(
            "{tag}: latest checkpoint {:?}",
            cg.latest_checkpoint_index()
        ));
    }
    if collected(&cg) != collected(&save.game) {
        wrong.push(format!("{tag}: collectibles differ from the saved ones"));
    }
    let errors_before = save.script.runtime().errors().len();

    let (mut ug, mut us) = (save.game.clone(), save.script.clone());
    let (mut c, mut u) = (Shown::default(), Shown::default());
    let idle = InputFrame::default();
    let mut first: Option<StateDiff> = None;
    let mut abilities_first = None;
    let mut script_first: Option<LevelScript> = None;
    for tick in 1..=WINDOW {
        let ct = step(&mut cg, &mut cs, &idle, &mut c, tick);
        let ut = step(&mut ug, &mut us, &idle, &mut u, tick);
        if ct.is_none() {
            wrong.push(format!("{tag}: the continued game stopped at tick {tick}"));
            break;
        }
        if ut.is_none() {
            break;
        }
        if tick == 1 {
            first = Some(diff(&saved, &cs.runtime().capture_state()));
            abilities_first = Some(abilities(&cg));
            script_first = Some(cs.clone());
        }
    }
    let first = first.unwrap_or_default();
    let last = diff(&us.runtime().capture_state(), &cs.runtime().capture_state());
    let movers = mover_diff(&us, &cs);
    // Movers at rest (where they were at the save after the whole stretch:
    // an opened door, a parked lift) must be where the save left them.
    let at_rest: BTreeSet<u32> = save
        .script
        .mover_ids()
        .into_iter()
        .filter(|id| !mover_diff(&save.script, &us).iter().any(|m| m.0 == *id))
        .collect();
    // Those that move in the continued game are on a Matinee that runs
    // here and not there (a loop the level-loaded chain starts on every
    // load, which the game that played on had stopped): named, not wrong.
    let moving_here: BTreeSet<u32> = script_first
        .as_ref()
        .map(|f| mover_diff(f, &cs).into_iter().map(|m| m.0).collect())
        .unwrap_or_default();
    let name = |id: u32| {
        cs.actor_refs(id)
            .first()
            .and_then(|r| cs.runtime().graph().actor(*r))
            .map_or_else(|| id.to_string(), |a| a.name.clone())
    };
    let restarted: Vec<(String, f32)> = movers
        .iter()
        .filter(|m| at_rest.contains(&m.0) && moving_here.contains(&m.0))
        .map(|m| (name(m.0), m.1))
        .collect();
    let rest_elsewhere: Vec<(String, f32)> = movers
        .iter()
        .filter(|m| at_rest.contains(&m.0) && !moving_here.contains(&m.0))
        .map(|m| (name(m.0), m.1))
        .collect();
    // What was said or prompted before the save and comes again.
    let said_again: Vec<&String> = c
        .narrator
        .iter()
        .filter(|l| save.narrator.contains(l) && !u.narrator.contains(l))
        .collect();
    let prompted_again: Vec<i32> = c
        .tutorials
        .iter()
        .copied()
        .filter(|t| save.tutorials.contains(t) && !u.tutorials.contains(t))
        .collect();
    let (uv, ut, uc, um) = switches(&ug);
    let (cv, ct, cc, cm) = switches(&cg);

    // What is on screen: hidden and toggled actors of the game that played
    // on (from the walk's outputs to the save, then its own) against the
    // continued game's.
    let mut u_hidden = save.hidden.clone();
    u_hidden.extend(u.hidden.clone());
    let mut u_toggled = save.toggled.clone();
    u_toggled.extend(u.toggled.clone());
    let hidden_lost: Vec<&String> = u_hidden
        .iter()
        .filter(|(a, h)| **h && !c.hidden.get(*a).copied().unwrap_or(false))
        .map(|(a, _)| a)
        .collect();
    let shown_lost: Vec<&String> = u_hidden
        .iter()
        .filter(|(a, h)| !**h && c.hidden.get(*a).copied().unwrap_or(false))
        .map(|(a, _)| a)
        .collect();
    let toggles_lost = u_toggled
        .iter()
        .filter(|(a, on)| c.toggled.get(*a) != Some(*on))
        .count();

    eprintln!(
        "  checkpoint {} (walk tick {}{}): saved {} ev / {} var / {} mat",
        save.index,
        save.tick,
        if save.forced { ", triggered" } else { "" },
        saved.events.len(),
        saved.variables.len(),
        saved.matinees.len()
    );
    eprintln!("    continued shows: {}", c.view_line());
    eprintln!("    played-on shows: {}", u.view_line());
    eprintln!(
        "    continued: narrator {:?}, tutorials {:?}, transitions {:?}, deaths {}",
        c.narrator, c.tutorials, c.transitions, c.deaths
    );
    eprintln!(
        "    played-on: narrator {:?}, tutorials {:?}, transitions {:?}, deaths {}",
        u.narrator, u.tutorials, u.transitions, u.deaths
    );
    eprintln!(
        "    after tick 1 against the save: {} {:?} {:?} {:?} {:?}",
        first.line(),
        first.enabled.iter().take(4).collect::<Vec<_>>(),
        first.counts.iter().take(6).collect::<Vec<_>>(),
        first.variables.iter().take(6).collect::<Vec<_>>(),
        first.playing.iter().take(6).collect::<Vec<_>>(),
    );
    eprintln!(
        "    after {WINDOW} ticks against the game that played on: {} {:?} {:?} {:?} {:?} {:?}",
        last.line(),
        last.enabled.iter().take(4).collect::<Vec<_>>(),
        last.counts.iter().take(6).collect::<Vec<_>>(),
        last.variables.iter().take(6).collect::<Vec<_>>(),
        last.playing.iter().take(6).collect::<Vec<_>>(),
        last.positions.iter().take(6).collect::<Vec<_>>(),
    );
    eprintln!(
        "    world: movers elsewhere {} (at rest in the game that played on and at rest elsewhere \
         here: {} {:?}; at rest there and moving here: {} {:?}); volumes {} triggers {} checkpoints \
         {} differ; levels {um:#x}/{cm:#x}; hidden in the game that played on and not here {} {:?}; shown \
         there and hidden here {} {:?}; toggles not as there {}",
        movers.len(),
        rest_elsewhere.len(),
        rest_elsewhere.iter().take(6).collect::<Vec<_>>(),
        restarted.len(),
        restarted.iter().take(6).collect::<Vec<_>>(),
        count_diff(&uv, &cv),
        count_diff(&ut, &ct),
        count_diff(&uc, &cc),
        hidden_lost.len(),
        hidden_lost.iter().take(5).collect::<Vec<_>>(),
        shown_lost.len(),
        shown_lost.iter().take(5).collect::<Vec<_>>(),
        toggles_lost,
    );
    eprintln!(
        "    abilities (limit, boots, latch, story mode): saved {:?}, continued after tick 1 {:?}, \
         after {WINDOW} {:?}, played on {:?}",
        abilities(&save.game),
        abilities_first,
        abilities(&cg),
        abilities(&ug)
    );

    eprintln!(
        "    said before the save and again: {said_again:?}; prompted again: {prompted_again:?}"
    );

    // Rule 1: the view. Kinds the game that played on shows too are a
    // cutscene in progress at the save; the rest must not be the opening's.
    for (kind, ticks) in &c.view {
        if u.view.contains_key(kind) {
            continue;
        }
        if walk.opening.view.contains_key(kind) {
            wrong.push(format!(
                "{tag}: {kind} at ticks {:?}.. as in the opening, not in the game that played on",
                &ticks[..ticks.len().min(4)]
            ));
        }
    }
    // Rule 2: the opening's narration.
    for line in &c.narrator {
        if walk.opening.narrator.contains(line) && !u.narrator.contains(line) {
            wrong.push(format!(
                "{tag}: narrator line {line:?} of the opening again"
            ));
        }
    }
    // Rule 3: the saved variables are the continued game's after its first
    // tick (events and Matinees are printed above: the level-loaded chain
    // and what the pawn touches at the spawn point change them at once).
    if !first.variables.is_empty() {
        wrong.push(format!(
            "{tag}: variables not as saved after the first tick: {:?}",
            first.variables
        ));
    }
    if !rest_elsewhere.is_empty() {
        wrong.push(format!(
            "{tag}: movers at rest are not where the save left them: {rest_elsewhere:?}"
        ));
    }
    // Rule 4: abilities as saved (story mode is the level start's).
    let a_saved = abilities(&save.game);
    let a_now = abilities(&cg);
    let a_on = abilities(&ug);
    if (a_now.0, a_now.1, a_now.2) != (a_on.0, a_on.1, a_on.2) {
        wrong.push(format!(
            "{tag}: abilities {a_now:?}, the game that played on has {a_on:?} (saved {a_saved:?})"
        ));
    }
    // Rule 6.
    if !c.problems.is_empty() {
        wrong.push(format!("{tag}: non-finite {:?}", c.problems));
    }
    let errors = cs.runtime().errors();
    if errors.len() > errors_before.max(us.runtime().errors().len()) {
        wrong.push(format!(
            "{tag}: script errors {:?}",
            errors.iter().rev().take(3).collect::<Vec<_>>()
        ));
    }

    // Rule 5: a death.
    let respawn = cg.scene_respawn_point().map(|p| p.0);
    cg.kill_player();
    let mut d = Shown::default();
    for tick in 1..=240 {
        if step(&mut cg, &mut cs, &idle, &mut d, tick).is_none() {
            break;
        }
    }
    if d.respawns == 0 {
        wrong.push(format!("{tag}: no respawn after a death"));
    }
    if cg.latest_checkpoint_index() != Some(save.index) {
        wrong.push(format!("{tag}: a death changed the latest checkpoint"));
    }
    if cg.scene_respawn_point().map(|p| p.0) != respawn {
        wrong.push(format!("{tag}: a death moved the respawn point"));
    }
    for kind in ["cut", "cinematic", "view target"] {
        if d.view.contains_key(kind)
            && walk.opening.view.contains_key(kind)
            && !u.view.contains_key(kind)
            && !c.view.contains_key(kind)
        {
            wrong.push(format!("{tag}: {kind} after a death, as in the opening"));
        }
    }
    for line in &d.narrator {
        if walk.opening.narrator.contains(line) {
            wrong.push(format!(
                "{tag}: narrator line {line:?} of the opening after a death"
            ));
        }
    }
    eprintln!(
        "    a death: respawns {}, shows: {}, narrator {:?}",
        d.respawns,
        d.view_line(),
        d.narrator
    );
    wrong
}

#[test]
#[ignore = "walks every story map and continues from its checkpoints; run with --ignored"]
fn every_story_map_continues_without_its_opening() {
    let Some(dir) = converted_dir() else {
        eprintln!("SKIP: ASAMU_CONVERTED_DIR (a conversion with Kismet and Matinee) not set");
        return;
    };
    let only = std::env::var("ASAMU_CONTINUE_MAP").ok();
    let mut wrong = Vec::new();
    let mut compared = 0;
    for chapter in ChapterId::ALL {
        let map = chapter.map_name();
        if only
            .as_deref()
            .is_some_and(|o| !o.eq_ignore_ascii_case(map))
        {
            continue;
        }
        if load_level_with_kismet(&dir, map).is_err() {
            eprintln!("SKIP: {map} is not converted");
            continue;
        }
        let w = walk(&dir, map);
        eprintln!(
            "{map}: walk of {} ticks, {} triggers touched, {} deaths, {} of {} checkpoints saved \
             {:?}, exit {:?}",
            w.ticks,
            w.touched,
            w.deaths,
            w.saves.len(),
            w.checkpoints,
            w.saves.iter().map(|s| s.index).collect::<Vec<_>>(),
            w.transition
        );
        if !w.unsaved.is_empty() {
            eprintln!("  no save at: {:?}", w.unsaved);
        }
        eprintln!(
            "  fresh start shows: {}; narrator {:?}; tutorials {:?}",
            w.opening.view_line(),
            w.opening.narrator,
            w.opening.tutorials
        );
        for i in pick(w.saves.len()) {
            wrong.extend(compare(&dir, map, &w, &w.saves[i]));
            compared += 1;
        }
        // The first and a late save, for a look at them in the app.
        let mut out = vec![0, w.saves.len() * 2 / 3];
        out.dedup();
        for i in out {
            if let Some(save) = w.saves.get(i) {
                write_save(map, &format!("{map}-cp{}", save.index), &save.snapshot);
            }
        }
    }
    eprintln!("{compared} saves compared");
    assert!(
        wrong.is_empty(),
        "{} findings:\n{}",
        wrong.len(),
        wrong.join("\n")
    );
}

/// The way out of a level after a Continue: the story chain from the level
/// continued at its first saved checkpoint and at a late one.
#[test]
#[ignore = "walks every story map and follows its story exit from continued saves; run with --ignored"]
fn every_story_map_can_be_finished_after_a_continue() {
    let Some(dir) = converted_dir() else {
        eprintln!("SKIP: ASAMU_CONVERTED_DIR (a conversion with Kismet and Matinee) not set");
        return;
    };
    let only = std::env::var("ASAMU_CONTINUE_MAP").ok();
    let mut wrong = Vec::new();
    let mut followed = 0;
    for chapter in &ChapterId::ALL[..6] {
        let map = chapter.map_name();
        if only
            .as_deref()
            .is_some_and(|o| !o.eq_ignore_ascii_case(map))
        {
            continue;
        }
        if load_level_with_kismet(&dir, map).is_err() {
            eprintln!("SKIP: {map} is not converted");
            continue;
        }
        let expected = chapter.next().map(ChapterId::map_name);
        let fresh = smoke::chain_step(&dir, map, expected, 7_200).unwrap();
        eprintln!(
            "{map}: fresh start reaches {:?} in {} ticks",
            fresh.reached, fresh.ticks
        );
        let w = walk(&dir, map);
        if w.saves.is_empty() {
            eprintln!("{map}: the walk wrote no save");
            continue;
        }
        let mut picks = vec![0, w.saves.len() * 2 / 3, w.saves.len() - 1];
        picks.dedup();
        for i in picks {
            let save = &w.saves[i];
            let step = smoke::chain_step_from(&dir, map, expected, 7_200, &|game, script| {
                let index = game.apply_snapshot(&save.snapshot).checkpoint_index;
                script.start_from_save(Some(&save.snapshot), Some(index));
            })
            .unwrap();
            followed += 1;
            eprintln!(
                "{map}: continued at checkpoint {} reaches {:?} in {} ticks (triggers {:?}, \
                 enabled by touch {:?}, by toggle {:?}, direct touch {}, interactions fired {} \
                 injected {}, streamed {:?}, credits {}), notes {:?}",
                save.index,
                step.reached,
                step.ticks,
                step.triggers,
                step.enabled_by_touch,
                step.enabled,
                step.direct_touch,
                step.interactions_fired.len(),
                step.interactions_injected.len(),
                step.streamed,
                step.credits,
                step.notes
            );
            if fresh.ok() && !step.ok() {
                wrong.push(format!(
                    "{map} continued at checkpoint {}: reached {:?}, expected {:?}",
                    save.index, step.reached, step.expected
                ));
            }
            // No more help than the fresh start needed.
            if step.direct_touch && !fresh.direct_touch {
                wrong.push(format!(
                    "{map} continued at checkpoint {}: a touch had to be sent to Kismet directly",
                    save.index
                ));
            }
            if step.enabled_by_toggle && !fresh.enabled_by_toggle {
                wrong.push(format!(
                    "{map} continued at checkpoint {}: the exit had to be enabled by hand {:?}",
                    save.index, step.enabled
                ));
            }
            if step.interactions_injected.len() > fresh.interactions_injected.len() {
                wrong.push(format!(
                    "{map} continued at checkpoint {}: interactions injected {:?}",
                    save.index, step.interactions_injected
                ));
            }
        }
    }
    eprintln!("{followed} continued games followed to their exit");
    assert!(
        wrong.is_empty(),
        "{} findings:\n{}",
        wrong.len(),
        wrong.join("\n")
    );
}

/// AG-IceCave streams `TheCore` in from a one-time touch trigger. Where that
/// trigger lies among the level's checkpoints, and what a Continue from a
/// checkpoint saved after it gives: the restored trigger count keeps the
/// touch event from firing again (KISMET_RUNTIME.md §12), so the sub-level
/// is only there if the load brings it back.
#[test]
#[ignore = "walks AG-IceCave past its streaming trigger; run with --ignored"]
fn icecave_continued_after_the_core_was_streamed_in() {
    let Some(dir) = converted_dir() else {
        eprintln!("SKIP: ASAMU_CONVERTED_DIR (a conversion with Kismet and Matinee) not set");
        return;
    };
    const MAP: &str = "AG-IceCave";
    const CORE: &str = "TheCore";
    if load_level_with_kismet(&dir, MAP).is_err() {
        eprintln!("SKIP: {MAP} is not converted");
        return;
    }
    let fresh = Snapshot::fresh();
    let (mut game, mut script, _, _) = start_from(&dir, MAP, &fresh);
    let idle = InputFrame::default();
    let mut shown = Shown::default();
    let mut ticks = 0;
    for _ in 0..OPENING {
        ticks += 1;
        step(&mut game, &mut script, &idle, &mut shown, ticks).unwrap();
    }
    let graph = script.runtime().graph().clone();
    let stream = smoke::exit_triggers(&graph)
        .into_iter()
        .find(|t| t.exit.starts_with("stream"))
        .expect("a trigger that streams a level in");
    let trigger_id = script
        .world_id(stream.actor)
        .expect("the trigger is in the world");
    let at = trigger_point(&game, trigger_id).expect("the trigger has a place");
    let mut checkpoints = game.scene_map().unwrap().actors.checkpoints.clone();
    checkpoints.sort_by_key(|c| c.index);
    let mut by_distance: Vec<(i32, f32)> = checkpoints
        .iter()
        .map(|c| (c.index, c.location.distance(at)))
        .collect();
    by_distance.sort_by(|a, b| a.1.total_cmp(&b.1));
    eprintln!(
        "{}: at {at:?}; nearest checkpoints (index, distance) {:?}; the last three checkpoints {:?}",
        stream.path,
        &by_distance[..by_distance.len().min(4)],
        checkpoints
            .iter()
            .rev()
            .take(3)
            .map(|c| (c.index, c.location, c.location.distance(at)))
            .collect::<Vec<_>>()
    );

    // The trigger lies beyond the last checkpoint: of all checkpoints the
    // last is the nearest to it, far nearer than the one before. So in play
    // no checkpoint is saved after it, and a Continue starts before it with
    // its touch event unspent (the chain test continues from that save).
    let last_index = checkpoints.last().expect("checkpoints").index;
    assert_eq!(by_distance[0].0, last_index, "{by_distance:?}");
    assert!(by_distance[1].1 > 2.0 * by_distance[0].1, "{by_distance:?}");

    // The case play does not produce (the last checkpoint missed on the way
    // and touched only after the trigger), measured, not asserted: every
    // checkpoint but the last, the trigger, then the last.
    let mut previous = fresh;
    let save_at = |game: &mut Game,
                   script: &mut LevelScript,
                   shown: &mut Shown,
                   ticks: &mut u64,
                   previous: &mut Snapshot|
     -> Option<i32> {
        *ticks += 1;
        let t = step(game, script, &idle, shown, *ticks)?;
        let index = checkpoint_saved(&t.report)?;
        let mut snapshot = game.capture_snapshot(Some(previous));
        script.save_into(&mut snapshot);
        *previous = snapshot;
        Some(index)
    };
    let last = checkpoints.last().expect("checkpoints").clone();
    for c in checkpoints.iter().skip(1) {
        if c.index == last.index {
            break;
        }
        place(&mut game, c.location);
        save_at(
            &mut game,
            &mut script,
            &mut shown,
            &mut ticks,
            &mut previous,
        );
    }
    place(&mut game, at);
    for _ in 0..600 {
        ticks += 1;
        step(&mut game, &mut script, &idle, &mut shown, ticks).unwrap();
    }
    let streamed = game.is_level_streamed(CORE) && script.runtime().is_level_attached(CORE);
    eprintln!(
        "after the trigger: {CORE} streamed in {streamed}, trigger count of its touch event {:?}",
        script
            .runtime()
            .capture_state()
            .events
            .get(&graph.node(stream.event).unwrap().path)
            .map(|e| e.trigger_count)
    );
    assert!(streamed, "the trigger streams {CORE} in");
    // The last checkpoint, reached with the sub-level in.
    place(&mut game, last.location);
    let saved = save_at(
        &mut game,
        &mut script,
        &mut shown,
        &mut ticks,
        &mut previous,
    );
    eprintln!(
        "the last checkpoint ({}) saved after the trigger: {saved:?}",
        last.index
    );
    if saved.is_none() {
        return;
    }
    let (mut cg, mut cs, index, _) = start_from(&dir, MAP, &previous);
    let mut c = Shown::default();
    for tick in 1..=WINDOW {
        step(&mut cg, &mut cs, &idle, &mut c, tick).unwrap();
    }
    let here = cg.is_level_streamed(CORE) && cs.runtime().is_level_attached(CORE);
    eprintln!(
        "continued at checkpoint {index}, saved after the trigger: {CORE} streamed in {here}; \
         deaths {}; shows {}",
        c.deaths,
        c.view_line()
    );
    let expected = ChapterId::IceCave.next().map(ChapterId::map_name);
    let chain = smoke::chain_step_from(&dir, MAP, expected, 7_200, &|game, script| {
        let index = game.apply_snapshot(&previous).checkpoint_index;
        script.start_from_save(Some(&previous), Some(index));
    })
    .unwrap();
    eprintln!(
        "continued at checkpoint {index}: the chain reaches {:?} (triggers {:?}, direct touch {}, \
         enabled by toggle {} {:?}, streamed {:?}, credits {}), notes {:?}",
        chain.reached,
        chain.triggers,
        chain.direct_touch,
        chain.enabled_by_toggle,
        chain.enabled,
        chain.streamed,
        chain.credits,
        chain.notes
    );
}

/// AG-BeautifulCity's first checkpoint is not touched: the "End" key of the
/// first-encounter cutscene's Matinee triggers it, while that Matinee is
/// still playing. What the save taken there holds, and what a Continue from
/// it says and does again.
#[test]
#[ignore = "plays AG-BeautifulCity's first cutscene to its checkpoint; run with --ignored"]
fn beautiful_city_continued_at_the_checkpoint_its_cutscene_triggers() {
    let Some(dir) = converted_dir() else {
        eprintln!("SKIP: ASAMU_CONVERTED_DIR (a conversion with Kismet and Matinee) not set");
        return;
    };
    const MAP: &str = "AG-BeautifulCity";
    if load_level_with_kismet(&dir, MAP).is_err() {
        eprintln!("SKIP: {MAP} is not converted");
        return;
    }
    let fresh = Snapshot::fresh();
    let (mut game, mut script, _, _) = start_from(&dir, MAP, &fresh);
    let idle = InputFrame::default();
    let mut before = Shown::default();
    let mut ticks = 0;
    for _ in 0..OPENING {
        ticks += 1;
        step(&mut game, &mut script, &idle, &mut before, ticks).unwrap();
    }
    // The trigger of the first encounter: the touch event whose links reach
    // the action that triggers a checkpoint through a Matinee's event key.
    let graph = script.runtime().graph().clone();
    let trigger = graph
        .nodes
        .iter()
        .filter(|n| n.class == OpClass::Touch && n.path.contains("Cutscenes"))
        .filter_map(|n| {
            let actor = n.event.as_ref()?.originator.as_deref()?;
            let id = script.world_id(graph.actor_by_path(actor)?)?;
            // Touch -> (look-at) -> Matinee -> TriggerCheckpoint, at most
            // three ops away.
            let mut frontier = vec![n.id];
            for _ in 0..3 {
                frontier = frontier
                    .iter()
                    .filter_map(|i| graph.node(*i))
                    .flat_map(|x| x.outputs.iter().flat_map(|o| o.links.iter().map(|l| l.0)))
                    .collect();
                if frontier.iter().any(|i| {
                    graph
                        .node(*i)
                        .is_some_and(|x| x.class_name.ends_with("SeqAct_TriggerCheckpoint"))
                }) {
                    return Some((n.path.clone(), id));
                }
            }
            None
        })
        .next()
        .expect("the first encounter's trigger");
    let at = trigger_point(&game, trigger.1).expect("the trigger has a place");
    place(&mut game, at);
    let mut saved = None;
    let mut previous = fresh;
    for _ in 0..12_000 {
        ticks += 1;
        let t = step(&mut game, &mut script, &idle, &mut before, ticks).unwrap();
        if let Some(index) = checkpoint_saved(&t.report) {
            let mut snapshot = game.capture_snapshot(Some(&previous));
            script.save_into(&mut snapshot);
            previous = snapshot;
            saved = Some((index, ticks));
            break;
        }
    }
    let Some((index, tick)) = saved else {
        panic!("{}: no checkpoint was triggered in 200 s", trigger.0);
    };
    write_save(MAP, "AG-BeautifulCity-cutscene", &previous);
    let state = script.runtime().capture_state();
    let playing: Vec<(&str, f32)> = state
        .matinees
        .iter()
        .filter(|(_, m)| m.playing)
        .map(|(p, m)| (short(p), m.position))
        .collect();
    eprintln!(
        "{}: checkpoint {index} saved at tick {tick}; said before it {:?}; Matinees playing at \
         the save {playing:?}; abilities {:?}",
        short(&trigger.0),
        before.narrator,
        abilities(&game)
    );
    // Continue, next to the game that plays on.
    let (mut cg, mut cs, loaded, _) = start_from(&dir, MAP, &previous);
    assert_eq!(loaded, index);
    let (mut c, mut u) = (Shown::default(), Shown::default());
    for tick in 1..=3_600 {
        step(&mut cg, &mut cs, &idle, &mut c, tick).unwrap();
        step(&mut game, &mut script, &idle, &mut u, tick).unwrap();
    }
    eprintln!(
        "continued, 60 s: says {:?}; shows {}; outputs {:?}; abilities {:?}",
        c.narrator,
        c.view_line(),
        c.kinds,
        abilities(&cg)
    );
    eprintln!(
        "played on, 60 s: says {:?}; shows {}; outputs {:?}; abilities {:?}",
        u.narrator,
        u.view_line(),
        u.kinds,
        abilities(&game)
    );
    let again: Vec<&String> = c
        .narrator
        .iter()
        .filter(|l| before.narrator.contains(l))
        .collect();
    eprintln!("said before the save and again after Continue: {again:?}");

    // What is certain: the level's own script saved the level's first
    // registered checkpoint while a cutscene Matinee was playing; the
    // continued game keeps the player's view and the saved abilities, and
    // says nothing the cutscene had not keyed.
    assert_eq!(index, 1);
    assert!(
        playing.iter().any(|(p, _)| p.starts_with("Cutscenes.")),
        "{playing:?}"
    );
    for kind in c.view.keys() {
        assert!(u.view.contains_key(kind), "{kind}: {}", c.view_line());
    }
    let (a, b) = (abilities(&cg), abilities(&game));
    assert_eq!((a.0, a.1, a.2), (b.0, b.1, b.2));
    for line in &c.narrator {
        assert!(
            before.narrator.contains(line) || u.narrator.contains(line),
            "{line}: not a line of the cutscene"
        );
    }
}
