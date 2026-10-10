//! Runtime particle effects: the placed `Emitter` actors of the current
//! converted map (and its streamed sub-levels), simulated on the CPU by
//! `asamu_assets::particles` and drawn as camera-facing quads.
//!
//! Flow (every frame, `Update`):
//!
//! 1. A new level (the converted level's name changes or it is re-planned)
//!    drops the current effects.
//! 2. Once the level's render plan is spawned ([`RenderLevels`] exists),
//!    `particles/particles.json`, the placement files of the level and its
//!    sub-levels and the material/texture manifests load on the async pool.
//! 3. Each placement with a converted template becomes a
//!    [`SystemInstance`]; `bAutoActivate` placements start active (the
//!    stock `Emitter` sets its `bCurrentlyActive` from it at
//!    `PostBeginPlay`).
//! 4. **Kismet hook**: the plugin reads [`KismetFrame`] itself (the
//!    orchestrator's router needs no change). `SeqAct_Toggle` on an emitter
//!    (`Output::ActorToggled`) and Matinee toggle keys (`ETTA_On` / `Off` /
//!    `Toggle`, `Output::MatineeKey`) turn the matching effects on or off the
//!    way the stock `Emitter.OnToggle` does: *on* activates (an effect that
//!    is still running starts its emitters' clocks over and keeps what is
//!    on screen, as the engine's `ActivateSystem` does), *off* deactivates
//!    (live particles finish unless an emitter's `bKillOnDeactivate`),
//!    *toggle* activates when the effect is not active or is deactivating.
//!    `SeqAct_Destroy` (`Output::ActorDestroyed`) stops the effect. Toggles
//!    that arrive before the effects finished loading are kept and applied
//!    once they exist. Actors hidden or destroyed by Kismet
//!    ([`Presentation::hidden`]) are not drawn; actors Kismet moves carry
//!    their effects; effects of sub-levels that are not streamed in are not
//!    drawn.
//! 5. The effects advance in fixed 1/60 s steps (deterministic for a given
//!    frame-time sequence and seed), with the camera's distance choosing
//!    each system's LOD, and their sprites are rebuilt into one mesh per
//!    emitter.
//!
//! Rendering (ours, an approximation; evidence in
//! `docs/reverse-engineering/PARTICLES.md`): one unlit sprite material per
//! particle material (displayed texture × colour × vertex colour, opacity
//! from a mask texture channel, the UE3 blend mode); a sprite's size is its
//! full quad extent (CONFIRMED from the shipped sprite vertex shader);
//! sub-UV blending shows the nearer image; mesh emitters draw as sprites and
//! beams as straight strips; no soft-particle depth fade, distortion,
//! lighting or fog.

mod material;
mod render;

use std::collections::HashMap;
use std::sync::Arc;

use asamu_assets::particles::{
    ActivityState, ParticleLibrary, ParticleMaterial, ParticleMaterials, Placement, SystemInstance,
    load_placements,
};
use asamu_core::glam as sim_glam;
use asamu_game::asamu_kismet::{Output, ToggleMode};
use bevy::prelude::*;
use bevy::tasks::futures::check_ready;
use bevy::tasks::{AsyncComputeTaskPool, Task};

use crate::converted::{ConvertedLevel, LevelPhase, RenderLevels};
use crate::kismet::{KismetFrame, Presentation};
use crate::{PlayerCamera, Sim};

/// Fixed simulation step (seconds).
pub const STEP: f32 = 1.0 / 60.0;
/// Most simulation steps per frame (a long frame is not caught up beyond).
const MAX_STEPS_PER_FRAME: u32 = 6;
/// Seed of the first effect (each placement adds its index).
const SEED: u64 = 0x0A5A_4D55_5041_5254;

/// Runtime particle effects.
pub struct ParticlesPlugin;

impl Plugin for ParticlesPlugin {
    fn build(&self, app: &mut App) {
        material::register(app);
        app.init_resource::<ParticleRuntime>().add_systems(
            Update,
            (
                reset_on_level_change,
                start_loading,
                restart_on_new_game,
                poll_loading,
                route_kismet,
                simulate,
                render::draw,
            )
                .chain(),
        );
    }
}

