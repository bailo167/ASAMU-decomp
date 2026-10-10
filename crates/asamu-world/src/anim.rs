//! Skeletal animation as the simulation needs it: a sequence node's time
//! keeping (`UAnimNodeSequence`) and the animation notifies it fires
//! (`AnimNotify_Kismet` → Kismet `SeqEvent_AnimNotify`, `AnimNotify_Sound` →
//! a sound at the actor). Rendering follows the node's sequence and
//! position.
//!
//! Engine rules (locally decompiled `UAnimNodeSequence::{TickAnim, AdvanceBy,
//! SetPosition, IssueNotifies}`, `UAnimNodeSlot::MAT_SetAnimPosition`,
//! `UAnimNotify_Kismet::Notify`, `UAnimNotify_Sound::Notify`; described in
//! our own words; NPCS.md "Animation notifies"):
//!
//! - `TickAnim`: a playing node advances by `Rate · RateScale · dt`
//!   (`AdvanceBy`), issuing the notifies of the move first, then moving the
//!   position: past the end a looping node wraps (`fmod`), a non-looping one
//!   stops at the end.
//! - `SetPosition(p, fire)`: `p` is clamped to `[0, SequenceLength + 1e-4]`
//!   (double `1e-4`); with `fire` and a non-zero move the notifies between the
//!   old and the new position are issued.
//! - `IssueNotifies(delta)` (positive moves): the first notify at or ahead of
//!   the current position (a looping node also looks past the end) whose
//!   distance is below `delta` fires, then each following one while the
//!   accumulated gap stays below `delta`; notifies at the same time all
//!   fire; a non-looping node stops at the last notify. A notify exactly at
//!   the new position fires on the next move. Negative moves (reversed
//!   playback) fire nothing here (TENTATIVE: `IssueNegativeRateNotifies` not
//!   read; no shipped track plays backwards).
//! - Matinee's `SetAnimPosition` on a `SkeletalMeshActorMAT` switches the
//!   slot node's sequence first (`SetAnim`, then `SetPosition(p, false)`, so
//!   a switch fires nothing) and then calls `SetPosition(p, fire)`; it does
//!   not make the node play.
//! - On a plain `SkeletalMeshActor` the script event drives the component's
//!   own sequence node instead ([`SequenceNode::actor_set`]; CONFIRMED (src),
//!   the engine class read locally): `SetAnim` when the name differs, the
//!   looping flag, `SetPosition(p, fire)`. `SetAnim` leaves the node's time
//!   alone (CONFIRMED, decompiled), so there a switch with `fire` issues the
//!   new sequence's notifies between the old time and `p`; the node keeps
//!   its rate and keeps playing if it was.

use serde::{Deserialize, Serialize};

/// `SetPosition`'s upper slack past the sequence length (double `1e-4` at
/// 0x1016393A0 in the macOS executable, CONFIRMED).
pub const POSITION_SLACK: f64 = 1.0e-4;

/// What a notify does.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum NotifyKind {
    /// `AnimNotify_Kismet`: every `SeqEvent_AnimNotify` of the owner with
    /// this `NotifyName`.
    Kismet {
        /// `NotifyName`.
        name: String,
    },
    /// `AnimNotify_Sound`: plays the cue at the owner (or its bone).
    Sound {
        /// `SoundCue`.
        cue: String,
        /// `bFollowActor`.
        follow_actor: bool,
        /// `BoneName`.
        bone: Option<String>,
        /// `VolumeMultiplier`.
        volume: f32,
        /// `PitchMultiplier`.
        pitch: f32,
        /// `PercentToPlay` (a random draw below it plays).
        percent_to_play: f32,
        /// `bIgnoreIfActorHidden`.
        ignore_if_hidden: bool,
    },
    /// Any other notify (footsteps, ...): not acted on.
    Other {
        /// Class or object path.
        what: String,
    },
}

/// One notify of a sequence.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct NotifyDef {
    /// `Time`, s.
    pub time: f32,
    /// `Duration`, s (duration notifies' tick/end callbacks are not
    /// modelled; none of the acted-on classes uses them).
    pub duration: f32,
    /// What it does.
    pub kind: NotifyKind,
}

