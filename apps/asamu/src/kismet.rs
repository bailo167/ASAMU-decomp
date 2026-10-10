//! Kismet presentation: turns what the level script and the NPCs report
//! each fixed tick into sound, music, subtitles, HUD and UI changes, camera
//! work, actor rendering and save records. Frame order and data flow:
//! `docs/INTEGRATION.md`.
//!
//! `main.rs` runs the game frame (`LevelScript::tick`, which also ticks the
//! NPCs inside `Game::tick`) and writes one [`KismetFrame`] per tick. Here:
//!
//! - **audio**: `SeqAct_PlaySound` → `AudioCommand::PlaySound` (with the
//!   action's node, spatialised at the first target actor), its stop input →
//!   `StopSound`; the narrator's lines (the Kismet runtime runs the narrator
//!   queue) → `NarratorPlay` / `NarratorStop`; `SeqAct_SetSoundMode` →
//!   `SetSoundMode`; `SeqAct_Toggle` on ambient sound actors and Matinee
//!   toggle keys → `ToggleAmbient`; Matinee sound keys → `PlaySound`; the
//!   adaptive-music actions → [`crate::audio::music`];
//! - **UI**: tutorial pop-ups, the title logo, crosshair toggles (the action
//!   and the `ToggleCrosshair` console command), HUD toggles, the pause-menu
//!   switch, achievements, game finished, time-trial start/end, the credits
//!   screen that stands in for the credits movie (its end fires
//!   `SeqEvent_CreditsEnded`);
//! - **camera**: fades (`SeqAct_CameraFade`, Matinee fade tracks), view
//!   targets (`SeqAct_SetCameraTarget`, Matinee director cuts), shakes (the
//!   action; the worm's growl falls back to a placeholder shake only when
//!   its camera animation is not converted), cinematic mode (blocks player
//!   input in `main.rs` while active), and the camera animations the level
//!   script plays (gameplay events and `SeqAct_PlayCameraAnim`;
//!   [`apply_camera_anims`] runs after `main.rs`'s `sync_camera` and applies
//!   them to the point of view as `ACamera::ApplyAnimToCamera` does);
//! - **level transitions**: `open <map>` → [`crate::ui::OpenMap`] (the flow
//!   loads the next map; the save strings are already in the general save);
//! - **rendering**: actors Kismet moved (`LevelScript::moved_actors`: movers,
//!   their passengers, skeletal Matinee actors) follow their transform,
//!   hidden/destroyed actors disappear, and the meshes of sub-levels Kismet
//!   streams (TheCore) show only while streamed in; level lights
//!   ([`sync_light_render`]) follow the same rules (moved by Matinee, hidden
//!   with their streamed sub-level), take the brightness, radius and colour
//!   Matinee property tracks write, and switch with `SeqAct_Toggle` and
//!   Matinee toggle keys;
//! - **NPC events**: collectibles and story items → progression records
//!   ([`crate::ui::CollectibleFound`], [`crate::ui::StoryItemFound`]), the
//!   worm's camera shake, animation sound notifies (`AnimNotify_Sound`, e.g.
//!   the cutscene characters' lines) → `PlaySound` at the actor.
//!
//! Everything here is presentation; the simulation never reads it back
//! (except the cinematic input block and the credits' end, which the
//! original also feeds back: input is ignored and the credits movie raises
//! the Kismet event).

use std::collections::{BTreeMap, BTreeSet, HashMap};

use asamu_assets::audio::{AudioCommand, SoundSource, ToggleAction};
use asamu_core::glam as sim_glam;
use asamu_game::asamu_kismet::{KValue, Output, ToggleMode};
use asamu_game::npc::{NpcEvent, collectible_save_key, story_item_save_key};
use asamu_game::save::Achievement;
use bevy::ecs::system::SystemParam;
use bevy::prelude::*;

use crate::audio::AudioCommandMessage;
use crate::audio::music::{MusicCommand, MusicCommandMessage, TrackSpec};
use crate::converted::{LevelEntity, LevelLight, RenderLevels};
use crate::ui::{
    AchievementEarned, CollectibleFound, GameFinished, OpenMap, SaveStringEdited, Saves,
    StoryItemFound, TimeTrialEnd, TimeTrialStart, UiStrings,
};
use crate::{Sim, to_render};
use asamu_game::asamu_kismet::PropertyValue;
use asamu_game::asamu_kismet::camera_anim::{Pov, paths as camera_paths};

/// How long the stand-in credits screen stays before `SeqEvent_CreditsEnded`
/// fires (ours: the original's Scaleform credits movie is not ported, so
/// its length is unknown; Enter, Space or Esc end it at once).
pub const CREDITS_SECONDS: f32 = 20.0;

/// Camera-shake amplitude in render units at `ShakeScale` 1 (ours: a
/// presentation placeholder; the original's shake objects are not decoded).
const SHAKE_RENDER_UNITS: f32 = 0.04;

/// The outputs of one Kismet-driven tick (written by `main.rs` after every
/// `LevelScript::tick`).
#[derive(Message, Clone, Debug, Default)]
pub(crate) struct KismetFrame {
    /// Kismet outputs, in emission order.
    pub outputs: Vec<Output>,
    /// NPC events of the game tick.
    pub npc_events: Vec<NpcEvent>,
}

/// A fade towards a target opacity.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct Fade {
    /// Current opacity.
    pub current: f32,
    from: f32,
    to: f32,
    time: f32,
    elapsed: f32,
    persist: bool,
}

impl Fade {
    fn start(&mut self, to: f32, time: f32, persist: bool) {
        self.from = self.current;
        self.to = if to.is_finite() {
            to.clamp(0.0, 1.0)
        } else {
            0.0
        };
        self.time = if time.is_finite() { time.max(0.0) } else { 0.0 };
        self.elapsed = 0.0;
        self.persist = persist;
        if self.time == 0.0 {
            self.finish();
        }
    }

    fn finish(&mut self) {
        // `bPersistFade` false: the fade goes away once complete
        // (TENTATIVE reading of the stock camera fade).
        self.current = if self.persist { self.to } else { 0.0 };
        self.time = 0.0;
    }

    fn update(&mut self, dt: f32) {
        if self.time <= 0.0 {
            return;
        }
        self.elapsed += dt;
        if self.elapsed >= self.time {
            self.finish();
        } else {
            let a = self.elapsed / self.time;
            self.current = self.from + (self.to - self.from) * a;
        }
    }
}

/// What the level script asked of the presentation.
#[derive(Resource, Debug)]
pub(crate) struct Presentation {
    /// Kismet crosshair switch (`SeqAct_ToggleCrosshair`, `ToggleCrosshair`).
    pub crosshair: bool,
    /// HUD switch (`SeqAct_ToggleHUD`).
    pub hud: bool,
    /// Pause menu available (`SeqAct_DisablePauseMenu`).
    pub pause_menu: bool,
    /// "Restart from checkpoint" offered (`None`: not set by Kismet).
    pub restart_option: Option<bool>,
    /// Cinematic mode flags while active: hide player, hide HUD, disable
    /// movement, disable turning, disable input.
    pub cinematic: Option<[bool; 5]>,
    /// Title logo shown.
    pub title_logo: bool,
    /// Tutorial pop-up: id, text, seconds left (`None`: until hidden).
    pub tutorial: Option<(i32, String, Option<f32>)>,
    /// `SeqAct_CameraFade`.
    pub fade: Fade,
    /// Matinee fade amount per `SeqAct_Interp` node.
    pub matinee_fade: BTreeMap<usize, f32>,
    /// `SeqAct_CameraShake` scale while shaking.
    pub shake: Option<f32>,
    /// The worm's growl shake.
    pub worm_shake: bool,
    /// `SeqAct_SetCameraTarget` view target (actor path; `None` = player).
    pub view_target: Option<String>,
    /// Matinee director cut: `(SeqAct_Interp node, actor path)`.
    pub cut: Option<(usize, String)>,
    /// Credits screen: seconds left.
    pub credits: Option<f32>,
    /// `SeqAct_SetLookAtTarget` state per looking actor (presentation only:
    /// the original's head and eye controls always track the player pawn
    /// plus the offsets, whatever the stored target; see NPCS.md).
    pub look_at: BTreeMap<String, Option<String>>,
    /// World actor ids hidden or destroyed by Kismet.
    pub hidden: BTreeSet<u32>,
    /// World actor ids Kismet unhid (actors hidden at level start show).
    pub shown: BTreeSet<u32>,
    /// Lights switched by `SeqAct_Toggle` or Matinee toggle keys (world
    /// actor id → enabled; unlisted lights keep their placed state).
    pub light_enabled: BTreeMap<u32, bool>,
}

impl Default for Presentation {
    fn default() -> Self {
        Self {
            crosshair: true,
            hud: true,
            pause_menu: true,
            restart_option: None,
            cinematic: None,
            title_logo: false,
            tutorial: None,
            fade: Fade::default(),
            matinee_fade: BTreeMap::new(),
            shake: None,
            worm_shake: false,
            view_target: None,
            cut: None,
            credits: None,
            look_at: BTreeMap::new(),
            hidden: BTreeSet::new(),
            shown: BTreeSet::new(),
            light_enabled: BTreeMap::new(),
        }
    }
}

impl Presentation {
    /// Hides (`true`) or shows world actor `id`.
    fn set_hidden(&mut self, id: u32, hide: bool) {
        if hide {
            self.hidden.insert(id);
            self.shown.remove(&id);
        } else {
            self.hidden.remove(&id);
            self.shown.insert(id);
        }
    }

    /// The screen's fade opacity (the larger of the camera and Matinee
    /// fades).
    #[must_use]
    pub fn fade_opacity(&self) -> f32 {
        self.matinee_fade
            .values()
            .copied()
            .fold(self.fade.current, f32::max)
            .clamp(0.0, 1.0)
    }

    /// The HUD (crosshair, ability panel) is shown.
    #[must_use]
    pub fn hud_shown(&self) -> bool {
        self.hud && !self.cinematic.is_some_and(|f| f[1])
    }

    /// Toggles light actor `id` (`ALight.OnToggle`: on / off / flip of the
    /// light component's `bEnabled`; TENTATIVE, the stock light toggle was
    /// not re-read).
    fn toggle_light(&mut self, id: u32, mode: ToggleMode) {
        let current = self.light_enabled.get(&id).copied().unwrap_or(true);
        self.light_enabled.insert(id, toggle(current, mode));
    }

