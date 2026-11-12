"""HTTP to an engine with the standard library: kept-alive connections, JSON, gzip, and the
engine's structured errors mapped to Python exceptions."""

from __future__ import annotations

import contextlib
import gzip
import http.client
import json
import random
import ssl
import threading
import time
import urllib.parse
from collections.abc import Mapping
from typing import Any

USER_AGENT = "vigily-python"


class RunDeletedError(LookupError):
    """The run was deleted (in the web app or by another process); nothing more is recorded for it."""


class EngineUnreachable(ConnectionError):
    """No engine answered at the address."""


class ApiError(OSError):
    """An error answer from the engine: ``status``, ``code`` and ``message``."""

    def __init__(self, status: int, code: str | None, message: str, retry_after: float | None = None):
        super().__init__(message)
        self.status = status
        self.code = code
        self.message = message
        self.retry_after = retry_after

    @property
    def retryable(self) -> bool:
        return self.status in (408, 425, 429, 502, 503, 504) or self.code == "overloaded"


def to_exception(e: ApiError) -> Exception:
    """The Python exception for an engine error, as the SDK raises it."""
    if e.code == "run_deleted" or e.status == 410:
        return RunDeletedError(e.message)
    if e.status == 404:
        return FileNotFoundError(e.message)
    if e.status in (400, 409, 413, 422):
        return ValueError(e.message)
    if e.status in (401, 403):
        hint = " (set VIGILY_TOKEN to a token with the read and ingest scopes)" if e.status == 401 else ""
        return PermissionError(e.message + hint)
    return e


def retry_after(value: str | None) -> float | None:
    """A ``Retry-After`` header in seconds (a number of seconds; dates are not used by engines)."""
    if not value:
        return None
    try:
        return max(0.0, float(value.strip()))
    except ValueError:
        return None


def backoff(attempt: int, base: float = 0.5, cap: float = 30.0) -> float:
    """Exponential backoff with full jitter: a random wait up to ``base * 2**attempt``, capped."""
    return random.uniform(0, min(cap, base * (2 ** min(attempt, 16))))


