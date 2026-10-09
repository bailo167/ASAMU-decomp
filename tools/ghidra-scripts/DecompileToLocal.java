// Decompile selected functions into a LOCAL, git-ignored directory for
// private reading during reverse engineering (research/decompiled/).
//
// NEVER commit the output. Decompiled code of the original game is not
// publishable; findings go into docs/ in our own words.
//
// Usage (headless, after analysis):
//   analyzeHeadless research/ghidra ASAMU -process ASAMU -noanalysis \
//     -scriptPath tools/ghidra-scripts \
//     -postScript DecompileToLocal.java names=<file> out=research/decompiled
//
// <file>: one qualified name per line; trailing '*' = prefix match.
//
// @category ASAMU

import ghidra.app.decompiler.DecompInterface;
import ghidra.app.decompiler.DecompileResults;
import ghidra.app.script.GhidraScript;
import ghidra.program.model.listing.Function;
import ghidra.program.model.listing.FunctionManager;

import java.io.*;
import java.nio.charset.StandardCharsets;
import java.nio.file.*;
import java.util.*;

public class DecompileToLocal extends GhidraScript {

    @Override
    protected void run() throws Exception {
        Map<String, String> args = new HashMap<>();
        for (String a : getScriptArgs()) {
            int i = a.indexOf('=');
            if (i > 0) args.put(a.substring(0, i), a.substring(i + 1));
        }
        String namesFile = args.get("names");
        String outDir = args.getOrDefault("out", "research/decompiled");
        if (namesFile == null) {
            printerr("names=<file> is required");
            return;
        }
        if (!outDir.contains("research")) {
            printerr("refusing to write decompiled output outside research/ (git-ignored)");
            return;
        }
        List<String> exact = new ArrayList<>();
        List<String> prefixes = new ArrayList<>();
        for (String line : Files.readAllLines(Paths.get(namesFile), StandardCharsets.UTF_8)) {
            String t = line.trim();
            if (t.isEmpty() || t.startsWith("#")) continue;
            if (t.endsWith("*")) prefixes.add(t.substring(0, t.length() - 1));
            else exact.add(t);
        }
        Files.createDirectories(Paths.get(outDir));
        DecompInterface di = new DecompInterface();
        di.openProgram(currentProgram);
        FunctionManager fm = currentProgram.getFunctionManager();
        int n = 0;
        for (Function f : fm.getFunctions(true)) {
            monitor.checkCancelled();
            String q = f.getName(true);
            boolean hit = exact.contains(q);
            for (String p : prefixes) if (q.startsWith(p)) hit = true;
            if (!hit) continue;
            DecompileResults r = di.decompileFunction(f, 120, monitor);
            String c = (r != null && r.getDecompiledFunction() != null)
                    ? r.getDecompiledFunction().getC()
                    : "// decompilation failed: " + (r == null ? "null" : r.getErrorMessage());
            String file = q.replaceAll("[^A-Za-z0-9_.-]", "_") + "@" + f.getEntryPoint() + ".c";
            Files.write(Paths.get(outDir, file), c.getBytes(StandardCharsets.UTF_8));
            n++;
        }
        di.dispose();
        println("DecompileToLocal: wrote " + n + " functions to " + outDir);
    }
}
