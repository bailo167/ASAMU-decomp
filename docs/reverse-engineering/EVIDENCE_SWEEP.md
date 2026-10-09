# Evidence sweep: independent checks for "implemented" tracker items (2026-10-10)

Purpose: for tracker items that were `implemented`, run checks that do **not** use our Rust code, and record whether
the results justify `verified` (CONFIRMED agreement with the original data for the tracked scope) or why not. The
orchestrator updates `progress/progress.toml` from this page.

Evidence source: the 42 packages, the three `.tfc` texture caches and the shipped `.ini` files of the legitimately owned
Mac install (Steam build 1822049), read only. Nothing below is game content: counts, structure and pass/fail only.

## Method

- **A second decoder, written for this sweep** as throwaway Python under the git-ignored `research/local/` (deleted
  afterwards, not committed). It shares no code with `asamu-ue3`:
  - its own summary, chunk and table parsing; every LZO block (package chunks and texture payloads) decompressed by
    the reference `liblzo2` (`lzo1x_decompress_safe`) instead of `lzo.rs`;
  - its own tagged-property walker and object prelude (state frame, component template, NetIndex, dominant-light
    shadow-map prefix), written from [OBJECT_FORMAT.md](OBJECT_FORMAT.md);
  - its own value resolution: own tags, then the export's archetype, else the class default object, and for a default
    object the super class's default object, resolved across packages by object path;
  - its own native readers for `Texture2D` mips ([TEXTURES.md](TEXTURES.md)), `ULevel` actor lists, `UModel`
    (bounds, `Vectors`, `Points`, `Nodes`, `Surfs`, `Verts`, zones, `Polys`) and `UPolys`/`FPoly`
    ([LEVEL_FORMAT.md](LEVEL_FORMAT.md)); its own BC1/BC3 block decoder and DDS header parser; its own
    actor/component transform composition (closed-form rotation matrix with the 16,384-entry quantized sine table
    documented in LEVEL_FORMAT.md).
- **Independence caveat.** The second decoder was written from our own format documentation, so it re-checks the
  *implementation* (importer, scene/graph builders, CLI output), not the *format understanding*. The format
  understanding itself was verified separately (exact payload consumption, earlier independent decoders); where it
  matters, the original executable was consulted (see [PACKAGE_ANALYSIS.md](PACKAGE_ANALYSIS.md)).
- **What was compared.** The files our tools write: `asamu-import textures` (DDS + manifest), `asamu-import levels`
  (scene JSON, BSP JSON/bin), `asamu-inspect kismet --out-dir` (graph JSON + DOT), `asamu-inspect --json map`,
  `asamu-inspect --json props`, and the committed `docs/reverse-engineering/data/defaults/*.json`. Output went to
  `research/local/` and was deleted after the checks.

To repeat (any independent decoder written from the docs will do; keep every output local):

```sh
export CARGO_TARGET_DIR=target/agents
C="$HOME/Library/Application Support/Steam/steamapps/common/A Story About My Uncle/A Story About My Uncle.app/Contents/Resources/ASAMU/CookedMac"
cargo build -p asamu-import -p asamu-inspect
OUT=research/local/<you>/conv
target/agents/debug/asamu-import --out "$OUT" textures --package <file name>     # one package per run
target/agents/debug/asamu-import --out "$OUT" levels                            # all 12 maps (scene + BSP)
target/agents/debug/asamu-inspect kismet "$C" --out-dir research/local/<you>/kis
target/agents/debug/asamu-inspect --json map "$C/Maps/<map>.asamu"
target/agents/debug/asamu-inspect --json props "$C/<package>" '#<export index>'
```

## Summary

