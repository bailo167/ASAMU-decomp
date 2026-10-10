"""Core of the ASAMU trace recorder: memory layout, sampling, raw format, conversion.

Pure Python 3.8+, no third-party modules and no ``lldb`` import, so it can be
tested without a debugger. ``asamu_lldb.py`` is the thin LLDB front end (Mac
build, one breakpoint per frame); ``asamu_win.py`` is the Windows front end
(32-bit build, read-only polling with ``layout_win_x86.json``). Pointer size,
array, string and name-entry shapes all come from the layout file.

What it does
------------
* ``Layout`` loads ``layout_<platform>.json`` (next to this file). Every offset
  the recorder reads is looked up there by name (``off``, ``bit``, ``st``,
  ``sym``); the Rust test ``tools/asamu-trace/tests/recorder_layout.rs`` checks
  each name used in this file against that layout, the layout against
  ``docs/reverse-engineering/data/defaults/native_layout.json`` and, when the
  install is present, against the executable.
* ``Sampler`` reads one frame of player state through a ``read(addr, size)``
  callable and returns a *raw record* (format ``asamu-trace-raw`` v1).
* ``convert_raw`` turns a raw recording into canonical ``asamu-trace`` v1
  traces (``crates/asamu-player/src/trace.rs``), exactly as
  ``asamu-trace convert`` (Rust, the reference implementation) does.

Command line (no debugger needed)::

    python3 -I asamu_recorder_core.py convert RAW.raw.jsonl [--out-dir DIR]
    python3 -I asamu_recorder_core.py selftest [--layout layout_win_x86.json]

Raw records never contain addresses or pointers; see docs/TRACE_CAPTURE.md.
"""

import json
import math
import os
import re
import struct
import sys

RECORDER = "asamu_lldb 0.1.0"
RAW_FORMAT = "asamu-trace-raw"
RAW_VERSION = 1
TRACE_FORMAT = "asamu-trace"
TRACE_SCHEMA_VERSION = 1
SAMPLE_POINT = "entry of UWorld::Tick (after input dispatch, before any actor tick)"
DEFAULT_LAYOUT = "layout_mac_x86_64.json"

# UE3 EPhysics values used here (NATIVE_PHYSICS.md 1.3, CONFIRMED).
PHYS_WALKING = 1
PHYS_FLYING = 4

MAX_KEYS = 64
MAX_BINDINGS = 4096
MAX_NAME_CHARS = 1024
MAX_STRING_CHARS = 4096
MAX_OUTER_DEPTH = 16
# Most command parts visited when one key's binding is expanded (aliases
# included); mirrors MAX_PARTS_PER_KEY in tools/asamu-trace/src/bindings.rs.
MAX_PARTS_PER_KEY = 256
TAU_OVER_UNITS = math.tau / 65536.0


class LayoutError(Exception):
    """The layout file is missing, malformed or lacks a name."""


class ReadError(Exception):
    """A memory read failed or returned fewer bytes than asked for."""


# --------------------------------------------------------------------- layout


class Layout:
    """Offsets and symbols of one game build (``layout_*.json``)."""

    def __init__(self, data):
        if data.get("schema") != "asamu-decomp/recorder-layout/v1":
            raise LayoutError("unsupported layout schema %r" % data.get("schema"))
        self.data = data
        self.id = data["id"]
        self.game_build = data.get("game_build")
        self.pointer_size = int(data["pointer_size"])
        self._fields = {}
        for f in data["fields"]:
            key = (f["class"], f["name"])
            if key in self._fields:
                raise LayoutError("duplicate field %s.%s" % key)
            self._fields[key] = f
        self._structs = data["structs"]
        self._symbols = data["symbols"]

    @classmethod
    def load(cls, path=None):
        if path is None:
            path = os.path.join(os.path.dirname(os.path.abspath(__file__)), DEFAULT_LAYOUT)
        with open(path, "r", encoding="utf-8") as fh:
            return cls(json.load(fh))

    def field(self, cls_name, name):
        try:
            return self._fields[(cls_name, name)]
        except KeyError:
            raise LayoutError("layout has no field %s.%s" % (cls_name, name))

    def off(self, cls_name, name):
        return int(self.field(cls_name, name)["offset"], 16)

    def bit(self, cls_name, name):
        f = self.field(cls_name, name)
        if f.get("kind") != "Bool" or f.get("bit") is None:
            raise LayoutError("%s.%s is not a bool field" % (cls_name, name))
        return int(f["offset"], 16), int(f["bit"])

    def st(self, struct_name, member):
        try:
            return int(self._structs[struct_name]["members"][member])
        except KeyError:
            raise LayoutError("layout has no struct member %s.%s" % (struct_name, member))

    def st_size(self, struct_name):
        return int(self._structs[struct_name]["size"])

    def st_attr(self, struct_name, attr):
        return self._structs[struct_name][attr]

    def sym(self, name):
        try:
            return self._symbols[name]
        except KeyError:
            raise LayoutError("layout has no symbol %s" % name)

    def symbol_names(self):
        return list(self._symbols.keys())

    def sentinels(self):
        return list(self.data.get("sentinels", []))


