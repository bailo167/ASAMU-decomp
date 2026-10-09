# Script analysis — where is `ASAMU.u`?

**Answer (CONFIRMED): the ASAMU script package was cooked into `Startup.upk`.** There is no standalone
`ASAMU.u` because UE3's cooker merged every `[Engine.StartupPackages]` entry into the seek-free
`Startup.upk`. Inside it, `asamu` is a top-level `Package` export that owns all 172 ASAMU script classes.

Evidence below is reproducible with `tools/asamu-inspect` (`objects FILE --top-level`, `exports FILE --json`)
against the user's own install. Only names and counts are published here.

## CONFIRMED

### Config
- `[Engine.ScriptPackages]`: `+NonNativePackages=UTGame`, `UTGameContent`, `ASAMU`; `+NativePackages=UDKBase`.
- `[UnrealEd.EditorEngine]` `+EditPackages`: `UDKBase, UTEditor, UTGame, UTGameContent, ASAMU`.
- `[Engine.StartupPackages]`: `UI_Fonts, UI_Fonts_Final, UI_Fonts_Final_RUS, UI_Fonts_Final_UKR, FX_HitEffects,
  UDKFonts, UTGame, ASAMU, ASAMUFonts`.
- `DefaultGame.ini`: `DefaultGame=ASAMU.ASAMUGameInfo`, `PlayerControllerClassName=ASAMU.ASAMUPlayerController`,
  `DefaultMapPrefixes=(Prefix="AG",…,GameType="ASAMU.ASAMUInfo")`, `[UTGame.UTPawn] Bob=0.010, bWeaponBob=true`.
- `DefaultEngine.ini`: `GameViewportClientClassName=ASAMU.ASAMUViewportClient`.

### File system
- `CookedMac` has `UTGameContent.u` but no `ASAMU.u`, `UTGame.u`, `ASAMUFonts`, `UI_Fonts*`, `FX_HitEffects` or
  `UDKFonts` package files.

### Package structure (`Startup.upk`, parsed by `asamu-ue3`)
- 37,183 exports; **131 top-level exports**: 130 `Package` + 1 `ObjectReferencer` (`ObjectReferencer_0`).
- The top-level packages include **every** StartupPackage: `asamu`, `UTGame`, `asamufonts`, `UI_Fonts`,
  `UI_Fonts_Final`, `FX_HitEffects`, `UDKFonts`, plus content packages such as `GrapplingGun`, `PowerGlove`,
  `rocketBoots`, `checkpoints`, `Platforms`, `PlayerHand`, `AdventureSuitEffects`, `Maddie`, `Village`,
  `Dark_Cave_worm`, `ParadiseCave`, `IceCave_Mechanics`, `StarHaven_Sounds`, `ASAMUHudFlash`, `ASAMUCameraAnimations`.
- `asamu` (name stored lower-case) and `UTGame` are `Package` exports with PackageFlags `0x20200000`
  (ContainsScript | NoExportAllowed, names TENTATIVE) and non-zero GUIDs; their generation net-object counts
  (4,371 and 17,371) equal the number of exports beneath each.
- **Under `asamu`: 4,371 exports** — 922 `Function`, 62 `State`, 172 `Class`, 172 `TextBuffer`, 12 `ScriptStruct`,
  9 `Enum`, 19 `Const`, and 2,800+ properties (832 Object, 456 Str, 324 Bool, 322 Float, 228 Struct, 222 Int,
  100 Array, 92 Name, 55 Component, 48 Byte, 7 Class) plus default subobject components.
- **Under `UTGame`: 17,371 exports**, 411 classes (stock UDK `UTGame`).
- **No map defines a `Class` export**; maps only import ASAMU classes (BeautifulCity 32, Darkcave 42, Epilogue 8,
  IceCave 38, ParadiseCave 35, StarHaven 39, Workshop 20, FrontEndMap 6, Legal 2, TheCore 10, Entry 0,
  Freds_place 0).
