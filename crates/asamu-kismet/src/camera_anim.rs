//! Camera animations (`CameraAnim` assets played on the player's camera):
//! the importer's export (`<converted>/matinee/camera_anims.json` for the
//! gameplay animations of `Startup.upk`, the maps' own `camera_anims` in
//! `<map>.matinee.json`), the engine's camera-animation pool and blending
//! (`ACamera::PlayCameraAnim`, `UCameraAnimInst::{Play, AdvanceAnim, Stop,
//! Update}`, `ACamera::ApplyCameraModifiers`, `ACamera::ApplyAnimToCamera`,
//! `StopCameraAnim`, `StopAllCameraAnims(ByType)`), and the calls the
//! gameplay script makes ([`GameplayCue`]).
//!
//! Engine rules (read in the locally decompiled natives of the shipped Mac
//! executable; described in our own words, nothing copied;
//! `docs/reverse-engineering/MATINEE.md` "Camera animations"):
//!
//! - The camera owns a fixed pool of [`MAX_ACTIVE_CAMERA_ANIMS`] instances.
//!   `PlayCameraAnim` takes the last free one; with none free it plays
//!   nothing. The script keeps the returned instance object, so a stale
//!   reference to an instance that finished and was reused stops whatever
//!   that instance plays now ([`CameraAnimHandle`] is the pool slot, which
//!   reproduces this).
//! - `Play` resets the time (or a random start), the blend timers and the
//!   flags: blending in is always set (even with a zero blend-in time), not
//!   blending out, not finished; `RemainingTime` is `Duration − BlendOutTime`
//!   when a duration is given.
//! - `AdvanceAnim(dt)`: time += `dt · PlayRate`; blend timers += `dt`. A
//!   non-looping animation past its length finishes; otherwise, within
//!   `BlendOutTime` of the end it (re)starts blending out with the timer at
//!   the overshoot. A looping one past its length subtracts the length once.
//!   Blending in ends once its timer exceeds `BlendInTime` (strictly);
//!   blending out finishes the animation once its timer exceeds
//!   `BlendOutTime` (the timer is clamped first). The weight is
//!   `min(in, out) · BasePlayScale · TransientScaleModifier` with
//!   `in = t_in / BlendInTime` while blending in (else 1) and
//!   `out = 1 − t_out / BlendOutTime` while blending out (else 1). The group
//!   is then evaluated at the new time (move track on a camera actor reset to
//!   the origin, `FOVAngle` reset to the animation's `BaseFOV`). A finished
//!   animation still contributes this frame; afterwards it is released.
//!   `RemainingTime` (a duration) counts down and starts the blend-out.
//! - `Stop(immediate)`: immediately, or when `BlendOutTime ≤ 0`, the
//!   animation finishes; otherwise it starts blending out from 0.
//! - `ApplyAnimToCamera` (play space `CAPS_CameraLocal`, the default and the
//!   only one used): with `s` the weight, the point of view moves by the
//!   animated location × `s` rotated into camera space, its rotation becomes
//!   `rotator(R(anim rotation × s, truncated to whole units) · R(view))`, and
//!   the FOV changes by `s · (animated FOVAngle − 90)` (90 = `CameraActor`
//!   default `FOVAngle`). Animations apply in pool order, each onto the
//!   result of the previous one, when their weight is above 0.
//! - The move track's initial transform is taken at time 0 (the group
//!   instance's outer is not a `SeqAct_Interp`, so `CalcInitialTransform`
//!   uses 0; CONFIRMED from the disassembly), from a camera actor at the
//!   origin with zero rotation.
//!
//! Ours, not the engine's: non-finite play parameters fall back to the
//! defaults (rate and scale 1, no blend, no duration) and a sample that
//! would make the point of view non-finite is skipped.
//!
//! Not modelled: the animations' post-process settings and post-process
//! tracks (`BasePPSettings`, `CamOverridePostProcess.*`; e.g. the grapple
//! animations carry only post-process settings), play spaces other than
//! camera-local (no shipped call uses one), the anim-node registration
//! (`RegisterAnimNode`; unused by the shipped script).

use std::collections::BTreeMap;
use std::sync::Arc;

use serde::Deserialize;

use crate::matinee::{
    InterpCurve, InterpGroup, Matrix, MoveTrack, TrackData, matrix_rotator,
    rotation_translation_matrix,
};

/// `Camera.MAX_ACTIVE_CAMERA_ANIMS` (CONFIRMED: class constant of
/// `Engine.Camera`, the size of the instance pool the camera creates).
pub const MAX_ACTIVE_CAMERA_ANIMS: usize = 8;
/// `CameraActor` default `FOVAngle` (CONFIRMED (cdo): `Engine.CameraActor`
/// defaults), the reference `ApplyAnimToCamera` measures the FOV offset
/// from.
pub const CAMERA_ACTOR_DEFAULT_FOV: f32 = 90.0;
/// `format` of `camera_anims.json`.
pub const CAMERA_ANIMS_FORMAT: &str = "asamu-camera-anims";

/// Paths of the camera animations the gameplay script plays (CONFIRMED:
/// class defaults and object literals of the `asamu` script, read locally).
pub mod paths {
    /// `GrappleGun.Grapple` (once, on every attach).
    pub const GRAPPLE_BEGIN: &str = "ASAMUCameraAnimations.Grapple.GrappleBegin";
    /// `GrappleGun.Grapple` (looping until the common release).
    pub const GRAPPLE_LOOP: &str = "ASAMUCameraAnimations.Grapple.GrappleLoop";
    /// `ASAMUPawn.normalLandCameraAnim` (cdo).
    pub const NORMAL_LAND: &str = "ASAMUCameraAnimations.NormalLand";
    /// `ASAMUPawn.hardLandCameraAnim` (cdo).
    pub const HARD_LAND: &str = "ASAMUCameraAnimations.HardLanding";
    /// `ASAMUPowerJump.OnPowerJumpStartCharging`.
    pub const POWER_JUMP_CHARGE: &str = "Zeth_CameraStuffs.PowerJumpChargeCameraAnim";
    /// `ASAMUPowerJump.PlayCameraAnimation` (power jump, parkour mode off).
    pub const POWER_JUMP_BOB: &str = "ASAMUCameraAnimations.PowerJumpBob";
    /// `ASAMUPowerJump.PlayCameraAnimation` (power leap, parkour mode off).
    pub const POWER_LEAP_BOB: &str = "ASAMUCameraAnimations.PowerLeapBob";
    /// `ASAMURocketBoots.ChargeCameraAnim` (cdo).
    pub const BOOTS_CHARGE: &str = "ASAMUCameraAnimations.rocketBoots.RocketBootsBegin";
    /// `ASAMURocketBoots.BoostingCameraAnim` (cdo).
    pub const BOOTS_BOOSTING: &str = "ASAMUCameraAnimations.rocketBoots.RocketBootsBoosting";
    /// `ASAMUPlayerController.StartCameraShake` (the worm's scream).
    pub const WORM_GROWL: &str = "Zeth_CameraStuffs.MonsterGrowl";
}

// ================================================================== data

