# Windows binary — original Win32 executable

Target: `<install>/Binaries/Win32/ASAMU-Win32-Shipping.exe`, Steam build 1822049, 42,971,136 bytes,
SHA-256 `17f2aeb601086089f4cbf144d2eb2b4b672c2d0594ad81f8f75ba27736d26e4e` (the copy analysed locally and the
file in the Windows install have the same hash).

This file covers what a **memory-reading trace recorder** needs from the Windows build
([TRACE_CAPTURE.md](../TRACE_CAPTURE.md) section 6): how the image is loaded, where the engine globals and the
frame boundary are, the size and vtable of every native class, and the struct offsets that instructions show
directly. Field layouts of whole classes are not derived here (the layout stage does that with the
[DEFAULTS.md](DEFAULTS.md) rules in a Win32 mode and checks them against the class sizes below).

Everything was read statically from the executable. **The game was not run for this analysis**; a live check
against the running game followed on 2026-10-10 (section 8). The executable has no symbols, so every name below is assigned by us from a string
literal, an export, or the position of a call or reference that matches the symbolised Mac build
([BINARY_ANALYSIS.md](BINARY_ANALYSIS.md)). Only addresses, sizes, names, counts and a few 2–10 byte
instruction encodings are published; string contents appear only as the short anchors the tools search for.

A second, independent pass re-derived the globals, the frame-boundary functions, the class sizes, 16 vtables and
the field offsets by other routes (section 12). It confirmed every address and size in this file, found one wrong
offset in the first version of section 7 (the object index, now corrected) and one wrong check in the live checker
(section 8).

Addresses are **RVAs** (relative virtual addresses). Runtime address = module base + RVA. Ghidra shows
preferred-base addresses = `0x00400000` + RVA.

## Most important findings

| # | Finding | Confidence |
|---|---|---|
| 1 | PE32 i386 GUI executable, image base `0x00400000`, **`DYNAMICBASE` set with 1,247,113 base relocations**: the recorder must add the module base it reads at run time. Linker 10.0, timestamp 2017-05-15 08:50:35 UTC. | CONFIRMED |
| 2 | **Not packed, no Steam DRM wrapper**: five ordinary sections, `.text` entropy 6.71 bits/byte, no `.bind` section, no overlay. It imports `steam_api.dll` (24 functions, including `SteamAPI_RestartAppIfNecessary`). | CONFIRMED (absence of a wrapper section, entropy, imports); "no DRM" STRONG |
| 3 | It is a **mixed-mode (C++/CLI) image**: the entry point is `jmp [mscoree.dll!_CorExeMain]` and there is a CLR header (metadata `v4.0.30319`). It also links the **editor** (wxWidgets imports, 436 `UnrealEd` classes). Native code is ordinary x86 in `.text`. | CONFIRMED |
| 4 | **TCHAR is 2-byte UTF-16**: 45,981 UTF-16 literals in `.rdata`, every native class name among them; wide name entries are compared with `_wcsicmp`. | CONFIRMED |
| 5 | **MSVC RTTI is absent for UObject classes.** Only 200 type descriptors exist (FaceFX, PhysX wrappers, wxWidgets and editor window classes, `std::exception`, a few helper types). Of the 1,982 native classes only `UObject` and `UObjectSerializer` have a type descriptor, and neither has a vtable locator. Vtables are found through the class constructors instead (section 5). | CONFIRMED |
| 6 | **1,982 native classes** register through 2,103 `UClass` static-constructor calls. 1,533 of the Mac build's 1,535 classes are present (missing: `UALAudioDevice`, `UMacClient`); 449 are Windows-only (436 `UnrealEd`). Class flags, cast flags, super and within equal the Mac build's for all 1,526 classes compared; every Win32 `sizeof` is smaller than the Mac one. | CONFIRMED |
| 7 | All recorder globals are located, each from two or more functions: `GEngine`, `GWorld`, `GFrameCounter`, `GDeltaTime`, `GFixedDeltaTime`, `GIsBenchmarking`, `GUseFixedTimeStep`, `FName::Names`, `UObject::GObjObjects` (section 4). The verification pass found each of the nine again in functions the first pass did not use (section 12.1). | CONFIRMED (static); live check pending |
| 8 | **The frame order equals the Mac order** (TRACE_CAPTURE E15): time update → `GEngine->Tick` → inside it `Client->Tick`, `UObject::StaticTick`, one `UWorld::Tick` → `GFrameCounter++` → message pump. `UWorld::Tick` is at RVA `0x00635450` (thiscall: `ecx` = world, `[esp+4]` = tick type, `[esp+8]` = `float DeltaSeconds`). | CONFIRMED |
| 9 | `-BENCHMARK` and `-FPS=` work as on the Mac: `FEngineLoop::PreInit` stores `ParseParam(..., "BENCHMARK")` to `GIsBenchmarking`; `FEngineLoop::Init` stores `1 / FPS` to `GFixedDeltaTime`; the time update copies `GFixedDeltaTime` to `GDeltaTime` when `GIsBenchmarking \|\| GUseFixedTimeStep`. Nothing stores to `GUseFixedTimeStep`. | CONFIRMED (code) |
| 10 | The linker folded identical vtables: 134 classes share 31 vtable addresses (always with a sibling, parent or child). The gameplay classes the recorder walks have unique vtables. | CONFIRMED (shared addresses); folding as the cause STRONG |

## 1. Identification

### 1.1 Header (CONFIRMED)

| Field | Value |
|---|---|
| Format | PE32, machine `0x14C` (i386), GUI subsystem (2), subsystem and OS version 5.1 |
| Image base / size of image | `0x00400000` / 44,654,592 bytes |
| Characteristics | `0x0122`: executable, large-address-aware, 32-bit machine; relocations **not** stripped |
| DLL characteristics | `0x8140`: `DYNAMIC_BASE`, `NX_COMPAT`, `TERMINAL_SERVER_AWARE` |
| Base relocations | 1,247,113 `HIGHLOW` entries in 8,832 blocks |
| Entry point | RVA `0x01B8CCC2`: `jmp dword ptr [IAT: mscoree.dll!_CorExeMain]` |
| Linker version | 10.0 |
| Time stamp | `0x59196BDB` = 2017-05-15 08:50:35 UTC (the Mac objects date from 2017-04-24 … 05-09) |
| TLS directory | none |
| Load config | security cookie present; SafeSEH table with 14,696 handlers |
| Debug directory | one CodeView `RSDS` record naming `ASAMU-Win32-Shipping.pdb` (age 1); **no PDB ships** with the game. The build-machine directory in that record is not reproduced here |

| Section | RVA | Virtual size | Raw size | Flags | Entropy (bits/byte) |
|---|---|---:|---:|---|---:|
| `.text` | `0x00001000` | 31,258,630 | 31,259,136 | code, execute, read | 6.707 |
| `.rdata` | `0x01DD1000` | 8,213,714 | 8,214,016 | initialised data, read | 5.313 |
| `.data` | `0x025A7000` | 1,966,392 | 296,960 | initialised data, read, write (1,669,432 bytes are zero-fill) | 4.693 |
| `.rsrc` | `0x02788000` | 17,568 | 17,920 | initialised data, read | 4.987 |
| `.reloc` | `0x0278D000` | 3,181,886 | 3,182,080 | initialised data, discardable, read | 6.021 |

The last section ends exactly at the end of the file (no overlay).

### 1.2 Protection (CONFIRMED observations; STRONG conclusion)

- No `.bind` section (the Steam DRM wrapper's section), no unusual section names, no high-entropy section, no
  overlay, and the entry point is the standard CLR stub. The code is plain: 2,103 class registrations and all
  the functions below are readable where they sit in the file.
- `steam_api.dll` is imported (24 functions: `SteamAPI_Init`, `SteamAPI_RestartAppIfNecessary`,
  `SteamAPI_RunCallbacks`, user/stats/remote-storage/game-server accessors). The install also carries
  `steam_appid.txt` next to the executable.
- `KERNEL32!IsDebuggerPresent` (4 references) and `OutputDebugStringW` (3 references) are imported, as in every
  UE3 Windows build. What the game does differently under a debugger was not examined (UNKNOWN); a plain
  `ReadProcessMemory` reader is not a debugger.
- No anti-cheat component is in the install's `Binaries/Win32` folder (54 files: the executable, a console
  launcher stub, PhysX/APEX, wxWidgets, Vorbis, Steam and tool DLLs).

### 1.3 Toolchain

| Evidence | Reading | Confidence |
|---|---|---|
| Optional header linker version 10.0; imports `MSVCR100.dll` (296 functions) and `MSVCP100.dll` (61) | Visual C++ 2010 toolchain and runtime | CONFIRMED |
| Rich header: 882 objects with tool id 171 build 40219, 41 with id 170 build 40219, linker record id 157 build 40219 | main build compiled with the Visual C++ 2010 SP1 compiler (16.00.40219) | STRONG (the usual reading of those ids) |
| Rich header: further records with builds 21022 / 30729 (Visual C++ 2008), 50727 (2005), 3077 (2003) | prebuilt third-party libraries from older compilers, as on the Mac | STRONG |
| CLR header: runtime header 2.5, metadata `v4.0.30319`, flags 0 (not IL-only), managed entry token `0x06000225`; import `mscoree.dll!_CorExeMain` | mixed-mode assembly started through the .NET 4 runtime | CONFIRMED |
| Imports of seven `wxmsw28u_*_vc_custom.dll` libraries (2,710 functions), `nvtt.dll`, `dbghelp.dll`, `EasyHook32.dll`; 449 Windows-only classes | the "shipping" executable is the UDK-style build with the editor compiled in | CONFIRMED |
| 44 import DLLs with 3,583 functions; delay imports `d3d11.dll`, `dxgi.dll`, `OPENGL32.dll`, `PhysXLoader.dll`; 24 exports (Scaleform helpers plus `GDebugger`, `GetOutermost`, `GetStackOwnerClass`, `PIBGetInterface`, `nFringePFO`) | Direct3D 9/11 renderer, XAudio2/X3DAudio, DirectInput/XInput; the five named exports are the script-debugger interface | CONFIRMED (tables); TENTATIVE (purpose of the exports) |

### 1.4 TCHAR (CONFIRMED)

