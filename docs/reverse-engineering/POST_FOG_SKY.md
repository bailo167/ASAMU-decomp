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
- The fog vector handed to the shader is a light direction negated: that of the first light in the scene's light
  list whose type is 3, or `(0, 0, 1)` when there is none. Type 3 is the dominant directional light (CONFIRMED:
  the value `UDominantDirectionalLightComponent::GetLightType` returns; an ordinary directional light is 2). So a
  level without a dominant directional light — AG-ParadiseCave, AG-BeautifulCity — has its fog brightest straight
  up, towards the inscattering colour, and darkest downwards. The runtime does the same (it used to fall back to
  an ordinary directional light; no shipped map with height fog has one without a dominant light, so no picture
  changed).
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
the player camera and the screenshot camera. Our approximations:

- colours are scaled from UE3 scene units to render radiance by `lux_per_brightness · exposure / π` (the factor
  the light and lightmap mapping implies; `post.rs`, `render_per_ue`);
- pixels with nothing drawn use a ray of 10⁹ UU (effectively infinite);
- with MSAA the depth of sample 0 is used; translucent surfaces receive the fog of what is behind them (UE3 fogs
  translucency per vertex).

## 3. Fog volumes

### Data — CONFIRMED

Fog volume actors (`FogVolumeConstantDensityInfo`, `FogVolumeLinearHalfspaceDensityInfo`) carry a density
component and an automatic mesh component (`EngineMeshes.Cube`, whose bounds are ±128 UU — meshes manifest)
scaled to the volume. The export (`atmosphere.fog_volumes`) has the component values (`Density`; or
`HalfspacePlane` + `PlaneDistanceFactor`; `bEnabled`, `StartDistance`, `MaxDistance`, `SimpleLightColor`,
`ApproxFogLightColor`, `bAffectsTranslucency`), the fog material's own vector/scalar parameters and the mesh matrix.

| Map | Volume | Density function | Colour (linear) | Box (full size, UU) | Distances (UU) |
|---|---|---|---|---|---|
| AG-Workshop, AG-Epilogue, ASAMUFrontEndMap | `…ConstantDensityInfo_3` | constant, `Density` 3·10⁻⁵ | (0.5, 0.5, 0.7) | 2,582 × 2,582 × 1,024 | max 65,535 |
| | `…ConstantDensityInfo_0` | constant, 7·10⁻⁵ | (0.5, 0.5, 0.7) | 1,466 × 2,873 × 620 | max 65,535 |
| AG-ParadiseCave | `…ConstantDensityInfo_1` | constant, 3·10⁻⁶ | (0.434, 0.7, 0.527) | 235,995 × 208,024 × 56,612 (the whole level) | max 65,535 |
| AG-BeautifulCity | `…ConstantDensityInfo_0` | constant, 10⁻⁶ | (0.148, 0.125, 0.221) | 10,080,180 cubed | start 30,000, max 65,535 |
| AG-Darkcave | seven `…LinearHalfspaceDensityInfo` | half-space, `PlaneDistanceFactor` 0.1 | black | from 3,600 to 413,000 across | max 65,535 |
| AG-StarHaven | `…ConstantDensityInfo_0` | constant, 10⁻⁶, **`bEnabled` false** | (0.7, 0.6, 0.489) | — | — |
| AG-IceCave | `…ConstantDensityInfo_0` | constant, 4·10⁻⁶ | (0.5, 0.5, 0.7) (engine material) | 476,791 × 229,209 × 138,601 | start 20,000, max 65,535 |
| | `…LinearHalfspaceDensityInfo_2` | half-space, 6·10⁻⁶ | (0.5, 0.5, 0.7) (engine material) | | max 650,000 |
| | `…LinearHalfspaceDensityInfo_0`, `_8`, `_4` | half-space, 7·10⁻⁴, 0.03, 4·10⁻⁴ | (0.310, 0.310, 0.597) | | max 65,535 |

- Every volume is shaped by its own cube. `FogVolumeActors` (other actors whose meshes shape a volume) is empty
  everywhere.
- The stored half-space plane has a vertical normal in all eleven cases, and its `W` is minus the actor's height
  (also where the normal points down, `…HalfspaceDensityInfo_4` of AG-IceCave).
- No level script touches a fog volume: no Kismet object and no Matinee track of any map names one (search of the
  converted `kismet/` and `matinee/` data for the actor names; AG-IceCave's Kismet does name its
  `ExponentialHeightFog_0`, which is the height fog, not handled here). The volumes are static: what `bEnabled`
  says at load is what the level shows.
