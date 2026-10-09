# Level format: `ULevel`, BSP (`UModel`), brushes, volumes and scene extraction

Evidence source: the 12 map packages (`*.asamu`) under
`~/Library/Application Support/Steam/steamapps/common/A Story About My Uncle/A Story About My Uncle.app/Contents/Resources/ASAMU/CookedMac/Maps`
of the legitimately owned Mac install (Steam build 1822049), decoded read-only by our own code in `crates/asamu-ue3`
(`level.rs`, `bsp.rs`). The native serializers (`ULevel::Serialize`, `UModel::Serialize`, `UPolys::Serialize`,
`UBrushComponent::Serialize` and the element `operator<<`s they call, plus `AActor::LocalToWorld` and
`UPrimitiveComponent::SetTransformedToWorld`) were read locally in the unstripped Mac executable with Ghidra
(output in ignored `research/decompiled/`, never committed); every layout below was then proven on the data by
exact consumption and value checks. This page gives structure, counts and our own descriptions only: no payload
bytes, no scene dumps, no decompiled code, no script source.

Builds on `OBJECT_FORMAT.md` (object prelude, tagged properties), `KISMET.md` and `LEVELS.md`.

Reproduce:

```sh
cargo test -p asamu-ue3 --release --test level_real_data -- --nocapture   # every (T) claim; skips without data
cargo test -p asamu-ue3 --test level_synthetic --test level_scene_synthetic   # synthetic + hostile-input tests
cargo run --release -p asamu-import -- --out <local dir> levels [--map AG-IceCave] [--gltf]
```

`(T)` marks a claim asserted by `crates/asamu-ue3/tests/level_real_data.rs` against the install.

## Result — CONFIRMED (T)

| Export class | Count | Result |
|---|---:|---|
| `Level` (one `TheWorld.PersistentLevel` per map) | 12 | native tail consumed **exactly** |
| `Model` (level BSP + one per brush/volume) | 372 | native tail consumed exactly; 0 of 41,108 index references out of range; 0 non-unit plane normals |
| `Polys` | 372 | native tail consumed exactly |
| `BrushComponent` | 1,204 | native tail consumed exactly |

- `ULevel::Actors` lists **30,284 actors** in the 12 maps (plus 319 null slots for deleted actors). Exactly the
  exports with a state frame (`RF_HasStack`, see OBJECT_FORMAT.md) are listed: per map the counts are equal and
  every listed actor carries the flag (T). No actor-class export inside a level is missing from the list, no
  listed actor lives outside its level and none is listed twice (T).
- `Actors[0]` is the `WorldInfo` and `Actors[1]` the builder `Brush` in every map (T), the usual UE3 arrangement.
- All gameplay actor counts recorded earlier from export census and class defaults are reproduced from the actor
  lists (T): 10 player starts, 109 `ASAMUCheckpoint` (ParadiseCave 24, StarHaven 25, IceCave 28, Darkcave 17,
  BeautifulCity 12, Workshop 1, Epilogue 1, FrontEnd 1), 91 `ASAMUKillZone` + 3 `ASAMUDynamicKillZone`, 134
  `ASAMUFallingRock` + 32 `ASAMUFallingWhenGrappledRock` (IceCave), 105 `ASAMURechargeCrystal` (IceCave 98,
  StarHaven 7), 3 `ASAMUTelePad_Attractor` (Workshop, FrontEnd, TheCore), 25 `ASAMUCollectible` (5 in each middle
  map). Instance deltas agree with DEFAULTS.md section 7: `checkpointIndex` stored on 101 checkpoints,
  `spawnPointOffset` on 97, `bTriggeredFromKismet` true on 8, `fallDistance` on all 134 falling rocks (T).
- `WorldInfo.KillZ` per map equals DEFAULTS.md (−1e10 BeautifulCity/Darkcave, −1e9 ParadiseCave, −1e7
  IceCave/StarHaven, 1.0 Workshop/Epilogue/FrontEnd, −262,143 elsewhere); `DefaultGravityZ` −520 (class default)
  and `GlobalGravityZ` 0 in every map: no map overrides gravity (T).
- Streaming (T): AG-BeautifulCity's `WorldInfo.StreamingLevels` holds one `LevelStreamingAlwaysLoaded`
  (`freds_place`), AG-IceCave's one `LevelStreamingKismet` (`thecore`); both with `Offset` (0, 0, 0). No other map
  streams.

## `ULevel` native data — CONFIRMED (T, exact consumption of all 12)

