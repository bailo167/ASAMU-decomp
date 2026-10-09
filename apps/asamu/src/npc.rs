//! NPCs, story actors and skinned meshes (`docs/reverse-engineering/NPCS.md`).
//!
//! On a converted level, once the level's game has loaded, this plugin:
//!
//! - spawns an [`asamu_game::npc::NpcSystem`] from the converted scene (on the
//!   async pool) and drives it after every simulation tick (**interim**: the
//!   system belongs inside `Game::tick`, between the map actors and the
//!   player's controller; see the wiring notes in `asamu_game::npc`; until
//!   then the worm's push reaches the player one tick later than in the
//!   original);
//! - renders the placed skinned actors (villagers, Maddie, the worm) from the
//!   importer's skeletal glTF output with Bevy skins and an `AnimationPlayer`
//!   ([`skins`]): the component's own looping animation when the scene
//!   provides it, the worm's animation from its state machine, a stand-in
//!   idle otherwise;
//! - renders the first-person hands (`PlayerHand`) on an overlay camera with
//!   the original's mesh FOV (70°) and picks the hand animation from the
//!   player's state ([`hands`]);
//! - draws state gizmos for collectibles, story interactables, glow flowers
//!   and the worm (F10, with the other level gizmos).
//!
//! Needs `asamu-import levels` and `asamu-import skeletal` output; without the
//! skeletal manifest nothing is rendered (the simulation still runs).

mod hands;
mod skins;

use asamu_core::glam as sim_glam;
use asamu_game::npc::{
    NpcEvent, NpcOptions, NpcSystem, SkeletalIndex, StoryItemStateName, WormStateName,
    death_notifies_npcs, load_skeletal_index,
};
use bevy::ecs::message::{MessageCursor, Messages};
use bevy::prelude::*;
use bevy::tasks::futures::check_ready;
use bevy::tasks::{AsyncComputeTaskPool, Task};

use crate::ui::GameTick;
use crate::{LevelGizmos, Sim, converted, to_render};

/// NPC and skinned-mesh rendering/animation (hands, Maddie, villagers, worm).
pub struct NpcPlugin;

impl Plugin for NpcPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<NpcLoad>()
            .add_systems(
                Update,
                (
                    begin_npc_load,
                    finish_npc_load,
                    skins::spawn_skins,
                    skins::attach_animations,
                    skins::update_skin_animations,
                    hands::spawn_hands,
                    hands::tag_overlay_meshes,
                    hands::update_hands,
                    draw_npc_gizmos,
                )
                    .chain(),
            )
            .add_systems(
                FixedPostUpdate,
                drive_npcs
                    .run_if(resource_exists::<Sim>)
                    .run_if(resource_exists::<NpcWorld>),
            );
    }
}

/// The background load (restarted when the game changes map).
#[derive(Resource, Default)]
struct NpcLoad {
    /// Map the current load (or loaded system) belongs to.
    map: Option<String>,
    task: Option<Task<NpcLoaded>>,
}

struct NpcLoaded {
    system: NpcSystem,
    skeletal: Result<SkeletalIndex, String>,
}

/// The running NPC system and the skeletal index.
#[derive(Resource)]
pub(crate) struct NpcWorld {
    /// NPC state machines.
    pub system: NpcSystem,
    /// Converted skeletal meshes (`None` when not converted).
    pub skeletal: Option<SkeletalIndex>,
    /// The map's skinned actors have been spawned.
    pub skins_spawned: bool,
    last_tick: u64,
    last_respawns: u32,
}

