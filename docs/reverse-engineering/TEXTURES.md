# Textures: native data, bulk data, texture file caches, pixel formats

Evidence source: every texture export of the 42 packages under
`~/Library/Application Support/Steam/steamapps/common/A Story About My Uncle/A Story About My Uncle.app/Contents/Resources/ASAMU/CookedMac`
(and `Maps/`), plus the three texture file caches `Textures.tfc`, `CharTextures.tfc` and `Lighting.tfc` next to them,
of the legitimately owned Mac install (Steam build 1822049). Everything was read by our own code in `crates/asamu-ue3`
(`texture.rs`, `bulkdata.rs`, on top of the object decoder in `OBJECT_FORMAT.md`). This page is structure, names and
counts only: no texel data, no image content and no decompiled code.

Builds on `OBJECT_FORMAT.md` (prelude and tagged properties) and `PACKAGE_ANALYSIS.md` (package compression). Neither
changed.

Reproduce:

```sh
cargo run --release -p asamu-import -- textures --check           # every number on this page (writes nothing)
cargo run --release -p asamu-import -- textures --check --json    # the same, machine-readable
cargo test -p asamu-ue3 --test texture_real_data -- --nocapture   # asserts the (T) claims; skips without data
cargo test -p asamu-ue3 --test texture                            # synthetic fixtures, known answers, hostile input
```

`(T)` marks a claim asserted by `crates/asamu-ue3/tests/texture_real_data.rs` against the install.

## Result — CONFIRMED (T)

The 42,026 exports with native data after their tags (see `OBJECT_FORMAT.md`) include 8,908 textures. All of them now
decode exactly:

| Class | Exports | Default objects | Native data consumed exactly |
|---|---:|---:|---:|
| `LightMapTexture2D` | 5,442 | 0 | 5,442 |
| `Texture2D` | 3,216 | 1 | 3,215 |
| `ShadowMapTexture2D` | 220 | 1 | 219 |
| `TextureCube` | 23 | 1 | 22 |
| `TextureFlipBook` | 8 | 1 | 7 |
| `TextureRenderTarget2D` | 4 | 1 | 3 |
| `TextureMovie` | 1 | 1 | 0 (no instance ships) |
| `TextureRenderTargetCube` | 1 | 1 | 0 (no instance ships) |
| `Engine.Texture` itself and its other subclasses (`ScriptedTexture`, `TerrainWeightMapTexture`, `Texture2DComposite`, `Texture2DDynamic`, `TextureRenderTarget`) | 6 | 6 | 0 (default objects only) |

- Class default objects have no native data: their tagged properties end at `SerialSize` (all 13).
- Correction (2026-10-10, independent re-check): the first version of this page counted 5 "other" default objects and
  named `Texture` among them. The decoder did not treat `Engine.Texture` itself as a texture class (only classes
  derived from it), so `Default__Texture` was missed, and the fifth object counted was `Default__TextureRenderTarget`.
  The classifier now includes `Engine.Texture`; no texture with native data was affected.
- Every other texture's prelude, tagged properties and native data consume exactly `SerialSize`: 8,883 with the
  `Texture2D` layout and 25 with only `SourceArt` (22 cubes, 3 render targets).
- Mips: 73,783 records. 58,835 are stored inline in their package, 14,706 in a `.tfc` and 242 are unused (stripped by
  the cooker).
- Every inline record's offset points at its own bytes, and every `.tfc` record lies inside its file.
- Every one of the 73,541 stored mips has exactly the byte size that its format and stored size require. Each one
  loads, and decompresses where it is compressed, to exactly `ElementCount` bytes. In total that is 2,083,163,931 texel
  bytes, from 1,137,933,383 stored bytes.
- `asamu-import textures --dry-run --png` converts all of them in memory without errors. That is 7,701 distinct object
  paths, plus 1,204 copies of a texture cooked into a second package and the 3 render targets, which have no texels.
  The copies are identical to the first copy in format, size and mip count, and also in content: the CRC-32 of every
  loaded mip agrees for all 1,198 copied `Texture2D`/`TextureFlipBook` exports (the 6 copied cubes consist of such
  faces). The content check is from the independent re-check below (CONFIRMED, local probe).

