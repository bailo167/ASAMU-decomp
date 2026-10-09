# Parity

We measure **behavioural parity** with the original game, not byte-matching builds. A matching build, a
deterministic export and behavioural parity are separate numbers and are never added together.

| Area | Measure | Oracle | Status |
|---|---|---|---|
| Package reading | Every shipped package parses; offsets/sizes cross-check against file size and each other | The original files | not started |
| Gameplay constants | Value recovered from script defaults / native code / config, cited | The original files | not started |
| Player movement | Tick-by-tick position/velocity error vs original trace | Original game on Windows (traces) | not started |
| Grapple | Attach point, swing path, release velocity vs original trace | Original game on Windows (traces) | not started |
| Levels | Geometry/transforms match original placement | Original maps (converted locally) | not started |
| Story flow | Kismet-driven event order matches original | Kismet graphs + playthrough | not started |

## Known deviations

None recorded yet. Every intentional or known deviation must be listed here with its reason.

## Trace format

Implemented in `crates/asamu-player/src/trace.rs` (schema version **1**). Original-game traces (to be
recorded later on Windows) and runtime traces share this schema so a harness can compare them tick by tick.
Producing original traces is not possible yet; the format is the contract a future recorder must meet.

### Layout: JSON Lines

- **Line 1** is a `TraceMeta` object; **every following non-blank line** is one `TraceSample` object.
  Blank lines are ignored; `\n` and `\r\n` endings are accepted; lines must be UTF-8 and at most
  `MAX_LINE_BYTES` (1 MiB). Unknown fields and duplicate keys are rejected (`deny_unknown_fields`), so
  typos fail loudly. Exactly one JSON object per line (trailing text is an error).
- Readers validate everything and report 1-based line numbers: missing/invalid meta, wrong `format`,
  unsupported `schema_version` (only `1` is accepted; there is no silent upgrade), `units` other than
  `"uu"`, a non-positive or non-finite `tick_rate`, invalid UTF-8, over-long lines, non-finite numbers
  (including JSON numbers that overflow `f32`), `fov` outside (0, 180), negative `rope_length`, ticks
  that do not strictly increase, and `grapple_state` / `grapple_anchor` / `rope_length` disagreement.
  Writers validate before writing. All of these are covered by tests in `trace.rs`.
- `f32` values are written as the exact decimal of the `f32` widened to `f64`
  (`asamu_core::exact_f32`), and `serde_json` is built with `float_roundtrip`, so runtime traces round-trip
  **bit-for-bit** (tested). Hand-written or recorder-produced traces may use any JSON number; values are
  parsed as `f64` and rounded to the nearest `f32`.

### Conventions

UE3 axes (left-handed: X forward, Y right, Z up), distances in **Unreal units**, velocities in UU/s,
yaw/pitch in **radians** using the UE3 rotator convention (+yaw turns right, +pitch looks up; convert
rotator units with `asamu_core::rotator`), FOV in **degrees, horizontal**. Sample `tick = k` holds the
input applied during tick `k` and the state at the **end** of that tick. A recording may start with a sample
whose input is neutral and whose state is the initial state (runtime recordings do; tick 0 for
`record_run`). Inputs are stored sanitized (exactly as the simulation consumed them), so replaying a trace's
inputs from its first state reproduces it.

### `TraceMeta` (line 1)

