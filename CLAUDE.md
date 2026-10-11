# CLAUDE.md — operating contract for agents working on ASAMU-decomp

ASAMU-decomp is a **clean, behaviourally compatible reimplementation** of
*A Story About My Uncle* (Gone North Games / Coffee Stain Studios, 2014) in
**Rust + Bevy**, driven by reverse engineering of a legitimately owned
original installation.

It is an **engine recreation** (universal-modder "Pattern 4 — reimplement").
It is **not** a passthrough mod, a two-process bridge, an injected mod, a
wrapper around the original executable, a byte-matching decompilation, or an
attempt to make the obsolete Mac executable run.

```
legitimate original install ──► static RE / UE3 package parsing / behavioural observation
                            ──► ASAMU importer/converter (runs on the user's machine)
                            ──► user-local converted data ──► native Rust/Bevy runtime
```

## Session start checklist (do this every time, in order)

1. `git pull --ff-only`
2. Read this file.
3. Read `docs/STATUS.md` (current state, what was last done, next task).
4. Read `docs/reverse-engineering/*` — **use existing evidence instead of restarting discovery.**
5. Inspect `progress/progress.toml`.
6. `git status` — understand any uncommitted work before touching anything.
7. Run the tests: `cargo test --workspace` (and `cargo clippy --workspace --all-targets -- -D warnings`).
8. Choose the **highest-value unblocked task** (STATUS.md lists candidates).
9. Implement / research it in small verified steps.
10. Update evidence docs (with confidence labels) as facts are confirmed.
11. Update `progress/progress.toml` honestly.
12. Regenerate the matrix: `cargo run -p progress-gen`.
13. Test: `cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace && cargo run -p progress-gen -- --check`.
14. Hygiene: `cargo run -p repo-hygiene -- --staged` after staging; also `git status` and `git diff --cached --stat`.
15. Commit (small, logical, conventional-commit messages).
16. Push to `main`. Then continue with the next task — do not stop at the bootstrap.

Commit early. A previous session lost all of its work by researching for a
whole session without committing. Knowledge that is not committed does not exist.

## Original game hygiene (the repository is PUBLIC)

Never commit, paste or upload:

- the original executable or dylibs, any `.u`, `.upk`, `.asamu`, `.tfc`, `.int`, shader caches
- textures, meshes, animations, music, voice, movies, Scaleform/GFx movies, whole map payloads
- giant `strings`/`nm` dumps, complete Ghidra output, bulk decompiled functions, verbatim proprietary code
- decompressed packages or any byte-for-byte extracted object payload
- **UnrealScript source text.** The cooked packages contain `TextBuffer` `ScriptText` objects holding the original
  `.uc` source of all 583 ASAMU/UTGame classes (and engine classes in the `.u` files). It may be read locally
  (under ignored `research/`) to understand behaviour, but never extracted into, quoted in, paraphrased
  line-by-line in, or committed to this repository. Describe behaviour in our own words; implement independently.

Safe to publish: hashes, sizes, counts, format structures and offsets, class/function/symbol names
needed to explain architecture, behavioural descriptions, sanitized inventories, evidence summaries,
independently written Rust, tests, importer code.

