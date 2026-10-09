//! Bevy playback of the audio engine's voices: one entity per voice
//! (`AudioPlayer` + `PlaybackSettings`), volume and speed updated every
//! frame, despawned when the engine drops the voice.
//!
//! Wave files are read on the IO pool, validated as complete Ogg Vorbis
//! streams (`asamu_assets::audio::validate_ogg_vorbis`) and probed once
//! with Bevy's own decoder inside `catch_unwind` (Bevy unwraps the decoder
//! it creates, so data it cannot open would abort the app), then turned
//! into `AudioSource` assets directly, so no asset source has to be
//! registered and the graybox can play gameplay sounds too.
//!
//! Distance attenuation is the original's (the engine computes the gain);
//! Bevy only pans: spatial voices sit half a render unit from the listener
//! in the source's direction with the ears one unit apart, so Bevy's own
//! distance falloff (1/d², capped at 1) never applies. Its panning law is
//! Bevy's, not OpenAL's (TENTATIVE match). The distance low-pass filter is
//! not applied (Bevy sinks have no filter).

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::Duration;

use asamu_assets::audio::{AudioLibrary, Voice, WaveInfo, gameplay_cues, read_wave_file};
use bevy::audio::{AudioSinkPlayback, Decodable, PlaybackMode, SpatialListener, Volume};
use bevy::prelude::*;
use bevy::tasks::futures::check_ready;
use bevy::tasks::{IoTaskPool, Task};

use crate::PlayerCamera;

/// Distance between the listener's ears, render units (see the module
/// docs; a playback convention, not original data).
const EAR_GAP: f32 = 1.0;

/// Distance of a spatial voice from the listener, render units.
const EMITTER_RADIUS: f32 = 0.5;

/// Start a voice whose audio finished loading late part-way into its wave
/// only when it is this far in (seconds); shorter offsets start from the
/// beginning. Looping voices always start at the beginning: Bevy 0.20
/// applies the start offset inside the repeat (`skip_duration` before
/// `repeat_infinite`), so every repetition would lose that much.
const MIN_START_OFFSET: f32 = 0.05;

/// One playing voice.
#[derive(Component, Debug, Clone, Copy)]
pub struct VoiceEntity {
    /// The engine's voice id.
    pub id: u64,
}

/// The voice entities with their sinks (present once Bevy started them).
pub type VoiceQuery<'w, 's> = Query<
    'w,
    's,
    (
        Entity,
        &'static VoiceEntity,
        Option<&'static mut AudioSink>,
        Option<&'static mut SpatialAudioSink>,
        &'static mut Transform,
    ),
>;

enum WaveLoad {
    Loading(Task<Result<AudioSource, String>>),
    Ready(Handle<AudioSource>),
    Failed,
}

/// Wave files (loading or loaded) and the voice entities.
#[derive(Resource, Default)]
pub struct VoiceSet {
    loads: BTreeMap<String, WaveLoad>,
    entities: BTreeMap<u64, Entity>,
    failures: usize,
}

impl VoiceSet {
    fn start_load(&mut self, dir: &Path, wave: &WaveInfo) {
        if self.loads.contains_key(&wave.path) {
            return;
        }
        if wave.file.is_none() {
            self.loads.insert(wave.path.clone(), WaveLoad::Failed);
            return;
        }
        let dir: PathBuf = dir.to_path_buf();
        let info = wave.clone();
        let task = IoTaskPool::get().spawn(async move {
            let bytes = read_wave_file(&dir, &info).map_err(|e| e.to_string())?;
            probe_decoder(bytes)
        });
        self.loads
            .insert(wave.path.clone(), WaveLoad::Loading(task));
    }

