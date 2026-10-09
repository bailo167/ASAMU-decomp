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

## Execution plan to 100% (living; update as phases land)

Each phase runs as parallel workstreams with an adversarial verification pass; results are merged in small
commits with tracker updates.

| Phase | Workstreams | Unlocks |
|---|---|---|
| 1 ✅ | UE3 reader + LZO, symbol map, locator/inventory, native physics spec, deterministic player slice | M1, M2, M3 |
| 2 (running) | Object payload decoder (tagged properties, UClass/UFunction/UProperty), grapple + abilities behaviour specs, native physics port, class defaults, Kismet graphs | M4, M5, real constants |
| 2b (running) | Binary RE completion (load commands, RTTI, strings), save/progression format | Binary RE, Save |
| 3 | Gameplay port: real defaults wired into `Ue3PawnMovement`; grapple (`GrappleGun` + `Grappling` state); power jump; rocket boots; sprint/story speed; camera/FOV/eye height; falling damage; kill zones, death, checkpoints; input layer per `DefaultInput.ini` | M7, M8 (pending traces) |
| 4 | Asset importer: Texture2D + TFC (DXT → PNG/KTX2), static meshes (+LODs, sections, materials), collision (BSP `Model`, static-mesh collision, volumes), level actor placement (StaticMeshActor, InterpActor, lights, PlayerStart, triggers) → user-local converted format; Bevy loaders | M6, M9 |
| 5 | Kismet runtime subset (events/actions/conditions/variables used by ASAMU), Matinee/interp movers, level streaming, story sequencing, narration + subtitles, audio (SoundNodeWave/SoundCue), music manager | M10 |
| 6 | Skeletal meshes/animations (hands, Maddie, worm, villagers), NPC behaviour, UI (Bevy replacements for Scaleform menus/HUD), save/progression, time trial, collectibles/achievements | M11 |
| 7 | Behavioural parity: trace capture on the original (Windows) + replay suite; regression traces per level | M12 |
| 8 | Release packaging for Windows/Linux/macOS (arm64 + x86_64), importer UX | M13 |
