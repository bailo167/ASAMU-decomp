//! The original grapple gun (`asamu.GrappleGun`), reimplemented from the
//! behavioural specification `docs/reverse-engineering/GRAPPLE.md` (rule ids
//! G-… below) in our own words; no script text is reproduced here.
//!
//! The grapple is **not a rope** (GRAPPLE.md finding 1): attaching switches
//! the pawn to native `PHYS_Flying` (no gravity, drag, speed capped at
//! `AirSpeed` = `fGrappleAccel` = 2000 uu/s; [`crate::ue3_movement`]) and the
//! gun adds a pull of `10⁷ / d` uu/s² toward the anchor on every gun tick,
//! after the pawn's physics. Closer than `fGrappleReleaseDistance` (200 uu)
//! the velocity is halved and the grapple released.
//!
//! The gun's run-time state is [`GunState`] in
//! [`crate::pawn::PawnScript::gun`]; [`crate::pawn`] calls the entry points
//! of this module in the original's frame order (GRAPPLE.md G-TM-2):
//!
//! 1. input events: [`fire_button`] (press → `StartFire`, release →
//!    `StopFire`), synchronously, before any actor ticks (G-IN-5);
//! 2. map actors: [`apply_pending_crystal_refill`] (the recharge crystal's
//!    `GrappledState` begin code, G-CT-4);
//! 3. … controller, pawn (physics), power jump, rocket boots …;
//! 4. the gun: [`tick`] (crosshair, anchor follow, pull and proximity
//!    release, released-flag reset, counter clamp, then its timers: instant
//!    release, refire check, hand-hiding).
//!
//! Releases from outside the tick (death, story mode) use [`release`].
//! Kismet actions and execs: [`set_max_grapples`], [`enable_grapple`],
//! [`unlimited_grapples`], [`hide_grapple_gun`], [`reset_grapple_amount`].
//!
//! # Weapon firing state machine (G-IN-2, stock `Engine.Weapon`)
//!
//! A press marks fire mode 0 pending. In `Active` the weapon enters
//! `WeaponFiring`, which fires at once and starts a looping refire-check
//! timer of `FireInterval` (0.1 s). Each refire check fires again while the
//! mode is still pending (a no-op here because of the latch, G-IN-3) and
//! otherwise returns to `Active`. A press while still `WeaponFiring` only
//! marks the mode pending and is served by the next refire check. The equip
//! delay at level start (`EquipTime` 0.33 s) is **not** modelled: the
//! weapon starts `Active` (GRAPPLE.md open question 6, UNKNOWN).
//!
//! # Surfaces
//!
//! The trace hits a [`crate::world::Surface`]: `grapple_able == false` is the
//! tag `NotGrappleAble`; the class decides the interfaces
//! (`ASAMUGrappleAbleInterface`, `ASAMUReleaseGrappleInstantInterface`) and
//! whether the anchor follows the target (`InterpActor`, G-AT-7); the tag
//! decides the top/bottom-only rule and `grappleInteractable`.

use glam::Vec3;
use serde::{Deserialize, Serialize};

use crate::events::SimEvent;
use crate::params::{GrappleGunParams, PlayerParams};
use crate::pawn::{self, PawnStateName, ScriptConstant, poll_timer};
use crate::rocket_boots::BootsStateName;
use crate::sim::{PlayerState, StepEvents};
use crate::ue3_movement::safe_normal;
use crate::world::{ActorClass, ActorTag, CollisionWorld, Surface};

// ---------------------------------------------------------------------------
// Literal constants of the original script code (provenance: ScriptCode).
// ---------------------------------------------------------------------------

/// `asamu.GrappleGun`.
pub const SCRIPT_CLASS_GUN: &str = "asamu.GrappleGun";
/// `asamu.GrappleGunLightManager` (owns the counter clamp).
pub const SCRIPT_CLASS_LIGHT_MANAGER: &str = "asamu.GrappleGunLightManager";

