# UnrealScript bytecode (UE3 v868)

Decoding of the script bytecode that every `UStruct` export stores after its header (`ScriptStorageSize` bytes,
see [OBJECT_FORMAT.md](OBJECT_FORMAT.md)). This page documents the token set, operand encodings, offset rules and
coverage. It contains **no disassembly of game code**: listings are the original game's logic and stay local
(`asamu-inspect disasm`). Token names, operand layouts, counts and the names of called engine functions are
format facts and are published here.

Evidence source: the 12 `.u` packages and `Startup.upk` in `~/Library/Application Support/Steam/steamapps/common/A Story About My Uncle/A Story About My Uncle.app/Contents/Resources/ASAMU/CookedMac`
of the legitimately owned Mac install (Steam build 1822049), read-only, decoded by our own code in
`crates/asamu-ue3/src/bytecode.rs`. Token numbers come from the fixed `GNatives` registrations recovered from the
executable ([BINARY_ANALYSIS.md](BINARY_ANALYSIS.md) §6); operand encodings were fixed empirically against the
data.

`(T)` marks a number asserted by `crates/asamu-ue3/tests/bytecode_real_data.rs` against the install.

## Result — CONFIRMED (T)

| Kind | Exports | With bytecode | Decoded exactly | Memory size = `ScriptBytecodeSize` | Targets and skips valid | Ends with `EndOfScript` |
|---|---:|---:|---:|---:|---:|---:|
| Function | 12,511 | 12,511 | 12,511 | 12,511 | 12,511 | 12,511 |
| State | 211 | 211 | 211 | 211 | 211 | 211 |
| Class | 2,521 | 79 | 79 | 79 | 79 | 79 |
| ScriptStruct | 848 | 0 | — | — | — | — |
| **Total** | 16,091 | **12,801** | **12,801** | **12,801** | **12,801** | **12,801** |

- 1,749,984 storage bytes decode to 417,163 expression nodes; the memory total is 2,508,276 bytes (T).
- 23,533 absolute code targets (jumps, cases, loop ends, labels): **0** miss a token boundary, and every one of
  them points at a top-level statement start (T).
