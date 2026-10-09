# StaticMesh native data (UE3 v868) and the glTF importer

Evidence source: every `StaticMesh` export of the 42 packages under
`~/Library/Application Support/Steam/steamapps/common/A Story About My Uncle/A Story About My Uncle.app/Contents/Resources/ASAMU/CookedMac`
(and `Maps/`) of the legitimately owned Mac install (Steam build 1822049), decoded read-only by our own code in
`crates/asamu-ue3/src/staticmesh.rs`. The field order was read from the unstripped Mac executable's serializers
(local Ghidra decompilation, never committed) and then proven on the data. This page holds structure, names and
counts only: no payload bytes, no mesh data and no decompiled code.

Builds on `OBJECT_FORMAT.md` (prelude and tagged properties, unchanged) and uses the bulk-data record reader of
`crates/asamu-ue3/src/bulkdata.rs` (texture workstream).

Reproduce:

```sh
cargo test --release -p asamu-ue3 --test staticmesh_real_data -- --nocapture    # every number on this page; asserts (T)
cargo test -p asamu-ue3 --test staticmesh --test staticmesh_hostile              # synthetic + hostile payloads
cargo run --release -p asamu-import -- meshes --check --all-lods --collision     # convert + validate in memory, writes nothing
cargo run --release -p asamu-import -- --out <user-local dir> meshes --all-lods --collision
```

`(T)` marks a claim asserted by `crates/asamu-ue3/tests/staticmesh_real_data.rs` against the install.

## Result — CONFIRMED (T)

| | Count |
|---|---:|
| `StaticMesh` exports (class `Engine.StaticMesh`) | **1,512** in 14 packages |
| native tails decoded, ending exactly at `SerialSize` | **1,512 / 1,512** (83,988,600 bytes) |
| re-encoded byte for byte from the decoded fields (`encode_static_mesh_native`) | **1,512 / 1,512** |
| passing every structural cross-check (`validate_static_mesh`, list below) | **1,512 / 1,512** |
| distinct object paths (the same mesh is cooked into several maps) | 882 |
| `FracturedStaticMesh` exports (subclass with more native data; refused by the decoder) | 0 |

Per package (meshes / LOD models / vertices / triangles over all LODs):

| Package | Meshes | LODs | Vertices | Triangles |
|---|---:|---:|---:|---:|
| `Engine.u` | 25 | 25 | 7,345 | 6,340 |
| `Startup.upk` | 57 | 59 | 27,113 | 28,048 |
| `UDKBase.u` | 1 | 1 | 56 | 28 |
| `UnrealEd.u` | 2 | 2 | 94 | 76 |
| `AG-BeautifulCity` | 284 | 300 | 314,791 | 284,163 |
| `AG-Darkcave` | 107 | 115 | 98,895 | 100,181 |
| `AG-Epilogue` | 141 | 143 | 96,065 | 76,699 |
| `AG-IceCave` | 101 | 103 | 120,060 | 118,072 |
| `AG-ParadiseCave` | 118 | 133 | 116,936 | 100,728 |
| `AG-StarHaven` | 247 | 248 | 314,438 | 241,797 |
| `AG-Workshop` | 188 | 190 | 139,103 | 120,729 |
| `ASAMUFrontEndMap` | 156 | 158 | 113,256 | 91,268 |
| `Freds_place` | 27 | 27 | 20,993 | 11,181 |
| `TheCore` | 58 | 58 | 39,390 | 41,101 |
| **Total** | **1,512** | **1,562** | **1,408,535** | **1,220,411** |

LOD models per mesh: 1 on 1,471 meshes, 2 on 32, 3 on 9.

## Layout — CONFIRMED (T, exact consumption and byte-exact round trip)

All little-endian. "obj" = `i32` package index. A **bulk array** (`TArray::BulkSerialize`) is
`i32 ElementSize, i32 Count, Count × ElementSize bytes`; the element size is written even for empty arrays, and the
decoder rejects any size other than the one listed. Type names in the first column are the executable's symbol
names (CONFIRMED); member names follow UE3 conventions and are TENTATIVE unless a section below confirms their
meaning.

