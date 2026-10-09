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
//! Kismet integration (for the orchestrator): write
//! [`AudioCommandMessage`]s (`SeqAct_PlaySound` with its action node in
//! `node`, `SeqAct_SetSoundMode`, `SeqAct_Toggle` on ambient sounds). The
//! narrator has two mutually exclusive paths: the Kismet runtime's own
//! narrator queue (`asamu-kismet`, timed by the fixed simulation tick)
//! drives the sound with `NarratorPlay` / `NarratorStop`; a host without it
//! uses this engine's queue (`NarratorAddLine` / `NarratorRemoveLine`),
//! whose timers run on the frame time, and reads
//! [`AudioFeedbackMessage`]s (narrator started / line finished / finished).

mod backend;
mod gameplay;

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
            .add_systems(Startup, start_library_load)
            .add_systems(
                Update,
                (
                    poll_library_load,
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
    let Some(dir) = audio_dir(level.as_deref()) else {
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
        return;
    };
    for c in std::mem::take(&mut rt.queued) {
        rt.engine.apply(&lib, &c, rt.listener);
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
    if frame.subtitle != rt.last_subtitle {
        rt.last_subtitle.clone_from(&frame.subtitle);
        if let Some(mut subtitle) = subtitle {
            subtitle.0.clone_from(&frame.subtitle);
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