| Tracker item | Independent check | Result | Recommendation |
|---|---|---|---|
| importer / `texture-convert` | every DDS the importer writes, decoded back, vs an independent decode of the source mips (inline and `.tfc`) | 8,883 / 8,883 textures and 22 / 22 cube maps byte-identical; 73,541 mips, 2,083,163,931 texel bytes; headers and formats agree; texel decode compared on 5,850 textures | **verified** |
| importer / `level-convert` | scene JSON of all 12 maps vs independent actor list, values, components, transforms, lights, volumes; BSP bin vs independent BSP triangulation | 30,284 / 30,284 actors, 39,961 components, 3,437 lights, 1,323 hulls, 360 brush-polygon sets, 3,110 BSP triangles — 0 differences | **verified** |
| kismet / `sequence-objects` | independent Kismet object set, classes, names, parents per map | 3,504 / 3,504 objects; classes, effective `ObjName` and parent sequences all agree | **verified** |
| kismet / `links` | independent decode of every output, variable and event link | 3,362 / 3,362 stored links identical (kind, ports, input index); 61 / 61 derived edges | **verified** |
| kismet / `graph-export` | graph JSON nodes/edges as above; DOT consistent with JSON | 3,423 DOT edges = 3,423 JSON edges; 78 clusters = 78 sequences; 3,230 node declarations = every non-frame node | **verified** |
| maps / `actor-census` | independent class census vs `asamu-inspect map`; instance counts and overrides vs `map_instances.json` | 12 / 12 maps identical; 9 / 9 classes' per-map counts; 31 / 32 override statistics (the 32nd differs only by a documented reporting filter) | **verified** |
| maps / `player-starts` | player starts and checkpoints from independent actor lists; transforms; collision cylinders | 10 player starts, 109 checkpoints (per-map counts agree), all transforms identical, 10 / 10 cylinders 40 × 80 | **verified** |
| maps / `volumes` | brush components, convex-element counts, hull vertices, brush polygons | 1,204 / 1,204 components; 1,323 hulls (10,822 vertices); 2,950 polygons (11,834 vertices) identical | **verified** |
| maps / `streaming` | `WorldInfo.StreamingLevels`, summary `AdditionalPackagesToCook`, Kismet streaming action | both relationships confirmed three ways; 12 / 12 maps agree with the scene JSON | **verified** |
| objects / `cross-package` | super chains and inherited defaults resolved across packages vs committed defaults; import binding | 31 / 31 chains; 1,177 / 1,177 values with the same providing class; 205 / 205 template values; 389 / 389 "zero" values unstored; 10,259 / 10,259 resolvable imports bind to `Public` exports | **verified** |
| objects / `object-summary` | `asamu-inspect --json props` on a sample across all packages vs independent decode | 1,700 objects: prelude, 6,473 tags (name, type, index, size, struct), end offset and 5,262 scalar values identical | **verified** for the sampled scope (see note) |
| script / `movement-script` | — | cannot be verified from files alone | **stays implemented** |
| script / `grapple-script` | — | cannot be verified from files alone | **stays implemented** |

## importer / texture-convert — verified

Check: for every package that holds textures (21), the importer was run into an empty folder (one package per run, so
that nothing is skipped as a cross-package duplicate). For every `Texture2D`, `LightMapTexture2D`,
`ShadowMapTexture2D` and `TextureFlipBook` export (not default objects) the second decoder read the tags (`Format`,
`SizeX`, `SizeY`, `TextureFileCacheName`), the mip records, loaded inline payloads or `.tfc` ranges, and decompressed
LZO payloads with `liblzo2`. It then compared with the DDS file named for that object in `manifest.json`:

- DDS width × height = the natural size of the first stored mip, mip count = stored mips, pixel format per the
  documented mapping (FourCC `DXT1`/`DXT5`; 32-bit RGBA masks `00FF0000/0000FF00/000000FF/FF000000`; 8-bit
  luminance; DX10 `R8G8_SNORM`);
- DDS payload = the concatenated stored mips, byte for byte;
- texels: the first stored mip decoded independently from both sides (BC1/BC3, and BGRA→RGBA through the DDS masks)
  for 5,850 textures (203,000,741 texels; DXT mips up to 256 × 256 and every `A8R8G8B8` texture);
- cube maps: the cube DDS = the six faces' mip chains in the order of `FacePosX` … `FaceNegZ`, with the cube-map cap.

Result: 8,883 / 8,883 textures pass all checks (every texture with the `Texture2D` layout in the install, all five
formats), 22 / 22 cubes byte-identical. 73,541 mips and 2,083,163,931 bytes were compared — exactly the stored-mip
totals of TEXTURES.md. 0 failures.

Not covered: PNG previews (optional) and manifest metadata beyond file, format, size and mips (`srgb`, address, filter,
LOD group, which come from the already verified property decoder).

Recommendation: **verified** — every byte the importer writes for texels agrees with an independent decode of the
original data.

## importer / level-convert — verified

Check: `asamu-import levels` for all 12 maps, compared with the second decoder:

- **Actor list**: `ULevel::Actors` decoded independently; scene actors appear in slot order with the same export
  indices — 12 / 12 maps, 30,284 actors, 319 null slots.
- **Actor values** (resolved through archetypes and class defaults): class, `Location`, `Rotation`, `DrawScale`,
  `DrawScale3D`, `PrePivot`, `Tag`, `bHidden`, `bCollideActors`, `bBlockActors`, `bStatic` — 30,284 / 30,284 per field.
