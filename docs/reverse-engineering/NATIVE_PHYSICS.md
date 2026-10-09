# Native movement physics — behavioural specification

Scope: the stock UE3/UDK native pawn-movement routines in the original Mac executable that move the
player in *A Story About My Uncle*. ASAMU's script package is NonNative (its only native class is the
settings manager), so the player is moved by these routines, parameterised by script defaults, config
values and per-map data. This is the spec that `crates/asamu-player` will reimplement.

Publication rules followed: behaviour is described in our own words as prose, formulas and compact
pseudocode. No decompiled code is reproduced. Numeric constants and field offsets are facts read from
the binary and cite `Class::Function @ address` (and the data address the value is loaded from).
Every claim carries a confidence label (see `CLAUDE.md`):
**[CONFIRMED]** read directly in code/bytes · **[STRONG]** several independent indicators ·
**[TENTATIVE]** plausible, weak evidence · **[UNKNOWN]**.
Property names such as `GroundSpeed` are hypotheses unless stated otherwise; section 7 lists the
evidence for each so a later agent can confirm them against the script property layout of `Engine.u` /
`UDKBase.u`.

## Contents

0. [Evidence and reproduction](#0-evidence-and-reproduction)
1. [Per-tick call flow, physics modes, sub-stepping](#1-per-tick-call-flow)
2. [Velocity update: CalcVelocity and braking](#2-velocity-update-calcvelocity-and-braking)
3. [Walking](#3-walking-physwalking)
4. [Falling](#4-falling-physfalling)
5. [Jumping and landing](#5-jumping-and-landing)
6. [Rotation, eye height, camera](#6-rotation-eye-height-camera)
7. [Field-offset table](#7-field-offset-table)
8. [Constants table](#8-constants-table)
9. [Implementation notes for asamu-player](#9-implementation-notes-for-asamu-player)
10. [Other modes and non-pawn routines](#10-other-modes-and-non-pawn-routines)
11. [Verification log](#11-verification-log)

### Most important findings (read this first)

| # | Finding | Confidence |
|---|---|---|
| 1 | The player's pawn runs the **AUDKPawn** overrides (`performPhysics`, `CalcVelocity`, `physFalling`, `GetGravityZ`, `CalculateSlopeSlide`, `physicsRotation`) on top of stock `APawn` walking/falling. | CONFIRMED that these are the AUDKPawn vtable entries; STRONG that ASAMU's pawn derives from UDKPawn (config has a `[UTGame.UTPawn]` section; UTGame is NonNative) |
| 2 | **Ground acceleration magnitude is always `AccelRate`**: walking normalises the script's `Acceleration` to a direction and the UDK velocity update multiplies it by `AccelRate`. Analog input magnitude is discarded natively. | CONFIRMED |
| 3 | Walking velocity is updated **once per physWalking call with the whole delta time**; only the *movement* is sub-stepped (≤ 0.05 s pieces, ≤ 8 iterations per tick shared across modes). | CONFIRMED |
| 4 | Braking (no input) integrates in 0.03 s pieces with factor `2·GroundFriction`, then **replaces the velocity by the time-weighted average** of the pieces; speeds below 10 uu/s snap to zero. | CONFIRMED |
| 5 | Walking up slopes is implemented as **step-up → move → step-down**; the pawn hovers 1.9–2.4 uu above the floor (target 2.15). Final walking velocity = actual displacement / dt with Z = 0. | CONFIRMED |
| 6 | Falling uses semi-implicit Euler per sub-step **followed by a velocity "refinement" `V = 2·avg − V_old`**, which makes the *effective* vertical acceleration **2 × GravityZ** (and doubles air acceleration). With `DefaultGravityZ = −520` this is −1040 uu/s². | CONFIRMED (code + disassembly); TENTATIVE external corroboration from stock UDK jump-height defaults (see 4.6) |
| 7 | Gravity = `WorldInfo` gravity (lazy: GlobalGravityZ if non-zero, else DefaultGravityZ = **−520.0** from `ASAMU/Config/DefaultGame.ini`), overridden inside GravityVolumes, then × `CustomGravityScaling` (UDKPawn+0x5A4). | CONFIRMED (code + config) |
| 8 | Air control: if a pawn flag (Pawn+0x298 bit 51, hypothesis `bLimitFallAccel`) is set, air acceleration is clamped to `AccelRate·AirControl` (+ a low-speed boost below 10 uu/s, and a horizontal speed bound when already above GroundSpeed). If the flag is clear, the script acceleration is used unclamped. **The flag's default must be read from script.** | CONFIRMED logic; UNKNOWN default |
| 9 | Landing happens on any hit whose normal Z ≥ `WalkableFloorZ` (Pawn+0x258). On a direct landing the unused sub-step time is carried into walking; a landing detected only after a wall/ceiling slide carries **zero** time. | CONFIRMED (disassembly of both call sites) |
| 10 | Jumping is **not native** (no `DoJump` symbol); the jump impulse comes from script. Native code only provides `SuggestJumpVelocity` (AI) and landing bookkeeping. `SetMaxLandingVelocity` is an empty stub. | CONFIRMED |

## 0. Evidence and reproduction

- Binary: `A Story About My Uncle.app/Contents/MacOS/ASAMU` — Mach-O 64-bit **x86_64**, unstripped.
- Ghidra project (local, git-ignored): `research/ghidra`, project `ASAMU`. Function lists (names only):
  `tools/ghidra-scripts/anchors/player-physics.txt` (48 functions) and
  `tools/ghidra-scripts/anchors/player-physics-2.txt` (63 functions). Regenerate local output with
  `DecompileToLocal.java` / `ExportFunctionSummaries.java` as in `tools/ghidra-scripts/README.md`
  (`-readOnly`, output under `research/decompiled/physics{,2}` and `research/ghidra/out/`).
- **Virtual calls** were resolved by reading vtables from the symbol table (`nm -n`; vtables
  `__ZTV8AUDKPawn`, `__ZTV5APawn`, `__ZTV6AActor`, `__ZTV20AUDKPlayerController`, `__ZTV14APhysicsVolume`)
  and mapping slot offsets to symbol names (e.g. AUDKPawn slot `+0x9D8` → `AUDKPawn::CalcVelocity`,
  `+0x458` → `APawn::physWalking`, `+0x450` → `AUDKPawn::physFalling`, `+0x2B0` → `AUDKPawn::GetGravityZ`).
- **Float arguments** dropped by the decompiler at call sites (SysV x86-64 passes floats in XMM
  registers) were checked in the disassembly (`objdump -d --x86-asm-syntax=intel --start-address=…`).
- **Constants** were read from the file bytes at the addresses the code loads them from.
- **Config** values come from `Contents/Resources/{ASAMU,Engine}/Config/*.ini` (read-only).
- Units: Unreal units (uu), seconds; rotators are 16-bit-wrapped integer angles (65536 = 360°).

## 1. Per-tick call flow

### 1.1 Actor tick → physics [CONFIRMED unless noted]

For an actor with authority (single player: the local pawn), one engine tick does
(`AActor::Tick @ 0x10090A210`, `AActor::TickAuthoritative @ 0x100908E50`, `APawn::Tick @ 0x100A78E70`):

1. `APawn::Tick`: if the pawn is based on another actor and a pawn flag (Pawn+0x298 bit 59) asks for
   it, the base is ticked first.
2. Script event `Tick(dt)` (only if the current state accepts it), then latent state code
   (`AActor::ProcessState`), then timers, then the LifeSpan countdown (Actor+0x120; destroyed at ≤ 1e-4).
3. `performPhysics(dt)` if the actor is not being deleted, `Physics ≠ None` and its role is not
   autonomous-proxy (Actor+0xC2 ≠ 2). For a network-relevant client copy a different path is taken;
   irrelevant for single player.
4. `TickSpecial(dt)` — for the pawn this is `AUDKPawn::TickSpecial @ 0x100F6C240`, i.e. **after**
   physics. It calls the script event `UpdateEyeHeight(dt)` (when a UDKPawn flag at UDKPawn+0x590
   bit 15 is set) and does visual work (blob shadow trace, feign-death/ragdoll bookkeeping).
5. Outside-world-bounds check.

The local player controller ticks separately (`APlayerController::Tick @ 0x10090A760`): input
interactions tick, then script `PlayerTick(dt)` (which is where `PlayerMove` writes the pawn's
`Acceleration` and rotation — script, not verified here [TENTATIVE]), then its own state/timers.
**Whether the controller ticks before the pawn in the same frame is not established** [UNKNOWN];
UE3 ticks actors in list order and the controller is normally spawned before its pawn, which would put
input before physics [TENTATIVE].

### 1.2 performPhysics [CONFIRMED]

`AUDKPawn::performPhysics @ 0x100F6BBF0` stores the current `Location.Z` into UDKPawn+0x61C
(hypothesis `OldZ`, used by script eye-height smoothing), then runs `APawn::performPhysics @ 0x100AD8990`:

1. Root-motion early-out: if the mesh (Pawn+0x470) is in root-motion mode 3 and a flag comparison
   between Pawn+0x298 bit 54 and the mesh says so, physics is skipped this tick. Not expected for the
   first-person player [TENTATIVE].
2. `CheckStillInWorld`; abort if being deleted (Actor+0xE8 bit 3). If no physics volume is assigned
   (Actor+0x188 null), assign one (`SetZone`).
3. Remember `OldVelocity = Velocity` (for rotation).
4. Crouch housekeeping: when walking and both "wants to crouch" (bit 3) and "can crouch" (bit 6) are
   set — crouch if not crouched; if crouched and "try to uncrouch" (bit 5) is set, decrement
   `UncrouchTime` (Pawn+0x2A8) and clear bits 3 and 5 when it reaches ≤ 0. When not walking and not
   falling but crouched, uncrouch.
5. **`startNewPhysics(dt, Iterations = 0)`**.
6. `PostProcessPhysics(dt, OldVelocity)` — empty in `APawn` (`@ 0x100AD8E10`) and not overridden.
7. Set "simulate gravity" (Pawn+0x298 bit 20) = (Physics is Walking or Falling).
8. If crouched and (no longer wants to crouch, or Physics not Walking/Falling) → uncrouch.
9. If a controller exists: `Controller.MoveTimer` (Controller+0x29C) −= dt. Without a controller,
   rotation is skipped unless "run physics with no controller" (Pawn+0x298 bit 49) is set.
10. Unless Physics is Interpolating or RigidBody: `physicsRotation(dt, OldVelocity)` (section 6).
11. Smoothed tick time (Pawn+0x2E8, hypothesis `AvgPhysicsTime`) = 0.2·dt + 0.8·old.
12. Pending-touch notification (Actor+0x208 chain, script `PostTouch`).

### 1.3 Mode dispatch: startNewPhysics [CONFIRMED]

`APawn::startNewPhysics(remaining, Iterations) @ 0x100AD8C30` returns immediately if
`remaining < 0.0003` (data 0x101663900) or `Iterations > 7`; otherwise it dispatches on `Physics`
(Actor+0xC0). The value → handler mapping below is confirmed by resolving each dispatch slot in the AUDKPawn
vtable (re-checked against the vtable bytes during verification). Mode *names* for values with a handler
follow the handler symbol [STRONG]; the names for 5, 6 and 13 come from the stock UE3 enum, not from this
binary [TENTATIVE]:

| Value | Mode | Handler for the player's pawn |
|---|---|---|
| 0 | None | nothing |
| 1 | Walking | `APawn::physWalking @ 0x100ADA470` |
| 2 | Falling | `AUDKPawn::physFalling @ 0x100F6DB80` → `APawn::physFalling @ 0x100ADF900` |
| 3 | Swimming | `APawn::physSwimming @ 0x100AE21A0` |
| 4 | Flying | `APawn::physFlying @ 0x100AE1710` |
| 5 | Rotating | not dispatched (logged as unsupported, falls back to None) |
| 6 | Projectile | not dispatched for pawns (logged as unsupported → None; actor-level only) |
| 7 | Interpolating | `AActor::physInterpolating @ 0x100AE5A70` |
| 8 | Spider | `APawn::physSpider @ 0x100AE4790` |
| 9 | Ladder | `APawn::physLadder @ 0x100AE3120` |
| 10 | RigidBody | `AActor::physRigidBody @ 0x100A82260` |
| 11 | SoftBody | `AActor::physSoftBody @ 0x100A822A0` |
| 12 | NavMeshWalking | `APawn::physNavMeshWalking @ 0x10096F900` |
| 13 | (unused) | logged as unsupported → None |
| 14 | Custom | `AActor::physCustom @ 0x1001A48E0` (empty function) |

### 1.4 Sub-stepping, iteration budget and time carry-over [CONFIRMED]

- **Iteration budget**: `Iterations` starts at 0 each tick and is passed through every mode change.
  Walking and falling each increment it once per sub-step and stop when it exceeds 7, so a tick has at
  most **8 movement sub-steps in total across modes**. Time still remaining after the 8th is dropped.
- **Sub-step size** (walking and falling identical, constants 0.05 @ 0x101636058, 0.5 @ 0x101636054):
  ```
  step = remaining                      if remaining <= 0.05
       = min(0.05, 0.5 * remaining)     otherwise
  ```
- **Walking → falling, normal path** (`APawn::StartFalling @ 0x100ADC160`, used by the fall/ledge
  decision of 3.4 — i.e. walking off a ledge): the time of the current sub-step that was not actually
  travelled is given back. `remaining` here is the value *after* this sub-step was subtracted; the
  caller passes it and `step` in XMM0/XMM1 [CONFIRMED, disassembly of the call]:
  ```
  frac      = min(1, |Location.xy − StepStart.xy| / |Delta|)   (Delta = this sub-step's intended move)
  remaining = remaining + step * (1 − frac)                    (remaining = 0 if |Delta| = 0)
  Velocity.Z = 0
  ```
  then script `Falling()` (if the state accepts it), `setPhysics(Falling)` if still walking, and
  `startNewPhysics(remaining, Iterations)`.
- **Walking → falling, inline path** (inside `APawn::physWalking`, taken when `processHitWall` or
  `stepUp` has already switched the pawn to Falling): same `remaining + step·(1 − frac)` carry (computed
  before, and independent of, the state check [CONFIRMED, disassembly]), script `Falling()`, then
  `startNewPhysics`. It differs from `StartFalling` in three ways [CONFIRMED]: it does **not** zero
  `Velocity.Z`, it does not call `setPhysics` (the pawn is already falling), and if script `Falling()`
  switched the pawn to Flying it sets `Velocity = (0,0,AirSpeed)` and `Acceleration = (0,0,AccelRate)`.
  (In practice walking velocity has Z = 0 already, so the first difference only matters if script
  changed the velocity during the hit notification.) *Correction (verification pass): an earlier
  revision called this path "identical" to StartFalling.*
- **Falling → walking** (direct landing): `remaining = remaining_after_step + step * (1 − Hit.Time)`
  is passed to `processLanded`, which ends with `startNewPhysics(remaining, Iterations)`. When the
  landing is detected only after a slide inside the sub-step, the carried time is **0**.
- `startNewPhysics` refuses remainders below 0.0003 s.

## 2. Velocity update: CalcVelocity and braking

Signature (both versions): `CalcVelocity(AccelDir, dt, MaxSpeed, Friction, bFluid, bBrake, bBuoyant)`.
Walking calls it with `MaxSpeed = GroundSpeed` (Pawn+0x33C), `Friction = PhysicsVolume.GroundFriction`
(Volume+0x294), `bFluid = 0`, `bBrake = 1`, `bBuoyant = 0` [CONFIRMED, disassembly of the call in
`APawn::physWalking`]. Falling does **not** call CalcVelocity [CONFIRMED].

Helpers [CONFIRMED]:

- `APawn::MaxSpeedModifier @ 0x100ADCF30`:
  `m = (human-controlled ? 1 : DesiredSpeed[+0x2D0]); if crouched (bit 4) m *= CrouchedPct[+0x368]
  else if walking-slowly (bit 2) m *= WalkingPct[+0x360]; return m * MovementSpeedModifier[+0x364]`.
- `APawn::GetMaxAccel(m) @ 0x100ADCF90` = `m * AccelRate[+0x34C]`.
- `APawn::GetMaxSpeed @ 0x100A6C7A0` = WaterSpeed[+0x340] when swimming, AirSpeed[+0x344] when flying,
  else GroundSpeed[+0x33C] (used by AI/script, not by walking itself).
- "SafeNormal" everywhere: unit-length input returned unchanged; length² < 1e-8 (0x10163FED4) → zero.
  "Nearly zero" tests use |component| < 1e-4 (0x10163FED8).

### 2.1 AUDKPawn::CalcVelocity @ 0x100F6BCA0 — the path the player uses [CONFIRMED]

Taken when (Pawn+0x298 bit 54 set) or (bit 53 clear and (no mesh or mesh root-motion mode = 2)), and
the controller is not in "precise destination" mode (Controller+0x272 bit 0). For a first-person
player without root motion this is the path [STRONG]. Otherwise it defers to `APawn::CalcVelocity`.

```
Acceleration = AccelDir * AccelRate            // overwrites the script value; AccelDir is unit or zero
if (!bBrake || Acceleration != 0):             // exact-zero test
    s = |Velocity|
    Velocity -= (Velocity - AccelDir * s) * Friction * dt     // "turning friction"
else:
    Velocity = Brake(Velocity, dt, Friction)    // 2.3, inlined copy of ApplyVelocityBraking
Velocity = Velocity * (1 - bFluid * Friction * dt) + Acceleration * dt
if bBuoyant: Velocity.Z += GetGravityZ() * dt * (1 - Buoyancy[+0x2F0])
MaxSpeedEff = MaxSpeed * MaxSpeedModifier()
if |Velocity|^2 > MaxSpeedEff^2: Velocity = SafeNormal(Velocity) * MaxSpeedEff   // 3D clamp
```

Notes: the acceleration is **not** scaled by `MaxSpeedModifier` in this path (only the speed cap is);
the "turning friction" term pulls the velocity direction toward the input direction at rate
`Friction·dt` without changing the speed scale beyond that; `Friction·dt > 1` overshoots (no guard).
The UDK path does not store the root-motion velocity copy (Pawn+0x3B0) [CONFIRMED].

### 2.2 APawn::CalcVelocity @ 0x100ADC440 — stock version (fallback) [CONFIRMED]

Differences from 2.1, for completeness:

- If Pawn+0x298 bit 53 (hypothesis `bForceRMVelocity`) is set, velocity is copied from Pawn+0x3B0
  (hypothesis `RMVelocity`) and nothing else happens.
- Root-motion modes 1 and 3 of the mesh (mesh+0x6E4, previous-mode byte +0x6E5) feed the mesh's
  root-motion velocity (mesh+0x6B0) into velocity/acceleration.
- `MaxAccel = AccelRate * MaxSpeedModifier()`; the script `Acceleration` magnitude is kept and clamped
  to `MaxAccel` (instead of being replaced by `AccelDir·AccelRate`).
- With Pawn+0x298 bit 50 (hypothesis `bForceMaxAccel`) the acceleration is forced to full `MaxAccel`
  along the input (or current velocity, or facing direction when both are zero).
- With a controller in precise-destination mode, velocity is steered straight at the destination.
- Ends by storing the velocity into Pawn+0x3B0.

### 2.3 Braking: APawn::ApplyVelocityBraking @ 0x100ADC2C0 [CONFIRMED]

Used when `bBrake` and the acceleration is exactly zero (same algorithm inlined in 2.1):

```
V0 = Velocity; avg = 0; t = dt
while t > 0:
    h = min(0.03, t); t -= h
    Velocity = Velocity - 2 * Velocity * h * Friction          // = Velocity * (1 - 2 h F)
    if dot(Velocity, V0) > 0: avg += Velocity * (h / dt)       // uses the post-update velocity
Velocity = avg
if dot(Velocity, V0) < 0 or |Velocity|^2 < 100: Velocity = 0  // 100 → 10 uu/s
```

The result is the **time-weighted average** of the decaying velocity over the sub-intervals, not the
end value. Constants: 0.03 @ 0x101731DB0, 100.0 @ 0x10163713C.

## 3. Walking: physWalking

`APawn::physWalking(dt, Iterations) @ 0x100ADA470` (not overridden by UDK). All steps CONFIRMED unless
labelled. "Floor" is the stored floor normal (Pawn+0x37C). Traces use the collision cylinder extent
(radius, radius, half-height) unless noted.

### 3.1 Algorithm

```
if no Controller and not RunPhysicsWithNoController (bit 49): Velocity = Acceleration = 0; return

Velocity.Z = 0; Acceleration.Z = 0
AccelDir = (Acceleration.xy == 0) ? 0 : SafeNormal(Acceleration)
CalcVelocity(AccelDir, dt, GroundSpeed, Volume.GroundFriction, 0, 1, 0)        // once, whole dt
DesiredMove = (Velocity.x, Velocity.y, 0)
if Volume flag (Volume+0x290 bit 0, hyp. bVelocityAffectsWalking):
    DesiredMove.xy += Volume.ZoneVelocity(+0x284).xy * 25 * dt

OldLocation = Location; OldFloor = Floor; OldBase = Base (+ its location)
clear bJustTeleported (Actor+0xF0 bit 12); remember then clear Pawn+0x298 bit 19
remaining = dt
while remaining > 0 and Iterations <= 7 and (Controller or bit 49):
    step = substep(remaining); Iterations += 1                                 // 1.4
    Delta = step * DesiredMove; StepStart = Location
    if Delta nearly zero:  remaining = 0; zeroMove = true; wallActor = none
    else:
        remaining -= step
        if Controller.WantsLedgeCheck():                                        // 3.6
            Delta = CheckForLedges(AccelDir, Delta, down, &checkedFall, &mustJump)
            if Controller.MoveTimer == -1 or Delta == 0: remaining = 0
        if Floor.Z >= 0.98 or dot(Floor, Delta) >= 0:  MoveActor(Delta) → Hit
        else:  Hit.Time = 0; Hit.Normal = Floor    // moving uphill on a slope: go via step-up;
                                                   // Hit.Actor keeps its previous value (none on the
                                                   // first sub-step, else the last floor-trace actor)
        if Hit.Time < 1:  handle blocking hit (3.2); may switch to falling and return
    // ---- floor check and snapping (3.3) ----
    // ---- fall / ledge decision (3.4) ----
    // ---- slope gravity slide (3.5) ----
    if Physics became Swimming: startSwimming(...); return
after loop:
    if still Walking:
        if not (old bit 19) and not bJustTeleported: Velocity = (Location - OldLocation) / dt
        Velocity.Z = 0
    Controller.PostPhysWalking(dt)                                              // empty for players
```

### 3.2 Blocking hit while walking

- *Physics objects*: a hit static mesh on world geometry that can become dynamic is converted
  (`AKActorFromStatic::MakeDynamic`) and receives an impulse scaled by GroundSpeed. Not expected to
  matter for parity [TENTATIVE].
- If the hit actor exists, is not the base and is **not** step-up-able (Actor+0xE8 bit 17 clear,
  hypothesis `bCanStepUpOn`): `processHitWall`; if now falling → falling transition (1.4). Otherwise
  slide along the wall: horizontal wall normal `N = SafeNormal(Hit.Normal.x, Hit.Normal.y, 0)`,
  `Slide = (Delta − N·dot(Delta,N))·(1 − Hit.Time)`; if `dot(Slide, Delta) ≥ 0` and not nearly zero,
  move by it; on a second hit: `processHitWall` (if this one switches to falling, physWalking returns
  at once **without** carrying the remaining time), `TwoWallAdjust` (4.8) and move again.
- Otherwise (no actor, the base, or step-up-able): `stepUp(down=(0,0,−1), DesiredDir=SafeNormal(Delta),
  Delta·(1 − Hit.Time), Hit)` (3.2.1). If that leaves the pawn falling → falling transition.

#### 3.2.1 APawn::stepUp @ 0x100ADCFA0 [CONFIRMED]

`StepDown = down · (MaxStepHeight[+0x250] + 2.0)`.

1. If `dot(down, Hit.Normal) > −0.08` (wall nearly vertical or overhanging: normal Z < 0.08 when
   `down = (0,0,−1)`) **or** `Hit.Normal.Z ≥ WalkableFloorZ`: move up by `−StepDown`, then move by
   `Delta`; plan to step down at the end.
2. Else (a non-walkable slope) and not walking: move along the slope
   `(Delta.x, Delta.y, Delta.z + |Delta|·Hit.Normal.Z)` and do **not** step down at the end.
   When walking nothing is moved here, the step-down stays planned, and the original hit is handled by
   steps 4–5 below.
3. If the last move had no hit → step down by `StepDown` (if planned) and finish.
4. On a hit: (physics-object conversion as in 3.2). If the hit is still a near-vertical wall
   (`dot(down,N) > −0.08`) **and** `|Delta|² · Hit.Time > 144.0` (literally squared length times time)
   **and** the hit actor is absent or step-up-able: step down first (if planned) and **recurse** with
   `Delta·(1 − Hit.Time)` (climbs successive stairs within one move).
5. Otherwise `processHitWall` (return if falling), slide along the horizontalised normal as in 3.2
   (`Slide = (Delta − N·dot(Delta,N))·(1 − Hit.Time)`, moved only if `dot(Slide, Delta) ≥ 0`; unlike 3.2
   there is **no** "nearly zero" skip here), with one `processHitWall` + `TwoWallAdjust` retry on a
   second hit, then step down by `StepDown` if planned.

Consequence: on walkable slopes the pawn keeps its full horizontal speed (step up, move horizontally,
step down) [CONFIRMED by construction]. Constants: 2.0 @ 0x101653788, −0.08 @ 0x10173D2C8,
144.0 @ 0x10173D2CC.

### 3.3 Floor check and height adjustment

The floor trace is **skipped** only when nothing was hit this step, the move was nearly zero, the base
exists and is world geometry (Actor+0xE8 bit 7, hypothesis `bWorldGeometry`), the pawn's stored
relative location (Actor+0x1D8) still equals `Location − Base.Location` exactly, and the force-floor-
check flag (Pawn+0x298 bit 27) is clear. A base whose Actor+0xE8 bit 59 is clear (and which is not the
WorldInfo) sets the force flag on every step, so the floor is re-traced each step on such bases. When skipped, the old floor is reused with `Hit.Time = 0.1`,
`FloorDist = 2.4`, `Hit.Actor = Base`.

Otherwise (clearing the force flag):
```
start = collision-component centre (component+0x8C; Location if no component)    // TENTATIVE meaning
SingleLineCheck(start → start − (0,0, MaxStepHeight + 2.0), extent = cylinder, flags 0x20DF
                [0x1020DF if Actor+0xF0 bit 1])
FloorDist = (MaxStepHeight + 2.0) * Hit.Time
Floor = Hit.Normal
```
Then, if `Hit.Normal.Z ≥ WalkableFloorZ`, or the floor is too steep but the move does not push into it
(`Delta` nearly zero or `dot(Delta, Hit.Normal) ≥ 0`):

- If no floor in reach (`Hit.Time ≥ 1`), or the trace started penetrating (flag at hit+0x64), or
  (`FloorDist ≤ 2.4` and the hit actor is the current base): only if `FloorDist < 1.9` (and not
  penetrating) move **up** by `(0,0, 2.15 − FloorDist)` and treat the floor as touching (`Hit.Time = 0`).
- Else (floor found farther than 2.4, or a new base): `ShouldCatchAir` is consulted — it always returns
  false for this pawn (`APawn::ShouldCatchAir @ 0x100ADC150` returns 0; UDK does not override) —
  then move by `(0,0, 2.15 − FloorDist)` (i.e. **snap down** to 2.15 above the floor; restore the
  previous hit if that move hit nothing). If the hit actor differs from the base and blocks the pawn,
  it becomes the new base (`SetBase(actor, normal)`).

If instead the floor is too steep **and** the move pushes into it: slide down the slope by
`0.1·N + (D − N·dot(D,N))` with `D = (0,0,−MaxStepHeight)` (a MaxStepHeight-long drop projected onto
the slope plus a 0.1 push-off), set base/floor from the resulting hit, and remember whether a wall was
also hit while the floor is unwalkable (this blocks the falling path below and reverts the step instead
[TENTATIVE interpretation]).

Constants: 2.0 (trace fudge), 2.4 @ 0x10173D2B0, 1.9 @ 0x10173D2B4, 2.15 @ 0x10173D2B8,
0.1 @ 0x1016457B8.

### 3.4 Fall / ledge decision

Entered when `mustJump` is set, or no floor was found (`Hit.Time ≥ 1`), or the floor is unwalkable:

1. If the fall has not been checked yet this call, `mustJump` is not already set, and a controller
   exists (and its state accepts the event): set Pawn+0x298 bit 10 (a "may fall" permission flag;
   declaration-order hypothesis `bCanJump`), mark checked, and call the controller's script event
   `MayFall(bFloor = (Hit.Time < 1), FloorNormal)`.
2. If, after that, `mustJump` is still clear, bit 10 has been cleared by script, and the old base is
   absent **or** (its Actor+0xE8 bit 59 is clear and it is not the WorldInfo), `mustJump` becomes true.
   *(Correction, verification pass: an earlier revision said "static"; the code tests bit 59 clear, the
   same bit whose clear state forces a floor re-trace in 3.3. Its meaning is UNKNOWN.)*
3. Unless a steep-slope wall block was recorded (3.3): **start falling** (1.4) when any of:
   bJustTeleported is set; `mustJump`; bit 10 set and "can walk off ledges" (bit 22) set; bit 10 set,
   not walking-slowly (bit 2 clear) and (move was zero or not crouched (bit 4 clear)); bit 10 set,
   walking-slowly and the move was zero.
4. Otherwise **revert the step**: `Velocity = Acceleration = 0`, teleport back to `OldLocation`
   (`FarMoveActor`), clear bJustTeleported, re-attach to the old base with the old floor normal
   (`SetBase`), and call `Controller.FailMove()` (`AController::FailMove`, controller vtable +0x838);
   return. The re-attach is skipped when the old base is absent, or when it is an actor with
   Actor+0xE8 bits 0 and 7 clear and bit 49 set **and** either (its bit 59 is clear), or (its Physics is
   neither Interpolating nor RigidBody and its Actor+0xF0 bit 4 is clear), or (it moved since the start
   of the call). [CONFIRMED logic; meaning of the bits UNKNOWN/TENTATIVE.]

For the ASAMU player running normally (not crouched, not walking-slowly) leaving a floor always starts
falling [STRONG; depends on script not clearing bit 10 in `MayFall`].

### 3.5 Slope gravity slide (slippery floors)

On a walkable floor with `Floor.Z < 0.99` and `Floor.Z · GroundFriction < 3.3`:
```
g' = GetGravityZ() * dt / (2 * max(GroundFriction, 0.5)) * dt        // dt = the WHOLE call's dt
Slide = (0,0,g') − Floor * dot(Floor, (0,0,g'))                        // gravity projected on floor
if dot(Slide, (0,0,g')) >= 0: MoveActor(Slide)
```
Note it uses the full delta time of this physWalking call, applied on every sub-step [CONFIRMED,
disassembly]. Constants: 0.99 @ 0x1016A7A60, 3.3 @ 0x10173D2BC, 0.5 @ 0x101636054.

### 3.6 Ledge checks (crouched / walking-slowly only)

`APlayerController::WantsLedgeCheck @ 0x1007A68A0` returns true only when the pawn is crouched
(bit 4) or walking-slowly (bit 2) [CONFIRMED]; `APlayerController::StopAtLedge` returns false.
`APawn::CheckForLedges @ 0x100AD9550` then probes, with a small (0.5, 0.5, 0.5) extent and with the
full cylinder, whether there is walkable floor within
`min(MaxStepHeight, (Radius + |Delta|)·sqrt(1 − W²)/W) + LedgeCheckThreshold[+0x25C] + CollisionHeight`
below the destination (W = WalkableFloorZ) — the follow-up acceptance test repeats this bound with the
actual probe-hit normal Z in place of W — tries the two perpendicular directions, may set the
"partially over ledge" flag (bit 19) and `PartialLedgeMoveDir` (+0x260), and may raise `MayFall`.
Its returned (adjusted) delta was not recovered from the decompiler output; **the exact output vector
is UNKNOWN** and must be re-read from disassembly before implementing crouch/slow-walk ledge behaviour.
When not on a base it first checks for a floor `LedgeCheckThreshold` below and sets `mustJump` if none.

### 3.7 Crouch (affects collision size) [CONFIRMED]

`APawn::Crouch @ 0x100A6F140`: set the cylinder to (CrouchRadius[+0x2B0], CrouchHeight[+0x2AC]); if the
crouched cylinder is larger in any dimension, test encroachment at `Location − (0,0,HeightAdjust)` and
undo if blocked; set crouched (bit 4) and force-floor-check (bit 27); script `StartCrouch(HeightAdjust)`
with `HeightAdjust = oldHeight − CrouchHeight`. **Location is not moved**; the next floor check snaps
the smaller cylinder down. `APawn::UnCrouch @ 0x100A6F470` restores the class-default cylinder, moves
**up** by the height difference (`FarMoveActor`), reverting if blocked, clears bit 4, sets bit 27 and
calls script `EndCrouch`.

## 4. Falling: physFalling

`AUDKPawn::physFalling @ 0x100F6DB80` = `APawn::physFalling @ 0x100ADF900` followed by the UDK
stuck-falling check (4.9). All CONFIRMED unless labelled.

### 4.1 Gravity source chain [CONFIRMED]

```
PawnGravityZ  = AUDKPawn::GetGravityZ @ 0x100F6DA20
             = ActorGravityZ * CustomGravityScaling[UDKPawn+0x5A4]
               (RigidBody + water volume: additionally * (1 − Buoyancy[+0x2F0]))
ActorGravityZ = AActor::GetGravityZ @ 0x100AD7C30
             = PhysicsVolume ? PhysicsVolume.GetGravityZ() : World gravity
               (RigidBody: WorldInfo.RBPhysicsGravityScaling[+0x608] * volume RB gravity)
APhysicsVolume::GetGravityZ @ 0x100AD7BF0 = World gravity          (plain volumes add nothing)
AGravityVolume::GetGravityZ @ 0x1006D6370 = its own GravityZ [Volume+0x2D8]
World gravity = AWorldInfo::GetGravityZ @ 0x100AD7B10 (via UWorld::GetGravityZ @ 0x100CF46A0):
    if WorldGravityZ[+0x5FC] == 0:
        WorldGravityZ = (GlobalGravityZ[+0x604] != 0) ? GlobalGravityZ : DefaultGravityZ[+0x600]
    return WorldGravityZ
```
Config [CONFIRMED]: `ASAMU/Config/DefaultGame.ini` `[Engine.WorldInfo] DefaultGravityZ=-520.0`,
`RBPhysicsGravityScaling=2.0` (overriding Engine `BaseGame.ini` −750.0 / 1.0). A map can override via
its WorldInfo properties or GravityVolumes [STRONG]; per-map values are still to be read [UNKNOWN].

### 4.2 Air control and acceleration limit

Before the sub-step loop (once per call):
```
Controller.PreAirSteering(dt)                                          // empty for players
OldAcceleration = Acceleration; Acceleration.Z = 0
TickAirControl = AirControl[+0x35C]
if TickAirControl > 0.05 (and no root motion):
    TestWalk = (Velocity.xy + AccelRate * TickAirControl * SafeNormal(Acceleration.xy)) * dt, z = 0
    if TestWalk != 0 and SingleLineCheck(centre → centre + TestWalk, extent = cylinder,
                                         flags 0x2286 = world geometry, stop at first hit) hits:
        TickAirControl = 0                                             // no air control into walls
BoundSpeed = 0
if Pawn+0x298 bit 51 (hyp. bLimitFallAccel) and no root motion:
    maxAccel = AccelRate * TickAirControl
    s2 = |Velocity.xy|
    if TickAirControl > 0 and s2 < 10:   maxAccel += (10 - s2) / dt     // help from standstill
    elif s2 >= GroundSpeed:
        if TickAirControl > 0.05: BoundSpeed = s2                       // may not exceed current speed
        else:                     maxAccel = 1.0
    if |Acceleration| > maxAccel: Acceleration = SafeNormal(Acceleration) * maxAccel
Controller.PostAirSteering(dt)                                         // empty for players
```
`Acceleration` is restored to `OldAcceleration` when physFalling returns normally (the clamp is
per-tick only). The default of bit 51 decides whether air control limits anything at all
[UNKNOWN — must be read from Pawn/UDKPawn/UTPawn/ASAMU defaults]. Constants: 0.05 @ 0x101636058,
10.0 @ 0x101682598.

### 4.3 Sub-step: velocity, move

```
while remaining > 0 and Iterations < 8:
    step = substep(remaining); Iterations += 1                              // 1.4
    OldLocation = Location; clear bJustTeleported; OldVelocity = Velocity
    Velocity = NewFallVelocity(OldVelocity, (Acceleration.x, Acceleration.y, PawnGravityZ), step)
    if Controller and Velocity.Z <= 0 and Controller "notify apex" flag (Controller+0x270 bit 5):
        set bJustTeleported; clear the flag; Controller script NotifyJumpApex()
    if BoundSpeed != 0 and |Velocity.xy| > BoundSpeed:
        Velocity.xy = SafeNormal(Velocity.xy) * BoundSpeed
    Adjusted = (Velocity + Volume.ZoneVelocity) * step                      // NEW velocity: semi-implicit
    MoveActor(Adjusted, flags 4) → Hit                                      // flag 4: hyp. "want hit material"
    if deleted: return
    remaining -= step
    if Physics became Swimming: startSwimming; return
    if Hit.Time < 1: 4.4 / 4.5
    4.6 velocity refinement
Controller.PostPhysFalling(dt)                                              // empty for players
Acceleration = OldAcceleration
```
`APawn::NewFallVelocity @ 0x100ADF860`:
`V' = V·(1 − NetFluidFriction·dt) + A·(1 − NetBuoyancy)·dt`, where `APawn::GetNetBuoyancy @ 0x100AE1F00`
returns non-zero values only inside water volumes (Volume+0x291 bit 4); in air `V' = V + A·dt`.

### 4.4 Landing

If `Hit.Normal.Z ≥ WalkableFloorZ` [+0x258]:
- if `step·Hit.Time > 0.003` and `Hit.Time > 0.1` and not bJustTeleported:
  `Velocity = (Location − OldLocation) / (step·Hit.Time)` (velocity actually travelled until contact);
- `processLanded(Hit.Normal, Hit.Actor, remaining + step·(1 − Hit.Time), Iterations)` (5.2); return.

(If the hit actor is a pawn, the move was upward, `Hit.Time = 0` and the normal is straight up, script
`StuckOnPawn` is raised first.) Constants: 0.003 @ 0x10173D2D4, 0.1 @ 0x1016457B8.

### 4.5 Walls and ceilings (non-walkable hits)

1. `processHitWall(Hit)` (4.10) — skipped only if the old acceleration was exactly zero and the
   controller's `AirControlFromWall` succeeds (always false for players). Return if deleted or no longer
   falling.
2. `Slide = CalculateSlopeSlide(Adjusted, Hit)` (4.7). If `dot(Slide, Adjusted) ≥ 0`, move by it.
   - No further hit → continue at step 4.
   - Hit walkable → land with **zero** carried time.
   - Hit non-walkable: `processHitWall`; `TwoWallAdjust(SafeNormal(Adjusted), Slide, newN, oldN,
     Hit.Time)`; move again. If both normals point upward (Z > 0) and the adjusted slide has Z = 0 and
     `dot(oldN, newN) < 0` (wedged in a V-shaped crease) → land with zero carried time. If the final hit
     is walkable → land with zero carried time.
3. If `dot(Slide, Adjusted) < 0`, nothing more is moved.
4. For the refinement below, the old horizontal velocity is replaced by the actual average horizontal
   velocity of this sub-step (so no horizontal extrapolation after wall contact).

### 4.6 Velocity refinement after each sub-step (the "2× gravity" effect)

If not landed, not root-motion, not bJustTeleported and Physics ≠ None [CONFIRMED, disassembly]:
```
Velocity = (Location - OldLocation) / step                    // average velocity actually travelled
if OldVelocity.Z >= 0 or Velocity.Z < OldVelocity.Z:
    Velocity = 2 * Velocity - OldVelocity                      // "end velocity", all three axes
if |Velocity| > TerminalVelocity: Velocity = SafeNormal(Velocity) * TerminalVelocity   // 3D
```
`TerminalVelocity` = PhysicsVolume+0x298 (or the default PhysicsVolume's value) via
`AActor::GetTerminalVelocity @ 0x100AD7B60`.

Because the move already used the *end* velocity of the semi-implicit step, in free fall the average
equals `V0 + a·dt`, so the refined velocity is `V0 + 2·a·dt`: **velocity grows by twice the applied
acceleration per sub-step**, vertically (gravity) and horizontally (air acceleration). With
`DefaultGravityZ = −520`, `CustomGravityScaling = 1`, the effective vertical acceleration is
**−1040 uu/s²** [CONFIRMED arithmetic consequence of the code]. Corroboration [TENTATIVE, external
reference only]: the publicly known stock UDK `UTPawn` script defaults (JumpZ 322 with an AI
`MaxJumpHeight` of 49 under gravity −520) agree only if gravity is effectively doubled:
322²/(4·520) ≈ 49.8 versus 99.7 for single gravity. These numbers are **not** read from this game's
files and must not be used as ASAMU parameters.
A consequence for parity: with `BoundSpeed` active, `2·V1 − V0` can end slightly above the bound when
the direction changes, so horizontal speed can creep upward under air steering [TENTATIVE, derived].

### 4.7 CalculateSlopeSlide (AUDKPawn @ 0x100F6DCD0; APawn @ 0x100AE1280) [CONFIRMED]

Both compute `Slide = (Adjusted − N·dot(Adjusted, N)) · (1 − Hit.Time)`.
- `APawn`: if `Slide.Z > 0`, clamp `Slide.Z ≤ Adjusted.Z·(1 − Hit.Time)` (no height gained by sliding).
- `AUDKPawn` (the one used): if `SlopeBoostFriction` [UDKPawn+0x78C, hypothesis] is **0**, no clamp at
  all (sliding up slopes can gain height — "slope boosting"). Otherwise the clamp applies unless the hit
  surface's physical material friction (material → physical material +0x64) is below
  `SlopeBoostFriction`. Without a physical material the clamp applies. Default of UDKPawn+0x78C:
  [UNKNOWN — script].

### 4.8 AActor::TwoWallAdjust @ 0x100970D20 [CONFIRMED]

```
if dot(OldN, N) <= 0:                       // walls meet at <= 90°: slide along the crease
    C = SafeNormal(cross(N, OldN))
    Delta = C * dot(Delta, C) * (1 - HitTime)
    if dot(Delta, DesiredDir) < 0: Delta = -Delta
else:
    Delta = (Delta - N * dot(Delta, N)) * (1 - HitTime)
    if dot(Delta, DesiredDir) <= 0: Delta = 0
    elif |dot(OldN, N) - 1| < 1e-4: Delta += 0.1 * N       // same wall twice: nudge away (double compare)
```
Constants: 1e-4 (f64) @ 0x1016393A0, 0.1 @ 0x1016457B8.

### 4.9 UDK stuck-falling recovery (AUDKPawn::physFalling) [CONFIRMED]

After the stock routine: if `Velocity` is not exactly zero, record `WorldInfo.TimeSeconds` in
UDKPawn+0x788. If it is exactly zero, `t = now − recorded`: nothing while `t ≤ 5`; on the tick that
crosses 5 s teleport by (+1, +1, +1); after 10 s raise script `StuckFalling`. (`AUDKPawn::setPhysics`
also records the time in UDKPawn+0x788 when leaving Falling, and raises script `StoppedFalling` when
UDKPawn+0x590 bit 6 is set.) Constants: 5.0 @ 0x101639278, 10.0 @ 0x101682598; nudge X/Y 1.0 from the
two-lane constant @ 0x101666960 and nudge Z 1.0 @ 0x101633D88.

### 4.10 processHitWall (APawn @ 0x100ADDF50) — behaviourally relevant parts [CONFIRMED]

No hit actor → nothing. Portal teleporters may transform the pawn and end processing. For a non-pawn
obstacle with a controller present and "direct hit wall" (bit 25) clear:
- `dir = SafeNormal(Controller.DesiredDirection())` — for player controllers this is simply the pawn's
  velocity (`AController::DesiredDirection @ 0x1007AECF0`); when walking both `dir` and the normal are
  flattened to 2D.
- If `dot(dir, HitNormal) > Controller.MinHitWall` (Controller+0x278; a glancing contact), only
  `NotifyFallingHitWall` is raised (when falling and Controller+0x270 bit 14 is set) and the function
  returns — the pawn's `HitWall` event is **not** raised.
- Otherwise the controller's script `NotifyHitWall(normal, wall)` runs (returning true consumes the
  hit); then `NotifyFallingHitWall` when falling (same flag), or, for non-human walking pawns, crouch-walk
  retries; then the pawn's script `HitWall(normal, wall, component)`.
- If `NotifyFallingHitWall` changed the velocity, bJustTeleported is set, which disables the 4.6
  refinement for that sub-step.
Without a controller, or with bit 25 set, the pawn's `HitWall` is raised directly. Pawn-vs-pawn bumps
run AI side-step logic (constants 120.0, 1.2, 0.3, −2.0) — irrelevant for the player [TENTATIVE].

## 5. Jumping and landing

### 5.1 Jump impulse — script [CONFIRMED not native]

There is no native `DoJump` (or dodge/double-jump) symbol in the binary. The jump velocity is set by
UnrealScript (`Pawn.DoJump` / UTPawn / ASAMU overrides), presumably `Velocity.Z = JumpZ[+0x350]` plus
`SetPhysics(Falling)` [TENTATIVE until the script bytecode is read]. Native consequences once the script
has done that: falling physics (section 4) with the doubled effective gravity, and walking's
`Velocity.Z = 0` if physics is still Walking at the next tick.

`APawn::SuggestJumpVelocity @ 0x100A74C00` (AI/script helper) solves for a jump to a destination
using JumpZ, GroundSpeed and **single** (not doubled) gravity, stepping flight time by 0.1 s;
`AUDKPawn::SuggestJumpVelocity @ 0x100F698A0` retries with `JumpZ·1.3 + int at UDKPawn+0x5A0`
(hypothesis multi-jump boost) when UDKPawn+0x590 bit 2 allows double jumps and records it in bit 1
(`SetHighJumpFlag`). `APawn::GetFallDuration @ 0x100A74AB0` also uses single gravity (trace 1024 down).
These are prediction helpers, not movement [CONFIRMED].

### 5.2 processLanded (APawn @ 0x100ADF1E0) [CONFIRMED]

1. Sanity trace from the collision centre down by `2·Pawn[+0x25C] + 0.2·CollisionHeight` with
   0.9× cylinder extent (flags 0x22DF). If nothing is found, `FindSpot` is tried with a 1.1× extent;
   only if it succeeds **and** moves the pawn is the landing rejected ("stuck on a ledge"): teleport to
   the found spot, add a random horizontal kick `(rand − 0.5)·0.2·GroundSpeed` per axis, and count
   consecutive rejections (Pawn+0x500). Counts 150, 200, 250, 300 also set `Velocity.Z = max(JumpZ, 1)`;
   a rejection when the count is already ≥ 300 raises script `TakeDamage(1000)`. A rejected landing
   returns without landing (the pawn keeps falling). If `FindSpot` fails or does not move the pawn,
   the landing proceeds normally.
2. Reset the counter; `Floor = HitNormal`.
3. Controller script `NotifyLanded(HitNormal, FloorActor)`; if not handled, pawn script
   `Landed(HitNormal, FloorActor)`.
4. If still falling: `SetPostLandedPhysics` = `setPhysics(Health[+0x398] > 0 ? WalkingPhysics[+0x2A0]
   : None, FloorActor, HitNormal)` (`@ 0x100ADF830`).
5. If now walking: `Acceleration = SafeNormal(Acceleration)` (unit length).
6. `startNewPhysics(remaining, Iterations)`; then controller script `NotifyPostLanded` if
   Controller+0x270 bit 4.

`APawn::SetMaxLandingVelocity @ 0x1006D8150` is an empty function [CONFIRMED]: there is no native
landing-speed limit. `APawn::setPhysics @ 0x100AD8C00` sets force-floor-check when entering Walking;
`AActor::setPhysics @ 0x100AD8540` (re)acquires a base for None/Walking/Rotating/Spider (via a
`SetBase` to the given floor or `FindBase`, which passes a search distance of 8.0 uu to
`AActor::SearchForBaseBelow @ 0x100AD82E0` — `AActor::FindBase @ 0x100AD8450`; "downward" STRONG),
drops the base for other modes (except Interpolating), and zeroes velocity and acceleration for
None/Rotating.

## 6. Rotation, eye height, camera

- `AUDKPawn::physicsRotation @ 0x100F6E110` [CONFIRMED]: needs a controller. Turn rate comes from
  `AController::SetRotationRate(dt) @ 0x1007AEB70`, which returns **zero for human-controlled pawns**
  (AI: `RotationRate[Actor+0x1FC..0x204]·dt`), so yaw/pitch of the player's pawn are not turned by
  physics (script sets the pawn rotation from the controller). The desired pitch (DesiredRotation at
  +0x4AC..+0x4B4) is forced to 0 when walking/falling unless Pawn+0x298 bit 41 is set. Roll: a UDK "lean"
  proportional to lateral acceleration `(Velocity − OldVelocity)/dt` relative to AccelRate, limited to
  `MaxLeanRoll` [UDKPawn+0x798, hypothesis], decaying when walking below 200 uu/s (speed² < 40000) or
  when acceleration² ≤ 10000, with blend rates 8·dt and 5·dt (capped at 1). Any change is applied by a
  zero-length `MoveActor` with the new rotation. Affects the mesh, not the first-person view
  [TENTATIVE: the view uses the controller rotation, see next item].
- `APawn::GetViewRotation @ 0x100A78D40` [CONFIRMED]: controller rotation if possessed; else the
  rotation of a player controller viewing this pawn (PC+0x4C0); else the pawn's rotation.
- `APawn::GetPawnViewLocation @ 0x100A78D10` [CONFIRMED]: `Location + (0,0,BaseEyeHeight[+0x374])`.
  Script classes may override this (UTPawn does in stock UDK) [TENTATIVE].
- `AUDKPawn::UpdateEyeHeight @ 0x100F6D990` [CONFIRMED]: not locally controlled → `EyeHeight[+0x378] =
  BaseEyeHeight`; no controller → 0; otherwise the work is done by the **script** event
  `UpdateEyeHeight(dt)` (eye smoothing, landing dip, view bob live in script). Called from
  `AUDKPawn::TickSpecial` after physics (1.1).
- `ACamera` natives (`SetViewTarget @ 0x10075EB60`, `CheckViewTarget @ 0x10075EF30`,
  `ApplyCameraModifiers @ 0x10075F280`) [CONFIRMED]: view-target bookkeeping, and per-frame application
  of the camera modifier list (Camera+0x4E0, first modifier that returns true stops the chain) and of
  active camera animations (Camera+0x570) to the POV. The first-person POV itself is computed in script
  (`UpdateViewTarget` / `CalcCamera`) [TENTATIVE]. For parity: POV location ≈ pawn view location,
  rotation = controller rotation, plus script eye-height/bob and any camera modifiers/animations.

## 7. Field-offset table

Offsets are from the object start (x86_64 Mac build). "Evidence" names the functions that use the
field and how; confidence is for the *name/meaning*, the offset itself is CONFIRMED wherever cited.
Declaration-order consistency with stock UE3/UDK classes is used as a supporting indicator only.

### 7.1 Actor

| Offset | Hypothesised name | Type | Evidence | Conf. |
|---|---|---|---|---|
| +0x080 | Location | FVector | updated by MoveActor; trace origins everywhere | STRONG |
| +0x08C | Rotation | FRotator (3×int32) | passed as rotation to MoveActor; physicsRotation output | STRONG |
| +0x0C0 | Physics | byte (enum, 1.3) | dispatch switch in startNewPhysics/performPhysics | CONFIRMED |
| +0x0C1 | RemoteRole | byte | AActor::Tick tests value 2 for the client-driven path | TENTATIVE |
| +0x0C2 | Role | byte | performPhysics skipped when 2 (autonomous proxy) | STRONG |
| +0x0D0 | Base | Actor* | walking base logic, SetBase, base chain walks | STRONG |
| +0x0E8 bit 0 | bStatic | bool | aggregate base velocity skips such bases | TENTATIVE |
| +0x0E8 bit 3 | bDeleteMe | bool | performPhysics/TickAuthoritative abort when set | STRONG |
| +0x0E8 bit 4 | bTicked | bool | AActor::Tick writes the world's tick parity here | STRONG |
| +0x0E8 bit 7 | bWorldGeometry | bool | floor-check skip; static-mesh-to-dynamic conversion | TENTATIVE |
| +0x0E8 bit 17 | bCanStepUpOn | bool | decides step-up vs wall slide in walking/stepUp | TENTATIVE |
| +0x0E8 bit 59 | (unknown; "base is static-like") | bool | clear on base → force floor check each step | UNKNOWN |
| +0x0F0 bit 1 | (unknown) | bool | adds trace flag 0x100000 to the walking floor trace | UNKNOWN |
| +0x0F0 bit 12 | bJustTeleported | bool | cleared at walking/falling step start; set at apex and by FarMoveActor; suppresses velocity recompute | STRONG |
| +0x118 | WorldInfo | WorldInfo* | gravity (+0x5FC..), TimeSeconds (+0x538), NetMode (+0x5B8) reads | STRONG |
| +0x120 | LifeSpan | float | counted down; destroy at ≤ 1e-4 | STRONG |
| +0x188 | PhysicsVolume | PhysicsVolume* | gravity, friction, terminal velocity, zone velocity | STRONG |
| +0x190 | Velocity | FVector | integrated by CalcVelocity/NewFallVelocity; recomputed from displacement | CONFIRMED (usage) |
| +0x19C | Acceleration | FVector | input to CalcVelocity/physFalling; overwritten by UDK CalcVelocity | CONFIRMED (usage) |
| +0x1D8 | RelativeLocation | FVector | compared with Location − Base.Location for floor-check skip | TENTATIVE |
| +0x1F0 | CollisionComponent | PrimitiveComponent* | its +0x200 Translation offsets trace origins; cylinder test in GetCylinderExtent | STRONG |
| +0x1FC/+0x200/+0x204 | RotationRate | FRotator | read by SetRotationRate (AI) and actor rotation | STRONG |
| +0x208 | PendingTouch | Actor* | PostTouch chain at end of performPhysics | STRONG |

### 7.2 Pawn (APawn fields start at +0x250)

| Offset | Hypothesised name | Type | Evidence | Conf. |
|---|---|---|---|---|
| +0x250 | MaxStepHeight | float | stepUp height (+2.0); floor-trace length (+2.0) | STRONG |
| +0x254 | MaxJumpHeight | float | declaration order only | TENTATIVE |
| +0x258 | WalkableFloorZ | float | landing/walkable threshold on normal Z everywhere | STRONG |
| +0x25C | LedgeCheckThreshold | float | CheckForLedges drop tolerance; processLanded sanity trace | STRONG |
| +0x260 | PartialLedgeMoveDir | FVector | written by CheckForLedges with bit 19 | STRONG |
| +0x270 | Controller | Controller* | WantsLedgeCheck/MayFall/MoveTimer calls | CONFIRMED (usage) |
| +0x298 | pawn bool bitfield (64-bit) | bits | see 7.3 | — |
| +0x2A0 | WalkingPhysics | byte | physics set by SetPostLandedPhysics when alive | STRONG |
| +0x2A8 | UncrouchTime | float | counted down while trying to uncrouch | STRONG |
| +0x2AC | CrouchHeight | float | Crouch cylinder height | STRONG |
| +0x2B0 | CrouchRadius | float | Crouch cylinder radius | STRONG |
| +0x2D0 | DesiredSpeed | float | speed modifier for non-human pawns | STRONG |
| +0x2E8 | AvgPhysicsTime | float | 0.2/0.8 running average of dt | TENTATIVE |
| +0x2F0 | Buoyancy | float | (1 − x) scales gravity when buoyant; net buoyancy | STRONG |
| +0x33C | GroundSpeed | float | walking MaxSpeed; air BoundSpeed threshold; landing kick | STRONG |
| +0x340 | WaterSpeed | float | GetMaxSpeed when swimming | STRONG |
| +0x344 | AirSpeed | float | GetMaxSpeed when flying; walking→flying velocity | STRONG |
| +0x348 | LadderSpeed | float | declaration order only | TENTATIVE |
| +0x34C | AccelRate | float | GetMaxAccel; UDK acceleration magnitude; air accel limit | STRONG |
| +0x350 | JumpZ | float | SuggestJumpVelocity; stuck-landing hop | STRONG |
| +0x35C | AirControl | float | physFalling air-control factor | STRONG |
| +0x360 | WalkingPct | float | MaxSpeedModifier when bit 2 | STRONG |
| +0x364 | MovementSpeedModifier | float | MaxSpeedModifier final factor | STRONG |
| +0x368 | CrouchedPct | float | MaxSpeedModifier when crouched | STRONG |
| +0x374 | BaseEyeHeight | float | GetPawnViewLocation; UpdateEyeHeight copy source | STRONG |
| +0x378 | EyeHeight | float | UpdateEyeHeight target | STRONG |
| +0x37C | Floor | FVector | floor normal stored by walking/processLanded | STRONG |
| +0x398 | Health | int32 | SetPostLandedPhysics alive test | STRONG |
| +0x3B0 | RMVelocity | FVector | stock CalcVelocity copy/forced velocity | TENTATIVE |
| +0x470 | Mesh | SkeletalMeshComponent* | root-motion mode reads (+0x6E4, +0x6E5, +0x6B0) | STRONG |
| +0x478 | CylinderComponent | CylinderComponent* | CollisionHeight (+0x238), CollisionRadius (+0x23C) | STRONG |
| +0x4AC | DesiredRotation | FRotator | physicsRotation target | STRONG |
| +0x500 | (stuck-landing counter) | int32 | processLanded rejection counter | CONFIRMED (usage) |

### 7.3 Pawn bitfield at +0x298 (bit n = 1 << n of the 64-bit word)

| Bit | Hypothesised name | Evidence | Conf. |
|---|---|---|---|
| 2 | bIsWalking (walk-slowly) | WalkingPct; ledge-check request | STRONG |
| 3 | bWantsToCrouch | crouch trigger in performPhysics | STRONG |
| 4 | bIsCrouched | CrouchedPct; set by Crouch | STRONG |
| 5 | bTryToUncrouch | UncrouchTime countdown | TENTATIVE |
| 6 | bCanCrouch | crouch trigger | TENTATIVE |
| 7 | bCrawler | rotation special-casing | TENTATIVE |
| 10 | may-fall permission (order suggests bCanJump) | set before MayFall; read after | TENTATIVE |
| 16 | bAvoidLedges | CheckForLedges branch | TENTATIVE |
| 18 | bAllowLedgeOverhang | CheckForLedges branch | TENTATIVE |
| 19 | bPartiallyOverLedge | set with PartialLedgeMoveDir; cleared at walking start; suppresses velocity recompute | STRONG |
| 20 | bSimulateGravity | = Walking or Falling after each physics tick | STRONG |
| 22 | bCanWalkOffLedges | fall decision | TENTATIVE |
| 25 | bDirectHitWall | processHitWall branch | TENTATIVE |
| 27 | bForceFloorCheck | set by setPhysics(Walking), Crouch/UnCrouch, dynamic base | STRONG |
| 41 | (rotation: keep pitch / roll to desired) | physicsRotation | UNKNOWN |
| 49 | bRunPhysicsWithNoController | walking/performPhysics gates | STRONG |
| 50 | bForceMaxAccel | stock CalcVelocity branch | TENTATIVE |
| 51 | bLimitFallAccel | gates the air-acceleration clamp | TENTATIVE |
| 53 | bForceRMVelocity | copy of +0x3B0 in CalcVelocity/physFalling | TENTATIVE |
| 54 | bForceRegularVelocity | disables root-motion paths; selects UDK path | TENTATIVE |
| 59 | (tick base first) | APawn::Tick | UNKNOWN |

### 7.4 UDKPawn, Controller, PhysicsVolume, WorldInfo, components, hit result

| Object+offset | Hypothesised name | Type | Evidence | Conf. |
|---|---|---|---|---|
| UDKPawn+0x590 bit 1/2/6/15 | high-jump flag / can-double-jump / notify-stopped-falling / update-eye-height | bools | SetHighJumpFlag, SuggestJumpVelocity, setPhysics, TickSpecial | TENTATIVE |
| UDKPawn+0x5A0 | MultiJumpBoost | int32 | added to JumpZ in UDK SuggestJumpVelocity | TENTATIVE |
| UDKPawn+0x5A4 | CustomGravityScaling | float | final factor of AUDKPawn::GetGravityZ | STRONG |
| UDKPawn+0x61C | OldZ | float | Location.Z saved before physics | TENTATIVE |
| UDKPawn+0x788 | last-moving time while falling | float | stuck-falling timer | CONFIRMED (usage) |
| UDKPawn+0x78C | SlopeBoostFriction | float | CalculateSlopeSlide clamp switch | TENTATIVE |
| UDKPawn+0x798 | MaxLeanRoll | int32 | roll limit in physicsRotation | TENTATIVE |
| Controller+0x250 | Pawn | Pawn* | controller tick/ledge checks | STRONG |
| Controller+0x270 bit 4 / bit 5 / bit 14 / bit 16 | bNotifyPostLanded / bNotifyApex / bNotifyFallingHitWall / bPreciseDestination | bools | processLanded, physFalling, processHitWall, CalcVelocity | TENTATIVE |
| Controller+0x278 | MinHitWall | float | processHitWall threshold | TENTATIVE |
| Controller+0x29C | MoveTimer | float | decremented per tick; −1 sentinel in walking | STRONG |
| PhysicsVolume+0x284 | ZoneVelocity | FVector | GetZoneVelocityForActor; added to moves | STRONG |
| PhysicsVolume+0x290 bit 0 | bVelocityAffectsWalking | bool | gates zone velocity in walking | TENTATIVE |
| PhysicsVolume+0x291 bit 4 | bWaterVolume | bool | buoyancy/fluid friction gates | STRONG |
| PhysicsVolume+0x294 | GroundFriction | float | walking friction; slope slide | STRONG |
| PhysicsVolume+0x298 | TerminalVelocity | float | GetTerminalVelocity | STRONG |
| PhysicsVolume+0x2AC | FluidFriction | float | net fluid friction in water | TENTATIVE |
| GravityVolume+0x2D8 | GravityZ | float | AGravityVolume::GetGravityZ | STRONG |
| WorldInfo+0x538 | TimeSeconds | float | timers | STRONG |
| WorldInfo+0x5FC | WorldGravityZ | float | returned gravity; lazily initialised | STRONG |
| WorldInfo+0x600 | DefaultGravityZ | float | fallback; config −520.0 | STRONG |
| WorldInfo+0x604 | GlobalGravityZ | float | preferred initialiser when non-zero | STRONG |
| WorldInfo+0x608 | RBPhysicsGravityScaling | float | rigid-body gravity factor; config 2.0 | STRONG |
| PrimitiveComponent+0x08C | Bounds.Origin (?) | FVector | walking floor-trace start | TENTATIVE |
| PrimitiveComponent+0x200 | Translation | FVector | added to Location for trace starts | STRONG |
| CylinderComponent+0x238 | CollisionHeight | float | extents, crouch | STRONG |
| CylinderComponent+0x23C | CollisionRadius | float | extents, crouch | STRONG |
| Hit+0x08 / +0x10 / +0x1C / +0x28 | Actor / Location / Normal / Time | — | every MoveActor/trace consumer | STRONG |
| Hit+0x30 | Material | Material* | physical material for slope slide | TENTATIVE |
| Hit+0x40 | Component | PrimitiveComponent* | static-mesh conversion | STRONG |
| Hit+0x64 | bStartPenetrating | bool | walking floor adjustment guard | TENTATIVE |

## 8. Constants table

All values were re-read from the file bytes at the given data address during the verification pass
(f32 unless noted; file offset = address − 0x100000000). "Imm." = immediate operand. Every function
address was checked against the symbol table (`nm | c++filt`). Abbreviations used below:
PW = `APawn::physWalking @ 0x100ADA470`, PF = `APawn::physFalling @ 0x100ADF900`,
SU = `APawn::stepUp @ 0x100ADCFA0`, PL = `APawn::processLanded @ 0x100ADF1E0`,
UPF = `AUDKPawn::physFalling @ 0x100F6DB80`.

| Value | Function @ address | Data addr. | Role | Conf. |
|---|---|---|---|---|
| 0.0003 | APawn::startNewPhysics @ 0x100AD8C30 | 0x101663900 | minimum time slice | CONFIRMED |
| 7 (→ max 8 steps) | APawn::startNewPhysics @ 0x100AD8C30; PW; PF (compares with 8) | imm. | iteration budget per tick | CONFIRMED |
| 0.05 | PW; PF | 0x101636058 | max sub-step; air-control threshold | CONFIRMED |
| 0.5 | PW; PF | 0x101636054 | sub-step halving; min friction in slope slide (PW) | CONFIRMED |
| 1.0 | PW, PF, AUDKPawn::CalcVelocity @ 0x100F6BCA0 and most others | 0x101633D88 | unit constant (1 − x terms, SafeNormal shortcut, Z nudge in UPF) | CONFIRMED |
| 1e-4 | PW (nearly-zero move test), and others | 0x10163FED8 | "nearly zero" per component | CONFIRMED |
| 1e-8 | AUDKPawn::CalcVelocity @ 0x100F6BCA0, PW, PF, and others | 0x10163FED4 | SafeNormal length² threshold | CONFIRMED |
| 0.03 | APawn::ApplyVelocityBraking @ 0x100ADC2C0; AUDKPawn::CalcVelocity @ 0x100F6BCA0 | 0x101731DB0 | braking sub-interval | CONFIRMED |
| 2 (×V) | APawn::ApplyVelocityBraking @ 0x100ADC2C0; AUDKPawn::CalcVelocity @ 0x100F6BCA0 | code (V+V) | braking factor 2·Friction | CONFIRMED |
| 100.0 | APawn::ApplyVelocityBraking @ 0x100ADC2C0; AUDKPawn::CalcVelocity @ 0x100F6BCA0 | 0x10163713C | braking stop speed² (10 uu/s) | CONFIRMED |
| 25.0 (×2 lanes) | PW | 0x10173D2F0 | zone-velocity factor in walking | CONFIRMED |
| 0.98 | PW | 0x101724EC4 | floor "flat enough" → plain move | CONFIRMED |
| 2.0 | PW; SU | 0x101653788 | step / floor-trace fudge | CONFIRMED |
| 2.4 | PW | 0x10173D2B0 | max hover before snapping down; assumed dist when trace skipped | CONFIRMED |
| 1.9 | PW | 0x10173D2B4 | min hover before pushing up | CONFIRMED |
| 2.15 | PW | 0x10173D2B8 | target hover height | CONFIRMED |
| 0.1 | PW; PF; AActor::TwoWallAdjust @ 0x100970D20 | 0x1016457B8 | assumed Hit.Time; steep-slope push-off; landing min Hit.Time; same-wall nudge | CONFIRMED |
| 0.99 | PW | 0x1016A7A60 | slope-slide normal Z limit | CONFIRMED |
| 3.3 | PW | 0x10173D2BC | slope-slide Nz·friction limit | CONFIRMED |
| −1.0 | PW; AActor::TwoWallAdjust @ 0x100970D20 | 0x101639274 | MoveTimer sentinel (PW); "dot − 1" (TwoWallAdjust) | CONFIRMED |
| −0.08 | SU | 0x10173D2C8 | near-vertical wall test (dot with down) | CONFIRMED |
| 144.0 | SU; AActor::stepUp @ 0x100ADD770 | 0x10173D2CC | recursion threshold on \|Delta\|²·Time | CONFIRMED |
| 35.0 | AActor::stepUp @ 0x100ADD770 | 0x10173D300 | fixed step height for non-pawn actors | CONFIRMED |
| 0.08 | AActor::stepUp @ 0x100ADD770 | 0x101701918 | \|Nz\| wall test for non-pawns | CONFIRMED |
| 10.0 | PF; UPF | 0x101682598 | air low-speed boost limit (PF); StuckFalling delay (UPF) | CONFIRMED |
| 0.003 | PF | 0x10173D2D4 | min contact time for landing velocity | CONFIRMED |
| (0,0) | PF | 0x101636340 | X/Y lanes added to `Acceleration` when building the fall acceleration (Z lane = GravityZ) | CONFIRMED (disassembly) |
| 0.0 | PF | 0x1016526A4 | Z term in the 2-D BoundSpeed length test | CONFIRMED |
| 5.0 | UPF | 0x101639278 | stuck-falling nudge delay | CONFIRMED |
| (1,1) + 1.0 | UPF | 0x101666960 (X/Y), 0x101633D88 (Z) | stuck-falling nudge (1,1,1) | CONFIRMED |
| 0.2 / 0.8 | APawn::performPhysics @ 0x100AD8990 | 0x1016825B8 / 0x101656608 | dt running average | CONFIRMED |
| 0.2 | PL | 0x1016825B8 | CollisionHeight fraction in sanity trace; kick scale | CONFIRMED |
| 0.9 | PL | 0x10173D310 (X/Y), 0x10171CA54 (Z) | sanity-trace extent scale | CONFIRMED |
| 1.1 | PL | 0x1016844C0 (X/Y), 0x1016844A8 (Z) | FindSpot extent scale | CONFIRMED |
| −0.5 | PL | 0x10163926C | random kick centring | CONFIRMED |
| 149, 50, 300, 1000 | PL | imm. | stuck-landing counter: hop when old count ≥ 149, new count a multiple of 50 and old < 300 (i.e. counts 150/200/250/300); damage 1000 once old count ≥ 300 | CONFIRMED |
| 1e-4 (f64) | AActor::TwoWallAdjust @ 0x100970D20 | 0x1016393A0 | same-wall test | CONFIRMED |
| 8.0 | AActor::FindBase @ 0x100AD8450 (passed to AActor::SearchForBaseBelow @ 0x100AD82E0) | 0x101684250 | base search distance | CONFIRMED value; STRONG role |
| 1.001 | UWorld::MoveActor @ 0x1008E94A0 | 0x10171DA60 | horizontal extent inflation of the move sweep | STRONG |
| 40000, 10000, 8.0, 5.0, 65536 | AUDKPawn::physicsRotation @ 0x100F6E110 | 0x10176482C, 0x101682424, 0x101684250, 0x101639278, 0x101639264 | lean-roll speed²/accel² gates, blend rates, wrap | CONFIRMED |
| 0.1, 0.25, 4.0 | APawn::SuggestJumpVelocity @ 0x100A74C00 | 0x1016457B8, 0x1016856E8, 0x1016457CC | AI jump solver | CONFIRMED |
| 0.3 | AUDKPawn::SuggestJumpVelocity @ 0x100F698A0 | 0x101633D98 | extra JumpZ fraction | CONFIRMED |
| −1024 | APawn::GetFallDuration @ 0x100A74AB0 | 0x101731DAC | fall-prediction trace | CONFIRMED |
| −520.0 | config `ASAMU/Config/DefaultGame.ini` [Engine.WorldInfo] DefaultGravityZ (read by AWorldInfo::GetGravityZ @ 0x100AD7B10 via WorldInfo+0x600) | — | world gravity default | CONFIRMED |
| 2.0 | config same section RBPhysicsGravityScaling (read by AActor::GetGravityZ @ 0x100AD7C30 via WorldInfo+0x608, RigidBody only) | — | rigid-body gravity factor | CONFIRMED |
| 22 / 62, TRUE | config `Engine/Config/BaseEngine.ini` [Engine.Engine] Min/MaxSmoothedFrameRate, bSmoothFrameRate (the same file's [UnrealEd.EditorEngine] section has FALSE / 5 / 120, editor only) | — | frame-rate smoothing → typical dt ≥ 1/62 s | CONFIRMED value; TENTATIVE effect (user ini may differ) |

Trace-flag masks seen (raw values CONFIRMED; meaning TENTATIVE): 0x20DF all blocking incl. pawns;
0x22DF same + stop at first hit; 0x2086 world geometry; 0x2286 world geometry + stop at first hit;
0x1020DF / 0x24DF variants.

## 9. Implementation notes for asamu-player

### 9.1 Parameters the Rust model needs

Per pawn (from script defaults / ASAMU overrides — **to be read from packages**, never guessed):
GroundSpeed, AccelRate, AirControl, JumpZ (script jump), MaxStepHeight, WalkableFloorZ,
LedgeCheckThreshold, CollisionRadius, CollisionHeight, CrouchHeight, CrouchRadius, WalkingPct,
CrouchedPct, MovementSpeedModifier, BaseEyeHeight, CustomGravityScaling, SlopeBoostFriction,
Buoyancy, flags: bit 51 (limit fall accel), bit 22 (walk off ledges), bit 49, WalkingPhysics.
Per volume: GroundFriction, TerminalVelocity, ZoneVelocity + bVelocityAffectsWalking, water flag,
FluidFriction, GravityVolume GravityZ. Per world: WorldGravityZ / GlobalGravityZ (map WorldInfo),
DefaultGravityZ (−520.0 config).

### 9.2 World queries the Rust model needs

1. **Swept cylinder move** (`MoveActor`): sweep a vertical cylinder (radius ×1.001 horizontally,
   half-height) by Delta; stop at the first blocking hit; return Time ∈ [0,1], Normal, hit actor,
   material/physical-material friction, "started penetrating". The original's collision primitives
   keep a small skin; the walking code relies on hovering 1.9–2.4 uu above floors, so our sweep must be
   conservative in the same way [exact skin UNKNOWN].
2. **Cylinder line check** (`SingleLineCheck` with extent): floor probe (length MaxStepHeight + 2),
   air-control wall probe (world only), landing sanity probe (0.9× extent), base search (8 uu).
3. **Point/small-extent probes** for ledge checks (only needed for crouch/slow walk).
4. **Encroachment test** for crouch growth, **teleport** (`FarMoveActor`) for uncrouch/revert.
5. Volume lookup at the pawn location (gravity/friction/terminal velocity).

### 9.3 Order of operations per fixed tick (summary)

```
tick(dt):
  [script] controller: input → Acceleration (expected AccelRate·normalised wish dir), rotation
  [script] pawn Tick / state code; jump sets Velocity.Z and Physics = Falling
  performPhysics(dt): crouch housekeeping → startNewPhysics(dt, 0) → rotation
      Walking:  CalcVelocity once (2.1) → ≤8 sub-steps of move/step-up/floor-snap/ledge → V = disp/dt, Vz=0
      Falling:  air-control clamp once → sub-steps: V=V+(A,g)dt; move by V·h; land/slide; V=2·avg−V_old; clamp terminal
      transitions carry unused sub-step time (1.4)
  TickSpecial: [script] UpdateEyeHeight
```

### 9.4 Determinism / parity tests to write

- Braking: from v = (400,0,0), F = GroundFriction, compare the averaged result for dt = 1/62 and 1/30.
- Free fall from rest: after n sub-steps of h, `Vz = 2·g·n·h` and `z = g·h²·n²` (exact closed form of
  the refined scheme: position uses V0 + (2k−1)·g·h at sub-step k).
- Jump apex for Vz0 = JumpZ: ≈ JumpZ²/(4·|g|) (discrete error depends on h).
- Slope walking: horizontal speed unchanged on a walkable ramp; hover height in [1.9, 2.4].
- Iteration budget: dt = 0.5 → 8 sub-steps of 0.05/…; remainder dropped.

### 9.5 Open questions

1. Default of Pawn+0x298 bit 51 (air-acceleration limit) for ASAMU's pawn class — decides whether
   AirControl matters at all. (Script defaults.)
2. ASAMU pawn class chain (expected ASAMUPawn → UTPawn → UDKPawn) and its defaults for every parameter
   in 9.1; whether ASAMU overrides `DoJump`, `PlayerMove`, `UpdateEyeHeight`, `CalcCamera`.
3. Does the controller tick before the pawn each frame (input latency)? Read `UWorld::Tick` /
   tick-group ordering.
4. What physics mode and forces the **grapple** uses (script); `physCustom` is empty natively.
5. Exact return value of `CheckForLedges` (crouch/slow-walk ledge behaviour).
6. Mesh root-motion mode of the player pawn (must be "ignore" for the UDK velocity path).
7. Collision skin/pullback inside the original line checks (BSP/static mesh) — affects hover distances
   and wall contact.
8. Per-map WorldGravityZ/GlobalGravityZ, GravityVolumes, PhysicsVolume GroundFriction/TerminalVelocity
   values in ASAMU maps.
9. Default of UDKPawn+0x78C (slope boosting) and of the lean-roll limit (visual only).
10. Effective frame-time behaviour on the original (frame smoothing 22–62 fps per BaseEngine.ini; the
    user's generated ini and the ASAMU settings manager may change it).

## 10. Other modes and non-pawn routines

Present but not specified here (not expected for the player) [TENTATIVE]: `APawn::physSwimming`,
`physFlying`, `physLadder`, `physSpider` (uses `GetGravityDirection` = −Floor when spidering),
`physNavMeshWalking`, `AActor::physProjectile`, `AActor::physInterpolating` (matinee/movers),
rigid-body modes. Actor-level `AActor::physWalking @ 0x100AE2760` / `AActor::physFalling @ 0x100AE07A0`
/ `AActor::stepUp` (fixed 35.0 step, |Nz| < 0.08) / `AActor::physicsRotation` / `AActor::moveSmooth`
serve non-pawn actors and AI. `APawn::physicsRotation @ 0x100AD9000` is superseded by the UDK version.
If script ever switches the player into one of these modes (e.g. ladders, swimming, grapple via
Flying/Spider), that routine must be specified next.

## 11. Verification log

An independent adversarial pass (2026-10-09) re-read the local decompilation, the disassembly and the
raw constant bytes for the claims below, without relying on the first author's notes. Method: every
`Class::Function @ address` in this document was matched against `nm | c++filt` (all distinct
citations, 63 after this pass, match; four are local `t` symbols: `AActor::TwoWallAdjust`, `AActor::physCustom`,
`AGravityVolume::GetGravityZ`, `APawn::SetMaxLandingVelocity`); every constant in section 8 was re-read
from the file at its data address; virtual-call targets were re-resolved from the `AUDKPawn`,
`APawn`, `APhysicsVolume` and `AUDKPlayerController` vtables; dropped float arguments were read from
the call-site disassembly.

| # | Claim | Result |
|---|---|---|
| 1 | Mode dispatch values → handlers (1.3), incl. AUDKPawn overrides at slots +0x450, +0x9D8, +0x2B0 | CONFIRMED |
| 2 | Sub-step = remaining if ≤ 0.05, else min(0.05, 0.5·remaining); ≤ 8 sub-steps per tick across modes; slices < 0.0003 s ignored | CONFIRMED |
| 3 | Walking → falling time carry `remaining_after + step·(1 − frac)`, frac from horizontal progress | CONFIRMED (both paths); **corrected**: the inline path is not identical to StartFalling (1.4) |
| 4 | Falling → walking carry `remaining_after + step·(1 − Hit.Time)` on direct landing, 0 after a slide | CONFIRMED (disassembly) |
| 5 | Walking calls CalcVelocity once with (GroundSpeed, GroundFriction, bFluid 0, bBrake 1, bBuoyant 0) and the whole dt | CONFIRMED (disassembly) |
| 6 | UDK CalcVelocity: Accel = dir·AccelRate; turning friction; inlined braking; 3-D cap at MaxSpeed·MaxSpeedModifier (vtable +0x990 = MaxSpeedModifier) | CONFIRMED |
| 7 | Braking in 0.03 s pieces, factor (1 − 2Fh), time-weighted average of post-update velocities, stop below speed² 100 or on reversal | CONFIRMED |
| 8 | Uphill moves routed through stepUp when Floor.Z < 0.98 and the move pushes into the floor | CONFIRMED |
| 9 | stepUp: StepDown = MaxStepHeight + 2; −0.08 wall test; recursion when \|Delta\|²·Time > 144 | CONFIRMED; **refined**: walking keeps the planned step-down on unwalkable slopes, and stepUp's slide has no nearly-zero skip (3.2.1) |
| 10 | Hover band 1.9 / 2.4 / target 2.15, floor probe MaxStepHeight + 2, skip-trace values (Time 0.1, dist 2.4) | CONFIRMED |
| 11 | ShouldCatchAir always false; WantsLedgeCheck only when crouched or walking slowly | CONFIRMED |
| 12 | Fall decision flags (bits 10/22/2/4, bJustTeleported, mustJump) incl. the "zero move" flag | CONFIRMED (the zero-move flag was traced in disassembly because the decompiler mistyped it); **corrected** bit-59 wording and the MayFall precondition (3.4) |
| 13 | Slope slide: Nz < 0.99 and Nz·F < 3.3, `g·dt²/(2·max(F,0.5))`, whole-call dt on every sub-step | CONFIRMED |
| 14 | Final walking velocity = displacement / dt with Z = 0, suppressed by the bit-19 value at call start or bJustTeleported | CONFIRMED |
| 15 | Falling: velocity updated by NewFallVelocity(Acceleration + (0,0,GravityZ)) **before** the move; move uses the new velocity; afterwards `V = 2·avg − V_old` when `V_old.Z ≥ 0 or avg.Z < V_old.Z`; terminal-velocity clamp via vtable +0x280 | CONFIRMED (code + disassembly); the 2× effective acceleration is an arithmetic consequence |
| 16 | Air-control probe and clamp (0.05, 10, BoundSpeed, bit 51 = byte +0x29E bit 3) | CONFIRMED |
| 17 | Landing threshold WalkableFloorZ (Pawn+0x258); landing velocity only if step·Time > 0.003 and Time > 0.1 | CONFIRMED |
| 18 | Gravity chain and config −520.0 / 2.0 | CONFIRMED |
| 19 | UDK slope slide: no Z clamp when SlopeBoostFriction = 0; otherwise clamp unless phys-material friction < SlopeBoostFriction; clamp also when no phys material | CONFIRMED (disassembly; the decompiler dropped the clamp) |
| 20 | TwoWallAdjust formulas and 1e-4 (f64) / 0.1 nudge | CONFIRMED |
| 21 | Stuck-falling recovery 5 s nudge / 10 s event | CONFIRMED; **citation corrected**: the Z part of the (1,1,1) nudge comes from 0x101633D88 |
| 22 | processLanded counter rules 149/50/300/1000 | CONFIRMED |
| 23 | Human pawns get zero turn rate from SetRotationRate (controller slot +0x828 not overridden) | CONFIRMED |
| 24 | No native DoJump / dodge symbol | CONFIRMED (no matching symbol in `nm`) |

No claim checked was refuted outright; the items marked **corrected** were imprecise and have been fixed
in place. A scan of this file for decompiler identifiers and pasted pseudo-C found none.

## Changelog

- 2026-10-09: first complete version (sections 1–10) from the 111 functions in the two anchor lists,
  vtable resolution and disassembly checks.
- 2026-10-09: verification pass (section 11). Corrected the walking→falling inline-path description,
  the stepUp walking/slide details, the fall-decision preconditions and base-restore rule, the
  CheckForLedges acceptance bound, and the stuck-falling nudge citation; labelled the names of modes
  5/6/13 TENTATIVE; every constants-table row now cites `Function @ address`.
