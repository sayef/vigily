import subprocess
import sys
import textwrap
from datetime import UTC, datetime

import pytest
from vigily import Repo, Run


@pytest.fixture
def repo(tmp_path):
    return str(tmp_path / "repo")


def test_run_logs_params_and_metrics(repo):
    run = Run(repo=repo, experiment="mnist", name="baseline", tags=["sweep"])
    run["hparams"] = {"lr": 3e-4, "batch_size": 64, "layers": (128, 64)}
    run["seed"] = 7
    assert run["hparams.lr"] == 3e-4
    assert run["hparams"]["batch_size"] == 64
    for step in range(100):
        run.track(1.0 / (step + 1), name="loss", step=step, context={"subset": "train"})
        if step % 10 == 0:
            run.track(2.0 / (step + 1), name="loss", step=step, epoch=step // 10, context={"subset": "val"})
    run.track(0.5, name="accuracy")
    run.track(0.6, name="accuracy")
    run.finish()

    r = Repo(repo)
    [info] = r.runs()
    assert info["id"] == run.id
    assert info["status"] == "finished"
    assert info["experiment"] == "mnist"
    assert info["tags"] == ["sweep"]
    params = r.params(run.id)
    assert params["hparams"] == {"lr": 3e-4, "batch_size": 64, "layers": [128, 64]}
    assert params["__system__"]["python"]
    assert run.params == {"hparams": {"lr": 3e-4, "batch_size": 64, "layers": [128, 64]}, "seed": 7}
    names = sorted((s["name"], s["context"].get("subset", "")) for s in r.series(run.id))
    assert names == [("accuracy", ""), ("loss", "train"), ("loss", "val")]
    val = r.metric(run.id, "loss", {"subset": "val"})
    assert val["steps"] == list(range(0, 100, 10))
    assert val["epochs"] == list(range(10))
    assert r.metric(run.id, "accuracy")["steps"] == [0, 1]


def test_context_manager_marks_failure(repo):
    with pytest.raises(RuntimeError), Run(repo=repo, capture_system=False) as run:
        run.track(1.0, name="loss")
        raise RuntimeError("diverged")
    assert {r["id"]: r["status"] for r in Repo(repo).runs()}[run.id] == "failed"


def test_metadata_resume_and_import_times(repo):
    start = datetime(2026, 1, 5, 9, 30, tzinfo=UTC)
    run = Run(repo=repo, experiment="cifar", created_at=start, capture_system=False)
    run.name = "resnet-a"
    run.description = "First try"
    run.add_tag("keep")
    run.add_tag("lr")
    run.remove_tag("lr")
    run.track(1.0, name="loss", timestamp=start.timestamp() + 60)
    run.flush()
    assert (run.name, run.description, run.tags) == ("resnet-a", "First try", ["keep"])

    again = Run(run.id, repo=repo, tags=["resumed"])
    again.track(0.5, name="loss")
    again.finish("aborted", ended_at=start.timestamp() + 3600)
    run._finished = True  # the first handle is superseded

    [info] = Repo(repo).runs()
    assert info["created_at"] == int(start.timestamp() * 1_000_000)
    assert info["ended_at"] == int((start.timestamp() + 3600) * 1_000_000)
    assert info["status"] == "aborted"
    assert info["tags"] == ["keep", "resumed"]
    loss = Repo(repo).metric(run.id, "loss")
    assert loss["steps"] == [0, 1]
    assert loss["timestamps"][0] == int((start.timestamp() + 60) * 1_000_000)


def test_kind_and_target(repo):
    run = Run(repo=repo, kind="eval", target={"model": "clf-v2", "git": "4f2c1ab"}, capture_system=False)
    assert (run.kind, run.target) == ("eval", {"model": "clf-v2", "git": "4f2c1ab"})
    assert run.params == {}
    other = Run(repo=repo, target="endpoint-a", capture_system=False)
    assert (other.kind, other.target) == ("training", {"ref": "endpoint-a"})
    run.finish()
    other.finish()
    again = Run(run.id, repo=repo)
    assert again.kind == "eval"
    again.finish()


def test_rejects_bad_input(repo):
    run = Run(repo=repo, capture_system=False)
    with pytest.raises(ValueError):
        run.finish("exploded")
    with pytest.raises(FileNotFoundError):
        Run("no-such-run", repo=repo)
    run.finish()


def test_unfinished_run_is_finished_at_exit(repo):
    script = textwrap.dedent(
        f"""
        from vigily import Run
        run = Run(repo={repo!r}, capture_system=False)
        run.track(1.0, name="loss")
        print(run.id)
        """
    )
    out = subprocess.run([sys.executable, "-c", script], capture_output=True, text=True, check=True)
    run_id = out.stdout.strip()
    script_err = script.replace("print(run.id)", "print(run.id, flush=True)\nraise ValueError('x')")
    err = subprocess.run([sys.executable, "-c", script_err], capture_output=True, text=True)
    failed_id = err.stdout.strip()
    statuses = {r["id"]: r["status"] for r in Repo(repo).runs()}
    assert statuses[run_id] == "finished"
    assert statuses[failed_id] == "failed"


def test_metric_reads_a_time_range(repo):
    run = Run(repo=repo, experiment="mnist", capture_system=False)
    t0 = 1_700_000_000.0
    for step in range(10):
        run.track(float(step), name="loss", step=step, timestamp=t0 + step)
    run.finish()
    m = Repo(repo).metric(run.id, "loss", start_time=t0 + 3, stop_time=t0 + 6)
    assert m["steps"] == [3, 4, 5]
    late = Repo(repo).metric(run.id, "loss", start_time=datetime.fromtimestamp(t0 + 8, UTC))
    assert late["steps"] == [8, 9]


def test_logging_to_a_deleted_run_says_so(repo):
    from vigily import RunDeletedError

    run = Run(repo=repo, experiment="mnist", capture_system=False)
    with pytest.raises(ValueError, match="still running"):
        Repo(repo).delete_runs([run.id])
    run.track(1.0, name="loss")
    run.finish()
    again = Run(run.id, repo=repo)  # resumed later, then deleted in the web app
    assert Repo(repo).delete_runs([run.id]) == 1
    with pytest.raises(RunDeletedError, match="was deleted"):
        again.track(2.0, name="loss")
        again.flush()
    with pytest.raises(RunDeletedError):
        again.finish()
    again.finish()  # nothing left to finish; no second error
    assert Repo(repo).runs() == []


def test_heartbeat_moves_last_write_without_changing_fields(repo):
    import time as _time

    run = Run(repo=repo, experiment="mnist", name="hb", capture_system=False)
    run.flush()
    before = Repo(repo).runs()[0]["updated_at"]
    _time.sleep(0.01)
    run._h.heartbeat()
    run.flush()
    after = Repo(repo).runs()[0]
    assert after["updated_at"] > before
    assert (after["name"], after["status"]) == ("hb", "running")
    run.finish()


def test_git_info_records_the_commit_hash_and_the_branch(tmp_path):
    from vigily import _git_info

    def git(*args):
        subprocess.run(
            ["git", "-c", "user.name=Test", "-c", "user.email=test@example.com", *args],
            cwd=tmp_path,
            check=True,
            capture_output=True,
        )

    git("init", "-q", "-b", "feature-x")
    git("commit", "-q", "--allow-empty", "-m", "first")
    head = subprocess.run(
        ["git", "rev-parse", "HEAD"], cwd=tmp_path, check=True, capture_output=True, text=True
    ).stdout.strip()
    info = _git_info(str(tmp_path))
    assert info == {"branch": "feature-x", "commit": head}
    assert len(info["commit"]) == 40
    assert _git_info(str(tmp_path / "missing")) is None