/// A sequence's timing and notifies (`AnimSequence`).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SequenceInfo {
    /// `SequenceLength`, s.
    pub length: f32,
    /// `RateScale`.
    pub rate_scale: f32,
    /// `Notifies`, in stored order (sorted by time in the shipped data).
    pub notifies: Vec<NotifyDef>,
}

/// `x > 0` (false for NaN).
fn positive(x: f32) -> bool {
    x.partial_cmp(&0.0) == Some(std::cmp::Ordering::Greater)
}

/// `IssueNotifies(delta)` from `current` (see the module docs): indices of
/// the notifies fired, in order.
#[must_use]
pub fn issue_notifies(
    notifies: &[NotifyDef],
    current: f32,
    delta: f32,
    looping: bool,
    length: f32,
) -> Vec<usize> {
    let n = notifies.len();
    let mut out = Vec::new();
    if n == 0 || !positive(delta) {
        return out;
    }
    // The nearest notify at or ahead of the current position.
    let mut first: Option<usize> = None;
    let mut best_dist = 3.4e38f32;
    let mut best_time = 3.4e38f32;
    for (i, nd) in notifies.iter().enumerate() {
        let mut d = nd.time - current;
        if d < 0.0 {
            if !looping {
                continue;
            }
            d += length;
        }
        if d < best_dist {
            best_dist = d;
            best_time = nd.time;
            first = Some(i);
        }
    }
    let Some(mut idx) = first else { return out };
    let mut remaining = delta - best_dist;
    if !positive(remaining) {
        return out;
    }
    let mut prev = best_time;
    // Bounded: every pass consumes a notify gap; a zero-length looping
    // sequence would not, so the walk stops after one lap per notify.
    let mut guard = 0usize;
    loop {
        out.push(idx);
        guard += 1;
        if guard > n.saturating_mul(64) {
            break;
        }
        idx = (idx + 1) % n;
        let t = notifies.get(idx).map_or(prev, |x| x.time);
        let mut gap = t - prev;
        if idx == 0 {
            if !looping {
                break;
            }
            gap += length;
        }
        remaining -= gap;
        prev = t;
        if !positive(remaining) {
            break;
        }
    }
    out
}

/// An `AnimNodeSequence`'s state.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SequenceNode {
    /// `AnimSeqName` (`None`: nothing plays).
    pub sequence: Option<String>,
    /// `CurrentTime`, s.
    pub position: f32,
    /// `Rate`.
    pub rate: f32,
    /// `bPlaying`.
    pub playing: bool,
    /// `bLooping`.
    pub looping: bool,
}

impl SequenceNode {
    /// A node as placed (`AnimSeqName`, `CurrentTime`, `Rate`, `bPlaying`,
    /// `bLooping`).
    #[must_use]
    pub fn new(
        sequence: Option<String>,
        position: f32,
        rate: f32,
        playing: bool,
        looping: bool,
    ) -> Self {
        let finite = |v: f32, d: f32| if v.is_finite() { v } else { d };
        SequenceNode {
            sequence,
            position: finite(position, 0.0),
            rate: finite(rate, 1.0),
            playing,
            looping,
        }
    }

    /// `SetAnim`: the node plays `sequence` from now on. Its time stays
    /// where it was (CONFIRMED: the decompiled native swaps the sequence and
    /// clears caches, nothing else).
    pub fn set_anim(&mut self, sequence: &str) {
        let same = self
            .sequence
            .as_deref()
            .is_some_and(|s| s.eq_ignore_ascii_case(sequence));
        if !same {
            self.sequence = Some(sequence.to_owned());
        }
    }

    /// `SkeletalMeshActor.SetAnimPosition` on the actor's own node (see the
    /// module docs): the notifies fired (indices into `info.notifies`, the
    /// data of `sequence`).
    pub fn actor_set(
        &mut self,
        sequence: &str,
        position: f32,
        fire: bool,
        looping: bool,
        info: Option<&SequenceInfo>,
    ) -> Vec<usize> {
        self.set_anim(sequence);
        self.looping = looping;
        self.set_position(position, fire, info)
    }

