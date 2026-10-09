//! Sound data of package version 868: `SoundNodeWave` (properties,
//! subtitles, native bulk payloads), `SoundCue` node graphs and the
//! `SoundNode*` classes, `SoundClass` / `SoundMode`, ambient sound actors and
//! reverb volumes of maps, plus an Ogg/Vorbis container checker and a WAV
//! writer (see `docs/reverse-engineering/AUDIO.md`).
//!
//! Native layouts (CONFIRMED by exact consumption of every export of these
//! classes in the macOS build):
//!
//! ```text
//! SoundNodeWave  tagged properties
//!                FByteBulkData RawData               (always empty when cooked)
//!                FByteBulkData CompressedPCData      (Ogg Vorbis stream, inline)
//!                FByteBulkData CompressedXbox360Data (always empty)
//!                FByteBulkData CompressedPS3Data     (always empty)
//!                FByteBulkData CompressedWiiUData    (always empty)
//!                FByteBulkData CompressedIPhoneData  (always empty)
//!                FByteBulkData CompressedFlashData   (always empty)
//! SoundCue       tagged properties
//!                TMap<SoundNode, SoundNodeEditorData> EditorData:
//!                    i32 Count, Count x { i32 Node, i32 NodePosX, i32 NodePosY }
//! SoundClass     tagged properties
//!                TMap<SoundClass, SoundClassEditorData> EditorData (same layout)
//! SoundNode*, SoundMode: tagged properties only
//! ```
//!
//! Class default objects store no native data. Every value a runtime needs is
//! a tagged property, resolved here over the object's archetype (normally its
//! class default object), the same delta rule the engine uses.
//!
//! All input is hostile: counts, sizes and offsets are checked, graphs are
//! walked with node and depth limits, and malformed data yields a
//! [`SoundError`] instead of a panic. Decoded waves, cues and subtitles are
//! original game data: keep them in user-local output only; publish counts.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};
use std::sync::Arc;

use serde::Serialize;
use thiserror::Error;

use crate::bulkdata::{self, BulkCompression, BulkDataRecord, BulkError, BulkStorage};
use crate::error::Ue3Error;
use crate::flags;
use crate::level::{self, ParamValue, merge_properties, param_map, param_value};
use crate::model::{LoadedPackage, PackageSet};
use crate::object::{self, DecodedObject, ObjectError};
use crate::property::{ObjRef, Property, Value};
use crate::reader::Reader;
use crate::schema::{Schema, last_component};
use crate::types::PackageIndex;

/// Bulk data records after the tagged properties of a `SoundNodeWave`, in
/// serialization order (= declaration order of the `UntypedBulkData_Mirror`
/// properties of `Engine.SoundNodeWave`).
pub const WAVE_BULK_SLOTS: [&str; 7] = [
    "RawData",
    "CompressedPCData",
    "CompressedXbox360Data",
    "CompressedPS3Data",
    "CompressedWiiUData",
    "CompressedIPhoneData",
    "CompressedFlashData",
];

/// Index of `CompressedPCData` in [`WAVE_BULK_SLOTS`].
pub const SLOT_COMPRESSED_PC: usize = 1;
/// Index of `RawData` in [`WAVE_BULK_SLOTS`].
pub const SLOT_RAW: usize = 0;

/// Serialized size of one `EditorData` map entry (node, x, y).
pub const EDITOR_ENTRY_SIZE: usize = 12;

/// Most nodes followed in one cue graph (real cues have at most a few dozen).
pub const MAX_CUE_NODES: usize = 4096;

/// Longest archetype chain followed when resolving values.
pub const MAX_ARCHETYPE_DEPTH: usize = 16;

/// Most Ogg pages accepted in one stream.
pub const MAX_OGG_PAGES: usize = 1 << 20;

/// Most bytes kept of the Vorbis header packets (identification, comment,
/// setup) while checking a stream.
pub const MAX_HEADER_PACKET: usize = 1 << 20;

/// Most failure samples kept per statistic.
const MAX_SAMPLES: usize = 12;

/// Errors from sound decoding.
#[derive(Debug, Error)]
pub enum SoundError {
    /// A low-level read failed.
    #[error(transparent)]
    Ue3(#[from] Ue3Error),
    /// Prelude or tagged properties failed to decode.
    #[error(transparent)]
    Object(#[from] ObjectError),
    /// A bulk data record is malformed or cannot be loaded.
    #[error(transparent)]
    Bulk(#[from] BulkError),
    /// The export is not of the expected sound class.
    #[error("export {export} ({class}) is not a {expected}")]
    WrongClass {
        /// Export index.
        export: usize,
        /// Class path.
        class: String,
        /// Expected kind.
        expected: &'static str,
    },
    /// Native data is inconsistent.
    #[error("sound native data: {0}")]
    Malformed(String),
    /// An Ogg stream is malformed.
    #[error("ogg stream: {0}")]
    Ogg(String),
    /// A WAV file cannot be written or parsed.
    #[error("wav: {0}")]
    Wav(String),
}

// ---------------------------------------------------------------------------
// Classes
// ---------------------------------------------------------------------------

/// Role of a `SoundNode` subclass.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
pub enum NodeKind {
    /// `SoundNodeWave`: a leaf holding audio.
    Wave,
    /// `SoundNodeWaveStreaming` (procedural; native layout unverified).
    WaveStreaming,
    /// `SoundNodeWaveParam`: a wave supplied at run time by parameter name.
    WaveParam,
    /// `SoundNodeAttenuation`.
    Attenuation,
    /// `SoundNodeAttenuationAndGain`.
    AttenuationAndGain,
    /// `SoundNodeRandom`.
    Random,
    /// `SoundNodeMixer`.
    Mixer,
    /// `SoundNodeModulator`.
    Modulator,
    /// `SoundNodeModulatorContinuous`.
    ModulatorContinuous,
    /// `SoundNodeLooping`.
    Looping,
    /// `SoundNodeDelay`.
    Delay,
    /// `SoundNodeConcatenator`.
    Concatenator,
    /// `SoundNodeConcatenatorRadio`.
    ConcatenatorRadio,
    /// `SoundNodeDistanceCrossFade`.
    DistanceCrossFade,
    /// `SoundNodeAmbient` (inline node of `AmbientSoundSimple` actors).
    Ambient,
    /// `SoundNodeAmbientNonLoop`.
    AmbientNonLoop,
    /// `SoundNodeAmbientNonLoopToggle`.
    AmbientNonLoopToggle,
    /// `SoundNodeDoppler`.
    Doppler,
    /// `SoundNodeEnveloper`.
    Enveloper,
    /// `SoundNodeOscillator`.
    Oscillator,
    /// `SoundNodeMature`.
    Mature,
    /// `ForcedLoopSoundNode` (used by the spline ambient sound components).
    ForcedLoop,
    /// Another `SoundNode` subclass.
    Other,
}

const NODE_CLASSES: &[(&str, NodeKind)] = &[
    ("soundnodewave", NodeKind::Wave),
    ("soundnodewavestreaming", NodeKind::WaveStreaming),
    ("soundnodewaveparam", NodeKind::WaveParam),
    ("soundnodeattenuation", NodeKind::Attenuation),
    ("soundnodeattenuationandgain", NodeKind::AttenuationAndGain),
    ("soundnoderandom", NodeKind::Random),
    ("soundnodemixer", NodeKind::Mixer),
    ("soundnodemodulator", NodeKind::Modulator),
    (
        "soundnodemodulatorcontinuous",
        NodeKind::ModulatorContinuous,
    ),
    ("soundnodelooping", NodeKind::Looping),
    ("soundnodedelay", NodeKind::Delay),
    ("soundnodeconcatenator", NodeKind::Concatenator),
    ("soundnodeconcatenatorradio", NodeKind::ConcatenatorRadio),
    ("soundnodedistancecrossfade", NodeKind::DistanceCrossFade),
    ("soundnodeambient", NodeKind::Ambient),
    ("soundnodeambientnonloop", NodeKind::AmbientNonLoop),
    (
        "soundnodeambientnonlooptoggle",
        NodeKind::AmbientNonLoopToggle,
    ),
    ("soundnodedoppler", NodeKind::Doppler),
    ("soundnodeenveloper", NodeKind::Enveloper),
    ("soundnodeoscillator", NodeKind::Oscillator),
    ("soundnodemature", NodeKind::Mature),
    ("forcedloopsoundnode", NodeKind::ForcedLoop),
    ("soundnode", NodeKind::Other),
];

impl NodeKind {
    /// Kind of the class called `name` (last path component, any case).
    pub fn from_class_name(name: &str) -> Option<NodeKind> {
        let n = last_component(name).to_ascii_lowercase();
        NODE_CLASSES.iter().find(|(c, _)| *c == n).map(|(_, k)| *k)
    }

    /// True for leaves that carry audio (`SoundNodeWave` and subclasses).
    pub fn is_wave(self) -> bool {
        matches!(self, NodeKind::Wave | NodeKind::WaveStreaming)
    }
}

/// Which sound object an export is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
pub enum SoundKind {
    /// A `SoundNode` (waves included).
    Node(NodeKind),
    /// `SoundCue`.
    Cue,
    /// `SoundClass`.
    SoundClass,
    /// `SoundMode`.
    SoundMode,
}

impl SoundKind {
    /// Kind of the class called `name` (last path component, any case).
    pub fn from_class_name(name: &str) -> Option<SoundKind> {
        let n = last_component(name).to_ascii_lowercase();
        match n.as_str() {
            "soundcue" => Some(SoundKind::Cue),
            "soundclass" => Some(SoundKind::SoundClass),
            "soundmode" => Some(SoundKind::SoundMode),
            _ => NodeKind::from_class_name(&n).map(SoundKind::Node),
        }
    }

    /// Kind of a class from its own name or, failing that, the nearest known
    /// class in `chain` (lower-case names, nearest first; see
    /// [`Schema::class_chain`]).
    pub fn classify(class_path: &str, chain: &[String]) -> Option<SoundKind> {
        SoundKind::from_class_name(class_path)
            .or_else(|| chain.iter().find_map(|c| SoundKind::from_class_name(c)))
    }
}

/// Ambient sound actor classes (`Engine.AmbientSound*`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
pub enum AmbientKind {
    /// `AmbientSound`: plays the `SoundCue` of its audio component.
    AmbientSound,
    /// `AmbientSoundMovable`.
    Movable,
    /// `AmbientSoundNonLoop`.
    NonLoop,
    /// `AmbientSoundNonLoopingToggleable`.
    NonLoopingToggleable,
    /// `AmbientSoundSimple`: plays an inline `SoundNodeAmbient`.
    Simple,
    /// `AmbientSoundSimpleSpline`.
    SimpleSpline,
    /// `AmbientSoundSimpleSplineNonLoop`.
    SimpleSplineNonLoop,
    /// `AmbientSoundSimpleToggleable`.
    SimpleToggleable,
    /// `AmbientSoundSpline`.
    Spline,
    /// `AmbientSoundSplineMultiCue`.
    SplineMultiCue,
    /// Another subclass of an ambient sound class.
    Other,
}

const AMBIENT_CLASSES: &[(&str, AmbientKind)] = &[
    ("ambientsound", AmbientKind::AmbientSound),
    ("ambientsoundmovable", AmbientKind::Movable),
    ("ambientsoundnonloop", AmbientKind::NonLoop),
    (
        "ambientsoundnonloopingtoggleable",
        AmbientKind::NonLoopingToggleable,
    ),
    ("ambientsoundsimple", AmbientKind::Simple),
    ("ambientsoundsimplespline", AmbientKind::SimpleSpline),
    (
        "ambientsoundsimplesplinenonloop",
        AmbientKind::SimpleSplineNonLoop,
    ),
    (
        "ambientsoundsimpletoggleable",
        AmbientKind::SimpleToggleable,
    ),
    ("ambientsoundspline", AmbientKind::Spline),
    ("ambientsoundsplinemulticue", AmbientKind::SplineMultiCue),
];

impl AmbientKind {
    /// Kind of the class called `name` (last path component, any case).
    pub fn from_class_name(name: &str) -> Option<AmbientKind> {
        let n = last_component(name).to_ascii_lowercase();
        AMBIENT_CLASSES
            .iter()
            .find(|(c, _)| *c == n)
            .map(|(_, k)| *k)
    }

    /// Kind from the class name, or [`AmbientKind::Other`] when an ancestor
    /// in `chain` is an ambient sound class.
    pub fn classify(class_path: &str, chain: &[String]) -> Option<AmbientKind> {
        AmbientKind::from_class_name(class_path).or_else(|| {
            chain
                .iter()
                .any(|c| AmbientKind::from_class_name(c).is_some())
                .then_some(AmbientKind::Other)
        })
    }
}

/// True for `ReverbVolume` and subclasses (by name or chain).
pub fn is_reverb_volume(class_path: &str, chain: &[String]) -> bool {
    let is = |n: &str| {
        let n = last_component(n).to_ascii_lowercase();
        n == "reverbvolume" || n == "reverbvolumetoggleable"
    };
    is(class_path) || chain.iter().any(|c| is(c))
}

// ---------------------------------------------------------------------------
// Value helpers
// ---------------------------------------------------------------------------

/// Property `name` (array index 0) in `props`.
fn prop<'a>(props: &'a [Property], name: &str) -> Option<&'a Value> {
    props
        .iter()
        .find(|p| p.array_index == 0 && p.name.eq_ignore_ascii_case(name))
        .map(|p| &p.value)
}

/// Struct member `name` of a struct value.
fn member<'a>(v: &'a Value, name: &str) -> Option<&'a Value> {
    match v {
        Value::Struct { fields, .. } => prop(fields, name),
        _ => None,
    }
}

fn as_f32(v: &Value) -> Option<f32> {
    match v {
        Value::Float(f) => Some(*f),
        Value::Int(i) => Some(*i as f32),
        Value::Byte(b) => Some(f32::from(*b)),
        _ => None,
    }
}

fn as_i32(v: &Value) -> Option<i32> {
    match v {
        Value::Int(i) => Some(*i),
        Value::Byte(b) => Some(i32::from(*b)),
        _ => None,
    }
}

fn as_bool(v: &Value) -> Option<bool> {
    match v {
        Value::Bool(b) => Some(*b),
        _ => None,
    }
}

fn as_text(v: &Value) -> Option<String> {
    match v {
        Value::Name(s) | Value::Str(s) | Value::Enum(s) => Some(s.clone()),
        _ => None,
    }
}

/// Non-null object reference.
fn as_object(v: &Value) -> Option<&ObjRef> {
    match v {
        Value::Object(o) if o.index != 0 => Some(o),
        _ => None,
    }
}

fn f32_of(props: &[Property], name: &str) -> Option<f32> {
    prop(props, name).and_then(as_f32)
}

fn i32_of(props: &[Property], name: &str) -> Option<i32> {
    prop(props, name).and_then(as_i32)
}

fn bool_of(props: &[Property], name: &str) -> Option<bool> {
    prop(props, name).and_then(as_bool)
}

fn text_of(props: &[Property], name: &str) -> Option<String> {
    prop(props, name).and_then(as_text)
}

fn object_of(props: &[Property], name: &str) -> Option<String> {
    prop(props, name)
        .and_then(as_object)
        .map(|o| o.path.clone())
}