class Http:
    """Requests to one engine. Each thread keeps its own connection alive between requests."""

    def __init__(self, base: str, token: str | None, timeout: float = 30.0) -> None:
        u = urllib.parse.urlsplit(base.rstrip("/"))
        if u.scheme not in ("http", "https") or not u.hostname:
            raise ValueError(f"not an engine address: {base!r} (use http://host:port)")
        self.base = base.rstrip("/")
        self.scheme = u.scheme
        self.host = u.hostname
        self.port = u.port or (443 if u.scheme == "https" else 80)
        self.prefix = u.path.rstrip("/")
        self.token = token
        self.timeout = timeout
        self._local = threading.local()

    def __repr__(self) -> str:
        return f"Http({self.base!r})"

    def _conn(self, timeout: float) -> http.client.HTTPConnection:
        c: http.client.HTTPConnection | None = getattr(self._local, "conn", None)
        if c is None:
            if self.scheme == "https":
                c = http.client.HTTPSConnection(
                    self.host, self.port, timeout=timeout, context=ssl.create_default_context()
                )
            else:
                c = http.client.HTTPConnection(self.host, self.port, timeout=timeout)
            self._local.conn = c
        c.timeout = timeout
        if c.sock is not None:
            c.sock.settimeout(timeout)
        return c

    def _drop(self) -> None:
        c = getattr(self._local, "conn", None)
        self._local.conn = None
        if c is not None:
            with contextlib.suppress(OSError):
                c.close()

    def request(
        self,
        method: str,
        path: str,
        *,
        body: bytes | None = None,
        content_type: str | None = None,
        gzipped: bool = False,
        headers: Mapping[str, str] | None = None,
        query: Mapping[str, Any] | None = None,
        timeout: float | None = None,
    ) -> tuple[int, dict[str, str], bytes]:
        """One request; answers (status, headers, body). Raises :class:`EngineUnreachable` when the
        engine cannot be reached. A kept-alive connection the engine closed is reopened once."""
        url = self.prefix + path
        if query:
            q = {k: v for k, v in query.items() if v is not None}
            if q:
                url += "?" + urllib.parse.urlencode(q)
        h = {"user-agent": USER_AGENT, "accept-encoding": "gzip"}
        if self.token:
            h["authorization"] = f"Bearer {self.token}"
        if content_type:
            h["content-type"] = content_type
        if gzipped:
            h["content-encoding"] = "gzip"
        if headers:
            h.update(headers)
        t = self.timeout if timeout is None else timeout
        for attempt in range(2):
            c = self._conn(t)
            try:
                c.request(method, url, body=body if body is not None else b"", headers=h)
                r = c.getresponse()
                data = r.read()
            except (http.client.RemoteDisconnected, BrokenPipeError, ConnectionResetError) as e:
                self._drop()
                if attempt == 0:
                    continue
                raise EngineUnreachable(f"{self.base}: {e}") from None
            except (OSError, http.client.HTTPException) as e:
                self._drop()
                raise EngineUnreachable(f"{self.base}: {e}") from None
            rh = {k.lower(): v for k, v in r.getheaders()}
            if r.will_close:
                self._drop()
            if rh.get("content-encoding", "").lower() == "gzip":
                data = gzip.decompress(data)
            return r.status, rh, data
        raise EngineUnreachable(self.base)  # pragma: no cover

    def call(
        self,
        method: str,
        path: str,
        payload: Any = None,
        *,
        query: Mapping[str, Any] | None = None,
        headers: Mapping[str, str] | None = None,
        timeout: float | None = None,
        retries: int = 4,
        raw: bool = False,
    ) -> Any:
        """A JSON request with retries for transient errors (unreachable, 429, 503); answers the
        decoded JSON (or bytes with ``raw``), raising :class:`ApiError` for error answers."""
        body = None
        ct = None
        if payload is not None:
            body = json.dumps(payload, separators=(",", ":"), default=str).encode()
            ct = "application/json"
        elif method != "GET":
            ct = "application/json"
        attempt = 0
        while True:
            try:
                status, rh, data = self.request(
                    method, path, body=body, content_type=ct, query=query, headers=headers, timeout=timeout
                )
            except EngineUnreachable:
                if attempt >= retries:
                    raise
                time.sleep(backoff(attempt, 0.2, 2.0))
                attempt += 1
                continue
            if status < 400:
                if raw:
                    return data
                return json.loads(data) if data else None
            err = error_from(status, rh, data)
            if err.retryable and attempt < retries:
                time.sleep(err.retry_after if err.retry_after is not None else backoff(attempt, 0.2, 5.0))
                attempt += 1
                continue
            raise err


def error_from(status: int, headers: Mapping[str, str], data: bytes) -> ApiError:
    """The engine's error answer: ``{"error": {"code", "message"}}`` (or a plain body)."""
    code = None
    message = ""
    try:
        doc = json.loads(data)
        e = doc.get("error") if isinstance(doc, dict) else None
        if isinstance(e, dict):
            code = e.get("code")
            message = str(e.get("message") or "")
        elif isinstance(e, str):
            message = e
    except (ValueError, UnicodeDecodeError):
        pass
    if not message:
        message = f"HTTP {status} {http.client.responses.get(status, '')}".strip()
    return ApiError(status, code, message, retry_after(headers.get("retry-after")))


def probe(base: str, timeout: float = 0.3) -> dict[str, Any] | None:
    """``GET /api/v1/info`` within ``timeout``: the engine's info, or None when nothing answers."""
    try:
        h = Http(base, None, timeout)
        status, _, data = h.request("GET", "/api/v1/info", timeout=timeout)
        h._drop()
    except (TimeoutError, EngineUnreachable, ValueError):
        return None
    if status != 200:
        return None
    try:
        info = json.loads(data)
    except ValueError:
        return None
    return info if isinstance(info, dict) else None
