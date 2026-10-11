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

## Phases so far

Each phase ran as parallel workstreams with an independent verification pass; results were merged as one commit
per workstream, with tracker updates. `docs/STATUS.md` has the session-by-session record.

| Phase | Workstreams | Unlocked |
|---|---|---|
| 1 ✅ | UE3 reader + LZO, symbol map, locator/inventory, native physics spec, deterministic player slice | M1, M2, M3 |
| 2 ✅ | Object payload decoder (tagged properties, classes, functions), class defaults, Kismet graphs, grapple and abilities behaviour specs, native physics port, binary RE completion, save format | M4, M5, real constants |
| 3 ✅ | Gameplay port: recovered defaults wired into the physics port; grapple, power jump, rocket boots, sprint/story speed, camera/FOV/eye height, landing, kill zones, death, checkpoints; original input bindings | M7 |
| 4 ✅ | Asset importer: textures, static meshes with collision, level actors, BSP, volumes; Bevy loaders | M6, M9 |
| 5 ✅ | Runtime world (triangle collision, scene loading, streaming), level rendering, materials, audio decoding, Matinee curves, skeletal meshes and animations | M9 |
| 6 ✅ | Kismet runtime and Matinee movers, runtime audio with narration and adaptive music, menus and saves, lightmaps, NPCs and the worm, skinned rendering, one-command importer, release workflow | M10 |
| 7 ✅ | Integration into one connected game: frame order, level transitions, story chain in the smoke harness | M11 (partial) |
| 8 ✅ | Particles, decals, gameplay VFX, localized text, camera animations and anim notifies, NPC collision, time trial, gamepad, water and foliage | M11 (partial) |

## What is next

In priority order. Nothing below is claimed as done.

| Work | State | Unlocks |
|---|---|---|
| **Behavioural parity.** Record the original (read-only), replay the same inputs through the simulation, find and fix divergences, keep the recordings as regression traces. | In progress: the Windows recorder made the first recordings of the original on 2026-10-10. The first replay diverges; the causes found so far are tooling limits (no movement input from a gamepad recording, no Kismet in the variable-step replay) and open questions (a Workshop story-mode speed of 132, a 1 uu offset), not yet classified simulation differences. The macOS recorder passed its attach and layout checks but has not recorded a trace yet | M8, M12 |
| **A hand-played playthrough**, fixing whatever blocks a route | Not started (the automated chain only proves the scripted exits) | M11 |
| **Rendering fidelity:** material placeholders, actors hidden at level start, cutscene camera FOV, post effects not yet drawn, the look of the original's HUD | Partly done; known gaps listed in `docs/INTEGRATION.md` (parts of that list are stale and need a refresh) and `docs/PARITY.md` | — |
| **Sandbox mode:** an opt-in playground for the movement and game systems (tuning, inspection, time control), kept strictly apart from the faithful game | Partly done: a first version is built ([SANDBOX.md](SANDBOX.md)), with live tuning, time control, in-memory save states, read-outs, two hand-made arenas and guard tests that the faithful game is unchanged. It is covered by automated tests and unattended screenshots, has not been used by hand yet, and its CI steps have not run yet | later tooling and modding work |
| **Packaged releases** for Windows, Linux and macOS | Workflow written, never run; needs a maintainer decision to tag | M13 |
| **Other builds of the game:** verify the importer on the Windows data (`CookedPC`) | Not started | — |
