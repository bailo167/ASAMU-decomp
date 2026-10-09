//! Adaptive music: the original's layered music tracks
//! (`ASAMUAdaptiveMusicManager`, `ASAMUAdaptiveMusicTrack`,
//! `ASMAMUAdaptiveVolumeMultiplier`) driven by the Kismet actions
//! `SeqAct_AddAdaptiveTracks`, `SeqAct_SetAdaptiveTrackVolumeMultiplier` and
//! `SeqAct_EditMultiplierForAllTracks`.
//!
//! The rules come from local reading of the ASAMU script, the class default
//! objects and the executable; they are described in our own words, with
//! confidence labels, in `docs/reverse-engineering/AUDIO.md`, "Adaptive
//! music". In short:
//!
//! - a level adds all of its tracks once, when the pawn spawns; every track
//!   is a cue started at once on its own audio component, so the stems of a
//!   level start together and stay in step (a muted stem keeps its voice);
//! - a track's volume is the product of its named volume multipliers, applied
//!   with `AudioComponent::AdjustVolume(time, product)`, a linear ramp over
//!   `time` seconds on the component's playback clock (the ramp rules are
//!   the audio engine's, [`AudioEngine::adjust_volume`]);
//! - track and multiplier IDs compare exactly (case-sensitive: the script's
//!   string `==` is a `wcscmp`, CONFIRMED native);
//! - beat events (`SeqEvent_TrackBeat` + `ASAMUBeatEventActor`) are a
//!   fixed-period timer per event that never looks at the audio. The Kismet
//!   runtime (`asamu-kismet`, `Runtime::tick`) already produces them, so
//!   nothing here feeds back into Kismet.
//!
//! Layers: [`MusicManager`] is the device-free, deterministic model (it
//! returns [`MusicEffect`]s); [`run_effects`] plays those on the
//! [`AudioEngine`]; [`MusicPlugin`] is the Bevy side
//! ([`MusicCommandMessage`] in, applied once the converted audio has
//! loaded; `AudioCommand::StopAll`, the level-change command, forgets every
//! track).
//!
//! Integration: the audio plugin registers [`MusicPlugin`]; the Kismet
//! presentation layer turns the runtime's outputs into commands, in emission
//! order, with [`MusicCommand::from_output`]:
//!
//! ```text
//! Output::AdaptiveTracks { tracks } => MusicCommand::AddTracks(
//!     tracks.items().iter().map(TrackSpec::from_holder).collect()),
//!     // TrackSpec::from_holder: the holder's `ID`, `trackSoundCue`,
//!     // `numberOfTracks` (field names are case-insensitive FNames)
//! Output::AdaptiveMultiplier { track, multiplier_id, multiplier, adjust_time } =>
//!     MusicCommand::edit(track.as_deref(), multiplier_id, *multiplier, *adjust_time),
//! ```
//!
//! Guards (ours, for crafted data only; the shipped levels stay far below
//! them): at most [`MAX_TRACKS`] tracks and [`MAX_QUEUED_COMMANDS`] commands
//! waiting for the audio to load.

// Parts of the original API that no Kismet action reaches (adding and
// removing multipliers, track handles, the waiting list) are kept for
// completeness and only used by the tests.
#![allow(dead_code)]

use asamu_assets::audio::{AudioCommand, AudioEngine, AudioLibrary, InstanceId, PlayParams};
use asamu_game::asamu_kismet::{KValue, Output};
use bevy::prelude::*;

use super::{AudioCommandMessage, AudioRuntime};

/// Most tracks (and parked track specs) the manager keeps. Ours, not the
/// original's: the original appends without limit, but only a crafted level
/// that re-runs `SeqAct_AddAdaptiveTracks` could grow the list; the shipped
/// levels add at most 7 tracks each.
pub const MAX_TRACKS: usize = 256;

/// Most commands held while the converted audio loads (ours, a guard: the
/// load takes seconds and the shipped levels issue a handful of commands per
/// second at most). Beyond it the oldest multiplier edit is dropped.
pub const MAX_QUEUED_COMMANDS: usize = 4096;

// ---------------------------------------------------------------------------
// Model (device-free)
// ---------------------------------------------------------------------------

/// One `TrackHolder` of `SeqAct_AddAdaptiveTracks.tracksToAdd`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TrackSpec {
    /// `ID`: the name the multiplier actions and beat events use.
    pub id: String,
    /// `trackSoundCue` (object path); `None` plays nothing.
    pub cue: Option<String>,
    /// `numberOfTracks`: stored on the track as its "number of bars" and
    /// never read by the script (0 in every shipped track).
    pub bars: i32,
}

impl TrackSpec {
    /// A track from the `TrackHolder` fields (a missing ID is the empty
    /// string, the `StrProperty` default).
    #[must_use]
    pub fn new(id: Option<&str>, cue: Option<&str>, bars: i32) -> Self {
        Self {
            id: id.unwrap_or_default().to_owned(),
            cue: cue.filter(|c| !c.is_empty()).map(str::to_owned),
            bars,
        }
    }

    /// A track from one `TrackHolder` struct of the Kismet graph (`ID`,
    /// `trackSoundCue`, `numberOfTracks`; the converted graph stores the ID
    /// field as `Id`, field lookup is case-insensitive like UE3's names).
    #[must_use]
    pub fn from_holder(holder: &KValue) -> Self {
        Self::new(
            holder.field("ID").and_then(KValue::as_str),
            holder.field("trackSoundCue").and_then(KValue::as_obj),
            holder.field("numberOfTracks").map_or(0, KValue::as_int),
        )
    }
}

/// One `ASMAMUAdaptiveVolumeMultiplier` (the class default stores ID
/// `"NONE"` and -1.0; the script overwrites both when it creates one).
#[derive(Clone, Debug, PartialEq)]
pub struct VolumeMultiplier {
    /// Multiplier ID (every shipped action uses `"Mute"`).
    pub id: String,
    /// Factor.
    pub value: f32,
}

/// Index of a track in the manager's list (what a
/// `SeqVar_ASAMUAdaptiveMusicTrack` would hold). Tracks are never removed
/// while a level runs, so a handle stays valid until a reset.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TrackHandle(pub usize);

/// One `ASAMUAdaptiveMusicTrack`: a cue on its own audio component and the
/// multipliers that set its volume.
#[derive(Clone, Debug, PartialEq)]
pub struct MusicTrack {
    id: String,
    cue: Option<String>,
    bars: i32,
    multipliers: Vec<VolumeMultiplier>,
    /// `currentAdjustVolumeTime`: the ramp time of the latest change.
    adjust_time: f32,
    instance: Option<InstanceId>,
}

impl MusicTrack {
    fn new(spec: TrackSpec) -> Self {
        Self {
            id: spec.id,
            cue: spec.cue,
            bars: spec.bars,
            multipliers: Vec::new(),
            adjust_time: 0.0,
            instance: None,
        }
    }

    /// Track ID.
    #[must_use]
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Cue object path.
    #[must_use]
    pub fn cue(&self) -> Option<&str> {
        self.cue.as_deref()
    }

    /// `numberOfBars` (unused by the original).
    #[must_use]
    pub fn bars(&self) -> i32 {
        self.bars
    }

    /// The multipliers, in the order they were added.
    #[must_use]
    pub fn multipliers(&self) -> &[VolumeMultiplier] {
        &self.multipliers
    }

    /// Ramp time of the latest multiplier change.
    #[must_use]
    pub fn adjust_time(&self) -> f32 {
        self.adjust_time
    }

    /// The playing cue instance (the track's audio component), if its cue
    /// started.
    #[must_use]
    pub fn instance(&self) -> Option<InstanceId> {
        self.instance
    }

    /// The volume the track asks for: the product of its multipliers in list
    /// order, starting from 1 (single-precision, as the script computes it).
    #[must_use]
    pub fn volume(&self) -> f32 {
        self.multipliers.iter().fold(1.0_f32, |v, m| v * m.value)
    }

    fn position(&self, multiplier_id: &str) -> Option<usize> {
        self.multipliers.iter().position(|m| m.id == multiplier_id)
    }

    /// `ActivateAllMultipliers`.
    fn activate(&self, track: usize) -> MusicEffect {
        MusicEffect::AdjustVolume {
            track,
            duration: self.adjust_time,
            volume: self.volume(),
        }
    }

    /// `AddVolumeMultiplier`: always appends (an existing ID gets a second
    /// record).
    fn add_multiplier(&mut self, track: usize, id: &str, value: f32, time: f32) -> MusicEffect {
        self.multipliers.push(VolumeMultiplier {
            id: id.to_owned(),
            value,
        });
        self.adjust_time = time;
        self.activate(track)
    }

