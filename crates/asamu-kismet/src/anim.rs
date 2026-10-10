//! Matinee animation-control tracks (`InterpTrackAnimControl`): which
//! sequence a skeletal actor plays at a track time and the `SetAnimPosition`
//! calls an update makes on it (with or without firing the sequence's
//! notifies).
//!
//! Port of the locally decompiled `UInterpTrackAnimControl::GetAnimForTime`,
//! `UpdateTrack` and `CalcChannelIndex` of the shipped Mac executable
//! (described in our own words; MATINEE.md "Animation-control tracks"):
//!
//! - Keys are `(StartTime, AnimSeqName, AnimStartOffset, AnimEndOffset,
//!   AnimPlayRate, bLooping, bReverse)`. The key in effect at `t` is the last
//!   one starting at or before `t`; before the first key, the first key's
//!   sequence at its start offset.
//! - The position in a key is `(t − StartTime) · AnimPlayRate`. A looping key
//!   wraps it by `fmod` into the sequence length minus both offsets (at least
//!   0.01 s) and adds the start offset; a non-looping key adds the start
//!   offset and clamps to `[0, SequenceLength − AnimEndOffset + 1e-4]`. A
//!   reversed key mirrors the position inside the offsets. When the
//!   sequence is not in the track's anim sets the raw position is used.
//! - An update that jumps, goes backwards, or of a track without keys calls
//!   `SetAnimPosition` once for the time's key with notifies off. A forwards
//!   update walks every key between the last and the new time: each key's
//!   span up to the next key's start (or the new time) is played with
//!   notifies on (unless `bSkipAnimNotifiers`, or the position equals the
//!   start offset); a looping key that wraps first plays to its end
//!   (`SequenceLength − AnimEndOffset + 1e-4`, notifies on) and restarts at
//!   its start offset (notifies off) once per wrap; then the next key's
//!   sequence is set at its start offset with notifies off. Constants
//!   CONFIRMED from the executable: 1e-4 (float), 0.01 (float), 1e-4
//!   (double).
//! - The channel is the number of earlier animation-control tracks of the
//!   group with the same slot name.

use crate::matinee::{AnimControlKey, AnimControlTrack, TrackData};

/// The upper position slack of a non-looping key (float `1e-4` at
/// 0x10163FED8, CONFIRMED).
pub const END_SLACK: f32 = 1.0e-4;
/// The shortest loop length (float `0.01` at 0x101633DA8, CONFIRMED).
pub const MIN_LOOP_LENGTH: f32 = 0.01;
/// The end position a wrapping loop plays to, past the end offset (double
/// `1e-4` at 0x1016393A0, CONFIRMED).
pub const LOOP_END_SLACK: f64 = 1.0e-4;

/// Most `SetAnimPosition` calls one track update makes (ours: the native
/// has no limit; a real update makes a handful, and a hostile track with
/// many keys or absurd play rates must not allocate without bound).
pub const MAX_CALLS_PER_UPDATE: usize = 4096;

/// One `SetAnimPosition` event call on the group's actor.
#[derive(Debug, Clone, PartialEq)]
pub struct AnimPosition {
    /// `SlotName`.
    pub slot: Option<String>,
    /// Channel index (see the module docs).
    pub channel: usize,
    /// `AnimSeqName`.
    pub sequence: String,
    /// Position, s.
    pub position: f32,
    /// `bFireNotifies`.
    pub fire_notifies: bool,
    /// `bLooping`.
    pub looping: bool,
    /// `bEnableRootMotion`.
    pub root_motion: bool,
}

/// `CalcChannelIndex`: earlier animation-control tracks of `tracks` with the
/// same slot name as track `index`. Disabled tracks are not counted (the
/// native tests the earlier track's disabled flag; CONFIRMED, decompiled).
#[must_use]
pub fn channel_index(tracks: &[crate::matinee::Track], index: usize) -> usize {
    let slot = match tracks.get(index).map(|t| &t.data) {
        Some(TrackData::AnimControl(a)) => a.slot.clone(),
        _ => return 0,
    };
    tracks
        .iter()
        .take(index)
        .filter(|t| !t.disabled && matches!(&t.data, TrackData::AnimControl(a) if a.slot == slot))
        .count()
}

