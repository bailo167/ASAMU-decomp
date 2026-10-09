//! Simulation events that the original reports to Kismet, and the grapple
//! handler calls that world objects react to.
//!
//! The original fires Kismet sequence events from gameplay script
//! (`docs/reverse-engineering/GRAPPLE.md` §14, `ABILITIES.md` §5/§7). The
//! simulation reports them as [`SimEvent`]s in [`EventLog`] so `asamu-game`
//! (and later a Kismet runtime) can react, together with the target
//! handler calls (`Grappled` / `UnGrappled`, G-AT-3 and G-RL-1) that drive
//! world-object state machines such as recharge crystals.
//!
//! [`EventLog`] has a fixed capacity so that [`crate::StepEvents`] stays
//! `Copy`; one tick produces at most a handful of events.

use serde::{Deserialize, Serialize};

/// Events one [`EventLog`] can hold. A tick produces at most an attach, a
/// release, a landing, a boost event and their handler calls, so this is
/// a generous bound (not a gameplay value).
pub const EVENT_CAPACITY: usize = 8;

/// One event, in the order the original would raise it within the tick.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum SimEvent {
    /// Kismet `SeqEvent_PlayerGrappled` (every attach, GRAPPLE.md §14). The
    /// originator is the hit actor for interactable targets (tag
    /// `grappleInteractable` or `ASAMUGrappleAbleInterface`), otherwise the
    /// level's `WorldInfo` (`None`).
    PlayerGrappled {
        /// Actor id of the originator, `None` for the `WorldInfo`.
        originator: Option<u32>,
    },
    /// Kismet `SeqEvent_PlayerReleasedGrapple` (every release, G-RL-1;
    /// originator `WorldInfo`).
    PlayerReleasedGrapple,
    /// Kismet `SeqEvent_PlayerLanded` (a landing with `V.z ≤ −500` of a
    /// visible pawn, ABILITIES.md §5).
    PlayerLanded,
    /// Kismet `SeqEvent_PlayerRocketBoosted`: output 0 when the charge
    /// starts (`boosting == false`), output 1 when the boost starts.
    PlayerRocketBoosted {
        /// Output 1 (boost start) instead of output 0 (charge start).
        boosting: bool,
    },
    /// The story-mode fire hit an `ASAMUInteractable_Actor` within
    /// `interactRange` of the pawn (G-AC-0): its `InteractWith` runs, which
    /// counts the use against `MaxInteractTimes` and then fires Kismet
    /// `SeqEvent_ActorInteractedWith` (the count lives with the world
    /// object, see `asamu-game`).
    InteractWith {
        /// Actor id of the interactable.
        actor: u32,
    },
    /// Handler call (not a Kismet event): the grappled target's `Grappled`
    /// handler ran (`ASAMUGrappleAbleInterface`, G-AT-3).
    ActorGrappled {
        /// Actor id of the target.
        actor: u32,
    },
    /// Handler call (not a Kismet event): the released target's `UnGrappled`
    /// handler ran (G-RL-1).
    ActorUngrappled {
        /// Actor id of the target.
        actor: u32,
    },
}

/// A fixed-capacity, ordered list of [`SimEvent`]s.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventLog {
    items: [Option<SimEvent>; EVENT_CAPACITY],
    len: u8,
    overflowed: bool,
}

impl EventLog {
    /// An empty log.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Appends `event`; when the log is full the event is dropped and
    /// [`Self::overflowed`] is set.
    pub fn push(&mut self, event: SimEvent) {
        match self.items.get_mut(usize::from(self.len)) {
            Some(slot) => {
                *slot = Some(event);
                self.len = self.len.saturating_add(1);
            }
            None => self.overflowed = true,
        }
    }

    /// Appends every event of `other`.
    pub fn extend(&mut self, other: &Self) {
        for e in other.iter() {
            self.push(e);
        }
        if other.overflowed {
            self.overflowed = true;
        }
    }

    /// The events in order.
    pub fn iter(&self) -> impl Iterator<Item = SimEvent> + '_ {
        self.items
            .iter()
            .take(usize::from(self.len))
            .flatten()
            .copied()
    }

    /// Number of events.
    #[must_use]
    pub fn len(&self) -> usize {
        usize::from(self.len)
    }

    /// `true` when no event was recorded.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// `true` if `event` is in the log.
    #[must_use]
    pub fn contains(&self, event: &SimEvent) -> bool {
        self.iter().any(|e| e == *event)
    }

    /// An event was dropped because the log was full.
    #[must_use]
    pub fn overflowed(&self) -> bool {
        self.overflowed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn log_keeps_order_and_reports_overflow() {
        let mut log = EventLog::new();
        assert!(log.is_empty());
        log.push(SimEvent::PlayerGrappled { originator: None });
        log.push(SimEvent::PlayerReleasedGrapple);
        assert_eq!(
            log.iter().collect::<Vec<_>>(),
            vec![
                SimEvent::PlayerGrappled { originator: None },
                SimEvent::PlayerReleasedGrapple
            ]
        );
        assert!(log.contains(&SimEvent::PlayerReleasedGrapple));
        for _ in 0..EVENT_CAPACITY {
            log.push(SimEvent::PlayerLanded);
        }
        assert_eq!(log.len(), EVENT_CAPACITY);
        assert!(log.overflowed());
        let json = serde_json::to_string(&log).unwrap();
        let back: EventLog = serde_json::from_str(&json).unwrap();
        assert_eq!(back, log);
        let mut other = EventLog::new();
        other.extend(&log);
        assert_eq!(other.len(), EVENT_CAPACITY);
        assert!(other.overflowed());
    }
}
