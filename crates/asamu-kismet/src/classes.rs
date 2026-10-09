//! Class behaviour: what each op does when activated, updated and
//! deactivated (KISMET_RUNTIME.md §3 describes every class in our own
//! words with its evidence and confidence).

use crate::graph::ActorRef;
use crate::host::{Host, Output, ToggleMode};
use crate::ops::OpClass;
use crate::runtime::{Latent, Obj, PendingAttractor, Runtime};
use crate::value::KValue;

/// `SMALL_NUMBER` (1.0e-8): the threshold `USeqAct_PlaySound` compares
/// `|ExtraDelay|` with to start the sound at once or after the delay
/// (CONFIRMED: the decompiled `Activated` and `UpdateOp` read the float at
/// 0x10163FED4 in the macOS executable, which holds 1.0e-8, bits 0x322BCC77).
const SMALL_NUMBER: f32 = 1.0e-8;

impl Runtime {
    fn toggle_mode(&self, op: usize) -> Option<ToggleMode> {
        if self.impulse(op, 0) {
            Some(ToggleMode::On)
        } else if self.impulse(op, 1) {
            Some(ToggleMode::Off)
        } else if self.impulse(op, 2) {
            Some(ToggleMode::Toggle)
        } else {
            None
        }
    }

    /// The objects of the op's `Targets` property (filled from its `Target`
    /// link).
    fn targets(&self, op: usize) -> Vec<Obj> {
        let t = self.prop(op, "Targets");
        match t {
            KValue::Array(items) => items.iter().map(|v| self.resolve(v)).collect(),
            KValue::Obj(_) => vec![self.resolve(&t)],
            _ => Vec::new(),
        }
    }

    fn target_actors(&self, op: usize) -> Vec<ActorRef> {
        self.targets(op)
            .into_iter()
            .filter_map(|o| match o {
                Obj::Actor(a) => Some(a),
                _ => None,
            })
            .collect()
    }

    fn has_player_target(&self, op: usize) -> bool {
        self.targets(op).contains(&Obj::Player)
    }

    fn obj_path(&self, v: &KValue) -> Option<String> {
        v.as_obj().map(str::to_owned)
    }

    fn prop_actor(&self, op: usize, name: &str) -> Option<ActorRef> {
        match self.resolve(&self.prop(op, name)) {
            Obj::Actor(a) => Some(a),
            _ => None,
        }
    }

    fn with_actor(&self, a: ActorRef) -> Option<crate::graph::ActorInfo> {
        self.graph.actor(a).cloned()
    }

    /// Script `Activated` of events activated through `ActivateEvent`.
    pub(crate) fn event_script_activated(&mut self, ev: usize) {
        let class = self.graph.node(ev).map_or(OpClass::Unknown, |x| x.class);
        match class {
            OpClass::PlayerGrappled => {
                // Clear output 0; pulse it only when the `inputActor`
                // variable holds the originator.
                if let Some(o) = self.ops.get_mut(ev)
                    && let Some(x) = o.out_impulse.get_mut(0)
                {
                    *x = false;
                }
                let originator = self.ops.get(ev).map(|o| o.originator.clone());
                let links: Vec<usize> = self.graph.node(ev).map_or_else(Vec::new, |n| {
                    n.variables
                        .iter()
                        .filter(|l| l.property.as_deref() == Some("inputactor"))
                        .filter_map(|l| l.vars.first().copied())
                        .collect()
                });
                for v in links {
                    let value = self.read_var(v);
                    let obj = self.resolve(&value);
                    if obj.is_some() && Some(obj) == originator {
                        self.activate_output(ev, 0);
                    }
                }
            }
            OpClass::CollectibleCollected => self.force_output(ev, 0),
            _ => {}
        }
    }

