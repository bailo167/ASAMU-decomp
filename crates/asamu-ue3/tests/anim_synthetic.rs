//! Synthetic and hostile-input tests for AnimSequence / AnimSet decoding.
//!
//! Every stream is written byte by byte here from the documented layouts (no
//! game data); the decoder must recover the hand-chosen values and the
//! encoder must reproduce the bytes.

#![allow(clippy::unwrap_used)]

use asamu_ue3::anim::{
    AnimSequenceInfo, AnimSequenceNative, Codec, CompressedTrack, CompressionFormat, KeyData,
    KeyEncoding, PAD_BYTE, PER_TRACK_NUM_COMPONENTS, RawAnimTrack, TrackKind, anim_sequence_info,
    bone_to_track, decode_anim_sequence_native, decode_tracks, encode_anim_sequence_native,
    encode_tracks, even_key_position, nlerp, pose_rotation, sample_rotation, sample_translation,
};
use asamu_ue3::{Property, Value};

#[derive(Default)]
struct W(Vec<u8>);

impl W {
    fn u8(&mut self, v: u8) {
        self.0.push(v);
    }
    fn u16(&mut self, v: u16) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    fn u32(&mut self, v: u32) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    fn f32(&mut self, v: f32) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    fn pad(&mut self) {
        while !self.0.len().is_multiple_of(4) {
            self.0.push(PAD_BYTE);
        }
    }
}

fn info(
    enc: KeyEncoding,
    t: CompressionFormat,
    r: CompressionFormat,
    frames: i32,
    offsets: Vec<i32>,
) -> AnimSequenceInfo {
    AnimSequenceInfo {
        sequence_name: "Test".to_owned(),
        num_frames: frames,
        sequence_length: 1.0,
        rate_scale: 1.0,
        no_looping_interpolation: false,
        is_additive: false,
        translation_format: t,
        rotation_format: r,
        key_encoding: enc,
        compressed_track_offsets: offsets,
        notifies: Vec::new(),
        compression_scheme: None,
        additive_ref_name: None,
        encoding_pkg_version: 0,
    }
}

/// Rotation interval word: X in bits 21..31, Y in 10..20, Z in 0..9.
fn rot_word(x: u32, y: u32, z: u32) -> u32 {
    (x << 21) | (y << 10) | z
}

/// Translation interval word: X in bits 0..9, Y in 10..20, Z in 21..31.
fn trans_word(x: u32, y: u32, z: u32) -> u32 {
    (z << 21) | (y << 10) | x
}

fn close(a: f32, b: f32) -> bool {
    (a - b).abs() < 1e-5
}

