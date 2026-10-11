# Parity

We measure **behavioural parity** with the original game, not byte-matching builds. A matching build, a
deterministic export and behavioural parity are separate numbers and are never added together.

| Area | Measure | Oracle | Status |
|---|---|---|---|
| Package reading | Every shipped package parses; offsets/sizes cross-check against file size and each other | The original files | verified (see `docs/reverse-engineering/PACKAGE_ANALYSIS.md`) |
| Gameplay constants | Value recovered from script defaults / native code / config, cited | The original files | movement, camera, pawn-script, grapple-gun and rocket-boots values recovered and wired with provenance (drift-tested against `docs/reverse-engineering/data/defaults/*.json`) |
| Player movement | Tick-by-tick position/velocity error vs original trace | Recordings of the original game (`docs/TRACE_CAPTURE.md`) | native physics (walking, falling, flying) + ASAMU pawn script, power jump and rocket boots ported from the specs; **not measured yet**: the first recordings of the original exist (2026-10-10, Windows build) and the first replay diverges, see "First comparison" below |
| Grapple | Attach point, pull path, release velocity vs original trace | Recordings of the original game | original `GrappleGun` ported from `GRAPPLE.md` (targeting, acceptance, attach, flying pull, every release path, budget, Kismet events); reproduces the spec's re-simulated numbers (G-PH-5); **not measured** against the recordings yet |
| Levels | Geometry/transforms match original placement | Original maps (converted locally) | structure verified by the importer (30,284 actor transforms and all static meshes agree with an independent decoder, `LEVEL_FORMAT.md`, `MESHES.md`); collision and visual agreement with the running original **not measured** |
| Story flow | Kismet-driven event order matches original | Kismet graphs + playthrough | the interpreter runs every sequence-object class the maps use and each level's scripted exit leads to the next in the smoke run (`INTEGRATION.md` §8); event order and timing against a playthrough of the original **not measured** |

### First comparison (2026-10-10)

One recording of the original has been replayed so far: 50 s in AG-Workshop (story mode), Windows build, gamepad,
variable frame lengths (`asamu-trace replay --compare` with each sample's own frame length). Verdict: diverged.
This is a first look, not a parity number.

| Observation | Status |
|---|---|
| The recorded walk gains 33.5 uu/s per 16.4 ms frame up to its cap: acceleration 2048 uu/s², one step per frame | agrees with `NATIVE_PHYSICS.md` (walking acceleration); CONFIRMED by the recording |
| The original's `GroundSpeed` during the Workshop's story mode read 132 (66 about a second after level start); AG-BeautifulCity's story mode read 264 | our story-mode speed in the Workshop has to be checked against this; cause not yet traced (the level's Kismet is the first suspect) |
| With a gamepad the recorder sees no stick deflection at its sample point, so the converted trace has no movement input and the replay does not move | a converter limitation, not a simulation difference; the recorded pawn acceleration carries the direction |
| Position differs by 1 uu from the first tick | not yet classified |
| FOV differs by 40° from the first zoom | consistent with the story-mode zoom not being reproduced in the replay; not yet classified |

No tolerance was changed and no item was upgraded on the strength of this run.

## Parameters

The runtime has two parameter sets (`crates/asamu-player/src/params.rs`):

- **`PlayerParams::asamu_original()`** — the default of `asamu-game` and the app. Every recovered value carries
  its provenance: `script_default` = the class whose default object stores the value (the most derived one in
  the inheritance chain) and the property; `config` = the original `.ini` file and `[Section] Key`. The values
  come from `docs/reverse-engineering/DEFAULTS.md` and its JSON data; the test
  `crates/asamu-player/tests/original_params.rs` re-reads the committed JSON and fails if a value or a
  provenance differs. The pawn group (`pawn.*`) feeds the ASAMU pawn script layer (`asamu_player::pawn`, below);
  `gun.*` the original grapple gun (`asamu_player::grapple_gun`) and `boots.*` the rocket boots
  (`asamu_player::rocket_boots`). The only placeholders left are values read **only by the debug models**:
  `movement.gravity_z` / `movement.braking_deceleration` (the placeholder movement model) and the rope group
  `grapple.*` (the placeholder rope grapple, which runs only without the script layer).
- **`PlayerParams::placeholder()`** (= `Default`) — the old graybox values, kept for the placeholder model, the
  placeholder rope grapple and the raw-physics tests. It has no pawn, gun or boots group, so it runs without the
  script layer.

A Sandbox session ([SANDBOX.md](SANDBOX.md)) runs a copy of the original set with overrides: each overridden
value carries `placeholder` provenance with a `sandbox override` note, and nothing a session shows or records is
parity evidence.

Rules and literal constants of the original script code are named constants in `asamu_player::pawn`,
`asamu_player::grapple_gun` and `asamu_player::rocket_boots` with provenance `script_code` (class and
function/state of the original UnrealScript that holds them; values from `docs/reverse-engineering/ABILITIES.md`
and `GRAPPLE.md`, never from copied script text). Native-physics algorithm constants
are listed in `crates/asamu-player/src/ue3_movement.rs` (provenance `native_code`, see "Movement model").

All tables below are generated by `cargo run -p asamu-player --example provenance_table`; the test
`crates/asamu-player/tests/docs_sync.rs` fails if this file drifts from the code.

### Original parameter set (`PlayerParams::asamu_original`)

| Parameter | Value | Unit | Provenance | Note / source |
|---|---|---|---|---|
| `movement.gravity_z` | -1000 | uu/s^2 | placeholder | graybox placeholder used only by PlaceholderMovement (the UE3 pawn model uses movement.world_gravity_z x movement.custom_gravity_scaling, doubled in effect by the falling refinement) |
| `movement.max_ground_speed` | 440 | uu/s | script_default | script default UTGame.UTPawn.GroundSpeed |
| `movement.ground_acceleration` | 2048 | uu/s^2 | script_default | script default Engine.Pawn.AccelRate |
| `movement.braking_deceleration` | 3000 | uu/s^2 | placeholder | graybox placeholder used only by PlaceholderMovement (the original brakes with 2 x ground friction, see movement.ground_friction) |
| `movement.air_control` | 0.3 | fraction | script_default | script default asamu.ASAMUPawn.AirControl |
| `movement.jump_velocity` | 1000 | uu/s | script_default | script default asamu.ASAMUPawn.JumpZ |
| `movement.capsule_radius` | 21 | uu | script_default | script default UTGame.UTPawn.CollisionCylinder.CollisionRadius |
| `movement.capsule_half_height` | 44 | uu | script_default | script default UTGame.UTPawn.CollisionCylinder.CollisionHeight |
| `movement.step_height` | 26 | uu | script_default | script default UTGame.UTPawn.MaxStepHeight |
| `movement.max_fall_speed` | 2500 | uu/s | script_default | script default asamu.ASAMUPawn.MaxFallSpeed |
| `movement.walkable_floor_z` | 0.78 | normal z | script_default | script default UTGame.UTPawn.WalkableFloorZ |
| `movement.world_gravity_z` | -520 | uu/s^2 | config | config ASAMU/Config/DefaultGame.ini [Engine.WorldInfo] DefaultGravityZ |
| `movement.custom_gravity_scaling` | 1 | factor | script_default | script default UDKBase.UDKPawn.CustomGravityScaling |
| `movement.ground_friction` | 8 | 1/s | script_default | script default Engine.PhysicsVolume.GroundFriction |
| `movement.terminal_velocity` | 10000 | uu/s | script_default | script default asamu.ASAMUPawn.fTerminalVelocity |
| `movement.limit_fall_accel` | true | - | script_default | script default Engine.Pawn.bLimitFallAccel |
| `movement.slope_boost_friction` | 0.2 | friction | script_default | script default UTGame.UTPawn.SlopeBoostFriction |
| `movement.movement_speed_modifier` | 1 | factor | script_default | script default Engine.Pawn.MovementSpeedModifier |
| `movement.air_speed` | 440 | uu/s | script_default | script default UTGame.UTPawn.AirSpeed |
| `movement.fluid_friction` | 0.3 | 1/s | script_default | script default Engine.PhysicsVolume.FluidFriction |
| `grapple.max_range` | 2500 | uu | placeholder | graybox placeholder of the debug rope grapple (raw pipeline only); the original grapple uses gun.max_distance (GrappleGun fMaxDistance) |
| `grapple.pull_acceleration` | 900 | uu/s^2 | placeholder | graybox placeholder of the debug rope grapple (raw pipeline only); the original pull is 10^7/d uu/s^2 (GRAPPLE.md G-PH-2) |
| `grapple.min_rope_length` | 60 | uu | placeholder | graybox placeholder of the debug rope grapple (raw pipeline only); the original has no rope (GRAPPLE.md G-PH-5) |
| `grapple.rope_mode` | inelastic | - | placeholder | graybox placeholder of the debug rope grapple (raw pipeline only); the original has no rope (GRAPPLE.md G-PH-5) |
| `grapple.release_mode` | preserve_velocity | - | placeholder | graybox placeholder of the debug rope grapple (raw pipeline only); the original keeps the velocity except on a proximity release (G-RL-1/2) |
| `grapple.attached_max_speed` | 2500 | uu/s | placeholder | graybox placeholder of the debug rope grapple (raw pipeline only); the original caps at AirSpeed = gun.grapple_accel (G-PH-3/4) |
| `camera.fov_degrees` | 90 | deg (horizontal) | config | config ASAMU/Config/DefaultSettings.ini [ASAMU.ASAMUSettingsManager] FOV |
| `camera.eye_height` | 38 | uu | script_default | script default UTGame.UTPawn.BaseEyeHeight |
| `camera.max_pitch_degrees` | 98.87695 | deg | script_default | script default UTGame.UTPawn.ViewPitchMax |
| `pawn.move_speed` | 440 | uu/s | script_default | script default asamu.ASAMUPawn.MoveSpeed |
| `pawn.sprint_speed_multiplier` | 2 | factor | script_default | script default asamu.ASAMUPawn.bSprintSpeedMultiplier |
| `pawn.story_speed_multiplier` | 0.6 | factor | script_default | script default asamu.ASAMUPawn.storyModeSpeedMultiplier |
| `pawn.landed_air_control` | 0.35 | fraction | script_default | script default UTGame.UTPawn.DefaultAirControl |
| `pawn.hard_landing_threshold` | 2000 | uu/s | script_default | script default asamu.ASAMUPawn.hardLandingThreshold |
| `pawn.land_sound_threshold` | 500 | uu/s | script_default | script default asamu.ASAMUPawn.normalLandSoundVelocityThreshold |
| `pawn.zoom_enabled` | true | - | script_default | script default asamu.ASAMUPawn.bZoomEnabled |
| `pawn.zoom_fov` | 50 | deg (horizontal) | script_default | script default asamu.ASAMUPawn.zoomFOV |
| `pawn.zoom_duration` | 0.3 | s | script_default | script default asamu.ASAMUPawn.zoomDuration |
| `pawn.bob` | 0.01 | factor | config | config ASAMU/Config/DefaultGame.ini [UTGame.UTPawn] Bob |
| `pawn.weapon_bob` | true | - | config | config ASAMU/Config/DefaultGame.ini [UTGame.UTPawn] bWeaponBob |
| `pawn.bob_rate_story` | 0.55 | factor | script_default | script default asamu.ASAMUPawn.storyModeBobSpeedMultiplier |
| `pawn.bob_rate_sprint` | 0.85 | factor | script_default | script default asamu.ASAMUPawn.sprintingBobSpeedMultiplier |
| `pawn.bob_rate_walk` | 0.65 | factor | script_default | script default asamu.ASAMUPawn.walkingNormallyBobSpeedMultiplier |
| `pawn.custom_time_dilation` | 1 | factor | script_default | script default Engine.Actor.CustomTimeDilation |
| `pawn.power_jump_charge_time` | 0.6 | s | script_default | script default asamu.ASAMUPowerJump.powerJumpChargeTime |
| `pawn.power_jump_strength` | 1600 | uu/s | script_default | script default asamu.ASAMUPowerJump.powerJumpStrength |
| `pawn.power_leap_horizontal_multiplier` | 2 | factor | script_default | script default asamu.ASAMUPowerJump.powerLeapHorizontalStrengthMultiplier |
| `pawn.power_leap_vertical_strength` | 750 | uu/s | script_default | script default asamu.ASAMUPowerJump.powerLeapVerticalStrength |
| `gun.max_distance` | 5000 | uu | script_default | script default asamu.GrappleGun.fMaxDistance |
| `gun.weapon_range` | 16384 | uu | script_default | script default Engine.Weapon.WeaponRange |
| `gun.release_distance` | 200 | uu | script_default | script default asamu.GrappleGun.fGrappleReleaseDistance |
| `gun.grapple_accel` | 2000 | uu/s | script_default | script default asamu.GrappleGun.fGrappleAccel |
| `gun.max_speed` | 10000 | uu/s | script_default | script default asamu.GrappleGun.fGrappleMaxSpeed |
| `gun.fire_interval` | 0.1 | s | script_default | script default asamu.GrappleGun.FireInterval[0] |
| `gun.instant_release_delay` | 0.05 | s | script_default | script default asamu.GrappleGun.instantReleaseDelay |
| `gun.top_grapple_angle` | 0.8 | normal z | script_default | script default asamu.GrappleGun.TOP_GRAPPLE_ANGLE |
| `gun.bottom_grapple_angle` | 0.8 | normal z | script_default | script default asamu.GrappleGun.BOTTOM_GRAPPLE_ANGLE |
| `gun.interact_range` | 200 | uu | script_default | script default asamu.GrappleGun.interactRange |
| `gun.initial_max_grapples` | 0 | count | script_default | script default asamu.GrappleGun.iMaxGrapples |
| `gun.initial_can_grapple` | true | - | script_default | script default asamu.GrappleGun.bCanGrapple |
| `boots.boost_delay` | 1 | s | script_default | script default asamu.ASAMURocketBoots.boostDelay |
| `boots.boost_duration` | 2 | s | script_default | script default asamu.ASAMURocketBoots.boostDuration |
| `boots.boost_strength` | 2500 | uu/s | script_default | script default asamu.ASAMURocketBoots.boostStrength |
| `boots.boost_spiral_strength` | 1000 | uu/s | script_default | script default asamu.ASAMURocketBoots.boostSpiralStrength |
| `boots.total_spin_angle` | 720 | deg | script_default | script default asamu.ASAMURocketBoots.totalSpinAngle |
| `boots.boost_exhausted_delay` | 1 | s | script_default | script default asamu.ASAMURocketBoots.boostExhaustedDelay |
| `boots.initial_enabled` | false | - | script_default | script default asamu.ASAMURocketBoots.bEnabled |