    /// Class `Activated` (native and script parts).
    pub(crate) fn activated(&mut self, op: usize, host: &mut dyn Host) {
        let Some(node) = self.graph.node(op) else {
            return;
        };
        let class = node.class;
        if node.latent_base
            && !matches!(
                class,
                OpClass::Delay
                    | OpClass::PlaySound
                    | OpClass::Interp
                    | OpClass::MultiLevelStreaming
            )
            && let Some(o) = self.ops.get_mut(op)
        {
            // `SeqAct_Latent::Activated`: no latent actors → aborted. The
            // classes above override `Activated` without calling it
            // (decompiled `USeqAct_Delay`, `USeqAct_PlaySound`,
            // `USeqAct_Interp` and `USeqAct_MultiLevelStreaming::Activated`),
            // so they are never aborted: streaming finishes on output 0.
            o.aborted = true;
        }
        match class {
            // ---------------------------------------------------- engine
            OpClass::ActivateRemoteEvent => {
                let name = self.prop(op, "EventName");
                let name = name.as_str().unwrap_or("").to_owned();
                let instigator = match self.resolve(&self.prop(op, "Instigator")) {
                    Obj::None => Obj::World,
                    o => o,
                };
                let roots = self.attached_roots();
                let mut found = false;
                for e in self.graph.find_by_class(&roots, OpClass::RemoteEvent) {
                    let matches = self
                        .graph
                        .node(e)
                        .and_then(|x| x.param("EventName"))
                        .and_then(KValue::as_str)
                        .is_some_and(|n| n.eq_ignore_ascii_case(&name));
                    if !matches {
                        continue;
                    }
                    found = true;
                    if self.ops.get(e).is_some_and(|o| o.enabled) {
                        self.populate(e);
                        self.check_activate(e, Obj::World, instigator.clone(), false, None, false);
                    }
                }
                if !found {
                    self.error(format!("op {op}: no remote event named {name}"));
                }
            }
            OpClass::AddInt => {
                let a = self.prop(op, "ValueA").as_int();
                let b = self.prop(op, "ValueB").as_int();
                let f = a.wrapping_add(b) as f32;
                self.set_prop(op, "FloatResult", KValue::Float(f));
                self.set_prop(op, "IntResult", KValue::Int(f.round() as i32));
                self.activate_output(op, 0);
            }
            OpClass::CameraFade => {
                let time = self.prop(op, "FadeTime").as_float();
                self.emit(Output::CameraFade {
                    opacity: self.prop(op, "FadeOpacity").as_float(),
                    time,
                    persist: self.prop(op, "bPersistFade").as_bool(),
                    fade_audio: self.prop(op, "bFadeAudio").as_bool(),
                });
                if let Some(o) = self.ops.get_mut(op) {
                    o.latent = Latent::Fade { remaining: time };
                }
                self.activate_output(op, 0);
            }
            OpClass::CameraShake => {
                let start = self.impulse(op, 0);
                self.emit(Output::CameraShake {
                    start,
                    shake: self.obj_path(&self.prop(op, "Shake")),
                    scale: self.prop(op, "ShakeScale").as_float(),
                });
            }
            OpClass::ChangeCollision => {
                let c = self.prop(op, "bCollideActors").as_bool();
                let b = self.prop(op, "bBlockActors").as_bool();
                for a in self.target_actors(op) {
                    if let Some(info) = self.with_actor(a) {
                        host.change_collision(&info, c, b);
                    }
                }
            }
            OpClass::ConsoleCommand => {
                if self.has_player_target(op) {
                    let mut cmds: Vec<String> = self
                        .prop(op, "Commands")
                        .items()
                        .iter()
                        .filter_map(|v| v.as_str().map(str::to_owned))
                        .collect();
                    if cmds.is_empty()
                        && let Some(c) = self.prop(op, "Command").as_str()
                        && !c.is_empty()
                    {
                        cmds.push(c.to_owned());
                    }
                    for c in cmds {
                        self.console_command(&c);
                    }
                }
            }
            OpClass::Delay => {
                let linked = !self.vars_of(op, "Duration").is_empty();
                let d = if linked {
                    self.prop(op, "Duration").as_float()
                } else {
                    self.prop(op, "DefaultDuration").as_float()
                };
                let frame = self.frame;
                if let Some(o) = self.ops.get_mut(op) {
                    o.latent = Latent::Delay {
                        remaining: d,
                        active: false,
                        start_frame: frame,
                    };
                }
            }
            OpClass::Destroy => {
                for a in self.target_actors(op) {
                    if let Some(info) = self.with_actor(a) {
                        host.destroy_actor(&info);
                        self.emit(Output::ActorDestroyed { actor: info.path });
                    }
                }
            }
            OpClass::Interp => self.interp_activated(op, host),
            OpClass::MultiLevelStreaming | OpClass::LevelStreaming => {
                let load = self.impulse(op, 0);
                let visible = load && self.prop(op, "bMakeVisibleAfterLoad").as_bool();
                let mut names: Vec<String> = Vec::new();
                for l in self.prop(op, "Levels").items() {
                    if let Some(n) = l.field("LevelName").and_then(KValue::as_str) {
                        names.push(n.to_owned());
                    }
                }
                if let Some(n) = self.prop(op, "LevelName").as_str()
                    && !n.is_empty()
                {
                    names.push(n.to_owned());
                }
                for n in names {
                    if !host.set_level_streamed(&n, load, visible) {
                        self.error(format!("op {op}: unknown streaming level {n}"));
                    }
                    if visible {
                        self.attach_level(&n);
                    }
                }
            }
            OpClass::PlayCameraAnim => {
                self.emit(Output::CameraAnim {
                    play: self.impulse(op, 0),
                    anim: self.obj_path(&self.prop(op, "CameraAnim")),
                    looping: self.prop(op, "bLoop").as_bool(),
                    rate: self.prop(op, "Rate").as_float(),
                });
            }
            OpClass::PlayMusicTrack => {
                self.emit(Output::MusicTrack {
                    track: self.prop(op, "MusicTrack"),
                });
            }
            OpClass::PlaySound => self.play_sound_activated(op),
            OpClass::SetBool => {
                let vals: Vec<usize> = self.vars_of(op, "Value");
                let v = if vals.is_empty() {
                    self.prop(op, "DefaultValue").as_bool()
                } else {
                    vals.iter()
                        .all(|v| self.vars.get(*v).is_some_and(KValue::as_bool))
                };
                for t in self.vars_of(op, "Target") {
                    self.write_var(t, KValue::Bool(v));
                }
            }
            OpClass::SetFloat => {
                let sum = self
                    .prop(op, "Value")
                    .items()
                    .iter()
                    .fold(0.0f32, |a, v| a + v.as_float());
                let single = match self.prop(op, "Value") {
                    KValue::Float(f) => Some(f),
                    _ => None,
                };
                self.set_prop(op, "Target", KValue::Float(single.unwrap_or(sum)));
            }
            OpClass::SetInt => {
                let v = self.prop(op, "Value");
                let n = match &v {
                    KValue::Array(items) => {
                        items.iter().fold(0i32, |a, x| a.wrapping_add(x.as_int()))
                    }
                    other => other.as_int(),
                };
                self.set_prop(op, "Target", KValue::Int(n));
            }
            OpClass::SetString => {
                let v = self.prop(op, "Value");
                self.set_prop(op, "Target", v);
            }
            OpClass::SetObject => {
                let mut v = self.prop(op, "Value");
                if matches!(v, KValue::Obj(None) | KValue::None) {
                    v = self.prop(op, "DefaultValue");
                }
                let n = self.prop(op, "Targets").items().len();
                self.set_prop(op, "Targets", KValue::Array(vec![v; n]));
            }
            OpClass::SetCameraTarget => {
                let target = self
                    .vars_of(op, "Cam Target")
                    .first()
                    .map(|v| self.read_var(*v))
                    .or_else(|| Some(self.prop(op, "CameraTarget")))
                    .and_then(|v| v.as_obj().map(str::to_owned));
                self.emit(Output::CameraTarget { target });
            }
            OpClass::SetMatInstScalarParam => {
                self.emit(Output::MaterialScalar {
                    material: self.obj_path(&self.prop(op, "MatInst")),
                    param: self.prop(op, "ParamName").as_str().unwrap_or("").to_owned(),
                    value: self.prop(op, "ScalarValue").as_float(),
                });
            }
            OpClass::SetSoundMode => {
                self.emit(Output::SoundMode {
                    start: self.impulse(op, 0),
                    mode: self.obj_path(&self.prop(op, "SoundMode")),
                    top_priority: self.prop(op, "bTopPriority").as_bool(),
                });
            }
            OpClass::SetVelocity => {
                if self.has_player_target(op) {
                    let dir = self.prop(op, "VelocityDir").as_vec3().unwrap_or([0.0; 3]);
                    let mag = self.prop(op, "VelocityMag").as_float();
                    let len = (dir[0] * dir[0] + dir[1] * dir[1] + dir[2] * dir[2]).sqrt();
                    let v = if len > 0.0 {
                        dir.map(|c| c / len * mag)
                    } else {
                        [0.0; 3]
                    };
                    host.set_player_velocity(v);
                }
            }
            OpClass::Teleport => {
                let dest = self
                    .vars_of(op, "Destination")
                    .first()
                    .map(|v| self.read_var(*v))
                    .map(|v| self.resolve(&v));
                if self.has_player_target(op)
                    && let Some(Obj::Actor(a)) = dest
                    && let Some((loc, rot)) = self.current_transform(a, host)
                {
                    let update = self.prop(op, "bUpdateRotation").as_bool();
                    host.teleport_player(loc, update.then_some(rot));
                }
            }
            OpClass::Toggle | OpClass::ToggleHidden => {
                if let Some(mode) = self.toggle_mode(op) {
                    for v in self.vars_of(op, "Bool") {
                        let cur = self.vars.get(v).is_some_and(KValue::as_bool);
                        let new = match mode {
                            ToggleMode::On => true,
                            ToggleMode::Off => false,
                            ToggleMode::Toggle => !cur,
                        };
                        self.write_var(v, KValue::Bool(new));
                    }
                    let events: Vec<usize> = self
                        .graph
                        .node(op)
                        .and_then(|n| n.event_links.first())
                        .map(|l| l.events.clone())
                        .unwrap_or_default();
                    for e in events {
                        if let Some(o) = self.ops.get_mut(e) {
                            o.enabled = match mode {
                                ToggleMode::On => true,
                                ToggleMode::Off => false,
                                ToggleMode::Toggle => !o.enabled,
                            };
                        }
                    }
                    for a in self.target_actors(op) {
                        if let Some(info) = self.with_actor(a) {
                            if class == OpClass::ToggleHidden {
                                host.set_actor_hidden(&info, mode);
                                self.emit(Output::ActorHidden {
                                    actor: info.path,
                                    mode,
                                });
                            } else {
                                host.toggle_actor(&info, mode);
                                self.emit(Output::ActorToggled {
                                    actor: info.path,
                                    mode,
                                });
                            }
                        }
                    }
                }
            }
            OpClass::ToggleCinematicMode => {
                if let Some(mode) = self.toggle_mode(op) {
                    let flags = [
                        self.prop(op, "bHidePlayer").as_bool(),
                        self.prop(op, "bHideHUD").as_bool(),
                        self.prop(op, "bDisableMovement").as_bool(),
                        self.prop(op, "bDisableTurning").as_bool(),
                        self.prop(op, "bDisableInput").as_bool(),
                    ];
                    self.emit(Output::CinematicMode { mode, flags });
                }
            }
            OpClass::ToggleHud => {
                if let Some(mode) = self.toggle_mode(op) {
                    self.emit(Output::Hud { mode });
                }
            }
            OpClass::Gate => {
                // Stock gate: inputs In, Open, Close, Toggle; passes In while
                // open.
                let open = self.prop(op, "bOpen").as_bool();
                let mut open_now = open;
                if self.impulse(op, 1) {
                    open_now = true;
                }
                if self.impulse(op, 2) {
                    open_now = false;
                }
                if self.impulse(op, 3) {
                    open_now = !open_now;
                }
                self.set_prop(op, "bOpen", KValue::Bool(open_now));
                if self.impulse(op, 0) && open {
                    self.activate_output(op, 0);
                }
            }
            OpClass::Log => {}
            OpClass::OpenMovie => {
                self.emit(Output::OpenMovie {
                    movie: self.obj_path(&self.prop(op, "Movie")),
                    class: self.obj_path(&self.prop(op, "MoviePlayerClass")),
                });
            }
            // ---------------------------------------------------- asamu
            OpClass::AddAdaptiveTracks => {
                self.emit(Output::AdaptiveTracks {
                    tracks: self.prop(op, "tracksToAdd"),
                });
            }
            OpClass::DisablePauseMenu => {
                if self.impulse(op, 0) {
                    self.emit(Output::PauseMenu { enabled: true });
                }
                if self.impulse(op, 1) {
                    self.emit(Output::PauseMenu { enabled: false });
                }
            }
            OpClass::EditMultiplierForAllTracks => {
                self.emit(Output::AdaptiveMultiplier {
                    track: None,
                    multiplier_id: self
                        .prop(op, "multiplierID")
                        .as_str()
                        .unwrap_or("")
                        .to_owned(),
                    multiplier: self.prop(op, "Multiplier").as_float(),
                    adjust_time: self.prop(op, "adjustTime").as_float(),
                });
            }
            OpClass::SetAdaptiveTrackVolumeMultiplier => {
                self.emit(Output::AdaptiveMultiplier {
                    track: self.prop(op, "trackID").as_str().map(str::to_owned),
                    multiplier_id: self
                        .prop(op, "multiplierID")
                        .as_str()
                        .unwrap_or("")
                        .to_owned(),
                    multiplier: self.prop(op, "Multiplier").as_float(),
                    adjust_time: self.prop(op, "adjustTime").as_float(),
                });
            }
            OpClass::EditOrAddSaveString => {
                let id = self.prop(op, "Id").as_str().unwrap_or("").to_owned();
                let v = self.prop(op, "Value").as_int();
                self.save_strings.insert(id, v);
            }
            OpClass::GetSaveStringValue => {
                let id = self.prop(op, "Id").as_str().unwrap_or("").to_owned();
                match self.save_strings.get(&id).copied() {
                    Some(v) => {
                        self.set_prop(op, "Value", KValue::Int(v));
                        self.force_output(op, 0);
                    }
                    None => self.force_output(op, 1),
                }
            }
            OpClass::StartTimeTrial | OpClass::EndTimeTrial => {
                if host.is_time_trial() || self.time_trial {
                    self.emit(Output::TimeTrial {
                        start: class == OpClass::StartTimeTrial,
                    });
                }
            }
            OpClass::ShowTutorialPopup => {
                self.tutorial_id = self.tutorial_id.saturating_add(1);
                let id = self.tutorial_id;
                self.set_prop(op, "Id", KValue::Int(id));
                self.emit(Output::TutorialShow {
                    id,
                    msg: self.prop(op, "msg"),
                });
            }
            OpClass::HideTutorialPopup => {
                let id = self.prop(op, "Id").as_int();
                self.emit(Output::TutorialHide { id: id.max(0) });
            }
            OpClass::NarratorLine => {
                let id = self.prop(op, "Id").as_str().unwrap_or("").to_owned();
                let cue = self.obj_path(&self.prop(op, "Cue"));
                if self.impulse(op, 0) && self.narrator.lines.len() >= crate::narrator::MAX_LINES {
                    self.error(format!("op {op}: narrator queue full; line dropped"));
                } else if self.impulse(op, 0) {
                    let duration = self.cue_duration(cue.as_deref());
                    let calls = self.narrator.add_line(
                        op,
                        &id,
                        cue,
                        duration,
                        self.prop(op, "Delay").as_float(),
                        self.prop(op, "CueVolume").as_float(),
                    );
                    self.apply_narrator_calls(calls);
                }
                if self.impulse(op, 1) {
                    let stop = self.prop(op, "fadeOutIfActiveCue").as_bool();
                    let calls = self.narrator.remove_line(&id, stop);
                    self.apply_narrator_calls(calls);
                }
                self.force_output(op, 0);
            }
            OpClass::PauseWorm | OpClass::StartWorm | OpClass::ShutDownWorm => {
                let worm = self.obj_path(&self.prop(op, "wormPawn"));
                let action = match class {
                    OpClass::StartWorm => "start",
                    OpClass::ShutDownWorm => "shutdown",
                    _ if self.impulse(op, 0) => "unpause",
                    _ => "pause",
                };
                if worm.is_some() {
                    self.emit(Output::Worm { action, worm });
                }
                self.force_output(op, 0);
            }
            OpClass::PlayerDiedAction => host.kill_player(),
            OpClass::PlaySuitOnAnimation => {
                self.emit(Output::SuitOnAnimation);
                self.force_output(op, 0);
            }
            OpClass::SetGameFinished => {
                let mut finished = self.prop(op, "gameFinished").as_bool();
                if self.impulse(op, 0) {
                    finished = true;
                } else if self.impulse(op, 1) {
                    finished = false;
                }
                self.set_prop(op, "gameFinished", KValue::Bool(finished));
                self.emit(Output::GameFinished { finished });
                self.force_output(op, 0);
            }
            OpClass::SetLookAtTarget => {
                self.emit(Output::LookAtTarget {
                    look: self.impulse(op, 0),
                    target: self.obj_path(&self.prop(op, "LookAtActor")),
                    looking: self.obj_path(&self.prop(op, "lookingAtActorActor")),
                });
            }
            OpClass::SetMaxGrapples => host.set_max_grapples(self.prop(op, "Grapples").as_int()),
            OpClass::SetRotationToPlayerRotation => {
                if let Some(a) = self.prop_actor(op, "Obj")
                    && let Some((loc, rot)) = self.current_transform(a, host)
                {
                    let p = host.player_rotation();
                    let new = [
                        if self.prop(op, "Pitch").as_bool() {
                            p[0]
                        } else {
                            rot[0]
                        },
                        if self.prop(op, "Yaw").as_bool() {
                            p[1]
                        } else {
                            rot[1]
                        },
                        if self.prop(op, "Roll").as_bool() {
                            p[2]
                        } else {
                            rot[2]
                        },
                    ];
                    self.place_actor(a, loc, Some(new), host);
                }
            }
            OpClass::SetVelocityConeMaterial => {
                self.emit(Output::VelocityConeMaterial {
                    material: self.obj_path(&self.prop(op, "Mat")),
                });
            }
            OpClass::ShowTitleLogo => {
                if self.impulse(op, 0) {
                    self.emit(Output::TitleLogo { show: true });
                }
                if self.impulse(op, 1) {
                    self.emit(Output::TitleLogo { show: false });
                }
            }
            OpClass::ToggleAttractor => {
                self.force_output(op, 0);
                match self.prop_actor(op, "Attractor") {
                    Some(a) => {
                        let ok = self
                            .with_actor(a)
                            .is_some_and(|info| host.activate_attractor(&info));
                        if ok && self.attractors.len() < crate::runtime::MAX_PENDING_ATTRACTORS {
                            self.attractors
                                .push(PendingAttractor { node: op, actor: a });
                        } else if ok {
                            self.error(format!("op {op}: too many pending attractors"));
                        } else {
                            self.error(format!("op {op}: attractor not activated"));
                        }
                    }
                    None => self.error(format!("op {op}: attractor input is not an attractor")),
                }
            }
            OpClass::ToggleCheckpointEnable => {
                if let Some(a) = self.prop_actor(op, "Checkpoint")
                    && let Some(info) = self.with_actor(a)
                {
                    if self.impulse(op, 0) {
                        host.set_checkpoint_enabled(&info, true);
                    }
                    if self.impulse(op, 1) {
                        host.set_checkpoint_enabled(&info, false);
                    }
                }
            }
            OpClass::ToggleCrosshair => {
                let fade = self.prop(op, "Fade").as_bool();
                if self.impulse(op, 0) {
                    self.emit(Output::Crosshair { show: true, fade });
                }
                if self.impulse(op, 1) {
                    self.emit(Output::Crosshair { show: false, fade });
                }
            }
            OpClass::ToggleFallingRocksActive => {
                if self.impulse(op, 0) {
                    host.set_falling_rocks_active(true);
                }
                if self.impulse(op, 1) {
                    host.set_falling_rocks_active(false);
                }
            }
            OpClass::ToggleFollowCollision => {
                let actor = self.obj_path(&self.prop(op, "followCollisionActor"));
                if actor.is_some() {
                    if self.impulse(op, 0) {
                        self.emit(Output::FollowCollision {
                            actor: actor.clone(),
                            enable: true,
                        });
                    } else if self.impulse(op, 1) {
                        self.emit(Output::FollowCollision {
                            actor,
                            enable: false,
                        });
                    }
                }
                self.force_output(op, 0);
            }
            OpClass::ToggleGrapple => host.enable_grapple(self.prop(op, "Enable").as_bool()),
            OpClass::ToggleRestartFromCheckpointOption => {
                if self.impulse(op, 0) {
                    self.emit(Output::RestartCheckpointOption { enabled: true });
                }
                if self.impulse(op, 1) {
                    self.emit(Output::RestartCheckpointOption { enabled: false });
                }
            }
            OpClass::ToggleRocketBoots => {
                host.enable_rocket_boots(self.prop(op, "Enable").as_bool());
            }
            OpClass::ToggleSpawnInStoryMode => host.set_spawn_in_story_mode(self.impulse(op, 0)),
            OpClass::ToggleStoryMode => {
                if self.impulse(op, 0) && !host.in_story_mode() {
                    host.set_story_mode(true);
                }
                if self.impulse(op, 1) && host.in_story_mode() {
                    host.set_story_mode(false);
                }
                if self.impulse(op, 2) {
                    let on = host.in_story_mode();
                    host.set_story_mode(!on);
                }
            }
            OpClass::ToggleVisibleGrapple => {
                let animate = self.prop(op, "AnimateHand").as_bool();
                let vis = self.prop(op, "Visibility").as_bool();
                if self.impulse(op, 0) {
                    host.hide_grapple_gun(true, animate, vis);
                }
                if self.impulse(op, 1) {
                    host.hide_grapple_gun(false, animate, vis);
                }
            }
            OpClass::ToggleZoomAvailable => {
                if self.impulse(op, 0) {
                    host.set_zoom_available(true);
                }
                if self.impulse(op, 1) {
                    host.set_zoom_available(false);
                }
                self.force_output(op, 0);
            }
            OpClass::TriggerCheckpoint => {
                let ok = self
                    .prop_actor(op, "checkpointPositionObject")
                    .and_then(|a| self.with_actor(a))
                    .is_some_and(|info| host.trigger_checkpoint(&info));
                if !ok {
                    self.error(format!("op {op}: checkpoint not found"));
                }
            }
            OpClass::UnlockAchievement => {
                self.emit(Output::Achievement {
                    id: self
                        .prop(op, "achievementToUnlock")
                        .as_str()
                        .unwrap_or("")
                        .to_owned(),
                });
            }
            OpClass::MenuInvoke => {
                self.emit(Output::MenuInvoke {
                    path: self
                        .prop(op, "FunctionPath")
                        .as_str()
                        .unwrap_or("")
                        .to_owned(),
                    function: self
                        .prop(op, "InvokeFunction")
                        .as_str()
                        .unwrap_or("")
                        .to_owned(),
                });
            }
            // ---------------------------------------------------- conditions
            OpClass::CompareBool => {
                let vars = self.vars_of(op, "Bool");
                let r = vars
                    .iter()
                    .all(|v| self.vars.get(*v).is_some_and(KValue::as_bool));
                self.set_prop(op, "bResult", KValue::Bool(r));
                self.activate_output(op, usize::from(!r));
            }
            OpClass::CompareFloat => {
                let a = self.prop(op, "ValueA").as_float();
                let b = self.prop(op, "ValueB").as_float();
                self.compare_outputs(op, a.partial_cmp(&b));
            }
            OpClass::CompareInt => {
                let a = self.prop(op, "ValueA").as_int();
                let b = self.prop(op, "ValueB").as_int();
                self.compare_outputs(op, Some(a.cmp(&b)));
            }
            OpClass::Increment => {
                let a = self
                    .prop(op, "ValueA")
                    .as_int()
                    .wrapping_add(self.prop(op, "IncrementAmount").as_int());
                self.set_prop(op, "ValueA", KValue::Int(a));
                let b = self.prop(op, "ValueB").as_int();
                self.compare_outputs(op, Some(a.cmp(&b)));
            }
            OpClass::CompareObject => {
                let a = self
                    .vars_of(op, "A")
                    .first()
                    .map(|v| self.read_var(*v))
                    .map(|v| self.resolve(&v))
                    .unwrap_or_default();
                let b = self
                    .vars_of(op, "B")
                    .first()
                    .map(|v| self.read_var(*v))
                    .map(|v| self.resolve(&v))
                    .unwrap_or_default();
                self.activate_output(op, usize::from(a != b));
            }
            OpClass::IsPie => {
                self.activate_output(op, 1);
            }
            OpClass::IsTimeTrial => {
                let tt = host.is_time_trial() || self.time_trial;
                self.force_output(op, usize::from(!tt));
            }
            // Events, variables, sequences, frames.
            _ if self.graph.node(op).is_some_and(|n| n.class.is_event()) => {}
            OpClass::Unknown => {
                let class = self
                    .graph
                    .node(op)
                    .map(|n| n.class_name.clone())
                    .unwrap_or_default();
                *self.stats.unhandled.entry(class.clone()).or_insert(0) += 1;
                self.emit(Output::Unhandled { node: op, class });
            }
            _ => {}
        }
    }

