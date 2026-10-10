//! In-memory save states.
//!
//! A [`SimSnapshot`] is a clone of the whole simulation: the game (level
//! objects, player, clock, random state, death timer, NPCs) and the level
//! script. Restoring assigns the clones back in place. What is **not**
//! captured: the app's presentation state on scripted levels (audio, VFX,
//! Kismet-driven UI), which is why such slots are labelled "simulation
//! only"; and nothing is written to disk.
//!
//! - [`Slots`]: [`SLOT_COUNT`] manual slots, on any level.
//! - [`RewindRing`]: a ring of keyframes taken every few ticks, for hand-made
//!   levels (the clone cost on converted maps is unmeasured).
//!
//! Two things are deliberately not part of a save state:
//!
//! - a **recording**: the clone's recording is dropped at capture, and a
//!   recording running in the live game is lost by a restore, so a caller
//!   stops it first (the session does, and hands the finished recording to
//!   the host);
//! - the game's **run state** (playing or paused): a restore keeps the
//!   running game's, so loading a state from inside a pause does not unpause.
//!
//! Save states are a Sandbox tool. They are ours and say nothing about the
//! original's save system (`asamu_game::save`), which they never touch.

use std::collections::VecDeque;

use asamu_game::{Game, GameState, LevelScript};

use crate::session::SimCx;

/// Number of manual save-state slots.
pub const SLOT_COUNT: usize = 4;

/// A clone of the simulation at one tick.
#[derive(Clone, Debug)]
pub struct SimSnapshot {
    game: Game,
    script: Option<LevelScript>,
    label: String,
}

impl SimSnapshot {
    /// Clones the game and its script. A recording in progress is not
    /// carried: the clone's recording is dropped.
    #[must_use]
    pub fn capture(game: &Game, script: Option<&LevelScript>, label: impl Into<String>) -> Self {
        let mut game = game.clone();
        // The slot must not hold (or later resume) the live game's trace.
        drop(game.stop_recording());
        Self {
            game,
            script: script.cloned(),
            label: label.into(),
        }
    }

    /// Puts the snapshot back in place of the running simulation. The
    /// session reconciles the parameters afterwards, so the current tuning
    /// survives a load.
    ///
    /// The running game's run state (playing or paused) is kept. A recording
    /// running in the live game is dropped: stop it first.
    ///
    /// # Errors
    /// The snapshot was taken on a different level; nothing changes then.
    pub fn restore(&self, cx: &mut SimCx<'_>) -> Result<(), SnapshotError> {
        let (game, script) = self.instantiate(cx.game)?;
        *cx.game = game;
        *cx.script = script;
        Ok(())
    }

    /// Fresh clones of the snapshot's simulation, ready to replace `live`:
    /// checked against its level and carrying its run state. `live` is not
    /// changed.
    pub(crate) fn instantiate(
        &self,
        live: &Game,
    ) -> Result<(Game, Option<LevelScript>), SnapshotError> {
        let saved = self.game.level();
        let current = live.level();
        if saved.name != current.name || saved.origin != current.origin {
            return Err(SnapshotError::DifferentLevel {
                saved: saved.name.clone(),
                current: current.name.clone(),
            });
        }
        let mut game = self.game.clone();
        carry_run_state(live.state(), &mut game);
        Ok((game, self.script.clone()))
    }

    /// The tick the snapshot was taken at.
    #[must_use]
    pub fn tick(&self) -> u64 {
        self.game.clock().tick()
    }

    /// The label given at capture.
    #[must_use]
    pub fn label(&self) -> &str {
        &self.label
    }

    /// Name of the level the snapshot was taken on.
    #[must_use]
    pub fn level_name(&self) -> &str {
        &self.game.level().name
    }

    /// The snapshot holds a level script, so restoring it on a scripted
    /// level brings back the simulation only (not the app's presentation).
    #[must_use]
    pub fn scripted(&self) -> bool {
        self.script.is_some()
    }
}

/// Makes `game` playing or paused as `state` says. The game's own
/// transitions are used, so nothing else changes.
fn carry_run_state(state: GameState, game: &mut Game) {
    match state {
        GameState::Playing => {
            game.start();
            game.resume();
        }
        GameState::Paused => {
            game.start();
            game.pause();
        }
        // A game that has not started keeps whatever the snapshot had.
        GameState::Boot => {}
    }
}

