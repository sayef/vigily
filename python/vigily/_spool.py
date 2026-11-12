"""The offline spool: batches that could not be sent yet, on disk until an engine takes them.

Layout, shared with ``vigily sync`` (crates/vigily-cli/src/sync.rs)::

    <state dir>/spool/<writer>/meta.json           where the batches go (no token)
    <state dir>/spool/<writer>/lock                 locked by the writing process while it lives
    <state dir>/spool/<writer>/<n>-b.ndjson.gz      ingest batches, one JSON object per line
    <state dir>/spool/<writer>/<n>-s.ndjson.gz      spans, one {"spans": [...]} per line

``<n>`` is a 12-digit counter: files are replayed in name order, which is the order they were
written. Every file is written to a temporary name, synced and renamed, so a reader never sees half
a file; files and directories are private to the user (0600 and 0700). A file is removed only once
the engine has accepted all of it.
"""

from __future__ import annotations

import contextlib
import gzip
import json
import os
import re
import time
from collections.abc import Iterator
from typing import Any

from vigily._engine import is_locked, state_dir

SPOOL_VERSION = 1
_FILE = re.compile(r"^(\d{12})-([bs])\.ndjson\.gz$")


def root() -> str | None:
    d = state_dir()
    return os.path.join(d, "spool") if d else None


def _private_dir(path: str) -> None:
    os.makedirs(path, mode=0o700, exist_ok=True)
    with contextlib.suppress(OSError):
        os.chmod(path, 0o700)


def total_bytes(base: str | None = None) -> int:
    """The size of every spooled file."""
    base = base or root()
    if not base or not os.path.isdir(base):
        return 0
    n = 0
    for w in os.listdir(base):
        d = os.path.join(base, w)
        try:
            for f in os.listdir(d):
                if _FILE.match(f):
                    n += os.path.getsize(os.path.join(d, f))
        except OSError:
            continue
    return n


class Spool:
    """One writer's spool directory, locked while this process lives."""

    def __init__(self, writer: str, target: dict[str, Any]) -> None:
        base = root()
        if base is None:
            raise OSError("no home directory for vigily's spool; set VIGILY_STATE_DIR")
        self.dir = os.path.join(base, writer)
        self.writer = writer
        self.target = target
        self._lock_fd: int | None = None
        self._next = 0

    def open(self) -> None:
        """Creates the directory, takes its lock and writes ``meta.json``."""
        if self._lock_fd is not None:
            return
        _private_dir(os.path.dirname(self.dir))
        _private_dir(self.dir)
        self._lock_fd = lock(self.dir, wait=True)
        meta = {"version": SPOOL_VERSION, "writer": self.writer, "pid": os.getpid(), **self.target}
        _write_atomic(os.path.join(self.dir, "meta.json"), json.dumps(meta).encode())
        self._next = max((n for n, _, _ in files(self.dir)), default=0) + 1

    def close(self) -> None:
        """Lets go of the lock; removes the directory when nothing is left in it."""
        if self._lock_fd is None:
            return
        if not any(True for _ in files(self.dir)):
            remove_dir(self.dir)
        os.close(self._lock_fd)
        self._lock_fd = None

    def forget_lock(self) -> None:
        """In a forked child: close the inherited lock (the parent still holds it)."""
        if self._lock_fd is not None:
            with contextlib.suppress(OSError):
                os.close(self._lock_fd)
            self._lock_fd = None

    def write(self, kind: str, lines: list[bytes]) -> int:
        """Writes one file of ``kind`` (``b`` batches, ``s`` spans); answers its size."""
        self.open()
        data = gzip.compress(b"".join(line + b"\n" for line in lines), compresslevel=6)
        name = f"{self._next:012d}-{kind}.ndjson.gz"
        self._next += 1
        _write_atomic(os.path.join(self.dir, name), data)
        return len(data)

    def pending(self) -> list[tuple[int, str, str]]:
        return list(files(self.dir)) if os.path.isdir(self.dir) else []


def lock(directory: str, wait: bool) -> int | None:
    """Takes ``<directory>/lock`` exclusively; None when another process holds it (``wait=False``)."""
    path = os.path.join(directory, "lock")
    fd = os.open(path, os.O_RDWR | os.O_CREAT, 0o600)
    try:
        import fcntl

        fcntl.flock(fd, fcntl.LOCK_EX | (0 if wait else fcntl.LOCK_NB))
    except BlockingIOError:
        os.close(fd)
        return None
    except ImportError:  # pragma: no cover - not on POSIX
        pass
    return fd


def files(directory: str) -> Iterator[tuple[int, str, str]]:
    """``(number, kind, path)`` of each spooled file, in order."""
    try:
        names = sorted(os.listdir(directory))
    except OSError:
        return
    for name in names:
        m = _FILE.match(name)
        if m:
            yield int(m.group(1)), m.group(2), os.path.join(directory, name)


def read(path: str) -> list[bytes]:
    with gzip.open(path, "rb") as f:
        return [line for line in f.read().split(b"\n") if line.strip()]


def remove_dir(directory: str) -> None:
    for name in ("meta.json", "lock"):
        with contextlib.suppress(OSError):
            os.unlink(os.path.join(directory, name))
    with contextlib.suppress(OSError):
        os.rmdir(directory)


def orphans(target: dict[str, Any]) -> Iterator[str]:
    """Spool directories for ``target`` whose writer no longer runs."""
    base = root()
    if not base or not os.path.isdir(base):
        return
    for w in sorted(os.listdir(base)):
        d = os.path.join(base, w)
        try:
            with open(os.path.join(d, "meta.json"), encoding="utf-8") as f:
                meta = json.load(f)
        except (OSError, ValueError):
            continue
        if meta.get("url") != target.get("url") or meta.get("repo") != target.get("repo"):
            continue
        if is_locked(os.path.join(d, "lock")):
            continue
        yield d


def _write_atomic(path: str, data: bytes) -> None:
    tmp = f"{path}.tmp-{os.getpid()}-{time.monotonic_ns()}"
    fd = os.open(tmp, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    try:
        with os.fdopen(fd, "wb") as f:
            f.write(data)
            f.flush()
            os.fsync(f.fileno())
        os.replace(tmp, path)
    except BaseException:
        with contextlib.suppress(OSError):
            os.unlink(tmp)
        raise