/// One placed effect.
pub struct Effect {
    /// The placement (actor, transform, template).
    pub placement: Placement,
    /// Plan level index (0 = persistent).
    pub level: usize,
    /// Level package name.
    pub level_name: String,
    /// The running system.
    pub instance: SystemInstance,
    /// `Emitter.bCurrentlyActive`.
    pub currently_active: bool,
    /// The placement's transform (UE space) before any Kismet move.
    pub rest: sim_glam::Mat4,
    /// Mesh entity per emitter (spawned on first draw).
    pub entities: Vec<Option<Entity>>,
}

/// Loading state.
#[derive(Default)]
enum Phase {
    /// Nothing loaded for the current level.
    #[default]
    Idle,
    /// Loading on the async pool.
    Loading(Task<Result<Loaded, String>>),
    /// Effects spawned.
    Ready,
    /// No converted particles (nothing to do until the level changes).
    Off,
}

/// What the loader produces.
pub struct Loaded {
    library: Arc<ParticleLibrary>,
    placements: Vec<(usize, String, Placement)>,
    materials: HashMap<String, ParticleMaterial>,
}

/// The current level's particle effects.
#[derive(Resource, Default)]
pub struct ParticleRuntime {
    phase: Phase,
    /// Level the effects belong to.
    level: Option<String>,
    /// The library (kept across levels).
    library: Option<Arc<ParticleLibrary>>,
    /// Effects.
    pub effects: Vec<Effect>,
    /// Particle materials by lower-case material path.
    pub materials: HashMap<String, ParticleMaterial>,
    /// Bevy material handles by lower-case material path.
    pub handles: HashMap<String, Handle<material::SpriteMaterial>>,
    /// Image handles by converted file name and colour space.
    pub images: material::ImageCache,
    /// Simulation time not yet stepped.
    accumulator: f32,
    /// Placements whose template was not converted.
    pub missing_templates: usize,
    /// Toggles that arrived before the effects were loaded (a level's
    /// first Kismet frames can precede the asynchronous load); applied once
    /// the effects exist.
    pending: Vec<(String, ToggleMode)>,
}

/// Most toggles kept while the effects load.
const MAX_PENDING_TOGGLES: usize = 1024;

impl ParticleRuntime {
    /// Live particles over every effect.
    pub fn particle_count(&self) -> usize {
        self.effects
            .iter()
            .map(|e| e.instance.particle_count())
            .sum()
    }

    fn clear(&mut self, commands: &mut Commands) {
        for e in self.effects.drain(..) {
            for ent in e.entities.into_iter().flatten() {
                commands.entity(ent).despawn();
            }
        }
        self.handles.clear();
        self.materials.clear();
        // The next level loads its own textures; holding the handles would
        // keep every visited level's particle textures in memory.
        self.images.clear();
        self.accumulator = 0.0;
        self.missing_templates = 0;
        self.pending.clear();
        self.phase = Phase::Idle;
    }

    /// Apply a toggle to every effect of `actor` (a qualified object path,
    /// compared without case). Returns how many effects matched.
    pub fn toggle_actor(&mut self, actor: &str, mode: ToggleMode) -> usize {
        let mut n = 0;
        for e in &mut self.effects {
            if !e.placement.actor_path.eq_ignore_ascii_case(actor) {
                continue;
            }
            n += 1;
            apply_toggle(e, mode);
        }
        n
    }
}

/// `Emitter.OnToggle` (read locally in the shipped script: on activates,
/// off deactivates, toggle activates when spawning is suppressed or the
/// emitter is not currently active).
pub fn apply_toggle(e: &mut Effect, mode: ToggleMode) {
    match mode {
        ToggleMode::On => {
            // `ActivateSystem` on a component that ran before initializes
            // its emitter instances again (CONFIRMED from the executable):
            // clocks, loops and bursts start over, live particles stay.
            e.instance.activate();
            e.currently_active = true;
        }
        ToggleMode::Off => {
            e.instance.deactivate();
            e.currently_active = false;
        }
        ToggleMode::Toggle => {
            let suppressed = e.instance.state != ActivityState::Active;
            if suppressed || !e.currently_active {
                e.instance.activate();
                e.currently_active = true;
            } else {
                e.instance.deactivate();
                e.currently_active = false;
            }
        }
    }
}

fn reset_on_level_change(
    mut commands: Commands,
    level: Option<Res<ConvertedLevel>>,
    mut rt: ResMut<ParticleRuntime>,
) {
    let Some(level) = level else {
        return;
    };
    let replanning = matches!(level.phase, Some(LevelPhase::Planning(_)));
    let renamed = rt
        .level
        .as_deref()
        .is_some_and(|l| !l.eq_ignore_ascii_case(&level.level));
    if (replanning && !matches!(rt.phase, Phase::Idle)) || renamed {
        info!(
            "particles: level changed to {}; dropping effects",
            level.level
        );
        rt.clear(&mut commands);
        rt.level = None;
    }
}

