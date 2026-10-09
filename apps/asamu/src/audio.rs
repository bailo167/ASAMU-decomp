//! Runtime audio: sound cues, ambient sounds, narration + subtitles, sound
//! classes and modes, gameplay sound events, and the [`AudioCommandMessage`]
//! API for the Kismet runtime.
//!
//! The rules live in the device-free [`asamu_assets::audio`] (the SoundCue
//! evaluator, sound classes and modes, the narrator queue, subtitles,
//! ambient actors); this module only feeds it and plays its voices through
//! Bevy's audio:
//!
//! - the converted audio documents load on the async pool from
//!   `<converted>/audio/` (the converted level's directory, else
//!   `ASAMU_CONVERTED_DIR`, else the importer's default location); without
//!   them the game is silent;
//! - every fixed tick the player simulation's events become the original's
//!   gameplay cues ([`gameplay`]);
//! - every frame the engine updates with the camera as the listener and its
//!   voices are reconciled with Bevy audio entities ([`backend`]); without
//!   an audio device Bevy plays nothing and everything else still runs;
//! - the subtitle line goes to the HUD's subtitle box
//!   ([`crate::hud::Subtitle`]).
//!
//! Kismet integration: `crate::kismet` writes [`AudioCommandMessage`]s
//! (`SeqAct_PlaySound` with its action node in `node`, `SeqAct_SetSoundMode`,
//! `SeqAct_Toggle` on ambient sounds) and [`music::MusicCommandMessage`]s
//! (adaptive music). The narrator has two mutually exclusive paths: the
//! Kismet runtime's own narrator queue (`asamu-kismet`, timed by the fixed
//! simulation tick) drives the sound with `NarratorPlay` / `NarratorStop` —
//! the path the game uses; a host without it uses this engine's queue
//! (`NarratorAddLine` / `NarratorRemoveLine`), whose timers run on the frame
//! time, and reads [`AudioFeedbackMessage`]s (narrator started / line
//! finished / finished).
//!
//! A new game (a level loaded, from the menu or a Kismet `open`) stops every
//! sound (`AudioCommand::StopAll`, which also resets the adaptive music),
//! restarts the gameplay observer and reloads the level's ambient sounds;
//! the converted documents load again if they were not available before.
//! The settings' music / SFX / voice volumes are the sound classes' own
//! volumes (`set_class_volume` on `Music`, `SFX`, `Voice`, children of
//! `Master` in the shipped class tree); the master volume stays Bevy's
//! global volume (`ui::settings`). With subtitles off no line is shown.

mod backend;
mod gameplay;
pub mod music;

use std::path::PathBuf;
use std::sync::Arc;

use asamu_assets::ConvertedDir;
use asamu_assets::audio::{AUDIO_DIR, AudioCommand, AudioEngine, AudioFeedback, AudioLibrary};
use asamu_core::glam::Vec3 as UeVec3;
use bevy::prelude::*;
use bevy::tasks::futures::check_ready;
use bevy::tasks::{AsyncComputeTaskPool, Task};
use bevy::transform::TransformSystems;

use crate::Sim;
use crate::converted::ConvertedLevel;
use crate::hud::Subtitle;
use crate::ui::UserSettings;

pub use gameplay::GameplayAudio;

/// Seed of the audio engine's random generator. A runtime choice: the
/// original seeds its global generator at start-up and shares it with
/// other systems, so its sequences cannot be reproduced anyway.
pub const AUDIO_RANDOM_SEED: u32 = 0x00A5_A3D0;

/// A request for the audio engine (from the Kismet runtime or other
/// systems). Applied before the next audio update.
#[derive(Message, Clone, Debug, PartialEq)]
pub struct AudioCommandMessage(pub AudioCommand);

/// An event reported by the audio engine (narrator progress, unknown cues).
#[derive(Message, Clone, Debug, PartialEq, Eq)]
pub struct AudioFeedbackMessage(pub AudioFeedback);

