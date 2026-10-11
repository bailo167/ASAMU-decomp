#!/usr/bin/env python3
"""Read-only live check of the Win32 field layout against the engine's own property objects.

Run on the Windows machine while the original game (ASAMU-Win32-Shipping.exe) is running; the main
menu is enough. The engine keeps one UProperty object per script property, and each one stores the
offset the engine itself computed for that field. This script walks every loaded class, reads each
property's offset (and each bool's bit mask) and compares them with the statically derived layout
(expected_win32.json, produced by an independent implementation of the Win32 layout rules). It also
compares values of class default objects with the recorded script defaults (GroundSpeed 440,
JumpZ 1000, ...), and, when a level is running, shows the live pawn's values.

It opens the process with query + read rights only and reads memory with ReadProcessMemory. It
never writes to the process, injects nothing and attaches no debugger.

    python win32_props_check.py                    (data files next to the script or in data\\win32)
    python win32_props_check.py --expected FILE --data DIR --verbose
    python win32_props_check.py --selftest         (no game needed; any OS)

Two expectations follow from how the engine links a class (WINDOWS_BINARY.md 8):

* UStruct.PropertiesSize of a class whose script is loaded is the END of its last property, not the
  C++ sizeof: the registered sizeof is that end rounded up to the class's alignment (4, 8 or 16). A
  native class whose script package the game does not load keeps the sizeof its constructor set.
* The executable registers the native classes of every package it was built with, the editor's
  too. Their class objects exist, but without their script package they have no property objects:
  the layout's fields of those classes are "not loaded", not "missing".

Every line, and every list in full, also goes to a log file (--log FILE; default
logs/win32_props_check-<UTC time>.log next to the script, or research/local/win/live-checks/ in a
repository checkout; --no-log for none). The console shows the first rows of a long list unless
--verbose is given.

Stock CPython 3.9+ (ctypes only). Exit code: 0 every check passed, 1 a check failed, 2 the game is
not running.
"""

import argparse
import ctypes
import json
import os
import struct
import sys
import time

EXE_NAME = "ASAMU-Win32-Shipping.exe"
MAX_OBJECTS = 4000000
MAX_FIELDS = 20000
MAX_NAME_CHARS = 1024
SHOWN_ROWS = 10  # rows of a long list on the console (the log has all of them)
ALIGNMENTS = (4, 8, 16)  # class alignments of this build (DEFAULTS.md 9.3, rules 5 and 6)


class ReadError(Exception):
    pass


class Memory:
    """read(addr, size) -> bytes over a process handle or a fake image."""

    def __init__(self, read, base):
        self._read = read
        self.base = base
        self.reads = 0

    def raw(self, addr, size):
        self.reads += 1
        data = self._read(addr, size)
        if data is None or len(data) != size:
            raise ReadError("cannot read %d bytes at %#x" % (size, addr))
        return data

    def try_raw(self, addr, size):
        try:
            return self.raw(addr, size)
        except ReadError:
            return None

    def u32(self, addr):
        return struct.unpack("<I", self.raw(addr, 4))[0]

    def f32(self, addr):
        return struct.unpack("<f", self.raw(addr, 4))[0]


class Report:
    """Prints the checks; ``log`` (a file object) gets every line, and every
    row of a list whether or not the console shows it."""

    def __init__(self, verbose=False, log=None, quiet=False):
        self.failed = 0
        self.passed = 0
        self.verbose = verbose
        self.log = log
        self.quiet = quiet

    def _line(self, text, console=True):
        if console and not self.quiet:
            print(text)
        if self.log is not None:
            self.log.write(text + "\n")

    def check(self, ok, text):
        if ok:
            self.passed += 1
        else:
            self.failed += 1
        self._line("[%s] %s" % ("ok" if ok else "FAIL", text))
        return ok

    def info(self, text):
        self._line("[info] %s" % text)

    def detail(self, text):
        self._line("       %s" % text, console=self.verbose)

    def rows(self, title, rows):
        """A list: its title and length, then the rows (all of them in the
        log; the first SHOWN_ROWS on the console unless verbose)."""
        if not rows:
            return
        self._line("       %s: %d" % (title, len(rows)))
        for i, row in enumerate(rows):
            self._line("         %s" % row, console=self.verbose or i < SHOWN_ROWS)
        if not self.verbose and len(rows) > SHOWN_ROWS:
            self._line("         ... %d more in the log" % (len(rows) - SHOWN_ROWS))


def align_up(value, alignment):
    return (value + alignment - 1) // alignment * alignment


def rounds_up_to(end, size):
    """The alignment (one of ALIGNMENTS) by which ``end`` rounds up to
    ``size``; 1 when they are equal; None when no alignment gives it."""
    if end == size:
        return 1
    for a in ALIGNMENTS:
        if end < size and align_up(end, a) == size:
            return a
    return None


