# Particles: systems, emitters, modules, distributions, placements and the runtime

Evidence source: every particle-related export of the 39 non-shader-cache packages under
`~/Library/Application Support/Steam/steamapps/common/A Story About My Uncle/A Story About My Uncle.app/Contents/Resources/ASAMU/CookedMac`
(and `Maps/`) of the legitimately owned Mac install (Steam build 1822049), decoded read-only by our own code in
`crates/asamu-ue3/src/particle.rs`; the unstripped Mac executable (local Ghidra decompilation into the ignored
`research/decompiled/`, never committed, plus local disassembly of single functions) for behaviour; the shipped
config files for engine limits; and the GLSL text of the shipped OpenGL shader cache, read locally, for the sprite
vertex layout. This page is structure, names, counts, constants with their source and our own description of
behaviour: no payload bytes, no particle systems, no shader text, no script text and no decompiled code.

**Verification pass (2026-10-10).** The decoding was repeated by an independent Python reader (its own class
hierarchy, prelude, tag walker, archetype / class-default merge, distribution decoder and curve evaluator; nothing
shared with the Rust code) and compared with the importer's output, and the behaviour claims were re-read in the
executable. The decoding agreed completely (numbers in "Independent check" below). Several behaviour claims of the
first version of this page were wrong and are corrected here, with the runtime: the order of the emitter tick (the
reset runs after the spawn), the alpha clamp (there is none at run time), the LOD rule and LOD methods, what
`ActivateSystem` does to a running system, the system `Delay`, fixed-time systems, the warm-up tick, linked orbit
chains, `SizeScaleByTime`, system completion, and the size figures of the largest effects. Each is marked
"(corrected)" where it stands.

Builds on `OBJECT_FORMAT.md` (prelude, tagged properties, archetypes, class defaults), `MATINEE.md` (the engine's
curve evaluation), `MATERIALS.md` (blend modes, `materials.json`), `LEVEL_FORMAT.md` (actors, component transforms)
and `KISMET_RUNTIME.md` (outputs). None of them changed.

Reproduce:

```sh
cargo test --release -p asamu-ue3 --test particle_real_data -- --nocapture   # every (T) number on this page
cargo test -p asamu-ue3 --test particle                                      # synthetic + hostile fixtures
cargo run --release -p asamu-import -- particles --check                     # coverage report, writes nothing
cargo run --release -p asamu-import -- --out <user-local dir> particles      # particles/particles.json + particles/maps/*.json
cargo test -p asamu-assets --lib particles                                   # evaluator + simulator (synthetic, hand-checked)
ASAMU_CONVERTED_DIR=<dir> cargo test --release -p asamu-assets --lib particles   # + the converted systems
cargo test -p asamu particles                                                # renderer, material, Kismet hook
cargo run -p asamu -- --converted <dir> --level AG-Workshop --fly            # see them
```

`(T)` marks a claim asserted by `crates/asamu-ue3/tests/particle_real_data.rs` (or the gated importer test
`converts_the_install_when_present`) against the install; `(A)` one asserted by the gated tests of
`crates/asamu-assets/src/particles.rs` against converted data. Counts without a mark were taken from the converted
JSON by throwaway scripts and are not asserted by a test.

Native functions read (symbol names of the unstripped executable): `FRawDistribution::GetValue1/3` and their
`Random` / `Extreme` variants, `FRawDistributionFloat/Vector::{GetValue, GetFastRawDistribution, Initialize}`,
`UDistribution{Float,Vector}{Constant,Uniform,ConstantCurve,UniformCurve,ParameterBase}::GetValue`, the `Serialize`
functions listed below, `FParticleEmitterInstance::{Tick, Tick_EmitterTimeSetup, Tick_SpawnParticles, Spawn, PreSpawn,
PostSpawn, KillParticles, ResetParticleParameters, UpdateBoundingBox, SetupEmitterDuration,
GetCurrentBurstRateOffset, UpdateOrbitData, CalculateOrbitOffset, Resize, Init, InitParameters, HasCompleted}`, the
`Spawn` / `SpawnEx` / `Update` of the modules in the table below, `UParticleSystemComponent::{Serialize,
ActivateSystem, DeactivateSystem, InitializeSystem, InitParticles, Tick, HasCompleted,
DetermineLODLevelForLocation}`, `FDynamicSpriteEmitterData::GetVertexAndIndexData`, and `FEngineLoop::{PreInit,
Init}`, the two places that derive the engine's particle count limits. The order of the calls inside
`FParticleEmitterInstance::Tick` was taken from the class's vtable (slot → symbol), not from the decompiler's guess.

## Result — CONFIRMED (T)

| | Count |
|---|---:|
| particle exports (all packages, class default objects included) | 10,978 in 15 packages |
| of which prelude + tagged properties end exactly at `SerialSize` | 10,978 (2,541,859 payload bytes) |
| classes | 156 (131 module classes, 9 float and 8 vector distribution classes, 4 component classes, 2 emitter classes, system, LOD level); 100 of them occur only as their class default object |
| `ParticleSystem` exports / decoded completely | 111 (1 default object) / 110, 0 failures |
| distinct system paths | 100 (10 repeats: 8 systems cooked into two or three maps each; the copies agree in everything but one editor statistic, `PeakActiveParticles` of one LOD level) (T) |
| emitters / LOD levels / modules reached through the systems | 396 / 667 / 6,343 (modules counted per LOD level that lists them) |
| raw distribution properties reached | 8,801, **every one with a distribution object** |
| emitter kinds | sprite 294, mesh 76, beam 26 (no trail, ribbon or PhysX emitter outside class defaults) |
| placed particle system components (12 maps) | 162, all on `Engine.Emitter` actors, all with a template that decodes |