/// Pull numerator: the pull adds `dt · unit(A − P) · PULL_NUMERATOR / (d /
/// PULL_DISTANCE_SCALE)` to the velocity, i.e. `10⁷/d` uu/s² (G-PH-2).
/// ScriptCode `asamu.GrappleGun.UpdateGrapple`. CONFIRMED (src+bc).
pub const PULL_NUMERATOR: f32 = 10_000.0;
/// See [`PULL_NUMERATOR`]. ScriptCode `asamu.GrappleGun.UpdateGrapple`.
/// CONFIRMED (src+bc).
pub const PULL_DISTANCE_SCALE: f32 = 1_000.0;
/// The proximity release divides the velocity by this (G-RL-2).
/// ScriptCode `asamu.GrappleGun.UpdateGrapple`. CONFIRMED (src+bc).
pub const PROXIMITY_VELOCITY_DIVISOR: f32 = 2.0;
/// The crosshair trace reaches `fMaxDistance` + this (G-TG-2).
/// ScriptCode `asamu.GrappleGun.CheckForRange`. CONFIRMED (src+bc).
pub const CROSSHAIR_EXTRA_RANGE: f32 = 1_000.0;
/// Capacity used for "unlimited" (`SetMaxGrapples(n ≤ −1)`, G-CT-2).
/// ScriptCode `asamu.GrappleGun.SetMaxGrapples`. CONFIRMED (src+bc).
pub const UNLIMITED_GRAPPLES: i32 = 32_767;
/// The light manager clamps the used count into `[0, this]` every gun tick,
/// so capacities ≥ 4 are effectively unlimited (G-CT-6). ScriptCode
/// `asamu.GrappleGunLightManager.UpdateLights`. CONFIRMED (src+bc).
pub const COUNTER_CLAMP: i32 = 3;
/// `HideGrappleGun` with animation and visibility off hides the hand after
/// this timer (G-AC-3). ScriptCode `asamu.GrappleGun.HideGrappleGun`.
/// CONFIRMED (src).
pub const HIDE_HAND_DELAY: f32 = 0.3;

/// Safety bound on the number of times a looping timer fires in one tick.
/// Not a gameplay value: it only matters for absurd `dt`/rate pairs.
pub const MAX_TIMER_FIRES_PER_TICK: u32 = 64;

macro_rules! gun_constant {
    ($name:ident, $class:expr, $unit:expr, $function:expr, $meaning:expr) => {
        ScriptConstant {
            name: stringify!($name),
            value: $name as f64,
            unit: $unit,
            class: $class,
            function: $function,
            meaning: $meaning,
        }
    };
}

/// Every script-code constant of this module (for `docs/PARITY.md`).
pub const GUN_SCRIPT_CONSTANTS: &[ScriptConstant] = &[
    gun_constant!(
        PULL_NUMERATOR,
        SCRIPT_CLASS_GUN,
        "factor",
        "UpdateGrapple",
        "pull dt x unit(A-P) x 10000 / (d / 1000): 10^7/d uu/s^2"
    ),
    gun_constant!(
        PULL_DISTANCE_SCALE,
        SCRIPT_CLASS_GUN,
        "uu",
        "UpdateGrapple",
        "distance divisor of the pull"
    ),
    gun_constant!(
        PROXIMITY_VELOCITY_DIVISOR,
        SCRIPT_CLASS_GUN,
        "factor",
        "UpdateGrapple",
        "velocity divided by this at the proximity release"
    ),
    gun_constant!(
        CROSSHAIR_EXTRA_RANGE,
        SCRIPT_CLASS_GUN,
        "uu",
        "CheckForRange",
        "crosshair trace length fMaxDistance + this"
    ),
    gun_constant!(
        UNLIMITED_GRAPPLES,
        SCRIPT_CLASS_GUN,
        "count",
        "SetMaxGrapples",
        "capacity for SetMaxGrapples(n <= -1)"
    ),
    gun_constant!(
        COUNTER_CLAMP,
        SCRIPT_CLASS_LIGHT_MANAGER,
        "count",
        "UpdateLights",
        "used count clamped to [0, 3] every gun tick"
    ),
    gun_constant!(
        HIDE_HAND_DELAY,
        SCRIPT_CLASS_GUN,
        "s",
        "HideGrappleGun",
        "animated hide with visibility off: hand hidden after this"
    ),
];

// ---------------------------------------------------------------------------
// State.
// ---------------------------------------------------------------------------

/// The weapon's firing state (stock `Engine.Weapon`, G-IN-2).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WeaponState {
    /// `Active`: a press fires at once.
    #[default]
    Active,
    /// `WeaponFiring`: the refire-check timer runs; presses only mark the
    /// mode pending.
    Firing,
}

/// The current attachment (`bIsGrappling`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Attachment {
    /// Actor id of the hit actor, if the world tracks it.
    pub target: Option<u32>,
    /// Class of the hit actor.
    pub class: ActorClass,
    /// The target implements `ASAMUGrappleAbleInterface`: its `UnGrappled`
    /// handler runs at the release (`interactableGrappledActor`).
    pub handler_target: bool,
}

/// The anchor helper (`GrappleGunHitLocActor`) based on a moving target
/// (`bGrappledInterpActor`, G-AT-7/8). It keeps following after a release
/// (cosmetic then).
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct AnchorFollow {
    /// The base actor.
    pub actor: u32,
    /// Helper location minus the base's location when it was placed, UU
    /// (translation-only basing; rotation with the base is TENTATIVE and
    /// not modelled).
    pub offset: Vec3,
    /// Current helper location, UU.
    pub helper: Vec3,
}