    fn preload_cues<'a>(
        &mut self,
        lib: &AudioLibrary,
        dir: &Path,
        cues: impl Iterator<Item = &'a str>,
    ) {
        for cue in cues {
            let Some(def) = lib.cue(cue) else {
                continue;
            };
            for wave in def.wave_paths() {
                if let Some(info) = lib.wave(&wave) {
                    self.start_load(dir, info);
                }
            }
        }
    }

    /// Starts reading the waves of every gameplay cue.
    pub fn preload_gameplay(&mut self, lib: &AudioLibrary, dir: &Path) {
        use gameplay_cues as g;
        let single = [
            g::PLAYER_JUMP_GRUNT,
            g::PLAYER_LAND_GRUNT,
            g::SPRINTING_CLOTHES,
            g::SPRINTING_FOOTSTEPS_THUD,
            g::RESPAWN,
            g::FALLING_WIND,
            g::GRAPPLE_START,
            g::GRAPPLE_STOP,
            g::GRAPPLE_FAIL,
            g::GRAPPLE_RECHARGED,
            g::GRAPPLE_DECAL,
            g::GRAPPLE_BEAM,
            g::POWER_JUMP_CHARGE,
            g::POWER_JUMP_STATIC,
            g::POWER_JUMP_RELEASE,
            g::POWER_LEAP,
            g::POWER_JUMP_LIGHT,
            g::BOOST_ACTIVE,
            g::BOOST_CHARGE,
            g::BOOST_INTERRUPT,
            g::BOOST_EXHAUSTED,
            g::CRYSTAL_DRAINED,
        ];
        let tables = [
            g::FOOTSTEP_SOUNDS,
            g::JUMPING_SOUNDS,
            g::LANDING_SOUNDS,
            g::FALLING_LAND_SOUNDS,
        ];
        let cues = single
            .into_iter()
            .chain(tables.into_iter().flat_map(|t| t.iter().map(|(_, c)| *c)));
        self.preload_cues(lib, dir, cues);
    }

    /// Starts reading the waves of a map's ambient sounds.
    pub fn preload_ambient(&mut self, lib: &AudioLibrary, dir: &Path, map: &str) {
        let cues: Vec<String> = lib
            .ambient_for_map(map)
            .iter()
            .filter_map(|a| a.cue.clone())
            .collect();
        self.preload_cues(lib, dir, cues.iter().map(String::as_str));
    }

    fn poll_loads(&mut self, assets: &mut Assets<AudioSource>) {
        for (path, load) in &mut self.loads {
            let WaveLoad::Loading(task) = load else {
                continue;
            };
            let Some(result) = check_ready(task) else {
                continue;
            };
            *load = match result {
                Ok(source) => WaveLoad::Ready(assets.add(source)),
                Err(e) => {
                    self.failures += 1;
                    if self.failures <= 5 {
                        warn!("audio: cannot play {path}: {e}");
                    }
                    WaveLoad::Failed
                }
            };
        }
    }

    /// Makes the Bevy audio entities match `voices`.
    #[allow(clippy::too_many_arguments)]
    pub fn reconcile(
        &mut self,
        lib: &AudioLibrary,
        dir: &Path,
        voices: &[Voice],
        camera: Option<Transform>,
        (paused, global_volume): (bool, f32),
        query: &mut VoiceQuery,
        commands: &mut Commands,
        assets: &mut Assets<AudioSource>,
    ) {
        self.poll_loads(assets);
        let wanted: BTreeMap<u64, &Voice> = voices.iter().map(|v| (v.id, v)).collect();
        let listener = camera.map_or(Vec3::ZERO, |t| t.translation);
        let emitter = |v: &Voice| -> Vec3 {
            if !v.spatial {
                return listener;
            }
            let dir = (crate::to_render(v.location) - listener).normalize_or_zero();
            listener + dir * EMITTER_RADIUS
        };

        // Update or drop existing voices.
        let mut alive = BTreeSet::new();
        for (entity, voice, sink, spatial_sink, mut transform) in query.iter_mut() {
            let Some(v) = wanted.get(&voice.id) else {
                commands.entity(entity).despawn();
                self.entities.remove(&voice.id);
                continue;
            };
            alive.insert(voice.id);
            transform.translation = emitter(v);
            let pause = paused && !v.ui;
            if let Some(mut s) = sink {
                apply(&mut *s, v, pause, global_volume);
            }
            if let Some(mut s) = spatial_sink {
                apply(&mut *s, v, pause, global_volume);
            }
        }
        self.entities
            .retain(|id, _| alive.contains(id) || wanted.contains_key(id));

        // Start new voices whose audio is ready.
        for v in voices {
            if self.entities.contains_key(&v.id) {
                continue;
            }
            let handle = match self.loads.get(&v.wave) {
                Some(WaveLoad::Ready(h)) => h.clone(),
                Some(WaveLoad::Failed | WaveLoad::Loading(_)) => continue,
                None => {
                    if let Some(info) = lib.wave(&v.wave) {
                        self.start_load(dir, info);
                    }
                    continue;
                }
            };
            let settings = PlaybackSettings {
                mode: if v.looping {
                    PlaybackMode::Loop
                } else {
                    PlaybackMode::Once
                },
                volume: Volume::Linear(v.gain),
                speed: v.pitch,
                paused: paused && !v.ui,
                spatial: v.spatial,
                start_position: (!v.looping && v.position > MIN_START_OFFSET)
                    .then(|| Duration::try_from_secs_f32(v.position).ok())
                    .flatten(),
                ..PlaybackSettings::ONCE
            };
            let entity = commands
                .spawn((
                    Name::new("audio voice"),
                    VoiceEntity { id: v.id },
                    AudioPlayer(handle),
                    settings,
                    Transform::from_translation(emitter(v)),
                ))
                .id();
            self.entities.insert(v.id, entity);
        }
    }
}

