//! The game side of the interpreter: [`Host`] (calls that change the game)
//! and [`Output`] (typed events for presentation: audio, UI, narration,
//! camera, level transitions).
//!
//! Game-affecting ASAMU actions call the host at once, in the frame the
//! action runs (the original's script calls the pawn, gun, checkpoint or
//! game-info functions synchronously). Presentation actions only emit an
//! [`Output`]; the app turns them into sound, subtitles, HUD changes.

use crate::anim::AnimPosition;
use crate::graph::ActorInfo;
use crate::value::KValue;

/// A value a Matinee property track writes on an actor.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize)]
#[serde(tag = "type", content = "value", rename_all = "snake_case")]
pub enum PropertyValue {
    /// `InterpTrackFloatProp` (e.g. a light's `Brightness` or `Radius`).
    Float(f32),
    /// `InterpTrackVectorProp` (e.g. `DrawScale3D`).
    Vector([f32; 3]),
    /// `InterpTrackColorProp`: linear colour components in 0..1 (the engine
    /// stores them as an `FColor`: `255 · c^(1/2.2)`, clamped).
    Color([f32; 3]),
}

/// Which input of an on/off/toggle action fired.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ToggleMode {
    /// First input (`Turn On`, `Hide`, `Enable`, `Show`).
    On,
    /// Second input (`Turn Off`, `UnHide`, `Disable`, `Hide`).
    Off,
    /// Third input (`Toggle`).
    Toggle,
}