fn start_loading(
    level: Option<Res<ConvertedLevel>>,
    levels: Option<Res<RenderLevels>>,
    mut rt: ResMut<ParticleRuntime>,
) {
    if !matches!(rt.phase, Phase::Idle) {
        return;
    }
    let (Some(level), Some(levels)) = (level, levels) else {
        return;
    };
    if !matches!(level.phase, Some(LevelPhase::Spawned(_))) {
        return;
    }
    // The plan's level list is inserted by a deferred command: wait until it
    // describes this level (not the previous one).
    if !levels
        .names
        .first()
        .is_some_and(|n| n.eq_ignore_ascii_case(&level.level))
    {
        return;
    }
    let root = level.dir.root().to_path_buf();
    let names = levels.names.clone();
    let dir = level.dir.clone();
    let cached = rt.library.clone();
    rt.level = Some(level.level.clone());
    rt.phase = Phase::Loading(AsyncComputeTaskPool::get().spawn(async move {
        let library = match cached {
            Some(l) => l,
            None => match ParticleLibrary::load(&root).map_err(|e| e.to_string())? {
                Some(l) => Arc::new(l),
                None => return Err("no converted particles (asamu-import particles)".to_owned()),
            },
        };
        let mut placements = Vec::new();
        for (i, name) in names.iter().enumerate() {
            match load_placements(&root, name) {
                Ok(Some(list)) => {
                    placements.extend(list.into_iter().map(|p| (i, name.clone(), p)));
                }
                Ok(None) => {}
                Err(e) => warn!("particles: placements of {name}: {e}"),
            }
        }
        let manifests = dir.load_manifests().map_err(|e| e.to_string())?;
        let described = ParticleMaterials::load(&root).map_err(|e| e.to_string())?;
        let mut materials = HashMap::new();
        for (_, _, p) in &placements {
            let Some(def) = p.template.as_deref().and_then(|t| library.get(t)) else {
                continue;
            };
            for e in &def.emitters {
                for l in &e.lods {
                    let Some(m) = &l.required.material else {
                        continue;
                    };
                    materials
                        .entry(m.to_ascii_lowercase())
                        .or_insert_with(|| described.get(m, manifests.textures.as_ref()));
                }
            }
        }
        Ok(Loaded {
            library,
            placements,
            materials,
        })
    }));
}

fn poll_loading(mut rt: ResMut<ParticleRuntime>) {
    let Phase::Loading(task) = &mut rt.phase else {
        return;
    };
    let Some(result) = check_ready(task) else {
        return;
    };
    match result {
        Ok(loaded) => {
            rt.library = Some(loaded.library.clone());
            rt.materials = loaded.materials;
            rt.effects.clear();
            let mut missing = 0usize;
            for (i, (level, name, p)) in loaded.placements.into_iter().enumerate() {
                let Some(def) = p.template.as_deref().and_then(|t| loaded.library.get(t)) else {
                    missing += 1;
                    continue;
                };
                let rest = p.matrix();
                let mut instance =
                    SystemInstance::new(def.clone(), rest, p.params(), SEED.wrapping_add(i as u64));
                instance.kill_on_deactivate = p.kill_on_deactivate;
                let active = p.auto_activate;
                if active {
                    instance.activate();
                }
                let emitters = def.emitters.len();
                rt.effects.push(Effect {
                    placement: p,
                    level,
                    level_name: name,
                    instance,
                    currently_active: active,
                    rest,
                    entities: vec![None; emitters],
                });
            }
            rt.missing_templates = missing;
            let unsupported: std::collections::BTreeMap<String, usize> = rt
                .effects
                .iter()
                .flat_map(|e| e.instance.def.unsupported.iter())
                .fold(Default::default(), |mut m, (k, v)| {
                    *m.entry(k.clone()).or_default() += v;
                    m
                });
            info!(
                "particles: {} effects ({} active, {} placements without a converted template), \
                 {} materials; modules not simulated: {:?}",
                rt.effects.len(),
                rt.effects.iter().filter(|e| e.currently_active).count(),
                missing,
                rt.materials.len(),
                unsupported
            );
            rt.phase = Phase::Ready;
            for (actor, mode) in std::mem::take(&mut rt.pending) {
                rt.toggle_actor(&actor, mode);
            }
        }
        Err(e) => {
            info!("particles: {e}");
            rt.phase = Phase::Off;
        }
    }
}