fn vec3_of(props: &[Property], name: &str) -> [f32; 3] {
    let Some(v) = prop(props, name) else {
        return [0.0; 3];
    };
    let c = |n: &str| member(v, n).and_then(as_f32).unwrap_or(0.0);
    [c("X"), c("Y"), c("Z")]
}

fn rotator_of(props: &[Property], name: &str) -> [i32; 3] {
    let Some(v) = prop(props, name) else {
        return [0; 3];
    };
    let c = |n: &str| member(v, n).and_then(as_i32).unwrap_or(0);
    [c("Pitch"), c("Yaw"), c("Roll")]
}

/// Parameter map of `props` without the named keys.
fn params_without(props: &[Property], skip: &[&str]) -> BTreeMap<String, ParamValue> {
    let mut m = param_map(props);
    m.retain(|k, _| !skip.iter().any(|s| s.eq_ignore_ascii_case(k)));
    m
}

fn is_default_object(lp: &LoadedPackage, index: usize) -> bool {
    lp.package
        .export(index)
        .is_ok_and(|e| e.object_flags & flags::object::CLASS_DEFAULT_OBJECT != 0)
}

fn is_archetype(lp: &LoadedPackage, index: usize) -> bool {
    lp.package
        .export(index)
        .is_ok_and(|e| e.object_flags & flags::object::ARCHETYPE_OBJECT != 0)
}

fn push_sample(v: &mut Vec<String>, s: String) {
    if v.len() < MAX_SAMPLES {
        v.push(s);
    }
}

fn bump<K: Ord>(m: &mut BTreeMap<K, usize>, k: K) {
    *m.entry(k).or_insert(0) += 1;
}

/// FNV-1a 64-bit hash, used to compare copies of the same payload across
/// packages without keeping them in memory.
pub fn content_hash(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

// ---------------------------------------------------------------------------
// SoundNodeWave
// ---------------------------------------------------------------------------

/// The seven bulk data records of a wave (see [`WAVE_BULK_SLOTS`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct WaveNative {
    /// Records in [`WAVE_BULK_SLOTS`] order.
    pub records: Vec<BulkDataRecord>,
}

impl WaveNative {
    /// Record of slot `slot` ([`WAVE_BULK_SLOTS`] index).
    pub fn record(&self, slot: usize) -> Option<&BulkDataRecord> {
        self.records.get(slot)
    }

    /// Slots that hold data (used, at least one element).
    pub fn filled_slots(&self) -> Vec<usize> {
        self.records
            .iter()
            .enumerate()
            .filter(|(_, r)| !r.is_empty())
            .map(|(i, _)| i)
            .collect()
    }
}

/// Read the native tail of a non-default `SoundNodeWave` starting at payload
/// offset `start`; returns the records and the offset after them.
pub fn read_wave_native(payload: &[u8], start: usize) -> Result<(WaveNative, usize), SoundError> {
    let mut r = Reader::at(payload, start)?;
    let mut records = Vec::with_capacity(WAVE_BULK_SLOTS.len());
    for _ in WAVE_BULK_SLOTS {
        records.push(bulkdata::read_bulk_record(&mut r)?);
    }
    Ok((WaveNative { records }, r.position()))
}

/// Container format recognised at the start of a payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
pub enum PayloadFormat {
    /// Ogg stream whose first packet is a Vorbis identification header.
    OggVorbis,
    /// Ogg stream of another codec.
    OggOther,
    /// RIFF/WAVE file.
    RiffWave,
    /// No payload.
    Empty,
    /// Anything else.
    Unknown,
}

impl PayloadFormat {
    /// File extension for writing the payload as is (`None` when it is not
    /// a self-describing file).
    pub fn extension(self) -> Option<&'static str> {
        match self {
            PayloadFormat::OggVorbis | PayloadFormat::OggOther => Some("ogg"),
            PayloadFormat::RiffWave => Some("wav"),
            PayloadFormat::Empty | PayloadFormat::Unknown => None,
        }
    }
}

/// Recognise the container of `bytes` from its first bytes.
pub fn sniff_payload(bytes: &[u8]) -> PayloadFormat {
    if bytes.is_empty() {
        return PayloadFormat::Empty;
    }
    if bytes.starts_with(b"OggS") {
        // Page header is 27 bytes plus the segment table; the first packet
        // follows it.
        let segs = bytes.get(26).copied().unwrap_or(0) as usize;
        let at = 27 + segs;
        return if bytes.get(at..at + 7) == Some(b"\x01vorbis") {
            PayloadFormat::OggVorbis
        } else {
            PayloadFormat::OggOther
        };
    }
    if bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WAVE") {
        return PayloadFormat::RiffWave;
    }
    PayloadFormat::Unknown
}

/// One subtitle line (`SubtitleCue`: `Text`, `Time`). Array elements store
/// every non-transient member (STRONG, see AUDIO.md); a missing member is
/// read as empty / zero.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SubtitleCue {
    /// Line text (original game text: keep local).
    pub text: String,
    /// Start time in seconds from the start of the wave.
    pub time: f32,
}

/// One entry of `LocalizedSubtitles`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct LocalizedSubtitle {
    /// Position in the array.
    pub slot: usize,
    /// `LanguageExt` (empty for unused slots).
    pub language: String,
    /// Lines.
    pub subtitles: Vec<SubtitleCue>,
    /// `bMature`.
    pub mature: bool,
    /// `bManualWordWrap`.
    pub manual_word_wrap: bool,
    /// `bSingleLine`.
    pub single_line: bool,
}

fn subtitle_lines(v: Option<&Value>) -> Vec<SubtitleCue> {
    let Some(Value::Array(items)) = v else {
        return Vec::new();
    };
    items
        .iter()
        .map(|it| SubtitleCue {
            text: member(it, "Text").and_then(as_text).unwrap_or_default(),
            time: member(it, "Time").and_then(as_f32).unwrap_or(0.0),
        })
        .collect()
}

fn localized_subtitles(v: Option<&Value>) -> Vec<LocalizedSubtitle> {
    let Some(Value::Array(items)) = v else {
        return Vec::new();
    };
    items
        .iter()
        .enumerate()
        .map(|(slot, it)| LocalizedSubtitle {
            slot,
            language: member(it, "LanguageExt")
                .and_then(as_text)
                .unwrap_or_default(),
            subtitles: subtitle_lines(member(it, "Subtitles")),
            mature: member(it, "bMature").and_then(as_bool).unwrap_or(false),
            manual_word_wrap: member(it, "bManualWordWrap")
                .and_then(as_bool)
                .unwrap_or(false),
            single_line: member(it, "bSingleLine").and_then(as_bool).unwrap_or(false),
        })
        .collect()
}

/// `SoundNodeWave` properties (the object's tags over its class defaults).
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct WaveProps {
    /// `Duration` in seconds.
    pub duration: Option<f32>,
    /// `NumChannels`.
    pub num_channels: Option<i32>,
    /// `SampleRate` in Hz.
    pub sample_rate: Option<i32>,
    /// `RawPCMDataSize`: bytes of 16-bit PCM the stream decodes to.
    pub raw_pcm_data_size: Option<i32>,
    /// `Volume`.
    pub volume: Option<f32>,
    /// `Pitch`.
    pub pitch: Option<f32>,
    /// `bLoopingSound`.
    pub looping: Option<bool>,
    /// `CompressionQuality`.
    pub compression_quality: Option<i32>,
    /// `bForceRealTimeDecompression`.
    pub force_realtime_decompression: Option<bool>,
    /// `MobileDetailMode`.
    pub mobile_detail_mode: Option<String>,
    /// `bMature`.
    pub mature: bool,
    /// `bManualWordWrap`.
    pub manual_word_wrap: bool,
    /// `bSingleLine`.
    pub single_line: bool,
    /// `bUseTTS`.
    pub use_tts: bool,
    /// `TTSSpeaker`.
    pub tts_speaker: Option<String>,
    /// `SpokenText` (original game text: keep local).
    pub spoken_text: Option<String>,
    /// `Comment` (editor-only text: keep local).
    pub comment: Option<String>,
    /// `Subtitles` (original game text: keep local).
    pub subtitles: Vec<SubtitleCue>,
    /// `LocalizedSubtitles` (original game text: keep local).
    pub localized_subtitles: Vec<LocalizedSubtitle>,
    /// True when the editor-only `SourceFilePath` is stored (its value, a
    /// developer machine path, is never exported).
    pub has_source_file_path: bool,
}

impl WaveProps {
    /// Read the known properties from `props` (already merged over defaults).
    pub fn from_properties(props: &[Property]) -> WaveProps {
        WaveProps {
            duration: f32_of(props, "Duration"),
            num_channels: i32_of(props, "NumChannels"),
            sample_rate: i32_of(props, "SampleRate"),
            raw_pcm_data_size: i32_of(props, "RawPCMDataSize"),
            volume: f32_of(props, "Volume"),
            pitch: f32_of(props, "Pitch"),
            looping: bool_of(props, "bLoopingSound"),
            compression_quality: i32_of(props, "CompressionQuality"),
            force_realtime_decompression: bool_of(props, "bForceRealTimeDecompression"),
            mobile_detail_mode: text_of(props, "MobileDetailMode"),
            mature: bool_of(props, "bMature").unwrap_or(false),
            manual_word_wrap: bool_of(props, "bManualWordWrap").unwrap_or(false),
            single_line: bool_of(props, "bSingleLine").unwrap_or(false),
            use_tts: bool_of(props, "bUseTTS").unwrap_or(false),
            tts_speaker: text_of(props, "TTSSpeaker"),
            spoken_text: text_of(props, "SpokenText").filter(|s| !s.is_empty()),
            comment: text_of(props, "Comment").filter(|s| !s.is_empty()),
            subtitles: subtitle_lines(prop(props, "Subtitles")),
            localized_subtitles: localized_subtitles(prop(props, "LocalizedSubtitles")),
            has_source_file_path: prop(props, "SourceFilePath").is_some(),
        }
    }
}

/// Borrowed view used by [`WaveProps::subtitle_view`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LocalizedSubtitleView<'a> {
    /// Language actually used.
    pub language: &'a str,
    /// Lines.
    pub lines: &'a [SubtitleCue],
    /// `bMature`.
    pub mature: bool,
    /// `bManualWordWrap`.
    pub manual_word_wrap: bool,
    /// `bSingleLine`.
    pub single_line: bool,
    /// True when the lines come from `LocalizedSubtitles` (false: `Subtitles`).
    pub from_localized: bool,
}

impl WaveProps {
    /// Subtitle lines for `language` (`LanguageExt`, any case): the matching
    /// non-empty `LocalizedSubtitles` entry, else, for `INT`, the plain
    /// `Subtitles` with the wave's own flags. `None` when the wave has no
    /// lines in that language.
    pub fn subtitle_view(&self, language: &str) -> Option<LocalizedSubtitleView<'_>> {
        if let Some(l) = self
            .localized_subtitles
            .iter()
            .find(|l| l.language.eq_ignore_ascii_case(language) && !l.subtitles.is_empty())
        {
            return Some(LocalizedSubtitleView {
                language: &l.language,
                lines: &l.subtitles,
                mature: l.mature,
                manual_word_wrap: l.manual_word_wrap,
                single_line: l.single_line,
                from_localized: true,
            });
        }
        if language.eq_ignore_ascii_case("INT") && !self.subtitles.is_empty() {
            return Some(LocalizedSubtitleView {
                language: "INT",
                lines: &self.subtitles,
                mature: self.mature,
                manual_word_wrap: self.manual_word_wrap,
                single_line: self.single_line,
                from_localized: false,
            });
        }
        None
    }
}

/// A decoded `SoundNodeWave` export.
#[derive(Debug, Clone, Serialize)]
pub struct SoundWave {
    /// Package (file stem).
    pub package: String,
    /// Export index.
    pub export_index: usize,
    /// Qualified object path.
    pub path: String,
    /// Qualified class path.
    pub class_path: String,
    /// True for a class default object.
    pub is_default_object: bool,
    /// Properties (tags over class defaults).
    pub props: WaveProps,
    /// Names of the properties stored on the object itself.
    pub tagged: Vec<String>,
    /// Native bulk records (`None` for a class default object).
    pub native: Option<WaveNative>,
    /// Export `SerialOffset`.
    pub serial_offset: i64,
    /// Payload offset where the tagged properties end.
    pub properties_end: usize,
    /// `SerialSize`.
    pub payload_size: usize,
}

impl SoundWave {
    /// Record of slot `slot`.
    pub fn record(&self, slot: usize) -> Option<&BulkDataRecord> {
        self.native.as_ref().and_then(|n| n.record(slot))
    }

    /// The slot that holds the playable audio: `CompressedPCData`, else
    /// `RawData`, else the first filled slot.
    pub fn audio_slot(&self) -> Option<usize> {
        let n = self.native.as_ref()?;
        let filled = n.filled_slots();
        [SLOT_COMPRESSED_PC, SLOT_RAW]
            .into_iter()
            .find(|s| filled.contains(s))
            .or_else(|| filled.first().copied())
    }

    /// Bytes of slot `slot` (decompressed when the record says so). `payload`
    /// is this export's payload. Cooked sound payloads are always inline.
    pub fn load_slot(&self, payload: &[u8], slot: usize) -> Result<Vec<u8>, SoundError> {
        let rec = self
            .record(slot)
            .ok_or_else(|| SoundError::Malformed(format!("{}: no bulk slot {slot}", self.path)))?;
        Ok(bulkdata::load(rec, payload, None, None, 1)?)
    }

    /// Every inline record's `BulkDataOffsetInFile` equals its absolute
    /// stream position.
    pub fn inline_offsets_match(&self) -> bool {
        self.native.as_ref().is_none_or(|n| {
            n.records.iter().all(|r| {
                r.storage() != BulkStorage::Inline || r.inline_offset_matches(self.serial_offset)
            })
        })
    }
}

/// Compact wave facts used in cue graphs and manifests.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct WaveSummary {
    /// `Duration`.
    pub duration: Option<f32>,
    /// `NumChannels`.
    pub num_channels: Option<i32>,
    /// `SampleRate`.
    pub sample_rate: Option<i32>,
    /// `bLoopingSound`.
    pub looping: Option<bool>,
    /// `Volume`.
    pub volume: Option<f32>,
    /// `Pitch`.
    pub pitch: Option<f32>,
    /// Slot holding the audio ([`WAVE_BULK_SLOTS`] name).
    pub audio_slot: Option<&'static str>,
    /// Stored bytes of that slot.
    pub audio_bytes: usize,
    /// Subtitle lines (plain `Subtitles`).
    pub subtitle_lines: usize,
}

impl SoundWave {
    /// Summary of this wave.
    pub fn summary(&self) -> WaveSummary {
        let slot = self.audio_slot();
        WaveSummary {
            duration: self.props.duration,
            num_channels: self.props.num_channels,
            sample_rate: self.props.sample_rate,
            looping: self.props.looping,
            volume: self.props.volume,
            pitch: self.props.pitch,
            audio_slot: slot.and_then(|s| WAVE_BULK_SLOTS.get(s).copied()),
            audio_bytes: slot
                .and_then(|s| self.record(s))
                .map_or(0, BulkDataRecord::stored_len),
            subtitle_lines: self.props.subtitles.len(),
        }
    }
}

