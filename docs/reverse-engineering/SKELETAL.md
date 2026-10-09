# Skeletal meshes and animations (UE3 v868) and the glTF importer

Evidence source: every `SkeletalMesh`, `AnimSet` and `AnimSequence` export of the 42 packages under
`~/Library/Application Support/Steam/steamapps/common/A Story About My Uncle/A Story About My Uncle.app/Contents/Resources/ASAMU/CookedMac`
(and `Maps/`) of the legitimately owned Mac install (Steam build 1822049), decoded read-only by our own code in
`crates/asamu-ue3/src/skeletal.rs` and `crates/asamu-ue3/src/anim.rs`. Field order and codec semantics were read
from the unstripped Mac executable's serializers and pose code (local Ghidra decompilation under the ignored
`research/decompiled/skeletal/`, never committed) and then proven on the data. This page holds structure, names,
counts and our own descriptions only: no payload bytes, no mesh or animation data and no decompiled code.

Builds on `OBJECT_FORMAT.md` (prelude, tagged properties), `MESHES.md` (bulk arrays, packed normals, half floats,
kDOP records, axis mapping) and the bulk-data record reader of `crates/asamu-ue3/src/bulkdata.rs`.

Reproduce:

```sh
cargo test --release -p asamu-ue3 --test skeletal_real_data --test anim_real_data -- --nocapture  # numbers on this page; asserts (T)
cargo test -p asamu-ue3 --test skeletal_synthetic --test anim_synthetic --test anim_hostile      # synthetic + hostile
cargo test --release -p asamu-import skeletal                                                     # exporter, incl. engine-vs-glTF check
cargo run --release -p asamu-import -- skeletal --check [--all-lods] [--resample]                 # convert + validate in memory, writes nothing
cargo run --release -p asamu-import -- --out <user-local dir> skeletal                            # writes <out>/skeletal/...
```

`(T)` marks a claim asserted by `crates/asamu-ue3/tests/skeletal_real_data.rs` or `anim_real_data.rs` against the
install.

## Result — CONFIRMED (T)

| | Count |
|---|---:|
| `SkeletalMesh` exports (class `Engine.SkeletalMesh`, CDO excluded) | **28** in 5 packages (27 distinct paths) |
| native tails consumed exactly to `SerialSize` | **28 / 28** (15,404,214 bytes) |
| re-encoded byte for byte from the decoded fields (`encode_skeletal_mesh_native`) | **28 / 28** |
| passing every structural cross-check (`validate_skeletal_mesh`) | **28 / 28** |
| `AnimSet` exports | **18** in 5 packages (17 distinct paths); all tagged properties only, native tail 0 bytes |
| `AnimSequence` exports | **395** (388 distinct paths) |
| native tails consumed exactly, byte streams fully parsed at their offsets | **395 / 395** (25,815,812 stream bytes) |
| byte streams re-encoded byte for byte from the decoded tracks / native tails re-encoded | **395 / 395** / **395 / 395** |
| track count equals the owning AnimSet's `TrackBoneNames` count; sequence listed in its set's `Sequences` | 395 / 395 each |
| tracks in a format the engine cannot decode; decoded keys non-finite or non-unit | 0; 0 |

| Package | Meshes | LODs | Bones | Vertices | Triangles | AnimSets | Sequences |
|---|---:|---:|---:|---:|---:|---:|---:|
| `Startup.upk` | 18 | 22 | 676 | 81,225 | 103,356 | 9 | 358 |
| `AG-BeautifulCity` | 2 | 2 | 312 | 12,675 | 19,185 | 2 | 9 |
| `AG-Darkcave` | 1 | 1 | 5 | 354 | 360 | 1 | 3 |
| `AG-StarHaven` | 6 | 6 | 785 | 26,749 | 42,448 | 5 | 20 |
| `TheCore` | 1 | 1 | 95 | 12,338 | 17,686 | 1 | 5 |
| **Total** | **28** | **32** | **1,873** | **133,341** | **183,035** | **18** | **395** |

LOD models per mesh: 1 on 26 meshes, 2 on one, 4 on one. The meshes are the player hands, Maddie (+ book, arm
test), the worm, Uncle Fred, the villagers (+ accessories), the StarHaven characters and Samuel, plus stock UDK
assets that ship in `Startup.upk` (gibs, Iron Guard, LIAM, link gun).

## SkeletalMesh layout — CONFIRMED (T, exact consumption and byte-exact round trip)