/// A new game on the same map (the level's entities are kept) puts every
/// effect back to its level-start state.
fn restart_on_new_game(sim: Option<Res<Sim>>, mut rt: ResMut<ParticleRuntime>) {
    let Some(sim) = sim else { return };
    if !sim.is_added() || !matches!(rt.phase, Phase::Ready) {
        return;
    }
    for e in &mut rt.effects {
        e.instance.transform = e.rest;
        // Drop the emitter instances: the restarted level replays the same
        // particle sequence as its first run.
        e.instance.reset();
        e.currently_active = e.placement.auto_activate;
        if e.currently_active {
            e.instance.activate();
        }
    }
    rt.accumulator = 0.0;
}

/// The toggle a Kismet output asks of an actor, if any.
fn toggle_of(out: &Output) -> Option<(&str, ToggleMode)> {
    match out {
        Output::ActorToggled { actor, mode } => Some((actor.as_str(), *mode)),
        Output::MatineeKey {
            actor: Some(actor),
            action,
            ..
        } => {
            let mode = match action.as_str() {
                "ETTA_On" => ToggleMode::On,
                "ETTA_Off" => ToggleMode::Off,
                "ETTA_Toggle" => ToggleMode::Toggle,
                _ => return None,
            };
            Some((actor.as_str(), mode))
        }
        Output::ActorDestroyed { actor } => Some((actor.as_str(), ToggleMode::Off)),
        _ => None,
    }
}

/// The Kismet hook: toggles from `SeqAct_Toggle`, Matinee toggle keys and
/// `SeqAct_Destroy`. Toggles that arrive while the effects are still
/// loading are kept and applied once they exist.
fn route_kismet(mut frames: MessageReader<KismetFrame>, mut rt: ResMut<ParticleRuntime>) {
    for frame in frames.read() {
        for (actor, mode) in frame.outputs.iter().filter_map(toggle_of) {
            if matches!(rt.phase, Phase::Ready) {
                let n = rt.toggle_actor(actor, mode);
                if n > 0 {
                    debug!("particles: {actor} toggled {mode:?} ({n} effects)");
                }
            } else if !matches!(rt.phase, Phase::Off) && rt.pending.len() < MAX_PENDING_TOGGLES {
                rt.pending.push((actor.to_owned(), mode));
            }
        }
    }
}

/// World actor id of an effect's actor in the running game.
fn actor_id(sim: &Sim, e: &Effect) -> Option<u32> {
    let map = sim.game.scene_map()?;
    let level = map.level_index(&e.level_name)?;
    asamu_game::asamu_world::scene::actor_id(u8::try_from(level).ok()?, e.placement.slot)
}

/// UE rotation matrix (actor axes as columns) of a rotator.
fn rotation(r: [i32; 3]) -> sim_glam::Mat3 {
    let rows = asamu_game::asamu_world::rotation::rotation_rows(r);
    sim_glam::Mat3::from_cols(rows[0].as_vec3(), rows[1].as_vec3(), rows[2].as_vec3())
}

