# Integration: one connected game

How the pieces built in Phases 5–6 run together as one game: the order of work inside a frame, where every event
goes, and how a level hands over to the next. Behaviour claims about the original carry confidence labels and point
at the evidence docs; everything else describes our runtime.

Code: `crates/asamu-game` (`kismet_host.rs`, `converted.rs`, `npc.rs`, `save.rs`, `smoke.rs`), `apps/asamu`
(`main.rs`, `kismet.rs`, `ui/flow.rs`, `audio.rs`, `audio/music.rs`, `npc.rs`, `converted.rs`).

## 1. Loading a level

`asamu_game::load_level_with_kismet_options(dir, map, LevelOptions { npcs, time_trial })` (the app always uses it;
`load_level_with_kismet` is the default options) loads, from the user's converted data:

1. the scene (`asamu_world::scene`): collision, streamed sub-levels (loaded but masked until Kismet streams them),
   gameplay actors;
2. the Kismet graphs of the map and its sub-levels and their Matinee data (`asamu_kismet::load_level_scripts`);
   actors that Matinee, attachment or Kismet move or re-collide become mover bodies with dynamic collision;
3. the NPCs and story actors (`NpcSystem::load_from_dir`, same actor ids as the game), attached to the `Game`;
4. the `Game` at the `PlayerStart`, then the `LevelScript` bound to it. Attaching the script undoes the hard-coded
   level-start ability table and drops it from the level, so the map's own Kismet decides the abilities and a save
   snapshot applied later is not overwritten by the table (KISMET_RUNTIME.md §9).

The app's level start (`ui::flow::level_start` + `prepare_script`) then follows the original's save semantics
(SAVE.md §5): the save session's level start (chapter unlock and pointer in story play), the snapshot applied at the
spawn (abilities, checkpoint table, reset to the latest checkpoint), and, before the first tick, the script's
start-save index (`Runtime::set_start_save_index`: the snapshot's checkpoint for this chapter, −1 without one, no
event in time trial) and the Kismet save strings from the general save (`Runtime::set_save_strings`). Time trial
also selects the time-trial game type (`Runtime::set_time_trial`, NPC collectibles hidden).

## 2. Frame order (one fixed tick, 60 Hz)

`apps/asamu` `fixed_tick` builds the `InputFrame` from the devices (cinematic mode removes movement, turning or
buttons, §4), then calls `LevelScript::tick(&mut game, &input)` (`Game::tick` on maps without Kismet):

| Step | Where | What |
|---|---|---|
| 1 | `LevelScript::tick` | note the mover under a standing player (simple basing, TENTATIVE) |
| 2 | `Runtime::tick` | **Kismet update**: level-start events on the first call, queued events, latent actions, Matinee (moves mover bodies through the host), narrator and beat timers. Game-affecting actions call the host at once (abilities, story mode, checkpoints, streaming, teleports, kills) |
| 2b | `LevelScript::apply_game_outputs` | outputs whose original actions act on actors synchronously: `SeqAct_StartWorm` / `ShutDownWorm` / `PauseWorm` → `NpcSystem`; `SeqAct_ToggleFollowCollision` → the actor's mover collision |
| 3 | `LevelScript::tick` | carry a based player with its mover |
| 4 | `Game::tick` → `tick_scene` | the **game frame** (GRAPPLE.md G-TM-2 order): (a) the player's input events; (b) map actors: world objects (crystals, flowers, attractor pads), falling rocks, then **NPCs** (`NpcSystem::tick_actors`: worm push, scream kill, collectibles, villagers, story items, glow flowers); (c) the death fade (A-DT-2); (d) controller, pawn physics, power jump, boots, gun; (e) touches along the move (scene volumes, triggers, kill zones, `KillZ`, then the NPC side: collectibles, foliage); deaths |
| 5 | `LevelScript::feed` | the tick's events become Kismet events for the next update: grapple/landing/boost handler calls, touches, the death sequence's reset (`PlayerDied`), interactions, and the NPC events (worm, collectibles, story items). Event activation only queues ops, so this rarely emits an output; anything it does emit goes through step 2b's routing too and is returned with the rest |

