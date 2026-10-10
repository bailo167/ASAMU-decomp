# Water, instanced foliage and SpeedTree

Scope: what the shipped maps contain in instanced static meshes (painted foliage), fluid surfaces (water) and
SpeedTree placements, how that data is serialized, how the importer exports it and how the runtime draws it.

**Evidence and publication rules.** Map exports decoded with `asamu-inspect` and the importer (counts, property
values, positions — nothing copied into the repository), serializers and helpers of the unstripped Mac executable
decompiled locally with Ghidra (output in ignored `research/local/`, never committed), and class default objects.
Labels as in `CLAUDE.md`; sources in brackets: (map), (cdo), (native), (src).

## 1. Census of the shipped maps [CONFIRMED (map)]

| Map | `InstancedStaticMeshComponent` (instances) | `InstancedFoliageActor` meshes / instances / clusters | `FluidSurfaceActor` | `SpeedTreeActor` |
|---|---|---|---|---|
| AG-BeautifulCity | 1 (12): `AsamuProps.Meshes.rockpile_06` | 1 / 13 / 1 | 0 | 0 |
| TheCore (streamed into AG-IceCave) | 101 (4,531): 13 crystal meshes `IceCave_Cave.Meshes.Paintable_crystal*` | 13 / 8,765 / 101 | 0 | 0 |
| AG-StarHaven | 0 | 1 / 100 / 0 | 0 | 0 |
| AG-Workshop, AG-Epilogue | 0 | 1 / 1 / 0 | 0 | 0 |
| AG-ParadiseCave | 0 | — | 2 | 0 |
| AG-Darkcave | 0 | — | 0 | 4 |
| others | 0 | — | 0 | 0 |

Every instanced component of the shipped maps is a foliage cluster component (a subobject of the map's
`InstancedFoliageActor`); there are no other instanced or interactive-foliage components, no `FoliageComponent`, and
no `SpeedTree` asset object in any shipped package (only `Engine.Default__SpeedTree`).

## 2. Instanced foliage

### 2.1 Format [CONFIRMED (native, map)]

- `InstancedStaticMeshComponent` native data (after its tagged properties): the static mesh component's light-map
  LOD data, then a bulk array of 80-byte instances `FMatrix Transform, FVector2D LightmapUVBias, FVector2D
  ShadowmapUVBias` (already decoded by `asamu_ue3::lightmap`, LIGHTMAPS.md).
- `AInstancedFoliageActor::Serialize`: the actor's tagged properties, then the `FoliageMeshes` map
  (`TMap<UStaticMesh*, FFoliageMeshInfo>`):

```text
i32 count, then per entry:
  obj StaticMesh                                   (the key)
  TArray<FFoliageInstanceCluster>   i32 n, each: FVector Origin, FVector BoxExtent, f32 SphereRadius,
                                    obj ClusterComponent, TArray<i32> InstanceIndices
  TArray<FFoliageInstance>          i32 n, each 64 bytes: obj Base, FVector Location, FRotator Rotation,
                                    FVector DrawScale3D, i32 ClusterIndex, FRotator PreAlignRotation,
                                    u32 Flags, f32 ZOffset
  obj Settings                                     (InstancedFoliageSettings; present from version 833)
```

Field order from the stream operators of `FFoliageMeshInfo` (clusters, then instances; extra tables only while
transacting in the editor), `FFoliageInstanceCluster` and `FFoliageInstance` (`ClusterIndex`, `PreAlignRotation`
and `Flags` from version 830, `ZOffset` from 850; the shipped packages are version 868). Every foliage actor of every
shipped map decodes to its exact serial size with no warning (importer test `foliage_matches_the_original_data`).

### 2.2 Meaning [CONFIRMED (map) unless marked]

- **Only clustered instances are drawn.** The instances of a cluster are exactly the instances of its
  `ClusterComponent`, in `InstanceIndices` order: for all 4,531 instances of TheCore and the 12 of
  AG-BeautifulCity, the instanced component's matrix translation equals the foliage instance's `Location` exactly and
  its row lengths equal `DrawScale3D` (two independent serializations of the same data agree), and the static meshes
  match. Instances with `ClusterIndex` −1 have no `Base` and belong to no cluster: they are the editor's free slots
  (deleted foliage) and are drawn by nothing. So AG-StarHaven's 100, AG-Workshop's and AG-Epilogue's single
  foliage instances are **not visible in the original**, and neither are 4,234 of TheCore's and 1 of
  AG-BeautifulCity's.
