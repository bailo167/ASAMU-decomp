//! Curated Ghidra anchors for reproducing player / grapple behaviour.
//!
//! The *selection* and the one-line rationale are ours (based on the role these
//! UE3/UDK functions play in the stock engine); the *facts* reported for each
//! anchor — presence, address, signature, compilation unit, category — come
//! from the binary. Anchors that are not present are reported as missing, which
//! is evidence too.

use serde::Serialize;

use crate::analysis::Analysis;
use crate::demangle::strip_template_args;
use crate::macho::SymKind;

/// Core gameplay classes (movement, control, camera, input, Kismet, Matinee,
/// UDK/GameFramework player layer, and the one ASAMU native class). Used to
/// rank keyword hits and to select `classes_of_interest`.
pub const GAMEPLAY_CLASSES: &[&str] = &[
    "UObject",
    "AActor",
    "APawn",
    "AController",
    "APlayerController",
    "ACamera",
    "UCameraModifier",
    "UPlayerInput",
    "UInput",
    "AWorldInfo",
    "APhysicsVolume",
    "AKActor",
    "ARB_ConstraintActor",
    "USequence",
    "USequenceOp",
    "USequenceEvent",
    "USeqAct_Latent",
    "USeqAct_Interp",
    "AMatineeActor",
    "AInterpActor",
    "AGamePawn",
    "AGamePlayerController",
    "AGamePlayerCamera",
    "UGameThirdPersonCamera",
    "UGameThirdPersonCameraMode",
    "AUDKPawn",
    "AUDKPlayerController",
    "UUDKPlayerInput",
    "UASAMUSystemSettingsManager",
];

/// One curated anchor.
#[derive(Debug, Clone, Copy)]
pub struct AnchorSpec {
    /// Topic group.
    pub topic: &'static str,
    /// Qualified name (`Class::function`, no parameters).
    pub symbol: &'static str,
    /// Why it matters.
    pub why: &'static str,
}

const fn a(topic: &'static str, symbol: &'static str, why: &'static str) -> AnchorSpec {
    AnchorSpec { topic, symbol, why }
}

