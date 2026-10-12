# Materials: native data, expression graphs, instance chains and the approximate PBR export

Evidence source: every `Material`, `DecalMaterial`, `MaterialInstanceConstant`, `MaterialInstanceTimeVarying`,
`MaterialFunction` and `MaterialExpression*` export of the 42 packages under
`~/Library/Application Support/Steam/steamapps/common/A Story About My Uncle/A Story About My Uncle.app/Contents/Resources/ASAMU/CookedMac`
(and `Maps/`) of the legitimately owned Mac install (Steam build 1822049), decoded read-only by our own code in
`crates/asamu-ue3/src/material.rs`. The native field order was read from the unstripped Mac executable's serializers
(local Ghidra decompilation into the ignored `research/decompiled/`, never committed) and then proven on the data.
This page is structure, names, counts and our own description of behaviour: no payload bytes, no material graphs,
no shader code and no decompiled code.

Builds on `OBJECT_FORMAT.md` (prelude, tagged properties, class defaults), `TEXTURES.md` (texture object paths),
`MESHES.md` (section materials) and `LEVEL_FORMAT.md` (component overrides, BSP surfaces). None of them changed.

Reproduce:

```sh
cargo test --release -p asamu-ue3 --test material_real_data -- --nocapture   # every (T) number on this page
cargo test -p asamu-ue3 --test material --test material_hostile              # synthetic + hostile fixtures
cargo run --release -p asamu-import -- materials --check                      # coverage report, writes nothing
cargo run --release -p asamu-import -- materials --check --shader-caches      # + resource Id search (~45 s)
cargo run --release -p asamu-import -- materials --check --name <text> --explain   # how each input was reduced (local output)
cargo run --release -p asamu-import -- --out <user-local dir> materials       # writes <out>/materials/materials.json
```

`(T)` marks a claim asserted by `crates/asamu-ue3/tests/material_real_data.rs` against the install.

## Result — CONFIRMED (T)

| Class | Exports (of which class default objects) | Prelude + tags + native data consumed exactly | Native data re-encoded byte for byte |
|---|---:|---:|---:|
| `Material` | 1,163 (1) | 1,163 | 1,163 |
| `DecalMaterial` | 67 (1) | 67 | 67 |
| `MaterialInstanceConstant` | 478 (1) | 478 | 478 |
| `MaterialInstanceTimeVarying` | 6 (1) | 6 | 6 |
| `MaterialFunction` | 23 (1) | 23 | 23 |
| default objects of `MaterialInstance`, `LandscapeMaterialInstanceConstant` (`Engine.u`) and `PreviewMaterial` (`UnrealEd.u`) | 3 (3) | 3 | 3 |
| `MaterialExpression*` (121 classes) | 17,449 | 17,449 (tags end at `SerialSize`) | — |

- 315,024 bytes of material native data in total; every package's material exports decode (per-package table:
  `asamu-import materials --check`).
- Expression subobjects and material functions carry **only** tagged properties (no native data).
- The 3 `RefShaderCache-*.upk` packages hold one `ShaderCache` export each (130 MB, 597 MB and 133 MB
  decompressed). They contain no materials and are not decoded (see "Shader maps").

Most frequent expression classes (exports): `Multiply` 3,797, `TextureSample` 2,896, `Constant` 2,204,
`ScalarParameter` 958, `Constant3Vector` 828, `Add` 765, `TextureCoordinate` 736, `Comment` 661, `LinearInterpolate`
660, `TextureSampleParameter2D` 522, `VectorParameter` 357, `ComponentMask` 292, `Panner` 265, `OneMinus` 204,
`VertexColor` 195, `Desaturation` 174, `ConstantClamp` 170, `MaterialFunctionCall` 113, `Power` 113.

## Native layout — CONFIRMED (T, exact consumption and byte-exact round trip)

All little-endian; "obj" = `i32` package index; booleans are `u32` 0/1 (the decoder refuses other values).

```text
UMaterial / UDecalMaterial (after the tagged properties)
  u32 QualityMask                 bit 0 = high-quality resource, bit 1 = low-quality resource (other bits refused)
  FMaterialResource               once per set bit, bit 0 first

UMaterialInstance (MaterialInstanceConstant, MaterialInstanceTimeVarying)
  nothing at all                  unless the tagged bool bHasStaticPermutationResource is true; then:
  u32 QualityMask
  per set bit: FMaterialResource, FStaticParameterSet

FMaterialResource
  TArray<FString> CompileErrors
  i32 Count, Count x { obj Expression, i32 Length }     TextureDependencyLengthMap (TMap, stored as its pairs)
  i32 MaxTextureDependencyLength
  FGuid Id
  u32 NumUserTexCoords
  TArray<obj> UniformExpressionTextures
  u32 bUsesSceneColor, u32 bUsesSceneDepth, u32 bUsesDynamicParameter, u32 bUsesLightmapUVs,
  u32 bUsesMaterialVertexPositionOffset
  u32 UsingTransforms
  TArray<FTextureLookup>          { i32 TexCoordIndex, i32 TextureIndex, f32 UScale, f32 VScale }
  u32 (read and discarded by the loader)
  u32 x 3                         FMaterialResource's own fields (the loader discards the first)

FStaticParameterSet
  FGuid BaseMaterialId
  TArray { FName ParameterName, u32 Value, u32 bOverride, FGuid ExpressionGUID }               static switches
  TArray { FName ParameterName, u32 R, G, B, A, u32 bOverride, FGuid ExpressionGUID }          component masks
  TArray { FName ParameterName, u8 CompressionSettings, u32 bOverride, FGuid ExpressionGUID }  normal parameters
  TArray { FName ParameterName, i32 WeightmapIndex, u32 bOverride, FGuid ExpressionGUID }      terrain layer weights
```

