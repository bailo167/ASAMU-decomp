# UE3 package analysis

## CONFIRMED (header bytes of all 42 `.u`/`.upk`/`.asamu` files)

| Offset | Size | Field | Observed |
|---|---|---|---|
| 0x00 | u32 | Tag | `0x9E2A83C1` (bytes `C1 83 2A 9E`) in every package |
| 0x04 | u16 | FileVersion | 868 (`0x0364`) in every package |
| 0x06 | u16 | LicenseeVersion | 0 in every package |
| 0x08 | i32 | TotalHeaderSize | varies per package |
| 0x0C | FString | FolderName | `"None"` (length 5 incl. NUL) in every package |
| 0x15 | u32 | PackageFlags | varies; e.g. `0x22A80008` (Core.u), `0x0288000D` (Startup.upk), `0x228A0009` (maps) |
| 0x19 | i32 | NameCount | e.g. 827 (Core.u), 17,658 (Startup.upk) |

## Pending (to be confirmed by `crates/asamu-ue3`)

- Remaining summary: name offset, export/import counts+offsets, depends offset, import/export GUID data,
  thumbnail table, package GUID, generations, engine version, cooker version, compression flags, compressed chunk
  table, package source, additional packages to cook, texture allocations.
- Previous session reported CompressionFlags = 2 for packages it examined and believed 2 = LZO. In UE3,
  `COMPRESS_ZLIB = 0x01`, `COMPRESS_LZO = 0x02`, `COMPRESS_LZX = 0x04`. **TENTATIVE** until the field is parsed at
  the correct offset for every package and a chunk decompresses with LZO1X to the declared size.