/// The curated list, in output order.
pub const ANCHORS: &[AnchorSpec] = &[
    // --- pawn movement physics -------------------------------------------
    a(
        "movement",
        "APawn::performPhysics",
        "per-tick pawn physics entry; dispatches on the Physics mode (EPhysics)",
    ),
    a(
        "movement",
        "APawn::startNewPhysics",
        "re-dispatch to the phys* routine for the remaining delta time / iterations",
    ),
    a(
        "movement",
        "APawn::physWalking",
        "PHYS_Walking ground movement: acceleration, floor tracking, ledge handling",
    ),
    a(
        "movement",
        "APawn::physFalling",
        "PHYS_Falling airborne movement: gravity, air control, landing detection — jump and swing arcs",
    ),
    a(
        "movement",
        "APawn::NewFallVelocity",
        "gravity integration step used by physFalling",
    ),
    a(
        "movement",
        "APawn::CalcVelocity",
        "acceleration / friction / braking → Velocity (ground speed model)",
    ),
    a(
        "movement",
        "APawn::stepUp",
        "step-up over small obstacles while walking",
    ),
    a(
        "movement",
        "APawn::processLanded",
        "landing transition (Landed event, back to PHYS_Walking)",
    ),
    a(
        "movement",
        "APawn::processHitWall",
        "wall impact handling (HitWall / NotifyHitWall events)",
    ),
    a(
        "movement",
        "APawn::SmoothHitWall",
        "sliding along walls after an impact",
    ),
    a(
        "movement",
        "APawn::physicsRotation",
        "rotation toward DesiredRotation using RotationRate",
    ),
    a(
        "movement",
        "APawn::setPhysics",
        "physics-mode transitions for pawns",
    ),
    a(
        "movement",
        "APawn::CalculateSlopeSlide",
        "slope sliding response on steep floors",
    ),
    a(
        "movement",
        "APawn::MaxSpeedModifier",
        "speed scaling (walking/crouch modifiers)",
    ),
    a(
        "movement",
        "APawn::physFlying",
        "PHYS_Flying (free 3D movement, debug/fly modes)",
    ),
    a(
        "movement",
        "APawn::physSwimming",
        "PHYS_Swimming (water volumes)",
    ),
    a(
        "movement",
        "APawn::physSpider",
        "PHYS_Spider (surface-relative movement; candidate for wall/ceiling locomotion)",
    ),
    a(
        "movement",
        "APawn::physLadder",
        "PHYS_Ladder (constrained climbing)",
    ),
    a(
        "movement",
        "APawn::GetFallDuration",
        "time-to-land estimate used by AI/script",
    ),
    a(
        "movement",
        "AActor::moveSmooth",
        "collision-aware move with wall sliding used by the phys* routines",
    ),
    a("movement", "AActor::stepUp", "actor-level step-up helper"),
    a(
        "movement",
        "AActor::performPhysics",
        "per-tick physics entry for non-pawn actors",
    ),
    a(
        "movement",
        "AActor::physFalling",
        "falling for non-pawn actors (projectile-like bodies)",
    ),
    a(
        "movement",
        "AActor::physProjectile",
        "PHYS_Projectile ballistic movement",
    ),
    a(
        "movement",
        "AActor::physInterpolating",
        "PHYS_Interpolating — Matinee-driven movers / InterpActors (moving platforms)",
    ),
    a(
        "movement",
        "AActor::physRigidBody",
        "PHYS_RigidBody — PhysX-simulated actors",
    ),
    a(
        "movement",
        "AActor::setPhysics",
        "physics-mode transitions for actors",
    ),
    a(
        "movement",
        "AActor::SetBase",
        "attach to a moving base (riding platforms)",
    ),
    a("movement", "AActor::FindBase", "floor/base detection"),
    a(
        "movement",
        "AWorldInfo::GetGravityZ",
        "world gravity source",
    ),
    a(
        "movement",
        "APhysicsVolume::GetGravityZ",
        "per-volume gravity override",
    ),
    a(
        "movement",
        "APhysicsVolume::GetZoneVelocityForActor",
        "volume-imposed velocity (currents, wind)",
    ),
    // --- collision ----------------------------------------------------------
    a(
        "collision",
        "UWorld::MoveActor",
        "swept move with collision response (FCheckResult); core of every movement step",
    ),
    a(
        "collision",
        "UWorld::SingleLineCheck",
        "line/extent trace — what script Trace() uses (e.g. grapple target acquisition)",
    ),
    a("collision", "UWorld::MultiLineCheck", "multi-hit trace"),
    a(
        "collision",
        "UWorld::FarMoveActor",
        "teleport/placement (respawn, checkpoints)",
    ),
    // --- tick order -----------------------------------------------------------
    a(
        "tick",
        "UWorld::Tick",
        "frame tick: actor tick groups, physics scene step",
    ),
    a(
        "tick",
        "AActor::Tick",
        "actor tick → TickAuthoritative/TickSimulated",
    ),
    a(
        "tick",
        "AActor::TickAuthoritative",
        "authoritative tick calling performPhysics",
    ),
    a("tick", "APawn::Tick", "pawn tick"),
    a("tick", "APawn::TickSimulated", "simulated-proxy pawn tick"),
    a("tick", "AController::Tick", "controller tick"),
    a(
        "tick",
        "APlayerController::Tick",
        "player controller tick (drives PlayerTick/PlayerInput in script)",
    ),
    // --- script-callable movement natives (exec thunks) -----------------------
    a(
        "script-natives",
        "AActor::execTrace",
        "script Trace() native",
    ),
    a(
        "script-natives",
        "AActor::execFastTrace",
        "script FastTrace() native",
    ),
    a(
        "script-natives",
        "AActor::execSetPhysics",
        "script SetPhysics() native",
    ),
    a("script-natives", "AActor::execMove", "script Move() native"),
    a(
        "script-natives",
        "AActor::execMoveSmooth",
        "script MoveSmooth() native",
    ),
    a(
        "script-natives",
        "AActor::execSetLocation",
        "script SetLocation() native",
    ),
    a(
        "script-natives",
        "AActor::execSetBase",
        "script SetBase() native",
    ),
    a(
        "script-natives",
        "AActor::execSetCollision",
        "script SetCollision() native",
    ),
    a(
        "script-natives",
        "AActor::execSuggestTossVelocity",
        "script SuggestTossVelocity() — ballistic launch solver",
    ),
    a(
        "script-natives",
        "AActor::execCalculateMinSpeedTrajectory",
        "script CalculateMinSpeedTrajectory() — trajectory solver",
    ),
    a(
        "script-natives",
        "APawn::execSuggestJumpVelocity",
        "script SuggestJumpVelocity()",
    ),
    a(
        "script-natives",
        "APawn::execGetFallDuration",
        "script GetFallDuration()",
    ),
    // --- UDK pawn / controller overrides --------------------------------------
    a(
        "udk",
        "AUDKPawn::performPhysics",
        "UDK override of the pawn physics entry",
    ),
    a(
        "udk",
        "AUDKPawn::physFalling",
        "UDK override of falling; how it differs from APawn::physFalling is to be established in Ghidra",
    ),
    a(
        "udk",
        "AUDKPawn::CalcVelocity",
        "UDK override of the velocity model (differences vs APawn::CalcVelocity to be established)",
    ),
    a(
        "udk",
        "AUDKPawn::physicsRotation",
        "UDK override of pawn rotation",
    ),
    a(
        "udk",
        "AUDKPawn::setPhysics",
        "UDK override of physics transitions",
    ),
    a("udk", "AUDKPawn::GetGravityZ", "UDK gravity override hook"),
    a(
        "udk",
        "AUDKPawn::CalculateSlopeSlide",
        "UDK slope-slide override",
    ),
    a("udk", "AUDKPawn::TickSpecial", "UDK pawn per-tick extras"),
    a(
        "udk",
        "AUDKPawn::UpdateEyeHeight",
        "eye-height smoothing (camera bob/landing dip)",
    ),
    a(
        "udk",
        "AUDKPawn::SuggestJumpVelocity",
        "UDK jump solver override",
    ),
    a(
        "udk",
        "AUDKPlayerController::Tick",
        "UDK player controller tick",
    ),
    a(
        "udk",
        "AUDKPlayerController::MoveWithInterpMoveTrack",
        "player movement driven by a Matinee move track",
    ),
    // --- camera ---------------------------------------------------------------
    a(
        "camera",
        "ACamera::ApplyCameraModifiers",
        "camera modifier stack (shakes, FOV effects)",
    ),
    a(
        "camera",
        "ACamera::SetViewTarget",
        "view-target switching with blend parameters",
    ),
    a(
        "camera",
        "ACamera::AssignViewTarget",
        "view-target assignment",
    ),
    a("camera", "ACamera::PlayCameraAnim", "camera animations"),
    a(
        "camera",
        "ACamera::ApplyAnimToCamera",
        "applies a camera animation to the POV",
    ),
    a(
        "camera",
        "UCameraModifier::ModifyCamera",
        "base camera modifier hook",
    ),
    a(
        "camera",
        "UCameraModifier_CameraShake::UpdateCameraShake",
        "camera shake evaluation",
    ),
    a(
        "camera",
        "AGamePlayerCamera::AdjustFOVForViewport",
        "FOV correction for aspect ratio",
    ),
    a(
        "camera",
        "UGameThirdPersonCamera::PlayerUpdateCamera",
        "GameFramework third-person camera update",
    ),
    a(
        "camera",
        "UGameThirdPersonCamera::PreventCameraPenetration",
        "camera collision avoidance",
    ),
    // --- input ----------------------------------------------------------------
    a(
        "input",
        "UInput::InputKey",
        "key event dispatch to bindings",
    ),
    a("input", "UInput::Tick", "input axis accumulation per frame"),
    a(
        "input",
        "UPlayerInput::InputKey",
        "player input key handling",
    ),
    a(
        "input",
        "UPlayerInput::InputAxis",
        "player input axis handling (mouse/gamepad look)",
    ),
    // --- Kismet ---------------------------------------------------------------
    a(
        "kismet",
        "USequence::ExecuteActiveOps",
        "Kismet scheduler: runs active sequence ops each tick",
    ),
    a("kismet", "USequence::UpdateOp", "sequence update"),
    a(
        "kismet",
        "USequence::QueueSequenceOp",
        "queues an op for activation",
    ),
    a(
        "kismet",
        "USequence::InitializeSequence",
        "sequence initialisation at level start",
    ),
    a("kismet", "USequence::BeginPlay", "sequence BeginPlay"),
    a(
        "kismet",
        "USequenceOp::ActivateOutputLink",
        "fires an output link",
    ),
    a(
        "kismet",
        "USequenceEvent::CheckActivate",
        "event activation (Touch/Used/LevelLoaded …)",
    ),
    a("kismet", "USeqAct_Latent::UpdateOp", "latent action update"),
    // --- Matinee --------------------------------------------------------------
    a(
        "matinee",
        "USeqAct_Interp::Activated",
        "Matinee action activation",
    ),
    a("matinee", "USeqAct_Interp::Play", "Matinee playback start"),
    a(
        "matinee",
        "USeqAct_Interp::StepInterp",
        "advances a Matinee by dt",
    ),
    a(
        "matinee",
        "USeqAct_Interp::UpdateInterp",
        "evaluates all tracks at a time",
    ),
    a(
        "matinee",
        "USeqAct_Interp::UpdateOp",
        "Matinee per-tick update",
    ),
    a(
        "matinee",
        "UInterpTrackMove::GetKeyTransformAtTime",
        "move-track sampling (moving platforms, cutscene movers)",
    ),
    a(
        "matinee",
        "UInterpTrackMove::GetKeyframePosition",
        "move-track key position",
    ),
    a(
        "matinee",
        "UInterpTrackInstMove::CalcInitialTransform",
        "relative-move initial transform",
    ),
    a(
        "matinee",
        "AMatineeActor::TickSpecial",
        "replicated Matinee actor tick",
    ),
    a(
        "matinee",
        "AInterpActor::TickSpecial",
        "InterpActor (mover) tick",
    ),
    // --- rigid-body constraints (possible rope/grapple building blocks) --------
    a(
        "constraints",
        "ARB_ConstraintActor::InitConstraint",
        "runtime PhysX constraint between actors",
    ),
    a(
        "constraints",
        "URB_ConstraintInstance::InitConstraint",
        "constraint instance creation",
    ),
    a(
        "constraints",
        "AKActor::physRigidBody",
        "KActor rigid-body physics",
    ),
];