fn mirrored(key: &AnimControlKey, len: f32, pos: f32) -> f32 {
    ((len - (key.end_offset + key.start_offset)) - (pos - key.start_offset)) + key.start_offset
}

fn fmodf(a: f32, b: f32) -> f32 {
    a % b
}

/// Position inside key `key` for `elapsed` key time (already multiplied by
/// the play rate), with the sequence length `len` when known.
fn key_position(key: &AnimControlKey, scaled: f32, len: Option<f32>) -> f32 {
    let Some(len) = len else {
        return scaled;
    };
    let pos = if key.looping {
        let mut loop_len = len - (key.start_offset + key.end_offset);
        if loop_len <= MIN_LOOP_LENGTH {
            loop_len = MIN_LOOP_LENGTH;
        }
        fmodf(scaled, loop_len) + key.start_offset
    } else {
        let p = key.start_offset + scaled;
        let max = (len - key.end_offset) + END_SLACK;
        if p < 0.0 {
            0.0
        } else if max <= p {
            max
        } else {
            p
        }
    };
    if key.reverse {
        mirrored(key, len, pos)
    } else {
        pos
    }
}

/// `GetAnimForTime`: the sequence, position and looping flag at `t`
/// (`None` without keys or when the key names no sequence). `len` gives a
/// sequence's `SequenceLength` (`None` when it is not in the anim sets).
pub fn anim_for_time(
    track: &AnimControlTrack,
    t: f32,
    len: &dyn Fn(&str) -> Option<f32>,
) -> Option<(String, f32, bool)> {
    let first = track.keys.first()?;
    if t < first.start_time || t.is_nan() {
        let seq = first.sequence.clone()?;
        let mut pos = first.start_offset;
        // The native writes the looping flag only for a reversed first key
        // (whether or not its sequence is found); otherwise the caller's
        // value is left as it was, which we take as not looping.
        let mut looping = false;
        if first.reverse {
            if let Some(l) = len(&seq) {
                pos = mirrored(first, l, pos);
            }
            looping = first.looping;
        }
        return Some((seq, pos, looping));
    }
    let i = track
        .keys
        .iter()
        .rposition(|k| k.start_time <= t)
        .unwrap_or(0);
    let key = track.keys.get(i)?;
    let seq = key.sequence.clone()?;
    let scaled = (t - key.start_time) * key.play_rate;
    let l = len(&seq);
    let pos = key_position(key, scaled, l);
    Some((seq, pos, l.is_some() && key.looping))
}

