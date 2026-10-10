# Gameplay visual effects and decals

Scope: the visual side of the grapple (beam, hit decal, hand lights), the speed-line cone, the HUD crosshair
states, the rocket-boots lens effect, the power-jump charge light, the hard-landing decal, and the level decals
(`DecalActor`, `DecalActorMovable`, the checkpoint markers' decal): what the original does, the decal data format,
the importer output and how the runtime draws it. Companion documents: [`GRAPPLE.md`](GRAPPLE.md) (rules G-…),
[`ABILITIES.md`](ABILITIES.md), [`MATERIALS.md`](MATERIALS.md), [`LEVEL_FORMAT.md`](LEVEL_FORMAT.md),
[`LIGHTMAPS.md`](LIGHTMAPS.md), [`KISMET.md`](KISMET.md).

**Publication rules followed.** Behaviour was derived by reading, locally only, the UnrealScript source text shipped
in the cooked packages, class default objects (CDOs), content objects (particle systems, materials) and the
unstripped Mac executable (Ghidra output under the ignored `research/`). Nothing is quoted or transcribed; names of
classes, functions, properties and parameters and numeric values are published as facts with their source.

Evidence source: `Startup.upk`, `Engine.u` and the 12 map packages under
`~/Library/Application Support/Steam/steamapps/common/A Story About My Uncle/A Story About My Uncle.app/Contents/Resources/ASAMU/CookedMac`
(Steam build 1822049), read-only.

| Label | Meaning here |
|---|---|
| **CONFIRMED (src)** | read in the shipped script text of the class named |
| **CONFIRMED (cdo)** | class default object value decoded by `asamu-inspect defaults` / `props` |
| **CONFIRMED (content)** | tagged property of a content object (particle module, material) decoded by `asamu-inspect props` or `materials.json` |
| **CONFIRMED (config)** | value in the shipped `.ini` files |
| **CONFIRMED (native)** | read in the executable (function and address given) |
| **(T)** | asserted against the install by `crates/asamu-ue3/tests/decal_real_data.rs` |

Reproduce:

```sh
cargo test -p asamu-ue3 --test decal                                   # synthetic layout, hostile input, frame math
cargo test -p asamu-ue3 --test decal_synthetic                         # extraction end to end on a synthetic map
cargo test -p asamu-ue3 --test decal_real_data -- --nocapture          # every (T) claim; skips without the install
cargo run -p asamu-import -- decals --check                            # coverage table, writes nothing
cargo run -p asamu-import -- --out <user-local dir> decals             # <out>/decals/: <map>.decals.json, masks/, manifest.json
cargo test -p asamu-import decals                                      # document shape, decal masks
cargo test -p asamu vfx                                                 # beam, cone, lights, crosshair, decal pool math
asamu-inspect defaults <CookedMac>/Startup.upk asamu.GrappleGun --inherited   # CDO values quoted below
asamu-inspect props <CookedMac>/Startup.upk AdventureSuitEffects.ParticleSystems.GrappleBeam.<module>   # content values
asamu-inspect decompress <CookedMac>/RefShaderCache-PC-OpenGL.upk --out research/local/<dir>/gl.dec   # GLSL text (§8.4)
```

Native functions were decompiled locally with Ghidra (project and output under the ignored `research/`):
`UDecalComponent::Serialize`, `UpdateOrthoPlanes`, `CaptureDecalState`, `UpdateTransform` by name with
`tools/ghidra-scripts/DecompileToLocal.java`, and the two `operator<<` overloads by address (0x1003429C0,
0x10033FC50) with a throwaway by-address variant of that script. The verification pass added, the same way,
`UDecalComponent::Attach` @ 0x100338660, `BeginPlay` @ 0x10033CEF0, `ComputeReceivers` @ 0x100338800,
`AttachReceiver` @ 0x10033C560, `UStaticMeshComponent::GenerateDecalRenderData` @ 0x100C80AE0 and
`ADecalManager::TickSpecial` @ 0x10033F630 (names list `UDecalComponent::*`, `ADecalManager::*`).

**Verification pass (2026-10-10).** The class default, config and content values the runtime and the importer
rely on (gun, cone, pawn landing, hand lights, anchor light, lens effect, HUD, Kismet actions, decal templates,
the beam's and the lens effect's particle modules, the effect materials) were re-read with `asamu-inspect`; the
behaviour of §2–§8.6 was re-read in the shipped script text; and the 469 decal components were decoded a second
time by an independent Python reader (own tag walker and native-tail reader over the earlier verifier's package
reader; scratch, deleted): counts, owner classes, receivers, vertices, light map kinds and the stored hit frames
agree. Names of effects that belong to other workstreams (particle systems, sounds, camera animations) were not
re-checked. What that pass changed is marked **(V)**: decals on mirrored owners (§8.4), the receivers of movable
decals (§8.5), mirrored receivers, the triangle source, a BSP receiver, decal masks (§9, §10), and the beam's
emitter order (§2).

## Most important findings

| # | Finding | Confidence |
|---|---|---|
| 1 | **No light is spawned at the grapple anchor.** `DecalDynamicLight` exists (a `PointLightMovable` that fades in at 5/s to brightness 10, then out over 5 s) but no map places one, no script spawns one, and the gun's only reference (`lightTestThingy`) is never assigned (its archetype declaration is commented out). The glow at the anchor is the hit decal's emissive material. | STRONG (src, cdo, map census) |
| 2 | The hit decal goes through the stock decal manager with **at most 5 live decals** (`fHitDecalLimit` → `MaxActiveDecals`) and a **30 s lifetime** (`DecalLifeSpan`, `DefaultGame.ini`); `fDecalLifespan` (20) is never read. The hard-landing decal shares the same pool. | CONFIRMED (src, cdo, config) |
| 3 | The default beam, decal and hand-plate colour is `DefaultBeamColor` (0.04, 0.19, 0.79); `defaultdecalcolor` and `defaultplatecolor` are never read. | CONFIRMED (src, cdo) |
| 4 | The speed-line cone's material parameter is 0 below 1200 uu/s and `|V|/5000` above, faded out for near-vertical motion; it ticks before the gun, so while attached it reads the capped 2000 uu/s (0.4). | CONFIRMED (src, cdo); order STRONG (G-TM-2) |
| 5 | 469 decal components sit on placed actors in 7 maps: 370 `DecalActorMovable`, 1 `DecalActor` and **98 checkpoint markers** (`ASAMUCheckpointVisuals.checkpointDecal`). All decode with exact consumption and re-encode byte for byte. | CONFIRMED (T; independent Python decode) |
| 6 | Only the 99 decals of the `DecalActor` and the checkpoint markers carry cooked geometry (210 receivers). The 370 movable decals store none: the engine projects them at run time. Our projection (the receiver's collision triangles, back faces skipped, clipped to the decal box) **reproduces the cooked receivers**: equal area within 2 % on 209 of 210, 0.04 % in total, vertex-exact on 3 of the 4 receivers of the static `DecalActor`. | CONFIRMED (T) |
| 7 | The decal box is **not** scaled by the owner's `DrawScale` / `DrawScale3D`. A **mirrored owner** (negative `DrawScale3D` product) reverses the projection direction instead; one shipped decal is one. | CONFIRMED (T, cooked receivers; native, T for the mirror) **(V)** |
| 8 | The crosshair shows the suit variant whenever the gun ticks; `SeqAct_ChangeCrosshair` is in no shipped map. | CONFIRMED (src, Kismet census) |
| 9 | **A movable decal's receivers are computed when play begins**, from the world's collision hash inside the decal's bounds; the serialized `DecalReceivers` list is only what the editor had attached when the map was saved, and misses every receiver that comes after the decal in the actor list (197 receiver meshes on the shipped maps). | CONFIRMED (native) for the control flow; STRONG (T) for the contents **(V)** |
| 10 | 42 of the 69 decal materials (72 of the 469 decals: the cave paintings, runes and painted symbols) display **no texture**: a constant colour or glow, with the picture only in an opacity texture. Drawn through a one-texture material model they are solid patches; the importer bakes a mask for each. | CONFIRMED (content; seen in the app before and after) **(V)** |

## 1. Classes

| Class (`asamu.*`) | Super | Role |
|---|---|---|
| `GrappleGun` | `UDKWeapon` | owns the beam particle component, the decal and beam material instances, the light manager |
| `GrappleGunHitLocActor` | `Actor` | empty class; one is spawned by the gun (tag `HITLOCACTOR`). It is placed at the hit point and based on the hit actor when that is an `InterpActor`; the gun then copies its location into `vGrappleLocation` every tick (G-AT-7/8), which is both the pull anchor and the beam's end |
| `GrappleGunLightManager` | `Object` | drives the hand plate material's lamp parameters (§4) |
| `DecalDynamicLight` | `PointLightMovable` | unused (finding 1) |
| `ASAMUDoesNotAcceptGrappleDecal` | interface | marker: no hit decal. Implemented by `ASAMURechargeCrystal` and `ASAMUGlowFlower` (GRAPPLE.md §1) |
| `ASAMUVelocityCone` | `DynamicSMActor` | the speed-line cone (§3) |
| `RocketBootsCameraLensEffect` | `EmitterCameraLensEffectBase` | lens effect of a rocket boost (§7) |
| `SeqAct_ChangeCrosshair`, `SeqAct_ToggleCrosshair`, `SeqAct_SetVelocityConeMaterial` | `SequenceAction` | Kismet actions (§3, §5) |
| `ASAMUCheckpointVisuals` | `Actor` | checkpoint marker: a decal on the ground plus an effect mesh that flashes on activation (§8) |

## 2. Grapple beam

**Set-up** [CONFIRMED (src, cdo)]. The gun's `MyBeam` component (a `UDKParticleSystemComponent`, template
`AdventureSuitEffects.ParticleSystems.GrappleBeam`, foreground depth group, not auto-activated) is attached to the
hand mesh's socket `GrappleSocket` and given the hand mesh's FOV (70°). At start the gun creates a material instance
of `AdventureSuitEffects.GrappleBeam_inst` and hands it to the particle system through the material parameter
`beammaterial`; it sets the source tangent strength of emitter 0 to 1000.

**Update** [CONFIRMED (src)]. Every gun tick the vector parameter `GrappleBeamEnd` is set to `vGrappleLocation` (the
anchor, or the helper's location while following an `InterpActor`). The system is activated on attach and
deactivated on release; whether already-emitted beam particles linger after deactivation is UNKNOWN (the runtime
hides the beam at once).

**Content** [CONFIRMED (content)]. The system has five emitters: three beam emitters named `GrappleBeam` and two
sprite emitters. Only the first two (emitters 0 and 1 of the system's `Emitters` list) are enabled; the third beam
emitter and both sprite emitters have `bEnabled` false on their LOD level **(V)**. Per enabled beam emitter:
`ParticleModuleSize.StartSize` 4 (emitter 0) and 2 (emitter 1), constant — the beam widths in UU;
`ParticleModuleTypeDataBeam2`: 40 interpolation points, 2 beams at most, speed 0; `ParticleModuleBeamTarget`:
absolute target from the particle parameter `GrappleBeamEnd`, target tangent strength 20;
`ParticleModuleBeamSource`: tangent from the emitter, strength 1250 in the asset — the script's call changes
emitter 0 only, so the wide beam runs with 1000 and the narrow one with 1250 **(V)**; `ParticleModuleBeamNoise`:
frequency 20, range ±1 on the wide emitter (the narrow emitter's range table differs and is scaled 1.3;
distributions are not decoded here, TENTATIVE), speed 100, lock time 0.2 s, tessellation 5, tangent strength 35,
smooth. Material `GrappleBeam_inst`: additive, unlit, texture
`AdventureSuitEffects.BeamParticle.ThinBeam` through the UV window scale (1, 0.5) offset (0, 0.25), vector
parameter `GrappleBeamColor` = (0.044, 0.191, 0.796) — the CDO's `DefaultBeamColor` rounded.

**Colours** [CONFIRMED (src, cdo)] — `UpdateBeamVisuals`:

| Mode | Beam `GrappleBeamColor` | Decal `DecalColor` and plate `LightColor` | `DecalEmissiveMultiplier` |
|---|---|---|---|
| default | `DefaultBeamColor` (0.04, 0.19, 0.79) | same | 300 |
| custom (`bCustomBeamColor`) | `customBeamModeColor` | same | 50 |
| Midas (`bMidasMode`; wins over custom) | `midasBeamColor` (1.0, 0.95, 0.0) | same | 50 |
| goat (`bGoatMode`; wins for the beam only) | `DefaultBeamColor` (the beam is replaced by the "tongue" systems `Sigge.Particles.Goat_GrappleBeam` / `Goat_drool_ignite_PS` / `Goat_Tongue_Connect_PS`) | Midas or custom colour when those are on, else default | 50 with Midas/custom, else 300 |

When any mode is on the HUD crosshair is tinted with the decal colour. The custom colour is HSV(`BeamColor`
setting as hue in degrees, S 0.85, V 0.85) converted by the settings manager's own sector formula
(`ApplyBeamColor`, `HSVToRGB`). A mode is on only when its setting is on **and** enough collectibles were found:
beam colour ≥ 10, goat ≥ 15, Midas ≥ 20 (parkour ≥ 25) (`CheckIfUnlocked`). Settings defaults
(`DefaultSettings.ini`): `BeamColorActive` false, `BeamColor` 0, `SpeedlinesActive` true [CONFIRMED (config)].

**Other attach effects** [CONFIRMED (src, cdo)]: `grappleBeginEffect` (`Sigge.Particles.Beam_Ignite_PS`, at the
socket) is activated on attach; `ImpactParticleSystem` (`Sigge.Particles.Beam_Connect_PS`) is spawned through the
emitter pool at the hit location, oriented along the hit normal and attached to the hit actor. Particle systems are
the particle workstream's.

**Runtime** (`apps/asamu/src/vfx/beam.rs`, `vfx.rs`): two camera-facing ribbons 4 and 2 UU wide, each through 41
points of its own cubic Hermite curve (source tangent 1000 for the wide and 1250 for the narrow one along the view
direction, target tangent 20 along the chord — the tangent *directions* are TENTATIVE: the socket's own axis is
not exposed), displaced by 20 noise points of ±1 UU
re-randomised every 0.2 s, additive, textured with the converted beam material's texture and UV window, coloured by
the mode table. Deviations: the beam starts at a fixed eye-space offset instead of the hand socket and is drawn with
the world FOV (ours); brightness is ours.

## 3. Speed lines (`ASAMUVelocityCone`)

[CONFIRMED (src, cdo) unless noted] The pawn spawns one cone at start. It is a `DynamicSMActor` with static mesh
`ASAMUVelocityEffect.VelocityCone` (66 vertices, 64 triangles, bounds ±28.2 UU; section material
`ASAMUVelocityEffect.VelocityCone_Mat`) and no collision; at start it creates a material instance of slot 0.

Every tick:

1. the material instance's scalar `Velocity` (`matInstanceParamName`) is set from the pawn's velocity `V`:
   - `|V| < fadeMinVelocity` (1200): 0;
   - else `clamp(|V| / fadeMaxVelocity, 0, 1)` (`fadeMaxVelocity` 5000), multiplied by `1 − (|v̂.z| − 0.9) / 0.1`
     when `|v̂.z| ≥ 0.9` (motion within about 26° of vertical fades the cone out, to 0 at vertical);
2. the cone is moved to the player's view point plus `V · dt` and turned to face along `V`.

`ASAMUPawn.ToggleSpeedlines(bool)` hides or shows the cone actor; the settings manager calls it with the
`SpeedlinesActive` setting (default true). `SeqAct_SetVelocityConeMaterial` (property `mat`) replaces the cone's
material and re-creates the instance; it is used once each in AG-BeautifulCity, AG-StarHaven and AG-IceCave
(KISMET.md) with `ASAMUVelocityEffect.*` materials.

Materials [CONFIRMED (content, `materials.json`)]: `VelocityCone_Mat` is unlit and translucent with opacity = the
texture `ASAMUVelocityEffect.wind_df` (UV scale 5 × 5) times 0.1; the instances `VelocityConeMedium_INST` and
`VelocityConeLight_INST` set the scalar `max opacity` to 0.3 and 0.5. How `Velocity` enters the opacity is TENTATIVE
(taken as a multiplier).

Tick order (G-TM-2, STRONG): the cone is spawned by the pawn before the gun exists, so it ticks after the pawn's
physics and before the gun's pull: while attached it sees the capped 2000 uu/s → parameter 0.4.

**Runtime** (`vfx/cone.rs`): `cone_parameter`, `cone_rotation`, `cone_location` as above;
`velocity_seen_by_cone` removes the gun's pull increment (`dt · 10⁷/d` toward the anchor) from the post-tick
velocity so the parameter matches the original's tick order. The converted cone mesh is drawn at the interpolated
eye with an additive material (wind texture, UV scale and maximum opacity from the converted material's opacity
channel) whose alpha is `parameter × max opacity`; Kismet's material switch is honoured. Without converted data a
procedural cone of the same size stands in.

## 4. Hand lights (`GrappleGunLightManager`) and the unused anchor light

[CONFIRMED (src, cdo)] The manager replaces the hand's plate material (slot 0) with an instance and writes four
scalars every gun tick (`UpdateLights`, skipped in workshop mode, which no map enables): `grappleLight0`,
`grappleLight1`, `grappleLight2` (one lamp per grapple of the capacity, at most 3) and `powerJump`. The lamp values
are four floats that move at `fSmallLightGrowth` / `fSmallLightReduction` / `fBigLightReduction` (all 15 per
second) between `fOriginalColor` (never assigned: 0) and `fSmallLightStrength` (1.0):

- for each grapple used since the last refill a lamp goes down, from the highest lamp of the capacity (lamp 2 for
  capacities ≥ 3, lamp 1 for 2, lamp 0 for 1);
- lamps go up again only when the used count is 0 **and** the fire latch `bCanGrapple` is set — all lamps of the
  capacity at once (so the lamps relight after a landing or a crystal, once the button is released);
- the `powerJump` lamp goes up while `GrappleGun.bLightUp` is set — set when a power-jump charge completes,
  cleared when the jump fires or is cancelled — and down otherwise. Quirk: going up, this lamp adds its growth
  value **without the delta time** (15 in one tick), is clamped to 1 on the next tick, and so flashes for one
  frame;
- the same function clamps the used count to `[0, 3]` (G-CT-6).

When the charge completes the gun also activates `PowerJumpLightEffect` (`AdventureSuitEffects.ParticleSystems.
PowerJumpCharged`, attached to the hand socket `PowerJumpLight`); the jump itself spawns
`AdventureSuitEffects.ParticleSystems.PowerJumpTakeoff` at the pawn through the emitter pool. The controller's
`PowerJumpEffect` and `LightUpPowerJump` are empty (their bodies are commented out).

`DecalDynamicLight` [CONFIRMED (src, cdo)]: `fLifeTime` 5, `fFadeInRate` 5, `FadeIn` true, point light brightness
10. At start it stores the brightness and sets it to 0; while fading in it adds `dt × fFadeInRate` until the
brightness exceeds the stored value, then subtracts `stored / fLifeTime` per second and destroys itself after
`fLifeTime`. It is never instantiated (finding 1).

**Runtime** (`vfx/lights.rs`): `GrappleLights::update` ports `UpdateLights` (quirks included; tested);
`DecalLight` models the unused light and is not spawned. The lamps are drawn as four HUD dots (bottom right)
because they live on the hand's plate material, which the hand overlay does not parameterise yet.

## 5. Crosshair

[CONFIRMED (src, cdo)] The HUD keeps `bShowCrosshair` (default true), `bHUDStoryMode` and `CurrentCrosshair`
(`HIDDEN`, `DOT`, `DISABLED`, `ENABLED`, `STORY_DISABLED`, `STORY_ENABLED`; start value `DOT`) and tells the
Scaleform crosshair movie which frame label to play:

| Call | Effect |
|---|---|
| `ChangeCrosshair(enable, suit)` | ignored while not shown. Story mode: (false, true) → `story_normal`, (true, true) → `story_enabled`, anything else nothing. Otherwise: (false, true) → `normal` (disabled), (true, true) → `enabled`, (·, false) → `dot`. A frame is sent only when the look changes |
| `ToggleCrosshair(show, fade)` | with `fade`: plays `fadein_*` / `fadeout_*` (`_story` or `_normal`) unless already in that state, keeps the look; without: hiding sends `hidden`, then `ChangeCrosshair(show, show)` |
| `ToggleStoryMode(on)` (HUD) | sets the story flag, forgets the look, then `ChangeCrosshair(false, true)` |
| `ApplyCrossHairColor(enable, r, g, b)` | tints the crosshair (beam colour modes, §2) |

Callers: the gun's per-tick crosshair trace (G-TG-2) calls (true, true) or (false, true), or nothing for a
non-static-mesh hit in range; `EnableGrapple(false)` calls (false, false); the pawn's story state entry and exit
call `ToggleStoryMode`; Kismet's `SeqAct_ToggleCrosshair` (`Fade`) and the console command `ToggleCrosshair`
call `ToggleCrosshair`. `SeqAct_ChangeCrosshair` (`bEnableCrosshair`, `bSuitCrosshair`, `bShowCrosshair`, all
default true) calls `ChangeCrosshair(bEnableCrosshair, bSuitCrosshair)`; no map contains it.

The pause menu is a caller too **(V)** [CONFIRMED (src)]: opening it calls `ToggleCrosshair(false)` and closing it
`ToggleCrosshair(true)`, without a fade. So a pause and resume shows the crosshair again even where Kismet had
hidden it, and leaves the enabled look until the gun's next tick. Not ported: the HUD's "+" and the ring keep
their state across a pause (`kismet.rs` owns the HUD's switch).

Consequence: the dot look is only visible until the gun's first tick (and for one tick after
`EnableGrapple(false)`); a level without grapples shows the disabled suit crosshair. Whether the movie's `normal`
frame differs visibly from `dot` is UNKNOWN (the movie's art is not ported).

**Runtime** (`vfx/crosshair.rs`): `CrosshairState` ports the three functions; `vfx.rs` draws a ring around the
HUD's crosshair per look (sizes, colours and the 0.25 s fade are ours) and tints the enabled look with the beam
colour modes' tint.

## 6. Run-time decals: grapple hit and hard landing

[CONFIRMED (src, cdo, config)]

- At start the gun sets the decal manager's `MaxActiveDecals` to `fHitDecalLimit` (5) and its `DecalDepthBias` to
  −0.000001, and creates a material instance of `Decals.GrappleDecal_inst` (parent
  `Decals.DecalMaterials.GrappleDecal`: unlit, masked, texture `Decals.grapple_Decal`; parameters `DecalColor`
  and `DecalEmissiveMultiplier`, 0.36/0.9/1.0 and 200 in the instance, overwritten by §2's table).
- A successful attach on a target that does not implement `ASAMUDoesNotAcceptGrappleDecal` spawns a decal through
  `DecalManager.SpawnDecal`: location = hit location, orientation = the rotator of the negated hit normal, width
  and height `DecalWidth` / `DecalHeight` (80 / 80), thickness 50 (near plane −25, far plane +25), no-clip false,
  in-plane rotation random in [0, 360), receiver restricted to the hit component, depth bias −0.000001. The sound
  `GrappleDecalSound` plays at the hit either way.
- A hard landing (`V.z < −hardLandingThreshold`, 2000; not on `NotLandable` floors) spawns
  `Shared_Materials.HardLandDecalMaterial` (translucent: black with the alpha of
  `Shared_Materials.Textures.ImpactDecal_df`) at the pawn's location lowered by half its collision half-height (the
  script halves an already-halved height), oriented along the negated floor normal, 200 × 200 (`hardLandDecalSize`),
  thickness 100 (`hardLandDecalDepth`), random rotation, plus the camera animation, rumble and the particle system
  `AdventureSuitEffects.ParticleSystems.PowerJumpLand`.
- Decal manager (stock `Engine.DecalManager`): a spawn takes a pooled component, else creates one; with nothing
  pooled and `MaxActiveDecals` active the **oldest** is expired first (its component is reset and dropped, a new
  one made). Each decal lives `DecalLifeSpan` seconds — 30 (`[Engine.DecalManager] DecalLifeSpan=30.0` in
  `DefaultGame.ini`, over `BaseGame.ini`'s 10) since neither caller passes a lifetime — and then returns to the
  pool. `GrappleGun.fDecalLifespan` (20) has no reader.
- **(V)** The manager's tick (`ADecalManager::TickSpecial`) also finishes, at once, a decal that has no receiver
  [CONFIRMED (native)]. A run-time decal is not a static one, so it attaches only to primitives with
  `bAcceptsDynamicDecals` (true by default; the fog volumes' meshes, for one, turn it off). Spawning needs dynamic decals enabled (`DynamicDecals=True` in the desktop system settings; off only in
  the mobile bucket) [CONFIRMED (src, cdo, config)].

**Runtime** (`vfx/hit_decals.rs`): `DecalPool` ports the pool rule (5 active, 30 s on game time, cleared with a new
game); `DecalSpawn::grapple` / `hard_landing` carry the values above; a spawn that hits nothing takes no slot
(the original would drop it on the next tick, after evicting the oldest when five were live: not modelled). The
per-component `bAcceptsDynamicDecals` is not in the collision world and is not checked. Deviation: the original clips the decal onto
the receiver's triangles; the runtime's collision cannot enumerate triangles, so the decal is a 9 × 9 lattice of
rays cast along the projection direction between the near and far planes, meshed where neighbouring rays hit
surfaces facing the decal (exact on flat receivers, approximate at edges). The decal does not follow a moving
receiver. The grapple decal uses the converted decal texture tinted with the mode's decal colour at full
brightness; the hit normal is re-traced from the eye to the anchor (the tick report carries no normal).

## 7. Rocket-boots lens effect, other effects

[CONFIRMED (src, cdo, content)] When a boost's thrust phase begins (after the charge) the boots call the
controller's `RocketEffect`, which spawns the camera lens effect class `RocketBootsCameraLensEffect`: particle
system `Philip_Test.rocketBoots.P_RocketBootsEffect` held `DistFromCamera` = 20 UU in front of the camera (the
base class default is 90). The system is one local-space emitter that bursts 15 particles of material
`Philip_Test.M_FX_Smoke_01` living 0.3–0.5 s, once; the actor destroys itself when the system finishes. The boots
also activate their trail particle component for the boost's duration and play camera animations (not this
workstream).

**Runtime**: a full-screen overlay fading out over 0.5 s (the particles' longest lifetime); its opacity is ours.

## 8. Level decals

### 8.1 Census — CONFIRMED (T)

| Map | Decal components | `DecalActorMovable` | `DecalActor` | Checkpoint decals | Hidden at start | With cooked receivers | With geometry after import | Receiver meshes (outside the editor's list) | Decal triangles |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| AG-BeautifulCity | 37 | 27 | | 10 | 1 | 10 | 37 | 118 (41) | 11,500 |
| AG-Darkcave | 163 | 146 | 1 | 16 | 1 | 17 | 158 | 551 (131) | 58,374 |
| AG-IceCave | 130 | 103 | | 27 | 1 | 27 | 130 | 179 (8) | 8,586 |
| AG-ParadiseCave | 68 | 46 | | 22 | 1 | 22 | 67 | 161 (8) | 8,204 |
| AG-StarHaven | 69 | 46 | | 23 | 1 | 23 | 69 | 135 (6) | 7,654 |
| ASAMUFrontEndMap | 1 | 1 | | | | | 1 | 2 | 36 |
| TheCore | 1 | 1 | | | | | 1 | 4 (3) | 234 |
| **total** | **469** | **370** | **1** | **98** | **5** | **99** | **463** | **1,150 (197)** | **94,588** |

AG-Workshop, AG-Epilogue, ASAMUEntry, ASAMULegal and Freds_place have none. Every decal component belongs to an
actor listed in `ULevel::Actors` (no stray templates). 69 distinct decal materials are used (28 masked and 17
translucent `DecalMaterial`s, 23 masked instances, 1 unlit); all are in `materials.json`. No decal uses
`DecalRotation`, `bNoClip`, `bProjectOnBackfaces`, a filter or a `SortOrder`; 8 use tiling or offsets; one is not
square; one has `bFlipBackfaceDirection` (its owner is mirrored, §8.4). Widths range from 17 to 16,155 UU; 440 of
the 469 have `FarPlane = Width + 100` (the editor's scaling rule, TENTATIVE). The 5 hidden decals are
`DecalActorMovable`s that Kismet unhides (INTEGRATION.md §6). 6 movable decals project onto nothing where they
are placed (five in AG-Darkcave, one in AG-ParadiseCave whose box holds only a mover that does not collide).

The first version of this table (the builder's) counted 458 decals with geometry, 945 receiver meshes and 58,263
triangles: it projected onto the editor's receiver lists only, along the unmirrored direction, by stored winding
(§8.4, §8.5 say what changed and why).

### 8.2 Tagged properties and templates — CONFIRMED (cdo, map decode)

A decal component's values are its own tags over its template chain and the class defaults
(`Default__DecalActorMovable.NewDecalComponent` → `Default__DecalActorBase.NewDecalComponent` → `DecalComponent`
defaults; for checkpoints `Default__ASAMUCheckpointVisuals.checkpointDecal`).

| Property | Class default | Notes |
|---|---|---|
| `DecalMaterial` | none | set on every placed decal |
| `Width`, `Height` | 200, 200 | UU |
| `NearPlane`, `FarPlane` | 0, 300 | along the projection direction |
| `TileX`, `TileY`, `OffsetX`, `OffsetY` | 1, 1, 0, 0 | texture tiling / offset |
| `DecalRotation` | 0 | degrees about the projection direction |
| `FieldOfView` | 80 | not used by these (orthographic) decals |
| `bProjectOnBSP` / `StaticMeshes` / `SkeletalMeshes` / `Terrain` | true | |
| `bProjectOnBackfaces`, `bProjectOnHidden`, `bNoClip` | false | |
| `DepthBias`, `BackfaceAngle`, `BlendRange` | −6e−5, 0.001, (89.5, 180) | |
| `DecalTransform` | `SpawnRelative`; `OwnerAbsolute` in the decal actors' template; `OwnerRelative` in the checkpoint template | |
| `bStaticDecal` | true in the base template and the checkpoint template | so on all 469, the movable ones included (their template only adds `bMovableDecal`) (T) |
| `bMovableDecal` | true in `DecalActorMovable`'s template | the cooker stores no receivers for these |
| `bFlipBackfaceDirection` | false | recomputed by the engine on every update (§8.4); set on one decal |
| `ParentRelativeOrientation` | (0, 0, 0); pitch 49152 (−90°: straight down) in the checkpoint template | |
| `HitLocation`, `HitNormal`, `HitTangent`, `HitBinormal` | — | stored results of the editor's last update: location, −direction, −width axis, +height axis (CONFIRMED against the frame of §8.4) |
| `DecalReceivers[].Component` | — | the components the editor had attached the decal to when the map was saved: 2,455 static mesh components and one BSP `ModelComponent` (the front end's decal). Cleared and recomputed by the engine at run time (§8.5) **(V)** |

### 8.3 Native data — CONFIRMED (T: exact consumption and byte-exact re-encode of all 469)

Read in `UDecalComponent::Serialize` @ 0x10033E1F0, `operator<<(FArchive&, FStaticReceiverData&)` @ 0x1003429C0
and `operator<<(FArchive&, FDecalVertex&)` @ 0x10033FC50. `UPrimitiveComponent::Serialize` writes nothing
(LIGHTMAPS.md), so after the tagged properties:

```text
DecalComponent        i32 Count | Count × FStaticReceiverData
FStaticReceiverData   obj Component
                      bulk TArray<FDecalVertex>   i32 ElementSize (28) | i32 Count | elements
                      bulk TArray<u16> Indices    i32 ElementSize (2)  | i32 Count | elements
                      u32 NumTriangles
                      light map reference         u32 type (0 none, 1 FLightMap1D, 2 FLightMap2D) + the light map
                      TArray<obj> ShadowMap1D     (file version > 665)
                      i32 Data                    (file version > 620)
                      i32 InstanceIndex           (file version > 664)
FDecalVertex (28)     FVector Position | FPackedNormal TangentX | FPackedNormal TangentZ
                      | FVector2D LegacyLightMapCoordinate
```

Field names are UE3 conventions (TENTATIVE where only the layout is proven). On the shipped data (T): the 370
movable decal components store no receiver and the 99 static ones 210 in all (18,394 vertices); every receiver is
a `StaticMeshComponent`; `Data` and `InstanceIndex` are 0 and the shadow map array is empty everywhere; 190
receivers have no light map and 20 a vertex light map (`FLightMap1D`, bulk sample offsets absolute as in
LIGHTMAPS.md). `NumTriangles` equals the index count over three and every index is in range on all 210.

**Vertex space and clipping** (T): `Position` is in the **receiver component's local space** (read as world
positions, the vertices of only 3 of 200 receivers checked independently fall in their decal's box). 190
receivers are
stored clipped to the decal box (their vertices lie on the box faces; within 8 UU where the receiver is scaled);
the 20 receivers with a vertex light map are stored as **whole triangles** (their vertices are the receiver's
own, up to tens of thousands of UU outside the box) — STRONG: so that the light map's per-vertex samples stay
valid; the engine then clips per pixel. Whole triangles or not, the cooker kept only triangles that face the
decal: none of the 6,848 cooked triangles of those 200 receivers faces away (independent check).

### 8.4 Decal frame — CONFIRMED (native; T on the cooked receivers)

From `UDecalComponent::UpdateOrthoPlanes` @ 0x100337A20 and `CaptureDecalState` @ 0x10033B210. With location `L`
and orientation axes `X, Y, Z` (the rotator's rows, LEVEL_FORMAT.md):

- projection direction `D = X`; width axis `W = cos r · Y + sin r · Z`, height axis `H = cos r · Z − sin r · Y`
  with `r = DecalRotation` in degrees;
- a point is inside when `|W·(p − L)| ≤ Width/2`, `|H·(p − L)| ≤ Height/2` and
  `NearPlane ≤ D·(p − L) ≤ FarPlane`;
- the function also stores `HitLocation = L`, `HitNormal = −D` (times −1 again when `bFlipBackfaceDirection`),
  `HitTangent = −W`, `HitBinormal = H`. (T): on all 469 components the stored hit frame equals the frame
  computed here (axes within 0.0003, origin exactly);
- **mirrored owners (V).** The function first recomputes `bFlipBackfaceDirection`: true when the decal is static
  (`bStaticDecal`) and its owner's `DrawScale3D` has a negative product, false otherwise. With the flag the
  stored normal is `+D`, and since the near and far planes (and the frustum corners of `CaptureDecalState`) are
  built from that normal, the box lies on the other side of `L`: such a decal projects along **`−D`**, onto
  faces that look back at `L`; `W`, `H` and the texture coordinates do not change, so the picture is the mirror
  image of the unmirrored decal's, as the owner is. CONFIRMED (native) for the flag, the normal and the planes.
  The receiver's face test negates its cosine under the same flag; read as keeping it a test against the actual
  projection direction (STRONG: the native code, and the data — inside the mirrored box the listed receivers
  have 91,346 UU² of faces looking back at `L` and 10,071 UU² looking away). (T): the serialized flag equals the
  rule on all 469; it is set on one decal (AG-BeautifulCity, owner scale (1, −1, 1)), whose stored normal is
  `+D`; projected along `+D` it touches none of its 13 listed receivers, along `−D` it covers a surface of its
  own cross-section's size next to its unmirrored twin (same material, same width);
- the decal matrix maps `p − L` to `m.x` along `HitTangent · TileX / Width` and `m.y` along
  `HitBinormal · TileY / Height`;
- the decal vertex shaders output the texture coordinate `−m.xy + DecalOffset + 0.5`: the shipped OpenGL shader
  cache holds its shaders as GLSL text, and 192 of its 2,403 distinct vertex shaders end their decal coordinate
  with exactly this sequence (subtract a location constant from the world position, combine with four matrix
  constants, negate, add a two-component constant, add 0.5) [CONFIRMED (shader cache); that the added constant is
  (`OffsetX`, `OffsetY`) is STRONG: `FDecalVertexFactoryBase::SetDecalOffset(FVector2D)`]. With the stored
  `HitTangent = −W` this gives **`u = 0.5 + OffsetX + TileX · W·(p − L) / Width`** and
  **`v = 0.5 + OffsetY − TileY · H·(p − L) / Height`**: seen along the projection direction with `H` up, the
  texture is upright and unmirrored. (The verification pass found the 2,403 distinct vertex shaders too; its own,
  looser pattern for "negate, add a two-component constant, add 0.5" matches 257 of them.) Checked by eye in the
  app (§10): a decal with lettering reads upright and left to right, and painted decals run their drips downward.

Placement: for `DecalTransform_OwnerAbsolute` (decal actors) `L` and the axes are the owner's `Location` and
`Rotation`; for `DecalTransform_OwnerRelative` (checkpoints) the owner's transform applied to
`ParentRelativeLocation` and the axes of `ParentRelativeOrientation` composed with the owner's rotation. The
owner's `DrawScale` (10 on many decal actors) and `DrawScale3D` do **not** scale the box. (T): with these rules
every cooked vertex of the 190 clipped receivers lies within 16 UU of its decal's box (the largest excess seen
is about 8 UU, on receivers scaled 20×), and exactly the 20 vertex-lit receivers exceed it.

### 8.5 Projection and receivers

**Triangles and the face test — CONFIRMED (native, T).** `UStaticMeshComponent::GenerateDecalRenderData` takes
the receiver's triangles from the mesh's **collision kDOP tree** (a frustum query with the decal's planes), so
only LOD 0's triangles of sections with collision enabled can carry a decal (MESHES.md: the kDOP triangles are
exactly those) **(V)**. For each it forms the normal in the receiver's local space and its cosine `c` with the
reversed projection direction; the triangle is kept when `c > BackfaceAngle` (0.001), or, with
`bProjectOnBackfaces`, when `|c| > BackfaceAngle`; then it is clipped to the box (unless the no-clip path
applies, §8.3). `asamu_ue3::decal::DecalBox` / `DecalProjector` do the same in world space: collision triangles,
a repeated triangle once, the face test of `DecalBox::faces`, Sutherland–Hodgman against the six planes, fans.
A face almost edge-on to the projection (up to 89.9°) still passes, so a decal aimed along a floor smears over
it in the original too; whether `BlendRange` (89.5°, 180°) fades any of that is not decoded (TENTATIVE: the
default range starts half a degree before edge-on).

**Mirrored receivers (V) — STRONG (native reading; T for the data).** A receiver with a negative scale (its
transform's determinant is negative) reaches world space with its triangles' winding reversed, so a winding
normal taken there points into the surface; the native test, in local space, is unaffected. The projector swaps two corners of every triangle
of such a receiver (`transform_mirrors`). (T): no cooked receiver is mirrored (0 of 196), so the cooked data
never showed this; five receivers of three movable decals are, and their decals' boxes hold 868,977 UU² on
outward faces against 12,386 UU² on faces chosen by stored winding. One decal (AG-StarHaven) lands only on such
a receiver.

Checks against the cooker's output (T):

- static `DecalActor` (AG-Darkcave, 4 receivers, 121 cooked triangles): 3 receivers identical in vertex and
  triangle count with every cooked vertex within 0.07 UU of ours; the fourth has 85 triangles against 83 (two
  polygons clipped differently on a receiver scaled 20×);
- all 99 decals with cooked receivers: per receiver the projected area equals the cooked area (clipped to the
  box) within 2 % on 209 of 210 (the last one's cooked area is 2 UU²) and within 10 % on all; totals 5,772,596
  against 5,770,522 UU²; the components in the editor list that the cooker left out get zero area from the
  projection too; every cooked receiver is in the editor list. On these 210 the collision triangles and all of
  LOD 0's give the same areas.

**Which components receive a decal (V).** Read in `UDecalComponent::Attach`, `BeginPlay`, `ComputeReceivers`
and `AttachReceiver`:

- attaching detaches the decal from every entry of `DecalReceivers` and clears them; in the game a static decal
  then waits for `BeginPlay`, which calls `AttachToStaticReceivers` when cooked receivers exist and
  **`ComputeReceivers`** otherwise; later re-attachments (a moved decal) compute again;
- `ComputeReceivers` (no `HitComponent`, no `ReceiverImages`) asks the world's **collision hash** for the
  primitives overlapping the decal's bounds (the box's corners), keeps those whose owner is in the decal owner's
  level when the decal is static, and hands each to `AttachReceiver`; BSP comes from a separate query of the
  level's model when `bProjectOnBSP` is set;
- `AttachReceiver` refuses a primitive that does not accept the decal (`bAcceptsStaticDecals` for a static
  decal, `bAcceptsDynamicDecals` for a dynamic one; a movable decal passes either flag), one that is hidden
  (`HiddenGame`, or `bHidden` on its owner) unless `bProjectOnHidden`, one already attached, and one the actor
  filter (`FilterMode`, `Filter`) excludes.

CONFIRMED (native) for this control flow. So the 99 decals with cooked receivers show exactly those, and the 370
movable ones whatever the query finds when the level starts — not the serialized list. What the hash holds is
STRONG, from the data (T, `editor_receiver_lists_are_a_prefix_of_the_run_time_query`):

- all 2,455 static mesh components in the editor lists collide (`CollideActors` on the component and
  `bCollideActors` on its actor), accept static decals and are visible, while 2,574 of the 22,545 placed static
  mesh components (11 %) do not collide or have no collision triangles: the lists are the output of this query;
- of the movable decals' static mesh receivers that end up with geometry, the 756 listed ones all belong to
  actors that come **before** the decal's actor in `ULevel::Actors`, and the 197 the query adds all to actors
  that come **after** it, without one exception either way. The editor attaches components actor by actor when
  it loads a map, so a decal's list is the query's answer at that moment; in the game the query runs at
  `BeginPlay`, with every component attached.

`DecalProjector::world_receivers` models the query for static mesh components: class exactly
`Engine.StaticMeshComponent` (the only class in the lists besides the one `ModelComponent`), owner listed in the
same level, colliding, with collision triangles, bounds overlapping the decal's; `extract_map_decals` takes a
movable decal's listed receivers first (each once) and then the rest of the query. The listed BSP component is
projected too (the polygons of its nodes on drawn surfaces, wound by the node planes; TENTATIVE conventions, 2
triangles on the front end's decal). Not modelled: receivers that are not static meshes (a colliding skeletal
mesh inside a box would take a movable decal in the original, TENTATIVE; instanced meshes are untested), BSP
beyond listed components, receivers Kismet hides, shows or moves after the level starts, and `BlendRange`.

Cost: 4.7 million triangle tests over all maps. The extraction is bounded for hostile input: repeated actor
entries and repeated receivers are taken once, at most 4,096 receivers per decal, 2^18 query candidates and 2^22
decal triangles per map, and 2^28 triangle tests (candidate scans count), checked before a receiver's triangles
are built.

### 8.6 Checkpoint markers (`ASAMUCheckpointVisuals`) — CONFIRMED (src, cdo)

98 placed actors (one per checkpoint marker). Components: the decal `checkpointDecal` (material
`CavePaintings.Checkpoint_Decal`, static, owner-relative, pointing straight down, 200 × 200, far plane 300) and
`EffectMesh` (static mesh `CavePaintings.Meshes.Checkpoint`). At start the actor gives the effect mesh a material
instance, **hides it** and sets the instance's scalar `Opacity` (`matInstParam`) to 0. `Activate` (state
`InActive` → `Activating`) spawns `AsamuParticles.Particles.CheckpointParticle` at the actor, shows the mesh,
plays `checkpoints.Checkpoints_Reached_Cue`, ramps `Opacity` 0 → 1 in 50 steps and back in 50 steps (a latent
sleep of 0.016 s each, so one frame per step, G-TM-3), hides the mesh again and stays `Active`. The state is saved
(0 / 1 / 2). The decal itself is always visible.

Runtime: the decal is drawn (§10). The effect mesh is an ordinary static mesh component in the scene, so the
level renderer currently draws it permanently; hiding it until activation and the flash belong to the render
plan and the checkpoint workstream (reported, not done here).

## 9. Importer output (`asamu-import decals`)

`<out>/decals/`, written only to the user-local output with the importer's shared safety checks: 7 map files
(about 17 MB), 42 masks (57 MB) and a manifest, in about 3 s.

`<map>.decals.json`, format `asamu-decals` version 1:

```text
{ "format": "asamu-decals", "version": 1, "package": map, "coordinates": "...", "notice": "...",
  "decals": [ {
      "slot": index in ULevel::Actors, "name", "path", "class", "decal_actor": bool, "component": path,
      "hidden": bool, "movable": bool, "static_decal": bool, "tag", "base", "material": path,
      "location": [x,y,z], "rotation": [pitch,yaw,roll], "draw_scale",
      "width", "height", "near_plane", "far_plane", "tile": [x,y], "offset": [x,y], "rotation_degrees",
      "depth_bias", "slope_scale_depth_bias", "sort_order", "blend_range": [a,b], "no_clip", "project_on_backfaces",
      "mirrored": bool,
      "frame": { "origin", "direction", "width_axis", "height_axis" },
      "static_receivers": count, "unclipped_receivers": count, "hidden_receivers": count,
      "receivers": [ { "component": path, "component_class", "source": "cooked" | "projected",
                       "positions": [x,y,z, ...] (world UU, 0.01), "normals": [...], "uvs": [u,v, ...],
                       "indices": [...], "outside": 0, "has_light_map": bool, "listed": bool } ],
      "unresolved_receivers": [path...] } ],
  "masks": { material path: { "file": "masks/<name>.dds", "texture", "channel", "scale", "size", "mips" } },
  "stats": counts, "warnings": [...] }
```

`frame.direction` is the direction the decal projects along (reversed for a mirrored owner, `mirrored`).
`listed` is false for a receiver only the run-time query finds (§8.5). Cooked receivers are transformed to
world space and clipped to the box; UVs follow §8.4 (`u` along the width axis, `v` against the height axis, box
centre 0.5, tiling and offset applied); every vertex carries the outward face normal of the receiver triangle it
was clipped from; triangles are wound so that UE3's winding rule gives that normal, also on mirrored receivers.
An existing map file is kept unless `--force`; `--check` prints the coverage table and writes nothing. The
fields added by the verification pass (`mirrored`, `listed`, `masks`) did not change the version: a reader
ignores what it does not know, and a file from before them still loads (re-run with `--force` to get the
receivers and masks it lacks).

**Decal masks (V).** A material needs one when it is masked or translucent, the channel it displays (emissive
when unlit, else the base colour) has no texture, and its opacity comes from a texture — 42 of the 69 decal
materials: 20 read the opacity texture's alpha, 20 its red, one green, one blue (a colour output wired to the
scalar opacity input gives its first component; STRONG, the engine's cast). `masks/<name>.dds` is a white
`A8R8G8B8` image, at most 1,024 texels on its longer edge, with mips, whose alpha is that channel (a colour
channel of an sRGB texture linearised as the GPU would, alpha as stored) times the material's constant. The name
is the material path made file-safe plus a hash of the exact path, so it depends on the material alone; the
masks of a map's materials are listed in that map's file, and all of the run's in `manifest.json` next to the
per-map counts and totals.

## 10. Runtime rendering (`apps/asamu/src/vfx.rs`, `vfx/level_decals.rs`)

**Choice: projected meshes**, not Bevy's forward or clustered decals. The importer already produces the decal
geometry the original would (§8.5), so the app draws ordinary meshes lying on their receivers: lifted 0.2 UU along
the normal, depth-biased, both faces, alpha-blended or alpha-tested as the converted decal material says, no
shadows. That needs no depth prepass on the shared camera (forward decals do, and they are camera-facing quads
faded by depth rather than projections) and no clustered-decal support (binding arrays), and renders the same on
every platform. Materials come from the shared render-material model (`materials.json`) with the approximations
of MATERIALS.md, except for the shape:

**Shape (V).** The shared model shows one texture per material and takes the alpha from it. For the 42 materials
of §9 that would leave a solid patch in the material's constant colour (the first local check of this pass
showed exactly that: sheets of glowing cyan where the cave paintings are). For them the app binds the baked
mask as the material's texture: white, so the constant colour and glow stay, with the picture in the alpha
(`decal_shape`). A decal file without masks (written before they existed) falls back to the opacity texture
itself when its alpha channel is the mask; where the mask is a colour channel the decal is **not drawn** and a
warning says to re-run the importer.

Each decal mesh is tagged with its actor: decals visible at start carry `converted::LevelEntity` (actor slot and
plan level), so `kismet::sync_actor_render` hides them with Kismet, moves them with Matinee and gates the ones of
streamed sub-levels (TheCore) like any level mesh; the 5 decals hidden at start carry `kismet::KismetActor` and
appear when Kismet unhides them. Decals reload with every new render plan. The decal file of a level is looked
up by the level's name only when that is a plain file name, is read with a size bound, and its receivers and
masks are validated (counts, index ranges, finite numbers, mask file names inside `masks/`).

Local check by the builder (2026-10-10; converted data and screenshots under an ignored `research/` folder,
deleted afterwards, nothing committed): `asamu-import` `levels`, `meshes --collision`, `textures`, `materials`,
`kismet`, `matinee` and `decals` for AG-ParadiseCave (plus the effect textures of `Startup`), then:

- `asamu --level AG-ParadiseCave --fly --camera …` at a rune decal and above a checkpoint: the rune (a
  `DecalActorMovable` on a rock face) and the checkpoint symbol (on the ground) lie on their receivers, masked
  by their textures.
- `ASAMU_VFX_SELFTEST=17 asamu --level AG-ParadiseCave --screenshot … --screenshot-delay 17.4` (the debug
  self-test of `vfx/selftest.rs`: after the level's intro it turns the player to the first grapple-able surface
  600–1,800 UU away and holds fire): the gun attaches 1,160 UU away, the blue beam runs from the lower right of
  the view to the anchor, the hit decal (98 triangles, the grapple rune in the beam's blue) sits on the anchor
  and stays after the release.
- The gated test `vfx::tests::converted_level_grapple_effects` (`ASAMU_CONVERTED_DIR`) asserts the same on the
  simulation side: a non-empty hit decal around the anchor with unit-square texture coordinates, the beam's end
  points, the speed-line parameter of the capped 2000 uu/s (0.4) on every pulled tick although the stored
  velocity is higher, and the top hand lamp going dark for the used grapple.

Local check by the verification pass (same day, two other maps, same hygiene):

- AG-BeautifulCity, fly camera, 118 level decal meshes (11,500 triangles). Before the masks: large solid cyan
  sheets across the view where cave paintings are (one of them a decal aimed along a floor). With them: line
  drawings. A painted map on a wooden board (constant colour, alpha mask) and a black drawing on a crate (red
  channel mask) show as drawings, upright, their paint drips running down; a decal with lettering on a crate
  reads upright and left to right, which settles the texture coordinates of §8.4 by eye; a column of glowing
  glyphs sits on a mossy rock. (By the converted data, not by eye: the mirrored decal and its unmirrored twin
  share three receivers, a pile of stones.)
- AG-Darkcave, 551 decal meshes (58,374 triangles), `ASAMU_VFX_SELFTEST=15`: no beam a moment before the shot;
  0.25 s after the attach (about 800 UU away) the blue beam runs from the lower right to the anchor at the centre of
  the view, the hit decal (124 triangles) sits there, faint radial speed lines are visible, and a glowing cave
  painting on the wall beside it is a line drawing. AG-BeautifulCity starts in story mode: the self-test's fire
  is a story interaction there, not a grapple.

Not checked by eye: the HUD parts (crosshair ring, hand lamps, lens overlay) — the screenshot camera does not
draw the HUD — and Kismet hiding, unhiding or moving a decal in play.

## 11. UNKNOWN / not done

- The beam's exact look: the tangent directions, the noise scaling (the narrow emitter's range table), whether
  beam particles linger after release; the goat-mode tongue systems. (The system's sprite emitters are disabled:
  nothing to draw.)
- The crosshair movie's art and fade timing; the pause menu's re-show of the crosshair (§5); the hand plate
  material's lamp parameters are shown as HUD dots.
- `BlendRange` fading; decal light maps; the uniform layout of the decal shaders beyond the coordinate sequence.
- Run-time decals are ray lattices, not clipped receiver triangles, do not follow moving receivers and do not
  check `bAcceptsDynamicDecals`.
- Level decals are projected once, where the map places them: a decal or receiver that Matinee moves keeps its
  first geometry (the original computes the receivers again whenever the decal moves), and receivers that are
  skeletal meshes, instanced foliage or unlisted BSP get nothing.
- Lit decal materials get dynamic lights only (no light maps): painted decals in baked-light areas are darker
  than in the original; glowing ones are unaffected.
- Extras settings (beam colour, goat, Midas) are modelled (`ExtrasSettings`) but not wired to the settings menu
  or the collectibles count.
- The checkpoint markers' effect mesh is drawn permanently by the level renderer (§8.6).
- Emitter-pool effects named here (ignite, connect, power-jump charged/takeoff/land, checkpoint particles) belong
  to the particle workstream; camera animations and rumble to theirs.