/// Script events (FName globals generated by `AutoGenerateNames`) that native
/// movement/collision code raises into UnrealScript.
pub const SCRIPT_EVENTS: &[&str] = &[
    "ENGINE_Landed",
    "ENGINE_Falling",
    "ENGINE_HitWall",
    "ENGINE_Bump",
    "ENGINE_Touch",
    "ENGINE_UnTouch",
    "ENGINE_BaseChange",
    "ENGINE_PhysicsVolumeChange",
    "ENGINE_MayFall",
    "ENGINE_LongFall",
    "ENGINE_FellOutOfWorld",
    "ENGINE_OutsideWorldBounds",
    "ENGINE_EncroachingOn",
    "ENGINE_EncroachedBy",
    "ENGINE_RanInto",
    "ENGINE_StuckOnPawn",
    "ENGINE_NotifyLanded",
    "ENGINE_NotifyPostLanded",
    "ENGINE_NotifyHitWall",
    "ENGINE_NotifyFallingHitWall",
    "ENGINE_NotifyJumpApex",
    "ENGINE_NotifyBump",
    "ENGINE_NotifyPhysicsVolumeChange",
    "ENGINE_SetWalking",
    "ENGINE_StartCrouch",
    "ENGINE_EndCrouch",
    "ENGINE_PlayerTick",
    "ENGINE_PlayerInput",
    "ENGINE_UpdateCamera",
    "ENGINE_GetPlayerViewPoint",
    "ENGINE_GetActorEyesViewPoint",
    "ENGINE_GetFOVAngle",
    "ENGINE_BecomeViewTarget",
    "ENGINE_InterpolationStarted",
    "ENGINE_InterpolationFinished",
    "ENGINE_InterpolationChanged",
    "ENGINE_MoverFinished",
    "ENGINE_Activated",
    "ENGINE_Deactivated",
    "ENGINE_RigidBodyCollision",
    "ENGINE_ConstraintBrokenNotify",
    "UDKBASE_StoppedFalling",
    "UDKBASE_StuckFalling",
    "UDKBASE_UpdateEyeHeight",
    "UDKBASE_PhysicsVolumeChanged",
    "ASAMU_",
];

