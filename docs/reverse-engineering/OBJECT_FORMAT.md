# UE3 v868 export payloads: objects, tagged properties, script objects

Evidence source: every export payload of the 42 packages under
`~/Library/Application Support/Steam/steamapps/common/A Story About My Uncle/A Story About My Uncle.app/Contents/Resources/ASAMU/CookedMac`
(and `Maps/`) of the legitimately owned Mac install (Steam build 1822049), decoded read-only by our own code in
`crates/asamu-ue3` (`object.rs`, `property.rs`, `script.rs`, `schema.rs`, `model.rs`, `coverage.rs`). This page is
structure, names and counts only. No payload bytes, no script source and no decompiled code are reproduced here.

Builds on `PACKAGE_ANALYSIS.md` (summary, compression, tables), which this work did not change.

Reproduce:

```sh
C="<CookedMac>"
cargo run --release -p asamu-inspect -- coverage --all-objects "$C"            # every number on this page
cargo run --release -p asamu-inspect -- class "$C/Startup.upk" asamu.ASAMUPawn  # class model
cargo run --release -p asamu-inspect -- defaults --inherited "$C/Startup.upk" asamu.ASAMUPawn
cargo run --release -p asamu-inspect -- props "$C/Engine.u" Default__EdCoordSystem
cargo test -p asamu-ue3 --test objects_real_data -- --nocapture                # asserts the (T) claims; skips without data
```

`(T)` marks a claim asserted by `crates/asamu-ue3/tests/objects_real_data.rs` against the install.

## Result — CONFIRMED (T)

All 202,685 exports of the install are accounted for:

| Group | Exports | Result |
|---|---:|---|
| Script objects (Class, State, Function, ScriptStruct, Enum, Const, TextBuffer, 14 `*Property` kinds) | 70,946 | decoder consumes **exactly** `SerialSize` for all 70,946 |
| Class default objects (`Default__*`, `RF_ClassDefaultObject`) | 2,521 | prelude + tagged properties consume **exactly** `SerialSize` for all 2,521; 0 raw values, 0 warnings |
| Every other export (actors, components, textures, meshes, sounds, levels, ...) | 129,218 | prelude + tagged properties decode for all 129,218; 87,192 end exactly at `SerialSize`, 42,026 carry class-specific native data after the tags (not decoded yet); 0 raw values, 0 warnings |

Cross-checks on the decoded tagged properties (CDOs and all other objects) (T):

- every tag names a property declared on the object's class or one of its supers (0 undeclared tags), and its type
  agrees with the declaration;
- tags appear in the class's property-link order (0 order violations, see "Property order");
- the only classes without a script definition are the native-only `Engine.Level`, `Engine.LightMapTexture2D` and
  `Engine.StaticMesh` (their tags still decode; there is nothing to check them against).

Startup.upk's 583 class default objects are the 172 `asamu` and 411 `UTGame` classes' (T).

Every number above was re-derived by a second, strict decoder written independently in Python. The decoded values
(every CDO, every class model, and a 14,013-object sample of map objects) were compared with the Rust output: no
differences. See "Independent re-check" below.

## Object prelude (`UObject::Serialize`) — CONFIRMED (T)

Bytes before the tagged properties, in order. Each optional part is selected by the export's flags and class;
the rules below are the ones under which all 202,685 payloads decode.

| Part | Present when | Layout |
|---|---|---|
| Shadow-map prefix | class is (or extends) `DominantDirectionalLightComponent` or `DominantSpotLightComponent`, and the object is not a CDO | `TArray<u16>` (e.g. 520 entries in one map instance; empty in the `Engine.u` templates). It precedes all UObject data. |
| State frame | `ObjectFlags & 0x0200000000000000` (`RF_HasStack`) — 30,284 map actors, no export of the script packages | `i32 Node, i32 StateNode, u32 ProbeMask, u16 LatentAction, TArray StateStack, i32 CodeOffset` (CodeOffset only when `Node != 0`) |
| Component template | class extends `Core.Component` and the object is not a CDO | `i32 TemplateOwnerClass`, then `FName TemplateName` only when an outer is a CDO |
| NetIndex | always | `i32` |
| Tagged properties | every object except `Class` exports | see next section |

