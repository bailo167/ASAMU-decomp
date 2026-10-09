# UE3 package analysis (file version 868)

Evidence source: the 42 `.u` / `.upk` / `.asamu` files under
`~/Library/Application Support/Steam/steamapps/common/A Story About My Uncle/A Story About My Uncle.app/Contents/Resources/ASAMU/CookedMac`
(and its `Maps/` subfolder) of the legitimately owned Mac install (Steam build 1822049), parsed read-only by our own
reader `crates/asamu-ue3`. Nothing below is copied package content; it is structure, counts and sizes.

Reproduce:

```sh
cargo run -p asamu-inspect -- census --deep "<CookedMac>"          # per-package table + full parse of all 42
cargo run -p asamu-inspect -- package "<CookedMac>/Startup.upk"    # summary, chunks, table extents, findings
cargo run -p asamu-inspect -- objects --top-level "<CookedMac>/Startup.upk"
cargo run -p asamu-inspect -- map "<CookedMac>/Maps/AG-Workshop.asamu"
cargo test -p asamu-ue3 --test real_data -- --nocapture           # asserts every claim marked (T) below
cargo test -p asamu-ue3 --test flags_real_data                     # asserts every claim marked (F) below
```

`(T)` marks a fact asserted by `crates/asamu-ue3/tests/real_data.rs` against all 42 packages (the test skips when
the install is absent). `(F)` marks a flag fact asserted by `crates/asamu-ue3/tests/flags_real_data.rs` (same skip
rule).

## Result in one paragraph — CONFIRMED

All 42 packages parse completely: summary, LZO-compressed body (38 packages), name/import/export tables, depends map,
thumbnail table (4 packages), every package index resolved, every export payload in range. Across the install that is
77,358 names, 11,148 imports and 202,685 exports. The 42 files total 571,709,422 bytes on disk; their uncompressed
streams total 1,575,126,283 bytes (the 4 uncompressed packages count as their own file size). No cross-check finding
(warning or error) is raised for any shipped package. (T)

### Independent cross-check — CONFIRMED (2026-10-09)

A second, independently written parser (a throwaway Python script kept under the git-ignored `research/local/`, not
committed) re-derived everything above without using `asamu-ue3` code:

- its own summary / chunk-header / table parser (`struct.unpack`, layouts as documented on this page);
- the reference LZO1X decoder (`lzo1x_decompress_safe` from a locally installed `liblzo2`) instead of our `lzo.rs`;
- the stream's summary built by byte surgery on the original summary (drop the chunk table, zero `CompressionFlags`)
  instead of re-serialization;
- its own name display, object-path, class-name and class-package resolution.

Compared against `asamu-inspect` (`decompress`, `package`, `names`, `imports`, `exports`, `objects`, all `--json`) for
all 42 packages: every uncompressed stream is byte-identical (SHA-256 of the full stream, 1,575,126,283 bytes in
total), and every summary field, table extent, name (text and flags), import row and export row (paths, class,
super/archetype paths, flags, serial offset/size, net counts, GUID, package flags) agrees — **0 disagreements**. To
repeat it, write an independent parser from this page and compare it the same way; do not commit decompressed data.

## Summary (`FPackageFileSummary`) — CONFIRMED (T)

Little-endian. Fixed offsets are valid when FolderName is `"None"` (true for every package) and there is one
generation (true for every package); later fields shift with array lengths. On-disk summaries range from 129 bytes
(`GuidCache.upk`, `GlobalPersistentCookerData.upk`) to 9,005 bytes (`AG-StarHaven.asamu`, 84 chunk entries).

| Offset | Type | Field | Observed |
|---|---|---|---|
| 0x00 | u32 | Tag | `0x9E2A83C1` (bytes `C1 83 2A 9E`) everywhere |
| 0x04 | u16 | FileVersion | 868 everywhere |
| 0x06 | u16 | LicenseeVersion | 0 everywhere |
| 0x08 | i32 | TotalHeaderSize | end of the header in the *uncompressed* stream = offset of the first export payload |
| 0x0C | FString | FolderName | `"None"` everywhere (len 5 incl. NUL) |
| 0x15 | u32 | PackageFlags | see flag table below |
| 0x19 | i32, i32 | NameCount, NameOffset | |
| 0x21 | i32, i32 | ExportCount, ExportOffset | |
| 0x29 | i32, i32 | ImportCount, ImportOffset | |
| 0x31 | i32 | DependsOffset | |
| 0x35 | i32 ×3 | ImportExportGuidsOffset, ImportGuidsCount, ExportGuidsCount | counts 0 everywhere |
| 0x41 | i32 | ThumbnailTableOffset | non-zero in 4 packages |
| 0x45 | 16 B | Package GUID | 4 × u32 |
| 0x55 | TArray | Generations: `{i32 ExportCount, i32 NameCount, i32 NetObjectCount}` | 1 entry everywhere; its export/name counts equal the table counts |
| 0x65 | i32 | EngineVersion | 12097 everywhere |
| 0x69 | i32 | CookerVersion | 136 everywhere except `RefShaderCache-PC-D3D-SM3.upk` = 0 |
| 0x6D | u32 | CompressionFlags | 2 on 38 packages, 0 on 4 |
| 0x71 | TArray | CompressedChunks: `{i32 UncompressedOffset, UncompressedSize, CompressedOffset, CompressedSize}` | 0–84 entries |
| var | u32 | PackageSource | CRC-32 of the package's base file name — CONFIRMED for all 42; see [PackageSource](#packagesource--confirmed) |
| var | TArray\<FString\> | AdditionalPackagesToCook | `["freds_place"]` in AG-BeautifulCity, `["thecore"]` in AG-IceCave, empty elsewhere |
| var | TArray | TextureAllocations: `{i32 SizeX, SizeY, NumMips; u32 Format, TexCreateFlags; TArray<i32> ExportIndices}` | non-empty in 21 of 42 packages (T): `Startup.upk` 66, every map (1–36), `Startup_LOC_INT.upk` 10, `Engine.u` 10, `ASAMUFrontEndFlash.upk` 4, `UDKBase.u` 4, `UnrealEd.u` 3, `ASAMUFrontEndMap_LOC_INT.upk` 3, `GameFramework.u` 2, `UDKBase_LOC_INT.upk` 2 |