fn simulate(
    time: Res<Time>,
    sim: Option<Res<Sim>>,
    camera: Query<&GlobalTransform, With<PlayerCamera>>,
    mut rt: ResMut<ParticleRuntime>,
    mut report: Local<f32>,
) {
    if !matches!(rt.phase, Phase::Ready) {
        return;
    }
    let paused = sim
        .as_ref()
        .is_some_and(|s| s.game.state() != asamu_game::GameState::Playing);
    if paused {
        return;
    }
    // Effects follow the actors Kismet moved.
    if let Some(sim) = sim.as_ref()
        && let Some(script) = sim.script.as_ref()
    {
        let moved: HashMap<u32, (sim_glam::Vec3, [i32; 3])> = script
            .moved_actors()
            .into_iter()
            .map(|(id, l, r)| (id, (l, r)))
            .collect();
        if !moved.is_empty() {
            for e in &mut rt.effects {
                let Some(id) = actor_id(sim, e) else { continue };
                let (Some((loc, rot)), Some((old_loc, old_rot))) =
                    (moved.get(&id), script.actor_placement(id))
                else {
                    continue;
                };
                let linear = rotation(*rot) * rotation(old_rot).transpose();
                let delta = sim_glam::Mat4::from_translation(*loc)
                    * sim_glam::Mat4::from_mat3(linear)
                    * sim_glam::Mat4::from_translation(-old_loc);
                e.instance.transform = delta * e.rest;
            }
        }
    }
    *report += time.delta_secs();
    if *report >= 10.0 {
        *report = 0.0;
        debug!(
            "particles: {} live particles in {} effects",
            rt.particle_count(),
            rt.effects.len()
        );
    }
    let viewer = camera.iter().next().map(|g| {
        let p = g.translation();
        asamu_core::coords::bevy_pos_to_ue(sim_glam::Vec3::new(p.x, p.y, p.z), crate::SCALE)
    });
    rt.accumulator = (rt.accumulator + time.delta_secs()).min(STEP * MAX_STEPS_PER_FRAME as f32);
    let mut steps = 0;
    while rt.accumulator >= STEP && steps < MAX_STEPS_PER_FRAME {
        rt.accumulator -= STEP;
        steps += 1;
        for e in &mut rt.effects {
            e.instance.tick(STEP, viewer);
            finished(e);
        }
    }
}

/// `Emitter.OnParticleSystemFinished` (read locally in the shipped script):
/// when the system completes on its own (its component deactivates it; see
/// `SystemInstance::has_completed`), the actor is no longer "currently
/// active", so the next *Toggle* turns it on again.
fn finished(e: &mut Effect) {
    if e.currently_active && e.instance.state != ActivityState::Active {
        e.currently_active = false;
    }
}

