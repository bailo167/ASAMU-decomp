# Levels

How the shipped maps fit together: the story order, the role of every map, and a high-level flow of each
level. The order is derived from game data (the game-info level table, Kismet map transitions and level
streaming), not from map names. Kismet details and counts are in [KISMET.md](KISMET.md).

## What changed

The earlier TENTATIVE order, guessed from map names, was:
Workshop → BeautifulCity → Darkcave → IceCave → ParadiseCave → StarHaven → TheCore → Epilogue.

Game data shows a different order (below). ParadiseCave comes second and IceCave sixth. TheCore is not a
separate stop in the sequence: it is streamed into IceCave.

## Story order (CONFIRMED)

Two independent sources agree:

1. **The level table.** The class default object of `asamu.ASAMUGameInfo` stores `LevelFileNames` =
   `ASAMUFrontEndMap, AG-Workshop, AG-ParadiseCave, AG-BeautifulCity, AG-Darkcave, AG-StarHaven,
   AG-IceCave, AG-Epilogue`. The enum `asamu.ASAMUGameInfo.ASAMULevels` lists `NoLevel, Workshop,
   Sanctuary, Village, DarkCave, StarHaven, IceCave, Epilogue` in the same order.
   (`asamu-inspect defaults Startup.upk asamu.ASAMUGameInfo --inherited`, `asamu-inspect class ...`)
2. **The Kismet transitions.** Each story map has exactly one story transition, either an `open <map>`
   console-command action or a Kismet streaming action. Following them from AG-Workshop gives the chain
   below. `kismet_transitions_follow_the_level_table` in `crates/asamu-ue3/tests/kismet_real_data.rs`
   asserts both the chain and its agreement with the table.

| # | Map file | `ASAMULevels` | `WorldInfo.Title` | Menu label | Left by (Kismet) | Triggered by |
|---|---|---|---|---|---|---|
| 1 | AG-Workshop | Workshop | Workshop | Workshop | `open AG-ParadiseCave` | an interaction, or a touch relayed by a remote event |
| 2 | AG-ParadiseCave | Sanctuary | Sanctuary | Sanctuary | `open AG-BeautifulCity` | a touch trigger (directly, or relayed by a remote event) |
| 3 | AG-BeautifulCity (+ `freds_place`) | Village | Village | Village | `open AG-DarkCave` | touches relayed by a remote event |
| 4 | AG-Darkcave | DarkCave | DarkCave | Chasms | `open AG-StarHaven` | a touch relayed by a remote event |
| 5 | AG-StarHaven | StarHaven | StarHaven | Star Haven | `open AG-IceCave` | a touch volume |
| 6 | AG-IceCave (+ `thecore` streamed) | IceCave | IceCave | Ice Cave | `SeqAct_MultiLevelStreaming` loads `thecore` | a touch trigger |
| 6b | TheCore | — | (none) | — | `open AG-Epilogue` | `asamu.SeqEvent_CreditsEnded` |
| 7 | AG-Epilogue | Epilogue | Epilogue | Epilogue | `open ASAMUFrontEndMap?game=…` | a touch trigger |

Notes:
- Menu labels come from the main-menu class defaults (`LevelSelect*Tooltip` strings). The `ASAMULevels`
  name and the title for AG-Darkcave say "DarkCave", but the menu calls it "Chasms". CONFIRMED
- AG-BeautifulCity's `open AG-DarkCave` differs in case from the file `AG-Darkcave.asamu`. Map names
  resolve case-insensitively. STRONG
- Every middle map (2–6) also has an `open ASAMUFrontEndMap?game=ASAMU.GFxASAMUMenuGameInfo` action, the
  "back to menu" path. It sits in the same sub-sequence as the time-trial end action. CONFIRMED (wiring) /
  STRONG (its time-trial purpose)
- AG-BeautifulCity has one more `open ASAMUFrontEndMap` action that no event reaches. TENTATIVE: unused.

## Map roles (CONFIRMED unless noted)

| Map | Role | Evidence |
|---|---|---|
| ASAMULegal | Start-up map (`[URL] LocalMap`) | Game type `asamu.ASAMULegalGameInfo`. Kismet: level-loaded → `GFxAction_OpenMovie`, with no transition. The legal-screen movie class opens the front end itself (STRONG; behaviour of its script, read locally). |
| ASAMUFrontEndMap | Main menu (`[URL] Map`) | Game type `asamu.GFxASAMUMenuGameInfo`. Its `WorldInfo.Title` is "Workshop" and it reuses Workshop props, so the menu backdrop is built from the Workshop set (STRONG). Its Kismet (30 objects) plays a Matinee and a music track and opens the menu movie, with no transition. |
| ASAMUEntry | Transition map (`[URL] TransitionMap`) | No Kismet, no game type, a minimal level. |
| AG-* (7 maps) | Story levels | Game type `asamu.ASAMUGameInfo` in `WorldInfo.DefaultGameType`. Config gives the `AG` prefix the game type `ASAMU.ASAMUInfo`, a class that does not exist (see [CORRELATION.md](CORRELATION.md)). |
| TheCore | Sub-level of AG-IceCave, streamed in for the finale | AG-IceCave's `WorldInfo.StreamingLevels` holds a `LevelStreamingKismet` with `PackageName` `thecore`. AG-IceCave's package summary lists `thecore` in `AdditionalPackagesToCook`, and its Kismet loads it with `SeqAct_MultiLevelStreaming`. TheCore has no title or game type of its own and is absent from the level table and the enum. |
| Freds_place | Always-loaded sub-level of AG-BeautifulCity | AG-BeautifulCity's `StreamingLevels` holds a `LevelStreamingAlwaysLoaded` with `PackageName` `freds_place`, and its summary lists `freds_place` in `AdditionalPackagesToCook`. Its Kismet is a single empty `Main_Sequence`. |

