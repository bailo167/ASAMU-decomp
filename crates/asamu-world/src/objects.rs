//! Grapple-reactive and scripted world objects and their run-time state
//! machines (`docs/reverse-engineering/GRAPPLE.md` §12, `ABILITIES.md` §13).
//!
//! Level data ([`crate::Level`]) places the objects; [`WorldObjects`] holds
//! their run-time state and ticks them as the original ticks map-placed
//! actors: after the frame's input events and before the player controller
//! and pawn (GRAPPLE.md G-TM-2; `asamu-game` runs them between the two
//! halves of the player's tick). The grapple gun reports `Grappled` /
//! `UnGrappled` handler calls and story-mode interactions as events, which
//! [`WorldObjects::apply`] receives as soon as they happen: those of the
//! input events (a fire press) before the objects tick, those of the gun's
//! tick (releases, refire-served attaches) after it.
//!
//! What is modelled:
//!
//! - [`RechargeCrystal`] (`ASAMURechargeCrystal`, G-WO-1): `Charged` →
//!   `GrappledState` (grappled while charged) → `UnCharged` (on `UnGrappled`;
//!   a child tells its linked parent, a parent uncharges all its children)
//!   → 21 latent fade steps of 0.005 s → `RechargeDelay` → `Charged`, if
//!   `bShouldRecharge`. The budget refill of `GrappledState` is applied by
//!   the grapple gun itself (`asamu_player::grapple_gun`), from the
//!   crystal's charge state in the collision world.
//! - [`GlowFlower`] (`ASAMUGlowFlower`, G-WO-2): an ordinary release-instant
//!   target; its glow is visual and not modelled.
//! - [`Mover`] (`InterpActor`, G-WO-6): a block moving on a path. The
//!   original's movers follow Matinee data that is not imported yet; the
//!   path here ([`MoverPath`]) is **our** test motion, not original data.
//! - [`Interactable`] (`ASAMUInteractable_Actor`, G-AC-0): story-mode
//!   interaction counted against `MaxInteractTimes` (0 = unlimited).
//! - [`Attractor`] (`ASAMUTelePad_Attractor`, ABILITIES.md §13): once
//!   activated (Kismet `SeqAct_ToggleAttractor`) it adds a velocity toward
//!   the pad every 0.05 s forever.
//!
//! Latent sleeps follow the rule of GRAPPLE.md G-TM-3 (also implemented in
//! `asamu_player::pawn`; this crate does not depend on `asamu-player`):
//! polled from the tick after the one that issued them, woken when the
//! remaining time is below half of the tick's `dt`.

use glam::Vec3;
use serde::{Deserialize, Serialize};

/// A latent sleep wakes when the remaining time is below this fraction of
/// the tick's `dt` (evaluated in `f64`; GRAPPLE.md G-TM-3). NativeCode
/// `AActor::execPollSleep @ 0x100B35B70` (data 0x1016BC0C0). CONFIRMED.
pub const LATENT_WAKE_FRACTION: f64 = 0.5;