FString: `i32 len`; `len > 0` → `len` Latin-1 bytes including a trailing NUL; `len < 0` → `-len` UTF-16LE code units
including a trailing NUL; `len == 0` → empty. Every string in every shipped package satisfies this (the reader rejects a
missing terminator). CONFIRMED. All 77,358 name-table strings use the single-byte form and are pure ASCII: the UTF-16
form and bytes ≥ 0x80 never occur there, so decoding high bytes as Latin-1 (UE3 itself writes the platform ANSI code
page) is untested against real data — TENTATIVE.

All counts and offsets are non-negative in every package; the reader rejects negative values.

## Compression — CONFIRMED (T)

- `CompressionFlags = 2` is LZO1X: every block of all 38 compressed packages decompresses with an LZO1X decoder to
  exactly the size in its block table, and the rebuilt streams parse with zero findings. (Earlier status: TENTATIVE.)
  Two independent decoders agree: our `lzo.rs` and the reference `liblzo2` produce byte-identical streams for all 38
  (12,063 blocks, 555,598,975 compressed block bytes into 1,559,127,860 bytes; see "Independent cross-check").
  `CompressionFlags = 0` (no chunks) on `UTGameContent.u`, `ASAMUFrontEndFlash.upk`, `GuidCache.upk`,
  `GlobalPersistentCookerData.upk`. ZLIB (1) / LZX (4) never occur; the reader rejects them as unsupported.
- Each summary chunk entry points at a chunk header in the file:

  ```text
  i32 Tag (0x9E2A83C1) | i32 BlockSize | i32 CompressedSize | i32 UncompressedSize
  ceil(UncompressedSize / BlockSize) x { i32 CompressedSize, i32 UncompressedSize }
  compressed blocks, back to back
  ```

  - `BlockSize = 0x20000` (128 KiB) in every chunk; every block except the last of a chunk decompresses to exactly
    128 KiB.
  - Header `UncompressedSize` equals the summary entry's; header `CompressedSize` equals the sum of block compressed
    sizes; `16 + 8 × blocks + CompressedSize` equals the summary entry's `CompressedSize`.
  - Example `Core.u`: one chunk (uOff 129, uSize 309,301, cOff 145, cSize 90,240), header block size 131,072,
    3 blocks (compressed 27,559 + 44,772 + 17,869 = 90,200 bytes; uncompressed 131,072 + 131,072 + 47,157).
- Chunk table invariants (every compressed package):
  - The first chunk's `CompressedOffset` equals the end of the on-disk summary; chunks are contiguous in the file
    and the last one ends exactly at EOF.
  - The first chunk's `UncompressedOffset` equals `NameOffset`, and chunks are contiguous in uncompressed space.
  - `NameOffset` equals the on-disk summary size minus `16 × chunkCount`: the uncompressed stream's own summary is
    the same summary with an empty chunk table.
- Our rebuilt stream = the summary re-serialized with `CompressionFlags = 0` and an empty chunk table (exactly
  `chunk[0].UncompressedOffset` bytes in every package) followed by each chunk's output at its `UncompressedOffset`.
  The bytes of the original pre-compression summary are UNKNOWN (e.g. whether it stored `CompressionFlags = 2`);
  only its length is confirmed. Everything after it is decompressed data.
- Totals: 485 chunks. Largest stream: `RefShaderCache-PC-D3D-SM5.upk`, 2 chunks / 4,555 blocks, 596,946,352 bytes.

## Uncompressed stream layout — CONFIRMED (T)

In every package the header regions are adjacent, in this order:

```text
[summary][name table][import table][export table][depends map][thumbnail records][thumbnail table] = TotalHeaderSize
          ^NameOffset ^ImportOffset ^ExportOffset ^DependsOffset ^ImportExportGuidsOffset  ^ThumbnailTableOffset
```

- `ImportExportGuidsOffset` = end of the depends map. With both GUID counts 0 there are no GUID records, so it equals
  `TotalHeaderSize` unless a thumbnail table follows.