```text
UStaticMesh (after the tagged properties)
  FBoxSphereBounds Bounds          FVector Origin, FVector BoxExtent, f32 SphereRadius      (28 bytes)
  obj   BodySetup                  an RB_BodySetup export, or null
  kDOP tree                        TkDOP root bound: f32 Min[3], f32 Max[3]                (24 bytes)
                                   bulk TkDOPNodeCompact[]          element size 6
                                   bulk FkDOPCollisionTriangle[]    element size 8: u16 v0, v1, v2, u16 MaterialIndex
  i32   InternalVersion
  u32   bHasSourceData             0 or 1; when 1 an FStaticMeshRenderData follows (layout below)
  TArray<FStaticMeshOptimizationSettings>   24 bytes each: u8, f32, f32, u8, u8, u8, u32, f32, f32
  u32   (bHasBeenSimplified)
  u32   (bIsMeshProxy)
  TIndirectArray<FStaticMeshRenderData> LODModels     i32 count, then each LOD (below)
  i32   LODInfo count              TArray<FStaticMeshLODInfo>: on load only the count is stored
  FRotator ThumbnailAngle          i32 Pitch, Yaw, Roll
  f32   ThumbnailDistance
  FString HighResSourceMeshName
  u32   HighResSourceMeshCRC
  FGuid LightingGuid
  i32   VertexPositionVersionNumber
  TArray<f32> CachedStreamingTextureFactors
  u32   (bRemoveDegenerates)
  u32   (bPerLODStaticLightingForInstancing)
  i32   (ConsolePreallocInstanceCount)

FStaticMeshRenderData (one LOD)
  FStaticMeshTriangleBulkData RawTriangles   bulk-data record: u32 Flags, i32 ElementCount, i32 SizeOnDisk,
                                             i32 OffsetInFile, then SizeOnDisk inline bytes unless Flags & 1
  TArray<FStaticMeshElement> Elements        sections, below
  FPositionVertexBuffer        u32 Stride, u32 NumVertices, bulk FVector[] (element size 12)
  FStaticMeshVertexBuffer      u32 NumTexCoords, u32 Stride, u32 NumVertices, u32 bUseFullPrecisionUVs,
                               bulk vertex[] (element size 8 + 4·NumTexCoords, or 8 + 8·NumTexCoords at full precision)
                               vertex = FPackedNormal TangentX, FPackedNormal TangentZ, NumTexCoords × UV
                               UV = 2 × f16 (half) or 2 × f32
  FColorVertexBuffer           u32 Stride, u32 NumVertices, bulk FColor[] (element size 4) only when NumVertices != 0
  u32   NumVertices
  FRawStaticIndexBuffer        bulk u16[]   triangle list
  FRawIndexBuffer              bulk u16[]   wireframe (line list)
  FRawStaticIndexBuffer        bulk u16[]   adjacency (12 indices per triangle when present)

FStaticMeshElement (section)
  obj Material, u32 EnableCollision, u32 OldEnableCollision, u32 bEnableShadowCasting, u32 FirstIndex,
  u32 NumTriangles, u32 MinVertexIndex, u32 MaxVertexIndex, i32 MaterialIndex,
  TArray<FFragmentRange> Fragments (i32 BaseIndex, i32 NumPrimitives), u8 bHasPlatformData
```

Notes on the layout:

- **Where it comes from.** The order was read from `UStaticMesh::Serialize`, `TIndirectArray<FStaticMeshRenderData>::Serialize`,
  `FStaticMeshRenderData::Serialize`, `operator<<(FArchive&, FStaticMeshElement&)`, `FPositionVertexBuffer::Serialize`,
  `FStaticMeshVertexBuffer::Serialize` (+ `AllocateData`), `FColorVertexBuffer::Serialize`, `FRawStaticIndexBuffer::Serialize`,
  `operator<<(FArchive&, FRawIndexBuffer&)`, the `TArray<...>::BulkSerialize` instantiations, `FUntypedBulkData::Serialize`,
  `operator<<` for `TkDOP`, `FStaticMeshSourceData`, `FStaticMeshOptimizationSettings` and `TArray<FStaticMeshLODInfo>`
  (all present by name in the unstripped executable), then proven on the data. Ghidra: `tools/ghidra-scripts/DecompileToLocal.java`
  with these names, output to the ignored `research/decompiled/`.