    /// The five comparison outputs: `A <= B`, `A > B`, `A == B`, `A < B`,
    /// `A >= B` (several at once; NaN fires none).
    fn compare_outputs(&mut self, op: usize, ord: Option<std::cmp::Ordering>) {
        use std::cmp::Ordering::{Equal, Greater, Less};
        let Some(o) = ord else { return };
        if matches!(o, Less | Equal) {
            self.activate_output(op, 0);
        }
        if o == Greater {
            self.activate_output(op, 1);
        }
        if o == Equal {
            self.activate_output(op, 2);
        }
        if o == Less {
            self.activate_output(op, 3);
        }
        if matches!(o, Greater | Equal) {
            self.activate_output(op, 4);
        }
    }

    fn console_command(&mut self, cmd: &str) {
        let trimmed = cmd.trim();
        let mut parts = trimmed.splitn(2, char::is_whitespace);
        let verb = parts.next().unwrap_or("");
        if verb.eq_ignore_ascii_case("open") {
            let rest = parts.next().unwrap_or("").trim();
            let (map, options) = match rest.split_once('?') {
                Some((m, o)) => (m.to_owned(), Some(o.to_owned())),
                None => (rest.to_owned(), None),
            };
            self.emit(Output::LevelTransition { map, options });
        } else if !trimmed.is_empty() {
            self.emit(Output::ConsoleCommand {
                command: trimmed.to_owned(),
            });
        }
    }