/// Fade steps per second of a recharge crystal; the fade loops run this
/// plus one times (G-WO-1). ScriptCode `asamu.ASAMURechargeCrystal`
/// (`STEPS_PER_SECOND`). CONFIRMED (src).
pub const CRYSTAL_FADE_STEPS_PER_SECOND: u32 = 20;
/// Latent sleep of one crystal fade step, s (G-WO-1). ScriptCode
/// `asamu.ASAMURechargeCrystal` (`SLEEP_TIME`). CONFIRMED (src).
pub const CRYSTAL_FADE_STEP_SLEEP: f32 = 0.005;
/// Class default `RechargeDelay`, s. ScriptDefault
/// `asamu.ASAMURechargeCrystal.RechargeDelay`. CONFIRMED (cdo).
pub const CRYSTAL_DEFAULT_RECHARGE_DELAY: f32 = 10.0;
/// Class default `MaxInteractTimes` (G-AC-0). ScriptDefault
/// `asamu.ASAMUInteractable_Actor.MaxInteractTimes`. CONFIRMED (cdo).
pub const INTERACTABLE_DEFAULT_MAX_INTERACT_TIMES: i32 = 1;
/// Attractor step: the velocity write repeats every this many seconds and
/// the ramp time advances by it (ABILITIES.md §13). ScriptCode
/// `asamu.ASAMUTelePad_Attractor` state `Activated`. CONFIRMED (src).
pub const ATTRACTOR_STEP: f32 = 0.05;
/// Class default `attractDuration`, s. ScriptDefault
/// `asamu.ASAMUTelePad_Attractor.attractDuration`. CONFIRMED (cdo).
pub const ATTRACTOR_DEFAULT_DURATION: f32 = 10.0;
/// Class default `Range`. ScriptDefault `asamu.ASAMUTelePad_Attractor.Range`.
/// CONFIRMED (cdo). Every placed attractor overrides it (1000).
pub const ATTRACTOR_DEFAULT_RANGE: f32 = 500.0;
/// Class default `Strength`. ScriptDefault
/// `asamu.ASAMUTelePad_Attractor.Strength`. CONFIRMED (cdo). Every placed
/// attractor overrides it (200).
pub const ATTRACTOR_DEFAULT_STRENGTH: f32 = 500.0;
/// `Range` of every attractor placed in the shipped maps (Workshop,
/// FrontEnd, TheCore). CONFIRMED (map decode, `DEFAULTS.md` §7).
pub const ATTRACTOR_PLACED_RANGE: f32 = 1000.0;
/// `Strength` of every placed attractor. CONFIRMED (map decode).
pub const ATTRACTOR_PLACED_STRENGTH: f32 = 200.0;
/// `velocityBaseAmount` of every placed attractor (class default 0).
/// CONFIRMED (map decode).
pub const ATTRACTOR_PLACED_VELOCITY_BASE_AMOUNT: f32 = 0.05;

/// G-TM-3 latent sleep poll (see the module docs). Returns `true` when the
/// sleep ended this tick.
fn poll_sleep(remaining: &mut Option<f32>, dt: f32) -> bool {
    let Some(r) = *remaining else {
        return false;
    };
    let r = r - dt;
    if f64::from(r) < LATENT_WAKE_FRACTION * f64::from(dt) {
        *remaining = None;
        true
    } else {
        *remaining = Some(r);
        false
    }
}

/// UE3 `SafeNormal` (zero below a squared length of 1e-8).
fn safe_normal(v: Vec3) -> Vec3 {
    let sq = v.length_squared();
    if sq == 1.0 {
        v
    } else if sq.is_finite() && sq >= 1.0e-8 {
        v * (1.0 / sq.sqrt())
    } else {
        Vec3::ZERO
    }
}

// ---------------------------------------------------------------------------
// Level data.
// ---------------------------------------------------------------------------

/// A recharge crystal (`ASAMURechargeCrystal`), a cube.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RechargeCrystal {
    /// Actor id (unique among all level objects).
    pub id: u32,
    /// Centre, UU.
    pub center: Vec3,
    /// Half edge length, UU.
    pub half_extent: f32,
    /// `RechargeDelay`, s.
    pub recharge_delay: f32,
    /// `bShouldRecharge`.
    pub should_recharge: bool,
    /// `bParentCrystal`.
    pub parent_crystal: bool,
    /// `linkedParentCrystal` (id of the parent, for a child).
    pub linked_parent: Option<u32>,
}

/// A glow flower (`ASAMUGlowFlower`), a cube.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct GlowFlower {
    /// Actor id.
    pub id: u32,
    /// Centre, UU.
    pub center: Vec3,
    /// Half edge length, UU.
    pub half_extent: f32,
}

/// Motion of a [`Mover`] (**our** test motion; original movers follow
/// Matinee data that is not imported yet).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MoverPath {
    /// Linear back and forth between the rest position and `rest + offset`
    /// with period `period` seconds.
    PingPong {
        /// Displacement at the far end, UU.
        offset: Vec3,
        /// Seconds for a full back-and-forth cycle.
        period: f32,
    },
}

impl MoverPath {
    /// Displacement from the rest position at time `t` (seconds).
    #[must_use]
    pub fn offset_at(&self, t: f64) -> Vec3 {
        match *self {
            Self::PingPong { offset, period } => {
                let valid = period > 0.0 && t.is_finite();
                if !valid {
                    return Vec3::ZERO;
                }
                let phase = (t / f64::from(period)).rem_euclid(1.0);
                let tri = if phase < 0.5 {
                    phase * 2.0
                } else {
                    2.0 - phase * 2.0
                };
                offset * tri as f32
            }
        }
    }
}