/// The audio state: the converted documents (once loaded), the engine and
/// the gameplay observer.
#[derive(Resource)]
pub struct AudioRuntime {
    library: Option<Arc<AudioLibrary>>,
    audio_dir: Option<PathBuf>,
    load: Option<Task<Result<AudioLibrary, String>>>,
    /// The audio engine (cue instances, modes, narrator, ambient).
    pub engine: AudioEngine,
    gameplay: GameplayAudio,
    ambient_loaded_for: Option<String>,
    /// Listener position (UU) of the latest frame.
    listener: UeVec3,
    /// Commands that arrived before the documents finished loading.
    queued: Vec<AudioCommand>,
    /// The subtitle line this module last wrote to the HUD (other writers
    /// are left alone while the line does not change).
    last_subtitle: Option<String>,
    /// Music / SFX / voice volumes last given to the sound classes.
    class_volumes: Option<[f32; 3]>,
}

impl Default for AudioRuntime {
    fn default() -> Self {
        Self {
            library: None,
            audio_dir: None,
            load: None,
            engine: AudioEngine::new(AUDIO_RANDOM_SEED),
            gameplay: GameplayAudio::default(),
            ambient_loaded_for: None,
            listener: UeVec3::ZERO,
            queued: Vec::new(),
            last_subtitle: None,
            class_volumes: None,
        }
    }
}

// The accessors are the integration API for the options menu and the
// Kismet host (not called inside this module yet).
#[allow(dead_code)]
impl AudioRuntime {
    /// The loaded audio documents, if any.
    #[must_use]
    pub fn library(&self) -> Option<&Arc<AudioLibrary>> {
        self.library.as_ref()
    }

    /// The directory the documents came from.
    #[must_use]
    pub fn audio_dir(&self) -> Option<&std::path::Path> {
        self.audio_dir.as_deref()
    }

    /// Sets a sound class's volume (options menu: e.g. `Master`, `Music`,
    /// `Voice`, `SFX`). No effect until the documents have loaded.
    pub fn set_class_volume(&mut self, class: &str, volume: f32) {
        if let Some(lib) = self.library.clone() {
            self.engine.set_class_volume(&lib, class, volume);
        }
    }
}

/// Runtime audio plugin (registered in `add_default_plugins`).
pub struct AudioPlugin;

impl Plugin for AudioPlugin {
    fn build(&self, app: &mut App) {
        app.add_message::<AudioCommandMessage>()
            .add_message::<AudioFeedbackMessage>()
            .init_resource::<AudioRuntime>()
            .init_resource::<backend::VoiceSet>()
            .add_plugins(music::MusicPlugin)
            .add_systems(Startup, start_library_load)
            .add_systems(
                Update,
                (
                    reset_for_new_game,
                    poll_library_load,
                    apply_class_volumes,
                    start_level_ambient,
                    apply_audio_commands,
                    backend::attach_listener,
                )
                    .chain(),
            )
            .add_systems(FixedPostUpdate, observe_gameplay)
            .add_systems(PostUpdate, update_audio.before(TransformSystems::Propagate));
    }
}

/// Where the converted audio documents are: the converted level's
/// directory, else `ASAMU_CONVERTED_DIR`, else the importer's default.
fn audio_dir(level: Option<&ConvertedLevel>) -> Option<PathBuf> {
    let root = match level {
        Some(l) => Some(l.dir.clone()),
        None => std::env::var_os("ASAMU_CONVERTED_DIR")
            .map(ConvertedDir::new)
            .or_else(ConvertedDir::default_location),
    }?;
    let dir = root.root().join(AUDIO_DIR);
    dir.join("cues.json").is_file().then_some(dir)
}

fn start_library_load(mut runtime: ResMut<AudioRuntime>, level: Option<Res<ConvertedLevel>>) {
    begin_library_load(&mut runtime, level.as_deref());
}

fn begin_library_load(runtime: &mut AudioRuntime, level: Option<&ConvertedLevel>) {
    let Some(dir) = audio_dir(level) else {
        info!(
            "audio: no converted audio (run `asamu-import --out <dir> audio`); the game is silent"
        );
        return;
    };
    runtime.audio_dir = Some(dir.clone());
    runtime.load = Some(
        AsyncComputeTaskPool::get()
            .spawn(async move { AudioLibrary::load(&dir).map_err(|e| e.to_string()) }),
    );
}

/// A new game: every sound stops (the adaptive music resets on the same
/// command), and the documents load again when they were missing.
fn reset_for_new_game(
    mut runtime: ResMut<AudioRuntime>,
    sim: Option<Res<Sim>>,
    level: Option<Res<ConvertedLevel>>,
    mut commands: MessageWriter<AudioCommandMessage>,
) {
    if !sim.is_some_and(|s| s.is_added()) {
        return;
    }
    commands.write(AudioCommandMessage(AudioCommand::StopAll));
    if runtime.library.is_none() && runtime.load.is_none() {
        begin_library_load(&mut runtime, level.as_deref());
    }
}

