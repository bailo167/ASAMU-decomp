# Legal scope

ASAMU-decomp is an **unofficial**, non-commercial interoperability and preservation project. It is not affiliated
with, endorsed by, or connected to Gone North Games or Coffee Stain Studios. *A Story About My Uncle* and all of
its assets are the property of their respective owners.

## What this repository contains

- Independently written Rust code, tooling and tests.
- Documentation of file formats and program structure written in our own words.
- Sanitized metadata: file hashes, sizes, counts, and names (classes, functions, symbols, packages) where needed to
  explain structure.
- Synthetic test fixtures created by us.
- A small number of curated screenshots and short clips of **our runtime** in `docs/images/` (see below).

## What this repository never contains

- The original executable, libraries, packages (`.u`, `.upk`), maps (`.asamu`), texture caches (`.tfc`),
  localization files, shader caches, movies or any other game file.
- Textures, meshes, animations, music, voice, video or other extracted assets.
- Decompiled or disassembled code in bulk, complete Ghidra/IDA databases, or verbatim proprietary source.
- Large raw dumps (`strings`, `nm`, object dumps).
- Keys, credentials or anything that circumvents access control or DRM.

## Screenshots and clips

`docs/images/` holds a few screenshots and short clips that show the recreation running, so that a visitor can see
what the project does. They are pictures of our own runtime rendering data that the maintainer converted locally
from a legitimately owned copy. The game's art that is visible in them belongs to its owners; the images are
included only to illustrate this project, and they are not a substitute for the game.

The rules:

- Only rendered frames of our runtime. Never the original game's own screen, and never an extracted asset shown
  on its own (a texture, a model turntable, a sprite sheet, a map overview that could stand in for the level).
- Only in `docs/images/`, a handful of files, each small. `tools/repo-hygiene` refuses image files anywhere else
  in the repository and refuses texture, model and video container formats everywhere.
- No audio. No narration text or subtitles beyond what happens to be on screen.
- If a rights holder asks for an image to be removed, it is removed.

## How users get game data

Users must own a legitimate copy. The importer reads their installation locally (read-only) and writes converted
data to a user-local directory. Converted data must not be redistributed.

## Enforcement

- `.gitignore` excludes original and RE payloads.
- `tools/repo-hygiene` (run in CI and before every push) rejects forbidden extensions, UE3/Mach-O/PE magic bytes,
  oversized files, image files outside `docs/images/`, private absolute paths, and flags decompiler fingerprints.
