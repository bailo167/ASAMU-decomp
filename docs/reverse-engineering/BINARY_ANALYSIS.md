# Binary analysis — original Mac executable

Target: `A Story About My Uncle.app/Contents/MacOS/ASAMU`, Steam build 1822049, 67,378,284 bytes,
SHA-256 `b611c4a0a64d220f3f2b8bdbd6287700327976bd2f196fca328a4b1b2d13d004`,
`LC_UUID` `9601348A-2FAC-3566-998D-B4114A14260B`.

This file covers the executable as a container: load commands, segments, linker metadata, RTTI and vtables,
strings, toolchain, and how script-callable natives reach C++. Symbol statistics and the decoded native
tables are in [SYMBOL_ANALYSIS.md](SYMBOL_ANALYSIS.md). The pawn physics that runs on these natives is in
[NATIVE_PHYSICS.md](NATIVE_PHYSICS.md). Only counts, names, offsets and structures are published here. String
contents appear only as a few generic examples, and no build-machine path is reproduced.

Addresses are unslid virtual addresses. For both `__TEXT` and `__DATA`, **file offset = vmaddr − 0x100000000**
(CONFIRMED: `__TEXT` maps file offset 0 at 0x100000000, and `__DATA` maps file offset 0x20EC000 at 0x1020EC000).

## Most important findings

| # | Finding | Confidence |
|---|---|---|
| 1 | Classic ld64 layout: `LC_UNIXTHREAD` entry (not `LC_MAIN`), `LC_DYLD_INFO_ONLY` opcode fixups (no chained fixups), `LC_VERSION_MIN_MACOSX` 10.7 / SDK 10.12, **no `LC_CODE_SIGNATURE`**. The file ends exactly at the end of the string table. | CONFIRMED |
| 2 | **RTTI is disabled** for all game and engine code. All 5,756 defined vtables have a null typeinfo slot. Only 2 typeinfo objects exist (`std::exception`, `std::bad_alloc`). | CONFIRMED |
| 3 | Vtables are complete, named and prefix-stable under single inheritance. Every one of the 1,535 native UObject classes has its own `__ZTV` symbol. Every non-pure slot resolves to a named function. A vtable displacement in decompiled code therefore resolves exactly once the receiver's class is known. | CONFIRMED |
| 4 | Script-only classes (all ASAMU gameplay classes, `UTPawn`, …) have no vtable of their own. They run on their **nearest native ancestor's** vtable, because `UClass::Bind` copies the superclass's `ClassConstructor`. `ASAMUPawn` uses `AUDKPawn`'s vtable, `ASAMUPlayerController` uses `AUDKPlayerController`'s, and `GrappleGun` uses `AUDKWeapon`'s. | CONFIRMED (code) |
| 5 | Script natives and events are **not** vtable entries. Natives dispatch through `GNatives[]` (fixed index) or by name through `GNativeLookupFuncs` (a `TMap<FName, FNativeFunctionLookup*>`). Events go through `UObject::ProcessEvent` (vtable slot 67). The vtable matters one step later: roughly 40–45% of the 2,505 exec thunks reach a **virtual** C++ implementation (about 1,020–1,120, depending on how it is counted; §4.4). | CONFIRMED (dispatch) / STRONG (share) / TENTATIVE (exact count) |
| 6 | **TCHAR is a 4-byte `wchar_t` (UTF-32LE)**. The scan finds 15,022 UTF-32 literals in `__TEXT,__const` and **0** UTF-16 ones. The 11,215 ASCII literals in `__TEXT,__cstring` come mostly from third-party code, plus the 2,468 ANSI native-lookup names. | CONFIRMED |
| 7 | Toolchain: **Apple LLVM 8.1.0 (clang-802.0.42)**, i.e. Xcode 8.3.x, with the macOS 10.12 SDK, linked against **libstdc++** (not libc++). The build was driven by CMake. UE3 objects date from April–May 2017. Prebuilt PhysX and Scaleform archives (2012–2013) came from older toolchains. | CONFIRMED (compiler string, SDK, imports) / STRONG (Xcode version) |
| 8 | Fixed native indices were recovered from code: 306 `GRegisterNative` registrations, with no collisions. They agree **202/202** with the non-zero `iNative` of every script function in the 7 native script packages. | CONFIRMED |

## 1. Load commands (CONFIRMED)

`otool -hv`: `MH_MAGIC_64`, `CPU_TYPE_X86_64`, subtype `ALL` (caps `LIB64`), `MH_EXECUTE`, 28 commands,
`sizeofcmds` 3,752. Flags `NOUNDEFS DYLDLINK TWOLEVEL WEAK_DEFINES BINDS_TO_WEAK PIE`. The executable is
not `ALLOW_STACK_EXECUTION`. It carries no `NO_HEAP_EXECUTION` flag.

| # | Command | Size | Content |
|---:|---|---:|---|
| 0 | `LC_SEGMENT_64 __PAGEZERO` | 72 | vm 0x0–0x100000000 (4 GiB), no file data, prot `---/---` |
| 1 | `LC_SEGMENT_64 __TEXT` | 872 | vm 0x100000000 + 0x20EC000, file 0 + 34,521,088, max `rwx` / init `r-x`, 10 sections |
| 2 | `LC_SEGMENT_64 __DATA` | 1,112 | vm 0x1020EC000 + 0x4C8000, file 34,521,088 + 2,826,240, max `rwx` / init `rw-`, 13 sections |
| 3 | `LC_SEGMENT_64 __LINKEDIT` | 72 | vm 0x1025B4000 + 0x1CA4000, file 37,347,328 + 30,030,956, max `rwx` / init `r--` |
| 4 | `LC_DYLD_INFO_ONLY` | 48 | rebase / bind / weak-bind / lazy-bind / export streams (§2.2) |
| 5 | `LC_SYMTAB` | 24 | 535,281 `nlist_64` at file 42,761,432; strings 15,994,504 B at 51,383,780 |
| 6 | `LC_DYSYMTAB` | 80 | locals 0–458,246 (458,247), ext-defined 76,584, undefined 450; 14,463 indirect symbols at 51,325,928; no TOC/module/reloc tables |
| 7 | `LC_LOAD_DYLINKER` | 32 | `/usr/lib/dyld` |
| 8 | `LC_UUID` | 24 | `9601348A-2FAC-3566-998D-B4114A14260B` |
| 9 | `LC_VERSION_MIN_MACOSX` | 16 | min **10.7**, SDK **10.12** |
| 10 | `LC_UNIXTHREAD` | 184 | `x86_THREAD_STATE64`, `rip` = 0x100004D60 (all other registers 0) |
| 11–25 | `LC_LOAD_DYLIB` ×15 | 1,184 total | table below |
| 26 | `LC_FUNCTION_STARTS` | 16 | 122,920 B at 42,635,176 → 85,918 function starts (0x100004D60 … 0x10162B650) |
| 27 | `LC_DATA_IN_CODE` | 16 | 3,336 B at 42,758,096 → 417 entries, all kind 4 (`DICE_KIND_JUMP_TABLE32`) |

**Absent commands.** There is no `LC_CODE_SIGNATURE` (`codesign -dv`: "code object is not signed at all";
the bundled dylibs are unsigned too). The other absent commands are `LC_MAIN` (`LC_UNIXTHREAD` is used
instead), `LC_SOURCE_VERSION`, `LC_BUILD_VERSION`, `LC_DYLD_CHAINED_FIXUPS`, `LC_DYLD_EXPORTS_TRIE`,
`LC_RPATH`, `LC_ENCRYPTION_INFO*`, `LC_LOAD_WEAK_DYLIB`, `LC_REEXPORT_DYLIB` and
`LC_DYLD_ENVIRONMENT`. There is no `__DWARF` segment and no `__RESTRICT` segment. No `.dSYM`, `.pdb` or
map file ships anywhere in the install.