/// Run-time state of the grapple gun.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct GunState {
    /// The pawn has the gun (`ASAMUGameInfo.AddDefaultInventory`); without
    /// gun parameters it never fires.
    pub spawned: bool,
    /// `bCanGrapple`: the fire latch (G-IN-3).
    pub can_grapple: bool,
    /// `bHasGrappled`: an attempt was evaluated during this press (G-AC-1).
    pub has_grappled: bool,
    /// The attachment while attached.
    pub attached: Option<Attachment>,
    /// `bReleasedGrapple`: set by a release, cleared at the next gun tick
    /// (G-RL-6).
    pub released: bool,
    /// `iMaxGrapples`: capacity (G-CT-1/2).
    pub max_grapples: i32,
    /// `iTimesGrappled`: grapples used since the last refill.
    pub times_grappled: i32,
    /// `iLastMaxGrapples`: capacity remembered by `UnlimitedGrapples(true)`.
    pub last_max_grapples: i32,
    /// Firing state of the weapon.
    pub weapon: WeaponState,
    /// Fire mode 0 is pending (stock `PendingFire`).
    pub pending_fire: bool,
    /// Count of the looping refire-check timer while `WeaponFiring`, s.
    pub refire_timer: Option<f32>,
    /// Count of the one-shot `ReleaseInstantTimed` timer, s. A release does
    /// not clear it (it is cleared only when it fires).
    pub instant_release_timer: Option<f32>,
    /// Count of the one-shot hand-hiding timer (G-AC-3), s.
    pub hide_hand_timer: Option<f32>,
    /// `bGrappleGunHidden`: the hand is hidden with visibility off (G-AC-3).
    pub hand_hidden: bool,
    /// `vDistance`: pawn-centre-to-anchor distance measured by the last
    /// gun tick while attached (G-PH-2; read by the rocket boots, G-IX-3).
    pub distance: f32,
    /// `vGrappleLocation`: the anchor while attached (kept afterwards).
    pub grapple_location: Vec3,
    /// The anchor helper's base, while it follows one.
    pub follow: Option<AnchorFollow>,
    /// The HUD crosshair says "grapple-able" (G-TG-2; HUD only).
    pub crosshair: bool,
    /// A charged recharge crystal was grappled: its `GrappledState` begin
    /// code refills the budget at the next map-actor run (G-CT-4).
    pub pending_crystal_refill: bool,
}

impl GunState {
    /// The gun as spawned with `params` (`PostBeginPlay` / CDO values).
    #[must_use]
    pub fn spawn(params: &GrappleGunParams) -> Self {
        Self {
            spawned: true,
            can_grapple: params.initial_can_grapple.value,
            max_grapples: params.initial_max_grapples.value,
            ..Self::default()
        }
    }

    /// The anchor while attached.
    #[must_use]
    pub fn anchor(&self) -> Option<Vec3> {
        self.attached.map(|_| self.grapple_location)
    }

    /// `true` while attached.
    #[must_use]
    pub fn is_attached(&self) -> bool {
        self.attached.is_some()
    }

    /// Grapples left before the next refill (`max − used`, at least 0).
    #[must_use]
    pub fn grapples_left(&self) -> i32 {
        self.max_grapples.saturating_sub(self.times_grappled).max(0)
    }

    /// `true` if every float is finite.
    #[must_use]
    pub fn is_finite(&self) -> bool {
        self.distance.is_finite()
            && self.grapple_location.is_finite()
            && self.refire_timer.is_none_or(f32::is_finite)
            && self.instant_release_timer.is_none_or(f32::is_finite)
            && self.hide_hand_timer.is_none_or(f32::is_finite)
            && self
                .follow
                .is_none_or(|f| f.offset.is_finite() && f.helper.is_finite())
    }
}

// ---------------------------------------------------------------------------
// Events.
// ---------------------------------------------------------------------------

/// What an evaluated fire did (GRAPPLE.md §5 decision order).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FireOutcome {
    /// Story mode or zoom: no grapple (G-AC-0); `interacted` when an
    /// interactable within `interactRange` of the pawn centre was hit.
    StoryMode {
        /// An `InteractWith` request was raised.
        interacted: bool,
    },
    /// Capacity ≤ 0: nothing at all (G-AC-1).
    Silent,
    /// Fail sound: already attempted this press, out of grapples, nothing
    /// hit, `NotGrappleAble` (G-AC-1) or out of range (G-AC-5).
    Failed,
    /// Already attached, released in this gun-tick window, or the hand is
    /// hidden: nothing happens but the press is used (G-AC-2).
    Consumed,
    /// Top/bottom-only face rule failed: silent, before the press is marked
    /// used (G-AC-4).
    WrongFace,
    /// Attached (§6).
    Attached,
}

