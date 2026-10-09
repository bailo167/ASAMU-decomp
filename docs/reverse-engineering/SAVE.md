# Save system, settings and progression

Scope: where the original game keeps saves and settings on macOS and Windows, the four save files and the
classes that own them, the on-disk container format (`Engine.BasicSaveObject` + AES + tagged properties +
embedded JSON), when each file is read and written, the progression model (chapters, unlocks, collectibles,
achievements, time trial) and a design for our compatible save system.

**Evidence and publication rules.** Behaviour comes from local reading of the shipped `ScriptText` of the
`asamu` package (never reproduced or paraphrased line by line here), class default objects and map exports
decoded by `asamu-inspect` (`defaults`, `class`, `objects`, `props`), the shipped config files, Steam's own
metadata on this machine, and native code of the unstripped Mac executable read with Ghidra headless
(decompiled output stays in the ignored `research/local/save/`). Labels: CONFIRMED / STRONG / TENTATIVE /
UNKNOWN as defined in `CLAUDE.md`; the source kind is given in brackets: (src) script source reading,
(cdo) class default object, (map) map export decode, (native) Mac executable, (config) shipped ini,
(steam) Steam metadata. No save file of the original game exists on this machine (section 1.5), so the
byte layout in section 3 is derived from the code that writes it and has **not** been checked against a
real file.

**Verification pass (2026-10-09).** An independent re-derivation (fresh `ScriptText` extraction, fresh
Ghidra decompile, `nm`/`objdump`, map/CDO decoding, Steam metadata) re-checked 33 claims of this document;
section 12 lists them. Corrections made in that pass: float text precision (3.5), the content of
`latestSaveMap` (4.1), quirk Q3 (refuted as stated), the `AutoSaveDir` ini section (1.1), the meaning of
the Steam cloud bookkeeping (1.5); upgrades: "New Game" scope, Q4, and the inertness of the `SaveLevel*`
console commands (6.1).

## Most important findings

| # | Finding | Confidence |
|---|---|---|
| 1 | Four save files, all in one folder `ASAMU/Saves/` below the per-user game directory: `SaveGame.bin` (world/checkpoint snapshot), `PlayerProgression.bin` (unlocks, collectibles, achievements, finished flag), `GeneralSave.bin` (current chapter pointer + Kismet integer flags), `TTS.bin` (time-trial best times). | CONFIRMED (cdo, src) |
| 2 | macOS per-user directory: `~/Library/Application Support/A Story About My Uncle/` (Application Support + `MyDocumentsSubDirName`). Every file write under the install root is redirected there; reads try it first and fall back to the install. Saves therefore land in `~/Library/Application Support/A Story About My Uncle/ASAMU/Saves/`. | CONFIRMED (native) redirection rule; STRONG final path |
| 3 | Windows: `Documents\My Games\A Story About My Uncle\ASAMU\Saves\*.bin`; Steam Auto-Cloud syncs exactly that folder (`*.bin`, root `WinMyDocuments`) **on Windows only**. | CONFIRMED (steam, config) |
| 4 | Container: optional 4-byte magic `0xC0DEDBAD` (bytes `AD DB DE C0`), then AES-256-ECB (32-byte ASCII key compiled into the executable, zero padding to 16) over `i32 version` + one UObject serialized as tagged properties with names and object references written as strings. `SaveGame.bin`, `PlayerProgression.bin`, `GeneralSave.bin` are encrypted; `TTS.bin` is plain. Versions must match exactly: 3, 3, 1, 3. | CONFIRMED (native, src) |
| 5 | Most real data is JSON text stored inside string properties (UE3 `JsonObject`: numbers/bools as unquoted values, case-insensitive keys). The world snapshot is an array of JSON strings: checkpoint table, grapple/boots state, savable actors, **every Kismet event and variable of the map**, every Matinee. | CONFIRMED (src) |
| 6 | Story chain (Kismet `open` commands): Workshop → ParadiseCave ("Sanctuary") → BeautifulCity ("Village") → Darkcave → StarHaven → IceCave (+ streamed TheCore) → Epilogue → main menu. The level enum and `LevelFileNames` agree. This corrects LEVELS.md's TENTATIVE name-based order (ParadiseCave is second, StarHaven precedes IceCave). | CONFIRMED (map, cdo, src) |
| 7 | Saving happens on: entering a story level (progression + general), reaching a new latest checkpoint (snapshot), picking up a new collectible / optional story item / achievement (progression), Kismet flag writes (general), Epilogue (finished flag), finishing a time trial (TTS). No manual save; no save on death. Time-trial mode never writes the snapshot or the chapter pointer. | CONFIRMED (src, map) |
| 8 | 25 collectibles (5 in each of ParadiseCave, BeautifulCity, Darkcave, StarHaven, IceCave) unlock extras at 10/15/20/25 (beam colour, goat, Midas, parkour). Time trial unlocks when the game is finished; 5 levels with gold/silver/bronze targets. 15 achievements (Steam ID = enum index + 1). | CONFIRMED (cdo, map, src) |
| 9 | "New Game" resets only the snapshot and the chapter pointer. Unlocked chapters, collectibles, achievements, the finished flag, Kismet flags, time-trial times and settings survive. | CONFIRMED (src; upgraded from STRONG in the verification pass) |

## 1. Where the files live

### 1.1 Config keys (shipped `ASAMU/Config/DefaultEngine.ini`) [CONFIRMED (config)]