All little-endian; "obj" = `i32` package index; a **bulk array** is `i32 ElementSize, i32 Count, Count × ElementSize`
bytes (the decoder requires the listed element size). Type and member names are the executable's symbol names
(CONFIRMED) or UE3 conventions (TENTATIVE where marked).

```text
USkeletalMesh (after the tagged properties)
  FBoxSphereBounds Bounds            FVector Origin, FVector BoxExtent, f32 SphereRadius
  TArray<obj> Materials
  FVector Origin                     mesh-to-component offset (see "Origin and RotOrigin")
  FRotator RotOrigin                 i32 Pitch, Yaw, Roll
  TArray<FMeshBone> RefSkeleton      52 bytes each (below)
  i32 SkeletalDepth
  TIndirectArray<FStaticLODModel> LODModels
  TMap<FName,i32> NameIndexMap       i32 count, then (FName, i32) pairs
  TArray<FPerPolyBoneCollisionData> PerPolyBoneKDOPs
                                     TkDOP bounds (24 bytes), bulk compact nodes (6), bulk triangles (8),
                                     TArray<FVector> collision vertices
  TArray<FString> BoneBreakNames
  TArray<u8> BoneBreakOptions
  TArray<obj> ClothingAssets         native copy (the tagged property of the same name also exists)
  TArray<f32> CachedStreamingTextureFactors
  u32 bHaveSourceData                FSkeletalMeshSourceData; one more FStaticLODModel when 1

FMeshBone: FName Name, u32 Flags, FQuat Orientation (x, y, z, w), FVector Position,
           i32 NumChildren, i32 ParentIndex, FColor BoneColor

FStaticLODModel (one LOD)
  TArray<FSkelMeshSection>           u16 MaterialIndex, u16 ChunkIndex, u32 BaseIndex, u32 NumTriangles,
                                     u8 TriangleSorting (13 bytes)
  FMultiSizeIndexContainer           u32 bNeedsCPUAccess, u8 DataTypeSize (2|4), bulk index array of that size
  TArray<u16> ActiveBoneIndices
  TArray<FSkelMeshChunk> Chunks      u32 BaseVertexIndex, TArray<FRigidSkinVertex>, TArray<FSoftSkinVertex>,
                                     TArray<u16> BoneMap, i32 NumRigidVertices, i32 NumSoftVertices,
                                     i32 MaxBoneInfluences
  u32 Size                           (meaning below)
  u32 NumVertices
  TArray<u8> RequiredBones
  bulk-data record RawPointIndices   i32 elements, inline
  u32 NumTexCoords
  FSkeletalMeshVertexBuffer          u32 NumTexCoords, u32 bUseFullPrecisionUVs, u32 bUsePackedPosition,
                                     FVector MeshExtension, FVector MeshOrigin (names TENTATIVE),
                                     bulk GPU skin vertices (below)
  [only when the tagged bool bHasVertexColors is set] bulk FColor[] (element size 4)
  TArray<FSkeletalMeshVertexInfluences>  TArray<FVertexInfluence> (u8 Weights[4], u8 Bones[4]),
                                     TMap<(i32,i32), TArray<u32>>, TArray<FSkelMeshSection>,
                                     TArray<FSkelMeshChunk>, TArray<u8> RequiredBones, u8 Usage
  FMultiSizeIndexContainer           adjacency indices (12 per triangle when present)

FRigidSkinVertex: FVector Position, FPackedNormal TangentX/Y/Z, FVector2D UVs[4], FColor Color, u8 Bone  (61 bytes)
FSoftSkinVertex:  as rigid, but u8 InfluenceBones[4], u8 InfluenceWeights[4] instead of Bone             (68 bytes)
GPU skin vertex:  FPackedNormal TangentX, FPackedNormal TangentZ, u8 InfluenceBones[4],
                  u8 InfluenceWeights[4], FVector Position, NumTexCoords × (half2 | float2)
                  = 28 + 4·N bytes (half UVs) or 28 + 8·N (full precision)
```

Notes on the layout:

- **Where it comes from.** `USkeletalMesh::Serialize`, `FStaticLODModel::Serialize`,
  `FSkeletalMeshSourceData::Serialize`, `operator<<` for `FMeshBone`, `FSkelMeshSection`, `FSkelMeshChunk`,
  `FRigidSkinVertex`, `FSoftSkinVertex`, `FMultiSizeIndexContainer`, `FSkeletalMeshVertexBuffer`,
  `FSkeletalMeshVertexColorBuffer`, `FSkeletalMeshVertexInfluences`, `FVertexInfluence`,
  `FPerPolyBoneCollisionData`, `FGPUSkinVertexBase::Serialize`, the `TSkeletalMeshVertexData<...>::Serialize`
  instantiations (element sizes) and `FRawStaticIndexBuffer16or32<...>::Serialize`, all present by name in the
  unstripped executable (decompiled locally by symbol address with a small headless Ghidra script; the overloaded
  `operator<<` names are ambiguous for `tools/ghidra-scripts/DecompileToLocal.java`, so take addresses from `nm`).
