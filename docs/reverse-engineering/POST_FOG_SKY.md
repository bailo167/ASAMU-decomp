# Post-processing, fog and sky

What each map uses for its atmosphere (height fog, fog volumes, post-process settings and volumes, the
post-process chain, sky and lights), how the original engine evaluates it, and how the runtime approximates it.

Evidence sources (all local and read-only, Steam build 1822049, Mac install):

- the 12 map packages and `Startup.upk` / `Engine.u`, decoded by our own code (`asamu-import levels`,
  `asamu-inspect props/defaults/class`);
- the game config (`ASAMU/Config/DefaultEngine.ini`, `Engine/Config/BaseEngine.ini`,
  `ASAMU/Config/DefaultSystemSettings.ini`);
- the unstripped Mac executable, read locally with Ghidra (`tools/ghidra-scripts/DecompileToLocal.java`, output in
  ignored `research/`), plus `objdump` for a few instruction sequences and their float constants;
- the GLSL text of the global shaders in `CookedMac/GlobalShaderCache-PC-OpenGL.bin` (read locally; the shader
  sources in `Engine/Shaders/Binaries/*.bin` are encrypted and were not used).

No decompiled code, shader text or scene dump is reproduced here; the maths is described in our own words.
Function names and addresses identify where each behaviour was read.

Reproduce:

```sh
cargo run --release -p asamu-import -- --out <local dir> levels          # scenes with the `atmosphere` block
cargo run --release -p asamu-inspect -- props <map>.asamu <object path>   # component values
cargo run --release -p asamu-inspect -- defaults --inherited Engine.u ExponentialHeightFogComponent
cargo test -p asamu-import --release levels::tests::atmosphere_matches_the_original_data   # skips without data
cargo test -p asamu-import --release -- --ignored every_map   # every map: scene bytes unchanged by the block
cargo test -p asamu post::                                                # UE3 rules and mapping maths
```

## 1. Census — CONFIRMED (scene export of every map)

| Map | Exponential height fog | Fog volumes | Post-process volumes | Directional lights | World LUT |
|---|---:|---|---:|---|---|
| AG-Workshop | 0 | 2 constant density | 2 (room-sized) | 1 dominant | none |
| AG-ParadiseCave | 1 | 1 constant | 2 (level-wide + 1 small, priority 1) | 0 | `LUT_Night` |
| AG-BeautifulCity | 1 | 1 constant | 1 (level-wide) | 0 | `LUT_Night` |
| AG-Darkcave | 0 | 7 linear half-space | 1 (level-wide) | 0 | `LUT_Night` |
| AG-StarHaven | 1 | 1 constant (disabled) | 1 (level-wide) | 1 dominant + 3 | `LUT_Daytime` |
| AG-IceCave | 1 | 1 constant + 4 linear half-space | 0 | 1 dominant | `LUT_Daytime` |
| AG-Epilogue | 0 | 2 constant | 2 (room-sized) | 1 dominant | none |
| TheCore | 0 | 0 | 1 (small) | 0 | none |
| ASAMUFrontEndMap | 0 | 2 constant | 2 | 1 dominant | none |
| Freds_place, ASAMUEntry, ASAMULegal | 0 | 0 | 0 | 0 | none |

No map contains a legacy `HeightFog` or a `SkyLight`, and no `WorldInfo` sets `WorldPostProcessChain`. One volume
sets `bOverrideWorldPostProcessChain`: the small priority-1 volume of AG-ParadiseCave (section 4). Sky domes are
ordinary static meshes with unlit materials (e.g. a `SkySphere` mesh in AG-Workshop, AG-ParadiseCave, AG-BeautifulCity).
`WorldInfo.bFogEnabled` / `FogStart` / `FogColor` are set in some maps; in this engine generation those fields
belong to the mobile renderer's fog (TENTATIVE: the desktop GL path draws height fog through the shaders below and
nothing in it reads these fields) and are not used. `WorldInfo.LightmassSettings.EnvironmentColor` only feeds
the lightmap bake.

## 2. Exponential height fog

### Data — CONFIRMED

