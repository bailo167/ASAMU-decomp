# NPCs and story actors

Behaviour of the original's NPCs (the worm, Maddie, villagers) and story actors (interactables, collectibles,
glow flowers, sound-making foliage), the first-person hands, and how this repository implements them.

Evidence: local reading of the `asamu` script classes listed below (their `ScriptText`, read under the ignored
`research/local/`, never reproduced here), their class default objects and component templates, the worm's
`AnimTree` and `AnimSet` objects, the Kismet graphs of the shipped maps, a map census, and a few engine natives
decompiled locally from the unstripped Mac executable (output in the ignored `research/decompiled/npcs/`). Every
description below is ours; no script text or decompiled code is reproduced. Confidence labels follow
`CLAUDE.md`; "(src)" = script source read locally, "(cdo)" = class default or template value, "(data)" = asset
object values, "(native)" = engine code read in Ghidra, "(map)" = map census.

Reproduce (local only; the outputs are game data):

```sh
C="$HOME/Library/Application Support/Steam/steamapps/common/A Story About My Uncle/A Story About My Uncle.app/Contents/Resources/ASAMU/CookedMac"
I=target/agents/release/asamu-inspect
$I defaults "$C/Startup.upk" ASAMUNPC_Worm --inherited                  # worm tuning (cdo)
$I props "$C/Startup.upk" Dark_Cave_worm.Animations.WormAnimTree.AnimNodeBlendList_5   # tree nodes (data)
$I props "$C/Startup.upk" Dark_Cave_worm.Animations.WormAnimSet.AnimSequence_4          # SequenceLength (data)
$I objects "$C/Maps/AG-Darkcave.asamu" | grep -E "ASAMUNPC|ASAMUWorm|PathNode"          # census (map)
$I props "$C/Startup.upk" Dark_Cave_worm.Animations.WormAnimTree.AnimNodeBlendList_2   # bPlayActiveChild (data)
$I scripttext "$C/Startup.upk" ASAMUNPC_Worm --out research/local/npcs/ASAMUNPC_Worm.uc  # local reading only
cargo test -p asamu-world --test npc_worm --test npc_actors --test npc_scene
cargo test -p asamu-game --test npc_system
ASAMU_CONVERTED_DIR=<dir with `asamu-import levels` (+ `skeletal`) output> cargo test -p asamu-world --test npc_scene real_
```

## 1. What the shipped maps contain — CONFIRMED (map)

