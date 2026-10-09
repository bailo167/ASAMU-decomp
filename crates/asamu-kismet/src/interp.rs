//! `SeqAct_Interp` (Matinee) in the interpreter: activation, inputs,
//! stepping and track updates (MATINEE.md "SeqAct_Interp"; the input order
//! and the one-frame-late deactivation follow the decompiled
//! `USeqAct_Interp::Activated`, `UpdateOp` and `DeActivated`).
//!
//! - Activation (only when not playing, and only by Play, Reverse or Change
//!   Dir) builds the group instances at the current position: one per bound
//!   actor of each group (an unbound instance when none), with the move
//!   tracks' initial transforms taken from the actors' current transforms.
//!   Then it plays forwards (Play), backwards (Reverse) or flips the
//!   direction (Change Dir).
//! - Every update applies at most one input (Pause while playing, else Play,
//!   Reverse, Stop, Change Dir). Without an input, a stopped action finishes
//!   (so it deactivates one update after it stopped). Otherwise it steps by
//!   `dt · PlayRate` and stays active.
//! - Deactivation fires `Completed` when the position is at the end and
//!   `Reversed` when it is at the start.
//! - Track updates: move tracks place their actor (through the host) and
//!   carry its attached actors; event keys fire the output of the same name
//!   (in any group, the director group included); director cuts, fades,
//!   sound keys and visibility/toggle keys emit outputs.

use crate::graph::ActorRef;
use crate::host::{Host, Output, ToggleMode};
use crate::matinee::{
    InterpData, MatineeAction, MoveInstance, Playback, PlaybackInputs, TrackData, fired_keys,
    remove_scaling, rotation_translation_matrix,
};
use crate::runtime::{Latent, Runtime};

/// Below this the position counts as the start (`Reversed`), within it of
/// the end as the end (`Completed`). CONFIRMED: `USeqAct_Interp::DeActivated`
/// compares in double precision against the doubles at 0x1016393A0 (1.0e-4)
/// and 0x1016457F8 (-1.0e-4) in the macOS executable, read directly.
const END_EPSILON: f64 = 1.0e-4;

/// One group instance.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct GroupInst {
    pub(crate) group: usize,
    pub(crate) actor: Option<ActorRef>,
    pub(crate) moves: Vec<(usize, MoveInstance)>,
}

/// Run-time state of a `SeqAct_Interp`.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct InterpRun {
    pub(crate) playback: Playback,
    pub(crate) groups: Vec<GroupInst>,
    pub(crate) cut: Option<(usize, usize)>,
    pub(crate) fade: Option<f32>,
}

impl Runtime {
    fn interp_data(&self, op: usize) -> Option<(MatineeAction, InterpData)> {
        self.matinee
            .data_of(op)
            .map(|(a, d)| (a.clone(), d.clone()))
    }

    fn interp_inputs(&self, op: usize) -> PlaybackInputs {
        PlaybackInputs {
            play: self.impulse(op, 0),
            reverse: self.impulse(op, 1),
            stop: self.impulse(op, 2),
            pause: self.impulse(op, 3),
            change_dir: self.impulse(op, 4),
        }
    }

    fn take_run(&mut self, op: usize) -> Option<Box<InterpRun>> {
        let st = self.ops.get_mut(op)?;
        match std::mem::take(&mut st.latent) {
            Latent::Interp(r) => Some(r),
            other => {
                st.latent = other;
                None
            }
        }
    }

    fn put_run(&mut self, op: usize, run: Box<InterpRun>) {
        if let Some(st) = self.ops.get_mut(op) {
            st.latent = Latent::Interp(run);
        }
    }

    /// The current transform of an actor: the host's, else the one Matinee
    /// last set, else its placement.
    pub(crate) fn current_transform(
        &self,
        a: ActorRef,
        host: &dyn Host,
    ) -> Option<([f32; 3], [i32; 3])> {
        let info = self.graph.actor(a)?;
        host.actor_transform(info)
            .or_else(|| self.actor_xf.get(&a).copied())
            .or(Some((info.location, info.rotation)))
    }

