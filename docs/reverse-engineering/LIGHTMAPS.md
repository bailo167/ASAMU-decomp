# Baked lighting: light maps, shadow maps and their use in the runtime

Evidence source: every map package (`*.asamu`) and every other package under
`~/Library/Application Support/Steam/steamapps/common/A Story About My Uncle/A Story About My Uncle.app/Contents/Resources/ASAMU/CookedMac`
of the legitimately owned Mac install (Steam build 1822049), decoded read-only by our own code in
`crates/asamu-ue3/src/lightmap.rs`. The field order was read from the unstripped Mac executable's serializers (local
Ghidra decompilation under the ignored `research/`, never committed: `UStaticMeshComponent::Serialize`, the
`FStaticMeshComponentLODInfo`, `FModelElement` and light-map-reference `operator<<`s, `FLightMap1D::Serialize`,
`FLightMap2D::Serialize`, `UModelComponent::Serialize`, `UInstancedStaticMeshComponent::Serialize`,
`USpeedTreeComponent::Serialize`, `UFluidSurfaceComponent::Serialize`, `UShadowMap1D::Serialize`,
`FColorVertexBuffer::Serialize`, `FLightMap2D::GetInteraction`, `ULightMapTexture2D::Serialize` /
`InitializeIntrinsicPropertyValues`) and then proven on the data by exact consumption and a byte-for-byte re-encode.
This page holds structure, names, counts and our own descriptions only: no texel data, no decompiled code, no shader
source.

Builds on `OBJECT_FORMAT.md` (prelude, tagged properties), `TEXTURES.md` (the `LightMapTexture2D` /
`ShadowMapTexture2D` textures themselves), `MESHES.md` (light map UV channel), `LEVEL_FORMAT.md` (BSP, scene) and
`MATERIALS.md`. None of their claims changed; this page settles two of their open points (below).

Reproduce:

```sh
cargo test --release -p asamu-ue3 --test lightmap_real_data -- --nocapture --test-threads=1   # asserts every (T)
cargo test -p asamu-ue3 --test lightmap                          # synthetic fixtures, round trips, hostile input
cargo run --release -p asamu-import -- lightmaps --check         # per-map coverage and statistics, writes nothing
cargo run --release -p asamu-import -- --out <user-local dir> lightmaps --map AG-Workshop --png
```

`(T)` marks a claim asserted by `crates/asamu-ue3/tests/lightmap_real_data.rs` against the install.

**Independent re-check (2026-10-10, adversarial verification pass).** A second decoder written from scratch (Python:
its own summary/chunk parser, `liblzo2` through `ctypes`, its own tag skipper, its own light map layout code; local
only, under the ignored `research/`) re-decoded every lighting export of the 12 map packages and agreed with every
map-side count on this page: per-class exports consumed exactly (24,557 + 102 + 161 + 4 + 2 + 20), light map types
(784 / 24,575 / 326), all 1,568 vertex-sample bulk records inline with flags 0, `SizeOnDisk = count x size` and
`OffsetInFile` equal to their absolute stream position, shadow-map references (5,364 / 20, each pointing at the right
class), instances, model elements, override-colour LODs, painted vertices, texture names and pairs, scale-vector
extremes, light GUID matches and the vertex light map fit statistics below. Corrections made in that pass are marked
"(changed 2026-10-10)".

## Result — CONFIRMED (T)

Every export with baked-lighting native data, in every package, decodes with its native tail consumed exactly,
re-encodes byte for byte (`encode_lighting_native`, with the bulk-data offsets recomputed from the export's
position) and passes the structural checks (`validate_lighting`: references in range, shadow-map references point at
`ShadowMap2D`/`ShadowMap1D` exports, vertex-colour counts consistent, 1D sample arrays of equal length):