class Game:
    """The engine structures the checks walk. Every offset comes from the data files."""

    def __init__(self, mem, layout, globals_json, class_sizes, expected, optional=None):
        self.mem = mem
        self.expected = expected
        # The recorder's optional fields (recorder_optional_win32.json), when the file is there.
        self.optional = {(f["class"], f["name"]): f for f in (optional or {}).get("fields", [])}
        r = expected["reflection"]
        self.r = r
        self.o_outer = r["object_outer"]
        self.o_name = r["object_name"]
        self.o_class = r["object_class"]
        self.o_index = r["object_index"]
        self.head = max(self.o_class, self.o_index) + 4
        st = layout["structs"]
        self.entry_index = st["FNameEntry"]["members"]["index"]
        self.entry_chars = st["FNameEntry"]["members"]["chars"]
        self.wide_mask = st["FNameEntry"]["wide_flag_mask"]
        self.wide_size = st["FNameEntry"]["wide_char_size"]
        self.symbols = {k: int(v["rva"], 16) for k, v in layout["symbols"].items()}
        self.fields = {(f["class"], f["name"]): f for f in layout["fields"]}
        self.image = layout["image"]
        self.globals = {g["name"]: g["rva"] for g in globals_json["globals"]}
        cols = class_sizes["columns"]
        self.native = [dict(zip(cols, row)) for row in class_sizes["classes"]]
        self.native_by_path = {"%s.%s" % (c["package"], c["name"]): c for c in self.native}
        self.names = {}
        self.props = {}

    # ---- names
    def name_text(self, index):
        if index in self.names:
            return self.names[index]
        base = self.mem.base + self.symbols["FName::Names"]
        data, count, _cap = struct.unpack("<Iii", self.mem.raw(base, 12))
        text = None
        if data and 0 <= index < count:
            entry = self.mem.u32(data + 4 * index)
            if entry:
                wide = bool(self.mem.u32(entry + self.entry_index) & self.wide_mask)
                unit = self.wide_size if wide else 1
                chars = []
                pos = entry + self.entry_chars
                done = False
                while not done and len(chars) < MAX_NAME_CHARS:
                    # Read ahead in chunks; fall back to smaller reads near the end of a mapping.
                    blob = None
                    for n in (64, 8, 1):
                        blob = self.mem.try_raw(pos, n * unit)
                        if blob:
                            break
                    if not blob:
                        break
                    for i in range(0, len(blob), unit):
                        c = struct.unpack_from("<H" if unit == 2 else "<B", blob, i)[0]
                        if c == 0:
                            done = True
                            break
                        chars.append(chr(c))
                    pos += len(blob)
                text = "".join(chars)
        self.names[index] = text
        return text

    def object_head(self, obj):
        return self.mem.try_raw(obj, self.head)

    def object_name(self, head):
        index, number = struct.unpack_from("<ii", head, self.o_name)
        base = self.name_text(index)
        if base is None:
            return None
        return base if number == 0 else "%s_%d" % (base, number - 1)

    def name_of(self, obj):
        head = self.object_head(obj)
        return self.object_name(head) if head else None

    # ---- objects
    def objects(self):
        base = self.mem.base + self.globals["UObject::GObjObjects"]
        data, count, cap = struct.unpack("<Iii", self.mem.raw(base, 12))
        if not data or count <= 0 or count > MAX_OBJECTS or count > cap:
            raise ReadError("UObject::GObjObjects does not look like an array (%#x, %d, %d)" % (data, count, cap))
        table = self.mem.raw(data, 4 * count)
        return struct.unpack("<%dI" % count, table)

    def properties(self, cls):
        """(name, class name, offset, bit mask or None, outer) of every property the class declares."""
        if cls in self.props:
            return self.props[cls]
        r = self.r
        out = []
        field = self.mem.u32(cls + r["struct_children"])
        seen = 0
        while field and seen < MAX_FIELDS:
            seen += 1
            head = self.object_head(field)
            if not head:
                raise ReadError("unreadable field object at %#x" % field)
            kind = self.name_of(struct.unpack_from("<I", head, self.o_class)[0]) or ""
            if kind.endswith("Property"):
                offset = self.mem.u32(field + r["property_offset"])
                mask = self.mem.u32(field + r["bool_bitmask"]) if kind == "BoolProperty" else None
                dim = self.mem.u32(field + r["property_array_dim"])
                out.append((self.object_name(head), kind, offset, mask, struct.unpack_from("<I", head, self.o_outer)[0], dim))
            field = self.mem.u32(field + r["field_next"])
        self.props[cls] = out
        return out


def expected_field(expected, path, name):
    """(offset, bit, declaring class) of `name` in class `path` or one of its supers."""
    p = path
    while p:
        c = expected["classes"].get(p)
        if c is None:
            return None
        f = c["fields"].get(name)
        if f is not None:
            return f[0], f[1], p
        p = c.get("super")
    return None


