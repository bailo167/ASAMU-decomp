# Ghidra scripts

Headless Ghidra scripts for the unstripped original Mac executable. They are **our code** and are committed;
everything they produce goes to git-ignored folders.

| Script | Output | Publishable? |
|---|---|---|
| `ExportFunctionSummaries.java` | JSON: address, size, signature, callers/callees (names), short referenced strings, float constants loaded from memory | Sanitized metadata may be summarised in docs; raw file stays in `research/ghidra/out/` |
| `DecompileToLocal.java` | decompiled C per function in `research/decompiled/` | **Never** — local reading only |

Anchor lists (symbol names only) live in `anchors/`.

## Setup

```bash
brew install ghidra            # pulls openjdk@21
export JAVA_HOME="$(brew --prefix openjdk@21)/libexec/openjdk.jdk/Contents/Home"
GHIDRA="$(brew --prefix ghidra)/libexec/support/analyzeHeadless"
BIN="$ASAMU_ORIGINAL_DIR/A Story About My Uncle.app/Contents/MacOS/ASAMU"

# one-time import + auto-analysis (~tens of minutes; project in research/ghidra, ignored)
GHIDRA_HEADLESS_MAXMEM=6G "$GHIDRA" research/ghidra ASAMU -import "$BIN" -overwrite

# sanitized summaries for the physics anchors
"$GHIDRA" research/ghidra ASAMU -process ASAMU -noanalysis -scriptPath tools/ghidra-scripts \
  -postScript ExportFunctionSummaries.java names=tools/ghidra-scripts/anchors/player-physics.txt \
  out=research/ghidra/out/player-physics.json

# local-only decompilation for reading
"$GHIDRA" research/ghidra ASAMU -process ASAMU -noanalysis -scriptPath tools/ghidra-scripts \
  -postScript DecompileToLocal.java names=tools/ghidra-scripts/anchors/player-physics.txt out=research/decompiled
```

The Mach-O keeps its symbol table, so Ghidra's names come from the original symbols; avoid renaming functions
that already have original names.
