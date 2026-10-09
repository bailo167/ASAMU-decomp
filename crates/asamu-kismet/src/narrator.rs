//! The narrator manager (`ASAMUNarratorManager`), as `SeqAct_NarratorLine`
//! drives it. Behaviour described in our own words from local reading of
//! the script (no script text reproduced); timers follow the native
//! `AActor::SetTimer` / `UpdateTimers` (CONFIRMED from the decompilation):
//! a timer set with rate 0 is removed without firing, counts grow by `dt`
//! and a timer fires when its count exceeds its rate.
//!
//! - Adding a line appends it to a queue. When it is the only line, the
//!   narrator events' "started" output fires, the line starts playing and a
//!   timer of the cue's `Duration` is set.
//! - When that timer fires, the line's action fires its `FinishedLine`
//!   output and the line leaves the queue (stopping its sound). If lines
//!   remain, the next one starts after its own `Delay` (a second timer);
//!   otherwise the narrator events' "finished" output fires.
//! - Removing a line drops it from the queue; removing the line that plays
//!   (with the stop flag) stops its sound and cancels a pending delayed
//!   start, but not the duration timer (so that timer still fires later for
//!   whatever line is first by then; when the queue is empty, it only fires
//!   the "finished" narrator events).
//!
//! Consequences kept on purpose: a queued line with `Delay` 0 never starts
//! (its start timer is removed unfired), so the queue stalls; and a cue
//! without a known duration (0) never finishes.

/// Most queued lines (ours: the script's array is unbounded; a line whose
/// cue has no duration never leaves the queue, so a graph that keeps adding
/// lines would grow it forever). Further lines are dropped with an error.
pub const MAX_LINES: usize = 1024;

/// What the narrator asks the interpreter to do.
#[derive(Debug, Clone, PartialEq)]
pub enum Call {
    /// Start playing a line.
    Play {
        /// `SeqAct_NarratorLine` node.
        node: usize,
        /// Line id.
        id: String,
        /// Cue path.
        cue: Option<String>,
        /// Volume.
        volume: f32,
    },
    /// Stop the playing line's sound.
    Stop {
        /// Line id.
        id: String,
    },
    /// The line's `FinishedLine` output fires (`ActivateFinishedOutput`).
    Finished {
        /// Node.
        node: usize,
    },
    /// Narrator events: started narrating.
    Started,
    /// Narrator events: finished narrating.
    AllFinished,
}

/// One queued line.
#[derive(Debug, Clone, PartialEq)]
pub struct Line {
    /// Id.
    pub id: String,
    /// Cue path.
    pub cue: Option<String>,
    /// Cue `Duration`.
    pub duration: f32,
    /// Volume.
    pub volume: f32,
    /// Start delay when it follows another line.
    pub delay: f32,
    /// The action that added it.
    pub node: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TimerKind {
    Finished,
    DelayedStart,
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct Timer {
    kind: TimerKind,
    rate: f32,
    count: f32,
    paused: bool,
}

/// Narrator state.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Narrator {
    /// Queue (first = playing or about to play).
    pub lines: Vec<Line>,
    timers: Vec<Timer>,
}

impl Narrator {
    /// `SetTimer(rate)`: an existing timer is reset (count 0, unpaused); a
    /// new one is appended. Rate 0 marks it for removal.
    fn set_timer(&mut self, kind: TimerKind, rate: f32) {
        if let Some(t) = self.timers.iter_mut().find(|t| t.kind == kind) {
            t.rate = rate;
            t.count = 0.0;
            t.paused = false;
            return;
        }
        self.timers.push(Timer {
            kind,
            rate,
            count: 0.0,
            paused: false,
        });
    }

    fn clear_timer(&mut self, kind: TimerKind) {
        self.set_timer(kind, 0.0);
    }

    /// `AddLine`.
    #[allow(clippy::too_many_arguments)]
    pub fn add_line(
        &mut self,
        node: usize,
        id: &str,
        cue: Option<String>,
        duration: f32,
        delay: f32,
        volume: f32,
    ) -> Vec<Call> {
        let mut calls = Vec::new();
        self.lines.push(Line {
            id: id.to_owned(),
            cue: cue.clone(),
            duration,
            volume,
            delay,
            node,
        });
        if self.lines.len() == 1 {
            calls.push(Call::Started);
            calls.push(Call::Play {
                node,
                id: id.to_owned(),
                cue,
                volume,
            });
            self.set_timer(TimerKind::Finished, duration);
        }
        calls
    }

