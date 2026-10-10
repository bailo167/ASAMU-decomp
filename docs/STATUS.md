# Status

_Session journal. Newest entry first. Each entry: what was done, what is true now, what to do next._

## 2026-10-10 — Session 2: first recordings of the original, showcase pass, Phase 8 merged

**Done**
- **Phase 8 merged**: particle systems, gameplay VFX and decals, localized text, camera animations and anim
  notifies, NPC collision, time trial, the gamepad mapping (found later in this session not to be connected to
  the simulation tick: only look, jump, pause and restart reach the game), water and foliage (tracker: 143
  items).
- **`asamu-import all` runs all twelve stages** (particles, decals and localization are chained in): 13,091
  files, 2.3 GB, 44 s on this machine.
- **The original Mac build runs on Apple silicon** under Rosetta with `-ONETHREAD` (it crashes without it: the
  rendering thread has no GL context). The LLDB recorder attached and passed its feasibility checks F1–F3
  (symbols, fixed step, live layout). It has **not** recorded a trace yet: the first attempt stalled the game
  because the driver drained debugger events too slowly; the driver loop was rewritten and still needs a live
  test. Details in `docs/TRACE_CAPTURE.md` §4.0.
- **Windows route built and used.** Static RE of the Win32 executable (globals, frame order, 1,982 native class
  sizes), a Win32 field layout, a read-only memory-polling recorder driven over SSH, and replay with each frame's
  own length. Against the running game on 2026-10-10:
  - the build, module and layout checks pass; **15,081 of 15,081 live property offsets** equal the derived
    layout, 3,226 of 3,226 bool masks, and the class default values of the six player classes sit where the
    layout says (CONFIRMED, live; the checkers' output is kept locally, and `docs/TRACE_CAPTURE.md` §6.5 still
    has to be updated with these results);
  - `DeltaSeconds` equals the clamped tick argument on every recorded frame (17,307 frames);
  - two checker expectations failed and are **not yet explained**: `UStruct.PropertiesSize` equals the
    registered native size for 1,803 of 1,954 classes only, and 630 layout fields had no live property object;
  - **first recordings of the original**: a story-mode walk in AG-Workshop (3,001 frames), four minutes of
    free play through AG-ParadiseCave into AG-BeautifulCity with sprinting, jumps, power jumps and about 3,800
    frames of grappling (14,311 frames), and a longer free-play recording. No torn, late or missed samples in
    the first; one torn and one late frame in the second, at a level change.
- **First replay and comparison** (AG-Workshop walk, variable step): the pipeline runs end to end and the
  verdict is *diverged*. What it shows so far:
  - the recorded walk reaches its speed cap with acceleration 2048 uu/s² per frame, as the physics spec says;
  - the original's `GroundSpeed` in the Workshop's story mode read **132** (and 66 about a second after level
    start); AG-BeautifulCity's story mode read 264. Our Workshop speed has to be checked against this;
  - with a gamepad the stick's deflection is not visible to the recorder at its sample point (the engine has
    already cleared the axes), so the converter produces no movement input and the replay stands still. The
    recorded pawn acceleration carries the direction; deriving the input from it is the next converter change;
  - positions differ by 1 uu from the first tick (not yet classified).
- **App**: skinned meshes (the first-person hand, villagers, Maddie, the worm) now use their converted
  materials and the hand is lit by the level's lights; the developer read-out is hidden on converted levels
  (F1 shows it); on-screen text uses only glyphs the built-in font has.
- **Showcase pass**: new README, `docs/SHOWCASE.md`, curated screenshots in `docs/images/` with a written policy
  (`docs/LEGAL.md`) and a hygiene rule that allows images only there.

**True now**
- Tracker: 143 items; the parity suite is no longer blocked on recordings (it is `partial`: recordings exist
  locally, no regression suite yet). Nothing gameplay-related was upgraded to verified.