// ---------------------------------------------------------------------------
// Ogg / Vorbis
// ---------------------------------------------------------------------------

const fn ogg_crc_table() -> [u32; 256] {
    let mut t = [0u32; 256];
    let mut i = 0;
    while i < 256 {
        let mut r = (i as u32) << 24;
        let mut j = 0;
        while j < 8 {
            r = if r & 0x8000_0000 != 0 {
                (r << 1) ^ 0x04c1_1db7
            } else {
                r << 1
            };
            j += 1;
        }
        t[i] = r;
        i += 1;
    }
    t
}

static OGG_CRC: [u32; 256] = ogg_crc_table();

/// Ogg page checksum (CRC-32, polynomial 0x04C11DB7, no reflection, zero
/// initial value) over `data`.
pub fn ogg_crc(data: &[u8]) -> u32 {
    data.iter().fold(0u32, |crc, &b| {
        (crc << 8) ^ OGG_CRC[usize::from(((crc >> 24) as u8) ^ b)]
    })
}

/// Vorbis identification header.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct VorbisIdent {
    /// `vorbis_version` (0).
    pub version: u32,
    /// `audio_channels`.
    pub channels: u8,
    /// `audio_sample_rate`.
    pub sample_rate: u32,
    /// `bitrate_maximum`.
    pub bitrate_maximum: i32,
    /// `bitrate_nominal`.
    pub bitrate_nominal: i32,
    /// `bitrate_minimum`.
    pub bitrate_minimum: i32,
    /// `blocksize_0` (log2).
    pub blocksize_0: u8,
    /// `blocksize_1` (log2).
    pub blocksize_1: u8,
}

/// Structure of an Ogg stream.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct OggInfo {
    /// Pages.
    pub pages: usize,
    /// Pages whose stored checksum differs from the computed one.
    pub crc_mismatches: usize,
    /// Distinct bitstream serial numbers.
    pub streams: usize,
    /// Pages whose sequence number does not follow the previous page of the
    /// same stream.
    pub sequence_gaps: usize,
    /// The first page has the beginning-of-stream flag.
    pub first_page_bos: bool,
    /// The last page has the end-of-stream flag.
    pub last_page_eos: bool,
    /// Granule position of the last page that sets one (samples per
    /// channel for Vorbis).
    pub final_granule: Option<i64>,
    /// Complete packets.
    pub packets: usize,
    /// Vorbis identification header (first packet).
    pub vorbis: Option<VorbisIdent>,
    /// Vorbis comment header vendor string (encoder library).
    pub vendor: Option<String>,
    /// Number of user comments in the comment header.
    pub comments: Option<u32>,
    /// The third packet is a Vorbis setup header.
    pub setup_header: bool,
    /// Pages whose continued-packet flag (`0x01`) disagrees with whether the
    /// previous page of the same stream ended inside a packet.
    pub continuation_errors: usize,
    /// Pages whose granule position is below an earlier one of the same
    /// stream.
    pub granule_regressions: usize,
    /// The stream ends inside a packet (its last lacing value is 255).
    pub unterminated_packet: bool,
}

impl OggInfo {
    /// True when the container is sound: every checksum correct, one
    /// logical stream from a BOS page to an EOS page, no sequence or
    /// continuation errors, non-decreasing granules, no unterminated packet
    /// and all three Vorbis header packets present.
    pub fn is_valid_vorbis(&self) -> bool {
        self.crc_mismatches == 0
            && self.streams == 1
            && self.sequence_gaps == 0
            && self.continuation_errors == 0
            && self.granule_regressions == 0
            && !self.unterminated_packet
            && self.first_page_bos
            && self.last_page_eos
            && self.vorbis.is_some()
            && self.vendor.is_some()
            && self.setup_header
    }
}

impl OggInfo {
    /// Duration in seconds from the final granule and the sample rate.
    pub fn duration(&self) -> Option<f64> {
        let g = self.final_granule?;
        let rate = self.vorbis.as_ref()?.sample_rate;
        (rate > 0 && g >= 0).then(|| g as f64 / f64::from(rate))
    }
}

fn parse_vorbis_ident(p: &[u8]) -> Option<VorbisIdent> {
    if p.len() < 30 || p.get(..7) != Some(b"\x01vorbis") {
        return None;
    }
    let u32_at = |o: usize| {
        p.get(o..o + 4)
            .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    };
    let i32_at = |o: usize| u32_at(o).map(|v| v as i32);
    let bs = *p.get(28)?;
    Some(VorbisIdent {
        version: u32_at(7)?,
        channels: *p.get(11)?,
        sample_rate: u32_at(12)?,
        bitrate_maximum: i32_at(16)?,
        bitrate_nominal: i32_at(20)?,
        bitrate_minimum: i32_at(24)?,
        blocksize_0: bs & 0x0f,
        blocksize_1: bs >> 4,
    })
}

fn parse_vorbis_comment(p: &[u8]) -> Option<(String, u32)> {
    if p.get(..7) != Some(b"\x03vorbis") {
        return None;
    }
    let mut r = Reader::at(p, 7).ok()?;
    let len = r.read_u32().ok()? as usize;
    let vendor = r.read_bytes(len).ok()?;
    let count = r.read_u32().ok()?;
    Some((String::from_utf8_lossy(vendor).into_owned(), count))
}

/// Walk every page of an Ogg stream, verify checksums and sequence numbers,
/// and read the Vorbis header packets. `bytes` must be exactly the stream:
/// bytes before the first page or after the last are an error.
pub fn parse_ogg(bytes: &[u8]) -> Result<OggInfo, SoundError> {
    let bad = |m: String| SoundError::Ogg(m);
    let mut info = OggInfo::default();
    let mut pos = 0usize;
    let mut seq: HashMap<u32, u32> = HashMap::new();
    let mut headers: Vec<Vec<u8>> = Vec::new();
    let mut current: Vec<u8> = Vec::new();
    let mut current_len = 0usize;
    let mut last_flags = 0u8;
    // Per stream: the previous page ended inside a packet / last granule.
    let mut pending: HashMap<u32, bool> = HashMap::new();
    let mut granules: HashMap<u32, i64> = HashMap::new();
    while pos < bytes.len() {
        if info.pages >= MAX_OGG_PAGES {
            return Err(bad(format!("more than {MAX_OGG_PAGES} pages")));
        }
        let header = bytes
            .get(pos..pos + 27)
            .ok_or_else(|| bad(format!("truncated page header at {pos}")))?;
        if &header[..4] != b"OggS" {
            return Err(bad(format!("no capture pattern at {pos}")));
        }
        if header[4] != 0 {
            return Err(bad(format!("page version {} at {pos}", header[4])));
        }
        let flags_byte = header[5];
        let granule = i64::from_le_bytes([
            header[6], header[7], header[8], header[9], header[10], header[11], header[12],
            header[13],
        ]);
        let serial = u32::from_le_bytes([header[14], header[15], header[16], header[17]]);
        let sequence = u32::from_le_bytes([header[18], header[19], header[20], header[21]]);
        let stored_crc = u32::from_le_bytes([header[22], header[23], header[24], header[25]]);
        let nsegs = usize::from(header[26]);
        let table = bytes
            .get(pos + 27..pos + 27 + nsegs)
            .ok_or_else(|| bad(format!("truncated segment table at {pos}")))?;
        let body_len: usize = table.iter().map(|&b| usize::from(b)).sum();
        let page_len = 27 + nsegs + body_len;
        let page = bytes
            .get(pos..pos + page_len)
            .ok_or_else(|| bad(format!("page at {pos} needs {page_len} bytes")))?;
        let mut copy = page.to_vec();
        if let Some(c) = copy.get_mut(22..26) {
            c.fill(0);
        }
        if ogg_crc(&copy) != stored_crc {
            info.crc_mismatches += 1;
        }
        if info.pages == 0 {
            info.first_page_bos = flags_byte & 0x02 != 0;
        }
        match seq.get(&serial) {
            Some(&prev) if prev.wrapping_add(1) != sequence => info.sequence_gaps += 1,
            _ => {}
        }
        seq.insert(serial, sequence);
        let continued = flags_byte & 0x01 != 0;
        if continued != pending.get(&serial).copied().unwrap_or(false) {
            info.continuation_errors += 1;
        }
        if let Some(&l) = table.last() {
            pending.insert(serial, l == 255);
        }
        if granule != -1 {
            if granules.get(&serial).is_some_and(|&g| granule < g) {
                info.granule_regressions += 1;
            }
            granules.insert(serial, granule);
            info.final_granule = Some(granule);
        }
        // Packets: a lacing value below 255 ends a packet.
        let mut body_at = pos + 27 + nsegs;
        for &lace in table {
            let lace = usize::from(lace);
            if headers.len() < 3
                && current_len.saturating_add(lace) <= MAX_HEADER_PACKET
                && let Some(seg) = bytes.get(body_at..body_at + lace)
            {
                current.extend_from_slice(seg);
            }
            current_len = current_len.saturating_add(lace);
            body_at += lace;
            if lace < 255 {
                info.packets += 1;
                if headers.len() < 3 {
                    headers.push(std::mem::take(&mut current));
                }
                current.clear();
                current_len = 0;
            }
        }
        last_flags = flags_byte;
        info.pages += 1;
        pos += page_len;
    }
    if info.pages == 0 {
        return Err(bad("empty stream".to_owned()));
    }
    info.last_page_eos = last_flags & 0x04 != 0;
    info.streams = seq.len();
    info.unterminated_packet = pending.values().any(|&p| p);
    if let Some(p) = headers.first() {
        info.vorbis = parse_vorbis_ident(p);
    }
    if let Some((vendor, count)) = headers.get(1).and_then(|p| parse_vorbis_comment(p)) {
        info.vendor = Some(vendor);
        info.comments = Some(count);
    }
    info.setup_header = headers
        .get(2)
        .is_some_and(|p| p.get(..7) == Some(b"\x05vorbis"));
    Ok(info)
}

// ---------------------------------------------------------------------------
// WAV
// ---------------------------------------------------------------------------

/// Format of a PCM WAV file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct WavInfo {
    /// `wFormatTag` (1 = PCM).
    pub format_tag: u16,
    /// Channels.
    pub channels: u16,
    /// Sample rate.
    pub sample_rate: u32,
    /// Bits per sample.
    pub bits_per_sample: u16,
    /// `nBlockAlign` (bytes per sample frame).
    pub block_align: u16,
    /// `nAvgBytesPerSec`.
    pub byte_rate: u32,
    /// Offset of the `data` chunk payload.
    pub data_offset: usize,
    /// Length of the `data` chunk payload.
    pub data_len: usize,
    /// The RIFF size field equals the file length minus 8.
    pub riff_size_matches: bool,
}

impl WavInfo {
    /// Sample frames in the `data` chunk.
    pub fn frames(&self) -> usize {
        self.data_len
            .checked_div(usize::from(self.block_align))
            .unwrap_or(0)
    }
}

/// A RIFF/WAVE file holding 16-bit (or 8/24/32-bit) little-endian PCM:
/// 44-byte canonical header (`fmt ` chunk of 16 bytes, then `data`), a pad
/// byte when the data length is odd.
pub fn wav_file(
    pcm: &[u8],
    channels: u16,
    sample_rate: u32,
    bits_per_sample: u16,
) -> Result<Vec<u8>, SoundError> {
    let bad = |m: String| SoundError::Wav(m);
    if channels == 0 || sample_rate == 0 {
        return Err(bad(format!("{channels} channels at {sample_rate} Hz")));
    }
    if !matches!(bits_per_sample, 8 | 16 | 24 | 32) {
        return Err(bad(format!("{bits_per_sample} bits per sample")));
    }
    let block_align = u32::from(channels) * u32::from(bits_per_sample / 8);
    let block = u16::try_from(block_align).map_err(|_| bad("block align overflows".to_owned()))?;
    if !pcm.len().is_multiple_of(block_align as usize) {
        return Err(bad(format!(
            "{} PCM bytes are not a whole number of {block_align}-byte frames",
            pcm.len()
        )));
    }
    let byte_rate = sample_rate
        .checked_mul(block_align)
        .ok_or_else(|| bad("byte rate overflows".to_owned()))?;
    let data_len = u32::try_from(pcm.len()).map_err(|_| bad("data too large".to_owned()))?;
    let pad = data_len % 2;
    let riff_len = data_len
        .checked_add(36 + pad)
        .ok_or_else(|| bad("data too large".to_owned()))?;
    let mut out = Vec::with_capacity(44 + pcm.len() + pad as usize);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&riff_len.to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&channels.to_le_bytes());
    out.extend_from_slice(&sample_rate.to_le_bytes());
    out.extend_from_slice(&byte_rate.to_le_bytes());
    out.extend_from_slice(&block.to_le_bytes());
    out.extend_from_slice(&bits_per_sample.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_len.to_le_bytes());
    out.extend_from_slice(pcm);
    if pad == 1 {
        out.push(0);
    }
    Ok(out)
}

/// Parse the chunks of a RIFF/WAVE file far enough to find its format and
/// sample data. Unknown chunks are skipped; every size is bounds-checked.
/// The format must be self-consistent: at least one channel, a non-zero
/// rate and sample size and, for PCM (`wFormatTag` 1), `nBlockAlign` =
/// channels x bytes per sample, `nAvgBytesPerSec` = rate x `nBlockAlign`
/// and a `data` chunk of whole frames.
pub fn parse_wav(bytes: &[u8]) -> Result<WavInfo, SoundError> {
    let bad = |m: String| SoundError::Wav(m);
    if bytes.get(..4) != Some(b"RIFF") || bytes.get(8..12) != Some(b"WAVE") {
        return Err(bad("not a RIFF/WAVE file".to_owned()));
    }
    let riff_size_matches = bytes
        .get(4..8)
        .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
        .and_then(|n| usize::try_from(n).ok())
        .and_then(|n| n.checked_add(8))
        == Some(bytes.len());
    let mut r = Reader::at(bytes, 12)?;
    let mut fmt: Option<(u16, u16, u32, u32, u16, u16)> = None;
    while r.remaining() >= 8 {
        let id = r.read_bytes(4)?;
        let len = r.read_u32()? as usize;
        let at = r.position();
        if len > r.remaining() {
            return Err(bad(format!(
                "chunk of {len} bytes at {at} runs past the end"
            )));
        }
        if id == b"fmt " {
            if len < 16 {
                return Err(bad(format!("fmt chunk of {len} bytes")));
            }
            let mut f = Reader::at(bytes, at)?;
            let tag = f.read_u16()?;
            let ch = f.read_u16()?;
            let rate = f.read_u32()?;
            let byte_rate = f.read_u32()?;
            let align = f.read_u16()?;
            let bits = f.read_u16()?;
            if ch == 0 || rate == 0 || bits == 0 || align == 0 {
                return Err(bad(format!(
                    "fmt: {ch} channels, {rate} Hz, {bits} bits, block align {align}"
                )));
            }
            if tag == 1 {
                let expect_align = u32::from(ch) * u32::from(bits.div_ceil(8));
                let expect_rate = rate.checked_mul(expect_align);
                if u32::from(align) != expect_align || Some(byte_rate) != expect_rate {
                    return Err(bad(format!(
                        "PCM fmt: block align {align} / byte rate {byte_rate} do not match \
                         {ch} channels of {bits} bits at {rate} Hz"
                    )));
                }
            }
            fmt = Some((tag, ch, rate, byte_rate, align, bits));
        } else if id == b"data" {
            let (format_tag, channels, sample_rate, byte_rate, block_align, bits_per_sample) =
                fmt.ok_or_else(|| bad("data chunk before fmt chunk".to_owned()))?;
            if format_tag == 1 && !len.is_multiple_of(usize::from(block_align)) {
                return Err(bad(format!(
                    "data chunk of {len} bytes is not a whole number of {block_align}-byte frames"
                )));
            }
            return Ok(WavInfo {
                format_tag,
                channels,
                sample_rate,
                bits_per_sample,
                block_align,
                byte_rate,
                data_offset: at,
                data_len: len,
                riff_size_matches,
            });
        }
        r.skip(len)?;
        if len % 2 == 1 && r.remaining() > 0 {
            r.skip(1)?;
        }
    }
    Err(bad("no data chunk".to_owned()))
}

