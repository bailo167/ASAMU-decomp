# Menus, settings and saves

Status of the Bevy menus (`apps/asamu/src/ui.rs`, `apps/asamu/src/ui/`) and of the save system
(`crates/asamu-game/src/save.rs`). The original's behaviour these follow is documented in
`docs/reverse-engineering/SAVE.md` (file layout, lifecycle, progression model; labels there). This page
describes **our** implementation and says where it deliberately differs. Confidence labels: CONFIRMED /
STRONG / TENTATIVE / UNKNOWN as in `CLAUDE.md`; "ours" marks a choice of this project with no original
counterpart.

## Summary

| Area | State |
|---|---|
| Save model + format (progression, general, snapshot, time trial, settings) | implemented, unit-tested (round trip, corruption, quarantine, migration chain, lifecycle) |
| Snapshot capture / apply on a running `Game` | implemented (checkpoint table, grapple capacity, boots, grapple latch, reset to the latest checkpoint); actor and Kismet entries are opaque slots for the world / Kismet workstreams |
| Main menu, chapter select, time-trial select, settings, pause menu, confirmation, loading screen | implemented (Bevy UI, `bevy_ui_widgets` buttons, Tab / Enter, Esc = back) |
| New Game / Continue / chapter select loading converted maps | implemented and checked on a local conversion (see "Verification") |
| Checkpoint snapshot saves from the simulation | implemented (`GameTick` message after every tick; real-data test) |
| Collectibles counter, story-item / achievement / extra notices | implemented on the save side and in the UI; **no producer yet** (collectibles and optional story items are not modelled in `asamu-world`; achievements come from Kismet) |
| Kismet saved strings, finished flag, time-trial start/end, `open <map>` chain | implemented as messages; **no producer yet** (Kismet host) |
| Settings persisted and applied | FOV, mouse sensitivity / invert, window mode and size, master volume (Bevy global volume), subtitle visibility; music / SFX / voice volumes are stored and exposed for the audio workstream |
| Localized menu text from the user's install | loader for an optional user-local `ui/strings.json`; producing it from the install is an importer task (not done) |
| Import of the original's own save files | out of scope; documented stub (`asamu_game::save::import_original_saves`) |

## Save system (`asamu_game::save`)

### Where