- `.rdata` holds 45,981 UTF-16LE literals of four or more printable units. Only 21 runs look like UTF-32; they
  are consecutive one-character UTF-16 literals or small integer tables.
- All 1,982 class-name literals passed to the class registrations are UTF-16.
- The name table's wide entries are compared with `MSVCR100!_wcsicmp` (section 7).

So `FString` data and wide `FNameEntry` characters are **2 bytes per character** (the Mac build uses 4).

## 2. Method

Tool: [`tools/ghidra-scripts/win32/win32_scan.py`](../../tools/ghidra-scripts/win32/win32_scan.py) (our code;
Python standard library plus an LLVM `objdump` for PE/i386 disassembly). It hard-codes no address. Every
address comes from a string literal, an export, or an address already found, and each step asserts what it
expects (87 checks; the run fails if one breaks). Its output is the JSON in
[`data/win32/`](data/win32/); `--check` regenerates and compares (byte-identical).

| Anchor in the Windows file | Leads to | Mac counterpart used to read it |
|---|---|---|
| UTF-16 literal `UASAMUSystemSettingsManager` (+1 character) pushed once | the `UClass` static constructor, then all 2,103 registration calls | `<Class>::GetPrivateStaticClass<Class>` (`IMPLEMENT_CLASS`) |
| literal `BENCHMARK` (pushed once) | `FEngineLoop::PreInit`, `GIsBenchmarking` | `FEngineLoop::PreInit` references `GIsBenchmarking` |
| literal `FPS=` (pushed once) | `FEngineLoop::Init`, `GEngine`, `GFixedDeltaTime` | `FEngineLoop::Init` writes `GFixedDeltaTime` |
| the function reading both and copying `GFixedDeltaTime` | `appUpdateTimeAndHandleMaxTickRate`, `GDeltaTime`, `GUseFixedTimeStep`, `GCurrentTime`, `GLastTime` | same function, same statements |
| its caller that increments a 64-bit counter | `FEngineLoop::Tick`, `GFrameCounter`, the vtable slot of `GEngine->Tick` | `FEngineLoop::Tick` |
| that slot in `UGameEngine`'s vtable | `UGameEngine::Tick`, `GWorld`, `UWorld::Tick`, `Engine.Client`, `Engine.GamePlayers` | `UGameEngine::Tick` call order (TRACE_CAPTURE E15) |
| literal starting `Hardcoded name` | `FName::Names` | `FName` hard-coded name registration |
| configuration key literal `MaxObjectsNotConsideredByGC` | `UObject::StaticInit`, `UObject::GObjObjects`, `GObjFirstGCIndex` | `UObject::StaticInit` |
| export `GDebugger`, export `GetOutermost` | cross-check of `UGameEngine::Tick`; `UObject.Outer` offset | `_GDebugger`, `UObject::GetOutermost` |

Independent cross-checks:

- **Mac build:** class flags, cast flags, super and within of all 1,526 classes parsed on both sides are
  equal; names match 1,533 of 1,535; reference patterns of the globals match (for example `GUseFixedTimeStep`
  has exactly three readers, each of which also reads `GIsBenchmarking`, and no writer).
- **Ghidra** (headless analysis of the same file, script
  [`ApplyWin32Names.java`](../../tools/ghidra-scripts/ApplyWin32Names.java)): Ghidra has a function starting
  at each of the 21 function addresses; it finds 2,103 call references to the `UClass` static constructor (the
  table has 2,103); the first slot of all 1,982 vtables is code; all 1,982 `PrivateStaticClass` addresses are
  referenced.

## 3. Native class registrations

### 3.1 Pattern (CONFIRMED)

`IMPLEMENT_CLASS` expands to a function that allocates `sizeof(UClass)` = **0x1C8** bytes and calls one shared
constructor with twelve stack arguments (`this` in `ecx`):

```text
UClass::UClass(EC_StaticConstructor = 0, sizeof(Class), ClassFlags, CastFlags, Name, Package, ConfigName,
               ObjectFlags (64-bit: high 0x04084084, low 0x00004000),
               InternalConstructor, StaticConstructor, InitializeIntrinsicPropertyValues)
```

- The constructor is at RVA `0x001AAC50`; it has 2,103 direct callers. Each call site is parsed backwards from
  the call (twelve `push` instructions; the two fixed object-flag words and three code addresses make the parse
  unique). 121 of the sites are inlined copies of a class's `StaticClass()`; copies of one class agree on every
  argument.
- `Name` points one character into the class-name literal (`TEXT("APawn") + 1`), and twelve characters in for
  the nine classes flagged `CLASS_Deprecated` (`UDEPRECATED_…`).
- `Package` is the argument of the out-of-line getter; it is read from the getter's callers
  (`push <literal>; call getter; mov [PrivateStaticClass], eax; call Initialize…`). The same pattern gives the
  address of the static `UClass* <Class>::PrivateStaticClass`.
- `Initialize<Class>PrivateStaticClass` pushes `(Within::StaticClass(), PrivateStaticClass, Super::StaticClass())`
  and calls a shared function (RVA `0x00181D70`). Tracking which `PrivateStaticClass` variables it loads gives
  **super** and **within** for every class. Every class's own pointer appears in the middle slot, `UObject` is
  the only root, and each class's vtable repeats more than half of its super's first 60 slots. The same three
  arguments read from the Mac build's `Initialize<Class>PrivateStaticClass` functions (named symbols) are equal
  for all 1,526 classes parsed there. (CONFIRMED for those classes; STRONG for the Windows-only ones)

### 3.2 Result

[`data/win32/class_sizes.json`](data/win32/class_sizes.json): one row per class with `package`, `name`,
`cpp_name`, `size`, `super`, `within`, `class_flags`, `vtable_rva`, `private_static_class_rva` (decimal
numbers, columnar).

| Package | Classes | | Package | Classes |
|---|---:|---|---|---:|
| `Engine` | 1,264 | | `OnlineSubsystemSteamworks` | 6 |
| `UnrealEd` | 436 | | `WinDrv` | 6 |
| `UDKBase` | 100 | | `GFxUIEditor` | 4 |
| `GameFramework` | 70 | | `UTEditor` | 2 |
| `Core` | 44 | | `XAudio2` | 1 |
| `IpDrv` | 32 | | `asamu` | 1 (`UASAMUSystemSettingsManager`, 0x40 bytes) |
| `GFxUI` | 16 | | **Total** | **1,982** |

Comparison with the Mac list (`nm … | grep 18PrivateStaticClassE`, 1,535 names): 1,533 common; Mac-only
`UALAudioDevice`, `UMacClient`; Windows-only 449 = 436 `UnrealEd` + `WinDrv` (`UWindowsClient`,
`UXnaForceFeedbackManager`, `UFacebookWindows`, `UHttpRequestWindows`, `UHttpResponseWindows`,
`USwrveAnalyticsWindows`) + `GFxUIEditor` 4 + `UTEditor` 2 + `XAudio2` 1. (CONFIRMED)

Sizes of the classes on the player path (bytes; Mac x86_64 values for comparison, read from the Mac
registration functions the same way, [DEFAULTS.md](DEFAULTS.md) §2):

| Class | Win32 `sizeof` | Mac `sizeof` | | Class | Win32 `sizeof` | Mac `sizeof` |
|---|---:|---:|---|---|---:|---:|
| `UObject` | 0x03C | 0x060 | | `AInventory` | 0x218 | 0x2C0 |
| `AActor` | 0x1CC | 0x248 | | `AWeapon` | 0x2BC | 0x398 |
| `APawn` | 0x454 | 0x590 | | `AUDKWeapon` | 0x2DC | 0x3C0 |
| `AUDKPawn` | 0x628 | 0x7E0 | | `UInput` | 0x118 | 0x180 |
| `AController` | 0x350 | 0x448 | | `UPlayerInput` | 0x260 | 0x2C8 |
| `APlayerController` | 0x588 | 0x760 | | `ACamera` | 0x4A8 | 0x598 |
| `AUDKPlayerController` | 0x870 | 0xA88 | | `AWorldInfo` | 0x858 | 0xAC0 |
| `UEngine` / `UGameEngine` | 0x638 / 0x79C | 0x8A8 / 0xA98 | | `APhysicsVolume` | 0x238 | 0x2D8 |
| `UPlayer` / `ULocalPlayer` | 0x060 / 0x3A4 | 0x090 / 0x450 | | `UClass` | 0x1C8 | — |

"—" = not read on the Mac side. The Win32 sizes are the check values for the Win32 layout rules, exactly as
the Mac sizes were for the 64-bit rules.

## 4. Globals

[`data/win32/globals.json`](data/win32/globals.json) (name, RVA, type, size, how found, cross-checks,
confidence). `tarray` = `{data pointer +0, i32 count +4, i32 max +8}` (12 bytes).