| Field | Type | Meaning |
|---|---|---|
| `format` | string | Always `"asamu-trace"`. |
| `schema_version` | integer | `1`. |
| `source` | `"original"` \| `"runtime"` | Who produced the trace. |
| `game_build` | string or null | Original build (e.g. Steam build id) when known. |
| `level` | string or null | Map / level name when known. |
| `tick_rate` | number or null | Fixed tick rate in Hz; `null` for variable-rate recordings (the original's tick model is UNKNOWN; UE3 uses variable `DeltaTime`). |
| `units` | string | Must be `"uu"`. |
| `notes` | array of strings | Free-form provenance/caveats (e.g. respawn ticks, "placeholder physics"). |

### `TraceSample` (lines 2+)

| Field | Type | Unit | Meaning |
|---|---|---|---|
| `tick` | integer | tick | Strictly increasing; the alignment key for comparison. |
| `time` | number | s | Time at the end of the tick. Informational; not compared. |
| `input` | object | — | `move_forward`, `move_right` (−1..1), `look_yaw_delta`, `look_pitch_delta` (rad), `jump_pressed` (edge), `jump_held`, `grapple_held` (level). |
| `position` | [x, y, z] | UU | Collision-shape centre after the tick. |
| `velocity` | [x, y, z] | UU/s | Velocity after the tick. |
| `yaw`, `pitch` | number | rad | View rotation. |
| `fov` | number | deg | Horizontal field of view. |
| `grapple_state` | `"idle"` \| `"attached"` | — | Grapple state. |
| `grapple_anchor` | [x, y, z] or null | UU | Required when attached, null when idle. |
| `rope_length` | number or null | UU | Rope length when attached and known; null when idle. |
| `grounded` | bool | — | Standing on a walkable floor. |

### Comparison (`trace::compare` / `compare_with`)

Samples are aligned by `tick` with a linear merge (no hash ordering). For each matched tick the harness
computes:

| Field | Error metric |
|---|---|
| position, velocity, grapple anchor | Euclidean distance (anchor only when both are attached) |
| rope length | absolute difference (only when both are attached and both record a length) |
| yaw | absolute angular difference, wrapped to [0, π] |
| pitch, fov | absolute difference |
| grapple state, grounded, input | mismatch counts |

Each continuous field reports `count`, `max` (and the tick of the max; ties keep the first), `mean` and
`rms`, accumulated in `f64`. The diff also reports matched / only-in-a / only-in-b tick counts and the
**first divergence**: the first tick at which any field exceeds its tolerance, checking fields in the
table's order within a tick (`CompareTolerances`; default all zero, i.e. any difference counts —
tolerances are an analysis choice per study, not a property of the game; NaN or negative tolerances act as
zero, `+inf` ignores a field). `TraceDiff::is_exact()` is true when nothing differs and no tick is
unmatched. The metrics are tested on hand-constructed traces with known answers.

### Determinism

Runtime traces are reproducible byte for byte: tests run the same scenario twice (and interleaved with
another simulation) and compare the serialized JSONL bytes. The simulation uses no platform maths
library: trigonometry comes from `asamu_core::det_math` (pure Rust, IEEE basic operations only, pinned by
golden bit patterns), so the same trace is expected on Windows, Linux and macOS. That cross-platform
expectation has **not** yet been checked by producing the same trace on each OS (TENTATIVE).

## Placeholder parameters

**Every gameplay parameter in the runtime is a PLACEHOLDER except `movement.world_gravity_z`**, which is the
original's `DefaultGravityZ = -520` from `ASAMU/Config/DefaultGame.ini` (`[Engine.WorldInfo]`; CONFIRMED, see
`docs/reverse-engineering/NATIVE_PHYSICS.md` 4.1; per-map overrides are UNKNOWN). No other value below was
recovered from the original game; they were chosen so the graybox is playable and were not deliberately taken
from stock UE3/UDK defaults either. If a value happens to coincide with an engine default, that is not evidence
about ASAMU and must not be cited as such. Engine property names in the notes are *leads* for where to look
(TENTATIVE). Findings in `docs/reverse-engineering/` do not change this table until a value is wired into
the code with its `Provenance`. The table is
generated from `PlayerParams::default().provenance_report()`
(`cargo run -p asamu-player --example provenance_table`); a test
(`crates/asamu-player/tests/docs_sync.rs`) fails if it drifts from the code. When a value is recovered,
its `Provenance` changes (script default / native code / config / measured trace) and the table shows the
source instead. Which model reads which parameter is listed under "Movement model" below.