- **Version gates** all satisfied by v868: section `NumTriangles` as u32 (≥ 806, older u16), `TriangleSorting`
  (≥ 599), raw point indices as a bulk record (> 805), `NumTexCoords` (≥ 709), vertex colors (≥ 710, and only with
  `bHasVertexColors`), vertex influences (≥ 534), adjacency buffer (> 840), source data (> 833), streaming factors
  (> 796), clothing array (≥ 680), bone-break names/options (≥ 609 / ≥ 694), the bone color (> 514), the vertex
  buffer's packed-position fields (≥ 592), the 4-UV rigid/soft vertices with color (≥ 709 / ≥ 710). Older-only
  branches (discarded arrays at < 686, the legacy `TArray<FSoftSkinVertex>` vertex buffer at < 493) are never
  taken; a further block (inverse reference matrices and caches) is serialized only by archives that neither load
  nor save.
- **`bHasVertexColors` gate.** The loader tests a bit of the owning mesh's bool bitfield; by property order that bit
  is the tagged `bHasVertexColors` (STRONG). No shipped mesh sets it (T); decoding the colorless meshes with the gate
  forced on fails, as expected.
- **`bUsePackedPosition`** is stored (set on all 32 LODs, T) but `FSkeletalMeshVertexBuffer::AllocateData` in this
  build chooses the vertex type from `bUseFullPrecisionUVs` and `NumTexCoords` only, so positions are always full
  `FVector`s (CONFIRMED: decompiled `AllocateData`, and the element sizes of all 32 LODs).
- **Not exercised by the data** (decoded, round-tripped by synthetic tests, layout CONFIRMED from the serializers
  but values unverified): vertex colors, non-empty vertex influences (the mapping key is read as two `i32`,
  TENTATIVE), per-poly collision, source data, full-precision UVs, 32-bit indices, bone-break data.

## Field values and meaning — CONFIRMED (T) unless marked

| Field | Observed |
|---|---|
| index buffers | all 16-bit; sections cover each index buffer; each section's indices lie inside its chunk |
| sections vs chunks | one chunk per section on all 32 LODs (74 each); `TriangleSorting` 0 everywhere |
| chunks | contiguous vertex ranges covering the vertex buffer; `NumRigidVertices + NumSoftVertices` per chunk |
| source vertices | every chunk keeps its rigid and soft vertices (cooker did not strip them) |
| GPU vertex order | per chunk: the rigid vertices, then the soft vertices — positions, influences and normal directions (`TangentZ` x, y, z bytes) identical on all 133,341 vertices; the `TangentZ.W` byte is not (source 0 or 128, GPU 0 or 255, differing on 109,046 vertices: the GPU sign is computed separately) |
| GPU UVs | the source float UV converted to half by **truncating** the mantissa (133,341 / 133,341; round-to-nearest matches only 34,762) |
| influences | `InfluenceBones[4]` then `InfluenceWeights[4]`; weights sum to 255 and are sorted descending on every vertex; bones index the chunk's `BoneMap`; 1/2/3/4 influences on 75,449 / 29,146 / 25,682 / 3,064 vertices; never more than the chunk's `MaxBoneInfluences` |
| rigid vertices | exactly the 75,449 single-influence vertices |
| packed normals | as static meshes (`MESHES.md`): `TangentZ.W` byte 0 or 255 (bitangent sign); `TangentX.W` 0 or 128 |
| winding | 182,018 triangles wind against the stored normals, 71 along (thresholds as in `MESHES.md`): clockwise fronts, as static meshes |
| `RawPointIndices` | inline, one per vertex on 31 LODs; empty on one stock UDK LOD |
| `Size` | 0 on 31 LODs; on one stock UDK LOD `NumVertices × 40`, which is not that LOD's 32-byte stride (meaning UNKNOWN) |
| `RequiredBones` / `ActiveBoneIndices` | sorted bone indices in range; equal to each other on 7 LODs |
| `NameIndexMap` | full (one entry per bone, mapping each bone name to its index) on all 28 meshes |
| `SkeletalDepth` | the number of bones on the longest root-to-leaf chain (validated) |
| bone `Flags` | 0 on all 1,873 bones |
| `Origin` / `RotOrigin` | zero origin on 23 meshes; `RotOrigin` `(0, -16384, 0)` (yaw −90°) on 21, zero on 7 |
| `Bounds` | contain LOD 0's reference-pose vertices (validated); tight to the vertex box on only 3 meshes |
| `ClothingAssets` (native) | one null entry on every mesh; `CachedStreamingTextureFactors` 4 entries |
| sockets | 74 `SkeletalMeshSocket` objects (tagged `Sockets` array); every `BoneName` exists in the skeleton |

