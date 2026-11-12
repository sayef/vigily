"""Identifiers chosen by the client: run ids and writer ids."""

from __future__ import annotations

import os
import threading
import time

_lock = threading.Lock()
_last_ms = 0
_counter = 0


def uuid7_hex() -> str:
    """A UUIDv7 as 32 lower-case hex digits, without dashes: a millisecond timestamp, then random
    bits. Ids made in one process within the same millisecond still sort in creation order (a
    12-bit counter in ``rand_a``, RFC 9562 method 1)."""
    global _last_ms, _counter
    with _lock:
        ms = time.time_ns() // 1_000_000
        if ms > _last_ms:
            _last_ms = ms
            _counter = int.from_bytes(os.urandom(2), "big") & 0x7FF
        else:
            _counter += 1
            if _counter > 0xFFF:
                # More than 2048 ids in one millisecond: borrow the next millisecond.
                _last_ms += 1
                _counter = 0
            ms = _last_ms
        counter = _counter
    rand_b = int.from_bytes(os.urandom(8), "big") & ((1 << 62) - 1)
    value = (ms & ((1 << 48) - 1)) << 80 | 0x7 << 76 | counter << 64 | 0b10 << 62 | rand_b
    return f"{value:032x}"


def _after_fork_in_child() -> None:
    global _lock
    _lock = threading.Lock()


if hasattr(os, "register_at_fork"):
    os.register_at_fork(after_in_child=_after_fork_in_child)
