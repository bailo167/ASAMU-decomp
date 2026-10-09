//! `AnimSet` / `AnimSequence` decoding for UE3 v868 / licensee 0.
//!
//! An `AnimSet` is tagged properties only (no native tail): the track-to-bone
//! table `TrackBoneNames`, the `Sequences` it owns, `bAnimRotationOnly`
//! (class default **true**), `UseTranslationBoneNames`,
//! `ForceMeshTranslationBoneNames` and the preview mesh name.
//!
//! An `AnimSequence` is tagged properties (`SequenceName`, `SequenceLength`,
//! `NumFrames`, `RateScale`, the compression formats, `CompressedTrackOffsets`,
//! notifies, additive data, ...) followed by native data written by
//! `UAnimSequence::Serialize`:
//!
//! ```text
//! TArray<FRawAnimSequenceTrack> RawAnimationData   per track: bulk FVector[] PosKeys,
//!                                                  bulk FQuat[] RotKeys
//! i32                           NumBytes
//! u8[NumBytes]                  CompressedByteStream
//! ```
//!
//! The byte stream holds one translation and one rotation track per
//! `TrackBoneNames` entry. Its layout depends on `KeyEncodingFormat`:
//!
//! - **ConstantKeyLerp / VariableKeyLerp** ("legacy" codecs): four offsets per
//!   track in `CompressedTrackOffsets` (translation offset, translation key
//!   count, rotation offset, rotation key count). A track is an optional
//!   24-byte `Mins[3], Ranges[3]` header (interval format with more than one
//!   key), the keys, and for VariableKeyLerp with more than one key a key ->
//!   frame table (`u8`, or `u16` when `NumFrames > 255`) after padding to 4.
//!   Each track is padded to 4 bytes. A one-key rotation is always stored as
//!   `Float96NoW`, a one-key translation as three floats.
//! - **PerTrackCompression**: two offsets per track (translation, rotation;
//!   -1 = identity). A track starts with a `u32` header: key count in bits
//!   0..23, component mask (x, y, z) in bits 24..26, "has time markers" in bit
//!   27, the format in bits 28..31. Interval tracks then hold a (min, range)
//!   float pair per present component; then the keys; then, with time markers,
//!   a key -> frame table after padding to 4; then padding to 4.
//!
//! Key sizes come from the engine's tables (CONFIRMED, read from the Mac
//! executable's constant data): [`TRANSLATION_STRIDES`], [`TRANSLATION_NUM`],
//! [`ROTATION_STRIDES`], [`ROTATION_NUM`] and [`PER_TRACK_NUM_COMPONENTS`].
//! Decoding of each format follows the engine's `GetBoneAtomRotation` /
//! `GetBoneAtomTranslation` (see `docs/reverse-engineering/SKELETAL.md`).
//!
//! Every count and offset is checked before use; malformed input yields an
//! [`ObjectError`]; nothing here panics.

use std::collections::BTreeMap;

use serde::Serialize;

use crate::model::LoadedPackage;
use crate::object::{DecodedObject, ObjResult, ObjectError, decode_object};
use crate::package::Package;
use crate::property::{Property, Value};
use crate::reader::Reader;
use crate::schema::Schema;
use crate::writer::Writer;

/// `AnimationCompressionFormat`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
pub enum CompressionFormat {
    /// `ACF_None`: full floats (quaternion with W, or vector).
    None,
    /// `ACF_Float96NoW`: three floats, W reconstructed.
    Float96NoW,
    /// `ACF_Fixed48NoW`: three 16-bit fixed-point values.
    Fixed48NoW,
    /// `ACF_IntervalFixed32NoW`: 11/11/10 bits scaled into per-track ranges.
    IntervalFixed32NoW,
    /// `ACF_Fixed32NoW`: 11/11/10 bits fixed point.
    Fixed32NoW,
    /// `ACF_Float32NoW`: 11/11/10-bit small floats.
    Float32NoW,
    /// `ACF_Identity`: no data.
    Identity,
}

impl CompressionFormat {
    /// All formats in enum order.
    pub const ALL: [CompressionFormat; 7] = [
        CompressionFormat::None,
        CompressionFormat::Float96NoW,
        CompressionFormat::Fixed48NoW,
        CompressionFormat::IntervalFixed32NoW,
        CompressionFormat::Fixed32NoW,
        CompressionFormat::Float32NoW,
        CompressionFormat::Identity,
    ];

    /// Format with enum value `v`.
    pub fn from_index(v: u32) -> Option<Self> {
        Self::ALL.get(usize::try_from(v).ok()?).copied()
    }

    /// Format with the enumerator name `name` (`ACF_...`).
    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|f| f.name() == name)
    }

    /// Enum value.
    pub fn index(self) -> usize {
        match self {
            CompressionFormat::None => 0,
            CompressionFormat::Float96NoW => 1,
            CompressionFormat::Fixed48NoW => 2,
            CompressionFormat::IntervalFixed32NoW => 3,
            CompressionFormat::Fixed32NoW => 4,
            CompressionFormat::Float32NoW => 5,
            CompressionFormat::Identity => 6,
        }
    }

    /// Enumerator name.
    pub fn name(self) -> &'static str {
        match self {
            CompressionFormat::None => "ACF_None",
            CompressionFormat::Float96NoW => "ACF_Float96NoW",
            CompressionFormat::Fixed48NoW => "ACF_Fixed48NoW",
            CompressionFormat::IntervalFixed32NoW => "ACF_IntervalFixed32NoW",
            CompressionFormat::Fixed32NoW => "ACF_Fixed32NoW",
            CompressionFormat::Float32NoW => "ACF_Float32NoW",
            CompressionFormat::Identity => "ACF_Identity",
        }
    }
}

/// `AnimationKeyFormat`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
pub enum KeyEncoding {
    /// `AKF_ConstantKeyLerp`: keys evenly spaced over the sequence.
    ConstantKeyLerp,
    /// `AKF_VariableKeyLerp`: keys with a key -> frame table.
    VariableKeyLerp,
    /// `AKF_PerTrackCompression`: per-track format headers.
    PerTrackCompression,
}

impl KeyEncoding {
    /// Encoding with the enumerator name `name` (`AKF_...`).
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "AKF_ConstantKeyLerp" => Some(KeyEncoding::ConstantKeyLerp),
            "AKF_VariableKeyLerp" => Some(KeyEncoding::VariableKeyLerp),
            "AKF_PerTrackCompression" => Some(KeyEncoding::PerTrackCompression),
            _ => None,
        }
    }

    /// Enumerator name.
    pub fn name(self) -> &'static str {
        match self {
            KeyEncoding::ConstantKeyLerp => "AKF_ConstantKeyLerp",
            KeyEncoding::VariableKeyLerp => "AKF_VariableKeyLerp",
            KeyEncoding::PerTrackCompression => "AKF_PerTrackCompression",
        }
    }

    /// Offsets per track in `CompressedTrackOffsets`.
    pub fn offsets_per_track(self) -> usize {
        match self {
            KeyEncoding::PerTrackCompression => 2,
            _ => 4,
        }
    }
}

/// Bytes per stored component of a translation key, by format
/// (`CompressedTranslationStrides`).
pub const TRANSLATION_STRIDES: [usize; 7] = [4, 4, 4, 4, 4, 4, 0];
/// Components per translation key, by format (`CompressedTranslationNum`).
pub const TRANSLATION_NUM: [usize; 7] = [3, 3, 3, 1, 3, 3, 0];
/// Bytes per stored component of a rotation key, by format
/// (`CompressedRotationStrides`; the per-track codec uses it for both kinds).
pub const ROTATION_STRIDES: [usize; 7] = [4, 4, 2, 4, 4, 4, 0];
/// Components per rotation key, by format (`CompressedRotationNum`).
pub const ROTATION_NUM: [usize; 7] = [4, 3, 3, 1, 1, 1, 0];
/// `PerTrackNumComponentTable[format * 8 + mask]`: stored components (for the
/// interval format: floats of the range header).
pub const PER_TRACK_NUM_COMPONENTS: [u8; 56] = [
    4, 4, 4, 4, 4, 4, 4, 4, //
    3, 1, 1, 2, 1, 2, 2, 3, //
    3, 1, 1, 2, 1, 2, 2, 3, //
    6, 2, 2, 4, 2, 4, 4, 6, //
    1, 1, 1, 1, 1, 1, 1, 1, //
    1, 1, 1, 1, 1, 1, 1, 1, //
    0, 0, 0, 0, 0, 0, 0, 0, //
];
/// Fill byte of the stream's alignment padding (written by the engine's
/// saver).
pub const PAD_BYTE: u8 = 0x55;