`SaveStore::default_root()`: `ASAMU_SAVE_DIR` when set, else `asamu-decomp/` under the platform's per-user
application-data folder — macOS `~/Library/Application Support`, Windows `%LOCALAPPDATA%`, elsewhere
`$XDG_DATA_HOME` or `~/.local/share` (the same base the importer's default output uses). Saves go to
`<root>/saves/`, settings to `<root>/settings.json`. Never the repository, never the original game's
folders (SAVE.md §9.1).

### Documents (format version 1)

Four documents mirror the original's four save lifetimes so that New Game, chapter select, Continue and
time trial reset exactly what the original resets (SAVE.md §2, §5); settings are a fifth document.

| File | Original (SAVE.md) | Fields |
|---|---|---|
| `progression.json` | `PlayerProgression.bin` (§4.2) | `unlocked_chapters` (chapter enum names, first-entry order), `collectibles` (chapter → actor paths relative to the map), `story_items` (`<level file><parent path or None>` keys, Q5), `achievements` (`EASAMUAchievements` names), `finished_game` |
| `general.json` | `GeneralSave.bin` (§4.1) | `current_chapter` (Continue target, `null` after New Game), `saved_strings` (Kismet flag id → int) |
| `snapshot.json` | `SaveGame.bin` (§4.3) | `map`, `checkpoints` (chapter → latest checkpoint index), `abilities` (`max_grapples`, `rocket_boots`, `grapple_enabled`, or `null` when written from the menu), `actors`, `kismet` (opaque objects keyed by object path) |
| `time_trial.json` | `TTS.bin` (§4.4) | `best_seconds` (collectible chapter → best time) |
| `settings.json` | user ini files (§7) | see "Settings" |

Every file is a JSON object with `"format": "asamu-decomp/<kind>"` and `"format_version"`. Keys are written
sorted (deterministic output); chapters and achievements are written by name, not number, so files stay
readable and survive enum reordering. No encryption (local single-player data; SAVE.md §9.1).

The JSON reader/writer is our own small strict implementation inside `save.rs` (the crate has no
`serde_json` dependency): depth limit 64, finite numbers only, validated escapes and surrogate pairs,
nothing after the value, last duplicate key wins. `f32` values are written in their shortest form that
reads back as the identical `f32`.

### Writing, corruption, versions (ours)

- **Atomic write**: temporary file `.<name>.tmp` in the same directory, `sync_all`, rename over the target;
  a failed write removes the temporary file. A crash leaves the old or the new document, never a torn one.
- **Unreadable documents** (malformed JSON, wrong `format`, missing/invalid `format_version`, a field of the
  wrong shape, not UTF-8, larger than 16 MiB) are **quarantined**: renamed to `<name>.corrupt-<n>.json`
  (first free `n` up to 999; when the rename fails the file is copied there instead), never overwritten in
  place; the loader then starts that document from its defaults and reports a `LoadIssue` (the app logs it
  and shows a notice). When neither rename nor copy works, the document is **protected** for the rest of
  the session: every write of it is refused (`SaveError::Protected`) so the file survives for manual
  recovery (settings: changes are applied but not saved).
- The size limit is enforced while reading (at most 16 MiB + 1 byte is read), not only by the size check
  before it.
- **Newer `format_version`** than this build reads: treated as unreadable and quarantined the same way, so
  a downgrade never destroys a newer save.
- **Older versions** run through `upgrade_steps(kind)` (entry *i* upgrades version *i+1* to *i+2*). Version 1
  is the first format, so the chain is empty today; a gap in a chain is an error, not a silent pass. The
  mechanism is tested with test-only steps. Additive changes need no step: missing fields take their
  defaults and unknown fields are ignored.

### Lifecycle (`SaveSession`)

| Original moment (SAVE.md §5) | Our hook | Notes |
|---|---|---|
| Menu start: load progression, then general; "legible" when both load | `SaveSession::open` | Missing / unreadable documents are written back at once (the original rewrites both after a reset) |
| Main menu New Game: fresh snapshot (empty checkpoint table, no abilities), chapter pointer cleared, flags kept; open `AG-Workshop` | `new_game` | everything else survives (SAVE.md finding 9, CONFIRMED) |
| Level select: fresh snapshot; open the chapter's map | `start_chapter` | locked chapters refused; Workshop always unlocked (§6.2) |
| Continue: open `LevelFileNames[currentLevelIndex]` | `continue_game` / `continue_target` | needs a legible save **and** a chapter pointer (our Q6 fix) |
| Story map start: unlock the chapter (progression written every time), set the pointer (general) | `begin_level` / `begin_level_lossy` | chapter from `WorldInfo.Title` (case-insensitive), else the map name; the front end (`ASAMUFrontEndMap`, title also `Workshop`) is never a chapter; time trial changes nothing |
| Pawn spawn: load the snapshot, restore table + abilities, reset to the latest checkpoint, fire "save loaded" with the index; on a failed load fire it with −1 | `Game::apply_snapshot` (returns `SnapshotApplied`); app: `ui::flow::level_start` | see below; story play without a snapshot still sends `SnapshotLoaded` with −1 (the player stays at the `PlayerStart`); time trial loads nothing and sends nothing (A-TT-2) |
| New latest checkpoint: write the snapshot | `checkpoint_saved(&TickReport)` + `Game::capture_snapshot` + `on_checkpoint_saved` | not in time trial |
| New collectible / story item / achievement: write progression | `on_collectible`, `on_story_item`, `on_achievement` | extras notice at exactly 10/15/20/25; `ALL_COLLECTIBLES_FOUND` at 5 here + 25 total; `INTERACT_ALL_STORY` at 11 keys (§6.2–6.4) |
| `SeqAct_EditOrAddSaveString`: write general | `on_save_string` | reads: `General::save_string` |
| `SeqAct_SetGameFinished`: finished flag | `on_game_finished` | unlocks time trial |
| Time-trial end: store if faster or first, file written every finish; five golds → `ALL_GOLD_MEDALS` | `on_time_trial_end` | medals from the CDO targets (§6.5, CONFIRMED (cdo)) |
| Death / quick restart | — | no disk access, as in the original |

**Snapshot apply order** (`Game::apply_snapshot`): (1) grapple capacity (`SetMaxGrapples`), boots enabled,
grapple latch from the snapshot; (2) the level's own start abilities on top (our stand-in for the level
Kismet, which in the original runs after the save load and sets its chapter's values — SAVE.md §5
"chapter-to-chapter carry-over", STRONG); (3) this chapter's latest checkpoint from the table; (4) on
converted levels the player reset at the checkpoint the respawn lookup returns — the first checkpoint when
the table has no entry (ABILITIES.md A-CP-4/5, CONFIRMED (src)). A load does not count as a respawn (no
`PlayerRespawned`, respawn count unchanged); what the teleport touches — trigger volumes, the checkpoint at
the spawn point (activated, no new save by A-CP-3), a kill zone — is reported with the first tick, as the
original's teleport fires its touches. Story
mode set at load (Workshop, Epilogue) is kept. Checked on a synthetic converted map and on the user's
converted `AG-ParadiseCave` (Continue at checkpoint 5).

