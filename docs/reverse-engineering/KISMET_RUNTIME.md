# Kismet runtime

How the recreation executes each level's Kismet: the engine's sequence execution model, the behaviour of
every class the shipped maps use, Matinee playback with moving collision, and how the interpreter is wired
into the game tick. Structure and graph recovery are in [KISMET.md](KISMET.md); Matinee data and curve
evaluation in [MATINEE.md](MATINEE.md). Everything below is described in our own words. Where behaviour comes
from the game's script, it was read locally and is restated, never quoted.

**Status**
- All 97 classes used by level sequences in the 12 maps (62 actions, 16 events, 6 conditions, 10 variable
  classes, 3 sequence classes; 3,207 non-comment level objects) have interpreter behaviour. 0 classes fall
  back to the generic path. CONFIRMED (coverage test on the converted data).
- The engine scheduling rules were read from the decompiled natives listed in §2 and are reproduced rule for
  rule. CONFIRMED (structure); behavioural parity with the running game is unmeasured.
- The Matinee evaluators in `asamu-kismet` (a port of `asamu_ue3::matinee`) reproduce the importer's
  reference samples bit for bit: 264 of 264 probes (every bound move track of every level action). CONFIRMED
  (self-consistency with `asamu-ue3`, not with the engine).
- Every story map runs 600 ticks from level start inside the game with 0 interpreter errors, and produces the
  ability state of §9. CONFIRMED (gated test on converted data). The adversarial pass of §11 re-ran every
  loadable map for 3,000 ticks, drove every input of every op, compared Matinee movers with a direct
  `asamu_ue3::matinee` evaluation and checked that players and passengers ride movers; it found and fixed
  six behaviour bugs (§11).

## Tooling and reproduction

| Part | Where |
|---|---|
| Runtime graph export | `tools/asamu-import/src/kismet.rs` (`asamu-import kismet`) |
| Interpreter | `crates/asamu-kismet` (`graph`, `runtime`, `classes`, `interp`, `matinee`, `narrator`, `host`) |
| Game host, movers, frame order | `crates/asamu-game/src/kismet_host.rs` (`LevelScript`, `load_level_with_kismet`) |
| Moving collision | `CollisionScene::take_actor_statics` in `crates/asamu-world/src/collision/mod.rs` |

```sh
export CARGO_TARGET_DIR=target/agents
OUT=research/local/conv            # git-ignored; any user-local folder works
cargo run -p asamu-import -- --out $OUT levels
cargo run -p asamu-import -- --out $OUT meshes --collision
cargo run -p asamu-import -- --out $OUT matinee
cargo run -p asamu-import -- --out $OUT kismet
cargo test -p asamu-kismet                                   # synthetic + hostile tests
ASAMU_CONVERTED_DIR=$OUT cargo test -p asamu-kismet --test real_data -- --nocapture
ASAMU_CONVERTED_DIR=$OUT cargo test -p asamu-game --lib kismet_host -- --nocapture
```

The engine functions were decompiled with `tools/ghidra-scripts/DecompileToLocal.java` into the ignored
`research/decompiled/` folder (local reading only), and `UWorld::Tick` was disassembled with `objdump`.
Functions read: `USequence::{UpdateOp, ExecuteActiveOps, QueueSequenceOp, QueueDelayedSequenceOp, BeginPlay,
NotifyMatchStarted, InitializeSequence, Activated}`, `USequenceOp::{ActivateOutputLink, ForceActivateOutput,
ForceActivateInput, DeActivated, UpdateOp, Populate/PublishLinkedVariableValues}`, `USequenceEvent::{CheckActivate,
ActivateEvent}`, `USeqEvent_Touch::{CheckActivate, CheckTouchActivate, CheckUnTouchActivate, DoTouchActivation,
DoUnTouchActivation}`, `USeqAct_Latent::{Activated, UpdateOp, DeActivated}`, `USeqAct_Delay`, `USeqAct_Toggle`,
`USeqAct_SetBool/SetFloat/SetObject/AddInt`, `USeqCond_CompareBool/CompareInt/CompareFloat/Increment/IsPIE`,
`USeqAct_PlaySound`, `USeqAct_CameraFade`, `USeqAct_MultiLevelStreaming`, `USeqAct_ActivateRemoteEvent`,
`USeqAct_Interp::{Activated, UpdateOp, DeActivated, NotifyEventTriggered}`, `USeqVar_{Bool,Int,Float,Object,
String}::{PopulateValue, PublishValue}`, `USeqVar_Random{Int,Float}::GetRef`, `AActor::{SetTimer, UpdateTimers}`,
`UInterpTrackMove::GetMoveRefFrame`, `UWorld::{BeginPlay, AddToWorld}`, `AWorldInfo::NotifyMatchStarted`.

## Runtime graph format (`asamu-kismet-runtime` v1)

