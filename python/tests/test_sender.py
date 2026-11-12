"""The sender: batches, the spool, replays after an engine restart, forks, backpressure, privacy
before anything leaves the process, and the exit flush."""

import glob
import gzip
import json
import os
import pathlib
import shutil
import signal
import stat
import subprocess
import sys
import textwrap
import time

import pytest
import vigily
from conftest import ENGINE, ROOT, STATE, Engine, free_port
from vigily import Repo, Run, _config, _sender, tracing

FIXTURES = ROOT / "crates" / "vigily-api" / "tests" / "fixtures"


def spool_files(writer: str = "*") -> list[str]:
    return sorted(glob.glob(os.path.join(STATE, "spool", writer, "*.ndjson.gz")))


def read_lines(path: str) -> list[bytes]:
    with gzip.open(path) as f:
        return f.read().splitlines()


def wait_for(cond, timeout: float = 90.0, what: str = "condition") -> None:
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if cond():
            return
        time.sleep(0.1)
    raise AssertionError(f"timed out waiting for {what}")


def run_script(code: str, env: dict | None = None, timeout: float = 120) -> subprocess.CompletedProcess:
    return subprocess.run(
        [sys.executable, "-c", textwrap.dedent(code)],
        capture_output=True,
        text=True,
        timeout=timeout,
        env={**os.environ, **(env or {})},
    )


def test_the_batch_format_matches_the_engines_fixture():
    """The ingest batch the sender writes is the one the engine's golden file pins."""
    golden = json.loads((FIXTURES / "ingest_batch.json").read_text())
    s = _sender.Sender(_sender.Connection("http://127.0.0.1:9", None, None))
    s._pid = -1  # no thread: nothing is sent
    s.writer = golden["writer"]
    for op in golden["ops"]:
        if op["op"] == "points":
            ctx = op["context"]
            for step, v, ts in zip(op["steps"], op["values"], op["timestamps"], strict=True):
                s.track(op["run_id"], op["name"], json.dumps(ctx, sort_keys=True), ctx, v, step, None, ts)
        else:
            s.add(op.get("run_id") or op.get("id", ""), op)
    with s._cond:
        s._seal_locked()
        batch = s._sealed[0]
    body, _ = s._encode(batch)
    got = json.loads(body)
    # Optional columns the sender leaves out when empty.
    for op in golden["ops"]:
        if op["op"] == "points":
            op.pop("key")
            if not op["epochs"]:
                op.pop("epochs")
    assert got == golden


def test_an_engine_killed_mid_run_gets_the_spooled_points_once(engine, monkeypatch):
    monkeypatch.setenv("VIGILY_SPOOL_AFTER", "1")
    vigily.configure(url=engine.url, token=engine.token)
    try:
        run = Run(experiment="restart", capture_system=False)
        for s in range(1000):
            run.track(float(s), name="loss")
        run.flush()
        engine.kill(signal.SIGKILL)
        for s in range(1000, 2000):
            run.track(float(s), name="loss")
        assert vigily.flush(60), "a flush returns once the batches are in the spool"
        writer = run._h.sender().writer
        assert spool_files(writer), "nothing was spooled"
        for f in spool_files(writer):
            assert stat.S_IMODE(os.stat(f).st_mode) == 0o600
            assert stat.S_IMODE(os.stat(os.path.dirname(f)).st_mode) == 0o700
        engine.start()
        run.track(2000.0, name="loss")
        wait_for(lambda: not spool_files(writer), what="the spool to be sent")
        run.finish()
        m = Repo(url=engine.url).metric(run.id, "loss")
        assert m["steps"] == list(range(2001))
        assert m["values"] == [float(s) for s in range(2001)]
        assert Repo(url=engine.url).runs()[0]["status"] == "finished"
    finally:
        _config.reset()


