# Sandbox (experimental)

The Sandbox is a playground and a lab inside the `asamu` app. It opens into a level of its own, built for this
game's movement (section 9), and behind one key it has the tools: live tuning of the movement and ability
parameters, cheats, time control, in-memory save states, read-outs, visualisers and two hand-made lab arenas, all
on top of the unchanged simulation.

**It is ours. It is not the original's behaviour, and nothing it shows or records is evidence of parity with the
original.** The faithful game ("Classic": the recreation's own parameter set and tick order, its saves and time
trial, and the parity tooling) stays the authoritative target and is not changed by it. How closely Classic itself
matches the original is a separate question, answered in [PARITY.md](PARITY.md), not here.

This document describes it as it is built. It has been exercised by automated tests (which play every route of
the playground with a simple pilot) and by unattended runs that take screenshots. The owner has opened it once, on
a converted map, before the playground existed; nobody has yet played the playground with real keys and a real
mouse. See [Known limitations](#12-known-limitations-and-what-comes-later).

## 1. What it is, and what it is not

| It is | It is not |
|---|---|
| A way to change the recreation's parameters while playing, and to see what changes | A mode of the original game, or a statement about how the original behaves |
| A set of tools around the simulation (freeze, step, save states, teleport, read-outs) | A second implementation of physics, grapple or world logic: it calls the same code Classic runs |
| A playground and test arenas, hand-made by us | Original content, or data derived from it |
| An optional part of the app that can be compiled out | Something Classic depends on |

Every number the Sandbox prints (speeds, jump heights, swing times, the arena rulers that are measured rather
than read from a parameter) is a measurement of **this recreation** in the session. Parity with the original is
measured elsewhere, by replaying recordings of the original ([PARITY.md](PARITY.md),
[TRACE_CAPTURE.md](TRACE_CAPTURE.md)), and the Sandbox takes no part in that.

## 2. Starting it

| Launch | What you get |
|---|---|
| `asamu --sandbox` | **The playground, playing.** One line on screen; F5 opens the tools |
| `asamu --sandbox --converted DIR` | The same playground (with the game's own hand, beam and sounds from your data); the tools' stage list offers the converted maps of that folder |
| `asamu --sandbox --arena movement-lab` (or `grapple-lab`) | Straight into that lab arena (without `--converted`) |
| `asamu --sandbox --no-menu` | Straight into the stock graybox test level (without `--converted`) |
| `asamu --sandbox --level AG-Workshop` | Straight into that converted map |
| `asamu`, then **Sandbox (experimental)** in the main menu | The stage launcher (playground, graybox test level and arenas) |

A session starts with a clean screen: the watermark line, which also names the tools key (and says "click to
play" until the mouse is captured: a start from the command line leaves the mouse free, as every run that skips
the menu does). Everything else is off until it is asked for (section 8).

The stage launcher is where a session that ends returns to, and the inspector's Stage tab has a button for it.

In a source checkout, `asamu` stands for `cargo run -p asamu --`, for example
`cargo run -p asamu -- --sandbox --arena movement-lab`.

The playground gives the full suit by itself (three grapples, the highest budget a shipped map sets, and the
rocket boots), so a session there starts on the Classic set with nothing changed; the profile `infinite-grapple`
is one click away in the tools.

`--sandbox-profile NAME` starts with a tuning profile. `NAME` is a profile's name, not a file path: one of the
built-in presets (section 4) or a profile saved under `<root>/sandbox/profiles/` (section 10). To try the example
in the repository, copy `crates/asamu-sandbox/examples/profiles/floaty.json` into that folder and start with
`--sandbox-profile floaty`. A name that matches no profile does not start a session on an arena (the launcher
opens and says why); with `--level` the map is already loading, so the session runs the Classic set and says so.

Converted maps need your own imported data, exactly as in Classic ([PLAYING.md](PLAYING.md)).

**The main-menu button is enabled only while this run keeps its saves in memory.** That is the case for a plain
`asamu` launch (the graybox). On a launch whose saves are on disk (`asamu --converted DIR` with the main menu) the
button is disabled and says to start with `--sandbox` instead.

A `--sandbox` process never opens the saves on disk and has no Classic menus: it starts in the playground, without
a session it shows the launcher, and the launcher has a Quit button. Classic is a separate launch.

A session ends through the pause menu's "Main menu" entry or the inspector's Stage tab ("end the session"). It
returns to the launcher in a `--sandbox` process, and to the main menu otherwise.

Combinations that are refused at start-up, with a message:

- `--arena` or `--sandbox-profile` without `--sandbox`; an unknown arena; a profile name that is not 1 to 40
  characters of `a-z`, `0-9`, `_` and `-`.
- `--sandbox` with `--placeholder` or `ASAMU_MOVEMENT=placeholder` (a session tunes the Classic set), with `--fly`
  or with `--walk`.
- `--arena` together with `--converted` or `--level` (the lab arenas run in the graybox composition; the playground
  runs in both).
- `--sandbox --screenshot` without `--screenshot-window`: the offscreen screenshot has no UI, so it would lack the
  Sandbox watermark.
- `--sandbox` while `ASAMU_MENU_ACTION` is set (that variable drives the Classic main menu in unattended checks).

A build without the Sandbox (section 11) refuses all three options.

## 3. Guarantees, and what the tests prove

What a session cannot do:

| Classic asset | How it is kept apart |
|---|---|
| Saves, progression, achievements, time-trial records | A session cannot start unless the save session of the run is in memory, and one that finds itself next to saves on disk is ended at once. `--sandbox` never opens the disk store. A session on a converted map loads it through the unchanged menu flow as a story level, so everything that flow writes goes to memory |
| The Classic parameter set | A session runs a **copy** with overrides applied. `PlayerParams::asamu_original()` is not edited and gains no field |
| Tick order | Nothing hooks into the app's tick, its input gathering or its pause handling, and the Sandbox never ticks the running game. The app ticks it exactly as in Classic; a session acts between ticks. (Two things run simulation code on their own copies: the arena builder measures its jump rulers on a throwaway game, and the predicted arc steps a copy of the player) |
| Parity traces | A Sandbox recording is its own file kind in its own directory. The parity trace reader rejects it. F9 makes a Sandbox recording while a session runs, never a parity trace |
| Labels | A session writes its own banner and a permanent on-screen watermark; a modified parameter set is never called original, and on the clean screen the watermark line itself says what a session changed |
| `settings.json` | The Sandbox writes only under its own directory. The Settings screen still stores your own preferences, as in Classic |

A session that changes nothing (no override, default rules, no command, time control never used) is called
*pristine*. The central guarantee is that a pristine session leaves the game bit-identical to one that never met
a session.

### The guard tests

`crates/asamu-sandbox/tests/classic_guard.rs` holds the differential guards. Each compares a run through the
Sandbox with the Classic path run in the same process, so a legitimate change to Classic moves both sides and
breaks nothing, while a leak from the Sandbox shows as a difference. "Identical" means: every tick's report and
the whole player state, the recorded trace as bytes, the level objects, and the text of the entire game value.

| Test | What it shows |
|---|---|
| `empty_overlay_is_the_classic_set`, `set_then_clear_restores_the_classic_set` | No override, or an override that was taken back, is exactly the Classic set, provenance included |
| `pristine_session_game_is_game_graybox` | A game built by a pristine session equals `Game::graybox()` over 3,000 driven ticks |
| `noop_set_params_mid_run_changes_no_bit`, `pristine_parameter_calls_are_inert` | Handing a running game the set it already runs writes nothing |
| `pristine_hooks_are_inert` | The session's per-tick hooks change nothing in a pristine session |
| `snapshot_resume_is_bit_identical_and_isolated` | Saving a state, playing on, restoring and continuing equals the uninterrupted run; the slot is not affected by the live game |
| `converted_fixture_is_unchanged_by_a_pristine_session`, `scripted_level_is_unchanged_by_a_pristine_session` | The same on a synthetic triangle level and on a synthetic level with a level script |
| `real_maps_are_unchanged_by_a_pristine_session` | The same on the user's converted maps, 2,000 scripted ticks each. Skips without `ASAMU_CONVERTED_DIR`, so it does not run in CI |
| `a_tuned_session_leaves_nothing_behind_for_classic`, `the_harness_tells_a_tuned_game_from_classic` | A tuned session does not leak into a later Classic game, and the comparison does detect a tuned game |
| `parity_reader_rejects_sandbox_recordings`, `parity_reader_rejects_the_sandbox_header_line` | The parity trace reader refuses a Sandbox recording |
| `a_pristine_sessions_own_recording_holds_the_classic_samples` | The session's own recording path (its record command, with every hook around every tick) holds, sample for sample, the bytes a Classic game records under the same inputs; only the labels differ |
| `a_tuned_set_is_never_labelled_classic`, `a_tuned_recording_is_never_labelled_original` | Labels and recording notes never call a modified set Classic or original |

`crates/asamu-sandbox/tests/boundary.rs` scans the source tree: no Classic crate or parity tool depends on the
Sandbox crate; the crate adds no dependency feature; the app's `sandbox` feature is not referenced under `crates/`
or `tools/`; `Game::set_params` is called only by the Sandbox; and in the app plugin only `sandbox/control.rs`
takes the simulation or the clocks mutably (as a system parameter, a field of a system-parameter struct, or by
type through the world or commands). These are text scans: they catch the type where it is spelled out, not a
value passed along in a variable.

`crates/asamu-sandbox/tests/hostile_input.rs` treats what a user can edit as untrusted. It feeds the profile and
recording readers mutated, truncated, wrong-typed, deeply nested and oversized input; puts links, odd names and
hundreds of files into the profile directory; and runs a session on every parameter at extreme values that the
parameter validation accepts. Nothing may panic, hang or leave the Sandbox's own directory. It asserts nothing
about what the simulation does with such values.

`crates/asamu-sandbox/tests/playground.rs` plays the playground (section 9) and, among the rest, holds a pristine
session around every tick of it against no session at all: the same inputs give the same game, tick for tick.

The app's own tests (`cargo test -p asamu -- sandbox`) run the plugin's runtime headless with the real tick: a
session cannot start next to saves on disk and the save files are byte-identical afterwards; a `--sandbox`
process starts in the playground in both compositions and leaves nothing of it behind; ending a session
puts the Classic graybox game and the clocks back; an idle plugin never writes the simulation; 400 frames under
an idle plugin, and under a pristine session, equal a bare `Game::graybox()` given the same held keys. One test
(`a_classic_process_is_the_same_with_and_without_the_plugin`) plays the same script of key presses, the
Sandbox's own keys and Classic's developer keys among them, through Classic's real input handling in two
headless apps, one with the plugin registered and idle (everything but the gizmo visualisers) and one with no
Sandbox code at all: every frame, the final game and the trace Classic's own F9 recorder produced are identical.

### What they do not prove

- Nothing here says anything about the original game. The guards show that the Sandbox does not change what the
  recreation's Classic mode does.
- The guards are differential on driven inputs. They cover the paths those inputs reach.
- The interactive app (real key presses, mouse clicks, a human at the controls) is covered by headless tests and
  screenshots only.

## 4. Tuning

The tunable keys are the 67 dotted names of the Classic parameter set's provenance report, in six groups:
`movement.*` (20), `grapple.*` (6), `camera.*` (3), `pawn.*` (19), `gun.*` (12), `boots.*` (7). The tables in
[PARITY.md](PARITY.md#parameters) list each with its Classic value, unit and source.

A change is an *override* on a copy of the Classic set:

- The whole resulting set must pass the same validation as any parameter set. A refused value changes nothing and
  the message says what the parameter requires.
- An override carries `placeholder` provenance with a note starting `sandbox override`, so the on-screen banner
  and every parameter report count it as not original.
- Setting a key back to its Classic value removes the override.
- The stepper sizes are ours: for a number, about a twentieth of the Classic value, rounded to 1, 2 or 5 times a
  power of ten.
  Left Alt multiplies a step by ten, Left Ctrl by a tenth.

### Effect badges

The simulation reads some parameters every tick and copies others into the pawn at specific moments. The badge
says when a change reaches the running pawn:

| Badge | Meaning | Keys |
|---|---|---|
| live | Read every tick; the next tick runs the new value | `movement.world_gravity_z`, `custom_gravity_scaling`, `ground_acceleration`, `ground_friction`, `movement_speed_modifier`, `terminal_velocity`; `camera.max_pitch_degrees` |
| latched | The pawn runs on a copy; the Sandbox brings the copy up to date at once, where it still holds what the old value would have latched | `pawn.move_speed`, `sprint_speed_multiplier`, `story_speed_multiplier`, `landed_air_control`, `zoom_enabled`; `movement.air_control`, `jump_velocity`; `camera.fov_degrees`; `gun.grapple_accel` |
| spawn only | Read when the gun and the boots spawn. It applies to hand-made stages started with it, not to the running game, and on a converted map the level's own ability state overwrites it | `gun.initial_max_grapples`, `gun.initial_can_grapple`, `boots.initial_enabled` |
| inert | Changing it has no effect on the Classic pipeline | `movement.gravity_z`, `braking_deceleration` (placeholder model only); `movement.max_ground_speed`, `air_speed` (overwritten by the script layer); `gun.max_speed` (not read); all of `grapple.*` (debug rope grapple only) |
| (none) | Not classified yet: a change may or may not reach the running pawn at once | the other 37 keys |

The live and inert rows are each checked by a test (`tests/keys.rs`): a live key changes the next tick, and an
inert key leaves a driven run bit-identical. A key missing from the table is unclassified, so a new Classic
parameter never breaks the Sandbox.

### Keys worth trying first

The quick tuner (section 7) starts with seven keys pinned: `movement.custom_gravity_scaling`,
`movement.jump_velocity`, `pawn.landed_air_control`, `pawn.move_speed`, `gun.grapple_accel`, `gun.max_distance`
and `boots.boost_strength`. What these and a few others do on the arenas:

| Key | What a change does |
|---|---|
| `movement.jump_velocity`, `movement.custom_gravity_scaling` | Height and air time of a jump |
| `pawn.move_speed`, `pawn.sprint_speed_multiplier` | Walking and sprinting speed. A change reaches a pawn that is already walking |
| `pawn.landed_air_control`, `movement.air_control` | How far a jump can be steered. The pawn copies `movement.air_control` when it spawns and `pawn.landed_air_control` at every landing, so from its first landing on only the second key reaches it |
| `movement.step_height` | Which stair risers are climbed |
| `gun.max_distance` | Which hooks the grapple reaches |
| `gun.grapple_accel` | The pawn's air speed while the grapple pulls (uu/s; not an acceleration, whatever the original's name says): a higher value gives a faster, shorter pull |
| `boots.boost_strength`, `pawn.power_jump_strength` | The rocket boost and the power jump |
| `camera.fov_degrees` | The field of view, at once |

Each row is held by a test in `crates/asamu-sandbox/tests/tuning_effects.rs`: the same scripted action is
measured under the Classic set and under an override, and the direction of the difference is asserted.
`cargo test -p asamu-sandbox --test tuning_effects -- --nocapture` prints the values (the lines starting with
`MEASURE`). They are measurements of **this recreation** on the day of the run, not of the original, and no number
of them is written down in the test.

The Classic validation is the only limit on a value, and it is wide. The same test file sets extreme values for
every key in the middle of a run: none of them stops the program, and the player's state stays finite. With the
largest representable number in a speed key the simulation refuses its own steps and the pawn stands still;
"reset all" (Left Alt + Backspace) and a respawn (R) give a pawn that plays under the Classic set again.

### What cannot be tuned

The constants of the original's script code and native physics (the grapple's pull numerator, the jump-release
multiplier and so on) are named constants in the simulation crates, not parameters. They are not keys and cannot
be changed in a session.

### Profiles

A profile is a named set of overrides, rules and an optional time scale on top of the Classic set (format in
section 10). The built-in presets are ours and are not modes of the original. Each override in them is a factor
of the Classic value read when the preset is built; no Classic number is written into them.

| Preset | What it changes |
|---|---|
| `classic` | Nothing |
| `moon` | Gravity scale at 0.35 of Classic |
| `heavy` | Gravity scale at 1.8, jump at 0.85 |
| `super-jump` | Jump at 1.6, power jump at 1.5 |
| `ice` | Ground friction at 0.1, acceleration at 0.4 |
| `sprinter` | Walking speed at 1.5, sprint multiplier at 1.25 |
| `long-reach` | Grapple reach at twice Classic |
| `infinite-grapple` | Classic values; a rule makes the grapple budget unlimited and self-refilling |
| `bullet-time` | Classic values at a quarter of normal speed |

The Profiles tab loads a profile into the running session and saves the current tuning as `custom-1`, `custom-2`
and so on (there is no text input yet), or over the profile in use when it is one of your own.

## 5. Rules and actions

**Rules** are sticky cheats that are not parameters. They are enforced before every tick, over whatever the level
sets, through the same calls the level's own script actions use:

| Rule | Values |
|---|---|
| Grapples | the level's, a fixed number per landing, or unlimited |
| Rocket boots | the level's, forced on, or forced off |
| Auto-refill | refill the grapple budget whenever it is used up, without a landing |

The default rules change nothing. Setting a rule back to "the level's" does not restore the level's earlier value.

**Actions** are one-off, and the level may change the state again: story mode, cycle the grapple capacity
(0, 1, 2, 3, unlimited), rocket boots, activate the attractor pads, refill the grapples, re-arm the boots, respawn,
quick load (the death sequence, then the last checkpoint), teleport, fly placement, bookmarks. An action on an
ability that a rule pins is refused with "change the rule instead".

**Teleport** targets: the crosshair hit, the level start, each checkpoint, the next target of that list, a
bookmark, or an explicit position. A teleport places the pawn with zero velocity and lets it fall from there. It
is refused while the grapple is attached and during the death sequence; a teleport to the crosshair is also
refused when the crosshair is not on a surface or there is no room for the pawn against it. No teleport places
the pawn more than 1,000,000 uu from the origin (a limit of ours, far beyond any level).

**Fly placement** freezes time and moves the frozen player with W A S D, Space (up) and Left Ctrl (down); Left
Shift moves faster. It is placement, not flight physics. Leaving it logs one teleport to where you ended up.

Every executed command is logged with its tick. A refused command changes nothing and is not logged.

## 6. Time control, save states, rewind

**Time control** changes how often a tick runs, never what a tick computes: the simulation step stays the fixed
step.

| Function | Effect |
|---|---|
| Freeze | No tick runs. The game stays "playing", so the pause menu does not open. You can still look around |
| Single step | While frozen, exactly one tick per requested step |
| Speed | 0.1x, 0.25x, 0.5x, 1x, 2x, 4x, 8x |

Speed is applied to the app's virtual clock, so everything that follows that clock (level-script presentation
timers, effects, particles, audio timing) is scaled with it. The clocks are back to normal speed whenever a
session ends.