    fn play_sound_activated(&mut self, op: usize) {
        let cue = self.obj_path(&self.prop(op, "PlaySound"));
        if let Some(o) = self.ops.get_mut(op) {
            o.latent = Latent::Sound {
                duration: 0.0,
                delay_reached: false,
                stopped: false,
            };
        }
        if cue.is_some() {
            if self.impulse(op, 0) {
                let extra = self.prop(op, "ExtraDelay").as_float();
                if extra.abs() < SMALL_NUMBER {
                    self.start_sound(op);
                }
                let duration = self.first_wave_duration(cue.as_deref()) + extra;
                if let Some(o) = self.ops.get_mut(op) {
                    o.latent = Latent::Sound {
                        duration,
                        delay_reached: false,
                        stopped: false,
                    };
                    if let Some(i) = o.in_impulse.get_mut(0) {
                        *i = false;
                    }
                }
            } else if self.impulse(op, 1) {
                self.stop_sound(op);
            }
        }
        self.activate_output(op, 0);
    }

    fn start_sound(&mut self, op: usize) {
        let targets = self
            .targets(op)
            .into_iter()
            .filter_map(|t| match t {
                Obj::Actor(a) => self.graph.actor(a).map(|x| x.path.clone()),
                _ => None,
            })
            .collect();
        self.emit(Output::PlaySound {
            node: op,
            cue: self.obj_path(&self.prop(op, "PlaySound")),
            targets,
            volume: self.prop(op, "VolumeMultiplier").as_float(),
            pitch: self.prop(op, "PitchMultiplier").as_float(),
            fade_in: self.prop(op, "FadeInTime").as_float(),
        });
    }