`<converted>/kismet/<map>.kismet.json`, user-local, never committed:

- `nodes`: every Kismet object in export order. Ids equal the `asamu_ue3::kismet` node ids, so the Matinee
  export's `node` fields match. Level-scope nodes carry: qualified class, kind, parent sequence, `bEnabled`,
  input links (label, delay, disabled), output links (label, delay, disabled, target `{op, input}` list),
  variable links (label, `PropertyName`, linked variable ids; named variables are replaced by the variables
  they find), event links, sequence members, event settings (originator, trigger count, re-trigger delay,
  player-only), variable values, effective class properties, and three class flags from the merged class
  defaults: `bAutoActivateOutputLinks`, `bLatentExecution`, and whether the class derives from
  `SeqAct_Latent`. Prefab-archetype and detached objects are inert `other` nodes.
- `actors`: every referenced actor and its base, with level package, `ULevel::Actors` slot, placement and draw
  scale. The game maps them to world actor ids `(level << 16) | slot`.
- `sounds`: for each cue a sound or narrator action names, the cue's `Duration` and the duration of its first
  wave node (depth first, as `FindFirstWaveNode`). Voice and narrator waves live in the map's localized
  companion `<map>_LOC_INT`, which the export searches; the INT durations are used (other languages'
  recordings may be longer or shorter: TENTATIVE for them). All 214 sound actions and 101 narrator lines
  have both durations (gated test).
- `probes`: reference move-track samples from `asamu_ue3::matinee` (see Status).

Values use a small JSON encoding (`asamu_kismet::value`): numbers keep their int/float kind, `{"$obj": path}`
for object references, `{"$struct": name, ...}` for structs. The loader validates every id and port index,
caps sizes and drops broken links with a warning.

## 1. Frame order

| Fact | Evidence | Confidence |
|---|---|---|
| The engine updates the game sequence once per frame inside `UWorld::Tick`, in a loop over the level's game sequences placed immediately before the first actor tick group (`TickActors<FGlobalActorIterator>`). The loop is skipped when the world is paused for "players only". | Disassembly of `UWorld::Tick`: the sequence loop calls the sequences' `UpdateOp` with the frame time just before the call to `TickActors`. | CONFIRMED |
| A streamed level's root sequence becomes a nested sequence of the persistent level's game sequence when the level is added to the world, so it updates (and is searched) as part of the persistent level. Its begin-play then runs. | Decompiled `UWorld::AddToWorld`. | CONFIRMED |
| Events raised by actors during their ticks (touches, grapple, landing, deaths) queue Kismet work that runs in the next frame's update. | Activation only queues ops; execution happens in `ExecuteActiveOps` (§2). | CONFIRMED |
| The frame's input events (fire, boost presses) are processed before `UWorld::Tick`. | Stock engine flow, as assumed by GRAPPLE.md G-TM-2. | STRONG |

**Ours** (`LevelScript::tick`): Kismet update → carry a based player (§6) → `Game::tick` (input events, map
actors, controller and pawn, touches) → hand the tick's events to the interpreter for the next update. The
one deviation: `Game::tick` begins with the input events, so Kismet runs just before them rather than just
after. An input event therefore sees this frame's Kismet effects one frame early. Impact TENTATIVE (small;
trace parity needed).

## 2. Sequence execution model

Every sequence owns a stack of active ops, a list of delayed activations, a list of latent ops deferred to the
next frame and a queue of pending event activations. CONFIRMED (all rules in this section, from the
decompiled natives).

**Sequence update.** A sequence first updates its own ops, then its nested sequences in member order. A
disabled sequence updates neither (and its events cannot activate).

**Executing the active ops.**
1. Each delayed activation counts down by the frame time. When it reaches zero, the target input gets an
   impulse (an impulse already present adds a queued activation; a disabled input ignores it), and the target
   op goes to the bottom of the stack.
2. Latent ops deferred last frame go to the bottom of the stack.
3. The loop runs only when the stack is not empty. Ops are popped from the top, at most 999 per update. Each
   time processing leaves the stack empty, one pending event activation (the oldest) is performed; the loop
   ends when the stack is still empty afterwards (an event that is still active re-queues that activation at
   the back instead, so the next attempt is in a later update).
4. Processing an op: values flow from linked variables into the op (§4). An inactive op becomes active and
   runs its activation behaviour. A latent op records that it was updated this frame. An active op then runs
   its update. A latent op that is not finished is re-queued (on top) when the stack is done. A finished op
   runs its deactivation behaviour, and its properties flow back to the linked variables. Every output that
   carries an impulse then activates its links: with no delay (output delay plus input delay) at once, else
   through the delayed list.
5. After processing, each input of the op with further queued impulses keeps its impulse and re-queues the op
   (at the bottom), but only for non-latent ops; latent ops drop extra impulses. Output impulses are cleared.
   The links collected in step 4 then get their impulses and their ops go on top of the stack, so they run
   next, in link order.