- **Actor matrix**: `LocalToWorld` recomposed independently (relative tolerance 1e-5) — 30,284 / 30,284.
- **Components**: the scene lists exactly the actor's component subobjects (30,284 / 30,284 actors); `Translation`,
  `Rotation`, `Scale`, `Scale3D` and the three `Absolute*` flags — 39,961 / 39,961; static-mesh references
  24,631 / 24,631; world matrices of the 32,630 non-absolute components (tolerance 1e-4) — all agree.
- **Lights**: brightness, colour, enabled and shadow flags (3,437), radius and falloff (3,429), cone angles (134) — all
  agree.
- **Volumes**: convex-element count of each `BrushComponent.BrushAggGeom` 1,204 / 1,204; 1,323 hulls with 10,822
  vertices transformed to world space agree; brush polygons (`Brush` → `Model` → `Polys`, fan-triangulated, world
  space) for 360 actors, 2,950 polygons, 11,834 vertices — positions and indices identical.
- **BSP**: the level `Model` decoded independently and every node polygon fan-triangulated: 3,110 triangles in the
  9 maps with BSP geometry; the importer's visible and collision sets are identical in order, positions and surface
  index (the other 3 maps have an empty BSP).
- **Streaming and WorldInfo**: see maps / streaming.

Not covered: the optional glTF viewer file, the full `params`/`instance` maps (their values come from the verified
property decoder), and the cooked PhysX blobs (counted, not decoded, by design).

Recommendation: **verified**.

## kismet / sequence-objects, links, graph-export — verified

Check: the graph JSON and DOT written by `asamu-inspect kismet --out-dir` for all 12 maps, against the second decoder:

- **Objects**: every non-default export whose class chain (resolved across packages) contains
  `Engine.SequenceObject` — the node set is identical in every map, 3,504 objects (3,481 level scope + 23 prefab
  archetypes); class 3,504 / 3,504; effective `ObjName` 3,504 / 3,504.
- **Parents**: each object's sequence, from the owning sequence's `SequenceObjects` array, equals the graph parent:
  3,504 / 3,504.
- **Stored links**: `OutputLinks[].Links[]` (`LinkedOp`, `InputLinkIdx`), `VariableLinks[].LinkedVariables[]`
  (`matinee` when the target is an `InterpData`) and `EventLinks[].LinkedEvents[]`: the multiset of
  (kind, source, port, target, input index) is identical in every map — 3,362 links (3,345 in level trees, 17 in
  prefab archetypes). 0 dangling.
- **Derived edges**: remote-event (same `EventName`) and named-variable (`FindVarName` = `VarName`) edges recomputed
  within each level tree: 61 / 61.
- **DOT**: one arrow per JSON edge (3,423), one cluster per sequence (78), one node declaration per non-frame node
  (3,230; frames are omitted by design).

Recommendation: **verified** for all three items.

## maps / actor-census, player-starts, volumes, streaming — verified

- **actor-census**: the class census of all exports (class and class package) equals `asamu-inspect --json map` in
  12 / 12 maps, as do export/name/import counts, world and level paths, `AdditionalPackagesToCook` and the
  `ContainsMap` flag. Against `docs/reverse-engineering/data/defaults/map_instances.json` (actor-list census): 9 / 9
  classes have the same per-map instance counts; 31 of 32 override statistics (instances, distinct values, min/max)
  agree. The 32nd, `WorldInfo.DefaultPostProcessSettings`, is stored on 7 instances, but 5 of them carry large struct
  deltas that the generator leaves out by design ("scalar and small-struct values only"); the 2 it reports agree.
- **player-starts**: 10 player starts (one in each map except TheCore and Freds_place) and 109 `ASAMUCheckpoint`s
  with the per-map counts of LEVEL_FORMAT.md; their transforms are part of the 30,284-actor check above; all 10
  player-start cylinders resolve to radius 40, height 80, as in the scene; checkpoint overrides
  (`checkpointIndex` 101, `spawnPointOffset` 97, `bTriggeredFromKismet` 8) agree with `map_instances.json`.
- **volumes**: 1,204 `BrushComponent` exports; convex elements, hull vertices and brush polygons as in level-convert;
  91 `ASAMUKillZone` + 3 `ASAMUDynamicKillZone`.