/// One camera animation, ready to play.
#[derive(Debug, Clone, PartialEq)]
pub struct CameraAnim {
    /// Object path.
    pub path: String,
    /// `AnimLength`, s.
    pub length: f32,
    /// `BaseFOV`.
    pub base_fov: f32,
    /// The group's first move track (the only one `Play` keeps).
    pub move_track: Option<MoveTrack>,
    /// A float property track on `FOVAngle`, if any.
    pub fov_track: Option<InterpCurve<f32>>,
}

impl CameraAnim {
    fn from_info(info: CameraAnimJson) -> Option<CameraAnim> {
        let group = info.group?;
        let mut move_track = None;
        let mut fov_track = None;
        for t in group.tracks {
            if t.disabled {
                continue;
            }
            match t.data {
                TrackData::Move(m) if move_track.is_none() => move_track = Some(m),
                TrackData::FloatProperty(f)
                    if fov_track.is_none()
                        && f.name
                            .as_deref()
                            .is_some_and(|n| n.eq_ignore_ascii_case("FOVAngle")) =>
                {
                    fov_track = Some(f.curve);
                }
                _ => {}
            }
        }
        let finite = |v: f32, d: f32| if v.is_finite() { v } else { d };
        Some(CameraAnim {
            path: info.path,
            // A negative length would make a looping animation's time run
            // away; no asset has one.
            length: finite(info.length, 0.0).max(0.0),
            base_fov: finite(info.base_fov, CAMERA_ACTOR_DEFAULT_FOV),
            move_track,
            fov_track,
        })
    }

    /// The camera actor's location, rotation and `FOVAngle` at `t`
    /// (`AdvanceAnim`'s group evaluation on an actor reset to the origin).
    #[must_use]
    pub fn sample(&self, t: f32) -> ([f32; 3], [i32; 3], f32) {
        let mut location = [0.0f32; 3];
        let mut rotation = [0i32; 3];
        if let Some(m) = &self.move_track
            && !m.disable_movement
        {
            let inst = m.instance([0.0; 3], [0; 3], 0.0);
            if let Some((l, r)) = m.sample(t, &inst) {
                location = l;
                if let Some(r) = r {
                    rotation = r;
                }
            }
        }
        let fov = self
            .fov_track
            .as_ref()
            .map_or(self.base_fov, |c| c.eval(t, self.base_fov));
        (location, rotation, fov)
    }
}

#[derive(Debug, Clone, Deserialize)]
struct CameraAnimJson {
    #[serde(default)]
    path: String,
    #[serde(default)]
    length: f32,
    #[serde(default)]
    base_fov: f32,
    #[serde(default)]
    group: Option<InterpGroup>,
}

#[derive(Debug, Clone, Deserialize)]
struct CameraAnimsJson {
    #[serde(default)]
    camera_anims: Vec<CameraAnimJson>,
}

/// Camera animations by object path.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct CameraAnimSet {
    anims: BTreeMap<String, Arc<CameraAnim>>,
}

impl CameraAnimSet {
    /// Adds the `camera_anims` of a `camera_anims.json` or a map's
    /// `<map>.matinee.json` (the first entry of a path wins). Returns how many
    /// were added.
    ///
    /// # Errors
    /// Invalid JSON.
    pub fn add_json(&mut self, data: &[u8]) -> Result<usize, String> {
        let f: CameraAnimsJson = serde_json::from_slice(data).map_err(|e| e.to_string())?;
        let mut n = 0;
        for info in f.camera_anims {
            let key = info.path.to_ascii_lowercase();
            if self.anims.contains_key(&key) {
                continue;
            }
            if let Some(a) = CameraAnim::from_info(info) {
                self.anims.insert(key, Arc::new(a));
                n += 1;
            }
        }
        Ok(n)
    }

    /// Adds one animation (tests, tools).
    pub fn insert(&mut self, anim: CameraAnim) {
        self.anims
            .insert(anim.path.to_ascii_lowercase(), Arc::new(anim));
    }

    /// The animation at `path` (case-insensitive).
    #[must_use]
    pub fn get(&self, path: &str) -> Option<&Arc<CameraAnim>> {
        self.anims.get(&path.to_ascii_lowercase())
    }

    /// Number of animations.
    #[must_use]
    pub fn len(&self) -> usize {
        self.anims.len()
    }

    /// `true` without animations.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.anims.is_empty()
    }
}

/// Loads `<dir>/matinee/camera_anims.json` and the `camera_anims` of the
/// named maps' Matinee files (each optional; a malformed file is skipped and
/// reported).
#[must_use]
pub fn load_camera_anims(dir: &std::path::Path, maps: &[String]) -> (CameraAnimSet, Vec<String>) {
    let mut set = CameraAnimSet::default();
    let mut problems = Vec::new();
    let mut files: Vec<std::path::PathBuf> = Vec::new();
    if let Some(p) = crate::find_file(dir, "matinee", "camera_anims", ".json") {
        files.push(p);
    }
    for m in maps {
        if let Some(p) = crate::find_file(dir, "matinee", m, ".matinee.json") {
            files.push(p);
        }
    }
    for p in files {
        match crate::read_limited(&p) {
            Ok(data) => {
                if let Err(e) = set.add_json(&data) {
                    problems.push(format!("{}: {e}", p.display()));
                }
            }
            Err(e) => problems.push(e.to_string()),
        }
    }
    (set, problems)
}

// ================================================================== pool

/// A camera-animation instance of the pool (the object the script keeps).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CameraAnimHandle(pub u8);

#[derive(Debug, Clone, PartialEq)]
struct Inst {
    anim: Option<Arc<CameraAnim>>,
    cur_time: f32,
    looping: bool,
    finished: bool,
    auto_release: bool,
    blend_in_time: f32,
    blend_out_time: f32,
    blending_in: bool,
    blending_out: bool,
    cur_blend_in: f32,
    cur_blend_out: f32,
    play_rate: f32,
    base_scale: f32,
    transient_scale: f32,
    weight: f32,
    remaining: f32,
}

impl Default for Inst {
    /// `Engine.Default__CameraAnimInst` (CONFIRMED (cdo): `bFinished` and
    /// `bAutoReleaseWhenFinished` true, `PlayRate` and
    /// `TransientScaleModifier` 1).
    fn default() -> Self {
        Inst {
            anim: None,
            cur_time: 0.0,
            looping: false,
            finished: true,
            auto_release: true,
            blend_in_time: 0.0,
            blend_out_time: 0.0,
            blending_in: false,
            blending_out: false,
            cur_blend_in: 0.0,
            cur_blend_out: 0.0,
            play_rate: 1.0,
            base_scale: 0.0,
            transient_scale: 1.0,
            weight: 0.0,
            remaining: 0.0,
        }
    }
}

/// A play parameter, with `default` for a non-finite one (ours: the engine
/// takes whatever it is given, and a NaN blend time would keep an instance
/// from ever finishing; no shipped call passes one).
fn finite_or(v: f32, default: f32) -> f32 {
    if v.is_finite() { v } else { default }
}

/// `RemainingTime` for a `Duration` (positive) and blend-out time.
fn remaining_for(duration: f32, blend_out: f32) -> f32 {
    if 0.0 < duration {
        duration - blend_out
    } else {
        0.0
    }
}

impl Inst {
    fn finish(&mut self) {
        // `TermGroupInst` and the move-track references go; `bFinished`.
        self.finished = true;
    }