    /// Player input allowed by cinematic mode: (movement, turning, buttons).
    #[must_use]
    pub fn input_allowed(&self) -> (bool, bool, bool) {
        match self.cinematic {
            Some(f) => (!f[2] && !f[4], !f[3] && !f[4], !f[4]),
            None => (true, true, true),
        }
    }
}

fn toggle(current: bool, mode: ToggleMode) -> bool {
    match mode {
        ToggleMode::On => true,
        ToggleMode::Off => false,
        ToggleMode::Toggle => !current,
    }
}

/// Object name of an object path (`Pkg.TheWorld.PersistentLevel.Name`).
fn object_name(path: &str) -> &str {
    path.rsplit('.').next().unwrap_or(path)
}

/// Where a sound of `targets` is heard: the first target actor's location,
/// else 2-D (the engine's `Kismet_ClientPlaySound` on the player).
fn sound_source(sim: &Sim, targets: &[String]) -> SoundSource {
    let Some(script) = sim.script.as_ref() else {
        return SoundSource::TwoD;
    };
    targets
        .iter()
        .find_map(|t| script.actor_transform_by_path(t))
        .map_or(SoundSource::TwoD, |(l, _)| SoundSource::Location {
            location: l.to_array(),
        })
}

/// The tutorial pop-up's text: the override when set, else the preset's
/// string from the per-user strings (`tutorial.<preset>`), else the preset
/// name itself (the original's localized text is not shipped).
fn tutorial_text(msg: &KValue, strings: &UiStrings) -> String {
    let field = |n: &str| msg.field(n).and_then(KValue::as_str).unwrap_or("");
    let custom = field("tutorialStringOverride");
    if !custom.is_empty() {
        return custom.to_owned();
    }
    let preset = field("tutorialStringPreset");
    let fallback = preset.strip_prefix("Tutorial_").unwrap_or(preset);
    strings
        .get(&format!("tutorial.{preset}"), fallback)
        .to_owned()
}

/// Message writers of the UI integration points.
#[derive(SystemParam)]
pub(crate) struct UiWriters<'w> {
    open: MessageWriter<'w, OpenMap>,
    achievements: MessageWriter<'w, AchievementEarned>,
    finished: MessageWriter<'w, GameFinished>,
    trial_start: MessageWriter<'w, TimeTrialStart>,
    trial_end: MessageWriter<'w, TimeTrialEnd>,
    collectibles: MessageWriter<'w, CollectibleFound>,
    story: MessageWriter<'w, StoryItemFound>,
}

/// Routes the tick's outputs and NPC events.
#[allow(clippy::too_many_arguments)]
pub(crate) fn route_frames(
    mut frames: MessageReader<KismetFrame>,
    sim: Option<Res<Sim>>,
    strings: Res<UiStrings>,
    mut pres: ResMut<Presentation>,
    mut audio: MessageWriter<AudioCommandMessage>,
    mut music: MessageWriter<MusicCommandMessage>,
    mut ui: UiWriters,
    mut had_sim: Local<bool>,
) {
    let Some(sim) = sim else {
        frames.clear();
        // Between games (a level loading, the menu): the previous level's
        // presentation goes, so its cinematic input block, pause switch or
        // fade cannot reach the next level's first fixed ticks (which run
        // before this system first sees the new game).
        if std::mem::take(&mut *had_sim) {
            *pres = Presentation::default();
        }
        return;
    };
    *had_sim = true;
    // A new game starts from a fresh presentation, before its first tick's
    // outputs are routed.
    if sim.is_added() {
        *pres = Presentation::default();
    }
    for frame in frames.read() {
        for o in &frame.outputs {
            route_output(
                o, &sim, &strings, &mut pres, &mut audio, &mut music, &mut ui,
            );
        }
        for e in &frame.npc_events {
            route_npc_event(e, &sim, &mut pres, &mut ui, &mut audio);
        }
    }
}

#[allow(clippy::too_many_lines)]
fn route_output(
    o: &Output,
    sim: &Sim,
    strings: &UiStrings,
    pres: &mut Presentation,
    audio: &mut MessageWriter<AudioCommandMessage>,
    music: &mut MessageWriter<MusicCommandMessage>,
    ui: &mut UiWriters,
) {
    let mut send = |c: AudioCommand| {
        audio.write(AudioCommandMessage(c));
    };
    match o {
        Output::LevelTransition { map, options } => {
            info!("kismet: open {map} ({options:?})");
            ui.open.write(OpenMap { map: map.clone() });
        }
        Output::ConsoleCommand { command } => {
            let mut words = command.split_whitespace();
            match words.next() {
                Some(c) if c.eq_ignore_ascii_case("ToggleCrosshair") => {
                    pres.crosshair = match words.next() {
                        Some(v) => v.eq_ignore_ascii_case("true") || v == "1",
                        None => !pres.crosshair,
                    };
                }
                // `SetSpeed`, `ChangeSize`, `ToggleAdventureSuit`,
                // `NormalMode`, `DisableAllScreenMessages` and the commands
                // without a handler (KISMET.md): not acted on.
                _ => debug!("kismet: console command {command:?} (not acted on)"),
            }
        }
        Output::PlaySound {
            node,
            cue: Some(cue),
            targets,
            volume,
            pitch,
            fade_in,
        } => {
            debug!("kismet: play sound {cue} (node {node})");
            send(AudioCommand::PlaySound {
                cue: cue.clone(),
                source: sound_source(sim, targets),
                volume_multiplier: *volume,
                pitch_multiplier: *pitch,
                fade_in_time: *fade_in,
                suppress_subtitles: false,
                suppress_spatialization: false,
                node: u64::try_from(*node).ok(),
            });
        }
        Output::StopSound { node, fade_out } => send(AudioCommand::StopSound {
            cue: String::new(),
            fade_out_time: *fade_out,
            node: u64::try_from(*node).ok(),
        }),
        Output::NarratorLine {
            id,
            cue: Some(cue),
            volume,
            ..
        } => {
            info!("kismet: narrator line {id:?} ({cue})");
            send(AudioCommand::NarratorPlay {
                id: id.clone(),
                cue: cue.clone(),
                volume: *volume,
            });
        }
        Output::NarratorStop { id } => send(AudioCommand::NarratorStop { id: id.clone() }),
        Output::MatineeSound {
            cue: Some(cue),
            actor,
            volume,
            pitch,
            ..
        } => send(AudioCommand::PlaySound {
            cue: cue.clone(),
            source: sound_source(sim, actor.as_slice()),
            volume_multiplier: *volume,
            pitch_multiplier: *pitch,
            fade_in_time: 0.0,
            suppress_subtitles: false,
            suppress_spatialization: false,
            node: None,
        }),
        Output::SoundMode { start, mode, .. } => send(AudioCommand::SetSoundMode {
            mode: if *start { mode.clone() } else { None },
        }),
        Output::ActorToggled { actor, mode } => {
            if let Some(id) = sim.script.as_ref().and_then(|s| s.actor_id_by_path(actor)) {
                pres.toggle_light(id, *mode);
            }
            send(AudioCommand::ToggleAmbient {
                actor: object_name(actor).to_owned(),
                action: match mode {
                    ToggleMode::On => ToggleAction::TurnOn,
                    ToggleMode::Off => ToggleAction::TurnOff,
                    ToggleMode::Toggle => ToggleAction::Toggle,
                },
            });
        }
        Output::AdaptiveTracks { tracks } => {
            let specs = tracks
                .items()
                .iter()
                .map(|t| {
                    TrackSpec::new(
                        t.field("ID").and_then(KValue::as_str),
                        t.field("trackSoundCue").and_then(KValue::as_obj),
                        t.field("numberOfTracks").map_or(0, KValue::as_int),
                    )
                })
                .collect();
            music.write(MusicCommandMessage(MusicCommand::AddTracks(specs)));
        }
        Output::AdaptiveMultiplier {
            track,
            multiplier_id,
            multiplier,
            adjust_time,
        } => {
            music.write(MusicCommandMessage(MusicCommand::edit(
                track.as_deref(),
                multiplier_id,
                *multiplier,
                *adjust_time,
            )));
        }
        Output::TutorialShow { id, msg } => {
            let seconds = msg
                .field("displayLength")
                .map(KValue::as_float)
                .filter(|s| s.is_finite() && *s > 0.0);
            pres.tutorial = Some((*id, tutorial_text(msg, strings), seconds));
        }
        Output::TutorialHide { id } => {
            if *id <= 0 || pres.tutorial.as_ref().is_some_and(|t| t.0 == *id) {
                pres.tutorial = None;
            }
        }
        Output::Crosshair { show, .. } => pres.crosshair = *show,
        Output::CinematicMode { mode, flags } => {
            let on = toggle(pres.cinematic.is_some(), *mode);
            pres.cinematic = on.then_some(*flags);
        }
        Output::Hud { mode } => pres.hud = toggle(pres.hud, *mode),
        Output::CameraTarget { target } => pres.view_target.clone_from(target),
        Output::CameraFade {
            opacity,
            time,
            persist,
            ..
        } => pres.fade.start(*opacity, *time, *persist),
        Output::CameraShake { start, scale, .. } => {
            pres.shake = start.then_some(if scale.is_finite() { *scale } else { 1.0 });
        }
        Output::MatineeCut { node, group, .. } => {
            pres.cut = group.as_deref().and_then(|g| {
                let path = sim.script.as_ref()?.matinee_group_actor(*node, g)?;
                Some((*node, path.to_owned()))
            });
        }
        Output::MatineeFade { node, amount } => {
            pres.matinee_fade.insert(*node, *amount);
        }
        Output::MatineeKey { actor, action, .. } => {
            let id = actor
                .as_deref()
                .and_then(|a| sim.script.as_ref()?.actor_id_by_path(a));
            match action.as_str() {
                "EVTA_Hide" => {
                    if let Some(id) = id {
                        pres.set_hidden(id, true);
                    }
                }
                "EVTA_Show" => {
                    if let Some(id) = id {
                        pres.set_hidden(id, false);
                    }
                }
                a => {
                    let toggle_action = match a {
                        "ETTA_On" => Some(ToggleAction::TurnOn),
                        "ETTA_Off" => Some(ToggleAction::TurnOff),
                        "ETTA_Toggle" => Some(ToggleAction::Toggle),
                        _ => None,
                    };
                    if let (Some(action), Some(actor)) = (toggle_action, actor) {
                        if let Some(id) = id {
                            let mode = match action {
                                ToggleAction::TurnOn => ToggleMode::On,
                                ToggleAction::TurnOff => ToggleMode::Off,
                                ToggleAction::Toggle => ToggleMode::Toggle,
                            };
                            pres.toggle_light(id, mode);
                        }
                        send(AudioCommand::ToggleAmbient {
                            actor: object_name(actor).to_owned(),
                            action,
                        });
                    }
                }
            }
        }
        Output::ActorHidden { actor, mode } => {
            if let Some(id) = sim.script.as_ref().and_then(|s| s.actor_id_by_path(actor)) {
                let hide = toggle(pres.hidden.contains(&id), *mode);
                pres.set_hidden(id, hide);
            }
        }
        Output::ActorDestroyed { actor } => {
            if let Some(id) = sim.script.as_ref().and_then(|s| s.actor_id_by_path(actor)) {
                pres.set_hidden(id, true);
            }
        }
        Output::OpenMovie { movie, .. } => {
            if movie
                .as_deref()
                .is_some_and(|m| m.to_ascii_lowercase().contains("credits"))
            {
                pres.credits = Some(CREDITS_SECONDS);
            } else {
                // Menu and legal movies: our own menus stand in.
                debug!("kismet: movie {movie:?} (our menus stand in)");
            }
        }
        Output::TitleLogo { show } => pres.title_logo = *show,
        Output::Achievement { id } => match Achievement::from_name(id) {
            Some(a) => {
                ui.achievements.write(AchievementEarned(a));
            }
            None => warn!("kismet: unknown achievement {id:?}"),
        },
        Output::GameFinished { finished } => {
            if *finished {
                ui.finished.write(GameFinished);
            }
        }
        Output::TimeTrial { start } => {
            if *start {
                ui.trial_start.write(TimeTrialStart);
            } else {
                ui.trial_end.write(TimeTrialEnd);
            }
        }
        Output::RestartCheckpointOption { enabled } => pres.restart_option = Some(*enabled),
        Output::PauseMenu { enabled } => pres.pause_menu = *enabled,
        Output::LookAtTarget {
            look,
            target,
            looking: Some(looking),
        } => {
            pres.look_at
                .insert(looking.clone(), if *look { target.clone() } else { None });
        }
        // Handled inside the game frame (the worm controller, follow
        // collision, camera animations, look-at targets) or not presented
        // yet (music tracks of the front end, material parameters, the
        // speed-line cone, the suit-on hand animation, menu calls).
        other => debug!("kismet: {other:?}"),
    }
}