/// `UInterpTrackAnimControl::UpdateTrack` from `last` to `new` (`jump` for
/// a jump): the `SetAnimPosition` calls in order.
pub fn update_track(
    track: &AnimControlTrack,
    channel: usize,
    last: f32,
    new: f32,
    jump: bool,
    len: &dyn Fn(&str) -> Option<f32>,
) -> Vec<AnimPosition> {
    let mut out = Vec::new();
    let call = |sequence: String, position: f32, fire: bool, looping: bool| AnimPosition {
        slot: track.slot.clone(),
        channel,
        sequence,
        position,
        fire_notifies: fire,
        looping,
        root_motion: track.root_motion,
    };
    let fire_if = |moved: bool| moved && !track.skip_notifiers;
    if track.keys.is_empty() || jump || new <= last || new.is_nan() {
        if let Some((seq, pos, looping)) = anim_for_time(track, new, len) {
            out.push(call(seq, pos, false, looping));
        }
        return out;
    }
    let n = track.keys.len();
    // The key in effect at `last` (−1 before the first key) and at `new`.
    let key_at = |t: f32| -> isize {
        let mut i: isize = -1;
        while let Some(k) = track.keys.get((i + 1) as usize) {
            if (i + 1) as usize >= n || t < k.start_time {
                break;
            }
            i += 1;
        }
        i.min(n as isize - 1)
    };
    let start = key_at(last);
    let end = key_at(new);
    let mut i = start;
    while i <= end && out.len() < MAX_CALLS_PER_UPDATE {
        if i < 0 {
            // Before the first key: its sequence at its start offset.
            if let Some(k) = track.keys.first()
                && let Some(seq) = k.sequence.clone()
            {
                let mut pos = k.start_offset;
                if k.reverse
                    && let Some(l) = len(&seq)
                {
                    pos = mirrored(k, l, pos);
                }
                out.push(call(seq, pos, false, k.looping));
            }
        } else if let Some(k) = track.keys.get(i as usize) {
            let from = if i == start { last } else { k.start_time };
            let to = if i == end {
                new
            } else {
                track
                    .keys
                    .get(i as usize + 1)
                    .map_or(new, |next| next.start_time)
            };
            if let Some(seq) = k.sequence.clone() {
                let l = len(&seq);
                if k.looping {
                    if let Some(l) = l {
                        let mut loop_len = l - (k.start_offset + k.end_offset);
                        if loop_len <= MIN_LOOP_LENGTH {
                            loop_len = MIN_LOOP_LENGTH;
                        }
                        let scaled_to = (to - k.start_time) * k.play_rate;
                        if !k.reverse {
                            let n0 = (((from - k.start_time) * k.play_rate + k.start_offset)
                                / loop_len)
                                .floor();
                            let n1 = ((k.start_offset + scaled_to) / loop_len).floor();
                            let wraps = (n1 as i64).saturating_sub(n0 as i64);
                            let end_pos = (f64::from(l - k.end_offset) + LOOP_END_SLACK) as f32;
                            let room = MAX_CALLS_PER_UPDATE.saturating_sub(out.len()) / 2;
                            let room = i64::try_from(room).unwrap_or(i64::MAX);
                            for _ in 0..wraps.clamp(0, room.min(1024)) {
                                out.push(call(seq.clone(), end_pos, true, true));
                                out.push(call(seq.clone(), k.start_offset, false, true));
                            }
                        }
                        let mut pos = fmodf(scaled_to, loop_len) + k.start_offset;
                        if k.reverse {
                            pos = mirrored(k, l, pos);
                        }
                        out.push(call(seq, pos, fire_if(true), true));
                    } else {
                        let pos = k.start_offset + k.play_rate * (to - k.start_time);
                        out.push(call(seq, pos, fire_if(true), true));
                    }
                } else {
                    let scaled = (to - k.start_time) * k.play_rate;
                    // Without the sequence the update keeps the start
                    // offset (unlike `GetAnimForTime`).
                    let pos = match l {
                        Some(_) => key_position(k, scaled, l),
                        None => scaled + k.start_offset,
                    };
                    let moved = pos != k.start_offset;
                    out.push(call(seq, pos, fire_if(moved), false));
                }
            }
        }
        // The next key's sequence at its start (notifies off). Not after
        // the before-the-first-key step, which already made that call (the
        // native's loop has no second one there).
        if i >= 0
            && i < end
            && let Some(next) = track.keys.get((i + 1) as usize)
            && let Some(seq) = next.sequence.clone()
        {
            let mut pos = next.start_offset;
            if next.reverse
                && let Some(l) = len(&seq)
            {
                pos = mirrored(next, l, pos);
            }
            out.push(call(seq, pos, false, next.looping));
        }
        i += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::matinee::Track;

    fn key(t: f32, seq: &str) -> AnimControlKey {
        AnimControlKey {
            start_time: t,
            sequence: Some(seq.into()),
            ..AnimControlKey::default()
        }
    }

    fn lens(name: &str) -> Option<f32> {
        match name {
            "Walk" => Some(2.0),
            "Wave" => Some(1.0),
            _ => None,
        }
    }

    #[test]
    fn positions_follow_key_offsets_rates_loops_and_reversal() {
        let mut t = AnimControlTrack {
            keys: vec![key(1.0, "Walk")],
            ..AnimControlTrack::default()
        };
        // Before the first key: its sequence at its start offset.
        assert_eq!(
            anim_for_time(&t, 0.5, &lens),
            Some(("Walk".into(), 0.0, false))
        );
        // Non-looping: clamped to the length + 1e-4.
        assert_eq!(anim_for_time(&t, 2.0, &lens).unwrap().1, 1.0);
        assert_eq!(anim_for_time(&t, 9.0, &lens).unwrap().1, 2.0 + END_SLACK);
        t.keys[0].looping = true;
        t.keys[0].play_rate = 2.0;
        // 1.5 s of key time × 2 = 3 → wrapped into 2 s.
        let (_, p, looping) = anim_for_time(&t, 2.5, &lens).unwrap();
        assert!((p - 1.0).abs() < 1e-6 && looping);
        t.keys[0].reverse = true;
        let (_, p, _) = anim_for_time(&t, 2.5, &lens).unwrap();
        assert!((p - 1.0).abs() < 1e-6);
        let (_, p, _) = anim_for_time(&t, 1.25, &lens).unwrap();
        assert!((p - 1.5).abs() < 1e-6);
        // Unknown sequence: the raw scaled position.
        let u = AnimControlTrack {
            keys: vec![AnimControlKey {
                play_rate: 3.0,
                ..key(0.0, "Unknown")
            }],
            ..AnimControlTrack::default()
        };
        assert_eq!(anim_for_time(&u, 1.0, &lens).unwrap().1, 3.0);
        assert!(anim_for_time(&AnimControlTrack::default(), 1.0, &lens).is_none());
    }

    #[test]
    fn forwards_updates_fire_notifies_and_jumps_do_not() {
        let t = AnimControlTrack {
            keys: vec![key(0.0, "Wave"), key(2.0, "Walk")],
            ..AnimControlTrack::default()
        };
        let calls = update_track(&t, 0, 0.5, 2.5, false, &lens);
        // Wave played to its clamped end with notifies, then Walk set at its
        // start without, then Walk played to 0.5 with notifies.
        assert_eq!(calls.len(), 3);
        assert_eq!(calls[0].sequence, "Wave");
        assert!(calls[0].fire_notifies && (calls[0].position - (1.0 + END_SLACK)).abs() < 1e-6);
        assert_eq!(calls[1].sequence, "Walk");
        assert!(!calls[1].fire_notifies && calls[1].position == 0.0);
        assert!(calls[2].fire_notifies && (calls[2].position - 0.5).abs() < 1e-6);
        // Backwards or a jump: one call, notifies off.
        let back = update_track(&t, 0, 2.5, 0.5, false, &lens);
        assert_eq!(back.len(), 1);
        assert!(!back[0].fire_notifies && back[0].sequence == "Wave");
        let jump = update_track(&t, 0, 0.0, 2.5, true, &lens);
        assert_eq!(jump.len(), 1);
        // Skipping notifiers.
        let skip = AnimControlTrack {
            skip_notifiers: true,
            ..t.clone()
        };
        assert!(
            update_track(&skip, 0, 0.5, 2.5, false, &lens)
                .iter()
                .all(|c| !c.fire_notifies)
        );
    }

    #[test]
    fn looping_keys_play_to_the_end_once_per_wrap() {
        let t = AnimControlTrack {
            keys: vec![AnimControlKey {
                looping: true,
                ..key(0.0, "Wave")
            }],
            ..AnimControlTrack::default()
        };
        let calls = update_track(&t, 3, 0.5, 2.25, false, &lens);
        // Two wraps (1.0 and 2.0): end (notifies) + restart (no notifies)
        // each, then the position 0.25.
        assert_eq!(calls.len(), 5);
        assert!(calls[0].fire_notifies && calls[0].position > 1.0);
        assert!(!calls[1].fire_notifies && calls[1].position == 0.0);
        assert!((calls[4].position - 0.25).abs() < 1e-5);
        assert!(calls.iter().all(|c| c.channel == 3 && c.looping));
    }

    #[test]
    fn channels_count_earlier_tracks_of_the_same_slot() {
        let track = |slot: &str| Track {
            class: "Engine.InterpTrackAnimControl".into(),
            disabled: false,
            data: TrackData::AnimControl(AnimControlTrack {
                slot: Some(slot.into()),
                ..AnimControlTrack::default()
            }),
        };
        let mut tracks = vec![track("Body"), track("Face"), track("Body")];
        assert_eq!(channel_index(&tracks, 0), 0);
        assert_eq!(channel_index(&tracks, 1), 0);
        assert_eq!(channel_index(&tracks, 2), 1);
        assert_eq!(channel_index(&tracks, 7), 0);
        // A disabled earlier track of the slot is not counted.
        tracks[0].disabled = true;
        assert_eq!(channel_index(&tracks, 2), 0);
    }

    /// An update that starts before the first key makes the native's two
    /// calls: the first key's sequence at its start offset (notifies off),
    /// then that key played to the new time (notifies on) — no second
    /// "next key" call in between.
    #[test]
    fn an_update_from_before_the_first_key_sets_the_sequence_once() {
        let t = AnimControlTrack {
            keys: vec![
                AnimControlKey {
                    start_offset: 0.25,
                    ..key(1.0, "Walk")
                },
                key(5.0, "Wave"),
            ],
            ..AnimControlTrack::default()
        };
        let calls = update_track(&t, 0, 0.5, 1.5, false, &lens);
        assert_eq!(calls.len(), 2, "{calls:?}");
        assert_eq!(calls[0].sequence, "Walk");
        assert!(!calls[0].fire_notifies && calls[0].position == 0.25);
        assert!(calls[1].fire_notifies && (calls[1].position - 0.75).abs() < 1e-6);
        // Still before the first key: only the silent call.
        let calls = update_track(&t, 0, 0.25, 0.5, false, &lens);
        assert_eq!(calls.len(), 1);
        assert!(!calls[0].fire_notifies && calls[0].position == 0.25);
        // Across both keys from before the first: start of Walk, Walk played
        // to its end, Wave set at its start, Wave played.
        let calls = update_track(&t, 0, 0.5, 5.5, false, &lens);
        let seqs: Vec<(&str, bool)> = calls
            .iter()
            .map(|c| (c.sequence.as_str(), c.fire_notifies))
            .collect();
        assert_eq!(
            seqs,
            vec![
                ("Walk", false),
                ("Walk", true),
                ("Wave", false),
                ("Wave", true)
            ]
        );
        // A key played to exactly its start offset fires nothing.
        let calls = update_track(&t, 0, 0.5, 1.0, false, &lens);
        assert!(calls.iter().all(|c| !c.fire_notifies), "{calls:?}");
    }

    /// A reversed first key reports its looping flag before its start even
    /// when its sequence is unknown (the native writes the flag whenever the
    /// key is reversed).
    #[test]
    fn a_reversed_first_key_reports_looping_before_its_start() {
        let t = AnimControlTrack {
            keys: vec![AnimControlKey {
                looping: true,
                reverse: true,
                ..key(1.0, "Unknown")
            }],
            ..AnimControlTrack::default()
        };
        assert_eq!(
            anim_for_time(&t, 0.0, &lens),
            Some(("Unknown".into(), 0.0, true))
        );
        let known = AnimControlTrack {
            keys: vec![AnimControlKey {
                looping: true,
                reverse: true,
                ..key(1.0, "Walk")
            }],
            ..AnimControlTrack::default()
        };
        // Mirrored inside the 2 s sequence: the start offset 0 becomes 2.
        assert_eq!(
            anim_for_time(&known, 0.0, &lens),
            Some(("Walk".into(), 2.0, true))
        );
    }

    /// One update never makes more than [`MAX_CALLS_PER_UPDATE`] calls,
    /// however many keys it crosses and however often each loop wraps.
    #[test]
    fn one_update_makes_a_bounded_number_of_calls() {
        let keys: Vec<AnimControlKey> = (0..20_000)
            .map(|i| AnimControlKey {
                looping: true,
                play_rate: 1.0e6,
                ..key(i as f32 * 0.001, "Wave")
            })
            .collect();
        let t = AnimControlTrack {
            keys,
            ..AnimControlTrack::default()
        };
        let calls = update_track(&t, 0, -1.0, 100.0, false, &lens);
        assert!(
            calls.len() <= MAX_CALLS_PER_UPDATE + 4,
            "{} calls",
            calls.len()
        );
        assert!(!calls.is_empty());
    }
}