| Class | Placed | Where |
|---|---:|---|
| `ASAMUNPC_WormPawn` | 1 | AG-Darkcave |
| `ASAMUWormScreamVolume` | 1 | AG-Darkcave (a box of roughly 43 000 × 63 000 × 32 000 uu around the worm's lair; the worm stands inside it) |
| `ASAMUWormShadowVolume` | **0** | — (the "player hides in a shadow" branch can never be true) |
| `ASAMUNPC_MaddiePawn`, `ASAMUNPC_VillagerPawn`, `ASAMUBackpackMaddie`, `Villager` | **0** | — |
| `PathNode` (any navigation point other than one `PlayerStart` per map) | **0** | — |
| `SkeletalMeshActor` (+ `…MAT`, `asamu.SkeletalMeshActorMATWithFollowCollision`) | 138 | BeautifulCity 70, StarHaven 64, Darkcave 3, TheCore 1 |
| `ASAMUInteractable_Actor` | 66 | Workshop 6, Paradise 12, Beautiful 4, Dark 3, StarHaven 17, Ice 22, FrontEnd 2 |
| `ASAMUCollectible` | 25 | 5 in each of Paradise, Beautiful, Dark, StarHaven, Ice |
| `ASAMUGlowFlower` | 15 | AG-Darkcave |
| `ASAMUSoundMakingFoliage` | 1 834 | Paradise 1 018, StarHaven 410, Dark 406 (ParadiseCave's package also holds 4 unplaced archetypes of the class, `Platforms.Rock_Grass.Rock_Grass_Arc0..3`, which an export count includes) |

Kismet census (all maps): `SeqAct_StartWorm`, `SeqAct_PauseWorm`, `SeqAct_ShutDownWorm` 2 each and
`SeqEvent_WormEvents` 4 (all AG-Darkcave); **no** `SeqAct_MaddieBackpack`, `SeqAct_PlayMaddieBackpackAnim`,
`SeqAct_WormResetSleepTimer` or `SeqEvent_Used`. `SeqEvent_ActorInteractedWith` 23, `SeqEvent_CollectibleCollected`
5 (one per collectible map). CONFIRMED (map).

Consequences: the only NPC with AI in the game is the worm. Maddie and the villagers are scenery: skeletal mesh
actors that loop an ambient animation or follow a Matinee. The Maddie/villager pawn AI exists in script but is
never instantiated. CONFIRMED (map, src).

## 2. Classes and defaults

| Class | Base | Role |
|---|---|---|
| `ASAMUNPC` | `AIController` | Base NPC controller; an empty `Idle` state |
| `ASAMUNPC_Pawn` | `UDKPawn` | Base NPC pawn; uses its `NPCController` class; collision becomes block-all-but-weapons at begin play |
| `ASAMUNPC_Worm` / `ASAMUNPC_WormPawn` | `ASAMUNPC` / `ASAMUNPC_Pawn` | The worm (§3) |
| `ASAMUWormScreamVolume`, `ASAMUWormShadowVolume` | `DynamicTriggerVolume` | Point-in-volume tests for the worm |
| `ASAMUNPC_Maddie` / `ASAMUNPC_MaddiePawn` | | Maddie pawn (§4) |
| `ASAMUBackpackMaddie` | `SkeletalMeshActor` | Maddie's arms on the player's grapple hand (§4) |
| `ASAMUNPC_Villager` / `ASAMUNPC_VillagerPawn` | | Villager pawn AI (§5) |
| `Villager` | `SkeletalMeshActor` | Villager mesh with hat / eye patch / bandana / beard accessory components attached to sockets |
| `ASAMUInteractable_Actor` (+ `ASAMUInteractableInterface`) | `DynamicSMActor` | Story interactables (§7) |
| `ASAMUCollectible` | `DynamicSMActor` | Collectibles (§8) |
| `ASAMUGlowFlower` | `InterpActor` | Glow flowers (§9) |
| `ASAMUSoundMakingFoliage` | `StaticMeshActor` | Rustling plants (§10) |

| Value | Source | Confidence |
|---|---|---|
| NPC pawn `GroundSpeed` 200 | `asamu.ASAMUNPC_Pawn` | CONFIRMED (cdo) |
| NPC pawn collision 34 × 78 (worm, Maddie: inherited) | template `Engine.Default__Pawn.CollisionCylinder` (the `GamePawn`, `UDKPawn` and `ASAMUNPC_Pawn` templates set no size; corrected from `UDKPawn` in the verification pass) | CONFIRMED (cdo) |
| Villager collision 16 × 52, mesh offset −52 z | `asamu.Default__ASAMUNPC_VillagerPawn` templates | CONFIRMED (cdo) |
| Villager `PeripheralVision` 0.4, `SightRadius` 5 000, `bCanWalkOffLedges` | `ASAMUNPC_VillagerPawn` / `Engine.Pawn` | CONFIRMED (cdo) |
| Villager `reachedDestinationTolerance` 300 | `asamu.ASAMUNPC_Villager` | CONFIRMED (cdo) |
| Worm pawn `SightRadius` 10 000, `PeripheralVision` −1 | `asamu.ASAMUNPC_WormPawn` | CONFIRMED (cdo) (the AI never uses sight) |
| Worm light `Radius` 15 000, `Brightness` 2 500, cones 90/45; the placed worm: 100 000, 25, 40/15, colour (255, 247, 247) | cdo; map instance | CONFIRMED (cdo, map) |
| Maddie `EyeOffset` 40 (never read) | `asamu.ASAMUNPC_MaddiePawn` | CONFIRMED (cdo, src) |
| Maddie mesh `Maddie.Maddie`, tree `Maddie_AnimTree`, set `MaddiePaperAirplanes` | template | CONFIRMED (cdo) |
| Worm mesh `Dark_Cave_worm.SkeletalMesh.Worm_SkeletalMesh`, tree `WormAnimTree`, set `WormAnimSet` | template | CONFIRMED (cdo) |

## 3. The worm

### 3.1 Tuning (class defaults of the controller `asamu.ASAMUNPC_Worm`) — CONFIRMED (cdo)

`PlayerPush` 500, `wakeUpTime` 4, `awakeTimeMin` 6, `awakeTimeMax` 8, `alertedSleepTime` 1, `alertedTime` 4,
`fallAsleepTime` 2, `sleepTimeMin` 12, `sleepTimeMax` 14, `screamTimeMax` 8, `zVelocityOffset` −50,
`lookAroundSpeed` 0.5, `positionSensitivity` 25; `lookAroundInterval` 2 and `lookAtPlayerDuration` 3 are never
read (src). The controller is spawned at run time, so no map can override these. Every worm loop steps every
0.1 s (src).

### 3.2 States and behaviour — CONFIRMED (src) unless noted

- **Disabled** (initial state): sleep pose (the sleep blend list on `SleepIdle`, looping), look-at off. Nothing
  else. Kismet `SeqAct_StartWorm` moves it to **Idle**, which immediately continues to **WakingUp**; starting also
  clears the shut-down flag.
- **WakingUp**: fires worm event `WakingUp`, restarts every animation node of the tree, shows the wake-up
  animation and ramps its eye light from 0 to full over `wakeUpTime` in 0.1 s steps; then **Awake**. The player is
  not checked while waking.
- **Awake**: fires `Awaken`; picks an awake time in [`awakeTimeMin`, `awakeTimeMax`]; light full; remembers the
  player's position as the reference; every 0.1 s, until the awake time has passed, it checks the player. The
  check succeeds only if the player pawn's location is inside a scream volume, the worm is not paused, the player
  is not inside a shadow volume, and the player has moved more than `positionSensitivity` from the reference. A
  check that finds the player inside the volume (not hiding) but not moving refreshes the reference to the
  current position — so slow creeping in steps below 25 uu between checks is never noticed. A success goes to
  **Alerted**. When the awake time runs out the checks simply stop and a "sleep queued" flag is set; the worm
  falls asleep at the next end of a "look back to the middle" animation (§3.3).
- **Alerted**: fires `Alerted`, turns head and eyes to the player, restarts the tree's nodes and plays the
  "discovered" animation, starts a one-shot `alertedTime` timer, waits `alertedSleepTime`, takes a fresh reference
  position and then checks every 0.1 s (here the pause flag is **not** consulted). Movement → **Screaming**
  (the alert timer is cancelled). The timer firing first fires `FinishedAlerted` and returns to **Awake** (a new
  awake period).
- **Screaming**: fires `Screaming`, starts the looping camera shake `Zeth_CameraStuffs.MonsterGrowl`, starts a
  one-shot `screamTimeMax` timer if none runs, plays the scream. Every 0.1 s it **pushes the player**: the
  grapple is released (the common release, GRAPPLE.md G-RL-1/5), physics set to Falling, and
  `(unit(player − worm).x · 500, unit(player − worm).y · 500, −50)` is **added** to the velocity (the X/Y
  components of the 3D unit vector, Z replaced). After each push, if the player has left every scream volume the
  worm stops screaming: `StoppedScreaming`, camera shake off, `FallingAsleep`, **Sleeping**.
  If the scream timer fires: `StoppedScreaming`, the player dies (`PlayerDied`), and the game's kill notification
  reaches the worm, which (still screaming) stops screaming as above. The scream timer is only cleared by
  **Sleeping**, so if Kismet restarts the worm mid-scream the old timer can still kill the player later.
- **Sleeping**: camera shake off, sleep animation, "discovered" and "screaming" flags cleared, scream timer
  cleared; dims the light from full to 0 over `fallAsleepTime` (0.1 s steps); then, unless shut down, sleeps a
  random time in [`sleepTimeMin`, `sleepTimeMax`] and (if still not shut down) wakes up (**WakingUp**).
- **ScriptedMove**, **ScriptedRouteMove**: only log a message; nothing enters them.

Kismet entry points (src; port labels from the class defaults, cdo):

| Action | Effect |
|---|---|
| `SeqAct_StartWorm` (`Start`) | → Idle (→ WakingUp), clears shut-down |
| `SeqAct_ShutDownWorm` | sets shut-down; in Awake also queues the sleep. A sleeping worm then never wakes; a worm in other states finishes its current episode normally and stays asleep afterwards |
| `SeqAct_PauseWorm` (`UnPause` = input 0, `Pause` = input 1) | sets the pause flag, which only gates the Awake check |
| `SeqAct_WormResetSleepTimer` | in Sleeping only: restarts the light-dimming counter (not the random sleep); unused by any map |
| `SeqEvent_WormEvents` outputs | 0 `WakingUp`, 1 `Awaken`, 2 `FallingAsleep`, 3 `Alerted`, 4 `Screaming`, 5 `StoppedScreaming`, 6 `FinishedAlerted`; every instance in the level fires (when enabled) |

Any kill-zone death also sends the kill notification (kill zones and dynamic kill zones call it after
`PlayerDied`, src), so dying in a kill zone while the worm screams puts it to sleep. No other death does: falling
below `KillZ` (the pawn's `Died` override), Kismet `SeqAct_PlayerDied`, the restart key and the pause-menu restart
only call `PlayerDied`, and the worm's own scream timeout is the only other `NotifyKilled` caller in the `asamu`
package (CONFIRMED (src)). A worm screaming through such a death keeps pushing the respawned player while the
player stays inside the volume, and its scream timer still runs.

Kismet use in AG-Darkcave (sanitized, CONFIRMED (map)): five touch volumes and the time-trial branch start the
worm; a narrator line un-pauses it; the shut-down action is followed by a pause; the worm events drive sounds,
boolean variables, an integer comparison and adaptive-music volume.

### 3.3 Animation tree and the look-around — data CONFIRMED, natives CONFIRMED, emulation TENTATIVE

The pawn's tree (`Dark_Cave_worm.Animations.WormAnimTree`, CONFIRMED (data)):

```text
StateAnimation  0 SleepState    0 FallAsleep (Worm_FallAsleep)     1 SleepIdle (Worm_SleepIdle, loops)
                1 WokeUpState   0 WakeUp (Worm_WakeUp1)            1 LookAroundList
                                     LookAroundList 0 LookingLeftList   0 LongLookRight   1 LeftToMiddle
                                                    1 LookingMiddleList 0 ShortLookleft   1 ShortLookRight
                                                    2 LookingRightList  0 LongLookLeft    1 RightToMiddle
                2 AlertState    0 DiscoveredIdle (loops)           1 Scream
```

Only the six look nodes have `bCauseActorAnimEnd`; only `ShortLookleft` and `ShortLookRight` start with
`bPlaying`. The five other sequence nodes are `UTAnimNodeSequence` (`UDKAnimNodeSequence`) with `bAutoStart`
false and no sequence stack, so they behave as plain sequence nodes; in particular `FallAsleep` raises no
animation-end event and the pawn's `FallAsleep` → `SleepIdle` switch is dead code: a sleeping worm holds the last
frame of `Worm_FallAsleep` (CONFIRMED (data, cdo, src)). `bPlayActiveChild` is set on every blend list except
`StateAnimation` and `WokeUpState` (CONFIRMED (data): `AlertState`, `SleepState`, `LookAroundList` and the three
look lists). Sequence lengths (`SequenceLength`, `RateScale` not stored = 1, CONFIRMED (data)): ShortLookLeft
1.4583334, ShortLookRight 1.4166666, LongLookLeft 2.4583333, LongLookRight 2.5, LeftToMiddle 1.2916666,
RightToMiddle 1.9166666, WakeUp1 3.9583333, FallAsleep 2.5, SleepIdle 5.8333335, DiscoveredIdle 3.125,
Scream 0.8333333 s.

The pawn's animation-end handler (src): a look to the left/right side switches the look-around list to that
side and tells the controller "finished looking to side"; a look back to the middle switches to the middle list
and tells it "finished looking to middle". Only Awake reacts: after a side look it looks back to the middle if the
sleep is queued, on a 50 % roll, or if shut down — otherwise it looks to the other side; after a middle look it
goes to sleep if queued or shut down, otherwise looks to a random side.

Engine rules this depends on (CONFIRMED (native), locally decompiled):

| Rule | Native |
|---|---|
| `SetActiveChild` sets the weights; when the list has `bPlayActiveChild` and the new active child is a sequence node it **replays** that node (`ReplayAnim`: `PlayAnim` from time 0 with the node's own looping flag and rate) — on every call, also when that child was already active | `UAnimNodeBlendList::SetActiveChild @ 0x1006FF8F0`, `UAnimNodeSequence::ReplayAnim @ 0x1006EE450` |
| `PlayAnim` on a blend node restarts **every** sequence node below it (position, rate, looping) | `UAnimNodeBlendBase::PlayAnim @ 0x1006FC2A0`, `UAnimNodeSequence::PlayAnim @ 0x1006EE2E0` |
| A non-looping node that reaches its end stops and then calls `OnAnimEnd` (actor event when `bCauseActorAnimEnd`) | `UAnimNodeSequence::AdvanceBy @ 0x1006EA240`, `OnAnimEnd @ 0x1006EA9C0` |
| Only nodes with weight tick | `USkeletalMeshComponent::TickAnimNodes @ 0x100BD6110` |
| State code that changes state continues with the new state's code in the same tick (≤ 4 changes) | `AActor::ProcessState @ 0x100B40E20` |

Every look selection (`LookToSide`, `LookToMiddle`, `FoundPlayer`) goes through one of the three look lists,
which replay the chosen node from its start, so the look chain never stalls: the worm keeps looking around for
as long as it is awake (`LookAroundList`'s own switches in the animation-end handler replay nothing, its
children being lists, but the controller's answer always selects through a look list). Once the awake checks
have ended (or the worm is shut down) every side look is answered with a look back to the middle and the end of
that middle look sends the worm to sleep. An undisturbed worm therefore sleeps between 0 and about 4.5 s
(longest side look plus longest middle look) after its awake time ran out. Our emulation (§12) gives, over 200
random streams of our generator, an awake period of 9.3 s on average, 2.3 s past the awake time (at most
4.5 s) — the timing chain is CONFIRMED (src, data, native); the exact figures are TENTATIVE until checked against
a trace of the original. Simplifications (TENTATIVE): cross-blends are ignored, so only the fully active path
ticks (in the original a look node keeps ticking during the 1 s blend into `Alerted`, so after an alert the first
look can end up to 1 s early), and the pawn's animation is assumed to update before its controller's state code.

**Correction (verification pass):** an earlier version of this section said `bPlayActiveChild` was false in this
tree and predicted that an undisturbed worm usually stalls awake (no further player checks) on an already played
look node. The tree's list objects carry `bPlayActiveChild = true` (all but two lists) and the native replays
the selected sequence on every `SetActiveChild`, so no such stall exists.

The aim (head/eye look-at, visual): every tick the controller moves its aim towards the player at
`lookAroundSpeed · 5 · dt` while the player is discovered, otherwise towards the world origin at
`lookAroundSpeed · dt` (the "centre" is the zero vector, a quirk) (src). The eye light's brightness and radius are
the placed values times the light strength (src).

## 4. Maddie and the backpack Maddie — CONFIRMED (src) unless noted

- `ASAMUNPC_MaddiePawn`: its tick aims head and eyes at the player; auto state `Idle` (empty);
  `TalkingWithPlayer` returns to `Idle` as soon as the player is at least 200 uu away — but no script, Kismet
  action or map enters it (CONFIRMED (src, map)). Its loop has no latent call, so a close player would make the
  original spin until the engine's runaway-loop guard (TENTATIVE, never reached). The `AnimIsTalkingNode` it
  declares is never assigned.
- `ASAMUBackpackMaddie`: spawned at the player by `SeqAct_MaddieBackpack` input `Enable` and attached to the grapple
  hand mesh's `RootSocket`; `Disable` destroys it. `SeqAct_PlayMaddieBackpackAnim` plays `Maddie_Arm_Animtest`
  (the only enum value, `Wave`). Mesh `Maddie.Meshes.Maddie_Arm_Animtest`, set `Maddie_Arm_Animset` (cdo). No map
  uses either action (map).

## 5. Villager pawns — CONFIRMED (src) unless noted; never placed

- **Idle** (auto): with 20 % probability waits a random 1–3 s; then walks its scripted path if
  `bUseScriptedPath`, else roams.
- **WalkingScriptedPath**: walks to the first `PathNode` of `scriptedPath`, then forward node by node (each move
  completes, then a 0.1 s pause), waits 0–5 s at the end, walks back node by node, and repeats.
- **Roaming**: picks a random reachable destination (`FindRandomDest`); with none it warns and disables itself.
  While walking there it re-plans if unreachable and, when it sees the player within 300 uu, turns to face the
  direction of the player's **location vector** (not the direction from itself to the player — a quirk); then
  goes back to Idle. Sight toggles between "see player" and "enemy not visible".
- **TalkWithPawn** (pushed by `StartTalkingWithPawn`, popped by `StopTalking`; no caller): focuses the other pawn.
- The villager pawn tree (`AnimTree_Villager_Adult_01`) blends by speed between two sequence nodes that carry no
  `AnimSeqName` (CONFIRMED (data)), so which animations it would show is UNKNOWN.

With no `PathNode` in any map and only one `PlayerStart`, a placed villager could only roam to the player start.

## 6. Ambient skinned actors (what players actually see) — CONFIRMED (data, map)

The villagers and Maddie seen in BeautifulCity and StarHaven are `SkeletalMeshActor`s whose mesh component has
its own `AnimNodeSequence` with `AnimSeqName`, `bLooping`, `bPlaying` and a start `CurrentTime` (for example a
seated villager looping `StrayVillager_Sitting1_01`). Census: sitting, talking, leaning, idle,
bench, cliff-dangling, hammering, smithing, farming, lying and child variants (129 nodes, 125 with a sequence
name; all play looped except one `Captain_Idle`, which does not loop, and three nodes that do not start
playing). Meshes: `Villagers.Meshes.{Villager,Stray}_{Adult,Child}_01`, `Maddie.Maddie`,
`Misc_Characters.*`, `Murmur_Character_Package.Samuel_Cove_Samuel`, `Maddie_Assets.Maddie_Book_Idle`. In
BeautifulCity, StarHaven and Darkcave 13 of them (plus TheCore's single one) are referenced by a Matinee (`SeqAct_Interp`; animation control
tracks not imported).

**Importer gap (integration item):** the scene export (`asamu-import levels`) carries each skeletal component's
mesh but not its animation node. The runtime reads an optional component field

```json
"animation": {"sequence": "StrayVillager_Talk_02", "looping": true, "playing": true, "start_time": 4.0, "rate": 1.0}
```

(`AnimSeqName`, `bLooping`, `bPlaying`, `CurrentTime`, `Rate` of the component's `Animations` subobject). Until the
importer writes it, the app plays the mesh's first idle-named sequence (ours).

## 7. Story interactables — CONFIRMED (src) unless noted

- Interaction is the story-mode fire within 200 uu (GRAPPLE.md G-AC-0), not the `use` key. `use` is ignored outside
  story mode; in story mode it runs the stock use search, which finds only actors with a Kismet `SeqEvent_Used`,
  and no map has one (CONFIRMED (src, map); the stock search TENTATIVE).
- An interaction is accepted while `MaxInteractTimes` is 0 (unlimited) or the use count is below it (default 1,
  cdo). Accepted: the count rises; an item in `Idle` with a glow mesh starts fading its interact symbol (101 steps
  of 0.01 s, then `Idle` if uses remain, else `Disabled`); every `SeqEvent_ActorInteractedWith` is offered the
  activation — each counts it against its own `MaxTriggerCount` regardless of originator, and only those whose
  originator is this actor fire (a quirk: an event instance can be used up by other actors' interactions).
- Linking: a non-parent with `linkedParentActor` registers with that parent at begin play. Interacting with such
  a child makes the parent fade, register itself as an optional story item if `bIsOptional`, fire the
  interacted event **with the parent as originator**, and exhaust and fade all its children. Interacting with a
  parent does the same with itself. A stand-alone optional item registers its (null) parent — the per-level
  `<level>None` key of SAVE.md 6.3/Q5. The 11 keys feed `INTERACT_ALL_STORY` (SAVE.md).

## 8. Collectibles — CONFIRMED (src, cdo, map)

Pick-up is a touch by the player pawn of the actor's trigger cylinder (radius 50, half-height 40, 40 uu above the
actor; template `asamu.Default__ASAMUCollectible.Trigger`), once (`bCollected`). Collecting fades the hum, stops
the beep, plays the turn-off and paper-rip sounds, registers the collectible with the progression manager
(SAVE.md 6.3: per level, no duplicates, 5 per level, 25 in total), sets the collected material and fires every
`SeqEvent_CollectibleCollected` (output 0). The static mesh blocks the player. In a time-trial game the
collectible is hidden and has no collision. The collected state is saved in the world snapshot and restored from
it (SAVE.md).

## 9. Glow flowers — CONFIRMED (src, cdo)

`NotGlowing` → (grappled) → `Glowing`, spawning the grappled particle effect each time. Glowing: the glow time is
set to `glowDuration` (10), the glow sound plays and the lights fade in over `FadeTime` (1) with `FInterpTo`
steps of `UPDATE_RATE` (0.016667 s), hold while glow time remains, then fade out over `FadeTime` and return to
`NotGlowing`. A grapple while glowing resets the glow time and is remembered: at the start of the fade-out the
flower jumps back to the fade-in (the sound replays, the lights dip and rise again) and, since the hold time has
already been spent, fades out right after. The grapple itself (release-instant, costs a grapple) is GRAPPLE.md
G-WO-2. `FInterpTo` is the stock formula (TENTATIVE: not re-read).

## 10. Sound-making foliage — CONFIRMED (src, cdo)

Every touch by any actor plays the actor's `TouchSound` (`MiscSounds.Foliage_Rustle_Cue` by default) at its
location. Collision type touch-all-but-weapons; the touch shape used here is the trigger cylinder (radius 50,
half-height 40, +40 z; template) — whether the foliage mesh's own collision also produces touches is TENTATIVE.

## 11. First-person hands — CONFIRMED (cdo, src) unless noted

The grapple gun's `FirstPersonMesh` is `PlayerHand.Meshes.PlayerHand` with `PlayerHand.PlayerHand_AnimTree` and
the `PlayerHand.Root` set, foreground depth group, its own FOV 70° (cdo). The gun places it at the pawn's view
location plus the weapon bob, with the controller's rotation; its X/Y/Z offsets are never written (zero), so the
mesh geometry carries the hand's placement (src). Bob: `BobDamping` 0.15, `JumpDamping` 1.0 (cdo), ABILITIES.md
A-CM-5. The tree switches between sequences named `Idle`/`Idle_02..04` (random), `Sprint`, `JumpIdle`, `Falling`,
`Jump`, `JumpLand`, `Grapple`, `Grapple_Loop`, `Grapple_Release`, `rocketBoots`, `RocketBoots_Land`,
`powerJump`, `PowerJumpIdle`, `PowerJumpLand`, `HandsDown`, `SuitOn` (CONFIRMED (data)); which blend nodes the
gun drives at which moment is only mapped approximately here (TENTATIVE).

## 12. Implementation in this repository

| Piece | Where |
|---|---|
| Definitions from converted scenes (worm, worm volumes, Maddie/villager pawns, navigation points, collectibles, story items, glow flowers, foliage, other skinned actors), skeletal manifest index | `crates/asamu-world/src/npc.rs` (`load_npc_scene`, `load_npc_scene_for_map`, `SkeletalIndex`) |
| Worm state machine with the emulated tree, timers, ProcessState rule; Maddie, backpack, villager state machines; story items, collectibles, glow flowers, foliage | `crates/asamu-world/src/npc.rs` (`NpcRuntime`, `tick_worm`, `tick_villager`, …) |
| Game side: spawn from a loaded map, tick, apply the push and the kill, route the player's handler calls, Kismet entry points, `use`, hand animation mapping and bob | `crates/asamu-game/src/npc.rs` (`NpcSystem`, `apply_worm_push`, `hand_animation`, `hand_bob_offset`) |
| Rendering: skinned actors (Bevy skins, `AnimationPlayer`, state-driven clips), hands on an overlay camera, state gizmos | `apps/asamu/src/npc.rs`, `apps/asamu/src/npc/{skins,hands}.rs` |

Wiring into `Game::tick` is documented in `crates/asamu-game/src/npc.rs` (module docs); until it lands the app
runs the NPC system after each game tick, so the push acts one tick later than in the original, and forwards
only kill-zone deaths to the worm (`death_notifies_npcs`, read from the game's tick reports). Our own choices
(not original): the random generator; NPC pawns are not collision; villager movement is a straight kinematic walk
at `GroundSpeed`; the idle stand-in animation; animation blend times; the overlay light.

Tests: `crates/asamu-world/tests/npc_worm.rs` (worm timings, checks, pushes, scream timeout and kill, kill-zone
notification, a Kismet restart mid-scream keeping the scream timer, sleep/wake, shut-down, the look chain
replaying its nodes and never stalling, the sleep delay after the awake time over 200 seeds, determinism, hostile
`dt`), `npc_actors.rs` (collectibles, time trial, foliage incl. 1 000-plant index and hostile trigger radii,
story-item counting/linking/registration and fade timing, a parent fading without a glow mesh, glow-flower timing
and regrapple, Maddie, backpack, villager paths/roaming/talk/empty paths), `npc_scene.rs` (synthetic scene
loading, streamed sub-level ids, hostile/truncated JSON, hulls, skeletal index and unsafe glTF paths, real-data
census of AG-Darkcave and of all six story maps), `crates/asamu-game/tests/npc_system.rs` (push application,
full scream-to-kill run, routing, touches, Kismet entry points, spawn from scene JSON, real-data worm alert on
converted AG-Darkcave).

Real-data check (local, `asamu-import levels` output of AG-Darkcave): 1 worm, 1 scream volume (the worm and a
point 1 500 uu from it inside), 6 stored look targets, 5 collectibles, 3 story items, 15 flowers, 406 foliage,
0 warnings; a player moving in the lair alerts the awake worm. CONFIRMED (test).

Verification pass (local, the six story maps and their streamed sub-levels converted with `asamu-import levels`,
plus `asamu-import skeletal`; `real_story_map_census`): collectibles 0/5/5/5/5/5 (Workshop, Paradise, Beautiful,
Dark, StarHaven, Ice; 25 in total), story items 6/12/4/3/17/22, foliage 1 018 (Paradise), 406 (Dark), 410
(StarHaven), 15 glow flowers and 1 worm in Darkcave, no Maddie or villager pawn, 0 warnings; interacting with
every story item through the runtime registers exactly **11** distinct optional story keys, 5 of them the
per-level `<level>None` key (Workshop, Beautiful, Dark, StarHaven, Ice) — SAVE.md 6.3 and
`TOTAL_INTERACTABLES_COUNT` agree. Every skinned component of the maps resolves to a converted skeletal mesh
(BeautifulCity 70 actors, StarHaven 64, Darkcave 2 with a mesh — its third `SkeletalMeshActor` has none —,
IceCave's TheCore 1). The app (`--fly` on converted StarHaven and Darkcave, local screenshots) draws the
villagers posed by their animation and the worm. CONFIRMED (test, local run).

## 13. Open items

- Trace from the original: worm cycle timings (the awake period and the sleep delay after it, §3.3), push and
  scream timing relative to the player's physics.
- The importer should export each skeletal component's animation node (§6) and Matinee animation tracks.
- NPC pawn collision for the player; the worm's eye spot light and `MonsterGrowl` camera shake in the renderer.
- The stock `Encompasses`, use search and `FInterpTo` natives were not re-read (TENTATIVE).
