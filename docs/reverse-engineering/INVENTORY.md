# Original installation inventory

Source: the legitimately owned Steam installation on the analysis Mac
(`~/Library/Application Support/Steam/steamapps/common/A Story About My Uncle`). Read-only: nothing
in the install is modified, and no original bytes are committed. Everything below is metadata
(paths, sizes, hashes, counts, magic numbers) produced by the repository's own tools.

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
# Re-inventory the install and diff it against the committed JSON (skips without game data):
cargo test -p asamu-inventory --test real_install
```

Committed sanitized data:

| File | Content |
|---|---|
| [`data/inventory/mac-depot278362-build1822049.json`](data/inventory/mac-depot278362-build1822049.json) | Per-file path, size, extension, SHA-256, sniffed type (+ UE3 version/licensee, short detail), category; per-category/extension/type totals; TOC cross-check reports. Schema `asamu-inventory/1`. |
| [`data/inventory/mac-depot278362-build1822049.summary.md`](data/inventory/mac-depot278362-build1822049.summary.md) | The generated Markdown summary (all tables below come from it). |

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
| 7 | `config` | extension `ini`, or a path component `Config` |
| 8 | `localization` | a path component `Localization`, or a UE3 language extension (`int`, `deu`, `fra`, ...) |
| 9 | `audio` | sniffed Ogg / WAVE, or extension `ogg` `wav` `mp3` `xma` `fsb` `bnk` `flac` `opus` |
| 10 | `movie` | sniffed Bink / SWF / GFx, or extension `bik` `bk2` `usm` `mp4` `avi` `webm` `swf` `gfx` |
| 11 | `font` | extension `ttf` `otf` `ttc` `fon` |
| 12 | `mesh-animation` | extension `psk` `psa` `fbx` `ase` `obj` `fxa` `apx` `apb` |
| 13 | `metadata` | `Info.plist`, `PkgInfo`, `steam_appid.txt`, cooker TOCs, `CookerSync*`, extension `plist` `strings` `nib` |
| 14 | `image` | sniffed raster image or image extension outside editor resources (splash screens) |
| 15 | `engine-resource` | `Engine/Stats/...` (FPS/memory chart HTML/CSS templates) |
| 16 | `documentation` | extension `txt` `html` `htm` `rtf` `chm` `pdf` `md` |
| 17 | `unknown` | everything else |

The Mac depot has no files in the audio, movie, font, mesh/animation, documentation or unknown
categories (CONFIRMED by the category totals below). Documentation-like files that do ship
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
  depot. (`ASAMU/Config/*.ini` match the TOC sizes.)
- `Engine/Shaders/Binaries`: 147 `.bin` files, all starting `01 00 00 00`; the rest is high-entropy
  (median 7.96 bits/byte over the 116 files ≥ 1 KiB). File names mirror UE3 `.usf` shader source names
  (`MaterialTemplate`, `BasePassPixelShader`, `Common`, ...). The PC TOC lists them with identical sizes.
- 9 empty files: three FaceFX `placeholder.txt` and `Engine/Localization/<LANG>/Properties.<LANG>` for
  all six engine languages.
- Text encodings: 258 UTF-16LE files (250 of the 276 language-extension files, plus 8 `.xaml`;
  of the other 26 language files 20 are plain ASCII and 6 are empty), 93 ASCII, 30 UTF-8 (28 `.xaml`,
  one `.ini`, `UDKOffline.html`), one UTF-16BE (`InfoPlist.strings`).
- `ASAMU/Config/DefaultEngine.ini` sets `MapExt=asamu` and `SeekFreePCPaths=..\..\ASAMU\CookedPC`.

### STRONG

- **The Mac depot reuses the PC cook.** 47/48 cooked files have exactly the sizes the PC cook's TOC
  records (sizes only; contents of the PC files are not available to compare), the cook TOC is a `PCTOC`, shader caches are PC-named, and the config still points
  `SeekFreePCPaths` at `CookedPC`. Importer consequence (TENTATIVE until packages are parsed):
  cooked data should use PC platform formats.
- Expected Windows layout: `Binaries/Win32/ASAMU-Win32-Shipping.exe` and `ASAMU/CookedPC/` (from the
  PC TOC, which the PC cooker wrote). No `Win64` entries appear. Not verified against a Windows depot;
  `asamu-locate` treats Windows/Linux layouts as UNVERIFIED and also accepts `ASAMU.exe`/`UDK.exe`
  and `CookedPCConsole`.

### TENTATIVE

- The D3D shader caches, `WinDrv.u`, the PC splash bitmaps and the editor resources are unused by the
  Mac runtime (inferred from their nature; not traced).
- The `RefShaderCache-PC-D3D-SM3.upk` size difference suggests that file was re-cooked or replaced
  after the TOC was written.
- `Engine/Shaders/Binaries/*.bin` are packed (compressed and/or encrypted) shader sources.

### UNKNOWN

- The container format of `Engine/Shaders/Binaries/*.bin`.
- Why `ASAMU/Localization/INT/ASAMU.int` is 12 bytes smaller than its TOC size (UTF-16LE, no CR bytes on disk).
- Depot IDs and contents of any Windows or Linux depot of App 278360 (none installed here).
