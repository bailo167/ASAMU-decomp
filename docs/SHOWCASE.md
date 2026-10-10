# Showcase copy

Ready-to-paste descriptions of ASAMU-decomp for release notes, project channels and posts. Everything here is
meant to stay true as written: when the project's state changes, change this file with it. The live numbers are
in the [README](../README.md#progress) and [`progress/progress.toml`](../progress/progress.toml).

Repository: <https://github.com/bailo167/ASAMU-decomp>

Ground rules when you post about the project:

- Say **"engine recreation"** or **"reimplementation"**. It is not a decompilation of the original source, a mod,
  or a port of the original executable.
- Say that it **needs the player's own copy** of the game and ships no game data.
- Say **pre-alpha**, and do not claim it plays like the original: that has not been measured yet.
- Screenshots and clips must show the recreation, not the original game, and must not be bundled with converted
  data.

## One line

An open-source Rust/Bevy engine recreation of *A Story About My Uncle* that plays the original levels from your
own copy of the game.

## Short (about 100 words)

ASAMU-decomp is an open-source, from-scratch recreation of the engine behind *A Story About My Uncle*, written in
Rust on Bevy. It ships no game data: an importer converts your own Steam copy locally, and the new runtime builds
for Windows, Linux and macOS (played so far only on macOS). All seven story levels load with their original
scripting, collision, baked lighting, characters and audio, behind stand-in menus, and the grapple, power jump
and rocket boots are ported from values recovered from the game's own files. It is pre-alpha: nobody has played it start to finish yet, and how closely
it matches the original's feel is still being measured against recordings of the real game.

## Technical (about 250 words)

ASAMU-decomp reimplements a 2014 Unreal Engine 3 game as a clean Rust codebase, using the original only as
read-only evidence.

The importer contains a UE3 package reader written from scratch for the game's package version (868), including
LZO chunk decompression, the object and property system, a decoder for all 12,801 compiled UnrealScript scripts,
and converters for static and skeletal meshes, animations, textures, materials, lightmaps, sound cue graphs,
particle systems, Kismet graphs and Matinee tracks. All 42 shipped packages parse; where it can be proven, decoding
is exact (every static mesh re-encodes byte for byte). Output is glTF, DDS, Ogg and JSON, plus a few raw buffers
of our own, written to the user's own disk.

The runtime never sees UE3 data. Its simulation core is deterministic and runs without a renderer: a port of the
engine's pawn physics specified from the unstripped macOS executable, the game's grapple and abilities, and an
interpreter for Kismet, the visual scripting that drives every level (all 97 kinds of sequence object the maps
use). Bevy provides rendering, audio, UI and input on top.

Every gameplay constant carries its source (class default, config file or native code), and tests fail if a value
drifts. Parity with the original is treated as something to measure, not assume: a read-only recorder captures
per-frame player state from the real game (the Windows recorder made the first recordings; the macOS one has
not recorded yet), and a replay tool runs the same inputs through the recreation and reports the first
divergence.

CI builds on Windows, Linux and macOS on every push and runs the library and tool tests there, and a hygiene
check keeps game data out of the repository.

## Feature list

- All seven story levels of the original load from your own copy: geometry, placements, streamed sub-levels
- Pawn physics ported from the original engine: walking, falling, stepping, air control, releasable jump, sprint
- Grapple with the original's targeting, range, pull, release rules and limited charges; power jump; rocket boots
- Kismet and Matinee interpreter running the levels' own scripts: triggers, movers, cutscenes, checkpoints,
  level transitions
- Triangle collision against the converted level geometry, moving platforms, kill and trigger volumes
- Baked lightmaps, approximate materials, skinned characters and animations, particles, decals, fog, colour
  grading and bloom
- Maddie, the villagers, the Dark Cave worm, collectibles and story items; first-person hand with its animations
- Sound cues, ambient sound, narration with subtitles, adaptive music
- Main menu, chapter select, pause, settings, saves and progression, time trial with medals (unlocked by finishing
  the game)
- Text in the game's 14 languages; keyboard and mouse with the original bindings (gamepad support is partial:
  look and jump work, movement and grapple are not connected yet)
- One-command importer (`asamu-import all`): about a minute, 2.3 GB, resumable, verifies your install first
- Builds on Windows, Linux and macOS (Apple silicon) in CI, with the library and tool tests run on each

## Current limitations

- Pre-alpha. No packaged release yet; build from source.
- Parity with the original's movement and grapple is not measured yet; recordings of the original are in progress.
- No start-to-finish human playthrough. The level-to-level chain is proven by an automated run that jumps to each
  exit trigger.
- Rendering is an approximation of the original's materials and lighting.
- The original's Scaleform menus and HUD are replaced by functional stand-ins.
- Verified against one build of the game only (macOS Steam build 1822049).
- Props and platforms that start hidden and are shown later by a level's script are not drawn yet.
- Saves from the original cannot be imported.

## Facts and figures

Use these only as stated; each comes from the evidence documents in
[`docs/reverse-engineering/`](reverse-engineering), from [`BUILDING.md`](BUILDING.md) (importer output) or from
the CI workflow (platforms).

| Fact | Value |
|---|---|
| Original engine | Unreal Engine 3 (UDK), package file version 868 |
| Packages parsed | 42 of 42 (38 are compressed; all 38 decompress) |
| Game script classes recovered (package `asamu`, found inside `Startup.upk`) | 172 |
| Compiled scripts decoded (12,511 functions, 211 states, 79 classes with class-level code) | 12,801 |
| Kismet sequence-object classes used by the maps, all interpreted | 97 (3,207 placed objects in 12 maps) |
| Static meshes decoded and re-encoded byte for byte | 1,512 |
| Placed actors in the 12 maps | 30,284 |
| Symbols in the unstripped macOS executable | about 135,000 |
| Importer output for the verified build | 13,091 files, 2.3 GB (2.1 GiB) |
| Platforms built in CI (library and tool tests run on each) | Windows x86_64, Linux x86_64, macOS arm64 (+ macOS x86_64 compile check) |
| Licence | MIT or Apache-2.0 (code only; the game belongs to its owners) |

## Images

Curated screenshots of the recreation live in [`docs/images/`](images) and may be reused with a link to the
repository. They show the recreation rendering data converted locally from an owned copy of the game; the game's
art belongs to its owners.

## Legal line

Unofficial project, not affiliated with Gone North Games or Coffee Stain Studios. Contains no original game
assets, code or data; you need your own copy of *A Story About My Uncle*.