6. An active `SeqAct_Latent` subclass already updated in this frame that is popped again is deferred to the
   next frame. The test is the class (the engine walks the class chain for `SeqAct_Latent`), not
   `bLatentExecution`: a latent action outside that class (`SeqAct_CameraFade`) pulsed again after it ran is
   simply processed, and counts down, a second time in the same update.

**Queues.** Queuing an op that is already on its sequence's stack does nothing. "On top" means processed
next; "at the bottom" means after everything already queued. A delayed activation of an input that is already
waiting restarts its countdown.

**Outputs from code.** Setting an output (the normal path, used by conditions, latent actions and Matinee
events) marks it and lets step 4 propagate it. Forcing an output (used by much of the game's script) instead
activates the linked inputs at once and puts their ops at the bottom of their stack, without marking the
output itself and without checking disabled inputs.

**Default deactivation.** An op whose class auto-activates its outputs marks all non-disabled outputs when it
finishes. A `SeqAct_Latent` subclass instead marks output 1 when it was aborted and has more than one output,
else output 0. Its activation counts as aborted when no actor took the latent work (`SeqAct_Latent::Activated`
sets the flag), which is always the case for the classes here that run that function. `SeqAct_Delay`,
`SeqAct_PlaySound`, `SeqAct_Interp` and `SeqAct_MultiLevelStreaming` override `Activated` without calling it
(decompiled), so they are never aborted: streaming finishes on output 0 even with a second output. Latent
actions without an update of their own (narrator lines, streaming) finish in the frame they start;
`SeqAct_Delay`, `SeqAct_PlaySound` and `SeqAct_Interp` have their own update and deactivation. CONFIRMED

## 3. Events

**Activation check** (`CheckActivate`). The event needs an originator, an enabled parent sequence, a player
instigator when it is player-only, and a trigger count below its maximum (0 = unlimited). With a re-trigger
delay and at least one earlier trigger, the time since the last activation must exceed the delay. Only an
enabled event then activates. CONFIRMED

**Activation** (`ActivateEvent`). It records originator and instigator, and (unless it replays a pending
activation) the time and one more trigger. An event already pending queues the activation for later.
Otherwise the event becomes active, runs its script activation, writes the instigator into its "Instigator"
variables, writes its properties to linked variables, marks either all outputs or the listed ones, and goes
on its sequence's stack. CONFIRMED

**Level start.** The order follows the engine and the ASAMU game info:
1. `USequence::BeginPlay` on the game sequence (nested sequences first): every level-loaded event whose first
   output has links is checked with its first output ("Loaded and Visible"). CONFIRMED
2. The match starts and the pawn spawns. That starts the adaptive music manager: it runs every
   `SeqAct_AddAdaptiveTracks` directly (they have no inputs) and starts a beat timer for each
   `SeqEvent_TrackBeat` whose track exists. Then the game info loads the level's save. Without a save of this
   level, it reports index −1 to every `SavedGameStateLoaded` event (events with index −1 always fire) and
   registers checkpoint 0. CONFIRMED (src) for the ASAMU part; STRONG for the stock match-start order.
3. `NotifyMatchStarted` (nested sequences first): each level-loaded event is checked again with its first
   output, then with its second ("Beginning of Level") when that has links. Events with a trigger count of 1
   (143 of the 144 in level sequences; the other has 0, unlimited, and fires at both steps) fire only once.
   CONFIRMED (decompiled); STRONG that StartMatch passes both flags.

The level-loaded activations of steps 1–3 all execute in the first Kismet update.

**Touch** (`SeqEvent_Touch`). A touch of the originator by the player runs the activation check as a test,
then activates output 0 ("Touched") and remembers the player. An untouch after a touch runs the test with the
activation time set to zero and the player-only flag cleared. It then activates "UnTouched", plus "Empty" once
nothing touches. An untouch counts as a trigger, so a single-use touch never fires "UnTouched". With the
activation time zeroed, the re-trigger delay is measured from level start, so an untouch within the first
`ReTriggerDelay` seconds of a level is ignored. CONFIRMED. The engine also requires the instigator to overlap
the originator; ours uses the world's touch events (A-CP and trigger rules in `asamu_world::gameplay`),
which already mean overlap. STRONG

**Remote events.** `SeqAct_ActivateRemoteEvent` searches the whole game sequence (depth first, member order)
for enabled remote events with the same name and checks each with the world as originator and the action's
instigator (or the world). No match is an error in the log. CONFIRMED

**ASAMU events.** Where the game's script raises them (local reading of the script; CONFIRMED (src)):

| Event | Raised by | Mechanism |
|---|---|---|
| `SeqEvent_PlayerGrappled` | every attach (GRAPPLE.md §14) | check of every such event with the grappled interactable actor (else the world) as originator, output indices {1, 1} (out of range, so none); the event's script activation pulses output 0 only when its `inputActor` variable holds the originator |
| `SeqEvent_PlayerReleasedGrapple` | every release | check with the world as originator |
| `SeqEvent_CollectibleCollected` | a collectible pickup | check with the world as originator; its script activation also forces output 0, so a linked op receives two impulses and runs twice |
| `SeqEvent_PlayerLanded`, `SeqEvent_PlayerDied` | the pawn's landing (hard landing, ABILITIES.md §5) and death (`WorldEvent::PlayerRespawned`) | every event, forced output 0 when enabled; no trigger count |
| `SeqEvent_PlayerRocketBoosted` | boots charge start / boost start | forced output 0 / 1 when enabled |
| `SeqEvent_ActorInteractedWith` | a story-mode interaction | every event counts the call (up to its trigger count) and forces output 0 when its originator is the interacted actor. The count also grows on calls for other actors. |
| `SeqEvent_NarratorEvents` | the narrator (§7) | forced output 0 / 1 when enabled |
| `SaveGameState_SeqEvent_SavedGameStateLoaded` | the level-start load (above) | forced output 0 when the event's index is −1 or equals the loaded checkpoint index |
| `SeqEvent_TrackBeat` | a beat timer | a latent sleep of `beatAmount` seconds (GRAPPLE.md G-TM-3), then forced output 0, in a loop |
| `SeqEvent_WormEvents`, `SeqEvent_CreditsEnded` | the worm NPC, the credits | API for those systems (`Runtime::worm_event`, `credits_ended`) |
| `SeqEvent_AnimNotify` | animation notifies | API (`Runtime::anim_notify`): an event whose originator and `NotifyName` match is checked. TENTATIVE dispatch: no animation playback drives it yet. |

## 4. Variables and properties

| Rule | Confidence |
|---|---|
| Before an op is processed, each variable link with a `PropertyName` fills that property from its variables. Ints and floats are summed, bools are ANDed (true with no variables), an object property takes the first non-null object, and an array property gets one element per linked variable, with nulls kept. | CONFIRMED (decompiled `PublishValue` functions; in this build, "publish" moves variables into the op) |
| After an op finishes, each such link writes the property back to all its variables, element by element for arrays. Writeability is not checked, which is how `SeqCond_Increment` updates its counter. Links flagged `bSequenceNeedsPublishing` are skipped (the export writes false; the flag is not decoded yet). | CONFIRMED (decompiled `PopulateValue` functions and `PublishLinkedVariableValues`); TENTATIVE (the flag) |
| `SeqVar_RandomInt` draws `rand() % (max + 1 − min) + min` on every read, with the bounds swapped when min > max. `SeqVar_RandomFloat` uses the engine's float generator (`seed·0x0BB38435 + 0x3619636B`, mantissa bits as a float in [1, 2)). | CONFIRMED (arithmetic). Ours: a seeded LCG stands in for the C library's `rand()`, and the float seed starts at 0; the original streams are not reproducible. |
| `SeqVar_Player` reads as the player; named variables were resolved by the importer. | CONFIRMED |

## 5. Classes

"Native": decompiled engine code. "Src": the game's script, read locally. All CONFIRMED unless noted.

**Engine actions and conditions**

| Class | Behaviour | Evidence |
|---|---|---|
| `SeqAct_ActivateRemoteEvent` | §3 | native |
| `SeqAct_AddInt` | `FloatResult = A + B`, `IntResult = round(FloatResult)`, output 0 | native |
| `SeqAct_SetBool` | the AND of the "Value" bools (or `DefaultValue` without any) goes into every "Target" bool | native |
| `SeqAct_SetFloat` | `Target` = sum of the "Value" floats | native |
| `SeqAct_SetObject` | `Value` (or `DefaultValue` when null) goes into every target object variable | native |
| `SeqAct_SetInt`, `SeqAct_SetString`, `SeqAct_Gate`, `SeqAct_Log`, `SeqCond_CompareObject` | stock semantics; not used by the maps | TENTATIVE |
| `SeqAct_Toggle` | per input (on / off / toggle): the "Bool" variables, the `bEnabled` of the events on its first event link, and `OnToggle` on target actors (host: triggers and trigger volumes switch collision; others are output events) | native (the actor handler is the stock dispatch; actor reactions TENTATIVE) |
| `SeqAct_ToggleHidden` | a toggle (bools and events as above) plus hide/unhide of target actors | native (class derives from `SeqAct_Toggle`) |
| `SeqAct_Delay` | starts with `Duration` (from its variable; `DefaultDuration` when unlinked, TENTATIVE). The activation frame does not count down. Start restarts it (`bStartWillRestart`), Stop finishes it silently, and Pause halts it. When the time is up it marks "Finished". | native |
| `SeqAct_PlaySound` | Play starts the sound (after `ExtraDelay`; at once when `|ExtraDelay|` < 1e-8, the float the decompiled code reads at 0x10163FED4) and marks "Out" at once. It runs for the first wave's duration plus the delay, marks "BeforeEnd" at `BeforeEndTime`, and finishes with "Finished", or "Stopped" after Stop. Emits `PlaySound` / `StopSound`. | native |
| `SeqAct_CameraFade` | emits the fade, marks "Out", and after `FadeTime` marks "Finished" | native |
| `SeqAct_MultiLevelStreaming` | loads or unloads the named levels (host), attaches a level's scripts when it becomes visible, then "Finished" | native (ours completes at once) |
| `SeqAct_ConsoleCommand` | runs only with a player target. `open <map>[?options]` emits a level transition; other commands are emitted as console-command outputs (KISMET.md lists which have handlers) | native dispatch; command effects per KISMET.md |
| `SeqAct_Teleport`, `SeqAct_SetVelocity` | player target: teleport to the destination actor (rotation with `bUpdateRotation`); velocity = normalized `VelocityDir` × `VelocityMag` | stock; TENTATIVE details |
| `SeqAct_Destroy`, `SeqAct_ChangeCollision` | host: the actor's collision is removed / switched (such actors are made dynamic at load) | stock; TENTATIVE (actor removal is not modelled beyond collision) |
| `SeqAct_CameraShake`, `SeqAct_PlayCameraAnim`, `SeqAct_PlayMusicTrack`, `SeqAct_SetSoundMode`, `SeqAct_SetCameraTarget`, `SeqAct_SetMatInstScalarParam`, `SeqAct_ToggleCinematicMode`, `SeqAct_ToggleHUD`, `GFxAction_OpenMovie` | typed presentation outputs | stock |
| `SeqAct_Interp` | §6 | native + MATINEE.md |
| `SeqCond_CompareBool` | AND of the "Bool" variables → "True" or "False"; result written to `bResult` | native |
| `SeqCond_CompareInt`, `SeqCond_CompareFloat` | marks every true output among `A <= B`, `A > B`, `A == B`, `A < B`, `A >= B` | native |
| `SeqCond_Increment` | `A += IncrementAmount`, then the comparisons; `A` is written back | native |
| `SeqCond_IsPIE` | always "No" in the shipped game | native |

**ASAMU actions** (src; game-affecting ones call the host, the rest emit outputs)

| Class | Behaviour |
|---|---|
| `SeqAct_SetMaxGrapples` | gun capacity (GRAPPLE.md G-CT-2) |
| `SeqAct_ToggleGrapple` | the gun's fire latch (G-IN-4) |
| `SeqAct_ToggleRocketBoots` | enable or disable the boots (A-RB-1) |
| `SeqAct_ToggleVisibleGrapple` | hide or show the hand (G-AC-3) |
| `SeqAct_ToggleStoryMode` | input 0 enters story state unless already in it; input 1 leaves it if in it; input 2 flips it |
| `SeqAct_ToggleSpawnInStoryMode` | the pawn's flag: at death a pawn in story state stays in it (ours re-enters story mode after the death's exit) instead of leaving it |
| `SeqAct_ToggleZoomAvailable` | zoom on/off, then forces "Out". Its class also auto-activates outputs, so linked ops get two impulses (as in the original). |
| `SeqAct_TriggerCheckpoint`, `SeqAct_ToggleCheckpointEnable` | activate the checkpoint / set its `bEnabled` (A-CP rules) |
| `SeqAct_ToggleAttractor` | forces "Out", activates the pad; the pad's end forces "Finished" (`Runtime::attractor_finished`; the world model never ends a pad yet) |
| `SeqAct_ToggleFallingRocksActive` | all falling rocks on/off (G-WO-3) |
| `SeqAct_EditOrAddSaveString`, `SeqAct_GetSaveStringValue` | the general save manager's strings. Get forces "FoundSave" (and sets `Value`) or "DidntFindSave". Strings live in the runtime (`save_strings`) so the app can carry them across levels. |
| `SeqAct_NarratorLine` | §7 |
| `SeqAct_StartTimeTrial`, `SeqAct_EndTimeTrial`, `SeqCond_IsTimeTrial` | only under the time-trial game type; the condition forces "Yes"/"No" |
| `SeqAct_SetGameFinished` | sets the flag from its input, emits it, forces "Out" |
| `SeqAct_SetRotationToPlayerRotation` | copies the selected rotator components of the player to the actor |
| `SeqAct_ShowTutorialPopup` / `SeqAct_HideTutorialPopup`, `SeqAct_ToggleCrosshair`, `SeqAct_ShowTitleLogo`, `SeqAct_ToggleRestartFromCheckpointOption`, `SeqAct_DisablePauseMenu`, `SeqAct_SetLookAtTarget`, `SeqAct_SetVelocityConeMaterial`, `SeqAct_ToggleFollowCollision`, `SeqAct_PlaySuitOnAnimation`, `SeqAct_StartWorm` / `PauseWorm` / `ShutDownWorm`, `SeqAct_AddAdaptiveTracks`, `SeqAct_EditMultiplierForAllTracks`, `SeqAct_SetAdaptiveTrackVolumeMultiplier`, `SeqAct_UnlockASAMUAchievement`, `SeqAction_GFx_CustomInvoke_AS3_Menu` | typed outputs; the ones whose script forces "Out" do so |
| `SeqAct_PlayerDied` (not in the maps) | kills the player |