- Export payloads then tile `[TotalHeaderSize, end of stream)` exactly: sorted by `SerialOffset`, each starts where the
  previous ended, the first at `TotalHeaderSize`, the last at the end of the stream; no zero-size exports. All 42
  packages.

### Name table — CONFIRMED

`FString Name | u64 Flags`. The flags are the in-memory name-entry flags at save time: every one of the 77,358 entries
carries `0x0007001000000000`, and 51 also carry `0x0000100000000000`. See
[Object, export and name flags](#object-export-and-name-flags) — CONFIRMED (F).

### Import table — CONFIRMED

28 bytes: `FName ClassPackage | FName ClassName | i32 OuterIndex | FName ObjectName`.
Example: `Core.Class Engine.PlayerStart` (class package `Core`, class `Class`, outer = import `Engine`).

### Export table — CONFIRMED

`68 + 4 × n` bytes:
`i32 ClassIndex | i32 SuperIndex | i32 OuterIndex | FName ObjectName | i32 ArchetypeIndex | u64 ObjectFlags |
i32 SerialSize | i32 SerialOffset | u32 ExportFlags | TArray<i32> GenerationNetObjectCount (n entries) |
16 B PackageGuid | u32 PackageFlags`.
`SerialOffset` is always serialized (no "only when SerialSize > 0" rule). Evidence: the table extents computed with
this layout end exactly at `DependsOffset` in all 42 packages.

### Depends map — CONFIRMED

One `TArray<i32>` per export at `DependsOffset`; every array is empty in every package (cooked packages).

### Thumbnails — CONFIRMED

Present only in `ASAMUFrontEndFlash.upk`, `AG-BeautifulCity.asamu`, `ASAMUEntry.asamu`, `ASAMULegal.asamu`.
Table at `ThumbnailTableOffset`: `i32 count`, then `count × { FString ObjectClassName, FString ObjectPath, i32 FileOffset }`.
Each record at `FileOffset`: `i32 Width | i32 Height | TArray<u8> ImageData`. 21 records in total (13 in
`ASAMUFrontEndFlash.upk`, 4 in `AG-BeautifulCity.asamu`, 2 each in `ASAMUEntry.asamu` and `ASAMULegal.asamu`): every
non-empty record (13) starts with the PNG signature; the other 8 (all in `ASAMUFrontEndFlash.upk`, for
`PostProcessChain`, `SoundCue`, 5 `SwfMovie` and `UISoundTheme` objects) are empty: width = height = 0 and no image
bytes. (T) Records are contiguous from `ImportExportGuidsOffset` to `ThumbnailTableOffset`; the table ends at
`TotalHeaderSize`.

### References — CONFIRMED

- `FName` = `i32 name index + i32 number`; displayed as `Name` when number = 0, `Name_{number-1}` otherwise.
- Package index: 0 = null, `> 0` = export `i-1`, `< 0` = import `-i-1`. Every class/super/outer/archetype index of
  all 202,685 exports and every import outer index resolves; the deepest outer chain is 9 objects long (the reader's
  cycle guard is 256). (T)
- Object paths are printed with `.` between every outer level (UE3 may use `:` for some subobject paths; we do not).

## Package flags

Updated 2026-10-10 (flags-evidence pass). Earlier this table carried UE3 names with TENTATIVE confidence only; every
bit now also carries the behaviour of the original executable for it, where the executable tests or sets it.

**Method.** Three independent lines of evidence per bit:

1. *Correlation across the 42 packages* (observation counts CONFIRMED (T)/(F)).
2. *The original executable.* The unstripped Mach-O keeps its symbol names, so the loader, saver, net and download
   functions are named (`ULinkerLoad::SerializePackageFileSummary`, `ULinkerLoad::FinalizeCreation`,
   `ULinkerLoad::CreateImport`, `ULinkerLoad::Verify`, `UPackage::InitNetInfo`, `UObject::SetNetIndex`,
   `UObject::CreatePackage`, `UWorld::Serialize`, `ULevel::Serialize`, `UDownload::TrySkipFile`, ...). Field
   offsets read from them: the linker's copy of the summary starts at linker `+0x68`, so its `PackageFlags` is at
   linker `+0x74` and `PackageSource` at `+0xE4`; `UPackage::PackageFlags` is at package `+0x128`; the linker's
   `LinkerRoot` (the `UPackage`) at `+0x60`. A headless Ghidra pass (read-only project in `research/ghidra/`, a
   throwaway scan script, not committed) listed every `TEST`/`AND`/`OR` of an immediate against those fields in all
   89,081 functions; the hits in package/linker code were then read locally in decompiled form (never committed).
   Reproduce: decompile the functions named in the table below with `tools/ghidra-scripts/DecompileToLocal.java`
   into an ignored `research/` folder and look for the field offsets above.
3. *UE3 naming convention* (the `PKG_` names; no flag-name strings exist in the binary).

Confidence below: "meaning" is about what the bit does; when the meaning is CONFIRMED from code and data, the UE3
*name* is STRONG.

| Bit | UE3 name | Shipped summaries | Executable behaviour (functions) | Meaning |
|---|---|---|---|---|
| `0x00000001` | AllowDownload | 32 (all but `Core.u`, `Engine.u`, `GameFramework.u`, `IpDrv.u`, `OnlineSubsystemSteamworks.u`, `WinDrv.u`, `UTEditor.u`, `UnrealEd.u`, `UTGameContent.u`, `GuidCache.upk`) (F) | set on every newly created package (`UObject::CreatePackage`); cleared on the GUID-cache package (`UGuidCache::CreateInstance`) — and `GuidCache.upk` lacks it (F) | TENTATIVE (download permission not exercised by code we read) |
| `0x00000002` | ClientOptional | never | the download code lets a client skip a package file that has it (`UDownload::TrySkipFile`, `UChannelDownload::TrySkipFile`, `UDownload::ReceiveData` test the copy in the package-info record) | STRONG |
| `0x00000004` | ServerSideOnly | 20 (F) | objects whose outermost package has it never get a net index (`UObject::SetNetIndex`, also `SerializeNetIndex`, `StaticAllocateObject`); a forced-export package whose export entry has no net-object counts gets it at load (`UPackage::InitNetInfo`); the local shader-cache save sets it (`SaveLocalShaderCache`) | CONFIRMED |
| `0x00000008` | Cooked | 39 — all except the 3 `RefShaderCache-*` (T) | switches the loader's archive to cooked mode (`SerializePackageFileSummary`); gates many cooked-only paths (`CreateExport`, `Verify`, `VerifyImportInner`, `GetImportPathName`); cleared on load when the editor runs in script-compile mode | STRONG |
| `0x00000020` | SavedWithNewerVersion | never | set (with a one-time warning) when the package's engine version is newer than the running engine (`SerializePackageFileSummary`); a `SavedWithNewerVersion` map-check message string exists | STRONG |
| `0x00010000` | Compiling | never | import verification stops early for a package with it (`VerifyImportInner`) | TENTATIVE |
| `0x00020000` | ContainsMap | exactly the 12 `.asamu` (T) | set on the outermost package by `UWorld::Serialize` and `ULevel::Serialize` when saving a non-template world/level, and by `UWorld::CreateNew` | CONFIRMED |
| `0x00040000` | Trash | never | never copied from the file (`SerializePackageFileSummary` masks it out); set when the file path contains `__Trashcan` | STRONG |
| `0x00080000` | DisallowLazyLoading | 37 (F) | the loader turns lazy loading off for such packages, unless the editor loads a cooked one (`SerializePackageFileSummary`) | STRONG |
| `0x00100000` | PlayInEditor | never | tested by level streaming, dirty marking and selection code on the outermost package | TENTATIVE |
| `0x00200000` | ContainsScript | exactly the 12 `.u` (T), plus the `asamu`/`UTGame` package exports in `Startup.upk` | never marked dirty (`UPackage::SetDirtyFlag`, `UObject::MarkPackageDirty`); tested in `CreateExport` | STRONG |
| `0x00800000` | RequireImportsAlreadyLoaded | 35 (F) | the loader skips import verification when set (`ULinkerLoad::Verify`, `CreateImport`); the editor clears it on load | STRONG |
| `0x02000000` | StoreCompressed | exactly the 38 packages with CompressionFlags ≠ 0 (T) | the loader installs the compressed-chunk map on its file reader only when set (`SerializePackageFileSummary`) | CONFIRMED |
| `0x04000000` | StoreFullyCompressed | never | tested by the saver | TENTATIVE |
| `0x10000000` | ContainsFaceFXData | never stored | set in memory while reading the export table when an export's class is `FaceFXAsset` or `FaceFXAnimSet` (`ULinkerLoad::SerializeExportMap`; `Engine.u` holds the two default objects, so it gets the bit at load) | CONFIRMED |
| `0x20000000` | NoExportAllowed | 27 (F) | set on the package at load when `PackageSource` equals the checksum of its base file name; otherwise the engine records "user-created content loaded" (`ULinkerLoad::FinalizeCreation`). All 42 shipped packages match, so all of them get it at load whether or not it is stored | CONFIRMED |

The other UE3 names in `summary::package_flags` (`Unsecure` 0x10, `Need` 0x8000, `ContainsDebugInfo` 0x400000,
`SelfContainedLighting` 0x1000000, `ContainsInlinedShaders` 0x8000000, `StrippedSource` 0x40000000) are never stored
and were not seen in the code read: TENTATIVE.

Exactly nine bits occur in shipped summaries (`0x22AA000D` is their union) (F). Distinct values: maps `0x228A0009`;
LOC packages and `Startup.upk` `0x0288000D`; `.u` packages `0x22A80008` / `0x22A80009` / `0x22A8000C`, `UTGameContent.u`
`0x20280008` (uncompressed); `ASAMUFrontEndFlash.upk` `0x20080009`; `GuidCache.upk` `0x0000000C`;
`GlobalPersistentCookerData.upk` `0x0000000D`; `RefShaderCache-PC-D3D-SM3.upk` `0x02000005`; the other two shader
caches `0x22000005`.

### Correlations — CONFIRMED (F)

- **ServerSideOnly tracks net objects.** The 17 packages whose generation `NetObjectCount` is 0 (the 11 LOC packages,
  `Startup.upk`, `GuidCache.upk`, `GlobalPersistentCookerData.upk`, `IpDrv.u`, `OnlineSubsystemSteamworks.u`,
  `WinDrv.u`) all have the bit; the 22 packages with net objects do not; the 3 shader caches have the bit with
  `NetObjectCount` 1 (they are written by a different save path that sets it explicitly, and they are the only
  packages without `Cooked`). The same holds for the export-level copy (next section): forced package exports have
  the bit exactly when their net-object count array is empty (4 vs 707). And 8,365 of the 8,365 non-actor objects
  checked whose *effective* package (the forced package export they live in, else the file) has the bit store
  `NetIndex = -1` (local probe; the 4 apparent exceptions were component subobjects whose first `i32` is the
  template-owner field, not the net index).
- **Export-level `PackageFlags`** (the `u32` at the end of each export row): non-zero on exactly the 711 forced
  top-level `Package` exports and zero on every other export, including all 1,113 nested (group) `Package` exports
  and the one non-forced top-level `Package` (`Sound` in `ASAMUFrontEndFlash.upk`) (F). `UPackage::InitNetInfo` copies
  this value, with the GUID and net-object counts at export-entry offsets `+0x60`, `+0x50` and `+0x40`, into the
  `UPackage` created for the forced export. Values: `0x20000001` (609), `0x228A0009` (12, each map's own package
  inside itself), `0x00000001` (84), `0x20000005` (3), `0x20200000` (2: `asamu`, `UTGame`), `0x0288000D` (1:
  `Startup` inside `Startup.upk`).

### PackageSource — CONFIRMED

`PackageSource` (summary) equals, for all 42 packages, the engine's case-insensitive string CRC of the file name without
directory and extension (`AG-Workshop`, `Startup_LOC_INT`, `Core`, ...) (F): CRC-32 with polynomial `0x04C11DB7`, MSB
first, initial value `0xFFFFFFFF`, final complement (the CRC-32/BZIP2 parameters), over each upper-cased character
fed as a 16-bit code unit, low byte first. Source of the algorithm: `appStrCrcCaps` in the executable (read locally);
our implementation is `flags::package_source::package_source_crc`. The loader compares the two values in
`ULinkerLoad::FinalizeCreation` (table above). Renaming a package file therefore changes how the engine classifies it
(stock vs user-created content); our runtime has no such distinction.

## Object, export and name flags

Names are in `crates/asamu-ue3/src/flags.rs` (`object`, `export`, `name`). Same three lines of evidence as for package
flags; the export-table entry in memory is 0x68 bytes with `ObjectFlags` at `+0x18`, `ExportFlags` at `+0x3C`,
`GenerationNetObjectCount` at `+0x40`, `PackageGuid` at `+0x50` and `PackageFlags` at `+0x60` (read from
`UPackage::InitNetInfo`, `ULinkerLoad::CreateExport`, `ULinkerLoad::FinalizeCreation`).

### ObjectFlags (export table, 64-bit)

- **Loader keep-mask — CONFIRMED.** When `ULinkerLoad::CreateExport` creates an object it keeps only
  `ObjectFlags & 0x067F012500080700` from the stored value. Every one of the 202,685 shipped exports has flags inside
  that mask (F).
- **Load context — CONFIRMED.** `ULinker`'s constructor builds a context mask from three globals: `0x0004000000000000`
  when `GIsEditor` (unless cooking for one of a set of cook targets), `0x0001000000000000` when `GIsClient`, `0x0002000000000000`
  when `GIsServer`; `CreateExport` skips an export whose `ObjectFlags` do not intersect it. So the UE3 names
  `RF_LoadForEdit` / `RF_LoadForClient` / `RF_LoadForServer` describe exactly what the bits do. In the data
  `LoadForEdit` is on all 202,685 exports; 185,947 have all three, 8 have client + edit, and **16,730 have only
  `LoadForEdit`** (F): a standalone game (client and server) never creates them. They are editor helpers — sprite,
  arrow, light-radius/cone and sound-radius components, path rendering, `Brush`/`BrushComponent`/`Model`/`Polys` of
  builder brushes, `InterpCurveEdSetup`, all `ScriptText` buffers, 103 editor textures — and the cooked-away
  `Distribution*` objects (2,1xx float and 2,0xx vector distributions; particle modules use their baked lookup
  tables). `flags::object::loaded_in_game` encodes the rule for our importer/runtime.

| Bit | UE3 name | Exports | Evidence | Confidence |
|---|---|---:|---|---|
| `0x100` | Protected | 211 | only on property objects; set on exactly the member properties whose declaration carries the `protected` specifier (204 of 204 parsed declarations; 0 others — a local, count-only check of the shipped class sources) | STRONG |
| `0x200` | ClassDefaultObject | 2,521 | exactly the `Default__` objects (F); `UWorld`/`ULevel::Serialize` skip their package marking for it | STRONG |
| `0x400` | ArchetypeObject | 153 | prefab archetypes and their subobjects | TENTATIVE |
| `0x100000000` | Transactional | 102,095 | map actors/components, material expressions | TENTATIVE |
| `0x400000000` | Public | 82,447 | `VerifyImportInner` binds an import only to an export with this bit; all 10,259 imports that resolve to an export of another shipped package (same path and class) target one with it (F); `CreatePackage` allocates packages with it | STRONG |
| `0x10000000000` | PerObjectLocalized | 877 | all 852 `SoundNodeWave`s; 873 of the 877 have a class with `CLASS_Localized` | TENTATIVE |
| `0x1000000000000` | LoadForClient | 185,955 | context mask (above) | CONFIRMED meaning |
| `0x2000000000000` | LoadForServer | 185,947 | context mask (above) | CONFIRMED meaning |
| `0x4000000000000` | LoadForEdit | 202,685 | context mask (above) | CONFIRMED meaning |
| `0x8000000000000` | Standalone | 10,422 | assets: textures, classes, meshes, materials, sounds | TENTATIVE |
| `0x10000000000000` / `0x20000000000000` | NotForClient / NotForServer | 3,315 each, always together (F) | all 3,315 also lack both game load-context bits (F) (`TextBuffer`, `InterpCurveEdSetup`, `Brush`, `Model`, `Polys`, `MetaData`, editor textures) | TENTATIVE |
| `0x200000000000000` | HasStack | 30,284 | selects the state frame (OBJECT_FORMAT.md) | STRONG |
| `0x400000000000000` | Native | 1,652 | on `Class` exports exactly when `ClassFlags` has `CLASS_Native` (1,652 both, 869 neither) and on nothing else (F); `CreateExport` treats native classes specially | STRONG |

The other bits of the keep-mask (`0x80000` LocalizedResource, `0x2000000000` Obsolete, `0x40000000000000`
NotForEdit) never occur: TENTATIVE names.

### ExportFlags

| Bit | UE3 name | Evidence | Confidence |
|---|---|---|---|
| `0x1` | ForcedExport | the only non-zero value; on exactly the 62,169 exports that lie inside a forced top-level `Package` export (0 exceptions either way) (F). for a top-level export with the bit, `CreateExport` creates a top-level package of the export's name (instead of an object inside the file's package), initializes it from the export entry (`UPackage::InitNetInfo`: flags, GUID, net-object counts) and increments a global named `UObject::GForcedExportCount`; the counter is also incremented for every other forced export it creates | STRONG |
| `0x2` | ScriptPatcherExport | `FAsyncPackage::CreateExports` skips its precache step for such exports; never observed | TENTATIVE |
| `0x4` | MemberFieldPatchPending | `CreateExport` sets object flag `0x200000` on struct objects created from such exports; never observed | TENTATIVE |