def run_checks(game, report, seconds=1.0):
    mem = game.mem
    exp = game.expected
    # ---- the module is this build
    e_lfanew = mem.u32(mem.base + 0x3C)
    stamp = mem.u32(mem.base + e_lfanew + 8)
    report.check(stamp == game.image["time_date_stamp"], "module time stamp %d (want %d)" % (stamp, game.image["time_date_stamp"]))
    report.check(game.name_text(0) == "None", "FName::Names[0] is %r (want 'None')" % game.name_text(0))

    # ---- objects, class objects
    objects = game.objects()
    report.info("UObject::GObjObjects holds %d slots" % len(objects))
    uclass_psc = game.native_by_path.get("Core.Class")
    uclass = mem.u32(mem.base + uclass_psc["private_static_class_rva"]) if uclass_psc else 0
    report.check(uclass != 0 and game.name_of(uclass) == "Class", "UClass::PrivateStaticClass points at the class named 'Class'")
    heads = {}
    index_ok = index_seen = 0
    classes = {}
    by_name = {}
    for slot, obj in enumerate(objects):
        if not obj:
            continue
        head = game.object_head(obj)
        if not head:
            continue
        heads[obj] = head
        if index_seen < 5000:
            index_seen += 1
            index_ok += struct.unpack_from("<i", head, game.o_index)[0] == slot
        if struct.unpack_from("<I", head, game.o_class)[0] == uclass:
            classes[obj] = head
    report.check(index_seen > 0 and index_ok == index_seen, "object index (+%#x) equals the object's slot for %d of %d sampled objects" % (game.o_index, index_ok, index_seen))
    paths = {}
    for obj, head in classes.items():
        outer = struct.unpack_from("<I", head, game.o_outer)[0]
        oname = game.name_of(outer) if outer else None
        name = game.object_name(head)
        if name and oname:
            paths["%s.%s" % (oname, name)] = obj
    report.info("%d class objects loaded, %d with a package outer" % (len(classes), len(paths)))

    # ---- every property of every loaded class (walked first: which script packages are loaded
    # decides what the class objects must hold)
    own = {}
    for path in sorted(paths):
        cls = paths[path]
        own[path] = [p for p in game.properties(cls) if p[4] == cls]
    loaded_packages = sorted({path.split(".")[0] for path, props in own.items() if props})
    report.info("script packages with live properties (loaded): %d (%s)" % (len(loaded_packages), ", ".join(loaded_packages)))

    # ---- native classes: PrivateStaticClass and PropertiesSize
    # A class whose script is loaded was linked by the engine: PropertiesSize is the end of its
    # last property, and the registered sizeof is that end rounded up to the class's alignment. A
    # class whose script package is not loaded keeps the sizeof its constructor set.
    psc_ok = psc_set = 0
    unset = []
    linked = unlinked = old_equal = 0
    rounded = {a: 0 for a in ALIGNMENTS}
    unlinked_packages = {}
    size_bad = []
    static_bad = []
    for c in game.native:
        path = "%s.%s" % (c["package"], c["name"])
        e = exp["classes"].get(path)
        ptr = mem.u32(mem.base + c["private_static_class_rva"])
        if not ptr:
            unset.append("%s (%s)%s" % (c["cpp_name"], path, "" if e is None or e["end"] == c["size"]
                                         else ": its property end %#x is below its sizeof %#x" % (e["end"], c["size"])))
            continue
        psc_set += 1
        if game.name_of(ptr) != c["name"]:
            continue
        psc_ok += 1
        got = mem.u32(ptr + game.r["struct_properties_size"])
        old_equal += got == c["size"]
        if e is not None and c["package"] in loaded_packages:
            want, what = e["end"], "property end"
            a = rounds_up_to(e["end"], c["size"])
            if a is None:
                static_bad.append("%s (%s): layout end %#x does not round up to the registered sizeof %#x" % (c["cpp_name"], path, e["end"], c["size"]))
            elif a > 1 and got == want:
                rounded[a] += 1
            linked += got == want
        else:
            want, what = c["size"], "registered sizeof"
            if got == want:
                unlinked += 1
                unlinked_packages[c["package"]] = unlinked_packages.get(c["package"], 0) + 1
        if got != want:
            also = "the registered sizeof" if got == c["size"] else ("the layout's property end" if e is not None and got == e["end"] else "neither end nor sizeof")
            size_bad.append("%s (%s): live %#x, expected the %s %#x; layout end %s, sizeof %#x; live equals %s; package %s"
                            % (c["cpp_name"], path, got, what, want, "%#x" % e["end"] if e is not None else "none", c["size"], also,
                               "loaded" if c["package"] in loaded_packages else "not loaded"))
    report.check(psc_set > 0 and psc_ok == psc_set, "%d of %d set PrivateStaticClass pointers name their class" % (psc_ok, psc_set))
    report.rows("native classes whose PrivateStaticClass is not set (not registered yet; not checked)", unset)
    report.check(psc_ok > 0 and not size_bad,
                 "UStruct.PropertiesSize (+%#x) is the property end (script loaded) or the registered sizeof (not loaded) for %d of %d native classes"
                 % (game.r["struct_properties_size"], linked + unlinked, psc_ok))
    report.info("  script loaded: %d classes hold their property end; for %d of them it is below the sizeof (rounded up to %s)"
                % (linked, sum(rounded.values()), ", ".join("%d: %d" % (a, rounded[a]) for a in ALIGNMENTS)))
    report.info("  script not loaded or no script class: %d classes hold their sizeof (%s)"
                % (unlinked, ", ".join("%s %d" % kv for kv in sorted(unlinked_packages.items())) or "none"))
    report.info("  for comparison, PropertiesSize equals the registered sizeof itself for %d of %d (the earlier, wrong expectation)" % (old_equal, psc_ok))
    report.rows("PropertiesSize not as expected", size_bad)
    report.check(not static_bad, "the layout's property end rounds up to the registered sizeof (alignment %s) for every native class with loaded script"
                 % ", ".join(str(a) for a in ALIGNMENTS))
    report.rows("layout end against sizeof", static_bad)

    # ---- the properties against the layout
    total = match = 0
    bad = []
    unknown = []
    bits_total = bits_ok = 0
    bits_bad = []
    classes_checked = classes_unknown = 0
    missing = []
    not_loaded = {}  # package -> [classes, layout fields] of registered classes without their script
    script_sizes = [0, 0]
    script_bad = []
    for path in sorted(paths):
        cls = paths[path]
        e = exp["classes"].get(path)
        if e is None:
            classes_unknown += 1
            continue
        package = path.split(".")[0]
        if not own[path] and package not in loaded_packages:
            # The executable registered the class natively; its script package was never loaded,
            # so it has no property objects: nothing to compare, nothing missing.
            entry = not_loaded.setdefault(package, [0, 0])
            entry[0] += 1
            entry[1] += len(e["fields"])
            continue
        classes_checked += 1
        seen = set()
        for name, kind, offset, mask, _outer, _dim in own[path]:
            total += 1
            seen.add(name)
            want = e["fields"].get(name)
            if want is None:
                unknown.append("%s.%s" % (path, name))
                continue
            if offset == want[0]:
                match += 1
            else:
                bad.append("%s.%s: live %#x, layout %#x" % (path, name, offset, want[0]))
            if kind == "BoolProperty" and want[1] is not None:
                bits_total += 1
                if mask == (1 << want[1]):
                    bits_ok += 1
                else:
                    bits_bad.append("%s.%s: live mask %#x, layout bit %d" % (path, name, mask or 0, want[1]))
        for name in e["fields"]:
            if name not in seen:
                missing.append("%s.%s" % (path, name))
        if path not in game.native_by_path and e.get("size") is not None:
            got = mem.u32(cls + game.r["struct_properties_size"])
            script_sizes[1] += 1
            if got in (e["size"], e.get("end")):
                script_sizes[0] += 1
            else:
                script_bad.append("%s: live %#x, layout end %#x, size %#x" % (path, got, e.get("end") or 0, e["size"]))
    report.check(total > 0 and not bad, "UProperty.Offset equals the layout for %d of %d properties in %d classes" % (match, total, classes_checked))
    report.rows("property offsets that differ", bad)
    report.check(bits_total > 0 and bits_ok == bits_total, "UBoolProperty bit mask (+%#x) equals 1 << bit for %d of %d bool properties" % (game.r["bool_bitmask"], bits_ok, bits_total))
    report.rows("bool masks that differ", bits_bad)
    report.check(not unknown and not missing,
                 "property lists agree for the %d classes with loaded script: %d live properties not in the layout, %d layout fields not live"
                 % (classes_checked, len(unknown), len(missing)))
    report.rows("live properties not in the layout", unknown)
    report.rows("layout fields without a live property", missing)
    # Where every field of the layout went.
    fields_total = sum(len(c["fields"]) for c in exp["classes"].values())
    fields_not_loaded = sum(v[1] for v in not_loaded.values())
    unvisited = {}
    for path, c in exp["classes"].items():
        if path not in paths:
            entry = unvisited.setdefault(path.split(".")[0], [0, 0])
            entry[0] += 1
            entry[1] += len(c["fields"])
    fields_unvisited = sum(v[1] for v in unvisited.values())
    report.info("layout fields: %d = %d live and matched by name + %d not live + %d of %d registered classes whose script package is not loaded (%s) + %d of %d classes with no class object (%s)"
                % (fields_total, total - len(unknown), len(missing), fields_not_loaded, sum(v[0] for v in not_loaded.values()),
                   ", ".join("%s: %d classes, %d fields" % (k, v[0], v[1]) for k, v in sorted(not_loaded.items())) or "none",
                   fields_unvisited, sum(v[0] for v in unvisited.values()),
                   ", ".join("%s: %d classes, %d fields" % (k, v[0], v[1]) for k, v in sorted(unvisited.items())) or "none"))
    report.check(fields_total == total - len(unknown) + len(missing) + fields_not_loaded + fields_unvisited,
                 "every layout field is accounted for (live, not live, script not loaded, or no class object)")
    report.info("script-only classes whose PropertiesSize equals the computed size: %d of %d; classes loaded but not in the layout data: %d" % (script_sizes[0], script_sizes[1], classes_unknown))
    report.rows("script-only classes whose PropertiesSize is neither the layout's end nor its size", script_bad)

    # ---- the fields the recorder reads
    rec_total = rec_ok = 0
    rec_bad = []
    for (cls_path, name), f in sorted(game.fields.items()):
        if "." in name or cls_path not in paths:
            continue
        for pname, kind, offset, mask, outer, _dim in game.properties(paths[cls_path]):
            if pname == name and outer == paths[cls_path]:
                rec_total += 1
                ok = offset == int(f["offset"], 16) and (f.get("bit") is None or mask == (1 << f["bit"]))
                rec_ok += ok
                if not ok:
                    rec_bad.append("%s.%s: live %#x mask %s, recorder layout %s bit %s" % (cls_path, name, offset, mask, f["offset"], f.get("bit")))
    report.check(rec_total > 0 and not rec_bad, "recorder layout: %d of %d plain fields equal the live property offsets" % (rec_ok, rec_total))
    report.rows("recorder fields that differ", rec_bad)
    if game.optional:
        opt_total = opt_ok = 0
        opt_bad = []
        opt_skipped = []
        for (cls_path, name), f in sorted(game.optional.items()):
            if "." in name or cls_path not in paths:
                opt_skipped.append("%s.%s (%s)" % (cls_path, name, "a struct member" if "." in name else "class not loaded"))
                continue
            for pname, kind, offset, mask, outer, _dim in game.properties(paths[cls_path]):
                if pname == name and outer == paths[cls_path]:
                    opt_total += 1
                    ok = offset == int(f["offset"], 16) and (f.get("bit") is None or mask == (1 << f["bit"]))
                    opt_ok += ok
                    if not ok:
                        opt_bad.append("%s.%s: live %#x mask %s, optional layout %s bit %s" % (cls_path, name, offset, mask, f["offset"], f.get("bit")))
        report.check(opt_total > 0 and not opt_bad, "recorder optional fields: %d of %d plain fields equal the live property offsets" % (opt_ok, opt_total))
        report.rows("optional fields that differ", opt_bad)
        report.rows("optional fields not checked here", opt_skipped)
    else:
        report.info("recorder optional fields: recorder_optional_win32.json not found, not checked")

    # ---- class default objects against the recorded script defaults
    for obj, head in heads.items():
        by_name.setdefault(struct.unpack_from("<i", head, game.o_name)[0], []).append(obj)
    name_index = {}
    for index in by_name:
        text = game.name_text(index)
        if text:
            name_index.setdefault(text, index)
    for path, rows in sorted(exp.get("defaults", {}).items()):
        cls = paths.get(path)
        if not cls:
            report.info("%s is not loaded: default values not checked" % path)
            continue
        short = "Default__" + path.split(".")[-1]
        cdo = None
        for obj in by_name.get(name_index.get(short, -1), []):
            if struct.unpack_from("<I", heads[obj], game.o_class)[0] == cls:
                cdo = obj
        if not cdo:
            report.check(False, "%s: class default object %s not found" % (path, short))
            continue
        good = 0
        wrong = []
        for _decl, name, kind, value in rows:
            want = expected_field(exp, path, name)
            if want is None:
                wrong.append("%s: not in the layout" % name)
                continue
            offset, bit, _owner = want
            raw = mem.raw(cdo + offset, 4)
            if kind == "float":
                got = struct.unpack("<f", raw)[0]
                ok = abs(got - value) <= 1e-4 * max(1.0, abs(value))
            elif kind == "int":
                got = struct.unpack("<i", raw)[0]
                ok = got == value
            else:
                got = bool(struct.unpack("<I", raw)[0] & (1 << bit))
                ok = got == bool(value)
            good += ok
            if not ok:
                wrong.append("%s at %#x: live %r, default %r" % (name, offset, got, value))
        report.check(not wrong, "%s: %d of %d recorded default values are at their layout offsets in %s" % (path, good, len(rows), short))
        report.rows("%s: values that differ" % path, wrong)

    # ---- the live player (informational: values change during play)
    engine = mem.u32(mem.base + game.symbols["GEngine"])
    world = mem.u32(mem.base + game.symbols["GWorld"])
    report.check(engine != 0, "GEngine is set (%#x)" % engine)
    report.info("GWorld = %#x" % world)

    def off(cls_path, name):
        return int(game.fields[(cls_path, name)]["offset"], 16)

    try:
        players, count = struct.unpack("<Ii", mem.raw(engine + off("Engine.Engine", "GamePlayers"), 8))
        controller = mem.u32(mem.u32(players) + off("Engine.Player", "Actor")) if count > 0 and players else 0
        pawn = mem.u32(controller + off("Engine.Controller", "Pawn")) if controller else 0
        report.info("GamePlayers: %d; controller %#x (%s); pawn %#x (%s)" % (
            count, controller, game.name_of(mem.u32(controller + game.o_class)) if controller else None,
            pawn, game.name_of(mem.u32(pawn + game.o_class)) if pawn else None))
        if pawn:
            values = {n: mem.f32(pawn + off("Engine.Pawn", n)) for n in ("GroundSpeed", "AirSpeed", "JumpZ", "AirControl", "WalkableFloorZ", "EyeHeight")}
            loc = struct.unpack("<3f", mem.raw(pawn + off("Engine.Actor", "Location"), 12))
            report.info("pawn values: %s; Location %.1f %.1f %.1f; Physics %d" % (
                ", ".join("%s %.4g" % kv for kv in values.items()), loc[0], loc[1], loc[2],
                mem.raw(pawn + off("Engine.Actor", "Physics"), 1)[0]))
            report.check(abs(values["WalkableFloorZ"] - 0.78) < 1e-4, "live pawn WalkableFloorZ is %.4g (want 0.78)" % values["WalkableFloorZ"])
    except (ReadError, KeyError) as error:
        report.info("player walk stopped: %s" % error)

    # ---- the frame counter advances
    if seconds > 0:
        fc = mem.base + game.symbols["GFrameCounter"]
        a = struct.unpack("<Q", mem.raw(fc, 8))[0]
        time.sleep(seconds)
        b = struct.unpack("<Q", mem.raw(fc, 8))[0]
        report.check(b > a, "GFrameCounter advanced from %d to %d in %.1f s" % (a, b, seconds))
    report.info("%d memory reads" % mem.reads)


