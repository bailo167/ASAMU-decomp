//! Runtime audio model for the user-local output of `asamu-import audio`
//! (`<converted>/audio/`): the converted documents (`manifest.json`,
//! `cues.json`, `subtitles.json`, `sound_classes.json`, `ambient.json`), a
//! **SoundCue evaluator** that reproduces the original engine's node-graph
//! semantics, sound classes and modes, the ASAMU narrator queue, the
//! subtitle timing, ambient sound actors and an [`AudioCommand`] API for the
//! Kismet runtime.
//!
//! Everything here is render- and device-free: [`AudioEngine::update`]
//! turns the playing cue instances into a list of [`Voice`]s (wave, final
//! gain, final pitch, spatialisation, position) that an audio back end (the
//! Bevy app) plays. No audio device is needed, so every rule is unit
//! tested.
//!
//! # Evidence
//!
//! The semantics come from the original Mac executable (unstripped, read
//! locally with Ghidra; nothing decompiled is reproduced here), the class
//! models and class default objects of `Engine.u` / `Startup.upk`, the
//! shipped config, and local reading of the ASAMU script (described, never
//! quoted). `docs/reverse-engineering/AUDIO.md` ("Audio runtime") lists each
//! rule with its confidence; the short version:
//!
//! - Node semantics (Random with weights and no-repeat, Mixer, Modulator,
//!   Looping, Delay, Concatenator, Attenuation curves and LPF, Distance
//!   cross-fade, WaveParam, Ambient / AmbientNonLoop / toggle), the
//!   component volume chain, fades, the voice limit, the pitch clamp and
//!   the subtitle queue: CONFIRMED (native functions read; constants read
//!   from the executable).
//! - The random generator is the engine's LCG (CONFIRMED), but the original
//!   shares one global seed with every other engine system, so exact
//!   random sequences cannot match the original; seeds here are explicit
//!   and deterministic.
//! - The narrator queue and the gameplay cue choices: STRONG (script read
//!   locally, cue references from class defaults).
//!
//! # Units
//!
//! Positions are UE3 world units (UU), times are seconds.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use asamu_core::glam::Vec3;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::{AssetError, AssetResult};
use crate::files::{parse_json, read_bounded, safe_relative_path};

// ---------------------------------------------------------------------------
// Constants (each with its source)
// ---------------------------------------------------------------------------

/// Sub-directory of the converted directory written by `asamu-import audio`.
pub const AUDIO_DIR: &str = "audio";

/// Largest audio JSON document accepted (the full `cues.json` of the
/// shipped game is about 1.4 MB).
pub const MAX_AUDIO_JSON_BYTES: u64 = 64 * 1024 * 1024;

/// Largest single audio file accepted (the biggest shipped stream is a few
/// megabytes).
pub const MAX_WAVE_FILE_BYTES: u64 = 64 * 1024 * 1024;

/// Duration the cooker stores for a cue that loops forever, and the value
/// `SoundNodeLooping::GetDuration` returns for an indefinite loop.
/// CONFIRMED (constant read from the executable; equals the cooked
/// `Duration` of the 154 looping cues, AUDIO.md).
pub const INDEFINITE_DURATION: f32 = 10_000.0;

/// Lowest pitch a source plays at (the OpenAL source update clamps).
/// CONFIRMED (constant read from the executable).
pub const MIN_PITCH: f32 = 0.4;

/// Highest pitch a source plays at. CONFIRMED (as [`MIN_PITCH`]).
pub const MAX_PITCH: f32 = 2.0;

/// A wave instance plays only when its priority (volume of the node chain,
/// plus 1 for `bAlwaysPlay` classes) exceeds this. CONFIRMED (constant read
/// from the executable; used by the wave parse and the voice selection).
pub const MIN_PLAY_PRIORITY: f32 = 1.0e-4;

/// Voices played at once (the loudest wave instances win).
/// CONFIRMED (config `BaseEngine.ini` `[ALAudio.ALAudioDevice] MaxChannels`).
pub const MAX_CHANNELS: usize = 32;

/// Gain applied by the source to stereo waves whose sound class has a
/// non-zero `StereoBleed` (every class of the game: the class default is
/// 0.25). CONFIRMED (constant read from the OpenAL source update).
pub const STEREO_BLEED_GAIN: f32 = 1.25;

/// Subtitle priority of the ASAMU narrator component and of Kismet
/// `SeqAct_PlaySound` components. STRONG (narrator: ASAMU script; Kismet:
/// the engine's `PlayerController` script); every other component keeps
/// the class default 0, and the subtitle queue ignores priority 0
/// (CONFIRMED, native).
pub const SUBTITLE_PRIORITY_SCRIPTED: f32 = 10_000.0;

/// Fade time the narrator passes when "remove all other cues" removes
/// queued lines (the script's literal). STRONG.
pub const NARRATOR_REMOVE_FADE: f32 = 0.2;

/// Distance used when a cue has no attenuation (`WORLD_MAX`).
/// CONFIRMED (constant read from `USoundCue::CalculateMaxAudibleDistance`).
pub const WORLD_MAX_AUDIBLE_DISTANCE: f32 = 524_288.0;

/// Deepest node chain evaluated (the deepest shipped cue has 7 edges,
/// AUDIO.md); a guard against crafted graphs only.
const MAX_PARSE_DEPTH: usize = 64;

/// Node visits per instance and update. The engine walks a node once per
/// path that reaches it, so a crafted graph that shares nodes (a "diamond"
/// chain) costs 2^depth visits; the largest shipped cue has fewer than 40
/// nodes and no shared node (AUDIO.md). A guard against crafted graphs only.
const MAX_PARSE_VISITS: usize = 4096;

/// Class defaults of the sound objects (`Engine.Default__*`, CONFIRMED
/// cdo, AUDIO.md). The converter already merges stored values over these;
/// they are only fallbacks for incomplete documents.
mod cdo {
    pub const CUE_VOLUME_MULTIPLIER: f32 = 0.75;
    pub const CUE_PITCH_MULTIPLIER: f32 = 1.0;
    pub const CUE_MAX_CONCURRENT_PLAY_COUNT: i32 = 16;
    pub const WAVE_VOLUME: f32 = 0.75;
    pub const WAVE_PITCH: f32 = 1.0;
    pub const ATT_DB_AT_MAX: f32 = -60.0;
    pub const ATT_RADIUS_MIN: f32 = 400.0;
    pub const ATT_RADIUS_MAX: f32 = 4000.0;
    pub const ATT_LPF_RADIUS_MIN: f32 = 3000.0;
    pub const ATT_LPF_RADIUS_MAX: f32 = 6000.0;
    pub const MOD_PITCH_MIN: f32 = 0.95;
    pub const MOD_PITCH_MAX: f32 = 1.05;
    pub const MOD_VOLUME_MIN: f32 = 0.95;
    pub const MOD_VOLUME_MAX: f32 = 1.05;
    pub const LOOP_COUNT: f32 = 1_000_000.0;
    pub const AMB_RADIUS_MIN: f32 = 2000.0;
    pub const AMB_RADIUS_MAX: f32 = 5000.0;
    pub const AMB_LPF_RADIUS_MIN: f32 = 3500.0;
    pub const AMB_LPF_RADIUS_MAX: f32 = 7000.0;
    pub const AMB_PITCH: f32 = 1.0;
    pub const AMB_VOLUME: f32 = 0.7;
    pub const CLASS_STEREO_BLEED: f32 = 0.25;
    pub const CLASS_LFE_BLEED: f32 = 0.5;
    /// `Engine.Default__DistanceFloatParameterBase`: `MaxInput`, `MaxOutput`.
    pub const PARAM_MAX: f32 = 1.0;
    /// `Engine.Default__AmbientSoundSimpleToggleable`.
    pub const TOGGLE_FADE_IN_DURATION: f32 = 1.0;
    pub const TOGGLE_FADE_IN_VOLUME: f32 = 1.0;
    pub const TOGGLE_FADE_OUT_DURATION: f32 = 1.0;
}

// ---------------------------------------------------------------------------
// Random numbers
// ---------------------------------------------------------------------------

/// The engine's random generator (`appSRand`): a 32-bit LCG whose state
/// becomes the mantissa of a float in `[1, 2)`, minus one.
///
/// CONFIRMED (the multiplier and increment are read from every sound node
/// that draws). The original's seed is global and shared with unrelated
/// engine systems, so its sequences are not reproducible; this one is
/// seeded explicitly (determinism for tests and replays).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct UeRand {
    seed: u32,
}

impl UeRand {
    /// LCG multiplier.
    pub const MULTIPLIER: u32 = 0x0BB3_8435;
    /// LCG increment.
    pub const INCREMENT: u32 = 0x3619_636B;

    /// A generator with the given state.
    #[must_use]
    pub fn new(seed: u32) -> Self {
        Self { seed }
    }

    /// Current state.
    #[must_use]
    pub fn seed(&self) -> u32 {
        self.seed
    }

    /// Next value in `[0, 1)`.
    pub fn frand(&mut self) -> f32 {
        self.seed = self
            .seed
            .wrapping_mul(Self::MULTIPLIER)
            .wrapping_add(Self::INCREMENT);
        f32::from_bits((self.seed & 0x007F_FFFF) | 0x3F80_0000) - 1.0
    }

    /// The nodes' range draw for a `(min, max)` pair: `f · (min − max) +
    /// max` (uniform over the range; the formula's orientation is the
    /// engine's, so a pair stored as `min > max` behaves like the original).
    pub fn pick(&mut self, min: f32, max: f32) -> f32 {
        self.frand() * (min - max) + max
    }
}

// ---------------------------------------------------------------------------
// Distance models
// ---------------------------------------------------------------------------

/// `SoundDistanceModel` (enum order CONFIRMED from `Engine.u`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum DistanceModel {
    /// `ATTENUATION_Linear` (enum entry 0, the value when nothing is stored).
    #[default]
    Linear,
    /// `ATTENUATION_Logarithmic`.
    Logarithmic,
    /// `ATTENUATION_Inverse`.
    Inverse,
    /// `ATTENUATION_LogReverse`.
    LogReverse,
    /// `ATTENUATION_NaturalSound`.
    NaturalSound,
    /// Any other stored value: the engine applies no curve (full volume
    /// inside `RadiusMax`).
    Other,
}

impl DistanceModel {
    /// From an enumerator name or index.
    #[must_use]
    pub fn from_value(v: Option<&Value>) -> Self {
        match v {
            Some(Value::String(s)) => match s.to_ascii_lowercase().as_str() {
                "attenuation_linear" => Self::Linear,
                "attenuation_logarithmic" => Self::Logarithmic,
                "attenuation_inverse" => Self::Inverse,
                "attenuation_logreverse" => Self::LogReverse,
                "attenuation_naturalsound" => Self::NaturalSound,
                _ => Self::Other,
            },
            Some(v) => match v.as_i64() {
                Some(0) => Self::Linear,
                Some(1) => Self::Logarithmic,
                Some(2) => Self::Inverse,
                Some(3) => Self::LogReverse,
                Some(4) => Self::NaturalSound,
                _ => Self::Other,
            },
            None => Self::Linear,
        }
    }
}

/// `ESoundDistanceCalc` (enum order CONFIRMED from `Engine.u`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum DistanceType {
    /// Full 3-D distance.
    #[default]
    Normal,
    /// `SOUNDDISTANCE_InfiniteXYPlane`: only the Z difference counts.
    InfiniteXyPlane,
    /// `SOUNDDISTANCE_InfiniteXZPlane`: only the Y difference counts.
    InfiniteXzPlane,
    /// `SOUNDDISTANCE_InfiniteYZPlane`: only the X difference counts.
    InfiniteYzPlane,
}

impl DistanceType {
    fn from_value(v: Option<&Value>) -> Self {
        match v {
            Some(Value::String(s)) => match s.to_ascii_lowercase().as_str() {
                "sounddistance_infinitexyplane" => Self::InfiniteXyPlane,
                "sounddistance_infinitexzplane" => Self::InfiniteXzPlane,
                "sounddistance_infiniteyzplane" => Self::InfiniteYzPlane,
                _ => Self::Normal,
            },
            Some(v) => match v.as_i64() {
                Some(1) => Self::InfiniteXyPlane,
                Some(2) => Self::InfiniteXzPlane,
                Some(3) => Self::InfiniteYzPlane,
                _ => Self::Normal,
            },
            None => Self::Normal,
        }
    }

    /// Distance between a source and the listener under this rule.
    #[must_use]
    pub fn distance(self, source: Vec3, listener: Vec3) -> f32 {
        let d = listener - source;
        match self {
            Self::Normal => d.length(),
            Self::InfiniteXyPlane => d.z.abs(),
            Self::InfiniteXzPlane => d.y.abs(),
            Self::InfiniteYzPlane => d.x.abs(),
        }
    }
}

/// Volume factor of the attenuation curve at `distance`.
///
/// Outside the curve: `distance ≥ max` gives 0 and `distance ≤ min` gives 1.
/// Inside, with `t = (distance − min) / (max − min)`:
///
/// - linear: `1 − t`;
/// - logarithmic: `ln(distance/max) / ln(min/max)` (`−0.25 · ln(distance/max)`
///   when `min` is 0), at most 1;
/// - inverse: `0.02 · (max/distance) · (max/min)` (`max/min` read as 1 when
///   `min` is 0), at most 1;
/// - log-reverse: `1 − ln(1 / (1 − distance/max)) / −ln(min/max)` (the same
///   0.25 factor when `min` is 0), at least 0;
/// - natural sound: `10^(t · dB_at_max / 20)`.
///
/// CONFIRMED: a port of the executable's attenuation function (its
/// constants 1, −1, 0.25, 0.02, 10 and 20 read from the binary).
/// Non-finite results are returned as 0.
#[must_use]
pub fn attenuation_eval(
    model: DistanceModel,
    distance: f32,
    min: f32,
    max: f32,
    db_at_max: f32,
) -> f32 {
    // `!(d < max)` also sends NaN distances to silence.
    if distance.partial_cmp(&max) != Some(std::cmp::Ordering::Less) {
        return 0.0;
    }
    if distance <= min {
        return 1.0;
    }
    let log_scale = |min: f32| {
        if min == 0.0 {
            0.25
        } else {
            -1.0 / (min / max).ln()
        }
    };
    let v = match model {
        DistanceModel::Linear => 1.0 - (distance - min) / (max - min),
        DistanceModel::Logarithmic => (-((distance / max).ln() * log_scale(min))).min(1.0),
        DistanceModel::Inverse => {
            let ratio = if min == 0.0 { 1.0 } else { max / min };
            ((0.02 / (distance / max)) * ratio).min(1.0)
        }
        DistanceModel::LogReverse => {
            let v = 1.0 - (1.0 / (1.0 - distance / max)).ln() * log_scale(min);
            if v >= 0.0 { v } else { 0.0 }
        }
        DistanceModel::NaturalSound => {
            10.0_f32.powf(((distance - min) / (max - min)) * db_at_max / 20.0)
        }
        DistanceModel::Other => 1.0,
    };
    if v.is_finite() { v } else { 0.0 }
}

/// High-frequency gain of the distance low-pass filter: 1 inside `min`,
/// 0 from `max`, linear between. CONFIRMED (native).
#[must_use]
pub fn lpf_gain(distance: f32, min: f32, max: f32) -> f32 {
    if max <= distance {
        0.0
    } else if distance <= min {
        1.0
    } else {
        let v = 1.0 - (distance - min) / (max - min);
        if v.is_finite() { v } else { 1.0 }
    }
}

/// Gain of one `SoundNodeDistanceCrossFade` input at `distance`: a linear
/// fade in between `FadeInDistanceStart` and `…End`, a linear fade out
/// between `FadeOutDistanceStart` and `…End`, full `Volume` between the
/// two, silence elsewhere. CONFIRMED (native).
#[must_use]
pub fn cross_fade_gain(d: &DistanceDatum, distance: f32) -> f32 {
    let v = if distance >= d.fade_in_start && distance <= d.fade_in_end {
        ((distance - d.fade_in_start) / (d.fade_in_end - d.fade_in_start)) * d.volume
    } else if distance >= d.fade_out_start && distance <= d.fade_out_end {
        (1.0 - (distance - d.fade_out_start) / (d.fade_out_end - d.fade_out_start)) * d.volume
    } else if distance >= d.fade_in_end && distance <= d.fade_out_start {
        d.volume
    } else {
        0.0
    };
    if v.is_finite() { v } else { d.volume }
}

// ---------------------------------------------------------------------------
// JSON helpers
// ---------------------------------------------------------------------------

fn get<'v>(map: &'v Value, key: &str) -> Option<&'v Value> {
    map.as_object()?.get(key)
}

fn num(v: Option<&Value>) -> Option<f32> {
    let x = v?.as_f64()? as f32;
    x.is_finite().then_some(x)
}

fn num_or(map: &Value, key: &str, default: f32) -> f32 {
    num(get(map, key)).unwrap_or(default)
}

fn flag_or(map: &Value, key: &str, default: bool) -> bool {
    get(map, key).and_then(Value::as_bool).unwrap_or(default)
}

fn text(v: Option<&Value>) -> Option<String> {
    v?.as_str().map(str::to_owned)
}

fn floats(v: Option<&Value>) -> Vec<f32> {
    v.and_then(Value::as_array)
        .map(|a| a.iter().map(|x| num(Some(x)).unwrap_or(0.0)).collect())
        .unwrap_or_default()
}

fn vec3(v: Option<&Value>) -> Option<Vec3> {
    match v? {
        Value::Array(a) if a.len() == 3 => {
            Some(Vec3::new(num(a.first())?, num(a.get(1))?, num(a.get(2))?))
        }
        Value::Object(_) => {
            let v = v?;
            Some(Vec3::new(
                num(get(v, "X"))?,
                num(get(v, "Y"))?,
                num(get(v, "Z"))?,
            ))
        }
        _ => None,
    }
}

fn check_format(path: &Path, doc: &Value, expected: &str) -> AssetResult<()> {
    let found = get(doc, "format").and_then(Value::as_str).unwrap_or("");
    if found != expected {
        return Err(AssetError::Format {
            path: path.to_path_buf(),
            expected: format!("format {expected:?}"),
            found: format!("format {found:?}"),
        });
    }
    let version = get(doc, "version").and_then(Value::as_u64).unwrap_or(0);
    if version != 1 {
        return Err(AssetError::Format {
            path: path.to_path_buf(),
            expected: "version 1".to_owned(),
            found: format!("version {version}"),
        });
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Data model
// ---------------------------------------------------------------------------

/// One wave of `manifest.json`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct WaveInfo {
    /// Object path (the key).
    pub path: String,
    /// Audio file relative to the `audio/` directory (validated), `None`
    /// when the converter wrote no file.
    pub file: Option<String>,
    /// Exact length in seconds (`samples / sample_rate` when known, else
    /// the stored `Duration`).
    pub duration: f32,
    /// `NumChannels`.
    pub channels: u16,
    /// `Volume` (default 0.75).
    pub volume: f32,
    /// `Pitch` (default 1.0).
    pub pitch: f32,
}

/// One subtitle line.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SubtitleLine {
    /// Text (game text: local only).
    pub text: String,
    /// Start time in seconds from the start of the wave.
    pub time: f32,
}

/// The subtitles of one wave (`subtitles.json`).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SubtitleTrack {
    /// Lines as stored.
    pub lines: Vec<SubtitleLine>,
    /// Wave duration in seconds.
    pub duration: Option<f32>,
    /// `bManualWordWrap`.
    pub manual_word_wrap: bool,
    /// `bSingleLine`.
    pub single_line: bool,
    /// Sound classes of the cues that play the wave (speaker hint).
    pub cue_sound_classes: Vec<String>,
}

/// Node index inside a [`CueDef`].
pub type NodeId = usize;

/// A wave leaf.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct WaveRef {
    /// Wave object path.
    pub path: String,
    /// `Volume`.
    pub volume: f32,
    /// `Pitch`.
    pub pitch: f32,
}

/// `SoundNodeAttenuation` values.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Attenuation {
    /// `bAttenuate`.
    pub attenuate: bool,
    /// `bSpatialize`.
    pub spatialize: bool,
    /// `bAttenuateWithLPF`.
    pub attenuate_with_lpf: bool,
    /// `dBAttenuationAtMax`.
    pub db_at_max: f32,
    /// `OmniRadius`.
    pub omni_radius: f32,
    /// `DistanceAlgorithm`.
    pub model: DistanceModel,
    /// `DistanceType`.
    pub distance_type: DistanceType,
    /// `RadiusMin`.
    pub radius_min: f32,
    /// `RadiusMax`.
    pub radius_max: f32,
    /// `LPFRadiusMin`.
    pub lpf_radius_min: f32,
    /// `LPFRadiusMax`.
    pub lpf_radius_max: f32,
}

impl Attenuation {
    fn from_params(p: &Value) -> Self {
        Self {
            attenuate: flag_or(p, "bAttenuate", true),
            spatialize: flag_or(p, "bSpatialize", true),
            attenuate_with_lpf: flag_or(p, "bAttenuateWithLPF", false),
            db_at_max: num_or(p, "dBAttenuationAtMax", cdo::ATT_DB_AT_MAX),
            omni_radius: num_or(p, "OmniRadius", 0.0),
            model: DistanceModel::from_value(get(p, "DistanceAlgorithm")),
            distance_type: DistanceType::from_value(get(p, "DistanceType")),
            radius_min: num_or(p, "RadiusMin", cdo::ATT_RADIUS_MIN),
            radius_max: num_or(p, "RadiusMax", cdo::ATT_RADIUS_MAX),
            lpf_radius_min: num_or(p, "LPFRadiusMin", cdo::ATT_LPF_RADIUS_MIN),
            lpf_radius_max: num_or(p, "LPFRadiusMax", cdo::ATT_LPF_RADIUS_MAX),
        }
    }
}

/// `SoundNodeRandom` values.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RandomNode {
    /// `Weights` (one per input).
    pub weights: Vec<f32>,
    /// `bRandomizeWithoutReplacement`.
    pub without_replacement: bool,
    /// `PreselectAtLevelLoad` (0: off).
    pub preselect_at_level_load: i32,
}

/// `SoundNodeModulator` ranges.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct ModulatorRanges {
    /// `PitchMin`.
    pub pitch_min: f32,
    /// `PitchMax`.
    pub pitch_max: f32,
    /// `VolumeMin`.
    pub volume_min: f32,
    /// `VolumeMax`.
    pub volume_max: f32,
}

/// How a parameter distribution reads its input (`DistributionParamMode`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum ParamMode {
    /// `DPM_Normal`: clamp to the input range, map to the output range.
    #[default]
    Normal,
    /// `DPM_Abs`: as `Normal` on the absolute value.
    Abs,
    /// `DPM_Direct`: the parameter value itself.
    Direct,
}

/// A `DistributionFloatSoundParameter` (or a constant) feeding a
/// `SoundNodeModulatorContinuous`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ParamDistribution {
    /// `ParameterName` (`None` for a constant distribution).
    pub name: Option<String>,
    /// `MinInput`.
    pub min_input: f32,
    /// `MaxInput`.
    pub max_input: f32,
    /// `MinOutput`.
    pub min_output: f32,
    /// `MaxOutput`.
    pub max_output: f32,
    /// `ParamMode`.
    pub mode: ParamMode,
    /// `Constant` (used when the parameter is not set, and by constant
    /// distributions).
    pub constant: f32,
}

impl ParamDistribution {
    fn from_info(class: &str, p: &Value) -> Self {
        let mode = match get(p, "ParamMode") {
            Some(Value::String(s)) if s.eq_ignore_ascii_case("DPM_Abs") => ParamMode::Abs,
            Some(Value::String(s)) if s.eq_ignore_ascii_case("DPM_Direct") => ParamMode::Direct,
            Some(v) if v.as_i64() == Some(1) => ParamMode::Abs,
            Some(v) if v.as_i64() == Some(2) => ParamMode::Direct,
            _ => ParamMode::Normal,
        };
        let constant = num_or(p, "Constant", 0.0);
        if class.to_ascii_lowercase().contains("constant") {
            return Self {
                name: None,
                min_input: 0.0,
                max_input: 0.0,
                min_output: constant,
                max_output: constant,
                mode: ParamMode::Direct,
                constant,
            };
        }
        Self {
            name: text(get(p, "ParameterName")),
            min_input: num_or(p, "MinInput", 0.0),
            max_input: num_or(p, "MaxInput", cdo::PARAM_MAX),
            min_output: num_or(p, "MinOutput", 0.0),
            max_output: num_or(p, "MaxOutput", cdo::PARAM_MAX),
            mode,
            constant,
        }
    }

    /// The distribution's value for the component's float parameters.
    ///
    /// TENTATIVE (engine library behaviour, not traced to the end in the
    /// binary): a missing parameter reads `Constant`; `Normal` clamps the
    /// input to `[MinInput, MaxInput]` and maps it linearly onto
    /// `[MinOutput, MaxOutput]` (gradient 0 when the input range is empty).
    #[must_use]
    pub fn value(&self, params: &BTreeMap<String, f32>) -> f32 {
        let Some(name) = &self.name else {
            return self.constant;
        };
        let mut x = params.get(name).copied().unwrap_or(self.constant);
        match self.mode {
            ParamMode::Direct => return x,
            ParamMode::Abs => x = x.abs(),
            ParamMode::Normal => {}
        }
        let gradient = if self.max_input <= self.min_input {
            0.0
        } else {
            (self.max_output - self.min_output) / (self.max_input - self.min_input)
        };
        let clamped = x.max(self.min_input).min(self.max_input);
        self.min_output + (clamped - self.min_input) * gradient
    }
}

/// One `DistanceDatum` of a cross-fade node.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct DistanceDatum {
    /// `FadeInDistanceStart`.
    pub fade_in_start: f32,
    /// `FadeInDistanceEnd`.
    pub fade_in_end: f32,
    /// `FadeOutDistanceStart`.
    pub fade_out_start: f32,
    /// `FadeOutDistanceEnd`.
    pub fade_out_end: f32,
    /// `Volume`.
    pub volume: f32,
}

impl Default for DistanceDatum {
    /// The struct defaults (CONFIRMED, `Engine.u`): all distances 0,
    /// volume 1.
    fn default() -> Self {
        Self {
            fade_in_start: 0.0,
            fade_in_end: 0.0,
            fade_out_start: 0.0,
            fade_out_end: 0.0,
            volume: 1.0,
        }
    }
}

/// One `AmbientSoundSlot`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AmbientSlot {
    /// Wave object path.
    pub wave: Option<String>,
    /// `PitchScale`.
    pub pitch_scale: f32,
    /// `VolumeScale`.
    pub volume_scale: f32,
    /// `Weight`.
    pub weight: f32,
}

/// The non-looping variant's values (`SoundNodeAmbientNonLoop`).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct NonLoop {
    /// `DelayMin`.
    pub delay_min: f32,
    /// `DelayMax`.
    pub delay_max: f32,
    /// `SoundNodeAmbientNonLoopToggle`: stop the component after one sound.
    pub toggle: bool,
}

/// `SoundNodeAmbient*` values (the inline node of `AmbientSoundSimple*`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AmbientNode {
    /// `bAttenuate`.
    pub attenuate: bool,
    /// `bSpatialize`.
    pub spatialize: bool,
    /// `bAttenuateWithLPF`.
    pub attenuate_with_lpf: bool,
    /// `dBAttenuationAtMax`.
    pub db_at_max: f32,
    /// `DistanceModel`.
    pub model: DistanceModel,
    /// `RadiusMin`.
    pub radius_min: f32,
    /// `RadiusMax`.
    pub radius_max: f32,
    /// `LPFRadiusMin`.
    pub lpf_radius_min: f32,
    /// `LPFRadiusMax`.
    pub lpf_radius_max: f32,
    /// Modulation ranges.
    pub ranges: ModulatorRanges,
    /// `SoundSlots`.
    pub slots: Vec<AmbientSlot>,
    /// Non-looping variant.
    pub non_loop: Option<NonLoop>,
}

impl AmbientNode {
    fn from_params(p: &Value, kind: &str) -> Self {
        let slots = get(p, "SoundSlots")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .map(|s| AmbientSlot {
                        wave: text(get(s, "Wave")),
                        pitch_scale: num_or(s, "PitchScale", 1.0),
                        volume_scale: num_or(s, "VolumeScale", 1.0),
                        weight: num_or(s, "Weight", 1.0),
                    })
                    .collect()
            })
            .unwrap_or_default();
        let non_loop = match kind {
            "AmbientNonLoop" | "AmbientNonLoopToggle" => Some(NonLoop {
                delay_min: num_or(p, "DelayMin", 0.0),
                delay_max: num_or(p, "DelayMax", 0.0),
                toggle: kind == "AmbientNonLoopToggle",
            }),
            _ => None,
        };
        Self {
            attenuate: flag_or(p, "bAttenuate", true),
            spatialize: flag_or(p, "bSpatialize", true),
            attenuate_with_lpf: flag_or(p, "bAttenuateWithLPF", false),
            db_at_max: num_or(p, "dBAttenuationAtMax", cdo::ATT_DB_AT_MAX),
            model: DistanceModel::from_value(get(p, "DistanceModel")),
            radius_min: num_or(p, "RadiusMin", cdo::AMB_RADIUS_MIN),
            radius_max: num_or(p, "RadiusMax", cdo::AMB_RADIUS_MAX),
            lpf_radius_min: num_or(p, "LPFRadiusMin", cdo::AMB_LPF_RADIUS_MIN),
            lpf_radius_max: num_or(p, "LPFRadiusMax", cdo::AMB_LPF_RADIUS_MAX),
            ranges: ModulatorRanges {
                pitch_min: num_or(p, "PitchMin", cdo::AMB_PITCH),
                pitch_max: num_or(p, "PitchMax", cdo::AMB_PITCH),
                volume_min: num_or(p, "VolumeMin", cdo::AMB_VOLUME),
                volume_max: num_or(p, "VolumeMax", cdo::AMB_VOLUME),
            },
            slots,
            non_loop,
        }
    }
}

/// What a node does.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum NodeKind {
    /// `SoundNodeWave`.
    Wave(WaveRef),
    /// `SoundNodeWaveParam`: the wave set on the component under this name,
    /// else the children.
    WaveParam {
        /// `WaveParameterName`.
        name: String,
    },
    /// `SoundNodeAttenuation` (also used for `SoundNodeAttenuationAndGain`).
    Attenuation(Attenuation),
    /// `SoundNodeRandom`.
    Random(RandomNode),
    /// `SoundNodeMixer`.
    Mixer {
        /// `InputVolume`.
        input_volume: Vec<f32>,
    },
    /// `SoundNodeConcatenator`.
    Concatenator {
        /// `InputVolume`.
        input_volume: Vec<f32>,
    },
    /// `SoundNodeModulator`.
    Modulator(ModulatorRanges),
    /// `SoundNodeModulatorContinuous`.
    ModulatorContinuous {
        /// `VolumeModulation`.
        volume: Option<ParamDistribution>,
        /// `PitchModulation`.
        pitch: Option<ParamDistribution>,
    },
    /// `SoundNodeLooping` (and `ForcedLoopSoundNode`, read as an indefinite
    /// loop, TENTATIVE).
    Looping {
        /// `bLoopIndefinitely`.
        indefinitely: bool,
        /// `LoopCountMin`.
        count_min: f32,
        /// `LoopCountMax`.
        count_max: f32,
    },
    /// `SoundNodeDelay`.
    Delay {
        /// `DelayMin`.
        min: f32,
        /// `DelayMax`.
        max: f32,
    },
    /// `SoundNodeDistanceCrossFade`.
    DistanceCrossFade {
        /// `CrossFadeInput`.
        inputs: Vec<DistanceDatum>,
    },
    /// `SoundNodeAmbient*`.
    Ambient(Box<AmbientNode>),
    /// A node evaluated like the base `SoundNode` (every child, nothing
    /// else): classes that no shipped cue reaches.
    Passthrough {
        /// Class name.
        class: String,
    },
}

/// One node of a cue graph.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct NodeDef {
    /// Object path.
    pub path: String,
    /// Behaviour and values.
    pub kind: NodeKind,
    /// `ChildNodes` (`None`: empty input or unresolved reference).
    pub children: Vec<Option<NodeId>>,
}

/// A compiled `SoundCue`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CueDef {
    /// Object path.
    pub path: String,
    /// `SoundClass` name.
    pub sound_class: Option<String>,
    /// `VolumeMultiplier`.
    pub volume_multiplier: f32,
    /// `PitchMultiplier`.
    pub pitch_multiplier: f32,
    /// Cooked `Duration` (10000: loops forever).
    pub duration: Option<f32>,
    /// `MaxConcurrentPlayCount` (0: unlimited).
    pub max_concurrent_play_count: i32,
    /// `FirstNode`.
    pub first: Option<NodeId>,
    /// Nodes (index = [`NodeId`]).
    pub nodes: Vec<NodeDef>,
}

