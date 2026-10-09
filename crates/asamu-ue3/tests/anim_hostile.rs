//! Hostile-input and edge-case tests for the animation decoder and the
//! engine-semantics helpers (sampling, key times, key lookup). Streams are
//! written byte by byte here; no game data.

#![allow(clippy::unwrap_used)]

use asamu_ue3::anim::{
    AnimSequenceInfo, Codec, CompressedTrack, CompressionFormat, KeyData, KeyEncoding, PAD_BYTE,
    TrackKind, decode_tracks, encode_tracks, even_key_position, sample_rotation,
    sample_translation,
};

fn info(enc: KeyEncoding, frames: i32, offsets: Vec<i32>) -> AnimSequenceInfo {
    AnimSequenceInfo {
        sequence_name: "Hostile".to_owned(),
        num_frames: frames,
        sequence_length: 1.0,
        rate_scale: 1.0,
        no_looping_interpolation: false,
        is_additive: false,
        translation_format: CompressionFormat::None,
        rotation_format: CompressionFormat::Float96NoW,
        key_encoding: enc,
        compressed_track_offsets: offsets,
        notifies: Vec::new(),
        compression_scheme: None,
        additive_ref_name: None,
        encoding_pkg_version: 0,
    }
}

fn track(
    kind: TrackKind,
    values: Vec<f32>,
    keys: usize,
    frames: Option<Vec<u16>>,
) -> CompressedTrack {
    CompressedTrack {
        kind,
        codec: Codec::PerTrack,
        offset: 0,
        format: CompressionFormat::Float96NoW,
        component_mask: 7,
        num_keys: keys,
        header: Vec::new(),
        components_per_key: 3,
        data: KeyData::F32(values),
        has_frame_table: frames.is_some(),
        frames: frames.unwrap_or_default(),
        end: 0,
        padding: Vec::new(),
    }
}

/// Translation track whose keys are `(k, 10k, 100k)` for key `k`.
fn ramp(keys: usize, frames: Option<Vec<u16>>) -> CompressedTrack {
    let values = (0..keys)
        .flat_map(|k| {
            let k = k as f32;
            [k, 10.0 * k, 100.0 * k]
        })
        .collect();
    track(TrackKind::Translation, values, keys, frames)
}

#[test]
fn sampling_and_key_lookup_survive_hostile_tags() {
    let rotation = track(
        TrackKind::Rotation,
        vec![0.1, 0.2, 0.3, 0.3, 0.2, 0.1, -0.2, 0.1, 0.0],
        3,
        None,
    );
    let tracks = [
        ramp(4, None),
        ramp(4, Some(vec![0, 3, 9, 9])),
        ramp(0, None),
        ramp(1, Some(vec![0])),
        rotation,
    ];
    let frames = [i32::MIN, -1, 0, 1, 2, 255, 256, i32::MAX];
    let lengths = [
        f32::NAN,
        f32::INFINITY,
        f32::NEG_INFINITY,
        -1.0,
        0.0,
        1.0,
        f32::MAX,
    ];
    let times = [
        f32::NAN,
        f32::NEG_INFINITY,
        -1.0,
        0.0,
        0.5,
        1.0,
        2.0,
        f32::INFINITY,
    ];
    for t in &tracks {
        for &nf in &frames {
            for &len in &lengths {
                for k in [0, 1, 3, usize::MAX / 2, usize::MAX] {
                    let _ = t.key_time(k, len, nf);
                    // Out-of-range keys read as missing data, never overflow.
                    let q = t.rotation_key(k);
                    let v = t.translation_key(k);
                    assert!(q.iter().chain(v.iter()).all(|c| c.is_finite()));
                }
                for &time in &times {
                    let q = sample_rotation(t, time, len, nf);
                    let v = sample_translation(t, time, len, nf);
                    // Finite keys always sample to finite values.
                    assert!(
                        q.iter().chain(v.iter()).all(|c| c.is_finite()),
                        "{t:?} nf {nf} len {len} time {time}: {q:?} {v:?}"
                    );
                }
            }
        }
    }
    for &nf in &frames {
        for &rel in &times {
            for keys in [0usize, 1, 2, 7] {
                for looping in [false, true] {
                    let (k0, k1, a) = even_key_position(keys, nf, rel, looping);
                    assert!(k0 < keys.max(1) && k1 < keys.max(1), "{keys} {nf} {rel}");
                    assert!(a.is_finite());
                }
            }
        }
    }
}