Notes:

- `StateStack` is empty in every shipped object, so its element layout is UNKNOWN; the decoder rejects a non-empty one.
- `Node` is non-null in all 30,284 state frames, so `CodeOffset` is always present. That it is *absent* when `Node` is
  null follows the UE3 convention and is never exercised by the data (TENTATIVE).
- `ProbeMask` is `0xFFFFFFFF` on 30,276 of the 30,284 actors (every actor of the ten gameplay and front-end maps); only
  4 actors each in `ASAMUEntry.asamu` and `ASAMULegal.asamu` have other values (re-checked over all maps). The `u16`
  after it (UE3 name `LatentAction`) varies per object with no visible pattern; meaning UNKNOWN.
- `TemplateName` is written for components under class default objects only. Components under archetypes
  (`RF_ArchetypeObject`, e.g. prefab archetypes in maps) have no `TemplateName`; this was found when the first
  hypothesis ("any template") failed on 44 prefab components. A CDO of a component class has no template part at all.
- `TemplateOwnerClass` is usually null; for subobjects of subobjects (e.g. distributions inside particle modules) it is
  the class of the object that owns the template.
- `NetIndex` (UE3 name; TENTATIVE meaning): `-1` for every export of `IpDrv.u`, `WinDrv.u` and
  `OnlineSubsystemSteamworks.u`; otherwise equal to the export index for most exports of the other `.u` files (about
  89% in `Engine.u`), and apparently numbered per merged package in `Startup.upk` (maximum 17,370, below `UTGame`'s
  net object count 17,371).

## Tagged properties — CONFIRMED (T)

```text
FName Name                         "None" (compared case-insensitively) ends the stream
FName Type                         IntProperty, FloatProperty, BoolProperty, ByteProperty, NameProperty, StrProperty,
                                   ObjectProperty, InterfaceProperty, DelegateProperty, StructProperty, ArrayProperty
i32   Size                         bytes of the value after the tag header
i32   ArrayIndex                   element of a static array (ArrayDim > 1)
StructProperty: FName StructName
BoolProperty:   u8 Value           (Size = 0)
ByteProperty:   FName EnumName     ("None" for a plain byte)
value: Size bytes
```

Value encodings (tag level):