Every handler call the game hands to the world objects (input events, the player's step, death and push grapple
releases, story-mode changes) also reaches the NPC side (`NpcSystem::apply_object_event`). A kill-zone or
dynamic-kill-zone death notifies the NPC controllers (`death_notifies_npcs`, CONFIRMED (src), NPCS.md §3.2); a
scripted death, the worm's own kill and `KillZ` do not; every respawn teleport restarts the NPC touches.

The handler calls raised in step 4b (the push's grapple release, the scream kill's release) are placed in the
tick's event list after the input events' (4a) and before the step's own (4d), in the order they happened; the
death of step 4e comes last.

The result is `ScriptedTick { report, outputs, npc_events }`: the game's `TickReport` (unchanged, `Copy`), the
Kismet outputs of the update in emission order, and the NPC events of the frame (`Game::npc_events`).

Known deviation (TENTATIVE impact, KISMET_RUNTIME.md §1): the original runs the input events just before the Kismet
update; we run Kismet first. Worm actions and follow-collision changes take effect in step 2b, before the NPCs tick
in 4b of the same frame, as the original's synchronous calls would.

After the tick the app writes `ui::GameTick(report)` (checkpoint snapshot saves, the time-trial death rule) and
`kismet::KismetFrame { outputs, npc_events }`; the audio module observes the game for gameplay sounds
(`FixedPostUpdate`).

## 3. Per frame (app `Update`)

1. `kismet::route_frames` (before the UI flow, `UiFlowSet`): routes every output and NPC event (§4, §5). The
   presentation state (cinematic mode, fades, pause switch, tutorial, view target, ...) starts fresh with every
   new game, and is already cleared while no game runs (a level loading, the menu): the first fixed ticks of the
   next level run before this system sees the new game and must not inherit the previous level's input block.
2. `kismet::sync_save_strings`: a save string the runtime changed is written to the general save
   (`ui::SaveStringEdited`; the original rewrites `GeneralSave.bin` at once, SAVE.md).
3. `kismet::update_presentation`: fade and tutorial timers, the end of Matinee cuts and fades with their action,
   the credits screen (its end calls `Runtime::credits_ended`).
4. `kismet::apply_overlays`: screen fade, title logo, tutorial pop-up, credits screen, crosshair and ability panel
   visibility.