| Dylib (install name) | current / compat version |
|---|---|
| `Carbon.framework/Versions/A/Carbon` | 157.0.0 / 2.0.0 |
| `Cocoa.framework/Versions/A/Cocoa` | 22.0.0 / 1.0.0 |
| `@loader_path/openal.dylib` | 1.15.1 / 1.0.0 |
| `OpenGL.framework/Versions/A/OpenGL` | 1.0.0 / 1.0.0 |
| `IOKit.framework/Versions/A/IOKit` | 275.0.0 / 1.0.0 |
| `Security.framework/Versions/A/Security` | 57740.51.2 / 1.0.0 |
| `@loader_path/libSDL2-2.0.0.dylib` | 5.0.0 / 5.0.0 |
| `@loader_path/libsteam_api.dylib` | 1.0.0 / 1.0.0 |
| `/usr/lib/libstdc++.6.dylib` | 104.1.0 / 7.0.0 |
| `/usr/lib/libSystem.B.dylib` | 1238.50.2 / 1.0.0 |
| `AppKit.framework/Versions/C/AppKit` | 1504.82.104 / 45.0.0 |
| `CoreFoundation.framework/Versions/A/CoreFoundation` | 1349.64.0 / 150.0.0 |
| `CoreServices.framework/Versions/A/CoreServices` | 775.19.0 / 1.0.0 |
| `Foundation.framework/Versions/C/Foundation` | 1349.63.0 / 300.0.0 |
| `/usr/lib/libobjc.A.dylib` | 228.0.0 / 1.0.0 |

All 15 dylib commands carry the ld64 placeholder timestamp 2. The system framework versions are the ones
recorded in the macOS 10.12 SDK stubs, so they say which SDK was linked against, not which OS is required.

Bundled dylibs (same folder, all unsigned):

- `libSDL2-2.0.0.dylib`: x86_64 only, min 10.6, SDK 10.10.
- `libsteam_api.dylib`: i386 + x86_64, min 10.5.
- `openal.dylib`: i386 + x86_64, min 10.5, SDK 10.8.

`Info.plist` says `LSMinimumSystemVersion` **10.6**, but the executable itself declares 10.7. The plist has
`CFBundleShortVersionString` 1.1, identifier `com.coffeestainstudios.astoryaboutmyuncle`, and no `DT*`
(Xcode build-system) keys.

## 2. Segment and section map (CONFIRMED)

### 2.1 Sections

Sections are packed against the end of `__TEXT`: the last byte of `__eh_frame` is 8 bytes short of the
segment end. This leaves 16,024 bytes of free header space between the load commands (which end at file
offset 3,784) and `__text` (file offset 19,808).

| Section | Address | Size (bytes) | File offset | Align | Type / attributes |
|---|---|---:|---:|---|---|
| `__TEXT,__text` | 0x100004D60 | 23,226,295 (0x16267B7) | 19,808 | 2^4 | regular, `PURE_INSTRUCTIONS SOME_INSTRUCTIONS` |
| `__TEXT,__const_coal` | 0x10162B520 | 320 | 23,246,112 | 2^5 | regular + instruction attrs (holds 12 coalesced constants) |
| `__TEXT,__stubs` | 0x10162B660 | 19,026 | 23,246,432 | 2^1 | `S_SYMBOL_STUBS`, 6-byte stubs → 3,171 stubs |
| `__TEXT,__stub_helper` | 0x1016300B4 | 4,316 | 23,265,460 | 2^2 | regular + instructions |
| `__TEXT,__const` | 0x1016311A0 | 4,557,120 | 23,269,792 | 2^5 | regular (holds all wide string literals, §5) |
| `__TEXT,__cstring` | 0x101A89AE0 | 305,699 | 27,826,912 | 2^3 | `S_CSTRING_LITERALS` |
| `__TEXT,__gcc_except_tab` | 0x101AD4504 | 2,004,688 | 28,132,612 | 2^2 | regular (LSDA tables; 18,233 `GCC_except_table*` labels) |
| `__TEXT,__objc_methname` | 0x101CBDBD4 | 147 | 30,137,300 | 2^0 | `S_CSTRING_LITERALS` (11 selectors) |
| `__TEXT,__unwind_info` | 0x101CBDC68 | 395,812 | 30,137,448 | 2^2 | regular (compact unwind) |
| `__TEXT,__eh_frame` | 0x101D1E690 | 3,987,816 | 30,533,264 | 2^3 | regular |
| `__DATA,__program_vars` | 0x1020EC000 | 40 | 34,521,088 | 2^3 | regular (crt `NXArgc`/`NXArgv`/`environ`/`progname`) |
| `__DATA,__nl_symbol_ptr` | 0x1020EC028 | 16 | 34,521,128 | 2^3 | `S_NON_LAZY_SYMBOL_POINTERS` (2) |
| `__DATA,__got` | 0x1020EC038 | 64,952 | 34,521,144 | 2^3 | `S_NON_LAZY_SYMBOL_POINTERS` (8,119) |
| `__DATA,__la_symbol_ptr` | 0x1020FBDF0 | 25,368 | 34,586,096 | 2^3 | `S_LAZY_SYMBOL_POINTERS` (3,171) |
| `__DATA,__mod_init_func` | 0x102102108 | 6,120 | 34,611,464 | 2^3 | `S_MOD_INIT_FUNC_POINTERS` (765 initialisers) |
| `__DATA,__const` | 0x102103900 | 2,034,288 | 34,617,600 | 2^5 | regular (2,901 vtables, other read-only data) |
| `__DATA,__cfstring` | 0x1022F4370 | 128 | 36,651,888 | 2^3 | regular (4 CFStrings) |
| `__DATA,__objc_imageinfo` | 0x1022F43F0 | 8 | 36,652,016 | 2^2 | version 0, flags 0x40 |
| `__DATA,__objc_selrefs` | 0x1022F43F8 | 88 | 36,652,024 | 2^3 | `S_LITERAL_POINTERS`, `NO_DEAD_STRIP` (11) |
| `__DATA,__objc_classrefs` | 0x1022F4450 | 16 | 36,652,112 | 2^3 | regular, `NO_DEAD_STRIP` (`NSUserDefaults`, `NSAutoreleasePool`) |
| `__DATA,__data` | 0x1022F4460 | 692,704 | 36,652,128 | 2^5 | regular (2,855 vtables, `int…exec…` pointers, `G…Natives` tables) |
| `__DATA,__common` | 0x10239D640 | 1,782,676 | — | 2^5 | `S_ZEROFILL` (`GNatives`, `GNativeLookupFuncs`, …) |
| `__DATA,__bss` | 0x1025509E0 | 405,184 | — | 2^5 | `S_ZEROFILL` |

`__DATA` holds 2,826,240 bytes of file data and 5,013,504 bytes of VM; the 2,187,860 zero-fill bytes are
`__common` + `__bss`. Objective-C use is minimal: 11 selector references, 2 class references and 4
CFStrings.

### 2.2 `__LINKEDIT` layout

The blobs are contiguous, with no gaps, from file offset 37,347,328 to the end of file at 67,378,284.

| Blob | File offset | Size (bytes) | Content |
|---|---:|---:|---|
| rebase opcodes | 37,347,328 | 36,256 | 290,128 rebases: `__DATA,__const` 229,415, `__data` 48,651, `__got` 8,110, `__la_symbol_ptr` 3,167, `__mod_init_func` 765, `__objc_selrefs` 11, `__program_vars` 5, `__cfstring` 4 |
| bind opcodes | 37,383,584 | 5,208 | 4,177 binds: **4,148 are `___cxa_pure_virtual` vtable slots** (3,735 in `__data`, 413 in `__const`). The other 29 are 9 GOT imports, 8 records for the 4 lazy pointers of `operator new`/`new[]`/`delete`/`delete[]`, 4 `___CFConstantStringClassReference`, 3 function pointers in `__DATA,__const` (`fclose`/`fread`/`ftell`), 2 Objective-C class refs, the 2 libstdc++ `__cxxabiv1` typeinfo vtables and `dyld_stub_binder` |
| weak-bind opcodes | 37,388,792 | 1,793,312 | 30,037 coalescing sites over 19,530 weak symbols (`__data` 11,526, `__const` 9,660, `__got` 6,106, `__la_symbol_ptr` 2,745) |
| lazy-bind opcodes | 39,182,104 | 9,888 | 430 lazily bound imports |
| export trie | 39,191,992 | 3,443,184 | 76,584 exported symbols (= `nextdefsym`; 19,889 weak definitions) — every global is exported |
| function starts | 42,635,176 | 122,920 | 85,918 starts (`nm` has 85,907 text symbols) |
| data in code | 42,758,096 | 3,336 | 417 jump tables |
| symbol table | 42,761,432 | 8,564,496 | 535,281 × 16 B (400,188 of them STABS) |
| indirect symbols | 51,325,928 | 57,852 | 14,463 × 4 B = stubs 3,171 + GOT 8,119 + lazy pointers 3,171 + non-lazy 2 |
| string table | 51,383,780 | 15,994,504 | ends exactly at EOF |