| Class (`Engine.*`) | Exports | Exact | Re-encoded | Valid | Native bytes |
|---|---:|---:|---:|---:|---:|
| `StaticMeshComponent` | 24,628 | 24,628 | 24,628 | 24,628 | 12,193,982 |
| `InstancedStaticMeshComponent` | 102 | 102 | 102 | 102 | 417,074 |
| `ModelComponent` | 161 | 161 | 161 | 161 | 366,142 |
| `SpeedTreeComponent` | 4 | 4 | 4 | 4 | 80 |
| `FluidSurfaceComponent` | 2 | 2 | 2 | 2 | 4,704 |
| `ShadowMap1D` | 20 | 20 | 20 | 20 | 17,868 |

- 24,557 of the static mesh components are in maps; the other 71 are component templates in `Startup`,
  `UTGameContent`, `GameFramework` and `UDKBase` (all with an empty `LODData`, 4 bytes). The script subclass
  `UTGame.UTGibStaticMeshComponent` adds no native data: its 2 template instances decode with the
  `StaticMeshComponent` layout (T). Class default objects of these classes carry no native data and are skipped.
- No `TerrainComponent` ships. `DecalComponent` (469 exports) also serializes static receivers with their own vertex
  light maps; it is not decoded here (decals are not rendered by the runtime yet). UNKNOWN layout.
- Light map references (T): 24,575 texture light maps (`FLightMap2D`), 784 vertex light maps (`FLightMap1D`), 326
  references without a light map. 5,364 references to `ShadowMap2D` and 20 to `ShadowMap1D`; 161 LODs carry
  per-component vertex colours (`OverrideVertexColors`, all non-empty), with 177,007 mesh-paint source vertices;
  4,543 instanced-mesh instances; 223 BSP model elements (T).

## Native layouts — CONFIRMED (T, exact consumption and re-encode)

All little-endian. "obj" = `i32` package index; "bulk record" = the 16-byte `FUntypedBulkData` header of
`TEXTURES.md` followed by its inline payload.

```text
light map reference          u32 Type: 0 none | 1 FLightMap1D | 2 FLightMap2D, then the light map
FLightMap1D (vertex)         TArray<FGuid> LightGuids
                             obj Owner                       (T: always the component itself, 784 of 784)
                             bulk record DirectionalSamples  elements of 8 bytes = 2 x FColor (coefficients 0, 1)
                             3 x FVector ScaleVectors        coefficients 0, 1, 2
                             bulk record SimpleSamples       elements of 4 bytes = 1 x FColor (coefficient 2)
FLightMap2D (texture)        TArray<FGuid> LightGuids
                             3 x { obj Texture, FVector ScaleVector }   coefficients 0, 1, 2
                             FVector2D CoordinateScale, FVector2D CoordinateBias

StaticMeshComponent          TArray<FStaticMeshComponentLODInfo> LODData        (one entry per mesh LOD)
FStaticMeshComponentLODInfo  TArray<obj> ShadowMaps            (ShadowMap2D)
                             TArray<obj> ShadowVertexBuffers   (ShadowMap1D)
                             light map reference
                             u8 bHasOverrideVertexColors, then FColorVertexBuffer when 1:
                                 u32 Stride, u32 NumVertices, bulk TArray<FColor> (element size 4) when NumVertices != 0
                             TArray<FPaintedVertex>            FVector Position, FPackedNormal Normal, FColor Color
InstancedStaticMeshComponent StaticMeshComponent data
                             bulk TArray<instance> (element size 80):
                                 FMatrix Transform, FVector2D LightmapUVBias, FVector2D ShadowmapUVBias
ModelComponent               obj Model | i32 ZoneIndex | TIndirectArray<FModelElement> Elements
                             | u16 ComponentIndex | TArray<u16> Nodes
FModelElement                light map reference | obj Component | obj Material | TArray<u16> Nodes
                             | TArray<obj> ShadowMaps | TArray<FGuid> IrrelevantLights
SpeedTreeComponent           5 light map references            (T: all type 0 in the 4 shipped components)
FluidSurfaceComponent        1 light map reference             (both shipped: type 2)
ShadowMap1D                  TArray<f32> Samples | FGuid LightGuid
ShadowMap2D                  tagged properties only (no native data): Texture, CoordinateScale, CoordinateBias,
                             LightGuid, bIsShadowFactorTexture
```