    /// `EditVolumeMultiplier`: the first record with the ID takes the new
    /// value; without one, the multiplier is added.
    fn edit_multiplier(&mut self, track: usize, id: &str, value: f32, time: f32) -> MusicEffect {
        match self.position(id) {
            Some(i) => {
                if let Some(m) = self.multipliers.get_mut(i) {
                    m.value = value;
                }
                self.adjust_time = time;
                self.activate(track)
            }
            None => self.add_multiplier(track, id, value, time),
        }
    }

    /// `RemoveVolumeMultiplier`: removes the first record with the ID (if
    /// any), then re-applies the volume with the new time in every case.
    fn remove_multiplier(&mut self, track: usize, id: &str, time: f32) -> MusicEffect {
        if let Some(i) = self.position(id) {
            self.multipliers.remove(i);
        }
        self.adjust_time = time;
        self.activate(track)
    }
}

/// What the manager asks of the audio engine, in order.
#[derive(Clone, Debug, PartialEq)]
pub enum MusicEffect {
    /// `audioComp.Play()` on the track's new component (its cue set first).
    Play {
        /// Track index.
        track: usize,
    },
    /// `audioComp.AdjustVolume(duration, volume)`.
    AdjustVolume {
        /// Track index.
        track: usize,
        /// Ramp seconds.
        duration: f32,
        /// Target volume.
        volume: f32,
    },
    /// Stop a component (the level's tracks go away).
    Stop(InstanceId),
    /// `EditMultiplierTrack` found no track with this ID (the script logs it
    /// and changes nothing).
    TrackNotFound {
        /// The ID asked for.
        id: String,
    },
    /// A track was not added because the manager already holds
    /// [`MAX_TRACKS`] (our guard; never reached by the shipped levels).
    TrackRefused {
        /// The ID of the refused track.
        id: String,
    },
}

/// Which tracks a multiplier change targets.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TrackTarget {
    /// The first track with this ID (`SeqAct_SetAdaptiveTrackVolumeMultiplier`
    /// without a track variable: `EditMultiplierTrack`).
    Id(String),
    /// Every track, in list order (`SeqAct_EditMultiplierForAllTracks`:
    /// `EditAllMultipliers`).
    All,
    /// One track by handle (`SeqAct_SetAdaptiveTrackVolumeMultiplier` with a
    /// `SeqVar_ASAMUAdaptiveMusicTrack`; no shipped action links one).
    Handle(TrackHandle),
}

/// A request for the adaptive music.
#[derive(Clone, Debug, PartialEq)]
pub enum MusicCommand {
    /// `SeqAct_AddAdaptiveTracks` (run by the music manager when the pawn
    /// spawns): add and start these tracks, in order.
    AddTracks(Vec<TrackSpec>),
    /// Edit (or create) a volume multiplier.
    EditMultiplier {
        /// Which tracks.
        target: TrackTarget,
        /// `multiplierID`.
        multiplier_id: String,
        /// `Multiplier`.
        multiplier: f32,
        /// `adjustTime` (seconds).
        adjust_time: f32,
    },
    /// Forget every track and stop its sound (level change).
    Reset,
}

impl MusicCommand {
    /// A multiplier edit from the Kismet runtime's `Output::AdaptiveMultiplier`
    /// (`track` `None`: every track).
    #[must_use]
    pub fn edit(
        track: Option<&str>,
        multiplier_id: &str,
        multiplier: f32,
        adjust_time: f32,
    ) -> Self {
        Self::EditMultiplier {
            target: track.map_or(TrackTarget::All, |t| TrackTarget::Id(t.to_owned())),
            multiplier_id: multiplier_id.to_owned(),
            multiplier,
            adjust_time,
        }
    }

    /// The command for one of the Kismet runtime's outputs:
    /// `Output::AdaptiveTracks` (the tracks of a `SeqAct_AddAdaptiveTracks`,
    /// in order) and `Output::AdaptiveMultiplier` (both multiplier actions;
    /// `track` `None` is `SeqAct_EditMultiplierForAllTracks`). Any other
    /// output is not music (`None`).
    #[must_use]
    pub fn from_output(output: &Output) -> Option<Self> {
        match output {
            Output::AdaptiveTracks { tracks } => Some(Self::AddTracks(
                tracks.items().iter().map(TrackSpec::from_holder).collect(),
            )),
            Output::AdaptiveMultiplier {
                track,
                multiplier_id,
                multiplier,
                adjust_time,
            } => Some(Self::edit(
                track.as_deref(),
                multiplier_id,
                *multiplier,
                *adjust_time,
            )),
            _ => None,
        }
    }
}

/// `ASAMUAdaptiveMusicManager`: the level's tracks, in the order they were
/// added. Deterministic; knows nothing about audio devices.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct MusicManager {
    tracks: Vec<MusicTrack>,
    /// `tracksWaitingToBeAdded`: tracks added while no pawn existed. The
    /// shipped script never adds them later.
    waiting: Vec<TrackSpec>,
}

impl MusicManager {
    /// The tracks, in list order.
    #[must_use]
    pub fn tracks(&self) -> &[MusicTrack] {
        &self.tracks
    }

    /// Tracks parked because no pawn existed when they were added.
    #[must_use]
    pub fn waiting(&self) -> &[TrackSpec] {
        &self.waiting
    }

    /// `GetTrackFromID`: the first track whose ID equals `id` exactly.
    #[must_use]
    pub fn handle(&self, id: &str) -> Option<TrackHandle> {
        self.tracks.iter().position(|t| t.id == id).map(TrackHandle)
    }

    /// The first track whose ID equals `id` exactly.
    #[must_use]
    pub fn track(&self, id: &str) -> Option<&MusicTrack> {
        self.handle(id).and_then(|h| self.get(h))
    }

    /// The track behind a handle.
    #[must_use]
    pub fn get(&self, handle: TrackHandle) -> Option<&MusicTrack> {
        self.tracks.get(handle.0)
    }

    /// `AddTrack`: with a pawn, a new track is appended (no check for an
    /// existing ID) and started; without one the spec is parked. Beyond
    /// [`MAX_TRACKS`] (our guard) the track is refused and reported.
    pub fn add_track(
        &mut self,
        spec: TrackSpec,
        pawn_present: bool,
        out: &mut Vec<MusicEffect>,
    ) -> Option<TrackHandle> {
        let list = if pawn_present {
            self.tracks.len()
        } else {
            self.waiting.len()
        };
        if list >= MAX_TRACKS {
            out.push(MusicEffect::TrackRefused { id: spec.id });
            return None;
        }
        if !pawn_present {
            self.waiting.push(spec);
            return None;
        }
        let index = self.tracks.len();
        self.tracks.push(MusicTrack::new(spec));
        out.push(MusicEffect::Play { track: index });
        Some(TrackHandle(index))
    }

    /// `SeqAct_AddAdaptiveTracks.Activated`: every spec in order.
    pub fn add_tracks(&mut self, specs: &[TrackSpec], pawn_present: bool) -> Vec<MusicEffect> {
        let mut out = Vec::new();
        for s in specs {
            self.add_track(s.clone(), pawn_present, &mut out);
        }
        out
    }

    /// `AddMultiplierToTrack` (no shipped script calls it): appends a
    /// multiplier to the first track with the ID; nothing when missing.
    pub fn add_multiplier_to_track(
        &mut self,
        track_id: &str,
        multiplier_id: &str,
        multiplier: f32,
        adjust_time: f32,
    ) -> Vec<MusicEffect> {
        let Some(TrackHandle(i)) = self.handle(track_id) else {
            return Vec::new();
        };
        self.tracks
            .get_mut(i)
            .map(|t| t.add_multiplier(i, multiplier_id, multiplier, adjust_time))
            .into_iter()
            .collect()
    }

    /// `EditMultiplierTrack`: edits the multiplier of the first track with
    /// the ID; a missing track is reported and nothing changes.
    pub fn edit_multiplier_track(
        &mut self,
        track_id: &str,
        multiplier_id: &str,
        multiplier: f32,
        adjust_time: f32,
    ) -> Vec<MusicEffect> {
        match self.handle(track_id) {
            Some(h) => self.edit_multiplier_at(h, multiplier_id, multiplier, adjust_time),
            None => vec![MusicEffect::TrackNotFound {
                id: track_id.to_owned(),
            }],
        }
    }

    /// `EditVolumeMultiplier` on one track (the track-variable path; an
    /// unknown handle changes nothing, like a script access to `None`).
    pub fn edit_multiplier_at(
        &mut self,
        handle: TrackHandle,
        multiplier_id: &str,
        multiplier: f32,
        adjust_time: f32,
    ) -> Vec<MusicEffect> {
        let i = handle.0;
        self.tracks
            .get_mut(i)
            .map(|t| t.edit_multiplier(i, multiplier_id, multiplier, adjust_time))
            .into_iter()
            .collect()
    }