# ------------------------------------------------------------- data files


def find_file(name, explicit, subdirs):
    here = os.path.dirname(os.path.abspath(__file__))
    candidates = [explicit] if explicit else []
    for s in subdirs:
        candidates.append(os.path.join(here, s, name))
    for c in candidates:
        if c and os.path.isfile(c):
            return c
    raise SystemExit("%s not found (looked in %s)" % (name, ", ".join(str(c) for c in candidates)))


def load_inputs(args):
    data_dirs = ["", "data/win32", "../../../docs/reverse-engineering/data/win32"]
    layout_dirs = ["", "../../../tools/trace-recorder"]
    files = {
        "layout": find_file("layout_win_x86.json", args.layout, layout_dirs),
        "globals": find_file("globals.json", args.data and os.path.join(args.data, "globals.json"), data_dirs),
        "class_sizes": find_file("class_sizes.json", args.data and os.path.join(args.data, "class_sizes.json"), data_dirs),
        "expected": find_file("expected_win32.json", args.expected, ["", "data/win32"]),
    }
    out = {}
    for key, path in files.items():
        with open(path, "r", encoding="utf-8") as fh:
            out[key] = json.load(fh)
    # Optional: the recorder's optional fields are checked too when their file is found.
    out["optional"] = None
    here = os.path.dirname(os.path.abspath(__file__))
    for d in ([args.data] if args.data else []) + [os.path.join(here, x) for x in data_dirs]:
        path = os.path.join(d, "recorder_optional_win32.json")
        if os.path.isfile(path):
            with open(path, "r", encoding="utf-8") as fh:
                out["optional"] = json.load(fh)
            break
    return out