    fn stop_sound(&mut self, op: usize) {
        self.emit(Output::StopSound {
            node: op,
            fade_out: self.prop(op, "FadeOutTime").as_float(),
        });
        if let Some(o) = self.ops.get_mut(op)
            && let Latent::Sound {
                duration, stopped, ..
            } = &mut o.latent
        {
            *stopped = true;
            *duration = 0.0;
        }
    }

    /// Class `UpdateOp`; true when finished.
    pub(crate) fn update_op(&mut self, op: usize, dt: f32, host: &mut dyn Host) -> bool {
        let Some(node) = self.graph.node(op) else {
            return true;
        };
        let class = node.class;
        match class {
            OpClass::Delay => {
                let start = self.impulse(op, 0);
                let stop = self.impulse(op, 1);
                let pause = self.impulse(op, 2);
                let restart = self.prop(op, "bStartWillRestart").as_bool();
                let linked = !self.vars_of(op, "Duration").is_empty();
                let duration = if linked {
                    self.prop(op, "Duration").as_float()
                } else {
                    self.prop(op, "DefaultDuration").as_float()
                };
                let frame = self.frame;
                let mut fire = false;
                let mut finished = false;
                if let Some(o) = self.ops.get_mut(op)
                    && let Latent::Delay {
                        remaining,
                        active,
                        start_frame,
                    } = &mut o.latent
                {
                    if start {
                        if restart {
                            *remaining = duration;
                            *start_frame = frame;
                        }
                        *active = true;
                    } else if stop {
                        *active = false;
                        return true;
                    } else if pause {
                        *active = false;
                    }
                    if *active && frame != *start_frame {
                        *remaining -= dt;
                        if *remaining <= 0.0 {
                            fire = true;
                            finished = true;
                        }
                    }
                }
                if fire {
                    self.activate_output(op, 0);
                }
                finished
            }
            OpClass::PlaySound => {
                if self.impulse(op, 0) {
                    self.play_sound_activated(op);
                } else if self.impulse(op, 1) {
                    self.stop_sound(op);
                } else {
                    let cue = self.obj_path(&self.prop(op, "PlaySound"));
                    let wave = self.first_wave_duration(cue.as_deref());
                    let extra = self.prop(op, "ExtraDelay").as_float();
                    let before_end = self.prop(op, "BeforeEndTime").as_float();
                    let mut start = false;
                    let mut before = false;
                    if let Some(o) = self.ops.get_mut(op)
                        && let Latent::Sound {
                            duration,
                            delay_reached,
                            stopped,
                        } = &mut o.latent
                    {
                        *duration -= dt;
                        if !*delay_reached
                            && !*stopped
                            && extra.abs() >= SMALL_NUMBER
                            && *duration <= wave
                        {
                            *delay_reached = true;
                            start = true;
                        }
                        if before_end >= 0.0
                            && *duration <= before_end
                            && before_end < *duration + dt
                        {
                            before = true;
                        }
                    }
                    if start {
                        self.start_sound(op);
                    }
                    if before && self.graph.node(op).is_some_and(|n| n.outputs.len() > 3) {
                        self.activate_output(op, 3);
                    }
                }
                match self.ops.get(op).map(|o| &o.latent) {
                    Some(Latent::Sound { duration, .. }) => *duration <= 0.0,
                    _ => true,
                }
            }
            OpClass::CameraFade => {
                let mut done = false;
                if let Some(o) = self.ops.get_mut(op)
                    && let Latent::Fade { remaining } = &mut o.latent
                {
                    *remaining -= dt;
                    done = *remaining <= 0.0;
                }
                if done {
                    self.activate_output(op, 1);
                }
                done
            }
            OpClass::Interp => self.interp_update(op, dt, host),
            // Streaming completes at once in the simulation.
            _ => true,
        }
    }