| Parameter | Value | Unit | Provenance | Note / source |
|---|---|---|---|---|
| `movement.gravity_z` | -1000 | uu/s^2 | placeholder | graybox placeholder used only by PlaceholderMovement (the UE3 pawn model uses movement.world_gravity_z x movement.custom_gravity_scaling, doubled in effect by the falling refinement) |
| `movement.max_ground_speed` | 450 | uu/s | placeholder | graybox placeholder; replace with value recovered from ASAMU pawn defaults (lead: GroundSpeed, TENTATIVE) |
| `movement.ground_acceleration` | 3000 | uu/s^2 | placeholder | graybox placeholder; replace with value recovered from ASAMU pawn defaults (lead: AccelRate, TENTATIVE) |
| `movement.braking_deceleration` | 3000 | uu/s^2 | placeholder | graybox placeholder used only by PlaceholderMovement (the original brakes with 2 x ground friction, see movement.ground_friction) |
| `movement.air_control` | 0.25 | fraction | placeholder | graybox placeholder; replace with value recovered from ASAMU pawn defaults (lead: AirControl, TENTATIVE) |
| `movement.jump_velocity` | 450 | uu/s | placeholder | graybox placeholder; replace with value recovered from ASAMU pawn defaults (lead: JumpZ, TENTATIVE) |
| `movement.capsule_radius` | 20 | uu | placeholder | graybox placeholder (human-sized at the presentation scale); replace with the ASAMU pawn collision component radius (lead: CollisionRadius, TENTATIVE) |
| `movement.capsule_half_height` | 45 | uu | placeholder | graybox placeholder (human-sized at the presentation scale); replace with the ASAMU pawn collision component height (lead: CollisionHeight, TENTATIVE) |
| `movement.step_height` | 18 | uu | placeholder | graybox placeholder; replace with value recovered from ASAMU pawn defaults (lead: MaxStepHeight, TENTATIVE) |
| `movement.max_fall_speed` | 4000 | uu/s | placeholder | graybox placeholder used only by PlaceholderMovement (the original clamps the 3-D falling speed to movement.terminal_velocity) |
| `movement.walkable_floor_z` | 0.7 | normal z | placeholder | graybox placeholder; replace with the original's walkable-slope threshold (lead: WalkableFloorZ, TENTATIVE) |
| `movement.world_gravity_z` | -520 | uu/s^2 | config | config ASAMU/Config/DefaultGame.ini Engine.WorldInfo.DefaultGravityZ |
| `movement.custom_gravity_scaling` | 1 | factor | placeholder | graybox placeholder (neutral factor); replace with the ASAMU pawn default (lead: UDKPawn CustomGravityScaling, TENTATIVE) |
| `movement.ground_friction` | 6 | 1/s | placeholder | graybox placeholder; replace with the PhysicsVolume ground friction of the original maps (lead: PhysicsVolume GroundFriction, TENTATIVE) |
| `movement.terminal_velocity` | 4000 | uu/s | placeholder | graybox placeholder (same number as movement.max_fall_speed); replace with the original's PhysicsVolume terminal velocity (lead: TerminalVelocity, TENTATIVE) |
| `movement.limit_fall_accel` | true | - | placeholder | graybox placeholder; the default of the air-acceleration limit flag (Pawn+0x298 bit 51) is UNKNOWN (lead: bLimitFallAccel in pawn defaults, TENTATIVE) |
| `movement.slope_boost_friction` | 0.5 | friction | placeholder | graybox placeholder (only zero vs non-zero matters without physical materials); replace with the ASAMU pawn default (lead: UDKPawn SlopeBoostFriction, TENTATIVE) |
| `movement.movement_speed_modifier` | 1 | factor | placeholder | graybox placeholder (neutral factor); replace with the ASAMU pawn default (lead: Pawn MovementSpeedModifier, TENTATIVE) |
| `grapple.max_range` | 2500 | uu | placeholder | graybox placeholder; replace with the grapple range from ASAMU script defaults (class not yet identified) |
| `grapple.pull_acceleration` | 900 | uu/s^2 | placeholder | graybox placeholder; replace with the grapple pull behaviour recovered from ASAMU script (UNKNOWN whether it is an acceleration at all) |
| `grapple.min_rope_length` | 60 | uu | placeholder | graybox placeholder; numerical guard so pull never reaches the anchor; replace once the original's behaviour near the anchor is known |
| `grapple.rope_mode` | inelastic | - | placeholder | graybox placeholder; the original's rope model (fixed length vs reel-in vs spring) is UNKNOWN |
| `grapple.release_mode` | preserve_velocity | - | placeholder | graybox placeholder; whether the original modifies velocity on release is UNKNOWN (verify with traces) |
| `grapple.attached_max_speed` | 2500 | uu/s | placeholder | graybox placeholder; it is UNKNOWN whether the original caps swing speed |
| `camera.fov_degrees` | 90 | deg (horizontal) | placeholder | graybox placeholder; replace with the FOV from ASAMU camera/viewport defaults or config (lead: ASAMU.ASAMUViewportClient, TENTATIVE) |
| `camera.eye_height` | 38 | uu | placeholder | graybox placeholder; replace with value recovered from ASAMU pawn defaults (lead: BaseEyeHeight, TENTATIVE) |
| `camera.max_pitch_degrees` | 89 | deg | placeholder | graybox placeholder; replace with the original's view pitch limits (lead: camera/controller pitch clamp, TENTATIVE) |

