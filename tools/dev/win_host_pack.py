#!/usr/bin/env python3
"""The packing half of `tools/dev/win_host.sh sync`.

    win_host_pack.py check REPO LIST MANIFEST
        LIST is the output of `git ls-files -co --exclude-standard -z` run in
        REPO. Decides which of those files can be mirrored to a Windows
        machine, writes their names to MANIFEST (UTF-8, `/` separators, one per
        line, sorted) and prints "<files> <bytes> <SHA-256 of MANIFEST>". Exit
        code 3, with one line per problem on standard error, when the checkout
        cannot be mirrored as it is: nothing is ever skipped or renamed
        silently. A name is sent in the upper and lower case it has on disk,
        which on a file system that ignores case is not always the one git
        lists (see on_disk).

    win_host_pack.py tar REPO MANIFEST
        Writes a tar stream (pax format) to standard output: the manifest as
        .win_host_incomplete/manifest, then file number i of the manifest as
        .win_host_incomplete/<i as six digits>. The other side gives the files
        their names, because Windows' tar cannot create every name.

It is a separate program, and Python, so that the stream is the same on every
development machine: the tar programs of macOS and Linux differ in options and
in what they add (owner names, extended attributes). Only the standard library
is used.
"""

import hashlib
import io
import os
import stat
import sys
import tarfile
import unicodedata

EXIT_REFUSED = 3

# A folder in the work tree on the other machine while a sync is unfinished.
# The files travel as MARKER/000001, MARKER/000002, ... in the order of the
# manifest, and are given their names there (win_host_remote.ps1 says why).
MARKER = ".win_host_incomplete"

# Characters a Windows file name cannot contain (the path separator `/` aside).
FORBIDDEN = '<>:"|?*\\'

RESERVED = {"con", "prn", "aux", "nul"}
RESERVED.update("com%d" % n for n in range(10))
RESERVED.update("lpt%d" % n for n in range(10))

MAX_PROBLEMS_SHOWN = 20


def say(message):
    sys.stderr.write("win_host: %s\n" % message)


def shown(name):
    """A name for a message: one line, control characters made visible."""
    return ascii(name) if any(ord(c) < 32 or ord(c) == 127 for c in name) else "'%s'" % name


def name_problem(name):
    """Why Windows cannot hold a file of this name, or None."""
    for part in name.split("/"):
        if any(ord(c) < 32 for c in part):
            return "has a control character in its name"
        bad = sorted(set(part) & set(FORBIDDEN))
        if bad:
            return "has %s in its name, which Windows does not allow" % " ".join(bad)
        if part.endswith(" ") or part.endswith("."):
            return "has a folder or file name that ends with a space or a dot, which Windows drops"
        if part.split(".")[0].strip().lower() in RESERVED:
            return "uses a name that Windows reserves for a device (%s)" % part
    if len(name) > 200:
        return "has a path longer than 200 characters (Windows tools stop at 260, the work tree's folder included)"
    return None


def on_disk(repo, name, listings):
    """`name` with every part spelled in the case the file system has it in.

    On a file system that ignores case (the usual one on macOS and Windows) git
    goes on listing a tracked file under its old name after `mv Readme.txt
    README.txt`, and the file opens under either. The mirror must get the name
    that is on disk. Where case matters every part is found as it is and the
    name comes back unchanged; so does a part that differs from the folder's
    entry only in how accented characters are composed (git lists those
    precomposed).
    """
    folder = repo
    real = []
    for part in name.split("/"):
        entries = listings.get(folder)
        if entries is None:
            try:
                entries = set(os.listdir(folder))
            except OSError:
                entries = set()
            listings[folder] = entries
        if part not in entries:
            wanted = unicodedata.normalize("NFC", part)
            composed = {entry: unicodedata.normalize("NFC", entry) for entry in entries}
            if wanted not in composed.values():
                other_case = [entry for entry, text in composed.items() if text.casefold() == wanted.casefold()]
                if len(other_case) == 1:
                    part = other_case[0]
        real.append(part)
        folder = os.path.join(folder, part)
    return "/".join(real)


def case_clashes(names):
    """Pairs of paths (files, or folders on the way to them) that differ only in case.

    Windows file names are not case sensitive: such paths would land on each
    other there. They can only be on disk where case matters.
    """
    seen = {}
    clashes = []
    for name in names:
        parts = name.split("/")
        for depth in range(1, len(parts) + 1):
            prefix = "/".join(parts[:depth])
            other = seen.setdefault(prefix.casefold(), prefix)
            if other != prefix:
                clashes.append((other, prefix))
                seen[prefix.casefold()] = prefix
    return clashes


