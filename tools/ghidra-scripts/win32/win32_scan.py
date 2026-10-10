#!/usr/bin/env python3
"""Static scan of the original Win32 executable (ASAMU-Win32-Shipping.exe).

Our own code. It reads the user's own executable and writes *sanitized*
metadata only: relative virtual addresses, sizes, class and function names,
counts and a few 2-8 byte instruction encodings that restate an offset. It
never writes code, strings dumps or memory contents.

What it derives (docs/reverse-engineering/WINDOWS_BINARY.md explains each step)
-------------------------------------------------------------------------------
* image.json           PE identification: base, ASLR, sections, linker, imports
* class_sizes.json     every UE3 native class registration: package, name,
                       sizeof, class flags, super, within, primary vtable,
                       ``PrivateStaticClass`` pointer
* globals.json         global variables the trace recorder needs
* functions.json       functions the trace recorder needs (frame boundary)
* native_evidence.json struct offsets shown by instruction encodings

Every address is found from a string literal, an export or an already found
address; nothing is hard-coded except the instruction shapes. Each derivation
asserts what it expects and the run fails (exit 1) when an expectation breaks.

Requirements: Python 3.9+, and an LLVM ``objdump`` that can disassemble
PE/i386 (the Xcode command line tools' ``objdump`` or ``llvm-objdump``; set
``OBJDUMP`` to choose one). No third-party Python modules.

    python3 tools/ghidra-scripts/win32/win32_scan.py EXE --out docs/reverse-engineering/data/win32
    python3 tools/ghidra-scripts/win32/win32_scan.py EXE --out docs/reverse-engineering/data/win32 --check

``--check`` regenerates everything in memory and compares it with the files
already in ``--out`` (exit 1 on any difference).
"""

import argparse
import hashlib
import json
import math
import os
import re
import struct
import subprocess
import sys
import time
from collections import Counter, defaultdict

SCHEMA_PREFIX = "asamu-decomp/win32"
EXPECTED_SHA256 = "17f2aeb601086089f4cbf144d2eb2b4b672c2d0594ad81f8f75ba27736d26e4e"
BUILD = "Win32 (Steam build 1822049)"
GAME_BUILD = "steam-1822049-win32"

# UE3 EObjectFlags passed by IMPLEMENT_CLASS: RF_Public | RF_Standalone |
# RF_Transient | RF_Native | RF_RootSet | RF_DisregardForGC (stock UE3).
STATIC_CLASS_FLAGS_HI = 0x04084084
STATIC_CLASS_FLAGS_LO = 0x00004000
CLASS_DEPRECATED = 0x02000000


class ScanError(Exception):
    pass


# ----------------------------------------------------------------------- PE


class PE:
    """Just enough of a PE32 reader."""

    def __init__(self, data):
        self.d = d = data
        if d[:2] != b"MZ":
            raise ScanError("not an MZ file")
        self.e_lfanew = struct.unpack_from("<I", d, 0x3C)[0]
        o = self.e_lfanew
        if d[o : o + 4] != b"PE\0\0":
            raise ScanError("no PE signature")
        (self.machine, nsec, self.timestamp, _symptr, _nsyms, optsize, self.characteristics) = struct.unpack_from(
            "<HHIIIHH", d, o + 4
        )
        opt = o + 24
        self.magic = struct.unpack_from("<H", d, opt)[0]
        if self.magic != 0x10B:
            raise ScanError("not PE32")
        self.linker = struct.unpack_from("<BB", d, opt + 2)
        self.entry_rva = struct.unpack_from("<I", d, opt + 16)[0]
        self.image_base, self.section_alignment, self.file_alignment = struct.unpack_from("<III", d, opt + 28)
        self.os_version = struct.unpack_from("<HH", d, opt + 40)
        self.subsystem_version = struct.unpack_from("<HH", d, opt + 48)
        (_w, self.size_of_image, self.size_of_headers, self.checksum, self.subsystem, self.dll_characteristics) = (
            struct.unpack_from("<IIIIHH", d, opt + 52)
        )
        ndirs = struct.unpack_from("<I", d, opt + 92)[0]
        self.dirs = [struct.unpack_from("<II", d, opt + 96 + 8 * i) for i in range(ndirs)]
        self.sections = []
        so = opt + optsize
        for i in range(nsec):
            name = d[so : so + 8].rstrip(b"\0").decode("latin1")
            vsize, va, rsize, rptr = struct.unpack_from("<IIII", d, so + 8)
            chars = struct.unpack_from("<I", d, so + 36)[0]
            self.sections.append({"name": name, "rva": va, "vsize": vsize, "rptr": rptr, "rsize": rsize, "chars": chars})
            so += 40
        self.by_name = {s["name"]: s for s in self.sections}

    # address helpers (va = preferred-base virtual address)
    def section_of(self, va):
        r = va - self.image_base
        for s in self.sections:
            if s["rva"] <= r < s["rva"] + max(s["vsize"], s["rsize"]):
                return s
        return None

    def off(self, va):
        """File offset of va, or None when it is outside the file data."""
        s = self.section_of(va)
        if s is None:
            return None
        delta = va - self.image_base - s["rva"]
        return s["rptr"] + delta if delta < s["rsize"] else None

    def va_of_off(self, off):
        for s in self.sections:
            if s["rptr"] <= off < s["rptr"] + s["rsize"]:
                return self.image_base + s["rva"] + off - s["rptr"]
        return None

    def within(self, va, name):
        s = self.by_name[name]
        r = va - self.image_base
        return s["rva"] <= r < s["rva"] + s["vsize"]

    def u32(self, va):
        o = self.off(va)
        return None if o is None else struct.unpack_from("<I", self.d, o)[0]

    def cstr(self, va, limit=512):
        o = self.off(va)
        if o is None:
            return None
        e = self.d.find(b"\0", o, o + limit)
        return self.d[o:e].decode("latin1")

    def wstr(self, va, limit=512):
        """NUL-terminated UTF-16LE text at va (printable ASCII only), else None."""
        o = self.off(va)
        if o is None:
            return None
        out = []
        for i in range(limit):
            if o + 2 * i + 2 > len(self.d):
                return None
            c = struct.unpack_from("<H", self.d, o + 2 * i)[0]
            if c == 0:
                return "".join(out)
            if not (0x20 <= c < 0x7F or c in (9, 10, 13)):
                return None
            out.append(chr(c))
        return None

    def rva(self, va):
        return va - self.image_base


def entropy(b):
    if not b:
        return 0.0
    counts = Counter(b)
    n = len(b)
    return -sum(c / n * math.log2(c / n) for c in counts.values())


# ------------------------------------------------------------- disassembler

INS = re.compile(r"^\s*([0-9a-f]+):\s+(\S+)\s*(.*)$")


class Disassembler:
    """Linear disassembly of address ranges through LLVM objdump (Intel syntax)."""

    def __init__(self, path, objdump):
        self.path = path
        self.objdump = objdump
        self.cache = {}

    def lines(self, start, length=0x400):
        key = (start, length)
        if key in self.cache:
            return self.cache[key]
        cmd = [
            self.objdump,
            "-d",
            "--no-show-raw-insn",
            "--x86-asm-syntax=intel",
            "--start-address=%#x" % start,
            "--stop-address=%#x" % (start + length),
            self.path,
        ]
        res = subprocess.run(cmd, capture_output=True, text=True)
        if res.returncode != 0:
            raise ScanError("objdump failed: %s" % res.stderr.strip()[:200])
        out = []
        for line in res.stdout.splitlines():
            m = INS.match(line)
            if not m:
                continue
            ops = re.sub(r"\s*<[^>]*>", "", m.group(3)).replace("\t", " ").strip()
            ops = re.sub(r"\s+#.*$", "", ops)
            out.append((int(m.group(1), 16), m.group(2), ops))
        if len(self.cache) > 64:
            self.cache.clear()
        self.cache[key] = out
        return out

    def function(self, start, limit=0x4000):
        """Instructions from start up to the first int3 padding (or limit)."""
        out = []
        for ins in self.lines(start, limit):
            if ins[1] == "int3" and len(out) > 2:
                break
            out.append(ins)
        return out


def check_objdump(objdump):
    try:
        res = subprocess.run([objdump, "--version"], capture_output=True, text=True)
    except OSError as e:
        raise ScanError("cannot run %s: %s" % (objdump, e))
    if "LLVM" not in res.stdout:
        raise ScanError("%s is not an LLVM objdump (set OBJDUMP=llvm-objdump)" % objdump)


# ------------------------------------------------------------------ scanner