impl CueDef {
    /// Compiles one entry of `cues.json`.
    #[must_use]
    pub fn from_json(path: &str, entry: &Value) -> Self {
        let raw_nodes = get(entry, "nodes")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let mut index: BTreeMap<String, NodeId> = BTreeMap::new();
        for (i, n) in raw_nodes.iter().enumerate() {
            if let Some(p) = get(n, "path").and_then(Value::as_str) {
                index.entry(p.to_owned()).or_insert(i);
            }
        }
        let nodes = raw_nodes
            .iter()
            .map(|n| {
                let node_path = text(get(n, "path")).unwrap_or_default();
                let children = get(n, "children")
                    .and_then(Value::as_array)
                    .map(|a| {
                        a.iter()
                            .map(|c| c.as_str().and_then(|p| index.get(p).copied()))
                            .collect()
                    })
                    .unwrap_or_default();
                let kind = node_kind(n, &node_path);
                NodeDef {
                    path: node_path,
                    kind,
                    children,
                }
            })
            .collect();
        let first = get(entry, "first_node")
            .and_then(Value::as_str)
            .and_then(|p| index.get(p).copied());
        Self {
            path: path.to_owned(),
            sound_class: text(get(entry, "sound_class")),
            volume_multiplier: num_or(entry, "volume_multiplier", cdo::CUE_VOLUME_MULTIPLIER),
            pitch_multiplier: num_or(entry, "pitch_multiplier", cdo::CUE_PITCH_MULTIPLIER),
            duration: num(get(entry, "duration")),
            max_concurrent_play_count: get(entry, "max_concurrent_play_count")
                .and_then(Value::as_i64)
                .and_then(|v| i32::try_from(v).ok())
                .unwrap_or(cdo::CUE_MAX_CONCURRENT_PLAY_COUNT),
            first,
            nodes,
        }
    }

    /// The cue loops forever (cooked `Duration` 10000).
    #[must_use]
    pub fn loops_forever(&self) -> bool {
        self.duration.is_some_and(|d| d >= INDEFINITE_DURATION)
    }

    /// Wave paths of the graph (nodes and ambient slots), in node order.
    #[must_use]
    pub fn wave_paths(&self) -> Vec<String> {
        let mut out = Vec::new();
        let mut seen = BTreeSet::new();
        for n in &self.nodes {
            match &n.kind {
                NodeKind::Wave(w) => {
                    if seen.insert(w.path.clone()) {
                        out.push(w.path.clone());
                    }
                }
                NodeKind::Ambient(a) => {
                    for s in &a.slots {
                        if let Some(w) = &s.wave
                            && seen.insert(w.clone())
                        {
                            out.push(w.clone());
                        }
                    }
                }
                _ => {}
            }
        }
        out
    }

    /// `USoundCue::CalculateMaxAudibleDistance`: the largest distance any
    /// node reports, else [`WORLD_MAX_AUDIBLE_DISTANCE`]. A looping node
    /// reports `WORLD_MAX` (CONFIRMED, native: looping cues are always
    /// audible so they keep their state); a cross-fade node reports its
    /// largest `FadeInDistanceEnd` / `FadeOutDistanceEnd` (CONFIRMED,
    /// native). Attenuation and ambient nodes report `RadiusMax` (their
    /// functions were not read: TENTATIVE).
    #[must_use]
    pub fn max_audible_distance(&self) -> f32 {
        let mut d: f32 = 0.0;
        for n in &self.nodes {
            match &n.kind {
                NodeKind::Attenuation(a) => d = d.max(a.radius_max),
                NodeKind::Ambient(a) => d = d.max(a.radius_max),
                NodeKind::Looping { .. } => d = d.max(WORLD_MAX_AUDIBLE_DISTANCE),
                NodeKind::DistanceCrossFade { inputs } => {
                    for i in inputs {
                        d = d.max(i.fade_in_end).max(i.fade_out_end);
                    }
                }
                _ => {}
            }
        }
        if d == 0.0 {
            WORLD_MAX_AUDIBLE_DISTANCE
        } else {
            d
        }
    }
}

fn node_kind(n: &Value, node_path: &str) -> NodeKind {
    let empty = Value::Object(serde_json::Map::new());
    let p = get(n, "params").unwrap_or(&empty);
    let kind = get(n, "kind").and_then(Value::as_str).unwrap_or("");
    let class = get(n, "class").and_then(Value::as_str).unwrap_or("");
    match kind {
        "Wave" | "WaveStreaming" => {
            let w = get(n, "wave");
            NodeKind::Wave(WaveRef {
                path: node_path.to_owned(),
                volume: w
                    .and_then(|w| num(get(w, "volume")))
                    .unwrap_or(cdo::WAVE_VOLUME),
                pitch: w
                    .and_then(|w| num(get(w, "pitch")))
                    .unwrap_or(cdo::WAVE_PITCH),
            })
        }
        "WaveParam" => NodeKind::WaveParam {
            name: text(get(p, "WaveParameterName")).unwrap_or_default(),
        },
        "Attenuation" | "AttenuationAndGain" => NodeKind::Attenuation(Attenuation::from_params(p)),
        "Random" => NodeKind::Random(RandomNode {
            weights: floats(get(p, "Weights")),
            without_replacement: flag_or(p, "bRandomizeWithoutReplacement", true),
            preselect_at_level_load: get(p, "PreselectAtLevelLoad")
                .and_then(Value::as_i64)
                .and_then(|v| i32::try_from(v).ok())
                .unwrap_or(0),
        }),
        "Mixer" => NodeKind::Mixer {
            input_volume: floats(get(p, "InputVolume")),
        },
        "Concatenator" | "ConcatenatorRadio" => NodeKind::Concatenator {
            input_volume: floats(get(p, "InputVolume")),
        },
        "Modulator" => NodeKind::Modulator(ModulatorRanges {
            pitch_min: num_or(p, "PitchMin", cdo::MOD_PITCH_MIN),
            pitch_max: num_or(p, "PitchMax", cdo::MOD_PITCH_MAX),
            volume_min: num_or(p, "VolumeMin", cdo::MOD_VOLUME_MIN),
            volume_max: num_or(p, "VolumeMax", cdo::MOD_VOLUME_MAX),
        }),
        "ModulatorContinuous" => {
            let dist = |property: &str| {
                get(n, "distributions")
                    .and_then(Value::as_array)?
                    .iter()
                    .find(|d| get(d, "property").and_then(Value::as_str) == Some(property))
                    .map(|d| {
                        ParamDistribution::from_info(
                            get(d, "class").and_then(Value::as_str).unwrap_or(""),
                            get(d, "params").unwrap_or(&Value::Null),
                        )
                    })
            };
            NodeKind::ModulatorContinuous {
                volume: dist("VolumeModulation"),
                pitch: dist("PitchModulation"),
            }
        }
        "Looping" => NodeKind::Looping {
            indefinitely: flag_or(p, "bLoopIndefinitely", true),
            count_min: num_or(p, "LoopCountMin", cdo::LOOP_COUNT),
            count_max: num_or(p, "LoopCountMax", cdo::LOOP_COUNT),
        },
        "ForcedLoop" => NodeKind::Looping {
            indefinitely: true,
            count_min: cdo::LOOP_COUNT,
            count_max: cdo::LOOP_COUNT,
        },
        "Delay" => NodeKind::Delay {
            min: num_or(p, "DelayMin", 0.0),
            max: num_or(p, "DelayMax", 0.0),
        },
        "DistanceCrossFade" => NodeKind::DistanceCrossFade {
            inputs: get(p, "CrossFadeInput")
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .map(|d| DistanceDatum {
                            fade_in_start: num_or(d, "FadeInDistanceStart", 0.0),
                            fade_in_end: num_or(d, "FadeInDistanceEnd", 0.0),
                            fade_out_start: num_or(d, "FadeOutDistanceStart", 0.0),
                            fade_out_end: num_or(d, "FadeOutDistanceEnd", 0.0),
                            volume: num_or(d, "Volume", 1.0),
                        })
                        .collect()
                })
                .unwrap_or_default(),
        },
        "Ambient" | "AmbientNonLoop" | "AmbientNonLoopToggle" => {
            NodeKind::Ambient(Box::new(AmbientNode::from_params(p, kind)))
        }
        _ => NodeKind::Passthrough {
            class: class.to_owned(),
        },
    }
}

/// `SoundClassProperties` (members CONFIRMED from `Engine.u`).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct SoundClassProps {
    /// `Volume`.
    pub volume: f32,
    /// `Pitch`.
    pub pitch: f32,
    /// `StereoBleed`.
    pub stereo_bleed: f32,
    /// `LFEBleed`.
    pub lfe_bleed: f32,
    /// `VoiceCenterChannelVolume`.
    pub voice_center_channel_volume: f32,
    /// `bAlwaysPlay`.
    pub always_play: bool,
    /// `bIsUISound` (keeps playing while the game is paused).
    pub is_ui: bool,
    /// `bIsMusic`.
    pub is_music: bool,
    /// `bReverb`.
    pub reverb: bool,
}

impl Default for SoundClassProps {
    /// The struct defaults (CONFIRMED, `Engine.u`).
    fn default() -> Self {
        Self {
            volume: 1.0,
            pitch: 1.0,
            stereo_bleed: cdo::CLASS_STEREO_BLEED,
            lfe_bleed: cdo::CLASS_LFE_BLEED,
            voice_center_channel_volume: 0.0,
            always_play: false,
            is_ui: false,
            is_music: false,
            reverb: true,
        }
    }
}

impl SoundClassProps {
    fn from_params(p: &Value) -> Self {
        let d = Self::default();
        Self {
            volume: num_or(p, "Volume", d.volume),
            pitch: num_or(p, "Pitch", d.pitch),
            stereo_bleed: num_or(p, "StereoBleed", d.stereo_bleed),
            lfe_bleed: num_or(p, "LFEBleed", d.lfe_bleed),
            voice_center_channel_volume: num_or(
                p,
                "VoiceCenterChannelVolume",
                d.voice_center_channel_volume,
            ),
            always_play: flag_or(p, "bAlwaysPlay", d.always_play),
            is_ui: flag_or(p, "bIsUISound", d.is_ui),
            is_music: flag_or(p, "bIsMusic", d.is_music),
            reverb: flag_or(p, "bReverb", d.reverb),
        }
    }

    /// Linear interpolation of the numeric members (flags switch at the
    /// end), as the engine interpolates class properties during a sound
    /// mode fade.
    #[must_use]
    pub fn lerp(&self, to: &Self, t: f32) -> Self {
        let l = |a: f32, b: f32| a + (b - a) * t;
        let fin = t >= 1.0;
        Self {
            volume: l(self.volume, to.volume),
            pitch: l(self.pitch, to.pitch),
            stereo_bleed: l(self.stereo_bleed, to.stereo_bleed),
            lfe_bleed: l(self.lfe_bleed, to.lfe_bleed),
            voice_center_channel_volume: l(
                self.voice_center_channel_volume,
                to.voice_center_channel_volume,
            ),
            always_play: if fin {
                to.always_play
            } else {
                self.always_play
            },
            is_ui: if fin { to.is_ui } else { self.is_ui },
            is_music: if fin { to.is_music } else { self.is_music },
            reverb: if fin { to.reverb } else { self.reverb },
        }
    }
}

/// A `SoundClass`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SoundClassDef {
    /// Object name.
    pub name: String,
    /// Own `Properties`.
    pub props: SoundClassProps,
    /// `ChildClassNames`.
    pub children: Vec<String>,
}

/// One `SoundClassAdjuster` of a mode.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ClassAdjuster {
    /// Class name.
    pub class: String,
    /// `VolumeAdjuster`.
    pub volume: f32,
    /// `PitchAdjuster`.
    pub pitch: f32,
    /// `VoiceCenterChannelVolumeAdjuster`.
    pub voice_center: f32,
    /// `bApplyToChildren`.
    pub apply_to_children: bool,
}

/// A `SoundMode`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SoundModeDef {
    /// Object name.
    pub name: String,
    /// `InitialDelay`.
    pub initial_delay: f32,
    /// `FadeInTime`.
    pub fade_in_time: f32,
    /// `Duration` (negative: until another mode is set; it becomes the
    /// base mode).
    pub duration: f32,
    /// `FadeOutTime`.
    pub fade_out_time: f32,
    /// `SoundClassEffects`.
    pub effects: Vec<ClassAdjuster>,
}

impl SoundModeDef {
    fn from_json(name: &str, params: &Value) -> Self {
        let effects = get(params, "SoundClassEffects")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(|e| {
                        Some(ClassAdjuster {
                            class: text(get(e, "SoundClass"))?,
                            volume: num_or(e, "VolumeAdjuster", 1.0),
                            pitch: num_or(e, "PitchAdjuster", 1.0),
                            voice_center: num_or(e, "VoiceCenterChannelVolumeAdjuster", 1.0),
                            apply_to_children: flag_or(e, "bApplyToChildren", false),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();
        Self {
            name: name.to_owned(),
            initial_delay: num_or(params, "InitialDelay", 0.0),
            fade_in_time: num_or(params, "FadeInTime", 0.0),
            duration: num_or(params, "Duration", 0.0),
            fade_out_time: num_or(params, "FadeOutTime", 0.0),
            effects,
        }
    }
}

/// Ambient sound actor classes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum AmbientActorKind {
    /// `AmbientSound` (plays its audio component's cue).
    AmbientSound,
    /// `AmbientSoundMovable`.
    Movable,
    /// `AmbientSoundNonLoop`.
    NonLoop,
    /// `AmbientSoundSimple`.
    Simple,
    /// `AmbientSoundSimpleToggleable` (Kismet `SeqAct_Toggle`).
    SimpleToggleable,
    /// `AmbientSoundNonLoopingToggleable` (Kismet `SeqAct_Toggle`).
    NonLoopingToggleable,
    /// `AmbientSoundSpline` / `AmbientSoundSimpleSpline*`.
    Spline,
    /// `AmbientSoundSplineMultiCue`.
    SplineMultiCue,
    /// Anything else.
    Other,
}

impl AmbientActorKind {
    fn from_name(kind: &str) -> Self {
        match kind {
            "AmbientSound" => Self::AmbientSound,
            "Movable" => Self::Movable,
            "NonLoop" => Self::NonLoop,
            "Simple" => Self::Simple,
            "SimpleToggleable" => Self::SimpleToggleable,
            "NonLoopingToggleable" => Self::NonLoopingToggleable,
            "Spline" | "SimpleSpline" | "SimpleSplineNonLoop" => Self::Spline,
            "SplineMultiCue" => Self::SplineMultiCue,
            _ => Self::Other,
        }
    }

    /// Responds to Kismet `SeqAct_Toggle` (the toggleable classes define
    /// `OnToggle`; STRONG, engine script).
    #[must_use]
    pub fn toggleable(self) -> bool {
        matches!(self, Self::SimpleToggleable | Self::NonLoopingToggleable)
    }

    /// Class default of `bAutoPlay` (CONFIRMED cdo: true for `AmbientSound`
    /// and its subclasses, false for the toggleable ones).
    #[must_use]
    pub fn default_auto_play(self) -> bool {
        !self.toggleable()
    }
}

/// Toggle fades of the toggleable ambient sounds (`bFadeOnToggle`,
/// `FadeInDuration`, `FadeInVolumeLevel`, `FadeOutDuration`,
/// `FadeOutVolumeLevel`).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct ToggleFade {
    /// `bFadeOnToggle`.
    pub fade_on_toggle: bool,
    /// `FadeInDuration` (default 1.0).
    pub fade_in_duration: f32,
    /// `FadeInVolumeLevel` (default 1.0).
    pub fade_in_volume: f32,
    /// `FadeOutDuration` (default 1.0).
    pub fade_out_duration: f32,
    /// `FadeOutVolumeLevel` (default 0).
    pub fade_out_volume: f32,
}

impl Default for ToggleFade {
    fn default() -> Self {
        Self {
            fade_on_toggle: false,
            fade_in_duration: cdo::TOGGLE_FADE_IN_DURATION,
            fade_in_volume: cdo::TOGGLE_FADE_IN_VOLUME,
            fade_out_duration: cdo::TOGGLE_FADE_OUT_DURATION,
            fade_out_volume: 0.0,
        }
    }
}

/// One placed ambient sound actor (`ambient.json`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AmbientActorDef {
    /// Object name.
    pub name: String,
    /// Object path (`<map>.TheWorld.PersistentLevel.<name>`, the form the
    /// Kismet runtime reports targets in).
    pub path: String,
    /// Export index (joins with the level scene).
    pub export_index: usize,
    /// Class kind.
    pub kind: AmbientActorKind,
    /// `Location`.
    pub location: Vec3,
    /// Starts playing at level start.
    pub auto_play: bool,
    /// The cue played.
    pub cue: Option<String>,
    /// Audio component `VolumeMultiplier` (multi-cue splines: times the
    /// slot's `VolumeScale`, TENTATIVE).
    pub volume_multiplier: f32,
    /// Audio component `PitchMultiplier`.
    pub pitch_multiplier: f32,
    /// Toggle fades.
    pub toggle: ToggleFade,
    /// Spline points (spline actors).
    pub spline_points: Vec<Vec3>,
}

impl AmbientActorDef {
    fn from_json(a: &Value) -> Option<Self> {
        let name = text(get(a, "name"))?;
        let kind =
            AmbientActorKind::from_name(get(a, "kind").and_then(Value::as_str).unwrap_or(""));
        let location = vec3(get(a, "location")).unwrap_or(Vec3::ZERO);
        let empty = Value::Object(serde_json::Map::new());
        let comp = get(a, "audio_component_params").unwrap_or(&empty);
        let instance = get(a, "instance").unwrap_or(&empty);
        let mut volume_multiplier = num_or(a, "volume_multiplier", 1.0);
        if kind == AmbientActorKind::SplineMultiCue
            && let Some(slot) = get(comp, "SoundSlots")
                .and_then(Value::as_array)
                .and_then(|s| s.first())
        {
            volume_multiplier *= num_or(slot, "VolumeScale", 1.0);
        }
        let spline_points = get(comp, "Points")
            .and_then(Value::as_array)
            .map(|pts| {
                pts.iter()
                    .filter_map(|p| vec3(get(p, "Position")))
                    .collect()
            })
            .unwrap_or_default();
        let d = ToggleFade::default();
        Some(Self {
            path: text(get(a, "path")).unwrap_or_default(),
            name,
            export_index: get(a, "export_index")
                .and_then(Value::as_u64)
                .and_then(|v| usize::try_from(v).ok())
                .unwrap_or(0),
            kind,
            location,
            auto_play: get(a, "auto_play")
                .and_then(Value::as_bool)
                .unwrap_or_else(|| kind.default_auto_play()),
            cue: text(get(a, "sound_cue")),
            volume_multiplier,
            pitch_multiplier: num_or(a, "pitch_multiplier", 1.0),
            toggle: ToggleFade {
                fade_on_toggle: flag_or(instance, "bFadeOnToggle", d.fade_on_toggle),
                fade_in_duration: num_or(instance, "FadeInDuration", d.fade_in_duration),
                fade_in_volume: num_or(instance, "FadeInVolumeLevel", d.fade_in_volume),
                fade_out_duration: num_or(instance, "FadeOutDuration", d.fade_out_duration),
                fade_out_volume: num_or(instance, "FadeOutVolumeLevel", d.fade_out_volume),
            },
            spline_points,
        })
    }

    /// Where the sound is heard from: the actor, or for spline actors the
    /// closest point of the spline polyline to the listener (TENTATIVE:
    /// the engine's virtual speaker placement is not ported).
    #[must_use]
    pub fn source_location(&self, listener: Vec3) -> Vec3 {
        if self.spline_points.len() < 2 {
            return self.spline_points.first().copied().unwrap_or(self.location);
        }
        let mut best = self.location;
        let mut best_d = f32::INFINITY;
        for w in self.spline_points.windows(2) {
            let (a, b) = (w[0], w[1]);
            let ab = b - a;
            let len2 = ab.length_squared();
            let t = if len2 > 0.0 {
                ((listener - a).dot(ab) / len2).clamp(0.0, 1.0)
            } else {
                0.0
            };
            let p = a + ab * t;
            let d = p.distance_squared(listener);
            if d < best_d {
                best_d = d;
                best = p;
            }
        }
        best
    }
}

// ---------------------------------------------------------------------------
// Library
// ---------------------------------------------------------------------------

/// The converted audio documents of one converted directory.
#[derive(Clone, Debug, Default)]
pub struct AudioLibrary {
    /// Subtitle language.
    pub language: String,
    /// Language of the audio.
    pub audio_language: String,
    /// Waves by object path.
    pub waves: BTreeMap<String, WaveInfo>,
    /// Cues by object path.
    pub cues: BTreeMap<String, Arc<CueDef>>,
    /// Subtitles by wave path.
    pub subtitles: BTreeMap<String, SubtitleTrack>,
    /// Sound classes by name.
    pub classes: BTreeMap<String, SoundClassDef>,
    /// Sound modes by name.
    pub modes: BTreeMap<String, SoundModeDef>,
    /// Ambient actors by lower-case map package name.
    pub ambient: BTreeMap<String, Vec<AmbientActorDef>>,
    /// Lower-case cue path → cue path.
    lower_cues: BTreeMap<String, String>,
    /// Non-fatal problems found while loading.
    pub warnings: Vec<String>,
}

impl AudioLibrary {
    /// Loads `<audio_dir>/{manifest,cues,subtitles,sound_classes,ambient}.json`.
    /// Missing files leave their part empty (with a warning); malformed
    /// files are errors.
    ///
    /// # Errors
    /// Unreadable or malformed documents, or a wrong `format`/`version`.
    pub fn load(audio_dir: &Path) -> AssetResult<Self> {
        let read = |name: &str| -> AssetResult<Option<Value>> {
            let path = audio_dir.join(name);
            if !path.is_file() {
                return Ok(None);
            }
            let data = read_bounded(&path, MAX_AUDIO_JSON_BYTES)?;
            parse_json::<Value>(&path, &data).map(Some)
        };
        let docs = AudioDocuments {
            manifest: read("manifest.json")?,
            cues: read("cues.json")?,
            subtitles: read("subtitles.json")?,
            sound_classes: read("sound_classes.json")?,
            ambient: read("ambient.json")?,
        };
        Self::from_documents(audio_dir, docs)
    }

    /// Builds the library from parsed documents (`path` names the source in
    /// errors).
    ///
    /// # Errors
    /// A document with a wrong `format`/`version`, or an unsafe file path.
    pub fn from_documents(path: &Path, docs: AudioDocuments) -> AssetResult<Self> {
        let mut lib = Self::default();
        let p = |name: &str| -> PathBuf { path.join(name) };
        if let Some(m) = &docs.manifest {
            check_format(&p("manifest.json"), m, "asamu-audio-manifest")?;
            lib.language = text(get(m, "language")).unwrap_or_default();
            lib.audio_language = text(get(m, "audio_language")).unwrap_or_default();
            if let Some(waves) = get(m, "waves").and_then(Value::as_object) {
                for (wave_path, w) in waves {
                    let file = match get(w, "file").and_then(Value::as_str) {
                        Some(f) => Some(safe_relative_path(f)?),
                        None => None,
                    };
                    let rate = num(get(w, "sample_rate")).unwrap_or(0.0);
                    let samples = get(w, "samples").and_then(Value::as_u64);
                    let duration = match samples {
                        Some(s) if rate > 0.0 => s as f32 / rate,
                        _ => num(get(w, "duration")).unwrap_or(0.0),
                    };
                    lib.waves.insert(
                        wave_path.clone(),
                        WaveInfo {
                            path: wave_path.clone(),
                            file,
                            duration: duration.max(0.0),
                            channels: get(w, "channels")
                                .and_then(Value::as_u64)
                                .and_then(|c| u16::try_from(c).ok())
                                .unwrap_or(1),
                            volume: num_or(w, "volume", cdo::WAVE_VOLUME),
                            pitch: num_or(w, "pitch", cdo::WAVE_PITCH),
                        },
                    );
                }
            }
        } else {
            lib.warnings
                .push("no manifest.json: no audio files".to_owned());
        }
        if let Some(c) = &docs.cues {
            check_format(&p("cues.json"), c, "asamu-audio-cues")?;
            if let Some(cues) = get(c, "cues").and_then(Value::as_object) {
                for (cue_path, entry) in cues {
                    lib.add_cue(CueDef::from_json(cue_path, entry));
                }
            }
        } else {
            lib.warnings.push("no cues.json: no sound cues".to_owned());
        }
        if let Some(s) = &docs.subtitles {
            check_format(&p("subtitles.json"), s, "asamu-audio-subtitles")?;
            if lib.language.is_empty() {
                lib.language = text(get(s, "language")).unwrap_or_default();
            }
            if let Some(waves) = get(s, "waves").and_then(Value::as_object) {
                for (wave_path, t) in waves {
                    let lines = get(t, "lines")
                        .and_then(Value::as_array)
                        .map(|a| {
                            a.iter()
                                .map(|l| SubtitleLine {
                                    text: text(get(l, "text")).unwrap_or_default(),
                                    time: num_or(l, "time", 0.0),
                                })
                                .collect()
                        })
                        .unwrap_or_default();
                    let classes = get(t, "cue_sound_classes")
                        .and_then(Value::as_array)
                        .map(|a| a.iter().filter_map(|c| text(Some(c))).collect())
                        .unwrap_or_default();
                    lib.subtitles.insert(
                        wave_path.clone(),
                        SubtitleTrack {
                            lines,
                            duration: num(get(t, "duration")),
                            manual_word_wrap: flag_or(t, "manual_word_wrap", false),
                            single_line: flag_or(t, "single_line", false),
                            cue_sound_classes: classes,
                        },
                    );
                }
            }
        }
        if let Some(s) = &docs.sound_classes {
            check_format(&p("sound_classes.json"), s, "asamu-audio-sound-classes")?;
            for c in get(s, "classes")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                let Some(name) = text(get(c, "name")) else {
                    continue;
                };
                let props = get(c, "properties")
                    .map(SoundClassProps::from_params)
                    .unwrap_or_default();
                let children = get(c, "child_class_names")
                    .and_then(Value::as_array)
                    .map(|a| a.iter().filter_map(|n| text(Some(n))).collect())
                    .unwrap_or_default();
                lib.classes.insert(
                    name.clone(),
                    SoundClassDef {
                        name,
                        props,
                        children,
                    },
                );
            }
            for m in get(s, "modes")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                let Some(name) = text(get(m, "name")) else {
                    continue;
                };
                let params = get(m, "params").cloned().unwrap_or(Value::Null);
                lib.modes
                    .insert(name.clone(), SoundModeDef::from_json(&name, &params));
            }
        }
        if let Some(a) = &docs.ambient {
            check_format(&p("ambient.json"), a, "asamu-audio-ambient")?;
            for m in get(a, "maps")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                let Some(package) = text(get(m, "package")) else {
                    continue;
                };
                let actors = get(m, "ambient")
                    .and_then(Value::as_array)
                    .map(|a| a.iter().filter_map(AmbientActorDef::from_json).collect())
                    .unwrap_or_default();
                lib.ambient.insert(package.to_ascii_lowercase(), actors);
            }
        }
        Ok(lib)
    }

    /// Adds (or replaces) a cue.
    pub fn add_cue(&mut self, cue: CueDef) {
        self.lower_cues
            .insert(cue.path.to_ascii_lowercase(), cue.path.clone());
        self.cues.insert(cue.path.clone(), Arc::new(cue));
    }

    /// Adds (or replaces) a wave.
    pub fn add_wave(&mut self, wave: WaveInfo) {
        self.waves.insert(wave.path.clone(), wave);
    }

    /// The cue at `path` (exact, then ignoring ASCII case like UE3 names).
    #[must_use]
    pub fn cue(&self, path: &str) -> Option<&Arc<CueDef>> {
        self.cues.get(path).or_else(|| {
            self.lower_cues
                .get(&path.to_ascii_lowercase())
                .and_then(|p| self.cues.get(p))
        })
    }

    /// The wave at `path`.
    #[must_use]
    pub fn wave(&self, path: &str) -> Option<&WaveInfo> {
        self.waves.get(path)
    }

    /// The mode named `name` (ignoring ASCII case).
    #[must_use]
    pub fn mode(&self, name: &str) -> Option<&SoundModeDef> {
        let short = name.rsplit('.').next().unwrap_or(name);
        self.modes.get(short).or_else(|| {
            self.modes
                .values()
                .find(|m| m.name.eq_ignore_ascii_case(short))
        })
    }

    /// Ambient actors of a map package (ignoring ASCII case).
    #[must_use]
    pub fn ambient_for_map(&self, map: &str) -> &[AmbientActorDef] {
        self.ambient
            .get(&map.to_ascii_lowercase())
            .map_or(&[], Vec::as_slice)
    }
}

/// Parsed audio documents (each optional), for [`AudioLibrary::from_documents`].
#[derive(Clone, Debug, Default)]
pub struct AudioDocuments {
    /// `manifest.json`.
    pub manifest: Option<Value>,
    /// `cues.json`.
    pub cues: Option<Value>,
    /// `subtitles.json`.
    pub subtitles: Option<Value>,
    /// `sound_classes.json`.
    pub sound_classes: Option<Value>,
    /// `ambient.json`.
    pub ambient: Option<Value>,
}

/// True when `bytes` start with an Ogg page whose first packet is a Vorbis
/// identification header. The runtime checks this before handing a file
/// to a decoder (a decoder that cannot recognise the data may abort).
#[must_use]
pub fn looks_like_ogg_vorbis(bytes: &[u8]) -> bool {
    if bytes.len() < 27 || &bytes[0..4] != b"OggS" {
        return false;
    }
    let segments = usize::from(bytes[26]);
    let start = 27 + segments;
    bytes
        .get(start..start + 7)
        .is_some_and(|h| h == b"\x01vorbis")
}

/// CRC-32 table of the Ogg page checksum (polynomial 0x04C11DB7, no
/// reflection, initial value 0; the Ogg specification).
const OGG_CRC_TABLE: [u32; 256] = {
    let mut table = [0_u32; 256];
    let mut i = 0;
    while i < 256 {
        let mut r = (i as u32) << 24;
        let mut k = 0;
        while k < 8 {
            r = if r & 0x8000_0000 != 0 {
                (r << 1) ^ 0x04C1_1DB7
            } else {
                r << 1
            };
            k += 1;
        }
        table[i] = r;
        i += 1;
    }
    table
};

