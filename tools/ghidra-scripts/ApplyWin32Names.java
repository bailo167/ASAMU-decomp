// Labels the Ghidra program of the original Win32 executable with the names recovered by
// tools/ghidra-scripts/win32/win32_scan.py (docs/reverse-engineering/data/win32/*.json), and
// cross-checks those tables against Ghidra's own analysis.
//
// Our own code. It reads our sanitized JSON tables (addresses, names, sizes) and writes a small
// text report with counts; it exports no code and no data from the program.
//
// Script arguments (key=value):
//   data=<dir>      directory with globals.json, functions.json, class_sizes.json, vtables.json
//   report=<file>   where to write the cross-check report (default: no file, console only)
//   apply=<bool>    create labels and function names (default true; use false with -readOnly)
//
// analyzeHeadless research/ghidra/win ASAMUWin -process ASAMU-Win32-Shipping.exe -noanalysis \
//   -scriptPath tools/ghidra-scripts -postScript ApplyWin32Names.java \
//   data=docs/reverse-engineering/data/win32 report=research/ghidra/out/win32-names.txt
//
// @category ASAMU
import java.io.File;
import java.io.FileReader;
import java.io.PrintWriter;
import java.util.ArrayList;
import java.util.List;

import com.google.gson.JsonArray;
import com.google.gson.JsonElement;
import com.google.gson.JsonObject;
import com.google.gson.JsonParser;

import ghidra.app.script.GhidraScript;
import ghidra.app.util.NamespaceUtils;
import ghidra.program.model.address.Address;
import ghidra.program.model.listing.Function;
import ghidra.program.model.listing.Instruction;
import ghidra.program.model.symbol.Namespace;
import ghidra.program.model.symbol.Reference;
import ghidra.program.model.symbol.SourceType;
import ghidra.program.model.symbol.SymbolTable;

public class ApplyWin32Names extends GhidraScript {

    private boolean apply = true;
    private long imageBase;
    private int labels = 0;
    private int labelFailures = 0;
    private final List<String> lines = new ArrayList<>();

    @Override
    protected void run() throws Exception {
        String dataDir = null;
        String reportPath = null;
        for (String arg : getScriptArgs()) {
            int eq = arg.indexOf('=');
            if (eq < 0) {
                continue;
            }
            String key = arg.substring(0, eq);
            String value = arg.substring(eq + 1);
            if (key.equals("data")) {
                dataDir = value;
            } else if (key.equals("report")) {
                reportPath = value;
            } else if (key.equals("apply")) {
                apply = Boolean.parseBoolean(value);
            }
        }
        if (dataDir == null) {
            throw new IllegalArgumentException("data=<dir> is required");
        }
        imageBase = currentProgram.getImageBase().getOffset();
        log("program: " + currentProgram.getName() + ", image base 0x" + Long.toHexString(imageBase)
            + ", functions known to Ghidra: " + currentProgram.getFunctionManager().getFunctionCount());

        JsonObject globals = read(new File(dataDir, "globals.json"));
        JsonObject functions = read(new File(dataDir, "functions.json"));
        JsonObject classes = read(new File(dataDir, "class_sizes.json"));

        // ---- globals
        for (JsonElement e : globals.getAsJsonArray("globals")) {
            JsonObject g = e.getAsJsonObject();
            Address a = at(g.get("rva").getAsLong());
            int refs = getReferencesTo(a).length;
            log(String.format("global   %-28s rva 0x%08X  references in Ghidra: %d", g.get("name").getAsString(),
                g.get("rva").getAsLong(), refs));
            label(a, g.get("name").getAsString());
        }

        // ---- functions: does Ghidra have a function that starts at each address?
        int starts = 0;
        int total = 0;
        for (JsonElement e : functions.getAsJsonArray("functions")) {
            JsonObject f = e.getAsJsonObject();
            String name = f.get("name").getAsString();
            Address a = at(f.get("rva").getAsLong());
            Function fn = getFunctionAt(a);
            total++;
            if (fn != null) {
                starts++;
            }
            log(String.format("function %-40s rva 0x%08X  Ghidra function start: %s%s", name, f.get("rva").getAsLong(),
                fn != null ? "yes" : "NO", fn != null ? ", body " + fn.getBody().getNumAddresses() + " bytes" : ""));
            nameFunction(a, name);
        }
        log("function starts confirmed by Ghidra: " + starts + " of " + total);

        // ---- class registrations: callers of the UClass static constructor
        Address ctor = at(classes.get("uclass_static_constructor_rva").getAsLong());
        int callRefs = 0;
        for (Reference r : getReferencesTo(ctor)) {
            if (r.getReferenceType().isCall()) {
                callRefs++;
            }
        }
        int expected = classes.getAsJsonObject("counts").get("registration_call_sites").getAsInt();
        log("calls to the UClass static constructor: Ghidra " + callRefs + ", table " + expected
            + (callRefs == expected ? " (equal)" : " (DIFFERENT)"));

        // ---- classes: PrivateStaticClass pointers and vtables
        JsonArray cols = classes.getAsJsonArray("columns");
        int iCpp = indexOf(cols, "cpp_name");
        int iVt = indexOf(cols, "vtable_rva");
        int iPsc = indexOf(cols, "private_static_class_rva");
        int vtablesWithCode = 0;
        int pscReferenced = 0;
        int n = 0;
        java.util.Set<Long> seenVtables = new java.util.HashSet<>();
        for (JsonElement e : classes.getAsJsonArray("classes")) {
            JsonArray row = e.getAsJsonArray();
            String cpp = row.get(iCpp).getAsString();
            long vt = row.get(iVt).getAsLong();
            long psc = row.get(iPsc).getAsLong();
            n++;
            // first vtable slot must be the start of a function (or at least code) for Ghidra too
            Address slot0 = at(vt);
            long target = getInt(slot0) & 0xFFFFFFFFL;
            Address t = toAddr(target);
            Instruction ins = getInstructionAt(t);
            if (ins != null || getFunctionAt(t) != null) {
                vtablesWithCode++;
            }
            if (getReferencesTo(at(psc)).length > 0) {
                pscReferenced++;
            }
            label(at(psc), cpp + "::PrivateStaticClass");
            if (seenVtables.add(vt)) {
                label(slot0, cpp + "::vftable");
            }
            if (monitor.isCancelled()) {
                break;
            }
        }
        log("classes: " + n + "; vtables whose first slot is code in Ghidra: " + vtablesWithCode
            + "; PrivateStaticClass addresses referenced in Ghidra: " + pscReferenced);
        log("labels " + (apply ? "created: " : "that would be created: ") + labels + ", failures: " + labelFailures);

        if (reportPath != null) {
            File out = new File(reportPath);
            if (out.getParentFile() != null) {
                out.getParentFile().mkdirs();
            }
            try (PrintWriter w = new PrintWriter(out, "UTF-8")) {
                for (String line : lines) {
                    w.println(line);
                }
            }
        }
    }