## Native layout — CONFIRMED (T, exact consumption)

All fields are little-endian. `FBulkData` is the 16-byte record header described in the next section, followed by its
payload when the payload is inline.

```text
UTexture            tagged properties
                    FBulkData SourceArt                    always flags 0, ElementCount 0, SizeOnDisk 0 (8,908 of 8,908)
UTexture2D          i32 MipCount, then MipCount x FTexture2DMipMap:
                        FBulkData Data | i32 SizeX | i32 SizeY
                    FGuid TextureFileCacheGuid             never zero (8,883 of 8,883)
                    i32 CachedPVRTCMips count              always 0  \
                    i32 CachedFlashMipsMaxResolution       see below  | mobile and Flash platform data,
                    i32 CachedATITCMips count              always 0   | never filled in this build
                    FBulkData CachedFlashMips              always 0x21 (unused, separate), 0, -1, -1
                    i32 CachedETCMips count                always 0  /
ULightMapTexture2D  UTexture2D | u32 LightmapFlags
```

| Class | Layout after the tags |
|---|---|
| `Texture2D`, `ShadowMapTexture2D`, `TextureFlipBook` | `UTexture2D` |
| `LightMapTexture2D` | `UTexture2D` + `u32 LightmapFlags` |
| `TextureCube`, `TextureRenderTarget2D` | `SourceArt` only |
| `TextureRenderTargetCube` | `SourceArt` only (TENTATIVE: inherited layout, no instance ships) |
| `TextureMovie` | not decoded (no instance ships; the default object has no native data) |

Notes:

- The cached platform arrays are empty in every texture, but `CachedFlashMipsMaxResolution` is non-zero in 621
  textures (values from 4 to 2048), even though their `CachedFlashMips` is unused. It looks like a cook-time leftover
  with no data behind it (TENTATIVE).
- Cube faces are ordinary `Texture2D` exports, normally named `<cube>.CubemapFace0` to `CubemapFace5`. The cube refers
  to them through its `FacePosX`, `FaceNegX`, `FacePosY`, `FaceNegY`, `FacePosZ` and `FaceNegZ` object properties.
- `ShadowmapFlags` is a tagged property of `ShadowMapTexture2D` (205 of 219 tag it). `LightmapFlags` is native data.
- `LightMapTexture2D` has no script class and no default object in the data. Its native constructor defaults (for
  example `SRGB` and `LODGroup`) are therefore UNKNOWN. Lightmaps tag only `SizeX`, `SizeY`, `Format`,
  `MipTailBaseIdx`, `TextureFileCacheName` (5,388), `FirstResourceMemMip` (3,174) and `NeverStream` (54). The decoder
  therefore merges no class defaults into a lightmap's properties (not even those of `Texture2D`, which the native
  constructor may override): for all 5,442 lightmaps the merged properties equal the tagged ones. The importer leaves
  `srgb`, `lod_group` and `filter` out of the manifest for them; their `address` is the enum's zero value `TA_Wrap`
  because nothing is tagged, and the native default is UNKNOWN.

## Bulk data records (`FUntypedBulkData`) — CONFIRMED (T)

```text
u32 BulkDataFlags
i32 ElementCount          elements (bytes, for texture mips)
i32 BulkDataSizeOnDisk    stored bytes: the compressed size when compressed, -1 when unused and "separate"
i32 BulkDataOffsetInFile  inline: absolute offset in the uncompressed package stream; separate: offset in the .tfc
[SizeOnDisk bytes]        only when StoreInSeparateFile is clear
```

Flag values on the 73,783 mip records (T):

| Flags | Meaning | Mips |
|---|---|---:|
| `0x00` | inline, uncompressed | 58,831 |
| `0x10` | inline, LZO (`SerializeCompressedLZO`) | 4 (single-mip UI textures in `ASAMUFrontEndFlash.upk`) |
| `0x11` | in the `.tfc`, LZO (`StoreInSeparateFile`, `SerializeCompressedLZO`) | 14,706 |
| `0x21` | unused, stripped by the cooker (`StoreInSeparateFile`, `Unused`); `ElementCount` 0, `SizeOnDisk` -1, offset -1 | 242 |