The symbol and string tables together take 24.6 MB, or 36% of the file. All of `__LINKEDIT` takes 45%.

`WEAK_DEFINES`/`BINDS_TO_WEAK` has a practical consequence. Only **434** of the 3,171 `__stubs` entries name
an undefined (imported) symbol in the indirect-symbol table: 430 of them are lazily bound, and the other 4 are
the C++ allocation operators, which the regular bind stream binds at launch (they are also weak-bound). The
other **2,737** stubs name weak (inline/template) definitions inside the executable itself, which are coalesced
at load time. A call through a stub in Ghidra is therefore **not necessarily an import** (CONFIRMED: indirect
symbol table against `nm -u`. The 2,741 distinct weak-bound `__la_symbol_ptr` slots are exactly those 2,737 plus
the 4 operators. An earlier version of this paragraph said 430 imports and about 2,740 weak stubs.)

### 2.3 Entry point and startup (CONFIRMED)

- `LC_UNIXTHREAD.rip` = 0x100004D60. That is the first byte of `__text` and the crt1 symbol `start`.
  `start` calls `_main` at 0x100FE81F0, which is defined in **`Mac/Src/LinuxImpl.cpp`**: the Mac platform
  layer reuses a Linux/SDL-style implementation file. The engine path continues through
  `appMacCallGuardedMain()` (`Mac.cpp`) and `GuardedMain(const wchar_t*, void*, void*, int)` (`Launch.cpp`).
- There are 765 module initialisers in `__mod_init_func`. 599 are clang-style `__GLOBAL__sub_I_<file>`, 62
  are `___cxx_global_var_init*`, and 104 are GCC-style `__GLOBAL__I_<first-symbol>`. Of the 104, 99 are
  Scaleform, 2 are PhysX (`HullLib`, `getFoundationSDK`), 2 are `__GLOBAL__I_a` and 1 is `Check_Address`.
  These initialisers register the native classes and natives (§6.1).
- Platform module units (Mac module, 10): `AsyncLoadingMac`, `FFileManagerMac`, `LinuxImpl`, `Mac`,
  `MacClient`, `MacObjCWrapper.mm`, `MacThreading`, `MacViewport`, `MacWideChar.mm`, `UnSocketBSD`.
  `OpenGLDrv` has 17 units, including `OpenGLD3DBytecodeConverter.cpp` and `OpenGLViewportMac.cpp`.
  TENTATIVE reading of that file name: the GL renderer converts D3D shader bytecode, which would explain
  the D3D shader caches that ship in `CookedMac`.

## 3. Debug information and source paths (CONFIRMED)

- **No DWARF in the executable.** It has no `__DWARF` segment and no dSYM. It does keep the **ld64 STABS
  debug map**: 400,188 entries, of which 3,990 `N_SO`, 1,330 `N_OSO`, 2,489 `N_SOL` and 171,830 `N_FUN`.
  These entries point at the original `.o` files and archive members, which do not ship. Function and static
  names are recoverable from the map; types and line tables are not.
- Path strings in the debug map (`N_SO`/`N_OSO`/`N_SOL`): 7,809 entries with 5,174 distinct names. 4,823
  entries (3,546 distinct) are absolute:
  - 4,808 entries sit under macOS home directories on the build machines, which belong to several distinct user
    accounts. **These paths are never reproduced here.** `tools/asamu-symbols` reduces them to
    module-relative labels.
  - The remaining 15 entries are SDK headers: 11 under `/Applications/Xcode.app/…` and 4 under
    `/Developer/SDKs/…`.
- Path strings in loadable data: **148 source-file paths** in `__TEXT,__cstring` (`__FILE__` arguments):
  - 137 are absolute build-machine paths: 136 PhysX (131 `.cpp`, 5 `.h`) and 1 UE3 Engine `.cpp`;
  - 11 are PhysX-relative `../../../…` header paths.

  The wide-string section holds no absolute path.