Class flags (`bAutoActivateOutputLinks`, latent) come from the class defaults in the data, so a class that
both auto-activates and forces an output reproduces the double activation automatically.

## 6. Matinee and movers

`SeqAct_Interp` follows MATINEE.md: inputs, precedence, stepping and track windows. The decompiled native
parts:
- Activation happens only for a stopped action and only through Play, Reverse or Change Dir. It builds the
  group instances at the current position (one per bound actor; initial transforms from the actor's current
  transform and base), then plays.
- Each update applies at most one input. Without an input, a stopped action finishes, so `Completed` /
  `Reversed` fire one update after the action stopped. Otherwise the action steps and stays active.
- On deactivation, a position above `length − 1e-4` fires `Completed` and a position below `1e-4` fires
  `Reversed`, compared in double precision. CONFIRMED: the two doubles the decompiled `DeActivated` reads
  (at 0x1016393A0 and 0x1016457F8 in the macOS executable) are 1.0e-4 and −1.0e-4.
- An event key marks the output with its name.

Every group except a folder is instanced, the director group included: its event and sound tracks fire like
any other group's (11 event tracks and 3 sound tracks of the shipped Matinees sit in director groups, with 23
linked event keys, among them TheCore's five narrator cues). Director cuts, fades, sound keys and
visibility/toggle keys become outputs (visibility also hides actors through the host). Animation, property,
skeletal-control and particle tracks are not played. MATINEE.md lists them as decoded only.