fn begin_npc_load(
    mut commands: Commands,
    mut load: ResMut<NpcLoad>,
    sim: Option<Res<Sim>>,
    level: Option<Res<converted::ConvertedLevel>>,
    skins: Query<(Entity, &skins::NpcSkin)>,
) {
    let Some(level) = level else {
        return;
    };
    // The game's map when it runs (its level list gives the actor ids), else
    // the rendered level (fly mode: NPCs shown, not simulated; the same
    // level indexing, so the ids agree once the game starts).
    let (map_name, levels) = match sim.as_ref().and_then(|s| s.game.scene_map()) {
        Some(m) => (m.map.clone(), Some(m.levels.clone())),
        None => (level.level.clone(), None),
    };
    if load
        .map
        .as_deref()
        .is_some_and(|m| m.eq_ignore_ascii_case(&map_name))
    {
        return;
    }
    // A (new) map: drop the previous map's NPCs, load this one's.
    if load.map.is_some() {
        commands.remove_resource::<NpcWorld>();
        for (e, skin) in &skins {
            if !skin.is_hands() {
                commands.entity(e).despawn();
            }
        }
    }
    load.map = Some(map_name.clone());
    let root = level.dir.root().to_path_buf();
    load.task = Some(AsyncComputeTaskPool::get().spawn(async move {
        let system = match levels {
            Some(levels) => NpcSystem::load_from_dir(&root, &levels, NpcOptions::default()),
            None => NpcSystem::load_for_map(&root, &map_name, NpcOptions::default())
                .unwrap_or_else(|e| {
                    warn!("NPCs of {map_name}: {e}");
                    NpcSystem::default()
                }),
        };
        NpcLoaded {
            system,
            skeletal: load_skeletal_index(&root).map_err(|e| e.to_string()),
        }
    }));
}

fn finish_npc_load(mut commands: Commands, mut load: ResMut<NpcLoad>, sim: Option<Res<Sim>>) {
    let Some(task) = load.task.as_mut() else {
        return;
    };
    let Some(loaded) = check_ready(task) else {
        return;
    };
    load.task = None;
    let scene = loaded.system.scene();
    info!(
        "NPCs: {} worm(s), {} collectibles, {} story items, {} glow flowers, {} foliage, {} skinned actors{}",
        scene.worms.len(),
        scene.collectibles.len(),
        scene.story_items.len(),
        scene.flowers.len(),
        scene.foliage.len(),
        scene.skinned.len(),
        if scene.warnings.is_empty() {
            String::new()
        } else {
            format!(
                " ({} warnings, e.g. {:?})",
                scene.warnings.len(),
                scene.warnings.first()
            )
        }
    );
    let skeletal = match loaded.skeletal {
        Ok(index) => Some(index),
        Err(e) => {
            info!(
                "no converted skeletal meshes ({e}); run `asamu-import skeletal` to see NPCs and hands"
            );
            None
        }
    };
    let (last_tick, last_respawns) = sim
        .map(|s| (s.game.clock().tick(), s.game.respawn_count()))
        .unwrap_or_default();
    commands.insert_resource(NpcWorld {
        system: loaded.system,
        skeletal,
        skins_spawned: false,
        last_tick,
        last_respawns,
    });
}

