import json

import pytest
from vigily import Dataset, Repo, Run, datasets, evaluate
from vigily import evaluators as ev


@pytest.fixture
def repo(tmp_path):
    return str(tmp_path / "repo")


ITEMS = [
    {"id": "q1", "input": "where is my parcel", "expected": "shipping", "metadata": {"lang": "en"}},
    {"id": "q2", "input": "refund please", "expected": "billing", "metadata": {"lang": "en"}},
    {"id": "q3", "input": "wo ist mein paket", "expected": "shipping", "metadata": {"lang": "de"}},
]


def test_dataset_versions(repo, tmp_path):
    v1 = Dataset.create("intents", ITEMS, description="Support intents", note="first", repo=repo)
    assert (v1.version, len(v1)) == (1, 3)
    assert [i.id for i in v1] == ["q1", "q2", "q3"]
    assert Dataset.create("intents", ITEMS, repo=repo).version == 1, "same items, same version"
    v2 = v1.add(
        [{"id": "q2", "input": "refund please", "expected": "refunds"}, ("cancel my plan", "billing")]
    )
    assert v2.version == 2
    assert v2.changes == {"added": 1, "updated": 1, "removed": 0}
    assert Dataset("intents", version=1, repo=repo).items[1].expected == "billing"
    assert Dataset("intents", repo=repo).version == 2
    assert [v["version"] for v in v2.versions()] == [1, 2]
    assert datasets(repo)[0]["description"] == "Support intents"

    jl = tmp_path / "d.jsonl"
    jl.write_text(
        "\n".join(json.dumps({"text": i["input"], "label": i["expected"], "src": "x"}) for i in ITEMS)
    )
    d = Dataset.from_jsonl("from-jsonl", jl, input="text", expected="label", id=None, repo=repo)
    assert len(d) == 3 and d.items[0].metadata == {"src": "x"}
    cs = tmp_path / "d.csv"
    cs.write_text("id,input,expected,lang\nq1,hello,greet,en\nq2,hallo,greet,de\n")
    d = Dataset.from_csv("from-csv", cs, repo=repo)
    assert [i.metadata["lang"] for i in d] == ["en", "de"]
    with pytest.raises(ValueError):
        Dataset.create("bad name", ITEMS, repo=repo)
    with pytest.raises(FileNotFoundError):
        Dataset("missing", repo=repo)


def test_builtin_evaluators():
    def run(e, output, expected=None):
        return e(input=None, output=output, expected=expected, item=None)

    assert run(ev.exact_match(), " shipping ", "shipping").passed
    assert not run(ev.exact_match(case_sensitive=True), "Shipping", "shipping").passed
    assert run(ev.contains(), "it is shipping related", "shipping").passed
    assert run(ev.regex(r"^\d{4}-\d{2}$"), "2026-11").passed
    assert run(ev.numeric(tolerance=0.01), "3.141", 3.14).passed
    assert not run(ev.numeric(tolerance=0.01), "abc", 3.14).passed
    schema = {
        "type": "object",
        "required": ["intent"],
        "properties": {"intent": {"enum": ["shipping", "billing"]}},
    }
    assert run(ev.json_schema(schema), '{"intent": "billing"}').passed
    bad = run(ev.json_schema(schema), '{"intent": "other"}')
    assert not bad.passed and "not one of" in bad.reason
    assert not run(ev.json_schema(schema), "not json").passed
    judge = ev.custom("judge", lambda output: (0.8, True, "close enough"))
    assert run(judge, "x").value == 0.8
    assert judge.version.startswith("sha256:")


def test_evaluate_records_items_scores_and_features(repo, monkeypatch):
    monkeypatch.setenv("VIGILY_SCHEDULE", "nightly-intents")
    monkeypatch.setenv("VIGILY_SCHEDULE_RUN", "00000000000000000001-1")
    ds = Dataset.create("intents", ITEMS, repo=repo)

    def classify(text):
        if "refund" in text:
            raise RuntimeError("model timeout")
        return "shipping" if "parcel" in text or "paket" in text else "other"

    result = evaluate(
        ds,
        classify,
        [ev.exact_match(), ev.custom("short", lambda output: len(output or "") < 10)],
        target_ref={"model": "intent-clf-v1"},
        experiment="intents-nightly",
        features=lambda item: {"length": len(item.input), "lang": item.metadata["lang"]},
        repo=repo,
    )
    assert (result.items, result.errors) == (3, 1)
    assert result.scores["exact_match"] == pytest.approx(2 / 3)
    assert result.failures == ["q2"]

    r = Repo(repo)
    [info] = [x for x in r.runs() if x["id"] == result.run_id]
    assert info["status"] == "finished"
    assert (info["kind"], info["target"]) == ("eval", {"model": "intent-clf-v1"})
    params = r.params(result.run_id)
    assert set(params["__run__"]) == {"schedule"}
    assert params["__run__"]["schedule"]["name"] == "nightly-intents"
    assert params["__eval__"]["dataset"] == {"name": "intents", "version": 1, "digest": ds.digest, "items": 3}
    assert [e["name"] for e in params["__eval__"]["evaluators"]] == ["exact_match", "short"]
    assert r.metric(result.run_id, "eval/exact_match")["values"] == [pytest.approx(2 / 3)]

    rows = json.loads(r._repo.results(result.run_id))
    by_id = {x["item_id"]: x for x in rows}
    assert by_id["q2"]["error"].startswith("RuntimeError: model timeout")
    assert by_id["q1"]["scores"][0]["evaluator_version"] == "vigily/exact_match@1"
    assert by_id["q1"]["item_hash"] == ds.items[0].hash

    run = Run(repo=repo, capture_system=False)
    run.log_features([{"x": 1.0}, {"x": 2.0}])
    run.finish()


def test_results_are_stored_before_the_run_is_finished(repo):
    """Another process that sees the eval run finished also sees all its results."""
    import subprocess
    import sys
    import textwrap

    child = textwrap.dedent(
        f"""
        import time
        from vigily import Dataset, evaluate, evaluators as ev
        items = [{{"input": f"q{{i}}", "expected": f"a{{i}}"}} for i in range(50)]
        ds = Dataset.create("toy", items, repo={repo!r})
        r = evaluate(ds, lambda x: "a" + x[1:], [ev.exact_match()], repo={repo!r}, experiment="e")
        print(r.run_id, flush=True)
        time.sleep(30)
        """
    )
    p = subprocess.Popen([sys.executable, "-c", child], stdout=subprocess.PIPE, text=True)
    try:
        run_id = p.stdout.readline().strip()
        r = Repo(repo)
        [info] = [x for x in r.runs() if x["id"] == run_id]
        assert info["status"] == "finished"
        assert len(json.loads(r._repo.results(run_id))) == 50
    finally:
        p.kill()