**Showcase images** are the one narrow exception to "no game content": a handful of curated screenshots and
short clips of **our runtime** (never the original's screen, never an asset shown on its own, no audio) may live
in `docs/images/` and nowhere else; `repo-hygiene` enforces the folder, formats and size. The rules are in
`docs/LEGAL.md` ("Screenshots and clips"). Screenshots taken while developing stay under ignored
`research/local/` (the app refuses to write them anywhere else in the repository); copy a chosen few to
`docs/images/` by hand.

Rules of thumb:

- Never modify, patch, move or delete anything inside the Steam install. Read only.
- Local RE state lives under the git-ignored `research/local/`, `research/ghidra/`,
  `research/decompressed/`, `research/symbol-dumps/` (see `.gitignore`).
- Tests that need the original data read `ASAMU_ORIGINAL_DIR` (or the Steam locator) and **skip** when
  it is unavailable. Synthetic fixtures are written by us, byte by byte, in test code.
- Before every push: `git status`, `git diff --cached --stat`, `cargo run -p repo-hygiene -- --staged`.
- Don't paste the user's home path into docs; write `~/Library/Application Support/Steam/...`.

## Locating the original install

- `tools/asamu-locate` finds the game through Steam: it reads
  `<Steam>/steamapps/libraryfolders.vdf`, finds the library containing App ID **278360**, and
  reads `appmanifest_278360.acf` for `installdir`. Default Steam roots:
  - macOS: `~/Library/Application Support/Steam`
  - Linux: `~/.steam/steam`, `~/.local/share/Steam`
  - Windows: `C:\Program Files (x86)\Steam` (plus registry-less fallbacks)
- Override with `ASAMU_ORIGINAL_DIR=/path/to/A Story About My Uncle`.
- Mac layout: `A Story About My Uncle.app/Contents/{MacOS/ASAMU, Resources/{ASAMU,Engine}}`.
  Windows layout (STRONG, from the TOC files shipped in the Mac depot): `Binaries/Win32/ASAMU-Win32-Shipping.exe`,
  `ASAMU/CookedPC`. The locator also accepts `ASAMU.exe` / `CookedPCConsole` variants.
- Never hard-code a username or home path.

### Environment variables

| Variable | Meaning |
|---|---|
| `ASAMU_ORIGINAL_DIR` | Root of the original install (the folder that contains `A Story About My Uncle.app`, or a Windows/Linux install root). Overrides Steam discovery. |
| `ASAMU_STEAM_ROOT` | Override the Steam root used for discovery. |
| `ASAMU_RESEARCH_DIR` | Where local (ignored) RE output goes. Defaults to `research/local`. |
| `GHIDRA_INSTALL_DIR` | Ghidra installation used by headless scripts. |

## Where things are (quick map)

- ASAMU script classes: `Startup.upk` → top-level package `asamu` (172 classes); stock UDK game script:
  `Startup.upk` → `UTGame`. See `docs/reverse-engineering/SCRIPT_ANALYSIS.md`.
- Player pawn `ASAMUPawn` → `UTPawn` → `UDKPawn`: movement runs on native pawn physics
  (`docs/reverse-engineering/NATIVE_PHYSICS.md`). Grapple = `GrappleGun` (`UDKWeapon`) +
  `ASAMUPlayerController` state `Grappling`.
- Tools: `asamu-inspect` (packages), `asamu-symbols` (executable), `asamu-locate`/`asamu-inventory` (install),
  `tools/ghidra-scripts` (Ghidra headless).

## Ghidra strategy

- Location: `$GHIDRA_INSTALL_DIR`, else `brew --prefix ghidra` (`/opt/homebrew/Caskroom/ghidra/*/ghidra_*`),
  else `~/ghidra*`. Ghidra needs a JDK 21+ (`brew install --cask temurin` or `openjdk@21`).
- Project lives in `research/ghidra/` (ignored). Import with original symbols; the Mach-O is
  **not stripped**, so do not let renames overwrite original names. Prefer headless
  (`analyzeHeadless research/ghidra ASAMU -import <binary> -postScript ...`) with scripts that write
  sanitized summaries (counts, names, call relationships) rather than decompiled bodies.
- Focus: ASAMU-specific natives, the script VM entry points the gameplay script relies on, and
  physics paths reached by player/grapple script. Avoid generic UE3 rendering/containers/networking.

## Methodology (universal-modder, Pattern 4)

- **Read the real thing, don't guess.** Symbols → xrefs → functions; package bytes → structures.
- **Reproducible static analysis**: every finding cites a tool/command that regenerates it.
- **Behavioural oracles**: the original game (later, on Windows) records traces
  (time, input, position, velocity, camera, grapple anchor/state, grounded) that our deterministic
  simulation replays. Matching build ≠ behavioural parity; we care about behavioural parity.
- **Evidence journal**: findings go into `docs/reverse-engineering/*.md` with confidence labels;
  `docs/STATUS.md` records session-level progress.
- **Separate original-game evidence from runtime tests.** Label which side every finding came from.
- **Converters run on the user's files**; the repository never ships game data.
- **Cap attempts**: ~3 identical failures → change approach and write down why.

## Confidence levels (every substantive RE claim carries one)

| Level | Meaning |
|---|---|
| **CONFIRMED** | Direct, repeatable evidence (bytes parsed, symbol present, tool output reproducible). |
| **STRONG** | Multiple independent supporting indicators. |
| **TENTATIVE** | Plausible but weak or incomplete evidence. |
| **UNKNOWN** | Insufficient evidence. |

When evidence disproves an earlier claim, update the doc and say what changed.
Never invent numeric constants. A gameplay constant is recorded only with its source
(script default property, native code, config, or measured trace).

## Progress tracking — no fake progress

`progress/progress.toml` is canonical; `progress/progress.svg` and the README table are generated.

| Status | Meaning |
|---|---|
| `not_started` | No meaningful work. |
| `investigating` | Active reverse engineering. |
| `partial` | Real progress, materially incomplete. |
| `implemented` | Our implementation/tooling exists and passes appropriate tests; parity not proven. For pure-RE items: documented with reproducible tooling. |
| `verified` | Evidence shows the implementation or format understanding agrees with the original for the tracked scope (e.g. parses every real package with structural cross-checks). |
| `blocked` | Specific documented blocker (`blocker = "..."` required). |

Never mark a stub implemented. Never mark something verified because it compiles.
`implemented`/`verified` items require an `evidence` field; the generator rejects them otherwise.

## Architecture rules

- `asamu-core` shared math/types/config · `asamu-player` movement/grapple/camera (deterministic,
  render-free) · `asamu-world` levels/triggers/checkpoints/platforms · `asamu-assets` runtime assets ·
  `asamu-ue3` original data parsing · `asamu-game` high-level state · `apps/asamu` the Bevy executable ·
  `asamu-sandbox` the experimental Sandbox model (ours, never evidence of parity; `docs/SANDBOX.md`).
- **Classic comes first.** The faithful game (original parameters, tick order, saves, the parity tooling) must
  behave bit-identically whether or not experimental code such as the Sandbox is compiled in. Experiments wrap
  the simulation from outside; they never edit `Game::tick`, the loaders or the input path, and
  `crates/asamu-sandbox/tests/{classic_guard,boundary}.rs` must keep passing.
- Gameplay logic never knows about Steam paths or UE3 serialization.
- Physics/gameplay must be testable without rendering; prefer pure step functions with fixed timesteps.
- Cross-platform is a requirement: Windows x86_64, Linux x86_64, macOS arm64 (+ x86_64 where practical).
  No Intel-only or Mac-only assumptions; no `unsafe` (workspace forbids it).

## Code quality for parsers

Hostile-input discipline: checked arithmetic, bounds checks, explicit error types, no blind offsets or
serialized lengths, no `unwrap()` in parser code, no panics on malformed input. Fuzz-style tests on
truncated/corrupted synthetic data.

## Hard gates — ask the user first

Authentication needing a human, destructive actions, credential/legal/account actions, or a major
mutually exclusive architecture decision. Never ask for passwords, tokens or keys.