class Scanner:
    def __init__(self, path, objdump):
        with open(path, "rb") as fh:
            data = fh.read()
        self.pe = PE(data)
        self.d = data
        self.sha256 = hashlib.sha256(data).hexdigest()
        self.dis = Disassembler(path, objdump)
        self.text = self.pe.by_name[".text"]
        self.text_lo = self.pe.image_base + self.text["rva"]
        self.text_hi = self.text_lo + self.text["vsize"]
        self.checks = []  # (ok, text)
        self._calls = None
        self.imports = {}  # iat slot va -> "dll!name"
        self.import_dlls = []  # (dll, count)

    # -- bookkeeping
    def check(self, ok, text):
        self.checks.append((bool(ok), text))
        return bool(ok)

    def require(self, ok, text):
        if not self.check(ok, text):
            raise ScanError("expectation failed: " + text)

    # -- byte searches
    def find_all(self, needle, lo=0, hi=None):
        d = self.d
        hi = len(d) if hi is None else hi
        out = []
        i = d.find(needle, lo, hi)
        while i >= 0:
            out.append(i)
            i = d.find(needle, i + 1, hi)
        return out

    def text_range(self):
        return self.text["rptr"], self.text["rptr"] + self.text["rsize"]

    def wide_literal(self, s, prefix=False):
        """Addresses of the NUL-terminated UTF-16 literal s (or of literals starting
        with s when prefix is set) at a 2-byte aligned offset."""
        b = s.encode("utf-16-le") + (b"" if prefix else b"\0\0")
        out = []
        for o in self.find_all(b):
            va = self.pe.va_of_off(o)
            if va is None or o % 2:
                continue
            if o >= 2 and self.d[o - 2 : o] != b"\0\0" and o % 4:
                continue
            out.append(va)
        return out

    def refs(self, va):
        """Addresses in .text of 4-byte operands equal to va."""
        lo, hi = self.text_range()
        return [self.pe.va_of_off(o) for o in self.find_all(struct.pack("<I", va), lo, hi)]

    def call_index(self):
        """target -> [call sites] for every E8 rel32 whose target is in .text (byte scan)."""
        if self._calls is None:
            lo, hi = self.text_range()
            d = self.d
            idx = defaultdict(list)
            i = d.find(b"\xe8", lo, hi)
            while 0 <= i < hi - 5:
                site = self.text_lo + (i - lo)
                target = (site + 5 + struct.unpack_from("<i", d, i + 1)[0]) & 0xFFFFFFFF
                if self.text_lo <= target < self.text_hi:
                    idx[target].append(site)
                i = d.find(b"\xe8", i + 1, hi)
            self._calls = idx
        return self._calls

    def is_code(self, va):
        return self.text_lo <= va < self.text_hi

    def is_function_start(self, va):
        """Functions are 16-byte aligned and separated by int3 padding that follows a
        return, a jump, a call that does not return, or a jump table."""
        if va & 15:
            return False
        d = self.d
        o = self.pe.off(va)
        if o is None or d[o] == 0xCC:
            return False
        q = o - 1
        if d[q] != 0xCC:
            # no padding: the previous function ended exactly here
            ends = d[q] == 0xC3 or d[q - 2] == 0xC2 or d[q - 4] == 0xE9
            return ends and va in self.call_index()
        while d[q] == 0xCC:
            q -= 1
        if d[q] == 0xC3 or d[q - 2] == 0xC2 or d[q - 4] in (0xE9, 0xE8) or d[q - 1] == 0xEB:
            return True
        if (d[q - 1] == 0xFF and 0xE0 <= d[q] <= 0xE7) or (d[q - 5] == 0xFF and d[q - 4] == 0x25):
            return True
        if d[q - 6] == 0xFF and d[q - 5] == 0x24:
            return True
        return self.is_code(struct.unpack_from("<I", d, q - 3)[0])  # jump table entry

    def enclosing_function(self, va, back=0x8000):
        """Start of the function containing va (nearest preceding function start).
        Each use is checked by what the function must contain."""
        a = va & ~15
        while a > va - back:
            if self.is_function_start(a):
                return a
            a -= 16
        return None

    # -------------------------------------------------------------- image

    def scan_image(self):
        pe = self.pe
        d = self.d
        self.require(self.sha256 == EXPECTED_SHA256, "executable SHA-256 is the analysed build's")
        self.require(pe.machine == 0x14C, "machine is i386")
        sections = []
        for s in pe.sections:
            raw = d[s["rptr"] : s["rptr"] + s["rsize"]]
            sections.append(
                {
                    "name": s["name"],
                    "rva": s["rva"],
                    "virtual_size": s["vsize"],
                    "raw_offset": s["rptr"],
                    "raw_size": s["rsize"],
                    "characteristics": "0x%08X" % s["chars"],
                    "entropy_bits_per_byte": round(entropy(raw), 3),
                }
            )
        last_raw = max(s["rptr"] + s["rsize"] for s in pe.sections)
        # imports
        dlls = []
        rva, _size = pe.dirs[1]
        o = pe.off(pe.image_base + rva)
        while True:
            ilt, _ts, _fwd, name, iat = struct.unpack_from("<IIIII", d, o)
            if not (ilt or name or iat):
                break
            dll = pe.cstr(pe.image_base + name)
            n = 0
            while True:
                v = pe.u32(pe.image_base + (ilt or iat) + 4 * n)
                if not v:
                    break
                fn = "#%d" % (v & 0xFFFF) if v & 0x80000000 else pe.cstr(pe.image_base + v + 2)
                self.imports[pe.image_base + iat + 4 * n] = "%s!%s" % (dll, fn)
                n += 1
            dlls.append((dll, n))
            o += 20
        self.import_dlls = dlls
        delay = []
        rva, _size = pe.dirs[13]
        if rva:
            o = pe.off(pe.image_base + rva)
            while True:
                attrs, name = struct.unpack_from("<II", d, o)
                if not name:
                    break
                base = 0 if attrs & 1 else pe.image_base
                delay.append(pe.cstr(pe.image_base + name - base))
                o += 32
        steam = sorted(n.split("!")[1] for n in self.imports.values() if n.lower().startswith("steam_api.dll!"))
        # exports (names only)
        exports = []
        rva, _size = pe.dirs[0]
        if rva:
            o = pe.off(pe.image_base + rva)
            (_c, _t, _maj, _min, _name, _base, _nfun, nnames, afun, anames, aords) = struct.unpack_from("<IIHHIIIIIII", d, o)
            for i in range(nnames):
                nm = pe.cstr(pe.image_base + pe.u32(pe.image_base + anames + 4 * i))
                ordi = struct.unpack_from("<H", d, pe.off(pe.image_base + aords + 2 * i))[0]
                frva = pe.u32(pe.image_base + afun + 4 * ordi)
                exports.append({"name": nm, "rva": frva, "section": pe.section_of(pe.image_base + frva)["name"]})
        # debug directory: PDB file name only (the build-machine directory is not reproduced)
        pdb = None
        rva, size = pe.dirs[6]
        if rva:
            o = pe.off(pe.image_base + rva)
            for i in range(size // 28):
                _c, _t, _maj, _min, typ, sod, _arva, ptr = struct.unpack_from("<IIHHIIII", d, o + 28 * i)
                if typ == 2 and d[ptr : ptr + 4] == b"RSDS":
                    path = d[ptr + 24 : ptr + sod].split(b"\0")[0].decode("latin1")
                    pdb = {
                        "file_name": re.split(r"[\\/]", path)[-1],
                        "guid": d[ptr + 4 : ptr + 20].hex(),
                        "age": struct.unpack_from("<I", d, ptr + 20)[0],
                        "shipped_with_the_game": False,
                    }
        # CLR header
        clr = None
        rva, size = pe.dirs[14]
        if rva:
            o = pe.off(pe.image_base + rva)
            _cb, maj, mnr, md_rva, _md_size, flags, entry_token = struct.unpack_from("<IHHIIII", d, o)
            mo = pe.off(pe.image_base + md_rva)
            vlen = struct.unpack_from("<I", d, mo + 12)[0]
            clr = {
                "runtime_header_version": "%d.%d" % (maj, mnr),
                "metadata_version": d[mo + 16 : mo + 16 + vlen].rstrip(b"\0").decode("ascii"),
                "flags": "0x%X" % flags,
                "il_only": bool(flags & 1),
                "managed_entry_point_token": "0x%08X" % entry_token,
            }
        # relocations
        nreloc = 0
        rva, size = pe.dirs[5]
        if rva:
            o = pe.off(pe.image_base + rva)
            end = o + size
            while o < end:
                _page, block = struct.unpack_from("<II", d, o)
                if block == 0:
                    break
                for j in range((block - 8) // 2):
                    if struct.unpack_from("<H", d, o + 8 + 2 * j)[0] >> 12 == 3:
                        nreloc += 1
                o += block
        # Rich header (tool id, build, object count)
        rich = []
        ri = d.find(b"Rich", 0x80, pe.e_lfanew)
        if ri > 0:
            key = struct.unpack_from("<I", d, ri + 4)[0]
            vals = []
            i = ri - 4
            while i >= 0x80:
                v = struct.unpack_from("<I", d, i)[0] ^ key
                if v == 0x536E6144:
                    break
                vals.append(v)
                i -= 4
            vals.reverse()
            ents = vals[3:]
            for j in range(0, len(ents) - 1, 2):
                rich.append({"tool_id": ents[j] >> 16, "build": ents[j] & 0xFFFF, "count": ents[j + 1]})
        entry_off = pe.off(pe.image_base + pe.entry_rva)
        entry_target = None
        if d[entry_off : entry_off + 2] == b"\xff\x25":
            entry_target = self.imports.get(struct.unpack_from("<I", d, entry_off + 2)[0])
        wide16, wide32 = self.count_wide_literals()
        dynamic_base = bool(pe.dll_characteristics & 0x0040)
        self.require(dynamic_base and nreloc > 0, "DYNAMICBASE is set and base relocations are present")
        self.require(".bind" not in pe.by_name and last_raw == len(d), "no .bind section and no overlay (no Steam DRM wrapper)")
        self.require(entry_target == "mscoree.dll!_CorExeMain", "entry point is the CLR start-up stub")
        self.require(wide16 > 1000 and wide32 * 1000 < wide16, "wide literals are UTF-16 (UTF-32-shaped runs are negligible)")
        return {
            "schema": SCHEMA_PREFIX + "/image/v1",
            "build": BUILD,
            "game_build": GAME_BUILD,
            "executable": "Binaries/Win32/ASAMU-Win32-Shipping.exe",
            "file_size": len(d),
            "sha256": self.sha256,
            "format": "PE32 (i386)",
            "image_base": "0x%08X" % pe.image_base,
            "size_of_image": pe.size_of_image,
            "entry_point_rva": pe.entry_rva,
            "entry_point_target": entry_target,
            "time_date_stamp": pe.timestamp,
            "time_date_stamp_utc": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime(pe.timestamp)),
            "linker_version": "%d.%d" % pe.linker,
            "os_version": "%d.%d" % pe.os_version,
            "subsystem": pe.subsystem,
            "subsystem_version": "%d.%d" % pe.subsystem_version,
            "characteristics": "0x%04X" % pe.characteristics,
            "large_address_aware": bool(pe.characteristics & 0x20),
            "dll_characteristics": "0x%04X" % pe.dll_characteristics,
            "dynamic_base": dynamic_base,
            "nx_compatible": bool(pe.dll_characteristics & 0x0100),
            "relocations_stripped": bool(pe.characteristics & 1),
            "base_relocations_highlow": nreloc,
            "address_rule": "runtime address = module base + rva; preferred-base address (Ghidra) = image_base + rva",
            "sections": sections,
            "overlay_bytes": len(d) - last_raw,
            "tchar": {
                "type": "wchar_t, 2 bytes (UTF-16LE)",
                "utf16_literals_in_rdata": wide16,
                "utf32_shaped_runs_in_rdata": wide32,
                "note": "runs of 4+ printable units followed by a NUL unit; the few UTF-32-shaped runs are consecutive "
                "one-character UTF-16 literals or small integer tables. Every native class name is a UTF-16 literal.",
            },
            "imports": [{"dll": dll, "functions": n} for dll, n in dlls],
            "imported_functions_total": sum(n for _dll, n in dlls),
            "delay_imports": delay,
            "steam_api_imports": steam,
            "exports": exports,
            "debug_pdb": pdb,
            "clr_header": clr,
            "rich_header": rich,
            "tls_directory": bool(pe.dirs[9][0]),
        }

    def count_wide_literals(self):
        """Aligned printable literals of >= 4 units followed by a NUL unit in .rdata."""
        s = self.pe.by_name[".rdata"]
        blob = self.d[s["rptr"] : s["rptr"] + s["rsize"]]
        n16 = len(re.findall(rb"(?:[\x20-\x7e]\x00){4,}\x00\x00", blob))
        n32 = len(re.findall(rb"(?:[\x20-\x7e]\x00\x00\x00){4,}\x00\x00\x00\x00", blob))
        return n16, n32

    # --------------------------------------------------------------- RTTI

    def scan_rtti(self):
        """Census of MSVC RTTI: type descriptors (.data), complete-object locators and the
        vtables that point at them (.rdata). Needs the class table."""
        pe = self.pe
        d = self.d
        data = pe.by_name[".data"]
        blob = d[data["rptr"] : data["rptr"] + data["rsize"]]
        cand = {}
        for m in re.finditer(rb"\.\?A[VUTW][^\0]{1,400}\0", blob):
            cand[pe.image_base + data["rva"] + m.start() - 8] = m.group()[:-1].decode("latin1")
        # every descriptor starts with the address of type_info's vtable
        first = Counter(pe.u32(va) for va in cand)
        type_info_vtable = first.most_common(1)[0][0]
        descriptors = {va: n for va, n in cand.items() if pe.u32(va) == type_info_vtable}
        rd = pe.by_name[".rdata"]
        words = struct.unpack_from("<%dI" % (rd["rsize"] // 4), d, rd["rptr"])
        base = pe.image_base + rd["rva"]
        locators = {}
        for i in range(len(words) - 5):
            if words[i] == 0 and words[i + 3] in descriptors and pe.within(words[i + 4], ".rdata"):
                locators[base + 4 * i] = words[i + 3]
        vtables = [base + 4 * (i + 1) for i in range(len(words) - 1) if words[i] in locators and self.is_code(words[i + 1])]
        located = {descriptors[locators[pe.u32(v - 4)]] for v in vtables}
        native = {n: ".?AV%s@@" % n for n in self.classes}
        with_descriptor = sorted(n for n, mangled in native.items() if mangled in descriptors.values())
        with_locator = sorted(n for n, mangled in native.items() if mangled in located)
        self.require(not with_locator, "no native UObject class has an RTTI locator")
        return {
            "type_descriptors": len(descriptors),
            "complete_object_locators": len(locators),
            "vtables_with_locator": len(vtables),
            "native_classes_with_type_descriptor": with_descriptor,
            "native_classes_with_locator": len(with_locator),
            "note": "the engine is compiled without RTTI; the descriptors belong to third-party, editor and a few helper types",
        }

    # ------------------------------------------------------------ classes

    def parse_ctor_pushes(self, call_site):
        """The 12 pushes before ``mov ecx, r; call UClass::UClass`` by a backward parse.

        Returns a list in push order (last pushed last) of ("imm", value) or
        ("reg", n). Exactly one parse must have the fixed object-flag words and
        three code addresses in front of them.
        """
        d = self.d
        o = self.pe.off(call_site)
        if not (d[o - 2] == 0x8B and (d[o - 1] & 0xF8) == 0xC8):
            return None
        found = []

        def walk(pos, acc):
            if len(acc) == 12:
                seq = acc[::-1]
                fixed = seq[3] == ("imm", STATIC_CLASS_FLAGS_HI) and seq[4] == ("imm", STATIC_CLASS_FLAGS_LO)
                # the three trailing arguments are function addresses
                if fixed and all(k == "imm" and self.is_code(v) for k, v in seq[:3]):
                    found.append(seq)
                return
            if d[pos - 5] == 0x68:
                walk(pos - 5, acc + [("imm", struct.unpack_from("<I", d, pos - 4)[0])])
            if d[pos - 2] == 0x6A:
                walk(pos - 2, acc + [("imm", struct.unpack_from("<b", d, pos - 1)[0] & 0xFFFFFFFF)])
            if 0x50 <= d[pos - 1] <= 0x57:
                walk(pos - 1, acc + [("reg", d[pos - 1] - 0x50)])

        walk(o - 2, [])
        uniq = []
        for f in found:
            if f not in uniq:
                uniq.append(f)
        return uniq[0] if len(uniq) == 1 else None

    def scan_classes(self):
        pe = self.pe
        d = self.d
        calls = self.call_index()
        # 1. UClass::UClass(EC_StaticConstructor, ...) from the game's own native class.
        lits = self.wide_literal("UASAMUSystemSettingsManager")
        self.require(len(lits) == 1, "one UTF-16 literal UASAMUSystemSettingsManager")
        name_refs = self.refs(lits[0] + 2)
        self.require(len(name_refs) == 1, "one code reference to that literal + 1 character")
        o = pe.off(name_refs[0])
        k = d.find(b"\xe8", o, o + 24)
        ctor = (pe.va_of_off(k) + 5 + struct.unpack_from("<i", d, k + 1)[0]) & 0xFFFFFFFF
        sites = sorted(calls.get(ctor, []))
        self.uclass_ctor = ctor
        rows = []
        for site in sites:
            p = self.parse_ctor_pushes(site)
            if p is None:
                raise ScanError("unparsed UClass constructor call at rva %#x" % pe.rva(site))
            _intrinsic, _static_ctor, internal_ctor, _hi, _lo, config, package, name_ptr, cast, cflags, size, ec = p
            if not (internal_ctor[0] == "imm" and name_ptr[0] == "imm" and size[0] == "imm" and config[0] == "imm"):
                raise ScanError("unexpected register argument at rva %#x" % pe.rva(site))
            # A register in the flag slots is the zeroed register that is also pushed as
            # EC_StaticConstructor (0).
            zero_regs = set()
            if ec[0] == "reg":
                zero_regs.add(ec[1])
            elif ec[1] != 0:
                raise ScanError("EC_StaticConstructor is not 0 at rva %#x" % pe.rva(site))

            def value(arg):
                if arg[0] == "imm":
                    return arg[1]
                if arg[1] in zero_regs:
                    return 0
                raise ScanError("unknown register argument at rva %#x" % pe.rva(site))

            cflags_v = value(cflags)
            cast_v = value(cast)
            back = 24 if cflags_v & CLASS_DEPRECATED else 2
            cpp_name = pe.wstr(name_ptr[1] - back)
            script_name = pe.wstr(name_ptr[1])
            if not cpp_name or not script_name or not cpp_name.endswith(script_name):
                raise ScanError("bad class name literal at rva %#x" % pe.rva(site))
            row = {
                "site": site,
                "cpp_name": cpp_name,
                "name": script_name,
                "size": size[1],
                "class_flags": cflags_v,
                "cast_flags": cast_v,
                "config": pe.wstr(config[1]),
                "internal_ctor": internal_ctor[1],
                "package": None,
                "psc": Counter(),
                "init": Counter(),
                "init_inline": Counter(),
            }
            if package[0] == "imm":
                row["package"] = pe.wstr(package[1])
                # inlined StaticClass(): "mov [PrivateStaticClass], eax" follows the call
                after = d[pe.off(site) + 5 : pe.off(site) + 5 + 0x30]
                m = re.search(rb"\xa3(....)[\xe8\xe9](....)", after, re.S)
                if m and after[: m.start()].count(b"\xe8") == 0:
                    row["psc"][struct.unpack("<I", m.group(1))[0]] += 1
                    row["init"][(site + 5 + m.end() + struct.unpack("<i", m.group(2))[0]) & 0xFFFFFFFF] += 1
            else:
                # out-of-line Get<Class>PrivateStaticClass(const TCHAR* Package)
                so = pe.off(site)
                win = d[so - 0xC0 : so]
                k = win.rfind(b"\x6a\xff\x68")
                if k < 0 or win[k + 7 : k + 13] != b"\x64\xa1\x00\x00\x00\x00":
                    raise ScanError("no function prologue before rva %#x" % pe.rva(site))
                getter = site - 0xC0 + k
                packages = set()
                for c in calls.get(getter, []):
                    co = pe.off(c)
                    if d[co - 5] != 0x68:
                        continue
                    pkg = pe.wstr(struct.unpack_from("<I", d, co - 4)[0])
                    if not pkg:
                        continue
                    packages.add(pkg)
                    after = d[co + 5 : co + 5 + 0x18]
                    m = re.match(rb"(?:\x83\xc4\x04)?\xa3(....)(?:\x83\xc4.)?", after, re.S)
                    if m:
                        row["psc"][struct.unpack("<I", m.group(1))[0]] += 1
                        nxt = c + 5 + m.end()
                        if after[m.end()] in (0xE8, 0xE9):
                            # call / tail jump to Initialize<Class>PrivateStaticClass
                            row["init"][(nxt + 5 + struct.unpack_from("<i", after, m.end() + 1)[0]) & 0xFFFFFFFF] += 1
                        else:
                            # that function is inlined here and its code follows
                            row["init_inline"][nxt] += 1
                if len(packages) != 1:
                    raise ScanError("package of %s is not unique: %r" % (cpp_name, sorted(packages)))
                row["package"] = packages.pop()
            rows.append(row)
        # merge the inlined copies of one class
        classes = {}
        for r in rows:
            c = classes.get(r["cpp_name"])
            if c is None:
                c = classes[r["cpp_name"]] = dict(r, sites=1)
                continue
            for key in ("name", "size", "class_flags", "cast_flags", "config", "internal_ctor", "package"):
                if c[key] != r[key]:
                    raise ScanError("registrations of %s disagree on %s" % (r["cpp_name"], key))
            c["psc"] += r["psc"]
            c["init"] += r["init"]
            c["init_inline"] += r["init_inline"]
            c["sites"] += 1
        for c in classes.values():
            # Every inlined StaticClass() stores the result to the class's PrivateStaticClass and
            # then calls its init function. The call index is a byte scan, so allow a stray hit:
            # the winner must have at least 99% of the votes.
            if not c["init"]:
                c["init"] = Counter(dict(sorted(c["init_inline"].items())[:1]))  # any inlined copy will do
            for key in ("psc", "init"):
                votes = c[key]
                if not votes:
                    raise ScanError("%s: no %s found" % (c["cpp_name"], key))
                best, n = votes.most_common(1)[0]
                if n * 100 < sum(votes.values()) * 99:
                    raise ScanError("%s: %s is not unique: %r" % (c["cpp_name"], key, votes.most_common(3)))
                c[key] = best
        psc_owner = {}
        for c in classes.values():
            if c["psc"] in psc_owner:
                raise ScanError("PrivateStaticClass shared by %s and %s" % (c["cpp_name"], psc_owner[c["psc"]]))
            psc_owner[c["psc"]] = c["cpp_name"]
        self.check(True, "%d UClass static-constructor call sites parsed, %d distinct classes" % (len(rows), len(classes)))
        self.registration_sites = len(rows)
        self.classes = classes
        self.psc_owner = psc_owner
        self.scan_supers()
        self.scan_vtables()
        return classes

    def scan_supers(self):
        """Super and Within from Initialize<Class>PrivateStaticClass: it pushes
        (Within::StaticClass(), PrivateStaticClass, Super::StaticClass()) and calls the
        shared InitializePrivateStaticClass."""
        asamu = self.classes["UASAMUSystemSettingsManager"]
        common = None
        for _a, mn, ops in self.dis.function(asamu["init"], 0x100):
            if mn == "call" and re.match(r"^0x[0-9a-f]+$", ops):
                common = int(ops, 16)  # the last call is the shared function
        self.require(common is not None, "shared InitializePrivateStaticClass found")
        self.init_common = common
        psc_owner = self.psc_owner
        for c in self.classes.values():
            regs = {}
            pushes = []
            result = None
            for _a, mn, ops in self.dis.lines(c["init"], 0x200):
                if mn == "mov":
                    m = re.match(r"^(e[a-ds][xip]), dword ptr \[0x([0-9a-f]+)\]$", ops)
                    if m:
                        regs[m.group(1)] = int(m.group(2), 16)
                        continue
                    m = re.match(r"^(e[a-ds][xip]), (e[a-ds][xip])$", ops)
                    if m:
                        regs[m.group(1)] = regs.get(m.group(2))
                elif mn == "push":
                    m = re.match(r"^(e[a-ds][xip])$", ops)
                    if m:
                        pushes.append(regs.get(m.group(1)))
                        continue
                    m = re.match(r"^dword ptr \[0x([0-9a-f]+)\]$", ops)
                    pushes.append(int(m.group(1), 16) if m else "other")
                elif mn == "pop":
                    if pushes:
                        pushes.pop()
                elif mn in ("call", "jmp"):
                    m = re.match(r"^0x([0-9a-f]+)$", ops)
                    if m and int(m.group(1), 16) == common:
                        result = pushes[-3:]
                        break
                    if mn == "call" and pushes and pushes[-1] == "other":
                        pushes.pop()  # the package literal of an inlined StaticClass()
                elif mn == "int3":
                    break
            if not result or len(result) != 3 or any(x not in psc_owner for x in result):
                raise ScanError("cannot read super of %s" % c["cpp_name"])
            within, me, sup = (psc_owner[x] for x in result)
            if me != c["cpp_name"]:
                raise ScanError("init function of %s registers %s" % (c["cpp_name"], me))
            c["super"] = None if sup == c["cpp_name"] else sup
            c["within"] = within
        self.require(self.classes["UObject"]["super"] is None, "UObject has no super")
        roots = [n for n, c in self.classes.items() if c["super"] is None]
        self.require(roots == ["UObject"], "UObject is the only root class")

    def is_vtable(self, va):
        if not self.pe.within(va, ".rdata"):
            return False
        w = self.pe.u32(va)
        return w is not None and self.is_code(w)

    def ctor_vtables(self, func, this_reg=None, depth=0):
        """(primary vtable, [(offset, vtable)]) stored into the object by a constructor."""
        regs = {this_reg} if this_reg else set()
        stores = []
        last_call = None
        tail = None
        for _a, mn, ops in self.dis.lines(func, 0x400):
            if mn == "int3":
                break
            if mn == "mov":
                m = re.match(r"^(e[a-ds][xip]), dword ptr \[esp \+ 0x[0-9a-f]+\]$", ops)
                if m and this_reg is None and not regs:
                    regs.add(m.group(1))
                    continue
                m = re.match(r"^(e[a-ds][xip]), (e[a-ds][xip])$", ops)
                if m:
                    if m.group(2) in regs:
                        regs.add(m.group(1))
                    elif m.group(1) in regs and m.group(1) != "ecx":
                        regs.discard(m.group(1))
                    continue
                m = re.match(r"^dword ptr \[(e[a-ds][xip])(?: \+ 0x([0-9a-f]+))?\], 0x([0-9a-f]+)$", ops)
                if m and m.group(1) in regs and self.is_vtable(int(m.group(3), 16)):
                    stores.append((int(m.group(2) or "0", 16), int(m.group(3), 16)))
            elif mn == "call":
                m = re.match(r"^0x([0-9a-f]+)$", ops)
                if m:
                    last_call = int(m.group(1), 16)
                regs.discard("eax")
                regs.discard("edx")
            elif mn == "jmp":
                m = re.match(r"^0x([0-9a-f]+)$", ops)
                if m and not (func <= int(m.group(1), 16) < func + 0x400):
                    tail = int(m.group(1), 16)
                    break
            elif mn == "ret" and (stores or tail):
                break
        primary = [v for off, v in stores if off == 0]
        if primary:
            secondary = {}
            for off, v in stores:
                if off:
                    secondary[off] = v  # the last store per offset is the most derived one
            return primary[-1], sorted(secondary.items())
        if depth < 4 and (tail or last_call):
            return self.ctor_vtables(tail or last_call, "ecx", depth + 1)
        return None, []

    def scan_vtables(self):
        for c in self.classes.values():
            primary, secondary = self.ctor_vtables(c["internal_ctor"])
            if primary is None:
                raise ScanError("no vtable store found for %s" % c["cpp_name"])
            c["vtable"] = primary
            c["secondary_vtables"] = secondary
        groups = defaultdict(list)
        for n, c in self.classes.items():
            groups[c["vtable"]].append(n)
        self.vtable_groups = {v: sorted(ns) for v, ns in groups.items()}
        # Identical vtables are folded by the linker: sharers must be related.
        for v, names in self.vtable_groups.items():
            if len(names) < 2:
                continue
            sups = {self.classes[n]["super"] for n in names}
            related = len(sups) == 1 or any(self.classes[n]["super"] in names for n in names)
            if not related:
                raise ScanError("unrelated classes share a vtable: %r" % names)
        # A derived class's vtable repeats most of its parent's slots.
        low = 0
        for c in self.classes.values():
            if not c["super"]:
                continue
            parent = self.classes[c["super"]]
            same = total = 0
            for i in range(60):
                a = self.pe.u32(c["vtable"] + 4 * i)
                b = self.pe.u32(parent["vtable"] + 4 * i)
                total += 1
                same += a == b
            if same * 2 < total:
                low += 1
        self.require(low == 0, "every class shares more than half of its first 60 vtable slots with its super")

    def class_tables(self, mac_names=None, mac_gpsc=None, mac_ipsc=None):
        classes = self.classes
        cols = ["package", "name", "cpp_name", "size", "super", "within", "class_flags", "vtable_rva", "private_static_class_rva"]
        rows = []
        for n in sorted(classes, key=lambda x: (classes[x]["package"].lower(), x)):
            c = classes[n]
            rows.append(
                [
                    c["package"],
                    c["name"],
                    n,
                    c["size"],
                    c["super"],
                    None if c["within"] == "UObject" else c["within"],
                    c["class_flags"],
                    self.pe.rva(c["vtable"]),
                    self.pe.rva(c["psc"]),
                ]
            )
        by_pkg = Counter(c["package"] for c in classes.values())
        shared = sorted((names for names in self.vtable_groups.values() if len(names) > 1), key=lambda x: x[0])
        out = {
            "schema": SCHEMA_PREFIX + "/class-sizes/v1",
            "build": BUILD,
            "game_build": GAME_BUILD,
            "method": "arguments of every UClass::UClass(EC_StaticConstructor, sizeof(Class), ClassFlags, CastFlags, Name, Package, "
            "ConfigName, ObjectFlags, InternalConstructor, StaticConstructor, InitializeIntrinsicPropertyValues) call "
            "(IMPLEMENT_CLASS); super and within from Initialize<Class>PrivateStaticClass; vtable from the stores of "
            "<Class>::InternalConstructor (tools/ghidra-scripts/win32/win32_scan.py)",
            "notes": [
                "size is sizeof(Class) in bytes on this 32-bit build. name is the class name the registration passes (cpp_name "
                "without its U/A prefix; classes flagged CLASS_Deprecated 0x02000000 also drop 'DEPRECATED_'); the script class "
                "path is package + '.' + name.",
                "super and within are cpp names. super is null only for UObject. within is null when it is UObject.",
                "All numbers are decimal. Rows are sorted by package (case-insensitive), then cpp_name.",
                "vtable_rva and private_static_class_rva are relative virtual addresses (add the module base at run time). "
                "private_static_class_rva is the address of the static UClass* <Class>::PrivateStaticClass, which is null until "
                "the class registers.",
                "The linker folded identical vtables: the groups in shared_vtables use one address, so a vtable pointer alone "
                "does not always identify the class.",
            ],
            "confidence": "CONFIRMED for package, name, size, flags (immediate operands of the registration call; class and cast "
            "flags equal the Mac build's for every class compared); super and within are derived by register tracking and equal "
            "the Mac build's for every class compared (CONFIRMED for those, STRONG for the Windows-only classes); vtable STRONG "
            "(constructor stores, checked for consistency)",
            "counts": {
                "registration_call_sites": self.registration_sites,
                "classes": len(classes),
                "by_package": dict(sorted(by_pkg.items())),
                "classes_sharing_a_vtable": sum(len(g) for g in shared),
                "shared_vtable_groups": len(shared),
            },
            "uclass_static_constructor_rva": self.pe.rva(self.uclass_ctor),
            "initialize_private_static_class_rva": self.pe.rva(self.init_common),
            "columns": cols,
            "classes": rows,
            "shared_vtables": shared,
        }
        if mac_names is not None:
            win = set(classes)
            only_win = sorted(win - mac_names)
            out["mac_comparison"] = {
                "mac_native_classes": len(mac_names),
                "common": len(win & mac_names),
                "mac_only": sorted(mac_names - win),
                "windows_only": len(only_win),
                "windows_only_by_package": dict(sorted(Counter(classes[n]["package"] for n in only_win).items())),
                "windows_only_outside_unrealed": sorted(n for n in only_win if classes[n]["package"] != "UnrealEd"),
            }
            self.check(True, "class names compared with the Mac list: %d common" % len(win & mac_names))
        if mac_gpsc is not None:
            flags_diff = [n for n in mac_gpsc if n in classes and mac_gpsc[n]["flags"] != classes[n]["class_flags"]]
            cast_diff = [n for n in mac_gpsc if n in classes and mac_gpsc[n]["cast"] != classes[n]["cast_flags"]]
            bigger = [n for n in mac_gpsc if n in classes and classes[n]["size"] >= mac_gpsc[n]["size"]]
            compared = sum(1 for n in mac_gpsc if n in classes)
            self.require(not flags_diff and not cast_diff, "class and cast flags equal the Mac build's for all compared classes")
            self.require(not bigger, "every Win32 sizeof is smaller than the Mac x86_64 sizeof")
            out["mac_comparison"]["registration_arguments"] = {
                "classes_compared": compared,
                "class_flags_different": len(flags_diff),
                "cast_flags_different": len(cast_diff),
                "win32_size_not_smaller_than_mac": len(bigger),
            }
        if mac_ipsc is not None:
            compared = [n for n in mac_ipsc if n in classes]
            diff = [
                n
                for n in compared
                if mac_ipsc[n]["super"] != (classes[n]["super"] or n) or mac_ipsc[n]["within"] != classes[n]["within"]
            ]
            self.require(not diff, "super and within equal the Mac build's for all compared classes")
            out["mac_comparison"]["super_and_within"] = {"classes_compared": len(compared), "different": len(diff)}
        return out

    # ------------------------------------------------------- globals / code

    def find_string_function(self, literal, prefix=False):
        """First code reference to a UTF-16 literal (or to literals starting with it).
        All references must sit in one function."""
        lits = self.wide_literal(literal, prefix)
        sites = sorted(r for lit in lits for r in self.refs(lit))
        ok = bool(sites) and len({self.enclosing_function(r) for r in sites}) == 1
        self.require(ok, "the literal %r is referenced from exactly one function" % literal)
        return sites[0]

    def fn_lines(self, start, limit=0x4000):
        return self.dis.function(start, limit)

    def abs_refs(self, lines):
        """Absolute data addresses used by instructions: [(index, addr)]."""
        out = []
        for i, (_a, _mn, ops) in enumerate(lines):
            for m in re.finditer(r"\[0x([0-9a-f]+)\]", ops):
                out.append((i, int(m.group(1), 16)))
        return out

    def scan_globals(self):
        pe = self.pe
        g = {}  # name -> dict
        f = {}

        def add_global(name, va, typ, size, how, checks, confidence, extra=None):
            sec = pe.section_of(va)
            o = pe.off(va)
            e = {
                "name": name,
                "rva": pe.rva(va),
                "rva_hex": "0x%08X" % pe.rva(va),
                "va_at_preferred_base": "0x%08X" % va,
                "type": typ,
                "size": size,
                "section": sec["name"],
                "initialised_in_file": o is not None,
                "how_found": how,
                "cross_checks": checks,
                "confidence": confidence,
            }
            if extra:
                e.update(extra)
            g[name] = e

        def add_function(name, va, how, confidence, extra=None):
            e = {
                "name": name,
                "rva": pe.rva(va),
                "rva_hex": "0x%08X" % pe.rva(va),
                "va_at_preferred_base": "0x%08X" % va,
                "how_found": how,
                "confidence": confidence,
            }
            if extra:
                e.update(extra)
            f[name] = e

        # --- FEngineLoop::PreInit: GIsBenchmarking = ParseParam(appCmdLine(), TEXT("BENCHMARK"))
        site = self.find_string_function("BENCHMARK")
        lines = self.dis.lines(site - 1, 0x40)
        self.require(lines[0][1] == "push", "BENCHMARK literal is pushed")
        stores = [ops for _a, mn, ops in lines if mn == "mov" and re.match(r"^dword ptr \[0x[0-9a-f]+\], eax$", ops)]
        self.require(len(stores) >= 1, "the ParseParam result is stored to a global")
        g_bench = int(re.match(r"^dword ptr \[0x([0-9a-f]+)\]", stores[0]).group(1), 16)
        preinit_site = site

        # --- FEngineLoop::Init: Parse(appCmdLine(), TEXT("FPS="), f); GEngine->MatineeCaptureFPS = f; GFixedDeltaTime = 1 / f
        site = self.find_string_function("FPS=")
        lines = self.dis.lines(site - 1, 0x70)
        g_engine = g_fixed = None
        for _a, mn, ops in lines:
            m = re.match(r"^eax, dword ptr \[0x([0-9a-f]+)\]$", ops)
            if mn == "mov" and m and g_engine is None:
                g_engine = int(m.group(1), 16)
            m = re.match(r"^qword ptr \[0x([0-9a-f]+)\], xmm0$", ops)
            if mn == "movsd" and m and g_fixed is None:
                g_fixed = int(m.group(1), 16)
        self.require(g_engine and g_fixed, "FPS= block loads GEngine and stores GFixedDeltaTime")
        init_site = site
        fixed_file = struct.unpack_from("<d", self.d, pe.off(g_fixed))[0]
        self.require(fixed_file == 1.0 / 30.0, "GFixedDeltaTime is 1/30 in the file")

        # --- appUpdateTimeAndHandleMaxTickRate: the function that reads GIsBenchmarking and copies
        #     GFixedDeltaTime into GDeltaTime.
        cands = {self.enclosing_function(r) for r in self.refs(g_fixed)} & {self.enclosing_function(r) for r in self.refs(g_bench)}
        upd = None
        for c in sorted(x for x in cands if x):
            ls = self.fn_lines(c, 0x800)
            for i in range(len(ls) - 1):
                if (
                    ls[i][1] == "movsd"
                    and ls[i][2] == "xmm0, qword ptr [%#x]" % g_fixed
                    and ls[i + 1][1] == "movsd"
                    and re.match(r"^qword ptr \[0x[0-9a-f]+\], xmm0$", ls[i + 1][2])
                ):
                    upd = c
                    upd_lines = ls
                    g_delta = int(re.match(r"^qword ptr \[0x([0-9a-f]+)\]", ls[i + 1][2]).group(1), 16)
        self.require(upd is not None, "appUpdateTimeAndHandleMaxTickRate found (GDeltaTime = GFixedDeltaTime)")
        # bUseFixedTimeStep = GIsBenchmarking || GUseFixedTimeStep; GLastTime = GCurrentTime
        g_usefixed = g_current = g_last = None
        for i, (_a, mn, ops) in enumerate(upd_lines):
            if mn == "cmp" and ops.startswith("dword ptr [%#x]" % g_bench):
                nxt = [x for x in upd_lines[i + 1 : i + 4] if x[1] == "cmp"]
                if nxt:
                    g_usefixed = int(re.match(r"^dword ptr \[0x([0-9a-f]+)\]", nxt[0][2]).group(1), 16)
                for j in range(i + 1, min(i + 12, len(upd_lines) - 1)):
                    m1 = re.match(r"^xmm1, qword ptr \[0x([0-9a-f]+)\]$", upd_lines[j][2])
                    m2 = re.match(r"^qword ptr \[0x([0-9a-f]+)\], xmm1$", upd_lines[j + 1][2])
                    if m1 and m2:
                        g_current = int(m1.group(1), 16)
                        g_last = int(m2.group(1), 16)
                        break
                break
        self.require(g_usefixed and g_current and g_last, "GUseFixedTimeStep, GCurrentTime and GLastTime read from the same function")

        # --- FEngineLoop::Tick: the caller of that function which increments a 64-bit counter.
        tick = None
        for c in sorted(self.call_index().get(upd, [])):
            start = self.enclosing_function(c)
            ls = self.fn_lines(start, 0x1000)
            idx_call = [i for i, x in enumerate(ls) if x[0] == c]
            if not idx_call:
                continue
            for i in range(idx_call[0], len(ls) - 3):
                m = re.match(r"^eax, dword ptr \[0x([0-9a-f]+)\]$", ls[i][2])
                if not (ls[i][1] == "mov" and m and ls[i + 1][1] == "add" and ls[i + 1][2].startswith("eax,")):
                    continue
                lo = int(m.group(1), 16)
                if ls[i + 2][1] == "adc" and ls[i + 2][2].startswith("dword ptr [%#x]" % (lo + 4)) and ls[i + 3][2] == "dword ptr [%#x], eax" % lo:
                    tick = (start, c, ls, idx_call[0], i, lo)
                    break
        self.require(tick is not None, "FEngineLoop::Tick found (calls the time update, then increments GFrameCounter)")
        tick_start, upd_call, tl, i_upd, i_inc, g_frame = tick
        # GEngine virtual calls taking (float)GDeltaTime between the time update and the increment
        slots = []
        for i in range(i_upd, i_inc):
            if tl[i][1] == "mov" and tl[i][2] == "ecx, dword ptr [%#x]" % g_engine:
                window = tl[i : i + 8]
                if any(x[1] == "movsd" and x[2] == "xmm0, qword ptr [%#x]" % g_delta for x in window):
                    for x in window:
                        m = re.match(r"^e[a-d]x, dword ptr \[e[a-d]x \+ 0x([0-9a-f]+)\]$", x[2])
                        if x[1] == "mov" and m:
                            slots.append((x[0], int(m.group(1), 16)))
                            break
        self.require(len(slots) == 3, "three GEngine virtual calls with GDeltaTime before the counter increment")
        tick_slot = slots[-1][1]
        # frame-limit test at the top: GIsBenchmarking && MaxFrameCounter && GFrameCounter > MaxFrameCounter
        head = tl[:i_upd]
        self.require(
            any(x[2].startswith("dword ptr [%#x]" % g_bench) for x in head) and any("[%#x]" % g_frame in x[2] for x in head),
            "FEngineLoop::Tick tests GIsBenchmarking and GFrameCounter before the time update",
        )
        # message pump after the increment
        pump = None
        for i in range(i_inc, len(tl)):
            if tl[i][1] == "call" and re.match(r"^0x[0-9a-f]+$", tl[i][2]):
                t = int(tl[i][2], 16)
                body = self.fn_lines(t, 0x200)
                if any(self.imports.get(a) == "USER32.dll!PeekMessageW" for _i, a in self.abs_refs(body)):
                    pump = (tl[i][0], t)
                    break
        self.require(pump is not None, "the Windows message pump is called after the GFrameCounter increment")

        # --- UGameEngine::Tick = slot of UGameEngine's vtable; UWorld::Tick inside it
        ge_vt = self.classes["UGameEngine"]["vtable"]
        ge_tick = pe.u32(ge_vt + tick_slot)
        gl = self.fn_lines(ge_tick, 0x2000)
        self.require(gl[-1][1] == "ret" and gl[-1][2] == "0x4", "UGameEngine::Tick is a thiscall with one stack argument")
        # first GWorld use: GWorld->GetGameInfo() right after the GForceLowGore test
        g_world = get_game_info = None
        for i, (_a, mn, ops) in enumerate(gl[:40]):
            m = re.match(r"^ecx, dword ptr \[0x([0-9a-f]+)\]$", ops)
            if mn == "mov" and m and gl[i + 1][1] == "call":
                g_world = int(m.group(1), 16)
                get_game_info = int(gl[i + 1][2], 16)
                break
        self.require(g_world is not None, "UGameEngine::Tick starts with a call on GWorld")
        # GWorld->Tick(LEVELTICK_All, DeltaSeconds): mov ecx,[GWorld]; ...; push 2; call
        world_tick = None
        i_world_tick = None
        for i, (_a, mn, ops) in enumerate(gl):
            if mn == "push" and ops == "0x2" and gl[i + 1][1] == "call":
                before = gl[i - 4 : i]
                if any(x[2] == "ecx, dword ptr [%#x]" % g_world for x in before):
                    self.require(world_tick is None, "one GWorld->Tick(LEVELTICK_All, DeltaSeconds) call")
                    world_tick = int(gl[i + 1][2], 16)
                    i_world_tick = i + 1
        self.require(world_tick is not None, "UWorld::Tick call found in UGameEngine::Tick")
        direct = [x for x in gl if x[1] == "call" and x[2] == "%#x" % world_tick]
        self.require(len(direct) == 1, "UGameEngine::Tick calls UWorld::Tick exactly once")
        # the only other caller is the same vtable slot of the editor's engine class
        ed_tick = pe.u32(self.classes["UEditorEngine"]["vtable"] + tick_slot)
        tick_callers = {self.enclosing_function(c) for c in self.call_index().get(world_tick, [])}
        self.require(tick_callers == {ge_tick, ed_tick}, "UWorld::Tick is called by UGameEngine::Tick and UEditorEngine::Tick only")
        # before it: Client->Tick (virtual, DeltaSeconds), UObject::StaticTick, GSeamlessTravelHandler
        client_off = client_slot = None
        for i in range(i_world_tick):
            m = re.match(r"^ecx, dword ptr \[esi \+ 0x([0-9a-f]+)\]$", gl[i][2])
            if gl[i][1] == "mov" and m and client_off is None:
                for x in gl[i + 1 : i + 9]:
                    m2 = re.match(r"^eax, dword ptr \[edx \+ 0x([0-9a-f]+)\]$", x[2])
                    if x[1] == "mov" and m2:
                        client_off = int(m.group(1), 16)
                        client_slot = int(m2.group(1), 16)
                        break
        self.require(client_off is not None, "Engine.Client is ticked through its vtable before UWorld::Tick")
        prev_calls = [x for x in gl[:i_world_tick] if x[1] == "call" and re.match(r"^0x[0-9a-f]+$", x[2])]
        seamless_tick = int(prev_calls[-1][2], 16)
        static_tick = int(prev_calls[-2][2], 16)
        seamless = None
        for x in gl[i_world_tick - 14 : i_world_tick]:
            m = re.match(r"^ecx, 0x([0-9a-f]+)$", x[2])
            if x[1] == "mov" and m:
                seamless = int(m.group(1), 16)
        self.require(seamless is not None, "GSeamlessTravelHandler.Tick() sits between StaticTick and UWorld::Tick")
        # after it: loop over Engine.GamePlayers
        gp = None
        for x in gl[i_world_tick + 1 : i_world_tick + 6]:
            m = re.match(r"^dword ptr \[esi \+ 0x([0-9a-f]+)\], e[a-z]+$", x[2])
            if x[1] == "cmp" and m:
                gp = int(m.group(1), 16) - 4
        self.require(gp is not None and gp == client_off + 4, "Engine.GamePlayers follows Engine.Client")
        # UWorld::Tick: thiscall, two stack arguments, begins with GetWorldInfo(0) and the players loop
        wl = self.fn_lines(world_tick, 0x3000)
        self.require(wl[-1][1] == "ret" and wl[-1][2] == "0x8", "UWorld::Tick is a thiscall with two stack arguments")
        first_call = next(x for x in wl if x[1] == "call")
        get_world_info = int(first_call[2], 16)
        self.require(
            any(x[2] == "ecx, dword ptr [%#x]" % g_engine for x in wl[:30]), "UWorld::Tick loads GEngine after GetWorldInfo"
        )
        # WorldInfo time fields, in source order: RealTimeSeconds += dt; AudioTimeSeconds += dt;
        # dt *= TimeDilation; DeltaSeconds = dt; TimeSeconds += dt  (all before any actor ticks)
        float_stores = []
        for i, (a, mn, ops) in enumerate(wl):
            m = re.match(r"^dword ptr \[ebx \+ 0x([0-9a-f]+)\], xmm0$", ops)
            if mn == "movss" and m:
                float_stores.append((i, a, int(m.group(1), 16)))
        self.require(len(float_stores) >= 4, "UWorld::Tick stores four WorldInfo time fields")
        (i_rts, a_rts, off_rts), (_i2, a_audio, off_audio), (_i3, a_dt, off_dt), (i_ts, a_ts, off_ts) = float_stores[:4]
        self.require(
            off_audio == off_rts + 4 and off_dt == off_rts + 8 and off_ts == off_rts - 4,
            "the four fields are TimeSeconds, RealTimeSeconds, AudioTimeSeconds, DeltaSeconds in a row",
        )
        dil = a_dil = None
        for x in wl[i_rts : i_ts]:
            m = re.match(r"^xmm0, dword ptr \[ebx \+ 0x([0-9a-f]+)\]$", x[2])
            if x[1] == "movss" and m and int(m.group(1), 16) < off_ts:
                dil = int(m.group(1), 16)
                a_dil = x[0]
        self.require(dil == off_ts - 8, "TimeDilation is read 8 bytes before TimeSeconds")
        is_paused = int(next(x for x in wl[i_rts:] if x[1] == "call")[2], 16)

        # --- FName::Names: the hard-coded-name error path; the three words of the array
        site = self.find_string_function("Hardcoded name '%s' at index", prefix=True)
        nfn = self.enclosing_function(site)
        nl = self.fn_lines(nfn, 0x800)
        cnt = Counter(a for _i, a in self.abs_refs(nl) if pe.within(a, ".data"))
        top = sorted(a for a, _n in cnt.most_common(3))
        self.require(top[1] == top[0] + 4 and top[2] == top[0] + 8, "FName::Names is three consecutive words (data, count, max)")
        g_names = top[0]
        # second function: FShaderParameter::Bind
        # second and third function: FShaderParameter::Bind and FShaderResourceParameter::Bind
        bind_sites = [r for lit in self.wide_literal("Failure to bind non-optional shader parameter", True) for r in self.refs(lit)]
        bind_funcs = sorted({self.enclosing_function(r) for r in bind_sites})
        uses = [any(a in (g_names, g_names + 4) for _i, a in self.abs_refs(self.fn_lines(fn, 0x400))) for fn in bind_funcs]
        self.require(len(bind_funcs) == 2 and all(uses), "both shader parameter Bind functions use the same array")
        names_funcs = {self.enclosing_function(r) for k in (0, 4, 8) for r in self.refs(g_names + k)}

        # --- UObject::GObjObjects: UObject::StaticInit presizes it after reading MaxObjectsNotConsideredByGC
        site = self.find_string_function("MaxObjectsNotConsideredByGC")
        sfn = self.enclosing_function(site)
        sl = self.fn_lines(sfn, 0x400)
        g_objs = g_first_gc = None
        for i, (_a, mn, ops) in enumerate(sl):
            m = re.match(r"^dword ptr \[0x([0-9a-f]+)\], eax$", ops)
            if mn == "mov" and m and g_first_gc is None and i > 5:
                g_first_gc = int(m.group(1), 16)
            m = re.match(r"^ecx, 0x([0-9a-f]+)$", ops)
            if mn == "mov" and m and g_first_gc is not None and g_objs is None and sl[i + 1][1] == "call":
                g_objs = int(m.group(1), 16)
        self.require(g_objs is not None, "UObject::StaticInit reserves GObjObjects")
        psc_package = self.classes["UPackage"]["psc"]
        self.require(any(a == psc_package for _i, a in self.abs_refs(sl)), "UObject::StaticInit uses UPackage::PrivateStaticClass")
        site2 = self.find_string_function("Object subsystem successfully closed", prefix=True)
        efn = self.enclosing_function(site2)
        el = self.fn_lines(efn, 0x1000)
        n_exit = sum(1 for _i, a in self.abs_refs(el) if a in (g_objs, g_objs + 4))
        self.require(n_exit >= 2, "UObject::StaticExit walks the same array")

        # --- exported names
        exports = {}
        rva, _size = pe.dirs[0]
        o = pe.off(pe.image_base + rva)
        (_c, _t, _maj, _min, _name, _base, _nfun, nnames, afun, anames, aords) = struct.unpack_from("<IIHHIIIIIII", self.d, o)
        for i in range(nnames):
            nm = pe.cstr(pe.image_base + pe.u32(pe.image_base + anames + 4 * i))
            ordi = struct.unpack_from("<H", self.d, pe.off(pe.image_base + aords + 2 * i))[0]
            exports[nm] = pe.image_base + pe.u32(pe.image_base + afun + 4 * ordi)
        g_debugger = exports.get("GDebugger")
        self.require(g_debugger is not None, "GDebugger is exported")
        self.require(
            any(x[2] == "ecx, dword ptr [%#x]" % g_debugger for x in gl[:60]),
            "UGameEngine::Tick uses the exported GDebugger before Client->Tick (as on the Mac)",
        )
        get_outermost = exports["GetOutermost"]
        ol = self.fn_lines(get_outermost, 0x80)
        outer = None
        for x in ol:
            m = re.match(r"^eax, dword ptr \[edi \+ 0x([0-9a-f]+)\]$", x[2])
            if x[1] == "mov" and m:
                outer = int(m.group(1), 16)
                break
        self.require(outer is not None, "exported GetOutermost walks UObject.Outer")

        # --- reference statistics and writers
        def writers(va, size=4):
            out = set()
            total = 0
            for k in range(0, size, 4):
                for r in self.refs(va + k):
                    total += 1
                    if self.is_store(r):
                        out.add(self.enclosing_function(r))
            return total, out

        n_use, w_use = writers(g_usefixed)
        self.require(not w_use, "nothing stores to GUseFixedTimeStep")
        use_funcs = {self.enclosing_function(r) for r in self.refs(g_usefixed)}
        bench_funcs = {self.enclosing_function(r) for r in self.refs(g_bench)}
        self.require(len(use_funcs) == 3 and use_funcs <= bench_funcs, "GUseFixedTimeStep has three readers, all of which read GIsBenchmarking")
        _n, w_frame = writers(g_frame, 8)
        self.require(w_frame == {tick_start}, "only FEngineLoop::Tick writes GFrameCounter")
        _n, w_delta = writers(g_delta)
        self.require(w_delta == {upd}, "only appUpdateTimeAndHandleMaxTickRate writes GDeltaTime")
        # GFrameCounter readers identified by their own strings
        frame_funcs = {self.enclosing_function(r) for k in (0, 4) for r in self.refs(g_frame + k)}
        contact = self.enclosing_function(self.find_string_function("OnContactNotify(): Actors", prefix=True))
        skel = self.enclosing_function(self.find_string_function("Size of list: %d %d"))
        self.require(contact in frame_funcs and skel in frame_funcs, "two more identified functions read GFrameCounter")
        # DrawUnitTimes reads GUseFixedTimeStep
        unit = self.enclosing_function(self.find_string_function("Game thread time"))
        self.require(unit in use_funcs, "DrawUnitTimes reads GUseFixedTimeStep")
        # GWorld: a second function (UGameEngine::LoadMap) writes it
        loadmap = self.enclosing_function(self.find_string_function("LoadMap: %s"))
        ll = self.fn_lines(loadmap, 0x4000)
        self.require(any(a == g_world for _i, a in self.abs_refs(ll)), "UGameEngine::LoadMap uses the same GWorld")
        # GMalloc: the allocator used by three inlined UClass allocations
        #   mov ecx,[G]; test ecx,ecx; jne; call create; mov ecx,[G]; mov eax,[ecx]; mov edx,[eax+8]; push 8; push size
        g_malloc = None
        lo, hi = self.text_range()
        m = re.search(
            rb"\x8b\x0d(....)\x85\xc9\x75.\xe8....\x8b\x0d(....)\x8b\x01\x8b\x50\x08\x6a\x08\x68", self.d[lo:hi], re.S
        )
        if m and m.group(1) == m.group(2):
            g_malloc = struct.unpack("<I", m.group(1))[0]

        # ------------------------------------------------------------- output
        ptr = "pointer"
        add_global(
            "GEngine", g_engine, ptr, 4,
            "FEngineLoop::Init: after Parse(appCmdLine(), L\"FPS=\", f) the block loads this pointer, stores (int)f into the object "
            "(MatineeCaptureFPS) and stores 1/f to GFixedDeltaTime",
            [
                "FEngineLoop::Tick calls three virtual functions on it with (float)GDeltaTime; the last one (vtable +0x%X) is "
                "UGameEngine::Tick in UGameEngine's vtable" % tick_slot,
                "appUpdateTimeAndHandleMaxTickRate calls a virtual function on it with the frame time (GetMaxTickRate)",
                "UWorld::Tick reads its GamePlayers array",
            ],
            "CONFIRMED", {"code_references": len(self.refs(g_engine))},
        )
        add_global(
            "GWorld", g_world, ptr, 4,
            "UGameEngine::Tick: loaded into ecx for GWorld->Tick(LEVELTICK_All = 2, DeltaSeconds), the single direct call that "
            "follows UObject::StaticTick and GSeamlessTravelHandler.Tick()",
            [
                "the same function starts with a call on it right after the GForceLowGore test (GetGameInfo on the Mac)",
                "UGameEngine::LoadMap (literal \"LoadMap: %s\") uses it",
            ],
            "CONFIRMED", {"code_references": len(self.refs(g_world))},
        )
        add_global(
            "GFrameCounter", g_frame, "u64", 8,
            "FEngineLoop::Tick: 64-bit increment (mov eax,[lo]; add eax,1; adc [hi],0; mov [lo],eax) after GEngine->Tick",
            [
                "the same function compares it with this->MaxFrameCounter when GIsBenchmarking is set, before the time update",
                "read by FNxContactReport::onContactNotify and PrintOutSkelMeshLODs (identified by their literals), as on the Mac",
                "written by FEngineLoop::Tick only",
            ],
            "CONFIRMED", {"functions_referencing": len(frame_funcs)},
        )
        add_global(
            "GDeltaTime", g_delta, "f64", 8,
            "appUpdateTimeAndHandleMaxTickRate: receives GFixedDeltaTime in the fixed-step branch (movsd xmm0,[GFixedDeltaTime]; "
            "movsd [GDeltaTime],xmm0)",
            [
                "FEngineLoop::Tick converts it to float for each GEngine virtual call, including GEngine->Tick",
                "written by appUpdateTimeAndHandleMaxTickRate only",
            ],
            "CONFIRMED", {"file_value_f64": struct.unpack_from("<d", self.d, pe.off(g_delta))[0]},
        )
        add_global(
            "GFixedDeltaTime", g_fixed, "f64", 8,
            "FEngineLoop::Init: movsd [GFixedDeltaTime], xmm0 with xmm0 = 1 / FPS= value",
            ["appUpdateTimeAndHandleMaxTickRate copies it to GDeltaTime when fixed stepping", "its value in the file is 1/30"],
            "CONFIRMED", {"file_value_f64": fixed_file},
        )
        add_global(
            "GIsBenchmarking", g_bench, "u32", 4,
            "FEngineLoop::PreInit: receives the result of ParseParam(appCmdLine(), L\"BENCHMARK\")",
            [
                "appUpdateTimeAndHandleMaxTickRate tests it first (GIsBenchmarking || GUseFixedTimeStep)",
                "FEngineLoop::Tick tests it for the frame limit",
            ],
            "CONFIRMED", {"functions_referencing": len(bench_funcs)},
        )
        add_global(
            "GUseFixedTimeStep", g_usefixed, "u32", 4,
            "appUpdateTimeAndHandleMaxTickRate: the second flag of bUseFixedTimeStep, compared right after GIsBenchmarking",
            [
                "exactly three functions read it and each also reads GIsBenchmarking (Mac: DrawUnitTimes, "
                "appUpdateTimeAndHandleMaxTickRate, UGameViewportClient::SetDropDetail)",
                "one of them is DrawUnitTimes (literal \"Game thread time\")",
                "no instruction stores to it (%d references, all loads/compares)" % n_use,
            ],
            "CONFIRMED",
        )
        add_global(
            "FName::Names", g_names, "tarray", 12,
            "function holding the literal that starts \"Hardcoded name\": the three most used .data words are consecutive "
            "(data, count, max)",
            [
                "FShaderParameter::Bind and FShaderResourceParameter::Bind (literals that start \"Failure to bind non-optional "
                "shader parameter\") index the same array, as on the Mac",
                "small accessors compare an index with the word at +4 and load data[index*4] from the word at +0",
            ],
            "CONFIRMED", {"functions_referencing": len(names_funcs)},
        )
        add_global(
            "UObject::GObjObjects", g_objs, "tarray", 12,
            "UObject::StaticInit (configuration key literal \"MaxObjectsNotConsideredByGC\"): passed as this (mov ecx, imm) to the array presize call "
            "right after GObjFirstGCIndex is stored",
            [
                "UObject::StaticExit (literal that starts \"Object subsystem successfully closed\") walks the same array",
                "the same StaticInit uses UPackage::PrivateStaticClass at the address found independently by the class scan",
            ],
            "CONFIRMED",
            {
                "code_references": sum(len(self.refs(g_objs + k)) for k in (0, 4, 8)),
                "functions_referencing": len({self.enclosing_function(r) for k in (0, 4, 8) for r in self.refs(g_objs + k)}),
            },
        )
        add_global(
            "UObject::GObjFirstGCIndex", g_first_gc, "i32", 4,
            "UObject::StaticInit: receives the MaxObjectsNotConsideredByGC value read from the configuration", [], "STRONG",
        )
        add_global(
            "GCurrentTime", g_current, "f64", 8,
            "appUpdateTimeAndHandleMaxTickRate: copied to GLastTime at the top, advanced by GDeltaTime when fixed stepping", [], "STRONG",
        )
        add_global("GLastTime", g_last, "f64", 8, "appUpdateTimeAndHandleMaxTickRate: GLastTime = GCurrentTime", [], "STRONG")
        add_global(
            "GDebugger", g_debugger, ptr, 4, "export table (name GDebugger)",
            ["UGameEngine::Tick calls through it before Client->Tick, at the position where the Mac build uses GDebugger"], "CONFIRMED",
        )
        add_global(
            "GSeamlessTravelHandler", seamless, "struct", None,
            "UGameEngine::Tick: tested and passed as this between UObject::StaticTick and UWorld::Tick (same order as the Mac build)",
            [], "STRONG",
        )
        if g_malloc:
            add_global(
                "GMalloc", g_malloc, ptr, 4,
                "three class registration functions allocate the UClass through its vtable (+8) with (size, alignment 8) "
                "instead of calling the allocation function",
                [], "STRONG",
            )

        add_function(
            "UWorld::Tick", world_tick,
            "the only GWorld->f(2, DeltaSeconds) call of UGameEngine::Tick; starts with GetWorldInfo(0) and the GEngine->GamePlayers loop "
            "like the Mac function",
            "CONFIRMED",
            {
                "calling_convention": "thiscall: ecx = UWorld*, [esp+4] = ELevelTick TickType, [esp+8] = float DeltaSeconds; ret 8",
                "role": "frame boundary: once per frame, after input dispatch (Client->Tick), before any actor ticks",
                "direct_call_sites": len(self.call_index().get(world_tick, [])),
                "calling_functions": ["UGameEngine::Tick", "UEditorEngine::Tick"],
                "end_rva": pe.rva(wl[-1][0]),
            },
        )
        add_function(
            "UGameEngine::Tick", ge_tick,
            "slot +0x%X of UGameEngine's vtable, the slot FEngineLoop::Tick calls on GEngine with (float)GDeltaTime last before "
            "GFrameCounter is incremented" % tick_slot,
            "CONFIRMED",
            {"calling_convention": "thiscall: ecx = UGameEngine*, [esp+4] = float DeltaSeconds; ret 4", "vtable_offset": tick_slot,
             "end_rva": pe.rva(gl[-1][0])},
        )
        add_function(
            "FEngineLoop::Tick", tick_start,
            "the caller of appUpdateTimeAndHandleMaxTickRate that increments GFrameCounter", "CONFIRMED",
            {"gframecounter_increment_rva": pe.rva(tl[i_inc][0]), "time_update_call_rva": pe.rva(upd_call),
             "gengine_tick_call_slot_load_rva": pe.rva(slots[-1][0]), "message_pump_call_rva": pe.rva(pump[0])},
        )
        add_function(
            "appUpdateTimeAndHandleMaxTickRate", upd,
            "reads GIsBenchmarking and GUseFixedTimeStep, copies GFixedDeltaTime to GDeltaTime", "CONFIRMED",
            {"direct_callers": len(self.call_index().get(upd, []))},
        )
        add_function("appWinPumpMessages", pump[1], "called by FEngineLoop::Tick after the counter increment; calls PeekMessageW", "STRONG")
        add_function("UObject::StaticTick", static_tick, "direct call with DeltaSeconds between Client->Tick and GSeamlessTravelHandler (Mac order)", "STRONG")
        add_function("FSeamlessTravelHandler::Tick", seamless_tick, "called on GSeamlessTravelHandler right before UWorld::Tick (Mac order)", "STRONG")
        add_function("UWorld::GetWorldInfo", get_world_info, "first call of UWorld::Tick, argument 0 (Mac order)", "STRONG")
        add_function("UWorld::GetGameInfo", get_game_info, "first call of UGameEngine::Tick, on GWorld (Mac order)", "STRONG")
        add_function("UWorld::IsPaused", is_paused, "called by UWorld::Tick after RealTimeSeconds is advanced; its result gates AudioTimeSeconds and TimeSeconds", "STRONG")
        add_function("UWindowsClient::Tick", pe.u32(self.classes["UWindowsClient"]["vtable"] + client_slot),
                     "slot +0x%X of UWindowsClient's vtable, the slot UGameEngine::Tick calls on Engine.Client" % client_slot, "STRONG",
                     {"vtable_offset": client_slot})
        add_function("UEditorEngine::Tick", ed_tick, "slot +0x%X of UEditorEngine's vtable; the only other caller of UWorld::Tick "
                     "(editor only, not on the game's path)" % tick_slot, "STRONG", {"vtable_offset": tick_slot})
        add_function("UObject::StaticInit", sfn, "holds the literal \"MaxObjectsNotConsideredByGC\"", "CONFIRMED")
        add_function("UObject::StaticExit", efn, "holds the literal \"Object subsystem successfully closed\"", "CONFIRMED")
        add_function("UObject::GetOutermost", get_outermost, "export table (name GetOutermost)", "CONFIRMED")
        add_function("UClass::UClass(EC_StaticConstructor)", self.uclass_ctor, "callee of every native class registration", "CONFIRMED",
                     {"direct_callers": self.registration_sites})
        add_function("InitializePrivateStaticClass", self.init_common, "shared tail of every Initialize<Class>PrivateStaticClass", "CONFIRMED")
        add_function("FEngineLoop::PreInit", self.enclosing_function(preinit_site), "function holding the push of the literal \"BENCHMARK\"",
                     "STRONG", {"benchmark_parse_site_rva": pe.rva(preinit_site - 1)})
        add_function("FEngineLoop::Init", self.enclosing_function(init_site), "function holding the push of the literal \"FPS=\"",
                     "STRONG", {"fps_parse_site_rva": pe.rva(init_site - 1)})
        add_function("UGameEngine::LoadMap", loadmap, "function holding the literal \"LoadMap: %s\"", "STRONG")
        add_function("DrawUnitTimes", unit, "function holding the literal \"Game thread time\"", "STRONG")

        frame = {
            "order": [
                "FEngineLoop::Tick: frame-limit test (GIsBenchmarking, GFrameCounter)",
                "appUpdateTimeAndHandleMaxTickRate() writes GDeltaTime",
                "GEngine virtual +0x%X and +0x%X with (float)GDeltaTime" % (slots[0][1], slots[1][1]),
                "GEngine->Tick((float)GDeltaTime) = UGameEngine::Tick (vtable +0x%X)" % tick_slot,
                "  UGameEngine::Tick: Engine.Client->Tick(DeltaSeconds) (vtable +0x%X, UWindowsClient::Tick)" % client_slot,
                "  UGameEngine::Tick: UObject::StaticTick(DeltaSeconds), GSeamlessTravelHandler.Tick()",
                "  UGameEngine::Tick: GWorld->Tick(LEVELTICK_All, DeltaSeconds), exactly one direct call",
                "  UGameEngine::Tick: loop over Engine.GamePlayers, later viewport and other work",
                "GFrameCounter += 1",
                "appWinPumpMessages() (PeekMessageW loop)",
            ],
            "matches_mac_order": True,
            "gengine_virtual_calls_with_delta_time": [s[1] for s in slots],
            "uworld_tick_time_updates": {
                "order": ["RealTimeSeconds += DeltaSeconds", "AudioTimeSeconds += DeltaSeconds (not paused)",
                          "DeltaSeconds *= TimeDilation, clamped", "WorldInfo.DeltaSeconds = DeltaSeconds",
                          "TimeSeconds += DeltaSeconds (not paused)"],
                "real_time_seconds_store_rva": pe.rva(a_rts),
                "time_seconds_store_rva": pe.rva(a_ts),
                "note": "all four stores come before the first tick-group work of UWorld::Tick",
            },
        }
        self.frame = frame
        self.evidence_inputs = {
            "outer": outer, "get_outermost": get_outermost, "client_off": client_off, "gp": gp, "ge_tick": ge_tick,
            "world_tick": world_tick, "off_ts": off_ts, "off_rts": off_rts, "off_audio": off_audio, "off_dt": off_dt, "dil": dil,
            "a_ts": a_ts, "a_rts": a_rts, "a_audio": a_audio, "a_dt": a_dt, "a_dil": a_dil,
            "tick_start": tick_start, "g_names": g_names, "g_engine": g_engine,
        }
        return g, f

    def is_store(self, operand_va):
        """True when the instruction whose absolute operand sits at operand_va writes memory."""
        d = self.d
        o = self.pe.off(operand_va)
        b1, b2, b3 = d[o - 1], d[o - 2], d[o - 3]
        if b1 == 0xA3:
            return True
        if (b1 & 0xC7) != 0x05:
            return False
        if b2 in (0x89, 0xC7, 0x01, 0x09, 0x11, 0x21, 0x29, 0x31):
            return True
        if b2 in (0x83, 0x81):
            return ((b1 >> 3) & 7) != 7
        if b2 == 0xFF:
            return ((b1 >> 3) & 7) in (0, 1)
        if b2 == 0x11 and b3 == 0x0F:
            return True
        if b2 in (0xD9, 0xDD, 0xDB, 0xDF):
            return ((b1 >> 3) & 7) in (2, 3)
        return False

    # ------------------------------------------------------ native evidence

    def find_in_function(self, start, needle, limit=0x4000):
        """rva of the first occurrence of the bytes inside the function, or None."""
        lines = self.fn_lines(start, limit)
        lo = self.pe.off(start)
        hi = self.pe.off(lines[-1][0]) + 8
        k = self.d.find(needle, lo, hi)
        return None if k < 0 else self.pe.rva(self.pe.va_of_off(k))

    def scan_evidence(self, functions):
        pe = self.pe
        e = self.evidence_inputs
        items = []

        def item(what, function, rva, bytes_hex, fields, confidence):
            self.require(rva is not None, "evidence bytes present: " + what)
            items.append({"what": what, "function": function, "rva": rva, "bytes": bytes_hex, "shows": fields, "confidence": confidence})

        def enc_disp32(prefix, off):
            return prefix + struct.pack("<I", off)

        # UObject.Outer
        b = bytes([0x8B, 0x47, e["outer"]])
        item("UObject.Outer at +0x%X (the loop of the exported GetOutermost)" % e["outer"], "UObject::GetOutermost",
             self.find_in_function(e["get_outermost"], b, 0x80), b.hex(), {"Core.Object.Outer": e["outer"]}, "CONFIRMED")
        # Engine.Client / GamePlayers
        b = enc_disp32(b"\x8b\x8e", e["client_off"])
        item("Engine.Client at +0x%X (loaded for Client->Tick before UWorld::Tick)" % e["client_off"], "UGameEngine::Tick",
             self.find_in_function(e["ge_tick"], b), b.hex(), {"Engine.Engine.Client": e["client_off"]}, "CONFIRMED")
        b = enc_disp32(b"\x39\xae", e["gp"] + 4)
        item("Engine.GamePlayers count at +0x%X (TArray count at +4)" % (e["gp"] + 4), "UGameEngine::Tick",
             self.find_in_function(e["ge_tick"], b), b.hex(), {"Engine.Engine.GamePlayers": e["gp"], "TArray.count": 4}, "CONFIRMED")
        b = enc_disp32(b"\x8b\x86", e["gp"])
        item("Engine.GamePlayers data pointer at +0x%X (TArray data at +0)" % e["gp"], "UGameEngine::Tick",
             self.find_in_function(e["ge_tick"], b), b.hex(), {"Engine.Engine.GamePlayers": e["gp"], "TArray.data": 0}, "CONFIRMED")
        # Player.Actor: FEngineLoop::Tick first-frame block: mov ecx,[eax+ebp*4]; ...; mov esi,[ecx+0x40]
        tl = self.fn_lines(e["tick_start"], 0x1000)
        actor = None
        for i, (a, mn, ops) in enumerate(tl):
            if mn == "mov" and re.match(r"^ecx, dword ptr \[eax \+ 4\*ebp\]$", ops):
                for x in tl[i + 1 : i + 4]:
                    m = re.match(r"^esi, dword ptr \[ecx \+ 0x([0-9a-f]+)\]$", x[2])
                    if x[1] == "mov" and m:
                        actor = (int(m.group(1), 16), x[0])
        self.require(actor is not None, "FEngineLoop::Tick reads GamePlayers(i)->Actor")
        b = bytes([0x8B, 0x71, actor[0]])
        item("Player.Actor at +0x%X (first-frame loop over GEngine->GamePlayers)" % actor[0], "FEngineLoop::Tick",
             pe.rva(actor[1]), b.hex(), {"Engine.Player.Actor": actor[0]}, "STRONG")
        self.require(self.d[pe.off(actor[1]) : pe.off(actor[1]) + 3] == b, "Player.Actor instruction bytes")
        # WorldInfo time fields
        store, load = b"\xf3\x0f\x11\x83", b"\xf3\x0f\x10\x83"  # movss [ebx+disp32], xmm0 / movss xmm0, [ebx+disp32]
        for name, off, opc, at in (
            ("RealTimeSeconds", e["off_rts"], store, e["a_rts"]),
            ("AudioTimeSeconds", e["off_audio"], store, e["a_audio"]),
            ("TimeDilation", e["dil"], load, e["a_dil"]),
            ("DeltaSeconds", e["off_dt"], store, e["a_dt"]),
            ("TimeSeconds", e["off_ts"], store, e["a_ts"]),
        ):
            b = enc_disp32(opc, off)
            self.require(self.d[pe.off(at) : pe.off(at) + len(b)] == b, "UWorld::Tick time update touches WorldInfo." + name)
            item("WorldInfo.%s at +0x%X (time update at the top of UWorld::Tick, in source order)" % (name, off), "UWorld::Tick",
                 pe.rva(at), b.hex(), {"Engine.WorldInfo." + name: off}, "CONFIRMED" if name != "AudioTimeSeconds" else "STRONG")
        # FName / FNameEntry
        lo, hi = self.text_range()
        blob = self.d[lo:hi]
        # index test + data[index*4] of FName::Names
        m = re.search(
            rb"\x8b\x01\x85\xc0\x78.\x3b\x05" + re.escape(struct.pack("<I", e["g_names"] + 4)) + rb"\x7d.\x8b\x0d"
            + re.escape(struct.pack("<I", e["g_names"])) + rb"\x83\x3c\x81\x00",
            blob, re.S,
        )
        self.require(m is not None, "an FName validity test indexes FName::Names by the word at FName+0")
        items.append({
            "what": "FName.Index is the first word; FName::Names is {data +0, count +4}; entries are 4-byte pointers "
                    "(index < count, then data[index*4] != 0)",
            "function": "FName validity test (small accessor)", "rva": pe.rva(self.text_lo + m.start()),
            "bytes": "8b01 / 3b05 <Names+4> / 8b0d <Names> / 833c8100",
            "shows": {"FName.index": 0, "TArray.data": 0, "TArray.count": 4}, "confidence": "CONFIRMED",
        })
        # wide flag and character offset: test byte [esi+8],1 ; je ; lea eax,[esi+0x10] ... add esi,0x10
        m = re.search(rb"\xf6\x46\x08\x01\x74.\x8d\x46\x10\xeb.\x83\xc6\x10", blob, re.S)
        self.require(m is not None, "FNameEntry wide-flag test followed by the character offset")
        site = self.text_lo + m.start()
        near = self.dis.lines(site, 0x40)
        cmp_import = None
        for _a, mn, ops in near:
            mm = re.match(r"^dword ptr \[0x([0-9a-f]+)\]$", ops)
            if mn == "call" and mm:
                cmp_import = self.imports.get(int(mm.group(1), 16))
                break
        self.require(cmp_import == "MSVCR100.dll!_wcsicmp", "the wide name is compared with _wcsicmp (2-byte wchar_t)")
        items.append({
            "what": "FNameEntry: bit 0 of the word at +8 selects wide characters; characters start at +0x10 for both forms; "
                    "the wide form is compared with _wcsicmp, the ANSI form is widened first",
            "function": "FName lookup (hash chain compare)", "rva": pe.rva(site), "bytes": "f6460801 / 8d4610 / 83c610",
            "shows": {"FNameEntry.index": 8, "FNameEntry.chars": 16, "FNameEntry.wide_flag_mask": 1, "FNameEntry.wide_char_size": 2},
            "confidence": "CONFIRMED",
        })
        # FNameEntry.Flags: Names[NAME_Log]->Flags & RF_Suppress (high dword at +4)
        m = re.search(rb"\xa1" + re.escape(struct.pack("<I", e["g_names"])) + rb"\x8b\x80(....)\x8b\x48\x04\x81\xe1\x00\x10\x00\x00", blob, re.S)
        self.require(m is not None, "a log function tests the suppress flag of a hard-coded name entry")
        name_log = struct.unpack("<I", m.group(1))[0] // 4
        items.append({
            "what": "FNameEntry.Flags is a 64-bit word at +0: a log function tests bit 0x1000 of its high half (+4) on "
                    "Names[%d] (the hard-coded name index of Log)" % name_log,
            "function": "FOutputDevice log helper", "rva": pe.rva(self.text_lo + m.start()), "bytes": "8b4804 / 81e100100000",
            "shows": {"FNameEntry.flags": 0, "hardcoded_name_index_Log": name_log}, "confidence": "STRONG",
        })
        self.name_log = name_log
        return items


# ---------------------------------------------------------------- mac inputs


def read_mac_names(path):
    with open(path, "r", encoding="utf-8") as fh:
        return {line.strip() for line in fh if line.strip()}


def read_mac_gpsc(path):
    """sizeof / class flags / cast flags from a local disassembly of the Mac
    Get<Class>PrivateStaticClass functions (DEFAULTS.md, Reproduce step 2)."""
    with open(path, "r", encoding="utf-8", errors="replace") as fh:
        txt = fh.read()
    out = {}
    for f in re.split(r"\n(?=[0-9a-f]+ <[^>]+>:\n)", txt):
        m = re.match(r"[0-9a-f]+ <__ZN(\d+)([^>]+GetPrivateStaticClass[^>]+)>:", f)
        if not m:
            continue
        cls = m.group(2)[: int(m.group(1))]
        k = f.find("__ZN6UClassC1E")
        if k < 0:
            continue
        pre = f[:k]

        def last(reg):
            vals = [(mo.start(), int(mo.group(1), 16)) for mo in re.finditer(r"movl\t\$0x([0-9a-f]+), %" + reg + r"\b", pre)]
            vals += [(mo.start(), 0) for mo in re.finditer(r"xorl\t%" + reg + ", %" + reg, pre)]
            return sorted(vals)[-1][1] if vals else None

        size, flags, cast = last("edx"), last("ecx"), last("r8d")
        if size is not None and flags is not None and cast is not None:
            out[cls] = {"size": size, "flags": flags, "cast": cast}
    return out


def read_mac_ipsc(path):
    """(super, self, within) per class from a local disassembly of the Mac
    Initialize<Class>PrivateStaticClass functions: the three arguments of the shared
    InitializePrivateStaticClass(UClass*, UClass*, UClass*) call."""
    common = "__Z28InitializePrivateStaticClassP6UClassS0_S0_"

    def split(sym):
        m = re.match(r"__ZN(\d+)", sym)
        if not m:
            return None, ""
        start = 4 + len(m.group(1))
        n = int(m.group(1))
        return sym[start : start + n], sym[start + n :]

    with open(path, "r", encoding="utf-8", errors="replace") as fh:
        txt = fh.read()
    out = {}
    for f in re.split(r"\n(?=[0-9a-f]+ <[^>]+>:\n)", txt):
        m = re.match(r"[0-9a-f]+ <(__ZN\d+\w+InitializePrivateStaticClass\w+Ev)>:", f)
        if not m:
            continue
        me = split(m.group(1))[0]
        regs = {}
        addr = {}
        args = None
        for line in f.splitlines()[1:]:
            mi = re.match(r"\s*[0-9a-f]+:\s+(\S+)\s*(.*)$", line)
            if not mi:
                continue
            mn, ops = mi.group(1), mi.group(2)
            sym = re.search(r"<(__ZN\d+\w+?18PrivateStaticClassE)>", ops)
            cls = split(sym.group(1))[0] if sym else None
            if mn == "movq":
                a = re.match(r"[-0-9a-fx]+\(%rip\), %(\w+)", ops)
                if a and cls:
                    regs[a.group(1)] = cls
                    continue
                a = re.match(r"\(%(\w+)\), %(\w+)", ops)
                if a:
                    regs[a.group(2)] = addr.get(a.group(1))
                    continue
                a = re.match(r"%(\w+), %(\w+)$", ops.split("#")[0].strip())
                if a:
                    regs[a.group(2)] = regs.get(a.group(1))
            elif mn == "leaq":
                a = re.match(r"[-0-9a-fx]+\(%rip\), %(\w+)", ops)
                if a and cls:
                    addr[a.group(1)] = cls
            elif mn in ("callq", "jmp"):
                t = re.search(r"<(\w+)>", ops)
                if not t:
                    continue
                if t.group(1) == common:
                    args = (regs.get("rdi"), regs.get("rsi"), regs.get("rdx"))
                    if mn == "jmp":
                        break
                    continue
                name, rest = split(t.group(1))
                if name and (rest.startswith("11StaticClassEv") or "GetPrivateStaticClass" in rest):
                    regs["rax"] = name
        if args and all(args) and args[1] == me:
            out[me] = {"super": args[0], "within": args[2]}
    return out


# --------------------------------------------------------------------- main

KEY_CLASSES = [
    "UObject", "UClass", "UPackage", "UWorld", "ULevel", "AActor", "AWorldInfo", "UEngine", "UGameEngine", "UPlayer", "ULocalPlayer",
    "UGameViewportClient", "UWindowsClient", "AController", "APlayerController", "AGamePlayerController", "AUDKPlayerController",
    "APawn", "AGamePawn", "AUDKPawn", "AInventory", "AWeapon", "AUDKWeapon", "UInteraction", "UInput", "UPlayerInput", "ACamera",
    "AVolume", "APhysicsVolume", "ADefaultPhysicsVolume", "AInfo", "AZoneInfo", "AGameInfo", "AFrameworkGame", "AHUD",
    "UASAMUSystemSettingsManager",
]

# script-only gameplay classes and the native ancestor whose vtable they carry
# (BINARY_ANALYSIS.md 4.4, from the script super chains)
SCRIPT_CLASSES = {
    "asamu.ASAMUPawn": "AUDKPawn",
    "asamu.ASAMUPlayerController": "AUDKPlayerController",
    "asamu.GrappleGun": "AUDKWeapon",
    "asamu.ASAMUCamera": "ACamera",
    "asamu.ASAMUPlayerInput": "UPlayerInput",
    "asamu.ASAMUGameInfo": "AFrameworkGame",
}


def dumps(obj):
    return json.dumps(obj, indent=1, sort_keys=False, ensure_ascii=True) + "\n"


def compact_class_json(obj):
    """class_sizes.json with one class per line."""
    rows = obj["classes"]
    shared = obj["shared_vtables"]
    head = dict(obj)
    head["classes"] = "@@ROWS@@"
    head["shared_vtables"] = "@@SHARED@@"
    text = json.dumps(head, indent=1, ensure_ascii=True)
    text = text.replace('"@@ROWS@@"', "[\n" + ",\n".join("  " + json.dumps(r, separators=(",", ":")) for r in rows) + "\n ]")
    text = text.replace('"@@SHARED@@"', "[\n" + ",\n".join("  " + json.dumps(r, separators=(",", ":")) for r in shared) + "\n ]")
    return text + "\n"


def build(args):
    objdump = args.objdump or os.environ.get("OBJDUMP") or "objdump"
    check_objdump(objdump)
    sc = Scanner(args.exe, objdump)
    image = sc.scan_image()
    sc.scan_classes()
    mac_names = read_mac_names(args.mac_classes) if args.mac_classes else None
    mac_gpsc = read_mac_gpsc(args.mac_gpsc) if args.mac_gpsc else None
    mac_ipsc = read_mac_ipsc(args.mac_ipsc) if args.mac_ipsc else None
    if (mac_gpsc is not None or mac_ipsc is not None) and mac_names is None:
        raise ScanError("--mac-gpsc and --mac-ipsc need --mac-classes")
    class_json = sc.class_tables(mac_names, mac_gpsc, mac_ipsc)
    image["msvc_rtti"] = sc.scan_rtti()
    globals_, functions = sc.scan_globals()
    evidence = sc.scan_evidence(functions)
    pe = sc.pe
    vt = []
    for n in KEY_CLASSES:
        c = sc.classes[n]
        vt.append({
            "class": n,
            "package": c["package"],
            "size": c["size"],
            "super": c["super"],
            "vtable_rva": pe.rva(c["vtable"]),
            "vtable_rva_hex": "0x%08X" % pe.rva(c["vtable"]),
            "secondary_vtables": [{"object_offset": off, "vtable_rva": pe.rva(v)} for off, v in c["secondary_vtables"]],
            "shared_with": [x for x in sc.vtable_groups[c["vtable"]] if x != n],
            "private_static_class_rva": pe.rva(c["psc"]),
            "internal_constructor_rva": pe.rva(c["internal_ctor"]),
        })
    script = []
    for path, native in SCRIPT_CLASSES.items():
        c = sc.classes[native]
        script.append({"script_class": path, "native_ancestor": native, "vtable_rva": pe.rva(c["vtable"]),
                       "unique_vtable": len(sc.vtable_groups[c["vtable"]]) == 1})
    common_head = {"build": BUILD, "game_build": GAME_BUILD, "image_base": "0x%08X" % pe.image_base,
                   "address_rule": "runtime address = module base + rva"}
    out = {
        "image.json": dumps(image),
        "class_sizes.json": compact_class_json(class_json),
        "globals.json": dumps(dict({"schema": SCHEMA_PREFIX + "/globals/v1"}, **common_head, **{
            "notes": [
                "pointer = 4 bytes; tarray = {data pointer +0, i32 count +4, i32 max +8}; u64/f64 = 8 bytes little endian.",
                "MSVC RTTI exists only for a few non-UObject classes, so objects are identified by vtable or by their Class "
                "pointer (class_sizes.json).",
            ],
            "globals": [globals_[k] for k in globals_],
        })),
        "functions.json": dumps(dict({"schema": SCHEMA_PREFIX + "/functions/v1"}, **common_head, **{
            "functions": [functions[k] for k in functions],
            "frame": sc.frame,
        })),
        "vtables.json": dumps(dict({"schema": SCHEMA_PREFIX + "/vtables/v1"}, **common_head, **{
            "notes": [
                "MSVC layout: the object's first word points at the vtable's first slot (no offset-to-top / typeinfo words to skip).",
                "A script-only class is constructed by its nearest native ancestor and carries that ancestor's vtable.",
                "RTTI complete-object locators do not exist for UObject classes on this build (compiled without RTTI); the "
                "vtables come from the constructors.",
            ],
            "classes": vt,
            "script_classes": script,
        })),
        "native_evidence.json": dumps(dict({"schema": SCHEMA_PREFIX + "/native-evidence/v1"}, **common_head, **{
            "notes": [
                "Each entry is one or more single-instruction encodings (2-8 bytes) found in the named function; they restate "
                "the offsets in 'shows'. rva is where the first one is.",
                "sizeof(UObject) = %d and Outer at +0x%X leave Name at +0x%X (8 bytes), Class at +0x%X and ObjectArchetype at "
                "+0x%X when the Mac member order is kept (STRONG)."
                % (sc.classes["UObject"]["size"], sc.evidence_inputs["outer"], sc.evidence_inputs["outer"] + 4,
                   sc.evidence_inputs["outer"] + 12, sc.evidence_inputs["outer"] + 16),
            ],
            "struct_sizes": {"UObject": sc.classes["UObject"]["size"], "UClass": sc.classes["UClass"]["size"], "TArray": 12, "FName": 8,
                             "pointer": 4},
            "evidence": evidence,
        })),
    }
    return sc, out


def main(argv):
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("exe", help="path to ASAMU-Win32-Shipping.exe")
    ap.add_argument("--out", required=True, help="output directory (docs/reverse-engineering/data/win32)")
    ap.add_argument("--check", action="store_true", help="compare with the files in --out instead of writing")
    ap.add_argument("--mac-classes", help="text file with the Mac build's native class names, one per line")
    ap.add_argument("--mac-gpsc", help="local disassembly of the Mac Get<Class>PrivateStaticClass functions (gpsc.asm)")
    ap.add_argument("--mac-ipsc", help="local disassembly of the Mac Initialize<Class>PrivateStaticClass functions (ipsc.asm)")
    ap.add_argument("--objdump", help="LLVM objdump to use (default: $OBJDUMP or objdump)")
    ap.add_argument("--verbose", action="store_true")
    args = ap.parse_args(argv)
    try:
        sc, out = build(args)
    except ScanError as e:
        print("win32_scan: %s" % e, file=sys.stderr)
        return 1
    failed = [t for ok, t in sc.checks if not ok]
    if args.verbose:
        for ok, t in sc.checks:
            print("%s %s" % ("ok  " if ok else "FAIL", t))
    print("%d checks, %d failed" % (len(sc.checks), len(failed)))
    if failed:
        return 1
    if args.check:
        diff = 0
        for name, text in out.items():
            path = os.path.join(args.out, name)
            try:
                with open(path, "r", encoding="utf-8") as fh:
                    same = fh.read() == text
            except OSError:
                same = False
            if not same:
                diff += 1
                print("differs: %s" % name)
        print("%d files compared, %d differ" % (len(out), diff))
        return 1 if diff else 0
    os.makedirs(args.out, exist_ok=True)
    for name, text in out.items():
        with open(os.path.join(args.out, name), "w", encoding="utf-8", newline="\n") as fh:
            fh.write(text)
        print("wrote %s (%d bytes)" % (name, len(text)))
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