- Bit values are read from the data. The UE3 names are conventions, and the bits that never occur are TENTATIVE:
  `0x02` zlib, `0x04` single-element serialization, `0x08` single use, `0x80` LZX. The reader rejects unknown bits and
  more than one compression bit, and reports zlib or LZX payloads as unsupported.
- Inline offsets are absolute positions in the uncompressed stream. For all 58,835 inline mip records and all 8,908
  (empty) `SourceArt` records, `BulkDataOffsetInFile == export SerialOffset + position of the payload` (T). This holds
  in compressed packages too: offsets refer to the rebuilt stream, not to the file on disk.
- Every `.tfc` mip is LZO-compressed. Inline mips are uncompressed except for the 4 above.

### Compressed payloads — CONFIRMED (T)

A compressed payload uses the same layout as a compressed package chunk (see `PACKAGE_ANALYSIS.md`):

```text
u32 Tag 0x9E2A83C1 | u32 BlockSize | u32 CompressedSize | u32 UncompressedSize
ceil(UncompressedSize / BlockSize) x { u32 CompressedSize, u32 UncompressedSize }
blocks back to back, each one complete LZO1X stream
```

- All 14,710 LZO payloads (14,706 in `.tfc` files and 4 inline) use `BlockSize` 131,072. Together they hold 23,967
  blocks (T).
- In every payload, header, table and blocks add up to exactly `SizeOnDisk`, and `UncompressedSize == ElementCount`.
- Every block decompresses with the crate's existing strict LZO1X decoder (`lzo.rs`) to exactly its table size.

## Texture file caches (`.tfc`) — CONFIRMED (T)

`TextureFileCacheName` names the cache: the file `<name>.tfc` in the cooked folder. Textures with no `.tfc` mip may
still carry the name. A `.tfc` has no file header: it is compressed payloads back to back, starting at offset 0.

| Cache | Size (bytes) | Mip records | Distinct ranges | Distinct `TextureFileCacheGuid`s | Bytes no shipped texture references |
|---|---:|---:|---:|---:|---:|
| `Textures.tfc` | 444,637,918 | 8,403 | 4,573 | 1,423 | 1,810,755 in 14 gaps = 53 complete payloads |
| `CharTextures.tfc` | 54,859,175 | 320 | 320 | 81 | 45,700 in 1 gap = 6 complete payloads |
| `Lighting.tfc` | 69,046,031 | 5,983 | 5,983 | 3,303 | 0 |

- No record lies outside its file, and no two distinct ranges overlap.
- A range referenced more than once (`Textures.tfc`: 8,403 records, 4,573 ranges) is the same texture cooked into
  several packages. Its records always carry the same `TextureFileCacheGuid` (0 conflicts, T).
- The unreferenced bytes walk as complete, decompressible payloads with nothing left over. The likeliest explanation is
  texture data whose textures did not end up in any shipped package (TENTATIVE).
- `TextureFileCacheGuid` is not a per-file identifier: each cache has thousands of distinct values. A local probe found
  that it nearly always identifies one texture's cached data (4,797 of 4,807 GUIDs map to one set of ranges, 10 to two).
  STRONG for "identifies the texture's cached data"; the exact rule is UNKNOWN.
- Which textures use which cache: `Textures` for 2,542 `Texture2D` and 7 `TextureFlipBook`, `CharTextures` for 81
  `Texture2D`, and `Lighting` for 3,174 lightmaps and 129 shadow maps (local probe, consistent with the counts above).

## Mip chains — CONFIRMED (T)

- **Order and sizes.** Mips are stored largest first, and mip 0's stored size equals `SizeX` x `SizeY` in every
  texture (8,883). Each further mip halves both edges, never below 1.
