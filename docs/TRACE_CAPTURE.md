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
| E11 | While the grapple is attached the pawn is in `PHYS_Flying` (4); the anchor is the location of the gun's `GrappleGunHitLocActor`. | CONFIRMED ([GRAPPLE.md](reverse-engineering/GRAPPLE.md) §1, §2); "nothing else flies" STRONG |
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
| `eye_height` | pawn `EyeHeight` (0x378, CONFIRMED offset): the camera height above the centre before bob; not in v1 samples |
| `gun` | `grappling`, `released`, `can_grapple`, `anchor` (helper location), `grapple_location`, `distance`, `times_grappled`, `max_grapples` |
| `pawn_flags`, `boots` | `ASAMUPawn` flags; boots `enabled`, `finished` |

### 3.3 Canonical trace (schema v1, unchanged)

`asamu-trace convert` (Rust, the reference; the recorder's Python converter is tested to agree) writes the format
of `crates/asamu-player/src/trace.rs` ([PARITY.md](PARITY.md#trace-format)) with `source: "original"`:

| Canonical | From the raw records (run `R_0..R_n` → samples `0..n`) |
|---|---|
| sample 0 | state of `R_0`, neutral input, time 0 |
| sample `k ≥ 1`: input | keys of `R_(k−1)` mapped through the recorded bindings: `move_forward`/`move_right` = summed signs of `Axis aBaseY` / `Axis aStrafe` commands (clamped to ±1), `jump_held` (`Jump`), `grapple_held` (`StartFire`), `sprint_held` (`StartSprinting`), `power_jump_held` (`PowerJumpKeyDown`); `jump_pressed` = jump key rising edge against `R_(k−2)` or a rising `bPressedJump` (catches a press and release inside one frame; at `k = 1` there is no `R_(−1)`, so only a set `bPressedJump` counts); `use_pressed` = rising edge of `use`. Aliases are expanded up to depth 8 and at most 256 command parts per key (a hostile table cannot stall the conversion; real tables use a few) |
| look deltas | wrapped change of the controller rotation `R_(k−1) → R_k`, × 2π/65536 — exact and independent of the mouse scaling we have not ported (PARITY deviation 13) |
| state | `R_k`: `position`, `velocity`; `yaw`/`pitch` = signed 16-bit rotator units × 2π/65536; `fov` = camera POV FOV, else `FOVAngle`; `grapple_state` attached ⇔ `bIsGrappling`, anchor = helper location (else `vGrappleLocation`); without gun data (weapon not a `GrappleGun`) no anchor is known, so the sample is idle even in `Physics` 4, and such samples are counted in a `check:` note; `rope_length` null; `grounded` ⇔ `Physics` 1 |
| `time` | Σ `DeltaSeconds` of `R_1..R_k` |
| `tick_rate` | `1 / DeltaSeconds` when every frame of the run had the same length (rounded to an integer within 0.001 Hz), else `null` |
| `notes` | recorder, sample point, launch options, scenario, frame range and segment, input source, cross-check counts (axes vs keys, grapple flag vs `PHYS_Flying`, `PHYS_Flying` without gun data, FOV fallbacks), and `init: {...}`: the initial `physics`, `base`, `max_grapples`, `times_grappled`, `rocket_boots`, `sprinting`, `ground_speed`, `air_control`, `jump_z` (replay applies the grapple and boots fields) |

A run ends at a frame gap, a paused frame, a frame without a player, or a change of map, pawn object or class;
each run of at least two records is one trace (`<stem>.trace.jsonl`, or `<stem>.seg<i>.trace.jsonl`). Without
recorded bindings the converter falls back to the signs of `aBaseY`/`aStrafe` (of `R_k`) and `bPressedJump`, and
says so in the notes.

## 4. Recording on this Mac (Route A): step by step

The person at the Mac does every step below; nothing here may be automated by an agent (it launches the game and
needs the user's own authorisation). Steam offline mode, single player.

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
# Replay on the converted map with its Kismet (ASAMU_CONVERTED_DIR = asamu-import output), or on the graybox
# for flat-ground scenarios:
cargo run -p asamu-trace -- replay $T/<run>.trace.jsonl --out $T/<run>.replay.jsonl \
    --converted "$ASAMU_CONVERTED_DIR" --kismet --compare --json $T/<run>.summary.json \
    --tol-position 1 --tol-velocity 10 --tol-angle 0.001 --tol-fov 0.1 --tol-anchor 1
cargo run -p asamu-trace -- compare $T/<run>.trace.jsonl $T/<run>.replay.jsonl --tol-position 1 --fail-on-divergence
cargo run -p asamu-trace -- replay $T/<run>.trace.jsonl --out $T/<run>.from300.jsonl --from-tick 300 --ticks 60 --compare
cargo run -p asamu-trace -- report $T/*.summary.json --out $T/parity.md      # paste into docs/PARITY.md
```

`--from-tick`/`--ticks` restart from the original's state at any tick (segment replays) to separate local error
from accumulated drift. With `--kismet` the map's Kismet always starts from level start (also for a segment
replay; the replay's notes say so), its level-start actions may override the `init:` grapple and boots state, and
a map without a Kismet export replays without Kismet (also stated in the notes). Tolerances must be finite and
non-negative (use a large value such as `1e30` to ignore a field; an infinity cannot be stored in the summary
JSON). The tolerances above are a starting point for analysis, not properties of the game; state the ones used
next to any published number. Every divergence becomes a hypothesis, then evidence (RE or a
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

Each scenario starts standing still with 30 neutral frames (the replay starts from sample 0's position, velocity,
view and walking/falling, plus the `init:` grapple and boots state; other script state starts fresh) and ends 30
frames after landing.

## 6. Route B: Windows build (plan)

The Windows build needs its own layout file and front end; the raw format, `asamu-trace convert` and everything
after it stay the same.

1. **Get the build** with your own licence (Steam on Windows, or SteamCMD with
   `+@sSteamCmdForcePlatformType windows`). It is 32-bit (E14), compiled with MSVC, probably without symbols
   (TENTATIVE).
2. **Field layout.** Recompute the layout with the DEFAULTS.md method in a Win32 mode: pointers 4/4, dynamic
   arrays and strings 12/4 (`{data, count, max}` at 0/4/8), names 8/4, delegates 12/4, interfaces 8/4, and MSVC's
   rule that a derived class starts after its parent's **padded** size (no Itanium tail-padding reuse). Check the
   class sizes against the sizes the registration code passes to the `UClass` constructor, as for the Mac build.
   `TCHAR` is 2-byte UTF-16 on Windows, so `FString` and wide `FNameEntry` characters are 2 bytes; recheck the
   `FNameEntry` header in the code that reads it.
3. **Addresses.** Find `FName::Names` by its fixed first entries (`None`, `ByteProperty`, `IntProperty`, ...),
   `GEngine` from the code that reads `GamePlayers`, `GFrameCounter` from the main loop, and `UWorld::Tick` from
   `UGameEngine::Tick`'s single call after the client tick (the E15 shape). Store them as offsets from the module
   base (ASLR moves the base only).
4. **Layout file.** Write `tools/trace-recorder/layout_win32_<build>.json` in the same schema (`pointer_size` 4,
   Win32 struct members, module offsets instead of symbol names), with sentinels unchanged (they are values, not
   offsets). `check-recorder` needs two changes for this file: its schema group accepts only `pointer_size` 8
   today, and its binary group reads Mach-O only (a PE reader would be added). The sampler takes pointer reads and
   string character sizes from the layout; its per-object block reads assume 8-byte pointer fields, which only
   over-reads by 4 bytes on Win32 (recheck when the Win32 layout exists).
5. **Front end.** A small Python debugger loop with `ctypes` (no dependency): `DebugActiveProcess`, a hardware
   execute breakpoint in `Dr0` on `UWorld::Tick`, `ReadProcessMemory` on each hit through the same
   `asamu_recorder_core.Sampler` (it reads through any `read(addr, size)` and takes the pointer size from the
   layout), `ContinueDebugEvent`. Hardware breakpoints leave the code untouched. Same launch options if E5 holds
   on Windows (verify).
6. **Rejected:** DLL injection, code hooks, patched files, external polling readers for frame-exact traces (a
   poller cannot see frame boundaries; acceptable only for coarse position checks with `tick_rate: null`).

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
  Terminal only for recording sessions. An agent never types or asks for the user's password; the person at the
  Mac answers the authorisation dialog.
- **Agents do not launch the game.** Recording is a human session; agents prepare, check and analyse.
- **What may be kept.** Raw recordings and traces are numeric behavioural measurements and contain no addresses
  or pointers. They stay under the git-ignored `research/local/traces/`; only small curated canonical traces may
  be committed as parity fixtures, after review and `repo-hygiene`. Never commit memory dumps, executables,
  decompiled code, screenshots or recordings of the game.

## 10. Next steps

1. Feasibility session on the Mac (4.4), run by the user; record the outcome in docs/STATUS.md.
2. First traces: A1, A3, A4-x, T1, T3; replay, compare, publish the measured numbers in PARITY.md with
   `asamu-trace report`.
3. Schema v2 for fields that make mid-trace replays exact (physics mode, base, grapple budget, boots and jump
   damping state, eye height); v1 stays the contract until then. The raw format already records most of them.
4. Route B once Route A works, or at once if the Mac build does not run (F1).