    /// `SetPosition(p, fire)`: the notifies fired (indices into
    /// `info.notifies`).
    pub fn set_position(&mut self, p: f32, fire: bool, info: Option<&SequenceInfo>) -> Vec<usize> {
        let max = info.map_or(1.0e-4, |i| (f64::from(i.length) + POSITION_SLACK) as f32);
        let clamped = if p < 0.0 || p.is_nan() {
            0.0
        } else if max <= p {
            max
        } else {
            p
        };
        let delta = clamped - self.position;
        let fired = match info {
            Some(i) if fire && delta != 0.0 => {
                issue_notifies(&i.notifies, self.position, delta, self.looping, i.length)
            }
            _ => Vec::new(),
        };
        self.position = clamped;
        fired
    }

    /// Matinee's `SetAnimPosition` on the node (see the module docs).
    pub fn matinee_set(
        &mut self,
        sequence: &str,
        position: f32,
        fire: bool,
        looping: bool,
        info: Option<&SequenceInfo>,
    ) -> Vec<usize> {
        let switched = !self
            .sequence
            .as_deref()
            .is_some_and(|s| s.eq_ignore_ascii_case(sequence));
        if switched {
            self.set_anim(sequence);
            self.set_position(position, false, info);
        }
        self.rate = 1.0;
        self.looping = looping;
        self.set_position(position, fire, info)
    }