/// The settings' group volumes on the sound classes (once the documents are
/// in, and whenever they change).
fn apply_class_volumes(mut runtime: ResMut<AudioRuntime>, user: Option<Res<UserSettings>>) {
    let Some(user) = user else {
        return;
    };
    if runtime.library.is_none() {
        return;
    }
    let s = &user.settings;
    let want = [s.music_volume, s.sfx_volume, s.voice_volume];
    if runtime.class_volumes == Some(want) {
        return;
    }
    for (class, volume) in ["Music", "SFX", "Voice"].into_iter().zip(want) {
        runtime.set_class_volume(class, volume);
    }
    runtime.class_volumes = Some(want);
}

fn poll_library_load(mut runtime: ResMut<AudioRuntime>, mut voices: ResMut<backend::VoiceSet>) {
    let Some(task) = runtime.load.as_mut() else {
        return;
    };
    let Some(result) = check_ready(task) else {
        return;
    };
    runtime.load = None;
    match result {
        Ok(lib) => {
            info!(
                "audio: {} cues, {} waves, {} subtitled waves, {} maps with ambient sounds",
                lib.cues.len(),
                lib.waves.len(),
                lib.subtitles.len(),
                lib.ambient.len()
            );
            for w in &lib.warnings {
                warn!("audio: {w}");
            }
            let lib = Arc::new(lib);
            if let Some(dir) = runtime.audio_dir.clone() {
                voices.preload_gameplay(&lib, &dir);
            }
            runtime.library = Some(lib);
        }
        Err(e) => warn!("audio: could not load the converted audio ({e}); the game is silent"),
    }
}

/// Starts the converted level's ambient sounds once the documents are in.
fn start_level_ambient(
    mut runtime: ResMut<AudioRuntime>,
    mut voices: ResMut<backend::VoiceSet>,
    level: Option<Res<ConvertedLevel>>,
) {
    let (Some(level), Some(lib)) = (level, runtime.library.clone()) else {
        return;
    };
    if runtime.ambient_loaded_for.as_deref() == Some(level.level.as_str()) {
        return;
    }
    let listener = runtime.listener;
    let n = runtime.engine.load_ambient(&lib, &level.level, listener);
    info!("audio: {n} ambient sound actors in {}", level.level);
    if let Some(dir) = runtime.audio_dir.clone() {
        voices.preload_ambient(&lib, &dir, &level.level);
    }
    runtime.ambient_loaded_for = Some(level.level.clone());
}

fn apply_audio_commands(
    mut runtime: ResMut<AudioRuntime>,
    mut commands: MessageReader<AudioCommandMessage>,
) {
    let rt = &mut *runtime;
    rt.queued.extend(commands.read().map(|c| c.0.clone()));
    let Some(lib) = rt.library.clone() else {
        let loading = rt.load.is_some();
        trim_waiting_commands(&mut rt.queued, loading);
        return;
    };
    for c in std::mem::take(&mut rt.queued) {
        rt.engine.apply(&lib, &c, rt.listener);
        if c == AudioCommand::StopAll {
            // A new level: the observer starts over and the level's ambient
            // sounds start again.
            rt.gameplay = GameplayAudio::default();
            rt.ambient_loaded_for = None;
        }
    }
}

/// Most audio commands kept while the documents load (ours; a level's
/// Kismet issues a few hundred at most before they arrive).
pub(crate) const MAX_WAITING_COMMANDS: usize = 4096;

/// The commands kept while the documents are not loaded: nothing plays
/// without them, so a level change (`StopAll`) drops what came before it;
/// with no load running (no converted audio, or a failed load) nothing can
/// arrive until a new game starts a load (and sends its own `StopAll`), so
/// every command is dropped; the queue never holds more than
/// [`MAX_WAITING_COMMANDS`] (the oldest go first).
fn trim_waiting_commands(queued: &mut Vec<AudioCommand>, loading: bool) {
    if !loading {
        queued.clear();
        return;
    }
    if let Some(i) = queued.iter().rposition(|c| *c == AudioCommand::StopAll) {
        queued.drain(..=i);
    }
    if queued.len() > MAX_WAITING_COMMANDS {
        let excess = queued.len() - MAX_WAITING_COMMANDS;
        queued.drain(..excess);
    }
}

