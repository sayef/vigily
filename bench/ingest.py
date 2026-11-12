"""Measure ingest through the Python SDK into a running engine.

N processes each log R runs of P points with ``Run.track`` and then wait for ``vigily.flush()``.
Reports the latency of ``track()`` calls (p50, p99), each process's rate while tracking, the
engine-wide rate until every flush returned, the engine's CPU time and memory, and how long a
point takes to become readable over the API (visibility lag).

    cargo build --release -p vigily-server
    uv run --no-sync python bench/ingest.py --procs 8 --runs 4 --points 100000

Without ``--url`` it starts ``vigily-engine`` (``--engine``, default target/release/vigily-engine)
on a free port with a temporary repository and state directory, and stops it afterwards. With
``--url`` and ``--token`` (``VIGILY_URL``, ``VIGILY_TOKEN``; read and ingest scopes) it uses that
engine; ``--engine-pid`` then names its process for the CPU and memory figures (Linux, or ``ps``).
"""

from __future__ import annotations

import argparse
import json
import multiprocessing as mp
import os
import shutil
import signal
import socket
import subprocess
import sys
import tempfile
import time
import urllib.request
from pathlib import Path

TARGETS = {
    "track_p50_us": 1.5,
    "per_process_points_s": 300_000,
    "engine_points_s_8_procs": 1_000_000,
    "visibility_s": 3.0,
}


def _free_port() -> int:
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


def _get(url: str, token: str, path: str) -> dict:
    req = urllib.request.Request(url + path, headers={"Authorization": f"Bearer {token}"})
    with urllib.request.urlopen(req, timeout=10) as r:
        return json.load(r)


class Engine:
    """A vigily-engine started for the benchmark, or the process of one given by pid."""

    def __init__(self, pid: int | None, proc: subprocess.Popen | None = None) -> None:
        self.pid = pid
        self.proc = proc

    @classmethod
    def start(cls, binary: str, tmp: Path) -> tuple[Engine, str, str]:
        port = _free_port()
        token_file = tmp / "token"
        log = (tmp / "engine.log").open("wb")
        proc = subprocess.Popen(
            [
                binary,
                "--repo",
                str(tmp / "repo"),
                "--port",
                str(port),
                "--mode",
                "local",
                "--state-dir",
                str(tmp / "state"),
                "--token-file",
                str(token_file),
            ],
            stdout=log,
            stderr=subprocess.STDOUT,
        )
        url = f"http://127.0.0.1:{port}"
        deadline = time.monotonic() + 60
        while True:
            if proc.poll() is not None:
                sys.exit(f"vigily-engine exited with {proc.returncode}; see {tmp / 'engine.log'}")
            try:
                with urllib.request.urlopen(url + "/api/v1/health", timeout=1):
                    break
            except OSError:
                if time.monotonic() > deadline:
                    proc.kill()
                    sys.exit("vigily-engine did not answer within 60 s")
                time.sleep(0.1)
        return cls(proc.pid, proc), url, token_file.read_text().strip()

    def stop(self) -> None:
        if self.proc is not None:
            self.proc.send_signal(signal.SIGTERM)
            try:
                self.proc.wait(timeout=60)
            except subprocess.TimeoutExpired:
                self.proc.kill()

    def cpu_seconds(self) -> float | None:
        """User plus system CPU time of the engine so far (Linux only)."""
        if self.pid is None:
            return None
        try:
            fields = Path(f"/proc/{self.pid}/stat").read_text().rsplit(")", 1)[1].split()
        except OSError:
            return None
        return (int(fields[11]) + int(fields[12])) / os.sysconf("SC_CLK_TCK")

    def memory_mb(self) -> tuple[float | None, float | None]:
        """Current and peak resident memory in MB (the peak on Linux only)."""
        if self.pid is None:
            return None, None
        try:
            status = Path(f"/proc/{self.pid}/status").read_text().splitlines()
            pairs = (ln.split(":", 1) for ln in status)
            kb = {k: int(v.split()[0]) for k, v in pairs if k in ("VmRSS", "VmHWM")}
            return kb["VmRSS"] / 1024, kb["VmHWM"] / 1024
        except (OSError, KeyError):
            out = subprocess.run(["ps", "-o", "rss=", "-p", str(self.pid)], capture_output=True, text=True)
            return (int(out.stdout) / 1024 if out.stdout.strip() else None), None


def _worker(url: str, token: str, runs: int, points: int, sample: int, ready, go, results) -> None:
    import vigily

    vigily.configure(url=url, token=token)
    clock = time.perf_counter_ns
    lat: list[int] = []
    ready.wait()
    go.wait()
    began = time.perf_counter()
    tracking = 0.0
    for r in range(runs):
        run = vigily.Run(experiment="bench-ingest", name=f"p{os.getpid()}-r{r}", capture_system=False)
        track = run.track
        t0 = time.perf_counter()
        for s in range(points):
            if s % sample == 0:
                a = clock()
                track(1.0 / (s + 1), "loss")
                lat.append(clock() - a)
            else:
                track(1.0 / (s + 1), "loss")
        tracking += time.perf_counter() - t0
        run.finish()
    flushed = vigily.flush(timeout=600)
    ended = time.perf_counter()
    results.put(
        {
            "began": began,
            "ended": ended,
            "tracking": tracking,
            "points": runs * points,
            "flushed": flushed,
            "lat": lat,
        }
    )