The fog parameters live on the `ExponentialHeightFogComponent` subobject, which the scene's actor list does not
describe; `asamu-import levels` now exports its effective values (own values over the archetype / class defaults)
in the scene's `atmosphere.height_fogs`. Class defaults of `Engine.ExponentialHeightFogComponent`: `FogDensity`
0.02, `FogHeightFalloff` 0.2, `FogMaxOpacity` 1, `LightTerminatorAngle` 45, `OppositeLightColor`
(177, 208, 255), `OppositeLightBrightness` 0.2, `LightInscatteringColor` (245, 212, 41),
`LightInscatteringBrightness` 1, `bEnabled` true.

| Map | FogDensity | FogHeightFalloff | FogMaxOpacity | StartDistance | LightTerminatorAngle |
|---|---:|---:|---:|---:|---:|
| AG-StarHaven | 0.02 | 0.2 | 1 | 1024 | 50 |
| AG-IceCave | 0.02 | 0.45 | 1 | 1024 | 50 |
| AG-ParadiseCave | 5.0 | 1.0 | 0.7 | 1 | 30 |
| AG-BeautifulCity | 0.04 | 0.6 | 1 | 0 | 45 |

(`FogHeight` is the actor's height; colours and brightnesses differ per map.)

### Evaluation — CONFIRMED

Read in `FExponentialHeightFogSceneInfo::FExponentialHeightFogSceneInfo` (@0x10056a540),
`FSceneRenderer::InitFogConstants` (@0x1003baf40), `TExponentialHeightFogPixelShader<…>::SetParameters`
(@0x1003bfe40) and the GLSL of `TExponentialHeightFogPixelShader<MSAASF_NoMSAA>`:

- Scene info: density and falloff are divided by 1000 (float constant 1000.0 @0x101692480); each colour is
  converted to linear and multiplied by its brightness. The conversion is `FLinearColor(const FColor&)`, a lookup
  in `FLinearColor::PowOneOver255Table` (@0x1022f4490, initialised data): all 256 entries equal `pow(i/255, 2.2)`
  rounded to `f32` (re-read from the executable; CONFIRMED), alpha is `A/255`.
- Per view: the density at the eye is `density · 2^(−falloff · (eyeZ − FogHeight))`.
- The terminator angle becomes an exponent `E = −0.30103 / ln(x)` with `x = 0.5 − 0.5·min(cos θ, 0.99999)`
  (`x = 0.999995` when `cos θ < −0.99999`). The code uses the natural logarithm with a log10(2) numerator
  (constants @0x10168ffec…0x10168fff8; the call at 0x1003bb543 goes to the `_logf` stub although `_log10f` is also
  imported); we reproduce it as it is.
- The fog vector handed to the shader is a light direction negated, from the first light of one particular type
  in the scene's light list, or `(0, 0, 1)` when there is none. Which type: TENTATIVE (we use the first enabled
  dominant directional light, else any directional light — the fog is then brighter towards the sun, which matches
  the colour names).
- Pixel shader, for the vector `R` from the eye to the pixel (length `d`, vertical part `z`, with `|z| ≤ 0.01`
  replaced by 0.01): line factor `(1 − 2^(−falloff·z)) / (falloff·z)`; integral = eye density × line factor ×
  `max(d − StartDistance, 0)`; scene transmittance `t = max(clamp(2^(−integral)), 1 − FogMaxOpacity)`;
  colour weight `w = |0.5 − 0.499 · dot(fog vector, R/d)|^E`; fog colour = inscattering colour blended towards
  the opposite colour by `w`; output = scene · t + colour · (1 − t), applied to the HDR scene before bloom.
  Pixels with nothing drawn see the far plane, so rising rays end with a finite amount of fog and the others
  fill up to the max opacity.

### Runtime mapping (ours)

Bevy's `DistanceFog` only depends on distance, so it cannot reproduce the height falloff (fog that thickens below
the camera, a clear sky above). The runtime draws the fog as its own full-screen pass (`apps/asamu/src/post/
height_fog.wgsl`, reference maths and tests in `post/ue3.rs`, `HeightFog`) with exactly the evaluation above, on
the player camera and the screenshot camera; it replaces the old placeholder `DistanceFog`. Our approximations:

- colours are scaled from UE3 scene units to render radiance by `lux_per_brightness · exposure / π` (the factor
  the light and lightmap mapping implies; `post.rs`, `render_per_ue`);
- pixels with nothing drawn use a ray of 10⁹ UU (effectively infinite);
- with MSAA the depth of sample 0 is used; translucent surfaces receive the fog of what is behind them (UE3 fogs
  translucency per vertex).

## 3. Fog volumes — data CONFIRMED, rendering not done

Fog volume actors (`FogVolumeConstantDensityInfo`, `FogVolumeLinearHalfspaceDensityInfo`) carry a density
component and an automatic mesh component (`EngineMeshes.Cube`, whose bounds are ±128 UU — meshes manifest)
scaled to the volume. The export (`atmosphere.fog_volumes`) has the component values (`Density`; or
`HalfspacePlane` + `PlaneDistanceFactor`; `bEnabled`, `MaxDistance`, `ApproxFogLightColor`,
`bAffectsTranslucency`), the fog material's own vector/scalar parameters and the mesh matrix. All seven
AG-Darkcave volumes are linear half-space volumes with a horizontal plane at their own height,
`PlaneDistanceFactor` 0.1 and a black `EmissiveColor`: dark mist that thickens below each plane. The stored plane
has normal (0, 0, 1) and `W` = −(actor height) (all seven), i.e. it is kept for an `n·x + W` evaluation, not as
UE3's usual `n·x = W` (sign convention CONFIRMED from the data; how the shader uses it UNKNOWN). The AG-StarHaven
volume is disabled.

How the original integrates the density is in material-based shaders (`TFogIntegralPixelShader<…Policy>`,
`FFogVolumeApplyPixelShader`, whose code lives in the material shader caches, not the global one): UNKNOWN. The
runtime does not draw fog volumes yet.

## 4. Post-process settings and volumes

### Data — CONFIRMED

`WorldInfo.DefaultPostProcessSettings` and `PostProcessVolume.Settings` / `Priority` / `bEnabled` are in the actors'
`params` (effective values), the volume's convex hulls in its `volume` (one box hull per shipped volume, planes
`n·p = w` with outward normals). No shipped volume stores its own `bEnabled` (the class default is true) and only
AG-ParadiseCave's small volume stores a `Priority` (1); the others use the class default 0. Struct defaults of
`PostProcessSettings` (class data): bloom on (scale 1, threshold 1, screen-blend threshold 10, white tint), DOF
off, motion blur on (0.5), scene effect on (desaturation 0, highlights 1, midtones 1, shadows 0, colorize 1,
tonemapper scale 1), every `*_InterpolationDuration` 1, and `bOverride_*` flags true except the DOF minimum and
bokeh, all motion-blur values, `Scene_Colorize`, `Scene_TonemapperScale`, `Scene_ImageGrainScale`,
`Scene_ColorGradingLUT` and the mobile blocks.

World settings of note: AG-ParadiseCave / BeautifulCity / Darkcave use `LUT_Night`, StarHaven / IceCave
`LUT_Daytime` (`bOverride_Scene_ColorGradingLUT`); all five set `Bloom_ScreenBlendThreshold` 50000, and the three
cave maps also `Bloom_Threshold` 0; StarHaven `Bloom_Scale` 0.8, IceCave 0.5; ParadiseCave turns bloom off;
`Scene_ImageGrainScale` 0.01 where a LUT is set; `DOF_BlurBloomKernelSize` 200. The level-wide volumes then
override: Darkcave `Bloom_Scale` 0.3 and `Scene_HighLights` (1, 1, 0.7); StarHaven `Scene_MidTones`
(1.25, 1.3, 1.3); BeautifulCity `Bloom_Scale` 0.3 and highlights (1, 1.1, 1); ParadiseCave turns bloom back on
(`bEnableBloom` is a struct default with its override flag set) with `Bloom_Scale` 0.4 and highlights
(1, 0.85, 1), and its small priority-1 volume has `Bloom_Scale` 0.5, `Scene_Desaturation` 0.3 and the chain
override; and, because a volume's struct defaults have their override flags set, every one of them also sets
`Bloom_Threshold` 1 and `Bloom_ScreenBlendThreshold` 10. (Independent re-decode of AG-StarHaven and AG-Darkcave,
section 6: their volumes' own `Settings` hold only the values named here; the threshold values come from the class
default object of `Engine.PostProcessVolume`, whose `Settings` has `Bloom_Threshold` 1, `Bloom_ScreenBlendThreshold`
10 and both override flags true.)