Where it comes from: `UMaterial::Serialize`, `UMaterialInterface::Serialize`, `UMaterialInstance::Serialize`,
`UDecalMaterial::Serialize`, `FMaterialResource::Serialize`, `FMaterial::Serialize`,
`FMaterial::FTextureLookup::Serialize`, `UMaterialExpression::Serialize` and the `operator<<` instantiations for
`FStaticSwitchParameter`, `FStaticComponentMaskParameter`, `TArray<FNormalParameter>`,
`TArray<FMaterial::FTextureLookup>` and `TMap<UMaterialExpression*, int>` (all present by name in the unstripped
executable), then proven on the data. `UMaterialInterface::Serialize` and `UMaterialExpression::Serialize` add no
bytes at v868; `UDecalMaterial::Serialize` adds none either. Version gates seen in those functions, all satisfied by
v868: the quality mask (> 857), the three resource fields (> 852), uniform expression textures instead of a legacy
expression set (≥ 656), `bUsesDynamicParameter` (> 557), `bUsesLightmapUVs` (> 644),
`bUsesMaterialVertexPositionOffset` (> 646), the lookup scales (≥ 506), normal parameters (> 630) and terrain layer
weights (> 713). Older packages also carried an extra resource (< 711), never at v868.

UE3 member names are conventions (CONFIRMED where the executable's symbol names them, TENTATIVE for the three
resource `u32`, see below).

### Field values — CONFIRMED (T)

| Field | Observed over the 1,408 resources |
|---|---|
| `QualityMask` | 1 on every material and static-permutation instance (only the high-quality resource ships) |
| `CompileErrors` | empty everywhere |
| `TextureDependencyLengthMap` | 14,594 entries; every key is an expression subobject; `MaxTextureDependencyLength` equals the largest value on all 1,408 |
| `NumUserTexCoords` | 0: 73, 1: 1,123, 2: 190, 3: 22 |
| `UniformExpressionTextures` | 4,043 references, all to texture exports or imports |
| `UsingTransforms` | 0: 1,262, 1: 141, 8: 2, 9: 3 |
| flags | scene colour 17, scene depth 54, dynamic parameter 1, lightmap UVs 1, vertex position offset 14 |
| `TextureLookups` | 2,966; scale 1×1 on 2,867, the rest square scales from 0.6 to 25. `TextureIndex` is below the number of uniform expression textures on 2,858 (TENTATIVE: it indexes the shader map's texture expressions, which this page does not decode) |
| discarded `u32` | one constant value, 0x01081F52, on all 1,408 (leftover; meaning UNKNOWN) |
| resource `u32` × 3 | **`[EBlendMode index, 0, bIsMasked]`** of the (base) material on all 1,228 material resources and on 169 of the 180 instance resources; 11 instances of stock UT character/FX content carry `[0, 0, 0]` although their base is masked or translucent (STRONG for the meaning; the loader discards the first value) |

Static parameter sets (180 instances with `bHasStaticPermutationResource`, T): 256 static switches, 55 component
masks, 0 normal parameters, 0 terrain layer weights; 50 entries have `bOverride` set. **`BaseMaterialId` equals the
`Id` of the base material's resource on all 180**, and an instance's own resource `Id` never equals its base's: a
static permutation is a separately compiled resource.

## Shader maps — STRONG (identified and skipped)

The compiled shaders are not in the materials. They live in the `ShaderCache` export of
`RefShaderCache-PC-D3D-SM3.upk`, `-PC-D3D-SM5.upk` and `-PC-OpenGL.upk` (the `GlobalShaderCache-*.bin` files are not
packages). Evidence that a resource's `Id` keys its shader map: of the 825 distinct resource `Id`s, the 16 serialized
bytes of 778 occur in the decompressed OpenGL cache (T; 2,349 occurrences) and 780 in each D3D cache. All 760
distinct `Material`/`DecalMaterial` `Id`s are found; the 47 missing ones are among the 65 distinct `Id`s of
static-permutation `MaterialInstanceConstant`s (T). The `ShaderCache` layout (and why 47 instance permutations do not appear by `Id`) is UNKNOWN and not
needed: a modern renderer rebuilds shading from the approximation below. `materials.json` keeps each object's
`resource_id` for a future shader-cache decoder.

## Tagged properties used — CONFIRMED

- **Material**: the material inputs `DiffuseColor`, `DiffusePower`, `SpecularColor`, `SpecularPower`, `Normal`,
  `EmissiveColor`, `Opacity`, `OpacityMask`, `Distortion`, `CustomLighting`, ... are tagged structs
  (`ColorMaterialInput`, `ScalarMaterialInput`, `VectorMaterialInput`, `Vector2MaterialInput`) with members
  `Expression`, `OutputIndex`, `Mask`, `MaskR..A`, `UseConstant` and `Constant`. They are stored as deltas against
  `Default__Material` and merged member-wise onto it (`OBJECT_FORMAT.md`). `BlendMode` (`EBlendMode`),
  `LightingModel` (`EMaterialLightingModel`), `TwoSided`, `OpacityMaskClipValue` (default 0.3333 from
  `Default__Material`), `bIsMasked`, `PhysMaterial`, `Expressions`.
- **Enum order** (read from `Engine.u`, CONFIRMED): `EBlendMode` = Opaque, Masked, Translucent, Additive, Modulate,
  ModulateAndAdd, SoftMasked, AlphaComposite, DitheredTranslucent; `EMaterialLightingModel` = Phong, NonDirectional,
  Unlit, SHPRT, Custom, Anisotropic. An untagged value is the first enumerator (Opaque / Phong).
- **Instances**: `Parent`, `bHasStaticPermutationResource`, `ScalarParameterValues` / `VectorParameterValues` /
  `TextureParameterValues` / `FontParameterValues` (name + value + expression GUID). Time-varying instances store
  the same lists with `InterpCurve*` members plus `LinearColorParameterValues`.
- **Expressions**: inputs are `ExpressionInput` structs (`Expression`, `OutputIndex`, masks); the masks copy the
  selected output's channel mask (e.g. a `TextureSample` output 4 has only `MaskA`). Constants, parameters
  (`ParameterName`, `DefaultValue`), `Texture`, `CoordinateIndex`/`UTiling`/`VTiling`, `SpeedX`/`SpeedY`, etc.
- **Material functions** — CONFIRMED by data: a `MaterialFunctionCall` names its function, and its
  `FunctionInputs` / `FunctionOutputs` entries **do not store the object references** to the function's
  `FunctionInput` / `FunctionOutput` expressions in the cooked data. They are bound by GUID: `ExpressionInputId` /
  `ExpressionOutputId` equal the `Id` property of the function's input/output expressions (found when 709 function
  outputs failed to bind by reference).