The locomotion model `asamu_player::movement::PlaceholderMovement` is also a placeholder, not a
reimplementation of the original's native physics; see `docs/ARCHITECTURE.md`. So are its behavioural
choices: analog input scales walking speed, the floor supports a grounded player against upward
acceleration weaker than gravity, a taut grapple rope tethers a standing player horizontally, the rope does
not wrap around geometry, and a grapple tap shorter than one tick is not seen (the button is sampled as a
level). Simulation tolerances in code (`CONTACT_SKIN`, `MIN_MOVE`, `ROPE_TAUT_EPSILON`, floor-probe
distance, `MAX_SLIDE_ITERATIONS`, `sim::MAX_STEP_DT`) are numerical implementation details, not gameplay
constants; `CONTACT_SKIN` is nevertheless visible in traces (standing height), so it is a known source of
runtime-vs-original difference.

## Movement model

Two implementations of `asamu_player::movement::MovementModel` exist; `MovementModelKind` selects one at run
time. `asamu_player::sim::step` and `asamu_game::Game` still default to the placeholder;
`Game::with_movement_model(MovementModelKind::Ue3Pawn)` or `sim::step_with(&Ue3PawnMovement, ...)` runs the port.

| Model | What it is | Status |
|---|---|---|
| `PlaceholderMovement` | Our own graybox model (see above) | placeholder, not parity-relevant |
| `Ue3PawnMovement` (`crates/asamu-player/src/ue3_movement.rs`) | Port of the original's native UE3/UDK pawn walking/falling physics, written independently from `docs/reverse-engineering/NATIVE_PHYSICS.md` (the spec; no decompiled code was used) | implemented and tested against closed-form consequences of the spec; **parity with the original not measured** (no original traces yet) |

### What `Ue3PawnMovement` reproduces (spec section → behaviour)

| Spec | Behaviour in the port |
|---|---|
| 1.3, 1.4 | `startNewPhysics` loop: slices < 0.0003 s ignored; sub-step = remaining if ≤ 0.05 s else `min(0.05, remaining/2)`; at most 8 sub-steps per tick shared by walking and falling, the rest dropped; walking → falling gives back `step·(1 − frac)` (frac = horizontal progress of the sub-step; 0 for an exactly zero move); a direct landing carries `remaining + step·(1 − Hit.Time)` into walking, a landing found after a slide carries 0. |
| 2.1 | UDK `CalcVelocity` once per walking call with the whole `dt`: acceleration = input direction × `AccelRate` (analog magnitude discarded), turning friction `V −= (V − dir·|V|)·F·dt`, 3-D cap at `GroundSpeed · MovementSpeedModifier`. |
| 2.3 | Braking without input: 0.03 s pieces, factor `1 − 2·F·h`, time-weighted average of the pieces, zero on reversal or below 10 uu/s. |
| 3.1, 3.2, 3.2.1 | Uphill moves on floors with normal Z < 0.98 go through `stepUp` (up `MaxStepHeight + 2`, across, down); wall hits step up, repeat while `|Delta|²·Time > 144` (climbs several stairs per sub-step), else slide along the horizontalised normal with one `TwoWallAdjust` retry. |
| 3.3 | Floor probe `MaxStepHeight + 2` below the centre; hover band: pushed up to 2.15 when < 1.9, snapped down to 2.15 when > 2.4 or on a new base; probe skipped for a zero move on an unchanged static base (floor reused, distance 2.4); steep floor pushed into → slide down the slope (`0.1·N` + projected `MaxStepHeight` drop). |
| 3.4 | No walkable floor → falling (a running player always falls off ledges); the steep-slope case that also hits an unwalkable surface reverts the step (TENTATIVE reading of the spec). |
| 3.5 | Slope gravity slide when floor Z < 0.99 and `Z·GroundFriction < 3.3`: `g·dt/(2·max(F, 0.5))·dt` projected on the floor, with the whole call's `dt`, every sub-step. |
| 3.1 (end) | Walking velocity = displacement / `dt` with Z = 0 (long frames under-report speed, e.g. 0.8× for a 0.5 s frame). |
| 4.1 | Gravity = `world_gravity_z × custom_gravity_scaling`. |
| 4.2 | Air-control wall probe (`AirControl > 0.05`); limiter when `limit_fall_accel`: `AccelRate·AirControl`, +`(10 − v)/dt` below 10 uu/s, `BoundSpeed` = current horizontal speed at/above `GroundSpeed`, or 1 uu/s² with air control ≤ 0.05. |
| 4.3, 4.6 | Semi-implicit sub-steps moving with the new velocity, then `V = 2·(displacement/step) − V_old` → effective acceleration doubled (gravity −1040 uu/s² with the config value), then 3-D `TerminalVelocity` clamp. |
| 4.4 | Landing on hit normal Z ≥ `WalkableFloorZ`; landing velocity re-derived when `step·Time > 0.003` and `Time > 0.1`. |
| 4.5, 4.7, 4.8 | Wall/ceiling slides with `CalculateSlopeSlide` (height clamp unless `SlopeBoostFriction = 0`), `TwoWallAdjust`, V-crease ("ditch") landing, no horizontal extrapolation after wall contact. |
| 5.2 | Landing sets the floor, the base, force-floor-check and a unit-length acceleration. |