/// Why a snapshot could not be used.
///
/// Slot numbers in these errors are the numbers shown to the user: they
/// count from 1 (slot index 0 is "slot 1").
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum SnapshotError {
    /// The snapshot belongs to another level.
    #[error("the save state was taken on {saved:?}, not on {current:?}")]
    DifferentLevel {
        /// Level of the snapshot.
        saved: String,
        /// Level that is running.
        current: String,
    },
    /// The slot is empty (the number counts from 1).
    #[error("slot {0} is empty")]
    EmptySlot(usize),
    /// No such slot (the number counts from 1).
    #[error("there is no slot {0} (slots 1 to {SLOT_COUNT})")]
    NoSuchSlot(usize),
}

/// The manual save-state slots.
#[derive(Clone, Debug, Default)]
pub struct Slots {
    slots: [Option<SimSnapshot>; SLOT_COUNT],
    selected: usize,
}

impl Slots {
    /// Index of the selected slot (0-based).
    #[must_use]
    pub fn selected(&self) -> usize {
        self.selected
    }

    /// The snapshot in `slot`, if the slot exists and is filled.
    #[must_use]
    pub fn get(&self, slot: usize) -> Option<&SimSnapshot> {
        self.slots.get(slot).and_then(Option::as_ref)
    }

    /// Every slot in order (`None`: empty).
    pub fn iter(&self) -> impl Iterator<Item = Option<&SimSnapshot>> {
        self.slots.iter().map(Option::as_ref)
    }

    /// The snapshot in `slot`, or why there is none.
    pub(crate) fn filled(&self, slot: usize) -> Result<&SimSnapshot, SnapshotError> {
        self.slots
            .get(slot)
            .ok_or(SnapshotError::NoSuchSlot(slot.saturating_add(1)))?
            .as_ref()
            .ok_or(SnapshotError::EmptySlot(slot.saturating_add(1)))
    }

    /// Makes `slot` the selected one.
    pub(crate) fn select(&mut self, slot: usize) -> Result<(), SnapshotError> {
        self.entry(slot)?;
        self.selected = slot;
        Ok(())
    }

    /// Stores `snapshot` in `slot` (replacing what was there) and selects it.
    pub(crate) fn save(&mut self, slot: usize, snapshot: SimSnapshot) -> Result<(), SnapshotError> {
        *self.entry(slot)? = Some(snapshot);
        self.selected = slot;
        Ok(())
    }

    /// Empties `slot`; `true` if it held a snapshot.
    pub(crate) fn clear(&mut self, slot: usize) -> Result<bool, SnapshotError> {
        Ok(self.entry(slot)?.take().is_some())
    }

    fn entry(&mut self, slot: usize) -> Result<&mut Option<SimSnapshot>, SnapshotError> {
        self.slots
            .get_mut(slot)
            .ok_or(SnapshotError::NoSuchSlot(slot.saturating_add(1)))
    }
}

/// A ring of keyframes taken every `interval_ticks` ticks.
///
/// [`RewindRing::observe`] is fed the simulation after each tick and keeps a
/// keyframe when one is due; the oldest is dropped when the ring is full.
/// Keyframes are whole [`SimSnapshot`]s, so going back to one is exact, but
/// only keyframe ticks can be reached (there is no per-tick rewind).
#[derive(Clone, Debug)]
pub struct RewindRing {
    interval_ticks: u32,
    capacity: usize,
    frames: VecDeque<SimSnapshot>,
}

impl RewindRing {
    /// A ring keeping at most `capacity` keyframes, one every
    /// `interval_ticks` ticks (v1: 30 ticks, 120 frames; ours). An interval
    /// of 0 is taken as 1; a capacity of 0 keeps nothing.
    #[must_use]
    pub fn new(interval_ticks: u32, capacity: usize) -> Self {
        Self {
            interval_ticks: interval_ticks.max(1),
            capacity,
            frames: VecDeque::new(),
        }
    }