- **Version gates** seen in those serializers, all satisfied by v868: compact kDOP nodes (version > 769; older
  packages stored 32-byte nodes), source data (≥ 823), optimisation settings array (≥ 829) in the 24-byte form
  (≥ 863), the u32 after `bHasBeenSimplified` (≥ 859), high-res source mesh name/CRC (> 531), `LightingGuid` (≥ 600),
  `VertexPositionVersionNumber` (≥ 801), streaming factors (> 796), `bRemoveDegenerates` (> 803), the last two u32
  (≥ 848); in the LOD: the current vertex buffer (≥ 615), adjacency buffer (> 840), section fragments (> 513) and
  the platform-data flag (> 617). Branches that only older packages take (never at v868): a discarded array after
  `InternalVersion` (when it is > 16 and the version < 593), the legacy vertex buffer (< 615), a colour-buffer
  probe (< 842) and an extrusion buffer plus edge arrays (< 686) in the LOD. One more array after
  `ThumbnailDistance` is serialized only when an archive flag that loading leaves clear is set (TENTATIVE: reference
  collection); exact consumption shows it is never stored. (Gates re-read from the local decompilation by the
  verify pass, 2026-10-10.)
- **Vertex types.** The executable instantiates `TStaticMeshFullVertexFloat16UVs<1..4>` and
  `TStaticMeshFullVertexFloat32UVs<1..4>`; the serialized element sizes are 12/16/20/24 and 16/24/32/40. The
  buffer picks one by `NumTexCoords` (1..4) and `bUseFullPrecisionUVs`. Other channel counts are rejected.
- **Platform data.** A non-zero `bHasPlatformData` would be followed by eight more arrays whose element layouts the
  shipped data never exercises; the decoder refuses it instead of guessing. It is 0 in all 2,054 sections (T).
- **Source data.** The decoder handles `bHasSourceData = 1` (a full extra LOD model) but no shipped mesh has it (T).
- **LOD info.** `FStaticMeshLODInfo` elements are only serialized by archives that neither load nor save (reference
  collection), so a package stores just the count. The count equals the number of LOD models on every mesh (T).

## Field values and meaning

### Constant or near-constant fields — CONFIRMED (T)

| Field | Observed |
|---|---|
| `InternalVersion` | 18 on all 1,512 |
| `bHasSourceData` | 0 on all |
| optimisation settings | empty on all |
| `bHasBeenSimplified`, `bIsMeshProxy`, `bPerLODStaticLightingForInstancing`, `ConsolePreallocInstanceCount` | 0 on all |
| `HighResSourceMeshName` | empty on all |
| `LightingGuid` | non-zero on all |
| `CachedStreamingTextureFactors` | 4 entries on all |
| `bRemoveDegenerates` | 1 on 1,352, 0 on 160 |
| `VertexPositionVersionNumber` | 0 .. 37 |
| `RawTriangles` | flags 0, 0 elements, 0 bytes on all 1,562 LODs: the cooker strips the editor triangles but keeps the inline record, whose `OffsetInFile` equals its own position in the uncompressed stream on every LOD |
| wireframe index buffer | empty on all 1,562 LODs |
| adjacency index buffer | present on 1,448 LODs, always 12 indices per triangle; empty on the rest |
| full-precision UVs | none (all UVs are halves) |
| UV channels per LOD | 1: 421, 2: 1,119, 3: 14, 4: 8 |
| vertex colors | 1 LOD of 1 mesh |

### Sections — CONFIRMED (T)

- `MaterialIndex` equals the section's position in every one of the 2,054 sections.
- Every section has exactly one fragment range, equal to its own `(FirstIndex, NumTriangles)`.
- The section's index range lies inside the index buffer, its indices lie in `[MinVertexIndex, MaxVertexIndex]`, and
  the sections cover the whole index buffer (validation, all LODs).
- `Material` resolves to a `Material` (1,702 sections), a `MaterialInstanceConstant` (281) or null (71).
- `(EnableCollision, OldEnableCollision)`: (1, 1) on 1,892 sections, (0, 0) on 158, (0, 1) on 4.
  `bEnableShadowCasting`: 1 on 2,044, 0 on 10.

### Collision — CONFIRMED (T) except the node bytes

- The kDOP collision triangles are LOD 0's triangles of the sections with `EnableCollision != 0`: the vertex indices
  are LOD 0 indices and `MaterialIndex` is the section index. Compared as sets this holds on all 1,512 meshes (31
  meshes have no collision triangles and no collision-enabled triangles). On 2 meshes the kDOP list repeats some
  triangles; on the other 1,510 the lists are equal as multisets.
