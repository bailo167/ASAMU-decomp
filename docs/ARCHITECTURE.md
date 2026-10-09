# Architecture

ASAMU-decomp is an **engine recreation** (universal-modder Pattern 4). The original game is used only as
read-only evidence and as a behavioural oracle; our runtime is new code.

```
legitimate original install (read-only)
        │
        ├── static RE of the unstripped Mac Mach-O (symbols, Ghidra)      → docs/reverse-engineering
        ├── UE3 package parsing (crates/asamu-ue3, tools/asamu-inspect)    → structure + sanitized metadata
        └── behavioural observation (later: traces from the Windows game)  → parity tests
        │
        ▼
importer / converter (runs on the user's machine; tools/asamu-import, planned)
        │
        ▼
user-local converted data (never committed, never distributed)
        │
        ▼
native Rust / Bevy runtime (apps/asamu + crates)
```

## Crates

| Crate | Responsibility | Depends on |
|---|---|---|
| `asamu-core` | Shared math (glam), units, coordinate conventions, config types | — |
| `asamu-player` | Deterministic, render-free movement / grapple / camera simulation | core |
| `asamu-world` | Levels, triggers, checkpoints, moving platforms (runtime form) | core |
| `asamu-assets` | Runtime asset representation (converted meshes/textures/levels) | core |
| `asamu-ue3` | Defensive UE3 v868 package reader: summary, names, imports, exports, compression | — |
| `asamu-game` | High-level game state (chapters, progression) | core, player, world |
| `apps/asamu` | Bevy executable: windowing, input, rendering, glue systems | core, game, player, bevy |

Tools: `asamu-locate` (Steam discovery), `asamu-inventory` (sanitized install inventory), `asamu-inspect`
(package/map inspection), `asamu-symbols` (symbol statistics), `progress-gen`, `repo-hygiene`.

## Boundaries

1. **Importer ↔ runtime.** Only the importer knows about UE3 serialization or Steam paths. The runtime reads
   converted, open/runtime-friendly formats (e.g. glTF/PNG/our own RON or binary level format). The renderer is
   not taught UE3 formats.
2. **Simulation ↔ rendering.** `asamu-player` and `asamu-world` expose pure step functions on plain data at a
   fixed timestep. Bevy systems call into them. This makes trajectory traces reproducible and testable in CI.
3. **Evidence ↔ implementation.** Every gameplay constant in the runtime cites its source (script default
   property, native code, config, or a measured trace). Placeholders are explicitly marked as such.

## Coordinate conventions

Implemented in `crates/asamu-core` (`units`, `coords`, `rotator`, `clock`); tested there.

| System | Handedness | Forward | Right | Up | Unit | Used by |
|---|---|---|---|---|---|---|
| UE3 | left-handed | +X | +Y | +Z | Unreal units (UU) | simulation, traces, importer, level data |
| Bevy | right-handed | −Z | +X | +Y | presentation scale | rendering only (`apps/asamu`) |

- **The simulation runs in UE3 axes and UU.** Recovered constants (script defaults, native code, config,
  traces) can then be used verbatim and parity is evaluated in UU without rescaling.
- **UE3 → Bevy:** `bevy = (ue.y, ue.z, −ue.x) · scale` (`coords::ue_pos_to_bevy`; inverse
  `bevy_pos_to_ue`). The linear part is orthogonal with determinant **−1** (it flips handedness, as a
  left- to right-handed change must), so cross products change sign (`M a × M b = −M (a × b)`): normals
  derived from cross products and triangle winding need care in the importer. Extents/sizes convert
  without sign (`ue_extents_to_bevy`). Applied once, at the render boundary (and later in the importer).
- **Scale:** `units::PRESENTATION_UU_PER_METRE = 50` (1 UU = 2 cm) is a commonly quoted UE3-era
  **convention used only for presentation** (Bevy rendering, HUD m/s). It is **not** a recovered ASAMU fact
  and nothing in the simulation depends on it.
- **Rotations:** UE3 rotators use 65536 units per turn (engine convention; `rotator` module, with UE3
  `NormalizeAxis` semantics). The simulation stores yaw/pitch in radians; view direction is
  `(cos p · cos y, cos p · sin y, sin p)` (+yaw turns right, +pitch looks up), and the Bevy camera rotation
  is `Ry(−yaw) · Rx(pitch)` (`coords::ue_view_to_bevy_rotation`). The axis/rotator conventions are standard
  UE3 knowledge; that ASAMU's data uses them unchanged is **TENTATIVE** until checked against parsed map
  data and recorded traces. FOV parameters are treated as horizontal (UE3 convention, **TENTATIVE** for
  ASAMU) and converted to Bevy's vertical FOV using the window aspect.
- **Time:** the simulation uses a fixed step (`clock::FixedClock`, default 60 Hz). This is a **runtime
  choice**: the original's tick model is **UNKNOWN** (UE3 advances actors with a variable `DeltaTime`), to be
  revisited against traces. `sim::step` clamps a single `dt` to `sim::MAX_STEP_DT` (a numerical safety
  bound, not a gameplay value) and treats a non-finite or non-positive `dt` as a no-op.
- **Determinism:** the simulation uses only IEEE-754 basic arithmetic, `sqrt`, exact operations and
  `det_math::sin_cos` (pure Rust; never the platform `sin`/`cos`, whose last bit can differ between
  OS maths libraries). Rust does not contract `a * b + c` into FMA and the supported targets have no
  extended-precision float arithmetic, so runs are expected to be bit-identical across Windows, Linux and
  macOS. `det_math` is pinned by golden bit patterns; whole-run identity across platforms is **TENTATIVE**
  until the same trace has been produced on each OS. Render-side helpers (`ue_view_to_bevy_rotation`, the
  app's FOV conversion) may use platform maths; nothing flows from them back into the simulation.

## Behavioural traces

Implemented in `crates/asamu-player/src/trace.rs`; the format (JSON Lines: one `TraceMeta` line, then one
`TraceSample` per tick with input, position, velocity, yaw/pitch, FOV, grapple state/anchor/rope length,
grounded) and the comparison metrics are specified in [PARITY.md](PARITY.md#trace-format). Original-game
traces (recorded later on Windows) and runtime traces share the schema.

- `trace::record_run` replays a sequence of inputs through the deterministic `sim::step` from an initial
  state and records a runtime trace; `asamu_game::Game` can record live sessions (the graybox app toggles
  this with F9 and writes JSONL to `$ASAMU_TRACE_DIR` or the temp dir).
- `trace::compare` aligns two traces by tick and reports per-field max/mean/RMS error and the first
  divergence above a tolerance. Tests prove runtime traces are bit-reproducible and round-trip losslessly.
- **Simulation boundary for parity.** `sim::step` is pure (no globals, RNG, wall clock, hash ordering or
  hidden state; a test interleaves two simulations and compares bytes with isolated runs). Hostile input is
  contained: inputs are sanitized, and a step that would produce a non-finite state leaves the state
  unchanged and reports `StepEvents::non_finite_rejected`.
  Locomotion sits behind `movement::MovementModel`; today's `PlaceholderMovement` is our own documented
  placeholder model. The original's player movement is expected to be stock UE3 native Pawn physics
  (`physWalking` / `physFalling` / `CalcVelocity`) driven by ASAMU script parameters, with the grapple in
  UnrealScript — **TENTATIVE** (see `docs/reverse-engineering/`). A faithful reimplementation will be a new
  `MovementModel` (and, once the script is understood, a new grapple module), validated by replaying
  original traces' inputs and comparing with `trace::compare`.