// ---------------------------------------------------------------------------
// SoundCue, SoundNode, SoundClass, SoundMode
// ---------------------------------------------------------------------------

/// One `EditorData` entry (cue node or sound class position in the editor).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct EditorEntry {
    /// Key object (`None` when null).
    pub object: Option<String>,
    /// `NodePosX`.
    pub x: i32,
    /// `NodePosY`.
    pub y: i32,
}

/// Read an `EditorData` map (`i32` count, `count` x `{i32 object, i32 x,
/// i32 y}`) starting at payload offset `start`; returns the entries and the
/// offset after them.
pub fn read_editor_map(
    lp: &LoadedPackage,
    payload: &[u8],
    start: usize,
) -> Result<(Vec<EditorEntry>, usize), SoundError> {
    let mut r = Reader::at(payload, start)?;
    let n = r.read_count("EditorData", EDITOR_ENTRY_SIZE)?;
    let mut out = Vec::with_capacity(n);
    for _ in 0..n {
        let at = r.position();
        let key = r.read_package_index()?;
        let x = r.read_i32()?;
        let y = r.read_i32()?;
        let object = lp
            .ref_path(key)
            .map_err(|e| SoundError::Malformed(format!("EditorData key at {at}: {e}")))?;
        out.push(EditorEntry { object, x, y });
    }
    Ok((out, r.position()))
}

/// A decoded `SoundCue` export (without its graph).
#[derive(Debug, Clone, Serialize)]
pub struct SoundCueData {
    /// Package (file stem).
    pub package: String,
    /// Export index.
    pub export_index: usize,
    /// Qualified object path.
    pub path: String,
    /// True for a class default object.
    pub is_default_object: bool,
    /// `SoundClass` (name of a `SoundClass` object).
    pub sound_class: Option<String>,
    /// `FirstNode`.
    pub first_node: Option<String>,
    /// `VolumeMultiplier`.
    pub volume_multiplier: Option<f32>,
    /// `PitchMultiplier`.
    pub pitch_multiplier: Option<f32>,
    /// `Duration` (cooker-computed; 10000 marks a looping cue).
    pub duration: Option<f32>,
    /// `MaxConcurrentPlayCount`.
    pub max_concurrent_play_count: Option<i32>,
    /// `FaceFXAnimSetRef`.
    pub face_fx_anim_set: Option<String>,
    /// `FaceFXGroupName`.
    pub face_fx_group: Option<String>,
    /// `FaceFXAnimName`.
    pub face_fx_anim: Option<String>,
    /// `EditorData` entries.
    pub editor: Vec<EditorEntry>,
    /// Raw `FirstNode` package index (0 = null).
    #[serde(skip)]
    pub first_node_index: i32,
    /// Payload offset where the tagged properties end.
    pub properties_end: usize,
    /// `SerialSize`.
    pub payload_size: usize,
}

/// A `DistributionFloat*` subobject referenced by a node value.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DistributionInfo {
    /// Node property holding it (`PitchModulation`, ...).
    pub property: String,
    /// Object path.
    pub path: String,
    /// Class name.
    pub class: String,
    /// Values (tags over defaults).
    pub params: BTreeMap<String, ParamValue>,
}

/// A decoded `SoundNode` export.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct NodeData {
    /// Package (file stem).
    pub package: String,
    /// Export index.
    pub export_index: usize,
    /// Qualified object path.
    pub path: String,
    /// Class name.
    pub class: String,
    /// Role.
    pub kind: NodeKind,
    /// `ChildNodes` (`None` = empty input).
    pub children: Vec<Option<String>>,
    /// Raw `ChildNodes` package indices.
    #[serde(skip)]
    pub child_indices: Vec<i32>,
    /// Values (tags over defaults) without `ChildNodes`; empty for waves,
    /// which are described by `wave`.
    pub params: BTreeMap<String, ParamValue>,
    /// Wave facts (wave nodes).
    pub wave: Option<WaveSummary>,
    /// Distribution subobjects referenced by the values.
    pub distributions: Vec<DistributionInfo>,
}

/// One node of a cue graph.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CueNode {
    /// Object path (the node's id inside the graph).
    pub path: String,
    /// Package the node was decoded from (`None` when unresolved).
    pub package: Option<String>,
    /// Class name.
    pub class: String,
    /// Role.
    pub kind: Option<NodeKind>,
    /// True when the object was found and decoded.
    pub resolved: bool,
    /// `ChildNodes` (`None` = empty input).
    pub children: Vec<Option<String>>,
    /// Values (tags over defaults).
    pub params: BTreeMap<String, ParamValue>,
    /// Wave facts (wave nodes).
    pub wave: Option<WaveSummary>,
    /// Distribution subobjects.
    pub distributions: Vec<DistributionInfo>,
    /// Longest path from `FirstNode` (0) to this node.
    pub depth: usize,
}

/// A cue with its node graph (reachable from `FirstNode`).
#[derive(Debug, Clone, Serialize)]
pub struct CueGraph {
    /// The cue.
    pub cue: SoundCueData,
    /// Nodes in breadth-first order from `FirstNode`.
    pub nodes: Vec<CueNode>,
    /// Paths of the wave leaves (deduplicated, first-seen order).
    pub waves: Vec<String>,
    /// Empty child inputs.
    pub null_children: usize,
    /// References that could not be resolved to a decodable node.
    pub dangling: Vec<String>,
    /// True when the graph has a cycle.
    pub cycle: bool,
    /// Longest path (edges) from `FirstNode`.
    pub max_depth: usize,
    /// `SoundNode` exports inside the cue that `FirstNode` does not reach.
    pub unreachable: Vec<String>,
    /// True when the `EditorData` keys are exactly the reachable nodes.
    pub editor_matches_graph: bool,
    /// `EditorData` keys that are not reachable nodes.
    pub editor_extra: usize,
    /// Reachable nodes without an `EditorData` entry.
    pub editor_missing: usize,
    /// Class name of the object that owns the cue (its outer), `None` when
    /// the cue sits directly in a package.
    pub owner_class: Option<String>,
    /// Non-fatal problems.
    pub issues: Vec<String>,
}

/// A decoded `SoundClass`.
#[derive(Debug, Clone, Serialize)]
pub struct SoundClassData {
    /// Package.
    pub package: String,
    /// Export index.
    pub export_index: usize,
    /// Object path.
    pub path: String,
    /// Object name (what cues reference).
    pub name: String,
    /// `Properties` (tags over defaults).
    pub properties: BTreeMap<String, ParamValue>,
    /// `ChildClassNames`.
    pub child_class_names: Vec<String>,
    /// `bIsChild`.
    pub is_child: bool,
    /// `EditorData` entries.
    pub editor: Vec<EditorEntry>,
}

/// A decoded `SoundMode`.
#[derive(Debug, Clone, Serialize)]
pub struct SoundModeData {
    /// Package.
    pub package: String,
    /// Export index.
    pub export_index: usize,
    /// Object path.
    pub path: String,
    /// Object name.
    pub name: String,
    /// Values (tags over defaults): `bApplyEQ`, `EQSettings`,
    /// `SoundClassEffects`, `InitialDelay`, `FadeInTime`, `Duration`,
    /// `FadeOutTime`.
    pub params: BTreeMap<String, ParamValue>,
}

// ---------------------------------------------------------------------------
// Ambient sound actors and reverb volumes
// ---------------------------------------------------------------------------

/// The inline `SoundNodeAmbient*` of an `AmbientSoundSimple*` actor.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct AmbientNodeInfo {
    /// Object path.
    pub path: String,
    /// Class name.
    pub class: String,
    /// Values (tags over defaults), `SoundSlots` included.
    pub params: BTreeMap<String, ParamValue>,
    /// Waves of the `SoundSlots`.
    pub waves: Vec<String>,
}

/// One placed ambient sound actor.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct AmbientActor {
    /// Export index (joins with the level scene's `export_index`).
    pub export_index: usize,
    /// Object name (joins with the level scene's `name`).
    pub name: String,
    /// Object path.
    pub path: String,
    /// Class name.
    pub class: String,
    /// Kind.
    pub kind: AmbientKind,
    /// Index in `ULevel::Actors` (`None` when not listed there).
    pub level_slot: Option<usize>,
    /// `Location`.
    pub location: [f32; 3],
    /// `Rotation` (Pitch, Yaw, Roll; 65536 units per turn).
    pub rotation: [i32; 3],
    /// `DrawScale`.
    pub draw_scale: f32,
    /// `bAutoPlay`.
    pub auto_play: Option<bool>,
    /// `bIsPlaying` as saved.
    pub is_playing: Option<bool>,
    /// `AudioComponent`.
    pub audio_component: Option<String>,
    /// Class name of the audio component (`AudioComponent`,
    /// `SplineAudioComponent`, `MultiCueSplineAudioComponent`, ...).
    pub audio_component_class: Option<String>,
    /// Values stored on the audio component itself (its deltas over the
    /// template), e.g. spline `Points` and multi-cue `SoundSlots`; the
    /// editor-only `PreviewSoundRadius` is left out.
    pub audio_component_params: BTreeMap<String, ParamValue>,
    /// The cue played: the audio component's `SoundCue`, else the actor's
    /// `SoundCueInstance`.
    pub sound_cue: Option<String>,
    /// Audio component `VolumeMultiplier` (tags over template).
    pub volume_multiplier: Option<f32>,
    /// Audio component `PitchMultiplier` (tags over template).
    pub pitch_multiplier: Option<f32>,
    /// Inline ambient node (`AmbientProperties` / `SoundNodeInstance`).
    pub ambient_node: Option<AmbientNodeInfo>,
    /// Values stored on the actor itself (designer deltas) except
    /// placement and component references.
    pub instance: BTreeMap<String, ParamValue>,
}

/// One placed reverb volume.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ReverbVolumeInfo {
    /// Export index.
    pub export_index: usize,
    /// Object name.
    pub name: String,
    /// Object path.
    pub path: String,
    /// Class name.
    pub class: String,
    /// Index in `ULevel::Actors`.
    pub level_slot: Option<usize>,
    /// `Location`.
    pub location: [f32; 3],
    /// `Priority`.
    pub priority: Option<f32>,
    /// `bEnabled`.
    pub enabled: Option<bool>,
    /// `Settings` (reverb type, volume, fade time).
    pub settings: Option<ParamValue>,
    /// `AmbientZoneSettings`.
    pub ambient_zone: Option<ParamValue>,
}

/// Audio placed in one map package.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct MapAudio {
    /// Package (file stem).
    pub package: String,
    /// Ambient sound actors.
    pub ambient: Vec<AmbientActor>,
    /// Reverb volumes.
    pub reverb: Vec<ReverbVolumeInfo>,
    /// Non-fatal problems.
    pub issues: Vec<String>,
}

// ---------------------------------------------------------------------------
// Decoder
// ---------------------------------------------------------------------------

type EffectiveKey = (String, usize);

/// Cue export -> `SoundNode` exports inside it (ascending), for one package.
type CueMembers = Arc<HashMap<usize, Vec<usize>>>;

/// Where an object reference points.
enum Target {
    /// Export of the package holding the reference.
    Local(usize),
    /// Export of another package.
    Other(Arc<LoadedPackage>, usize),
}

impl Target {
    fn parts<'p>(&'p self, owner: &'p LoadedPackage) -> (&'p LoadedPackage, usize) {
        match self {
            Target::Local(i) => (owner, *i),
            Target::Other(lp, i) => (lp, *i),
        }
    }
}

/// Decodes sound exports of a [`PackageSet`], caching class defaults and
/// resolved values.
///
/// References that are not exports of the package holding them are
/// resolved in a fixed order (see [`SoundDecoder::locate_from`]), so the
/// package a shared object is taken from never depends on hash order.
pub struct SoundDecoder<'a> {
    set: &'a PackageSet,
    schema: Option<&'a dyn Schema>,
    class_defaults: RefCell<HashMap<String, Arc<Vec<Property>>>>,
    effective: RefCell<HashMap<EffectiveKey, Arc<Vec<Property>>>>,
    kinds: RefCell<HashMap<String, Option<SoundKind>>>,
    chains: RefCell<HashMap<String, Arc<Vec<String>>>>,
    /// Packages handed to or registered with this decoder (lower-case name
    /// -> name), searched in this (sorted) order.
    seen: RefCell<BTreeMap<String, String>>,
    /// Per package: cue export -> `SoundNode` exports inside it.
    cue_members: RefCell<HashMap<String, CueMembers>>,
}