#[test]
fn constant_key_lerp_with_interval_rotations() {
    let mut w = W::default();
    // Track 0 translation: one key (forced to three floats).
    w.f32(1.0);
    w.f32(2.0);
    w.f32(3.0);
    // Track 0 rotation: three interval keys: Mins[3], Ranges[3], words.
    let rot_at = w.0.len();
    for v in [0.1f32, -0.2, 0.0, 0.2, 0.4, 0.3] {
        w.f32(v);
    }
    w.u32(rot_word(1023, 1023, 511)); // -> mins + 0
    w.u32(rot_word(2046, 0, 1022)); // -> +1, -1, +1
    w.u32(rot_word(1023 + 512, 1023, 511)); // x: 512/1023
    // Track 1 translation: three float keys.
    let t1 = w.0.len();
    for k in 0..3 {
        w.f32(k as f32);
        w.f32(0.0);
        w.f32(-(k as f32));
    }
    // Track 1 rotation: one key, forced to Float96NoW.
    let r1 = w.0.len();
    w.f32(0.0);
    w.f32(0.0);
    w.f32(0.6);
    let stream = w.0;
    let offsets = vec![0, 1, rot_at as i32, 3, t1 as i32, 3, r1 as i32, 1];
    let i = info(
        KeyEncoding::ConstantKeyLerp,
        CompressionFormat::None,
        CompressionFormat::IntervalFixed32NoW,
        3,
        offsets,
    );
    let tracks = decode_tracks(&i, &stream).unwrap();
    assert_eq!(tracks.len(), 2);
    let t0 = tracks[0].translation.as_ref().unwrap();
    assert_eq!(t0.translation_key(0), [1.0, 2.0, 3.0]);
    let r0 = tracks[0].rotation.as_ref().unwrap();
    assert_eq!(r0.format, CompressionFormat::IntervalFixed32NoW);
    assert_eq!(r0.header.len(), 6);
    let q = r0.rotation_key(0);
    assert!(close(q[0], 0.1) && close(q[1], -0.2) && close(q[2], 0.0));
    let q = r0.rotation_key(1);
    assert!(close(q[0], 0.1 + 0.2) && close(q[1], -0.2 - 0.4) && close(q[2], 0.3));
    let q = r0.rotation_key(2);
    assert!(close(q[0], 0.1 + 0.2 * 512.0 / 1023.0));
    // W is rebuilt non-negative.
    assert!(q[3] > 0.0 && close(q.iter().map(|c| c * c).sum::<f32>(), 1.0));
    let r1t = tracks[1].rotation.as_ref().unwrap();
    assert_eq!(r1t.format, CompressionFormat::Float96NoW);
    assert!(close(r1t.rotation_key(0)[3], 0.8));
    // Keys are evenly spaced: key k at k / 2 of the sequence.
    let t1t = tracks[1].translation.as_ref().unwrap();
    assert_eq!(t1t.key_time(1, 1.0, 3), 0.5);
    assert_eq!(sample_translation(t1t, 0.25, 1.0, 3), [0.5, 0.0, -0.5]);
    assert_eq!(encode_tracks(&tracks, 3, PAD_BYTE).unwrap(), stream);
}

#[test]
fn variable_key_lerp_frame_tables_and_padding() {
    for frames in [10, 300] {
        let wide = frames > 255;
        let mut w = W::default();
        // Translation: 2 keys of floats + frame table.
        for v in [0.0f32, 0.0, 0.0, 9.0, 0.0, 0.0] {
            w.f32(v);
        }
        w.pad();
        for f in [0u16, (frames - 1) as u16] {
            if wide { w.u16(f) } else { w.u8(f as u8) }
        }
        w.pad();
        // Rotation: 3 Fixed48 keys (18 bytes), pad, frame table, pad.
        let rot_at = w.0.len();
        for v in [32767u16, 32767, 32767, 65534, 32767, 32767, 32767, 0, 32767] {
            w.u16(v);
        }
        w.pad();
        for f in [0u16, 3, (frames - 1) as u16] {
            if wide { w.u16(f) } else { w.u8(f as u8) }
        }
        w.pad();
        let stream = w.0;
        let i = info(
            KeyEncoding::VariableKeyLerp,
            CompressionFormat::None,
            CompressionFormat::Fixed48NoW,
            frames,
            vec![0, 2, rot_at as i32, 3],
        );
        let tracks = decode_tracks(&i, &stream).unwrap();
        let r = tracks[0].rotation.as_ref().unwrap();
        assert!(r.has_frame_table);
        assert_eq!(r.frames, vec![0, 3, (frames - 1) as u16]);
        assert_eq!(r.rotation_key(0), [0.0, 0.0, 0.0, 1.0]);
        assert_eq!(r.rotation_key(1), [1.0, 0.0, 0.0, 0.0]);
        assert_eq!(r.rotation_key(2), [0.0, -1.0, 0.0, 0.0]);
        assert!(r.padding.iter().all(|&b| b == PAD_BYTE));
        // Key 1 sits at frame 3 of NumFrames - 1.
        let t = 3.0 / (frames - 1) as f32;
        assert!(close(r.key_time(1, 1.0, frames), t));
        let q = sample_rotation(r, t, 1.0, frames);
        assert!(close(q[0], 1.0));
        let tr = tracks[0].translation.as_ref().unwrap();
        assert_eq!(sample_translation(tr, 0.5, 1.0, frames)[0], 4.5);
        assert_eq!(encode_tracks(&tracks, frames, PAD_BYTE).unwrap(), stream);
    }
}

