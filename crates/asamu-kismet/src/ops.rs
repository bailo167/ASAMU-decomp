//! The Kismet classes the interpreter knows ([`OpClass`]) and the coverage
//! list ([`IMPLEMENTED`]).
//!
//! The enumeration covers every class that occurs in a level sequence of
//! the shipped maps (`docs/reverse-engineering/KISMET.md` census) plus a few
//! stock classes that the maps do not use but are simple and common (`Gate`,
//! `Log`, `CompareObject`, `SetInt`, `SetString`, the `PlayerDied` action,
//! `Used`/`Destroyed` events). Classes not listed map to
//! [`OpClass::Unknown`], which the interpreter runs as a generic action or
//! latent action (handler-less: outputs only), and reports in coverage.

use crate::graph::NodeKind;

macro_rules! op_classes {
    ($( $variant:ident => $name:literal ),* $(,)?) => {
        /// An interpreter class.
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub enum OpClass {
            $(
                #[doc = concat!("`", $name, "`.")]
                $variant,
            )*
            /// A class the interpreter has no special behaviour for (generic
            /// semantics by node kind).
            Unknown,
        }

        impl OpClass {
            /// Every known class, in declaration order.
            pub const ALL: &'static [OpClass] = &[$(OpClass::$variant),*];

            /// Short UE3 class name (`SeqAct_Toggle`); `"?"` for unknown.
            #[must_use]
            pub fn name(self) -> &'static str {
                match self {
                    $(OpClass::$variant => $name,)*
                    OpClass::Unknown => "?",
                }
            }

            fn from_short(short: &str) -> OpClass {
                $(
                    if short.eq_ignore_ascii_case($name) {
                        return OpClass::$variant;
                    }
                )*
                OpClass::Unknown
            }
        }
    };
}

op_classes! {
    // Sequences and variables.
    Sequence => "Sequence",
    PrefabSequence => "PrefabSequence",
    PrefabSequenceContainer => "PrefabSequenceContainer",
    VarBool => "SeqVar_Bool",
    VarInt => "SeqVar_Int",
    VarFloat => "SeqVar_Float",
    VarString => "SeqVar_String",
    VarObject => "SeqVar_Object",
    VarPlayer => "SeqVar_Player",
    VarNamed => "SeqVar_Named",
    VarRandomInt => "SeqVar_RandomInt",
    VarRandomFloat => "SeqVar_RandomFloat",
    InterpData => "InterpData",
    // Engine events.
    LevelLoaded => "SeqEvent_LevelLoaded",
    Touch => "SeqEvent_Touch",
    RemoteEvent => "SeqEvent_RemoteEvent",
    AnimNotify => "SeqEvent_AnimNotify",
    Used => "SeqEvent_Used",
    Destroyed => "SeqEvent_Destroyed",
    // ASAMU events.
    ActorInteractedWith => "SeqEvent_ActorInteractedWith",
    SavedGameStateLoaded => "SaveGameState_SeqEvent_SavedGameStateLoaded",
    CollectibleCollected => "SeqEvent_CollectibleCollected",
    NarratorEvents => "SeqEvent_NarratorEvents",
    PlayerDied => "SeqEvent_PlayerDied",
    PlayerReleasedGrapple => "SeqEvent_PlayerReleasedGrapple",
    PlayerGrappled => "SeqEvent_PlayerGrappled",
    PlayerRocketBoosted => "SeqEvent_PlayerRocketBoosted",
    PlayerLanded => "SeqEvent_PlayerLanded",
    TrackBeat => "SeqEvent_TrackBeat",
    WormEvents => "SeqEvent_WormEvents",
    CreditsEnded => "SeqEvent_CreditsEnded",
    // Engine actions.
    ActivateRemoteEvent => "SeqAct_ActivateRemoteEvent",
    AddInt => "SeqAct_AddInt",
    CameraFade => "SeqAct_CameraFade",
    CameraShake => "SeqAct_CameraShake",
    ChangeCollision => "SeqAct_ChangeCollision",
    ConsoleCommand => "SeqAct_ConsoleCommand",
    Delay => "SeqAct_Delay",
    Destroy => "SeqAct_Destroy",
    Interp => "SeqAct_Interp",
    MultiLevelStreaming => "SeqAct_MultiLevelStreaming",
    LevelStreaming => "SeqAct_LevelStreaming",
    PlayCameraAnim => "SeqAct_PlayCameraAnim",
    PlayMusicTrack => "SeqAct_PlayMusicTrack",
    PlaySound => "SeqAct_PlaySound",
    SetBool => "SeqAct_SetBool",
    SetCameraTarget => "SeqAct_SetCameraTarget",
    SetFloat => "SeqAct_SetFloat",
    SetInt => "SeqAct_SetInt",
    SetString => "SeqAct_SetString",
    SetObject => "SeqAct_SetObject",
    SetMatInstScalarParam => "SeqAct_SetMatInstScalarParam",
    SetSoundMode => "SeqAct_SetSoundMode",
    SetVelocity => "SeqAct_SetVelocity",
    Teleport => "SeqAct_Teleport",
    Toggle => "SeqAct_Toggle",
    ToggleCinematicMode => "SeqAct_ToggleCinematicMode",
    ToggleHud => "SeqAct_ToggleHUD",
    ToggleHidden => "SeqAct_ToggleHidden",
    Gate => "SeqAct_Gate",
    Log => "SeqAct_Log",
    OpenMovie => "GFxAction_OpenMovie",
    // ASAMU actions.
    AddAdaptiveTracks => "SeqAct_AddAdaptiveTracks",
    DisablePauseMenu => "SeqAct_DisablePauseMenu",
    EditMultiplierForAllTracks => "SeqAct_EditMultiplierForAllTracks",
    EditOrAddSaveString => "SeqAct_EditOrAddSaveString",
    EndTimeTrial => "SeqAct_EndTimeTrial",
    GetSaveStringValue => "SeqAct_GetSaveStringValue",
    HideTutorialPopup => "SeqAct_HideTutorialPopup",
    NarratorLine => "SeqAct_NarratorLine",
    PauseWorm => "SeqAct_PauseWorm",
    PlayerDiedAction => "SeqAct_PlayerDied",
    PlaySuitOnAnimation => "SeqAct_PlaySuitOnAnimation",
    SetAdaptiveTrackVolumeMultiplier => "SeqAct_SetAdaptiveTrackVolumeMultiplier",
    SetGameFinished => "SeqAct_SetGameFinished",
    SetLookAtTarget => "SeqAct_SetLookAtTarget",
    SetMaxGrapples => "SeqAct_SetMaxGrapples",
    SetRotationToPlayerRotation => "SeqAct_SetRotationToPlayerRotation",
    SetVelocityConeMaterial => "SeqAct_SetVelocityConeMaterial",
    ShowTitleLogo => "SeqAct_ShowTitleLogo",
    ShowTutorialPopup => "SeqAct_ShowTutorialPopup",
    ShutDownWorm => "SeqAct_ShutDownWorm",
    StartTimeTrial => "SeqAct_StartTimeTrial",
    StartWorm => "SeqAct_StartWorm",
    ToggleAttractor => "SeqAct_ToggleAttractor",
    ToggleCheckpointEnable => "SeqAct_ToggleCheckpointEnable",
    ToggleCrosshair => "SeqAct_ToggleCrosshair",
    ToggleFallingRocksActive => "SeqAct_ToggleFallingRocksActive",
    ToggleFollowCollision => "SeqAct_ToggleFollowCollision",
    ToggleGrapple => "SeqAct_ToggleGrapple",
    ToggleRestartFromCheckpointOption => "SeqAct_ToggleRestartFromCheckpointOption",
    ToggleRocketBoots => "SeqAct_ToggleRocketBoots",
    ToggleSpawnInStoryMode => "SeqAct_ToggleSpawnInStoryMode",
    ToggleStoryMode => "SeqAct_ToggleStoryMode",
    ToggleVisibleGrapple => "SeqAct_ToggleVisibleGrapple",
    ToggleZoomAvailable => "SeqAct_ToggleZoomAvailable",
    TriggerCheckpoint => "SeqAct_TriggerCheckpoint",
    UnlockAchievement => "SeqAct_UnlockASAMUAchievement",
    MenuInvoke => "SeqAction_GFx_CustomInvoke_AS3_Menu",
    // Conditions.
    CompareBool => "SeqCond_CompareBool",
    CompareFloat => "SeqCond_CompareFloat",
    CompareInt => "SeqCond_CompareInt",
    CompareObject => "SeqCond_CompareObject",
    Increment => "SeqCond_Increment",
    IsPie => "SeqCond_IsPIE",
    IsTimeTrial => "SeqCond_IsTimeTrial",
    // Editor-only objects.
    Frame => "SequenceFrame",
    FrameWrapped => "SequenceFrameWrapped",
}

