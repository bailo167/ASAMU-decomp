# Cross-correlation: native symbols ↔ package objects ↔ config ↔ localization

Goal: recover ASAMU's architecture quickly by lining up independent evidence sources for each game class:

1. **Native symbols** in the unstripped Mac executable (`tools/asamu-symbols`, SYMBOL_ANALYSIS.md)
2. **Package exports** in the cooked UE3 packages (`tools/asamu-inspect`, PACKAGE_ANALYSIS.md, SCRIPT_ANALYSIS.md)
3. **Config references** (`ASAMU/Config/*.ini`)
4. **Localization sections** (`ASAMU/Localization/INT/*.int`)

Confidence: CONFIRMED / STRONG / TENTATIVE / UNKNOWN (see CLAUDE.md).

## Game classes

| Game class | Native symbol evidence | Package evidence | Config evidence | Localization evidence | Likely responsibility | Confidence |
|---|---|---|---|---|---|---|
| `ASAMU.ASAMUViewportClient` | none found yet | pending (expected in `Startup.upk`) | `[Engine.Engine] GameViewportClientClassName` | — | Game viewport client (UI/menus/loading glue) | CONFIRMED (name, config) |
| `UASAMUSystemSettingsManager` | full native class: `Get/Set{Int,Float,Bool}Setting`, `SetLanguage`, `Get/SetTextureDetail`, `GetAvailableResolutions`, `SaveSystemSettings`, `ResetLastSavedSettings`, `Init` + `exec*` thunks; natives table `GasamuUASAMUSystemSettingsManagerNatives` | pending | — | — | Native settings backend for the options menu | CONFIRMED (symbols) |
| `ASAMUSettingsManager` | none | pending | — | `[ASAMUSettingsManager]` in `ASAMU.int` | Script-side settings/options logic (localized strings) | CONFIRMED (name) |
| `ASAMUHUD`, `ASAMUHUDMovie`, `ASAMUHUDMovieTimeTrial` | none | pending | — | sections in `ASAMU.int` | HUD (Scaleform movie) incl. time-trial HUD | CONFIRMED (names) |
| `GFxASAMUMainMenu`, `GFxASAMUPauseMenu`, `GFxASAMUPauseMenuTimeTrial`, `GFxASAMUCredits`, `GFxASAMUWorkshopMonitor` | none | pending (Scaleform movies in `ASAMUFrontEndFlash.upk`?) | — | sections in `ASAMU.int` | Scaleform menus | CONFIRMED (names) |
| Player controller (grapple/abilities) | no ASAMU natives; exec names from input config: `PowerJumpKeyDown/Up`, `RocketBoostKeyDown`, `ReleaseJump`, `StartSprinting/StopSprinting`, `QuickLoad`, `TimeTrialRestart`, `SmartJump` | pending | `DefaultInput.ini` bindings | — | Input → abilities (power jump, rocket boost, sprint), grapple via `StartFire/StopFire` (TENTATIVE) | STRONG (exec names exist in config) |
| Player pawn | stock `APawn`/`AUDKPawn` physics natives only | pending | — | — | Movement via native pawn physics with script defaults | TENTATIVE |
| `UTGame.UTConsole`, `UTGame.UTScout` | no `UT*` natives (UTGame is NonNative) | pending (no `UTGame.u` on disk) | `[Engine.Engine] ConsoleClassName`, `ScoutClassName` | — | Stock UDK console / path scout | CONFIRMED (config) |
| `FX_HitEffects.UTPostProcess_PC` | — | pending | `DefaultPostProcessName` | — | Default post-process chain | CONFIRMED (config) |

## Packages

| Package | On disk | Config | Native registrants | Notes |
|---|---|---|---|---|
| `ASAMU` | **no `ASAMU.u`** | NonNativePackages, EditPackages, StartupPackages | `AutoInitializeRegistrantsASAMU`, `AutoGenerateNamesASAMU` | Script package; native part = settings manager only |
| `UTGame` | **no `UTGame.u`** | NonNativePackages, EditPackages, StartupPackages | none | Stock UDK game script |
| `UTGameContent` | `UTGameContent.u` (uncompressed) | NonNativePackages, EditPackages | none | Stock UDK content script |
| `UDKBase` | `UDKBase.u` | NativePackages, EditPackages | pending (symbols agent) | Native UDK layer (AUDKPawn etc.) |
| `ASAMUFonts`, `UI_Fonts*`, `FX_HitEffects`, `UDKFonts` | no standalone files | StartupPackages | — | Expected inside `Startup.upk` |

_To be completed from `asamu-inspect` export tables and `asamu-symbols` registration data._