- Object timestamps (the `N_OSO` value is the object's mtime; the dates below are UTC):

| Group | Objects | Dates |
|---|---:|---|
| UE3 modules + External libs compiled by the CMake target `ASAMU-amd64` | 745 | 2017-04-24: 209 · 2017-04-25: 534 · 2017-04-27: 1 (`MacViewport.cpp.o`) · 2017-05-09: 1 (`ScaleformFullscreenMovie.cpp.o`) |
| PhysX archive members | 398 | 2012-09-14/20: 38 · 2013-02-06: 360 |
| Scaleform archive members (including Scaleform's own 2 zlib-support objects) | 187 | 2012-04-24 |

The 745 CMake objects comprise the UE3 modules (601) and the External libraries compiled in the same target
(lzopro 111, libogg/libvorbis 23, zlib 10). The build tree is a CMake tree named `cmake-build-amd64`, with
target `ASAMU-amd64`. That directory pattern is the default naming used by JetBrains CLion (TENTATIVE).

## 4. RTTI and vtables

### 4.1 Census (CONFIRMED)

| Symbol family | Count | Notes |
|---|---:|---|
| `__ZTV` vtables | 5,758 | 5,756 defined (2,901 in `__DATA,__const`, 2,855 in `__DATA,__data`) + 2 undefined (`__cxxabiv1` class/si-class typeinfo vtables from libstdc++) |
| `__ZTI` typeinfo | **2** | `std::exception`, `std::bad_alloc` only (defined in Core units) |
| `__ZTS` typeinfo names | **2** | same two |
| `__ZTT` VTTs / `__ZTC` construction vtables | 12 / 23 | the few classes with virtual bases (static-lighting mesh/mapping types) |
| `__ZThn` non-virtual thunks / `__ZTv` virtual thunks | 4,841 / 30 | multiple-inheritance adjustor thunks |

- **The typeinfo slot (address point − 8) is 0 in all 5,756 vtables**. RTTI is compiled out (`-fno-rtti`)
  for all game, engine and middleware code, so the vtable layout cannot tell you the dynamic type. The
  symbol names can. The offset-to-top slot is 0 for primary vtables; for the 12 classes with virtual bases it
  is preceded by vbase offsets.
- Primary-vtable slots: about 246,700–247,100 in total. The exact figure depends on how the end of a primary
  table is detected: the census script gives 246,883, and an independent reader that stops at the first entry
  that is not a function gives 246,737. Every non-pure slot resolves to a named function symbol (0 unresolved;
  no slot points into `__text` at an address without a symbol). Pure-virtual slots (bound to
  `___cxa_pure_virtual`) number 4,106–4,109 in primary vtables, out of 4,148 bind sites in all. (CONFIRMED to
  within that range; the per-class figures below are exact.)
- **UE3 native classes:** each of the **1,535** `PrivateStaticClass` classes has its own vtable (none
  missing). Together they hold 185,597 primary slots: minimum 76 (`UObject`), median 100, maximum 347
  (`AUDKVehicle`). They have **0 pure virtuals**, because every UObject class must be instantiable for its
  default object. 147 of them have one or more secondary vtables (native interfaces). The other 4,221
  vtables belong to Scaleform (1,845), UE3 `F*`/`T*` helper types and render commands, PhysX and so on.

### 4.2 Layout rules (CONFIRMED)

- Itanium C++ ABI. The vtable symbol points at `[offset-to-top][typeinfo=0]`. The **address point is
  symbol + 16**, and a decompiled call `(*(*this + D))(this, …)` uses slot index **D / 8** from the address
  point.
- Single inheritance is **prefix-stable**: a derived class's primary vtable begins with its parent's slots in
  the same order. The only slots that change name are 2 and 3, the complete and deleting destructors.

| Chain | Slots | Same method name in parent's slots | Overridden | New |
|---|---|---|---:|---:|
| `UObject` → `AActor` | 76 → 228 | 74/76 (the 2 misses are the destructors) | 19 | 152 |
| `AActor` → `APawn` | 228 → 316 | 226/228 | 65 | 88 |
| `APawn` → `AUDKPawn` | 316 → 319 | 314/316 | 29 | 3 |
| `AActor` → `AController` → `APlayerController` → `AUDKPlayerController` | 228 → 285 → 317 → 325 | all but the destructor pair | 27 / 19 / 13 | 57 / 32 / 8 |
| `UObject` → `UASAMUSystemSettingsManager` | 76 → 88 | 74/76 | 4 | 12 (its natives, §4.4) |

"Overridden" counts the parent slots whose pointer changes, **including** the destructor pair. For example,
`UObject` → `AActor` has 17 overridden methods besides the two destructors.

- Secondary vtables (interfaces) follow the primary one inside the same symbol, with a negative
  offset-to-top. Example: `AUDKPawn`'s primary vtable has 319 slots, followed by an `Interface_Speaker`
  sub-vtable at offset-to-top −0x248, whose entries are non-virtual thunks. The symbol-size difference
  therefore overstates the primary slot count.

### 4.3 Physics-relevant slots (CONFIRMED)

The table shows the pawn chain the player runs on. A row says which class introduced the slot and whose
implementation each vtable holds. Offsets are measured from the address point.

| Method | Slot | Offset | Introduced by | `AActor` | `APawn` | `AUDKPawn` (player at runtime) |
|---|---:|---:|---|---|---|---|
| `ProcessEvent` | 67 | +0x218 | UObject | AActor | AActor | AActor |
| `GetTerminalVelocity` | 80 | +0x280 | AActor | AActor | AActor | AActor |
| `FindBase` | 81 | +0x288 | AActor | AActor | AActor | AActor |
| `GetGravityZ` | 86 | +0x2B0 | AActor | AActor | AActor | **AUDKPawn** |
| `Tick` | 105 | +0x348 | AActor | AActor | APawn | APawn |
| `SetBase` | 120 | +0x3C0 | AActor | AActor | APawn | APawn |
| `TickAuthoritative` | 121 | +0x3C8 | AActor | AActor | AActor | AActor |
| `TickSpecial` | 123 | +0x3D8 | AActor | AActor | APawn | AUDKPawn |
| `setPhysics` | 132 | +0x420 | AActor | AActor | APawn | AUDKPawn |
| `performPhysics` | 133 | +0x428 | AActor | AActor | APawn | **AUDKPawn** |
| `processHitWall` | 136 | +0x440 | AActor | AActor | APawn | APawn |
| `processLanded` | 137 | +0x448 | AActor | AActor | APawn | APawn |
| `physFalling` | 138 | +0x450 | AActor | AActor | APawn | **AUDKPawn** |
| `physWalking` | 139 | +0x458 | AActor | AActor | APawn | APawn |
| `physicsRotation` | 142 | +0x470 | AActor | AActor | APawn | AUDKPawn |
| **`GetNetBuoyancy`** | **144** | **+0x480** | AActor | AActor (0x100AE1EF0) | **APawn (0x100AE1F00)** | APawn |
| `stepUp` | 146 | +0x490 | AActor | AActor | APawn | APawn |
| `physFlying` / `physSwimming` / `physSpider` / `physLadder` | 295–298 | +0x938–0x950 | APawn | — | APawn | APawn |
| `startNewPhysics` | 299 | +0x958 | APawn | — | APawn | APawn |
| `StartFalling` / `ShouldCatchAir` / `SetPostLandedPhysics` | 300 / 301 / 302 | +0x960 / 0x968 / 0x970 | APawn | — | APawn | APawn |
| `NewFallVelocity` | 305 | +0x988 | APawn | — | APawn | APawn |
| `MaxSpeedModifier` | 306 | +0x990 | APawn | — | APawn | APawn |
| `CalculateSlopeSlide` | 308 | +0x9A0 | APawn | — | APawn | AUDKPawn |
| `ApplyVelocityBraking` | 314 | +0x9D0 | APawn | — | APawn | APawn |
| `CalcVelocity` | 315 | +0x9D8 | APawn | — | APawn | **AUDKPawn** |
| `UpdateEyeHeight` | 318 | +0x9F0 | AUDKPawn | — | — | AUDKPawn |

`APawn::NewFallVelocity` (0x100ADF860) calls `[vtbl+0x480]` at 0x100ADF897, i.e. `GetNetBuoyancy`.
`AActor::physFalling` (call site 0x100AE093B) and `APawn::physSwimming` (call site 0x100AE21D3) make the
same call. This confirms the slot that NATIVE_PHYSICS.md §4 relies on. `AActor::moveSmooth` and
`APawn::IsHumanControlled` are **not** virtual; they are called directly. `PostPhysFalling` is a controller
virtual: `AController` slot 266 (+0x850), overridden by `AUDKBot`.

### 4.4 Script natives, script events and vtables

- **Natives** (script → C++) are `exec<Func>(FFrame&, void*)` thunks. Of the 2,505 thunks, only **2 are
  virtual**: `AController::execPollMoveTo` and `AController::execPollMoveToward`, at controller vtable slots
  272/273 (+0x880/+0x888). They are registered as virtual pointers-to-member `0x881`/`0x889`. The thunks are
  reached through `GNatives[]` or `GNativeLookupFuncs` (§6), never through a C++ vtable. (CONFIRMED)
- **Events** (C++ → script) are inline, non-virtual `event<Name>()` wrappers that call
  `ProcessEvent(FindFunctionChecked(<PKG>_<Name>), &Parms)`. The call goes through UObject vtable slot 67,
  which `AActor` overrides. Which script function runs is decided at runtime by FName lookup on the object's
  class. To resolve it statically, find the function of that name in the script class chain (for example
  `ASAMUPawn` → `UTPawn` → `UDKPawn`), not in the vtable. Only **5** UE3 `event*` wrappers occupy vtable
  slots, all native-interface implementations: `APawn::eventSpeak`, and `eventNotifyPathChanged` in
  `AController`, `ACrowdAgentBase`, `AGameCrowdPopulationManager` and `APylon`. Each is also reached through a
  non-virtual thunk in an interface sub-vtable. (CONFIRMED. An earlier version said 8; that count also took in
  3 PhysX methods named `event` (`NxFoundation::Observable`, `NxFoundation::FoundationSDK`,
  `BodyPairEffector`), which are not UnrealScript events.)
- **Where vtables do matter for natives:** the C++ implementation a thunk calls. Three counting methods
  give somewhat different splits of the 2,505 thunks:
  - Name matching that includes ancestor classes (census script): **1,121** call a same-named method in the
    class's vtable (virtual), 875 call a non-virtual one, and 509 have no same-named method (the body is
    inside the thunk, as with the `UObject` operators and iterators).
  - Name matching against the thunk's own class only (verification pass): 1,074 / 792 / 639.
  - Disassembly of all 2,505 thunks (verification pass): 1,064 contain a `call/jmp [reg+disp]` vtable call.
    1,023 of those have no direct call to a same-named function, and 41 have both. 653 contain only a direct
    same-named call, and 788 contain neither.

  The share is STRONG: about 40–45% of thunks land on a virtual method. The exact counts are TENTATIVE
  because they depend on the method. Example verified in disassembly:
  `UASAMUSystemSettingsManager::execInit` ends with `jmp [rax+0x260]`. That is slot 76 =
  `UASAMUSystemSettingsManager::Init`, the first of 12 virtual natives occupying slots 76–87.
  `execSetLanguage` calls `SetLanguage` directly; that function is declared `static` in script.
- **Rule (STRONG)**: across the 7 native script packages:
  - 1,155 of the 1,183 plain (non-final, non-static) native functions have a virtual C++ method of the same
    name;
  - `final` natives are mostly non-virtual (734 non-virtual, 71 virtual, 106 inline);
  - `static` natives are never virtual (138 non-virtual, 247 inline).

  This matches the UE3 header generator, which emits `virtual` only for natives that are neither final nor
  static. The category totals (1,183 plain, 911 final, 385 static) are CONFIRMED. The verification pass
  matched only the C++ class with the same name as the script class and got 1,110 virtual plain natives,
  61 virtual final ones and 0 virtual static ones. So the rule holds under both methods, but the exact counts
  are TENTATIVE.
- **Runtime vtable of script classes (CONFIRMED by code).** `UClass::Bind` (0x100096120) does the
  following when a class's `ClassConstructor` (`UClass+0x1F8`) is null: it calls `SuperClass->Bind()`
  (`UClass` vtable +0x270), copies the superclass's `ClassConstructor`, and ORs in its cast flags
  (`+0x13C`). The field names are TENTATIVE, read from the UE3 layout; the copy itself is CONFIRMED. A script-only class is therefore constructed by its nearest native ancestor's
  `InternalConstructor` and carries that ancestor's vtable. Example (CONFIRMED in disassembly):
  `AUDKPawn::InternalConstructor` (0x100F6BAD0) stores `__ZTV8AUDKPawn+0x10` (the primary address point) and
  `+0xA18` (the `Interface_Speaker` sub-vtable) into the new object. For the 172 `asamu` classes this gives 36
  distinct native vtables. That figure is STRONG: it is computed from the script super chains (see
  Reproduce), and the verification pass reproduced every count below with the current `asamu-inspect`. The
  most common targets are:
  - `USequenceAction` 49, `UObject` 20, `USequenceEvent` 20, `AActor` 12, `UGFxMoviePlayer` 11;
  - `AUDKPawn` 5 (including `ASAMUPawn`), `AUDKPlayerController` 2 (including `ASAMUPlayerController`);
  - `AUDKWeapon` 1 (`GrappleGun`), `ACamera` 1 (`ASAMUCamera`), `UPlayerInput` 1 (`ASAMUPlayerInput`),
    `AFrameworkGame` 3 (`ASAMUGameInfo`…);
  - `UASAMUSystemSettingsManager` 1 (its own native class).

### 4.5 Can vtables resolve indirect calls?

**Yes, deterministically, once the receiver's class is known (CONFIRMED method).** For the player, the
dynamic classes are fixed by item 4: `AUDKPawn`, `AUDKPlayerController`, `AUDKWeapon` and so on. To
resolve a call:

```
target = u64 at file offset (vtable_symbol + 16 + D − 0x100000000)
```

then look the address up in `nm -n`. Even when the dynamic class is unknown, the static type's vtable gives
the **declared method** at displacement D, because layouts are prefix-stable. Only the choice of override
then depends on runtime.

Heuristic measurement on `Engine/Src/UnPhysic.cpp` (all of its 75 functions, TENTATIVE). The unit has 261
direct calls, 45 `call reg` (member-pointer and `GNatives` dispatch) and 148 vtable-style
`call/jmp [reg+disp]`. Of these:

- about 96 load the vtable from `this` and resolve mechanically through the method's own class (42 distinct
  slots);