# ------------------------------------------------------------- windows

TH32CS_SNAPPROCESS = 0x00000002
TH32CS_SNAPMODULE = 0x00000008
TH32CS_SNAPMODULE32 = 0x00000010
PROCESS_VM_READ = 0x0010
PROCESS_QUERY_INFORMATION = 0x0400
INVALID_HANDLE = ctypes.c_void_p(-1).value


class PROCESSENTRY32W(ctypes.Structure):
    _fields_ = [
        ("dwSize", ctypes.c_uint32), ("cntUsage", ctypes.c_uint32), ("th32ProcessID", ctypes.c_uint32),
        ("th32DefaultHeapID", ctypes.c_size_t), ("th32ModuleID", ctypes.c_uint32), ("cntThreads", ctypes.c_uint32),
        ("th32ParentProcessID", ctypes.c_uint32), ("pcPriClassBase", ctypes.c_int32), ("dwFlags", ctypes.c_uint32),
        ("szExeFile", ctypes.c_wchar * 260),
    ]


class MODULEENTRY32W(ctypes.Structure):
    _fields_ = [
        ("dwSize", ctypes.c_uint32), ("th32ModuleID", ctypes.c_uint32), ("th32ProcessID", ctypes.c_uint32),
        ("GlblcntUsage", ctypes.c_uint32), ("ProccntUsage", ctypes.c_uint32), ("modBaseAddr", ctypes.c_void_p),
        ("modBaseSize", ctypes.c_uint32), ("hModule", ctypes.c_void_p), ("szModule", ctypes.c_wchar * 256),
        ("szExePath", ctypes.c_wchar * 260),
    ]