fn route_npc_event(
    e: &NpcEvent,
    sim: &Sim,
    pres: &mut Presentation,
    ui: &mut UiWriters,
    audio: &mut MessageWriter<AudioCommandMessage>,
) {
    let Some(npcs) = sim.game.npcs() else {
        return;
    };
    let scene = npcs.scene();
    match e {
        NpcEvent::AnimSound {
            actor,
            cue,
            volume,
            pitch,
            ..
        } => {
            // At the actor (its Kismet transform when Matinee moved it, else
            // its first skeletal component's placement).
            let location = sim
                .script
                .as_ref()
                .and_then(|s| {
                    s.moved_actors()
                        .into_iter()
                        .find(|(id, _, _)| id == actor)
                        .map(|(_, l, _)| l)
                })
                .or_else(|| {
                    let def = scene.skinned.iter().find(|d| d.id == *actor)?;
                    let m = def.components.first()?.local_to_world;
                    Some(sim_glam::Vec3::new(m[3][0], m[3][1], m[3][2]))
                });
            audio.write(AudioCommandMessage(AudioCommand::PlaySound {
                cue: cue.clone(),
                source: location
                    .filter(|l| l.is_finite())
                    .map_or(SoundSource::TwoD, |l| SoundSource::Location {
                        location: l.to_array(),
                    }),
                volume_multiplier: *volume,
                pitch_multiplier: *pitch,
                fade_in_time: 0.0,
                suppress_subtitles: false,
                suppress_spatialization: false,
                node: None,
            }));
        }
        NpcEvent::AnimNotify { .. } => {}
        NpcEvent::CollectibleCollected { id } => {
            if let Some(def) = scene.collectibles.iter().find(|c| c.id == *id) {
                ui.collectibles.write(CollectibleFound {
                    key: collectible_save_key(def),
                });
            }
        }
        NpcEvent::StoryItemRegistered { item } => {
            let map = sim.game.map_name().unwrap_or_default();
            let def = item.and_then(|id| scene.story_items.iter().find(|s| s.id == id));
            ui.story.write(StoryItemFound {
                key: story_item_save_key(map, def),
            });
        }
        NpcEvent::CameraShake { start, .. } => {
            // The worm's growl is a camera animation the level script plays;
            // the placeholder shake only stands in when it is not converted.
            let converted = sim.script.as_ref().is_some_and(|s| {
                s.camera_anims()
                    .set()
                    .get(camera_paths::WORM_GROWL)
                    .is_some()
            });
            pres.worm_shake = *start && !converted;
        }
        NpcEvent::FoliageTouched { .. } | NpcEvent::GlowFlowerGlow { .. } => debug!("npc: {e:?}"),
        _ => info!("npc: {e:?}"),
    }
}

/// The general save's Kismet strings follow the runtime's
/// (`SeqAct_EditOrAddSaveString` writes; the original rewrites the general
/// save at once).
pub(crate) fn sync_save_strings(
    sim: Option<Res<Sim>>,
    saves: Res<Saves>,
    mut edits: MessageWriter<SaveStringEdited>,
) {
    let Some(script) = sim.as_ref().and_then(|s| s.script.as_ref()) else {
        return;
    };
    for (id, value) in script.runtime().save_strings() {
        if saves.0.general.save_string(id) != Some(*value) {
            edits.write(SaveStringEdited {
                id: id.clone(),
                value: *value,
            });
        }
    }
}

/// Timers (fades, tutorial, credits), the end of Matinee cuts and fades,
/// and a fresh state for a new game.
pub(crate) fn update_presentation(
    time: Res<Time>,
    keys: Res<ButtonInput<KeyCode>>,
    sim: Option<ResMut<Sim>>,
    mut pres: ResMut<Presentation>,
) {
    let Some(mut sim) = sim else {
        return;
    };
    let dt = time.delta_secs();
    pres.fade.update(dt);
    if let Some((_, _, Some(left))) = pres.tutorial.as_mut() {
        *left -= dt;
        if *left <= 0.0 {
            pres.tutorial = None;
        }
    }
    let Some(script) = sim.script.as_mut() else {
        return;
    };
    // A director group's cut and fade end with its Matinee (the engine
    // restores the player's view when the action terminates; TENTATIVE for
    // the fade).
    if pres
        .cut
        .as_ref()
        .is_some_and(|(node, _)| !script.runtime().is_active(*node))
    {
        pres.cut = None;
    }
    pres.matinee_fade
        .retain(|node, _| script.runtime().is_active(*node));
    if let Some(left) = pres.credits.as_mut() {
        *left -= dt;
        let skip = keys.any_just_pressed([KeyCode::Enter, KeyCode::Space, KeyCode::Escape]);
        if *left <= 0.0 || skip {
            pres.credits = None;
            script.runtime_mut().credits_ended();
            info!("credits ended (SeqEvent_CreditsEnded)");
        }
    }
}

/// The camera transform Kismet asks for (a Matinee cut's actor, else the
/// `SetCameraTarget` actor): UE location and rotator.
#[must_use]
pub(crate) fn view_override(sim: &Sim, pres: &Presentation) -> Option<(sim_glam::Vec3, [i32; 3])> {
    let script = sim.script.as_ref()?;
    let path = pres
        .cut
        .as_ref()
        .map(|c| c.1.as_str())
        .or(pres.view_target.as_deref())?;
    script
        .actor_transform_by_path(path)
        .filter(|(l, _)| l.is_finite())
}

/// The camera-shake offset (render units) at `t` seconds.
#[must_use]
pub(crate) fn shake_offset(pres: &Presentation, t: f32) -> Vec3 {
    let scale = pres.shake.unwrap_or(0.0) + if pres.worm_shake { 1.0 } else { 0.0 };
    if scale <= 0.0 {
        return Vec3::ZERO;
    }
    let a = SHAKE_RENDER_UNITS * scale;
    Vec3::new(
        (t * 71.0).sin() * a,
        (t * 89.0).sin() * a,
        (t * 53.0).cos() * a * 0.5,
    )
}

// ---------------------------------------------------------------------------
// Camera animations
// ---------------------------------------------------------------------------

/// Radians → rotator units (rounded).
fn rad_to_units(r: f32) -> i32 {
    let v = (f64::from(r) * 32768.0 / std::f64::consts::PI).round();
    if v.is_finite() {
        v.clamp(f64::from(i32::MIN), f64::from(i32::MAX)) as i32
    } else {
        0
    }
}

/// The UE point of view of a render camera transform (no roll) and
/// horizontal FOV (degrees).
#[must_use]
pub(crate) fn pov_of(transform: &Transform, hfov_degrees: f32) -> Pov {
    use asamu_core::coords::{bevy_dir_to_ue, bevy_pos_to_ue};
    let t = transform.translation;
    let location = bevy_pos_to_ue(sim_glam::Vec3::new(t.x, t.y, t.z), crate::SCALE);
    let f = transform.rotation * Vec3::NEG_Z;
    let f = bevy_dir_to_ue(sim_glam::Vec3::new(f.x, f.y, f.z));
    let yaw = f.y.atan2(f.x);
    let pitch = f.z.clamp(-1.0, 1.0).asin();
    Pov {
        location: location.to_array(),
        rotation: [rad_to_units(pitch), rad_to_units(yaw), 0],
        fov: hfov_degrees,
    }
}