**Capture** (`Game::capture_snapshot(previous)`): map name; the checkpoint table carried from the previous
snapshot with this chapter's latest index updated (one shared snapshot, SAVE.md Q1); abilities from the
pawn's gun and boots (`None` without the script layer); actor and Kismet entries carried only when the
previous snapshot came from the same map (entries of another map would not resolve, as in the original).

### Deliberate differences from the original

| Original quirk (SAVE.md §8) | Ours |
|---|---|
| Q2: when either progression or general fails to load, achievements, story items, the finished flag, the pointer and the flags are wiped | only the unreadable document resets (and is quarantined); Continue is still disabled for that run, as in the original |
| A missing snapshot at a story map's start triggers the same "reset all" | no reset: the level starts at its `PlayerStart` |
| Q3: achievement indices appended on every load | a set |
| Q6: Continue can open the front end after a reset | Continue needs a chapter pointer |
| Q4: checkpoint visuals saved without a name | not modelled (actor entries are the world workstream's) |
| Q1 (one snapshot for all chapters), Q7 (index-as-position lookup), Q8 (abilities as at the last checkpoint) | kept |

## Settings

Stored in `settings.json` (`asamu_game::save::Settings`), sanitized on load and on every change.

| Setting | Default | Source of the default | Applied by |
|---|---|---|---|
| `fov_degrees` | 90 (range 60–120, ours) | `FOV=90` in `DefaultSettings.ini` `[ASAMU.ASAMUSettingsManager]`, CONFIRMED (config); ≤ 0 falls back to 90 as the original does (SAVE.md §7) | `main.rs` `sync_camera`: the game's run-time FOV shifted by (setting − parameter default 90), so the story-mode zoom keeps its offset (ours; the original's behaviour with a changed FOV during zoom is UNKNOWN) |
| `mouse_sensitivity` | 1.0 × the app's radians per count (range 0.1–5, ours) | ours (the original's `PlayerInput` scaling is not ported) | `main.rs` `gather_input` |
| `invert_mouse` | off | not set in the shipped ini; ours | `gather_input` |
| `master_volume` / `music_volume` / `sfx_volume` / `voice_volume` | 1.0 / 0.8 / 0.8 / 0.8 | `DefaultSettings.ini`, CONFIRMED (config) | master → Bevy `GlobalVolume` (affects sounds started afterwards); groups → `UserSettings::group_volume` for the audio workstream |
| `fullscreen` | off | `Fullscreen=False` in `DefaultSystemSettings.ini`, CONFIRMED (config) | borderless fullscreen on the current monitor |
| `resolution` | app default window | ours (menu offers 1280×720 … 3840×2160) | window size when windowed |
| `subtitles` | on | `bSubtitlesEnabled=True` in `BaseEngine.ini`, CONFIRMED (config) | hides the HUD subtitle box when off; the audio workstream should also skip subtitle lines |

## Menus (`apps/asamu/src/ui/`)

Functional replacement of the Scaleform menus; the look is ours. Screens are described as item lists
(`menus::screen_items`, pure and unit-tested) and rebuilt when the state changes.

- **Main**: Return to game (when a game is paused) · Continue — *chapter* · New Game · Chapter select · Time
  trial · Settings · Quit, plus a progress line (chapters entered, collectibles, story items, achievements).
  Graybox (no converted data): New Game restarts the hand-made test level; Continue, chapter select and time
  trial are disabled with an explanation.
- **Chapter select**: the seven chapters in story order with collectible counts; enabled when entered
  (Workshop always) and converted.
- **Confirmation**: New Game and chapter select ask before replacing the Continue point when one exists
  (the original asks on level select with a legible save, SAVE.md §6.2, CONFIRMED (src); asking on New Game
  too is ours).
- **Time trial**: the five collectible chapters with best time (`MM:SS:hh`, SAVE.md §6.5), medal and gold
  target; available once the game is finished.
