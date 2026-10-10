# Matinee

Matinee is UE3's keyframe animation system. Kismet starts a `SeqAct_Interp` action, which plays an
`InterpData`: groups of tracks that move actors (lifts, rocks, airships, rotors, cameras), fire events back
into Kismet, cut cameras, fade the screen, play sounds and animations, and drive actor properties. This
page records how that data is stored in the shipped maps, how the original engine evaluates it, how our
decoder and evaluator reproduce it, and a census of every map. It publishes structure, counts, class
names and behaviour in our own words. Track contents, object names, actor references and sound or
animation names are original game data and stay local (see [Local outputs](#local-outputs)).

**Status**
- Every `InterpData`, group and track in the 12 maps decodes: 161 actions, 158 `InterpData`, 242 groups
  and 723 tracks, plus 3 `CameraAnim` assets. There are 0 decode failures, 0 warnings, 0 unmodelled track
  classes and 0 unresolved group links. CONFIRMED
- Curve evaluation follows the shipped executable's arithmetic, operation for operation. Recomputing the
  automatic tangents with our code reproduces the stored tangents bit for bit for 1,860 of 1,902 automatic
  keys; 40 more agree to within 1e-5. CONFIRMED
- Move tracks: all 262 relative move tracks bound to an actor start exactly at that actor's placement in
  the map, using our initial-transform and world-space code. This is a numerical round trip of our own
  transform chain (by construction the factored-out key transform cancels), not evidence about the
  engine. CONFIRMED (numerical self-consistency only)
- An independent re-check (own walk, own curve decoding, own tangent and curve evaluators; see
  [Verification](#verification)) reproduced every census number and the tangent results exactly.
- Run-time effects: move, event, director, fade, sound, visibility and toggle tracks, and (2026-10-10,
  camera-npc pass) animation-control, float/vector/colour property and skeletal-control strength tracks
  are played by the Kismet runtime; camera animations play on the player camera with the engine's pool
  and blending. Skeletal-control *scale* and particle-replay tracks are decoded only. See
  [Camera animations](#camera-animations), [Animation-control tracks](#animation-control-tracks),
  [Property and skeletal-control tracks](#property-and-skeletal-control-tracks) and
  [Runtime coverage](#runtime-coverage).

## Tooling and reproduction

- `crates/asamu-ue3/src/matinee.rs` contains the model, decoder, curve and move-track evaluation, playback
  stepping and coverage. Entry points are `matinee::extract_for(&PackageSet, &LoadedPackage)` and
  `matinee::extract(pkg, own_name, schema, class_defaults, kismet_graph)`.
- `tools/asamu-import/src/matinee.rs` provides `asamu-import matinee`, which writes per-map JSON to the
  user-local output folder.
- `crates/asamu-ue3/tests/matinee.rs` holds 39 synthetic tests: curves (hand-computed values for every
  mode and method), tangents (hand-computed), rotators, move tracks (including a restart from the stopped
  position), playback (input precedence, NaN steps), value decoding, a byte-built map with a prefab
  instance and a camera animation, and hostile inputs (cycles, truncation and corruption, capped notes,
  and a shared-group fan-out that hits the per-map budget).
- `crates/asamu-ue3/tests/matinee_real_data.rs` holds 5 gated tests: census, links, tangent
  reproduction, relative-track start positions, and every shipped curve against an independent
  double-precision evaluator plus the data facts the runtime rules rely on. They skip when the game data
  is absent.
- Hostile input: walks are bounded by visited sets, depth limits and per-map budgets (65,536 track
  decodes, 16,384 group decodes, 4,194,304 list items, 16,777,216 values copied from archetypes; the
  shipped maps use 723, 242, a few thousand, and on the order of 10,000, all in AG-StarHaven). Lookups
  by path, group name and archetype are hashed. Notes are capped at 64 per track or `InterpData` and 512
  per map.

```sh
export CARGO_TARGET_DIR=target/agents
cargo test -p asamu-ue3 --test matinee --test matinee_real_data -- --nocapture
cargo test -p asamu-import matinee
# user-local JSON (default output: <user data dir>/asamu-decomp/converted/matinee)
cargo run -p asamu-import -- matinee [--map AG-IceCave] [--pretty] [--force] [--out DIR]
```

The engine's native evaluators were read in the unstripped Mac executable. Ghidra headless decompilation
went to a local, ignored folder (`DecompileToLocal.java`, see `tools/ghidra-scripts/README.md`), and
`objdump -d` was used for the exact floating-point operation order and constants. The functions read
were:

- `FInterpCurve<float|FVector>::Eval` and `AutoSetTangents`, `FClampFloatTangent`, and
  `ComputeClampableFloatVectorCurveTangent<FVector>`.
- `UInterpTrackMove::{EvalPositionAtTime, EvalRotationAtTime, GetKeyTransformAtTime, GetLocationAtTime,
  ComputeWorldSpaceKeyTransform, GetMoveRefFrame, GetKeyframePosition}`,
  `UInterpTrackMoveAxis::EvalValueAtTime`, `UInterpTrackInstMove::CalcInitialTransform`.
- `FRotator::{MakeFromEuler, Euler, GetWindingAndRemainder}`, `FMatrix::Rotator`, `GetCleanedUpRotator`,
  `FQuat::MakeFromEuler`, `SlerpQuat`, `FRotator(const FQuat&)`.
- `UInterpTrackEvent::UpdateTrack`, `UInterpTrackDirector::{GetKeyframeIndex, GetViewedGroupName}`,
  `UInterpTrackFade::GetFadeAmountAtTime`, `UInterpTrackSlomo::GetSlomoFactorAtTime`.
- `USeqAct_Interp::{Activated, UpdateOp, Play, Reverse, ChangeDirection, StepInterp, InitInterp,
  FindGroupLinkedVariable}`.

Property and enum declarations come from the `Engine.u` and `Core.u` class models
(`asamu-inspect class`). Class defaults come from `asamu-inspect defaults --inherited`.

## How Matinee is stored in a cooked map (v868)

| Fact | Evidence | Confidence |
|---|---|---|
| `InterpData`, every `InterpGroup*` and every `InterpTrack*` store nothing but tagged properties. There is no native tail. | All Matinee exports of the 12 maps decode, and the generic decoder consumes them exactly (`asamu-inspect props`). | CONFIRMED |
| `InterpData` is a Kismet variable (`SequenceVariable`). A `SeqAct_Interp` reaches it through a variable link labelled `Data`. The action's own `InterpData` property is not stored; the engine finds the data at run time. | 161 actions, each with exactly one Matinee edge in the Kismet graph ([KISMET.md](KISMET.md)). | CONFIRMED |
| Groups are listed in `InterpData.InterpGroups` and tracks in `InterpGroup.InterpTracks`. Split movement uses `InterpTrack.SubTracks`: a move track whose six `InterpTrackMoveAxis` sub-tracks hold X/Y/Z translation and roll/pitch/yaw. | 59 split move tracks with 6 sub-tracks each. In every one, sub-track *i* has `MoveAxis` *i*. | CONFIRMED |
| Curves are `InterpCurveFloat/Vector/LinearColor` structs `{Points[], InterpMethod}`. A point is a tagged struct `{InVal, OutVal, ArriveTangent, LeaveTangent, InterpMode}`; vector members are binary `Vector`s. Shipped points store every member. `InterpMethod` is never stored, so every curve uses the struct default `IMT_UseFixedTangentEvalAndNewAutoTangents`. | Decoded value tree. 635 curves with keys, all with the default method. | CONFIRMED |
| Enumerator orders: `EInterpCurveMode` = Linear, CurveAuto, Constant, CurveUser, CurveBreak, CurveAutoClamped. `EInterpMethodType` = FixedTangentEvalAndNewAutoTangents, FixedTangentEval, BrokenTangentEval. `EInterpTrackMoveFrame` = World, RelativeToInitial. `EInterpTrackMoveRotMode` = Keyframed, LookAtGroup, Ignore. `EInterpMoveAxis` = TranslationX..Z, RotationX..Z. | `Core.u` and `Engine.u` class models. | CONFIRMED |
| Group and track *instances* (`InterpGroupInst*`, `InterpTrackInst*`) are never stored. The engine creates them when the action initialises. | 0 instance exports in all maps. `USeqAct_Interp::InitInterp` constructs them. | CONFIRMED |
| Unstored values come from the archetype when the export has one (prefab instances), else from the class defaults. Inherited references into a prefab archetype are remapped onto the instance's own subobjects, found through the export table's archetype field inside the same instance tree. | AG-StarHaven: 214 Matinee objects merge an archetype, with 0 references left unmapped. A synthetic prefab test covers the remap. | CONFIRMED |
| `CameraAnim` assets (camera shakes and canned camera moves) hold a single `InterpGroupCamera` whose tracks are ordinary Matinee tracks. They belong to no `InterpData`. | AG-Darkcave has 2 and AG-StarHaven has 1; their groups were the only unreachable groups before they were decoded as camera animations. | CONFIRMED |
| One track export is unreachable: a move track in an AG-StarHaven prefab archetype group that the group no longer lists. It is an editor leftover. | The census counts 724 track exports, 723 of them reached. | CONFIRMED |
| `InterpCurveEdSetup` (curve-editor layout) is editor-only and ignored. The same class also appears under particle systems. | Class purpose; never read at run time. | STRONG |
| No track stores `CurveTension` (the class default is 0), so automatic tangents of float and vector-base tracks use tension 0. The decoder does not keep `CurveTension`; it matters only when keys are edited. | Raw properties of all 723 tracks. | CONFIRMED |
| `UpgradeInterpMethod` (editor) turns automatic keys of a curve with a non-default `InterpMethod` into `CurveUser` keys and resets the method. No shipped curve has a non-default method, so it changes nothing. `UInterpTrackMove::PostLoad` does not call it. | Decompiled functions. | STRONG |

## Decoded model and JSON schema

`asamu-import matinee` writes `<out>/matinee/<map>.matinee.json` for every map that has Matinee data,
plus `manifest.json`, which holds per-map coverage counts. The JSON has `format` = `asamu-matinee` and
`version` = 1.

```text
MatineeMap { format, version, package, actions[], interp_data[], camera_anims[], coverage, orphans[], warnings[] }
MatineeAction { path, export_index, node (Kismet node id), class, scope (level|prefab|detached),
                parent_sequence, comment, interp_data, settings, inputs[], outputs[],
                bindings[] { link, label, group, group_kind, targets[] { variable, variable_class, named,
                                                                         object, object_class } },
                property_links[] { link, label, property, variables[] { variable, variable_class, value } } }
InterpSettings { play_rate, looping, rewind_on_play, no_reset_on_rewind, rewind_if_already_playing,
                 force_start_pos, force_start_position, client_side_only, skip_update_if_not_visible,
                 is_skippable, disable_radio_filter, interp_for_path_building, camera_flags[],
                 preferred_split_screen, constant_camera_anim, constant_camera_anim_rate,
                 stored[] (settings the designer changed) }
InterpDataInfo { path, export_index, scope, length, path_build_time, ed_section[2], bake_and_prune,
                 groups[], used_by[] }
InterpGroup { path, export_index, class, kind (group|director|ai|camera), name, color[RGBA], folder,
              parented, anim_sets[], ai?, tracks[] }
Track { path, export_index, class, title, disabled, active_condition?, data { type, ... }, sub_tracks[] }
CameraAnimInfo { path, export_index, length, base_fov, group? }
curve  { points[] { in, out, arrive, leave, mode }, method }   (out/arrive/leave: number or [x,y,z] or [r,g,b,a])
```

Track `data.type` values and their contents:

- `move`: `pos`, `euler` (degrees: X roll, Y pitch, Z yaw), `lookup[] {time, group?}`, `move_frame`,
  `rot_mode`, `look_at_group`, `lin_curve_tension`, `ang_curve_tension`, `use_quat_interpolation`,
  `disable_movement`, `use_raw_actor_tm`, `axes[] {path, axis, curve, lookup}`.
- `event`: `keys[] {time, name}` and the three firing flags.
- `director`: `cuts[] {time, transition_time, target_group, shot}`.
- `sound`: `keys[] {time, volume, pitch, sound}`, a vector `curve`, and the four flags.
- `float_property`, `vector_property`, `color_property`, `linear_color_property`: `{name, curve}`.
- `fade`: `{curve, persist_fade}`. `slomo`, `color_scale`, `audio_master`: `{curve}`.
- `skel_control_strength`, `skel_control_scale`, `float_particle_param`, `morph_weight`: `{name, curve}`.
- `float_material_param`, `vector_material_param`: `{param, materials[], curve}`.
- `anim_control`: `{slot, anim_sets[], keys[] {start_time, sequence, start_offset, end_offset,
  play_rate, looping, reverse}, weight, root_motion, skip_notifiers}`.
- `toggle`, `head_tracking`: `keys[] {time, action}`. `visibility`: `keys[] {time, action, condition}`.
- `particle_replay`: `keys[] {time, duration, clip}`. `notify`: `{parent_node, keys[] {time, notify}}`.
- Fallbacks: `float_base`, `vector_base` and `linear_color_base` for unknown subclasses of the curve
  bases, and `unknown` (property names only).

Values that are not stored take the merged class defaults (for example `PlayRate` 1,
`ConstantCameraAnimRate` 4 and the event-firing flags). For curve points they take the UnrealScript struct
defaults: zeros, `CIM_Linear`, and alpha 1 for `LinearColor`. An unknown curve mode decodes as
`CurveUser` with a warning; the native evaluator treats every mode other than linear and constant as
cubic.

## Curve evaluation (CONFIRMED from the disassembly)

Our `InterpCurve::eval(t, default)` reproduces the engine in this order:

1. No keys: return the default. Our tracks use zero, as the engine does.
2. One key, or `t` at or before the first key's `InVal`: return the first key's value.
3. `t` not before the last key's `InVal`, including a NaN `t`: return the last key's value.
4. Otherwise, scan from the second key for the first key with `InVal > t`. It closes the segment
   `[k0, k1]`, and the **start key's** mode governs the segment.
   - The span is `d = in1 − in0`. If `d` is not positive, or `k0` is `Constant`, return `out0`.
   - Otherwise `a = (t − in0) / d`.
   - `Linear`: `out0 + a·(out1 − out0)`.
   - Every other mode is a cubic Hermite segment. The tangents are `T0 = leave0 · d` and
     `T1 = arrive1 · d`; they are not scaled by `d` only under `IMT_UseBrokenTangentEval`, which no
     shipped curve uses. With `a2 = a·a` and `a3 = a·a2`, the result is
     `((h00·out0 + h10·T0) + h11·T1) + h01·out1`, where
     `h00 = (2a3 − 3a2) + 1`, `h10 = (a3 − 2a2) + a`, `h11 = a3 − a2` and `h01 = 3a2 − 2a3`.
     The grouping shown is the executable's; it matters for bit-exact `f32` results.
   - Vector and colour curves apply the same arithmetic to each component.

So at run time `CurveAuto`, `CurveAutoClamped`, `CurveUser` and `CurveBreak` behave identically: each
uses its stored tangents. They differ only in how the editor computed those tangents. The move-track
evaluators (`EvalPositionAtTime`, `EvalRotationAtTime` and `EvalValueAtTime`) repeat this formula
inline with the same operation order, so a move track without lookup keys evaluates exactly as its curves
do.

### Automatic tangents (CONFIRMED, `AutoSetTangents`)

The editor recomputes the tangents of `CurveAuto` and `CurveAutoClamped` keys whenever keys change.
Ours (`InterpCurve::auto_set_tangents(tension)`, float and vector curves) applies these rules:

- **First key:** an automatic key, or a curve's only key, gets a zero leave tangent. Its arrive tangent
  is kept.
- **Last key:** an automatic key gets a zero arrive tangent.
- **Inner automatic key with a cubic previous key:** arrive and leave get the same slope.
  - Default method, `CurveAutoClamped`: the clamped tangent below, times `1 − tension`.
  - Default method, `CurveAuto`: `((v − vp) + (vn − v)) · (1 − tension)` over the span
    `max(1e-4, tn − tp)`. The span maximum is taken in double precision. Float curves divide by the span;
    vector curves multiply by its reciprocal, which rounds differently.
  - Older methods: `((v − vp) + (vn − v)) · 0.5 · (1 − tension)`.
- **Inner automatic key when its own or the previous key's mode is `Constant`:** both tangents become
  zero. When the previous key is `Linear` and neither key is `Constant`, the tangents are left alone.

**Clamped tangent (`FClampFloatTangent`).**
- A key that is a local maximum or minimum of its neighbours gets a flat tangent. Ties count as extremes.
- Otherwise the tangent starts from the average slope `(vn − vp) / (tn − tp)`. Each time span is floored at
  `1e-4`.
- Let `h = (v − vp) / (vn − vp)` be the key's height between its neighbours.
  - Below 0.333, the tangent is blended towards the slope from the previous key with weight
    `1 − h/0.333`.
  - Above 0.667, it is blended towards the slope to the next key with weight `(h − 0.667)/0.333`.
- After each blend the result is clamped: never above the average slope on a rising stretch and never
  below it on a falling one.
- Vector curves apply this per component.

**Verification against the shipped data.** Recomputing every automatic key in all maps with tension 0 for
float curves, and the track's `LinCurveTension` or `AngCurveTension` for vector curves, gives these
results:

| Keys | Bit-exact | Within 1e-5 | Off |
|---|---|---|---|
| Float, 1,028 | 1,023 | 3 | 2 |
| Vector, 874 | 837 | 37 | 0 |

All 40 near-misses are `CurveAutoClamped` keys: 36 differ by at most 16 units in the last place (ulps),
and the other 4 by at most 8e-6 absolute in a small component, where the slope arithmetic cancels. The 2
"off" keys are clamped keys at the same time, about t = 106 s, with relative differences of 2.5e-4 and
1.3e-3. Our explanation is that the editor build that baked them (Windows, another compiler) rounded
differently, or that neighbouring keys moved afterwards. TENTATIVE

**Evaluation against an independent evaluator.** Every shipped curve, sampled at five points per segment
and outside its key range (18,924 component samples), matches a separately written double-precision
Hermite, linear and constant evaluator to within 6e-8 of the curve's own scale. No shipped segment starts
at a `Constant` key (all 10 `Constant` keys end their curve), so constant segments are covered only by
synthetic tests. CONFIRMED

## Rotations

| Rule | Confidence |
|---|---|
| Euler vectors are degrees `(X roll, Y pitch, Z yaw)`. `MakeFromEuler` yields rotator `[pitch, yaw, roll]` with each component `trunc(deg × 182.04445)`, truncating toward zero. `Euler()` multiplies each unit by 360/65536. | CONFIRMED (constants read from the executable) |
| Rotator to matrix uses the 16384-entry sine table (index `(angle >> 2) & 0x3FFF`; the cosine is read a quarter turn later). Angles are therefore effectively quantized to 4 units. | CONFIRMED indexing. TENTATIVE table contents: assumed `sin(2πi/16384)`; it is filled at run time. |
| Matrix to rotator (`FMatrix::Rotator`) gives pitch = `atan2(M02, √(M00² + M01²))` and yaw = `atan2(M01, M00)`. Roll is measured against the Y axis rebuilt from that pitch and yaw. Each angle is multiplied by 32768 in `f32`, divided by π in double precision, then rounded and truncated. `GetCleanedUpRotator` additionally flushes `atan2` inputs below 1e-5 to zero. | CONFIRMED |
| Winding split: the remainder is each component normalized to `[-32768, 32767]`, and the winding is `r − remainder`. | CONFIRMED |
| `FQuat::MakeFromEuler` builds the quaternion of the table-based rotation matrix of `MakeFromEuler(e)` using the trace method. `SlerpQuat` takes the dot product summed x, y, z, w in order and treats `|c| ≥ 0.9999` as linear. Otherwise it uses `sin`-weighted blending with `ω = acos(min(|c|, 1))`, negates the second weight when `c < 0` (the shorter path), and does not normalize. `FRotator(FQuat)` is `FMatrix::Rotator` of the quaternion's matrix. | CONFIRMED structure and constants. TENTATIVE that Rust's `sin`/`acos`/`atan2` match the C library bit for bit. |

## Move tracks

**Relative transform (`GetKeyTransformAtTime`).**
- Position: when the track has sub-tracks, sub-tracks 0, 1 and 2 give X, Y and Z, chosen by position
  rather than by their `MoveAxis`. Otherwise the position comes from `PosTrack`.
- Rotation, Euler path (the default): sub-tracks 3, 4 and 5 or `EulerTrack` give an Euler vector, which
  becomes a rotator through `MakeFromEuler`.
- Rotation, quaternion path: used only when `bUseQuatInterpolation` is set and the track has no
  sub-tracks.
  - Find the two Euler keys around `t`, with alpha clamped to `[0, 1]`. Key modes are ignored, so even
    `Constant` keys slerp.
  - Slerp their quaternions and convert the result back to a rotator.
  - Outside the key range the first or last key applies. Without keys the result is the identity.
  - One shipped track (AG-Workshop) uses this path.
  - CONFIRMED

**Lookup keys.**
- A move key whose `LookupTrack` entry names a group takes the location, or the rotation as Euler, of
  that group's actor. A controller is replaced by its pawn.
- Neighbouring tangents are rebuilt Catmull-Rom style with `LinCurveTension`/`AngCurveTension`; the first
  and last such keys get zero tangents.
- Structure: STRONG. Exact operation order: TENTATIVE.
- No shipped key names a group: every lookup entry is `None`, and every non-empty lookup track has one
  entry per key.

**Initial transform (`CalcInitialTransform`, for an actor that is not an AI pawn).**
- `InitialTM` is the actor's rotation-and-translation matrix, made relative to its base when the actor is
  attached.
- Unless `bUseRawActorTMforRelativeToInitial` is set (never in the data), the inverse of the track's own
  key transform at a time `T` comes first. A relative track therefore leaves its actor exactly where it
  stands at `T`, even when that key is not the identity (24 of the 163 relative tracks have a non-identity
  key at time 0).
- `T` depends on the caller, through the function's flag argument. CONFIRMED from the disassembly:
  - When the instances are built (`InitInterp` → `InitTrackInst`, flag clear), `T` is the owning action's
    current `Position`. `Activated` rebuilds the instances every time a stopped action is started by Play,
    Reverse or Change Dir, so an action restarted where it stopped (a lift sent back down with Reverse at
    its end position) moves its actor along the same path. Assuming time 0 here would shift the whole path
    by the motion already played.
  - When relative tracks are re-based for `bNoResetOnRewind` (Play's rewind and the forwards loop wrap,
    flag set), `T` is 0.
  - No shipped action stores `Position`, so at level start `T` is always 0.
- Scale is removed by normalizing the rows.
- The engine inverts with the general `FMatrix::Inverse` and sums each matrix product from the last column
  down. We use a rigid inverse (transpose) and the natural order, which can differ in the last bits.
  TENTATIVE that this never changes a whole rotator unit.
- STRONG overall (decompiled structure plus the disassembled flag branch).
- Ours: `MoveInstance::new(track, location, rotation, position, actors)` and `MoveInstance::with_base`;
  the caller passes the action's position when the action (re)initialises and 0 for a re-base.

**Reference frame (`GetMoveRefFrame`).**
- `IMF_World`: the base actor's matrix, or the identity when the actor is unattached.
- `IMF_RelativeToInitial`: `InitialTM · base`, with the rows normalized.
- CONFIRMED structure. TENTATIVE: `GetBaseMatrix` carries no scale.
- **Correction (2026-10-10):** the engine reads the base actor's matrix again on every evaluation (the base may
  itself be moving, e.g. a passenger on an airship), whereas `MoveInstance` stores the base matrix once when the
  instance is built (`MoveInstance::with_base` in `asamu_ue3`, `instance_with_base` in `asamu_kismet`). An earlier
  note here implied the stored base was used as is. The Kismet runtime replaces the stored base with the base
  actor's current transform before each evaluation (`asamu_kismet` `interp.rs`, `with_current_base`), which matches
  the engine. STRONG (verifier's reading of `GetMoveRefFrame` / `GetBaseMatrix`; consistent with the passenger
  checks in KISMET_RUNTIME.md §11).

**World transform (`ComputeWorldSpaceKeyTransform`).** CONFIRMED structure; TENTATIVE operation order in
the matrix products.
1. The position is transformed by the frame.
2. The rotation is split into whole turns and a remainder.
3. The remainder's matrix is transformed by the frame and converted back with the cleaned rotator, then
   normalized.
4. The whole turns, as Euler degrees divided by 360, are rotated by the frame, rounded to whole numbers,
   multiplied back by 360 and added through `MakeFromEuler`.

This keeps multi-turn spins intact through the frame change: rotors, wheels, and an Euler key of 720° stay
two full turns.

**Output (`GetLocationAtTime`).**
- A track with neither sub-tracks nor `EulerTrack` keys does nothing.
- `IMR_Ignore` keeps the actor's rotation.
- `IMR_LookAtGroup` faces the named group's actor. This is unused in the data, and our direction-to-rotator
  conversion for it is TENTATIVE.

**Round trip on real data.** For every level action and every bound actor with an active
`IMF_RelativeToInitial` move track, we compute the instance from the actor's stored `Location`/`Rotation`
(and its base actor's, for the 11 attached ones) at position 0 and sample at time 0. The result
reproduces the actor's placement: location within `max(0.05, 4e-7·|x|)` units, the float precision at
coordinates up to about 511,000, and rotation within the 4-unit table step. This holds for 262 of 262
tracks. Because the initial transform factors out the very key transform we sample, this only shows that
our matrix, winding and table code round-trips numerically at real coordinates; it says nothing about the
engine. CONFIRMED (numerical self-consistency only)

## Other tracks (semantics ported)

| Track | Rule | Confidence |
|---|---|---|
| Event | The track's instance remembers the last position. An update runs backwards when the action is playing in reverse, or for a jump to an earlier position while the action is stopped. (A jump to an earlier position while playing forwards counts as forwards with an empty window, so it fires nothing either.) Forwards it fires keys with `last ≤ time < new`, widening the window by 1e-4 (the float `1e-4`) when `new` equals the length. Backwards it fires keys with `new < time ≤ last`, widening by 1e-4 when `new` is 0. These obey `bFireEventsWhenForwards` and `bFireEventsWhenBackwards`. Jumps fire only forwards, with both `bFireEventsWhenJumpingForwards` and `bFireEventsWhenForwards`. So the wrap of a reverse-looping action, a jump from 0 to the end while playing in reverse, fires nothing. Our first port treated that jump as forwards; it is fixed. No shipped event track sets `bFireEventsWhenJumpingForwards` (the class default is false), so the shipped maps were not affected. A fired key activates the action's output link of the same name. | STRONG (decompiled and disassembled; the mapping of the three flag bits to names follows the declaration order and the class defaults) |
| Director | There is no cut before or at the first cut's time. After that, the cut in effect is the last cut whose time is at or before `t`, so the first cut needs `t` strictly after it. With no cut in effect, the viewed group is the director group itself, meaning the player's camera. `TransitionTime` is the blend length. | CONFIRMED |
| Fade | `clamp(curve(t), 0, 1)`, with curve(t) = 0 without keys. | CONFIRMED |
| Slomo | `max(curve(t), 0.1)`. | CONFIRMED |

## Camera animations

`CameraAnim` assets are played on the player's camera by script (`Camera.PlayCameraAnim`) and by
`SeqAct_PlayCameraAnim`. Ours: `asamu_kismet::camera_anim` (data, pool, blending, application, the gameplay
script's calls), owned by the level script (`asamu_game::LevelScript::camera_anims`), applied to the render
camera by `apps/asamu` `kismet::apply_camera_anims`.

**Where they are (CONFIRMED, export census).** `Startup.upk` holds 25 `CameraAnim` assets: 11 in
`ASAMUCameraAnimations` (grapple begin and loop, normal and hard landing, power-jump and power-leap bobs,
rocket-boots begin and boosting, three parkour-mode ones), 6 in `Zeth_CameraStuffs` (power-jump charge and
keep-charging, sprint, two landing ones, the worm's growl) and 8 stock UDK ones. The maps add 3 (AG-Darkcave 2,
AG-StarHaven 1), which their `SeqAct_PlayCameraAnim` actions name. `asamu-import matinee` writes the
`Startup.upk` ones to `matinee/camera_anims.json` (same shape as a map file's `camera_anims`).

**What they contain (CONFIRMED, decoded).** A camera group with at most one move track, float property tracks
on `FOVAngle` or on post-process settings (`CamOverridePostProcess.*`) and vector property tracks on
post-process settings, plus the asset's own `BasePPSettings`. The two grapple animations and the normal-landing
one have **no tracks at all**: they move nothing (the grapple ones carry post-process settings only; the
landing one is an empty asset with the class-default `AnimLength` 3). The hard landing, the bobs, the boots
animations and the growl have move tracks.

**Engine rules** (CONFIRMED unless noted; locally decompiled `ACamera::{PlayCameraAnim, AllocCameraAnimInst,
ReleaseCameraAnimInst, StopCameraAnim, StopAllCameraAnims, ApplyCameraModifiers, InitTempCameraActor,
ApplyAnimToCamera}`, `UCameraAnimInst::{Play, AdvanceAnim, Stop, Update}`, `USeqAct_PlayCameraAnim::Activated`;
class constants and defaults from `Engine.u`). The verification pass (2026-10-10) re-read these natives on its
own and found the port in agreement on every rule below; the two float constants the blend code compares and
defaults against were read from the executable (the weight threshold of `ApplyCameraModifiers` is 0.0, the
unblended weight 1.0), and each rule is pinned by a unit test in `asamu_kismet::camera_anim`:

| Rule | Evidence |
|---|---|
| The camera owns a pool of `MAX_ACTIVE_CAMERA_ANIMS` = 8 instances. `PlayCameraAnim` takes the last free one and returns it; with none free it returns nothing and the animation does not play. Script keeps the returned object, so a kept reference to an instance that finished and was handed out again acts on the new animation. | class constant; `AllocCameraAnimInst` |
| `PlayCameraAnim(Anim, Rate = 1, Scale = 1, BlendInTime, BlendOutTime, bLoop, bRandomStartTime, Duration, bSingleInstance)`: the two defaults are bytecode default-parameter values. `bSingleInstance` re-uses a running instance of the same animation (`Update`) instead of allocating. | bytecode of `Camera.PlayCameraAnim`; native |
| `Play`: time 0 (or `frand · AnimLength`), blend timers 0, **blending in always set**, not blending out, not finished; `RemainingTime = Duration − BlendOutTime` for a positive duration; the animated camera actor is reset to the origin; the group instance is initialised and its first move track kept. | `UCameraAnimInst::Play` |
| `AdvanceAnim(dt)`: time += `dt · PlayRate`, blend timers += `dt`. Not looping: past `AnimLength` the animation finishes; otherwise, within `BlendOutTime` of the end it (re)starts blending out with the timer at the overshoot. Looping: past the length, the length is subtracted once. Blending in ends once its timer is strictly above `BlendInTime` (so a zero blend-in ends on the first update); a blend-out timer above `BlendOutTime` is clamped and finishes the animation. Weight = `min(t_in / BlendInTime or 1, 1 − t_out / BlendOutTime or 1) · BasePlayScale · TransientScaleModifier`. Then the group is evaluated at the new time. A finished animation is terminated; a running `RemainingTime` counts down and starts the blend-out (or ends the animation when `BlendOutTime` is 0). | `UCameraAnimInst::AdvanceAnim` |
| `Stop(immediate)`: immediately, or with `BlendOutTime ≤ 0`, the animation is terminated; otherwise it starts blending out from 0. `StopCameraAnim` and `StopAllCameraAnims(ByType)` do the same per instance. | natives |
| Per camera update (`ApplyCameraModifiers`, after the camera modifiers): for every active instance in pool order — the animated camera actor is reset (origin, zero rotation, `FOVAngle` = the animation's `BaseFOV`), the instance advances, and when its weight is above 0 it is applied to the point of view; a finished auto-release instance returns to the pool; the transient scale is reset to 1. An animation that finishes in this update was evaluated and weighted before it was terminated, so it still contributes this one frame (at full weight when it ran out without a blend-out, not at all when its blend-out completed). | `ApplyCameraModifiers`, `AdvanceAnim` |
| Application (`CAPS_CameraLocal`, the default play space and the only one the shipped calls use), weight `s`: location += (animated location × `s`) rotated by the view rotation; rotation = `rotator(R(animated rotation × s, truncated to whole units) · R(view))`; FOV += `s · (animated FOVAngle − 90)`, 90 being the `CameraActor` class default. So an animation whose `BaseFOV` is not 90 shifts the FOV even without an FOV track. | `ApplyAnimToCamera`; `CameraActor` defaults |
| The move track's initial transform is taken at time 0: `CalcInitialTransform` uses the owning `SeqAct_Interp`'s position only when the group instance's outer is one, which it is not here. With the camera actor at the origin, the animated offset is the track's key transform relative to its first key. | disassembly of `CalcInitialTransform` (the `Cast<USeqAct_Interp>` result is tested before `Position` is read) |
| `SeqAct_PlayCameraAnim`: "Play" wins over "Stop". Play calls `PlayCameraAnim(CameraAnim, Rate, IntensityScale, BlendInTime, BlendOutTime, bLoop, bRandomStartTime)` on each target's camera; Stop calls `StopAllCameraAnimsByType(CameraAnim, false)`. Nothing happens without an animation, and nothing without a target that is a player controller or a pawn with one (all 5 shipped actions target the player). | `USeqAct_PlayCameraAnim::Activated`; map census |

**The gameplay script's calls** (CONFIRMED (src): local reading of the `asamu` classes; rate 1, default scale,
no blend, not looping unless noted):

| When | Call |
|---|---|
| every grapple attach (`GrappleGun.Grapple`) | `GrappleBegin` once; `GrappleLoop` looping, its instance kept |
| every release (`ReleaseGrappleButton`, the common release) | stop the kept loop instance (no blend-out time, so it ends at once) |
| `ASAMUPawn.Landed`, normal handler, floor not `NotLandable` | `HardLanding` when `V.z < −hardLandingThreshold`, else `NormalLand`; the story-state handler plays nothing |
| power-jump `Charging` begin code | `PowerJumpChargeCameraAnim` with 0.5 s blend in and out, instance kept |
| power-jump `Canceled` begin code | stop the kept charge instance (blends out over 0.5 s) |
| power jump / power leap | `PowerJumpBob` / `PowerLeapBob` (parkour mode, off by default, would play the flip animations instead) |
| rocket boots: charge begins / boost begins | `RocketBootsBegin` / `RocketBootsBoosting`, instances kept |
| rocket boots: boost runs out / `ResetBoots` (a landing cancels the boost; the pawn's death reset `ResetPlayer` calls it too whenever the boots are enabled) | stop the boosting instance / stop the boosting and charge instances |
| worm starts / stops screaming (`StartCameraShake` / `StopCameraShake`) | `MonsterGrowl` looping, instance kept / stopped (twice: `StopCameraAnim`, then the instance's own `Stop`) |
| sprint | the sprint animation is never started: its start requires an instance that only that start creates (ABILITIES.md) |

Consequences (STRONG, from the rules and the data): the grapple and normal-landing animations have no visible
camera motion; a normal landing still occupies a pool instance for its 3 s, the grapple-begin one for its
0.25 s and the grapple loop for as long as the grapple lasts, so eight of them alive at once (in practice:
landings less than 0.4 s apart) exhaust the 8 instances, after which further animations (a hard landing's, the
boots') silently do not play until instances free up. Our pool reproduces this (unit test
`normal_landings_can_exhaust_the_pool`); how often the original gets there is unmeasured. The kept instance
references are another quirk the pool reproduces: the boots', the charge's and the growl's references outlive
their animations, so a later stop through one of them (every death reset with the boots enabled, every scream
end) ends whatever animation has been handed that instance since.

Ours: `LevelScript::tick` turns the tick's events into these calls after the game tick (the death reset,
attach, release, landing handler, power-jump state code and events, boots events, the worm's shake event),
routes Kismet's `CameraAnim` outputs in the same frame as the update, and advances the pool by the fixed tick.
Deviations and limits: the order of several calls within one tick is ours (the death reset's boots stop,
release before attach, then power jump, boots, landing, worm; TENTATIVE); the post-process settings and tracks of the animations are not applied; play spaces
other than camera-local are not implemented (unused); the animations do not yet bias the player's aim
(GRAPPLE.md G-TG-1: the original's fire trace uses the camera's point of view, which includes them) — the
simulation would have to read `LevelScript::camera_anims().apply(...)`; random start times use our own seed;
non-finite play parameters fall back to the defaults and a sample that would make the point of view non-finite
is skipped (ours; the engine takes what it is given, and no shipped call passes such values). The pool
advances with the 60 Hz simulation tick while the app applies the latest samples every rendered frame, so at
higher frame rates the offsets move in tick-sized steps. Parity with the running game is unmeasured.

## Animation-control tracks

`InterpTrackAnimControl` plays skeletal animations on a group's actor through the actor event
`SetAnimPosition(SlotName, ChannelIndex, AnimSeqName, Position, bFireNotifies, bLooping,
bEnableRootMotion)`. Ours: `asamu_kismet::anim` (a port of the locally decompiled
`UInterpTrackAnimControl::{GetAnimForTime, UpdateTrack, CalcChannelIndex}`), called from the interpreter's
Matinee update; the host hands each call to the NPC system's slot node (`asamu_world::anim::SequenceNode`).

| Rule | Confidence |
|---|---|
| A key is `(StartTime, AnimSeqName, AnimStartOffset, AnimEndOffset, AnimPlayRate, bLooping, bReverse)`. The key in effect at `t` is the last one starting at or before `t`; before the first key, the first key's sequence at its start offset. | CONFIRMED |
| Position in a key: `(t − StartTime) · AnimPlayRate`. Looping keys wrap it with `fmod` into `SequenceLength − AnimStartOffset − AnimEndOffset` (at least 0.01) and add the start offset. Other keys add the start offset and clamp to `[0, SequenceLength − AnimEndOffset + 1e-4]`. Reversed keys mirror the position inside the offsets. A sequence missing from the group's anim sets keeps the raw position. | CONFIRMED (constants 1e-4 float, 0.01 float read from the executable) |
| An update without keys, a jump, or one that does not advance calls `SetAnimPosition` once for the key at the new time with notifies off. | CONFIRMED |
| A forwards update walks every key from the one at the last position to the one at the new position: each key is played up to the next key's start (or the new position) with notifies on — unless `bSkipAnimNotifiers`, or the position equals the start offset; a looping key first plays to `SequenceLength − AnimEndOffset + 1e-4` (double) with notifies on and restarts at its start offset with notifies off, once per wrap; then the next key's sequence is set at its start offset with notifies off. | CONFIRMED |
| The track instance's last position starts at the action's position when the instances are built. The channel index is the number of earlier, enabled animation-control tracks of the group with the same slot name. | CONFIRMED (`UInterpTrackInstAnimControl::InitTrackInst`, `CalcChannelIndex`) |
| A forwards update that starts before the first key makes one call for that stretch (the first key's sequence at its start offset, notifies off) and then plays the first key; the "next key" call is only made after a key that was actually played. | CONFIRMED (`UpdateTrack`; found by the verification pass, which removed a duplicate call) |
| `SkeletalMeshActorMAT.SetAnimPosition` (native `MAT_SetAnimPosition`) finds the slot node by name; the slot's sequence node switches sequence when the name differs (`SetAnim`, then `SetPosition(p, false)`), takes rate 1 and the looping flag, then `SetPosition(p, bFireNotifies)`. The node is not set playing, so it holds the pose Matinee last gave it; neither class resets anything when the track ends (`MAT_FinishAnimControl` only drops the group's anim sets). | CONFIRMED (`UAnimNodeSlot::MAT_SetAnimPosition`, `ASkeletalMeshActor::MAT_FinishAnimControl`) |
| A plain `SkeletalMeshActor` has no slots: its `SetAnimPosition` script event drives the component's own `AnimNodeSequence` (when the component's `Animations` is one; otherwise nothing happens) — `SetAnim` when the name differs, the looping flag, `SetPosition(p, bFireNotifies)`. `SetAnim` leaves the node's time alone, so a switch with notifies on issues the new sequence's notifies between the old time and `p`; rate and `bPlaying` are untouched, so a node that was playing keeps ticking between Matinee's updates. | CONFIRMED (src: the engine class, read locally; `UAnimNodeSequence::SetAnim` decompiled) |

Ours treats every slot of a `...MAT` actor as its one body animation (every shipped slot track uses the slot
`FullBody`), drives a plain `SkeletalMeshActor`'s own node as above (the two shipped tracks without a slot
name, both in AG-StarHaven, sit on such actors, and both have their own playing, non-looping node), ignores
the slot weight curve (TENTATIVE: full weight), resolves sequence lengths through the actor's converted mesh,
and makes at most 4,096 `SetAnimPosition` calls per track update (ours, against hostile data; a real update
makes a handful). Notifies: [NPCS.md](NPCS.md) "Animation notifies".

## Property and skeletal-control tracks

| Track | Rule | Confidence |
|---|---|---|
| `InterpTrackFloatProp`, `InterpTrackVectorProp` | The instance resolves `PropertyName` on the group's actor once; every update writes `curve.Eval(t, current value)`, so a curve without keys writes the current value back (no effect), then runs the property's update callback or re-attaches the actor's components. | CONFIRMED (`UpdateTrack`, `InitTrackInst`); how a name such as `Brightness` reaches a light actor's component (`AActor::GetInterpFloatPropertyRef`) was not read: TENTATIVE |
| `InterpTrackColorProp` | As above with a vector curve; the result is stored as an `FColor`: each component `255 · c^(1/2.2)` (float `0.4545454`), clamped to 0–255. | CONFIRMED |
| `InterpTrackSkelControlStrength` | Calls the actor's `SetSkelControlStrength(SkelControlName, curve(t))` with 0 as the value of an empty curve; `SkeletalMeshActorMAT` finds the control and sets its strength with blend time 0. | CONFIRMED (`UpdateTrack`, `MAT_SetSkelControlStrength`) |
| `InterpTrackSkelControlScale` | Decoded only (4 tracks, each one key of value 1 on look-at controls). | — |

What the shipped tracks drive (CONFIRMED, decoded data): `Brightness` of 3 `PointLightMovable`s in AG-Workshop
(and one in the front end) with keys; `Radius` tracks with keys in 3 AG-StarHaven groups that no action binds
to an actor, so they do nothing; every `LightColor` track (7) and both `FOVAngle` tracks on camera actors are
empty; `DrawScale3D` of 10 movers (StarHaven's winch ropes, a Workshop pad light); the strength of look-at
controls (17 tracks, 2 of them in AG-StarHaven without a control name, which do nothing; NPCS.md
"Look-at"). The verification pass recounted these from a fresh conversion with its own script and got the
same numbers.

Ours: the interpreter evaluates the curves each Matinee update and skips empty ones; values are kept per actor
(`Runtime::actor_properties`) for the renderer, which scales a light's intensity linearly with `Brightness`
and its range with `Radius` (our UE3 → physical light mapping, `asamu_assets::lighting`); `DrawScale3D` also
rescales the mover's collision through the host; strengths go to the NPC system. Not modelled: the camera
actors' `FOVAngle` (no keys anyway), any other property name (none in the data).

## Runtime coverage

`MatineeSet::track_coverage` (gated test `matinee_track_coverage` in `crates/asamu-kismet/tests/real_data.rs`)
counts, over the level-scope actions' data of all maps, the tracks of each class, those with keys, and those
with keys that act on something (a bound object, or a director group). CONFIRMED (converted data, 2026-10-10):

| Track class | Tracks | With keys | Effective | Runtime |
|---|---:|---:|---:|---|
| InterpTrackMove (split sub-tracks counted with their track) | 160 | 153 | 149 | gameplay: movers, moving collision |
| InterpTrackAnimControl | 49 | 49 | 49 | gameplay: skeletal animation and notifies |
| InterpTrackEvent | 35 | 34 | 34 | gameplay: Kismet outputs |
| InterpTrackSkelControlStrength | 17 | 17 | 17 | presentation: look-at strengths |
| InterpTrackSound | 17 | 14 | 14 | presentation |
| InterpTrackFloatProp | 16 | 7 | 4 | presentation: light brightness |
| InterpTrackDirector | 15 | 3 | 3 | presentation: cuts |
| InterpTrackFade | 14 | 14 | 14 | presentation |
| InterpTrackVectorProp | 10 | 10 | 10 | presentation (+ collision scale) |
| InterpTrackColorProp | 7 | 0 | 0 | presentation (no keys) |
| InterpTrackVisibility | 2 | 2 | 2 | presentation |
| InterpTrackToggle | 1 | 1 | 1 | presentation |
| InterpTrackSkelControlScale | 4 | 4 | 4 | not played |
| InterpTrackParticleReplay | 1 | 0 | 0 | not played (no keys) |

(The census table below counts every track export including prefab archetypes and the camera animations'
tracks; this table counts what level actions can play.)

## SeqAct_Interp

**Inputs.** The five inputs are always, in order, Play, Reverse, Stop, Pause and Change Dir.
- **Play:** if the action is not playing and `bForceStartPos` is set, jump to `ForceStartPosition`.
  Otherwise, if `bRewindOnPlay` is set and the action is not playing (or `bRewindIfAlreadyPlaying` is
  set), jump to 0. With `bNoResetOnRewind`, relative move tracks first re-base their initial transform on
  the actor's current placement (time 0, see above). Then play forwards, unpaused.
- **Reverse:** play backwards, unpaused.
- **Change Dir:** play, unpaused, in the opposite direction.
- **Pause:** toggles the pause, but only while playing. A stopped action ignores it.
- **Stop:** stops and clears the pause. The position is kept.
- **Precedence:** one input per update. While playing, Pause wins; otherwise Play, then Reverse, Stop and
  Change Dir.
- **Starting a stopped action:** a stopped action is deactivated at its next update. Only Play, Reverse
  or Change Dir activate it again, and activation first rebuilds the group and track instances
  (`InitInterp`) at the current position. An input in the very update after Stop reaches the still-active
  action through `UpdateOp` instead and rebuilds nothing.

STRONG (flag arithmetic decompiled for `Play`, `Reverse`, `ChangeDirection`, `UpdateOp` and `Activated`).
Ours: `Playback::apply_inputs` and `Playback::needs_init`.

**Stepping (`StepInterp`).**
- The action steps only while playing and not paused.
- The position moves by `dt · PlayRate` in the play direction.
- Past an end, a looping action first updates its tracks to that end, jumps to the other end and wraps by
  whole lengths. Only the forwards wrap re-bases the relative move tracks, and only with
  `bNoResetOnRewind`.
- A non-looping action clamps at the end and stops; `Completed` or `Reversed` fires.
- The end tests are `position ≤ length` forwards and `0 ≤ position` backwards, so a NaN position counts as
  past the end.
- An action with `bClientSideOnly` and `bSkipUpdateIfNotVisible` skips updates while none of its actors
  has been rendered recently.

STRONG. `Playback` implements this logic, minus the rendering check. Two safety deviations: a looping
action whose length is not positive stops instead of looping forever, and one step wraps at most a million
lengths.

**Group bindings (`InitInterp`).**
- Each non-director, non-folder group collects the objects linked to the variable links whose label equals
  its `GroupName`, plus named object variables of that name.
- It gets one group instance per actor, or one unbound instance when it has none. An actor may belong to
  only one group.
- The director group gets an instance per player controller. `InterpGroupAI` groups bind pawns.

STRONG. Our bindings record the label-to-group match and the linked objects. A variable link with a
`PropertyName` feeds that action property instead: 2 shipped actions drive `PlayRate` from a float
variable.

## Census (CONFIRMED)

Level scope plus prefab archetypes. AG-StarHaven's 60 actions include 3 in prefab archetypes, and its 57
`InterpData` include 3 in prefab archetypes.

| map | actions | InterpData | groups | tracks (incl. sub-tracks) | group links | camera anims |
|---|---|---|---|---|---|---|
| AG-Workshop | 19 | 19 | 22 | 63 | 18 | 0 |
| AG-ParadiseCave | 35 | 35 | 42 | 97 | 39 | 0 |
| AG-BeautifulCity | 17 | 17 | 21 | 83 | 19 | 0 |
| AG-Darkcave | 6 | 6 | 11 | 28 | 8 | 2 |
| AG-StarHaven | 60 | 57 | 114 | 397 | 114 | 1 |
| AG-IceCave | 13 | 13 | 15 | 18 | 14 | 0 |
| TheCore | 7 | 7 | 10 | 20 | 8 | 0 |
| AG-Epilogue | 3 | 3 | 6 | 13 | 3 | 0 |
| ASAMUFrontEndMap | 1 | 1 | 1 | 4 | 1 | 0 |
| ASAMULegal, ASAMUEntry, Freds_place | 0 | 0 | 0 | 0 | 0 | 0 |
| **total** | **161** | **158** | **242** | **723** | **224** | **3** |

Groups: 221 `InterpGroup`, 18 `InterpGroupDirector` and 3 `InterpGroupCamera` (in camera animations). No
folders and no `InterpGroupAI`.

| track class | tracks | curve keys | discrete keys |
|---|---|---|---|
| InterpTrackMove | 172 | 972 | — |
| InterpTrackMoveAxis | 354 | 1,035 | — |
| InterpTrackAnimControl | 49 | 81 (weight) | 56 anim keys |
| InterpTrackEvent | 35 | — | 79 events |
| InterpTrackFloatProp | 23 | 79 | — |
| InterpTrackSkelControlStrength | 17 | 145 | — |
| InterpTrackSound | 17 | 17 | 33 sounds |
| InterpTrackDirector | 15 | — | 3 cuts |
| InterpTrackFade | 14 | 36 | — |
| InterpTrackVectorProp | 12 | 36 | — |
| InterpTrackColorProp | 7 | 0 (all empty) | — |
| InterpTrackSkelControlScale | 4 | 4 | — |
| InterpTrackVisibility | 2 | — | 3 |
| InterpTrackToggle | 1 | — | 1 |
| InterpTrackParticleReplay | 1 | — | 0 |

Curve keys by mode: 1,586 `CurveAutoClamped`, 316 `CurveAuto`, 260 `CurveUser`, 218 `Linear`, 15
`CurveBreak` and 10 `Constant`.

Move tracks:
- 163 are `IMF_RelativeToInitial` and 9 are `IMF_World`; the world-frame tracks are mostly empty, plus one
  intro camera.
- All are `IMR_Keyframed`.
- 59 are split into axis sub-tracks.
- 1 uses quaternion interpolation.
- None uses lookup groups or the raw actor transform.
- 162 of the 169 move tracks in `InterpData` move their actor at all; the rest have no rotation keys and
  no sub-tracks.

Effective settings of the 158 level actions:
- 84 loop (5 of them inherit `bLooping` from a prefab archetype).
- 22 change `PlayRate`, to values between 0.03 and 1.3.
- 10 rewind on play, 2 of them also when already playing.
- 8 force a start position.
- None sets `bNoResetOnRewind`, `bIsSkippable`, `bClientSideOnly` or the camera-transition flags.
- `InterpLength` ranges from 0.33 s to 1,099 s.

Bound objects, by class:
- `InterpActor`: 237
- `SkeletalMeshActorMATWithFollowCollision`: 36
- `ASAMUFallingRock`: 35
- `PointLightMovable`: 8
- `CameraActor`: 4
- `SkeletalMeshActorMAT`: 3
- `StaticMeshActor`: 3
- `SkeletalMeshActor`: 2
- `ASAMUInteractable_Actor`: 2
- `Emitter`: 1

7 groups have a link labelled with their name but no variable attached to it in the action that uses
them (5 in AG-StarHaven, 2 in AG-ParadiseCave). No group link uses a named variable.

## Verification

An adversarial re-check (2026-10-10) wrote a throwaway program that used only the package reader and the
generic property decoder. It walked `InterpData` → groups → tracks → sub-tracks itself, merged prefab
archetypes itself, decoded every curve from the raw values, recomputed the automatic tangents with its own
implementation, evaluated every curve with its own double-precision evaluator, and counted variable links
and bound objects from the raw `VariableLinks`. It agreed with the decoder on:

- the per-map census (161 actions, 158 `InterpData`, 242 groups, 723 track visits, 724 track exports with
  1 unreachable, 3 camera animations, 0 instances);
- curve keys per mode and per track class, and that no curve stores `InterpMethod`;
- all 841 decoded curves, bit for bit (times, values, tangents, modes);
- the tangent results (float 1,023 / 3 / 2, vector 837 / 37 / 0);
- 161 data links, 224 group links, 2 `PlayRate` property links, the bound-object classes and the 7 groups
  without a variable;
- the settings counts (84 looping, 22 play rates from 0.03 to 1.3, 10 rewinds of which 2 when already
  playing, 8 forced starts) and `InterpLength` from 0.328 s to 1,099.5 s.

Constants read back from the executable: the event window and quaternion threshold use the float `1e-4`,
the tangent time-span floor is the double `1e-4`, the Euler scale is `182.04445` and the slomo floor
`0.1`. Behavioural findings of the re-check (all fixed in the code, with tests): the event direction rule
for jumps while playing in reverse, the initial-transform time at (re)initialisation, the input
precedence, NaN stepping and the per-map budgets. Parity with the running game is still unmeasured.

## Local outputs

`asamu-import matinee` writes `<out>/matinee/<map>.matinee.json` and `manifest.json`, about 0.9 MB for
all maps. These files contain original object paths, curves, sound and animation names: keep them local
and never commit them. `--out` refuses the repository (except git-ignored `research/` folders), the game
install, and symlinks, the same guard as `asamu-import levels`.

## Open items

- **Run-time effects not yet ported or simplified:**
  - AnimControl: the slot weight curve (full weight assumed) and slot selection by name (one body
    animation per actor); notifies of backwards moves (`IssueNegativeRateNotifies`).
  - Sound: stop on reverse and continue-on-end rules.
  - Property tracks: the engine's property-name resolution on actors and components (only the names the
    data uses are mapped); camera actors' `FOVAngle`.
  - SkelControl scale, ParticleReplay; the engine side of toggle tracks on emitters.
  - Director transition blending, camera-cut streaming hints.
  - Camera animations: post-process settings and tracks, non-local play spaces, the aim bias (see
    [Camera animations](#camera-animations)).
- **AI groups and pawn offsets** in `CalcInitialTransform`: unused by the shipped data. Not implemented.
- **Base matrix scale** (`GetBaseMatrix`), the operation order of the winding transform and frame
  products, and the general `FMatrix::Inverse` with reversed summation in `CalcInitialTransform`:
  TENTATIVE. Rotators come out rounded to whole units, so the effect is at most one unit.
- **Runtime wiring:** the game has to rebuild every `MoveInstance` at the action's position whenever
  `Playback::needs_init` says the inputs start a stopped action, and at 0 when a step or a rewind asks for
  a re-base.
- **Lookup-key tangent operation order**: TENTATIVE, and unused by the data.
- **Trig table contents and C-library math** (`atan2f`, `acosf`, `sinf`, `roundf`): assumed to match
  Rust's `f32` methods.
- **Behavioural parity** (traces of the original game playing a Matinee) is still to be measured on
  Windows. Everything above is static analysis plus self-consistency.