- about 45 go through another object (Controller, PhysicsVolume, Base, the hit actor) and need that
  field's declared type;
- 7 did not match the simple load pattern.

NATIVE_PHYSICS.md resolved the ones on the player path by hand.

The following are **not** vtable calls, and the vtable cannot resolve them:

- `call rax` in exec thunks: script parameter evaluation through `GNatives[*Code++]` (§6);
- `ProcessEvent` targets (name lookup);
- Scaleform and PhysX callback tables.

## 5. String census

Method (CONFIRMED counts):

- **ASCII**: every NUL-separated literal in `__TEXT,__cstring` (`S_CSTRING_LITERALS`).
- **Wide**: aligned scans of `__TEXT,__const` for runs of ≥ 4 code units followed by a NUL unit, in two
  variants:
  - UTF-32LE, with 4-byte units in 0x09/0x0A/0x0D, 0x20–0x7E or 0xA0–0x2FFF;
  - UTF-16LE, with 2-byte printable units.

The other sections were scanned the same way. Categories are regex heuristics, may overlap, and are
TENTATIVE in their boundaries.

### 5.1 TCHAR size (CONFIRMED)

| Evidence | Result |
|---|---|
| UTF-32LE literals in `__TEXT,__const` | **15,022** (12,133 distinct; 1,665,120 bytes = 36.5% of the section; 21 contain non-ASCII characters) |
| UTF-16LE literals (≥ 4 units) anywhere in `__TEXT,__const`, `__TEXT,__cstring`, `__DATA,__const`, `__DATA,__data` | **0** |
| UTF-32LE in `__DATA,__data` (writable wide arrays) | 22 |
| Native class names (`TEXT(#Class)` in `IMPLEMENT_CLASS`) found as UTF-32 literals | 1,535 / 1,535 (1,534 by exact scan; the last is preceded by a stray `$` code unit and found by direct search) |
| Mangled signatures | `GetPrivateStaticClass<Class>(wchar_t const*)`, `ScriptConsoleExec(wchar_t const*, …)`; ANSI ↔ wide helpers `appMacWideCharToMultiByte` (`Mac/Src/MacWideChar.mm`) |

So **TCHAR = `wchar_t` = 4 bytes (UTF-32LE)** on this Mac build. The build does not use `-fshort-wchar`.
This affects only in-memory strings. The cooked package format (FString as ANSI or UTF-16) is unchanged;
see PACKAGE_ANALYSIS.md. Wide literals sit in a regular section and are **not de-duplicated** across
compilation units: the section name `Engine.Engine` occurs 9 times, and the most duplicated literal occurs 330
times. The ASCII literals are fully de-duplicated (0 repeats).

### 5.2 Categories