- **DXT size clamping.** In the block-compressed formats, the stored `SizeX`/`SizeY` of mips smaller than 4 texels are
  clamped up to 4: 18,134 mips (for example, the 2x2 and 1x1 mips of a DXT1 texture are both recorded as 4x4). Their
  data is one 4x4 block either way. The other 55,649 mips have their natural size, and no mip follows any other rule.
  Converters must derive the natural size from mip 0 and the mip index, not from the stored fields.
- **Chains reach 1x1, with no packed mip tail.** Every multi-mip texture has `floor(log2(max(SizeX, SizeY))) + 1` mips
  (8,517 of 8,517), each stored on its own. 366 textures have one mip (UI art, non-power-of-two images, LUTs).
- **`MipTailBaseIdx`** equals the last mip index in every multi-mip texture (8,517). On the 366 single-mip textures it
  carries no layout information: it is absent on 280, equals the last index a full chain of their size would have
  (`floor(log2(largest edge))`) on 81, and is one more than that on 5 UI textures (T). Correction (2026-10-10): the
  first version said "absent or the full-chain value", which missed the 5.
- **The streaming split.** In a multi-mip texture without `NeverStream`, exactly the stored mips whose larger edge is at
  least 128 texels are in the `.tfc`. The mips of 64 texels and below are inline. Single-mip and `NeverStream` textures
  keep every mip inline. All 8,883 textures follow this rule (T). The `.tfc` mips always precede the inline ones.
- **`FirstResourceMemMip`** (0 when absent) equals the index of the first inline mip, that is, the number of leading
  unused and `.tfc` mips, in all 8,883 textures (T).
- **Stripped mips.** Unused mips are always the leading ones. They occur exactly in multi-mip textures that are larger
  than their LOD group's `MaxLODSize` in the `[SystemSettings]` section of the shipped `ASAMU/Config/DefaultSystemSettings.ini`.
  That value is 1024 for `TEXTUREGROUP_World`, `WorldNormalMap`, `WorldSpecular` and `MobileFlattened`, and 2048 for
  `Character`, `Cinematic`, `Skybox` and `Weapon*`. One mip is stripped per halving needed (T). An independent re-check
  extended this to every multi-mip texture whose LOD group appears in that section, whatever its size: the number of
  unused mips is `log2(largest edge) - log2(MaxLODSize)` when positive and 0 otherwise, with no exception (local probe).
  The 164 lightmaps of 1024 texels (untagged group) and the one `TEXTUREGROUP_ImageBasedReflection` texture (a group the
  section does not list) are 1024 texels at most and keep every mip:

  | LOD group (untagged = `TEXTUREGROUP_World`) | Largest edge | Unused mips | Textures |
  |---|---:|---:|---:|
  | `World` | 2048 | 1 | 155 |
  | `World` | 4096 | 2 | 2 |
  | `WorldNormalMap` | 2048 | 1 | 78 |
  | `WorldSpecular` | 2048 | 1 | 1 |
  | `MobileFlattened` | 2048 | 1 | 4 |
  | `Character` / `Cinematic` / `Skybox` / `Weapon` / `WeaponNormalMap` / `WeaponSpecular` | 2048 | 0 | 19 / 41 / 12 / 8 / 3 / 1 |

  `SizeX`/`SizeY` still give the full size. The largest mip the game can show for these textures is the first stored
  one (for example 1024 for a 2048 `World` texture).

## Pixel formats — CONFIRMED (T) unless noted

`Format` is a byte property typed by `Engine.Texture.EPixelFormat`. Its 30 enumerators are read from `Engine.u` (`PF_Unknown`
= 0 ... `PF_R5G6B5` = 29) and match `texture::PIXEL_FORMATS`. Only five occur, and every texture tags its format:

| Format | Textures | Mips (inline / `.tfc` / unused) | Texel layout (verified by the exact size of every stored mip) | Texel bytes |
|---|---:|---|---|---:|
| `PF_DXT1` | 7,639 | 64,492 (51,851 / 12,482 / 159) | 8 bytes per 4x4 block | 1,205,113,136 |
| `PF_DXT5` | 839 | 5,791 (4,171 / 1,548 / 72) | 16 bytes per 4x4 block | 415,978,320 |
| `PF_A8R8G8B8` | 156 | 1,522 (1,040 / 471 / 11) | 4 bytes per texel, stored B, G, R, A | 420,389,032 |
| `PF_G8` | 236 | 1,849 (1,682 / 167 / 0) | 1 byte per texel | 30,214,641 |
| `PF_V8U8` | 13 | 129 (91 / 38 / 0) | 2 signed bytes per texel (U, V) | 11,468,802 |