    /// `TickAnim(dt)` → `AdvanceBy(Rate · RateScale · dt, dt, fire)`: the
    /// notifies fired.
    pub fn tick(&mut self, dt: f32, fire: bool, info: Option<&SequenceInfo>) -> Vec<usize> {
        let Some(info) = info else {
            return Vec::new();
        };
        if !self.playing || self.sequence.is_none() || !(dt.is_finite() && dt != 0.0) {
            return Vec::new();
        }
        let delta = self.rate * info.rate_scale * dt;
        if !delta.is_finite() || delta == 0.0 {
            return Vec::new();
        }
        let fired = if fire {
            issue_notifies(
                &info.notifies,
                self.position,
                delta,
                self.looping,
                info.length,
            )
        } else {
            Vec::new()
        };
        let len = info.length;
        let p = self.position + delta;
        if p <= len {
            if p < 0.0 {
                self.position = if self.looping && len > 0.0 {
                    p.rem_euclid(len)
                } else {
                    self.playing = false;
                    0.0
                };
            } else {
                self.position = p;
            }
        } else if self.looping && len > 0.0 {
            self.position = p % len;
        } else {
            self.position = len.max(0.0);
            self.playing = false;
        }
        fired
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kismet(t: f32, name: &str) -> NotifyDef {
        NotifyDef {
            time: t,
            duration: 0.0,
            kind: NotifyKind::Kismet { name: name.into() },
        }
    }

    fn info() -> SequenceInfo {
        SequenceInfo {
            length: 10.0,
            rate_scale: 1.0,
            notifies: vec![
                kismet(0.0, "A"),
                kismet(4.3, "B1"),
                kismet(4.3, "B2"),
                kismet(6.4, "C"),
            ],
        }
    }

    #[test]
    fn notifies_fire_in_the_window_and_wrap_when_looping() {
        let i = info();
        // [1, 4.3): nothing; [4.3, ...) the two at 4.3.
        assert!(issue_notifies(&i.notifies, 1.0, 3.3, true, 10.0).is_empty());
        assert_eq!(
            issue_notifies(&i.notifies, 1.0, 3.31, true, 10.0),
            vec![1, 2]
        );
        // A notify at the current position fires.
        assert_eq!(
            issue_notifies(&i.notifies, 4.3, 0.1, true, 10.0),
            vec![1, 2]
        );
        // Wrapping: 9 → 11 passes the end and the notify at 0.
        assert_eq!(issue_notifies(&i.notifies, 9.0, 2.0, true, 10.0), vec![0]);
        // Not looping: past the last notify nothing more.
        assert!(issue_notifies(&i.notifies, 7.0, 5.0, false, 10.0).is_empty());
        // A big move fires everything once per lap passed.
        assert_eq!(
            issue_notifies(&i.notifies, 0.5, 15.0, true, 10.0),
            vec![1, 2, 3, 0, 1, 2]
        );
        assert!(issue_notifies(&i.notifies, 1.0, -1.0, true, 10.0).is_empty());
        assert!(issue_notifies(&[], 1.0, 1.0, true, 10.0).is_empty());
        assert!(issue_notifies(&i.notifies, 1.0, f32::NAN, true, 10.0).is_empty());
    }

    #[test]
    fn ticking_wraps_or_stops_and_fires_each_lap() {
        let i = info();
        let mut n = SequenceNode::new(Some("Talk".into()), 1.0, 1.0, true, true);
        let mut fired = Vec::new();
        for _ in 0..600 {
            fired.extend(n.tick(1.0 / 60.0, true, Some(&i)));
        }
        // 10 s from 1.0: B1, B2, C, A once each (9 s → wraps once).
        assert_eq!(fired, vec![1, 2, 3, 0]);
        assert!((n.position - 1.0).abs() < 1e-3, "{}", n.position);
        let mut once = SequenceNode::new(Some("Talk".into()), 9.5, 1.0, true, false);
        once.tick(1.0, true, Some(&i));
        assert_eq!(once.position, 10.0);
        assert!(!once.playing);
        assert!(once.tick(1.0, true, Some(&i)).is_empty());
        // Unknown sequence data: nothing moves.
        let mut unknown = SequenceNode::new(Some("X".into()), 0.0, 1.0, true, true);
        assert!(unknown.tick(1.0, true, None).is_empty());
        assert_eq!(unknown.position, 0.0);
    }

    #[test]
    fn matinee_positions_fire_forwards_and_switch_silently() {
        let i = info();
        let mut n = SequenceNode::default();
        assert!(n.matinee_set("Talk", 4.0, true, false, Some(&i)).is_empty());
        assert_eq!(
            n.matinee_set("Talk", 5.0, true, false, Some(&i)),
            vec![1, 2]
        );
        // No fire flag: silent.
        assert!(
            n.matinee_set("Talk", 7.0, false, false, Some(&i))
                .is_empty()
        );
        // Clamped to the length + 1e-4.
        n.matinee_set("Talk", 99.0, true, false, Some(&i));
        assert!((n.position - 10.0001).abs() < 1e-5);
        // A switch to another sequence fires nothing.
        assert!(n.matinee_set("Other", 6.5, true, true, Some(&i)).is_empty());
        assert_eq!(n.sequence.as_deref(), Some("Other"));
        assert!(!n.playing);
    }

    #[test]
    fn a_plain_actors_own_node_is_driven_in_place() {
        let i = info();
        // The actor's node is playing its ambient loop at 5.0.
        let mut n = SequenceNode::new(Some("Talk".into()), 5.0, 2.0, true, true);
        // Same sequence: the notifies between the node's time and the new
        // position fire; rate and playing are untouched, looping is set.
        assert_eq!(n.actor_set("talk", 7.0, true, false, Some(&i)), vec![3]);
        assert_eq!(
            (n.position, n.rate, n.playing, n.looping),
            (7.0, 2.0, true, false)
        );
        // Another sequence: `SetAnim` keeps the time, so the new sequence's
        // notifies between it and the new position fire too.
        assert_eq!(
            n.actor_set("Other", 4.5, true, true, Some(&i)),
            Vec::<usize>::new()
        );
        assert_eq!(n.sequence.as_deref(), Some("Other"));
        assert_eq!(n.actor_set("Talk", 6.5, true, true, Some(&i)), vec![3]);
        // Without the fire flag nothing fires; backwards moves fire nothing.
        assert!(n.actor_set("Talk", 9.0, false, true, Some(&i)).is_empty());
        assert!(n.actor_set("Talk", 1.0, true, true, Some(&i)).is_empty());
        // It keeps ticking from where Matinee put it.
        assert_eq!(n.tick(0.5, true, Some(&i)), Vec::<usize>::new());
        assert_eq!(n.position, 2.0);
        // An unknown sequence clamps to the no-sequence limit.
        n.actor_set("Missing", 3.0, true, false, None);
        assert!((n.position - 1.0e-4).abs() < 1.0e-9);
    }
}