class Offsets:
    """Every offset the sampler uses, looked up once by literal name.

    Keep each lookup a call of ``off``, ``bit``, ``st``, ``st_size``,
    ``st_attr`` or ``sym`` with literal string arguments: the Rust test finds
    them by pattern and checks each against the layout.
    """

    def __init__(self, L):
        self.ptr = L.pointer_size
        # Core.Object
        self.outer = L.off("Core.Object", "Outer")
        self.name = L.off("Core.Object", "Name")
        self.cls = L.off("Core.Object", "Class")
        # Engine / player chain
        self.game_players = L.off("Engine.Engine", "GamePlayers")
        self.player_actor = L.off("Engine.Player", "Actor")
        self.controller_pawn = L.off("Engine.Controller", "Pawn")
        # Actor
        self.location = L.off("Engine.Actor", "Location")
        self.rotation = L.off("Engine.Actor", "Rotation")
        self.physics = L.off("Engine.Actor", "Physics")
        self.base = L.off("Engine.Actor", "Base")
        self.world_info = L.off("Engine.Actor", "WorldInfo")
        self.velocity = L.off("Engine.Actor", "Velocity")
        self.acceleration = L.off("Engine.Actor", "Acceleration")
        # Pawn
        self.walkable_floor_z = L.off("Engine.Pawn", "WalkableFloorZ")
        self.ground_speed = L.off("Engine.Pawn", "GroundSpeed")
        self.air_speed = L.off("Engine.Pawn", "AirSpeed")
        self.jump_z = L.off("Engine.Pawn", "JumpZ")
        self.air_control = L.off("Engine.Pawn", "AirControl")
        self.eye_height = L.off("Engine.Pawn", "EyeHeight")
        self.weapon = L.off("Engine.Pawn", "Weapon")
        # PlayerController
        self.player_camera = L.off("Engine.PlayerController", "PlayerCamera")
        self.pressed_jump = L.bit("Engine.PlayerController", "bPressedJump")
        self.fov_angle = L.off("Engine.PlayerController", "FOVAngle")
        self.player_input = L.off("Engine.PlayerController", "PlayerInput")
        # Input / PlayerInput
        self.bindings = L.off("Engine.Input", "Bindings")
        self.pressed_keys = L.off("Engine.Input", "PressedKeys")
        self.a_base_y = L.off("Engine.PlayerInput", "aBaseY")
        self.a_mouse_x = L.off("Engine.PlayerInput", "aMouseX")
        self.a_mouse_y = L.off("Engine.PlayerInput", "aMouseY")
        self.a_forward = L.off("Engine.PlayerInput", "aForward")
        self.a_turn = L.off("Engine.PlayerInput", "aTurn")
        self.a_strafe = L.off("Engine.PlayerInput", "aStrafe")
        self.a_look_up = L.off("Engine.PlayerInput", "aLookUp")
        self.move_forward_speed = L.off("Engine.PlayerInput", "MoveForwardSpeed")
        # Camera
        self.camera_fov = L.off("Engine.Camera", "CameraCache.POV.FOV")
        # WorldInfo
        self.time_dilation = L.off("Engine.WorldInfo", "TimeDilation")
        self.time_seconds = L.off("Engine.WorldInfo", "TimeSeconds")
        self.real_time_seconds = L.off("Engine.WorldInfo", "RealTimeSeconds")
        self.delta_seconds = L.off("Engine.WorldInfo", "DeltaSeconds")
        self.pauser = L.off("Engine.WorldInfo", "Pauser")
        # GrappleGun (script class)
        self.gun_grappling = L.bit("asamu.GrappleGun", "bIsGrappling")
        self.gun_released = L.bit("asamu.GrappleGun", "bReleasedGrapple")
        self.gun_can_grapple = L.bit("asamu.GrappleGun", "bCanGrapple")
        self.gun_location = L.off("asamu.GrappleGun", "vGrappleLocation")
        self.gun_distance = L.off("asamu.GrappleGun", "vDistance")
        self.gun_hit_loc_actor = L.off("asamu.GrappleGun", "HitLocActor")
        self.gun_times = L.off("asamu.GrappleGun", "iTimesGrappled")
        self.gun_max = L.off("asamu.GrappleGun", "iMaxGrapples")
        self.gun_max_distance = L.off("asamu.GrappleGun", "fMaxDistance")
        self.gun_release_distance = L.off("asamu.GrappleGun", "fGrappleReleaseDistance")
        self.gun_max_speed = L.off("asamu.GrappleGun", "fGrappleMaxSpeed")
        self.gun_instant_release = L.off("asamu.GrappleGun", "instantReleaseDelay")
        # ASAMUPawn (script class)
        self.pawn_has_jumped = L.bit("asamu.ASAMUPawn", "bHasJumped")
        self.pawn_power_jumped = L.bit("asamu.ASAMUPawn", "bPowerJumped")
        self.pawn_released_jump = L.bit("asamu.ASAMUPawn", "bHasReleasedJump")
        self.pawn_sprinting = L.bit("asamu.ASAMUPawn", "bSprinting")
        self.pawn_is_falling = L.bit("asamu.ASAMUPawn", "bIsFalling")
        self.pawn_rocket_boots = L.off("asamu.ASAMUPawn", "rocketBoots")
        self.pawn_terminal_velocity = L.off("asamu.ASAMUPawn", "fTerminalVelocity")
        self.pawn_jump_lower = L.off("asamu.ASAMUPawn", "jumpVelocityLowerMultiplier")
        self.pawn_zoom_fov = L.off("asamu.ASAMUPawn", "zoomFOV")
        self.pawn_story_speed = L.off("asamu.ASAMUPawn", "storyModeSpeedMultiplier")
        # ASAMURocketBoots (script class)
        self.boots_finished = L.bit("asamu.ASAMURocketBoots", "bFinished")
        self.boots_enabled = L.bit("asamu.ASAMURocketBoots", "bEnabled")
        # Native structs
        self.arr_data = L.st("TArray", "data")
        self.arr_count = L.st("TArray", "count")
        self.fname_index = L.st("FName", "index")
        self.fname_number = L.st("FName", "number")
        self.fname_size = L.st_size("FName")
        self.entry_index = L.st("FNameEntry", "index")
        self.entry_chars = L.st("FNameEntry", "chars")
        self.entry_wide_mask = int(L.st_attr("FNameEntry", "wide_flag_mask"))
        self.entry_wide_size = int(L.st_attr("FNameEntry", "wide_char_size"))
        self.fstring_data = L.st("FString", "data")
        self.fstring_count = L.st("FString", "count")
        self.fstring_char_size = int(L.st_attr("FString", "char_size"))
        self.keybind_name = L.st("KeyBind", "Name")
        self.keybind_command = L.st("KeyBind", "Command")
        self.keybind_size = L.st_size("KeyBind")
        # Symbols (load addresses are resolved by the front end)
        self.sym_engine = L.sym("GEngine")["mangled"]
        self.sym_world = L.sym("GWorld")["mangled"]
        self.sym_frame_counter = L.sym("GFrameCounter")["mangled"]
        self.sym_delta_time = L.sym("GDeltaTime")["mangled"]
        self.sym_fixed_delta_time = L.sym("GFixedDeltaTime")["mangled"]
        self.sym_benchmarking = L.sym("GIsBenchmarking")["mangled"]
        self.sym_fixed_step = L.sym("GUseFixedTimeStep")["mangled"]
        self.sym_names = L.sym("FName::Names")["mangled"]
        self.sym_world_tick = L.sym("UWorld::Tick")["mangled"]


# --------------------------------------------------------------------- memory


class Mem:
    """Typed little-endian reads through ``read(addr, size) -> bytes``."""

    def __init__(self, read, pointer_size=8):
        self._read = read
        self.ptr_size = pointer_size
        self.reads = 0

    def raw(self, addr, size):
        if addr <= 0 or size < 0:
            raise ReadError("bad read %#x+%d" % (addr, size))
        self.reads += 1
        data = self._read(addr, size)
        if data is None or len(data) != size:
            raise ReadError("short read %#x+%d" % (addr, size))
        return bytes(data)

    def ptr(self, addr):
        b = self.raw(addr, self.ptr_size)
        return struct.unpack("<Q" if self.ptr_size == 8 else "<I", b)[0]

    def u32(self, addr):
        return struct.unpack("<I", self.raw(addr, 4))[0]

    def u64(self, addr):
        return struct.unpack("<Q", self.raw(addr, 8))[0]

    def f64(self, addr):
        return struct.unpack("<d", self.raw(addr, 8))[0]


class Block:
    """One contiguous read of an object span; fields are unpacked from it."""

    def __init__(self, mem, base, start, end):
        self.base = base
        self.start = start
        self.buf = mem.raw(base + start, end - start)
        self.ptr_fmt = "<Q" if mem.ptr_size == 8 else "<I"
        self.ptr_size = mem.ptr_size

    def _at(self, off, size):
        i = off - self.start
        if i < 0 or i + size > len(self.buf):
            raise ReadError("offset %#x outside block" % off)
        return self.buf[i : i + size]

    def f32(self, off):
        return struct.unpack("<f", self._at(off, 4))[0]

    def i32(self, off):
        return struct.unpack("<i", self._at(off, 4))[0]

    def u32(self, off):
        return struct.unpack("<I", self._at(off, 4))[0]

    def u8(self, off):
        return self._at(off, 1)[0]

    def ptr(self, off):
        return struct.unpack(self.ptr_fmt, self._at(off, self.ptr_size))[0]

    def vec3(self, off):
        return list(struct.unpack("<3f", self._at(off, 12)))

    def rot(self, off):
        return list(struct.unpack("<3i", self._at(off, 12)))

    def flag(self, off_bit):
        off, bit = off_bit
        return bool((self.u32(off) >> bit) & 1)

    def name(self, off):
        return struct.unpack("<ii", self._at(off, 8))


def span(*offsets_and_sizes):
    """(start, end) covering every (offset, size) pair."""
    start = min(o for o, _ in offsets_and_sizes)
    end = max(o + s for o, s in offsets_and_sizes)
    return start, end


# ---------------------------------------------------------------------- names


def text_char(c):
    """One character of a game string; invalid code points (beyond U+10FFFF,
    or UTF-16 surrogates, which JSON readers reject) become U+FFFD."""
    if c >= 0x110000 or 0xD800 <= c <= 0xDFFF:
        return "\ufffd"
    return chr(c)


class Names:
    """Reads ``FName::Names`` entries; strings are cached by index."""

    def __init__(self, mem, offs, names_addr):
        self.mem = mem
        self.o = offs
        self.addr = names_addr
        self.cache = {}
        self.data = 0
        self.count = 0
        self.refresh()

    def refresh(self):
        self.data = self.mem.ptr(self.addr + self.o.arr_data)
        self.count = struct.unpack("<i", self.mem.raw(self.addr + self.o.arr_count, 4))[0]

    def entry(self, index):
        if index in self.cache:
            return self.cache[index]
        if index < 0:
            return None
        # The table is a growing array: when it grows the engine moves it, so
        # the data pointer read earlier may point at freed memory. Entries
        # themselves never move or change, hence the cache by index; every
        # miss reads the table's header again (two reads, and misses are rare
        # after the first frames).
        self.refresh()
        if index >= self.count or not self.data:
            return None
        ptr = self.mem.ptr(self.data + index * self.mem.ptr_size)
        if ptr == 0:
            return None
        word = self.mem.u32(ptr + self.o.entry_index)
        wide = bool(word & self.o.entry_wide_mask)
        text = self._chars(ptr + self.o.entry_chars, wide)
        self.cache[index] = text
        return text

    def _chars(self, addr, wide):
        unit = self.o.entry_wide_size if wide else 1
        out = []
        chunk = 64
        pos = addr
        while len(out) < MAX_NAME_CHARS:
            try:
                buf = self.mem.raw(pos, chunk * unit)
            except ReadError:
                if chunk == 1:
                    raise
                chunk = 1
                continue
            for i in range(0, len(buf), unit):
                c = int.from_bytes(buf[i : i + unit], "little")
                if c == 0:
                    return "".join(out)
                out.append(text_char(c))
            pos += len(buf)
        return "".join(out)

    def name(self, index, number):
        base = self.entry(index)
        if base is None:
            return None
        if number > 0:
            return "%s_%d" % (base, number - 1)
        return base