- **streaming**: AG-BeautifulCity streams `freds_place` through a `LevelStreamingAlwaysLoaded` and AG-IceCave
  `thecore` through a `LevelStreamingKismet` (both with zero offset) — read from `WorldInfo.StreamingLevels`, from the
  summary's `AdditionalPackagesToCook`, and (IceCave) from the `SeqAct_MultiLevelStreaming` `Levels[]` entry naming
  `thecore`. The scene JSON agrees in 12 / 12 maps (10 with none).

Recommendation: **verified** for all four items.

## objects / cross-package — verified

Check: the second decoder's own cross-package resolution (object paths across `Core`, `Engine`, `GameFramework`,
`UDKBase`, `UTGameContent`, `GFxUI`, `IpDrv`, `OnlineSubsystemSteamworks` and `Startup.upk`) against the committed
class-default files (generated by our `model.rs`):

- super chains of all 31 classes: 31 / 31 identical;
- every property with a stored source (`cdo`/`inherited`): 1,177 / 1,177 values equal **and** the providing class
  equal (localized strings compared by length, as the files omit their text); 83 complex values (tagged structs,
  arrays, delegates) not compared;
- component templates: 205 / 205 values;
- properties listed as zero: 389 / 389 have no stored value anywhere in the chain;
- import binding: 10,259 imports resolve to an export of another shipped package with the same path and class (all
  target exports are `Public`). The other 889 are explained: 345 class imports with no class export anywhere in the
  install (intrinsic classes such as `Core.Package` or `Core.ArrayProperty`; "native-only" is the UE3 reading,
  TENTATIVE), 133
  top-level package imports and 71 group-package imports (package objects, not exports), 319 objects in packages that
  are not shipped (288 of them referenced by `UTGameContent.u`, the stock UT sample content) and 21 other objects
  (members of native-only classes; editor meshes referenced by maps).

Recommendation: **verified**.

## objects / object-summary — verified (sampled)

Check: `asamu-inspect --json props` on an evenly spaced sample of up to 60 non-class exports per package (every package
except the shader caches and the two cooker-data packages), compared with the second decoder: NetIndex, the end
offset of the tagged properties, every tag's name, type, array index, size and struct name, and every scalar value
(int, float, bool, name, string, object index, byte/enum, `Vector`, `Rotator`).

Result: 1,700 objects, 6,473 tags, 5,262 scalar values — 0 differences. (6 further sampled objects could not be read by
the throwaway decoder because its component test keys on the class name; this is a limitation of the check, not a
disagreement.)

Recommendation: **verified** for the summaries' decoding. The exhaustive object decode was already independently
re-checked (OBJECT_FORMAT.md); this sweep adds the CLI summaries on top. The note on the tracker ("native data tails of
42,026 exports not yet decoded") is partly outdated: the texture, mesh, level, material, sound and animation tails now
have their own decoders.

## script / movement-script, grapple-script — stay implemented

These items are behaviour specifications ([ABILITIES.md](ABILITIES.md), [GRAPPLE.md](GRAPPLE.md)) written from local
reading of the shipped script and bytecode and from the native physics code. What can be checked from files already
is: the constants (class defaults, drift-tested against the committed JSON and re-resolved above), the bytecode decode
(12,801 / 12,801 scripts exact), and the native physics port against the decompiled algorithm. What `verified` would
need is a **behavioural oracle**: traces of the original game (time, input, position, velocity, grapple state) replayed
by our deterministic simulation within a tolerance ([TRACE_CAPTURE.md](../TRACE_CAPTURE.md),
[PARITY.md](../PARITY.md)). No original traces exist yet, and this sweep may not run the original game. A second
reading of the same sources would only repeat the adversarial re-checks the specs already had; it would not remove the
open questions the specs mark as "to verify by trace" (for example frame-rate dependent release speeds).

Recommendation: **stay implemented** until original traces are replayed.

## Side findings

- **Editor-only exports.** 16,730 exports have only the `LoadForEdit` load-context bit, and the original loader never
  creates them in a game process (PACKAGE_ANALYSIS.md, "Object, export and name flags"). Our scene JSON lists their
  component subobjects (e.g. 6,881 light-radius helper components) and 158 of the 162 CSG `Brush` actors. All of
  these are inert for collision (CSG brushes have no hulls and non-blocking components; checked), but a runtime that
  wants to mirror the original object set can drop them with `asamu_ue3::flags::object::loaded_in_game`.
- **Package flags and `PackageSource`** are now documented with executable evidence (PACKAGE_ANALYSIS.md).
- The Kismet totals differ by scope only: 3,362 stored links over all objects, 3,345 in level trees (KISMET.md counts
  the latter).