Algorithm constants are `NativeCode` facts, kept as named constants with their symbol and data address in
`ue3_movement.rs` (0.0003, 8, 0.05, 0.5, 0.03, 2, 100, 1e-8, 1e-4, 0.98, 2.0, 2.4, 1.9, 2.15, 0.1, 0.99, 3.3,
0.5, −0.08, 144, 10, 0.003, 1e-4 (f64), 1.001).

### Parameter mapping (UE3 property → `MovementParams` field)

`ue3_movement::PawnTuning::from_params` is the single place where the port reads parameters. Recovered class
defaults are plugged in by setting the fields below with `Param::set(value, Provenance::ScriptDefault {..})`
(or `Config` / `MeasuredTrace`); nothing else changes. Property names are TENTATIVE (spec section 7).

| UE3 property (offset) | Field | Notes |
|---|---|---|
| `GroundSpeed` (Pawn+0x33C) | `movement.max_ground_speed` | walking cap; air `BoundSpeed` threshold |
| `AccelRate` (Pawn+0x34C) | `movement.ground_acceleration` | ground and air acceleration magnitude |
| `AirControl` (Pawn+0x35C) | `movement.air_control` | |
| `JumpZ` (Pawn+0x350) | `movement.jump_velocity` | script jump stand-in |
| `MaxStepHeight` (Pawn+0x250) | `movement.step_height` | |
| `WalkableFloorZ` (Pawn+0x258) | `movement.walkable_floor_z` | |
| `CollisionRadius` / `CollisionHeight` (cylinder) | `movement.capsule_radius` / `movement.capsule_half_height` | cylinder half-height = UE3 `CollisionHeight` |
| `CustomGravityScaling` (UDKPawn+0x5A4) | `movement.custom_gravity_scaling` | |
| `WorldInfo` gravity (`DefaultGravityZ`, config) | `movement.world_gravity_z` | per-map `GlobalGravityZ`/GravityVolumes not modelled |
| `PhysicsVolume.GroundFriction` (+0x294) | `movement.ground_friction` | one implicit volume |
| `PhysicsVolume.TerminalVelocity` (+0x298) | `movement.terminal_velocity` | |
| Pawn+0x298 bit 51 (`bLimitFallAccel`) | `movement.limit_fall_accel` | default UNKNOWN; decides whether `AirControl` matters |
| `SlopeBoostFriction` (UDKPawn+0x78C) | `movement.slope_boost_friction` | only zero vs non-zero matters (no physical materials) |
| `MovementSpeedModifier` (Pawn+0x364) | `movement.movement_speed_modifier` | |

`movement.gravity_z`, `movement.braking_deceleration` and `movement.max_fall_speed` are read only by the
placeholder model.

### Stand-ins, scope limits and known deviations of the port

- **Script is not modelled.** Stand-ins: script acceleration = `AccelRate · Normal(input)` (TENTATIVE, spec
  9.3); jump = `Velocity.Z = JumpZ` and Physics = Falling, walking only (TENTATIVE, spec 5.1); `MayFall`
  leaves the fall permission set (STRONG for a running player, spec 3.4); `NotifyHitWall` / `HitWall` /
  `NotifyFallingHitWall` / `Landed` / `NotifyJumpApex` are no-ops, so `bJustTeleported` is never set and hit
  notifications never change velocity or mode. ASAMU's grapple/power-jump/rocket-boot script is not ported.