#[test]
fn per_track_masks_headers_and_time_markers() {
    let frames = 300; // u16 frame table
    let mut w = W::default();
    // Track 0 translation: Float96NoW, mask 4 (Z only), 2 keys, time markers.
    let t0 = w.0.len();
    w.u32(2 | ((4 | 8) << 24) | (1 << 28));
    w.f32(5.0);
    w.f32(-5.0);
    w.pad();
    w.u16(0);
    w.u16(299);
    w.pad();
    // Track 0 rotation: Fixed48NoW, mask 5 (X and Z), 1 key.
    let r0 = w.0.len();
    w.u32(1 | (5 << 24) | (2 << 28));
    w.u16(65534); // x = 1
    w.u16(32767); // z = 0
    w.pad();
    // Track 1: translation identity (-1); rotation interval, mask 7, 1 key.
    let r1 = w.0.len();
    w.u32(1 | (7 << 24) | (3 << 28));
    for v in [0.0f32, 0.5, 0.1, 0.2, -0.3, 0.6] {
        w.f32(v); // (min, range) for x, y, z
    }
    w.u32(rot_word(2046, 1023, 0));
    // Track 2: translation interval, mask 2 (Y only), 2 keys; rotation -1.
    let t2 = w.0.len();
    w.u32(2 | (2 << 24) | (3 << 28));
    w.f32(10.0);
    w.f32(4.0);
    w.u32(trans_word(0, 1023, 0));
    w.u32(trans_word(1022, 2046, 2046));
    let stream = w.0;
    let i = info(
        KeyEncoding::PerTrackCompression,
        CompressionFormat::None,
        CompressionFormat::None,
        frames,
        vec![t0 as i32, r0 as i32, -1, r1 as i32, t2 as i32, -1],
    );
    let tracks = decode_tracks(&i, &stream).unwrap();
    let a = tracks[0].translation.as_ref().unwrap();
    assert_eq!(a.codec, Codec::PerTrack);
    assert_eq!(
        (a.component_mask, a.num_keys, a.has_frame_table),
        (4, 2, true)
    );
    assert_eq!(a.translation_key(0), [0.0, 0.0, 5.0]);
    assert_eq!(a.translation_key(1), [0.0, 0.0, -5.0]);
    assert_eq!(a.frames, vec![0, 299]);
    let b = tracks[0].rotation.as_ref().unwrap();
    let q = b.rotation_key(0);
    assert!(close(q[0], 1.0) && q[1] == 0.0 && q[2] == 0.0);
    assert!(tracks[1].translation.is_none());
    let c = tracks[1].rotation.as_ref().unwrap();
    let q = c.rotation_key(0);
    assert!(close(q[0], 0.5) && close(q[1], 0.1) && close(q[2], -0.3 - 0.6));
    let d = tracks[2].translation.as_ref().unwrap();
    assert_eq!(d.header, vec![10.0, 4.0]);
    assert_eq!(d.translation_key(0), [0.0, 10.0, 0.0]);
    // Absent components stay zero even when their bits are set.
    assert_eq!(d.translation_key(1), [0.0, 14.0, 0.0]);
    assert!(tracks[2].rotation.is_none());
    assert_eq!(encode_tracks(&tracks, frames, PAD_BYTE).unwrap(), stream);
    // Interval header floats follow the component table.
    assert_eq!(PER_TRACK_NUM_COMPONENTS[3 * 8 + 2], 2);
    assert_eq!(PER_TRACK_NUM_COMPONENTS[8], 3);
    assert_eq!(PER_TRACK_NUM_COMPONENTS[6 * 8 + 7], 0);
}

fn track(
    codec: Codec,
    kind: TrackKind,
    format: CompressionFormat,
    data: KeyData,
    comps: usize,
    keys: usize,
) -> CompressedTrack {
    CompressedTrack {
        kind,
        codec,
        offset: 0,
        format,
        component_mask: 7,
        num_keys: keys,
        header: Vec::new(),
        components_per_key: comps,
        data,
        frames: Vec::new(),
        has_frame_table: false,
        end: 0,
        padding: Vec::new(),
    }
}