/// One resolved anchor symbol.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ResolvedAnchor {
    /// Topic.
    pub topic: String,
    /// `Class::function`.
    pub symbol: String,
    /// Address (hex).
    pub address: String,
    /// Demangled signature.
    pub signature: String,
    /// Sanitized compilation unit.
    pub unit: Option<String>,
    /// Rationale.
    pub why: String,
}

/// Anchor report.
#[derive(Debug, Clone, Default, Serialize, PartialEq, Eq)]
pub struct AnchorReport {
    /// Resolved anchors (one row per matching symbol, so overloads appear twice).
    pub resolved: Vec<ResolvedAnchor>,
    /// Curated anchors that are not present in the binary.
    pub missing: Vec<String>,
    /// Script events present (FName globals).
    pub script_events_present: Vec<String>,
    /// Script events looked for but absent.
    pub script_events_missing: Vec<String>,
}

/// Resolve the curated anchors against the analysis.
pub fn resolve(an: &Analysis) -> AnchorReport {
    use std::collections::{HashMap, HashSet};
    let wanted: HashSet<&str> = ANCHORS.iter().map(|s| s.symbol).collect();
    // Index text symbols by template-stripped qualified name (only wanted ones).
    let mut by_name: HashMap<String, Vec<usize>> = HashMap::new();
    for (i, e) in an.entries.iter().enumerate() {
        let Some(sym) = an.symbol(e) else { continue };
        if sym.kind != SymKind::Text {
            continue;
        }
        let Some(q) = e.qualified.as_deref() else {
            continue;
        };
        let key = if q.contains('<') {
            strip_template_args(q)
        } else {
            q.to_string()
        };
        if wanted.contains(key.as_str()) {
            by_name.entry(key).or_default().push(i);
        }
    }
    let mut rep = AnchorReport::default();
    for spec in ANCHORS {
        let Some(indices) = by_name.get(spec.symbol) else {
            rep.missing.push(spec.symbol.to_string());
            continue;
        };
        let mut rows: Vec<ResolvedAnchor> = indices
            .iter()
            .filter_map(|&i| {
                let e = an.entries.get(i)?;
                let sym = an.symbol(e)?;
                Some(ResolvedAnchor {
                    topic: spec.topic.to_string(),
                    symbol: spec.symbol.to_string(),
                    address: format!("0x{:x}", sym.value),
                    signature: an.display(e).to_string(),
                    unit: an.unit(e).map(|u| u.rel_path.clone()),
                    why: spec.why.to_string(),
                })
            })
            .collect();
        rows.sort_by(|x, y| x.address.cmp(&y.address));
        rep.resolved.extend(rows);
    }
    // Script events.
    let mut fnames: Vec<&str> = an
        .entries
        .iter()
        .filter(|e| e.full.is_none())
        .filter_map(|e| an.symbol(e))
        .filter(|s| s.kind != SymKind::Undefined)
        .map(|s| crate::demangle::strip_macho_underscore(&s.raw))
        .collect();
    fnames.sort_unstable();
    for ev in SCRIPT_EVENTS {
        if ev.ends_with('_') {
            // Prefix probe: any FName with this package prefix.
            let n = fnames.iter().filter(|f| f.starts_with(ev)).count();
            if n > 0 {
                rep.script_events_present.push(format!("{ev}* ({n})"));
            } else {
                rep.script_events_missing.push(format!("{ev}*"));
            }
            continue;
        }
        if fnames.binary_search(ev).is_ok() {
            rep.script_events_present.push((*ev).to_string());
        } else {
            rep.script_events_missing.push((*ev).to_string());
        }
    }
    rep
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn anchor_list_is_well_formed() {
        let mut seen = std::collections::BTreeSet::new();
        for spec in ANCHORS {
            assert!(spec.symbol.contains("::"), "{}", spec.symbol);
            assert!(!spec.symbol.contains('('), "{}", spec.symbol);
            assert!(!spec.why.is_empty());
            assert!(seen.insert(spec.symbol), "duplicate {}", spec.symbol);
        }
        for ev in SCRIPT_EVENTS {
            assert!(ev.contains('_'));
        }
    }
}