Notes on individual values:

- `movement.terminal_velocity`: the physics volume's own default is 4000 (`Engine.PhysicsVolume`), but
  `ASAMUPawn` writes its `fTerminalVelocity` (10 000) into the volume at pawn start, so 10 000 is the effective
  value (ABILITIES.md §1). No AG map places a physics volume.
- `movement.max_ground_speed` is the class default `GroundSpeed`; at run time the script layer writes
  `GroundSpeed` (`MoveSpeed` 440, × 2 sprint, × 0.6 story), which the model reads through `PawnHooks`.
- `movement.max_fall_speed` (`MaxFallSpeed`) is not a speed limit in the original; it only selects landing
  sound cues. The placeholder model uses the field as its fall-speed cap.
- `camera.max_pitch_degrees` is `ViewPitchMax` = 18 000 rotator units = 98.876953125°, i.e. the view can pitch
  slightly past vertical (ABILITIES.md A-CM-2).
- `camera.eye_height` is `BaseEyeHeight`; the run-time `EyeHeight` starts at the same 38 and is smoothed by
  the script layer.
- `movement.air_speed` is the class default `AirSpeed` (440); the grapple gun writes `gun.grapple_accel` (2000)
  into the run-time value at its spawn and after every release (G-PH-4), so the flying cap is 2000 in play.
- `gun.max_speed` (`fGrappleMaxSpeed`, 10 000) is recorded but never read by the original script (GRAPPLE.md §2).
- `gun.initial_max_grapples` (`iMaxGrapples`) and `boots.initial_enabled` (`bEnabled`) have source `zero` in the
  data (no default object stores them, so they are 0/false, STRONG); levels change them through Kismet
  (`asamu_world::LevelAbilities`).
- `gun.fire_interval` is element 0 of `FireInterval` (fire mode 0, the only one the gun uses).

### Script-code constants (`asamu_player::pawn`, `grapple_gun`, `rocket_boots`)

| Constant | Value | Unit | Provenance | Meaning |
|---|---|---|---|---|
| `JUMP_RELEASE_MULTIPLIER` | 0.7 | factor | script code asamu.ASAMUPawn.ReleasedJump | V.z factor per damping step after the jump key is released |
| `JUMP_RELEASE_INTERVAL` | 0.1 | s | script code asamu.ASAMUPawn.ReleasedJump | latent sleep between damping steps |
| `JUMP_RELEASE_MIN_VELOCITY_Z` | 0.05 | uu/s | script code asamu.ASAMUPawn.ReleasedJump | damping continues while V.z exceeds this |
| `LANDED_EYE_RESET_SPEED` | 200 | uu/s | script code asamu.ASAMUPawn.Landed | V.z < -this resets the eye-smoothing baseline at a normal landing |
| `LAND_CUE_FALL_SPEED_FACTOR` | 0.5 | factor | script code asamu.ASAMUPawn.Landed | second landing cue below -MaxFallSpeed x this |
| `HAS_LANDED_IDLE_DELAY` | 1 | s | script code asamu.ASAMUPawn.HasLanded | HasLanded -> Idle delay (cosmetic) |
| `ZOOM_STEP` | 0.016 | s | script code asamu.ASAMUPawn.Zooming | zoom step: latent sleep and FOV fraction step/zoomDuration |
| `EYE_SMOOTH_RATE` | 10 | 1/s | script code asamu.ASAMUPawn.UpdateEyeHeight | eye smoothing k = min(0.9, 10 dt / CustomTimeDilation) |
| `EYE_SMOOTH_MAX` | 0.9 | fraction | script code asamu.ASAMUPawn.UpdateEyeHeight | upper bound of the eye smoothing factor |
| `EYE_MIN_HEIGHT_FACTOR` | 0.5 | factor | script code asamu.ASAMUPawn.UpdateEyeHeight | walking EyeHeight >= -this x CollisionHeight |
| `EYE_CEILING_MARGIN` | 12 | uu | script code asamu.ASAMUPawn.UpdateEyeHeight | ceiling probe when CollisionHeight - EyeHeight < this |
| `EYE_CEILING_PROBE_EXTENT` | 12 | uu | script code asamu.ASAMUPawn.UpdateEyeHeight | half-extent of the ceiling probe box |
| `BOB_IDLE_SPEED` | 10 | uu/s | script code asamu.ASAMUPawn.UpdateEyeHeight | below this speed: idle bob rate, no vertical bob |
| `BOB_IDLE_RATE` | 0.2 | factor | script code asamu.ASAMUPawn.UpdateEyeHeight | bob phase rate when nearly standing |
| `BOB_LATERAL_FREQUENCY` | 8 | factor | script code asamu.ASAMUPawn.UpdateEyeHeight | lateral bob sin(8 phase) |
| `BOB_VERTICAL_FREQUENCY` | 16 | factor | script code asamu.ASAMUPawn.UpdateEyeHeight | vertical bob sin(16 phase) |
| `BOB_VERTICAL_FACTOR` | 0.75 | factor | script code asamu.ASAMUPawn.UpdateEyeHeight | vertical bob amplitude 0.75 Bob speed |
| `BOB_CLAMP` | 0.05 | factor | script code asamu.ASAMUPawn.UpdateEyeHeight | Bob clamped to +-this |
| `BOB_AIR_DECAY_RATE` | 8 | 1/s | script code asamu.ASAMUPawn.UpdateEyeHeight | walk-bob decay off the ground |
| `BOB_DISABLED_FACTOR` | 0.1 | factor | script code asamu.ASAMUPawn.UpdateEyeHeight | walk-bob scale when bWeaponBob is false |
| `PULL_NUMERATOR` | 10000 | factor | script code asamu.GrappleGun.UpdateGrapple | pull dt x unit(A-P) x 10000 / (d / 1000): 10^7/d uu/s^2 |
| `PULL_DISTANCE_SCALE` | 1000 | uu | script code asamu.GrappleGun.UpdateGrapple | distance divisor of the pull |
| `PROXIMITY_VELOCITY_DIVISOR` | 2 | factor | script code asamu.GrappleGun.UpdateGrapple | velocity divided by this at the proximity release |
| `CROSSHAIR_EXTRA_RANGE` | 1000 | uu | script code asamu.GrappleGun.CheckForRange | crosshair trace length fMaxDistance + this |
| `UNLIMITED_GRAPPLES` | 32767 | count | script code asamu.GrappleGun.SetMaxGrapples | capacity for SetMaxGrapples(n <= -1) |
| `COUNTER_CLAMP` | 3 | count | script code asamu.GrappleGunLightManager.UpdateLights | used count clamped to [0, 3] every gun tick |
| `HIDE_HAND_DELAY` | 0.3 | s | script code asamu.GrappleGun.HideGrappleGun | animated hide with visibility off: hand hidden after this |
| `CHARGE_STEP` | 0.1 | s | script code asamu.ASAMURocketBoots.Boosting | charge write step and latent sleep |
| `BOOST_STEP` | 0.05 | s | script code asamu.ASAMURocketBoots.Boosting | boost write step and latent sleep |
| `SPIRAL_FORWARD` | 1 | uu/s | script code asamu.ASAMURocketBoots.Boosting | local X of the corkscrew offset before scaling |
| `DEGREES_TO_RADIANS` | 0.017453292 | rad/deg | script code asamu.ASAMURocketBoots.ConvertRadiansToDegrees | spin angle (degrees) to radians |

### Placeholder parameter set (`PlayerParams::placeholder`, debugging)