### Name-table flags

- `0x0007001000000000` on all 77,358 entries (F): `UObject::SavePackage` writes `TagExp | LoadForClient | LoadForServer |
  LoadForEdit` into every name it saves and builds the name map from names with `TagExp` — CONFIRMED (code + data).
  The value has no load-time meaning for us.
- `0x0000100000000000` (UE3 `RF_Suppress`) on 51 entries: exactly the entries whose text is in the effective log
  `Suppress=` list of the shipped configuration (`Engine/Config/BaseEngine.ini` minus the game's `-Suppress=`
  removals in `ASAMU/Config/DefaultEngine.ini`); `DevOnline`, which the game un-suppresses, occurs twice without it
  (F). CONFIRMED correlation; the bit records log-category suppression in the cooking session.

## Package contents observations (metadata only)

- `Startup.upk`: 131 top-level exports = 130 `Package` + 1 `ObjectReferencer`. Two of the packages are the script
  packages `asamu` (lower-case in the name table) and `UTGame`: 4,371 and 17,371 exports lie under them, including
  172 and 411 `Class` exports. Both `Package` exports carry `PackageFlags = 0x20200000` (ContainsScript |
  NoExportAllowed), a non-zero package GUID, and `GenerationNetObjectCount = [4371]` / `[17371]` — equal to the number
  of exports under each. CONFIRMED (T). This resolves the "where is `ASAMU.u`" question at the structural level: its
  classes are cooked into `Startup.upk`.
