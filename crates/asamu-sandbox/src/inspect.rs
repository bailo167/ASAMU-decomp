//! Read-only views of a session, for a user interface.
//!
//! An [`Inspection`] borrows the game, its level script and the session and
//! hands out plain, owned views (no references into the simulation), so a
//! panel, a HUD or a JSON dump can show a consistent picture without being
//! able to change anything. Every mutation goes through
//! [`Session::execute`](crate::session::Session::execute) instead.
//!
//! The views describe this recreation as it runs now. Nothing in them is a
//! measurement of the original, and a value shown as "classic" is the
//! Classic parameter set's value with its recorded provenance, not a claim
//! that the recreation behaves like the original with it.

use asamu_game::{Game, LevelScript};
use asamu_player::grapple_gun::UNLIMITED_GRAPPLES;
use asamu_player::rocket_boots::BootsStateName;
use asamu_world::LevelOrigin;
use glam::Vec3;
use serde::Serialize;

use crate::command::TeleportTarget;
use crate::keys::{Effect, TuneValue, ValueKind};
use crate::overlay::ParamSetLabel;
use crate::rules::Rules;
use crate::session::Session;
use crate::snapshot::SLOT_COUNT;
use crate::telemetry::{JumpStats, SwingStats};

/// A read-only look at a running session.
#[derive(Clone, Copy, Debug)]
pub struct Inspection<'a> {
    game: &'a Game,
    script: Option<&'a LevelScript>,
    session: &'a Session,
}

/// The session at a glance (the HUD's first line).
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct SessionSummary {
    /// What the running parameter set is.
    pub label: ParamSetLabel,
    /// Name of the profile in use.
    pub profile: String,
    /// The overrides or rules changed since the profile was loaded (a user
    /// interface shows the name with a mark then).
    pub profile_modified: bool,
    /// Name of the level.
    pub level: String,
    /// Tick count of the game.
    pub tick: u64,
    /// Number of parameter overrides.
    pub overrides: usize,
    /// The sticky rules in force.
    pub rules: Rules,
    /// The simulation is frozen.
    pub frozen: bool,
    /// Single steps asked for and not yet run.
    pub pending_steps: u32,
    /// Speed factor (1 = normal).
    pub speed: f32,
    /// A Sandbox recording is running.
    pub recording: bool,
    /// Frozen-world fly placement is on.
    pub flying: bool,
    /// The level runs a level script (save states are "simulation only").
    pub scripted: bool,
    /// The level is hand-made, so the keyframe rewind is available.
    pub rewind_available: bool,
    /// Rewind keyframes held.
    pub rewind_keyframes: usize,
    /// Seconds between the oldest and the latest rewind keyframe.
    pub rewind_seconds: f32,
    /// Commands the session has executed.
    pub actions: usize,
    /// No override, default rules, no command executed, time control never
    /// used.
    pub pristine: bool,
}

/// One row of the parameter list.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ParamRow {
    /// Parameter key.
    pub key: String,
    /// Value type.
    pub kind: ValueKind,
    /// The value the session runs.
    pub current: TuneValue,
    /// The Classic value.
    pub classic: TuneValue,
    /// Unit.
    pub unit: &'static str,
    /// What the parameter does.
    pub description: &'static str,
    /// When a change takes effect.
    pub effect: Effect,
    /// The session overrides this key.
    pub overridden: bool,
    /// Where the Classic value comes from, as text.
    pub provenance: String,
}

