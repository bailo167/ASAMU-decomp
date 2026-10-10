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
cargo test -p asamu-ue3 --test bodysetup_real_data -- --nocapture                # "Simple collision": asserts (B)
cargo test -p asamu-ue3 --test bodysetup --test bodysetup_hostile                # synthetic + hostile body setups
cargo test -p asamu-ue3 --test bodysetup_real_fuzz -- --nocapture                # the shipped payloads cut and corrupted
cargo test -p asamu-import meshes:: -- --nocapture                               # importer, incl. the gated counts (I)
cargo run --release -p asamu-import -- meshes --check --all-lods --collision     # convert + validate in memory, writes nothing
cargo run --release -p asamu-import -- --out <user-local dir> meshes --all-lods --collision
```

`(T)` marks a claim asserted by `crates/asamu-ue3/tests/staticmesh_real_data.rs` against the install, `(B)` one
asserted by `crates/asamu-ue3/tests/bodysetup_real_data.rs`, and `(I)` one asserted by the gated test
`real_simple_collision_counts` in `tools/asamu-import/src/meshes.rs`.

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
- `BodySetup` is set on 508 meshes and always resolves to an `RB_BodySetup` export that is the mesh's own subobject;
  the same reference is also stored as a tagged `BodySetup` property on exactly those 508 (B). It holds the simple
  collision shapes: see "Simple collision" below. **The original collides the pawn with those shapes, not with the
  kDOP triangles, on every mesh whose box switch is on** (and with nothing at all when such a mesh has no body
  setup), so the triangles alone do not describe what blocks the player.

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

## Simple collision: the switches and `RB_BodySetup` — CONFIRMED (B)

Evidence side: the original game only (packages and executable of the Mac install). Nothing here comes from our
runtime. Code: `crates/asamu-ue3/src/bodysetup.rs`, the switch accessors in `staticmesh.rs`, and the importer.
Addresses are those of the Mac executable (Steam build 1822049); constants were read from the file.

### The three switches of a `StaticMesh`

| Tagged property (`BoolProperty`) | Member offset | Native default | Stored in the packages |
|---|---|---|---|
| `UseSimpleLineCollision` | +0x140 | **true** | `false` on 69 exports (65 paths); never `true` |
| `UseSimpleBoxCollision` | +0x144 | **true** | `false` on 80 exports (70 paths); never `true` |
| `UseSimpleRigidBodyCollision` | +0x148 | **true** | `false` on 66 exports (57 paths); never `true` |

- **Names and offsets — CONFIRMED (native).** `UStaticMesh::StaticConstructor` (0x100C5CEC0) registers the three
  as bool properties at those offsets (followed by `UseFullPrecisionUVs` at +0x14C and `bUsedForInstancing` at
  +0x150).
- **Defaults — CONFIRMED (native constant, and consistent with the data).**
  `UStaticMesh::InitializeIntrinsicPropertyValues` (0x100C5E6C0) copies the four words `1, 1, 1, 0` from the constant
  at 0x10172AE20 to +0x140 … +0x14C: the three switches start **on**, `UseFullPrecisionUVs` off. `StaticMesh` is an
  intrinsic class: no package has a script class or a class default object for it, so the packages cannot show the
  default directly. They agree with it: tagged values are stored only where they differ from the default
  (`OBJECT_FORMAT.md`), and of 1,512 exports none stores `true` (B). This settles the first open question of
  `docs/PARITY_FINDINGS.md` section 7: the inferred default "on" is right.
- **Which trace reads which — CONFIRMED (native, same reading as PARITY_FINDINGS V3).**
  `UStaticMeshComponent::LineCheck` (0x100C58790) reads +0x144 (`UseSimpleBoxCollision`) for a trace with a non-zero
  extent and +0x140 (`UseSimpleLineCollision`) for a zero-extent trace. The simple shapes are used only when that
  switch is set, the trace flags 0x20100 are clear, and the pointer at component +0x78 is not null; a null body
  setup (mesh +0xF8) then means "no hit". Otherwise the kDOP triangles are tested. Component +0x78 is the
  component's owner actor (STRONG, verify pass: on a hit the routine copies that pointer into the result at +0x08
  and the component itself at +0x40, and the result's hit time, which it first sets to 1.0, is at +0x28; a placed
  mesh always has an owner). The mesh is read from component +0x250 (null: no hit), and the aggregate handed to
  `FKAggregateGeom::LineCheck` is at body setup +0x70, with the trace-flag bit 0x200 and a constant 0 as its last
  two arguments (their names were not established).
- A swept trace therefore sees three kinds of mesh (distinct object paths, (B) and (I)):

| Body setup | `UseSimpleBoxCollision` | Paths | A swept (pawn) trace hits |
|---|---|---:|---|
| yes | on (not stored) | 297 | the simple shapes |
| no | on (not stored) | 515 | nothing |
| yes | stored `false` | 19 | the triangles |
| no | stored `false` | 51 | the triangles |

  316 paths have a body setup, 70 store the box switch `false`. For line traces: 297 with a body setup and the line
  switch on, 520 with no body setup and the switch on, 65 with the switch stored `false` (19 of them with a body
  setup). Every copy of a path in another package has the same switches and the same shapes (0 differing of 630).
- No body setup is empty: all 508 mesh bodies hold at least one shape (B), so "a body setup with no geometry"
  does not occur in the shipped game. 507 hold convex elements only and 1 a single sphere (160 uu radius on a mesh
  whose half extent is 160 uu; its line switch is off, its box switch on).
- Scalar properties stored on the 316 distinct mesh bodies (I): `PreCachedPhysDataVersion` on all, `PhysMaterial` on
  9, `bNoCollision = true` on 1. What `bNoCollision` does to the mesh's own line checks was not read (UNKNOWN).

### `RB_BodySetup` layout — CONFIRMED (B: exact consumption, byte-exact re-encode, second decoder)

```text
i32 NetIndex
tagged properties            FName Name, FName Type, i32 Size, i32 ArrayIndex, [extra], value   (OBJECT_FORMAT.md)
  scalar tags                BoneName, PhysMaterial, bNoCollision, bBlockZeroExtent, bBlockNonZeroExtent, ...
  PreCachedPhysScale         ArrayProperty: i32 Count, Count × FVector
  PreCachedPhysDataVersion   IntProperty
  AggGeom                    StructProperty KAggregateGeom: a tagged stream ending in None
    SphereElems  ArrayProperty  i32 Count, Count × tagged KSphereElem  { TM, Radius, bNoRBCollision, bPerPolyShape }
    BoxElems     ArrayProperty  i32 Count, Count × tagged KBoxElem     { TM, X, Y, Z, bNoRBCollision, bPerPolyShape }
    SphylElems   ArrayProperty  i32 Count, Count × tagged KSphylElem   { TM, Radius, Length, bNoRBCollision, bPerPolyShape }
    ConvexElems  ArrayProperty  i32 Count, Count × tagged KConvexElem:
      VertexData            ArrayProperty  i32 Count, Count × FVector (12 bytes)
      PermutedVertexData    ArrayProperty  i32 Count, Count × FPlane  (16 bytes)
      FaceTriData           ArrayProperty  i32 Count, Count × i32
      EdgeDirections        ArrayProperty  i32 Count, Count × FVector
      FaceNormalDirections  ArrayProperty  i32 Count, Count × FVector
      FacePlaneData         ArrayProperty  i32 Count, Count × FPlane
      ElemBox               StructProperty Box: FVector Min, FVector Max, u8 IsValid (25 bytes)
  None