After the prelude and tagged properties (`ShadowmapTotalSize`, `LightmapTotalSize`, ...). "obj" = `i32` package
index; "bulk" = `i32 ElementSize, i32 Count, Count×ElementSize raw bytes` (the engine's `BulkSerialize`).

| # | Field (UE3 name) | Layout | Shipped data |
|---|---|---|---|
| 1 | `Actors` | obj owner (the level) + `array<obj>` | 4 – 7,552 slots per map; null = deleted actor |
| 2 | `URL` | 4 × FString (protocol, host, map, portal), `array<FString>` options, `i32` port, `i32` valid | stale editor values (`GearStart`, `UDKFrontEndMap`, `Index.ut3`, `ASAMUFrontEndMap`) — not a gameplay fact |
| 3 | `Model` | obj | the level BSP (`Model` export) |
| 4 | `ModelComponents` | `array<obj>` | 0 – 52 `ModelComponent`s |
| 5 | `GameSequences` | `array<obj>` | 1 per map: `Main_Sequence`, or null in ASAMUEntry (no Kismet) |
| 6 | `TextureToInstancesMap` | `map<obj, array<20-byte instance>>` (`i32` count, then key + array) | 2 – 1,300 textures |
| 7 | `DynamicTextureInstances` | `map<obj, array<32-byte instance>>` | 0 – 4,080 keys, 0 instances everywhere |
| 8 | (unnamed block) | `i32 N` + N bytes, skipped by the loader | N = 16 in every map; meaning UNKNOWN |
| 9 | `CachedPhysBSPData` | bulk<u8> | cooked physics of the BSP |
| 10 | `CachedPhysSMDataMap` | `map<obj, (vec3 scale, i32 index)>` | equals the store's count in every map |
| 11 | `CachedPhysSMDataStore` | `array<array<bulk<u8>>>` | cooked convex data |
| 12 | `CachedPhysPerTriSMDataMap` / `Store` | `map<obj, (vec3, i32)>` / `array<bulk<u8>>` | |
| 13 | `CachedPhysBSPDataVersion`, `CachedPhysSMDataVersion` | `i32`, `i32` | 34,079,889 (BSP version 0 in Freds_place, which has no BSP) |
| 14 | `ForceStreamTextures` | `map<obj, u32>` | 0 – 14 |
| 15 | `CachedPhysConvexBSPData` + version | `array<bulk<u8>>`, `i32` | empty, version 0 |
| 16 | nav / cover / pylon list heads and tails | 6 × obj | |
| 17 | `CrossLevelCoverGuidRefs`, `CoverLinkRefs`, `CoverIndexPairs` | `array<(guid, i32)>`, `array<obj>`, `array<(i32, u8)>` | empty |
| 18 | `CrossLevelActors` | `array<obj>` | empty |
| 19 | `PrecomputedLightVolume` | `u32` initialized; if set: box (vec3, vec3, `u8`), `f32`, `array<33-byte sample>` | set in 10 maps (not ASAMUEntry/ASAMULegal); the `f32` is 0 everywhere |
| 20 | `PrecomputedVisibilityHandler` | vec2, 4 × `i32`, `array<bucket>` | empty in every map |
| 21 | `PrecomputedVolumeDistanceField` | `f32`, box, 3 × `i32`, `array<FColor>` | empty in every map |

Element layouts that the shipped data never exercises (visibility buckets: `i32` + `array<16-byte cell>` +
`array<chunk {u32, i32, array<u8>}>`; distance-field voxels) come from the code reading only: TENTATIVE. The
synthetic tests cover them.

## `UModel` (BSP / brush model) — CONFIRMED (T)

```text
FBoxSphereBounds  vec3 Origin, vec3 BoxExtent, f32 SphereRadius
bulk<vec3>        Vectors                     (normals, texture axes; element size 12)
bulk<vec3>        Points                      (vertex positions)
bulk<FBspNode>    Nodes                       (element size 64)
obj + array<FBspSurf> Surfs                   (owner, then per-element 60 bytes)
bulk<FVert>       Verts                       (element size 24 in all 372 models)
i32 NumSharedSides | i32 NumZones | NumZones × FZoneProperties (24 bytes; at most 64)
obj Polys | bulk<i32> LeafHulls | bulk<i32> Leaves | u32 RootOutside | u32 Linked | bulk<i32> PortalNodes
u32 NumVertices | bulk<FModelVertex> VertexBuffer (36 bytes) | FGuid LightingGuid
array<FLightmassPrimitiveSettings> (36 bytes each)
```

| Element | Layout (little-endian, in order) |
|---|---|
| `FBspNode` (64) | plane `X, Y, Z, W` · `i32` iVertPool · `i32` iSurf · `i32` iVertexIndex · `u16` ComponentIndex · `u16` ComponentNodeIndex · `i32` ComponentElementIndex · `i32` iBack · `i32` iFront · `i32` iPlane (next coplanar) · `i32` iCollisionBound · `u8` iZone[2] · `u8` NumVertices · `u8` NodeFlags · `i32` iLeaf[2] |
| `FBspSurf` (60) | obj Material · `u32` PolyFlags · `i32` pBase (point) · `i32` vNormal · `i32` vTextureU · `i32` vTextureV (vectors) · `i32` iBrushPoly · obj Actor (source brush) · plane `X, Y, Z, W` · `f32` ShadowMapScale · `u32` LightingChannels · `i32` iLightmassIndex |
| `FVert` (24) | `i32` pVertex · `i32` iSide · vec2 ShadowTexCoord · vec2 BackfaceShadowTexCoord (the native code writes 16-byte elements without the last field for some cook targets; not seen) |
| `FZoneProperties` (24) | obj ZoneActor · `u64` Connectivity · `u64` Visibility · `f32` LastRenderTime |
| `FModelVertex` (36) | vec3 Position · packed TangentX · packed TangentZ · vec2 TexCoord · vec2 ShadowTexCoord |
| `FLightmassPrimitiveSettings` (36) | `u32` bUseTwoSidedLighting · `u32` bShadowIndirectOnly · `f32` FullyOccludedSamplesFraction · `u32` bUseEmissiveForStaticLighting · `f32` EmissiveLightFalloffExponent · `f32` EmissiveLightExplicitInfluenceRadius · `f32` EmissiveBoost · `f32` DiffuseBoost · `f32` SpecularBoost |

Field *names* are UE3 conventions (TENTATIVE where only the layout is proven); order and sizes are CONFIRMED.
**A native `FPlane` is serialized `X, Y, Z, W`**, unlike the tagged binary `Core.Object.Plane` value, which stores
`W` first (OBJECT_FORMAT.md).

Value checks over all 372 models (T):

- every node's surface, children, coplanar link, leaves and collision bound, every surface's point/vector
  indices, and every vertex-pool entry a node uses are in range (41,108 references). The pool may hold stale
  entries beyond the last node's range (found in one builder-brush model), so only used entries are checked;
- every node plane normal has unit length;
- the polygon of each node (`NumVertices` entries of `Verts` from `iVertPool`, each naming a `Points` entry)
  winds so that its Newell normal points along the node normal: 2,693 of 2,693 polygons;
- in the 10 level BSPs, 5,191 of 5,274 polygon points lie within 0.1 UU (plus a 1e-5 relative term) of their node
  plane; the other 83 are off by at most 6.7 UU (vertex welding). Volume BSPs are not plane-consistent in places
  (their own BSP is not used by our pipeline).

What the shipped BSPs contain (T):

| Map | Nodes | Surfaces | Triangles |
|---|---:|---:|---:|
| AG-Workshop / ASAMUFrontEndMap / AG-Epilogue | 297 / 252 / 307 | 130 / 121 / 134 | 877 / 747 / 904 |
| AG-Darkcave | 178 | 49 | 486 |
| AG-BeautifulCity, AG-ParadiseCave, AG-IceCave | 6, 6, 12 | 6, 6, 12 | 12, 12, 24 |
| ASAMUEntry, ASAMULegal | 12 | 12 | 24 |
| AG-StarHaven, Freds_place, TheCore | 0 | 0 | 0 |

The playable spaces of the large AG maps are built from static meshes, not BSP (21,283 static-mesh actors and
24,631 static mesh components in all maps), so level collision depends mostly on static-mesh collision (mesh
workstream) and on blocking volumes. In the level BSPs every surface has `PolyFlags` `0xE00` and every node
`NodeFlags` 0 (T): no surface is invisible, non-solid or a portal, so the drawn and the blocking triangle sets are
identical.
The flag classification in `bsp::is_visible_surface` / `is_collision_surface` (UE3 bits `0x1` invisible, `0x8`
not solid, `0x4000000` portal) is TENTATIVE and not exercised; the meaning of the `0xE00` bits is UNKNOWN.

## `UPolys`, `FPoly`, `BrushComponent` — CONFIRMED (T)

```text
UPolys          i32 Num | i32 Max (>= Num) | obj Owner | Num × FPoly
FPoly           vec3 Base, Normal, TextureU, TextureV | array<vec3> Vertices | u32 PolyFlags | obj Actor
                | FName ItemName | obj Material | i32 iLink | i32 iBrushPoly | f32 ShadowMapScale
                | u32 LightingChannels | FLightmassPrimitiveSettings | FName RulesetVariation
BrushComponent  (after tags) array<bulk<u8>> CachedPhysBrushData   (cooked physics convex elements)
```

`FPoly` vertices are in brush-local space. Four in-memory bytes between `iBrushPoly` and `ShadowMapScale` (UE3:
`SmoothingMask`, TENTATIVE) are not serialized.

## Brushes and volumes

What the cooked maps keep, per actor kind (T, from the scenes):

| Actors | Count | `BrushComponent.BrushAggGeom` convex hulls | `Model` → `Polys` polygons |
|---|---:|:---:|:---:|
| `BlockingVolume` / `DynamicBlockingVolume` | 844 | yes | **stripped** |
| CSG `Brush` | 162 | no | yes (161; the builder brush of Freds_place has an empty `Polys`) |
| kill zones, trigger volumes, post-process / reverb / lightmass volumes | 198 | yes | yes |

- The collision shape of a volume is the tagged `BrushAggGeom.ConvexElems[]` (`VertexData`, `FaceTriData`
  triangles, `FacePlaneData`, `ElemBox`, ...): 1,323 hulls, all with triangles. Hull vertices are in the same local
  space as the brush polygons: for 194 of the 198 volumes that keep both, the world bounds agree within 1 UU (T);
  the other 4 differ by 9 UU to 21,000 UU (brushes edited after their collision was built, TENTATIVE).
- The cooked PhysX data in the `BrushComponent` tail and in the level (`CachedPhys*`) is counted but not decoded;
  `BrushAggGeom` already gives the convex shapes.
- None of the 1,204 brushes and volumes in the shipped maps has a rotation, a `PrePivot` or a mirroring (negative
  determinant) scale; 6 are scaled (T). Hull triangles and brush-polygon fans keep their local winding: under a
  mirroring transform (none shipped) they would face inwards, while the transformed hull planes stay correct.

## Transforms — CONFIRMED (native code), partly data-verified

- `AActor::LocalToWorld`: `world = R(S · (local − PrePivot)) + Location`, with `S = DrawScale · DrawScale3D` and
  `R` the UE3 rotation matrix of `(Pitch, Yaw, Roll)` (rows: `(CP·CY, CP·SY, SP)`, `(SR·SP·CY − CR·SY,
  SR·SP·SY + CR·CY, −SR·CP)`, `(−(CR·SP·CY + SR·SY), CY·SR − CR·SP·SY, CR·CP)`, row-vector convention).
- Sine and cosine come from a 16,384-entry table indexed by `(angle >> 2) & 0x3FFF` (cosine = the entry a quarter
  turn later): angles are effectively quantized to 4 rotator units. The index computation is CONFIRMED; that the
  table holds `sin(i·2π/16384)` is TENTATIVE (it is filled at run time). `level::rotator_sin_cos` reproduces it.
- Components (`UPrimitiveComponent::SetTransformedToWorld`): the component's own `Scale · Scale3D`, `Rotation` and
  `Translation` are applied first, then the owner's transform; `AbsoluteTranslation` drops the parent translation,
  `AbsoluteScale` normalizes the parent axes, `AbsoluteRotation` keeps only the parent axis lengths.
- Data check (T): every BSP surface records its source brush and polygon. Transforming that polygon with the
  brush's `LocalToWorld` reproduces the surface plane (Newell normal of the whole transformed polygon within 0.999
  of the surface normal up to sign — exactly the 201 surfaces of `CSG_Subtract` brushes face the other way (T) — and plane offset
  within 1 UU) for 437 of 482 surfaces; 80 brushes match on every surface, 8 only partly (TENTATIVE: brushes
  edited after the last BSP build). An earlier version of the test took the normal of the first fan triangle only
  and found 434 / 78 / 10; the difference is that criterion, not the decoding. Because no brush is rotated or
  pre-pivoted, this check covers translation (and the volume check above covers nothing more); rotation and
  pre-pivot rest on the native code.