The AG-StarHaven player start lies about 4,350 UU outside its level-wide volume (start Y −176,468, box
Y ≥ −172,121): at the start the world settings apply, and the volume's settings blend in once the player is
inside. Whether the original's box is the same is CONFIRMED only as far as our brush export goes (an axis-aligned
box centred on the actor, no rotation, scale 1).

`bOverrideWorldPostProcessChain` — CONFIRMED (native, `ULocalPlayer::UpdatePostProcessSettings` @0x100afd700,
`ULocalPlayer::CalcSceneView` @0x100af5090, `UEngine::GetWorldPostProcessChain` @0x10083ff60): when the applied
volume has the flag, the view is rendered with the engine's default chain (`DefaultPostProcessName`) instead of the
local player's own chain; otherwise the player's chain. `GetWorldPostProcessChain` returns `WorldInfo.
WorldPostProcessChain`, else the default chain; that the player's chain is built from it plus the chains script
inserts is STRONG (`RebuildPlayerPostProcessChain` / `InsertPostProcessingChain` exist; not read). No map sets a
world chain, so both are the same default chain unless script changed the player's chain: the expected effect is
none (TENTATIVE:
the ASAMU gameplay classes checked with `asamu-inspect calls` — controller, camera, pawn, game info, HUD, viewport
client, grapple gun, rocket boots, worm — call no chain functions; the stock UTGame classes were not checked). The
runtime ignores the flag.

The two LUT textures are 256 × 16 `PF_A8R8G8B8` (`TEXTUREGROUP_ColorLookupTable`, one mip): 16 slices of 16 × 16,
texel `(b·16 + r, g)` (re-checked on the converted DDS files: node (15, 0, 0) at texel (15, 0) is red in
`LUT_Night`, (0, 15, 0) at (0, 15) green, (0, 0, 15) at (240, 0) blue). `LUT_Night` lifts and blue-tints the
mid-tones strongly (mid grey 136 → (159, 177, 186)); `LUT_Daytime` is close to neutral with slightly desaturated
primaries.

### Which volume applies — CONFIRMED

`AWorldInfo::GetPostProcessSettings` (@0x1008f0760): walk the world's volume list from the highest priority and
take the **first enabled volume that encompasses the view point**; start from the persistent level's
`DefaultPostProcessSettings` and let that one volume override it (`FPostProcessSettings::OverrideSettingsFor`,
@0x100b06200, weight 1). Only one volume applies; there is **no blend radius or distance weighting** in this
engine. The list is ordered by `APostProcessVolume::UpdateComponentsInternal` (@0x100b06080): a volume is inserted
before the first one of strictly lower priority, so equal priorities keep their registration order.

`OverrideSettingsFor`: a group toggle (`bEnableBloom`, `bEnableDOF`, `bEnableMotionBlur`, `bEnableSceneEffect`,
`bAllowAmbientOcclusion`) is copied when its override flag is set; then, only while the destination group is
enabled, each value whose override flag is set moves to the volume's value (by the weight) and its override flag
is set in the result. The bloom tint is blended in linear colour; the LUT reference is copied.

### Time blend — CONFIRMED

`ULocalPlayer::UpdatePostProcessSettings` (@0x100afd700) / `UpdatePPSetting` (@0x100afe120): when the applied
volume changes, a blend restarts (real time). Each frame, for every enabled group of the desired settings, the
current values move towards the desired ones by `Δ / max(duration − elapsed, 0)` (1 once the remaining time is
shorter than the frame), `duration` being that group's `*_InterpolationDuration` — a linear blend over the
duration. Toggles switch at once. On a map change the durations are zeroed (snap) only when the previous world
did not set `bPersistPostProcessToNextLevel` (every shipped map sets it). Camera modifiers and script/Matinee
overrides are applied after this and are not modelled.