- Selecting by `OldEnableCollision` instead matches only 1,508 of the 1,512 meshes: the four sections with
  `(EnableCollision, OldEnableCollision) = (0, 1)` have no collision triangles. `EnableCollision` is the selector.
- The node count is a power of two (or zero) on every mesh.
- The root bound contains LOD 0's vertex AABB (1 uu tolerance) on 1,448 of the 1,481 meshes with collision.
- **Node bytes — TENTATIVE.** Each node is six bytes. They are not per-axis min/max bytes of the node's own box:
  "min ≤ max per axis" holds for only 23.8 % of the 332,934 nodes, "min + max ≤ 255" for 24.4 %, and
  "pairs (0,1), (2,3), (4,5) ordered" for 24.8 %. The engine's traversal compares each byte against 127.5 and scales
  it against the parent's bounds, which suggests both children's bounds are quantised relative to the parent. The
  decoder keeps the bytes opaque; collision consumers can rebuild their own hierarchy from the decoded triangles.
- `BodySetup` is set on 508 meshes and always resolves to an `RB_BodySetup` export (simple collision shapes, tagged
  properties; not decoded here).

### Packed normals, tangents and winding — CONFIRMED (T)

`FPackedNormal` is four bytes X, Y, Z, W, each mapping `b / 127.5 − 1`. Over all 1,408,535 vertices:

- `TangentZ` has unit length (±0.02) on 1,396,388 vertices; 3,541 are near zero (length < 0.5, degenerate source
  vertices). `TangentX` has unit length on 1,407,459. `|TangentX · TangentZ| < 0.05` on 1,375,225 (97.6 %).
- `TangentZ.W` is only ever byte 0 (191,659) or 255 (1,216,876): a pure sign. `TangentX.W` is 127 or 128 (unused).
- **Winding:** for 1,215,628 triangles `cross(b − a, c − a)` points against the sum of the three vertex normals and
  for 160 along it. Criterion of these counts: triangles with `|cross| < 1e-6` are skipped, and "against"/"along"
  means a cosine below −0.5 / above 0.5 (the rest are undecided). With a plain sign test instead (only
  `|cross| < 1e-9` skipped) the counts are 1,219,561 against and 850 along (independent re-check, below). UE3's
  front faces therefore wind clockwise with respect to the stored normals.
- **Tangent frame:** at each corner of a triangle whose UV-0 map is non-degenerate (`|det| ≥ 1e-8`), `TangentX`
  follows the triangle's `dP/du` (cosine > 0.5) in 3,524,016 cases against 4,928 (cosine < −0.5), and the bitangent
  `cross(TangentZ, TangentX) · sign(TangentZ.W)` follows `dP/dv` in 3,469,276 cases against 11,803. By plain sign the
  counts are 3,602,111 / 31,999 and 3,584,216 / 49,575. This confirms X = tangent, Z = normal, and the sign
  convention of W.
- **UVs:** 5,313,324 half values, all finite, 5,313,306 within ±64.

Vertex colors are stored as `FColor` in byte order B, G, R, A (STRONG: the struct's member order, `OBJECT_FORMAT.md`;
only one LOD in the game has colors, so there is little data to check). Whether they are sRGB or linear is UNKNOWN.

### Lightmap UV channel — CONFIRMED (T for the counts)

The lightmap channel is the **tagged** property `LightMapCoordinateIndex` (not native data). It is tagged on 1,213
meshes: value 1 on 1,211 and 2 on 2. 138 of those meshes have fewer UV channels than the index needs, i.e. they carry
no usable lightmap channel. `LightMapResolution` is tagged on 1,417 meshes. `Engine.StaticMesh` has no script class
or class default object, so an untagged value's default is not recoverable from the packages (UNKNOWN; the
importer records "absent").

## Structural cross-checks (`validate_static_mesh`)

Run on every decoded mesh by `mesh_coverage` and by the importer; all 1,512 pass (T):

- object references (`BodySetup`, section materials) are in range;
- uncompressed inline raw-triangle records hold `ElementCount × 372` bytes and record their own stream position;
- position/vertex/color/`NumVertices` counts agree, strides equal the element sizes;
- every index (triangle list, wireframe, adjacency) is below the vertex count; wireframe counts are even; adjacency
  has 12 indices per triangle;