    /// `UCameraAnimInst::Stop`.
    fn stop(&mut self, immediate: bool) {
        if immediate || self.blend_out_time <= 0.0 {
            self.finish();
        } else {
            self.blending_out = true;
            self.cur_blend_out = 0.0;
        }
    }

    /// `UCameraAnimInst::Update` (a single-instance replay).
    fn update(&mut self, rate: f32, scale: f32, blend_in: f32, blend_out: f32, duration: f32) {
        let (rate, scale) = (finite_or(rate, 1.0), finite_or(scale, 1.0));
        let (blend_in, blend_out) = (finite_or(blend_in, 0.0), finite_or(blend_out, 0.0));
        let duration = finite_or(duration, 0.0);
        if self.blending_out {
            self.blending_out = false;
            self.cur_blend_out = 0.0;
            self.blending_in = true;
            // The engine computes the restart point of the blend-in from the
            // old blend-out timer, which it has just cleared: 1 · BlendInTime.
            self.cur_blend_in = blend_in;
        }
        self.play_rate = rate;
        self.base_scale = scale;
        self.blend_in_time = blend_in;
        self.blend_out_time = blend_out;
        self.remaining = remaining_for(duration, blend_out);
        self.finished = false;
    }

    /// `UCameraAnimInst::AdvanceAnim(dt, bJump = false)`: returns `false`
    /// when nothing ran (finished or no animation).
    fn advance(&mut self, dt: f32) -> bool {
        let Some(anim) = self.anim.clone() else {
            return false;
        };
        if self.finished {
            return false;
        }
        let t = self.play_rate * dt + self.cur_time;
        self.cur_time = t;
        if self.blending_in {
            self.cur_blend_in += dt;
        }
        if self.blending_out {
            self.cur_blend_out += dt;
        }
        let len = anim.length;
        let mut finished_now = false;
        if self.looping {
            if len < t {
                self.cur_time = t - len;
            }
        } else if t <= len {
            if len - self.blend_out_time < t {
                self.blending_out = true;
                self.cur_blend_out = t - (len - self.blend_out_time);
            }
        } else {
            finished_now = true;
        }
        if self.blending_in
            && self.blend_in_time <= self.cur_blend_in
            && self.cur_blend_in != self.blend_in_time
        {
            self.blending_in = false;
        }
        if self.blending_out && self.blend_out_time < self.cur_blend_out {
            self.cur_blend_out = self.blend_out_time;
            finished_now = true;
        }
        let w_in = if self.blending_in {
            self.cur_blend_in / self.blend_in_time
        } else {
            1.0
        };
        let w_out = if self.blending_out {
            1.0 - self.cur_blend_out / self.blend_out_time
        } else {
            1.0
        };
        let m = if w_in <= w_out { w_in } else { w_out };
        self.weight = m * self.base_scale * self.transient_scale;
        if !finished_now {
            if self.remaining <= 0.0 {
                return true;
            }
            self.remaining -= dt;
            if 0.0 < self.remaining {
                return true;
            }
            if 0.0 < self.blend_out_time {
                self.blending_out = true;
                self.cur_blend_out = 0.0;
                return true;
            }
        }
        self.finish();
        true
    }
}

/// The point of view a camera animation modifies (UE3 units).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Pov {
    /// Location, UU.
    pub location: [f32; 3],
    /// Rotation (pitch, yaw, roll), rotator units.
    pub rotation: [i32; 3],
    /// Horizontal FOV, degrees.
    pub fov: f32,
}

/// One animation's contribution to this frame's point of view.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CameraAnimSample {
    /// The instance.
    pub handle: CameraAnimHandle,
    /// `CurrentBlendWeight`.
    pub weight: f32,
    /// Animated camera-actor location, UU.
    pub location: [f32; 3],
    /// Animated camera-actor rotation.
    pub rotation: [i32; 3],
    /// Animated camera-actor `FOVAngle`.
    pub fov: f32,
}

/// The camera's animation pool (`ACamera` `ActiveAnims` / `FreeAnims`).
#[derive(Debug, Clone, PartialEq)]
pub struct CameraAnimPlayer {
    pool: Vec<Inst>,
    active: Vec<u8>,
    free: Vec<u8>,
    rand_seed: u32,
    frame: Vec<CameraAnimSample>,
}

impl Default for CameraAnimPlayer {
    fn default() -> Self {
        Self::new()
    }
}

fn trunc_units(v: f32) -> i32 {
    if v.is_nan() || v >= 2_147_483_648.0 || v < -2_147_483_648.0 {
        i32::MIN
    } else {
        v as i32
    }
}

/// `ApplyAnimToCamera` (camera-local play space) of one sample onto `pov`,
/// in the executable's operation order.
#[must_use]
pub fn apply_sample(pov: Pov, s: &CameraAnimSample) -> Pov {
    let scale = s.weight;
    let cam: Matrix = rotation_translation_matrix(pov.rotation, [0.0; 3]);
    let [xs, ys, zs] = s.location.map(|c| c * scale);
    let mut location = pov.location;
    for (j, l) in location.iter_mut().enumerate() {
        *l += ((0.0 + cam[2][j] * zs) + cam[1][j] * ys) + cam[0][j] * xs;
    }
    let scaled = s.rotation.map(|r| trunc_units(r as f32 * scale));
    let anim = rotation_translation_matrix(scaled, [0.0; 3]);
    let mut m: Matrix = [[0.0; 4]; 4];
    for (i, row) in m.iter_mut().enumerate().take(3) {
        for (j, cell) in row.iter_mut().enumerate().take(3) {
            *cell = (anim[i][2] * cam[2][j] + anim[i][1] * cam[1][j]) + anim[i][0] * cam[0][j];
        }
    }
    m[3][3] = 1.0;
    let fov = scale * (s.fov - CAMERA_ACTOR_DEFAULT_FOV) + pov.fov;
    // A sample that would leave the point of view non-finite (an absurd
    // scale or curve value) is dropped (ours: nothing downstream may see a
    // NaN camera).
    if !(fov.is_finite() && location.iter().all(|c| c.is_finite())) {
        return pov;
    }
    Pov {
        location,
        rotation: matrix_rotator(&m, false),
        fov,
    }
}

impl CameraAnimPlayer {
    /// A camera with its pool of [`MAX_ACTIVE_CAMERA_ANIMS`] free instances.
    #[must_use]
    pub fn new() -> Self {
        CameraAnimPlayer {
            pool: vec![Inst::default(); MAX_ACTIVE_CAMERA_ANIMS],
            active: Vec::new(),
            free: (0..MAX_ACTIVE_CAMERA_ANIMS as u8).collect(),
            rand_seed: 0,
            frame: Vec::new(),
        }
    }

