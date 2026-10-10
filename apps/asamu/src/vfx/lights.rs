//! The grapple gun's hand lights (`asamu.GrappleGunLightManager`) and the
//! unused `asamu.DecalDynamicLight`. Pure state machines.
//!
//! The light manager drives scalar parameters of the hand's plate material:
//! `grappleLight0..2` (one lamp per grapple left) and `powerJump` (lit while
//! a power jump is charged). Ported from local reading of the script
//! (VFX_DECALS.md §4), including its quirks: a lamp goes dark only for
//! grapples actually used, lamps light up again only once the count is back
//! at zero with the fire latch open, and the power-jump lamp jumps up by its
//! full growth value in one tick (the delta time is not applied to it).

/// Light manager parameters (`GrappleGun` CDO values passed to `Initialize`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LightParams {
    /// `fBigLightStrength` (1.0). CONFIRMED (cdo).
    pub big_strength: f32,
    /// `fSmallLightStrength` (1.0). CONFIRMED (cdo).
    pub small_strength: f32,
    /// `fBigLightGrowth` (15.0). CONFIRMED (cdo).
    pub big_growth: f32,
    /// `fSmallLightGrowth` (15.0). CONFIRMED (cdo).
    pub small_growth: f32,
    /// `fBigLightReduction` (15.0). CONFIRMED (cdo).
    pub big_reduction: f32,
    /// `fSmallLightReduction` (15.0). CONFIRMED (cdo).
    pub small_reduction: f32,
    /// `fOriginalColor`: never assigned, so 0. CONFIRMED (src).
    pub original: f32,
}

impl Default for LightParams {
    fn default() -> Self {
        Self {
            big_strength: 1.0,
            small_strength: 1.0,
            big_growth: 15.0,
            small_growth: 15.0,
            big_reduction: 15.0,
            small_reduction: 15.0,
            original: 0.0,
        }
    }
}

/// The lamp values `fLampLight[0..4]` (index 3 is the power-jump lamp).
#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub struct GrappleLights {
    /// Lamp values; the material parameters equal these after each update.
    pub lamps: [f32; 4],
    /// Parameters.
    pub params: LightParams,
}

/// What the gun state passes to `UpdateLights` each tick.
#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub struct LightInputs {
    /// `iMaxGrapples`.
    pub max_grapples: i32,
    /// `iTimesGrappled` (already clamped to `[0, 3]` by the same function in
    /// the simulation, G-CT-6).
    pub times_grappled: i32,
    /// `bCanGrapple` (the fire latch).
    pub can_grapple: bool,
    /// `GrappleGun.bLightUp`: a charged power jump.
    pub light_up: bool,
    /// `bWorkShopMode` (no map enables it).
    pub workshop_mode: bool,
}

fn light_up(value: &mut f32, dt: f32, strength: f32, growth: f32, power_jump: bool) {
    if *value >= strength {
        *value = strength;
    }
    if *value < strength {
        if power_jump {
            *value += growth;
        } else {
            *value += growth * dt;
        }
    }
}

fn light_down(value: &mut f32, dt: f32, original: f32, reduction: f32) {
    if *value < original {
        *value = original;
    }
    if *value > original {
        *value -= dt * reduction;
    }
}

impl GrappleLights {
    /// One `UpdateLights(dt)` call (once per gun tick).
    pub fn update(&mut self, dt: f32, i: LightInputs) {
        if i.workshop_mode || !dt.is_finite() {
            return;
        }
        let p = self.params;
        let max_lights = i.max_grapples.min(3);
        let used = i.times_grappled.clamp(0, 3);
        // Lamps of used grapples go down, from the highest lamp of the
        // capacity downwards.
        let top: i32 = match i.max_grapples {
            m if m >= 3 => 2,
            2 => 1,
            1 => 0,
            _ => -1,
        };
        if top >= 0 {
            let downs = if i.max_grapples == 1 {
                used.min(1)
            } else {
                used
            };
            for k in 0..downs {
                let idx = top - k;
                if let Some(l) = usize::try_from(idx)
                    .ok()
                    .and_then(|i| self.lamps.get_mut(i))
                {
                    light_down(l, dt, p.original, p.small_reduction);
                }
            }
        }
        // The power-jump lamp.
        if i.light_up {
            light_up(
                &mut self.lamps[3],
                dt,
                p.small_strength,
                p.small_growth,
                true,
            );
        } else {
            light_down(&mut self.lamps[3], dt, p.original, p.big_reduction);
        }
        // All lamps of the capacity light up again once nothing is used.
        if i.can_grapple && used == 0 {
            for k in 0..max_lights.max(0) {
                if let Some(l) = usize::try_from(k).ok().and_then(|k| self.lamps.get_mut(k)) {
                    light_up(l, dt, p.small_strength, p.small_growth, false);
                }
            }
        }
    }
}

/// `DecalDynamicLight` (a `PointLightMovable` subclass): fades in at
/// `fFadeInRate` per second up to the light's initial brightness, then fades
/// linearly to zero over `fLifeTime` seconds and destroys itself.
///
/// **Not used by the shipped game** (STRONG): no map places one, no script
/// spawns one, and the gun's only reference (`lightTestThingy`) is never
/// assigned — its archetype line is commented out. Kept as a model for
/// completeness; the runtime does not spawn it (VFX_DECALS.md §4).
#[derive(Clone, Copy, Debug, PartialEq)]
#[cfg_attr(not(test), allow(dead_code))]
pub struct DecalLight {
    /// Current brightness.
    pub brightness: f32,
    initial: f32,
    fade_in: bool,
    expired: f32,
    life_time: f32,
    fade_in_rate: f32,
}