#[test]
fn packed_rotation_formats_decode_like_the_engine() {
    // Fixed32NoW: 11/11/10 fixed point.
    let t = track(
        Codec::Legacy,
        TrackKind::Rotation,
        CompressionFormat::Fixed32NoW,
        KeyData::U32(vec![rot_word(1023 + 511, 1023, 511 + 255)]),
        1,
        1,
    );
    let q = t.rotation_key(0);
    assert!(close(q[0], 511.0 / 1023.0) && q[1] == 0.0 && close(q[2], 255.0 / 511.0));
    // Float32NoW: small floats (3 exponent bits, sign above the mantissa).
    let x = (3 << 7) as u32; // 0.5
    let y = (1 << 10) | (2 << 7); // -0.25
    let z = (3 << 6) | 32; // 0.75
    let t = track(
        Codec::PerTrack,
        TrackKind::Rotation,
        CompressionFormat::Float32NoW,
        KeyData::U32(vec![(x << 21) | (y << 10) | z]),
        1,
        1,
    );
    let q = t.rotation_key(0);
    assert_eq!([q[0], q[1], q[2]], [0.5, -0.25, 0.75]);
    assert!(close(q[3], 0.125f32.sqrt()));
    // ACF_None (legacy): four floats as stored.
    let t = track(
        Codec::Legacy,
        TrackKind::Rotation,
        CompressionFormat::None,
        KeyData::F32(vec![0.0, 0.6, 0.0, -0.8]),
        4,
        1,
    );
    assert_eq!(t.rotation_key(0), [0.0, 0.6, 0.0, -0.8]);
    // Identity, and the engine's unsupported per-track ACF_None.
    let t = track(
        Codec::PerTrack,
        TrackKind::Rotation,
        CompressionFormat::Identity,
        KeyData::Empty,
        0,
        1,
    );
    assert_eq!(t.rotation_key(0), [0.0, 0.0, 0.0, 1.0]);
    let t = track(
        Codec::PerTrack,
        TrackKind::Rotation,
        CompressionFormat::None,
        KeyData::F32(vec![1.0; 4]),
        4,
        1,
    );
    assert!(!t.format_supported());
    assert_eq!(t.rotation_key(0), [0.0, 0.0, 0.0, 1.0]);
    // Per-track Fixed48 translation: integer offset by 255.
    let mut t = track(
        Codec::PerTrack,
        TrackKind::Translation,
        CompressionFormat::Fixed48NoW,
        KeyData::U16(vec![255, 0, 300]),
        3,
        1,
    );
    assert_eq!(t.translation_key(0), [0.0, -255.0, 45.0]);
    t.component_mask = 4;
    t.components_per_key = 1;
    t.data = KeyData::U16(vec![256]);
    assert_eq!(t.translation_key(0), [0.0, 0.0, 1.0]);
}

#[test]
fn malformed_streams_are_refused() {
    let base = |offsets: Vec<i32>| {
        info(
            KeyEncoding::ConstantKeyLerp,
            CompressionFormat::None,
            CompressionFormat::Float96NoW,
            2,
            offsets,
        )
    };
    let stream = vec![0u8; 24];
    // A valid layout for comparison.
    assert!(decode_tracks(&base(vec![0, 1, 12, 1]), &stream).is_ok());
    // Offsets that are not the sequential layout.
    assert!(decode_tracks(&base(vec![0, 1, 16, 1]), &stream).is_err());
    // Trailing bytes after the last track.
    assert!(decode_tracks(&base(vec![0, 1, 12, 1]), &[0u8; 28]).is_err());
    // Truncated stream.
    assert!(decode_tracks(&base(vec![0, 1, 12, 1]), &[0u8; 20]).is_err());
    // Zero or negative key counts.
    assert!(decode_tracks(&base(vec![0, 0, 0, 1]), &stream).is_err());
    assert!(decode_tracks(&base(vec![0, -3, 12, 1]), &stream).is_err());
    // Offset count not a multiple of four.
    assert!(decode_tracks(&base(vec![0, 1, 12]), &stream).is_err());
    // Legacy translation formats the engine cannot decode.
    let mut i = base(vec![0, 2, 24, 1]);
    i.translation_format = CompressionFormat::Fixed48NoW;
    assert!(decode_tracks(&i, &[0u8; 36]).is_err());
    // Per-track: unknown format nibble, huge key counts.
    let pt = |offsets: Vec<i32>| {
        info(
            KeyEncoding::PerTrackCompression,
            CompressionFormat::None,
            CompressionFormat::None,
            2,
            offsets,
        )
    };
    let mut w = W::default();
    w.u32(1 | (7 << 28));
    assert!(decode_tracks(&pt(vec![0, -1]), &w.0).is_err());
    let mut w = W::default();
    w.u32(0x00ff_ffff | (1 << 28));
    assert!(decode_tracks(&pt(vec![0, -1]), &w.0).is_err());
    // Odd per-track offset count.
    assert!(decode_tracks(&pt(vec![-1]), &[]).is_err());
    // Empty stream with only identity tracks is fine.
    assert_eq!(decode_tracks(&pt(vec![-1, -1]), &[]).unwrap().len(), 1);
}