impl<'a> SoundDecoder<'a> {
    /// Decoder over `set` (also used as the value schema).
    pub fn new(set: &'a PackageSet) -> SoundDecoder<'a> {
        SoundDecoder {
            set,
            schema: None,
            class_defaults: RefCell::default(),
            effective: RefCell::default(),
            kinds: RefCell::default(),
            chains: RefCell::default(),
            seen: RefCell::default(),
            cue_members: RefCell::default(),
        }
    }

    /// Add the package called `name` to the packages searched for
    /// references (packages passed to the decoding methods are added
    /// automatically). Register every package of a run up front to make
    /// resolution independent of the order in which they are decoded.
    pub fn register_package(&self, name: &str) {
        let key = name.to_ascii_lowercase();
        if !self.seen.borrow().contains_key(&key) {
            self.seen.borrow_mut().insert(key, name.to_owned());
        }
    }

    fn note(&self, lp: &LoadedPackage) {
        self.register_package(&lp.name);
    }

    /// Locate the export at qualified `path` for a reference held by an
    /// object of `owner`, in this order: an export of `owner` itself; the
    /// registered localized companions of `owner` (`<owner>_LOC_*`, sorted);
    /// the package named by the path's first component; the seek-free
    /// startup packages (`Startup*`, not localized, sorted); the registered
    /// packages (sorted); finally [`PackageSet::locate`]. Copies of a shared
    /// object therefore always resolve to the same package.
    pub fn locate_from(
        &self,
        owner: &LoadedPackage,
        path: &str,
    ) -> Option<(Option<Arc<LoadedPackage>>, usize)> {
        if let Some(i) = owner.export_by_qualified(path) {
            return Some((None, i));
        }
        let in_pkg = |name: &str| {
            let lp = self.set.package(name)?;
            let i = lp.export_by_qualified(path)?;
            Some((Some(lp), i))
        };
        let seen: Vec<(String, String)> = self
            .seen
            .borrow()
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        if !is_localized_package(&owner.name) {
            let prefix = format!("{}_loc_", owner.name.to_ascii_lowercase());
            if let Some(hit) = seen
                .iter()
                .filter(|(k, _)| k.starts_with(&prefix))
                .find_map(|(_, name)| in_pkg(name))
            {
                return Some(hit);
            }
        }
        let first = path.split('.').next().unwrap_or(path);
        if let Some(hit) = in_pkg(first) {
            return Some(hit);
        }
        let startup: Vec<String> = self
            .set
            .file_names()
            .filter(|k| k.starts_with("startup") && !is_localized_package(k))
            .map(str::to_owned)
            .collect();
        if let Some(hit) = startup.iter().find_map(|name| in_pkg(name)) {
            return Some(hit);
        }
        if let Some(hit) = seen.iter().find_map(|(_, name)| in_pkg(name)) {
            return Some(hit);
        }
        self.set.locate(path).map(|(lp, i)| (Some(lp), i))
    }

    fn locate_target(&self, owner: &LoadedPackage, path: &str) -> Option<Target> {
        match self.locate_from(owner, path)? {
            (None, i) => Some(Target::Local(i)),
            (Some(lp), i) if lp.name.eq_ignore_ascii_case(&owner.name) => Some(Target::Local(i)),
            (Some(lp), i) => Some(Target::Other(lp, i)),
        }
    }

    /// Decoder over `set` that decodes values with `schema` (tests, or sets
    /// without the engine script packages).
    pub fn with_schema(set: &'a PackageSet, schema: &'a dyn Schema) -> SoundDecoder<'a> {
        SoundDecoder {
            schema: Some(schema),
            ..SoundDecoder::new(set)
        }
    }

