"""Finding and starting a repository's engine, as the command line does (vigily_api::local)."""

import json
import os
import subprocess
import sys
import textwrap

import pytest
from conftest import STATE
from vigily import Repo, _engine


def engine_files() -> dict[str, dict]:
    d = os.path.join(STATE, "engines")
    out = {}
    for name in os.listdir(d) if os.path.isdir(d) else []:
        if name.endswith(".json"):
            with open(os.path.join(d, name), encoding="utf-8") as f:
                out[name[: -len(".json")]] = json.load(f)
    return out


def test_keys_follow_the_engines_spelling_of_the_repository(tmp_path):
    real = tmp_path / "real"
    real.mkdir()
    (tmp_path / "link").symlink_to(real)
    # Through a symbolic link, with `..`, and not created yet: one key, the engine's.
    repo = str(tmp_path / "link" / "x" / ".." / "runs")
    canonical = _engine.canonical_location(repo)
    assert canonical == str(real.resolve() / "runs")
    assert _engine.canonical_location("s3://bucket/runs/") == "s3://bucket/runs"
    assert len(_engine.repo_key(canonical)) == 16

    Repo(repo).runs()  # starts the engine with VIGILY_ENGINE; it creates the directory
    f = engine_files()[_engine.repo_key(canonical)]
    assert f["repo"] == canonical
    assert f["auto_started"] is True
    assert (real / "runs").is_dir()
    assert _engine.canonical_location(repo) == canonical, "the same spelling once it exists"


def test_two_processes_starting_at_once_share_one_engine(tmp_path):
    repo = str(tmp_path / "repo")
    script = textwrap.dedent(
        f"""
        from vigily import Run
        run = Run(repo={repo!r}, capture_system=False)
        run.track(1.0, name="loss")
        run.flush()
        print(run.id, flush=True)
        """
    )
    procs = [
        subprocess.Popen(
            [sys.executable, "-c", script], stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True
        )
        for _ in range(2)
    ]
    ids = []
    for p in procs:
        out, err = p.communicate(timeout=120)
        assert p.returncode == 0, err
        ids.append(out.strip())
    key = _engine.repo_key(_engine.canonical_location(repo))
    f = engine_files()[key]
    log = os.path.join(STATE, "engines", f"{key}.log")
    with open(log, encoding="utf-8") as fh:
        text = fh.read()
    # A process that lost the race started an engine that found the repository served and left.
    for line in text.splitlines():
        if "is already served at" in line:
            assert f["url"] in line
    assert sorted(r["id"] for r in Repo(repo).runs()) == sorted(ids)
    assert engine_files()[key]["pid"] == f["pid"]


def test_a_missing_engine_says_how_to_install_it(tmp_path, monkeypatch):
    monkeypatch.delenv("VIGILY_ENGINE")
    monkeypatch.setenv("PATH", str(tmp_path))
    with pytest.raises(_engine.EngineNotFound, match=r"pip install 'vigily\[local\]'"):
        Repo(str(tmp_path / "repo")).runs()


def test_a_run_without_an_engine_never_blocks_and_is_spooled(tmp_path):
    env = {k: v for k, v in os.environ.items() if k != "VIGILY_ENGINE"}
    env["PATH"] = str(tmp_path)
    out = subprocess.run(
        [
            sys.executable,
            "-c",
            textwrap.dedent(
                f"""
                import time
                from vigily import Run
                t = time.perf_counter()
                run = Run(repo={str(tmp_path / "repo")!r}, capture_system=False)
                run.track(1.0, name="loss")
                print(time.perf_counter() - t, flush=True)
                """
            ),
        ],
        capture_output=True,
        text=True,
        timeout=120,
        env=env,
    )
    assert out.returncode == 0, out.stderr
    assert float(out.stdout) < 0.5
    assert "pip install 'vigily[local]'" in out.stderr
    assert "in the spool at" in out.stderr