## Compile semantics the approximation follows — CONFIRMED (executable)

From `FColorMaterialInput::Compile`, `FScalarMaterialInput::Compile`, `FVectorMaterialInput::Compile` and
`FMaterialResource::CompileProperty`:

- An input with `UseConstant` set compiles its `Constant` **even when an expression is connected**. Otherwise the
  connected expression is compiled, masked when `Mask` is non-zero. With neither, the engine uses a fixed default.
- Defaults (constants read from the executable): `EmissiveColor` black, `Opacity` 1, `OpacityMask` 1,
  `DiffuseColor` black, `DiffusePower` 1, `SpecularColor` black, `SpecularPower` 15, `Normal` (0, 0, 1),
  `TwoSidedLightingMask` 0, `AnisotropicDirection` (0, 1, 0), `WorldPositionOffset` 0, `TessellationMultiplier` 1.
- A `Color` constant becomes linear through `FLinearColor(FColor)`: R, G, B via the executable's
  `PowOneOver255Table` (its entries equal `(i / 255)^2.2`, checked against the table data) and A as `A / 255`.

## Approximation for a modern renderer

`MaterialDecoder::approximate` produces one `ApproxMaterial` per material or instance:

1. **Instance chain.** Follow `Parent` from the object to a `Material` (same package first, then the whole package
   set), at most 16 links, refusing cycles. Chain lengths in the data: 1 link 1,228, 2: 444, 3: 30, 4: 8 (T, every
   copy).
2. **Parameters.** Collect scalar, vector and texture overrides from every instance of the chain, the nearest
   instance winning; time-varying instances contribute the first key of their curve (or the plain value). Static
   switches and component masks come from the native static parameter sets of the chain (entries with
   `bOverride`), nearest first; without an override an expression uses its `DefaultValue`.
   A texture override whose value is null does **not** clear the parameter: the next instance up the chain (and
   finally the expression's own `Texture`) decides. CONFIRMED from the executable: the render proxies'
   `FMaterialInstanceConstantResource::GetTextureValue` and `FMaterialInstanceTimeVaryingResource::GetTextureValue`
   find the entry by name and, when its texture pointer is null, ask the parent's proxy instead (read from the
   disassembly). The shipped data has 6 null texture overrides, none of which shadows a texture set further up, and no
   parameter name repeats within one instance's list (T), so this changes no shipped output; the evaluator follows
   the engine anyway (fixed by the verification pass, which found the earlier "null clears" reading).