| Type | Value |
|---|---|
| Int / Float | 4 bytes |
| Bool | none (the tag's `u8`) |
| Byte | 1 byte when `EnumName` is `None`; otherwise an 8-byte FName holding the enumerator's name |
| Name | FName (8) |
| Str | FString |
| Object | `i32` package index. **Class and component properties are tagged `ObjectProperty`** too: no `ClassProperty`/`ComponentProperty` tag occurs anywhere (T) |
| Interface | `i32` package index |
| Delegate | `i32` object + FName function (12) |
| Array | `i32 count` + elements in the item encoding below; the element type comes from the declaring property's `Inner` |
| Struct | binary or tagged, by the struct's flags (next section) |

No `MapProperty` tag occurs (45 map properties are declared; their values are never serialized).

**Item encoding** (array elements and binary struct members): as above, except that a bool is one byte and an
enum-typed byte is an 8-byte FName (enumerator name) while a plain byte is one byte. Found when arrays of enums (e.g.
`array<EWeaponFireType>`) decoded to 6 of 20 bytes with a one-byte assumption; with FNames all decode exactly.

### Binary vs tagged structs — CONFIRMED (T)

A struct value is stored in binary form when its `StructFlags` has `0x20` (`STRUCT_Immutable`), or `0x80`
(`STRUCT_ImmutableWhenCooked`) in a cooked package (every package except the three shader caches is cooked).
Otherwise it is a nested tagged stream ending in `None`. Before the rule was written down, every struct tag in the
script packages was tested both ways: exactly the `0x30`-flagged structs failed the tagged parse.

Binary structs in the install (31 definitions, T):

| StructFlags | Structs |
|---|---|
| `0x30` Atomic \| Immutable | `Core.Object.`: `Box`, `Color`, `Guid`, `IntPoint`, `LinearColor`, `Matrix`, `PackedNormal`, `Plane`, `Quat`, `Rotator`, `TwoVectors`, `Vector`, `Vector2D`, `Vector4` |
| `0x31` Native \| Atomic \| Immutable | `Engine.Font.FontCharacter` |
| `0x181` Native \| ImmutableWhenCooked \| AtomicWhenCooked | `Engine.Actor.ActorReference`, `Engine.Actor.NavReference`, `Engine.AnimNodeAimOffset.AimComponent`, `.AimOffsetProfile`, `.AimTransform`, `Engine.CoverLink.CovPosInfo`, `.CoverInfo`, `.CoverReference`, `.CoverSlot`, `.DynamicLinkInfo`, `.ExposedLink`, `.FireLink`, `.FireLinkItem`, `.SlotMoveRef`, `Engine.InstancedStaticMeshComponent.InstancedStaticMeshInstanceData`, `Engine.Pylon.PolyReference` |

All other StructFlags values (`0x0` ×231, `0x1` ×535, `0x3` ×5, `0x4` ×5, `0x5` ×22, `0x9` ×14, `0xB` ×3, `0xD` ×2;
848 script structs in total) are tagged.

Binary layout = the struct's property link (next section) member by member in the item encoding, each member
`ArrayDim` times:

- **Members are written own-first, then the super struct's** — CONFIRMED: `Plane` extends `Vector`, and the identity
  matrix default of `Engine.EdCoordSystem` decodes as planes `(W=0,X=1,Y=0,Z=0)`, ..., `(W=1,X=0,Y=0,Z=0)` only with
  `W` before `X, Y, Z` (T). Exact consumption cannot tell own-first from super-first (both consume every payload), so
  the order rests on values. Over every decoded `Matrix` in the install, 3,475 of 3,480 have the affine shape (W
  column 0, 0, 0, 1) when read own-first, and none do when read super-first (T). 47,791 of 91,268 `Plane` values have a
  unit `(X, Y, Z)` normal own-first, against 3,443 super-first (independent re-check, below). Sizes: Vector 12, Rotator 12,
  Color 4 (`B, G, R, A`), LinearColor 16, Guid 16, Plane 16 (`W, X, Y, Z`), Box 25 (`Min, Max, u8 IsValid`),
  Matrix 64.
- **Transient members are written** — CONFIRMED: `Default__CoverLink`'s `CoverSlot` elements are 108 bytes and decode
  exactly only with their `CPF_Transient` members (97 bytes without them). This is the only object in the install that
  tells the two rules apart.
- `CPF_Native` members: UNKNOWN for binary structs. The only binary struct with one is `Pylon.PolyReference`
  (`CachedPoly`), and no package stores a `PolyReference` value (the name occurs only in `Engine.u`) (T). The decoder
  does not skip them. Tagged streams never contain native properties: 0 of the 1,155,815 decoded tags name a
  `CPF_Native` property (T for top-level tags). Script struct defaults also omit the 8 native members that the struct sources assign. Both hint (TENTATIVE)
  that the serializer skips native members, but binary structs are untested.

The fallback layouts in `property::NATIVE_LAYOUTS` (used without a schema) equal the schema's member order (T).

### Property order — CONFIRMED (T)

The property link of a struct or class (UE3 `PropertyLink`) is its own properties in declaration order followed by
its super's link. Class default objects and all other objects store their tags in this order: own class first, then
the parent's, and so on (0 violations in any CDO or object whose class has a script definition).

A CDO stores only values that differ from its super class's defaults. `PackageSet::inherited_defaults` therefore
merges CDOs root first (child overrides parent, tagged structs member-wise; arrays and binary structs replace
wholesale).

What a tagged struct value contains — STRONG (counts over every decoded value, independent re-check):

- When the parent class has no value for the property (the property is declared by the object's own class), the struct
  is written **in full**: 343 of 344 such CDO struct values hold every non-native member.
- Elements of arrays of tagged structs are also written in full: 26,117 of 26,149 elements are complete, and none of
  the 32 missing members has a non-zero struct default. Merging script-struct defaults into array elements is therefore
  not needed for the shipped data.
- When the parent already has a value for the property, the struct is a member-wise **delta** against the parent's
  value: only 27 of 98 such values are complete. The member-wise merge above handles this.
- Map objects follow the same rule against their archetype (class default object or archetype object), so a map
  actor's tagged struct must be merged onto the archetype's value. The API does not do that yet.
- Tagged streams do contain `CPF_Transient` properties: 1,841 tags at any depth, e.g. `ReplicatedCollisionType` in
  `Engine.Default__Actor`. They never contain `CPF_Native` ones (T).

`config` and `globalconfig` values from `.ini` files are not merged. At run time the `.ini` wins: see "Value semantics"
below.

## Script objects — CONFIRMED (T, exact consumption)

All fields little-endian; "obj" = `i32` package index; FName = `i32 index + i32 number`.

```text
UObject        i32 NetIndex | tagged properties (always just "None" here; absent for Class)
UField         obj Next                                     (not TextBuffer)
UStruct        obj SuperStruct | obj ScriptText | obj Children | obj CppText | i32 Line | i32 TextPos
               | i32 ScriptBytecodeSize | i32 ScriptStorageSize | ScriptStorageSize bytes of bytecode
UFunction      UStruct | u16 iNative | u8 OperPrecedence | u32 FunctionFlags
               | u16 RepOffset (FunctionFlags & 0x40) | FName FriendlyName
UState         UStruct | u32 ProbeMask | u16 LabelTableOffset | u32 StateFlags | TMap<FName, obj> FuncMap
UClass         UState | u32 ClassFlags | obj ClassWithin | FName ClassConfigName
               | TMap<FName, obj> ComponentNameToDefaultObjectMap | TArray<{obj Class, obj PointerProperty}> Interfaces
               | TArray<FName> DontSortCategories | TArray<FName> HideCategories
               | TArray<FName> AutoExpandCategories | TArray<FName> AutoCollapseCategories
               | u32 bForceScriptOrder | TArray<FName> ClassGroupNames | FString ClassHeaderFilename
               | FName DLLBindName | obj ClassDefaultObject
UScriptStruct  UStruct | u32 StructFlags | tagged struct defaults
UProperty      UField | i32 ArrayDim | u64 PropertyFlags | FName Category | obj ArrayEnum
               | u16 RepOffset (PropertyFlags & 0x20) | per-type fields:
                 Byte: obj Enum · Object/Component: obj PropertyClass · Class: obj PropertyClass, obj MetaClass
                 Interface: obj InterfaceClass · Struct: obj Struct · Array: obj Inner · Map: obj Key, obj Value
                 Delegate: obj Function, obj SourceDelegate · Int/Float/Bool/Str/Name: nothing
UEnum          UField | TArray<FName> Names
UConst         UField | FString Value
UTextBuffer    UObject | i32 Pos | i32 Top | FString Text
```

Byte-level notes (all CONFIRMED over every export of the kind):

- `ScriptStorageSize` is the bytecode length on disk; `ScriptBytecodeSize` is the in-memory size (object references
  are wider in memory), so it can be larger. Bytecode itself is not decoded yet.
- `SuperStruct` equals the export table's `SuperIndex` for all 2,521 classes, 12,511 functions, 211 states and 848
  script structs. `CppText` is null everywhere. `ScriptText` is set on every class (2,521) and on nothing else.
- `ClassDefaultObject` points at the class's `Default__<Name>` export for all 2,521 classes (case-insensitively:
  `UTGameContent.UTProj_Shockball`'s CDO is `Default__UTProj_ShockBall`).
- The four `TArray<FName>` category lists were told apart by comparing, locally and without recording any source
  text, which list is non-empty with the presence of the matching class-declaration keyword: non-empty lists are
  either declared by the class or inherited from its parent; a declared keyword never comes with an empty list.
  `HideCategories` is non-empty on 1,844 classes, `AutoExpandCategories` on 30, `DontSortCategories` on 9,
  `AutoCollapseCategories` on none; `ClassGroupNames` on 74; `bForceScriptOrder` = 1 on 344. STRONG for the order of
  the four lists (exact consumption cannot distinguish lists that are all empty), CONFIRMED for the layout.
- `ClassHeaderFilename` is set on 1,041 classes and holds the native header group name (e.g. `Scene`, `Mesh`,
  `Texture`). `DLLBindName` is `None` on every class.
- `RepOffset` presence: 284 properties with `CPF_Net`, 273 functions with `FUNC_Net`.
- On the 211 states, `LabelTableOffset` is `0xFFFF` except on 100, and `StateFlags` takes the values 0, 2, 4, 6, 8,
  0xA.

### Children chain order — STRONG

`UStruct::Children` and each field's `Next` form a linked list. Compared, locally, with the order of declarations in
the classes' source (positions only; nothing recorded):

- properties and function parameters are in **declaration order**: properties increase in 842 of 862 classes checked
  (20 mixed, none decreasing); 2,361 multi-parameter signatures match in order and none is reversed (358 could not be
  matched by the plain text search);
- functions, states, enums, consts and nested structs are in **reverse** declaration order: functions decrease in 493
  of 494 classes (1 mixed, none increasing); structs (115 classes), enums (55) and consts (27) always decrease; states
  decrease in 34 classes with 6 exceptions (not investigated; the comparison is a plain text search).

The class model (`PackageSet::class_model`) reports everything in declaration order. The export table lists a
struct's members in reverse of the chain.

## Flag names

Bit values are read from the data (CONFIRMED); names follow UE3 conventions (`crates/asamu-ue3/src/flags.rs`).

| Group | Corroborated bits (STRONG or CONFIRMED) | Everything else |
|---|---|---|
| ObjectFlags | `0x200` ClassDefaultObject (exactly the 2,521 `Default__` objects), `0x0200000000000000` HasStack (selects the state frame, CONFIRMED), `0x400` ArchetypeObject (prefab archetypes) | TENTATIVE |
| ClassFlags | `0x4000` Interface: set on exactly the 44 classes deriving from `Core.Interface` | TENTATIVE |
| FunctionFlags | `0x40` Net (selects RepOffset, CONFIRMED); `0x400` Native (every function with a non-zero `iNative`, e.g. 129 for the boolean `!` operator); `0x1000` Operator; `0x10` PreOperator | TENTATIVE |
| PropertyFlags | `0x20` Net (selects RepOffset, CONFIRMED); `0x80` Parm (function locals only, never on members); `0x400` ReturnParm (exactly the 3,996 `ReturnValue` function locals; the 26 other objects named `ReturnValue` are the inner properties of array return values and lack it) | TENTATIVE |
| StructFlags | `0x20` Immutable, `0x80` ImmutableWhenCooked (select binary form, CONFIRMED) | TENTATIVE |
| StateFlags | — | TENTATIVE |

## Coverage per kind (all packages) — CONFIRMED (T)

| Kind | Exact / total | | Kind | Exact / total |
|---|---:|---|---|---:|
| Class | 2,521 / 2,521 | | IntProperty | 5,896 / 5,896 |
| State | 211 / 211 | | FloatProperty | 7,009 / 7,009 |
| Function | 12,511 / 12,511 | | BoolProperty | 8,349 / 8,349 |
| ScriptStruct | 848 / 848 | | StrProperty | 4,745 / 4,745 |
| Enum | 379 / 379 | | NameProperty | 1,674 / 1,674 |
| Const | 614 / 614 | | ObjectProperty | 9,750 / 9,750 |
| TextBuffer | 2,521 / 2,521 | | ClassProperty | 690 / 690 |
| ByteProperty | 1,985 / 1,985 | | ComponentProperty | 630 / 630 |
| StructProperty | 7,349 / 7,349 | | InterfaceProperty | 113 / 113 |
| ArrayProperty | 2,339 / 2,339 | | MapProperty | 45 / 45 |
| DelegateProperty | 767 / 767 | | **Total** | **70,946 / 70,946** |

## Coverage per package — CONFIRMED

Script objects and CDOs consume exactly in every package (all rows "x / x"). "Other objects" = prelude + tagged
properties decoded / ending exactly at `SerialSize` (the rest have native data after the tags).

| Package | Script objects | CDOs | Other objects decoded / exact |
|---|---:|---:|---:|
| `Core.u` | 1,532 | 9 | 1 / 0 |
| `Engine.u` | 31,023 | 1,343 | 1,077 / 861 |
| `GameFramework.u` | 3,661 | 105 | 45 / 36 |
| `UDKBase.u` | 2,607 | 120 | 154 / 108 |
| `IpDrv.u` | 6,757 | 79 | 5 / 4 |
| `GFxUI.u` | 743 | 17 | 1 / 0 |
| `GFxUIEditor.u` | 21 | 4 | 1 / 0 |
| `OnlineSubsystemSteamworks.u` | 1,303 | 4 | 1 / 0 |
| `UTEditor.u` | 4 | 2 | 0 / 0 |
| `UTGameContent.u` | 707 | 56 | 241 / 213 |
| `UnrealEd.u` | 1,836 | 195 | 23 / 9 |
| `WinDrv.u` | 121 | 4 | 1 / 0 |
| `Startup.upk` (`asamu` + `UTGame` + content) | 20,631 | 583 | 15,969 / 14,030 |
| 8 other `.upk` in CookedMac (LOC, front end, caches) | 0 | 0 | 343 / 39 |
| 12 maps (`.asamu`) | 0 | 0 | 111,104 / 71,827 |
| 9 map `_LOC_INT.upk` | 0 | 0 | 252 / 65 |

Per-package script-kind counts: `Startup.upk` 583 classes, 4,337 functions, 152 states, 53 structs, 28 enums, 465
consts, 14,430 properties; `Engine.u` 1,343 / 4,985 / 36 / 474 / 274 / 97 / 22,471; `Core.u` 9 / 308 / 0 / 54 / 11 /
15 / 1,126 (`asamu-inspect --json coverage` prints every package).

## Class model and defaults API

- `PackageSet::new(&[cooked_dir, cooked_dir/Maps])` opens packages by name on demand. A package with no file of its
  own (`asamu`, `UTGame`) is looked up inside `Startup*.upk`. Qualified paths: imports keep their package; exports are
  prefixed with their file's name unless their outermost object is a `Package` export.
- `class_model(path)`: super chain (across packages), flags, within/config/categories/header, interfaces, components,
  properties (type text, flags, array dim, category, rep offset, struct/enum/class references), functions (flags,
  native index, operator precedence, friendly name, parameters with out/optional/coerce, return type, locals),
  states with their functions, enums, consts and structs (members and defaults).
- `class_defaults(path)` / `inherited_defaults(path)`: CDO tags, and their merge across the super chain with the
  source class of each value. Example (T): `asamu.ASAMUPawn` merges 7 default objects from 5 packages; `JumpZ` comes
  from `asamu.ASAMUPawn`, `GroundSpeed` from `UTGame.UTPawn`, `CustomGravityScaling` from `UDKBase.UDKPawn`.
- `asamu-inspect scripttext` writes a class's `ScriptText` only to an explicit local path outside the repository or
  under a git-ignored `research/` subdirectory, refuses the install and symlinks, and warns that the content is
  copyrighted. **Never commit, quote or paraphrase it.**
- Every object-taking subcommand (`class`, `defaults`, `props`, `scripttext`) also accepts `#N` for export `N`
  (0-based, as in the `export_index` JSON fields). Use it for the 8 qualified paths in the maps that two exports share
  (same name and outer, different class: a material or mesh and a texture). A path lookup returns the first export.

## Value semantics consumers must know

- **Class defaults exist only in the CDOs** — CONFIRMED (T). None of the 2,521 shipped `ScriptText` buffers contains
  a `defaultproperties` block outside comments; cooking strips them. 61 buffers keep `structdefaultproperties` blocks.
  The decoded script-struct defaults agree with those blocks on 394 of 416 parsed assignments (checked locally, no
  text recorded). The other 22:
  - 11 are in `Engine.LensFlare.LensFlareElement`, whose cooked struct defaults are all zero, including the subobject
    references of its distribution members (cause UNKNOWN).
  - 8 assign `CPF_Native` members, which are never serialized.
  - 1 is an integer literal outside the `int32` range, stored as `-1`.
  - 1 assigns an empty string to a name, stored as `None`.
  - 1 assigns an enum from a string and was not compared.
- **Enum values named `None`** — CONFIRMED in the data (T), cause TENTATIVE. 853 enum-typed tag values hold the FName
  `None`, which is not among the enum's names: 529 in the script packages and 324 elsewhere. By property:
  - 388 are `SoundCue.SoundClassName`. The cooked `ESoundClassName` lists only `Master` and the generated `_MAX`
    entry, so a cue's sound class cannot be recovered from this property.
  - 462 are `InterpMethod` members of `InterpCurveVector` (332) and `InterpCurveFloat` (130) values.
  - 3 are single CDO values: `Actor.ReplicatedCollisionType`, `WorldInfo.LevelLightingQuality` and
    `EditorEngine.DetailMode`.

  The decoder reports them as `Value::Enum("None")`. Treat that value as "byte value not representable in the cooked
  enum", not as an enumerator. The likely cause is that UE3 writes `None` for a value outside the enum's real entries.
- **`config` properties hold a cook-time snapshot** — STRONG. Shipped `.ini` keys (Engine `Base*.ini`, ASAMU
  `Default*.ini` and `Mac/*.ini`) were compared with the merged CDO values of the 581 config properties that have a
  key in their class's section:
  - 490 are equal: 436 scalars, and 54 structs compared member by member.
  - 2 more are equal after normalisation: an enum written as its number, and a quoted string with a trailing `;`.
  - 74 keys set a zero value that the CDO omits.
  - 12 keys have no CDO value at all, e.g. the `IpDrv` web server and `DebugCameraController` keys.
  - 2 differ: `Engine.Console.ConsoleKey` and `TypeKey` are `Tab` and `Tilde` in the CDO, but `Tilde` and `Tab` in
    `BaseInput.ini` and `None` in `DefaultInput.ini`.
  - 1 editor option differs.

  At run time `LoadConfig` overrides the CDO, so config values must come from the merged `.ini`, not from the CDO.
- **Tagged struct deltas**: see "Property order" above. Values of inherited properties, and of map objects against
  their archetype, must be merged member-wise.

## Independent re-check (2026-10-09) — CONFIRMED

A second decoder was written from scratch in Python. It lives in ignored `research/local/verify-objects/` and is never
committed. It does its own summary and table parsing of the streams from `asamu-inspect decompress`, and its own
script-object, prelude and tagged-property decoding. Unlike the Rust decoder it is strict: there is no raw fallback,
every tag must name a declared property of the right type and static-array index, and an enum-typed byte is decided
by the declaration, not by the tag. Results:

- Script objects: 70,946 / 70,946 consume exactly, with the same per-kind counts.
- Class default objects: 2,521 / 2,521 consume exactly with strict values.
- The other 129,218 exports: all preludes and tag streams decode strictly. 87,192 end at `SerialSize` and 42,026 have a
  native tail. The totals equal the Rust coverage for the script packages (17,519 decoded, 15,261 exact) and for the
  other 29 packages (111,699 decoded, 71,931 exact). In total 1,155,815 tags were decoded (150,807 + 1,005,008).
- Value agreement with the Rust output:
  - Every CDO matches `asamu-inspect --json defaults`: 11,836 top-level tags with all nested values, float bit
    patterns and object paths.
  - Every class model matches `asamu-inspect --json class`: 2,521 classes, 15,856 class properties, 12,511 functions
    (with 888 state functions) including flags, native index, operator precedence, friendly name, rep offset, super
    function, parameters and locals, plus 211 states, 379 enums, 614 consts and 848 structs.
  - 14,013 map and other objects match `asamu-inspect --json props` on prelude, NetIndex, state frame, component
    template, shadow-map prefix, tag stream end and values. They are every 8th export plus every object with a
    shadow-map prefix or a non-null template owner class, covering 3,783 state frames, 5,217 components and 82,538
    top-level tags.
- Discriminating power: each alternative rule makes the strict decoder fail on real objects.
  - Skipping transient members of binary structs: 1 failure (`Default__CoverLink`).
  - Enum items as one byte: 13 failures.
  - Ignoring `0x80` (ImmutableWhenCooked): 4.
  - Binary members in reverse link order: 4.
  - A `TemplateName` also under archetypes: 44.
  - No shadow-map prefix: 9, which is every non-CDO dominant-light component.
  - Super-first binary members cause no failure; the matrix and plane statistics above decide that case.
- Every count in the "Byte-level notes" and "Flag names" sections was recomputed and agrees. The `ReturnValue` wording
  is now precise, and three corroborations were added:
  - `CPF_Parm` occurs only on function children (20,802).
  - The 125 functions with `0x1000` (Operator) are the 122 with symbolic friendly names plus 3 word operators.
  - All 9 functions with `0x10` (PreOperator) are operators.

## Hostile-input discipline

Every count is checked against the remaining bytes before allocating, and array storage reserved up front is capped
at 4,096 elements (`property::MAX_PREALLOC`). Tag sizes are bounded by the payload; nested values stop at depth 32;
children chains, super chains and inner-property chains are cycle- and length-limited. Each payload decode has a
work budget of 8 decoded values per payload byte plus 65,536 (`property::work_budget_for`); real data needs at most
0.9998 values per byte (a byte array of 1,095,848 elements). The budget bounds schema-driven fan-out, which the depth limit alone does not: zero-byte
binary struct members with a huge `ArrayDim`, or structs nested 30 deep that each repeat a member 16 times.

`tests/objects_hostile.rs` does the following, with no panics and no hangs:

- truncates every synthetic payload at every offset (script objects must be rejected);
- flips bits and writes extreme `i32` values at every other offset;
- builds children and super cycles;
- nests struct tags 200 deep and feeds 3,000 random tag streams;
- runs the fan-out attacks above through a hand-written `Schema`.

`tests/objects.rs` checks every decoder against hand-written payloads.

## UNKNOWN / not yet done

- Script bytecode (decoding the `ScriptStorageSize` bytes) — next step for behaviour work.
- Class-specific native data after the tags (42,026 objects: textures, meshes, levels, sounds, ...).
- `StateStack` element layout (never non-empty); meaning of the state frame's `u16` and of `NetIndex`.
- Whether binary structs skip `CPF_Native` members (never exercised; tagged streams do skip them).
- `config`/`globalconfig` values from `.ini` files are not merged into defaults (the run-time value comes from the
  `.ini`); map objects are not merged onto their archetypes.
- Why `LensFlareElement`'s cooked struct defaults are zero, and the exact rule behind `None` enum values.