/// A render camera rotation for a UE rotator (pitch, yaw, roll).
#[must_use]
pub(crate) fn rotation_of(rot: [i32; 3]) -> Quat {
    use asamu_core::rotator::{normalize_rotator_axis, rotator_units_to_radians};
    let pitch = rotator_units_to_radians(normalize_rotator_axis(rot[0]));
    let yaw = rotator_units_to_radians(rot[1]);
    let roll = rotator_units_to_radians(normalize_rotator_axis(rot[2]));
    // UE roll turns about the forward axis (Bevy −Z), clockwise as seen by
    // the viewer for a positive roll (TENTATIVE sign; no shipped gameplay
    // animation rolls far).
    crate::bevy_quat(asamu_core::coords::ue_view_to_bevy_rotation(yaw, pitch))
        * Quat::from_rotation_z(-roll)
}

/// Whether a camera-animation sample changes the point of view at all (an
/// animated camera actor at the origin with the default FOV does not).
fn sample_has_effect(s: &asamu_game::asamu_kismet::camera_anim::CameraAnimSample) -> bool {
    use asamu_game::asamu_kismet::camera_anim::CAMERA_ACTOR_DEFAULT_FOV;
    s.location != [0.0; 3] || s.rotation != [0; 3] || s.fov != CAMERA_ACTOR_DEFAULT_FOV
}

/// Applies the level script's camera animations to the camera `sync_camera`
/// placed this frame (location, rotation and FOV offsets in camera space,
/// `ACamera::ApplyAnimToCamera`; the pool advances with the fixed tick).
pub(crate) fn apply_camera_anims(
    sim: Option<Res<Sim>>,
    camera: Option<Single<(&mut Transform, &mut Projection), With<crate::PlayerCamera>>>,
) {
    let (Some(sim), Some(camera)) = (sim, camera) else {
        return;
    };
    let Some(script) = sim.script.as_ref() else {
        return;
    };
    let anims = script.camera_anims();
    // Animations without tracks (the grapple and normal-landing ones) hold
    // an instance but move nothing: leave the camera alone then, so the
    // float view rotation is not rounded to rotator units for nothing.
    if !anims.player().samples().iter().any(sample_has_effect) {
        return;
    }
    let (mut transform, mut projection) = camera.into_inner();
    let Projection::Perspective(persp) = projection.as_mut() else {
        return;
    };
    let aspect = persp.aspect_ratio.max(0.1);
    let hfov = 2.0 * ((persp.fov * 0.5).tan() * aspect).atan();
    let pov = pov_of(&transform, hfov.to_degrees());
    let out = anims.apply(pov);
    let location = sim_glam::Vec3::from_array(out.location);
    if !location.is_finite() || !out.fov.is_finite() {
        return;
    }
    transform.translation = to_render(location);
    transform.rotation = rotation_of(out.rotation);
    let h = out.fov.clamp(1.0, 170.0).to_radians();
    persp.fov = 2.0 * ((h * 0.5).tan() / aspect).atan();
}

// ---------------------------------------------------------------------------
// Lights
// ---------------------------------------------------------------------------

/// Each level light's render transform before its actor first moved.
#[derive(Resource, Default)]
pub(crate) struct LightRender {
    rest: HashMap<Entity, Transform>,
}

/// The brightness and range a light shows for Matinee's property values:
/// `(intensity scale, range scale)` from `Brightness` and `Radius` relative
/// to the placed values (our UE3 → physical mapping is linear in brightness
/// and quadratic in radius, `asamu_assets::lighting`).
#[must_use]
pub(crate) fn light_scales(
    base_brightness: f32,
    base_radius: Option<f32>,
    brightness: Option<f32>,
    radius: Option<f32>,
) -> (f32, f32) {
    let b = match brightness {
        Some(b) if base_brightness > 0.0 && b.is_finite() => (b / base_brightness).max(0.0),
        _ => 1.0,
    };
    let r = match (radius, base_radius) {
        (Some(r), Some(r0)) if r0 > 0.0 && r.is_finite() => (r / r0).max(0.0),
        _ => 1.0,
    };
    (b * r * r, r)
}

/// An `InterpTrackColorProp` value (linear 0..1) as the `FColor` the engine
/// stores: `255 · c^(1/2.2)`, clamped (CONFIRMED constants 0.4545454 and 255
/// read from the executable).
#[must_use]
pub(crate) fn color_prop_to_srgb(c: [f32; 3]) -> [u8; 3] {
    c.map(|v| {
        let x = (v.max(0.0).powf(0.454_545_44) * 255.0) as i32;
        u8::try_from(x.clamp(0, 255)).unwrap_or(255)
    })
}