/// Game-affecting effects and the game state Kismet reads.
///
/// Every method has a do-nothing default so tests and tools can implement
/// a subset. Actors are passed as their [`ActorInfo`] from the graph's actor
/// table (object path, class, level package, slot).
pub trait Host {
    /// The game runs the time-trial game type (`SeqCond_IsTimeTrial`).
    fn is_time_trial(&self) -> bool {
        false
    }
    /// The pawn is in story state (`SeqAct_ToggleStoryMode` reads it).
    fn in_story_mode(&self) -> bool {
        false
    }
    /// Enter (`true`) or leave story state.
    fn set_story_mode(&mut self, _on: bool) {}
    /// `ToggleSpawnInStoryMode` on the pawn.
    fn set_spawn_in_story_mode(&mut self, _on: bool) {}
    /// `GrappleGun.SetMaxGrapples`.
    fn set_max_grapples(&mut self, _n: i32) {}
    /// `ASAMUPawn.EnableGrapple` (one-shot latch, GRAPPLE.md G-IN-4).
    fn enable_grapple(&mut self, _enable: bool) {}
    /// `ASAMUPawn.EnableRocketBoots`.
    fn enable_rocket_boots(&mut self, _enable: bool) {}
    /// `GrappleGun.HideGrappleGun(hide, animate, visibility)`.
    fn hide_grapple_gun(&mut self, _hide: bool, _animate: bool, _visibility: bool) {}
    /// `ASAMUPawn.ToggleZoomAvailable`.
    fn set_zoom_available(&mut self, _on: bool) {}
    /// `ASAMUCheckpoint.ActivateCheckpoint`; `false` when the actor is not a
    /// checkpoint of the level.
    fn trigger_checkpoint(&mut self, _actor: &ActorInfo) -> bool {
        false
    }
    /// Sets a checkpoint's `bEnabled`.
    fn set_checkpoint_enabled(&mut self, _actor: &ActorInfo, _enabled: bool) -> bool {
        false
    }
    /// `ASAMUTelePad_Attractor.ActivatePad`. The host reports the pad's end
    /// later with `Runtime::attractor_finished`.
    fn activate_attractor(&mut self, _actor: &ActorInfo) -> bool {
        false
    }
    /// `ASAMUFallingRockManager.ActivateAllRocks` / `DisableAllRocks`.
    fn set_falling_rocks_active(&mut self, _active: bool) {}
    /// Streams level `package` in (`loaded`) or out; `visible` as requested.
    /// Returns whether the level exists.
    fn set_level_streamed(&mut self, _package: &str, _loaded: bool, _visible: bool) -> bool {
        false
    }
    /// `SeqAct_Toggle` on an actor (`OnToggle`: triggers, volumes, lights).
    fn toggle_actor(&mut self, _actor: &ActorInfo, _mode: ToggleMode) {}
    /// `SeqAct_ToggleHidden` on an actor (`On` = hide, `Off` = unhide).
    fn set_actor_hidden(&mut self, _actor: &ActorInfo, _mode: ToggleMode) {}
    /// `SeqAct_Destroy` on an actor.
    fn destroy_actor(&mut self, _actor: &ActorInfo) {}
    /// `SeqAct_ChangeCollision` on an actor.
    fn change_collision(&mut self, _actor: &ActorInfo, _collide: bool, _block: bool) {}
    /// Current location and rotation of an actor (Matinee initial transforms,
    /// teleport destinations); `None` = use the placement in the table.
    fn actor_transform(&self, _actor: &ActorInfo) -> Option<([f32; 3], [i32; 3])> {
        None
    }
    /// Moves an actor (Matinee move tracks, `SetRotationToPlayerRotation`).
    /// `rotation` `None` keeps the current rotation.
    fn set_actor_transform(
        &mut self,
        _actor: &ActorInfo,
        _location: [f32; 3],
        _rotation: Option<[i32; 3]>,
    ) {
    }
    /// The player's rotation (pitch, yaw, roll).
    fn player_rotation(&self) -> [i32; 3] {
        [0; 3]
    }
    /// `SeqAct_Teleport` of the player to a location; `rotation` when the
    /// action updates it. Returns success.
    fn teleport_player(&mut self, _location: [f32; 3], _rotation: Option<[i32; 3]>) -> bool {
        false
    }
    /// `SeqAct_SetVelocity` on the player.
    fn set_player_velocity(&mut self, _velocity: [f32; 3]) {}
    /// Kills the player (`SeqAct_PlayerDied`).
    fn kill_player(&mut self) {}
    /// The `SequenceLength` of `sequence` on the actor's skeletal mesh
    /// (`None`: unknown; Matinee animation tracks then use raw positions, as
    /// the engine does for a sequence missing from the track's anim sets).
    fn anim_sequence_length(&self, _actor: &ActorInfo, _sequence: &str) -> Option<f32> {
        None
    }
    /// The `SetAnimPosition` event of a skeletal actor (Matinee animation
    /// control). Returns the `NotifyName`s of the `AnimNotify_Kismet`
    /// notifies the move fired; the runtime activates the matching
    /// `SeqEvent_AnimNotify` events at once, as the notify's
    /// `CheckActivate` does inside the Matinee update.
    fn set_anim_position(&mut self, _actor: &ActorInfo, _call: &AnimPosition) -> Vec<String> {
        Vec::new()
    }
    /// `SetSkelControlStrength(name, strength)` (Matinee
    /// `InterpTrackSkelControlStrength`; blend time 0).
    fn set_skel_control_strength(&mut self, _actor: &ActorInfo, _control: &str, _strength: f32) {}
    /// A Matinee property track's value on an actor (`PropertyName`).
    fn set_actor_property(&mut self, _actor: &ActorInfo, _name: &str, _value: PropertyValue) {}
    /// `SeqAct_SetLookAtTarget` on a look-at actor: the stored target (never
    /// read by the original's tick) and the head and eye offsets added to
    /// the player pawn's location.
    fn set_look_at(
        &mut self,
        _actor: &ActorInfo,
        _target: Option<&ActorInfo>,
        _head_offset: [f32; 3],
        _eyes_offset: [f32; 3],
    ) {
    }
}

/// A host that does nothing (tests, headless tools).
#[derive(Debug, Clone, Copy, Default)]
pub struct NullHost;

impl Host for NullHost {}