/// Checks that `bytes` are one complete, undamaged Ogg Vorbis stream as
/// far as a decoder's set-up reads it, so that handing them to a decoder
/// that aborts on bad input (Bevy's) is safe: every page is in bounds with
/// a correct checksum, one logical stream from a BOS page to an EOS page
/// with consecutive sequence numbers and no bytes after it, and the first
/// three packets are the Vorbis identification (version 0, channels and
/// rate non-zero, block sizes 64..=8192 with the short one not larger, the
/// framing bit set), comment and setup headers. The same container rules
/// the converter applies before writing a file (AUDIO.md).
///
/// # Errors
/// A description of the first problem found.
pub fn validate_ogg_vorbis(bytes: &[u8]) -> Result<(), String> {
    // Header packets are a few kilobytes; a larger one is refused.
    const MAX_HEADER_PACKET: usize = 1 << 20;
    let mut pos = 0_usize;
    let mut serial = None;
    let mut sequence = 0_u32;
    let mut last_flags = 0_u8;
    let mut packets: Vec<Vec<u8>> = Vec::new();
    let mut current: Vec<u8> = Vec::new();
    while pos < bytes.len() {
        let page = &bytes[pos..];
        if page.len() < 27 {
            return Err(format!("truncated page header at byte {pos}"));
        }
        if &page[0..4] != b"OggS" || page[4] != 0 {
            return Err(format!("no Ogg page (version 0) at byte {pos}"));
        }
        let flags = page[5];
        if flags > 7 {
            return Err(format!("unknown page flags {flags:#x} at byte {pos}"));
        }
        let page_serial = u32::from_le_bytes([page[14], page[15], page[16], page[17]]);
        let page_seq = u32::from_le_bytes([page[18], page[19], page[20], page[21]]);
        let crc = u32::from_le_bytes([page[22], page[23], page[24], page[25]]);
        let segments = usize::from(page[26]);
        let table = page
            .get(27..27 + segments)
            .ok_or_else(|| format!("truncated segment table at byte {pos}"))?;
        let body_len: usize = table.iter().map(|&l| usize::from(l)).sum();
        let page_len = 27 + segments + body_len;
        let whole = page
            .get(..page_len)
            .ok_or_else(|| format!("truncated page at byte {pos}"))?;
        let mut sum = 0_u32;
        for (i, &b) in whole.iter().enumerate() {
            let b = if (22..26).contains(&i) { 0 } else { b };
            sum = (sum << 8) ^ OGG_CRC_TABLE[usize::from((sum >> 24) as u8 ^ b)];
        }
        if sum != crc {
            return Err(format!("page checksum mismatch at byte {pos}"));
        }
        match serial {
            None => {
                if flags & 0x02 == 0 {
                    return Err("the first page is not a BOS page".to_owned());
                }
                serial = Some(page_serial);
                sequence = page_seq;
            }
            Some(s) => {
                if page_serial != s {
                    return Err("more than one logical stream".to_owned());
                }
                if flags & 0x02 != 0 {
                    return Err(format!("BOS flag on a later page at byte {pos}"));
                }
                if last_flags & 0x04 != 0 {
                    return Err(format!("page after the EOS page at byte {pos}"));
                }
                sequence = sequence.wrapping_add(1);
                if page_seq != sequence {
                    return Err(format!("page sequence gap at byte {pos}"));
                }
            }
        }
        // Assemble the first three packets (the Vorbis headers).
        if packets.len() < 3 {
            if (flags & 0x01 != 0) == current.is_empty() {
                return Err(format!("continuation flag mismatch at byte {pos}"));
            }
            let mut offset = 27 + segments;
            for &len in table {
                let len = usize::from(len);
                if packets.len() < 3 {
                    current.extend_from_slice(&whole[offset..offset + len]);
                    if current.len() > MAX_HEADER_PACKET {
                        return Err("oversized Vorbis header packet".to_owned());
                    }
                    if len < 255 {
                        packets.push(std::mem::take(&mut current));
                    }
                }
                offset += len;
            }
        }
        last_flags = flags;
        pos += page_len;
    }
    if serial.is_none() {
        return Err("no Ogg page".to_owned());
    }
    if last_flags & 0x04 == 0 {
        return Err("the last page is not an EOS page".to_owned());
    }
    let header = |i: usize, kind: u8| -> Result<&Vec<u8>, String> {
        packets
            .get(i)
            .filter(|p| p.len() >= 7 && p[0] == kind && &p[1..7] == b"vorbis")
            .ok_or_else(|| format!("Vorbis header packet {i} (type {kind}) missing"))
    };
    let ident = header(0, 1)?;
    header(1, 3)?;
    header(2, 5)?;
    if ident.len() < 30 {
        return Err("short Vorbis identification header".to_owned());
    }
    let version = u32::from_le_bytes([ident[7], ident[8], ident[9], ident[10]]);
    let channels = ident[11];
    let rate = u32::from_le_bytes([ident[12], ident[13], ident[14], ident[15]]);
    let (short, long) = (ident[28] & 0x0F, ident[28] >> 4);
    if version != 0 || channels == 0 || rate == 0 {
        return Err("invalid Vorbis identification header".to_owned());
    }
    if !(6..=13).contains(&short) || !(6..=13).contains(&long) || short > long {
        return Err("invalid Vorbis block sizes".to_owned());
    }
    if ident[29] & 0x01 == 0 {
        return Err("Vorbis identification header without framing bit".to_owned());
    }
    Ok(())
}

/// Reads the audio file of `wave` from `audio_dir` (bounded, Ogg Vorbis
/// checked with [`validate_ogg_vorbis`]).
///
/// # Errors
/// No file, unreadable or oversized file, or data that is not a valid Ogg
/// Vorbis stream.
pub fn read_wave_file(audio_dir: &Path, wave: &WaveInfo) -> AssetResult<Vec<u8>> {
    let Some(file) = &wave.file else {
        return Err(AssetError::Format {
            path: audio_dir.to_path_buf(),
            expected: format!("an audio file for {}", wave.path),
            found: "no file in manifest.json".to_owned(),
        });
    };
    let rel = safe_relative_path(file)?;
    let path = audio_dir.join(rel);
    let data = read_bounded(&path, MAX_WAVE_FILE_BYTES)?;
    if let Err(problem) = validate_ogg_vorbis(&data) {
        return Err(AssetError::Format {
            path,
            expected: "a valid Ogg Vorbis stream".to_owned(),
            found: problem,
        });
    }
    Ok(data)
}

// ---------------------------------------------------------------------------
// Sound classes and modes
// ---------------------------------------------------------------------------

/// Effective class properties: each class's own values times its parent's
/// effective volume and pitch, from `Master` down; `bIsUISound` and
/// `bIsMusic` are inherited. Classes not reached from `Master` are absent
/// (a cue of such a class gets no class factor). CONFIRMED (native class
/// parse).
#[must_use]
pub fn resolve_classes(
    classes: &BTreeMap<String, SoundClassDef>,
    volume_overrides: &BTreeMap<String, f32>,
) -> BTreeMap<String, SoundClassProps> {
    fn own(
        classes: &BTreeMap<String, SoundClassDef>,
        overrides: &BTreeMap<String, f32>,
        name: &str,
    ) -> Option<SoundClassProps> {
        let mut p = classes.get(name)?.props;
        if let Some(v) = overrides.get(name) {
            p.volume = *v;
        }
        Some(p)
    }
    let mut out = BTreeMap::new();
    let Some(master) = own(classes, volume_overrides, "Master") else {
        return out;
    };
    out.insert("Master".to_owned(), master);
    let mut stack = vec![("Master".to_owned(), master, 0usize)];
    while let Some((name, parent, depth)) = stack.pop() {
        if depth > MAX_PARSE_DEPTH {
            continue;
        }
        let children = classes
            .get(&name)
            .map(|c| c.children.clone())
            .unwrap_or_default();
        for child in children {
            if out.contains_key(&child) {
                continue;
            }
            let Some(mut p) = own(classes, volume_overrides, &child) else {
                continue;
            };
            p.volume *= parent.volume;
            p.pitch *= parent.pitch;
            p.is_ui |= parent.is_ui;
            p.is_music |= parent.is_music;
            out.insert(child.clone(), p);
            stack.push((child, p, depth + 1));
        }
    }
    out
}

/// Applies a mode's adjusters to resolved class properties (volume, pitch
/// and centre-channel volume multiply; `bApplyToChildren` also adjusts every
/// descendant). CONFIRMED (native).
#[must_use]
pub fn apply_mode(
    base: &BTreeMap<String, SoundClassProps>,
    classes: &BTreeMap<String, SoundClassDef>,
    mode: Option<&SoundModeDef>,
) -> BTreeMap<String, SoundClassProps> {
    let mut out = base.clone();
    let Some(mode) = mode else {
        return out;
    };
    for adj in &mode.effects {
        let mut targets = vec![adj.class.clone()];
        if adj.apply_to_children {
            let mut i = 0;
            while i < targets.len() && targets.len() <= classes.len() + 1 {
                let kids = classes
                    .get(&targets[i])
                    .map(|c| c.children.clone())
                    .unwrap_or_default();
                for k in kids {
                    if !targets.contains(&k) {
                        targets.push(k);
                    }
                }
                i += 1;
            }
        }
        for t in targets {
            if let Some(p) = out.get_mut(&t) {
                p.volume *= adj.volume;
                p.pitch *= adj.pitch;
                p.voice_center_channel_volume *= adj.voice_center;
            }
        }
    }
    out
}

/// The current sound mode and its fade (`UAudioDevice::ApplySoundMode` /
/// `Interpolate` semantics, CONFIRMED native; the starting base mode, none,
/// is TENTATIVE).
#[derive(Clone, Debug, Default)]
pub struct SoundModeState {
    current: Option<String>,
    base: Option<String>,
    fade_start: f64,
    fade_end: f64,
    end: Option<f64>,
    source: BTreeMap<String, SoundClassProps>,
    dest: BTreeMap<String, SoundClassProps>,
    now: BTreeMap<String, SoundClassProps>,
    overrides: BTreeMap<String, f32>,
    initialised: bool,
}

impl SoundModeState {
    /// The active mode's name.
    #[must_use]
    pub fn current(&self) -> Option<&str> {
        self.current.as_deref()
    }

    /// Effective properties of `class` now (`None`: no such class in the
    /// tree, which means no class factor).
    #[must_use]
    pub fn class(&self, class: &str) -> Option<&SoundClassProps> {
        self.now.get(class)
    }

    fn ensure_init(&mut self, lib: &AudioLibrary) {
        if !self.initialised {
            self.initialised = true;
            let base = resolve_classes(&lib.classes, &self.overrides);
            self.source = base.clone();
            self.dest = base.clone();
            self.now = base;
        }
    }

    /// Overrides a class's own volume (user volume settings; the engine's
    /// `SetClassVolume`), applied to its descendants through the tree.
    pub fn set_class_volume(&mut self, lib: &AudioLibrary, class: &str, volume: f32, time: f64) {
        self.ensure_init(lib);
        self.overrides.insert(class.to_owned(), volume.max(0.0));
        let base = resolve_classes(&lib.classes, &self.overrides);
        let mode = self.current.as_deref().and_then(|m| lib.mode(m));
        self.dest = apply_mode(&base, &lib.classes, mode);
        let t = self.fraction(time);
        self.source = self.interpolated(t);
        self.fade_start = time;
        self.fade_end = time;
        self.update(time);
    }

    /// Activates `name` at `time`. An unknown name changes nothing (the
    /// engine's `SetSoundMode` finds no mode and returns; CONFIRMED,
    /// native); `None` (our API, not the engine's) returns to the base mode,
    /// or to the plain class values when no base mode was set.
    pub fn set(&mut self, lib: &AudioLibrary, name: Option<&str>, time: f64) {
        self.ensure_init(lib);
        let mode = match name {
            Some(n) => match lib.mode(n) {
                Some(m) => Some(m.clone()),
                None => return,
            },
            None => self.base.as_deref().and_then(|b| lib.mode(b)).cloned(),
        };
        let new_name = mode.as_ref().map(|m| m.name.clone());
        if new_name == self.current && self.current.is_some() {
            return;
        }
        let t = self.fraction(time);
        self.source = self.interpolated(t);
        let old_fade_out = self
            .current
            .as_deref()
            .and_then(|m| lib.mode(m))
            .map_or(0.0, |m| m.fade_out_time);
        if new_name.is_some() && new_name == self.base || new_name.is_none() {
            // Back to the base: fade over the leaving mode's FadeOutTime.
            self.fade_start = time;
            self.fade_end = time + f64::from(old_fade_out.max(0.0));
            self.end = None;
        } else if let Some(m) = &mode {
            self.fade_start = time + f64::from(m.initial_delay.max(0.0));
            self.fade_end = self.fade_start + f64::from(m.fade_in_time.max(0.0));
            self.end = (m.duration >= 0.0).then(|| self.fade_end + f64::from(m.duration));
            if m.duration < 0.0 {
                self.base = Some(m.name.clone());
            }
        }
        self.current = new_name;
        let base = resolve_classes(&lib.classes, &self.overrides);
        self.dest = apply_mode(&base, &lib.classes, mode.as_ref());
        self.now = self.interpolated(self.fraction(time));
    }

    fn fraction(&self, time: f64) -> f32 {
        if time < self.fade_start {
            0.0
        } else if time >= self.fade_end {
            1.0
        } else {
            ((time - self.fade_start) / (self.fade_end - self.fade_start)) as f32
        }
    }

    fn interpolated(&self, t: f32) -> BTreeMap<String, SoundClassProps> {
        self.dest
            .iter()
            .map(|(k, d)| {
                let s = self.source.get(k).unwrap_or(d);
                (k.clone(), s.lerp(d, t))
            })
            .collect()
    }

    /// Advances to `time`; a timed mode whose duration ran out returns to
    /// the base mode.
    pub fn update_with(&mut self, lib: &AudioLibrary, time: f64) {
        self.ensure_init(lib);
        if let Some(end) = self.end
            && time >= end
        {
            let base = self.base.clone();
            self.end = None;
            self.set(lib, base.as_deref(), time);
        }
        self.update(time);
    }

    fn update(&mut self, time: f64) {
        self.now = self.interpolated(self.fraction(time));
    }
}

// ---------------------------------------------------------------------------
// Cue instances (audio components)
// ---------------------------------------------------------------------------

/// Id of a playing cue instance (an "audio component").
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct InstanceId(pub u64);

/// How a cue instance is started.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PlayParams {
    /// Source location (UU).
    pub location: Vec3,
    /// `bAllowSpatialization`: false for the player's own sounds (the
    /// engine's `ClientHearSound` turns it off when the source is the view
    /// target, STRONG) and 2-D sounds; attenuation nodes then do nothing.
    pub allow_spatialization: bool,
    /// Component `VolumeMultiplier`.
    pub volume_multiplier: f32,
    /// Component `PitchMultiplier`.
    pub pitch_multiplier: f32,
    /// `SubtitlePriority` (0: no subtitles).
    pub subtitle_priority: f32,
    /// `bSuppressSubtitles`.
    pub suppress_subtitles: bool,
    /// `FadeIn(duration, volume)` instead of `Play()`.
    pub fade_in: Option<(f32, f32)>,
    /// Refuse to start when the listener is beyond the cue's audible
    /// distance (`USoundCue::IsAudible` for sounds heard from another
    /// actor).
    pub check_audible: bool,
    /// `bShouldRemainActiveIfDropped`: a wave instance that does not get a
    /// channel waits for one instead of finishing (CONFIRMED native rule;
    /// true for the ambient sound actors' components, CONFIRMED cdo of
    /// `AmbientSound`, inherited by its subclasses).
    pub remain_active_if_dropped: bool,
}

impl PlayParams {
    /// A non-spatialised sound (UI, the player's own sounds).
    #[must_use]
    pub fn two_d() -> Self {
        Self {
            location: Vec3::ZERO,
            allow_spatialization: false,
            volume_multiplier: 1.0,
            pitch_multiplier: 1.0,
            subtitle_priority: 0.0,
            suppress_subtitles: false,
            fade_in: None,
            check_audible: false,
            remain_active_if_dropped: false,
        }
    }

    /// A sound heard from `location`.
    #[must_use]
    pub fn at(location: Vec3) -> Self {
        Self {
            location,
            allow_spatialization: true,
            ..Self::two_d()
        }
    }
}

/// How a wave instance loops at the source.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum LoopMode {
    /// Plays once, then notifies its hook.
    Never,
    /// Loops seamlessly, notifying the hook at every loop end (a wave under
    /// a looping node with no node in between that clears the flag).
    WithNotification,
    /// Loops seamlessly without notifications (ambient slots).
    Forever,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum ChildSlot {
    Index(usize),
    /// The wave of a `SoundNodeWaveParam` (the engine passes index −1).
    Param,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct WaveKey {
    parent: Option<NodeId>,
    child: ChildSlot,
    wave: String,
}

#[derive(Clone, Debug, PartialEq)]
struct WaveInstance {
    wave: String,
    ancestors: Vec<NodeId>,
    hook: Option<NodeId>,
    mode: LoopMode,
    volume: f32,
    pitch: f32,
    hf_gain: f32,
    spatial: bool,
    started: bool,
    finished: bool,
    notified: bool,
    position: f32,
    voice: Option<u64>,
}

impl WaveInstance {
    fn new(wave: &str) -> Self {
        Self {
            wave: wave.to_owned(),
            ancestors: Vec::new(),
            hook: None,
            mode: LoopMode::Never,
            volume: 0.0,
            pitch: 1.0,
            hf_gain: 1.0,
            spatial: false,
            started: false,
            finished: false,
            notified: false,
            position: 0.0,
            voice: None,
        }
    }
}

/// Per-instance node payload (the engine keeps one per component and node;
/// `Fresh` is its "needs initialisation" flag).
#[derive(Clone, Copy, Debug, PartialEq)]
enum NodeState {
    Fresh,
    Random {
        chosen: usize,
    },
    Modulator {
        volume: f32,
        pitch: f32,
    },
    Looping {
        remaining: i32,
        finished_count: u32,
    },
    Delay {
        delay: f32,
        start: f32,
    },
    Concatenator {
        index: usize,
    },
    Ambient {
        volume: f32,
        pitch: f32,
    },
    AmbientNonLoop {
        volume: f32,
        pitch: f32,
        next: f32,
        slot: usize,
    },
}

/// No-repeat bookkeeping of a `SoundNodeRandom`, shared by every instance
/// of the node (the engine keeps it on the node object).
#[derive(Clone, Debug, Default, PartialEq)]
struct RandomUsage {
    used: Vec<bool>,
    num_used: usize,
}

/// State shared by all instances: the random nodes' no-repeat lists (keyed
/// by node path).
#[derive(Clone, Debug, Default)]
pub struct SharedNodeState {
    random: BTreeMap<String, RandomUsage>,
}

/// Component fades (`FadeIn`, `FadeOut`, `AdjustVolume`). CONFIRMED (native).
#[derive(Clone, Copy, Debug, PartialEq)]
struct Fades {
    in_start: f32,
    in_stop: f32,
    in_target: f32,
    out_start: f32,
    out_stop: f32,
    out_target: f32,
    adj_start: f32,
    adj_stop: f32,
    adj_target: f32,
    adj_current: f32,
}

impl Default for Fades {
    /// The component class defaults (CONFIRMED, `Engine.u`): stop times −1,
    /// targets 1.
    fn default() -> Self {
        Self {
            in_start: 0.0,
            in_stop: -1.0,
            in_target: 1.0,
            out_start: 0.0,
            out_stop: -1.0,
            out_target: 1.0,
            adj_start: 0.0,
            adj_stop: -1.0,
            adj_target: 1.0,
            adj_current: 1.0,
        }
    }
}

fn clamp01_or(x: f32, nan: f32) -> f32 {
    if x.is_nan() {
        nan
    } else if x < 0.0 {
        0.0
    } else {
        x.min(1.0)
    }
}

impl Fades {
    fn fade_in(&self, t: f32) -> f32 {
        if t <= self.in_stop {
            clamp01_or(
                ((t - self.in_start) / (self.in_stop - self.in_start)) * self.in_target,
                self.in_target,
            )
        } else if self.in_stop < t {
            self.in_target
        } else {
            1.0
        }
    }

    fn fade_out(&self, t: f32) -> f32 {
        if t <= self.out_stop {
            let frac = (t - self.out_start) / (self.out_stop - self.out_start);
            let target = self.out_target;
            if target < 1.0 {
                1.0 - clamp01_or(frac * (1.0 - target), 0.0)
            } else if target > 1.0 {
                1.0 + clamp01_or(frac * (target - 1.0), 0.0)
            } else {
                1.0
            }
        } else if self.out_stop < t {
            self.out_target
        } else {
            1.0
        }
    }

    fn adjust(&mut self, t: f32) -> f32 {
        if t <= self.adj_stop {
            let frac = (t - self.adj_start) / (self.adj_stop - self.adj_start);
            let (cur, target) = (self.adj_current, self.adj_target);
            if cur < target {
                cur + clamp01_or(frac * (target - cur), 0.0)
            } else if cur > target {
                cur - clamp01_or(frac * (cur - target), 0.0)
            } else {
                cur
            }
        } else if self.adj_stop < t {
            self.adj_current = self.adj_target;
            self.adj_target
        } else {
            1.0
        }
    }
}

/// One playing cue (an audio component).
#[derive(Clone, Debug)]
pub struct CueInstance {
    cue: Arc<CueDef>,
    params: PlayParams,
    location: Vec3,
    playback_time: f32,
    nodes: Vec<NodeState>,
    waves: BTreeMap<WaveKey, WaveInstance>,
    float_params: BTreeMap<String, f32>,
    wave_params: BTreeMap<String, String>,
    fades: Fades,
    stopped: bool,
}

impl CueInstance {
    fn new(cue: Arc<CueDef>, params: PlayParams) -> Self {
        let n = cue.nodes.len();
        let mut fades = Fades::default();
        if let Some((d, v)) = params.fade_in
            && d >= 0.0
        {
            fades.in_start = 0.0;
            fades.in_stop = d;
            fades.in_target = v;
        }
        Self {
            location: params.location,
            cue,
            params,
            playback_time: 0.0,
            nodes: vec![NodeState::Fresh; n],
            waves: BTreeMap::new(),
            float_params: BTreeMap::new(),
            wave_params: BTreeMap::new(),
            fades,
            stopped: false,
        }
    }

    /// The cue played.
    #[must_use]
    pub fn cue(&self) -> &Arc<CueDef> {
        &self.cue
    }

    /// Seconds since `Play`.
    #[must_use]
    pub fn playback_time(&self) -> f32 {
        self.playback_time
    }

    /// The instance stopped (finished, faded out or stopped).
    #[must_use]
    pub fn is_stopped(&self) -> bool {
        self.stopped
    }

    fn component_volume(&mut self) -> f32 {
        let t = self.playback_time;
        self.params.volume_multiplier
            * self.cue.volume_multiplier
            * self.fades.fade_in(t)
            * self.fades.fade_out(t)
            * self.fades.adjust(t)
    }

    /// `FadeIn` on a playing component: while fading out the fade reverses
    /// smoothly from the current level (no restart). Returns false when
    /// the component would have to restart (the engine calls `Play`).
    fn fade_in_playing(&mut self, duration: f32, volume: f32) -> bool {
        let t = self.playback_time;
        if self.fades.out_stop <= t {
            return false;
        }
        if duration >= 0.0 {
            let level = self.fades.fade_out(t);
            let start = t - level * duration;
            self.fades.in_start = start;
            self.fades.in_stop = start + duration;
            self.fades.in_target = volume;
        }
        self.fades.out_start = 0.0;
        self.fades.out_stop = -1.0;
        self.fades.out_target = 1.0;
        true
    }

    /// `FadeOut(duration, volume)`: a negative duration stops at once; the
    /// component stops when the fade ends. While fading in the fade-out
    /// starts from the current level.
    fn fade_out(&mut self, duration: f32, volume: f32) {
        if duration < 0.0 {
            self.stop();
            return;
        }
        let t = self.playback_time;
        if self.fades.in_stop <= t {
            self.fades.out_start = t;
            self.fades.out_stop = t + duration;
            self.fades.out_target = volume;
        } else {
            let level = self.fades.fade_in(t);
            let start = t - (1.0 - level) * duration;
            self.fades.out_start = start;
            self.fades.out_stop = start + duration;
            self.fades.out_target = volume;
            self.fades.in_start = 0.0;
            self.fades.in_stop = -1.0;
            self.fades.in_target = 1.0;
        }
    }

    fn adjust_volume(&mut self, duration: f32, volume: f32) {
        if duration >= 0.0 {
            let t = self.playback_time;
            self.fades.adj_start = t;
            self.fades.adj_stop = t + duration;
            self.fades.adj_target = volume;
        }
    }

    fn stop(&mut self) {
        self.stopped = true;
        self.waves.clear();
    }

    /// Nodes reachable from `node` through the current selections (the
    /// engine's `GetNodes`): a random node that has chosen continues only
    /// into its choice; ambient nodes have no child nodes.
    fn active_subtree(&self, node: NodeId) -> Vec<NodeId> {
        let mut out = Vec::new();
        let mut seen = vec![false; self.cue.nodes.len()];
        let mut stack = vec![node];
        while let Some(n) = stack.pop() {
            match seen.get_mut(n) {
                Some(s) if !*s => *s = true,
                _ => continue,
            }
            out.push(n);
            let Some(def) = self.cue.nodes.get(n) else {
                continue;
            };
            match (&def.kind, self.nodes.get(n)) {
                (NodeKind::Random(_), Some(NodeState::Random { chosen })) => {
                    if let Some(Some(c)) = def.children.get(*chosen) {
                        stack.push(*c);
                    }
                }
                (NodeKind::Random(_), _) | (NodeKind::Ambient(_), _) => {}
                _ => {
                    for c in def.children.iter().rev().flatten() {
                        stack.push(*c);
                    }
                }
            }
        }
        out
    }
}

/// Component state threaded through a parse (the engine's `Current*`
/// members of the audio component).
#[derive(Clone, Copy, Debug)]
struct Comp {
    volume: f32,
    pitch: f32,
    hf: f32,
    spatial: bool,
    notify_on_loop: bool,
    hook: Option<NodeId>,
    finished: bool,
}

/// What Mixer / Concatenator / cross-fade save and restore around each
/// child (`FAudioComponentSavedState`: hook, location, volume, pitch, HF
/// gain, spatialisation, loop notification; CONFIRMED native).
#[derive(Clone, Copy, Debug)]
struct Saved {
    volume: f32,
    pitch: f32,
    hf: f32,
    spatial: bool,
    notify_on_loop: bool,
    hook: Option<NodeId>,
}

impl Comp {
    fn save(&self) -> Saved {
        Saved {
            volume: self.volume,
            pitch: self.pitch,
            hf: self.hf,
            spatial: self.spatial,
            notify_on_loop: self.notify_on_loop,
            hook: self.hook,
        }
    }

    fn restore(&mut self, s: Saved) {
        self.volume = s.volume;
        self.pitch = s.pitch;
        self.hf = s.hf;
        self.spatial = s.spatial;
        self.notify_on_loop = s.notify_on_loop;
        self.hook = s.hook;
    }
}

struct Parse<'a> {
    lib: &'a AudioLibrary,
    cue: &'a CueDef,
    rng: &'a mut UeRand,
    shared: &'a mut SharedNodeState,
    listener: Vec3,
    location: Vec3,
    time: f32,
    allow_spatialization: bool,
    always_play: bool,
    float_params: &'a BTreeMap<String, f32>,
    wave_params: &'a BTreeMap<String, String>,
    nodes: &'a mut [NodeState],
    waves: &'a mut BTreeMap<WaveKey, WaveInstance>,
    path: Vec<NodeId>,
    out: Vec<WaveKey>,
    /// Wave instances created by this parse (the engine's `HandleStart`).
    created: Vec<WaveKey>,
    visits: usize,
}

impl<'a> Parse<'a> {
    fn children(&mut self, c: &mut Comp, node: NodeId) {
        let cue: &'a CueDef = self.cue;
        let Some(def) = cue.nodes.get(node) else {
            return;
        };
        for (i, child) in def.children.iter().enumerate() {
            if let Some(child) = child {
                self.node(c, *child, Some(node), ChildSlot::Index(i));
            }
        }
    }

    fn node(&mut self, c: &mut Comp, node: NodeId, parent: Option<NodeId>, slot: ChildSlot) {
        if self.path.len() >= MAX_PARSE_DEPTH
            || self.path.contains(&node)
            || self.visits >= MAX_PARSE_VISITS
        {
            return;
        }
        self.visits += 1;
        let cue: &'a CueDef = self.cue;
        let Some(def) = cue.nodes.get(node) else {
            return;
        };
        self.path.push(node);
        match &def.kind {
            NodeKind::Wave(w) => {
                let wave_path = w.path.clone();
                self.wave(c, &wave_path, w.volume, w.pitch, parent, slot);
            }
            NodeKind::WaveParam { name } => {
                let param = self.wave_params.get(name).cloned();
                match param {
                    Some(wave_path) => {
                        let (vol, pitch) = self
                            .lib
                            .wave(&wave_path)
                            .map_or((cdo::WAVE_VOLUME, cdo::WAVE_PITCH), |w| (w.volume, w.pitch));
                        self.wave(c, &wave_path, vol, pitch, Some(node), ChildSlot::Param);
                    }
                    None => self.children(c, node),
                }
            }
            NodeKind::Attenuation(a) => {
                if self.allow_spatialization {
                    let d = a.distance_type.distance(self.location, self.listener);
                    if a.attenuate {
                        c.volume *=
                            attenuation_eval(a.model, d, a.radius_min, a.radius_max, a.db_at_max);
                    }
                    if a.attenuate_with_lpf {
                        c.hf = lpf_gain(d, a.lpf_radius_min, a.lpf_radius_max);
                    }
                    c.spatial |= a.spatialize;
                } else {
                    c.spatial = false;
                }
                self.children(c, node);
            }
            NodeKind::Random(r) => self.random(c, node, def, r),
            NodeKind::Mixer { input_volume } => {
                c.notify_on_loop = false;
                for (i, child) in def.children.iter().enumerate() {
                    if let Some(child) = child {
                        let saved = c.save();
                        c.volume *= input_volume.get(i).copied().unwrap_or(1.0);
                        self.node(c, *child, Some(node), ChildSlot::Index(i));
                        c.restore(saved);
                    }
                }
            }
            NodeKind::Concatenator { input_volume } => {
                if self.nodes[node] == NodeState::Fresh {
                    self.nodes[node] = NodeState::Concatenator { index: 0 };
                }
                let index = match self.nodes[node] {
                    NodeState::Concatenator { index } => index,
                    _ => 0,
                };
                let n = def.children.len();
                if index < n {
                    c.notify_on_loop = false;
                    if index + 1 < n {
                        c.hook = Some(node);
                    }
                    if let Some(Some(child)) = def.children.get(index) {
                        let saved = c.save();
                        c.volume *= input_volume.get(index).copied().unwrap_or(1.0);
                        self.node(c, *child, Some(node), ChildSlot::Index(index));
                        c.restore(saved);
                    }
                }
            }
            NodeKind::Modulator(m) => {
                if self.nodes[node] == NodeState::Fresh {
                    let volume = self.rng.pick(m.volume_min, m.volume_max);
                    let pitch = self.rng.pick(m.pitch_min, m.pitch_max);
                    self.nodes[node] = NodeState::Modulator { volume, pitch };
                }
                if let NodeState::Modulator { volume, pitch } = self.nodes[node] {
                    c.volume *= volume;
                    c.pitch *= pitch;
                }
                self.children(c, node);
            }
            NodeKind::ModulatorContinuous { volume, pitch } => {
                let v = volume.as_ref().map_or(1.0, |d| d.value(self.float_params));
                let p = pitch.as_ref().map_or(1.0, |d| d.value(self.float_params));
                c.volume *= v;
                c.pitch *= p;
                self.children(c, node);
            }
            NodeKind::Looping {
                indefinitely,
                count_min,
                count_max,
            } => {
                if self.nodes[node] == NodeState::Fresh {
                    let count = self.rng.pick(*count_min, *count_max);
                    // Truncation toward zero, saturating (float → int cast).
                    self.nodes[node] = NodeState::Looping {
                        remaining: count as i32,
                        finished_count: 0,
                    };
                }
                if let NodeState::Looping { remaining, .. } = self.nodes[node]
                    && (*indefinitely || remaining > 0)
                {
                    c.hook = Some(node);
                    c.notify_on_loop = true;
                }
                self.children(c, node);
            }
            NodeKind::Delay { min, max } => {
                c.notify_on_loop = false;
                if self.nodes[node] == NodeState::Fresh {
                    let delay = self.rng.pick(*min, *max);
                    self.nodes[node] = NodeState::Delay {
                        delay,
                        start: self.time,
                    };
                }
                if let NodeState::Delay { delay, start } = self.nodes[node] {
                    if delay <= self.time - start {
                        self.children(c, node);
                    } else {
                        c.finished = false;
                    }
                }
            }
            NodeKind::DistanceCrossFade { inputs } => {
                c.notify_on_loop = false;
                let d = self.location.distance(self.listener);
                for (i, child) in def.children.iter().enumerate() {
                    if let Some(child) = child {
                        let saved = c.save();
                        let datum = inputs.get(i).copied().unwrap_or_default();
                        c.volume *= cross_fade_gain(&datum, d);
                        self.node(c, *child, Some(node), ChildSlot::Index(i));
                        c.restore(saved);
                    }
                }
            }
            NodeKind::Ambient(a) => self.ambient(c, node, a),
            NodeKind::Passthrough { .. } => self.children(c, node),
        }
        self.path.pop();
    }

    /// `SoundNodeRandom`: the choice is made once per play (or after a
    /// loop re-initialises the node) and kept. CONFIRMED (native), including
    /// the engine's quirk that, without replacement, used inputs drop out
    /// of the weight sum but are still subtracted while walking the inputs.
    fn random(&mut self, c: &mut Comp, node: NodeId, def: &NodeDef, r: &RandomNode) {
        c.notify_on_loop = false;
        let n = def.children.len();
        let usage = self.shared.random.entry(def.path.clone()).or_default();
        if r.without_replacement {
            usage.used.resize(n, false);
            usage.num_used = usage
                .num_used
                .min(usage.used.iter().filter(|u| **u).count());
        }
        if self.nodes[node] == NodeState::Fresh {
            let mut chosen = 0;
            let weight_count = r.weights.len();
            let sum: f32 = if r.without_replacement {
                r.weights
                    .iter()
                    .enumerate()
                    .filter(|(i, _)| !usage.used.get(*i).copied().unwrap_or(false))
                    .map(|(_, w)| *w)
                    .sum()
            } else {
                r.weights.iter().sum()
            };
            let mut choice = sum * self.rng.frand();
            for i in 0..n {
                if i >= weight_count {
                    break;
                }
                let w = r.weights[i];
                if r.without_replacement {
                    let used = usage.used.get(i).copied().unwrap_or(false);
                    if choice <= w && !used {
                        if let Some(u) = usage.used.get_mut(i) {
                            *u = true;
                        }
                        usage.num_used += 1;
                        chosen = i;
                        break;
                    }
                } else if choice <= w {
                    chosen = i;
                    break;
                }
                choice -= w;
            }
            self.nodes[node] = NodeState::Random { chosen };
        }
        let chosen = match self.nodes[node] {
            NodeState::Random { chosen } => chosen,
            _ => 0,
        };
        if r.without_replacement && !usage.used.is_empty() && usage.num_used >= usage.used.len() {
            usage.used.iter_mut().for_each(|u| *u = false);
            if let Some(u) = usage.used.get_mut(chosen) {
                *u = true;
            }
            usage.num_used = 1;
        }
        if let Some(Some(child)) = def.children.get(chosen) {
            self.node(c, *child, Some(node), ChildSlot::Index(chosen));
        }
    }

