// Export SANITIZED summaries of selected functions from the analysed ASAMU
// Mach-O: address, size, signature, callers/callees (names only), referenced
// short strings, and floating-point constants loaded from memory.
//
// The output is metadata for our evidence docs. It deliberately contains no
// decompiled code or instruction listings. Write it under the git-ignored
// research/ghidra/out/ and copy only what is needed (in our own words) into
// docs/reverse-engineering/.
//
// Usage (headless, after analysis):
//   analyzeHeadless research/ghidra ASAMU -process ASAMU -noanalysis \
//     -scriptPath tools/ghidra-scripts \
//     -postScript ExportFunctionSummaries.java names=<file> out=<file.json>
//
// <file> lists one qualified function name per line (e.g. APawn::physFalling).
// A trailing '*' matches by prefix (e.g. APawn::phys*). '#' starts a comment.
//
// @category ASAMU

import ghidra.app.script.GhidraScript;
import ghidra.program.model.address.Address;
import ghidra.program.model.listing.*;
import ghidra.program.model.mem.Memory;
import ghidra.program.model.symbol.Reference;
import ghidra.program.model.symbol.ReferenceManager;

import java.io.*;
import java.nio.charset.StandardCharsets;
import java.nio.file.*;
import java.util.*;

public class ExportFunctionSummaries extends GhidraScript {

    private static String esc(String s) {
        StringBuilder b = new StringBuilder();
        for (char c : s.toCharArray()) {
            switch (c) {
                case '"': b.append("\\\""); break;
                case '\\': b.append("\\\\"); break;
                case '\n': b.append("\\n"); break;
                case '\r': b.append("\\r"); break;
                case '\t': b.append("\\t"); break;
                default:
                    if (c < 0x20) b.append(String.format("\\u%04x", (int) c));
                    else b.append(c);
            }
        }
        return b.toString();
    }

    private Map<String, String> parseArgs() {
        Map<String, String> m = new HashMap<>();
        for (String a : getScriptArgs()) {
            int i = a.indexOf('=');
            if (i > 0) m.put(a.substring(0, i), a.substring(i + 1));
        }
        return m;
    }

    @Override
    protected void run() throws Exception {
        Map<String, String> args = parseArgs();
        String namesFile = args.get("names");
        String outFile = args.getOrDefault("out", "research/ghidra/out/functions.json");
        if (namesFile == null) {
            printerr("names=<file> is required");
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

        FunctionManager fm = currentProgram.getFunctionManager();
        ReferenceManager rm = currentProgram.getReferenceManager();
        Memory mem = currentProgram.getMemory();
        Listing listing = currentProgram.getListing();

        List<Function> selected = new ArrayList<>();
        for (Function f : fm.getFunctions(true)) {
            String q = f.getName(true);
            boolean hit = exact.contains(q);
            for (String p : prefixes) if (q.startsWith(p)) hit = true;
            if (hit) selected.add(f);
        }
        selected.sort(Comparator.comparing(f -> f.getEntryPoint()));

        Files.createDirectories(Paths.get(outFile).toAbsolutePath().getParent());
        try (Writer w = new OutputStreamWriter(new FileOutputStream(outFile), StandardCharsets.UTF_8)) {
            w.write("{\n  \"program\": \"" + esc(currentProgram.getName()) + "\",\n");
            w.write("  \"requested\": " + (exact.size() + prefixes.size()) + ",\n");
            w.write("  \"functions\": [\n");
            boolean firstF = true;
            for (Function f : selected) {
                monitor.checkCancelled();
                TreeSet<String> callers = new TreeSet<>();
                for (Function c : f.getCallingFunctions(monitor)) callers.add(c.getName(true));
                TreeSet<String> callees = new TreeSet<>();
                for (Function c : f.getCalledFunctions(monitor)) callees.add(c.getName(true));

                TreeSet<String> strings = new TreeSet<>();
                TreeMap<String, Integer> floats = new TreeMap<>();
                InstructionIterator it = listing.getInstructions(f.getBody(), true);
                while (it.hasNext()) {
                    Instruction ins = it.next();
                    String mn = ins.getMnemonicString().toUpperCase(Locale.ROOT);
                    for (Reference r : rm.getReferencesFrom(ins.getAddress())) {
                        if (!r.getReferenceType().isData()) continue;
                        Address to = r.getToAddress();
                        Data d = listing.getDataAt(to);
                        if (d != null && d.hasStringValue()) {
                            Object v = d.getValue();
                            if (v != null) {
                                String s = v.toString();
                                if (s.length() > 96) s = s.substring(0, 96) + "...";
                                strings.add(s);
                            }
                            continue;
                        }
                        try {
                            String key = null;
                            if (mn.endsWith("SS") || mn.equals("CVTSS2SD")) {
                                float fv = Float.intBitsToFloat(mem.getInt(to));
                                key = "f32 " + Float.toString(fv);
                            } else if (mn.endsWith("SD") && !mn.startsWith("MOVSD_")) {
                                double dv = Double.longBitsToDouble(mem.getLong(to));
                                key = "f64 " + Double.toString(dv);
                            } else if (mn.endsWith("PS")) {
                                StringBuilder sb = new StringBuilder("f32x4");
                                for (int k = 0; k < 4; k++) {
                                    sb.append(' ').append(Float.intBitsToFloat(mem.getInt(to.add(4L * k))));
                                }
                                key = sb.toString();
                            }
                            if (key != null) floats.merge(key, 1, Integer::sum);
                        } catch (Exception e) {
                            // unreadable / uninitialised memory: ignore
                        }
                    }
                }

                if (!firstF) w.write(",\n");
                firstF = false;
                w.write("    {\"name\": \"" + esc(f.getName(true)) + "\", ");
                w.write("\"address\": \"0x" + f.getEntryPoint().toString() + "\", ");
                w.write("\"size\": " + f.getBody().getNumAddresses() + ", ");
                w.write("\"signature\": \"" + esc(f.getSignature().getPrototypeString()) + "\",\n");
                w.write("     \"callers\": [");
                writeList(w, callers);
                w.write("],\n     \"callees\": [");
                writeList(w, callees);
                w.write("],\n     \"strings\": [");
                writeList(w, strings);
                w.write("],\n     \"float_constants\": {");
                boolean first = true;
                for (Map.Entry<String, Integer> e : floats.entrySet()) {
                    if (!first) w.write(", ");
                    first = false;
                    w.write("\"" + esc(e.getKey()) + "\": " + e.getValue());
                }
                w.write("}}");
            }
            w.write("\n  ]\n}\n");
        }
        println("ExportFunctionSummaries: wrote " + selected.size() + " functions to " + outFile);
    }

    private static void writeList(Writer w, Collection<String> xs) throws IOException {
        boolean first = true;
        for (String x : xs) {
            if (!first) w.write(", ");
            first = false;
            w.write("\"" + esc(x) + "\"");
        }
    }
}
