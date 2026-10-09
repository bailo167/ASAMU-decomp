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

## Coordinate conventions (planned, to be verified)

UE3 uses a left-handed, Z-up coordinate system measured in Unreal units (UU). Bevy uses a right-handed, Y-up
system in metres by convention. The conversion lives in `asamu-core` and is applied once in the importer.
The UU→metre scale is a *convention*, not a recovered game fact; gameplay parity is evaluated in UU.

## Behavioural traces (planned)

Trace samples: `time, input, position, velocity, camera (rotation, FOV), grapple anchor, grapple state,
grounded`. Original-game traces (recorded later on Windows) and runtime traces use the same schema so a replay
harness can compare them tick by tick. See [PARITY.md](PARITY.md).
