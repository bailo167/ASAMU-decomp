# Symbol analysis

The executable keeps its full symbol table (see BINARY_ANALYSIS.md). This file records **sanitized statistics and
selected names** — never the full dump. Full dumps live in the ignored `research/symbol-dumps/`.

## CONFIRMED (manual `nm`, to be replaced by `tools/asamu-symbols` output)

- 53 symbol lines contain `ASAMU`. They all belong to:
  - `UASAMUSystemSettingsManager` — native class with `Get/Set{Int,Float,Bool}Setting`, `SetLanguage`,
    `Get/SetTextureDetail`, `GetAvailableResolutions`, `SaveSystemSettings`, `ResetLastSavedSettings`, `Init`,
    and matching `exec*` script thunks; vtable `__ZTV27UASAMUSystemSettingsManager`.
  - Package registration: `AutoInitializeRegistrantsASAMU(int&)`, `AutoGenerateNamesASAMU()`,
    natives table `GasamuUASAMUSystemSettingsManagerNatives`.
  - Compilation units `ASAMU.cpp`, `ASAMUSystemSettingsManager.cpp`.
- **Zero** symbols contain `grapple` (case-insensitive).

## STRONG

- The ASAMU package's native surface is essentially the settings manager. Movement, grapple and game flow are
  therefore expected in UnrealScript (or inherited from UDK/UTGame native classes), not in ASAMU-specific C++.

## Pending

- Full category census (UE3 Core/Engine, UDK/UTGame, GFx/Scaleform, PhysX, OpenAL, SDL, Steam, STL/runtime,
  Objective-C) via `tools/asamu-symbols`.
- Search list: Grapple, Hook, Tether, Player, Pawn, Controller, Movement, Velocity, Jump, Swing, Rope, Target,
  Checkpoint, Respawn, Death, Camera, FOV, Input, Save, Progress, Collectible, Story, Sequence, Kismet, Matinee.