An attached actor's move track is evaluated in its base's current frame on every update: the engine's
`GetMoveRefFrame` reads the base matrix each call (decompiled), so the actor follows a base that moves after
the action started. CONFIRMED (structure).

**Attachment** (UE3 `Base`). Moving an actor carries the actors attached to it, at any depth: each keeps
the transform relative to its base that it had when it last moved by itself (or when its base first moved),
and is re-placed from that relative transform and the base's new transform, so nothing drifts. This is what
the engine does for hard attachment; soft attachment (2 of StarHaven's 120 passengers) is treated the same
way (TENTATIVE). The importer lists every actor attached to a Matinee-bound actor in the actor table.
Without a Matinee of their own, 120 such passengers sit in StarHaven (crates, buckets, candles and a blocking
volume on the airships; 105 with collision flags), 78 in ParadiseCave and 19 in BeautifulCity; 13 more in
StarHaven (propellers, wings) also have their own move tracks. CONFIRMED (counts from the scene data).

**Movers** (ours). At load, every actor bound to a Matinee group, every passenger, the actor
`SetRotationToPlayerRotation` turns, and every target of a destroy or change-collision action becomes a
mover body (falling rocks excepted: they move through their rock state). Its static collision instances are
removed from the static BVH (`take_actor_statics`) and re-created as dynamic instances placed relative to the
actor's transform; actors without collision get a body too, so their transform and their passengers' are
tracked. Move tracks and attachment set the actor transform, which re-places the instances, and the actor's
location is published for grapple anchors (G-AT-8). On the shipped maps 149 actors in ParadiseCave, 225 in
StarHaven, 47 in BeautifulCity, 41 in IceCave, 16 in Workshop, 10 in Darkcave and 3 in Epilogue get bodies
(before passengers were added: 59, 81 and 29 with collision in ParadiseCave, StarHaven and IceCave).

**Basing** (TENTATIVE). Before the Kismet update, a grounded player standing on a mover is found with a short
downward sweep. After the update, the player is moved by the mover's change of transform (position through
the full transform, yaw by the yaw change). UE3's own basing, encroachment, pushing and crushing are not
modelled.