/// A moving block (`InterpActor`): the grapple anchor follows it (G-AT-7).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Mover {
    /// Actor id.
    pub id: u32,
    /// Minimum corner at rest, UU.
    pub min: Vec3,
    /// Maximum corner at rest, UU.
    pub max: Vec3,
    /// Motion.
    pub path: MoverPath,
    /// Label (debugging / rendering).
    pub label: String,
}

/// A story-mode interaction target (`ASAMUInteractable_Actor`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Interactable {
    /// Actor id.
    pub id: u32,
    /// Minimum corner, UU.
    pub min: Vec3,
    /// Maximum corner, UU.
    pub max: Vec3,
    /// `MaxInteractTimes` (0 = unlimited).
    pub max_interact_times: i32,
    /// Label.
    pub label: String,
}

/// A tele-pad attractor (`ASAMUTelePad_Attractor`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Attractor {
    /// Actor id.
    pub id: u32,
    /// Pad location, UU.
    pub position: Vec3,
    /// `attractDuration`, s (ramp time).
    pub attract_duration: f32,
    /// `Range`.
    pub range: f32,
    /// `Strength`.
    pub strength: f32,
    /// `velocityBaseAmount`.
    pub velocity_base_amount: f32,
}

impl Attractor {
    /// An attractor with the values every shipped map places (`Range`
    /// 1000, `Strength` 200, `velocityBaseAmount` 0.05, `attractDuration`
    /// class default 10).
    #[must_use]
    pub fn as_placed(id: u32, position: Vec3) -> Self {
        Self {
            id,
            position,
            attract_duration: ATTRACTOR_DEFAULT_DURATION,
            range: ATTRACTOR_PLACED_RANGE,
            strength: ATTRACTOR_PLACED_STRENGTH,
            velocity_base_amount: ATTRACTOR_PLACED_VELOCITY_BASE_AMOUNT,
        }
    }

    /// One velocity write (ABILITIES.md §13): `V += −(t/D)·unit(P − pad)·
    /// ((Range/|P − pad|)·Strength·(1 − b) + Strength·b)`, i.e. toward the
    /// pad. `None` when the pawn is exactly at the pad (robustness guard:
    /// the original's arithmetic would produce a NaN there).
    #[must_use]
    pub fn velocity_change(&self, t: f32, pawn_location: Vec3) -> Option<Vec3> {
        let away = pawn_location - self.position;
        let distance = away.length();
        let valid = distance > 0.0 && distance.is_finite();
        if !valid {
            return None;
        }
        let ramp = -(t / self.attract_duration);
        let b = self.velocity_base_amount;
        let magnitude = (self.range / distance) * self.strength * (1.0 - b) + self.strength * b;
        Some(safe_normal(away) * ramp * magnitude)
    }
}

// ---------------------------------------------------------------------------
// Run-time state.
// ---------------------------------------------------------------------------

/// `ASAMURechargeCrystal` states.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CrystalStateName {
    /// `Charged` (auto state; its fade-in is cosmetic).
    #[default]
    Charged,
    /// `GrappledState`: grappled while charged.
    Grappled,
    /// `UnCharged`: fading out, then waiting for the recharge.
    UnCharged,
}

/// Run-time state of one crystal.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct CrystalState {
    /// Current state.
    pub state: CrystalStateName,
    /// The state code restarts at `Begin` at the next state-code run.
    pub begin_pending: bool,
    /// Remaining latent sleep, s.
    pub sleep: Option<f32>,
    /// Fade step counter (`forIndex`).
    pub fade_index: u32,
    /// The fade has finished and the code waits for the recharge.
    pub waiting_for_recharge: bool,
}

impl CrystalState {
    fn goto(&mut self, new: CrystalStateName) {
        self.state = new;
        self.sleep = None;
        self.begin_pending = true;
        self.fade_index = 0;
        self.waiting_for_recharge = false;
    }
}

