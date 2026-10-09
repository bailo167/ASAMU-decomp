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

## What this repository never contains

- The original executable, libraries, packages (`.u`, `.upk`), maps (`.asamu`), texture caches (`.tfc`),
  localization files, shader caches, movies or any other game file.
- Textures, meshes, animations, music, voice, video or other extracted assets.
- Decompiled or disassembled code in bulk, complete Ghidra/IDA databases, or verbatim proprietary source.
- Large raw dumps (`strings`, `nm`, object dumps).
- Keys, credentials or anything that circumvents access control or DRM.

## How users get game data

Users must own a legitimate copy. The importer reads their installation locally (read-only) and writes converted
data to a user-local directory. Converted data must not be redistributed.

## Enforcement

- `.gitignore` excludes original and RE payloads.
- `tools/repo-hygiene` (run in CI and before every push) rejects forbidden extensions, UE3/Mach-O/PE magic bytes,
  oversized files, private absolute paths, and flags decompiler fingerprints.
