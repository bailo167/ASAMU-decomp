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

**Every gameplay parameter in the runtime is currently a PLACEHOLDER.** No value below was recovered from
the original game; they were chosen so the graybox is playable and were not deliberately taken from stock
UE3/UDK defaults either. If a value happens to coincide with an engine default, that is not evidence about
ASAMU and must not be cited as such. Engine property names in the notes are *leads* for where to look
(TENTATIVE). Findings in `docs/reverse-engineering/` do not change this table until a value is wired into
the code with its `Provenance`. The table is
generated from `PlayerParams::default().provenance_report()`
(`cargo run -p asamu-player --example provenance_table`); a test
(`crates/asamu-player/tests/docs_sync.rs`) fails if it drifts from the code. When a value is recovered,
its `Provenance` changes (script default / native code / config / measured trace) and the table shows the
source instead.

| Parameter | Value | Unit | Provenance | Note / source |
|---|---|---|---|---|
| `movement.gravity_z` | -1000 | uu/s^2 | placeholder | graybox placeholder; replace with the original's effective gravity (lead: WorldInfo/zone gravity settings, TENTATIVE) or a measured trace |
| `movement.max_ground_speed` | 450 | uu/s | placeholder | graybox placeholder; replace with value recovered from ASAMU pawn defaults (lead: GroundSpeed, TENTATIVE) |
| `movement.ground_acceleration` | 3000 | uu/s^2 | placeholder | graybox placeholder; replace with value recovered from ASAMU pawn defaults (lead: AccelRate, TENTATIVE) |
| `movement.braking_deceleration` | 3000 | uu/s^2 | placeholder | graybox placeholder; replace with the original's ground braking/friction behaviour (native walking physics + pawn defaults, TENTATIVE) |
| `movement.air_control` | 0.25 | fraction | placeholder | graybox placeholder; replace with value recovered from ASAMU pawn defaults (lead: AirControl, TENTATIVE) |
| `movement.jump_velocity` | 450 | uu/s | placeholder | graybox placeholder; replace with value recovered from ASAMU pawn defaults (lead: JumpZ, TENTATIVE) |
| `movement.capsule_radius` | 20 | uu | placeholder | graybox placeholder (human-sized at the presentation scale); replace with the ASAMU pawn collision component radius (lead: CollisionRadius, TENTATIVE) |
| `movement.capsule_half_height` | 45 | uu | placeholder | graybox placeholder (human-sized at the presentation scale); replace with the ASAMU pawn collision component height (lead: CollisionHeight, TENTATIVE) |
| `movement.step_height` | 18 | uu | placeholder | graybox placeholder; replace with value recovered from ASAMU pawn defaults (lead: MaxStepHeight, TENTATIVE) |
| `movement.max_fall_speed` | 4000 | uu/s | placeholder | graybox placeholder; it is UNKNOWN whether the original caps fall speed at all (lead: terminal velocity in physics volumes, TENTATIVE) |
| `movement.walkable_floor_z` | 0.7 | normal z | placeholder | graybox placeholder; replace with the original's walkable-slope threshold (lead: WalkableFloorZ, TENTATIVE) |
| `grapple.max_range` | 2500 | uu | placeholder | graybox placeholder; replace with the grapple range from ASAMU script defaults (class not yet identified) |
| `grapple.pull_acceleration` | 900 | uu/s^2 | placeholder | graybox placeholder; replace with the grapple pull behaviour recovered from ASAMU script (UNKNOWN whether it is an acceleration at all) |
| `grapple.min_rope_length` | 60 | uu | placeholder | graybox placeholder; numerical guard so pull never reaches the anchor; replace once the original's behaviour near the anchor is known |
| `grapple.rope_mode` | inelastic | - | placeholder | graybox placeholder; the original's rope model (fixed length vs reel-in vs spring) is UNKNOWN |
| `grapple.release_mode` | preserve_velocity | - | placeholder | graybox placeholder; whether the original modifies velocity on release is UNKNOWN (verify with traces) |
| `grapple.attached_max_speed` | 2500 | uu/s | placeholder | graybox placeholder; it is UNKNOWN whether the original caps swing speed |
| `camera.fov_degrees` | 90 | deg (horizontal) | placeholder | graybox placeholder; replace with the FOV from ASAMU camera/viewport defaults or config (lead: ASAMU.ASAMUViewportClient, TENTATIVE) |
| `camera.eye_height` | 38 | uu | placeholder | graybox placeholder; replace with value recovered from ASAMU pawn defaults (lead: BaseEyeHeight, TENTATIVE) |
| `camera.max_pitch_degrees` | 89 | deg | placeholder | graybox placeholder; replace with the original's view pitch limits (lead: camera/controller pitch clamp, TENTATIVE) |

The locomotion model itself (`asamu_player::movement::PlaceholderMovement`) is also a placeholder, not a
reimplementation of the original's native physics; see `docs/ARCHITECTURE.md`. So are its behavioural
choices: analog input scales walking speed, the floor supports a grounded player against upward
acceleration weaker than gravity, a taut grapple rope tethers a standing player horizontally, the rope does
not wrap around geometry, and a grapple tap shorter than one tick is not seen (the button is sampled as a
level). Simulation tolerances in code (`CONTACT_SKIN`, `MIN_MOVE`, `ROPE_TAUT_EPSILON`, floor-probe
distance, `MAX_SLIDE_ITERATIONS`, `sim::MAX_STEP_DT`) are numerical implementation details, not gameplay
constants; `CONTACT_SKIN` is nevertheless visible in traces (standing height), so it is a known source of
runtime-vs-original difference.
