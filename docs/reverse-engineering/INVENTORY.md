# Original installation inventory

Source: the legitimately owned Steam installation on the analysis Mac. Read-only.

## Steam metadata — CONFIRMED

| Field | Value | Source |
|---|---|---|
| App ID | 278360 | `steamapps/appmanifest_278360.acf` |
| Install dir | `A Story About My Uncle` | appmanifest `installdir` |
| Depot | 278362 (manifest 7137994883443283717) | appmanifest `InstalledDepots` |
| Build ID | 1822049 | appmanifest `buildid` |
| Size on disk | 1,246,349,269 bytes | appmanifest `SizeOnDisk` |
| Files | 1,636 | `find -type f` |

## Layout — CONFIRMED

```
A Story About My Uncle/
└── A Story About My Uncle.app/Contents/
    ├── Info.plist             CFBundleIdentifier com.coffeestainstudios.astoryaboutmyuncle, version 1.1, LSMinimumSystemVersion 10.6
    ├── MacOS/
    │   ├── ASAMU              main executable (x86_64 Mach-O, 67,378,284 bytes)
    │   ├── libSDL2-2.0.0.dylib
    │   ├── libsteam_api.dylib
    │   └── openal.dylib
    └── Resources/
        ├── ASAMU/
        │   ├── Config/        Default*.ini, Mac/*.ini
        │   ├── CookedMac/     12 .u, 9 .upk, 3 .tfc, 3 GlobalShaderCache .bin
        │   │   └── Maps/      12 .asamu + 9 *_LOC_INT.upk
        │   ├── Localization/  15 languages
        │   ├── Splash/, Build/
        └── Engine/            Config, Localization, Shaders, EditorResources, ...
```

Executable SHA-256: `b611c4a0a64d220f3f2b8bdbd6287700327976bd2f196fca328a4b1b2d13d004`.

## CookedMac packages

Script packages present: `Core.u`, `Engine.u`, `GameFramework.u`, `GFxUI.u`, `GFxUIEditor.u`, `IpDrv.u`,
`OnlineSubsystemSteamworks.u`, `UDKBase.u`, `UTEditor.u`, `UTGameContent.u`, `UnrealEd.u`, `WinDrv.u`.

**Absent:** `ASAMU.u`, `UTGame.u` (both named in config; see SCRIPT_ANALYSIS.md).

Other packages: `Startup.upk` (52,103,624 B), `Startup_LOC_INT.upk`, `UDKBase_LOC_INT.upk`,
`ASAMUFrontEndFlash.upk`, `GlobalPersistentCookerData.upk`, `GuidCache.upk`, `RefShaderCache-PC-{D3D-SM3,D3D-SM5,OpenGL}.upk`.
Texture caches: `Textures.tfc` (444,637,918 B), `Lighting.tfc`, `CharTextures.tfc`.

Maps (`CookedMac/Maps`): `ASAMUEntry`, `ASAMULegal`, `ASAMUFrontEndMap`, `AG-Workshop`, `AG-BeautifulCity`,
`AG-Darkcave`, `AG-IceCave`, `AG-ParadiseCave`, `AG-StarHaven`, `AG-Epilogue`, `TheCore`, `Freds_place`
(all `.asamu`), most with a `<map>_LOC_INT.upk` companion.

A reproducible, hash-level inventory will be produced by `tools/asamu-inventory`.