    private JsonObject read(File f) throws Exception {
        try (FileReader r = new FileReader(f)) {
            return JsonParser.parseReader(r).getAsJsonObject();
        }
    }

    private static int indexOf(JsonArray cols, String name) {
        for (int i = 0; i < cols.size(); i++) {
            if (cols.get(i).getAsString().equals(name)) {
                return i;
            }
        }
        throw new IllegalArgumentException("column " + name + " missing");
    }

    private Address at(long rva) {
        return toAddr(imageBase + rva);
    }

    private void log(String s) {
        println(s);
        lines.add(s);
    }

    private static String clean(String part) {
        return part.replaceAll("[^A-Za-z0-9_]", "_");
    }

    /** "A::B::name" -> namespace A::B and symbol name. */
    private Namespace namespaceOf(String qualified) throws Exception {
        int cut = qualified.lastIndexOf("::");
        if (cut < 0) {
            return currentProgram.getGlobalNamespace();
        }
        StringBuilder path = new StringBuilder();
        for (String part : qualified.substring(0, cut).split("::")) {
            if (path.length() > 0) {
                path.append("::");
            }
            path.append(clean(part));
        }
        return NamespaceUtils.createNamespaceHierarchy(path.toString(), null, currentProgram, SourceType.USER_DEFINED);
    }

    private static String leaf(String qualified) {
        int cut = qualified.lastIndexOf("::");
        return clean(cut < 0 ? qualified : qualified.substring(cut + 2));
    }

    private void label(Address a, String qualified) {
        labels++;
        if (!apply) {
            return;
        }
        try {
            SymbolTable st = currentProgram.getSymbolTable();
            st.createLabel(a, leaf(qualified), namespaceOf(qualified), SourceType.USER_DEFINED).setPrimary();
        } catch (Exception ex) {
            labelFailures++;
        }
    }

    private void nameFunction(Address a, String qualified) {
        labels++;
        if (!apply) {
            return;
        }
        try {
            Function fn = getFunctionAt(a);
            if (fn == null) {
                fn = createFunction(a, null);
            }
            if (fn == null) {
                label(a, qualified);
                labels--;
                return;
            }
            fn.setParentNamespace(namespaceOf(qualified));
            fn.setName(leaf(qualified), SourceType.USER_DEFINED);
        } catch (Exception ex) {
            labelFailures++;
        }
    }
}
