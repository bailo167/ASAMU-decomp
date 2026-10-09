# Levels

## CONFIRMED (config + file system)

- `[URL] MapExt=asamu`, `Map=ASAMUFrontEndMap.asamu`, `LocalMap=ASAMULegal.asamu`,
  `TransitionMap=ASAMUEntry.asamu`; `[Core.System] +Extensions=asamu`.
- 12 `.asamu` files in `CookedMac/Maps` (see INVENTORY.md); all begin with the UE3 tag, version 868.

## STRONG

- `.asamu` is the game's map extension for ordinary UE3 map packages (config + identical header format).
  Header PackageFlags of the maps (`0x228A0009`) include bit `0x00020000`, which is `PKG_ContainsMap` in UE3
  — to be confirmed against v868 flag definitions and a `World`/`Level` export.

## TENTATIVE

- Story order from map names: `AG-Workshop` → `AG-BeautifulCity` → `AG-Darkcave` → `AG-IceCave` →
  `AG-ParadiseCave` → `AG-StarHaven` → `TheCore` → `AG-Epilogue`. Must be confirmed from game data
  (Kismet level transitions / chapter tables), not names.