| Parameter | Value | Unit | Provenance | Note / source |
|---|---|---|---|---|
| `movement.gravity_z` | -1000 | uu/s^2 | placeholder | graybox placeholder used only by PlaceholderMovement (the UE3 pawn model uses movement.world_gravity_z x movement.custom_gravity_scaling, doubled in effect by the falling refinement) |
| `movement.max_ground_speed` | 450 | uu/s | placeholder | graybox placeholder (debugging); the original value is in PlayerParams::asamu_original (UTPawn GroundSpeed) |
| `movement.ground_acceleration` | 3000 | uu/s^2 | placeholder | graybox placeholder (debugging); the original value is in PlayerParams::asamu_original (Pawn AccelRate) |
| `movement.braking_deceleration` | 3000 | uu/s^2 | placeholder | graybox placeholder used only by PlaceholderMovement (the original brakes with 2 x ground friction, see movement.ground_friction) |
| `movement.air_control` | 0.25 | fraction | placeholder | graybox placeholder (debugging); the original value is in PlayerParams::asamu_original (ASAMUPawn AirControl) |
| `movement.jump_velocity` | 450 | uu/s | placeholder | graybox placeholder (debugging); the original value is in PlayerParams::asamu_original (ASAMUPawn JumpZ) |
| `movement.capsule_radius` | 20 | uu | placeholder | graybox placeholder (debugging); the original value is in PlayerParams::asamu_original (UTPawn collision cylinder radius) |
| `movement.capsule_half_height` | 45 | uu | placeholder | graybox placeholder (debugging); the original value is in PlayerParams::asamu_original (UTPawn collision cylinder height) |
| `movement.step_height` | 18 | uu | placeholder | graybox placeholder (debugging); the original value is in PlayerParams::asamu_original (UTPawn MaxStepHeight) |
| `movement.max_fall_speed` | 4000 | uu/s | placeholder | graybox placeholder used only by PlaceholderMovement (the original clamps the 3-D falling speed to movement.terminal_velocity) |
| `movement.walkable_floor_z` | 0.7 | normal z | placeholder | graybox placeholder (debugging); the original value is in PlayerParams::asamu_original (UTPawn WalkableFloorZ) |
| `movement.world_gravity_z` | -520 | uu/s^2 | config | config ASAMU/Config/DefaultGame.ini [Engine.WorldInfo] DefaultGravityZ |
| `movement.custom_gravity_scaling` | 1 | factor | placeholder | graybox placeholder (debugging); the original value is in PlayerParams::asamu_original (UDKPawn CustomGravityScaling) |
| `movement.ground_friction` | 6 | 1/s | placeholder | graybox placeholder (debugging); the original value is in PlayerParams::asamu_original (PhysicsVolume GroundFriction) |
| `movement.terminal_velocity` | 4000 | uu/s | placeholder | graybox placeholder (debugging); the original value is in PlayerParams::asamu_original (ASAMUPawn fTerminalVelocity) |
| `movement.limit_fall_accel` | true | - | placeholder | graybox placeholder (debugging); the original value is in PlayerParams::asamu_original (Pawn bLimitFallAccel) |
| `movement.slope_boost_friction` | 0.5 | friction | placeholder | graybox placeholder (debugging); the original value is in PlayerParams::asamu_original (UTPawn SlopeBoostFriction) |
| `movement.movement_speed_modifier` | 1 | factor | placeholder | graybox placeholder (debugging); the original value is in PlayerParams::asamu_original (Pawn MovementSpeedModifier) |
| `movement.air_speed` | 450 | uu/s | placeholder | graybox placeholder (debugging; nothing flies without the script layer); the original value is in PlayerParams::asamu_original (UTPawn AirSpeed) |
| `movement.fluid_friction` | 0.3 | 1/s | placeholder | graybox placeholder (debugging; nothing flies without the script layer); the original value is in PlayerParams::asamu_original (PhysicsVolume FluidFriction) |
| `grapple.max_range` | 2500 | uu | placeholder | graybox placeholder of the debug rope grapple (raw pipeline only); the original grapple uses gun.max_distance (GrappleGun fMaxDistance) |
| `grapple.pull_acceleration` | 900 | uu/s^2 | placeholder | graybox placeholder of the debug rope grapple (raw pipeline only); the original pull is 10^7/d uu/s^2 (GRAPPLE.md G-PH-2) |
| `grapple.min_rope_length` | 60 | uu | placeholder | graybox placeholder of the debug rope grapple (raw pipeline only); the original has no rope (GRAPPLE.md G-PH-5) |
| `grapple.rope_mode` | inelastic | - | placeholder | graybox placeholder of the debug rope grapple (raw pipeline only); the original has no rope (GRAPPLE.md G-PH-5) |
| `grapple.release_mode` | preserve_velocity | - | placeholder | graybox placeholder of the debug rope grapple (raw pipeline only); the original keeps the velocity except on a proximity release (G-RL-1/2) |
| `grapple.attached_max_speed` | 2500 | uu/s | placeholder | graybox placeholder of the debug rope grapple (raw pipeline only); the original caps at AirSpeed = gun.grapple_accel (G-PH-3/4) |
| `camera.fov_degrees` | 90 | deg (horizontal) | placeholder | graybox placeholder (debugging); the original value is in PlayerParams::asamu_original (settings FOV) |
| `camera.eye_height` | 38 | uu | placeholder | graybox placeholder (debugging); the original value is in PlayerParams::asamu_original (UTPawn BaseEyeHeight) |
| `camera.max_pitch_degrees` | 89 | deg | placeholder | graybox placeholder (debugging); the original value is in PlayerParams::asamu_original (UTPawn ViewPitchMax) |

## Known deviations

Every known difference between the runtime and the original (stage 2 gameplay: script layer, grapple gun, rocket
boots, world objects):