/// The player at a glance.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct PlayerView {
    /// Collision-centre position, UU.
    pub position: Vec3,
    /// Velocity, uu/s.
    pub velocity: Vec3,
    /// View yaw, radians.
    pub yaw: f32,
    /// View pitch, radians.
    pub pitch: f32,
    /// Eye (view) position, UU.
    pub eye: Vec3,
    /// Speed, uu/s.
    pub speed: f32,
    /// Horizontal speed, uu/s.
    pub horizontal_speed: f32,
    /// `walking`, `falling` or `flying` (the grapple pull).
    pub physics: &'static str,
    /// Standing on a walkable floor.
    pub grounded: bool,
    /// Floor normal the physics last found (zero when unknown).
    pub floor_normal: Vec3,
    /// Collision cylinder radius the game runs, UU.
    pub capsule_radius: f32,
    /// Collision cylinder half-height the game runs, UU.
    pub capsule_half_height: f32,
    /// Run-time ground speed the script layer latched, uu/s.
    pub ground_speed: f32,
    /// Run-time air control.
    pub air_control: f32,
    /// Run-time jump velocity, uu/s.
    pub jump_z: f32,
    /// Run-time air speed, uu/s.
    pub air_speed: f32,
    /// Run-time FOV, degrees.
    pub fov: f32,
    /// Grapple capacity (see [`Self::unlimited_grapples`]).
    pub max_grapples: i32,
    /// The capacity is the "unlimited" value.
    pub unlimited_grapples: bool,
    /// Grapples used since the last refill.
    pub times_grappled: i32,
    /// Grapples left before the next refill.
    pub grapples_left: i32,
    /// The grapple is attached.
    pub grapple_attached: bool,
    /// The anchor while attached, UU.
    pub grapple_anchor: Option<Vec3>,
    /// The gun's reach the game runs (`gun.max_distance`), UU; 0 without the
    /// gun.
    pub grapple_range: f32,
    /// The distance at which the gun lets go (`gun.release_distance`), UU;
    /// 0 without the gun.
    pub release_distance: f32,
    /// The rocket boots are enabled.
    pub boots_enabled: bool,
    /// `ready`, `boosting` or `unavailable`.
    pub boots_state: &'static str,
    /// Story mode is on.
    pub story_mode: bool,
    /// The death sequence is running (converted levels).
    pub dying: bool,
    /// Respawns so far.
    pub respawns: u32,
}

/// What the crosshair points at.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct AimView {
    /// The fire trace hit something.
    pub hit: bool,
    /// The hit can be grappled from here.
    pub acceptable: bool,
    /// Eye-to-hit distance, UU.
    pub distance: f32,
    /// Where the trace started (the eye), UU.
    pub start: Vec3,
    /// Where the trace ended, UU.
    pub location: Vec3,
    /// Surface normal at the hit.
    pub normal: Option<Vec3>,
    /// The gun's reach the game runs (`gun.max_distance`), UU.
    pub max_distance: f32,
}

/// The level at a glance.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct WorldView {
    /// Name of the level.
    pub level: String,
    /// Hand-made (ours) rather than converted from the user's install.
    pub hand_made: bool,
    /// Number of checkpoints.
    pub checkpoints: usize,
    /// Id of the active checkpoint.
    pub active_checkpoint: Option<u32>,
    /// Further facts as label / value pairs (converted levels: scene
    /// statistics).
    pub details: Vec<(String, String)>,
}

/// A place the player can be teleported to.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct TeleportTargetInfo {
    /// What to show.
    pub label: String,
    /// The operand for `Command::Teleport`.
    pub target: TeleportTarget,
}

/// One save-state slot.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct SlotView {
    /// Slot index (0-based; show it plus one).
    pub slot: usize,
    /// This is the selected slot.
    pub selected: bool,
    /// Tick of the saved state (`None`: empty).
    pub tick: Option<u64>,
    /// Label of the saved state.
    pub label: Option<String>,
    /// The state can be loaded now (it was taken on the running level).
    pub loadable: bool,
    /// Loading it restores the simulation only (a scripted level's audio,
    /// effects and Kismet-driven interface are not part of a save state).
    pub simulation_only: bool,
}

/// The read-outs at a glance.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize)]
pub struct TelemetryView {
    /// Highest speed of the current attempt, uu/s.
    pub peak_speed: f32,
    /// The latest completed jump.
    pub last_jump: Option<JumpStats>,
    /// The latest completed swing.
    pub last_swing: Option<SwingStats>,
    /// Ticks in the history ring.
    pub samples: usize,
    /// Archived attempt trails.
    pub attempts: usize,
}

/// Why an inspection could not be dumped.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum InspectError {
    /// Serialization failure.
    #[error("serialization failed: {0}")]
    Json(String),
}