/// Why the grapple was released (GRAPPLE.md §8).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReleaseReason {
    /// Fire button released (`StopFire`, G-RL-4).
    Button,
    /// Closer than `fGrappleReleaseDistance` to the anchor; the velocity was
    /// halved first (G-RL-2).
    Proximity,
    /// The 0.05 s timer of a release-instant target (G-RL-3).
    InstantTimer,
    /// Death (`ASAMUPawn.PlayerDied`, G-RL-5).
    Death,
    /// Entering story mode (G-RL-5).
    Story,
    /// Another caller (e.g. a Kismet-driven reset).
    External,
}

/// An attach.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct GunAttach {
    /// The anchor (the impact's hit location), UU.
    pub anchor: Vec3,
    /// What was hit.
    pub surface: Surface,
    /// The target is release-instant (the 0.05 s timer started).
    pub instant_release: bool,
    /// The fire came from the refire-check timer (gun tick) rather than the
    /// press itself.
    pub from_refire: bool,
}

/// A release.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct GunRelease {
    /// Why.
    pub reason: ReleaseReason,
    /// Velocity handed over (after the proximity halving), UU/s.
    pub velocity: Vec3,
}

/// Grapple-gun events of one tick (at most one attach and one release).
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct GunEvents {
    /// The evaluated fire, if any (a press with the latch closed evaluates
    /// nothing).
    pub fire: Option<FireOutcome>,
    /// An attach.
    pub attached: Option<GunAttach>,
    /// A release.
    pub released: Option<GunRelease>,
}

// ---------------------------------------------------------------------------
// Trace and acceptance.
// ---------------------------------------------------------------------------

/// The impact of the fire trace (one impact: non-blocking triggers are
/// treated as transparent, GRAPPLE.md G-TG-1 / open question 1).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Impact {
    /// Trace start (the view location, eye height plus walk bob).
    pub start: Vec3,
    /// Hit location, or the trace end when nothing was hit.
    pub location: Vec3,
    /// What was hit (`None`: the impact has no actor).
    pub hit: Option<ImpactHit>,
}

/// The actor part of an [`Impact`].
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct ImpactHit {
    /// Hit normal.
    pub normal: Vec3,
    /// `false` = tag `NotGrappleAble`.
    pub grapple_able: bool,
    /// Class, tag and id of the hit actor.
    pub surface: Surface,
}

/// The fire trace (G-TG-1) during a tick: from the view location (eye
/// height and walk bob as they are now) along the camera's cached
/// point-of-view rotation ([`PlayerState::aim_direction`]: the view as the
/// previous tick ended — for a press in the input event that is the current
/// view, for a refire-served fire in the gun's timers it lacks this tick's
/// look update), `WeaponRange` long, zero extent.
#[must_use]
pub fn fire_trace<W: CollisionWorld + ?Sized>(
    state: &PlayerState,
    params: &PlayerParams,
    gun: &GrappleGunParams,
    world: &W,
) -> Impact {
    fire_trace_along(state, params, gun, world, state.aim_direction())
}

/// [`fire_trace`] along an explicit unit direction.
fn fire_trace_along<W: CollisionWorld + ?Sized>(
    state: &PlayerState,
    params: &PlayerParams,
    gun: &GrappleGunParams,
    world: &W,
    dir: Vec3,
) -> Impact {
    let start = state.view_location(params);
    let range = gun.weapon_range.value;
    match world.raycast(start, dir, range) {
        Some(h) => Impact {
            start,
            location: h.position,
            hit: Some(ImpactHit {
                normal: h.normal,
                grapple_able: h.grapple_able,
                surface: h.surface,
            }),
        },
        None => Impact {
            start,
            location: start + dir * range,
            hit: None,
        },
    }
}

/// What the fire trace would hit now and whether that hit is in range (for
/// HUD gizmos; the HUD crosshair itself is [`GunState::crosshair`]).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct GunAim {
    /// The impact.
    pub impact: Impact,
    /// Eye-to-hit distance, UU.
    pub distance: f32,
    /// A hit actor that is grapple-able (not `NotGrappleAble`), passes the
    /// top/bottom rule and is closer than `fMaxDistance`.
    pub acceptable: bool,
}

/// Previews the fire trace of a press in the next tick (no state change):
/// along the current view, which the next tick's camera cache will hold.
/// `None` without gun parameters.
#[must_use]
pub fn aim<W: CollisionWorld + ?Sized>(
    state: &PlayerState,
    params: &PlayerParams,
    world: &W,
) -> Option<GunAim> {
    let gun = params.gun.as_ref()?;
    let impact = fire_trace_along(state, params, gun, world, state.view_direction());
    let distance = (impact.start - impact.location).length();
    let acceptable = impact.hit.is_some_and(|h| {
        h.grapple_able && face_rule_passes(&h, gun) && distance < gun.max_distance.value
    });
    Some(GunAim {
        impact,
        distance,
        acceptable,
    })
}