/// One `AnimNotifyEvent`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct AnimNotifyEvent {
    /// Trigger time (seconds).
    pub time: f32,
    /// Notify object path (`None` when unset).
    pub notify: Option<String>,
    /// `Comment`.
    pub comment: String,
    /// `Duration`.
    pub duration: f32,
}

/// Tagged-property summary of an `AnimSequence` with class defaults applied
/// (`RateScale` 1.0, everything else zero / false / `None`).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct AnimSequenceInfo {
    /// `SequenceName`.
    pub sequence_name: String,
    /// `NumFrames`.
    pub num_frames: i32,
    /// `SequenceLength` (seconds).
    pub sequence_length: f32,
    /// `RateScale`.
    pub rate_scale: f32,
    /// `bNoLoopingInterpolation`.
    pub no_looping_interpolation: bool,
    /// `bIsAdditive`.
    pub is_additive: bool,
    /// `TranslationCompressionFormat`.
    pub translation_format: CompressionFormat,
    /// `RotationCompressionFormat`.
    pub rotation_format: CompressionFormat,
    /// `KeyEncodingFormat`.
    pub key_encoding: KeyEncoding,
    /// `CompressedTrackOffsets`.
    pub compressed_track_offsets: Vec<i32>,
    /// `Notifies`.
    pub notifies: Vec<AnimNotifyEvent>,
    /// `CompressionScheme` object path (editor data; usually stripped).
    pub compression_scheme: Option<String>,
    /// `AdditiveRefName`.
    pub additive_ref_name: Option<String>,
    /// `EncodingPkgVersion`.
    pub encoding_pkg_version: i32,
}

/// One `FRawAnimSequenceTrack`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RawAnimTrack {
    /// Translation keys.
    pub pos_keys: Vec<[f32; 3]>,
    /// Rotation keys `(x, y, z, w)`.
    pub rot_keys: Vec<[f32; 4]>,
}

/// Native tail of an `AnimSequence`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct AnimSequenceNative {
    /// Payload offset where the native data starts.
    pub start: usize,
    /// `RawAnimationData` (editor source keys; empty when stripped).
    pub raw_tracks: Vec<RawAnimTrack>,
    /// `CompressedByteStream`.
    #[serde(skip)]
    pub compressed: Vec<u8>,
}

/// Whether a compressed track is a translation or a rotation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum TrackKind {
    /// Translation (`FVector`).
    Translation,
    /// Rotation (`FQuat`).
    Rotation,
}

/// Which codec family wrote a track.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum Codec {
    /// ConstantKeyLerp / VariableKeyLerp.
    Legacy,
    /// PerTrackCompression.
    PerTrack,
}

/// Raw key payload of a track, flattened in stored order.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub enum KeyData {
    /// 32-bit floats.
    F32(Vec<f32>),
    /// 16-bit words.
    U16(Vec<u16>),
    /// 32-bit words.
    U32(Vec<u32>),
    /// No key data.
    Empty,
}

/// One decoded compressed track (raw storage plus layout facts).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CompressedTrack {
    /// Translation or rotation.
    pub kind: TrackKind,
    /// Codec family.
    pub codec: Codec,
    /// Stream offset of the track.
    pub offset: usize,
    /// Storage format of the keys (legacy one-key tracks: the forced format).
    pub format: CompressionFormat,
    /// Component mask (per-track codec; 0 for the legacy codecs).
    pub component_mask: u8,
    /// Key count.
    pub num_keys: usize,
    /// Interval header floats (legacy: `Mins[3], Ranges[3]`; per-track: one
    /// `(min, range)` pair per present component).
    pub header: Vec<f32>,
    /// Stored components per key.
    pub components_per_key: usize,
    /// Key payload.
    pub data: KeyData,
    /// Key -> frame table (empty when keys are evenly spaced).
    pub frames: Vec<u16>,
    /// True when the track stores a frame table.
    pub has_frame_table: bool,
    /// Stream offset just after the track (padding included).
    pub end: usize,
    /// Values of the padding bytes inside the track.
    pub padding: Vec<u8>,
}

/// One bone track: translation and rotation (`None` = identity, per-track
/// codec only).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct BoneTrack {
    /// Translation track.
    pub translation: Option<CompressedTrack>,
    /// Rotation track.
    pub rotation: Option<CompressedTrack>,
}

/// A decoded `AnimSequence` export.
#[derive(Debug, Clone, Serialize)]
pub struct AnimSequence {
    /// Prelude and tagged properties.
    pub object: DecodedObject,
    /// Tagged-property summary.
    pub info: AnimSequenceInfo,
    /// Native tail.
    pub native: AnimSequenceNative,
    /// Decoded compressed tracks.
    pub tracks: Vec<BoneTrack>,
}

/// A decoded `AnimSet` export (tags only).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct AnimSetInfo {
    /// Object path.
    pub path: String,
    /// `TrackBoneNames`.
    pub track_bone_names: Vec<String>,
    /// `Sequences` (object paths; `None` for null entries).
    pub sequences: Vec<Option<String>>,
    /// `Sequences` raw package indices.
    pub sequence_indices: Vec<i32>,
    /// `bAnimRotationOnly` (class default true).
    pub anim_rotation_only: bool,
    /// `UseTranslationBoneNames`.
    pub use_translation_bone_names: Vec<String>,
    /// `ForceMeshTranslationBoneNames`.
    pub force_mesh_translation_bone_names: Vec<String>,
    /// `PreviewSkelMeshName`.
    pub preview_skel_mesh_name: Option<String>,
    /// `BestRatioSkelMeshName`.
    pub best_ratio_skel_mesh_name: Option<String>,
}

impl AnimSetInfo {
    /// True when a bone track uses the animation's translation (otherwise the
    /// mesh's reference-pose translation): the root bone always does; other
    /// bones unless (`bAnimRotationOnly` and the track is not in
    /// `UseTranslationBoneNames`) or the track is in
    /// `ForceMeshTranslationBoneNames` (STRONG, from the engine's pose code).
    pub fn uses_anim_translation(&self, track_index: usize, is_root_bone: bool) -> bool {
        if is_root_bone {
            return true;
        }
        let Some(name) = self.track_bone_names.get(track_index) else {
            return false;
        };
        let listed = |list: &[String]| list.iter().any(|n| n.eq_ignore_ascii_case(name));
        let rotation_only = self.anim_rotation_only && !listed(&self.use_translation_bone_names);
        !(rotation_only || listed(&self.force_mesh_translation_bone_names))
    }
}

fn malformed(what: &'static str, offset: usize, detail: impl Into<String>) -> ObjectError {
    ObjectError::Malformed {
        what,
        offset,
        detail: detail.into(),
    }
}

// ---------------------------------------------------------------------------
// Tagged properties
// ---------------------------------------------------------------------------

fn prop<'a>(props: &'a [Property], name: &str) -> Option<&'a Value> {
    props
        .iter()
        .find(|p| p.name.eq_ignore_ascii_case(name) && p.array_index == 0)
        .map(|p| &p.value)
}

