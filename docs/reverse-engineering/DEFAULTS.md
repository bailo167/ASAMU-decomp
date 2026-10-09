# Gameplay defaults, config overrides and native field layout

Scope: the real starting values of every gameplay parameter of the player pawn, controller, input, camera, grapple
gun, power jump, rocket boots and the level actors, with inheritance resolved, `.ini` overrides applied and the source
of each value recorded. Also the native x86_64 field layout of the engine classes, computed from the script property
layout and checked against the executable. These values feed `asamu-player` as `Provenance::ScriptDefault` /
`Provenance::Config` parameters.

Evidence source: the legitimately owned Mac install (Steam build 1822049), read only. The packages are decoded by our
own `crates/asamu-ue3` (see [`OBJECT_FORMAT.md`](OBJECT_FORMAT.md)). No payload bytes, script source or decompiled code
appear here or in the data files: only names, types, numbers, offsets and counts.

Behaviour (what the script does with these values at run time) is specified in [`ABILITIES.md`](ABILITIES.md) and
[`GRAPPLE.md`](GRAPPLE.md). The native physics that reads the pawn fields is in [`NATIVE_PHYSICS.md`](NATIVE_PHYSICS.md).

## Reproduce

```sh
export CARGO_TARGET_DIR=target/wf2-defaults
# 1. Values, config, layout, map census. Skips cleanly if the install is absent (set ASAMU_ORIGINAL_DIR).
cargo run --release -p asamu-inspect --example gameplay_defaults -- --out docs/reverse-engineering/data/defaults \
    --native-sizes research/local/defaults/gpsc.asm
# 2. Native sizeof values (local, git-ignored). The example parses this text; nothing from it is committed
#    except the sizes. B = the Mac executable Contents/MacOS/ASAMU of the install.
nm "$B" | awk '$3 ~ /GetPrivateStaticClass/ {print $3}' > research/local/defaults/gpsc-syms.txt
objdump -d --no-show-raw-insn --disassemble-symbols=$(paste -sd, research/local/defaults/gpsc-syms.txt) "$B" \
    > research/local/defaults/gpsc.asm
# 3. What each layout rule decides (prints match counts, writes nothing):
cargo run --release -p asamu-inspect --example gameplay_defaults -- --ablate no-bool-merge \
    --native-sizes research/local/defaults/gpsc.asm
```

Run step 2 before step 1. Without `--native-sizes`, step 1 leaves `native_class_sizes.json` as it is. The output is
byte-for-byte identical across runs.

## Most important findings

| # | Finding | Confidence |
|---|---|---|
| 1 | **The script property layout gives the native field offsets.** Laying out each class from its script properties with the UE3 link rules for a 64-bit build (section 2) gives: every one of the 105 named native offsets cited in `NATIVE_PHYSICS.md` (104 distinct fields) (Actor, Pawn, UDKPawn, Controller, PhysicsVolume, WorldInfo, components); **and** a class size equal to the native `sizeof` for all 1,447 native classes that have both a script definition and a registration function in the executable (0 differences). Each layout rule decides real cases: removing any one breaks 6 to 1,447 class sizes. | CONFIRMED |
| 2 | **The flag at Pawn+0x298 bit 51 is `bLimitFallAccel`** (the 32-bit word at Pawn+0x29C, bit 19). Its default is **true**, set by the `Engine.Pawn` default object. No script class assigns it. The native air-control clamp is therefore always active for the player. This resolves NATIVE_PHYSICS.md open question 1. | CONFIRMED (layout, cdo); "never assigned" STRONG (text scan) |
| 3 | Player pawn defaults: `GroundSpeed` 440, `AirSpeed` 440, `AccelRate` 2048, `JumpZ` 1000, `AirControl` 0.3, `DefaultAirControl` 0.35, `MaxStepHeight` 26, `WalkableFloorZ` 0.78, `MaxFallSpeed` 2500, `CustomGravityScaling` 1.0, eye height 38. Collision cylinder: radius 21, half-height 44. The full table is in section 4. | CONFIRMED (cdo) |
| 4 | Config only overrides two pawn values, and both equal the CDO snapshot: `Bob` 0.01 and `bWeaponBob` true (`[UTGame.UTPawn]`, `DefaultGame.ini`). The world uses `DefaultGravityZ` −520 and `RBPhysicsGravityScaling` 2.0 (`[Engine.WorldInfo]`, `DefaultGame.ini`). Input uses `MouseSensitivity` 30, `LookRightScale` 300 and `LookUpScale` −250 (`DefaultInput.ini`). The player's FOV setting is 90 (`DefaultSettings.ini`). | STRONG (UE3 LoadConfig rule); values CONFIRMED |
| 5 | 14 native fields that `NATIVE_PHYSICS.md` left unnamed now have script names (section 3.3). Examples: Pawn+0x500 = `FailedLandingCount`, UDKPawn+0x788 = `StartedFallingTime`, Pawn bit 59 = `bNeedsBaseTickedFirst`. The names fit the uses the native code makes of the fields; for `bCollideAsEncroacher` the fit is weakest. | CONFIRMED offsets; meaning STRONG |
| 6 | Grapple gun: `fMaxDistance` 5000, `fGrappleReleaseDistance` 200, `fGrappleAccel` 2000, `fGrappleMaxSpeed` 10 000, trace `WeaponRange` 16 384, `FireInterval` [0.1], instant-hit fire type. `iMaxGrapples` defaults to 0, and Kismet sets it per level. | CONFIRMED (cdo) |
| 7 | Map placements override only a few properties per class (section 7). Examples: every `ASAMUTelePad_Attractor` uses `Range` 1000, `Strength` 200 and `velocityBaseAmount` 0.05 instead of the class defaults 500 / 500 / 0. The 134 `ASAMUFallingRock`s use 4 fall distances and 3 fall rates. | CONFIRMED (map decode) |
| 8 | The `AG` map-prefix game type in `DefaultGame.ini`, `ASAMU.ASAMUInfo`, names a class that does not exist: no name table of any package contains `ASAMUInfo`. `DefaultPawnClass` is `None` in every default object of the `ASAMUGameInfo` chain; `ASAMUGameInfo`'s own script chooses the pawn class. | CONFIRMED (name tables, cdo); fallback path TENTATIVE |