| # | Deviation | Why / status |
|---|---|---|
| 1 | The grapple gun starts each level `Active`: the stock 0.33 s equip delay (`EquipTime`) is not modelled. | GRAPPLE.md open question 6 (UNKNOWN). |
| 2 | The fire trace evaluates a single impact: non-blocking `Trigger`/`TriggerVolume` actors are treated as transparent (G-TG-1); camera-animation offsets (grapple loop, landing, boots) are not added to the aim rotation. Every aim (fire trace, refire-served fire, crosshair, boost aim) uses the camera's cached point of view — the view rotation as the previous tick ended (`PawnScript::pov_yaw`/`pov_pitch`, recorded when a tick begins), because the camera updates once per frame after all actor tick groups (native `UWorld::Tick` calls the camera's `UpdateCamera` event after the last `TickActors` group; CONFIRMED (native), verification pass 2) — so aims taken after the controller's look update lack that tick's look delta; the trace origin is the live view location (`UTPawn` override). | Open questions 1 and 3; camera animations are content not imported yet. |
| 3 | Cinematic mode and "players only" are not modelled (they gate fire presses, G-IN-1, the power-jump key, and stack on the move-input lock, A-IL-1). | Kismet stage. |
| 4 | Without a Kismet runtime a level states its ability state directly (`asamu_world::LevelAbilities`: capacity and boots at level start). The graybox uses `LevelAbilities::graybox_test()` — every ability on, 3 grapples — a **test configuration**. `asamu_world::ORIGINAL_ABILITY_ACTIONS` lists each map's `SetMaxGrapples` / `ToggleGrapple` / `ToggleRocketBoots` actions (CONFIRMED values); which ones run at level start is per path and not derived (TENTATIVE). | Kismet stage. |
| 5 | The anchor helper follows a mover's translation only (rotation with the base is TENTATIVE, G-AT-8). Graybox movers move on **our** back-and-forth path, not on Matinee data, and the pawn is never based on a mover (the physics port maps every surface to static geometry). | Matinee/basing not imported yet. |
| 6 | World objects: falling rocks (`ASAMUFallingRock`, `ASAMUFallingWhenGrappledRock`, G-WO-3/4) exist only as surface classes (no motion, deceleration or death reset); the glow flower's glow and the crystals' fades are visual and not modelled; crystal charge is not saved. The charged crystal's refill (its `GrappledState` begin code) is applied by the gun from the crystal's charge in the collision world at hit time, in the map-actor step of the same tick — equivalent to the original because the fire runs in the input event against the world as the previous tick left it and the map actors tick after it (`asamu_player::begin_step` / `finish_step`; before verification pass 2 `asamu-game` ticked the map actors before the input events, so a crystal recharging in the press tick was already charged and the trace saw movers one tick ahead). Re-entering `UnCharged` restarts its fade and its whole `RechargeDelay` (UE3 same-state `GotoState` jumps back to `Begin`, STRONG; tested). | Later stages (IceCave content, save system). |
| 7 | Story-mode interaction: the gun raises `InteractWith`; the use count (`MaxInteractTimes`, class default 1) is kept by `asamu-game`'s world objects, which then fire `SeqEvent_ActorInteractedWith`. The story crosshair treats every interactable as still accepting. | The count belongs to the world actor. |
| 8 | Rocket boots: the corkscrew offset is rotated with the exact aim direction instead of the original's integer rotator (≤ 2π/65 536 difference); `sin`/`cos` come from `asamu_core::det_math` (the original's last bits may differ). | Determinism across platforms. |
| 9 | Robustness guards (not original): the grapple pull is skipped when the pawn centre is exactly at the anchor and the attractor write when the pawn is exactly at the pad (the original's arithmetic would produce NaN); looping timers fire at most 64 times per tick. | Unreachable in play. |
| 10 | When several grapple-gun timers fire in the same tick they run in the order instant release → refire check → hand hiding (TENTATIVE); every reachable combination gives the same outcome. | UE3 timer-array order not tracked. |
| 11 | Not exposed: the worm's push (G-RL-5; NPCs are a later stage), the `ResetPlayer`, `UnlimitedGrapples` (available as `grapple_gun::unlimited_grapples`), `WorkshopMode` (which would skip the counter clamp), `SetSpeed` and `changesize` console commands. | Level/Kismet stage. |
| 12 | Death and checkpoints follow the graybox rules (`asamu-game`): falling below `kill_z` respawns at once, without the original's 0.3 s fade during which the pawn keeps simulating; checkpoint volumes are ours, not `ASAMUCheckpoint`. The respawn itself follows the original's player reset (A-DT-2: grapple released with reason `Death`, rocket boots reset when enabled, teleport, velocity 0, spawn rotation, story mode exited; script state incl. grapple budget and physics mode kept). | ABILITIES.md §11 not ported yet. |
| 13 | Mouse look uses the app's own sensitivity. The original's `PlayerInput` look scaling (`MouseSensitivity` 30, `LookRightScale` 300, `LookUpScale` −250, mouse smoothing) is not ported because its exact formula is not specified in the evidence docs. Move axes need no scaling: the acceleration direction is normalised (A-WK-1). | Needs a spec of the axis path. |
| 14 | Camera animations (grapple begin/loop, landing, power-jump bobs, rocket boots, worm shake), the hand/jump bob, speed lines, rumble, the grapple beam, decals and sounds are not rendered. The simulation reports them as events (`StepEvents::landing`, `power_jump`, `gun`, `boots`, `kismet`). | Visual/audio stage. |
| 15 | The eye-height ceiling probe sweeps an upright cylinder (radius and half-height 12) instead of the original's box of half-extent 12: identical for horizontal ceilings, slightly different at edges. | Collision worlds sweep cylinders only. |
| 16 | Key events inside one tick are processed in a fixed order (sprint, jump release, jump press with the rocket-boost key, power-jump key, `use`, fire release/press); the original's order of several key events in one frame is UNKNOWN. Buttons are sampled as levels, so a press and a release of the same button inside one tick are not seen (a jump press and release inside the same tick give a full jump); G-AC-2's "released and re-pressed within one frame" case is therefore reachable only by state set-up (tested that way). | Tick-rate artefact; matches the original at one key event per frame. |
| 17 | ABILITIES.md §3 lists a "release at 0 s" apex of 121 uu; that figure assumes the first damping step in the jump frame. With the frame order above, the shortest damped jump releases one tick after the press (≈ 134.7 uu at 60 Hz); releases 0.1 / 0.2 / 0.4 / 0.7 s after the jump reproduce the spec's 198 / 266 / 371 / 461 uu (tested). | Spec figure vs event order; trace-verify. |
| 18 | The tick rate is fixed (60 Hz by default); the original runs variable frame times. Latent sleeps and actor timers (damping, zoom, power-jump charge, boots, gun refire and instant release, crystals, attractor) are frame-quantised exactly as in the original (G-TM-3/4), so their timing depends on the tick rate the same way. | Runtime choice. |
| 19 | The pitch limit is symmetric (±18 000 rotator units). The boundary handling of the stock view-rotation clamp (one-unit asymmetry, wrap at 65 535) is not verified. In the first walking tick after a grapple release the original takes the move axes from the pawn rotation that still carries the flying pitch (it matters only at |pitch| ≥ 90°, G-RL-7); our move direction uses the yaw only. | TENTATIVE details. |
| 20 | Whether the drop from a `PlayerStart` counts as a landing (AirControl 0.35 and a budget refill before the first jump) is UNKNOWN; our spawn places the pawn on the floor. | Trace-verify. |
| 21 | Trace samples do not carry the script state (gun budget, latch, boots, damping), so replaying from the middle of a trace restarts the script layer; the schema has no fields for the grapple budget or physics mode yet. | Schema v1 kept stable. |
| 22 | Pawn states that only drive animation (`IdleIdle`, `Running`) are folded into `Idle`; the cosmetic code of `Release` (→ `FallingState` after 1 s) is not modelled. | No behavioural effect. |
| 23 | With `PlaceholderMovement` (debug flag) the landing reaction runs after the whole move (the placeholder has no sub-steps), the fall speed is capped at `movement.max_fall_speed`, and a flying (grappling) pawn moves with the native port's `physFlying`. Without the script layer (`PlayerParams::placeholder`) the grapple is the placeholder rope model (`asamu_player::grapple`), not the original. | Debug configurations only. |
| 24 | `trace::record_run_with` (and any caller of `step_with`) uses one collision world for both halves of a tick; only `asamu-game` ticks map actors between the input events and the rest of the tick. A run with moving map actors therefore cannot be re-recorded from inputs alone against a static world. | Traces have no world-object state yet. |

## Trace format

Implemented in `crates/asamu-player/src/trace.rs` (schema version **1**). Recordings of the original (Windows
build, first made 2026-10-10; `docs/TRACE_CAPTURE.md`) and runtime traces share this schema so they can be
compared tick by tick.

### Layout: JSON Lines

- **Line 1** is a `TraceMeta` object; **every following non-blank line** is one `TraceSample` object.
- Schema version 1 was extended (stage 1 gameplay) by three optional input fields that default to `false`
  when absent, so older traces still parse; a reader built before the extension rejects new runtime traces
  (unknown fields), which is acceptable while no external reader exists.
  Blank lines are ignored; `\n` and `\r\n` endings are accepted; lines must be UTF-8 and at most
  `MAX_LINE_BYTES` (1 MiB). Unknown fields and duplicate keys are rejected (`deny_unknown_fields`), so
  typos fail loudly. Exactly one JSON object per line (trailing text is an error).
- Readers validate everything and report 1-based line numbers: missing/invalid meta, wrong `format`,
  unsupported `schema_version` (only `1` is accepted; there is no silent upgrade), `units` other than
  `"uu"`, a non-positive or non-finite `tick_rate`, invalid UTF-8, over-long lines, non-finite numbers
  (including JSON numbers that overflow `f32`), `fov` outside (0, 180), negative `rope_length`, ticks
  that do not strictly increase, and `grapple_state` / `grapple_anchor` / `rope_length` disagreement.
  Writers validate before writing. All of these are covered by tests in `trace.rs`.
- `f32` values are written as the exact decimal of the `f32` widened to `f64`
  (`asamu_core::exact_f32`), and `serde_json` is built with `float_roundtrip`, so runtime traces round-trip
  **bit-for-bit** (tested). Hand-written or recorder-produced traces may use any JSON number; values are
  parsed as `f64` and rounded to the nearest `f32`.

### Conventions

UE3 axes (left-handed: X forward, Y right, Z up), distances in **Unreal units**, velocities in UU/s,
yaw/pitch in **radians** using the UE3 rotator convention (+yaw turns right, +pitch looks up; convert
rotator units with `asamu_core::rotator`), FOV in **degrees, horizontal**. Sample `tick = k` holds the
input applied during tick `k` and the state at the **end** of that tick. A recording may start with a sample
whose input is neutral and whose state is the initial state (runtime recordings do; tick 0 for
`record_run`). Inputs are stored sanitized (exactly as the simulation consumed them), so replaying a trace's
inputs from its first state reproduces it.

### `TraceMeta` (line 1)

| Field | Type | Meaning |
|---|---|---|
| `format` | string | Always `"asamu-trace"`. |
| `schema_version` | integer | `1`. |
| `source` | `"original"` \| `"runtime"` | Who produced the trace. |
| `game_build` | string or null | Original build (e.g. Steam build id) when known. |
| `level` | string or null | Map / level name when known. |
| `tick_rate` | number or null | Fixed tick rate in Hz; `null` for variable-rate recordings (the original's tick model is UNKNOWN; UE3 uses variable `DeltaTime`). |
| `units` | string | Must be `"uu"`. |
| `notes` | array of strings | Free-form provenance/caveats (e.g. respawn ticks, "placeholder physics"). |

### `TraceSample` (lines 2+)

| Field | Type | Unit | Meaning |
|---|---|---|---|
| `tick` | integer | tick | Strictly increasing; the alignment key for comparison. |
| `time` | number | s | Time at the end of the tick. Informational; not compared. |
| `input` | object | — | `move_forward`, `move_right` (−1..1), `look_yaw_delta`, `look_pitch_delta` (rad), `jump_pressed` (edge), `jump_held`, `grapple_held` (level), and the optional (default `false`) `sprint_held`, `power_jump_held` (level) and `use_pressed` (edge) added with the script layer. |
| `position` | [x, y, z] | UU | Collision-shape centre after the tick. |
| `velocity` | [x, y, z] | UU/s | Velocity after the tick. |
| `yaw`, `pitch` | number | rad | View rotation. |
| `fov` | number | deg | Horizontal field of view (the run-time FOV: the story-mode zoom changes it). |
| `grapple_state` | `"idle"` \| `"attached"` | — | Grapple state (the original gun's or the debug rope's). |
| `grapple_anchor` | [x, y, z] or null | UU | Required when attached, null when idle. |
| `rope_length` | number or null | UU | Rope length of the debug rope grapple; always null for the original gun (no rope). |
| `grounded` | bool | — | Standing on a walkable floor. |

### Comparison (`trace::compare` / `compare_with`)

Samples are aligned by `tick` with a linear merge (no hash ordering). For each matched tick the harness
computes:

| Field | Error metric |
|---|---|
| position, velocity, grapple anchor | Euclidean distance (anchor only when both are attached) |
| rope length | absolute difference (only when both are attached and both record a length) |
| yaw | absolute angular difference, wrapped to [0, π] |
| pitch, fov | absolute difference |
| grapple state, grounded, input | mismatch counts |

Each continuous field reports `count`, `max` (and the tick of the max; ties keep the first), `mean` and
`rms`, accumulated in `f64`. The diff also reports matched / only-in-a / only-in-b tick counts and the
**first divergence**: the first tick at which any field exceeds its tolerance, checking fields in the
table's order within a tick (`CompareTolerances`; default all zero, i.e. any difference counts —
tolerances are an analysis choice per study, not a property of the game; NaN or negative tolerances act as
zero, `+inf` ignores a field). `TraceDiff::is_exact()` is true when nothing differs and no tick is
unmatched. The metrics are tested on hand-constructed traces with known answers.

### Determinism

Runtime traces are reproducible byte for byte: tests run the same scenario twice (and interleaved with
another simulation) and compare the serialized JSONL bytes. The simulation uses no platform maths
library: trigonometry comes from `asamu_core::det_math` (pure Rust, IEEE basic operations only, pinned by
golden bit patterns), so the same trace is expected on Windows, Linux and macOS. That cross-platform
expectation has **not** yet been checked by producing the same trace on each OS (TENTATIVE).

## Movement model

Two implementations of `asamu_player::movement::MovementModel` exist; `MovementModelKind` selects one at run
time. `MovementModelKind::Ue3Pawn` (the port) is the default of `MovementModelKind`, `asamu_game::Game` and
the app; `Game::graybox_placeholder()` / the app's `--placeholder` flag run the placeholder model (the raw
`sim::step` also still uses it). On top of either model, `sim::step_with` runs the ASAMU pawn script layer
whenever the parameters have a pawn group (the original set); see "Pawn script layer" below.

| Model | What it is | Status |
|---|---|---|
| `PlaceholderMovement` | Our own graybox model (see above) | placeholder, not parity-relevant |
| `Ue3PawnMovement` (`crates/asamu-player/src/ue3_movement.rs`) | Port of the original's native UE3/UDK pawn walking/falling physics, written independently from `docs/reverse-engineering/NATIVE_PHYSICS.md` (the spec; no decompiled code was used) | implemented and tested against closed-form consequences of the spec; **parity with the original not measured** (the first recordings of the original exist; this model has not been compared against them within a tolerance yet) |

### What `Ue3PawnMovement` reproduces (spec section → behaviour)