- `ShadowMap2D` (5,326 exports in the maps, each naming a `ShadowMapTexture2D`) ends exactly after its tagged
  properties (T).
- `UPrimitiveComponent::Serialize` writes nothing to disk in this build (its only extra field is serialized for
  in-memory archives); the static mesh component tail starts directly with `LODData`. The older-version branches the
  serializers keep (vertex colours stored as a plain colour array, a 4-coefficient light map, a pre-`LODData` field)
  are never taken by version 868 data.
- Every sample bulk record is inline and uncompressed, with `SizeOnDisk = ElementCount x element size` and
  `OffsetInFile` equal to its own absolute stream position (T, through the re-encode). The decoder refuses any other
  form rather than guess.
- A vertex light map has exactly one sample per vertex of the matching LOD of the component's static mesh (T: 682 of
  the 784, whose mesh is in the same package; the other 102 reference a mesh in another package and were not
  checked). LODs past 0 also carry light maps (1,192 texture / 37 vertex on LOD 1, 808 / 37 on LOD 2).
- `InstancedStaticMeshComponent`: 101 of 102 have one `ShadowMap2D` per instance; every one has one LOD with a
  texture light map whose per-instance `LightmapUVBias` offsets the shared rectangle (STRONG, from the field names and
  counts; the runtime does not draw instanced components yet).
- Names of fields and flags are UE3 conventions (TENTATIVE where only the layout is proven); order and sizes are
  CONFIRMED.

## Coefficient textures — CONFIRMED (T)

UE3 stores three light map coefficients per sample: two *directional* ones and one *simple* (non-directional) one.

- Every `LightMapTexture2D` (5,442) is named either `NormalizedAverageColor<n>_<m>` (2,721) or
  `DirectionalMaxComponent<n>_<m>` (2,721) (T). Every 2D light map's texture 0 is a `NormalizedAverageColor` and its
  texture 1 the `DirectionalMaxComponent` of the same atlas (same suffix, 24,575 of 24,575) (T). Texture 2, the
  simple coefficient, is **null in every shipped light map** (T): the cook kept only the two directional textures.
  The 219 `ShadowMapTexture2D` are named `ShadowMapTexture2D_<n>` (T).
- Which coefficients the renderer binds (CONFIRMED, executable, `FLightMap2D::GetInteraction`): with directional
  light maps allowed it uses textures 0 and 1 with scale vectors 0 and 1 (two coefficients); otherwise only texture 2
  with scale vector 2 (one coefficient). The light map remembers at load time whether directional light maps were
  allowed. The shipped `Engine/Config/BaseSystemSettings.ini` sets `DirectionalLightmaps=True` in `[SystemSettings]`
  and `False` only in the mobile and Flash buckets, and `ASAMU/Config/DefaultSystemSettings.ini` does not override it,
  so the PC/Mac game renders the two directional textures (CONFIRMED, config). Since texture 2 is absent anyway, the
  simple path cannot be used with the shipped data.
- Vertex light maps keep all three coefficients (2 directional colours per sample + 1 simple colour).
- Scale vectors (T, over all 2D light maps that receive light): scale 0 is 1 within 2.4e-7 in every channel, scale 1
  is per channel and reaches 16; scale 2 is still stored although its texture is absent (components from 1.0 to
  14.6, about 78% of them within 0.05 of 1 or 2; independent decoder, changed 2026-10-10 from "near 1 or 2").
  A texel is multiplied by its coefficient's scale vector (CONFIRMED: the interaction passes texture and scale
  together; per-component scales with a shared atlas mean the stored texels are normalized per light map).