    /// `inst` with its base matrix replaced by the base actor's current
    /// transform (unchanged for an unattached actor or an unknown base).
    fn with_current_base(&self, a: ActorRef, inst: MoveInstance, host: &dyn Host) -> MoveInstance {
        if inst.base.is_none() {
            return inst;
        }
        match self
            .graph
            .actor(a)
            .and_then(|x| x.base)
            .and_then(|b| self.current_transform(b, host))
        {
            Some((bl, br)) => MoveInstance {
                base: Some(remove_scaling(&rotation_translation_matrix(br, bl))),
                ..inst
            },
            None => inst,
        }
    }

    /// `InitInterp` at the current position.
    fn init_interp(
        &mut self,
        run: &mut InterpRun,
        action: &MatineeAction,
        data: &InterpData,
        host: &dyn Host,
    ) {
        run.groups.clear();
        let position = run.playback.position;
        for (gi, g) in data.groups.iter().enumerate() {
            // Every group but a folder gets an instance, the director group
            // included: its event and sound tracks fire like any group's (11
            // event and 3 sound tracks of the shipped Matinees live in
            // director groups); its cuts and fades are handled below.
            if g.folder {
                continue;
            }
            let mut actors: Vec<ActorRef> = Vec::new();
            for b in &action.bindings {
                let matches = b.label.eq_ignore_ascii_case(&g.name)
                    || b.group
                        .as_deref()
                        .is_some_and(|n| n.eq_ignore_ascii_case(&g.name));
                if !matches {
                    continue;
                }
                for t in &b.targets {
                    if let Some(a) = t
                        .object
                        .as_deref()
                        .and_then(|p| self.graph.actor_by_path(p))
                        && !actors.contains(&a)
                    {
                        actors.push(a);
                    }
                }
            }
            if actors.is_empty() {
                run.groups.push(GroupInst {
                    group: gi,
                    actor: None,
                    moves: Vec::new(),
                });
                continue;
            }
            for a in actors {
                let mut moves = Vec::new();
                if let Some((loc, rot)) = self.current_transform(a, host) {
                    let base = self
                        .graph
                        .actor(a)
                        .and_then(|x| x.base)
                        .and_then(|b| self.current_transform(b, host));
                    for (ti, t) in g.tracks.iter().enumerate() {
                        if t.disabled {
                            continue;
                        }
                        if let TrackData::Move(m) = &t.data {
                            let inst = match base {
                                Some((bl, br)) => m.instance_with_base(
                                    loc,
                                    rot,
                                    &rotation_translation_matrix(br, bl),
                                    position,
                                ),
                                None => m.instance(loc, rot, position),
                            };
                            moves.push((ti, inst));
                        }
                    }
                }
                run.groups.push(GroupInst {
                    group: gi,
                    actor: Some(a),
                    moves,
                });
            }
        }
    }

    /// Re-bases relative move tracks at time 0 (`bNoResetOnRewind`).
    fn rebase_moves(&mut self, run: &mut InterpRun, data: &InterpData, host: &dyn Host) {
        for gi in &mut run.groups {
            let Some(a) = gi.actor else { continue };
            let Some((loc, rot)) = self.current_transform(a, host) else {
                continue;
            };
            let Some(g) = data.groups.get(gi.group) else {
                continue;
            };
            for (ti, inst) in &mut gi.moves {
                if let Some(TrackData::Move(m)) = g.tracks.get(*ti).map(|t| &t.data) {
                    let current = self.with_current_base(a, *inst, host);
                    *inst = match current.base {
                        Some(b) => m.instance_with_base(loc, rot, &b, 0.0),
                        None => m.instance(loc, rot, 0.0),
                    };
                }
            }
        }
    }

    pub(crate) fn interp_activated(&mut self, op: usize, host: &mut dyn Host) {
        let Some((action, data)) = self.interp_data(op) else {
            self.error(format!("SeqAct_Interp {op}: no Matinee data"));
            return;
        };
        let mut run = self.take_run(op).unwrap_or_else(|| {
            Box::new(InterpRun {
                playback: Playback::new(data.length, action.settings.clone()),
                groups: Vec::new(),
                cut: None,
                fade: None,
            })
        });
        if run.playback.playing {
            self.put_run(op, run);
            return;
        }
        let inputs = self.interp_inputs(op);
        if !(inputs.play || inputs.reverse || inputs.change_dir) {
            self.put_run(op, run);
            return;
        }
        self.init_interp(&mut run, &action, &data, host);
        if inputs.play {
            self.interp_play(op, &mut run, &data, host);
        } else if inputs.reverse {
            run.playback.playing = true;
            run.playback.paused = false;
            run.playback.reverse = true;
        } else {
            run.playback.playing = true;
            run.playback.paused = false;
            run.playback.reverse = !run.playback.reverse;
        }
        self.put_run(op, run);
    }