## 7. Narrator

`SeqAct_NarratorLine` is a latent action without latent actors. It finishes in the frame it starts: it
forces "Out", and its deactivation marks "NO". Its two inputs add or remove a line in the narrator manager.
CONFIRMED (src + native latent rules).

The narrator manager (src; native timers `SetTimer`/`UpdateTimers`, CONFIRMED):
- A line added to an empty queue fires the narrator events' "started" output, starts playing, and sets a
  timer of the cue's `Duration`.
- When that timer fires, the line's action forces "FinishedLine" and the line leaves the queue. The next line
  starts after its own `Delay`, through a second timer. With no line left, the "finished narrating" output
  fires.
- Removing the playing line (with the stop flag) stops it and cancels a pending delayed start, but not the
  duration timer.

Timers count up by the frame time and fire when the count exceeds the rate. A timer set with rate 0 is
removed without firing (`SetTimer` adds or updates the entry with rate 0; `UpdateTimers` removes it before
testing it; decompiled). Consequences, kept on purpose: a queued line with `Delay` 0 never starts and stalls
the queue, and a cue with an unknown duration never finishes. STRONG (direct consequence of the native timer
rules). 11 of the 101 shipped lines have `Delay` 0 (TheCore 5, Darkcave 3, Epilogue, StarHaven and Workshop 1
each). TheCore's five are added by Matinee event keys 11–53 s apart, each after the previous cue (2.9–9.8 s)
has ended, so they never queue and all five play (checked by running the credits sequence). The other six are
not yet checked.

