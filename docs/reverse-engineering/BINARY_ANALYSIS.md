# Binary analysis — original Mac executable

Target: `A Story About My Uncle.app/Contents/MacOS/ASAMU`, build 1822049,
SHA-256 `b611c4a0a64d220f3f2b8bdbd6287700327976bd2f196fca328a4b1b2d13d004`.

## CONFIRMED

| Fact | Evidence |
|---|---|
| x86_64 Mach-O executable (`MH_MAGIC_64`, `CPU_TYPE_X86_64`, subtype ALL) | `file`, `otool -hv` |
| 28 load commands, 3,752 bytes; flags `NOUNDEFS DYLDLINK TWOLEVEL WEAK_DEFINES BINDS_TO_WEAK PIE` | `otool -hv` |
| Not code-signed ("code object is not signed at all") | `codesign -dv` |
| Not stripped: 135,093 symbol-table entries; 134,643 defined, 450 undefined | `nm`, `nm -U`, `nm -u` |
| Local symbols retained: 23,631 local text (`t`), 21,156 local `s`, 11,214 local bss (`b`), 2,058 local data (`d`) | `nm` type census |
| Linked: Carbon, Cocoa, AppKit, Foundation, CoreFoundation, CoreServices, OpenGL, IOKit, Security, libobjc, libstdc++.6, libSystem | `otool -L` |
| Bundled middleware via `@loader_path`: `openal.dylib` (1.15.1), `libSDL2-2.0.0.dylib`, `libsteam_api.dylib` | `otool -L` |
| C++ symbols are Itanium-mangled (`__ZN...`), e.g. `UASAMUSystemSettingsManager::SetLanguage(const FString&)` | `nm` |
| Static-initializer names expose compilation units, e.g. `__GLOBAL__sub_I_ASAMU.cpp`, `__GLOBAL__sub_I_ASAMUSystemSettingsManager.cpp` | `nm` |

## STRONG

- The executable is a UE3/UDK build with the standard native registration scheme
  (`AutoInitializeRegistrants<Package>`, `AutoGenerateNames<Package>`, `G<pkg>U<Class>Natives` tables,
  `exec<Function>` thunks, `PrivateStaticClass`). Evidence: symbol names of that form for the ASAMU package.

## TENTATIVE

- Linking `libstdc++.6` (not libc++) and an OS X 10.6 minimum suggests an older Xcode/GCC-era toolchain port.

## UNKNOWN

- Whether DWARF debug info or source paths survive (to check with `dwarfdump`, `strings`).
- Usefulness of RTTI/vtables (`__ZTV*`/`__ZTI*` present for at least `UASAMUSystemSettingsManager`).
- Segment/section map and size of `__TEXT,__text`.

## Reproduce

```bash
B="$ASAMU_ORIGINAL_DIR/A Story About My Uncle.app/Contents/MacOS/ASAMU"
file "$B"; otool -hv "$B"; otool -L "$B"; codesign -dv "$B"
nm "$B" | wc -l; nm -U "$B" | wc -l; nm -u "$B" | wc -l
nm "$B" | awk '{print $2}' | sort | uniq -c | sort -rn
```