# -------------------------------------------------------------------- sampler


def finite(values):
    return all(math.isfinite(v) for v in values)


class Sampler:
    """Reads the player's state at the start of a frame.

    ``symbols`` maps the layout's mangled symbol names to load addresses.
    """

    def __init__(self, layout, read, symbols, ignore_sentinels=False):
        self.L = layout
        self.o = Offsets(layout)
        self.mem = Mem(read, layout.pointer_size)
        self.addr = dict(symbols)
        missing = [
            s
            for s in (self.o.sym_engine, self.o.sym_frame_counter, self.o.sym_names)
            if s not in self.addr
        ]
        if missing:
            raise LayoutError("unresolved symbols: %s" % ", ".join(missing))
        self.names = Names(self.mem, self.o, self.addr[self.o.sym_names])
        self.ignore_sentinels = ignore_sentinels
        self.class_names = {}
        self.checked = {}
        self.sentinel_failures = []
        self.pawn_ids = {}
        self.bindings = None
        # Addresses of the objects the last ``sample`` call walked (controller,
        # pawn, world_info, input), for a front end that reads more from the
        # same objects. Process-local: never written to a record.
        self.objects = {}

    # -- objects
    def obj_name(self, ptr):
        if not ptr:
            return None
        idx, num = struct.unpack("<ii", self.mem.raw(ptr + self.o.name, 8))
        return self.names.name(idx, num)

    def class_name(self, ptr):
        if not ptr:
            return None
        return self.class_name_of(self.mem.ptr(ptr + self.o.cls))

    def class_name_of(self, cls):
        """Name of a class object (cached by address)."""
        if not cls:
            return None
        if cls in self.class_names:
            return self.class_names[cls]
        n = self.obj_name(cls)
        self.class_names[cls] = n
        return n

    def outermost_name(self, ptr):
        """Name of the outermost object (the map package for a WorldInfo).

        Walked every frame (about four reads; the name strings are cached by
        name index): a new map's WorldInfo can reuse the address of the
        previous one, so a cache keyed by address could report the old map.
        """
        cur = ptr
        last = ptr
        for _ in range(MAX_OUTER_DEPTH):
            outer = self.mem.ptr(cur + self.o.outer)
            if not outer:
                break
            last = outer
            cur = outer
        return self.obj_name(last)

    def fstring(self, addr):
        data = self.mem.ptr(addr + self.o.fstring_data)
        count = struct.unpack("<i", self.mem.raw(addr + self.o.fstring_count, 4))[0]
        if not data or count <= 0:
            return ""
        count = min(count, MAX_STRING_CHARS)
        unit = self.o.fstring_char_size
        buf = self.mem.raw(data, count * unit)
        out = []
        for i in range(0, len(buf), unit):
            c = int.from_bytes(buf[i : i + unit], "little")
            if c == 0:
                break
            out.append(text_char(c))
        return "".join(out)

    def tarray(self, addr):
        b = Block(self.mem, addr, *span((self.o.arr_data, self.mem.ptr_size), (self.o.arr_count, 4)))
        return b.ptr(self.o.arr_data), b.i32(self.o.arr_count)

    # -- globals
    def frame_counter(self):
        return self.mem.u64(self.addr[self.o.sym_frame_counter])

    def global_f64(self, mangled):
        a = self.addr.get(mangled)
        return None if a is None else self.mem.f64(a)

    def global_u32(self, mangled):
        a = self.addr.get(mangled)
        return None if a is None else self.mem.u32(a)

    def world(self):
        a = self.addr.get(self.o.sym_world)
        return None if a is None else self.mem.ptr(a)

    def timing(self):
        return {
            "benchmarking": bool(self.global_u32(self.o.sym_benchmarking) or 0),
            "fixed_step": bool(self.global_u32(self.o.sym_fixed_step) or 0),
            "fixed_delta_time": self.global_f64(self.o.sym_fixed_delta_time),
            "delta_time": self.global_f64(self.o.sym_delta_time),
        }

    # -- resolution
    def resolve(self):
        """Pointers of the local player's objects, or None outside a level."""
        o = self.o
        engine = self.mem.ptr(self.addr[o.sym_engine])
        if not engine:
            return None
        data, count = self.tarray(engine + o.game_players)
        if not data or count < 1 or count > 8:
            return None
        player = self.mem.ptr(data)
        if not player:
            return None
        pc = self.mem.ptr(player + o.player_actor)
        if not pc:
            return None
        pawn = self.mem.ptr(pc + o.controller_pawn)
        if not pawn:
            return None
        return {"controller": pc, "pawn": pawn}

    def check_sentinels(self, role, ptr):
        """Compare the layout's sentinel fields of one object with the class defaults."""
        key = (role, ptr)
        if key in self.checked:
            return self.checked[key]
        bad = []
        for s in self.L.sentinels():
            if s["object"] != role:
                continue
            off = int(self.L.field(s["class"], s["name"])["offset"], 16)
            got = struct.unpack("<f", self.mem.raw(ptr + off, 4))[0]
            want = struct.unpack("<f", struct.pack("<f", float(s["expected"])))[0]
            if got != want:
                bad.append("%s.%s = %r (expected %r)" % (s["class"], s["name"], got, want))
        ok = not bad
        self.checked[key] = ok
        if bad:
            self.sentinel_failures.extend(bad)
        return ok

    def read_bindings(self, input_ptr):
        o = self.o
        data, count = self.tarray(input_ptr + o.bindings)
        out = []
        if not data or count <= 0:
            return out
        for i in range(min(count, MAX_BINDINGS)):
            kb = data + i * o.keybind_size
            idx, num = struct.unpack("<ii", self.mem.raw(kb + o.keybind_name, 8))
            name = self.names.name(idx, num)
            if name is None:
                continue
            out.append({"name": name, "command": self.fstring(kb + o.keybind_command)})
        return out

    def pawn_id(self, pawn):
        if pawn not in self.pawn_ids:
            self.pawn_ids[pawn] = len(self.pawn_ids)
        return self.pawn_ids[pawn]

    def pressed_keys(self, input_ptr, strict=False):
        """Names of ``PlayerInput.PressedKeys`` (the keys currently held).

        ``strict``: an array that cannot be a key list (a negative count, more
        than MAX_KEYS entries, entries without a data pointer, a name that
        does not resolve) raises ReadError instead of being read as "fewer
        keys". For a reader that samples a running process and must tell
        "no keys" from "could not read the keys".
        """
        o = self.o
        keys = []
        kd, kc = self.tarray(input_ptr + o.pressed_keys)
        if strict and (kc < 0 or kc > MAX_KEYS or (kc > 0 and not kd)):
            raise ReadError("PressedKeys is not a key list (count %d)" % kc)
        if kd and 0 < kc <= MAX_KEYS:
            step = o.fname_size
            raw = self.mem.raw(kd, kc * step)
            for i in range(kc):
                idx, num = struct.unpack("<ii", raw[i * step : i * step + 8])
                n = self.names.name(idx, num)
                if n is not None:
                    keys.append(n)
                elif strict:
                    raise ReadError("PressedKeys[%d] has no name (index %d)" % (i, idx))
        return keys

    def pressed_jump(self, controller_ptr):
        """``PlayerController.bPressedJump`` read on its own."""
        off, bit = self.o.pressed_jump
        return bool((self.mem.u32(controller_ptr + off) >> bit) & 1)

    def sample(self, frame, dt_arg=None, world_ptr=None):
        """One raw record, or None when there is nothing to record this frame.

        Returns (record, reason): reason is None for a record, else a short
        word saying why the frame was skipped.
        """
        o = self.o
        P = self.mem.ptr_size
        self.objects = {}
        objs = self.resolve()
        if objs is None:
            return None, "no-player"
        pc, pawn = objs["controller"], objs["pawn"]
        self.objects = dict(objs)
        if world_ptr is not None:
            gworld = self.world()
            if gworld and gworld != world_ptr:
                return None, "other-world"

        cb = Block(self.mem, pc, *span(
            (o.cls, P), (o.rotation, 12), (o.world_info, P), (o.player_camera, P),
            (o.pressed_jump[0], 4), (o.fov_angle, 4), (o.player_input, P)))
        wi = cb.ptr(o.world_info)
        if not wi:
            return None, "no-worldinfo"
        self.objects["world_info"] = wi
        self.objects["input"] = cb.ptr(o.player_input)
        wb = Block(self.mem, wi, *span(
            (o.time_dilation, 4), (o.time_seconds, 4), (o.real_time_seconds, 4),
            (o.delta_seconds, 4), (o.pauser, P)))
        if wb.ptr(o.pauser):
            return None, "paused"

        pb = Block(self.mem, pawn, *span(
            (o.cls, P), (o.location, 12), (o.rotation, 12), (o.physics, 1), (o.base, P),
            (o.velocity, 12), (o.acceleration, 12), (o.walkable_floor_z, 4),
            (o.ground_speed, 4), (o.air_speed, 4), (o.jump_z, 4), (o.air_control, 4),
            (o.eye_height, 4), (o.weapon, P)))
        controller_class = self.class_name_of(cb.ptr(o.cls))
        pawn_class = self.class_name_of(pb.ptr(o.cls))

        if not self.ignore_sentinels and pawn_class == "ASAMUPawn":
            if not self.check_sentinels("pawn", pawn):
                return None, "sentinel-mismatch"

        inp = cb.ptr(o.player_input)
        keys = []
        axes = None
        if inp:
            if not self.ignore_sentinels and not self.check_sentinels("input", inp):
                return None, "sentinel-mismatch"
            keys = self.pressed_keys(inp)
            ib = Block(self.mem, inp, *span(
                (o.a_base_y, 4), (o.a_mouse_x, 4), (o.a_mouse_y, 4), (o.a_forward, 4),
                (o.a_turn, 4), (o.a_strafe, 4), (o.a_look_up, 4)))
            axes = {
                "base_y": ib.f32(o.a_base_y),
                "strafe": ib.f32(o.a_strafe),
                "forward": ib.f32(o.a_forward),
                "turn": ib.f32(o.a_turn),
                "look_up": ib.f32(o.a_look_up),
                "mouse_x": ib.f32(o.a_mouse_x),
                "mouse_y": ib.f32(o.a_mouse_y),
            }
            if self.bindings is None:
                self.bindings = self.read_bindings(inp)

        cam = cb.ptr(o.player_camera)
        fov_camera = None
        if cam:
            v = struct.unpack("<f", self.mem.raw(cam + o.camera_fov, 4))[0]
            fov_camera = v

        gun = None
        weapon = pb.ptr(o.weapon)
        if weapon and self.class_name(weapon) == "GrappleGun":
            if not self.ignore_sentinels and not self.check_sentinels("gun", weapon):
                return None, "sentinel-mismatch"
            gb = Block(self.mem, weapon, *span(
                (o.gun_grappling[0], 4), (o.gun_location, 12), (o.gun_distance, 4),
                (o.gun_hit_loc_actor, P), (o.gun_times, 4), (o.gun_max, 4)))
            helper = gb.ptr(o.gun_hit_loc_actor)
            anchor = None
            if helper:
                anchor = list(struct.unpack("<3f", self.mem.raw(helper + o.location, 12)))
            gun = {
                "grappling": gb.flag(o.gun_grappling),
                "released": gb.flag(o.gun_released),
                "can_grapple": gb.flag(o.gun_can_grapple),
                "anchor": anchor,
                "grapple_location": gb.vec3(o.gun_location),
                "distance": gb.f32(o.gun_distance),
                "times_grappled": gb.i32(o.gun_times),
                "max_grapples": gb.i32(o.gun_max),
            }

        pawn_flags = None
        boots = None
        if pawn_class == "ASAMUPawn":
            fb = Block(self.mem, pawn, *span((o.pawn_has_jumped[0], 4), (o.pawn_rocket_boots, P)))
            pawn_flags = {
                "has_jumped": fb.flag(o.pawn_has_jumped),
                "power_jumped": fb.flag(o.pawn_power_jumped),
                "has_released_jump": fb.flag(o.pawn_released_jump),
                "sprinting": fb.flag(o.pawn_sprinting),
                "is_falling": fb.flag(o.pawn_is_falling),
            }
            bp = fb.ptr(o.pawn_rocket_boots)
            if bp:
                word = self.mem.u32(bp + o.boots_enabled[0])
                boots = {
                    "enabled": bool((word >> o.boots_enabled[1]) & 1),
                    "finished": bool((word >> o.boots_finished[1]) & 1),
                }

        base_ptr = pb.ptr(o.base)
        record = {
            "frame": frame,
            "dt_arg": dt_arg,
            "world": {
                "map": self.outermost_name(wi),
                "time_seconds": wb.f32(o.time_seconds),
                "real_time_seconds": wb.f32(o.real_time_seconds),
                "delta_seconds": wb.f32(o.delta_seconds),
                "time_dilation": wb.f32(o.time_dilation),
                "paused": False,
            },
            "player": {
                "controller_class": controller_class,
                "pawn_class": pawn_class,
                "pawn_id": self.pawn_id(pawn),
                "location": pb.vec3(o.location),
                "velocity": pb.vec3(o.velocity),
                "acceleration": pb.vec3(o.acceleration),
                "pawn_rotation": pb.rot(o.rotation),
                "view_rotation": cb.rot(o.rotation),
                "physics": pb.u8(o.physics),
                "base": self.obj_name(base_ptr) if base_ptr else None,
                "fov_camera": fov_camera,
                "fov_controller": cb.f32(o.fov_angle),
                "keys": keys,
                "pressed_jump": cb.flag(o.pressed_jump),
                "axes": axes,
                "ground_speed": pb.f32(o.ground_speed),
                "air_speed": pb.f32(o.air_speed),
                "jump_z": pb.f32(o.jump_z),
                "air_control": pb.f32(o.air_control),
                "eye_height": pb.f32(o.eye_height),
                "gun": gun,
                "pawn_flags": pawn_flags,
                "boots": boots,
            },
        }
        if not record_is_finite(record):
            return None, "non-finite"
        return record, None