- 24,573 of the 24,575 rectangles (`CoordinateBias` .. `CoordinateBias + CoordinateScale`) lie inside `[0, 1]`; the
  other 2 both belong to `InstancedStaticMeshComponent`s (foliage), one with a bias of about -2.5e13: for instanced
  components the shared rectangle is completed by each instance's `LightmapUVBias` and is not usable on its own (T for
  the counts and classes; the reading is STRONG). (changed 2026-10-10: these are now asserted.)
- Reading of the names (TENTATIVE): coefficient 0 holds the light's colour normalized to its brightest channel,
  coefficient 1 holds, per channel, the brightest component of the lighting along each of three basis directions.
  Data support: inside light map regions, texels whose `DirectionalMaxComponent` is above zero have a
  `NormalizedAverageColor` whose largest channel is mostly 224–255, while texels without light are black in both
  (local probe over AG-Workshop, `--check` prints the per-map histogram; for example AG-Workshop: 833,362 texels in
  the darkest bin, 1,007,922 in the brightest of 2,276,200). Not exact: dim texels have smaller maxima (DXT1 and
  quantisation may explain part of it).

### How the shipped game combines them — UNKNOWN (approximated)

The shaders that turn the two coefficients and the surface normal into light ship only as compiled shader caches and
as obfuscated shader sources (`Engine/Shaders/Binaries/*.bin`); they were not decoded. What the data says:

- In the 784 vertex light maps (272,592 samples), which keep both forms, the simple colour times scale 2 is compared
  per colour channel with `coefficient0 x scale0 x combine_i(coefficient1_i x scale1_i)` (channels where either side
  is below 1e-3 are left out) (T, `simple_coefficient_matches_the_mean_combination`, which prints the table):

  | Bytes decoded as | combine = mean | sum | max |
  |---|---:|---:|---:|
  | linear (660,243 channel samples) | ratio 1.014, log sd 0.338 | 0.338, 0.339 | 0.730, 0.375 |
  | sRGB (610,351 channel samples) | ratio 1.152, log sd 0.376 | 0.389, 0.389 | 0.746, 0.445 |

  ("ratio" = geometric mean of simple / predicted; "log sd" = standard deviation of its logarithm.) The **mean** is
  the only combination without a systematic bias; the typical scatter is a factor of about 1.4. The simple
  coefficient is baked with normal maps (`bUseNormalMapsForSimpleLightMaps=TRUE` in `BaseEngine.ini`), which adds
  scatter. So the product with the mean of the three scaled max components is a fair stand-in for the irradiance an
  unperturbed normal receives (TENTATIVE). (changed 2026-10-10: the earlier figures, log sd 0.28 / 0.36 and ratios
  1.03 / 1.15, came from an uncommitted probe with a different sample filter and could not be reproduced; the table
  is now produced by the committed test and matched by the independent decoder.)

### Colour space — STRONG