#[test]
fn native_tail_round_trips_and_rejects_bad_sizes() {
    let n = AnimSequenceNative {
        start: 4,
        raw_tracks: vec![
            RawAnimTrack {
                pos_keys: vec![[1.0, 2.0, 3.0]],
                rot_keys: vec![[0.0, 0.0, 0.0, 1.0], [0.0, 1.0, 0.0, 0.0]],
            },
            RawAnimTrack {
                pos_keys: Vec::new(),
                rot_keys: Vec::new(),
            },
        ],
        compressed: vec![1, 2, 3, 4, 5],
    };
    let mut data = vec![9u8; 4];
    data.extend(encode_anim_sequence_native(&n).unwrap());
    let back = decode_anim_sequence_native(&data, 4).unwrap();
    assert_eq!(back, n);
    for cut in 4..data.len() {
        assert!(decode_anim_sequence_native(&data[..cut], 4).is_err());
    }
    let mut longer = data.clone();
    longer.push(0);
    assert!(decode_anim_sequence_native(&longer, 4).is_err());
    // A position bulk header with the wrong element size.
    let mut bad = data.clone();
    bad[8..12].copy_from_slice(&16i32.to_le_bytes());
    assert!(decode_anim_sequence_native(&bad, 4).is_err());
    // Huge counts are refused before allocating.
    let mut bad = data.clone();
    bad[4..8].copy_from_slice(&i32::MAX.to_le_bytes());
    assert!(decode_anim_sequence_native(&bad, 4).is_err());
}

#[test]
fn stream_mutations_never_panic() {
    // Base: the per-track stream of the test above, rebuilt here.
    let mut w = W::default();
    w.u32(2 | ((4 | 8) << 24) | (1 << 28));
    w.f32(5.0);
    w.f32(-5.0);
    w.pad();
    w.u16(0);
    w.u16(299);
    w.pad();
    let r0 = w.0.len() as i32;
    w.u32(1 | (5 << 24) | (2 << 28));
    w.u16(65534);
    w.u16(32767);
    w.pad();
    let base = w.0;
    let i = info(
        KeyEncoding::PerTrackCompression,
        CompressionFormat::None,
        CompressionFormat::None,
        300,
        vec![0, r0],
    );
    let mut seed = 0x1234_5678_u64;
    let mut next = || {
        seed = seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
        seed >> 33
    };
    for _ in 0..5000 {
        let mut m = base.clone();
        let at = (next() as usize) % m.len();
        m[at] = next() as u8;
        if let Ok(tracks) = decode_tracks(&i, &m) {
            for t in tracks
                .iter()
                .flat_map(|t| [&t.translation, &t.rotation])
                .flatten()
            {
                for k in 0..t.num_keys.min(4) {
                    let _ = t.rotation_key(k);
                    let _ = t.translation_key(k);
                }
                let _ = sample_rotation(t, 0.3, 1.0, 300);
            }
            // Padding may hold any byte; with the canonical fill the stream
            // re-encodes exactly when the padding was canonical.
            let canonical = tracks
                .iter()
                .flat_map(|t| [&t.translation, &t.rotation])
                .flatten()
                .all(|t| t.padding.iter().all(|&b| b == PAD_BYTE));
            if canonical {
                assert_eq!(encode_tracks(&tracks, 300, PAD_BYTE).unwrap(), m);
            }
        }
    }
}