Exports per package (T): `Startup` 8,303, `AG-ParadiseCave` 422, `Engine` 381, `AG-StarHaven` 328, `AG-Darkcave` 318,
`AG-IceCave` 312, `ASAMUFrontEndMap` 231, `AG-BeautifulCity` 226, `AG-Workshop` 181, `TheCore` 149, `AG-Epilogue` 92,
`UTGameContent` 22, `UDKBase` 10, `Core` 2, `UnrealEd` 1. 73 of the 100 distinct systems live in `Startup.upk` (the
game's own effects and the stock UT content it carries); the rest are cooked into the maps that use them.

Most frequent classes (exports): `DistributionFloatConstant` 1,955, `DistributionVectorUniform` 1,053,
`DistributionFloatUniform` 1,042, `ParticleLODLevel` 686, `DistributionVectorConstantCurve` 631,
`DistributionVectorConstant` 483, `ParticleModuleSpawn` 474, `DistributionFloatConstantCurve` 410,
`ParticleModuleRequired` 408, `ParticleSpriteEmitter` 406, `ParticleModuleSize` 406, `ParticleModuleLifetime` 399,
`ParticleModuleColorOverLife` 315, `ParticleModuleVelocity` 275, `ParticleModuleSizeMultiplyLife` 245.

## Serialization — CONFIRMED (T)

**No particle class has native data at v868.** Every one of the 10,978 exports is a prelude plus tagged properties.
The executable has native `Serialize` functions for `UDistributionFloat`, `UDistributionVector`, their
`Uniform` / `Constant` / `ConstantCurve` / `UniformCurve` / `UniformRange` vector subclasses and for
`UParticleSystemComponent`; read locally, the distribution ones only call their base class, and the component's
only walks its live emitter instances, of which a loading object has none. (`UParticleSystemReplay` serializes
replay frames; its only export is its class default object and it is not decoded here.) The decoder therefore refuses any particle export whose tags do
not end at `SerialSize`.

Distributions are `Component` subclasses (`Core.DistributionFloat` / `Core.DistributionVector`), so their prelude has
the component template fields.

**Object graph.** `ParticleSystem.Emitters` → `ParticleSpriteEmitter.LODLevels` → `ParticleLODLevel` with
`RequiredModule`, `SpawnModule`, `TypeDataModule`, `EventGenerator` and `Modules`. The modules are subobjects of the
*system* (LOD levels share them); each distribution is a subobject of its module. An emitter's kind comes from LOD
0's type data: none = sprites, `ParticleModuleTypeDataMesh`, `ParticleModuleTypeDataBeam2`.

**Values.** Tags are deltas against the archetype (in practice the class default object), member-wise for tagged
structs. This matters for `RawDistributionFloat` / `RawDistributionVector` properties: an instance often stores only
`Distribution` and inherits `Op`, the element counts or even the lookup table from the class default object. The
decoder merges own tags over the archetype's effective values, or over the merged class defaults.

`RawDistribution*` members: `Distribution` (object), `Type`, `Op`, `LookupTableNumElements`, `LookupTableChunkSize`,
`LookupTable` (floats), `LookupTableTimeScale`, `LookupTableStartTime`.

### Which one the game evaluates — CONFIRMED (executable), STRONG that `GIsGame` is set in the shipped game

`FRawDistributionFloat/Vector::GetValue` and `GetFastRawDistribution` read the lookup table only when no
distribution object is set, or when the process is not running as a game (and a table exists). With an object and
`GIsGame` set they call the object's `GetValue`; the "fast" accessor returns nothing, so its callers
(`ColorOverLife::Update` for one) fall back to `GetValue` too. Since all 8,801 shipped raw distributions have an
object (T), **the shipped game evaluates the distribution objects**; the baked tables are not what it samples. Our
runtime does the same and keeps the table path only for a raw distribution without an object (none shipped).

`GIsGame`: `FEngineLoop::PreInit` stores 1 in it on both of its game paths (the seek-free one a cooked build takes
and the regular one), next to clearing the editor and commandlet flags (read in the disassembly). No later write
was looked for, hence STRONG.

### The baked lookup table — CONFIRMED (executable + T)

- Layout: `[range min, range max]`, then entries of `LookupTableChunkSize` floats. `Op` 1 (none) stores the value
  (1 float or 3), `Op` 2 (random) and 3 (extreme) store `min…` then `max…` (2 or 6).
- Read (`GetValue1` / `GetValue3`): `t = max((time − start) · scale, 0)`, entry `i = trunc(t)`, the two entries at
  `min(i · chunk + 2, len − chunk)` and one chunk later (same clamp), lerped by `t − i`. Random draws one number per
  component and lerps min→max; extreme picks min or max (a draw above 0.5 picks max).
- Baking (`Initialize`): 2 entries for a domain shorter than 0.05 (and for a two-key curve that needs no more;
  TENTATIVE for the exact condition), else `min(trunc(20 · span), 99) + 1` entries, one every `span / (n − 1)`;
  `scale` is the reciprocal of that step (0 for a constant), `start` the domain start. The two constants (0.05, 20)
  were read from the executable's data.
- **Agreement with our decoding (T; each system path counted once).** For every table we evaluate the decoded object at each entry's sample time
  with the engine-exact curve evaluator of `MATINEE.md` and compare: **3,916 tables agree** (1,700 float constants,
  256 float curves, 492 float uniforms, 1 float uniform curve, 425 vector constants, 345 vector curves, 697 vector
  uniforms); **179 curve tables** belong to curves whose stored `InterpMethod` is not an enumerator (below) and
  match exactly when the tangents are *not* multiplied by the key span; **6 uniform tables** hold other min/max
  values than their object (tables older than the object's current values; all in 4 systems of the game's own
  content). Nothing else disagrees.
- The range header is not sorted: 99 uniform tables (56 float, 43 vector) have entries outside `[header0, header1]`,
  all of them uniforms whose `Min` exceeds `Max` in some component (T).

### Legacy curve method — CONFIRMED data fact, STRONG interpretation

381 of the 820 distribution curves reached (T) store `InterpMethod` as the name `None`, which is not an enumerator
of `EInterpMethodType`. All of them are stock UT content (`Envy_Effects`, `WP_LinkGun`, `WP_RocketLauncher`,
`WP_ShockRifle`, `FX_VehicleExplosions`, `T_FX`, `Pickups`, `Envy_Level_Effects_2`, `VH_All`, `WP_Translocator`,
`FoliageDemo`). Their baked tables were sampled with unscaled tangents (the engine's older evaluation). The current
evaluator scales tangents unless the method is exactly `IMT_UseBrokenTangentEval` (`MATINEE.md`), and a name that is
not an enumerator cannot load as that value, so the shipped game evaluates these curves with scaled tangents and
their tables are stale. We decode the method as the default and flag the curve `legacy_method`. No system authored
for the game is affected; of the placed stock systems only the `FoliageDemo` water splashes have such curves (4).
UNKNOWN: the exact byte the loader assigns for the unknown name (it changes nothing unless it were 2).

## Distribution objects — CONFIRMED (executable) unless noted

Enumerator orders (T, from `Core.u` / `Engine.u`): `EDistributionVectorLockFlags` = None, XY, XZ, YZ, XYZ;
`EDistributionVectorMirrorFlags` = Same, Different, Mirror; `DistributionParamMode` = Normal, Abs, Direct.

| Class | Value at `t` |
|---|---|
| `DistributionFloatConstant` | `Constant` |
| `DistributionFloatUniform` | `Max + (Min − Max) · r` |
| `DistributionFloatConstantCurve` | the curve at `t` (`FInterpCurve<float>::Eval`) |
| `DistributionFloatUniformCurve` | the `Vector2D` curve at `t`, then `X + (Y − X) · r` |
| `DistributionFloat*Parameter` | the component's parameter of that name, else `Constant`; `Direct` returns it, `Abs` takes its magnitude first, then the value is clamped to `[MinInput, MaxInput]` and mapped linearly to `[MinOutput, MaxOutput]` (slope 0 when `MaxInput ≤ MinInput`) |
| `DistributionVectorConstant` | `Constant` with the lock applied: XY copies X to Y, XZ copies X to Z, YZ copies Y to Z, XYZ copies X to both |
| `DistributionVectorUniform` | per component the lower bound is `Min` (Different), `Max` (Same) or `−Max` (Mirror); `bUseExtremes` picks all-min or all-max (one draw, above 0.5 = max); otherwise `Max + (min' − Max) · r` with one draw per unlocked component in X, Y, Z order; a locked component copies as above (XYZ draws once) |
| `DistributionVectorConstantCurve` | the vector curve at `t`, then the lock |
| `DistributionVectorUniformCurve` | TENTATIVE (no shipped system uses one): the `TwoVectors` curve at `t`, lock and mirror, then per-component lerp or extremes |
| `DistributionVector*Parameter` | as the float one, per component with `ParamModes[3]` |

Locks apply from `LockedAxes` alone; `bLockAxes` is not read by `GetValue`.

Kinds reached through the systems (T): float constant 3,003, uniform 1,643, constant curve 574, parameter 4, uniform
curve 2; vector uniform 1,593, constant curve 963, constant 929, parameter 90.

**Random numbers** (CONFIRMED): every draw is `seed = seed · 0x0BB38435 + 0x3619636B`, the low 23 bits used as the
mantissa of a float in `[1, 2)`, minus its integer part. Modules pass no stream in normal play, so the engine's one
global seed is shared by everything that draws; a faithful replay of the original's particle randomness would need
the whole game's draw order. Our runtime uses the same generator with one stream per emitter instance.

## Emitter instance — CONFIRMED (executable)

One tick, in the executable's order (corrected: the first version of this page had the reset before the spawn):

1. **Time setup.** The emitter origin moves to the component origin (the previous one is kept for spawn
   interpolation). `SecondsSinceCreation += dt`. With `bUseLegacyEmitterTime` (the class default; 544 of the 631
   required-module uses in the 100 distinct systems) the emitter time is `SecondsSinceCreation mod EmitterDuration`
   when the duration exceeds 1e-4 and a loop is counted when `SecondsSinceCreation − duration · loops ≥ duration`;
   otherwise the emitter time advances by `dt` and wraps at the duration. A loop resets the fired bursts and may
   set the duration up again. The delay of the tick is the one in force *before* that (a newly drawn delay counts
   from the next tick); it is subtracted from the emitter time for the tick (not after the first loop with
   `bDelayFirstLoopOnly`) and added back at the end.
2. **Kill**: particles whose relative time exceeded 1, scanned from the end, each swapped with the last live one.
3. **Spawn**, unless suppressed, halted, before the delay, or past `EmitterLoops`:
   - rate = `Rate · RateScale` of the LOD's spawn module at the emitter time (the scale is drawn first), floored at 0;
     bursts: every entry of the spawn module's `BurstList` whose `Time` has passed and that has not fired this loop
     adds `Count`, or `CountLow + round((Count − CountLow) · r)` when `CountLow ≥ 0`;
   - `leftover' = leftover + dt · rate`, `Number = floor(leftover')`, increment `1 / rate`;
   - **count limits**: the tick's `Number + bursts + live` is clamped to `MaxParticleSpriteCount` (no sub-UV
     interpolation) or `MaxParticleSubUVCount`: bursts get the remaining room first, the rate particles what is left.
     The engine derives the limits as `MaxParticleVertexMemory / 272` and `/ 368` (four 68- or 92-byte vertices per
     particle; both divisions read in `FEngineLoop::PreInit` and `Init`); the shipped `MaxParticleVertexMemory=131972`
     (`[Engine.Engine]` of `BaseEngine.ini` and `Mac-ASAMUEngine.ini`) gives **485** and **358**. The clamp is skipped
     for a component whose `bSkipSpawnCountCheck` is set; `InitParticles` copies that flag from the system's
     property of the same name, and **no shipped system sets it** (T). Storage never grows past
     `MaxParticleResize=1024` (`[Engine.Engine]` of `DefaultEngine.ini` and `Mac-ASAMUEngine.ini`; `Resize` refuses
     and the tick spawns nothing, keeping its old leftover), which only an emitter without the count clamp can reach;
   - rate particle `i` gets spawn time `leftover · increment + dt − increment − i · increment` and interpolation
     `1 − (i + 1) / Number`; burst particles get 0 and 0; the leftover drops the unclamped `Number`;
   - per particle: `PreSpawn` zeroes it and puts it at the emitter origin (world-space emitters) or the local
     origin; the type data and every enabled spawn-stage module run in order; `PostSpawn` shifts a world-space
     particle back along the emitter's motion by the interpolation when the emitter moved more than 1 unit, then
     advances it by `velocity · spawn time`.
4. **Reset** (`ResetParticleParameters`), over every live particle, **the ones just spawned included**: velocity,
   size, rotation rate and colour go back to their base values and `RelativeTime += OneOverMaxLifetime · dt`; orbit
   payloads reset their offset and rate. Two consequences: a particle ages by one step in the tick that spawns it
   (a burst particle with lifetime `L` is at `(k + 1) · dt / L` after its `k`-th tick), and whatever a spawn-stage
   function did to the *transient* size, colour or rotation rate (the life multipliers have such functions) is
   gone before the updates run.
5. **Module updates** in order (below), then, with live particles, the orbit chain and **integration**
   (`UpdateBoundingBox`): `Location += Velocity · dt`, `Rotation = fmod(Rotation + RotationRate · dt, 2π)`.

The medium-detail spawn scale applies only below detail mode 2; the shipped system settings set `DetailMode=2`.

Rendering draws at most `MaxDrawCount` particles when `bUseMaxDrawCount` is set, which it is on 628 of those 631
(500 on 384 of them). With the count limits above below 500, the draw limit rarely decides anything; which
particles are dropped (we keep the first ones in live order) is TENTATIVE.

Size of the shipped effects (corrected). Among the placed systems the largest requests are one front-end system
whose two emitters ask for 20,000 particles a second (`Rate` 200 × `RateScale` 100) with lifetimes of 2,000–3,000 s,
and one system placed in AG-Workshop and the front end (not auto-activated; Kismet toggles the Workshop one) whose
one-loop emitters ask for 9,000 a second with a 1 s lifetime and 5,000 a second with 3–5 s lifetimes, for one
second each. The count limits above are what bounds them. The stored `PeakActiveParticles` of those LOD levels
(30,000,320 and 300,640; 9,001; 5,001) are editor-side values: for the one-loop emitters exactly rate × duration
+ 1, so they read as estimates, not measurements (STRONG; the function that fills the field was not read).

**The component around it** (`UParticleSystemComponent`, CONFIRMED unless noted):

- *Which emitters tick.* An emitter instance is ticked only while its current LOD level is enabled; a disabled
  level freezes it (clock and particles), and it is not drawn.
- *Fixed-time systems* (`SystemUpdateMode = EPSUM_FixedTime`; one shipped, stock content, placed once): every
  component tick advances the system by `UpdateTime_Delta`, whatever the frame time (corrected: not an accumulator).
  At the 1/30 s the shipped one stores, it runs at twice real time at 60 frames a second.
- *Activation* (`ActivateSystem`, corrected). Spawning is no longer suppressed and, except for the first
  activation of an auto-activating component (whose instances were initialized when it was attached),
  `InitializeSystem` runs again: the component's emitter delay is set from the system's `Delay` (drawn between
  `DelayLow` and `Delay` with `bUseDelayRange`), and every emitter instance gets `InitParameters` (duration set up)
  and `Init`: spawn fraction, time since creation, loop count and fired bursts start over, the origin snaps to the
  component, the kill flags are taken from the required module, **and live particles stay**. So activating a
  system that is still running restarts its emitters' clocks without clearing the screen.
- *System `Delay`* (corrected): it is added to every emitter's own `EmitterDelay` (and so to its duration); the
  emitters tick during it with a negative emitter time. It is not a pause before the first tick. No shipped
  system stores `Delay` (T).
- *Warm-up* (corrected). `InitParticles` copies `WarmupTime` and `WarmupTickRate` from the system to the component
  on every initialization (the component's stored values do not matter); `ActivateSystem` then ticks the component
  in steps of `WarmupTickRate` capped at the warm-up time, or of **0.032 s** when no rate is set (constant read in
  the executable), until the warm-up time is covered. Three systems have a `WarmupTime` (5, 5 and 6 s) (T), none
  a `WarmupTickRate` (T); they are the water splashes placed 26 times in AG-Darkcave and AG-ParadiseCave and one
  system in AG-StarHaven and TheCore.
- *Deactivation*: spawning is suppressed; an emitter instance whose `bKillOnDeactivate` is set (from its required
  module; script can set it per emitter) is destroyed with its particles, the others let theirs finish. The
  component class has **no** `bKillOnDeactivate` / `bKillOnCompleted` property at v868 (corrected; the importer's
  placement fields of those names are always false).
- *Completion* (`HasCompleted`): per emitter instance — a disabled LOD level is ignored when its loops are finite
  (an endless one holds the system open until it is deactivated); an enabled finite one is done when the time
  since creation has reached `EmitterLoops · EmitterDuration` and it has no particles (once deactivated: when it
  has no particles); an enabled endless one only after a deactivation, with no particles. When the system
  completes, the component's tick fires `OnSystemFinished` and deactivates it. 11 of the 162 placements use systems
  with finite emitters (3 of them only finite ones).
- *Inactivity*: when a component has not been rendered for longer than `SecondsBeforeInactive` (the larger of the
  system's and the component's; the component default is 1 s and every placement has it), its tick sets
  `bForcedInActive` (CONFIRMED). What a forced-inactive component then skips was not read; the stock meaning is
  that it stops simulating until it is rendered again (TENTATIVE). **Not simulated**: our effects keep running
  off screen.

### Modules — CONFIRMED (executable) where "read", otherwise as labelled

Reached through the shipped systems (module instances per LOD level, T) and what the runtime does:

| Module | Count | Behaviour | Runtime |
|---|---:|---|---|
| `Required` | 667 | material, space, duration/loops/delay, screen alignment, sub-images, draw limit | yes |
| `Spawn` | 667 | rate, rate scale, bursts (read) | yes |
| `Lifetime` | 667 | lifetime at the emitter time; a second lifetime module adds to the first; `OneOverMaxLifetime = 1 / life` (0 when ≤ 0), relative time starts at `spawn time / life` (read); the reset of the same tick adds `dt / life` | yes |
| `Size` | 667 | adds `StartSize` to size and base size (read) | yes |
| `ColorOverLife` | 463 | sets colour and alpha from the curves at the relative time, at spawn (also the base colour) and every update (read). `bClampAlpha` is read only by the editor's curve display: **the alpha is not clamped at run time** (corrected) | yes |
| `SizeMultiplyLife` | 441 | multiplies the flagged size components by the curve at the relative time, in the update (read). Its spawn function does the same to the transient size, which the reset of the same tick undoes | yes |
| `Velocity` | 414 | `StartVelocity` (rotated into the world for a world-space emitter unless `bInWorldSpace`) plus `StartVelocityRadial` along the normalized offset from the emitter, times the owner scale when asked; added to velocity and base velocity (read) | yes |
| `Rotation` / `RotationRate` | 382 / 193 | turns: added as `value · 2π` radians (read; the factor is the double 2π in the executable) | yes |
| `Location` | 356 | adds `StartLocation`, rotated and scaled by the component for world-space emitters (read) | yes (`DistributeOverNPoints` not simulated; unused) |
| `Color` | 214 | sets colour and alpha (and base colour) at spawn (read); no alpha clamp (as above; 2 shipped modules have `bClampAlpha` and an alpha range above 1) | yes |
| `Acceleration` | 186 | stores the value per particle; `velocity += a · spawn time` at spawn and `a · dt` every update, on velocity and base velocity (read) | yes |
| `TypeDataMesh` | 147 | mesh particles | drawn as sprites |
| `MeshRotation` / `MeshRotationRate` | 114 / 38 | 3-axis mesh rotation | first axis as sprite rotation (ours) |
| `LocationPrimitiveSphere` | 100 | three draws give a direction (per axis `2r − 1`, `r`, `−r` or 0 from the positive/negative flags); offset = direction · `StartRadius`, each component limited to the normalized direction's share of the radius; `SurfaceOnly` normalizes; the start location is added before the component's rotation and scale; optional velocity = (placed offset − the *untransformed* start location) · `VelocityScale` (read) | yes |
| `SubUV` | 86 | linear methods: image `trunc(SubImageIndex(relative time))` clamped to the grid, blend = the fraction for `Linear_Blend`; random methods: a random image at spawn, and a new one in the update only when `RandomImageChanges` is set and more than `RandomImageTime` of relative time has passed; `Init` derives `RandomImageTime = 0.99 / (RandomImageChanges + 1)`, 1 without changes (read). The 2 shipped random emitters (placed) set no changes: their image stays | yes; blending shows the nearer image |
| `ColorScaleOverLife` | 72 | multiplies colour and alpha, at the relative or the emitter time, in the update (read); its spawn function's effect is undone by the reset | yes |
| `SizeScale` | 59 | `Size = BaseSize · scale` (read) | yes |
| `BeamSource` / `BeamTarget` / `BeamNoise` / `TypeDataBeam2` | 46 / 48 / 48 / 46 | beams (method `PEB2M_Target` on all) | straight segment source → target, no noise (ours) |
| `LocationPrimitiveCylinder` | 31 | directions are drawn until the radial part lies in the unit disc (at most 50 tries); height component `direction · StartHeight / 2`, radial `direction · StartRadius`, pushed out to the radius for `SurfaceOnly` unless on a cap (read) | yes |
| `OrientationAxisLock` | 30 | locks the sprite's facing to an axis | TENTATIVE orientation |
| `Orbit` | 29 | per-particle offset, rotation (turns) and rotation rate, added at spawn and/or update per their options. Per particle the chain is folded in module order: *add* and *scale* accumulate into the current link, *link* closes it and starts the next. Closing a link integrates the rotation (`+= rate · dt`, stored back in the link's last payload); when any component reaches 1e-4 turns the rotation vector is first turned by the chain's matrix so far, read as Euler angles (× 360°, roll X, pitch Y, yaw Z), appended to that matrix (earlier links apply first), and the link's offset is rotated by the result. The link offsets are **summed** into a render-time offset from the particle location (read; corrected: the first version did not turn the rotation vector and composed the links the other way round, which differs once a chain has two links with rotations about different axes). 22 of the 24 distinct orbit modules are links; 6 LOD levels chain two | yes (exact sines instead of the engine's 65536-step table) |
| `MeshMaterial`, `ColorByParameter`, `ParameterDynamic`, `MaterialByParameter` | 28 / 10 / 3 / 2 | material selection and parameters | ignored |
| `LocationEmitter` | 27 | spawn at another emitter's particles | emitter origin (ours) |
| `VelocityInheritParent` | 17 | adds the component's velocity | no-op (placed emitters do not move) |
| `SpawnPerUnit` | 11 | spawn by distance moved | **not simulated** |
| `Collision` | 8 | world collision | **skipped**: particles pass through |
| `VelocityOverLifetime` | 8 | multiplies the velocity by the curve (rotated like `Velocity`), or replaces it with `Absolute` (read) | yes |
| `AttractorLine` | 6 | the particle location relative to the component origin; within `Range` (evaluated at the line parameter) of the segment, velocity gains `(offset from the line × line direction) · Strength · dt`: a swirl around the line, on the transient velocity only (read) | yes |
| `SizeScaleByTime` | 5 | a per-particle clock: set to the spawn time at spawn, advanced by `dt` in every update, then the size's enabled components are multiplied by the curve at it (read; corrected from TENTATIVE) | yes |
| `RotationRateMultiplyLife` | 4 | multiplies the rotation rate in the update (read); its spawn function's effect is undone by the reset | yes |
| `SizeMultiplyVelocity` | 2 | size by speed | not simulated |
| `AttractorPoint` | 1 | position and range at the emitter time; the range is multiplied by the length of the owner scale vector (√3 at unit scale); within it the velocity gains `direction · Strength · dt`, strength by distance or at the emitter time; base velocity too when flagged (read) | yes |

`KillBox` and `KillHeight` are not reached by any shipped system (T). The runtime still implements them as the
executable has them (read): the height (or the two corners) at the emitter time, plus the component origin unless
`bAbsolute`; `KillHeight` scales by the component's Z axis with `bApplyPSysScale` and kills below a floor / above a
ceiling; `KillBox` takes a world-space particle into the component's frame unless `bAbsolute` or
`bAxisAlignedAndFixedSize`, tests strictly inside, and kills inside or outside per `bKillInside`.

The placed systems (27 distinct templates, 367 emitters counting placements) are all world-space sprite emitters with
`PSA_Square` (328) or `PSA_Velocity` (39) alignment and use only: lifetime, size, velocity, colour over life,
location, rotation, size multiply life, rotation rate, orbit, acceleration, cylinder, attractor line, colour, sub-UV,
parameter dynamic, axis lock, velocity over lifetime, rotation rate multiply life, location emitter, sphere,
attractor point and size scale. No placed emitter uses a mesh, a beam, collision or spawn-per-unit.

## Sprites on screen

- **Size is the full quad extent** — CONFIRMED (shader text, re-read in the verification pass). The sprite vertex
  shaders of the shipped OpenGL shader cache (GLSL text; read locally) offset each corner from the particle position
  by `Size.x · (cx − 0.5)` and `Size.y · (cy − 0.5)`, where `(cx, cy)` is the corner's coordinate from a small
  uniform table picked by a per-vertex index, along two camera-plane axes turned by the particle rotation (wrapped
  into ±π). The vertex carries position, old position, size with two flip signs, UV, rotation and colour (68 bytes;
  92 with sub-UV data).
  A square sprite gets `Size.X` for both extents when the vertices are filled (STRONG, `GetVertexAndIndexData`).
- Which camera axis is "right" and the rotation's sense on screen: TENTATIVE (the two axis uniforms are not named in
  the compiled text); it does not matter for the round sprites most effects use.
- Sizes are multiplied by the component scale; an orbit offset is rotated by the component for world-space emitters.
- Materials: 283 of the 367 placed emitters draw additively, 53 alpha-blended, 31 masked. The opacity input is often
  a channel of a *different* texture than the colour (alpha-blended smoke and mist: 43 placed emitters), or the blue
  or RGB channels of the colour texture (85). (Counts re-derived from the converted `materials.json` and particle
  files in the verification pass.) Additive output is colour times opacity (TENTATIVE: stock UE3 base pass; the
  blend state itself is add).
- The simulator hands the renderer the alpha as the engine computes it (it may exceed 1); our sprite shader clamps
  the final opacity to `[0, 1]` (ours).

## Components, `Emitter` actors and Kismet

- `ParticleSystemComponent` values decoded: `Template`, `bAutoActivate`, `InstanceParameters` (`ParticleSysParam`:
  name, `EParticleSysParamType`, scalar/vector/colour/actor/material), `WarmupTime`, `SecondsBeforeInactive`,
  `EmitterDelay`, `HiddenGame`. Of these the placed components store only `Template` and, on 6, `bAutoActivate`
  (independent check); `WarmupTime` and `EmitterDelay` are overwritten from the system at initialization.
- Placements (T): AG-Darkcave 43, AG-ParadiseCave 25, AG-IceCave 22, AG-StarHaven 21, AG-Epilogue 19,
  ASAMUFrontEndMap 12, AG-Workshop 11, AG-BeautifulCity 7, TheCore 2, ASAMUEntry / ASAMULegal / Freds_place 0.
  `bAutoActivate` is false on 6 of the 162. No placement has instance parameters or a base; one (AG-Workshop) is
  bound to a Matinee.
- `Emitter` (script, read locally, re-read in the verification pass; CONFIRMED): `bCurrentlyActive` starts as the
  component's `bAutoActivate`. `OnToggle`: *Turn On* activates the system, *Turn Off* deactivates it, *Toggle*
  activates when spawning is suppressed or the emitter is not currently active, else deactivates. When the system
  finishes on its own (`OnSystemFinished`), `bCurrentlyActive` goes false. That matters for the one-shot effects
  Kismet toggles (AG-Workshop's dome effect, 8 one-loop emitters, and one in AG-ParadiseCave): each *Toggle* after
  the effect ran out starts it again.
- Kismet census (converted graphs): 6 emitters are referenced by level scripts, all under the same object path as
  their placement (A). By a local walk of the graphs (CONFIRMED, not asserted by a test): `SeqAct_Toggle` on 4
  (AG-Workshop 1, AG-ParadiseCave 3), `SeqAct_Destroy` on 2 (AG-BeautifulCity, AG-IceCave), and one Matinee binding
  (the toggled AG-Workshop emitter).
- 51 particle system components live outside levels, all in class default objects; 30 have a template. The game's own:
  `GrappleGun` (grapple beam, beam ignite, power-jump-charged) and `ASAMURocketBoots` (boost trail); the rest are
  stock UT pickups, weapons and flags. Script activates these at run time (the gameplay VFX workstream).
- Activation, deactivation, completion, warm-up: see "The component around it" above (all read in the executable).
- LOD (corrected from TENTATIVE; `DetermineLODLevelForLocation` read in the disassembly). `LODDistances` has 1 or 2
  entries (63 of 100 systems have 2, the largest 2,500) (T). With fewer than two entries the level is 0; otherwise
  the distance from the nearest local player's view point to the component origin is compared with the entries
  from index 1 on, and the level is the one before the first entry greater than the distance, or the last. Entry 0
  is never compared and no LOD bias enters. *When*: `ParticleSystemLODMethod` is `Automatic` (enumerator 0, the
  default; 95 systems) → at activation and again whenever more than `LODDistanceCheckTime` has accumulated;
  `ActivateAutomatic` → at activation only (none shipped); `DirectSet` (5 systems, two of them placed, both with a
  single LOD distance) → never by distance. A component can override the method; no placement does. 267 of 364
  distinct emitters have 2 LOD levels.
- One system uses `EPSUM_FixedTime` (T).

## Importer output

`asamu-import particles` writes, under the user-local `--out` directory only (shared safety checks: no repository
paths except ignored `research/`, no install, no symlinks; existing files kept unless `--force`):

```text
particles/particles.json            format "asamu-particles", version 1
  systems: { "<object path>": System }, component_templates: [...], coverage: {...}
particles/maps/<Map>.json           format "asamu-particle-placements", version 1
  placements: [Placement]

System    = { path, package, export_index, params: {name: Param}, emitters: [Emitter], also_in: [package] }
Emitter   = { path, class, name, kind: sprite|mesh|beam|trail|anim_trail|ribbon|other, params, lods: [Lod] }
Lod       = { path, level, enabled, peak_active_particles, required, spawn, type_data, event_generator, modules: [Module] }
Module    = { path, class, enabled, spawn, update, lod_validity, params: {property name: Param} }
Param     = bool | number | string (name, enumerator, object path) | [Param] | {member: Param} | RawDistribution | null
RawDistribution = { dist: float|vector, object, class, value: Distribution,
                    baked: { op, elements, chunk, len, time_scale, start_time, range: [a, b] } }
Distribution = { kind: constant, value, locked_axes } | { kind: uniform, min, max, locked_axes, mirror, use_extremes }
             | { kind: constant_curve, curve, locked_axes } | { kind: uniform_curve, curve, locked_axes, mirror, use_extremes }
             | { kind: parameter, name, modes, min_input, max_input, min_output, max_output, constant, class }
             | { kind: lookup, op, elements, chunk, table, time_scale, start_time } | { kind: unsupported, class }
Curve     = { dim, keys: [{ t, v, arrive, leave, mode }], broken_tangents?, legacy_method? }
Placement = { actor, actor_path, actor_class, slot, tag?, actor_hidden, base?, moved_by_matinee, location, rotation,
              component, component_class, local_to_world (UE3 row-vector matrix), template, auto_activate, hidden_game,
              kill_on_deactivate, kill_on_completed, warmup_time, seconds_before_inactive, emitter_delay,
              instance_parameters? }
```

Module parameters are the effective tagged properties by their original names (editor-only ones such as editor
colours, thumbnails and preview floors are left out), so the file covers every module class, simulated or not.
`actor_path` is the qualified path Kismet uses for the actor. `particles.json` is about 5.5 MB and deterministic.
A system cooked into several packages is written once, from the first package in file order; the gated importer
test checks that every other copy has the same system values and the same emitters (T). The placement fields
`kill_on_deactivate`, `kill_on_completed`, `warmup_time` and `emitter_delay` are kept in the schema but carry
nothing the game uses (see "The component around it").

`asamu-import all` integration (not wired here; `all.rs` belongs to another workstream): the stage would be
`stage(<id>, Some("particles.json"), true, vec![], crate::particles::run)`; it reads only packages and writes only
`particles/`.

## Runtime (ours)

`crates/asamu-assets/src/particles.rs` (render-agnostic, deterministic):

- typed model of the converted systems, the distribution evaluator (formulas above; curves with the engine's Hermite
  operation order), the engine's random generator seeded per emitter instance, and a simulator of the emitter tick
  and of the component around it as described above: tick order, count limits (and the opt-out flag), component
  transform, instance parameters, LOD rule and methods, warm-up, system delay, fixed-time stepping, activation that
  keeps live particles, deactivation, completion; `SystemInstance::reset` drops everything so that a restarted level
  replays the same particles;
- `SystemInstance::render()` → per emitter the sprites (position, size, rotation, colour, velocity, sub-image) or
  beam segments, in UE units; `beam_source` / `beam_target` let script-driven beams set their ends;
- `ParticleMaterials`: the particle view of `materials.json` — displayed texture and colour multiplier (HDR strength
  kept), opacity mask texture and channel, blend mode.

`apps/asamu/src/particles.rs` (+ `particles/render.rs`, `particles/material.rs`):

- loads the current map's placements (and its merged sub-levels') on the async pool once the level is spawned,
  creates one `SystemInstance` per placement, auto-activates per `bAutoActivate`, advances them in fixed 1/60 s steps
  (paused with the game; a fixed-time system therefore moves as the original does at 60 frames a second), and
  rebuilds one batched quad mesh per emitter each frame: camera-facing with rotation, velocity-aligned, or
  axis-locked; alpha-blended emitters are sorted back to front;
- draws with a small custom unlit material (colour texture × colour × vertex colour, opacity from a mask texture
  channel, additive / alpha / modulate / masked output);
- **Kismet hook** (no change to the router needed): the plugin reads `KismetFrame` itself. `Output::ActorToggled`
  and Matinee `ETTA_*` keys toggle the effects of the matching `actor_path` with the `Emitter.OnToggle` rules;
  `Output::ActorDestroyed` stops them; an effect that finished on its own is no longer "currently active", so the
  next *Toggle* starts it again; toggles that arrive while the effects are still loading are kept and applied once
  they exist; actors in `Presentation::hidden` are not drawn; actors Kismet moves carry their effects
  (`LevelScript::moved_actors`); effects of sub-levels that are not streamed in are not drawn; a new game on the
  same map resets every effect to its level-start state and random sequence.

Approximations (ours, not parity claims): no collision, no spawn-per-unit, no size-by-velocity; mesh emitters as
sprites; beams as straight strips; no soft-particle depth fade, distortion, lighting or fog on particles; sub-UV
blends snap to the nearer image; one colour texture and one mask per material; the final opacity is clamped to
`[0, 1]` in our shader; effects keep simulating while off screen; the LOD distance is taken from our camera;
particle randomness is reproducible per placement but not the original's sequence; a module that is disabled on
one LOD level only shifts the per-particle payload slots (acceleration, orbit, size-scale clock) of that level.

### Local check (2026-10-10; nothing committed, scratch deleted)

A minimal conversion (levels, meshes and textures of AG-Workshop, materials, particles, plus the particle textures
that live in `Startup`) under ignored `research/`:

- **AG-Workshop**, fly camera outside the workshop: the log reports 11 effects (9 active; the two with
  `bAutoActivate` false stay off) and about 3,160 live particles after ten seconds; the screenshot shows the
  outside snow as small soft white flakes scattered through the air in front of the rocks and the observatory
  tower, larger nearby and smaller far away, drifting down. Before the snow texture was converted the same flakes
  drew as neutral squares; before the mask channel was honoured they were brighter and blockier.
- **AG-Darkcave** (scene and the seven particle materials' textures only), camera at one of the water-splash
  emitters: 43 effects, about 2,500 live particles; the alpha-blended mist draws as a dense cloud of small soft
  discs. With the stock material path (texture alpha only) the same emitters were large overlapping rectangles;
  that is what the custom material's separate opacity mask fixed.
- Gameplay mode on AG-Workshop loads the same effects next to the level's Kismet and NPCs. No toggle fires at level
  start there; the hook itself is tested with synthetic frames and the path check above.

The two paragraphs above were written before the verification pass changed the simulator and were not repeated on
screen. Headless, with the corrected simulator (`converted_placements_simulate`, (A): every placement of every map,
auto-activated ones only, LOD 0, ten seconds at 60 Hz, everything finite and inside the per-emitter limits, a
second run identical): AG-Workshop 11 placements (9 active) and 3,156 live particles, AG-Darkcave 43 and 2,420,
AG-Epilogue 19 (18) and 5,458, AG-IceCave 22 and 3,987, ASAMUFrontEndMap 12 (10) and 3,641, AG-StarHaven 21 and
2,157, AG-ParadiseCave 25 (24) and 871, AG-BeautifulCity 7 and 640, TheCore 2 and 35.

## Independent check (verification pass, 2026-10-10)

A second decoder, written in Python for this pass and kept local (git-ignored scratch, deleted afterwards): the
earlier verifier's package reader (summary, liblzo2 stream, tables) plus its own class hierarchy from the class
exports' super indices, object prelude, tagged-property walker, archetype and class-default merge (member-wise for
tagged structs), distribution decoder and a double-precision `FInterpCurve` evaluator. Results over the same 39
packages, all equal to the Rust decoder's:

| | Python | Rust |
|---|---:|---:|
| particle exports / ending exactly at `SerialSize` / payload bytes | 10,978 / 10,978 / 2,541,859 | same |
| packages with particle exports, and every per-package count | 15 | same |
| classes by root (module / float / vector / component / emitter / system / LOD), only as default object | 131 / 9 / 8 / 4 / 2 / 1 / 1, 100 | same |
| systems / distinct / emitters / LOD levels / modules / raw distributions (without object) | 110 / 100 / 396 / 667 / 6,343 / 8,801 (0) | same |
| emitter kinds, distribution kinds (all nine counts) | sprite 294, mesh 76, beam 26; … | same |
| baked tables: agree / only with unscaled tangents / other | 3,916 / 179 / 6 | same |
| curves reached, with `InterpMethod` stored as `None` | 820 / 381 | same |
| components in levels (per map), `bAutoActivate` false; outside levels, with a template | 162, 6; 51, 30 | same |

Against the importer's `particles.json` (the 100 written systems): the emitter, LOD and module structure and
classes, the three module flags, **8,352 distributions** (kind, every value, lock / mirror / extremes flags, every
curve key with tangents and mode, the legacy-method flag, the baked header) and **15,328 scalar module parameters**
were compared value by value: **0 differences**. The 6 tables that disagree with their object were confirmed to
hold exactly the values the object stores (the table is the stale side).

Also re-derived in this pass: both config keys and the files they are in; the two divisions that give 485 and 358
(disassembly); the generator's constants; the Kismet census (4 toggles, 2 destroys, 1 Matinee binding, by walking
the variable links of the converted graphs); the material blend counts. Not re-read: the two baking constants
(0.05, 20) of the lookup tables.