/// Opens Bevy's decoder once on `bytes` (it unwraps internally) and turns a
/// panic into an error, so a stream it cannot open never reaches playback.
fn probe_decoder(bytes: Vec<u8>) -> Result<AudioSource, String> {
    let source = AudioSource {
        bytes: bytes.into(),
    };
    let opened = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _decoder = source.decoder();
    }));
    match opened {
        Ok(()) => Ok(source),
        Err(_) => Err("the audio decoder cannot open this stream".to_owned()),
    }
}

/// Per-frame sink update; `global_volume` is Bevy's `GlobalVolume`, which
/// Bevy itself applies only when it creates the sink.
fn apply(sink: &mut impl AudioSinkPlayback, v: &Voice, pause: bool, global_volume: f32) {
    sink.set_volume(Volume::Linear(v.gain * global_volume));
    sink.set_speed(v.pitch);
    if pause && !sink.is_paused() {
        sink.pause();
    } else if !pause && sink.is_paused() {
        sink.play();
    }
}

/// Puts the spatial listener on the player camera.
pub fn attach_listener(
    mut commands: Commands,
    cameras: Query<Entity, (With<PlayerCamera>, Without<SpatialListener>)>,
) {
    for camera in &cameras {
        commands
            .entity(camera)
            .insert(SpatialListener::new(EAR_GAP));
    }
}

#[cfg(test)]
mod tests {
    use bevy::audio::{Decodable, Source};

    use super::*;

    /// Real converted waves (`ASAMU_CONVERTED_DIR`) pass the load path
    /// (validation and the decoder probe) and decode through Bevy's decoder
    /// (no audio device needed) to the manifest's length; skips when
    /// absent. Decodes the first 64 files, or every file when
    /// `ASAMU_AUDIO_DECODE_ALL` is set.
    #[test]
    fn real_converted_waves_decode() {
        let Some(dir) = std::env::var_os("ASAMU_CONVERTED_DIR") else {
            eprintln!("skipping: ASAMU_CONVERTED_DIR not set");
            return;
        };
        let audio = Path::new(&dir).join(asamu_assets::audio::AUDIO_DIR);
        let Ok(lib) = AudioLibrary::load(&audio) else {
            eprintln!("skipping: no converted audio in {}", audio.display());
            return;
        };
        let limit = if std::env::var_os("ASAMU_AUDIO_DECODE_ALL").is_some() {
            usize::MAX
        } else {
            64
        };
        let mut decoded = 0;
        let mut exact = 0;
        let mut worst = 0.0_f32;
        let files = lib
            .waves
            .values()
            .filter(|w| w.file.as_deref().is_some_and(|f| audio.join(f).is_file()));
        for w in files.take(limit) {
            let bytes = read_wave_file(&audio, w).unwrap();
            let source = probe_decoder(bytes).unwrap();
            let decoder = source.decoder();
            let channels = u64::from(decoder.channels().get());
            let rate = decoder.sample_rate().get();
            let frames = decoder.count() as u64 / channels;
            let expected = (f64::from(w.duration) * f64::from(rate)).round() as u64;
            let diff = (frames as f32 - expected as f32).abs() / rate as f32;
            worst = worst.max(diff);
            exact += usize::from(frames == expected);
            assert!(
                diff < 0.05,
                "{}: decoded {frames} frames, manifest {expected}",
                w.path
            );
            decoded += 1;
        }
        eprintln!(
            "decoded {decoded} waves; {exact} to the exact frame count; worst difference {worst} s"
        );
        // Data the decoder cannot open is refused, not a crash.
        assert!(probe_decoder(b"OggS but not really".to_vec()).is_err());
    }
}