    fn pick_slot(&mut self, a: &AmbientNode) -> usize {
        if a.slots.is_empty() {
            return 0;
        }
        let sum: f32 = a.slots.iter().map(|s| s.weight).sum();
        let choice = sum * self.rng.frand();
        let mut acc = 0.0;
        for (i, s) in a.slots.iter().enumerate() {
            acc += s.weight;
            if choice <= acc {
                return i;
            }
        }
        a.slots.len() - 1
    }

    /// `SoundNodeAmbient` (every slot plays at once, looping forever) and
    /// `SoundNodeAmbientNonLoop` (one weighted slot at a time, a random
    /// delay before each). CONFIRMED (native). Unlike the attenuation node,
    /// the ambient node always attenuates by the 3-D distance.
    fn ambient(&mut self, c: &mut Comp, node: NodeId, a: &AmbientNode) {
        if self.nodes[node] == NodeState::Fresh {
            let volume = self.rng.pick(a.ranges.volume_min, a.ranges.volume_max);
            let pitch = self.rng.pick(a.ranges.pitch_min, a.ranges.pitch_max);
            self.nodes[node] = match a.non_loop {
                Some(nl) => {
                    let next = self.rng.pick(nl.delay_min, nl.delay_max) + self.time;
                    let slot = self.pick_slot(a);
                    NodeState::AmbientNonLoop {
                        volume,
                        pitch,
                        next,
                        slot,
                    }
                }
                None => NodeState::Ambient { volume, pitch },
            };
        }
        let d = self.location.distance(self.listener);
        if a.attenuate {
            c.volume *= attenuation_eval(a.model, d, a.radius_min, a.radius_max, a.db_at_max);
        }
        if a.attenuate_with_lpf {
            c.hf = lpf_gain(d, a.lpf_radius_min, a.lpf_radius_max);
        }
        c.spatial |= a.spatialize;
        match self.nodes[node] {
            NodeState::Ambient { volume, pitch } => {
                c.volume *= volume;
                c.pitch *= pitch;
                c.hook = Some(node);
                let (base_v, base_p) = (c.volume, c.pitch);
                for (i, s) in a.slots.iter().enumerate() {
                    let Some(wave_path) = &s.wave else {
                        continue;
                    };
                    let Some(info) = self.lib.wave(wave_path) else {
                        continue;
                    };
                    let (wv, wp) = (info.volume, info.pitch);
                    c.volume = base_v * s.volume_scale;
                    c.pitch = base_p * s.pitch_scale;
                    let before = self.out.len();
                    self.wave(c, wave_path, wv, wp, Some(node), ChildSlot::Index(i));
                    for key in &self.out[before..] {
                        if let Some(w) = self.waves.get_mut(key) {
                            w.mode = LoopMode::Forever;
                        }
                    }
                    c.volume = base_v;
                    c.pitch = base_p;
                }
            }
            NodeState::AmbientNonLoop {
                volume,
                pitch,
                next,
                slot,
            } => {
                c.volume *= volume;
                c.pitch *= pitch;
                if let Some(s) = a.slots.get(slot) {
                    c.volume *= s.volume_scale;
                    c.pitch *= s.pitch_scale;
                }
                c.hook = Some(node);
                c.finished = false;
                if self.time >= next && slot < a.slots.len() {
                    let wave = a.slots[slot]
                        .wave
                        .as_ref()
                        .and_then(|w| self.lib.wave(w).map(|i| (w.clone(), i.volume, i.pitch)));
                    match wave {
                        Some((wave_path, wv, wp)) => {
                            self.wave(c, &wave_path, wv, wp, Some(node), ChildSlot::Index(slot));
                        }
                        None => {
                            let nl = a.non_loop.unwrap_or(NonLoop {
                                delay_min: 0.0,
                                delay_max: 0.0,
                                toggle: false,
                            });
                            let next = self.rng.pick(nl.delay_min, nl.delay_max) + self.time;
                            let slot = self.pick_slot(a);
                            self.nodes[node] = NodeState::AmbientNonLoop {
                                volume,
                                pitch,
                                next,
                                slot,
                            };
                        }
                    }
                }
            }
            _ => {}
        }
    }

    /// `SoundNodeWave`: multiplies the wave's own volume and pitch, then
    /// creates or updates the wave instance; a finished instance is left
    /// alone and does not keep the component alive. CONFIRMED (native).
    fn wave(
        &mut self,
        c: &mut Comp,
        wave_path: &str,
        wave_volume: f32,
        wave_pitch: f32,
        parent: Option<NodeId>,
        slot: ChildSlot,
    ) {
        c.volume *= wave_volume;
        c.pitch *= wave_pitch;
        let key = WaveKey {
            parent,
            child: slot,
            wave: wave_path.to_owned(),
        };
        let wi = match self.waves.entry(key.clone()) {
            std::collections::btree_map::Entry::Occupied(e) => e.into_mut(),
            std::collections::btree_map::Entry::Vacant(e) => {
                self.created.push(key.clone());
                e.insert(WaveInstance::new(wave_path))
            }
        };
        if wi.finished {
            return;
        }
        wi.volume = c.volume;
        wi.pitch = c.pitch;
        wi.hf_gain = c.hf;
        wi.spatial = c.spatial;
        wi.mode = if c.notify_on_loop {
            LoopMode::WithNotification
        } else {
            LoopMode::Never
        };
        wi.hook = c.hook;
        wi.ancestors.clone_from(&self.path);
        // `bIsStarted` set and `bAlreadyNotifiedHook` cleared by every parse
        // of an unfinished instance (CONFIRMED, native).
        wi.started = true;
        wi.notified = false;
        let priority = c.volume + if self.always_play { 1.0 } else { 0.0 };
        if priority > MIN_PLAY_PRIORITY {
            self.out.push(key);
        }
        c.finished = false;
    }
}

// ---------------------------------------------------------------------------
// Subtitles
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq)]
struct SubtitleEntry {
    priority: f32,
    /// (absolute start time, text); the last entry is the empty end marker.
    lines: Vec<(f64, String)>,
}

impl SubtitleEntry {
    /// The line index the engine's cursor is on at `now`: it starts at the
    /// first line and moves on whenever the next line has started, so a
    /// line whose successor starts earlier (2 shipped lists are not in time
    /// order) is skipped. `None` once the end marker is reached.
    fn cursor(&self, now: f64) -> Option<usize> {
        let last = self.lines.len().checked_sub(1)?;
        let mut i = 0;
        while i < last && self.lines.get(i + 1).is_some_and(|(t, _)| *t <= now) {
            i += 1;
        }
        (i < last).then_some(i)
    }
}

/// The subtitle queue (`FSubtitleManager`). CONFIRMED (native): a wave with
/// lines queues them when its wave instance is created
/// (`USoundNodeWave::HandleStart`), on a component with a non-zero
/// subtitle priority and subtitles not suppressed, keyed by the wave
/// instance; times are offset by the audio clock (a line later than the
/// wave's `Duration` is clamped to it) and an empty line at the end of the
/// wave hides it; a stopping component kills its entries. The shown line
/// is the highest-priority entry whose current line has started (ties: the
/// most recently queued, TENTATIVE: the engine keeps whichever its hash
/// set visits last).
#[derive(Clone, Debug, Default)]
pub struct SubtitleManager {
    entries: BTreeMap<(InstanceId, u64), SubtitleEntry>,
    counter: u64,
}

impl SubtitleManager {
    /// Queues `lines` of a wave lasting `duration` seconds (its `Duration`
    /// property) that starts at `now` on the component `id`.
    pub fn queue(
        &mut self,
        id: InstanceId,
        priority: f32,
        duration: f32,
        lines: &[SubtitleLine],
        now: f64,
    ) {
        if priority == 0.0 || duration == 0.0 || lines.is_empty() {
            return;
        }
        let mut out: Vec<(f64, String)> = lines
            .iter()
            .map(|l| {
                // Negative times are left as stored (not offset).
                let t = if l.time < 0.0 {
                    f64::from(l.time)
                } else {
                    now + f64::from(if l.time <= duration { l.time } else { duration })
                };
                (t, l.text.clone())
            })
            .collect();
        out.push((now + f64::from(duration), String::new()));
        self.counter += 1;
        self.entries.insert(
            (id, self.counter),
            SubtitleEntry {
                priority,
                lines: out,
            },
        );
    }

    /// Removes the lines of a component (it stopped).
    pub fn kill(&mut self, id: InstanceId) {
        self.entries.retain(|(i, _), _| *i != id);
    }

    /// The line to show at `now` (and drops finished entries).
    pub fn current(&mut self, now: f64) -> Option<String> {
        self.entries.retain(|_, e| e.cursor(now).is_some());
        let mut best: Option<(f32, u64, &str)> = None;
        for ((_, order), e) in &self.entries {
            let Some(i) = e.cursor(now) else {
                continue;
            };
            let Some((start, text)) = e.lines.get(i) else {
                continue;
            };
            if *start > now {
                continue;
            }
            if best.is_none_or(|(p, o, _)| e.priority > p || (e.priority == p && *order > o)) {
                best = Some((e.priority, *order, text));
            }
        }
        best.and_then(|(_, _, t)| (!t.is_empty()).then(|| t.to_owned()))
    }
}

// ---------------------------------------------------------------------------
// Narrator
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq)]
struct NarratorLine {
    id: String,
    cue: String,
    volume: f32,
    delay: f32,
    /// The Kismet action that added the line (part of the line's identity:
    /// removing a line removes every queued line equal to it).
    node: Option<u64>,
}

/// The narrator's two timers (`Finishedline`, `DelayedStart`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum NarratorTimerKind {
    FinishedLine,
    DelayedStart,
}

/// An actor timer (`AActor::SetTimer` / `UpdateTimers`, CONFIRMED native):
/// the count grows by the frame time and the timer fires once it exceeds
/// the rate; a rate of 0 removes the timer without firing.
#[derive(Clone, Copy, Debug, PartialEq)]
struct NarratorTimer {
    kind: NarratorTimerKind,
    rate: f32,
    count: f32,
}

/// The ASAMU narrator queue (`ASAMUNarratorManager`; behaviour from local
/// reading of the script, STRONG; timers CONFIRMED native):
///
/// - a line added to an empty queue starts at once (its delay is not
///   used) and fires "started narrating";
/// - each line ends after its cue's cooked `Duration` (a timer, not the
///   audio), fires its Kismet `FinishedLine` output, and the next queued
///   line starts after **its** delay;
/// - a timer of 0 seconds never fires, so a queued line with delay 0
///   stalls the queue;
/// - removing the playing line stops it at once but leaves its end timer
///   running, which later ends the *next* line (a script quirk, kept);
///   removing a line removes every queued line equal to it;
/// - "remove all other cues" removes queued lines but never the playing
///   one;
/// - when the last line ends, "finished narrating" fires.
///
/// The Kismet runtime (`asamu-kismet`) has its own port of this queue; a
/// host that uses it drives the narrator sound with
/// [`AudioCommand::NarratorPlay`] / [`AudioCommand::NarratorStop`] instead
/// of [`AudioCommand::NarratorAddLine`].
#[derive(Clone, Debug, Default)]
pub struct Narrator {
    lines: Vec<NarratorLine>,
    component: Option<InstanceId>,
    /// The narrator component's `SoundCue` (set when a line ends, played
    /// by the delayed start).
    pending_cue: Option<String>,
    timers: Vec<NarratorTimer>,
}

impl Narrator {
    /// Queued line ids (the first is the current one).
    #[must_use]
    pub fn queue(&self) -> Vec<&str> {
        self.lines.iter().map(|l| l.id.as_str()).collect()
    }

    /// `SetTimer(rate)`: an existing timer restarts (count 0) with the new
    /// rate; a new one is appended.
    fn set_timer(&mut self, kind: NarratorTimerKind, rate: f32) {
        let rate = if rate.is_finite() { rate } else { 0.0 };
        if let Some(t) = self.timers.iter_mut().find(|t| t.kind == kind) {
            t.rate = rate;
            t.count = 0.0;
        } else {
            self.timers.push(NarratorTimer {
                kind,
                rate,
                count: 0.0,
            });
        }
    }

    /// `ClearTimer`.
    fn clear_timer(&mut self, kind: NarratorTimerKind) {
        self.timers.retain(|t| t.kind != kind);
    }
}

// ---------------------------------------------------------------------------
// Commands and feedback
// ---------------------------------------------------------------------------

/// Where a commanded sound is heard from.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SoundSource {
    /// Not spatialised (UI, narration, the player's own sounds).
    #[default]
    TwoD,
    /// A fixed world position (UU).
    Location {
        /// Position, UU.
        location: [f32; 3],
    },
    /// The player pawn (follows it; the back end updates the position).
    Player,
}

/// Kismet `SeqAct_Toggle` inputs.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToggleAction {
    /// Input 0, "Turn On".
    TurnOn,
    /// Input 1, "Turn Off".
    TurnOff,
    /// Input 2, "Toggle".
    Toggle,
}

fn one() -> f32 {
    1.0
}

fn yes() -> bool {
    true
}

fn remove_fade() -> f32 {
    NARRATOR_REMOVE_FADE
}

/// Requests from gameplay or the Kismet runtime.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "command", rename_all = "snake_case")]
pub enum AudioCommand {
    /// Engine `SeqAct_PlaySound` (`Kismet_ClientPlaySound`): subtitle
    /// priority 10000, `FadeIn(fade_in_time, 1)`.
    PlaySound {
        /// Cue object path.
        cue: String,
        /// Where it is heard from.
        #[serde(default)]
        source: SoundSource,
        /// `VolumeMultiplier`.
        #[serde(default = "one")]
        volume_multiplier: f32,
        /// `PitchMultiplier`.
        #[serde(default = "one")]
        pitch_multiplier: f32,
        /// `FadeInTime`.
        #[serde(default)]
        fade_in_time: f32,
        /// `bSuppressSubtitles`.
        #[serde(default)]
        suppress_subtitles: bool,
        /// Not spatialised even with a location.
        #[serde(default)]
        suppress_spatialization: bool,
        /// The Kismet action that plays it (lets [`AudioCommand::StopSound`]
        /// stop exactly that action's sounds).
        #[serde(default)]
        node: Option<u64>,
    },
    /// Engine `SeqAct_PlaySound` "Stop" input (`Kismet_ClientStopSound`):
    /// fades out (or stops) the commanded instances of `node` when given,
    /// else those of `cue`.
    StopSound {
        /// Cue object path (ignored when `node` is given).
        #[serde(default)]
        cue: String,
        /// `FadeOutTime` (0: stop now).
        #[serde(default)]
        fade_out_time: f32,
        /// The Kismet action whose sounds stop.
        #[serde(default)]
        node: Option<u64>,
    },
    /// `SeqAct_NarratorLine` "AddLine".
    NarratorAddLine {
        /// Line id.
        id: String,
        /// Cue object path.
        cue: String,
        /// `CueVolume`.
        #[serde(default = "one")]
        volume: f32,
        /// `Delay`.
        #[serde(default)]
        delay: f32,
        /// `removeAllOtherCues`.
        #[serde(default)]
        remove_all_others: bool,
        /// `fadeOutIfActiveCue`.
        #[serde(default = "yes")]
        fade_out_if_active: bool,
        /// The `SeqAct_NarratorLine` node (part of the line's identity).
        #[serde(default)]
        node: Option<u64>,
    },
    /// `SeqAct_NarratorLine` "RemoveLine".
    NarratorRemoveLine {
        /// Line id.
        id: String,
        /// Stop the line if it is playing (`fadeOutIfActiveCue`).
        #[serde(default = "yes")]
        stop_if_active: bool,
        /// Fade time (unused by the script: it stops at once).
        #[serde(default = "remove_fade")]
        fade_time: f32,
    },
    /// Plays one narrator line now, for a host that runs the narrator
    /// queue itself (the Kismet runtime's port of `ASAMUNarratorManager`,
    /// whose `Output::NarratorLine` this mirrors): the narrator component
    /// restarts with `cue` (2-D, subtitle priority 10000, `VolumeMultiplier`
    /// = `volume`). No queue, timers or feedback; do not mix with
    /// [`AudioCommand::NarratorAddLine`].
    NarratorPlay {
        /// Line id (informational).
        #[serde(default)]
        id: String,
        /// Cue object path.
        cue: String,
        /// `CueVolume`.
        #[serde(default = "one")]
        volume: f32,
    },
    /// Stops the narrator component (`narratorAudioComponent.Stop()`; the
    /// Kismet runtime's `Output::NarratorStop`).
    NarratorStop {
        /// Line id (informational).
        #[serde(default)]
        id: String,
    },
    /// `SeqAct_SetSoundMode` (mode object path or name; an unknown mode
    /// changes nothing; `None` returns to the base).
    SetSoundMode {
        /// Mode name or object path.
        mode: Option<String>,
    },
    /// `SeqAct_Toggle` on a toggleable ambient sound actor (by object name).
    ToggleAmbient {
        /// Actor object name.
        actor: String,
        /// Which input fired.
        action: ToggleAction,
    },
    /// Stops every sound (level change).
    StopAll,
}

/// Events the audio side reports back (to Kismet).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum AudioFeedback {
    /// `SeqEvent_NarratorEvents` "StartedNarrating".
    NarratorStarted,
    /// `SeqAct_NarratorLine` output 2, "FinishedLine", of the line `id`.
    NarratorLineFinished {
        /// Line id.
        id: String,
    },
    /// `SeqEvent_NarratorEvents` "FinishedNarrating".
    NarratorFinished,
    /// A command named a cue the converted data does not have.
    UnknownCue {
        /// Cue path.
        cue: String,
    },
}

// ---------------------------------------------------------------------------
// Engine
// ---------------------------------------------------------------------------

/// One sound to play now.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Voice {
    /// Stable id while the same wave plays uninterrupted; a restart gets a
    /// new id.
    pub id: u64,
    /// Instance that plays it.
    pub instance: InstanceId,
    /// Wave object path.
    pub wave: String,
    /// Final linear gain in `[0, 1]` (node chain × component × cue × class
    /// × fades, stereo bleed, clamped like the source).
    pub gain: f32,
    /// Final pitch (speed), clamped to `[MIN_PITCH, MAX_PITCH]`.
    pub pitch: f32,
    /// Distance low-pass gain (not applied by the current back end).
    pub hf_gain: f32,
    /// Spatialised (mono waves only: the engine does not spatialise stereo).
    pub spatial: bool,
    /// Source position (UU).
    pub location: Vec3,
    /// Loops seamlessly at the source.
    pub looping: bool,
    /// Keeps playing while the game is paused (`bIsUISound`).
    pub ui: bool,
    /// Seconds into the wave (content time).
    pub position: f32,
}

/// Output of one [`AudioEngine::update`].
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct AudioFrame {
    /// Voices to play, loudest first, at most [`MAX_CHANNELS`].
    pub voices: Vec<Voice>,
    /// Events for Kismet.
    pub feedback: Vec<AudioFeedback>,
    /// Subtitle line to show.
    pub subtitle: Option<String>,
}

#[derive(Clone, Debug)]
struct AmbientRuntime {
    def: AmbientActorDef,
    instance: Option<InstanceId>,
}

/// An instance started by an [`AudioCommand::PlaySound`].
#[derive(Clone, Debug, Default)]
struct Commanded {
    cue: String,
    node: Option<u64>,
}

/// The audio engine: cue instances, the shared random state, sound modes,
/// the narrator, subtitles and ambient actors. Deterministic for a given
/// seed and input sequence.
#[derive(Clone, Debug)]
pub struct AudioEngine {
    rng: UeRand,
    shared: SharedNodeState,
    instances: BTreeMap<InstanceId, CueInstance>,
    next_instance: u64,
    next_voice: u64,
    modes: SoundModeState,
    subtitles: SubtitleManager,
    narrator: Narrator,
    ambient: Vec<AmbientRuntime>,
    commanded: BTreeMap<InstanceId, Commanded>,
    followers: BTreeSet<InstanceId>,
    feedback: Vec<AudioFeedback>,
    /// Real time (sound mode fades: the engine interpolates modes on
    /// `GCurrentTime`).
    real_time: f64,
    /// Audio time: game time that stops while the game is paused (the
    /// subtitle clock, `GetAudioTimeSeconds`; CONFIRMED native call, the
    /// pause behaviour of that clock is STRONG).
    audio_time: f64,
    paused: bool,
    master_volume: f32,
    player_location: Vec3,
}

impl AudioEngine {
    /// A silent engine seeded with `seed`.
    #[must_use]
    pub fn new(seed: u32) -> Self {
        Self {
            rng: UeRand::new(seed),
            shared: SharedNodeState::default(),
            instances: BTreeMap::new(),
            next_instance: 1,
            next_voice: 1,
            modes: SoundModeState::default(),
            subtitles: SubtitleManager::default(),
            narrator: Narrator::default(),
            ambient: Vec::new(),
            commanded: BTreeMap::new(),
            followers: BTreeSet::new(),
            feedback: Vec::new(),
            real_time: 0.0,
            audio_time: 0.0,
            paused: false,
            master_volume: 1.0,
            player_location: Vec3::ZERO,
        }
    }

    /// The random generator's state.
    #[must_use]
    pub fn rng(&self) -> UeRand {
        self.rng
    }

    /// Pauses game sounds (sounds of `bIsUISound` classes keep playing).
    pub fn set_paused(&mut self, paused: bool) {
        self.paused = paused;
    }

    /// Game sounds are paused.
    #[must_use]
    pub fn is_paused(&self) -> bool {
        self.paused
    }

    /// Master volume (a runtime setting, not original data).
    pub fn set_master_volume(&mut self, volume: f32) {
        self.master_volume = if volume.is_finite() {
            volume.max(0.0)
        } else {
            1.0
        };
    }

    /// Overrides a sound class's volume (options menu).
    pub fn set_class_volume(&mut self, lib: &AudioLibrary, class: &str, volume: f32) {
        self.modes
            .set_class_volume(lib, class, volume, self.real_time);
    }

    /// The player pawn's location (UU) for [`SoundSource::Player`] sounds
    /// and [`Self::follow_player`] instances.
    pub fn set_player_location(&mut self, location: Vec3) {
        if location.is_finite() {
            self.player_location = location;
        }
    }

    /// The player pawn's location (UU) as last set.
    #[must_use]
    pub fn player_location(&self) -> Vec3 {
        self.player_location
    }

    /// Makes an instance follow the player pawn (sounds of actors attached
    /// to the pawn: the gun, the boots, the power glove, the wind).
    pub fn follow_player(&mut self, id: InstanceId) {
        if self.instances.contains_key(&id) {
            self.followers.insert(id);
        }
    }

    /// The sound mode state.
    #[must_use]
    pub fn modes(&self) -> &SoundModeState {
        &self.modes
    }

    /// The narrator queue.
    #[must_use]
    pub fn narrator(&self) -> &Narrator {
        &self.narrator
    }

    /// The instance `id` is playing.
    #[must_use]
    pub fn is_playing(&self, id: InstanceId) -> bool {
        self.instances.get(&id).is_some_and(|i| !i.stopped)
    }

    /// The instance `id`, if it still exists.
    #[must_use]
    pub fn instance(&self, id: InstanceId) -> Option<&CueInstance> {
        self.instances.get(&id)
    }

    /// Number of live instances.
    #[must_use]
    pub fn instance_count(&self) -> usize {
        self.instances.len()
    }

    /// Cue paths of the playing instances, in instance order (debugging,
    /// tests).
    #[must_use]
    pub fn playing_cues(&self) -> Vec<&str> {
        self.instances
            .values()
            .filter(|i| !i.stopped)
            .map(|i| i.cue.path.as_str())
            .collect()
    }

    /// Starts `cue` (`UAudioComponent::Play`, or `FadeIn` with
    /// `params.fade_in`). Refused (`None`) for an unknown cue, when the
    /// cue already plays `MaxConcurrentPlayCount` times (CONFIRMED,
    /// native), or when `check_audible` and the listener is out of range.
    pub fn play(
        &mut self,
        lib: &AudioLibrary,
        cue: &str,
        params: PlayParams,
        listener: Vec3,
    ) -> Option<InstanceId> {
        let def = lib.cue(cue)?.clone();
        if def.max_concurrent_play_count > 0 {
            let playing = self
                .instances
                .values()
                .filter(|i| !i.stopped && Arc::ptr_eq(&i.cue, &def))
                .count();
            if playing >= usize::try_from(def.max_concurrent_play_count).unwrap_or(usize::MAX) {
                return None;
            }
        }
        if params.check_audible
            && params.allow_spatialization
            && params.location.distance(listener) > def.max_audible_distance()
        {
            return None;
        }
        let id = InstanceId(self.next_instance);
        self.next_instance += 1;
        self.instances.insert(id, CueInstance::new(def, params));
        Some(id)
    }

    /// Stops an instance at once.
    pub fn stop(&mut self, id: InstanceId) {
        if let Some(i) = self.instances.get_mut(&id) {
            i.stop();
        }
        self.subtitles.kill(id);
    }

    /// `FadeOut(duration, volume)` on an instance (it stops at the end).
    pub fn fade_out(&mut self, id: InstanceId, duration: f32, volume: f32) {
        if let Some(i) = self.instances.get_mut(&id) {
            i.fade_out(duration, volume);
            if i.stopped {
                self.subtitles.kill(id);
            }
        }
    }

    /// `FadeIn(duration, volume)` on a component (CONFIRMED, native):
    /// while a fade-out runs it reverses from the current level (same
    /// instance, no restart); otherwise it sets the fade and calls `Play`,
    /// and `Play` on a component that is still playing restarts it with the
    /// fade values reset, so the fade-in is lost (full volume at once). A
    /// stopped component (or `None`) starts anew with the fade.
    pub fn fade_in(
        &mut self,
        lib: &AudioLibrary,
        existing: Option<InstanceId>,
        cue: &str,
        mut params: PlayParams,
        (duration, volume): (f32, f32),
        listener: Vec3,
    ) -> Option<InstanceId> {
        let mut was_playing = false;
        if let Some(id) = existing
            && let Some(i) = self.instances.get_mut(&id)
            && !i.stopped
        {
            if i.fade_in_playing(duration, volume) {
                return Some(id);
            }
            was_playing = true;
        }
        if let Some(id) = existing {
            self.stop(id);
        }
        params.fade_in = (!was_playing).then_some((duration, volume));
        self.play(lib, cue, params, listener)
    }

    /// `AdjustVolume(duration, volume)`.
    pub fn adjust_volume(&mut self, id: InstanceId, duration: f32, volume: f32) {
        if let Some(i) = self.instances.get_mut(&id) {
            i.adjust_volume(duration, volume);
        }
    }

    /// Moves an instance's source.
    pub fn set_location(&mut self, id: InstanceId, location: Vec3) {
        if let Some(i) = self.instances.get_mut(&id)
            && location.is_finite()
        {
            i.location = location;
        }
    }

    /// `SetFloatParameter` (read by continuous modulators).
    pub fn set_float_parameter(&mut self, id: InstanceId, name: &str, value: f32) {
        if let Some(i) = self.instances.get_mut(&id)
            && value.is_finite()
        {
            i.float_params.insert(name.to_owned(), value);
        }
    }

    /// `SetWaveParameter` (read by `SoundNodeWaveParam`).
    pub fn set_wave_parameter(&mut self, id: InstanceId, name: &str, wave: &str) {
        if let Some(i) = self.instances.get_mut(&id) {
            i.wave_params.insert(name.to_owned(), wave.to_owned());
        }
    }

    /// Activates a sound mode (`SetSoundMode`).
    pub fn set_sound_mode(&mut self, lib: &AudioLibrary, mode: Option<&str>) {
        self.modes.set(lib, mode, self.real_time);
    }

    /// Stops every instance and clears the narrator and the ambient set.
    pub fn stop_all(&mut self) {
        let ids: Vec<InstanceId> = self.instances.keys().copied().collect();
        for id in ids {
            self.stop(id);
        }
        self.narrator = Narrator::default();
        self.ambient.clear();
        self.commanded.clear();
        self.followers.clear();
    }

    // ----------------------------------------------------------- ambient

    /// Replaces the ambient set with the actors of `map` and starts the
    /// auto-playing ones. Returns how many actors were loaded.
    pub fn load_ambient(&mut self, lib: &AudioLibrary, map: &str, listener: Vec3) -> usize {
        for a in std::mem::take(&mut self.ambient) {
            if let Some(id) = a.instance {
                self.stop(id);
            }
        }
        self.ambient = lib
            .ambient_for_map(map)
            .iter()
            .map(|def| AmbientRuntime {
                def: def.clone(),
                instance: None,
            })
            .collect();
        for i in 0..self.ambient.len() {
            if self.ambient[i].def.auto_play {
                self.start_ambient(lib, i, false, listener);
            }
        }
        self.ambient.len()
    }

    /// Ambient actors currently playing (names).
    #[must_use]
    pub fn playing_ambient(&self) -> Vec<&str> {
        self.ambient
            .iter()
            .filter(|a| a.instance.is_some_and(|id| self.is_playing(id)))
            .map(|a| a.def.name.as_str())
            .collect()
    }

    fn start_ambient(&mut self, lib: &AudioLibrary, index: usize, toggled: bool, listener: Vec3) {
        let Some(a) = self.ambient.get(index) else {
            return;
        };
        let Some(cue) = a.def.cue.clone() else {
            return;
        };
        let mut params = PlayParams::at(a.def.source_location(listener));
        params.remain_active_if_dropped = true;
        params.volume_multiplier = a.def.volume_multiplier;
        params.pitch_multiplier = a.def.pitch_multiplier;
        let existing = a.instance;
        let fade = (toggled && a.def.toggle.fade_on_toggle)
            .then_some((a.def.toggle.fade_in_duration, a.def.toggle.fade_in_volume));
        let id = match fade {
            Some(fade) => self.fade_in(lib, existing, &cue, params, fade, listener),
            None => {
                if let Some(id) = existing {
                    self.stop(id);
                }
                self.play(lib, &cue, params, listener)
            }
        };
        if let Some(a) = self.ambient.get_mut(index) {
            a.instance = id;
        }
    }

    /// Kismet `SeqAct_Toggle` on the ambient actor `name` (its object name
    /// or object path; toggleable classes only, the plain `AmbientSound`
    /// has no toggle handler; STRONG, engine script): "Turn On", or
    /// "Toggle" while not playing, starts it (`FadeIn` when
    /// `bFadeOnToggle`); otherwise it stops (`FadeOut` when
    /// `bFadeOnToggle`). Returns false for an unknown or non-toggleable
    /// actor.
    pub fn toggle_ambient(
        &mut self,
        lib: &AudioLibrary,
        name: &str,
        action: ToggleAction,
        listener: Vec3,
    ) -> bool {
        let Some(index) = self.ambient.iter().position(|a| {
            a.def.name.eq_ignore_ascii_case(name)
                || (!a.def.path.is_empty() && a.def.path.eq_ignore_ascii_case(name))
        }) else {
            return false;
        };
        if !self.ambient[index].def.kind.toggleable() {
            return false;
        }
        let playing = self.ambient[index]
            .instance
            .is_some_and(|id| self.is_playing(id));
        let start = matches!(action, ToggleAction::TurnOn)
            || matches!(action, ToggleAction::Toggle) && !playing;
        if start {
            self.start_ambient(lib, index, true, listener);
        } else if let Some(id) = self.ambient[index].instance {
            let t = self.ambient[index].def.toggle;
            if t.fade_on_toggle {
                self.fade_out(id, t.fade_out_duration, t.fade_out_volume);
            } else {
                self.stop(id);
            }
        }
        true
    }

    // ---------------------------------------------------------- narrator

    /// `narratorAudioComponent.Play()` of `cue` with `VolumeMultiplier`
    /// `volume`: 2-D (the narrator cues have no attenuation node; the
    /// component sits on the narrator manager actor), subtitle priority
    /// 10000. Restarts the component.
    fn narrator_sound(&mut self, lib: &AudioLibrary, cue: &str, volume: f32, listener: Vec3) {
        if let Some(id) = self.narrator.component.take() {
            self.stop(id);
        }
        let mut params = PlayParams::two_d();
        params.volume_multiplier = volume;
        params.subtitle_priority = SUBTITLE_PRIORITY_SCRIPTED;
        self.narrator.component = self.play(lib, cue, params, listener);
    }

    /// The cooked `Duration` of a cue (0 when unknown: `SetTimer(0)` then
    /// never fires).
    fn cue_duration(lib: &AudioLibrary, cue: &str) -> f32 {
        lib.cue(cue).and_then(|c| c.duration).unwrap_or(0.0)
    }