- sections stay inside the index buffer, respect their vertex ranges and cover it completely;
- LOD 0 positions lie inside `Bounds` (tolerance 0.01 uu plus 1e-5 relative: large world-space meshes are at the f32
  rounding limit). Coarser LODs are not checked: they may poke slightly outside LOD 0's bounds (seen on 2 meshes);
- collision triangles reference existing LOD 0 vertices and sections;
- the stored LOD info count equals the number of LOD models (added by the verify pass; true on all 1,512).

## Hostile-input discipline

Every count is checked against the remaining bytes before allocating (bulk arrays with their exact element size),
booleans must be 0 or 1, `NumTexCoords` must be 1..4, LOD count is capped at 64, unknown platform data and unknown
bulk-data flags are refused, and the decoder must end exactly at the payload end. Allocation is bounded by the
input: no decoded element takes more than about twice its serialized size. `tests/staticmesh_hostile.rs`
truncates a synthetic mesh at every offset (all rejected), appends a byte (rejected), flips bits and writes extreme
`i32` values at every offset, patches individual fields to invalid values and runs 3,000 deterministic random
mutations plus pure noise; nothing panics. It also checks that every bulk header insists on its element size and
refuses impossible counts, that raw-triangle records with unknown or conflicting flags, negative counts or
oversized inline data are refused, that validation reports extreme values (`u32::MAX` section ranges,
`i32::MIN` references, NaN/infinite positions and bounds) without overflowing, and that **every input the decoder
accepts re-encodes to the same bytes** (20,000 mutations). The only normalisations are inline raw-triangle payload
bytes, which are not kept, and a non-canonical FString encoding of `HighResSourceMeshName` (UTF-16 for a Latin-1
string, or a lone NUL). This property found that the half-float encoder lost NaN payloads (a UV half such as
`0x7c01` re-encoded as `0x7e00`). It now keeps them, so `f32_to_half(half_to_f32(h)) == h` for all 65,536 halves
(`tests/staticmesh.rs`). Shipped UVs are all finite, so the real-data results are unchanged.
`tests/staticmesh.rs` checks a hand-written payload field by field (and that the encoder reproduces it), every
vertex format through encode/decode, half-float conversion over all 65,536 values, and that validation reports each
kind of inconsistency.

## Importer: `asamu-import meshes` (glTF 2.0)

`tools/asamu-import/src/meshes.rs` converts every `StaticMesh` to glTF 2.0 (`.gltf` JSON + `.bin`, written with
`serde_json`; no glTF library) under `<out>/meshes/<Package>/<Path...>/<Name>.gltf`, plus `manifest.json`. Output
goes only to the user-local `--out` directory through the shared safety checks (no repository paths except ignored
`research/`, no `.app`/`steamapps`; existing files kept unless `--force`).

- **Links.** `--out` and `meshes/` themselves are resolved once (`canonicalize`) and checked. Inside the tree no link
  is followed. Directories are created one component at a time, and each new one is checked first. An existing
  directory symlink is refused before anything is created behind it. A file symlink at a target is kept without
  `--force` and refused with it. Relative paths with `..` or a root are refused. Before the verify pass, a
  directory symlink inside the tree (for example into a `.app`) let `create_dir_all` create directories behind it
  before the file check refused the write (unit test `links_inside_the_output_tree_are_refused`).
- **Names.** Path components keep `[A-Za-z0-9_-]` (others become `_`), avoid Windows device names, and are capped
  at 96 bytes (longer names are truncated with a hash suffix). Output names are claimed case-insensitively per run,
  including the files of an existing manifest. A second object path that would reuse a file gets a
  `~<hash>` suffix instead of silently sharing it. Such a collision happens when two names differ only in case,
  sanitise alike, or a mesh is called `X_LOD1` next to LOD 1 of `X`. The shipped data has no such collision: the
  full export writes 1,804 distinct mesh files.
- **Axes:** UE3 `(x, y, z)` → glTF `(y, z, −x)`, the mapping of `asamu_core::coords::ue_dir_to_bevy`
  (determinant −1). **Scale:** `--scale` glTF units per UU, default 1 (UU kept; recorded per mesh).
- **Winding** is kept: the mirror turns UE3's clockwise-front triangles into glTF's counter-clockwise ones.
- **Normals** from `TangentZ` (renormalised; zero normals replaced by the area-weighted face normal),
  **tangents** from `TangentX` orthogonalised against the normal with `w = −sign(TangentZ.W)` (the mirror negates the
  cross product in the bitangent rule above).