## Hostile-input discipline

Decoder: archetype chains at most 16 links (cycles end there with a note), at most 256 emitters, 16 LOD levels and
256 modules per list, 4,096 curve keys, 65,536 table entries; arrays whose element type is unknown (a package set
without the script classes) are re-read by name with every count checked against the tag's bytes;
`tests/particle.rs` truncates every payload of a synthetic system at every third offset, applies 1,500 random
mutations, and builds archetype cycles. A system may list one emitter, LOD level or module many times (the list
bounds alone allow 256 × 16 × 256 uses of one module), so its decoded size is not bounded by the package's size:
each system is decoded against a budget of 32 MiB of emitter, LOD, module and distribution payload, counted per
use, and stops listing modules (with a note) when it is spent; at most 512 notes are kept per system (added in the
verification pass, with a test that builds such a package). Runtime: every count from JSON is bounded (systems,
emitters, LOD levels, modules, curve keys, table entries, bursts, placements per map, instance parameters), missing
or malformed values take the UE3 zero defaults, non-finite transforms fall back to identity, long steps are split,
warm-up is capped at 30 s and 1,024 ticks, and the simulator is bounded by the engine's own limits plus a hard
1,024 particles per emitter.

## UNKNOWN / not done

- The original's on-screen sprite orientation (which axis is right, rotation sense), `OrientationAxisLock`, the
  vector uniform curve, which particles a draw-count limit drops, what a forced-inactive component skips:
  TENTATIVE as labelled, none read to the end.
- The cylinder module's surface and clamp branches were read only as far as the decompiler output is legible
  (STRONG for the shape described above); `AttractorPoint`, the beam modules and the mesh rotation modules were not
  re-read in the verification pass.
- Collision, spawn-per-unit, mesh particles (static meshes per particle), beam noise/tangents/taper, trails,
  `LocationEmitter` selection, particle events, dynamic material parameters, `SecondsBeforeInactive`,
  `DistributeOverNPoints` of the location module, the sub-UV module's real-time option (none shipped sets it).
- The exact random sequence of the original (global seed shared with the whole game).
- Script-spawned effects (grapple beam, rocket boots, checkpoint, power jump): the systems are converted and the
  runtime can run them, but nothing here spawns them; that is the gameplay VFX workstream.
- Behavioural parity (counts, timing, look) against the original needs captures of the original game.