3. **Graph walk.** Each input of the base material is reduced by a small symbolic evaluator to one of: a constant
   (up to 4 components), a **texture term** `texture.channels × value + bias` with a UV transform, a UV transform,
   vertex colour, time, or "unsupported". Rules, in our words:
   - Constants, scalar/vector/static-bool parameters fold to constants; `Add`, `Subtract`, `Multiply`, `Divide`,
     `OneMinus`, `Lerp` (constant alpha), `ConstantBiasScale`, `ConstantClamp`, `Clamp`, `Power`, `Desaturation`,
     `AppendVector`, `Abs`, `Floor`, `Ceil`, `Frac`, `SquareRoot`, `Sine`, `Cosine` and `If` fold exactly on
     constants.
   - A texture sample (any `TextureSample*`, sub-UV, flip-book, font or texture-object expression) becomes a texture
     term; a texture parameter takes the instance override. `DepthBiasBlend` is a `TextureSample` subclass (its
     super class in `Engine.u`, CONFIRMED) and is sampled like one, its soft depth fade noted. A constant
     multiplies/adds into the term's `value` / `bias` (so `1 - x` is value −1, bias 1). Output and component masks
     select channels (`rgb`, `a`, `g`, ...).
   - A single texture channel combined with an `n`-component constant is broadcast, as the shader compiler does
     for `float1 op floatN`: the channel keeps the one texture channel and `value` / `bias` carry the `n`
     per-component factors (e.g. a greyscale mask × a light colour). The verification pass found that this
     collapsed to the colour's first component before; 47 distinct paths change (T; by channel 43 emissive,
     3 specular, 1 normal), e.g. red lamp glows that had been exported grey.
   - `TextureCoordinate` (channel, tiling), `Panner` (speed × time-input scale added to the panning), `Rotator`
     (speed, centre) and constant arithmetic on coordinates build `uv' = uv[channel] × scale + offset + panning × t`,
     then the rotation. `BumpOffset` passes its coordinates through. A `Panner` or `Rotator` whose `Time` input is
     a constant (or a scalar parameter, which folds to one) is a fixed shift `speed × time` added to `offset`, resp.
     a fixed angle `speed × time` in `rotation_angle`. Expression exports (all copies, independent Python count):
     4 panners and 7 rotators have a `Constant` time, 2 and 18 a `ScalarParameter`; 3 distinct paths end up with a
     fixed rotation on a bound texture (T). Before the verification pass both were silently dropped and the material
     still counted as lossless.
     Panning or coordinate arithmetic applied *after* a rotation cannot be expressed in this transform order; it
     is applied before the rotation, with a note.
   - `StaticSwitchParameter` / `StaticComponentMaskParameter` are resolved statically; material function calls are
     evaluated through the bound function graph (inputs evaluated in the caller), and a function that reduces to
     unsupported math (the shipped image-adjustment and blend functions) is replaced by its textured input.
   - **Combining two textures keeps one**; the other is listed in `dropped_textures`. Kept, in this order: a sample
     whose coordinates follow the mesh's over one whose coordinates could not be derived; the one with more
     channels (colour over a mask channel); in a **product** of two colour samples the one whose texture carries
     colour over one that is grey (see "Texture statistics": a tiling surface texture over the occlusion or weight
     map that modulates it); otherwise, and always in a sum (its terms are peers), the one with the coarser tiling
     (base layer over detail layer), then the first. In the `DiffuseColor` and `SpecularColor` inputs the dropped
     sample stands in by its **mean** (a constant multiplied or added in), so the average colour is kept; an
     emissive colour and an opacity drop it without a stand-in (a glow keeps its strength where its mask lets it
     through, and a mean could push a whole cut-out under its clip value).
   - **Lerp with an alpha that is not a constant.** One side stands for both: input A, except that (a) an alpha
     known to be 1 and above (or 0 and below) everywhere picks input B (or A) exactly, which can only be said of a
     constant or of a texture term's range, and only when nothing but texture detail and vertex colour was left out
     of the alpha; (b) a **distance blend between two tilings of one texture** (alpha: a clamped depth) keeps the
     far tiling, input B, which is what shows over most of the screen (kept near, the finer tiling repeats visibly
     on the cave walls that fill most views); (c) a side that is not representable gives way to the other; between
     two constants the mean is used. A constant alpha that stands in for something that varies (a depth or view
     term treated as neutral on the way) is not taken as a blend weight.
   - **Vertex colour is not exported and counts as white**, the colour of a mesh that was never painted
     (TENTATIVE that this is what the shipped meshes carry; `color * x` is `x`, `color + x` is `1 + x`); the channel
     is flagged. `DepthBiasedAlpha`/`DepthBiasedBlend` (soft depth fades), un-mirrored coordinates, clamps of
     textures and powers of textures are ignored with a note.
   - **Lengths.** `PixelDepth`, `SceneDepth`, `DestDepth`, world positions and `ObjectRadius` are lengths in world
     units, not factors: a constant combined with one (its scale, its offset) stays part of it and is not folded
     into the result. Once clamped it is a factor in a fixed range and neutral from there on. (Before, a depth
     times 0.0005 reduced to the constant 0.0005: as a lerp weight it left rock materials at 0.05 % of their
     texture, i.e. black, and as an opacity it made beams invisible or, with the reader's placeholder, solid.)
   - **Directions.** A component of `ReflectionVector`, `CameraVector` or `LightVector` is centred on 0: scaled by
     a constant it is still such a component, and in a sum it drops out (a lookup at `0.5 + view.x / k` is taken at
     0.5, the middle of the texture, instead of at a constant built from `k`).
   - **Functions of the texture coordinates.** One component of the coordinates (`ComponentMask` of a
     `TextureCoordinate`) is a value along that axis; products, constant powers and `1 - x` of such values are a
     **gradient**: the product of `max(scale * coordinate + offset, 0) ^ power` factors (at most 4). A channel that
     reduces to one is resolved, with the gradient in `gradients`. `AppendVector` of a value along u (or a constant)
     and a value along v (or a constant) of one UV channel is a coordinate transform again.
   - **Texture statistics.** For every sampled texture the decoder reads the largest mip of at most 64 texels a
     side that is stored in the package itself (the small mips are; nothing is read from a texture file cache, so
     the stage still needs the packages only; a texture stored without a mip chain is read whole up to 1,024) and
     computes its mean colour (linear), its mean saturation and whether it is one colour everywhere (decided on the
     largest mip, and only for textures up to 256 texels a side: a small mip of a large texture can average stars
     or thin lines away). Ours: statistics of the user's data, the same on every system. They decide four things: a
     texture of **one colour** is that colour (the white or black placeholder a texture parameter defaults to); a
     texture is **grey** below a mean saturation of 0.028 (occlusion and cavity maps of the shipped data measure up
     to 0.021, coloured surface textures from 0.035; near-grey surface textures such as the black rock fall under
     it and are then decided by the tiling rule); a sample **without a place on the surface** (coordinates from a
     view or reflection vector, or any lookup in a cube map, whatever expression samples it) is the texture's mean
     (a cube map: the mean of its faces); and the mean of a dropped sample (above). Without statistics (texture not
     found, no texels) every rule falls back to what it was.
   - Anything else (`Fresnel`, `SceneTexture`, `DestColor`, `Transform`, `DotProduct`, `Custom`, ...) is
     "unsupported": treated as neutral inside a product or sum (1 resp. 0, noted), and as unresolved when it is the
     whole input.
4. **Output.** Scalar inputs (`Opacity`, `OpacityMask`, `SpecularPower`) read the first component of a wider value
   and are written splatted (TENTATIVE that the shader compiler's cast truncates this way; it is the standard UE3
   behaviour and the first component was already what consumers read: no shipped first component changed).
   `alpha_mode` from `BlendMode` (Opaque → `opaque`; Masked and SoftMasked → `mask` with
   `alpha_cutoff = OpacityMaskClipValue`; Translucent and DitheredTranslucent → `blend`; Additive → `add`;
   Modulate and ModulateAndAdd → `modulate`; AlphaComposite → `premultiplied`). `opacity` is `OpacityMask` for
   `mask` and `Opacity` for the translucent modes. `unlit` = `MLM_Unlit` (show `emissive` without lighting).
   `metallic` is 0 (UE3 Phong has no metalness). `specular_level` is the mean of the specular colour value clamped
   to 0..1. `roughness = sqrt(sqrt(2 / (p + 2)))` for the specular power `p`: the Blinn-Phong exponent mapped to a
   Beckmann slope `sqrt(2 / (p + 2))` (a standard equivalence), then to GGX "perceptual roughness" (its square
   root). `emissive_intensity` is the largest emissive component (HDR strength).
5. **Status.** `approximated` when the chain reached a base material and the main colour channel (emissive for
   unlit materials, base colour otherwise) was derived; `fallback` otherwise (neutral grey, no textures).
   `lossless` when no rule above had to simplify anything (no `notes`).
6. **What each channel lost.** While an input is reduced, every rule that leaves something out records its kind:
   `depth` (scene or pixel depth: soft intersections, depth fades, depth-edge outlines, distance blends), `view`
   (Fresnel, camera and reflection vectors), `coordinates` (texture coordinates used as a value that is not a
   gradient), `time` (time used as a value), `vertex_color`, `scene` (the colour behind the surface), `world`
   (world position, world normal, object placement: up-facing blends), `texture` (a second texture) and `other`.
   The entry's `dropped` map lists them per channel. A renderer reads it to tell a value from a stand-in: an
   opacity of 0 with `dropped: depth` is "nothing but depth decided this", not "invisible by design".

### Results (T for the counts; the approximation itself is ours, not a parity claim)

| | All copies | Distinct object paths |
|---|---:|---:|
| materials + instances (no functions, no class default objects) | 1,710 | 1,100 |
| approximated | 1,703 | 1,093 |
| of which lossless | 854 | 489 |
| fallback | 7 | 7 |

(Before the play-session pass of 2026-10-12: approximated 1,695 / 1,089, lossless 809 / 473, fallback 15 / 11.)

- The same path in several packages approximates alike in status and channels (610 repeats; the importer counts
  609 identical entries and 1 that differs only in `texture_stats`: a cave-painting plane whose texture is not
  among the exports of one of its four packages, so no statistics are read there; the first copy in package order
  is written).
- Distinct paths: alpha modes opaque 640, mask 200, blend 144, add 106, modulate 9, premultiplied 1; lighting
  models Phong 877, Unlit 213, Custom 10; base colour from a texture 730, from the default (black) 255, from a graph
  constant 112, vertex colour 3; 491 normal maps, 356 emissive textures, 64 materials with panning/rotating UVs,
  215 with tiling/offset/fixed rotation/UV channel ≠ 0 (`asamu-import materials --check`). The texture counts fell
  because uniform textures and lookups without a place on the surface are constants now (bound texture references
  3,394 → 3,069, T).
- The 7 distinct fallbacks are custom HLSL or `If` (6 engine debug view materials) and a non-constant
  `AppendVector` (`EngineMaterials.DefaultDecalMaterial`). None is used by a level mesh. (The 4 adventure-suit glow
  effects that were fallbacks reduce now.)
- Most frequent simplifications (distinct paths): two textures added (280) or multiplied (243), a dropped texture
  standing in by its mean (228), desaturation of a texture (186), lerp between two constants by a varying alpha
  (176), distance blend between two tilings kept far (162), lerp between two textures by a varying alpha (156),
  vertex colour in a product taken as white (155), a lookup without a place on the surface replaced by its mean
  (151).
- 591 entries have a `dropped` map (channels by kind: texture 900, depth 521, vertex_color 341, world 299, view
  193, coordinates 41, other 38, time 32, scene 24); 2 have gradients (the checkpoint effect and its base); 1 binds
  an object filled at run time (the Workshop monitor's render target).
- **Against the previous manifest** (same install; a local comparison of the two `materials.json` files): of the
  1,100 entries the displayed channel (emissive when unlit, else base colour) changed its **texture** on 108 (99
  of them used by a level mesh), went from a texture to a **constant** on 47 (22 used: uniform placeholders,
  cube-map and reflection lookups), changed only its tiling on 1 and only its **multiplier** on 74 (58 used).
  24 entries displayed a texture at a multiplier of 0.004 or less, i.e. black; 2 still do (an editor preview
  material, and one black-rock tunnel platform whose three factors are all small). Reviewed by name
  in each direction: occlusion/cavity maps → the tiling rock, moss or snow texture of the same material (the
  rock, platform and mountain families of every map); white placeholder → rock tile or ice colour; black rock at
  multiplier 0 → the same texture at its real multiplier; a cube-map "texture" that could not be bound → the
  constant the graph adds to it. Two changes are judgement calls and are listed under "Limits": the village cave
  walls show their black rock where they showed moss (the blend is vertex-painted), and metal props whose two
  textures are both grey keep the occlusion map times the metal's dark mean.
- **Every reference resolves** (T): all 1,983 static-mesh section materials (all LODs), 9,864 component material
  overrides of level actors and 482 BSP surface materials are keys of the approximation set (materials referenced
  as imports, e.g. `EngineMaterials.DefaultMaterial`, are keyed from the package that exports them), and all 3,394
  texture references of the approximations are texture exports (5 of the paths are shared with a non-texture
  export of the same name, see `OBJECT_FORMAT.md`; the texture export is the one meant).

### Limits of the approximation

What a renderer using `materials.json` does **not** get: UE3's lighting models (Phong, custom lighting, two-sided
lighting, subsurface, anisotropy), rim/Fresnel terms, environment/reflection-vector lookups (their mean colour
only), scene-colour and depth effects (refraction, soft particles, depth fades, depth-edge outlines), distortion,
world-position offset, vertex-colour modulation (flagged, taken as white), world-aligned or screen-space UVs
(`Transform`, `AppendVector` of positions), sub-UV/flip-book animation (the whole texture is bound), time-varying
parameters beyond their first key, sine-driven animation, desaturation, and second texture layers (listed in
`dropped_textures`; in diffuse and specular colours their mean is kept). Colours are linear; textures carry their
own `srgb` flag in `textures/manifest.json`.

Graph features of the rock and platform families (`M_definedRock_UpVector_*`, the ice and black-rock variants)
that remain dropped, with what is shown instead:

| Feature of the original | Shown |
|---|---|
| Blend between a rock and a moss / grass / snow / ice layer by how far the surface faces up (world normal), sharpened by a mask texture | the rock layer everywhere (input A); grass-topped rocks have rock tops |
| Occlusion and cavity map of the mesh, blended over the tiling texture (soft light, optionally twice) | its mean as a constant factor: no baked crevice shading |
| Near and far tiling blended by distance | the far tiling at every distance |
| Desaturation of the tiling texture towards the viewer, saturated at grazing angles (Fresnel) | the saturated texture at the desaturated layer's strength |
| Blend between black rock and moss painted per vertex (village cave) | black rock (white vertex colour assumed) |
| Second detail normal map, specular from a diffuse channel | first normal map, constant specular |

A second layer (up-facing blend plus occlusion map) needs a material with two more textures and the world normal
in its shader; the standard material this renderer uses has neither, so the single tiling texture was preferred
over the occlusion map. Where both textures of a product are grey (dark metal over an occlusion map, black rock over
an occlusion map) the tiling rule still keeps the unique occlusion map, now multiplied by the other texture's mean,
so such props are as dark as their metal or rock instead of white.

## `materials.json` schema (format `asamu-materials`, version 1)

```text
{
  "format": "asamu-materials", "version": 1, "notice": "...", "conventions": "...",
  "materials": { "<object path>": Entry, ... },     // keys as referenced by meshes, actors and BSP surfaces
  "coverage": Totals                                 // the --check totals
}
Entry = {
  "package": stem of the package the entry was taken from, "export_index": n,
  "class": "Material" | "DecalMaterial" | "MaterialInstanceConstant" | "MaterialInstanceTimeVarying" | ...,
  "chain": [object path, parent, ..., base material], "base_material": path | absent,
  "status": "approximated" | "fallback", "resource_id": 32 hex digits | absent, "lossless": bool,
  "blend_mode": "BLEND_...", "alpha_mode": "opaque|mask|blend|add|modulate|premultiplied",
  "alpha_cutoff": f32 (mask only), "two_sided": bool, "unlit": bool, "lighting_model": "MLM_...", "decal": bool,
  "base_color" | "emissive" | "specular" | "specular_power": Channel, "normal": Channel (when connected),
  "opacity": Channel (non-opaque modes), "emissive_intensity": f32, "specular_level": f32,
  "roughness": f32, "metallic": 0,
  "phys_material": path | absent,
  "parameters": { "scalars": {name: f32}, "vectors": {name: [r,g,b,a]}, "textures": {name: path|null},
                  "switches": {name: bool}, "component_masks": {name: [r,g,b,a]}, "time_varying": n },
  "textures": [path...], "dropped_textures": [path...] (when any),
  "expressions": {kind: visits}, "unsupported": {kind: count} (when any), "notes": [text...],
  "dropped": { "<channel>": ["depth" | "view" | "coordinates" | "time" | "vertex_color" | "scene" | "world" |
                             "texture" | "other", ...] }                       (channels that lost something)
  "gradients": { "<channel>": { "factors": [ { "uv_channel": i32, "component": "u" | "v",
                                               "scale": f32, "offset": f32, "power": f32 }, ... ] } }
  "texture_stats": { "<texture path>": { "mean": [r,g,b,a], "chroma": f32, "uniform": bool } }   (bound textures)
  "runtime_textures": [path...]            (bound objects filled while the game runs: render targets, movies)
  "also_in": [package...], "differs_in": [package...]   (copies of the same path elsewhere)
}
Channel = {
  "source": "constant" | "expression" | "default",
  "value": [r,g,b,a],            // constant, or multiplier of the texture
  "bias": [r,g,b,a],             // added after the multiplier
  "texture": { "texture": path | null, "parameter": name (when a parameter), "sampler": "2d|normal|cube|subuv|flipbook|movie|font",
               "channels": "rgb" | "a" | ..., "uv": { "channel": i32, "scale": [u,v], "offset": [u,v],
               "panning": [u/s, v/s], "rotation": rad/s, "rotation_angle": rad (only when ≠ 0),
               "rotation_center": [u,v] } }   (when textured),
  "vertex_color": bool, "resolved": bool
}
```

`dropped`, `gradients`, `texture_stats` and `runtime_textures` were added by the play-session pass as optional
fields (absent when empty; the version stays 1: a file written before reads as before, and a reader that ignores
unknown fields is unaffected). `<channel>` is `base_color`, `normal`, `emissive`, `specular`, `specular_power` or
`opacity`. A channel with a gradient equals `value × clamp(product of max(scale × uv[uv_channel].component +
offset, 0) ^ power, 0, 1)` and is `resolved`. `texture_stats.mean` is linear (sRGB texels linearised before
averaging), alpha as stored; `chroma` is the mean of `max(R,G,B) − min(R,G,B)` over the stored texel values.

Values beyond a channel's component count are padded neutrally (value 1, bias 0). A single texture channel is
broadcast: its multiplier and bias are repeated in all four components, or hold per-component values when the graph
tints the channel with a colour. Scalar channels (`opacity`, `specular_power`) are splatted. `rotation_angle` was
added by the verification pass as an optional field (omitted when 0, so earlier files and readers that ignore
unknown fields are unaffected; the version stays 1). In `parameters.textures` a `null` means every instance of the
chain that names the parameter sets it to null (the expression's own texture is then used). Texture paths are the keys of `textures/manifest.json`; parameter names keep
their original case in `parameter` and are lower case in `parameters`. The file is deterministic (two runs produce
identical bytes) and is written only to the user-local `--out` directory with the importer's shared safety checks
(no repository paths except ignored `research/`, no install, no `.app`/`steamapps`, no symlinks; an existing file is
kept unless `--force`).

## Hostile-input discipline

Every count is checked against the remaining bytes before allocating, booleans must be 0 or 1, unknown quality bits
are refused and the decoder must end exactly at the payload end. `CompileErrors` strings must be in the encoding
this build's writer produces (`operator<<(FArchive&, FString&)` writes UTF-16 only when `appIsPureAnsi` fails, and
`appIsPureAnsi` accepts characters below 0x100, both read from the disassembly; an empty string is length 0): a
UTF-16 string of one-byte characters or an empty string with a terminator is refused. Without that rule an accepted
tail could re-encode differently (found by the verification pass; every shipped `CompileErrors` is empty, T). `tests/material_hostile.rs` truncates synthetic
material and instance tails at every offset (all rejected), appends a byte (rejected), flips every bit, writes
extreme `i32` values at every offset, applies 4,000 random multi-byte mutations plus pure noise, and checks that
**every accepted input re-encodes to the same bytes**. Graph walks are bounded: at most 64 nested links and 20,000
expression visits per material (the shipped maximum is 451), 16 chain links, and cycles are detected; tests cover a
self-referencing expression under a 40-level diamond (2^40 paths without the bound), links to non-expressions,
chains of 20 instances, and 1,500 mutated material/expression payloads in synthetic packages. Scanning a material
function's member list is charged to the same work budget. Class names come from the name table, so the
`MaterialExpression` prefix test cannot slice a string blindly: a hostile name with a multi-byte character across
the prefix boundary used to panic (fixed and tested by the verification pass, directly and through a package).
Non-canonical and 4,000 random compile-error strings are tested for refusal or exact re-encoding.
`tests/material.rs`
checks the native layout field by field against hand-written bytes, the encoder, and the approximation of
hand-built graphs (textured masked material with panning and tiling, constant folding, masks, `UseConstant`
precedence, unsupported expressions, a three-link instance chain with a static switch override, broken and cyclic
chains, parameter precedence including a time-varying curve) and, from the verification pass, a mask tinted by a
colour (with a mask picking one tint component), constant-time panner and rotator, panning after a rotation,
`DepthBiasBlend`, scalar inputs fed by vectors, and null texture overrides falling through a three-instance chain.

## Independent re-check (verification pass) — CONFIRMED

A second decoder written from scratch in Python (own package reader with Homebrew `liblzo2`, own tag parser, own
native-tail parser written from the layout above; local and throwaway, nothing committed) re-derived, over all 39
material-bearing packages:

- the census: 1,163 `Material`, 67 `DecalMaterial`, 478 `MaterialInstanceConstant`, 6
  `MaterialInstanceTimeVarying`, 23 `MaterialFunction` exports and the 3 other class default objects; 17,449
  expression exports in 121 classes, all ending at `SerialSize`, with the same top-19 counts; 315,024 native bytes;
  1,408 resources, all with quality mask 1; 180 static-permutation instances;
- every row of the field-value table (dependency map 14,594 entries, 4,043 uniform expression textures, all of them
  textures, 2,966 lookups of which 2,867 1×1 and all square, 2,858 `TextureIndex` below the texture count,
  `NumUserTexCoords` and `UsingTransforms` distributions, flag counts, the constant 0x01081F52, the static-set
  counts with 50 overrides);
- the resource-value rule (1,228 / 1,228 material resources, 169 / 180 instance resources; the 11 exceptions are the
  `CH_*` character instances and one `GDC_Materials` effect), `BaseMaterialId` = base `Id` on every permutation,
  chain lengths 1,228 / 444 / 30 / 8, 825 distinct `Id`s (760 material, 65 instance permutation) and the OpenGL
  shader-cache search (760 / 760 material `Id`s and 18 / 65 instance-only `Id`s found, 2,349 occurrences);
- for all 1,100 entries of `materials.json`: the instance chain, base material, `resource_id`, `blend_mode`,
  `lighting_model`, `two_sided`, `alpha_cutoff`, every scalar / vector / texture parameter, static switch and
  component mask agree exactly; the bound main-colour texture (base colour, or emissive when unlit) is always
  reachable from the base material's input with switches and overrides applied (924 entries; on the 586 whose input
  reaches a single texture it is that texture).

