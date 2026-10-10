//! NPCs, story actors and skinned meshes (`docs/reverse-engineering/NPCS.md`).
//!
//! On a converted level this plugin:
//!
//! - presents the game's [`asamu_game::npc::NpcSystem`] (the game ticks it
//!   inside its frame, between the map actors and the player's controller,
//!   and the Kismet router in `crate::kismet` routes its events; see
//!   `asamu_game::npc` and `docs/INTEGRATION.md`); in fly mode, with no
//!   game, a system is loaded from the converted scene only to show the
//!   NPCs;
//! - renders the placed skinned actors (villagers, Maddie, the worm) from the
//!   importer's skeletal glTF output with Bevy skins and an `AnimationPlayer`
//!   ([`skins`]): each actor follows the simulation's animation node (its
//!   component's own sequence, or the one Matinee's animation tracks set),
//!   the worm its state machine, a stand-in idle otherwise; look-at controls
//!   turn the head and eye bones towards the player (`SeqAct_SetLookAtTarget`
//!   actors) or the worm's aim;
//! - renders the first-person hands (`PlayerHand`) on an overlay camera with
//!   the original's mesh FOV (70°) and picks the hand animation from the
//!   player's state ([`hands`]);
//! - draws state gizmos for collectibles, story interactables, glow flowers
//!   and the worm (F10, with the other level gizmos).
//!
//! Needs `asamu-import levels` and `asamu-import skeletal` output (and
//! `asamu-import matinee` for the actors' own animations, notifies and
//! look-at controls); without the skeletal manifest nothing is rendered (the
//! simulation still runs).

mod hands;
mod skins;

use asamu_core::glam as sim_glam;
use asamu_game::npc::{
    NpcOptions, NpcSystem, SkeletalIndex, StoryItemStateName, WormStateName, load_skeletal_index,
};
use bevy::prelude::*;
use bevy::tasks::futures::check_ready;
use bevy::tasks::{AsyncComputeTaskPool, Task};

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
            // Look-at after the animation pose, before transforms propagate.
            .add_systems(
                PostUpdate,
                skins::apply_look_at
                    .after(bevy::app::AnimationSystems)
                    .before(bevy::transform::TransformSystems::Propagate),
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
    /// Fly mode only: a system to show the NPCs without a game.
    system: Option<NpcSystem>,
    skeletal: Result<SkeletalIndex, String>,
}

/// The skeletal index and, without a game, the NPCs to show.
#[derive(Resource)]
pub(crate) struct NpcWorld {
    /// Fly mode: NPC definitions and rest state (the game's own system is
    /// used whenever a game runs).
    fallback: Option<NpcSystem>,
    /// Converted skeletal meshes (`None` when not converted).
    pub skeletal: Option<SkeletalIndex>,
    /// The map's skinned actors have been spawned.
    pub skins_spawned: bool,
}

impl NpcWorld {
    /// The NPC system to present: the running game's, else the fly-mode
    /// fallback.
    #[must_use]
    pub fn system<'a>(&'a self, sim: Option<&'a Sim>) -> Option<&'a NpcSystem> {
        sim.and_then(|s| s.game.npcs()).or(self.fallback.as_ref())
    }
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
    let (map_name, running) = match sim.as_ref().and_then(|s| s.game.scene_map()) {
        Some(m) => (m.map.clone(), true),
        None => (level.level.clone(), false),
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
        // A running game has its own system (`load_level_with_kismet`).
        let system = (!running).then(|| {
            NpcSystem::load_for_map(&root, &map_name, NpcOptions::default()).unwrap_or_else(|e| {
                warn!("NPCs of {map_name}: {e}");
                NpcSystem::default()
            })
        });
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
    let Some(system) = loaded
        .system
        .as_ref()
        .or(sim.as_ref().and_then(|s| s.game.npcs()))
    else {
        return;
    };
    let scene = system.scene();
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
    commands.insert_resource(NpcWorld {
        fallback: loaded.system,
        skeletal,
        skins_spawned: false,
    });
}

/// Gizmos for the NPC-side state (F10 with the other level gizmos).
fn draw_npc_gizmos(
    world: Option<Res<NpcWorld>>,
    sim: Option<Res<Sim>>,
    level_gizmos: Option<Res<LevelGizmos>>,
    mut gizmos: Gizmos,
) {
    let (Some(world), Some(on)) = (world, level_gizmos) else {
        return;
    };
    if !on.0 {
        return;
    }
    let Some(system) = world.system(sim.as_deref()) else {
        return;
    };
    let scene = system.scene();
    let rt = system.runtime();
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