- The actors inherit `bHidden = true` from `Engine.Info`; only AG-ParadiseCave's stores `bHidden = false`. That
  makes no difference in the original (next section).

### What the original does

Read in the unstripped Mac executable (`objdump` of the named functions; addresses below) and in the GLSL of the
fog volume material's shaders in `RefShaderCache-PC-OpenGL.upk` (decompressed locally with `asamu-inspect
decompress`; the apply shader is the one small pixel shader that reads one texture and ends in a base-2
exponential, and the three integral shaders are stored right behind it, one per density policy named in that
cache: constant, linear half-space, sphere).

**The mesh is the volume's bounds, never a surface — CONFIRMED.**
- The mesh component template (`Default__FogVolumeDensityInfo.AutomaticMeshComponent0`) has
  `bIgnoreOwnerHidden = true`, and `UPrimitiveComponent::ShouldComponentAddToScene` (@0x100535390) adds a component
  of a hidden owner exactly when that flag is set. So the cube of every volume is in the scene, whatever the
  actor's `bHidden`.
- `UFogVolumeDensityComponent::Attach` (@0x1003c2390) does nothing more when `bEnabled` is clear. Otherwise it puts
  the fog material on the mesh — after writing the component's `SimpleLightColor` into the parameter
  `EmissiveColor` when the material is an instance — and registers the mesh with the scene as that component's fog
  volume (`FScene::AddFogVolume`), then the meshes of `FogVolumeActors` (`AddFogVolumeComponents`, @0x1003c1e90).
- `RenderFogVolume` (@0x1003c8b00) looks a mesh up among the scene's fog volumes and draws the two steps below.
  That the translucency pass sends every mesh whose material is marked for fog volumes there, and nowhere else,
  is STRONG (the function's arguments are a translucent mesh batch; its caller was not read). What happens to a
  mesh that is not registered (a disabled volume) was not traced: TENTATIVE that nothing is drawn.
- `CreateFogVolumeDensityInfo` makes no volume when the density (`PlaneDistanceFactor` for a half-space) is not
  positive (seen in the half-space variant, @0x1003c3150).
- The shipped configuration has `FogVolumes=True` (`BaseSystemSettings.ini`, the platform file).

**Two steps per volume — CONFIRMED for the blend states and the face signs, STRONG for the shader reading.**
1. *Integral.* The volume's faces are drawn into a buffer of their own with additive blending. The pixel shader
   writes the density integrated from the eye to that face, times −1 for a front face and +1 for a back face (a
   two-entry table, @0x101663ff0), so the buffer ends up with back minus front: the density along the part of the
   view ray that is inside the mesh. From inside the volume there is no front face and the integral starts at the
   eye.
2. *Apply.* The mesh is drawn again; the shader outputs the material's emissive colour and, as alpha,
   `clamp(2^(−integral))`. The blend is `source × (1 − alpha) + destination × alpha`
   (`TStaticBlendState<BO_Add, BF_InverseSourceAlpha, BF_SourceAlpha, …>`): **scene × T + colour × (1 − T)** with
   **T = 2^(−integral)**.

**The integrals.** Both shaders first choose a view depth `d`: the face's own depth, but never more than the
scene's depth (four neighbouring depth samples, each limited to `MaxDistance`, averaged) and never more than
65,535 UU (a constant of the shader, whatever `MaxDistance` is).
- *Constant density*: `Density × max(d − StartDistance, 0)`. The measure is view depth, not ray length: towards
  the screen edges the same depth is a longer ray, and the original ignores that.
- *Linear half-space*: the point at depth `d` on the pixel's ray is rebuilt in world space, and the density is
  integrated along the straight line from the eye to it in closed form: no fog above the plane, and under it a
  density of `PlaneDistanceFactor × 10⁻⁴` per UU of depth under the plane (the shader's constant is −0.00005,
  i.e. −½ · 10⁻⁴; a 10⁻⁴ is added to the divisor so that a level ray stays finite). `StartDistance` is not used.
  A point `p` is `HalfspacePlane.XYZ · p + HalfspacePlane.W` above the plane.
  `UFogVolumeLinearHalfspaceDensityComponent::SetParentToWorld` (@0x1003c30a0) sets the normal to the component's
  normalised Z axis and `W` to minus the origin's height — right for a level plane only (every shipped one is
  level; the one that points down in AG-IceCave therefore has its plane at minus its own height, and we evaluate
  what the engine evaluates). `GetSecondDensityFunctionParameters` (@0x1003c61b0) moves `W` into the view's
  translated space.
- The scene info (`FFogVolumeDensitySceneInfo`, @0x1003c56b0, and the per-class constructors behind it) holds
  `StartDistance`, `MaxDistance`, `bAffectsTranslucency`, `ApproxFogLightColor`, the bounds, and the density
  parameter. `GetMaxIntegral` is the density × 10⁶ (constant) or the factor × 10⁵ (half-space): a scale for the
  stored integral, with no effect on the result.
- Spherical and cone volumes: in no map; not read.

**Order — CONFIRMED.** The height fog pass (`FSceneRenderer::RenderFog`, @0x1003bb6b0) runs on the opaque scene of
the world depth group; fog volumes are translucent primitives and are blended afterwards, over the height-fogged
scene.

**The colour.** The material's `EmissiveColor`. The engine overwrites it with `SimpleLightColor` when a volume
attaches, in actor order, so the last volume on a shared instance decides; in the shipped maps the cooked value of
every instance already is that colour (checked by `fog_volumes::tests::the_real_levels_fog_volumes`), and the two
AG-IceCave volumes on the engine's own material have its default, (0.5, 0.5, 0.7).

### Runtime mapping (ours)

- **No surface.** `asamu-assets` never turns the mesh of a `FogVolume*DensityInfo` actor into a draw
  (`scene.rs`, counted in `stats.fog_volume_meshes`). Before, the actor's `bHidden` decided, which let exactly one
  volume through: AG-ParadiseCave's cube, drawn with its unlit additive fog material. From inside that cube
  nothing of it was visible (a one-sided surface seen from behind); it was a wrong picture waiting for a camera
  outside it.
- **The volume.** `apps/asamu/src/fog_volumes.rs` builds each enabled volume from the scene data
  (`FogVolume::from_info`) and the fog pass (`post/height_fog.wgsl`, prefixed with `fog_volumes.wgsl`) blends them
  per pixel after the height fog, in the original's order: for the pixel's ray, where it enters and leaves the
  volume's box (the box of the mesh's bounds: exact for the cube) stands for the front and back faces, and the
  integrals above are evaluated at those two depths, limited by the scene's depth, `MaxDistance` and 65,535 UU.
  The camera inside a volume, outside it, or in several at once are all the same formula (inside, the front term is
  zero). For a camera inside a constant-density volume this is an exponential distance fog of that density, up to
  the volume's far side. Several volumes are blended one after the other, the farthest centre first (the
  original sorts them as translucent primitives; TENTATIVE where two overlap).
- Colours are scaled like the height fog's (`render_per_ue`). At most eight volumes reach the shader (AG-Darkcave
  has seven); with more, the ones nearest the camera are kept.
- `--no-fog` and `ASAMU_POST=no-fog` turn the volumes off together with the height fog.
- Our approximations: the pixel's own depth instead of the average of four neighbours; translucent surfaces get
  the fog of what is behind them; spherical and cone volumes and `FogVolumeActors` shapes are not drawn (the log
  says so if a scene has one).
- The invented distance fog that used to stand in for the height fog (`converted.rs`, a grey-blue linear ramp
  sized from the level) is gone: fog comes from the level's data only. A scene converted before the `atmosphere`
  block existed has no fog until it is converted again.

### How much fog that is — from the data

Opacity `1 − 2^(−integral)` of the volume's colour:

| Map | Where | Opacity |
|---|---|---|
| AG-ParadiseCave | inside the level-wide volume, by view depth | 0.02 % at 100 UU, 1.0 % at 5,000, 4.1 % at 20,000, at most 12.7 % (65,535) |
| AG-Workshop / AG-Epilogue | through a room volume | 5 % and 13 % along the longest edge, 8 % and 15 % corner to corner |
| AG-BeautifulCity | beyond 30,000 UU | at most 2.4 % |
| AG-IceCave | constant volume beyond 20,000 UU | at most 11.9 % |
| AG-Darkcave | black; 500 UU under a plane, 1,000 UU of level ray | integral 5: 97 % black |

The volumes of the first four are a faint veil in the distance. AG-Darkcave's are what the data says they are:
pits that go black a few hundred UU under each plane.

### The first-person hand — CONFIRMED

The hand is not fogged in the original, and stays unfogged in ours (it is drawn by an overlay camera after the
fog pass). `FSceneRenderer::RenderFog` returns at once for every depth priority group but the world's (its first
comparison), and the hand's mesh component (`asamu.Default__GrappleGun.FirstPersonMesh`) is in
`SDPG_Foreground`, which is drawn after the world group and its translucency, fog volumes included.

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
leaves out single parts; `--no-fog` disables the height fog and the fog volumes. When no atmosphere applies to the current level (it is
still loading, failed to load, or another level is current) the plugin takes its components off the cameras and
gives back Bevy's tonemapping, so another level's fog and grade never linger. A scene whose `atmosphere` block has
an unexpected shape still loads (the block is ignored and the reason logged). The uber pass is skipped on frames
where the fog pass cannot run (pipeline still compiling), since it needs the fog pass's pre-bloom copy.
The first-person overlay camera keeps Bevy's own tonemapping (only the player and screenshot cameras get the UE3
tonemapper and LUT).

Not rendered: DOF (off in every map's settings), motion blur, SSAO, film grain, FXAA, the
hidden material effects, the view's colour scale / overlay (camera fades, script and Matinee post-process
overrides), LUT blending over time (one LUT per level).

## 6. Local verification

AG-Darkcave and AG-StarHaven converted locally (levels, meshes, textures, materials, lightmaps, the hand mesh) and
rendered with `--fly --screenshot` (offscreen camera) before (`ASAMU_POST=off`) and after, plus a window capture
with gameplay and the hands; images stay local and were deleted. No render validation errors.

- **AG-Darkcave** (no height fog; `LUT_Night`; level-wide volume: highlights (1, 1, 0.7), bloom 0.3, threshold 1).
  Before: neutral-to-warm colours from Bevy's tonemapper (orange stepping stones, green foliage). After: a cold
  moonlit grade — the night LUT and the volume's blue highlight boost turn mid-tones blue-grey, crystals and
  glowing props bloom blue, overall darker. (The seven half-space fog volumes, black mist in the pits, were
  not drawn then; they are since section 3's runtime mapping.)
- **AG-StarHaven** (height fog with `FogHeight` far below the islands, `LUT_Daytime`, volume midtones
  (1.25, 1.3, 1.3), bloom 1 with screen-blend threshold 10; the player start is just outside the volume,
  section 4, so a view from the start uses the world settings: bloom 0.8, screen-blend threshold 50000).
  Before: white-washed snow, no fog. After: below the
  islands the view sinks into a dense blue fog sea (the opposite-light colour; density grows exponentially below
  the fog height, so looking down fills up while horizontal views stay mostly clear — the behaviour
  `DistanceFog` cannot give); sunlit faces render warm and saturated (the per-channel UE3 curve keeps hues that
  Bevy's tonemapper desaturated); bloom halos appear around bright islands over darker backgrounds and fade on
  the bright surfaces themselves. The sky dome renders too dark (unlit-material scale, section 5).

**AG-ParadiseCave mist (2026-10-12).** The owner, continuing from the level's first checkpoint, found the cave "so
misty I almost couldn't see": a teal veil over everything, near and far. It was first put down to the level's fog
volume being drawn as a glowing surface. That is not what the frames show. Checked on the Windows machine with the
build that was played (`--level AG-ParadiseCave --fly --camera 147241,-154469,95875,-55,0 --hidden-window
--screenshot`, the eye position of that checkpoint's respawn; offscreen 1600 x 900; mean sRGB of image regions;
images stay local):

| Run | Dark gap between rocks (far) | Lit ground (near) |
|---|---|---|
| as played | (53, 91, 106) | (112, 147, 143) |
| `--no-fog` (the fog pass off; the cube still in the draw list) | (18, 24, 11) | (171, 207, 185) |
| `ASAMU_POST=no-lut,no-bloom` | (41, 54, 75) | (93, 89, 98) |

- **The veil is the level's exponential height fog, not the fog volume.** With the fog pass off the view is clear;
  the cube contributed nothing (the camera is inside it and its surface was one-sided).
- **The veil is what the level's data says** (section 2: `FogDensity` 5, `FogHeightFalloff` 1, `FogMaxOpacity` 0.7,
  `StartDistance` 1, the fog actor at height 100,930). The density at the eye is
  `0.005 × 2^((100,930 − eye height) / 1000)` per UU, and a level ray reaches the maximum opacity after
  `2.5 / density` UU:

  | Place | Height | Density at the eye (per UU) | Fog at its 70 % limit beyond |
  |---|---:|---:|---:|
  | checkpoint 16 (respawn eye) | 95,875 | 0.17 | 15 UU |
  | checkpoint 20 | about 97,100 | 0.073 | 35 UU |
  | checkpoint 19 | about 97,800 | 0.044 | 57 UU |
  | checkpoints 17, 18 | about 99,900 | 0.010 | 245 UU |
  | checkpoint 21 | about 107,100 | 0.00007 | 35,900 UU (half opacity at 20,600) |
  | checkpoint 22, the player start | about 111,000 | 0.000005 | never within the level |

  The first five checkpoints lie below the fog actor: there every pixel is 30 % scene and 70 % fog colour, at any
  distance — a flat veil with no depth in it. From checkpoint 21 on the player is above it and the view is clear,
  with the mist lying below. The recorded frames agree: veiled up to checkpoint 20, clear at 21.
- The fog pass draws that and nothing more: without the LUT the darkest regions are (41, 54, 75); 70 % of the fog
  colour for a level ray, (0.035, 0.054, 0.095) linear, is (47, 58, 75) through a plain 2.2 gamma.
- **The colour grading makes the veil bright and teal.** The level's LUT (`MapTemplates.lut.LUT_Night`, read from
  the converted texture) lifts and cools mid-tones — neutral grey 68 becomes (86, 99, 105), 102 becomes
  (125, 143, 152) — so the dark navy veil comes out as (53, 91, 106).
- **Looking up it is brighter still.** Without a dominant directional light the fog's bright side is straight up
  (section 2): 26 % of the light-blue inscattering colour at 60° above the horizon against 7 % level.
- An earlier build of the same day showed none of this in the same place only because the level's atmosphere was
  not applied after a level change, which left the invented distance fog of `converted.rs`.

What the runtime shows there now (same camera, and three more at the places of the recorded frames): the veil
unchanged — it is the data — and behind it the level's fog volume, which was missing: 2 % of its mint colour at
the 11,000 UU of that dark gap, up to 12.7 % in the distance. Far cave walls, pillars and floating rocks, which
were lost in an even dark teal, stand as silhouettes in a paler green mist; near things do not change (ground
(112, 147, 144), gap (63, 109, 116)).

**The fog volume against the original (2026-10-13) — STRONG.** One of the original's stills that ship in the UI
package (`LIGHTING.md`, 2.6) is taken on the start bridge of this level, above the fog actor, where the height fog
is thin; `LIGHTING.md` noted that it has "more and greener fog" than our render and nothing darker than about 21
of 255. That fog is the level's fog volume. Our render from the start platform
(`--camera 205560,-97397,109470,180,-6`; not the still's exact view, so only the far field compares), mean sRGB of
the same parts of the frame:

| Part of the frame | Before (no fog volume) | With the fog volume | The original's still |
|---|---|---|---|
| far cave, top centre | (11, 17, 15) | (51, 91, 69) | (57, 101, 76) |
| far wall on the left | (12, 18, 18) | (36, 61, 47) | (36, 63, 49) |
| floating rocks and island in the middle distance | (40, 69, 78) | (80, 139, 126) | (75, 127, 121) |
| mossy wall on the right | (38, 57, 40) | (53, 87, 64) | (49, 82, 62) |

Before, the cave beyond the first rocks was black; with the volume, evaluated as section 3 describes and with no
value of ours in it, the distance has the original's pale green mist to within a few counts.

OPEN, for the veiled places below the fog actor: no still of the original shows one for certain. A second still
(a mossy slope with a lantern, very likely this level, place unknown) has an even far fog of about (41, 72, 80)
behind near rocks that stay dark, (13, 19, 18) to (18, 24, 19): a place where the height fog needs some distance
to fill up, so above or near the fog actor's height. Our far veil over a dark background is (53, 91, 106) before
the fog volume is added: the same hue, brighter. Whether that is the place, the build the still came from, or our
tone curve and LUT handling is not known; the fog's values and maths are the level's and the engine's
(`FSceneRenderer::InitFogConstants`, the scene info's constructor and the fog shader were re-read for this). A
capture of the original at checkpoint 16 would settle it.

**Fog volumes in the other maps (2026-10-13).** Offscreen renders before and after (`--fly --hidden-window
--screenshot`, 1600 x 900, mean sRGB of image regions; images stay local). The log names the volumes read: 2 in
AG-Workshop, 1 in AG-ParadiseCave, 7 in AG-Darkcave, 5 in AG-IceCave (none in AG-StarHaven, whose only one is
disabled).

- **AG-Workshop**, from the player start, inside the larger room volume: a faint blue-grey lift on the dark walls,
  (12, 4, 3) to (17, 12, 14) in the middle of the view, (6, 3, 2) to (9, 6, 7) at the ceiling. AG-Epilogue is the
  same room.
- **AG-IceCave**, from the player start: the cave more than 20,000 UU away (the constant volume's start distance)
  turns from near black, (10, 12, 21), to a pale grey-violet haze, (65, 65, 79); everything nearer is the same to
  the count. Looking down the shaft at checkpoints 10 to 12 (`--camera -66700,-107900,-29300,0,-70`), the
  half-space volume there fills the bottom with a pale violet pool that the ice columns sink into, (12, 14, 22) to
  (145, 146, 176) in the middle; above its plane nothing changes.
- **AG-Darkcave**: no difference in the views tried (the start, the start looking down, checkpoint 33, two places
  above the long crossings). Its seven volumes are black and act under their planes only, 1,300 UU or more below
  every checkpoint, where those views are black already. What they do is hide whatever light would reach the
  bottom of the pits.
- **AG-BeautifulCity**: at most 2.4 % beyond 30,000 UU; not told apart from the view without it.

**AG-Workshop brightness (2026-10-12).** The owner found the Workshop very dark away from its lamps. Checked on
the Windows machine with the converted data (`--level AG-Workshop --fly --screenshot`, offscreen 1600 x 900, the
view from the player start down the corridor; mean sRGB values of image regions; images stay local):

| Run | Corridor wall (left / right) | Ceiling | Lit floor of the room beyond |
|---|---|---|---|
| default (light maps, uber effect) | (10, 4, 3) / (11, 4, 3) | (7, 3, 2) | (35, 16, 9) |
| `ASAMU_POST=off` (Bevy's tonemapper on render radiance) | (25, 4, 2) / (29, 4, 2) | (17, 3, 1) | (83, 37, 10) |
| `ASAMU_LIGHTMAPS=0` (dynamic lights and the ambient stand-in) | (21, 14, 13) / (21, 14, 13) | (17, 15, 13) | (20, 15, 12) |

- The level's data is exported and applied: light maps on 1,078 of 1,107 draws (13 atlases), 57 of 61 lights
  recognised as baked; 2 post-process volumes and the uber effect (`Scene_TonemapperScale` 1, shadows 0, midtones 1;
  highlights (1, 1, 1) with desaturation 0.2 in the volume around the start, (1, 0.6, 0.4) in the other).
- The baked light is dim away from the lamps in the data itself: over the 1,286 light-mapped BSP vertices the
  light map's brightest channel is 0.014 at the median and 0.26 at the 90th percentile (1.0 = a light of
  brightness 1 at normal incidence); the lamps are small (38 baked point lights, e.g. brightness 0.66, radius
  179 uu). Where one baked point light dominates and no spot light reaches (51 vertices), light map over analytic
  direct light (`brightness x N.L x (1 - (d/R)^2)^exponent`, no occlusion) is 1.2 at the 75th and 1.9 at the 90th
  percentile: in lit spots the decoded light map has the expected size (TENTATIVE, small sample; supports the
  sRGB decoding of LIGHTMAPS.md).
- The uber effect's tonemapper then halves such shadows on screen: at scene value 0.003 the gamma term alone gives
  0.071 (18 of 255), the curve term 0.014, and the toe factor 0.5 blends them to 0.043 (11 of 255), which is what
  the default run shows. `ASAMU_POST=off` looks brighter because Bevy's tonemapper is then fed render radiance
  (3.2 per scene unit, section 5), not because an exposure setting of the level is missed.
- So no presentation default of ours (exposure, tone mapping) explains it, and none was changed. Open, in order of
  how much they could matter: (1) the original sets a display gamma at start from its own `Gamma` setting
  (`ASAMUSettingsManager`, config default 0.5, DEFAULTS.md) through a native controller function whose mapping to
  the engine's display gamma is not recovered (UNKNOWN; the script's fallback for other controllers issues the
  `Gamma` console command with four times the setting); we use 2.2; (2) the tonemapper variant is STRONG, not
  CONFIRMED (above), and the toe blend is what crushes these shadows; (3) the light map's per-pixel combination
  with the normal is approximated (LIGHTMAPS.md). A reference capture of the original's Workshop would settle it.

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