| Section / key | Value | Used for ASAMU saves? |
|---|---|---|
| `[Core.System] SavePath` | `..\..\ASAMU\Save` | **No.** ASAMU script hard-codes its own folder `..\..\ASAMU\Saves\` (note the plural) in `ASAMUGameInfo.saveGameDirectory` (cdo) and once more literally in the main menu's time-trial loader. STRONG (cdo, src) |
| `[Core.System] ScreenShotPath`, `CachePath`; `[UnrealEd.EditorEngine] AutoSaveDir` | `..\..\ASAMU\ScreenShots`, `…\Cache`; `..\..\ASAMU\Autosaves` | editor/engine only (`AutoSaveDir` sits in the editor section, not `[Core.System]` — corrected in the verification pass) |
| `[Windows.StandardUser] MyDocumentsSubDirName` | `A Story About My Uncle` | **Yes**, on both platforms (1.2, 1.3) |
| `[OnlineSubsystemGameSpy.OnlineSubsystemGameSpy] ProfileDataDirectory` / `ProfileDataExtension` | `../ASAMU/SaveData` / `.ue3profile` | No evidence of use: the game runs on the Steamworks subsystem and no ASAMU script references profile data. TENTATIVE |
| `[URL]` maps | `Map=ASAMUFrontEndMap.asamu`, `LocalMap=ASAMULegal.asamu`, `TransitionMap=ASAMUEntry.asamu` | boot flow (see LEVELS.md) |

### 1.2 macOS per-user directory [CONFIRMED (native) unless marked]

Reproduce: `research/local/save/anchors-save.txt` → `DecompileToLocal.java` (section 11).

- `MacGetAppSupportDirectory` asks Cocoa for the user-domain Application Support folder
  (`NSSearchPathForDirectoriesInDomains(NSApplicationSupportDirectory = 14, NSUserDomainMask, expand tilde)`).
- `FFileManagerMac::Init`, unless the command line has `NOHOMEDIR`: reads `DefaultEngine.ini` from the
  game config dir, finds the first `MyDocumentsSubDirName=` (case-insensitive) and takes the text after it
  up to the end of the line (line split STRONG); builds **user dir = AppSupport + `/` + value + `/`** (the
  `/` literals CONFIRMED from the disassembly). A missing ini or key shows a message box and quits. It also stores the install "base dir" (the executable's base path cut just after the `/` that
  precedes `Engine/`).
- `FFileManagerMac::CreateFileWriter` = `ConvertToAbsolutePath` then `ConvertAbsolutePathToUserPath`
  (vtable slots +0xA8/+0xB0, verified against the vtable symbol), i.e. the base-dir prefix is replaced by the
  user dir for **every** write. `CreateFileReader` tries the user path first and falls back to the plain
  absolute path (so shipped defaults are read from the bundle until a user copy exists).
- Result for saves: `..\..\ASAMU\Saves\SaveGame.bin` → `~/Library/Application Support/A Story About My Uncle/ASAMU/Saves/SaveGame.bin`.
  STRONG (the exact base-dir string at runtime was not observed; the replacement rule is CONFIRMED).
- Generated user config files go to `…/A Story About My Uncle/ASAMU/Config/` by the same rule; their file
  names follow the UE3 `<GameName><Type>.ini` scheme (`…Engine.ini`, `…Game.ini`, `…Input.ini`,
  `…SystemSettings.ini`, and `…Settings.ini` for the `config(Settings)` class). Exact names TENTATIVE
  (game-name prefix not observed); defaults come from `Config/Mac/Mac*.ini` → `Default*.ini`.

### 1.3 Windows [CONFIRMED (steam, config); file names CONFIRMED (cdo)]

- Steam's cached app info for App 278360 (`~/Library/Application Support/Steam/appcache/appinfo.vdf`) declares
  Auto-Cloud save files: root `WinMyDocuments`, path `My Games/A Story About My Uncle/ASAMU/Saves`,
  pattern `*.bin`, platform `Windows`. So on Windows the files are
  `%USERPROFILE%\Documents\My Games\A Story About My Uncle\ASAMU\Saves\{SaveGame,PlayerProgression,GeneralSave,TTS}.bin`
  — the stock UE3 "standard user" layout (`My Games\<MyDocumentsSubDirName>\` + game-relative path).
- Mac saves are **not** cloud-synced (no macOS entry in that Auto-Cloud block), so Windows and Mac saves
  do not meet unless copied by hand. CONFIRMED (steam).
- Config on Windows: `…\My Games\A Story About My Uncle\ASAMU\Config\` (STRONG, UE3 convention).

### 1.4 Linux / Proton [TENTATIVE]

No native Linux build ships (the depot carries only stock `Engine/Config/Linux` files). Under Proton the
Windows layout applies inside the prefix:
`<steamapps>/compatdata/278360/pfx/drive_c/users/steamuser/Documents/My Games/A Story About My Uncle/ASAMU/Saves/`.

### 1.5 What exists on this machine (sanitized: existence, names, sizes only)

| Location | Result |
|---|---|
| `~/Library/Application Support/A Story About My Uncle/` | absent (the game has never run on this Mac). CONFIRMED |
| `~/Library/Preferences`, `~/Documents`, `~/Library/Containers`, `~/Library/Caches`, `~/Library/Saved Application State` | nothing ASAMU-related. CONFIRMED |
| `~/Library/Application Support/Steam/userdata/<id>/278360/remotecache.vdf` (862 B) | Steam's cloud bookkeeping lists three cloud files: `My Games/A Story About My Uncle/ASAMU/Saves/GeneralSave.bin`, `…/PlayerProgression.bin`, `…/SaveGame.bin`, each with root 2. Every other field is a placeholder: `size`, `localtime`, `time`, `remotetime` are 0 and `sha` is all zeros; `ChangeNumber` 0, `syncstate` 2, `persiststate` 0, `platformstosync2` 1. No `TTS.bin` entry, and no `remote/` folder. CONFIRMED |

Interpretation: the entries name exactly the three Windows cloud files, and `platformstosync2` = 1 fits a
Windows-only platform mask. Since the metadata is all zero (no size, time or hash), these entries alone do
**not** prove that real save contents exist in the cloud. The earlier reading "the account has Windows cloud
saves; no time trial was finished" is downgraded to TENTATIVE / UNKNOWN. Consequently no real save could be
decoded and section 3 remains a code-derived prediction.

## 2. Files and owning classes

| File | Owner (Object subclass) | Writer → reader | Version arg | Encrypted | `bIsSaveGame` |
|---|---|---|---|---|---|
| `SaveGame.bin` (`ASAMUGameInfo.saveGameLevelName`) | `SaveGameState` | `ASAMUGameInfo.SaveGame` → `ASAMUGameInfo.LoadGame` | `SaveGameState.SAVEGAMESTATE_REVISION` = 3 | yes | yes |
| `PlayerProgression.bin` (`ASAMUProgressionManager.SaveFileName`) | `ASAMUProgressionObject` | `ASAMUProgressionManager.SaveProgression` → `LoadProgression` | 3 (same constant) | yes | yes |
| `GeneralSave.bin` (`ASAMUGeneralSaveManager.SaveFileName`) | `ASAMUGeneralSaveObject` | `ASAMUGeneralSaveManager.SaveGeneralSave` → `LoadGeneralSave` | `ASAMUGameInfo.currentVersion` = 1 | yes | yes |
| `TTS.bin` (literal) | `TimeTrialSavefile` | `ASAMUHUDMovieTimeTrial.SaveScoreboard` → `LoadScoreboard`, `GFxASAMUMainMenu.LoadTimeTrialScores` | `TimeTrialSavefile.REVISION` = 3 | no | no |

All CONFIRMED (cdo for file names, src for the calls). Folder for all: `saveGameDirectory` =
`..\..\ASAMU\Saves\` (cdo).

`SaveGameState` / `SaveGameStateInterface` / `SaveGameState_SeqEvent_SavedGameStateLoaded` /
`SaveGameStateKActor` / `SaveGameStatePlayerController` are Epic's UDK "save game state" sample (2012
headers) with ASAMU additions (checkpoint table, grapple state, Kismet `bEnabled`, revision field, a
"loaded" Kismet event carrying the checkpoint index). `SaveGameStateKActor`, `SaveGameStatePlayerController`
(exec `SaveGameState`/`LoadGameState`, `.sav` names) and `ASAMUGameState` (a `BasicSaveObject` wrapper with
version 1) are never used by the game: no map instantiates the KActor class, ASAMU's controller does not
derive from the sample controller, and nothing calls `ASAMUGameState`'s save/load. STRONG (src, map).

## 3. Container format (`UEngine::BasicSaveObject` / `BasicLoadObject`) [CONFIRMED (native) unless marked]

### 3.1 Write path (`UEngine::BasicSaveObject(Obj, Path, bIsSaveGame, Version, bEncrypt)`, Mac 0x1008400D0)

1. Open a file writer for `Path` with flag `0x20` (save-game) when `bIsSaveGame` (Mac: same redirection
   either way, 1.2).
2. If `bEncrypt`: write the 4-byte `EncryptedMagic` (data symbol at 0x102336428, value
   `0xC0DEDBAD`, stored little-endian as `AD DB DE C0`) **directly to the file, unencrypted**.
3. Into a memory buffer: `i32 Version` (little-endian), then `Obj->Serialize` through an
   `FObjectAndNameAsStringProxyArchive` (3.4).
4. If `bEncrypt`: zero-pad the buffer to a multiple of 16 and encrypt it in place (3.3).
5. Append the buffer to the file. The function reports success whenever the writer opened.

### 3.2 Read path (`UEngine::BasicLoadObject(Obj, Path, bIsSaveGame, Version)`, Mac 0x100840680)

1. Load the whole file (read flag 2 when `bIsSaveGame`); missing file → `false`.
2. If the first 4 bytes equal `EncryptedMagic`: zero-pad the rest to 16 and decrypt from offset 4.
   (A file shorter than 4 bytes is treated as ciphertext from offset 0.) Otherwise the file is plain.
3. Read `i32` version; if it is **not equal** to the expected version → `false` and the object is untouched.
4. Otherwise deserialize into the existing object (properties absent from the file keep their current
   in-memory values) and return `true` — there is no further validation.

So plain and encrypted files are both accepted by the loader, which is useful for testing and import.

### 3.3 Encryption [CONFIRMED (native)]

`appEncryptData` / `appDecryptData` (Mac 0x100004DA0 / 0x100005E60, unit `AES.cpp`): a public-domain
Rijndael implementation; key schedule of 60 words fed with **32 bytes** read from a 32-character ASCII
string literal (the characters themselves are the key bytes — not hex-decoded), i.e. **AES-256**; every
16-byte block is processed independently and in place (**ECB, no IV, no MAC**). The key literal is the
only string referenced by both functions (Mac: `lea` at 0x100004E23 / 0x100005EE3). **The key bytes are
intentionally not reproduced in this repository**; an importer obtains them from the user's own
executable (9.4).

Verification details [CONFIRMED (native), re-derived with `objdump` and Ghidra]:
- Both `lea`s resolve to VA 0x101A89AE0 in `__TEXT,__cstring` (file offset 0x1A89AE0 in the Mac
  executable whose SHA-256 begins `b611c4a0a64d220f`, see INVENTORY.md). The literal is exactly 32 bytes
  followed by a NUL.
- The setup routines read key bytes 0…31 one by one, and the round-key buffer is 60 words, so this is
  AES-256 with the **raw characters** as key bytes. Every character of the literal happens to be a hexadecimal
  digit, but it is **not** hex-decoded into a 16-byte key. An importer that decodes it will fail.
- Fingerprint of the 32 key bytes, so an importer can check its extraction without the repository holding
  the key: SHA-256 `ca6ed26caf728656631403166e61ff9cd252c79de32474689df77d370036e869`.
- Each 16-byte block is encrypted or decrypted in place (source = destination), and the loop steps by 16:
  ECB.
- Explicit-key variants `appEncryptDataWithKey` / `appDecryptDataWithKey` (0x100006F20 / 0x100007000)
  also exist; `BasicSaveObject`/`BasicLoadObject` call only the fixed-key `appEncryptData`/`appDecryptData`.

### 3.4 Object body inside the (decrypted) buffer [STRONG (native) — not checked against a real file]

```
i32     Version                      (3.1 step 3)
i32     NetIndex                     UObject::SerializeNetIndex, written because port flags are 0;
                                     value for runtime-created objects expected to be -1 (TENTATIVE)
