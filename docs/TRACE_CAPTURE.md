# Trace capture from the original game

How we will record **behavioural traces** from the original *A Story About My Uncle* so the runtime can be
checked against it tick by tick (milestones M8 and M12 in [ROADMAP.md](ROADMAP.md)). A trace is the
`asamu-trace` JSON Lines format already implemented in `crates/asamu-player/src/trace.rs` and documented in
[PARITY.md](PARITY.md#trace-format); the recorder's only job is to produce that format from the original game.

Nothing here has been run against the original yet: **the game was not launched** while writing this plan. Every
fact below carries a confidence label and the command that reproduces it; steps that depend on unverified facts
say so.

## 1. Recommendation

| Route | What | Frame-exact | New RE needed | Verdict |
|---|---|---|---|---|
| **A** | Mac build (the one we analysed) under Rosetta 2, launched through Steam with a **fixed engine time step** (`-BENCHMARK -FPS=60`), sampled by an **LLDB Python script** at the start of every world tick | yes | little: symbols and field offsets are already CONFIRMED for this build | **Primary**, if the game launches on the analysis Mac (feasibility gate, §4.1) |
| **B** | Windows build, sampled by a debugger loop with a hardware breakpoint (B2), or polled by an external read-only memory reader (B1) | B2 yes, B1 no | 32-bit field layout and global/function addresses of the Windows executable | Second; needed if A fails, and to cover the Windows build |
| **C** | Built-in engine facilities (`DEMOREC` demos, `-EXEC=` command files) | no | demo format | Coarse cross-checks only |

The key enabler is the engine's fixed-time-step mode: with every frame's `DeltaTime` fixed, the simulation no longer
depends on wall-clock time, so a debugger may stop the game every frame without changing the trace, and the trace
replays in our fixed-tick simulation (`tick_rate` = the chosen FPS) with no resampling.

## 2. Evidence this plan relies on

All from the Mac depot 278362, build 1822049, `A Story About My Uncle.app/Contents/MacOS/ASAMU`, read-only.

| # | Fact | Confidence |
|---|---|---|
| E1 | The executable is a thin x86_64 Mach-O, **not code-signed** and **not stripped** (no arm64 slice). | CONFIRMED ([INVENTORY.md](reverse-engineering/INVENTORY.md), [BINARY_ANALYSIS.md](reverse-engineering/BINARY_ANALYSIS.md)) |
| E2 | The engine globals `GWorld`, `GEngine`, `GFrameCounter`, `GDeltaTime`, `GCurrentTime`, `GIsBenchmarking`, `GUseFixedTimeStep`, `GFixedDeltaTime`, `FName::Names` and `UObject::GObjObjects`, and the functions `UGameEngine::Tick(float)` and `UWorld::Tick(ELevelTick, float)`, are present as symbols. | CONFIRMED (`nm`, R1) |
| E3 | `GFixedDeltaTime` (in `__DATA` at 0x1022F5068) is a `double` initialised to 1/30 s (the `float` 1/30 widened). | CONFIRMED (data bytes, R2) |
| E4 | `appUpdateTimeAndHandleMaxTickRate()` reads `GIsBenchmarking`, `GUseFixedTimeStep` and `GFixedDeltaTime` and writes `GDeltaTime`; `FEngineLoop::PreInit` references `GIsBenchmarking`; `FEngineLoop::Init` writes `GFixedDeltaTime`. | CONFIRMED (disassembly references, R3) |
| E5 | `-BENCHMARK` sets `GIsBenchmarking`, `-FPS=<n>` sets `GFixedDeltaTime = 1/n`, and while benchmarking every frame's `DeltaTime` is `GFixedDeltaTime`. | STRONG (E4, the wide strings `BENCHMARK` and `FPS=` in the binary, and the stock UE3 design); confirm with the Ghidra xrefs of both strings before relying on it (§9 step 1) |
| E6 | Nothing stores to `GUseFixedTimeStep` (each of its three code references loads it; no data pointer to it exists outside the symbol table), so fixed stepping has to come from `-BENCHMARK`. | CONFIRMED (R3, R3b) |
| E7 | No map's Kismet uses `SeqCond_IsBenchmarking`, so benchmark mode does not change story logic. Other readers of `GIsBenchmarking` (startup movies, `UGameViewportClient::SetDropDetail`/`Exec`, `UEngine::Init`, stat drawing) affect presentation; their exact effects are TENTATIVE. | CONFIRMED for Kismet (local exports of all 12 maps, R4); TENTATIVE for the rest |
| E8 | The command-line words `DEMOREC`, `DEMOPLAY`, `DEMOSTOP`, `FIXEDSEED`, `EXEC=`, `DUMPMOVIE`, `NOSTEAM`, `GAMEINI=`, `ENGINEINI=` and `INPUTINI=` exist as UTF-32 strings; `ALLOWCONSOLE`, `SHOWDEBUG` and `FIXEDSTEP` do not. | CONFIRMED (R5); what each word does is TENTATIVE |
| E9 | `SteamAPI_Init` and `SteamAPI_RestartAppIfNecessary` are imported: started outside Steam, the game may relaunch itself through Steam (which would detach a debugger that started it). | CONFIRMED (import names); behaviour TENTATIVE |
| E10 | Native field offsets of the engine classes on this build (the script property layout reproduces all 1,447 native class sizes): see §3.2. | CONFIRMED ([DEFAULTS.md](reverse-engineering/DEFAULTS.md) §2, `data/defaults/native_layout.json`) |
| E11 | While the grapple is attached the pawn is in `PHYS_Flying` (value 4 of `Actor.Physics`); the anchor is the location of the gun's `GrappleGunHitLocActor` helper. Nothing else in normal play puts the player in `PHYS_Flying` (the stock cheat fly mode is not reachable without a console). | CONFIRMED for flying-while-attached and the helper ([GRAPPLE.md](reverse-engineering/GRAPPLE.md) §1, §2); STRONG for "nothing else" |
| E12 | The camera updates once per frame at the end of `UWorld::Tick`, after every actor tick group. | CONFIRMED (native; [PARITY.md](PARITY.md) deviation 2) |
| E13 | The depot ships no anti-cheat component. | CONFIRMED (inventory file list) |
| E14 | The Windows executable is `Binaries/Win32/ASAMU-Win32-Shipping.exe` (42,971,136 bytes), a 32-bit build; it is not in the Mac depot. | STRONG (the shipped `PCTOC.txt`, [INVENTORY.md](reverse-engineering/INVENTORY.md)) |

Reproduce (paths with `$EXE` = the Mac executable above; output is names and counts only, nothing to commit):

```sh
EXE="$HOME/Library/Application Support/Steam/steamapps/common/A Story About My Uncle/A Story About My Uncle.app/Contents/MacOS/ASAMU"
# R1 symbols
nm "$EXE" | grep -E ' _(GWorld|GEngine|GFrameCounter|GDeltaTime|GCurrentTime|GIsBenchmarking|GUseFixedTimeStep|GFixedDeltaTime)$'
nm "$EXE" | grep -E 'FName5NamesE$|UObject11GObjObjectsE$'
nm "$EXE" | c++filt | grep -E 'UGameEngine::Tick\(float\)$|UWorld::Tick\(ELevelTick, float\)$'
# R2 the default fixed step (file offset = address - 0x1020EC000 + 0x20EC000, see BINARY_ANALYSIS.md)
python3 -c "import struct,sys; f=open(sys.argv[1],'rb'); f.seek(0x22F5068); print(struct.unpack('<d', f.read(8))[0])" "$EXE"
# R3 which functions reference the time-step globals (about 20 s)
objdump -d --no-show-raw-insn "$EXE" | awk '/^[0-9a-f]+ <.*>:$/ {fn=$2} /0x1022f5068|0x1023bdaf0|0x1023cec10/ {print fn}' | c++filt | sort | uniq -c
# R3b the instructions after each reference: loads (`movl (%reg)` / `orl (%reg)`) only; the address appears
# once more in the file, in the symbol table
objdump -d --no-show-raw-insn "$EXE" | grep -A5 '<_GUseFixedTimeStep>$'
python3 -c "import struct,sys; d=open(sys.argv[1],'rb').read(); print(d.count(struct.pack('<Q',0x1023cec10)))" "$EXE"
# R4 Kismet: no SeqCond_IsBenchmarking in any map (local exports from `asamu-inspect kismet`, see KISMET.md)
grep -l SeqCond_IsBenchmarking research/local/kismet/*.kismet.json || echo none
# R5 wide (UTF-32) command-line words
python3 -c "import sys; d=open(sys.argv[1],'rb').read(); [print(w, d.count(w.encode('utf-32-le'))) for w in sys.argv[2:]]" "$EXE" BENCHMARK FPS= DEMOREC FIXEDSEED EXEC= NOSTEAM ALLOWCONSOLE
```

## 3. The trace a recorder must write

### 3.1 Format (schema version 1, exactly as `trace.rs`)

Line 1 is the `TraceMeta` object, every further line one `TraceSample`. Unknown fields are rejected
(`deny_unknown_fields`), so a recorder must not add fields; v1 readers accept any JSON number and round to `f32`.

| Meta field | Value for an original recording |
|---|---|
| `format` | `"asamu-trace"` |
| `schema_version` | `1` |
| `source` | `"original"` |
| `game_build` | e.g. `"steam-1822049-mac"` (Steam build id and platform) |
| `level` | the map file stem, e.g. `"AG-Workshop"` |
| `tick_rate` | the `-FPS=` value (e.g. `60`) when recorded in fixed-step mode; `null` otherwise |
| `units` | `"uu"` |
| `notes` | recorder name and version, launch options, sampling point, frames skipped (if any), the scenario id |

Sample `tick = k` holds the input applied during frame `k` and the state at the **end** of frame `k`.

| Sample field | Source in the original | Conversion |
|---|---|---|
| `tick` | `GFrameCounter` minus its value at the first sample | strictly increasing; a gap means frames were missed (noted in `notes`; replays stop at gaps) |
| `time` | `tick × GFixedDeltaTime` in fixed-step mode, else the sum of `WorldInfo.DeltaSeconds` | seconds, informational |
| `input.move_forward`, `input.move_right` | the move keys held (`PlayerInput.PressedKeys`), cross-checked with the signs of `PlayerInput.aBaseY` / `aStrafe` | each −1, 0 or 1 (the original normalises the acceleration direction, A-WK-1, so only the direction matters) |
| `input.look_yaw_delta`, `input.look_pitch_delta` | difference of the controller's `Rotation` between this sample and the previous one | rotator units × 2π/65536, wrapped to (−π, π]; exact by construction and independent of the mouse scaling we have not ported (PARITY deviation 13) |
| `input.jump_pressed` / `jump_held` | jump key newly held / held (`PressedKeys`); `PlayerController.bPressedJump` as a cross-check for a press and release inside one frame | booleans |
| `input.grapple_held`, `sprint_held`, `power_jump_held` | fire, sprint and power-jump keys held | booleans |
| `input.use_pressed` | use key newly held | boolean |
| `position` | the pawn's `Location` | UU, as stored |
| `velocity` | the pawn's `Velocity` | UU/s, as stored |
| `yaw`, `pitch` | the controller's `Rotation` (`Pitch`, `Yaw`, `Roll` as 32-bit rotator units) | units × 2π/65536, signed (`((u + 32768) mod 65536) − 32768`) |
| `fov` | the camera's cached point of view (`Camera.CameraCache.POV.FOV`) | degrees; expected 90 in normal play (the `FOV` setting) and 50 at full story-mode zoom (`zoomFOV`) |
| `grapple_state`, `grapple_anchor` | `"attached"` while the pawn's `Physics` is 4 (E11), with the `GrappleGunHitLocActor`'s `Location` as anchor; else `"idle"` and `null` | UU |
| `rope_length` | — | always `null` (the original gun has no rope) |
| `grounded` | the pawn's `Physics` is 1 (`PHYS_Walking`) | boolean; that this equals our "on a walkable floor" is TENTATIVE |

The key names behind each action are read from the install's input configuration at record time
(`DefaultInput.ini` bindings), not hard-coded in the recorder.

Illustrative lines (made-up numbers, not a measurement):

```json
{"format":"asamu-trace","schema_version":1,"source":"original","game_build":"steam-1822049-mac","level":"AG-Workshop","tick_rate":60.0,"units":"uu","notes":["asamu_lldb 0.1","-BENCHMARK -FPS=60","sampled at UWorld::Tick entry"]}
{"tick":0,"time":0.0,"input":{"move_forward":0.0,"move_right":0.0,"look_yaw_delta":0.0,"look_pitch_delta":0.0,"jump_pressed":false,"jump_held":false,"grapple_held":false},"position":[100.0,200.0,92.15],"velocity":[0.0,0.0,0.0],"yaw":0.0,"pitch":0.0,"fov":90.0,"grapple_state":"idle","grapple_anchor":null,"rope_length":null,"grounded":true}
```

### 3.2 Where the values live (Mac x86_64 build)

| Object | Field | Offset | Confidence |
|---|---|---|---|
| `Core.Object` | `StateFrame` / `Name` / `Class` | 0x20 / 0x48 / 0x50 | CONFIRMED (E10) |
| `Engine.Actor` | `Location` (3 × f32) / `Rotation` (3 × i32) / `Physics` (byte) / `Velocity` (3 × f32) | 0x80 / 0x8C / 0xC0 / 0x190 | CONFIRMED |
| `Engine.Controller` | `Pawn` | 0x250 | CONFIRMED |
| `Engine.PlayerController` | `PlayerCamera` / `bPressedJump` (word 0x460, bit 1) / `FOVAngle` / `PlayerInput` | 0x450 / 0x460 / 0x4A0 / 0x588 | CONFIRMED |
| `Engine.Input` (base of `PlayerInput`) | `PressedKeys` (`array<name>`) | 0xC0 | CONFIRMED |
| `Engine.PlayerInput` | `aBaseY` / `aForward` / `aTurn` / `aStrafe` / `aLookUp` | 0x19C / 0x1AC / 0x1B0 / 0x1B4 / 0x1BC | CONFIRMED |
| `Engine.Camera` | `CameraCache` (32 bytes) | 0x418 | CONFIRMED |
| `Engine.Camera` | `CameraCache.POV.FOV` | 0x434 (time stamp, then location, rotation, FOV) | TENTATIVE (stock struct order; check against the expected 90) |
| `Engine.WorldInfo` | `TimeDilation` / `TimeSeconds` / `DeltaSeconds` / `Pauser` | 0x530 / 0x538 / 0x544 / 0x550 | CONFIRMED |
| dynamic array / name | `{data pointer, i32 count, i32 max}` (16 bytes) / `{i32 index, i32 number}` (8 bytes) | — | sizes CONFIRMED, member order TENTATIVE (stock UE3) |
| `Engine.Engine.GamePlayers`, `Engine.Player.Actor`, the gun's helper property | — | to compute with the layout example (`asamu-inspect --example gameplay_defaults`, extended to these classes) | not yet computed |

Finding the objects: `GEngine` → `GamePlayers[0]` → `Actor` gives the player controller; its `Pawn`,
`PlayerCamera` and `PlayerInput` give the rest. Fallback that needs no further offsets: walk `GObjObjects` for the
one object of class `ASAMUPlayerController` that is not a class default object (`Default__…`), and for the one
`GrappleGunHitLocActor`. Names resolve through `FName::Names`. Every pointer is re-validated each frame (its `Class`
name must still match); on a mismatch (level change, respawn) the recorder re-resolves and starts a new trace file.

### 3.3 Sampling point

One breakpoint at the entry of `UWorld::Tick` for frame `k` sees:

- the state at the end of frame `k − 1` (physics, rotation, and the camera, which updated at the end of the
  previous world tick, E12), and
- the keys held for frame `k` (the platform's input events are pumped and dispatched to `PlayerInput` before the
  world ticks; TENTATIVE stock UE3 order, to confirm in `UGameEngine::Tick`, §9 step 2).

So sample `k − 1` is complete at the entry of frame `k`: its input was read at the entry of frame `k − 1`, its
state at the entry of frame `k`. A single breakpoint suffices, and with a fixed time step its latency is invisible
to the simulation.

## 4. Route A: Mac build under Rosetta 2 with LLDB

### 4.1 Feasibility gate (first session, with the user present)

1. Rosetta 2 installed (`softwareupdate --install-rosetta`), Xcode command line tools (LLDB) installed.
2. The game starts from Steam on this Mac (macOS 26, Apple silicon) under Rosetta 2 with OpenGL. UNKNOWN.
3. LLDB attaches to the translated process and reads `GFrameCounter` twice with increasing values. UNKNOWN
   (Terminal needs *Developer Tools* access in System Settings > Privacy & Security; the target is not code-signed
   or hardened, E1, so SIP and code signing stay untouched).
4. With the launch options `-BENCHMARK -FPS=60` set in Steam, `GDeltaTime` reads 1/60 every frame (confirms E5).

Record the outcome in `docs/STATUS.md`. If step 2 or 3 fails, go to Route B.

### 4.2 Recording

1. Steam > the game > Properties > Launch options: `-BENCHMARK -FPS=60` (plus `-FIXEDSEED` once its effect is
   known; it may make the engine's random numbers repeatable, which matters for the falling rocks, TENTATIVE).
   Steam offline mode; a separate save slot; a backup of the save folder.
2. `lldb --arch x86_64`, then `process attach --name ASAMU --waitfor`, then start the game from Steam (attaching
   after Steam starts it avoids the relaunch of E9).
3. `command script import tools/trace-recorder/asamu_lldb.py` (to be written; our own code), then
   `asamu-trace start --scenario <id> --out research/local/traces/` and `continue`.
4. The script resolves the symbols of E2, sets the breakpoint of §3.3 with a Python callback that reads about 300
   bytes per frame with `SBProcess.ReadMemory`, appends the finished sample, and returns `False` (continue). It
   writes nothing into the process apart from the breakpoint instruction LLDB places.
5. `asamu-trace stop` closes the file. The script then validates it with the same rules as `Trace::read_jsonl`
   (or `asamu-trace validate`, §8).

Expected cost: one stop per frame. Under a fixed step the game merely runs slower than real time while recording;
measure the achieved frames per second in the first session.

## 5. Route B: Windows build

Get the Windows build with your own licence (Steam on a Windows PC, or SteamCMD with
`+@sSteamCmdForcePlatformType windows`). It needs its own offsets and addresses:

1. **Field layout.** The build is 32-bit (E14) and compiled with MSVC. Recompute the layout of §3.2 with the
   DEFAULTS.md method in a Win32 mode: pointers 4/4, dynamic arrays and strings 12/4, names 8/4, delegates 12/4,
   interfaces 8/4, and MSVC's rule that a derived class starts after its parent's **padded** size (the Itanium
   tail-padding reuse of rule 4 does not apply). Check the result against the class sizes the registration code
   passes to the `UClass` constructor in the Windows executable (the same check that confirmed the Mac layout),
   then live (the pawn's `Location` changes as you walk).
2. **Addresses.** The shipping executable is unlikely to carry symbols (TENTATIVE). Find `FName::Names` by its
   fixed first entries (`None`, `ByteProperty`, `IntProperty`, ...) and `GObjObjects` as the array whose entries'
   classes chain up to `Class`; find `UWorld::Tick` by its string and call-graph neighbours in Ghidra. Store them as
   offsets from the module base (ASLR changes the base, not the offsets).
3. **B2, debugger sampler (recommended).** A small debugger loop (`DebugActiveProcess`, a hardware execute
   breakpoint in `Dr0` on `UWorld::Tick`, `ReadProcessMemory` on each hit, `ContinueDebugEvent`), written by us in
   Python with `ctypes` (no dependency), or WinDbg/x32dbg scripting. Hardware breakpoints leave the code bytes
   untouched. Same sampling point and fixed step as Route A (the Windows build accepts the same command line if E5
   holds there too; verify).
4. **B1, polling reader (coarse).** `OpenProcess(PROCESS_VM_READ | PROCESS_QUERY_LIMITED_INFORMATION)` and periodic
   `ReadProcessMemory`. It cannot see frame boundaries, so a read may mix two frames even when `GFrameCounter` is
   unchanged before and after; use it only for slow checks (positions along a route, `tick_rate: null`).
5. **Rejected:** DLL injection, code hooks or patched files. They change the process or the install, resemble
   cheat tools, and are not needed.

## 6. Route C: built-in facilities

- **Demos** (`DEMOREC` / `DEMOPLAY`, E8): stock UE3 demos store replicated actor properties at the network update
  rate with quantised vectors (STRONG for stock UE3), so they are neither frame-exact nor full precision, and the
  file format would need reverse engineering. Useful at most for coarse route shapes.
- **`-EXEC=<file>`** runs console commands at start-up; `GETALL` / `DISPLAYALL` exist as strings but the console
  itself appears compiled out (`ALLOWCONSOLE` absent). Possible use: dumping a property once to the log to
  cross-check an offset. TENTATIVE.
- **`DUMPMOVIE`** dumps frames: a later tool for rendering comparisons, not for behaviour.

## 7. What to record

Short scenarios tied to the specs, three repetitions each, each starting with the pawn standing still on flat
ground and 30 neutral frames (so the replay's initial state matches), ending 30 frames after landing:

| Scenario | Checks |
|---|---|
| walk forward 2 s, release | acceleration 2048, cap 440, braking ([NATIVE_PHYSICS.md](reverse-engineering/NATIVE_PHYSICS.md) §2) |
| sprint, story-mode walk | 880 and 264 uu/s caps (pawn script layer) |
| jump, released after 0 / 0.1 / 0.2 / 0.4 / 0.7 s | jump damping and apex heights (PARITY deviation 17) |
| walk off a ledge, walk up stairs, onto a slope | falling, step-up, slope slide (NATIVE_PHYSICS.md §3–4) |
| grapple attach, hold to proximity release; release halfway | pull 10⁷/d, 2000 cap, halving at 200 uu ([GRAPPLE.md](reverse-engineering/GRAPPLE.md) T-series) |
| power jump charged / uncharged, rocket boots | [ABILITIES.md](reverse-engineering/ABILITIES.md) |
| recharge crystal, attractor pad | world objects (GRAPPLE.md §12, ABILITIES.md §13) |
| one start-to-checkpoint route per level | accumulated drift, regression baseline |

## 8. What our replay harness does with a trace

Exists today (CONFIRMED in code): `asamu_player::trace` reads and validates traces (`Trace::read_jsonl`), compares
two traces aligned by tick (`compare` / `compare_with` with `CompareTolerances`: per-field max, mean and RMS, the
first divergence, unmatched ticks), and records runtime traces (`TraceSample::capture`, `record_run_with`);
`asamu_game::Game::start_recording` / `stop_recording` and the app's F9 key record runtime traces.

To build (planned, in this order):

1. `tools/asamu-trace`: `validate`, `summary`, `compare A B [--tol ...]`, `replay TRACE --converted DIR`.
2. **Replay:** load the trace's level from converted data (`Game::load_level`), place the pawn at sample 0
   (position, velocity, rotation; walking or falling from `grounded`), set the fixed step to `1 / tick_rate`
   (variable-rate traces replay with each sample's time difference), apply each sample's input, record, compare.
3. **Report:** the `TraceDiff` as JSON and text (first divergence: tick, field, error; statistics per field).
4. **Segment replays:** restart from the original's state every N ticks to separate local error from accumulated
   drift. Samples carry no script state (grapple budget, jump damping, boots; PARITY deviation 21), so segments
   start on the ground with neutral script state.
5. **Regression suite (M12):** small curated traces with per-trace tolerance budgets, run locally when
   `ASAMU_CONVERTED_DIR` is set; CI has no game data and skips them.
6. Every divergence becomes a hypothesis, then evidence (RE or a targeted trace), then a fix, with the "measured"
   column of PARITY.md updated.

Fields that would make replays from the middle of a trace exact (physics mode, base actor, grapple budget, boots
and damping state) need a schema version 2; v1 stays the recorder's contract until then.

## 9. Next steps

1. Ghidra: confirm what `FEngineLoop::PreInit` / `Init` do with the `BENCHMARK` and `FPS=` strings (E5 → CONFIRMED).
2. Ghidra: confirm in `UGameEngine::Tick` that input dispatch precedes `UWorld::Tick` (§3.3).
3. Compute the offsets of `Engine.Engine.GamePlayers`, `Engine.Player.Actor` and the gun's helper property; check
   the `CameraCache.POV.FOV` offset.
4. Feasibility session on the Mac (§4.1), started by the user.
5. Write `tools/trace-recorder/asamu_lldb.py`, `tools/asamu-trace`, and the configurable fixed step in `asamu-game`.
6. First traces: walk and stop, one jump, the jump-release series, one grapple attach and release.
7. Route B once Route A works (or at once if the Mac build does not start).

## 10. Safety, terms and hygiene

- **Offline single-player only.** The game has no online play and ships no anti-cheat (E13). Record in Steam's
  offline mode.
- **Your own licence, launched through Steam.** Do not use `NOSTEAM` or any other path around Steam's licence
  check.
- **Read-only.** Never modify the install, its files or its configuration; never write process memory, inject code
  or hook functions. The only change to the running process is the breakpoint instruction LLDB places (Route A) or
  none at all (hardware breakpoints, Route B). No cheats, no achievement or statistics changes. Back up saves first.
- **macOS protections stay on.** Never disable SIP or code-signing checks for this; grant *Developer Tools* access
  to the terminal only while recording.
- **What may be kept.** Traces are numeric behavioural measurements (inputs, positions, velocities, angles). Raw
  traces stay under the git-ignored `research/local/traces/`; only small curated traces may be committed as parity
  fixtures, after review and `repo-hygiene`. Never commit memory dumps, pointers or addresses from a user's
  machine, executables, decompiled code, screenshots or recordings of the game.