/// Everything an inspection shows, for [`Inspection::to_json_pretty`].
#[derive(Serialize)]
struct Dump {
    note: &'static str,
    summary: SessionSummary,
    player: PlayerView,
    aim: Option<AimView>,
    world: WorldView,
    telemetry: TelemetryView,
    slots: Vec<SlotView>,
    teleport_targets: Vec<TeleportTargetInfo>,
    params: Vec<ParamRow>,
}

/// A value as a user interface shows it: numbers in the shortest form that
/// reads back as the same `f32`, flags as `on` / `off`.
#[must_use]
pub fn format_value(value: &TuneValue) -> String {
    match value {
        TuneValue::Bool(true) => "on".to_owned(),
        TuneValue::Bool(false) => "off".to_owned(),
        TuneValue::Int(i) => i.to_string(),
        // Parameters are `f32`: show what the simulation gets.
        TuneValue::Float(f) => (*f as f32).to_string(),
        TuneValue::Text(t) => t.clone(),
    }
}

impl<'a> Inspection<'a> {
    /// A look at `game` (with its level script) under `session`.
    #[must_use]
    pub fn new(game: &'a Game, script: Option<&'a LevelScript>, session: &'a Session) -> Self {
        Self {
            game,
            script,
            session,
        }
    }

    /// The session at a glance.
    #[must_use]
    pub fn summary(&self) -> SessionSummary {
        let session = self.session;
        let hand_made = self.game.scene_map().is_none();
        let rewind = session.rewind();
        SessionSummary {
            label: session.label(),
            profile: session.profile().name.clone(),
            profile_modified: session.profile_modified(),
            level: self.game.level().name.clone(),
            tick: self.game.clock().tick(),
            overrides: session.overlay().len(),
            rules: *session.rules(),
            frozen: session.time().frozen(),
            pending_steps: session.time().pending_steps(),
            speed: session.time().speed(),
            recording: self.game.is_recording(),
            flying: session.flying(),
            scripted: self.script.is_some(),
            rewind_available: hand_made,
            rewind_keyframes: rewind.len(),
            rewind_seconds: (rewind.span_ticks() as f64 * self.game.clock().dt_f64()) as f32,
            actions: session.log().len(),
            pristine: session.is_pristine(),
        }
    }

    /// The parameter rows of `group` (`movement`, `gun`, ...), in report
    /// order. Empty for an unknown group.
    #[must_use]
    pub fn params(&self, group: &str) -> Vec<ParamRow> {
        self.param_rows(Some(group))
    }

    fn param_rows(&self, group: Option<&str>) -> Vec<ParamRow> {
        let overlay = self.session.overlay();
        self.session
            .catalog()
            .iter()
            .filter(|info| {
                group.is_none_or(|g| info.key.split_once('.').is_some_and(|(head, _)| head == g))
            })
            .map(|info| {
                let overridden = overlay.get(&info.key);
                ParamRow {
                    key: info.key.clone(),
                    kind: info.kind,
                    current: overridden.unwrap_or(&info.classic).clone(),
                    classic: info.classic.clone(),
                    unit: info.unit,
                    description: info.description,
                    effect: info.effect,
                    overridden: overridden.is_some(),
                    provenance: info.classic_provenance.to_string(),
                }
            })
            .collect()
    }

