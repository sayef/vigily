"""The SDK's tests run against real engines: `target/debug/vigily-engine`, built when missing (CI
builds it first), started in the background per repository by the SDK itself, with a temporary
state directory and ports the system picks. Every engine started here is stopped at the end."""

from __future__ import annotations

import contextlib
import glob
import json
import os
import pathlib
import shutil
import signal
import subprocess
import tempfile
import time

import pytest

ROOT = pathlib.Path(__file__).resolve().parents[2]
ENGINE = pathlib.Path(os.environ.get("VIGILY_TEST_ENGINE") or ROOT / "target" / "debug" / "vigily-engine")

# Before vigily is imported anywhere: nothing here may reach an engine or state of the user's.
STATE = tempfile.mkdtemp(prefix="vigily-tests-")
for name in ("VIGILY_URL", "VIGILY_TOKEN", "VIGILY_REPO", "VIGILY_OFFLINE", "VIGILY_PROJECT"):
    os.environ.pop(name, None)
os.environ["VIGILY_STATE_DIR"] = STATE
os.environ["VIGILY_ENGINE"] = str(ENGINE)
# Engines started here listen on a port the system picks.
os.environ["VIGILY_PORT"] = "0"
os.environ.setdefault("VIGILY_LOG", "warn")


def _build_engine() -> None:
    if ENGINE.is_file():
        return
    cargo = shutil.which("cargo")
    if cargo is None:
        pytest.exit(f"{ENGINE} is missing and cargo is not installed to build it", returncode=2)
    subprocess.run([cargo, "build", "-p", "vigily-server", "--bin", "vigily-engine"], cwd=ROOT, check=True)


def stop_engines(state: str) -> None:
    """Stops every engine whose file is in ``state`` (SIGTERM, then SIGKILL)."""
    pids = []
    for f in glob.glob(os.path.join(state, "engines", "*.json")):
        try:
            with open(f, encoding="utf-8") as fh:
                pids.append(int(json.load(fh)["pid"]))
        except (OSError, ValueError, KeyError):
            continue
    for pid in pids:
        with contextlib.suppress(OSError):
            os.kill(pid, signal.SIGTERM)
    deadline = time.monotonic() + 15
    for pid in pids:
        while time.monotonic() < deadline:
            try:
                os.kill(pid, 0)
            except OSError:
                break
            time.sleep(0.05)
        else:
            with contextlib.suppress(OSError):
                os.kill(pid, signal.SIGKILL)


@pytest.fixture(scope="session", autouse=True)
def engines():
    _build_engine()
    yield STATE
    import vigily

    vigily.flush(30)
    stop_engines(STATE)
    shutil.rmtree(STATE, ignore_errors=True)


def free_port() -> int:
    """A port nothing listens on (never vigily's default, 7600)."""
    import socket

    while True:
        with socket.socket() as s:
            s.bind(("127.0.0.1", 0))
            port = s.getsockname()[1]
        if port != 7600:
            return port


class Engine:
    """An engine run in the foreground on a fixed port, to stop and start at will."""

    def __init__(self, repo: str, state: str, config: str | None = None) -> None:
        self.repo = repo
        self.state = state
        self.config = config
        self.port = free_port()
        self.url = f"http://127.0.0.1:{self.port}"
        self.proc: subprocess.Popen[bytes] | None = None
        self.token: str | None = None

    def start(self) -> Engine:
        cmd = [str(ENGINE), "--repo", self.repo, "--state-dir", self.state, "--port", str(self.port)]
        cmd += ["--no-optimize"]
        if self.config:
            cmd += ["--config", self.config]
        self.proc = subprocess.Popen(cmd, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        from vigily import _engine

        paths = _engine.EnginePaths.new(self.state, _engine.canonical_location(self.repo))
        deadline = time.monotonic() + 60
        while time.monotonic() < deadline:
            f = paths.read()
            if f is not None and f.get("pid") == self.proc.pid and _engine.probe(f["url"]):
                self.token = f.get("token")
                return self
            if self.proc.poll() is not None:
                raise RuntimeError(f"the engine exited with {self.proc.returncode}")
            time.sleep(0.05)
        raise RuntimeError("the engine did not start")

    def kill(self, sig: int = signal.SIGKILL) -> None:
        if self.proc is not None and self.proc.poll() is None:
            self.proc.send_signal(sig)
            self.proc.wait(30)

    def stop(self) -> None:
        self.kill(signal.SIGTERM)


@pytest.fixture
def engine(tmp_path):
    """A foreground engine for a fresh repository, stopped after the test."""
    e = Engine(str(tmp_path / "repo"), str(tmp_path / "state")).start()
    yield e
    e.stop()


@pytest.fixture
def configured(engine):
    """The SDK pointed at ``engine`` with vigily.configure; reset after the test."""
    import vigily
    from vigily import _config

    vigily.configure(url=engine.url, token=engine.token)
    yield engine
    vigily.flush(30)
    _config.reset()