/// Run-time state of one attractor.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct AttractorState {
    /// `Activated` (after `ActivatePad`); never deactivated.
    pub active: bool,
    /// The state code restarts at `Begin` at the next run.
    pub begin_pending: bool,
    /// Remaining latent sleep, s.
    pub sleep: Option<f32>,
    /// `timeElapsed` (ramp time), s.
    pub time: f32,
}

/// A world-object reaction worth reporting (Kismet or HUD).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum WorldEvent {
    /// A crystal lost its charge (`UnCharged`).
    CrystalUncharged {
        /// Crystal id.
        id: u32,
    },
    /// A crystal is charged again.
    CrystalRecharged {
        /// Crystal id.
        id: u32,
    },
    /// Kismet `SeqEvent_ActorInteractedWith` (the interaction was accepted).
    ActorInteractedWith {
        /// Interactable id.
        id: u32,
    },
    /// The player began touching a trigger, trigger volume or kill zone
    /// (`Touch`; Kismet `SeqEvent_Touch`). Converted levels only.
    Touch {
        /// Actor id.
        id: u32,
    },
    /// The player stopped touching it (`UnTouch`).
    UnTouch {
        /// Actor id.
        id: u32,
    },
    /// A checkpoint was activated (`ActivateCheckpoint`, A-CP-2).
    CheckpointActivated {
        /// Checkpoint actor id.
        id: u32,
        /// Its `checkpointIndex`.
        index: i32,
    },
    /// The activation made `index` the level's latest checkpoint; the
    /// original saves the game here (A-CP-3).
    CheckpointSaved {
        /// `checkpointIndex`.
        index: i32,
    },
    /// A falling-when-grappled rock was grappled and starts its fall
    /// (G-WO-4).
    RockReleased {
        /// Rock actor id.
        id: u32,
    },
    /// The player died: start of the death sequence (A-DT-2, t = 0).
    PlayerDied {
        /// Why.
        cause: crate::gameplay::DeathCause,
    },
    /// The death sequence reset the player at the latest checkpoint (A-DT-2,
    /// t = 0.3 s; Kismet `SeqEvent_PlayerDied`).
    PlayerRespawned,
}

/// The handler calls and interactions the grapple gun reports (a
/// world-side mirror of `asamu_player::SimEvent`'s relevant variants).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum ObjectEvent {
    /// `Grappled` handler of the actor.
    Grappled(u32),
    /// `UnGrappled` handler of the actor.
    UnGrappled(u32),
    /// `InteractWith` of the actor (story mode).
    InteractWith(u32),
}

/// Run-time state of every level object, in level order.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct WorldObjects {
    /// Crystals (same order as `Level::crystals`).
    pub crystals: Vec<CrystalState>,
    /// Current displacement of each mover (same order as `Level::movers`).
    pub mover_offsets: Vec<Vec3>,
    /// Uses of each interactable (`timesInteractedWith`).
    pub interactions: Vec<i32>,
    /// Attractors (same order as `Level::attractors`).
    pub attractors: Vec<AttractorState>,
}

impl WorldObjects {
    /// The initial state for `level`.
    #[must_use]
    pub fn new(level: &crate::Level) -> Self {
        Self {
            crystals: vec![CrystalState::default(); level.crystals.len()],
            mover_offsets: vec![Vec3::ZERO; level.movers.len()],
            interactions: vec![0; level.interactables.len()],
            attractors: vec![AttractorState::default(); level.attractors.len()],
        }
    }

    /// `true` if the crystal `id` is in its `Charged` state.
    #[must_use]
    pub fn crystal_charged(&self, level: &crate::Level, id: u32) -> bool {
        level
            .crystals
            .iter()
            .position(|c| c.id == id)
            .and_then(|i| self.crystals.get(i))
            .is_some_and(|c| c.state == CrystalStateName::Charged)
    }

    /// Activates the attractor `id` (`ActivatePad`, Kismet
    /// `SeqAct_ToggleAttractor`); its code starts at the next map-actor
    /// tick. Returns whether such an attractor exists. Activating an active
    /// attractor does nothing (`ActivatePad` only exists in `Idle`).
    pub fn activate_attractor(&mut self, level: &crate::Level, id: u32) -> bool {
        let Some(i) = level.attractors.iter().position(|a| a.id == id) else {
            return false;
        };
        if let Some(a) = self.attractors.get_mut(i)
            && !a.active
        {
            a.active = true;
            a.begin_pending = true;
            a.sleep = None;
            a.time = 0.0;
        }
        true
    }

