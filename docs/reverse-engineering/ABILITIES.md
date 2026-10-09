# Movement abilities, landing, death and checkpoints — behavioural specification

Scope: the script layer of the player's movement on top of the native pawn physics
([`NATIVE_PHYSICS.md`](NATIVE_PHYSICS.md)): input → acceleration, walking/sprinting/story speed, the
jump and its variable height, air control parameters, landing, the power jump/power leap, the rocket
boots, the move-input lock, story mode and zoom, eye height/bob/camera/FOV, death, kill zones, respawn and
checkpoints, saving, and time-trial differences. The grapple is specified in [`GRAPPLE.md`](GRAPPLE.md)
(rules referenced as G-…).

**Publication rules, evidence method and confidence labels are the same as in `GRAPPLE.md`** (sections
"Confidence labels", "0. Evidence and reproduction" and "19. Verification pass"): rules come from local
reading of the shipped `ScriptText` (never reproduced here), class default objects decoded by two
independent decoders (CONFIRMED (cdo)), native code (CONFIRMED (native)) and config files. Behaviour is
described in our own words as rules, tables, timelines and formulas. Adversarially re-verified: §19.

## Most important findings

| # | Finding | Confidence |
|---|---|---|
| 1 | Walking speed **440 uu/s** (`ASAMUPawn.MoveSpeed`), sprint **880** (×2), story mode **264** (×0.6); acceleration 2048 uu/s² (`Pawn.AccelRate`); ground friction 8.0. | CONFIRMED (cdo, src) |
| 2 | **Jump `JumpZ` = 1000 uu/s.** Releasing the jump key while rising multiplies vertical speed by **0.7 immediately and again every 0.1 s** while it is above 0.05 — the variable jump height. No double jump, no dodge, no crouch. | CONFIRMED (cdo); CONFIRMED (src+bc) 0.7/0.1/0.05 |
| 3 | **No falling damage and no death by impact speed** (`TakeFallingDamage` overridden with nothing; the stock UT "slow to 10 % on hard landing" is disabled too); hard landing (`V.z < −2000`) is cosmetic. | CONFIRMED (src) |
| 4 | Air control is limited (`bLimitFallAccel` = true): `AirControl` **0.3 until the first normal landing, then 0.35** (every normal landing copies `UTPawn.DefaultAirControl`, which nothing writes). | CONFIRMED (cdo, src) |
| 5 | Power jump: hold 0.6 s, release while walking → **`V.z = 1600`**; if sprinting and moving → **power leap: horizontal velocity ×2, `V.z = 750`, move input locked until landing/grapple**. No cooldown. | CONFIRMED (cdo, src) |
| 6 | Rocket boots (jump key, only while falling, once per airtime): 1.0 s charge (velocity ×1.0/0.8/0.6/0.4/0.2 at 0.1 s steps, then 0.6 s with no writes), then a **2.05 s scripted boost along the aim direction from 2500 down to 1250 uu/s plus a decaying corkscrew of up to 1000 uu/s, one turn per second**, rewritten every 0.05 s. Landing re-arms. A jump-key release can damp the boost's vertical speed (A-RB-8). | CONFIRMED (cdo, src+bc) |
| 7 | Death = 0.3 s fade (pawn keeps simulating), then teleport to the latest checkpoint's spawn point with zero velocity. | CONFIRMED (src, cdo) |
| 8 | Terminal falling speed **10 000 uu/s** (`fTerminalVelocity` written into the pawn's physics volume at start); no AG map contains any physics/gravity volume. | CONFIRMED (cdo, src, census) |
| 9 | Script latent code (jump damping, boots, zoom, crystals) is **frame-quantised**; actors tick in the order controller → pawn → power jump → boots → gun (G-TM-2). | CONFIRMED (native) sleep; STRONG order |

## 1. Parameters

Defaults are inherited along `Pawn` → `GamePawn` → `UDKPawn` → `UTPawn` → `ASAMUPawn`; the most derived
value wins. "Runtime" marks values written by script after spawn. All CDO values below were reproduced
with `asamu-inspect defaults <pkg> asamu.ASAMUPawn --inherited` (and the classes named).

| Parameter | Effective value | Source chain | Confidence |
|---|---|---|---|
| `GroundSpeed` | 440 walking / 880 sprint / 264 story (runtime) | UTPawn CDO 440; runtime from `MoveSpeed` 440 (`ASAMUPawn` CDO) × multipliers (§2) | CONFIRMED (cdo, src) |
| `AccelRate` | 2048 | `Pawn` CDO | CONFIRMED (cdo) |
| `JumpZ` | 1000 | `ASAMUPawn` CDO (UTPawn 322, Pawn 420) | CONFIRMED (cdo) |
| `AirControl` | 0.3 → 0.35 after first normal landing | `ASAMUPawn` CDO 0.3; `UTPawn.DefaultAirControl` 0.35 copied on landing | CONFIRMED (cdo, src) |
| `bLimitFallAccel` | true | `Pawn` CDO; no script writes it — resolves NATIVE_PHYSICS.md open question 1 | CONFIRMED (cdo, src) |
| `AirSpeed` | 2000 (runtime) | UTPawn CDO 440; overwritten by the grapple gun (G-PH-4) | CONFIRMED (src) |
| `MaxFallSpeed` | 2500 | `ASAMUPawn` CDO (sound thresholds only) | CONFIRMED (cdo) |
| `bSprintSpeedMultiplier` / `storyModeSpeedMultiplier` | 2.0 / 0.6 | `ASAMUPawn` CDO | CONFIRMED (cdo) |
| `fTerminalVelocity` | 10 000 | `ASAMUPawn` CDO, written to the physics volume's `TerminalVelocity` at pawn start (volume default 4000) | CONFIRMED (cdo, src) |
| `hardLandingThreshold` / `normalLandSoundVelocityThreshold` | 2000 / 500 | `ASAMUPawn` CDO | CONFIRMED (cdo) |
| `fHardLandingVelocity`, `fLongJumpVerticalSpeed`, `fLongJumpHorizontalSpeed`, `jumpVelocityLowerMultiplier` | 2000, 700, 3000, 0.7 | `ASAMUPawn` CDO — **never read by any script** | CONFIRMED (cdo, src) |
| Collision cylinder | radius 21, half-height 44 | `UTPawn` `CollisionCylinder` template (ASAMU's template sets only an unrelated field) | CONFIRMED (cdo); template inheritance STRONG |
| `BaseEyeHeight` / `EyeHeight` | 38 / 38 | `UTPawn` CDO | CONFIRMED (cdo) |
| `MaxStepHeight` / `WalkableFloorZ` | 26 / 0.78 | `UTPawn` CDO (Pawn 35 / 0.7) | CONFIRMED (cdo) |
| `MaxJumpHeight` | 49 | `UTPawn` CDO (AI only) | CONFIRMED (cdo) |
| `LedgeCheckThreshold` | 4 | `Pawn` CDO | CONFIRMED (cdo) |
| `CustomGravityScaling` | 1.0 | `UDKPawn` CDO | CONFIRMED (cdo) |
| `SlopeBoostFriction` | 0.2 | `UTPawn` CDO | CONFIRMED (cdo) |
| `MovementSpeedModifier`, `WalkingPct`, `CrouchedPct` | 1.0, 0.4, 0.4 | `Pawn` / `UTPawn` CDO; walking-slowly and crouching never happen (A-JP-2, A-JP-4) | CONFIRMED (cdo, src) |
| `ViewPitchMin` / `ViewPitchMax` | −18 000 / 18 000 (rotator units, ≈ ±98.9°) | `UTPawn` CDO | CONFIRMED (cdo) |
| `MaxLeanRoll` | 2048 | `UTPawn` CDO (mesh lean, visual) | CONFIRMED (cdo) |
| `Bob`, `bWeaponBob` | 0.01, true | `[UTGame.UTPawn]` in `DefaultGame.ini` and CDO | CONFIRMED (config, cdo) |
| Gravity | `DefaultGravityZ` −520 | `DefaultGame.ini` and `WorldInfo` CDO; no AG map `WorldInfo` overrides gravity | CONFIRMED (config, map decode) |
| `GroundFriction` / `FluidFriction` / `ZoneVelocity` | 8.0 / 0.3 / 0 | `PhysicsVolume` CDO; only the default volume exists at runtime | CONFIRMED (cdo, census) |
| `bZoomEnabled`, `zoomFOV`, `zoomDuration` | true, 50, 0.3 s | `ASAMUPawn` CDO | CONFIRMED (cdo) |
| Bob speed multipliers | story 0.55, sprint 0.85, walk 0.65 | `ASAMUPawn` CDO | CONFIRMED (cdo) |
| Death fades | down 0.3 s, up 0.3 s | `ASAMUPawn` CDO | CONFIRMED (cdo) |
| FOV | 90 (setting `FOV`, locked on the camera) | `DefaultSettings.ini` `[ASAMU.ASAMUSettingsManager]` | CONFIRMED (config) |

Map census (export tables; class names only) [CONFIRMED]: no AG map contains a `PhysicsVolume` subclass,
gravity, water or ladder volume (only Blocking, DynamicBlocking, Trigger, DynamicTrigger, PostProcess,
Reverb and LightmassImportance volumes). Gameplay actors per map:

| Map (Title) | KillZ | Kill zones | Dyn. kill zones | Checkpoints | Recharge crystals | Falling rocks / when-grappled | Glow flowers | Attractor pads |
|---|---|---|---|---|---|---|---|---|
| AG-Workshop (Workshop) | 1.0 | 0 | 0 | 1 | 0 | 0 / 0 | 0 | 1 |
| AG-ParadiseCave (Sanctuary) | −1e9 | 26 | 0 | 24 | 0 | 0 / 0 | 0 | 0 |
| AG-BeautifulCity (Village) | −1e10 | 7 | 0 | 12 | 0 | 0 / 0 | 0 | 0 |
| AG-Darkcave (DarkCave) | −1e10 | 12 | 0 | 17 | 0 | 0 / 0 | 15 | 0 |
| AG-StarHaven (StarHaven) | −1e7 | 13 | 0 | 25 | 7 | 0 / 0 | 0 | 0 |
| AG-IceCave (IceCave) | −1e7 | 33 | 3 | 28 | 98 | 134 / 32 | 0 | 0 |
| AG-Epilogue (Epilogue) | 1.0 | 0 | 0 | 1 | 0 | 0 / 0 | 0 | 0 |
| TheCore | −262 143 (`ZoneInfo` default) | 0 | 0 | 0 | 0 | 0 / 0 | 0 | 1 |

KillZ values: CONFIRMED (map decode, both decoders).

## 2. Walking, sprinting, story speed

**A-WK-1 Acceleration from input** [CONFIRMED (src, stock `PlayerController.PlayerWalking`)]: each controller
tick in `PlayerWalking` (walking and falling) the wish vector is `aForward·X + aStrafe·Y` with X/Y the axes
of the **pawn** rotation (its pitch is forced to 0 while walking/falling by `UTPawn.FaceRotation`; the axes
are taken before that tick's rotation update), Z removed, normalised, times `AccelRate` (2048). Analog
magnitude is lost (also natively, NATIVE_PHYSICS.md finding 2). While the move-input lock is active (§8) the
input layer zeroes the move axes first, so the acceleration is zero. The value is written to the pawn, then
the jump check runs (§3).
→ Rust: `accel = AccelRate · normalize_or_zero(fwd·X + strafe·Y)` with yaw-only axes.

**A-WK-2 Ground physics** is the native walking of NATIVE_PHYSICS.md §2–3 with `MaxSpeed = GroundSpeed`,
`Friction = 8.0`.

**A-WK-3 `GroundSpeed` writers** [CONFIRMED (src); map strings CONFIRMED (map decode)]:

| Event | New `GroundSpeed` | Also |
|---|---|---|
| pawn start | 440 (`MoveSpeed`) | not sprinting |
| sprint applied | 880 (`MoveSpeed × 2.0`) | sprint flag set |
| sprint removed | 440 | sprint flag cleared |
| enter story mode | 264 (`MoveSpeed × 0.6`) | sprint stopped first |
| exit story mode | 440 | — |
| controller `ResetPlayer` exec | 440 | sprint stopped, power jump cancelled, velocity 0, AirSpeed 2000, grapple released |
| cheat `SetSpeed F` (the controller's `GameCheatManager` exists in standalone games) | `440 × F` (class default × F; water speed too, not AirSpeed) | used by map Kismet: Workshop `setspeed 0.3` (132); Epilogue `SetSpeed 0.15` (66) and `setspeed 0.3` (132) |

Any later sprint apply/remove or story transition overwrites a `SetSpeed` value with the 440-based values;
whether the Workshop/Epilogue sequences prevent sprinting is UNKNOWN.

**A-WK-4 Sprint state machine** [CONFIRMED (src, config)] (`StartSprinting`/`StopSprinting`, left shift /
gamepad left shoulder):
1. Press while Falling or Flying → only arm "sprint after landing".
2. Press otherwise → apply sprint immediately (also when standing still).
3. Release → remove sprint, disarm "sprint after landing".
4. A jump attempt while sprinting (§3) removes the sprint and arms "sprint after landing".
5. A normal landing applies the sprint if armed, then disarms (§5).
6. In story mode both keys are ignored.
7. The sprint camera animation is effectively **never started** (its start condition requires an already
   running instance, which nothing creates).
→ Rust: `SprintState { active, armed }` with exactly these transitions; reproduce the overwrite semantics.

## 3. Jumping

**A-JP-1 Input** [CONFIRMED (config + src)]: space bar / gamepad A run `Jump` on press and `ReleaseJump` on
release, **and** `RocketBoostKeyDown` on press (§7). `ASAMUPlayerInput.Jump` only sets the controller's
jump-pressed flag (no pause handling); the boost handler runs immediately in the input event. In
`PlayerWalking` the flag is consumed by the next controller move (the pawn never refuses a jump request, so
it is never kept), which calls the pawn's jump attempt. In `Grappling` the flag is discarded (G-PH-1).
`ReleaseJump` exists only on the pawn and only does something in script state `Jumped` (A-JP-3).

**A-JP-2 Jump attempt** (`ASAMUPawn.DoJump`) [CONFIRMED (src)]
- Always, whether or not it succeeds: the sprint camera animation is stopped; an active sprint is removed
  (`GroundSpeed` 440) and "sprint after landing" armed; the pawn's script state becomes `Jumped`.
- Succeeds only in Walking physics (ladder/spider branches exist but no map has ladders and spider physics
  is never used; crouching is never requested; the pawn is always jump-capable).
- On success: `V.z := JumpZ` (horizontal velocity untouched); if the floor is a non-world-geometry actor
  moving upward, its upward speed is added; physics → Falling; jump sound and hand animation unless a power
  jump is executing or workshop mode is on. The stock "walking slowly uses the default JumpZ" variant is
  unreachable: `UTPawn.SetWalking` is overridden with nothing, so the walking-slowly flag never becomes true.
- In story mode the attempt does nothing at all (overridden with an empty function).

**A-JP-3 Variable jump height** [CONFIRMED (src+bc)]: only in script state `Jumped`, `ReleaseJump` switches to
`ReleasedJump`, whose latent loop, at each wake-up, multiplies `V.z` by 0.7 and sleeps 0.1 s if `V.z > 0.05`,
otherwise ends in `FallingState` (wind sound). The first multiplication happens in the pawn's state code of
the same frame (key events precede the pawn tick; state code precedes physics, G-TM-1/2). The pawn leaves
`Jumped`/`ReleasedJump` on landing (`HasLanded`), grapple attach (`Shooting`), grapple release (`Release`),
power jump/leap (`FallingState`), story mode — and on nothing else; in particular **not** when a rocket boost
starts (A-RB-8). Consequences:
- A failed mid-air jump attempt (Space while falling) re-enters `Jumped`, so **tapping Space in the air while
  rising damps the upward speed**.
- Power jumps and leaps are not damped (they end in `FallingState`).
- Model (re-simulated; 60 fps, effective gravity −1040 uu/s² from NATIVE_PHYSICS.md §4.6, flat ground): full
  jump apex ≈ 481 uu at ≈ 0.96 s; releasing at 0 / 0.1 / 0.2 / 0.4 / 0.7 s gives apexes ≈ 121 / 198 / 266 /
  371 / 461 uu. To be trace-verified.
→ Rust: pawn script state enum `{Idle, IdleIdle, Running, Shooting, Jumped, ReleasedJump, Release,
FallingState, HasLanded, StoryState, Zooming}` (only `Jumped`, `ReleasedJump`, `StoryState`, `Zooming` are
behavioural) and a latent-sleep primitive (§15).

**A-JP-4 No double jump, dodge or crouch** [CONFIRMED (src)]: the controller derives from
`UDKPlayerController`, not `UTPlayerController`, so the stock walking state never requests dodges or double
jumps; the jump attempt has no multi-jump branch; no script asks the pawn to crouch.

**A-JP-5 Workshop/Normal mode execs** [CONFIRMED (src, map decode)]: `WorkshopMode` sets `JumpZ = 150` and
suppresses jump sounds and hand lights; `NormalMode` sets `JumpZ = 1000`. No map issues `WorkshopMode`;
Sanctuary's Kismet issues `NormalMode` (no change from the default).

## 4. Air control

**A-AC-1** [CONFIRMED (cdo) + CONFIRMED (native logic)] While falling, the native limiter of NATIVE_PHYSICS.md
§4.2 is active: the input acceleration (2048 or 0) is clamped to `AccelRate × AirControl` = **614.4 uu/s²
(0.3) before the first normal landing, 716.8 uu/s² (0.35) after**, plus the native low-speed help below
10 uu/s, the "no air control into walls" probe, and the `BoundSpeed` rule (no horizontal speed gain above
`GroundSpeed` — 440, or 880 only if the sprint is still active, which a jump removes). The native falling
refinement doubles the effective air acceleration (NATIVE_PHYSICS.md §4.6).

**A-AC-2** [CONFIRMED (src)] A new pawn starts walking, so 0.3 lasts until the first normal (non-story,
non-`NotLandable`) landing. Story-mode landings do not reset it. Nothing else writes `AirControl` (the game
info's player-defaults hook is overridden with nothing).
→ Rust: `air_control` is runtime state, initialised 0.3, set to 0.35 by the normal landing handler.

## 5. Landing

Native `processLanded` (NATIVE_PHYSICS.md §5.2) calls the controller (not handled), then the pawn's
`Landed(HitNormal, FloorActor)`; afterwards the native code sets walking physics if health > 0. `V` below is
the landing velocity left by the native code (NATIVE_PHYSICS.md §4.4). Effects per handler [CONFIRMED (src);
thresholds CONFIRMED (cdo); impulse factor CONFIRMED (src+bc)]:

| Effect | Normal landing (`Landed`) | Story mode (`StoryState.Landed`) | Floor actor tagged `NotLandable` |
|---|---|---|---|
| Grapple budget reset (G-CT-4) | yes | yes | **no** |
| Rocket boots: re-arm, or cancel an active boost (A-RB-5/6) | yes | yes | **no** |
| Power-jump landing hook (no effect in practice, §6) | yes | yes | no |
| Move-input lock −1 (§8) | yes | yes | **no** |
| Sprint after landing | applied if armed, then disarmed | applied if armed, stays armed | no |
| `AirControl` := 0.35 | yes | no | no |
| Pawn script state → `HasLanded` (ends jump damping; → `Idle` after 1 s) | yes | no (stays story) | no |
| Hard-landing effects when `V.z < −2000` (camera anim `HardLanding`, rumble, particle, decal) | yes, cosmetic | no | no |
| Normal landing camera anim (`NormalLand`) otherwise | yes, cosmetic | no | no |
| Impulse `(0, 0, 4·V.z)` to a `DynamicSMActor` floor at the pawn location | yes | no | no |
| `V.z < −200`: eye-smoothing baseline := current Z (the stock landing dip never starts: the controller's `LandingShake` is the stock "false") | yes | no | no |
| `V.z ≤ −500`: landing sound + grunt + Kismet `SeqEvent_PlayerLanded` (only if the pawn actor is not hidden); the extra cue paths (`V.z < −2500`, else `< −1250`) play **nothing** in ASAMU (see below) | yes | yes | no |
| `V.z ≤ −500`: `BaseEyeHeight` reset to 38 | yes | no | no |
| Falling damage / stock UT horizontal ×0.1 landing slowdown | **never** (empty override / disabled) | never | never |
| Physics → Walking (native, health > 0) | yes | yes | yes |

The `NotLandable` check is the first thing the normal handler does; the story handler has no such check.

**Correction (2026-10-10): the hard-landing extra cue plays nothing.** After the normal landing sound the landing
handler picks one extra sound by speed: below `−MaxFallSpeed` (−2500) the sound group's falling-damage landing
sound, whose ASAMU override (`ASAMUSoundGroup`) has an empty body; else below `−0.5·MaxFallSpeed` (−1250) the sound
group's stock land sound, which no ASAMU class default sets. Neither makes a sound. An earlier version of the table
listed an extra cue for `V.z < −2500` / `< −1250` as if it sounded. STRONG (local reading of `ASAMUPawn` and
`ASAMUSoundGroup` and of the class defaults; the runtime plays only the normal landing sound and grunt).
The tag name occurs in the IceCave and StarHaven name tables.
→ Rust: `on_landed(normal, floor) -> LandingOutcome` with the `NotLandable` early-out first; `asamu-game`
receives the event and sound triggers.

## 6. Power jump and power leap (`ASAMUPowerJump`)

States: `Ready` (initial), `Charging`, `Jumping`, `Canceled`, and `Unavailable` (declared, **never
entered**). The actor is spawned and based on the pawn at pawn start.

**A-PJ-1 Input** [CONFIRMED (config + src)]: right mouse button / gamepad right shoulder → key-down/key-up
(controller → pawn → power-jump actor). In story mode the pawn turns key-down into zoom (§9) and ignores
key-up.

**A-PJ-2 Charge** [CONFIRMED (src, cdo)]: key-down in `Ready` (not in cinematic mode) → `Charging`; after a
latent sleep of **0.6 s** (`powerJumpChargeTime`) the charge is complete (hand light on). Charging does not
depend on physics: it can start while falling or attached; movement, sprint and jumping remain available.
Cosmetics: charge sound, rumble, camera animation `Zeth_CameraStuffs.PowerJumpChargeCameraAnim` (0.5 s blend
in/out), particle.

**A-PJ-3 Release** [CONFIRMED (src)]: key-up in `Charging`: if not yet charged **or** physics is not Walking →
cancel (`Canceled` stops the charge camera animation, fades sounds, → `Ready`); otherwise → `Jumping`. Key-up
in any other state is ignored.

**A-PJ-4 Execution** [CONFIRMED (src, cdo)] — done by the `Jumping` state code at the power-jump actor's next
state-code run (same frame, after the pawn's physics, G-TM-2):
- **Choice**: leap if the pawn's sprint flag is set and any velocity component (including Z) is non-zero;
  otherwise vertical power jump.
- **Vertical**: a jump attempt (A-JP-2) with `JumpZ` temporarily 1600 (`powerJumpStrength`) → `V.z = 1600`
  (+ upward base speed), horizontal unchanged; nothing happens if physics is no longer Walking.
- **Leap**: move-input lock +1 (§8); a normal jump attempt (removes sprint, arms sprint-after-landing); then,
  unconditionally, `V.x, V.y` doubled (`powerLeapHorizontalStrengthMultiplier` 2.0) and `V.z := 750`
  (`powerLeapVerticalStrength`), replacing the jump's 1000.
- **Both**: the pawn's script state is then forced to `FallingState` (so no jump-release damping), the actor
  returns to `Ready`. Cosmetics: takeoff particle, sounds, `PowerJumpBob`/`PowerLeapBob` camera animations
  (parkour variants front/back flip), hand animation.

**A-PJ-5 No cooldown** [CONFIRMED (src)]: nothing enters `Unavailable`; landing does nothing in `Ready`. The
only limits are the 0.6 s charge and "walking at release". Story entry and the `ResetPlayer` exec force
`Canceled`.

**A-PJ-6 Numbers** [model]: vertical apex ≈ 1600²/(2·1040) ≈ 1231 uu; leap apex ≈ 270 uu; leap horizontal
speed = 2 × current horizontal speed (≈ 1760 uu/s from a full sprint at 880); the lock makes air
acceleration zero, so the horizontal velocity is preserved until landing, a grapple (G-AT-5) or the end of
a rocket boost (§8).
→ Rust: `PowerJump { state, charged, timer }` on the latent scheduler; the leap/jump decision uses the
pawn's sprint flag and raw velocity components.

## 7. Rocket boots (`ASAMURocketBoots`)

States: `Ready` (initial), `Boosting`, `Unavailable`, `UnavailableAndPlayedSound`. Spawned and based on the
pawn at pawn start. The enable flag starts **false**.

**A-RB-1 Enabling** [CONFIRMED (src, map decode)]: `EnableRocketBoots(bool)` from Kismet
`SeqAct_ToggleRocketBoots` (`Enable`, default true; no linked variables in the maps) or save-game load.
Instances: disable actions in Sanctuary, Village, DarkCave and IceCave (one each); enable actions in
StarHaven (3) and IceCave (3). Which fire when is the Kismet workstream's job.

**A-RB-2 Input** [CONFIRMED (config + src)]: the jump key's press also runs the boost handler, immediately. In
`Ready` a boost starts only if physics is **Falling**, the boots are enabled and the pawn is not in story
mode (incl. zoom). On the ground the jump wins (physics is still Walking when the key event runs; the jump
itself happens at the next controller move); while attached nothing happens (Flying).

**A-RB-3/4 Boost timeline** [CONFIRMED (src+bc); values CONFIRMED (cdo): `boostDelay` 1.0, `boostDuration`
2.0, `boostStrength` 2500, `boostSpiralStrength` 1000, `totalSpinAngle` 720, `boostExhaustedDelay` 1.0].
τ = time since the `Boosting` state code started (the boots' state-code run in the key-press frame, after
the pawn's physics). Waits are latent sleeps (frame-quantised, G-TM-3); every velocity write is an absolute
assignment (plus the corkscrew addition) integrated by the next frame's physics. τ starts at 0 for every
boost because state-scoped variables are zeroed on entering the state (G-TM-5).

| Phase | τ (nominal) | Velocity | Other effects |
|---|---|---|---|
| Charge, 5 writes | 0, 0.1, 0.2, 0.3, 0.4 | `V := V0 · (1 − 2τ/boostDelay)` = V0 × 1.0, 0.8, 0.6, 0.4, 0.2 (V0 = velocity at τ = 0). The float accumulation of 0.1 reaches exactly 0.5 after five steps, so there is no sixth (zero) write. | at τ = 0: Kismet `SeqEvent_PlayerRocketBoosted` output 0; move-input lock +1 **only if it was 0**; charge camera animation `rocketBoots.RocketBootsBegin`; sound, rumble, hand animation |
| No writes | 0.4 → 1.0 (0.1 s wait, then 0.5 s wait) | native falling physics from ≈ 0.2·V0: gravity; air control normally zero because of the lock | — |
| Boost, 41 writes | 1.0 + 0.05·i, i = 0…40 (the float accumulator passes 2.0 only after the 41st write, at boost time ≈ 1.99999) | with boost time u ∈ [0, 2) and s = (2 − u)/2: `V := â·2500·(1 + s)/2 + R(â)·(s, 1000·s·sin θ, 1000·s·cos θ)`, θ = 2π·u (one turn per second; the degree-to-radian helper is misnamed); `â` = unit adjusted aim (camera POV rotation, G-TG-1) sampled once at the first boost write; `R(â)` rotates local X onto `â` with zero roll, so at u = 0 the offset points along local up | at the first write: Kismet output 1, trail particle, `RocketBootsCameraLensEffect`, camera animation `RocketBootsBoosting` (parkour: roll), sounds, rumble |
| End | ≈ 3.05 | last write left in place (≈ 1250·â, corkscrew ≈ 0); normal falling continues | boosting camera animation stopped, trail off, move-input lock −1 (floor 0), → `Unavailable` |

Break rule: at a boost write while the grapple is attached, if the anchor distance last measured by the gun
exceeds 5000 uu, that write keeps only the aim term (no corkscrew) and the boost jumps to End (G-IX-3).

**A-RB-5 Re-arming** [CONFIRMED (src)]: `Unavailable`/`UnavailableAndPlayedSound` + landing → `Ready`. A key
press in `Unavailable` plays the "exhausted" sound and enters `UnavailableAndPlayedSound`, which returns to
`Unavailable` after 1.0 s — sound throttling only. So: **one boost per airtime, refilled by any landing
except on `NotLandable` floors** (A landing on such a floor during a boost does not cancel it: the boost keeps
writing velocity while the pawn walks.)

**A-RB-6 Landing during a boost** [CONFIRMED (src)]: cancels immediately (sounds, rumble, hand animation;
camera animations stopped, trail off, → `Ready`). The landing handler's lock decrement (§5) releases the
boost's lock. Death respawn also resets the boots to `Ready` when they are enabled (§11).

**A-RB-7 Grapple interplay**: G-IX-3/G-IX-4 (boost continues while attached, pull suspended, flying drag and
the 2000 cap apply to the scripted velocity).

**A-RB-8 Jump-release damping during a boost** [CONFIRMED (src) logic; frame-level effect STRONG] (added in
the verification pass): the press that starts a boost also makes the next controller move attempt a jump,
which fails (falling) but puts the pawn into `Jumped`; releasing the key then runs the A-JP-3 damping loop.
The boots never change the pawn's script state, so both run side by side:
- Tap released during the charge: each damping step multiplies the current `V.z` by 0.7; the loop ends at
  the first wake-up with `V.z ≤ 0.05` (immediately if the pawn was already falling, otherwise usually
  during the charge or the no-write phase as gravity acts).
- Key held past τ = 1.0 and released during the boost: the boost's vertical component is usually positive
  at first (the corkscrew starts pointing up; `V.z > 0` at the first write for any aim pitch above ≈ −21.8°),
  so the damping multiplies it by 0.7 every 0.1 s — the pawn's state code runs before physics and the boots
  rewrite velocity after it every 0.05 s, so each damping step shortens the vertical displacement of the
  frames up to the next boost write. The loop ends at the first wake-up where the rotating corkscrew has
  made `V.z ≤ 0.05` (within about half a corkscrew turn for level aim).
→ Rust: `RocketBoots { state, enabled, step, v0, aim }` on the latent scheduler; absolute writes except the
corkscrew offset; the pawn's damping loop must stay independent of the boots.

## 8. Move-input lock

**A-IL-1** [CONFIRMED (src, stock `PlayerController.IgnoreMoveInput`)] The lock is a **counter**: "ignore"
adds 1, "unignore" subtracts 1 with a floor of 0; while it is > 0 the input layer zeroes the forward,
strafe and up axes (look, jump, fire and ability keys still work).

| Writer | Effect |
|---|---|
| power leap (A-PJ-4) | +1 |
| rocket boost charge start, only if the counter is 0 | +1 |
| rocket boost end | −1 |
| any landing handler except on `NotLandable` floors (§5) | −1 |
| grapple attach (G-AT-5) | −1 |
| stock cinematic mode toggles (Kismet) | +1/−1 [TENTATIVE, stock behaviour] |

Consequences: a boost started during a power leap adds nothing (counter already 1) and its end removes the
leap's lock, restoring air control before landing [CONFIRMED (src)]; because landings decrement
unconditionally, a landing during a cinematic could release a cinematic lock early [TENTATIVE].
→ Rust: `move_input_lock: u8` with saturating decrement.

## 9. Story mode and zoom

**A-ST-1 Enter** (`EnterStoryState`: Kismet `SeqAct_ToggleStoryMode`, death with "spawn in story mode")
[CONFIRMED (src)]: stop sprinting, release the grapple, reset hand animations, `GroundSpeed` = 264, cancel the
power jump, pawn state → `StoryState` (hand lowered but still "visible" for G-AC-3; story crosshair).

**A-ST-2 In story mode**: jump attempts do nothing, sprint keys ignored, the power-jump key starts zoom if
enabled, fire interacts within 200 uu (G-AC-0), rocket boots blocked, special landing handler (§5). Walking
physics, air control and gravity are unchanged.

**A-ST-3 Exit** (`ExitStoryState`): `GroundSpeed` = 440, gameplay crosshair, hand shown (animated), animations
reset, state → `Idle`. From `Zooming` the settings FOV is restored first.

**A-ST-4 Zoom** (`Zooming`, pushed on top of `StoryState`; `SeqAct_ToggleZoomAvailable` toggles availability)
[CONFIRMED (src+bc) 0.016; CONFIRMED (cdo)]: with `B` = settings FOV (default 90), `Z` = 50, `T` = 0.3 s,
step `h` = 0.016 s:
- zoom in: 19 steps `i = 0…18` (all i with `i < T/h = 18.75`) setting `FOV = B − (B − Z)·(h/T)·i`, each
  followed by a latent sleep of h (one frame at ≤ 62.5 fps); then `FOV = Z` and hold;
- key-up (or zoom disabled) → zoom out from the current FOV `F` in 18 steps `i = 18…1` with
  `FOV = B − (B − F)·(h/T)·i`, then `FOV = B` and back to `StoryState`; a key-up during zoom-in switches to
  zoom-out from the FOV reached.
- Look sensitivity is **not** scaled by FOV while zoomed (`bEnableFOVScaling` false in the `PlayerInput` CDO,
  never set) [CONFIRMED (cdo, src)].

## 10. Eye height, bob, camera, FOV

**A-CM-1 View point** [CONFIRMED (src)]: camera location = `Location + (0, 0, EyeHeight) + WalkBob`
(`UTPawn` view location, eye updates enabled once the pawn is the local view target); rotation = controller
rotation, then camera animations/modifiers (stock camera, native). The weapon trace and the boost aim use
the same location and the camera's POV rotation (G-TG-1).

**A-CM-2 View pitch** [CONFIRMED (cdo)]: limited to [−18 000, 18 000] rotator units (≈ ±98.9°).

**A-CM-3 FOV** [CONFIRMED (config + src)]: the settings manager issues `FOV <setting>` at login (90 by default;
≤ 0 replaced by 90), locking the camera FOV. Only zoom changes it during play.

**A-CM-4 Eye height and view bob** (`ASAMUPawn.UpdateEyeHeight`, called natively after physics each tick)
[CONFIRMED (src)]. Notation: `k = min(0.9, 10·dt / CustomTimeDilation)`, `ΔZ` = height change since the
native pre-physics snapshot `OldZ`, `S = |V|`, phase = the bob phase.
- **Eye height**: walking: `EyeHeight ← max((EyeHeight − ΔZ)(1 − k) + 38k, −22)` (steps are absorbed and
  relaxed back to 38); any other mode: `EyeHeight ← EyeHeight(1 − k) + 38k`. `LandBob` decays by `(1 − k)`.
  The stock landing dip/recovery path never activates for ASAMU (§5).
- **Ceiling clamp**: because `44 − EyeHeight < 12` whenever EyeHeight > 32, a 12-uu box is swept up by
  `MaxStepHeight + CollisionHeight` = 70 uu from `Location + WalkBob` every tick; `EyeHeight` is limited to
  the hit height above the centre (70 when nothing is hit).
- **Walk bob** (moves the view and therefore the grapple/boost trace origin): walking: phase advances by
  `0.2·dt` when `S < 10`, else by `dt × {0.55 story | 0.85 sprinting | 0.65 otherwise}`; lateral offset =
  pawn-right axis × `0.01·S·sin(8·phase)`; vertical offset = `0.0075·S·sin(16·phase)` when `S > 10`, else 0
  (the stock additive vertical term is never set, so it stays 0). Not walking (and not swimming): phase := 0
  and the offset decays by `(1 − min(1, 8dt))`. Amplitudes at 440 uu/s: ±4.4 uu lateral, ±3.3 uu vertical;
  doubled at 880. The `Bob` value is clamped to ±0.05 (it is 0.01); with `bWeaponBob` off the offset would be
  scaled by 0.1 (it is on).
- **Jump bob** (hand only): rising: `JumpBob ← max(−1.5, JumpBob − 0.03·dt·min(V.z, 300))`; otherwise it
  decays by `(1 − min(1, 8dt))`.
- Footstep sounds are timed from the bob phase (audio only).
→ Rust: a pure function of (state, dt, physics, velocity, location, ceiling probe); it feeds the grapple
trace origin, so it is not cosmetic.

**A-CM-5 Hand bob** (visual): offset `0.15·WalkBob` laterally, `(0.10 + 0.15·0.15)·WalkBob.z + 1.0·(LandBob −
JumpBob)` vertically.

**A-CM-6 Camera animations** (content in `ASAMUCameraAnimations` / `Zeth_CameraStuffs`): GrappleBegin,
GrappleLoop, NormalLand, HardLanding, PowerJumpChargeCameraAnim, PowerJumpBob, PowerLeapBob,
RocketBootsBegin, RocketBootsBoosting, parkour variants, MonsterGrowl (worm shake). Import as data; they bias
the aim rotation.

**A-CM-7 Other feedback**: speed lines (G-FX-2); a falling-wind sound parameter equal to `|V.x + V.y + V.z|`
(sum of components, not the magnitude) sampled every 0.1 s; screen fades (death).

## 11. Death, kill zones, respawn, checkpoints, saving

**A-DT-1 Death sources** [CONFIRMED (src) unless noted]: touching an `ASAMUKillZone` (`Volume`) or an
`ASAMUDynamicKillZone` (`DynamicTriggerVolume`, Kismet-toggleable); the worm NPC; Kismet `SeqAct_PlayerDied`;
the pause menu's restart-from-checkpoint; the `QuickLoad` exec (F7) unless the HUD flag
`bRestartFromCheckpointEnabled` (default true) is off or the level index is 1 (Workshop) or 7 (Epilogue) in
the level enum; the `PlayerDied` exec; and any engine `Died()` (damage, KillZ), which ASAMU redirects to the
same routine. **Falling speed never kills** (§5).

**A-DT-2 Death sequence** (`ASAMUPawn.PlayerDied`) [CONFIRMED (src); times CONFIRMED (cdo)]

| t | Effects |
|---|---|
| 0 | grapple released (G-RL-1); fade to black over 0.3 s; death sound mode and respawn sound; story mode exited — or re-entered when "spawn in story mode" is set; a 0.3 s one-shot timer is (re)started (a second death within the fade restarts it). **The pawn keeps simulating normally during the fade.** |
| 0.3 | fade back in over 0.3 s; sound mode restored 0.6 s later; game-level death hook (falling-when-grappled rocks reset, G-WO-4; time-trial rule A-TT-2); Kismet `SeqEvent_PlayerDied`; player reset: teleport to the latest checkpoint's spawn position (A-CP-4; a failed teleport only logs), pawn and controller rotation := spawn rotation, hand animations reset, boots → `Ready` if enabled, **velocity := 0** |

Not reset: physics mode (a pawn that died falling falls again from the spawn point), grapple budget
(refilled at the next landing), power-jump state, move-input lock, sprint flags, `AirControl`, eye height.

**A-DT-3 KillZ** [CONFIRMED (map decode); reachability TENTATIVE]: `WorldInfo.KillZ` is 1.0 in Workshop and
Epilogue and far below the geometry elsewhere. Below it the engine raises `FellOutOfWorld` with the world's
KillZ damage type (`Engine.KillZDamageType`, the `ZoneInfo` default, never overridden), so the stock path
sets `Health = −1` and calls `Died` → `PlayerDied`; the "no damage type" branch (physics None, hidden) is not
taken [CONFIRMED (cdo, src)]. ASAMU never restores health and the native landing code picks walking physics
only when health > 0, so after a KillZ death the pawn would stay in falling physics on the ground. Whether
any level can reach its KillZ is UNKNOWN — verify before modelling.

**A-CP-1 Registration** [CONFIRMED (src)]: every `ASAMUCheckpoint` adds itself to the game's checkpoint
manager at level start; the list is kept sorted by `checkpointIndex` (equal indices only warn).

**A-CP-2 Activation**: a checkpoint with `bTriggeredFromKismet` has no collision and is activated only by
`SeqAct_TriggerCheckpoint`; otherwise it touches pawns (`COLLIDE_TouchAllButWeapons`) and activates when
touched by the game's player pawn. Activation requires `bEnabled` (default true; Kismet
`SeqAct_ToggleCheckpointEnable`) and not already activated; it activates the linked
`ASAMUCheckpointVisuals` and registers the index.

**A-CP-3 Latest checkpoint** [CONFIRMED (src); corrected]: a registered index becomes the level's latest only
if it is strictly greater than the index of the checkpoint the respawn lookup (A-CP-4) currently returns —
which, when nothing is stored, is the **first (lowest-index) checkpoint**, not "none". On success the game is
saved. Activating a lower or equal index marks it activated but moves nothing and saves nothing.

**A-CP-4 Respawn point lookup**: the stored latest index is used **as a position in the sorted list** (correct
only if indices are 0…N−1 without gaps); with no stored index the first checkpoint is used. The per-level
table is bounds-checked off by one (an index equal to the table length is accepted and reads as 0 — stock
out-of-range array reads return zero [STRONG]), which yields the same first checkpoint. Spawn position =
spawn-point actor location (or the checkpoint's own) + `spawnPointOffset` (default 0); the offset is rotated
by the spawn-point actor's rotation when `bOffsetLocalSpace`, `bRotatePlayerToSpawnPointRot` and a spawn-point
actor are all present, else by the checkpoint's rotation, and not rotated when `bOffsetLocalSpace` is false.
Spawn rotation = spawn-point actor rotation if `bRotatePlayerToSpawnPointRot` (warning and checkpoint
rotation if it is missing), else the checkpoint's rotation. Defaults: both flags true [CONFIRMED (cdo)].

**A-CP-5 Level start** [CONFIRMED (src); corrected]: the pawn spawns at the `PlayerStart` with only its yaw,
walking; the grapple gun is created; then (outside play-in-editor) the save file is loaded. On success:
restore the checkpoint table, grapple capacity, boots enabled and the grapple latch (G-IN-4) and the saved
state of savable world actors (e.g. crystal charge), reset the player to the latest checkpoint of this
level, fire the Kismet "save loaded" event with that index. On failure: fire it with −1 and register
checkpoint 0 — which by A-CP-3 normally changes nothing and **writes no save** (it would only if the level
had no checkpoints or a negative lowest index). The first version said it saves.

**A-CP-6 Saved movement state**: checkpoint table, capacity, boots enabled, grapple latch. Nothing else about
the pawn (no position — respawn always uses checkpoints).
→ Rust: `asamu-world` owns checkpoints (sorted list, index-as-position quirk, strict comparison against the
lookup result); `asamu-game` owns the death sequence (two 0.3 s phases on game time) and save state.

## 12. Time trial (`ASAMUGameInfoTimeTrial`)

**A-TT-1** [CONFIRMED (src)] Movement, abilities and grapple are **identical** (same pawn, controller and gun
classes; the time-trial classes override only HUD, menus and saving).

**A-TT-2** Differences: saving, save loading and save-manager init are disabled, so checkpoint 0 is not
registered at start; respawn uses the latest checkpoint reached in this run or else the first one; **dying
before any checkpoint is registered restarts the stopwatch**. The stopwatch is a HUD timer counting game time
from `SeqAct_StartTimeTrial` (ignored if running) until `SeqAct_EndTimeTrial` (pauses it and reports the
time). The `TimeTrialRestart` exec (F8) issues `restartlevel` only while a trial is active. Tutorial popups
are suppressed.

## 13. Velocity sources (every velocity writer in the `asamu` script package)

The table lists every place where ASAMU script writes the player's velocity [CONFIRMED (src), whole-package
search]. Stock Kismet actions that the maps use near the player (`SeqAct_Interp`, `SeqAct_ToggleCinematicMode`,
`SeqAct_ChangeCollision`, `SeqAct_ToggleHidden`, `SeqAct_SetCameraTarget`, `SeqAct_SetLookAtTarget`,
`SeqAct_PlayCameraAnim`) do not appear to write velocity but were not audited here [TENTATIVE].

| Source | Effect | Confidence |
|---|---|---|
| Jump attempt | `V.z := JumpZ` (+ upward base speed) (A-JP-2) | CONFIRMED (src) |
| Jump-release damping | `V.z ×= 0.7` per 0.1 s (A-JP-3, A-RB-8) | CONFIRMED (src+bc) |
| Power jump / leap | A-PJ-4 | CONFIRMED (src) |
| Rocket boots | A-RB-3/4 (absolute writes) | CONFIRMED (src) |
| Grapple pull / proximity halving | G-PH-2 | CONFIRMED (src+bc) |
| Respawn and `ResetPlayer` exec | `V := 0` | CONFIRMED (src) |
| Worm push | releases the grapple, sets Falling, adds `(unit(player − worm).x·500, unit(player − worm).y·500, −50)` (G-RL-5) | CONFIRMED (src, cdo) |
| `ASAMUTelePad_Attractor` (Workshop, TheCore; started by `SeqAct_ToggleAttractor`) | every 0.05 s forever: `V += (t/10)·unit(pad − pawn)·((500/dist)·500·(1 − b) + 500·b)`, `t` growing by 0.05 up to `attractDuration` 10, `b = velocityBaseAmount` (default 0), `Range`/`Strength` 500/500 by default; the loop never ends, so its "finished" output never fires | CONFIRMED (src, cdo) |
| `DynamicSMActor` floor | receives the landing impulse (§5); no effect on the pawn | CONFIRMED (src) |

Physics-mode writers: grapple attach/release (G-AT-6, G-RL-1), jump attempt, worm push, the stock cheat fly
state, and the native landing/walking transitions. Two pawn helpers that switch to Flying/Falling exist but
have no callers [CONFIRMED (src)].

## 14. Pawn size

[CONFIRMED (src, stock `CheatManager.ChangeSize`, map decode)] The console command `changesize F` sets the
collision cylinder to the class-default radius/half-height × F (21·F, 44·F), the draw scale to F, and
re-places the pawn; `BaseEyeHeight` is **not** scaled. The Epilogue's Kismet runs `changesize 1.4`
(29.4 × 61.6). `SeqAct_SetPawnSize` issues 1.5 (input 0) or 1 (input 1).

## 15. Timing model

Same primitives as G-TM-1…5: per-actor order script tick → latent code → timers → physics → eye height;
cross-actor order input → map actors → controller → pawn → power-jump actor → boots → speed-line cone → gun
(STRONG, G-TM-2); `Sleep(t)` wakes when the remaining time is below half a frame, overshoot discarded, code
continues in the same tick; timers fire when the accumulated time strictly exceeds the rate; state-scoped
variables are zeroed on a state change. Consequences here: jump damping steps (0.1 s), boots steps
(0.1 s / 0.05 s), zoom steps (0.016 s) and crystal fades are frame-quantised — at 60 fps `Sleep(0.1)` lasts
6 frames, `Sleep(0.05)` 3 frames, `Sleep(0.016)` 1 frame; at 30 fps 3, 1–2 (float rounding decides) and 1.
The jump attempt and damping act before the pawn's physics in their frame; power-jump and boots writes act
after it (integrated next frame).

## 16. Rust implementation checklist (asamu-player / asamu-game / asamu-world)

1. Parameters from §1 with provenance; runtime-mutable `ground_speed`, `air_control`, `jump_z`, `air_speed`,
   collision size.
2. Input layer: jump press/release, boost (same physical key, runs immediately), sprint, power-jump, fire,
   quick-load, time-trial restart; move-input lock counter.
3. Controller modes: `Walking` (walking+falling), `Grappling`, `ReleaseGrapple` (one-tick gap), story flags;
   jump consumption rules (A-JP-1).
4. Pawn script-state machine for `Jumped`/`ReleasedJump` damping, independent of the boots; latent scheduler.
5. Landing handler per the §5 table, with floor tag input and the `NotLandable` early-out.
6. Power jump and rocket boots as latent state machines writing absolute velocities, in the §15 order.
7. Eye height/bob port (A-CM-4), required for grapple/boost aim origins.
8. Death sequence, checkpoint list with index-as-position lookup and strict comparison, save subset.
9. Time-trial mode flag affecting only saving, the respawn-timer reset and the stopwatch.

## 17. Open questions

1. Input dispatch placement relative to the actor ticks (assumed before them; G-TM-2).
2. Whether the Workshop/Epilogue slow-walk (`SetSpeed`) can be overridden by sprinting in practice.
3. Is KillZ ever reached (A-DT-3)?
4. Which rocket-boot and grapple-capacity Kismet actions fire at which moment per level.
5. Exact landing velocity seen by `Landed` for slides/step landings (native, NATIVE_PHYSICS.md §4.4).
6. Does the camera-animation offset (e.g. landing animations) measurably change the boost aim direction?
7. Behaviour of the stock cinematic-mode input lock against landing decrements (A-IL-1).

## 18. Test plan (trace verification)

| # | Scenario | Expected (this spec) |
|---|---|---|
| A1 | Walk forward from rest on flat ground | Speed approaches 440; with sprint held 880; release sprint → 440. |
| A2 | Sprint, jump, keep sprint held, land | `GroundSpeed` 440 in the air, 880 again on landing. |
| A3 | Full jump (hold Space) | `V.z` 1000 at takeoff; apex ≈ 481 uu at ≈ 0.96 s (model). |
| A4 | Tap jump (release after 0, 0.1, 0.2, 0.4 s) | `V.z` ×0.7 at release and every 0.1 s while rising; apex ≈ 121 / 198 / 266 / 371 uu. |
| A5 | Tap Space while rising after a grapple release, boots unavailable | Upward velocity damped as in A4; no jump. |
| A6 | First jump after level start vs after a landing | Air acceleration limit 614.4 vs 716.8 uu/s². |
| A7 | Fall from a great height | No damage, no horizontal slowdown; hard-landing effects if `V.z < −2000`. |
| A8 | Land on a `NotLandable` actor after using grapples and boots | No refill of grapples or boots; input lock not released. |
| A9 | Power jump standing still: hold 0.5 s vs 0.7 s, release | 0.5 s: cancel; 0.7 s: `V.z` 1600, apex ≈ 1231 uu. |
| A10 | Power leap at full sprint | Horizontal ≈ 1760, `V.z` 750; no air control until landing; sprint resumes on landing if held. |
| A11 | Power jump release while airborne | Cancel. |
| A12 | Rocket boost after falling 0.5 s, aim level, tap Space | ×1.0/0.8/0.6/0.4/0.2 at 0.1 s steps, 0.6 s without writes, then 41 writes 0.05 s apart: 2500 → 1250 along the aim plus the corkscrew (1000 → 0, one turn per second, starting up). |
| A13 | Same, but hold Space ≈ 1.2 s and release during the boost | Vertical velocity ×0.7 at 0.1 s steps between boost writes until the corkscrew makes `V.z ≤ 0.05` (A-RB-8). |
| A14 | Second boost before landing | Exhausted sound only; after landing a boost works again. |
| A15 | Land during a boost | Boost cancelled immediately; boots `Ready`. |
| A16 | Power leap, then boost before landing | Air control returns when the boost ends (lock released). |
| A17 | Kill volume while moving | Fade 0.3 s (pawn still moving), respawn at the checkpoint with zero velocity and its rotation, fade-in 0.3 s; `PlayerDied` event. |
| A18 | Touch checkpoint 5 then 3, die | Respawn at checkpoint 5. |
| A19 | Time trial: die before the first checkpoint | Stopwatch reset; respawn at the first checkpoint. |
| A20 | Story mode zoom | FOV 90 → 50 in 19 one-frame steps (≤ 62.5 fps), held; back in 18 steps. |
| A21 | Eye height walking up stairs | Eye dips by the step height and recovers with `k = min(0.9, 10·dt)` per tick. |

## 19. Verification pass (2026-10-09)

Method as in GRAPPLE.md §0/§19. Outcome per item:

| Item | Result |
|---|---|
| Walk 440 / sprint ×2 / story ×0.6 / AccelRate 2048 / JumpZ 1000 / AirControl 0.3 → 0.35 / bLimitFallAccel | confirmed (cdo, both decoders; src for the writers) |
| Jump attempt side effects even on failure; success only when walking; walking-slowly unreachable | confirmed (src) |
| Jump-release damping 0.7 / 0.1 s / 0.05 | confirmed (src+bc) |
| Apex predictions (481 / 121 / 198 / 266 / 371 / 461 uu) | reproduced by re-simulation |
| No double jump/dodge/crouch | confirmed (src) |
| No falling damage | confirmed (src); disabled stock horizontal ×0.1 landing slowdown **added** |
| Landing effects, `NotLandable` early-out, story landing differences | confirmed (src); restructured as a decision table; Kismet landed event needs a visible pawn **added** |
| Power jump 0.6 s / 1600 / leap ×2 / 750 / leap condition incl. Z / FallingState | confirmed (src, cdo) |
| Boots charge factors and exactly five charge writes (float 0.5) | confirmed (src+bc, float32 recomputed) |
| Free-fall gap after the last charge write | **corrected** to 0.6 s (0.1 + 0.5) |
| 41 boost writes, speed 2500 → 1250, corkscrew 1000, one turn per second, start "up" | confirmed (src+bc, cdo, float32 recomputed) |
| Boost timer starts at 0 for every boost | **added** (native state-local reset) |
| Jump-release damping overlapping a boost | **added** (A-RB-8) |
| Boost during a leap releases the leap's lock at boost end | **added** |
| Move-input lock writers | confirmed (src; complete writer search) |
| Death sequence 0.3 s / velocity 0 / not-reset list | confirmed (src, cdo) |
| KillZ values; KillZ damage-type branch | confirmed (map decode); damage-type branch **added** |
| Checkpoint sorting, index-as-position, spawn offset/rotation rules | confirmed (src, cdo) |
| "Registering checkpoint 0 at level start saves" | **refuted / corrected** (A-CP-3, A-CP-5) |
| Time-trial differences | confirmed (src) |
| Attractor and worm formulas | confirmed (src, cdo) |
| Eye-height/bob formulas | confirmed (src); vertical stock term always 0 **added**; restructured as formulas |
| Zoom 19/18 steps | confirmed (src+bc) |
| Map console commands (`SetSpeed`, `changesize`, `NormalMode`, no `WorkshopMode`) | confirmed (map decode); `SetSpeed` does not touch AirSpeed **added** |
| Tick order assumptions | **upgraded** to STRONG (GRAPPLE.md G-TM-2) |

## Changelog

- 2026-10-09: first version, from local reading of `ASAMUPawn`, `ASAMUPlayerController`, `ASAMUPlayerInput`,
  `ASAMUPowerJump`, `ASAMURocketBoots`, `ASAMUCheckpoint(Manager)`, `ASAMUGameInfo(TimeTrial)`, kill zones,
  HUD time-trial timer, attractor, worm push, stock `Engine`/`UTGame` script; CDOs decoded locally; per-map
  census of volumes, gameplay actors, KillZ, Kismet console commands and ability actions.
- 2026-10-09 (verification pass, §19): CDO/map values re-decoded with the repository decoder (labels
  STRONG (cdo) → CONFIRMED (cdo)); checkpoint-0 save claim corrected; boots free-fall gap corrected; A-RB-8
  (damping during a boost), leap/boost lock interplay, KillZ damage-type branch, landing slowdown absence and
  the complete velocity-writer list added; tick order upgraded to STRONG; DoJump, landing, power-jump,
  boots, death and eye-height sections restructured as rules, tables, timelines and formulas.