native data                  TArray<FKCachedConvexData> PreCachedPhysData:
  i32 Count, Count × { i32 ElementCount, ElementCount × bulk TArray<u8> }
  bulk TArray<u8> = i32 ElementSize (1), i32 ByteCount, ByteCount bytes
```

- **Planes and matrices are stored W first.** A binary struct is written member by member in property-link order,
  where a struct's own members come before the inherited ones: `FPlane` (which extends `FVector`) is `W, X, Y, Z` on
  disk and `TM` (an `FMatrix`, 64 bytes) is four such planes. CONFIRMED (B): only under this order is
  `PermutedVertexData` the permutation of `VertexData` (bit for bit on all 3,446 elements) and the fourth column of
  every `TM` exactly `(0, 0, 0, 1)` (all 36).
- A `BoolProperty` tag has size 0 and its value in one byte after the array index. Every member of every shape is
  stored on every element, `false` booleans included (3,446 + 25 + 5 + 6 elements), and members come in
  declaration order. The decoder insists on that order, on one tag per member, on the exact tag type and struct
  name, and on values that end exactly at the tag size.
- **Native data.** `URB_BodySetup::Serialize` (0x100A96F60) runs its parent's serializer and then the array
  serializer for the member at +0x110 (0x1008F6DD0: a count and, per entry, the element array at 0x10075C8C0,
  whose elements are byte arrays written by `TArray<BYTE>::BulkSerialize`, 0x10075CB50). These are the physics
  middleware's pre-cooked convex meshes, one entry per `PreCachedPhysScale` and one blob per convex element (B).
  Only 12 exports carry any (13 blobs, 35,885 bytes); the other 530 store an empty array (4 zero bytes). The blobs
  are kept opaque: the game's own line and box checks read the properties above, not this data.
- **Class default object.** `Default__RB_BodySetup` ends after its tagged properties: class default objects store
  no native data (`OBJECT_FORMAT.md`). It stores `bBlockZeroExtent`, `bBlockNonZeroExtent`, `bConsiderForBounds`
  (all `true`) and `MassScale` 1.0.
- `COMNudge`, `bSkipCloseAndParallelChecks` and `SleepFamily` are never stored in the shipped packages; the decoder
  reads them by the general tag rules and only the synthetic tests exercise that (TENTATIVE for those three).

Result over all 42 packages (B):

| | Count |
|---|---:|
| `RB_BodySetup` exports | **543** in 13 packages |
| owner: `StaticMesh` / `PhysicsAsset` / none (the class default object) | 508 / 34 / 1 |
| decoded with every payload byte consumed | **543 / 543** (3,750,487 bytes) |
| re-encoded byte for byte from the decoded fields (`encode_body_setup`) | **543 / 543** |
| equal, value for value, to the schema-driven generic decoder (`decode_object` with the Engine script structs) | **543 / 543** (756,768 values) |
| passing every structural cross-check (`validate_body_setup`, below) | **543 / 543** |
| with an `AggGeom` / with an empty one | 542 / 0 |
| convex elements (vertices; planes) | 3,446 (32,524; 33,625) |
| boxes / spheres / capsules | 25 / 5 / 6 |
| with pre-cooked physics data (blobs; bytes) | 12 (13; 35,885) |

Per package: `AG-BeautifulCity` 85, `AG-StarHaven` 85, `AG-IceCave` 56, `AG-Workshop` 49, `AG-ParadiseCave` 48,
`AG-Epilogue` 47, `AG-Darkcave` 46, `ASAMUFrontEndMap` 45, `Startup.upk` 44, `Engine.u` 23, `TheCore` 9,
`Freds_place` 5, `UnrealEd.u` 1. Scalar tags: `PreCachedPhysDataVersion` 542, `BoneName` 34, `PhysMaterial` 16,
`bBlockZeroExtent` 9, `bBlockNonZeroExtent` 9, `bNoCollision` 1, `bConsiderForBounds` 1, `MassScale` 1.

The two decoders are independent in what matters: the generic one is driven by the property and struct
definitions read from the user's `Engine.u` (it knows nothing about body setups), the typed one in
`bodysetup.rs` is written against the layout above and uses no schema.

### Meaning of the fields

Structural cross-checks (`validate_body_setup`; all 543 pass (B)):

- every float is finite and every shape has all of its members;
- `FaceTriData` holds whole triangles of valid vertex indices;
- `PermutedVertexData` is the SIMD regrouping of `VertexData`, bit for bit: the vertices are taken four at a
  time, each group stored as three planes with the four X, the four Y and the four Z coordinates, and an
  incomplete last group filled by repeating that group's first vertex (1,859 elements need no padding, 640 / 593 /
  354 have 1 / 2 / 3 vertices in the last group);
- `ElemBox` is valid and is exactly the vertices' bounding box;
- every `TM` is affine;
- radii, lengths and box sizes are not negative;
- the pre-cooked data has one entry per `PreCachedPhysScale` and one blob per convex element.

Measured on the convex elements (B):

- 4 to 79 vertices per element. Every plane normal has unit length and every plane passes through at least three
  of its element's vertices (within 2e-4 of the element's size).
- `FaceTriData` is a closed surface (2V − 4 triangles) on 3,445 of 3,446 elements. Of its 51,262 triangles,
  50,427 lie in one of the element's planes; `cross(b − a, c − a)` points against that plane's normal on 50,305
  and along it on 122, when the plane is the first one in the list that holds the triangle. All 122 are triangles
  that lie in two or more of the element's planes within the tolerance, and against the plane most nearly
  parallel to them they wind like the rest (verify pass, measured separately in Python: 50,427 against, 0 along).
  The plane normals therefore point **out** of the element and the triangles wind clockwise seen from outside,
  like the render triangles.
- Every `FaceNormalDirections` entry is parallel to a plane normal and every plane normal is listed (28,200
  directions for 33,625 planes: parallel faces share one). Every `EdgeDirections` entry (43,867, unit length) is
  parallel to a triangle edge; 2 elements have an empty list.
- **The elements are not all convex.** On 584 of the 3,446 a vertex lies more than 0.01 uu outside one of the
  element's own planes (1,364 planes; more than 1 uu on 125; the largest 160.9 uu): hulls authored concave, whose
  plane list is one plane per face. A consumer must use each list the way the native check uses it (planes for
  one test, vertices and directions for another) and must not assume the vertices lie inside the planes.

Transforms and sizes:

- `TM` rows are the X axis, the Y axis, the Z axis and the origin (row-vector convention: a local point `p` is at
  `p.x·row0 + p.y·row1 + p.z·row2 + row3`). All 36 are affine with orthonormal axes (1e-4); 16 are unrotated (B).
- **Box:** `X`, `Y`, `Z` are full edge lengths. `FKBoxElem::CalcAABB` (0x100AA40F0) builds the local box
  ±0.5 · scale · (X, Y, Z) (constant 0.5 at 0x101636054; members at +0x40, +0x44, +0x48 after the matrix).
  CONFIRMED (native).
- **Capsule:** `FKSphylElem::CalcAABB` (0x100AA42A0) takes ±0.5 · scale · `Length` (+0x44) along the matrix's
  third row and adds scale · `Radius` (+0x40) on every axis: the axis is local Z, `Length` is the length of the
  cylinder part, and the capsule is `Length + 2·Radius` long. CONFIRMED (native).
- **Sphere:** `Radius` (+0x40), centred on the matrix origin.
- `bNoRBCollision` and `bPerPolyShape` are `false` on all 36 boxes, spheres and capsules (B).
- No static mesh uses a box or a capsule (they occur in the 34 physics-asset bodies only), so the mesh importer's
  box and capsule output is exercised by synthetic tests only.

The shapes against their meshes' own bounds (B, `shapes_sit_in_their_meshes_bounds`; counts first measured by the
verify pass's separate decoder). For each of the 316 distinct mesh bodies, the excess is how far a convex vertex
(or the sphere's extreme point) lies outside the mesh's stored `Bounds` box, relative to that box's largest edge:

| Excess | Bodies | of the 297 a swept trace uses |
|---|---:|---:|
| at most 1 % | 243 | 230 |
| 1 % to 5 % | 60 | 55 |
| more than 5 % | 13 | 12 |

- The median excess is 0.015 % of the mesh's size, and meshes whose local coordinates are tens of thousands of
  units from the origin have their hulls at the same place (`Village_Cave.Village_Entrance_Cave`: mesh box and
  hull both around x −64,600 … −49,400): the shapes are in the mesh's local space, with its axes and its scale.
  CONFIRMED.
- Simple collision is authored by hand and is loose on some meshes. The 13 over 5 %:
  `IceCave_Cave.CoreCollision` (a 21 uu placeholder mesh whose 34 hulls span about 6,400 × 2,700 × 1,500 uu: a
  collision-only asset), `AsamuProps02.Meshes.stairrailing_edge_01` (37 %, 6.6 uu),
  `Village_Cave.Village_Entrance_Cave` (17 %: 12 of its 801 hull vertices, up to 2,561 uu outside),
  `zeth_workshop.Workshop_stairs` (11 %, 40 uu: the hull reaches below and beyond the steps),
  `Star_Haven_Props.prefab_meshes.Winch_Woodlegs2_static`, `IceCave_Props.Meshes.AltarCrystal`,
  `Sanctuary_Props.Altar_Mountain` (box switch off), `Pickups.WeaponBase.S_Pickups_WeaponBase`,
  `IceCave_Mechanics.Meshes.rockcicle_upper`, `Platforms.Meshes.Cave_Boulder_2`,
  `Ice_Cave_Building_Blocks.Meshes.Ice_Boulder_1`, `Sigge.Meshes.MiningMetalContainer` and
  `Foliage_Trees.Tree_Crown2`. In absolute terms 64 of the 297 leave the box by more than 5 uu and 32 by more than
  20 uu. Nothing in the decode is wrong on these (they are bit-identical in both decoders); the pawn collides with
  what the hull says, not with what is drawn.
- The reverse also holds: a hull can cover much less than its mesh (8 bodies cover less than half of the mesh box
  along some axis, 52 less than 90 %; `zeth_darkcave.darkcavepart2_part2`, whose box switch is off, 4 %).

Native checks, for the runtime work (addresses and signatures only; **not read here**; the verify pass confirmed
the symbols and the call targets, and that the aggregate check also calls `FBox::TransformBy` and
`FMatrix::Inverse`):
`FKAggregateGeom::LineCheck` 0x100AA4840 calls `FKConvexElem::LineCheck` 0x100AA5A70, `FKSphereElem::LineCheck`
0x100AA6710, `FKBoxElem::LineCheck` 0x100AA69A0 and `FKSphylElem::LineCheck` 0x100AA7510 (CONFIRMED: call targets),
so all four shape kinds take part. The convex check takes the component scale as a vector, the other three as a
single float. Point checks: `FKAggregateGeom::PointCheck` 0x100AA7E50, `FKConvexElem::PointCheck` 0x100AA8C50,
`FKBoxElem::PointCheck` 0x100AA8740.

### Hostile-input discipline (body setups)

`tests/bodysetup.rs` checks a payload written byte by byte (a second writer, independent of the encoder) field by
field, the exact re-encode, the W-first order, the permutation rule on 0 to 9 vertices, every cross-check, class
default objects, and the decoder and the switches inside a synthetic package. `tests/bodysetup_hostile.rs` cuts
that payload at every offset (all rejected), appends bytes (rejected), writes impossible values into each of its
16 element counts and 36 tag sizes (all rejected), swaps 34 tag types and 8 struct names (all rejected), renames
members to unknown, repeated and out-of-order ones, writes bad booleans, array indices and name references, bad
bulk headers and counts that exceed the data (refused before anything is reserved), then writes extreme `i32`
values and flips every bit at every offset and runs 20,000 deterministic random mutations plus noise. Nothing
panics, and **every input the decoder accepts re-encodes to the same bytes**. A payload of 100,000 distinct scalar
tags decodes and re-encodes exactly (the duplicate check is a hash set, not a scan), and one repeated tag in it is
refused.

That exact re-encode presupposes a name table whose names are distinct without regard to case, which a package's
name table is (no such duplicate among the 77,358 names of the 42 shipped packages). With a table that repeats a
name in another spelling, both spellings decode to the same fields and the encoder writes the first entry
(`duplicate_names_decode_alike_and_encode_to_the_first_entry`).

`tests/bodysetup_real_fuzz.rs` (gated on the install, added by the verify pass) repeats the three properties on
all 543 shipped payloads with their packages' own name tables: 552,712 strict prefixes (every offset of the
payloads up to 4 KiB, the first and last 96 offsets and 160 sampled ones of the larger) and 4,344 extensions are
all rejected, and of 152,040 single-bit, single-byte and 32-bit corruptions none panics and the 103,704 the
decoder accepts (mostly changed float data) re-encode to exactly the corrupted bytes.

### Importer output

`simple_collision` of a manifest entry:

| Field | Meaning |
|---|---|
| `use_simple_box_collision`, `use_simple_line_collision`, `use_simple_rigid_body_collision` | the stored value, else the native default `true` |
| `has_body_setup` | the mesh has an `RB_BodySetup` (`body_setup` of the entry is its path) |
| `shapes` | the `.collision.json` file relative to the manifest; present exactly when `has_body_setup` |
| `convex`, `boxes`, `spheres`, `sphyls` | shape counts of that file |

`<Name>.collision.json` (`format` `"asamu-simple-collision"`, `version` 1; compact JSON, one line):

| Field | Meaning |
|---|---|
| `mesh`, `body_setup` | object paths |
| `space` | the coordinate statement below, in words |
| `body_properties` | the body's scalar tags by name (object references as paths, `null` when unresolvable) |
| `convex[]` | `vertices` (`[x, y, z]`), `planes` (`[nx, ny, nz, d]`: the plane `n·p = d`, normal outward), `face_triangles` (vertex index triples), `edge_directions`, `face_normal_directions`, `bounds_min`, `bounds_max` |
| `boxes[]` | `transform` (4 rows `[x, y, z, w]`: X axis, Y axis, Z axis, origin), `size` (full lengths), `no_rb_collision`, `per_poly_shape` |
| `spheres[]` | `transform`, `radius`, the two flags |
| `sphyls[]` | `transform`, `radius`, `length` (cylinder part, along local Z), the two flags |

- **Space.** Every point and direction is in the mesh's local space, in **UE3 axes (X forward, Y right, Z up,
  left-handed) and Unreal units**, exactly as stored: not converted to glTF axes, not multiplied by `--scale`. It
  is the same local space as the mesh's vertices and its collision triangles, which the glTF files (and their
  `UCX_<Name>` node) hold as `(y, z, −x) · scale`; a consumer that maps the triangles back to UE3 axes gets them in
  the frame of the shapes. A component's scale and placement apply to the shapes as they do to the triangles
  (with one reservation: the native sphere, box and capsule checks take a single float where the convex check
  takes the scale vector, and how a non-uniform scale becomes that float was not read; the only mesh with such a
  shape is `EngineMeshes.Sphere`).
- Floats are written in their shortest exact decimal form and read back bit for bit; the importer re-parses each
  document before writing it and compares it with the decoded shapes. Non-finite values, a missing member (the
  struct default is not invented) and a body that fails the cross-checks fail the mesh's conversion.
- Not written: `PermutedVertexData` (derived from `vertices` by the rule above), `ElemBox.IsValid` (1 on every
  element), `PreCachedPhysScale` and the pre-cooked blobs (the physics middleware's data), and `NetIndex`.

Result (`asamu-import all --only meshes`, which passes `--collision`; CONFIRMED, measured 2026-10-10):

| | Before (manifest version 1) | After (version 2) |
|---|---:|---:|
| files | 1,765 | 2,081 |
| bytes | 66,113,039 | 70,525,421 |
| `.gltf` / `.bin` | 882 / 882 | 882 / 882, byte-identical to before |
| `.collision.json` | 0 | 316 (4,119,188 bytes: 2,439 convex elements, 1 sphere) |
| `manifest.json` | 1,382,263 bytes | 1,675,457 bytes |

A second run is skipped as up to date; a forced run reproduces the run manifest and all 2,081 files byte for
byte. A checker written separately in Python (local, not committed) compared all 316 documents with the generic
decoder's output as printed by `asamu-inspect --json props`: 401,366 numbers, 0 differences.

The runtime reads the record through `asamu_assets::manifest::MeshManifest::simple_collision_for_package`; a
version 1 manifest still loads and simply has no record, which means "convert again", not "no simple collision".

### Verify pass (2026-10-10) — CONFIRMED

A second reviewer re-derived this section by another route. Its scripts and dumps are local and not committed.

- **A third decoder.** A package reader and an `RB_BodySetup` decoder written separately in Python (its own
  header, table and chunk parsing, the reference LZO library, a two-pass tag reader) decoded all 543 exports with
  every byte consumed (3,750,487 bytes). A canonical text dump of every field of every export (raw name indices,
  float bit patterns, counts, a hash per pre-cooked blob: 34,225 lines, 798,337 tokens) is byte-identical to the
  same dump written from this crate's typed decoder.
- **The importer's output** against that decoder: all 882 manifest entries (the three switches, the body setup
  flag and path, the bounds) and all 316 shapes documents (2,439 convex elements and the sphere, 401,385 numbers
  compared as float bit patterns) agree, and the glTF vertex box mapped back by `(x, y, z) = (−gz, gx, gy)`
  equals the stored bounds box on 882 of 882 meshes.
- **Counts** re-measured from the packages: 1,512 exports, 882 paths, 0 differing copies; the switches stored
  `false` on 69 / 80 / 66 exports and `true` on none; 316 / 297 / 515 / 19 / 51 / 70 for the swept-trace table and
  65 / 520 for the line switch; 584 non-convex elements (1,364 planes, 125 over 1 uu, largest 160.88 uu).
- **The executable**: the three registrations and their offsets, the default words `1, 1, 1, 0` at 0x10172AE20,
  the branch of `UStaticMeshComponent::LineCheck`, the serializer's call chain (`UKMeshProps::Serialize`, then
  the array at +0x110) and the 0.5 of the box and capsule bounds were read again from the file. No package holds a
  class default object or a script class for `StaticMesh`, and no class derives from `RB_BodySetup`.
- **Upgrade and determinism**: `asamu-import all --only meshes` over a copy of a version 1 conversion re-runs the
  stage and leaves version 2; that output, a forced re-run, a fresh directory and the builder's run are the same
  2,081 files (70,525,421 bytes) by an independent digest of every file. The runtime's scene loader
  (`asamu-world`, `scene_real_data`) loads every mesh of all 12 maps from the version 2 manifest.

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
- **Manifest** (format version 2): per object path: package, export index, LOD files and per-section
  material/triangle counts, UV channels, `LightMapCoordinateIndex`, `LightMapResolution`, `BodySetup` path,
  collision triangle count, `simple_collision` (the switches and the shapes file, below), UE3 bounds, scale and a
  content hash (which covers the switches and the shapes too). A path cooked into several packages is written
  once; `also_in` lists identical copies, `differs_in` (with a `<Name>@<Package>` file) differing ones. A manifest
  of another version is not merged into: the stand-alone command asks for a fresh directory, and
  `asamu-import all` rebuilds the stage anyway (its stage fingerprint includes the importer build).
- **Simple collision:** `<Name>.collision.json` next to the mesh's files, for every mesh with a body setup; format
  under "Simple collision" below. Unlike the glTF files it is **not** converted to glTF axes and not scaled.
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

A full export (`--all-lods --collision`) was 902 glTF files plus 902 buffers and the manifest (1,805 files, 67 MB)
before the shapes files were added; the 316 `.collision.json` files come on top (measured on the LOD 0 export
that `asamu-import all` writes: 2,081 files, 70,525,421 bytes; see "Simple collision"). An
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
- `StaticMeshComponent` per-instance data (lightmaps, vertex color overrides): separate objects, not part of this
  payload. (`RB_BodySetup` is decoded: "Simple collision" above.)
- The native line and box checks against the simple shapes (`FKAggregateGeom::LineCheck` and the element checks
  it calls) were not read here; their addresses are listed under "Simple collision" for the runtime work.
- Materials are placeholders: binding textures and material parameters belongs to the material work.
- A Bevy loader for the manifest/glTF output.
