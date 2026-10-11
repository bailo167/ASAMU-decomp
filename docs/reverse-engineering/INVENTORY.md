# Original installation inventory

Sources: two legitimately owned Steam installations of the same Steam build (1822049): the macOS
depot on the analysis Mac
(`~/Library/Application Support/Steam/steamapps/common/A Story About My Uncle`) and the Windows
depot on a Windows 11 machine (`<Steam>/steamapps/common/A Story About My Uncle`). Read-only:
nothing in an install is modified, and no original bytes are committed. Everything below is
metadata (paths, sizes, hashes, counts, magic numbers) produced by the repository's own tools.

The sections up to "Findings" describe the macOS depot; ["Windows depot"](#windows-depot-278361)
describes the Windows one and how the two differ, and
["Converting on Windows"](#converting-on-windows) records what `asamu-import` does with it.

## Method and reproduction

Tools (both written for this project, see their crate docs):

- `tools/asamu-locate` finds the install through Steam: `ASAMU_ORIGINAL_DIR` overrides discovery;
  otherwise `ASAMU_STEAM_ROOT` or the per-OS default Steam roots are searched, every library in
  `steamapps/libraryfolders.vdf` (modern `"path"`/`"apps"` format and the legacy numeric-key format)
  is checked for `appmanifest_278360.acf`, and the manifest's `installdir`, `buildid` and
  `InstalledDepots` are read with a small hostile-input KeyValues parser. Account fields such as
  `LastOwner` are never exposed.
- `tools/asamu-inventory` walks the install root (symlinks are recorded, not followed), streams
  every file through SHA-256 in 1 MiB blocks, sniffs the first 4 KiB for a type, applies the
  ordered category rules below, and cross-checks cooker TOC files (`*TOC*.txt`) against the files on
  disk. Output is deterministic: sorted by path, `/` separators, paths relative to the install root,
  no timestamps, no absolute paths.

```bash
cargo run -p asamu-locate                 # human summary (prints local absolute paths: do not commit)
cargo run -p asamu-locate -- --json
cargo run --release -p asamu-inventory -- \
  --out docs/reverse-engineering/data/inventory/mac-depot278362-build1822049.json \
  --summary docs/reverse-engineering/data/inventory/mac-depot278362-build1822049.summary.md
# On a Windows install the same command with the win-depot278361-build1822049 file names.
# Re-inventory the install and diff it against the committed JSON of its depot (skips without
# game data, or when the installed depot or build has no committed inventory):
cargo test -p asamu-inventory --test real_install
```

Committed sanitized data:

| File | Content |
|---|---|
| [`data/inventory/mac-depot278362-build1822049.json`](data/inventory/mac-depot278362-build1822049.json) | Per-file path, size, extension, SHA-256, sniffed type (+ UE3 version/licensee, short detail), category; per-category/extension/type totals; TOC cross-check reports. Schema `asamu-inventory/1`. |
| [`data/inventory/mac-depot278362-build1822049.summary.md`](data/inventory/mac-depot278362-build1822049.summary.md) | The generated Markdown summary (all tables below come from it). |
| [`data/inventory/win-depot278361-build1822049.json`](data/inventory/win-depot278361-build1822049.json) | The same for the Windows depot (1,699 files). |
| [`data/inventory/win-depot278361-build1822049.summary.md`](data/inventory/win-depot278361-build1822049.summary.md) | Its generated summary. |

### Type sniffing

Magic numbers checked (first match wins): UE3 package tag `C1 83 2A 9E` (then u16 file version,
u16 licensee; a zero version whose u32 is a power-of-two block size is reported as a UE3
compressed-chunk header instead), byte-swapped tag, `BMSG` (UE3 global shader cache), Mach-O
thin/fat (CPU types listed), `MZ`/PE, ELF, PNG, GIF, JPEG, `bplist00`, Bink `BIK`/`KB2`, SWF
`FWS`/`CWS`/`ZWS`, GFx `GFX`/`CFX`, `OggS`, RIFF/WAVE, FaceFX `FACE`, CHM `ITSF`, RTF `{\rtf`, BMP
(size or reserved-field check), ICO/CUR (directory sanity check), TGA (header heuristic, needs the
`.tga` extension or an exact uncompressed size), UTF-16LE/BE BOM, UTF-8 BOM, XML / XML plist,
ASCII / UTF-8 text heuristic, zlib header, else `unknown` with the first four bytes as `magic=`.

### Category rules (ordered, first match wins)

| # | Category | Rule |
|---|---|---|
| 1 | `executable` | sniffed Mach-O / fat Mach-O / PE / ELF, or extension `exe` `dll` `dylib` `so` `com` |
| 2 | `editor-resource` | a path component `EditorResources`, or `Engine/Extras/...` (DCC tool scripts) |
| 3 | `shader` | a path component `Shaders`; a name starting `GlobalShaderCache`, `RefShaderCache` or `LocalShaderCache`; sniffed `BMSG`; extension `usf` `ush` |
| 4 | `map` | extension `asamu` (ASAMU's `MapExt`), `umap`, `ut3`, `udk` |
| 5 | `texture-related` | extension `tfc` `dds`, or sniffed UE3 compressed-chunk header |
| 6 | `ue3-package` | sniffed UE3 package tag, or extension `u` `upk` (map `_LOC_INT` packages stay here) |
| 7 | `config` | extension `ini`, a path component `Config`, or a .NET application configuration file (`*.exe.config`, `*.dll.config`) |
| 8 | `localization` | a path component `Localization`, or a UE3 language extension (`int`, `deu`, `fra`, ...) |
| 9 | `audio` | sniffed Ogg / WAVE, or extension `ogg` `wav` `mp3` `xma` `fsb` `bnk` `flac` `opus` |
| 10 | `movie` | sniffed Bink / SWF / GFx, or extension `bik` `bk2` `usm` `mp4` `avi` `webm` `swf` `gfx` |
| 11 | `font` | extension `ttf` `otf` `ttc` `fon` |
| 12 | `mesh-animation` | extension `psk` `psa` `fbx` `ase` `obj` `fxa` `apx` `apb` |
| 13 | `metadata` | `Info.plist`, `PkgInfo`, `steam_appid.txt`, `build.properties` (the engine build stamp), cooker TOCs, `CookerSync*`, extension `plist` `strings` `nib` |
| 14 | `image` | sniffed raster image or image extension outside editor resources (splash screens) |
| 15 | `engine-resource` | `Engine/Stats/...` (FPS/memory chart HTML/CSS templates) |
| 16 | `documentation` | extension `txt` `html` `htm` `rtf` `chm` `pdf` `md` |
| 17 | `unknown` | everything else |

Neither depot has files in the audio, movie, font, mesh/animation, documentation or unknown
categories (CONFIRMED by the category totals of both inventories; the two rules marked for
`*.exe.config` and `build.properties` were added for the two loose text files of the Windows
depot's `Binaries` folder, which would otherwise be `unknown`). Documentation-like files that do ship
(`FaceFX.chm`, `EULA.rtf`, GPL/LGPL texts, `UDKOffline.html`) sit under `Engine/EditorResources`
and therefore count as `editor-resource` (rule 2 wins). Game audio, movies and meshes are
presumably inside the UE3 packages (TENTATIVE until the packages are parsed).

## Steam metadata — CONFIRMED

| Field | Value | Source |
|---|---|---|
| App ID | 278360 | `steamapps/appmanifest_278360.acf` |
| Install dir | `A Story About My Uncle` | appmanifest `installdir` |
| Depot | 278362 (manifest 7137994883443283717) | appmanifest `InstalledDepots` |
| Build ID | 1822049 | appmanifest `buildid` |
| Size on disk | 1,246,349,269 bytes | appmanifest `SizeOnDisk`; equals the sum of all inventoried file sizes exactly |
| Files | 1,636 | `find -type f`; re-confirmed by `asamu-inventory` (no symlinks, no hidden files) |
| StateFlags | 4 (fully installed) | appmanifest `StateFlags` |

`asamu-locate` on the analysis machine: layout `mac-app`, found via the Steam root's own library,
build 1822049, depot 278362 (unit test `real_install_if_present` asserts these when the game is present).

### Independent verification (CONFIRMED)

A second, adversarial pass checked the committed data against the install with tools other than
`asamu-inventory`:

- Two fresh `asamu-inventory --out ... --summary ...` runs produced byte-identical JSON and
  summary files, both identical to the committed ones (`cmp`). The JSON is 498,234 bytes and
  contains no absolute paths, no user name and no symlink entries.
- `find <root> -type f` counts 1,636 files totalling 1,246,349,269 bytes (`stat -f %z`); `find -type l`
  finds no symlinks. The per-category, per-extension and per-type totals in the JSON re-sum
  exactly from its per-file rows.
- 21 entries (14 random, the 3 largest, 2 executables, 1 empty file, `PCTOC.txt`) re-hashed with
  `shasum -a 256` and sized with `stat`: 21/21 match.
- The TOC findings below (CookedPC 47/48 size matches, the 11 CRLF→LF `Engine/Config` files,
  56 missing `Binaries` entries / 80,415,329 bytes, all CRC and uncompressed-size columns `0`,
  147 shader `.bin` files starting `01 00 00 00` with median entropy 7.959 bits/byte) were
  recomputed with an independent Python parser of `PCTOC.txt` and agree.

## Layout — CONFIRMED

```
A Story About My Uncle/
└── A Story About My Uncle.app/Contents/
    ├── Info.plist             CFBundleIdentifier com.coffeestainstudios.astoryaboutmyuncle, version 1.1, LSMinimumSystemVersion 10.6, CFBundleExecutable ASAMU
    ├── PkgInfo
    ├── MacOS/
    │   ├── ASAMU              main executable (thin x86_64 Mach-O, MH_EXECUTE, 67,378,284 bytes)
    │   ├── libSDL2-2.0.0.dylib
    │   ├── libsteam_api.dylib
    │   └── openal.dylib
    └── Resources/
        ├── steam_appid.txt
        ├── English.lproj/     InfoPlist.strings (UTF-16BE), MainMenu.nib (binary plist)
        ├── ASAMU/
        │   ├── Config/        Default*.ini (11), Mac/*.ini (4)
        │   ├── CookedMac/     12 .u, 9 .upk, 3 .tfc, 3 GlobalShaderCache .bin
        │   │   └── Maps/      12 .asamu + 9 *_LOC_INT.upk
        │   ├── Localization/  14 language folders: BRA CZE DEU ESN FIN FRA HUN INT ITA NLD POL POR SLO TUR
        │   ├── Splash/        Mac/ and PC/ (Splash.bmp, EdSplash.bmp each)
        │   ├── Build/         CookerSync_Game.xml
        │   └── PCTOC.txt, PCTOC_<LANG>.txt (13 languages)
        └── Engine/
            ├── Config/        Base*.ini, ConsoleVariables.ini, Linux/ Mac/ Mobile/ PCServer/
            ├── Localization/  DEU ESN FRA INT ITA POL
            ├── Shaders/Binaries/  147 .bin
            ├── EditorResources/   FaceFX/ WPF/ wxRC/ wxRes/ UDKOffline.html
            ├── Extras/3dsMaxScripts/PivotPainter.ms
            ├── Splash/Mac/, Stats/
```

Change from the session-1 notes: `ASAMU/Localization` holds **14** language folders (INT included),
not 15.

## Summary tables (from `asamu-inventory`)

### Per category

| Category | Files | Bytes |
|---|---:|---:|
| `texture-related` | 3 | 568,543,124 |
| `map` | 12 | 310,259,743 |
| `shader` | 153 | 172,765,731 |
| `ue3-package` | 27 | 94,809,180 |
| `executable` | 4 | 69,298,296 |
| `editor-resource` | 1080 | 18,373,230 |
| `localization` | 276 | 6,445,145 |
| `image` | 6 | 4,392,336 |
| `metadata` | 20 | 1,243,119 |
| `config` | 46 | 207,863 |
| `engine-resource` | 9 | 11,502 |

### Per extension

| Extension | Files | Bytes |
|---|---:|---:|
| `tfc` | 3 | 568,543,124 |
| `asamu` | 12 | 310,259,743 |
| `upk` | 18 | 255,473,829 |
| `(none)` | 2 | 67,378,293 |
| `bmp` | 612 | 15,600,906 |
| `bin` | 150 | 6,125,232 |
| `u` | 12 | 5,975,850 |
| `cdf` | 1 | 2,617,658 |
| `dylib` | 3 | 1,920,012 |
| `chm` | 1 | 1,752,020 |
| `txt` | 20 | 1,275,075 |
| `fra` | 28 | 898,862 |
| `deu` | 27 | 897,818 |
| `pol` | 27 | 894,556 |
| `ita` | 28 | 893,924 |
| `xaml` | 36 | 893,472 |
| `esn` | 28 | 891,328 |
| `int` | 29 | 855,997 |
| `png` | 214 | 842,063 |
| `ico` | 135 | 455,386 |
| `xrc` | 12 | 312,742 |
| `ini` | 46 | 207,863 |
| `tga` | 62 | 145,064 |
| `por` | 14 | 144,740 |
| `nld` | 14 | 143,000 |
| `bra` | 14 | 141,498 |
| `tur` | 14 | 139,022 |
| `hun` | 14 | 138,548 |
| `fin` | 13 | 138,384 |
| `cze` | 13 | 135,556 |
| `slo` | 13 | 131,912 |
| `ms` | 1 | 64,575 |
| `rtf` | 1 | 29,811 |
| `nib` | 1 | 10,545 |
| `html` | 7 | 10,302 |
| `fxg` | 5 | 7,708 |
| `css` | 3 | 1,736 |
| `plist` | 1 | 963 |
| `strings` | 1 | 96 |
| `xml` | 1 | 56 |

### Per detected type

| Type | Files | Bytes |
|---|---:|---:|
| `ue3-package` | 42 | 571,709,422 |
| `ue3-compressed-chunks` | 3 | 568,543,124 |
| `mach-o` | 1 | 67,378,284 |
| `bmp` | 612 | 15,600,906 |
| `utf16le-text` | 258 | 6,916,808 |
| `ue3-global-shader-cache` | 3 | 4,986,377 |
| `facefx` | 6 | 2,625,366 |
| `mach-o-fat` | 3 | 1,920,012 |
| `chm` | 1 | 1,752,020 |
| `ascii-text` | 93 | 1,611,617 |
| `unknown` | 147 | 1,138,855 |
| `png` | 214 | 842,063 |
| `ico` | 135 | 455,386 |
| `utf8-text` | 30 | 369,752 |
| `xml` | 13 | 312,798 |
| `tga` | 62 | 145,064 |
| `rtf` | 1 | 29,811 |
| `binary-plist` | 1 | 10,545 |
| `xml-plist` | 1 | 963 |
| `utf16be-text` | 1 | 96 |
| `empty` | 9 | 0 |

## UE3 packages

Script packages present: `Core.u`, `Engine.u`, `GameFramework.u`, `GFxUI.u`, `GFxUIEditor.u`, `IpDrv.u`,
`OnlineSubsystemSteamworks.u`, `UDKBase.u`, `UTEditor.u`, `UTGameContent.u`, `UnrealEd.u`, `WinDrv.u`.

**Absent:** `ASAMU.u`, `UTGame.u` (both named in config; see SCRIPT_ANALYSIS.md).

Other packages: `Startup.upk` (52,103,624 B), `Startup_LOC_INT.upk`, `UDKBase_LOC_INT.upk`,
`ASAMUFrontEndFlash.upk`, `GlobalPersistentCookerData.upk`, `GuidCache.upk`, `RefShaderCache-PC-{D3D-SM3,D3D-SM5,OpenGL}.upk`.
Texture caches: `Textures.tfc` (444,637,918 B), `Lighting.tfc`, `CharTextures.tfc`.

Maps (`CookedMac/Maps`, extension from `MapExt=asamu` in `ASAMU/Config/DefaultEngine.ini`): `ASAMUEntry`,
`ASAMULegal`, `ASAMUFrontEndMap`, `AG-Workshop`, `AG-BeautifulCity`, `AG-Darkcave`, `AG-IceCave`,
`AG-ParadiseCave`, `AG-StarHaven`, `AG-Epilogue`, `TheCore`, `Freds_place` (all `.asamu`); nine have a
`<map>_LOC_INT.upk` companion (not `ASAMUEntry`, `ASAMULegal`, `Freds_place`).

### Package table (42)

Paths relative to `A Story About My Uncle.app/Contents/Resources/ASAMU/CookedMac/`.

| Package | Category | Bytes | Version | Licensee | SHA-256 |
|---|---|---:|---:|---:|---|
| `ASAMUFrontEndFlash.upk` | ue3-package | 678,383 | 868 | 0 | `046853cffa6f7aeee3bf58a088110c129b17fd2684f090537786dca5742e8883` |
| `Core.u` | ue3-package | 90,385 | 868 | 0 | `0c52ad2b6f7143623d6266c08fbe48e056163b6d5fd81b090e2d0e7002d436d6` |
| `Engine.u` | ue3-package | 3,475,659 | 868 | 0 | `e7186ee1b9c594e0316fda49b9915d1d26cf025508a0f860d83beb1e09d3a639` |
| `GFxUI.u` | ue3-package | 55,777 | 868 | 0 | `2f2e43827de3abe33bb165cbde85696b93d2913cbd15dc2271a92adea7613cfc` |
| `GFxUIEditor.u` | ue3-package | 3,913 | 868 | 0 | `e4a13e4fac57014be06faf5f01a7c700fce73eee7e4477e0bbf51bfc9028662c` |
| `GameFramework.u` | ue3-package | 497,784 | 868 | 0 | `047d6eb4cccfc684289b360f9f89d53a74e2afca4faf68ffcc732c35db164279` |
| `GlobalPersistentCookerData.upk` | ue3-package | 14,877,903 | 868 | 0 | `c349c7a937d11e590873f691f0a372df8678e3c279d10d6c68683544433df38b` |
| `GuidCache.upk` | ue3-package | 17,656 | 868 | 0 | `2494b4eeb5950250778f8d0b2160c87c62fd409a62fef9921d03eb2418c28f64` |
| `IpDrv.u` | ue3-package | 539,729 | 868 | 0 | `c20c57702f2f41c47e248126c7b415255068fdd3f8a0ad0207d28671ec51ae33` |
| `Maps/AG-BeautifulCity.asamu` | map | 43,487,547 | 868 | 0 | `183353f2e67163bed15a51a09daddce54e53ed2a7a9c5097f824aa9ba0e146cf` |
| `Maps/AG-BeautifulCity_LOC_INT.upk` | ue3-package | 3,322,943 | 868 | 0 | `6970451bdb4e9e2e765ce1484739f90e55c3ea656d709ac96e53c99ebda86e50` |
| `Maps/AG-Darkcave.asamu` | map | 32,817,746 | 868 | 0 | `6879f4020fb6bdb4d912b4fa994f8b7c8d5771d79d7c5bbfabae26cd4109eac4` |
| `Maps/AG-Darkcave_LOC_INT.upk` | ue3-package | 3,000,029 | 868 | 0 | `de87016746e870cb1fdbfd6e0a1c3ea98deea98b0fdc7e841cdbcf5b1e2bdd68` |
| `Maps/AG-Epilogue.asamu` | map | 8,529,612 | 868 | 0 | `97db71ba3032ecee1c6cdf0a3d4b7c5bdf64ce3c1e547b0d314e99714c9cc7c4` |
| `Maps/AG-Epilogue_LOC_INT.upk` | ue3-package | 717,425 | 868 | 0 | `c5d606b215970a0de3bd3468ef91a42449b450fb5d4482162672655b585ea894` |
| `Maps/AG-IceCave.asamu` | map | 47,321,596 | 868 | 0 | `00475c815761f0b0ad403d72f0cbc39abb32acd8d008095dc38d5e48e72c7b47` |
| `Maps/AG-IceCave_LOC_INT.upk` | ue3-package | 2,099,314 | 868 | 0 | `ac57d3bd58bc6f214d6c2009891c1bb700bfca21034ae17c07c870e24a07a7b9` |
| `Maps/AG-ParadiseCave.asamu` | map | 48,736,462 | 868 | 0 | `2a298b0a12faa69ccc3816133a15d455e1141c4399ff203677a00cdbe5b97f75` |
| `Maps/AG-ParadiseCave_LOC_INT.upk` | ue3-package | 1,743,218 | 868 | 0 | `eeed9e5b246764b3a1ac1865ff2402d66fa180fcee304df25494442993539d35` |
| `Maps/AG-StarHaven.asamu` | map | 95,068,464 | 868 | 0 | `5d82a7c351141bf8953e3816e46caf01fa2b5058043be8a8a4ef108a0e8634a8` |
| `Maps/AG-StarHaven_LOC_INT.upk` | ue3-package | 4,129,354 | 868 | 0 | `4008ea8a0e390d198f7f4a307e6bce46176c7f31cc7e61e34e86e4b65d6b565b` |
| `Maps/AG-Workshop.asamu` | map | 10,775,112 | 868 | 0 | `9ef2a3122d90367485e50a8b6862b261a31c6aa2058c8afce05614b8c578fa8e` |
| `Maps/AG-Workshop_LOC_INT.upk` | ue3-package | 992,606 | 868 | 0 | `1a174dc1f054a8b512a170c3e13c7aca25224ab4ae79de669dbe9aff44db6f94` |
| `Maps/ASAMUEntry.asamu` | map | 9,512 | 868 | 0 | `bf434eb89d020444b0b39bc96b37f5186303b7a5cbdbe4e95994c84023b58a62` |
| `Maps/ASAMUFrontEndMap.asamu` | map | 9,017,496 | 868 | 0 | `64c3cfb9d881a2ffabe57359104050d6110b1d542596b14b332c01cb3e42a59f` |
| `Maps/ASAMUFrontEndMap_LOC_INT.upk` | ue3-package | 645,824 | 868 | 0 | `28fda5d3e891247794176a4a345a5168db19ec6283c311c01755e6278931b423` |
| `Maps/ASAMULegal.asamu` | map | 281,800 | 868 | 0 | `81c75e6b88ca680d793b04cca26dad0e3cbe6dbbbed07fdcf3dd1e2300131c1a` |
| `Maps/Freds_place.asamu` | map | 824,867 | 868 | 0 | `4ab81280d8c92b937f410d8bd83943dc4ebe447825ce9364bc71b1a6e286e23a` |
| `Maps/TheCore.asamu` | map | 13,389,529 | 868 | 0 | `d8dd1c5f720741a5178dc4ccdfb3c616c0102a89edb8af7c98355a1a400ba436` |
| `Maps/TheCore_LOC_INT.upk` | ue3-package | 1,296,434 | 868 | 0 | `62917ceaf564c1f43c341b1f3243c3f9e95db02ffd2a7cd5b4a1134b234977b1` |
| `OnlineSubsystemSteamworks.u` | ue3-package | 93,987 | 868 | 0 | `56702cdc32772b0e5c5945906f0425367b65039d62e912974e2af6e6c3472e57` |
| `RefShaderCache-PC-D3D-SM3.upk` | shader | 31,424,181 | 868 | 0 | `51cae58efa11c5a0543d8229706ddc3e6185c84297f424c1ea36b5f71dd93b5f` |
| `RefShaderCache-PC-D3D-SM5.upk` | shader | 111,239,152 | 868 | 0 | `897010e04897f8ac02bf8974921f0de0bbb3db54ce3ab65cecff222893bc2e81` |
| `RefShaderCache-PC-OpenGL.upk` | shader | 23,977,166 | 868 | 0 | `c5b45a42b42515b422850c389f7168399d6d292af893f97f7d11d8d3bc4de8c5` |
| `Startup.upk` | ue3-package | 52,103,624 | 868 | 0 | `1b53315f25b4522391b43a156577561c323ec05d9cc1c9356ec52a5d8451798d` |
| `Startup_LOC_INT.upk` | ue3-package | 2,968,840 | 868 | 0 | `b5db8b4222aad0150c52a4afd598aeafaa30083919c28e5be61b80f86b468c87` |
| `UDKBase.u` | ue3-package | 649,060 | 868 | 0 | `11fa79501de302f9462ce31cbd340252a11c139ba17b2d50bec8750146e91f9d` |
| `UDKBase_LOC_INT.upk` | ue3-package | 239,777 | 868 | 0 | `99b31f9c424c4bbb76ea4bbf1357d72d76c8003541bd04f3ef657f9da206f92d` |
| `UTEditor.u` | ue3-package | 995 | 868 | 0 | `e4f1ba92a54bc98bea9e30cabf77b01007cd21cca91a4dfc2bc24938f0cab095` |
| `UTGameContent.u` | ue3-package | 377,291 | 868 | 0 | `3a382d1aa166d600fdf173e86700420270487b6871d2c9895c86d4f9cb860d67` |
| `UnrealEd.u` | ue3-package | 181,227 | 868 | 0 | `63a43642d6ee37fb9873fe008f7ab49944012adc0b04eb8e4f92bbe89bc1dba7` |
| `WinDrv.u` | ue3-package | 10,043 | 868 | 0 | `a99b2894e5f909c8d94e409a10a7251752bf01fcd8c9557cca2f607b5914978c` |

## Executables, libraries, texture caches and global shader caches

Executable SHA-256 (unchanged from session 1): `b611c4a0a64d220f3f2b8bdbd6287700327976bd2f196fca328a4b1b2d13d004`.

| Path | Bytes | Type | Detail | SHA-256 |
|---|---:|---|---|---|
| `A Story About My Uncle.app/Contents/MacOS/ASAMU` | 67,378,284 | `mach-o` | x86_64 execute | `b611c4a0a64d220f3f2b8bdbd6287700327976bd2f196fca328a4b1b2d13d004` |
| `A Story About My Uncle.app/Contents/MacOS/libSDL2-2.0.0.dylib` | 1,171,576 | `mach-o-fat` | fat x86_64 | `32ecfb8b056a2c1d29609b21f912288162ce185894f1a5334b40d5e8b46533ff` |
| `A Story About My Uncle.app/Contents/MacOS/libsteam_api.dylib` | 119,020 | `mach-o-fat` | fat i386+x86_64 | `c2fde13c9e751dfcd9fdcb1521176f5bc9d4811240037be143374487f2c3e72c` |
| `A Story About My Uncle.app/Contents/MacOS/openal.dylib` | 629,416 | `mach-o-fat` | fat i386+x86_64 | `1f763c4dc842a2ec7489e7b0ac805d64dcda6785b3fdcebf9b497cdb628d8693` |
| `A Story About My Uncle.app/Contents/Resources/ASAMU/CookedMac/CharTextures.tfc` | 54,859,175 | `ue3-compressed-chunks` | chunk block_size=131072 first_chunk=8042->8192 | `66c179f2b73f60d5d5a6f149ef1096cb06f0368fd3510b6629b73f18f36802e5` |
| `A Story About My Uncle.app/Contents/Resources/ASAMU/CookedMac/GlobalShaderCache-PC-D3D-SM3.bin` | 927,313 | `ue3-global-shader-cache` | u32@4=868 | `f08c0c58035e8f27d064b0e32f637870487b09dacdd3b03b4edebfa6cf5a51bd` |
| `A Story About My Uncle.app/Contents/Resources/ASAMU/CookedMac/GlobalShaderCache-PC-D3D-SM5.bin` | 2,135,757 | `ue3-global-shader-cache` | u32@4=868 | `fc6d5ef9ba61a19cfa9f1f863c955c6e4c9695b67089c46926423e3e4f0cafc1` |
| `A Story About My Uncle.app/Contents/Resources/ASAMU/CookedMac/GlobalShaderCache-PC-OpenGL.bin` | 1,923,307 | `ue3-global-shader-cache` | u32@4=868 | `628d6e61aaf668056ba12240347bfb17c358c7c392fc42a13fd09147a8096ca2` |
| `A Story About My Uncle.app/Contents/Resources/ASAMU/CookedMac/Lighting.tfc` | 69,046,031 | `ue3-compressed-chunks` | chunk block_size=131072 first_chunk=4715->8192 | `84135e30b01a2d71b780e6ee0bdcece58a988a5fb8234ac6657baf762f1373d1` |
| `A Story About My Uncle.app/Contents/Resources/ASAMU/CookedMac/Textures.tfc` | 444,637,918 | `ue3-compressed-chunks` | chunk block_size=131072 first_chunk=7002->8192 | `ef3b1dcf06773cfdae680943f248d29f62bbb9c0bd240ebb0a6697b2129c5865` |

## Cooker TOC cross-check

`ASAMU/PCTOC.txt` and 13 `PCTOC_<LANG>.txt` files ship in the Mac bundle. Line format (CONFIRMED by
parsing all 14 files, 0 malformed lines): `<size> <uncompressed size> <path> <crc>`, Windows-style
paths relative to the engine `Binaries` folder (`..\ASAMU\...`). Every entry has uncompressed size 0
and CRC `0`, so only sizes can be compared.

TOC paths (`..\X`) are resolved from `<TOC folder>/../Binaries`; `remapped` means found only after mapping `CookedPC`→`Cooked*` and `PC`→platform folders. CRC columns are counted, not checked.

| TOC | Entries | Found exact | Found remapped | Size match | Size differ | Missing | Unlisted on disk | Non-zero CRC | Malformed |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| `PCTOC.txt` | 1397 | 1291 | 50 | 1327 | 14 | 56 | 289 | 0 | 0 |
| `PCTOC_BRA.txt` | 1371 | 1276 | 39 | 1303 | 12 | 56 | 315 | 0 | 0 |
| `PCTOC_CZE.txt` | 1384 | 1275 | 39 | 1302 | 12 | 70 | 316 | 0 | 0 |
| `PCTOC_DEU.txt` | 1384 | 1289 | 39 | 1316 | 12 | 56 | 302 | 0 | 0 |
| `PCTOC_ESN.txt` | 1385 | 1290 | 39 | 1317 | 12 | 56 | 301 | 0 | 0 |
| `PCTOC_FIN.txt` | 1370 | 1275 | 39 | 1302 | 12 | 56 | 316 | 0 | 0 |
| `PCTOC_FRA.txt` | 1385 | 1290 | 39 | 1317 | 12 | 56 | 301 | 0 | 0 |
| `PCTOC_HUN.txt` | 1385 | 1276 | 39 | 1303 | 12 | 70 | 315 | 0 | 0 |
| `PCTOC_ITA.txt` | 1385 | 1290 | 39 | 1317 | 12 | 56 | 301 | 0 | 0 |
| `PCTOC_NLD.txt` | 1371 | 1276 | 39 | 1303 | 12 | 56 | 315 | 0 | 0 |
| `PCTOC_POL.txt` | 1384 | 1289 | 39 | 1316 | 12 | 56 | 302 | 0 | 0 |
| `PCTOC_POR.txt` | 1371 | 1276 | 39 | 1303 | 12 | 56 | 315 | 0 | 0 |
| `PCTOC_SLO.txt` | 1384 | 1275 | 39 | 1302 | 12 | 70 | 316 | 0 | 0 |
| `PCTOC_TUR.txt` | 1371 | 1276 | 39 | 1303 | 12 | 56 | 315 | 0 | 0 |

### `A Story About My Uncle.app/Contents/Resources/ASAMU/PCTOC.txt`

Base folder: `A Story About My Uncle.app/Contents/Resources`.

Listed in the TOC but missing on disk:

| Folder | Files | TOC bytes |
|---|---:|---:|
| `Binaries` | 1 | 87 |
| `Binaries/Win32` | 54 | 79,562,250 |
| `Binaries/Win32/UserCode` | 1 | 852,992 |

Size differs from the TOC:

| Path (relative to base) | TOC bytes | Disk bytes | Difference |
|---|---:|---:|---:|
| `ASAMU/CookedMac/RefShaderCache-PC-D3D-SM3.upk` | 31,379,963 | 31,424,181 | +44218 |
| `ASAMU/Localization/INT/ASAMU.int` | 31,308 | 31,296 | -12 |
| `ASAMU/PCTOC.txt` | 88,813 | 88,877 | +64 |
| `Engine/Config/BaseEditor.ini` | 48,638 | 48,115 | -523 |
| `Engine/Config/BaseEditorKeyBindings.ini` | 5,268 | 5,217 | -51 |
| `Engine/Config/BaseEditorUserSettings.ini` | 11,085 | 10,718 | -367 |
| `Engine/Config/BaseEngine.ini` | 35,239 | 34,125 | -1114 |
| `Engine/Config/BaseGame.ini` | 3,948 | 3,778 | -170 |
| `Engine/Config/BaseGameStats.ini` | 425 | 416 | -9 |
| `Engine/Config/BaseInput.ini` | 26,891 | 26,498 | -393 |
| `Engine/Config/BaseLightmass.ini` | 9,192 | 8,941 | -251 |
| `Engine/Config/BaseSystemSettings.ini` | 30,799 | 30,065 | -734 |
| `Engine/Config/BaseUI.ini` | 1,812 | 1,778 | -34 |
| `Engine/Config/ConsoleVariables.ini` | 886 | 866 | -20 |

On disk under the base folder but not listed in the TOC:

| Folder | Files | Disk bytes |
|---|---:|---:|
| `.` | 1 | 7 |
| `ASAMU` | 13 | 1,142,566 |
| `ASAMU/Config/Mac` | 4 | 243 |
| `ASAMU/Localization/BRA` | 14 | 141,498 |
| `ASAMU/Localization/CZE` | 13 | 135,556 |
| `ASAMU/Localization/DEU` | 13 | 145,568 |
| `ASAMU/Localization/ESN` | 14 | 138,956 |
| `ASAMU/Localization/FIN` | 13 | 138,384 |
| `ASAMU/Localization/FRA` | 14 | 147,084 |
| `ASAMU/Localization/HUN` | 14 | 138,548 |
| `ASAMU/Localization/ITA` | 14 | 141,166 |
| `ASAMU/Localization/NLD` | 14 | 143,000 |
| `ASAMU/Localization/POL` | 13 | 144,354 |
| `ASAMU/Localization/POR` | 14 | 144,740 |
| `ASAMU/Localization/SLO` | 13 | 131,912 |
| `ASAMU/Localization/TUR` | 14 | 139,022 |
| `ASAMU/Splash/Mac` | 2 | 1,464,112 |
| `Engine/Config/Linux` | 5 | 528 |
| `Engine/Config/Mac` | 5 | 544 |
| `Engine/Config/Mobile` | 5 | 1,556 |
| `Engine/Config/PCServer` | 5 | 666 |
| `Engine/Localization/DEU` | 14 | 752,250 |
| `Engine/Localization/ESN` | 14 | 752,372 |
| `Engine/Localization/FRA` | 14 | 751,778 |
| `Engine/Localization/ITA` | 14 | 752,758 |
| `Engine/Localization/POL` | 14 | 750,202 |
| `English.lproj` | 2 | 10,641 |

### Language TOCs: differences from the primary TOC

- `PCTOC_CZE.txt` vs `PCTOC.txt`: also missing `Engine/Localization/CZE` (14 files, 750,892 bytes)
- `PCTOC_HUN.txt` vs `PCTOC.txt`: also missing `Engine/Localization/HUN` (14 files, 770,712 bytes)
- `PCTOC_SLO.txt` vs `PCTOC.txt`: also missing `Engine/Localization/SLO` (14 files, 751,800 bytes)

## Findings

### CONFIRMED

- 1,636 files, 1,246,349,269 bytes; the byte total equals the appmanifest `SizeOnDisk`.
- All 42 UE3 packages (12 `.u`, 18 `.upk`, 12 `.asamu`) start with tag `C1 83 2A 9E`, file version
  868, licensee 0 (re-confirmed by `asamu-inventory`).
- The three `.tfc` files start with the same tag but are **not** package summaries: the next u32 is
  `0x00020000` (131,072), followed by two small u32 sizes (first chunk e.g. 7,002 → 8,192 in
  `Textures.tfc`). Reported as `ue3-compressed-chunks`.
- The three `GlobalShaderCache-PC-*.bin` start with ASCII `BMSG` followed by u32 868.
- `Contents/MacOS/ASAMU` is a thin **x86_64-only** Mach-O executable; `libsteam_api.dylib` and
  `openal.dylib` are fat i386+x86_64; `libSDL2-2.0.0.dylib` is a fat container with a single x86_64
  slice. No arm64 code ships.
- Windows/PC artifacts in the Mac depot: Direct3D shader caches (`GlobalShaderCache-PC-D3D-SM3/SM5.bin`,
  `RefShaderCache-PC-D3D-SM3/SM5.upk`, together 145,726,403 bytes) next to the OpenGL ones; `WinDrv.u`;
  `ASAMU/Splash/PC/`; and the `PCTOC*.txt` files, which list 56 Windows-only files absent here
  (80,415,329 TOC bytes) under `Binaries\` — among them `Binaries\Win32\ASAMU-Win32-Shipping.exe`
  (42,971,136 B), `ASAMU-Win32-Shipping.com`, `ASAMU-Win32-Shipping.exe.config`, `steam_api.dll`,
  PhysX/APEX DLLs, wxWidgets DLLs, `UnrealLightmass.exe`, `UE3ShaderCompileWorker.exe`,
  `Binaries\Win32\UserCode\UnSetupNativeWrapper.exe` and `Binaries\build.properties`.
- Editor content ships: 1,080 files (18,373,230 B) under `Engine/EditorResources` (wxWidgets `.xrc`
  layouts and bitmaps, WPF `.xaml`, FaceFX editor data incl. `FonixData.cdf`, `FaceFX.chm`, GPL/LGPL
  texts, `EULA.rtf`), `Engine/Extras/3dsMaxScripts/PivotPainter.ms`, plus the editor script packages
  `UnrealEd.u`, `UTEditor.u`, `GFxUIEditor.u` in `CookedMac`. `Engine/Config` also carries
  `Linux/`, `Mobile/` and `PCServer/` platform configs.
- PC TOC vs disk: 1,341 of 1,397 `PCTOC.txt` entries exist on disk (1,291 at the exact path, 50 after
  mapping `CookedPC`→`CookedMac` and `Engine/Splash/PC`→`Mac`); 1,327 of those have identical sizes.
  All 48 `..\ASAMU\CookedPC\...` entries have a `CookedMac` counterpart and **47 of 48 have exactly
  the TOC size**; the exception is `RefShaderCache-PC-D3D-SM3.upk` (+44,218 B on disk).
- All 11 TOC-listed `Engine/Config` files (10 `Base*.ini` and `ConsoleVariables.ini`) are smaller on disk than in the TOC by
  exactly their line count, and contain no CR bytes: they were converted from CRLF to LF for the Mac
  depot. (`ASAMU/Config/*.ini` match the TOC sizes.) The Windows depot's copies have exactly the
  TOC sizes, and these 11 are the only files present in both depots that differ.
- `Engine/Shaders/Binaries`: 147 `.bin` files, all starting `01 00 00 00`; the rest is high-entropy
  (median 7.96 bits/byte over the 116 files ≥ 1 KiB). File names mirror UE3 `.usf` shader source names
  (`MaterialTemplate`, `BasePassPixelShader`, `Common`, ...). The PC TOC lists them with identical sizes.
- 9 empty files: three FaceFX `placeholder.txt` and `Engine/Localization/<LANG>/Properties.<LANG>` for
  all six engine languages.
- Text encodings: 258 UTF-16LE files (250 of the 276 language-extension files, plus 8 `.xaml`;
  of the other 26 language files 20 are plain ASCII and 6 are empty), 93 ASCII, 30 UTF-8 (28 `.xaml`,
  one `.ini`, `UDKOffline.html`), one UTF-16BE (`InfoPlist.strings`).
- `ASAMU/Config/DefaultEngine.ini` sets `MapExt=asamu` and `SeekFreePCPaths=..\..\ASAMU\CookedPC`.
- **The Mac depot is the PC cook** (was STRONG from sizes alone): all 48 files of `CookedMac`
  (42 packages, 3 texture file caches, 3 global shader caches; 1,145,238,923 bytes) are
  byte-identical to the 48 files of the Windows depot's `CookedPC` (same SHA-256, see
  ["Windows depot"](#windows-depot-278361)). The cook TOC is a `PCTOC`, shader caches are PC-named,
  and the config points `SeekFreePCPaths` at `CookedPC`. Importer consequence: one set of decoders
  serves both builds, and the conversions' inputs are the same bytes.
- The Windows layout is `Binaries/Win32/ASAMU-Win32-Shipping.exe`, `ASAMU/CookedPC/` and `Engine/`
  directly in the install folder, as the PC TOC lists it (was STRONG; now read from a Windows
  install). No `Win64` folder exists. `asamu-locate` reports that shape without a warning; it still
  accepts `ASAMU.exe`/`UDK.exe`, `Win64` and `CookedPCConsole` with an UNVERIFIED warning, and a
  Linux layout is UNVERIFIED (no Linux depot has been examined).

### STRONG

- (nothing at the moment)

### TENTATIVE

- The D3D shader caches, `WinDrv.u`, the PC splash bitmaps and the editor resources are unused by the
  Mac runtime (inferred from their nature; not traced).
- The `RefShaderCache-PC-D3D-SM3.upk` size difference suggests that file was re-cooked or replaced
  after the TOC was written. (It is not something the Mac port introduced: the Windows depot
  ships the same bytes.)
- `Engine/Shaders/Binaries/*.bin` are packed (compressed and/or encrypted) shader sources.

### UNKNOWN

- The container format of `Engine/Shaders/Binaries/*.bin`.
- Why `ASAMU/Localization/INT/ASAMU.int` is 12 bytes smaller than its TOC size (UTF-16LE, no CR bytes on disk;
  the Windows depot ships the same bytes).
- Whether App 278360 has a Linux depot, and what it holds (none has been examined).

## Windows depot 278361

Source: a Steam install on a Windows 11 machine, inventoried on 2026-10-10 with the tools above
(built for Windows). All of this section is CONFIRMED unless a line says otherwise.

### Steam metadata

| Field | Value | Source |
|---|---|---|
| App ID | 278360 | `steamapps/appmanifest_278360.acf` |
| Install dir | `A Story About My Uncle` | appmanifest `installdir` |
| Depot | 278361 (manifest 8314284209762417428) | appmanifest `InstalledDepots` |
| Build ID | 1822049 (the build of the inventoried macOS depot) | appmanifest `buildid` |
| Size on disk | 1,258,264,110 bytes | appmanifest `SizeOnDisk` and the depot's `size`; equals the sum of all inventoried file sizes exactly |
| Files | 1,699 | `asamu-inventory`; no symbolic links, junctions, hidden or system files |
| StateFlags | 4 (fully installed) | appmanifest `StateFlags` |

`asamu-locate`: layout `windows`, found through the default Steam root's own library, build 1822049,
depot 278361, no warnings (the unit test `real_install_if_present` asserts these when that depot is
installed). The game had been played on that machine, and the install holds depot files only: no
generated configuration, log or save is in it (a `My Games` folder named after the game exists in
the user's documents folder; that the game writes there is TENTATIVE, it was not opened).

### Layout

```
A Story About My Uncle/
├── ASAMU/
│   ├── Build/         CookerSync_Game.xml
│   ├── Config/        Default*.ini (11)
│   ├── CookedPC/      12 .u, 9 .upk, 3 .tfc, 3 GlobalShaderCache .bin
│   │   └── Maps/      12 .asamu + 9 *_LOC_INT.upk
│   ├── Localization/  the same 14 language folders as the macOS depot
│   ├── Splash/PC/     Splash.bmp, EdSplash.bmp
│   └── PCTOC.txt, PCTOC_<LANG>.txt (13 languages)
├── Binaries/
│   ├── build.properties
│   └── Win32/         ASAMU-Win32-Shipping.exe (42,971,136 bytes, PE i386), ASAMU-Win32-Shipping.com,
│                      ASAMU-Win32-Shipping.exe.config, steam_api.dll, steam_appid.txt, 47 more DLLs,
│                      UE3ShaderCompileWorker.exe, UnrealLightmass.exe, UserCode/UnSetupNativeWrapper.exe
└── Engine/
    ├── Config/        Base*.ini (10), ConsoleVariables.ini (no platform subfolders)
    ├── Localization/  9 folders: CZE DEU ESN FRA HUN INT ITA POL SLO
    ├── Shaders/Binaries/  147 .bin
    ├── EditorResources/, Extras/3dsMaxScripts/, Splash/PC/, Stats/
```

All 53 executable images (4 `.exe`, 48 `.dll`, 1 `.com`) are 32-bit (`PE i386`); there is no `Win64`
folder. `ASAMU-Win32-Shipping.exe` has SHA-256
`17f2aeb601086089f4cbf144d2eb2b4b672c2d0594ad81f8f75ba27736d26e4e`, the hash `WINDOWS_BINARY.md`
recorded independently for the executable it analysed.

### Per category

| Category | Files | Bytes |
|---|---:|---:|
| `texture-related` | 3 | 568,543,124 |
| `map` | 12 | 310,259,743 |
| `shader` | 153 | 172,765,731 |
| `ue3-package` | 27 | 94,809,180 |
| `executable` | 53 | 80,414,760 |
| `editor-resource` | 1080 | 18,373,230 |
| `localization` | 318 | 8,718,549 |
| `image` | 4 | 2,928,224 |
| `metadata` | 17 | 1,231,599 |
| `config` | 23 | 208,468 |
| `engine-resource` | 9 | 11,502 |

The other tables (per extension, per type, the 42 packages with their hashes, the TOC cross-check)
are in the generated summary.

### How the two depots differ

Paths are compared after removing the macOS depot's `A Story About My Uncle.app/Contents/Resources/`
prefix and reading `CookedMac` as `CookedPC` (regenerate: compare the `files` arrays of the two
committed JSON inventories by path and `sha256`).

| | Files | Bytes |
|---|---:|---:|
| In both depots, byte-identical (same SHA-256) | 1,588 | 1,173,937,075 |
| In both depots, different | 11 | Windows copies larger by 3,666 in total |
| Only in the Windows depot | 100 | 84,152,852 |
| Only in the macOS depot | 37 | 72,241,677 |

- **Byte-identical: everything the importer reads, and all other game data.** The whole cooked
  folder (48 files, 1,145,238,923 bytes: the 42 packages, the 3 texture file caches and the 3 global
  shader caches), `ASAMU/Config` (11), `ASAMU/Localization` (191), the 14 TOC files,
  `ASAMU/Build` (1), `ASAMU/Splash/PC` (2), `Engine/EditorResources` (1,079), `Engine/Extras` (1),
  `Engine/Shaders` (147), `Engine/Stats` (9) and the six engine languages both depots have (85).
  The importer's test `embedded_inventory_lists_the_windows_inputs` asserts that the 45 input files
  of the two committed inventories have the same paths, sizes and hashes.
- **Different (11):** `Engine/Config/Base*.ini` (10) and `Engine/Config/ConsoleVariables.ini`. Each
  Windows copy is larger than the macOS copy by the difference the macOS TOC cross-check lists above
  (523, 51, 367, 1,114, 170, 9, 393, 251, 734, 34 and 20 bytes) and has exactly the size the TOC
  records: the macOS depot's copies had their CRLF line ends turned into LF.
- **Only Windows (100):** `Binaries/` (56 files, 80,415,336 bytes: the executables above, the two
  loose text files and `steam_appid.txt`), the engine localization of three more languages
  (`Engine/Localization/CZE`, `HUN`, `SLO`: 42 files, 2,273,404 bytes) and `Engine/Splash/PC`
  (2 files, 1,464,112 bytes).
- **Only macOS (37):** the application bundle's own files (`Info.plist`, `PkgInfo`, the four
  binaries in `MacOS/`, `English.lproj/` (2) and `Resources/steam_appid.txt`: 9 files), `ASAMU/Config/Mac`
  (4), `ASAMU/Splash/Mac` (2), `Engine/Config/Linux`, `Mac`, `Mobile` and `PCServer` (5 each) and
  `Engine/Splash/Mac` (2; same sizes as the Windows depot's `Engine/Splash/PC` files, different
  bytes).

### Cooker TOC cross-check

On Windows the TOC's `..\X` paths resolve at the install root itself, so nothing is remapped and
nothing is missing: all 1,397 `PCTOC.txt` entries exist at their exact path, and 1,393 have the TOC
size. The four that differ:

| Path | TOC bytes | Disk bytes | Difference |
|---|---:|---:|---:|
| `ASAMU/CookedPC/RefShaderCache-PC-D3D-SM3.upk` | 31,379,963 | 31,424,181 | +44218 |
| `ASAMU/Localization/INT/ASAMU.int` | 31,308 | 31,296 | -12 |
| `ASAMU/PCTOC.txt` | 88,813 | 88,877 | +64 |
| `Binaries/build.properties` | 87 | 94 | +7 |

The first three are the three non-config differences of the macOS depot's cross-check (the same
files, byte-identical in both depots). 302 files on disk are not in `PCTOC.txt`: the 13 language
TOCs, the localization of the 13 other game languages (177 files) and of the 8 other engine
languages (112 files). The 13 language TOCs each list every one of their entries at its exact path
with two size differences (their sets are in the JSON).

### Independent verification

- Two `asamu-inventory --out ... --summary ...` runs on the Windows machine produced byte-identical
  JSON (410,816 bytes) and summary files; the committed files are those. The JSON holds no absolute
  path, no drive letter, no backslash and no user name.
- Every one of the 1,699 files was hashed a second time with PowerShell's `Get-FileHash` (another
  SHA-256 implementation) and sized with the file system's own length: 1,699 of 1,699 rows agree
  with the JSON, with no file on either side missing from the other.
- `cargo test -p asamu-inventory --test real_install` on that machine re-inventories the install and
  compares every row and the TOC reports with the committed JSON: passed.

## Converting on Windows

`asamu-import` (built for `x86_64-pc-windows-msvc`, Rust 1.98.0) was run on the Windows depot on
2026-10-10, and its results compared with a conversion of the macOS depot made on the analysis Mac
(`aarch64-apple-darwin`). Everything here is a measurement of the importer, not of the game.

The measurements were repeated on 2026-10-11 with the importer that also writes simple collision
(`MESHES.md`, mesh manifest version 2): a conversion from nothing, the comparison with the macOS
conversion file by file (path, size and SHA-256 of all 13,407 and 13,091 files, hashed with another
SHA-256 implementation than the importer's), and the experiments named below. The macOS conversion
on record is older than both the simple-collision output and the portable math, so it is the
reference for what the macOS importer wrote *before* those changes.

### Result

`asamu-import all` finds the install through Steam, reports
`all 45 input files match win-depot278361-build1822049 (size and SHA-256)`, and all twelve stages
finish `ok`: 13,407 files, 2,286,853,777 bytes (13,091 files and 2,282,441,395 bytes before the
simple-collision output). The input fingerprint in the run manifest
(`install.inputs.sha256`, over the 45 paths, sizes and hashes) is
`604eea70f27bfa8397c6ded72b834dbfb3fef63fb4b7ed4730d449de6309c521` on both systems.

A stage's digest in the run manifest is a SHA-256 over the path, size and SHA-256 of every file the
stage wrote, so equal digests mean byte-identical output trees.

| Stage | Files | Bytes | Same digest as the macOS conversion |
|---|---:|---:|---|
| `textures` | 7,702 | 1,287,215,285 | yes |
| `meshes` | 2,081 | 70,525,421 | not comparable: the macOS conversion has no simple collision, see below |
| `materials` | 1 | 2,538,921 | yes |
| `levels` | 37 | 76,104,460 | no: 3 of 37 files, see below |
| `audio` | 774 | 58,757,448 | yes |
| `matinee` | 16 | 1,062,984 | yes |
| `skeletal` | 55 | 85,849,167 | yes |
| `kismet` | 13 | 3,010,893 | yes |
| `lightmaps` | 2,649 | 617,684,091 | no: 10 of 2,649 files, see below |
| `particles` | 13 | 5,649,857 | yes |
| `decals` | 50 | 77,016,867 | yes |
| `localization` | 16 | 1,438,383 | yes |

Nine stages are byte-identical across the two systems, file for file. `localization` is identical
although the Windows depot has three more engine languages, because the stage reads
`ASAMU/Localization` only.

`meshes`: the 882 `.gltf` and 882 `.bin` files are byte-identical to the macOS conversion's. The
other 317 files are the simple-collision output, which the macOS conversion on record does not
have: 316 `.collision.json` files (4,119,188 bytes) and `manifest.json`, which differs from the
macOS one in its version (2 against 1), the 882 `simple_collision` records and the 882
`content_hash` values (the hash now covers the collision switches and shapes) and in nothing else.
Before the simple-collision output the stage had the macOS digest (1,765 files, 66,113,039 bytes,
`9dbdd7105c7b30e9…`, measured 2026-10-10), and the counts and sizes after it are the ones
`MESHES.md` records for a macOS conversion (2,081 files, 70,525,421 bytes, a manifest of 1,675,457
bytes); no macOS digest of the new output is on record.

### The two stages that differed, and why

Both differences come from the C math library of the system the importer runs on, not from the
game data (which is the same bytes).

- **`levels`**: the scene files of `AG-Epilogue`, `AG-Workshop` and `ASAMUFrontEndMap` differ in 88
  numbers in all, every one an element of a `local_to_world` matrix that is smaller than 10⁻¹⁵ in
  magnitude on both systems (for example `0.0` against `-7.27331e-17`; the largest difference is
  1.6 × 10⁻¹⁶); nothing else in the 37 files differs. These elements are differences of two
  products of table sines that cancel mathematically. The rotator table is `sin(i · 2π / 16384)` evaluated in double precision, and for
  847 of its 16,384 entries the macOS library and the Windows C runtime return doubles that differ
  in the last bit (neither is the correctly rounded value for all entries: 633 and 512 entries
  differ from it, counted against 70-digit decimal arithmetic; counted again on 2026-10-11 from
  tables read out of both C libraries: 847, 633 and 512). Rounded to single precision the two
  tables are identical, which is why only cancelling terms show the difference.
- **`lightmaps`**: 8 of the 12 map indexes and two atlas textures differ. In the eight indexes, 26
  `max_irradiance` / `mean_irradiance` numbers differ in all (1 to 5 per map), by at most
  1.5 × 10⁻⁷ (relative), and nothing else. In the two atlases (`AG-BeautifulCity/atlas_72.dds`,
  `AG-StarHaven/atlas_100.dds`) 1 and 4 texels differ, each by one step of one 9-bit channel of the
  `R9G9B9E5` value (what a last-bit difference gives when a value lies at a rounding boundary of
  the encoder; TENTATIVE as the mechanism, the counts are measured). The light map decode raises `(c + 0.055) / 1.055` to the power 2.4 in single precision for each of the 256 byte values;
  the macOS `powf` returns a different last bit than the Windows one for two of them (bytes 71 and
  180). The Windows results are the correctly rounded ones for all 256.

Proof that nothing else is involved: a test build that returned the macOS library's 16,384 sines and
256 decoded bytes (tables recorded on the Mac) instead of computing them, run on the Windows
machine, produced the macOS conversion's digests for both stages exactly
(`ce92daaa24494d51…` and `7a4b4d04c263c8e8…`). The experiment was repeated on 2026-10-11 with
tables recorded anew from the Mac's C library, with the same result.

**Fix (in this repository):** the decoders now take these functions from
`asamu_ue3::portable_math` (the pure-Rust `libm` crate, and for the single-precision functions a
double-precision evaluation rounded once), which computes the same bits on every system: the sine,
cosine and power of `libm` 0.2.16 use IEEE arithmetic and exactly rounded operations only (read in
its source; its architecture-specific paths are exactly rounded operations such as the square
root, and 32-bit x86 without SSE, which is not a target here). What was moved:

| Function | Was | Reaches |
|---|---|---|
| `asamu_ue3::level::rotator_sin_cos` (the rotator sine table) | `f64::sin` | `levels`, and through `decal::rotator_axes` the decal frames |
| `asamu_ue3::lightmap::srgb_to_linear` | `f32::powf` | `lightmaps` |
| `asamu_ue3::material::color_to_linear`, and the folding of constant Power, Sine and Cosine material expressions | `f32::powf`, `f32::sin`, `f32::cos` | `materials` and what reads material parameters |
| `asamu_ue3::decal::DecalFrame::from_axes` (the decal's own rotation) | `f64::sin`, `f64::cos` | `decals` |
| `asamu-import` `decals::srgb_to_linear` (mask values) | `f32::powf` | `decals` |
| `asamu-import` `skeletal::rotator_to_quat` | `f64::sin_cos` | `skeletal` |
| `asamu-import` `lightmaps::rgb9e5` (the shared exponent) | `f64::log2`, now the exponent field of the number | `lightmaps` |
| `asamu-import` `lightmaps::linear_to_srgb_byte` | `f32::powf` | PNG previews (`--png`) only |

Tests pin whole tables by hash. The hashes of the three power tables are those of the correctly
rounded results, computed independently with decimal arithmetic; the sine and cosine hashes were
reproduced on macOS arm64 by a transcription of the algorithm into another language, and the
Windows x86_64 build returns the same bits. Measured on the Mac's C library: its `powf` returns the
neighbouring value for the bytes 71 and 180 at the exponent 2.4 and for 79 and 198 at 2.2; the
Windows C runtime's `powf` is correctly rounded for all 256 bytes at both exponents.

On Windows these changes are invisible in the output: a build with every one of these calls put
back on the Windows C runtime (the preview function excepted, which a default run does not use)
wrote the same digests for `materials`, `levels`, `matinee`, `skeletal`, `kismet`, `lightmaps`,
`particles` and `decals` (measured 2026-10-11). So the digests every system should now produce are
the ones in the table above: `levels`
`ddb720291028e5c8fcf8d87ee62fe75d0e2bb86c87f623915d4f2e78adf9004f`, `lightmaps`
`0a867328d4bd4f7c5eb358c6fd86bb7fa640e613383aecc425fa15d6c02561bf`.

What this means for a macOS conversion, which has not been repeated with the fixed importer:

- `levels` and `lightmaps` **will change** there, to the Windows digests: 88 matrix elements in 3
  scene files, and 26 numbers and 5 texels in 10 light map files, as listed above (STRONG: the
  functions are platform-independent by construction and pinned by tests, and the experiment above
  shows they were the only platform-dependent inputs of the two stages).
- The nine stages that are byte-identical on the two systems are expected to stay as they are
  (STRONG for the same reason, and because the Windows output did not change either); for `meshes`
  that holds for the files the macOS conversion on record has.
- Until someone runs it, the macOS conversion on record still has the old two digests.

One decoder still calls the platform's C library where its results reach converted data:
`asamu_ue3::matinee` (the single-precision rotator table, `atan2`, `acos` and `sin` of the
rotation conversions and the quaternion interpolation). It reproduces the engine's own evaluation
and has a second implementation in `asamu-kismet` that must agree with it, so it was left alone;
its output (`matinee`, and the transforms `kismet` embeds) is identical on macOS arm64 and Windows
x86_64 for the shipped data, and a third C library (Linux has not been measured) could differ there.

### Determinism, reruns and the folder swap (Windows, NTFS)

- Two `all --force` runs in a row wrote byte-identical run manifests (same SHA-256 of
  `asamu-import-run.json`, which holds all twelve stage digests), and so did every other full run of
  the same build.
- A rerun with nothing to do takes 0.13 to 0.18 s (seven measurements; 0.11 to 0.16 s in three more
  on 2026-10-11), reports every stage `up to date`, and leaves the manifest unchanged.
- Two conversions from nothing into two different folders wrote byte-identical run manifests
  (2026-10-11).
- The swap of a finished stage folder into place (two folder renames) worked in every run while no
  program had the files open, with no `.staging` or `.replaced` folder left behind.
- **Windows refuses to rename a folder while any program has a file inside it open**, whatever
  sharing mode that program asked for (measured: "Access is denied", error 5). With a converted file
  held open, the stage fails after about 2.7 s of retries with a message that says so, the previous
  output stays complete and in place, the staged copy is removed, and the next run (after the file
  is closed) converts the stage again. So: close the game runtime before re-importing.

### Options tried on Windows

- `--package-cache`: the cache of the 42 decompressed packages is 1,575,133,703 bytes and took 2.5
  to 3.1 s to build. A forced run through it gave the same twelve digests as a run without it,
  **after a fix**: the first such run wrote a different `levels` folder (36,408 bytes smaller, digest
  `02d8227b20f8fed3…`). `levels` was marked as reading nothing but packages, but it also reads the
  install's engine configuration files (they name the default post-process chain), which the cache
  does not hold; through the cache it left that chain out without a word. The stage reads the
  install now. This was not specific to Windows: it would have happened on any system. Repeated on
  2026-10-11: a conversion from nothing with the option wrote the same twelve digests as one
  without it, and a build with `levels` marked as reading only packages again wrote the smaller
  folder with that digest.
- `--png` (PNG previews): `textures` 15,403 files, 3,703,000,306 bytes; `lightmaps` 5,267 files,
  1,073,300,906 bytes; both `ok`. No macOS run with this option is on record to compare with.
- `--lang DEU`: `audio` 774 files, 58,761,691 bytes, the same digest with and without the package
  cache.

### Time

Seven full default conversions were timed. Other compile jobs were running on the machine (18 to
21 of its 24 cores were busy each of the six times it was sampled), so these are upper bounds, not
a benchmark: 60.0, 65.5 and 67.2 s for three runs, and 101.5, 314.7, 366.3 and 546.2 s for four
runs under heavier load. The macOS conversion took 44.4 s. Fastest Windows run, per stage (macOS in brackets):
textures 8.5 s (5.1), meshes 4.0 (2.9), materials 4.6 (3.8), levels 4.2 (3.0), audio 1.9 (1.7),
matinee 2.4 (2.0), skeletal 2.9 (2.3), kismet 4.7 (3.3), lightmaps 15.8 (9.3), particles 6.8 (5.0),
decals 2.3 (2.1), localization 0.1 (0.2). A 60 s run used 54 s of user and 28 s of kernel CPU time
and peaked at 1.6 GB of memory; real-time virus scanning was on (it was not changed). How long an
otherwise idle machine takes is UNKNOWN. The three conversions of 2026-10-11 (with the
simple-collision output, the machine again busy with other builds) took 111.6, 136.7 and 178.4 s,
and a fourth with `--package-cache` (which builds the cache first) 66.2 s.