tagged properties                    UStruct::SerializeTaggedProperties
FString "None"                       terminator (NAME_None written through the proxy)
zero bytes                           only in encrypted files: padding to the 16-byte boundary
```

No other prelude: the save objects have no state frame and the name/outer/class block is only written for
non-loading, non-saving archives (`UObject::Serialize`, Mac 0x100142B00).

Tagged properties follow the package format in OBJECT_FORMAT.md (`Name`, `Type`, `i32 Size`,
`i32 ArrayIndex`, `u8` bool value / struct name / enum name, then `Size` value bytes), with two changes made
by the proxy archive (vtable slots +0x38/+0x40 resolve to `FNameAsStringProxyArchive::operator<<(FName&)`
and `FObjectAndNameAsStringProxyArchive::operator<<(UObject*&)`):

- every **FName** (tag name, type name, struct/enum name, enum-typed byte values, the terminator) is an
  **FString** (`i32` length incl. NUL, positive = 8-bit chars, negative = UTF-16) instead of an 8-byte index;
- every **object reference** is an FString holding the object's full path (`"None"` for null). On load the
  path is resolved with a find-only lookup (no package loading), so references to transient runtime objects
  from an earlier session come back as `None`.

Further rules:
- Properties are written in property-link order (own class first, declaration order; OBJECT_FORMAT.md) and
  **only when they differ from the class default object** (per-element `Identical` test); the `Size` field
  is back-patched after the value. CONFIRMED (native).
- Which properties are skipped is decided by `UProperty::ShouldSerializeValue` with flag mask
  `0x11C20203000` (native, transient on persistent archives, duplicate-transient, deprecated, editor-only,
  not-for-console, non-transactional, archetype cases). Nothing in it depends on the save-game flag, so all
  plain script `var`s of the save classes are written. CONFIRMED (native; verification pass: the save-game
  flag that `BasicSaveObject` stores in its memory archive is never read by `ShouldSerializeValue` or the
  save loop of `SerializeTaggedProperties`, and none of the save-class properties carries a native,
  transient or deprecated flag in `asamu-inspect class`).
- Order check: the `ASAMUGameInfo` CDO (written by the same routine) lists its own properties first in
  source declaration order, then the inherited `GameInfo` ones, which is the order the save files will use.
  CONFIRMED (cdo).
- Enum bytes, alone or as array items, are written as their enumerator **name** (string). STRONG
  (OBJECT_FORMAT item rule + proxy).

### 3.5 Embedded JSON dialect (`Engine.JsonObject`) [CONFIRMED (src, native)]

- A `JsonObject` keeps a string→string value map and a string→object map. Integer/float/bool setters store
  the value text behind a two-character marker (backslash + `#`); getters drop the first two characters.
- `EncodeJson` writes `{`, then the value pairs, then the object pairs, comma-separated: marked values as
  `"key":value` with the marker stripped (unquoted number/`true`/`false`), other values as `"key":"value"`.
  No escaping of quotes inside strings was seen (TENTATIVE). Pair order is the map's iteration order —
  insertion order in practice (STRONG; UE3 sparse-array maps without removals).
- Keys are compared case-insensitively (UE3 `FString` map keys) — one ASAMU writer depends on this (4.3).
  STRONG.
- Float text uses UnrealScript's float-to-string conversion, which formats with **four decimals**
  (`%.4f`, no trimming of zeros: 1.5 becomes `1.5000`). This bounds the precision of saved times, positions
  and Matinee positions to 0.0001. CONFIRMED (native: `UObject::execFloatToString` at 0x1000B8B80 passes the
  wide literal `%.4f` at 0x10164149C to `FString::Printf`). This corrects the earlier "TENTATIVE: two
  decimals". Float values appear in the time-trial scores (4.4), Kismet float/vector variables, event
  re-trigger delays and Matinee positions (4.3).

## 4. Data model per file

Confidence: field lists CONFIRMED (src, `asamu-inspect class`); "written when" rules CONFIRMED (src).

### 4.1 `GeneralSave.bin` — `ASAMUGeneralSaveObject` (file version 1)

| Property (order) | Type | Meaning |
|---|---|---|
| `latestSaveMap` | Str | **chapter title** of the last story level entered, i.e. its `WorldInfo.Title`, which is always an `ASAMULevels` enumerator name (e.g. `StarHaven`, `Village`); empty after a reset or New Game. Corrected in the verification pass; the earlier reading "map package name, e.g. `AG-StarHaven`" was wrong. STRONG (see note below) |
| `currentLevelIndex` | Int | `ASAMULevels` value of that level (6.1); omitted when 0 |
| `savedStringsJSON` | Str | JSON: `Revision` (2; files with < 2 are rejected), `arrayLength`, and objects `object0…objectN-1` each with `ID` (string) and `stringValue` (int) |

Note on `latestSaveMap`: the value comes from `WorldInfo.GetMapName()` called without the prefix flag.
Natively (`AWorldInfo::GetMapName` at 0x100D00F60) that call returns the `FString` stored at
`AWorldInfo+0x730` when it is non-empty. Otherwise it returns the map name with everything up to and
including the first `-` removed. The fallback path is confirmed to strip the `AG-` prefix (CONFIRMED, native); the other path returns the chapter title, which carries no prefix anyway. The field at
+0x730 is `Title`: laying out the `WorldInfo` properties in declaration order from the known
`RBPhysicsGravityScaling` at +0x608 (NATIVE_PHYSICS.md) puts `Title` at exactly +0x730, once the two
48-byte `MusicTrackStruct`s are counted (STRONG). The pointer update only runs when the title matched an
enumerator name, so the stored string is that name. An importer should ignore this field and use
`currentLevelIndex`.

