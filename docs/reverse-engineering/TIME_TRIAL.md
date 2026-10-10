# Time trial

Scope: how the original game's time-trial mode works — unlock, entry, the stopwatch, the restart rules, the HUD,
the end of a run and its scoreboard — and how our recreation models it. Best times, medals and `TTS.bin` itself
are in SAVE.md §4.4 / §6.5; the movement rules (identical to story play) in ABILITIES.md A-TT-1/2.

**Evidence and publication rules.** Behaviour comes from local reading of the shipped `ScriptText` of the
`asamu` package (never reproduced or paraphrased line by line here), class default objects and map exports decoded
by `asamu-inspect`, the shipped config, and native code of the unstripped Mac executable decompiled locally with
Ghidra (output stays in ignored `research/local/`). Labels as in `CLAUDE.md`; the source kind is given in brackets:
(src) script source reading, (cdo) class default object, (map) map export decode, (native) Mac executable,
(config) shipped ini.

## 1. Classes

| Class | Role | Confidence |
|---|---|---|
| `ASAMUGameInfoTimeTrial` (→ `ASAMUGameInfo`) | the game type of a trial; keeps `bTimeTrialActive` (class default false); start / end / restart entry points called by Kismet and the death hook; disables save-manager init, snapshot loading and snapshot saving | CONFIRMED (src, cdo) |
| `ASAMUHUDTimeTrial` (→ `ASAMUHUD`) | the stopwatch (an actor timer named `Timerrun`), forwards it to the HUD movie every tick; its own pause menu class; tutorial pop-ups disabled | CONFIRMED (src) |
| `ASAMUHUDMovieTimeTrial` (→ `ASAMUHUDMovie`) | the HUD fields (current time, target time, best time), the target progression, loading and saving `TTS.bin` | CONFIRMED (src, cdo) |
| `ASAMUUITimeTrialTimer` | the Scaleform widget behind those fields (three calls: stopwatch, scoreboard, target) | CONFIRMED (src) |
| `TimeTrialSavefile`, `TimeTrialScore` | the scoreboard and its five scores; targets per level; medal rule; "all gold" check | CONFIRMED (src, cdo) |
| `GFxASAMUPauseMenuTimeTrial` | the pause menu plus a "restart time trial" button with a confirmation pop-up | CONFIRMED (src) |
| `ASAMUPawn.TimeTrialRestart` (exec, F8) | restarts the level while a trial is active | CONFIRMED (src, config) |

## 2. Rules

**TT-1 Unlock and entry** [CONFIRMED (src, config)]. The main menu's time-trial button (under the custom-games
screen) is disabled until the progression's finished flag is set (Epilogue, SAVE.md §6.2). Its five entries open
`AG-ParadiseCave`, `AG-BeautifulCity`, `AG-Darkcave`, `AG-StarHaven` and `AG-IceCave` with
`?game=ASAMU.ASAMUGameInfoTimetrial`. Each entry shows the best time and medal (read from `TTS.bin` when the menu
opens).

**TT-2 Start and end come from Kismet** [CONFIRMED (map); run through our Kismet runtime on converted data,
`crates/asamu-game/tests/timetrial_real_data.rs`]. Every one of the five maps has exactly one
`SeqAct_StartTimeTrial`, reached from a trigger volume's touch through `SeqCond_IsTimeTrial`, and exactly one
`SeqAct_EndTimeTrial`, reached directly from the touch of a trigger at the level exit:

| Map | Start: touch of | Gate size (bounds, UU) | Gate from the respawn point | End: touch of | Front end opens |
|---|---|---|---|---|---|
| AG-ParadiseCave | `TriggerVolume_6` | 128 × 23,675 × 26,836 | 1,815 UU | `Trigger_10` | 4 s after the end |
| AG-BeautifulCity | `TriggerVolume_0` | 671 × 108 × 972 | 382 UU | `Trigger_6` | 5 s |
| AG-Darkcave | `TriggerVolume_0` | 256 × 6,128 × 5,072 | 3,242 UU | `Trigger_6` | 4 s |
| AG-StarHaven | `TriggerVolume_4` | 3,920 × 256 × 3,952 | 3,935 UU | `Trigger_6` | 4 s |
| AG-IceCave | `TriggerVolume_1` | 4,460 × 3,355 × 12,823 (turned slab) | 307 UU | `Trigger_6` | 4 s |

- **The start is a gate.** Each start volume is a thin slab across the route, some way past the spawn ("from the
  respawn point": the distance from the gate's hull to where a run without a registered checkpoint respawns; the
  player start is 241 to 4,068 UU from it). Standing at the spawn starts nothing. The start's touch event has
  `MaxTriggerCount` 0 (no limit, re-trigger delay 0.1 s): it fires at every crossing.