- The executable registers natives for package `asamu` (`AutoInitializeRegistrantsASAMU`): one native class,
  `ASAMUSystemSettingsManager` (C++ `UASAMUSystemSettingsManager`). All other ASAMU classes are pure script.

### Script bytecode and source presence
- Every one of the 583 `asamu`/`UTGame` classes has exactly one `TextBuffer` child named `ScriptText`
  (≈2.8 MB in total); the 12 `.u` packages carry 1,938 more. A presence test for `class <Name>` succeeds in 577
  of 583 (case-insensitive). **The UnrealScript source text appears to have shipped inside the cooked
  packages.** It is the original authors' copyrighted source: it may be read locally for behavioural
  understanding but must never be extracted into, quoted in, or committed to this repository.
- Function exports carry serialized bytecode (UStruct script storage) — format introspection pending.

### Localization
- `ASAMU.int` sections name `ASAMU` classes: `ASAMUHUD`, `ASAMUHUDMovie`, `ASAMUHUDMovieTimeTrial`,
  `ASAMUSettingsManager`, `GFxASAMUMainMenu`, `GFxASAMUPauseMenu`, `GFxASAMUPauseMenuTimeTrial`,
  `GFxASAMUWorkshopMonitor`, `GFxASAMUCredits` — all present as classes in `Startup.upk`.

## Class architecture (CONFIRMED from export `SuperIndex` chains)

| Class | Inherits | Role (from names) |
|---|---|---|
| `ASAMUPawn` | `UTGame.UTPawn` → `UDKBase.UDKPawn` | Player pawn — runs native `AUDKPawn`/`APawn` physics (NATIVE_PHYSICS.md) |
| `ASAMUPlayerController` | `UDKBase.UDKPlayerController` | Input/abilities; states `Grappling`, `ReleaseGrapple`, `PlayerFlying` |
| `ASAMUPlayerInput` | `Engine.PlayerInput` | Input processing |
| `GrappleGun` | `UDKBase.UDKWeapon` | **The grapple is a weapon** fired by `StartFire`/`StopFire` |
| `ASAMUPowerJump` | `Engine.Actor` | Charged power jump; states `Unavailable, Ready, Charging, Jumping, Canceled` |
| `ASAMURocketBoots` | `Engine.Actor` | Rocket boost; states `Unavailable, UnavailableAndPlayedSound, Ready, Boosting` |
| `ASAMUCamera` | `Engine.Camera` | Player camera |
| `ASAMUGameInfo` (+ `ASAMUGameInfoTimeTrial`, `ASAMULegalGameInfo`) | `GameFramework.FrameworkGame` | Game rules |
| `ASAMUCheckpoint`, `ASAMUCheckpointVisuals` | `Engine.Actor` | Checkpoints (visual states `Inactive, Activating, Active`) |
| `ASAMUCheckpointManager`, `ASAMUProgressionManager`, `ASAMUGeneralSaveManager` | `Core.Object` | Checkpoint/progression/save logic |
| `ASAMUKillZone`, `ASAMUDynamicKillZone` | `Engine.Volume` / … | Death volumes |
| `ASAMUInventoryManager`, `ASAMUInventory`, `ASAMUTimedPowerup` | `Engine.InventoryManager`/`Inventory` | Inventory/powerups |
| `ASAMUFallingRock`, `ASAMUFallingWhenGrappledRock` (`InterpActor`), `ASAMUFloatingRock`, `ASAMURechargeCrystal` | various | Grapple-reactive world objects (states `isGrappled`, `HasBeenGrappled`, `GrappledState`) |
| `ASAMUVelocityCone` | `Engine.DynamicSMActor` | Speed visual |
| `ASAMUTelePad_Attractor` | `Engine.Actor` | Attractor pad |
| `ASAMUGrappleAbleInterface`, `ASAMUReleaseGrappleInstantInterface`, `ASAMUDoesNotAcceptGrappleDecal` | `Core.Interface` / … | Grapple target rules |
| `ASAMUNPC_*` (Maddie, Villager, Worm) | `ASAMUNPC`/`ASAMUNPC_Pawn` | NPCs (worm has a 9-state AI) |

