# Status

_Session journal. Newest entry first. Each entry: what was done, what is true now, what to do next._

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