    fn interp_play(
        &mut self,
        op: usize,
        run: &mut InterpRun,
        data: &InterpData,
        host: &mut dyn Host,
    ) {
        let before = run.playback.position;
        if let Some(j) = run.playback.play() {
            if j.reset_initial_transforms {
                self.rebase_moves(run, data, host);
            }
            run.playback.position = before;
            self.update_interp(op, run, data, j.to, true, host);
        }
    }

    pub(crate) fn interp_update(&mut self, op: usize, dt: f32, host: &mut dyn Host) -> bool {
        let Some((_, data)) = self.interp_data(op) else {
            return true;
        };
        let Some(mut run) = self.take_run(op) else {
            return true;
        };
        let inputs = self.interp_inputs(op);
        if run.playback.playing && inputs.pause {
            run.playback.paused = !run.playback.paused;
        } else if inputs.play {
            self.interp_play(op, &mut run, &data, host);
        } else if inputs.reverse {
            run.playback.playing = true;
            run.playback.paused = false;
            run.playback.reverse = true;
        } else if inputs.stop {
            run.playback.stop();
        } else if inputs.change_dir {
            run.playback.playing = true;
            run.playback.paused = false;
            run.playback.reverse = !run.playback.reverse;
        } else if !run.playback.playing {
            self.put_run(op, run);
            return true;
        }
        if let Some(o) = self.ops.get_mut(op) {
            o.in_impulse.fill(false);
        }
        self.step_interp(op, &mut run, &data, dt, host);
        self.put_run(op, run);
        false
    }

    fn step_interp(
        &mut self,
        op: usize,
        run: &mut InterpRun,
        data: &InterpData,
        dt: f32,
        host: &mut dyn Host,
    ) {
        if !run.playback.playing || run.playback.paused {
            return;
        }
        let reverse = run.playback.reverse;
        let len = run.playback.length;
        let step = run.playback.step(dt);
        if step.wrapped {
            // Update to the end passed, jump to the other end, continue.
            let (end, other) = if reverse { (0.0, len) } else { (len, 0.0) };
            // The step already moved the position; replay the sequence.
            run.playback.position = step.from;
            let playing = run.playback.playing;
            run.playback.playing = true;
            self.update_interp(op, run, data, end, false, host);
            if step.reset_initial_transforms {
                self.rebase_moves(run, data, host);
            }
            self.update_interp(op, run, data, other, true, host);
            self.update_interp(op, run, data, step.to, false, host);
            run.playback.playing = playing;
        } else {
            let playing = run.playback.playing;
            run.playback.position = step.from;
            run.playback.playing = true;
            self.update_interp(op, run, data, step.to, false, host);
            run.playback.playing = playing;
        }
    }