Superclass census of the 172 classes: 49 `SequenceAction`, 20 `SequenceEvent`, 20 `Object`, 12 `Actor`,
7 `GFxMoviePlayer`, 5 `Interface`, 4 `DynamicSMActor`, 4 `InterpActor`, 3 `DynamicTriggerVolume`, …

### Gameplay-relevant functions (names only)

- `GrappleGun` (54): `StartFire`, `StopFire`, `FireAmmunition`, `ProcessInstantHit`, `CheckForRange`, `Grapple`,
  `UpdateGrapple`, `ResetGrapple`, `ReleaseGrappleButton`, `ReleaseInstantTimed`, `SetMaxGrapples`,
  `ResetGrappleAmount`, `UnlimitedGrapples`, `EnableGrapple`, `Tick`, `PlayerJumped`, `PlayerLanded`,
  `PowerJumped`, `PowerLeaped`, `JustStartedFalling`, `Falling`, `TriggerPlayerGrappledEvent`,
  `TriggerPlayerReleasedGrappleEvent`, beam/visual helpers, and `ToggleGoatMode`/`ToggleMidasMode` extras.
- `ASAMUPawn` (59 functions+states): `DoJump`, `ReleaseJump`, `Landed`, `HardLanding`, `TakeFallingDamage`,
  `PowerJumpKeyDown/Up`, `TryToStartSprinting`, `TryToStopSprinting`, `ApplySprintSpeed`, `RemoveSprintSpeed`,
  `ApplyStorySpeed`, `RemoveStorySpeed`, `EnableGrapple`, `EnableRocketBoots`, `UpdateEyeHeight`, `WeaponBob`,
  `Died`, `ResetPlayer`, `QuickLoad`, `TimeTrialRestart`, `SetFallingState`, `SetFlyingState`; states `Idle,
  IdleIdle, Running, Shooting, Jumped, ReleasedJump, Release, FallingState, HasLanded, StoryState, Zooming`.
- `ASAMUPlayerController` (34): `StartFire`, `ReleaseGrapple`, `RocketBoostKeyDown`, `PowerJumpKeyDown/Up`,
  `StartSprinting`, `StopSprinting`, `Use`, `GetPlayerViewPoint`, `StartCameraShake`, states `Grappling`,
  `ReleaseGrapple`, `PlayerFlying`.
- Kismet: 49 `SeqAct_*` (e.g. `SetMaxGrapples`, `ToggleGrapple`, `ToggleRocketBoots`, `TriggerCheckpoint`,
  `PlayerDied`, `SetPawnSize`, `NarratorLine`, `StartTimeTrial`) and 20 `SeqEvent_*` (e.g. `PlayerGrappled`,
  `PlayerReleasedGrapple`, `PlayerJumped`, `PlayerLanded`, `PowerJumped`, `PlayerRocketBoosted`, `PlayerDied`,
  `CollectibleCollected`).

## STRONG

- The grapple applies forces through `ASAMUPlayerController`'s `Grappling` state on top of native pawn physics
  (states `Grappling`/`PlayerFlying` + the pawn's `SetFlyingState`) — mechanism to be read from bytecode/defaults.
- The number of grapples is limited and refilled (`SetMaxGrapples`, `ResetGrappleAmount`, `UnlimitedGrapples`,
  `ASAMURechargeCrystal`).

## UNKNOWN / next

- Class default property values (pawn speeds, `JumpZ`, `AirControl`, grapple range/forces, power-jump/rocket-boot
  tuning) — requires decoding class default objects (tagged properties).
- Grapple, power-jump and rocket-boost algorithms — requires bytecode introspection (and local reading).
- `UTGame` overrides relevant to movement (`UTPawn` jump/dodge/double-jump logic that ASAMU inherits).