    /// The package set.
    pub fn set(&self) -> &'a PackageSet {
        self.set
    }

    fn schema(&self) -> &dyn Schema {
        match self.schema {
            Some(s) => s,
            None => self.set,
        }
    }

    /// Decode prelude and tagged properties of export `index`.
    pub fn decode_object(
        &self,
        lp: &LoadedPackage,
        index: usize,
    ) -> Result<DecodedObject, SoundError> {
        Ok(object::decode_object(
            &lp.package,
            Some(&lp.name),
            index,
            self.schema(),
        )?)
    }

    /// Lower-case class chain of `class_path` (nearest first, itself excluded
    /// when the schema omits it).
    pub fn class_chain(&self, class_path: &str) -> Arc<Vec<String>> {
        let key = class_path.to_ascii_lowercase();
        if let Some(c) = self.chains.borrow().get(&key) {
            return c.clone();
        }
        let c = Arc::new(self.schema().class_chain(class_path));
        self.chains.borrow_mut().insert(key, c.clone());
        c
    }

    /// Sound kind of the class at `class_path`, if it is one.
    pub fn kind_of_class(&self, class_path: &str) -> Option<SoundKind> {
        let key = class_path.to_ascii_lowercase();
        if let Some(k) = self.kinds.borrow().get(&key) {
            return *k;
        }
        let k = SoundKind::classify(class_path, &self.class_chain(class_path));
        self.kinds.borrow_mut().insert(key, k);
        k
    }

    /// Qualified class path of export `index`.
    pub fn class_path(&self, lp: &LoadedPackage, index: usize) -> Option<String> {
        object::export_class_path(&lp.package, Some(&lp.name), index).ok()
    }

    /// Sound kind of export `index`, with its class path.
    pub fn export_kind(&self, lp: &LoadedPackage, index: usize) -> Option<(String, SoundKind)> {
        let class = self.class_path(lp, index)?;
        let k = self.kind_of_class(&class)?;
        Some((class, k))
    }

    /// Class defaults of `class_path` merged across its super chain, as a
    /// property list (empty when the class is not in the set).
    pub fn class_defaults(&self, class_path: &str) -> Arc<Vec<Property>> {
        let key = class_path.to_ascii_lowercase();
        if let Some(d) = self.class_defaults.borrow().get(&key) {
            return d.clone();
        }
        let props: Vec<Property> = match self.set.inherited_defaults(class_path) {
            Ok(d) => d
                .values
                .into_iter()
                .map(|v| Property {
                    name: v.name,
                    type_name: v.type_name,
                    array_index: v.array_index,
                    size: 0,
                    struct_name: None,
                    enum_name: None,
                    value: v.value,
                    offset: 0,
                })
                .collect(),
            Err(_) => Vec::new(),
        };
        let props = Arc::new(props);
        self.class_defaults.borrow_mut().insert(key, props.clone());
        props
    }

    /// Values of export `index`: its own tags merged over its archetype's
    /// values (or its class defaults when the archetype is a class default
    /// object or unavailable). Never fails: undecodable objects contribute
    /// no tags.
    pub fn effective(&self, lp: &LoadedPackage, index: usize) -> Arc<Vec<Property>> {
        self.note(lp);
        self.effective_depth(lp, index, None, 0)
    }

    fn effective_with(
        &self,
        lp: &LoadedPackage,
        index: usize,
        own: &[Property],
    ) -> Arc<Vec<Property>> {
        self.effective_depth(lp, index, Some(own), 0)
    }

    fn effective_depth(
        &self,
        lp: &LoadedPackage,
        index: usize,
        own: Option<&[Property]>,
        depth: usize,
    ) -> Arc<Vec<Property>> {
        let key = (lp.name.to_ascii_lowercase(), index);
        if let Some(v) = self.effective.borrow().get(&key) {
            return v.clone();
        }
        let decoded;
        let own: &[Property] = match own {
            Some(o) => o,
            None => {
                decoded = self
                    .decode_object(lp, index)
                    .map(|o| o.properties)
                    .unwrap_or_default();
                &decoded
            }
        };
        let base = self.base_values(lp, index, depth);
        let mut merged = (*base).clone();
        merge_properties(&mut merged, own);
        let merged = Arc::new(merged);
        self.effective.borrow_mut().insert(key, merged.clone());
        merged
    }

    fn base_values(&self, lp: &LoadedPackage, index: usize, depth: usize) -> Arc<Vec<Property>> {
        let class_defaults = || {
            self.class_path(lp, index)
                .map(|c| self.class_defaults(&c))
                .unwrap_or_default()
        };
        if is_default_object(lp, index) {
            // A class default object's values sit over its super class's
            // defaults; callers only need its own tags merged over those of
            // its class chain, which `class_defaults` of its class already is.
            return Arc::new(Vec::new());
        }
        let Ok(entry) = lp.package.export(index) else {
            return class_defaults();
        };
        let arch = entry.archetype_index;
        if arch.is_null() || depth >= MAX_ARCHETYPE_DEPTH {
            return class_defaults();
        }
        let Ok(Some(apath)) = lp.ref_path(arch) else {
            return class_defaults();
        };
        if last_component(&apath)
            .to_ascii_lowercase()
            .starts_with("default__")
        {
            return class_defaults();
        }
        match self.locate_target(lp, &apath) {
            Some(t) => {
                let (alp, ai) = t.parts(lp);
                self.effective_depth(alp, ai, None, depth + 1)
            }
            None => class_defaults(),
        }
    }

    /// Decode a `SoundNodeWave` export strictly (prelude, tags and the seven
    /// bulk records must consume exactly `SerialSize` bytes).
    pub fn decode_wave(&self, lp: &LoadedPackage, index: usize) -> Result<SoundWave, SoundError> {
        self.note(lp);
        let class_path = self.class_path(lp, index).unwrap_or_default();
        match self.kind_of_class(&class_path) {
            Some(SoundKind::Node(k)) if k.is_wave() => {}
            _ => {
                return Err(SoundError::WrongClass {
                    export: index,
                    class: class_path,
                    expected: "SoundNodeWave",
                });
            }
        }
        let entry = lp.package.export(index)?;
        let serial_offset = i64::from(entry.serial_offset);
        let cdo = is_default_object(lp, index);
        let obj = self.decode_object(lp, index)?;
        let payload = lp.package.export_data(index)?;
        let native = if cdo {
            expect_no_tail(&obj, "SoundNodeWave class default object")?;
            None
        } else {
            let (n, end) = read_wave_native(payload, obj.properties_end)?;
            if end != payload.len() {
                return Err(SoundError::Malformed(format!(
                    "{}: wave native data ends at {end} of {} payload bytes",
                    obj.path,
                    payload.len()
                )));
            }
            Some(n)
        };
        let eff = if cdo {
            Arc::new(obj.properties.clone())
        } else {
            self.effective_with(lp, index, &obj.properties)
        };
        Ok(SoundWave {
            package: lp.name.clone(),
            export_index: index,
            tagged: obj.properties.iter().map(|p| p.name.clone()).collect(),
            path: obj.path,
            class_path,
            is_default_object: cdo,
            props: WaveProps::from_properties(&eff),
            native,
            serial_offset,
            properties_end: obj.properties_end,
            payload_size: payload.len(),
        })
    }

    /// Decode a `SoundCue` export strictly (tags plus `EditorData`).
    pub fn decode_cue(&self, lp: &LoadedPackage, index: usize) -> Result<SoundCueData, SoundError> {
        self.note(lp);
        let class_path = self.class_path(lp, index).unwrap_or_default();
        if self.kind_of_class(&class_path) != Some(SoundKind::Cue) {
            return Err(SoundError::WrongClass {
                export: index,
                class: class_path,
                expected: "SoundCue",
            });
        }
        let cdo = is_default_object(lp, index);
        let obj = self.decode_object(lp, index)?;
        let payload = lp.package.export_data(index)?;
        let editor = if cdo {
            expect_no_tail(&obj, "SoundCue class default object")?;
            Vec::new()
        } else {
            let (entries, end) = read_editor_map(lp, payload, obj.properties_end)?;
            if end != payload.len() {
                return Err(SoundError::Malformed(format!(
                    "{}: cue EditorData ends at {end} of {} payload bytes",
                    obj.path,
                    payload.len()
                )));
            }
            entries
        };
        let eff = if cdo {
            Arc::new(obj.properties.clone())
        } else {
            self.effective_with(lp, index, &obj.properties)
        };
        let first_node_index = prop(&eff, "FirstNode")
            .and_then(as_object)
            .map_or(0, |o| o.index);
        Ok(SoundCueData {
            package: lp.name.clone(),
            export_index: index,
            path: obj.path.clone(),
            is_default_object: cdo,
            sound_class: text_of(&eff, "SoundClass").filter(|s| !s.eq_ignore_ascii_case("None")),
            first_node: object_of(&eff, "FirstNode"),
            volume_multiplier: f32_of(&eff, "VolumeMultiplier"),
            pitch_multiplier: f32_of(&eff, "PitchMultiplier"),
            duration: f32_of(&eff, "Duration"),
            max_concurrent_play_count: i32_of(&eff, "MaxConcurrentPlayCount"),
            face_fx_anim_set: object_of(&eff, "FaceFXAnimSetRef"),
            face_fx_group: text_of(&eff, "FaceFXGroupName").filter(|s| !s.is_empty()),
            face_fx_anim: text_of(&eff, "FaceFXAnimName").filter(|s| !s.is_empty()),
            editor,
            first_node_index,
            properties_end: obj.properties_end,
            payload_size: payload.len(),
        })
    }

    /// Decode a `SoundNode` export strictly. Waves are decoded with
    /// [`SoundDecoder::decode_wave`] and summarised; other nodes must have no
    /// native data.
    pub fn decode_node(&self, lp: &LoadedPackage, index: usize) -> Result<NodeData, SoundError> {
        self.note(lp);
        let class_path = self.class_path(lp, index).unwrap_or_default();
        let Some(SoundKind::Node(kind)) = self.kind_of_class(&class_path) else {
            return Err(SoundError::WrongClass {
                export: index,
                class: class_path,
                expected: "SoundNode",
            });
        };
        let class = last_component(&class_path).to_owned();
        if kind.is_wave() {
            let w = self.decode_wave(lp, index)?;
            let own = self.effective(lp, index);
            let (children, child_indices) = child_refs(&own);
            return Ok(NodeData {
                package: lp.name.clone(),
                export_index: index,
                path: w.path.clone(),
                class,
                kind,
                children,
                child_indices,
                params: BTreeMap::new(),
                wave: Some(w.summary()),
                distributions: Vec::new(),
            });
        }
        let obj = self.decode_object(lp, index)?;
        expect_no_tail(&obj, "SoundNode")?;
        let eff = self.effective_with(lp, index, &obj.properties);
        let (children, child_indices) = child_refs(&eff);
        let distributions = self.distributions(lp, &eff);
        Ok(NodeData {
            package: lp.name.clone(),
            export_index: index,
            path: obj.path,
            class,
            kind,
            children,
            child_indices,
            params: params_without(&eff, &["ChildNodes"]),
            wave: None,
            distributions,
        })
    }

    /// Resolve a reference read from an object of `owner`. A value merged in
    /// from an archetype in another package carries that package's index, so
    /// a local export is used only when its path is the reference's path;
    /// everything else is located by path.
    fn resolve_ref(&self, owner: &LoadedPackage, r: &ObjRef) -> Option<Target> {
        if let Some(i) = PackageIndex(r.index).export_index()
            && owner
                .qualified(i)
                .is_ok_and(|p| p.eq_ignore_ascii_case(&r.path))
        {
            return Some(Target::Local(i));
        }
        self.locate_target(owner, &r.path)
    }

    fn distributions(&self, lp: &LoadedPackage, props: &[Property]) -> Vec<DistributionInfo> {
        let mut out = Vec::new();
        for p in props {
            let Some(d) = member(&p.value, "Distribution").and_then(as_object) else {
                continue;
            };
            let Some(target) = self.resolve_ref(lp, d) else {
                continue;
            };
            let (dlp, i) = target.parts(lp);
            let class = self
                .class_path(dlp, i)
                .map(|c| last_component(&c).to_owned())
                .unwrap_or_default();
            out.push(DistributionInfo {
                property: p.name.clone(),
                path: d.path.clone(),
                class,
                params: param_map(&self.effective(dlp, i)),
            });
        }
        out
    }

    /// Decode a `SoundCue` and walk its node graph from `FirstNode`.
    pub fn cue_graph(&self, lp: &LoadedPackage, index: usize) -> Result<CueGraph, SoundError> {
        self.note(lp);
        let cue = self.decode_cue(lp, index)?;
        let mut nodes: Vec<CueNode> = Vec::new();
        let mut edges: Vec<Vec<usize>> = Vec::new();
        let mut ids: HashMap<String, usize> = HashMap::new();
        let mut queue: VecDeque<(usize, Option<NodeData>)> = VecDeque::new();
        let mut issues = Vec::new();
        let mut dangling = Vec::new();
        let mut null_children = 0usize;
        let mut waves: Vec<String> = Vec::new();

        // Resolve a reference held by an object of `owner`: returns the node
        // id (allocating and decoding it on first sight).
        let mut visit = |owner: &LoadedPackage,
                         idx: i32,
                         path: &str,
                         nodes: &mut Vec<CueNode>,
                         edges: &mut Vec<Vec<usize>>,
                         queue: &mut VecDeque<(usize, Option<NodeData>)>,
                         issues: &mut Vec<String>,
                         dangling: &mut Vec<String>|
         -> Option<usize> {
            let key = path.to_ascii_lowercase();
            if let Some(&id) = ids.get(&key) {
                return Some(id);
            }
            if nodes.len() >= MAX_CUE_NODES {
                push_sample(
                    issues,
                    format!("more than {MAX_CUE_NODES} nodes; graph truncated"),
                );
                return None;
            }
            let pi = PackageIndex(idx);
            let r = ObjRef {
                index: idx,
                path: path.to_owned(),
            };
            let decoded = self.resolve_ref(owner, &r).map(|t| {
                let (tlp, ti) = t.parts(owner);
                self.decode_node(tlp, ti).map_err(|e| e.to_string())
            });
            let own_import = owner
                .ref_path(pi)
                .ok()
                .flatten()
                .is_some_and(|p| p.eq_ignore_ascii_case(path));
            let import_class = pi
                .import_index()
                .filter(|_| own_import)
                .and_then(|i| owner.package.import(i).ok())
                .map(|imp| owner.package.fname(imp.class_name));
            let id = nodes.len();
            match decoded {
                Some(Ok(n)) => {
                    nodes.push(CueNode {
                        path: n.path.clone(),
                        package: Some(n.package.clone()),
                        class: n.class.clone(),
                        kind: Some(n.kind),
                        resolved: true,
                        children: n.children.clone(),
                        params: n.params.clone(),
                        wave: n.wave.clone(),
                        distributions: n.distributions.clone(),
                        depth: 0,
                    });
                    edges.push(Vec::new());
                    ids.insert(key, id);
                    queue.push_back((id, Some(n)));
                }
                other => {
                    if let Some(Err(e)) = other {
                        push_sample(issues, format!("{path}: {e}"));
                    }
                    let class = import_class.unwrap_or_default();
                    nodes.push(CueNode {
                        path: path.to_owned(),
                        package: None,
                        kind: NodeKind::from_class_name(&class),
                        class,
                        resolved: false,
                        children: Vec::new(),
                        params: BTreeMap::new(),
                        wave: None,
                        distributions: Vec::new(),
                        depth: 0,
                    });
                    edges.push(Vec::new());
                    ids.insert(key, id);
                    dangling.push(path.to_owned());
                }
            }
            Some(id)
        };

        let root = match &cue.first_node {
            Some(path) => visit(
                lp,
                cue.first_node_index,
                path,
                &mut nodes,
                &mut edges,
                &mut queue,
                &mut issues,
                &mut dangling,
            ),
            None => None,
        };
        while let Some((id, data)) = queue.pop_front() {
            let Some(n) = data else { continue };
            // Children of a node decoded from another package resolve in
            // that package.
            let owner_pkg = if n.package.eq_ignore_ascii_case(&lp.name) {
                None
            } else {
                self.set.package(&n.package)
            };
            let owner: &LoadedPackage = owner_pkg.as_deref().unwrap_or(lp);
            for (child, &raw) in n.children.iter().zip(&n.child_indices) {
                let Some(path) = child else {
                    null_children += 1;
                    continue;
                };
                if let Some(cid) = visit(
                    owner,
                    raw,
                    path,
                    &mut nodes,
                    &mut edges,
                    &mut queue,
                    &mut issues,
                    &mut dangling,
                ) && let Some(e) = edges.get_mut(id)
                {
                    e.push(cid);
                }
            }
        }
        for n in &nodes {
            if n.kind.is_some_and(NodeKind::is_wave) && !waves.iter().any(|w| w == &n.path) {
                waves.push(n.path.clone());
            }
        }
        let (cycle, depths) = longest_paths(&edges, root);
        for (n, d) in nodes.iter_mut().zip(&depths) {
            n.depth = *d;
        }
        let max_depth = depths.iter().copied().max().unwrap_or(0);

        // SoundNode exports inside the cue that the graph does not reach.
        let reached: HashSet<String> = nodes.iter().map(|n| n.path.to_ascii_lowercase()).collect();
        let mut unreachable = Vec::new();
        if let Some(members) = self.cue_members(lp).get(&index) {
            for &i in members {
                if let Ok(p) = lp.qualified(i)
                    && !reached.contains(&p.to_ascii_lowercase())
                {
                    unreachable.push(p);
                }
            }
        }
        let editor_keys: BTreeSet<String> = cue
            .editor
            .iter()
            .filter_map(|e| e.object.as_ref().map(|o| o.to_ascii_lowercase()))
            .collect();
        let reached_set: BTreeSet<String> = reached.iter().cloned().collect();
        let editor_matches_graph = editor_keys == reached_set;
        let editor_extra = editor_keys.difference(&reached_set).count();
        let editor_missing = reached_set.difference(&editor_keys).count();
        let owner_class = lp
            .package
            .export(index)
            .ok()
            .and_then(|e| e.outer_index.export_index())
            .and_then(|o| lp.package.export_class_name(o).ok())
            .filter(|c| c != "Package");
        Ok(CueGraph {
            cue,
            nodes,
            waves,
            null_children,
            dangling,
            cycle,
            max_depth,
            unreachable,
            editor_matches_graph,
            editor_extra,
            editor_missing,
            owner_class,
            issues,
        })
    }

    /// `SoundNode` exports of `lp` grouped by every `SoundCue` export they
    /// lie inside (ascending export order), computed once per package with
    /// one outer-chain walk per node instead of one package scan per cue.
    fn cue_members(&self, lp: &LoadedPackage) -> CueMembers {
        let key = lp.name.to_ascii_lowercase();
        if let Some(m) = self.cue_members.borrow().get(&key) {
            return m.clone();
        }
        let mut members: HashMap<usize, Vec<usize>> = HashMap::new();
        for i in 0..lp.package.exports.len() {
            if !matches!(self.export_kind(lp, i), Some((_, SoundKind::Node(_)))) {
                continue;
            }
            let Some(r) = PackageIndex::from_export(i) else {
                continue;
            };
            let Ok(chain) = lp.package.outer_chain(r) else {
                continue;
            };
            for outer in chain.iter().skip(1) {
                if let Some(o) = outer.export_index()
                    && o != i
                    && matches!(self.export_kind(lp, o), Some((_, SoundKind::Cue)))
                {
                    let list = members.entry(o).or_default();
                    if list.last() != Some(&i) {
                        list.push(i);
                    }
                }
            }
        }
        let members = Arc::new(members);
        self.cue_members.borrow_mut().insert(key, members.clone());
        members
    }

    /// Decode a `SoundClass` export strictly (tags plus `EditorData`).
    pub fn decode_sound_class(
        &self,
        lp: &LoadedPackage,
        index: usize,
    ) -> Result<SoundClassData, SoundError> {
        self.note(lp);
        let class_path = self.class_path(lp, index).unwrap_or_default();
        if self.kind_of_class(&class_path) != Some(SoundKind::SoundClass) {
            return Err(SoundError::WrongClass {
                export: index,
                class: class_path,
                expected: "SoundClass",
            });
        }
        let cdo = is_default_object(lp, index);
        let obj = self.decode_object(lp, index)?;
        let payload = lp.package.export_data(index)?;
        let editor = if cdo {
            expect_no_tail(&obj, "SoundClass class default object")?;
            Vec::new()
        } else {
            let (entries, end) = read_editor_map(lp, payload, obj.properties_end)?;
            if end != payload.len() {
                return Err(SoundError::Malformed(format!(
                    "{}: sound class EditorData ends at {end} of {} payload bytes",
                    obj.path,
                    payload.len()
                )));
            }
            entries
        };
        let eff = self.effective_with(lp, index, &obj.properties);
        let properties = match prop(&eff, "Properties").map(param_value) {
            Some(ParamValue::Map(m)) => m,
            _ => BTreeMap::new(),
        };
        let child_class_names = match prop(&eff, "ChildClassNames") {
            Some(Value::Array(items)) => items.iter().filter_map(as_text).collect(),
            _ => Vec::new(),
        };
        let name = lp
            .package
            .export(index)
            .map(|e| lp.package.fname(e.object_name))
            .unwrap_or_default();
        Ok(SoundClassData {
            package: lp.name.clone(),
            export_index: index,
            path: obj.path,
            name,
            properties,
            child_class_names,
            is_child: bool_of(&eff, "bIsChild").unwrap_or(false),
            editor,
        })
    }

    /// Decode a `SoundMode` export strictly (tags only).
    pub fn decode_sound_mode(
        &self,
        lp: &LoadedPackage,
        index: usize,
    ) -> Result<SoundModeData, SoundError> {
        self.note(lp);
        let class_path = self.class_path(lp, index).unwrap_or_default();
        if self.kind_of_class(&class_path) != Some(SoundKind::SoundMode) {
            return Err(SoundError::WrongClass {
                export: index,
                class: class_path,
                expected: "SoundMode",
            });
        }
        let obj = self.decode_object(lp, index)?;
        expect_no_tail(&obj, "SoundMode")?;
        let eff = self.effective_with(lp, index, &obj.properties);
        let name = lp
            .package
            .export(index)
            .map(|e| lp.package.fname(e.object_name))
            .unwrap_or_default();
        Ok(SoundModeData {
            package: lp.name.clone(),
            export_index: index,
            path: obj.path,
            name,
            params: param_map(&eff),
        })
    }

    /// Ambient sound actors and reverb volumes placed in `lp` (exports of
    /// those classes that are neither class default objects nor archetypes),
    /// linked to `ULevel::Actors` slots when the package has a level.
    pub fn map_audio(&self, lp: &LoadedPackage) -> MapAudio {
        self.note(lp);
        let mut out = MapAudio {
            package: lp.name.clone(),
            ..MapAudio::default()
        };
        let mut slots: HashMap<usize, usize> = HashMap::new();
        for level_index in level::level_exports(&lp.package) {
            match level::decode_level(&lp.package, Some(&lp.name), level_index, self.schema()) {
                Ok((_, tail)) => {
                    for (slot, a) in tail.actors.iter().enumerate() {
                        if let Some(i) = a.export_index() {
                            slots.entry(i).or_insert(slot);
                        }
                    }
                }
                Err(e) => push_sample(&mut out.issues, format!("level export {level_index}: {e}")),
            }
        }
        for i in 0..lp.package.exports.len() {
            if is_default_object(lp, i) || is_archetype(lp, i) {
                continue;
            }
            let Some(class_path) = self.class_path(lp, i) else {
                continue;
            };
            let chain = self.class_chain(&class_path);
            if let Some(kind) = AmbientKind::classify(&class_path, &chain) {
                out.ambient.push(self.ambient_actor(
                    lp,
                    i,
                    &class_path,
                    kind,
                    slots.get(&i).copied(),
                ));
            } else if is_reverb_volume(&class_path, &chain) {
                let eff = self.effective(lp, i);
                out.reverb.push(ReverbVolumeInfo {
                    export_index: i,
                    name: export_name(lp, i),
                    path: lp.qualified(i).unwrap_or_default(),
                    class: last_component(&class_path).to_owned(),
                    level_slot: slots.get(&i).copied(),
                    location: vec3_of(&eff, "Location"),
                    priority: f32_of(&eff, "Priority"),
                    enabled: bool_of(&eff, "bEnabled"),
                    settings: prop(&eff, "Settings").map(param_value),
                    ambient_zone: prop(&eff, "AmbientZoneSettings").map(param_value),
                });
            }
        }
        out
    }

    fn ambient_actor(
        &self,
        lp: &LoadedPackage,
        i: usize,
        class_path: &str,
        kind: AmbientKind,
        level_slot: Option<usize>,
    ) -> AmbientActor {
        let own = self
            .decode_object(lp, i)
            .map(|o| o.properties)
            .unwrap_or_default();
        let eff = self.effective_with(lp, i, &own);
        let resolve = |r: &ObjRef| self.resolve_ref(lp, r);
        let component_ref = prop(&eff, "AudioComponent").and_then(as_object).cloned();
        let mut sound_cue = None;
        let mut volume_multiplier = None;
        let mut pitch_multiplier = None;
        let mut audio_component_class = None;
        let mut audio_component_params = BTreeMap::new();
        if let Some(c) = &component_ref
            && let Some(target) = resolve(c)
        {
            let (clp, ci) = target.parts(lp);
            let ceff = self.effective(clp, ci);
            sound_cue = object_of(&ceff, "SoundCue");
            volume_multiplier = f32_of(&ceff, "VolumeMultiplier");
            pitch_multiplier = f32_of(&ceff, "PitchMultiplier");
            audio_component_class = self
                .class_path(clp, ci)
                .map(|p| last_component(&p).to_owned());
            if let Ok(o) = self.decode_object(clp, ci) {
                audio_component_params = params_without(&o.properties, &["PreviewSoundRadius"]);
            }
        }
        if sound_cue.is_none() {
            sound_cue = object_of(&eff, "SoundCueInstance");
        }
        let node_ref = prop(&eff, "AmbientProperties")
            .and_then(as_object)
            .or_else(|| prop(&eff, "SoundNodeInstance").and_then(as_object))
            .cloned();
        let ambient_node = node_ref.and_then(|n| {
            let target = resolve(&n)?;
            let (nlp, ni) = target.parts(lp);
            let neff = self.effective(nlp, ni);
            let waves = match prop(&neff, "SoundSlots") {
                Some(Value::Array(slots)) => slots
                    .iter()
                    .filter_map(|s| member(s, "Wave").and_then(as_object))
                    .map(|o| o.path.clone())
                    .collect(),
                _ => Vec::new(),
            };
            Some(AmbientNodeInfo {
                path: n.path.clone(),
                class: self
                    .class_path(nlp, ni)
                    .map(|c| last_component(&c).to_owned())
                    .unwrap_or_default(),
                params: params_without(&neff, &["ChildNodes"]),
                waves,
            })
        });
        AmbientActor {
            export_index: i,
            name: export_name(lp, i),
            path: lp.qualified(i).unwrap_or_default(),
            class: last_component(class_path).to_owned(),
            kind,
            level_slot,
            location: vec3_of(&eff, "Location"),
            rotation: rotator_of(&eff, "Rotation"),
            draw_scale: f32_of(&eff, "DrawScale").unwrap_or(1.0),
            auto_play: bool_of(&eff, "bAutoPlay"),
            is_playing: bool_of(&eff, "bIsPlaying"),
            audio_component: component_ref.map(|c| c.path),
            audio_component_class,
            audio_component_params,
            sound_cue,
            volume_multiplier,
            pitch_multiplier,
            ambient_node,
            instance: params_without(
                &own,
                &[
                    "Location",
                    "Rotation",
                    "DrawScale",
                    "AudioComponent",
                    "AmbientProperties",
                    "SoundCueInstance",
                    "SoundNodeInstance",
                    "SpriteComp",
                    "Components",
                    "Tag",
                ],
            ),
        }
    }
}

fn export_name(lp: &LoadedPackage, i: usize) -> String {
    lp.package
        .export(i)
        .map(|e| lp.package.fname(e.object_name))
        .unwrap_or_default()
}

fn expect_no_tail(obj: &DecodedObject, what: &str) -> Result<(), SoundError> {
    if obj.native_tail() != 0 {
        return Err(SoundError::Malformed(format!(
            "{} ({what}): {} bytes after the tagged properties, none expected",
            obj.path,
            obj.native_tail()
        )));
    }
    Ok(())
}