/// Classes with interpreter behaviour (every [`OpClass`] except
/// [`OpClass::Unknown`]).
pub const IMPLEMENTED: &[OpClass] = OpClass::ALL;

impl OpClass {
    /// The class of a node from its qualified class path and kind.
    #[must_use]
    pub fn from_class(class: &str, kind: NodeKind) -> OpClass {
        let short = class.rsplit('.').next().unwrap_or(class);
        let c = OpClass::from_short(short);
        if c == OpClass::Unknown && kind == NodeKind::Sequence {
            return OpClass::Sequence;
        }
        c
    }

    /// True for the event classes.
    #[must_use]
    pub fn is_event(self) -> bool {
        matches!(
            self,
            OpClass::LevelLoaded
                | OpClass::Touch
                | OpClass::RemoteEvent
                | OpClass::AnimNotify
                | OpClass::Used
                | OpClass::Destroyed
                | OpClass::ActorInteractedWith
                | OpClass::SavedGameStateLoaded
                | OpClass::CollectibleCollected
                | OpClass::NarratorEvents
                | OpClass::PlayerDied
                | OpClass::PlayerReleasedGrapple
                | OpClass::PlayerGrappled
                | OpClass::PlayerRocketBoosted
                | OpClass::PlayerLanded
                | OpClass::TrackBeat
                | OpClass::WormEvents
                | OpClass::CreditsEnded
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classes_map_by_short_name_case_insensitively() {
        assert_eq!(
            OpClass::from_class("Engine.SeqAct_Toggle", NodeKind::Action),
            OpClass::Toggle
        );
        assert_eq!(
            OpClass::from_class("asamu.seqact_togglegrapple", NodeKind::Action),
            OpClass::ToggleGrapple
        );
        assert_eq!(
            OpClass::from_class("Foo.Bar", NodeKind::Sequence),
            OpClass::Sequence
        );
        assert_eq!(
            OpClass::from_class("Foo.Bar", NodeKind::Action),
            OpClass::Unknown
        );
        // Names are unique.
        let mut names: Vec<&str> = OpClass::ALL.iter().map(|c| c.name()).collect();
        names.sort_unstable();
        let n = names.len();
        names.dedup();
        assert_eq!(names.len(), n);
        assert!(OpClass::PlayerGrappled.is_event());
        assert!(!OpClass::Toggle.is_event());
    }
}