- **`A8R8G8B8` byte order: B, G, R, A** — CONFIRMED (T, `a8r8g8b8_is_stored_bgra`). In the colour-grading LUT
  `MapTemplates.lut.LUT_Night` (256 x 16, sixteen 16 x 16 slices), byte lane 2 tracks the in-slice x axis (red,
  correlation 0.995), lane 1 tracks y (green, 0.983) and lane 0 tracks the slice index (blue, 0.970). This matches the
  B, G, R, A order of the binary `Color` struct (`OBJECT_FORMAT.md`). As expected for alpha, lane 3 is the lane that is
  most often constant across a whole mip (8 of 145 textures, against 2 for each colour lane).
- **`V8U8` is signed**: the 13 textures are all `TC_NormalmapUncompressed` normal maps. In all 13, both channels of the
  first stored mip average between -2 and +1 when read as signed bytes (as unsigned bytes a flat normal map would
  average about 128). STRONG.
- **DXT decoding**: the crate's DXT1/DXT5 decoder (`texture::decode_to_rgba8`) was compared with macOS ImageIO
  decoding the DDS files the importer writes. They agree within 1 per channel on DXT1 and DXT5 (palette rounding) and
  exactly on `A8R8G8B8`, which also checks the DDS headers. This was a local check, not in CI, and was reproduced on
  46 DXT files (alpha identical) by the independent re-check below. STRONG.
- `PF_BC5` (`TC_NormalmapBC5`) and `PF_DXT3` never occur. The decoder and DDS writer support them anyway (BC5 through
  a DX10 header).
- How the cooker maps `CompressionSettings` to formats (T counts from `--check --json`, field `format_by_settings`):

  | Setting | Format (textures) |
  |---|---|
  | `TC_Default` | `DXT1` (1,688 + 1 flip book), `DXT5` (832 + 6 flip books), `A8R8G8B8` (13) |
  | `TC_Normalmap` | `DXT1` (508), `A8R8G8B8` (2) |
  | `TC_NormalmapAlpha` | `DXT5` (1) |
  | `TC_NormalmapUncompressed` | `V8U8` (13) |
  | `TC_Grayscale` | `A8R8G8B8` (140), `G8` (14) |
  | `TC_Displacementmap` | `G8` (3) |
  | `TC_VectorDisplacementmap` | `A8R8G8B8` (1) |
  | lightmaps / shadow maps | `DXT1` (5,442) / `G8` (219) |

- Texel layouts are also defined for the other per-texel formats (`texture::PixelFormat::layout`). For `PF_Unknown`,
  `FloatRGB` and the depth, shadow and 1-bit formats, the texel size depends on the platform, and no texture uses them.
  The reader returns `None` for those rather than guess.

## Platform-specific (macOS cook) observations

- The Mac cook stores PC-style formats: DXT1/DXT5 block compression, uncompressed `A8R8G8B8`, `G8`, `V8U8`. CONFIRMED.
- The mobile and Flash caches (`CachedPVRTCMips`, `CachedATITCMips`, `CachedETCMips`, `CachedFlashMips`) are present in
  the layout of version 868 but always empty or unused. CONFIRMED (T).
- The DXT size clamping and the `.tfc` split could be generic UE3 cooker behaviour rather than Mac-specific: no Windows
  cook is available to compare. UNKNOWN.
- Texture mips are little-endian and unswizzled, with no platform tiling. This follows from the exact sizes and from the
  image comparisons above. STRONG.

## Lightmaps and shadow maps

- `LightmapFlags` is 1 on 5,388 lightmaps and 0 on 54 (T). The 54 are exactly the lightmaps tagged `NeverStream` and
  without a `TextureFileCacheName`; the UE3 name of bit 1 is `LMF_Streamed` (name TENTATIVE, correlation CONFIRMED by a
  local probe).