- **World mapping.** Every collision surface is static world geometry of one actor (like BSP): step-up-able,
  a valid base, never forcing floor re-traces, no physical material. One implicit physics volume: no zone
  velocity, no water, no gravity volumes.
- **Not ported:** crouch / walk-slowly and `CheckForLedges` (its output is UNKNOWN in the spec), the
  `processLanded` sanity trace / `FindSpot` / random kick, the UDK stuck-falling nudge (5 s / 10 s), pawn
  rotation and lean, other physics modes.
- **Collision contact.** The original's pull-back inside line checks is UNKNOWN (spec 9.5 q7). The port's
  `MoveActor` pulls the hit time back by `CONTACT_SKIN` (0.05 uu) along the move — a numerical choice, not a
  game value — and inflates the sweep radius by 1.001 (STRONG). Floor probes are exact. Standing height is the
  native 2.15 uu hover above the floor (the placeholder model stands 0.05 uu above it).
- **Interpretations marked TENTATIVE in code:** the old normal passed to `TwoWallAdjust` inside `stepUp` (the
  horizontalised first normal) and the steep-slope "wall also hit" flag that reverts a walking step.
- **Spec gaps resolved by choice (TENTATIVE):** after a V-crease ("ditch") landing whose last move hit nothing,
  the floor normal handed to `processLanded` is the second wall's normal (the spec does not say which hit
  result is used; the next walking sub-step re-traces the floor anyway).
- **Contract-violating collision results** (NaN or out-of-range hit times, non-finite or clearly non-unit
  normals) are repaired and non-finite moves refused, so a broken `CollisionWorld` cannot make the state
  non-finite (robustness only; conforming worlds such as `BoxWorld`/`SlopeWorld` are unaffected bit for bit).
- **Hidden state.** `PlayerState::pawn` (floor normal, based, force-floor-check) is not part of the trace
  schema; a replay started from a recorded sample begins unbased, so its first walking tick snaps the hover to
  2.15 uu.
- **Frame length.** `sim::step`/`step_with` still clamp `dt` to `MAX_STEP_DT` (0.25 s) before the model runs;
  the native 8-sub-step budget drops time only for frames above about 0.4 s (or after several mode changes in
  one frame), which the clamp mostly prevents (call `Ue3PawnMovement::advance_with_stats` directly to study
  long frames).
- **Rounding.** The port uses IEEE `f32` in the spec's operation order; the original's vector normalisation and
  division helpers may round differently in the last bit (UNKNOWN).
- **Grapple bridge (not original).** The placeholder grapple pull is added to the falling acceleration after
  the air limiter (so it is doubled by the refinement like gravity); while walking it lifts the pawn into
  falling when it beats gravity, otherwise its horizontal part is added after `CalcVelocity`.

### Consequences to check against original traces (arithmetic consequences of the spec)

- Effective gravity `2 × world_gravity_z × custom_gravity_scaling` (−1040 uu/s² with the config value); a jump
  of `JumpZ` peaks after `round(JumpZ / (2·|g|·h))` ticks of length `h` at `JumpZ·n·h + g·h²·n²`
  (≈ `JumpZ²/(4·|g|)`), and lands with vertical speed `JumpZ + (2n − 1)·g·h` in tick `n`.
- Walking pawns hover 2.15 uu above the floor; steps up to about `MaxStepHeight + 4.15` uu are climbable.
- Ground speed reaches `GroundSpeed` after `GroundSpeed / (AccelRate·dt)` ticks regardless of analog input;
  landing clamps horizontal speed to `GroundSpeed` on the first walking sub-step.
- Air steering adds `2·AccelRate·AirControl·dt` per tick when the limiter is on (`2·AccelRate·dt` when off).
- Hitting a ceiling while rising refines the vertical velocity from the shortened displacement
  (`2·avg.z − V_old.z`), so the pawn moves *down* at once (e.g. −160 uu/s after rising only 2 uu into a ceiling
  from `Vz = 400` in a 1/60 s tick); horizontal speed is kept.
- After a landing, the rest of the tick walks with the *air-limited* acceleration, only normalised: if the
  air-control wall probe cut air control (wall within one tick of travel) while `10 ≤ speed < GroundSpeed`,
  that acceleration is zero and the landing tick brakes even with input held.
- A landing found only after a slide (e.g. falling along a wall onto the floor) carries no time: the pawn
  rests at the collision contact distance, not at the 2.15 uu hover, until the next tick.