/// `DecalDynamicLight.fLifeTime`, s. CONFIRMED (cdo).
#[cfg_attr(not(test), allow(dead_code))]
pub const DECAL_LIGHT_LIFETIME: f32 = 5.0;
/// `DecalDynamicLight.fFadeInRate`, brightness per second. CONFIRMED (cdo).
#[cfg_attr(not(test), allow(dead_code))]
pub const DECAL_LIGHT_FADE_IN_RATE: f32 = 5.0;
/// `PointLightComponent0.Brightness` of the class. CONFIRMED (cdo).
#[cfg_attr(not(test), allow(dead_code))]
pub const DECAL_LIGHT_BRIGHTNESS: f32 = 10.0;

#[cfg_attr(not(test), allow(dead_code))]
impl DecalLight {
    /// A light just spawned (`PostBeginPlay` stores the brightness and sets
    /// it to 0; `FadeIn` is true by default).
    #[must_use]
    pub fn spawn(initial: f32) -> Self {
        Self {
            brightness: 0.0,
            initial,
            fade_in: true,
            expired: 0.0,
            life_time: DECAL_LIGHT_LIFETIME,
            fade_in_rate: DECAL_LIGHT_FADE_IN_RATE,
        }
    }

    /// One `Tick(dt)`; `false` once the light destroyed itself.
    pub fn tick(&mut self, dt: f32) -> bool {
        if !dt.is_finite() {
            return true;
        }
        if self.fade_in {
            if self.brightness <= self.initial {
                self.brightness += dt * self.fade_in_rate;
            } else {
                self.fade_in = false;
            }
            true
        } else {
            self.expired += dt;
            let alive = self.expired < self.life_time;
            self.brightness -= (self.initial / self.life_time) * dt;
            alive
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DT: f32 = 1.0 / 60.0;

    fn inputs(max: i32, used: i32) -> LightInputs {
        LightInputs {
            max_grapples: max,
            times_grappled: used,
            can_grapple: true,
            light_up: false,
            workshop_mode: false,
        }
    }

    #[test]
    fn lamps_light_up_for_the_capacity_only() {
        let mut l = GrappleLights::default();
        for _ in 0..10 {
            l.update(DT, inputs(2, 0));
        }
        // 15/s growth reaches strength 1 in ≈ 4 ticks, then holds at the
        // clamp (one tick may overshoot by one step before clamping).
        assert!(l.lamps[0] >= 1.0 && l.lamps[0] <= 1.0 + 15.0 * DT);
        assert!(l.lamps[1] >= 1.0);
        assert_eq!(l.lamps[2], 0.0);
        assert_eq!(l.lamps[3], 0.0);
    }

    #[test]
    fn used_grapples_dim_from_the_top_lamp() {
        let mut l = GrappleLights::default();
        for _ in 0..10 {
            l.update(DT, inputs(3, 0));
        }
        // One grapple used: lamp 2 goes down, 0 and 1 stay.
        for _ in 0..10 {
            l.update(DT, inputs(3, 1));
        }
        assert!(l.lamps[2] <= 0.0 + 1e-6, "{:?}", l.lamps);
        assert!(l.lamps[0] >= 1.0 && l.lamps[1] >= 1.0);
        // Refill with the latch closed: nothing lights up yet.
        let mut closed = inputs(3, 0);
        closed.can_grapple = false;
        l.update(DT, closed);
        assert!(l.lamps[2] <= 1e-6);
        // Latch open again: all three light up.
        for _ in 0..10 {
            l.update(DT, inputs(3, 0));
        }
        assert!(l.lamps.iter().take(3).all(|v| *v >= 1.0));
    }

    #[test]
    fn capacity_one_dims_lamp_zero() {
        let mut l = GrappleLights::default();
        for _ in 0..10 {
            l.update(DT, inputs(1, 0));
        }
        for _ in 0..10 {
            l.update(DT, inputs(1, 1));
        }
        assert!(l.lamps[0] <= 1e-6);
        // Capacities ≥ 4 behave like 3.
        let mut l = GrappleLights::default();
        for _ in 0..10 {
            l.update(DT, inputs(32_767, 0));
        }
        assert!(l.lamps.iter().take(3).all(|v| *v >= 1.0));
    }

    #[test]
    fn power_jump_lamp_jumps_then_holds_then_fades() {
        let mut l = GrappleLights::default();
        let mut i = inputs(0, 0);
        i.light_up = true;
        l.update(DT, i);
        // Quirk: the full growth (15) is added in one tick.
        assert_eq!(l.lamps[3], 15.0);
        l.update(DT, i);
        assert_eq!(l.lamps[3], 1.0);
        i.light_up = false;
        l.update(DT, i);
        assert!((l.lamps[3] - (1.0 - 15.0 * DT)).abs() < 1e-6);
        for _ in 0..10 {
            l.update(DT, i);
        }
        assert!(l.lamps[3] <= 0.0 + 1e-6);
        // Workshop mode freezes everything.
        let before = l;
        i.workshop_mode = true;
        i.light_up = true;
        l.update(DT, i);
        assert_eq!(l, before);
    }

    #[test]
    fn decal_light_fades_in_then_out_and_dies() {
        let mut d = DecalLight::spawn(DECAL_LIGHT_BRIGHTNESS);
        let mut ticks = 0;
        while d.tick(0.1) {
            ticks += 1;
            assert!(ticks < 1000);
        }
        // Fade-in: 10 / 5 per s = 2 s (plus the switching tick); fade-out
        // 5 s.
        assert!((70..=72).contains(&ticks), "{ticks}");
        assert!(d.brightness.abs() < 0.5);
    }
}