/// Level lights follow Kismet: hidden with their streamed-out sub-level or
/// their hidden actor, switched by toggles, moved with their actor (Matinee,
/// attachment), and lit by Matinee's `Brightness`, `Radius` and `LightColor`
/// tracks.
#[allow(clippy::type_complexity)]
pub(crate) fn sync_light_render(
    sim: Option<Res<Sim>>,
    pres: Res<Presentation>,
    levels: Option<Res<RenderLevels>>,
    mut render: ResMut<LightRender>,
    mut lights: Query<(
        Entity,
        &LevelLight,
        &mut Transform,
        &mut Visibility,
        Option<&mut PointLight>,
        Option<&mut SpotLight>,
    )>,
) {
    let Some(sim) = sim else {
        return;
    };
    let Some(map) = sim.game.scene_map() else {
        return;
    };
    if sim.is_added() {
        for (e, rest) in render.rest.drain() {
            if let Ok((_, _, mut t, _, _, _)) = lights.get_mut(e) {
                *t = rest;
            }
        }
    }
    let Some(levels) = levels else {
        return;
    };
    let script = sim.script.as_ref();
    let moved: HashMap<u32, (sim_glam::Vec3, [i32; 3])> = script
        .map(|s| {
            s.moved_actors()
                .into_iter()
                .map(|(id, l, r)| (id, (l, r)))
                .collect()
        })
        .unwrap_or_default();
    let mut props: HashMap<u32, Vec<(String, PropertyValue)>> = HashMap::new();
    if let Some(s) = script {
        for (id, name, v) in s.actor_properties() {
            props.entry(id).or_default().push((name, v));
        }
    }
    for (e, light, mut t, mut vis, point, spot) in &mut lights {
        let Some(name) = levels.names.get(light.level) else {
            continue;
        };
        let id = map.level_index(name).and_then(|i| {
            asamu_game::asamu_world::scene::actor_id(u8::try_from(i).ok()?, light.actor_slot)
        });
        let streamed_out = !levels.force_all
            && light.level > 0
            && !levels
                .always_loaded
                .get(light.level)
                .copied()
                .unwrap_or(true)
            && !sim.game.is_level_streamed(name);
        let off = id.is_some_and(|id| {
            pres.hidden.contains(&id) || pres.light_enabled.get(&id) == Some(&false)
        });
        set_visible(&mut vis, !streamed_out && !off);
        let Some(id) = id else { continue };
        if let (Some((l, r)), Some(s)) = (moved.get(&id), script)
            && let Some(rest_xf) = s.actor_placement(id)
            && l.is_finite()
        {
            let base = *render.rest.entry(e).or_insert(*t);
            *t = Transform::from_matrix(render_delta(rest_xf, (*l, *r)) * base.to_matrix());
        }
        let Some(list) = props.get(&id) else { continue };
        let get_f = |n: &str| {
            list.iter().find_map(|(k, v)| match v {
                PropertyValue::Float(f) if k == n => Some(*f),
                _ => None,
            })
        };
        let color = list.iter().find_map(|(k, v)| match v {
            PropertyValue::Color(c) if k == "lightcolor" => Some(color_prop_to_srgb(*c)),
            _ => None,
        });
        let (scale, range_scale) = light_scales(
            light.brightness,
            light.radius,
            get_f("brightness"),
            get_f("radius"),
        );
        if let Some(mut p) = point {
            p.intensity = light.intensity * scale;
            p.range = light.range * range_scale;
            if let Some(c) = color {
                p.color = Color::srgb_u8(c[0], c[1], c[2]);
            }
        } else if let Some(mut s) = spot {
            s.intensity = light.intensity * scale;
            s.range = light.range * range_scale;
            if let Some(c) = color {
                s.color = Color::srgb_u8(c[0], c[1], c[2]);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Overlays
// ---------------------------------------------------------------------------

#[derive(Component)]
pub(crate) struct FadeOverlay;

#[derive(Component)]
pub(crate) struct TitleOverlay;

#[derive(Component)]
pub(crate) struct TutorialBox;

#[derive(Component)]
pub(crate) struct TutorialText;

#[derive(Component)]
pub(crate) struct CreditsOverlay;

pub(crate) fn spawn_overlays(mut commands: Commands) {
    commands.spawn((
        FadeOverlay,
        Node {
            position_type: PositionType::Absolute,
            left: px(0),
            right: px(0),
            top: px(0),
            bottom: px(0),
            ..default()
        },
        BackgroundColor(Color::srgba(0.0, 0.0, 0.0, 0.0)),
        GlobalZIndex(70),
        Pickable::IGNORE,
    ));
    commands.spawn((
        TitleOverlay,
        Node {
            position_type: PositionType::Absolute,
            left: px(0),
            right: px(0),
            top: percent(40),
            justify_content: JustifyContent::Center,
            ..default()
        },
        Visibility::Hidden,
        GlobalZIndex(75),
        children![(
            Text::new("ASAMU-decomp | title logo (the original's logo movie is not ported)"),
            TextFont {
                font_size: FontSize::Px(30.0),
                ..default()
            },
            TextColor(Color::srgb(0.95, 0.92, 0.85)),
        )],
    ));
    commands.spawn((
        TutorialBox,
        Node {
            position_type: PositionType::Absolute,
            right: px(24),
            top: percent(30),
            max_width: px(360),
            padding: UiRect::axes(px(14), px(10)),
            ..default()
        },
        BackgroundColor(Color::srgba(0.05, 0.05, 0.08, 0.75)),
        Visibility::Hidden,
        GlobalZIndex(85),
        children![(
            TutorialText,
            Text::new(""),
            TextFont {
                font_size: FontSize::Px(18.0),
                ..default()
            },
            TextColor(Color::WHITE),
        )],
    ));
    commands.spawn((
        CreditsOverlay,
        Node {
            position_type: PositionType::Absolute,
            left: px(0),
            right: px(0),
            top: px(0),
            bottom: px(0),
            justify_content: JustifyContent::Center,
            align_items: AlignItems::Center,
            ..default()
        },
        BackgroundColor(Color::srgb(0.0, 0.0, 0.0)),
        Visibility::Hidden,
        GlobalZIndex(95),
        children![(
            Text::new(
                "Credits\n\n(the original's credits movie is not ported)\n\nEnter / Space / Esc to continue"
            ),
            TextFont {
                font_size: FontSize::Px(24.0),
                ..default()
            },
            TextColor(Color::srgb(0.9, 0.9, 0.9)),
            TextLayout::justify(Justify::Center),
        )],
    ));
}

fn set_visible(v: &mut Visibility, show: bool) {
    let want = if show {
        Visibility::Inherited
    } else {
        Visibility::Hidden
    };
    if *v != want {
        *v = want;
    }
}

#[allow(clippy::type_complexity)]
pub(crate) fn apply_overlays(
    pres: Res<Presentation>,
    sim: Option<Res<Sim>>,
    mut fade: Query<&mut BackgroundColor, With<FadeOverlay>>,
    mut vis: ParamSet<(
        Query<&mut Visibility, With<TitleOverlay>>,
        Query<&mut Visibility, With<TutorialBox>>,
        Query<&mut Visibility, With<CreditsOverlay>>,
        Query<&mut Visibility, With<crate::hud::Crosshair>>,
        Query<&mut Visibility, With<crate::hud::AbilityPanel>>,
    )>,
    mut tutorial_text: Query<&mut Text, With<TutorialText>>,
) {
    let playing = sim.is_some();
    let alpha = if playing { pres.fade_opacity() } else { 0.0 };
    for mut c in &mut fade {
        let want = Color::srgba(0.0, 0.0, 0.0, alpha);
        if c.0 != want {
            c.0 = want;
        }
    }
    for mut v in &mut vis.p0() {
        set_visible(&mut v, playing && pres.title_logo);
    }
    for mut v in &mut vis.p1() {
        set_visible(&mut v, playing && pres.tutorial.is_some());
    }
    for mut v in &mut vis.p2() {
        set_visible(&mut v, playing && pres.credits.is_some());
    }
    for mut v in &mut vis.p3() {
        set_visible(&mut v, !playing || (pres.crosshair && pres.hud_shown()));
    }
    for mut v in &mut vis.p4() {
        set_visible(&mut v, !playing || pres.hud_shown());
    }
    if let Some((_, text, _)) = &pres.tutorial {
        for mut t in &mut tutorial_text {
            if t.0 != *text {
                t.0.clone_from(text);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Actor rendering
// ---------------------------------------------------------------------------

/// A rendered entity of world actor `id` that is not a level mesh (skinned
/// actors); the level meshes are found through [`LevelEntity`].
#[derive(Component, Clone, Copy, Debug)]
pub(crate) struct KismetActor(pub u32);

/// Level mesh entities per world actor id (rebuilt when the game's map or
/// the spawned entities change) and the render transform each moved entity
/// had before its actor first moved.
#[derive(Resource, Default)]
pub(crate) struct ActorRender {
    map: Option<String>,
    entities: usize,
    by_actor: HashMap<u32, Vec<Entity>>,
    /// World actor id of each indexed level mesh entity.
    id_of: HashMap<Entity, u32>,
    /// Plan level of each indexed entity streamed by Kismet (not always
    /// loaded): name.
    streamed_level: HashMap<Entity, String>,
    base: HashMap<Entity, Transform>,
    /// The visibility each skinned actor entity had before Kismet first hid
    /// or showed it (restored for a new game on the same map).
    rest_visibility: HashMap<Entity, Visibility>,
    /// Entities moved in the latest frame (for the log).
    moved: usize,
}

/// The render-space change of an actor moving from `old` to `new` (UE
/// location and rotator), as a matrix applied to its entities' rest
/// transforms.
#[must_use]
pub(crate) fn render_delta(
    old: (sim_glam::Vec3, [i32; 3]),
    new: (sim_glam::Vec3, [i32; 3]),
) -> Mat4 {
    use asamu_core::coords::ue_to_bevy_basis;
    use asamu_game::asamu_world::rotation::rotation_rows;
    let cols = |r: [i32; 3]| {
        let rows = rotation_rows(r);
        sim_glam::Mat3::from_cols(rows[0].as_vec3(), rows[1].as_vec3(), rows[2].as_vec3())
    };
    let basis = ue_to_bevy_basis();
    let linear = basis * cols(new.1) * cols(old.1).transpose() * basis.transpose();
    let t = to_render(new.0)
        - crate::bevy_vec(linear * asamu_core::coords::ue_pos_to_bevy(old.0, crate::SCALE));
    let c = linear.to_cols_array();
    Mat4::from_cols(
        Vec4::new(c[0], c[1], c[2], 0.0),
        Vec4::new(c[3], c[4], c[5], 0.0),
        Vec4::new(c[6], c[7], c[8], 0.0),
        t.extend(1.0),
    )
}

/// Moves the entities of actors Kismet moved, hides hidden/destroyed actors
/// and the meshes of sub-levels that are not streamed in. A new game puts
/// every moved entity back to its rest transform and every skinned actor
/// Kismet hid or showed back to its own visibility first (a reloaded map
/// keeps its entities).
#[allow(clippy::type_complexity)]
pub(crate) fn sync_actor_render(
    sim: Option<Res<Sim>>,
    pres: Res<Presentation>,
    levels: Option<Res<RenderLevels>>,
    mut render: ResMut<ActorRender>,
    mut meshes: Query<
        (Entity, &LevelEntity, &mut Transform, &mut Visibility),
        Without<KismetActor>,
    >,
    mut skins: Query<(Entity, &KismetActor, &mut Transform, &mut Visibility), Without<LevelEntity>>,
) {
    let Some(sim) = sim else {
        return;
    };
    let Some(map) = sim.game.scene_map() else {
        return;
    };
    if sim.is_added() {
        for (e, rest) in render.base.drain() {
            if let Ok((_, _, mut t, _)) = meshes.get_mut(e) {
                *t = rest;
            } else if let Ok((_, _, mut t, _)) = skins.get_mut(e) {
                *t = rest;
            }
        }
        for (e, rest) in render.rest_visibility.drain() {
            if let Ok((_, _, _, mut v)) = skins.get_mut(e) {
                *v = rest;
            }
        }
    }
    let count = meshes.iter().len();
    let new_map = render.map.as_deref() != Some(map.map.as_str());
    if new_map || render.entities != count {
        if new_map {
            render.base.clear();
            render.rest_visibility.clear();
        }
        render.map = Some(map.map.clone());
        render.entities = count;
        render.by_actor.clear();
        render.id_of.clear();
        render.streamed_level.clear();
        if let Some(levels) = &levels {
            for (e, le, _, _) in &meshes {
                let Some(name) = levels.names.get(le.level) else {
                    continue;
                };
                let id = map.level_index(name).and_then(|i| {
                    asamu_game::asamu_world::scene::actor_id(u8::try_from(i).ok()?, le.actor_slot)
                });
                if let Some(id) = id {
                    render.by_actor.entry(id).or_default().push(e);
                    render.id_of.insert(e, id);
                }
                if le.level > 0 && !levels.always_loaded.get(le.level).copied().unwrap_or(true) {
                    render.streamed_level.insert(e, name.clone());
                }
            }
        }
    }
    let force_all = levels.as_ref().is_some_and(|l| l.force_all);
    // Visibility: hidden actors, sub-levels not streamed in.
    for (e, _, _, mut v) in &mut meshes {
        let streamed_out = !force_all
            && render
                .streamed_level
                .get(&e)
                .is_some_and(|name| !sim.game.is_level_streamed(name));
        let hidden = render
            .id_of
            .get(&e)
            .is_some_and(|id| pres.hidden.contains(id));
        set_visible(&mut v, !streamed_out && !hidden);
    }
    for (e, a, _, mut v) in &mut skins {
        let show = if pres.hidden.contains(&a.0) {
            false
        } else if pres.shown.contains(&a.0) {
            true
        } else {
            continue;
        };
        render.rest_visibility.entry(e).or_insert(*v);
        set_visible(&mut v, show);
    }
    let Some(script) = sim.script.as_ref() else {
        return;
    };
    let ActorRender {
        by_actor,
        base,
        moved,
        ..
    } = &mut *render;
    let before = *moved;
    *moved = 0;
    for (id, location, rotation) in script.moved_actors() {
        let Some(rest) = script.actor_placement(id) else {
            continue;
        };
        if !location.is_finite() {
            continue;
        }
        let delta = render_delta(rest, (location, rotation));
        if let Some(list) = by_actor.get(&id) {
            for e in list {
                if let Ok((_, _, mut t, _)) = meshes.get_mut(*e) {
                    let b = *base.entry(*e).or_insert(*t);
                    *t = Transform::from_matrix(delta * b.to_matrix());
                    *moved += 1;
                }
            }
        }
        for (e, a, mut t, _) in &mut skins {
            if a.0 == id {
                let b = *base.entry(e).or_insert(*t);
                *t = Transform::from_matrix(delta * b.to_matrix());
                *moved += 1;
            }
        }
    }
    if *moved != before {
        debug!(
            "kismet: {} rendered entities follow {} moved actors",
            *moved,
            script.moved_actors().len()
        );
    }
}

/// Kismet presentation plugin (registered in `add_default_plugins`).
pub struct KismetPlugin;

impl Plugin for KismetPlugin {
    fn build(&self, app: &mut App) {
        app.add_message::<KismetFrame>()
            .init_resource::<Presentation>()
            .init_resource::<ActorRender>()
            .init_resource::<LightRender>()
            .add_systems(Startup, spawn_overlays)
            .add_systems(
                Update,
                (
                    route_frames,
                    sync_save_strings,
                    update_presentation,
                    apply_overlays,
                    sync_actor_render,
                    sync_light_render,
                )
                    .chain()
                    .before(crate::ui::UiFlowSet),
            )
            .add_systems(Update, apply_camera_anims.after(crate::sync_camera));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fades_reach_their_target_and_drop_unless_persistent() {
        let mut f = Fade::default();
        f.start(1.0, 2.0, true);
        f.update(1.0);
        assert!((f.current - 0.5).abs() < 1e-6);
        f.update(1.5);
        assert_eq!(f.current, 1.0);
        f.start(0.0, 0.0, true);
        assert_eq!(f.current, 0.0);
        f.start(1.0, 1.0, false);
        f.update(2.0);
        assert_eq!(f.current, 0.0, "a fade that does not persist goes away");
        f.start(f32::NAN, f32::NAN, true);
        assert_eq!(f.current, 0.0);
    }

    #[test]
    fn cinematic_flags_block_input_and_hud() {
        let mut p = Presentation::default();
        assert_eq!(p.input_allowed(), (true, true, true));
        assert!(p.hud_shown());
        p.cinematic = Some([false, true, true, false, false]);
        assert_eq!(p.input_allowed(), (false, true, true));
        assert!(!p.hud_shown());
        p.cinematic = Some([false, false, false, false, true]);
        assert_eq!(p.input_allowed(), (false, false, false));
        assert!(toggle(false, ToggleMode::Toggle));
        assert!(!toggle(true, ToggleMode::Off));
    }

    #[test]
    fn fade_opacity_takes_the_strongest_fade() {
        let mut p = Presentation::default();
        p.fade.current = 0.25;
        p.matinee_fade.insert(3, 0.75);
        assert_eq!(p.fade_opacity(), 0.75);
        p.matinee_fade.insert(4, 2.0);
        assert_eq!(p.fade_opacity(), 1.0);
    }

    #[test]
    fn render_delta_moves_and_turns_like_the_actor() {
        use asamu_core::coords::ue_pos_to_bevy;
        // A point attached to an actor at the origin, 100 uu in front of it.
        let p = sim_glam::Vec3::new(100.0, 0.0, 0.0);
        let rest = (sim_glam::Vec3::ZERO, [0, 0, 0]);
        // The actor moves up by 50 and turns 90 degrees right (yaw 16384).
        let new = (sim_glam::Vec3::new(0.0, 0.0, 50.0), [0, 16384, 0]);
        let d = render_delta(rest, new);
        let moved = d.transform_point3(to_render(p));
        let want = to_render(sim_glam::Vec3::new(0.0, 100.0, 50.0));
        assert!((moved - want).length() < 1e-4, "{moved} vs {want}");
        // No change: identity.
        let d = render_delta(new, new);
        let q = crate::bevy_vec(ue_pos_to_bevy(
            sim_glam::Vec3::new(3.0, 4.0, 5.0),
            crate::SCALE,
        ));
        assert!((d.transform_point3(q) - q).length() < 1e-4);
    }

    #[test]
    fn outputs_reach_audio_ui_and_the_presentation() {
        use bevy::ecs::message::Messages;
        let mut app = App::new();
        app.add_message::<KismetFrame>()
            .add_message::<AudioCommandMessage>()
            .add_message::<MusicCommandMessage>()
            .add_message::<OpenMap>()
            .add_message::<AchievementEarned>()
            .add_message::<GameFinished>()
            .add_message::<TimeTrialStart>()
            .add_message::<TimeTrialEnd>()
            .add_message::<CollectibleFound>()
            .add_message::<StoryItemFound>()
            .init_resource::<Presentation>()
            .init_resource::<UiStrings>()
            .insert_resource(Sim::new(
                asamu_game::Game::graybox().unwrap(),
                String::new(),
            ))
            .add_systems(Update, route_frames);
        let outputs = vec![
            Output::PlaySound {
                node: 7,
                cue: Some("Pkg.Cue".into()),
                targets: Vec::new(),
                volume: 0.5,
                pitch: 1.0,
                fade_in: 0.25,
            },
            Output::StopSound {
                node: 7,
                fade_out: 1.0,
            },
            Output::NarratorLine {
                node: 9,
                id: "Line1".into(),
                cue: Some("Pkg.Narrator".into()),
                volume: 0.8,
            },
            Output::SoundMode {
                start: false,
                mode: Some("Pkg.Mode".into()),
                top_priority: false,
            },
            Output::AdaptiveMultiplier {
                track: None,
                multiplier_id: "Mute".into(),
                multiplier: 0.0,
                adjust_time: 2.0,
            },
            Output::Achievement {
                id: "FLOOR_IS_LAVA".into(),
            },
            Output::LevelTransition {
                map: "AG-Next".into(),
                options: None,
            },
            Output::Crosshair {
                show: false,
                fade: false,
            },
            Output::ConsoleCommand {
                command: "ToggleCrosshair true".into(),
            },
            Output::PauseMenu { enabled: false },
            Output::CinematicMode {
                mode: ToggleMode::On,
                flags: [false, true, true, true, false],
            },
            Output::CameraFade {
                opacity: 1.0,
                time: 0.0,
                persist: true,
                fade_audio: false,
            },
            Output::OpenMovie {
                movie: Some("ASAMUFrontEndFlash.asamu_credits".into()),
                class: None,
            },
        ];
        app.world_mut().write_message(KismetFrame {
            outputs,
            npc_events: Vec::new(),
        });
        app.update();
        let audio: Vec<AudioCommand> = app
            .world()
            .resource::<Messages<AudioCommandMessage>>()
            .iter_current_update_messages()
            .map(|m| m.0.clone())
            .collect();
        assert_eq!(audio.len(), 4, "{audio:?}");
        assert!(matches!(
            &audio[0],
            AudioCommand::PlaySound { cue, node: Some(7), source: SoundSource::TwoD, volume_multiplier, fade_in_time, .. }
                if cue == "Pkg.Cue" && *volume_multiplier == 0.5 && *fade_in_time == 0.25
        ));
        assert!(
            matches!(&audio[1], AudioCommand::StopSound { node: Some(7), fade_out_time, .. } if *fade_out_time == 1.0)
        );
        assert!(
            matches!(&audio[2], AudioCommand::NarratorPlay { id, cue, .. } if id == "Line1" && cue == "Pkg.Narrator")
        );
        assert_eq!(audio[3], AudioCommand::SetSoundMode { mode: None });
        let w = app.world();
        assert_eq!(
            w.resource::<Messages<MusicCommandMessage>>()
                .iter_current_update_messages()
                .count(),
            1
        );
        let open: Vec<&str> = w
            .resource::<Messages<OpenMap>>()
            .iter_current_update_messages()
            .map(|m| m.map.as_str())
            .collect();
        assert_eq!(open, vec!["AG-Next"]);
        let earned: Vec<Achievement> = w
            .resource::<Messages<AchievementEarned>>()
            .iter_current_update_messages()
            .map(|m| m.0)
            .collect();
        assert_eq!(earned, vec![Achievement::FLOOR_IS_LAVA]);
        let p = w.resource::<Presentation>();
        assert!(p.crosshair, "the console command shows it again");
        assert!(!p.pause_menu);
        assert_eq!(p.input_allowed(), (false, false, true));
        assert!(!p.hud_shown());
        assert_eq!(p.fade_opacity(), 1.0);
        assert_eq!(p.credits, Some(CREDITS_SECONDS));
    }

    /// An app with the router and its message types.
    fn router_app(sim: Option<Sim>) -> App {
        let mut app = App::new();
        app.add_message::<KismetFrame>()
            .add_message::<AudioCommandMessage>()
            .add_message::<MusicCommandMessage>()
            .add_message::<OpenMap>()
            .add_message::<AchievementEarned>()
            .add_message::<GameFinished>()
            .add_message::<TimeTrialStart>()
            .add_message::<TimeTrialEnd>()
            .add_message::<CollectibleFound>()
            .add_message::<StoryItemFound>()
            .init_resource::<Presentation>()
            .init_resource::<UiStrings>()
            .add_systems(Update, route_frames);
        if let Some(sim) = sim {
            app.insert_resource(sim);
        }
        app
    }

    #[test]
    fn the_presentation_resets_between_games() {
        let mut app = router_app(Some(Sim::new(
            asamu_game::Game::graybox().unwrap(),
            String::new(),
        )));
        app.world_mut().write_message(KismetFrame {
            outputs: vec![
                Output::CinematicMode {
                    mode: ToggleMode::On,
                    flags: [false, true, true, true, true],
                },
                Output::PauseMenu { enabled: false },
                Output::CameraFade {
                    opacity: 1.0,
                    time: 0.0,
                    persist: true,
                    fade_audio: false,
                },
            ],
            npc_events: Vec::new(),
        });
        app.update();
        {
            let p = app.world().resource::<Presentation>();
            assert_eq!(p.input_allowed(), (false, false, false));
            assert!(!p.pause_menu);
            assert_eq!(p.fade_opacity(), 1.0);
        }
        // The level ends (a load removes the game): the next level's first
        // ticks must not inherit the block, the pause switch or the fade.
        app.world_mut().remove_resource::<Sim>();
        app.update();
        let p = app.world().resource::<Presentation>();
        assert_eq!(p.input_allowed(), (true, true, true));
        assert!(p.pause_menu);
        assert_eq!(p.fade_opacity(), 0.0);
    }

    #[test]
    fn npc_events_become_progression_records_and_shakes() {
        use asamu_game::npc::defs::{CollectibleDef, NpcCylinder, StoryItemDef};
        use asamu_game::npc::{NpcOptions, NpcScene, NpcSystem};
        use bevy::ecs::message::Messages;
        let mut game = asamu_game::Game::graybox().unwrap();
        let at = sim_glam::Vec3::ZERO;
        let scene = NpcScene {
            collectibles: vec![CollectibleDef {
                id: 8,
                level: "T".into(),
                name: "Gem".into(),
                location: at,
                trigger: NpcCylinder {
                    center: at,
                    radius: 10.0,
                    half_height: 10.0,
                },
            }],
            story_items: vec![StoryItemDef {
                id: 9,
                level: "T".into(),
                name: "Map".into(),
                location: at,
                max_interact_times: 1,
                optional: true,
                parent: true,
                linked_parent: None,
                linked_children: Vec::new(),
                has_glow: false,
            }],
            ..NpcScene::default()
        };
        game.attach_npcs(NpcSystem::new(scene, NpcOptions::default()));
        let mut app = router_app(Some(Sim::new(game, String::new())));
        app.world_mut().write_message(KismetFrame {
            outputs: Vec::new(),
            npc_events: vec![
                NpcEvent::CollectibleCollected { id: 8 },
                // Unknown ids are ignored, not panicked on.
                NpcEvent::CollectibleCollected { id: 99 },
                NpcEvent::StoryItemRegistered { item: Some(9) },
                NpcEvent::StoryItemRegistered { item: None },
                NpcEvent::CameraShake {
                    worm: 1,
                    start: true,
                },
            ],
        });
        app.update();
        let w = app.world();
        let found: Vec<String> = w
            .resource::<Messages<CollectibleFound>>()
            .iter_current_update_messages()
            .map(|m| m.key.clone())
            .collect();
        assert_eq!(found, vec!["TheWorld.PersistentLevel.Gem".to_owned()]);
        // The graybox has no map name: the level-file prefix is empty.
        let story: Vec<String> = w
            .resource::<Messages<StoryItemFound>>()
            .iter_current_update_messages()
            .map(|m| m.key.clone())
            .collect();
        assert_eq!(
            story,
            vec!["TheWorld.PersistentLevel.Map".to_owned(), "None".to_owned()]
        );
        assert!(w.resource::<Presentation>().worm_shake);
        assert_ne!(shake_offset(w.resource::<Presentation>(), 0.3), Vec3::ZERO);
    }

    /// Gated on converted data (`ASAMU_CONVERTED_DIR` with `levels`,
    /// `meshes --collision`, `kismet`, `matinee`; skips otherwise): a level
    /// mesh entity of a Matinee mover follows the mover (`D · rest`), and the
    /// meshes of TheCore, which AG-IceCave streams in, show only once the
    /// level script has streamed it.
    #[test]
    fn converted_movers_and_streamed_levels_render_with_kismet() {
        use crate::converted::{LevelEntity, RenderLevels};
        let Some(dir) = std::env::var_os("ASAMU_CONVERTED_DIR").map(std::path::PathBuf::from)
        else {
            eprintln!("SKIP: ASAMU_CONVERTED_DIR not set");
            return;
        };
        if !dir.join("kismet").is_dir() {
            eprintln!("SKIP: no converted Kismet");
            return;
        }
        let run = |map: &str| asamu_game::load_level_with_kismet(&dir, map).ok();
        // BeautifulCity: the first mover that moves within 10 s.
        if let Some((mut game, Some(mut script))) = run("AG-BeautifulCity") {
            game.start();
            let starts: Vec<(u32, sim_glam::Vec3)> = script
                .mover_ids()
                .into_iter()
                .filter_map(|id| script.mover_location(id).map(|l| (id, l)))
                .collect();
            for _ in 0..600 {
                script
                    .tick(&mut game, &asamu_player::InputFrame::default())
                    .unwrap();
            }
            let (id, _) = *starts
                .iter()
                .find(|(id, l)| {
                    script
                        .mover_location(*id)
                        .is_some_and(|n| (n - *l).length() > 10.0)
                })
                .expect("a mover moved");
            let placement = script.actor_placement(id).unwrap();
            let current = script.mover_transform(id).unwrap();
            let map_name = game.scene_map().unwrap().map.clone();
            let mut app = App::new();
            app.init_resource::<Presentation>()
                .init_resource::<ActorRender>()
                .insert_resource(RenderLevels {
                    names: vec![map_name],
                    always_loaded: vec![true],
                    force_all: false,
                })
                .add_systems(Update, sync_actor_render);
            let rest = Transform::from_xyz(1.0, 2.0, 3.0);
            let slot = (id & 0xFFFF) as usize;
            let e = app
                .world_mut()
                .spawn((
                    LevelEntity {
                        actor_slot: slot,
                        level: 0,
                    },
                    rest,
                    Visibility::Inherited,
                ))
                .id();
            app.insert_resource(Sim::new(game, String::new()).with_script(Some(script)));
            app.update();
            let want = Transform::from_matrix(render_delta(placement, current) * rest.to_matrix());
            let got = *app.world().get::<Transform>(e).unwrap();
            assert!(
                (got.translation - want.translation).length() < 1e-4,
                "{got:?} vs {want:?}"
            );
            assert!(
                (got.translation - rest.translation).length() > 1e-3,
                "moved"
            );
            // A skinned actor Kismet hides is hidden; a new game on the same
            // map (the entities are kept) shows it again and puts the moved
            // mesh back to rest.
            let skin = app
                .world_mut()
                .spawn((KismetActor(id), Transform::default(), Visibility::Inherited))
                .id();
            app.world_mut()
                .resource_mut::<Presentation>()
                .hidden
                .insert(id);
            app.update();
            assert_eq!(
                app.world().get::<Visibility>(skin),
                Some(&Visibility::Hidden)
            );
            let sim = app.world_mut().remove_resource::<Sim>().unwrap();
            let fresh = Sim::new(sim.game, String::new());
            *app.world_mut().resource_mut::<Presentation>() = Presentation::default();
            app.insert_resource(fresh);
            app.update();
            assert_eq!(
                app.world().get::<Visibility>(skin),
                Some(&Visibility::Inherited)
            );
            assert_eq!(app.world().get::<Transform>(e), Some(&rest));
        }
        // IceCave: TheCore shows once streamed in.
        if let Some((mut game, Some(mut script))) = run("AG-IceCave") {
            game.start();
            script
                .tick(&mut game, &asamu_player::InputFrame::default())
                .unwrap();
            let names: Vec<String> = game
                .scene_map()
                .unwrap()
                .levels
                .iter()
                .map(|l| l.name.clone())
                .collect();
            let Some(core) = names.iter().position(|n| n.eq_ignore_ascii_case("TheCore")) else {
                eprintln!("SKIP: TheCore not converted");
                return;
            };
            let trigger = asamu_game::smoke::exit_triggers(script.runtime().graph())
                .into_iter()
                .find(|t| t.exit == "stream")
                .expect("the streaming trigger");
            let mut app = App::new();
            app.init_resource::<Presentation>()
                .init_resource::<ActorRender>()
                .insert_resource(RenderLevels {
                    always_loaded: names.iter().enumerate().map(|(i, _)| i != core).collect(),
                    names,
                    force_all: false,
                })
                .add_systems(Update, sync_actor_render);
            let e = app
                .world_mut()
                .spawn((
                    LevelEntity {
                        actor_slot: 1,
                        level: core,
                    },
                    Transform::default(),
                    Visibility::Inherited,
                ))
                .id();
            app.insert_resource(Sim::new(game, String::new()).with_script(Some(script)));
            app.update();
            assert_eq!(
                app.world().get::<Visibility>(e),
                Some(&Visibility::Hidden),
                "not streamed in yet"
            );
            {
                let mut sim = app.world_mut().resource_mut::<Sim>();
                let Sim { game, script, .. } = &mut *sim;
                let script = script.as_mut().unwrap();
                script.runtime_mut().touch(trigger.actor, true);
                for _ in 0..3 {
                    script
                        .tick(game, &asamu_player::InputFrame::default())
                        .unwrap();
                }
                assert!(game.is_level_streamed("TheCore"));
            }
            app.update();
            assert_eq!(
                app.world().get::<Visibility>(e),
                Some(&Visibility::Inherited),
                "streamed in"
            );
        }
    }

    #[test]
    fn camera_povs_round_trip_and_light_values_follow_matinee() {
        use asamu_core::coords::ue_view_to_bevy_rotation;
        // A camera yawed 30° right and pitched 10° up, 5 m forward.
        let (yaw, pitch) = (30.0f32.to_radians(), 10.0f32.to_radians());
        let t = Transform {
            translation: to_render(sim_glam::Vec3::new(500.0, -200.0, 96.0)),
            rotation: crate::bevy_quat(ue_view_to_bevy_rotation(yaw, pitch)),
            scale: Vec3::ONE,
        };
        let pov = pov_of(&t, 90.0);
        assert!((pov.location[0] - 500.0).abs() < 1e-2 && (pov.location[1] + 200.0).abs() < 1e-2);
        assert!((pov.rotation[1] - 5461).abs() <= 1, "{:?}", pov.rotation);
        assert!((pov.rotation[0] - 1820).abs() <= 1, "{:?}", pov.rotation);
        assert_eq!(pov.rotation[2], 0);
        let back = rotation_of(pov.rotation);
        assert!(back.angle_between(t.rotation) < 1e-3);
        // A roll turns about the view axis only.
        let rolled = rotation_of([pov.rotation[0], pov.rotation[1], 8192]);
        assert!(((rolled * Vec3::NEG_Z) - (t.rotation * Vec3::NEG_Z)).length() < 1e-4);
        assert!((rolled.angle_between(t.rotation) - 45.0f32.to_radians()).abs() < 1e-3);
        // Light values: brightness is linear, the radius scales the range
        // and (squared) the intensity; missing or unusable values keep the
        // placed light.
        assert_eq!(light_scales(2.0, Some(400.0), Some(1.0), None), (0.5, 1.0));
        assert_eq!(
            light_scales(2.0, Some(400.0), None, Some(800.0)),
            (4.0, 2.0)
        );
        assert_eq!(light_scales(0.0, None, Some(3.0), Some(100.0)), (1.0, 1.0));
        assert_eq!(
            light_scales(2.0, Some(400.0), Some(f32::NAN), Some(-4.0)),
            (0.0, 0.0)
        );
        assert_eq!(color_prop_to_srgb([1.0, 0.0, 4.0]), [255, 0, 255]);
        assert_eq!(color_prop_to_srgb([0.5, -1.0, f32::NAN])[0], 186);
        // Toggles flip from the placed (enabled) state.
        let mut p = Presentation::default();
        p.toggle_light(4, ToggleMode::Toggle);
        assert_eq!(p.light_enabled.get(&4), Some(&false));
        p.toggle_light(4, ToggleMode::On);
        assert_eq!(p.light_enabled.get(&4), Some(&true));
    }

    /// A level script whose Kismet plays a camera animation at level start:
    /// the camera `sync_camera` placed is moved, turned and zoomed by it.
    #[test]
    fn kismet_camera_animations_move_the_camera() {
        use asamu_game::asamu_kismet::camera_anim::{CameraAnim, CameraAnimSet};
        use asamu_game::asamu_kismet::matinee::{
            CurveMode, CurvePoint, InterpCurve, MoveFrame, MoveTrack,
        };
        use asamu_game::asamu_kismet::{Graph, LevelScripts, MatineeSet};
        let doc = format!(
            r#"{{"format": "{}", "version": {}, "package": "T", "actors": [], "nodes": [
                {{"id": 0, "class": "Engine.Sequence", "kind": "sequence", "members": [1, 2, 3]}},
                {{"id": 1, "class": "Engine.SeqEvent_LevelLoaded", "kind": "event", "parent": 0,
                  "outputs": [{{"desc": "Loaded and Visible", "links": [{{"op": 2, "input": 0}}]}}],
                  "event": {{"max_trigger_count": 1}}}},
                {{"id": 2, "class": "Engine.SeqAct_PlayCameraAnim", "kind": "action", "parent": 0,
                  "inputs": [{{"desc": "Play"}}, {{"desc": "Stop"}}], "outputs": [{{"desc": "Out"}}],
                  "variables": [{{"desc": "Target", "property": "Targets", "vars": [3]}}],
                  "params": {{"CameraAnim": {{"$obj": "Anims.Nod"}}, "Rate": 1.0, "IntensityScale": 1.0,
                              "BlendInTime": 0.0, "BlendOutTime": 0.0, "bLoop": true, "Targets": []}},
                  "auto_activate_outputs": true}},
                {{"id": 3, "class": "Engine.SeqVar_Player", "kind": "variable", "parent": 0}}
            ]}}"#,
            asamu_game::asamu_kismet::RUNTIME_FORMAT,
            asamu_game::asamu_kismet::RUNTIME_VERSION
        );
        let graph = Graph::from_json_slice(doc.as_bytes()).unwrap();
        let mut game = asamu_game::Game::graybox().unwrap();
        let mut script = asamu_game::LevelScript::new(
            &mut game,
            LevelScripts {
                graph,
                matinee: MatineeSet::default(),
                missing_sublevels: Vec::new(),
            },
        );
        let key = |t: f32, v: [f32; 3]| CurvePoint {
            in_val: t,
            out_val: v,
            arrive: [0.0; 3],
            leave: [0.0; 3],
            mode: CurveMode::Constant,
        };
        let mut set = CameraAnimSet::default();
        set.insert(CameraAnim {
            path: "Anims.Nod".into(),
            length: 10.0,
            base_fov: 100.0,
            move_track: Some(MoveTrack {
                // 20 UU up and 45° of yaw from the first tick on.
                pos: InterpCurve {
                    points: vec![key(0.0, [0.0; 3]), key(0.001, [0.0, 0.0, 20.0])],
                    ..InterpCurve::default()
                },
                euler: InterpCurve {
                    points: vec![key(0.0, [0.0; 3]), key(0.001, [0.0, 0.0, 45.0])],
                    ..InterpCurve::default()
                },
                move_frame: MoveFrame::RelativeToInitial,
                ..MoveTrack::default()
            }),
            fov_track: None,
        });
        script.set_camera_anims(set);
        game.start();
        script
            .tick(&mut game, &asamu_player::InputFrame::default())
            .unwrap();
        assert_eq!(script.camera_anims().player().samples().len(), 1);
        let mut app = App::new();
        app.add_systems(Update, apply_camera_anims);
        let rest = Transform::from_translation(to_render(sim_glam::Vec3::new(0.0, 0.0, 100.0)));
        let aspect = 16.0 / 9.0;
        let cam = app
            .world_mut()
            .spawn((
                crate::PlayerCamera,
                rest,
                Projection::from(PerspectiveProjection {
                    fov: 2.0 * ((45.0f32).to_radians().tan() / aspect).atan(),
                    aspect_ratio: aspect,
                    ..default()
                }),
            ))
            .id();
        app.insert_resource(Sim::new(game, String::new()).with_script(Some(script)));
        app.update();
        let t = *app.world().get::<Transform>(cam).unwrap();
        let want = to_render(sim_glam::Vec3::new(0.0, 0.0, 120.0));
        assert!(
            (t.translation - want).length() < 1e-3,
            "{:?}",
            t.translation
        );
        // Yawed 45° to the right: forward (−Z) turns towards +X.
        let f = t.rotation * Vec3::NEG_Z;
        assert!((f.x - f.z.abs()).abs() < 1e-2 && f.x > 0.5, "{f:?}");
        // The FOV grew by BaseFOV − 90 = 10° (horizontal).
        let Projection::Perspective(p) = app.world().get::<Projection>(cam).unwrap() else {
            panic!("perspective");
        };
        let h = 2.0 * ((p.fov * 0.5).tan() * aspect).atan();
        assert!((h.to_degrees() - 100.0).abs() < 1e-2, "{}", h.to_degrees());
        // An animation without tracks (default FOV) holds an instance but
        // leaves the camera exactly as `sync_camera` placed it.
        {
            use asamu_game::asamu_kismet::camera_anim::{CameraAnimHandle, CameraAnimSample};
            let idle = CameraAnimSample {
                handle: CameraAnimHandle(0),
                weight: 1.0,
                location: [0.0; 3],
                rotation: [0; 3],
                fov: 90.0,
            };
            assert!(!sample_has_effect(&idle));
            assert!(sample_has_effect(&CameraAnimSample { fov: 89.98, ..idle }));
            assert!(sample_has_effect(&CameraAnimSample {
                rotation: [0, 1, 0],
                ..idle
            }));
            assert!(sample_has_effect(&CameraAnimSample {
                location: [0.0, 0.0, -0.5],
                ..idle
            }));
        }
        // Without samples nothing changes.
        let mut sim = app.world_mut().remove_resource::<Sim>().unwrap();
        sim.script = None;
        *app.world_mut().get_mut::<Transform>(cam).unwrap() = rest;
        app.insert_resource(sim);
        app.update();
        assert_eq!(app.world().get::<Transform>(cam), Some(&rest));
    }

    /// Gated on converted data: a level light bound to a Matinee brightness
    /// track (AG-Workshop) takes the track's value, a light of an actor
    /// Kismet moved follows it, toggles and hidden actors switch lights off,
    /// and a new game puts everything back.
    #[test]
    fn converted_lights_follow_matinee_and_kismet() {
        use crate::converted::{LevelLight, RenderLevels};
        let Some(dir) = std::env::var_os("ASAMU_CONVERTED_DIR").map(std::path::PathBuf::from)
        else {
            eprintln!("SKIP: ASAMU_CONVERTED_DIR not set");
            return;
        };
        if !dir.join("kismet").is_dir() {
            eprintln!("SKIP: no converted Kismet");
            return;
        }
        let Ok((mut game, Some(mut script))) =
            asamu_game::load_level_with_kismet(&dir, "AG-Workshop")
        else {
            eprintln!("SKIP: AG-Workshop not converted");
            return;
        };
        game.start();
        for _ in 0..600 {
            script
                .tick(&mut game, &asamu_player::InputFrame::default())
                .unwrap();
        }
        let props = script.actor_properties();
        let Some((id, value)) = props.iter().find_map(|(id, n, v)| match v {
            PropertyValue::Float(f) if n == "brightness" => Some((*id, *f)),
            _ => None,
        }) else {
            eprintln!("SKIP: no brightness track ran in 10 s");
            return;
        };
        let moved = script.moved_actors();
        let map_name = game.scene_map().unwrap().map.clone();
        let mut app = App::new();
        app.init_resource::<Presentation>()
            .init_resource::<LightRender>()
            .insert_resource(RenderLevels {
                names: vec![map_name],
                always_loaded: vec![true],
                force_all: false,
            })
            .add_systems(Update, sync_light_render);
        let light = |slot: usize| LevelLight {
            level: 0,
            actor_slot: slot,
            brightness: 2.0,
            radius: Some(512.0),
            intensity: 1000.0,
            range: 5.0,
        };
        let rest = Transform::from_xyz(1.0, 2.0, 3.0);
        let lit = app
            .world_mut()
            .spawn((
                light((id & 0xFFFF) as usize),
                PointLight::default(),
                rest,
                Visibility::Inherited,
            ))
            .id();
        let follower = moved.first().map(|(mid, _, _)| {
            let e = app
                .world_mut()
                .spawn((
                    light((mid & 0xFFFF) as usize),
                    PointLight::default(),
                    rest,
                    Visibility::Inherited,
                ))
                .id();
            (*mid, e)
        });
        let placement = follower.and_then(|(mid, _)| script.actor_placement(mid));
        let current = follower.and_then(|(mid, _)| {
            moved
                .iter()
                .find(|(i, _, _)| *i == mid)
                .map(|(_, l, r)| (*l, *r))
        });
        app.insert_resource(Sim::new(game, String::new()).with_script(Some(script)));
        app.update();
        let got = app.world().get::<PointLight>(lit).unwrap().intensity;
        assert!(
            (got - 1000.0 * value / 2.0).abs() < 1e-2,
            "{got} for {value}"
        );
        if let (Some((_, e)), Some(p), Some(c)) = (follower, placement, current) {
            let want = Transform::from_matrix(render_delta(p, c) * rest.to_matrix());
            let t = *app.world().get::<Transform>(e).unwrap();
            assert!((t.translation - want.translation).length() < 1e-4);
        }
        // Toggled off, then a fresh game: on and at rest again.
        app.world_mut()
            .resource_mut::<Presentation>()
            .toggle_light(id, ToggleMode::Off);
        app.update();
        assert_eq!(
            app.world().get::<Visibility>(lit),
            Some(&Visibility::Hidden)
        );
        let sim = app.world_mut().remove_resource::<Sim>().unwrap();
        *app.world_mut().resource_mut::<Presentation>() = Presentation::default();
        app.insert_resource(Sim::new(sim.game, String::new()));
        app.update();
        assert_eq!(
            app.world().get::<Visibility>(lit),
            Some(&Visibility::Inherited)
        );
        if let Some((_, e)) = follower {
            assert_eq!(app.world().get::<Transform>(e), Some(&rest));
        }
    }

    #[test]
    fn tutorial_text_prefers_override_then_strings_then_the_preset() {
        let s = UiStrings::default();
        let msg = |preset: &str, custom: &str| {
            KValue::Struct(
                [
                    (
                        "tutorialStringPreset".to_owned(),
                        KValue::Str(preset.to_owned()),
                    ),
                    (
                        "tutorialStringOverride".to_owned(),
                        KValue::Str(custom.to_owned()),
                    ),
                ]
                .into_iter()
                .collect(),
            )
        };
        assert_eq!(tutorial_text(&msg("Tutorial_Grapple", ""), &s), "Grapple");
        assert_eq!(tutorial_text(&msg("Tutorial_Grapple", "Hi"), &s), "Hi");
        assert_eq!(object_name("A.TheWorld.PersistentLevel.Lamp_3"), "Lamp_3");
    }
}