- Lightmaps are DXT1. Shadow maps are G8 (`TEXTUREGROUP_Shadowmap`; 164 of 219 tag `SRGB=false`). Both stream from
  `Lighting.tfc` with the same split as other textures.

## Coverage per package — CONFIRMED (T totals)

"2D-layout textures" = textures with mips (everything except cubes and render targets). No package has a single problem.

| Package | Texture exports (default objects) | Exact | 2D-layout textures | Mips inline / `.tfc` / unused | Formats (textures) |
|---|---:|---:|---:|---:|---|
| `ASAMUFrontEndFlash` | 4 (0) | 4 | 4 | 4 / 0 / 0 | DXT1 2, DXT5 2 |
| `Engine` | 85 (13) | 72 | 71 | 446 / 28 / 0 | A8R8G8B8 2, DXT1 12, DXT5 57 |
| `GameFramework` | 3 (0) | 3 | 3 | 3 / 0 / 0 | A8R8G8B8 1, DXT5 2 |
| `Startup` | 635 (0) | 635 | 624 | 3,798 / 1,152 / 18 | A8R8G8B8 15, DXT1 435, DXT5 162, G8 11, V8U8 1 |
| `Startup_LOC_INT` | 106 (0) | 106 | 106 | 106 / 0 / 0 | DXT5 103, G8 3 |
| `UDKBase` | 4 (0) | 4 | 4 | 4 / 0 / 0 | DXT5 4 |
| `UDKBase_LOC_INT` | 22 (0) | 22 | 22 | 22 / 0 / 0 | DXT5 22 |
| `UnrealEd` | 8 (0) | 8 | 8 | 56 / 6 / 0 | DXT1 4, DXT5 4 |
| `AG-BeautifulCity` | 995 (0) | 995 | 993 | 6,822 / 1,921 / 40 | A8R8G8B8 22, DXT1 877, DXT5 87, G8 6, V8U8 1 |
| `AG-Darkcave` | 1,331 (0) | 1,331 | 1,330 | 9,123 / 1,859 / 12 | A8R8G8B8 7, DXT1 1,265, DXT5 42, G8 16 |
| `AG-Epilogue` | 280 (0) | 280 | 277 | 1,932 / 815 / 14 | A8R8G8B8 9, DXT1 213, DXT5 36, G8 17, V8U8 2 |
| `AG-IceCave` | 1,333 (0) | 1,333 | 1,333 | 9,213 / 1,931 / 52 | A8R8G8B8 9, DXT1 1,257, DXT5 50, G8 17 |
| `AG-ParadiseCave` | 1,305 (0) | 1,305 | 1,304 | 8,799 / 2,175 / 13 | A8R8G8B8 10, DXT1 1,231, DXT5 48, G8 11, V8U8 4 |
| `AG-StarHaven` | 1,674 (0) | 1,674 | 1,673 | 10,954 / 2,053 / 48 | A8R8G8B8 41, DXT1 1,525, DXT5 80, G8 24, V8U8 3 |
| `AG-Workshop` | 337 (0) | 337 | 335 | 2,322 / 1,039 / 15 | A8R8G8B8 19, DXT1 253, DXT5 46, G8 16, V8U8 1 |
| `ASAMUEntry` | 2 (0) | 2 | 2 | 20 / 0 / 0 | DXT1 2 |
| `ASAMUFrontEndMap` | 302 (0) | 302 | 300 | 2,007 / 899 / 15 | A8R8G8B8 15, DXT1 227, DXT5 43, G8 14, V8U8 1 |
| `ASAMUFrontEndMap_LOC_INT` | 32 (0) | 32 | 32 | 32 / 0 / 0 | DXT5 32 |
| `ASAMULegal` | 5 (0) | 5 | 5 | 23 / 0 / 0 | DXT1 4, DXT5 1 |
| `Freds_place` | 46 (0) | 46 | 46 | 322 / 160 / 0 | DXT1 43, DXT5 3 |
| `TheCore` | 412 (0) | 412 | 411 | 2,827 / 668 / 15 | A8R8G8B8 6, DXT1 289, DXT5 15, G8 101 |