### Reference skeleton convention — CONFIRMED (T)

`ParentIndex` of the root (bone 0) is 0, every other bone's parent precedes it, `NumChildren` matches the links.
Bone transforms compose in the usual way: a bone's mesh-space transform is its parent's transform followed by its own
`(Orientation, Position)`, with the quaternion rotating as `q·v·q*`. The engine's own reference-pose matrices are
built this way (`USkeletalMesh::GetRefPoseMatrix`, `CalculateInvRefMatrices`, standard quaternion-to-matrix and
child·parent row-vector products). On the data, the vertices lie next to the bone that dominates them: the median
over meshes of each mesh's median vertex-to-dominant-joint distance is 3.10 uu with the stored quaternions against
15.29 uu with their conjugates; stored is closer on 20 meshes and equal on the other 8 (T). (Over all 133,341
vertices of all LODs pooled, an independent re-computation gives medians of 6.14 against 26.67 uu.)

### Origin and RotOrigin — STRONG

The component's local-to-world transform is prefixed with the mesh offset (`USkeletalMeshComponent::
CalcCurrentLocalToWorld`): a mesh-space point `v` becomes `R·(v + Origin)` in component space, where `R` is the
engine rotation matrix of `RotOrigin` (pitch, yaw, roll from the sine table). With the common yaw of −90° the meshes
are authored facing +Y and turned to face +X in game. Read from the decompilation; not yet checked against a
rendering of the original.

## AnimSet — CONFIRMED (T)

Tagged properties only (no native tail on any of the 18). Used: `TrackBoneNames` (track → bone name),
`Sequences`, `bAnimRotationOnly` (class default **true**: `Engine.Default__AnimSet` tags it, T),
`UseTranslationBoneNames`, `ForceMeshTranslationBoneNames`, `PreviewSkelMeshName`, `BestRatioSkelMeshName`.

How the engine applies a set to a mesh (STRONG, from `FAnimSetMeshLinkup::BuildLinkup` and
`UAnimNodeSequence::GetAnimationPose`):

- each mesh bone takes the track whose name equals its own (`BoneToTrackTable`); a bone without a track keeps its
  reference pose;
- the root bone (mesh bone 0) takes rotation and translation from its track;
- every other tracked bone takes its rotation from the track; its translation comes from the track unless
  (`bAnimRotationOnly` and the track is not in `UseTranslationBoneNames`) or the track is in
  `ForceMeshTranslationBoneNames`, in which case the reference-pose translation is used (a component may override
  `bAnimRotationOnly`);
- additive sequences use identity / zero instead of the reference pose (no shipped sequence is additive, T).

## AnimSequence

### Tagged properties — CONFIRMED (T)

`SequenceName`, `NumFrames`, `SequenceLength`, `RateScale` (class default 1.0, the only tagged default of
`Engine.Default__AnimSequence`), `bNoLoopingInterpolation`, `bIsAdditive`, `TranslationCompressionFormat`,
`RotationCompressionFormat`, `KeyEncodingFormat` (enum bytes; untagged = `ACF_None` / `AKF_ConstantKeyLerp`),
`CompressedTrackOffsets`, `Notifies` (struct `Time, Notify, Comment, Duration`), `CompressionScheme`,
`AdditiveRefName`, `EncodingPkgVersion`. Frame rates `(NumFrames − 1) / SequenceLength`: 15 fps on 159 sequences,
30 on 96, 29 on 53, 24 on 31, 28 on 23, 27 on 20, 2 on 1. Notifies: 74 on 29 sequences (`AnimNotify_Kismet` 28,
`AnimNotify_Footstep` 24, `AnimNotify_Sound` 22).

### Native tail — CONFIRMED (T)

```text
TArray<FRawAnimSequenceTrack> RawAnimationData   per track: bulk FVector[] PosKeys (12), bulk FQuat[] RotKeys (16)
i32 NumBytes, u8[NumBytes] CompressedByteStream
```

The cooker **kept** the raw (editor) keys on all 395 sequences: every raw track holds 1 or `NumFrames` keys (T).
They are not used at run time but give an independent reference for the decompressed keys (below).

### Compressed byte stream — CONFIRMED (T, exact consumption at the stored offsets + byte-exact re-encoding)

Tracks are laid out in track order, translation before rotation, each starting where the previous one ended (the
engine's loader reads them sequentially and stores them at the offsets; on the data the offsets are exactly the
sequential positions). Alignment padding fills with byte `0x55` (all 37,150 padding bytes, T).

- **ConstantKeyLerp / VariableKeyLerp** (`AnimationEncodingFormatLegacyBase::ByteSwapIn`,
  `AEFConstantKeyLerpShared` / `AEFVariableKeyLerpShared::ByteSwap{Translation,Rotation}In`): four offsets per track
  (translation offset, translation key count, rotation offset, rotation key count). A track is an optional
  interval header (`Mins[3]`, `Ranges[3]` floats, only for the interval format with more than one key), the keys,
  then for VariableKeyLerp with more than one key: padding to 4 and a key → frame table (`u8`, or `u16` when
  `NumFrames > 255`). Each track ends padded to 4. A one-key rotation is always stored as `Float96NoW` and a one-key
  translation as three floats, whatever the sequence's format.
- **PerTrackCompression** (`AEFPerTrackCompressionCodec::ByteSwapIn` / `ByteSwapOneTrack`): two offsets per track
  (translation, rotation), −1 = no data (identity rotation / zero translation). A track is a `u32` header — key count
  in bits 0–23, component mask (x, y, z) in bits 24–26, "time markers" in bit 27, format in bits 28–31 — then for
  the interval format one `(min, range)` float pair per present component, the keys, then with time markers
  padding to 4 and a key → frame table as above, then padding to 4.

Key sizes come from four engine tables and `PerTrackNumComponentTable`, read from the executable's constant data
(CONFIRMED; `anim.rs` constants):

| format | translation bytes/key (legacy) | rotation bytes/key | per-track components (mask 0 / per set bit) |
|---|---|---|---|
| `ACF_None` | 12 | 16 (x, y, z, w) | 4 / 4 |
| `ACF_Float96NoW` | 12 | 12 | 3 / one float per set bit |
| `ACF_Fixed48NoW` | 12 | 6 | 3 / one `u16` per set bit |
| `ACF_IntervalFixed32NoW` | 4 (+24-byte header) | 4 (+24-byte header) | header floats 6 / 2 per set bit; one `u32` per key |
| `ACF_Fixed32NoW` | 12 | 4 | 1 `u32` |
| `ACF_Float32NoW` | 12 | 4 | 1 `u32` |
| `ACF_Identity` | 0 | 0 | 0 |

### Formats present — CONFIRMED (T)

| Key encoding | Sequences (exports / distinct) | Tracks in the stream |
|---|---:|---|
| `AKF_PerTrackCompression` | 312 / 312 | rotation `Fixed48NoW` 19,504, `Float96NoW` 636; translation `Float96NoW` 18,146, `IntervalFixed32NoW` 475; 4,143 identity (no data) |
| `AKF_VariableKeyLerp` | 76 / 69 | rotation `Float96NoW` 8,214; translation `ACF_None` 8,214 (export counts) |
| `AKF_ConstantKeyLerp` | 7 / 7 | rotation `Fixed48NoW` 62, `Float96NoW` 517 (mostly forced one-key); translation `ACF_None` 579 |

Per-track component masks used (track counts): 7: 17,977, 1: 10,798, 4: 7,491, 3: 1,418, 0: 535, 5: 278, 6: 218,
2: 46. Absent from the shipped data: `IntervalFixed32NoW`, `Fixed32NoW`, `Float32NoW` and `None` rotations,
per-track `Fixed48NoW` translations, explicit `Identity` tracks. The tagged
`RotationCompressionFormat` of a per-track sequence does not describe its tracks (e.g. `ACF_Identity` on sequences
whose tracks are `Fixed48NoW`); the per-track headers do.

### Key decoding — CONFIRMED for the present formats, TENTATIVE for the absent ones

From the codecs' `GetBoneAtomRotation` / `GetBoneAtomTranslation` (constants read from the executable: 511, 1023,
−32767, 1/32767), then checked against the raw keys:

- **NoW rotations** store x, y, z; `w = sqrt(1 − x² − y² − z²)` (0 when negative), so decoded keys have `w ≥ 0`.
- `Float96NoW`: three floats (per-track: always three for rotations). `ACF_None` rotation: four floats.
- `Fixed48NoW` rotation: legacy `(v − 32767) / 32767`; per-track `(v − 32767) · (1/32767)` for each component whose
  mask bit is set, 0 otherwise.
- `IntervalFixed32NoW` rotation: one word, X in bits 21–31, Y 10–20 (both `(q − 1023) / 1023`), Z 0–9
  (`(q − 511) / 511`), each scaled by the range and offset by the minimum. Translations use the mirrored word:
  X in bits 0–9, Y 10–20, Z 21–31.
- `Fixed32NoW` rotation: the same 11/11/10 word without range. `Float32NoW`: three small floats (3-bit exponent
  biased to 2^−4, 7/7/6-bit mantissas, a sign bit per component; all-zero bits decode to 0) — TENTATIVE (absent).
- Translations: `ACF_None`/`Float96NoW` three floats (per-track: one float per set mask bit, or three for mask 0);
  per-track `Fixed48NoW` translation decodes as `v − 255` per set component (TENTATIVE, absent); the legacy
  codecs cannot decode `Fixed48`/`Fixed32`/`Float32` translations (stored as three floats, read as packed) and the
  decoder refuses them; the per-track codec returns zero for unsupported translation formats and identity for
  `ACF_None` rotations, as the engine does after logging an error.

**Key times** (the engine's non-looping mapping): evenly spaced keys span the whole sequence, key `k` of `n` at
`k / (n − 1) · SequenceLength`; with a frame table, key `k` sits at `frame[k] / (NumFrames − 1)`. Interpolation is
linear for translations and a normalised lerp along the shorter arc for rotations. Looping playback gives the last
key a full interval back to key 0 (implemented in `even_key_position`, not used by the exporter). Frame tables
increase strictly (4,287) or repeat the last frame `NumFrames − 1` for their final two keys (515), T; the two keys
of such a pair differ on 399 of the 515 tracks (T).

**Frame-table search** (CONFIRMED, decompiled `AEFVariableKeyLerp<N>::GetBoneAtomRotation`; the per-track codec
shares the rule): for a position strictly inside the sequence, the engine takes the last key whose frame is at or
below the integer frame position (clamped to `NumFrames − 2` when not looping) and interpolates towards the next
key; at or past the end it returns the last key. With a repeated final frame it therefore interpolates *towards the
first key of the pair* during the final interval and shows the second key only at the end. The importer keeps both
(below); `anim_hostile.rs` pins the rule.

**Against the raw keys** (every frame of every track, non-looping evaluation, T): mean rotation error ≤ 0.21° and
mean translation error ≤ 0.043 uu in every (codec, format, key layout) class; maxima ≤ 5.4° and ≤ 1.15 uu except
where the compressor thinned keys to fewer evenly spaced ones (`Fixed48NoW`, 82,741 + 6,705 samples): there the
even mapping shifts keys in time and single frames differ by up to 34°/42°. Translation tracks without data
(per-track −1) match zero raw translations exactly. This is the evidence that the decoders and the time mapping are
right; the exporter reproduces the engine's behaviour, including the thinning shift.

### Pose rule: W negation — CONFIRMED

The engine's pose code multiplies every decompressed rotation of a **non-root** bone by `(1, 1, 1, −1)`
(`GlobalVectorConstants::Float111_Minus1` in `AEFPerTrackCompressionCodec::GetPoseRotations` and in the raw-data
path of `UAnimNodeSequence::GetAnimationPose`); mesh bone 0 is fetched separately without it. Negating W conjugates
the rotation, which puts animation keys into the reference skeleton's convention. Two independent views of the data
agree with the code (both T):

- **Joint spread.** Skinning LOD 0 with the frame-0 pose of each of the 388 sequences paired with a mesh, the
  positions a blended vertex gets from its different bones lie closer together with the rule (median spread
  1.83 uu against 2.86 uu without; a correct bent pose is not zero). The rule is better on 385 sequences, ties on 1
  and is worse on 2, both on the five-bone `Maddie_book` prop (one by a negligible margin).
- **Distance from the reference pose.** The frame-0 local rotation of a tracked non-root bone lies a median 19.7°
  from its reference rotation with the rule and 49.6° without (28,756 bones); for the root bone it is the other way
  round, 4.9° without against 177.8° with (94 tracked roots), which confirms the root exception.

## Importer: `asamu-import skeletal` (glTF 2.0)

`tools/asamu-import/src/skeletal.rs` reads every package (meshes, sets, sequences and the `SkeletalMeshComponent`
pairings), then writes one `.gltf` + `.bin` per mesh under `<out>/skeletal/<Package>/<Path...>/<Name>` (coarser
LODs with `--all-lods`, skin only) and `manifest.json`. Output goes only to the user-local `--out` directory through
the shared safety checks (no repository paths except ignored `research/`, no `.app`/`steamapps`, no links followed,
existing files kept unless `--force`).

- **Axes** as the static mesh exporter: UE3 `(x, y, z)` → glTF `(y, z, −x)` (determinant −1), times `--scale`; a
  rotation `(x, y, z, w)` → `(−y, −z, x, w)`. Winding kept, normals/tangents as in `MESHES.md`.
- **Nodes:** a root node holding the mesh-to-component transform (`R·(v + Origin)`), the bones under it (reference
  pose as local TRS), sockets as `SOCKET_<Name>` children of their bones (location, rotator, scale), and the skinned
  mesh node as a second scene root.
- **Skin:** one joint per bone in skeleton order; inverse bind matrices of the composed reference pose; `JOINTS_0`
  through the chunk bone maps; `WEIGHTS_0` the stored bytes as normalised `UNSIGNED_BYTE` (every vertex sums to 255;
  floats would be written otherwise).
- **Pairing:** an AnimSet goes with the meshes a `SkeletalMeshComponent` uses it with (≥ 50 % of its track names on
  the mesh), else its `PreviewSkelMeshName`, else the mesh matching ≥ 90 % of its track names. All 17 distinct sets
  pair: 8 through components (the two villager sets with two meshes each), 8 through their preview name, 1 by bone
  names (`PlayerHand.Root`, whose preview mesh does not ship).
- **Animations:** one per sequence, `<AnimSet>/<SequenceName>`, LINEAR samplers at the stored key times or, with
  `--resample`, at every frame evaluated with the engine's interpolation. The pose rules above are applied (W
  negation on non-root bones; translation channels only where the engine uses the animation's translation).
  `RateScale`, `bNoLoopingInterpolation`, notifies and the key encoding go to the manifest.
- **Keys sharing a time** (the repeated final frames): glTF needs strictly increasing times, so the first key of a
  run is written one `f32` step before the shared time and the last key at it, which reproduces the engine's search
  (interpolate towards the first, show the last from that time on; at time 0 the order is mirrored). An earlier
  version kept only the last key, which moved vertices by up to 0.41 uu (at the sampled frames) during the final
  interval of sequences with such tracks (found by the independent check below). Non-finite or negative times (malformed tags only) become 0;
  `--resample` evaluates at most 65,536 frames per sequence (the longest shipped one has 3,091).
- **Rotation blending — known difference.** glTF defines LINEAR rotation sampling as spherical interpolation
  (slerp); the engine uses a normalised lerp. The two agree on every key and differ between keys: over all 416
  exported animations the largest difference seen is 0.134 uu on a vertex (stock `Dodge_Idle_Rif_LU`, whose keys are
  far apart); `--resample` shrinks the gaps to one frame but cannot remove the difference.
- **Validation** (`validate_gltf`, run on every document before it is written; a document that fails is not
  written and the run reports it): buffer/view/accessor ranges and alignment, POSITION and sampler-input min/max
  equal to the data, attribute counts, unit normals/tangents/rotations, joint indices inside the skin, weights
  summing to 1, a single-parent acyclic node tree, node mesh/skin references, a skin's `skeleton` above its joints,
  at least one primitive per mesh, inverse bind matrices that invert the joints' bind transforms, finite and
  strictly increasing key times and one channel per target.
- **File names and the manifest**: every output file is claimed case-insensitively by its mesh (case-insensitive
  file systems; sanitising can merge names; a mesh can be named like another mesh's `_LOD<n>` file); a colliding
  mesh gets a hash of its object path appended. Claims and entries of an existing `manifest.json` (same version)
  are kept, so a filtered run (`--package`/`--name`/`--limit`) updates its own meshes without dropping the others
  and every mesh keeps the file names it was first given; a manifest that does not parse is refused rather than
  overwritten.

Result over the whole install (`--check`, about 2.5 s in release): **27 meshes (27 LODs; 31 with `--all-lods`),
115,330 vertices, 164,019 triangles, 416 animations, 47,842 channels, 0 failures, 0 validation issues**; 18 meshes
carry animations, 9 have none (gibs, villager accessories, link gun 3P, Corrupt arms). A full write is about 82 MB.

**Behavioural check** (`real_data_skinning_matches_the_engine_rules`, `synthetic_skinning_matches_the_engine_rules`,
`duplicated_final_frame_follows_the_engine`): a separate evaluator applies glTF semantics to the written document
(animation sampling with slerp, node hierarchy, `joint global × inverse bind` skinning, the skinned node's own
transform ignored) and is compared with an evaluation of the decoded UE3 data under the engine's rules (reference
pose, `BoneToTrackTable`, W negation, translation rule, `Origin`/`RotOrigin`) followed by the axis map. On the real
data (18 meshes with animations, the first sequence of each paired set, up to 8 frames plus the last two, and four
times between frames for the keyed export): resampled export within 0.05 uu at frames; keyed export within
0.00023 uu when its rotations are blended like the engine (nlerp) and within 0.029 uu with glTF's slerp.

**Independent verification** (a throwaway re-implementation written from this page, run locally and not kept):
it decodes every mesh and sequence on its own and agrees with `skeletal.rs`/`anim.rs` on all 28 meshes (skeleton,
indices, vertices, chunks) and all 395 sequences (every key bit for bit); it reproduces the package table, the
influence, weight, vertex-order and UV-truncation statistics, `Origin`/`RotOrigin`, the pairings, the format, mask,
padding, identity-track and stream-byte counts, the raw-key error table and both W-negation views; and it re-validates the 27 written documents (31 with
`--all-lods`) with its own glTF checks (0 issues) and evaluates all 416 animations against its own engine-rule
evaluation: reference pose within 0.00008 uu, and, with the blend matched (slerp on both sides), within 0.0007 uu
at frames and 0.0005 uu between frames.

## Hostile-input discipline

Counts are checked against the remaining bytes before allocating, bulk arrays insist on their element size,
booleans must be 0 or 1, index sizes 2 or 4, UV channels 1–4, LOD count ≤ 64; unknown or compressed inline
raw-point payloads are refused; the mesh decoder and the sequence tail must end exactly at `SerialSize`; stream
tracks must sit at the sequential offsets and consume the whole stream; unknown per-track format nibbles,
zero-key legacy tracks and undecodable legacy translation formats are refused. On the engine-semantics side,
sampling and key lookup take any tag values (NaN or infinite lengths and times, `NumFrames` from `i32::MIN` to
`i32::MAX`, out-of-range key indices) without overflow and return finite values for finite keys (before the
verification pass, `NumFrames = i32::MIN` overflowed the frame arithmetic in debug builds and a NaN position gave NaN
translations). `tests/skeletal_synthetic.rs` checks a hand-written payload field by field (and that the encoder reproduces it), round-trips every optional part
(colors, vertex influences, per-poly collision, source data, full-precision UVs, 32-bit indices, Unicode bone-break
names), checks that validation reports 24 kinds of inconsistency, rejects every truncation and an appended byte,
and runs 6,000 deterministic mutations (bit flips, random bytes, extreme `i32`s) plus noise: nothing panics and every
accepted mutation re-encodes to the same bytes. `tests/anim_synthetic.rs` builds legacy and per-track streams by hand
(interval headers, u8 and u16 frame tables, masks, identity tracks, padding), checks the decoded values of every
format including the absent ones, the time mapping and the pose helpers, refuses malformed streams and runs 5,000
stream mutations. `tests/anim_hostile.rs` adds the extreme tag values above, extreme per-track and legacy counts and
offsets, the frame-table search rule, and 8,000 mutations of legacy streams (u16 frame tables, interval headers,
offsets, `NumFrames`). The importer's tests add hostile tags (valid documents or clean errors), 1,500 rounds of
mutated mesh payloads and streams through the whole exporter (no panics; what it builds validates), and the
validator's own failure cases.

## UNKNOWN / not done

- Meaning of the LOD `Size` field (one stock LOD only); names of the vertex buffer's extension/origin vectors.
- Values of the absent formats (`Float32NoW`, `Fixed32NoW`, interval rotations, per-track `Fixed48` translations) —
  decoded per the decompilation, never seen in data.
- APEX clothing assets, morph targets, physics assets (`PhysicsAsset` exports exist), AnimTrees and blending, root
  motion options, looping playback timing, additive animations (none ship).
- Materials are placeholders (named after the UE3 material paths); binding belongs to the material work.
- A Bevy loader for the manifest/glTF output; checking `Origin`/`RotOrigin` against a rendering of the original.