def open_game():
    """(Memory, close) for the running game, or (None, None) when it is not running. Read-only access."""
    if not hasattr(ctypes, "windll"):
        raise SystemExit("the live check needs Windows; use --selftest elsewhere")
    k = ctypes.windll.kernel32
    k.CreateToolhelp32Snapshot.restype = ctypes.c_void_p
    k.CreateToolhelp32Snapshot.argtypes = [ctypes.c_uint32, ctypes.c_uint32]
    k.OpenProcess.restype = ctypes.c_void_p
    k.OpenProcess.argtypes = [ctypes.c_uint32, ctypes.c_int, ctypes.c_uint32]
    k.CloseHandle.argtypes = [ctypes.c_void_p]
    k.ReadProcessMemory.argtypes = [ctypes.c_void_p, ctypes.c_void_p, ctypes.c_void_p, ctypes.c_size_t, ctypes.POINTER(ctypes.c_size_t)]
    k.ReadProcessMemory.restype = ctypes.c_int
    for fn in ("Process32FirstW", "Process32NextW", "Module32FirstW", "Module32NextW"):
        getattr(k, fn).argtypes = [ctypes.c_void_p, ctypes.c_void_p]
        getattr(k, fn).restype = ctypes.c_int
    pid = None
    snap = k.CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0)
    if snap and snap != INVALID_HANDLE:
        try:
            e = PROCESSENTRY32W()
            e.dwSize = ctypes.sizeof(e)
            ok = k.Process32FirstW(snap, ctypes.byref(e))
            while ok:
                if e.szExeFile.lower() == EXE_NAME.lower():
                    pid = e.th32ProcessID
                    break
                ok = k.Process32NextW(snap, ctypes.byref(e))
        finally:
            k.CloseHandle(snap)
    if not pid:
        return None, None
    base = None
    for _attempt in range(20):
        snap = k.CreateToolhelp32Snapshot(TH32CS_SNAPMODULE | TH32CS_SNAPMODULE32, pid)
        if snap and snap != INVALID_HANDLE:
            try:
                e = MODULEENTRY32W()
                e.dwSize = ctypes.sizeof(e)
                ok = k.Module32FirstW(snap, ctypes.byref(e))
                while ok:
                    if e.szModule.lower() == EXE_NAME.lower():
                        base = e.modBaseAddr
                        break
                    ok = k.Module32NextW(snap, ctypes.byref(e))
            finally:
                k.CloseHandle(snap)
            break
        time.sleep(0.1)
    if base is None:
        raise SystemExit("the game is running (pid %d) but its module list cannot be read" % pid)
    handle = k.OpenProcess(PROCESS_QUERY_INFORMATION | PROCESS_VM_READ, 0, pid)
    if not handle:
        raise SystemExit("OpenProcess failed for pid %d (run as the same user, or elevated)" % pid)

    def read(addr, size):
        buf = ctypes.create_string_buffer(size)
        got = ctypes.c_size_t(0)
        if not k.ReadProcessMemory(handle, ctypes.c_void_p(addr), buf, size, ctypes.byref(got)) or got.value != size:
            return None
        return buf.raw

    return Memory(read, base), (lambda: k.CloseHandle(handle))


# ------------------------------------------------------------- self-test


class Fake:
    """A tiny fake process image laid out with the same offsets the checks use."""

    def __init__(self, inputs):
        self.inputs = inputs
        self.base = 0x00C40000
        self.data = {}
        self.next = 0x30000000
        self.names = []
        self.name_ids = {}
        self.objects = [0]
        r = inputs["expected"]["reflection"]
        self.r = r
        layout = inputs["layout"]
        self.sym = {k: int(v["rva"], 16) for k, v in layout["symbols"].items()}
        self.glob = {g["name"]: g["rva"] for g in inputs["globals"]["globals"]}
        self.st = layout["structs"]["FNameEntry"]

    def write(self, addr, blob):
        for i, b in enumerate(blob):
            self.data[addr + i] = b

    def alloc(self, size):
        addr = self.next
        self.next += (size + 15) & ~15
        self.write(addr, bytes(size))
        return addr

    def read(self, addr, size):
        try:
            return bytes(self.data[addr + i] for i in range(size))
        except KeyError:
            return None

    def p32(self, addr, value):
        self.write(addr, struct.pack("<I", value & 0xFFFFFFFF))

    def name(self, text):
        if text not in self.name_ids:
            self.name_ids[text] = len(self.names)
            self.names.append(text)
        return self.name_ids[text]

    def obj(self, name, cls, outer, size=0x80):
        addr = self.alloc(size)
        self.write(addr + self.r["object_name"], struct.pack("<ii", self.name(name), 0))
        self.p32(addr + self.r["object_class"], cls)
        self.p32(addr + self.r["object_outer"], outer)
        self.p32(addr + self.r["object_index"], len(self.objects))
        self.objects.append(addr)
        return addr

    def finish(self):
        # name table
        table = self.alloc(4 * len(self.names))
        for i, text in enumerate(self.names):
            wide = i % 2 == 1
            raw = text.encode("utf-16-le") + b"\0\0" if wide else text.encode("latin1") + b"\0"
            e = self.alloc(self.st["members"]["chars"] + len(raw))
            self.p32(e + self.st["members"]["index"], (i << 1) | (1 if wide else 0))
            self.write(e + self.st["members"]["chars"], raw)
            self.p32(table + 4 * i, e)
        self.write(self.base + self.sym["FName::Names"], struct.pack("<Iii", table, len(self.names), len(self.names)))
        arr = self.alloc(4 * len(self.objects))
        for i, o in enumerate(self.objects):
            self.p32(arr + 4 * i, o)
        self.write(self.base + self.glob["UObject::GObjObjects"], struct.pack("<Iii", arr, len(self.objects), len(self.objects)))