/// After every fixed tick: the simulation's events → gameplay sounds.
fn observe_gameplay(mut runtime: ResMut<AudioRuntime>, sim: Option<Res<Sim>>) {
    let Some(sim) = sim else {
        return;
    };
    let rt = &mut *runtime;
    let Some(lib) = rt.library.clone() else {
        return;
    };
    rt.gameplay
        .observe(&sim.game, &mut rt.engine, &lib, rt.listener);
}

/// Every frame: update the engine and reconcile the voices.
#[allow(clippy::too_many_arguments)]
fn update_audio(
    mut runtime: ResMut<AudioRuntime>,
    mut voices: ResMut<backend::VoiceSet>,
    time: Res<Time>,
    sim: Option<Res<Sim>>,
    camera: Query<&Transform, (With<crate::PlayerCamera>, Without<backend::VoiceEntity>)>,
    subtitle: Option<ResMut<Subtitle>>,
    user: Option<Res<UserSettings>>,
    mut feedback: MessageWriter<AudioFeedbackMessage>,
    mut voice_query: backend::VoiceQuery,
    mut commands: Commands,
    mut audio_assets: ResMut<Assets<AudioSource>>,
    global_volume: Option<Res<bevy::audio::GlobalVolume>>,
) {
    let rt = &mut *runtime;
    let Some(lib) = rt.library.clone() else {
        return;
    };
    let camera = camera.iter().next().copied();
    let listener = camera.map_or(rt.listener, |t| {
        let p = t.translation;
        asamu_core::coords::bevy_pos_to_ue(UeVec3::new(p.x, p.y, p.z), crate::SCALE)
    });
    rt.listener = listener;
    if let Some(sim) = &sim {
        rt.engine
            .set_paused(sim.game.state() == asamu_game::GameState::Paused);
    }
    let frame = rt.engine.update(&lib, listener, time.delta_secs());
    for f in &frame.feedback {
        feedback.write(AudioFeedbackMessage(f.clone()));
    }
    // Subtitles off in the settings: no line.
    let line = if user.as_ref().is_none_or(|u| u.settings.subtitles) {
        frame.subtitle.clone()
    } else {
        None
    };
    if line != rt.last_subtitle {
        debug!("subtitle: {line:?}");
        rt.last_subtitle.clone_from(&line);
        if let Some(mut subtitle) = subtitle {
            subtitle.0 = line;
        }
    }
    // Bevy applies its global volume (the options menu's master volume)
    // only when a sink is created; the per-frame volume updates re-apply it.
    let global = global_volume.map_or(1.0, |g| g.volume.to_linear());
    if let Some(dir) = rt.audio_dir.clone() {
        voices.reconcile(
            &lib,
            &dir,
            &frame.voices,
            camera,
            (rt.engine.is_paused(), global),
            &mut voice_query,
            &mut commands,
            &mut audio_assets,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stop(node: u64) -> AudioCommand {
        AudioCommand::StopSound {
            cue: String::new(),
            fade_out_time: 0.0,
            node: Some(node),
        }
    }

    #[test]
    fn waiting_commands_are_bounded_and_dropped_without_a_load() {
        // While loading: a level change drops what came before it.
        let mut q = vec![stop(1), AudioCommand::StopAll, stop(2), stop(3)];
        trim_waiting_commands(&mut q, true);
        assert_eq!(q, vec![stop(2), stop(3)]);
        // The queue keeps the newest `MAX_WAITING_COMMANDS`.
        let total = MAX_WAITING_COMMANDS as u64 + 10;
        let mut q: Vec<AudioCommand> = (0..total).map(stop).collect();
        trim_waiting_commands(&mut q, true);
        assert_eq!(q.len(), MAX_WAITING_COMMANDS);
        assert_eq!(q.first(), Some(&stop(10)));
        assert_eq!(q.last(), Some(&stop(total - 1)));
        // No load running: nothing can play them.
        let mut q = vec![stop(1), stop(2)];
        trim_waiting_commands(&mut q, false);
        assert!(q.is_empty());
    }
}