#[test]
fn frame_table_search_follows_the_engine() {
    // Keys at frames 0, 3, 9, 9 of 10 (the last frame repeated, as on 515
    // shipped tracks): the engine interpolates towards the FIRST key of the
    // repeated frame and only shows the last key from the end on.
    let t = ramp(4, Some(vec![0, 3, 9, 9]));
    let at = |frame: f32| sample_translation(&t, frame / 9.0, 1.0, 10);
    assert_eq!(at(0.0), [0.0, 0.0, 0.0]);
    assert!((at(3.0)[0] - 1.0).abs() < 1e-5);
    // Frame 6: halfway between key 1 (frame 3) and key 2 (frame 9).
    let v = at(6.0);
    assert!((v[0] - 1.5).abs() < 1e-5, "{v:?}");
    // Frame 8.9: almost at key 2, nowhere near key 3.
    let v = at(8.9);
    assert!((v[0] - 2.0).abs() < 0.02, "{v:?}");
    // The end of the sequence: key 3.
    assert_eq!(at(9.0), [3.0, 30.0, 300.0]);
    assert_eq!(t.key_time(2, 1.0, 10), 1.0);
    assert_eq!(t.key_time(3, 1.0, 10), 1.0);
    // Even keys span the sequence (non-looping).
    let e = ramp(4, None);
    assert_eq!(sample_translation(&e, 0.5, 1.0, 10)[0], 1.5);
    assert_eq!(e.key_time(3, 2.0, 10), 2.0);
}

#[test]
fn extreme_counts_and_offsets_are_refused() {
    // Per-track header claiming 2^24 - 1 keys over an 8-byte stream.
    let mut stream = Vec::new();
    stream.extend_from_slice(&(0x00ff_ffffu32 | (7 << 24) | (1 << 28)).to_le_bytes());
    stream.extend_from_slice(&[0; 4]);
    let i = info(KeyEncoding::PerTrackCompression, 10, vec![0, -1]);
    assert!(decode_tracks(&i, &stream).is_err());
    // Interval header claiming more floats than present.
    let mut stream = Vec::new();
    stream.extend_from_slice(&(2u32 | (3 << 28)).to_le_bytes());
    let i = info(KeyEncoding::PerTrackCompression, 10, vec![0, -1]);
    assert!(decode_tracks(&i, &stream).is_err());
    // Legacy key counts and offsets at the extremes.
    for counts in [
        [0, i32::MAX, 0, 1],
        [0, 1, 12, i32::MAX],
        [0, -5, 12, 1],
        [0, 0, 12, 1],
        [i32::MAX, 1, 12, 1],
        [-1, 1, 12, 1],
        [0, 1, -2, 1],
    ] {
        let mut stream = vec![0u8; 24];
        stream.extend_from_slice(&[0; 12]);
        for enc in [KeyEncoding::ConstantKeyLerp, KeyEncoding::VariableKeyLerp] {
            let i = info(enc, 10, counts.to_vec());
            assert!(decode_tracks(&i, &stream).is_err(), "{counts:?}");
        }
    }
    // Offsets that are not a whole number of tracks.
    let i = info(KeyEncoding::ConstantKeyLerp, 10, vec![0, 1, 12]);
    assert!(decode_tracks(&i, &[0; 24]).is_err());
    let i = info(KeyEncoding::PerTrackCompression, 10, vec![-1]);
    assert!(decode_tracks(&i, &[]).is_err());
    // Per-track: a negative offset other than -1, and an unknown format.
    let i = info(KeyEncoding::PerTrackCompression, 10, vec![-2, -1]);
    assert!(decode_tracks(&i, &[0; 8]).is_err());
    let i = info(KeyEncoding::PerTrackCompression, 10, vec![0, -1]);
    assert!(decode_tracks(&i, &(1u32 | (9 << 28)).to_le_bytes()).is_err());
}

