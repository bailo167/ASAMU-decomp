# Playing with your own copy

ASAMU-decomp is an unofficial, work-in-progress recreation of *A Story About My Uncle*'s engine in Rust and
Bevy. **It contains no game data.** You need your own copy of the game (Steam App 278360). The importer reads
your install (read-only), converts it into a folder on your computer, and the runtime plays from that folder.
Converted data is copyrighted game data: keep it on your machine and never share it.

This is pre-alpha software. Read [What works today](#what-works-today) before you expect a full playthrough.

## What you need

| | |
|---|---|
| The game | Installed through Steam. The importer is verified against the **macOS build 1822049** (depot 278362). Windows and Linux installs of the game are detected but **not verified** (see [Other builds of the game](#other-builds-of-the-game)). |
| Disk | About **2.2 GB** for the converted data. Re-converting needs room for one more copy of the largest stage for a moment (textures, 1.3 GB: the old copy is kept until the new one is complete). `--package-cache` adds about 1.5 GB. |
| Memory | About 3 GB free while importing (peak resident memory measured at 2.7 GB). |
| GPU | Anything Bevy 0.20 supports: Metal (macOS), Vulkan or DirectX 12 (Windows), Vulkan (Linux). |
| OS | Windows 10/11 x86_64, Linux x86_64 (glibc 2.35 or newer), macOS 11 or newer (Apple silicon or Intel). |

## 1. Get the programs

Download the zip for your system from the project's GitHub releases page (`asamu-decomp-<version>-<target>.zip`)
and unzip it anywhere. It holds two programs, `asamu` (the game runtime) and `asamu-import` (the importer),
plus `LICENSE` and this file. `SHA256SUMS.txt` on the release page lists the zip hashes.

You can also build both from source: see `docs/BUILDING.md` in the source repository
(<https://github.com/bailo167/ASAMU-decomp>).

## 2. Import your copy

Open a terminal in the unzipped folder and run:

```sh
./asamu-import all          # Windows: .\asamu-import.exe all
```

The importer:

1. **Finds the game** through Steam (it reads Steam's library list and the game's app manifest). If the game
   lives somewhere else, point at it: `--original "/path/to/A Story About My Uncle"` or the environment
   variable `ASAMU_ORIGINAL_DIR`. On macOS that is the folder that contains `A Story About My Uncle.app`.
2. **Checks the files it needs** (42 packages and 3 texture caches) against the published inventory of the
   verified build, by size and SHA-256. Missing files stop the import; different files only produce a warning.
3. **Converts** everything, stage by stage:

| Stage | What it writes | Size (Mac build) |
|---|---|---|
| `textures` | DDS textures + manifest | 7,702 files, 1.29 GB |
| `meshes` | glTF static meshes with collision + manifest | 1,765 files, 66 MB |
| `materials` | approximate material descriptions | 1 file, 2.5 MB |
| `levels` | scene descriptions and BSP geometry of all 12 maps | 37 files, 73 MB |
| `audio` | Ogg sounds, sound cues, subtitles, ambient sounds | 774 files, 59 MB |
| `matinee` | scripted movement / cutscene tracks | 10 files, 0.9 MB |
| `skeletal` | glTF characters with skins and animations | 55 files, 86 MB |
| `kismet` | the level scripts of every map, for the runtime's Kismet interpreter | 13 files, 3.0 MB |
| `lightmaps` | the baked lighting of every map (approximate HDR atlases) | 2,649 files, 618 MB |
| `particles` | particle systems and where the maps place them | 13 files, 5.6 MB |
| `decals` | level decals as projected meshes, with their masks | 50 files, 77 MB |
| `localization` | menu text, subtitles and credits in every language of your install | 16 files, 1.4 MB |

Together that is 13,091 files and 2.3 GB. A full import takes well under a minute on a recent Apple-silicon Mac
(44 s measured for all twelve stages with other programs running, including 1.5 s to hash the 1.1 GB of game
files). It prints one line per stage and a summary at the end. A converter that is not part of your build is
listed as `unavailable`.

The `localization` stage also reads two small Steam files next to the install when they exist (the game's app
manifest for your default language, and Steam's cached list of achievement names). Nothing else outside the
install is read.

### Where the data goes

| System | Default folder |
|---|---|
| macOS | `~/Library/Application Support/asamu-decomp/converted` |
| Windows | `%LOCALAPPDATA%\asamu-decomp\converted` |
| Linux | `$XDG_DATA_HOME/asamu-decomp/converted`, else `~/.local/share/asamu-decomp/converted` |

Choose another folder with `--out DIR` (for example `./asamu-import --out /path/to/asamu-data all`). Use a new
or empty folder, or one you imported into before: the importer replaces its stage folders as a whole, so it
refuses a folder that already holds other files, and it refuses to write inside the game install. The folder
also holds two small files about the import:
`asamu-import-run.json` (importer version, game build, per-stage status and output hashes; no personal paths,
error messages included, so it is safe to attach to a bug report) and `asamu-import-state.json` (timings and a file-hash cache).

### Running it again

Running `asamu-import all` again only redoes what is out of date: a new importer version, changed game files,
a stage that failed or was interrupted, or a stage folder in which files were deleted or added. When nothing
changed it finishes in well under a second. A file edited in place without changing its size is not noticed;
`--force` converts everything again. Useful options (`asamu-import all --help` lists them all):

| Option | Effect |
|---|---|
| `--plan` | Show the file check and what would run; write nothing. |
| `--force` | Convert every stage again. |
| `--only textures,levels` / `--skip audio` | Choose stages. |
| `--lang DEU` | Subtitle language (the shipped audio is English only). |
| `--png` | Also write PNG previews of the textures. |
| `--no-verify` | Skip the file check (converts whatever is there). |
| `--package-cache` | Keep decompressed packages (about 1.5 GB) so forced re-imports finish about 3 seconds sooner. |

An interrupted import is safe: each stage is written to a temporary folder and only replaces the previous
output when it is complete. Just run the command again.

## 3. Play

```sh
./asamu --level AG-Workshop          # Windows: .\asamu.exe --level AG-Workshop
```

`--level` loads a converted map from the default folder (or from `--converted DIR`, or `ASAMU_CONVERTED_DIR`).
`./asamu --converted DIR` without `--level` starts at the main menu (New Game, Continue, chapter select; saves
are kept in the user data folder). Without either option the runtime starts its built-in graybox test level
(through the same menu), which needs no game data.

The story order of the maps is: `AG-Workshop`, `AG-ParadiseCave`, `AG-BeautifulCity`, `AG-Darkcave`,
`AG-StarHaven`, `AG-IceCave` (with `TheCore`), `AG-Epilogue`.

**Controls** follow the original bindings. Keyboard and mouse: WASD move, mouse look, Space jump (in the air it
fires the rocket boots when you have them), left Shift sprint, left mouse button grapple (release the button to
let go), hold the right mouse button to power jump, E or Enter use, F7 respawn at the last checkpoint. Gamepad:
left stick move, right stick look, A jump, right trigger grapple, right shoulder power jump, left shoulder sprint,
Back restart from the checkpoint, Start pause. Click into the window to capture the mouse; Esc pauses and
releases it.

Developer keys (not in the original): F1 shows a read-out of the simulation's state and the key list, R respawn,
F2 story mode, F3 grapple count, F4 rocket boots, F6 attractor pad, F9 start/stop recording a movement trace,
F10 show the level objects' volumes.

Other options: `--fly` (free camera, no collision), `--all-sublevels`, `--light-scale F`, `--no-shadows`,
`--no-fog`, `--normal-maps`, `--debug-info` (start with the F1 read-out shown), `--help` for the full list.

## What works today

This describes the state when this guide was last updated; the progress table in the project README is the live
status. "Original values" means numbers recovered from the game's own data, each with its source. Nothing in
this table has been measured against recordings of the original game yet: that work has started, and until it
is done, expect the feel to differ in places.

| Area | Status |
|---|---|
| Movement | Walking, jumping (with the original's jump-release damping), sprinting, falling and landing on a port of the original engine's pawn physics, with the original values. |
| Grapple, power jump, rocket boots | Ported from the original's script behaviour with the original values, including the limited grapple charges, recharge crystals and every release rule. |
| Levels | All seven story maps load from your converted data and play with triangle collision, checkpoints, kill zones, moving platforms on their original paths, and the grapple-reactive objects (recharge crystals, attractor pads, falling rocks). `docs/PARITY.md` in the repository lists every known difference. |
| Story scripting (Kismet), cutscenes, level transitions | The levels' own scripts run: triggers, abilities granted by the story, scripted movers, cutscene cameras, level streaming and the exit of each level into the next. The whole chain from the Workshop to the Epilogue has been followed by an automated run that jumps to each exit; nobody has played it through by hand yet, so you may find a route that is blocked. |
| Lighting and effects | The original's baked lightmaps, approximate materials, particles, decals, water, height fog, colour grading and bloom. An approximation: colour and brightness can differ visibly from the original, and a few materials still show as flat placeholders. |
| Characters | Maddie, the villagers and the Dark Cave worm with their animations; collectibles and story items; the first-person hand. |
| Sound, music, narration, subtitles | Sound cues, ambient sound, narration with subtitles and the adaptive music play. |
| Menus, saving, languages | Main menu, chapter select, pause and settings; saves and progression in the recreation's own format; time trial with medals; the game's text in the 14 languages of your install (a functional replacement of the original's menus, not a visual copy; some characters outside basic Latin may not display yet). |

## Troubleshooting

| Problem | What to do |
|---|---|
| `no Steam installation found` / `App ID 278360 ... is not installed` | Install the game through Steam, or pass `--original` / set `ASAMU_ORIGINAL_DIR` to the game folder. |
| `inventoried input file(s) are missing` | The install is incomplete. In Steam: game Properties > Installed Files > Verify integrity of game files. `--no-verify` converts what is there. |
| `warning: N of 45 input files differ` | You have another build of the game. The import continues, but only build 1822049 is verified. If something breaks, open an issue and attach `asamu-import-run.json`. |
| A stage shows `FAILED` | Run `asamu-import all` again; it retries only failed stages. `--only <stage>` reruns one stage. Report persistent failures with `asamu-import-run.json`. |
| `refusing to write inside the game install` (or inside the source repository) | Choose an output folder elsewhere with `--out`. |
| `... already holds other files` | The `--out` folder contains files the importer did not write. Choose a new or empty folder. |
| The runtime shows the gray test level | It did not find converted data. Pass `--converted DIR --level AG-Workshop`, or run the import first. |
| `no converted levels in ...` | The `levels` stage has not run in that folder: `asamu-import all` (or `--only levels`). |
| macOS: "cannot be opened because the developer cannot be verified" | The binaries are not code-signed. Right-click > Open once, or run `xattr -d com.apple.quarantine asamu asamu-import` in the unzipped folder. |
| Windows: "Windows protected your PC" | The binaries are not signed. Click "More info" > "Run anyway". |
| Linux: the runtime fails to start | Install the ALSA, udev and Vulkan runtime libraries (`libasound2`, `libudev1`, a Vulkan driver such as `mesa-vulkan-drivers`). |
| Out of disk space during an import | Free about 2.5 GB, or delete `<out>/.package-cache` if you used `--package-cache`. Rerun; finished stages are kept. |
| Windows: a stage fails with "moving the old ... aside" | Another program (often the runtime itself) has files of that folder open. Close it and rerun. |

## Other builds of the game

The importer reads the game's cooked packages. It has been checked against the macOS build only. A Windows or
Linux Steam install (cooked folder `CookedPC`) is located and converted, but there is no published inventory for
it and its packages may differ, so expect problems and please report what you find. If you own the game, Valve's
SteamCMD tool can also download the macOS version of it with your own account
(`+@sSteamCmdForcePlatformType macos +app_update 278360`); point `--original` at the downloaded folder.

## Removing everything

Delete the unzipped folder and the `asamu-decomp` folder in your user data folder (the parent of the
converted-data folder listed above; it also holds the runtime's `saves/` and `settings.json`), plus any folder you
chose with `--out`. The importer never changes the game install, so there is nothing to undo there.