#[test]
fn key_positions_and_pose_helpers() {
    // Non-looping: keys span the whole sequence.
    assert_eq!(even_key_position(5, 5, 0.0, false), (0, 0, 0.0));
    assert_eq!(even_key_position(5, 5, 1.0, false), (4, 4, 0.0));
    let (k0, k1, a) = even_key_position(5, 5, 0.6, false);
    assert_eq!((k0, k1), (2, 3));
    assert!(close(a, 0.4));
    // Looping with one key per frame: the last key blends back to key 0.
    let (k0, k1, _) = even_key_position(5, 5, 0.95, true);
    assert_eq!((k0, k1), (4, 0));
    assert_eq!(even_key_position(5, 5, 1.0, true), (0, 0, 0.0));
    // nlerp takes the short arc.
    let q = nlerp([0.0, 0.0, 0.0, 1.0], [0.0, 0.0, 0.0, -1.0], 0.5);
    assert!(close(q[3].abs(), 1.0));
    // Pose rule: W negated except on the root bone.
    assert_eq!(pose_rotation([0.1, 0.2, 0.3, 0.9], 0), [0.1, 0.2, 0.3, 0.9]);
    assert_eq!(
        pose_rotation([0.1, 0.2, 0.3, 0.9], 3),
        [0.1, 0.2, 0.3, -0.9]
    );
    let table = bone_to_track(
        &["spine".to_owned(), "Root".to_owned()],
        &["root".to_owned(), "Arm".to_owned(), "Spine".to_owned()],
    );
    assert_eq!(table, vec![Some(1), None, Some(0)]);
}

fn prop(name: &str, value: Value) -> Property {
    Property {
        name: name.to_owned(),
        type_name: String::new(),
        array_index: 0,
        size: 0,
        struct_name: None,
        enum_name: None,
        value,
        offset: 0,
    }
}

#[test]
fn sequence_info_applies_class_defaults() {
    let i = anim_sequence_info(&[
        prop("SequenceName", Value::Name("Run".to_owned())),
        prop("NumFrames", Value::Int(31)),
        prop("SequenceLength", Value::Float(1.0)),
        prop(
            "RotationCompressionFormat",
            Value::Enum("ACF_Fixed48NoW".to_owned()),
        ),
        prop(
            "KeyEncodingFormat",
            Value::Enum("AKF_VariableKeyLerp".to_owned()),
        ),
        prop(
            "CompressedTrackOffsets",
            Value::Array(vec![
                Value::Int(0),
                Value::Int(1),
                Value::Int(12),
                Value::Int(1),
            ]),
        ),
    ])
    .unwrap();
    assert_eq!(i.sequence_name, "Run");
    assert_eq!(i.rate_scale, 1.0);
    assert_eq!(i.translation_format, CompressionFormat::None);
    assert_eq!(i.rotation_format, CompressionFormat::Fixed48NoW);
    assert_eq!(i.key_encoding, KeyEncoding::VariableKeyLerp);
    assert_eq!(i.compressed_track_offsets, vec![0, 1, 12, 1]);
    // Defaults with no tags at all.
    let d = anim_sequence_info(&[]).unwrap();
    assert_eq!(d.key_encoding, KeyEncoding::ConstantKeyLerp);
    assert_eq!(d.rotation_format, CompressionFormat::None);
    // Unknown enumerators are refused.
    assert!(
        anim_sequence_info(&[prop(
            "KeyEncodingFormat",
            Value::Enum("AKF_Bogus".to_owned())
        )])
        .is_err()
    );
    assert!(
        anim_sequence_info(&[prop(
            "RotationCompressionFormat",
            Value::Enum("ACF_Bogus".to_owned())
        )])
        .is_err()
    );
}