/// The top/bottom-only rule of `ProcessInstantHit` (G-AC-4; comparisons
/// non-strict there, strict in the crosshair).
fn face_rule_passes(hit: &ImpactHit, gun: &GrappleGunParams) -> bool {
    match hit.surface.tag {
        ActorTag::TopOnlyGrappleAble => hit.normal.z >= gun.top_grapple_angle.value,
        ActorTag::BottomOnlyGrappleAble => hit.normal.z <= -gun.bottom_grapple_angle.value,
        _ => true,
    }
}

// ---------------------------------------------------------------------------
// Input events (G-IN-*).
// ---------------------------------------------------------------------------

/// The fire button's input events of one tick: `released` runs `StopFire`
/// (latch set, attempt flag cleared, common release, mode no longer
/// pending), `pressed` runs `StartFire` (mode pending; fires at once when
/// the weapon is `Active`). Synchronous, before any actor ticks (G-IN-5).
/// Cinematic mode and "players only" (which gate presses, G-IN-1) are not
/// modelled.
pub fn fire_button<W: CollisionWorld + ?Sized>(
    state: &mut PlayerState,
    pressed: bool,
    released: bool,
    params: &PlayerParams,
    world: &W,
    events: &mut StepEvents,
) {
    let Some(gun) = params.gun.as_ref() else {
        return;
    };
    if !state.script.gun.spawned {
        return;
    }
    if released {
        // StopFire (G-IN-3, G-RL-4).
        let g = &mut state.script.gun;
        g.has_grappled = false;
        g.can_grapple = true;
        release(state, ReleaseReason::Button, events);
        state.script.gun.pending_fire = false;
    }
    if pressed {
        // StartFire → BeginFire (G-IN-2).
        state.script.gun.pending_fire = true;
        if state.script.gun.weapon == WeaponState::Active {
            // `WeaponFiring.BeginState`: fire, then start the refire timer.
            state.script.gun.weapon = WeaponState::Firing;
            fire_ammunition(state, params, gun, world, false, events);
            state.script.gun.refire_timer = Some(0.0);
        }
    }
}

/// `FireAmmunition`: fires only through the latch (G-IN-3).
fn fire_ammunition<W: CollisionWorld + ?Sized>(
    state: &mut PlayerState,
    params: &PlayerParams,
    gun: &GrappleGunParams,
    world: &W,
    from_refire: bool,
    events: &mut StepEvents,
) {
    if !state.script.gun.can_grapple {
        return;
    }
    state.script.gun.can_grapple = false;
    let impact = fire_trace(state, params, gun, world);
    let outcome = process_instant_hit(state, params, gun, world, &impact, from_refire, events);
    events.gun.fire = Some(outcome);
}

/// `ProcessInstantHit` (§5 decision order).
fn process_instant_hit<W: CollisionWorld + ?Sized>(
    state: &mut PlayerState,
    params: &PlayerParams,
    gun: &GrappleGunParams,
    world: &W,
    impact: &Impact,
    from_refire: bool,
    events: &mut StepEvents,
) -> FireOutcome {
    // G-AC-0: story mode or zoom → interaction instead of a grapple, within
    // interactRange of the pawn centre (not the eye). The press is not
    // marked used in this branch.
    if state.script.is_story() {
        let mut interacted = false;
        if (state.position - impact.location).length() < gun.interact_range.value
            && let Some(hit) = impact.hit
            && hit.surface.class == ActorClass::Interactable
            && let Some(actor) = hit.surface.actor
        {
            events.kismet.push(SimEvent::InteractWith { actor });
            interacted = true;
        }
        return FireOutcome::StoryMode { interacted };
    }
    // G-AC-1.
    let g = state.script.gun;
    if g.max_grapples >= 1 {
        let not_grapple_able = impact.hit.is_none_or(|h| !h.grapple_able);
        if g.has_grappled || g.times_grappled >= g.max_grapples || not_grapple_able {
            return FireOutcome::Failed;
        }
    } else {
        return FireOutcome::Silent;
    }
    let Some(hit) = impact.hit else {
        return FireOutcome::Failed;
    };
    let outcome = if g.attached.is_none() && !g.released && !g.hand_hidden {
        // G-AC-4: silent rejection before the press is marked used.
        if !face_rule_passes(&hit, gun) {
            return FireOutcome::WrongFace;
        }
        // G-AC-5: eye-to-hit distance (the eye recomputed at evaluation; it
        // has not moved since the trace).
        let distance = (state.view_location(params) - impact.location).length();
        if distance < gun.max_distance.value {
            attach(state, world, impact.location, &hit, from_refire, events);
            FireOutcome::Attached
        } else {
            FireOutcome::Failed
        }
    } else {
        // G-AC-2.
        FireOutcome::Consumed
    };
    state.script.gun.has_grappled = true;
    outcome
}

