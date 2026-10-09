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
| `ASAMU.ASAMUGameInfo` | none | `Startup.upk` `asamu.ASAMUGameInfo` → `GameFramework.FrameworkGame` | `DefaultGame.ini` `DefaultGame`, `DefaultServerGame`, `DefaultGameType` | — | Game rules / mode | CONFIRMED (config) |
| `ASAMU.ASAMUInfo` | none | **not found** among the 172 `asamu` classes (config may reference a missing/renamed class) | `DefaultMapPrefixes=(Prefix="AG",GameType="ASAMU.ASAMUInfo")` | — | Game type used by `AG-*` maps | CONFIRMED (config) |
| `ASAMU.ASAMUPlayerController` | none (controller natives are stock `APlayerController`/`AUDKPlayerController`) | `asamu.ASAMUPlayerController` → `UDKBase.UDKPlayerController`; states `Grappling`, `ReleaseGrapple`, `PlayerFlying` | `DefaultGame.ini` `PlayerControllerClassName` | — | Player input, abilities, grapple (TENTATIVE) | CONFIRMED (config) |
| `UTGame.UTPawn` | stock natives via `AUDKPawn` | `UTGame.UTPawn` → `UDKBase.UDKPawn`; base of `asamu.ASAMUPawn` (CONFIRMED) | `DefaultGame.ini` `[UTGame.UTPawn] Bob=0.010, bWeaponBob=true` | — | Base of the ASAMU player pawn | CONFIRMED |
| `ASAMU.ASAMUViewportClient` | none found yet | `asamu.ASAMUViewportClient` → `UTGame.UTGameViewportClient` | `[Engine.Engine] GameViewportClientClassName` | — | Game viewport client (UI/menus/loading glue) | CONFIRMED (name, config) |
| `UASAMUSystemSettingsManager` (script name `ASAMUSystemSettingsManager`) | full native class: `Get/Set{Int,Float,Bool}Setting`, `SetLanguage`, `Get/SetTextureDetail`, `GetAvailableResolutions`, `SaveSystemSettings`, `ResetLastSavedSettings`, `Init` + `exec*` thunks; natives table `GasamuUASAMUSystemSettingsManagerNatives` | `asamu.ASAMUSystemSettingsManager` class export (the only native class in `asamu`) | — | — | Native settings backend for the options menu | CONFIRMED (symbols) |
| `ASAMUSettingsManager` | none | `asamu.ASAMUSettingsManager` class export | — | `[ASAMUSettingsManager]` in `ASAMU.int` | Script-side settings/options logic (localized strings) | CONFIRMED (name) |
| `ASAMUHUD`, `ASAMUHUDMovie`, `ASAMUHUDMovieTimeTrial` | none | class exports (`ASAMUHUD` → `UDKBase.UDKHUD`) | — | sections in `ASAMU.int` | HUD (Scaleform movie) incl. time-trial HUD | CONFIRMED (names) |
| `GFxASAMUMainMenu`, `GFxASAMUPauseMenu`, `GFxASAMUPauseMenuTimeTrial`, `GFxASAMUCredits`, `GFxASAMUWorkshopMonitor` | none | class exports (`GFxUI.GFxMoviePlayer` subclasses); movies in `ASAMUFrontEndFlash.upk` / `Startup.upk` `ASAMUHudFlash` | — | sections in `ASAMU.int` | Scaleform menus | CONFIRMED (names) |
| Player controller (grapple/abilities) | no ASAMU natives; exec names from input config: `PowerJumpKeyDown/Up`, `RocketBoostKeyDown`, `ReleaseJump`, `StartSprinting/StopSprinting`, `QuickLoad`, `TimeTrialRestart`, `SmartJump` | `ASAMUPlayerController`: `StartFire`, `ReleaseGrapple`, `RocketBoostKeyDown`, `PowerJumpKeyDown/Up`, `StartSprinting/StopSprinting`, `Use`; `ASAMUPawn`: `ReleaseJump`, `QuickLoad`, `TimeTrialRestart`; `GrappleGun` (`UDKWeapon`): `StartFire/StopFire` | `DefaultInput.ini` bindings | — | Input → abilities; the grapple is a weapon fired via `StartFire/StopFire` | CONFIRMED (names in config and package) |
| `ASAMUPawn` | stock `APawn`/`AUDKPawn` physics natives only | `asamu.ASAMUPawn` → `UTGame.UTPawn` → `UDKBase.UDKPawn`; `DoJump`, `ReleaseJump`, sprint/story speed functions | `[UTGame.UTPawn]` | — | Movement via native `AUDKPawn` physics with script defaults | CONFIRMED (chain) |
| `UTGame.UTConsole`, `UTGame.UTScout` | no `UT*` natives (UTGame is NonNative) | `Startup.upk` → `UTGame` (411 classes) | `[Engine.Engine] ConsoleClassName`, `ScoutClassName` | — | Stock UDK console / path scout | CONFIRMED (config) |
| `FX_HitEffects.UTPostProcess_PC` | — | `Startup.upk` → top-level `FX_HitEffects` package | `DefaultPostProcessName` | — | Default post-process chain | CONFIRMED (config) |

## Packages

| Package | On disk | Config | Native registrants | Notes |
|---|---|---|---|---|
| `ASAMU` | **no `ASAMU.u`** — merged into `Startup.upk` (`asamu`, 4,371 exports, 172 classes) | NonNativePackages, EditPackages, StartupPackages | `AutoInitializeRegistrantsASAMU`, `AutoGenerateNamesASAMU` | Script package; native part = settings manager only |
| `UTGame` | **no `UTGame.u`** — merged into `Startup.upk` (17,371 exports, 411 classes) | NonNativePackages, EditPackages, StartupPackages | none | Stock UDK game script |
| `UTGameContent` | `UTGameContent.u` (uncompressed) | NonNativePackages, EditPackages | none | Stock UDK content script |
| `UDKBase` | `UDKBase.u` | NativePackages, EditPackages | registers natives: 100 native classes / 79 exec thunks | Native UDK layer (AUDKPawn etc.) |
| `ASAMUFonts`, `UI_Fonts*`, `FX_HitEffects`, `UDKFonts` | no standalone files | StartupPackages | — | Top-level packages inside `Startup.upk` (CONFIRMED) |

Additional grapple-related classes: `GrappleGun` (`UDKWeapon`), `GrappleGunHitLocActor`, `GrappleGunLightManager`, `ASAMUGrappleAbleInterface`, `ASAMUReleaseGrappleInstantInterface`, `ASAMUDoesNotAcceptGrappleDecal`, `ASAMUFallingWhenGrappledRock`, `ASAMURechargeCrystal`, `SeqAct_SetMaxGrapples`, `SeqAct_ToggleGrapple`, `SeqEvent_PlayerGrappled`, `SeqEvent_PlayerReleasedGrapple`. None has native code.