    /// `UpdateInterp`: moves every track to `new_pos` (from the current
    /// position) and sets the position.
    fn update_interp(
        &mut self,
        op: usize,
        run: &mut InterpRun,
        data: &InterpData,
        new_pos: f32,
        jump: bool,
        host: &mut dyn Host,
    ) {
        let last = run.playback.position;
        let len = run.playback.length;
        let playing_reverse = run.playback.playing && run.playback.reverse;
        let groups = run.groups.clone();
        for gi in &groups {
            let Some(g) = data.groups.get(gi.group) else {
                continue;
            };
            let actor_path = gi
                .actor
                .and_then(|a| self.graph.actor(a))
                .map(|x| x.path.clone());
            for (ti, t) in g.tracks.iter().enumerate() {
                if t.disabled {
                    continue;
                }
                match &t.data {
                    TrackData::Move(m) => {
                        let (Some(a), Some((_, inst))) =
                            (gi.actor, gi.moves.iter().find(|(i, _)| *i == ti))
                        else {
                            continue;
                        };
                        // `GetMoveRefFrame` reads the base's matrix on every
                        // evaluation (decompiled), so an attached actor
                        // follows its base as the base moves.
                        let inst = self.with_current_base(a, *inst, host);
                        if let Some((loc, rot)) = m.sample(new_pos, &inst) {
                            self.place_actor(a, loc, rot, host);
                        }
                    }
                    TrackData::Event(e) => {
                        let times: Vec<f32> = e.keys.iter().map(|k| k.time).collect();
                        for k in fired_keys(
                            &times,
                            last,
                            new_pos,
                            len,
                            playing_reverse,
                            jump,
                            e.fire_forwards,
                            e.fire_backwards,
                            e.fire_jumping_forwards,
                        ) {
                            let Some(name) = e.keys.get(k).map(|x| x.name.clone()) else {
                                continue;
                            };
                            if let Some(i) = self.graph.node(op).and_then(|n| n.output_named(&name))
                            {
                                self.activate_output(op, i);
                            }
                        }
                    }
                    TrackData::Sound(s) => {
                        let times: Vec<f32> = s.keys.iter().map(|k| k.time).collect();
                        let fire = fired_keys(
                            &times,
                            last,
                            new_pos,
                            len,
                            playing_reverse,
                            jump,
                            true,
                            s.play_on_reverse,
                            false,
                        );
                        for k in fire {
                            if let Some(key) = s.keys.get(k) {
                                self.emit(Output::MatineeSound {
                                    node: op,
                                    cue: key.sound.clone(),
                                    actor: actor_path.clone(),
                                    volume: key.volume,
                                    pitch: key.pitch,
                                });
                            }
                        }
                    }
                    TrackData::Visibility(v) | TrackData::Toggle(v) => {
                        let times: Vec<f32> = v.keys.iter().map(|k| k.time).collect();
                        let fire = fired_keys(
                            &times,
                            last,
                            new_pos,
                            len,
                            playing_reverse,
                            jump,
                            v.fire_forwards,
                            v.fire_backwards,
                            false,
                        );
                        let is_vis = matches!(t.data, TrackData::Visibility(_));
                        for k in fire {
                            let Some(key) = v.keys.get(k) else { continue };
                            if is_vis
                                && let Some(info) =
                                    gi.actor.and_then(|a| self.graph.actor(a)).cloned()
                            {
                                let mode = match key.action.as_str() {
                                    "EVTA_Hide" => Some(ToggleMode::On),
                                    "EVTA_Show" => Some(ToggleMode::Off),
                                    "EVTA_Toggle" => Some(ToggleMode::Toggle),
                                    _ => None,
                                };
                                if let Some(mode) = mode {
                                    host.set_actor_hidden(&info, mode);
                                }
                            }
                            self.emit(Output::MatineeKey {
                                node: op,
                                actor: actor_path.clone(),
                                action: key.action.clone(),
                            });
                        }
                    }
                    TrackData::Director(_) | TrackData::Fade(_) | TrackData::Other => {}
                }
            }
        }
        // Director group tracks (cuts and fades).
        for g in &data.groups {
            if g.kind != "director" {
                continue;
            }
            for (ti, t) in g.tracks.iter().enumerate() {
                if t.disabled {
                    continue;
                }
                match &t.data {
                    TrackData::Director(d) => {
                        let cut = d.cut_index(new_pos).map(|c| (ti, c));
                        if cut != run.cut {
                            run.cut = cut;
                            let (group, transition) = match cut.and_then(|(_, c)| d.cuts.get(c)) {
                                Some(c) => {
                                    let own = c
                                        .target_group
                                        .as_deref()
                                        .is_some_and(|n| n.eq_ignore_ascii_case(&g.name));
                                    (
                                        if own { None } else { c.target_group.clone() },
                                        c.transition_time,
                                    )
                                }
                                None => (None, 0.0),
                            };
                            self.emit(Output::MatineeCut {
                                node: op,
                                group,
                                transition,
                            });
                        }
                    }
                    TrackData::Fade(f) => {
                        let amount = f.amount_at(new_pos);
                        if run.fade != Some(amount) {
                            run.fade = Some(amount);
                            self.emit(Output::MatineeFade { node: op, amount });
                        }
                    }
                    _ => {}
                }
            }
        }
        run.playback.position = new_pos;
    }

    pub(crate) fn interp_deactivated(&mut self, op: usize) {
        let (pos, len) = match self.ops.get(op).map(|o| &o.latent) {
            Some(Latent::Interp(r)) => (r.playback.position, r.playback.length),
            _ => return,
        };
        let (pos, len) = (f64::from(pos), f64::from(len));
        if END_EPSILON <= pos {
            if len - END_EPSILON < pos {
                self.activate_output(op, 0);
            }
        } else {
            self.activate_output(op, 1);
        }
    }
}