- **Settings**: steppers / toggles for every setting above, reset to defaults.
- **Pause** (Esc while playing): Resume · Restart from checkpoint · Settings · Main menu, with the chapter
  and the collectibles counter. Restart runs the death sequence and respawns at the latest checkpoint (the
  original's quick load); it is disabled in Workshop and Epilogue as in the original (SAVE.md §5).
- **Loading**: render-plan progress and gameplay load state; Cancel.
- Keys: Esc = back (pause: resume); Tab / Shift-Tab + Enter / Space through the buttons; F8 restarts a
  running time trial (the original's `TimeTrialRestart`, ABILITIES.md A-TT-2).
- HUD additions: notices (top centre), collectibles counter in collectible chapters (top right), time-trial
  stopwatch (game time in simulation ticks). A death before any checkpoint restarts it at the player
  reset 0.3 s into the death sequence, where the original runs its game-level death hook
  (ABILITIES.md A-TT-2, A-DT-2 timeline; `TickReport::respawned`).

### App flow

- `asamu` (graybox): starts in the main menu; `--no-menu`, `--walk` or `--screenshot` skip it.
- `asamu --converted DIR` (no `--level`): main menu with a converted level rendered behind it (the original's
  front end `ASAMUFrontEndMap` when converted, else the first converted chapter); saves in the user data
  directory.
- `asamu --converted DIR --level MAP`: starts straight in the map as before; saves stay in memory for that
  run (the level start still runs on them: chapter, `SnapshotLoaded` with −1 — `ui::start_direct_level`);
  Esc opens the pause menu, from which the main menu works normally.
- Loading a chapter removes the running simulation, rebuilds the render plan when the map differs (level
  entities — `LevelEntity`, `LevelBsp`, `LevelLight` and the light-mapped `LightmappedBsp` meshes — are
  despawned, `converted.rs` spawns the new plan) and loads the map's `Game` on the async pool; when ready:
  `begin_level`, `apply_snapshot`, start, capture the mouse. Plugins that prepare per-level data must follow
  `ConvertedLevel::level` changes: the NPC plugin does, and the lightmap plugin starts over when the level
  is re-planned or renamed (`reset_on_level_change`).
- `ASAMU_MENU_ACTION` (unattended checks): `new-game`, `continue`, `chapter:<Name>`, `time-trial:<Name>`;
  a confirmation it raises is answered with yes.

### Menu text

The default labels are ours. An optional user-local `<converted>/ui/strings.json`
(`{"format": "asamu-decomp/ui-strings", "format_version": 1, "strings": {key: text}}`) overrides them by key
(`chapter.<EnumName>` for chapter titles, `menu.continue`, `menu.new_game`, `menu.chapters`,
`menu.time_trial`, `menu.settings`, `menu.quit`, `menu.return`, `menu.resume`, `menu.restart`,
`menu.main_menu`, `menu.paused`, `menu.loading`, `menu.cancel`, `menu.back`). It is meant to be produced on
the user's machine from their own install's localization files; that importer step does not exist yet.
Bounded (1 MiB, 2048 entries, 256 characters each, control characters dropped).

## Integration (for the other workstreams)

Messages (`apps/asamu/src/ui.rs`; register nothing, `UiPlugin` does):

| Message | Producer | Effect |
|---|---|---|
| `ui::GameTick(TickReport)` | `main.rs` `fixed_tick` (wired) | checkpoint snapshot save, time-trial death rule, stopwatch |
| `ui::CollectibleFound { key }` | world workstream, when an `ASAMUCollectible` is touched (key = actor path relative to the map) | progression + notice + extras / achievement |
| `ui::StoryItemFound { key }` | world workstream (optional `ASAMUInteractable_Actor`, key rule SAVE.md §6.3) | progression + notice |
| `ui::AchievementEarned(Achievement)` | Kismet host (`SeqAct_UnlockASAMUAchievement`) | progression + notice (platform unlock: `Achievement::steam_id`) |
| `ui::SaveStringEdited { id, value }` | Kismet host (`SeqAct_EditOrAddSaveString`); reads use `Res<ui::Saves>` → `0.general.save_string(id)` | general save |
| `ui::GameFinished` | Kismet host (`SeqAct_SetGameFinished`) | finished flag |
| `ui::TimeTrialStart` / `ui::TimeTrialEnd` | Kismet host (`SeqAct_StartTimeTrial` / `SeqAct_EndTimeTrial`) | stopwatch, best time, medal |
| `ui::OpenMap { map }` | Kismet host (console `open <map>`; `Output::LevelTransition.map`) | `?options` are cut off; next chapter with the carried snapshot (story play only); `ASAMUFrontEndMap` ends the game: the simulation is dropped and the main menu opens (no "Return to game"). A map that is not converted leaves the game paused behind the main menu with a message |
| `ui::SnapshotLoaded { checkpoint_index }` | flow, at every story level start (`level_start`): the applied index, or −1 without a snapshot | Kismet host fires `SaveGameState_SeqEvent_SavedGameStateLoaded` |

Pure API for code outside the app: `SaveSession` hooks above; `Game::capture_snapshot`,
`Game::apply_snapshot`, `Game::chapter`, `Game::map_name`, `Game::latest_checkpoint_index`,
`save::checkpoint_saved`; Kismet / actor state goes into `Snapshot::kismet` / `Snapshot::actors` before
`on_checkpoint_saved` and is read back from the `LevelStart::snapshot` it returns.

Settings for the audio workstream: `Res<ui::UserSettings>` → `group_volume(AudioGroup::Music | Sfx | Voice)`
(master included) and `settings.subtitles`.

## Original save import (out of scope)

`import_original_saves` always returns `ImportError::NotImplemented`. Three of the four original files are
AES-256-ECB encrypted with a key compiled into the executable (SAVE.md §3.3), which the repository does not
contain. The plan is SAVE.md §9.4: read the key from the user's own executable and check it against the
published SHA-256 fingerprint, decrypt, read the tagged properties in names-as-strings mode, parse the UE3
JSON dialect, map to the documents above.

## Verification

```bash
# save model (no game data; with ASAMU_CONVERTED_DIR also the real-data chapter check below)
CARGO_TARGET_DIR=target/agents cargo test -p asamu-game --lib save
# app: menus, flow, settings, stopwatch, strings (+ the real-data checkpoint test when ASAMU_CONVERTED_DIR is set)
cargo test -p asamu
# menu flow on your own conversion (saves in a scratch directory)
ASAMU_SAVE_DIR=/tmp/asamu-saves ASAMU_MENU_ACTION=new-game \
  cargo run -p asamu -- --converted <converted dir> --exit-after 20
ASAMU_SAVE_DIR=/tmp/asamu-saves ASAMU_MENU_ACTION=continue \
  cargo run -p asamu -- --converted <converted dir> --exit-after 20
```

Checked locally on a full user-local conversion: New Game (fresh snapshot, Workshop unlocked, pointer
set), chapter select into `AG-ParadiseCave` after the confirmation, Continue from a snapshot at checkpoint
5 (player reset there), and a checkpoint activated in a tick written as the snapshot (`Sanctuary: 1`).

Independent re-check (verification pass, a levels-only conversion of the twelve maps in a scratch
directory, deleted afterwards): the same New Game / chapter select / Continue runs and an unreadable
`general.json` set aside at start (Continue then off) behaved as described; the real-data test
`save::tests::real_data_chapters_titles_and_snapshot_resets` found every chapter map's own
`WorldInfo.Title` to name its chapter (7/7; the front end is not one), a Continue table entry lands on that
checkpoint's spawn point in every chapter (horizontal error 0.0 UU) without counting a respawn, and the
first checkpoint — where New Game and chapter select put the player, as the original's save load does
(A-CP-4/5) — lies 0–69 UU from the `PlayerStart` in six chapters and 316 UU in `AG-StarHaven`
(horizontal distance; CONFIRMED (map) as a property of the shipped data).

## Open items

- Producers for the collectible, story-item, achievement, flag, finished, time-trial and `open` messages
  (world / Kismet workstreams).
- Live per-group audio mixing and subtitle suppression in the audio workstream.
- `ui/strings.json` from the user's localization files (importer).
- Gamepad navigation; key rebinding; the original's extras toggles (beam colour, goat, Midas, parkour) are
  tracked as unlocked but have no effect yet.
- Behavioural checks of SAVE.md Q6/Q8 against the original.
- The index the original passes to the "save loaded" event when the table has no entry for the chapter
  (we send −1, the value of a failed load) is not traced (TENTATIVE); all shipped instances use the default
  `Index` −1 ("any load"), so only Kismet that reads the index would notice.
- Continue inside the same map applies the level's start abilities after the snapshot's (stand-in for the
  level Kismet). For chapter transitions that is the documented order (STRONG); within one map a capacity
  raised mid-level (e.g. Sanctuary's second `SetMaxGrapples`) is overridden until the Kismet host, with its
  saved event state, replaces the stand-in.
