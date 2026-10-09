# Kismet

Structural recovery of the Kismet (UE3 visual scripting) graphs in every shipped map. The graphs show what
each level does: when it starts, when it unlocks abilities, which checkpoints it triggers, and which map it
loads next. This page publishes structure, counts and class names only. The full graphs contain object
names, editor comments, actor references and dialogue cue ids, so they stay local (see
[Local outputs](#local-outputs)).

**Status (CONFIRMED):**
- All 12 maps build into graphs with 0 decode failures, 0 decoder warnings, 0 dangling links and 0
  unresolved name-matched links.
- There are 3,504 Kismet objects: 3,481 in level sequence trees and 23 in prefab archetypes. The level
  trees hold 3,345 stored links, plus 61 derived ones.
- `Startup.upk` holds no Kismet instances, only the classes and their default objects.
- The data-derived level order is in [LEVELS.md](LEVELS.md).

## Tooling and reproduction

- `crates/asamu-ue3/src/kismet.rs` builds the graph model (`build_graph`, `build_graph_for`,
  `KismetGraph::summary`, `KismetGraph::to_dot`).
- The CLI is `tools/asamu-inspect` with the `kismet` subcommand.

```sh
export CARGO_TARGET_DIR=target/wf2-kismet
C="$HOME/Library/Application Support/Steam/steamapps/common/A Story About My Uncle/A Story About My Uncle.app/Contents/Resources/ASAMU/CookedMac"
cargo run --release -p asamu-inspect -- kismet "$C"                                # one row per map
cargo run --release -p asamu-inspect -- kismet "$C/Maps/AG-IceCave.asamu"          # text summary + milestones
cargo run --release -p asamu-inspect -- --json kismet "$C/Maps/AG-IceCave.asamu" --summary
mkdir -p research/local/kismet
cargo run --release -p asamu-inspect -- kismet "$C" --out-dir research/local/kismet  # full JSON + DOT, LOCAL ONLY
cargo test -p asamu-ue3 --test kismet --test kismet_real_data
```

`--out` and `--out-dir` use the same guard as `decompress`. They refuse paths inside the repository (except
git-ignored `research/local/` and similar folders), inside the game install, and paths that are symlinks.

## How Kismet is stored in a cooked map (v868)

| Fact | Evidence | Confidence |
|---|---|---|
| A Kismet object is any export whose class chain contains `Engine.SequenceObject`. Its role comes from the nearest of: `SequenceFrame` (comment box), `Sequence` (also `PrefabSequence` and `PrefabSequenceContainer`), `SequenceEvent`, `SequenceAction`, `SequenceCondition`, `SequenceVariable`, `SequenceOp`. | Class chains resolved through `PackageSet`; every Kismet export in the 12 maps classifies, with 0 unresolved classes. | CONFIRMED |
| `InterpData` (Matinee data) is a `SequenceVariable`. `SeqAct_Interp` reaches it through a variable link labelled `Data`. | Class chain; 158 Matinee edges, each from a `SeqAct_Interp` to an `InterpData`. | CONFIRMED |
| Every link is an ordinary tagged property, decoded by the object decoder ([OBJECT_FORMAT.md](OBJECT_FORMAT.md)). The table below lists them. | Exact decode of all 3,504 objects with 0 warnings. | CONFIRMED |
| A cooked object stores only values that differ from its archetype, or from its class defaults when it has no archetype. Port arrays missing from an object come from those layers. | Prefab instances in AG-StarHaven: one instance `SeqAct_Interp` omits `InputLinks`, and its archetype holds an identical array. | CONFIRMED (one case) / STRONG (rule) |
| References inside a prefab archetype point at archetype objects. The builder remaps inherited references onto the matching instance objects, found through the export table's archetype field within the same instance sequence. | Synthetic test `prefab_instances_inherit_from_their_archetype`. In the shipped data no inherited array contains a reference, so the remap never fires there. | STRONG (UE3 rule) / CONFIRMED (no real case affected) |
| `ParentSequence` always agrees with the sequence whose `SequenceObjects` lists the object. | 0 parent mismatches and 0 unlisted members in all maps. | CONFIRMED |
| Every stored link stays inside one sequence. Only remote events cross sequence boundaries. | 0 stored cross-sequence edges; 42 cross-sequence edges, all derived remote-event edges. | CONFIRMED |
| The shipped maps never use sub-sequence boundary links: no `SeqVar_External`, `SeqEvent_SequenceActivated` or `SeqAct_FinishSequence`, and no `LinkedOp` on a sequence's own input or output links. Sub-sequences are self-contained and react to their own events and to remote events. | Class census; 0 `subsequence_*` edges. | CONFIRMED |
| Each remote event resolves within its own map, and so does each named variable. | 0 unresolved. TheCore, which is streamed into AG-IceCave, has no remote-event actions or listeners. | CONFIRMED |
| Root sequences have a `Level` export as their outer. Prefab archetype sequences sit under a `Prefab` export inside a top-level package export, and are not part of the level's tree. | AG-StarHaven has 3 prefab archetypes (23 objects) and 10 prefab instances. | CONFIRMED |
| Remote events are matched only within one map's level tree. | Unresolved count is 0, but how the engine searches across streamed levels at run time is not established. | TENTATIVE |

Link properties as decoded. Struct names are those carried by the tags.

```text
SequenceOp.InputLinks[]     SeqOpInputLink        LinkDesc, ActivateDelay, bDisabled, bDisabledPIE, LinkedOp, DrawY, ...
SequenceOp.OutputLinks[]    SeqOpOutputLink       Links[] (SeqOpOutputInputLink: LinkedOp, InputLinkIdx),
                                                  LinkDesc, ActivateDelay, bDisabled, bDisabledPIE, LinkedOp, ...
SequenceOp.VariableLinks[]  SeqVarLink            ExpectedType, LinkedVariables[], LinkDesc, LinkVar, PropertyName,
                                                  bWriteable, MinVars, MaxVars, ...
SequenceOp.EventLinks[]     SeqEventLink          ExpectedType, LinkedEvents[], LinkDesc, ...
SequenceObject              ParentSequence, ObjName, ObjComment, ObjPosX/Y, DrawWidth/Height, ObjInstanceVersion
Sequence                    SequenceObjects[], bEnabled, DefaultViewX/Y/Zoom
SequenceEvent               Originator, MaxTriggerCount, ReTriggerDelay, bEnabled, bPlayerOnly, Priority, ...
SequenceVariable            VarName; SeqVar_Object.ObjValue, SeqVar_Int.IntValue, SeqVar_Bool.bValue (an int),
                            SeqVar_String.StrValue, SeqVar_Named.FindVarName, ...
SeqAct_ActivateRemoteEvent  EventName  -> matched with SeqEvent_RemoteEvent.EventName
SeqAct_ConsoleCommand       Commands[] (strings)
SeqAct_MultiLevelStreaming  Levels[] (LevelStreamingInfo: Level, LevelName)
```

A variable link with a `PropertyName` feeds the named op property from the linked variables, for example
`SeqAct_SetMaxGrapples.Grapples`. The summary reports linked values as `Prop<-value`. When no variable is
linked it reports the effective property value (own, then archetype, then class default, then the zero
value).

## Graph model

`KismetGraph` (JSON `format` = `asamu-kismet-graph`, `version` = 1; field order and enum spellings are
pinned by `graph_json_format_is_stable`):

- **nodes**, in export order. Each node has an id, export index, path, class, kind, `custom` (the class is
  from the `asamu` package), scope (`level` / `prefab` / `detached`), parent sequence, archetype, editor
  label and comment, and `enabled`.
  - Ports: inputs, outputs, variables and events, with labels, delays, disabled flags and link counts.
  - Events: originator and its class, `MaxTriggerCount`, `ReTriggerDelay`, `bPlayerOnly`, priority.
  - Variables: `VarName`, the value and where it came from, `FindVarName`, label.
  - `params`: the effective values of every property declared below the generic Kismet base classes.
- **edges**, typed:

  | Kind | Meaning |
  |---|---|
  | `output` | activation, from an output port to an input port, with the output delay |
  | `variable` | a variable link |
  | `matinee` | a `SeqAct_Interp` link to its `InterpData` |
  | `event` | an event link |
  | `subsequence_input` / `subsequence_output` | a sequence-boundary `LinkedOp` |
  | `remote_event` | derived: an `ActivateRemoteEvent` action to listeners with the same `EventName` |
  | `named_variable` | derived: a `SeqVar_Named` to variables with that `VarName` |

  Each edge carries `derived` and `cross_sequence` flags.
- **dangling**: stored references that do not resolve to a node of the right kind. Reasons are
  `null_target`, `import`, `not_kismet`, `wrong_kind`, `input_index_out_of_range`, `other_scope` and
  `bad_index`. Input indices are checked against the target's effective input count.
- **unresolved**: name matches with no partner.
- **summary** (publishable): counts by kind, class and custom class; event classes; sequence counts; link
  counts; disabled elements; feature counts; map transitions; streamed levels; and milestones. A milestone
  is an ability, checkpoint, story-mode, streaming or console action, together with the events it can be
  traced back to through activation, sub-sequence and remote-event edges. The trace passes through
  conditions without evaluating them.
- **DOT**: one cluster per sequence (nested). Shapes follow node kind, ASAMU classes are orange, comment
  frames are omitted, and edge styles follow edge kind.

## Per-map statistics (CONFIRMED)

These figures cover the level scope only; AG-StarHaven additionally holds 23 prefab-archetype objects
(1,074 Kismet objects in the package). In
the "seqs" column, `c` = prefab containers, `i` = prefab instances and `a` = prefab archetypes. "Remote"
and "named" are derived edges.

| map | objects | seqs root/sub (+prefab) | events | actions | conds | vars | frames | stored links | output | variable | matinee | event | remote | named | delayed outputs | disabled events |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| AG-Workshop | 257 | 1/4 (+2c) | 31 | 101 | 6 | 89 | 23 | 247 | 137 | 87 | 19 | 4 | 7 | 0 | 8 | 1 |
| AG-ParadiseCave | 549 | 1/8 | 83 | 195 | 22 | 193 | 47 | 524 | 279 | 188 | 35 | 22 | 7 | 0 | 0 | 4 |
| AG-BeautifulCity | 581 | 1/8 | 102 | 177 | 37 | 197 | 59 | 518 | 270 | 205 | 17 | 26 | 5 | 1 | 0 | 5 |
| AG-Darkcave | 432 | 1/7 | 72 | 158 | 35 | 125 | 34 | 407 | 231 | 137 | 6 | 33 | 5 | 4 | 4 | 5 |
| AG-StarHaven | 1,051 | 1/9 (+1c, 10i; 3a outside) | 151 | 316 | 48 | 450 | 65 | 1,088 | 495 | 487 | 57 | 49 | 14 | 1 | 7 | 17 |
| AG-IceCave | 427 | 1/8 | 76 | 131 | 15 | 162 | 34 | 393 | 189 | 163 | 13 | 28 | 12 | 1 | 1 | 13 |
| TheCore | 68 | 1/0 | 6 | 32 | 0 | 28 | 1 | 74 | 42 | 22 | 7 | 3 | 0 | 0 | 0 | 2 |
| AG-Epilogue | 80 | 1/3 (+2c) | 13 | 32 | 1 | 20 | 8 | 62 | 41 | 18 | 3 | 0 | 4 | 0 | 0 | 0 |
| ASAMUFrontEndMap | 30 | 1/0 (+2c) | 2 | 12 | 1 | 9 | 3 | 29 | 16 | 12 | 1 | 0 | 0 | 0 | 3 | 0 |
| ASAMULegal | 5 | 1/0 | 1 | 1 | 0 | 2 | 0 | 3 | 1 | 2 | 0 | 0 | 0 | 0 | 0 | 0 |
| ASAMUEntry | 0 | — | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 |
| Freds_place | 1 | 1/0 (empty) | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 |

Totals (level scope): 3,481 objects; 537 events, 1,155 actions, 165 conditions, 1,275 variables, 274
frames, 75 sequences. Sequences
nest at most 1 level deep, except in AG-StarHaven, where prefab instances inside a prefab container reach
depth 2.

### Event types by map (level scope)

| event class | Workshop | Paradise | Beautiful | Dark | StarHaven | Ice | Core | Epilogue | FrontEnd | Legal |
|---|---|---|---|---|---|---|---|---|---|---|
| Engine.SeqEvent_LevelLoaded | 6 | 17 | 35 | 15 | 50 | 13 | 1 | 4 | 2 | 1 |
| Engine.SeqEvent_Touch | 13 | 50 | 27 | 34 | 56 | 36 | 4 | 5 | | |
| Engine.SeqEvent_RemoteEvent | 6 | 7 | 5 | 5 | 14 | 12 | | 4 | | |
| Engine.SeqEvent_AnimNotify | | | 24 | | 12 | | | | | |
| asamu.SeqEvent_ActorInteractedWith | 6 | 2 | 4 | 2 | 6 | 3 | | | | |
| asamu.SaveGameState_SeqEvent_SavedGameStateLoaded | | 1 | 3 | 2 | 1 | | | | | |
| asamu.SeqEvent_CollectibleCollected | | 1 | 1 | 1 | 1 | 1 | | | | |
| asamu.SeqEvent_NarratorEvents | | 1 | 1 | 1 | 1 | 1 | | | | |
| asamu.SeqEvent_PlayerDied | | 1 | 1 | 1 | 5 | 1 | | | | |
| asamu.SeqEvent_PlayerReleasedGrapple | | 1 | 1 | 1 | 2 | 1 | | | | |
| asamu.SeqEvent_PlayerGrappled | | | | 2 | 1 | 6 | | | | |
| asamu.SeqEvent_PlayerRocketBoosted | | | | | 2 | | | | | |
| asamu.SeqEvent_PlayerLanded | | | | | | 1 | | | | |
| asamu.SeqEvent_TrackBeat | | 2 | | 4 | | 1 | | | | |
| asamu.SeqEvent_WormEvents | | | | 4 | | | | | | |
| asamu.SeqEvent_CreditsEnded | | | | | | | 1 | | | |

### ASAMU custom classes by map (level scope)

The maps use 49 distinct ASAMU Kismet classes. Columns run in story order: Workshop, ParadiseCave,
BeautifulCity, Darkcave, StarHaven, IceCave, TheCore, Epilogue. FrontEnd, Legal, Entry and Freds_place use
none.

| class | Wk | Pa | Be | Da | St | Ic | Co | Ep |
|---|---|---|---|---|---|---|---|---|
| SeqAct_NarratorLine | 6 | 10 | 11 | 22 | 30 | 13 | 5 | 4 |
| SeqAct_SetMaxGrapples | 1 | 2 | 2 | 1 | 1 | 1 | | |
| SeqAct_ToggleGrapple | | 1 | 1 | 1 | 1 | 1 | | |
| SeqAct_ToggleRocketBoots | | 1 | 1 | 1 | 3 | 4 | | |
| SeqAct_ToggleVisibleGrapple | 2 | 2 | | 1 | 2 | | | |
| SeqAct_TriggerCheckpoint | | 1 | 1 | 2 | 3 | 2 | | |
| SeqAct_ToggleCheckpointEnable | | | | | 1 | 2 | | |
| SeqAct_ToggleRestartFromCheckpointOption | 1 | 5 | 2 | 4 | 3 | | 1 | 1 |
| SeqAct_ToggleStoryMode | 1 | 4 | 6 | 6 | 9 | 4 | 1 | 1 |
| SeqAct_ToggleSpawnInStoryMode | | 1 | 2 | | 5 | 1 | | |
| SeqAct_StartTimeTrial / SeqAct_EndTimeTrial | | 1/1 | 1/1 | 1/1 | 1/1 | 1/1 | | |
| SeqCond_IsTimeTrial | | 7 | 12 | 15 | 13 | 5 | | |
| SeqAct_UnlockASAMUAchievement | | 2 | 2 | 2 | 3 | 3 | | |
| SeqAct_EditOrAddSaveString / SeqAct_GetSaveStringValue | | 2/1 | 1/1 | 1/1 | 2/2 | 1/1 | | |
| SeqAct_ShowTutorialPopup / SeqAct_HideTutorialPopup | 4/3 | 18/12 | | 1/– | 4/1 | 2/– | | |
| SeqAct_AddAdaptiveTracks | | 2 | | 1 | | 1 | | |
| SeqAct_EditMultiplierForAllTracks | | 2 | | 1 | | 1 | | |
| SeqAct_SetAdaptiveTrackVolumeMultiplier | | 7 | | 17 | | 4 | | |
| SeqAct_SetLookAtTarget | | | 4 | | 3 | | | |
| SeqAct_SetVelocityConeMaterial | | | 1 | | 1 | 1 | | |
| SeqAct_ToggleFollowCollision | | | 1 | 1 | 2 | | | |
| SeqAct_ToggleCrosshair | 3 | 3 | 1 | | | | | 1 |
| SeqAct_ToggleZoomAvailable | 1 | | | | | | 1 | 1 |
| SeqAct_ToggleAttractor | 1 | | | | | | 1 | |
| SeqAct_StartWorm / SeqAct_PauseWorm / SeqAct_ShutDownWorm | | | | 2/2/2 | | | | |
| SeqAct_ToggleFallingRocksActive | | | | | | 1 | | |
| SeqAct_PlaySuitOnAnimation, SeqAct_ShowTitleLogo, SeqAct_SetRotationToPlayerRotation, SeqAction_GFx_CustomInvoke_AS3_Menu | 1 each | | | | | | | |
| SeqAct_DisablePauseMenu | | | | | | | 1 | |
| SeqAct_SetGameFinished | | | | | | | | 1 |

Event classes are listed in the previous table.

### Gameplay milestones (CONFIRMED values; meaning as noted)

- **Max grapples** (`SeqAct_SetMaxGrapples.Grapples`, passed to the grapple gun's limit):
  - Workshop sets 0.
  - ParadiseCave sets 1, then 2.
  - BeautifulCity sets 2 or 3.
  - Darkcave, StarHaven and IceCave set 3.

  STRONG: a negative value means unlimited, and the limit counts grapples until the player lands. This
  comes from the grapple gun's behaviour, read locally.
- **Grapple enabled** (`SeqAct_ToggleGrapple`; `Enable` is never stored, so every instance uses the class
  default, true). Workshop has no grapple toggle. ParadiseCave is the first map with one, fired from a touch
  volume (STRONG: the grapple unlock). Every later story map fires it on a path from level start, so a
  player who loads straight into that map has the grapple. Which conditions guard that path is TENTATIVE.
- **Rocket boots** (`SeqAct_ToggleRocketBoots`):
  - ParadiseCave, BeautifulCity and Darkcave only ever disable them (`Enable` = false, stored), at level
    start.
  - StarHaven is the first map that enables them (`Enable` from the class default, true). It does so in 3
    places: an interaction and a touch inside a cutscene sub-sequence (STRONG: the unlock), and a path from
    level start in the sub-sequence that grants abilities (TENTATIVE: for loads straight into the map).
  - IceCave enables them on a path from level start and toggles them in 3 more places (2 on, 1 off).
- **Checkpoints**: 9 `SeqAct_TriggerCheckpoint` (Paradise 1, Beautiful 1, Dark 2, StarHaven 3, Ice 2), each
  bound to an `ASAMUCheckpoint` actor. They are fired by touches, plus one at Darkcave level start. A
  further 3 `SeqAct_ToggleCheckpointEnable` occur (StarHaven 1, IceCave 2; IceCave's are tied to a
  player-grappled remote event). Most checkpoints are `ASAMUCheckpoint` actors that need no Kismet. The
  story maps hold 1 to 28 of them each (Workshop 1, Paradise 24, Beautiful 12, Dark 17, StarHaven 25,
  Ice 28, Epilogue 1; TheCore none).
- **Narration**: 101 `SeqAct_NarratorLine` actions. Dialogue ids and cues are not reproduced here.
- **Time trial**: each of the 5 middle maps has one start and one end action plus 5–15 `SeqCond_IsTimeTrial`
  branches. The end of a time trial opens the main menu.

### Console commands issued by Kismet

`SeqAct_ConsoleCommand` carries the map transitions. Its other commands:
- **Native or script commands that exist:**
  - `DisableAllScreenMessages`: an engine command; its UTF-32 string is in the executable. CONFIRMED
  - `ToggleAdventureSuit` and `NormalMode`: exec functions in `asamu`. CONFIRMED
  - `ToggleCrosshair`: an `asamu` name. CONFIRMED
  - `SetSpeed` and `ChangeSize` (Epilogue and Workshop): `Engine.u` names, presumably the stock
    cheat-manager commands. Whether they take effect in a shipping build is UNKNOWN.
- **Commands with no handler anywhere:** `SaveLevelTwo`, `SaveLevelThree`, `SaveLevelFour` (issued by both
  Darkcave and IceCave), `CanGrapple`, `CannotGrapple`, `ThreeGrapples` and `TGCD`. No script package has a
  name for them, and the executable has no ASCII, UTF-16 or UTF-32 string for them. These look like
  leftover development commands that do nothing in the shipped build. STRONG

## Local outputs

`--out-dir research/local/kismet` writes three files per map: `<map>.kismet.json` (full graph),
`<map>.kismet.dot` and `<map>.kismet-summary.json`. All three are original-game-derived and git-ignored;
never commit them. Render a graph locally with `dot -Tsvg`.

## Open items

- Milestone traces walk through conditions (`SeqCond_IsTimeTrial`, `SeqCond_CompareBool`, save-string
  checks) without evaluating them, so "reachable from level start" can mean "on some branch". TENTATIVE
- Run-time semantics of individual ASAMU actions belong to the gameplay docs ([ABILITIES.md](ABILITIES.md),
  [GRAPPLE.md](GRAPPLE.md)). This page records only which actions are wired where and with what values.
- `InterpData` contents (Matinee tracks, curves, event-track names) are counted but not decoded into
  timelines.
- The engine's run-time search scope for remote events across streamed levels is not established. It does
  not matter for the shipped data.
