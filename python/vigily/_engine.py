"""Finding a repository's engine on this machine, and starting one when none runs.

The same rules as the ``vigily`` command line (``vigily_api::local``): the state directory is
``$VIGILY_STATE_DIR``, else ``~/Library/Caches/vigily`` on macOS and ``~/.cache/vigily`` elsewhere;
``engines/<key>.json`` names a running engine's address and token, where the key is the first 16
hex digits of the SHA-256 of the repository's canonical location; the file counts only while the
engine holds ``engines/<key>.lock`` and answers ``GET /api/v1/info`` within 300 ms. Without one,
``vigily-engine --detach`` starts an engine (``$VIGILY_ENGINE``, else the one installed beside
Python by ``pip install 'vigily[local]'``, else on ``PATH``) and the client waits for it.
"""

from __future__ import annotations

import contextlib
import hashlib
import json
import os
import shutil
import subprocess
import sys
import sysconfig
import time
from dataclasses import dataclass
from typing import Any

from vigily._http import probe

#: How long a client waits for an engine it started.
START_TIMEOUT = 30.0

INSTALL_HINT = (
    "vigily-engine was not found beside Python or on PATH; install it with "
    "`pip install 'vigily[local]'`, set VIGILY_ENGINE to its path, or set VIGILY_URL to an engine's address"
)


class EngineNotFound(RuntimeError):
    """No engine runs for the repository and none could be started."""


def state_dir() -> str | None:
    d = os.environ.get("VIGILY_STATE_DIR")
    if d:
        return d
    home = os.environ.get("HOME") or os.environ.get("USERPROFILE")
    if not home:
        return None
    if sys.platform == "darwin":
        return os.path.join(home, "Library", "Caches", "vigily")
    return os.path.join(home, ".cache", "vigily")


def _lexical(path: str) -> str:
    """``path`` without ``.`` and ``..`` components, resolved by name only."""
    parts: list[str] = []
    for p in path.split(os.sep):
        if p in ("", "."):
            continue
        if p == "..":
            if parts:
                parts.pop()
            continue
        parts.append(p)
    return os.sep + os.sep.join(parts)


def canonical_location(repo: str) -> str:
    """A repository's location in one spelling: URIs as given without a trailing ``/``,
    directories as absolute paths with symbolic links resolved. A directory that does not exist
    yet gets the spelling it will have once created."""
    if "://" in repo:
        return repo.rstrip("/")
    absolute = repo if os.path.isabs(repo) else os.path.join(os.getcwd(), repo)
    try:
        return os.path.realpath(absolute, strict=True)
    except OSError:
        pass
    lexical = _lexical(absolute)
    rest: list[str] = []
    base = lexical
    while True:
        try:
            out = os.path.realpath(base, strict=True)
        except OSError:
            parent, name = os.path.split(base)
            if not name or parent == base:
                return lexical
            rest.append(name)
            base = parent
            continue
        for part in reversed(rest):
            out = os.path.join(out, part)
        return out


def repo_key(canonical: str) -> str:
    return hashlib.sha256(canonical.encode()).hexdigest()[:16]


@dataclass(frozen=True)
class EnginePaths:
    dir: str
    json: str
    lock: str
    token: str
    log: str

    @classmethod
    def new(cls, state: str, canonical: str) -> EnginePaths:
        d = os.path.join(state, "engines")
        key = repo_key(canonical)
        return cls(
            d,
            os.path.join(d, f"{key}.json"),
            os.path.join(d, f"{key}.lock"),
            os.path.join(d, f"{key}.token"),
            os.path.join(d, f"{key}.log"),
        )

    def read(self) -> dict[str, Any] | None:
        try:
            with open(self.json, encoding="utf-8") as f:
                doc = json.load(f)
        except (OSError, ValueError):
            return None
        return doc if isinstance(doc, dict) and isinstance(doc.get("url"), str) else None

    def is_locked(self) -> bool:
        return is_locked(self.lock)