def record_is_finite(rec):
    def walk(v):
        if isinstance(v, float):
            return math.isfinite(v)
        if isinstance(v, dict):
            return all(walk(x) for x in v.values())
        if isinstance(v, list):
            return all(walk(x) for x in v)
        return True

    return walk(rec)


def make_header(layout, bindings, scenario=None, launch_options=None, timing=None, notes=None,
                recorder=RECORDER, sample_point=SAMPLE_POINT):
    timing = timing or {}
    return {
        "format": RAW_FORMAT,
        "version": RAW_VERSION,
        "recorder": recorder,
        "layout": layout.id,
        "game_build": layout.game_build,
        "sample_point": sample_point,
        "scenario": scenario,
        "launch_options": launch_options,
        "benchmarking": timing.get("benchmarking"),
        "fixed_delta_time": timing.get("fixed_delta_time"),
        "bindings": list(bindings or []),
        "notes": list(notes or []),
    }


def dumps(obj):
    return json.dumps(obj, separators=(",", ":"), allow_nan=False)


# ----------------------------------------------------------------- conversion
#
# Mirrors tools/asamu-trace/src/convert.rs (the reference). Record R_i is taken
# at the start of frame F_i: its state is the end of frame F_i - 1, its keys
# and bPressedJump are the input of frame F_i. Sample 0 is R_0's state with
# neutral input; sample k >= 1 is frame F_0 + k - 1: input from R_(k-1), state
# and dt (WorldInfo.DeltaSeconds) from R_k, look deltas from R_(k-1) -> R_k.


def f32(x):
    return struct.unpack("<f", struct.pack("<f", x))[0]


def signed16(units):
    return ((int(units) + 32768) % 65536) - 32768


def units_to_radians(units):
    return f32(float(units) * TAU_OVER_UNITS)


def floor_half(x):
    return math.floor(x + 0.5)


class Actions:
    __slots__ = ("forward", "right", "jump", "grapple", "sprint", "power_jump", "use")

    def __init__(self):
        self.forward = 0
        self.right = 0
        self.jump = False
        self.grapple = False
        self.sprint = False
        self.power_jump = False
        self.use = False