    /// `ACamera::PlayCameraAnim`: `None` when the pool is exhausted (the
    /// animation does not play).
    #[allow(clippy::too_many_arguments)]
    pub fn play(
        &mut self,
        anim: &Arc<CameraAnim>,
        rate: f32,
        scale: f32,
        blend_in: f32,
        blend_out: f32,
        looping: bool,
        random_start: bool,
        duration: f32,
        single_instance: bool,
    ) -> Option<CameraAnimHandle> {
        if single_instance {
            for &i in &self.active {
                let Some(inst) = self.pool.get_mut(usize::from(i)) else {
                    continue;
                };
                if inst.anim.as_ref().is_some_and(|a| Arc::ptr_eq(a, anim)) {
                    inst.update(rate, scale, blend_in, blend_out, duration);
                    return Some(CameraAnimHandle(i));
                }
            }
        }
        let (rate, scale) = (finite_or(rate, 1.0), finite_or(scale, 1.0));
        let (blend_in, blend_out) = (finite_or(blend_in, 0.0), finite_or(blend_out, 0.0));
        let duration = finite_or(duration, 0.0);
        let i = self.free.pop()?;
        self.active.push(i);
        let start = if random_start {
            // The engine's float generator (seed · 0x0BB38435 + 0x3619636B,
            // mantissa as [1, 2)); our seed starts at 0.
            self.rand_seed = self
                .rand_seed
                .wrapping_mul(0x0BB3_8435)
                .wrapping_add(0x3619_636B);
            let f = f32::from_bits((self.rand_seed & 0x007F_FFFF) | 0x3F80_0000);
            (f - f.trunc()) * anim.length
        } else {
            0.0
        };
        let inst = self.pool.get_mut(usize::from(i))?;
        *inst = Inst {
            anim: Some(Arc::clone(anim)),
            cur_time: start,
            looping,
            finished: false,
            auto_release: true,
            blend_in_time: blend_in,
            blend_out_time: blend_out,
            blending_in: true,
            blending_out: false,
            cur_blend_in: 0.0,
            cur_blend_out: 0.0,
            play_rate: rate,
            base_scale: scale,
            transient_scale: 1.0,
            weight: 0.0,
            remaining: remaining_for(duration, blend_out),
        };
        Some(CameraAnimHandle(i))
    }

    /// `ACamera::StopCameraAnim` on whatever instance `handle` is now.
    pub fn stop(&mut self, handle: CameraAnimHandle, immediate: bool) {
        if let Some(inst) = self.pool.get_mut(usize::from(handle.0)) {
            inst.stop(immediate);
        }
    }

    /// `ACamera::StopAllCameraAnims`.
    pub fn stop_all(&mut self, immediate: bool) {
        for &i in &self.active {
            if let Some(inst) = self.pool.get_mut(usize::from(i)) {
                inst.stop(immediate);
            }
        }
    }

    /// `ACamera::StopAllCameraAnimsByType` (instances playing `path`).
    pub fn stop_all_by_type(&mut self, path: &str, immediate: bool) {
        for &i in &self.active {
            if let Some(inst) = self.pool.get_mut(usize::from(i))
                && inst
                    .anim
                    .as_ref()
                    .is_some_and(|a| a.path.eq_ignore_ascii_case(path))
            {
                inst.stop(immediate);
            }
        }
    }

    /// The camera update's animation part (`ApplyCameraModifiers`): every
    /// active instance advances by `dt` in pool order and records its sample
    /// for [`Self::apply`]; finished instances are released afterwards and
    /// every transient scale is reset.
    pub fn advance(&mut self, dt: f32) {
        self.frame.clear();
        let dt = if dt.is_finite() { dt } else { 0.0 };
        let mut k = 0usize;
        while let Some(&i) = self.active.get(k) {
            let Some(inst) = self.pool.get_mut(usize::from(i)) else {
                k += 1;
                continue;
            };
            if !inst.finished
                && inst.advance(dt)
                && 0.0 < inst.weight
                && let Some(anim) = &inst.anim
            {
                let (location, rotation, fov) = anim.sample(inst.cur_time);
                self.frame.push(CameraAnimSample {
                    handle: CameraAnimHandle(i),
                    weight: inst.weight,
                    location,
                    rotation,
                    fov,
                });
            }
            let release = inst.finished && inst.auto_release;
            inst.transient_scale = 1.0;
            if release {
                self.active.remove(k);
                self.free.push(i);
            } else {
                k += 1;
            }
        }
    }

    /// The latest [`Self::advance`]'s samples, in application order.
    #[must_use]
    pub fn samples(&self) -> &[CameraAnimSample] {
        &self.frame
    }

    /// The point of view after this frame's animations.
    #[must_use]
    pub fn apply(&self, pov: Pov) -> Pov {
        self.frame.iter().fold(pov, apply_sample)
    }

    /// Active instances (pool order).
    #[must_use]
    pub fn active(&self) -> Vec<CameraAnimHandle> {
        self.active.iter().map(|&i| CameraAnimHandle(i)).collect()
    }

    /// The animation path an instance plays (`None` when free or finished).
    #[must_use]
    pub fn playing(&self, handle: CameraAnimHandle) -> Option<&str> {
        let inst = self.pool.get(usize::from(handle.0))?;
        if inst.finished || !self.active.contains(&handle.0) {
            return None;
        }
        inst.anim.as_ref().map(|a| a.path.as_str())
    }
}

// ================================================================== script

/// Camera-animation calls of the gameplay script (CONFIRMED (src): the
/// `asamu` classes named per variant, read locally; parameters as passed:
/// rate 1, scale 1 (the `PlayCameraAnim` default, CONFIRMED (bytecode
/// default-parameter values)), no blend unless noted, no random start, no
/// duration).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GameplayCue {
    /// `GrappleGun.Grapple`: `GrappleBegin` once and `GrappleLoop` looping
    /// (kept for the release).
    GrappleAttached,
    /// `GrappleGun.ReleaseGrappleButton` (every release): stops the loop.
    GrappleReleased,
    /// `ASAMUPawn.Landed` (normal handler, floor not `NotLandable`):
    /// `HardLanding` when hard, else `NormalLand`.
    Landed {
        /// `V.z < −hardLandingThreshold`.
        hard: bool,
    },
    /// `ASAMUPowerJump.OnPowerJumpStartCharging`: the charge animation with
    /// 0.5 s blend in and out (kept as `ChargeCamInstance`).
    PowerJumpChargeStarted,
    /// `ASAMUPowerJump` state `Canceled`: stops the charge animation.
    PowerJumpCanceled,
    /// `ASAMUPowerJump.PlayCameraAnimation`: the jump or leap bob.
    PowerJumped {
        /// Power leap.
        leap: bool,
    },
    /// `ASAMURocketBoots` `Boosting` begin: the charge animation.
    BootsChargeStarted,
    /// `ASAMURocketBoots` after the charge: the boosting animation (kept for
    /// the end).
    BootsBoostBegan,
    /// The boost ran out: stops the boosting animation.
    BootsFinished,
    /// `ASAMURocketBoots.ResetBoots` (a landing cancels the boost; the
    /// pawn's death reset `ResetPlayer` calls it too while the boots are
    /// enabled): stops the boosting and charge animations.
    BootsReset,
    /// `ASAMUPlayerController.StartCameraShake` / `StopCameraShake` (the
    /// worm's scream): `MonsterGrowl` looping / stopped.
    WormGrowl {
        /// Start (else stop).
        start: bool,
    },
}

/// The camera animations of a running game: the data, the pool and the
/// instance references the script keeps.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct CameraAnims {
    set: Arc<CameraAnimSet>,
    player: CameraAnimPlayer,
    grapple_loop: Option<CameraAnimHandle>,
    power_jump_charge: Option<CameraAnimHandle>,
    boots_charge: Option<CameraAnimHandle>,
    boots_loop: Option<CameraAnimHandle>,
    growl: Option<CameraAnimHandle>,
}