| Global | RVA | Type | Found in | Cross-checked by | Confidence |
|---|---|---|---|---|---|
| `GEngine` | `0x02708908` | pointer | `FEngineLoop::Init`: loaded after the `FPS=` parse to store the capture rate | `FEngineLoop::Tick` calls its vtable with `(float)GDeltaTime`; `appUpdateTimeAndHandleMaxTickRate` asks it for the tick rate; `UWorld::Tick` reads its `GamePlayers` | CONFIRMED |
| `GWorld` | `0x0270C490` | pointer | `UGameEngine::Tick`: `this` of the single `Tick(LEVELTICK_All = 2, DeltaSeconds)` call | first call of the same function (`GetGameInfo`, after the `GForceLowGore` test, as on the Mac); `UGameEngine::LoadMap` | CONFIRMED |
| `GFrameCounter` | `0x0264FA20` | u64 | `FEngineLoop::Tick`: 64-bit `+1` after `GEngine->Tick` | frame-limit compare at the top of the same function; read by `FNxContactReport::onContactNotify` and `PrintOutSkelMeshLODs` (as on the Mac); only `FEngineLoop::Tick` writes it | CONFIRMED |
| `GDeltaTime` | `0x025A7E70` | f64 | `appUpdateTimeAndHandleMaxTickRate`: receives `GFixedDeltaTime` | `FEngineLoop::Tick` narrows it to `float` for every `GEngine` call; only that one function writes it | CONFIRMED |
| `GFixedDeltaTime` | `0x025A7E68` | f64 | `FEngineLoop::Init`: receives `1 / FPS` | copied to `GDeltaTime` by the time update; file value 1/30 | CONFIRMED |
| `GIsBenchmarking` | `0x0264718C` | u32 | `FEngineLoop::PreInit`: receives the `BENCHMARK` switch | first test of the time update; frame-limit test of `FEngineLoop::Tick` | CONFIRMED |
| `GUseFixedTimeStep` | `0x0264FA64` | u32 | time update: second term of `GIsBenchmarking \|\| GUseFixedTimeStep` | exactly three readers, each also reads `GIsBenchmarking`; one is `DrawUnitTimes`; **no writer** (4 references, all loads) | CONFIRMED |
| `FName::Names` | `0x026AF4B0` | tarray | hard-coded-name registration (literal starting `Hardcoded name`) | `FShaderParameter::Bind` and `FShaderResourceParameter::Bind` index it (as on the Mac); a validity accessor shows `{data, count}` | CONFIRMED |
| `UObject::GObjObjects` | `0x026F17C8` | tarray | `UObject::StaticInit`: presized right after `GObjFirstGCIndex` is set | `UObject::StaticExit` walks it; the same `StaticInit` uses `UPackage::PrivateStaticClass` at the address the class scan found | CONFIRMED |
| `UObject::GObjFirstGCIndex` | `0x026F178C` | i32 | `UObject::StaticInit` | `UObject::AddObject` compares a new object's index with it before clearing the object's "not collected" flag, as on the Mac | CONFIRMED |
| `GCurrentTime` / `GLastTime` | `0x0264F9B8` / `0x0264F9C0` | f64 | time update (`GLastTime = GCurrentTime`) | — | STRONG |
| `GDebugger` | `0x02647180` | pointer | export table | used by `UGameEngine::Tick` where the Mac build uses `GDebugger` | CONFIRMED |
| `GSeamlessTravelHandler` | `0x0270C570` | struct | `UGameEngine::Tick`, between `StaticTick` and `UWorld::Tick` | — | STRONG |
| `GMalloc` | `0x0263F0DC` | pointer | three class registrations allocate through its vtable | — | STRONG |

Notes:

- `GFixedDeltaTime` is initialised to the **double** 1/30 (`0x3FA1111111111111`) in this file; the Mac file
  holds the `float` 1/30 widened. With `-FPS=n` both builds store a `float` division widened to `double`
  (`divss` then `cvtps2pd` here). (CONFIRMED)
- `GDeltaTime`, `GFixedDeltaTime` sit in the initialised part of `.data`; the others are in its zero-fill part.
- Outside the launch code, four functions write `GIsBenchmarking` and three of them also write
  `GFixedDeltaTime` (RVAs `0x0120D9E0`, `0x0121AE20`, `0x0121C470`, `0x01252B40`). They belong to the Matinee
  editor (`UnrealEd`), which the Mac build does not contain: `0x0121C470` saves the configuration key
  `FixedTimeStepPlayback` of section `Matinee` and then stores 1 or 0 to `GIsBenchmarking` and a `float`
  widened to `double` to `GFixedDeltaTime`; `0x0120D9E0` uses the `Matinee` / `SplitterPos` keys and clears the
  flag; the other two repeat the same two stores without literals. This is the stock UE3 "fixed time step
  playback" option of the Matinee window. (CONFIRMED for `0x0121C470` and `0x0120D9E0`, which use the
  literals; STRONG for the other two; that none of them is reachable in the game is STRONG — the editor is not
  started by the game.) These functions are also independent evidence for what the two globals mean.
- `GObjObjects` has 2,322 code references in 519 functions (the Mac build: 171 functions). The object
  iterators are inlined here and the editor code iterates objects in many places. (CONFIRMED counts; STRONG
  reason)

## 5. RTTI and vtables

### 5.1 RTTI (CONFIRMED)

The MSVC RTTI scan (`.?A…` type descriptors → complete-object locators → vtables; `msvc_rtti` in
[`data/win32/image.json`](data/win32/image.json)) finds 200 type descriptors, 157 locators and 119 vtables.
They belong to third-party and editor code and a few helper types: FaceFX (`OC3Ent::Face::Fx…`), PhysX wrapper
classes (`Nx…`, `Nxd…`), DirectShow base classes, wxWidgets windows, editor panels, Swarm, `FOutputDevice`,
`FRenderCommand`, `std::exception`, `std::bad_alloc`, `std::bad_cast`, `type_info`. The engine is compiled
without RTTI, as on the Mac. Two native classes have a type descriptor (`UObject`, `UObjectSerializer`), but no
locator and so no vtable link. **RTTI cannot locate or validate engine objects on this build.**

### 5.2 Vtables from constructors (STRONG)

The registration passes `<Class>::InternalConstructor`. That function (or the constructor it jumps to) stores the
vtable pointers into the new object; the last store to offset 0 is the class's own primary vtable, stores to
other offsets are interface sub-objects (`FExec` at +0x3C in `UEngine`/`UPlayer`/`UWorld`, an interface at
+0x1CC in pawns and controllers). The scanner follows this for all 1,982 classes.

- MSVC layout: the object's first word points at slot 0. There are no offset-to-top or typeinfo words.
- Slot numbers differ from the Mac build (one destructor slot instead of two, and editor virtuals). Example:
  `UEngine::Tick` is slot 77 (+0x134) here and slot 76 (+0x260) on the Mac, while the two calls before it in
  `FEngineLoop::Tick` are slots 84/85 on both.
- **Shared vtables.** 134 classes share 31 vtable addresses (for example `AInfo`, `ARigidBodyBase`,
  `ACrowdPopulationManagerBase`, `AGameCrowdSpawnRelativeActor`). In every group the classes are siblings or
  parent and child, so the tables are byte-identical and were merged by the linker (STRONG). A vtable pointer
  therefore identifies a class only up to such a group; the `Class` pointer (compare with
  `*PrivateStaticClass`) is exact.
- Script-only classes carry their nearest native ancestor's vtable ([BINARY_ANALYSIS.md](BINARY_ANALYSIS.md)
  §4.4, CONFIRMED on the Mac; the same `UClass::Bind` logic is assumed here, STRONG).

- **Independent check of 16 vtables (15 classes).** The constructor route names a vtable by the class that
  registers the constructor. The verification pass named vtables the other way round: a virtual function that a
  class overrides is found by UTF-16 literals that only it uses (the same literals as the symbolised Mac
  function), and the vtables that hold that function are listed. For the primary vtables of `UObject`,
  `AActor`, `APawn`, `AUDKPawn`, `AInventory`, `AUDKWeapon`, `APlayerController`, `AUDKPlayerController`,
  `AWorldInfo`, `UEngine`, `UGameEngine` and `UInput` the function sits in the vtable listed here (and
  otherwise only in vtables of subclasses); for `UWorld`, `ULocalPlayer`, `UGameViewportClient` and
  `UGameEngine` the same holds for the secondary (interface) vtables. Section 12.2 has the list. (CONFIRMED for
  those vtables; the others stay STRONG.)

[`data/win32/vtables.json`](data/win32/vtables.json) lists 36 key classes with secondary vtables, sharers,
`PrivateStaticClass` and `InternalConstructor` addresses; all classes are in `class_sizes.json`.

| Class (script classes that use it) | Vtable RVA | `PrivateStaticClass` RVA | Unique |
|---|---|---|---|
| `UObject` | `0x01E5AD38` | `0x026F17AC` | shared with `UObjectSerializer` |
| `UClass` | `0x01E4C540` | `0x02666470` | yes |
| `UWorld` | `0x01F991E8` | `0x0270C48C` | yes |
| `AWorldInfo` | `0x01EE85B0` | `0x026FD604` | yes |
| `UGameEngine` | `0x01F32A50` | `0x027089A4` | yes |
| `ULocalPlayer` | `0x01F61918` | `0x0270B764` | yes |
| `UWindowsClient` | `0x021FBEF0` | `0x0274A9E4` | yes |
| `APlayerController` | `0x01EE7258` | `0x026FD6A4` | yes |
| `AUDKPlayerController` (`ASAMUPlayerController`) | `0x0221E990` | `0x027514D0` | yes |
| `APawn` | `0x01EDD188` | `0x0270B5BC` | yes |
| `AUDKPawn` (`ASAMUPawn`) | `0x02219FA0` | `0x027514CC` | yes |
| `AUDKWeapon` (`GrappleGun`) | `0x02216B80` | `0x02751578` | yes |
| `UPlayerInput` (`ASAMUPlayerInput`) | `0x01FAD470` | `0x0270C9AC` | yes |
| `ACamera` (`ASAMUCamera`) | `0x01F02BC8` | `0x02703F08` | yes |
| `APhysicsVolume` / `ADefaultPhysicsVolume` | `0x01ED7CB0` / `0x01ED8098` | `0x026FD6C8` / `0x026FD6D0` | yes |
| `AFrameworkGame` (`ASAMUGameInfo`) | `0x0200A530` | `0x027288F4` | yes |
| `UASAMUSystemSettingsManager` | `0x02222B20` | `0x02751AE0` | yes |

## 6. Frame structure

Functions ([`data/win32/functions.json`](data/win32/functions.json)):