The "saved strings" are named integer flags written by Kismet `SeqAct_EditOrAddSaveString` (ID, value) and
read by `SeqAct_GetSaveStringValue` (output 0 = found + value, output 1 = absent). IDs used by the shipped
maps [CONFIRMED (map)]: `NarratorCollectable` (written 1 in ParadiseCave, BeautifulCity, Darkcave, StarHaven,
IceCave; ParadiseCave has a second writer with value 0) and `RevealAirship` (StarHaven, value 1). Every edit
rewrites the file immediately. (ID and value come from linked `SeqVar_String`/`SeqVar_Int` variables, not
from tags on the action; the ParadiseCave value-0 writer links an `SeqVar_Int` left at its default.) When
the file is read back, an ID already held in memory keeps its in-memory value, so duplicates are never
added. CONFIRMED (map, src).

### 4.2 `PlayerProgression.bin` — `ASAMUProgressionObject` (file version 3)

| Property (order) | Type | Meaning |
|---|---|---|
| `collectibleSave` | Str | JSON (below); the authoritative collectible + story-item + finished data |
| `achievementSave` | Str | JSON: `Revision` 2, `arrayLength`, keys `"0"…"N-1"` → achievement enum index (6.4) |
| `timesGrappled` | Int | never written by any script — dead field |
| `interactedInteractables` | array<Str> | duplicate of the JSON list (in-memory list; written as a property too) |
| `collectedCollectibles` | array<Str> | never filled — dead field |
| `levelProgressions` | array<object> | paths of transient helper objects — **useless after restart** (3.4); rebuilt from `collectibleSave` |
| `unlockedLevels` | array<enum `ASAMULevels`> | **only stored here** (not in JSON): chapters entered at least once, in first-entry order, as enumerator names |
| `bFinishedGame` | Bool | also mirrored in the JSON |

`collectibleSave` JSON: `Name` = `Collectibles`, `levelCount`, `Revision` 2, `finishedGame` 0/1, objects
`Level0…` each with `Name` (level **file** name, e.g. `AG-Darkcave`), `collectedCount`,
`collectible0…` (full actor path of each collected `ASAMUCollectible`, e.g.
`AG-Darkcave.TheWorld.PersistentLevel.ASAMUCollectible_3` — STRONG format), and object `interactables` with
`interactedCount` and `interactable0…` (level file name concatenated directly with an actor path; 6.3).
A JSON revision below 2 (either JSON) makes the load fail → full reset (section 5).

### 4.3 `SaveGame.bin` — `SaveGameState` (file version 3)

| Property (order) | Type | Meaning |
|---|---|---|
| `PersistentMapFileName` | Str | package name of the map that wrote the snapshot |
| `GameInfoClassName` | Str | path of the game class (informational; never read back) |
| `StreamingMapFileNames` | array<Str> | streamed levels visible/pending at save time (never read back) |
| `SerializedWorldData` | array<Str> | one JSON document per entry, in this order: |
| `savedRevisionNumber` | Int | 3; a load with < 3 ignores the snapshot contents |

`SerializedWorldData` entries:
1. **Checkpoint table** — `name` (lower-case) = `CheckpointManager`, `latestCompletedCheckpointPerLevelLength`
   = L, and L keys of the form `latestCompletedCheckpointPerLevel[ i ]` (the spaces are part of the key) →
   latest checkpoint index for level enum value i. The loader looks the entry up by `Name`; this works only
   because keys are case-insensitive (3.5).
2. **Grapple/abilities** — `Name` = `GrappleGun`, `AvailableGrappleAmount` (capacity, see GRAPPLE.md
   G-CT-2), `IsRocketBootsAvailable` 0/1, `IsGrappleGunAvailable` 0/1 (the grapple latch, GRAPPLE.md G-IN-4).
   Written from the pawn's current gun; in a world without a player pawn (the main menu) this entry comes out
   empty (TENTATIVE: UE3 "accessed none" yields an empty string).
3. **Savable actors** (dynamic actors implementing `SaveGameStateInterface`), each keyed by `Name` = actor
   path: `ASAMUCollectible` (`bCollected`), `ASAMURechargeCrystal` (`currentState` 0/1),
   `ASAMUFallingRock` and `ASAMUFallingWhenGrappledRock` (`ObjectArchetype`, `Location_X/Y/Z`,
   `Rotation_Pitch/Yaw/Roll`, `currentFallRate`, `fallDistance`, `startPos_Z`, plus `Visible` and `State`
   0/1/2 for the falling rock), `ASAMUCheckpointVisuals` (`State` 0/1/2 — **but no `Name`**, so it can
   never be matched on load).
4. **Kismet events** of every root sequence (persistent and streamed): `Name` (object path),
   `ActivationTime` (remaining re-trigger delay, ≥ 0), `TriggerCount`, `bEnabled` 0/1.
5. **Kismet variables**: `Name` + `Value` for Bool (int), Float, Int, Object (path string), String;
   `Value_X/Y/Z` for Vector.
6. **Matinee** (`SeqAct_Interp`): `Name`, `Position`, `IsPlaying` 0/1, `Paused` 0/1.

Ordering detail (verification pass, CONFIRMED (src)): entries 4 and 5 are emitted **per root sequence** —
all events of the first root sequence, then its variables, then the events of the next root sequence, and so
on; all Matinee entries follow after the last root sequence. The world snapshot's minimum legible
`savedRevisionNumber` is the class constant `LOWEST_LEGIBLE_REVISION` = 3 (cdo/class model).

Load (`SaveGameState.LoadGameState`): entries are dispatched by `Name` — the checkpoint table, the grapple
entry, names containing `SeqAct_Interp` (Matinee: playing ones restart from the saved position), names
containing `SeqEvent`/`SeqVar` (Kismet), anything else is looked up as an actor (and spawned from
`ObjectArchetype` if missing) and handed its JSON. Matinee entries that were not playing are still moved to
their saved position. Unknown paths are skipped silently, which is what makes a snapshot from one map
harmless in another. Two early exits skip everything below: a snapshot with no entries at all, and a
revision below 3 (the caller ignores the result in both cases). Afterwards the player is reset to the latest checkpoint
(ABILITIES.md A-CP-4/5) and every `SaveGameState_SeqEvent_SavedGameStateLoaded` is told the loaded
checkpoint index (all shipped instances keep the default `Index` −1 = "fire on any load"; 1 in ParadiseCave,
3 in BeautifulCity, 2 in Darkcave, 1 in StarHaven) [CONFIRMED (src, cdo, map)].

### 4.4 `TTS.bin` — `TimeTrialSavefile` (file version 3, plain)

| Property | Type | Meaning |
|---|---|---|
| `Scores` | array<`TimeTrialScore` object> | transient objects → paths only, useless after restart |
| `SerializedData` | Str | JSON: objects `level0…level4`, each with `score` (float seconds, four decimals (3.5); 0 = no time). Unlike the other JSON documents it carries **no** `Revision` key |
| `LevelTargetScores` | array<struct> | equal to the class default, therefore normally **not written** |

Index i = level enum − 2 (ParadiseCave 0, BeautifulCity 1, Darkcave 2, StarHaven 3, IceCave 4).

## 5. Lifecycle: what is loaded and saved, and when [CONFIRMED (src, map) unless marked]

