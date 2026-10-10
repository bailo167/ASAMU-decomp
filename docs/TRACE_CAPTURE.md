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

## 6. Route B: Windows build (read-only polling, no debugger)

The owner plays the Windows build natively; everything else is automated from the analysis Mac over SSH. The
recorder on the game machine is one Python script that only **reads** the game's memory: no debugger, nothing
installed, nothing written to the process. The raw format, `asamu-trace validate`/`convert` and everything
after them are the ones of section 3.

**Status (2026-10-10).** Built, tested, reviewed a second time (6.8) and deployed; **not yet run against the
game**, which was not running on the game machine at any check (the last one after the review). Verified without
it: the logic on a fake 32-bit image with exact interleavings (6.6), the real Windows API path against a stand-in
process on the game machine (300 of 300 frames at 62 frames per second, none torn), and the whole remote flow
from the Mac. The feasibility gate W-F1..W-F6 (6.5) is what the first session with the game has to show.

### 6.1 Components

| Path | What it is |
|---|---|
| `tools/trace-recorder/asamu_win.py` | Windows front end: `check`, `start`, `status`, `stop`. Stock CPython 3.8+ with `ctypes` (3.13 on the game machine); imports on macOS/Linux for the tests. Opens the game once, with `PROCESS_VM_READ \| PROCESS_QUERY_INFORMATION` only, and reads with `ReadProcessMemory`; that handle is the only one to the game (W1). |
| `tools/trace-recorder/asamu_recorder_core.py` | The same core as the Mac recorder: layout lookups, object walk, sentinels, raw format, converter. Pointer size, array, string and name-entry shapes come from the layout file. |
| `tools/trace-recorder/layout_win_x86.json` | The Win32 layout: 4-byte pointers, RVAs of the globals, 64 field offsets, sentinels ([DEFAULTS.md](reverse-engineering/DEFAULTS.md) §9, [WINDOWS_BINARY.md](reverse-engineering/WINDOWS_BINARY.md)). |
| `tools/trace-recorder/win_remote.sh` | Mac-side helper: `deploy`, `selftest`, `game`, `check`, `start`, `status`, `stop`, `fetch` (finished recordings only), each one SSH call with `powershell -EncodedCommand`. |
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
| W7 | Field offsets: the Win32 layout rules reproduce 1,652 of 1,652 native class sizes and 76 of 76 offsets shown by instructions; 29 of the recorder's 64 fields are shown by an instruction, the script-only classes are rule-derived. | CONFIRMED rules, STRONG script-class offsets (DEFAULTS.md §9); W-F3 upgrades them |
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
$R deploy              # copies 4 files (+ globals.json) to %USERPROFILE%\asamu-trace, compares SHA-256
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
   how many repeat the frame before, 6.3). In the main menu it says `player: not sampled (no-player)`.
3. `$R start <scenario> <frames>`, for example `$R start A1 400`. The recorder runs detached on the game machine
   and waits for a player. **Owner:** wait about a second without input, perform the scenario (section 5), stand
   still for a second.
4. The recording ends after `<frames>` records, or `--seconds S` after the first record, or with `$R stop`, or
   when the game exits; `$R status` shows the counters at any time. If the game was not running, `start` fails
   at once (`--wait-game S` makes the recorder wait for it instead).
5. `$R fetch --validate` copies finished recordings to `research/local/traces/win/` (a raw file without its
   `.stats.json` is still being written and is left for the next fetch) and runs `asamu-trace validate` and
   `convert` on them. Then replay and compare as in 4.6. A trace with `tick_rate: null` is stepped with each
   sample's own length; the replay's notes state what that mode simulates on a converted level (the player and
   the level objects only, no Kismet: `--kismet` is refused in that mode).

Further arguments of `$R start` go to `asamu_win.py start`: `--seconds S`, `--wait-game S` (start the recorder
first, the game later), `--timeout S` (give up after this long, default 1800), `--note TEXT`,
`"--launch-options=-WINDOWED"` (recorded only; write it with `=`), `--keep-late`, `--cpu-saver`, `--convert`,
`--ignore-sentinels` (never for real data), `--out DIR`, `--ctl DIR`. Files on the game machine:
`%USERPROFILE%\asamu-trace\traces\<UTC time>-<scenario>.raw.jsonl` and `.stats.json`, and in `traces\ctl`
`status.json`, `recorder.log` and the `STOP` file. The recorder can also be run by hand there:
`py -3 asamu_win.py check`.

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
| W-F1 | Read-only access from the SSH session to the game in the desktop session; module base; build match | `$R check`, first lines | pid, base, time stamp and image size match; `handle: access 0x1410 granted` | Not run on the game. Stand-ins pass: the base of a 32-bit `SysWOW64` process by both routes of W1, access mask 0x1410, a read-only handle across sessions, `ReadProcessMemory` on the stand-in |
| W-F2 | Globals (W2) | `$R check` | `FName::Names[0]='None'`, `GFrameCounter` rising at the frame rate, `GIsBenchmarking=False`, plausible `GDeltaTime` | Not run |
| W-F3 | Live object walk and layout (W7, E19, E20) | in a level: `$R check` | map, `ASAMUPlayerController` / `ASAMUPawn`, plausible location, bindings read, `layout sentinels: ok` | Not run |
| W-F4 | Sampling quality and frame lengths | `$R start idle --seconds 5` standing still; `$R fetch --validate` | records ≈ 5 × frame rate, `torn + late + missed + resyncs` ≈ 0, `bindings_confirmed: true` in the stats file, `DeltaSeconds = clamp(tick argument × TimeDilation)` on every frame | Not run. Stand-in at 62 frames per second: 312 of 313 frames recorded in 5 s (the first is the start-up `unconfirmed`), 0 torn, 0 late, 0 missed, 0 resyncs, bindings confirmed, burst 49 µs mean, 92 µs at the 99th percentile, 95 µs max; `DeltaSeconds` cross-check 311 of 311 |
| W-F5 | Input timing (W5) | record: stand 1 s, press and hold W | the first sample with `move_forward = 1` is the first whose velocity changes; note any `check:` line of the converted trace | Not run. On the fake image: exact |
| W-F6 | Grapple state | record one grapple attach and release | `grapple_state` attached exactly while `Physics` is 4, or one sample apart at the press (6.3) | Not run |

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

Not verified (needs the running game): everything in 6.5.

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