- **The end is a time-trial-only trigger.** In all five maps the end's touch event is **disabled in the map** and has
  `MaxTriggerCount` 1. The level-start Kismet turns it on in time trial only (`SeqEvent_LevelLoaded` →
  `SeqCond_IsTimeTrial` → `SeqAct_Toggle` on the event); in story play it stays off for the whole level. The same
  touch also runs a story-mode toggle and a hidden-toggle that leads to a Matinee. (The first version of this
  document said only AG-IceCave's end event starts disabled, "enabled during play"; all five do, and what enables
  them is the level start.)
- **After the end** Kismet runs a tutorial pop-up action in four maps (not AG-BeautifulCity) — which shows nothing,
  because the time-trial HUD's `PushTutorial` is empty (TT-8) —, fades the camera and runs
  `open ASAMUFrontEndMap?game=ASAMU.GFxASAMUMenuGameInfo` after an input delay of 4 s (5 s in AG-BeautifulCity).

**TT-3 The stopwatch** [CONFIRMED (src); timer semantics CONFIRMED (native), re-read in the verification pass].
- `bTimeTrialActive` (game type): set by every start; cleared by the end, which does nothing at all while it is
  clear. A death never clears it.
- The HUD timer: a start creates it, counting from 0, **only if no timer exists**. The end pauses it and reports its
  count. A death under the death rule (TT-4) clears it: the timer is gone and the display shows 0.
- Actor timers add each frame's game time (scaled by a per-timer factor that nothing in the time-trial script
  sets) while not paused, so the stopwatch is game time: it stops while the game is paused and follows time
  dilation (`AActor::UpdateTimers`).
  `IsTimerActive` tests only that the timer exists with a positive rate — a **paused** timer counts as active
  (`AActor::IsTimerActive`; `PauseTimer` only sets a flag). So after the end the HUD keeps showing the frozen time
  and a later start does not restart it. `ClearTimer` sets the rate to 0 and the next timer update removes the
  entry; from then on the count of the missing timer is −1 (`AActor::GetTimerCount` returns the constant −1.0).
- The count is a 32-bit float that grows by one frame time per frame, so the original's times carry a small
  frame-rate-dependent rounding drift (CONFIRMED mechanism; the size — up to tenths of a second over a long level —
  is our estimate, TENTATIVE). Our stopwatch counts simulation ticks exactly and does not reproduce the drift.