## 5. The post-process chain and the uber effect

### Chain — CONFIRMED

`DefaultEngine.ini [Engine.Engine] DefaultPostProcessName=FX_HitEffects.UTPostProcess_PC` (overriding
`EngineMaterials.DefaultScenePostProcess` from `BaseEngine.ini`). On the Mac the config chain is
`ASAMU/Config/Mac/MacEngine.ini` → `Engine/Config/Mac/MacEngine.ini` → `ASAMU/Config/DefaultEngine.ini` →
`BaseEngine.ini` (`[Configuration] BasedOn`); neither Mac file sets the key. The importer reads a fixed list
(`ASAMU/Config/Mac/MacEngine.ini`, `DefaultEngine.ini`, `BaseEngine.ini`, first file that sets the key wins) rather
than following `BasedOn`, which gives the same answer for this install. The chain (in `Startup.upk`) holds, in order:
two `MaterialEffect`s (`UDK_LUT.M_Vingette_INST`, `UDK_LUT.FilmGrain.M_FilmGrain_INST`), an
`UberPostProcessEffect`, a `MaterialEffect` named `HitEffect`, and an `AmbientOcclusionEffect`. The three material
effects have `bShowInGame` false. The uber effect: `TonemapperType` `Tonemapper_Customizable`,
`TonemapperToeFactor` 0.5, `PostProcessAAType` FXAA3, `BloomWeightSmall` 2, `BloomWeightLarge` 4,
`BloomSizeScaleMedium` 0.75, `MotionBlurAmount` 0.3, `bUseWorldSettings` true, other values the class defaults
(`TonemapperRange` 8, `TonemapperScale` 1, and its own scene transform: shadows (0, 0, −0.003), highlights 0.8,
midtones 1.3, desaturation 0.4). The AO effect uses world settings (angle-based SSAO, radius 20). The export
puts the chain with each effect's effective values in `atmosphere.post_process_chain`.

### Effect parameters — CONFIRMED

`FDOFAndBloomPostProcessSceneProxy` / `FUberPostProcessSceneProxy` constructors (@0x100385c00, @0x1006946b0):
with world settings, each parameter comes from the blended settings when its override flag is set, otherwise from
the effect's own property. Hence with the world struct defaults the scene transform is the settings' identity
(overrides on) but `Colorize`, `TonemapperScale` and image grain come from the effect unless overridden. Bloom is
zeroed when the settings override bloom off; the scene transform becomes the identity when scene effects are off;
desaturation is clamped to 0..1. Override-flag bit order (bits 0–46, a 64-bit field at the start of the struct):
the `bOverride_*` flags in declaration order, then the five group toggles and `bOverrideRimShaderColor`.

### Bloom — CONFIRMED (original), approximated (runtime)

Gather (`FBloomGatherPixelShader`): each of four samples is multiplied by a scene scale, weighted by
`saturate((max(r, g, b) − Bloom_Threshold) / 2)`, summed, multiplied by a second scale and by 1/16 and clamped to
0..1: the bloom buffer holds a quarter of the weighted average, so it saturates at 4. Blend
(`FUberPostProcessBlendPixelShader…`): scene + blurred bloom × `Bloom_Tint` × `Bloom_Scale` × 4 × a screen-blend
factor `saturate(2^(−3 · luminance(scene)) · Bloom_ScreenBlendThreshold)` (bloom fades on bright pixels when the
threshold is small; with 50000 it never does). The ×4 undoes the gather's quarter, so the net gain of the bright
parts is 1 times the two scales.