def test_a_forked_child_logs_to_the_parents_run(tmp_path):
    repo = str(tmp_path / "repo")
    out = run_script(
        f"""
        import multiprocessing, os, sys
        from vigily import Run
        run = Run(repo={repo!r}, experiment="fork", capture_system=False)
        for s in range(100):
            run.track(float(s), name="parent")

        def child(series):
            for s in range(250):
                run.track(float(s), name=series)

        pid = os.fork()
        if pid == 0:
            child("forked")
            sys.exit(0)  # the exit hook sends what the child queued
        os.waitpid(pid, 0)
        # multiprocessing ends its children with os._exit: its finalizers flush.
        p = multiprocessing.get_context("fork").Process(target=child, args=("worker",))
        p.start()
        p.join()
        print(run.id, p.exitcode, flush=True)
        """
    )
    assert out.returncode == 0, out.stderr
    run_id, code = out.stdout.split()
    assert code == "0"
    r = Repo(repo)
    assert [x["status"] for x in r.runs()] == ["finished"], "the parent finished it; the children did not"
    assert r.metric(run_id, "parent")["steps"] == list(range(100))
    assert r.metric(run_id, "forked")["steps"] == list(range(250))
    assert r.metric(run_id, "worker")["steps"] == list(range(250))


def test_backpressure_blocks_and_drops_nothing(tmp_path):
    repo = str(tmp_path / "repo")
    Repo(repo).runs()  # starts the repository's engine
    from vigily import _engine

    engine_file = _engine.EnginePaths.new(STATE, _engine.canonical_location(repo)).read()
    p = subprocess.Popen(
        [
            sys.executable,
            "-c",
            textwrap.dedent(
                f"""
                import time
                from vigily import Run
                run = Run(repo={repo!r}, capture_system=False)
                n, slowest, began = 0, 0.0, time.monotonic()
                print("started", flush=True)
                while time.monotonic() - began < 6:
                    t = time.perf_counter()
                    run.track(float(n), name="loss")
                    slowest = max(slowest, time.perf_counter() - t)
                    n += 1
                run.finish()
                print(run.id, n, slowest, flush=True)
                """
            ),
        ],
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
        env={**os.environ, "VIGILY_BUFFER_MB": "0.05"},
    )
    assert p.stdout.readline().strip() == "started"
    time.sleep(1.5)
    os.kill(engine_file["pid"], signal.SIGSTOP)
    try:
        time.sleep(2.5)
    finally:
        os.kill(engine_file["pid"], signal.SIGCONT)
    out, err = p.communicate(timeout=120)
    assert p.returncode == 0, err
    run_id, n, slowest = out.split()
    assert float(slowest) > 1.0, "track() did not wait while the engine was stopped"
    steps = Repo(repo).metric(run_id, "loss")["steps"]
    assert len(steps) == int(n) and steps == list(range(int(n)))
    assert "dropped" not in err


def test_redaction_and_the_payload_policy_apply_before_the_spool(tmp_path, monkeypatch):
    config = tmp_path / "vigily.toml"
    config.write_text('[tracing]\npayloads = "metadata"\n')
    e = Engine(str(tmp_path / "repo"), str(tmp_path / "state"), str(config)).start()
    monkeypatch.setenv("VIGILY_SPOOL_AFTER", "0")
    before = set(spool_files())
    try:
        tracing.configure(e.url, token=e.token, enabled=True, flush_interval=0.05)

        def redact(span):
            span["attributes"]["account"] = "[redacted]"
            return span

        tracing.set_redactor(redact)
        with vigily.trace("online"):
            pass
        assert tracing.flush()  # also fetches the engine's policy: metadata only
        e.kill(signal.SIGKILL)
        with vigily.trace("offline") as sp:
            sp.set_input("Question from user@example.com")
            sp.set_output("Answer for Customer A")
            sp.set_attribute("account", "Customer A")
            sp.set_attribute("retries", 2)
            sp.record_error("LookupError: no account for user@example.com")
        assert tracing.flush(60)
        new = [f for f in spool_files() if f not in before]
        assert new, "the span was not spooled"
        spooled = [json.loads(line) for f in new for line in read_lines(f)]
        (span,) = [s for doc in spooled for s in doc["spans"] if s["name"] == "offline"]
        assert "input" not in span and "output" not in span
        assert span["error"] == "LookupError"
        assert span["attributes"] == {"retries": 2}
        raw = b"".join(line for f in new for line in read_lines(f))
        assert b"user@example.com" not in raw and b"Customer A" not in raw
        for f in new:
            assert stat.S_IMODE(os.stat(f).st_mode) == 0o600
    finally:
        tracing.set_redactor(None)
        tracing.flush()
        tracing._settings.update(url=None, token=None)
        tracing._target = None
        e.stop()
        for d in {os.path.dirname(f) for f in spool_files() if f not in before}:
            shutil.rmtree(d, ignore_errors=True)


