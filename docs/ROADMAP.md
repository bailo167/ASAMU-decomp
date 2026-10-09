# Roadmap

Milestone status lives in `progress/progress.toml`; this file explains each milestone's exit criteria.

| ID | Milestone | Exit criteria |
|---|---|---|
| M0 | Repository + analysis infrastructure | Workspace, hygiene, docs, tracker, generator, CI green on all three OSes |
| M1 | Original installation inventory | `asamu-locate` + `asamu-inventory` produce a reproducible sanitized inventory |
| M2 | Binary/symbol map | Reproducible symbol classifier; ASAMU-specific natives enumerated; Ghidra project with original names |
| M3 | UE3 package reader | Summary, names, imports, exports, compression parsed for **every** shipped package with structural cross-checks |
| M4 | Import/export/object recovery | Object graph resolution, tagged properties, class defaults |
| M5 | Kismet/script structural recovery | ASAMU class/function/property lists; per-level Kismet graphs (local, sanitized) |
| M6 | Original-data importer | `asamu-import` converts meshes, textures and one level to user-local data |
| M7 | Rust/Bevy player controller | Walk/jump/air control in a deterministic sim with values sourced from the original |
| M8 | Grapple behavioural parity | Grapple target → attach → swing → release → land, matched against original traces |
| M9 | First original level imported/playable | One original level converted locally and traversable in the Bevy runtime |
| M10 | Core gameplay complete | All movement abilities, checkpoints, death/reset, moving platforms |
| M11 | Full game/story path | Every level, story sequencing, narration, save/progression |
| M12 | Behavioural/regression parity pass | Trace-replay suite over representative routes in every level |
| M13 | Windows/Linux/macOS release | Packaged builds for all targets; importer UX polished |

## First gameplay slice

`walk → jump → grapple target → grapple → swing/accelerate → release → preserve momentum → land`

Proven in a deterministic simulation first (no rendering), then wired into Bevy.
