# Status

_Session journal. Newest entry first. Each entry: what was done, what is true now, what to do next._

## 2026-10-09/10 — Session 1 (overnight): object decoder, defaults, Kismet, behaviour specs, save, binary

**Done**
- Object payload decoder + class model (all 70,946 script objects and 2,521 CDOs consume exactly; independent
  Python decoder agrees). `asamu-inspect class/defaults/props/coverage/kismet/scripttext`.
- Real gameplay defaults with provenance (`DEFAULTS.md`, `data/defaults/`): pawn GroundSpeed 440, AccelRate 2048,
  JumpZ 1000, AirControl 0.3, MaxStepHeight 26, WalkableFloorZ 0.78, collision 21/44, eye 38, bLimitFallAccel true;
  GrappleGun range 5000, accel 2000, max speed 10000, release distance 200. Script layout = native layout.
- Kismet graphs for all 12 maps (0 dangling links); level order confirmed from data; per-level grapple limits.
- `GRAPPLE.md`, `ABILITIES.md` behaviour specs; `SAVE.md`; complete `BINARY_ANALYSIS.md`; faithful native physics
  port `Ue3PawnMovement` (177 tests).
- Disk filled once (per-agent cargo target dirs); cleaned. Agents now share a target dir.

**Next**: Phase 3 gameplay port (real params, jump/sprint/landing, grapple, power jump, rocket boots) and Phase 4
importer foundations (textures, static meshes, level actors) in parallel; bytecode introspection.

## 2026-10-09 — Session 1 (continued): package reader, ASAMU.u found, symbol map

**Done**
- `asamu-ue3` + `asamu-inspect`: verified UE3 v868 reader with safe LZO1X; all 42 packages parse with zero
  findings; independent Python + liblzo2 cross-check agreed on every name/import/export row and stream hash.
- `asamu-symbols`: native registration map (1,535 native classes, 2,505 exec thunks); ASAMU native surface is
  only the settings manager.
- `asamu-locate` + `asamu-inventory`: Steam discovery and a byte-reproducible sanitized inventory (committed).
- **Missing `ASAMU.u` solved (CONFIRMED):** merged into `Startup.upk` as package `asamu` (172 classes,
  4,371 exports). `ASAMUPawn → UTPawn → UDKPawn`; grapple = `GrappleGun → UDKWeapon` +
  `ASAMUPlayerController` state `Grappling`. Every class ships a `ScriptText` source buffer (local reading only;
  never commit — see CLAUDE.md).

**Next (highest value)**
1. Object payload decoding: tagged properties + UClass/UStruct/UFunction/UProperty for v868, verified by exact
   payload consumption across all script packages.
2. Class default objects → real values for `ASAMUPawn`/`UTPawn`/`UDKPawn` (GroundSpeed, AccelRate, AirControl,
   JumpZ, air-control flag, CustomGravityScaling), `GrappleGun`, `ASAMUPowerJump`, `ASAMURocketBoots`.
3. Grapple/power-jump/rocket-boost behaviour spec (local reading of script + bytecode introspection), then a
   faithful `asamu-player` port on top of the native physics spec.
4. Kismet graphs per map (sanitized), checkpoint and level flow.

## 2026-10-09 — Session 1 (continued): native physics spec, player slice

**Done**
- Ghidra 12.1.4 headless project of the unstripped Mach-O (analysis 647 s) with committed scripts
  (`tools/ghidra-scripts`) and anchor lists; decompiled output stays in ignored `research/`.
- `docs/reverse-engineering/NATIVE_PHYSICS.md`: verified behavioural spec of the native pawn physics the player
  runs on (sub-stepping, CalcVelocity/braking, walking, falling with displacement-derived velocity, gravity chain).
- Config evidence: `ASAMU.ASAMUGameInfo`, `ASAMU.ASAMUPlayerController`, `ASAMU.ASAMUInfo` (AG maps),
  `[UTGame.UTPawn]`, `DefaultGravityZ=-520`, input exec bindings and PlayerInput constants.
- Player slice (placeholder physics, deterministic): `asamu-core` (units, coords, rotators, clock, provenance,
  deterministic trig), `asamu-player` (input, params with provenance, collision world, movement model trait,
  grapple, sim, JSONL traces + compare), `asamu-world` graybox level, `asamu-game` tick, Bevy graybox app.
  118 tests.

**Next**
1. Merge the Phase-1 workstreams (UE3 reader + LZO, symbols, locator/inventory) once verified.
2. Prove where the ASAMU script classes live (parse `Startup.upk`), recover class defaults (pawn speeds, JumpZ,
   air control flag) and grapple script.
3. Replace `PlaceholderMovement` with a faithful port of the native physics spec, fed by recovered defaults.

## 2026-10-09 — Session 1: bootstrap

**Done**
- Repository bootstrapped: Rust workspace (crates/tools/app), source-hygiene `.gitignore`, `tools/repo-hygiene`,
  `progress/progress.toml` + `tools/progress-gen` (SVG matrix + README table with `--check`), docs skeleton,
  `CLAUDE.md` operating contract.
- Re-verified previous-session claims against the installed game (see `docs/reverse-engineering/`).

**True now** (details and confidence in `docs/reverse-engineering/`)
- Install: Steam App 278360, depot 278362, build 1822049, Mac app bundle `A Story About My Uncle.app`. CONFIRMED
- Executable `Contents/MacOS/ASAMU`: x86_64 Mach-O, PIE, **not code-signed**, **not stripped**:
  135,093 symbols (134,643 defined, 450 undefined), 23,631 local text symbols. CONFIRMED
- All 42 UE3 packages (`.u`, `.upk`, `.asamu`) start with tag `0x9E2A83C1`, file version 868, licensee 0. CONFIRMED
- No `ASAMU.u` (and no `UTGame.u`) on disk; both are listed in `[Engine.StartupPackages]`. STRONG lead toward
  `Startup.upk` (52 MB). Not yet proven by parsing.
- ASAMU-specific native code visible in symbols is limited to `UASAMUSystemSettingsManager` and the ASAMU package
  registrants. No grapple-related symbols exist in the executable. STRONG indicator that gameplay is UnrealScript.

**Next**
1. Symbol classifier (`tools/asamu-symbols`) with sanitized statistics.
2. Steam locator + inventory.
3. UE3 v868 package reader (summary → names → imports → exports → compression).
4. Prove where the ASAMU script classes live (parse `Startup.upk` exports).