Not modelled: `removeAllOtherCues` (no shipped line sets it; the script's loop would remove every queued line
except the playing one). Ours caps the queue at 1,024 lines (`narrator::MAX_LINES`; the script's array is
unbounded) so a graph that keeps adding lines whose cue never ends cannot grow it without limit.

## 8. Host and outputs

`asamu_kismet::Host` receives the game-affecting calls: abilities, story mode, checkpoints, attractors,
falling rocks, level streaming, actor toggles, hide, destroy and collision, actor transforms, player
teleport, velocity and kill. Triggering a checkpoint that is already activated or disabled is a silent no-op,
as in the original (A-CP rules); only an actor that is not a checkpoint of the level is reported as an
error. `asamu_game::kismet_host` implements it on `Game`. With a script, the level-start
ability table (`asamu_world::level_start_abilities`) is undone at attach (the pawn's script state is reset to
a fresh start) and the map's Kismet decides.

`asamu_kismet::Output` carries the presentation events: level transitions, console commands, sounds,
narration, tutorials, crosshair, cinematic mode, HUD, camera (target, fade, shake, animation, Matinee
cuts/fades), music and sound modes, adaptive music, menus, achievements, time trial, game finished, NPC look-at
and worm control, actor toggled/hidden/destroyed. `LevelScript::tick` returns them with the tick report
(`ScriptedTick`). `TickReport` itself is unchanged.

## 9. What the shipped maps do at level start

Fresh shipped-game start of each story map, 600 ticks without input. CONFIRMED (gated test on converted data,
values from the maps; meaning as noted):

| Map | Grapple limit | Rocket boots | Story mode at start | Notes |
|---|---|---|---|---|
| AG-Workshop | 0 | off | on | |
| AG-ParadiseCave | 0 | off (disabled at start) | off | The grapple unlock is a touch volume: limit 1 plus the grapple latch (verified by touching it). A second volume sets 2. |
| AG-BeautifulCity | 2 | off | on | Granted by the saved-game-state-loaded path that every fresh level start takes (§3). A trigger touch sets 3. |
| AG-Darkcave | 3 | off | off | Also triggers a checkpoint at start. |
| AG-StarHaven | 3 | off | on | Boots come from a trigger whose touch event a cutscene enables (verified by enabling it and touching), or from the interaction cutscene. |
| AG-IceCave | 3 | on | off | |
| AG-Epilogue | 0 | off | on | Fires `SetGameFinished`. |

**Corrections to [LEVELS.md](LEVELS.md) and `asamu_world::level_start_abilities`** (CONFIRMED by evaluating
the conditions; those docs called them TENTATIVE):
- ParadiseCave's level-start "limit 2" and StarHaven's level-start "boots enable" sit behind `SeqCond_IsPIE`
  "Yes" (play-in-editor only). They never run in the shipped game. 8 `IsPIE` branches exist (Workshop 1,
  ParadiseCave 2, BeautifulCity 2, StarHaven 2, Epilogue 1).
- BeautifulCity grants the grapple (limit 2) through `SavedGameStateLoaded`, which fires at every level start
  without a save of that level, not through a level-loaded path. Its `IsTimeTrial` "Yes" path applies only to
  time trials, and its limit 3 comes from a touch.
- The hard-coded table therefore differs from the Kismet result for ParadiseCave (table: 2) and StarHaven
  (table: boots on). With a `LevelScript` the table is not used.

## 10. Coverage

| | Classes | Level objects |
|---|---|---|
| Present in level sequences (excluding comment frames) | 97 | 3,207 |
| With interpreter behaviour | 97 | 3,207 |
| Generic fallback (`Unhandled` output) | 0 | 0 |

Stock classes implemented beyond the data: `SeqAct_Gate`, `SeqAct_Log`, `SeqAct_SetInt`, `SeqAct_SetString`,
`SeqAct_LevelStreaming`, `SeqCond_CompareObject`, `SeqAct_PlayerDied`, `SeqEvent_Used`, `SeqEvent_Destroyed`.
Not implemented (absent from the data): switch conditions, `SeqEvent_PlayerJumped`-style events (no such
class exists in the maps; GRAPPLE.md §14 lists the events the game raises).