def test_the_exit_hook_finishes_and_sends_everything(tmp_path):
    repo = str(tmp_path / "repo")
    out = run_script(
        f"""
        from vigily import Run
        run = Run(repo={repo!r}, capture_system=False)
        for s in range(50_000):
            run.track(s * 0.5, name="loss", context={{"subset": "train"}})
        print(run.id)
        """
    )
    assert out.returncode == 0, out.stderr
    run_id = out.stdout.strip()
    r = Repo(repo)
    assert {x["id"]: x["status"] for x in r.runs()}[run_id] == "finished"
    assert len(r.metric(run_id, "loss", {"subset": "train"})["steps"]) == 50_000


def test_an_unreachable_engine_at_exit_spools_within_the_timeout(tmp_path):
    port = free_port()
    began = time.monotonic()
    out = run_script(
        """
        from vigily import Run
        run = Run(capture_system=False)
        run.track(1.0, name="loss")
        print(run.id)
        """,
        env={"VIGILY_URL": f"http://127.0.0.1:{port}", "VIGILY_EXIT_TIMEOUT": "3"},
    )
    assert out.returncode == 0, out.stderr
    assert time.monotonic() - began < 20
    assert "in the spool at" in out.stderr and "vigily sync" in out.stderr
    run_id = out.stdout.strip()
    spooled = [
        f
        for f in spool_files()
        if json.loads((pathlib.Path(f).parent / "meta.json").read_text())["url"] == f"http://127.0.0.1:{port}"
    ]
    assert spooled
    ops = [op for f in spooled for line in read_lines(f) for op in json.loads(line)["ops"]]
    assert [op["op"] for op in ops] == ["create", "points", "finish"]
    assert {op.get("run_id", op.get("id")) for op in ops} == {run_id}

    # `vigily sync` sends them once an engine answers there.
    cli = ENGINE.parent / "vigily"
    if not cli.is_file():
        pytest.skip(f"{cli} is not built")
    e = Engine(str(tmp_path / "repo"), str(tmp_path / "state"))
    e.port, e.url = port, f"http://127.0.0.1:{port}"
    e.start()
    try:
        sync = subprocess.run(
            [str(cli), "sync", "--url", e.url, "--token", e.token or ""],
            capture_output=True,
            text=True,
            timeout=120,
        )
        assert sync.returncode == 0, sync.stderr
        assert not [f for f in spooled if os.path.exists(f)]
        vigily.configure(url=e.url, token=e.token)
        try:
            r = Repo()
            assert {x["id"]: x["status"] for x in r.runs()}[run_id] == "finished"
            assert r.metric(run_id, "loss")["values"] == [1.0]
        finally:
            _config.reset()
    finally:
        e.stop()


def test_offline_mode_spools_without_contacting_an_engine(tmp_path):
    out = run_script(
        f"""
        import vigily
        vigily.configure(offline=True)
        run = vigily.Run(repo={str(tmp_path / "repo")!r}, capture_system=False)
        run.track(1.0, name="loss")
        print(run.id)
        """
    )
    assert out.returncode == 0, out.stderr
    assert "in the spool at" in out.stderr
    assert not os.path.exists(tmp_path / "repo"), "no engine was started"