/// Places the anchor helper at `location`, based on `actor` (if the world
/// knows where the actor is; otherwise it stays put).
fn place_helper<W: CollisionWorld + ?Sized>(
    world: &W,
    actor: Option<u32>,
    location: Vec3,
) -> Option<AnchorFollow> {
    let actor = actor?;
    let base = world.actor_location(actor).unwrap_or(location);
    Some(AnchorFollow {
        actor,
        offset: location - base,
        helper: location,
    })
}

/// `Grapple` plus the tail of `ProcessInstantHit` (§6, atomic).
fn attach<W: CollisionWorld + ?Sized>(
    state: &mut PlayerState,
    world: &W,
    location: Vec3,
    hit: &ImpactHit,
    from_refire: bool,
    events: &mut StepEvents,
) {
    let surface = hit.surface;
    let class = surface.class;
    // G-AT-1: budget.
    let g = &mut state.script.gun;
    g.times_grappled = g.times_grappled.saturating_add(1);
    // G-AT-3: grapple-able targets (interface) and `grappleInteractable`.
    let handler_target = class.grapple_able_interface();
    let interactable = surface.tag == ActorTag::GrappleInteractable || handler_target;
    if interactable {
        g.follow = place_helper(world, surface.actor, location);
        events.kismet.push(SimEvent::PlayerGrappled {
            originator: surface.actor,
        });
        if handler_target {
            if let Some(actor) = surface.actor {
                events.kismet.push(SimEvent::ActorGrappled { actor });
            }
            // A charged crystal enters `GrappledState`, whose begin code
            // refills the budget at its next state-code run (G-CT-4).
            if class == (ActorClass::RechargeCrystal { charged: true }) {
                g.pending_crystal_refill = true;
            }
        }
    } else {
        events
            .kismet
            .push(SimEvent::PlayerGrappled { originator: None });
    }
    // G-AT-4: anchor at the hit location.
    g.grapple_location = location;
    g.attached = Some(Attachment {
        target: surface.actor,
        class,
        handler_target,
    });
    // G-AT-5: one level of the move-input lock is removed.
    state.script.release_move_input();
    // G-AT-6: physics → Flying (velocity unchanged, G-AT-10; the base is
    // dropped), controller → `Grappling` (overrides a pending
    // `ReleaseGrapple`).
    state.pawn.flying = true;
    state.grounded = false;
    state.pawn.based = false;
    state.script.release_gap = false;
    // Release-instant targets start the one-shot timer (G-RL-3).
    let instant_release = class.release_instant();
    let g = &mut state.script.gun;
    if instant_release {
        g.instant_release_timer = Some(0.0);
    }
    // Pawn script state `Shooting` (ends the jump-release damping).
    state.script.code.goto(PawnStateName::Shooting);
    // G-AT-7: the anchor follows the target iff it is an `InterpActor`
    // (decided last, overriding the interactable branch).
    let g = &mut state.script.gun;
    g.follow = if class.is_interp_actor() {
        place_helper(world, surface.actor, location)
    } else {
        None
    };
    events.gun.attached = Some(GunAttach {
        anchor: location,
        surface,
        instant_release,
        from_refire,
    });
}

/// The common release (`ReleaseGrappleButton`, G-RL-1). Ignored when not
/// attached; returns whether it released. Velocity and budget are not
/// touched (the proximity halving happens before this call).
pub fn release(state: &mut PlayerState, reason: ReleaseReason, events: &mut StepEvents) -> bool {
    let Some(attachment) = state.script.gun.attached.take() else {
        return false;
    };
    // Physics → Falling (velocity unchanged).
    state.pawn.flying = false;
    state.grounded = false;
    state.pawn.based = false;
    // Pawn `Release` (ends the jump damping), controller `ReleaseGrapple`
    // (one tick without move and look, G-RL-7).
    state.script.code.goto(PawnStateName::Release);
    state.script.release_gap = true;
    state.script.gun.released = true;
    events.kismet.push(SimEvent::PlayerReleasedGrapple);
    if attachment.handler_target
        && let Some(actor) = attachment.target
    {
        events.kismet.push(SimEvent::ActorUngrappled { actor });
    }
    events.gun.released = Some(GunRelease {
        reason,
        velocity: state.velocity,
    });
    true
}

// ---------------------------------------------------------------------------
// Map actors.
// ---------------------------------------------------------------------------