    /// Class `DeActivated`.
    pub(crate) fn deactivated(&mut self, op: usize, _host: &mut dyn Host) {
        let Some(node) = self.graph.node(op) else {
            return;
        };
        let class = node.class;
        let latent = node.latent_base;
        let auto = node.auto_activate_outputs;
        let n_out = node.outputs.len();
        match class {
            OpClass::Delay => {}
            OpClass::PlaySound => {
                let stopped = matches!(
                    self.ops.get(op).map(|o| &o.latent),
                    Some(Latent::Sound { stopped: true, .. })
                );
                self.activate_output(op, if stopped { 2 } else { 1 });
            }
            OpClass::Interp => self.interp_deactivated(op),
            OpClass::CameraFade => {}
            _ if latent => {
                // `SeqAct_Latent::DeActivated`: output 1 when aborted and
                // there is more than one output, else output 0.
                let aborted = self.ops.get(op).is_some_and(|o| o.aborted);
                let idx = usize::from(n_out != 1 && aborted);
                if n_out > 0 {
                    self.activate_output(op, idx);
                }
                if let Some(o) = self.ops.get_mut(op) {
                    o.aborted = false;
                }
                if class == OpClass::NarratorLine {
                    // Script `Deactivated`: output 1 when not aborted (the
                    // flag is already cleared), else 2.
                    self.activate_output(op, 1);
                }
            }
            _ if auto => {
                for i in 0..n_out {
                    self.activate_output(op, i);
                }
            }
            _ => {}
        }
    }
}