## 1. How values are resolved

### 1.1 Class default objects — CONFIRMED

- Values come from the class default objects (`Default__<Class>`), merged root first along the super chain by
  `PackageSet::inherited_defaults`. A child's value overrides its parent's; tagged structs merge member by member;
  arrays and binary structs replace wholesale (OBJECT_FORMAT.md, "Property order").
- `source` in the data files:
  - `cdo`: the class's own default object stores the value;
  - `inherited`: an ancestor's default object stores it, and `value_from` names that class;
  - `zero`: no default object in the chain stores it, so the value is 0 / false / `None` / empty / the first
    enumerator. STRONG: UE3 writes every value that differs from the parent's defaults, and a property declared in a
    class has no parent default;
  - `not_serialized`: a `CPF_Native` property, which is never written. Its value cannot be recovered from packages
    (UNKNOWN);
  - `config`: an `.ini` value overrides the CDO (1.2). The CDO value is kept as `cdo_value` when it differs.
- **No script source holds class defaults.** A local scan of all 2,151 locally extracted class sources (`asamu`,
  `UTGame`, `Engine`, `GameFramework`, `UDKBase`) found 0 `defaultproperties` blocks; cooking strips them
  (OBJECT_FORMAT.md). So the default objects are the only source for these values, and the requested comparison with
  shipped `defaultproperties` cannot be made. Section 5 lists the checks done instead.