- The closed forms above are also checked against an independent composition `T(−PrePivot) · S · Roll(X) ·
  Pitch(Y) · Yaw(Z) · T(Location)` of elementary matrices for arbitrary, negative and quantized angles and
  mirrored scales, and the component flags against hand-computed results (`tests/level_synthetic.rs`).
  Re-reading the decompiled `UPrimitiveComponent::SetTransformedToWorld` confirms that `AbsoluteScale` and
  `AbsoluteRotation` act on the parent matrix *rows* (the world images of the local axes).

## Scene extraction (`level::extract_scene`)

For each entry of `ULevel::Actors`:

- **Values** are the object's tagged properties merged over its archetype (the export's `ArchetypeIndex`, followed
  recursively; 80 prefab-instance actors have one (T): 78 in AG-ParadiseCave, 2 in AG-BeautifulCity) or, without an archetype, over its
  class's inherited defaults — the same delta rule as for class defaults (tagged structs merge member-wise; arrays
  and binary structs replace). Components merge over their templates (e.g. `Default__StaticMeshActor.StaticMeshComponent0`).
  An enum property that is not stored takes the enum's first value (e.g. `PHYS_None`).
- **Actor fields**: class, gameplay kind (from the class chain), `Location`, `Rotation`, `DrawScale`,
  `DrawScale3D`, `PrePivot`, the computed `LocalToWorld`, `Base` / `BaseBoneName` / `bHardAttach`, `bHidden`,
  `bCollideActors`, `bBlockActors`, `bStatic`, `bMovable`, `bNoDelete`, `Physics`, `CollisionType`, `Tag`,
  `Group`, `Layer`; `params` = effective values of every property declared by the actor's classes below
  `Engine.Actor` (all actors except plain static meshes, lights, emitters and CSG brushes, unless requested);
  `instance` = the values stored on the instance.