The inspector holds time while it is open by default ("freeze while open"). That hold is not part of the
session's own time state, so opening the inspector does not make a session non-pristine and closing it puts time
back as it was.

**Save states** are four in-memory slots holding a clone of the whole simulation (game, level objects, clock,
random state, level script). They work on every level and are gone when the session ends. Nothing is written to
disk, and they have nothing to do with the game's save system.

- A loaded state runs the session's *current* tuning and rules, so "save, change one number, load, retry" works.
- A slot of another level is refused.
- A load while recording first finishes and writes the recording.
- **On levels with a level script a save state restores the simulation only.** Sounds, effects and the
  script-driven interface are not part of it and are not rewound. The slot list says so.

**Rewind** steps back through keyframes taken every 30 ticks (half a second), 120 of them (one minute). It is
available on the hand-made levels (the playground, the graybox test level and the lab arenas), not on converted
maps, and is not an exact per-tick rewind. No keyframes are taken while a recording runs. On the playground a
keyframe holds the pawn and everything that turns and moves (the map data itself is shared between the
keyframes, so a keyframe copies only the state of the run).

## 7. Hotkeys

The Sandbox keys work in play: a session is running, no screen is open and the mouse is captured (click into the
window). The inspector key also works with the mouse free.

| Key | Action |
|---|---|
| F5 or `` ` `` | **The tools**: open or close the inspector (Esc also closes it). Opening it also switches the read-outs and drawings on |
| `[` / `]` | Previous / next parameter of the quick tuner |
| `-` / `=` | Step the selected parameter down / up (Left Alt x10, Left Ctrl x0.1) |
| Backspace | Reset the selected parameter (with Left Alt: reset all) |
| P | Freeze on/off |
| O | Single step (with Left Alt: ten steps) |
| `,` / `.` / `/` | Slower / faster / back to 1x |
| 1 to 4 | Select a save-state slot |
| K / L | Save to / load from the selected slot |
| Z | Rewind one keyframe (the playground, the graybox and the lab arenas) |
| T / G | Teleport to the crosshair hit / to the next teleport target |
| B / V | Set the "quick" bookmark / return to it |
| N | Fly placement on/off |
| H | Read-outs and drawings on/off. Off is the clean screen a session starts with: the watermark line alone |
| F9 | Start/stop a Sandbox recording |
| F2, F3, F4, F6 | Story mode, grapple capacity, rocket boots, attractor pads: the same effects as Classic's developer keys, run as logged commands |
| F7, R | Quick load on a converted level, respawn on a hand-made one, as a logged command |

While a session runs, F2, F3, F4, F6, F7, R and F9 are taken from Classic's developer-key handler and act through
the session instead; so is the Esc that closes the inspector. No other key is taken from the game: W A S D, the
arrow keys, Space, Left Shift, Left Ctrl, E, Q, Enter, Tab, F1, F8, F10 and F12 keep their Classic meaning. The
Sandbox additionally reads Left Alt and Left Ctrl as step modifiers, and the movement keys during fly placement
(time is frozen then).

The developer read-out (F1) keeps its Classic key list during a session, including "F9 record trace". In a
session F9 makes a Sandbox recording; the Sandbox HUD's own key line says so.

## 8. The inspector, the HUD and the visualisers

**The inspector** is a panel across the top of the window with six tabs. While it is open the mouse is free,
mouse look and the grapple are off, and time is held unless you switch "freeze while open" off.

| Tab | Contents |
|---|---|
| Tune | One page per parameter group: steppers, the Classic value, unit, effect badge, reset, pin to the quick tuner; the selected row's description and Classic source |
| Rules & actions | The rules, the one-off actions, the teleport targets and bookmarks |
| Time & states | Freeze, step, speed, the four slots with their ticks, rewind, recording |
| View | Visualiser switches, HUD read-outs, the quick tuner's pinned keys |
| Profiles | Built-in presets and saved profiles; save the current tuning |
| Stage | The stages of this launch (the playground, and the lab arenas or the converted maps); starting one ends the session and starts a new one. Buttons for the stage launcher and for ending the session |

**The clean screen.** A session starts with one line at the top of the window: the watermark
`SANDBOX - experimental - not the original's behaviour - saves off`, followed by what the session has changed, if
anything (`profile moon: 1 override`, `rules on`, `time 0.5x`, `REC`), by `click to play` while the mouse is free,
and by `F5 tools`. On the playground a timed route adds its clock under it while you are on one (section 9).
Nothing else of the Sandbox is on screen, and in the playground the developer read-out (F1) and the level gizmos
(F10) start switched off too.