| Function | RVA | How found | Confidence |
|---|---|---|---|
| `FEngineLoop::Tick` | `0x014A8CC0` | caller of the time update that increments `GFrameCounter` | CONFIRMED |
| `appUpdateTimeAndHandleMaxTickRate` | `0x014A6820` | reads `GIsBenchmarking`, `GUseFixedTimeStep`; `GDeltaTime = GFixedDeltaTime` | CONFIRMED |
| `UGameEngine::Tick` | `0x00589720` | slot +0x134 of `UGameEngine`'s vtable, the slot `FEngineLoop::Tick` calls last before the increment; thiscall, `ret 4` | CONFIRMED |
| **`UWorld::Tick`** | **`0x00635450`** | the only `GWorld->f(2, DeltaSeconds)` call of `UGameEngine::Tick`; starts with `GetWorldInfo(0)` and the `GEngine->GamePlayers` loop like the Mac function; thiscall, `ret 8`; 3 direct call sites: one in `UGameEngine::Tick`, two in `UEditorEngine::Tick` (the same slot +0x134 of `UEditorEngine`'s vtable, RVA `0x010323A0`, not on the game's path) | CONFIRMED |
| `UWindowsClient::Tick` | `0x014536D0` | slot +0x138 of `UWindowsClient`'s vtable, the slot `UGameEngine::Tick` calls on `Engine.Client` | STRONG |
| `appWinPumpMessages` | `0x0021D860` | called after the increment; `PeekMessageW` loop | STRONG |
| `UObject::StaticTick`, `FSeamlessTravelHandler::Tick`, `UWorld::GetGameInfo`, `UWorld::IsPaused` | `0x001EDC40`, `0x008AF8F0`, `0x008A6300`, `0x00618DD0` | position in the Mac call order (`UWorld::IsPaused` also reads the same five `WorldInfo` members as the Mac function) | STRONG |
| `UWorld::GetWorldInfo` | `0x008A3370` | position in the Mac call order; the exec thunk `UEngine::execGetCurrentWorldInfo` (name table) calls it with `GWorld` as `this`, as on the Mac | CONFIRMED |
| `FEngineLoop::PreInit`, `FEngineLoop::Init` | `0x014ABE70`, `0x014AA180` | functions holding the `BENCHMARK` / `FPS=` parse | STRONG (function starts) |

`UWorld::Tick` and `UGameEngine::Tick` were found a second time without the frame loop: `UWorld::Tick` is one
of two functions that use the literals `ConnectionFailed_Title` and `?closed` (the other, `0x008A6FF0`, is
`UWorld::NotifyControlMessage` with its `NMT_` literals) and uses the same three literals as the Mac function
(`ConnectionFailed`, `ConnectionFailed_Title`, `?closed`);
`UGameEngine::Tick` is the only function that uses `Negative delta time!` and `All Windows Closed`, as on the
Mac, and sits in slot 77 of `UGameEngine`'s vtable. (CONFIRMED)

Order of one frame (CONFIRMED from the instruction order; identical to the Mac order in TRACE_CAPTURE E15):

```text
FEngineLoop::Tick
  frame-limit test (GIsBenchmarking && MaxFrameCounter && GFrameCounter > MaxFrameCounter)
  appUpdateTimeAndHandleMaxTickRate()                    writes GDeltaTime (fixed step: = GFixedDeltaTime, no wait)
  GEngine vtable +0x150, +0x154 ((float)GDeltaTime)      (slots 84/85, as on the Mac)
  GEngine->Tick((float)GDeltaTime)                       vtable +0x134 = UGameEngine::Tick
    Engine.Client->Tick(DeltaSeconds)                    Engine+0x4B0, vtable +0x138 = UWindowsClient::Tick (viewport input)
    UObject::StaticTick(DeltaSeconds)
    GSeamlessTravelHandler.Tick()                        when a travel is pending
    GWorld->Tick(LEVELTICK_All, DeltaSeconds)            UWorld::Tick, exactly one direct call
      WorldInfo.RealTimeSeconds += dt; AudioTimeSeconds += dt (not paused)
      dt *= TimeDilation (clamped); WorldInfo.DeltaSeconds = dt; TimeSeconds += dt (not paused)
      ... tick groups, actors, camera ...
    loop over Engine.GamePlayers (Engine+0x4B4), viewport and remaining engine work
  one more virtual call with GDeltaTime on another global object
  GFrameCounter += 1                                     RVA 0x014A8F11
  ... end-of-frame work ...
  appWinPumpMessages()                                   RVA 0x014A907F (call site)
```

Consequences for a recorder:

- **Breakpoint recorder (exact, same sample point as the Mac recorder).** Entry of `UWorld::Tick` (module base
  + `0x00635450`) is reached once per frame, after the frame's input dispatch and before any actor ticks.
  At entry `ecx` is the `UWorld` (compare with `GWorld`), `[esp+4]` the tick type and **`[esp+8]` the
  `float DeltaSeconds`** (the Mac recorder reads it from `xmm0`). `GFrameCounter` still holds the previous
  frame's increment at that point, as on the Mac. (CONFIRMED)
- **Polling recorder (no debugger).** `GFrameCounter` changes only after `UGameEngine::Tick` returns, and the
  next change of world state begins with the `RealTimeSeconds` store at the top of the next `UWorld::Tick`.
  The state between those two events is the finished frame's state. A reader that has seen the pair
  (`GFrameCounter`, `WorldInfo.RealTimeSeconds`) go from (n, r) to (n+1, r), reads its fields, and then still
  sees (n+1, r) has a consistent post-tick sample of frame n. A sample whose pair was first seen as (n+1, r′)
  with r′ ≠ r may be mid-tick and must be dropped. While paused `RealTimeSeconds` still advances
  (`TimeSeconds` does not). A poller has no `dt_arg`: `GDeltaTime` read in that window is the finished frame's
  step, not the coming one's. (CONFIRMED order; the rule is our derivation from it)
- The polling window differs from the Mac sample point in one respect: input for the next frame is delivered
  by the message pump at the end of `FEngineLoop::Tick` and by `Client->Tick` at the start of the next engine
  tick, both inside that window. `PressedKeys`, `bPressedJump` and anything a key binding executes
  immediately can therefore belong to either side of a polled sample, while a breakpoint at `UWorld::Tick`
  always sees them applied. (STRONG: follows from the order; the input paths inside `UWindowsClient::Tick`
  and the window procedure were not traced)
- In fixed-step mode the time update does not wait, so the window can be short; where the game thread waits
  for the renderer was not located on this build (UNKNOWN).

## 7. Struct layout shown by instructions

[`data/win32/native_evidence.json`](data/win32/native_evidence.json) records each item with its function, RVA
and instruction bytes; the scanner asserts the bytes are there.

| Fact | Evidence | Confidence |
|---|---|---|
| Pointers are 4 bytes; `TArray` = `{data +0, count +4, max +8}`, 12 bytes | `FName::Names` users; the `GamePlayers` loop (`cmp [esi+0x4B8]`, `mov eax,[esi+0x4B4]`, `[eax+edi*4]`) | CONFIRMED |
| `FName.Index` is the first word of the 8-byte `FName` | a validity test loads `[this]`, compares with `Names.count`, tests `Names.data[index*4]` | CONFIRMED |
| `FNameEntry`: 64-bit flags at +0, index word at +8 with **bit 0 = wide**, characters at **+0x10** for both forms, wide characters 2 bytes | name lookup: `test byte ptr [esi+8],1`, then `lea eax,[esi+0x10]` (wide, compared with `_wcsicmp`) or `add esi,0x10` (ANSI, widened first); a log helper tests bit 0x1000 of the flags' high half on `Names[760]` | CONFIRMED (index, wide flag, characters); STRONG (flags) |
| `FName.Number` is the second word (+4); a name with number n > 0 prints as `Name_<n−1>` | `FName::AppendString` (reached by direct calls from `FName::SafeString`, the only user of the literal `*INVALID*`): `cmp dword ptr [edi+4],0`, then it appends `_` and the number minus 1 | CONFIRMED |
| Hard-coded name index of `Log` is 760 | same log helper (`Names.data + 0xBE0`) | STRONG (live check: `Names[760] == "Log"`) |
| `sizeof(UObject)` = 0x3C; `Outer` at **+0x28** | registration; the loop of the exported `GetOutermost` (`mov eax,[edi+0x28]`) | CONFIRMED |
| `Name` at +0x2C, `Class` at +0x34, `ObjectArchetype` at +0x38 | `UObject::execIsA`, `UObject::execGetFuncName`, `AActor::execAllActors` (name table) read `Name` and `Class` there; the Mac member order with 4-byte pointers fills 0x3C exactly | CONFIRMED (`Name`, `Class`); STRONG (`ObjectArchetype`) |
| **Object index at +0x20** (`ObjectInternalInteger`, the object's slot in `GObjObjects`; −1 = none); `HashNext` at +0x04; object flags (64-bit) at +0x08; `HashOuterNext` at +0x10 | `UObject::AddObject` (the last direct call of `UObject::Register`): `mov [GObjObjects.data + 4*index], this`, then `mov [this+0x20], index`; it tests bit 0x80 of the flags' high half at +0x0C. `UObject::execGetFuncName` compares `[obj+0x20]` with −1 before it reads the name. The Mac function stores the index at +0x38, where the script declares `ObjectInternalInteger`. | CONFIRMED (index, flags); STRONG (the two hash links, from the script member order) |
| `Engine.Client` at +0x4B0, `Engine.GamePlayers` at +0x4B4 (count at +0x4B8) | `UGameEngine::Tick` (Mac: 0x6D0 / 0x6D8) | CONFIRMED |
| `Player.Actor` at +0x40 | `FEngineLoop::Tick` first-frame loop: `mov ecx,[eax+ebp*4]`, `mov esi,[ecx+0x40]` (Mac: 0x68); `UPlayer` is `UObject` (0x3C) plus one interface pointer | STRONG |
| `WorldInfo.TimeDilation` +0x41C, `TimeSeconds` +0x424, `RealTimeSeconds` +0x428, `AudioTimeSeconds` +0x42C, `DeltaSeconds` +0x430 | the time update at the top of `UWorld::Tick`, in source order (Mac: 0x530, 0x538, 0x53C, 0x540, 0x544 — same spacing) | CONFIRMED (four of them by their roles; `AudioTimeSeconds` STRONG) |
| `sizeof(UClass)` = 0x1C8 | allocation size in every registration; equals `UClass`'s own registered size | CONFIRMED |
| Reflection: `UField.Next` at +0x3C, `UStruct.Children` at +0x4C, **`UProperty.Offset` at +0x60**; `UProperty.ArrayDim` +0x40, `PropertyFlags` (64-bit) +0x48, `RepIndex` (16-bit) +0x52; `UClass` cast flags at +0xD0 | `UGameEngine::Init` walks a class's properties in its member-offset checks (`mov edx,[class+0x4C]`, `mov eax,[prop+0x60]`, `mov edx,[field+0x3C]`); the field iterator tests bit 0x8000 of `[field.Class+0xD0]` to pick properties; the replication functions read `[prop+0x48]` and `[prop+0x52]`. `sizeof(UField)` 0x40, `sizeof(UProperty)` 0x70 and `sizeof(UState)` 0xCC (registrations) fit. | CONFIRMED (`Next`, `Children`, `Offset`); STRONG (`PropertyFlags`, `RepIndex`, cast flags: instructions whose meaning is the stock one); TENTATIVE (`ArrayDim`: from the stock member order only) |
| `UStruct.PropertiesSize` at +0x50; `UBoolProperty.BitMask` at +0x70 | the script compiler's size-mismatch message reads `[class+0x50]`; `sizeof(UProperty)` is 0x70 and `sizeof(UBoolProperty)` 0x74 | TENTATIVE (the live check tests both) |

**Correction (verification pass).** The first version of this table gave the object index at +0x04 as a
TENTATIVE value taken from an older stock layout. On this engine version +0x04 is the `HashNext` link; the index
is the `ObjectInternalInteger` member at +0x20 (row above).

## 8. Live validation

**Status (2026-10-10).** The two read-only checkers below were run against the running game during the first
recording session. What that run showed is recorded in `docs/STATUS.md`; **its output was not kept**, so the
numbers here are that entry's, and the per-class lists behind them do not exist. Both checkers now write every
line to a log file, and `tools/trace-recorder/win_remote.sh layoutcheck` runs them and keeps the logs under
`research/local/win/live-checks/` (git-ignored: they list class and property names).

| Result of the live run (STATUS.md) | Confidence |
|---|---|
| The module is this build; the build, module and layout checks of the recorder pass | CONFIRMED (live) |
| `UProperty.Offset` equals the derived layout for **15,081 of 15,081** properties of the loaded classes; `UBoolProperty.BitMask` equals `1 << bit` for 3,226 of 3,226; the recorded class defaults of the six player classes sit at their layout offsets | CONFIRMED (live). `UBoolProperty.BitMask` at +0x70 (TENTATIVE in section 7) is thereby CONFIRMED; `UStruct.PropertiesSize` at +0x50 is STRONG (it held the registered `sizeof` for 1,803 classes, 8.1) |
| `UStruct.PropertiesSize` equals the registered `sizeof` for 1,803 of 1,954 native classes (reported as a failed check) | a wrong expectation of the checker, 8.1 |
| 630 layout fields had no live property object (reported as a failed check) | a wrong expectation of the checker, 8.2 |

### 8.1 `PropertiesSize` is the end of the last property, not `sizeof` (STRONG; the per-class list is pending)

For a class linked from script the engine stores in `PropertiesSize` the end of the last property. The C++
`sizeof` the registration passes is that end rounded up to the class's alignment (rule 6 of the layout rules,
[DEFAULTS.md](DEFAULTS.md) §9.3). The two differ whenever the last member does not end on the alignment.

