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
   - Combining two textures keeps one: the one with more channels (colour over mask), on a tie the one with coarser
     tiling (base layer over detail layer), then the first; the other is listed in `dropped_textures`. A lerp with
     a varying alpha keeps the textured side (input A between two textures; the mean between two constants).
   - Vertex colour, `DepthBiasedAlpha`/`DepthBiasedBlend` (soft depth fades), un-mirrored coordinates, clamps of
     textures and powers of textures are ignored with a note; vertex colour is flagged on the channel.
   - Anything else (`Fresnel`, `ReflectionVector`, `CameraVector`, `PixelDepth`, `SceneTexture`, `DestColor`,
     `Transform`, `DotProduct`, non-constant `AppendVector`, `Custom`, ...) is "unsupported": treated as neutral
     inside a product or sum (1 resp. 0, noted), and as unresolved when it is the whole input.
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

### Results (T for the counts; the approximation itself is ours, not a parity claim)

| | All copies | Distinct object paths |
|---|---:|---:|
| materials + instances (no functions, no class default objects) | 1,710 | 1,100 |
| approximated | 1,695 | 1,089 |
| of which lossless | 809 | 473 |
| fallback | 15 | 11 |

- The same path in several packages always approximates identically (610 repeats, 0 differing).
- Distinct paths: alpha modes opaque 640, mask 200, blend 144, add 106, modulate 9, premultiplied 1; lighting
  models Phong 877, Unlit 213, Custom 10; base colour from a texture 769, from the default (black) 255, from a graph
  constant 73, vertex colour 3; 516 normal maps, 365 emissive textures, 64 materials with panning/rotating UVs, 230
  with tiling/offset/fixed rotation/UV channel ≠ 0 (`asamu-import materials --check`; 227 before the verification
  pass added constant-time panners and rotators).
- The 11 distinct fallbacks are unlit materials whose emissive output is built from coordinate math (4
  adventure-suit glow effects), custom HLSL or `If` (6 engine debug view materials) or a non-constant `AppendVector`
  (`EngineMaterials.DefaultDecalMaterial`).
- Most frequent simplifications (distinct paths): two textures added (322) or multiplied (304), lerp between two
  textures by a varying alpha (231), desaturation of a texture (187), lerp between two constants by a varying alpha
  (167), clamp of a texture (165), unsupported `PixelDepth` as a factor (161).
- **Every reference resolves** (T): all 1,983 static-mesh section materials (all LODs), 9,864 component material
  overrides of level actors and 482 BSP surface materials are keys of the approximation set (materials referenced
  as imports, e.g. `EngineMaterials.DefaultMaterial`, are keyed from the package that exports them), and all 3,394
  texture references of the approximations are texture exports (5 of the paths are shared with a non-texture
  export of the same name, see `OBJECT_FORMAT.md`; the texture export is the one meant).

### Limits of the approximation

What a renderer using `materials.json` does **not** get: UE3's lighting models (Phong, custom lighting, two-sided
lighting, subsurface, anisotropy), rim/Fresnel terms, environment/reflection-vector lookups, scene-colour and depth
effects (refraction, soft particles, depth fades), distortion, world-position offset, vertex-colour modulation (only
flagged), world-aligned or screen-space UVs (`Transform`, `AppendVector` of positions), sub-UV/flip-book animation
(the whole texture is bound), time-varying parameters beyond their first key, sine-driven animation, and second
texture layers (listed in `dropped_textures`). Colours are linear; textures carry their own `srgb` flag in
`textures/manifest.json`.

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
- `crates/asamu-assets/src/material_manifest.rs` (another workstream) reads `materials.json`; it uses each
  channel's texture, `value` and the UV channel, scale and offset; it drops `bias`, panning and rotation (its own
  module documentation).
