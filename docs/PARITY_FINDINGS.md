# Parity findings: first recordings of the original (2026-10-10)

What the first recordings of the original game show when they are replayed through our simulation: which
behaviours the recordings confirm, where our runtime differs and why, and the order in which the differences are
to be fixed. This is the synthesis of one converter pass and five independent analyses (ground movement, air,
grapple, level state, sampling), with the lead's own re-measurements in section 0.

**Read this first:**

- Every comparison so far has the verdict *diverged*. Nothing here is a parity number, no tolerance was changed,
  and no tracker item was upgraded to verified on the strength of it.
- The recordings (45,237 frames, Windows build, gamepad) and everything derived from them stay on the
  maintainer's machine under git-ignored `research/local/`; so do the analysts' scratch scripts named below
  (`ground/…`, `air/…`, `grapple/…`). The commands are reproducible by anyone with their own copy of the game
  and their own recordings (`docs/TRACE_CAPTURE.md`).
- Confidence labels are the project's usual ones (CLAUDE.md). "Ours" means this repository's runtime.
- Findings are numbered P1… (differences in the simulation, level data or level state) and N1… (not simulation
  differences: recorder, converter or harness limits).

Sources: the converter engineer's report, the five analyst reports (ground, air, grapple, level-state, sampling) over the three Windows recordings (45,237 records), and the lead's own re-measurements in section 0.

Tick convention: `segN tick T` is the tick of the v2 canonical trace, and raw record index = segment base + T.
- Bases: DC1 `[0, 7795, 8156, 8194]`, PLAY2 `[0, 3138, 13770, 14337, 15038, 18404, 25512, 25717, 27242]`, WS1 `[0]`.
- The air analyst's numbers are raw indices; they are marked "raw" where cited.

```sh
# from the repository root
T=target/debug/asamu-trace; W=research/local/traces/win
C="$HOME/Library/Application Support/asamu-decomp/converted"
WS1=$W/v2/WS1/20261010T045947Z-WS1-story-walk.trace.jsonl
DC1=$W/v2/DC1/20261010T051704Z-DC1-A1-A3-T1-T3      # + .segN.trace.jsonl
PL2=$W/v2/PLAY2/20261010T052115Z-PLAY2-free         # + .segN.trace.jsonl
YAW=research/local/parity/ground/yaw                # diagnostic --move-frame yaw conversions
TOL="--compare --tol-position 1 --tol-velocity 10 --tol-angle 0.001 --tol-fov 0.1 --tol-anchor 1"
# every replay below: $T replay <trace> --out <scratch>/x.jsonl --converted "$C" <options> $TOL
```

## 0. What the lead re-measured (CONFIRMED unless marked)