| Moment | Reads | Writes |
|---|---|---|
| Any map whose game class is `ASAMUGameInfo` or a subclass starts (`InitGame`; includes the menu and time trial, not the legal screen) | `PlayerProgression.bin`, then `GeneralSave.bin` | if either fails (missing, wrong file version, JSON revision < 2): **reset all** = clear achievements, story items and finished flag (collectibles and unlocked chapters stay in memory), clear chapter pointer and flags → rewrite both files; "legible save" = false (disables Continue) |
| Story map starts (`InitSaveManagerForLevel`, skipped in menu and time trial) | `GeneralSave.bin` (re-read before the pointer update) | if the map's `WorldInfo.Title` names a chapter: add it to `unlockedLevels` (no duplicate) and write `PlayerProgression.bin` (written on every chapter start, even when already unlocked); set chapter pointer (index + chapter title, 4.1) → `GeneralSave.bin` |
| Player pawn spawned in a story map (not in play-in-editor, not time trial) | `SaveGame.bin` | on failure: fire "loaded" event with −1, register checkpoint 0, **reset all** (row 1) |
| New latest checkpoint (ABILITIES.md A-CP-3) | — | `SaveGame.bin` (snapshot of the current world) |
| New collectible / new optional story item / new achievement | — | `PlayerProgression.bin` |
| Kismet `SeqAct_EditOrAddSaveString` | — | `GeneralSave.bin` |
| Kismet `SeqAct_SetGameFinished` (Epilogue intro) | — | `PlayerProgression.bin` (`bFinishedGame`), enables menu quit voices |
| Main menu "New Game" | — | fresh `SaveGame.bin` (written from the menu world: empty checkpoint table), `GeneralSave.bin` with cleared pointer (flags kept); then opens `AG-Workshop` |
| Level select (chapter) | — | fresh `SaveGame.bin` as above; opens that chapter's map |
| Continue | `PlayerProgression.bin` again | — ; opens `LevelFileNames[currentLevelIndex]` |
| Death, F7 quick-restart (`QuickLoad`: death sequence → latest checkpoint; disabled in Workshop and Epilogue and when the HUD option is off) | — | nothing (no disk access) |
| Kismet `SeqAct_LoadGame` | `SaveGame.bin` | (not placed in any shipped map) |
| Time-trial end (`EndTimeTrial`) | — | `TTS.bin` always (new best stored only if faster or first); all five golds → achievement → `PlayerProgression.bin` |
| Main menu / time-trial HUD start | `TTS.bin` | — |

Chapter-to-chapter carry-over: a level transition is a plain `open <map>` from Kismet, so the new map loads
the previous map's **last checkpoint snapshot**. Its Kismet/actor entries do not resolve in the new map and
are skipped; what carries over is the per-level checkpoint table and the grapple entry (capacity, boots,
grapple latch) as they were at the last checkpoint. The new level's Kismet then sets its own abilities
(GRAPPLE.md G-CT-3). STRONG (src, map).

## 6. Progression model

### 6.1 Chapters [CONFIRMED (src, cdo, map)]

| Enum `ASAMULevels` | Value | `LevelFileNames[v]` | `WorldInfo.Title` | Collectibles | `ASAMUCheckpoint`s | Next map (Kismet) |
|---|---|---|---|---|---|---|
| `NoLevel` | 0 | `ASAMUFrontEndMap` | (front end: `Workshop`) | 0 | 1 | — |
| `Workshop` | 1 | `AG-Workshop` | `Workshop` | 0 | 1 | `AG-ParadiseCave` |
| `Sanctuary` | 2 | `AG-ParadiseCave` | `Sanctuary` | 5 | 24 | `AG-BeautifulCity` |
| `Village` | 3 | `AG-BeautifulCity` (+ always-loaded `Freds_place`) | `Village` | 5 | 12 | `AG-DarkCave` |
| `DarkCave` ("Chasms") | 4 | `AG-Darkcave` | `DarkCave` | 5 | 17 | `AG-StarHaven` |
| `StarHaven` ("SkyCave") | 5 | `AG-StarHaven` | `StarHaven` | 5 | 25 | `AG-IceCave` |
| `IceCave` | 6 | `AG-IceCave` (+ Kismet-streamed `TheCore`) | `IceCave` | 5 | 28 | `TheCore` Kismet opens `AG-Epilogue` |
| `Epilogue` | 7 | `AG-Epilogue` | `Epilogue` | 0 | 1 | back to `ASAMUFrontEndMap` |

- The current chapter is found by matching `WorldInfo.Title` against the enum names (not the file name);
  any other map yields −1 (no unlock, no pointer update). `TheCore` runs inside IceCave's world, so it counts
  as IceCave.
- Every chapter except Workshop also has a Kismet "back to main menu" command
  (`?game=ASAMU.GFxASAMUMenuGameInfo`); BeautifulCity additionally has a plain `open ASAMUFrontEndMap`
  without a game option. The BeautifulCity → Darkcave command is spelled `open AG-DarkCave` (different
  case from the file `AG-Darkcave`), so map lookups must be case-insensitive. CONFIRMED (map).
- Kismet also issues console commands that have no handler anywhere (`SaveLevelTwo` in ParadiseCave,
  `SaveLevelThree` in BeautifulCity, `SaveLevelFour` in Darkcave and IceCave, plus those listed in GRAPPLE.md
  G-CT-3). They are inert leftovers. STRONG (upgraded from TENTATIVE in the verification pass): no name
  table of any of the 12 cooked `.u` files, `Startup.upk` or the other cooked `.upk` files contains
  `SaveLevel*`. The executable contains it in neither ASCII, UTF-16 nor UTF-32, and no shipped ini or
  localization file defines it as an alias. As positive controls, the native console command
  `DisableAllScreenMessages` (issued in the same Kismet actions) is found in the executable as UTF-32, and
  the script exec commands `ToggleAdventureSuit`/`NormalMode` are found in `Startup.upk`'s name table.

### 6.2 Unlocks

| What | Rule | Confidence |
|---|---|---|
| Chapter in level select | enabled once entered (`unlockedLevels`); Workshop always enabled; choosing one overwrites the snapshot (confirmation popup if a legible save exists) | CONFIRMED (src) |
| Time-trial menu | enabled when `bFinishedGame` (set in Epilogue) | CONFIRMED (src, map) |
| Continue | enabled when both progression and general save loaded legibly at menu start | CONFIRMED (src) |
| Extras (options menu) | by total collectibles: beam colour ≥ 10, goat mode ≥ 15, Midas mode ≥ 20, parkour mode ≥ 25; a HUD notice fires when the total reaches exactly 10, 15, 20 or 25 | CONFIRMED (src) |
| Grapple capacity per chapter | Kismet `SeqAct_SetMaxGrapples` (values in GRAPPLE.md G-CT-3), carried between chapters by the snapshot | CONFIRMED (map) / STRONG (carry-over) |
| Rocket boots | `SeqAct_ToggleRocketBoots`: one "off" each in ParadiseCave, BeautifulCity, Darkcave; three "on" in StarHaven; three "on" + one "off" in IceCave (boots acquired in StarHaven, break in IceCave). The class default of `Enable` is **true**, so instances without an `Enable` tag are "on" (a converter must apply the CDO default). Firing order is the Kismet workstream's. | CONFIRMED (map, cdo) census; TENTATIVE story reading |

### 6.3 Collectibles and optional story items

- `ASAMUProgressionManager` CDO: `COLLECTIBLES_PER_LEVEL` = 5, `TOTAL_INTERACTABLES_COUNT` = 11,
  `levelWithCollectiblesNames` = ParadiseCave, BeautifulCity, Darkcave, StarHaven, IceCave. Map census: exactly
  5 `ASAMUCollectible` in each of those maps, 0 elsewhere → 25 total. CONFIRMED (cdo, map).
- Pick-up (touch by the player pawn): records the actor path under the current chapter's file name (no
  duplicates) and saves; when a level reaches 5 and the total reaches 25 → `ALL_COLLECTIBLES_FOUND`.
  In time trial collectibles are hidden and non-colliding. CONFIRMED (src).
- A collectible's "already taken" look is restored only from the same map's snapshot; replaying a chapter
  from level select shows them untaken, but taking them again adds nothing. STRONG (src).
- Optional story items (`ASAMUInteractable_Actor` with `bIsOptional`): a parent item registers itself; a
  child forwards to its parent; a stand-alone optional item registers its (null) parent, i.e. the key
  `<level file name>None`, at most once per level. Census of optional items (Workshop 1 stand-alone;
  ParadiseCave 2 parents; BeautifulCity, Darkcave, StarHaven, IceCave each 1 parent + 1 stand-alone) gives
  1 + 2 + 2 + 2 + 2 + 2 = **11 distinct keys = `TOTAL_INTERACTABLES_COUNT`** → `INTERACT_ALL_STORY`.
  STRONG (src + map census; the agreement with the constant is the cross-check).