EDITOR_PACKAGES = ("UnrealEd", "UTEditor", "GFxUIEditor")  # registered by the executable, never loaded by the game


def selftest_cases(inputs):
    """Three classes of the data files that exercise the two expectations:
    a native class whose property end rounds up to its sizeof by 4, one that
    rounds up by 16, and a native class of an editor package with layout
    fields (registered by the executable, script never loaded)."""
    exp = inputs["expected"]["classes"]
    cols = inputs["class_sizes"]["columns"]
    by4 = by16 = editor = None
    for row in inputs["class_sizes"]["classes"]:
        c = dict(zip(cols, row))
        path = "%s.%s" % (c["package"], c["name"])
        e = exp.get(path)
        if e is None:
            continue
        if c["package"] in EDITOR_PACKAGES:
            if editor is None and e["fields"]:
                editor = path
            continue
        if e["end"] != c["size"]:
            if by4 is None and align_up(e["end"], 4) == c["size"]:
                by4 = path
            elif by16 is None and align_up(e["end"], 4) != c["size"] and align_up(e["end"], 16) == c["size"]:
                by16 = path
    return by4, by16, editor


def build_fake(inputs, break_field=None, sizeof_for=None, drop_field=None, extra=()):
    """A fake image of the classes the checks walk. ``break_field``: one
    property 4 bytes off; ``sizeof_for``: a loaded class that holds its
    sizeof instead of its property end; ``drop_field``: one property without
    its object; ``extra``: further class paths to build."""
    f = Fake(inputs)
    exp = inputs["expected"]
    layout = inputs["layout"]
    r = f.r
    f.name("None")
    # PE header with the time stamp
    f.write(f.base, bytes(0x200))
    f.p32(f.base + 0x3C, 0x80)
    f.p32(f.base + 0x80 + 8, layout["image"]["time_date_stamp"])
    cols = inputs["class_sizes"]["columns"]
    native = {"%s.%s" % (c[cols.index("package")], c[cols.index("name")]): dict(zip(cols, c)) for c in inputs["class_sizes"]["classes"]}
    for c in native.values():
        f.write(f.base + c["private_static_class_rva"], bytes(4))
    uclass = f.obj("Class", 0, 0, 0x1C8)
    f.p32(uclass + r["object_class"], uclass)
    packages = {}
    kinds = {}

    def kind_class(kind):
        if kind not in kinds:
            kinds[kind] = f.obj(kind, uclass, 0, 0x1C8)
        return kinds[kind]

    wanted = sorted(exp.get("defaults", {})) + ["Engine.Actor", "Engine.Pawn", "Engine.Controller", "Engine.Player", "Engine.Engine"]
    wanted += [p for p in extra if p]
    todo = []
    for path in wanted:
        p = path
        while p and p not in todo:
            todo.append(p)
            p = exp["classes"][p].get("super")
    class_objs = {}
    for path in todo:
        e = exp["classes"][path]
        pkg, name = path.split(".")
        if pkg not in packages:
            packages[pkg] = f.obj(pkg, 0, 0)
        if path == "Core.Class":
            cls = uclass
        else:
            cls = f.obj(name, uclass, packages[pkg], 0x1C8)
        class_objs[path] = cls
        n = native.get(path)
        if n:
            f.p32(f.base + n["private_static_class_rva"], cls)
        if pkg in EDITOR_PACKAGES:
            # Registered natively, script never loaded: the constructor's sizeof, no properties.
            f.p32(cls + r["struct_properties_size"], n["size"] if n else e["size"])
            continue
        # A linked class holds the end of its last property.
        f.p32(cls + r["struct_properties_size"], n["size"] if n and path == sizeof_for else e["end"])
        prev = None
        for fname, (offset, bit) in e["fields"].items():
            if (path, fname) == drop_field:
                continue
            kind = "BoolProperty" if bit is not None else "IntProperty"
            prop = f.obj(fname, kind_class(kind), cls, 0x80)
            if (path, fname) == break_field:
                offset += 4
            f.p32(prop + r["property_offset"], offset)
            f.p32(prop + r["property_array_dim"], 1)
            if bit is not None:
                f.p32(prop + r["bool_bitmask"], 1 << bit)
            if prev is None:
                f.p32(cls + r["struct_children"], prop)
            else:
                f.p32(prev + r["field_next"], prop)
            prev = prop
        if prev is not None:
            # a function after the properties: skipped by the walk
            fn = f.obj("SomeFunction", kind_class("Function"), cls, 0x80)
            f.p32(prev + r["field_next"], fn)
    f.p32(f.base + native["Core.Class"]["private_static_class_rva"], uclass)
    if "Core.Class" not in class_objs:
        e = exp["classes"].get("Core.Class")
        f.p32(uclass + r["struct_properties_size"], e["end"] if e else native["Core.Class"]["size"])
    for path, rows in exp.get("defaults", {}).items():
        size = exp["classes"][path]["size"] + 0x40
        cdo = f.obj("Default__" + path.split(".")[-1], class_objs[path], packages[path.split(".")[0]], size)
        for _decl, name, kind, value in rows:
            offset, bit, _owner = expected_field(exp, path, name)
            if kind == "float":
                f.write(cdo + offset, struct.pack("<f", value))
            elif kind == "int":
                f.write(cdo + offset, struct.pack("<i", value))
            elif value:
                cur = struct.unpack("<I", f.read(cdo + offset, 4))[0]
                f.p32(cdo + offset, cur | (1 << bit))
    engine = f.alloc(0x800)
    f.p32(f.base + f.sym["GEngine"], engine)
    f.write(f.base + f.sym["GWorld"], bytes(4))
    f.write(f.base + f.sym["GFrameCounter"], struct.pack("<Q", 7))
    f.finish()
    return f