`LightMapTexture2D` has no script class and no default object; its native `InitializeIntrinsicPropertyValues` only
sets `LODGroup` to `TEXTUREGROUP_Lightmap` (17; `ShadowMapTexture2D::Serialize` likewise sets
`TEXTUREGROUP_Shadowmap`, 22) (CONFIRMED, executable; enumerator values from `Engine.u`). Everything else comes from
`Engine.Texture`'s defaults, where `SRGB` is `true` (CONFIRMED: `asamu-inspect defaults Engine.u Texture2D
--inherited`). Tagged properties are deltas against the defaults, and no light map tags `SRGB` (`TEXTURES.md`), so
light map textures are sampled with sRGB decoding (STRONG). This settles the "native defaults UNKNOWN" note of
`TEXTURES.md` for `SRGB` and `LODGroup`. 164 of the 219 shadow maps tag `SRGB=false`; the rest keep `true`.

The transfer function of the vertex light map samples (read by the vertex shader, not a texture) is UNKNOWN; the fit
above slightly favours linear bytes. The importer decodes them like the textures (TENTATIVE).

## Light GUIDs: which lights are baked — CONFIRMED (T)

- A light map's `LightGuids` are the `LightmapGuid` property of light components (153,514 of 153,534 references,
  matched over all maps), never their `LightGuid` (0) (T). Streamed sub-levels (`TheCore`, `Freds_place`) are lit by
  lights of the level that streams them (931 references resolve only across packages). The other 20 references name
  no shipped light (stale, TENTATIVE).
- `ShadowMap2D.LightGuid` and `ShadowMap1D.LightGuid` are a light's `LightGuid` (5,345 of 5,346; 1 unmatched in
  AG-Darkcave) (T).
- Of the 3,437 light components, 3,242 are baked into at least one light map (3,220 distinct `LightmapGuid`s: 22
  GUIDs occur twice, in lights that `AG-Epilogue` shares with `AG-Workshop`) and 40 cast static shadow maps (the
  dominant directional light of each map and a few point/spot lights), the rest neither (dynamic lights, or lights
  that reach nothing) (T). No light is both baked and shadow-mapped (T). Every light actor of the shipped maps owns
  exactly one light component (independent decoder). In UE3 a light baked into a light map does not render
  dynamically on the primitives that carry that light map; shadow-mapped lights render dynamically and take their
  shadowing from the shadow map. (changed 2026-10-10: "3,220 lights" counted distinct GUIDs, not components.)
- BSP model elements' light maps list no GUIDs (151 texture light maps, 72 elements without a light map); BSP
  elements name lights through `IrrelevantLights` instead.

## BSP light maps — CONFIRMED (structure), STRONG (UV meaning)

`ModelComponent` elements group BSP nodes by material and light map; several elements of one component share one
light map rectangle. The level model's render vertex buffer (`FModelVertex`, `LEVEL_FORMAT.md`) gives each node's
polygon (`iVertexIndex`, `NumVertices`) with its texture UV and its shadow-map UV. Over the 151 BSP elements with a
texture light map the shadow-map UVs lie in `[0, 1]` (0 outside, `--check`), i.e. they are coordinates inside the
element's light map, mapped to the atlas by `CoordinateScale` / `CoordinateBias` like mesh light map UVs (STRONG).

The other 72 elements have no light map (`AG-Workshop` 32, `AG-Epilogue` 31, `AG-IceCave` 6, `AG-Darkcave` 3). They
are still drawn by the original (they are elements of a model component; without a light map UE3 gives them no
static lighting). In `AG-Workshop` they hold 105 of the 877 visible BSP triangles, among them the very large faces of
one enclosing brush. All 223 elements together give exactly the triangle count of the flat visible BSP that
`asamu-import levels` triangulates from the BSP nodes, on every map (`AG-Workshop` 877, `AG-Epilogue` 904,
`AG-Darkcave` 486, `AG-IceCave` 24, `ASAMUFrontEndMap` 747, ...), and in `AG-Workshop` the same area per material
(local comparison of the two converted outputs).

## Light map UV channel of static meshes — STRONG

`LightMapCoordinateIndex` is tagged with 1 (1,211 meshes) or 2 (2) and untagged elsewhere (`MESHES.md`). Tagged
values are deltas against the default, and both 1 and 2 are tagged, so the default is neither: it is 0 (STRONG; the
class has no default object). Every texture-light-mapped component in the shipped maps uses a mesh that has the
channel (`--check`: 0 missing). Supporting check (local, `AG-Workshop`, converted meshes): of the 33 distinct
light-mapped meshes that resolve to channel 0, 29 have a single UV channel (so channel 0 is the only possible one),
and in the other 4 channel 0 lies inside `[0, 1]` except for one stray vertex value.

## Importer: `asamu-import lightmaps`

```sh
asamu-import [--original <install>] [--out <dir>] lightmaps [--map NAME]... [--check [--json]] [--png] [--pretty] [--force]
```

Writes to `<out>/lightmaps/` (the shared safety rules of the other importers: never the repository outside its
ignored `research/`, never the install, never through symlinks):

| File | Content |
|---|---|
| `<map>/atlas_<n>.dds` | one HDR atlas per `NormalizedAverageColor` / `DirectionalMaxComponent` pair: linear RGB irradiance, `DXGI_FORMAT_R9G9B9E5_SHAREDEXP` (DX10 header), one mip |
| `<map>/atlas_<n>.png` | `--png`: Reinhard tone-mapped sRGB preview |
| `<map>/vertex.dds` | vertex light maps reduced to one constant each (4 x 4 texel cells) |
| `<map>.bsp.bin` | geometry of every BSP element: positions, normals, texture UVs, light map UVs, triangles (UE3 world space) |
| `<map>.lightmaps.json` | `format` `asamu-lightmaps`, version 1: atlases; one entry per lit static mesh component (actor, actor slot, component, mesh, kind, atlas, UV rectangle, UV channel, scale vectors, baked light count, shadow maps); BSP elements (material, byte spans; atlas and rectangle only when the element has a texture light map); every light component with `LightGuid`, `LightmapGuid`, `baked`, `shadow_mapped`; statistics |

- Atlas texels: for each light map region, `irradiance = srgb(coefficient 0) x scale 0 x mean_i(srgb(coefficient
  1)_i x scale 1_i)` with that light map's own scale vectors (`lightmap::directional_irradiance`; the approximation
  above). Regions are written twice: first grown by one texel (a guard band so bilinear filtering does not pull in
  unused atlas texels), then their interiors, which always win. Regions never overlap in the shipped data (0 texels
  claimed by two regions with different scales, every map).
- Units: 1.0 is what a light of `Brightness` 1 delivers at normal incidence; UE3 multiplies it with the diffuse colour.
- All 12 maps convert in about 6–12 s (release) with `--check`; the full output is about 153 million atlas texels
  (about 0.6 GB of DDS), AG-Workshop alone 13 atlases (21 MB with previews).
- Instanced static mesh components (`AG-BeautifulCity` 1, `TheCore` 101), SpeedTree and fluid surfaces are counted in
  `stats.skipped` but not written: an instanced component's shared rectangle needs each instance's `LightmapUVBias`,
  which a single draw cannot apply. (changed 2026-10-10: instanced components were counted as skipped but also
  written as ordinary components, with a rectangle that is wrong or out of range for every instance.)
- Every BSP element is written, also the 72 without a light map (no atlas, no rectangle), so that the light-mapped
  BSP covers the whole visible BSP. (changed 2026-10-10: they were left out, and the runtime then hid the flat BSP,
  so their surfaces disappeared whenever light maps were on.)

## Runtime (approximation, `apps/asamu/src/lightmaps.rs`, `crates/asamu-assets/src/lightmaps.rs`)

- Each light-mapped draw gets a Bevy `Lightmap` (the atlas and the component's rectangle). Bevy samples light maps
  through `UV_1`; meshes whose light map is channel 0 and vertex-light-mapped meshes get a copy with a second UV set
  (for the constant kind the rectangle is a point, so any UV reads the component's constant).
- Bevy adds `lightmap x lightmap_exposure x diffuse colour` as indirect light. `lightmap_exposure` is set to
  `directional_lux_per_brightness / pi` of the app's dynamic-light mapping (`asamu_assets::lighting`), because a
  Lambertian surface turns `E` lux into `albedo x E / pi`: a baked brightness-1 light and a dynamic brightness-1
  light then land on the same scale. This is our convention, not a recovered constant.
- Lights whose `LightmapGuid` is in some light map get `affects_lightmapped_mesh_diffuse = false` (they still light
  dynamic objects, as UE3's light environments do); shadow-mapped and unbaked lights are unchanged; the constant
  ambient term no longer reaches light-mapped surfaces.
- The flat level BSP is hidden and replaced by meshes built from the original's BSP vertex buffer with light map UVs
  and the same materials (tangents generated when the flat BSP has them), for the persistent level and every merged
  sub-level (moved by its streaming offset). Elements without a light map are drawn without one. The replacement
  happens only when the light-mapped BSP has exactly the flat BSP's triangle count; otherwise (a sub-level without a
  light map file, or a file from an importer that skipped unlit elements) the flat BSP stays and a warning names
  the counts.
- The plugin starts over when another level is planned (chapter select, story flow). Source meshes whose draws were
  moved to a copy with a second UV set stay loaded for the level's lifetime: the level tracks its assets by id, and
  without that the level never counted as settled (changed 2026-10-10: `--screenshot` then waited for the
  `--exit-after` deadline and the HUD kept showing "streaming assets").
- A light baked into light maps still lights the surfaces that have no light map (the 72 BSP elements above, draws
  without a pairing, dynamic objects); UE3 gives static surfaces without a light map no static light at all, so
  those surfaces are brighter here than in the original.
- What is lost: per-pixel directionality (normal-mapped detail in the baked light), the static shadow maps of
  dominant lights (Bevy's real-time shadows stand in), instanced meshes (not drawn), decal light maps.

Local check (not committed; repeated 2026-10-10 after the fixes above): AG-Workshop converted into `research/local/`,
rendered with `--screenshot` with and without `ASAMU_LIGHTMAPS=0`. 1,078 of the 1,107 draws received a light map
(31 of them constants), 57 of the 61 render lights were recognised as baked, 56 BSP meshes (24 light-mapped, 32
without a light map; 877 of 877 flat BSP triangles) replaced the flat BSP with all 7 materials matched, and the
level's assets settled within a second. With light maps the corridor walls take a dim warm cast and the room beyond
shows a baked pool of lamp light on the wall behind the desk and a lit rug; without them the scene is evenly lit by
the dynamic approximation. Screenshots were deleted after viewing.

## Hostile-input discipline

Every count is checked against the remaining bytes before allocating (GUID, index, node, colour, painted-vertex and
sample arrays; bulk arrays with their exact element size), the light map type and the vertex-colour flag must be
valid, sample records must be inline, uncompressed and self-consistent, and every decoder must end exactly at the
payload end. `tests/lightmap.rs` builds fixtures of every layout with the encoder, checks exact round trips and
self-consistent bulk offsets, truncates every fixture at every offset (always an error), appends a byte (error),
overwrites every byte and every 4-byte position with extreme values (never a panic) and checks the refusals. The
importer's atlas combiner clamps rectangles (including non-finite or inverted ones) to the atlas; the runtime reader
validates stored paths, atlas indices, rectangles and every binary span.

## UNKNOWN / not done

- The exact shader combination of the two directional coefficients with the normal (shader sources ship
  obfuscated); the basis directions behind the three channels of coefficient 1.
- The transfer function of vertex light map samples.
- `DecalComponent` static-receiver data; `TerrainComponent` (none ships).
- Rendering of instanced static meshes, SpeedTree and fluid surfaces; LOD light maps (the runtime draws LOD 0).
- Static shadow maps (`ShadowMap2D` textures) are decoded as references but not applied.
- Light maps of draws spawned after the level load (Kismet streaming) are not applied.
- The shipped shader files (`Engine/Shaders/Binaries/*.bin`, 147 files) are not plain text: `BasePassPixelShader.bin`
  has 7.99 bits of entropy per byte (CONFIRMED, re-checked 2026-10-10). They were not decoded, and no attempt is
  made to undo that encoding. Whether the Mac build's OpenGL renderer evaluates the directional path exactly like
  the executable's `GetInteraction` suggests was not checked separately (TENTATIVE that it does).