The other 21 packages (`Core.u`, `IpDrv.u`, the shader caches, the remaining `_LOC_INT` packages, ...) hold no
textures.

## Importer: `asamu-import textures`

```sh
asamu-import [--original <install>] [--out <dir>] textures [--package <text>]... [--name <text>] [--limit N]
             [--png [--png-max 512]] [--skip-lighting] [--force] [--check [--json]] [--dry-run]
```

- The output goes to `<out>/textures/` (default `<out>`: the user data directory, e.g.
  `~/Library/Application Support/asamu-decomp/converted`). The safety module shared with `asamu-inspect` refuses the
  repository (except git-ignored `research/` subfolders), anything under an `.app` bundle or a `steamapps` folder, and
  symlinked targets. The importer also refuses any output inside the located install root, wherever it is (an install
  given with `--original` / `ASAMU_ORIGINAL_DIR` need not have either marker), including through a symlinked `--out`.
  The output root is validated before any directory is created. Below it, folders are created one level at a time and
  an existing symlink (or non-directory) in the output tree is refused, so a link planted there cannot redirect writes
  into the install or elsewhere. Converted data is copyrighted game data: keep it local and never redistribute it.
- DDS files go to `<Package>/<object path with dots as folders>.dds`. Path components keep only ASCII letters, digits,
  `-` and `_` (anything else becomes `_`), and Windows device names (`CON`, `NUL`, `COM1`, ...) get a `_` prefix. Two
  different objects that would share a file name (after this sanitizing, or by case on a case-insensitive file system)
  are not allowed to share it: the second one fails instead (no shipped texture is affected). DDS files hold every
  stored mip from the first stored one, at natural sizes derived from mip 0. Cube maps get one cube DDS assembled from
  their six face textures. Mapping:

  | Format | DDS pixel format |
  |---|---|
  | `DXT1` / `DXT3` / `DXT5` | FourCC `DXT1` / `DXT3` / `DXT5` |
  | `A8R8G8B8` | RGB + alpha, 32 bits, masks `00FF0000` / `0000FF00` / `000000FF` / `FF000000` (B, G, R, A in memory) |
  | `G8` | luminance, 8 bits |
  | `V8U8` | DX10 header, `DXGI_FORMAT_R8G8_SNORM` |
  | `BC5` | DX10 header, `DXGI_FORMAT_BC5_UNORM` |
  | `G16` | DX10 header, `DXGI_FORMAT_R16_UNORM` |

- `--png` writes RGBA previews (the largest stored mip whose edge fits `--png-max`) with the crate's own block decoder
  and a minimal PNG writer (stored deflate blocks, CRC-32 and Adler-32 written by us). `G8` previews are grey. `V8U8`
  and `BC5` previews show X and Y biased into red and green, with the reconstructed normal Z in blue.
- `manifest.json` maps each qualified object path to its package, class, DDS file, PNG preview, format, size, written
  size, mip count, cube flag, `srgb` (left out for lightmaps), address modes, filter, LOD group, compression setting
  and lightmap or shadow-map flags. A path cooked into several packages is written once, from the first package; the
  others are listed in `also_in`. Reruns merge into the existing manifest (keeping the `also_in` lists recorded by
  earlier runs) and keep existing files unless `--force`.
- `--check` loads and verifies everything and writes nothing. `--dry-run` runs the whole conversion in memory.

## Hostile-input discipline

- Bulk records reject unknown flag bits, conflicting compression bits, negative counts and sizes, and inline payloads
  larger than the remaining bytes.
- Payload reads are bounded by the file or stream length and by `bulkdata::MAX_BULK_SIZE` (256 MiB), checked before
  allocating. Compressed headers are validated: tag, block size up to 16 MiB, block count (the block table must fit
  the stored bytes before it is read), every block but the last exactly `BlockSize`, sums equal to the totals, payload
  inside the stored bytes. The output buffer reserved up front is at most 16 times the stored bytes, so a small header
  that claims 256 MiB cannot reserve it before its blocks fail to decompress.
