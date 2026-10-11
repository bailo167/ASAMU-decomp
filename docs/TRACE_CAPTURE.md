# Trace capture from the original game

How to record **behavioural traces** from the original *A Story About My Uncle* and check our runtime against
them tick by tick (milestones M8 and M12 in [ROADMAP.md](ROADMAP.md)). The tooling is built and tested; this
page is the procedure a person follows on the analysis Mac, the evidence behind each step, and what the results
are allowed to change in the tracker.

**Status.** Nothing here has been run against the running game yet: **the game was not launched** while this
tooling was written. Everything that can be checked without running the game is checked by tests (section 1.2).
What remains UNKNOWN until the first session is listed in section 4.4 (the feasibility gate).

## 1. Overview

```text
original game (Mac build, x86_64, Rosetta 2) ── LLDB + tools/trace-recorder ──► raw recording   (asamu-trace-raw v1)
raw recording ── asamu-trace convert ──► canonical trace (asamu-trace v1, source "original")
canonical trace ── asamu-trace replay ──► our trace (same inputs, same initial state, our simulation)
both traces ── asamu-trace compare ──► per-field errors, first divergence, summary JSON
summaries ── asamu-trace report ──► markdown table for docs/PARITY.md
```

### 1.1 Components

| Path | What it is |
|---|---|
| `tools/trace-recorder/asamu_lldb.py` | LLDB front end: the `asamu-rec` command (`check`, `start`, `stop`, `status`, `convert`) and the per-frame breakpoint callback. |
| `tools/trace-recorder/asamu_recorder_core.py` | Pure-Python core (no `lldb` import): layout lookups, object walk, sampling, raw format, a converter that mirrors the Rust one, a fake-memory self-test. Runs on Python 3.8+ (LLDB's embedded Python on this Mac is 3.9). |
| `tools/trace-recorder/layout_mac_x86_64.json` | Every symbol, field offset and native structure member the recorder reads, each with its evidence, plus sentinel fields and native-code evidence (section 2). |
| `tools/trace-recorder/test_lldb_glue.py` | Runs the LLDB front end against a stand-in `lldb` module and a fake game image. |
| `tools/asamu-trace` | Rust CLI and library: `convert`, `replay`, `compare`, `report`, `validate`, `check-recorder`. |

### 1.2 What is verified without the game (CONFIRMED by tests and recorded re-checks)

| Check | Command | Result (2026-10-10) |
|---|---|---|
| Recorder layout against `native_layout.json`, the defaults data, its own Python sources (88 literal lookups) and the executable (symbols, segments, extents, initial values, code bytes, call counts) | `cargo run -p asamu-trace -- check-recorder` | 309 checks, 0 failed |
| Every field offset against `native_layout.json` (native classes) or against a fresh run of the layout rules on the install (`gameplay_defaults --layout` for `asamu.GrappleGun`, `asamu.ASAMUPawn`, `asamu.ASAMURocketBoots`, `Engine.Engine`, `Engine.Player`, `Engine.Input.KeyBind`, `Engine.Camera.TCameraCache`, `Core.Object.TPOV`) | independent re-check, 2026-10-10 (not a committed test; `check-recorder` checks the script-class fields' names and types, not their offsets) | 64/64 fields agree; `KeyBind` ends at 0x1C with alignment 8 (32-byte stride) |
| LLDB resolves every recorder symbol in the executable file (no process) | `PYTHONPATH="$(lldb -P)" xcrun python3 tools/trace-recorder/asamu_lldb.py "$EXE"` | 10/10, same addresses as `nm`; every LLDB API name the front end calls exists in the installed module (embedded Python 3.9.6) |
| Python sampler and converter on a fake memory image built from the layout (also: a new map's `WorldInfo` at the old address, invalid code points in names, the alias budget, output-path guards) | `python3 -I -B tools/trace-recorder/asamu_recorder_core.py selftest` | ok (25 memory reads per frame after warm-up); passes under Python 3.9 and 3.14 |
| LLDB front end (symbol lookup, breakpoint callback, stop conditions, conversion, refusal of an output folder inside the install, no overwriting of a recording started in the same second) against a stand-in `lldb` | `python3 -I -B tools/trace-recorder/test_lldb_glue.py` | ok |
| Python converter = Rust converter on pseudo-random recordings (gaps, pauses, map/pawn changes, FOV fallback, odd bindings, rotation wraps, flying without gun data) and on a hostile binding table (wide alias fan-out) | `cargo test -p asamu-trace --test python_crosscheck` | identical samples and metadata |
| raw → convert → replay → compare reproduces our own run (a "fake original" made by our simulation) | `cargo test -p asamu-trace --test pipeline` | positions, velocities, flags, inputs exact; yaw within 1e-6 rad (f32 wrap of the summed look deltas) |
| Replay of a runtime recording on a converted level, with and without Kismet | `ASAMU_CONVERTED_DIR=... cargo test -p asamu-trace --test real_data` | exact on AG-Workshop (levels, Kismet, Matinee and static-mesh collision converted locally) |
| Replay determinism through the command line | two `replay` runs of the same trace on converted AG-Workshop, with and without `--kismet` | byte-identical outputs |

Not verified (needs the running game): attaching under Rosetta 2, the cost of a breakpoint stop per frame, the
live object walk and the sentinel values, benchmark-mode timing, the PressedKeys/axes timing at run time. These
are the feasibility gate (section 4.4).

## 2. Evidence the recorder relies on

All from the Mac depot 278362, build 1822049, `A Story About My Uncle.app/Contents/MacOS/ASAMU`, read only.

| # | Fact | Confidence |
|---|---|---|
| E1 | Thin x86_64 Mach-O, not code-signed, not stripped (no arm64 slice). | CONFIRMED ([INVENTORY.md](reverse-engineering/INVENTORY.md), [BINARY_ANALYSIS.md](reverse-engineering/BINARY_ANALYSIS.md)) |
| E2 | Symbols `GWorld`, `GEngine`, `GFrameCounter`, `GDeltaTime`, `GFixedDeltaTime`, `GIsBenchmarking`, `GUseFixedTimeStep`, `FName::Names`, `UWorld::Tick(ELevelTick, float)`, `UGameEngine::Tick(float)` exist; each data symbol has room for the read the recorder makes (`GFrameCounter` and `GDeltaTime` 8 bytes, `GIsBenchmarking` 4, `FName::Names` 16). | CONFIRMED (`check-recorder`, R1) |
| E3 | `GFixedDeltaTime` is a `double` initialised to the `float` 1/30 widened (0.03333333507180214). | CONFIRMED (file bytes, `check-recorder`) |
| E4 | `appUpdateTimeAndHandleMaxTickRate()` reads `GIsBenchmarking`, `GUseFixedTimeStep` and `GFixedDeltaTime` and writes `GDeltaTime`; `FEngineLoop::PreInit` references `GIsBenchmarking`; `FEngineLoop::Init` writes `GFixedDeltaTime`. | CONFIRMED (R3) |
| E5 | `-BENCHMARK` sets `GIsBenchmarking`, `-FPS=<n>` sets `GFixedDeltaTime = 1/n`, and while benchmarking every frame's `DeltaTime` is `GFixedDeltaTime`. | STRONG (E4, the wide strings `BENCHMARK` and `FPS=`, stock UE3); the feasibility gate confirms it (`asamu-rec check` prints both values) |
| E6 | Nothing stores to `GUseFixedTimeStep`, so fixed stepping has to come from `-BENCHMARK`. | CONFIRMED (R3, R3b) |
| E7 | No map's Kismet uses `SeqCond_IsBenchmarking`; other readers of `GIsBenchmarking` affect presentation. | CONFIRMED for Kismet (R4); TENTATIVE for the rest |
| E8 | Command-line words present as UTF-32 strings: `BENCHMARK`, `FPS=`, `FIXEDSEED`, `WINDOWED`, `ResX=`, `ResY=`, `DEMOREC`, `EXEC=`, `NOSTEAM`; absent: `ALLOWCONSOLE`, `NOSOUND`, `VSYNC`. | CONFIRMED (R5); what each does is TENTATIVE |
| E9 | `SteamAPI_RestartAppIfNecessary` is imported: started outside Steam the game may relaunch itself through Steam, which would detach a debugger that started it. | CONFIRMED (import); behaviour TENTATIVE |
| E10 | Native field offsets of the engine classes on this build (the script layout reproduces all 1,447 native class sizes). | CONFIRMED ([DEFAULTS.md](reverse-engineering/DEFAULTS.md) §2) |
| E11 | While the grapple is attached the pawn is in `PHYS_Flying` (4); the anchor is the gun's `vGrappleLocation` (the hit point), which follows the gun's `GrappleGunHitLocActor` only for a target that carries its anchor. The helper's own location is **not** the anchor otherwise (corrected 2026-10-10 from the first recordings, section 3.3). | CONFIRMED ([GRAPPLE.md](reverse-engineering/GRAPPLE.md) §1, §2, G-AT-4/7/8; measured, 3.3); "nothing else flies" STRONG |
| E12 | The camera updates once per frame at the end of `UWorld::Tick`. | CONFIRMED (native; [PARITY.md](PARITY.md) deviation 2) |
| E13 | The depot ships no anti-cheat component. | CONFIRMED (inventory) |
| E14 | The Windows executable is a 32-bit `Binaries/Win32/ASAMU-Win32-Shipping.exe`, not in the Mac depot. | STRONG (`PCTOC.txt`) |
| E15 | **Frame structure.** `FEngineLoop::Tick` calls `appUpdateTimeAndHandleMaxTickRate()`, then `GEngine->Tick((float)GDeltaTime)` (vtable slot 0x260 of `UGameEngine` = `UGameEngine::Tick`), then increments `GFrameCounter` (a 64-bit counter, `+1` per frame), and pumps the OS events (`appMacPumpMessages`) at the end of the loop. `UGameEngine::Tick` calls `Client->Tick` (Engine+0x6D0, vtable slot 0x268 = `UMacClient::Tick`), whose tail call goes to vtable slot 0x80 of the object at Client+0x78 — slot 0x80 of `FMacViewport` is `FMacViewport::ProcessInput(float)` — then `UObject::StaticTick`, then calls `UWorld::Tick` exactly once (one direct call). `UGameViewportClient::Tick` (the interactions' tick) is called later, after `UWorld::Tick`. | CONFIRMED for the calls, the counter and the slot contents (disassembly and the vtables' initial values in the file, re-checked 2026-10-10; `check-recorder` call count and order check); that the object at Client+0x78 is the `FMacViewport` is STRONG |
| E16 | Hence **the frame's input is dispatched to `PlayerInput` before `UWorld::Tick`**: at its entry, `PressedKeys` and `bPressedJump` belong to the coming frame. This answers GRAPPLE.md §17 Q2 / ABILITIES.md §17 Q1 for the order relative to the world tick. | STRONG (E15 plus the stock UE3 route `ProcessInput` → viewport client `InputKey` → `PlayerInput`; the indirect calls inside `FMacViewport::ProcessInput` were not resolved); the first trace confirms it (F5: keys lead the motion by exactly one frame) |
| E17 | `Engine.GamePlayers` (array) at Engine+0x6D8 with its count at +0x6E0, and `Player.Actor` at Player+0x68. | CONFIRMED (`UGameViewportClient::GetPlayerOwner`, `ULocalPlayer::SpawnPlayActor`; equal to the layout rule) |
| E18 | Dynamic arrays are `{data pointer, i32 count, i32 max}` (count at +8); `FNameEntry` is a 64-bit flags word, the 32-bit index word at +8 (bit 0 set = wide, 4-byte UTF-32 characters), characters at +0x18. | CONFIRMED (disassembly of `GetPlayerOwner`, `FNameEntry::GetNameLength`, `FNameEntry::GetSize`) |
| E19 | Offsets inside the script-only classes `GrappleGun`, `ASAMUPawn`, `ASAMURocketBoots` come from the same link rules (the rules model the engine's own property linking, which also lays out script-only classes). | STRONG; upgraded to CONFIRMED by the live sentinel check (section 2.2) |
| E20 | `FName` is `{i32 index, i32 number}`; a name with number `n > 0` prints as `Name_<n−1>`. | TENTATIVE (stock UE3); every class name the recorder resolves at run time checks it |

Reproduce (`$EXE` = the executable above; output is names, counts and small numbers only, nothing to commit):

```sh
EXE="$HOME/Library/Application Support/Steam/steamapps/common/A Story About My Uncle/A Story About My Uncle.app/Contents/MacOS/ASAMU"
# All layout, symbol, code-byte and call checks in one go (E2, E3, E15, E17, E18):
cargo run -p asamu-trace -- check-recorder --verbose
# R1 symbols
nm "$EXE" | grep -E ' _(GWorld|GEngine|GFrameCounter|GDeltaTime|GIsBenchmarking|GUseFixedTimeStep|GFixedDeltaTime)$'
nm "$EXE" | grep -E 'FName5NamesE$|6UWorld4TickE10ELevelTickf$|11UGameEngine4TickEf$'
# E15: calls inside UGameEngine::Tick and FEngineLoop::Tick (look for Client->Tick, UWorld::Tick, GFrameCounter)
objdump -d --no-show-raw-insn --disassemble-symbols=__ZN11UGameEngine4TickEf "$EXE" | c++filt | grep -E 'callq|0x6d0\(%r14\)' | head -20
objdump -d --no-show-raw-insn --disassemble-symbols=__ZN11FEngineLoop4TickEv "$EXE" | c++filt | grep -E 'GFrameCounter|appMacPumpMessages|\*0x260'
objdump -d --no-show-raw-insn --disassemble-symbols=__ZN10UMacClient4TickEf "$EXE"
# E17, E18
objdump -d --disassemble-symbols=__ZN19UGameViewportClient14GetPlayerOwnerEi,__ZNK10FNameEntry13GetNameLengthEv "$EXE"
# R2 the default fixed step (file offset = address - 0x1020EC000 + 0x20EC000, see BINARY_ANALYSIS.md)
python3 -c "import struct,sys; f=open(sys.argv[1],'rb'); f.seek(0x22F5068); print(struct.unpack('<d', f.read(8))[0])" "$EXE"
# R3 which functions reference the time-step globals (about 20 s)
objdump -d --no-show-raw-insn "$EXE" | awk '/^[0-9a-f]+ <.*>:$/ {fn=$2} /0x1022f5068|0x1023bdaf0|0x1023cec10/ {print fn}' | c++filt | sort | uniq -c
# R3b the instructions after each reference to GUseFixedTimeStep are loads only; its address appears once more in
# the file, in the symbol table
objdump -d --no-show-raw-insn "$EXE" | grep -A5 '<_GUseFixedTimeStep>$'
python3 -c "import struct,sys; d=open(sys.argv[1],'rb').read(); print(d.count(struct.pack('<Q',0x1023cec10)))" "$EXE"
# R4 Kismet: no SeqCond_IsBenchmarking in any map (local exports from `asamu-inspect kismet`, see KISMET.md)
grep -l SeqCond_IsBenchmarking research/local/kismet/*.kismet.json || echo none
# R5 wide (UTF-32) command-line words
python3 -c "import sys; d=open(sys.argv[1],'rb').read(); [print(w, d.count(w.encode('utf-32-le'))) for w in sys.argv[2:]]" "$EXE" BENCHMARK FPS= WINDOWED ResX= ResY= FIXEDSEED DEMOREC EXEC= NOSTEAM ALLOWCONSOLE NOSOUND VSYNC
```

### 2.1 Where the values live

The authoritative table is `tools/trace-recorder/layout_mac_x86_64.json` (each entry names its evidence:
`native_layout` = a row of `native_layout.json`, `layout_rule` = the same rules applied with
`cargo run --release -p asamu-inspect --example gameplay_defaults -- --layout <Class>`, `native_code` = shown by
the instruction bytes listed in the file). Those `native_evidence` entries are 3–7 byte encodings of single
instructions, each restating an offset already given in its `what` text (the same kind of evidence as a quoted
disassembly line, not a code payload); `check-recorder` finds each one in its function. The walk, done every
frame:

```text
GEngine ─► Engine.GamePlayers[0] (+0x6D8) ─► Player.Actor (+0x68) = player controller
controller ─► Controller.Pawn (+0x250), PlayerController.PlayerInput (+0x588), .PlayerCamera (+0x450), Actor.WorldInfo (+0x118)
pawn ─► Actor.Location/Rotation/Physics/Base/Velocity/Acceleration, Pawn.Weapon (+0x4C8) = the GrappleGun
WorldInfo ─► TimeSeconds, DeltaSeconds, TimeDilation, Pauser; outermost Outer = the map package
```

| Object | Fields read | Confidence |
|---|---|---|
| Pawn (`Engine.Actor`/`Pawn`) | `Location` 0x80, `Rotation` 0x8C, `Physics` 0xC0, `Base` 0xD0, `Velocity` 0x190, `Acceleration` 0x19C, `GroundSpeed`, `AirSpeed`, `JumpZ`, `AirControl` | CONFIRMED (E10) |
| Controller | `Rotation` (the view), `bPressedJump` (word 0x460 bit 1), `FOVAngle` 0x4A0 | CONFIRMED (E10) |
| `PlayerInput` | `Bindings` 0xB0 (each `KeyBind` 32 bytes: name, command string), `PressedKeys` 0xC0, `aBaseY`, `aStrafe`, `aForward`, `aTurn`, `aLookUp`, `aMouseX`, `aMouseY` | CONFIRMED (E10); `KeyBind` size from the layout rule, STRONG |
| Camera | `CameraCache.POV.FOV` 0x434 | STRONG (layout rule inside the CONFIRMED `CameraCache` at 0x418) |
| `GrappleGun` | `bIsGrappling` / `bReleasedGrapple` / `bCanGrapple` (word 0x3D8 bits 0, 1, 5), `vGrappleLocation` 0x3DC, `vDistance` 0x3EC, `HitLocActor` 0x4E0, `iTimesGrappled` 0x4F0, `iMaxGrapples` 0x4F4 | STRONG (E19) |
| `ASAMUPawn` | flags word 0xAE0 (`bHasJumped` 0, `bPowerJumped` 1, `bHasReleasedJump` 2, `bSprinting` 4, `bIsFalling` 8), `rocketBoots` 0xAF0 | STRONG (E19) |
| `ASAMURocketBoots` | `bFinished` / `bEnabled` (word 0x280 bits 0, 1) | STRONG (E19) |
| `WorldInfo` | `TimeDilation` 0x530, `TimeSeconds` 0x538, `RealTimeSeconds` 0x53C, `DeltaSeconds` 0x544, `Pauser` 0x550 | CONFIRMED (E10) |

### 2.2 Live layout check (sentinels)

Before it records anything from an object, the recorder reads fields that should still hold their class defaults
and compares them with the values recorded in `docs/reverse-engineering/data/defaults/`: pawn `WalkableFloorZ` 0.78,
`fTerminalVelocity` 10000, `jumpVelocityLowerMultiplier` 0.7, `zoomFOV` 50, `storyModeSpeedMultiplier` 0.6; gun
`fMaxDistance` 5000, `fGrappleReleaseDistance` 200, `fGrappleMaxSpeed` 10000, `instantReleaseDelay` 0.05; input
`MoveForwardSpeed` 1200. One wrong value stops the recording with the field named (`asamu-rec check` shows the
same). The test checks that each expected value equals the data file. Each object is checked once, at its first
sample.

That these values do not change at run time is CONFIRMED by the script write scan for `WalkableFloorZ`,
`storyModeSpeedMultiplier` and `fTerminalVelocity` ([DEFAULTS.md](reverse-engineering/DEFAULTS.md) §4.1, §4.2),
STRONG for `fGrappleMaxSpeed` and `jumpVelocityLowerMultiplier` (no script reads them at all: GRAPPLE.md §2,
ABILITIES.md §1; writes were not searched for) and TENTATIVE for the rest (no write is known, but none was
searched for). A pass upgrades E19 to
CONFIRMED. A failure on one field only, with the other fields of the same object passing, more likely means a
script changed that value: check it before touching offsets. A failure on every field of an object means the
script-class offsets are wrong — recompute them with the `--layout` command above, fix the layout file, rerun
`check-recorder`.

## 3. Formats

### 3.1 Sampling point and frame alignment

One software breakpoint at the entry of `UWorld::Tick`, with a Python callback that returns `False` (LLDB
continues at once and reports no stop). Why there:

- once per frame (E15), so one stop per frame; `rdi` is the `UWorld` (checked against `GWorld`, other worlds
  are skipped) and `xmm0` the frame's `DeltaSeconds`;
- after the frame's input dispatch (E16): `PressedKeys`/`bPressedJump` are the coming frame's input;
- before any actor ticks: pawn, gun, helper and camera hold the state the previous frame ended with, including
  the gun's pull and proximity release that run after the pawn's physics (GRAPPLE.md G-TM-2).

`APawn::performPhysics` would run mid-frame (before the gun), once per pawn and with the input already consumed;
`UGameEngine::Tick` entry would come before the input dispatch (E15).

Record `R_i` at the entry of frame `F_i` therefore holds the **state at the end of frame `F_i − 1`** and the
**input of frame `F_i`**; its `WorldInfo.DeltaSeconds` is the length of frame `F_i − 1`. The `PlayerInput` axes
in `R_i` are the values the previous frame's controller tick produced (kept as a cross-check).

### 3.2 Raw recording (`asamu-trace-raw` v1)

JSON Lines, written by the recorder while it runs (`research/local/traces/<UTC time>-<scenario>.raw.jsonl`; the
scenario is reduced to one file-name component, and a `-1`, `-2`, ... suffix is added instead of overwriting an
existing file). Line 1 is the header, then one record per frame that has a player and is not paused. Engine units
throughout. **No addresses or pointers** are written: objects appear as class names, object names and a
per-recording pawn number. Names read from the game are written with invalid code points (UTF-16 surrogates,
values above U+10FFFF) replaced by U+FFFD, so every JSON reader accepts the file.

| Header field | Meaning |
|---|---|
| `format`, `version` | `"asamu-trace-raw"`, `1` |
| `recorder`, `layout`, `game_build`, `sample_point` | provenance |
| `scenario`, `launch_options` | as given to `asamu-rec start` (launch options are recorded as told, not read) |
| `benchmarking`, `fixed_delta_time` | `GIsBenchmarking` and `GFixedDeltaTime` at the first sample |
| `bindings` | the game's key bindings (`[{name, command}]`) read from the running `PlayerInput`, so rebinding is honoured |
| `notes` | free-form |

| Record field | Source |
|---|---|
| `frame`, `dt_arg` | `GFrameCounter`; `xmm0` at the breakpoint |
| `world` | `map` (outermost package of the `WorldInfo`, walked every frame: a new map's `WorldInfo` can reuse the previous one's address), `time_seconds`, `real_time_seconds`, `delta_seconds`, `time_dilation`, `paused` |
| `player.controller_class`, `pawn_class`, `pawn_id` | class names; a number per pawn object address (a new pawn at a new address gets a new number; frames without a pawn, as during a respawn, also split the run) |
| `location`, `velocity`, `acceleration` | pawn, UU, UU/s, UU/s² |
| `pawn_rotation`, `view_rotation` | pawn and controller `Rotation`, rotator units (65536 per turn), as stored |
| `physics`, `base` | `Physics` (1 walking, 2 falling, 4 flying, ...), name of the `Base` actor |
| `fov_camera`, `fov_controller` | camera POV FOV, controller `FOVAngle` |
| `keys`, `pressed_jump`, `axes` | `PressedKeys` names, `bPressedJump`, the `PlayerInput` axes |
| `ground_speed`, `air_speed`, `jump_z`, `air_control` | pawn movement values (script changes them: sprint, story mode, grapple) |
| `eye_height` | pawn `EyeHeight` (0x378, CONFIRMED offset): the camera height above the centre before bob; not in v1 samples, kept per tick in the `state:` note (3.5) |
| `gun` | `grappling`, `released`, `can_grapple`, `anchor` (the helper actor's location: the anchor only while it rides a moving target, 3.3), `grapple_location` (`vGrappleLocation`: the anchor), `distance` (`vDistance`), `times_grappled`, `max_grapples` |
| `pawn_flags`, `boots` | `ASAMUPawn` flags; boots `enabled`, `finished` |

### 3.3 Canonical trace (schema v1, unchanged)

`asamu-trace convert` (Rust, the reference; the recorder's Python converter is tested to agree, bit for bit)
writes the format of `crates/asamu-player/src/trace.rs` ([PARITY.md](PARITY.md#trace-format)) with
`source: "original"`:

| Canonical | From the raw records (run `R_0..R_n` → samples `0..n`) |
|---|---|
| sample 0 | state of `R_0`, neutral input, time 0 |
| sample `k ≥ 1`: buttons | keys of `R_(k−1)` mapped through the recorded bindings: `jump_held` (`Jump`), `grapple_held` (`StartFire`), `sprint_held` (`StartSprinting`), `power_jump_held` (`PowerJumpKeyDown`); `jump_pressed` = jump key rising edge against `R_(k−2)` or a rising `bPressedJump` (catches a press and release inside one frame; at `k = 1` there is no `R_(−1)`, so only a set `bPressedJump` counts); `use_pressed` = rising edge of `use`. Aliases are expanded up to depth 8 and at most 256 command parts per key (a hostile table cannot stall the conversion; real tables use a few). A gamepad's buttons are keys like any other (`XboxTypeS_A`, ...) |
| sample `k ≥ 1`: `move_forward`, `move_right` | **keyboard:** summed signs of the held keys' `Axis aBaseY` / `Axis aStrafe` commands (clamped to ±1). **Gamepad:** the stick's direction, derived from the pawn's `Acceleration` of `R_k` in the axes of `R_(k−1)`'s pawn rotation (3.4). Chosen per run (`--move-input auto`): the keys if any record of the run holds a key bound to a move axis, else the acceleration if the run has any |
| look deltas | wrapped change of the controller rotation `R_(k−1) → R_k`, × 2π/65536 — exact and independent of the mouse scaling we have not ported (PARITY deviation 13) |
| state | `R_k`: `position`, `velocity`; `yaw`/`pitch` = signed 16-bit rotator units × 2π/65536; `fov` = camera POV FOV, else `FOVAngle` (**the cached view FOV, which does not show the zoom**: see "The FOV column" below); `grapple_state` attached ⇔ `bIsGrappling`, anchor = the gun's `vGrappleLocation` (see below); without gun data (weapon not a `GrappleGun`) no anchor is known, so the sample is idle even in `Physics` 4, and such samples are counted in a `check:` note; `rope_length` null; `grounded` ⇔ `Physics` 1 |
| `time` | Σ `DeltaSeconds` of `R_1..R_k` |
| `tick_rate` | `1 / DeltaSeconds` when every frame of the run had the same length (rounded to an integer within 0.001 Hz), else `null` |
| `notes` | recorder, sample point, launch options, scenario, frame range and segment; `input:` (bindings); `move input:` (where the move axes come from, how many samples got a direction and how many got 0 for which reason, 3.4); `anchor:` and `check:` lines (counts: axes vs keys, acceleration without a move key, grapple flag vs `PHYS_Flying`, `PHYS_Flying` without gun data, helper away from the anchor, `vDistance` vs the distance to the anchor, FOV fallbacks, an eye height left out of the state note); `fov: cached view FOV …` (on how many samples the FOV column is the camera's cached view FOV); `event:` lines (teleports, level-script state changes, attaches of the grapple: 3.6); `state: {...}` (the recorded script state of every tick, 3.5); `init: {...}` (the older form: tick 0 only) |

A run ends at a frame gap, a paused frame, a frame without a player, a change of map, pawn object or class, and
where `WorldInfo.TimeSeconds` goes back (the level was loaded again); each run of at least two records is one
trace (`<stem>.trace.jsonl`, or `<stem>.seg<i>.trace.jsonl`). A respawn after a death ends no run (same map,
same pawn object, consecutive frames): it is marked as an event instead (3.6). Without recorded bindings the
converter falls back to the signs of `aBaseY`/`aStrafe` (of `R_k`) and `bPressedJump`, and says so in the notes.

**The FOV column** ([PARITY_FINDINGS.md](PARITY_FINDINGS.md) N1). `fov_camera` is the camera's cached view FOV
(`CameraCache.POV.FOV`). The camera writes its default FOV into that field on every view update, while the zoom
locks the FOV in other fields of the camera, which recorder 0.1.0 does not read: the field is 90.0 on all 45,237
records of 2026-10-10, through zoom-button holds (CONFIRMED that it cannot show the zoom; whether the original
zoomed in those frames is UNKNOWN until a recording has the lock fields). The converter therefore writes
`fov: cached view FOV (the camera's CameraCache.POV.FOV) on N of M samples: …`, and `asamu-trace compare` leaves
the FOV out of the **verdict** of a trace whose note covers **every** sample (`on N of N samples`;
`--fov-verdict auto`, the default; `count` and `exclude` state it). A trace whose note covers only part of its
samples (the rest is the controller's `FOVAngle`, of which nothing says that it cannot show the zoom), or whose
note cannot be read, keeps its FOV in the verdict unless `exclude` is given; all 14 traces of 2026-10-10 are
covered in full. A converter that reads the camera's locked FOV must not write the note. Nothing else changes:
the FOV's tolerance, its statistics and its first exceedance are reported as before, and the summary says
whether the FOV counted and why. (A comparison in which only an excluded FOV differs has the verdict `Exact`:
exact in every field that counts. The text and the report show the FOV's numbers next to it.)

**The anchor** (corrected 2026-10-10). The converter used to write the location of the gun's helper actor
(raw `anchor`). The first recordings show that this is the anchor only for some grapples: the gun's own
`vDistance` equals the pawn's distance to `vGrappleLocation` within 0.01 uu on 9,594 of 9,601 attached records
(the other 7, up to 6.1 uu off, are each the first record of a grapple), but its distance to the helper within
1 uu on 5,071 only; in one segment the helper was 24,700 uu from the anchor and moving. That is what
GRAPPLE.md G-AT-7/8 describe (the helper is placed, and followed, only for a target that carries its anchor;
otherwise it stays where an earlier target left it): CONFIRMED by the measurement
(`ASAMU_TRACE_RAW_DIR=... cargo test -p asamu-trace --test real_recordings -- --nocapture`). Conversions made
before the change (`research/local/traces/win/{WS1.trace.jsonl,DC1,PLAY2}`) have wrong anchors on those
samples.

### 3.4 Move axes of a gamepad recording

A stick is not a key, and the polling recorder reads the `PlayerInput` axes after the engine cleared them: on
all 45,237 records of the first three recordings `aBaseY = aStrafe = 0`, while the keys list holds only pad
buttons. So the first conversions had `move_forward = move_right = 0` on every tick. The recorded pawn
`Acceleration` carries the direction; `tools/asamu-trace/src/move_input.rs` inverts the original's mapping and
documents the evidence (M1–M6 there, with counts). In short:

| Pawn state of `R_k` | What the acceleration is | Derived move axes |
|---|---|---|
| Walking or falling, acceleration ≠ 0 | `AccelRate` (2048) along `normal(aForward·X + aStrafe·Y)` without its Z part, `X`/`Y` = forward and right axes of the pawn's rotation **as the frame began** (`R_(k−1)`), each angle **truncated to 4 rotator units**; pitch and roll take part. The stick's magnitude is not in it (the original discards it). After a landing the value can have unit length instead (2 records) | the unit vector `(forward, right)` that solves it: the stick's direction after the dead zone. CONFIRMED for the yaw, its time and its truncation (10,051 of 10,051 on-axis samples), STRONG for pitch and roll (kept pitch: 15 of 22 decidable samples continue into the next frame, against 1 with yaw-only axes; lean alone: the direction holds still in 14 of 188 decidable windows, against none) |
| Walking or falling, acceleration = 0 | no deflection beyond the dead zone, **or** a move suppressed in a way the record does not show (the move-input lock, a controller state without a walking move) | 0, counted as "others" |
| Up to two records after a grapple let go, acceleration = 0 | the controller's tick after a release runs no move (GRAPPLE.md G-RL-7; measured: after 75 releases by the button never fewer than one zero record, exactly one after 69; after 43 releases with the button held exactly two after 30, one after one, more after the rest) | 0, counted apart: what the stick did is not known and neither game reads it |
| Grapple attached (`Physics` 4) | always 0 (9,601 of 9,601 records): no steering is read (G-PH-1) | 0, counted apart |

Not well defined, in other words: the stick's magnitude anywhere; its direction while attached and in the
release gap (irrelevant to either game); and whether a zero outside those is "no input" or "input ignored".
The last matters if our simulation ignores input at other moments than the original: the replay then gets 0
where the player may have pushed the stick, and that difference does not show.

Because the derived axes are the stick's direction as the **original** used it, a replay shows where our
simulation maps it differently: it builds its axes from the exact yaw alone (no 4-unit truncation: up to 0.017°;
no lean roll: up to 0.56°, on 3,133 of the 14,744 derived samples without pitch; no kept pitch in the first move after a
flight: up to 4.8°, on 105 samples). `convert --move-frame yaw` is the diagnostic for that: it reads the
acceleration in the exact yaw-only axes instead, so a yaw-only simulation reproduces the recorded acceleration
direction (and those three differences no longer show). `--move-input keys|acceleration` overrules the per-run
choice; a keyboard recording keeps its keys under `auto`, and a `check:` note counts samples that have an
acceleration but no move key.

### 3.5 Recorded script state and where a replay starts

`state:` (one notes line, `tools/asamu-trace/src/state.rs`) lists, for every tick, the fields of the raw record
that the samples do not carry, as changes: `physics`, `base`, `ground_speed`, `air_speed`, `jump_z`,
`air_control`, `eye_height`, the pawn flags (`sprinting`, `has_jumped`, `power_jumped`, `has_released_jump`,
`is_falling`), the gun's `grappling`, `released`, `can_grapple`, `times_grappled`, `max_grapples`, and the boots'
`boots_enabled`, `boots_finished`. The eye height changes on most frames of a moving pawn (26,283 of the 45,237
samples of 2026-10-10), which makes the line long (the meta line of the longest of those runs has 277 KB); a trace's meta
line may have 1 MiB, so a run whose eye height changes on more than 16,000 ticks is written without it and says
so in a `check:` note (a limit of the format, not of the game). Traces converted before the field was kept do
not have it.

`asamu-trace replay` starts from any tick `T` (`--from-tick`): position, velocity, view and walking/falling
from sample `T`, and from the recording, never from a guess:

| Our state | From |
|---|---|
| `GroundSpeed`, `AirControl`, `JumpZ`, `AirSpeed` | the recorded values at `T` (so 0.3 before the first normal landing and the landed value after it, 880 while sprinting, 132 in AG-Workshop) |
| sprint applied | `bSprinting` |
| story mode | `GroundSpeed` when it is the story speed 264 (on) or the walking or sprint speed 440/880 (off): the pawn's own script writes no other value (ABILITIES.md A-WK-3). Any other value comes from a console `SetSpeed` (AG-Workshop, AG-Epilogue) and shows nothing: the replay leaves story mode as it is, prints a warning, and `--story-mode on|off` states it (for AG-Workshop: on, [LEVELS.md](reverse-engineering/LEVELS.md); in the recording the jump button never lifts the pawn) |
| grapple capacity, used count, fire latch; rocket boots enabled | the gun's and boots' recorded values at `T` |
| eye height | the recorded `EyeHeight` at `T` (it matters to a grapple pressed from the ground: the fire trace starts at the eye, and at walking presses the recorded eye height was 28.0 to 44.6, not the standing 38) |
| the pawn has a base | a walking start whose recorded `base` is not null. Our first floor check then leaves the pawn where it stands if its floor is 1.9 to 2.4 uu below it, as the native check leaves a based pawn (NATIVE_PHYSICS.md 3.3); without a base it re-seats the pawn at 2.15 in any case |
| button levels before `T` (for press and release edges in the first tick) | sample `T`'s own input; at the first sample they are unknown and taken as released |
| FOV | sample `T` |

Not recorded, so they start as our simulation starts them: "sprint after landing", the pawn's and the power
jump's state code (jump-release damping, a charging power jump, a running zoom), zoom availability, the gun's
attachment and timers, the boots' boost, the walk bob, the floor normal. All of
these are at rest when the pawn stands still with nothing held, so **a replay of an original recording starts
only there**: at a sample that is walking, has zero velocity, no attached grapple, no move input and no jump,
grapple or power-jump button (a held sprint button is fine), whose previous sample is the same, and in whose
tick the pawn was not teleported. A start
anywhere else (in the air, moving, attached) is **refused**, with the standing-still ticks before and after it
in the error; `--start snap` moves the start forward to the next such tick, `--start force` starts there all the
same with a warning in the notes (a forced start while attached does not recreate the attachment). Traces of
our own runtime start anywhere, as before. A trace converted before the `state:` note existed has `init:` for
its first sample only.

**What the replay says about its start** ([PARITY_FINDINGS.md](PARITY_FINDINGS.md) N2). The recorded position is
the original's, on the original's collision; ours has other shapes and rest heights, and that alone can decide a
replay before any rule of movement is exercised. Whenever the start state is written, the replay's notes carry:

| Note | Meaning |
|---|---|
| `start: our floor is D uu below the pawn at the recorded position …: <surface>; <base>` | the distance our own floor check will measure, what it hits (the actor's object name on a converted level, read from the level's scene file; world geometry otherwise) and how that compares with the recorded base actor: the same name (names are not unique across streamed levels), another actor, world geometry on both sides (the original's base is then its `WorldInfo`), or not comparable |
| `start: no floor of ours within 28 uu below …` | the floor check will find nothing: our pawn starts to fall |
| `warning: the start position overlaps our collision …` | the pawn's shape at the recorded position is inside our geometry; the note says on what, and how far above the recorded height a pawn lowered onto that place comes to rest. Our pawn usually cannot move from there (four of the first recordings' "pawn does not move" starts were this) |
| `start: in the first tick our pawn moves A uu vertically and the original B uu while both walk` | the first tick's vertical move of both. A **warning** instead when the two differ by more than 0.5 uu (the width of the native hover band): the two stand on different floor heights from the start, and every vertical number of the replay carries that offset |
| `warning: our pawn did not move in N tick(s) …` | it never left an overlapping start while the original travelled |

`asamu-trace starts TRACE` lists the standing-still stretches with the recorded state at each (speed, story
mode, air control, grapple budget, floor actor) and what happens between them (moves, sprint, jumps, power-jump
button, grapple button, attach, release by the button or with it held, leaving the ground, landing; `--events`
for every event with its tick): that is how a segment is picked.

### 3.6 Events a replay cannot follow, and attaches no sample shows

A recording contains things that a replay of its inputs cannot reproduce, and one thing its samples do not
show. The converter marks them in `event:` notes; `asamu-trace starts --events` lists them among the player's
own events; readers compute them from the samples and the `state:` note (`tools/asamu-trace/src/segments.rs`),
so they are also found in traces converted before the notes existed. The schema stays v1.

| Event | Rule | In the recordings of 2026-10-10 |
|---|---|---|
| teleport | the pawn moved more than 10,000 uu/s × the frame length in one tick. 10,000 is `ASAMUPawn.fTerminalVelocity` (class default), the 3-D speed clamp of the falling physics: no fall covers more | 5 ticks, each a respawn after a death (10,035 to 34,860 uu in one tick; the largest displacement per time anywhere else is 6,776 uu/s, in the fall before one of them): DC1 seg0 997 and 5746, PLAY2 seg1 5946, PLAY2 seg5 2393 and 6779. CONFIRMED on these 45,237 samples. Limit of the rule: a script move shorter than that (167 uu in a frame of 1/60 s) passes as a move, so a respawn that close to the place of death would not be found (the samples alone cannot show whether there was one; the analysts counted 5 deaths, 5 of 5 found) |
| story mode on / off | the recorded `GroundSpeed` becomes the story speed 264, or goes from it to the walking or sprint speed 440 / 880 (between 440 and 880 is the pawn's own sprint: no event) | 6: DC1 seg0 7214, DC1 seg3 5019, PLAY2 seg0 1475, PLAY2 seg1 785 and 8250, PLAY2 seg4 2470 |
| `GroundSpeed` set | any other change of the recorded `GroundSpeed`: a console `SetSpeed` (ABILITIES.md A-WK-3) | none (AG-Workshop holds 132 from its first record) |
| grapple capacity changed | `iMaxGrapples` changes | 1: PLAY2 seg4 2535 (2 → 3) |
| rocket boots enabled / disabled | the boots' `bEnabled` changes | none |
| grapple attached and released within the frame | the gun's used-grapple counter `iTimesGrappled` rose, but the attached flag did not rise in that sample: the attach, its pull and a release all ran inside one frame. Every attach raises the counter (GRAPPLE.md G-AT-1), so the attaches of a recording are **counted from the counter** | 15 of 134 attaches (DC1 seg0 3655; PLAY2 seg1 2176; PLAY2 seg7 1436, 1449, 1462, 1517; PLAY2 seg8 13, 40, 128, 156, 184, 212, 238, 265, 278) |

**Validity of a replay** (`replay --level-events stop|inject|ignore`; original recordings only, since our own
simulation reproduces its own respawns). A teleport and the level-script changes (the four rows from "story
mode" to "rocket boots") come from outside the player's simulation: a death and the level's Kismet, neither of
which a per-sample replay runs.

- `stop` (the default): the replay **ends before the first such tick** and says so (`validity: stopped before
  tick T (…): … N of M tick(s) replayed`). The trace it writes is the valid part; nothing after an event is
  compared any more.
- `inject`: the recorded change is taken over at that tick, under an `injected:` note, and the replay goes on.
  A level-script change is written into our state before our tick (story mode entered or left, the console
  speed, the capacity, the boots); the record after the frame shows the change, so the frame ran with it, but
  whether the script acted before or after the pawn's physics of that frame is not recorded: TENTATIVE (one
  instance agrees with "before": at DC1 seg3 tick 5019 the original leaves story mode and walks 299.0 uu/s at
  the end of that same frame, 333.2 the frame after; ours with the change written before the tick 299.1 and
  333.4). A teleport replaces our position, velocity, view and physics mode after our tick.
- `ignore`: the tick is simulated like any other, as replays did before; the notes carry a warning that names
  the tick from which the comparison is not valid.

The replay also counts both sides' attaches the same way (`attaches:` note: the attached flag rose or the
counter rose) and compares, after every tick, our `GroundSpeed`, `AirControl`, sprint flag, used grapples,
capacity and eye height with the recorded ones (`state check:` notes), because canonical samples do not carry
them ([PARITY_FINDINGS.md](PARITY_FINDINGS.md) N7, N10).

### 3.7 One-step replays and what a comparison reports

**Free-running** (the default): the replay starts from the recording at one tick and runs on with our rules.
Its errors accumulate: a pawn that stands 1 uu lower from the first tick has a position error of at least 1 uu
on every later tick, whatever our rules do there.

**One step at a time** (`replay --one-step`): every tick restarts from the recording's previous sample, so
sample `k` of our trace is one tick of our rules applied to the original's state `k − 1`. The errors are those
of one tick; they must never be read against, or gated with, a free-running tolerance (the comparison's text,
its summary JSON and the report mark the mode).

| | |
|---|---|
| Resynchronised before each tick, from sample `k − 1` and the `state:` note at `k − 1` | position, velocity, yaw, pitch; the physics mode (walking, falling, or attached at the recorded anchor: an attachment we do not have is created on world geometry, one the original does not have is released); `GroundSpeed` with the story mode it shows, `AirControl`, `JumpZ`, `AirSpeed`, the sprint flag; the gun's used count, capacity, fire latch and released flag; rocket boots enabled; the eye height |
| **Not** resynchronised: no record shows them, so they continue from our own previous tick | the pawn's and the power jump's state code and timers (jump-release damping, a charging power jump, a running zoom); "sprint after landing"; the move-input lock; the gun's weapon state and timers; the boots' boost; the walk bob; the FOV (the recorded one is the cached view FOV); the floor normal and the "has a base" flag while the physics mode agrees (set from the recorded base when the mode has to be changed); the level's objects and Kismet; the placeholder rope grapple of `--placeholder` |
| Left alone | button levels: both sides get the same inputs |
| Options | `--story-mode` states the story mode at the start only; on later ticks the recorded `GroundSpeed` decides wherever it shows it (264, 440, 880), and where it does not (132 in AG-Workshop) the stated mode stays. With `--no-init` the recorded script state is not written on any tick: position, velocity, view and physics mode only |
| Left out | a tick with a teleport or a level-script state change (3.6): its sample is missing from our trace and the next tick starts from the recording after it (unless `--level-events ignore`). On a hand-made level replayed with fixed ticks the level's clock (its movers) falls one tick behind per tick left out |

When nothing differs, the resynchronisation writes back what is already there: a one-step replay of a recording
made by our own simulation is that recording bit for bit, on every tick (tested with fixed and with variable
frame lengths, through walking, sprinting, jumps and a grapple flight). The replay's `one-step:` notes say how
many ticks it ran and how often it had to change more than values (the physics mode, an attachment created or
released, the anchor moved).

That the mode also **reports** a difference truthfully is tested with "originals" that differ from our
simulation in a known way (`tools/asamu-trace/tests/one_step_truth.rs`; the expected numbers come from the
rules, not from a replay):

- an original that falls under another gravity (a test-only copy of the parameters) is off by
  `2·(g − g')·dt` in vertical velocity and `(g − g')·dt²` in height on every falling tick, with `dt` that tick's
  own frame length (NATIVE_PHYSICS.md 4.3 and 4.6), and by nothing sideways; free-running, the same difference
  piles up. Forced to ticks of another length, most ticks miss the prediction, and `compare` says that the frame
  lengths differ;
- a `JumpZ` that a level script changes for one record only is used by the tick that starts from that record
  and by no other (so the recorded state is neither written a tick late nor a tick early);
- a button held in the start sample gives the first tick its release edge; without it, the `state check:` note
  names the tick;
- a recording whose inputs are paired with the states one tick late is wrong at the ticks where the input
  changes, and only there.

**Ticks that start inside our collision.** The recording's positions are the original's, on the original's
collision. Where ours has geometry the original does not have there, the resynchronised pawn starts the tick
stuck in or pushed out of it, which is a collision difference and not one tick of our movement rules. Every
such tick is counted and listed (`one-step: warning: N of M tick(s) start inside our collision … Tick(s) …`; the
library returns all of them), by the same test as the start position's (the pawn's shape at the recorded
position against our collision as it was when the replay started). On the recordings of 2026-10-10: 185 of the
3,000 ticks of the Workshop walk (from tick 1798), and 5 of 150 in the flight through the blocking volume that
is switched off in the original (P5), where the largest one-step horizontal error is 18.2 uu with those ticks
and 6.1 uu without. The test does not separate every collision difference: a walking tick that starts above or
below our floor is moved onto it by our floor check (up to the check's reach in one tick), and a move may meet a
step of ours where the original has a ramp. The largest one-step errors of the Workshop walk (11.7 uu and
678 uu/s horizontally, 22.3 uu vertically, on the stairs) are of that kind and are not overlap ticks. Until the
collision agrees (P1 to P5), one-step numbers of walking ticks are numbers about the collision too.

**What `compare` reports**, for either mode ([PARITY_FINDINGS.md](PARITY_FINDINGS.md) N8):

- per field as before (count, max and its tick, mean, RMS, first tick over the tolerance), and under the
  position and the velocity their **horizontal** part (length of the X/Y difference) and **vertical** part
  (|ΔZ|, with the range of the signed difference `b − a`). The first tick at which a part alone exceeds its
  field's tolerance is listed too (`position_horizontal`, `position_vertical`, `velocity_horizontal`,
  `velocity_vertical` in the summary's `first_exceedance`); the verdict is decided by the 3-D number as before;
- yaw and pitch also in **rotator units** (65536 per turn; the original keeps its view angles as integers of
  that unit, so 1 unit is its smallest step, and the tolerance line shows the angle tolerance in both: 0.001 rad
  is 10.43 units);
- whether the FOV counted for the verdict (3.3, "The FOV column");
- the mode (`mode: one-step …`) and the replay's own notes (validity, what a one-step replay resynchronised, the
  start notes, the state checks, warnings), which the summary JSON carries as `harness_notes` and
  `asamu-trace report` lists under its table. The report's table has the horizontal and vertical cells, the
  angles in degrees and rotator units, and a "Mode" column.

## 4. Recording on this Mac (Route A): step by step

**Who does what (owner's decision, 2026-10-10).** Automation is preferred for all terminal, debugger and
analysis work: an agent may launch the original game, attach LLDB read-only, run the recorder, convert, replay,
compare and update the docs. The person at the Mac does only two things: answers macOS authorisation prompts
(password / Developer Tools / Privacy & Security), and plays when a scenario needs gameplay input. The
original game's files are read-only throughout. Single player; Steam only has to be running.

### 4.0 Launching on modern macOS (session of 2026-10-10, macOS 26.6 on Apple silicon)

| Finding | Evidence | Confidence |
|---|---|---|
| The build runs under Rosetta 2 **only with `-ONETHREAD`**. Without it the rendering thread crashes at once: `FFullScreenMovieGFx::Tick` → `FOpenGLDynamicRHI::Clear` → `glClear` faults at address 0 (no current GL context on that thread). | three crash reports with that stack; with `-ONETHREAD` the menu and levels run (~58 fps in the menu) | CONFIRMED |
| A map can be started directly by passing its name as the first argument (`AG-Darkcave` loads into gameplay with the level's own Kismet state: 3 grapples, story mode off). | observed; `asamu-rec check` reported the map, `ASAMUPlayerController`, `ASAMUPawn`, `max_grapples 3` | CONFIRMED |
| The bundled SDL2 (hg-9168, 2014) has no mapping for the PS5 DualSense; `SDL_GAMECONTROLLERCONFIG` with GUID `4c05000000000000e60c000000000000` (that SDL's format for vendor 054c / product 0ce6) and the PS4 button layout makes it a game controller. | a probe program linked against the game's own SDL: `isGameController` 0 → 1; the owner confirmed the pad works in game | CONFIRMED |
| Sound crackles with the default OpenAL Soft 1.15.1 settings (output over AirPlay at 44.1 kHz); `ALSOFT_CONF` with `frequency=44100`, `resampler=linear`, larger buffers is applied by the launcher. | owner's report before; not explicitly re-confirmed after | TENTATIVE |
| Every `process attach` raises a macOS authorisation prompt while Developer Mode is off; attach **once per game session** and keep the driver attached. | four attaches, four prompts | CONFIRMED |

`tools/trace-recorder/launch_original.sh [MAP]` applies all of the above (fullscreen at the main display's
resolution by default, `ASAMU_WINDOWED=1` for a 1280×720 window) and prints the game's pid.
`tools/trace-recorder/asamu_drive.py` is a headless LLDB driver: it attaches, loads the recorder and executes
commands written to a control file (`rec <args>`, `recrun <args>`, `lldb <cmd>`, `interrupt`, `continue`,
`detach`), so a session can be scripted. In LLDB's async mode a breakpoint's script callback only runs when its
stop event is taken off the listener, so the driver must drain events immediately: the first version polled
every 0.2 s and froze the game during a recording (0 frames recorded). The loop was rewritten to block on the
listener; **the rewritten loop has not been run against the live game yet**.

**Feasibility gate so far (4.4):** F1 pass (with `-ONETHREAD`), F2 pass (`GIsBenchmarking=True`,
`GFixedDeltaTime` = 1/60), F3 pass (map, controller, pawn, location, physics 1, base actor, view rotation, FOV
camera 90 / controller 85, grapple-gun state, 64 bindings, `layout sentinels: ok` — this upgrades E19 and E20),
F4–F6 not yet measured (the only recording attempt hit the driver bug above). The session was ended at the
owner's request; the Windows route (section 6) is being prepared because the owner can play natively there.

The manual steps below remain valid and are what the scripts automate.

### 4.1 One-time setup

1. Rosetta 2 (`softwareupdate --install-rosetta`) and the Xcode command line tools (`xcode-select --install`;
   gives `lldb`).
2. Let Terminal debug other processes: System Settings › Privacy & Security › Developer Tools › enable Terminal.
   The first attach also shows a macOS authorisation dialog; you type your own password there. Leave SIP and code
   signing alone; the game is not hardened (E1), so nothing else is needed.
3. Back up your saves: `~/Library/Application Support/A Story About My Uncle/ASAMU/Saves/`
   ([SAVE.md](reverse-engineering/SAVE.md)). Steam Cloud may restore earlier saves on the first launch.
4. Build and run the static checks once: `cargo run -p asamu-trace -- check-recorder` must print `0 failed`.
5. Optional but recommended: set the display to 60 Hz (System Settings › Displays › Refresh rate) and enable
   V-Sync in the game's video options, so game time runs at real speed between recordings (TENTATIVE: benchmark
   mode may not throttle on its own, E5).

### 4.2 Launch options

Steam › Library › *A Story About My Uncle* › Properties › General › Launch options:

```text
-BENCHMARK -FPS=60 -WINDOWED -ResX=1280 -ResY=720
```

`-BENCHMARK -FPS=60` gives every frame the same 1/60 s step (E5), so stopping the game at the breakpoint does not
change the trace and the trace replays in our 60 Hz simulation without resampling. `-WINDOWED -ResX/-ResY` make
switching between the game and Terminal easy (strings present, E8; effect TENTATIVE). Without `-BENCHMARK` the
recording still works, with variable frame lengths (`tick_rate: null`, replay then needs `--tick-rate`).

### 4.3 Attach

From the repository root in Terminal:

```text
$ lldb
(lldb) process attach --name ASAMU --waitfor
```

Now press Play in Steam (attaching to the process Steam starts avoids the self-relaunch of E9). When LLDB reports
the attach, the game is stopped:

```text
(lldb) command script import tools/trace-recorder/asamu_lldb.py
(lldb) asamu-rec check
(lldb) continue
```

`asamu-rec check` must show 10 symbols resolved, `FName::Names[0]='None'`, and, with the launch options above,
`GIsBenchmarking=True GFixedDeltaTime=0.0166...`. In the main menu it says `player: not sampled (no-player)`.
If LLDB stops on a signal while the game runs, tell it to pass that signal through
(`process handle -p true -s false -n false <SIGNAL>`) and `continue`.

### 4.4 Feasibility gate (first session; record the outcome in docs/STATUS.md)

| # | Check | How | Expected |
|---|---|---|---|
| F1 | The game runs under Rosetta 2 with LLDB attached | 4.3 | menu and levels work |
| F2 | Fixed step active (E5) | `asamu-rec check` | `GIsBenchmarking=True`, `GFixedDeltaTime` ≈ 1/60 |
| F3 | Live object walk and layout | load a level, stand still, `process interrupt`, `asamu-rec check` | map name, `ASAMUPlayerController` / `ASAMUPawn`, plausible location, `layout sentinels: ok` (upgrades E19 and E20) |
| F4 | Recording cost | a 10 s free recording (4.5) | the recorder keeps the game at real speed; note frames skipped/errors from `asamu-rec status` |
| F5 | Input timing (E16) | record: stand 1 s, press and hold W | the first sample with `move_forward = 1` is the first tick whose velocity changes |
| F6 | Grapple state | record one grapple attach and release | `grapple_state` attached exactly while `Physics` is 4 (no "disagree" note) |

If F1 fails, use Route B (section 6). If F3 fails on the sentinels, see 2.2.

### 4.5 Recording one scenario

1. In the game: load the map/checkpoint of the scenario (section 5), stand still on flat ground, release all
   keys, look level.
2. In Terminal: `process interrupt` (or Ctrl-C), then for example

   ```text
   (lldb) asamu-rec start --scenario T1 --frames 600 --launch-options "-BENCHMARK -FPS=60 -WINDOWED -ResX=1280 -ResY=720"
   (lldb) continue
   ```

3. Switch to the game. Wait at least 30 frames (0.5 s) without input, perform the scenario, then stand still for
   at least 30 frames after landing.
4. The recording ends by itself after `--frames` recorded frames, or when you create the STOP file
   (`touch research/local/traces/STOP` in a second Terminal), or with `process interrupt` + `asamu-rec stop`.
   It prints the raw file and the converted trace(s); the game keeps running at full speed afterwards.
5. Repeat each scenario three times. One recording per run keeps files small and segments clean.

Options of `asamu-rec start`: `--out DIR` (default `research/local/traces`, relative to the folder LLDB was
started in), `--frames N`, `--pace X` (in fixed-step mode hold the game to X × real time while recording;
default 1, 0 = off), `--note TEXT`, `--ignore-sentinels` (never for real data), `--continue`. `start` refuses an
output folder inside the game install (the folder holding the `.app`). `asamu-rec status` shows counters at any
time. When a recording ends by itself, the conversion to canonical traces runs inside the breakpoint callback,
so the game pauses for about a second per few thousand frames. When finished: `detach`, then quit LLDB (or just
quit the game).

### 4.6 From recording to parity numbers

```sh
T=research/local/traces
cargo run -p asamu-trace -- validate $T/*.raw.jsonl                       # records, segments, bindings
cargo run -p asamu-trace -- convert $T/<run>.raw.jsonl [--list] [--segment N]   # (the recorder already did this)
cargo run -p asamu-trace -- starts $T/<run>.trace.jsonl [--events]              # where a replay may start (3.5)
# Replay on the converted map with its Kismet (ASAMU_CONVERTED_DIR = asamu-import output), or on the graybox
# for flat-ground scenarios:
cargo run -p asamu-trace -- replay $T/<run>.trace.jsonl --out $T/<run>.replay.jsonl \
    --converted "$ASAMU_CONVERTED_DIR" --kismet --compare --json $T/<run>.summary.json \
    --tol-position 1 --tol-velocity 10 --tol-angle 0.001 --tol-fov 0.1 --tol-anchor 1
cargo run -p asamu-trace -- compare $T/<run>.trace.jsonl $T/<run>.replay.jsonl --tol-position 1 --fail-on-divergence
cargo run -p asamu-trace -- replay $T/<run>.trace.jsonl --out $T/<run>.from300.jsonl --from-tick 300 --ticks 60 --compare
# One tick of our rules at a time, every tick restarted from the recording (3.7):
cargo run -p asamu-trace -- replay $T/<run>.trace.jsonl --out $T/<run>.onestep.jsonl --converted "$ASAMU_CONVERTED_DIR" \
    --from-tick 300 --ticks 60 --one-step --compare --json $T/<run>.onestep.summary.json
cargo run -p asamu-trace -- report $T/*.summary.json --out $T/parity.md      # paste into docs/PARITY.md
```

`--from-tick`/`--ticks` restart from the original's state at a standing-still tick (segment replays, 3.5) to
separate local error from accumulated drift; `--one-step` goes all the way and restarts every tick (3.7). A
replay of an original recording ends before the first respawn teleport or level-script state change inside it
and says so (`validity:` note; `--level-events inject` takes the recorded change and runs on under an
`injected:` note, `ignore` runs through with a warning: 3.6). Read the replay's `start:` notes and warnings
before its numbers: a start that overlaps our collision, or a first tick that moves our pawn onto another floor
height, decides the vertical columns by itself (3.5). With `--kismet` the map's Kismet always starts from level start (also for a segment
replay; the replay's notes say so), its level-start actions may override the recorded start state, and
a map without a Kismet export replays without Kismet (also stated in the notes).

`compare` (also behind `replay --compare`) gives every position and velocity error as a 3-D number, a
horizontal and a vertical part, and the angles in radians and rotator units (3.7). The FOV of a recording whose
FOV column is the camera's cached view FOV on every sample does not count for the verdict (`--fov-verdict auto`,
the default; `count` or `exclude` to state it), because that field cannot show the zoom (3.3); its numbers are
still listed and no tolerance is changed for it. A one-step replay lists the ticks that start inside our
collision (`one-step: warning:`); read its numbers with and without them (3.7). Tolerances must be finite and
non-negative (use a large value such as `1e30` to ignore a field; an infinity cannot be stored in the summary
JSON). The tolerances above are a starting point for analysis, not properties of the game; state the ones used
next to any published number, and never use one tolerance for a free-running and a one-step comparison (a
one-step error is one tick's; [PARITY_FINDINGS.md](PARITY_FINDINGS.md) section 5 has the measured noise of
each). Every divergence becomes a hypothesis, then evidence (RE or a
targeted trace), then a fix, with the "measured" column of PARITY.md updated.

## 5. Scenarios

From the test plans in [GRAPPLE.md](reverse-engineering/GRAPPLE.md) §18 (T-series) and
[ABILITIES.md](reverse-engineering/ABILITIES.md) §18 (A-series). Maps: walking, jumping and sprinting work on any
flat ground (AG-ParadiseCave onwards; AG-Workshop is in story mode with a slow walk, use it only for A20);
the grapple is enabled with limit 3 from AG-Darkcave's start (AG-ParadiseCave enables it by a touch volume);
rocket boots are granted at AG-IceCave's start and enabled during AG-StarHaven ([LEVELS.md](reverse-engineering/LEVELS.md)).
Keys are the defaults (W/A/S/D, Space, Left Shift, left mouse = grapple, right mouse = power jump, E = use); the
recorder reads the actual bindings.

| Id | Do | `--frames` | Checks |
|---|---|---:|---|
| A1 | hold W 2 s, release; then W + Left Shift 2 s, release Shift | 400 | acceleration 2048, caps 440 / 880, braking ([NATIVE_PHYSICS.md](reverse-engineering/NATIVE_PHYSICS.md) §2) |
| A2 | sprint, jump, keep sprint held, land | 300 | `GroundSpeed` 440 in the air, 880 after landing |
| A3 | full jump (hold Space until landing) | 200 | `V.z` 1000 at take-off, apex ≈ 481 uu |
| A4-x | jump released after x = 0 / 0.1 / 0.2 / 0.4 s (one recording each) | 200 | ×0.7 damping per 0.1 s while rising; apex ≈ 121 / 198 / 266 / 371 uu (PARITY deviation 17) |
| A6 | first jump after level start, then a jump after a landing | 300 | air acceleration limit 614.4 vs 716.8 uu/s² |
| A9 | power jump: hold right mouse 0.5 s, release; then 0.7 s | 300 | cancel vs `V.z` 1600 |
| A10 | power leap at full sprint | 300 | horizontal ≈ 1760, `V.z` 750 |
| A12/A13 | fall 0.5 s, tap Space (boost); then hold Space ≈ 1.2 s | 400 | boost timeline, damping during the boost |
| A20 | AG-Workshop story mode: hold the zoom (right mouse) | 120 | FOV 90 → 50 in 19 one-frame steps |
| A21 | walk up stairs | 200 | eye height dip and recovery (raw `eye_height`; v1 samples carry only position, view and FOV) |
| T1 | stand 1000 uu from a grapple-able wall, aim at eye height, hold the grapple button until release | 300 | flying on the press frame, ≈ 2000 uu/s, release below 200 uu with halved speed |
| T3 | as T1, release the button halfway | 300 | release keeps the last post-pull velocity |
| T4 | fall above 2500 uu/s, then grapple | 400 | speed ≤ 2000 in the attach frame |
| T5 | aim at targets 4999 and 5001 uu away | 300 | first attaches, second fails |
| T6 | grapple sideways while running fast | 300 | curved path, no pendulum |
| T8 | limit 2: two grapples in the air, a third press, land | 400 | third fails; count 0 after landing |
| T9/T10 | grapple a charged / uncharged recharge crystal; a glow flower | 300 | 0.05 s hang, release, budget change |
| T14 | re-press less than 0.1 s after a release | 200 | second grapple at the next 0.1 s refire check |
| T16 | hold Space while attached; tap Space after detaching while rising | 300 | no jump while attached; damping after |
| T19 | move the mouse continuously across a release | 200 | one controller tick with unchanged rotation |
| R-level | one start-to-checkpoint route per level | as needed | accumulated drift, regression baseline (M12) |

Each scenario starts standing still with 30 neutral frames and ends 30 frames after landing: a replay starts
from a standing-still sample (position, velocity, view, walking, plus the recorded script state of 3.5; what no
record shows starts fresh). In a longer recording every standing-still moment is such a start
(`asamu-trace starts`), so free play with pauses gives many scenarios.

## 6. Route B: Windows build (read-only polling, no debugger)

The owner plays the Windows build natively; everything else is automated from the analysis Mac over SSH. The
recorder on the game machine is one Python script that only **reads** the game's memory: no debugger, nothing
installed, nothing written to the process. The raw format, `asamu-trace validate`/`convert` and everything
after them are the ones of section 3.

**Status (2026-10-10).** Recorder 0.1.0 made the first three recordings of the original (6.9; the feasibility
gate is in 6.5). Recorder **0.2.0** adds what those recordings showed to be missing (6.10: the camera's
field-of-view lock, the level of the base actor, the floor normal, the view bob, the collision cylinder, the
weapon's state and timers), a log of every dropped frame with its reason, and markers. It is tested on the fake
32-bit image and against the stand-in process on the game machine (6.6) and deployed there; **the optional
fields have not been read from the game yet** (it was not running at any check): the first `check` of the next
session shows them.

### 6.1 Components

| Path | What it is |
|---|---|
| `tools/trace-recorder/asamu_win.py` | Windows front end: `check`, `start`, `mark`, `status`, `stop`, and `v1view` (any OS: a copy of a recording without its optional fields). Stock CPython 3.8+ with `ctypes` (3.13 on the game machine); imports on macOS/Linux for the tests. Opens the game once, with `PROCESS_VM_READ \| PROCESS_QUERY_INFORMATION` only, and reads with `ReadProcessMemory`; that handle is the only one to the game (W1). |
| `tools/trace-recorder/asamu_recorder_core.py` | The same core as the Mac recorder: layout lookups, object walk, sentinels, raw format, converter. Pointer size, array, string and name-entry shapes come from the layout file. |
| `tools/trace-recorder/layout_win_x86.json` | The Win32 layout: 4-byte pointers, RVAs of the globals, 64 field offsets, sentinels ([DEFAULTS.md](reverse-engineering/DEFAULTS.md) §9, [WINDOWS_BINARY.md](reverse-engineering/WINDOWS_BINARY.md)). |
| `docs/reverse-engineering/data/win32/recorder_optional_win32.json` | The optional fields (6.10): 25 offsets in eight groups (a ninth, `base_level`, needs none), two native structures, three sentinels. Generated with the layout; `deploy` copies it to `data\win32` of the working folder. Without it the recorder writes version-1 records plus `base_level`. |
| `tools/trace-recorder/win_remote.sh` | Mac-side helper: `deploy`, `selftest`, `game`, `check`, `start`, `session`, `mark`, `status`, `stop`, `fetch` (finished recordings only), `layoutcheck`, each one SSH call with `powershell -EncodedCommand`. |
| `tools/ghidra-scripts/win32/win32_live_check.py`, `win32_props_check.py` | The two live layout checkers (read-only like the recorder; [WINDOWS_BINARY.md](reverse-engineering/WINDOWS_BINARY.md) §8). `win_remote.sh layoutcheck` runs them against the running game and keeps their whole output under `research/local/win/live-checks/`. |
| `tools/trace-recorder/test_win_glue.py` | Tests on a fake 32-bit image (any OS) and, with `--live-fake` on Windows, against a stand-in process through the real API. |

### 6.2 Evidence (Windows build, Steam build 1822049, read only)

| # | Fact | Confidence |
|---|---|---|
| W1 | PE32 image with `DYNAMICBASE`: every address is module base + RVA, the base is read per process, **through the recorder's own read-only handle**: `NtQueryInformationProcess(ProcessWow64Information)` gives the 32-bit environment block of the process, whose `ImageBaseAddress` (block + 8) is the executable's base; the fallback is `EnumProcessModulesEx(LIST_MODULES_32BIT)`, which reads the module list through the same handle. A candidate counts only if an executable header is readable there. No Toolhelp module snapshot is taken: the system would make it with a second handle of its own, whose access rights the recorder neither chooses nor sees. After `OpenProcess` the recorder asks the system which access the handle got (`NtQueryObject`) and closes it unused if that is more than read + query; `check` prints the mask (0x1410 = the two rights asked for plus the limited-query right Windows adds). Then it compares the header's time stamp (1494838235) and image size (44654592) at the base with the layout and refuses another build. | CONFIRMED for the file facts; CONFIRMED on the game machine for the search (a 32-bit system process: both routes give the same base, the mask is 0x1410; 6.6); that the block's field is at +8 is the stock 32-bit layout (STRONG), checked on every use by the header test |
| W2 | RVAs of `GEngine`, `GWorld`, `GFrameCounter`, `GDeltaTime`, `GFixedDeltaTime`, `GIsBenchmarking`, `GUseFixedTimeStep`, `FName::Names`; `GCurrentTime` (RVA 0x0264F9B8, from `data/win32/globals.json`, used only in fixed-step mode). | CONFIRMED statically (WINDOWS_BINARY.md §4; `GCurrentTime` STRONG); the live check is W-F2 |
| W3 | **Frame order**, the same as E15: `appUpdateTimeAndHandleMaxTickRate` → `UGameEngine::Tick` { `Client->Tick`, `UObject::StaticTick`, one `UWorld::Tick` } → `GFrameCounter += 1` → end-of-frame work → message pump. `UWorld::Tick` stores `WorldInfo.RealTimeSeconds` before any actor ticks. | CONFIRMED (WINDOWS_BINARY.md §6) |
| W4 | **Key and button messages are only queued by the window procedure.** The viewport's message handler (RVA 0x0144FFB0) sends `WM_KEYDOWN`/`WM_KEYUP`/`WM_SYSKEYDOWN`/`WM_SYSKEYUP` (0x100, 0x101, 0x104, 0x105), `WM_CHAR`/`WM_SYSCHAR`, the mouse button messages 0x201–0x209 and 0x20B–0x20D, and focus/close to the function at RVA 0x0144A4A0 and returns; that function reads five key states with `GetKeyState` (virtual keys 0xA2, 0xA3, 0xA0, 0xA1, 0x12) and appends a 36-byte element to the array at client+0x1D0. `UWindowsClient::Tick` (RVA 0x014536D0) begins by calling RVA 0x01451E80, which hands each queued element to RVA 0x014508D0 and empties the array; then it ticks the viewports and calls RVA 0x0144B9E0. | CONFIRMED (disassembly: the queue, its five `GetKeyState` calls, the replay loop, the call order); the names `DeferMessage` / `ProcessDeferredMessages` / `ProcessInput` are stock UE3's (STRONG); that RVA 0x0144B9E0 reads mouse movement and pads was not traced (STRONG) |
| W5 | Hence **all input reaches the game inside `Client->Tick`**: nothing between the `GFrameCounter` increment and the next `Client->Tick` changes `PressedKeys` or runs a key command, and `PressedKeys` then stays as it is through the whole `UWorld::Tick`. | STRONG (W3 + W4; a script that resets the input during a tick would change it, not examined) |
| W6 | The time update (RVA 0x014A6820): with a variable step `GDeltaTime` is stored after the frame-limit wait, as the last thing (RVA 0x014A6B17); with a fixed step `GDeltaTime = GFixedDeltaTime` (RVA 0x014A68A8) and `GCurrentTime` advances (RVA 0x014A68C2) without any wait. | CONFIRMED (disassembly) |
| W7 | Field offsets: the Win32 layout rules reproduce 1,652 of 1,652 native class sizes and 77 of 77 offsets shown by instructions (66 of 66 on the independent route); 29 of the recorder's 64 fields are shown by an instruction, the script-only classes are rule-derived. Of the 25 optional offsets (6.10) 11 are shown by an instruction, 3 are rows of a native class, 11 are rule-derived. | CONFIRMED rules, STRONG script-class offsets (DEFAULTS.md §9); W-F3 upgrades them |
| W8 | **The `RealTimeSeconds` store is not the first thing `UWorld::Tick` does.** Between the function's entry (RVA 0x00635450) and the store (RVA 0x00635841) it makes 22 calls, among them an indirect call (RVA 0x00635574) in its loop over `GEngine`'s local players (+0x4B4) and one `UWorld::IsPaused` call; the Mac function has the same shape (21 calls before its store at 0x100910B71: a virtual call on each local player's controller, `FParticleDataManager::Clear`, the demo and net-client hooks, `UActorComponent::BeginDeferredReattach`). `TickActors` comes after the store in both. So "no actor has ticked" holds from the counter increment to the store, but "nothing has run" holds only up to the next frame's time update: it is the late marker (6.3), not the `RealTimeSeconds` rule, that keeps a sample clear of the input dispatch and of this code. | CONFIRMED (call lists of both functions up to the store; what the Windows calls are was not resolved beyond `GetWorldInfo` and `IsPaused`); that none of them changes a field the recorder reads is UNKNOWN |

Reproduce W4, W6 and W8 (`$EXE` = a local copy of `Binaries/Win32/ASAMU-Win32-Shipping.exe`; image base
0x00400000; output is instructions to look at, nothing to commit):

```sh
D="objdump -d --no-show-raw-insn --x86-asm-syntax=intel"
$D --start-address=0x18536D0 --stop-address=0x1853790 "$EXE"   # UWindowsClient::Tick: first call, viewport loop, third call
$D --start-address=0x1851E80 --stop-address=0x1851FB0 "$EXE"   # the replay loop over the array at +0x1D0 (36-byte elements)
$D --start-address=0x184A4A0 --stop-address=0x184A538 "$EXE"   # the queueing function: five GetKeyState calls, append at +0x1D0
$D --start-address=0x1850022 --stop-address=0x1850470 "$EXE" | grep -E 'cmp|jmp|call.*0x184a4a0'   # message switch
# (0x100/0x101 by compare to 0x1850292; the other messages through the byte-indexed jump tables at 0x1850794,
#  0x185085C and 0x1850888, whose targets 0x1850292, 0x1850451 and 0x1850056 each call 0x184A4A0)
$D --start-address=0x18A6820 --stop-address=0x18A6B50 "$EXE" | grep -E '0x29a7e70|0x2a4f9b8'       # GDeltaTime, GCurrentTime
$D --start-address=0xA35450 --stop-address=0xA35900 "$EXE" | grep -E 'call|\+ 0x4(1c|24|28|30)\]'  # W8: calls before the store to +0x428
```

In the variable-step path `GCurrentTime` is stored first thing (VA 0x018A68F4, before the frame-limit wait, and
again while waiting), `GDeltaTime` last (W6). That is why the late marker is `GDeltaTime` there: `GCurrentTime`
would call every sample late that was read after the message pump.

### 6.3 Sampling point and frame alignment

```text
FEngineLoop::Tick
  appUpdateTimeAndHandleMaxTickRate   waits for the frame limit, stores GDeltaTime           (W6)
  UGameEngine::Tick
    Client->Tick                      applies the queued key/button messages                 (W4)
    UWorld::Tick                      stores WorldInfo.RealTimeSeconds, then ticks all actors (W3)
  GFrameCounter += 1                  ── the window opens: the frame's state is final ──
  end-of-frame work, message pump     (key messages are only queued)
```

Every actor tick lies between the `RealTimeSeconds` store and the counter increment (W3). From the increment to
the next store, **the window**, the world holds the finished frame's state. Per frame the recorder

1. spins on `GFrameCounter` and `RealTimeSeconds` (two small reads per turn). When the counter changes, it reads
   every object span in **one burst**: the spans the previous frame's sample used, merged per object (16 or 17
   reads, about 50 µs on the game machine, in the stand-in test). Then it reads the counter, `RealTimeSeconds`
   and the late marker again and parses the record from the burst.
2. keeps the sample only if the counter advanced by exactly one, `RealTimeSeconds` (in the burst and after it)
   still has the value the recorder saw while the finished frame was ticking, **and** the late marker has not
   moved. Then neither the next tick nor the next frame's time update (after which the input dispatch and the
   first part of `UWorld::Tick` run, W8) had started when the last byte was read, so the sample is the finished
   frame's state and nothing else.
3. keeps polling `RealTimeSeconds`. When it moves forward, once, the next `UWorld::Tick` has started and that
   frame's input is in place (W5): the recorder reads `PressedKeys`, `bPressedJump` and `GDeltaTime` (the tick's
   `DeltaSeconds` argument, as `dt_arg`), checks that the counter has not moved, and writes the record. A key
   array that cannot be read or cannot be a key list (a negative count, more than 64 entries, a name that does
   not resolve) is read again while the tick lasts; it is never taken as "no keys".

What is dropped, and counted in `status.json` and `<recording>.stats.json`:

| Counter | Meaning | Effect |
|---|---|---|
| `torn` | `RealTimeSeconds` changed before the burst was complete: a tick started while reading | the record is not written; a one-frame gap splits the run |
| `unconfirmed` | the finished frame's tick was not seen (start-up, after a missed frame, new `WorldInfo`) | not written |
| `late` | `GDeltaTime` changed (fixed step: `GCurrentTime`): the next frame's time update had run, so its input dispatch may have too | not written; `--keep-late` keeps it |
| `missed` | the counter advanced by more than one between two polls | the frames in between were never seen |
| `no_input` | a record written without its frame's keys: its tick was not seen, its keys never read, or the recording ended first. The sample after it is never confirmed, so it is always the last record of its run | the keys of a run's last record are never used |
| `resyncs` | frames in which `RealTimeSeconds` went back, or changed a second time: the world was reset or replaced at the same address (a level loaded again), or a tick start had been misjudged | that frame's sample is not confirmed (it counts under `unconfirmed` too) and a record waiting for its keys ends its run; the next frame starts clean |
| `input_retries` | failed reads of a frame's keys at its tick start | read again while the tick lasts; a record that never gets its keys counts under `no_input` |

A failed read inside the window is retried once; a sentinel mismatch counts only on a sample read in a window.

**Every dropped frame is logged with its reason** (`stats.drops` of `status.json` and of the stats file; recorder
0.2.0). An entry has the frame number the record has or would have had (`frame`, `last`, `count`: consecutive
frames of one reason share an entry, and a stretch without a player, a menu, is one entry), a `reason` (`missed`,
`skipped`, `unconfirmed`, `torn`, `late`, or `no-input` for a record written without its input) and a `detail`:
which marker moved during a torn read; why a sample could not be confirmed (the counter advanced by more than
one, the frame's input could not be read, the `WorldInfo` changed, or a resync with what `RealTimeSeconds` did:
"did not move forward" or "changed a second time in one frame", with both values); for `skipped` the sampler's
word (`no-player`, `paused`, ...). The log holds 2,000 entries (`drops_omitted` counts what did not fit). A test
checks on every scripted interleaving that each frame missing from a recording is in the log and that no logged
frame has a record (6.6). The first recordings had no such log: their eight single-frame gaps inside a map
(frames 33306 and 33345 of DC1; 43298, 53931, 54499, 74164, 81480 and 83006 of PLAY2, whose stats count 10
`resyncs`) cannot be attributed after the fact.

**What a level change does.** A new `WorldInfo` at a new address makes the next sample unconfirmed. A level
loaded again can leave the new `WorldInfo` and pawn at the old addresses, with the same map name (not observed
on this game; the engine's allocator reuses freed blocks of a size, so it has to be expected): then
`RealTimeSeconds` starts again below its old value, which is a `resyncs` frame or a value that does not fit
(`torn`), and the load frame is not recorded. Either way a gap ends the run, so no trace joins two worlds; `convert` also ends
a run where `WorldInfo.TimeSeconds` goes back (section 3.3 does not say so yet), which is what protects a
breakpoint recording, where a load inside one frame leaves no gap. While the old world is being torn down the
recorder may still read its freed objects: such reads fail or give values that the rules above reject, and what
they could reach is the keys of the run's last record, which are never used. When the game exits, the recording
ends with what it has (`the game exited`).

**Names and key bindings.** The name table is a growing array that the engine moves when it grows, while an
entry stays where it is (stock UE3; STRONG for this build, where `FName::Names` is a `{data, count, max}`
array whose data pointer and count the code reads, as the layout file's `native_evidence` shows). Names are
cached by index and every cache miss reads the table's address again, so a name first needed after a level load
is not read through freed memory. The key bindings written to a recording's header
are read once more in the window of the first accepted sample and kept only if the window was still open
afterwards (`bindings_confirmed` in the stats file). If no window in the first 60 accepted samples is long
enough (four reads per binding; the Mac session found 64 bindings), the table the first look found is kept and
the header says so; if a later window then shows a different table, the recording is marked failed.

**Alignment.** Record `R` holds the state at the end of frame `R.frame − 1`, `WorldInfo.DeltaSeconds` = the
(dilated) length of that frame, and the keys and `dt_arg` of frame `R.frame`: the same as a Mac record (3.1), so
`convert` is unchanged and `validate` cross-checks `DeltaSeconds = clamp(dt_arg × TimeDilation, 0.0005, 0.4)`.
Frame lengths vary on Windows (no benchmark mode needed), so traces get `tick_rate: null` and replay with each
sample's own length.

| | Mac (breakpoint) | Windows (polling) |
|---|---|---|
| State read | at `UWorld::Tick` entry, the process stopped | in the window after the previous frame's increment, the process running |
| What a key command changed at once (during the dispatch) | already in the record with the key | in the next record (the state is read before the dispatch) |
| `pressed_jump` | exact | read early in the tick: a press the controller has already consumed is missed; jump presses come from the key edge anyway |
| `dt_arg` | `xmm0` | `(float)GDeltaTime`, read during the tick |
| Header | `recorder: asamu_lldb`, layout `mac-x86_64-…` | `recorder: asamu_win 0.1.0`, layout `win-x86-steam-1822049`, `game_build: steam-1822049-win32`, note `platform: win-x86` (raw v1 has no platform field) |

Which commands change state at once was not examined (TENTATIVE that the grapple's start is one); the difference
is at most one record at a key press and never mixes two frames.

**What it needs from the game.** The window has to be longer than a burst (in the stand-in runs of 6.6: 49 to
64 µs mean, 0.09 to 0.13 ms at the 99th percentile, 0.18 ms at worst). The engine's frame-limit wait lies inside the window
(W6: it comes before `Client->Tick`), so a limited game waits there for most of the frame (14.2 ms of 16.1 in the
stand-in). Running uncapped at several hundred frames per second leaves no window: the stand-in at 524 frames
per second gave one record per run (four runs of 4,441 to 13,617 frames; nearly all others torn), none wrong. Whether this game's settings limit
the frame rate, and where its game thread waits with V-Sync, is UNKNOWN until W-F4. `-BENCHMARK` removes the
frame-limit wait (W6): do not use it with this recorder unless a probe shows a usable window. `check` prints the
frame rate and the counters of a 120-frame probe before anything is recorded.

**What the late marker cannot see.** With a variable step the marker is `GDeltaTime`, which the time update
stores after the frame-limit wait. Should two consecutive frames have the same `GDeltaTime` to the last bit, a
sample read after that store and before the tick's `RealTimeSeconds` store would not be called late. The burst
follows the counter change within microseconds and the wait lasts milliseconds, so this needs the recorder to
lose the processor for the whole wait and then land in the short stretch before the tick, in a frame whose
length repeats exactly. Not observed. Repeats themselves are not rare: `check` counts the probed frames whose
`dt_arg` equals the previous frame's (37 of 120 in the stand-in, which paces its frames by spinning to an exact
time; compared as the `float` the tick gets, so an upper bound for the `double` the marker is). How often this
game repeats a frame length is UNKNOWN until W-F2. With a fixed step and no `GCurrentTime` address there is no
marker at all, and the recording's header says so.

**Cost.** The recorder spins on one core while a player exists (96 to 100% of one of the game machine's 24
logical cores in the stand-in test) and sleeps in menus. It raises its own process to above-normal priority
(`--priority normal` to leave it); it never touches the game's priority. `--cpu-saver` sleeps through most of
the wait between frames (36% of a core, 311 records without a gap in the stand-in test) at the risk of a missed
frame when a sleep overruns.

### 6.4 Procedure

The owner does two things: starts the game through Steam, and plays. Everything else runs from the repository
root on the Mac. `ASAMU_WIN_HOST` is the SSH host of the game machine (Windows OpenSSH server, key
authentication, Python 3 with the `py` launcher; nothing else is needed there).

```sh
export ASAMU_WIN_HOST=<ssh host of the game machine>
R=tools/trace-recorder/win_remote.sh
$R deploy              # copies 4 files (+ globals.json and the optional layout into data\win32) to
                       # %USERPROFILE%\asamu-trace, compares SHA-256
$R selftest --live     # the tests there, then the real API path against a stand-in process (no game needed)
```

`fetch --validate` builds and runs `asamu-trace` with `cargo run`: set `CARGO_TARGET_DIR` first if the build is
to go anywhere but `target/`.

1. **Owner:** start *A Story About My Uncle* in Steam (no launch options needed; keep the frame limit or
   V-Sync on), load the scenario's level, stand still.
2. `$R game` says whether the game runs; `$R check` must end with `result: ok` and show the module base, the
   build match, `handle: access 0x1410 granted`, `FName::Names[0]='None'`, the map, `ASAMUPlayerController` /
   `ASAMUPawn`, `layout sentinels: ok`, a rising `GFrameCounter` with the frame rate, and a probe with (almost)
   no `torn`, `late`, `missed` or `resyncs` (its last line also says on how many frames `dt_arg` was read and
   how many repeat the frame before, 6.3). In the main menu it says `player: not sampled (no-player)`. In a
   level it also prints the optional fields (6.10): the base actor's level, the floor normal, the cylinder, the
   three field-of-view values, the camera's view, the bob, the gun's state and timers, and a last line
   `optional fields on: …; not in the layout: none; off: none`. A group named under `off` failed its check and
   is not recorded; that does not fail `check` and does not stop a recording.
3. `$R start <scenario> <frames>`, for example `$R start A1 400`. The recorder runs detached on the game machine
   and waits for a player. **Owner:** wait about a second without input, perform the scenario (section 5), stand
   still for a second.
4. The recording ends after `<frames>` records, or `--seconds S` after the first record, or with `$R stop`, or
   when the game exits; `$R status` shows the counters at any time. If the game was not running, `start` fails
   at once (`--wait-game S` makes the recorder wait for it instead).
5. `$R fetch --validate` copies finished recordings to `research/local/traces/win/` (a raw file without its
   `.stats.json` is still being written and is left for the next fetch) and runs `asamu-trace validate` and
   `convert` on them; for a recording with optional fields it first writes the version-1 view into `v1/` next
   to it and validates and converts that (6.10). Then replay and compare as in 4.6. A trace with
   `tick_rate: null` is stepped with each sample's own length; the replay's notes state what that mode
   simulates on a converted level (the player and the level objects only, no Kismet: `--kismet` is refused in
   that mode).

**One recording for a whole session, with markers** (keyboard and mouse; section 6 of
[PARITY_FINDINGS.md](PARITY_FINDINGS.md) lists the scenarios). The owner plays; whoever sits at the Mac marks
where each scenario begins:

```sh
$R session KM1 keyboard and mouse    # before or after the game is started; no frame limit
$R mark workshop-zoom-hold           # just before each scenario; any words
$R status                            # counters, and every marker with its frame
$R stop && $R fetch --validate
```

`session` is `start --detach` without a frame limit: the recorder waits for the game (up to
`ASAMU_WIN_SESSION_S` seconds, default 5400, which is also the longest recording), sits out menus and loading
screens, and writes the note into the header. `mark` writes a request file into the control folder of the
game machine; the recorder's file thread picks it up within a quarter of a second and gives it the number of
the last recorded frame. The marker is in `status.json`, in the stats file (`markers`: name, frame, record
count, UTC time) and in `recorder.log`; the raw file is not touched, and nothing is sent to the game (a test
checks that the command opens no process). A marker is as exact as the moment the command was typed: it says
where to look, not on which frame a key went down.

Further arguments of `$R start` go to `asamu_win.py start`: `--seconds S`, `--wait-game S` (start the recorder
first, the game later), `--timeout S` (give up after this long, default 1800), `--note TEXT`,
`"--launch-options=-WINDOWED"` (recorded only; write it with `=`), `--keep-late`, `--cpu-saver`, `--convert`,
`--ignore-sentinels` (never for real data), `--raw-v1` (no optional fields), `--optional-layout FILE`,
`--out DIR`, `--ctl DIR`. Files on the game machine:
`%USERPROFILE%\asamu-trace\traces\<UTC time>-<scenario>.raw.jsonl` and `.stats.json`, and in `traces\ctl`
`status.json`, `recorder.log`, the `STOP` file and the `MARK-*` requests. The recorder can also be run by hand
there: `py -3 asamu_win.py check`.

Safety on the game machine (section 9 applies): the game's files and process are never written. The one handle
to the game has read + query access (W1); no debugger is attached, no thread is suspended, no privilege is
enabled, and none of the calls that could change another process appears in the script (a test scans its source
for them). The recorder refuses an output or control folder inside the running game's install
(`<install>\Binaries\Win32\…` → `<install>`) and inside any copy of the game it finds on disk above that folder
(a folder holding `Binaries\Win32\ASAMU-Win32-Shipping.exe`, whatever it is called), also when the game is not
running yet; with the game running and its location unknown it writes nothing. No software is installed and no
system setting is changed; the only process it starts is itself, and the only priority it changes is its own.

### 6.5 Feasibility gate for Windows (first session with the game; record the outcome in docs/STATUS.md)

| # | Check | How | Expected | Status 2026-10-10 |
|---|---|---|---|---|
| W-F1 | Read-only access from the SSH session to the game in the desktop session; module base; build match | `$R check`, first lines | pid, base, time stamp and image size match; `handle: access 0x1410 granted` | **Passed on the game** (2026-10-10: STATUS.md records that the build and module checks passed, and the three recordings of 6.9 were made; the `check` output was not kept). Stand-ins: the base of a 32-bit `SysWOW64` process by both routes of W1, access mask 0x1410, a read-only handle across sessions, `ReadProcessMemory` on the stand-in |
| W-F2 | Globals (W2) | `$R check` | `FName::Names[0]='None'`, `GFrameCounter` rising at the frame rate, `GIsBenchmarking=False`, plausible `GDeltaTime` | **Passed** as far as the recordings show: `benchmarking: false` in all three headers, frame numbers rising by one at 59.65 to 60.0 frames per second, `DeltaSeconds` = the clamped tick argument on every frame (17,307 checked, STATUS.md) |
| W-F3 | Live object walk and layout (W7, E19, E20) | in a level: `$R check` | map, `ASAMUPlayerController` / `ASAMUPawn`, plausible location, bindings read, `layout sentinels: ok` | **Passed**: three recordings in four maps with `ASAMUPlayerController` / `ASAMUPawn`, 64 bindings, no sentinel mismatch (`skipped` holds only `no-player` and `paused`); the live property check matched every loaded property offset (WINDOWS_BINARY.md §8) |
| W-F4 | Sampling quality and frame lengths | `$R start idle --seconds 5` standing still; `$R fetch --validate` | records ≈ 5 × frame rate, `torn + late + missed + resyncs` ≈ 0, `bindings_confirmed: true` in the stats file, `DeltaSeconds = clamp(tick argument × TimeDilation)` on every frame | **Passed** (stats files of the three recordings): WS1 3,001 records, 0 torn, 0 late, 0 missed, 0 resyncs, 0 gaps; DC1 14,311 records, 1 torn, 1 late, 3 resyncs, 3 gaps (one at a level change); PLAY2 27,925 records, 0 torn, 0 late, 10 resyncs, 8 gaps (one of 15,597 frames: the stats count 15,872 paused frames skipped; one at a level change); `bindings_confirmed: true` in all three; burst 69 to 75 µs mean with 15 reads (median), window 15.0 to 15.7 ms mean. The single-frame gaps of DC1 and PLAY2 were not attributed (6.3: the drop log is new). Stand-in at 62 frames per second: 312 of 313 frames recorded in 5 s, 0 torn, 0 late, 0 missed |
| W-F5 | Input timing (W5) | record: stand 1 s, press and hold W | the first sample with `move_forward = 1` is the first whose velocity changes; note any `check:` line of the converted trace | **Passed** ([PARITY_FINDINGS.md](PARITY_FINDINGS.md) §3: controller move and pawn physics in the same frame on 91 of 91 walk starts, measured with a gamepad from the acceleration). With keys it is exact on the fake image; a keyboard recording is still to come |
| W-F6 | Grapple state | record one grapple attach and release | `grapple_state` attached exactly while `Physics` is 4, or one sample apart at the press (6.3) | **Passed** (PARITY_FINDINGS.md §3: 0 of 45,237 records with `grappling` and `Physics` 4 disagreeing) |
| W-F7 | Optional fields (6.10; recorder 0.2.0) | in a level: `$R check`, then the first `$R session` | `optional fields on:` all twelve members, `off: none`; in the stats file `optional.off` and `optional.left_out` empty; `fov_locked` true while the zoom key is held | Not run on the game. On the fake image and the stand-in process: every member exact on every record (6.6) |

If W-F1 fails with access denied, run `py -3 asamu_win.py check` in a terminal of the desktop session and
compare. If W-F3 fails on the sentinels, see 2.2 (recompute with `--target win32`). If W-F4 shows more than a
few torn or late samples, turn V-Sync on; if that is not enough, use the debugger route (6.7).

### 6.6 What is verified without the game (2026-10-10)

| Check | Command | Result |
|---|---|---|
| Front end on a fake 32-bit image at a relocated base (4-byte pointers, UTF-16 names and strings, globals at base + RVA), the engine's frame order played one event per memory read: variable frame lengths, every start time of a tick against a burst (256 cases), late samples with and without a marker, missed frames, menu, new pawn, pause, new map, a level loaded again at the old addresses at every point of the frame (20 cases), keys that cannot be read for a moment or for a whole tick, key bindings confirmed in a window, failing reads, sentinels, limits, STOP, output guards (also by the files of a game copy on disk), commands, and a source scan for any call that changes a process or takes a second handle | `python3 -I tools/trace-recorder/test_win_glue.py`; also run by `cargo test -p asamu-trace --test python_crosscheck` | ok, 15 tests, 76,464 checks; Python 3.9, 3.13 and 3.14 on the Mac, 3.13 on the game machine (76,463 there). No written record ever mixes two frames or two worlds |
| Core sampler and converter with the Windows layout (also: a name table that moved, a world clock that goes back) | `python3 -I tools/trace-recorder/asamu_recorder_core.py selftest --layout tools/trace-recorder/layout_win_x86.json` | ok (25 reads per frame before merging) |
| Mac front end unchanged | `python3 -I tools/trace-recorder/test_lldb_glue.py` | ok |
| A Windows raw file through the Rust tools | `asamu-trace validate`, `convert` on a recording of the fake image | accepted unchanged; `DeltaSeconds` cross-check 59 of 59; the Python converter writes the same trace |
| Real API path on the game machine: the executable's base in a 32-bit process by both routes of W1 and in a 64-bit one, the granted access mask, `check`, detached `start`, `status`, `stop`, a 300-frame recording of a stand-in process at 62 frames per second compared record by record with the stand-in's own log, and the same stand-in uncapped | `$R selftest --live` (run four times after the review) | ok: 300 of 300 records, 0 gaps, 0 torn, 0 late, 0 missed, 0 resyncs, burst 50 µs mean (p99 0.10 ms, max 0.13 to 0.18 ms); mask 0x1410; uncapped at 524 frames per second 1 record per run (of 4,441 to 13,617 frames), exact; the 120-frame probe counted 37 frames whose `dt_arg` repeats the previous frame's |
| Remote flow from the Mac against the stand-in: `deploy`, `check`, `start` (survives the SSH session), a `fetch` while recording (left alone), `status`, `fetch --validate`, then `asamu-trace replay` with per-sample frame lengths on the graybox and on converted AG-Workshop | `win_remote.sh`, `asamu-trace` | ok: 312 records in 5 s, 1 segment, `DeltaSeconds` cross-check 311 of 311, inputs and frame lengths of the replay agree on 311 of 311 ticks (the stand-in's motion is not the game's, so positions differ by design) |
| With the game not running: `check`, `start`, `status`, `stop` | `win_remote.sh` | `… is not running` (exit 2), a failed start that creates no recording, a one-line status |
| Read-only handle from the SSH session (session 0) to a process of the desktop session | one `OpenProcess` with the recorder's access mask, no memory read | granted |
| Per-sample replay = fixed-tick replay when all frames have the same length | `cargo test -p asamu-trace` (`equal_frame_lengths_replay_exactly_like_fixed_ticks_at_any_rate`, `equal_lengths_replay_like_the_fixed_tick`); with `ASAMU_CONVERTED_DIR` also `--test real_data` | identical samples at 24, 30, 50, 60, 62, 75, 120, 144 and 240 Hz on the graybox, from level start and from a tick in the middle, and for a converted fixed-step recording; identical at 30, 60 and 144 Hz on converted AG-Workshop, AG-Darkcave, AG-ParadiseCave, AG-IceCave, AG-StarHaven, AG-BeautifulCity and AG-Epilogue (150 ticks each, no run dies; `TheCore` does not load: no player start) |

**Recorder 0.2.0 (2026-10-10, after the first recordings).**

| Check | Command | Result |
|---|---|---|
| The same front-end tests, plus: the optional fields on an image that has what they lead to (a camera with a field-of-view lock and a cached view, a floor normal, the bob fields, a cylinder component, a state frame with two states, two timers, a second actor of the floor actor's name in another level), each member compared with the engine's own log on every record; sentinel and class checks that fail, values that cannot be right, unreadable memory (a group goes, the record stays); the optional layout file against the core's lookups, against the rule-derived layout and against the instructions of the recorder layout; files that must be refused; the dropped-frame log against the frames missing from the recording on every scripted interleaving (torn 256 cases, late, missed, naps, menu, pause, new map, reload 20 cases, unreadable keys) and on 16 seeded random ones (random windows and delays, a reader that is not scheduled now and then: 217 torn, 16 late, 218 missed frames, 270 records without input, every optional member still exact); markers set by `mark` while a recording runs; `--raw-v1` and `v1view`; the source scan also as an allow-list (the two system libraries, every function taken from them, every access right named, where the game is opened, nothing of the kind in the shared core) | `python3 -I tools/trace-recorder/test_win_glue.py`; also run by `cargo test -p asamu-trace --test python_crosscheck` | ok, 20 tests, 101,420 checks; Python 3.9, 3.13 and 3.14 on the Mac, 3.13 on the game machine (101,373 there: it has no rule-derived layout to compare with) |
| Old recordings and old layouts | the three recordings of 6.9 through `core.v1_record` and `asamu-trace validate`; a layout without the optional part; the Mac layout | 45,237 of 45,237 records are their own version-1 view; all three validate; without the optional part a recording has `base_level` only and its header says which groups are missing; the Mac recorder's records are unchanged (`test_lldb_glue.py` ok) |
| A recording with optional fields through the Rust tools | `asamu-trace validate` on a 70-record recording of the fake image and on its `v1view` | the recording itself is refused (`unknown field optional_fields`: the raw reader of `asamu-trace` accepts no member it does not know); its view validates, `DeltaSeconds` cross-check 69 of 69, and both Python conversions give the same samples |
| `asamu-trace check-recorder` with the regenerated layout | `cargo run -p asamu-trace -- check-recorder` (a local copy of the Windows executable present) | 1,635 checks, 0 failed (1,573 before): the 14 new evidence entries are checked against the rule-derived rows, the executable's bytes and the functions their anchors name |
| Real API path on the game machine with the optional fields | `$R deploy`, `$R selftest --live` (four runs) | ok: 300 of 300 records, all twelve optional members exact on each; 0 torn in three runs, 2 torn and 1 gap in one; burst 68 to 73 µs mean with 23.7 reads (16 to 17 reads and 50 µs without them), window 14.0 to 14.2 ms; a marker set through `mark` was taken by the detached recorder and is in its stats file and log; uncapped at about 500 frames per second 1 to 6 records per run, exact. Verification, four more runs: three ok (300 of 300, 0 torn, 0 late, burst 66 to 72 µs); one made while a compiler job of another task ran on that machine lost 20 of 321 frames (19 torn, 1 late: the stand-in was not scheduled in time), each of them in the drop log and no record wrong, and failed the test's limit of 6 lost frames. The limit is a quiet-machine figure: record on a machine that does nothing else |

Not verified (needs the running game): W-F7, and how long the burst is on the game itself with the optional
reads (the first recordings measured 69 to 75 µs with 15 reads in a window of 15 ms).

### 6.7 Fallback and rejected routes

- **Debugger with one hardware breakpoint** (`DebugActiveProcess`, `Dr0` on `UWorld::Tick` at base + 0x00635450,
  `ecx` = world, `[esp+8]` = `DeltaSeconds`, WOW64 thread contexts): the exact Mac sample point, and the route to
  take if the window is too short on some machine. Not built; the game imports `IsDebuggerPresent` and its
  behaviour under a debugger is UNKNOWN (WINDOWS_BINARY.md §9).
- **Rejected:** DLL injection, code hooks, patched files, suspending the game's threads.

### 6.8 Review of 2026-10-10 (before the first session with the game)

A second pass over the recorder, its tests and the replay path, with the game still not running. Found and
fixed:

| Finding | Was | Now |
|---|---|---|
| A level loaded again with its `WorldInfo` at the old address made the poller take the reset of `RealTimeSeconds` for a tick start. | Depending on when in the frame it happened: either every later frame counted as torn and nothing more was recorded (reproduced: 65 torn frames in a row), or one run continued across the load with the old world's last record next to the new world's first. | `resyncs` (6.3): the load frame is dropped, the run ends, recording goes on; 20 placements of the reload tested, each against the engine's own log. |
| The executable's base came from a Toolhelp module snapshot. | The system takes that snapshot with a handle of its own, so "one read-only handle" described the recorder's handle only. | W1: the base is read through the recorder's handle; the granted access mask is checked and printed. |
| A failed read of a frame's keys at its tick start. | The record was written with the keys the window sample had (the previous frame's) and its run went on. | Read again while the tick lasts; otherwise the record ends its run (`no_input`). |
| The name table's address was read once per session. | After the table grew and moved, a name not seen before was read through freed memory. | Read again on every cache miss (both recorders). |
| The header's key bindings came from the first look at the game, at an arbitrary moment. | A table caught while a level was being set up would have been used for the whole recording. | Confirmed in a window (6.3). |
| Output folders were checked against the running game's path, and by folder name when it was not running. | A detached start that waits for the game could create its control folder inside a copy of the game under another name. | Also checked against the files on disk (6.4). |
| `fetch` copied every raw file it did not have. | A file fetched while it was being written was kept as if complete. | Only recordings with their `.stats.json` are fetched. |
| `convert` joined consecutive frames of one map and pawn number. | A breakpoint recording across a reload at the old addresses would have been one run. | A run ends where `WorldInfo.TimeSeconds` goes back (Rust and Python, cross-checked). |

Confirmed as reported: the frame order and the input queue (W3, W4, W6: re-read from the disassembly), the
alignment of a record with a Mac record, the access mask of the recorder's handle, the stand-in numbers, and
variable-rate replay (per-sample steps equal fixed ticks when the frames are equally long, 6.6).

Residual risks, none of which can be closed without the game: everything in 6.5; the equal-bits case of the late
marker (6.3); whether the code before the `RealTimeSeconds` store touches a recorded field (W8, covered by the
late marker); `pressed_jump` (6.3 table); a level left running for days: `RealTimeSeconds` is a 32-bit float,
and once a frame's length is below half its spacing (after about 36 hours at 144 frames per second, later at
lower rates) the store no longer changes it and no tick start is seen (nothing is recorded; nothing wrong is
written).

### 6.9 The first recordings (2026-10-10): conversion and replay

Three recordings were made on the Windows build, with a gamepad, at about 60 frames per second with variable
frame lengths: `WS1` (AG-Workshop, 3,001 records), `DC1` (AG-ParadiseCave then AG-BeautifulCity, 14,311) and
`PLAY2` (AG-BeautifulCity then AG-Darkcave, 27,925). They stay under `research/local/traces/win/`. What they
changed in conversion and replay:

| Finding | Consequence |
|---|---|
| With a gamepad the recorded `PlayerInput` axes are 0 on every record and the stick is no key, so the first conversions had no move input at all | the move axes are derived from the pawn's acceleration (3.4) |
| The helper actor's location is not the anchor for about half of the attached records | the anchor is `vGrappleLocation` (3.3) |
| A replay from the middle of a level started with level-start script state | the `state:` note and the standing-still start rule (3.5) |
| The recorded map name follows the name the level was opened with (`ag-workshop`) | `replay --converted` finds the converted level ignoring case |
| `GroundSpeed` 132 throughout AG-Workshop, 264 in story mode elsewhere | the recorded speed is applied as it is; story mode there has to be stated (`--story-mode on`) |

The reconverted traces are in `research/local/traces/win/v2/{WS1,DC1,PLAY2}/` (14 traces; the earlier ones next
to the raw files were left as they are and have neither move input nor the right anchors). First replays on
the converted levels, each with its samples' own frame lengths (no Kismet), tolerances position 1 uu, velocity
10 uu/s, angle 0.001 rad, FOV 0.1°, anchor 1 uu:

| Replay | Trace, start tick, ticks | First exceeded (tick: error) | Maxima (position uu / velocity uu/s) | Flags |
|---|---|---|---|---|
| WS1, whole (`--story-mode on`) | `WS1/…trace.jsonl`, 0, 3000 | position 1515: 1.0; velocity 1563: 119.7; FOV 1881: 2.1 | 646.7 / 333.4; FOV 40.0 | grounded differs on 77 ticks (first 2001) |
| walk | `DC1/…seg0`, 1010, 68 | position 1015: 2.875 | 3.48 / 0.000034 | — |
| sprint | `PLAY2/…seg1`, 7627, 114 | position 7628: 2.2; velocity 7675: 149.5 | 13.07 / 213.9 | grounded differs on 11 ticks (first 7692) |
| jump | `PLAY2/…seg4`, 3014, 96 | position 3015: 1.55; velocity 3076: 33.8 | 5.24 / 33.8 | — |
| power jump | `PLAY2/…seg5`, 1750, 155 | position 1811: 1.0 | 1.96 / 2.79 | — |
| grapple, released with the button held | `PLAY2/…seg5`, 6920, 150 | position 6921: 4.7; anchor 6950: 5.7; velocity 7027: 12.3 | 283.9 / 977.8 | attached on the same 80 ticks |
| grapple, released by the button | `PLAY2/…seg4`, 3110, 150 | position 3111: 3.1; anchor 3112: 4.5; velocity 3145: 348.5 | 1279.6 / 1739.1 | attached on the same 114 ticks |
| jump, then a grapple released with the button held | `DC1/…seg0`, 3987, 60 | position 3988: 3.3; anchor 4015: 8.1; velocity 4018: 13.4 | 4.22 / 38.0 | attached on the same 17 ticks |

In every replay the inputs agree on all ticks, the frame lengths agree, view angles stay within 0.000003 rad
and every verdict is "diverged": these are the first measurements, not parity (the analysis belongs in
[PARITY.md](PARITY.md)). What they show about the harness: the replay now moves, accelerates, jumps and
grapples when the original does, and the grapple attaches and lets go on the same ticks. Two things recur and
are the simulation's (or the converted collision's), not the harness's: in all eight replays our pawn drops
at the first replayed tick, by 0.73 to 4.68 uu depending on the place, and stays lower than the original's
(purely vertical: the position column's first entry is that offset in every row); and in AG-BeautifulCity at
DC1 segment 3, tick 742, our pawn does not move at all where the original walks (with or without the recorded
start state).

**What the harness still cannot reproduce.**

- Per-sample replays on a converted level simulate the player and the level objects only: no Kismet, touch
  volumes, checkpoints, kill zones, deaths and respawns, falling rocks, level streaming, NPCs or Matinee movers
  (6.4, step 5). A segment that crosses a trigger (story mode switching on or off, the grapple budget changing,
  a respawn) is right only up to it; `asamu-trace starts` shows the recorded state at every standing stretch,
  which is where such a change becomes visible.
- Script state no record shows (3.5): starts are limited to standing-still ticks for that reason. Recordings
  with long stretches without a standstill (PLAY2 segment 7 has one in 1,525 ticks) give few starts.
- The stick's magnitude and its direction while attached or in the release gap (3.4); whether a zero
  acceleration is "no input" or "input ignored".
- The eye height at the start tick (recorded per frame, not applied): the aim's origin can be off by the eye
  height's smoothing after a landing or on stairs.
- The camera's FOV as recorded stayed at 90 on every record, also while the zoom button was held in
  AG-Workshop: the recorded field cannot show a zoom (PARITY_FINDINGS.md N1). Recorder 0.2.0 reads the lock
  (6.10); whether the original zoomed on those presses is still UNKNOWN.
- `pressed_jump` is read early in the frame (6.3); jump presses come from the button's edge.

### 6.10 Optional fields (recorder 0.2.0)

What the first recordings could not show ([PARITY_FINDINGS.md](PARITY_FINDINGS.md) N1, N5, N9) is read by
recorder 0.2.0 as **optional members** of the raw record. They are in the same burst as everything else, so a
record's optional members are the same finished frame's. The raw version stays 1; a record or a header without
them is a plain version-1 one.

| Member of `player` | Value | Read from (Win32 offsets) | Evidence | Check |
|---|---|---|---|---|
| `base_level` | name of the outermost object of the pawn's `Base` actor: the package of the level it lies in (`AG-BeautifulCity`, `freds_place`); null without a base | `Object.Outer` 0x28 followed to the end, `Object.Name` 0x2C | native_code (main layout) | — |
| `fov_default`, `fov_locked`, `fov_lock` | the camera's `DefaultFOV`, `bLockedFOV`, `LockedFOV` | 0x1D8; 0x1DC bit 0; 0x1E0 | 0x1D8 native_code (`ACamera::AssignViewTarget`); the other two native_layout, bracketed by 0x1D8 and `DefaultAspectRatio` 0x1E8 (native_code, same function) | sentinel `DefaultAspectRatio` = 1.33333 |
| `camera_pov` `{location, rotation}` | `CameraCache.POV.Location` and `.Rotation`: the camera's cached view, which the grapple's aim direction is taken from ([GRAPPLE.md](reverse-engineering/GRAPPLE.md), fire trace); `fov_camera` is the third member of the same structure | 0x384, 0x390 | layout_rule (struct members; `fov_camera` at 0x39C read 90.0 on 45,237 records) | sentinel `FreeCamDistance` (0x438) = 256 |
| `floor` | the pawn's `Floor`: the normal the walking physics keeps (zero until the pawn has walked) | 0x2CC | native_code (`APawn::processLanded` stores it) | per record: all zero, or length 1 within 0.001 |
| `base_eye_height` | the pawn's `BaseEyeHeight` (the native eye-height update reads it and writes `eye_height`) | 0x2C4 | native_code (`AUDKPawn::UpdateEyeHeight`) | a number |
| `walk_bob`; `bob` `{bob, land, jump, applied, time, just_landed, land_recovery}` | `UTPawn.WalkBob` (the aim's origin is `Location` + `EyeHeight` + `WalkBob`); `Bob`, `LandBob`, `JumpBob`, `AppliedBob`, `BobTime`, `bJustLanded`, `bLandRecovery` | 0x6CC; 0x6B8, 0x6BC, 0x6C0, 0x6C4, 0x6C8; 0x628 bits 10 and 11 | layout_rule (script-only class) | sentinel `DoubleJumpEyeHeight` (0x6AC) = 43; only for a pawn of class `ASAMUPawn` |
| `cylinder` `{radius, half_height, translation, collision_component}` | the pawn's `CylinderComponent`: `CollisionRadius`, `CollisionHeight`, `Translation`, and whether it is the actor's `CollisionComponent` | pawn 0x38C, then 0x1DC, 0x1D8, 0x1A0; actor 0x18C | native_code, all five (`APawn::physFalling`, `APawn::processLanded`) | the component's class is `CylinderComponent`; radius and height above 0 |
| `gun.state` | name of the weapon's script state (`Active`, `WeaponFiring`, `WeaponEquipping`, ...); null without a state | `Object.StateFrame` 0x14, the state node at +0x28 of that frame, its `Name` | native_code (`UObject::execGetStateName` does exactly this) | the node's class is `State` |
| `gun.timers` `[{name, rate, count, loop, paused}]` | the weapon's `Timers`. The refire check is the entry named `RefireCheckTimer` (the stock weapon sets a timer of that name when it fires; its period is `FireInterval`, 0.1 s here, GRAPPLE.md); `count` is the time it has run | `Actor.Timers` 0x0A4; 28-byte elements: flags +0 (bit 0 loop, bit 1 paused), name +4, rate +0xC, count +0x10 | native_code (`AActor::GetTimerCount`, `IsTimerActive`, `PauseTimer`; the loop bit: `SetTimer`) | at most 32 entries, every name resolves, numbers only |

Offsets and evidence are in `recorder_optional_win32.json` (generated; the instructions are in
`layout_win_x86.json` under `native_evidence` and `asamu-trace check-recorder` checks them against the
executable); [WINDOWS_BINARY.md](reverse-engineering/WINDOWS_BINARY.md) §13 has the reading. They are **not**
fields of `layout_win_x86.json`: `check-recorder` requires that file's field list to equal the Mac layout's, and
the Mac layout has none of them (its recorder writes version-1 records as before).

**How a recording says what it has.** The header gets `optional_fields`, the sorted names of the members being
written (`player.fov_locked`, `player.gun.timers`, ...), and `optional_layout`, the id of the file the offsets
came from; a version-1 header has neither. A converter can tell from `optional_fields` which field-of-view
members exist. The rule it should then apply is the camera's own: the effective field of view is `fov_lock`
while `fov_locked` is true, else `fov_camera` (PARITY_FINDINGS.md V19; that the lock is what the zoom sets on
this build is still to be seen on the game). No converter reads the optional members yet.

**A group is read whole or not at all.**

- A group whose offsets the layout file lacks is named in a header note and not read.
- A group whose sentinel or class check fails on an object is off for that object (a header note and
  `optional.off` in the stats say why). A check made on a sample that was not confirmed is forgotten and made
  again.
- A value that cannot be right (not a number, a floor normal that is no unit vector, a timer without a name,
  memory that cannot be read) leaves the group out of that one record; `optional.left_out` counts the records
  and keeps the last reason.
- None of this costs a record or a version-1 field, and none of it is fatal: only the 10 sentinels of the main
  layout stop a recording.

**Reading a recording with optional fields.** The raw reader of `asamu-trace` refuses a member it does not
know, so `validate`, `convert` and everything after them need the version-1 view until that reader has the
optional members (`tools/asamu-trace/src/raw.rs`: `optional_fields` and `optional_layout` in the header, the
members of the table above in the player and gun structures, all defaulting to absent):

```sh
python3 -I tools/trace-recorder/asamu_win.py v1view research/local/traces/win/<file>.raw.jsonl
cargo run -p asamu-trace -- validate research/local/traces/win/v1/<file>.raw.jsonl
```

The view keeps the file name, lies in `v1/` next to the recording, has exactly the version-1 members and a
header note that says which optional fields were removed; it is never written over a file. `fetch --validate`
does this by itself. A version-1 recording is its own view (45,237 of 45,237 records of the first three), and
`--raw-v1` records that way from the start. The Python converter (`asamu_recorder_core.py convert`) reads both.

**Two fields that were examined and not added.**

- `pawn_flags.power_jumped` and `pawn_flags.has_released_jump` were false on all 45,237 records. Their offsets
  are right, and the flags are never set in this build: both are bits 1 and 2 of the same word (0x874) as
  `has_jumped` (bit 0), `sprinting` (bit 4) and `is_falling` (bit 8), which behave as the game does (and the
  live check of 2026-10-10 matched every loaded bool mask, WINDOWS_BINARY.md §8); and in the
  whole cooked data the two properties are named by two functions only, the pawn's landing handler and its
  story-state override, and both only clear them (984 functions and states of the `asamu` package scanned; the
  names are in the name table of 1 of 42 packages; neither executable holds them). CONFIRMED. `has_jumped` is
  set by the jump function and never cleared, which is the latch the recordings show. The two members stay in
  the record because version 1 requires them.
- A distance to the floor or to the base: the pawn stores none (no property of `Actor`, `Pawn`, `GamePawn`,
  `UDKPawn`, `UTPawn` or `ASAMUPawn`). The floor check's result is local to the walking physics.

## 7. Other routes (coarse cross-checks only)

`DEMOREC`/`DEMOPLAY` demos store replicated properties at the network rate with quantised vectors (STRONG for
stock UE3); `-EXEC=<file>` runs console commands at start-up, but the console appears compiled out; `DUMPMOVIE`
dumps frames for rendering comparisons. None of them is frame-exact.

## 8. What results upgrade which tracker items

`progress/progress.toml` is changed by the session that has the evidence, never in advance.

| Evidence | Items | Allowed change |
|---|---|---|
| This tooling with its tests (section 1.2) | `trace-capture` | `partial` → `implemented` (tooling exists and passes tests; not yet run on the game). Evidence: `tools/trace-recorder`, `tools/asamu-trace`, this file |
| | `trace-replay` | add `tools/asamu-trace` (replay on converted levels with Kismet, segment replays, compare/report) to its evidence |
| Feasibility gate F1–F6 passed, first original traces exist | `parity-suite` | `blocked` → `investigating`; blocker removed |
| | `trace-capture` | `implemented` → `verified` once a recording replays with matching inputs (F5) and passes the sentinels (F3) |
| A1, A2 within tolerance (3 repetitions each) | `ground-move` (sprint/story speed included) | `implemented` → `verified` |
| A3, A4-x, A6 | `jump`, `air-control`, `gravity` | → `verified` |
| A20, A21 | `fov`, `camera` (view angles and FOV; eye height is not in v1 samples) | → `verified` for the traced scope |
| F5 plus all A-series inputs reproduced | `input-map` (logical actions; mouse scaling stays a documented deviation) | note the evidence |
| T1, T3, T4, T5, T6 | `targeting`, `range`, `pull`, `swing`, `max-speed`, `release` | → `verified` |
| T8, T9, T10, T14, T16 | `attach-rules`, `abilities` (grapple budget, crystals) | → `verified` for the traced scope |
| A9, A10, A12, A13 | `abilities` (power jump, rocket boots) | → `verified` for the traced scope |
| The full T-series within tolerance | milestone `M8` | `partial` → `verified` |
| One route per level within the drift budget, run as a regression suite | milestone `M12`, `parity-suite` | → `implemented`, then `verified` |

"Within tolerance" means `asamu-trace compare` reports `exact` or `within tolerance` with the tolerances stated
in PARITY.md for that scenario. A divergence is a finding, not a failure of the tooling: write it down
(PARITY.md known deviations), then fix or explain it.

## 9. Safety, terms and hygiene

- **Offline single player only.** The game has no online play and ships no anti-cheat (E13). Record in Steam's
  offline mode.
- **Your own licence, launched through Steam.** Do not use `NOSTEAM` or any other path around Steam's licence
  check.
- **Read only.** Never modify the install, its files or configuration; never write process memory, inject code or
  hook functions. The only change to the running process is the breakpoint instruction LLDB places; the recorder
  only reads memory (`SBProcess.ReadMemory` and register reads; it never evaluates expressions or writes memory or
  registers) and disables its breakpoint when a recording ends (`stop` or the next `start` deletes it). It writes
  files only in its output folder and refuses one inside the install. No cheats, no achievement or statistics
  changes. Back up saves first (4.1).
- **macOS protections stay on.** Never disable SIP or code signing checks; grant Developer Tools access to
  Terminal (or the app hosting the agent) only for recording sessions. An agent never types or asks for the
  user's password; the person at the Mac answers the authorisation dialog.
- **Automation boundary.** Agents may launch the game, attach read-only, record and analyse (section 4). The
  owner answers OS authorisation prompts and provides gameplay input. Original game files are never modified;
  launch-time environment variables and command-line options are the only things that differ from a normal run.
- **What may be kept.** Raw recordings and traces are numeric behavioural measurements and contain no addresses
  or pointers. They stay under the git-ignored `research/local/traces/`; only small curated canonical traces may
  be committed as parity fixtures, after review and `repo-hygiene`. Never commit memory dumps, executables,
  decompiled code, screenshots or recordings of the game.

## 10. Next steps

1. Feasibility on the Mac (4.4): F1–F3 passed on 2026-10-10 (4.0); F4–F6 and the first traces are pending,
   either on the Mac with the rewritten driver or on Windows (section 6).
2. First traces: A1, A3, A4-x, T1, T3; replay, compare, publish the measured numbers in PARITY.md with
   `asamu-trace report`.
3. Schema v2 for fields that make mid-trace replays exact (physics mode, base, grapple budget, boots and jump
   damping state, eye height); v1 stays the contract until then. The raw format already records most of them.
4. Route B once Route A works, or at once if the Mac build does not run (F1).