/// A VariableKeyLerp stream with `u16` frame tables (NumFrames > 255) and a
/// ConstantKeyLerp stream with an interval rotation track.
fn legacy_streams() -> Vec<(AnimSequenceInfo, Vec<u8>)> {
    let mut out = Vec::new();
    // VariableKeyLerp: translation ACF_None 3 keys, rotation Float96NoW 2 keys.
    let mut s = Vec::new();
    for v in [0.0f32, 1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0] {
        s.extend_from_slice(&v.to_le_bytes());
    }
    for f in [0u16, 150, 299] {
        s.extend_from_slice(&f.to_le_bytes());
    }
    while !s.len().is_multiple_of(4) {
        s.push(PAD_BYTE);
    }
    let rot_at = i32::try_from(s.len()).unwrap();
    for v in [0.1f32, 0.0, 0.0, 0.0, 0.2, 0.0] {
        s.extend_from_slice(&v.to_le_bytes());
    }
    for f in [0u16, 299] {
        s.extend_from_slice(&f.to_le_bytes());
    }
    let mut i = info(KeyEncoding::VariableKeyLerp, 300, vec![0, 3, rot_at, 2]);
    i.rotation_format = CompressionFormat::Float96NoW;
    out.push((i, s));
    // ConstantKeyLerp: translation one key, rotation interval 2 keys.
    let mut s = Vec::new();
    for v in [1.0f32, 2.0, 3.0] {
        s.extend_from_slice(&v.to_le_bytes());
    }
    let rot_at = i32::try_from(s.len()).unwrap();
    for v in [-0.5f32, -0.5, -0.5, 1.0, 1.0, 1.0] {
        s.extend_from_slice(&v.to_le_bytes());
    }
    for w in [0x8020_0100u32, 0x7fef_feff] {
        s.extend_from_slice(&w.to_le_bytes());
    }
    let mut i = info(KeyEncoding::ConstantKeyLerp, 30, vec![0, 1, rot_at, 2]);
    i.rotation_format = CompressionFormat::IntervalFixed32NoW;
    out.push((i, s));
    out
}

#[test]
fn legacy_stream_mutations_never_panic() {
    let mut seed = 0x0bad_5eed_u64;
    let mut next = move || {
        seed = seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
        seed >> 33
    };
    for (base_info, base) in legacy_streams() {
        let tracks = decode_tracks(&base_info, &base).unwrap();
        assert_eq!(
            encode_tracks(&tracks, base_info.num_frames, PAD_BYTE).unwrap(),
            base
        );
        let mut accepted = 0;
        for round in 0..4000 {
            let mut m = base.clone();
            let mut i = base_info.clone();
            match round % 4 {
                0 | 1 => {
                    let at = (next() as usize) % m.len();
                    m[at] = next() as u8;
                }
                2 => {
                    let k = (next() as usize) % i.compressed_track_offsets.len();
                    i.compressed_track_offsets[k] =
                        [i32::MIN, -1, 0, 1, 2, 3, 4, 12, 36, i32::MAX][(next() % 10) as usize];
                }
                _ => {
                    i.num_frames = [i32::MIN, -1, 0, 1, 255, 256, i32::MAX][(next() % 7) as usize];
                }
            }
            if let Ok(tracks) = decode_tracks(&i, &m) {
                accepted += 1;
                for t in tracks
                    .iter()
                    .flat_map(|t| [&t.translation, &t.rotation])
                    .flatten()
                {
                    for k in 0..t.num_keys.min(4) {
                        let _ = (t.rotation_key(k), t.translation_key(k));
                        let _ = t.key_time(k, 1.0, i.num_frames);
                    }
                    let _ = sample_rotation(t, 0.7, 1.0, i.num_frames);
                    let _ = sample_translation(t, 0.7, 1.0, i.num_frames);
                }
                let canonical = tracks
                    .iter()
                    .flat_map(|t| [&t.translation, &t.rotation])
                    .flatten()
                    .all(|t| t.padding.iter().all(|&b| b == PAD_BYTE));
                if canonical {
                    assert_eq!(
                        encode_tracks(&tracks, i.num_frames, PAD_BYTE).unwrap(),
                        m,
                        "round {round}"
                    );
                }
            }
        }
        assert!(accepted > 500, "{accepted}");
    }
}