    /// Ticks between two keyframes.
    #[must_use]
    pub fn interval_ticks(&self) -> u32 {
        self.interval_ticks
    }

    /// Takes a keyframe when one is due: the ring is empty, or the game is
    /// `interval_ticks` or more past the latest keyframe.
    ///
    /// Keyframes that no longer belong to the game's past are dropped first:
    /// all of them when the level changed, and those at or after the game's
    /// tick when time went back (a save state was loaded).
    pub fn observe(&mut self, game: &Game, script: Option<&LevelScript>) {
        if self.capacity == 0 {
            return;
        }
        let level = game.level();
        if self
            .frames
            .back()
            .is_some_and(|f| f.level_name() != level.name || f.game.level().origin != level.origin)
        {
            self.frames.clear();
        }
        let tick = game.clock().tick();
        while self.frames.back().is_some_and(|f| f.tick() >= tick) {
            self.frames.pop_back();
        }
        let due = self
            .frames
            .back()
            .is_none_or(|f| tick.saturating_sub(f.tick()) >= u64::from(self.interval_ticks));
        if !due {
            return;
        }
        while self.frames.len() >= self.capacity {
            self.frames.pop_front();
        }
        self.frames
            .push_back(SimSnapshot::capture(game, script, format!("tick {tick}")));
    }

    /// Removes and returns the latest keyframe.
    pub fn step_back(&mut self) -> Option<SimSnapshot> {
        self.frames.pop_back()
    }

    /// The keyframe to go back to from tick `now`, for a "rewind" key: the
    /// latest keyframe that is more than half an interval before `now`.
    /// Later keyframes are dropped, so pressing the key again shortly after
    /// a rewind goes one keyframe further back. The returned keyframe stays
    /// in the ring, and the oldest keyframe is never dropped: repeated
    /// rewinds end at the start of the ring.
    pub fn rewind_from(&mut self, now: u64) -> Option<&SimSnapshot> {
        let grace = u64::from(self.interval_ticks / 2);
        while self.frames.len() > 1
            && self
                .frames
                .back()
                .is_some_and(|f| f.tick().saturating_add(grace) >= now)
        {
            self.frames.pop_back();
        }
        self.frames.back()
    }

    /// Drops every keyframe.
    pub fn clear(&mut self) {
        self.frames.clear();
    }

    /// Number of keyframes held.
    #[must_use]
    pub fn len(&self) -> usize {
        self.frames.len()
    }