def is_locked(path: str) -> bool:
    """Whether another process holds an exclusive lock on ``path``: a shared lock taken and let go
    at once, which an exclusive lock refuses."""
    try:
        import fcntl
    except ImportError:  # pragma: no cover - not on POSIX
        return os.path.exists(path)
    try:
        fd = os.open(path, os.O_RDONLY)
    except OSError:
        return False
    try:
        fcntl.flock(fd, fcntl.LOCK_SH | fcntl.LOCK_NB)
    except BlockingIOError:
        return True
    except OSError:
        return False
    else:
        fcntl.flock(fd, fcntl.LOCK_UN)
        return False
    finally:
        os.close(fd)


def engine_binary() -> str | None:
    """``$VIGILY_ENGINE``, else beside this Python (where ``vigily[local]`` installs it), else
    on ``PATH``."""
    env = os.environ.get("VIGILY_ENGINE")
    if env:
        return env
    name = "vigily-engine.exe" if os.name == "nt" else "vigily-engine"
    for d in (sysconfig.get_path("scripts"), os.path.dirname(sys.executable)):
        if d:
            p = os.path.join(d, name)
            if os.path.isfile(p) and os.access(p, os.X_OK):
                return p
    return shutil.which(name)


@dataclass(frozen=True)
class Found:
    """An engine: its address, the token to send, and how it was found (``url``, ``running`` or
    ``started``)."""

    url: str
    token: str | None
    how: str
    repo: str | None = None


def running(paths: EnginePaths) -> dict[str, Any] | None:
    f = paths.read()
    if f is None or not paths.is_locked():
        return None
    if probe(f["url"]) is None:
        return None
    return f


def resolve(repo: str, token: str | None = None, start: bool = True) -> Found:
    """The engine of ``repo``: the running local one, else (with ``start``) one started with
    ``--detach`` and waited for. Processes racing to start one end up with the same engine."""
    d = state_dir()
    if d is None:
        raise EngineNotFound("no home directory for vigily's state; set VIGILY_STATE_DIR")
    canonical = canonical_location(repo)
    paths = EnginePaths.new(d, canonical)
    f = running(paths)
    if f is not None:
        return Found(f["url"], token or f.get("token"), "running", canonical)
    if not start:
        raise EngineNotFound(f"no engine serves {repo}; start one with `vigily up`, or set VIGILY_URL")
    binary = engine_binary()
    if binary is None:
        raise EngineNotFound(INSTALL_HINT)
    cmd = [binary, "--repo", canonical, "--idle-timeout", "30m", "--detach", "--state-dir", d]
    try:
        child = subprocess.Popen(
            cmd,
            stdin=subprocess.DEVNULL,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.PIPE,
            start_new_session=True,
        )
    except OSError as e:
        raise EngineNotFound(f"could not start {binary}: {e}") from None
    deadline = time.monotonic() + START_TIMEOUT
    exited: int | None = None
    try:
        while True:
            f = running(paths)
            if f is not None:
                return Found(f["url"], token or f.get("token"), "started", canonical)
            if exited is None:
                exited = child.poll()
                # 3: another engine serves the repository (a racing client started it); keep
                # waiting for its file.
                if exited is not None and exited not in (0, 3):
                    err = child.stderr.read().decode(errors="replace").strip() if child.stderr else ""
                    raise EngineNotFound(
                        f"the engine for {repo} did not start (exit code {exited}): {err}; see {paths.log}"
                    )
            if time.monotonic() >= deadline:
                raise EngineNotFound(
                    f"the engine for {repo} did not answer within {int(START_TIMEOUT)} seconds; "
                    f"see {paths.log}"
                )
            time.sleep(0.1)
    finally:
        if child.poll() is None:
            with contextlib.suppress(subprocess.TimeoutExpired):
                child.wait(timeout=2)
        if child.stderr is not None:
            child.stderr.close()