- **Components**: every subobject of the actor whose class is a component (the cooked actors do not store their
  `Components` arrays): class, kind, template, transform and world matrix, collision flags; static mesh path and
  material overrides (T: 21,283 static-mesh actors, 24,631 static mesh components, 9,470 with at least one
  non-null override, 46 of them inherited from the template); light type, brightness, colour, radius, falloff,
  cone angles, enabled and shadow flags (3,437 light components, T); cylinder radius and height (player starts
  40 × 80, T); brush model and hull counts.
- **Volumes and brushes**: world-space convex hulls (vertices, triangles, planes) and brush-polygon triangles.
- **Matinee**: for each `SeqAct_Interp`, the actors bound through `SeqVar_Object` variable links, with the
  `InterpData` and the link name (the Matinee group): 281 actors are driven by Matinee (T).
- **WorldInfo** (title, `KillZ`, `bSoftKillZ`, `DefaultGravityZ`, `GlobalGravityZ`, game type) and the streaming
  sub-levels (`PackageName`, `Offset`, all values).

Sub-levels are extracted as maps of their own (TheCore, Freds_place); the manifest records which map streams which.

## Importer output (`asamu-import levels`)

Written only to the user-local `--out` directory (`<out>/levels/`), validated by the shared safety module (refuses
the repository outside ignored `research/` folders, `.app` bundles, `steamapps` trees and symlinks; symlinked or
`..` paths are resolved before the check) and, in `levels.rs`, refusing anything inside the install root itself
(compared without regard to case), which matters for copies outside Steam whose Windows/Linux layout has neither an
`.app` bundle nor a `steamapps` parent:

| File | Content |
|---|---|
| `<map>.scene.json` | `format` `asamu-scene`, version 1: the scene above. UE3 world space (UU, left-handed, X forward, Y right, Z up), rotators in 65,536 units per turn, row-vector matrices |
| `<map>.bsp.json` + `<map>.bsp.bin` | `format` `asamu-bsp`, version 1: bounds, counts, consistency check, surface table (material, flags, texture origin/axes, source brush, plane) and the byte offset/count of each array in the `.bin` (per triangle set: `f32` vec3 positions, `u32` triangles, `u32` surface index per triangle), UE3 world space; winding as in the BSP (Newell normal along the surface normal) |
| `<map>.bsp.glb` (`--gltf`) | the same triangles as glTF 2.0 for viewing: axes `(y, z, −x)` (the `asamu-core` Bevy mapping), unscaled UU, winding reversed for the handedness flip |
| `manifest.json` | per-map counts, files, title and streaming sub-levels |

The 12 maps convert in about 4 s (release build) to about 70 MB of local files.

## Hostile-input discipline

Every count is checked against the remaining bytes before anything is allocated; pre-allocation is capped at 4,096
elements; bulk arrays must declare an expected element size and each element decoder must consume exactly that
size; `NumZones` is limited to the engine's 64; `Polys.Max` must be at least `Num`; skipped blocks must have a
non-negative size; archetype chains are limited to 16 links (cycles end there with a warning).