    /// No keyframe held.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.frames.is_empty()
    }

    /// Ticks between the oldest and the latest keyframe (how far back a
    /// rewind can reach from the latest keyframe).
    #[must_use]
    pub fn span_ticks(&self) -> u64 {
        match (self.frames.front(), self.frames.back()) {
            (Some(first), Some(last)) => last.tick().saturating_sub(first.tick()),
            _ => 0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use asamu_player::InputFrame;

    fn forward() -> InputFrame {
        InputFrame {
            move_forward: 1.0,
            ..InputFrame::default()
        }
    }

    fn running_game() -> Game {
        let mut game = Game::graybox().unwrap();
        game.start();
        game
    }

    #[test]
    fn slot_numbers_in_errors_count_from_one() {
        let mut slots = Slots::default();
        assert_eq!(slots.filled(0).err(), Some(SnapshotError::EmptySlot(1)));
        assert_eq!(
            slots.filled(SLOT_COUNT).err(),
            Some(SnapshotError::NoSuchSlot(SLOT_COUNT + 1))
        );
        assert_eq!(
            slots.select(SLOT_COUNT),
            Err(SnapshotError::NoSuchSlot(SLOT_COUNT + 1))
        );
        assert_eq!(
            slots.clear(usize::MAX),
            Err(SnapshotError::NoSuchSlot(usize::MAX))
        );
        assert_eq!(slots.selected(), 0);
        assert_eq!(slots.iter().count(), SLOT_COUNT);
        assert!(slots.iter().all(|s| s.is_none()));
    }

    #[test]
    fn saving_selects_the_slot_and_clearing_empties_it() {
        let game = running_game();
        let mut slots = Slots::default();
        slots
            .save(2, SimSnapshot::capture(&game, None, "a"))
            .unwrap();
        assert_eq!(slots.selected(), 2);
        assert_eq!(slots.get(2).map(SimSnapshot::label), Some("a"));
        assert_eq!(slots.filled(2).map(SimSnapshot::tick), Ok(0));
        assert_eq!(slots.clear(2), Ok(true));
        assert_eq!(slots.clear(2), Ok(false));
        assert!(slots.get(2).is_none());
    }

    #[test]
    fn a_restore_keeps_the_live_run_state() {
        let mut game = running_game();
        let snapshot = SimSnapshot::capture(&game, None, "playing");
        game.tick(&forward());
        game.pause();
        let mut script = None;
        let mut cx = SimCx {
            game: &mut game,
            script: &mut script,
        };
        snapshot.restore(&mut cx).unwrap();
        assert_eq!(game.state(), GameState::Paused);
        assert_eq!(game.clock().tick(), 0);
    }

    #[test]
    fn the_ring_keeps_one_keyframe_per_interval_and_drops_the_oldest() {
        let mut game = running_game();
        let mut ring = RewindRing::new(10, 3);
        assert!(ring.is_empty());
        for _ in 0..45 {
            game.tick(&forward());
            ring.observe(&game, None);
        }
        // Keyframes at ticks 1, 11, 21, 31, 41; three are kept.
        assert_eq!(ring.len(), 3);
        assert_eq!(ring.span_ticks(), 20);
        assert_eq!(ring.step_back().map(|f| f.tick()), Some(41));
        assert_eq!(ring.step_back().map(|f| f.tick()), Some(31));
        assert_eq!(ring.step_back().map(|f| f.tick()), Some(21));
        assert!(ring.step_back().is_none());
        assert_eq!(ring.span_ticks(), 0);
    }

    #[test]
    fn rewinding_again_soon_goes_one_keyframe_further_back() {
        let mut game = running_game();
        let mut ring = RewindRing::new(10, 16);
        for _ in 0..45 {
            game.tick(&forward());
            ring.observe(&game, None);
        }
        // From tick 45 the latest keyframe (41) is too close; 31 is not.
        assert_eq!(ring.rewind_from(45).map(SimSnapshot::tick), Some(31));
        // Right after going back to 31, the next press reaches 21, then 11.
        assert_eq!(ring.rewind_from(31).map(SimSnapshot::tick), Some(21));
        assert_eq!(ring.rewind_from(23).map(SimSnapshot::tick), Some(11));
        // Having played on for a while, the same keyframe again.
        assert_eq!(ring.rewind_from(19).map(SimSnapshot::tick), Some(11));
        // The oldest keyframe is the end of the line.
        assert_eq!(ring.rewind_from(11).map(SimSnapshot::tick), Some(1));
        assert_eq!(ring.rewind_from(1).map(SimSnapshot::tick), Some(1));
        assert_eq!(ring.len(), 1);
    }

    #[test]
    fn keyframes_from_an_abandoned_future_are_dropped() {
        let mut game = running_game();
        let mut ring = RewindRing::new(10, 16);
        let early = SimSnapshot::capture(&game, None, "start");
        for _ in 0..35 {
            game.tick(&forward());
            ring.observe(&game, None);
        }
        assert_eq!(ring.len(), 4);
        // Back to tick 0 (a loaded save state), then one tick.
        let mut script = None;
        early
            .restore(&mut SimCx {
                game: &mut game,
                script: &mut script,
            })
            .unwrap();
        game.tick(&forward());
        ring.observe(&game, None);
        // Only the new tick-1 keyframe is left.
        assert_eq!(ring.len(), 1);
        assert_eq!(ring.step_back().map(|f| f.tick()), Some(1));
    }

    #[test]
    fn a_ring_without_capacity_keeps_nothing() {
        let mut game = running_game();
        let mut ring = RewindRing::new(0, 0);
        assert_eq!(ring.interval_ticks(), 1);
        game.tick(&forward());
        ring.observe(&game, None);
        assert!(ring.is_empty());
        assert!(ring.rewind_from(100).is_none());
        ring.clear();
        assert_eq!(ring.len(), 0);
    }
}