_NUMBER = re.compile(r"[+-]?(?:[0-9]+(?:\.[0-9]*)?|\.[0-9]+)(?:[eE][+-]?[0-9]+)?\Z")
_ASCII_SPACE = re.compile(r"[ \t\n\x0c\r]+")


def ascii_lower(s):
    return "".join(chr(ord(c) + 32) if "A" <= c <= "Z" else c for c in s)


def ascii_tokens(s):
    return [t for t in _ASCII_SPACE.split(s) if t]


def _parse_speed(tokens):
    """Sign source of an ``Axis`` command: its ``Speed=`` value (default 1)."""
    for t in tokens[2:]:
        if ascii_lower(t).startswith("speed="):
            v = t[6:]
            return float(v) if _NUMBER.match(v) else 0.0
    return 1.0


def key_actions(bindings):
    """key name (ASCII lower case) -> list of (action, sign), following aliases.

    A later binding of the same name replaces an earlier one (UE3 searches the
    bindings from the end; TENTATIVE). Modifier flags are ignored. One key's
    expansion visits at most MAX_PARTS_PER_KEY command parts (aliases
    included), exactly as the Rust converter does.
    """
    table = {}
    for b in bindings:
        table[ascii_lower(b["name"])] = b["command"]

    def expand(command, depth, budget, out):
        for part in command.split("|"):
            if budget[0] == 0:
                return
            budget[0] -= 1
            tokens = ascii_tokens(part)
            if not tokens:
                continue
            head = ascii_lower(tokens[0])
            if head == "onrelease":
                continue
            if len(tokens) == 1 and head in table:
                if depth < 8:
                    expand(table[head], depth + 1, budget, out)
                continue
            if head == "axis" and len(tokens) >= 2:
                axis = ascii_lower(tokens[1])
                s = _parse_speed(tokens)
                sign = 1 if s > 0 else (-1 if s < 0 else 0)
                if axis == "abasey" and sign:
                    out.append(("forward", sign))
                elif axis == "astrafe" and sign:
                    out.append(("right", sign))
            elif head == "jump":
                out.append(("jump", 1))
            elif head == "startfire":
                out.append(("grapple", 1))
            elif head == "startsprinting":
                out.append(("sprint", 1))
            elif head == "powerjumpkeydown":
                out.append(("power_jump", 1))
            elif head == "use":
                out.append(("use", 1))

    result = {}
    for name, command in table.items():
        acts = []
        expand(command, 0, [MAX_PARTS_PER_KEY], acts)
        result[name] = acts
    return result


def actions_of(rec, kmap):
    a = Actions()
    p = rec["player"]
    for k in p["keys"]:
        for tag, sign in kmap.get(ascii_lower(k), ()):
            if tag == "forward":
                a.forward += sign
            elif tag == "right":
                a.right += sign
            else:
                setattr(a, tag, True)
    a.forward = max(-1, min(1, a.forward))
    a.right = max(-1, min(1, a.right))
    return a


def _sign(v):
    return 1 if v > 0 else (-1 if v < 0 else 0)


def usable(rec):
    w = rec.get("world")
    return rec.get("player") is not None and w is not None and not w.get("paused", False)


def split_runs(records):
    runs = []
    cur = []
    for r in records:
        if not usable(r):
            if cur:
                runs.append(cur)
            cur = []
            continue
        if cur:
            prev = cur[-1]
            same = (
                r["frame"] == prev["frame"] + 1
                and r["world"].get("map") == prev["world"].get("map")
                and r["player"]["pawn_id"] == prev["player"]["pawn_id"]
                and r["player"]["controller_class"] == prev["player"]["controller_class"]
                and r["player"]["pawn_class"] == prev["player"]["pawn_class"]
                # A world's clock never goes back: a smaller TimeSeconds is
                # another world (the level was loaded again, possibly with its
                # WorldInfo and pawn at the old addresses).
                and r["world"]["time_seconds"] >= prev["world"]["time_seconds"]
            )
            if not same:
                runs.append(cur)
                cur = []
        cur.append(r)
    if cur:
        runs.append(cur)
    return [r for r in runs if len(r) >= 2]


def _state(rec, counters):
    p = rec["player"]
    fov = p.get("fov_camera")
    if fov is None or not (0.0 < fov < 180.0):
        fov = p["fov_controller"]
        counters["fov_controller"] += 1
        if not (0.0 < fov < 180.0):
            raise ValueError("frame %d: no usable field of view" % rec["frame"])
    physics = p["physics"]
    flying = physics == PHYS_FLYING
    gun = p.get("gun")
    # Without gun data there is no anchor, and an attached sample needs one.
    if gun is not None:
        attached = bool(gun["grappling"])
        anchor = gun["anchor"] if gun["anchor"] is not None else gun["grapple_location"]
        if attached != flying:
            counters["grapple_physics"] += 1
    else:
        attached = False
        anchor = None
        if flying:
            counters["flying_without_gun"] += 1
    vr = p["view_rotation"]
    return {
        "position": [f32(x) for x in p["location"]],
        "velocity": [f32(x) for x in p["velocity"]],
        "yaw": units_to_radians(signed16(vr[1])),
        "pitch": units_to_radians(signed16(vr[0])),
        "fov": f32(fov),
        "grapple_state": "attached" if attached else "idle",
        "grapple_anchor": [f32(x) for x in anchor] if attached else None,
        "rope_length": None,
        "grounded": physics == PHYS_WALKING,
    }


def _init_note(rec):
    p = rec["player"]
    gun = p.get("gun")
    flags = p.get("pawn_flags")
    boots = p.get("boots")
    init = {
        "air_control": f32(p["air_control"]),
        "base": p.get("base"),
        "ground_speed": f32(p["ground_speed"]),
        "jump_z": f32(p["jump_z"]),
        "max_grapples": gun["max_grapples"] if gun else None,
        "physics": p["physics"],
        "rocket_boots": boots["enabled"] if boots else None,
        "sprinting": flags["sprinting"] if flags else None,
        "times_grappled": gun["times_grappled"] if gun else None,
    }
    return "init: " + json.dumps(init, sort_keys=True, separators=(",", ":"))


def tick_rate_of(dts):
    if not dts:
        return None
    d = dts[0]
    if d <= 0.0 or any(x != d for x in dts):
        return None
    rate = 1.0 / float(d)
    r = floor_half(rate)
    if abs(rate - r) < 1e-3:
        return f32(float(r))
    return f32(rate)