| # | Check | Result |
|---|---|---|
| V1 | `cargo build -p asamu-trace --locked` | Up to date; binary 7,554,176 bytes (17:15). No other cargo command was run. |
| V2 | Native pull-back in the Mac executable: `objdump` at `UStaticMeshComponent::LineCheck` 0x100C58790, `UModel::LineCheck` 0x10092BD20, `UBrushComponent::LineCheck` 0x100759CB0; constants read from the file | 0.1 @0x1016457B8, 1.0 @0x101633D88, 4.0 @0x1016457CC. The hit distance is reduced by 0.1 uu when the trace is shorter than 1 uu, else by min(0.1 × length, K). K = 1.0 for the BSP box path, brushes and the static-mesh simple path; K = 4.0 for the static-mesh per-triangle path. Trace flag 0x80000 skips it. The ground analyst's reading is right. |
| V3 | Same function, branch logic | Non-zero extent uses the flag at StaticMesh+0x144; zero extent uses the flag at +0x140. Trace flags 0x20100 force the per-triangle path. In the simple path a null BodySetup (+0xF8) returns "no hit". |
| V4 | NEW: `UModel::LineCheck` zero-extent path (0x10092C049 to 0x10092C11B) | The hit is moved back by a flat 0.5 uu along the trace (constant −0.5 @0x10163926C over the trace length, clamped to [0, 1]). No analyst reported this. Not measured: no recording has a BSP anchor. |
| V5 | Mesh flags (`ground/meshflags.pkl`, 882 manifest meshes) | 515 have no BodySetup and the box flag unset; 297 have a BodySetup and the box flag unset; 70 serialise the box flag false (19 of those have a BodySetup). 316 have a BodySetup in total. The analysts' counts (297, 316, 515) reconcile. |
| V6 | Blocking placements per map by class (persistent level only) | simple / no pawn collision / per-triangle: Workshop 273 / 755 / 3; ParadiseCave 1,518 / 222 / 237; BeautifulCity 2,054 / 1,080 / 142; Darkcave 1,301 / 800 / 108; StarHaven 3,112 / 1,828 / 7; IceCave 4,090 / 358 / 138; Epilogue 288 / 516 / 2; TheCore 44 / 98 / 0. |
| V7 | The air analyst's 91 standing places, re-classified with the ground analyst's flags | 36 on per-triangle meshes, 55 on simple-collision meshes, 0 on a no-BodySetup/flag-unset mesh. On per-triangle floors the box-footprint gap is 4.72 to 5.17, except four lower values (1.18, 2.26, 2.38, 2.62). The 2.62 is the after-landing rest at the Darkcave start; The other three were not checked. |
| V8 | Geometry probe (air analyst's `geom.py`) at the four "pawn cannot move" starts | Each has a no-BodySetup/flag-unset prop inside the original pawn's volume: DC1 seg3 742 `rockpile_01` (7.64 uu above the pawn's bottom; the cave floor mesh 1.25); PLAY2 seg6 95 `redrock_04` (1.44); PLAY2 seg1 3950 `candle_1` (9.43); PLAY2 seg0 1868 six props (`corkboard_bedroom`, `Wood`, `postit`, `basket_lid_01`, `file_holder_01`, `billboardNotes`). No blocking-volume hull is in the footprint at DC1 seg3 742. |
| V9 | Workshop door in our collision | `UTTeamStaticMesh_112` (`workshop_door_01`, BodySetup, flag unset) has 1,052 collision triangles in the pawn's sweep slab; largest x 281.96, side at y −342.0. Our pawn does not walk through it: it touches at x ≈ 297 and slides round the corner at y ≈ −365. |
| V10 | `$T replay $WS1 --from-tick 1500 --ticks 150 --story-mode on` | dz = −1.0000 from tick 1501. Horizontal bit-equal at 1513 and 1514; first horizontal difference at 1515 (0.00018 uu, 0.0112 uu/s). Tick 1563: original 12.365 uu/s at x 303.19, ours 132.0 at x 301.14. |
| V11 | Wall contact at WS1 1562 to 1569 against V2's rule | Original x: 303.4036, 303.1922, 303.0848, 303.0847, then 303.0844 to 303.0840, while the attempted moves are 2.266, 0.788, 0.662, 0.557 uu. With one extra sweep in the first contact tick (the step-up attempt), the rule predicts moves of 0.2115 and 0.1075; recorded 0.2114 and 0.1074. The rest gap solved from tick 1563 alone is 0.0999 uu (rule: 0.1). Contact plane for the pawn centre: x = 302.985. |
| V12 | Flat walks | DC1 seg0 1010+68: horizontal bit-equal on 69 of 69, velocity max 0.000034, dz −3.484 to 0. PLAY2 seg5 1757+80: standard conversion velocity max 0.2407, 54 of 81 bit-equal; yaw-frame conversion 0.000061, 81 of 81. DC1 seg3 1806+200: yaw-frame 201 of 201; standard 2 of 201, max 0.0234 uu. |
| V13 | `$T replay ${PL2}.seg4.trace.jsonl --from-tick 3110 --ticks 40` | Ours is deflected at tick 3145 (horizontal speed: original 1152.1, ours 861.2, then 565.2). The nearest converted collision surface to the original pawn centre at 3145, 3146, 3148 is `DynamicBlockingVolume_6` at 15.0, 3.1, 11.6 uu; nothing else within 120 uu. That actor has `bEnabled = false`. |
| V14 | Blocking volumes that start disabled | 15 of 24 `DynamicBlockingVolume` actors: BeautifulCity 2 of 7, Darkcave 6 of 6, IceCave 3 of 3, StarHaven 3 of 7, TheCore 1 of 1. `scene.rs` never reads `bEnabled` for them. |
| V15 | `$T replay $WS1 --kismet --tick-rate 60 --no-init --ticks 1700` | Our top horizontal speed is 264.0011 (tick 1554). The original's `GroundSpeed` is 132.0 on 3,001 of 3,001 records. Kismet data has `setspeed 0.3` in AG-Workshop; `SetSpeed 0.15`, `setspeed 0.3`, `changesize 1.4` in AG-Epilogue; nowhere else. |
| V16 | Grapple anchor (`grapple/pullback.json`) | 52 airborne static-anchor presses; on the 47 within 0.05 uu sideways, our hit is 3.9967 uu beyond the recorded anchor (median 3.9988, sd 0.024, 3.892 to 4.088). Levels: BeautifulCity 33, ParadiseCave 9, Darkcave 5; ranges 691 to 4,939 uu. Replay `${PL2}.seg5.trace.jsonl --from-tick 6526 --ticks 60 --start force`: anchor error 3.996796 on all 25 attached ticks. |
| V17 | Velocity float form, all 27,443 walking pairs | Recorded velocity == f32(f32(dLoc) × f32(1/dt)) on 27,443; the division form matches 23,839. |
| V18 | Other recounts | Free-fall dVz/dt median −1039.88 (n 2,490, with a stricter filter than the analysts used). First air record Vz exactly 1600 on 12 and 750 on 16 take-offs. 9,601 attached records, all Physics 4, all zero acceleration. 128 landings, used-grapple count 0 after each. AirControl changes only at raw DC1 13254 and PLAY2 25525. Workshop z is 119.15 on 1,817 records and 351.15 on 969. Both FOV fields constant on 45,237 records. No `GroundSpeed` 66 anywhere. |
| V19 | Camera FOV accessors (bytecode identifier names only, read locally) | The FOV getter returns the locked FOV when the lock flag is set, else the cached POV FOV; the setter writes the lock; the view-target update writes the default FOV into the POV. The level-state analyst's F2 is right. |
| V20 | Further reproductions | `$WS1 --from-tick 2673 --ticks 327 --story-mode on`: ours 132.0 → 68.0 uu/s at tick 2986, original stays 132.0. `${DC1}.seg3 --from-tick 742 --ticks 60`: original travels 258.6 uu, ours 0.000. `${DC1}.seg1 --from-tick 0 --ticks 16 --start force`: original lands at tick 7 (z 54996.598), ours at tick 9 (z 54984.168, −12.43). `$YAW/PLAY2/…seg1 --from-tick 3845 --ticks 70`: bit-equal through 3893; at 3894 the original is stopped 1.1655 uu before ours. |

Taken from the analysts as reported, not re-measured: the open-loop walking model counts, the grapple pull-law fit, refire timing, death/respawn timing, Matinee timing, eye height, the layout-checker reconciliation, the quantisation random walk.

## 1. Where the analysts disagreed, and who was right

| Topic | Positions | Verdict |
|---|---|---|
| "Pawn does not move" at DC1 seg3 tick 742 | air: inside `rockpile_01`. ground: inside our triangles of a simple-collision floor. grapple: inside the bounds of `DynamicBlockingVolume_2`. | Air is right (V8): the prop is the larger overlap (7.64 uu against 1.25). The grapple guess is wrong: no hull is in the footprint. All four stuck starts are no-collision props; the ground analyst's unidentified PLAY2 seg6 tick 95 is `redrock_04`. |
| Workshop stop at WS1 tick 1563 | sampling: unknown blocker, possibly Kismet or a mover. ground: the door's simple hull, "ours walks through the door". | The blocker is the door mesh (ground). But the door's triangles are in our collision and ours does not walk through it (V9). The difference is the pawn's shape: a box of half-width 21 meets the face x = 282 at centre x = 303.0 whatever the sideways offset, while a radius-21 cylinder at y = −351.1 passes the corner. This event is evidence for the box (P4) and for the pull-back rule (V11), not specifically for hulls. |
| `Platform_Medium_2` | air: a BodySetup mesh stood on at its hull (AIR-2), with the rim walk at raw DC1 5113 to 5118 as hull evidence. | Its box flag is serialised false, so it is per-triangle (V5); its 8 standing gaps (2.26 to 5.17) fit the per-triangle band. The rim walk is box against cylinder (P4): STRONG, not replayed with a box. |
| Cap of the pull-back on triangle meshes | air: 4.0 "from memory", TENTATIVE. ground: 4.0 read from the binary. | Ground is right (V2). |
| Grapple anchor 4 uu: flat or length-dependent | grapple: measured, form UNKNOWN. | It is the per-triangle rule min(0.1 × length, 4); a 16,384 uu trace always gives 4.0. The fire trace is a bullet trace (GRAPPLE.md line 162) and flags 0x20100 force the per-triangle path (V3): STRONG that this is why hulls are not hit. On BSP it is a flat 0.5 uu (V4). On brush hulls up to 1.0; the brush zero-extent path was not read separately: TENTATIVE. |
| What the pawn is swept as | grapple G4: "the pawn's cylinder" against simple shapes. ground and air: axis-aligned box. | Box (P4). |
| Why the original stands 1.0 uu higher on flat floors | sampling L-1: cylinder not 44 high, or a component offset, or a trace offset. level-state F4: unknown. | Trace offset: K = 1.0 on BSP and simple hulls, and 72 + 44 + 2.15 + 1.0 = 119.15 (V2, V18). No recorder field for the cylinder size is needed. |
| Phantom props: mechanism | air AIR-3: TENTATIVE. ground G2: CONFIRMED. | CONFIRMED (V3, V5, V7, V8): every instance the air analyst found independently is in the no-BodySetup/flag-unset class. |
| Recorded FOV | sampling R-1: cause unknown. level-state F2: the field cannot show a locked FOV. | Level-state is right (V19). |
| Six grapple attaches one or two frames late | sampling: not investigated. | They are the timer-served refires of the grapple analyst's weapon model (9 presses, latency 1 to 3 records). |
| Landing count | air 111; grapple and level-state 128. | Both: 111 from Falling plus 17 from Flying (release and landing in one frame). |
| Severity of pull-back and box | ground: medium. air: high. | High: they change the pawn's height on every tick, decide step and landing contacts, and block every vertical verdict. |
| Ground G1 "hull face at x = 282.0" | Stated as a hull face. | The hull is not decoded. The measured contact plane for the pawn centre is x = 302.985 (V11); with extent 21.0 the face is at 281.985, and our triangles end at 281.96. Not decidable yet. |

## 2. Findings, de-duplicated and ranked

Status values: **READY TO FIX** (evidence supports one specific change), **NEEDS EVIDENCE**, **NOT A SIMULATION DIFFERENCE**.

### P1. Meshes with no simple collision do not block the pawn in the original; ours collides with their triangles
- **Class / severity / confidence:** level-data; high; CONFIRMED (default value of the flag: STRONG). **READY TO FIX.**
- **Merges:** ground G2, air AIR-3, ground G7 (stuck starts), level-state F4 ("inside our geometry at 7"), sampling L-1 ("4 unchanged").
- **Repro:**
  - `$T replay ${DC1}.seg3.trace.jsonl --from-tick 742 --ticks 60`: the original travels 258.6 uu, ours 0.000.
  - `$T replay $WS1 --from-tick 2673 --ticks 327 --story-mode on`: at tick 2986 ours drops from 132.0 to 68.0 uu/s at a light-beam mesh the original walks through.
- **Original:**
  - For a pawn (non-zero extent) trace, a static mesh whose box flag is on and that has no BodySetup reports no hit (V3). 515 of 882 meshes are in this state.
  - 0 of 27,581 walking records and 0 of 91 standing places are on such a mesh.
  - Over the Workshop rug (2 triangles at z 72.2) the original stays at 119.150.
- **Ours:**
  - `scene.rs` `component_blocking` uses actor and component flags only, so these meshes block: 755 of 1,031 blocking placements in AG-Workshop (V6).
  - Our pawn rests on the rug at 118.34, starts inside props and cannot move (V8), and lands one tick early on `redrock_04` at the Darkcave start (+3.38 uu).
- **Fix:** the importer writes `UseSimpleBoxCollision` / `UseSimpleLineCollision` per mesh; the scene builder sets `blocks_pawn = false` for a mesh with the box flag on and no BodySetup. Bullet traces (the grapple) still hit the triangles, so `blocks_traces` stays as it is.
- **Unknown:** a BodySetup with empty geometry; non-bullet zero-extent traces (crosshair) with the line flag.

### P2. Meshes with a BodySetup are collided at their simplified hulls; ours uses their triangles
- **Class / severity / confidence:** level-data; high; CONFIRMED that the simple path is used, hull geometry not decoded. **READY TO FIX** (decode work on the user's files; no new recording needed).
- **Merges:** ground G1, air AIR-2, level-state F4, grapple G4 (part), air AIR-4 (part).
- **Repro:** `$T replay ${DC1}.seg1.trace.jsonl --from-tick 0 --ticks 16 --start force` (BeautifulCity start on `Village_Entrance_Cave`): ours lands 2 ticks late and rests 12.43 uu lower.
- **Original:**
  - 297 meshes take the simple path. On flat hull tops the pawn's bottom is 3.151 uu above the triangles (2.15 + 1.00, 7 places).
  - Elsewhere it is −4.2 to +23.6 uu from the triangles (55 standing places; one outlier at −38.9 on a broken bridge end).
  - 45.3% of walking records (12,481) stand on such meshes.
  - The Workshop stairs are a smooth ramp: +1.42 uu per frame at 132 uu/s.
- **Ours:** triangles for every mesh. Rest height differs by −23.2 to +25.1 uu on these floors (n 1,201; 48.2% beyond 5 uu); stairs are taken as steps.
- **Fix:** decode `RB_BodySetup.AggGeom` (convex, box, sphere, sphyl), carry it in the manifest, and use it for pawn queries when the box flag is on (triangles when it is false). The same applies with the line flag for non-bullet traces.

### P3. Line checks report the hit pulled back
The original therefore hovers a true 3.15 or 4.95 uu, rests 0.1 uu from walls, and puts the grapple anchor 4 uu short of the surface.
- **Class / severity / confidence:** simulation; high; CONFIRMED (native rule V2 to V4; measured on a BSP floor, simple floors, triangle floors, a wall and 47 anchors). **READY TO FIX.**
- **Merges:** ground G3, air AIR-1 (pull-back part), grapple G1, sampling L-1, level-state F4 (a), air AIR-4 (part), the first comparison's "1 uu from tick 1", the converter report's "drop at the first tick".
- **Repro:**
  - Floor: `$T replay $WS1 --from-tick 1500 --ticks 12 --story-mode on` (dz −1.0000 from 1501).
  - Wall: the same from 1500 for 70 ticks (V11).
  - Anchor: `$T replay ${PL2}.seg5.trace.jsonl --from-tick 6526 --ticks 60 --start force` (3.9968 uu).
- **Original:**
  - The 28 uu floor check reports 1.0 uu (BSP, simple hulls) or 2.8 uu (triangles) less than the true distance, so the reported 2.15 hover is a true 3.15 or 4.95.
  - A landing leaves 0.1 × the fall step; the walking floor check then lifts in stages (2.615, then 4.95).
  - The fire trace's anchor is 3.9967 uu before the surface.
  - Threshold effect: in PLAY2 seg8 presses 155 to 277 the first gun tick measured 197.0 to 199.9 uu and released at once at 335 to 365 uu/s.
- **Ours:**
  - `CONTACT_SKIN` 0.05 uu on moves; nothing on the floor check or on rays.
  - Standing 1.000 uu low on BSP and simple floors, 2.7 to 4.9 low on triangle floors; anchor 4.0 too far.
  - The same seg8 presses measured 200.0 to 202.9 uu, stayed attached and reached 640 to 720 uu/s.
  - Release velocity is 9.8 to 13.5 uu/s off.
- **Fix:** the collision bridge returns hit times reduced by the native rule per primitive kind, for sweeps and rays. Extent: K 1.0 or 4.0. Zero extent: triangles K 4.0, BSP flat 0.5, brush K 1.0 (TENTATIVE). Remove `CONTACT_SKIN`. Nothing is tuned: all four constants come from the executable.
- **Derived, not measured:** step reach about 31.15 or 32.95 uu (ours 30.15): TENTATIVE.

### P4. The pawn is swept as an axis-aligned box (21, 21, 44), not as an upright cylinder
- **Class / severity / confidence:** simulation (collision shape); high; CONFIRMED on floors, STRONG on walls. **READY TO FIX** for the shape. The normal reported on edge and corner contacts must first be read from the native box check; that is not a recording question.
- **Merges:** ground G4, air AIR-1 (shape part), sampling L-2 and L-3, ground G1 (door), air AIR-2 (rim walk).
- **Repro:**
  - Wall: `$T replay $WS1 --from-tick 1500 --ticks 150 --story-mode on`, tick 1563.
  - Oblique wall: `$T replay $YAW/PLAY2/20261010T052115Z-PLAY2-free.seg1.trace.jsonl --from-tick 3845 --ticks 70`, tick 3894.
  - Slope: `$T replay ${DC1}.seg0.trace.jsonl --from-tick 3724 --ticks 52`.
- **Original:**
  - On triangle floors the gap under a 42 × 42 square footprint is 4.954 (sd 0.113, n 23); under a radius-21 disc it is 5.811 (sd 0.908).
  - Box plus 2.8 explains 89.9% of 276 planar samples within 0.3 uu; cylinder plus 2.8 explains 23.9%.
  - Door: stopped with the centre 21.0 uu from the face x = 282 at a sideways offset of 9 uu, and later slid at y = −363.02, which is 21.02 uu from the side face.
  - The oblique wall was reached 1.17 uu before ours.
- **Ours:** `collision/cylinder.rs`. Up to 4.4 uu too low on slopes in addition to P3; passes corners a box cannot.
- **Fix:** a swept axis-aligned box against triangles, hulls and BSP for pawn queries; the test worlds likewise.

### P5. A `DynamicBlockingVolume` with `bEnabled = false` is solid in our world
- **Class / severity / confidence:** level-data; high; CONFIRMED that the obstacle is the disabled volume (V13); STRONG for the mechanism (the class sets its collision from `bEnabled` at start; only identifiers were read). **READY TO FIX.**
- **Merges:** grapple G3; the converter report's "grapple, manual release" row (1,279.59 uu).
- **Repro:** `$T replay ${PL2}.seg4.trace.jsonl --from-tick 3110 --ticks 150`.
- **Original:** free flight on all 113 attached steps 3112 to 3225, reaching 2,099 uu/s.
- **Ours:** deflected from tick 3145; 1,246 uu off by the release.
- **Fix:** hulls of such actors start switched off when `bEnabled` is false and follow Kismet toggles. 15 of 24 start disabled (V14); Kismet references them in 5 maps.

### P6. `SetSpeed` is not executed: AG-Workshop walks at 264 uu/s, the original at 132
- **Class / severity / confidence:** kismet-or-level-state; medium (visible at once in the first level); CONFIRMED. **READY TO FIX.**
- **Merges:** ground G5, level-state F1.
- **Repro:** V15.
- **Fix:** when the Kismet host receives a console command `setspeed F` (case-insensitive), set `GroundSpeed` = class default 440 × F; later sprint and story transitions overwrite it (A-WK-3). AG-Epilogue needs the same and has two commands plus `changesize`: untested.
- **Open:** whether a sprint press in the Workshop overwrites 132 (WS1 has no sprint press).

### P7. Direction vectors are built from exact angles; the original truncates rotator angles to 4 units
- **Class / severity / confidence:** simulation; low; CONFIRMED (effect), STRONG (cause: 14-bit angle table). **READY TO FIX** for the truncation. The lean-roll and kept-flight-pitch parts need the pawn's roll and pitch in the simulation first.
- **Merges:** ground G6, sampling S-1, grapple G2, converter evidence M1 to M6.
- **Repro:** V12 (velocity 0.2407 against 0.000061 uu/s); first differing bit at WS1 tick 1515 (start yaw −32777).
- **Original:** move axes come from the previous record's pawn rotation, each angle floored to a multiple of 4 (10,051 of 10,051 on-axis samples). The aim ray is the same: 47 of 52 anchors within 0.05 uu; with exact angles the median is 0.74 and the worst 1.99 uu.
- **Ours:** `controller_move` and `aim_direction` use exact radians.

### P8. Velocity from displacement divides by dt; the original multiplies by the f32 reciprocal
- **Class / severity / confidence:** simulation (float order); low; CONFIRMED (V17). **READY TO FIX.**
- **Ours:** `ue3_movement.rs` lines 886, 1134, 1140 (walking, falling). Line 1016 (flying) is untested; leave it.
- **Effect:** 1 ulp of velocity on 111 of 635 otherwise bit-equal ticks.

### P9. View angles accumulate in f32 radians; the original keeps integer rotator units
- **Class / severity / confidence:** simulation; low; CONFIRMED. **READY TO FIX.**
- **Original:** integer deltas reproduce yaw and pitch on 45,223 of 45,223 ticks.
- **Ours:** 1.0e-5 rad off after 7,795 ticks, 1.1e-5 after 10,632.

### P10. Spawn and respawn placement
- **Class / severity / confidence:** simulation; low; CONFIRMED (original numbers), TENTATIVE (ours: read, not run). **NEEDS EVIDENCE:** the native placement routine's search order (binary), and a run of our respawn path after P1 to P4.
- **Merges:** air AIR-6, level-state F5 and F9.
- **Original:**
  - At level start the pawn falls from the PlayerStart height (implied 4518.584 against 4518.5845; 55030.278 against 55030.293) and lands at world time 0.678 and 0.680 s. The fall starts at 0.400.
  - At 3 of 5 respawns the pawn appears 6.2 to 8.8 uu above the checkpoint's spawn point and lands 4 frames later.
- **Ours:** `from_loaded_map` places the pawn grounded; `find_spot` is a marked stand-in.
- **Doc error to correct:** ABILITIES.md A-CP-5 says the pawn spawns walking.

### P11. An idle or slow pawn pops up 2.148 uu, falls 4 to 7 frames and lands again
- **Class / severity / confidence:** unknown; low; CONFIRMED (observation), TENTATIVE (cause). **NEEDS EVIDENCE:** re-run after P3 and P4.
- **Source:** air AIR-5 (raw DC1 13247; PLAY2 17594, 18040, 21574, 21621, 21629, 25247).
- The 2.148 rise is what P3 predicts for a pawn resting below the pull-back (the gap was 2.38 before the pop), so it may reproduce by itself. Why it then falls is not explained.
- These are real landings: the one at raw DC1 13254 switched AirControl to 0.35.

### P12. Remaining contact differences (consequences, to re-measure)
- **Class / severity:** unknown until P1 to P5 land; medium. **NEEDS EVIDENCE:** re-run `ground/starts_sweep_yaw.py`, `ground/classify.py`, `grapple/flycol.py`, `grapple/postrel.py`, `air/batch_air.py`.
- **Merges:**
  - ground G8: 26 first divergences, all contact events.
  - grapple G4: 6 of 52 flights blocked first in the original, 5 same-tick with a different outcome.
  - air AIR-4: landing tick and landing-frame speed, e.g. raw PLAY2 18114 where ours keeps 358.4 uu/s and the original 324.6.
- The air rules themselves agree: with the original's own numbers the landing accounting closes to 0.01 to 0.03 uu.

### P13. Kismet timing leftovers
- **Class / severity:** kismet-or-level-state; low. **NEEDS EVIDENCE:** our runtime's transition times in the replay output.
- ParadiseCave exit: at most 12.414 s after the trigger overlap against a nominal 12.374 s chain, which leaves 2.4 frames; the documented rules suggest 4 to 6.
- Second BeautifulCity story segment: 40.834 s against a 40.0 s key; the 0.83 s is the phase of a looping Matinee.

### Not simulation differences

| ID | What | Class | Source | Action |
|---|---|---|---|---|
| N1 | The recorded FOV cannot show the zoom: it is the cached POV FOV, reset every update, while the zoom locks the camera FOV. The 40° FOV "divergence" is an artefact. | recorder | V19; constant on 45,237 records | Record the camera's FOV lock flag and locked FOV. Exclude FOV from verdicts until then; do not change the tolerance. |
| N2 | A replay start snaps our pawn to our floor + 2.15 and says nothing when the start overlaps our collision. | harness | ground G7; V8 | Note the overlap and the first-tick snap in the replay output; compare the recorded base actor. |
| N3 | Level-script state changes inside a segment are not followed: DC1 seg0 7214, seg3 5019; PLAY2 seg0 1475, seg1 785 and 8250, seg4 2470 and 2535. | harness | level-state F3; grapple G7 | End validity at the first such tick, or inject the recorded change under a labelled option. |
| N4 | Moving grapple targets do not exist in per-sample replays: 11 grapples, 5,071 of 9,601 attached frames. | harness | grapple G5 | Run Matinee movers per sample, or take the recorded anchor where the helper coincides with it. |
| N5 | The start state lacks eye height (28.0 to 44.6 at walking presses), walk bob and the weapon refire timer; ground-press anchors are off by 3.5 to 129.6 uu. | harness | grapple G6 | Apply the recorded eye height; start grapple replays 0.2 s before the first press. |
| N6 | Respawn teleports inside a run are unmarked: DC1 seg0 997, 5746; PLAY2 seg1 5946; PLAY2 seg5 2393, 6779. | converter | sampling H-1 | Mark them, list them as events, stop replays there. |
| N7 | 15 of 134 attaches start and end inside one frame and are invisible in `grapple_state`. | converter | grapple G8 | Count attaches from the used-grapple counter. |
| N8 | `compare` reports one 3-D number, angles in radians, free-running only. | harness | sampling H-2 | Horizontal and vertical columns, rotator units, a one-step mode. |
| N9 | The base actor name is not unique across streamed levels (280 records). `power_jumped` and `has_released_jump` are never true in 45,237 records. Dropped samples are not attributed (8 single-frame gaps). | recorder | ground G9; air AIR-7; sampling R-2 | Write the outer level; check the two flag offsets or drop them; log dropped frames. |
| N10 | Our side of a trace has no eye height, AirControl or GroundSpeed, so those cannot be compared. | harness | air AIR-7 | Optional columns or notes. |
| N11 | Live layout check 1 (1,803 of 1,954) is a wrong expectation: sizeof is the property end rounded to the alignment (159 predicted against 151 observed; 8 unreconciled). Check 2 (630 fields) is exactly the native classes of three editor packages. | recorder tooling | level-state F7, F8 | Fix the checker's expectations and keep its full output. Neither touches the recorder's 14 classes. |
| N12 | `GroundSpeed` 66 "a second after level start" is in no recording and cannot come from the Workshop's Kismet. | unknown | level-state F6; V18 | Remove the claim from PARITY.md unless re-observed (session item 1). |

## 3. Confirmed by the recordings, and what the tracker may honestly say

### Behaviours confirmed (original against the specs)

| Doc item | Now | Numbers |
|---|---|---|
| NATIVE_PHYSICS 2.1/2.3: walking velocity update, AccelRate 2048, friction 8, caps, braking, snap below 10 uu/s | CONFIRMED | 8,952 of 9,385 input frames and 1,282 of 1,299 braking frames within 0.5 uu/s (the misses are contacts); stopping distance from 440 is 19.669 against 19.672 uu; caps 132/264/440/880 |
| NATIVE_PHYSICS 3.1: walking velocity = displacement × (1/dt), Z = 0 | CONFIRMED; float form fixed | 27,443 of 27,443 |
| NATIVE_PHYSICS 1.1: order of controller move and pawn physics (was UNKNOWN) | CONFIRMED: same frame, controller first | 91 of 91 walk starts, no lead or lag |
| NATIVE_PHYSICS 3.3: floor check 28 uu, band 1.9 to 2.4, target 2.15 | CONFIRMED with the pull-back | 119.150 on 1,817 Workshop records; landing snaps 2.126 to 2.159 |
| NATIVE_PHYSICS 9.5 q7: pull-back (was UNKNOWN) | CONFIRMED (native and measured) | V2, V4, V11, V16 |
| NATIVE_PHYSICS 4.3/4.6: gravity −520 doubled by the refinement; no apex skip; no clamp at 4000 | CONFIRMED | medians −1039.62 and −1039.92 (n 7,713); 46 apex frames; 278 frames above 4000 uu/s, fastest 6,784.7 |
| NATIVE_PHYSICS 4.2: air control, low-speed help, speed bound, wall probe | CONFIRMED | 1431.57 and 1433.18 against 1433.6; 1229.55 against 1228.8 |
| NATIVE_PHYSICS 1.4/5.2: ledge and landing time carry, landing clamp | CONFIRMED | 111 + 17 landings; 6 walk-offs with Vz 0.000 in the first falling record |
| ABILITIES A-JP-1/2/3: jump 1000, 0.7 damping every 6 frames | CONFIRMED (early-released jumps only) | 4 jumps; 12 damping steps, factor 0.69948 to 0.70131 |
| ABILITIES A-PJ-1 to 5: power jump 1600, charge threshold between 0.548 and 0.633 s | CONFIRMED | 12 jumps; apex 1231.35 against 1230.8 uu |
| ABILITIES A-PJ-4, A-IL-1: leap ×2 horizontal, 750, move lock | CONFIRMED | 16 leaps, ratio 1.9997 to 2.0000 |
| ABILITIES A-AC-2: AirControl 0.30 → 0.35 at the first non-story landing | CONFIRMED | 2 changes in 45,237 records |
| ABILITIES A-ST-1/2, A-WK-3/4.6: story-mode input rules and GroundSpeed writers | CONFIRMED | 3 of 3 jump presses ignored; 159 sprint-held frames at 264; 880 → 264 in one frame |
| ABILITIES A-DT-2, A-CP-4: death after 0.3 s, respawn at the highest-index checkpoint, unscaled checkpoint cylinders | CONFIRMED | 5 of 5; x, y within 0.006 uu |
| ABILITIES A-CM-4: eye-height smoothing | CONFIRMED (original only) | 27,443 of 27,443 walking pairs within 0.01 |
| ABILITIES A-CP-5: "spawns walking" | WRONG: falls from the PlayerStart | P10 |
| PARITY deviation 20: does the PlayerStart drop count as a landing | CONFIRMED: yes | the Darkcave start landing sets AirControl 0.35 |
| GRAPPLE G-IN-2/3/5, G-TM-2: fire timing | CONFIRMED | 169 of 169 presses (3 after carrying the timer across a one-frame gap) |
| GRAPPLE G-PH-1/2/3/6: no steering, pull, drag, 2000 cap, blocked pulls | CONFIRMED | 9,481 pairs; position median 0.002 uu, velocity p99 0.50 uu/s |
| GRAPPLE G-RL-1/2/4/7, G-MO-1: releases | CONFIRMED | 42 proximity releases with error 0.000 uu/s; 56 button releases |
| GRAPPLE G-CT-1/3/4: budget | CONFIRMED | 134 increments, 128 refills |
| GRAPPLE G-AT-7/8, G-RL-5: moving targets | CONFIRMED | anchor = helper on 5,071 records |
| GRAPPLE G-AT-4: anchor = hit location | CONFIRMED, with the hit pulled back 4 uu on triangles | V16 |
| GRAPPLE range 5000 | bracketed 4,939 to 5,067 (STRONG for 5000) | 24 rejected presses |
| KISMET_RUNTIME 1 (trigger effect next frame), 2 (link order), 9 (level-start abilities), Matinee event frame | CONFIRMED, for arrivals through a level change only | 4 of 4 triggers; 3,298 frames on both sides |
| TRACE_CAPTURE W-F5, W-F6 | CONFIRMED pass | 91 of 91; 0 of 45,237 mixed records |
| ViewPitchMax +18000 | CONFIRMED, upper limit only | 50 records at exactly 18000 |

Ours against the original, measured in replays:
- Free-fall vertical velocity is within 0.000 to 0.002 uu/s over up to 128 air frames; one replay at z ≈ 1e5 reads up to 2.2, which is position rounding.
- Take-off ticks of 6 power jumps and 12 leaps are equal.
- The attach tick is equal in 100 of 101 and the release tick in 49 of 50 airborne grapples.
- Flat-walk horizontal position is bit-equal on 584 of 584 ticks, but only with the diagnostic yaw-frame conversion.

### Tracker (`progress/progress.toml`)

Nothing moves to `verified`. No comparison passes at a measured-noise tolerance without a diagnostic conversion, and no gated regression test is in the repository yet.

| Item | Now | Honest move |
|---|---|---|
| `collision-capsule` | implemented | **partial**: the dimensions are confirmed, but the shape (box) is not implemented (P4). |
| `collision` ("Collision geometry") | verified | Keep `verified` only for its stated scope (BVH against brute force) and rename it to say so. Add an item for the pawn collision representation (simple hulls, per-mesh flags, switchable volumes) as `investigating`. |
| `ground-move`, `gravity`, `jump`, `air-control`, `pull`, `release`, `max-speed`, `range`, `targeting`, `abilities` | implemented | Stay. Replace "Not trace-verified" with the measured statement. Candidates for `verified` once the gated tests of section 4 exist and pass: `gravity` first (free-fall velocity), then `ground-move` (after P7), then `pull` / `release` / `max-speed` (after P3 and P7). |
| `fov`, `camera` | implemented | Stay; not observable yet (N1, N10). |
| `death-reset` | implemented | Stay; the note should say 0.3 s (18 or 19 frames observed), not "18 ticks". |
| `trace-capture` | implemented | Stay (the macOS route has no trace). Note that W-F5 and W-F6 were evaluated and pass, and that the FOV field is uninformative. |
| `trace-replay`, `parity-suite`, M8, M12 | implemented / partial | Stay. Note: eight segment comparisons, all diverged; causes classified here. |
| `runtime`, `story-sequencing` | implemented | Stay; `SetSpeed` is missing (P6). |

Doc statements to correct:
- PARITY.md: "Standing height is the native 2.15 uu hover"; "Collision contact"; deviations 15 and 20; the apex-notify open point; the two "not yet classified" rows and the 66 claim in "First comparison".
- MESHES.md: the BodySetup is used for pawn collision.
- TRACE_CAPTURE.md: section 1.1 and the stale status lines.

## 4. Fix plan (work packages, disjoint file ownership, dependency order)

`crates/asamu-game/src/{lib,tooling}.rs`, `crates/asamu-sandbox` and `apps/asamu` belong to the Sandbox work and are in no package. Expected values in tests come from the native rule or a spec formula, never from a replay.

### Stage 0 (parallel)

**WP-T1: `tools/asamu-trace/src/{compare,replay,convert,segments,state}.rs`, `tests/{pipeline,variable_dt}.rs`**
- Covers N2, N3, N6, N7, N8, N10: horizontal/vertical columns; angles in rotator units; a one-step mode; a start-overlap note; end of validity at level-state changes and teleports; attaches from the counter; eye height in the state note.
- Tests: the one-step mode on a fake original made by our simulation is exact; a synthetic recording with a teleport, and one with a start inside a box, produce the notes.

**WP-I: `crates/asamu-ue3` (BodySetup decode), `tools/asamu-import/src/meshes.rs`**
- Outside the listed crates, but required by P1 and P2. The manifest gains the two flags and the simple shapes.
- Tests: synthetic BodySetup fixture round trip; truncated-input rejection; gated count 297 / 515 / 70 (V5).

**WP-D: `crates/asamu-player/src/{pawn.rs,sim.rs,grapple_gun.rs}`**
- Covers P7 (truncation in `controller_move` and `aim_direction`), P9 (integer rotator view angles), and the P6 entry point (`GroundSpeed` = 440 × factor).
- Tests: yaw units 1, 2, 3 mod 4 and a negative yaw give the axes of the floored angle; yaw is exact after 10,000 integer deltas; factor 0.3 gives 132 and a sprint transition overwrites it.

**WP-C1: `crates/asamu-player/src/ue3_movement.rs`**
- Covers P8.
- Test: dt 0.01631380245089531 and dLoc.x −14.2109375 give −871.0989990234375 (division gives −871.0990600585938).

### Stage 1

**WP-A: `crates/asamu-world/src/collision/` (new box module, `mod.rs`), `tests/collision_props.rs`**
- A swept axis-aligned box against triangles, hulls and BSP; a primitive kind per instance (BSP, brush hull, simple hull, triangle mesh); instances that can be switched off. Raw hit times stay exact; the caller applies the pull-back.
- Before coding, read the native box check for the normal on edge contacts.
- Tests: on a plane of gradient (a, b) the box centre rests 44 + 21(|a| + |b|) above the plane at its centre; a box against a wall face stops at 21.0 whatever the sideways overlap; BVH equals brute force for boxes.

### Stage 2

**WP-C2: `crates/asamu-player/src/{ue3_movement.rs,world.rs}`, `tests/ue3_movement.rs`, `tests/ue3_spec_conformance.rs`** (same owner as C1)
- Covers P3 and P4 on the simulation side: the pull-back function (V2, V4), `CONTACT_SKIN` removed, box shape in the trait and in `BoxWorld` / `SlopeWorld` (K = 1.0).
- Tests:
  - A BSP-kind plane at z 72 gives centre 119.15; a triangle-kind plane gives 120.95.
  - A blocked 7 uu move stops 0.7 short; a 0.5 uu move stops 0.1 short.
  - Wall, from a gap of 0.4189 with attempted moves 2.266, 0.788, 0.662: a single sweep leaves 0.2266; the walking step with its step-up retry leaves 0.2074; then 0.1000 and 0.1000.
  - Landing with a 4.66 uu fall step on a triangle floor rests at 2.616, and at 4.766 after the next moving tick.

**WP-B: `crates/asamu-world/src/{scene.rs,fixtures.rs}`** (needs WP-I and WP-A)
- Covers P1, P2 and P5 on the data side.
- Tests: a fixture mesh without simple shapes is skipped by pawn sweeps and still hit by traces; a fixture mesh whose hull differs from its triangles; a disabled dynamic blocking volume lets a sweep through and blocks after a toggle.

### Stage 3

**WP-E: `crates/asamu-game/src/{world.rs,kismet_host.rs,converted.rs}`** (needs A, B, C2, D)
- The bridge applies the pull-back per primitive kind for sweeps and rays; `setspeed` handler; blocking-volume toggles; the spawn falls from the PlayerStart; later the placement rule (P10).
- Tests gated on converted data:
  - A Workshop pawn at (131, −339) rests at z 119.15.
  - Walking −x along y = −351 from x = 330 stops with the centre at x 303.08 ± 0.02.
  - The Workshop with Kismet walks at 132.0.
  - A sweep through `DynamicBlockingVolume_6` at (4353, −595, 37937) is free.

**WP-K: `crates/asamu-kismet`**
- No change expected: the console command is already an output and toggles already reach the host. Only needed if a blocking-volume toggle requires a new op.

### Stage 4

**WP-T2: `tools/asamu-trace/tests/real_recordings.rs`** (gated by `ASAMU_TRACE_RAW_DIR` and `ASAMU_CONVERTED_DIR`; tolerances of section 5)
- WS1 1500+62: z = 119.1500015 on every tick; horizontal bit-equal on the standard conversion (after P7).
- WS1 1500+150: no horizontal divergence at 1563; x within 0.001 of 303.1922, 303.0848, 303.0847.
- WS1 2673+327: no deflection at 2986.
- DC1 seg3 742+30: the pawn moves at 744.
- DC1 seg0 1010+68 and 3724+52: |dz| within the hover band.
- PLAY2 seg4 3110+150: unblocked at 3145.
- PLAY2 seg5 6526+60, and the 47 airborne anchors: anchor within 0.15 uu.
- PLAY2 seg8 presses 155 to 277: release in the press frame.
- Free fall, PLAY2 seg4 from 3014 (jump at raw 18055) and five respawn falls: |dVz| within 2 quanta until contact, landing tick equal.
- Raw-recording identities with no simulation: V17; attached == Physics 4.

**WP-R: `tools/trace-recorder`** (outside the listed crates; before the next session)
- N1 and N9, plus the pawn's floor normal, the weapon state and timer, and the walk-bob offset; then `win_remote.sh deploy`.

**WP-Z: docs and `progress/progress.toml`**
- Section 3.

## 5. Tolerances for a regression suite (measured noise only)

| Quantity | Tolerance | Basis |
|---|---|---|
| Tick alignment, per-tick dt, inputs, grounded, grapple state | 0 | dt equal on 45,223 of 45,223; flags exact on 45,237 |
| Yaw, pitch | 0 in rotator units; until P9, 2.4e-5 rad | Integer deltas reproduce 45,223 of 45,223; our drift is 1.1e-5 rad per 10,632 ticks. The 0.001 rad used so far is about 10 units and would hide a one-unit error. |
| Walking, horizontal position | 4 ulp32 of the larger coordinate per tick | 0 ulp on 584 ticks at coordinates above 26,705; at most 2 ulp over 200 ticks near 300 |
| Walking, horizontal velocity | 2 quanta, quantum = ulp32(coordinate) / dt | Measured quanta (max): Workshop 0.0037, BeautifulCity 0.479, Darkcave 0.527, ParadiseCave 0.957 uu/s |
| One-step (resynchronised) walking and falling | 2 ulp | The f32 spec model is one-step bit-exact on 10,211 of 10,684 walking and 2,486 of 2,517 falling frames |
| One-step grapple flight | position 0.01 uu, velocity 0.5 uu/s | Pull-law fit: median 0.002 uu, p99 0.50 uu/s |
| Free-running airborne stretch of N ticks, not bit-equal at take-off | velocity 4 × 0.78 q √N; position 4 × 0.45 q dt N^1.5 | The original's own kick: sd 0.554 quanta per tick (n 4,878). TENTATIVE (one replay); prefer the one-step mode. |
| Vertical velocity in free fall | 2 quanta (the quantum doubles while falling) | 0.000 to 0.002 uu/s in 17 of 18 replays; 2.2 in one at z ≈ 1e5, where the doubled quantum is 1.9 |
| Vertical position | none yet | Constant offsets of 0.12 to 22.7 uu until P1 to P4. The original's own standing gap spans 4.717 to 5.162 (the native band). Measure again afterwards. |
| Grapple anchor, airborne press | 0.15 uu after P3 and P7 | Residual 0.108 along and 0.05 sideways at 47 points |
| Grapple anchor, ground press | none yet | Eye height and bob are not in the start state (N5) |
| FOV | excluded | N1 |

With the standard conversion today, the horizontal gap (at most 0.056 uu per 200 ticks, 0.28 uu/s) is deviation P7. It must be reported as a deviation, not absorbed.

## 6. Next recording session (after WP-R is deployed)

Use keyboard and mouse for the whole session: move keys are recorded exactly, while the stick's size and its direction while attached are not.

1. Start the recorder at the main menu, choose New Game, and stand still for 5 seconds after the Workshop appears. *(The 66 reading, the 0.4 s clock, fresh level-start state.)*
2. In the Workshop: hold the zoom key 2 seconds and release, twice; tap it three times; then hold sprint while walking forward for 3 seconds. *(Zoom timing with the new FOV fields; whether sprint overwrites 132.)*
3. In the Workshop: walk straight into a wall and keep holding forward for 2 seconds; repeat at about 45 degrees; walk up and down the stairs once without stopping. *(Wall rest gap, box contact normal, simple-collision ramp.)*
4. In AG-ParadiseCave on flat ground: stand still and hold jump until you land, twice; then jump and tap jump once more while rising; then stand still and look fully down and fully up. *(Full jump apex, mid-air damping, pitch limits.)*
5. Sprint and power-leap without grappling, holding forward until you land, twice; then hold the power-jump key for 2 seconds while standing. *(Move lock ending at a landing; FOV while charging.)*
6. Stand still facing a fixed wall or pillar and hold grapple until it releases by itself, three times from different distances; once press jump while attached; once aim at something very far away and press grapple. *(Ground-start anchor with eye height, jump while attached, the 5000 uu limit.)*
7. Limit the game to 20 frames per second (15 if possible) and for 30 seconds walk, stop, jump, fall off a ledge and grapple once. *(Sub-steps above 0.05 s, braking pieces above 0.03 s.)*
8. Optional, if a save reaches them: the first 10 seconds of AG-Epilogue; one crystal and one glow-flower grapple; one rocket-boots flight. *(Second `SetSpeed`, release-instant targets, boots.)*

## 7. Open questions not settled by anything above

- The default of the two simple-collision flags is inferred (no mesh serialises true, 70 serialise false): STRONG.
- What a BodySetup with empty geometry does; which normal the box check reports on edges; whether the brush zero-extent path uses K = 1.0; whether the crosshair trace is a bullet trace.
- Three of 52 airborne anchors are off the pattern (along 0.28, 0.69, 2.51 uu with sideways 1.88, 0.69, 0.15).
- Two landings end slightly above `GroundSpeed` (443.4 and 883.6); one frame of grapple flight is 72 uu/s off (PLAY2 seg0 tick 1342); the speed-cap normalisation misses by 1 ulp on 13 to 17 Workshop frames.
- Why single frames are dropped during ordinary play in PLAY2 (10 resyncs).
- Maximum step height, terminal velocity 10,000, the lower pitch limit, rocket boots, release-instant targets, AG-StarHaven, AG-IceCave and AG-Epilogue are in no recording.