    /// Ticks the map-placed actors for one frame (after the player's input
    /// events, before its controller and pawn): movers
    /// move to their position at `time_after` (seconds since level start at
    /// the end of this tick), crystals run their state code, active
    /// attractors write the pawn's velocity.
    pub fn tick(
        &mut self,
        level: &crate::Level,
        dt: f32,
        time_after: f64,
        pawn_location: Vec3,
        pawn_velocity: &mut Vec3,
        events: &mut Vec<WorldEvent>,
    ) {
        for (offset, mover) in self.mover_offsets.iter_mut().zip(&level.movers) {
            *offset = mover.path.offset_at(time_after);
        }
        for (state, crystal) in self.crystals.iter_mut().zip(&level.crystals) {
            tick_crystal(state, crystal, dt, events);
        }
        for (state, attractor) in self.attractors.iter_mut().zip(&level.attractors) {
            tick_attractor(state, attractor, dt, pawn_location, pawn_velocity);
        }
    }

    /// Applies a grapple handler call or interaction reported by the player
    /// simulation.
    pub fn apply(
        &mut self,
        level: &crate::Level,
        event: ObjectEvent,
        events: &mut Vec<WorldEvent>,
    ) {
        match event {
            ObjectEvent::Grappled(id) => {
                if let Some(i) = level.crystals.iter().position(|c| c.id == id)
                    && let Some(c) = self.crystals.get_mut(i)
                    && c.state == CrystalStateName::Charged
                {
                    // `Charged.Grappled` → `GrappledState` (other states have
                    // the empty global handler).
                    c.goto(CrystalStateName::Grappled);
                }
            }
            ObjectEvent::UnGrappled(id) => {
                let Some(i) = level.crystals.iter().position(|c| c.id == id) else {
                    return;
                };
                let grappled = self
                    .crystals
                    .get(i)
                    .is_some_and(|c| c.state == CrystalStateName::Grappled);
                if !grappled {
                    return;
                }
                // `GrappledState.UnGrappled`: notify the family, then
                // `UnCharged`.
                if let Some(crystal) = level.crystals.get(i) {
                    self.notify_grappled(level, crystal, events);
                }
                self.uncharge(i, level, events);
            }
            ObjectEvent::InteractWith(id) => {
                if let Some(i) = level.interactables.iter().position(|x| x.id == id)
                    && let (Some(times), Some(def)) =
                        (self.interactions.get_mut(i), level.interactables.get(i))
                    && (def.max_interact_times == 0 || *times < def.max_interact_times)
                {
                    *times = times.saturating_add(1);
                    events.push(WorldEvent::ActorInteractedWith { id });
                }
            }
        }
    }

    /// `NotifyGrappled`: a child tells its linked parent (which uncharges
    /// itself and its children); a parent uncharges itself and its children.
    fn notify_grappled(
        &mut self,
        level: &crate::Level,
        crystal: &RechargeCrystal,
        events: &mut Vec<WorldEvent>,
    ) {
        let family_head = if crystal.parent_crystal {
            Some(crystal.id)
        } else {
            crystal.linked_parent
        };
        let Some(head) = family_head else {
            return;
        };
        if let Some(p) = level.crystals.iter().position(|c| c.id == head) {
            self.uncharge(p, level, events);
        }
        // Children registered with the head (`AddChildCrystal`).
        let children: Vec<usize> = level
            .crystals
            .iter()
            .enumerate()
            .filter(|(_, c)| !c.parent_crystal && c.linked_parent == Some(head))
            .map(|(i, _)| i)
            .collect();
        for i in children {
            self.uncharge(i, level, events);
        }
    }

    fn uncharge(&mut self, i: usize, level: &crate::Level, events: &mut Vec<WorldEvent>) {
        if let (Some(c), Some(def)) = (self.crystals.get_mut(i), level.crystals.get(i)) {
            if c.state != CrystalStateName::UnCharged {
                events.push(WorldEvent::CrystalUncharged { id: def.id });
            }
            // `GotoState('UnCharged')` (re-entering restarts its code).
            c.goto(CrystalStateName::UnCharged);
        }
    }
}