Runtime: Bevy's bloom supplies only the blurred bright parts (additive, prefilter threshold `Bloom_Threshold` in
render units, Bevy's default low-frequency boost, intensity solved so that its total gain over the mip chain is 1:
`post.rs`, `bevy_bloom`, `bloom_total_gain`; the gather's quarter and the blend's ×4 cancel, so neither appears,
and the saturation of the original's bloom buffer at 4 is not reproduced). The fog pass also writes a copy of the
scene before Bevy's bloom;
the uber pass takes the bloom as the difference and blends it exactly as the original's blend shader does: × tint
× scale × the screen-blend fade (`post/ue3.rs`, `bloom_blend`, and `uber.wgsl`). Not reproduced: the original's
three-size Gaussian kernel and its `/2` soft extraction ramp (Bevy's knee differs), and the gather's own scale
constants (UNKNOWN; we assume 1).

### Tonemapper — CONFIRMED (constants), STRONG (variant)

`RenderVariationFullRes` (@0x1006a0f60) sets, with `S = max(TonemapperScale, 1e-6)` and `R = TonemapperRange`:
`A = 0.22 / S`, `B = (R + A) / R`, crossover `c0 = sqrt(A·B / S) − A`. The blend-shader variant with a toe (the
one fed by `TonemapperToeFactor`; we take it to be the `Tonemapper_Customizable` variant, STRONG) maps each
channel: curve `B·x / (x + A)`; toe `(x·S)^(1/2.2)`; use the toe below `c0` and the curve above (a hard switch);
then blend towards the pure curve by the toe factor; saturate. The result is display-gamma encoded.

### Colour-grading LUT — CONFIRMED (STRONG for the final gamma step)

`FLUTBlender` / `SetLUTBlenderShader` / `FColorRemapShaderParameters::Set` (@0x10022a4b0) and the
`FLUTBlenderPixelShader<N>` GLSL: the final 16³ LUT is built per entry from the weighted LUT textures (the
settings' LUT at weight 1, else the neutral LUT), then **subtract shadows and clamp, divide by highlights, raise to
the midtones exponent, desaturate towards luminance (0.30, 0.59, 0.11) by the desaturation, multiply by the view's
colour scale and add its overlay (camera fades), multiply by colorize**, then raise to `2.2 / display gamma` (1 at
the configured 2.2). The uber shader looks the tonemapped (gamma-space) colour up in it: blue picks two adjacent
slices (index from `b · 14.9999`, linear blend), red/green are filtered within a slice. The scene colour transform
is therefore applied in display-gamma space.

### Runtime mapping (ours)

`apps/asamu/src/post/uber.wgsl` runs after Bevy's bloom and replaces Bevy's tonemapping on the affected cameras
(the player camera and the offscreen screenshot camera, switched to HDR): render radiance → UE3 units, the bloom
blend above, the tonemapper, the graded LUT (built on the CPU each time the transform or LUT changes:
`post/ue3.rs`, `Lut::graded`, uploaded as a 256 × 16 texture), then sRGB decoding so that the swap chain shows
exactly the original's display values.

Render radiance → UE3 units uses one factor, `lux_per_brightness · exposure / π` (≈ 3.2 with the default light
mapping and exposure): right for surfaces lit by the converted lights and lightmaps, but **unlit / emissive
materials (sky domes, glowing props) are already in UE3 units**, so they come out about 3× darker than they
should. Fixing that needs a per-material scale in the level renderer (`converted.rs`, not part of this work).

The first-person overlay camera used to draw into the player camera's intermediate texture; with the player
camera in HDR the two no longer share it, so the plugin switches overlay cameras (children of the player camera
with a later order and no clear) to clear to transparent and blend over the window with premultiplied alpha.
Checked on a window capture: scene, hands and HUD composite correctly.

`ASAMU_POST=off` restores Bevy's default look for comparisons; `ASAMU_POST=no-fog,no-bloom,no-lut,no-tonemap`
leaves out single parts; `--no-fog` disables the fog pass. When no atmosphere applies to the current level (it is
still loading, failed to load, or another level is current) the plugin takes its components off the cameras and
gives back Bevy's tonemapping, so another level's fog and grade never linger. A scene whose `atmosphere` block has
an unexpected shape still loads (the block is ignored and the reason logged). The uber pass is skipped on frames
where the fog pass cannot run (pipeline still compiling), since it needs the fog pass's pre-bloom copy.
The first-person overlay camera keeps Bevy's own tonemapping (only the player and screenshot cameras get the UE3
tonemapper and LUT).

Not rendered: fog volumes (section 3), DOF (off in every map's settings), motion blur, SSAO, film grain, FXAA, the
hidden material effects, the view's colour scale / overlay (camera fades, script and Matinee post-process
overrides), LUT blending over time (one LUT per level).

## 6. Local verification

AG-Darkcave and AG-StarHaven converted locally (levels, meshes, textures, materials, lightmaps, the hand mesh) and
rendered with `--fly --screenshot` (offscreen camera) before (`ASAMU_POST=off`) and after, plus a window capture
with gameplay and the hands; images stay local and were deleted. No render validation errors.

- **AG-Darkcave** (no height fog; `LUT_Night`; level-wide volume: highlights (1, 1, 0.7), bloom 0.3, threshold 1).
  Before: neutral-to-warm colours from Bevy's tonemapper (orange stepping stones, green foliage). After: a cold
  moonlit grade — the night LUT and the volume's blue highlight boost turn mid-tones blue-grey, crystals and
  glowing props bloom blue, overall darker. The seven half-space fog volumes (black mist in the pits) are not
  drawn yet.
- **AG-StarHaven** (height fog with `FogHeight` far below the islands, `LUT_Daytime`, volume midtones
  (1.25, 1.3, 1.3), bloom 1 with screen-blend threshold 10; the player start is just outside the volume,
  section 4, so a view from the start uses the world settings: bloom 0.8, screen-blend threshold 50000).
  Before: white-washed snow, no fog. After: below the
  islands the view sinks into a dense blue fog sea (the opposite-light colour; density grows exponentially below
  the fog height, so looking down fills up while horizontal views stay mostly clear — the behaviour
  `DistanceFog` cannot give); sunlit faces render warm and saturated (the per-channel UE3 curve keeps hues that
  Bevy's tonemapper desaturated); bloom halos appear around bright islands over darker backgrounds and fade on
  the bright surfaces themselves. The sky dome renders too dark (unlit-material scale, section 5).

Independent re-check (verification pass, 2026-10-10; throwaway scripts under ignored `research/local/`, deleted):

- A separate Python decoder (its own tagged-property and prelude decoding following `OBJECT_FORMAT.md`; only the
  summary/table reader of the earlier independent verifier reused) re-decoded the fog components, post-process
  volumes and `WorldInfo` of AG-StarHaven and AG-Darkcave. Merging each component's own tags over its archetype
  template in `Engine.u` and the class defaults (`asamu-inspect defaults --inherited`) gives exactly the exported
  `atmosphere` values for all 9 fog components (2 + 7; every key, float bit patterns included); the volumes' and
  `WorldInfo`'s own `Settings` members equal the exported effective values. Own tags: StarHaven's height fog stores
  7 values (`FogHeight`, both brightnesses and colours, `LightTerminatorAngle`, `StartDistance`); `FogDensity`,
  `FogHeightFalloff` and `FogMaxOpacity` are class defaults.
- Census, fog table, LUT paths and the settings above re-derived from a fresh export of all 12 maps.
- The scene part of every map's file is byte-identical to the scene serialised alone (compact and pretty), with
  `atmosphere` appended as the last key (`every_map_keeps_its_scene_bytes`).
- Native constants and calls re-read with `objdump` (1000.0, 0.99999, 0.999995, −0.30103, the `_logf` call, the
  colour table); fog, gather and uber-blend GLSL re-read; `OverrideSettingsFor`, `GetPostProcessSettings`,
  `UpdatePostProcessSettings`, `UpdatePPSetting` (step = time since the previous update over the duration minus the
  blend time elapsed up to that update, so the frame of a volume change already moves by its frame time),
  `GetWorldPostProcessChain` and `CalcSceneView` re-decompiled locally; all agree with sections 2–5.

Whether these match the original's screen exactly is UNKNOWN until reference captures of the original exist
(`docs/TRACE_CAPTURE.md`); every formula above is CONFIRMED or labelled, the unit conversion between our lighting
and UE3's is ours.