### 6.4 Achievements (`ASAMUAchievementManager.EASAMUAchievements`) [CONFIRMED (src, map)]

| Enum (index) | Steam ID | Trigger |
|---|---|---|
| `MADDIE_CHALLENGE` (0) | 1 | Kismet, StarHaven |
| `SANCTUARY_GRAPPLE_CHALLENGE` … `ICECAVE_GRAPPLE_CHALLENGE` (1–5) | 2–6 | Kismet, one per collectible chapter |
| `FLOOR_IS_LAVA` (6) | 7 | Kismet, IceCave |
| `ALL_GOLD_MEDALS` (7) | 8 | five gold time-trial medals (6.5) |
| `ALL_COLLECTIBLES_FOUND` (8) | 9 | 25 collectibles |
| `SANCTUARY_NO_FAIL` … `ICECAVE_NO_FAIL` (9–13) | 10–14 | Kismet, one per collectible chapter |
| `INTERACT_ALL_STORY` (14) | 15 | 11 optional story keys |

Unlocking records the index once (saving progression) and always also calls the online subsystem's
`UnlockAchievement(controller, index + 1)`. The 12 Kismet-driven achievements map one-to-one onto the 12
`SeqAct_UnlockASAMUAchievement` instances in the maps (2 each in ParadiseCave, BeautifulCity, Darkcave; 3
each in StarHaven, IceCave; none elsewhere); the StarHaven Maddie instance relies on the property default
(index 0).

### 6.5 Time trial

- Entered from the time-trial menu with `?game=ASAMU.ASAMUGameInfoTimetrial` on the five collectible maps.
  Same movement; no snapshot, no snapshot load, no chapter pointer; dying before any checkpoint restarts the
  stopwatch; F8 restarts the level while a trial runs (ABILITIES.md A-TT-1/2). CONFIRMED (src, map).
- Targets (`TimeTrialSavefile.LevelTargetScores`, seconds, gold / silver / bronze) [CONFIRMED (cdo)]:

| Level | Gold | Silver | Bronze |
|---|---|---|---|
| ParadiseCave (Sanctuary) | 260 | 290 | 320 |
| BeautifulCity (Village) | 200 | 220 | 270 |
| Darkcave | 240 | 270 | 320 |
| StarHaven | 480 | 540 | 650 |
| IceCave | 810 | 960 | 1150 |

- Medal = the first of gold, silver, bronze whose target is ≥ the best time (best time must be > 0); none
  otherwise. A finished run is stored if faster than the best or if no best exists; the file is rewritten at
  every finish. All five gold → `ALL_GOLD_MEDALS`. The menu shows times as `MM:SS:hh`; ≥ 5999.59 s shows
  `99:99:99`. CONFIRMED (src).

## 7. Settings

- `ASAMUSettingsManager` is a `config(Settings)` class; its setters (generic float/bool setters, gamma, speed
  lines, controller layout) persist with `SaveConfig` (user `…Settings.ini`, 1.2). `HandScale` has no setter
  in script. Keys and shipped defaults (`DefaultSettings.ini` `[ASAMU.ASAMUSettingsManager]`)
  [CONFIRMED (config, src)]: `MasterGroupVolume` 1.0, `MusicGroupVolume` 0.8, `SFXGroupVolume` 0.8,
  `VoiceGroupVolume` 0.8, `FOV` 90 (≤ 0 falls back to 90), `Gamma` 0.5, `HandScale` 1.0, `BeamColor` 0,
  `BeamColorActive` false, `SpeedlinesActive` true, `RumbleActive` true, `GoatModeActive` / `MidasModeActive`
  / `ParkourModeActive` false, `XboxStickLayout` 0, `XboxButtonLayout` 0. Applied at login (audio group
  volumes, `FOV`, gamma, beam colour, speed lines, goat/Midas/parkour toggles).
- Settings stored elsewhere through console `set`: mouse sensitivity and smoothing (`Engine.PlayerInput`),
  controller sensitivity (`ASAMU.ASAMUControllerInput`), subtitles (`Engine.Engine`), invert mouse, key
  bindings (`PlayerInput.SaveConfig`) → the user input/engine ini files. CONFIRMED (src).
- `ASAMUSystemSettingsManager` (`config(SystemSettings)`, the only native ASAMU class) gets/sets engine
  system settings by name and saves them with `SaveSystemSettings`; names used by the menus: `ResX`, `ResY`,
  `Fullscreen`, `UseVsync`, `MotionBlur`, `Bloom`, `DetailMode`, `MaxMultiSamples`, `bAllowD3d9MSAA`.
  CONFIRMED (src, symbols).
- Settings are never in the `.bin` files and are not cloud-synced. CONFIRMED.

## 8. Quirks worth deciding on (keep for parity or fix deliberately)

| # | Quirk | Confidence |
|---|---|---|
| Q1 | One shared snapshot file for all chapters; it only fully restores the map that wrote it (6, 4.3). | CONFIRMED (src) |
| Q2 | "Reset all" after an unreadable save keeps collectibles and unlocked chapters (they live in memory and are re-saved) but wipes achievements list, story items, finished flag (re-locking time trial), chapter pointer and flags. | STRONG (src) |
| Q3 | Achievement indices are appended on every progression load without clearing (CONFIRMED, src). The earlier claim that Continue therefore writes duplicates is **refuted**. Continue's second load happens in the menu's own manager, which is discarded at the map change it triggers; every map's `InitGame` creates fresh managers that load once. No shipped path saves progression between the second load and the travel. A duplicate would need a failed travel followed by a save in the menu. Importers should still de-duplicate. | REFUTED as stated (STRONG, src); latent only |
| Q4 | Checkpoint visuals are saved without a `Name` and are never restored: on load the empty name matches no branch and resolves to no actor. | CONFIRMED (src; upgraded) |
| Q5 | Stand-alone optional story items all collapse into one `<level>None` key per level — and the 11-key achievement count relies on it (6.3). | STRONG (src, map) |
| Q6 | Continue code tests `latestSaveMap` against `NoLevel`. Only chapter titles or an empty string are ever stored (4.1), so the test is never true. After a reset or New Game the pointer is index 0, which is the front-end map. A likely trigger: the first launch (or any reset) fails the load, so Continue is disabled for that session. If the player quits without entering a chapter, both files read back fine at the next launch, Continue is enabled, and it opens `ASAMUFrontEndMap` with the gameplay game class (which then treats the front end as "Workshop"). The code path is CONFIRMED (src); the visible outcome needs a behavioural check. | TENTATIVE (behaviour) |
| Q7 | Checkpoint index used as a list position and the off-by-one bounds check (ABILITIES.md A-CP-3/4). | CONFIRMED (src) |
| Q8 | The snapshot stores the state at the last checkpoint, so abilities that change between the last checkpoint and the level exit are not what the next chapter starts with (until its Kismet sets them). | STRONG (src) |
| Q9 | `levelProgressions`, `Scores` (object arrays) and `timesGrappled`, `collectedCollectibles` are written but meaningless. | CONFIRMED (src, native) |

## 9. Rust implication — our compatible save system

### 9.1 Principles