**The tools** come with F5 (or the back-quote): the inspector opens, and the read-outs and drawings are on from
then on, also after the inspector is closed, so you can tune and watch while you play. H switches them off again
(and on).

**The HUD read-outs** are non-interactive text in the left half of the window, under the watermark: the profile,
override count and time state, the quick tuner, speed with its peak and height, the last jump, the last swing, the
slots and rewind length, the latest outcome or refusal, and two lines of key help. Two small bar graphs at the
bottom right show speed and height over the last 600 ticks.

**Visualisers** are lines drawn over the game. They change nothing, and none is drawn while the read-outs are off
(the "Default" column is what the switches are set to when the tools first come on).

| Visualiser | Default | Shows |
|---|---|---|
| Velocity | on | The velocity and its horizontal part, as an arrow ahead of you |
| Collision cylinder | off | The pawn's collision cylinder and the floor normal |
| Aim and grapple range | on | The aim ray (green where the hit can be grappled), a ring at the gun's reach, a release ring while attached |
| Trail | on | Where the pawn was during the latest ticks |
| Earlier attempts | on | The trails of the last three attempts (a respawn, teleport or loaded state ends an attempt) |
| Predicted arc | off | Where the held movement keys would take you with the grapple let go |
| Arena markers | on | Posts and labels of a hand-made arena |
| Graphs | on | The two bar graphs |