impl CameraAnims {
    /// Camera animations over `set`.
    #[must_use]
    pub fn new(set: Arc<CameraAnimSet>) -> Self {
        CameraAnims {
            set,
            ..CameraAnims::default()
        }
    }

    /// The data.
    #[must_use]
    pub fn set(&self) -> &CameraAnimSet {
        &self.set
    }

    /// The pool.
    #[must_use]
    pub fn player(&self) -> &CameraAnimPlayer {
        &self.player
    }

    #[allow(clippy::too_many_arguments)]
    fn play_path(
        &mut self,
        path: &str,
        rate: f32,
        scale: f32,
        blend_in: f32,
        blend_out: f32,
        looping: bool,
        random_start: bool,
    ) -> Option<CameraAnimHandle> {
        let anim = Arc::clone(self.set.get(path)?);
        self.player.play(
            &anim,
            rate,
            scale,
            blend_in,
            blend_out,
            looping,
            random_start,
            0.0,
            false,
        )
    }

    /// A gameplay script call.
    pub fn cue(&mut self, cue: GameplayCue) {
        use paths::*;
        match cue {
            GameplayCue::GrappleAttached => {
                self.play_path(GRAPPLE_BEGIN, 1.0, 1.0, 0.0, 0.0, false, false);
                self.grapple_loop = self.play_path(GRAPPLE_LOOP, 1.0, 1.0, 0.0, 0.0, true, false);
            }
            GameplayCue::GrappleReleased => {
                if let Some(h) = self.grapple_loop {
                    self.player.stop(h, false);
                }
            }
            GameplayCue::Landed { hard } => {
                let path = if hard { HARD_LAND } else { NORMAL_LAND };
                self.play_path(path, 1.0, 1.0, 0.0, 0.0, false, false);
            }
            GameplayCue::PowerJumpChargeStarted => {
                self.power_jump_charge =
                    self.play_path(POWER_JUMP_CHARGE, 1.0, 1.0, 0.5, 0.5, false, false);
            }
            GameplayCue::PowerJumpCanceled => {
                if let Some(h) = self.power_jump_charge {
                    self.player.stop(h, false);
                }
            }
            GameplayCue::PowerJumped { leap } => {
                let path = if leap { POWER_LEAP_BOB } else { POWER_JUMP_BOB };
                self.play_path(path, 1.0, 1.0, 0.0, 0.0, false, false);
            }
            GameplayCue::BootsChargeStarted => {
                self.boots_charge = self.play_path(BOOTS_CHARGE, 1.0, 1.0, 0.0, 0.0, false, false);
            }
            GameplayCue::BootsBoostBegan => {
                self.boots_loop = self.play_path(BOOTS_BOOSTING, 1.0, 1.0, 0.0, 0.0, false, false);
            }
            GameplayCue::BootsFinished => {
                if let Some(h) = self.boots_loop {
                    self.player.stop(h, false);
                }
            }
            GameplayCue::BootsReset => {
                for h in [self.boots_loop, self.boots_charge].into_iter().flatten() {
                    self.player.stop(h, false);
                }
            }
            GameplayCue::WormGrowl { start: true } => {
                self.growl = self.play_path(WORM_GROWL, 1.0, 1.0, 0.0, 0.0, true, false);
            }
            GameplayCue::WormGrowl { start: false } => {
                if let Some(h) = self.growl {
                    // `StopCameraAnim(inst, false)`, then `inst.Stop()`.
                    self.player.stop(h, false);
                    self.player.stop(h, false);
                }
            }
        }
    }

    /// `SeqAct_PlayCameraAnim` "Play" on the player's camera
    /// (`USeqAct_PlayCameraAnim::Activated` → `PlayCameraAnim(anim, Rate,
    /// IntensityScale, BlendInTime, BlendOutTime, bLoop, bRandomStartTime)`).
    #[allow(clippy::too_many_arguments)]
    pub fn kismet_play(
        &mut self,
        path: &str,
        rate: f32,
        scale: f32,
        blend_in: f32,
        blend_out: f32,
        looping: bool,
        random_start: bool,
    ) -> bool {
        self.play_path(
            path,
            rate,
            scale,
            blend_in,
            blend_out,
            looping,
            random_start,
        )
        .is_some()
    }

    /// `SeqAct_PlayCameraAnim` "Stop": `StopAllCameraAnimsByType(anim,
    /// false)`.
    pub fn kismet_stop(&mut self, path: &str) {
        self.player.stop_all_by_type(path, false);
    }

    /// One camera update.
    pub fn advance(&mut self, dt: f32) {
        self.player.advance(dt);
    }

