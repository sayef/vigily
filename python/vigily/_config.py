"""Process-wide settings: where runs and spans go, and how the sender behaves.

Values given to :func:`vigily.configure` win over the environment, which wins over the defaults.
"""

from __future__ import annotations

import os
import threading
from dataclasses import dataclass
from typing import Any

DEFAULT_REPO = ".vigily"


def _env(name: str) -> str | None:
    v = os.environ.get(name)
    return v if v else None


def _env_float(name: str, default: float) -> float:
    v = _env(name)
    if v is None:
        return default
    try:
        return float(v)
    except ValueError:
        return default


def _truthy(v: str | None) -> bool:
    return v is not None and v.strip().lower() in ("1", "true", "yes", "on")


@dataclass
class Settings:
    """What :func:`vigily.configure` set; ``None`` falls back to the environment."""

    url: str | None = None
    token: str | None = None
    project: str | None = None
    offline: bool | None = None


_settings = Settings()
_lock = threading.Lock()


def configure(
    url: str | None = None,
    token: str | None = None,
    project: str | None = None,
    offline: bool | None = None,
) -> None:
    """Sets where runs and spans go. Arguments left out keep their current value.

    ``url`` is an engine's address, such as a team server (``VIGILY_URL``); without one, the
    repository's local engine is found or started. ``token`` is sent with every request
    (``VIGILY_TOKEN``; a local engine's own token otherwise); it needs the ``read`` and ``ingest``
    scopes. ``project`` is the default project of traces, whose payload policy applies (the run's
    experiment otherwise). ``offline=True`` (``VIGILY_OFFLINE=1``) never contacts an engine: every
    batch goes to the spool, for ``vigily sync`` to send later.

    Runs and spans already queued go to the engine they were queued for.
    """
    with _lock:
        if url is not None:
            _settings.url = url.rstrip("/") or None
        if token is not None:
            _settings.token = token
        if project is not None:
            _settings.project = project or None
        if offline is not None:
            _settings.offline = bool(offline)


def reset() -> None:
    """Forgets everything :func:`configure` set (for tests)."""
    global _settings
    with _lock:
        _settings = Settings()


def url() -> str | None:
    u = _settings.url or _env("VIGILY_URL")
    return u.rstrip("/") if u else None


def token() -> str | None:
    """The token given to :func:`configure` (an empty string means none), else ``VIGILY_TOKEN``."""
    if _settings.token is not None:
        return _settings.token or None
    return _env("VIGILY_TOKEN")


def project() -> str | None:
    return _settings.project or _env("VIGILY_PROJECT")


def offline() -> bool:
    if _settings.offline is not None:
        return _settings.offline
    return _truthy(_env("VIGILY_OFFLINE"))


def repo() -> str:
    return _env("VIGILY_REPO") or DEFAULT_REPO


def exit_timeout() -> float:
    """Seconds the exit hook spends sending what is queued (``VIGILY_EXIT_TIMEOUT``, 30)."""
    return max(0.0, _env_float("VIGILY_EXIT_TIMEOUT", 30.0))


def spool_after() -> float:
    """Seconds without reaching the engine before batches go to the spool (``VIGILY_SPOOL_AFTER``,
    120)."""
    return max(0.0, _env_float("VIGILY_SPOOL_AFTER", 120.0))


def spool_max_bytes() -> int:
    """The spool's size limit in bytes (``VIGILY_SPOOL_MAX_MB``, 2048 MiB)."""
    return int(max(1.0, _env_float("VIGILY_SPOOL_MAX_MB", 2048.0)) * 1024 * 1024)


def buffer_max_bytes() -> int:
    """How much the sender holds in memory before ``track()`` waits (``VIGILY_BUFFER_MB``, 64 MiB)."""
    return int(max(0.01, _env_float("VIGILY_BUFFER_MB", 64.0)) * 1024 * 1024)


def snapshot() -> dict[str, Any]:
    return {"url": url(), "token": token(), "project": project(), "offline": offline()}


def _after_fork_in_child() -> None:
    global _lock
    _lock = threading.Lock()


if hasattr(os, "register_at_fork"):
    os.register_at_fork(after_in_child=_after_fork_in_child)