/// The recharge crystal's `GrappledState` begin code: the budget is reset
/// (G-CT-4). Map-placed actors tick after the input events and before the
/// controller, so an attach in an input event refills in the same tick.
pub fn apply_pending_crystal_refill(state: &mut PlayerState) {
    let g = &mut state.script.gun;
    if g.pending_crystal_refill {
        g.pending_crystal_refill = false;
        g.times_grappled = 0;
    }
}

// ---------------------------------------------------------------------------
// The gun tick (G-PH-2, G-RL-6, G-CT-6, G-TM-4).
// ---------------------------------------------------------------------------

/// The gun's tick, after the pawn, power jump and rocket boots
/// (G-TM-2): crosshair, anchor follow, pull and proximity release,
/// released-flag reset (`AirSpeed` re-set), counter clamp; then its timers
/// (instant release, refire check, hand hiding; the order of several timers
/// firing in the same tick is TENTATIVE and does not change the outcome).
pub fn tick<W: CollisionWorld + ?Sized>(
    state: &mut PlayerState,
    params: &PlayerParams,
    world: &W,
    dt: f32,
    events: &mut StepEvents,
) {
    let Some(gun) = params.gun.as_ref() else {
        return;
    };
    if !state.script.gun.spawned {
        return;
    }
    // CheckForRange (HUD crosshair).
    state.script.gun.crosshair = crosshair(state, params, gun, world);
    // G-AT-8: follow the helper's base.
    let g = &mut state.script.gun;
    if let Some(mut f) = g.follow {
        if let Some(base) = world.actor_location(f.actor) {
            f.helper = base + f.offset;
        }
        g.follow = Some(f);
        g.grapple_location = f.helper;
    }
    if state.script.gun.attached.is_some() {
        update_grapple(state, gun, dt, events);
    }
    // G-RL-6: the released flag clears at the next gun tick (and AirSpeed is
    // re-set to fGrappleAccel, G-PH-4).
    if state.script.gun.released {
        state.script.gun.released = false;
        state.script.air_speed = gun.grapple_accel.value;
    }
    // G-CT-6: the light manager clamps the used count (workshop mode, which
    // skips it, is never enabled by any map).
    let g = &mut state.script.gun;
    g.times_grappled = g.times_grappled.clamp(0, COUNTER_CLAMP);

    // Timers (G-TM-4).
    if poll_timer(
        &mut state.script.gun.instant_release_timer,
        gun.instant_release_delay.value,
        false,
        dt,
    ) > 0
    {
        release(state, ReleaseReason::InstantTimer, events);
    }
    let fires = poll_timer(
        &mut state.script.gun.refire_timer,
        gun.fire_interval.value,
        true,
        dt,
    )
    .min(MAX_TIMER_FIRES_PER_TICK);
    for _ in 0..fires {
        // `RefireCheckTimer`: fire again while pending, else back to Active.
        if state.script.gun.pending_fire {
            fire_ammunition(state, params, gun, world, true, events);
        } else {
            let g = &mut state.script.gun;
            g.weapon = WeaponState::Active;
            g.refire_timer = None;
            break;
        }
    }
    if poll_timer(
        &mut state.script.gun.hide_hand_timer,
        HIDE_HAND_DELAY,
        false,
        dt,
    ) > 0
    {
        state.script.gun.hand_hidden = true;
    }
}

/// `UpdateGrapple` (G-PH-2): measure `d`, then — unless the rocket boots are
/// `Boosting` or the instant-release timer is pending — add the pull and
/// release (velocity halved) when `d` was below `fGrappleReleaseDistance`.
fn update_grapple(
    state: &mut PlayerState,
    gun: &GrappleGunParams,
    dt: f32,
    events: &mut StepEvents,
) {
    let anchor = state.script.gun.grapple_location;
    let d = (state.position - anchor).length();
    state.script.gun.distance = d;
    let boosting = state.script.boots.state == BootsStateName::Boosting;
    if boosting || state.script.gun.instant_release_timer.is_some() {
        return;
    }
    // Robustness guard (not original): at d == 0 the original's arithmetic
    // would give a NaN velocity; collision keeps the pawn centre away from a
    // surface point, so this never happens in play.
    if d > 0.0 && d.is_finite() {
        let dir = safe_normal(anchor - state.position);
        let inverse = 1.0 / (d / PULL_DISTANCE_SCALE);
        state.velocity += dir * dt * PULL_NUMERATOR * inverse;
    }
    if d < gun.release_distance.value {
        state.velocity *= 1.0 / PROXIMITY_VELOCITY_DIVISOR;
        release(state, ReleaseReason::Proximity, events);
    }
}