Static reconciliation (`class_sizes.json` against the script layout of every class; no game needed):

| Native classes | Count |
|---|---:|
| registered | 1,982 |
| without a script class (no layout to compare) | 330 |
| with a script layout | 1,652 |
| … whose property end equals the registered `sizeof` | 1,484 |
| … whose `sizeof` is the end rounded up to 4 (the last member is a byte or ends off a word: `ACoverLink` 813 → 816, `AStaticMeshActor` 469 → 472, `UActorComponent` 85 → 88) | 38 |
| … whose `sizeof` is the end rounded up to 16 (a 16-aligned class: `AKActor` 712 → 720, `AUDKVehicle` 1940 → 1952) | 130 |
| … any other relation | 0 |

Of the 168 classes whose end is below their `sizeof`, 9 are in the editor packages, whose script the game does
not load (8.2): their class objects keep the `sizeof` the constructor set. That predicts **159** classes with
`PropertiesSize` ≠ `sizeof` (Engine 133, UDKBase 23, GameFramework 3); the live run found **151**
(1,954 − 1,803). The mechanism is STRONG (no class has an end above its `sizeof` or a tail the alignment does
not explain; and the same run matched every property offset, including first members of classes derived from
these).

**The 8 classes between 159 and 151 are not explained.** Two readings fit, and the data kept from the live run
cannot tell them apart: (a) eight of the 159 were among the 28 native classes whose `PrivateStaticClass` was
not set when the check ran (1,982 − 1,954; such a class is not visited); (b) eight linked classes hold their
`sizeof` after all, for a reason not found (30 of the 159 declare no property of their own, so "a class without
own properties" is not the answer by itself). UNKNOWN. The corrected checker decides it on its next run: it
lists by name every class that is not set (with a note when its end is below its `sizeof`) and every class
whose `PropertiesSize` is not the value expected for it, saying which of the two values it holds.

What the checkers expect now:

- `win32_props_check.py`: `PropertiesSize` = the layout's property end for a native class of a package whose
  script is loaded (a package with at least one live property), = the registered `sizeof` otherwise; and,
  statically, every such end rounds up to the `sizeof` by 4, 8 or 16. It prints how many classes hold each, how
  many ends are below the `sizeof` per alignment, and for comparison how many equal the `sizeof` (the old
  check's number).
- `win32_live_check.py` has no script layout. It accepts the `sizeof` or a smaller value that rounds up to it
  by 4, 8 or 16, counts both kinds, and lists every class that is neither.

### 8.2 The 630 "missing" fields are native classes of packages the game does not load (CONFIRMED count; STRONG that the live 630 are these)

The executable registers the native classes of every package it was built with, the editor's too, so their
class objects exist and the checker visited them. Their script packages are never loaded by the game, so they
have no property objects. The layout holds exactly 630 fields of native classes of the three editor packages
(`UnrealEd` 623, `GFxUIEditor` 7, `UTEditor` 0), and the whole layout reconciles:

| Layout fields | Count |
|---|---:|
| matched by a live property | 15,081 |
| of native classes of the three editor packages (class object without properties) | 630 |
| of classes with no class object at all (script-only classes of the editor packages 52, `UTGameContent` 93) | 145 |
| all script classes | 15,856 |

`win32_props_check.py` now treats a class without any live property, in a package without any live property,
as "registered, script not loaded": its fields are counted per package and not called missing. A field
without a property object in a class of a loaded package is still a failure. It prints the three sets above and
checks that they add up to the layout.

### 8.3 The checkers

[`tools/ghidra-scripts/win32/win32_live_check.py`](../../tools/ghidra-scripts/win32/win32_live_check.py) is a
read-only checker (ctypes; `OpenProcess` with query + read rights, `ReadProcessMemory`; no debugger, no
writes) of the scanner's address tables. With the game running it checks:

- the module header at the discovered base has this build's time stamp;
- `FName::Names[0] == "None"`, `Names[760] == "Log"`;
- every set `PrivateStaticClass` points at a `UClass` (by vtable) whose name is the class name; the class's
  size field is the `sizeof` from the table or a property end that rounds up to it (8.1) and its default object
  carries the listed vtable (the checker finds the two `UClass` field offsets by agreement across classes and
  reports them);
- `GEngine` carries `UGameEngine`'s vtable, `GWorld` carries `UWorld`'s;
- `GObjObjects` is plausible and `object.Index` (+0x20) equals the slot;
- `GamePlayers[0].Actor` carries `AUDKPlayerController`'s vtable (in a level), and which controller fields
  point at an `AUDKPawn`-vtable object (a hint for the layout stage);
- `GFrameCounter` and `WorldInfo.RealTimeSeconds` advance over an interval; `GDeltaTime == GFixedDeltaTime`
  when benchmarking.

[`tools/ghidra-scripts/win32/win32_props_check.py`](../../tools/ghidra-scripts/win32/win32_props_check.py)
compares the engine's own reflection objects with the layout. Every script property is a `UProperty` object
whose `Offset` (+0x60) is the offset the engine itself linked for that field, and every class object lists its
properties (`Children` +0x4C, `Next` +0x3C), so *every* field offset of every loaded class can be compared, at
the main menu, without a level. It also compares the class default objects of the player classes with the
recorded script defaults (`GroundSpeed` 440, `JumpZ` 1000, `AirControl` 0.3, `MaxStepHeight` 26, ...), the
64 fields of the recorder layout and, when `recorder_optional_win32.json` is next to it, the recorder's
optional fields (section 13). Its input `expected_win32.json` (every script class laid out by an independent
implementation of the rules: 2,521 classes, 15,856 fields) is local data and not in the repository
(`research/local/verify-win-re-layout/`).

Both have a self-test (`--selftest`, a fake memory image built from the data files; any OS). They check the
checker, not the game: the live checker's fake holds classes whose size field is a property end below the
`sizeof` (both alignments) and one whose size is neither; the property checker's is built with a class whose
end rounds up by 4 (`ACoverLink`), one by 16 (`AKActor`) and a registered editor class without properties, and
it must catch a property moved by 4 bytes, a linked class that holds its `sizeof`, and a property without its
object. Both pass on macOS with Python 3.9, 3.13 and 3.14.

## 9. Notes for the layout and recorder stages

1. **Module base.** Read it per process: the image is relocatable and `DYNAMICBASE` is set. The Python on the
   Windows machine is 64-bit and the game is a 32-bit process, so enumerate modules with
   `CreateToolhelp32Snapshot(TH32CS_SNAPMODULE | TH32CS_SNAPMODULE32, pid)` (as `win32_live_check.py` does) or
   `EnumProcessModulesEx(..., LIST_MODULES_32BIT)`. All addresses fit in 32 bits.
2. **Layout file.** `pointer_size` 4; `TArray` `{data 0, count 4, max 8}` size 12; `FName` `{index 0, number 4}`;
   `FNameEntry` `{flags 0, index 8, chars 16}`, `wide_flag_mask` 1, `wide_char_size` 2; `FString` `char_size` 2;
   `Core.Object` `Outer` 0x28, `Name` 0x2C, `Class` 0x34. Symbols become module offsets from `globals.json` /
   `functions.json` (`UWorld::Tick` 0x00635450, `UGameEngine::Tick` 0x00589720).
3. **Field offsets** of `Actor`, `Pawn`, `Controller`, `PlayerController`, `PlayerInput`, `Camera`, the script
   classes and the rest of `WorldInfo` are **not** derived here. Derive them with the Win32 layout rules and
   check against `class_sizes.json` (1,982 sizes) and the anchors in section 7 (`Engine.GamePlayers` 0x4B4,
   `Player.Actor` 0x40, the five `WorldInfo` time fields).
4. **Breakpoint front end.** `UWorld::Tick` is thiscall: read `DeltaSeconds` from `[esp+8]` and the world from
   `ecx`. A debugger of a 32-bit process from 64-bit Python needs the WOW64 thread-context calls
   (`Wow64GetThreadContext` / `Wow64SetThreadContext`) and sees 32-bit breakpoint and single-step exceptions
   under their WOW64 codes (TENTATIVE: general Windows behaviour, not tested here). The game imports
   `IsDebuggerPresent`; behaviour under a debugger is UNKNOWN.
5. **Object identification without RTTI.** Use the vtable for the unique gameplay classes (section 5.2) or
   compare the object's `Class` pointer (+0x34) with `*(base + private_static_class_rva)`; script classes have
   no `PrivateStaticClass`, so name them through `Class → Name` as the Mac recorder does.
6. **`check-recorder`.** `asamu-trace check-recorder` has a PE reader and `win32:*` check groups since the layout
   stage ([DEFAULTS.md](DEFAULTS.md) section 9.6): with a local copy of the executable it verifies every
   instruction of `layout_win_x86.json` at its RVA inside the function its anchor identifies. `win32_scan.py
   --check` regenerates the six scanner files of `data/win32/`.

## 10. Reproduce

`EXE` = a local copy of `<install>/Binaries/Win32/ASAMU-Win32-Shipping.exe` (git-ignored, for example
`research/local/win/ASAMU-Win32-Shipping.exe`). Outputs are names, addresses, sizes and counts.

```sh
EXE=research/local/win/ASAMU-Win32-Shipping.exe
shasum -a 256 "$EXE"                                   # 17f2aeb6...6d26e4e

# Optional Mac inputs for the cross-checks (names; sizes and flags of the Mac registrations).
MAC="$ASAMU_ORIGINAL_DIR/A Story About My Uncle.app/Contents/MacOS/ASAMU"
nm "$MAC" | grep '18PrivateStaticClassE$' | awk '{print $3}' | c++filt -_ \
  | sed 's/::PrivateStaticClass//' | sort > research/local/win/mac_classes.txt        # 1535 names
#   research/local/defaults/gpsc.asm: DEFAULTS.md, Reproduce step 2
nm "$MAC" | awk '$3 ~ /InitializePrivateStaticClass/ {print $3}' > research/local/defaults/ipsc-syms.txt
objdump -d --no-show-raw-insn --disassemble-symbols=$(paste -sd, research/local/defaults/ipsc-syms.txt) "$MAC" \
    > research/local/defaults/ipsc.asm

# Everything in docs/reverse-engineering/data/win32 (about 2.5 minutes; 87 checks, 0 failed):
python3 tools/ghidra-scripts/win32/win32_scan.py "$EXE" --out docs/reverse-engineering/data/win32 \
    --mac-classes research/local/win/mac_classes.txt --mac-gpsc research/local/defaults/gpsc.asm \
    --mac-ipsc research/local/defaults/ipsc.asm --verbose
# Same command with --check: regenerates and compares, "6 files compared, 0 differ". The three --mac-* inputs
# are optional, but the committed class_sizes.json was written with them (its mac_comparison block).

# Spot checks without the tool (LLVM objdump; addresses are image base 0x400000 + RVA):
objdump -d --x86-asm-syntax=intel --start-address=0x18a6820 --stop-address=0x18a68d1 "$EXE"   # time update: GIsBenchmarking, GUseFixedTimeStep, GDeltaTime = GFixedDeltaTime
objdump -d --x86-asm-syntax=intel --start-address=0x18a8dc0 --stop-address=0x18a8f24 "$EXE"   # FEngineLoop::Tick: time update ... GEngine vtable +0x134 ... GFrameCounter += 1
objdump -d --x86-asm-syntax=intel --start-address=0x9897ce --stop-address=0x9898c3 "$EXE"     # UGameEngine::Tick: Client->Tick, StaticTick, GWorld->Tick(2, dt)
objdump -d --x86-asm-syntax=intel --start-address=0xa3582e --stop-address=0xa358fe "$EXE"     # UWorld::Tick: the WorldInfo time update
objdump -d --x86-asm-syntax=intel --start-address=0xb0f193 --stop-address=0xb0f1ed "$EXE"     # APawn registration: push 0x454 (sizeof), push 0x805 (flags)

# Verification pass (section 12): the engine's own member-offset checks and one replication lookup.
objdump -d --x86-asm-syntax=intel --start-address=0x98dfb3 --stop-address=0x98dfde "$EXE"     # UGameEngine::Init: mov eax,[edi+0x60]; cmp eax,0x9c; push L"Owner"; push L"Actor"; push the format literal
objdump -d --x86-asm-syntax=intel --start-address=0x8370c1 --stop-address=0x837102 "$EXE"     # APawn::GetOptimizedRepList: push L"GroundSpeed" ... mov edx,[ebx+0x28c]
objdump -d --x86-asm-syntax=intel --start-address=0x5f4136 --stop-address=0x5f4143 "$EXE"     # UObject::AddObject: GObjObjects.data[index] = this; [this+0x20] = index
# All 80 evidence entries of the verification pass (anchors, bytes, name literals) against the executable:
cargo run -p asamu-trace -- check-recorder --verbose | grep 'win32:binary'

# Ghidra (optional): label the local project and cross-check function starts and call counts.
export JAVA_HOME="$(brew --prefix openjdk@21)/libexec/openjdk.jdk/Contents/Home"
GHIDRA="$(brew --prefix ghidra)/libexec/support/analyzeHeadless"
"$GHIDRA" research/ghidra/win ASAMUWin -import "$EXE" -overwrite          # one-time, about 22 minutes
"$GHIDRA" research/ghidra/win ASAMUWin -process ASAMU-Win32-Shipping.exe -noanalysis \
    -scriptPath tools/ghidra-scripts -postScript ApplyWin32Names.java \
    data=docs/reverse-engineering/data/win32 report=research/ghidra/out/win32-names.txt

# Live check (on the Windows machine, game running; read only). Copy the script and the six JSON files:
#   %USERPROFILE%\asamu-trace\win32_live_check.py, %USERPROFILE%\asamu-trace\data\win32\*.json
python win32_live_check.py --seconds 2
python3 tools/ghidra-scripts/win32/win32_live_check.py --selftest          # any OS, no game
```

## 11. Findings by confidence

### CONFIRMED

- Header, sections, relocations, imports, exports, CLR header and the absence of a wrapper section or overlay
  (section 1); the hash equals the installed file's.
- TCHAR is UTF-16; MSVC RTTI does not cover engine classes.
- 2,103 registration calls, 1,982 classes; package, name, `sizeof`, class flags and cast flags of each
  (immediate operands; flags equal the Mac build's for all 1,526 classes compared; Ghidra counts the same 2,103
  calls). Super and within of the 1,526 classes that could be compared with the Mac build.
- The nine recorder globals and the frame-boundary functions (sections 4 and 6), statically, each by two
  independent routes (section 12.1). The frame order, including the position of the `GFrameCounter` increment
  and of the `WorldInfo` time update.
- `-BENCHMARK` / `-FPS=` handling and the absence of any store to `GUseFixedTimeStep`.
- The struct offsets marked CONFIRMED in section 7, including the object index at +0x20, `Name`, `Class`,
  `FName.Number`, `Player.Actor` and where a `UProperty` keeps its offset.
- 16 vtables of 15 classes named independently by literals of their own virtual functions (section 12.2).
- `UObject::GObjFirstGCIndex`, `UWorld::GetWorldInfo`.

### STRONG

- No Steam DRM (no wrapper artefacts; not tested by running).
- Super and within of the Windows-only classes; the primary vtable of every class; vtable sharing caused by
  linker folding.
- `FNameEntry` flags; the helper functions named by call position; `ObjectArchetype`, `HashNext` and
  `HashOuterNext` offsets in `UObject`.
- The four other writers of `GIsBenchmarking` are Matinee-editor code and are not reached by the game.
- Visual C++ 2010 SP1 as the compiler of the main build.
- Input is dispatched before `UWorld::Tick` (by `Client->Tick` and the message pump), as on the Mac.

### TENTATIVE

- Purpose of the non-Scaleform exports.
- `UStruct.PropertiesSize` (+0x50), `UBoolProperty.BitMask` (+0x70), `UProperty.ArrayDim` (+0x40).
- WOW64 debugger details in section 9.

### UNKNOWN

- Everything that needs the running game: the live values, whether the build behaves differently with a
  debugger attached, where the game thread waits in fixed-step mode, the cost of sampling.
- Field offsets inside the gameplay classes are in [DEFAULTS.md](DEFAULTS.md) section 9; the offsets of the
  script-only classes have no static evidence beyond the layout rules.

## 12. Independent verification (2026-10-10)

A second pass checked this file and the layout stage adversarially. It used its own PE reader, the base
relocation table as a cross-reference index, Ghidra's function list, the symbolised Mac executable and the class
model of the script packages; it did not use `win32_scan.py`, the vtable slot pairs or the first pass's scratch
comparison. Its tooling was scratch (not in the repository); what it found is recorded here, 61 instruction
rows and 19 further evidence entries were added to the generated layout
(`data/win32/native_layout_win32.json` → `independent_offsets`, `layout_win_x86.json` → `native_evidence`), and
`asamu-trace check-recorder` verifies those against the executable.

Functions are identified in this pass by one of three anchors, none of which involves a field offset:

- **a literal only that function uses**: every use of the literal's address in `.text` lies in the function
  (the base relocation table lists every absolute operand, 1,247,113 of them, the same count as section 1.1);
- **the native function name table** (2,526 `<Class>exec<Name>` entries, parsed again: same count, one address
  each);
- **a direct call** from a function identified one of those two ways.

### 12.1 Globals — 9 of 9 found again in other functions (CONFIRMED)

| Global | Function (how identified) | What it does there; Mac counterpart |
|---|---|---|
| `GEngine` | `UEngine::execGetEngine` (name table); `AActor::GetALocalPlayerController` (called by its exec thunk); `UGameViewportClient::GetPlayerOwner` (same) | returned as the result; base of the `GamePlayers` walk (`+0x4B4`, count `+0x4B8`, `Player.Actor` `+0x40`; Mac `+0x6D8`, `+0x6E0`, `+0x68`) |
| `GWorld` | `UEngine::execGetCurrentWorldInfo` (name table) | `this` of the call to `UWorld::GetWorldInfo` (RVA `0x008A3370`), as on the Mac |
| `GFrameCounter` | `FDynamicLightEnvironmentState::Tick` (RVA `0x002B1140`, paired with the Mac function by content: flag-bit test, then the counter modulo 10 against a random number); `FPhysXVerticalEmitter::Tick` (`0x00A4CAC0`) | both halves are read as one unsigned 64-bit value |
| `GDeltaTime` | `UAudioDevice::GetSortedActiveWaveInstances` (literal) | loaded as a `double` and narrowed to `float`, as on the Mac |
| `GFixedDeltaTime` | Matinee editor's fixed-time-step setter (RVA `0x0121C470`, uses the key literal `FixedTimeStepPlayback`; Windows only) | receives a `float` widened to `double`; it is 8 bytes below `GDeltaTime`, as in the Mac image |
| `GIsBenchmarking` | `FEngineLoop::Exit` (literal `benchmark.log`); `appInitFullScreenMoviePlayer` (literal `bForceNoMovies`); the Matinee setter | tested before the benchmark log is written; set to 1 or 0 with the fixed step |
| `GUseFixedTimeStep` | `DrawUnitTimes` (literal `Game thread time`) | tested right after `GIsBenchmarking`, as on the Mac |
| `FName::Names` | `FName::SafeString` (literal `*INVALID*`) | `index < count (+4)`, then `data[index * 4] != 0` |
| `UObject::GObjObjects` | `UObject::AddObject`; `UGameViewportClient::Exec` and `UObject::StaticExec` (literals) | `data[index] = this`; iterated with the count at +4 (6 and 21 uses) |

The identity of the two `GFrameCounter` readers rests on content (STRONG); that the value at that address is
read as a 64-bit frame number in them is what the instructions show.

### 12.2 Vtables named by their own virtual functions (CONFIRMED for the listed vtables)

Slot numbers are Windows slots; each function was found by literals that only it uses and that the Mac function
of that name uses.

| Class | Vtable RVA | Functions found in it (slot) |
|---|---|---|
| `UObject` | `0x01E5AD38` | `BeginDestroy` (9), `FinishDestroy` (12), `Register` (35), `Rename` (49), `ScriptConsoleExec` (75) |
| `AActor` | `0x01E682C0` | `PostLoad` (8), `PostEditChangeProperty` (18), `GetOptimizedRepList` (100), `SetBase` (130), `PreBeginPlay` (140) |
| `APawn` | `0x01EDD188` | `GetOptimizedRepList` (100), `UpdatePushBody` (172), `GetAnimControlSlotDesc` (177), `PreviewBeginAnimControl` (178), `PreviewFinishAnimControl` (181) |
| `AUDKPawn` | `0x02219FA0` | `GetOptimizedRepList` (100; 18 of the Mac function's 19 literals) |
| `AInventory`, `AUDKWeapon` | `0x01EDA748`, `0x02216B80` | `GetOptimizedRepList` (100) |
| `APlayerController` | `0x01EE7258` | `ConsoleCommand` (78), `GetOptimizedRepList` (100), `TellPeerToTravel` (300), `TellPeerToTravelToSession` (301), `PeerTravelAsHost` (314) |
| `AUDKPlayerController` | `0x0221E990` | `PreSave` (5) |
| `AWorldInfo` | `0x01EE85B0` | `PostEditChangeProperty` (18), `GetOptimizedRepList` (100), `BeginHostMigration` (248) |
| `UEngine` | `0x01F28AC8` | `Init` (78), `TickFPSChart` (84), `DumpFPSChartToLog` (92) |
| `UGameEngine` | `0x01F32A50` | `FinishDestroy` (12), `Tick` (77), `Init` (78), `SpawnServerActors` (98); `Exec` in the interface vtable at `0x01F32A4C` |
| `UInput` | `0x01FAD308` | `InputKey` (79), `Exec` (86) |
| `UWorld` (interface vtable, object +0x3C) | `0x01F991BC` | seven `Notify…` functions of the network-notify interface |
| `ULocalPlayer`, `UGameViewportClient` (interface vtables) | `0x01F61914`; `0x01F61728`, `0x01F61720` | `Exec`; `Precache`, `Draw`; `Exec` |

- Not covered: the primary vtables of `UWorld`, `ULocalPlayer`, `ACamera` and `UPlayerInput` have no override
  with a literal of its own. They stay STRONG; functions in their slots pair with the Mac functions by the
  members they touch (12.4).
- The 31 shared vtable addresses (134 classes) were recounted; every group is made of a parent with children or
  of siblings. `UObject` shares its table with `UObjectSerializer` although their sizes differ (0x3C and 0x48),
  which shows that a vtable alone does not give the class.
- The Windows slot of a function is not the Mac slot plus a constant that only grows: Mac slot 105 is Windows
  slot 116 while Mac slot 120 is Windows slot 130 (Visual C++ orders overloaded virtuals differently). A slot
  taken "by the same shift as its neighbours" therefore needs a content check; the ones this project uses have
  one (12.4).

### 12.3 Class sizes (CONFIRMED)

- **Registrations decoded again.** Ghidra's own instruction decoding of the 2,103 call sites of the `UClass`
  static constructor gives 1,982 classes; name, `sizeof` and class flags equal `class_sizes.json` for all of
  them, and copies of one class agree.
- **An independent implementation of the layout rules** (Python, written from the rule text of
  [DEFAULTS.md](DEFAULTS.md) section 9.3, reading the class model of the script packages) gives the registered
  `sizeof` for all 1,652 native script classes, the same offset and bit for all 1,632 rows of
  `native_layout_win32.json`, the same offset and bit for all 64 fields of the recorder layout, 60 bytes for
  `Map_Mirror` and 24 for `KeyBind`, and the same failure counts when a rule is removed (1,652 / 1,193 / 93 /
  150 / 16 / 882).
- **Sizes the compiler states itself.** In the engine's own offset checks (12.4) the first member of
  `SequenceObject` is at 0x3C = `sizeof(UObject)`, and the first member of `MeshComponent` is at 0x1D8, below
  `sizeof(PrimitiveComponent)` = 0x1E0: a class continues at its parent's unpadded end by the compiler's own
  number. `sizeof(UProperty)` = 0x70 fits `Offset` at +0x60.

### 12.4 Field offsets

| Evidence | Result | Confidence |
|---|---|---|
| **The engine's own checks of compiled offsets.** `UGameEngine::Init` (the only user of the literal `engine-ini:Engine.Engine.Client`) finds a property by class and member name and compares its linked offset with an immediate, the offset the C++ compiler gave the member; a mismatch would log `Class %s Member %s problem`. 16 such checks: `Actor.Owner` 0x9C, `PlayerController.ViewTarget` 0x374, `Pawn.Health` 0x2E0, `Texture.UnpackMax` 0x50, `Sequence.DefaultViewZoom` 0x148, `SequenceObject.ObjInstanceVersion` 0x3C, `SequenceOp.PlayerIndex` 0xD4, `SequenceAction.HandlerName` 0xE4, `SeqAct_Latent.LatentActors` 0xFC, `SeqAct_Interp.PlayRate` 0x188 and `.RenderingOverrides` 0x1D0, `PrimitiveComponent.Tag` 0x58 and `.LightingChannels` 0x144, `MeshComponent.Materials` 0x1D8, `SkeletalMeshComponent.SkeletalMesh` 0x1E4, `SkeletalMesh.RefBasesInvMatrix` 0xF0 | 16 of 16 equal the rule offsets | CONFIRMED |
| **Replication functions.** A class's `GetOptimizedRepList` (vtable slot 100) looks each replicated property up by its name literal and compares the member with the same member of the last replicated state. In the 26 Actor classes that have their own function, the rule offset of the named member is used right after its name in | 158 of 167 cases (the other 9 compare through a helper, a member of the struct, or a single byte of a flags word) | CONFIRMED |
| **Exec thunks.** 2,353 thunks of the name table were compared with the Mac thunk of the same name; a member counts when the Mac thunk uses its Mac offset and the Windows thunk its rule offset | 104 different non-bool members in 162 thunks (a thunk that does not show a member proves nothing: the two compilers inline differently) | STRONG (pairing by name and offset, not by instruction) |
| **The first pass's functions, paired by member names.** Each function the layout stage located by vtable slot was compared with the Mac function of that name: members the Mac function uses at their Mac offsets and the Windows function at their rule offsets | `AUDKPawn::TickSpecial` 43, `APawn::physWalking` 21, `UGameEngine::Tick` 20, `APawn::physFalling` 19, `UWorld::Tick` 17 (`WorldInfo` members), `APlayerController::Tick` 16, `APawn::processLanded` 14, `APawn::performPhysics` 11, others 2–5 | CONFIRMED identity of those functions |

For the 64 fields the recorder reads:

- 18 have an instruction of this second route in the repository (`Object.Name`, `.Class`; `Engine.GamePlayers`;
  `Player.Actor`; `Actor.Location`, `.Rotation`, `.Physics`, `.Base`, `.Velocity`; `Controller.Pawn`;
  `Pawn.GroundSpeed`, `.AirSpeed`, `.JumpZ`, `.AirControl`; `Input.Bindings`, `.PressedKeys`;
  `WorldInfo.TimeDilation`, `.Pauser`);
- 11 more are shown only by first-pass instructions, in functions whose identity the pairing above confirms
  (`Object.Outer`, `Actor.WorldInfo`, `.Acceleration`, `Pawn.WalkableFloorZ`, `.EyeHeight`, `.Weapon`,
  `PlayerController.PlayerCamera`, `.PlayerInput`, `WorldInfo.TimeSeconds`, `.RealTimeSeconds`,
  `.DeltaSeconds`). `Pawn.Weapon` 0x3C8 is also bracketed exactly by `InvManager` 0x3C4 and `FlashLocation`
  0x3CC of the replication function;
- 35 have no instruction on either pass (the `PlayerInput` axes, `bPressedJump`, `FOVAngle`,
  `MoveForwardSpeed`, `CameraCache.POV.FOV` and the script-only classes). They rest on the rules, which two
  implementations apply identically; only the live check can show them.

### 12.5 Discrepancies found, and what is not in the repository

- Section 7 gave the object index at +0x04 (TENTATIVE); it is at +0x20. Corrected there. The live checker
  still reads +0x04 (section 8).
- The four "unidentified" writers of `GIsBenchmarking` are Matinee-editor functions (section 4).
- Section 9 item 6 was out of date (`check-recorder` reads PE images).
- The `FName` number (+4) and `UObject::GObjFirstGCIndex` were STRONG or TENTATIVE and are now shown by
  instructions.
- No address, size or offset of `data/win32/` or of the recorder layout was found wrong.
- Labels inside the scanner's JSON files are the first pass's: `globals.json` still says STRONG for
  `UObject::GObjFirstGCIndex` and a note of `native_evidence.json` says STRONG for the `Name` and `Class` offsets.
  This file states the current confidence; the JSON is regenerated by `win32_scan.py` and was not edited by hand.

Not in the repository: the scratch tooling of this pass (literal and relocation cross-references, the Mac and
Windows function pairing, the Python layout implementation) and `expected_win32.json`, the data file of the live
property checker (every script class laid out by the Python implementation: 2,521 classes, 15,856 fields, plus
355 recorded default values of six player classes). The checker itself, `win32_props_check.py`, is in the
repository since (section 8.3); it was run against the game on 2026-10-10 (section 8).

## 13. Optional recorder fields (2026-10-10)

The fields recorder 0.2.0 reads beyond the 64 of the layout ([TRACE_CAPTURE.md](../TRACE_CAPTURE.md) §6.10).
Offsets are the layout rules' (`native_layout_win32.json` has the rows; the Python implementation of the
verification pass gives the same offset and bit for all 23 that are plain properties). The instructions below
are in `layout_win_x86.json` → `native_evidence` and in `native_layout_win32.json` (`native_offsets` now 77 of
77, `independent_offsets` 66 of 66); `asamu-trace check-recorder` finds each at its RVA in the executable,
inside the function its anchor names (1,635 checks, 0 failed). The file the recorder reads is
`data/win32/recorder_optional_win32.json` (25 fields, two structures, three sentinels), generated with the
layout by `gameplay_defaults --target win32`.

Functions (found through the native function name table, then by direct calls, as in section 12):

| Function | RVA | Found by | Mac counterpart |
|---|---|---|---|
| `UObject::execGetStateName` | `0x0018D100` | name table | same thunk; it reads the state frame at +0x20 and the state node at +0x48 of the frame |
| `AActor::GetTimerCount` | `0x0075B670` | the only direct call of the thunk `AActor::execGetTimerCount` (`0x00516C00`) | `AActor::GetTimerCount(FName, UObject*)`, the only direct call of the Mac thunk; timers at +0xD8, count at +0xE0 |
| `AActor::IsTimerActive` | `0x0075B540` | the only direct call of `AActor::execIsTimerActive` (`0x00516B50`) | same |
| `AActor::PauseTimer` | `0x0075B410` | the only direct call of `AActor::execPauseTimer` (`0x00516A70`) | same |
| `AActor::SetTimer` | `0x0075ACF0` | the only direct call of `AActor::execSetTimer` (`0x00516810`) | `AActor::SetTimer(float, unsigned int, FName, UObject*)`, the only direct call of the Mac thunk; it also masks its loop argument to bit 0, writes it into the timer's first word and clears bit 1 of that word |
| `ACamera::SetViewTarget` | `0x004C8B10` | the only direct call of `ACamera::execSetViewTarget` (`0x00528ED0`) | same |
| `ACamera::AssignViewTarget` | `0x004BD9B0` | called twice by `ACamera::SetViewTarget`, as on the Mac | it copies `DefaultAspectRatio` (+0x268) and `DefaultFOV` (+0x258) into the view target and loads `PCOwner` (+0x248), in that order on both builds |
| `APawn::physFalling` | `0x00727590` | section 12.4 (vtable slot, members paired) | loads `CylinderComponent` (+0x478), then its radius and height |

Fields:

| Field | Win32 offset | Instruction (function, RVA) | Confidence |
|---|---|---|---|
| `Object.StateFrame` | 0x14 | `UObject::execGetStateName` loads it, `0x0018D125` | CONFIRMED |
| state node in the native state frame (no script declaration) | +0x28 | same function, `0x0018D12D`; then it tests that object's index (+0x20) and returns its name (+0x2C) | CONFIRMED (the instruction is the only source: no layout rule covers a native-only structure) |
| `Actor.Timers` | 0xA4 (count 0xA8) | `AActor::GetTimerCount` loads it, `0x0075B6D2` | CONFIRMED |
| `TimerData`: 28-byte elements; `FuncName` +4, `Rate` +0xC, `Count` +0x10, `TimerObj` +0x18; `bPaused` bit 1 and `bLoop` bit 0 of the word at +0 | — | `GetTimerCount` `0x0075B6D8` (index × 7 × 4), `0x0075B6E5`, `0x0075B793`, `0x0075B734`; `IsTimerActive` `0x0075B651`; `PauseTimer` `0x0075B518` (mask 2); `SetTimer` `0x0075AF39` (the loop argument masked with 1 and written into the word; added by the verification of this section) | CONFIRMED, every member and both bits; equal to the layout rule for `Engine.Actor.TimerData` |
| `Camera.DefaultFOV`, `Camera.DefaultAspectRatio` | 0x1D8, 0x1E8 | `ACamera::AssignViewTarget`, `0x004BD9DD`, `0x004BD9D2` | CONFIRMED |
| `Camera.bLockedFOV`, `Camera.LockedFOV` | 0x1DC bit 0, 0x1E0 | none found: no `ACamera` function of the symbolised Mac build uses their Mac offsets (0x25C, 0x260), and the field-of-view accessors are script functions; other native classes were not searched. They lie between the two fields above, with `ConstrainedAspectRatio` 0x1E4 after them | STRONG (rule row of a native class whose `sizeof` the rules reproduce; bracketed on both sides; the live run of section 8 matched every loaded property) |
| `Camera.CameraCache.POV.Location`, `.Rotation` | 0x384, 0x390 | none (struct members by the rule; the third member, `.FOV` at 0x39C, read 90.0 on 45,237 records) | STRONG |
| `Camera.FreeCamDistance` (sentinel) | 0x438 | none | STRONG |
| `Pawn.Floor`, `Pawn.BaseEyeHeight` | 0x2CC, 0x2C4 | already in the layout stage's table: `APawn::processLanded` stores the normal (`0x007190F9`), `AUDKPawn::UpdateEyeHeight` loads the height (`0x014B7A9C`) | CONFIRMED |
| `Pawn.CylinderComponent` | 0x38C | `APawn::physFalling` loads it, `0x007279E4`; the next two instructions load `CollisionRadius` (+0x1DC) and `CollisionHeight` (+0x1D8) from it | CONFIRMED |
| `Actor.CollisionComponent`, `PrimitiveComponent.Translation`, `CylinderComponent.CollisionHeight`, `.CollisionRadius` | 0x18C, 0x1A0, 0x1D8, 0x1DC | already in the layout stage's table (`APawn::processLanded`, `APawn::physFalling`) | CONFIRMED |
| `UTPawn.WalkBob`, `Bob`, `LandBob`, `JumpBob`, `AppliedBob`, `BobTime`, `bJustLanded`, `bLandRecovery`, `DoubleJumpEyeHeight` (sentinel) | 0x6CC, 0x6B8, 0x6BC, 0x6C0, 0x6C4, 0x6C8, 0x628 bits 10 and 11, 0x6AC | none (script-only class) | STRONG (rules; live run of section 8) |

Sentinel values are class defaults read from the defaults data (`DefaultAspectRatio` 1.33333 and
`FreeCamDistance` 256 of `ASAMUCamera`, `DoubleJumpEyeHeight` 43 of `ASAMUPawn`). **None of these fields has
been read from the running game by the recorder yet.** The property checker compares the 23 plain ones with the
live property objects when the optional file is next to it (8.3).

Two things the recorder was asked for and the build does not have:

- **A stored distance to the floor or the base.** No property of `Actor`, `Pawn`, `GamePawn`, `UDKPawn`,
  `UTPawn` or `ASAMUPawn` holds one. (CONFIRMED: the property lists of those classes.)
- **`bPowerJumped` and `bHasReleasedJump` ever being true.** The offsets are right (bits 1 and 2 of the word at
  0x874, between `bHasJumped` bit 0 and `bSprinting` bit 4, which the recordings show working), and nothing
  sets them: in the cooked data the two properties are referenced by two functions only (`ASAMUPawn.Landed`
  and its `StoryState` override), and both assign false. Scanned: all 984 function and state exports of the
  `asamu` package (identifier names of the bytecode only); the two names occur in the name table of 1 of the
  42 cooked packages and in neither executable. (CONFIRMED.) `bHasJumped` is assigned true by one function
  (`ASAMUPawn.DoJump`) and never cleared.

```sh
EXE=research/local/win/ASAMU-Win32-Shipping.exe
D="objdump -d --no-show-raw-insn --x86-asm-syntax=intel"
$D --start-address=0x58D100 --stop-address=0x58D192 "$EXE"   # UObject::execGetStateName: [this+0x14], [frame+0x28], name at +0x2C
$D --start-address=0xB5B670 --stop-address=0xB5B7AA "$EXE"   # AActor::GetTimerCount: [this+0xA4], 28-byte elements, +4, +0x18, +0x10
$D --start-address=0xB5AF2C --stop-address=0xB5AF3F "$EXE"   # AActor::SetTimer: the flags word, the loop argument, mask 1, written back
$D --start-address=0x8BD9B0 --stop-address=0x8BD9F0 "$EXE"   # ACamera::AssignViewTarget: +0x1E8, +0x1D8, +0x1CC
$D --start-address=0xB279E4 --stop-address=0xB27A04 "$EXE"   # APawn::physFalling: +0x38C, then +0x1DC and +0x1D8
cargo run -p asamu-trace -- check-recorder --verbose | grep -E 'StateFrame|Timer|AssignViewTarget|CylinderComponent'
cargo run -p asamu-inspect --example gameplay_defaults -- --target win32 --check     # 3 files compared, 0 differ
```