- All exports of `Startup.upk` except `ObjectReferencer_0` (the only top-level non-`Package` export) have
  `ExportFlags = 0x1` (UE3 `EF_ForcedExport`; name STRONG, see
  [Object, export and name flags](#object-export-and-name-flags)).
- `Startup.upk` contains 583 `TextBuffer` exports, each named `ScriptText` and parented to a `Class` export, with 583
  distinct parents: exactly one per `Class` export, i.e. per class of `asamu` (172) and `UTGame` (411). CONFIRMED (T).
  The 12 script `.u` packages contain 1,938 more, all named `ScriptText` and parented to classes (e.g. 1,343 in
  `Engine.u`). (T for the counts.) In UE3 a class's `ScriptText` buffer holds its UnrealScript source. A yes/no
  presence check — does the payload contain `class <OuterName>`, compared case-insensitively (UnrealScript keywords and
  identifiers are case-insensitive) — succeeds for 577 of the 583 in `Startup.upk` (567 with a case-sensitive
  comparison), so these very likely hold class source text — STRONG (structure CONFIRMED; content interpretation from
  the check plus UE3 convention). The check prints nothing but counts and is not part of the committed tests.
  **Hygiene: never extract, quote or commit their contents** — they are original game data.
- Maps (`.asamu`) define no `Class` exports; every map export's class is an import (T). Each map has exactly one
  `World` (`TheWorld`) and one `PersistentLevel` (T). Top-level map exports are the `World`, `Package` exports for
  referenced content packages, `LightMapTexture2D`/`ShadowMapTexture2D`, `Model`, `Polys` and a few
  `MaterialInstanceConstant`.

## Per-package census — CONFIRMED

| File | Size (B) | PackageFlags | Compression | Chunks | Names | Imports | Exports | Uncompressed stream (B) |
|---|---:|---|---|---:|---:|---:|---:|---:|
| `ASAMUFrontEndFlash.upk` | 678,383 | `0x20080009` | none | 0 | 140 | 27 | 17 | 678,383 |
| `Core.u` | 90,385 | `0x22a80008` | lzo | 1 | 827 | 20 | 1,542 | 309,430 |
| `Engine.u` | 3,475,659 | `0x22a80008` | lzo | 10 | 20,153 | 182 | 33,443 | 11,380,536 |
| `GFxUI.u` | 55,777 | `0x22a80009` | lzo | 1 | 544 | 60 | 761 | 194,407 |
| `GFxUIEditor.u` | 3,913 | `0x22a80009` | lzo | 1 | 63 | 23 | 26 | 12,216 |
| `GameFramework.u` | 497,784 | `0x22a80008` | lzo | 3 | 3,116 | 680 | 3,811 | 2,375,498 |
| `GlobalPersistentCookerData.upk` | 14,877,903 | `0x0000000d` | none | 0 | 10 | 2 | 1 | 14,877,903 |
| `GuidCache.upk` | 17,656 | `0x0000000c` | none | 0 | 344 | 2 | 1 | 17,656 |
| `IpDrv.u` | 539,729 | `0x22a8000c` | lzo | 3 | 2,629 | 236 | 6,841 | 2,126,199 |
| `Maps/AG-BeautifulCity.asamu` | 43,487,547 | `0x228a0009` | lzo | 54 | 2,942 | 481 | 16,412 | 68,046,925 |
| `Maps/AG-BeautifulCity_LOC_INT.upk` | 3,322,943 | `0x0288000d` | lzo | 4 | 69 | 5 | 44 | 3,565,220 |
| `Maps/AG-Darkcave.asamu` | 32,817,746 | `0x228a0009` | lzo | 47 | 1,877 | 437 | 12,905 | 64,704,509 |
| `Maps/AG-Darkcave_LOC_INT.upk` | 3,000,029 | `0x0288000d` | lzo | 4 | 61 | 5 | 34 | 3,205,876 |
| `Maps/AG-Epilogue.asamu` | 8,529,612 | `0x228a0009` | lzo | 19 | 1,588 | 297 | 4,471 | 17,481,271 |
| `Maps/AG-Epilogue_LOC_INT.upk` | 717,425 | `0x0288000d` | lzo | 1 | 36 | 5 | 8 | 761,671 |
| `Maps/AG-IceCave.asamu` | 47,321,596 | `0x228a0009` | lzo | 47 | 1,602 | 384 | 19,037 | 92,218,741 |
| `Maps/AG-IceCave_LOC_INT.upk` | 2,099,314 | `0x0288000d` | lzo | 3 | 47 | 5 | 19 | 2,209,232 |
| `Maps/AG-ParadiseCave.asamu` | 48,736,462 | `0x228a0009` | lzo | 32 | 1,999 | 415 | 16,074 | 91,657,471 |
| `Maps/AG-ParadiseCave_LOC_INT.upk` | 1,743,218 | `0x0288000d` | lzo | 3 | 44 | 5 | 16 | 1,830,479 |
| `Maps/AG-StarHaven.asamu` | 95,068,464 | `0x228a0009` | lzo | 84 | 3,372 | 547 | 23,892 | 152,935,833 |
| `Maps/AG-StarHaven_LOC_INT.upk` | 4,129,354 | `0x0288000d` | lzo | 5 | 90 | 5 | 64 | 4,479,305 |
| `Maps/AG-Workshop.asamu` | 10,775,112 | `0x228a0009` | lzo | 22 | 1,918 | 356 | 5,762 | 21,219,061 |
| `Maps/AG-Workshop_LOC_INT.upk` | 992,606 | `0x0288000d` | lzo | 1 | 36 | 5 | 9 | 1,045,676 |
| `Maps/ASAMUEntry.asamu` | 9,512 | `0x228a0009` | lzo | 1 | 77 | 22 | 26 | 198,306 |
| `Maps/ASAMUFrontEndMap.asamu` | 9,017,496 | `0x228a0009` | lzo | 16 | 1,662 | 352 | 4,925 | 14,361,017 |
| `Maps/ASAMUFrontEndMap_LOC_INT.upk` | 645,824 | `0x0288000d` | lzo | 3 | 57 | 6 | 39 | 2,458,640 |
| `Maps/ASAMULegal.asamu` | 281,800 | `0x228a0009` | lzo | 3 | 158 | 33 | 36 | 4,898,204 |
| `Maps/Freds_place.asamu` | 824,867 | `0x228a0009` | lzo | 2 | 368 | 65 | 546 | 1,454,019 |
| `Maps/TheCore.asamu` | 13,389,529 | `0x228a0009` | lzo | 17 | 1,428 | 297 | 7,018 | 21,759,075 |
| `Maps/TheCore_LOC_INT.upk` | 1,296,434 | `0x0288000d` | lzo | 2 | 47 | 5 | 19 | 1,403,063 |
| `OnlineSubsystemSteamworks.u` | 93,987 | `0x22a8000c` | lzo | 1 | 824 | 98 | 1,308 | 356,416 |
| `RefShaderCache-PC-D3D-SM3.upk` | 31,424,181 | `0x02000005` | lzo | 2 | 1,600 | 2 | 1 | 130,517,915 |
| `RefShaderCache-PC-D3D-SM5.upk` | 111,239,152 | `0x22000005` | lzo | 2 | 1,714 | 2 | 1 | 596,946,352 |
| `RefShaderCache-PC-OpenGL.upk` | 23,977,166 | `0x22000005` | lzo | 2 | 855 | 2 | 1 | 133,360,199 |
| `Startup.upk` | 52,103,624 | `0x0288000d` | lzo | 70 | 17,658 | 3,998 | 37,183 | 95,091,980 |
| `Startup_LOC_INT.upk` | 2,968,840 | `0x0288000d` | lzo | 10 | 287 | 8 | 288 | 10,009,953 |
| `UDKBase.u` | 649,060 | `0x22a80009` | lzo | 4 | 3,103 | 772 | 2,881 | 2,418,164 |
| `UDKBase_LOC_INT.upk` | 239,777 | `0x0288000d` | lzo | 2 | 60 | 7 | 33 | 1,422,345 |
| `UTEditor.u` | 995 | `0x22a80008` | lzo | 1 | 20 | 9 | 6 | 2,355 |
| `UTGameContent.u` | 377,291 | `0x20280008` | none | 0 | 2,013 | 1,069 | 1,004 | 377,291 |
| `UnrealEd.u` | 181,227 | `0x22a80008` | lzo | 1 | 1,802 | 137 | 2,054 | 724,364 |
| `WinDrv.u` | 10,043 | `0x22a8000c` | lzo | 1 | 118 | 80 | 126 | 33,127 |

## Reader design notes (`crates/asamu-ue3`)

- `reader::Reader`: bounds-checked cursor; every array count is checked against the remaining bytes
  (`count × min_element_size ≤ remaining`) before allocating.
- Compressed blocks are additionally limited to `BlockSize ≤ 16 MiB` and a 256:1 expansion ratio; the rebuilt stream is
  capped at 2 GiB (configurable, `ReadOptions::max_stream_size`); a gap between the summary and the first chunk above
  4 KiB is rejected; compressed chunk ranges may not overlap each other or the summary (no output amplification by
  re-reading the same bytes).
- The same stream cap applies to uncompressed packages (the file is the stream), and `Package::open_with` refuses a file
  larger than the cap before reading it. `Summary::read_from_path` reads a growing prefix but never more than 64 MiB
  (`MAX_SUMMARY_PREFIX`). Explicit decompression thread counts are capped at 64, and thread-spawn failure is an error,
  not a panic. Chunk-header errors name the chunk and its file offset.
- `asamu-inspect decompress` never writes through an existing link: a symlink (even dangling) or non-regular file at
  `--out` is refused; without `--force` the file is created with `create_new`; with `--force` the data is written to a
  fresh temporary file and renamed over the target, so a hard link to an original package cannot be overwritten.
- Hard errors: malformed summary/chunks/tables, unresolvable name or package indices. Soft findings (`Package::issues`):
  out-of-range export payloads (error severity; the payload accessor then fails), generation/table count mismatch,
  overlapping or out-of-header table regions, payload tiling gaps/overlaps, unparsed GUID records.
- Tested with synthetic packages written byte by byte in the tests (uncompressed and LZO literal-run compressed
  variants, with and without a thumbnail table), plus truncation at every offset, byte flips at every offset, extreme
  `i32` values at every offset and random multi-byte mutations: no panics (`tests/robustness.rs`). Every truncation is
  detected (hard error or an error-level finding, never a clean parse). `tests/hostile.rs` adds targeted cases:
  negative values in every summary count/offset field and chunk entry, table offsets at or past the end of the stream,
  huge counts inside tables, out-of-range indices in every reference and FName slot, self-referential and mutual
  import/export outer cycles, the exact outer-depth boundary (256 ok, 257 rejected), hostile thumbnail tables,
  overlapping or out-of-order chunks, block-table inconsistencies, and the resource caps above.

## UNKNOWN / not yet done

- Export payload formats (UObject serialization, property tags, class/struct/function bodies, state frames on actors,
  component template prefixes). Next step for the importer.
- Layout of import/export GUID records (counts are 0 everywhere, never observed).
- `ObjectFlags` bits marked TENTATIVE above (`ArchetypeObject`, `Transactional`, `PerObjectLocalized`, `Standalone`,
  `NotForClient`/`NotForServer`): only correlations, no executable behaviour read for them. (`PackageSource`,
  name-table flags, `ExportFlags` and the load-context bits were resolved on 2026-10-10.)
- Exact on-disk bytes of the pre-compression summary (only its length is confirmed).