/// `CheckForRange` (G-TG-2): the HUD crosshair state after this gun tick.
/// A hit without a static-mesh component within range leaves it unchanged;
/// with no hit at all the hit location stays the zero vector (UnrealScript
/// locals start zeroed), so the range test then measures the eye's distance
/// from the world origin.
fn crosshair<W: CollisionWorld + ?Sized>(
    state: &PlayerState,
    params: &PlayerParams,
    gun: &GrappleGunParams,
    world: &W,
) -> bool {
    let previous = state.script.gun.crosshair;
    let start = state.view_location(params);
    let dir = state.aim_direction();
    let hit = world.raycast(start, dir, gun.max_distance.value + CROSSHAIR_EXTRA_RANGE);
    let location = hit.map_or(Vec3::ZERO, |h| h.position);
    let distance = (location - start).length();
    let g = &state.script.gun;
    if state.script.is_story() {
        // Positive within interactRange of an interactable (its use count is
        // the world's; treated as still accepting here).
        return distance < gun.interact_range.value
            && hit.is_some_and(|h| h.surface.class == ActorClass::Interactable);
    }
    if distance < gun.max_distance.value && g.times_grappled < g.max_grapples {
        match hit {
            Some(h) if h.surface.class.has_static_mesh_component() => {
                if !h.grapple_able {
                    false
                } else {
                    match h.surface.tag {
                        ActorTag::TopOnlyGrappleAble => h.normal.z > gun.top_grapple_angle.value,
                        ActorTag::BottomOnlyGrappleAble => {
                            h.normal.z < -gun.bottom_grapple_angle.value
                        }
                        _ => true,
                    }
                }
            }
            _ => previous,
        }
    } else {
        false
    }
}

// ---------------------------------------------------------------------------
// Kismet actions and execs.
// ---------------------------------------------------------------------------

/// `SetMaxGrapples(n)` (Kismet `SeqAct_SetMaxGrapples`, save load; G-CT-2):
/// `n ≤ −1` → [`UNLIMITED_GRAPPLES`], else `n` (0 disables the grapple
/// silently).
pub fn set_max_grapples(state: &mut PlayerState, n: i32) {
    state.script.gun.max_grapples = if n <= -1 { UNLIMITED_GRAPPLES } else { n };
}

/// `EnableGrapple(enable)` (Kismet `SeqAct_ToggleGrapple`, save load;
/// G-IN-4): writes the fire latch, so disabling only swallows the next
/// press (every button release sets the latch again).
pub fn enable_grapple(state: &mut PlayerState, enable: bool) {
    state.script.gun.can_grapple = enable;
}

/// `UnlimitedGrapples(enable)` exec (G-CT-2): on remembers the capacity and
/// sets "unlimited", off restores it; both reset the used count.
pub fn unlimited_grapples(state: &mut PlayerState, enable: bool) {
    if enable {
        state.script.gun.last_max_grapples = state.script.gun.max_grapples;
        set_max_grapples(state, -1);
    } else {
        let last = state.script.gun.last_max_grapples;
        set_max_grapples(state, last);
    }
    state.script.gun.times_grappled = 0;
}

/// `ResetGrappleAmount` (landing, crystal): used count → 0 (G-CT-4).
pub fn reset_grapple_amount(state: &mut PlayerState) {
    state.script.gun.times_grappled = 0;
}

/// `HideGrappleGun(hide, animate, visibility)` (Kismet
/// `SeqAct_ToggleVisibleGrapple`, story mode; G-AC-3). Only a hide with
/// visibility off sets the "hand hidden" flag that blocks grappling: at
/// once, or after [`HIDE_HAND_DELAY`] when animated (that timer is not
/// cancelled by a later show). A show clears the flag.
pub fn hide_grapple_gun(state: &mut PlayerState, hide: bool, animate: bool, visibility: bool) {
    let g = &mut state.script.gun;
    if hide {
        if !visibility {
            if animate {
                g.hide_hand_timer = Some(0.0);
            } else {
                g.hand_hidden = true;
            }
        }
    } else {
        g.hand_hidden = false;
    }
}

/// Releases the grapple from outside the tick (death, story mode, Kismet;
/// G-RL-5) and returns the events it raised (the release, if any, and the
/// Kismet/handler events).
pub fn release_from_outside(state: &mut PlayerState, reason: ReleaseReason) -> StepEvents {
    let mut events = StepEvents::default();
    release(state, reason, &mut events);
    events
}

/// Spawns the gun for a started pawn (`AddDefaultInventory` →
/// `PostBeginPlay`): the latch and capacity from the class defaults, and the
/// pawn's `AirSpeed` := `fGrappleAccel` (G-PH-4).
pub(crate) fn spawn_for(script: &mut pawn::PawnScript, params: &PlayerParams) {
    match params.gun.as_ref() {
        Some(gun) => {
            script.gun = GunState::spawn(gun);
            script.air_speed = gun.grapple_accel.value;
        }
        None => script.gun = GunState::default(),
    }
}