def convert_raw(header, records, level=None):
    """List of (meta, samples) for every usable run."""
    if header.get("format") != RAW_FORMAT or header.get("version") != RAW_VERSION:
        raise ValueError("not an %s v%d file" % (RAW_FORMAT, RAW_VERSION))
    bindings = header.get("bindings") or []
    kmap = key_actions(bindings) if bindings else None
    runs = split_runs(records)
    out = []
    for si, run in enumerate(runs):
        counters = {"fov_controller": 0, "grapple_physics": 0, "flying_without_gun": 0, "axis_mismatch": 0}
        samples = []
        st = _state(run[0], counters)
        neutral = {
            "move_forward": 0.0, "move_right": 0.0, "look_yaw_delta": 0.0,
            "look_pitch_delta": 0.0, "jump_pressed": False, "jump_held": False,
            "grapple_held": False, "sprint_held": False, "power_jump_held": False,
            "use_pressed": False,
        }
        samples.append(dict(tick=0, time=0.0, input=neutral, **st))
        time = 0.0
        dts = []
        for k in range(1, len(run)):
            cur, nxt = run[k - 1], run[k]
            prev = run[k - 2] if k >= 2 else None
            dt = f32(nxt["world"]["delta_seconds"])
            dts.append(dt)
            time = time + float(dt)
            cp, np_ = cur["player"], nxt["player"]
            if kmap is not None:
                a = actions_of(cur, kmap)
                pa = actions_of(prev, kmap) if prev is not None else None
                fwd, right = a.forward, a.right
                axes = np_.get("axes")
                if axes is not None and (
                    _sign(axes["base_y"]) != fwd or _sign(axes["strafe"]) != right
                ):
                    counters["axis_mismatch"] += 1
                jump_held = a.jump
                key_edge = pa is not None and a.jump and not pa.jump
                use_pressed = pa is not None and a.use and not pa.use
                grapple, sprint, power = a.grapple, a.sprint, a.power_jump
            else:
                axes = np_.get("axes") or {}
                fwd = _sign(axes.get("base_y", 0.0))
                right = _sign(axes.get("strafe", 0.0))
                jump_held = False
                key_edge = False
                use_pressed = False
                grapple = sprint = power = False
            flag_edge = bool(cp["pressed_jump"]) and not (
                prev is not None and prev["player"]["pressed_jump"]
            )
            dyaw = signed16(np_["view_rotation"][1] - cp["view_rotation"][1])
            dpitch = signed16(np_["view_rotation"][0] - cp["view_rotation"][0])
            inp = {
                "move_forward": float(fwd),
                "move_right": float(right),
                "look_yaw_delta": units_to_radians(dyaw),
                "look_pitch_delta": units_to_radians(dpitch),
                "jump_pressed": bool(key_edge or flag_edge),
                "jump_held": bool(jump_held),
                "grapple_held": bool(grapple),
                "sprint_held": bool(sprint),
                "power_jump_held": bool(power),
                "use_pressed": bool(use_pressed),
            }
            st = _state(nxt, counters)
            samples.append(dict(tick=k, time=time, input=inp, **st))
        notes = ["raw: %s; sample point: %s" % (header.get("recorder"), header.get("sample_point"))]
        if header.get("launch_options"):
            notes.append("launch options: %s" % header["launch_options"])
        if header.get("scenario"):
            notes.append("scenario: %s" % header["scenario"])
        notes.append(
            "frames %d..=%d (segment %d of %d)"
            % (run[0]["frame"], run[-1]["frame"], si + 1, len(runs))
        )
        if kmap is not None:
            notes.append("input: key bindings read from the running game (%d bindings)" % len(bindings))
            if counters["axis_mismatch"]:
                notes.append(
                    "check: aBaseY/aStrafe sign disagrees with the mapped keys on %d of %d samples"
                    % (counters["axis_mismatch"], len(run) - 1)
                )
        else:
            notes.append(
                "input: no key bindings in the recording; move axes from the signs of "
                "PlayerInput aBaseY/aStrafe, jump from bPressedJump, other actions unknown (false)"
            )
        if counters["grapple_physics"]:
            notes.append(
                "check: grapple flag and PHYS_Flying disagree on %d samples" % counters["grapple_physics"]
            )
        if counters["flying_without_gun"]:
            notes.append(
                "check: PHYS_Flying without grapple gun data on %d samples (written as idle: no anchor)"
                % counters["flying_without_gun"]
            )
        if counters["fov_controller"]:
            notes.append(
                "fov: controller FOVAngle used on %d samples (camera POV FOV unavailable)"
                % counters["fov_controller"]
            )
        notes.extend(header.get("notes") or [])
        notes.append(_init_note(run[0]))
        meta = {
            "format": TRACE_FORMAT,
            "schema_version": TRACE_SCHEMA_VERSION,
            "source": "original",
            "game_build": header.get("game_build"),
            "level": level if level is not None else run[0]["world"].get("map"),
            "tick_rate": tick_rate_of(dts),
            "units": "uu",
            "notes": notes,
        }
        out.append((meta, samples))
    return out


def read_raw(path):
    header = None
    records = []
    with open(path, "r", encoding="utf-8") as fh:
        for n, line in enumerate(fh, 1):
            line = line.strip()
            if not line:
                continue
            obj = json.loads(line)
            if header is None:
                header = obj
            else:
                if "frame" not in obj:
                    raise ValueError("line %d: not a frame record" % n)
                records.append(obj)
    if header is None:
        raise ValueError("empty raw file")
    return header, records


def output_paths(raw_path, count, out_dir=None):
    base = os.path.basename(raw_path)
    stem = base[: -len(".raw.jsonl")] if base.endswith(".raw.jsonl") else os.path.splitext(base)[0]
    d = out_dir or os.path.dirname(os.path.abspath(raw_path))
    if count == 1:
        return [os.path.join(d, stem + ".trace.jsonl")]
    return [os.path.join(d, "%s.seg%d.trace.jsonl" % (stem, i)) for i in range(count)]


def install_root(executable):
    """The folder that holds the game's ``.app`` bundle (Mac), else the
    executable's folder (other platforms)."""
    exe = os.path.realpath(executable)
    cur = os.path.dirname(exe)
    while True:
        if cur.endswith(".app"):
            return os.path.dirname(cur)
        parent = os.path.dirname(cur)
        if parent == cur:
            return os.path.dirname(exe)
        cur = parent


def is_inside(path, root):
    """True if ``path`` is ``root`` or lies below it (symbolic links resolved)."""
    p = os.path.realpath(path)
    r = os.path.realpath(root)
    try:
        return os.path.commonpath([p, r]) == r
    except ValueError:  # different drives (Windows)
        return False


def create_unique(directory, stem, suffix):
    """Creates and opens ``<stem><suffix>`` in ``directory`` for writing,
    adding ``-1``, ``-2``, ... instead of overwriting an existing file.
    Returns (file object, absolute path)."""
    for n in range(1000):
        name = stem + ("" if n == 0 else "-%d" % n) + suffix
        path = os.path.abspath(os.path.join(directory, name))
        try:
            return open(path, "x", encoding="utf-8", newline="\n"), path
        except FileExistsError:
            continue
    raise OSError("no free file name for %s%s in %s" % (stem, suffix, directory))


def safe_component(text):
    """A user-given word as one harmless file-name component."""
    out = "".join(c if (c.isalnum() and c.isascii()) or c in "-_.+" else "_" for c in text)
    out = out.strip(".") or "_"
    return out[:64]


def write_trace(meta, samples, path):
    with open(path, "w", encoding="utf-8", newline="\n") as fh:
        fh.write(dumps(meta) + "\n")
        for s in samples:
            fh.write(dumps(s) + "\n")


def convert_file(raw_path, out_dir=None):
    header, records = read_raw(raw_path)
    traces = convert_raw(header, records)
    paths = output_paths(raw_path, len(traces), out_dir)
    for (meta, samples), p in zip(traces, paths):
        write_trace(meta, samples, p)
    return paths


# ------------------------------------------------------------------ self-test


class FakeMemory:
    """A flat memory image for the self-test (addresses from ``base``).

    ``buf`` may be any writable buffer of bytes (default: a new bytearray);
    ``ptr_size`` is the width ``p`` writes.
    """

    def __init__(self, base=0x10000000, size=1 << 20, ptr_size=8, buf=None):
        self.base = base
        self.buf = bytearray(size) if buf is None else buf
        self.ptr_size = ptr_size
        self.top = base + 0x100

    def alloc(self, size, align=16):
        a = (self.top + align - 1) // align * align
        self.top = a + size
        if self.top > self.base + len(self.buf):
            raise MemoryError("fake memory full")
        return a

    def write(self, addr, data):
        i = addr - self.base
        self.buf[i : i + len(data)] = data

    def read(self, addr, size):
        i = addr - self.base
        if i < 0 or i + size > len(self.buf):
            return None
        return bytes(self.buf[i : i + size])

    def p(self, addr, v):
        self.write(addr, struct.pack("<Q" if self.ptr_size == 8 else "<I", v))

    def f(self, addr, v):
        self.write(addr, struct.pack("<f", v))

    def i(self, addr, v):
        self.write(addr, struct.pack("<i", v))

    def setbit(self, addr, bit, on):
        w = struct.unpack("<I", self.read(addr, 4))[0]
        w = (w | (1 << bit)) if on else (w & ~(1 << bit))
        self.write(addr, struct.pack("<I", w))