    /// `RemoveLine(id, stop_if_active)`.
    pub fn remove_line(&mut self, id: &str, stop_if_active: bool) -> Vec<Call> {
        let mut calls = Vec::new();
        let Some(i) = self.lines.iter().position(|l| l.id == id) else {
            return calls;
        };
        if stop_if_active && i == 0 {
            self.clear_timer(TimerKind::DelayedStart);
            calls.push(Call::Stop { id: id.to_owned() });
        }
        self.lines.remove(i);
        calls
    }

    fn finished_line(&mut self) -> Vec<Call> {
        let mut calls = Vec::new();
        self.clear_timer(TimerKind::Finished);
        if let Some(first) = self.lines.first() {
            calls.push(Call::Finished { node: first.node });
            let id = first.id.clone();
            calls.extend(self.remove_line(&id, true));
        }
        if let Some(next) = self.lines.first() {
            let d = next.delay;
            self.set_timer(TimerKind::DelayedStart, d);
        } else {
            calls.push(Call::AllFinished);
        }
        calls
    }

    fn delayed_start(&mut self) -> Vec<Call> {
        let mut calls = Vec::new();
        self.clear_timer(TimerKind::DelayedStart);
        if let Some(first) = self.lines.first() {
            calls.push(Call::Play {
                node: first.node,
                id: first.id.clone(),
                cue: first.cue.clone(),
                volume: first.volume,
            });
            let d = first.duration;
            self.set_timer(TimerKind::Finished, d);
        }
        calls
    }

    /// `UpdateTimers(dt)`: counts grow, zero-rate timers are removed, due
    /// timers fire in order (non-looping: removed when fired).
    pub fn tick(&mut self, dt: f32) -> Vec<Call> {
        for t in &mut self.timers {
            if !t.paused {
                t.count += dt;
            }
        }
        let mut calls = Vec::new();
        let mut i = 0;
        while i < self.timers.len() {
            let Some(t) = self.timers.get(i).copied() else {
                break;
            };
            if t.rate == 0.0 {
                self.timers.remove(i);
                continue;
            }
            if !t.paused && t.count > t.rate {
                self.timers.remove(i);
                let more = match t.kind {
                    TimerKind::Finished => self.finished_line(),
                    TimerKind::DelayedStart => self.delayed_start(),
                };
                calls.extend(more);
                continue;
            }
            i += 1;
        }
        calls
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lines_play_in_order_with_delays() {
        let mut n = Narrator::default();
        let c = n.add_line(1, "a", None, 1.0, 0.0, 1.0);
        assert_eq!(c.first(), Some(&Call::Started));
        assert!(matches!(c.get(1), Some(Call::Play { node: 1, .. })));
        assert!(n.add_line(2, "b", None, 0.5, 0.25, 1.0).is_empty());
        let mut all = Vec::new();
        for _ in 0..200 {
            all.extend(n.tick(0.01));
        }
        let finished: Vec<usize> = all
            .iter()
            .filter_map(|c| match c {
                Call::Finished { node } => Some(*node),
                _ => None,
            })
            .collect();
        assert_eq!(finished, vec![1, 2]);
        assert!(all.iter().any(|c| matches!(c, Call::Play { node: 2, .. })));
        assert_eq!(all.last(), Some(&Call::AllFinished));
        assert!(n.lines.is_empty());
    }

    #[test]
    fn zero_delay_queued_line_stalls_and_zero_duration_never_finishes() {
        let mut n = Narrator::default();
        n.add_line(1, "a", None, 0.1, 0.0, 1.0);
        n.add_line(2, "b", None, 0.1, 0.0, 1.0);
        let mut all = Vec::new();
        for _ in 0..100 {
            all.extend(n.tick(0.01));
        }
        assert!(all.contains(&Call::Finished { node: 1 }));
        assert!(!all.iter().any(|c| matches!(c, Call::Play { node: 2, .. })));
        assert_eq!(n.lines.len(), 1, "line b stays queued");
        let mut m = Narrator::default();
        m.add_line(3, "c", None, 0.0, 0.0, 1.0);
        let calls: Vec<Call> = (0..50).flat_map(|_| m.tick(0.02)).collect();
        assert!(calls.is_empty());
    }

    #[test]
    fn removing_the_playing_line_keeps_its_duration_timer() {
        let mut n = Narrator::default();
        n.add_line(1, "a", None, 0.2, 0.0, 1.0);
        n.add_line(2, "b", None, 0.2, 0.5, 1.0);
        let c = n.remove_line("a", true);
        assert_eq!(c, vec![Call::Stop { id: "a".into() }]);
        // The duration timer of "a" fires for "b" (now first).
        let calls: Vec<Call> = (0..30).flat_map(|_| n.tick(0.01)).collect();
        assert!(calls.contains(&Call::Finished { node: 2 }));
    }
}