/// `ChildNodes` of `props`: paths (`None` for null) and raw indices.
fn child_refs(props: &[Property]) -> (Vec<Option<String>>, Vec<i32>) {
    let Some(Value::Array(items)) = prop(props, "ChildNodes") else {
        return (Vec::new(), Vec::new());
    };
    items
        .iter()
        .map(|v| match v {
            Value::Object(o) if o.index != 0 => (Some(o.path.clone()), o.index),
            _ => (None, 0),
        })
        .unzip()
}

/// Longest path from `root` to every node (0 for unreachable ones) and
/// whether the graph has a cycle (Kahn's algorithm over all nodes).
fn longest_paths(edges: &[Vec<usize>], root: Option<usize>) -> (bool, Vec<usize>) {
    let n = edges.len();
    let mut indegree = vec![0usize; n];
    for e in edges {
        for &t in e {
            if let Some(d) = indegree.get_mut(t) {
                *d += 1;
            }
        }
    }
    let mut queue: VecDeque<usize> = (0..n).filter(|&i| indegree[i] == 0).collect();
    let mut order = Vec::with_capacity(n);
    while let Some(i) = queue.pop_front() {
        order.push(i);
        for &t in edges.get(i).map(Vec::as_slice).unwrap_or(&[]) {
            if let Some(d) = indegree.get_mut(t) {
                *d -= 1;
                if *d == 0 {
                    queue.push_back(t);
                }
            }
        }
    }
    let cycle = order.len() != n;
    let mut depth = vec![0usize; n];
    let mut reached = vec![false; n];
    if let Some(r) = root
        && r < n
    {
        reached[r] = true;
    }
    for &i in &order {
        if !reached[i] {
            continue;
        }
        let d = depth[i];
        for &t in edges.get(i).map(Vec::as_slice).unwrap_or(&[]) {
            if t < n {
                reached[t] = true;
                depth[t] = depth[t].max(d + 1);
            }
        }
    }
    (cycle, depth)
}

// ---------------------------------------------------------------------------
// Coverage
// ---------------------------------------------------------------------------

/// Decoding counts for one class.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct ClassStats {
    /// Exports of the class.
    pub total: usize,
    /// Of which class default objects.
    pub default_objects: usize,
    /// Exports decoded with exact payload consumption.
    pub exact: usize,
    /// Exports that failed.
    pub failed: usize,
}

/// Counts for one bulk slot of the wave native data.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct SlotStats {
    /// Records with no elements (and no stored bytes).
    pub empty: usize,
    /// Records holding inline bytes.
    pub inline_filled: usize,
    /// Records stored in a separate file.
    pub separate_file: usize,
    /// Records marked unused.
    pub unused: usize,
    /// Records with a compression flag.
    pub compressed: usize,
    /// Stored bytes over all records.
    pub stored_bytes: u64,
    /// Record flag values seen.
    pub flags: BTreeMap<String, usize>,
}

/// Wave statistics.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct WaveStats {
    /// Non-default waves decoded.
    pub decoded: usize,
    /// Waves in localized (`_LOC_<LANG>`) packages.
    pub localized: usize,
    /// `NumChannels` histogram.
    pub channels: BTreeMap<i32, usize>,
    /// `SampleRate` histogram.
    pub sample_rates: BTreeMap<i32, usize>,
    /// Sum of `Duration` in seconds.
    pub total_duration_s: f64,
    /// Waves with `bLoopingSound` set.
    pub looping: usize,
    /// Per bulk slot.
    pub slots: BTreeMap<String, SlotStats>,
    /// Waves by which slots hold data (`CompressedPCData`, ... joined by `+`).
    pub filled_slot_sets: BTreeMap<String, usize>,
    /// Container of the audio payload.
    pub payload_formats: BTreeMap<PayloadFormat, usize>,
    /// Waves whose inline record offsets all equal their stream positions.
    pub inline_offsets_match: usize,
    /// Waves where they do not.
    pub inline_offset_mismatches: usize,
    /// Payloads that failed to load.
    pub load_failures: usize,
    /// Distinct wave paths.
    pub unique_paths: usize,
    /// Waves whose path also occurs in an earlier package.
    pub duplicate_copies: usize,
    /// Duplicates whose audio bytes equal the first copy.
    pub identical_duplicates: usize,
    /// Duplicates whose audio bytes differ.
    pub differing_duplicates: usize,
    /// Tag names stored on waves (how often).
    pub tag_names: BTreeMap<String, usize>,
}

/// Ogg checks over the wave payloads.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct OggStats {
    /// Streams parsed without error.
    pub parsed: usize,
    /// Streams that failed to parse.
    pub failed: usize,
    /// Pages.
    pub pages: usize,
    /// Pages with a checksum mismatch.
    pub crc_mismatches: usize,
    /// Streams with more than one logical bitstream.
    pub multiplexed: usize,
    /// Sequence number gaps.
    pub sequence_gaps: usize,
    /// Streams whose first page lacks the BOS flag.
    pub missing_bos: usize,
    /// Streams whose last page lacks the EOS flag.
    pub missing_eos: usize,
    /// Streams without the three Vorbis header packets.
    pub incomplete_headers: usize,
    /// Pages whose continued-packet flag disagrees with the page before.
    pub continuation_errors: usize,
    /// Pages whose granule position goes backwards.
    pub granule_regressions: usize,
    /// Streams that end inside a packet.
    pub unterminated_packets: usize,
    /// Streams that pass every container check ([`OggInfo::is_valid_vorbis`]).
    pub valid_streams: usize,
    /// Vorbis `audio_channels` equals `NumChannels`.
    pub channels_match: usize,
    /// It does not.
    pub channel_mismatches: usize,
    /// Vorbis `audio_sample_rate` equals `SampleRate`.
    pub rate_match: usize,
    /// It does not.
    pub rate_mismatches: usize,
    /// `RawPCMDataSize` equals final granule x channels x 2.
    pub pcm_size_match: usize,
    /// It does not.
    pub pcm_size_mismatches: usize,
    /// |`Duration` - final granule / rate| below one millisecond.
    pub duration_match: usize,
    /// Larger differences.
    pub duration_mismatches: usize,
    /// Largest |`Duration` - final granule / rate| in seconds.
    pub max_duration_error_s: f64,
    /// Encoder vendor strings (comment header).
    pub vendors: BTreeMap<String, usize>,
    /// Streams whose comment header holds user comments.
    pub with_user_comments: usize,
    /// Nominal bitrate histogram (bits per second).
    pub nominal_bitrates: BTreeMap<i32, usize>,
    /// Failure samples.
    pub failures: Vec<String>,
}

/// Subtitle statistics (counts only: the text is game content).
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct SubtitleStats {
    /// Waves with plain `Subtitles` lines.
    pub waves_with_subtitles: usize,
    /// Plain `Subtitles` lines.
    pub lines: usize,
    /// Waves with a `LocalizedSubtitles` array.
    pub waves_with_localized: usize,
    /// `LocalizedSubtitles` array lengths.
    pub localized_array_lengths: BTreeMap<usize, usize>,
    /// Slots with an empty `LanguageExt`.
    pub empty_slots: usize,
    /// Waves with non-empty lines per `LanguageExt`.
    pub waves_per_language: BTreeMap<String, usize>,
    /// Lines per `LanguageExt`.
    pub lines_per_language: BTreeMap<String, usize>,
    /// `LanguageExt` order of the first array seen (slot -> language).
    pub slot_languages: Vec<String>,
    /// Arrays whose slot languages differ from the first array's.
    pub slot_order_differences: usize,
    /// Waves whose plain `Subtitles` equal the `INT` entry.
    pub plain_equals_int: usize,
    /// Waves where they differ.
    pub plain_differs_int: usize,
    /// Waves with lines in a localized package.
    pub in_localized_packages: usize,
    /// Waves with lines outside localized packages.
    pub outside_localized_packages: usize,
    /// Line lists whose times are not non-decreasing.
    pub non_monotonic_times: usize,
    /// Lines starting after the wave's `Duration`.
    pub lines_after_duration: usize,
    /// `bMature` waves.
    pub mature: usize,
    /// `bManualWordWrap` waves.
    pub manual_word_wrap: usize,
    /// `bSingleLine` waves.
    pub single_line: usize,
    /// Waves with `SpokenText`.
    pub spoken_text: usize,
    /// Waves with `bUseTTS`.
    pub use_tts: usize,
}

/// Cue graph statistics.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct CueStats {
    /// Non-default cues decoded (exact consumption).
    pub decoded: usize,
    /// Graphs built.
    pub graphs: usize,
    /// Nodes reached per class.
    pub nodes: BTreeMap<String, usize>,
    /// Graph nodes in total.
    pub total_nodes: usize,
    /// Cues without `FirstNode`.
    pub without_first_node: usize,
    /// Wave leaves reached.
    pub wave_leaves: usize,
    /// Wave leaves decoded from another package (imports).
    pub cross_package_leaves: usize,
    /// References that could not be resolved.
    pub dangling: usize,
    /// Dangling samples.
    pub dangling_samples: Vec<String>,
    /// Empty child inputs.
    pub null_children: usize,
    /// Graphs with a cycle.
    pub cycles: usize,
    /// Depth histogram (longest path).
    pub depths: BTreeMap<usize, usize>,
    /// Node exports inside cues not reachable from `FirstNode`.
    pub unreachable_nodes: usize,
    /// `EditorData` entries.
    pub editor_entries: usize,
    /// Cues with an empty `EditorData` map.
    pub editor_empty: usize,
    /// Empty `EditorData` maps by the class of the cue's owner (`package`
    /// when the cue sits directly in a package).
    pub editor_empty_by_owner: BTreeMap<String, usize>,
    /// Non-empty maps: keys that are not reachable nodes.
    pub editor_extra_keys: usize,
    /// Non-empty maps: reachable nodes without an entry.
    pub editor_missing_nodes: usize,
    /// Cues whose `EditorData` keys equal the reachable node set.
    pub editor_matches_graph: usize,
    /// Cues where they differ.
    pub editor_differs: usize,
    /// `SoundClass` names used by cues.
    pub sound_classes: BTreeMap<String, usize>,
    /// Cues whose `SoundClass` names no `SoundClass` object.
    pub unresolved_sound_class: usize,
    /// Those `SoundClass` names.
    pub unresolved_sound_class_names: BTreeMap<String, usize>,
    /// Cues per `Duration` value class: `looping` (10000), `finite`, `absent`.
    pub duration_classes: BTreeMap<String, usize>,
    /// Graph issue samples.
    pub issue_samples: Vec<String>,
}

/// Ambient statistics.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct AmbientStats {
    /// Actors per class.
    pub actors: BTreeMap<String, usize>,
    /// Actors listed in `ULevel::Actors`.
    pub in_level: usize,
    /// Actors with a cue.
    pub with_cue: usize,
    /// Actors whose cue was found as an export or located import.
    pub cue_resolved: usize,
    /// Actors with an inline ambient node.
    pub with_ambient_node: usize,
    /// Ambient node sound slots.
    pub ambient_slots: usize,
    /// Reverb volumes.
    pub reverb_volumes: usize,
    /// Issue samples.
    pub issues: Vec<String>,
}

/// Per-package counts.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct PackageSoundCoverage {
    /// Package.
    pub package: String,
    /// Waves.
    pub waves: usize,
    /// Cues.
    pub cues: usize,
    /// Other nodes.
    pub nodes: usize,
    /// Sound classes and modes.
    pub classes_and_modes: usize,
    /// Ambient actors.
    pub ambient_actors: usize,
    /// Failures.
    pub failures: usize,
}

/// Coverage of the sound decoders over a set of packages.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct SoundCoverage {
    /// Packages examined.
    pub packages: usize,
    /// Packages with at least one sound object.
    pub per_package: Vec<PackageSoundCoverage>,
    /// Per class name.
    pub classes: BTreeMap<String, ClassStats>,
    /// Waves.
    pub waves: WaveStats,
    /// Ogg checks.
    pub ogg: OggStats,
    /// Subtitles.
    pub subtitles: SubtitleStats,
    /// Cue graphs.
    pub cues: CueStats,
    /// Sound classes decoded.
    pub sound_classes: usize,
    /// Sound class `EditorData` entries.
    pub sound_class_editor_entries: usize,
    /// Sound modes decoded.
    pub sound_modes: usize,
    /// Ambient actors and reverb volumes.
    pub ambient: AmbientStats,
    /// Failure samples.
    pub failures: Vec<String>,
    /// Total failures.
    pub failure_count: usize,
    #[serde(skip)]
    state: CoverageState,
}

/// Cross-package bookkeeping of [`SoundCoverage`] (not reported).
#[derive(Clone, Default, PartialEq)]
struct CoverageState {
    wave_hashes: HashMap<String, (String, u64)>,
    sound_class_names: BTreeSet<String>,
    cue_sound_classes: Vec<String>,
}

impl std::fmt::Debug for CoverageState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("CoverageState { .. }")
    }
}

/// True when the package's file name marks it as localized
/// (`<name>_LOC_<LANG>`).
pub fn is_localized_package(name: &str) -> bool {
    name.to_ascii_lowercase().contains("_loc_")
}

/// Language of a localized package name (`AG-Workshop_LOC_INT` -> `INT`).
pub fn package_language(name: &str) -> Option<String> {
    let lower = name.to_ascii_lowercase();
    let at = lower.rfind("_loc_")?;
    let lang = name.get(at + 5..)?;
    (!lang.is_empty()).then(|| lang.to_ascii_uppercase())
}

impl SoundCoverage {
    fn fail(&mut self, class: &str, msg: String) {
        self.failure_count += 1;
        push_sample(&mut self.failures, msg);
        self.classes.entry(class.to_owned()).or_default().failed += 1;
    }