    fn narrator_add(
        &mut self,
        lib: &AudioLibrary,
        line: NarratorLine,
        remove_all_others: bool,
        fade_out_if_active: bool,
        listener: Vec3,
    ) {
        if remove_all_others {
            // The script's loop runs from `Length` down to 1: the first
            // pass reads past the end (an empty id) and index 0 is never
            // reached.
            let mut i = self.narrator.lines.len();
            while i > 0 {
                let id = self
                    .narrator
                    .lines
                    .get(i)
                    .map(|l| l.id.clone())
                    .unwrap_or_default();
                self.narrator_remove(&id, fade_out_if_active);
                i -= 1;
            }
        }
        self.narrator.lines.push(line);
        if self.narrator.lines.len() == 1
            && let Some(first) = self.narrator.lines.first().cloned()
        {
            self.feedback.push(AudioFeedback::NarratorStarted);
            self.narrator.pending_cue = Some(first.cue.clone());
            self.narrator_sound(lib, &first.cue, first.volume, listener);
            let d = Self::cue_duration(lib, &first.cue);
            self.narrator.set_timer(NarratorTimerKind::FinishedLine, d);
        }
    }

    /// `RemoveLine`: finds the first line with `id`; when it is the playing
    /// one and `stop_if_active`, the delayed start is cleared and the sound
    /// stops (the end timer keeps running); then every line equal to it
    /// leaves the queue (`RemoveItem`).
    fn narrator_remove(&mut self, id: &str, stop_if_active: bool) {
        let Some(line) = self.narrator.lines.iter().find(|l| l.id == id).cloned() else {
            return;
        };
        if stop_if_active && self.narrator.lines.first() == Some(&line) {
            self.narrator.clear_timer(NarratorTimerKind::DelayedStart);
            if let Some(c) = self.narrator.component.take() {
                self.stop(c);
            }
        }
        self.narrator.lines.retain(|l| *l != line);
    }

    /// The `Finishedline` timer fired.
    fn narrator_finished_line(&mut self) {
        self.narrator.clear_timer(NarratorTimerKind::FinishedLine);
        if let Some(first) = self.narrator.lines.first().cloned() {
            self.feedback.push(AudioFeedback::NarratorLineFinished {
                id: first.id.clone(),
            });
            self.narrator_remove(&first.id, true);
        }
        if let Some(next) = self.narrator.lines.first().cloned() {
            self.narrator.pending_cue = Some(next.cue.clone());
            self.narrator
                .set_timer(NarratorTimerKind::DelayedStart, next.delay);
        } else {
            self.feedback.push(AudioFeedback::NarratorFinished);
        }
    }

    /// The `DelayedStart` timer fired: the component plays the cue set when
    /// the previous line ended, at the volume of the line now first, and
    /// the end timer takes that line's cue duration. With the queue emptied
    /// in between, the script reads an empty line: volume 0 and no end
    /// timer (kept).
    fn narrator_delayed_start(&mut self, lib: &AudioLibrary, listener: Vec3) {
        self.narrator.clear_timer(NarratorTimerKind::DelayedStart);
        let first = self.narrator.lines.first().cloned();
        let volume = first.as_ref().map_or(0.0, |l| l.volume);
        if let Some(cue) = self.narrator.pending_cue.clone() {
            self.narrator_sound(lib, &cue, volume, listener);
        }
        let d = first.map_or(0.0, |l| Self::cue_duration(lib, &l.cue));
        self.narrator.set_timer(NarratorTimerKind::FinishedLine, d);
    }

    /// `UpdateTimers(dt)` of the narrator manager (CONFIRMED, native): every
    /// count grows first; then, in order, a rate-0 timer is removed unfired
    /// and a timer whose count exceeds its rate fires (and is removed). A
    /// timer set while firing starts at 0 and is not advanced this tick.
    fn narrator_tick(&mut self, lib: &AudioLibrary, dt: f32, listener: Vec3) {
        for t in &mut self.narrator.timers {
            t.count += dt;
        }
        let mut i = 0;
        // Each firing removes its timer and adds at most one other, so the
        // walk ends; the bound only guards against a logic error.
        let mut budget = 16;
        while let Some(t) = self.narrator.timers.get(i).copied() {
            if budget == 0 {
                break;
            }
            if t.rate == 0.0 {
                self.narrator.timers.remove(i);
                continue;
            }
            if t.rate < t.count {
                budget -= 1;
                self.narrator.timers.remove(i);
                match t.kind {
                    NarratorTimerKind::FinishedLine => self.narrator_finished_line(),
                    NarratorTimerKind::DelayedStart => {
                        self.narrator_delayed_start(lib, listener);
                    }
                }
                i = 0;
                continue;
            }
            i += 1;
        }
    }

    // ---------------------------------------------------------- commands

    /// Applies a command; unknown cues are reported as feedback.
    pub fn apply(&mut self, lib: &AudioLibrary, command: &AudioCommand, listener: Vec3) {
        match command {
            AudioCommand::PlaySound {
                cue,
                source,
                volume_multiplier,
                pitch_multiplier,
                fade_in_time,
                suppress_subtitles,
                suppress_spatialization,
                node,
            } => {
                if lib.cue(cue).is_none() {
                    self.feedback
                        .push(AudioFeedback::UnknownCue { cue: cue.clone() });
                    return;
                }
                let (location, spatial, follows_player) = match source {
                    SoundSource::TwoD => (listener, false, false),
                    SoundSource::Location { location } => {
                        (Vec3::from_array(*location), true, false)
                    }
                    SoundSource::Player => (self.player_location, true, true),
                };
                let mut params = PlayParams::at(location);
                params.allow_spatialization = spatial && !suppress_spatialization;
                params.volume_multiplier = *volume_multiplier;
                params.pitch_multiplier = *pitch_multiplier;
                params.subtitle_priority = SUBTITLE_PRIORITY_SCRIPTED;
                params.suppress_subtitles = *suppress_subtitles;
                params.fade_in = Some((*fade_in_time, 1.0));
                if let Some(id) = self.play(lib, cue, params, listener) {
                    self.commanded.insert(
                        id,
                        Commanded {
                            cue: cue.clone(),
                            node: *node,
                        },
                    );
                    if follows_player {
                        self.followers.insert(id);
                    }
                }
            }
            AudioCommand::StopSound {
                cue,
                fade_out_time,
                node,
            } => {
                let ids: Vec<InstanceId> = self
                    .commanded
                    .iter()
                    .filter(|(_, c)| match node {
                        Some(n) => c.node == Some(*n),
                        None => c.cue.eq_ignore_ascii_case(cue),
                    })
                    .map(|(id, _)| *id)
                    .collect();
                for id in ids {
                    if *fade_out_time > 0.0 {
                        self.fade_out(id, *fade_out_time, 0.0);
                    } else {
                        self.stop(id);
                    }
                }
            }
            AudioCommand::NarratorAddLine {
                id,
                cue,
                volume,
                delay,
                remove_all_others,
                fade_out_if_active,
                node,
            } => {
                if lib.cue(cue).is_none() {
                    self.feedback
                        .push(AudioFeedback::UnknownCue { cue: cue.clone() });
                }
                let line = NarratorLine {
                    id: id.clone(),
                    cue: cue.clone(),
                    volume: *volume,
                    delay: *delay,
                    node: *node,
                };
                self.narrator_add(lib, line, *remove_all_others, *fade_out_if_active, listener);
            }
            AudioCommand::NarratorPlay { cue, volume, .. } => {
                if lib.cue(cue).is_none() {
                    self.feedback
                        .push(AudioFeedback::UnknownCue { cue: cue.clone() });
                }
                self.narrator_sound(lib, cue, *volume, listener);
            }
            AudioCommand::NarratorStop { .. } => {
                if let Some(c) = self.narrator.component.take() {
                    self.stop(c);
                }
            }
            AudioCommand::NarratorRemoveLine {
                id, stop_if_active, ..
            } => self.narrator_remove(id, *stop_if_active),
            AudioCommand::SetSoundMode { mode } => self.set_sound_mode(lib, mode.as_deref()),
            AudioCommand::ToggleAmbient { actor, action } => {
                self.toggle_ambient(lib, actor, *action, listener);
            }
            AudioCommand::StopAll => self.stop_all(),
        }
    }

    // ------------------------------------------------------------ update

    /// Advances every instance by `dt` seconds with the listener at
    /// `listener` (UU) and returns the voices to play.
    pub fn update(&mut self, lib: &AudioLibrary, listener: Vec3, dt: f32) -> AudioFrame {
        let dt = if dt.is_finite() { dt.max(0.0) } else { 0.0 };
        self.real_time += f64::from(dt);
        self.modes.update_with(lib, self.real_time);
        let game_dt = if self.paused { 0.0 } else { dt };
        self.audio_time += f64::from(game_dt);
        self.narrator_tick(lib, game_dt, listener);

        // Sources that move: ambient splines and player-following sounds.
        for a in &self.ambient {
            if let Some(id) = a.instance
                && let Some(i) = self.instances.get_mut(&id)
                && a.def.spline_points.len() > 1
            {
                i.location = a.def.source_location(listener);
            }
        }
        for id in &self.followers {
            if let Some(i) = self.instances.get_mut(id) {
                i.location = self.player_location;
            }
        }

        // 1. Parse every instance (the engine's UpdateWaveInstances).
        struct Candidate {
            instance: InstanceId,
            key: WaveKey,
            priority: f32,
            gain: f32,
            pitch: f32,
            ui: bool,
        }
        let mut candidates: Vec<Candidate> = Vec::new();
        let ids: Vec<InstanceId> = self.instances.keys().copied().collect();
        for id in &ids {
            let Some(inst) = self.instances.get_mut(id) else {
                continue;
            };
            if inst.stopped {
                continue;
            }
            let class = inst
                .cue
                .sound_class
                .as_deref()
                .and_then(|c| self.modes.class(c))
                .copied();
            let ui = class.is_some_and(|c| c.is_ui);
            let always_play = class.is_some_and(|c| c.always_play);
            let step = if self.paused && !ui { 0.0 } else { dt };
            // Safety stop: a finite cue playing longer than its length at
            // the lowest pitch (CONFIRMED, native).
            if let Some(d) = inst.cue.duration
                && d < INDEFINITE_DURATION
                && d / MIN_PITCH < inst.playback_time
            {
                inst.stop();
                continue;
            }
            inst.playback_time += step;
            let t = inst.playback_time;
            let mut comp = Comp {
                volume: 1.0,
                pitch: 1.0,
                hf: 1.0,
                spatial: false,
                notify_on_loop: false,
                hook: None,
                finished: true,
            };
            let mut out = Vec::new();
            let mut created = Vec::new();
            if inst.fades.out_stop == -1.0 || t <= inst.fades.out_stop {
                let cue = inst.cue.clone();
                if let Some(first) = cue.first {
                    let mut p = Parse {
                        lib,
                        cue: &cue,
                        rng: &mut self.rng,
                        shared: &mut self.shared,
                        listener,
                        location: inst.location,
                        time: t,
                        allow_spatialization: inst.params.allow_spatialization,
                        always_play,
                        float_params: &inst.float_params,
                        wave_params: &inst.wave_params,
                        nodes: &mut inst.nodes,
                        waves: &mut inst.waves,
                        path: Vec::new(),
                        out: Vec::new(),
                        created: Vec::new(),
                        visits: 0,
                    };
                    p.node(&mut comp, first, None, ChildSlot::Index(0));
                    out = p.out;
                    created = p.created;
                }
            }
            if comp.finished {
                inst.stop();
                continue;
            }
            let comp_volume = inst.component_volume();
            let comp_pitch = inst.params.pitch_multiplier * inst.cue.pitch_multiplier;
            let final_pitch = |w: &WaveInstance| {
                let p = w.pitch * comp_pitch * class.map_or(1.0, |c| c.pitch);
                if p.is_finite() {
                    p.clamp(MIN_PITCH, MAX_PITCH)
                } else {
                    1.0
                }
            };
            // Subtitles are queued when a wave instance is created (the
            // engine's `HandleStart`), whether or not it gets a voice.
            if inst.params.subtitle_priority != 0.0 && !inst.params.suppress_subtitles {
                for key in &created {
                    let Some(w) = inst.waves.get(key) else {
                        continue;
                    };
                    let Some(track) = lib.subtitles.get(&w.wave) else {
                        continue;
                    };
                    // The wave's `Duration`, not scaled by pitch (CONFIRMED,
                    // native).
                    let duration = lib.wave(&w.wave).map_or(0.0, |i| i.duration);
                    self.subtitles.queue(
                        *id,
                        inst.params.subtitle_priority,
                        duration,
                        &track.lines,
                        self.audio_time,
                    );
                }
            }
            for key in out {
                let Some(w) = inst.waves.get(&key) else {
                    continue;
                };
                // `FSoundSource::SetStereoBleed` tests for exactly two
                // channels (CONFIRMED, native).
                let stereo = lib.wave(&w.wave).is_some_and(|i| i.channels == 2);
                let mut gain = w.volume * comp_volume;
                if let Some(c) = class {
                    gain *= c.volume;
                    if stereo && c.stereo_bleed != 0.0 {
                        gain *= STEREO_BLEED_GAIN;
                    }
                }
                let gain = clamp01_or(gain, 0.0) * self.master_volume;
                let pitch = final_pitch(w);
                candidates.push(Candidate {
                    instance: *id,
                    priority: w.volume + if always_play { 1.0 } else { 0.0 },
                    key,
                    gain,
                    pitch,
                    ui,
                });
            }
        }

        // 2. Voice selection: the MAX_CHANNELS highest priorities.
        candidates.sort_by(|a, b| {
            b.priority
                .partial_cmp(&a.priority)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then(a.instance.cmp(&b.instance))
                .then(a.key.cmp(&b.key))
        });
        let dropped = if candidates.len() > MAX_CHANNELS {
            candidates.split_off(MAX_CHANNELS)
        } else {
            Vec::new()
        };
        let selected: BTreeSet<(InstanceId, WaveKey)> = candidates
            .iter()
            .map(|c| (c.instance, c.key.clone()))
            .collect();

        // A playing wave instance that is no longer among the voices (it
        // lost its channel, fell to the priority threshold, or its node
        // stopped parsing it) has its source stopped, and stopping a source
        // finishes the wave instance and notifies its hook node
        // (`FSoundSource::Stop`; CONFIRMED, native). Whatever plays it again
        // later starts a new source from the beginning.
        let mut stolen: Vec<(InstanceId, WaveKey)> = Vec::new();
        for (id, inst) in &mut self.instances {
            for (key, w) in &mut inst.waves {
                if w.voice.is_some() && !selected.contains(&(*id, key.clone())) {
                    w.voice = None;
                    w.position = 0.0;
                    if !w.finished && !w.notified {
                        stolen.push((*id, key.clone()));
                    }
                }
            }
        }
        for (id, key) in stolen {
            self.notify_finished(id, &key, false);
        }
        // Wave instances that did not get a channel finish, unless they
        // loop forever at the source or their component remains active if
        // dropped (`UAudioDevice::StopSources`; CONFIRMED, native).
        for d in &dropped {
            if let Some(inst) = self.instances.get_mut(&d.instance) {
                let keep = inst.params.remain_active_if_dropped;
                if let Some(w) = inst.waves.get_mut(&d.key) {
                    w.finished = !(keep || w.mode == LoopMode::Forever);
                }
            }
        }

        // 3. Start / advance the selected voices; finished waves notify.
        let mut frame = AudioFrame::default();
        let mut finished: Vec<(InstanceId, WaveKey, bool)> = Vec::new();
        for c in &candidates {
            let voice;
            {
                let Some(inst) = self.instances.get_mut(&c.instance) else {
                    continue;
                };
                let location = inst.location;
                let Some(w) = inst.waves.get_mut(&c.key) else {
                    continue;
                };
                let info = lib.wave(&w.wave);
                let duration = info.map_or(0.0, |i| i.duration);
                let mono = info.is_some_and(|i| i.channels == 1);
                let step = if self.paused && !c.ui { 0.0 } else { dt };
                if w.voice.is_none() {
                    w.voice = Some(self.next_voice);
                    self.next_voice += 1;
                } else {
                    w.position += step * c.pitch;
                    if duration > 0.0 && w.position >= duration {
                        match w.mode {
                            LoopMode::Forever => {
                                w.position %= duration;
                            }
                            LoopMode::WithNotification => {
                                w.position %= duration;
                                finished.push((c.instance, c.key.clone(), true));
                            }
                            LoopMode::Never => {
                                finished.push((c.instance, c.key.clone(), false));
                            }
                        }
                    } else if duration <= 0.0 {
                        finished.push((c.instance, c.key.clone(), false));
                    }
                }
                voice = Voice {
                    id: w.voice.unwrap_or(0),
                    instance: c.instance,
                    wave: w.wave.clone(),
                    gain: c.gain,
                    pitch: c.pitch,
                    hf_gain: w.hf_gain,
                    spatial: w.spatial && mono,
                    location,
                    looping: w.mode != LoopMode::Never,
                    ui: c.ui,
                    position: w.position,
                };
            }
            frame.voices.push(voice);
        }
        for (id, key, seamless) in finished {
            self.notify_finished(id, &key, seamless);
        }

        // 4. Drop stopped instances.
        let stopped: Vec<InstanceId> = self
            .instances
            .iter()
            .filter(|(_, i)| i.stopped)
            .map(|(id, _)| *id)
            .collect();
        for id in stopped {
            self.instances.remove(&id);
            self.commanded.remove(&id);
            self.followers.remove(&id);
            self.subtitles.kill(id);
            if self.narrator.component == Some(id) {
                self.narrator.component = None;
            }
        }
        frame.subtitle = self.subtitles.current(self.audio_time);
        frame.feedback = std::mem::take(&mut self.feedback);
        frame
    }

    /// A wave instance reached its end (`FWaveInstance::NotifyFinished` and
    /// the hook node's `NotifyWaveInstanceFinished`). CONFIRMED (native),
    /// with one simplification: a seamless loop that restarts keeps its
    /// voice instead of re-queuing the buffer.
    fn notify_finished(&mut self, id: InstanceId, key: &WaveKey, seamless: bool) {
        let mut stop_component = false;
        {
            let Some(inst) = self.instances.get_mut(&id) else {
                return;
            };
            let Some(w) = inst.waves.get_mut(key) else {
                return;
            };
            if w.notified && !seamless {
                return;
            }
            w.finished = true;
            w.notified = true;
            let hook = w.hook;
            let cue = inst.cue.clone();
            let Some(h) = hook else {
                return;
            };
            match cue.nodes.get(h).map(|n| &n.kind) {
                Some(NodeKind::Looping { indefinitely, .. }) => {
                    let (remaining, count) = match inst.nodes.get(h) {
                        Some(NodeState::Looping {
                            remaining,
                            finished_count,
                        }) => (*remaining, *finished_count),
                        _ => (0, 0),
                    };
                    if *indefinitely || remaining > 0 {
                        let all_done = inst
                            .waves
                            .values()
                            .filter(|w| w.ancestors.contains(&h) && w.started)
                            .all(|w| w.finished);
                        if all_done {
                            let remaining = if *indefinitely {
                                remaining
                            } else {
                                remaining - 1
                            };
                            inst.nodes[h] = NodeState::Looping {
                                remaining,
                                finished_count: 0,
                            };
                            let subtree = inst.active_subtree(h);
                            for n in subtree.iter().skip(1) {
                                if let Some(s) = inst.nodes.get_mut(*n) {
                                    *s = NodeState::Fresh;
                                }
                            }
                            // `ResetWaveInstances`: the subtree's wave
                            // instances become unstarted and unfinished; a
                            // seamless loop keeps its source playing, the
                            // others start a new source when parsed again.
                            for (k, w) in &mut inst.waves {
                                if !w.ancestors.contains(&h) {
                                    continue;
                                }
                                w.finished = false;
                                w.notified = false;
                                w.started = false;
                                if !(seamless && k == key) {
                                    w.voice = None;
                                    w.position = 0.0;
                                }
                            }
                        } else {
                            inst.nodes[h] = NodeState::Looping {
                                remaining,
                                finished_count: count + 1,
                            };
                        }
                    }
                }
                Some(NodeKind::Concatenator { .. }) => {
                    if let Some(NodeState::Concatenator { index }) = inst.nodes.get_mut(h) {
                        *index += 1;
                    }
                }
                Some(NodeKind::Ambient(a)) => match a.non_loop {
                    None => {
                        w.finished = false;
                    }
                    Some(nl) => {
                        let time = inst.playback_time;
                        let volume = self.rng.pick(a.ranges.volume_min, a.ranges.volume_max);
                        let pitch = self.rng.pick(a.ranges.pitch_min, a.ranges.pitch_max);
                        let next = self.rng.pick(nl.delay_min, nl.delay_max) + time;
                        let slot = if a.slots.is_empty() {
                            0
                        } else {
                            let sum: f32 = a.slots.iter().map(|s| s.weight).sum();
                            let choice = sum * self.rng.frand();
                            let mut acc = 0.0;
                            let mut pick = a.slots.len() - 1;
                            for (i, s) in a.slots.iter().enumerate() {
                                acc += s.weight;
                                if choice <= acc {
                                    pick = i;
                                    break;
                                }
                            }
                            pick
                        };
                        inst.nodes[h] = NodeState::AmbientNonLoop {
                            volume,
                            pitch,
                            next,
                            slot,
                        };
                        // The instance stays (started, unfinished) and
                        // replays from the start when parsed again.
                        if let Some(w) = inst.waves.get_mut(key) {
                            w.finished = false;
                            w.voice = None;
                            w.position = 0.0;
                        }
                        stop_component = nl.toggle;
                    }
                },
                _ => {}
            }
        }
        if stop_component {
            self.stop(id);
        }
    }
}

// ---------------------------------------------------------------------------
// Gameplay cues (class defaults) and footsteps
// ---------------------------------------------------------------------------

/// Cues the player's abilities and the world objects play, from the class
/// default objects (CONFIRMED cdo; `docs/reverse-engineering/data/defaults/`
/// and `asamu-inspect defaults`), and the moments they play (STRONG, script
/// read locally; see AUDIO.md "Audio runtime").
pub mod gameplay_cues {
    /// `ASAMUPawn.PlayerJumpGruntSound`.
    pub const PLAYER_JUMP_GRUNT: &str = "MiscSounds.Grunts.TheHand_Jump_Grunt_Cue";
    /// `ASAMUPawn.PlayerLandGruntSound`.
    pub const PLAYER_LAND_GRUNT: &str = "MiscSounds.Grunts.TheHand_Land_Grunt_Cue";
    /// `ASAMUPawn.SprintingClothesSound`.
    pub const SPRINTING_CLOTHES: &str = "MiscSounds.Sprint.Sprinting_Rustle_Cue";
    /// `ASAMUPawn.SprintingFootstepsThud`.
    pub const SPRINTING_FOOTSTEPS_THUD: &str = "FootSteps.Rock.Footsteps_Rock_Srpinting_Cue";
    /// `ASAMUPawn.RespawnSound` (played when the player dies).
    pub const RESPAWN: &str = "MiscSounds.Death_Blackout_Cue";
    /// `ASAMUPawn.FallingWindSound` (component template `WindSound`).
    pub const FALLING_WIND: &str = "MiscSounds.Player_Falling_Wind_Cue";
    /// Float parameter of the falling wind: `|V.x + V.y + V.z|`.
    pub const FALLING_WIND_PARAM: &str = "FallingWindParam";
    /// `ASAMUPawn.checkFallingSoundDelay`: seconds between wind parameter
    /// updates.
    pub const FALLING_WIND_UPDATE_INTERVAL: f32 = 0.1;

    /// `GrappleGun.GrapplingStartSound`.
    pub const GRAPPLE_START: &str = "GrapplingGun.Beam.GrapplingGun_Beam_Start_Cue";
    /// `GrappleGun.GrapplingStopSound`.
    pub const GRAPPLE_STOP: &str = "GrapplingGun.Beam.GrapplingGun_Beam_Stop_Cue";
    /// `GrappleGun.GrapplingFailSound`.
    pub const GRAPPLE_FAIL: &str = "GrapplingGun.Fail.GrapplingGun_Fail_Cue";
    /// `GrappleGun.GrappleRechargedSound` (a charged crystal was grappled).
    pub const GRAPPLE_RECHARGED: &str = "GrapplingGun.Recharged.GrapplingGun_Recharged_Cue";
    /// `GrappleGun.GrappleDecalSound` (at the hit location).
    pub const GRAPPLE_DECAL: &str = "GrapplingGun.Decal.GrapplingGun_Decal_Cue";
    /// `GrappleGun.GrapplingSound` (component template `ZethSound`): the
    /// beam loop, faded in over 0.5 s on attach and out over 0.2 s on
    /// release (script literals).
    pub const GRAPPLE_BEAM: &str = "GrapplingGun.Beam.GrapplingGun_Beam_Cue";
    /// Float parameter of the beam: `fMaxDistance − distance to the anchor`.
    pub const GRAPPLE_BEAM_PARAM: &str = "GrapplingBeamParam";
    /// Beam fade-in seconds (script literal).
    pub const GRAPPLE_BEAM_FADE_IN: f32 = 0.5;
    /// Beam fade-out seconds (script literal).
    pub const GRAPPLE_BEAM_FADE_OUT: f32 = 0.2;

    /// `ASAMUPowerJump.PowerChargeSound` (template `PowerJumpSound`).
    pub const POWER_JUMP_CHARGE: &str = "PowerGlove.powerJump.PowerJump_Charge_Cue";
    /// `ASAMUPowerJump.PowerChargeStaticSound` (template `PowerJumpStatic`).
    pub const POWER_JUMP_STATIC: &str = "PowerGlove.powerJump.PowerJump_Static_Cue";
    /// `ASAMUPowerJump.PowerJumpReleaseSound`.
    pub const POWER_JUMP_RELEASE: &str = "PowerGlove.PowerJump_Jump_Cue";
    /// `ASAMUPowerJump.PowerLeapSound`.
    pub const POWER_LEAP: &str = "PowerGlove.PowerLeap.PowerLeap_Jump_Cue";
    /// `ASAMUPowerJump.PowerJumpLightSound` (charge complete).
    pub const POWER_JUMP_LIGHT: &str = "PowerGlove.powerJump.PowerJump_Light_Cue";
    /// Charge loop fade-in when charging starts (script literal).
    pub const POWER_CHARGE_FADE_IN: f32 = 0.1;
    /// Charge fade-out and static fade-in when charged (script literals).
    pub const POWER_CHARGED_CHARGE_FADE_OUT: f32 = 0.2;
    /// Static fade-in when charged (script literal).
    pub const POWER_CHARGED_STATIC_FADE_IN: f32 = 0.1;
    /// Static fade-out after the jump (script literal).
    pub const POWER_FIRED_STATIC_FADE_OUT: f32 = 0.6;
    /// Charge and static fade-out on cancel (script literal).
    pub const POWER_CANCEL_FADE_OUT: f32 = 0.3;

    /// `ASAMURocketBoots.BoostActiveSound` (template, played as a component).
    pub const BOOST_ACTIVE: &str = "rocketBoots.bLast.RocketBoots_Blast_Cue";
    /// `ASAMURocketBoots.ChargeBoostSound`.
    pub const BOOST_CHARGE: &str = "rocketBoots.Charge.RocketBoots_Charge_Cue";
    /// `ASAMURocketBoots.ChargeInterruptSound` (landing during a boost).
    pub const BOOST_INTERRUPT: &str = "rocketBoots.Stopped.RocketBoots_Stopped_Cue";
    /// `ASAMURocketBoots.BoostExhaustedSound`.
    pub const BOOST_EXHAUSTED: &str = "rocketBoots.Exhausted.RocketBoots_Exhausted_Cue";
    /// Blast fade-out when a landing cancels the boost (script literal).
    pub const BOOST_CANCEL_FADE_OUT: f32 = 0.2;

    /// `ASAMURechargeCrystal.CrystalDrainedSound` (at the crystal).
    pub const CRYSTAL_DRAINED: &str = "MiscSounds.Crystal_Drained_Cue";
    /// `ASAMUGlowFlower.GlowFlowerGlowSound` (at the flower).
    pub const GLOW_FLOWER_GLOW: &str = "Chasms_Sounds.Chasms_GlowFlowers_Glow_Cue";

    /// Sound mode set when the player dies (`SetSoundMode('ASAMU_Death')`).
    pub const DEATH_SOUND_MODE: &str = "ASAMU_Death";
    /// Sound mode restored after the death fade (`ASAMU_Default`).
    pub const DEFAULT_SOUND_MODE: &str = "ASAMU_Default";
    /// Seconds from death to the mode reset: `playerDiedFadeDownTime` (0.3,
    /// cdo) plus the script's 0.6 s timer.
    pub const DEATH_MODE_RESET_DELAY: f32 = 0.3 + 0.6;

    /// `ASAMUSoundGroup.FootstepSounds` (`MaterialType`, cue).
    pub const FOOTSTEP_SOUNDS: &[(&str, &str)] = &[
        ("ASAMU_Rock", "FootSteps.Rock.Footsteps_Rock_Cue"),
        ("ASAMU_Ice", "FootSteps.Ice.Footsteps_Ice_Cue"),
        ("ASAMU_Grass", "FootSteps.Grass.Footsteps_Grass"),
        ("ASAMU_Wood", "FootSteps.Wood.Footsteps_Wood_Cue"),
        ("ASAMU_Snow", "FootSteps.Snow.Footsteps_Snow_Cue"),
        (
            "ASAMU_Workshop",
            "FootSteps.Workshop.Footsteps_Workshop_Wood_Cue",
        ),
        (
            "ASAMU_Carpet",
            "FootSteps.Workshop.Footsteps_Workshop_Carpet_Cue",
        ),
        ("ASAMU_Gold", "FootSteps.Gold.Footsteps_Gold_Cue"),
        (
            "Metal",
            "A_Character_Footsteps.FootSteps.A_Character_Footstep_MetalCue",
        ),
        (
            "Snow",
            "A_Character_Footsteps.FootSteps.A_Character_Footstep_SnowCue",
        ),
        (
            "Wood",
            "A_Character_Footsteps.FootSteps.A_Character_Footstep_WoodCue",
        ),
    ];

    /// `ASAMUSoundGroup.JumpingSounds`.
    pub const JUMPING_SOUNDS: &[(&str, &str)] = &[
        ("ASAMU_Rock", "Jump.Rock.Jump_Rock_Cue"),
        ("ASAMU_Ice", "Jump.Ice.Jump_Ice_Cue"),
        ("ASAMU_Grass", "Jump.Grass.Jump_Grass_Cue"),
        ("ASAMU_Wood", "Jump.Wood.Jump_Wood_Cue"),
        ("ASAMU_Snow", "Jump.Snow.Jump_Snow_Cue"),
        ("ASAMU_Gold", "FootSteps.Gold.Footsteps_Gold_Cue"),
        (
            "GlassBroken",
            "A_Character_Footsteps.FootSteps.A_Character_Footstep_GlassBrokenJumpCue",
        ),
        (
            "Grass",
            "A_Character_Footsteps.FootSteps.A_Character_Footstep_GrassJumpCue",
        ),
        (
            "Metal",
            "A_Character_Footsteps.FootSteps.A_Character_Footstep_MetalJumpCue",
        ),
        (
            "Mud",
            "A_Character_Footsteps.FootSteps.A_Character_Footstep_MudJumpCue",
        ),
        (
            "Metal",
            "A_Character_Footsteps.FootSteps.A_Character_Footstep_MetalJumpCue",
        ),
        (
            "Snow",
            "A_Character_Footsteps.FootSteps.A_Character_Footstep_SnowJumpCue",
        ),
        (
            "Tile",
            "A_Character_Footsteps.FootSteps.A_Character_Footstep_TileJumpCue",
        ),
        (
            "Water",
            "A_Character_Footsteps.FootSteps.A_Character_Footstep_WaterDeepJumpCue",
        ),
        (
            "ShallowWater",
            "A_Character_Footsteps.FootSteps.A_Character_Footstep_WaterShallowJumpCue",
        ),
        (
            "Wood",
            "A_Character_Footsteps.FootSteps.A_Character_Footstep_WoodJumpCue",
        ),
    ];