fn name_list(v: Option<&Value>) -> Vec<String> {
    match v {
        Some(Value::Array(items)) => items
            .iter()
            .filter_map(|i| match i {
                Value::Name(n) => Some(n.clone()),
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    }
}

fn name_value(v: Option<&Value>) -> Option<String> {
    match v {
        Some(Value::Name(n)) if !n.eq_ignore_ascii_case("None") => Some(n.clone()),
        _ => None,
    }
}

fn object_path(v: &Value) -> Option<String> {
    match v {
        Value::Object(o) if o.index != 0 => Some(o.path.clone()),
        _ => None,
    }
}

fn enum_value(v: Option<&Value>) -> Option<&str> {
    match v {
        Some(Value::Enum(s)) => Some(s.as_str()),
        _ => None,
    }
}

/// Build the info summary from tagged properties (class defaults applied).
pub fn anim_sequence_info(props: &[Property]) -> ObjResult<AnimSequenceInfo> {
    let format = |name: &str| -> ObjResult<CompressionFormat> {
        match prop(props, name) {
            None => Ok(CompressionFormat::None),
            Some(Value::Enum(s)) => CompressionFormat::from_name(s)
                .ok_or_else(|| malformed("compression format", 0, format!("{name} = {s}"))),
            Some(Value::Byte(b)) => CompressionFormat::from_index(u32::from(*b))
                .ok_or_else(|| malformed("compression format", 0, format!("{name} = {b}"))),
            Some(other) => Err(malformed(
                "compression format",
                0,
                format!("{name} = {other:?}"),
            )),
        }
    };
    let key_encoding = match enum_value(prop(props, "KeyEncodingFormat")) {
        None => KeyEncoding::ConstantKeyLerp,
        Some(s) => KeyEncoding::from_name(s)
            .ok_or_else(|| malformed("KeyEncodingFormat", 0, s.to_owned()))?,
    };
    let int = |name: &str| match prop(props, name) {
        Some(Value::Int(v)) => *v,
        _ => 0,
    };
    let float = |name: &str, default: f32| match prop(props, name) {
        Some(Value::Float(v)) => *v,
        _ => default,
    };
    let boolean = |name: &str| matches!(prop(props, name), Some(Value::Bool(true)));
    let compressed_track_offsets = match prop(props, "CompressedTrackOffsets") {
        Some(Value::Array(items)) => items
            .iter()
            .map(|v| match v {
                Value::Int(i) => Ok(*i),
                other => Err(malformed(
                    "CompressedTrackOffsets",
                    0,
                    format!("element {other:?}"),
                )),
            })
            .collect::<ObjResult<Vec<i32>>>()?,
        _ => Vec::new(),
    };
    let notifies = match prop(props, "Notifies") {
        Some(Value::Array(items)) => items
            .iter()
            .filter_map(|v| match v {
                Value::Struct { fields, .. } => Some(AnimNotifyEvent {
                    time: match prop(fields, "Time") {
                        Some(Value::Float(t)) => *t,
                        _ => 0.0,
                    },
                    notify: prop(fields, "Notify").and_then(object_path),
                    comment: name_value(prop(fields, "Comment")).unwrap_or_default(),
                    duration: match prop(fields, "Duration") {
                        Some(Value::Float(t)) => *t,
                        _ => 0.0,
                    },
                }),
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    };
    Ok(AnimSequenceInfo {
        sequence_name: name_value(prop(props, "SequenceName")).unwrap_or_default(),
        num_frames: int("NumFrames"),
        sequence_length: float("SequenceLength", 0.0),
        rate_scale: float("RateScale", 1.0),
        no_looping_interpolation: boolean("bNoLoopingInterpolation"),
        is_additive: boolean("bIsAdditive"),
        translation_format: format("TranslationCompressionFormat")?,
        rotation_format: format("RotationCompressionFormat")?,
        key_encoding,
        compressed_track_offsets,
        notifies,
        compression_scheme: prop(props, "CompressionScheme").and_then(object_path),
        additive_ref_name: name_value(prop(props, "AdditiveRefName")),
        encoding_pkg_version: int("EncodingPkgVersion"),
    })
}

/// Build the `AnimSet` summary from tagged properties.
pub fn anim_set_info(object: &DecodedObject) -> AnimSetInfo {
    let props = &object.properties;
    let (sequences, sequence_indices) = match prop(props, "Sequences") {
        Some(Value::Array(items)) => items
            .iter()
            .map(|v| match v {
                Value::Object(o) => (object_path(v), o.index),
                _ => (None, 0),
            })
            .unzip(),
        _ => (Vec::new(), Vec::new()),
    };
    AnimSetInfo {
        path: object.path.clone(),
        track_bone_names: name_list(prop(props, "TrackBoneNames")),
        sequences,
        sequence_indices,
        anim_rotation_only: match prop(props, "bAnimRotationOnly") {
            Some(Value::Bool(b)) => *b,
            _ => true,
        },
        use_translation_bone_names: name_list(prop(props, "UseTranslationBoneNames")),
        force_mesh_translation_bone_names: name_list(prop(props, "ForceMeshTranslationBoneNames")),
        preview_skel_mesh_name: name_value(prop(props, "PreviewSkelMeshName")),
        best_ratio_skel_mesh_name: name_value(prop(props, "BestRatioSkelMeshName")),
    }
}

// ---------------------------------------------------------------------------
// Native tail
// ---------------------------------------------------------------------------

/// Bulk array header with a required element size; returns the count.
fn read_bulk_count(r: &mut Reader<'_>, what: &'static str, expected: usize) -> ObjResult<usize> {
    let at = r.position();
    let elem = r.read_i32()?;
    if usize::try_from(elem).ok() != Some(expected) {
        return Err(malformed(
            what,
            at,
            format!("bulk element size {elem}, expected {expected}"),
        ));
    }
    Ok(r.read_count(what, expected)?)
}

/// Decode the native tail of an `AnimSequence` payload starting at `start`.
/// The decoder must end exactly at the end of `data`.
pub fn decode_anim_sequence_native(data: &[u8], start: usize) -> ObjResult<AnimSequenceNative> {
    let mut r = Reader::at(data, start)?;
    let n = r.read_count("RawAnimationData", 16)?;
    let mut raw_tracks = Vec::with_capacity(n);
    for _ in 0..n {
        let np = read_bulk_count(&mut r, "raw position keys", 12)?;
        let mut pos_keys = Vec::with_capacity(np);
        for _ in 0..np {
            pos_keys.push([r.read_f32()?, r.read_f32()?, r.read_f32()?]);
        }
        let nr = read_bulk_count(&mut r, "raw rotation keys", 16)?;
        let mut rot_keys = Vec::with_capacity(nr);
        for _ in 0..nr {
            rot_keys.push([r.read_f32()?, r.read_f32()?, r.read_f32()?, r.read_f32()?]);
        }
        raw_tracks.push(RawAnimTrack { pos_keys, rot_keys });
    }
    let nb = r.read_count("CompressedByteStream", 1)?;
    let compressed = r.read_bytes(nb)?.to_vec();
    if r.remaining() != 0 {
        return Err(malformed(
            "AnimSequence native data",
            r.position(),
            format!("{} bytes left after the byte stream", r.remaining()),
        ));
    }
    Ok(AnimSequenceNative {
        start,
        raw_tracks,
        compressed,
    })
}

/// Serialize an `AnimSequence` native tail (inverse of
/// [`decode_anim_sequence_native`]).
pub fn encode_anim_sequence_native(n: &AnimSequenceNative) -> Option<Vec<u8>> {
    let mut w = Writer::new();
    w.i32(i32::try_from(n.raw_tracks.len()).ok()?);
    for t in &n.raw_tracks {
        w.i32(12);
        w.i32(i32::try_from(t.pos_keys.len()).ok()?);
        for p in &t.pos_keys {
            for c in p {
                w.u32(c.to_bits());
            }
        }
        w.i32(16);
        w.i32(i32::try_from(t.rot_keys.len()).ok()?);
        for q in &t.rot_keys {
            for c in q {
                w.u32(c.to_bits());
            }
        }
    }
    w.i32(i32::try_from(n.compressed.len()).ok()?);
    w.bytes(&n.compressed);
    Some(w.into_bytes())
}

// ---------------------------------------------------------------------------
// Compressed byte stream
// ---------------------------------------------------------------------------

/// Bounds-checked cursor over the compressed stream (positions are stream
/// offsets).
struct StreamCursor<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> StreamCursor<'a> {
    fn take(&mut self, n: usize) -> ObjResult<&'a [u8]> {
        let end = self
            .pos
            .checked_add(n)
            .filter(|&e| e <= self.data.len())
            .ok_or_else(|| {
                malformed(
                    "compressed byte stream",
                    self.pos,
                    format!(
                        "{n} bytes needed, {} left",
                        self.data.len().saturating_sub(self.pos)
                    ),
                )
            })?;
        let s = &self.data[self.pos..end];
        self.pos = end;
        Ok(s)
    }

    fn u32(&mut self) -> ObjResult<u32> {
        let b = self.take(4)?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    fn pad4(&mut self) -> ObjResult<Vec<u8>> {
        let n = (4 - self.pos % 4) % 4;
        Ok(self.take(n)?.to_vec())
    }

    /// Key payload of `count` keys of `components` stored components of
    /// `stride` bytes each, typed by the storage format.
    fn keys(
        &mut self,
        count: usize,
        stride: usize,
        components: usize,
        format: CompressionFormat,
    ) -> ObjResult<KeyData> {
        let total = count
            .checked_mul(components)
            .ok_or_else(|| malformed("compressed keys", self.pos, "key count overflows"))?;
        let bytes = self.take(
            total
                .checked_mul(stride)
                .ok_or_else(|| malformed("compressed keys", self.pos, "key size overflows"))?,
        )?;
        if bytes.is_empty() {
            return Ok(KeyData::Empty);
        }
        Ok(match (format, stride) {
            (_, 2) => KeyData::U16(
                bytes
                    .as_chunks::<2>()
                    .0
                    .iter()
                    .map(|c| u16::from_le_bytes(*c))
                    .collect(),
            ),
            (
                CompressionFormat::IntervalFixed32NoW
                | CompressionFormat::Fixed32NoW
                | CompressionFormat::Float32NoW,
                4,
            ) => KeyData::U32(
                bytes
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .map(|c| u32::from_le_bytes(*c))
                    .collect(),
            ),
            (_, 4) => KeyData::F32(
                bytes
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .map(|c| f32::from_le_bytes(*c))
                    .collect(),
            ),
            _ => {
                return Err(malformed(
                    "compressed keys",
                    self.pos,
                    format!("unexpected component size {stride}"),
                ));
            }
        })
    }

    fn floats(&mut self, n: usize) -> ObjResult<Vec<f32>> {
        let bytes = self.take(
            n.checked_mul(4)
                .ok_or_else(|| malformed("interval header", self.pos, "size overflows"))?,
        )?;
        Ok(bytes
            .as_chunks::<4>()
            .0
            .iter()
            .map(|c| f32::from_le_bytes(*c))
            .collect())
    }

    fn frames(&mut self, count: usize, wide: bool) -> ObjResult<Vec<u16>> {
        let width = if wide { 2 } else { 1 };
        let bytes = self.take(
            count
                .checked_mul(width)
                .ok_or_else(|| malformed("frame table", self.pos, "size overflows"))?,
        )?;
        Ok(if wide {
            bytes
                .as_chunks::<2>()
                .0
                .iter()
                .map(|c| u16::from_le_bytes(*c))
                .collect()
        } else {
            bytes.iter().map(|&b| u16::from(b)).collect()
        })
    }
}

/// Legacy key storage: the format for `num_keys` keys (one-key tracks are
/// forced to the three-float form) and its key size.
fn legacy_layout(
    kind: TrackKind,
    format: CompressionFormat,
    num_keys: usize,
) -> (CompressionFormat, usize, usize) {
    match kind {
        TrackKind::Rotation => {
            let f = if num_keys == 1 {
                CompressionFormat::Float96NoW
            } else {
                format
            };
            (f, ROTATION_STRIDES[f.index()], ROTATION_NUM[f.index()])
        }
        TrackKind::Translation => {
            if num_keys == 1 {
                (
                    CompressionFormat::None,
                    TRANSLATION_STRIDES[0],
                    TRANSLATION_NUM[0],
                )
            } else {
                (
                    format,
                    TRANSLATION_STRIDES[format.index()],
                    TRANSLATION_NUM[format.index()],
                )
            }
        }
    }
}

fn parse_legacy_track(
    s: &mut StreamCursor<'_>,
    kind: TrackKind,
    format: CompressionFormat,
    num_keys: usize,
    variable: bool,
    wide_frames: bool,
) -> ObjResult<CompressedTrack> {
    let offset = s.pos;
    if num_keys == 0 {
        return Err(malformed(
            "compressed track",
            offset,
            "track with zero keys",
        ));
    }
    let (storage, stride, components) = legacy_layout(kind, format, num_keys);
    if kind == TrackKind::Translation
        && matches!(
            storage,
            CompressionFormat::Fixed48NoW
                | CompressionFormat::Fixed32NoW
                | CompressionFormat::Float32NoW
        )
    {
        // The engine stores these as three floats but decodes them as packed
        // data; the compressor never writes them. Refuse rather than guess.
        return Err(malformed(
            "compressed translation track",
            offset,
            format!(
                "{} is not a supported legacy translation format",
                storage.name()
            ),
        ));
    }
    let header = if storage == CompressionFormat::IntervalFixed32NoW {
        s.floats(6)?
    } else {
        Vec::new()
    };
    let data = s.keys(num_keys, stride, components, storage)?;
    let mut padding = Vec::new();
    let mut frames = Vec::new();
    let has_frame_table = variable && num_keys > 1;
    if has_frame_table {
        padding.extend(s.pad4()?);
        frames = s.frames(num_keys, wide_frames)?;
    }
    padding.extend(s.pad4()?);
    Ok(CompressedTrack {
        kind,
        codec: Codec::Legacy,
        offset,
        format: storage,
        component_mask: 0,
        num_keys,
        header,
        components_per_key: components,
        data,
        frames,
        has_frame_table,
        end: s.pos,
        padding,
    })
}

fn parse_per_track(
    s: &mut StreamCursor<'_>,
    kind: TrackKind,
    wide_frames: bool,
) -> ObjResult<CompressedTrack> {
    let offset = s.pos;
    let header_word = s.u32()?;
    let num_keys = usize::try_from(header_word & 0x00ff_ffff).unwrap_or(0);
    let flags = (header_word >> 24) & 0xf;
    let format = CompressionFormat::from_index(header_word >> 28).ok_or_else(|| {
        malformed(
            "per-track header",
            offset,
            format!("unknown format {}", header_word >> 28),
        )
    })?;
    let mask = u8::try_from(flags & 7).unwrap_or(0);
    let table = PER_TRACK_NUM_COMPONENTS
        .get(format.index() * 8 + usize::from(mask))
        .copied()
        .map(usize::from)
        .unwrap_or(0);
    let (header, components) = if format == CompressionFormat::IntervalFixed32NoW {
        (s.floats(table)?, 1)
    } else {
        (Vec::new(), table)
    };
    let data = s.keys(
        num_keys,
        ROTATION_STRIDES[format.index()],
        components,
        format,
    )?;
    let mut padding = Vec::new();
    let mut frames = Vec::new();
    let has_frame_table = flags & 8 != 0;
    if has_frame_table {
        padding.extend(s.pad4()?);
        frames = s.frames(num_keys, wide_frames)?;
    }
    padding.extend(s.pad4()?);
    Ok(CompressedTrack {
        kind,
        codec: Codec::PerTrack,
        offset,
        format,
        component_mask: mask,
        num_keys,
        header,
        components_per_key: components,
        data,
        frames,
        has_frame_table,
        end: s.pos,
        padding,
    })
}

/// Parse every track of a compressed byte stream. Each track is read at its
/// offset from `CompressedTrackOffsets`; the offsets must also be the
/// sequential layout the engine's loader assumes (each track starts where
/// the previous one ended), and the tracks must consume the whole stream.
pub fn decode_tracks(info: &AnimSequenceInfo, stream: &[u8]) -> ObjResult<Vec<BoneTrack>> {
    let offsets = &info.compressed_track_offsets;
    let per = info.key_encoding.offsets_per_track();
    if !offsets.len().is_multiple_of(per) {
        return Err(malformed(
            "CompressedTrackOffsets",
            0,
            format!("{} entries is not a multiple of {per}", offsets.len()),
        ));
    }
    let wide = info.num_frames > 255;
    let mut s = StreamCursor {
        data: stream,
        pos: 0,
    };
    let mut tracks = Vec::with_capacity(offsets.len() / per);
    let seek = |s: &mut StreamCursor<'_>, off: i32, what: &'static str| -> ObjResult<()> {
        let off = usize::try_from(off)
            .map_err(|_| malformed(what, 0, format!("negative offset {off}")))?;
        if off != s.pos {
            return Err(malformed(
                what,
                off,
                format!(
                    "track offset {off} is not the sequential position {}",
                    s.pos
                ),
            ));
        }
        Ok(())
    };
    for chunk in offsets.chunks_exact(per) {
        let track = match info.key_encoding {
            KeyEncoding::PerTrackCompression => {
                let translation = if chunk[0] == -1 {
                    None
                } else {
                    seek(&mut s, chunk[0], "translation track")?;
                    Some(parse_per_track(&mut s, TrackKind::Translation, wide)?)
                };
                let rotation = if chunk[1] == -1 {
                    None
                } else {
                    seek(&mut s, chunk[1], "rotation track")?;
                    Some(parse_per_track(&mut s, TrackKind::Rotation, wide)?)
                };
                BoneTrack {
                    translation,
                    rotation,
                }
            }
            enc => {
                let variable = enc == KeyEncoding::VariableKeyLerp;
                let count = |v: i32, what: &'static str| {
                    usize::try_from(v).map_err(|_| malformed(what, 0, format!("key count {v}")))
                };
                seek(&mut s, chunk[0], "translation track")?;
                let t = parse_legacy_track(
                    &mut s,
                    TrackKind::Translation,
                    info.translation_format,
                    count(chunk[1], "translation key count")?,
                    variable,
                    wide,
                )?;
                seek(&mut s, chunk[2], "rotation track")?;
                let r = parse_legacy_track(
                    &mut s,
                    TrackKind::Rotation,
                    info.rotation_format,
                    count(chunk[3], "rotation key count")?,
                    variable,
                    wide,
                )?;
                BoneTrack {
                    translation: Some(t),
                    rotation: Some(r),
                }
            }
        };
        tracks.push(track);
    }
    if s.pos != stream.len() {
        return Err(malformed(
            "compressed byte stream",
            s.pos,
            format!("tracks end at {} of {} bytes", s.pos, stream.len()),
        ));
    }
    Ok(tracks)
}

fn put_pad(w: &mut Vec<u8>, fill: u8) {
    while !w.len().is_multiple_of(4) {
        w.push(fill);
    }
}

fn put_track(w: &mut Vec<u8>, t: &CompressedTrack, num_frames: i32, fill: u8) -> Option<()> {
    if t.codec == Codec::PerTrack {
        let n = u32::try_from(t.num_keys)
            .ok()
            .filter(|&n| n <= 0x00ff_ffff)?;
        let flags = u32::from(t.component_mask & 7) | if t.has_frame_table { 8 } else { 0 };
        let format = u32::try_from(t.format.index()).ok()?;
        w.extend_from_slice(&(n | (flags << 24) | (format << 28)).to_le_bytes());
    }
    for h in &t.header {
        w.extend_from_slice(&h.to_bits().to_le_bytes());
    }
    match &t.data {
        KeyData::F32(v) => v
            .iter()
            .for_each(|x| w.extend_from_slice(&x.to_bits().to_le_bytes())),
        KeyData::U16(v) => v.iter().for_each(|x| w.extend_from_slice(&x.to_le_bytes())),
        KeyData::U32(v) => v.iter().for_each(|x| w.extend_from_slice(&x.to_le_bytes())),
        KeyData::Empty => {}
    }
    if t.has_frame_table {
        put_pad(w, fill);
        for &f in &t.frames {
            if num_frames > 255 {
                w.extend_from_slice(&f.to_le_bytes());
            } else {
                w.push(u8::try_from(f).ok()?);
            }
        }
    }
    put_pad(w, fill);
    Some(())
}

/// Re-encode decoded tracks into a byte stream (inverse of
/// [`decode_tracks`], with padding filled with `fill`).
pub fn encode_tracks(tracks: &[BoneTrack], num_frames: i32, fill: u8) -> Option<Vec<u8>> {
    let mut w = Vec::new();
    for t in tracks {
        for part in [&t.translation, &t.rotation].into_iter().flatten() {
            put_track(&mut w, part, num_frames, fill)?;
        }
    }
    Some(w)
}

// ---------------------------------------------------------------------------
// Key decoding (engine semantics)
// ---------------------------------------------------------------------------

/// Identity rotation.
pub const IDENTITY_QUAT: [f32; 4] = [0.0, 0.0, 0.0, 1.0];

fn quat_from_xyz(x: f32, y: f32, z: f32) -> [f32; 4] {
    let ww = ((1.0 - x * x) - y * y) - z * z;
    let w = if ww > 0.0 { ww.sqrt() } else { 0.0 };
    [x, y, z, w]
}

/// Unpack an 11/11/10 word with X in the top bits (rotation layout):
/// `((X - 1023) / 1023, (Y - 1023) / 1023, (Z - 511) / 511)`.
fn unpack_11_11_10_rot(v: u32) -> [f32; 3] {
    let x = (v >> 21).cast_signed() - 1023;
    let y = ((v >> 10) & 0x7ff).cast_signed() - 1023;
    let z = (v & 0x3ff).cast_signed() - 511;
    [x as f32 / 1023.0, y as f32 / 1023.0, z as f32 / 511.0]
}

/// Unpack an interval translation word: X in the low 10 bits, Y in the next
/// 11, Z in the top 11.
fn unpack_10_11_11_trans(v: u32) -> [f32; 3] {
    let x = (v & 0x3ff).cast_signed() - 511;
    let y = ((v >> 10) & 0x7ff).cast_signed() - 1023;
    let z = (v >> 21).cast_signed() - 1023;
    [x as f32 / 511.0, y as f32 / 1023.0, z as f32 / 1023.0]
}

/// One small float of `ACF_Float32NoW` (3 exponent bits, `mantissa_bits`
/// mantissa bits, a sign bit above them; zero bits decode to 0).
fn small_float(bits: u32, mantissa_bits: u32, sign: bool) -> f32 {
    if bits == 0 && !sign {
        return 0.0;
    }
    let shift = 23 - mantissa_bits;
    let u = bits << shift;
    let mant_mask = ((1u32 << mantissa_bits) - 1) << shift;
    let v =
        (u & 0x0380_0000) + 0x3d80_0000 + ((u & mant_mask) | if sign { 0x8000_0000 } else { 0 });
    f32::from_bits(v)
}

fn unpack_float32_now(v: u32) -> [f32; 3] {
    // X: bits 21..31 (sign bit 31), Y: bits 10..20 (sign bit 20),
    // Z: bits 0..9 (sign bit 9). A component whose bits are all zero is 0.
    let x = v >> 21;
    let y = (v >> 10) & 0x7ff;
    let z = v & 0x3ff;
    let fx = if x == 0 {
        0.0
    } else {
        small_float(x & 0x3ff, 7, x & 0x400 != 0)
    };
    let fy = if y == 0 {
        0.0
    } else {
        small_float(y & 0x3ff, 7, y & 0x400 != 0)
    };
    let fz = if z == 0 {
        0.0
    } else {
        small_float(z & 0x1ff, 6, z & 0x200 != 0)
    };
    [fx, fy, fz]
}

impl CompressedTrack {
    fn f32_at(&self, i: usize) -> f32 {
        match &self.data {
            KeyData::F32(v) => v.get(i).copied().unwrap_or(0.0),
            _ => 0.0,
        }
    }

    fn u16_at(&self, i: usize) -> u16 {
        match &self.data {
            KeyData::U16(v) => v.get(i).copied().unwrap_or(0),
            _ => 0,
        }
    }

    fn u32_at(&self, i: usize) -> u32 {
        match &self.data {
            KeyData::U32(v) => v.get(i).copied().unwrap_or(0),
            _ => 0,
        }
    }

    /// Per-track interval header: `(min, range)` for x, y, z (absent
    /// components are zero).
    fn per_track_ranges(&self) -> ([f32; 3], [f32; 3]) {
        let mut mins = [0f32; 3];
        let mut ranges = [0f32; 3];
        let mut k = 0;
        for c in 0..3 {
            if self.component_mask & (1 << c) != 0 {
                mins[c] = self.header.get(k).copied().unwrap_or(0.0);
                ranges[c] = self.header.get(k.saturating_add(1)).copied().unwrap_or(0.0);
                k += 2;
            }
        }
        (mins, ranges)
    }

    /// Masked per-track components of key `key` read with `read(index)`.
    fn masked<T: Copy>(&self, key: usize, zero: T, read: impl Fn(usize) -> T) -> [T; 3] {
        let mut out = [zero; 3];
        let mut k = self.first_component(key);
        for (c, o) in out.iter_mut().enumerate() {
            if self.component_mask & (1 << c) != 0 {
                *o = read(k);
                k = k.saturating_add(1);
            }
        }
        out
    }

    /// Index of the first stored component of key `key` (saturating: an
    /// out-of-range key reads as missing data, never overflows).
    fn first_component(&self, key: usize) -> usize {
        key.saturating_mul(self.components_per_key)
    }

    /// True when the engine can decode this track's format (others decode
    /// to identity / zero with an engine error).
    pub fn format_supported(&self) -> bool {
        match (self.codec, self.kind) {
            (Codec::Legacy, TrackKind::Rotation) => true,
            (Codec::Legacy, TrackKind::Translation) => matches!(
                self.format,
                CompressionFormat::None
                    | CompressionFormat::Float96NoW
                    | CompressionFormat::IntervalFixed32NoW
                    | CompressionFormat::Identity
            ),
            (Codec::PerTrack, TrackKind::Rotation) => self.format != CompressionFormat::None,
            (Codec::PerTrack, TrackKind::Translation) => matches!(
                self.format,
                CompressionFormat::Float96NoW
                    | CompressionFormat::Fixed48NoW
                    | CompressionFormat::IntervalFixed32NoW
                    | CompressionFormat::Identity
            ),
        }
    }

    /// Rotation key `key` as `(x, y, z, w)`, decoded like the engine.
    pub fn rotation_key(&self, key: usize) -> [f32; 4] {
        let k = self.first_component(key);
        match (self.codec, self.format) {
            (_, CompressionFormat::Identity) => IDENTITY_QUAT,
            (Codec::Legacy, CompressionFormat::None) => [
                self.f32_at(k),
                self.f32_at(k.saturating_add(1)),
                self.f32_at(k.saturating_add(2)),
                self.f32_at(k.saturating_add(3)),
            ],
            (Codec::PerTrack, CompressionFormat::None) => IDENTITY_QUAT,
            (_, CompressionFormat::Float96NoW) => quat_from_xyz(
                self.f32_at(k),
                self.f32_at(k.saturating_add(1)),
                self.f32_at(k.saturating_add(2)),
            ),
            (Codec::Legacy, CompressionFormat::Fixed48NoW) => {
                let c = |i: usize| {
                    (i32::from(self.u16_at(k.saturating_add(i))) - 32767) as f32 / 32767.0
                };
                quat_from_xyz(c(0), c(1), c(2))
            }
            (Codec::PerTrack, CompressionFormat::Fixed48NoW) => {
                let v = self.masked(key, 0.0f32, |i| {
                    (f32::from(self.u16_at(i)) + -32767.0) * (1.0 / 32767.0)
                });
                quat_from_xyz(v[0], v[1], v[2])
            }
            (_, CompressionFormat::IntervalFixed32NoW) => {
                let (mins, ranges) = match self.codec {
                    Codec::Legacy => (
                        [self.header_at(0), self.header_at(1), self.header_at(2)],
                        [self.header_at(3), self.header_at(4), self.header_at(5)],
                    ),
                    Codec::PerTrack => self.per_track_ranges(),
                };
                let n = unpack_11_11_10_rot(self.u32_at(key));
                quat_from_xyz(
                    n[0] * ranges[0] + mins[0],
                    n[1] * ranges[1] + mins[1],
                    n[2] * ranges[2] + mins[2],
                )
            }
            (_, CompressionFormat::Fixed32NoW) => {
                let n = unpack_11_11_10_rot(self.u32_at(key));
                quat_from_xyz(n[0], n[1], n[2])
            }
            (_, CompressionFormat::Float32NoW) => {
                let n = unpack_float32_now(self.u32_at(key));
                quat_from_xyz(n[0], n[1], n[2])
            }
        }
    }

    fn header_at(&self, i: usize) -> f32 {
        self.header.get(i).copied().unwrap_or(0.0)
    }

    /// Translation key `key`, decoded like the engine (unsupported formats
    /// decode to zero, as the engine does after logging an error).
    pub fn translation_key(&self, key: usize) -> [f32; 3] {
        let k = self.first_component(key);
        match (self.codec, self.format) {
            (_, CompressionFormat::Identity) => [0.0; 3],
            (Codec::Legacy, CompressionFormat::None | CompressionFormat::Float96NoW) => [
                self.f32_at(k),
                self.f32_at(k.saturating_add(1)),
                self.f32_at(k.saturating_add(2)),
            ],
            (Codec::Legacy, CompressionFormat::IntervalFixed32NoW) => {
                let n = unpack_10_11_11_trans(self.u32_at(key));
                [
                    n[0] * self.header_at(3) + self.header_at(0),
                    n[1] * self.header_at(4) + self.header_at(1),
                    n[2] * self.header_at(5) + self.header_at(2),
                ]
            }
            (Codec::PerTrack, CompressionFormat::Float96NoW) => {
                if self.component_mask == 0 {
                    [
                        self.f32_at(k),
                        self.f32_at(k.saturating_add(1)),
                        self.f32_at(k.saturating_add(2)),
                    ]
                } else {
                    self.masked(key, 0.0f32, |i| self.f32_at(i))
                }
            }
            (Codec::PerTrack, CompressionFormat::Fixed48NoW) => {
                self.masked(key, 0.0f32, |i| (i32::from(self.u16_at(i)) - 255) as f32)
            }
            (Codec::PerTrack, CompressionFormat::IntervalFixed32NoW) => {
                let (mins, ranges) = self.per_track_ranges();
                let n = unpack_10_11_11_trans(self.u32_at(key));
                [
                    mins[0] + ranges[0] * n[0],
                    mins[1] + ranges[1] * n[1],
                    n[2] * ranges[2] + mins[2],
                ]
            }
            _ => [0.0; 3],
        }
    }

    /// Time of key `key` in seconds, using the engine's non-looping mapping:
    /// evenly spaced keys span the whole sequence (`key / (keys - 1)`), a
    /// frame table maps a key to `frame / (NumFrames - 1)`.
    pub fn key_time(&self, key: usize, sequence_length: f32, num_frames: i32) -> f32 {
        if self.has_frame_table {
            let f = f32::from(self.frames.get(key).copied().unwrap_or(0));
            let last = num_frames.saturating_sub(1).max(1) as f32;
            return f / last * sequence_length;
        }
        if self.num_keys <= 1 {
            return 0.0;
        }
        key as f32 / (self.num_keys - 1) as f32 * sequence_length
    }
}

/// The rotation the engine's pose code uses for a decoded key that drives
/// mesh bone `mesh_bone`: every bone except the root (index 0) has its W
/// negated (the pose code multiplies by `(1, 1, 1, -1)`; CONFIRMED), which
/// puts animation keys into the reference skeleton's convention.
pub fn pose_rotation(q: [f32; 4], mesh_bone: usize) -> [f32; 4] {
    if mesh_bone == 0 {
        q
    } else {
        [q[0], q[1], q[2], -q[3]]
    }
}

/// `BoneToTrackTable`: for each mesh bone the track with the same name
/// (names compare case-insensitively, like `FName`s), or `None` (the bone
/// keeps its reference pose).
pub fn bone_to_track(track_names: &[String], bone_names: &[String]) -> Vec<Option<usize>> {
    bone_names
        .iter()
        .map(|b| track_names.iter().position(|t| t.eq_ignore_ascii_case(b)))
        .collect()
}

/// Rotation of `track` (identity when absent) at key `key`.
pub fn rotation_at(track: Option<&CompressedTrack>, key: usize) -> [f32; 4] {
    track.map_or(IDENTITY_QUAT, |t| t.rotation_key(key))
}

/// Translation of `track` (zero when absent) at key `key`.
pub fn translation_at(track: Option<&CompressedTrack>, key: usize) -> [f32; 3] {
    track.map_or([0.0; 3], |t| t.translation_key(key))
}

/// The engine's key interpolation for evenly spaced keys
/// (`relative_pos` in `[0, 1]` of the sequence): returns
/// `(key0, key1, alpha)`. Non-looping playback treats the last key as the
/// end of the sequence; looping playback gives the last key a full interval
/// that blends back to key 0, and rescales by `NumFrames` when it differs
/// from the key count.
pub fn even_key_position(
    num_keys: usize,
    num_frames: i32,
    relative_pos: f32,
    looping: bool,
) -> (usize, usize, f32) {
    if num_keys < 2 || relative_pos.is_nan() || relative_pos <= 0.0 {
        return (0, 0, 0.0);
    }
    let last = num_keys - 1;
    if relative_pos >= 1.0 {
        return if looping {
            (0, 0, 0.0)
        } else {
            (last, last, 0.0)
        };
    }
    if !looping {
        let pos = relative_pos * last as f32;
        let floor = pos.floor();
        let k0 = (floor as usize).min(last);
        return (k0, (k0 + 1).min(last), pos - floor);
    }
    let frames = usize::try_from(num_frames).unwrap_or(0).max(1);
    let pos = frames as f32 * relative_pos;
    let floor = pos.floor();
    let k0 = (floor as usize).min(frames.saturating_sub(1));
    if k0 + 1 == frames {
        return (last, 0, pos - floor);
    }
    if frames == num_keys {
        return (k0, k0 + 1, pos - floor);
    }
    let pos = last as f32 * (pos / (frames - 1).max(1) as f32);
    let floor = pos.floor();
    let k0 = (floor as usize).min(last);
    (k0, (k0 + 1).min(last), pos - floor)
}

/// Normalised linear blend of two rotations along the shorter arc (the
/// engine's key interpolation).
pub fn nlerp(a: [f32; 4], b: [f32; 4], alpha: f32) -> [f32; 4] {
    let dot = a[0] * b[0] + a[1] * b[1] + a[2] * b[2] + a[3] * b[3];
    let bias = if dot >= 0.0 { 1.0 } else { -1.0 };
    let q: [f32; 4] = std::array::from_fn(|i| a[i] * (1.0 - alpha) + b[i] * (alpha * bias));
    let len = (q[0] * q[0] + q[1] * q[1] + q[2] * q[2] + q[3] * q[3]).sqrt();
    if len > 1e-8 {
        q.map(|c| c / len)
    } else {
        IDENTITY_QUAT
    }
}

/// Sample one track at `time` seconds (non-looping engine semantics; frame
/// tables are searched like the engine does).
pub fn sample_rotation(
    t: &CompressedTrack,
    time: f32,
    sequence_length: f32,
    num_frames: i32,
) -> [f32; 4] {
    let (k0, k1, alpha) = track_position(t, time, sequence_length, num_frames);
    if k0 == k1 {
        t.rotation_key(k0)
    } else {
        nlerp(t.rotation_key(k0), t.rotation_key(k1), alpha)
    }
}

/// Sample a translation track at `time` seconds (non-looping semantics).
pub fn sample_translation(
    t: &CompressedTrack,
    time: f32,
    sequence_length: f32,
    num_frames: i32,
) -> [f32; 3] {
    let (k0, k1, alpha) = track_position(t, time, sequence_length, num_frames);
    let a = t.translation_key(k0);
    if k0 == k1 {
        return a;
    }
    let b = t.translation_key(k1);
    std::array::from_fn(|i| (b[i] - a[i]) * alpha + a[i])
}

fn track_position(
    t: &CompressedTrack,
    time: f32,
    sequence_length: f32,
    num_frames: i32,
) -> (usize, usize, f32) {
    if t.num_keys < 2 {
        return (0, 0, 0.0);
    }
    let rel = if sequence_length > 0.0 {
        time / sequence_length
    } else {
        0.0
    };
    if rel.is_nan() {
        // Non-finite time or length (malformed tags): the first key.
        return (0, 0, 0.0);
    }
    if !t.has_frame_table {
        return even_key_position(t.num_keys, num_frames, rel, false);
    }
    let last = t.num_keys - 1;
    if rel <= 0.0 {
        return (0, 0, 0.0);
    }
    if rel >= 1.0 {
        return (last, last, 0.0);
    }
    let frame_pos = num_frames.saturating_sub(1).max(0) as f32 * rel;
    let mut k0 = 0;
    for (i, &f) in t.frames.iter().enumerate() {
        if f32::from(f) <= frame_pos {
            k0 = i;
        }
    }
    let k0 = k0.min(last);
    let k1 = (k0 + 1).min(last);
    let f0 = t.frames.get(k0).copied().unwrap_or(0);
    let f1 = t.frames.get(k1).copied().unwrap_or(0);
    let span = if f1 > f0 { f32::from(f1 - f0) } else { 1.0 };
    (k0, k1, (frame_pos - f32::from(f0)) / span)
}

// ---------------------------------------------------------------------------
// Export-level decoding
// ---------------------------------------------------------------------------

fn is_engine_class(pkg: &Package, index: usize, class: &str) -> bool {
    let Ok(c) = pkg.export_class_name(index) else {
        return false;
    };
    c == class
        && matches!(pkg.export_class_package(index), Ok(Some(p)) if p.eq_ignore_ascii_case("Engine"))
}

/// True when export `index` is an `Engine.AnimSequence`.
pub fn is_anim_sequence(pkg: &Package, index: usize) -> bool {
    is_engine_class(pkg, index, "AnimSequence")
}

/// True when export `index` is an `Engine.AnimSet`.
pub fn is_anim_set(pkg: &Package, index: usize) -> bool {
    is_engine_class(pkg, index, "AnimSet")
}

/// Decode export `index` as an `AnimSequence`: tags, native tail and every
/// compressed track.
pub fn decode_anim_sequence(
    pkg: &Package,
    own_name: Option<&str>,
    index: usize,
    schema: &dyn Schema,
) -> ObjResult<AnimSequence> {
    if !is_anim_sequence(pkg, index) {
        return Err(ObjectError::WrongKind {
            export: index,
            expected: "Engine.AnimSequence",
            found: pkg.export_class_name(index).unwrap_or_default(),
        });
    }
    let object = decode_object(pkg, own_name, index, schema)?;
    let info = anim_sequence_info(&object.properties)?;
    let data = pkg.export_data(index)?;
    let native = decode_anim_sequence_native(data, object.properties_end)?;
    let tracks = decode_tracks(&info, &native.compressed)?;
    Ok(AnimSequence {
        object,
        info,
        native,
        tracks,
    })
}

/// Decode export `index` as an `AnimSet` (tags only; a native tail is
/// refused).
pub fn decode_anim_set(
    pkg: &Package,
    own_name: Option<&str>,
    index: usize,
    schema: &dyn Schema,
) -> ObjResult<AnimSetInfo> {
    if !is_anim_set(pkg, index) {
        return Err(ObjectError::WrongKind {
            export: index,
            expected: "Engine.AnimSet",
            found: pkg.export_class_name(index).unwrap_or_default(),
        });
    }
    let object = decode_object(pkg, own_name, index, schema)?;
    if object.native_tail() != 0 {
        return Err(ObjectError::SizeMismatch {
            export: index,
            kind: "AnimSet".to_owned(),
            consumed: object.properties_end,
            size: object.payload_size,
        });
    }
    Ok(anim_set_info(&object))
}

// ---------------------------------------------------------------------------
// Coverage
// ---------------------------------------------------------------------------

/// Most failure samples kept per package.
const MAX_FAILURE_SAMPLES: usize = 16;

/// Animation coverage of one package.
#[derive(Debug, Clone, Default, Serialize)]
pub struct AnimCoverage {
    /// Package name.
    pub package: String,
    /// `AnimSet` exports.
    pub sets: usize,
    /// `AnimSet` exports decoded (tags only, no native tail).
    pub sets_exact: usize,
    /// `AnimSequence` exports.
    pub sequences: usize,
    /// Sequences whose native tail and byte stream decoded exactly.
    pub sequences_exact: usize,
    /// Sequences whose byte stream re-encodes byte for byte.
    pub stream_round_trip: usize,
    /// Sequences whose native tail re-encodes byte for byte.
    pub native_round_trip: usize,
    /// Sequences whose track count equals their owning AnimSet's bone count.
    pub track_count_matches_set: usize,
    /// Sequences listed in their owning AnimSet's `Sequences`.
    pub listed_in_set: usize,
    /// Key encoding -> sequence count.
    pub key_encodings: BTreeMap<String, usize>,
    /// "codec kind format" -> track count.
    pub track_formats: BTreeMap<String, usize>,
    /// Translation tracks / rotation tracks absent (per-track identity).
    pub identity_tracks: usize,
    /// Tracks with a frame table.
    pub frame_table_tracks: usize,
    /// Tracks in a format the engine cannot decode.
    pub unsupported_tracks: usize,
    /// Per-track component masks -> track count.
    pub component_masks: BTreeMap<u8, usize>,
    /// Padding byte value -> count.
    pub padding_bytes: BTreeMap<u8, usize>,
    /// Sequences with raw (editor) tracks stored.
    pub with_raw_tracks: usize,
    /// Additive sequences.
    pub additive: usize,
    /// Sequences with notifies.
    pub with_notifies: usize,
    /// Notify events.
    pub notifies: usize,
    /// Keys over all tracks.
    pub keys: u64,
    /// Compressed stream bytes.
    pub stream_bytes: u64,
    /// Rotation keys that are not unit length (|len - 1| > 0.01).
    pub non_unit_rotation_keys: u64,
    /// Non-finite decoded values.
    pub non_finite_values: u64,
    /// First failures.
    pub failures: Vec<String>,
}

/// Decode every `AnimSet` and `AnimSequence` of `lp` and gather statistics.
pub fn anim_coverage(lp: &LoadedPackage, schema: &dyn Schema) -> AnimCoverage {
    let pkg = &lp.package;
    let mut cov = AnimCoverage {
        package: lp.name.clone(),
        ..AnimCoverage::default()
    };
    let mut sets: BTreeMap<usize, AnimSetInfo> = BTreeMap::new();
    for i in 0..pkg.exports.len() {
        if !is_anim_set(pkg, i) {
            continue;
        }
        cov.sets += 1;
        match decode_anim_set(pkg, Some(&lp.name), i, schema) {
            Ok(s) => {
                cov.sets_exact += 1;
                sets.insert(i, s);
            }
            Err(e) => {
                if cov.failures.len() < MAX_FAILURE_SAMPLES {
                    cov.failures.push(format!("{i}: {e}"));
                }
            }
        }
    }
    for i in 0..pkg.exports.len() {
        if !is_anim_sequence(pkg, i) {
            continue;
        }
        cov.sequences += 1;
        let seq = match decode_anim_sequence(pkg, Some(&lp.name), i, schema) {
            Ok(s) => s,
            Err(e) => {
                if cov.failures.len() < MAX_FAILURE_SAMPLES {
                    cov.failures.push(format!("{i}: {e}"));
                }
                continue;
            }
        };
        cov.sequences_exact += 1;
        let info = &seq.info;
        if encode_tracks(&seq.tracks, info.num_frames, PAD_BYTE).as_deref()
            == Some(seq.native.compressed.as_slice())
        {
            cov.stream_round_trip += 1;
        }
        let original = pkg
            .export_data(i)
            .ok()
            .and_then(|d| d.get(seq.object.properties_end..));
        if original.is_some() && encode_anim_sequence_native(&seq.native).as_deref() == original {
            cov.native_round_trip += 1;
        }
        // Owning AnimSet: the sequence's outer.
        let outer = pkg
            .export(i)
            .ok()
            .and_then(|e| e.outer_index.export_index());
        if let Some(set) = outer.and_then(|o| sets.get(&o)) {
            if set.track_bone_names.len() == seq.tracks.len() {
                cov.track_count_matches_set += 1;
            }
            let me = crate::types::PackageIndex::from_export(i).map(|p| p.0);
            if me.is_some_and(|m| set.sequence_indices.contains(&m)) {
                cov.listed_in_set += 1;
            }
        }
        *cov.key_encodings
            .entry(info.key_encoding.name().to_owned())
            .or_insert(0) += 1;
        if !seq.native.raw_tracks.is_empty() {
            cov.with_raw_tracks += 1;
        }
        if info.is_additive {
            cov.additive += 1;
        }
        if !info.notifies.is_empty() {
            cov.with_notifies += 1;
            cov.notifies += info.notifies.len();
        }
        cov.stream_bytes += u64::try_from(seq.native.compressed.len()).unwrap_or(0);
        for t in &seq.tracks {
            if t.translation.is_none() {
                cov.identity_tracks += 1;
            }
            if t.rotation.is_none() {
                cov.identity_tracks += 1;
            }
            for part in [&t.translation, &t.rotation].into_iter().flatten() {
                let codec = match part.codec {
                    Codec::Legacy => info.key_encoding.name(),
                    Codec::PerTrack => "AKF_PerTrackCompression",
                };
                let kind = match part.kind {
                    TrackKind::Translation => "translation",
                    TrackKind::Rotation => "rotation",
                };
                *cov.track_formats
                    .entry(format!("{codec} {kind} {}", part.format.name()))
                    .or_insert(0) += 1;
                if part.has_frame_table {
                    cov.frame_table_tracks += 1;
                }
                if !part.format_supported() {
                    cov.unsupported_tracks += 1;
                }
                if part.codec == Codec::PerTrack {
                    *cov.component_masks.entry(part.component_mask).or_insert(0) += 1;
                }
                for &b in &part.padding {
                    *cov.padding_bytes.entry(b).or_insert(0) += 1;
                }
                cov.keys += u64::try_from(part.num_keys).unwrap_or(0);
                for k in 0..part.num_keys {
                    match part.kind {
                        TrackKind::Rotation => {
                            let q = part.rotation_key(k);
                            if q.iter().any(|c| !c.is_finite()) {
                                cov.non_finite_values += 1;
                            }
                            let len =
                                (q[0] * q[0] + q[1] * q[1] + q[2] * q[2] + q[3] * q[3]).sqrt();
                            if (len - 1.0).abs() > 0.01 {
                                cov.non_unit_rotation_keys += 1;
                            }
                        }
                        TrackKind::Translation => {
                            if part.translation_key(k).iter().any(|c| !c.is_finite()) {
                                cov.non_finite_values += 1;
                            }
                        }
                    }
                }
            }
        }
    }
    cov
}
