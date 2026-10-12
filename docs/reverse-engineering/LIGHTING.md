# Lighting: who lights what, the black plants, and the precomputed light volume

Evidence sources: the map packages of the legitimately owned install (Steam build 1822049), decoded read-only by
our own code (`asamu-ue3`, `asamu-import`), and the user-local converted data made from them (scene, light map and
material files). Renderer observations are offscreen renders of **our** runtime and are labelled as such. Seven
full-screen images that ship inside the game's own UI package were looked at locally as a reference for the
original's look (section 2.6); none of them, and no other game data, is in this repository. This page holds names,
counts, structure and our own descriptions only.

Builds on `LIGHTMAPS.md` (light maps, which lights are baked, the runtime's light map scale), `LEVEL_FORMAT.md`
(the level's native tail), `MATERIALS.md` and `POST_FOG_SKY.md` (tone curve, fog). None of their claims changed.

Reproduce:

```sh
asamu-import lightmaps --check                      # per map: light map coverage and the "light volume:" line
asamu-import --out <user-local dir> lightmaps       # also writes <map>.lightvolume.bin
cargo test --release -p asamu-import lightmaps::volume   # layout checks on synthetic data and, with the install, on every map
cargo test --release -p asamu-assets lighting::     # the reader and the interpolation
cargo test --release -p asamu only_alpha_masked     # which light-mapped draws leave the automatic batches
# the same view twice must be the same picture (section 2.2); keep the pictures local
asamu --converted <dir> --level AG-ParadiseCave --fly --camera 205560,-97397,109470,180,-6 \
      --hidden-window --screenshot <local file> --screenshot-delay 8 --exit-after 16
```

`(T)` marks a claim asserted by `the_shipped_volumes_decode_and_pass_the_layout_checks` (importer, skips without the
install). The measurements of section 2 were made with throwaway scripts over the converted data on the build
machine (not committed); the numbers they produced are quoted here with what was measured.

## 1. Who lights what in the original

UE3 lights a primitive in one of two ways, and the shipped maps use both:

| Kind of object | How the original lights it | Evidence |
|---|---|---|
| Static mesh components of static actors, BSP | Baked light maps (texture or per vertex); lights baked into them do not light them again at run time; dynamic lights (movable, toggleable) do | `LIGHTMAPS.md` |
| Movable meshes (`InterpActor`), interactable props, collectibles, falling rocks, recharge crystals, characters, the first-person hand | No light map. A light environment per object: the direct light of the lights that reach it, plus the indirect light the level baked into its **precomputed light volume** (section 3) | no light map entry for any of them (table below); the volume is in 10 of the 12 maps (T) |

Visible static mesh components by actor class and light map kind (converted scene and light map files, six story
maps; CONFIRMED for the counts):

| Map | Class | Texture light map | Vertex light map | None |
|---|---|---:|---:|---:|
| AG-ParadiseCave | `StaticMeshActor` | 1,967 | 31 | 13 |
| | `ASAMUSoundMakingFoliage` | 1,018 | 0 | 0 |
| | `UTTeamStaticMesh` | 323 | 11 | 0 |
| | `InterpActor` | 0 | 0 | 138 |
| | `ASAMUInteractable_Actor` / `ASAMUCollectible` | 0 | 0 | 24 / 5 |
| | `ASAMUCheckpointVisuals` | 0 | 22 | 0 |
| AG-StarHaven | `ASAMUSoundMakingFoliage` | 410 | 0 | 0 |
| | `InterpActor` / interactable / recharge crystal / collectible | 0 | 0 | 377 / 34 / 7 / 5 |
| AG-Darkcave | `ASAMUSoundMakingFoliage` | 205 | 200 | 1 |
| | glow flower / interactable / collectible / `InterpActor` | 0 | 0 | 15 / 5 / 5 / 3 |
| AG-BeautifulCity | `InterpActor` / interactable / collectible | 0 | 0 | 185 / 8 / 5 |
| AG-IceCave | falling rocks / recharge crystals / `InterpActor` / interactable | 0 | 0 | 166 / 98 / 77 / 44 |
| AG-Workshop | `InterpActor` / interactable | 0 | 0 | 13 / 12 |

So the split is by class, as in UE3: what can move has no light map. Every one of the 1,834 sound-making plants is
a static actor (`bStatic`, not movable, `PHYS_None`) and all but one carry a light map.

### 1.1 The light environments' settings — CONFIRMED (data), not used by the runtime

Read with `asamu-inspect defaults` / `props` from `Engine.u` and `Startup.upk`, and from the `WorldInfo` of each
converted scene (verification pass, 2026-10-12):

- **Templates.** The light environment templates of `SkeletalMeshActor`, `SkeletalMeshActorMAT`, `InterpActor`,
  `DynamicSMActor`, `KActor` and of the game's own `SkeletalMeshActorMATWithFollowCollision`,
  `ASAMUInteractable_Actor`, `ASAMUCollectible`, `ASAMUFallingRock`, `ASAMURechargeCrystal`, `ASAMUPawn` and the
  three NPC pawn classes carry no tagged property of their own: they are the component class's defaults or their
  parent template. Those defaults are: enabled, `AmbientGlow` black, `AmbientShadowColor` (0.001, 0.001, 0.001),
  a synthesized directional light on, one visibility sample towards each light. So no class adds a floor of
  ambient light to its objects: where the volume and the lights give nothing, the data asks for a black object.
- **Characters.** `UTPawn` (which `ASAMUPawn` extends), `ASAMUNPC_Pawn` and the grapple gun (the first-person
  hand) set `bIsCharacterLightEnvironment` and a synthesized spherical light; `SkeletalMeshActor` villagers do
  not.
- **Per level.** Five story maps set `CharacterLitIndirectBrightness` to 1.7 (AG-ParadiseCave, AG-BeautifulCity,
  AG-StarHaven, AG-IceCave, AG-Darkcave; the engine default and AG-Workshop / AG-Epilogue: 1.0);
  `CharacterShadowedIndirectBrightness` is 1.0 and `CharacterLightingContrastFactor` 1.5 everywhere (the
  defaults). By their names these scale the indirect light of character light environments where the sample is
  not shadowed from the dominant light (flag 0, section 3.2: only in parts of AG-StarHaven among those five).
  How the engine applies them was not read from the executable (TENTATIVE); the runtime does not use them.

## 2. The black plants of the first play session (defect 5)

In the owner's screenshots of AG-ParadiseCave many plants were solid black next to brightly lit ground, while
others of the same kind were lit. The suspicion was that plants have no light map and are lit by a light
environment in the original. That is not so, and the first reading of this page ("the plants are dark because the
data is dark, nothing to change") was wrong too (changed 2026-10-12, verification pass).

### 2.1 Result

- **The plants are lit by their own baked light maps, in the original and here** (CONFIRMED: 1,018 of 1,018 plant
  components in AG-ParadiseCave have a texture light map and the runtime pairs them: 3,876 of the level's 4,091
  draws receive a light map, exactly the mesh sections of the components that have one, the 1,018 plant sections
  among them; none lacks a usable UV set). Nothing dynamic lights them except the level's few movable and
  toggleable lights.
- **They were black because they were drawn with another draw's light map** (CONFIRMED: renderer source and
  repeated renders, 2.2). Leaf materials are alpha-masked. The renderer (Bevy 0.20) keeps light map textures in
  groups of four and binds one group per batch; its opaque pass keeps batches of different groups apart, its
  alpha-mask pass does not. Every masked draw that shared a material group with others was therefore drawn with
  the light maps of whichever draw came first: at its own rectangle it read a foreign atlas, which is black in
  most places and lit in some. Which plants came out black changed from run to run.
- **Fix**: light-mapped draws with an alpha-masked material are taken out of the automatic batches, so each binds
  its own light maps (`apps/asamu/src/lightmaps.rs`, `needs_own_draw`). The plants then show their own baked
  light: lit like the ground they stand on, dim where the level is dim.
- **Where the level is dim the plants are dark, as in the original** (2.6, STRONG: stills of the original that
  ship with the game show plants away from the lamps as dark shapes). That part is data (2.3, 2.4); it is not
  what the play session showed, which had black plants beside lamps.
- **The near-white ground of the played build** (the materials defect, fixed separately; `MATERIALS.md`) made
  the black plants stand out, but did not cause them.

What the original has and this runtime does not (specular light from the directional light maps, normal-mapped
detail in the baked light) is listed in 2.8.

### 2.2 The cause — CONFIRMED (renderer source, repeated renders of our runtime)

- **Renderer source** (`bevy_pbr` and `bevy_core_pipeline` 0.20.0, read in the cargo registry): a light map
  texture lives in a slab of 4 (`LIGHTMAPS_PER_SLAB`), and a draw's mesh bind group carries one slab. The opaque
  pass's batch-set key has a `lightmap_slab` field; the alpha-mask pass uses the key of the passes that have no
  light maps (`OpaqueNoLightmap3dBatchSetKey`), so its batches are formed without regard to the slab and are
  drawn with the slab of their first entity. AG-ParadiseCave has 522 atlases, so at least 131 slabs.
- **Repeated renders.** The same build, the same view (the first checkpoint) and the same settings gave three
  different pictures in five runs: a broad-leaved plant in the foreground was near black in one, half bright
  green in three, and evenly dim green with a lit red fern beside it in one. Between two such runs 6.3 % of the
  frame's 20 x 20 pixel blocks differed by more than 4 of 255 levels (largest difference 162), all of them on
  plants. With the fix four runs are the same picture (0 blocks differ; largest difference 2) and it is the third
  of the three: every plant evenly lit by warm light, as the rock under it.
- **How many draws**: 1,768 alpha-masked light-mapped draws in AG-ParadiseCave (the runtime logs the number per
  level).
- **Cost**: those draws are no longer batched. At the first checkpoint the offscreen frame rate fell from 33 to
  27 frames per second on the build machine. An alternative that keeps batching would be to draw masked materials
  through the opaque pass (alpha to coverage), which changes how cut-outs look; not done.

### 2.3 The baked light on the plants — CONFIRMED (data)

Eight plant meshes make up the 1,018 actors. All use light map UV channel 1 of 2 and a light map of 32 or 64
texels (30 or 62 without the padding):

| Mesh (`Foliage.*`) | Actors | Light map mean over the component's rectangle: 25 % / median / 75 % | Rectangles darker than 0.01 |
|---|---:|---|---:|
| `Bush_Tropical` | 376 | 0.012 / 0.028 / 0.055 | 83 |
| `Bush` | 208 | 0.019 / 0.055 / 0.118 | 40 |
| `Bush_Alone` | 207 | 0.046 / 0.105 / 0.203 | 23 |
| `Bush_Tropical2` | 85 | 0.005 / 0.013 / 0.020 | 34 |
| `Flower_Bush` | 70 | 0.017 / 0.032 / 0.052 | 10 |
| `Flower_Bush_2` | 47 | 0.008 / 0.030 / 0.069 | 12 |
| `Bush_Flowers` | 17 | 0.006 / 0.018 / 0.030 | 7 |
| `Bush_Light` | 8 | 0.050 / 0.066 / 0.199 | 0 |

(Irradiance in the light maps' units: 1.0 is a light of `Brightness` 1 at normal incidence. The rectangle mean
includes texels no triangle uses, which are black, so it understates the leaves by the unused share.)

- The plants are not darker than the rest of the level: over all components the rectangle mean is 0.014 / 0.037 /
  0.088 (25 % / median / 75 %) for the plants and 0.005 / 0.026 / 0.083 for the 1,967 `StaticMeshActor` components.
- Next to the first checkpoint, 20 plants were compared with the light map of the ground triangle right below
  them: the ratio ground / plant has a median of 0.42 (10 % 0.07, 90 % 2.9). The plants there hold, on the whole,
  more baked light than the ground they stand on.
- Second measurement (verification pass, light map sampled at every vertex): within 700 UU of the hanging lamp
  of the second and third screenshots, the 23 plants have a mean of 0.01 to 0.41 in red (middle value 0.087) and
  a median luminance of 0 to 0.23 (middle value 0.045); the platform under them has a mean of (0.18, 0.08, 0.07)
  and a median luminance of 0.065. Plants and ground hold the same order of light, in the same warm colour. A
  plant that is black beside that ground is not what the data says.
- Inside one plant the light is uneven: every leaf has its own block of texels, bright at one end (0.05 to 0.2) and
  below 0.005 at the other, the way a dense bush shadows its own inside in a light bake.
- The decoded light map values have the expected size where it can be checked: in AG-Workshop, for vertices where
  one unshadowed baked light dominates, light map over analytic direct light is 0.39 at the median, 1.1 at the
  75th and 1.9 at the 90th percentile (13,836 vertices; `POST_FOG_SKY.md` reports the same from a smaller sample).
  A systematic error of several times in the decoding is not what darkens the plants (TENTATIVE: the upper
  percentiles include indirect light).

### 2.4 The leaf materials — CONFIRMED (data)

The plant materials are plain: lit (Phong), masked, two-sided, the diffuse texture alone as the colour, its alpha
as the mask, a normal map and a specular texture; the reduction is lossless for colour and mask (`materials.json`:
no notes, nothing dropped). Mean colour of the opaque texels (linear, from the importer's texture statistics):

| Material | Uses in the story maps | Opaque mean (R, G, B) |
|---|---:|---|
| `Foliage.foliage_green_Mat` | 776 | 0.27, 0.24, 0.14 |
| `Foliage.Tropical_Foliage_df_Mat` | 629 | 0.39, 0.32, 0.04 |
| `Foliage.Bush_Dark_Mat` | 444 | 0.025, 0.08, 0.014 |
| `Foliage.Grass_Dark_Green_Mat` | 268 | 0.21, 0.49, 0.13 |
| `Sigge.Materials.Tropical_tree_foliage_mat` | 25 | 0.007, 0.017, 0.002 |

Two of the five are dark textures: a plant with `Bush_Dark_Mat` in light of 0.04 to 0.14 gives a scene value of
0.003 to 0.011 in green and a third of that in red and blue, which the level's tone curve shows near black
(`POST_FOG_SKY.md`). Those plants are dark shapes when drawn correctly; the three bright materials are not.

### 2.5 What was ruled out

| Suspected | Finding |
|---|---|
| Plants have no light map and are lit by a light environment | They are static actors with texture light maps (2.1). CONFIRMED |
| Vertex lighting | None of AG-ParadiseCave's plants; 200 of AG-Darkcave's 406 plants have vertex light maps, which this runtime reduces to one constant per plant (`LIGHTMAPS.md`). Not the defect seen |
| Two-sided leaves shaded with a wrong back-face normal | The light map term has no normal in this runtime (it is the baked irradiance times the diffuse colour, on both faces), so a flipped normal cannot darken it. For the few dynamic lights the renderer flips the normal of back faces, as UE3 does for two-sided materials (the engine convention, not read from the executable: TENTATIVE) |
| The light map is sampled through the wrong UV set or rectangle | For two plants the texels the mesh's light map UVs cover are exactly the non-black blocks of their rectangle. CONFIRMED for those two. (The rectangle was right; the atlas it was applied to was not, 2.2) |
| The plants are not paired with their light map | 3,876 of 4,091 draws paired, which is every mesh section of a component with a light map; the other 215 are the sections of the components without one (section 1). CONFIRMED. (Paired on our side; bound wrongly in the renderer, 2.2) |
| The data is dark and the original shows the same black plants | True only where the level is dim (2.6). It does not explain black plants beside lamps, nor a picture that changes from run to run (2.2) |

### 2.6 What the original looks like — STRONG (reference stills)

The game's UI package ships seven full-screen textures (`ASAMUHudFlash.asamu_previewending_I*`, 1,920 x 1,080,
DXT1) for a preview-build end screen: six frames of the original game and one text card. One frame was taken in
AG-ParadiseCave for certain (the start bridge, with the first checkpoint's stones and the cave paintings), a
second very likely (the same moss, fog and lantern props; its exact place was not found). They may come from an
earlier build and from other graphics settings than a default run, so they show the look, not exact values.

- One is the view down the hanging bridge from the start towards the first checkpoint, which our fly camera can
  reproduce. On the checkpoint island the plants are dark olive against the moss, with a few lit leaves; they are
  not black. What differs most between that still and our render is around the plants: the original has more
  and greener fog (nothing in the still is darker than about 21 of 255) and moss where our rock material shows
  bare rock (`MATERIALS.md`, the up-facing layer).
- The other shows a mossy slope with a lantern: the plants away from the lantern are black shapes against the
  fog, a fern next to the lantern is lit, and the first-person hand is almost black with only its glowing ring
  showing. In a third still, at a lit place in the ice cave, the hand is clearly lit, and in a fourth two
  characters beside candles are clearly lit in a dark place. The hand and the characters follow the light around
  them, as objects with a light environment do (section 3).

### 2.7 Before and after (our renders)

Offscreen renders, 1,600 x 900, at the views of the play session's screenshots (the first checkpoint, the rocks
and the fence, the hanging lamp next to them) and at the start bridge; all with the corrected materials.

| View | Region | Before the fix (three to five runs) | With the fix |
|---|---|---|---|
| First checkpoint | plants on the left | mean (17, 18, 16) to (21, 22, 16); 25 % to 51 % of the region darker than 16 of 255, a different amount from run to run | (21, 22, 16); 25 % darker than 16 |
| | plants on the right | (36, 35, 23) to (40, 39, 24); 13 % to 31 % | (40, 39, 24); 13 % |
| | ground | (77, 50, 27) | (77, 50, 27) |
| Rocks and fence | plants on the rocks | (38, 43, 39) to (39, 44, 40): dark leaves, some of them near black | (51, 49, 41): a red fern, green and yellow leaves |
| | ground | (68, 62, 48) | (68, 62, 48) |
| Hanging lamp | plant in the foreground | (31, 48, 40) to (34, 49, 40); 11 % to 17 % darker than 16 (leaves that are black or dark red) | (35, 57, 42); 1 % |
| | ground | (52, 58, 46) | (52, 58, 46) |
| Start bridge | leaves in the foreground | (38, 55, 56): black shapes | (57, 85, 63): lit green and yellow |
| | bridge planks | (76, 55, 46) | (76, 55, 46) |

(Mean sRGB values of fixed rectangles of the frame; fog and background are part of each rectangle.) The ground
and everything else that is not alpha-masked is the same to the pixel block; only the plants change. One of the
five runs before the fix happened to bind the right light maps at the first checkpoint and is the same picture as
the fixed one. The far palm of the second view stays a dark shape with the fix: its leaf texture is the darkest
of the five (2.4) and it stands in dim light.

### 2.8 What the original adds that this runtime does not — UNKNOWN size

- **Specular light from the baked light.** The original renders directional light maps and takes a specular term
  from them; several leaf materials have a bright specular texture (`Tropical_Foliage_sc`: mean 0.62). Here baked
  lights give light-mapped surfaces their specular term as dynamic lights (unshadowed, with the physically based
  model's low reflectance for non-metals). How much sheen the original's leaves have was not measured.
- **Normal-mapped detail in the baked light** (`LIGHTMAPS.md`): normal maps are off by default.
- **The tone curve and display gamma** are STRONG, not CONFIRMED, and decide how dark "dim" is on screen
  (`POST_FOG_SKY.md`, open points).
- **Vertex light maps** are one constant per component.

## 3. The precomputed light volume

### 3.1 Where it is — CONFIRMED (T)

A `Level` export's native tail holds an `FPrecomputedLightVolume` (`LEVEL_FORMAT.md`, field 19): a flag, bounds
(two vectors and a byte), one `f32` (0 in every map) and an array of 33-byte samples. The level reader checks the
block's size and keeps the bounds and the count; the importer finds the block again inside the export by exactly
that header (37 bytes: the flag as `u32 1`, the bounds, the `f32`, the count), which must occur exactly once and be
followed by `count x 33` bytes (T: found in all 10 maps that have a volume).

| Map | Samples | Outside the stored bounds | Radius: smallest / largest | Flag set | Lit: indirect / environment / ambient |
|---|---:|---:|---|---:|---|
| AG-ParadiseCave | 1,102,996 | 0 | 282.84 / 53,949.24 | 1,102,996 | 241,025 / 24,746 / 277,762 |
| AG-IceCave | 301,867 | 0 | 282.84 / 10,342.17 | 301,867 | 112,359 / 0 / 113,886 |
| AG-Darkcave | 201,664 | 0 | 282.84 / 4,327.01 | 201,664 | 126,300 / 49,167 / 175,334 |
| AG-StarHaven | 105,313 | 22 | 282.84 / 8,730.26 | 45,852 | 80,343 / 73,592 / 79,157 |
| AG-BeautifulCity | 62,767 | 40,477 | 282.84 / 3,464.10 | 62,767 | 20,672 / 44,293 / 46,401 |
| TheCore | 1,203 | 0 | 282.84 / 282.84 | 1,203 | 858 / 0 / 810 |
| ASAMUFrontEndMap | 167 | 0 | 282.84 / 3,464.10 | 73 | 157 / 0 / 157 |
| AG-Workshop | 166 | 0 | 282.84 / 3,464.10 | 72 | 154 / 0 / 155 |
| AG-Epilogue | 165 | 0 | 282.84 / 3,464.10 | 57 | 144 / 0 / 128 |
| Freds_place | 0 | 0 | — | 0 | — |

1,776,308 samples in all (T). `ASAMUEntry` and `ASAMULegal` have no volume (T).

### 3.2 Sample layout — order and sizes STRONG, meaning TENTATIVE

```text
sample (33 bytes, little-endian)
  FVector Position            world space, UU
  f32     Radius              how far the sample reaches
  u8 x 2  IndirectDirection   theta, phi (quantized spherical coordinates)
  u8 x 2  EnvironmentDirection
  FColor  IndirectRadiance    bytes B, G, R, exponent
  FColor  EnvironmentRadiance
  FColor  AmbientRadiance
  u8      bShadowedFromDominantLights
```

The field order, the names and the colour encoding (`channel x 2^(exponent - 128) / 255`, black for exponent 0)
are UE3's conventions; they were **not** read from the shipped executable. What the data confirms (T):

- **Position**: in the eight maps without stray samples every position lies inside the stored bounds; in
  AG-BeautifulCity the lowest sample coordinate equals the bounds' minimum on all three axes to within 1 UU. The
  stored box is a cube, and where samples leave it (AG-BeautifulCity 40,477, AG-StarHaven 22) they leave it
  upwards only.
- **Radius**: positive and finite in every sample; the smallest is 200 x sqrt(2) = 282.84 in every map with
  samples, which is what a sample on a 200 UU grid needs to reach its diagonal neighbours. The detailed samples
  sit 200 UU apart above surfaces, in two layers about 210 UU apart (seen next to the first checkpoint of
  AG-ParadiseCave); a few coarse ones with a large radius fill the rest (AG-ParadiseCave: 1,052,343 detailed and
  50,653 coarse).
- **Flag**: 0 or 1 in every sample. It is 0 somewhere only in the four maps where the table shows fewer flags
  than samples, and each of those has a dominant directional light; in the three maps without any dominant light
  (AG-ParadiseCave, AG-BeautifulCity, TheCore) it is 1 in every sample, as "shadowed from every dominant light"
  must be. (AG-IceCave and AG-Darkcave have a dominant light and all flags set; not looked into.)
- **Radiance**: plausible sizes (largest channel 3.6 in AG-ParadiseCave, 19.6 in AG-StarHaven; median of the lit
  samples near the first checkpoint about 0.01), black in most of the open cave.

The directions are stored and exported but not used yet.

### 3.3 How a point is lit from it — TENTATIVE

- **Interpolation.** Every sample whose radius reaches the point contributes with the weight
  `(1 - d^2 / r^2) / r^2` (`d` the distance, `r` the sample's radius): a sample fades to nothing at its radius and
  detailed samples outweigh coarse ones. This is the weighting of the engine family's source (STRONG by
  convention; the shipped executable's function was not read). Where no sample reaches there is no baked light
  for a movable object (the start area of AG-BeautifulCity lies outside its volume).
- **From radiance to light on a surface.** The engine rebuilds a low-order spherical function from a sample (a
  lobe along the stored direction with the indirect radiance, plus a constant with the ambient radiance) and
  lights each surface by its normal. This runtime uses one value for all directions: **a quarter of the summed
  radiances**. A lobe of radiance `L` lights a surface by `L x max(0, cos)`, whose mean over all directions is
  `L / 4`, and the constant term gives the same quarter under the cosine law.
- **Check against the light maps.** For floor vertices (normal within 26 degrees of up) the light map value was
  compared with what the nearest samples give 60 UU above the floor, with the lobe evaluated for an upward
  normal. A light map holds direct and indirect light, the volume only indirect light, so the ratio must not fall
  below 1 by much and should pile up at 1 where no direct light falls:

  | Reading of the stored radiance | AG-Workshop (3,500 vertices): 10 % / median / 90 % of light map over volume | Next to the first checkpoint of AG-ParadiseCave (271 vertices) |
  |---|---|---|
  | radiance x (cosine terms of a low-order spherical function), i.e. a quarter for the constant part | 0.16 / 1.3 / 8.7, most frequent value 1.0 to 1.6 | 1.7 / 33 / 140 |
  | radiance taken as irradiance (no quarter) | 0.06 / 0.43 / 2.5, most frequent value 0.4 to 0.6 | 0.5 / 8.1 / 42 |

  Under the first reading the most frequent ratio is 1 and about four in ten floor vertices of AG-Workshop fall
  below it (the volume is asked 60 UU above the floor, and a floor's own contact shadows are not in it); under
  the second about three in four do, which a light map that contains the indirect light cannot do. The first
  reading is the consistent one and is what this runtime uses (TENTATIVE: a statistical check, not the shader).
- **Second check: movable objects beside a light-mapped twin** (verification pass; the volume asked at the
  movable object, reduced to a quarter, against the light map of a static component with the same mesh a short
  way off, sampled at every vertex):

  | Pair | Volume at the movable object (R, G, B) | Twin's light map: mean (R, G, B) | Twin's light map over the volume (luminance): median / 90 % |
  |---|---|---|---|
  | AG-StarHaven, a garden plant, 35 UU apart | 0.54, 0.31, 0.21 | 0.27, 0.12, 0.02 | 0.26 / 1.09 |
  | AG-StarHaven, a crate, 186 UU apart | 0.25, 0.61, 0.63 | 0.18, 0.39, 0.42 | 0.11 / 2.0 |
  | AG-Workshop, a crate, 65 UU apart | 0.048, 0.042, 0.038 | 0.017, 0.016, 0.015 | 0.27 / 0.73 |
  | AG-IceCave, a flare, 10 UU apart | 0.017, 0.050, 0.144 | 0.024, 0.047, 0.090 | 0 / 4.7 |

  The colours agree in all four (which also confirms the byte order of the stored colours), and the quarter
  reading puts the volume at the level of the twin's best-lit vertices (the 90th percentile is 0.7 to 2 times
  the volume), not above them all. A static prop's median is several times lower because its light map holds its
  own contact shadows and unlit sides. Without the quarter the volume would be 2 to 5.5 times brighter than the
  best-lit vertices of the twin in three of the four pairs. In AG-ParadiseCave a floating spire's light map
  (median 0.0154, most of it out of direct light) against the volume at the same place (0.0142) gives 1.1.

## 4. Importer and runtime

### 4.1 `asamu-import lightmaps` (additive)

| File | Content |
|---|---|
| `<map>.lightvolume.bin` | 16-byte header (`ASAMULV1`, record size 36, record count), then one record per sample: position and radius as stored (`f32` x 4), indirect, environment and ambient radiance as linear RGB in `R9G9B9E5` (`u32` x 3), the four direction bytes and the flag as stored, three zero bytes |
| `<map>.lightmaps.json` | new optional last key `light_volume`: `file`, `samples`, `record_size`, the stored bounds, and the statistics of 3.1. `version` stays 1; a file without the key reads as before |

`--check` prints the statistics and writes nothing. The whole stage takes about 25 s for the 12 maps on the build
machine; the volume files are 64 MB together (AG-ParadiseCave 40 MB).

### 4.2 Reader (`asamu_assets::lighting::LightVolume`)

Reads the files of a level and its merged sub-levels (each moved by its streaming offset), keeps position,
radius and the three packed radiances per sample (28 bytes; 31 MB for AG-ParadiseCave) and finds the samples
around a point through a grid of 512 UU cells; samples that reach further than a cell are kept in a short list.
`interpolate` returns the weighted radiances, `IncidentLight::irradiance` the quarter of their sum.

### 4.3 Renderer (`apps/asamu/src/light_volume.rs`) — APPROXIMATION

Before, everything without a light map received a constant ambient light, a number of ours
(`LightMapping::base_ambient`, 150). Now, when the level's volume is converted, that ambient light is the
volume's irradiance **at the viewer** times the light maps' exposure, with the volume's colour, plus the level's
sky lights (none of the six story levels has one that maps to ambient light); it is asked again when the viewer
has moved 16 UU and eased over 0.4 s (PLACEHOLDER: render-only smoothing; the original blends a light environment
over time, with durations that are not recovered). Light-mapped surfaces are not affected (the ambient term does
not reach them: with the plants drawn deterministically, section 2.2, every block of the frame that changes
between the volume off and on in the views below lies on something without a light map: a movable mesh, a
character, the sky dome, a glow card). Without a converted volume the constant stays;
`ASAMU_LIGHT_VOLUME=0` switches the module off.

**What that brightness is worth on screen** (CONFIRMED, renderer source: `bevy_pbr` 0.20.0, the ambient term and
the light map term; changed 2026-10-12, the first version of this page called it "the light maps' scale"). A
light map adds `texel x exposure x diffuse colour`. The ambient term adds 0.45 of `brightness x diffuse colour`
(its response for a fully rough surface, the same at every view angle) plus a reflection that does not depend on
the diffuse colour: for an ordinary non-metal about 0.03 of the brightness head-on and up to about 0.2 at a
grazing angle. So an object in baked irradiance `E` gets a little under half the diffuse light of a light-mapped
surface whose texel is `E`, and a sheen that outweighs it on dark materials. The stand-in of 150 is worth a baked
irradiance of about 0.021 in diffuse light, not the 0.047 that its number divided by the exposure gives. The
brightness is not raised to close the gap, because the sheen would grow with it (a choice of ours; render only).

First value at eighteen fly-camera views (our runs with the plants fixed, section 2.2; irradiance in the light
maps' units, ambient light in render units, the constant was 150; "blocks" = share of the frame's 20 x 20 pixel
blocks whose mean changed by more than 4 of 255 levels between the volume off and on, with the mean colour of
those blocks before and after):

| View | Irradiance (R, G, B) | Ambient light | Blocks that changed |
|---|---|---|---|
| AG-ParadiseCave, first checkpoint | 0.0090, 0.0045, 0.0021 | 29, 14, 7 | 0.6 %: far floating rocks, (27, 36, 33) to (23, 31, 29) |
| AG-ParadiseCave, rocks and fence | 0.0008, 0.0011, 0.0004 | 3, 4, 1 | 5.5 %: one pulsing glow card (two runs of one setting differ by the same 5.5 %) |
| AG-ParadiseCave, hanging lamp | 0.0018, 0.0019, 0.0007 | 6, 6, 2 | 4.9 %, by at most 8 levels: the lamp, a movable mesh, and its glow cards (two runs of one setting: 1.9 %) |
| AG-ParadiseCave, hanging lamp from the other side | 0.0009, 0.0020, 0.0007 | 3, 6, 2 | 5.9 %, by at most 11 levels: the same lamp and cards (two runs of one setting: 0.3 %) |
| AG-ParadiseCave, start bridge | 0.0149, 0.0063, 0.0029 | 47, 20, 9 | 1.4 %: floating rocks and the checkpoint stones, (28, 45, 40) to (24, 39, 36) |
| AG-ParadiseCave, the checkpoint stones at the start | 0.0150, 0.0157, 0.0141 | 48, 50, 45 | 8.7 %: the stones, (19, 27, 23) to (14, 21, 19) |
| AG-BeautifulCity, the girl at the start | no sample reaches | 0 | 0.3 %: the character, (25, 23, 22) to (19, 17, 15) |
| AG-BeautifulCity, a group of villagers | 0.035, 0.025, 0.030 | 111, 79, 95 | 0.1 % |
| AG-BeautifulCity, villagers at the market stalls | 0.010, 0.010, 0.013 | 32, 31, 40 | 0.2 % |
| AG-StarHaven, plants near the start | 0.056, 0.063, 0.082 | 177, 201, 260 | 0 |
| AG-StarHaven, village, a pavilion with a character | 1.06, 0.50, 0.34 | 3,380, 1,593, 1,074 | 5.7 %: characters, props, tree crowns and the sky dome, (125, 84, 58) to (136, 93, 65) |
| AG-StarHaven, village, a terrace with characters | 0.94, 0.45, 0.32 | 2,989, 1,428, 1,018 | 10.6 %: the same kinds, (71, 50, 46) to (90, 60, 54) |
| AG-IceCave, start | 0.0028, 0.0070, 0.0161 | 9, 22, 51 | 7.5 %: a large rock without a light map, (27, 39, 58) to (19, 32, 52) |
| AG-IceCave, a hanging rock near the roof, close up | 0.0002, 0.0003, 0.0008 | 0.5, 1, 3 | 100 %: the rock fills the view, (19, 21, 22) to (4, 9, 12) |
| AG-Darkcave, the girl with her lamp | 0.0042, 0.0020, 0.0025 | 13, 6, 8 | 0.3 % |
| AG-Darkcave, plants | 0.0073, 0.0039, 0.0132 | 23, 12, 42 | 0 |
| AG-Workshop, start | 0.0175, 0.0030, 0.0006 | 56, 10, 2 | 0 |
| AG-Workshop, the observatory doors | 0.0437, 0.0060, 0.0011 | 139, 19, 4 | 0 |

What that does to the objects of section 1:

- **Movable rocks, falling rocks, crystals, interactable props, collectibles**: in the caves their unlit sides get
  3 to 300 times less fill light than before and take the place's colour (warm by the lamps of the first cave,
  blue in the ice cave). The checkpoint stones at the start of AG-ParadiseCave go from dim grey to near black
  with glowing signs, which is how the original's still of that place shows them (section 2.6). Their lit sides
  are as before.
- **Under the open sky of AG-StarHaven they get more**: slightly more near the start (177 to 260), but 7 to 22
  times the stand-in in the village (about 1,000 to 3,400). The volume there holds as much light as the light
  maps of the village do (3.3), so this is the level's data; the picture stays plausible (characters and props
  in the open are no longer darker than everything around them), but an object in shade is lit like one in the
  open when the viewer stands in the open: a character under a pavilion roof came out paler than the posts
  beside it. The level's sky dome is a lit mesh without a light map and brightens with the ambient light too.
- **The first-person hand** is on its own layer but shares the ambient light, so it darkens and tints the same
  way; it still receives every level light without a visibility test and the overlay's own weak fill light, so it
  does not go as dark as in the original's still of section 2.6. Not captured: a picture with the hand needs a
  visible window.
- **Characters** (skinned meshes): as the movable meshes. Where no sample reaches the viewer they get no ambient
  light at all: the girl who waits at the start of AG-BeautifulCity, which lies outside that level's volume
  (3.3), is close to a black shape with a thin rim of light, where the stand-in showed her dim grey. The data asks
  for no baked light there (and no class adds a floor of its own, 1.1), so what is missing in that picture is
  the direct light of the lamps around her, which is the dynamic-light mapping's subject, not this page's.
- **Plants and everything else with a light map**: unchanged by the volume.

How far "one value for the whole view" is from one value per object (the volume asked at every object without a
light map, luminance of the irradiance; verification pass):

| Map | Objects | No sample reaches | 10 % / median / 90 % | Largest over smallest among objects within 1,500 UU of each other: median / 90 % |
|---|---:|---:|---|---|
| AG-StarHaven | 475 | 11 | 0.15 / 0.31 / 0.50 | 2.0 / 10.5 |
| AG-BeautifulCity | 266 | 26 | 0.010 / 0.020 / 0.033 | 2.8 / 4.9 |
| AG-ParadiseCave | 169 | 0 | 0 / 0.0002 / 0.0035 | 1.4 / 14 |
| AG-IceCave | 526 | 12 | 0 / 0.0002 / 0.025 | 2.4 / 364 |
| AG-Darkcave | 32 | 12 | 0.0001 / 0.0041 / 0.013 | 58 / 58 |
| AG-Workshop | 19 | 0 | 0.014 / 0.033 / 0.042 | 11 / 11 |

So neighbouring objects typically differ by a factor of 2 and sometimes by far more, and a viewer in the open
tends to stand in more light than the objects (AG-StarHaven: 0.54 and 0.61 at the two village views against a
median of 0.31 at the objects). A value per object is the next step.

Not reproduced (all UNKNOWN in size):

- one value per view instead of one per object: a movable object far from the viewer takes the viewer's ambient
  light;
- the direction of the indirect light, and the environment (sky) part's own shadowing;
- the original's visibility test between an object and each light: the level's lights still light these objects
  one by one, unshadowed (apart from the few lights with shadow maps);
- the original's single synthesized directional light per object.

## 5. Hostile-input discipline

- Importer: the sample count is bounded (8,000,000) before anything is searched or allocated; the block must be
  found exactly once and lie entirely inside the export; a sample shorter than 33 bytes is not decoded. Tests cut
  a synthetic export at every length (always an error) and give it a second header (refused).
- Reader: a file must start with the magic, name the record size 36 and have exactly `count x 36` bytes after the
  header, and all files of a level together stay under 8,000,000 samples; a record whose position or radius is no
  usable number is skipped and counted; the stored path must be a safe relative path. Tests truncate a file at
  every offset, change the magic, the record size and the count, mix unusable records with good ones and overwrite
  bytes at random 500 times: never a panic, never a non-finite light. The renderer's helpers clamp every
  non-finite or negative value before it reaches the ambient light.

## 6. UNKNOWN / not done

- The sample layout's meaning, the interpolation and the reduction to irradiance were not read from the shipped
  executable (3.2, 3.3).
- A light environment per object (direction, visibility of direct light, one value per object) is not modelled.
- `bShadowedFromDominantLights`, the environment radiance's separate treatment and the two direction fields are
  exported but unused; so are the levels' character lighting factors (1.1).
- Alpha-masked light-mapped draws are not batched (2.2): the cost is a few frames per second where many plants
  are in view. Should the renderer's alpha-mask pass learn to keep light map groups apart, `needs_own_draw` can
  go.
- The ambient term's reflection part grows with the ambient brightness and is not in the original's light
  environment (4.3); it shows as a pale sheen on objects without a light map in AG-StarHaven's village.
- The frames of our runtime are not the same from run to run where a material pulses (glow cards); comparisons
  of two renders should use a second run of one setting as the measure of that.
- Instanced static meshes (12 rock piles in AG-BeautifulCity, 4,531 crystals in TheCore) are still drawn without
  their light maps, although every instance is its own draw and its `LightmapUVBias` is exported
  (`WATER_FOLIAGE.md`).
- Vertex light maps are one constant per component (200 plants of AG-Darkcave among them).
- The specular term of baked light (2.8).
- The exact camera of the third play-session screenshot was not found; the view used is the hanging lamp on the
  same platform as the second.
