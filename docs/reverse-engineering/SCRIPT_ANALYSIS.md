# Script analysis — where is `ASAMU.u`?

`DefaultEngine.ini` names a script package `ASAMU`, but there is no `ASAMU.u` in `CookedMac`.

## CONFIRMED

- `[Engine.ScriptPackages]` lists `+NonNativePackages=UTGame`, `+NonNativePackages=UTGameContent`,
  `+NonNativePackages=ASAMU`, `+NativePackages=UDKBase`.
- `[UnrealEd.EditorEngine]` lists `+EditPackages=UDKBase, UTEditor, UTGame, UTGameContent, ASAMU`.
- `[Engine.StartupPackages]` lists `UI_Fonts, UI_Fonts_Final, UI_Fonts_Final_RUS, UI_Fonts_Final_UKR,
  FX_HitEffects, UDKFonts, UTGame, ASAMU, ASAMUFonts`.
- `[Engine.Engine] GameViewportClientClassName=ASAMU.ASAMUViewportClient` — a class in package `ASAMU`.
- `CookedMac` contains `UTGameContent.u` but **neither `UTGame.u` nor `ASAMU.u`**; none of the other
  `StartupPackages` (`UI_Fonts*`, `FX_HitEffects`, `UDKFonts`, `ASAMUFonts`) exist as files either.
- `Startup.upk` is 52 MB with 17,658 names (header field).
- The executable registers natives for package `ASAMU` (`AutoInitializeRegistrantsASAMU`), so a package named
  `ASAMU` must be loadable at runtime.

## STRONG

- Every package listed in `[Engine.StartupPackages]` is missing as a standalone file, while non-startup script
  packages (e.g. `UTGameContent`) are present. That pattern is consistent with UE3's cooker merging startup
  packages into a single seek-free `Startup.upk`.

## TENTATIVE

- `ASAMU` (and `UTGame`) classes are exported from `Startup.upk`, with `ASAMU`/`UTGame` as top-level package
  exports. To prove: parse `Startup.upk` exports and look for `Package ASAMU` and `Class ASAMU.*` objects.

## UNKNOWN

- Whether script bytecode (function bodies) survives cooking in `Startup.upk`.
- Whether map packages also contain copies of ASAMU classes.

## Evidence still to collect

Name/import/export tables of `Startup.upk` and every map; executable strings; cross-reference with symbols.