    /// `ASAMUSoundGroup.LandingSounds`.
    pub const LANDING_SOUNDS: &[(&str, &str)] = &[
        ("ASAMU_Rock", "Land.Rock.Land_Rock_Cue"),
        ("ASAMU_Ice", "Land.Ice.Land_Ice_Cue"),
        ("ASAMU_Grass", "Land.Grass.Land_Grass_Cue"),
        ("ASAMU_Wood", "Land.Wood.Land_Wood_Cue"),
        ("ASAMU_Snow", "Land.Snow.Land_Snow_Cue"),
        ("ASAMU_Gold", "FootSteps.Gold.Footsteps_Gold_Cue"),
        (
            "ASAMU_Workshop",
            "FootSteps.Workshop.Footsteps_Workshop_Wood_Cue",
        ),
        (
            "Grass",
            "A_Character_Footsteps.FootSteps.A_Character_Footstep_GrassLandCue",
        ),
        (
            "Metal",
            "A_Character_Footsteps.FootSteps.A_Character_Footstep_MetalLandCue",
        ),
        (
            "Mud",
            "A_Character_Footsteps.FootSteps.A_Character_Footstep_MudLandCue",
        ),
        (
            "Metal",
            "A_Character_Footsteps.FootSteps.A_Character_Footstep_MetalLandCue",
        ),
        (
            "Snow",
            "A_Character_Footsteps.FootSteps.A_Character_Footstep_SnowLandCue",
        ),
        (
            "Tile",
            "A_Character_Footsteps.FootSteps.A_Character_Footstep_TileLandCue",
        ),
        (
            "Water",
            "A_Character_Footsteps.FootSteps.A_Character_Footstep_WaterDeepLandCue",
        ),
        (
            "ShallowWater",
            "A_Character_Footsteps.FootSteps.A_Character_Footstep_WaterShallowLandCue",
        ),
        (
            "Wood",
            "A_Character_Footsteps.FootSteps.A_Character_Footstep_WoodLandCue",
        ),
    ];

    /// `ASAMUSoundGroup.FallingLandSounds` (hard landings).
    pub const FALLING_LAND_SOUNDS: &[(&str, &str)] = &[
        ("ASAMU_Rock", "Land.HardLanding.HardLanding_Rock_Grass_Cue"),
        ("ASAMU_Ice", "Land.HardLanding.HardLanding_Rock_Grass_Cue"),
        ("ASAMU_Grass", "Land.HardLanding.HardLanding_Rock_Grass_Cue"),
        ("ASAMU_Wood", "Land.HardLanding.HardLanding_Rock_Grass_Cue"),
        ("ASAMU_Gold", "FootSteps.Gold.Footsteps_Gold_Cue"),
    ];
}

/// The sound group's material lookup (`ASAMUSoundGroup.GetFootstepSound`,
/// `GetJumpSound`, `GetLandSound`; STRONG, script): a known material
/// selects its entry and remembers its index; an unknown or empty material
/// reuses the last remembered index (0 at start: rock). Landing and hard
/// landing share one remembered index, so an index valid only in the longer
/// table gives no hard-landing sound (kept). The converted collision has
/// no physical materials yet, so the runtime passes `None`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MaterialSounds {
    last_footstep: usize,
    last_jump: usize,
    last_land: usize,
}

impl MaterialSounds {
    fn pick(
        table: &[(&str, &'static str)],
        last: &mut usize,
        material: Option<&str>,
    ) -> Option<&'static str> {
        let found = material
            .filter(|m| !m.is_empty())
            .and_then(|m| table.iter().position(|(t, _)| t.eq_ignore_ascii_case(m)));
        if let Some(i) = found {
            *last = i;
        }
        table.get(*last).map(|(_, c)| *c)
    }

    /// Footstep cue.
    pub fn footstep(&mut self, material: Option<&str>) -> Option<&'static str> {
        Self::pick(
            gameplay_cues::FOOTSTEP_SOUNDS,
            &mut self.last_footstep,
            material,
        )
    }

    /// Jump cue.
    pub fn jump(&mut self, material: Option<&str>) -> Option<&'static str> {
        Self::pick(gameplay_cues::JUMPING_SOUNDS, &mut self.last_jump, material)
    }

    /// Landing cue (`hard`: the hard-landing table).
    pub fn land(&mut self, material: Option<&str>, hard: bool) -> Option<&'static str> {
        let table = if hard {
            gameplay_cues::FALLING_LAND_SOUNDS
        } else {
            gameplay_cues::LANDING_SOUNDS
        };
        Self::pick(table, &mut self.last_land, material)
    }
}