5. `kismet::sync_actor_render`: rendered actors follow Kismet (§6).
6. The UI flow (`ui::flow`): progression records and toasts, level transitions, loads.
7. Audio: a new game stops every sound (`AudioCommand::StopAll`; the adaptive music resets on the same command),
   restarts the gameplay observer and reloads the level's ambient sounds; commands apply once the converted audio
   is in (while it loads they wait, at most 4,096 and only those after the latest `StopAll`; with no load running
   — no converted audio, a failed load — they are dropped, since nothing can play them); the settings' group
   volumes are the `Music`, `SFX` and `Voice` class volumes (master stays Bevy's global volume); with subtitles
   off no line is shown.

`sync_camera` places the camera at the interpolated eye, or at Kismet's view target (§4), plus the camera shake.

## 4. Kismet outputs → app

| Output | Destination |
|---|---|
| `LevelTransition { map, options }` | `ui::OpenMap` → the flow loads the next map with the current snapshot (§7); `ASAMUFrontEndMap` ends the game |
| `ConsoleCommand` | `ToggleCrosshair [bool]` → crosshair; the rest are logged (KISMET.md: `SetSpeed`/`ChangeSize` UNKNOWN in a shipping build, several have no handler) |
| `PlaySound { node, cue, targets, volume, pitch, fade_in }` | `AudioCommand::PlaySound` with `node`, at the first target actor's current location (2-D without one) |
| `StopSound { node, fade_out }` | `AudioCommand::StopSound` of that node's sounds |
| `NarratorLine { id, cue, volume }` / `NarratorStop` | `AudioCommand::NarratorPlay` / `NarratorStop` (the Kismet runtime runs the narrator queue; subtitles come from the cue's waves) |
| `MatineeSound` | `AudioCommand::PlaySound` at the bound actor |
| `SoundMode { start, mode }` | `AudioCommand::SetSoundMode` (stop → back to the base mode) |
| `ActorToggled` / Matinee toggle keys (`ETTA_*`) | `AudioCommand::ToggleAmbient` by object name (non-sound actors ignore it) |
| `AdaptiveTracks` / `AdaptiveMultiplier` | `audio::music::MusicCommand::AddTracks` / `edit` |
| `TutorialShow` / `TutorialHide` | pop-up: the override text, else the per-user string `tutorial.<preset>`, else the preset name (the original's localized text is not shipped); hidden after `displayLength` s (TENTATIVE) |
| `TitleLogo` | title overlay (placeholder text; the logo movie is not ported) |
| `Crosshair`, `Hud` | crosshair / ability panel visibility |
| `CinematicMode { mode, flags }` | while on: `bHideHUD` hides the HUD, `bDisableMovement` / `bDisableTurning` / `bDisableInput` remove movement / look / buttons from the input (TENTATIVE mapping of the stock flags) |
| `CameraTarget`, `MatineeCut { node, group }` | view target: the actor (the Matinee group's first bound actor), at its current Kismet transform; the cut ends when its action stops |
| `CameraFade`, `MatineeFade` | full-screen fade (the stronger of the two); a fade without `bPersistFade` goes away when done, a Matinee fade when its action stops (both TENTATIVE) |
| `CameraShake`, worm growl | procedural shake (placeholder amplitude, ours) |
| `ActorHidden`, `ActorDestroyed`, Matinee visibility keys | hide the actor's rendered entities |
| `OpenMovie` (credits) | credits screen for `kismet::CREDITS_SECONDS` (ours) or until Enter/Space/Esc, then `SeqEvent_CreditsEnded` |
| `Achievement { id }` | `ui::AchievementEarned` → progression (`SaveSession::on_achievement`) and a toast |
| `GameFinished { finished: true }` | `ui::GameFinished` → `bFinishedGame` (time trial unlocks) |
| `TimeTrial { start }` | `ui::TimeTrialStart` / `TimeTrialEnd` (stopwatch, best times) |
| `PauseMenu { enabled }` | Esc does nothing while disabled (TheCore, during the credits) |
| `RestartCheckpointOption` | stored (the pause menu keeps its chapter rule) |
| `LookAtTarget` | stored only (§5) |
| `Worm`, `FollowCollision` | handled in the game frame (§2, step 2b) |
| `CameraAnim`, `MusicTrack` (front end), `MaterialScalar`, `VelocityConeMaterial`, `SuitOnAnimation`, `MenuInvoke` | logged; not presented yet |

## 5. NPC events → Kismet and app

| `NpcEvent` | Kismet (`LevelScript::feed`) | App (`kismet::route_npc_event`) |
|---|---|---|
| `Worm { kind }` | every enabled `SeqEvent_WormEvents`, output `kind.output()` | — |
| `CollectibleCollected { id }` | the collectible's own `SeqEvent_Touch` (the pick-up is the player's touch) and every `SeqEvent_CollectibleCollected` | `ui::CollectibleFound { key }` (`TheWorld.PersistentLevel.<name>`) → progression, counter, extras |
| `ActorInteractedWith { originator }` | `SeqEvent_ActorInteractedWith` (every instance counts, the originator's fire; CONFIRMED (src)); `WorldEvent::ActorInteractedWith` is ignored while NPCs are attached, so nothing is counted twice | — |
| `StoryItemRegistered { item }` | — | `ui::StoryItemFound { key }` (`<map><path>` or `<map>None`, SAVE.md §6.3; the path part's exact format is ours, TENTATIVE) |
| `CameraShake { start }` | — | camera shake |
| `WormState`, `FoliageTouched`, glow flowers | — | logged (sounds and particles not wired) |

`SeqAct_SetLookAtTarget`: local reading of the script shows the look-at actor's per-tick update aims the head and
eye controls at the player pawn's location plus the stored offsets; the stored target itself is never read (STRONG,
src; NPCS.md). We keep the state and render no head control.

The `use` key: outside story mode the controller ignores it; in story mode the stock use search finds only actors
with a `SeqEvent_Used`, and no shipped map has one (CONFIRMED map census, `NpcSystem::use_action`). Story
interactables are used through the fire button, which reaches Kismet as `ActorInteractedWith` above.

## 6. Rendering what Kismet moves

- **Movers and passengers**: `LevelScript::moved_actors()` lists every actor Kismet moved (Matinee, attachment,
  `SetRotationToPlayerRotation`) with its location (sub-level offset included) and rotation. Each rendered entity of
  such an actor (level meshes through their actor slot and plan level; skeletal Matinee actors tagged
  `KismetActor`) gets `D · rest`, where `rest` is the entity's transform before its actor first moved and `D` is the
  actor's change from its placement (`LevelScript::actor_placement`) to its current transform, conjugated into
  render space (`kismet::render_delta`, unit-tested against a turned and lifted actor).
- **Hidden / destroyed** actors' entities are hidden. A new game on the same map (the entities are kept) puts
  moved entities back to rest and skinned actors back to their own visibility. Gap: the render plan
  (`asamu_assets`) does not spawn actors hidden at level start, so Kismet's `UnHide` cannot show them. Census of
  the converted maps (2026-10-10, verify-integration): 17 linked `UnHide` inputs; their targets hidden at start
  are 13 `InterpActor`s that are therefore never rendered (StarHaven 2, Epilogue 3, and the same pair
  `InterpActor_81`/`_82` in ParadiseCave, BeautifulCity, Darkcave and IceCave), one skeletal actor (StarHaven)
  and 5 movable decals (decals are not rendered at all). Fix belongs in the plan: keep hidden actors as hidden
  entities.
- **Streaming**: the render plan always contains every sub-level; the meshes of sub-levels Kismet streams (TheCore
  in AG-IceCave) show only while `Game::is_level_streamed` says so (`--all-sublevels` shows them always). Sub-levels
  carry no BSP in the shipped data (TheCore and Freds_place: 0 BSP triangles, CONFIRMED from the import), so the
  merged BSP needs no gating. Lights of streamed sub-levels are not gated, and lights moved by Matinee
  (`PointLightMovable`; several of BeautifulCity's movers are such lights) stay where placed: the render plan's
  lights carry no actor slot yet.

## 7. Level transitions and the story chain

`Output::LevelTransition` → `ui::OpenMap` → `FlowRequest::Load` (story play only; time trial's Kismet opens the
front end instead). The running simulation is removed, the new map's render plan and game load on the async pool,
the level start of §1 runs (the snapshot carries the checkpoint table and abilities over; the save strings come
from the general save), the audio stops and reloads for the new map, the adaptive music resets, and the Kismet
presentation state starts fresh. Map names resolve case-insensitively (BeautifulCity opens `AG-DarkCave` for the
file `AG-Darkcave`, LEVELS.md).

## 8. Smoke runs (`asamu_game::smoke`)

`cargo run --release -p asamu-game --example smoke -- --converted <dir> [--chain] [--chain-each] [--movers]`, and the
ignored `cargo test -p asamu-game --test smoke -- --ignored` (both read the user's converted data; the test skips
without `ASAMU_CONVERTED_DIR`).

**Per map**: load with Kismet and NPCs, 5,000 ticks of a deterministic pseudo-random input script (SplitMix64:
movement held for 10–90 ticks, sprint, grapple and power-jump bursts, small look deltas, occasional jumps and `use`),
checking every tick that the player, camera, movers, Kismet-moved actors, worm aim and villagers stay finite.
Result on a full conversion (2026-10-10, default seed; re-run by the verification pass on a fresh conversion with
identical numbers, and bit-identical across two runs): every map that loads on its own (10; TheCore and
Freds_place have no PlayerStart) ran 5,000 ticks with 0 non-finite values, 0 interpreter errors and 0 host errors;
movers moved on BeautifulCity (27 of 47), IceCave (26/41), ParadiseCave (134/149), StarHaven (88/225), Workshop
(1/16); deaths and respawns happened on Darkcave (4 deaths, 3 respawns by the end), IceCave (5), ParadiseCave
(6), StarHaven (2) and the front end (1); the NPC side reported foliage touches on ParadiseCave and Darkcave. All
10 maps take about 3 s in the optimized profile. The ignored test asserts the 5,000 ticks, no non-finite value, no
host error and no interpreter error per map.

**Story chain**: for each story map, the touch events whose links (followed through remote events) reach the map's
exit — an `open <map>` console command, the level streaming that brings TheCore in, or the credits movie — are found
in the graph (`smoke::exit_triggers`); the player is teleported into the trigger's volume (the centre of its first
hull's bounds; the touch is registered by the world's touch logic) and the run continues until the transition
appears. Attempts, each on a fresh load: every trigger alone, then all in sequence (farthest first); then both again
after the level's story interactions performed **through the player's story-mode fire** (for each
`SeqEvent_ActorInteractedWith` originator in graph order, or one of its linked child interactables, the player is
placed at the first free spot 60–170 UU away whose eye ray hits the item, aims at it and presses fire once; the
game's story-item model reports the interaction); then once more with the interactions the fire cannot reach sent
to Kismet directly. A trigger whose touch event is disabled is first enabled the way play does it: the triggers
whose links reach that event's `SeqAct_Toggle` "Turn On"/"Toggle" input (`smoke::enabler_triggers`) are touched
and the run waits (up to 200 s) for what they start; only if that fails are the toggles pulsed directly. Each
step records which of these helps it needed (`ChainStep::{direct_touch, enabled_by_touch, enabled_by_toggle,
interactions_fired, interactions_injected}`), and the ignored test asserts that no touch was injected, no toggle
was forced and no interaction was injected. The credits movie is ended at once (the app shows a credits screen
instead). Result (2026-10-10, verification pass):

| Map | Reached | How (ticks from the first exit touch) |
|---|---|---|
| AG-Workshop | AG-ParadiseCave | the exit trigger alone does not lead out (the exit waits on the level's story interactions); after three story interactions fired by the player (one of them through a linked child interactable; a fourth, optional, item is out of the fire's reach from the spots tried and is not needed), the exit trigger (3,184) |
| AG-ParadiseCave | AG-BeautifulCity | two triggers in sequence (one starts the exit narration, the other is the exit; neither alone leads out) (1,984) |
| AG-BeautifulCity | AG-DarkCave | one trigger volume (via a remote event) (664) |
| AG-Darkcave | AG-StarHaven | one trigger (2,183) |
| AG-StarHaven | AG-IceCave | the exit volume's touch event starts disabled; touching the trigger whose cutscene enables it, then the exit volume (7,029) |
| AG-IceCave | AG-Epilogue | the exit trigger streams TheCore in; TheCore's credit trigger starts disabled and is enabled by the end of a 143.6 s Matinee that another TheCore trigger plays; touching that trigger, then the credit trigger, plays the Matinee whose event key opens the credits; the credits' end opens AG-Epilogue after the action's input delay (10,019) |

The first version of this table (integration pass) reached every exit too, but with weaker evidence: Workshop's
interactions were all sent to Kismet directly, and StarHaven's and TheCore's disabled touch events were enabled by
pulsing their toggles instead of playing the cutscenes that enable them.

## 9. Running the app

```sh
asamu-import --out <dir> all          # or: levels, meshes --collision, kismet, matinee, audio, skeletal, textures, materials
cargo run -p asamu -- --converted <dir>                    # main menu: New Game / Continue / chapters
cargo run -p asamu -- --converted <dir> --level AG-BeautifulCity   # straight into a level (saves in memory)
```

Debug: `--camera X,Y,Z,YAW,PITCH` with gameplay teleports the player's eye there at the start; `--screenshot PATH
--screenshot-delay S` takes a local screenshot `S` seconds after the assets settled (keep it local); `RUST_LOG=
asamu=debug` logs every routed output, subtitle line and the number of entities following movers.

Local check (2026-10-10, integration pass, full conversion, nothing committed):

- **AG-Workshop**: the level script's opening dialogue (Kismet `SeqAct_PlaySound` voice cues) plays through the
  audio engine and its subtitle lines reach the HUD one after another over the first ~35 s, then clear (seen in the
  debug log of the subtitle line).
- **AG-BeautifulCity**: the level loads with its Kismet and NPCs (70 skinned actors spawned, 5 collectibles and 4
  story items simulated); at level start the script's sounds play (platform swooshes, wind, the villagers' hums and
  whistles, the village music) and the first checkpoint registers; the skinned villagers render in the market
  (placeholder materials, seen in a local screenshot after a `--camera` teleport); the log reports 19 rendered
  entities following 31 Kismet-moved actors (the rest are moving lights and actors without meshes).
- **Menu flow**: New Game from the main menu loads AG-Workshop with its Kismet and NPCs, unlocks the chapter and
  writes the general save.

Verification pass (2026-10-10, fresh conversion of levels, meshes, Kismet, Matinee, audio, skeletal, materials and
the textures of Startup, AG-Workshop and AG-BeautifulCity under ignored `research/`, deleted afterwards; debug log
only, nothing committed):

- **AG-Workshop**: the prologue voice cue starts about 2 s in through a Kismet `PlaySound`; its subtitle lines
  follow one another for about 33 s, then clear.
- **Level transition in the app**: `--level AG-BeautifulCity --camera` placed in the exit volume (the centre of its
  hull bounds; the volume's actor location lies outside its rotated hull, so a teleport there touches nothing):
  the volume's touch registers, the exit narration plays with subtitles, and about 11 s later Kismet's `open
  AG-DarkCave` reaches the flow, which loads AG-Darkcave (case-insensitive name) with its Kismet and NPCs (1 worm,
  5 collectibles, 3 story items, 15 glow flowers, 406 foliage, 3 skinned actors), activates its level-start
  checkpoint and plays its entry narration. BeautifulCity reports 19 rendered entities following 31 moved actors.
- **Movers on screen** were not checked by eye; instead the gated app test
  `kismet::tests::converted_movers_and_streamed_levels_render_with_kismet` checks on converted data that a level
  mesh entity of a BeautifulCity mover follows it (`D · rest`), and that TheCore's meshes in AG-IceCave stay
  hidden until the level script streams TheCore in.
- Not repeated: the menu's New Game click (the same `FlowRequest::Load` path as the Kismet transition above).

## 10. Open items

- Animation-driven Kismet events (`SeqEvent_AnimNotify`, 24 on BeautifulCity) need the skeletal animation side to
  report notifies (`Runtime::anim_notify`).
- Matinee-moved lights and the lights of streamed sub-levels (§6); camera actors' FOV and camera animations.
- The original's tutorial strings, logo and credits movies (Scaleform) are not ported; placeholders stand in.
- `SeqAct_PlayMusicTrack` (front end), material parameters, the speed-line cone, the suit-on hand animation.
- `RestartCheckpointOption` does not yet change the pause menu's own chapter rule.
- Behavioural parity of the frame order and the first frames of a level needs traces of the original.
- Actors hidden at level start are not in the render plan, so Kismet cannot show them (§6; 13 `InterpActor`s).
- The story chain is followed by teleports into the exit volumes, not by playing the levels (no path finding);
  it proves the scripted exits work, not that every route is traversable.