How a game starts (STRONG; the main-menu and game-info classes, read locally, described in our own words):
- **New game** opens AG-Workshop with the ASAMU game type.
- **Continue** opens the entry of `LevelFileNames` at the saved level index.
- **Level select** has one button per story level. The widget names `lsmapworkshop`, `lsmapsanctuary`,
  `lsmapvillage`, `lsmapdarkcave`, `lsmapstarhaven`, `lsmapicecave` and `lsmapepilogue` are in the name
  table (CONFIRMED).
- **Time trial** offers the five middle levels (`ttmapsanctuary` … `ttmapicecave`, CONFIRMED names) under
  the game type `ASAMU.ASAMUGameInfoTimetrial`.

## Per-level flow (own words, high level)

Sources:
- The milestone traces of `asamu-inspect kismet MAP`.
- "At level start" means a path from `SeqEvent_LevelLoaded`, or from the save-state-loaded event. Such a
  path may pass through time-trial or save-state conditions (TENTATIVE which branch fires).
- Values are CONFIRMED and their meaning is STRONG; see [KISMET.md](KISMET.md).
- No dialogue is reproduced.

1. **Workshop.**
   - At level start: story mode is switched on and a slow movement speed is requested by console command.
   - Interacting with the suit plays the suit-on animation, shows the grapple hand and sets the grapple
     limit to 0.
   - A launch sequence (an interaction, or a touch relayed by a remote event) hides the grapple, shows the
     title logo and opens AG-ParadiseCave.
   - 6 narrator lines, 19 Matinee actions; no Kismet checkpoint triggers.
2. **ParadiseCave (Sanctuary).**
   - At level start: rocket boots are disabled, and the grapple limit is set to 2 on one path.
   - A touch volume enables the grapple (the first `ToggleGrapple` in the game). Touch volumes also set the
     limit to 1 on one path and to 2 on another.
   - 1 Kismet checkpoint trigger, 10 narrator lines and 30 tutorial pop-up actions.
   - A touch trigger at the exit opens AG-BeautifulCity.
3. **BeautifulCity (Village).**
   - At level start, or on a save-state load: the grapple is enabled and rocket boots are disabled. The
     limit is set to 2 on one path and to 3 on another, which a touch can also reach.
   - 1 checkpoint trigger and 11 narrator lines. This is the first map with `SeqEvent_AnimNotify` events
     (24).
   - Touches relayed by a remote event open AG-DarkCave.
4. **Darkcave (Chasms).**
   - At level start: the grapple is enabled, the limit is 3, rocket boots are disabled, and one checkpoint
     is triggered.
   - Has the worm encounter (start, pause and shut-down actions, 4 worm events), adaptive-music track
     control (17 volume-multiplier actions) and 22 narrator lines.
   - 2 checkpoint triggers in all.
   - A remote-relayed touch opens AG-StarHaven.
5. **StarHaven.**
   - At level start: the grapple is enabled and the limit is 3.
   - Rocket boots are enabled for the first time in the story, by an interaction and a touch in a cutscene
     sub-sequence. A third enable sits on a level-start path (TENTATIVE: for loads straight into the
     map).
   - 3 checkpoint triggers and 1 checkpoint enable; 30 narrator lines, the most of any map.
   - 10 prefab instances (moving crane and wheel winches and an airship), 57 Matinee actions.
   - A touch volume opens AG-IceCave.
6. **IceCave.**
   - At level start: the grapple, the limit of 3 and the rocket boots are granted, and one checkpoint is
     disabled. A falling-rocks sequence activates that checkpoint again when a player-grappled event
     arrives through a remote event.
   - 2 checkpoint triggers and 13 narrator lines.
   - A touch trigger loads TheCore by Kismet streaming. The map is not changed.
7. **TheCore** (inside IceCave).
   - One flat sequence: 5 narrator lines, 7 Matinee actions, crosshair removal and pause-menu disabling.
   - When the credits finish (`asamu.SeqEvent_CreditsEnded`), it opens AG-Epilogue.
8. **Epilogue.**
   - At level start: story mode is on, a very slow speed and a larger player size are requested by console
     command, and `SeqAct_SetGameFinished` fires.
   - 4 narrator lines.
   - A touch at the end returns to the main menu.

## Package-level facts

- `[URL] MapExt=asamu`, `Map=ASAMUFrontEndMap.asamu`, `LocalMap=ASAMULegal.asamu`,
  `TransitionMap=ASAMUEntry.asamu`; `[Core.System] +Extensions=asamu`. CONFIRMED (config)
- The 12 `.asamu` files in `CookedMac/Maps` (see INVENTORY.md) all begin with the UE3 tag, file version
  868. CONFIRMED
- `.asamu` is the ordinary UE3 map package format under the game's own extension. All 12 maps have
  PackageFlags `0x228A0009`, which includes `ContainsMap` (`0x00020000`). Each holds a `World` export
  `TheWorld` and a `Level` export `TheWorld.PersistentLevel`. CONFIRMED (this was STRONG before the object
  decoder; now verified through `asamu-inspect map` and the Kismet roots, whose outer is the `Level`).

## Open items

- Conditions in milestone traces are not evaluated, so "at level start" is per path, not per playthrough.
  A behavioural trace from the original game would settle which branch runs on a fresh story start.
- Some console commands issued at level start (`SaveLevelTwo/Three/Four`, `CanGrapple`, `CannotGrapple`,
  `ThreeGrapples`, `TGCD`) have no handler in the shipped build (STRONG, [KISMET.md](KISMET.md)). Saving
  progress and ability state therefore comes from script (the save system and the Kismet ability
  actions), not from these commands.