def collect(repo, list_path):
    """Returns (names, total_bytes, problems, notes)."""
    with open(list_path, "rb") as handle:
        raw = [entry for entry in handle.read().split(b"\0") if entry]
    sizes = {}
    problems = []
    notes = []
    link_dirs = {}
    listings = {}

    def through_link(name):
        """The first folder on the way to `name` that is a symbolic link, or None."""
        parts = name.split("/")[:-1]
        for depth in range(1, len(parts) + 1):
            folder = "/".join(parts[:depth])
            if folder not in link_dirs:
                link_dirs[folder] = os.path.islink(os.path.join(repo, folder))
            if link_dirs[folder]:
                return folder
        return None

    for entry in sorted(set(raw)):
        try:
            name = entry.decode("utf-8")
        except UnicodeDecodeError:
            problems.append("%s: the name is not valid UTF-8" % ascii(entry))
            continue
        if name.endswith("/"):
            # git lists a folder that is a repository of its own as one entry.
            notes.append("left out %s: a repository of its own, not part of this checkout" % shown(name))
            continue
        if name.startswith("/") or ".." in name.split("/") or "" in name.split("/"):
            problems.append("%s: not a path inside the checkout" % shown(name))
            continue
        path = os.path.join(repo, name)
        try:
            info = os.lstat(path)
        except (FileNotFoundError, NotADirectoryError):
            # Tracked, and deleted in the working tree (or a folder on its way
            # was replaced by a file): not there to mirror.
            continue
        except OSError as error:
            problems.append("%s: cannot be read (%s)" % (shown(name), error.strerror))
            continue
        folder = through_link(name)
        if folder is not None:
            problems.append("%s: is reached through the symbolic link %s" % (shown(name), shown(folder)))
            continue
        if stat.S_ISLNK(info.st_mode):
            problems.append(
                "%s: is a symbolic link (not copied: Windows needs a privilege to create one, "
                "and following it could send files from outside the checkout)" % shown(name)
            )
            continue
        if stat.S_ISDIR(info.st_mode):
            if os.path.lexists(os.path.join(path, ".git")):
                notes.append("left out %s: a repository of its own (submodule), not part of this checkout" % shown(name))
            # Otherwise a folder that took the place of a tracked file (git
            # lists the files in it on their own), or a submodule without files.
            continue
        if not stat.S_ISREG(info.st_mode):
            problems.append("%s: is not a regular file" % shown(name))
            continue
        why = name_problem(name)
        if why:
            problems.append("%s: %s" % (shown(name), why))
            continue
        if name.casefold().startswith(MARKER):
            problems.append("%s: names that start with %s are used by win_host.sh itself" % (shown(name), MARKER))
            continue
        real = on_disk(repo, name, listings)
        if real != name:
            notes.append("sending %s under the name it has on disk (git lists it as %s)" % (shown(real), shown(name)))
        sizes[real] = info.st_size

    names = sorted(sizes)
    total = sum(sizes.values())
    for one, other in case_clashes(names):
        problems.append("%s and %s differ only in case, which Windows cannot keep apart" % (shown(one), shown(other)))
    return names, total, sorted(set(problems)), sorted(set(notes))


def check(repo, list_path, manifest_path):
    names, total, problems, notes = collect(repo, list_path)
    for note in notes:
        say(note)
    if problems:
        for problem in problems[:MAX_PROBLEMS_SHOWN]:
            say("cannot mirror %s" % problem)
        if len(problems) > MAX_PROBLEMS_SHOWN:
            say("... and %d more" % (len(problems) - MAX_PROBLEMS_SHOWN))
        say("nothing was sent and nothing was changed on the other machine")
        return EXIT_REFUSED
    if not names:
        say("no files to send from %s: refusing to replace a work tree with an empty one" % repo)
        return EXIT_REFUSED
    names.sort(key=lambda text: text.encode("utf-8"))
    manifest = "".join(name + "\n" for name in names).encode("utf-8")
    with open(manifest_path, "wb") as handle:
        handle.write(manifest)
    sys.stdout.write("%d %d %s\n" % (len(names), total, hashlib.sha256(manifest).hexdigest()))
    return 0


def pack(repo, manifest_path):
    with open(manifest_path, "rb") as handle:
        manifest = handle.read()
    # Only "\n" ends a name (str.splitlines would also split at U+2028 and others).
    names = manifest.decode("utf-8").split("\n")[:-1]
    out = sys.stdout.buffer
    try:
        with tarfile.open(fileobj=out, mode="w|", format=tarfile.PAX_FORMAT) as archive:
            member = tarfile.TarInfo("%s/manifest" % MARKER)
            member.size = len(manifest)
            member.mode = 0o644
            archive.addfile(member, io.BytesIO(manifest))
            for number, name in enumerate(names, 1):
                path = os.path.join(repo, name)
                with open(path, "rb") as source:
                    info = os.fstat(source.fileno())
                    if not stat.S_ISREG(info.st_mode) or os.path.islink(path):
                        say("%s changed while it was being sent" % shown(name))
                        return 1
                    member = tarfile.TarInfo("%s/%06d" % (MARKER, number))
                    member.size = info.st_size
                    member.mode = 0o755 if info.st_mode & 0o111 else 0o644
                    # Whole seconds in the header, the exact time in a pax record:
                    # cargo decides what to rebuild from modification times.
                    member.mtime = info.st_mtime_ns // 1000000000
                    member.pax_headers = {"mtime": "%d.%09d" % divmod(info.st_mtime_ns, 1000000000)}
                    archive.addfile(member, source)
        out.flush()
    except BrokenPipeError:
        # The receiving side went away; win_host.sh reports why. Point standard
        # output elsewhere so that Python's exit does not complain as well.
        os.dup2(os.open(os.devnull, os.O_WRONLY), sys.stdout.fileno())
        return 1
    except OSError as error:
        say("could not read the checkout while sending it: %s" % error)
        return 1
    return 0


def main(argv):
    if len(argv) == 5 and argv[1] == "check":
        return check(argv[2], argv[3], argv[4])
    if len(argv) == 4 and argv[1] == "tar":
        return pack(argv[2], argv[3])
    sys.stderr.write(__doc__)
    return 2


if __name__ == "__main__":
    sys.exit(main(sys.argv))