    /// The player at a glance.
    #[must_use]
    pub fn player(&self) -> PlayerView {
        let game = self.game;
        let p = game.player();
        let params = game.params();
        let gun = &p.script.gun;
        let (grapple_range, release_distance) = params.gun.as_ref().map_or((0.0, 0.0), |g| {
            (g.max_distance.value, g.release_distance.value)
        });
        PlayerView {
            position: p.position,
            velocity: p.velocity,
            yaw: p.yaw,
            pitch: p.pitch,
            eye: game.eye_position(),
            speed: p.speed(),
            horizontal_speed: p.horizontal_speed(),
            physics: if p.pawn.flying {
                "flying"
            } else if p.grounded {
                "walking"
            } else {
                "falling"
            },
            grounded: p.grounded,
            floor_normal: p.pawn.floor,
            capsule_radius: params.movement.capsule_radius.value,
            capsule_half_height: params.movement.capsule_half_height.value,
            ground_speed: p.script.ground_speed,
            air_control: p.script.air_control,
            jump_z: p.script.jump_z,
            air_speed: p.script.air_speed,
            fov: game.fov(),
            max_grapples: gun.max_grapples,
            unlimited_grapples: gun.max_grapples >= UNLIMITED_GRAPPLES,
            times_grappled: gun.times_grappled,
            grapples_left: gun.grapples_left(),
            grapple_attached: p.is_grapple_attached(),
            grapple_anchor: p.grapple_anchor(),
            grapple_range,
            release_distance,
            boots_enabled: p.script.boots.enabled,
            boots_state: match p.script.boots.state {
                BootsStateName::Ready => "ready",
                BootsStateName::Boosting => "boosting",
                BootsStateName::Unavailable | BootsStateName::UnavailableAndPlayedSound => {
                    "unavailable"
                }
            },
            story_mode: game.in_story_mode(),
            dying: game.is_dying(),
            respawns: game.respawn_count(),
        }
    }

    /// What the crosshair points at (`None` without the grapple gun).
    #[must_use]
    pub fn aim(&self) -> Option<AimView> {
        let aim = self.game.gun_aim()?;
        let max_distance = self.game.params().gun.as_ref()?.max_distance.value;
        Some(AimView {
            hit: aim.impact.hit.is_some(),
            acceptable: aim.acceptable,
            distance: aim.distance,
            start: aim.impact.start,
            location: aim.impact.location,
            normal: aim.impact.hit.map(|h| h.normal),
            max_distance,
        })
    }

    /// The level at a glance.
    #[must_use]
    pub fn world(&self) -> WorldView {
        let game = self.game;
        let level = game.level();
        let mut details: Vec<(String, String)> = Vec::new();
        let mut add = |label: &str, value: String| details.push((label.to_owned(), value));
        let checkpoints = match game.scene_map() {
            Some(map) => {
                add("map", map.map.clone());
                add("sub-levels", map.levels.len().to_string());
                add("actors", map.stats.actors.to_string());
                add(
                    "collision components placed",
                    map.stats.placed_components.to_string(),
                );
                add("player starts", map.actors.player_starts.len().to_string());
                add("touch volumes", map.actors.volumes.len().to_string());
                add("triggers", map.actors.triggers.len().to_string());
                add("falling rocks", map.actors.rocks.len().to_string());
                add("load warnings", map.warnings.len().to_string());
                map.actors.checkpoints.len()
            }
            None => {
                add("origin", "hand-made by us; no original content".to_owned());
                add("boxes", level.static_boxes.len().to_string());
                add("grapple points", level.grapple_points.len().to_string());
                add("movers", level.movers.len().to_string());
                level.checkpoints.len()
            }
        };
        add("crystals", level.crystals.len().to_string());
        add("attractor pads", level.attractors.len().to_string());
        add("kill height", format!("{} uu", level.kill_z));
        add(
            "level script",
            match self.script {
                Some(script) => format!("running ({} host notes)", script.host_errors().len()),
                None => "none".to_owned(),
            },
        );
        add("movement model", game.movement_model().name().to_owned());
        add("tick rate", format!("{} Hz", game.clock().tick_rate_hz()));
        WorldView {
            level: level.name.clone(),
            hand_made: level.origin == LevelOrigin::HandMadeGraybox,
            checkpoints,
            active_checkpoint: game.active_checkpoint(),
            details,
        }
    }

    /// Places the player can be teleported to: the start, the checkpoints
    /// (on converted maps the player starts and checkpoint spawns) and the
    /// session's bookmarks on this level.
    #[must_use]
    pub fn teleport_targets(&self) -> Vec<TeleportTargetInfo> {
        self.session.teleport_targets(self.game)
    }