    /// The point of view after this frame's animations.
    #[must_use]
    pub fn apply(&self, pov: Pov) -> Pov {
        self.player.apply(pov)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::matinee::{CurveMode, CurvePoint, InterpMethod, MoveFrame};

    fn point3(t: f32, v: [f32; 3]) -> CurvePoint<[f32; 3]> {
        CurvePoint {
            in_val: t,
            out_val: v,
            arrive: [0.0; 3],
            leave: [0.0; 3],
            mode: CurveMode::Linear,
        }
    }

    /// A 1 s animation moving 0 → 10 UU forward and yawing 0 → 90°.
    fn anim(path: &str, length: f32) -> Arc<CameraAnim> {
        Arc::new(CameraAnim {
            path: path.into(),
            length,
            base_fov: 90.0,
            move_track: Some(MoveTrack {
                pos: InterpCurve {
                    points: vec![point3(0.0, [0.0; 3]), point3(1.0, [10.0, 0.0, 0.0])],
                    method: InterpMethod::default(),
                },
                euler: InterpCurve {
                    points: vec![point3(0.0, [0.0; 3]), point3(1.0, [0.0, 0.0, 90.0])],
                    method: InterpMethod::default(),
                },
                move_frame: MoveFrame::RelativeToInitial,
                ..MoveTrack::default()
            }),
            fov_track: Some(InterpCurve {
                points: vec![
                    CurvePoint {
                        in_val: 0.0,
                        out_val: 90.0,
                        arrive: 0.0,
                        leave: 0.0,
                        mode: CurveMode::Linear,
                    },
                    CurvePoint {
                        in_val: 1.0,
                        out_val: 100.0,
                        arrive: 0.0,
                        leave: 0.0,
                        mode: CurveMode::Linear,
                    },
                ],
                method: InterpMethod::default(),
            }),
        })
    }

    #[test]
    fn a_plain_animation_runs_to_its_end_and_is_released() {
        let a = anim("A", 1.0);
        let mut p = CameraAnimPlayer::new();
        let h = p
            .play(&a, 1.0, 1.0, 0.0, 0.0, false, false, 0.0, false)
            .unwrap();
        // The last free instance first.
        assert_eq!(h, CameraAnimHandle(7));
        p.advance(0.5);
        let s = p.samples()[0];
        // Blending in ended on the first advance (timer 0.5 > 0).
        assert_eq!(s.weight, 1.0);
        assert!((s.location[0] - 5.0).abs() < 1e-4);
        assert_eq!(s.rotation[1], (45.0f32 * 182.044_45) as i32);
        assert!((s.fov - 95.0).abs() < 1e-4);
        p.advance(0.5);
        assert_eq!(p.samples().len(), 1, "t = 1.0 is still inside");
        assert_eq!(p.playing(h), Some("A"));
        // Past the end: the animation finishes in this update. Its weight
        // was computed and its group evaluated before it was terminated, so
        // it still contributes this frame (the native applies any instance
        // whose weight is above 0 after `AdvanceAnim`), then it is released.
        p.advance(0.1);
        assert_eq!(p.samples().len(), 1);
        assert_eq!(p.samples()[0].weight, 1.0);
        assert!((p.samples()[0].location[0] - 10.0).abs() < 1e-4);
        assert!(p.active().is_empty());
        assert_eq!(p.playing(h), None);
        p.advance(0.1);
        assert!(p.samples().is_empty());
    }

    fn still(path: &str, length: f32, base_fov: f32) -> Arc<CameraAnim> {
        Arc::new(CameraAnim {
            path: path.into(),
            length,
            base_fov,
            move_track: None,
            fov_track: None,
        })
    }

    /// `AdvanceAnim`'s blend-in test is strict (`CurBlendInTime >
    /// BlendInTime`): a timer equal to the blend time still blends (at full
    /// weight), and a zero blend-in survives a zero-length update, where the
    /// weight is the blend-out side of the minimum (0/0 loses the
    /// comparison), i.e. full.
    #[test]
    fn blending_in_ends_strictly_after_its_time() {
        let a = still("A", 10.0, 90.0);
        let mut p = CameraAnimPlayer::new();
        let h = p
            .play(&a, 1.0, 1.0, 0.5, 0.0, false, false, 0.0, false)
            .unwrap();
        p.advance(0.5);
        assert_eq!(p.samples()[0].weight, 1.0);
        assert!(p.pool[usize::from(h.0)].blending_in, "0.5 is not above 0.5");
        p.advance(0.25);
        assert!(!p.pool[usize::from(h.0)].blending_in);
        assert_eq!(p.samples()[0].weight, 1.0);
        // Zero blend-in, zero-length update.
        let mut p = CameraAnimPlayer::new();
        let h = p
            .play(&a, 1.0, 1.0, 0.0, 0.0, false, false, 0.0, false)
            .unwrap();
        p.advance(0.0);
        assert!(p.pool[usize::from(h.0)].blending_in);
        assert_eq!(p.samples().len(), 1);
        assert_eq!(p.samples()[0].weight, 1.0);
        p.advance(1.0 / 60.0);
        assert!(!p.pool[usize::from(h.0)].blending_in);
    }

    /// Near the end the blend-out timer is *set* to the overshoot past
    /// `AnimLength − BlendOutTime` on every update (it is not the
    /// accumulated real time): with a play rate of 2 the two differ.
    #[test]
    fn the_end_blend_out_timer_is_the_overshoot_in_animation_time() {
        let a = still("A", 2.0, 90.0);
        let mut p = CameraAnimPlayer::new();
        p.play(&a, 2.0, 1.0, 0.0, 0.5, false, false, 0.0, false)
            .unwrap();
        p.advance(0.4); // t = 0.8
        assert_eq!(p.samples()[0].weight, 1.0);
        p.advance(0.4); // t = 1.6: 0.1 into the last 0.5
        assert!((p.samples()[0].weight - 0.8).abs() < 1e-5);
        p.advance(0.1); // t = 1.8: overshoot 0.3 (accumulated time would be 0.2)
        assert!(
            (p.samples()[0].weight - 0.4).abs() < 1e-5,
            "{}",
            p.samples()[0].weight
        );
        // t = 2.0 exactly is still inside (`CurTime > AnimLength` finishes);
        // the timer reaches the blend-out time without exceeding it: weight 0,
        // nothing applied, still active.
        p.advance(0.1);
        assert!(p.samples().is_empty());
        assert_eq!(p.active().len(), 1);
        p.advance(0.01);
        assert!(p.active().is_empty());
    }

    /// `PlayCameraAnim(..., bSingleInstance)` on a running animation calls
    /// `Update`: an instance that is blending out blends back in, with the
    /// blend-in timer already at the new blend-in time (so it is over on the
    /// next update); no second instance is taken.
    #[test]
    fn a_single_instance_replay_revives_a_blending_out_instance() {
        let a = still("A", 10.0, 90.0);
        let mut p = CameraAnimPlayer::new();
        let h = p
            .play(&a, 1.0, 1.0, 0.4, 0.4, true, false, 0.0, false)
            .unwrap();
        p.advance(1.0);
        p.stop(h, false);
        p.advance(0.1);
        assert!((p.samples()[0].weight - 0.75).abs() < 1e-5);
        let again = p
            .play(&a, 1.0, 0.5, 0.4, 0.4, true, false, 0.0, true)
            .unwrap();
        assert_eq!(again, h);
        assert_eq!(p.active().len(), 1);
        {
            let inst = &p.pool[usize::from(h.0)];
            assert!(inst.blending_in && !inst.blending_out && !inst.finished);
            assert_eq!(inst.cur_blend_in, 0.4);
        }
        p.advance(0.1);
        // Full blend, the new scale.
        assert_eq!(p.samples()[0].weight, 0.5);
        // A finished instance still in the active list is revived too.
        p.stop(h, true);
        assert_eq!(p.playing(h), None);
        p.play(&a, 1.0, 1.0, 0.0, 0.0, true, false, 0.0, true)
            .unwrap();
        assert_eq!(p.playing(h), Some("A"));
        // A different animation takes its own instance.
        let b = still("B", 10.0, 90.0);
        let hb = p
            .play(&b, 1.0, 1.0, 0.0, 0.0, true, false, 0.0, true)
            .unwrap();
        assert_ne!(hb, h);
    }

    /// The FOV offset is `weight · (animated FOVAngle − 90)`: an animation
    /// whose `BaseFOV` is not the `CameraActor` default shifts the FOV
    /// without any FOV track.
    #[test]
    fn the_fov_offset_is_measured_from_the_camera_actor_default() {
        let a = still("A", 10.0, 100.0);
        let mut p = CameraAnimPlayer::new();
        p.play(&a, 1.0, 0.5, 0.0, 0.0, true, false, 0.0, false)
            .unwrap();
        p.advance(0.1);
        let pov = Pov {
            location: [1.0, 2.0, 3.0],
            rotation: [100, 200, 0],
            fov: 80.0,
        };
        let out = p.apply(pov);
        assert!((out.fov - 85.0).abs() < 1e-5, "{}", out.fov);
        assert_eq!(out.location, pov.location);
        // The exact 90 leaves it alone.
        let b = still("B", 10.0, 90.0);
        let mut p = CameraAnimPlayer::new();
        p.play(&b, 1.0, 1.0, 0.0, 0.0, true, false, 0.0, false)
            .unwrap();
        p.advance(0.1);
        assert_eq!(p.apply(pov).fov, 80.0);
    }

    /// `AllocCameraAnimInst` pops the last free instance and appends it to
    /// the active list; `ReleaseCameraAnimInst` appends it to the free list:
    /// the instance released last is handed out next, and animations apply
    /// in the order they were started.
    #[test]
    fn the_pool_reuses_the_last_released_instance_and_applies_in_play_order() {
        let (a, b, c, d) = (
            still("A", 10.0, 91.0),
            still("B", 10.0, 92.0),
            still("C", 10.0, 93.0),
            still("D", 10.0, 94.0),
        );
        let mut p = CameraAnimPlayer::new();
        let play = |p: &mut CameraAnimPlayer, x: &Arc<CameraAnim>| {
            p.play(x, 1.0, 1.0, 0.0, 0.0, true, false, 0.0, false)
                .unwrap()
        };
        let (ha, hb, hc) = (play(&mut p, &a), play(&mut p, &b), play(&mut p, &c));
        assert_eq!(
            (ha, hb, hc),
            (
                CameraAnimHandle(7),
                CameraAnimHandle(6),
                CameraAnimHandle(5)
            )
        );
        p.stop(hb, true);
        p.advance(0.1);
        assert_eq!(p.active(), vec![ha, hc]);
        let hd = play(&mut p, &d);
        assert_eq!(hd, hb, "the instance released last");
        p.advance(0.1);
        let order: Vec<f32> = p.samples().iter().map(|s| s.fov).collect();
        assert_eq!(order, vec![91.0, 93.0, 94.0]);
        // By type: only that animation's instances stop.
        p.stop_all_by_type("c", true);
        p.advance(0.1);
        assert_eq!(p.active(), vec![ha, hd]);
    }

    /// A `Duration` counts down in real time and then blends out (or ends at
    /// once without a blend-out time); `RemainingTime` starts at `Duration −
    /// BlendOutTime`.
    #[test]
    fn a_duration_starts_the_blend_out_when_it_runs_out() {
        let a = still("A", 100.0, 100.0);
        let mut p = CameraAnimPlayer::new();
        let h = p
            .play(&a, 1.0, 1.0, 0.0, 0.5, true, false, 1.0, false)
            .unwrap();
        assert_eq!(p.pool[usize::from(h.0)].remaining, 0.5);
        p.advance(0.25);
        assert_eq!(p.samples()[0].weight, 1.0);
        p.advance(0.25); // runs out: blending out from 0, this frame still full
        assert_eq!(p.samples()[0].weight, 1.0);
        assert!(p.pool[usize::from(h.0)].blending_out);
        p.advance(0.25);
        assert!((p.samples()[0].weight - 0.5).abs() < 1e-6);
        p.advance(0.3);
        assert!(p.samples().is_empty());
        assert!(p.active().is_empty());
        // No blend-out time: the animation ends when the duration does (its
        // last frame still applies).
        let h = p
            .play(&a, 1.0, 1.0, 0.0, 0.0, true, false, 0.5, false)
            .unwrap();
        p.advance(0.25);
        assert_eq!(p.playing(h), Some("A"));
        p.advance(0.25);
        assert_eq!(p.samples().len(), 1);
        assert!(p.active().is_empty());
    }

    /// Non-finite play parameters fall back to the defaults and an absurd
    /// sample never makes the point of view non-finite.
    #[test]
    fn non_finite_parameters_and_samples_are_contained() {
        let a = anim("A", 1.0);
        let mut p = CameraAnimPlayer::new();
        let h = p
            .play(
                &a,
                f32::NAN,
                f32::INFINITY,
                f32::NAN,
                f32::NEG_INFINITY,
                true,
                false,
                f32::NAN,
                false,
            )
            .unwrap();
        {
            let inst = &p.pool[usize::from(h.0)];
            assert_eq!((inst.play_rate, inst.base_scale), (1.0, 1.0));
            assert_eq!((inst.blend_in_time, inst.blend_out_time), (0.0, 0.0));
            assert_eq!(inst.remaining, 0.0);
        }
        p.advance(0.5);
        let pov = Pov {
            location: [0.0; 3],
            rotation: [0; 3],
            fov: 90.0,
        };
        let out = p.apply(pov);
        assert!(out.location.iter().all(|c| c.is_finite()) && out.fov.is_finite());
        // A stop without a blend-out time ends it (a NaN blend-out would
        // have kept the instance forever).
        p.stop(h, false);
        p.advance(0.1);
        assert!(p.active().is_empty());
        // A sample with an absurd weight is skipped.
        let s = CameraAnimSample {
            handle: CameraAnimHandle(0),
            weight: f32::INFINITY,
            location: [0.0, 0.0, 1.0],
            rotation: [0; 3],
            fov: 90.0,
        };
        assert_eq!(apply_sample(pov, &s), pov);
        let huge = CameraAnimSample {
            weight: 1.0,
            location: [f32::MAX, f32::MAX, f32::MAX],
            fov: f32::NAN,
            ..s
        };
        assert_eq!(apply_sample(pov, &huge), pov);
        // A negative length in the data is read as 0.
        let mut set = CameraAnimSet::default();
        set.add_json(
            br#"{"camera_anims": [{"path": "N", "length": -5.0, "base_fov": 90.0,
                 "group": {"kind": "camera", "name": "G", "tracks": []}}]}"#,
        )
        .unwrap();
        assert_eq!(set.get("n").unwrap().length, 0.0);
    }

    /// The consequence MATINEE.md draws from the pool rules and the data:
    /// each normal landing holds an instance for the landing animation's
    /// length although it moves nothing, so eight of them inside that time
    /// leave no instance for a hard landing, which then does not play.
    #[test]
    fn normal_landings_can_exhaust_the_pool() {
        let mut set = CameraAnimSet::default();
        set.insert(CameraAnim {
            path: paths::NORMAL_LAND.into(),
            length: 3.0,
            base_fov: 90.0,
            move_track: None,
            fov_track: None,
        });
        set.insert(CameraAnim {
            path: paths::HARD_LAND.into(),
            ..(*anim("x", 0.75)).clone()
        });
        let mut c = CameraAnims::new(Arc::new(set));
        for _ in 0..MAX_ACTIVE_CAMERA_ANIMS {
            c.cue(GameplayCue::Landed { hard: false });
            c.advance(0.25);
        }
        assert_eq!(c.player().active().len(), MAX_ACTIVE_CAMERA_ANIMS);
        // Nothing moves: the landing animation has no tracks.
        assert!(c.player().samples().iter().all(|s| s.location == [0.0; 3]));
        c.cue(GameplayCue::Landed { hard: true });
        c.advance(0.25);
        assert!(
            c.player()
                .active()
                .iter()
                .all(|h| c.player().playing(*h) == Some(paths::NORMAL_LAND))
        );
        // The first landing's instance frees after its 3 s; a hard landing
        // then plays again.
        for _ in 0..5 {
            c.advance(0.25);
        }
        assert!(c.player().active().len() < MAX_ACTIVE_CAMERA_ANIMS);
        c.cue(GameplayCue::Landed { hard: true });
        c.advance(0.01);
        assert!(
            c.player()
                .active()
                .iter()
                .any(|h| c.player().playing(*h) == Some(paths::HARD_LAND))
        );
    }

    #[test]
    fn blends_follow_the_engine_timers() {
        let a = anim("A", 2.0);
        let mut p = CameraAnimPlayer::new();
        p.play(&a, 1.0, 1.0, 0.5, 0.5, false, false, 0.0, false)
            .unwrap();
        p.advance(0.25);
        assert!((p.samples()[0].weight - 0.5).abs() < 1e-6);
        p.advance(0.25);
        // Timer equal to the blend-in time: still blending (strict test).
        assert!((p.samples()[0].weight - 1.0).abs() < 1e-6);
        p.advance(1.0);
        assert_eq!(p.samples()[0].weight, 1.0);
        // t = 1.75: 0.25 into the 0.5 s blend-out.
        p.advance(0.25);
        assert!((p.samples()[0].weight - 0.5).abs() < 1e-6);
        // Past the blend-out: finishes (its last weight is 0 → nothing).
        p.advance(0.3);
        assert!(p.active().is_empty());
        // A scale multiplies the weight.
        p.play(&a, 1.0, 0.5, 0.0, 0.0, false, false, 0.0, false)
            .unwrap();
        p.advance(0.1);
        assert_eq!(p.samples()[0].weight, 0.5);
    }

    #[test]
    fn stop_blends_out_or_ends_and_stale_handles_hit_reused_instances() {
        let a = anim("A", 1.0);
        let b = anim("B", 1.0);
        let mut p = CameraAnimPlayer::new();
        let loop_h = p
            .play(&a, 1.0, 1.0, 0.0, 0.0, true, false, 0.0, false)
            .unwrap();
        for _ in 0..10 {
            p.advance(0.3);
        }
        assert_eq!(p.playing(loop_h), Some("A"), "looping keeps playing");
        // No blend-out time: Stop finishes at once.
        p.stop(loop_h, false);
        p.advance(0.1);
        assert!(p.active().is_empty());
        // The freed instance is the next one handed out: the stale handle
        // now stops `B`, as the script's kept reference would.
        let hb = p
            .play(&b, 1.0, 1.0, 0.0, 0.0, false, false, 0.0, false)
            .unwrap();
        assert_eq!(hb, loop_h);
        p.stop(loop_h, false);
        p.advance(0.1);
        assert_eq!(p.playing(hb), None);
        // With a blend-out time, Stop blends out from 0.
        let h = p
            .play(&a, 1.0, 1.0, 0.0, 0.4, true, false, 0.0, false)
            .unwrap();
        p.advance(0.1);
        p.stop(h, false);
        p.advance(0.2);
        assert!((p.samples()[0].weight - 0.5).abs() < 1e-6);
        p.advance(0.3);
        assert!(p.active().is_empty());
    }

    #[test]
    fn the_pool_runs_out_after_eight_instances() {
        let a = anim("A", 3.0);
        let mut p = CameraAnimPlayer::new();
        for _ in 0..MAX_ACTIVE_CAMERA_ANIMS {
            assert!(
                p.play(&a, 1.0, 1.0, 0.0, 0.0, false, false, 0.0, false)
                    .is_some()
            );
        }
        assert!(
            p.play(&a, 1.0, 1.0, 0.0, 0.0, false, false, 0.0, false)
                .is_none()
        );
        // Single instance: the running one is updated instead.
        assert!(
            p.play(&a, 1.0, 1.0, 0.0, 0.0, false, false, 0.0, true)
                .is_some()
        );
        p.stop_all(true);
        p.advance(0.0);
        assert!(p.active().is_empty());
        // A duration starts the blend-out when it runs out.
        let h = p
            .play(&a, 1.0, 1.0, 0.0, 0.5, true, false, 1.0, false)
            .unwrap();
        p.advance(0.4);
        assert_eq!(p.playing(h), Some("A"));
        p.advance(0.2);
        p.advance(0.6);
        assert!(p.active().is_empty());
    }

    #[test]
    fn application_rotates_into_camera_space() {
        let s = CameraAnimSample {
            handle: CameraAnimHandle(0),
            weight: 0.5,
            location: [10.0, 0.0, 0.0],
            rotation: [0, 16384, 0],
            fov: 100.0,
        };
        // Camera yawed 90°: forward 10 × 0.5 goes to +Y.
        let pov = Pov {
            location: [0.0; 3],
            rotation: [0, 16384, 0],
            fov: 90.0,
        };
        let out = apply_sample(pov, &s);
        assert!(out.location[0].abs() < 1e-4 && (out.location[1] - 5.0).abs() < 1e-4);
        // Half of 90° on top of 90°.
        assert_eq!(out.rotation, [0, 16384 + 8192, 0]);
        assert!((out.fov - 95.0).abs() < 1e-5);
        // Weight 0 changes nothing.
        let zero = apply_sample(pov, &CameraAnimSample { weight: 0.0, ..s });
        assert_eq!(zero.location, pov.location);
        assert_eq!(zero.rotation, pov.rotation);
    }

    #[test]
    fn camera_anim_json_loads_and_gameplay_cues_play() {
        let json = serde_json::json!({
            "format": CAMERA_ANIMS_FORMAT, "version": 1, "package": "Startup",
            "camera_anims": [
                {"path": paths::HARD_LAND, "length": 0.75, "base_fov": 90.0,
                 "group": {"kind": "camera", "name": "G", "tracks": [
                    {"class": "Engine.InterpTrackMove", "data": {"type": "move", "move_frame": "relative_to_initial",
                      "pos": {"points": [{"in": 0.0, "out": [0.0, 0.0, 0.0], "arrive": [0.0,0.0,0.0], "leave": [0.0,0.0,0.0], "mode": "linear"},
                                         {"in": 0.5, "out": [0.0, 0.0, -8.0], "arrive": [0.0,0.0,0.0], "leave": [0.0,0.0,0.0], "mode": "linear"}]},
                      "euler": {"points": [{"in": 0.0, "out": [0.0, 0.0, 0.0], "arrive": [0.0,0.0,0.0], "leave": [0.0,0.0,0.0], "mode": "linear"}]}}}
                 ]}},
                {"path": paths::GRAPPLE_LOOP, "length": 0.0, "base_fov": 90.0,
                 "group": {"kind": "camera", "name": "G", "tracks": []}},
                {"path": "No.Group", "length": 1.0, "base_fov": 90.0}
            ]
        });
        let mut set = CameraAnimSet::default();
        assert_eq!(
            set.add_json(&serde_json::to_vec(&json).unwrap()).unwrap(),
            2
        );
        assert!(set.add_json(b"not json").is_err());
        let mut c = CameraAnims::new(Arc::new(set));
        c.cue(GameplayCue::Landed { hard: true });
        c.advance(0.25);
        let pov = Pov {
            location: [0.0, 0.0, 100.0],
            rotation: [0; 3],
            fov: 90.0,
        };
        let out = c.apply(pov);
        assert!((out.location[2] - 96.0).abs() < 1e-3, "{:?}", out.location);
        // The grapple loop (no tracks, length 0) holds an instance until the
        // release stops it; the begin animation is missing from this set.
        c.cue(GameplayCue::GrappleAttached);
        c.advance(0.1);
        assert_eq!(c.player().active().len(), 2);
        c.cue(GameplayCue::GrappleReleased);
        c.advance(0.1);
        assert_eq!(c.player().active().len(), 1);
        // Unknown animations play nothing.
        assert!(!c.kismet_play("Missing.Anim", 1.0, 1.0, 0.0, 0.0, false, false));
        c.kismet_stop(paths::HARD_LAND);
        c.advance(0.01);
        assert!(c.player().active().is_empty());
    }
}