## 11. Adversarial verification (2026-10-10)

An independent pass re-checked the claims above on freshly converted data (`levels`, `meshes --collision`,
`matinee`, `kismet`) with throwaway local tools, then turned the findings into tests.

| Check | Result |
|---|---|
| Every loadable converted map (10; `TheCore` and `Freds_place` have no PlayerStart and load only as sub-levels) for 3,000 ticks without input | 0 interpreter errors, 0 step-limit hits, ability state as in §9 |
| Class coverage | 97 classes / 3,207 level objects present, all with behaviour (recounted from the export) |
| Every input of every op pulsed on every map, plus the runtime's whole event API | no panic; the only step-limit hits are a BeautifulCity data loop (pick a random line whose "already played" flag is false; with every flag forced true it never ends, as in the original) |
| Matinee movers against `asamu_ue3::matinee` evaluated directly from the package (forced Play from level start, 1/17/61/240 frames) | 267 comparisons, 0 mismatches (bit-exact location, equal rotation); 264 of 264 importer probes still match |
| A player placed on a moving mover (every mover whose top a downward ray hits, first action that moves it) | 67 of 67 carried with 0.00 UU horizontal error, grounded every frame |
| Passengers keep their distance to a moving base (every action, every attached pair) | 2,172 checks; the one change is a wing that has its own Matinee |
| IceCave → TheCore → Epilogue | the exit trigger streams TheCore in, its scripts attach and begin play; the credits' end opens AG-Epilogue after the action's 6 s input delay |

**Fixed in this pass** (each with a test):
1. Director-group event and sound keys never fired (the director group got no instance): 23 linked event keys
   on 7 maps, including TheCore's narrator cues. `director_group_event_and_sound_keys_fire`.
2. Actors attached to a Matinee-driven base stayed behind (passengers were not exported and nothing carried
   them), leaving floating collision where airships and lifts started; Matinee-bound attached actors used the
   base transform from activation time. `attached_matinee_actor_follows_its_moving_base`,
   `matinee_mover_moves_collision_and_carries_the_player` (crate passenger), `cyclic_attachments_terminate`,
   the StarHaven part of `converted_story_steps_streaming_checkpoints_and_passengers`.
3. The queued-activation loop could spin forever on an event still pending (reachable with an event whose
   class flags claim latent execution); it now follows the engine's loop exactly.
   `a_latent_flagged_event_activated_repeatedly_does_not_hang`,
   `queued_event_activations_unqueue_one_at_a_time_across_frames`.
4. Same-frame deferral tested `bLatentExecution` instead of the `SeqAct_Latent` class.
   `latent_actions_outside_seqact_latent_run_again_in_the_same_update`.
5. `SeqAct_MultiLevelStreaming` was treated as aborted (no effect on the shipped maps, whose one streaming
   action has a single output). `multi_level_streaming_finishes_on_its_first_output`.
6. Ten `SeqAct_PlaySound` cues (voice and narration whose waves sit in `<map>_LOC_INT`) were exported
   without a first-wave duration, so the action finished at once instead of after the line: among them
   Workshop's 32.6 s prologue and the Epilogue's 21.4 s line, whose `Finished` outputs drive four ops each.
   The export now searches the localized companion. `every_sound_action_has_its_durations`.
7. Two engine constants confirmed from the executable and used exactly: the `Completed`/`Reversed` test of
   `SeqAct_Interp` compares in double precision with ±1e-4; `SeqAct_PlaySound` treats `|ExtraDelay|` below
   1e-8 (not 1e-4) as no delay.

Also: re-triggering an activated checkpoint no longer logs a false "checkpoint not found";
`LevelScript::moved_actors` adds the sub-level offset for every actor, not only collision movers; caps on the
queued activations per sequence (4,096), pending attractor pads (1,024) and narrator lines (1,024).

## Open items

- Behavioural parity: traces of the original running the same sequences (the order of same-frame events, the
  first frames of a level).
- Animation-driven events (`SeqEvent_AnimNotify`), worm events and credits need the NPC, animation and UI
  systems to call the runtime's API.
- Cinematic mode does not yet block player input in the game (only an output).
- `bSequenceNeedsPublishing` is exported as false.
- UE3 basing and encroachment for movers (§6); soft attachment is treated as hard (2 actors).
- `SeqAct_ToggleAttractor`'s `Finished` needs the world to report a pad's end (`Runtime::attractor_finished`);
  no shipped action links that output, so nothing waits on it.
- `SeqEvent_CollectibleCollected` needs the collectible system to call `Runtime::collectible_collected`.
- Adaptive-music beat timing assumes every track named by `AddAdaptiveTracks` exists at level start.