| Spec | Behaviour in the port |
|---|---|
| 1.3, 1.4 | `startNewPhysics` loop: slices < 0.0003 s ignored; sub-step = remaining if ≤ 0.05 s else `min(0.05, remaining/2)`; at most 8 sub-steps per tick shared by walking and falling, the rest dropped; walking → falling gives back `step·(1 − frac)` (frac = horizontal progress of the sub-step; 0 for an exactly zero move); a direct landing carries `remaining + step·(1 − Hit.Time)` into walking, a landing found after a slide carries 0. |
| 2.1 | UDK `CalcVelocity` once per walking call with the whole `dt`: acceleration = input direction × `AccelRate` (analog magnitude discarded), turning friction `V −= (V − dir·|V|)·F·dt`, 3-D cap at `GroundSpeed · MovementSpeedModifier`. |
| 2.3 | Braking without input: 0.03 s pieces, factor `1 − 2·F·h`, time-weighted average of the pieces, zero on reversal or below 10 uu/s. |
| 3.1, 3.2, 3.2.1 | Uphill moves on floors with normal Z < 0.98 go through `stepUp` (up `MaxStepHeight + 2`, across, down); wall hits step up, repeat while `|Delta|²·Time > 144` (climbs several stairs per sub-step), else slide along the horizontalised normal with one `TwoWallAdjust` retry. |
| 3.3 | Floor probe `MaxStepHeight + 2` below the centre; hover band: pushed up to 2.15 when < 1.9, snapped down to 2.15 when > 2.4 or on a new base; probe skipped for a zero move on an unchanged static base (floor reused, distance 2.4); steep floor pushed into → slide down the slope (`0.1·N` + projected `MaxStepHeight` drop). |
| 3.4 | No walkable floor → falling (a running player always falls off ledges); the steep-slope case that also hits an unwalkable surface reverts the step (TENTATIVE reading of the spec). |
| 3.5 | Slope gravity slide when floor Z < 0.99 and `Z·GroundFriction < 3.3`: `g·dt/(2·max(F, 0.5))·dt` projected on the floor, with the whole call's `dt`, every sub-step. |
| 3.1 (end) | Walking velocity = displacement / `dt` with Z = 0 (long frames under-report speed, e.g. 0.8× for a 0.5 s frame). |
| 4.1 | Gravity = `world_gravity_z × custom_gravity_scaling`. |
| 4.2 | Air-control wall probe (`AirControl > 0.05`); limiter when `limit_fall_accel`: `AccelRate·AirControl`, +`(10 − v)/dt` below 10 uu/s, `BoundSpeed` = current horizontal speed at/above `GroundSpeed`, or 1 uu/s² with air control ≤ 0.05. |
| 4.3, 4.6 | Semi-implicit sub-steps moving with the new velocity, then `V = 2·(displacement/step) − V_old` → effective acceleration doubled (gravity −1040 uu/s² with the config value), then 3-D `TerminalVelocity` clamp. |
| 4.4 | Landing on hit normal Z ≥ `WalkableFloorZ`; landing velocity re-derived when `step·Time > 0.003` and `Time > 0.1`. |
| 4.5, 4.7, 4.8 | Wall/ceiling slides with `CalculateSlopeSlide` (height clamp unless `SlopeBoostFriction = 0`), `TwoWallAdjust`, V-crease ("ditch") landing, no horizontal extrapolation after wall contact. |
| 5.2 | Landing sets the floor, the base, force-floor-check and a unit-length acceleration; the landing hit's actor tag (`NotLandable`) is passed to the pawn's `Landed`. |
| GRAPPLE G-PH-3 | `physFlying` (set by the grapple gun, `PawnPhysicsState::flying`): one update per tick without sub-steps; UDK `CalcVelocity` with fluid friction `0.5 × FluidFriction` (0.15 /s), no braking, the 3-D cap at the run-time `AirSpeed` (2000); no gravity; one move; on a blocking hit `Floor` = the hit normal, then either `stepUp` (near-vertical wall `|N.z| < 0.2` and `−0.2 < (0,0,−1)·unit(V) < 0.5`, the gained height left out of the velocity; the non-walking `stepUp` follows a non-walkable slope and does not step down) or a slide with one `TwoWallAdjust` retry; velocity from the displacement; never lands. |

Algorithm constants are `NativeCode` facts, kept as named constants with their symbol and data address in
`ue3_movement.rs` (0.0003, 8, 0.05, 0.5, 0.03, 2, 100, 1e-8, 1e-4, 0.98, 2.0, 2.4, 1.9, 2.15, 0.1, 0.99, 3.3,
0.5, −0.08, 144, 10, 0.003, 1e-4 (f64), 1.001, and for flying 0.5, 0.2, −0.2, 0.5).

### Parameter mapping (UE3 property → `MovementParams` field)

`ue3_movement::PawnTuning::from_params` is the single place where the port reads parameters;
`GroundSpeed` and `AirControl` are then replaced by the run-time values of the script layer
(`movement::PawnHooks`; `ClassDefaults` = the parameter values). The property names are CONFIRMED by the
native layout check in `docs/reverse-engineering/DEFAULTS.md` §2–3; the values are those of
`PlayerParams::asamu_original()` above.

| UE3 property (offset) | Field | Notes |
|---|---|---|
| `GroundSpeed` (Pawn+0x33C) | `movement.max_ground_speed` | walking cap; air `BoundSpeed` threshold |
| `AccelRate` (Pawn+0x34C) | `movement.ground_acceleration` | ground and air acceleration magnitude |
| `AirControl` (Pawn+0x35C) | `movement.air_control` | initial value; the script layer sets 0.35 after the first normal landing |
| `JumpZ` (Pawn+0x350) | `movement.jump_velocity` | read by the script layer's `DoJump` (and by the raw jump stand-in) |
| `MaxStepHeight` (Pawn+0x250) | `movement.step_height` | |
| `WalkableFloorZ` (Pawn+0x258) | `movement.walkable_floor_z` | |
| `CollisionRadius` / `CollisionHeight` (cylinder) | `movement.capsule_radius` / `movement.capsule_half_height` | cylinder half-height = UE3 `CollisionHeight` |
| `CustomGravityScaling` (UDKPawn+0x5A4) | `movement.custom_gravity_scaling` | |
| `WorldInfo` gravity (`DefaultGravityZ`, config) | `movement.world_gravity_z` | per-map `GlobalGravityZ`/GravityVolumes not modelled |
| `PhysicsVolume.GroundFriction` (+0x294) | `movement.ground_friction` | one implicit volume |
| `PhysicsVolume.TerminalVelocity` (+0x298) | `movement.terminal_velocity` | |
| Pawn+0x298 bit 51 (`bLimitFallAccel`) | `movement.limit_fall_accel` | default true (`Engine.Pawn`), never written by script |
| `SlopeBoostFriction` (UDKPawn+0x78C) | `movement.slope_boost_friction` | only zero vs non-zero matters (no physical materials) |
| `MovementSpeedModifier` (Pawn+0x364) | `movement.movement_speed_modifier` | |
| `AirSpeed` (Pawn+0x344) | `movement.air_speed` | class default; the run-time value comes from the script layer (`PawnHooks::air_speed`, the gun's 2000) |
| `PhysicsVolume.FluidFriction` (+0x2AC) | `movement.fluid_friction` | flying drag `0.5 ×` this |

`movement.gravity_z`, `movement.braking_deceleration` and `movement.max_fall_speed` are read only by the
placeholder model.

### Stand-ins, scope limits and known deviations of the port

- **Script.** With the original parameters the script layer supplies acceleration (A-WK-1, now CONFIRMED by
  ABILITIES.md), the jump (`DoJump`, A-JP-2) and the `Landed` event (§5) — see "Pawn script layer". Without
  it (raw path) the stand-ins remain: acceleration = `AccelRate · Normal(input)`, jump = `Velocity.Z = JumpZ`
  and Physics = Falling, walking only. In both, `MayFall` leaves the fall permission set (STRONG for a
  running player, spec 3.4) and `NotifyHitWall` / `HitWall` / `NotifyFallingHitWall` / `NotifyJumpApex` are
  no-ops, so `bJustTeleported` is never set and hit notifications never change velocity or mode. The grapple
  gun and the rocket boots are ported in the script layer (they switch the mode to flying/falling and write
  velocities between physics updates).
- **World mapping.** Every collision surface is static world geometry of one actor (like BSP): step-up-able,
  a valid base, never forcing floor re-traces, no physical material. One implicit physics volume: no zone
  velocity, no water, no gravity volumes.
- **Not ported:** crouch / walk-slowly and `CheckForLedges` (its output is UNKNOWN in the spec), the
  `processLanded` sanity trace / `FindSpot` / random kick, the UDK stuck-falling nudge (5 s / 10 s), pawn
  rotation and lean, the physics modes other than walking, falling and flying.
- **Collision contact.** The original's pull-back inside line checks is UNKNOWN (spec 9.5 q7). The port's
  `MoveActor` pulls the hit time back by `CONTACT_SKIN` (0.05 uu) along the move — a numerical choice, not a
  game value — and inflates the sweep radius by 1.001 (STRONG). Floor probes are exact. Standing height is the
  native 2.15 uu hover above the floor (the placeholder model stands 0.05 uu above it).
- **Interpretations marked TENTATIVE in code:** the old normal passed to `TwoWallAdjust` inside `stepUp` (the
  horizontalised first normal) and the steep-slope "wall also hit" flag that reverts a walking step.
- **Spec gaps resolved by choice (TENTATIVE):** after a V-crease ("ditch") landing whose last move hit nothing,
  the floor normal handed to `processLanded` is the second wall's normal (the spec does not say which hit
  result is used; the next walking sub-step re-traces the floor anyway).
- **Contract-violating collision results** (NaN or out-of-range hit times, non-finite or clearly non-unit
  normals) are repaired and non-finite moves refused, so a broken `CollisionWorld` cannot make the state
  non-finite (robustness only; conforming worlds such as `BoxWorld`/`SlopeWorld` are unaffected bit for bit).
- **Hidden state.** `PlayerState::pawn` (floor normal, based, force-floor-check) is not part of the trace
  schema; a replay started from a recorded sample begins unbased, so its first walking tick snaps the hover to
  2.15 uu.
- **Frame length.** `sim::step`/`step_with` still clamp `dt` to `MAX_STEP_DT` (0.25 s) before the model runs;
  the native 8-sub-step budget drops time only for frames above about 0.4 s (or after several mode changes in
  one frame), which the clamp mostly prevents (call `Ue3PawnMovement::advance_with_stats` directly to study
  long frames).
- **Rounding.** The port uses IEEE `f32` in the spec's operation order; the original's vector normalisation and
  division helpers may round differently in the last bit (UNKNOWN).
