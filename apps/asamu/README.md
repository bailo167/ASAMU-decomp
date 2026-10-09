# `asamu` — the Bevy executable

Two modes:

- **Graybox** (default; needs no game data): the hand-made test level from `asamu-world`, driven by the deterministic
  simulation (`asamu-game` / `asamu-player`) with the original's movement values.
- **Converted original level**: renders and plays a level converted on your machine from your own install by
  `asamu-import`. Converted data is copyrighted game data: keep it local, never commit or share it (screenshots
  included).

## Converted levels

```sh
OUT=/path/outside/the/repo/converted        # or research/local/<name>/converted (git-ignored)
cargo run --release -p asamu-import -- --out "$OUT" levels   --map AG-Workshop
cargo run --release -p asamu-import -- --out "$OUT" meshes   --package AG-Workshop
cargo run --release -p asamu-import -- --out "$OUT" textures --package AG-Workshop --skip-lighting
cargo run --release -p asamu-import -- --out "$OUT" textures --package Startup --skip-lighting   # shared textures
cargo run --release -p asamu-import -- --out "$OUT" materials                                    # optional
cargo run -p asamu -- --converted "$OUT" --level AG-Workshop
```

Without `materials/materials.json` every material renders untextured in a neutral colour per material. `--level`
alone uses `ASAMU_CONVERTED_DIR` or the importer's default output directory.

What happens (details in the module docs of `apps/asamu/src/converted.rs` and `crates/asamu-assets`):

- the scene JSON, the BSP and the manifests are turned into an `asamu_assets::LevelPlan` on a background task;
  meshes (glTF primitives) and DDS textures stream in through Bevy's asset server from a `converted://` asset source;
  each distinct mesh primitive and material is one shared handle (instancing/batching);
- every visible static mesh component (static mesh actors, `InterpActor`s, ASAMU actors) is placed with its UE3
  `LocalToWorld` converted by `asamu_core::coords` (render scale: 50 UU per render unit, a presentation convention);
  mirrored transforms cull front faces; the level BSP (walls, floors) is drawn with its surface materials and the
  original's BSP texture rule (1/128, confirmed in the executable);
- lights: point, spot and directional lights with our own UE3 → physical intensity mapping (an approximation:
  the original's look comes from baked lightmaps, which are not rendered; a constant ambient term stands in);
  shadows for the directional light and the 4 most powerful point/spot lights (`--light-shadows N`);
- gameplay: `asamu_game::Game::load_level` (triangle collision, checkpoints, kill zones, ...) loads on a background
  task; once ready, the simulation drives the first-person camera with the graybox controls. Until then, if it fails,
  or with `--fly`, a render-only fly camera is used (WASD, mouse, Space/E up, Ctrl/Q down, Shift fast, wheel speed);
- HUD: information panel, crosshair (green on a grapple target), ability panel (grapples left, grapple gun, power
  jump, rocket boots), subtitle area (placeholder).

Unattended checks (e.g. after changing the renderer): `--screenshot PATH` renders an offscreen 1600×900 image once
the assets have loaded (window read-back is black on an occluded or locked macOS screen), `--exit-after SECONDS`,
`--walk SECONDS` (start playing and hold forward), `--camera X,Y,Z,YAW,PITCH` (fly camera, UE3 units/degrees).
Run `asamu --help` for every option.

- A screenshot of a converted level is game content: the path is refused before the window opens when it lies inside
  this repository (except the git-ignored `research/` folders such as `research/local/`), inside a game install
  (`.app` bundle, `steamapps` tree, or `ASAMU_ORIGINAL_DIR`) or on an existing symlink.
- A texture that is missing or fails to decode is dropped from its materials (they fall back to the neutral colour
  times their multiplier) instead of hiding every mesh that uses it.
- If Bevy's teardown hangs after an exit request (seen on macOS: the render thread and the main thread wait for each
  other), a watchdog ends the process after 10 s with the requested exit code, so scripted runs finish.

Real-data checks (skipped without converted data; never run in CI):

```sh
# every converted level: plan invariants plus an independent recomputation from the raw files
ASAMU_CONVERTED_DIR=<dir> cargo test -p asamu-assets --test converted_real_data -- --nocapture
# simulation vs renderer: same PlayerStart, spawn and eye position, and the floor the pawn stands on
ASAMU_CONVERTED_DIR=<dir> cargo test -p asamu converted_levels -- --nocapture
```