- **Component templates** (for example the pawn's `CollisionCylinder`) are merged along their archetype chain: the
  template under each class's default object, back to the root template. A root template has a null archetype, which
  means the class defaults of the component class. The data lists the values set by a template in that chain, plus the
  cylinder's shape for cylinder components.
- Floats are written as the shortest decimal that parses back to the same `f32`, so `asamu-core`'s exact-`f32` helpers
  recover the bit pattern.
- **Provenance mapping for `asamu-player`.** A value with source `cdo` or `inherited` maps to
  `Provenance::ScriptDefault { class: value_from, property: name }`. A value with source `config` maps to
  `Provenance::Config { file, key: "[section] key" }`.

### 1.2 Config overrides — STRONG

- **File chains**, following the shipped `[Configuration] BasedOn=` lines (the `..\` paths are relative to the binaries
  folder):
  - `Engine/Config/Base<Name>.ini` → `ASAMU/Config/Default<Name>.ini` → `Engine/Config/Mac/Mac<Name>.ini` →
    `ASAMU/Config/Mac/Mac<Name>.ini`. The Mac files of `Game` and `Input` add nothing.
  - `Settings` and `Controller` have no base file: just `DefaultSettings.ini` and `DefaultController.ini`.
- **Merge rules** (UE3): a plain key in a later file replaces the earlier values; repeated plain keys in one file form
  an array; `+` adds a value if it is absent, `.` always adds, `-` removes, `!` clears.
- **Which section applies** (UE3 `LoadConfig`):
  - A `config` property is read from the section of the class being loaded. A class's default object starts as a copy
    of its parent's, which already holds the parent's config values. So the effective value is the key in the most
    derived class's section that has one, unless a more derived default object stores its own value.
  - A `globalconfig` property is read only from the declaring class's section, in the declaring class's config file.
- This is the UE3 convention and agrees with the 490 equal pairs in OBJECT_FORMAT.md's config comparison. The native
  `LoadConfig` was not read, so this is STRONG, not CONFIRMED.
- **Not included:** the per-user files the game writes at run time. They can hold player changes (for example the FOV
  slider).

### 1.3 Data files (`docs/reverse-engineering/data/defaults/`)

| File | Content |
|---|---|
| `<Class>.json` (31 classes) | Every property declared by the class itself, any value: all types for `asamu` classes, scalars, vectors and rotators for engine classes. Every inherited property with a non-zero value, a config value, or a name in the movement-physics focus list. Fields: `name`, `type`, `declared_in`, `flags` (Config, GlobalConfig, Native, Transient, Localized, Const, Edit, Net, EditConst, Deprecated), `native_offset`/`native_bit` (native classes only), `value`, `source`, `value_from`, `config`, `cdo_value`, `confidence`. Also the merged component templates, the config files read, and config keys that name no property. |
| `native_layout.json` | Own fields of 27 engine classes (Object, Actor, Pawn, GamePawn, UDKPawn, Controller, PlayerController, GamePlayerController, UDKPlayerController, Brush, Volume, PhysicsVolume, GravityVolume, Info, ZoneInfo, WorldInfo, Component, ActorComponent, PrimitiveComponent, CylinderComponent, Camera, Inventory, Weapon, UDKWeapon, Interaction, Input, PlayerInput). One row per field: `[offset, name, kind, size, bit, array_dim]`. |
| `native_layout_check.json` | The 119 native offsets cited in `NATIVE_PHYSICS.md`, with the script property found at each. |
| `native_class_sizes.json` | Summary of the 1,447-class size comparison, and the `sizeof` of the classes in the gameplay chains. |
| `map_instances.json` | Per placed gameplay class and `WorldInfo`: instance counts per map, and the scalar properties overridden by placements (instance count, distinct values, min/max, most common values). |

Strings are emitted only when short (≤ 48 characters) and never when localized. Arrays longer than 24 elements and
structs with more than 24 members are summarized. Object references are object paths (names).

## 2. Native layout rules — CONFIRMED

The engine computes each class's in-memory layout by linking its script properties in declaration order after the
parent's. On the Mac x86_64 build this reproduces the C++ layout exactly with the rules below. Each rule's column
counts how many checks fail when only that rule is removed (`--ablate`); 105 named offsets and 1,447 class sizes are
checked.

| Rule | Named offsets wrong without it | Class sizes wrong without it |
|---|---:|---:|
| Element sizes and alignments: byte 1/1; int, float and bool 4/4; name 8/4; object, class and component pointers 8/8; string and dynamic array 16/8; interface 16/8; delegate 16/8 (object pointer + name). Map properties take the layout of `Core.Object.Map_Mirror` (80 bytes). Struct properties take the struct's padded size and alignment. | — | — |
| Pointer-sized mirrors: `Core.Object.Pointer` is 8 bytes, aligned 8 (the script declares a single `int`); `QWord` and `Double` are 8/8 (ablation `ptr32` makes pointers 4 bytes) | 105 | 1,447 |
| Consecutive bool properties of one class or struct share a 32-bit word, bit 0 first, up to 32 bits. A non-bool property, a static array or the start of a new class begins a new word. | 98 | 1,073 |
| A class continues at its parent's **unpadded** end. Example: `sizeof(PrimitiveComponent)` is 0x240, but `CylinderComponent`'s first field is at 0x238. This is the Itanium C++ ABI's reuse of a non-POD base's tail padding. Script structs are POD and continue after the padded parent. | 16 | 172 |
| `Matrix`, `Plane`, `Quat`, `Vector4`, `SHVector` and `SHVectorRGB` are aligned to 16 bytes | 5 | 140 |
| `Color` (4 bytes) is aligned to 4 | 0 | 6 |
| Struct size is padded to the struct's alignment; a class's `sizeof` is its end padded to its alignment | — | — |

What each rule's evidence covers:

- **16-byte alignment:** only the `Matrix`/`Plane` part is exercised by the named offsets (`PrimitiveComponent`,
  `SkeletalMeshComponent`). `Vector4` and `SHVector` are exercised by the class sizes (`MaterialExpressionFunctionInput`,
  `SphericalHarmonicLightComponent`). `Quat` follows the UE3 convention, and no check isolates it (TENTATIVE).
- **Native sizes:** each `<Class>::GetPrivateStaticClass<Class>` passes the class size as the third argument of
  `UClass::UClass(EStaticConstructor, …)` (an immediate loaded into `%edx`).
  - 1,528 of the 1,535 registration functions have that call.
  - The other 7 are the intrinsic Core classes (`Field`, `Struct`, `ScriptStruct`, `State`, `Class`, `Function`,
    `Const`), which build the `UClass` inline and have no script layout.
  - 205 native script classes have no registration function in the shipping executable (187 `UnrealEd`, 9 `Engine`,
    4 `GFxUIEditor`, 3 `WinDrv`, 2 `UTEditor`), so they could not be compared. All are editor or other-platform code.
- **Correction to `NATIVE_PHYSICS.md` §7.2:** the first `Pawn` field is `VfTable_IInterface_Speaker` (8-byte interface
  vtable pointer) at +0x248, not a field at +0x250. `sizeof(AActor)` is 0x248. `MaxStepHeight` is at +0x250 as stated.

## 3. Native field names

### 3.1 Confirmed hypotheses

All 105 named entries (104 distinct fields; `bPreciseDestination` is cited twice) of the `NATIVE_PHYSICS.md`
field-offset table and prose land on the property of the hypothesized name (`native_layout_check.json`). This includes every TENTATIVE name there, for example `MaxJumpHeight`,
`LadderSpeed`, `bCanJump` (bit 10), `bForceMaxAccel` (bit 50), `bLimitFallAccel` (bit 51), `bForceRMVelocity`,
`bForceRegularVelocity`, `MultiJumpBoost` (UDKPawn+0x5A0), `OldZ` (+0x61C), `SlopeBoostFriction` (+0x78C),
`MaxLeanRoll` (+0x798), the Controller notify bits and the PhysicsVolume fields. Their names are now CONFIRMED by
layout. `DesiredRotation` covers +0x4AC..+0x4B7, so the +0x4B4 read is its `Roll`.

### 3.2 The 64-bit bit numbers

`NATIVE_PHYSICS.md` numbers bits of 64-bit loads. The layout works in 32-bit words: bit *n* of the 64-bit word at
offset *o* is bit *n* mod 32 of the word at *o* + 4·⌊*n*/32⌋.

| Native citation | 32-bit word, bit | Property | Default for the player |
|---|---|---|---|
| Pawn+0x298 bit 51 | +0x29C bit 19 | `bLimitFallAccel` | true (`Engine.Pawn`) |
| Pawn+0x298 bit 50 | +0x29C bit 18 | `bForceMaxAccel` | false |
| Pawn+0x298 bit 49 | +0x29C bit 17 | `bRunPhysicsWithNoController` | true (`UTGame.UTPawn`) |
| Pawn+0x298 bit 53 / 54 | +0x29C bit 21 / 22 | `bForceRMVelocity` / `bForceRegularVelocity` | false / false |
| Pawn+0x298 bit 20 | +0x298 bit 20 | `bSimulateGravity` | true |
| Actor+0xE8 bit 17 | +0xE8 bit 17 | `bCanStepUpOn` | true |

### 3.3 Fields that were unnamed — offsets CONFIRMED, meaning STRONG

| Native citation (`NATIVE_PHYSICS.md`) | Use in native code | Script property |
|---|---|---|
| Actor+0xE8 bit 59 (= +0xEC bit 27) | clear on the base → force a floor check every step | `bCollideActors` |
| Actor+0xF0 bit 1 | adds trace flag 0x100000 to the walking floor trace | `bMoveIgnoresDestruction` |
| Actor+0xF0 bit 4 | part of the walking base-change test | `bCollideAsEncroacher` |
| Pawn+0x298 bit 41 (= +0x29C bit 9) | keep pitch/roll toward the desired rotation | `bRollToDesired` |
| Pawn+0x298 bit 59 (= +0x29C bit 27) | tick the base first | `bNeedsBaseTickedFirst` |
| Pawn+0x500 | processLanded rejection counter | `FailedLandingCount` |
| UDKPawn+0x590 bit 1 | high-jump flag (SetHighJumpFlag) | `bRequiresDoubleJump` |
| UDKPawn+0x590 bit 2 | can-double-jump (SuggestJumpVelocity) | `bCanDoubleJump` |
| UDKPawn+0x590 bit 6 | notify stopped falling (setPhysics) | `bNotifyStopFalling` |
| UDKPawn+0x590 bit 15 | update eye height (TickSpecial) | `bUpdateEyeheight` |
| UDKPawn+0x788 | time used by the stuck-while-falling check | `StartedFallingTime` |
| PlayerController+0x4C0 | rotation used for a viewed pawn | `BlendedTargetViewRotation` |
| Camera+0x570 | active camera animations | `ActiveAnims` |
| SkeletalMeshComponent+0x6B0 | root-motion input | `RootMotionDelta.Translation` (`RootMotionDelta` is a 16-aligned `BoneAtom` at +0x6A0) |

## 4. Player pawn — `asamu.ASAMUPawn`

Chain: `ASAMUPawn` → `UTGame.UTPawn` → `UDKBase.UDKPawn` → `GameFramework.GamePawn` → `Engine.Pawn` → `Engine.Actor`
→ `Core.Object`. Values are the starting values. "Writers" lists the script classes that assign the property directly.
This comes from a local regular-expression scan of the extracted sources (names only recorded, STRONG). It does not
see native setters such as `SetCollisionSize`. `ABILITIES.md` and `GRAPPLE.md` describe what those writes do.

### 4.1 Movement physics inputs

| Property | Value | Declared in | Native offset | Value from | Writers (asamu / UTGame / engine) |
|---|---|---|---|---|---|
| `GroundSpeed` | 440 | Pawn | 0x33C | UTPawn (Pawn 600) | ASAMUPawn, ASAMUPlayerController, GameInfo |
| `AirSpeed` | 440 | Pawn | 0x344 | UTPawn | GrappleGun, ASAMUPlayerController, GameInfo |
| `WaterSpeed` | 220 | Pawn | 0x340 | UTPawn | GameInfo |
| `LadderSpeed` | 200 | Pawn | 0x348 | Pawn | — |
| `AccelRate` | 2048 | Pawn | 0x34C | Pawn | GameInfo |
| `AirControl` | 0.3 | Pawn | 0x35C | ASAMUPawn (UTPawn 0.35, Pawn 0.05) | ASAMUPawn, UTPawn, GameInfo |
| `DefaultAirControl` | 0.35 | UTPawn | — (script class) | UTPawn | none |
| `JumpZ` | 1000 | Pawn | 0x350 | ASAMUPawn (UTPawn 322, Pawn 420) | ASAMUPowerJump, ASAMUPlayerController, GameInfo |
| `MaxStepHeight` | 26 | Pawn | 0x250 | UTPawn (Pawn 35) | none |
| `MaxJumpHeight` | 49 | Pawn | 0x254 | UTPawn | — |
| `WalkableFloorZ` | 0.78 | Pawn | 0x258 | UTPawn (Pawn 0.7) | none |
| `LedgeCheckThreshold` | 4 | Pawn | 0x25C | Pawn | none |
| `MaxFallSpeed` | 2500 | Pawn | 0x36C | ASAMUPawn (UTPawn 1250, Pawn 1200) | none in ASAMU |
| `CustomGravityScaling` | 1.0 | UDKPawn | 0x5A4 | UDKPawn | UTPawn, UDKPawn only (none in ASAMU) |
| `BaseEyeHeight` / `EyeHeight` | 38 / 38 | Pawn | 0x374 / 0x378 | UTPawn (Pawn `BaseEyeHeight` 64) | `EyeHeight`: ASAMUPawn, UTPawn, Pawn, PlayerController |
| `bLimitFallAccel` | true | Pawn | 0x29C bit 19 | Pawn | none |
| `bForceMaxAccel` | false | Pawn | 0x29C bit 18 | zero | PlayerController, SavedMove |
| `bSimulateGravity` | true | Pawn | 0x298 bit 20 | Pawn | — |
| `bCanJump` / `bJumpCapable` / `bCanWalk` | true / true / true | Pawn | 0x298 bits 10 / 9 / 11 | Pawn | `bCanJump`: UTBot only |
| `bCanFly` | true | Pawn | 0x298 bit 13 | **ASAMUPawn** (its own CDO sets it) | — |
| `bCanCrouch` / `bCanSwim` / `bCanClimbLadders` / `bCanStrafe` | true | Pawn | 0x298 bits 6 / 12 / 14 / 15 | UTPawn | — |
| `bCanWalkOffLedges` / `bAvoidLedges` / `bStopAtLedges` | false | Pawn | 0x298 bits 22 / 16 / 17 | zero | — |
| `bAllowLedgeOverhang` | true | Pawn | 0x298 bit 18 | Pawn | — |
| `bRunPhysicsWithNoController` | true | Pawn | 0x29C bit 17 | UTPawn | — |
| `bForceRMVelocity` / `bForceRegularVelocity` | false / false | Pawn | 0x29C bits 21 / 22 | zero | — |
| `WalkingPct` / `CrouchedPct` / `MovementSpeedModifier` | 0.4 / 0.4 / 1.0 | Pawn | 0x360 / 0x368 / 0x364 | UTPawn / UTPawn / Pawn | none |
| `CrouchHeight` / `CrouchRadius` | 29 / 21 | Pawn | 0x2AC / 0x2B0 | UTPawn | — |
| `DesiredSpeed` / `MaxDesiredSpeed` | 1.0 / 1.0 | Pawn | 0x2D0 / 0x2D4 | Pawn | `MaxDesiredSpeed`: UTBot |
| `AvgPhysicsTime` | 0.1 | Pawn | 0x2E8 | Pawn | — |
| `Mass` / `Buoyancy` | 100 / 0.99 | Pawn | 0x2EC / 0x2F0 | Pawn / UTPawn | — |
| `OutofWaterZ` / `MaxOutOfWaterStepHeight` | 420 / 40 | Pawn | 0x354 / 0x358 | Pawn | — |
| `SlopeBoostFriction` | 0.2 | UDKPawn | 0x78C | UTPawn | none |
| `MaxMultiJump` / `MultiJumpRemaining` / `MultiJumpBoost` | 1 / 1 / −45 | UDKPawn | 0x59C / 0x598 / 0x5A0 | UTPawn | `MultiJumpRemaining`: ASAMUPawn, UTPawn |
| `MaxDoubleJumpHeight` / `DoubleJumpEyeHeight` | 87 / 43 | UDKPawn / UTPawn | 0x594 / — | UTPawn | — |
| `bCanDoubleJump` / `bRequiresDoubleJump` / `bNoJumpAdjust` | true / false / false | UDKPawn | 0x590 bits 2 / 1 / 3 | UTPawn / zero / zero | — |
| `DodgeSpeed` / `DodgeSpeedZ` | 600 / 295 | UTPawn | — | UTPawn | none |
| `RotationRate` | (Pitch 20000, Yaw 20000, Roll 20000) | Actor | 0x1FC | Pawn | — |
| `ViewPitchMin` / `ViewPitchMax` | −18000 / 18000 | Pawn | 0x4A0 / 0x4A4 | UTPawn | none |
| `MaxLeanRoll` | 2048 | UDKPawn | 0x798 | UTPawn | none |
| `WalkingPhysics` / `LandMovementState` | `PHYS_Walking` / `PlayerWalking` | Pawn | 0x2A0 / 0x430 | Pawn | — |
| `bCollideActors` / `bCollideWorld` / `bBlockActors` | true | Actor | 0xEC bits 27 / 28 / 30 | Pawn | — |
| `Bob` / `bWeaponBob` | 0.01 / true | UTPawn | — | **config** `DefaultGame.ini` `[UTGame.UTPawn]` (globalconfig; equal to the CDO) | `Bob`: ASAMUPawn, UTPawn |
| Collision cylinder `CollisionRadius` / `CollisionHeight` | 21 / 44 | CylinderComponent | 0x23C / 0x238 | template `UTGame.Default__UTPawn.CollisionCylinder` (the ASAMU template sets only `ReplacementPrimitive`) | none (size changes go through native `SetCollisionSize`, e.g. `SeqAct_SetPawnSize`) |

All values are CONFIRMED (cdo) except the `zero` rows (STRONG) and the config rows (STRONG for the override rule;
the values are CONFIRMED). "—" in the writers column means the property was not part of the scan; "none" means the
scan found no direct assignment.

### 4.2 ASAMU's own pawn properties

`MoveSpeed` 440, `bSprintSpeedMultiplier` 2.0, `storyModeSpeedMultiplier` 0.6, `fTerminalVelocity` 10 000,
`fHardLandingVelocity` 2000, `hardLandingThreshold` 2000, `normalLandSoundVelocityThreshold` 500,
`fLongJumpVerticalSpeed` 700, `fLongJumpHorizontalSpeed` 3000, `jumpVelocityLowerMultiplier` 0.7, `zoomFOV` 50,
`zoomDuration` 0.3, `bZoomEnabled` true, `checkFallingSoundDelay` 0.1, `playerDiedFadeDownTime` 0.3,
`playerDiedFadeUpTime` 0.3, bob speed multipliers 0.55 (story), 0.85 (sprint) and 0.65 (walking), `hardLandDecalSize`
200, `hardLandDecalDepth` 100. The scan found no script that assigns `MoveSpeed`, the two speed multipliers or
`fTerminalVelocity`; they are read only. Every other own property, 57 entries including the run-time state flags that
start false, is in `ASAMUPawn.json`. CONFIRMED (cdo).

## 5. Cross-checks

- **Against the native executable:** sections 2 and 3, 105 offsets plus 1,447 class sizes, CONFIRMED. This also
  confirms the property names that `NATIVE_PHYSICS.md` used for the physics constants' inputs.
- **Against the two behaviour specifications**, which were written independently from the same CDOs and the script
  text. Every value in `ABILITIES.md` §1 and the gun constants in `GRAPPLE.md` equal the values here. That includes
  440 / 2048 / 1000 / 0.3 / 0.35 / 2500 / 21 / 44 / 38 / 26 / 0.78 / 49 / 4 / 1.0 / 0.2 / ±18000 / 2048, the ASAMU
  multipliers, and 5000 / 200 / 2000 / 10 000 / 16 384 / 0.1 / 0.05.
- **Against config**, where both exist: `DefaultGravityZ` −520 and `RBPhysicsGravityScaling` 2.0 equal the WorldInfo
  CDO, and `Bob` / `bWeaponBob` equal the UTPawn CDO. The CDO is a cook-time snapshot of these keys.
- **Against shipped `defaultproperties`:** not possible. 0 of 2,151 sources contain one (1.1).
- **Struct defaults:** the 394-of-416 agreement with `structdefaultproperties` is reported in OBJECT_FORMAT.md. No
  struct default is a gameplay parameter here.

## 6. Other classes

### 6.1 Controller, input, camera, FOV

| Class | Property | Value | Native offset | Source |
|---|---|---|---|---|
| `ASAMUPlayerController` | `RotationRate` | (30000, 30000, 2048) | Actor 0x1FC | CDO `Engine.Controller` |
| | `FOVAngle` / `DesiredFOV` / `DefaultFOV` | 85 / 85 / 85 | 0x4A0 / 0x4A4 / 0x4A8 | CDO `Engine.PlayerController` |
| | `MaxResponseTime` | 0.125 | 0x464 | CDO `Engine.PlayerController` |
| | `InteractDistance` | 512 | 0x650 | config `BaseGame.ini` `[Engine.PlayerController]` |
| | `MinHitWall` | −1.0 | 0x278 | CDO `Engine.Controller` |
| | `CameraClass` / `InputClass` | `asamu.ASAMUCamera` / `asamu.ASAMUPlayerInput` | 0x458 / 0x590 | CDO `asamu.ASAMUPlayerController` |
| `ASAMUCamera` | `DefaultFOV` | 90 | 0x258 | CDO `Engine.Camera` |
| | `DefaultAspectRatio` / `FreeCamDistance` | 1.33333 / 256 | 0x268 / 0x4F0 | CDO `Engine.Camera` |
| `ASAMUPlayerInput` | `MouseSensitivity` | 30 | 0x194 | config `DefaultInput.ini` `[Engine.PlayerInput]` |
| | `LookRightScale` / `LookUpScale` | 300 / −250 | 0x2A0 / 0x2A4 | same |
| | `MoveForwardSpeed` / `MoveStrafeSpeed` | 1200 / 1200 | 0x298 / 0x29C | same |
| | `DoubleClickTime` / `bEnableMouseSmoothing` | 0.25 / true | 0x190 / 0x180 bit 11 | same |
| | `MouseSamples` / `MouseSamplingTotal` | 1 / 0.0083 | 0x2BC / 0x2C0 | CDO `Engine.PlayerInput` |
| | `ControllerSensitivity` | 1.0 | — | CDO `asamu.ASAMUPlayerInput`; `ASAMUControllerInput.ControllerSensitivity` 1.0 from `DefaultController.ini` |
| `ASAMUSettingsManager` | `FOV` / `HandScale` / `Gamma` | 90 / 1.0 / 0.5 | — | config `DefaultSettings.ini` |
| | `SpeedlinesActive` / `RumbleActive` | true / true | — | same |
| | `GoatModeActive` / `MidasModeActive` / `ParkourModeActive` / `BeamColorActive` | false | — | same |

The controller and camera FOV defaults (85, 90) are engine defaults. ABILITIES.md describes how the settings FOV
(90) is applied at run time.

### 6.2 Grapple gun and weapon chain (`GrappleGun` → `UDKWeapon` → `Weapon` → `Inventory` → `Actor`)

| Property | Value | Native offset | Source |
|---|---|---|---|
| `fMaxDistance` / `fGrappleReleaseDistance` | 5000 / 200 | — | CDO `GrappleGun` |
| `fGrappleAccel` / `fGrappleMaxSpeed` | 2000 / 10 000 | — | CDO `GrappleGun` |
| `TOP_GRAPPLE_ANGLE` / `BOTTOM_GRAPPLE_ANGLE` | 0.8 / 0.8 | — | CDO `GrappleGun` |
| `instantReleaseDelay` / `fastFallRateDelay` / `interactRange` | 0.05 / 4.0 / 200 | — | CDO `GrappleGun` |
| `BobDamping` / `JumpDamping` | 0.15 / 1.0 | — | CDO `GrappleGun` |
| `bCanGrapple` / `iMaxGrapples` | true / 0 | — | CDO / zero (Kismet `SeqAct_SetMaxGrapples.Grapples`, default 0) |
| `fHitDecalLimit` / `fDecalLifespan` / `DecalWidth` / `DecalHeight` | 5 / 20 / 80 / 80 | — | CDO `GrappleGun` |
| `FireInterval` / `Spread` / `WeaponFireTypes` / `FiringStatesArray` | [0.1] / [0.0] / [`EWFT_InstantHit`] / [`WeaponFiring`] | Weapon 0x2F8 / 0x308 / 0x2D8 / 0x2C8 | CDO `GrappleGun` |
| `WeaponRange` | 16 384 | Weapon 0x360 | CDO `Engine.Weapon` |
| `EquipTime` / `PutDownTime` | 0.33 / 0.33 | Weapon 0x348 / 0x34C | CDO `Engine.Weapon` |
| `Priority` | −1.0 | Weapon 0x374 | config `BaseGame.ini` `[Engine.Weapon]` |

### 6.3 Power jump, rocket boots, timed power-up

| Class | Properties (CDO, CONFIRMED) |
|---|---|
| `ASAMUPowerJump` | `powerJumpChargeTime` 0.6, `powerJumpStrength` 1600, `powerLeapHorizontalStrengthMultiplier` 2.0, `powerLeapVerticalStrength` 750 |
| `ASAMURocketBoots` | `boostDelay` 1.0, `boostDuration` 2.0, `boostStrength` 2500, `boostSpiralStrength` 1000, `totalSpinAngle` 720, `boostExhaustedDelay` 1.0 |
| `ASAMUTimedPowerup` | `TimeRemaining` 10, `TransitionDuration` 0.5, `WarningTime` 3.0 (no instance in any shipped map) |

### 6.4 World, physics volume, gravity

| Class | Property | Value | Native offset | Source |
|---|---|---|---|---|
| `Engine.WorldInfo` | `DefaultGravityZ` | −520 | 0x600 | config `DefaultGame.ini` `[Engine.WorldInfo]` (equal to the CDO) |
| | `RBPhysicsGravityScaling` | 2.0 | 0x608 | same |
| | `GlobalGravityZ` / `WorldGravityZ` | 0 / 0 | 0x604 / 0x5FC | zero (run-time) |
| | `KillZ` | −262 143 | ZoneInfo 0x248 | CDO `Engine.ZoneInfo` (maps override, section 7) |
| | `TimeDilation` / `MaxPhysicsDeltaTime` / `MaxPhysicsSubsteps` | 1.0 / 0.33333334 / 5 | 0x530 / 0x7C4 / 0x7C8 | CDO / CDO / config `BaseGame.ini` |
| | `StallZ` | 1 000 000 | 0x5F8 | CDO |
| `Engine.PhysicsVolume` (and `DefaultPhysicsVolume`) | `GroundFriction` | 8.0 | 0x294 | CDO `Engine.PhysicsVolume` |
| | `TerminalVelocity` | 4000 | 0x298 | same (ASAMUPawn writes 10 000 at run time, ABILITIES.md) |
| | `FluidFriction` | 0.3 | 0x2AC | same |
| | `ZoneVelocity` / `bWaterVolume` / `bVelocityAffectsWalking` | 0 / false / true | 0x284 / 0x290 bit 12 / 0x290 bit 0 | zero / zero / CDO |

### 6.5 Level actors (class defaults)

| Class (parent) | Properties (CDO unless marked zero) |
|---|---|
| `ASAMUCheckpoint` (Actor) | `bEnabled` true, `bRotatePlayerToSpawnPointRot` true, `bOffsetLocalSpace` true, `bTriggeredFromKismet` false (zero), `checkpointIndex` 0 (zero), `spawnPointOffset` 0 (zero), `bCollideActors` true |
| `ASAMUKillZone` (Volume), `ASAMUDynamicKillZone` (DynamicTriggerVolume) | no own properties; `bCollideActors` true from Volume |
| `ASAMUVelocityCone` (DynamicSMActor) | `fadeMinVelocity` 1200, `fadeMaxVelocity` 5000, material parameter `Velocity` |
| `ASAMUTelePad_Attractor` (Actor) | `attractDuration` 10, `Range` 500, `Strength` 500, `velocityBaseAmount` 0 (zero) |
| `ASAMUFallingRock` (InterpActor) | `fallingLowerRate` 9.82, `fallingHigherRate` 9.82, `fallDistance` 1000, `AccelRate` 700, `decelRate` 1000, `updateRate` 0.017, `rotationLowerRate` (−10000, −10000, −10000), `rotationHigherRate` (10000, 10000, 10000), `RotationAccelRate` 1.0, `respawnAtStart` true, `bShouldRotate` false (zero) |
| `ASAMUFallingWhenGrappledRock` (InterpActor) | `fallingLowerRate` 9.82, `fallingHigherRate` 9.82, `fallDistance` 1000, `updateRate` 0.017, `particleOffset` (0, 0, 100), `reachMaxSpeedTime` 0 (zero) |
| `ASAMUFloatingRock` (DynamicSMActor) | `floatRange` 200, `floatRate` 2.0, `updateRate` 0.032, `restrictX/Y/Z` false (zero); no instance in any shipped map |
| `ASAMURechargeCrystal` (InterpActor) | `RechargeDelay` 10, `bShouldRecharge` true, `bParentCrystal` false (zero), `FadeTime` 0 (zero) |
| `ASAMUGlowFlower` (InterpActor) | `FadeTime` 1.0, `glowDuration` 10, `particleScaleMultiplier` 3.0 |
| `SeqAct_SetMaxGrapples` | `Grapples` 0 (zero; each Kismet node sets its own) |

## 7. Map placements — CONFIRMED (map decode)

From `map_instances.json`: map-placed instances store only deltas against the class defaults. Transforms and editor
fields are left out of the summary.

| Class | Instances (maps) | Overridden by placements |
|---|---|---|
| `WorldInfo` | 12 (one per map) | `KillZ` in 8 maps: 1.0 ×3, −1e7 ×2, −1e10 ×2, −1e9 ×1; the other 4 keep −262 143. Gravity is never overridden. |
| `ASAMUCheckpoint` | 109 (ParadiseCave 24, StarHaven 25, IceCave 28, Darkcave 17, BeautifulCity 12, Workshop 1, Epilogue 1, FrontEnd 1) | `checkpointIndex` on 101 (1..27); `spawnPointOffset` on 97 (most often (−300, 0, 0), 74 times; 10 distinct values); `bTriggeredFromKismet` true on 8 |
| `ASAMUKillZone` | 91 (IceCave 33, ParadiseCave 26, StarHaven 13, Darkcave 12, BeautifulCity 7) | nothing |
| `ASAMUDynamicKillZone` | 3 (IceCave) | nothing |
| `ASAMUFallingRock` | 134 (IceCave) | `fallDistance` on all (21 000 / 32 000 / 34 000 / 40 000); `fallingHigherRate` (1500 / 2160 / 6000) and `fallingLowerRate` (470 / 2189) on all; `bShouldRotate` true on 92; rotation rates on 98; `respawnAtStart` false on 35 |
| `ASAMUFallingWhenGrappledRock` | 32 (IceCave) | `fallDistance` 100 000 (31) or 15 000 (1); `fallingHigherRate` 3000, `fallingLowerRate` 1500, `reachMaxSpeedTime` 1.0 on all |
| `ASAMURechargeCrystal` | 105 (IceCave 98, StarHaven 7) | `bShouldRecharge` false on 6; `bParentCrystal` true on 2 |
| `ASAMUGlowFlower` | 15 (Darkcave) | nothing |
| `ASAMUTelePad_Attractor` | 3 (Workshop, FrontEnd, TheCore) | `Range` 1000, `Strength` 200, `velocityBaseAmount` 0.05 on all |

`ASAMUVelocityCone`, `ASAMUFloatingRock`, `ASAMUTimedPowerup` and `ASAMUFallingRockManager` have no placed instance in
any shipped map (export census; spawned at run time or unused).

## 8. Unknown / not done

- The native `LoadConfig` was not read; the section rule in 1.2 is the UE3 convention (STRONG). Per-user config files
  written by the game at run time are not modelled.
- `not_serialized` (`CPF_Native`) properties have no recoverable default.
- Map instances are summarized for scalar values only. Struct deltas of map instances are not merged onto their
  archetype, and Kismet-driven values (for example `iMaxGrapples` per level) are in the Kismet work, not here.
- The script writers scan sees only direct assignments in source text. It does not see writes through native functions,
  `out` parameters or bytecode-only paths.
- `Quat`'s 16-byte alignment is not isolated by any check (TENTATIVE). The other layout rules are each decisive.
- `NATIVE_PHYSICS.md` §7 should adopt the names in section 3; that file was outside this work's scope.