/// A presentation (or app-level) event emitted by the interpreter.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
#[serde(tag = "output", rename_all = "snake_case")]
pub enum Output {
    /// `open <map>[?options]` console command: change level.
    LevelTransition {
        /// Map name.
        map: String,
        /// Text after `?`, if any.
        options: Option<String>,
    },
    /// Any other console command (handled or not by the game; KISMET.md).
    ConsoleCommand {
        /// The command line.
        command: String,
    },
    /// `SeqAct_PlaySound` started a sound.
    PlaySound {
        /// Action node id.
        node: usize,
        /// `SoundCue` path.
        cue: Option<String>,
        /// Target actor paths (empty = 2D at the listener).
        targets: Vec<String>,
        /// `VolumeMultiplier`.
        volume: f32,
        /// `PitchMultiplier`.
        pitch: f32,
        /// `FadeInTime`.
        fade_in: f32,
    },
    /// `SeqAct_PlaySound` stopped its sound.
    StopSound {
        /// Action node id.
        node: usize,
        /// `FadeOutTime`.
        fade_out: f32,
    },
    /// The narrator starts a line (subtitles from the cue's waves).
    NarratorLine {
        /// Action node id.
        node: usize,
        /// Line id.
        id: String,
        /// `SoundCue` path.
        cue: Option<String>,
        /// `CueVolume`.
        volume: f32,
    },
    /// The narrator stopped the playing line.
    NarratorStop {
        /// Line id.
        id: String,
    },
    /// Tutorial pop-up shown (`ShowTutorialPopup`; `msg` is the
    /// `tutorialMsg` struct).
    TutorialShow {
        /// Id given to the pop-up (ours: a running counter from 1).
        id: i32,
        /// The message struct.
        msg: KValue,
    },
    /// Tutorial pop-up removed (`id <= 0`: the current one).
    TutorialHide {
        /// Id.
        id: i32,
    },
    /// Crosshair shown or hidden.
    Crosshair {
        /// Show.
        show: bool,
        /// Fade instead of pop.
        fade: bool,
    },
    /// Cinematic mode.
    CinematicMode {
        /// On/off/toggle.
        mode: ToggleMode,
        /// `bHidePlayer`, `bHideHUD`, `bDisableMovement`, `bDisableTurning`,
        /// `bDisableInput`.
        flags: [bool; 5],
    },
    /// HUD visibility.
    Hud {
        /// Show/hide/toggle.
        mode: ToggleMode,
    },
    /// View target change (`SetCameraTarget`; `None` = the player).
    CameraTarget {
        /// Actor path.
        target: Option<String>,
    },
    /// Screen fade (`SeqAct_CameraFade`).
    CameraFade {
        /// Target opacity.
        opacity: f32,
        /// Seconds.
        time: f32,
        /// `bPersistFade`.
        persist: bool,
        /// `bFadeAudio`.
        fade_audio: bool,
    },
    /// Camera shake start/stop.
    CameraShake {
        /// Start.
        start: bool,
        /// Shake object path.
        shake: Option<String>,
        /// `ShakeScale`.
        scale: f32,
    },
    /// `SeqAct_PlayCameraAnim` Play (else Stop: every instance of the
    /// animation, `StopAllCameraAnimsByType(anim, false)`).
    CameraAnim {
        /// Play.
        play: bool,
        /// `CameraAnim` path.
        anim: Option<String>,
        /// `bLoop`.
        looping: bool,
        /// `Rate`.
        rate: f32,
        /// `IntensityScale`.
        scale: f32,
        /// `BlendInTime`.
        blend_in: f32,
        /// `BlendOutTime`.
        blend_out: f32,
        /// `bRandomStartTime`.
        random_start: bool,
    },
    /// `SeqAct_PlayMusicTrack`.
    MusicTrack {
        /// The `MusicTrack` struct.
        track: KValue,
    },
    /// `SeqAct_SetSoundMode`.
    SoundMode {
        /// Start (else stop).
        start: bool,
        /// Mode path.
        mode: Option<String>,
        /// `bTopPriority`.
        top_priority: bool,
    },
    /// Adaptive music tracks registered (`SeqAct_AddAdaptiveTracks`).
    AdaptiveTracks {
        /// `tracksToAdd`.
        tracks: KValue,
    },
    /// Adaptive music volume multiplier change.
    AdaptiveMultiplier {
        /// Track id (`None` = all tracks).
        track: Option<String>,
        /// Multiplier id.
        multiplier_id: String,
        /// Multiplier.
        multiplier: f32,
        /// Seconds.
        adjust_time: f32,
    },
    /// Scaleform movie opened (`GFxAction_OpenMovie`).
    OpenMovie {
        /// Movie path.
        movie: Option<String>,
        /// Player class.
        class: Option<String>,
    },
    /// ActionScript call on the menu movie.
    MenuInvoke {
        /// `FunctionPath`.
        path: String,
        /// `InvokeFunction`.
        function: String,
    },
    /// Full-screen title logo.
    TitleLogo {
        /// Show.
        show: bool,
    },
    /// Achievement unlocked.
    Achievement {
        /// Enumerator name.
        id: String,
    },
    /// `SetGameFinished`.
    GameFinished {
        /// The flag.
        finished: bool,
    },
    /// Time trial start/end (time-trial game type only).
    TimeTrial {
        /// Start (else end).
        start: bool,
    },
    /// HUD "restart from checkpoint" option.
    RestartCheckpointOption {
        /// Enabled.
        enabled: bool,
    },
    /// Pause menu availability.
    PauseMenu {
        /// Enabled.
        enabled: bool,
    },
    /// NPC look-at target.
    LookAtTarget {
        /// Start looking (else stop).
        look: bool,
        /// Target actor path.
        target: Option<String>,
        /// Looking actor path.
        looking: Option<String>,
    },
    /// Speed-line cone material.
    VelocityConeMaterial {
        /// Material path.
        material: Option<String>,
    },
    /// Follow-collision of a skeletal Matinee actor.
    FollowCollision {
        /// Actor path.
        actor: Option<String>,
        /// Enable.
        enable: bool,
    },
    /// Worm NPC control.
    Worm {
        /// `start`, `pause`, `unpause`, `shutdown`.
        action: &'static str,
        /// Worm pawn path.
        worm: Option<String>,
    },
    /// Suit-on hand animation.
    SuitOnAnimation,
    /// Scalar material parameter.
    MaterialScalar {
        /// Material instance path.
        material: Option<String>,
        /// Parameter.
        param: String,
        /// Value.
        value: f32,
    },
    /// Matinee director: view target changes to a group's actor (`None` =
    /// back to the player's camera).
    MatineeCut {
        /// `SeqAct_Interp` node.
        node: usize,
        /// Group name.
        group: Option<String>,
        /// Blend seconds.
        transition: f32,
    },
    /// Matinee fade amount changed.
    MatineeFade {
        /// `SeqAct_Interp` node.
        node: usize,
        /// Fade amount in `[0, 1]`.
        amount: f32,
    },
    /// Matinee sound key.
    MatineeSound {
        /// `SeqAct_Interp` node.
        node: usize,
        /// Cue path.
        cue: Option<String>,
        /// Bound actor path (the group's actor), if any.
        actor: Option<String>,
        /// Volume.
        volume: f32,
        /// Pitch.
        pitch: f32,
    },
    /// Matinee visibility or toggle key on a bound actor.
    MatineeKey {
        /// `SeqAct_Interp` node.
        node: usize,
        /// Actor path.
        actor: Option<String>,
        /// Enumerator (`EVTA_Hide`, `ETTA_On`, ...).
        action: String,
    },
    /// `SeqAct_Toggle` on an actor (lights, emitters, sounds, triggers...).
    ActorToggled {
        /// Actor path.
        actor: String,
        /// On/off/toggle.
        mode: ToggleMode,
    },
    /// `SeqAct_ToggleHidden` (or a Matinee visibility key) on an actor.
    ActorHidden {
        /// Actor path.
        actor: String,
        /// Hide/unhide/toggle.
        mode: ToggleMode,
    },
    /// `SeqAct_Destroy` on an actor.
    ActorDestroyed {
        /// Actor path.
        actor: String,
    },
    /// An action of a class without interpreter behaviour ran.
    Unhandled {
        /// Node id.
        node: usize,
        /// Qualified class.
        class: String,
    },
}