- 39,146 relative skips: **0** disagree with the memory size of what they skip, under the rules in
  [Relative skips](#relative-skips) (T).
- Coverage is 100%: no struct with bytecode fails, in any package (T). The maps and the other `.upk` files hold no
  script objects.

Per package (structs with bytecode / decoded exactly): `Core.u` 308/308, `Engine.u` 5,060/5,060,
`GameFramework.u` 593/593, `UDKBase.u` 393/393, `IpDrv.u` 1,194/1,194, `GFxUI.u` 171/171, `GFxUIEditor.u` 2/2,
`OnlineSubsystemSteamworks.u` 291/291, `UTEditor.u` 0/0, `UTGameContent.u` 168/168, `UnrealEd.u` 77/77,
`WinDrv.u` 35/35, `Startup.upk` 4,509/4,509.

Exact consumption alone cannot prove the tree shape: an operand-count error can still consume every byte if a
later statement absorbs the difference. That happened during this work (see [History](#history)). The
following independent cross-checks therefore all hold as well (T):

| Check | Result |
|---|---|
| Argument count of every `FinalFunction` call = parameter count of the called function (resolved across packages) | 8,233 / 8,233 |
| Argument count of every native-token call = parameter count of the script function with that `iNative` | 46,160 / 46,160 |
| Every native token resolves to a script function with that `iNative` | 46,160 / 46,160 (202 indices, 0 conflicts) |
| Object operands whose referenced class fits the operand's role (property, function, class, struct; see `OperandRole::accepts`) | 189,573 / 189,573 |
| `LocalVariable`/`LocalOutVariable`/`NativeParm`/`ReturnNothing` properties whose outer is the function being decoded | 68,500 / 68,500 |
| Contexts over a plain variable whose r-value property is that variable | 16,316 / 16,316 |
| Top-level statements that are bare literals or terminators | 0 |
| Property `RepOffset`s (CPF_Net) that land on a statement of their class's bytecode | 284 / 284 |
| State/class `LabelTableOffset` (not `0xFFFF`) = memory offset of the first entry after a `LabelTable` token | 100 / 100 |
| Label-table terminators named `None` | 100 / 100 |

## Tooling and reproduction

```sh
export CARGO_TARGET_DIR=target/agents
C="$HOME/Library/Application Support/Steam/steamapps/common/A Story About My Uncle/A Story About My Uncle.app/Contents/Resources/ASAMU/CookedMac"
cargo run --release -p asamu-inspect -- bytecode-coverage "$C"                     # every number on this page
cargo run --release -p asamu-inspect -- --json bytecode-coverage "$C"
cargo run --release -p asamu-inspect -- calls "$C/Startup.upk" asamu.ASAMUPawn     # calls + constants per function
cargo run --release -p asamu-inspect -- disasm "$C/Startup.upk" asamu.ASAMUPawn.PostBeginPlay   # LOCAL ONLY
cargo test -p asamu-ue3 --test bytecode --test bytecode_real_data                  # synthetic + gated real data
```

- `disasm FILE OBJECT [--json]` prints the token tree of one function, state or class, with storage and memory
  offsets, resolved object paths, names and native functions, plus the validation summary. **Its output is the
  original game's code: keep it local, never commit, paste or paraphrase it.** The command prints this warning
  on stderr.
- `calls FILE CLASS [--json]` lists, for every function and state of a class, the functions it calls (final,
  virtual, global, delegate), the native functions it calls, and the float, int and name constants it uses. It
  prints identifiers and numbers only, which is what evidence notes need (for example "which natives does the
  grapple state call, and with which constants").
- Library API (`asamu_ue3::bytecode`): `decode(bytes, Layout)` → `Script` (a tree of `Expr { token, offset,
  size, mem_offset, mem_size, kind: ExprKind }`), `Script::validate`, `Script::references`,
  `Script::object_operands`, `export_bytecode`, `NativeTable`, `function_parameter_count`,
  `check_call_arity`, `package_bytecode_coverage`.

## Storage and memory sizes — CONFIRMED (T)

`ScriptStorageSize` counts the bytes on disk. `ScriptBytecodeSize` is the size after loading. They differ only in
object operands:

| Operand | Storage | Memory | Evidence |
|---|---:|---:|---|
| object reference (`UObject*`: property, function, class, struct, constant) | 4 (package index) | **8** | the memory totals equal `ScriptBytecodeSize` for all 12,801 scripts. With 4-byte references only 2,538 match, which are exactly the scripts without any object operand |
| name (`FName` = `i32` index + `i32` number) | 8 | 8 | same totals |
| everything else (bytes, `u16` offsets, `i32`, `f32`, strings) | same | same | same totals |

The 8-byte pointers show that the shipped scripts were cooked for a 64-bit build. **Every code offset stored in the
bytecode counts memory bytes**: jump targets, case links, loop ends, label offsets, relative skips, `RepOffset`
and `LabelTableOffset`. The decoder tracks both coordinates for every node (`Layout::SHIPPED`).

## Token set (v868)

Notation: `obj` = object reference (4 storage / 8 memory bytes); `name` = FName (8); `u8`/`u16`/`i32`/`f32` =
little-endian scalars; `code` = `u16` absolute memory offset; `skip` = `u16` memory byte count; `expr` = a nested
expression; `args` = expressions up to and including an `EndFunctionParms` (0x16) byte.

"Handler" says how the number was established. **reg**: a `UObject::exec<Name>` registered at that `GNatives`
slot (CONFIRMED from the executable, 80 slots below 0x60). **inline**: no registration, because the VM handles the
token inside another handler; the number and its role are confirmed by the data. Counts are expression nodes in
the shipped data (T for the total; per-token counts printed by `bytecode-coverage`).

| Hex | Token | Handler | Operands after the token byte | Count | Confidence |
|---|---|---|---|---:|---|
| 00 | LocalVariable | reg | `obj` property | 59,919 | CONFIRMED |
| 01 | InstanceVariable | reg | `obj` property | 56,608 | CONFIRMED |
| 02 | DefaultVariable | reg | `obj` property | 1,187 | CONFIRMED |
| 03 | StateVariable | reg | `obj` property | 137 | CONFIRMED |
| 04 | Return | inline | `expr` value (`Nothing` for a plain return) | 15,066 | CONFIRMED |
| 05 | Switch | reg | `obj` property (may be null), `u8` value size, `expr` | 167 | CONFIRMED |
| 06 | Jump | reg | `code` | 6,040 | CONFIRMED |
| 07 | JumpIfNot | reg | `code`, `expr` condition | 15,743 | CONFIRMED |
| 08 | Stop | reg | — | 211 | CONFIRMED |
| 09 | Assert | reg | `u16` line, `u8` flag (0 in all 8), `expr` | 8 | CONFIRMED |
| 0A | Case | reg | `code` next case; `0xFFFF` = `default:` with no expression, otherwise `expr` value | 1,355 | CONFIRMED |
| 0B | Nothing | reg | — | 13,005 | CONFIRMED |
| 0C | LabelTable | inline | entries `{name, u32 code}` until an entry with code `0xFFFF` (named `None`) | 100 | CONFIRMED |
| 0D | GotoLabel | reg | `expr` label name | 34 | CONFIRMED |
| 0E | EatReturnValue | reg | `obj` return property, `expr` call | 318 | CONFIRMED (layout); STRONG (call as operand, see below) |
| 0F | Let | reg | `expr` target, `expr` value | 16,478 | CONFIRMED |
| 10 | DynArrayElement | reg | `expr` index, `expr` array | 5,148 | CONFIRMED |
| 11 | New | reg | 5 × `expr` (outer, name, flags, class, template) | 212 | CONFIRMED |
| 12 | ClassContext | reg | as `Context` | 1,349 | CONFIRMED |
| 13 | MetaCast | reg | `obj` class, `expr` | 122 | CONFIRMED |
| 14 | LetBool | reg | `expr` target, `expr` value | 2,993 | CONFIRMED |
| 15 | EndParmValue | inline | — (closes `DefaultParmValue`) | 478 | CONFIRMED |
| 16 | EndFunctionParms | reg | — (closes argument lists and the dynamic-array operations below) | — | CONFIRMED |
| 17 | Self | reg | — | 1,105 | CONFIRMED |
| 18 | Skip | inline | `skip`, `expr` | 5,816 | CONFIRMED |
| 19 | Context | reg | `expr` object, `skip`, `obj` r-value property (may be null), `u8` r-value size, `expr` member | 29,849 | CONFIRMED |
| 1A | ArrayElement | reg | `expr` index, `expr` array | 1,044 | CONFIRMED |
| 1B | VirtualFunction | reg | `name` function, `args` | 13,815 | CONFIRMED |
| 1C | FinalFunction | reg | `obj` function, `args` | 8,233 | CONFIRMED |
| 1D | IntConst | reg | `i32` | 1,493 | CONFIRMED |
| 1E | FloatConst | reg | `f32` | 4,999 | CONFIRMED |
| 1F | StringConst | reg | bytes up to and including a NUL (Latin-1) | 8,791 | CONFIRMED |
| 20 | ObjectConst | reg | `obj` | 1,931 | CONFIRMED |
| 21 | NameConst | reg | `name` | 3,085 | CONFIRMED |
| 22 | RotationConst | reg | 3 × `i32` (pitch, yaw, roll) | 45 | CONFIRMED |
| 23 | VectorConst | reg | 3 × `f32` | 436 | CONFIRMED |
| 24 | ByteConst | reg | `u8` | 2,944 | CONFIRMED |
| 25 | IntZero | reg | — | 3,699 | CONFIRMED |
| 26 | IntOne | reg | — | 1,802 | CONFIRMED |
| 27 | True | reg | — | 3,361 | CONFIRMED |
| 28 | False | reg | — | 3,155 | CONFIRMED |
| 29 | NativeParm | reg | `obj` parameter | 3,982 | CONFIRMED |
| 2A | NoObject | reg | — | 7,679 | CONFIRMED |
| 2C | IntConstByte | reg | `u8` | 2,252 | CONFIRMED |
| 2D | BoolVariable | reg | `expr` variable | 9,117 | CONFIRMED |
| 2E | DynamicCast | reg | `obj` class, `expr` | 4,080 | CONFIRMED |
| 2F | Iterator | reg | `expr` iterator call, `code` loop end | 394 | CONFIRMED |
| 30 | IteratorPop | reg | — | 492 | CONFIRMED |
| 31 | IteratorNext | inline | — | 450 | CONFIRMED |
| 32 | StructCmpEq | reg | `obj` struct, `expr`, `expr` | 37 | CONFIRMED |
| 33 | StructCmpNe | reg | `obj` struct, `expr`, `expr` | 21 | CONFIRMED |
| 34 | UnicodeStringConst | reg | UTF-16LE units up to and including a 0 unit | 2 | CONFIRMED |
| 35 | StructMember | reg | `obj` property, `obj` struct, `u8` copy flag, `u8` modified flag, `expr` struct value | 7,980 | CONFIRMED (layout); flag meanings TENTATIVE |
| 36 | DynArrayLength | reg | `expr` array | 2,321 | CONFIRMED |
| 37 | GlobalFunction | reg | `name` function, `args` | 72 | CONFIRMED |
| 38 | PrimitiveCast | reg | `u8` cast index, `expr` | 8,577 | CONFIRMED |
| 39 | DynArrayInsert | reg | `expr` array, `expr` index, `expr` count, `0x16` | 18 | CONFIRMED |
| 3A | ReturnNothing | reg | `obj` return property | 2,644 | CONFIRMED |
| 3B | EqualEqual_DelDel | reg | `args` | 4 | CONFIRMED |
| 3C | NotEqual_DelDel | reg | `args` | 27 | CONFIRMED |
| 3D | EqualEqual_DelFunc | reg | `args` | 2 | CONFIRMED |
| 3E | NotEqual_DelFunc | reg | `args` | 0 | TENTATIVE (not in the data; same form as 3B–3D) |
| 3F | EmptyDelegate | reg | — | 31 | CONFIRMED |
| 40 | DynArrayRemove | reg | `expr` array, `expr` index, `expr` count, `0x16` | 263 | CONFIRMED |
| 41 | DebugInfo | reg | `i32` version, `i32` line, `i32` position, `u8` opcode | 0 | TENTATIVE (UE3 convention; cooked scripts carry no debug info) |
| 42 | DelegateFunction | reg | `u8` local flag, `obj` delegate property, `name` function, `args` | 207 | CONFIRMED |
| 43 | DelegateProperty | reg | `name` function, `obj` delegate property (null in 592 of 604) | 604 | CONFIRMED |
| 44 | LetDelegate | reg | `expr` target, `expr` value | 252 | CONFIRMED |
| 45 | Conditional | reg | `expr` condition, `skip`, `expr` true, `skip`, `expr` false | 461 | CONFIRMED |
| 46 | DynArrayFind | reg | `expr` array, `skip`, `expr` value, `0x16` | 226 | CONFIRMED |
| 47 | DynArrayFindStruct | reg | `expr` array, `skip`, `expr` member name, `expr` value, `0x16` | 273 | CONFIRMED |
| 48 | LocalOutVariable | reg | `obj` property | 1,955 | CONFIRMED |
| 49 | DefaultParmValue | reg | `skip`, `expr` value, `0x15` | 478 | CONFIRMED |
| 4A | EmptyParmValue | reg | — (an omitted optional argument) | 10,403 | CONFIRMED |
| 4B | InstanceDelegate | reg | `name` function | 2 | CONFIRMED |
| 51 | InterfaceContext | reg | `expr` | 553 | CONFIRMED |
| 52 | InterfaceCast | reg | `obj` interface class, `expr` | 263 | CONFIRMED |
| 53 | EndOfScript | reg | — | 12,801 | CONFIRMED |
| 54 | DynArrayAdd | reg | `expr` array, `expr` count, `0x16` | 4 | CONFIRMED |
| 55 | DynArrayAddItem | reg | `expr` array, `skip`, `expr` item, `0x16` | 205 | CONFIRMED |
| 56 | DynArrayRemoveItem | reg | `expr` array, `skip`, `expr` item, `0x16` | 19 | CONFIRMED |
| 57 | DynArrayInsertItem | reg | `expr` array, `skip`, `expr` index, `expr` item, `0x16` | 7 | CONFIRMED |
| 58 | DynArrayIterator | reg | `expr` array, `expr` item variable, `u8` has-index, `expr` index (`EmptyParmValue` when absent), `code` loop end | 40 | CONFIRMED |
| 59 | DynArraySort | reg | `expr` array, `skip`, `expr` comparator delegate, `0x16` | 2 | CONFIRMED |
| 5A | JumpIfNotEditorOnly | reg | `code` | 0 | TENTATIVE (not in the data) |
| 60–6F | native, two-byte | reg (`execHighNative0…15`) | `u8` low byte; index = `(token − 0x60) << 8 | low`; `args` | 2,074 | CONFIRMED |
| 70–FF | native, one-byte | reg | index = token; `args` | 44,086 | CONFIRMED |

Unassigned below 0x60: `0x2B`, `0x4C`–`0x50` and `0x5B`–`0x5F`. They have no registered handler and never occur.
The decoder rejects them (`UnknownToken`).

Notes:

- **Names.** Registered tokens take the name of their `exec` handler symbol. The five inline tokens (04, 0C, 15,
  18, 31) take UE3's names, and their role in the data agrees with those names. `Return` wraps a value. `LabelTable`
  holds state labels. `EndParmValue` closes every `DefaultParmValue`. `Skip` occurs only as the short-circuit
  operand of `&&` and `||`. `IteratorNext` ends loop bodies. STRONG for the names, CONFIRMED for the numbers and
  layouts.
- **Natives.** The two-byte prefixes in use are 0x61, 0x62, 0x63, 0x65 and 0x6F. The calls use 170 distinct
  native indices, from 112 to 3,971 (0xF83). All of them resolve to the 202 functions that carry an `iNative`, with
  no index claimed twice. This agrees with BINARY_ANALYSIS §6.
- **`PrimitiveCast`** operand bytes span 0x36–0x60 (28 distinct values). These are exactly the `GCasts` slots that
  the executable registers (BINARY_ANALYSIS §6.1), so this is CONFIRMED by two independent sources.
- **Dynamic-array operations end with `EndFunctionParms`.** `DynArrayAdd`, `AddItem`, `Insert`, `InsertItem`,
  `Remove`, `RemoveItem`, `Find`, `FindStruct` and `Sort` are always followed by a `0x16` byte (1,017 of 1,017),
  and the decoder consumes it as part of the token. `DynArrayLength` and `DynArrayIterator` have no such byte.
- **`EatReturnValue`** holds the return property of the call it discards: all 318 name a `ReturnValue` property.
  The call itself is decoded as its operand, because the VM evaluates the call into a buffer sized by that
  property. Byte consumption is the same either way, so the operand reading is STRONG, not CONFIRMED.
- **`Context` r-value property** (the slot the VM zeroes when the context object is `None`). It is the member
  variable itself for variable contexts (16,316 / 16,316), and the callee's `ReturnValue` or null (void calls)
  for call contexts. One context uses a `Const` (a constant read through an object). The `u8` after it is 0 in
  31,192 contexts and 7 in 6 (delegate-property contexts). Its meaning is TENTATIVE (a size or type code).
- **`StructMember`** flag pairs (copy, modified): (0,0) 5,493, (0,1) 2,464, (1,0) 23. The property's outer is the
  struct in 7,687 of 7,980 cases. In the rest the member is inherited from a super struct.
- **`DelegateFunction`** local flag: 0 in 179, 1 in 28.

## Absolute targets — CONFIRMED (T)

`Jump`, `JumpIfNot`, `Case` (non-default), `Iterator` and `DynArrayIterator` loop ends, and label-table entries
are `u16`/`u32` **memory** offsets from the start of the script. All 23,533 land on token starts that are
top-level statements. Loop ends point at the loop's `IteratorPop` (394 / 394 `Iterator`, 40 / 40
`DynArrayIterator`). `JumpIfNotEditorOnly` would be checked the same way, but it does not occur.

## Relative skips — CONFIRMED (T)

| Token | Rule (memory bytes) | Count |
|---|---|---:|
| `Context` / `ClassContext` | skip = size of the member expression | 30,727 |
| … that produce a `foreach` iterator or array (operand of `Iterator`/`DynArrayIterator`) | the skip lands one byte past the loop end, after the `IteratorPop` | 227 |
| … whose object is the `Outer` instance variable | skip = 9 = size of the **object** expression (see below) | 244 |
| `Skip` | size of its expression + 1 (the `EndFunctionParms` of the call it is the last argument of). Occurs only in `Object.AndAnd_BoolBool` (4,256) and `Object.OrOr_BoolBool` (1,560) | 5,816 |
| `Conditional` | each skip = size of the branch that follows it | 922 |
| `DynArrayAddItem`/`RemoveItem`/`InsertItem`/`Find`/`FindStruct`/`Sort` | size of the operands after the skip + 1 (the closing `0x16`) | 732 |
| `DefaultParmValue` | size of the value + 1 (the `EndParmValue`) | 478 |

The `Outer` exception holds for all 244 contexts on `Outer` whose skip does not cover the member expression. The
other 661 contexts on `Outer` follow the normal rule. A likely explanation is that the compiler writes the wrong
size when it inserts the implicit `Outer.` for members of a `within` class (for example cheat managers inside
their player controller). That explanation is TENTATIVE. The skip only matters when `Outer` is `None`, which a
`within` object never is.

## Script shapes — CONFIRMED

- **Functions** (12,511): every function has bytecode, including natives. Native functions hold
  `NativeParm` statements that name their own parameters (3,982 in all), or only `Nothing`. There are 478
  `DefaultParmValue` statements; 205 script and 106 native functions begin with one. Omitted arguments are
  explicit `EmptyParmValue` tokens, which is why argument counts equal parameter counts.
- **States** (211): state code ends with `EndOfScript`. Each of the 100 states with labels holds one
  `LabelTable`. In 38 of them a `Stop` statement comes directly before the table; others have `Nothing`
  padding in between. `LabelTableOffset` points at the table's first entry, 100 / 100. Entries are 12 bytes (`name`, `u32` offset), and the
  terminator has offset `0xFFFF` and name `None`.
- **Classes** (79 with bytecode): the class bytecode holds the replication conditions, which are 39 `BoolVariable`
  and 84 native-call statements, followed by `EndOfScript`. Every property `RepOffset` points at one of these
  statements (284 / 284). The `RepOffset` of every `FUNC_Net` function is `0xFFFF` (273 / 273): function
  replication does not use class bytecode.
- No `DebugInfo` tokens: cooked scripts carry no debugger information. The deepest nesting is 32 levels.

## Hostile-input discipline

`decode` never panics on malformed input. Every read is bounds-checked against the script. Offsets and memory
positions use checked arithmetic. Each node consumes at least one byte, so the node count is bounded by the input
length. Nesting deeper than `MAX_DEPTH` (128) is rejected before recursing further. Strings must be terminated
inside the script, and terminators (`0x15`/`0x16`) are rejected where an operand is required.
`tests/bytecode.rs` (13 tests, all synthetic, written byte by byte) covers the following:

- an encoding case for every named token below 0x60 and for both native forms, with storage and memory sizes;
- memory offsets under 8-byte and 4-byte object references;
- every target and skip rule, accepted when right and reported when wrong;
- structural errors: unknown tokens, misplaced terminators, missing `EndParmValue`/`EndFunctionParms`,
  unterminated strings and label tables;
- the nesting limit, including 100,000-deep inputs;
- truncation of a stream holding every token at every offset, and hostile byte replacement at every offset;
- 20,000 random streams;
- reference summaries and operand roles;
- package-level coverage, natives and arity on a synthetic script package.

## History

- The first decoding consumed every function exactly, but it treated the dynamic-array operations as ending after
  their last operand. The trailing `0x16` then closed the *enclosing* call early, and the leftover argument became
  a bogus top-level statement: 1,017 top-level `EndFunctionParms` and 108 bare `IntConst` statements. Checking
  the top-level statement kinds exposed it. Measuring the byte after each operation fixed it: always `0x16` for
  the nine operations listed above. This is why the cross-checks (arity, top-level shapes) are part of the
  acceptance test.
- The label-table terminator was first assumed to be name index 0, which failed on the 100 states with labels.
  `None` is not entry 0 of the name table (in `Engine.u` it is name 13,229), so the terminator is recognised by
  its `0xFFFF` offset, and its name is then checked to be `None`.

## UNKNOWN / not done

- Meaning of the `Context` size byte (0 or 7), of the `StructMember` copy/modified flags and of the
  `DelegateFunction` local flag beyond what their names suggest (TENTATIVE).
- `NotEqual_DelFunc`, `DebugInfo` and `JumpIfNotEditorOnly` do not occur in the data. Their layouts follow UE3
  convention and are only tested synthetically (TENTATIVE).
- Argument counts of `VirtualFunction`/`GlobalFunction`/`DelegateFunction` calls are not checked, because they
  need run-time name resolution through the receiver's class.
- Nothing executes bytecode. Behaviour work reads it locally and re-implements behaviour in our own words and
  code (CLAUDE.md hygiene).