- Running the original Mac build wrote its own configs, a log and saves into its app bundle; no original file
  changed (`asamu-inventory`'s check compares all 1,636).

**Next**
1. Converter: movement input from the recorded acceleration (gamepad); then replay the free-play recordings
   segment by segment (walk, sprint, jump, grapple attach/release), classify each first divergence and fix what
   the evidence supports, with regression tests. No tolerance is to be loosened to make a comparison pass.
2. Explain the two failed live checks (above) and the 66 / 132 story-mode speeds from the level's Kismet.
3. Sandbox mode (design done in this session; implementation next), kept apart from the faithful game.
4. Needs the owner: decide on a first tagged pre-release; a hand-played run through the recreation.

## 2026-10-10 — Morning summary (Session 1, overnight run)

**Where things stand**
- Every original data type decodes and converts from the user's own install (`asamu-import all`: ~13k files,
  2.2 GB, ~1.5 min): packages, script, class defaults, Kismet, bytecode, textures, static/skeletal meshes,
  animations, materials, audio, Matinee, levels, lightmaps.
- The app plays the story levels with the original movement physics, abilities and grapple (recovered values),
  triangle collision, checkpoints/kill zones, the original Kismet scripting (97/97 op classes), Matinee movers,
  NPCs/worm, audio (cues, narration + subtitles, adaptive music), baked lighting, fog/colour grading, menus and
  saves. The smoke harness follows the story chain Workshop → … → Epilogue.
- Tracker: 133 + 11 newly listed gap items; parity suite blocked on original-game traces (tooling ready:
  `tools/trace-recorder`, `asamu-trace`, `docs/TRACE_CAPTURE.md`).
- CI green on Windows/Linux/macOS; release workflow exists but no tag has been pushed (needs your go-ahead).

**Needs you**
1. Record original-game traces (Mac build via Steam + `tools/trace-recorder`, or Windows) to unlock behavioural
   verification of movement/grapple/abilities (M8, M12).
2. Decide whether to cut a first tagged pre-release (draft) with `release.yml`.
3. Original-save import needs the original's save encryption key read from your own executable — out of scope
   until you decide.

**Next (Phase 8)**: particles, decals, gameplay VFX (beam/velocity cone/speed lines), localized text, camera
animations/AnimNotify/look-at, NPC collision, time trial, gamepad, water/foliage.

## 2026-10-10 — Session 1: Phase 5 merged (M9 reached), Phase 6 running

**Done**: runtime world (triangle BVH collision, scene loading, death/checkpoints/streaming), Bevy rendering of
converted levels (AG-Workshop visually verified; placements checked on all maps), materials (approximate PBR),
audio decode (Ogg passthrough, cues, subtitles), Matinee (exact curve evaluation), skeletal meshes + animations.
**M9 reached**: an original level imports, renders and plays with original physics and grapple. Tracker scope
expanded (Kismet runtime, NPCs, worm, interactables, lightmaps, skinned render, trace capture, parity suite):
81% complete / 36% verified of 133 items.

**Running (Phase 6)**: Kismet runtime + Matinee movers, runtime audio + narration, menus + save system,
lightmaps, NPCs/worm/interactables + skinned rendering, `asamu-import all` + release workflow + PLAYING.md +
TRACE_CAPTURE.md.

## 2026-10-10 — Session 1 (overnight): gameplay port, importer foundations, bytecode, green CI

**Done**
- Gameplay defaults to the original: native physics port + 39 recovered parameters with provenance, pawn script
  layer (releasable jump, sprint 880, story speed, landing, zoom, power jump/leap), GrappleGun (range 5000, pull,
  2000 cap, release rules, budget/crystals), rocket boots, attractors, falling rocks; original frame order.
- Importer: textures → DDS (8,920 textures verified), static meshes → glTF (1,512/1,512 exact), level scenes
  (30,284 actors, BSP, volumes) — all independently re-decoded.
- Bytecode decoder: 12,801/12,801 scripts exact; token table matches the executable.
- **CI fully green** on Windows, Linux, macOS arm64 and the x86_64 cross-check (M0 complete).
- Tracker: 76% complete / 32% verified (125 items).

**Running**: Phase 5 — triangle collision + level load + death/checkpoints, Bevy rendering of converted levels,
materials, audio, Matinee, skeletal meshes/animations.

**Next**: Phase 6 — Kismet runtime subset + story sequencing, narration/subtitles, music, HUD/menus, save system,
chapter select, collectibles; then trace capture tooling for parity and release packaging.

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