/// A footstep is due when the walk bob phase crosses a step boundary:
/// `trunc(π/2 + 9·bob/π)` changes between the previous and the new
/// `BobTime`, while walking faster than 10 UU/s (`|V|² > 100`) in first
/// person (`ASAMUPawn` bob update; STRONG, script).
#[must_use]
pub fn footstep_due(old_bob_time: f32, new_bob_time: f32, walking: bool, speed_sq: f32) -> bool {
    use std::f32::consts::PI;
    if !walking || speed_sq <= 100.0 {
        return false;
    }
    let step = |b: f32| (0.5 * PI + 9.0 * b / PI) as i64;
    step(old_bob_time) != step(new_bob_time)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn wave_info(path: &str, duration: f32, channels: u16) -> WaveInfo {
        WaveInfo {
            path: path.to_owned(),
            file: Some(format!("waves/{path}.ogg")),
            duration,
            channels,
            volume: 1.0,
            pitch: 1.0,
        }
    }

    fn wave_node(path: &str) -> Value {
        json!({"path": path, "class": "SoundNodeWave", "kind": "Wave", "children": [],
               "params": {}, "wave": {"volume": 1.0, "pitch": 1.0}})
    }

    fn node(path: &str, kind: &str, children: &[Option<&str>], params: Value) -> Value {
        json!({"path": path, "class": format!("SoundNode{kind}"), "kind": kind,
               "children": children, "params": params})
    }

    fn cue(path: &str, first: &str, nodes: Vec<Value>) -> CueDef {
        CueDef::from_json(
            path,
            &json!({"sound_class": null, "first_node": first, "volume_multiplier": 1.0,
                    "pitch_multiplier": 1.0, "duration": 1.0, "max_concurrent_play_count": 16,
                    "nodes": nodes}),
        )
    }

    fn lib_with(cues: Vec<CueDef>, waves: &[(&str, f32, u16)]) -> AudioLibrary {
        let mut lib = AudioLibrary::default();
        for c in cues {
            lib.add_cue(c);
        }
        for (p, d, ch) in waves {
            lib.add_wave(wave_info(p, *d, *ch));
        }
        lib
    }

    #[test]
    fn rng_matches_the_engine_lcg() {
        let mut r = UeRand::new(0);
        // First state: 0 * M + I.
        let f = r.frand();
        assert_eq!(r.seed(), UeRand::INCREMENT);
        let expected = f32::from_bits((UeRand::INCREMENT & 0x007F_FFFF) | 0x3F80_0000) - 1.0;
        assert_eq!(f, expected);
        for _ in 0..1000 {
            let v = r.frand();
            assert!((0.0..1.0).contains(&v));
        }
        // pick() stays inside the range in either orientation.
        let mut r = UeRand::new(42);
        for _ in 0..100 {
            let v = r.pick(0.9, 1.1);
            assert!((0.9..=1.1).contains(&v));
            let w = r.pick(1.1, 0.9);
            assert!((0.9..=1.1).contains(&w));
        }
    }

    #[test]
    fn attenuation_curves_match_the_engine_formulas() {
        use DistanceModel::*;
        for m in [
            Linear,
            Logarithmic,
            Inverse,
            LogReverse,
            NaturalSound,
            Other,
        ] {
            assert_eq!(attenuation_eval(m, 100.0, 200.0, 1000.0, -60.0), 1.0);
            assert_eq!(attenuation_eval(m, 1000.0, 200.0, 1000.0, -60.0), 0.0);
            assert_eq!(attenuation_eval(m, 5000.0, 200.0, 1000.0, -60.0), 0.0);
            assert_eq!(attenuation_eval(m, f32::NAN, 200.0, 1000.0, -60.0), 0.0);
        }
        // Linear midpoint.
        assert!((attenuation_eval(Linear, 600.0, 200.0, 1000.0, -60.0) - 0.5).abs() < 1e-6);
        // Logarithmic: ln(d/max)/ln(min/max).
        let v = attenuation_eval(Logarithmic, 500.0, 100.0, 1000.0, -60.0);
        let e = (500.0_f32 / 1000.0).ln() / (100.0_f32 / 1000.0).ln();
        assert!((v - e).abs() < 1e-6, "{v} vs {e}");
        // Logarithmic with min 0: -0.25 ln(d/max).
        let v = attenuation_eval(Logarithmic, 500.0, 0.0, 1000.0, -60.0);
        assert!((v - (-0.25 * 0.5_f32.ln())).abs() < 1e-6);
        // Inverse: 0.02 (max/d)(max/min), capped at 1.
        let v = attenuation_eval(Inverse, 800.0, 10.0, 1000.0, -60.0);
        assert_eq!(v, 1.0);
        let v = attenuation_eval(Inverse, 800.0, 400.0, 1000.0, -60.0);
        assert!((v - 0.02 * 1000.0 / 800.0 * 2.5).abs() < 1e-6);
        // LogReverse is never negative.
        let v = attenuation_eval(LogReverse, 999.0, 100.0, 1000.0, -60.0);
        assert!(v >= 0.0);
        // NaturalSound: -60 dB at the max, -30 dB halfway.
        let v = attenuation_eval(NaturalSound, 600.0, 200.0, 1000.0, -60.0);
        assert!((v - 10.0_f32.powf(-30.0 / 20.0)).abs() < 1e-6);
        // LPF.
        assert_eq!(lpf_gain(10.0, 100.0, 200.0), 1.0);
        assert_eq!(lpf_gain(200.0, 100.0, 200.0), 0.0);
        assert!((lpf_gain(150.0, 100.0, 200.0) - 0.5).abs() < 1e-6);
        // Distance types.
        let s = Vec3::new(1.0, 2.0, 3.0);
        let l = Vec3::new(4.0, 6.0, 15.0);
        assert_eq!(DistanceType::InfiniteXyPlane.distance(s, l), 12.0);
        assert_eq!(DistanceType::InfiniteXzPlane.distance(s, l), 4.0);
        assert_eq!(DistanceType::InfiniteYzPlane.distance(s, l), 3.0);
        assert_eq!(DistanceType::Normal.distance(s, l), 13.0);
    }

    #[test]
    fn distance_model_parsing() {
        assert_eq!(DistanceModel::from_value(None), DistanceModel::Linear);
        assert_eq!(
            DistanceModel::from_value(Some(&json!("ATTENUATION_NaturalSound"))),
            DistanceModel::NaturalSound
        );
        assert_eq!(
            DistanceModel::from_value(Some(&json!(1))),
            DistanceModel::Logarithmic
        );
        assert_eq!(
            DistanceModel::from_value(Some(&json!(9))),
            DistanceModel::Other
        );
    }

    #[test]
    fn cross_fade_inputs() {
        let d = DistanceDatum {
            fade_in_start: 100.0,
            fade_in_end: 200.0,
            fade_out_start: 400.0,
            fade_out_end: 600.0,
            volume: 0.8,
        };
        assert_eq!(cross_fade_gain(&d, 50.0), 0.0);
        assert!((cross_fade_gain(&d, 150.0) - 0.4).abs() < 1e-6);
        assert_eq!(cross_fade_gain(&d, 300.0), 0.8);
        assert!((cross_fade_gain(&d, 500.0) - 0.4).abs() < 1e-6);
        assert_eq!(cross_fade_gain(&d, 700.0), 0.0);
    }

    fn voices_of(lib: &AudioLibrary, engine: &mut AudioEngine, dt: f32) -> Vec<Voice> {
        engine.update(lib, Vec3::ZERO, dt).voices
    }

    #[test]
    fn a_single_wave_plays_once_and_finishes() {
        let c = cue("C", "W", vec![wave_node("W")]);
        let lib = lib_with(vec![c], &[("W", 0.5, 1)]);
        let mut e = AudioEngine::new(1);
        let id = e.play(&lib, "C", PlayParams::two_d(), Vec3::ZERO).unwrap();
        let v = voices_of(&lib, &mut e, 0.0);
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].wave, "W");
        assert!(!v[0].looping);
        let first = v[0].id;
        // Still the same voice before the end.
        let v = voices_of(&lib, &mut e, 0.25);
        assert_eq!(v[0].id, first);
        // Ends after 0.5 s of content; the component then stops.
        let _ = voices_of(&lib, &mut e, 0.3);
        let v = voices_of(&lib, &mut e, 0.01);
        assert!(v.is_empty());
        assert!(!e.is_playing(id));
    }

    #[test]
    fn gain_chain_includes_cue_component_and_wave_volume() {
        let mut c = cue("C", "W", vec![wave_node("W")]);
        c.volume_multiplier = 0.5;
        let mut lib = lib_with(vec![c], &[("W", 1.0, 1)]);
        if let Some(cd) = lib.cues.get_mut("C") {
            let mut cd2 = (**cd).clone();
            if let NodeKind::Wave(w) = &mut cd2.nodes[0].kind {
                w.volume = 0.8;
            }
            *cd = Arc::new(cd2);
        }
        let mut e = AudioEngine::new(1);
        let mut p = PlayParams::two_d();
        p.volume_multiplier = 0.5;
        e.play(&lib, "C", p, Vec3::ZERO).unwrap();
        let v = voices_of(&lib, &mut e, 0.0);
        assert!((v[0].gain - 0.8 * 0.5 * 0.5).abs() < 1e-6);
    }

    #[test]
    fn modulator_draws_volume_then_pitch_once_per_play() {
        let c = cue(
            "C",
            "M",
            vec![
                node(
                    "M",
                    "Modulator",
                    &[Some("W")],
                    json!({"PitchMin": 0.5, "PitchMax": 1.5,
                     "VolumeMin": 0.2, "VolumeMax": 0.4}),
                ),
                wave_node("W"),
            ],
        );
        let lib = lib_with(vec![c], &[("W", 10.0, 1)]);
        let mut e = AudioEngine::new(7);
        e.play(&lib, "C", PlayParams::two_d(), Vec3::ZERO).unwrap();
        let v1 = voices_of(&lib, &mut e, 0.0);
        let mut r = UeRand::new(7);
        let vol = r.pick(0.2, 0.4);
        let pitch = r.pick(0.5, 1.5);
        assert!((v1[0].gain - vol).abs() < 1e-6);
        assert!((v1[0].pitch - pitch.clamp(MIN_PITCH, MAX_PITCH)).abs() < 1e-6);
        // Values persist across updates.
        let v2 = voices_of(&lib, &mut e, 0.1);
        assert_eq!(v1[0].gain, v2[0].gain);
        assert_eq!(e.rng().seed(), r.seed());
    }

    #[test]
    fn pitch_is_clamped_like_the_source() {
        let c = cue(
            "C",
            "M",
            vec![
                node(
                    "M",
                    "Modulator",
                    &[Some("W")],
                    json!({"PitchMin": 9.0, "PitchMax": 9.0,
                     "VolumeMin": 1.0, "VolumeMax": 1.0}),
                ),
                wave_node("W"),
            ],
        );
        let lib = lib_with(vec![c], &[("W", 10.0, 1)]);
        let mut e = AudioEngine::new(1);
        e.play(&lib, "C", PlayParams::two_d(), Vec3::ZERO).unwrap();
        assert_eq!(voices_of(&lib, &mut e, 0.0)[0].pitch, MAX_PITCH);
    }

    fn random_cue(without: bool, weights: Value, children: &[Option<&str>]) -> CueDef {
        let mut nodes = vec![node(
            "R",
            "Random",
            children,
            json!({"Weights": weights, "bRandomizeWithoutReplacement": without}),
        )];
        for c in children.iter().flatten() {
            nodes.push(wave_node(c));
        }
        cue("C", "R", nodes)
    }

    fn play_once(lib: &AudioLibrary, e: &mut AudioEngine) -> Option<String> {
        let id = e.play(lib, "C", PlayParams::two_d(), Vec3::ZERO)?;
        let v = e.update(lib, Vec3::ZERO, 0.0).voices;
        e.stop(id);
        let _ = e.update(lib, Vec3::ZERO, 0.0);
        v.first().map(|v| v.wave.clone())
    }

    #[test]
    fn random_without_replacement_cycles_through_inputs() {
        let c = random_cue(
            true,
            json!([1.0, 1.0, 1.0]),
            &[Some("A"), Some("B"), Some("D")],
        );
        let lib = lib_with(vec![c], &[("A", 1.0, 1), ("B", 1.0, 1), ("D", 1.0, 1)]);
        let mut e = AudioEngine::new(12345);
        // Every block of plays after the first reset contains no repeat
        // until the list resets.
        let picks: Vec<String> = (0..9).filter_map(|_| play_once(&lib, &mut e)).collect();
        assert_eq!(picks.len(), 9);
        // The first three plays use three different inputs.
        let first: BTreeSet<&String> = picks[..3].iter().collect();
        assert_eq!(first.len(), 3, "{picks:?}");
        // Deterministic for the seed.
        let mut e2 = AudioEngine::new(12345);
        let picks2: Vec<String> = (0..9).filter_map(|_| play_once(&lib, &mut e2)).collect();
        assert_eq!(picks, picks2);
    }

    #[test]
    fn random_weights_and_empty_inputs() {
        // Weight 0 on B: never chosen with replacement.
        let c = random_cue(false, json!([1.0, 0.0]), &[Some("A"), Some("B")]);
        let lib = lib_with(vec![c], &[("A", 1.0, 1), ("B", 1.0, 1)]);
        let mut e = AudioEngine::new(3);
        for _ in 0..50 {
            assert_eq!(play_once(&lib, &mut e).as_deref(), Some("A"));
        }
        // An empty input plays nothing (the grunt cues use this).
        let c = random_cue(false, json!([1.0, 1.0, 1.0]), &[None, Some("B"), None]);
        let lib = lib_with(vec![c], &[("B", 1.0, 1)]);
        let mut e = AudioEngine::new(99);
        let picks: Vec<Option<String>> = (0..60).map(|_| play_once(&lib, &mut e)).collect();
        let played = picks.iter().filter(|p| p.is_some()).count();
        assert!(played > 5 && played < 55, "played {played} of 60");
    }

    #[test]
    fn random_without_replacement_quirk_subtracts_used_weights() {
        // Inputs A, B, D with weights 1; after A is used the sum is 2 but
        // A's weight is still subtracted, so B is always next.
        let c = random_cue(
            true,
            json!([1.0, 1.0, 1.0]),
            &[Some("A"), Some("B"), Some("D")],
        );
        let lib = lib_with(vec![c], &[("A", 1.0, 1), ("B", 1.0, 1), ("D", 1.0, 1)]);
        let mut e = AudioEngine::new(0);
        e.shared.random.insert(
            "R".to_owned(),
            RandomUsage {
                used: vec![true, false, false],
                num_used: 1,
            },
        );
        assert_eq!(play_once(&lib, &mut e).as_deref(), Some("B"));
        assert_eq!(play_once(&lib, &mut e).as_deref(), Some("D"));
    }

    #[test]
    fn mixer_plays_every_input_with_its_volume() {
        let c = cue(
            "C",
            "X",
            vec![
                node(
                    "X",
                    "Mixer",
                    &[Some("A"), Some("B")],
                    json!({"InputVolume": [0.5, 0.25]}),
                ),
                wave_node("A"),
                wave_node("B"),
            ],
        );
        let lib = lib_with(vec![c], &[("A", 1.0, 1), ("B", 1.0, 1)]);
        let mut e = AudioEngine::new(1);
        e.play(&lib, "C", PlayParams::two_d(), Vec3::ZERO).unwrap();
        let mut v = voices_of(&lib, &mut e, 0.0);
        v.sort_by(|a, b| a.wave.cmp(&b.wave));
        assert_eq!(v.len(), 2);
        assert!((v[0].gain - 0.5).abs() < 1e-6);
        assert!((v[1].gain - 0.25).abs() < 1e-6);
    }

    #[test]
    fn looping_counts_and_indefinite_loops() {
        // Finite: count 2 → 3 plays in total.
        let c = cue(
            "C",
            "L",
            vec![
                node(
                    "L",
                    "Looping",
                    &[Some("R")],
                    json!({"bLoopIndefinitely": false,
                     "LoopCountMin": 2.0, "LoopCountMax": 2.0}),
                ),
                node(
                    "R",
                    "Random",
                    &[Some("W")],
                    json!({"Weights": [1.0],
                     "bRandomizeWithoutReplacement": false}),
                ),
                wave_node("W"),
            ],
        );
        let lib = lib_with(vec![c], &[("W", 0.1, 1)]);
        let mut e = AudioEngine::new(1);
        let id = e.play(&lib, "C", PlayParams::two_d(), Vec3::ZERO).unwrap();
        let mut voice_ids = BTreeSet::new();
        for _ in 0..200 {
            for v in voices_of(&lib, &mut e, 0.01) {
                voice_ids.insert(v.id);
            }
        }
        assert_eq!(voice_ids.len(), 3, "three separate plays");
        assert!(!e.is_playing(id));

        // Indefinite through a modulator: a seamless loop with one voice.
        let c = cue(
            "C",
            "L",
            vec![
                node(
                    "L",
                    "Looping",
                    &[Some("M")],
                    json!({"bLoopIndefinitely": true}),
                ),
                node(
                    "M",
                    "Modulator",
                    &[Some("W")],
                    json!({"PitchMin": 1.0, "PitchMax": 1.0,
                     "VolumeMin": 0.5, "VolumeMax": 1.0}),
                ),
                wave_node("W"),
            ],
        );
        let lib = lib_with(vec![c], &[("W", 0.1, 1)]);
        let mut e = AudioEngine::new(5);
        e.play(&lib, "C", PlayParams::two_d(), Vec3::ZERO).unwrap();
        let mut ids = BTreeSet::new();
        let mut gains = BTreeSet::new();
        for _ in 0..100 {
            let v = voices_of(&lib, &mut e, 0.01);
            assert_eq!(v.len(), 1);
            assert!(v[0].looping);
            ids.insert(v[0].id);
            gains.insert(v[0].gain.to_bits());
        }
        assert_eq!(ids.len(), 1, "seamless: one voice");
        assert!(gains.len() > 1, "the modulator re-rolls at each loop");
    }

    #[test]
    fn delay_postpones_and_keeps_the_component_alive() {
        let c = cue(
            "C",
            "D",
            vec![
                node(
                    "D",
                    "Delay",
                    &[Some("W")],
                    json!({"DelayMin": 0.5, "DelayMax": 0.5}),
                ),
                wave_node("W"),
            ],
        );
        let lib = lib_with(vec![c], &[("W", 0.2, 1)]);
        let mut e = AudioEngine::new(1);
        let id = e.play(&lib, "C", PlayParams::two_d(), Vec3::ZERO).unwrap();
        // The delay starts at the first update (the node initialises when
        // it is first parsed).
        assert!(voices_of(&lib, &mut e, 0.0).is_empty());
        assert!(voices_of(&lib, &mut e, 0.2).is_empty());
        assert!(e.is_playing(id));
        assert!(voices_of(&lib, &mut e, 0.2).is_empty());
        assert_eq!(voices_of(&lib, &mut e, 0.2).len(), 1);
    }

    #[test]
    fn concatenator_plays_inputs_in_order() {
        let c = cue(
            "C",
            "K",
            vec![
                node(
                    "K",
                    "Concatenator",
                    &[Some("A"), Some("B")],
                    json!({"InputVolume": [1.0, 0.5]}),
                ),
                wave_node("A"),
                wave_node("B"),
            ],
        );
        let lib = lib_with(vec![c], &[("A", 0.1, 1), ("B", 0.1, 1)]);
        let mut e = AudioEngine::new(1);
        let id = e.play(&lib, "C", PlayParams::two_d(), Vec3::ZERO).unwrap();
        let mut order = Vec::new();
        for _ in 0..60 {
            for v in voices_of(&lib, &mut e, 0.01) {
                if order.last() != Some(&v.wave) {
                    order.push(v.wave.clone());
                }
            }
        }
        assert_eq!(order, vec!["A".to_owned(), "B".to_owned()]);
        assert!(!e.is_playing(id));
    }

    #[test]
    fn attenuation_node_uses_distance_and_spatialises_mono_only() {
        let att = json!({"bAttenuate": true, "bSpatialize": true, "dBAttenuationAtMax": -60.0,
                         "DistanceAlgorithm": "ATTENUATION_Linear", "RadiusMin": 100.0,
                         "RadiusMax": 1100.0, "LPFRadiusMin": 0.0, "LPFRadiusMax": 0.0});
        let c = cue(
            "C",
            "T",
            vec![
                node("T", "Attenuation", &[Some("W")], att.clone()),
                wave_node("W"),
            ],
        );
        let s = CueDef::from_json(
            "S",
            &json!({"first_node": "T2", "volume_multiplier": 1.0, "nodes": [
                {"path": "T2", "kind": "Attenuation", "class": "SoundNodeAttenuation",
                 "children": ["W2"], "params": att},
                {"path": "W2", "kind": "Wave", "class": "SoundNodeWave", "children": [],
                 "params": {}, "wave": {"volume": 1.0, "pitch": 1.0}}]}),
        );
        let lib = lib_with(vec![c, s], &[("W", 10.0, 1), ("W2", 10.0, 2)]);
        let mut e = AudioEngine::new(1);
        e.play(
            &lib,
            "C",
            PlayParams::at(Vec3::new(600.0, 0.0, 0.0)),
            Vec3::ZERO,
        )
        .unwrap();
        let v = voices_of(&lib, &mut e, 0.0);
        assert!((v[0].gain - 0.5).abs() < 1e-5);
        assert!(v[0].spatial);
        // The same cue without spatialisation ignores the distance.
        let mut e = AudioEngine::new(1);
        let mut p = PlayParams::two_d();
        p.location = Vec3::new(600.0, 0.0, 0.0);
        e.play(&lib, "C", p, Vec3::ZERO).unwrap();
        let v = voices_of(&lib, &mut e, 0.0);
        assert_eq!(v[0].gain, 1.0);
        assert!(!v[0].spatial);
        // Stereo waves are never spatialised.
        let mut e = AudioEngine::new(1);
        e.play(
            &lib,
            "S",
            PlayParams::at(Vec3::new(10.0, 0.0, 0.0)),
            Vec3::ZERO,
        )
        .unwrap();
        assert!(!voices_of(&lib, &mut e, 0.0)[0].spatial);
        // Beyond the max radius: silent (no voice), but still playing.
        let mut e = AudioEngine::new(1);
        let id = e
            .play(
                &lib,
                "C",
                PlayParams::at(Vec3::new(5000.0, 0.0, 0.0)),
                Vec3::ZERO,
            )
            .unwrap();
        assert!(voices_of(&lib, &mut e, 0.0).is_empty());
        assert!(e.is_playing(id));
    }

    #[test]
    fn continuous_modulator_follows_float_parameters() {
        let n = json!({"path": "MC", "kind": "ModulatorContinuous",
            "class": "SoundNodeModulatorContinuous", "children": ["W"], "params": {},
            "distributions": [
                {"property": "VolumeModulation", "class": "DistributionFloatSoundParameter",
                 "params": {"ParameterName": "P", "MinInput": 0.0, "MaxInput": 100.0,
                            "MinOutput": 0.0, "MaxOutput": 1.0}},
                {"property": "PitchModulation", "class": "DistributionFloatSoundParameter",
                 "params": {"ParameterName": "P", "MinInput": 0.0, "MaxInput": 100.0,
                            "MinOutput": 1.0, "MaxOutput": 1.5}}]});
        let c = cue("C", "MC", vec![n, wave_node("W")]);
        let lib = lib_with(vec![c], &[("W", 10.0, 1)]);
        let mut e = AudioEngine::new(1);
        let id = e.play(&lib, "C", PlayParams::two_d(), Vec3::ZERO).unwrap();
        e.set_float_parameter(id, "P", 50.0);
        let v = voices_of(&lib, &mut e, 0.0);
        assert!((v[0].gain - 0.5).abs() < 1e-6);
        assert!((v[0].pitch - 1.25).abs() < 1e-6);
        e.set_float_parameter(id, "P", 1000.0);
        let v = voices_of(&lib, &mut e, 0.0);
        assert!((v[0].gain - 1.0).abs() < 1e-6);
    }

    #[test]
    fn wave_param_overrides_children() {
        let c = cue(
            "C",
            "P",
            vec![
                node(
                    "P",
                    "WaveParam",
                    &[Some("A")],
                    json!({"WaveParameterName": "Announcement"}),
                ),
                wave_node("A"),
            ],
        );
        let lib = lib_with(vec![c], &[("A", 1.0, 1), ("Other", 1.0, 1)]);
        let mut e = AudioEngine::new(1);
        let id = e.play(&lib, "C", PlayParams::two_d(), Vec3::ZERO).unwrap();
        assert_eq!(voices_of(&lib, &mut e, 0.0)[0].wave, "A");
        let id2 = e.play(&lib, "C", PlayParams::two_d(), Vec3::ZERO).unwrap();
        e.set_wave_parameter(id2, "Announcement", "Other");
        let v = voices_of(&lib, &mut e, 0.0);
        assert!(v.iter().any(|v| v.wave == "Other" && v.instance == id2));
        assert!(v.iter().any(|v| v.wave == "A" && v.instance == id));
    }

    fn ambient_cue(non_loop: bool, slots: Value) -> CueDef {
        let kind = if non_loop {
            "AmbientNonLoop"
        } else {
            "Ambient"
        };
        cue(
            "C",
            "AMB",
            vec![node(
                "AMB",
                kind,
                &[],
                json!({"bAttenuate": true, "bSpatialize": true, "dBAttenuationAtMax": -60.0,
                       "DistanceModel": "ATTENUATION_Linear", "RadiusMin": 0.0,
                       "RadiusMax": 1000.0, "PitchMin": 1.0, "PitchMax": 1.0,
                       "VolumeMin": 0.5, "VolumeMax": 0.5, "DelayMin": 1.0, "DelayMax": 1.0,
                       "SoundSlots": slots}),
            )],
        )
    }

    #[test]
    fn ambient_node_loops_every_slot() {
        let c = ambient_cue(
            false,
            json!([{"Wave": "A", "PitchScale": 1.0, "VolumeScale": 1.0, "Weight": 1.0},
                   {"Wave": "B", "PitchScale": 1.0, "VolumeScale": 0.5, "Weight": 1.0}]),
        );
        let lib = lib_with(vec![c], &[("A", 0.1, 1), ("B", 0.1, 1)]);
        let mut e = AudioEngine::new(1);
        e.play(
            &lib,
            "C",
            PlayParams::at(Vec3::new(500.0, 0.0, 0.0)),
            Vec3::ZERO,
        )
        .unwrap();
        let mut v = voices_of(&lib, &mut e, 0.0);
        v.sort_by(|a, b| a.wave.cmp(&b.wave));
        assert_eq!(v.len(), 2);
        assert!(v.iter().all(|v| v.looping));
        // 0.5 (node volume) × 0.5 (linear at half the radius) × slot scale.
        assert!((v[0].gain - 0.25).abs() < 1e-5);
        assert!((v[1].gain - 0.125).abs() < 1e-5);
        let ids: Vec<u64> = v.iter().map(|v| v.id).collect();
        for _ in 0..50 {
            let w = voices_of(&lib, &mut e, 0.01);
            assert_eq!(w.len(), 2);
            for x in &w {
                assert!(ids.contains(&x.id), "loops keep their voice");
            }
        }
    }

    #[test]
    fn ambient_non_loop_waits_then_plays_one_slot() {
        let c = ambient_cue(
            true,
            json!([{"Wave": "A", "PitchScale": 1.0, "VolumeScale": 1.0, "Weight": 1.0}]),
        );
        let lib = lib_with(vec![c], &[("A", 0.2, 1)]);
        let mut e = AudioEngine::new(1);
        let id = e
            .play(&lib, "C", PlayParams::at(Vec3::ZERO), Vec3::ZERO)
            .unwrap();
        assert!(voices_of(&lib, &mut e, 0.0).is_empty());
        assert!(voices_of(&lib, &mut e, 0.5).is_empty());
        assert!(e.is_playing(id));
        assert!(voices_of(&lib, &mut e, 0.4).is_empty());
        let v = voices_of(&lib, &mut e, 0.2);
        assert_eq!(v.len(), 1);
        assert!(!v[0].looping);
        // After the sound, a new delay; the component keeps running.
        for _ in 0..30 {
            let _ = voices_of(&lib, &mut e, 0.01);
        }
        assert!(e.is_playing(id));
        assert!(voices_of(&lib, &mut e, 0.1).is_empty());
    }

    #[test]
    fn ambient_non_loop_toggle_stops_after_one_sound() {
        let mut c = ambient_cue(
            true,
            json!([{"Wave": "A", "PitchScale": 1.0, "VolumeScale": 1.0, "Weight": 1.0}]),
        );
        if let NodeKind::Ambient(a) = &mut c.nodes[0].kind
            && let Some(nl) = a.non_loop.as_mut()
        {
            nl.toggle = true;
        }
        let lib = lib_with(vec![c], &[("A", 0.1, 1)]);
        let mut e = AudioEngine::new(1);
        let id = e
            .play(&lib, "C", PlayParams::at(Vec3::ZERO), Vec3::ZERO)
            .unwrap();
        for _ in 0..200 {
            let _ = voices_of(&lib, &mut e, 0.01);
        }
        assert!(!e.is_playing(id));
    }

    #[test]
    fn fades_follow_the_component_rules() {
        let mut f = Fades::default();
        assert_eq!(f.fade_in(0.5), 1.0);
        assert_eq!(f.fade_out(0.5), 1.0);
        assert_eq!(f.adjust(0.5), 1.0);
        let f = Fades {
            in_start: 0.0,
            in_stop: 2.0,
            in_target: 1.0,
            ..Fades::default()
        };
        assert!((f.fade_in(1.0) - 0.5).abs() < 1e-6);
        assert_eq!(f.fade_in(3.0), 1.0);
        let f = Fades {
            out_start: 1.0,
            out_stop: 3.0,
            out_target: 0.0,
            ..Fades::default()
        };
        assert!((f.fade_out(2.0) - 0.5).abs() < 1e-6);
        assert_eq!(f.fade_out(4.0), 0.0);
        let mut f = Fades {
            adj_start: 0.0,
            adj_stop: 1.0,
            adj_target: 0.5,
            ..Fades::default()
        };
        assert!((f.adjust(0.5) - 0.75).abs() < 1e-6);
        assert_eq!(f.adjust(2.0), 0.5);
        assert_eq!(f.adj_current, 0.5);
    }

    #[test]
    fn fade_out_stops_the_component_and_fade_in_reverses_it() {
        let c = cue("C", "W", vec![wave_node("W")]);
        let mut lib = lib_with(vec![c], &[("W", 100.0, 1)]);
        if let Some(cd) = lib.cues.get_mut("C") {
            let mut cd2 = (**cd).clone();
            cd2.duration = Some(INDEFINITE_DURATION);
            *cd = Arc::new(cd2);
        }
        let mut e = AudioEngine::new(1);
        let id = e.play(&lib, "C", PlayParams::two_d(), Vec3::ZERO).unwrap();
        let _ = voices_of(&lib, &mut e, 0.1);
        e.fade_out(id, 1.0, 0.0);
        let v = voices_of(&lib, &mut e, 0.5);
        assert!((v[0].gain - 0.5).abs() < 1e-5);
        // Reverse half-way: same instance, the level continues upward.
        let same = e
            .fade_in(
                &lib,
                Some(id),
                "C",
                PlayParams::two_d(),
                (1.0, 1.0),
                Vec3::ZERO,
            )
            .unwrap();
        assert_eq!(same, id);
        let v = voices_of(&lib, &mut e, 0.25);
        assert!((v[0].gain - 0.75).abs() < 1e-5, "{}", v[0].gain);
        e.fade_out(id, 0.2, 0.0);
        let _ = voices_of(&lib, &mut e, 0.3);
        let _ = voices_of(&lib, &mut e, 0.01);
        assert!(!e.is_playing(id));
    }

    #[test]
    fn max_concurrent_play_count_and_voice_limit() {
        let mut c = cue("C", "W", vec![wave_node("W")]);
        c.max_concurrent_play_count = 2;
        let lib = lib_with(vec![c], &[("W", 10.0, 1)]);
        let mut e = AudioEngine::new(1);
        assert!(e.play(&lib, "C", PlayParams::two_d(), Vec3::ZERO).is_some());
        assert!(e.play(&lib, "C", PlayParams::two_d(), Vec3::ZERO).is_some());
        assert!(e.play(&lib, "C", PlayParams::two_d(), Vec3::ZERO).is_none());

        let mut c = cue("D", "W", vec![wave_node("W")]);
        c.max_concurrent_play_count = 0;
        let lib = lib_with(vec![c], &[("W", 10.0, 1)]);
        let mut e = AudioEngine::new(1);
        for i in 0..40 {
            let mut p = PlayParams::two_d();
            p.volume_multiplier = 0.01 * (i as f32 + 1.0);
            assert!(e.play(&lib, "D", p, Vec3::ZERO).is_some());
        }
        let v = voices_of(&lib, &mut e, 0.0);
        assert_eq!(v.len(), MAX_CHANNELS);
    }

    #[test]
    fn subtitles_show_the_current_line_until_the_wave_ends() {
        let mut m = SubtitleManager::default();
        let lines = vec![
            SubtitleLine {
                text: "one".into(),
                time: 0.0,
            },
            SubtitleLine {
                text: "two".into(),
                time: 2.0,
            },
            SubtitleLine {
                text: "late".into(),
                time: 99.0,
            },
        ];
        m.queue(InstanceId(1), 0.0, 5.0, &lines, 10.0);
        assert_eq!(m.current(10.5), None, "priority 0 shows nothing");
        m.queue(InstanceId(1), 10.0, 5.0, &lines, 10.0);
        assert_eq!(m.current(10.5).as_deref(), Some("one"));
        assert_eq!(m.current(12.5).as_deref(), Some("two"));
        // A line after the end is clamped to the end (and hidden by the
        // end marker at the same time).
        assert_eq!(m.current(15.5), None);
        // Higher priority wins.
        m.queue(InstanceId(1), 1.0, 5.0, &lines, 20.0);
        m.queue(
            InstanceId(2),
            5.0,
            5.0,
            &[SubtitleLine {
                text: "loud".into(),
                time: 0.0,
            }],
            20.0,
        );
        assert_eq!(m.current(20.1).as_deref(), Some("loud"));
        m.kill(InstanceId(2));
        assert_eq!(m.current(20.1).as_deref(), Some("one"));
    }

    fn narration_lib() -> AudioLibrary {
        let mut a = cue("A", "WA", vec![wave_node("WA")]);
        a.duration = Some(2.0);
        let mut b = cue("B", "WB", vec![wave_node("WB")]);
        b.duration = Some(1.0);
        let mut z = cue("Z", "WZ", vec![wave_node("WZ")]);
        z.duration = Some(1.0);
        let mut lib = lib_with(
            vec![a, b, z],
            &[("WA", 2.0, 1), ("WB", 1.0, 1), ("WZ", 1.0, 1)],
        );
        lib.subtitles.insert(
            "WA".into(),
            SubtitleTrack {
                lines: vec![SubtitleLine {
                    text: "hello".into(),
                    time: 0.0,
                }],
                duration: Some(2.0),
                ..SubtitleTrack::default()
            },
        );
        lib
    }

    fn add(id: &str, cue: &str, delay: f32) -> AudioCommand {
        AudioCommand::NarratorAddLine {
            id: id.into(),
            cue: cue.into(),
            volume: 1.0,
            delay,
            remove_all_others: false,
            fade_out_if_active: true,
            node: None,
        }
    }

    #[test]
    fn narrator_queue_plays_lines_in_order_with_delays() {
        let lib = narration_lib();
        let mut e = AudioEngine::new(1);
        e.apply(&lib, &add("a", "A", 5.0), Vec3::ZERO);
        e.apply(&lib, &add("b", "B", 0.5), Vec3::ZERO);
        let f = e.update(&lib, Vec3::ZERO, 0.0);
        assert_eq!(f.feedback, vec![AudioFeedback::NarratorStarted]);
        assert_eq!(f.voices[0].wave, "WA", "the first line ignores its delay");
        assert_eq!(f.subtitle.as_deref(), Some("hello"));
        let mut events = Vec::new();
        let mut waves = Vec::new();
        for _ in 0..400 {
            let f = e.update(&lib, Vec3::ZERO, 0.01);
            events.extend(f.feedback);
            for v in f.voices {
                if waves.last() != Some(&v.wave) {
                    waves.push(v.wave);
                }
            }
        }
        assert_eq!(
            events,
            vec![
                AudioFeedback::NarratorLineFinished { id: "a".into() },
                AudioFeedback::NarratorLineFinished { id: "b".into() },
                AudioFeedback::NarratorFinished,
            ]
        );
        assert_eq!(waves, vec!["WA".to_owned(), "WB".to_owned()]);
    }

    #[test]
    fn narrator_zero_delay_queued_line_stalls_like_the_original() {
        let lib = narration_lib();
        let mut e = AudioEngine::new(1);
        e.apply(&lib, &add("a", "A", 0.0), Vec3::ZERO);
        e.apply(&lib, &add("b", "B", 0.0), Vec3::ZERO);
        let mut events = Vec::new();
        for _ in 0..500 {
            events.extend(e.update(&lib, Vec3::ZERO, 0.01).feedback);
        }
        assert!(events.contains(&AudioFeedback::NarratorLineFinished { id: "a".into() }));
        assert!(!events.contains(&AudioFeedback::NarratorFinished));
        assert_eq!(e.narrator().queue(), vec!["b"]);
    }

    #[test]
    fn narrator_remove_and_remove_all_quirks() {
        let lib = narration_lib();
        let mut e = AudioEngine::new(1);
        e.apply(&lib, &add("a", "A", 1.0), Vec3::ZERO);
        e.apply(&lib, &add("b", "B", 1.0), Vec3::ZERO);
        e.apply(&lib, &add("z", "Z", 1.0), Vec3::ZERO);
        // "Remove all others" keeps the playing line.
        e.apply(
            &lib,
            &AudioCommand::NarratorAddLine {
                id: "n".into(),
                cue: "B".into(),
                volume: 1.0,
                delay: 1.0,
                remove_all_others: true,
                fade_out_if_active: true,
                node: None,
            },
            Vec3::ZERO,
        );
        assert_eq!(e.narrator().queue(), vec!["a", "n"]);
        // Removing the playing line stops it, but its end timer still runs
        // and then ends the next line, which never played.
        e.apply(
            &lib,
            &AudioCommand::NarratorRemoveLine {
                id: "a".into(),
                stop_if_active: true,
                fade_time: 0.2,
            },
            Vec3::ZERO,
        );
        let f = e.update(&lib, Vec3::ZERO, 0.0);
        assert!(f.voices.is_empty());
        let mut events = Vec::new();
        for _ in 0..250 {
            events.extend(e.update(&lib, Vec3::ZERO, 0.01).feedback);
        }
        assert_eq!(
            events,
            vec![
                AudioFeedback::NarratorLineFinished { id: "n".into() },
                AudioFeedback::NarratorFinished,
            ]
        );
    }

    fn classes_doc() -> Value {
        json!({"format": "asamu-audio-sound-classes", "version": 1,
            "classes": [
                {"name": "Master", "properties": {"Volume": 1.0}, "child_class_names": ["SFX", "Music"]},
                {"name": "SFX", "properties": {"Volume": 0.8}, "child_class_names": ["Game"]},
                {"name": "Game", "properties": {"Volume": 0.5, "Pitch": 1.0}, "child_class_names": []},
                {"name": "Music", "properties": {"Volume": 0.8, "bIsMusic": true, "bAlwaysPlay": true},
                 "child_class_names": ["Tune"]},
                {"name": "Tune", "properties": {"Volume": 1.0}, "child_class_names": []},
                {"name": "Orphan", "properties": {"Volume": 0.1}, "child_class_names": []}],
            "modes": [
                {"name": "Duck", "params": {"InitialDelay": 0.0, "FadeInTime": 1.0, "Duration": 2.0,
                 "FadeOutTime": 1.0, "SoundClassEffects": [
                    {"SoundClass": "SFX", "VolumeAdjuster": 0.0, "PitchAdjuster": 1.0,
                     "VoiceCenterChannelVolumeAdjuster": 1.0, "bApplyToChildren": true}]}}]})
    }

    #[test]
    fn sound_classes_multiply_down_the_tree() {
        let docs = AudioDocuments {
            sound_classes: Some(classes_doc()),
            ..AudioDocuments::default()
        };
        let lib = AudioLibrary::from_documents(Path::new("x"), docs).unwrap();
        let r = resolve_classes(&lib.classes, &BTreeMap::new());
        assert!((r["Game"].volume - 0.4).abs() < 1e-6);
        assert!(r["Tune"].is_music, "bIsMusic is inherited");
        assert!(!r["Tune"].always_play, "bAlwaysPlay is not inherited");
        assert!(!r.contains_key("Orphan"));
        let mut o = BTreeMap::new();
        o.insert("SFX".to_owned(), 0.5);
        let r = resolve_classes(&lib.classes, &o);
        assert!((r["Game"].volume - 0.25).abs() < 1e-6);
    }

    #[test]
    fn sound_mode_fades_in_holds_and_returns() {
        let docs = AudioDocuments {
            sound_classes: Some(classes_doc()),
            ..AudioDocuments::default()
        };
        let lib = AudioLibrary::from_documents(Path::new("x"), docs).unwrap();
        let mut m = SoundModeState::default();
        m.update_with(&lib, 0.0);
        assert!((m.class("Game").unwrap().volume - 0.4).abs() < 1e-6);
        m.set(&lib, Some("SoundClassesAndModes.Duck"), 0.0);
        m.update_with(&lib, 0.5);
        assert!((m.class("Game").unwrap().volume - 0.2).abs() < 1e-6);
        m.update_with(&lib, 2.0);
        assert_eq!(m.class("Game").unwrap().volume, 0.0);
        // Duration 2 after the 1 s fade: at t = 3 it returns to the base
        // over the mode's fade-out time.
        m.update_with(&lib, 3.0);
        assert_eq!(m.current(), None);
        m.update_with(&lib, 3.5);
        assert!((m.class("Game").unwrap().volume - 0.2).abs() < 1e-6);
        m.update_with(&lib, 4.5);
        assert!((m.class("Game").unwrap().volume - 0.4).abs() < 1e-6);
        assert!((m.class("Tune").unwrap().volume - 0.8).abs() < 1e-6);
    }

    #[test]
    fn class_volume_and_stereo_bleed_reach_the_voice() {
        let mut docs = AudioDocuments {
            sound_classes: Some(classes_doc()),
            ..AudioDocuments::default()
        };
        docs.cues = Some(json!({"format": "asamu-audio-cues", "version": 1, "cues": {
            "C": {"sound_class": "Game", "first_node": "W", "volume_multiplier": 1.0,
                  "pitch_multiplier": 1.0, "duration": 1.0, "nodes": [
                    {"path": "W", "kind": "Wave", "class": "SoundNodeWave", "children": [],
                     "params": {}, "wave": {"volume": 1.0, "pitch": 1.0}}]}}}));
        docs.manifest = Some(json!({"format": "asamu-audio-manifest", "version": 1,
            "language": "INT", "audio_language": "INT", "waves": {
                "W": {"file": "waves/W.ogg", "samples": 44100, "sample_rate": 44100,
                      "channels": 2, "volume": 1.0, "pitch": 1.0}}}));
        let lib = AudioLibrary::from_documents(Path::new("x"), docs).unwrap();
        assert_eq!(lib.wave("W").unwrap().duration, 1.0);
        let mut e = AudioEngine::new(1);
        e.play(&lib, "C", PlayParams::two_d(), Vec3::ZERO).unwrap();
        let v = voices_of(&lib, &mut e, 0.0);
        assert!((v[0].gain - 0.4 * STEREO_BLEED_GAIN).abs() < 1e-6);
        e.set_class_volume(&lib, "Master", 0.5);
        let v = voices_of(&lib, &mut e, 0.0);
        assert!((v[0].gain - 0.2 * STEREO_BLEED_GAIN).abs() < 1e-6);
    }

    #[test]
    fn ambient_actors_load_autoplay_and_toggle() {
        let mut lib = narration_lib();
        let doc = json!({"format": "asamu-audio-ambient", "version": 1, "maps": [{
            "package": "AG-Test", "ambient": [
                {"name": "AmbientSound_0", "export_index": 1, "kind": "AmbientSound",
                 "location": [0.0, 0.0, 0.0], "auto_play": true, "sound_cue": "A",
                 "volume_multiplier": 0.5, "pitch_multiplier": 1.0, "instance": {},
                 "audio_component_params": {}},
                {"name": "AmbientSoundSimpleToggleable_0", "export_index": 2,
                 "path": "AG-Test.TheWorld.PersistentLevel.AmbientSoundSimpleToggleable_0",
                 "kind": "SimpleToggleable", "location": [0.0, 0.0, 0.0], "auto_play": false,
                 "sound_cue": "B", "volume_multiplier": 1.0, "pitch_multiplier": 1.0,
                 "instance": {"bFadeOnToggle": true, "FadeInDuration": 2.0},
                 "audio_component_params": {}}]}]});
        let parsed = AudioLibrary::from_documents(
            Path::new("x"),
            AudioDocuments {
                ambient: Some(doc),
                ..AudioDocuments::default()
            },
        )
        .unwrap();
        lib.ambient = parsed.ambient;
        let defs = lib.ambient_for_map("ag-test");
        assert_eq!(defs.len(), 2);
        assert_eq!(defs[1].toggle.fade_in_duration, 2.0);
        assert_eq!(defs[1].toggle.fade_out_duration, 1.0);
        let mut e = AudioEngine::new(1);
        assert_eq!(e.load_ambient(&lib, "AG-Test", Vec3::ZERO), 2);
        assert_eq!(e.playing_ambient(), vec!["AmbientSound_0"]);
        assert!(!e.toggle_ambient(&lib, "AmbientSound_0", ToggleAction::TurnOff, Vec3::ZERO));
        assert!(e.toggle_ambient(
            &lib,
            "AmbientSoundSimpleToggleable_0",
            ToggleAction::Toggle,
            Vec3::ZERO
        ));
        let f = e.update(&lib, Vec3::ZERO, 0.0);
        assert_eq!(f.voices.len(), 2);
        let b = f.voices.iter().find(|v| v.wave == "WB").unwrap();
        assert!(b.gain < 0.01, "fading in over 2 s");
        let f = e.update(&lib, Vec3::ZERO, 0.5);
        let b = f.voices.iter().find(|v| v.wave == "WB").unwrap();
        assert!((b.gain - 0.25).abs() < 1e-5);
        // The Kismet runtime names targets by object path.
        e.apply(
            &lib,
            &AudioCommand::ToggleAmbient {
                actor: "AG-Test.TheWorld.PersistentLevel.AmbientSoundSimpleToggleable_0".into(),
                action: ToggleAction::Toggle,
            },
            Vec3::ZERO,
        );
        for _ in 0..120 {
            let _ = e.update(&lib, Vec3::ZERO, 0.01);
        }
        assert_eq!(e.playing_ambient(), vec!["AmbientSound_0"]);
    }

    #[test]
    fn spline_source_is_the_closest_point() {
        let def = AmbientActorDef {
            name: "S".into(),
            path: "AG-Test.TheWorld.PersistentLevel.S".into(),
            export_index: 0,
            kind: AmbientActorKind::Spline,
            location: Vec3::ZERO,
            auto_play: true,
            cue: None,
            volume_multiplier: 1.0,
            pitch_multiplier: 1.0,
            toggle: ToggleFade::default(),
            spline_points: vec![Vec3::ZERO, Vec3::new(100.0, 0.0, 0.0)],
        };
        assert_eq!(
            def.source_location(Vec3::new(40.0, 30.0, 0.0)),
            Vec3::new(40.0, 0.0, 0.0)
        );
        assert_eq!(def.source_location(Vec3::new(-50.0, 0.0, 0.0)), Vec3::ZERO);
    }

    #[test]
    fn commands_round_trip_and_report_unknown_cues() {
        let cmd = AudioCommand::PlaySound {
            cue: "X".into(),
            source: SoundSource::Location {
                location: [1.0, 2.0, 3.0],
            },
            volume_multiplier: 0.5,
            pitch_multiplier: 1.0,
            fade_in_time: 0.0,
            suppress_subtitles: false,
            suppress_spatialization: false,
            node: Some(4),
        };
        let s = serde_json::to_string(&cmd).unwrap();
        let back: AudioCommand = serde_json::from_str(&s).unwrap();
        assert_eq!(back, cmd);
        let minimal: AudioCommand =
            serde_json::from_str(r#"{"command":"narrator_add_line","id":"x","cue":"C"}"#).unwrap();
        assert_eq!(
            minimal,
            AudioCommand::NarratorAddLine {
                id: "x".into(),
                cue: "C".into(),
                volume: 1.0,
                delay: 0.0,
                remove_all_others: false,
                fade_out_if_active: true,
                node: None,
            }
        );
        let lib = AudioLibrary::default();
        let mut e = AudioEngine::new(1);
        e.apply(&lib, &cmd, Vec3::ZERO);
        let f = e.update(&lib, Vec3::ZERO, 0.0);
        assert_eq!(
            f.feedback,
            vec![AudioFeedback::UnknownCue { cue: "X".into() }]
        );
    }

    #[test]
    fn kismet_play_and_stop_sound() {
        let lib = narration_lib();
        let mut e = AudioEngine::new(1);
        e.apply(
            &lib,
            &AudioCommand::PlaySound {
                cue: "A".into(),
                source: SoundSource::TwoD,
                volume_multiplier: 1.0,
                pitch_multiplier: 1.0,
                fade_in_time: 0.0,
                suppress_subtitles: false,
                suppress_spatialization: false,
                node: None,
            },
            Vec3::ZERO,
        );
        let f = e.update(&lib, Vec3::ZERO, 0.01);
        assert_eq!(f.voices.len(), 1);
        assert_eq!(
            f.subtitle.as_deref(),
            Some("hello"),
            "Kismet sounds show subtitles"
        );
        e.apply(
            &lib,
            &AudioCommand::StopSound {
                cue: "a".into(),
                fade_out_time: 0.0,
                node: None,
            },
            Vec3::ZERO,
        );
        let f = e.update(&lib, Vec3::ZERO, 0.01);
        assert!(f.voices.is_empty());
        assert_eq!(f.subtitle, None);
    }

    #[test]
    fn pause_freezes_game_sounds() {
        let c = cue("C", "W", vec![wave_node("W")]);
        let lib = lib_with(vec![c], &[("W", 0.5, 1)]);
        let mut e = AudioEngine::new(1);
        let id = e.play(&lib, "C", PlayParams::two_d(), Vec3::ZERO).unwrap();
        let _ = voices_of(&lib, &mut e, 0.0);
        e.set_paused(true);
        for _ in 0..100 {
            let _ = voices_of(&lib, &mut e, 0.1);
        }
        assert!(e.is_playing(id));
        assert_eq!(e.instance(id).unwrap().playback_time(), 0.0);
        e.set_paused(false);
        for _ in 0..10 {
            let _ = voices_of(&lib, &mut e, 0.1);
        }
        assert!(!e.is_playing(id));
    }

    #[test]
    fn safety_stop_after_duration_at_min_pitch() {
        // A finite cue whose graph keeps the component alive (a delay
        // that never ends) is stopped after duration / 0.4.
        let mut c = cue(
            "C",
            "D",
            vec![
                node(
                    "D",
                    "Delay",
                    &[Some("W")],
                    json!({"DelayMin": 100.0, "DelayMax": 100.0}),
                ),
                wave_node("W"),
            ],
        );
        c.duration = Some(1.0);
        let lib = lib_with(vec![c], &[("W", 1.0, 1)]);
        let mut e = AudioEngine::new(1);
        let id = e.play(&lib, "C", PlayParams::two_d(), Vec3::ZERO).unwrap();
        for _ in 0..24 {
            let _ = voices_of(&lib, &mut e, 0.1);
        }
        assert!(e.is_playing(id));
        for _ in 0..3 {
            let _ = voices_of(&lib, &mut e, 0.1);
        }
        assert!(!e.is_playing(id));
    }

    #[test]
    fn engine_is_deterministic_for_a_seed() {
        let c = random_cue(
            true,
            json!([1.0, 2.0, 3.0]),
            &[Some("A"), Some("B"), Some("D")],
        );
        let lib = lib_with(vec![c], &[("A", 0.3, 1), ("B", 0.2, 1), ("D", 0.1, 1)]);
        let run = || {
            let mut e = AudioEngine::new(2024);
            let mut log = Vec::new();
            for i in 0..50 {
                if i % 5 == 0 {
                    e.play(&lib, "C", PlayParams::two_d(), Vec3::ZERO);
                }
                for v in e.update(&lib, Vec3::ZERO, 0.05).voices {
                    log.push((v.id, v.wave, v.gain.to_bits(), v.pitch.to_bits()));
                }
            }
            log
        };
        assert_eq!(run(), run());
    }

    #[test]
    fn hostile_graphs_do_not_panic() {
        // A cycle, dangling children, NaN values and absurd counts.
        let nodes = vec![
            node(
                "A",
                "Mixer",
                &[Some("B"), Some("missing")],
                json!({"InputVolume": [1.0]}),
            ),
            node(
                "B",
                "Random",
                &[Some("A"), Some("W")],
                json!({"Weights": [f64::MAX, -1.0]}),
            ),
            node(
                "L",
                "Looping",
                &[Some("A")],
                json!({"LoopCountMin": 1e30, "LoopCountMax": -1e30}),
            ),
            wave_node("W"),
        ];
        let c = cue("C", "A", nodes);
        let lib = lib_with(vec![c], &[("W", 0.0, 1)]);
        let mut e = AudioEngine::new(1);
        for _ in 0..5 {
            e.play(&lib, "C", PlayParams::two_d(), Vec3::ZERO);
            let _ = e.update(&lib, Vec3::ZERO, 0.1);
            let _ = e.update(&lib, Vec3::ZERO, f32::NAN);
            let _ = e.update(&lib, Vec3::new(f32::NAN, 0.0, 0.0), 0.1);
        }
        // A cue with no nodes finishes at once.
        let empty = CueDef::from_json("E", &json!({}));
        assert!(empty.first.is_none());
        let lib = lib_with(vec![empty], &[]);
        let mut e = AudioEngine::new(1);
        let id = e.play(&lib, "E", PlayParams::two_d(), Vec3::ZERO).unwrap();
        let _ = e.update(&lib, Vec3::ZERO, 0.0);
        assert!(!e.is_playing(id));
    }

    #[test]
    fn documents_are_validated() {
        let bad = AudioDocuments {
            cues: Some(json!({"format": "something-else", "version": 1})),
            ..AudioDocuments::default()
        };
        assert!(AudioLibrary::from_documents(Path::new("x"), bad).is_err());
        let bad = AudioDocuments {
            cues: Some(json!({"format": "asamu-audio-cues", "version": 7})),
            ..AudioDocuments::default()
        };
        assert!(AudioLibrary::from_documents(Path::new("x"), bad).is_err());
        let unsafe_path = AudioDocuments {
            manifest: Some(json!({"format": "asamu-audio-manifest", "version": 1,
                "waves": {"W": {"file": "../../etc/passwd"}}})),
            ..AudioDocuments::default()
        };
        assert!(AudioLibrary::from_documents(Path::new("x"), unsafe_path).is_err());
        let lib = AudioLibrary::from_documents(Path::new("x"), AudioDocuments::default()).unwrap();
        assert!(!lib.warnings.is_empty());
    }

    #[test]
    fn ogg_sniffing() {
        let mut page = Vec::new();
        page.extend_from_slice(b"OggS");
        page.extend_from_slice(&[0u8; 22]);
        page.push(1); // one segment
        page.push(30);
        page.extend_from_slice(b"\x01vorbis");
        assert!(looks_like_ogg_vorbis(&page));
        assert!(!looks_like_ogg_vorbis(b"RIFF....WAVE"));
        assert!(!looks_like_ogg_vorbis(&page[..30]));
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("waves")).unwrap();
        let stream = vorbis_stream(ident_header(1, 0xB8));
        std::fs::write(dir.path().join("waves/W.ogg"), &stream).unwrap();
        std::fs::write(dir.path().join("waves/X.ogg"), b"junk").unwrap();
        // A first page that sniffs as Vorbis but is not a whole stream.
        std::fs::write(dir.path().join("waves/Y.ogg"), &page).unwrap();
        let w = wave_info("W", 1.0, 1);
        assert_eq!(read_wave_file(dir.path(), &w).unwrap(), stream);
        let x = wave_info("X", 1.0, 1);
        assert!(read_wave_file(dir.path(), &x).is_err());
        let y = wave_info("Y", 1.0, 1);
        assert!(read_wave_file(dir.path(), &y).is_err());
        let none = WaveInfo { file: None, ..w };
        assert!(read_wave_file(dir.path(), &none).is_err());
    }

    #[test]
    fn material_sounds_remember_the_last_material() {
        let mut m = MaterialSounds::default();
        assert_eq!(m.footstep(None), Some("FootSteps.Rock.Footsteps_Rock_Cue"));
        assert_eq!(
            m.footstep(Some("ASAMU_Ice")),
            Some("FootSteps.Ice.Footsteps_Ice_Cue")
        );
        assert_eq!(
            m.footstep(Some("Unknown")),
            Some("FootSteps.Ice.Footsteps_Ice_Cue")
        );
        assert_eq!(m.land(None, false), Some("Land.Rock.Land_Rock_Cue"));
        assert_eq!(
            m.land(None, true),
            Some("Land.HardLanding.HardLanding_Rock_Grass_Cue")
        );
        // The shared landing index: index 9 exists only in LandingSounds.
        assert!(m.land(Some("Mud"), false).is_some());
        assert_eq!(m.land(None, true), None);
        assert_eq!(m.jump(None), Some("Jump.Rock.Jump_Rock_Cue"));
    }

    /// Real converted data (`ASAMU_CONVERTED_DIR`, the output of
    /// `asamu-import --out <dir> audio`); skips when absent.
    #[test]
    fn real_converted_audio_evaluates() {
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
        assert!(lib.cues.len() > 500, "{} cues", lib.cues.len());
        // Every converted audio file passes the container validation the
        // runtime applies before decoding (files are absent after
        // `--no-audio`).
        let mut files = 0;
        for w in lib.waves.values().filter(|w| w.file.is_some()) {
            let path = audio.join(w.file.as_deref().unwrap_or_default());
            if path.is_file() {
                read_wave_file(&audio, w).unwrap();
                files += 1;
            }
        }
        eprintln!("{files} audio files validated");
        // Every cue reference of the class defaults exists.
        use gameplay_cues as g;
        let mut referenced = vec![
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
        for t in [
            g::FOOTSTEP_SOUNDS,
            g::JUMPING_SOUNDS,
            g::LANDING_SOUNDS,
            g::FALLING_LAND_SOUNDS,
        ] {
            referenced.extend(
                t.iter()
                    .filter(|(m, _)| m.starts_with("ASAMU_"))
                    .map(|(_, c)| *c),
            );
        }
        let missing: Vec<&&str> = referenced.iter().filter(|c| lib.cue(c).is_none()).collect();
        assert!(missing.is_empty(), "missing cues {missing:?}");
        for m in [g::DEATH_SOUND_MODE, g::DEFAULT_SOUND_MODE] {
            assert!(lib.mode(m).is_some(), "mode {m}");
        }
        // Every cue evaluates without NaN or out-of-range output.
        let mut audible = 0;
        let mut subtitled = 0;
        for (i, path) in lib.cues.keys().enumerate() {
            let mut e = AudioEngine::new(i as u32);
            let mut p = PlayParams::at(Vec3::new(300.0, 0.0, 0.0));
            p.subtitle_priority = SUBTITLE_PRIORITY_SCRIPTED;
            if e.play(&lib, path, p, Vec3::ZERO).is_none() {
                continue;
            }
            let mut any = false;
            for _ in 0..80 {
                let f = e.update(&lib, Vec3::ZERO, 0.05);
                for v in &f.voices {
                    assert!((0.0..=1.0).contains(&v.gain), "{path}: gain {}", v.gain);
                    assert!((MIN_PITCH..=MAX_PITCH).contains(&v.pitch), "{path}");
                    assert!(lib.wave(&v.wave).is_some(), "{path}: wave {}", v.wave);
                    any = true;
                }
                if f.subtitle.is_some() {
                    subtitled += 1;
                }
            }
            if any {
                audible += 1;
            }
        }
        eprintln!(
            "{audible} of {} cues produced voices; {subtitled} subtitle frames",
            lib.cues.len()
        );
        assert!(audible > lib.cues.len() / 2);
        assert!(subtitled > 0);
        // Every map's ambient set starts and runs.
        for (map, actors) in &lib.ambient {
            let mut e = AudioEngine::new(7);
            let n = e.load_ambient(&lib, map, Vec3::ZERO);
            assert_eq!(n, actors.len());
            for a in actors {
                let f = e.update(&lib, a.location, 0.05);
                assert!(f.voices.len() <= MAX_CHANNELS);
            }
        }
    }

    #[test]
    fn footsteps_follow_the_bob_phase() {
        use std::f32::consts::PI;
        // Boundaries at bob = (k − π/2)·π/9.
        let b1 = (1.0 - 0.5 * PI) * PI / 9.0 + 2.0 * PI / 9.0; // k = 3
        assert!(footstep_due(b1 - 0.01, b1 + 0.01, true, 200.0));
        assert!(!footstep_due(b1 + 0.01, b1 + 0.02, true, 200.0));
        assert!(!footstep_due(b1 - 0.01, b1 + 0.01, false, 200.0));
        assert!(!footstep_due(b1 - 0.01, b1 + 0.01, true, 100.0));
    }

    // -----------------------------------------------------------------
    // Hand-computed cases (verify-audio-runtime). The random values are
    // the engine LCG's outputs worked out independently (Python, double
    // precision, rounded to f32): from state 0 the first four draws are
    // 0.198346496, 0.868303537, 0.103443742, 0.993975163; from state 1:
    // 0.600818634, 0.655015349, 0.575659037, 0.300316453; from state 7:
    // 0.0156514645, 0.375286222, 0.408950806, 0.138364196; from state 42
    // the first is 0.102176309; from state 1234 0.848965287.
    // -----------------------------------------------------------------

    #[test]
    fn lcg_draws_match_hand_computed_values() {
        let expect = [
            (
                0_u32,
                [0.198_346_5_f32, 0.868_303_5, 0.103_443_74, 0.993_975_16],
            ),
            (1, [0.600_818_6, 0.655_015_35, 0.575_659_04, 0.300_316_45]),
            (7, [0.015_651_465, 0.375_286_22, 0.408_950_8, 0.138_364_2]),
        ];
        for (seed, values) in expect {
            let mut r = UeRand::new(seed);
            for v in values {
                assert!((r.frand() - v).abs() < 1e-6, "seed {seed}");
            }
        }
    }

    #[test]
    fn attenuation_curves_match_hand_computed_values() {
        use DistanceModel::*;
        // (model, distance, min, max, dB, expected) worked out in double
        // precision from the formulas read in the executable.
        let table = [
            (Linear, 750.0, 500.0, 1500.0, -60.0, 0.75),
            (Logarithmic, 300.0, 45.0, 400.0, -60.0, 0.131_674_2),
            (Logarithmic, 100.0, 0.0, 400.0, -60.0, 0.346_573_6),
            (Logarithmic, 10.0, 0.001, 400.0, -60.0, 0.285_976_94),
            (Inverse, 1000.0, 250.0, 2000.0, -60.0, 0.32),
            (Inverse, 1500.0, 0.0, 2000.0, -60.0, 0.026_666_667),
            (LogReverse, 300.0, 100.0, 1000.0, -60.0, 0.845_098),
            (LogReverse, 900.0, 100.0, 1000.0, -60.0, 0.0),
            (LogReverse, 500.0, 0.0, 1000.0, -60.0, 0.826_713_2),
            (NaturalSound, 300.0, 1.0, 1500.0, -60.0, 0.252_116_38),
            (NaturalSound, 1000.0, 200.0, 1000.5, -30.0, 0.031_691_07),
            (Other, 999.0, 100.0, 1000.0, -60.0, 1.0),
        ];
        for (m, d, min, max, db, e) in table {
            let v = attenuation_eval(m, d, min, max, db);
            assert!((v - e).abs() < 2e-6, "{m:?} at {d}: {v} vs {e}");
        }
        // Logarithmic is capped at 1: with min 0, −0.25·ln(1/1000) = 1.73.
        assert_eq!(attenuation_eval(Logarithmic, 1.0, 0.0, 1000.0, -60.0), 1.0);
    }

    fn pick_of(lib: &AudioLibrary, seed: u32) -> Option<String> {
        let mut e = AudioEngine::new(seed);
        play_once(lib, &mut e)
    }

    #[test]
    fn random_with_replacement_hand_computed_choices() {
        // choice = sum · f; walk the inputs subtracting their weights.
        // Weights [1, 3]: seed 0 → 0.793 (A), seed 1 → 2.403 (B),
        // seed 42 → 0.409 (A), seed 1234 → 3.396 (B).
        let c = random_cue(false, json!([1.0, 3.0]), &[Some("A"), Some("B")]);
        let lib = lib_with(vec![c], &[("A", 1.0, 1), ("B", 1.0, 1)]);
        assert_eq!(pick_of(&lib, 0).as_deref(), Some("A"));
        assert_eq!(pick_of(&lib, 1).as_deref(), Some("B"));
        assert_eq!(pick_of(&lib, 42).as_deref(), Some("A"));
        assert_eq!(pick_of(&lib, 1234).as_deref(), Some("B"));
    }

    #[test]
    fn random_without_replacement_hand_computed_sequence() {
        // From state 0: 3·0.198 → A; then the sum of unused weights is 2:
        // 2·0.868 = 1.737 → minus A's 1 → 0.737 → B; then 1·0.103 →
        // A and B are used but still subtracted → D, and the list resets
        // keeping D; then 2·0.994 = 1.988 → minus 1 → 0.988 → B.
        let c = random_cue(
            true,
            json!([1.0, 1.0, 1.0]),
            &[Some("A"), Some("B"), Some("D")],
        );
        let lib = lib_with(vec![c], &[("A", 1.0, 1), ("B", 1.0, 1), ("D", 1.0, 1)]);
        let mut e = AudioEngine::new(0);
        let picks: Vec<String> = (0..4).filter_map(|_| play_once(&lib, &mut e)).collect();
        assert_eq!(picks, vec!["A", "B", "D", "B"]);
    }

    #[test]
    fn modulator_hand_computed_values() {
        // State 7: volume = 0.01565·(0.2 − 0.4) + 0.4 = 0.396870;
        // pitch = 0.37529·(0.5 − 1.5) + 1.5 = 1.124714.
        let c = cue(
            "C",
            "M",
            vec![
                node(
                    "M",
                    "Modulator",
                    &[Some("W")],
                    json!({"PitchMin": 0.5, "PitchMax": 1.5,
                     "VolumeMin": 0.2, "VolumeMax": 0.4}),
                ),
                wave_node("W"),
            ],
        );
        let lib = lib_with(vec![c], &[("W", 10.0, 1)]);
        let mut e = AudioEngine::new(7);
        e.play(&lib, "C", PlayParams::two_d(), Vec3::ZERO).unwrap();
        let v = voices_of(&lib, &mut e, 0.0);
        assert!((v[0].gain - 0.396_870_7).abs() < 1e-5, "{}", v[0].gain);
        assert!((v[0].pitch - 1.124_713_8).abs() < 1e-5, "{}", v[0].pitch);
    }

    fn finite_loop_lib() -> AudioLibrary {
        let c = cue(
            "C",
            "L",
            vec![
                node(
                    "L",
                    "Looping",
                    &[Some("W")],
                    json!({"bLoopIndefinitely": false,
                     "LoopCountMin": 1.0, "LoopCountMax": 3.0}),
                ),
                wave_node("W"),
            ],
        );
        lib_with(vec![c], &[("W", 0.1, 1)])
    }

    /// Seconds a cue plays (the first update with no voice).
    fn audible_seconds(lib: &AudioLibrary, e: &mut AudioEngine, dt: f32) -> f32 {
        let mut t = 0.0;
        for _ in 0..1000 {
            if voices_of(lib, e, dt).is_empty() {
                return t;
            }
            t += dt;
        }
        t
    }

    #[test]
    fn looping_count_hand_computed() {
        // count = trunc(f·(1 − 3) + 3): state 0 → trunc(2.603) = 2 (three
        // plays, 0.3 s); state 1 → trunc(1.798) = 1 (two plays, 0.2 s).
        let lib = finite_loop_lib();
        for (seed, seconds) in [(0, 0.3), (1, 0.2)] {
            let mut e = AudioEngine::new(seed);
            let id = e.play(&lib, "C", PlayParams::two_d(), Vec3::ZERO).unwrap();
            let t = audible_seconds(&lib, &mut e, 0.01);
            assert!((t - seconds).abs() < 0.025, "seed {seed}: {t} s");
            assert!(!e.is_playing(id));
        }
    }

    #[test]
    fn delay_hand_computed() {
        // State 0: delay = 0.19835·(0.2 − 0.6) + 0.6 = 0.520661 s from the
        // first parse (playback time 0.05): first voice at t = 0.6, the
        // 12th update of 0.05 s.
        let c = cue(
            "C",
            "D",
            vec![
                node(
                    "D",
                    "Delay",
                    &[Some("W")],
                    json!({"DelayMin": 0.2, "DelayMax": 0.6}),
                ),
                wave_node("W"),
            ],
        );
        let lib = lib_with(vec![c], &[("W", 1.0, 1)]);
        let mut e = AudioEngine::new(0);
        e.play(&lib, "C", PlayParams::two_d(), Vec3::ZERO).unwrap();
        let first = (1..=20).find(|_| !voices_of(&lib, &mut e, 0.05).is_empty());
        assert_eq!(first, Some(12));
    }

    #[test]
    fn ambient_non_loop_slot_hand_computed() {
        // Draws: volume, pitch, delay, then the slot: cumulative weight
        // [1, 3, 4] ≥ 4·f. State 7: 0.553 → slot 0; state 1: 1.201 →
        // slot 1; state 0: 3.976 → slot 2.
        let slots = json!([
            {"Wave": "W0", "PitchScale": 1.0, "VolumeScale": 1.0, "Weight": 1.0},
            {"Wave": "W1", "PitchScale": 1.0, "VolumeScale": 1.0, "Weight": 2.0},
            {"Wave": "W2", "PitchScale": 1.0, "VolumeScale": 1.0, "Weight": 1.0}]);
        let c = cue(
            "C",
            "AMB",
            vec![node(
                "AMB",
                "AmbientNonLoop",
                &[],
                json!({"bAttenuate": false, "VolumeMin": 1.0, "VolumeMax": 1.0,
                       "PitchMin": 1.0, "PitchMax": 1.0, "DelayMin": 0.0,
                       "DelayMax": 0.0, "SoundSlots": slots}),
            )],
        );
        let lib = lib_with(vec![c], &[("W0", 1.0, 1), ("W1", 1.0, 1), ("W2", 1.0, 1)]);
        for (seed, wave) in [(7, "W0"), (1, "W1"), (0, "W2")] {
            let mut e = AudioEngine::new(seed);
            e.play(&lib, "C", PlayParams::two_d(), Vec3::ZERO).unwrap();
            let v = voices_of(&lib, &mut e, 0.01);
            assert_eq!(v.len(), 1, "seed {seed}");
            assert_eq!(v[0].wave, wave, "seed {seed}");
        }
    }

    #[test]
    fn distance_cross_fade_node_in_a_graph() {
        // Input 0 fades in over 100..200 (volume 0.8), input 1 is full
        // between 400 and 500; the listener 150 UU away hears input 0 at
        // 0.4 and not input 1. The node uses the distance even for a
        // non-spatialised component.
        let c = cue(
            "C",
            "X",
            vec![
                node(
                    "X",
                    "DistanceCrossFade",
                    &[Some("A"), Some("B")],
                    json!({"CrossFadeInput": [
                        {"FadeInDistanceStart": 100.0, "FadeInDistanceEnd": 200.0,
                         "FadeOutDistanceStart": 300.0, "FadeOutDistanceEnd": 350.0,
                         "Volume": 0.8},
                        {"FadeInDistanceStart": 300.0, "FadeInDistanceEnd": 400.0,
                         "FadeOutDistanceStart": 500.0, "FadeOutDistanceEnd": 600.0,
                         "Volume": 1.0}]}),
                ),
                wave_node("A"),
                wave_node("B"),
            ],
        );
        assert_eq!(c.max_audible_distance(), 600.0);
        let lib = lib_with(vec![c], &[("A", 5.0, 1), ("B", 5.0, 1)]);
        let mut e = AudioEngine::new(1);
        let mut p = PlayParams::two_d();
        p.location = Vec3::new(0.0, 150.0, 0.0);
        e.play(&lib, "C", p, Vec3::ZERO).unwrap();
        let v = voices_of(&lib, &mut e, 0.0);
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].wave, "A");
        assert!((v[0].gain - 0.4).abs() < 1e-6);
    }

    #[test]
    fn max_audible_distance_follows_the_node_functions() {
        let att = json!({"RadiusMin": 10.0, "RadiusMax": 1234.0});
        let c = cue(
            "C",
            "T",
            vec![
                node("T", "Attenuation", &[Some("W")], att.clone()),
                wave_node("W"),
            ],
        );
        assert_eq!(c.max_audible_distance(), 1234.0);
        // A looping node makes the cue audible everywhere (WORLD_MAX).
        let l = cue(
            "L",
            "T",
            vec![
                node("T", "Attenuation", &[Some("LP")], att),
                node("LP", "Looping", &[Some("W")], json!({})),
                wave_node("W"),
            ],
        );
        assert_eq!(l.max_audible_distance(), WORLD_MAX_AUDIBLE_DISTANCE);
        // So a looping world sound starts however far the listener is.
        let lib = lib_with(vec![c, l], &[("W", 1.0, 1)]);
        let mut e = AudioEngine::new(1);
        let mut p = PlayParams::at(Vec3::new(5000.0, 0.0, 0.0));
        p.check_audible = true;
        assert!(e.play(&lib, "C", p.clone(), Vec3::ZERO).is_none());
        assert!(e.play(&lib, "L", p, Vec3::ZERO).is_some());
    }

    fn timed_narration_lib() -> AudioLibrary {
        let mut a = cue("A1", "WA1", vec![wave_node("WA1")]);
        a.duration = Some(1.0);
        let mut b = cue("B1", "WB1", vec![wave_node("WB1")]);
        b.duration = Some(1.0);
        lib_with(vec![a, b], &[("WA1", 1.0, 1), ("WB1", 1.0, 1)])
    }

    #[test]
    fn narrator_timers_fire_strictly_after_their_rate() {
        // dt = 0.25 (exact in binary). The end timer of A (rate 1) fires
        // when its count exceeds 1: the 5th update. The delayed start of B
        // (rate 0.5) is set while firing and not advanced in that update:
        // it fires in the 8th update.
        let lib = timed_narration_lib();
        let mut e = AudioEngine::new(1);
        e.apply(&lib, &add("a", "A1", 0.0), Vec3::ZERO);
        e.apply(&lib, &add("b", "B1", 0.5), Vec3::ZERO);
        let mut finished_at = None;
        let mut b_at = None;
        for k in 1..=12 {
            let f = e.update(&lib, Vec3::ZERO, 0.25);
            if f.feedback
                .contains(&AudioFeedback::NarratorLineFinished { id: "a".into() })
            {
                finished_at = Some(k);
            }
            if b_at.is_none() && f.voices.iter().any(|v| v.wave == "WB1") {
                b_at = Some(k);
            }
        }
        assert_eq!(finished_at, Some(5));
        assert_eq!(b_at, Some(8));
    }

    #[test]
    fn narrator_remove_takes_every_equal_line() {
        let lib = timed_narration_lib();
        let mut e = AudioEngine::new(1);
        let line = |id: &str, node: u64| AudioCommand::NarratorAddLine {
            id: id.into(),
            cue: "B1".into(),
            volume: 1.0,
            delay: 1.0,
            remove_all_others: false,
            fade_out_if_active: true,
            node: Some(node),
        };
        e.apply(&lib, &add("a", "A1", 1.0), Vec3::ZERO);
        // The same Kismet action twice: two equal lines.
        e.apply(&lib, &line("b", 2), Vec3::ZERO);
        e.apply(&lib, &line("b", 2), Vec3::ZERO);
        // Same id from two actions: different lines.
        e.apply(&lib, &line("c", 3), Vec3::ZERO);
        e.apply(&lib, &line("c", 4), Vec3::ZERO);
        assert_eq!(e.narrator().queue(), vec!["a", "b", "b", "c", "c"]);
        let remove = |id: &str| AudioCommand::NarratorRemoveLine {
            id: id.into(),
            stop_if_active: true,
            fade_time: 0.2,
        };
        e.apply(&lib, &remove("b"), Vec3::ZERO);
        assert_eq!(e.narrator().queue(), vec!["a", "c", "c"]);
        e.apply(&lib, &remove("c"), Vec3::ZERO);
        assert_eq!(e.narrator().queue(), vec!["a", "c"]);
        // Removing a queued (not playing) line leaves the sound alone.
        let f = e.update(&lib, Vec3::ZERO, 0.01);
        assert_eq!(f.voices.len(), 1);
    }

    #[test]
    fn external_narrator_play_and_stop() {
        let lib = narration_lib();
        let mut e = AudioEngine::new(1);
        e.apply(
            &lib,
            &AudioCommand::NarratorPlay {
                id: "x".into(),
                cue: "A".into(),
                volume: 0.5,
            },
            Vec3::ZERO,
        );
        let f = e.update(&lib, Vec3::ZERO, 0.01);
        assert_eq!(f.voices.len(), 1);
        assert!((f.voices[0].gain - 0.5).abs() < 1e-6);
        assert!(!f.voices[0].spatial);
        assert_eq!(f.subtitle.as_deref(), Some("hello"));
        assert!(f.feedback.is_empty(), "no queue, no narrator events");
        // A second line restarts the narrator component.
        e.apply(
            &lib,
            &AudioCommand::NarratorPlay {
                id: "y".into(),
                cue: "B".into(),
                volume: 1.0,
            },
            Vec3::ZERO,
        );
        let f = e.update(&lib, Vec3::ZERO, 0.01);
        assert_eq!(f.voices.len(), 1);
        assert_eq!(f.voices[0].wave, "WB");
        assert_eq!(f.subtitle, None, "the stopped component's lines are killed");
        e.apply(
            &lib,
            &AudioCommand::NarratorStop { id: "y".into() },
            Vec3::ZERO,
        );
        assert!(e.update(&lib, Vec3::ZERO, 0.01).voices.is_empty());
        assert_eq!(e.instance_count(), 0);
    }

    #[test]
    fn stop_sound_by_kismet_node() {
        let lib = narration_lib();
        let mut e = AudioEngine::new(1);
        let play = |node: u64| AudioCommand::PlaySound {
            cue: "A".into(),
            source: SoundSource::TwoD,
            volume_multiplier: 1.0,
            pitch_multiplier: 1.0,
            fade_in_time: 0.0,
            suppress_subtitles: true,
            suppress_spatialization: false,
            node: Some(node),
        };
        e.apply(&lib, &play(1), Vec3::ZERO);
        e.apply(&lib, &play(2), Vec3::ZERO);
        assert_eq!(e.update(&lib, Vec3::ZERO, 0.01).voices.len(), 2);
        e.apply(
            &lib,
            &AudioCommand::StopSound {
                cue: String::new(),
                fade_out_time: 0.0,
                node: Some(1),
            },
            Vec3::ZERO,
        );
        assert_eq!(e.update(&lib, Vec3::ZERO, 0.01).voices.len(), 1);
    }

    fn subtitle_lib(lines: &[(f32, &str)], duration: f32) -> AudioLibrary {
        let mut c = cue("S", "WS", vec![wave_node("WS")]);
        c.duration = Some(duration);
        let mut lib = lib_with(vec![c], &[("WS", duration, 1)]);
        lib.subtitles.insert(
            "WS".into(),
            SubtitleTrack {
                lines: lines
                    .iter()
                    .map(|(t, s)| SubtitleLine {
                        text: (*s).into(),
                        time: *t,
                    })
                    .collect(),
                duration: Some(duration),
                ..SubtitleTrack::default()
            },
        );
        lib
    }

    #[test]
    fn subtitles_follow_the_audio_clock_and_pause_with_the_game() {
        let lib = subtitle_lib(&[(0.0, "one"), (1.0, "two")], 2.0);
        let mut e = AudioEngine::new(1);
        let mut p = PlayParams::two_d();
        p.subtitle_priority = SUBTITLE_PRIORITY_SCRIPTED;
        e.play(&lib, "S", p, Vec3::ZERO).unwrap();
        assert_eq!(
            e.update(&lib, Vec3::ZERO, 0.0).subtitle.as_deref(),
            Some("one")
        );
        assert_eq!(
            e.update(&lib, Vec3::ZERO, 0.5).subtitle.as_deref(),
            Some("one")
        );
        e.set_paused(true);
        // Real time passes while paused; the audio clock does not.
        assert_eq!(
            e.update(&lib, Vec3::ZERO, 1.0).subtitle.as_deref(),
            Some("one")
        );
        e.set_paused(false);
        assert_eq!(
            e.update(&lib, Vec3::ZERO, 0.6).subtitle.as_deref(),
            Some("two")
        );
        // The wave ends at audio time 2.0.
        assert_eq!(e.update(&lib, Vec3::ZERO, 1.0).subtitle, None);
    }

    #[test]
    fn subtitle_cursor_and_priority_follow_the_engine() {
        let line = |t: f32, s: &str| SubtitleLine {
            text: s.into(),
            time: t,
        };
        // Lines out of time order: the cursor moves on whenever the next
        // line has started, so "b" (followed by an earlier line) is
        // skipped and "a" shows until 2.0.
        let mut m = SubtitleManager::default();
        m.queue(
            InstanceId(1),
            1.0,
            3.0,
            &[line(0.0, "a"), line(2.0, "b"), line(1.0, "c")],
            0.0,
        );
        assert_eq!(m.current(0.5).as_deref(), Some("a"));
        assert_eq!(m.current(1.5).as_deref(), Some("a"));
        assert_eq!(m.current(2.5).as_deref(), Some("c"));
        assert_eq!(m.current(3.0), None);
        // The highest priority among entries whose line has started: a
        // louder entry that has not started yet does not hide another.
        let mut m = SubtitleManager::default();
        m.queue(InstanceId(1), 10.0, 5.0, &[line(1.0, "late")], 0.0);
        m.queue(InstanceId(2), 5.0, 5.0, &[line(0.0, "now")], 0.0);
        assert_eq!(m.current(0.5).as_deref(), Some("now"));
        assert_eq!(m.current(1.5).as_deref(), Some("late"));
        // Lines later than the wave are clamped to its end; priority 0 or
        // duration 0 queue nothing.
        let mut m = SubtitleManager::default();
        m.queue(
            InstanceId(3),
            1.0,
            1.0,
            &[line(0.0, "x"), line(9.0, "y")],
            0.0,
        );
        assert_eq!(m.current(0.99).as_deref(), Some("x"));
        assert_eq!(m.current(1.0), None);
        m.queue(InstanceId(4), 0.0, 1.0, &[line(0.0, "z")], 0.0);
        m.queue(InstanceId(5), 1.0, 0.0, &[line(0.0, "z")], 0.0);
        assert_eq!(m.current(0.5), None);
    }

    #[test]
    fn subtitles_use_the_wave_duration_not_the_pitch() {
        // At pitch 0.5 the 2 s wave plays for 4 s, but the engine queues
        // the wave's `Duration`: the line disappears after 2 s.
        let mut lib = subtitle_lib(&[(0.0, "one")], 2.0);
        if let Some(c) = lib.cues.get_mut("S") {
            let mut c2 = (**c).clone();
            c2.pitch_multiplier = 0.5;
            c2.duration = Some(4.0);
            *c = Arc::new(c2);
        }
        let mut e = AudioEngine::new(1);
        let mut p = PlayParams::two_d();
        p.subtitle_priority = 1.0;
        e.play(&lib, "S", p, Vec3::ZERO).unwrap();
        assert_eq!(
            e.update(&lib, Vec3::ZERO, 0.0).subtitle.as_deref(),
            Some("one")
        );
        assert_eq!(
            e.update(&lib, Vec3::ZERO, 1.9).subtitle.as_deref(),
            Some("one")
        );
        let f = e.update(&lib, Vec3::ZERO, 0.6);
        assert_eq!(f.voices.len(), 1, "still playing at 2.5 s");
        assert_eq!(f.subtitle, None);
    }

    #[test]
    fn unknown_sound_mode_changes_nothing() {
        let docs = AudioDocuments {
            sound_classes: Some(classes_doc()),
            ..AudioDocuments::default()
        };
        let lib = AudioLibrary::from_documents(Path::new("x"), docs).unwrap();
        let mut m = SoundModeState::default();
        m.set(&lib, Some("Duck"), 0.0);
        m.update_with(&lib, 1.0);
        assert_eq!(m.current(), Some("Duck"));
        m.set(&lib, Some("NoSuchMode"), 1.0);
        assert_eq!(m.current(), Some("Duck"));
        assert_eq!(m.class("Game").unwrap().volume, 0.0);
    }

    #[test]
    fn fade_in_on_a_playing_component_restarts_without_the_fade() {
        let c = cue("C", "W", vec![wave_node("W")]);
        let lib = lib_with(vec![c], &[("W", 10.0, 1)]);
        let mut e = AudioEngine::new(1);
        // A stopped (or new) component fades in.
        let id = e
            .fade_in(&lib, None, "C", PlayParams::two_d(), (1.0, 1.0), Vec3::ZERO)
            .unwrap();
        let v = voices_of(&lib, &mut e, 0.25);
        assert!((v[0].gain - 0.25).abs() < 1e-6);
        // FadeIn on the playing component: `Play` restarts it and resets
        // the fade, so it is at full volume at once.
        let id2 = e
            .fade_in(
                &lib,
                Some(id),
                "C",
                PlayParams::two_d(),
                (1.0, 1.0),
                Vec3::ZERO,
            )
            .unwrap();
        assert_ne!(id, id2);
        let v = voices_of(&lib, &mut e, 0.1);
        assert_eq!(v.len(), 1);
        assert!((v[0].gain - 1.0).abs() < 1e-6);
        // While fading out, FadeIn reverses on the same component.
        e.fade_out(id2, 1.0, 0.0);
        let v = voices_of(&lib, &mut e, 0.5);
        assert!((v[0].gain - 0.5).abs() < 1e-6);
        let id3 = e
            .fade_in(
                &lib,
                Some(id2),
                "C",
                PlayParams::two_d(),
                (1.0, 1.0),
                Vec3::ZERO,
            )
            .unwrap();
        assert_eq!(id3, id2);
        let v = voices_of(&lib, &mut e, 0.25);
        assert!((v[0].gain - 0.75).abs() < 1e-5, "{}", v[0].gain);
    }

    #[test]
    fn waves_without_a_channel_finish_unless_their_component_remains_active() {
        let mut c = cue("C", "W", vec![wave_node("W")]);
        c.max_concurrent_play_count = 0;
        let lib = lib_with(vec![c], &[("W", 0.1, 1)]);
        for remain in [false, true] {
            let mut e = AudioEngine::new(1);
            let mut ids = Vec::new();
            for _ in 0..=MAX_CHANNELS {
                let mut p = PlayParams::two_d();
                p.remain_active_if_dropped = remain;
                ids.push(e.play(&lib, "C", p, Vec3::ZERO).unwrap());
            }
            let last = *ids.last().unwrap();
            // Equal priorities: the earliest components win the channels.
            let v = voices_of(&lib, &mut e, 0.0);
            assert_eq!(v.len(), MAX_CHANNELS);
            assert!(v.iter().all(|v| v.instance != last));
            let mut heard_last = false;
            for _ in 0..30 {
                heard_last |= voices_of(&lib, &mut e, 0.01)
                    .iter()
                    .any(|v| v.instance == last);
            }
            assert_eq!(heard_last, remain, "remain active: {remain}");
        }
    }

    #[test]
    fn a_wave_that_loses_its_source_finishes() {
        // A one-shot heard 500 UU away; the listener leaves its radius, the
        // source stops and the wave is finished: it does not resume.
        let att = json!({"bAttenuate": true, "DistanceAlgorithm": "ATTENUATION_Linear",
                         "RadiusMin": 0.0, "RadiusMax": 1000.0});
        let c = cue(
            "C",
            "T",
            vec![node("T", "Attenuation", &[Some("W")], att), wave_node("W")],
        );
        let lib = lib_with(vec![c], &[("W", 5.0, 1)]);
        let mut e = AudioEngine::new(1);
        let id = e
            .play(
                &lib,
                "C",
                PlayParams::at(Vec3::new(500.0, 0.0, 0.0)),
                Vec3::ZERO,
            )
            .unwrap();
        assert_eq!(e.update(&lib, Vec3::ZERO, 0.1).voices.len(), 1);
        assert!(
            e.update(&lib, Vec3::new(3000.0, 0.0, 0.0), 0.1)
                .voices
                .is_empty()
        );
        let _ = e.update(&lib, Vec3::ZERO, 0.1);
        assert!(e.update(&lib, Vec3::ZERO, 0.1).voices.is_empty());
        assert!(!e.is_playing(id));
    }

    #[test]
    fn shared_node_chains_are_bounded() {
        // 40 mixers, each with both inputs on the next one: 2^40 paths.
        let mut nodes = Vec::new();
        for i in 0..40 {
            let next = format!("M{}", i + 1);
            nodes.push(node(
                &format!("M{i}"),
                "Mixer",
                &[Some(next.as_str()), Some(next.as_str())],
                json!({"InputVolume": [1.0, 1.0]}),
            ));
        }
        nodes.push(wave_node("M40"));
        let c = cue("C", "M0", nodes);
        let lib = lib_with(vec![c], &[("M40", 1.0, 1)]);
        let mut e = AudioEngine::new(1);
        e.play(&lib, "C", PlayParams::two_d(), Vec3::ZERO).unwrap();
        let start = std::time::Instant::now();
        let f = e.update(&lib, Vec3::ZERO, 0.01);
        assert!(f.voices.len() <= MAX_CHANNELS);
        assert!(start.elapsed().as_secs() < 5);
    }

    /// Ogg CRC computed bit by bit (independent of the table version).
    fn bitwise_ogg_crc(page: &[u8]) -> u32 {
        let mut crc = 0_u32;
        for &b in page {
            crc ^= u32::from(b) << 24;
            for _ in 0..8 {
                crc = if crc & 0x8000_0000 != 0 {
                    (crc << 1) ^ 0x04C1_1DB7
                } else {
                    crc << 1
                };
            }
        }
        crc
    }

    fn ogg_page(flags: u8, seq: u32, packets: &[Vec<u8>]) -> Vec<u8> {
        let mut table = Vec::new();
        let mut body = Vec::new();
        for p in packets {
            let mut n = p.len();
            while n >= 255 {
                table.push(255_u8);
                n -= 255;
            }
            table.push(n as u8);
            body.extend_from_slice(p);
        }
        let mut page = b"OggS".to_vec();
        page.push(0);
        page.push(flags);
        page.extend_from_slice(&0_u64.to_le_bytes());
        page.extend_from_slice(&7_u32.to_le_bytes());
        page.extend_from_slice(&seq.to_le_bytes());
        page.extend_from_slice(&[0; 4]);
        page.push(table.len() as u8);
        page.extend(table);
        page.extend(body);
        let crc = bitwise_ogg_crc(&page);
        page[22..26].copy_from_slice(&crc.to_le_bytes());
        page
    }

    fn ident_header(channels: u8, blocks: u8) -> Vec<u8> {
        let mut h = b"\x01vorbis".to_vec();
        h.extend_from_slice(&0_u32.to_le_bytes());
        h.push(channels);
        h.extend_from_slice(&44_100_u32.to_le_bytes());
        h.extend_from_slice(&[0; 12]);
        h.push(blocks);
        h.push(1);
        h
    }

    fn vorbis_stream(ident: Vec<u8>) -> Vec<u8> {
        let mut s = ogg_page(0x02, 0, &[ident]);
        // A setup header longer than one segment (lacing over 255).
        let mut setup = b"\x05vorbis".to_vec();
        setup.extend(std::iter::repeat_n(0xAA_u8, 600));
        s.extend(ogg_page(0, 1, &[b"\x03vorbis-comment".to_vec(), setup]));
        s.extend(ogg_page(0x04, 2, &[vec![0_u8; 40]]));
        s
    }

    #[test]
    fn ogg_vorbis_validation() {
        let good = vorbis_stream(ident_header(2, 0xB8));
        assert_eq!(validate_ogg_vorbis(&good), Ok(()));
        // Every truncation fails.
        for cut in [0, 10, 27, 40, good.len() / 2, good.len() - 1] {
            assert!(validate_ogg_vorbis(&good[..cut]).is_err(), "cut at {cut}");
        }
        // Any flipped byte fails (checksum or structure).
        for i in (0..good.len()).step_by(7) {
            let mut bad = good.clone();
            bad[i] ^= 0x20;
            assert!(validate_ogg_vorbis(&bad).is_err(), "flip at {i}");
        }
        // Trailing bytes, a missing EOS, invalid identification headers.
        let mut trailing = good.clone();
        trailing.extend_from_slice(b"junk");
        assert!(validate_ogg_vorbis(&trailing).is_err());
        let mut no_eos = ogg_page(0x02, 0, &[ident_header(2, 0xB8)]);
        no_eos.extend(ogg_page(
            0,
            1,
            &[b"\x03vorbis".to_vec(), b"\x05vorbis".to_vec()],
        ));
        assert!(validate_ogg_vorbis(&no_eos).is_err());
        assert!(validate_ogg_vorbis(&vorbis_stream(ident_header(0, 0xB8))).is_err());
        assert!(validate_ogg_vorbis(&vorbis_stream(ident_header(1, 0x8B))).is_err());
        assert!(validate_ogg_vorbis(&vorbis_stream(ident_header(1, 0xE5))).is_err());
        // The table CRC equals the bitwise one on a real-looking page.
        let page = ogg_page(0x02, 0, &[ident_header(1, 0xB8)]);
        let stored = u32::from_le_bytes([page[22], page[23], page[24], page[25]]);
        let mut zeroed = page.clone();
        zeroed[22..26].copy_from_slice(&[0; 4]);
        assert_eq!(stored, bitwise_ogg_crc(&zeroed));
    }
}