/// Runs the NPCs after each game tick (interim placement, see module docs).
fn drive_npcs(
    mut sim: ResMut<Sim>,
    mut world: ResMut<NpcWorld>,
    reports: Option<Res<Messages<GameTick>>>,
    mut cursor: Local<MessageCursor<GameTick>>,
) {
    // Death causes of the game ticks since the last run: only kill-zone
    // deaths reach the worm's `NotifyKilled` (CONFIRMED (src); a KillZ fall
    // or a scripted death does not). Read before the early return so the
    // cursor never falls behind.
    let mut notify_kill = false;
    if let Some(reports) = reports.as_deref() {
        for GameTick(report) in cursor.read(reports) {
            notify_kill |= report.died.is_some_and(death_notifies_npcs);
        }
    }
    let tick = sim.game.clock().tick();
    if tick <= world.last_tick {
        return;
    }
    let steps = (tick - world.last_tick).min(4);
    world.last_tick = tick;
    let dt = sim.game.clock().dt();
    let mut log: Vec<NpcEvent> = Vec::new();
    // Respawn teleports restart the touches.
    if sim.game.respawn_count() != world.last_respawns {
        world.last_respawns = sim.game.respawn_count();
        world.system.on_player_respawned();
    }
    let dying = sim.game.is_dying();
    if notify_kill {
        log.extend(world.system.notify_player_killed());
    }
    // Handler calls and interactions of the tick.
    let events = *sim.game.last_events();
    log.extend(world.system.apply_sim_events(&events));
    let mut kill = false;
    for _ in 0..steps {
        let report = world.system.tick_actors(sim.game.player_mut(), dt);
        kill |= report.kill_player;
        log.extend(report.events);
    }
    let (before, after) = (sim.prev_position, sim.curr_position);
    let touches = world
        .system
        .update_touches(before, after, sim.game.params());
    log.extend(touches);
    if kill && !dying {
        // The worm's own `PlayerDied` (its `NotifyKilled` is already applied).
        sim.game.kill_player();
    }
    for e in log {
        match e {
            NpcEvent::FoliageTouched { .. } | NpcEvent::GlowFlowerGlow { .. } => {
                debug!("npc: {e:?}");
            }
            _ => info!("npc: {e:?}"),
        }
    }
}

/// Gizmos for the NPC-side state (F10 with the other level gizmos).
fn draw_npc_gizmos(
    world: Option<Res<NpcWorld>>,
    level_gizmos: Option<Res<LevelGizmos>>,
    mut gizmos: Gizmos,
) {
    let (Some(world), Some(on)) = (world, level_gizmos) else {
        return;
    };
    if !on.0 {
        return;
    }
    let scene = world.system.scene();
    let rt = world.system.runtime();
    let scale = crate::SCALE.bevy_units_per_uu;
    for (c, s) in scene.collectibles.iter().zip(&rt.collectibles) {
        let color = if s.collected {
            Color::srgb(0.35, 0.35, 0.35)
        } else {
            Color::srgb(1.0, 0.85, 0.2)
        };
        gizmos.sphere(to_render(c.trigger.center), c.trigger.radius * scale, color);
    }
    for (d, s) in scene.story_items.iter().zip(&rt.story_items) {
        let color = match s.state {
            StoryItemStateName::Disabled => Color::srgb(0.3, 0.3, 0.3),
            _ => Color::srgb(0.3 + 0.7 * s.glow, 0.3 + 0.7 * s.glow, 0.3),
        };
        gizmos.sphere(to_render(d.location), 30.0 * scale, color);
    }
    for (d, s) in scene.flowers.iter().zip(&rt.flowers) {
        let a = s.alpha.clamp(0.0, 1.0);
        gizmos.sphere(
            to_render(d.location),
            25.0 * scale,
            Color::srgb(0.2, 0.3 + 0.7 * a, 0.3 + 0.7 * a),
        );
    }
    for (d, s) in scene.worms.iter().zip(&rt.worms) {
        let color = match s.state {
            WormStateName::Screaming => Color::srgb(1.0, 0.1, 0.1),
            WormStateName::Alerted => Color::srgb(1.0, 0.6, 0.1),
            WormStateName::Awake | WormStateName::WakingUp => Color::srgb(1.0, 1.0, 0.3),
            _ => Color::srgb(0.3, 0.3, 0.6),
        };
        let at = to_render(d.location);
        gizmos.sphere(at, 400.0 * scale, color);
        if s.current_aim != sim_glam::Vec3::ZERO {
            gizmos.line(at, to_render(s.current_aim), color);
        }
    }
    for v in &scene.worm_volumes {
        for h in &v.hulls {
            let centre = (h.min + h.max) * 0.5;
            let size = asamu_core::coords::ue_extents_to_bevy(h.max - h.min, crate::SCALE);
            gizmos.cube(
                Transform::from_translation(to_render(centre))
                    .with_scale(Vec3::new(size.x, size.y, size.z)),
                Color::srgb(0.6, 0.2, 0.8),
            );
        }
    }
}