    /// The save-state slots.
    #[must_use]
    pub fn slots(&self) -> Vec<SlotView> {
        let slots = self.session.slots();
        let level = self.game.level();
        (0..SLOT_COUNT)
            .map(|slot| {
                let saved = slots.get(slot);
                SlotView {
                    slot,
                    selected: slots.selected() == slot,
                    tick: saved.map(|s| s.tick()),
                    label: saved.map(|s| s.label().to_owned()),
                    loadable: saved.is_some_and(|s| s.level_name() == level.name),
                    simulation_only: saved.is_some_and(|s| s.scripted()),
                }
            })
            .collect()
    }

    /// The read-outs at a glance.
    #[must_use]
    pub fn telemetry(&self) -> TelemetryView {
        let t = self.session.telemetry();
        TelemetryView {
            peak_speed: t.peak_speed(),
            last_jump: t.last_jump(),
            last_swing: t.last_swing(),
            samples: t.trail().count(),
            attempts: t.attempts().len(),
        }
    }

    /// Everything above as pretty-printed JSON (for a dump file). The dump
    /// says of itself that it describes a Sandbox session.
    ///
    /// # Errors
    /// Serialization failure.
    pub fn to_json_pretty(&self) -> Result<String, InspectError> {
        let dump = Dump {
            note: "ASAMU-decomp Sandbox inspection: a description of this recreation in a \
                   sandbox session; not a measurement of the original and not evidence of parity",
            summary: self.summary(),
            player: self.player(),
            aim: self.aim(),
            world: self.world(),
            telemetry: self.telemetry(),
            slots: self.slots(),
            teleport_targets: self.teleport_targets(),
            params: self.param_rows(None),
        };
        serde_json::to_string_pretty(&dump).map_err(|e| InspectError::Json(e.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn values_read_as_the_simulation_gets_them() {
        assert_eq!(format_value(&TuneValue::Bool(true)), "on");
        assert_eq!(format_value(&TuneValue::Bool(false)), "off");
        assert_eq!(format_value(&TuneValue::Int(-3)), "-3");
        assert_eq!(format_value(&TuneValue::Float(0.5)), "0.5");
        // An `f32` widened to `f64` shows as the `f32`, not as its tail.
        assert_eq!(format_value(&TuneValue::Float(f64::from(0.3_f32))), "0.3");
        assert_eq!(format_value(&TuneValue::Float(1000.0)), "1000");
        assert_eq!(
            format_value(&TuneValue::Text("inelastic".to_owned())),
            "inelastic"
        );
    }

    #[test]
    fn a_fresh_classic_session_reads_as_classic() {
        let session = Session::classic();
        let mut game = Game::graybox().unwrap();
        game.start();
        let look = Inspection::new(&game, None, &session);
        let summary = look.summary();
        assert_eq!(summary.label, ParamSetLabel::Classic);
        assert_eq!(summary.overrides, 0);
        assert!(summary.pristine);
        assert!(!summary.profile_modified);
        assert!(!summary.frozen && summary.speed == 1.0);
        assert!(!summary.recording && !summary.flying && !summary.scripted);
        assert!(summary.rewind_available);
        assert_eq!(summary.level, game.level().name);

        let player = look.player();
        assert_eq!(player.position, game.player().position);
        assert_eq!(player.physics, "walking");
        assert_eq!(player.max_grapples, 3);
        assert_eq!(player.grapples_left, 3);
        assert!(!player.unlimited_grapples);
        assert!(player.boots_enabled);
        assert_eq!(player.boots_state, "ready");
        assert_eq!(player.fov, game.fov());

        let world = look.world();
        assert!(world.hand_made);
        assert_eq!(world.checkpoints, game.level().checkpoints.len());
        assert_eq!(world.active_checkpoint, None);
        assert!(world.details.iter().any(|(k, _)| k == "boxes"));

        let slots = look.slots();
        assert_eq!(slots.len(), SLOT_COUNT);
        assert!(slots.iter().all(|s| s.tick.is_none() && !s.loadable));
        assert_eq!(slots.iter().filter(|s| s.selected).count(), 1);
    }

    #[test]
    fn param_rows_follow_the_catalogue_and_the_overlay() {
        use crate::command::Command;
        use crate::session::SimCx;

        let mut session = Session::classic();
        let mut game = Game::graybox().unwrap();
        game.start();
        let groups: Vec<String> = session
            .catalog()
            .groups()
            .into_iter()
            .map(str::to_owned)
            .collect();
        assert!(!groups.is_empty());
        let total: usize = {
            let look = Inspection::new(&game, None, &session);
            groups.iter().map(|g| look.params(g).len()).sum()
        };
        assert_eq!(
            total,
            session.catalog().iter().count(),
            "every key is in one group"
        );
        {
            let look = Inspection::new(&game, None, &session);
            assert!(look.params("no-such-group").is_empty());
            for group in &groups {
                for row in look.params(group) {
                    assert!(row.key.starts_with(&format!("{group}.")), "{}", row.key);
                    assert!(!row.overridden);
                    assert_eq!(row.current, row.classic, "{}", row.key);
                    assert!(!row.provenance.is_empty(), "{}", row.key);
                }
            }
        }

        // One override: exactly that row changes.
        let key = "pawn.zoom_enabled";
        let mut script = None;
        session
            .execute(
                Command::SetParam {
                    key: key.to_owned(),
                    value: TuneValue::Bool(false),
                },
                &mut SimCx {
                    game: &mut game,
                    script: &mut script,
                },
            )
            .unwrap();
        let look = Inspection::new(&game, None, &session);
        let rows: Vec<ParamRow> = groups.iter().flat_map(|g| look.params(g)).collect();
        let changed: Vec<&ParamRow> = rows.iter().filter(|r| r.overridden).collect();
        assert_eq!(changed.len(), 1);
        assert_eq!(changed[0].key, key);
        assert_eq!(changed[0].current, TuneValue::Bool(false));
        assert_eq!(changed[0].classic, TuneValue::Bool(true));
        assert_eq!(changed[0].kind, ValueKind::Bool);
        let summary = look.summary();
        assert_eq!(summary.overrides, 1);
        assert_eq!(summary.label, ParamSetLabel::Modified { overrides: 1 });
        assert!(summary.profile_modified);
        assert!(!summary.pristine);
        assert_eq!(summary.actions, 1);
    }

    #[test]
    fn the_aim_view_is_the_games_own_aim() {
        let session = Session::classic();
        let mut game = Game::graybox().unwrap();
        game.start();
        let look = Inspection::new(&game, None, &session);
        let aim = look.aim().expect("the Classic set has the grapple gun");
        let own = game.gun_aim().unwrap();
        assert_eq!(aim.hit, own.impact.hit.is_some());
        assert_eq!(aim.acceptable, own.acceptable);
        assert_eq!(aim.distance, own.distance);
        assert_eq!(aim.start, own.impact.start);
        assert_eq!(aim.location, own.impact.location);
        assert_eq!(aim.max_distance, look.player().grapple_range);
        assert!(aim.max_distance > 0.0);
        // Without the gun there is no crosshair.
        let placeholder = Game::graybox_placeholder().unwrap();
        assert!(
            Inspection::new(&placeholder, None, &session)
                .aim()
                .is_none()
        );
    }

    #[test]
    fn the_dump_is_json_with_every_section_and_says_what_it_is() {
        let session = Session::classic();
        let mut game = Game::graybox().unwrap();
        game.start();
        let look = Inspection::new(&game, None, &session);
        let text = look.to_json_pretty().unwrap();
        let value: serde_json::Value = serde_json::from_str(&text).unwrap();
        for section in [
            "note",
            "summary",
            "player",
            "aim",
            "world",
            "telemetry",
            "slots",
            "teleport_targets",
            "params",
        ] {
            assert!(value.get(section).is_some(), "{section}");
        }
        let note = value["note"].as_str().unwrap();
        assert!(note.contains("Sandbox"));
        assert!(note.contains("not evidence of parity"));
        assert_eq!(
            value["params"].as_array().map(Vec::len),
            Some(session.catalog().iter().count())
        );
        assert_eq!(value["summary"]["label"], "classic");
        assert_eq!(value["slots"].as_array().map(Vec::len), Some(SLOT_COUNT));
        assert_eq!(
            value["teleport_targets"].as_array().map(Vec::len),
            Some(look.teleport_targets().len())
        );
        assert_eq!(value["world"]["hand_made"], true);
    }
}
