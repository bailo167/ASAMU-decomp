"""A recording of the recorder's fake game with markers and dropped frames,
left exactly as recorder 0.2.0 leaves one: the raw file and, next to it, the
``.stats.json`` the recorder itself writes when the recording ends.

    python3 -I -B fake_recording.py RECORDER_DIR OUT_DIR

RECORDER_DIR is ``tools/trace-recorder``; the fake game, its engine and the
recording loop are that folder's own (``test_win_glue.py``, ``asamu_win.py``),
so the files are what the recorder writes, not what a test thinks it writes.
``tools/asamu-trace/tests/pipeline.rs`` feeds them to the Rust tool.

What happens in the session (70 records, every optional group on):

- a marker is requested before the first record exists, and three more right
  after the 10th, the 30th and the 50th record were written (through the
  recorder's own request files and ``Recording.take_marks``);
- the tick of script frame 20 starts while its sample is read: that frame
  has no record (``torn``) and neither has the one after it
  (``unconfirmed``);
- the keys of one frame cannot be read during its whole tick: its record is
  written without its input (``no-input``) and the frame after it has no
  record;
- the grapple button is held twice (the fake game locks the camera's FOV at
  50 and fires its weapon meanwhile).

Prints ``skipped: ...`` and writes nothing when the recorder's optional
layout file is not there (as ``test_win_glue.py`` does).
"""
import contextlib
import io
import os
import shutil
import sys

sys.dont_write_bytecode = True

NO_INPUT_FRAME = 1040  # GFrameCounter of the frame whose keys cannot be read
TORN_SCRIPT_FRAME = 20  # index of the script frame whose tick starts early
MARKS = {10: "scenario A", 30: "zoom hold 1", 50: "wall 45"}  # records written -> name


def main(argv):
    if len(argv) != 2:
        print(__doc__)
        return 2
    recorder_dir, out_dir = argv
    sys.path.insert(0, recorder_dir)
    import asamu_win as win
    import test_win_glue as glue

    if glue.optional_layout() is None:
        print("skipped: no %s next to the layout" % win.OPTIONAL_FILE)
        return 0

    def spec(i):
        f = glue.default_spec(i)
        if 45 <= i < 52:
            f = dict(f, keys=f["keys"] + ("LeftMouseButton",))
        if i == TORN_SCRIPT_FRAME:
            f = dict(f, window=4, d_update=0, d_dispatch=0, d_a=12)
        return f

    with glue.Dirs() as d:
        img = glue.WinImage(optional=True)
        e = glue.Engine(img, spec=spec)
        # The pressed-keys list is unreadable from the tick start of one
        # frame until that frame ends (test_win_glue.test_input_read_failures).
        g, o, m = img.g, img.o, img.mem
        a_count = g.inp + o.pressed_keys + o.arr_count
        glue.hook_after(e, "tick_start", NO_INPUT_FRAME, lambda: m.i(a_count, -3))
        real_end = e.frame_end

        def frame_end(f):
            if e.counter == NO_INPUT_FRAME:
                m.i(a_count, len(e.keys))
            real_end(f)

        e.frame_end = frame_end
        vg = glue.VirtualGame(e)
        sess = glue.make_session(vg, img, d.exe, optional=True)
        os.makedirs(d.ctl, exist_ok=True)
        opts = glue.start_opts("--scenario", "session", "--frames", "70")
        rec = win.Recording(sess, opts, d.out, d.ctl, clock=vg.clock, sleep=vg.sleep)
        rec.open()
        requested = [0]

        def mark(name):
            # What `asamu_win.py mark NAME` does (cmd_mark), then the
            # recorder's own pass over the request files.
            requested[0] += 1
            ident = "fake-%03d" % requested[0]
            path = os.path.join(d.ctl, "%s%s.json" % (win.MARK_PREFIX, ident))
            win.write_json_atomic(path, {"name": win.marker_name(name), "id": ident})
            rec.take_marks()

        real_sink = rec.poller.sink
        done = set()

        def sink(record):
            real_sink(record)
            n = rec.records
            if n in MARKS and n not in done:
                done.add(n)
                mark(MARKS[n])

        rec.poller.sink = sink
        with contextlib.redirect_stdout(io.StringIO()):
            mark("before anything")
            rec.run()
        if rec.failed or rec.path is None:
            print("the fake recording failed: %s" % rec.done)
            return 1
        os.makedirs(out_dir, exist_ok=True)
        shutil.copy(rec.path, os.path.join(out_dir, "session.raw.jsonl"))
        shutil.copy(rec.path[: -len(".raw.jsonl")] + ".stats.json", os.path.join(out_dir, "session.stats.json"))
    print("recorded %d records with %s" % (rec.records, win.RECORDER))
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