class FakeGame:
    """A synthetic memory image of the objects the sampler walks (self-tests).

    Built from the layout, so it checks the sampler's use of the layout, not
    the layout itself (that is the Rust test's job).
    """

    NAMES = [
        "None", "ASAMUPlayerController", "ASAMUPawn", "GrappleGun", "ASAMUPlayerInput",
        "Camera", "WorldInfo", "W", "SpaceBar", "LeftMouseButton", "GBA_MoveForward",
        "GBA_ReleaseableJump", "GBA_Fire", "AG-Workshop", "TheWorld", "PersistentLevel",
        "StaticMeshActor", "Class", "ASAMURocketBoots", "Wide\u00e9Name",
    ]

    def __init__(self, layout=None, memory=None, symbols_at=None):
        """``memory``: a FakeMemory to build in (default: a new one with the
        layout's pointer size). ``symbols_at``: mangled symbol name -> address
        for the globals (default: allocated like everything else)."""
        L = self.L = layout or Layout.load()
        o = self.o = Offsets(L)
        m = self.m = memory or FakeMemory(ptr_size=o.ptr)
        P = o.ptr
        at = dict(symbols_at or {})
        self.ni = {n: i for i, n in enumerate(self.NAMES)}
        table = m.alloc(P * len(self.NAMES))
        for i, n in enumerate(self.NAMES):
            wide = any(ord(c) > 127 for c in n)
            e = m.alloc(o.entry_chars + (len(n) + 1) * (o.entry_wide_size if wide else 1))
            m.write(e + o.entry_index, struct.pack("<I", (i << 1) | (1 if wide else 0)))
            if wide:
                m.write(e + o.entry_chars, self.text(n, o.entry_wide_size))
            else:
                m.write(e + o.entry_chars, n.encode("ascii") + b"\0")
            m.p(table + P * i, e)
        names_sym = at.get(o.sym_names) or m.alloc(L.st_size("TArray"))
        m.p(names_sym + o.arr_data, table)
        m.i(names_sym + o.arr_count, len(self.NAMES))

        meta_cls = self.obj(0x100, 0, "Class")
        classes = {n: self.obj(0x100, meta_cls, n) for n in (
            "ASAMUPlayerController", "ASAMUPawn", "GrappleGun", "ASAMUPlayerInput", "Camera",
            "WorldInfo", "StaticMeshActor", "ASAMURocketBoots")}
        package = self.obj(0x80, 0, "AG-Workshop")
        self.world = self.obj(0x80, 0, "TheWorld", outer=package)
        level = self.obj(0x80, 0, "PersistentLevel", outer=self.world)
        self.wi = wi = self.obj(0xB00, classes["WorldInfo"], "WorldInfo", outer=level)
        m.f(wi + o.time_dilation, 1.0)
        m.f(wi + o.delta_seconds, 1.0 / 60.0)
        self.pc = pc = self.obj(0xB30, classes["ASAMUPlayerController"], "ASAMUPlayerController")
        self.pawn = pawn = self.obj(0xC00, classes["ASAMUPawn"], "ASAMUPawn")
        self.gun = gun = self.obj(0x700, classes["GrappleGun"], "GrappleGun")
        self.inp = inp = self.obj(0x300, classes["ASAMUPlayerInput"], "ASAMUPlayerInput")
        cam = self.obj(0x600, classes["Camera"], "Camera")
        floor = self.obj(0x300, classes["StaticMeshActor"], "StaticMeshActor", number=13)
        boots = self.obj(0x300, classes["ASAMURocketBoots"], "ASAMURocketBoots")
        self.helper = helper = self.obj(0x300, classes["StaticMeshActor"], "StaticMeshActor", number=1)
        for s in L.sentinels():
            target = {"pawn": pawn, "gun": gun, "input": inp}[s["object"]]
            m.f(target + L.off(s["class"], s["name"]), float(s["expected"]))
        engine = m.alloc(0x900)
        players = m.alloc(P)
        player = m.alloc(0x100)
        m.p(engine + o.game_players + o.arr_data, players)
        m.i(engine + o.game_players + o.arr_count, 1)
        m.p(players, player)
        m.p(player + o.player_actor, pc)
        m.p(pc + o.controller_pawn, pawn)
        m.p(pc + o.world_info, wi)
        m.p(pc + o.player_camera, cam)
        m.p(pc + o.player_input, inp)
        m.f(pc + o.fov_angle, 90.0)
        m.f(cam + o.camera_fov, 90.0)
        m.p(pawn + o.world_info, wi)
        m.p(pawn + o.weapon, gun)
        m.p(pawn + o.base, floor)
        m.p(pawn + o.pawn_rocket_boots, boots)
        m.setbit(boots + o.boots_enabled[0], o.boots_enabled[1], True)
        m.f(pawn + o.ground_speed, 440.0)
        m.f(pawn + o.air_control, 0.3)
        m.f(pawn + o.jump_z, 1000.0)
        m.f(pawn + o.eye_height, 38.0)
        m.p(gun + o.gun_hit_loc_actor, helper)
        m.i(gun + o.gun_max, 2)
        binds = [("GBA_MoveForward", "Axis aBaseY Speed=1.0"), ("W", "GBA_MoveForward"),
                 ("GBA_ReleaseableJump", "Jump | OnRelease ReleaseJump"),
                 ("SpaceBar", "GBA_ReleaseableJump | RocketBoostKeyDown"),
                 ("GBA_Fire", "StartFire | OnRelease StopFire"), ("LeftMouseButton", "GBA_Fire")]
        barr = m.alloc(o.keybind_size * len(binds))
        for i, (n, c) in enumerate(binds):
            kb = barr + i * o.keybind_size
            m.i(kb + o.keybind_name, self.ni[n])
            sp = m.alloc((len(c) + 1) * o.fstring_char_size)
            m.write(sp, self.text(c, o.fstring_char_size))
            m.p(kb + o.keybind_command + o.fstring_data, sp)
            m.i(kb + o.keybind_command + o.fstring_count, len(c) + 1)
        m.p(inp + o.bindings + o.arr_data, barr)
        m.i(inp + o.bindings + o.arr_count, len(binds))
        self.keys = m.alloc(o.fname_size * 4)
        m.p(inp + o.pressed_keys + o.arr_data, self.keys)
        self.counter = at.get(o.sym_frame_counter) or m.alloc(8)
        gengine = at.get(o.sym_engine) or m.alloc(P)
        m.p(gengine, engine)
        gworld = at.get(o.sym_world) or m.alloc(P)
        m.p(gworld, self.world)
        self.symbols = {
            o.sym_engine: gengine, o.sym_frame_counter: self.counter,
            o.sym_names: names_sym, o.sym_world: gworld,
        }

    @staticmethod
    def text(s, char_size):
        """``s`` as a zero-terminated game string with 2- or 4-byte characters."""
        return s.encode("utf-32-le" if char_size == 4 else "utf-16-le") + b"\0" * char_size

    def obj(self, size, cls_ptr, name, number=0, outer=0):
        m, o = self.m, self.o
        a = m.alloc(size)
        m.p(a + o.cls, cls_ptr)
        m.i(a + o.name, self.ni[name])
        m.i(a + o.name + 4, number)
        m.p(a + o.outer, outer)
        return a

    def press(self, *ks):
        m, o = self.m, self.o
        for i, k in enumerate(ks):
            m.i(self.keys + o.fname_size * i, self.ni[k])
            m.i(self.keys + o.fname_size * i + 4, 0)
        m.i(self.inp + o.pressed_keys + o.arr_count, len(ks))

    def set_state(self, frame, keys, x, yaw, physics, grappling, pitch=0):
        m, o = self.m, self.o
        m.write(self.counter, struct.pack("<Q", frame))
        self.press(*keys)
        m.f(self.pawn + o.location, x)
        m.f(self.pawn + o.location + 4, -3.5)
        m.f(self.pawn + o.location + 8, 45.05)
        m.f(self.pawn + o.velocity, x * 2.0)
        m.i(self.pc + o.rotation + 4, yaw)
        m.i(self.pc + o.rotation, pitch)
        m.write(self.pawn + o.physics, bytes([physics]))
        m.setbit(self.gun + o.gun_grappling[0], o.gun_grappling[1], grappling)
        m.f(self.helper + o.location, 500.0)

    # The scripted frames of the self-tests:
    # (keys, location x, yaw units, physics, grappling, pitch units)
    FRAMES = [
        ((), 0.0, 0, 1, False, 0),
        (("W",), 0.0, 0, 1, False, 0),
        (("W", "SpaceBar"), 7.25, 182, 1, False, 0),
        (("LeftMouseButton",), 20.5, 65536 + 364, 2, False, 0),
        (("LeftMouseButton",), 40.0, 364, 4, True, -1000),
    ]


def check_selftest_trace(meta, samples):
    """Expected canonical trace of FakeGame.FRAMES."""
    assert meta["level"] == "AG-Workshop" and meta["tick_rate"] == 60.0, meta
    assert meta["source"] == "original", meta
    assert [s["tick"] for s in samples] == [0, 1, 2, 3, 4]
    assert samples[1]["input"]["move_forward"] == 0.0  # frame 1000: no keys
    assert samples[2]["input"]["move_forward"] == 1.0  # frame 1001: W
    assert samples[3]["input"]["jump_pressed"] and samples[3]["input"]["jump_held"]
    # Yaw 182 -> 65536 + 364 wraps to a delta of 182 units; 65536 + 364 -> 364 is no turn.
    assert samples[3]["input"]["look_yaw_delta"] == units_to_radians(182)
    assert samples[4]["input"]["grapple_held"] and not samples[4]["input"]["jump_pressed"]
    assert samples[4]["input"]["look_yaw_delta"] == 0.0
    assert samples[2]["grounded"] is True and samples[3]["grounded"] is False
    assert samples[4]["grapple_state"] == "attached" and samples[4]["grapple_anchor"] == [500.0, 0.0, 0.0]
    assert samples[4]["pitch"] == units_to_radians(-1000)
    assert samples[2]["position"][0] == 7.25
    assert any(n.startswith("init: ") for n in meta["notes"])


