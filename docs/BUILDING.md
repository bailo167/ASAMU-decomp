# Building

Requirements: a stable Rust toolchain (see `rust-version` in `Cargo.toml`; Bevy 0.20 needs ≥ 1.97).

```bash
cargo build --workspace
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all --check
cargo run -p progress-gen -- --check
cargo run -p repo-hygiene
cargo run -p asamu           # Bevy runtime (pre-alpha)
```

`default-members` excludes the Bevy app so `cargo test` on the default set stays fast; `--workspace` includes it.

## Platform prerequisites

- **Linux:** `sudo apt-get install -y pkg-config libasound2-dev libudev-dev libwayland-dev libxkbcommon-dev libx11-dev libxcursor-dev libxi-dev libxrandr-dev`
- **Windows:** MSVC build tools.
- **macOS:** Xcode command line tools. Both `aarch64-apple-darwin` and `x86_64-apple-darwin` are targets:
  `rustup target add x86_64-apple-darwin && cargo check --workspace --target x86_64-apple-darwin`.

## Using your original install

The tools find the game through Steam (App ID 278360). To point them elsewhere:

```bash
export ASAMU_ORIGINAL_DIR="/path/to/A Story About My Uncle"
cargo run -p asamu-locate
```

Tests that need original data skip themselves when it is unavailable (`ASAMU_ORIGINAL_DIR` for the install,
`ASAMU_CONVERTED_DIR` for converted data).

## Converting and playing from source

```bash
cargo run --release -p asamu-import -- all                     # into the default user data folder
cargo run --release -p asamu-import -- --out research/local/converted all   # or a git-ignored folder
cargo run --release -p asamu -- --converted research/local/converted --level AG-Workshop
```

`asamu-import all` locates the install, checks its input files against the committed inventory
(`docs/reverse-engineering/data/inventory/*.json`, embedded at build time), runs every conversion and writes
`asamu-import-run.json` (deterministic: importer build, install identity, verification, per-stage status,
fingerprint and output digest) plus `asamu-import-state.json` (hash cache and timings). Reruns skip stages whose
fingerprint, key file and output file count and size are unchanged. The output folder must be new, empty or an
earlier import: `all` replaces whole stage folders, so it refuses a folder holding anything else. The module documentation in `tools/asamu-import/src/all.rs` describes the
resumability rules; [PLAYING.md](PLAYING.md) is the player-facing guide. Never write converted data into the
repository outside the git-ignored `research/local/` (the importer refuses), and never commit it.

Reference measurements (Apple-silicon Mac, release build, Mac build 1822049 of the game, warm file cache). The
machine was shared with other builds, so times vary by about ±30%; the package-cache comparison was repeated.

| Run | Time |
|---|---|
| First import of all nine stages into an empty folder | 32.1 s (hashing the 45 input files, 1.1 GB: 2.5 s; output digests: 2.2 s) |
| First import of all twelve stages (2026-10-10, development-profile build, machine in use) | 44.4 s (hashing 1.5 s); 13,091 files, 2,282,441,193 bytes |
| Seven stages (without kismet and lightmaps) on a quiet machine | 17.7 s |
| Rerun with nothing to do | 0.05–0.1 s |
| `--package-cache` on the six package-only stages | 3.0–3.6 s faster per forced run (building the cache: 1.6–1.7 s once, 1.5 GB) |
| Independent re-run (verification pass, machine heavily loaded by parallel builds) | 111 s first import, 0.14 s no-op rerun (wall clock incl. process start); peak footprint 2.5 GB |

Per stage (first import of the nine-stage pipeline): textures 4.9 s, meshes 2.2 s, materials 3.1 s, levels 2.4 s,
audio 1.5 s, matinee 1.3 s, skeletal 1.9 s, kismet 3.0 s, lightmaps 7.0 s. Output then: 13,108 files, 2.2 GB
(textures 1.29 GB, lightmaps 0.62 GB). The three later stages add particles 5.0 s, decals 2.1 s and
localization 0.2 s; the determinism and resume measurements below were made on the nine-stage pipeline and have
not been repeated for the twelve.
Peak resident memory of the whole run: 2.7 GB. The run manifest, including every stage's output digest, was byte
for byte identical across a first run, a no-op rerun, a forced rerun and forced reruns with the package cache, so
the conversions are deterministic and the cache does not change their output. The verification pass reproduced
this independently: same file counts and byte totals per stage (13,108 files, 2,201,376,317 bytes) and identical
manifests after a no-op rerun and after a forced lightmaps run that was killed and then resumed; a forced run of
the six package-only stages through the package cache gave the same output digests.

## Release builds and packages

`.github/workflows/release.yml` runs on a `v*` tag (or by hand with *Run workflow*). It builds `asamu` and
`asamu-import` in release mode for `x86_64-pc-windows-msvc`, `x86_64-unknown-linux-gnu` (on Ubuntu 22.04 for an
older glibc baseline), `aarch64-apple-darwin` and `x86_64-apple-darwin` (cross-compiled on the arm64 runner,
`MACOSX_DEPLOYMENT_TARGET=11.0`), checks the architecture of both macOS builds with `lipo`, smoke-tests the
native ones with `--help`, and packages one zip per target containing exactly the two executables, `LICENSE` and
`docs/PLAYING.md`. The package job requires exactly those four files in every zip and refuses any file that
starts with the magic bytes of game data or converted data (UE3 package/texture cache, Scaleform, Bink, DDS, Ogg,
WAV, glTF, PNG), writes `SHA256SUMS.txt`, uploads everything as workflow artifacts and, for a tag, attaches it to
a **draft** GitHub release that a maintainer reviews and publishes. A manual run with the `tag` input checks out
and packages that tag; a release that is already published is never modified. The workflow has not run on
GitHub yet; its scripts were exercised locally on stand-in files (including every rejection case).

Before publishing a release, check the licence notices of the statically linked dependencies (Bevy and the
rest of the dependency tree, mostly MIT/Apache-2.0): MIT requires their copyright notices in binary
distributions, and the zips currently carry only this project's `LICENSE`.

The same build locally:

```bash
cargo build --locked --release -p asamu -p asamu-import                          # host target
cargo build --locked --release --target x86_64-apple-darwin -p asamu -p asamu-import   # Intel Mac from an arm64 Mac
mkdir -p target/dist/asamu-decomp-dev    # target/ is git-ignored
cp target/release/asamu target/release/asamu-import LICENSE docs/PLAYING.md target/dist/asamu-decomp-dev/
(cd target/dist && zip -r asamu-decomp-dev.zip asamu-decomp-dev)
```

Packages must never contain game files or converted data: players convert their own copy with
`asamu-import all`.