/// One crystal's state code for a tick (G-WO-1).
fn tick_crystal(
    state: &mut CrystalState,
    crystal: &RechargeCrystal,
    dt: f32,
    events: &mut Vec<WorldEvent>,
) {
    let woke = poll_sleep(&mut state.sleep, dt);
    if !woke && (state.sleep.is_some() || !state.begin_pending) {
        return;
    }
    let begin = !woke;
    state.begin_pending = false;
    if state.state != CrystalStateName::UnCharged {
        // `Charged` (cosmetic fade-in) and `GrappledState` (the budget
        // refill, applied by the gun) have no world-side effect here.
        return;
    }
    if begin {
        state.fade_index = 0;
    } else if state.waiting_for_recharge {
        state.goto(CrystalStateName::Charged);
        state.begin_pending = false;
        events.push(WorldEvent::CrystalRecharged { id: crystal.id });
        return;
    } else {
        state.fade_index = state.fade_index.saturating_add(1);
    }
    // Fade-out loop: steps 0..=STEPS_PER_SECOND, one latent sleep each.
    if state.fade_index < CRYSTAL_FADE_STEPS_PER_SECOND.saturating_add(1) {
        state.sleep = Some(CRYSTAL_FADE_STEP_SLEEP);
        return;
    }
    if crystal.should_recharge {
        state.waiting_for_recharge = true;
        state.sleep = Some(crystal.recharge_delay);
    }
}

/// One attractor's state code for a tick (ABILITIES.md §13).
fn tick_attractor(
    state: &mut AttractorState,
    attractor: &Attractor,
    dt: f32,
    pawn_location: Vec3,
    pawn_velocity: &mut Vec3,
) {
    if !state.active {
        return;
    }
    let woke = poll_sleep(&mut state.sleep, dt);
    if !woke && (state.sleep.is_some() || !state.begin_pending) {
        return;
    }
    if !woke {
        state.time = 0.0;
    }
    state.begin_pending = false;
    if let Some(dv) = attractor.velocity_change(state.time, pawn_location) {
        *pawn_velocity += dv;
    }
    state.time = if state.time < attractor.attract_duration {
        state.time + ATTRACTOR_STEP
    } else {
        attractor.attract_duration
    };
    state.sleep = Some(ATTRACTOR_STEP);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ping_pong_path_is_a_triangle_wave() {
        let p = MoverPath::PingPong {
            offset: Vec3::new(100.0, 0.0, 0.0),
            period: 4.0,
        };
        assert_eq!(p.offset_at(0.0), Vec3::ZERO);
        assert_eq!(p.offset_at(1.0), Vec3::new(50.0, 0.0, 0.0));
        assert_eq!(p.offset_at(2.0), Vec3::new(100.0, 0.0, 0.0));
        assert_eq!(p.offset_at(3.0), Vec3::new(50.0, 0.0, 0.0));
        assert_eq!(p.offset_at(4.0), Vec3::ZERO);
        let bad = MoverPath::PingPong {
            offset: Vec3::ONE,
            period: 0.0,
        };
        assert_eq!(bad.offset_at(1.0), Vec3::ZERO);
        assert_eq!(p.offset_at(f64::NAN), Vec3::ZERO);
    }

    #[test]
    fn attractor_formula_points_at_the_pad() {
        let a = Attractor::as_placed(1, Vec3::ZERO);
        // At t = 0 nothing; at the full ramp (t = D) with b = 0.05 at 500 uu:
        // (1000/500)·200·0.95 + 200·0.05 = 390 uu/s toward the pad.
        let zero = a.velocity_change(0.0, Vec3::new(500.0, 0.0, 0.0)).unwrap();
        assert_eq!(zero.length(), 0.0);
        let dv = a.velocity_change(10.0, Vec3::new(500.0, 0.0, 0.0)).unwrap();
        assert!((dv - Vec3::new(-390.0, 0.0, 0.0)).length() < 1e-3, "{dv}");
        assert_eq!(a.velocity_change(1.0, Vec3::ZERO), None);
    }
}