**TT-4 Death rule** [CONFIRMED (src, map)]. The time-trial game type's `PlayerDied` (the death hook at the player
reset, 0.3 s into the death sequence, ABILITIES.md A-DT-2) clears the stopwatch when the checkpoint manager has no
checkpoint for the current level (index −1). Time trial never registers checkpoint 0 at the start (A-TT-2), and
touching the level's first checkpoint registers nothing either (a checkpoint becomes the latest only if its index
is above the one the respawn lookup already returns, A-CP-3), so the rule holds **until a checkpoint beyond the
first is reached**. The run itself stays active. The respawn is at the level's first checkpoint, which lies outside
the start gate in every map (TT-2 table): the stopwatch shows 0 and does not run until the player crosses the gate
again, and then counts from that crossing. (SAVE.md §6.5 and ABILITIES.md A-TT-2 put this as "restarts the
stopwatch"; the clear and the new start are two separate events.)

**TT-5 Restart** [CONFIRMED (src, config)]. F8 (`GBA_TimeTrialRestart`, keyboard only; no controller binding) runs
`restartlevel` while `bTimeTrialActive` is set — also after a TT-4 clear. The time-trial pause menu adds a "restart
time trial" button that asks for confirmation, then runs `restartlevel`; its "main menu" button opens the front end.
A level restart reloads the map, so the HUD (and its target) start over.

**TT-6 HUD** [CONFIRMED (src, cdo)]. Three text fields: the current time, the target time, the best time. At start
the target is the level's gold time. Each HUD update (every tick while the timer exists) that finds the time
strictly greater than the shown target moves on by one: gold → silver → bronze → no target (the localized
`NoDiceLabel` text, no time). The updates go on with the frozen time after the end (a paused timer is active, TT-3).
Targets per level are `TimeTrialSavefile.LevelTargetScores` (SAVE.md §6.5). A death clear (TT-4) shows 0 but does
**not** move the target back. The best field shows the stored best time and its medal; it is refreshed after every
save. A "go" pop-up hide rule after 3 s exists in the HUD movie but its switch (`bShowGoText`) is never set
(class default false; no other class of the package names it) — dead code. The timer widget sits at the top right
(`SetPosition(1108, 10)` on the HUD movie).

**TT-7 End of a run** [CONFIRMED (src)]. The end reads the count; if it is lower than the stored best, or no best
exists (best ≤ 0), it becomes the new best and a pop-up with the `NewBestTimeLabel` text shows; an equal time is not
a new best. Every finish then checks all five golds (→ `ALL_GOLD_MEDALS`, SAVE.md §6.4) and rewrites `TTS.bin`
(plain, version 3). The medal rule accepts a time equal to its target, the HUD target moves on only strictly past
it: the two agree at the boundary.

**TT-8 Otherwise like story play** [CONFIRMED (src); ABILITIES.md A-TT-1/2]. Same pawn, controller, gun and
abilities; no snapshot saved or loaded, no chapter pointer; collectibles hidden and not colliding; tutorial pop-ups
suppressed (`SeqAct_ShowTutorialPopup` calls the HUD's `PushTutorial`, which `ASAMUHUDTimeTrial` overrides with an
empty body); Kismet branches on `SeqCond_IsTimeTrial` (5–15 per map, KISMET.md).

## 3. Quirks

| # | Quirk | Confidence |
|---|---|---|
| TT-Q1 | Reaching the end while the stopwatch is cleared (died under the death rule, then reached the exit without crossing the start gate again) reports −1; −1 is lower than any stored best, so the real best time is **replaced by −1** (which then reads as "no time": medals need a time > 0). The gates span the route by their size in four maps and are doorway-sized in AG-BeautifulCity (TT-2 table); whether any can be passed by was not tried. | CONFIRMED (src, native) code path; TENTATIVE reachability |
| TT-Q2 | The target shown after a death clear is not reset (it only resets with a level reload). | CONFIRMED (src) |
| TT-Q3 | A start after the end (re-entering the start volume) sets `bTimeTrialActive` again but leaves the frozen stopwatch; a second end would re-report the same time. Not reachable in the shipped maps: the end event fires once (`MaxTriggerCount` 1) and the front end opens 4–5 s later, with the gate at the other end of the level. | CONFIRMED (src, native) code path; STRONG (map) that it cannot happen |
| TT-Q4 | F8 still restarts after a death clear (the flag is not cleared). | CONFIRMED (src) |

## 4. Our implementation

- `asamu_game::timetrial` (render-free, deterministic, in simulation ticks): `TimeTrialRun` models TT-3/4/5/6
  (`start`, `end` → `EndOutcome`, `on_player_reset`, `restart_allowed`, `count` with −1 for a missing timer,
  `display_seconds`, `update_target`, `target_seconds`), `targets_for`, the level-selection entries
  (`level_entries`: unlocked by the finished flag, the menu's order, best time, medal, targets) and the HUD view
  (`hud_view`). Recording goes through `save::SaveSession::on_time_trial_end` (best if strictly faster or first,
  medal, `ALL_GOLD_MEDALS`). Tests: `crates/asamu-game/tests/timetrial_rules.rs` (synthetic: the rules, the
  target/medal boundary on every level, the save hand-off of the quirks, damaged clocks) and
  `crates/asamu-game/tests/timetrial_real_data.rs` (ignored; the user's converted maps, see below).
- TT-Q1 is fixed deliberately (SAVE.md §9.1: quirks that only lose data get a documented fix): `end` reports
  `EndOutcome::NoTime` and nothing is recorded; the save session also refuses times that are not positive finite
  numbers.
- App (`apps/asamu/src/timetrial.rs`): a `TimeTrialHud` resource runs a `TimeTrialRun` from the Kismet start/end
  messages and each tick's respawn (TT-4), and shows the target and the best time with its medal under the menu
  flow's stopwatch (our own wording; the original's labels are not shipped with the app). A new game — a new
  simulation, or one whose clock went back — starts a fresh run with the gold target, as a level reload does in the
  original. In time-trial play it also drops every tutorial pop-up Kismet asks for before it is shown (TT-8).
- The menu flow (`apps/asamu/src/ui`, not part of this workstream) owns the stopwatch display, F8, the selection
  screen and the recording. Two of its rules differ from the original and should follow `TimeTrialRun`: it restarts
  a running stopwatch at the respawn after an early death (the original clears it until the gate is crossed again,
  TT-4 — with the gates 307 to 3,935 UU from the respawn point the recorded time comes out longer than the
  original's by the walk to the gate), and its F8 requires a running stopwatch (the original: an active run, TT-5 /
  TT-Q4). The pause menu has no "restart time trial" entry yet (TT-5).

**A run end to end (verification pass, 2026-10-10; converted data under ignored `research/local/`, deleted
afterwards).** `timetrial_real_data.rs` loads each of the five maps in the time-trial game type with its Kismet and
checks, driving `TimeTrialRun` from the Kismet outputs as the app does: the end event is off in the map and on
after the first frames; 300 idle frames start nothing; a teleport into the gate fires the start on the next frame;
a kill before any checkpoint respawns 18 frames later with the stopwatch cleared, the run active and no new start;
the gate again starts the count from that frame; a teleport onto the end trigger fires the end once, with the time
counted from the second crossing; the front end opens 4.02 s later (5.00 s in AG-BeautifulCity). A second run of
each map gives the same frames. In story play the gate starts nothing and the end event stays off. What this does
not show: that the route from the gate to the end can be played (the player is teleported), and how the original's
own HUD looks.

## 5. Reproduction

```bash
COOKED="<install>/A Story About My Uncle.app/Contents/Resources/ASAMU/CookedMac"
asamu-inspect defaults "$COOKED/Startup.upk" asamu.TimeTrialSavefile          # LevelTargetScores
asamu-inspect defaults "$COOKED/Startup.upk" asamu.ASAMUGameInfoTimeTrial     # bTimeTrialActive default
asamu-inspect defaults "$COOKED/Startup.upk" asamu.ASAMUHUDMovieTimeTrial     # no bShowGoText default
asamu-inspect props "$COOKED/Maps/AG-ParadiseCave.asamu" \
  TheWorld.PersistentLevel.Main_Sequence.Intro_Sequence.SeqEvent_Touch_21     # Originator, MaxTriggerCount 0
asamu-inspect kismet "$COOKED/Maps/AG-IceCave.asamu" --json --out research/local/k.json   # start/end wiring
# local reading only (never commit): asamu-inspect scripttext ... ASAMUGameInfoTimeTrial / ASAMUHUDTimeTrial /
#   ASAMUHUDMovieTimeTrial / TimeTrialSavefile / GFxASAMUPauseMenuTimeTrial / GFxASAMUMainMenu / ASAMUPawn
# native timers (local only): research/local/.../DecompileAddrs.java on AActor::IsTimerActive (0x100b39360),
#   AActor::GetTimerCount (0x100b393d0), AActor::PauseTimer (0x100b392d0), AActor::UpdateTimers (0x100908f50);
#   or without Ghidra (the four are short): objdump -d --start-address=0x100b39200 --stop-address=0x100b394b0
#   "<install>/A Story About My Uncle.app/Contents/MacOS/ASAMU"   (AActor::ClearTimer is at 0x100b39200)
cargo test -p asamu-game --test timetrial_rules
asamu-import --out <dir> levels && asamu-import --out <dir> kismet && asamu-import --out <dir> matinee \
  && asamu-import --out <dir> meshes --collision
ASAMU_CONVERTED_DIR=<dir> cargo test --release -p asamu-game --test timetrial_real_data -- --ignored --nocapture
```

## 6. Verification pass (2026-10-10)

Re-derived independently of the first pass (fresh `ScriptText` extraction, class defaults and Kismet graphs from
`asamu-inspect`, a plain disassembly of the four timer natives with `objdump`, the shipped ini files, and the run of
§4):

| Claim | Check | Result |
|---|---|---|
| Targets per level (TT-6, SAVE.md §6.5) | `Default__TimeTrialSavefile.LevelTargetScores` | agree: 260/290/320, 200/220/270, 240/270/320, 480/540/650, 810/960/1150 |
| Medal rule, best-time rule, all-gold check | script | agree (medal: time ≤ target and > 0; best: strictly lower or none) |
| Start wiring and `MaxTriggerCount` 0 on all five starts (TT-2) | Kismet graphs | agree |
| End wiring (TT-2) | Kismet graphs | **corrected**: disabled in the map and `MaxTriggerCount` 1 in all five; enabled by the level start in time trial |
| A start creates the timer only if none exists; the end pauses; the death hook clears (TT-3, TT-4) | script | agree |
| Paused timer is active; missing timer counts −1; timers add game time while not paused (TT-3) | `AActor::IsTimerActive`, `GetTimerCount`, `PauseTimer`, `ClearTimer`, `UpdateTimers` | agree; added: `ClearTimer` removes the entry at the next update, the count is a 32-bit float |
| "The clock starts again only when the start volume is touched again" (TT-4) | hull of each gate against the respawn point; the run of §4 | agree, and now CONFIRMED (map): the respawn never touches the gate |
| F8 only, level restart while the run is active, no controller binding (TT-5) | `DefaultInput.ini`, script | agree |
| The "go" pop-up switch is never set (TT-6) | the name occurs in the package's script only where it is declared and read | agree |
| TT-Q3 | Kismet graphs | code path agrees; **downgraded to unreachable** in the shipped maps |