    /// `EditAllMultipliers`: every track, in list order.
    pub fn edit_all_multipliers(
        &mut self,
        multiplier_id: &str,
        multiplier: f32,
        adjust_time: f32,
    ) -> Vec<MusicEffect> {
        self.tracks
            .iter_mut()
            .enumerate()
            .map(|(i, t)| t.edit_multiplier(i, multiplier_id, multiplier, adjust_time))
            .collect()
    }

    /// `RemoveMultiplierFromTrack` (no shipped script calls it): removes the
    /// first record with the ID from the first track with the ID; the
    /// volume is re-applied even when nothing was removed.
    pub fn remove_multiplier_from_track(
        &mut self,
        track_id: &str,
        multiplier_id: &str,
        adjust_time: f32,
    ) -> Vec<MusicEffect> {
        let Some(TrackHandle(i)) = self.handle(track_id) else {
            return Vec::new();
        };
        self.tracks
            .get_mut(i)
            .map(|t| t.remove_multiplier(i, multiplier_id, adjust_time))
            .into_iter()
            .collect()
    }

    /// Records the component that plays a track's cue.
    pub fn set_instance(&mut self, handle: TrackHandle, instance: Option<InstanceId>) {
        if let Some(t) = self.tracks.get_mut(handle.0) {
            t.instance = instance;
        }
    }

    /// Forgets every track; returns a stop for each started component.
    pub fn clear(&mut self) -> Vec<MusicEffect> {
        let out = self
            .tracks
            .iter()
            .filter_map(|t| t.instance.map(MusicEffect::Stop))
            .collect();
        self.tracks.clear();
        self.waiting.clear();
        out
    }

    /// Applies a command; `pawn_present` as for [`MusicManager::add_track`].
    pub fn apply(&mut self, command: &MusicCommand, pawn_present: bool) -> Vec<MusicEffect> {
        match command {
            MusicCommand::AddTracks(specs) => self.add_tracks(specs, pawn_present),
            MusicCommand::EditMultiplier {
                target,
                multiplier_id,
                multiplier,
                adjust_time,
            } => match target {
                TrackTarget::Id(id) => {
                    self.edit_multiplier_track(id, multiplier_id, *multiplier, *adjust_time)
                }
                TrackTarget::All => {
                    self.edit_all_multipliers(multiplier_id, *multiplier, *adjust_time)
                }
                TrackTarget::Handle(h) => {
                    self.edit_multiplier_at(*h, multiplier_id, *multiplier, *adjust_time)
                }
            },
            MusicCommand::Reset => self.clear(),
        }
    }
}

/// The audio component settings of a track: the track class's component
/// template stores no properties, so the engine's `AudioComponent` defaults
/// apply (spatialisation allowed, no subtitles, not kept when dropped). The
/// track actor is spawned at the pawn and based on it, so the sound follows
/// the pawn (TENTATIVE: the component is not in the actor's component list;
/// no shipped music cue has an attenuation node, so its position is never
/// heard).
fn track_params(at: asamu_core::glam::Vec3) -> PlayParams {
    PlayParams::at(at)
}

/// Plays `effects` on the engine. Returns log lines (unknown cues, edits of
/// missing tracks). Deterministic.
pub fn run_effects(
    manager: &mut MusicManager,
    effects: &[MusicEffect],
    engine: &mut AudioEngine,
    lib: &AudioLibrary,
) -> Vec<String> {
    let mut log = Vec::new();
    for e in effects {
        match e {
            MusicEffect::Play { track } => {
                let handle = TrackHandle(*track);
                let Some(t) = manager.get(handle) else {
                    continue;
                };
                // `Play()` without a cue plays nothing.
                let Some(cue) = t.cue.clone() else {
                    continue;
                };
                let at = engine.player_location();
                let id = engine.play(lib, &cue, track_params(at), at);
                match id {
                    Some(id) => engine.follow_player(id),
                    None => log.push(format!(
                        "music: cue {cue} of track {:?} did not start (not converted, or already \
                         playing its MaxConcurrentPlayCount times)",
                        t.id
                    )),
                }
                manager.set_instance(handle, id);
            }
            MusicEffect::AdjustVolume {
                track,
                duration,
                volume,
            } => {
                if let Some(id) = manager.get(TrackHandle(*track)).and_then(|t| t.instance) {
                    engine.adjust_volume(id, *duration, *volume);
                }
            }
            MusicEffect::Stop(id) => engine.stop(*id),
            MusicEffect::TrackNotFound { id } => {
                log.push(format!("music: multiplier edit for unknown track {id:?}"));
            }
            MusicEffect::TrackRefused { id } => {
                log.push(format!(
                    "music: track {id:?} refused (more than {MAX_TRACKS} tracks)"
                ));
            }
        }
    }
    log
}

// ---------------------------------------------------------------------------
// Bevy
// ---------------------------------------------------------------------------

/// A request for the adaptive music (from the Kismet runtime's outputs).
#[derive(Message, Clone, Debug, PartialEq)]
pub struct MusicCommandMessage(pub MusicCommand);

/// The adaptive music state of the running level.
#[derive(Resource, Debug, Default)]
pub struct AdaptiveMusic {
    manager: MusicManager,
    /// Commands that arrived before the converted audio finished loading.
    queued: Vec<MusicCommand>,
}

impl AdaptiveMusic {
    /// The manager (tracks, multipliers, components).
    #[must_use]
    pub fn manager(&self) -> &MusicManager {
        &self.manager
    }

    /// Commands waiting for the converted audio.
    #[must_use]
    pub fn queued(&self) -> &[MusicCommand] {
        &self.queued
    }

    fn reset(&mut self, engine: &mut AudioEngine) {
        for e in self.manager.clear() {
            if let MusicEffect::Stop(id) = e {
                engine.stop(id);
            }
        }
        self.queued.clear();
    }

    /// Holds a command until the converted audio is in. A reset makes
    /// everything before it moot; at [`MAX_QUEUED_COMMANDS`] the oldest
    /// multiplier edit goes (track additions are kept; with no edit to drop
    /// the new command is refused). Returns whether something was dropped.
    fn enqueue(&mut self, command: MusicCommand) -> bool {
        if command == MusicCommand::Reset {
            self.queued.clear();
        }
        let mut dropped = false;
        if self.queued.len() >= MAX_QUEUED_COMMANDS {
            dropped = true;
            match self
                .queued
                .iter()
                .position(|c| matches!(c, MusicCommand::EditMultiplier { .. }))
            {
                Some(i) => {
                    self.queued.remove(i);
                }
                None => return dropped,
            }
        }
        self.queued.push(command);
        dropped
    }

    /// Applies one command. Our Kismet runtime only emits the track-adding
    /// activation that runs once the pawn exists (the earlier one, at game
    /// start, parks the tracks in a list nothing reads), so tracks are
    /// always added with a pawn.
    fn apply(
        &mut self,
        command: &MusicCommand,
        engine: &mut AudioEngine,
        lib: &AudioLibrary,
    ) -> Vec<String> {
        let effects = self.manager.apply(command, true);
        run_effects(&mut self.manager, &effects, engine, lib)
    }
}

/// Adaptive music plugin. Register it with the audio plugin
/// (`app.add_plugins(music::MusicPlugin)`); it also works on its own (it
/// registers what it reads).
pub struct MusicPlugin;

impl Plugin for MusicPlugin {
    fn build(&self, app: &mut App) {
        app.add_message::<MusicCommandMessage>()
            .add_message::<AudioCommandMessage>()
            .init_resource::<AudioRuntime>()
            .init_resource::<AdaptiveMusic>()
            .add_systems(
                Update,
                apply_music_commands.after(super::apply_audio_commands),
            );
    }
}