- A walking pawn whose move is exactly zero and whose floor trace finds nothing starts falling only on the
  next tick (`|Delta| = 0` carries no time); a nearly-zero but non-zero move carries exactly one sub-step
  (≤ 0.05 s) into falling.
- With `SlopeBoostFriction = 0`, falling into an unwalkable slope slides *up* it without a height clamp and the
  refinement then launches the pawn upwards ("slope boosting").
- Braking has no guard against `2·GroundFriction·h > 1`: with huge friction the 0.03 s pieces oscillate in
  sign and the average can keep most of the speed (friction 40, 0.06 s frame: 98 %).
- The refinement re-derives velocity from an `f32` displacement, so slow sub-steps far from the origin carry
  the position's rounding (≈ 2·10⁻³ uu/s on a 0.07 uu step at z = 1000).

Tests: `crates/asamu-player/tests/ue3_movement.rs` (closed-form braking curve, acceleration timing, jump apex,
landing tick and impact speed, free fall and sub-step sequences for long frames, dropped time, ignored slices,
hover band and floor-trace skip, step-up limits, multi-stair step-up, walkable ramp, slope-slide thresholds,
landing threshold at `WalkableFloorZ`, V-crease landing and step revert, ledge and landing time carry,
air-control limiter on/off, low-speed boost, `BoundSpeed` creep, wall probe, terminal velocity, bit-identical
runs, fuzzed worlds/parameters/inputs/`dt` without NaN and with exact time accounting) and unit tests in
`ue3_movement.rs`; `asamu-game` tests the model selection.

`crates/asamu-player/tests/ue3_spec_conformance.rs` pins individual spec rules the scenario tests do not
distinguish, each against an expected value derived from the spec's formulas: the refinement condition
(skipped when a falling pawn does not speed up downwards), refinement from displacement at ceilings, walls and
in a vertical corner (`TwoWallAdjust` crease), `TerminalVelocity` applied after the move, every time-carry
branch (slide landing → 0, wall-shortened step over a pit → `step·(1 − frac)`, `|Delta| = 0` → 0, nearly-zero
move → one sub-step, budget shared by walking and falling), the air-clamped acceleration kept through a
landing, `CalcVelocity` once per call with the whole `dt`, `MaxSpeedModifier` on the cap only, the
penetrating floor-trace guard, the steep-floor slide (`0.1·N` + projected drop), the slope slide with the whole
`dt` per sub-step, wall slide and obtuse/right-angle corners inside `stepUp`, slope boosting, gravity scaling,
oscillating braking, snapshot/resume bit-identity and a contract-violating ("garbage") collision world.

### Verification pass (2026-10-09)

An adversarial line-by-line review of `ue3_movement.rs` against `NATIVE_PHYSICS.md` (sections 1.3–1.4, 2.1,
2.3, 3.1–3.5, 4.1–4.8, 5.2, the constants table and every address cited in code) found the port faithful to the
spec. Two defects were fixed: `stepUp` stepped down a second time when its repetition safety bound
(`MAX_STEP_UP_REPEATS`, not a game value) ran out (pathological inputs only), and a contract-violating collision
world could make the state non-finite (NaN hit times/normals were used unchecked). Each new conformance test was
checked against a deliberately injected deviation (22 mutations, e.g. always/never refining, refining from
velocity instead of displacement, slide landings carrying time, `frac` forced to 1, slope slide with the
sub-step `dt`, clamping terminal velocity before the move, landing restoring the unclamped acceleration,
skipping the `stepUp` `TwoWallAdjust` retry); all but one are caught. The exception is the landing-velocity
rule (`step·Time > 0.003` and `Time > 0.1`, spec 4.4): it is implemented but cannot be observed here, because
the port's `MoveActor` places the pawn exactly at `Delta·Time`, so the re-derived landing velocity equals the
semi-implicit one up to rounding.
Still open because they depend on script, not on the native spec: if the controller's apex-notify flag
(Controller+0x270 bit 5) is set when jumping, the sub-step in which `Velocity.Z` reaches ≤ 0 sets
`bJustTeleported` and skips the refinement (spec 4.3/4.6), lowering the apex velocity change of that one
sub-step from `2·g·h` to `g·h`; a `NotifyFallingHitWall` that changes velocity does the same (spec 4.10). The
port models neither (UNKNOWN until the ASAMU/UTGame jump and wall-hit script is read).