- **Our own format, our own location.** Never write into the original game's folders. Use the platform's
  application-data directory for our app (macOS `~/Library/Application Support/<our id>/`, Windows
  `%APPDATA%\<our id>\`, Linux `$XDG_DATA_HOME/<our id>/`), resolved in `apps/asamu`/`asamu-game` config,
  never inside gameplay crates.
- **Same semantics, cleaner storage.** Keep the original's four lifetimes as four documents so "New Game",
  "reset", chapter select and time trial behave identically: `progression`, `general`, `snapshot`,
  `time_trial`. Serde + a human-readable format (RON or JSON) with a top-level `format_version`, written
  atomically (temp file + rename) and loaded with explicit migration; corrupt files are quarantined (renamed),
  not silently replaced. Parity quirks Q1–Q8 are explicit, named options with the original behaviour as the
  default where it affects play (Q1, Q7, Q8) and a documented fix where it only loses data (Q2, Q3, Q4).
- **No encryption** in our format (it is local, single-player data); the importer handles the original's.

### 9.2 Model (in `asamu-game::save`, render-free, no UE3 types)

```rust
pub enum ChapterId { Workshop, Sanctuary, Village, DarkCave, StarHaven, IceCave, Epilogue }

pub struct Progression {                 // PlayerProgression.bin
    pub unlocked: Vec<ChapterId>,         // first-entry order kept
    pub collectibles: BTreeMap<ChapterId, BTreeSet<CollectibleKey>>, // stable per-map actor key
    pub story_items: BTreeSet<StoryItemKey>,
    pub achievements: BTreeSet<AchievementId>, // 15 ids, Steam id = index + 1
    pub finished_game: bool,
}
pub struct General {                     // GeneralSave.bin
    pub current: Option<ChapterId>,       // original also stores the chapter title (4.1); redundant
    pub flags: BTreeMap<String, i32>,      // "NarratorCollectable", "RevealAirship"
}
pub struct Snapshot {                    // SaveGame.bin
    pub map: String,
    pub latest_checkpoint: BTreeMap<ChapterId, i32>,
    pub abilities: Option<Abilities>,      // capacity, boots_enabled, grapple_enabled
    pub actors: BTreeMap<ActorKey, ActorState>,   // collectible, crystal, rock (+ checkpoint visuals if fixed)
    pub kismet: BTreeMap<KismetKey, KismetState>, // events, variables, matinee
}
pub struct TimeTrial { pub best: [Option<f32>; 5] } // targets are data, not save state
```

Keys (`CollectibleKey`, `ActorKey`, `KismetKey`) are the original object paths **relative to the map**
(e.g. `TheWorld.PersistentLevel.ASAMUCollectible_3`), produced by our map converter for every converted
actor and Kismet object. Keeping original paths as identities is what makes import (and the original's
"skip what does not resolve" rule) possible.

### 9.3 Lifecycle mapping

`asamu-game` owns the save service with the hooks of section 5: `on_map_start` (load progression + general,
unlock, set pointer; reset rule), `on_player_spawn` (load snapshot, apply checkpoint table and abilities,
apply actor/Kismet entries that resolve, respawn at latest checkpoint, emit `SaveLoaded(index)`),
`on_checkpoint_latest` (write snapshot), `on_collectible`, `on_story_item`, `on_achievement` (write
progression; platform achievement call behind a trait), `on_kismet_flag` (write general),
`on_game_finished`, `new_game` / `start_chapter` / `continue`, `on_time_trial_end`. Time-trial mode
disables the snapshot and pointer hooks. All hooks are plain functions over the model, unit-testable without
Bevy.

### 9.4 Importing original saves (`asamu-ue3::savegame` + a CLI subcommand)

1. **Locate** (read-only): macOS `~/Library/Application Support/A Story About My Uncle/ASAMU/Saves/`,
   Windows `Documents\My Games\A Story About My Uncle\ASAMU\Saves\`, Proton prefix path (1.4), or a
   user-given folder.
2. **Container**: if the first 4 bytes are `AD DB DE C0`, decrypt the rest with AES-256-ECB (zero-pad the
   tail to 16). The key is **read from the user's own executable**: Mac — follow the `lea` in the
   `appDecryptData` symbol to its 32-character literal (VA 0x101A89AE0 in the known build, 3.3), use the 32
   characters as raw bytes and check them against the SHA-256 fingerprint in 3.3; Windows — the executable is not symbol-rich, so scan
   for a 32-character ASCII literal referenced next to the Rijndael tables (to be confirmed on a Windows
   install) and accept the candidate whose SHA-256 matches the fingerprint. That the Windows build uses the
   same key is TENTATIVE: it comes from one shared engine source, but it is unverified. Alternatively accept a user-supplied key file. Without a key, only `TTS.bin` (plain) imports.
3. **Version**: `i32` must be 3 (snapshot, progression, TTS) or 1 (general); otherwise report "unsupported".
4. **Body**: `i32` NetIndex, then a tagged-property reader in "names as strings" mode (reuse the
   `asamu-ue3` property reader with an FName/object codec abstraction: index-based for packages,
   string-based for saves), typed by the save-class schemas from `Startup.upk` (or a small built-in table
   of the five classes); stop at `"None"`; ignore trailing zero padding.
5. **JSON**: a tolerant parser for the UE3 dialect (unquoted numeric/bool values, floats with four decimals,
   case-insensitive keys, last duplicate wins, possibly unescaped quotes, keys like
   `latestCompletedCheckpointPerLevel[ i ]`).
6. **Map** to 9.2: chapter enum names → `ChapterId`; collectible paths → keys by stripping the map package
   prefix; achievements by index (deduplicate, Q3); story items keep their raw `<level><path>` strings;
   snapshot entries kept by path, resolved later against the converted map (unresolved entries are kept but
   inert, like the original).
7. Hostile-input discipline as for packages (checked lengths, bounded string sizes, no panics), and
   fuzz-style tests on truncated/corrupted synthetic files.

Export back to the original format is possible (the format is fully specified above) but not planned.

### 9.5 Tests

Synthetic fixtures written byte by byte in test code: plain and encrypted variants (tests use a made-up
32-byte key injected into the decryptor; the real key never enters the repository), one per save class,
default-omitted properties, enum-name arrays, UTF-16 strings, padding, wrong version, wrong magic,
truncated ciphertext. Round-trip tests for our own format and migration tests per `format_version`.
Real-file tests read `ASAMU_ORIGINAL_DIR` + an optional `ASAMU_ORIGINAL_SAVES` folder and skip when absent.

## 10. Open questions

1. Verify section 3 on a real file (a Windows or Mac save of the user's own): NetIndex value, property order,
   default omission, float text precision. Highest value: one decrypted `PlayerProgression.bin`.
2. Windows key location in `ASAMU-Win32-Shipping.exe` (needs a Windows install; the Mac depot only ships TOC
   files for it).
3. Exact user-config ini names on Mac (game-name prefix) — check after one launch on a machine where running
   the original is acceptable, or read `appCreateIniNames` natively.
4. Q6 (Continue after a reset) and Q8 (ability carry-over) need behavioural confirmation.
5. How each chapter's Kismet uses the "save loaded" index (KISMET.md workstream) — determines which world
   state a resumed checkpoint restores beyond actors.
6. ~~Whether the leftover console commands (6.1) are truly inert~~ — answered in the verification pass
   (STRONG: no handler in any cooked package, the executable or the ini files).
7. On the first real `GeneralSave.bin`: confirm that `latestSaveMap` holds a chapter title (4.1; STRONG
   from native layout, cheap to confirm) and that float text has four decimals (3.5).
8. Whether a Windows save's key equals the Mac key (compare the SHA-256 fingerprint in 3.3).

## 11. Reproduction

```bash
# class defaults (file names, folder, level tables, constants)
asamu-inspect defaults "$COOKED/Startup.upk" asamu.ASAMUGameInfo        # also ASAMUGeneralSaveManager,
                                                                        # ASAMUProgressionManager, TimeTrialSavefile
asamu-inspect class    "$COOKED/Startup.upk" asamu.ASAMUProgressionObject  # field order/types (also
                                                                        # SaveGameState, ASAMUGeneralSaveObject)
# map census (collectibles, checkpoints, Kismet save actions, transitions)
asamu-inspect objects "$COOKED/Maps/AG-IceCave.asamu" --class ASAMUCollectible
asamu-inspect props   "$COOKED/Maps/AG-IceCave.asamu" TheWorld.PersistentLevel.WorldInfo_0   # Title
asamu-inspect objects "$COOKED/Maps/AG-StarHaven.asamu" --class SeqAct_EditOrAddSaveString  # + props of linked SeqVar_String
asamu-inspect objects "$COOKED/Maps/AG-Workshop.asamu" --class SeqAct_ConsoleCommand        # + props → Commands
# native (local only; output under research/local/save/, never committed)
analyzeHeadless research/ghidra ASAMU -process ASAMU -noanalysis -readOnly \
  -scriptPath tools/ghidra-scripts -postScript DecompileToLocal.java \
  names=research/local/save/anchors-save.txt out=research/local/save/decompiled
#   anchors: MacGetAppSupportDirectory, FFileManagerMac::{Init,ConvertAbsolutePathToUserPath,CreateFileWriter,
#   CreateFileReader}, UEngine::{BasicSaveObject,BasicLoadObject}, appEncryptData, appDecryptData,
#   UObject::{Serialize,SerializeNetIndex}, UProperty::ShouldSerializeValue, UStruct::SerializeTaggedProperties,
#   FObjectAndNameAsStringProxyArchive::*, UJsonObject::EncodeJson
nm ASAMU | grep EncryptedMagic          # data symbol; read 4 bytes at its file offset
# verification-pass additions (outputs local only)
#   extra anchors: AWorldInfo::GetMapName, UObject::execFloatToString, appEncryptDataWithKey
objdump -d --no-show-raw-insn --start-address=0x100004da0 --stop-address=0x100004e90 ASAMU  # key lea, ECB loop
objdump -d --no-show-raw-insn --start-address=0x1000b8b80 --stop-address=0x1000b8c00 ASAMU  # %.4f literal ref
asamu-inspect class "$COOKED/Engine.u" Engine.WorldInfo       # property order used for the +0x730 layout
asamu-inspect objects "$COOKED/Maps/AG-Epilogue.asamu" --class SeqAct_SetGameFinished   # + props of linking ops
```

`$COOKED` = `<install>/A Story About My Uncle.app/Contents/Resources/ASAMU/CookedMac`. Steam metadata:
`~/Library/Application Support/Steam/appcache/appinfo.vdf` (Auto-Cloud block of App 278360) and
`userdata/<id>/278360/remotecache.vdf` (file names only).

## 12. Verification log (2026-10-09)

Independent re-derivation of this document's claims. The ScriptText was extracted afresh (local, ignored)
and the anchors were decompiled afresh with Ghidra `-readOnly`, without reusing the first pass's output. Status:
the Result column says whether the claim was re-derived and agrees, was corrected, had its confidence raised, or was refuted.

| # | Claim | Evidence used | Result |
|---|---|---|---|
| 1 | Folder `..\..\ASAMU\Saves\`, file names `SaveGame`/`PlayerProgression`/`GeneralSave`/`TTS.bin` | `asamu-inspect defaults` (3 CDOs) + src call sites | agrees, CONFIRMED |
| 2 | File versions 3/3/1/3 | class-model consts `SAVEGAMESTATE_REVISION`, `currentVersion`, `REVISION` + call sites | agrees, CONFIRMED |
| 3 | Encrypted: snapshot, progression, general; TTS plain and not a "save game" | the fifth/third argument at all 8 call sites | agrees, CONFIRMED |
| 4 | `DefaultEngine.ini` keys | ini | corrected: `AutoSaveDir` is under `[UnrealEd.EditorEngine]` |
| 5 | `EncryptedMagic` 0xC0DEDBAD at 0x102336428 | `nm` + bytes `AD DB DE C0` in `__DATA,__data` | agrees, CONFIRMED |
| 6 | Native addresses (Basic*Object, app*Data, proxies, Init, readers/writers) | `nm` + `c++filt` | agrees, CONFIRMED |
| 7 | AES-256-ECB, 32 raw ASCII key bytes, one literal for both directions | `objdump` (setup reads 32 bytes; per-block in-place loop, step 16) | agrees, CONFIRMED; key location and fingerprint added (3.3) |
| 8 | Write path (flag 0x20, magic unencrypted, version, proxy, zero pad, success once opened) | Ghidra `BasicSaveObject` | agrees, CONFIRMED |
| 9 | Read path (magic test, < 4 bytes case, exact version, untouched on mismatch) | Ghidra `BasicLoadObject` | agrees, CONFIRMED |
| 10 | Names/objects as strings; find-only object lookup; vtable +0x38/+0x40 | Ghidra proxies + vtable words | agrees, CONFIRMED |
| 11 | Skip mask 0x11C20203000; no save-game-flag dependence; differs-from-default rule | Ghidra `ShouldSerializeValue`, `SerializeTaggedProperties`, `UObject::Serialize` | agrees, CONFIRMED (object body itself still STRONG) |
| 12 | Mac user dir = AppSupport + `/` + `MyDocumentsSubDirName` + `/`; writer/reader redirect | Ghidra `Init` + wide literals `/`, `DefaultEngine.ini`; vtable +0x18/+0xA8/+0xB0 | agrees, CONFIRMED |
| 13 | Steam Auto-Cloud rule (Windows only) | `appinfo.vdf` record of app 278360: one rule `WinMyDocuments` / path / `*.bin` / `Windows`, no macOS root | agrees, CONFIRMED |
| 14 | `remotecache.vdf` content | file | agrees on names; corrected interpretation (1.5) |
| 15 | No local ASAMU user data | directory listing | agrees, CONFIRMED |
| 16 | Story chain | all `SeqAct_ConsoleCommand` `Commands` in 12 maps | agrees, CONFIRMED (+ extra plain menu command, `AG-DarkCave` spelling) |
| 17 | `WorldInfo.Title` per map, game types, streaming | map props | agrees, CONFIRMED |
| 18 | Collectibles 5×5, checkpoint counts 1/1/24/12/17/25/28/1 | map census | agrees, CONFIRMED |
| 19 | 11 optional story keys | per-actor census of `bIsOptional`/`bParentInteractable`/`linkedParentActor` vs. src rules: 1+2+2+2+2+2 | agrees, CONFIRMED |
| 20 | Extras at 10/15/20/25 | src | agrees, CONFIRMED |
| 21 | 15 achievements, Steam ID = index + 1, 12 Kismet nodes, StarHaven default | src + map props | agrees, CONFIRMED |
| 22 | Time-trial targets, medal rule, index = level − 2, `99:99:99` | CDO + src | agrees, CONFIRMED |
| 23 | Saved-string IDs and values | linked `SeqVar_*` of all 7 writers and 6 readers | agrees, CONFIRMED |
| 24 | "Loaded" event instances 1/3/2/1, all `Index` −1 | map census + CDO | agrees, CONFIRMED |
| 25 | Settings defaults | `DefaultSettings.ini` | agrees, CONFIRMED; corrected "every change" wording |
| 26 | New Game scope | src | raised to CONFIRMED |
| 27 | Float text precision | native `%.4f` | corrected: was "two decimals" |
| 28 | `latestSaveMap` content | native `GetMapName` + layout | corrected: chapter title, not package name (STRONG) |
| 29 | Q3 duplicate achievements via Continue | src (fresh managers per map) | refuted as stated |
| 30 | Q4 checkpoint visuals never restored | src | raised to CONFIRMED |
| 31 | `SaveLevel*` inert | all package name tables, exe (3 encodings), ini; positive controls | raised to STRONG |
| 32 | Rocket-boot census | map props + CDO default `Enable` = true | agrees, CONFIRMED |
| 33 | Finished flag set by the Epilogue intro | `SeqAct_SetGameFinished_0` driven on its "True" input | agrees, CONFIRMED |

Residual uncertainty: everything in 3.4 (object body) and 4.x property presence is still unchecked against
a real file. That includes the NetIndex value, enum-as-name arrays, UTF-16 strings and the omission of
`LevelTargetScores`. The `Title` identification of `AWorldInfo+0x730` rests on a computed layout (STRONG,
not CONFIRMED).