- **Rope-grapple bridge (debug only, not original).** The placeholder rope grapple of the raw pipeline adds its
  pull to the falling acceleration after the air limiter (so it is doubled by the refinement like gravity);
  while walking it lifts the pawn into falling when it beats gravity, otherwise its horizontal part is added
  after `CalcVelocity`. The original grapple never uses this path (its pull writes velocity in the gun's tick).
- **World surfaces.** Collision primitives carry the UE3 actor class, tag and id of the actor they stand for
  (`asamu_player::world::Surface`): the grapple gun reads them (interfaces, top/bottom-only and
  `grappleInteractable` tags, `InterpActor` following) and landings report `NotLandable`.

### Consequences to check against original traces (arithmetic consequences of the spec)

- Effective gravity `2 × world_gravity_z × custom_gravity_scaling` (−1040 uu/s² with the config value); a jump
  of `JumpZ` peaks after `round(JumpZ / (2·|g|·h))` ticks of length `h` at `JumpZ·n·h + g·h²·n²`
  (≈ `JumpZ²/(4·|g|)`), and lands with vertical speed `JumpZ + (2n − 1)·g·h` in tick `n`.
- Walking pawns hover 2.15 uu above the floor; steps up to about `MaxStepHeight + 4.15` uu are climbable.
- Ground speed reaches `GroundSpeed` after `GroundSpeed / (AccelRate·dt)` ticks regardless of analog input;
  landing clamps horizontal speed to `GroundSpeed` on the first walking sub-step.
- Air steering adds `2·AccelRate·AirControl·dt` per tick when the limiter is on (`2·AccelRate·dt` when off).
- Hitting a ceiling while rising refines the vertical velocity from the shortened displacement
  (`2·avg.z − V_old.z`), so the pawn moves *down* at once (e.g. −160 uu/s after rising only 2 uu into a ceiling
  from `Vz = 400` in a 1/60 s tick); horizontal speed is kept.
- After a landing, the rest of the tick walks with the *air-limited* acceleration, only normalised: if the
  air-control wall probe cut air control (wall within one tick of travel) while `10 ≤ speed < GroundSpeed`,
  that acceleration is zero and the landing tick brakes even with input held.
- A landing found only after a slide (e.g. falling along a wall onto the floor) carries no time: the pawn
  rests at the collision contact distance, not at the 2.15 uu hover, until the next tick.
- A walking pawn whose move is exactly zero and whose floor trace finds nothing starts falling only on the
  next tick (`|Delta| = 0` carries no time); a nearly-zero but non-zero move carries exactly one sub-step
  (≤ 0.05 s) into falling.
- With `SlopeBoostFriction = 0`, falling into an unwalkable slope slides *up* it without a height clamp and the
  refinement then launches the pawn upwards ("slope boosting").
- Braking has no guard against `2·GroundFriction·h > 1`: with huge friction the 0.03 s pieces oscillate in
  sign and the average can keep most of the speed (friction 40, 0.06 s frame: 98 %).
- The refinement re-derives velocity from an `f32` displacement, so slow sub-steps far from the origin carry
  the position's rounding (≈ 2·10⁻³ uu/s on a 0.07 uu step at z = 1000).

Tests: `crates/asamu-player/tests/ue3_movement.rs` (closed-form braking curve, acceleration timing, jump apex,
landing tick and impact speed, free fall and sub-step sequences for long frames, dropped time, ignored slices,
hover band and floor-trace skip, step-up limits, multi-stair step-up, walkable ramp, slope-slide thresholds,
landing threshold at `WalkableFloorZ`, V-crease landing and step revert, ledge and landing time carry,
air-control limiter on/off, low-speed boost, `BoundSpeed` creep, wall probe, terminal velocity, bit-identical
runs, fuzzed worlds/parameters/inputs/`dt` without NaN and with exact time accounting) and unit tests in
`ue3_movement.rs`; `asamu-game` tests the model selection.

`crates/asamu-player/tests/ue3_spec_conformance.rs` pins individual spec rules the scenario tests do not
distinguish, each against an expected value derived from the spec's formulas: the refinement condition
(skipped when a falling pawn does not speed up downwards), refinement from displacement at ceilings, walls and
in a vertical corner (`TwoWallAdjust` crease), `TerminalVelocity` applied after the move, every time-carry
branch (slide landing → 0, wall-shortened step over a pit → `step·(1 − frac)`, `|Delta| = 0` → 0, nearly-zero
move → one sub-step, budget shared by walking and falling), the air-clamped acceleration kept through a
landing, `CalcVelocity` once per call with the whole `dt`, `MaxSpeedModifier` on the cap only, the
penetrating floor-trace guard, the steep-floor slide (`0.1·N` + projected drop), the slope slide with the whole
`dt` per sub-step, wall slide and obtuse/right-angle corners inside `stepUp`, slope boosting, gravity scaling,
oscillating braking, snapshot/resume bit-identity and a contract-violating ("garbage") collision world.

### Verification pass (2026-10-09)

An adversarial line-by-line review of `ue3_movement.rs` against `NATIVE_PHYSICS.md` (sections 1.3–1.4, 2.1,
2.3, 3.1–3.5, 4.1–4.8, 5.2, the constants table and every address cited in code) found the port faithful to the
spec. Two defects were fixed: `stepUp` stepped down a second time when its repetition safety bound
(`MAX_STEP_UP_REPEATS`, not a game value) ran out (pathological inputs only), and a contract-violating collision
world could make the state non-finite (NaN hit times/normals were used unchecked). Each new conformance test was
checked against a deliberately injected deviation (22 mutations, e.g. always/never refining, refining from
velocity instead of displacement, slide landings carrying time, `frac` forced to 1, slope slide with the
sub-step `dt`, clamping terminal velocity before the move, landing restoring the unclamped acceleration,
skipping the `stepUp` `TwoWallAdjust` retry); all but one are caught. The exception is the landing-velocity
rule (`step·Time > 0.003` and `Time > 0.1`, spec 4.4): it is implemented but cannot be observed here, because
the port's `MoveActor` places the pawn exactly at `Delta·Time`, so the re-derived landing velocity equals the
semi-implicit one up to rounding.
Still open because they depend on script, not on the native spec: if the controller's apex-notify flag
(Controller+0x270 bit 5) is set when jumping, the sub-step in which `Velocity.Z` reaches ≤ 0 sets
`bJustTeleported` and skips the refinement (spec 4.3/4.6), lowering the apex velocity change of that one
sub-step from `2·g·h` to `g·h`; a `NotifyFallingHitWall` that changes velocity does the same (spec 4.10). The
port models neither (UNKNOWN until the ASAMU/UTGame jump and wall-hit script is read).

## Pawn script layer (`crates/asamu-player/src/pawn.rs`)

A reimplementation of the ASAMU pawn/controller/power-jump script (`asamu.ASAMUPawn`,
`asamu.ASAMUPlayerController`, `asamu.ASAMUPlayerInput`, `asamu.ASAMUPowerJump`) from the behavioural spec
`docs/reverse-engineering/ABILITIES.md`, in our own code, driving the grapple gun and the rocket boots (sections
below). Its run-time state is `PlayerState::script` (`PawnScript`: `GroundSpeed`, `AirControl`, `JumpZ`,
`AirSpeed`, the pawn's script state with latent sleeps, sprint flags, move-input lock, zoom, FOV, eye height and
walk bob, the power-jump actor, the grapple gun, the rocket boots).

Order inside one tick (ABILITIES.md §15, GRAPPLE.md G-TM-1/2): input events (sprint, jump release, jump
press flag plus the rocket boots' `RocketBoostKeyDown`, power-jump key, `use`, then the fire button's
`StopFire`/`StartFire`; `asamu_player::begin_step`, against the world as the previous tick left it; the camera's
cached point of view — the view as the previous tick ended — is recorded here and used by every aim of the tick)
→ map actors (`asamu-game` ticks movers, crystal state code and attractor pads here, after the attach's handler
calls reached them, and rebuilds the collision world; then `asamu_player::finish_step` applies the recharge
crystal's budget refill) → controller, by state: `PlayerWalking`
(acceleration from the pawn yaw **before** this tick's look update, zero under the move-input lock; look;
`DoJump` if the jump flag is set), `Grappling` while attached (zero acceleration; look; jump flag discarded) or
`ReleaseGrapple` for the one tick after a release (no move, no look, jump flag discarded) → pawn state code
(jump-release damping, zoom steps) → native physics (walking, falling or flying), with the pawn's `Landed`
event called inside `processLanded` so the rest of the landing tick already uses the new `GroundSpeed` →
`UpdateEyeHeight` → power-jump actor state code → rocket boots state code → grapple gun tick (crosshair, anchor
follow, pull and proximity release, released-flag reset, counter clamp, then its timers). Velocity writes of
the power jump, boots and gun are integrated by the next tick's physics. Latent `Sleep` wakes when the
remaining time is below half the tick's `dt` (G-TM-3); actor timers fire when their count strictly exceeds the
rate (G-TM-4); `GotoState` to another state clears the state's locals (G-TM-5).

Tests live in `crates/asamu-player/tests/pawn_script.rs` and (verification pass, marked †)
`crates/asamu-player/tests/ability_rules.rs`.

| Rule (ABILITIES.md) | Behaviour | Tests |
|---|---|---|
| A-WK-1…3 | Acceleration along the previous pawn yaw, analog magnitude ignored; `GroundSpeed` 440 walk / 880 sprint / 264 story | `a_wk_*`, `a_st_*` |
| A-WK-4 | Sprint state machine: press on the ground applies, press in the air (or on the grapple) only arms, release removes and disarms, a jump attempt removes and arms, a normal landing applies and disarms, story mode ignores the keys | `a_wk_4_*`, `a2_*`, `a_wk_4_sprint_press_while_attached_only_arms`† |
| A-JP-1 | The jump flag is consumed by the next controller move and never kept (no buffering before a landing); discarded while attached and in the tick after a grapple release | `a_jp_1_*`†, `g_rl_7_*`†, `a_il_1_*` |
| A-JP-2 | `DoJump`: side effects even on failure (sprint removed/armed, state `Jumped`); success only when walking; nothing at all in story mode | `a3_*`, `a_jp_2_*`, `a5_*` |
| A-JP-3 | Release in `Jumped` → `ReleasedJump`: `V.z × 0.7` now and every 0.1 s (latent, frame-quantised) while `V.z > 0.05`, then `FallingState`; the loop ends on landing, grapple attach (`Shooting`), grapple release (`Release`), power jump/leap (`FallingState`) and story entry; a Space tap while rising (failed jump) damps again | `a4_*`, `a_jp_3_*`, `a5_*`, `a_jp_3_*`†, `a5_tap_after_a_grapple_release_*`†, `a_pj_4_*`†, `g_ix_1_*`†, `s15_*`† |
| A-JP-4 | No double jump, dodge or crouch | `a5_*` |
| A-AC-1/2 | `AirControl` 0.3 until the first normal landing, then 0.35 (air steering `2·AccelRate·AirControl·dt` per tick); story-mode landings keep it | `a6_*`, `a_ac_2_*`† |
| §5 | Landing table: normal / story / `NotLandable` handlers; grapple budget refill, rocket boots re-arm or cancel, lock release, sprint-after-landing, `HasLanded`, `AirControl`, eye baseline reset below −200, hard landing below −2000 (cosmetic), sound and Kismet `SeqEvent_PlayerLanded` at ≤ −500 with cues below −1250 / −2500; **no falling damage, no landing slowdown, no death by impact**; `NotLandable` comes from the floor actor's tag | `landing_table_*`, `a7_*`, `fast_landing_*`, `g_ct_4_not_landable_*` (grapple_gun.rs), `a_rb_5_not_landable_*` (rocket_boots.rs) |
| §1 | Terminal velocity 10 000 (3-D clamp while falling) | `a7_*` |
| A-PJ-1…6 | Power jump: 0.6 s latent charge, release while walking → `V.z = 1600`; sprinting and moving → leap (horizontal × 2, `V.z = 750`, move-input lock +1); release early, in the air or while attached cancels; no cooldown; leap apex ≈ 270 uu | `a9_*`, `a10_*`, `a11_*`, `power_jump_*`, `a_pj_3_*`†, `a_pj_4_*`†, `a_pj_6_*`† |
| A-IL-1 | Move-input lock counter: leap +1, landing −1, grapple attach −1, floor 0 | `a_il_1_*`, `a10_*` |
| A-ST-1…3 | Story mode: enter (stop sprint, release grapple, 264, cancel power jump), jump/sprint ignored, fire never grapples, `use` reported, exit (440, `Idle`) | `a_st_*`, `a_st_1_*`†, `a_st_2_*`† |
| A-ST-4 | Zoom (story mode, power-jump key): FOV 90 → 50 in 19 steps of 0.016 s (one per tick at ≤ 62.5 Hz), held, 18 steps back on release; release mid-zoom reverses from the FOV reached; disabling the zoom zooms out; leaving story mode restores the FOV | `a20_*`, `zoom_*` |
| A-CM-1/2/3 | View location = position + `EyeHeight` + walk bob (also the grapple ray origin); pitch ±18 000 rotator units; FOV 90 from the settings | `walk_bob_*`, `a_cm_2_*`, `pawn_start_*` |
| A-CM-4 | Eye height: walking `max((EyeHeight − ΔZ)(1 − k) + 38k, −22)`, otherwise relax to 38, `k = min(0.9, 10·dt)`; walk bob ±`Bob·speed` lateral (`sin 8φ`), ±`0.75·Bob·speed` vertical (`sin 16φ`), phase rates 0.2 / 0.55 / 0.85 / 0.65; decay off the ground; ceiling probe | `a21_*`, `eye_height_*`, `ceiling_*`, `walk_bob_*`, `weapon_bob_*` |
| G-PH-1 / G-RL-7 (controller) | While attached no acceleration from the move axes; the tick after a release has no move and no look update | `g_ph_1_*`†, `g_rl_7_*`†, `a_st_1_*`† |
| §15 | Latent sleeps are frame-quantised (0.1 s = 6 ticks at 60 Hz, 3 at 30 Hz); actor timers fire when the count strictly exceeds the rate; scripted runs are bit-reproducible | `a_jp_3_damping_*`, `s15_*`†, `scripted_runs_*`, `grapple_gun_runs_*`†, `timers_fire_when_*` (pawn.rs unit) |

`asamu-game` exposes `enter_story_mode` / `exit_story_mode` / `toggle_story_mode` / `set_zoom_available` (the
Kismet actions' effects) and records the run-time FOV in traces. The app maps `DefaultInput.ini`'s keyboard
bindings to the abstract actions: W/S/A/D move, Space jump (press + release; the press also runs the rocket
boots' key handler, so Space in the air boosts), left shift sprint, left mouse fire (grapple: press fires,
release releases), right mouse power jump / zoom, E or Enter `use`, F7 quick load (respawn). Debug keys (not
original): F2 story mode, F3 grapple capacity 0/1/2/3/unlimited, F4 rocket boots on/off, F6 attractor pads.

## Grapple gun (`crates/asamu-player/src/grapple_gun.rs`)

The original `asamu.GrappleGun` (with the stock `Engine.Weapon` firing states it relies on), reimplemented from
`docs/reverse-engineering/GRAPPLE.md` in our own code; parameters `gun.*` above, script constants in the table
above. It replaces the placeholder rope grapple for the original parameter set (the rope model survives only in
the debug configuration without the script layer). Not a rope: attaching switches the pawn to `PHYS_Flying`
(no gravity, drag, speed capped at `AirSpeed` = 2000) and every gun tick adds `10⁷/d` uu/s² toward the anchor
after physics; below 200 uu the velocity is halved and the grapple released.

Tests: `crates/asamu-player/tests/grapple_gun.rs` (rule ids in the names), the adversarial verification pass 2 in
`grapple_verification.rs` (marked ‡), the controller/script interplay in `ability_rules.rs` and `pawn_script.rs`, the
flying physics in `ue3_spec_conformance.rs`, the game-level flows in `crates/asamu-game/tests/abilities.rs`.

| Rule (GRAPPLE.md) | Behaviour | Tests |
|---|---|---|
| G-IN-2 | Stock firing states: a press fires at once when `Active`; `WeaponFiring` refire checks every `FireInterval` (0.1 s), a re-press during it is served at the next check (T14) | `g_in_2_*`, `g_in_3_*` |
| G-IN-3 | Latch: one attempt per press; holding never re-fires; release sets the latch and runs the release | `g_in_3_*`, `a_st_2_*`† |
| G-IN-4 | `EnableGrapple(false)` only swallows the next press (T18) | `g_in_4_*` |
| G-IN-5 | Fire runs in the input event: the attach trace uses the previous tick's view, against the world as the previous tick left it (map actors tick afterwards) | `g_in_5_*`, `g_tm_2_the_fire_uses_the_input_world_*`‡, `input_events_run_before_the_map_actors_*`‡ (game), `graybox_mover_carries_the_anchor` (game) |
| G-TG-1 | Trace from the live view location (eye height + walk bob) along the camera's cached point of view (the view as the previous tick ended — also for the refire-served fire and the boost aim), `WeaponRange` 16 384 long; one impact (triggers transparent); no hit → the impact location is the trace end | `g_ac_5_*`, `g_ac_5_the_range_check_is_strict_*`‡, `g_tg_1_aims_during_the_actor_ticks_*`‡ |
| G-TG-2 | HUD crosshair: positive in range on a static mesh with grapples left and the tag rules (strict), negative out of range / out of grapples / `NotGrappleAble`, unchanged on other components | `g_tg_2_*` |
| G-AC-0 | Story mode/zoom: no grapple; an interactable within 200 uu of the pawn centre gets `InteractWith` (uses counted by the world, `SeqEvent_ActorInteractedWith`) | `g_ac_0_*`, `story_mode_fire_*` (game), `g_ac_0_interactables_*` (world) |
| G-AC-1 | Capacity ≤ 0 silent; attempted, out of grapples, no hit actor or `NotGrappleAble` → fail sound | `g_ac_1_*` |
| G-AC-2 | Attached / released this gun tick / hand hidden → consumed silently, press marked | `g_ac_2_*`, `g_ac_3_*` |
| G-AC-3 | Only a hide with visibility off blocks; animated: after 0.3 s (not cancelled by a show); leaving story mode shows the hand | `g_ac_3_*` |
| G-AC-4 | `TopOnlyGrappleAble` / `BottomOnlyGrappleAble` faces (normal Z ≥ 0.8 / ≤ −0.8), silent and before the press is marked (T13) | `g_ac_4_*` |
| G-AC-5 | Range: eye-to-hit < 5000 (T5), strict: exactly 5000 fails, the next float below attaches | `g_ac_5_*` (2 tests‡) |
| G-AC-6 | Any surface without tags is grapple-able, BSP included | `g_ac_6_*` |
| G-AT-1…6, G-AT-10 | Attach: budget +1, anchor at the hit, move-input lock −1, flying, `Grappling`, `Shooting`, velocity untouched (only drag and the 2000 cap in the attach frame, T4), `SeqEvent_PlayerGrappled` | `g_at_attach_effects`, `g_ph_3_*`, `a_il_1_*` |
| G-AT-3 | Grapple-able targets (interface) and `grappleInteractable`: handler calls, actor as Kismet originator | `g_at_3_and_g_at_7_*`, `g_rl_3_*`, `g_ct_4_charged_*` |
| G-AT-7/8 | The anchor follows `InterpActor`s only (movers, crystals, flowers, falling rocks), not `DynamicSMActor`s (floating rock, interactables) or static geometry; the helper keeps following after a release (T11, T12) | `g_at_3_and_g_at_7_*`, `graybox_mover_carries_the_anchor` (game) |
| G-PH-1 | Attached: zero acceleration, jump flag discarded, look works | `g_ph_1_*`†, `g_ix_1_*`† |
| G-PH-2 | Pull `dt·unit(A − P)·10⁷/d` after physics; `d` measured first; 2 000 / 10 000 / ≈ 50 000 uu/s² at 5000 / 1000 / 201 uu; still added for slices below 0.0003 s, when physics moves nothing | `g_ph_2_*` (2 tests‡), `slices_below_min_tick_time_*`‡ |
| G-PH-3 | `physFlying`: one update, drag `(1 − 0.15·dt)²`, 2000 cap on the 3-D magnitude (direction kept), no gravity, slide/step-up, velocity from displacement, no landing | `g_ph_3_*`, `g_ph_3_the_cap_limits_the_3d_magnitude_*`‡, `flying_*` (conformance) |
| G-PH-4 | `AirSpeed` := `fGrappleAccel` at the gun's spawn and after every release | `g_ph_4_*` |
| G-PH-5 | Model numbers: release after 0.52 / 1.42 / 2.95 s from 1000 / 2500 / 4999 uu, speed after halving 1435–1460 (60 fps), 1210–1220 (120), 1945–2005 (30) (T1, T2); attached speed is a sawtooth above 2000; sideways speed is replaced, no pendulum (T6) | `g_ph_5_*` (3 tests) |
| G-PH-6 | Blocked pull stays attached and pressed (T7) | `g_ph_6_*` |
| G-PH-7 | No gravity, no terminal-velocity clamp while attached; the first falling tick after the release adds the effective gravity | `g_ph_3_*`, `g_ph_7_*`‡ |
| G-RL-1 | Release: Falling, `Release`, one `ReleaseGrapple` tick, `SeqEvent_PlayerReleasedGrapple`, `UnGrappled`; velocity and budget untouched; ignored when not attached | `g_rl_1_*`, `a_jp_3_grapple_release_*`†, `g_rl_7_*`† |
| G-RL-2 | Proximity `d < 200` (strict: exactly 200 pulls and stays attached): pull, then velocity halved, release; an attach inside 200 releases in the attach tick (`(0 + pull)/2`, events `PlayerGrappled` then `PlayerReleasedGrapple`); suspended while the boots are `Boosting`, even inside 200; at `d = 0` the pull is skipped (guard) and the release still happens | `g_rl_2_*` (3 tests, 2‡), `g_ph_5_*`, `g_rl_6_the_proximity_release_is_suspended_*`‡, `a_pawn_exactly_at_the_anchor_*`‡ |
| G-RL-3 | Release-instant targets: no pull, 0.05 s timer (strict: 3rd tick at 60 Hz, 2nd at 20 Hz where `dt` = 0.05 exactly, the attach tick itself at 15 Hz and at the 0.25 s step bound) (T9, T10) | `g_rl_3_*` (2 tests, 1‡), `the_largest_dt_is_clamped_*`‡, `graybox_crystal_*` (game) |
| G-RL-4 | Button release | `g_rl_1_*`, `g_in_2_*` |
| G-RL-5 | Death and story entry release (T17) | `kill_z_while_attached_*`, `respawn_while_attached_*` (game), `a_st_1_*`† |
| G-RL-6 | No cooldown; the released flag clears at the next gun tick | `g_rl_6_*`, `g_ph_4_*` |
| G-RL-7 | One controller tick without move and look after any release (T19); a Space press in that tick still starts a boost (input event) but makes no jump attempt, so its release damps nothing | `g_rl_7_*`†, `g_rl_7_a_boost_started_in_the_release_gap_*`‡ |
| G-MO-1/2 | No release impulse; the uncapped post-pull velocity is handed to falling (T3) | `g_ph_5_attached_speed_*`, `g_rl_1_*` |
| G-CT-1/2 | Capacity/used; `SetMaxGrapples(n ≤ −1)` → 32 767; `UnlimitedGrapples`; initial capacity 0, latch set | `g_ct_2_*` |
| G-CT-4 | Refills: any landing (both handlers) except on `NotLandable`; a charged crystal in the attach tick; an uncharged crystal costs one (T8, T9) | `g_rl_6_*`, `g_ct_4_*` (2 tests), `graybox_crystal_*` (game) |
| G-CT-6 | Counter clamped to [0, 3] each gun tick: capacity ≥ 4 is unlimited. The clamp runs before the gun's timers, so a refire-served attach leaves the count at 4 until the next gun tick (as in the original); no press is evaluated in between | `g_ct_2_*`, `random_play_keeps_every_gun_and_boots_invariant`‡ |
| G-IX-1 | Space while attached does nothing | `g_ix_1_*`† |
| G-IX-2 | Space after a release: failed jump (`Jumped`) and a boost, both run | `g_ix_2_*`, `a5_tap_after_*`† |
| G-IX-3 | Boost then grapple: pull suspended (also during the charge, whose absolute writes continue while flying), 2000 cap on the boost, boost ends strictly beyond 5000 uu measured by the gun's previous tick (attaching does not refresh it, so the attach tick sees the previous grapple's value), pull resumes (T15) | `g_ix_3_*` (3 tests, 1‡), `g_rl_6_the_proximity_release_is_suspended_*`‡ |
| G-IX-4 | Boost key while attached: nothing | `g_ix_4_*` |
| G-IX-5/7 | Power-jump release while attached cancels; a charge started while attached and held through the release and the landing fires on a key-up while walking; a grapple during the power jump's rise only drags (below the cap) and pulls; sprint while attached only arms; a power-jump key-up processed before a fire press in the same tick (walking at the key event) runs the leap after the attach: its jump attempt fails, its velocity writes (×2, `V.z` 750) and its lock still apply while flying | `a_pj_3_*`†, `a_wk_4_*`†, `g_ix_5_a_power_jump_charged_while_attached_*`‡, `power_leap_released_in_the_attach_tick_*`‡ |
| G-AT-5 / A-IL-1 | A grapple ends a power leap's move-input lock; air control returns after the release (and its one-tick gap) | `g_at_5_a_grapple_ends_the_power_leap_lock_*`‡ |
| §14 | Kismet events as `SimEvent`s: `PlayerGrappled` (originator), `PlayerReleasedGrapple`, `PlayerLanded`, `PlayerRocketBoosted` (0/1), `InteractWith`; handler calls `ActorGrappled` / `ActorUngrappled` | the tests above, `log_keeps_order_*` (events.rs unit) |
| §15 | Determinism at 30 / 60 / 144 Hz; the serialised state is complete (a JSON round trip at any tick continues bit-identically); hostile `dt` (non-finite, ≤ 0: no-op; > 0.25: clamped, bit-identical to 0.25), non-finite input and poisoned state never spread NaN; fuzzed play keeps flying ⇔ attached, never walking and flying, bounded attached speed | `gun_runs_are_bit_reproducible_*`, `graybox_gap_is_crossed_*` (game), `the_whole_script_state_round_trips_*`‡, `the_largest_dt_is_clamped_*`‡, `hostile_input_*`‡, `random_play_keeps_*`‡ |

## Rocket boots (`crates/asamu-player/src/rocket_boots.rs`)

The original `asamu.ASAMURocketBoots` from ABILITIES.md §7: `Ready` / `Boosting` / `Unavailable` /
`UnavailableAndPlayedSound`, the jump key's press runs the boost handler in the input event, the `Boosting`
code is a latent timeline (after the power-jump actor) with absolute velocity writes. Parameters `boots.*`,
constants in the script-constant table. Tests: `crates/asamu-player/tests/rocket_boots.rs`.

| Rule (ABILITIES.md) | Behaviour | Tests |
|---|---|---|
| A-RB-1 | `bEnabled` starts false; Kismet (`enable_rocket_boots`) enables | `a_rb_1_*` |
| A-RB-2 | A press starts a boost only while falling, enabled, not in story mode; on the ground the jump wins; attached: nothing | `a_rb_2_*`, `g_ix_4_*` |
| A-RB-3/4 | Charge writes ×1.0/0.8/0.6/0.4/0.2 every 0.1 s (exactly five), 0.6 s gap, 41 boost writes every 0.05 s: `â·2500·(1 + s)/2 + R(â)·(s, 1000·s·sin θ, 1000·s·cos θ)`, θ one turn per second, `â` the camera's cached view (the view as the previous tick ended); Kismet outputs 0 and 1; lock +1 only if 0, −1 at the end (A12) | `a_rb_3_4_*`, `g_tg_1_aims_during_the_actor_ticks_*`‡ (grapple_verification.rs) |
| A-RB-5 | One boost per airtime; exhausted sound; any landing except `NotLandable` re-arms (A14) | `a_rb_5_*` (2 tests) |
| A-RB-6 | Landing during a boost cancels; the landing releases the lock (A15) | `a_rb_6_*` |
| A-RB-7 | Grapple interplay | `g_ix_3_*`, `g_ix_4_*` (grapple_gun.rs) |
| A-RB-8 | Jump-release damping runs beside the boost (A13) | `a_rb_8_*`, `g_ix_2_*` |
| A-IL-1 | A boost during a leap adds no lock; its end releases the leap's lock (A16) | `a_il_1_boost_*` |
| A-DT-2 | Respawn resets enabled boots to `Ready` without the boost end's lock release: a death during a boost keeps the move-input lock until the next landing | `boots_reset_*`, `respawn_releases_*` (game), `a_death_during_a_boost_keeps_*`‡ (game) |

## World objects and ability state (`crates/asamu-world`, `crates/asamu-game`)

`asamu_world::objects` holds the run-time state machines of the grapple-reactive and scripted map actors;
`asamu-game` ticks them between the player's input events and the rest of its tick (`begin_step` / `finish_step`,
G-TM-2), rebuilds the collision world from them (mover positions, crystal charge → `Surface`), and feeds the gun's
handler calls back to them as they happen (an attach in the input event reaches a crystal before it ticks).

| Object (spec) | Behaviour | Tests |
|---|---|---|
| `ASAMURechargeCrystal` (G-WO-1) | `Charged` → `GrappledState` → (`UnGrappled`) `UnCharged`: 21 one-frame fade steps of 0.005 s, then `RechargeDelay` (10 s) → `Charged` if `bShouldRecharge`; a child uncharges its linked crystal (parent flag not required) and its siblings, a parent its children; uncharging an uncharged crystal restarts its fade and whole delay; grappling an uncharged crystal changes nothing — also in the tick its recharge completes, because the press runs before the map actors | `g_wo_1_*` (5 tests, 2‡, world), `graybox_crystal_*`, `input_events_run_before_the_map_actors_*`‡ (game) |
| `ASAMUGlowFlower` (G-WO-2) | Release-instant grapple target costing a grapple (glow not modelled) | `g_rl_3_*` |
| Movers / `InterpActor` (G-WO-6) | Move after the input events, before the controller; the anchor rides them, including their motion in the attach tick (our test path) | `movers_move_*` (world), `graybox_mover_*` (game) |
| `ASAMUInteractable_Actor` (G-AC-0) | Uses counted against `MaxInteractTimes` (1; 0 = unlimited) | `g_ac_0_interactables_*` (world), `story_mode_fire_*` (game) |
| `ASAMUTelePad_Attractor` (ABILITIES.md §13) | Activated by Kismet; every 0.05 s forever `V += −(t/D)·unit(P − pad)·((R/d)·S·(1 − b) + S·b)`, `t` += 0.05 up to `D`; placed values `Range` 1000, `Strength` 200, `velocityBaseAmount` 0.05 | `attractor_*` (world, game) |
| Level abilities (KISMET.md, LEVELS.md) | `LevelAbilities` applied at level start (`SetMaxGrapples`, `EnableRocketBoots`); graybox test configuration: 3 grapples, boots on; `ORIGINAL_ABILITY_ACTIONS` per map (grapple first in ParadiseCave, boots first in StarHaven) | `level_start_applies_*` (game), `kismet_table_*`, `graybox_is_a_test_configuration_*` (world) |

## Gameplay verification pass 2 (2026-10-10)

An adversarial re-check of the grapple gun, power jump, rocket boots and grapple-reactive world objects, rule by
rule against `GRAPPLE.md` / `ABILITIES.md` and the CDO values in `docs/reverse-engineering/data/defaults/`
(`GrappleGun`, `ASAMURocketBoots`, `ASAMUPowerJump`, `ASAMURechargeCrystal`, `ASAMUTelePad_Attractor`,
`PhysicsVolume`/`DefaultPhysicsVolume`: every value used by the code matches). Doubtful points were re-read in the
locally extracted script text (never copied; behaviour restated in our own words above) and, for the frame order,
in the executable (`objdump` of `UWorld::Tick`, local only).

Fixed:

| Finding | Evidence | Fix |
|---|---|---|
| `asamu-game` ticked the map actors (movers, crystals, attractors) **before** the input events, while the spec's frame order (and the gun's own crystal-refill model) has input events first. Consequences: the fire trace saw movers one tick ahead, and a press in the tick a crystal recharged already found it charged (refill instead of a paid grapple). | GRAPPLE.md G-TM-2 / G-IN-5 (STRONG order); the PARITY claim of equivalence (deviation 6) did not hold at that boundary. | `asamu_player::begin_step` (input events against the previous tick's world) / `finish_step` (the rest against the updated world); `asamu-game` ticks the map actors in between and hands the attach's handler calls to the crystals before they tick. `step_with` = both halves with one world (unchanged behaviour for every other caller). |
| Aims taken after the controller's look update (boost aim at the first boost write, refire-served fire, HUD crosshair) used the post-update view; the original reads the camera's cached point of view, updated once per frame **after all actor tick groups**. | `UWorld::Tick` calls the camera's `UpdateCamera` event in its controller loop after the last `TickActors` group — CONFIRMED (native, disassembly); `GetAdjustedAim` → `GetBaseAimRotation` → `GetPlayerViewPoint` → cached POV — CONFIRMED (src); trace origin = live `GetPawnViewLocation` (`UTPawn` override) — CONFIRMED (src). | `PawnScript::pov_yaw`/`pov_pitch` recorded when a tick begins; `PlayerState::aim_direction` used by the fire trace, crosshair and boost aim. A press in the input event is unaffected (no look update has run yet). |

Confirmed without change (selection; each pinned by a ‡ test): pull `dt·10⁷/d` with `d` measured before the pull and
the strict `d < 200` halving release in the same branch as the pull (both suspended while `Boosting` — including the
charge — or while the instant timer is pending); the 3-D 2000 cap of `physFlying`; strict range `< 5000` from the live
eye and the trace end at 16 384; strict 0.05 s instant timer; boost break strictly `> 5000` on the gun's previous
measurement (not refreshed by an attach); leap writes unconditional after a failed jump attempt; the reset after a
death sends enabled boots to `Ready` without releasing the boost's input lock; crystal family links ignore the
linked crystal's parent flag; re-entering `UnCharged` restarts fade and delay.

Mutation check: 14 deliberate faults were each caught by at least one new test — non-strict comparisons at the
200 uu release, the 0.05 s timer, the 5000 uu range and the 5000 uu boost break; an attach refreshing the measured
distance; a jump attempt in the release-gap tick; leap writes only after a successful jump; no crystal restart on
re-entering `UnCharged`; map actors before the input events; aims without the camera cache; the proximity release
not suspended while boosting; no pull in the release tick; the flying friction without its 0.5 factor; a skewed aim
preview.

Residual gaps (not modelled, see the deviation table): equip delay (1), trigger impacts and camera-animation aim
offsets (2), cinematic/players-only gating (3), Kismet runtime and per-map start state (4), mover rotation basing and
Matinee paths (5), falling rocks (6), worm push and `ResetPlayer` (11), the 0.3 s death fade (12), the key-event
order inside one tick (16), and trace-schema fields for the script state (21).