/// Whether an effect is drawn this frame: not hidden by Kismet or by its
/// placement, and its sub-level streamed in.
fn visible(
    e: &Effect,
    sim: Option<&Sim>,
    pres: Option<&Presentation>,
    levels: Option<&RenderLevels>,
) -> bool {
    if e.placement.hidden_game || e.placement.actor_hidden {
        return false;
    }
    let Some(sim) = sim else {
        return true;
    };
    if let (Some(pres), Some(id)) = (pres, actor_id(sim, e))
        && pres.hidden.contains(&id)
    {
        return false;
    }
    if let Some(levels) = levels
        && e.level > 0
        && !levels.force_all
        && !levels.always_loaded.get(e.level).copied().unwrap_or(true)
        && !sim.game.is_level_streamed(&e.level_name)
    {
        return false;
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use asamu_assets::particles::{InstanceParams, SystemDef};

    fn effect() -> Effect {
        let v = serde_json_value();
        let def = Arc::new(SystemDef::from_json("T.PS", &v));
        let rest = sim_glam::Mat4::IDENTITY;
        Effect {
            placement: Placement {
                actor: "Emitter_0".into(),
                actor_path: "AG-T.TheWorld.PersistentLevel.Emitter_0".into(),
                actor_class: "Engine.Emitter".into(),
                slot: 3,
                actor_hidden: false,
                moved_by_matinee: false,
                local_to_world: [
                    [1.0, 0.0, 0.0, 0.0],
                    [0.0, 1.0, 0.0, 0.0],
                    [0.0, 0.0, 1.0, 0.0],
                    [0.0, 0.0, 0.0, 1.0],
                ],
                template: Some("T.PS".into()),
                auto_activate: false,
                hidden_game: false,
                kill_on_deactivate: false,
                kill_on_completed: false,
                warmup_time: 0.0,
                instance_parameters: Vec::new(),
            },
            level: 0,
            level_name: "AG-T".into(),
            instance: SystemInstance::new(def, rest, InstanceParams::default(), 1),
            currently_active: false,
            rest,
            entities: vec![None],
        }
    }

    fn serde_json_value() -> asamu_assets::particles::JsonValue {
        asamu_assets::particles::JsonValue::Null
    }

    /// `Emitter.OnToggle` semantics: on, off, toggle from both states.
    #[test]
    fn toggles_follow_the_stock_emitter() {
        let mut e = effect();
        apply_toggle(&mut e, ToggleMode::On);
        assert!(e.currently_active);
        assert_eq!(e.instance.state, ActivityState::Active);
        apply_toggle(&mut e, ToggleMode::Toggle);
        assert!(!e.currently_active);
        assert_eq!(e.instance.state, ActivityState::Deactivating);
        apply_toggle(&mut e, ToggleMode::Toggle);
        assert!(e.currently_active, "a deactivating emitter toggles back on");
        assert_eq!(e.instance.state, ActivityState::Active);
        apply_toggle(&mut e, ToggleMode::Off);
        assert!(!e.currently_active);
        apply_toggle(&mut e, ToggleMode::Off);
        assert!(!e.currently_active, "off twice stays off");
        let mut rt = ParticleRuntime::default();
        rt.effects.push(effect());
        assert_eq!(
            rt.toggle_actor("ag-t.theworld.persistentlevel.emitter_0", ToggleMode::On),
            1
        );
        assert_eq!(
            rt.toggle_actor("AG-T.TheWorld.PersistentLevel.Emitter_9", ToggleMode::On),
            0
        );
        assert!(rt.effects[0].currently_active);
    }

    /// The Kismet hook end to end: frames written the way `main.rs` writes
    /// them reach the effects through the message queue.
    #[test]
    fn kismet_frames_toggle_and_destroy_effects() {
        let mut app = App::new();
        app.add_message::<KismetFrame>()
            .init_resource::<ParticleRuntime>()
            .add_systems(Update, route_kismet);
        {
            let mut rt = app.world_mut().resource_mut::<ParticleRuntime>();
            rt.effects.push(effect());
            rt.phase = Phase::Ready;
        }
        let actor = "AG-T.TheWorld.PersistentLevel.Emitter_0".to_owned();
        let frame = |outputs: Vec<Output>| KismetFrame {
            outputs,
            npc_events: Vec::new(),
        };
        app.world_mut()
            .write_message(frame(vec![Output::ActorToggled {
                actor: actor.clone(),
                mode: ToggleMode::On,
            }]));
        app.update();
        let state = |app: &App| {
            let rt = app.world().resource::<ParticleRuntime>();
            (rt.effects[0].currently_active, rt.effects[0].instance.state)
        };
        assert_eq!(state(&app), (true, ActivityState::Active));
        // A Matinee toggle key turns it off; an unrelated actor is ignored.
        app.world_mut().write_message(frame(vec![
            Output::MatineeKey {
                node: 7,
                actor: Some(actor.clone()),
                action: "ETTA_Off".to_owned(),
            },
            Output::ActorToggled {
                actor: "AG-T.TheWorld.PersistentLevel.PointLight_3".to_owned(),
                mode: ToggleMode::On,
            },
        ]));
        app.update();
        assert_eq!(state(&app), (false, ActivityState::Deactivating));
        app.world_mut()
            .write_message(frame(vec![Output::ActorToggled {
                actor: actor.clone(),
                mode: ToggleMode::Toggle,
            }]));
        app.update();
        assert_eq!(state(&app), (true, ActivityState::Active));
        // SeqAct_Destroy stops the effect.
        app.world_mut()
            .write_message(frame(vec![Output::ActorDestroyed { actor }]));
        app.update();
        assert!(!state(&app).0);
    }

    /// Toggles that arrive while the effects load are applied once ready.
    #[test]
    fn toggles_before_the_load_are_kept() {
        let mut app = App::new();
        app.add_message::<KismetFrame>()
            .init_resource::<ParticleRuntime>()
            .add_systems(Update, route_kismet);
        app.world_mut().write_message(KismetFrame {
            outputs: vec![Output::ActorToggled {
                actor: "AG-T.TheWorld.PersistentLevel.Emitter_0".to_owned(),
                mode: ToggleMode::On,
            }],
            npc_events: Vec::new(),
        });
        app.update();
        let mut rt = app.world_mut().resource_mut::<ParticleRuntime>();
        assert_eq!(rt.pending.len(), 1);
        // What `poll_loading` does when the effects arrive.
        rt.effects.push(effect());
        rt.phase = Phase::Ready;
        for (actor, mode) in std::mem::take(&mut rt.pending) {
            rt.toggle_actor(&actor, mode);
        }
        assert!(rt.effects[0].currently_active);
    }

    /// *Turn On* on an effect that is already running is `ActivateSystem`
    /// on a component with live emitter instances: the clocks and bursts
    /// start over and the particles on screen stay.
    #[test]
    fn turning_on_a_running_effect_restarts_it_without_clearing() {
        let v: asamu_assets::particles::JsonValue = r#"{"params": {}, "emitters": [{
            "name": "E", "kind": "sprite", "params": {}, "lods": [{
                "level": 0, "enabled": true,
                "required": {"class": "ParticleModuleRequired", "enabled": true, "params": {
                    "EmitterDuration": 1.0, "EmitterLoops": 0}},
                "spawn": {"class": "ParticleModuleSpawn", "enabled": true, "params": {
                    "Rate": {"dist": "float", "value": {"kind": "constant", "value": [0.0], "locked_axes": 0}},
                    "RateScale": {"dist": "float", "value": {"kind": "constant", "value": [1.0], "locked_axes": 0}},
                    "BurstList": [{"Count": 4, "CountLow": -1, "Time": 0.0}]}},
                "type_data": null,
                "modules": [{"class": "ParticleModuleLifetime", "enabled": true, "spawn": true, "update": false,
                    "params": {"Lifetime": {"dist": "float", "value": {"kind": "constant", "value": [100.0], "locked_axes": 0}}}}]
            }]}]}"#
            .parse()
            .unwrap_or_else(|e| panic!("{e}"));
        let mut e = effect();
        e.instance = SystemInstance::new(
            Arc::new(SystemDef::from_json("T.PS", &v)),
            e.rest,
            InstanceParams::default(),
            1,
        );
        apply_toggle(&mut e, ToggleMode::On);
        e.instance.tick(STEP, None);
        assert_eq!(e.instance.particle_count(), 4, "the burst at time 0");
        e.instance.tick(STEP, None);
        assert_eq!(e.instance.particle_count(), 4, "one burst per loop");
        apply_toggle(&mut e, ToggleMode::On);
        assert_eq!(e.instance.particle_count(), 4, "nothing is cleared");
        e.instance.tick(STEP, None);
        assert_eq!(e.instance.particle_count(), 8, "the burst fires again");
        // Off, then Toggle: back on, the remaining particles still there.
        apply_toggle(&mut e, ToggleMode::Off);
        apply_toggle(&mut e, ToggleMode::Toggle);
        assert!(e.currently_active);
        assert_eq!(e.instance.particle_count(), 8);
    }

    /// A one-shot effect that ran out is no longer "currently active": the
    /// next *Toggle* starts it again instead of switching it off.
    #[test]
    fn a_finished_effect_toggles_back_on() {
        let v: asamu_assets::particles::JsonValue = r#"{"params": {}, "emitters": [{
            "name": "E", "kind": "sprite", "params": {}, "lods": [{
                "level": 0, "enabled": true,
                "required": {"class": "ParticleModuleRequired", "enabled": true, "params": {
                    "EmitterDuration": 0.25, "EmitterLoops": 1}},
                "spawn": {"class": "ParticleModuleSpawn", "enabled": true, "params": {
                    "Rate": {"dist": "float", "value": {"kind": "constant", "value": [60.0], "locked_axes": 0}},
                    "RateScale": {"dist": "float", "value": {"kind": "constant", "value": [1.0], "locked_axes": 0}}}},
                "type_data": null,
                "modules": [{"class": "ParticleModuleLifetime", "enabled": true, "spawn": true, "update": false,
                    "params": {"Lifetime": {"dist": "float", "value": {"kind": "constant", "value": [0.1], "locked_axes": 0}}}}]
            }]}]}"#
            .parse()
            .unwrap_or_else(|e| panic!("{e}"));
        let mut e = effect();
        e.instance = SystemInstance::new(
            Arc::new(SystemDef::from_json("T.PS", &v)),
            e.rest,
            InstanceParams::default(),
            1,
        );
        apply_toggle(&mut e, ToggleMode::Toggle);
        assert!(e.currently_active);
        let mut peak = 0;
        for _ in 0..60 {
            e.instance.tick(STEP, None);
            finished(&mut e);
            peak = peak.max(e.instance.particle_count());
        }
        assert!(peak > 0);
        assert_eq!(e.instance.state, ActivityState::Inactive);
        assert!(!e.currently_active, "the system finished");
        apply_toggle(&mut e, ToggleMode::Toggle);
        assert!(e.currently_active);
        assert_eq!(e.instance.state, ActivityState::Active);
        e.instance.tick(STEP, None);
        assert!(e.instance.particle_count() > 0, "it runs again");
    }

    #[test]
    fn hidden_placements_are_not_drawn() {
        let mut e = effect();
        assert!(visible(&e, None, None, None));
        e.placement.hidden_game = true;
        assert!(!visible(&e, None, None, None));
    }
}