- **World matrix of an instance** = its serialized matrix followed by the component's world matrix (row-vector
  convention). Every shipped cluster component has the identity world matrix (the foliage actor sits at the origin),
  so the two readings "instance matrix is world" and "instance × component" give the same result on the shipped
  data; the general rule is STRONG (UE3's instanced-mesh convention), not exercised by the data.
- The instance matrix is the foliage instance's scale and rotation: its rows are the axes of the rotation built
  from `Rotation`, each scaled by the matching `DrawScale3D` component (largest element difference 0.00065 over all
  4,543 instances; row lengths equal the scale to 3 × 10⁻⁷ relative). Every clustered instance is in exactly one
  cluster and its `ClusterIndex` is that cluster's position in the mesh's cluster list.
- Instanced components carry their own light-map region; each instance offsets it by its `LightmapUVBias`.
- **No collision.** All 102 cluster components have `CollideActors` and `BlockActors` off (and block neither
  zero-extent nor non-zero-extent traces): shipped foliage is decoration only, and the runtime's collision does not
  need it.

## 3. Water (fluid surfaces)

### 3.1 Data [CONFIRMED (map, cdo)]

AG-ParadiseCave has two `FluidSurfaceActor`s at the same height (Z ≈ 102,858.7 UU, about 8,000 UU below the player
start), each with a `FluidSurfaceComponent` of `FluidWidth` × `FluidHeight` ≈ 49,999 × 49,999 UU, the material
`ParadiseCave.Materials.M_FluidActor_Water`, and otherwise the class defaults (`GridSpacing` 10, simulation on,
`SimulationQuadsX/Y` 200, `FluidHeightScale` 1, ...). Neither is rotated or scaled. Their centres are about
49,727 UU apart along X and 5,300 UU along Y, so the two squares lie side by side and overlap in a strip about
270 UU wide where both surfaces are coplanar. The material is a plain translucent `Material` (no instance
parameters): a macro water texture tiled at 0.01 for the base colour and a sky-reflection texture in the emissive
(`asamu-import materials` description; approximated).

### 3.2 Geometry [CONFIRMED (native) unless marked]

- `UFluidSurfaceComponent::UpdateBounds`: the surface spans ±`FluidWidth`/2 along the component's local X axis and
  ±`FluidHeight`/2 along local Y around the component origin (constant 0.5 read from the executable), ±10 UU in Z.
- Fluid vertices carry a height, a 2-D texture coordinate and a height gradient (20 bytes); the border geometry
  writes texture coordinates that run from 0 to 1 across the surface, and positions come from them in the vertex
  shader: UVs are normalized over the whole rectangle (STRONG: `FFluidSimulation::UpdateBorderGeometry`). With the
  material's 0.01 tiling the macro texture is stretched over the whole sea.
- The height-field simulation (impacts, `ForceImpact`, detail grid) runs only near the viewer
  (`DeactivationDistance` 3,000 UU class default); with the camera thousands of units above, the shipped water is
  effectively flat (TENTATIVE: visual consequence not observed).

## 4. SpeedTree [CONFIRMED (map, native)]

- AG-Darkcave has four `SpeedTreeActor`s with a `SpeedTreeComponent` each, placed and scaled, but none of the four
  components names a tree: the `SpeedTree` property is absent on the instance and the archetype is the class default
  (no tree). Nothing is drawn for them in any build.
- No package contains a `SpeedTree` asset. Even if one did, the Mac build could not draw it: `USpeedTree::Serialize`
  reads the size of the tree's binary blob and skips over it without reading it.
- Recreation: the placements are exported and logged; nothing is drawn (this matches the original).

## 5. Export (`asamu-import levels`, additive `foliage` block)

`<map>.scene.json` gains a last key `foliage` after `atmosphere` (all earlier bytes unchanged; test
`assert_scene_bytes_kept`). Version 1:

| Key | Content |
|---|---|
| `instanced_meshes[]` | `slot`, `actor`, `actor_class`, `component`, `class`, `static_mesh`, `materials`, `local_to_world` (component), `hidden`, `instances[]` = `local_to_world` (instance world matrix, §2.2), `lightmap_uv_bias`, `shadowmap_uv_bias` |
| `foliage_actors[]` | `slot`, `actor`, `meshes[]` = `static_mesh`, `settings`, `clusters[]` (`component`, `bounds_origin`, `bounds_extent`, `sphere_radius`, `instance_indices`), `instances[]` (`base`, `location`, `rotation`, `draw_scale3d`, `cluster_index`, `pre_align_rotation`, `flags`, `z_offset`) |
| `fluid_surfaces[]` | `slot`, `actor`, `actor_class`, `location`, `component`, `class`, `local_to_world`, `hidden`, `params` (effective component values: own over archetype over class defaults), `material` (path, class, parent, vector and scalar parameters) |
| `speedtrees[]` | `slot`, `actor`, `actor_class`, `component`, `local_to_world`, `hidden`, `speedtree` (null when none), `params` |
| `warnings[]` | non-fatal problems (none on the shipped maps) |

Bounds against damaged data: 65,536 instances per instanced component or foliage mesh, 262,144 per map over all
instanced components, 4,096 meshes per foliage actor, 1,024 placements (water and SpeedTree together); every count
of the native foliage data is checked against the remaining bytes before anything is allocated, so what is decoded
never exceeds what the bytes could hold (tests `foliage_actor_native_data_decodes_exactly`,
`damaged_foliage_data_never_panics_or_over_allocates`).

## 6. Runtime

- `asamu_assets::scene` reads the block: each instanced component becomes one `MeshInstance` per instance
  (`component_name` `<component>#<index>`, `instance: Some(index)`), so the level plan and the renderer draw foliage
  like any static mesh (shared primitives and materials). An instanced component without instance data (a scene
  converted before the block existed) is no longer drawn once at its component origin; it is counted in
  `LevelSceneStats::instanced_without_data`. Water surfaces and SpeedTree placements are in `LevelScene::foliage`.
  The reader keeps the importer's bounds against a damaged or hand-edited scene: at most 262,144 instances per
  scene (`MAX_INSTANCED_DRAWS`), 1,024 water surfaces and 1,024 SpeedTree placements (`MAX_FOLIAGE_PLACEMENTS`),
  and each instance list is drawn by one component only (a scene that repeats a component cannot multiply the
  draws); what is left out is counted in `FoliageInfo::over_limit` and reported by the water plugin's log line.
- Light maps are not applied to instances yet: the per-component light-map lookup would give every instance the
  whole component's region; the per-instance `lightmap_uv_bias` is exported for that.
- `apps/asamu/src/water.rs` draws each water surface as a flat 32 × 32-quad rectangle with UVs 0..1 and an
  approximate material (the converted material's textures and UV transform, colours clamped to 0..1, opacity at most
  0.85, roughness 0.08, double-sided; ours). The fluid simulation is not reproduced. The two shipped surfaces are
  drawn as two blended rectangles, so the strip where they overlap (§3.1) is blended twice and can show as a
  slightly denser band (ours; not checked by eye).

Local check (2026-10-10, levels of eight maps plus the 14 foliage meshes and the two water textures converted under
ignored `research/local/`, deleted afterwards): `--level AG-IceCave --all-sublevels` logs 101 instanced components
with 4,531 instances drawn and the crystals render along TheCore's cave walls; `--level AG-ParadiseCave` logs two
water surfaces and a camera above the start sees a flat water plane to the horizon (local screenshots, not committed).

## 7. Verification pass (2026-10-10)

Independent of the importer's decoder and of the first pass:

- **The export is additive.** Every map was converted with the current importer and compared with a conversion made
  before the foliage block existed (another workstream's local output): with the appended `,"foliage":{…}` cut off,
  all 12 `<map>.scene.json` files have the same SHA-256 as before, the 24 BSP files are byte-identical and the
  manifest has the same content.
- **The foliage layout.** A separate Python decoder of §2.1's layout, run over the decompressed `TheCore` and
  `AG-BeautifulCity` packages, tried every start offset in the first 64 KiB of the foliage actor's export: exactly
  one (the end of the tagged properties) consumes the export to its last byte, and every field of every instance
  and cluster it reads equals the importer's export (8,765 + 13 instances, 101 + 1 clusters, 0 differences).
- **The census of §1 and the meaning of §2.2** were recounted from the exported block (numbers as stated; added
  above: the rotation and cluster-index agreement, the collision flags, the overlap of the two water surfaces) and
  are now asserted by the importer's real-data test.
- **The runtime** reads a fresh conversion as stated (the app's gated test: two water surfaces with usable meshes
  in AG-ParadiseCave; 101 instanced components, 4,531 instances and none without data in AG-IceCave with TheCore
  merged), and the app starts AG-ParadiseCave with the water plugin logging its two surfaces (levels, collision
  meshes and Kismet only, no textures: a plain plane; nothing committed).
- **Not re-derived:** the native readings of §3.2 and §4 (`UpdateBounds`, the fluid vertex layout, the SpeedTree
  loader); they stand as the first pass labelled them.

## 8. Reproduction

```bash
COOKED="<install>/A Story About My Uncle.app/Contents/Resources/ASAMU/CookedMac"
asamu-inspect objects "$COOKED/Maps/TheCore.asamu" --class InstancedStaticMeshComponent
asamu-inspect props   "$COOKED/Maps/AG-ParadiseCave.asamu" TheWorld.PersistentLevel.FluidSurfaceActor_0.FluidSurfaceComponent_1
asamu-inspect props   "$COOKED/Maps/AG-Darkcave.asamu" TheWorld.PersistentLevel.SpeedTreeActor_0.SpeedTreeComponent_0
asamu-import --out <dir> levels --map TheCore --map AG-ParadiseCave --map AG-Darkcave   # foliage block
cargo test -p asamu-import --bin asamu-import levels          # incl. foliage_matches_the_original_data (install)
cargo test --release -p asamu-import --bin asamu-import -- --ignored every_map
# native (local only): AInstancedFoliageActor::Serialize (0x1001d6e20), the FFoliageMeshInfo / cluster / instance
#   stream operators (0x1001d61a0, 0x1001d60e0, 0x1001d5c70), USpeedTree::Serialize (0x100617af0),
#   UFluidSurfaceComponent::UpdateBounds (0x1003a50d0), FFluidSimulation::UpdateBorderGeometry (0x1003aa650)
```