def _pct(sorted_ns: list[int], q: float) -> float:
    return sorted_ns[min(len(sorted_ns) - 1, int(q * len(sorted_ns)))] / 1000


def visibility(url: str, token: str, points: int) -> float:
    """Seconds from the last track() of a run (no explicit flush) until all its points read back."""
    import vigily

    vigily.configure(url=url, token=token)
    run = vigily.Run(experiment="bench-visibility", capture_system=False)
    for s in range(points):
        run.track(float(s), "loss")
    t0 = time.perf_counter()
    path = f"/api/v1/runs/{run.id}/points?name=loss"
    while time.perf_counter() - t0 < 60:
        try:
            if len(_get(url, token, path).get("steps", [])) == points:
                break
        except OSError:
            pass
        time.sleep(0.02)
    lag = time.perf_counter() - t0
    run.finish()
    vigily.flush(timeout=60)
    return lag


def main() -> None:
    p = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    p.add_argument("--procs", type=int, default=8)
    p.add_argument("--runs", type=int, default=4, help="runs per process")
    p.add_argument("--points", type=int, default=100_000, help="points per run")
    p.add_argument("--sample", type=int, default=1, help="time every Nth track() call")
    p.add_argument("--url", default=os.environ.get("VIGILY_URL"))
    p.add_argument("--token", default=os.environ.get("VIGILY_TOKEN"))
    p.add_argument("--engine", default="target/release/vigily-engine")
    p.add_argument("--engine-pid", type=int)
    p.add_argument("--json", help="also write the results to this file")
    a = p.parse_args()

    tmp = Path(tempfile.mkdtemp(prefix="vigily-bench-"))
    engine: Engine
    if a.url:
        if not a.token:
            sys.exit("--url needs --token (VIGILY_TOKEN) with the read and ingest scopes")
        engine, url, token = Engine(a.engine_pid), a.url.rstrip("/"), a.token
    else:
        engine, url, token = Engine.start(a.engine, tmp)
    os.environ["VIGILY_STATE_DIR"] = str(tmp / "client-state")
    try:
        ctx = mp.get_context("spawn")
        ready, go, results = ctx.Barrier(a.procs + 1), ctx.Event(), ctx.Queue()
        procs = [
            ctx.Process(target=_worker, args=(url, token, a.runs, a.points, a.sample, ready, go, results))
            for _ in range(a.procs)
        ]
        for pr in procs:
            pr.start()
        ready.wait()
        cpu0 = engine.cpu_seconds()
        t0 = time.perf_counter()
        go.set()
        got = [results.get() for _ in procs]
        wall = time.perf_counter() - t0
        for pr in procs:
            pr.join()
        cpu = engine.cpu_seconds()
        rss, peak = engine.memory_mb()
        lag = visibility(url, token, 10_000)
    finally:
        engine.stop()
        shutil.rmtree(tmp, ignore_errors=True)

    lat = sorted(x for g in got for x in g["lat"])
    total = sum(g["points"] for g in got)
    span = max(g["ended"] for g in got) - min(g["began"] for g in got)
    per_proc = sorted(g["points"] / g["tracking"] for g in got)
    out = {
        "procs": a.procs,
        "runs_per_proc": a.runs,
        "points_per_run": a.points,
        "points": total,
        "track_p50_us": _pct(lat, 0.50),
        "track_p99_us": _pct(lat, 0.99),
        "per_process_points_s": per_proc[len(per_proc) // 2],
        "engine_points_s": total / span,
        "wall_s": wall,
        "all_flushed": all(g["flushed"] for g in got),
        "engine_cpu_s": None if cpu is None or cpu0 is None else cpu - cpu0,
        "engine_rss_mb": rss,
        "engine_peak_rss_mb": peak,
        "visibility_s": lag,
    }
    rows = [
        ("track() p50", f"{out['track_p50_us']:.2f} µs", f"≤ {TARGETS['track_p50_us']} µs"),
        ("track() p99", f"{out['track_p99_us']:.2f} µs", ""),
        ("per process, while tracking (median)", f"{out['per_process_points_s']:,.0f} points/s", "≥ 300,000"),
        (
            f"engine-wide, until flush returned ({a.procs} procs)",
            f"{out['engine_points_s']:,.0f} points/s",
            "≥ 1,000,000 with 8" if a.procs >= 8 else "",
        ),
        ("visibility lag (10 000 points, no flush)", f"{lag:.2f} s", "≤ 3 s"),
    ]
    if out["engine_cpu_s"] is not None:
        cpu_s = out["engine_cpu_s"]
        rows.append(("engine CPU time", f"{cpu_s:.1f} s ({cpu_s / span:.1f} cores)", ""))
    if rss is not None:
        rows.append(("engine RSS now / peak", f"{rss:.0f} MB / " + (f"{peak:.0f} MB" if peak else "?"), ""))
    print(f"{a.procs} processes x {a.runs} runs x {a.points:,} points = {total:,} points in {span:.1f} s")
    if not out["all_flushed"]:
        print("warning: a flush timed out")
    for name, value, target in rows:
        print(f"  {name:<44} {value:>26}  {target}")
    if a.json:
        Path(a.json).write_text(json.dumps(out, indent=2) + "\n")


if __name__ == "__main__":
    main()