- Mip arrays are capped at 32 entries, and texel decoding at 16,384 texels per edge (an RGBA preview of that size
  would be 1 GiB; it needs at least 128 MiB of real stored texels to get there).
- `tests/texture.rs` truncates the native data of every fixture texture at every offset, and flips bits and writes
  extreme values at every offset, loading whatever still decodes. It also flips and overwrites every byte of the
  fixture package from the export table on and runs the full decode, coverage and mip loading on each of the
  ~5,500 mutated packages that still parse (about 53,000 texture decodes). It corrupts `.tfc` tags, LZO streams, file
  lengths and unreferenced gaps (garbage, lying headers, broken blocks), checks each malformed compressed-header field
  and cache reads outside the file, and decodes every pixel format at every size from 1x1 to 9x9 with exact and short
  data. None of this panics.

## Independent re-check — CONFIRMED (2026-10-10)

A second texture decoder, written separately as a throwaway Python script under the git-ignored `research/local/`
(not committed), re-derived the claims on this page from the install without using `asamu-ue3`:

- its own package summary, chunk and table parsing, with every decompression (package chunks and texture payloads)
  done by the reference `liblzo2` (`lzo1x_decompress_safe`) instead of `lzo.rs`;
- its own tagged-property walk and texture native-data parser, written from the layouts on this page, and the texture
  class set taken from the `Engine.u` class hierarchy (plus the native-only `LightMapTexture2D`);
- `LODGroup` limits read from `DefaultSystemSettings.ini` by its own parser.

Compared with a per-export dump of the Rust decoder (path, class, tagged `SizeX`/`SizeY`/`Format`/cache
name/`LODGroup`/`NeverStream`/`FirstResourceMemMip`/`MipTailBaseIdx`/`CompressionSettings`/`SRGB`/`ShadowmapFlags`/cube
faces, cache GUID, Flash resolution, `LightmapFlags`, and every mip record's flags, `ElementCount`, `SizeOnDisk`,
offset and stored size): all 8,920 texture exports the Rust side then classified agree, all 73,783 mip records agree,
and the CRC-32 of all 73,541 loaded mips agrees (2,083,163,931 texel bytes decompressed by two independent LZO
decoders). The only difference was `Engine.Default__Texture` (see the correction under "Result"). Every count in the
per-package, per-format, flag, file-cache, mip-chain, stripped-mip, `CompressionSettings` and lightmap tables, the
`TextureFileCacheGuid` statistics (4,807 GUIDs, 4,797 with one set of ranges), the LUT lane correlations and the
cube-face naming (all 132 faces are `CubemapFace0`-`5` children of their cube) was reproduced.

Separately, the DDS files and PNG previews of 50 exported textures (all of `Freds_place` plus one `A8R8G8B8`, `G8` and
`V8U8` sample and the LUT) were checked: every DDS header (size, pitch or linear size, mip count, flags, DX10
extension) was parsed and matches its data length, and macOS ImageIO decodes the DXT1 (43) and DXT5 (3) files within
1 per colour channel of our decoder with identical alpha, and the `A8R8G8B8` files exactly. ImageIO cannot open the
`G8` (luminance) or `V8U8` (DX10 `R8G8_SNORM`) files, so those two mappings are checked structurally only (STRONG).

## UNKNOWN / not yet done

- `TextureMovie` native data (no instance ships). `TextureRenderTargetCube` is assumed to share `UTexture`'s layout.
- `LightMapTexture2D` native defaults (`SRGB`, `LODGroup`, address modes, filter): the class has no default object in
  the data. The executable could answer this.
- The exact meaning of `TextureFileCacheGuid`, and of the non-zero `CachedFlashMipsMaxResolution` values.
- Why 1,810,755 bytes of `Textures.tfc` and 45,700 bytes of `CharTextures.tfc` are not referenced (presumably textures
  that were cooked but not shipped).
- Runtime side: the Bevy loader for the DDS output and the material and shader mapping (other workstreams).