class _Lines:
    """Collects a report's log lines (self-test)."""

    def __init__(self):
        self.lines = []

    def write(self, text):
        self.lines.append(text)

    def text(self):
        return "".join(self.lines)


def selftest(inputs):
    by4, by16, editor = selftest_cases(inputs)
    cases = [c for c in (by4, by16, editor) if c]
    optional = sorted({f["class"] for f in (inputs.get("optional") or {}).get("fields", [])})
    print("self-test classes: end rounds up by 4: %s; by 16: %s; registered without script: %s" % (by4, by16, editor))

    def run(**kw):
        fake = build_fake(inputs, extra=cases + optional, **kw)
        log = _Lines()
        report = Report(log=log, quiet=True)
        game = Game(Memory(fake.read, fake.base), inputs["layout"], inputs["globals"], inputs["class_sizes"], inputs["expected"],
                    inputs.get("optional"))
        run_checks(game, report, seconds=0)
        return report, log.text()

    results = []
    # 1. A consistent image, with a class whose property end is below its sizeof by each alignment
    #    and a registered class of a package that is not loaded: every check passes.
    report, text = run()
    good = report.failed == 0 and report.passed >= 10 and len(cases) == 3
    good = good and "rounded up to 4: " in text and "4: 0," not in text.split("rounded up to ")[1].split(")")[0]
    good = good and "16: 0" not in text.split("rounded up to ")[1].split(")")[0]
    good = good and ("%s: 1 classes" % editor.split(".")[0]) in text and "0 layout fields not live" in text
    if optional:
        good = good and "[ok] recorder optional fields: " in text and "class not loaded" not in text
    if not good:
        sys.stdout.write(text)
    results.append(("consistent image (property ends below sizeof, an editor class without script)", good))
    # 2. A wrong offset must be caught.
    path = sorted(inputs["expected"].get("defaults", {}))[0]
    field = next(iter(inputs["expected"]["classes"][path]["fields"]))
    report, text = run(break_field=(path, field))
    results.append(("one property moved by 4 bytes is caught", report.failed >= 1 and "%s.%s: live" % (path, field) in text))
    # 3. A linked class that holds its sizeof instead of its property end must be caught (and the
    #    old expectation would have called it right).
    report, text = run(sizeof_for=by4)
    results.append(("a loaded class holding its sizeof instead of its property end is caught",
                    report.failed == 1 and "PropertiesSize not as expected: 1" in text and "live equals the registered sizeof" in text))
    # 4. A property of a loaded class without its object is "not live"; the same for the class of
    #    the unloaded package is not (it has no property objects at all).
    report, text = run(drop_field=(path, field))
    results.append(("a missing property of a loaded class is caught",
                    report.failed == 1 and "1 layout fields not live" in text and "%s.%s" % (path, field) in text))
    ok = True
    for what, passed in results:
        print("self-test (%s): %s" % (what, "ok" if passed else "FAILED"))
        ok = ok and passed
    return 0 if ok else 1


def default_log_path():
    """logs/ next to the script (the working folder on the game machine);
    in a repository checkout the git-ignored research/local/win/live-checks."""
    here = os.path.dirname(os.path.abspath(__file__))
    root = os.path.abspath(os.path.join(here, "..", "..", ".."))
    if os.path.isfile(os.path.join(root, "Cargo.toml")) and os.path.isdir(os.path.join(root, "research")):
        folder = os.path.join(root, "research", "local", "win", "live-checks")
    else:
        folder = os.path.join(here, "logs")
    return os.path.join(folder, "win32_props_check-%s.log" % time.strftime("%Y%m%dT%H%M%SZ", time.gmtime()))


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("--expected", help="expected_win32.json")
    ap.add_argument("--layout", help="layout_win_x86.json")
    ap.add_argument("--data", help="directory with globals.json and class_sizes.json")
    ap.add_argument("--seconds", type=float, default=1.0, help="interval of the frame-counter check")
    ap.add_argument("--verbose", action="store_true", help="list every difference on the console too")
    ap.add_argument("--log", help="file for the full output (default: see the module text)")
    ap.add_argument("--no-log", action="store_true", help="write no log file")
    ap.add_argument("--selftest", action="store_true", help="check the checker on a fake image; no game needed")
    args = ap.parse_args()
    inputs = load_inputs(args)
    if args.selftest:
        return selftest(inputs)
    mem, close = open_game()
    if mem is None:
        print("%s is not running" % EXE_NAME)
        return 2
    log = None
    log_path = None if args.no_log else (args.log or default_log_path())
    if log_path:
        os.makedirs(os.path.dirname(os.path.abspath(log_path)), exist_ok=True)
        log = open(log_path, "w", encoding="utf-8", newline="\n")
        log.write("win32_props_check %s UTC; module base %#x\n" % (time.strftime("%Y-%m-%d %H:%M:%S", time.gmtime()), mem.base))
    report = Report(args.verbose, log=log)
    try:
        game = Game(mem, inputs["layout"], inputs["globals"], inputs["class_sizes"], inputs["expected"], inputs.get("optional"))
        run_checks(game, report, args.seconds)
    except ReadError as error:
        report.check(False, "stopped: %s" % error)
    finally:
        close()
    report._line("%d checks passed, %d failed" % (report.passed, report.failed))
    if log is not None:
        log.close()
        print("full output: %s" % log_path)
    return 1 if report.failed else 0


if __name__ == "__main__":
    sys.exit(main())