From the executable: `FLinearColor::PowOneOver255Table` equals `(i / 255)^2.2` to 3e-8 (its 256 floats read from
`__DATA`), and the specular-power default 15 is the constant `FMaterialResource::CompileProperty` loads for that
input. Discrepancies the re-check found and fixed: the single-channel tint, constant-time panners and rotators,
`DepthBiasBlend`, null texture overrides, scalar inputs, the `CompileErrors` round-trip hole and the class-name panic
(all described above). `material_real_data.rs` now also asserts the reference counts (1,983 / 9,864 / 482), the
3,394 texture references and the chain lengths it used to print only, plus the parameter-list facts above.

## UNKNOWN / not done

- The `ShaderCache` layout; why 47 static-permutation instance `Id`s do not appear in it; `FTextureLookup`'s exact
  `TextureIndex` space; the discarded `u32` constant; the 11 instance resources whose extras do not follow the
  blend-mode rule.
- Approximation of the unsupported expression kinds listed above; per-material graphs are not exported (only the
  reduced description). A runtime that wants exact UE3 shading would need a graph-to-shader translator.
- `MaterialInstanceTimeVarying` curves beyond the first key, `FontParameterValues`, and `Mobile*` properties of
  `MaterialInterface` (unused by this PC build's renderer) are not applied.
- Panning and rotation of texture coordinates are exported and not animated by the renderer.
- A second texture layer in the renderer (the up-facing blend and the occlusion map of the rock families, a glow
  mask multiplied with a colour texture) and a view-dependent rim for outline shells and beams.
- Whether the shipped meshes carry painted vertex colours where a material blends by them (white is assumed).

## How the renderer uses the description (runtime, ours)

`crates/asamu-assets/src/material_manifest.rs` turns an entry into a `RenderMaterial`
(`crates/asamu-assets/src/materials.rs`), and `apps/asamu/src/converted.rs` turns that into a Bevy standard
material. Every level surface stays a standard material (light maps, shadows and fog work as before); what a
standard material cannot take from the data directly is prepared as a **composed picture** when the level loads
(`apps/asamu/src/material_pictures.rs`): one RGBA image with its mip chain, built from the converted DDS files
(DXT1, DXT3, DXT5, 32-bit RGBA, 8-bit grey; at most 1,024 texels a side) or from a gradient (256 × 256).

| In the entry | Drawn as |
|---|---|
| displayed texture × `value` + `bias` | texture × (`value` + `bias` / the texture's mean from `texture_stats`), at most 1; without the mean the bias is left out |
| `opacity` = the alpha channel of the displayed texture | that texture, alpha × `value` |
| `opacity` from another texture, or another channel of the displayed one, same coordinates | a picture: the displayed texture (or white) in RGB, `clamp(channel × value + bias)` in alpha, the channel as the GPU samples it. A plain factor in 0..1 stays on the material |
| `opacity` or the displayed colour from `gradients` | a picture of the gradient (grey in RGB, the fade in alpha); over a tiled texture the texture gives way to the material's constant colour |
| lit, black base colour, emissive from a gradient | the same picture as base colour texture and as emissive texture |
| emissive × **one channel** of a texture (alpha always; a colour channel of a texture `texture_stats` calls coloured) | a picture of that channel in grey as emissive texture (not the texture's own colours) |
| `opacity` texture with other coordinates than the displayed one, or not converted | PLACEHOLDER: alpha 0.1 for translucent modes, no cut-out for masked |
| `opacity` unresolved, translucent modes | PLACEHOLDER: alpha 0.1 (was: solid) |
| displayed texture in `runtime_textures` | PLACEHOLDER: a switched-off screen, linear (0.012, 0.012, 0.014); the original draws a picture into it |
| displayed texture not converted otherwise, or `status: fallback` | the neutral palette colour of the path (× `value`) |

The three placeholders are render-only stand-ins, not recovered values. Counts over the 664 materials that level
meshes use (all maps): 593 display a texture, 70 a constant colour, 1 a run-time texture (the Workshop monitor's
render target), 0 a texture that was not converted, 0 are fallbacks (the 7 unresolved fallbacks of the set are
engine debug materials no level mesh uses). Composed pictures per level, as logged by the app: AG-Workshop 12,
AG-ParadiseCave 24, AG-BeautifulCity 25, AG-StarHaven 11, AG-IceCave 7, AG-Darkcave 11; none failed.

### The first play session (2026-10-12): causes and what is drawn now

Evidence: the owner's screenshots of our build (local, not in the repository) and offscreen renders of the same
views before and after, on the same converted data except `materials.json`.

| Seen | Cause (CONFIRMED from the entry and the walk, `--explain`) | Now |
|---|---|---|
| Ground and rocks in one flat colour per area, white or grey floating rocks, black cave walls | the reduction kept the mesh's occlusion/cavity map (unique, tiling 1) over the tiling rock texture it modulates, or the white placeholder of an unset occlusion parameter; where a distance blend was involved, a depth scaled by 0.0005 became the lerp weight and left the texture at 0.05 % or 0 | the tiling rock (or moss, snow, black rock) texture at the far tiling, times the mean of the occlusion map; the layer blends above stay dropped |
| Solid cyan quads in the cave | cave-painting planes: masked, black base, constant emissive, the drawing in the alpha channel of a texture that is not displayed; the reader bound an opacity texture only when it was the displayed one | cut-out drawings glowing in the material's colour |
| Cyan shapes over the suit, jars, lever, world map | an unlit translucent shell on every interactable whose opacity is a comparison of scene depths around the pixel (a glow at depth edges, within a distance); depth was neutral, so opacity 1 | the opacity reduces to 0 (`dropped: depth`): the shell is not drawn and the object under it shows. PLACEHOLDER for the original's thin edge glow |
| Teal hanging monitor in the Workshop | its displayed texture is a render target, which has no converted file; the palette colour showed | a switched-off screen |
| Dark slabs at checkpoints | emissive and opacity are the fifth power of the V coordinate (times a colour, times a parameter): "coordinates used as a colour", unresolved, opacity default 1 | a gradient picture: the glow fades along the mesh as in the graph |
| Hard-edged solid light cones under the lamps | unlit translucent beam: falloff textures looked up by the reflection vector and by V, times a parameter, times a clamped depth; the depth's scale constant was folded in (opacity 0.00025) and the reader's placeholder for "another texture" then drew a flat 10 % cone | the falloff texture along V as alpha (U at the middle), times the parameter and the mean of the view-dependent lookup: 4 to 8 % at most. The view-dependent softening of the cone's silhouette stays dropped |
| Flat bright yellow lamp brackets | the lamp's emissive colour is multiplied by the alpha channel of its surface texture (the bulb); the renderer multiplied it by the texture's colours, so brass and shade glowed in their own colours | a picture of the alpha channel as emissive texture: the bulb glows, bracket and shade are lit by the lamp's light. The same for the lanterns (their glass band) |

Time-trial markers (start planes and goal arrow): the start planes are hidden actors in the map and are unhidden
on level load only when `SeqCond_IsTimeTrial` says yes; the goal arrow is visible in the map and is hidden on level
load when it says no (CONFIRMED, converted Kismet of AG-ParadiseCave: `SeqEvent_LevelLoaded` →
`SeqCond_IsTimeTrial` → `SeqAct_ToggleHidden`, with a Matinee on the yes branch). Nothing in their materials hides
them. Our level plan leaves hidden actors out and the app applies Kismet's hide and unhide, so in story play they
are not drawn; a render without gameplay (the fly camera) shows the arrow, now as a cut-out instead of a faint
quad.