def _selftest(layout_path=None):
    g = FakeGame(Layout.load(layout_path) if layout_path else None)
    sampler = Sampler(g.L, g.m.read, g.symbols)
    lines = []
    per_frame = []
    for n, (ks, x, yaw, phys, grappling, pitch) in enumerate(FakeGame.FRAMES):
        g.set_state(1000 + n, ks, x, yaw, phys, grappling, pitch)
        before = sampler.mem.reads
        rec, why = sampler.sample(sampler.frame_counter(), 1.0 / 60.0, world_ptr=g.world)
        per_frame.append(sampler.mem.reads - before)
        assert why is None, why
        lines.append(rec)
    header = make_header(g.L, sampler.bindings, scenario="selftest", timing=sampler.timing())
    assert header["bindings"][1] == {"name": "W", "command": "GBA_MoveForward"}, header["bindings"]
    assert lines[0]["frame"] == 1000
    assert lines[0]["world"]["map"] == "AG-Workshop", lines[0]["world"]
    assert lines[0]["player"]["base"] == "StaticMeshActor_12", lines[0]["player"]["base"]
    assert lines[0]["player"]["controller_class"] == "ASAMUPlayerController"
    assert sampler.names.name(g.ni["Wide\u00e9Name"], 0) == "Wide\u00e9Name"
    assert lines[2]["player"]["keys"] == ["W", "SpaceBar"]
    assert lines[2]["player"]["eye_height"] == 38.0
    for rec in lines:
        dumps(rec)
    traces = convert_raw(header, lines)
    assert len(traces) == 1, len(traces)
    check_selftest_trace(*traces[0])
    assert sampler.objects["world_info"] == g.wi and sampler.objects["input"] == g.inp, sampler.objects
    assert sampler.pressed_keys(g.inp) == lines[-1]["player"]["keys"]
    assert sampler.pressed_jump(g.pc) is lines[-1]["player"]["pressed_jump"]
    # A frame of another world (seamless travel) is skipped.
    rec, why = sampler.sample(2000, world_ptr=g.world + 8)
    assert rec is None and why == "other-world", why
    # A gap splits the recording.
    gap = [dict(r) for r in lines]
    gap[3] = dict(gap[3], frame=gap[3]["frame"] + 5)
    gap[4] = dict(gap[4], frame=gap[4]["frame"] + 5)
    assert [len(s) for _, s in convert_raw(header, gap)] == [3, 2]
    # So does a world clock that goes back (the level was loaded again and
    # everything landed at the old addresses): no run joins the two worlds.
    again = [dict(r, world=dict(r["world"], time_seconds=(50.0 if i < 3 else 0.25) + i / 60.0))
             for i, r in enumerate(lines)]
    assert [len(s) for _, s in convert_raw(header, again)] == [3, 2]
    steady = [dict(r, world=dict(r["world"], time_seconds=50.0)) for r in lines]
    assert [len(s) for _, s in convert_raw(header, steady)] == [5]
    # Sentinel mismatch is detected.
    g.m.f(g.gun + g.o.gun_max_distance, 1234.0)
    sampler2 = Sampler(g.L, g.m.read, g.symbols)
    rec, why = sampler2.sample(2000)
    assert rec is None and why == "sentinel-mismatch", why
    assert sampler2.sentinel_failures
    g.m.f(g.gun + g.o.gun_max_distance, 5000.0)

    # A new map whose WorldInfo reuses the old WorldInfo's address is named
    # by its own package (no stale cache).
    new_package = g.obj(0x80, 0, "Camera")  # any name stands in for the next map
    new_world = g.obj(0x80, 0, "TheWorld", outer=new_package)
    g.m.p(g.wi + g.o.outer, g.obj(0x80, 0, "PersistentLevel", outer=new_world))
    rec, why = sampler.sample(3000, world_ptr=g.world)
    assert why is None and rec["world"]["map"] == "Camera", (why, rec and rec["world"])

    # The name table grows and the engine moves it: a name first needed after
    # that is read through the table's new address, not through the old one
    # (here cleared, as freed memory may be).
    o, m = g.o, g.m
    names_sym = g.symbols[o.sym_names]
    old_table = sampler.names.data
    n = len(FakeGame.NAMES)
    new_table = m.alloc(o.ptr * (n + 8))
    m.write(new_table, m.read(old_table, o.ptr * n))
    m.write(old_table, bytes(o.ptr * n))
    m.p(names_sym + o.arr_data, new_table)
    assert "PersistentLevel" not in sampler.names.cache.values()
    floor = sampler.mem.ptr(g.pawn + o.base)
    m.i(floor + o.name, g.ni["PersistentLevel"])
    rec, why = sampler.sample(3001, world_ptr=g.world)
    assert why is None and rec["player"]["base"] == "PersistentLevel_12", (why, rec and rec["player"]["base"])
    assert sampler.names.data == new_table
    m.i(floor + o.name, g.ni["StaticMeshActor"])

    # Game strings never carry invalid code points (JSON readers reject lone
    # surrogates); they become U+FFFD.
    assert text_char(0xD800) == "\ufffd" and text_char(0x110000) == "\ufffd" and text_char(0xE9) == "\u00e9"
    assert json.loads(dumps({"n": "".join(text_char(c) for c in (0x41, 0xDFFF, 0x1F600))})) == {"n": "A\ufffd\U0001F600"}

    # Alias expansion is bounded and counts every part, as in Rust.
    wide = [{"name": "L%d" % i, "command": "|".join(["L%d" % (i + 1)] * 16)} for i in range(6)]
    wide += [{"name": "L6", "command": "Jump | Axis aBaseY Speed=1"}, {"name": "K", "command": "L0"}]
    km = key_actions(wide)
    assert 0 < len(km["k"]) <= MAX_PARTS_PER_KEY, len(km["k"])
    km = key_actions([{"name": "B", "command": "Jump|use"}, {"name": "A", "command": "B|B|B"}])
    assert len(km["a"]) == 6, km["a"]

    # Output paths: never inside the install, never overwriting.
    exe = os.path.join(os.sep, "x", "A Story", "Game.app", "Contents", "MacOS", "ASAMU")
    root = install_root(exe)
    assert root == os.path.realpath(os.path.join(os.sep, "x", "A Story")), root
    assert is_inside(os.path.join(root, "Game.app", "traces"), root)
    assert not is_inside(os.path.join(os.sep, "x", "A Story2"), root)
    assert safe_component("../T1 x") == "_T1_x" and safe_component("..") == "_"
    import tempfile

    with tempfile.TemporaryDirectory() as d:
        f1, p1 = create_unique(d, "a", ".raw.jsonl")
        f2, p2 = create_unique(d, "a", ".raw.jsonl")
        f1.close()
        f2.close()
        assert p1 != p2 and p2.endswith("a-1.raw.jsonl"), (p1, p2)

    steady = per_frame[-1]
    print(
        "selftest ok (layout %s, %d-byte pointers; %d memory reads for %d frames; %d per frame after warm-up)"
        % (g.L.id, g.o.ptr, sampler.mem.reads, len(FakeGame.FRAMES), steady)
    )
    return 0


def main(argv):
    import argparse

    ap = argparse.ArgumentParser(prog="asamu_recorder_core.py")
    sub = ap.add_subparsers(dest="cmd")
    c = sub.add_parser("convert", help="raw recording -> canonical asamu-trace v1 file(s)")
    c.add_argument("raw")
    c.add_argument("--out-dir")
    t = sub.add_parser("selftest", help="exercise sampler and converter on a fake memory image")
    t.add_argument("--layout", help="layout file to build the image from (default: the Mac layout)")
    args = ap.parse_args(argv)
    if args.cmd == "convert":
        for p in convert_file(args.raw, args.out_dir):
            print(p)
        return 0
    if args.cmd == "selftest":
        return _selftest(args.layout)
    ap.print_help()
    return 2


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