How the read-outs are measured, all in whole ticks of this recreation:

- A **jump** starts on the tick the pawn leaves the ground (a jump, or walking off a ledge) and ends on the tick it
  lands. Apex and distance are relative to where it stood before take-off. A jump that turns into a swing, or is
  cut by a respawn or teleport, is not reported.
- A **swing** starts on the tick the grapple attaches and ends on the tick it releases: attach distance, duration,
  peak speed, release speed.
- The **predicted arc** steps a copy of the player with the game's own step function. The world is taken as it is
  now: movers, crystals, attractor pads and the level script do not advance in the prediction.

## 9. Stages: the playground and the lab arenas

### The playground

The level the Sandbox opens into: one level of ours, built for this game's movement and large enough to stay in.
**It contains no original content and no data derived from it**: every shape is generated by
`crates/asamu-sandbox/src/playground.rs` from a few numbers, every position, size and count is ours, and it needs
no game data to load or to be drawn. The only values it takes from the game are Classic *parameter* values read
when it is built (the grapple's reach, for the shots that are at the edge of it). It is not a level of the
original, and nothing measured on it says anything about the original.

From the start, facing ahead:

| Where | What | For |
|---|---|---|
| Around you | **The hub**: a wide paved island with lamps, a ring in the floor and an arrow towards each landmark in that landmark's colour | The start, the only checkpoint, where every fall ends |
| Ahead | **The near chain**: five islands, each higher than the last. Three gaps a walking jump crosses, one wide gap for the power leap, one tall step for the power jump; a hook hangs over every landing | The warm-up, with or without the grapple |
| To the right | **The medium chain**: five islands 3,700 to 4,250 uu apart as the grapple flies, going down and up by as much as 1,000 uu | Grapple runs: every gap is far beyond a jump |
| To the left, past the runway | **The far chain**: two shots at 93 % of the Classic grapple range, then a stretch of five hooks without a landing, where two recharge crystals hang beside the line | Long pulls, and the budget: five hooks, three grapples |
| Left | **The runway**: a straight of 9,000 uu between two launch ramps; a sprint up either ramp lands on the island beyond it | Speed, the sprint, the ramps |
| Behind | **The ferry** to **the yard** (walk onto it from the hub's pier while it waits; it crosses in 9 s and waits 3 s at each end), **the carousel** (six rocks on a hub that turns once in 40 s, at two heights) and **the wheel** (six gondolas that stay level, once round in 48 s), with **a pier** from the yard along the wheel: at its far end a gondola passes the pier's edge every 8 s, a short step below it. A hook rides over every rock and every gondola, all the way round | Things that move: ride them, grapple them, be carried |
| Ahead and to the right | **The tower**: seven ledges a jump apart, the top a power jump above the last (or one pull from the bridge to the hook beside the top), a board, and under it **the catch** | Height, and a drop of more than 6,000 uu that ends at the hub |

Reading it at a glance (the colours are the view's; the roles are the level's):

| Looks like | Is |
|---|---|
| Pale paving | Built ground: the hub, piers, bridges, the runway, the tower's top |
| Green-topped brown rock | An island to land on |
| Grey rock | Spires, the tower's column and ledges |
| Teal rocks hanging in the air | Hooks: something to pull to. One hangs 1,000 uu over every landing of a chain |
| Magenta | Whatever moves: the ferry, the carousel's rocks, the gondolas (on dark machinery) |
| Cyan, glowing | A recharge crystal (grey while it is used up) |
| Orange | A launch ramp, the tower's board |
| A red sheet far below | The catch |
| A white arch | The start of a timed route |
| A gold frame with a light on top | Its finish |
| A tall spire with a coloured light | A landmark: gold ahead, cyan right, violet left, pink behind, green on the tower |

The rules behind the picture are the game's own, unchanged:

- **Every solid surface takes the grapple** (nothing in the playground is tagged otherwise). The hooks are not
  special; they hang where a route needs something to pull to.
- **The suit**: the playground's own level script gives three grapples and the rocket boots at level start, the
  way a level's start events do. A landing gives the grapples back.
- **A pull ends just short of what it holds, and a pull into a wall ends at the wall.** A pull to the top of an
  island you look up at ends against its side. That is why a hook hangs over every landing: hold the button on
  the hook until the gun lets go under it, and drop onto the landing. In the air, let go early and take the next
  thing: from under a hook, the next one may be hidden behind it.
- **A recharge crystal gives the grapples back and does not pull**: the gun lets go of it at once. On the far
  chain the crystals hang beside the line, to be taken in passing.
- **Only the hub is a checkpoint.** The catch under the tower and the depths below the level kill, and you wake
  at the hub. The other places worth going to are teleport targets of the tools (G steps through them; the Rules
  tab lists them by these names): `near`, `medium`, `far`, `crystals`, `runway`, `ferry`, `yard`, `carousel`,
  `wheel` (the end of the wheel's pier, where a gondola is boarded), `tower`.
- **What moves, moves by the game layer's rules**: the hubs are placed in the rotating physics with a rate, the
  rocks, their hooks and the gondolas are attached to them (the gondolas with the flag that keeps their own rotation),
  each gondola's hook is attached to its gondola (attached to the hub it would go round the gondola once a turn),
  the ferry is moved by a Matinee of ours, and a grapple's anchor rides all of them (section 11 says how the
  level reaches the simulation).
- **What moves carries whoever stands on it and pushes nobody** (the game layer does not model pushing or
  crushing by a mover). So nothing that moves passes through a place to stand (a test holds the level to that),
  and boarding is always level or a step down: the ferry's deck is level with both piers while it waits, and a
  gondola passes the wheel's pier 14 uu below its edge. A floor that rises past a pawn is not stepped onto: with
  a gondola level with the pier at its lowest point, the pawn was dragged along the pier's edge instead (measured
  while the level was checked).
- **A fall off the level lasts about four seconds** before the pawn wakes at the hub (3.7 to 4.3 s measured, from
  the hub, the last landing of each chain, the yard and the tower's island).

**Timed routes.** Four routes have a clock: the near chain, the medium chain and the far chain, each from its arch
at the hub (or the runway's edge) to the frame on its last island, and the runway sprint between its two gates. The
clock of a route starts on the last tick you are under its arch (standing there does not run it) and stops on the
first tick you are inside its frame; its line appears under the watermark while it runs and after a finish, with
the best time of the session. A death, a respawn, a teleport or a loaded save state ends the attempt without a
time. Times are simulated time, so freezing or slowing a session does not change them; a session that changed the
rules or the parameters times what it changed. They are measurements of this recreation in this session, never of
the original, and they are not kept: there is no record on disk.

**What the tests show** (`crates/asamu-sandbox/tests/playground.rs`; the pilot only aims, holds, lets go and
steers at the landing, so a route it finishes is one a player can):

| Test | What it plays |
|---|---|
| `it_loads_through_the_games_loader_without_game_data` | The generated data loads through the game's loader without a warning; two hubs turn; nothing stays on disk |
| `the_pawn_starts_on_the_hub_with_the_full_suit` | Standing at the start with three grapples and the boots, from the level's own script |
| `the_layout_fits_what_the_classic_set_does` | The jumps the layout was fitted to, measured and printed; a ledge is less than a jump apart, the wide gap more than a walking jump |
| `the_near_chain_can_be_done_on_foot_and_is_timed` | Three walking jumps, the leap, the power jump; the route's clock times it, and a teleport ends an attempt |
| `the_chains_can_be_done_by_grapple` | Every landing of the near and the medium chain by a pull to its hook and the drop; a landing gives the budget back |
| `the_far_chain_needs_its_crystals_and_can_be_done` | Both long shots (over 90 % of the reach), the stretch in the air with the budget at 1, 2, 0, 1, 2, 0, 1, the landing at the end |
| `the_tower_is_climbed_and_the_long_drop_ends_at_the_hub` | Up by seven jumps and a power jump; up by the hook from the bridge; off the board, more than 5,000 uu down into the catch, awake at the hub |
| `the_runway_is_timed_and_its_ramps_reach_their_landings` | The sprint between the gates is timed (about six seconds); a sprint up each ramp ends on its landing |
| `the_wheels_turn_and_the_ferry_keeps_its_timetable` | The hubs turn by their rate times the time; rocks keep their height and arm; gondolas keep level; the ferry is across after a crossing and home after the timetable |
| `a_pawn_rides_the_ferry_the_carousel_and_the_wheel` | A pawn stands on each of them for 24 s |
| `a_grapple_takes_a_moving_hook_and_the_pawn_ends_on_what_carries_it` | The anchor rides the hook; the pull ends over the rock or the gondola; the pawn rides on |
| `what_moves_keeps_clear_of_the_places_to_stand` | For a minute of the level's time, nothing that moves enters the room a pawn takes on any paving, island or ramp |
| `the_landing_behind_the_runway_is_left_alone_by_the_carousel` | A pawn stands under the carousel's circle on that landing for a whole turn, undisturbed |
| `every_station_is_a_place_to_stand` | Every station: the pawn is put there, stays and looks the station's way |
| `the_tools_list_the_stations_by_name` | The teleport list names the stations, in order, and each entry puts the pawn at its station |
| `the_grapple_routes_are_timed_from_the_arch_to_the_frame` | The medium and the far route by grapple from under the arch into the frame, with a time |
| `a_fall_from_anywhere_soon_ends_at_the_hub` | Walking off the hub, the last landing of each chain, the yard and the tower's island: awake at the hub within six seconds, never at rest on the way |
| `a_hook_stays_over_its_gondola_all_the_way_round` | Every gondola's hook is right over it for a whole turn of the wheel |
| `the_wheel_is_taken_from_the_yard_and_ridden_all_the_way_round` | From the yard: a pull to a gondola's hook, the drop onto the gondola, a whole turn on it |
| `the_wheel_is_boarded_on_foot_from_its_pier` | From the pier's edge: a few steps onto the passing gondola, a whole turn, and off onto the pier again |
| `the_ferry_is_walked_onto_and_off_at_both_ends` | From the hub's pier onto the ferry when it comes in, across, and off to the yard, walking |
| `the_keyframe_rewind_works_on_the_playground` | A rewind goes back to a keyframe exactly (the pawn and everything that moves), and the run goes on from there as if it had never been further |
| `a_session_on_the_playground_keeps_its_promises` | The level's name everywhere, save states with what moves, and a pristine session against none, tick for tick |

What the Classic set does there, measured in this recreation (not the original's numbers, and they move if the
recreation's physics is corrected): a jump rises 481 uu and covers 851 uu walking and 1,096 uu sprinting; a power
jump rises 1,231 uu; a power leap covers 2,563 uu. The layout was fitted to these, and
`cargo test -p asamu-sandbox --test playground the_layout_fits -- --nocapture` prints them (the lines starting with
`MEASURE`); the test asserts only the relations the level relies on (a ledge is less than a jump, the tower's top
more, and so on). A pull arrives under its hook at about 1,430 uu/s (one measurement, made while the level was
laid out).

**How it looks** is ours as well (`apps/asamu/src/sandbox/playground_view.rs`): flat-shaded meshes of the level's
own solids, an evening sky, a low sun with shadows, a light haze, lamps on the hub. Nothing of it imitates the
original, and nothing of it needs game data. In a run on converted data the game's own hand and grapple beam are
drawn as in any level, since they come from your data (so, presumably, are its hit mark and sounds: the unattended
runs made for this document were silent, and none of their frames shows a hit); without it the app's fallbacks
are used (no hand, a plain beam, and a flat blue patch where the grapple hits: the app's stand-in for the hit
mark, PLACEHOLDER for the original's glowing decal, not something the Sandbox draws).

### The lab arenas

Two built-in arenas, both hand-made box levels written by us. They contain no original content and no data
derived from it. Each has the level name `sandbox arena: <id> (hand-made, not original content)`.

| Id | Stations |
|---|---|
| `movement-lab` | A runway with a post for every second of Classic walking (and sprinting); lanes of platforms with rising heights across four gap widths; six stair lanes whose risers go from half to one and a half times the Classic step height |
| `grapple-lab` | A fan of hooks at fractions of the Classic grapple range; hooks around the Classic release distance; a grapple-able beam; top-only, bottom-only and not-landable surfaces; a wall that cannot be grappled; a recharge crystal; a moving block; an attractor pad |

The layouts, sizes, counts and fractions are ours. What ties an arena to the game is its *rulers*, which are fixed
when the arena is built and stay there while you tune, so the arena is a constant yardstick:

- **Parameter rulers** are Classic parameter values read from the Classic set at build time: walking and sprint
  speed, step height, the grapple's reach and release distance. No number of the original is written in the
  arena code.
- **Jump rulers** (the apex and the ground a walking jump covers) are **measured in this recreation**: a pawn with
  the Classic set makes a full jump on a flat floor through the game's own tick. They describe what the recreation
  does today, they are not measurements of the original, and they move if the recreation's physics is corrected.
  The markers carry that note.

Box levels cannot express slopes.

**Finding your way.** In `movement-lab` everything is in view from the start: the runway ahead, the stairs to the
left, the jump lanes to the right. In `grapple-lab` the first look shows little: you face the fan of hooks, which
hang ahead and well above eye level, so with the developer read-out shown (F1 toggles it) the hooks sit behind its
text and their labels are not drawn there. Every other station is beside or behind the start: the close-range pad
to the left, the tagged surfaces to the right, the beam straight behind, the crystal and the moving block behind
to the left, the attractor pad behind to the right. Turn around, or press G to step through the teleport targets
(the start, then the checkpoints at the start, on the close-range pad and facing the tagged surfaces).

The arenas run in a launch without converted data (`asamu --sandbox --arena ID`, or the stage list there). The
playground runs in every launch.

## 10. File formats and where files live

Everything the Sandbox writes is under `<root>/sandbox/`, where `<root>` is the user data folder that also holds
`saves/` and `settings.json` (macOS `~/Library/Application Support/asamu-decomp`, Windows
`%LOCALAPPDATA%\asamu-decomp`, Linux `$XDG_DATA_HOME/asamu-decomp` or `~/.local/share/asamu-decomp`;
`ASAMU_SAVE_DIR` overrides it).

```text
<root>/sandbox/profiles/<name>.json            tuning profiles you saved
<root>/sandbox/recordings/<name>.sbxrec.jsonl  Sandbox recordings
```

Nothing is created until something is saved. Nothing is written to `<root>/saves/`, to `$ASAMU_TRACE_DIR`, into
the converted data or into the repository. Without a user data folder nothing can be saved.

One thing is written outside that folder, and removed again at once: each start on the playground writes the
playground's generated data (a few hundred small files, under 1 MB, all produced by this program) into a new directory
`asamu-sandbox-playground-<pid>-<n>` under the system's temporary directory, loads it with the game's loader and
deletes the directory before the session begins (section 11). The directory is created by that start and refused
if the name already exists, so nothing that was there before is written through.

### Profile, version 1

```json
{
  "format": "asamu-sandbox-profile",
  "version": 1,
  "name": "floaty",
  "description": "example values, ours, not the original's",
  "base": "asamu_original",
  "overrides": {
    "movement.custom_gravity_scaling": 0.5,
    "pawn.zoom_enabled": false
  },
  "rules": { "grapples": "unlimited", "rocket_boots": "on", "auto_refill": false },
  "time_scale": null,
  "extensions": {}
}
```

(`crates/asamu-sandbox/examples/profiles/floaty.json`; a test parses it.)

| Field | Meaning |
|---|---|
| `format`, `version` | `asamu-sandbox-profile`, `1`. Another format or version is refused |
| `name` | 1 to 40 characters of `a-z`, `0-9`, `_`, `-`; also the file name |
| `description` | Free text (optional) |
| `base` | Required. `asamu_original` is the only base in this version |
| `overrides` | Parameter key to value. Each must exist, have the right type and leave the whole set valid |
| `rules` | `grapples`: `"level"`, `"unlimited"` or `{"fixed": N}`; `rocket_boots`: `"level"`, `"on"` or `"off"`; `auto_refill`: boolean |
| `time_scale` | `null` (1x) or a factor from 0.1 to 8 |
| `extensions` | Reserved for later tools; kept as written |

A profile never stores a whole parameter set, only overrides on the named base. Unknown fields are refused, and
files over 256 KiB are refused. A profile is a JSON object with named fields, and so are its `rules`: the same
values written as a JSON array in field order are refused.

### Recording, version 1

A Sandbox recording is a JSON Lines container: one Sandbox header line, then an ordinary trace (its meta line and
one sample per tick, as in [PARITY.md](PARITY.md#trace-format)). The header, shown wrapped here (it is one line
in the file):

```json
{"format":"asamu-sandbox-recording","version":1,"not_parity":true,
 "level":"sandbox arena: movement-lab (hand-made, not original content)",
 "param_set":"modified","profile":"moon",
 "overrides":{"movement.custom_gravity_scaling":0.35},
 "rules":{"grapples":"level","rocket_boots":"level","auto_refill":false},
 "time_control_used":false,
 "actions":[{"tick":120,"cmd":{"cmd":"respawn"}}],
 "actions_truncated":false}
```

- `not_parity` is always `true`; a file that says otherwise is refused by the Sandbox's own reader and writer.
- `param_set` is `classic` only when the recording ran on the Classic set from start to end.
- `actions` is the session's command log with the tick each command ran before.
- The embedded trace is tagged too: its level is prefixed with `sandbox:`, a note says
  `sandbox: not a parity run`, and its `parameters:` note is rewritten when the set was modified. A recording of a
  pristine session is tagged as well.
- The parity trace reader parses the first line as trace metadata, which rejects unknown fields, so it refuses
  the file. No parity tool was edited for that.
- That header line is the whole protection. With it removed by hand, what is left is an ordinary runtime trace,
  and today's parity tool reads it (`asamu-trace validate` and `compare` accept it); only the `sandbox:` level
  name and the note mark it. Making the tool refuse that note is listed in section 12.
- Files are named `asamu-sandbox-<level>-tick<N>.sbxrec.jsonl` and never replace an earlier recording.

The embedded trace carries the input of every tick, so a recording could later be replayed. No replay tool exists
yet.

## 11. Architecture, and how to compile it out

```text
asamu-core <- asamu-player <- asamu-world <- asamu-kismet <- asamu-game <- asamu-sandbox (render-free)
                                                                 ^               ^ optional, feature "sandbox"
                                                          tools/asamu-trace   apps/asamu (src/sandbox.rs, src/sandbox/)
                                                          (never sees asamu-sandbox)
```

- **`crates/asamu-sandbox`** is the model, without Bevy: the key catalogue and overlay, re-latching, rules,
  profiles, the command enum and the session, time control, save states, telemetry, prediction, the recording
  container, the playground with its timed routes, the arenas and read-only inspection views. It depends on the simulation crates; none of them depends
  on it. It reaches a running game through public APIs only.
- **`crates/asamu-game/src/tooling.rs`** is the one addition to a simulation crate: `Game::set_params`, which
  replaces the parameters between ticks after validating them. Classic code never calls it, and calling it with
  the set already in use writes nothing.
- **`apps/asamu/src/sandbox.rs` and `src/sandbox/`** are the Bevy plugin: `cli`, `lifecycle`, `control`,
  `storage` (the runtime) and `input`, `panel`, `widgets`, `launcher`, `hud`, `viz`, `graphs`, `arena_view`,
  `playground_view` (the view).
- **Single writer.** Only `sandbox/control.rs` takes the simulation or the virtual and fixed clocks mutably. The
  view reads through `Inspection` and asks for changes with a request message; `lifecycle` decides what a
  lifecycle request means and hands the writing to `control`.
- **One mutation entry point.** Every hotkey and button becomes a serialisable `Command` run by
  `Session::execute`.
- **Idle in Classic.** The plugin is registered in every process, but outside a session only its start-up system
  and two read-only lifecycle systems run (besides the run conditions of the others, and a click observer that
  returns at once for any button that is not the Sandbox's). It never writes the simulation or a clock there.

**How the playground reaches the simulation.** The game layer's support for actors that turn, for what is
attached to them and for Matinee-moved platforms lives in the level script, which only the loader of converted
levels builds. The playground therefore does not fork or patch any of it: `playground.rs` writes the level **in
the converted-data layout** (a scene file, a mesh manifest with one convex simple body per solid, a level script
of ours that gives the suit and starts the ferry's Matinee, and that Matinee's data), using the workspace's own
writer for synthetic converted data (`asamu_world::fixtures`); the files go into a fresh temporary directory, the
game's public loader (`load_level_with_kismet_options`, the one Classic uses) loads them, and the directory is
removed. From there on it is a level like any other to the simulation: the carousel turns because its hub is
placed in the rotating physics with a rate, and so on. The app runs it with the unchanged tick (`fixed_tick`
ticks a level script when the simulation has one) and draws it itself.

Two things follow from that route, and the Sandbox handles both in its own code:

- The loader names every level it builds "... (converted locally from the original)" and gives it that origin.
  The playground is not converted from anything, so the Sandbox prints it as `sandbox playground (hand-made, not
  original content)` wherever it prints a level's name (the banner, the inspector, a recording's header and
  embedded trace). The game's own level record still carries the loader's label. **What is missing in the game
  layer is a constructor for a hand-made scene level**, with its own origin; adding one is a change to a Classic
  crate and was not made here.
- In a run on converted data the app has no simulation outside a level and shows a converted level behind its
  menu. A session on the playground there makes the playground's game the simulation, and its view removes that
  level's entities and gives the level a name no converted level has, so that none of its lights, light maps or
  atmosphere applies. The menu flow plans a level again when a map is started from the stage list.

Hooks in existing files, all additive (the playground added none):

| File | Hook |
|---|---|
| `Cargo.toml`, `Cargo.lock` | The new workspace member |
| `crates/asamu-game/src/lib.rs` | `mod tooling;` and the `SetParamsError` re-export |
| `apps/asamu/Cargo.toml` | The optional dependency and the `sandbox` feature (default on) |
| `apps/asamu/src/main.rs` | `mod sandbox`, the three options and their check, the usage text, the plugin registration, `saves: menu && !cli.sandbox.enabled`, and a `LevelVisual` marker on the graybox meshes |
| `apps/asamu/src/ui.rs` | `Screen::Sandbox` and two re-exports |
| `apps/asamu/src/ui/menus.rs` | The main-menu button and three match arms |
| `.github/workflows/ci.yml` | A clippy run without the feature, and the app's Sandbox tests on Linux |

**Compiling it out.** The feature exists on the app only:

```sh
cargo build -p asamu --no-default-features
```

That build has no Sandbox code and refuses `--sandbox`, `--arena` and `--sandbox-profile` with a message.
Nothing under `crates/` or `tools/` is compiled differently by the feature.

**Removing it.** Delete `crates/asamu-sandbox`, `apps/asamu/src/sandbox.rs`, `apps/asamu/src/sandbox/` and
`crates/asamu-game/src/tooling.rs`, then take out the hooks listed above.

**Unattended looks.** For layout checks and screenshots, the environment variable `ASAMU_LAB_VIEW` plays a
comma-separated list of steps in a running session: the tab names (`tune`, `rules`, `time`, `view`, `profiles`,
`stage`; `tune:<group index>` picks a parameter group), `play`, `show` (the read-outs and every visualiser on),
`walk` (forward held), `respawn`, and for the playground `at:<station>` (the pawn placed at a station, section 9)
and `pull:<piece>` (the view turned to a piece, such as `near hook 1` or `carousel hook 1`, and the grapple held),
and `go:<stage>` (what a click on that entry of the stage list does: `playground`, `graybox`, an arena's id, or a
converted map's name). A step lasts 180 frames, or the seconds written after an `@` (`pull:far hook 1@4`). It
cannot start the first session and does nothing without one; the two playground steps act through the session's
teleport command, which is logged like any other. A developer's tool, for example:

```sh
ASAMU_LAB_VIEW="play@4,at:near@3,pull:near hook 1@4,at:wheel@4,stage@4" \
  asamu --sandbox --hidden-window --watch-window DIR --exit-after 20
```

In a run on converted data the playground shows no converted level, and the app's unattended captures wait for a
converted level's assets: there `--screenshot` is taken at its deadline (two seconds before `--exit-after`), and
`--record-frames` does not start by itself (F12 starts it). Without converted data both work as in any run.

## 12. Known limitations, and what comes later

Limitations of this version:

- **The playground has not been played by a person.** Its routes are done by the tests' pilot, which is not a
  player: how the jumps, the pulls and the stretch in the air feel in the hand, whether the hooks sit where the
  eye expects them, and whether the level is fun, is for someone with a mouse to say. The same goes for mouse
  clicks on the panel, starting a converted map from the stage list, and saving and loading a profile from disk.
- **The playground's look is plain**: flat colours on its own generated solids, no textures. Dressing it with the
  user's own converted meshes and materials (floating rocks, crystals, lamps), on top of the same collision, was
  left for later.
- **Only the hub is a checkpoint**, because the game's checkpoints only move forward. A fall from the end of a
  long chain is a walk back, or a teleport to a station (G).
- **The lab arenas are not offered in a run on converted data** (they are built for the graybox composition; the
  playground runs in both).
- **A hand-made scene level is labelled as converted by the game layer** (section 11); the Sandbox corrects the
  name where it prints it.
- **A start from the command line leaves the mouse free** until a click, as every run that skips the menu does;
  the screen says "click to play".
- **The new CI steps have not run yet** (the clippy pass without the feature, the app's Sandbox tests on Linux).
- **37 of the 67 keys are unclassified**: whether a change reaches the running pawn at once is not established
  for them.
- **Script and native constants cannot be tuned.**
- **Spawn-only keys do not follow** on a level the loader built; the level's own ability state wins.
- **Save states on scripted levels restore the simulation only**, and are in memory only. There are no save
  states on disk.
- **Rewind** exists on the hand-made levels only (the playground, the graybox and the lab arenas; not on
  converted maps, where the cost of a keyframe is unmeasured) and steps by keyframe, not by tick. Save states work
  everywhere.
- **Time control scales everything on the virtual clock**, including presentation timers and audio timing.
- **The predicted arc assumes a static world.**
- **A shared in-memory save session.** In a run that is not a `--sandbox` process but keeps its saves in memory
  anyway (`--level`, or no user data folder), a session on a converted map shares that in-memory save session with
  Classic play of the same run. Nothing reaches the disk.
- **The developer read-out's key list is Classic's** during a session (section 7).
- **No text input, sliders or scroll areas.** Profiles are saved under generated names; long lists are paged.
- **With the inspector open and "freeze while open" off**, you can still move, but the grapple cannot fire, and the
  right mouse button still charges the power jump.
- **The lab arenas are box levels**: no slopes. Their jump rulers are measurements of this recreation.
- **Release builds would include the Sandbox**, because the feature is on by default. Whether packaged releases
  should carry it is an open maintainer decision.

Later, in rough priority order. None of this exists yet:

1. Hardening that needs sign-off because it touches Classic code: keep Classic's developer keys from reaching a
   save on disk; label a tuned parameter set at the source in the game's trace recorder; make the parity tool
   refuse a `sandbox:` note; a constructor for hand-made scene levels, so the playground carries its own origin.
2. The playground, after someone has played it: its distances and heights tuned by hand, checkpoints of its own
   kind at the ends of the chains, the user's converted meshes and materials as dressing, more routes.
3. Lab depth: an input tap for exact per-tick rewind and deterministic replay of recordings; restoring
   presentation state on scripted levels; ghost playback; a text console; sliders.
4. Switching between Classic and the Sandbox in one process, with a unified stage picker.
5. World inspection: collision-mesh rendering, hit details, a level-script panel.
6. Authoring on hand-made levels: arena files, placed boxes and hooks.
7. Triangle arenas with slopes (the playground has slopes: its ramps).
8. Overlays on converted maps.
9. A foundation for mods: overlay directories merged at load, script constants as parameters (a Classic change
   with its own guard), editor gizmos.