/// Every frame: a level change (`AudioCommand::StopAll`) forgets the tracks
/// and any command still waiting; then the queued and new commands apply,
/// in order, once the converted audio is in (the audio engine then ramps the
/// volumes on its own update). A level change is handled before this
/// frame's commands, so the next level's tracks survive it.
///
/// Without the documents and with no load running, commands are dropped:
/// nothing could ever play them, since a load only starts again with a new
/// game, whose `StopAll` resets the music anyway (this keeps a silent game
/// from queueing a level's edits forever).
fn apply_music_commands(
    mut music: ResMut<AdaptiveMusic>,
    mut runtime: ResMut<AudioRuntime>,
    mut incoming: MessageReader<MusicCommandMessage>,
    mut audio_commands: MessageReader<AudioCommandMessage>,
) {
    let music = &mut *music;
    let rt = &mut *runtime;
    let mut stop_all = false;
    for c in audio_commands.read() {
        stop_all |= matches!(c.0, AudioCommand::StopAll);
    }
    if stop_all {
        music.reset(&mut rt.engine);
    }
    let Some(lib) = rt.library.clone() else {
        if rt.load.is_none() {
            incoming.clear();
            music.queued.clear();
            return;
        }
        let mut dropped = false;
        for m in incoming.read() {
            dropped |= music.enqueue(m.0.clone());
        }
        if dropped {
            warn!(
                "music: more than {MAX_QUEUED_COMMANDS} commands waiting for the audio; oldest edits dropped"
            );
        }
        return;
    };
    // The documents are in: what waited first, then this frame's commands,
    // all in order (no cap here).
    let waiting = std::mem::take(&mut music.queued);
    let new = incoming.read().map(|m| &m.0);
    for command in waiting.iter().chain(new) {
        for line in music.apply(command, &mut rt.engine, &lib) {
            debug!("{line}");
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::path::Path;
    use std::sync::Arc;

    use asamu_assets::audio::{AUDIO_DIR, CueDef, NodeDef, NodeKind, WaveInfo, WaveRef};
    use asamu_core::glam::Vec3 as UeVec3;
    use asamu_game::asamu_kismet::{NullHost, Runtime, load_level_scripts};

    use super::*;

    fn spec(id: &str, cue: &str) -> TrackSpec {
        TrackSpec::new(Some(id), Some(cue), 0)
    }

    /// A cue `<name>` that loops wave `<name>.W` forever (the shape of the
    /// shipped stems: a looping node over one wave, no attenuation).
    fn looping_cue(name: &str) -> CueDef {
        looping_cue_with_volume(name, 1.0)
    }

    /// [`looping_cue`] with a cue `VolumeMultiplier`.
    fn looping_cue_with_volume(name: &str, volume_multiplier: f32) -> CueDef {
        CueDef {
            path: name.to_owned(),
            sound_class: None,
            volume_multiplier,
            pitch_multiplier: 1.0,
            duration: Some(10000.0),
            max_concurrent_play_count: 16,
            first: Some(0),
            nodes: vec![
                NodeDef {
                    path: format!("{name}.Loop"),
                    kind: NodeKind::Looping {
                        indefinitely: true,
                        count_min: 1_000_000.0,
                        count_max: 1_000_000.0,
                    },
                    children: vec![Some(1)],
                },
                NodeDef {
                    path: format!("{name}.W"),
                    kind: NodeKind::Wave(WaveRef {
                        path: format!("{name}.W"),
                        volume: 1.0,
                        pitch: 1.0,
                    }),
                    children: Vec::new(),
                },
            ],
        }
    }

    /// Two mono stems of 4 s, no sound class (gain = component volume).
    fn library() -> AudioLibrary {
        library_of(&[("StemA", 1.0), ("StemB", 1.0)])
    }

    /// Looping mono stems of 4 s (name, cue `VolumeMultiplier`), no sound
    /// class (gain = component volume × cue volume).
    fn library_of(stems: &[(&str, f32)]) -> AudioLibrary {
        let mut lib = AudioLibrary::default();
        for (name, volume) in stems {
            lib.add_cue(looping_cue_with_volume(name, *volume));
            lib.add_wave(WaveInfo {
                path: format!("{name}.W"),
                file: None,
                duration: 4.0,
                channels: 1,
                volume: 1.0,
                pitch: 1.0,
            });
        }
        lib
    }

    /// `n` (at least one) audio updates of `dt`; the last frame.
    fn run_frames(
        engine: &mut AudioEngine,
        lib: &AudioLibrary,
        n: usize,
        dt: f32,
    ) -> asamu_assets::audio::AudioFrame {
        let mut f = engine.update(lib, UeVec3::ZERO, dt);
        for _ in 1..n {
            f = engine.update(lib, UeVec3::ZERO, dt);
        }
        f
    }

    fn apply_all(
        m: &mut MusicManager,
        commands: &[MusicCommand],
        engine: &mut AudioEngine,
        lib: &AudioLibrary,
    ) -> Vec<String> {
        let mut log = Vec::new();
        for c in commands {
            let fx = m.apply(c, true);
            log.extend(run_effects(m, &fx, engine, lib));
        }
        log
    }

    fn gain_of(frame: &asamu_assets::audio::AudioFrame, id: Option<InstanceId>) -> Option<f32> {
        let id = id?;
        frame
            .voices
            .iter()
            .find(|v| v.instance == id)
            .map(|v| v.gain)
    }

    // ------------------------------------------------------------ model

    #[test]
    fn ids_compare_exactly_and_the_first_match_wins() {
        let mut m = MusicManager::default();
        let fx = m.add_tracks(
            &[spec("Heart", "a"), spec("heart", "b"), spec("Heart", "c")],
            true,
        );
        // Every track starts, in order; a repeated ID is not refused.
        assert_eq!(
            fx,
            vec![
                MusicEffect::Play { track: 0 },
                MusicEffect::Play { track: 1 },
                MusicEffect::Play { track: 2 },
            ]
        );
        assert_eq!(m.handle("Heart"), Some(TrackHandle(0)));
        assert_eq!(m.handle("heart"), Some(TrackHandle(1)));
        assert_eq!(m.handle("HEART"), None);
        assert_eq!(m.track("Heart").and_then(MusicTrack::cue), Some("a"));
        // An edit by ID reaches only the first "Heart".
        let fx = m.edit_multiplier_track("Heart", "Mute", 0.0, 0.0);
        assert_eq!(
            fx,
            vec![MusicEffect::AdjustVolume {
                track: 0,
                duration: 0.0,
                volume: 0.0
            }]
        );
        assert!(m.tracks()[2].multipliers().is_empty());
        // Multiplier IDs compare exactly too: "mute" is a second record.
        m.edit_multiplier_track("Heart", "mute", 0.5, 0.0);
        assert_eq!(m.tracks()[0].multipliers().len(), 2);
    }

    #[test]
    fn edit_creates_then_updates_and_the_volume_is_the_product() {
        let mut m = MusicManager::default();
        m.add_tracks(&[spec("T", "c")], true);
        let adjust = |fx: Vec<MusicEffect>| match fx.as_slice() {
            [
                MusicEffect::AdjustVolume {
                    track: 0,
                    duration,
                    volume,
                },
            ] => (*duration, *volume),
            other => panic!("{other:?}"),
        };
        // First edit creates the multiplier.
        assert_eq!(
            adjust(m.edit_multiplier_track("T", "Mute", 0.0, 0.0)),
            (0.0, 0.0)
        );
        // Second edit changes it in place, with its own ramp time.
        assert_eq!(
            adjust(m.edit_multiplier_track("T", "Mute", 0.8, 2.0)),
            (2.0, 0.8)
        );
        assert_eq!(m.tracks()[0].multipliers().len(), 1);
        // A second multiplier multiplies.
        assert_eq!(
            adjust(m.add_multiplier_to_track("T", "Duck", 0.5, 1.0)),
            (1.0, 0.8 * 0.5)
        );
        // Add never merges: a second "Duck" record.
        assert_eq!(
            adjust(m.add_multiplier_to_track("T", "Duck", 0.5, 1.0)),
            (1.0, 0.8 * 0.5 * 0.5)
        );
        // Remove takes the first matching record only.
        assert_eq!(
            adjust(m.remove_multiplier_from_track("T", "Duck", 0.25)),
            (0.25, 0.8 * 0.5)
        );
        // Removing a missing multiplier still re-applies with the new time.
        assert_eq!(
            adjust(m.remove_multiplier_from_track("T", "Nope", 3.0)),
            (3.0, 0.8 * 0.5)
        );
        assert_eq!(m.tracks()[0].adjust_time(), 3.0);
        // Add/remove on a missing track do nothing (no log either).
        assert!(m.add_multiplier_to_track("X", "Mute", 1.0, 0.0).is_empty());
        assert!(m.remove_multiplier_from_track("X", "Mute", 0.0).is_empty());
    }

    #[test]
    fn the_product_is_folded_in_list_order_in_single_precision() {
        let mut m = MusicManager::default();
        m.add_tracks(&[spec("T", "c")], true);
        let values = [0.1_f32, 0.7, 0.3, 1.1];
        for (i, v) in values.iter().enumerate() {
            m.edit_multiplier_track("T", &format!("m{i}"), *v, 0.0);
        }
        let expected = ((((1.0_f32 * 0.1) * 0.7) * 0.3) * 1.1).to_bits();
        assert_eq!(m.tracks()[0].volume().to_bits(), expected);
        // No multipliers: full volume.
        m.add_tracks(&[spec("U", "c")], true);
        assert_eq!(m.tracks()[1].volume(), 1.0);
    }

    #[test]
    fn edit_all_reaches_every_track_in_order_and_unknown_ids_are_reported() {
        let mut m = MusicManager::default();
        m.add_tracks(&[spec("A", "a"), spec("B", "b"), spec("C", "c")], true);
        let fx = m.apply(&MusicCommand::edit(None, "Mute", 0.0, 0.0), true);
        let tracks: Vec<usize> = fx
            .iter()
            .map(|e| match e {
                MusicEffect::AdjustVolume { track, volume, .. } => {
                    assert_eq!(*volume, 0.0);
                    *track
                }
                other => panic!("{other:?}"),
            })
            .collect();
        assert_eq!(tracks, vec![0, 1, 2]);
        let fx = m.apply(&MusicCommand::edit(Some("Z"), "Mute", 1.0, 0.0), true);
        assert_eq!(fx, vec![MusicEffect::TrackNotFound { id: "Z".into() }]);
        // An empty ID (a missing trackID) is not "all tracks".
        let fx = m.apply(&MusicCommand::edit(Some(""), "Mute", 1.0, 0.0), true);
        assert_eq!(fx, vec![MusicEffect::TrackNotFound { id: String::new() }]);
        // The handle path; an unknown handle changes nothing.
        let fx = m.apply(
            &MusicCommand::EditMultiplier {
                target: TrackTarget::Handle(TrackHandle(1)),
                multiplier_id: "Mute".into(),
                multiplier: 0.7,
                adjust_time: 0.3,
            },
            true,
        );
        assert_eq!(
            fx,
            vec![MusicEffect::AdjustVolume {
                track: 1,
                duration: 0.3,
                volume: 0.7
            }]
        );
        assert!(
            m.edit_multiplier_at(TrackHandle(9), "Mute", 1.0, 0.0)
                .is_empty()
        );
    }

    #[test]
    fn without_a_pawn_tracks_wait_and_nothing_plays() {
        let mut m = MusicManager::default();
        let fx = m.add_tracks(&[spec("A", "a"), spec("B", "b")], false);
        assert!(fx.is_empty());
        assert!(m.tracks().is_empty());
        assert_eq!(m.waiting().len(), 2);
        // The pawn-time activation adds them (the parked ones stay parked).
        let fx = m.add_tracks(&[spec("A", "a"), spec("B", "b")], true);
        assert_eq!(fx.len(), 2);
        assert_eq!(m.tracks().len(), 2);
        assert_eq!(m.waiting().len(), 2);
        // Reset forgets both lists.
        m.set_instance(TrackHandle(1), Some(InstanceId(7)));
        assert_eq!(
            m.apply(&MusicCommand::Reset, true),
            vec![MusicEffect::Stop(InstanceId(7))]
        );
        assert!(m.tracks().is_empty() && m.waiting().is_empty());
    }

    #[test]
    fn kismet_fields_map_to_specs_and_commands() {
        assert_eq!(
            TrackSpec::new(None, None, 3),
            TrackSpec {
                id: String::new(),
                cue: None,
                bars: 3
            }
        );
        assert_eq!(TrackSpec::new(Some("X"), Some(""), 0).cue, None);
        assert_eq!(
            MusicCommand::edit(Some("A"), "Mute", 0.5, 2.0),
            MusicCommand::EditMultiplier {
                target: TrackTarget::Id("A".into()),
                multiplier_id: "Mute".into(),
                multiplier: 0.5,
                adjust_time: 2.0
            }
        );
    }

    #[test]
    fn kismet_outputs_become_commands() {
        // The converted graph stores the holder's ID field as `Id`.
        let holder = KValue::Struct(BTreeMap::from([
            ("Id".to_owned(), KValue::Str("Drums".into())),
            (
                "trackSoundCue".to_owned(),
                KValue::Obj(Some("Pkg.Drums_Cue".into())),
            ),
            ("numberOfTracks".to_owned(), KValue::Int(4)),
        ]));
        let empty = KValue::Struct(BTreeMap::new());
        let null_cue = KValue::Struct(BTreeMap::from([
            ("ID".to_owned(), KValue::Str("Silent".into())),
            ("trackSoundCue".to_owned(), KValue::Obj(None)),
        ]));
        let tracks = KValue::Array(vec![holder, empty, null_cue]);
        assert_eq!(
            MusicCommand::from_output(&Output::AdaptiveTracks { tracks }),
            Some(MusicCommand::AddTracks(vec![
                TrackSpec::new(Some("Drums"), Some("Pkg.Drums_Cue"), 4),
                TrackSpec::new(None, None, 0),
                TrackSpec::new(Some("Silent"), None, 0),
            ]))
        );
        // A non-array `tracksToAdd` adds nothing.
        assert_eq!(
            MusicCommand::from_output(&Output::AdaptiveTracks {
                tracks: KValue::None
            }),
            Some(MusicCommand::AddTracks(Vec::new()))
        );
        let edit = |track: Option<&str>| Output::AdaptiveMultiplier {
            track: track.map(str::to_owned),
            multiplier_id: "Mute".into(),
            multiplier: 0.7,
            adjust_time: 0.3,
        };
        assert_eq!(
            MusicCommand::from_output(&edit(None)),
            Some(MusicCommand::edit(None, "Mute", 0.7, 0.3))
        );
        assert_eq!(
            MusicCommand::from_output(&edit(Some("Searching"))),
            Some(MusicCommand::edit(Some("Searching"), "Mute", 0.7, 0.3))
        );
        assert_eq!(
            MusicCommand::from_output(&Output::PauseMenu { enabled: true }),
            None
        );
    }

    #[test]
    fn edits_without_tracks_and_stale_handles_change_nothing() {
        let mut m = MusicManager::default();
        assert!(m.edit_all_multipliers("Mute", 0.0, 0.0).is_empty());
        assert_eq!(
            m.edit_multiplier_track("A", "Mute", 0.0, 0.0),
            vec![MusicEffect::TrackNotFound { id: "A".into() }]
        );
        assert!(
            m.edit_multiplier_at(TrackHandle(0), "Mute", 0.0, 0.0)
                .is_empty()
        );
        m.add_tracks(&[spec("A", "a")], true);
        let h = m.handle("A");
        assert_eq!(h, Some(TrackHandle(0)));
        m.clear();
        // A handle from before the reset reaches nothing.
        assert!(
            m.edit_multiplier_at(TrackHandle(0), "Mute", 0.0, 0.0)
                .is_empty()
        );
        assert!(m.tracks().is_empty());
    }

    #[test]
    fn the_track_guard_refuses_beyond_the_limit() {
        let mut m = MusicManager::default();
        let specs: Vec<TrackSpec> = (0..MAX_TRACKS)
            .map(|i| spec(&format!("T{i}"), "c"))
            .collect();
        assert_eq!(m.add_tracks(&specs, true).len(), MAX_TRACKS);
        let fx = m.add_tracks(&[spec("Extra", "c")], true);
        assert_eq!(fx, vec![MusicEffect::TrackRefused { id: "Extra".into() }]);
        assert_eq!(m.tracks().len(), MAX_TRACKS);
        // The parked list has the same bound.
        assert!(m.add_tracks(&specs, false).is_empty());
        let fx = m.add_tracks(&[spec("Parked", "c")], false);
        assert_eq!(
            fx,
            vec![MusicEffect::TrackRefused {
                id: "Parked".into()
            }]
        );
        assert_eq!(m.waiting().len(), MAX_TRACKS);
        // The refusal is logged; nothing plays.
        let lib = library();
        let mut engine = AudioEngine::new(1);
        let log = run_effects(&mut m, &fx, &mut engine, &lib);
        assert_eq!(log.len(), 1, "{log:?}");
        assert_eq!(engine.instance_count(), 0);
    }

    // ------------------------------------------------------------ engine

    #[test]
    fn stems_start_together_muted_and_ramp_on_the_component_clock() {
        let lib = library();
        let mut engine = AudioEngine::new(1);
        let mut m = MusicManager::default();
        // The level start: tracks added, then the linked "mute all" in the
        // same frame, before the first audio update.
        let fx = m.add_tracks(&[spec("A", "StemA"), spec("B", "StemB")], true);
        assert!(run_effects(&mut m, &fx, &mut engine, &lib).is_empty());
        let fx = m.edit_all_multipliers("Mute", 0.0, 0.0);
        run_effects(&mut m, &fx, &mut engine, &lib);
        let (a, b) = (m.tracks()[0].instance(), m.tracks()[1].instance());
        assert!(a.is_some() && b.is_some() && a != b);

        // dt = 0.125 (exact in binary). Muted from the first audible
        // update; both keep their voices (in step).
        let f = engine.update(&lib, UeVec3::ZERO, 0.125);
        assert_eq!(gain_of(&f, a), Some(0.0));
        assert_eq!(gain_of(&f, b), Some(0.0));

        // Unmute B to 0.8 over 2 s at playback time 0.125: half-way after
        // one more second.
        let fx = m.edit_multiplier_track("B", "Mute", 0.8, 2.0);
        run_effects(&mut m, &fx, &mut engine, &lib);
        let mut f = engine.update(&lib, UeVec3::ZERO, 0.125);
        for _ in 0..7 {
            f = engine.update(&lib, UeVec3::ZERO, 0.125);
        }
        let g = gain_of(&f, b).unwrap();
        assert!((g - 0.4).abs() < 1e-6, "{g}");
        assert_eq!(gain_of(&f, a), Some(0.0));
        for _ in 0..9 {
            f = engine.update(&lib, UeVec3::ZERO, 0.125);
        }
        assert_eq!(gain_of(&f, b), Some(0.8));
        // Both stems advanced together (one voice each, same position).
        let pos = |id: Option<InstanceId>| {
            f.voices
                .iter()
                .find(|v| Some(v.instance) == id)
                .map(|v| v.position)
        };
        assert_eq!(pos(a), pos(b));
    }

    #[test]
    fn an_edit_during_a_ramp_starts_from_the_last_completed_volume() {
        // The component's AdjustVolume quirk (CONFIRMED native, ported in
        // asamu-assets): a new ramp starts from the last *completed*
        // target, not from the level reached so far.
        let lib = library();
        let mut engine = AudioEngine::new(1);
        let mut m = MusicManager::default();
        let fx = m.add_tracks(&[spec("A", "StemA")], true);
        run_effects(&mut m, &fx, &mut engine, &lib);
        let fx = m.edit_multiplier_track("A", "Mute", 0.0, 0.0);
        run_effects(&mut m, &fx, &mut engine, &lib);
        engine.update(&lib, UeVec3::ZERO, 0.125);
        let fx = m.edit_multiplier_track("A", "Mute", 1.0, 4.0);
        run_effects(&mut m, &fx, &mut engine, &lib);
        let mut f = engine.update(&lib, UeVec3::ZERO, 0.125);
        for _ in 0..15 {
            f = engine.update(&lib, UeVec3::ZERO, 0.125);
        }
        let id = m.tracks()[0].instance();
        let g = gain_of(&f, id).unwrap();
        assert!((g - 0.5).abs() < 1e-6, "{g}");
        // Fade back to 0 over 2 s: the ramp runs from 0 (the last completed
        // target) to 0, so the stem drops to silence at once.
        let fx = m.edit_multiplier_track("A", "Mute", 0.0, 2.0);
        run_effects(&mut m, &fx, &mut engine, &lib);
        let f = engine.update(&lib, UeVec3::ZERO, 0.125);
        assert_eq!(gain_of(&f, id), Some(0.0));
    }

    #[test]
    fn falling_rocks_fade_matches_hand_computed_gains() {
        // The IceCave script's shape (census): two stems (drums with a cue
        // VolumeMultiplier of 0.75), muted at the start; 15 s later the
        // drums go to 1.0 and the instruments to 0.75, both over 13 s; later
        // both fade to 0 over 1.5 s. dt = 0.125 keeps every time exact.
        let lib = library_of(&[("Drums", 0.75), ("Inst", 1.0)]);
        let mut engine = AudioEngine::new(1);
        let mut m = MusicManager::default();
        apply_all(
            &mut m,
            &[
                MusicCommand::AddTracks(vec![spec("Drums", "Drums"), spec("Inst", "Inst")]),
                MusicCommand::edit(None, "Mute", 0.0, 0.0),
            ],
            &mut engine,
            &lib,
        );
        let (drums, inst) = (m.tracks()[0].instance(), m.tracks()[1].instance());
        let near = |g: Option<f32>, want: f32| g.is_some_and(|g| (g - want).abs() < 1e-6);

        // Playback time 15.0: still silent.
        let f = run_frames(&mut engine, &lib, 120, 0.125);
        assert_eq!(gain_of(&f, drums), Some(0.0));
        assert_eq!(gain_of(&f, inst), Some(0.0));
        apply_all(
            &mut m,
            &[
                MusicCommand::edit(Some("Drums"), "Mute", 1.0, 13.0),
                MusicCommand::edit(Some("Inst"), "Mute", 0.75, 13.0),
            ],
            &mut engine,
            &lib,
        );
        // 21.5 s: half-way (6.5 / 13). Drums 0.5 × cue 0.75; instruments
        // 0 + 0.5 × 0.75.
        let f = run_frames(&mut engine, &lib, 52, 0.125);
        assert!(near(gain_of(&f, drums), 0.375), "{:?}", gain_of(&f, drums));
        assert!(near(gain_of(&f, inst), 0.375), "{:?}", gain_of(&f, inst));
        // 28.125 s: the ramps (15 → 28) are complete.
        let f = run_frames(&mut engine, &lib, 53, 0.125);
        assert!(near(gain_of(&f, drums), 0.75));
        assert!(near(gain_of(&f, inst), 0.75));
        // Fade out over 1.5 s: after 0.75 s both are half-way from their
        // completed targets (drums 1.0 → 0.5 × 0.75; instruments 0.75 → 0.375).
        apply_all(
            &mut m,
            &[
                MusicCommand::edit(Some("Inst"), "Mute", 0.0, 1.5),
                MusicCommand::edit(Some("Drums"), "Mute", 0.0, 1.5),
            ],
            &mut engine,
            &lib,
        );
        let f = run_frames(&mut engine, &lib, 6, 0.125);
        assert!(near(gain_of(&f, drums), 0.375));
        assert!(near(gain_of(&f, inst), 0.375));
        // Past the end: silent, voices kept (the stems stay in step).
        let f = run_frames(&mut engine, &lib, 7, 0.125);
        assert_eq!(gain_of(&f, drums), Some(0.0));
        assert_eq!(gain_of(&f, inst), Some(0.0));
    }

    #[test]
    fn non_finite_and_extreme_values_stay_bounded() {
        let lib = library();
        let mut engine = AudioEngine::new(1);
        let mut m = MusicManager::default();
        apply_all(
            &mut m,
            &[MusicCommand::AddTracks(vec![
                spec("A", "StemA"),
                spec("B", "StemB"),
            ])],
            &mut engine,
            &lib,
        );
        let values = [f32::NAN, f32::INFINITY, f32::NEG_INFINITY, -1.0, 1e30, 0.5];
        let times = [f32::NAN, -1.0, f32::INFINITY, 0.0, 1e-30, 2.0];
        for (i, v) in values.iter().enumerate() {
            for (j, t) in times.iter().enumerate() {
                let id = if (i + j) % 2 == 0 { "A" } else { "B" };
                let log = apply_all(
                    &mut m,
                    &[
                        MusicCommand::edit(Some(id), &format!("m{j}"), *v, *t),
                        MusicCommand::edit(None, "Mute", *v, *t),
                    ],
                    &mut engine,
                    &lib,
                );
                assert!(log.is_empty(), "{log:?}");
                let f = run_frames(&mut engine, &lib, 2, 0.125);
                for voice in &f.voices {
                    assert!(
                        voice.gain.is_finite() && (0.0..=1.0).contains(&voice.gain),
                        "{v} {t}: {}",
                        voice.gain
                    );
                }
            }
        }
        // The stems are still playing.
        assert_eq!(engine.instance_count(), 2);
    }

    #[test]
    fn the_same_script_replays_identically() {
        fn replay() -> (Vec<MusicEffect>, Vec<(InstanceId, u32, UeVec3)>) {
            let lib = library();
            let mut engine = AudioEngine::new(7);
            engine.set_player_location(UeVec3::new(10.0, 20.0, 30.0));
            let mut m = MusicManager::default();
            let script = [
                MusicCommand::AddTracks(vec![spec("A", "StemA"), spec("B", "StemB")]),
                MusicCommand::edit(None, "Mute", 0.0, 0.0),
                MusicCommand::edit(Some("B"), "Mute", 0.8, 1.5),
                MusicCommand::edit(Some("A"), "Duck", 0.3, 0.25),
                MusicCommand::edit(Some("A"), "Mute", 1.0, 3.0),
                MusicCommand::edit(Some("Missing"), "Mute", 1.0, 0.0),
            ];
            let mut effects = Vec::new();
            let mut voices = Vec::new();
            for (k, c) in script.iter().enumerate() {
                let fx = m.apply(c, true);
                run_effects(&mut m, &fx, &mut engine, &lib);
                effects.extend(fx);
                engine.set_player_location(UeVec3::new(k as f32, 0.0, 0.0));
                let f = run_frames(&mut engine, &lib, 5, 1.0 / 60.0);
                voices.extend(
                    f.voices
                        .iter()
                        .map(|v| (v.instance, v.gain.to_bits(), v.location)),
                );
            }
            (effects, voices)
        }
        let first = replay();
        assert!(!first.1.is_empty());
        assert_eq!(first, replay());
    }

    #[test]
    fn tracks_follow_the_pawn_and_unknown_cues_stay_silent() {
        let lib = library();
        let mut engine = AudioEngine::new(1);
        let pawn = UeVec3::new(100.0, -50.0, 30.0);
        engine.set_player_location(pawn);
        let mut m = MusicManager::default();
        let fx = m.add_tracks(
            &[
                spec("A", "StemA"),
                spec("Missing", "Nowhere.Cue"),
                TrackSpec::new(Some("NoCue"), None, 0),
            ],
            true,
        );
        let log = run_effects(&mut m, &fx, &mut engine, &lib);
        assert_eq!(log.len(), 1, "{log:?}");
        assert!(m.tracks()[0].instance().is_some());
        assert_eq!(m.tracks()[1].instance(), None);
        assert_eq!(m.tracks()[2].instance(), None);
        assert_eq!(engine.instance_count(), 1);
        // Multiplier edits on silent tracks are harmless.
        let fx = m.edit_all_multipliers("Mute", 0.5, 0.0);
        run_effects(&mut m, &fx, &mut engine, &lib);
        let moved = UeVec3::new(-400.0, 20.0, 0.0);
        engine.set_player_location(moved);
        let f = engine.update(&lib, UeVec3::ZERO, 0.125);
        let v = f
            .voices
            .iter()
            .find(|v| Some(v.instance) == m.tracks()[0].instance())
            .unwrap();
        assert_eq!(v.location, moved);
        assert_eq!(v.gain, 0.5);
        // Reset stops the component.
        let fx = m.apply(&MusicCommand::Reset, true);
        run_effects(&mut m, &fx, &mut engine, &lib);
        assert!(engine.playing_cues().is_empty());
    }

    // ------------------------------------------------------------ bevy

    fn app_with(lib: Option<AudioLibrary>) -> App {
        let mut app = App::new();
        app.add_plugins(MusicPlugin);
        if let Some(lib) = lib {
            app.world_mut().resource_mut::<AudioRuntime>().library = Some(Arc::new(lib));
        }
        app
    }

    /// Marks the converted audio as loading (a task nobody polls here).
    fn start_pending_load(app: &mut App) {
        let task = bevy::tasks::AsyncComputeTaskPool::get_or_init(bevy::tasks::TaskPool::new)
            .spawn(async { Err::<AudioLibrary, String>("never polled".to_owned()) });
        app.world_mut().resource_mut::<AudioRuntime>().load = Some(task);
    }

    /// The documents arrive (what `poll_library_load` does).
    fn finish_load(app: &mut App, lib: AudioLibrary) {
        let mut rt = app.world_mut().resource_mut::<AudioRuntime>();
        rt.load = None;
        rt.library = Some(Arc::new(lib));
    }

    fn write(app: &mut App, command: MusicCommand) {
        app.world_mut().write_message(MusicCommandMessage(command));
    }

    #[test]
    fn commands_wait_for_the_converted_audio() {
        let mut app = app_with(None);
        start_pending_load(&mut app);
        app.world_mut()
            .write_message(MusicCommandMessage(MusicCommand::AddTracks(vec![spec(
                "A", "StemA",
            )])));
        app.world_mut()
            .write_message(MusicCommandMessage(MusicCommand::edit(
                None, "Mute", 0.0, 0.0,
            )));
        app.update();
        assert_eq!(app.world().resource::<AdaptiveMusic>().queued().len(), 2);
        assert_eq!(
            app.world()
                .resource::<AudioRuntime>()
                .engine
                .instance_count(),
            0
        );
        // The documents arrive: the queue applies in order.
        finish_load(&mut app, library());
        app.update();
        let music = app.world().resource::<AdaptiveMusic>();
        assert!(music.queued().is_empty());
        let track = &music.manager().tracks()[0];
        assert!(track.instance().is_some());
        assert_eq!(track.volume(), 0.0);
        assert_eq!(
            app.world()
                .resource::<AudioRuntime>()
                .engine
                .instance_count(),
            1
        );
    }

    #[test]
    fn stop_all_forgets_the_tracks() {
        let mut app = app_with(Some(library()));
        app.world_mut()
            .write_message(MusicCommandMessage(MusicCommand::AddTracks(vec![
                spec("A", "StemA"),
                spec("B", "StemB"),
            ])));
        app.update();
        assert_eq!(
            app.world()
                .resource::<AdaptiveMusic>()
                .manager()
                .tracks()
                .len(),
            2
        );
        app.world_mut()
            .write_message(AudioCommandMessage(AudioCommand::StopAll));
        app.update();
        assert!(
            app.world()
                .resource::<AdaptiveMusic>()
                .manager()
                .tracks()
                .is_empty()
        );
        assert!(
            app.world()
                .resource::<AudioRuntime>()
                .engine
                .playing_cues()
                .is_empty()
        );
        // The next level's first Kismet tick and the level change arrive in
        // the same frame: the change applies first, the new tracks stay.
        app.world_mut()
            .write_message(MusicCommandMessage(MusicCommand::AddTracks(vec![spec(
                "C", "StemB",
            )])));
        app.world_mut()
            .write_message(AudioCommandMessage(AudioCommand::StopAll));
        app.update();
        let music = app.world().resource::<AdaptiveMusic>();
        assert_eq!(music.manager().tracks().len(), 1);
        assert_eq!(music.manager().tracks()[0].id(), "C");
        assert_eq!(
            app.world().resource::<AudioRuntime>().engine.playing_cues(),
            vec!["StemB"]
        );
    }

    #[test]
    fn commands_are_dropped_when_no_audio_can_arrive() {
        // No documents and no load: nothing could ever play these.
        let mut app = app_with(None);
        write(&mut app, MusicCommand::AddTracks(vec![spec("A", "StemA")]));
        write(&mut app, MusicCommand::edit(None, "Mute", 0.0, 0.0));
        app.update();
        let music = app.world().resource::<AdaptiveMusic>();
        assert!(music.queued().is_empty());
        assert!(music.manager().tracks().is_empty());
        // While a load runs they wait; a failed load drops them.
        start_pending_load(&mut app);
        write(&mut app, MusicCommand::AddTracks(vec![spec("A", "StemA")]));
        app.update();
        assert_eq!(app.world().resource::<AdaptiveMusic>().queued().len(), 1);
        app.world_mut().resource_mut::<AudioRuntime>().load = None;
        app.update();
        assert!(app.world().resource::<AdaptiveMusic>().queued().is_empty());
    }

    #[test]
    fn the_wait_queue_is_bounded_and_keeps_track_additions() {
        let mut app = app_with(None);
        start_pending_load(&mut app);
        write(&mut app, MusicCommand::AddTracks(vec![spec("A", "StemA")]));
        for i in 0..MAX_QUEUED_COMMANDS {
            let v = if i + 1 == MAX_QUEUED_COMMANDS {
                0.25
            } else {
                0.5
            };
            write(&mut app, MusicCommand::edit(Some("A"), "Mute", v, 0.0));
        }
        write(&mut app, MusicCommand::AddTracks(vec![spec("B", "StemB")]));
        app.update();
        let queued = app.world().resource::<AdaptiveMusic>().queued();
        assert_eq!(queued.len(), MAX_QUEUED_COMMANDS);
        assert!(matches!(queued.first(), Some(MusicCommand::AddTracks(_))));
        assert!(matches!(queued.last(), Some(MusicCommand::AddTracks(_))));
        finish_load(&mut app, library());
        app.update();
        let m = app.world().resource::<AdaptiveMusic>().manager();
        let ids: Vec<&str> = m.tracks().iter().map(MusicTrack::id).collect();
        assert_eq!(ids, ["A", "B"]);
        // The latest edit survived; B never got a multiplier.
        assert_eq!(m.tracks()[0].volume(), 0.25);
        assert_eq!(m.tracks()[0].multipliers().len(), 1);
        assert_eq!(m.tracks()[1].volume(), 1.0);
    }

    #[test]
    fn with_the_audio_loaded_every_command_of_a_frame_applies() {
        let mut app = app_with(Some(library()));
        write(&mut app, MusicCommand::AddTracks(vec![spec("A", "StemA")]));
        let n = MAX_QUEUED_COMMANDS + 10;
        for i in 0..n {
            write(
                &mut app,
                MusicCommand::edit(Some("A"), &format!("m{i}"), 1.0, 0.0),
            );
        }
        app.update();
        let music = app.world().resource::<AdaptiveMusic>();
        assert!(music.queued().is_empty());
        assert_eq!(music.manager().tracks()[0].multipliers().len(), n);
    }

    #[test]
    fn a_queued_reset_drops_what_came_before() {
        let mut app = app_with(None);
        start_pending_load(&mut app);
        write(&mut app, MusicCommand::AddTracks(vec![spec("A", "StemA")]));
        write(&mut app, MusicCommand::edit(None, "Mute", 0.0, 0.0));
        write(&mut app, MusicCommand::Reset);
        write(&mut app, MusicCommand::AddTracks(vec![spec("B", "StemB")]));
        app.update();
        assert_eq!(
            app.world().resource::<AdaptiveMusic>().queued(),
            [
                MusicCommand::Reset,
                MusicCommand::AddTracks(vec![spec("B", "StemB")])
            ]
        );
    }

    // ------------------------------------------------------------ real data

    /// The adaptive tracks of the shipped levels (track ID, cue): the
    /// `SeqAct_AddAdaptiveTracks` census (names only).
    const SHIPPED_TRACKS: &[(&str, &str)] = &[
        ("MusicPart2", "Sanctuary_Music.Sanctuary_Music_Part2_Cue"),
        ("TimeTrialStart", "Music.ASAMU_TimeTrial_Start_Cue"),
        ("TimeTrial1", "Music.ASAMU_TimeTrial_1_Cue"),
        ("TimeTrial2", "Music.ASAMU_TimeTrial_2_Cue"),
        ("TimeTrial3", "Music.ASAMU_TimeTrial_3_Cue"),
        ("TimeTrial4", "Music.ASAMU_TimeTrial_4_Cue"),
        ("TimeTrial5", "Music.ASAMU_TimeTrial_5_Cue"),
        (
            "HeartBeat",
            "Chasms_Music.Worm.Chasms_Worm_Music_HeartBeat_Cue",
        ),
        (
            "Sleeping1",
            "Chasms_Music.Worm.Chasms_Worm_Music_Sleeping_Cue",
        ),
        (
            "Sleeping2",
            "Chasms_Music.Worm.Chasms_Worm_Music_Sleeping_Cue_2",
        ),
        (
            "Searching",
            "Chasms_Music.Worm.Chasms_Worm_Music_Searching_Cue",
        ),
        (
            "Sleeping3",
            "Chasms_Music.Worm.Chasms_Worm_Music_Sleeping_Cue_3",
        ),
        (
            "FallingRocks_Music",
            "IceCave_Music.FallingRocks.FallingRocks_Music_Drums_Cue",
        ),
        (
            "FallingRocks_Inst",
            "IceCave_Music.FallingRocks.FallingRocks_Music_Inst_Cue",
        ),
    ];

    #[test]
    fn real_converted_adaptive_tracks_play() {
        let Some(dir) = std::env::var_os("ASAMU_CONVERTED_DIR") else {
            eprintln!("skipping: ASAMU_CONVERTED_DIR not set");
            return;
        };
        let audio = Path::new(&dir).join(AUDIO_DIR);
        if !audio.join("cues.json").is_file() {
            eprintln!("skipping: no {}/cues.json", audio.display());
            return;
        }
        let lib = AudioLibrary::load(&audio).unwrap();
        let mut engine = AudioEngine::new(1);
        let mut m = MusicManager::default();
        let specs: Vec<TrackSpec> = SHIPPED_TRACKS
            .iter()
            .map(|(id, cue)| spec(id, cue))
            .collect();
        let fx = m.add_tracks(&specs, true);
        let log = run_effects(&mut m, &fx, &mut engine, &lib);
        assert!(log.is_empty(), "{log:?}");
        for t in m.tracks() {
            let cue = lib.cue(t.cue().unwrap_or_default()).unwrap();
            // Music, and never spatialised (no attenuation node).
            assert_eq!(
                cue.sound_class.as_deref(),
                Some("ASAMU_Music"),
                "{}",
                t.id()
            );
            assert!(
                !cue.nodes
                    .iter()
                    .any(|n| matches!(n.kind, NodeKind::Attenuation(_))),
                "{}",
                t.id()
            );
        }
        // Mute all, as the level start does, then bring one stem in.
        let fx = m.edit_all_multipliers("Mute", 0.0, 0.0);
        run_effects(&mut m, &fx, &mut engine, &lib);
        let mut f = engine.update(&lib, UeVec3::ZERO, 1.0 / 60.0);
        for t in m.tracks() {
            assert_eq!(gain_of(&f, t.instance()), Some(0.0), "{}", t.id());
        }
        let fx = m.edit_multiplier_track("HeartBeat", "Mute", 0.8, 0.0);
        run_effects(&mut m, &fx, &mut engine, &lib);
        for _ in 0..120 {
            f = engine.update(&lib, UeVec3::ZERO, 1.0 / 60.0);
        }
        let heart = m.track("HeartBeat").and_then(MusicTrack::instance);
        assert!(gain_of(&f, heart).is_some_and(|g| g > 0.0));
        let looping = m
            .tracks()
            .iter()
            .filter(|t| t.instance().is_some_and(|i| engine.is_playing(i)))
            .count();
        eprintln!(
            "{} adaptive tracks started, {looping} still playing after 2 s",
            m.tracks().len()
        );
        assert_eq!(looping, SHIPPED_TRACKS.len());
    }

    /// The maps with adaptive music and their track IDs (census).
    const SHIPPED_LEVELS: &[(&str, &[&str])] = &[
        (
            "AG-ParadiseCave",
            &[
                "MusicPart2",
                "TimeTrialStart",
                "TimeTrial1",
                "TimeTrial2",
                "TimeTrial3",
                "TimeTrial4",
                "TimeTrial5",
            ],
        ),
        (
            "AG-Darkcave",
            &[
                "HeartBeat",
                "Sleeping1",
                "Sleeping2",
                "Searching",
                "Sleeping3",
            ],
        ),
        ("AG-IceCave", &["FallingRocks_Music", "FallingRocks_Inst"]),
    ];

    /// `ASAMU_CONVERTED_DIR` when it holds converted Kismet graphs.
    fn converted_kismet_dir() -> Option<std::path::PathBuf> {
        let Some(dir) = std::env::var_os("ASAMU_CONVERTED_DIR") else {
            eprintln!("skipping: ASAMU_CONVERTED_DIR not set");
            return None;
        };
        let dir = std::path::PathBuf::from(dir);
        if !dir.join("kismet").is_dir() {
            eprintln!("skipping: no {}/kismet", dir.display());
            return None;
        }
        Some(dir)
    }

    /// The level's first Kismet update through the real runtime, its music
    /// commands applied to a fresh manager (the pawn exists, as in the
    /// original's second initialisation).
    fn first_kismet_update(dir: &Path, map: &str) -> (MusicManager, Vec<MusicCommand>) {
        let scripts = load_level_scripts(dir, map, &[]).unwrap();
        let mut rt = Runtime::new(Arc::new(scripts.graph), Arc::new(scripts.matinee));
        rt.tick(1.0 / 60.0, &mut NullHost);
        let commands: Vec<MusicCommand> = rt
            .take_outputs()
            .iter()
            .filter_map(MusicCommand::from_output)
            .collect();
        let mut m = MusicManager::default();
        for c in &commands {
            m.apply(c, true);
        }
        (m, commands)
    }

    #[test]
    fn real_converted_kismet_adds_the_shipped_tracks() {
        let Some(dir) = converted_kismet_dir() else {
            return;
        };
        for (map, ids) in SHIPPED_LEVELS {
            let (m, commands) = first_kismet_update(&dir, map);
            let mut got: Vec<(&str, Option<&str>)> =
                m.tracks().iter().map(|t| (t.id(), t.cue())).collect();
            eprintln!("{map}: {} music commands, tracks {got:?}", commands.len());
            got.sort_unstable();
            let mut want: Vec<(&str, Option<&str>)> = ids
                .iter()
                .map(|id| {
                    let cue = SHIPPED_TRACKS
                        .iter()
                        .find(|(t, _)| t == id)
                        .map(|(_, c)| *c);
                    (*id, cue)
                })
                .collect();
            want.sort_unstable();
            assert_eq!(got, want, "{map}");
            assert!(m.waiting().is_empty(), "{map}");
        }
    }

    /// The original forces `SeqAct_AddAdaptiveTracks`' "Out", which every
    /// shipped instance links to a mute-all, so the stems are silent from
    /// the first update (AUDIO.md, "Adaptive music", cross-check 1).
    #[test]
    fn real_converted_kismet_starts_the_stems_muted() {
        let Some(dir) = converted_kismet_dir() else {
            return;
        };
        for (map, _) in SHIPPED_LEVELS {
            let (m, _) = first_kismet_update(&dir, map);
            assert!(!m.tracks().is_empty(), "{map}");
            for t in m.tracks() {
                assert_eq!(t.volume(), 0.0, "{map}: {}", t.id());
                assert_eq!(t.multipliers().len(), 1, "{map}: {}", t.id());
            }
        }
    }
}