| Category | ASCII (`__cstring`, 11,215 literals; 11,142 printable ASCII, 73 with high bytes, 72 of them valid UTF-8) | Wide (UTF-32, 15,022) |
|---|---:|---:|
| printf-style format strings | 419 — mostly Scaleform ActionScript errors and PhysX `checkValid()` messages | 2,892 — UE3 log, warning and error formats |
| messages (contain a space, > 12 chars) | ~1,683 | ~3,667 |
| native-lookup names `<Class>exec<Func>` (ANSI keys of the `G…Natives` tables) | **2,468** = number of decoded table entries | — |
| native class registration names | — | 1,535 |
| `b`-prefixed bool identifiers (config keys and properties) | — | 221 |
| `Package.Class` section- or class-path shaped | — | 81 |
| UPPERCASE tokens (console and exec keywords) | — | 753 |
| `.ini` references / relative `..\` or `../` path fragments / bare extensions | — | 34 / 15 / 29 |
| source-file paths (`__FILE__`) | 148 (137 absolute, 11 relative; §3) | 0 absolute |
| other identifiers and uncategorised | remainder | remainder |

Generic illustrative examples:

- the wide config section name `Engine.Engine` (9 duplicate literals);
- the wide class registration name `UASAMUSystemSettingsManager`;
- the ANSI lookup key `UASAMUSystemSettingsManagerexecInit`;
- the ANSI registrant key `ASAMUSystemSettingsManager`;
- wide log formats of the shape "`<text> %s <text> %d`".

No other string content is published.

## 6. How a script-callable native reaches C++

Everything below is CONFIRMED from the code of `GRegisterNative`, `UObject::execHighNative*`,
`UFunction::Bind`, `UClass::Bind` and `AutoInitializeRegistrantsASAMU`, unless it is marked otherwise.

### 6.1 Runtime tables

| Object | Address | Shape |
|---|---|---|
| `GNatives` | 0x1023D13C0 (`__common`) | 4,096 entries × 16 bytes. Each entry is an Itanium pointer-to-member `{ptr, adj}`; `ptr` odd means virtual, at vtable offset `ptr − 1`. On first use `GRegisterNative` fills every entry with `UObject::execUndefined` (0x1000B22C0) |
| `GNativeDuplicate` | 0x1023E13C0 | last index registered twice. It is written when the slot already holds something other than `{execUndefined, 0}`, and also when the index is out of range (negative other than −1, or above 0x1000) |
| `GCasts` | 0x1023E13D0 (`__common`) | 255 entries × 16 bytes (ends at `GCastDuplicate`, 0x1023E23C0). Conversion natives are registered here by an inlined `GRegisterCast`. `UObject::execPrimitiveCast` (token 0x38) reads one byte from `FFrame::Code` and dispatches through `GCasts[byte]`, using the same odd-pointer virtual rule |
| `GNativeLookupFuncs` | 0x1023D12B0 | `TMap<FName, FNativeFunctionLookup*>` (from the mangled `TSet<TMapBase<FName, FNativeFunctionLookup*>>::Add`). Referenced only by its initialiser, the 8 `AutoInitializeRegistrants<Pkg>` that own natives, and `UFunction::Bind` |
| `G<pkg><Class>Natives` | `__DATA,__data` | 324 tables of 24-byte `FNativeFunctionLookup {const ANSICHAR* Name; Native Pointer;}`, null-terminated; 2,468 entries in all (decoded by `asamu-symbols`, SYMBOL_ANALYSIS.md) |
| `int<Class>exec<Func>` | `__DATA,__data` | 2,510 16-byte member pointers (`IMPLEMENT_FUNCTION`) passed to `GRegisterNative(iNative, ptr)` from static initialisers |
| `FFrame::Code` | `FFrame + 0x28` | bytecode cursor read by the dispatchers |

**Fixed-index registrations recovered from code:**

- **2,127** out-of-line `GRegisterNative` calls (`call` or tail `jmp`): 2,074 pass `iNative = −1`, so the
  native is bound by name only; 53 pass a fixed index.
- **253** inlined registrations in Core's `__GLOBAL__sub_I_UnCorSc.cpp`, where `GRegisterNative` is defined
  and inlined. These are stores to constant addresses `GNatives + 16·i`.
- **130** further `UObject` `int` globals whose `−1` registration was compiled away. STRONG: an inlined
  call with constant −1 is dead code. 38 of the 130 are conversion natives (for example
  `execStringToByte`). The same initialiser stores them into `GCasts` slots 0x36–0x60 instead (CONFIRMED).
  The remaining 92 are bound by name only.

The fixed indices are therefore 306 in total, with **no collisions**:

| Index band | Count | Meaning |
|---|---:|---|
| 0x00–0x5F | 80 | bytecode expression handlers, e.g. `0x00` LocalVariable, `0x1B` VirtualFunction, `0x1C` FinalFunction, `0x37` GlobalFunction, `0x42` DelegateFunction |
| 0x60–0x6F | 16 | `execHighNative0…15` |
| 0x70–0xFF | 131 | one-byte natives, e.g. `0x81` = `Not_PreBool` (129), `0x70` = `Concat_StrStr` |
| 0x100–0xF83 | 79 | extended natives, e.g. `0x115` = `Actor.Trace` (277), `0x10A` = `Move`, `0x12A` = `SetBase`, `0xF81` = `MoveSmooth`, `0xF82` = `SetPhysics`, `0xF83` = `AutonomousPhysics` |

**Cross-check against the packages (CONFIRMED).** `asamu-inspect class --json` was run over every class of
Core, Engine, GameFramework, UDKBase, IpDrv, GFxUI and OnlineSubsystemSteamworks, plus
`asamu.ASAMUSystemSettingsManager`. That covers 7,917 functions, of which 2,479 are `Native`.

- **202** functions carry a non-zero `iNative`. **All 202** match the registration at that index on both
  class and function name, with 0 disagreements.
- 104 registered indices have no script function: the 96 below 0x70 and 8 latent `execPoll*` pollers.
- The other **2,277** natives have `iNative = 0` and are bound by name. 2,266 of them have an ANSI
  `<Class>exec<Func>` key. The remaining 11 are declared in interfaces (`OnlineAuthInterface`,
  `UIDataStoreSubscriber`/`Publisher`), and implementing classes provide the thunks, e.g.
  `UOnlineAuthInterfaceImpl::execAllClientAuthSessions`.

### 6.2 Recipe: bytecode → C++ function

1. **Read the token.** Every token goes through `GNatives[*Code++]`:
   - **T < 0x60**: an expression token. Function calls use `0x1B` VirtualFunction (name lookup on the
     object's class at run time), `0x1C` FinalFunction (an object reference to the `UFunction`), `0x37`
     GlobalFunction or `0x42` DelegateFunction. Resolve the referenced `UFunction` with `asamu-inspect`
     and continue at step 3. `0x38` PrimitiveCast is the exception: the next byte indexes `GCasts`, not
     `GNatives` (§6.1).
   - **0x60 ≤ T ≤ 0x6F**: `execHighNative(T−0x60)` reads one more byte B, and the index is
     `((T − 0x60) << 8) | B`. Example: `61 15` → 0x115 = 277 = `Actor.Trace`.
   - **T ≥ 0x70**: the native index is T.
2. **Index → thunk.** Look the index up in the recovered registration table, which yields
   `int<Class>exec<Func>`. Decode that 16-byte member pointer: an even `ptr` is the thunk address, which
   `nm -n` names `<Class>::exec<Func>(FFrame&, void*)`; an odd `ptr` is a virtual slot, as for the 2
   `PollMoveTo*`. Independent route: the script function whose `iNative` equals the index has the same
   class and name (202/202).
3. **`UFunction` → thunk** (calls by reference, and natives with `iNative = 0`). If the function lacks the
   `Native` flag, its `Func` is `UObject::ProcessInternal`, which interprets its bytecode. If `iNative ≠ 0`,
   `Func = GNatives[iNative]`, as in step 2. Otherwise `UFunction::Bind` converts the names to ANSI and
   finds the class's table in `GNativeLookupFuncs`, keyed by the class FName without its prefix. For
   ASAMU that key is `ASAMUSystemSettingsManager` → `GasamuUASAMUSystemSettingsManagerNatives`. It then
   matches the entry `<PrefixedClass>exec<Func>`. To do this offline, decode the `G…Natives` tables with
   `asamu-symbols` (106 entries point at an ancestor's thunk; take the pointer, not the name). If the
   declaring class is an interface, use the implementing class's table.
4. **Thunk → implementation.** In the thunk:
   - repeated `call rax` blocks evaluate the parameters (`P_GET_*` → `GNatives` dispatch);
   - a direct `call` goes to a non-virtual C++ method;
   - `call/jmp [rax+D]` goes to a virtual one. Resolve it with §4.5, using the vtable of the receiver's
     **nearest native class** (`ASAMUPawn` → `AUDKPawn`, and so on).

   Under the STRONG rule of §4.4, a plain (non-final, non-static) native will usually land on a virtual
   method.
5. **Script events in the opposite direction** (C++ → script): `event<Name>` → `ProcessEvent`
   → FName lookup. Read `<PKG>_<Name>` from the call site and find that function in the script class chain.

ASAMU worked example (CONFIRMED). All 13 `ASAMUSystemSettingsManager` natives have `iNative = 0`, and their
13 `int` registrations pass −1 from `__GLOBAL__sub_I_ASAMU.cpp`. `AutoInitializeRegistrantsASAMU` does the
following:

- creates the class with the wide package name `asamu`, in lower case like the package export in
  `Startup.upk`;
- adds `GNativeLookupFuncs[FName("ASAMUSystemSettingsManager")] = GasamuUASAMUSystemSettingsManagerNatives`.

`AutoGenerateNamesASAMU` is an empty function, consistent with the absence of `ASAMU_*` FNames. This shows
how the natives bind even though `ASAMU` is listed under `NonNativePackages`: binding is by class name, not
by package. It answers the corresponding UNKNOWN in SYMBOL_ANALYSIS.md: STRONG for the lookup path,
CONFIRMED for the registration.

## 7. Toolchain identification

| Evidence | Reading | Confidence |
|---|---|---|
| `___lzo_copyright` (`__TEXT,__const`) embeds the compiler's `__VERSION__`: "llvm-gcc 4.2.1 Compatible **Apple LLVM 8.1.0 (clang-802.0.42)**". The lzopro objects belong to the 2017 CMake target (111 objects, 2017-04-24) | the main build used Apple clang 802.0.42, the compiler of Xcode 8.3.x | CONFIRMED (string, object provenance); STRONG (Xcode 8.3.x mapping) |
| `LC_VERSION_MIN_MACOSX` SDK 10.12; `N_SOL` headers from `…/MacOSX10.12.sdk/…`; dylib current versions (libSystem 1238.50.2, AppKit 1504.82.104, CoreFoundation 1349.64.0) | macOS 10.12 SDK, consistent with Xcode 8.3 (10.12.4-era stubs) | CONFIRMED (SDK); TENTATIVE (point release) |
| `___clang_call_terminate` (1), `___cxx_global_var_init*` (62), `__GLOBAL__sub_I_<file>` (599), LLVM GlobalOpt `.b` shrink-to-bool statics (11 regular symbols) | clang/LLVM codegen | CONFIRMED |
| `__objc_imageinfo` flags 0x40 | flag emitted by Xcode 8+ clang (category class properties) | TENTATIVE |
| `LC_LOAD_DYLIB /usr/lib/libstdc++.6.dylib`. All 15 C++ runtime imports (`operator new/delete`, `___cxa_*`, `___gxx_personality_v0`, `std::terminate`, `__cxxabiv1` typeinfo vtables) come from it. **0** `std::__1::` (libc++) symbols, and no `c++/v1` header in the debug map | **libstdc++** (`-stdlib=libstdc++`, GCC 4.2.1-era headers) | CONFIRMED |
| `LC_UNIXTHREAD`, no `LC_MAIN`/`LC_SOURCE_VERSION`; deployment target 10.7 | ld64 emits `LC_MAIN` only for deployment targets ≥ 10.8, so a modern ld64 still produces this classic layout | STRONG |
| RTTI off (0 typeinfo in vtables); exceptions on in parts (18,233 LSDA labels, personality import) | `-fno-rtti`; exceptions not globally disabled | CONFIRMED |
| `N_OSO` object paths under `cmake-build-amd64/CMakeFiles/ASAMU-amd64.dir/<Module>/Src/*.cpp.o`; no `DT*` keys in Info.plist | CMake + command-line clang, not an Xcode project build | CONFIRMED (CMake); TENTATIVE (no Xcode project) |
| Prebuilt archives: 104 GCC-style `__GLOBAL__I_*` initialisers (99 Scaleform). Debug-map SDK headers, attributed through the enclosing `N_OSO`: Scaleform `libgfx.a` (2012-04) uses `/Developer/SDKs/MacOSX10.6.sdk` with `c++/4.0.0`. PhysX `libPhysXCooking.a`/`libPhysXExtensions.a` (2012-09) use an `Xcode.app`-hosted `MacOSX10.6.sdk` with `c++/4.2.1`. PhysX `libPhysXCore.a`/`libLowLevel.a` (2013-02) use `MacOSX10.8.sdk` and the Xcode toolchain's `lib/clang/4.1` | three older environments: a pre-Xcode-4.3 `/Developer` install for Scaleform, an Xcode 4.x install for the 2012 PhysX members, and Apple clang 4.1 for the 2013 PhysX members. Apple clang 4.1 is the Xcode 4.5 generation; Xcode 4.6 shipped clang 4.2 | STRONG (different toolchains); TENTATIVE (exact compilers and Xcode releases) |
| Bundled `libSDL2` current version 5.0.0 | SDL 2.0.5 | TENTATIVE |

**Correction to the previous version of this file.** It guessed "libstdc++ and an OS X 10.6 minimum suggest
an older Xcode/GCC-era toolchain". The minimum in the binary is **10.7** (the 10.6 figure comes from
`Info.plist`). The compiler is a 2017 Apple clang. libstdc++ was a deliberate `-stdlib` choice, still
possible with the 10.12 SDK; it does not indicate an old GCC. The previous version also listed `__ZTI*` as
present for `UASAMUSystemSettingsManager`. That was wrong: the class has a vtable but no typeinfo.

## 8. Findings by confidence

### CONFIRMED

- Header, load commands, sections, `__LINKEDIT` layout and fixup counts as tabulated in §1–§2. The executable
  is not signed and has no `LC_CODE_SIGNATURE`; the file ends at the string table. It is not stripped
  (135,093 regular symbols, 400,188 STABS), has no DWARF, and ships no dSYM.
- Entry `start` (0x100004D60) → `_main` (`Mac/Src/LinuxImpl.cpp`) → `appMacCallGuardedMain` →
  `GuardedMain`. There are 765 module initialisers.
- RTTI is off. There are 5,756 defined vtables, each with a null typeinfo slot. Each of the 1,535 native
  classes has its own vtable, with no pure virtuals. Layouts are prefix-stable. Slot tables are as in §4.3.
- Script-only classes inherit the native ancestor's `ClassConstructor` (`UClass::Bind`), so they inherit
  its vtable.
- 2 of 2,505 exec thunks are virtual; script events go through `ProcessEvent` (slot 67).
- TCHAR = 4-byte `wchar_t`: 15,022 UTF-32 literals and 0 UTF-16 ones. There are 2,468 ANSI `<Class>exec<Func>`
  keys and 148 `__FILE__` paths (137 absolute: PhysX plus 1 UE3).
- Compiler string Apple LLVM 8.1.0 (clang-802.0.42), SDK 10.12, libstdc++, CMake build tree, object dates
  2017-04-24 … 2017-05-09 for the CMake objects and 2012–2013 for the prebuilt archives.
- `GNatives` (4,096 × 16 B), `GNativeLookupFuncs` (`TMap<FName, FNativeFunctionLookup*>`), the
  `execHighNative` index formula, 306 fixed registrations with no collisions, and a 202/202 match with
  package `iNative`. ASAMU natives are bound by the class-name key `ASAMUSystemSettingsManager`.

### STRONG

- Xcode 8.3.x as the IDE/toolchain release, from the clang-802.0.42 to Xcode 8.3 mapping.
- Roughly 40–45% of exec thunks land on a virtual C++ method. Plain natives map to virtual C++, and
  final/static natives map to non-virtual C++. Three counting methods agree on this; their exact numbers do not
  (TENTATIVE, §4.4).
- 130 `UObject` natives registered with −1 had their inlined `GRegisterNative` optimised away. That 38 of
  them are instead stored into `GCasts` is CONFIRMED.
- The 36-native-vtable mapping of the 172 `asamu` classes. It is computed from `asamu-inspect` super chains,
  and the verification pass reproduced it exactly.
- `UFunction::Bind` name lookup is keyed by the unprefixed class FName and matches
  `<PrefixedClass>exec<Func>`. The registrant side is CONFIRMED; the lookup side is read from call and
  reference structure.
- Prebuilt Scaleform and PhysX archives came from older toolchains (initialiser naming, SDK header paths).

### TENTATIVE

- Indirect-call resolvability estimate for `UnPhysic.cpp` (~96 of 148 resolvable from `this` alone): this
  comes from a pattern-matching heuristic, not data flow.
- Exact thunk-implementation counts (1,121 / 875 / 509 with ancestor name matching; other methods in §4.4)
  and the exact virtual counts per native kind (1,155 vs 1,110 plain).
- Total primary-vtable slot count (246,883 ± about 0.2%, depending on how a table's end is detected).
- CLion as the CMake front end; SDL 2.0.5; `__objc_imageinfo` 0x40 as an Xcode 8 marker; the reading of
  `OpenGLD3DBytecodeConverter.cpp`.
- String category boundaries in §5.2. The totals are CONFIRMED; the categories come from regex heuristics.

### UNKNOWN

- The exact point release of Xcode/SDK (8.3.2 vs 8.3.3), and the exact compilers of the prebuilt archives.
- Whether any natives are rebound at run time beyond what `UFunction::Bind` does (for example script
  patching via `FScriptPatchWorker`). Not examined.
- The full static types of the ~45 non-`this` virtual receivers in `UnPhysic.cpp` (needs class field
  layouts).

## 9. Reproduce

```bash
B="$ASAMU_ORIGINAL_DIR/A Story About My Uncle.app/Contents/MacOS/ASAMU"
shasum -a 256 "$B"; otool -hv "$B"; otool -l "$B"; size -m -l "$B"; codesign -dv "$B"
otool -L "$B"; plutil -p "$(dirname "$B")/../Info.plist"
dyld_info -fixups "$B" | awk 'NR>3{print $4}' | sort | uniq -c      # 290128 rebase / 4177 bind / 430 lazy-bind
dyld_info -exports "$B" | grep -c 0x                                 # 76584
# (dyld_info prints wrong symbol names for this binary's classic bind stream; the counts match an
#  independent opcode decode. Weak-bind counts come from that decode.)
# stubs that name an imported symbol (434) vs weak definitions inside the executable (2737)
otool -Iv "$B" | awk '/^Indirect symbols for/{s=$4; next} s=="(__TEXT,__stubs)" && $1 ~ /^0x/ {print $3}' \
  | sort > stubs.txt; nm -u "$B" | sort | comm -12 stubs.txt - | wc -l              # 434
nm "$B" | grep -c ' __ZTV'; nm "$B" | grep -E ' __ZT[IS]'            # 5758 (5756 defined); 2 + 2
nm "$B" | grep -cE ' ___clang_call_terminate$| ___cxx_global_var_init'   # 63
nm "$B" | grep -c 'NSt3__1'                                          # 0 (no libc++)
strings -a "$B" | grep 'Apple LLVM'                                  # compiler string inside ___lzo_copyright
nm -ap "$B" | awk '$5=="OSO"' | wc -l                                # 1330 objects (value column = mtime)

# one vtable slot: AUDKPawn + 0x9D8 (file offset = vmaddr - 0x100000000)
V=$(nm "$B" | awk '$3=="__ZTV8AUDKPawn"{print $1}')
xxd -s $((0x$V + 16 + 0x9D8 - 0x100000000)) -l 8 -e -g 8 "$B"        # -> 0000000100f6bca0
nm -n "$B" | grep -i '^0000000100f6bca0'                             # AUDKPawn::CalcVelocity

# GetNetBuoyancy virtual call in NewFallVelocity
objdump -d --x86-asm-syntax=intel --start-address=0x100adf860 --stop-address=0x100adf900 "$B" | grep '0x480'

# TCHAR: UTF-32 vs UTF-16 literals in __TEXT,__const (file offset 23269792, size 4557120)
python3 -I -c 'import re,sys
d=open(sys.argv[1],"rb").read()[23269792:23269792+4557120]
print(sum(1 for m in re.finditer(rb"(?:[\x09\x0a\x0d\x20-\x7e]\x00\x00\x00){4,}\x00\x00\x00\x00",d) if m.start()%4==0),
      sum(1 for m in re.finditer(rb"(?:[\x20-\x7e]\x00){4,}\x00\x00",d) if m.start()%2==0))' "$B"
# -> 15006 0  (the census figure 15,022 also admits U+00A0..U+2FFF code units)

# native-index cross-check: package side
cargo run -p asamu-inspect -- --json class "$ASAMU_ORIGINAL_DIR/A Story About My Uncle.app/Contents/Resources/ASAMU/CookedMac/Core.u" Object
# (functions[].native_index; e.g. Not_PreBool = 129)
# nearest native class of a script class: first entry of [class] + super_chain whose A/U-prefixed name has a __ZTV
cargo run -p asamu-inspect -- --json class "$ASAMU_ORIGINAL_DIR/A Story About My Uncle.app/Contents/Resources/ASAMU/CookedMac/Startup.upk" asamu.ASAMUPawn
```

The code-side recoveries live in local helper scripts under the ignored `research/local/binary/`:

- the `GRegisterNative` scan (`E8`/`E9` rel32 calls to 0x1000B21F0, preceded by `mov edi, imm32` and
  `lea rsi, [rip+int…]`);
- the inlined `UnCorSc.cpp` stores to `GNatives+16·i`;
- the vtable census, which takes the address point from the `[0][0]` header pair and stops at a
  negative-offset secondary header;
- the classic bind-opcode decoder;
- the string scanner.

They are not committed. The algorithms above are the specification. Porting them into `tools/asamu-symbols`
would make the native-index table reproducible from the repository; see the open questions.

## Verification log

**2026-10-09, independent re-derivation.** A second pass did not reuse the census scripts. It re-derived the
claims with `otool`, `nm`, `objdump`, `dyld_info`, `codesign`, `lipo`, the current `asamu-inspect`, and new
throwaway decoders for the dyld opcode streams, vtables, strings and registration code (local only).

Reproduced exactly:

- **Container:** file hash, size and UUID; all 28 load commands and their sizes; every section address, size,
  offset and alignment; the gap-free `__LINKEDIT` layout ending at EOF; and the free header space and section
  packing.
- **Linker metadata:** min OS 10.7 / SDK 10.12 against the plist's 10.6; `LC_UNIXTHREAD` rip; no code
  signature; the dylib table; and the bundled dylibs' architectures, minimum OS and missing signatures.
- **Fixups:** rebase, bind, weak-bind, lazy-bind and export counts, with their per-section splits (own opcode
  decoder). Also that `dyld_info` mislabels the bind targets.
- **Symbols and debug map:** function starts (85,918) and data-in-code (417, all jump tables); STABS
  totals and per-type counts; debug-map path counts (absolute entries, distinct names, 4 build accounts, SDK
  entries); and `N_OSO` dates per group (745 / 398 / 187, and the 601 + 144 split of the CMake target).
- **Startup:** module-initialiser classes (599 / 62 / 104, of which 99 Scaleform); and `start` → `_main`
  (LinuxImpl) → `appMacCallGuardedMain` (Mac.cpp) → `GuardedMain` (Launch.cpp).
- **Typeinfo and vtables:** the `__ZTV` / `__ZTI` / `__ZTS` / `__ZTT` / `__ZTC` / `__ZThn` / `__ZTv` counts.
  A typeinfo slot that is null and neither rebased nor bound in all 5,756 vtables. 1,535 native classes, all
  with vtables, 0 pure virtuals, 185,597 slots, min 76, median 100, max 347, 147 with secondary tables. The
  §4.2 chain table, and every slot and address in §4.3.
- **Disassembly:** the `GetNetBuoyancy` call sites; the `execInit` tail call through +0x260; `UClass::Bind`;
  the virtual member pointers 0x881/0x889.
- **Strings:** 15,022 / 12,133 UTF-32 literals, 0 UTF-16, 11,215 ASCII, 2,468 lookup keys, the 148 / 137 /
  136 `__FILE__` paths, the 1,534 + 1 class-name literals, the 330 maximum duplicates; and the compiler string
  lying inside `___lzo_copyright`.
- **Native tables:** 324 tables / 2,468 entries / 106 ancestor pointers; and `GRegisterNative`'s 4,096 ×
  16-byte fill.
- **Native indices:** 2,127 calls (2,074 with −1, 53 fixed) + 253 inline stores = 306 indices in the stated
  bands, with no collisions. 7,917 functions / 2,479 native / 202 of 202 matching / 104 unused / 2,277 bound
  by name (2,266 + 11 interface).
- **ASAMU:** the registrant key and the lowercase package literal.
- **`UnPhysic.cpp`:** the 75 / 261 / 45 / 148 call counts.

Corrected in this pass:

- The stubs naming imports are 434, not 430 (§2.2).
- The non-pure bind breakdown is now listed in full.
- UE3 event wrappers in vtables are 5, not 8 (§4.4).
- The thunk and rule counts depend on the counting method and are now TENTATIVE (§4.4).
- The primary-slot total is given as a range.
- The 36-vtable mapping moved from TENTATIVE to STRONG.
- New: `GCasts` and token 0x38; the `GNativeDuplicate` out-of-range case; the per-archive SDK attribution;
  Apple clang 4.1 is Xcode 4.5, not "4.5/4.6".

Not re-derived:

- The §5.2 category counts (heuristic; spot checks were close: 34 `.ini`, 419 ASCII formats, about 2,850
  wide formats, 222 `b`-identifiers).
- The 96 / 45 / 7 receiver split in `UnPhysic.cpp` (still TENTATIVE).

## Open questions

1. Port the `GRegisterNative`/inline-store recovery and the vtable reader into `tools/asamu-symbols`, so that
   the 306-entry index table and the slot tables are generated and checked like `summary.json`.
2. SYMBOL_ANALYSIS.md says "17 GCC `.b`-suffixed local statics". Both passes count 11 regular `.b` symbols
   (`nm | grep -c '\.b$'`), plus 11 matching `N_STSYM` stabs (22 including stabs), so neither reading gives 17.
   Also, the `.b` suffix comes from LLVM GlobalOpt's shrink-to-bool, not from GCC. The owner of that file
   should re-check the figure.
3. SYMBOL_ANALYSIS.md's UNKNOWN on how the non-native `ASAMU` package binds `UASAMUSystemSettingsManager` is
   answered in §6.2 (class-name keyed `GNativeLookupFuncs`). The owner may want to cross-reference it.
4. Field-type tables for `AActor`/`APawn`/`AController` (from the script property layout that the
   OBJECT_FORMAT.md work decodes) would turn the remaining non-`this` virtual receivers into mechanical
   resolutions.