- **UVs:** every channel as `TEXCOORD_n` (UE3 and glTF share the top-left origin). **Colors:** `COLOR_0` RGBA.
- **Sections:** one primitive each with a placeholder material named after the UE3 material path (`None` when
  unassigned); `--all-lods` writes `<Name>_LOD<n>.gltf`; `--collision` adds the kDOP triangles as a `UCX_<Name>` node.
- **Manifest:** per object path: package, export index, LOD files and per-section material/triangle counts, UV
  channels, `LightMapCoordinateIndex`, `LightMapResolution`, `BodySetup` path, collision triangle count, UE3 bounds,
  scale and a content hash. A path cooked into several packages is written once; `also_in` lists identical copies,
  `differs_in` (with a `<Name>@<Package>` file) differing ones.
- **Validation:** every written document is re-parsed and checked against its buffer (`validate_gltf`): buffer and
  view bounds and alignment, accessor ranges, `POSITION` min/max equal to the data, equal attribute counts, index
  ranges, unit normals, unit tangents with `w = ±1`, and material/mesh/node/scene references.

Result over the whole install (CONFIRMED, `--check --all-lods --collision`, about 3 s in release):

| | |
|---|---:|
| meshes converted (distinct paths) | 882 (902 LODs, 958,050 vertices, 814,851 triangles) |
| repeated paths in later packages | 630, all identical in content (0 differing) |
| decode failures / glTF validation failures | 0 / 0 |
| zero normals replaced / tangents replaced | 2,499 / 55 |
| triangles whose glTF face normal agrees with the vertex normals | 814,273 (578 disagree) |

A full export (`--all-lods --collision`) is 902 glTF files plus 902 buffers and the manifest (1,805 files, 67 MB). An
independent checker written separately in Python (local, not committed) re-read every file and found the same:
902 files, 2,100 primitives, 0 issues. A second check by the verify pass compared the files with the independent
decoder below: positions (after the axis mapping) and every UV channel are bit-identical on all 902 LODs. Section
index ranges (1,248 primitives) and collision triangles (852 meshes) are identical. Material names match the
section references, and the tangent `w` follows `TangentZ.W`. Every file is referenced by the manifest, and a re-run
over the same tree (with and without `--force`) reproduces the manifest byte for byte.

## Independent re-check (verify pass, 2026-10-10) — CONFIRMED

A second decoder was written in Python from the documented layouts only (this page, `PACKAGE_ANALYSIS.md`,
`OBJECT_FORMAT.md`; local, deleted after use). It used its own summary,
name/import/export and tagged-property parser, `liblzo2` for decompression instead of `lzo.rs`, and its own native
layout reader. It decoded all 42 packages and compared every `StaticMesh` with the Rust decoder's output. Compared
fields: every scalar of the native data, every section field, the bulk-record header, and FNV-1a digests of every
array (kDOP nodes and triangles, positions, tangents, each UV channel after half conversion, colours and the three
index buffers), plus `LightMapCoordinateIndex`/`LightMapResolution` and the qualified path.

- 1,512 / 1,512 meshes decode with exact consumption; **0 differences** in any compared field.
- Every count in the tables above was reproduced: per-package meshes/LODs/vertices/triangles, the LOD histogram,
  83,988,600 native bytes, 882 distinct paths, 0 `FracturedStaticMesh`, the constant fields, sections,
  collision (set equality 1,512, multiset 1,510, `OldEnableCollision` 1,508, node-byte percentages), the root-bound
  containment 1,448 / 1,481, packed-normal and UV statistics, lightmap counts, and the importer totals (882 meshes,
  902 LODs, 958,050 vertices, 814,851 triangles, 630 repeats). The winding and tangent counts match exactly under
  the thresholds stated above.

## UNKNOWN / not done

- Decoding of the compact kDOP node bytes (above).
- Member names marked TENTATIVE in the layout (bytes and order are CONFIRMED).
- Vertex color space; the default `LightMapCoordinateIndex` of untagged meshes.
- `RB_BodySetup` simple collision (boxes, spheres, convex hulls) and `StaticMeshComponent` per-instance data
  (lightmaps, vertex color overrides) — separate objects, not part of this payload.
- Materials are placeholders: binding textures and material parameters belongs to the material work.
- A Bevy loader for the manifest/glTF output.