    /// Decode every sound export of `lp` (waves with payload and Ogg checks,
    /// cues with graphs, nodes, classes, modes) and its ambient actors.
    pub fn add_package(&mut self, dec: &SoundDecoder<'_>, lp: &LoadedPackage) {
        self.packages += 1;
        let mut pc = PackageSoundCoverage {
            package: lp.name.clone(),
            ..PackageSoundCoverage::default()
        };
        let localized = is_localized_package(&lp.name);
        for i in 0..lp.package.exports.len() {
            let Some((class_path, kind)) = dec.export_kind(lp, i) else {
                continue;
            };
            let class = last_component(&class_path).to_owned();
            let cdo = is_default_object(lp, i);
            {
                let s = self.classes.entry(class.clone()).or_default();
                s.total += 1;
                if cdo {
                    s.default_objects += 1;
                }
            }
            let ok = match kind {
                SoundKind::Node(k) if k.is_wave() => {
                    pc.waves += 1;
                    self.add_wave(dec, lp, i, localized)
                }
                SoundKind::Node(_) => {
                    pc.nodes += 1;
                    dec.decode_node(lp, i).map(|_| ())
                }
                SoundKind::Cue => {
                    pc.cues += 1;
                    self.add_cue(dec, lp, i, cdo)
                }
                SoundKind::SoundClass => {
                    pc.classes_and_modes += 1;
                    dec.decode_sound_class(lp, i).map(|c| {
                        if !cdo {
                            self.sound_classes += 1;
                            self.sound_class_editor_entries += c.editor.len();
                            self.state
                                .sound_class_names
                                .insert(c.name.to_ascii_lowercase());
                        }
                    })
                }
                SoundKind::SoundMode => {
                    pc.classes_and_modes += 1;
                    dec.decode_sound_mode(lp, i).map(|_| {
                        if !cdo {
                            self.sound_modes += 1;
                        }
                    })
                }
            };
            match ok {
                Ok(()) => self.classes.entry(class).or_default().exact += 1,
                Err(e) => {
                    pc.failures += 1;
                    let path = lp.qualified(i).unwrap_or_default();
                    self.fail(&class, format!("{} {path}: {e}", lp.name));
                }
            }
        }
        let audio = dec.map_audio(lp);
        pc.ambient_actors = audio.ambient.len();
        for msg in &audio.issues {
            push_sample(&mut self.ambient.issues, format!("{}: {msg}", lp.name));
        }
        for a in &audio.ambient {
            bump(&mut self.ambient.actors, a.class.clone());
            if a.level_slot.is_some() {
                self.ambient.in_level += 1;
            }
            if let Some(cue) = &a.sound_cue {
                self.ambient.with_cue += 1;
                if dec.locate_from(lp, cue).is_some() {
                    self.ambient.cue_resolved += 1;
                }
            }
            if let Some(n) = &a.ambient_node {
                self.ambient.with_ambient_node += 1;
                self.ambient.ambient_slots += n.waves.len();
            }
        }
        self.ambient.reverb_volumes += audio.reverb.len();
        if pc.waves + pc.cues + pc.nodes + pc.classes_and_modes + pc.ambient_actors > 0 {
            self.per_package.push(pc);
        }
    }

    fn add_wave(
        &mut self,
        dec: &SoundDecoder<'_>,
        lp: &LoadedPackage,
        i: usize,
        localized: bool,
    ) -> Result<(), SoundError> {
        let w = dec.decode_wave(lp, i)?;
        if w.is_default_object {
            return Ok(());
        }
        let ws = &mut self.waves;
        ws.decoded += 1;
        if localized {
            ws.localized += 1;
        }
        for t in &w.tagged {
            bump(&mut ws.tag_names, t.clone());
        }
        if let Some(c) = w.props.num_channels {
            bump(&mut ws.channels, c);
        }
        if let Some(r) = w.props.sample_rate {
            bump(&mut ws.sample_rates, r);
        }
        ws.total_duration_s += f64::from(w.props.duration.unwrap_or(0.0));
        if w.props.looping == Some(true) {
            ws.looping += 1;
        }
        if w.inline_offsets_match() {
            ws.inline_offsets_match += 1;
        } else {
            ws.inline_offset_mismatches += 1;
        }
        let mut filled = Vec::new();
        if let Some(n) = &w.native {
            for (slot, rec) in n.records.iter().enumerate() {
                let name = WAVE_BULK_SLOTS.get(slot).copied().unwrap_or("?");
                let s = ws.slots.entry(name.to_owned()).or_default();
                bump(&mut s.flags, format!("{:#x}", rec.flags));
                match rec.storage() {
                    BulkStorage::Unused => s.unused += 1,
                    BulkStorage::SeparateFile => s.separate_file += 1,
                    BulkStorage::Inline if rec.stored_len() > 0 => s.inline_filled += 1,
                    BulkStorage::Inline => s.empty += 1,
                }
                if rec.compression() != BulkCompression::None {
                    s.compressed += 1;
                }
                s.stored_bytes += rec.stored_len() as u64;
                if !rec.is_empty() {
                    filled.push(name);
                }
            }
        }
        bump(
            &mut ws.filled_slot_sets,
            if filled.is_empty() {
                "none".to_owned()
            } else {
                filled.join("+")
            },
        );
        // Audio payload.
        let payload = lp.package.export_data(i)?;
        if let Some(slot) = w.audio_slot() {
            match w.load_slot(payload, slot) {
                Ok(bytes) => {
                    let fmt = sniff_payload(&bytes);
                    bump(&mut self.waves.payload_formats, fmt);
                    if matches!(fmt, PayloadFormat::OggVorbis | PayloadFormat::OggOther) {
                        self.check_ogg(&w, &bytes);
                    }
                    self.record_copy(&w, &bytes);
                }
                Err(e) => {
                    self.waves.load_failures += 1;
                    push_sample(&mut self.failures, format!("{} {}: {e}", lp.name, w.path));
                }
            }
        } else {
            bump(&mut self.waves.payload_formats, PayloadFormat::Empty);
        }
        self.add_subtitles(&w, localized);
        Ok(())
    }

    fn record_copy(&mut self, w: &SoundWave, bytes: &[u8]) {
        let key = w.path.to_ascii_lowercase();
        let h = content_hash(bytes);
        match self.state.wave_hashes.get(&key) {
            Some((_, first)) => {
                self.waves.duplicate_copies += 1;
                if *first == h {
                    self.waves.identical_duplicates += 1;
                } else {
                    self.waves.differing_duplicates += 1;
                }
            }
            None => {
                self.state.wave_hashes.insert(key, (w.package.clone(), h));
                self.waves.unique_paths = self.state.wave_hashes.len();
            }
        }
    }

    fn check_ogg(&mut self, w: &SoundWave, bytes: &[u8]) {
        let o = &mut self.ogg;
        let info = match parse_ogg(bytes) {
            Ok(i) => i,
            Err(e) => {
                o.failed += 1;
                push_sample(&mut o.failures, format!("{}: {e}", w.path));
                return;
            }
        };
        o.parsed += 1;
        o.pages += info.pages;
        o.crc_mismatches += info.crc_mismatches;
        o.sequence_gaps += info.sequence_gaps;
        if info.streams > 1 {
            o.multiplexed += 1;
        }
        if !info.first_page_bos {
            o.missing_bos += 1;
        }
        if !info.last_page_eos {
            o.missing_eos += 1;
        }
        if info.vorbis.is_none() || info.vendor.is_none() || !info.setup_header {
            o.incomplete_headers += 1;
        }
        o.continuation_errors += info.continuation_errors;
        o.granule_regressions += info.granule_regressions;
        if info.unterminated_packet {
            o.unterminated_packets += 1;
        }
        if info.is_valid_vorbis() {
            o.valid_streams += 1;
        }
        if let Some(v) = &info.vendor {
            bump(&mut o.vendors, v.clone());
        }
        if info.comments.is_some_and(|c| c > 0) {
            o.with_user_comments += 1;
        }
        if let Some(v) = &info.vorbis {
            bump(&mut o.nominal_bitrates, v.bitrate_nominal);
            if w.props.num_channels == Some(i32::from(v.channels)) {
                o.channels_match += 1;
            } else {
                o.channel_mismatches += 1;
                push_sample(
                    &mut o.failures,
                    format!(
                        "{}: {} Vorbis channels, NumChannels {:?}",
                        w.path, v.channels, w.props.num_channels
                    ),
                );
            }
            if w.props
                .sample_rate
                .is_some_and(|r| u32::try_from(r).ok() == Some(v.sample_rate))
            {
                o.rate_match += 1;
            } else {
                o.rate_mismatches += 1;
            }
            if let (Some(g), Some(size)) = (info.final_granule, w.props.raw_pcm_data_size) {
                let expect = g
                    .checked_mul(i64::from(v.channels))
                    .and_then(|x| x.checked_mul(2));
                if expect == Some(i64::from(size)) {
                    o.pcm_size_match += 1;
                } else {
                    o.pcm_size_mismatches += 1;
                }
            }
        }
        if let (Some(d), Some(prop_d)) = (info.duration(), w.props.duration) {
            let err = (d - f64::from(prop_d)).abs();
            if err < 0.001 {
                o.duration_match += 1;
            } else {
                o.duration_mismatches += 1;
            }
            if err > o.max_duration_error_s {
                o.max_duration_error_s = err;
            }
        }
    }

    fn add_subtitles(&mut self, w: &SoundWave, localized: bool) {
        let s = &mut self.subtitles;
        let p = &w.props;
        if p.mature {
            s.mature += 1;
        }
        if p.manual_word_wrap {
            s.manual_word_wrap += 1;
        }
        if p.single_line {
            s.single_line += 1;
        }
        if p.spoken_text.is_some() {
            s.spoken_text += 1;
        }
        if p.use_tts {
            s.use_tts += 1;
        }
        let monotonic = |lines: &[SubtitleCue]| lines.windows(2).all(|w| w[0].time <= w[1].time);
        let mut any = false;
        if !p.subtitles.is_empty() {
            any = true;
            s.waves_with_subtitles += 1;
            s.lines += p.subtitles.len();
            if !monotonic(&p.subtitles) {
                s.non_monotonic_times += 1;
            }
            if let Some(d) = p.duration {
                s.lines_after_duration += p.subtitles.iter().filter(|l| l.time > d).count();
            }
        }
        if !p.localized_subtitles.is_empty() {
            s.waves_with_localized += 1;
            bump(&mut s.localized_array_lengths, p.localized_subtitles.len());
            let langs: Vec<String> = p
                .localized_subtitles
                .iter()
                .map(|l| l.language.clone())
                .collect();
            if s.slot_languages.is_empty() {
                s.slot_languages = langs;
            } else if s.slot_languages != langs {
                s.slot_order_differences += 1;
            }
            for l in &p.localized_subtitles {
                if l.language.is_empty() {
                    s.empty_slots += 1;
                }
                if !l.subtitles.is_empty() {
                    any = true;
                    bump(&mut s.waves_per_language, l.language.clone());
                    *s.lines_per_language.entry(l.language.clone()).or_insert(0) +=
                        l.subtitles.len();
                    if !monotonic(&l.subtitles) {
                        s.non_monotonic_times += 1;
                    }
                }
            }
            if let Some(int) = p
                .localized_subtitles
                .iter()
                .find(|l| l.language.eq_ignore_ascii_case("INT"))
            {
                if int.subtitles == p.subtitles {
                    s.plain_equals_int += 1;
                } else {
                    s.plain_differs_int += 1;
                }
            }
        }
        if any {
            if localized {
                s.in_localized_packages += 1;
            } else {
                s.outside_localized_packages += 1;
            }
        }
    }

    fn add_cue(
        &mut self,
        dec: &SoundDecoder<'_>,
        lp: &LoadedPackage,
        i: usize,
        cdo: bool,
    ) -> Result<(), SoundError> {
        if cdo {
            return dec.decode_cue(lp, i).map(|_| ());
        }
        let g = dec.cue_graph(lp, i)?;
        let c = &mut self.cues;
        c.decoded += 1;
        c.graphs += 1;
        c.editor_entries += g.cue.editor.len();
        if g.cue.editor.is_empty() {
            c.editor_empty += 1;
            bump(
                &mut c.editor_empty_by_owner,
                g.owner_class
                    .clone()
                    .unwrap_or_else(|| "package".to_owned()),
            );
        } else {
            c.editor_extra_keys += g.editor_extra;
            c.editor_missing_nodes += g.editor_missing;
        }
        if g.editor_matches_graph {
            c.editor_matches_graph += 1;
        } else {
            c.editor_differs += 1;
        }
        if g.cue.first_node.is_none() {
            c.without_first_node += 1;
        }
        bump(
            &mut c.duration_classes,
            match g.cue.duration {
                None => "absent".to_owned(),
                Some(d) if d >= 10_000.0 => "looping".to_owned(),
                Some(_) => "finite".to_owned(),
            },
        );
        match &g.cue.sound_class {
            Some(s) => {
                bump(&mut c.sound_classes, s.clone());
                self.state.cue_sound_classes.push(s.clone());
            }
            None => bump(&mut c.sound_classes, "(none)".to_owned()),
        }
        c.total_nodes += g.nodes.len();
        for n in &g.nodes {
            bump(&mut c.nodes, n.class.clone());
            if n.kind.is_some_and(NodeKind::is_wave) {
                c.wave_leaves += 1;
                if n.package
                    .as_deref()
                    .is_some_and(|p| !p.eq_ignore_ascii_case(&lp.name))
                {
                    c.cross_package_leaves += 1;
                }
            }
        }
        c.dangling += g.dangling.len();
        for d in &g.dangling {
            push_sample(
                &mut c.dangling_samples,
                format!("{} {}: {d}", lp.name, g.cue.path),
            );
        }
        c.null_children += g.null_children;
        if g.cycle {
            c.cycles += 1;
        }
        bump(&mut c.depths, g.max_depth);
        c.unreachable_nodes += g.unreachable.len();
        for issue in &g.issues {
            push_sample(&mut c.issue_samples, format!("{}: {issue}", g.cue.path));
        }
        Ok(())
    }

    /// Cross-checks that need every package (cue sound classes against the
    /// `SoundClass` objects).
    pub fn finish(&mut self) {
        let names = &self.state.sound_class_names;
        let mut unresolved = BTreeMap::new();
        for s in &self.state.cue_sound_classes {
            if !names.contains(&s.to_ascii_lowercase()) {
                bump(&mut unresolved, s.clone());
            }
        }
        self.cues.unresolved_sound_class = unresolved.values().sum();
        self.cues.unresolved_sound_class_names = unresolved;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc_known_answer() {
        // CRC-32/MPEG-2 style without final XOR and with zero init: the
        // check value of "123456789" for poly 0x04C11DB7, init 0, no
        // reflection, no xorout is 0x89A1897F.
        assert_eq!(ogg_crc(b"123456789"), 0x89A1_897F);
    }

    #[test]
    fn classification() {
        assert_eq!(
            SoundKind::from_class_name("Engine.SoundNodeWave"),
            Some(SoundKind::Node(NodeKind::Wave))
        );
        assert_eq!(SoundKind::from_class_name("SoundCue"), Some(SoundKind::Cue));
        assert_eq!(SoundKind::from_class_name("Engine.Texture2D"), None);
        let chain = vec!["soundnoderandom".to_owned(), "soundnode".to_owned()];
        assert_eq!(
            SoundKind::classify("Game.MyRandom", &chain),
            Some(SoundKind::Node(NodeKind::Random))
        );
        assert_eq!(
            AmbientKind::from_class_name("Engine.AmbientSoundSimple"),
            Some(AmbientKind::Simple)
        );
        assert_eq!(
            AmbientKind::classify("Game.MyAmbient", &["ambientsound".to_owned()]),
            Some(AmbientKind::Other)
        );
        assert!(is_reverb_volume("Engine.ReverbVolume", &[]));
        assert_eq!(
            package_language("AG-Workshop_LOC_INT").as_deref(),
            Some("INT")
        );
        assert_eq!(package_language("Startup"), None);
        assert!(is_localized_package("Startup_LOC_INT"));
    }

    #[test]
    fn longest_paths_and_cycles() {
        let (cycle, d) = longest_paths(&[vec![1, 2], vec![2], vec![]], Some(0));
        assert!(!cycle);
        assert_eq!(d, vec![0, 1, 2]);
        let (cycle, _) = longest_paths(&[vec![1], vec![0]], Some(0));
        assert!(cycle);
        let (cycle, d) = longest_paths(&[vec![5]], Some(9));
        assert!(!cycle);
        assert_eq!(d, vec![0]);
    }
}