Scene extraction is also bounded where shared data could multiply (a crafted package can let thousands of small
exports inherit one large archetype, template or brush model):

- a repeated `ULevel::Actors` entry is skipped with a warning (`stats.duplicate_slots`);
- each brush `Model`/`Polys` pair is decoded once per scene, however many brushes reference it;
- merging is linear (hashed property keys), and an object with nothing of its own shares its base values instead
  of copying them;
- merged values, parameter maps, material lists and Matinee references count against a budget
  (`SceneOptions::max_merged_weight`, default 2^24 units of roughly 100 bytes; the largest shipped map,
  AG-StarHaven, uses 2.7 million, also with `--all-params`), and world-space volume geometry against another
  (`SceneOptions::max_geometry`, default 2^22 elements; AG-StarHaven builds 12,988). Past a budget the values or
  geometry are left out with a warning (`stats.budget_skips`, 0 in every shipped map, T);
- hull triangles are kept only when every vertex and index of the convex element decodes, so a dropped element
  cannot shift indices onto the wrong vertices.

Tests: `tests/level_synthetic.rs` truncates synthetic `Model` (both vertex sizes), `Polys`, `BrushComponent` and
`Level` tails at every offset (always an error) and overwrites every byte and every 4-byte position with extreme
values (never a panic), and checks impossible counts, element sizes and element decoders that under-read.
`tests/level_scene_synthetic.rs` builds whole synthetic map packages (level, WorldInfo, brushes sharing a model,
archetypes) and checks repeated, foreign, import and out-of-range actor entries, broken brush references,
archetype cycles, both budgets, and scene extraction after corrupting every byte of the package with five values.

## Independent re-check — CONFIRMED (2026-10-10)

A second decoder written independently in Python (throwaway, kept under the ignored `research/local/`, deleted
afterwards; its own summary/table parser, the reference `liblzo2` for decompression, its own object prelude and
tag skipping, and native-tail decoders written from the tables on this page) decoded every `Level`, `Model`,
`Polys` and `BrushComponent` export of the 12 maps (exact consumption 12 / 372 / 372 / 1,204) and was compared with
the importer output (`scene.json`, `bsp.json`, `bsp.bin`): **0 disagreements** in every `ULevel` tail field, the
actor list (order, slots, nulls, the `RF_HasStack` rule, outers, class census), all 30,284 actor `LocalToWorld`
matrices recomputed from the stored tags, 73,858 stored location/rotation/scale/pivot values, the instance value
sets, 41,108 BSP references, 2,693 winding checks, the 5,274 / 83 / 6.661 UU plane check, the per-map BSP counts,
every level surface (flags, plane, source brush and polygon, material path), every BSP triangle and its surface
tag (bit-identical), all 11,834 world-space brush-polygon vertices, hull counts per volume (1,323), cached-physics
element counts per brush component, the brush/volume table above, gameplay instance deltas, `KillZ` tags,
streaming levels, static mesh paths of the 24,241 components that store one, material overrides, light components
and the 281 Matinee-driven actors. Its brush-polygon transform check reproduces 437 / 80 / 8 with the Newell
criterion.

## UNKNOWN / not done

- The meaning of the 16-byte block the level loader skips, of the light volume's `f32` after its bounds (0
  everywhere) and of the `0xE00` surface flags.
- Visibility-handler and distance-field element layouts (never present in the data; TENTATIVE from code).
- Cooked PhysX data (`CachedPhysBrushData`, `CachedPhys*` of the level) is not decoded.
- Static-mesh, skeletal-mesh, terrain-like actors (`InstancedFoliageActor` 5, `SpeedTreeActor` 4,
  `FluidSurfaceActor` 2) carry geometry outside this work: static meshes belong to the mesh workstream, the others
  are not decoded.
- Rotation and `PrePivot` of the actor transform are verified by native code only (no rotated brush exists).
- Matinee track contents (`InterpData`, `InterpGroup`, `InterpTrackMove` keys) are referenced, not decoded.
- Gameplay *behaviour* of the extracted actors (checkpoint triggering, falling rocks, kill zones) is specified in
  ABILITIES.md / GRAPPLE.md and implemented by the gameplay workstream, not here.
