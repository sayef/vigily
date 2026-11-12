import asyncio
import json
import subprocess
import sys
import textwrap
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

import pytest
import vigily
from vigily import Dataset, Run, evaluate, evaluators, tracing


class Collector:
    """A stand-in for ``vigily up``'s ingest endpoint that keeps every span it receives."""

    def __init__(self) -> None:
        self.spans: list[dict] = []
        self.auth: list[str | None] = []
        self.lock = threading.Lock()
        outer = self

        class Handler(BaseHTTPRequestHandler):
            def do_POST(self) -> None:  # noqa: N802
                body = self.rfile.read(int(self.headers["content-length"]))
                with outer.lock:
                    outer.spans.extend(json.loads(body)["spans"])
                    outer.auth.append(self.headers.get("authorization"))
                self.send_response(200)
                self.send_header("content-type", "application/json")
                self.end_headers()
                self.wfile.write(b"{}")

            def log_message(self, *args: object) -> None:
                pass

        self.server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        self.url = f"http://127.0.0.1:{self.server.server_address[1]}"
        threading.Thread(target=self.server.serve_forever, daemon=True).start()

    def by_name(self) -> dict[str, dict]:
        return {s["name"]: s for s in self.spans}


@pytest.fixture
def collector():
    c = Collector()
    tracing.configure(c.url, enabled=True, flush_interval=0.05)
    yield c
    tracing.set_redactor(None)
    tracing.flush()
    c.server.shutdown()


def test_spans_are_sent_with_the_token(collector, monkeypatch):
    monkeypatch.setenv("VIGILY_TOKEN", "token-from-env")
    tracing.configure(collector.url)
    with vigily.trace("from-env"):
        pass
    assert tracing.flush()
    tracing.configure(collector.url, token="token-given")
    with vigily.trace("given"):
        pass
    assert tracing.flush()
    tracing.configure(token="")
    monkeypatch.delenv("VIGILY_TOKEN")
    tracing.configure(collector.url)
    with vigily.trace("none"):
        pass
    assert tracing.flush()
    assert collector.auth == ["Bearer token-from-env", "Bearer token-given", None]


def test_nested_spans_share_a_trace_and_point_at_their_parent(collector):
    with vigily.trace("root", run="run-1") as root:
        with vigily.trace("chat", kind="llm", model="model-a", provider="example") as llm:
            llm.set_input([{"role": "user", "content": "Question A"}])
            llm.set_output("Answer A")
            llm.set_usage(input_tokens=120, output_tokens=30, cached_tokens=100)
        with vigily.trace("search", kind="tool"):
            assert tracing.current_span().name == "search"
        assert tracing.current_span() is root
    assert tracing.current_span() is None
    assert tracing.flush()
    s = collector.by_name()
    assert set(s) == {"root", "chat", "search"}
    assert {x["trace_id"] for x in s.values()} == {root.trace_id}
    assert "parent_id" not in s["root"]
    assert s["chat"]["parent_id"] == s["search"]["parent_id"] == root.span_id
    assert s["chat"]["run_id"] == "run-1", "children inherit the run"
    assert s["chat"]["usage"] == {"input_tokens": 120, "output_tokens": 30, "cached_tokens": 100}
    assert s["chat"]["model"] == "model-a"
    assert s["chat"]["status"] == "ok"
    assert (
        s["root"]["start_time"] <= s["chat"]["start_time"] <= s["chat"]["end_time"] <= s["root"]["end_time"]
    )


def test_decorator_records_arguments_result_and_errors(collector):
    @vigily.trace(kind="tool")
    def lookup(query: str, limit: int = 3) -> list[str]:
        return [query] * limit

    @vigily.trace
    def broken() -> None:
        raise KeyError("synthetic")

    assert lookup("doc", limit=2) == ["doc", "doc"]
    with pytest.raises(KeyError):
        broken()
    tracing.flush()
    s = collector.by_name()
    assert s[lookup.__qualname__]["input"] == {"query": "doc", "limit": 2}
    assert s[lookup.__qualname__]["output"] == ["doc", "doc"]
    assert s[lookup.__qualname__]["kind"] == "tool"
    assert s[broken.__qualname__]["status"] == "error"
    assert s[broken.__qualname__]["error"].startswith("KeyError")


def test_async_tasks_keep_their_own_parents(collector):
    @vigily.trace(kind="llm")
    async def call(i: int) -> int:
        await asyncio.sleep(0.01 * (3 - i))
        with vigily.trace(f"inner-{i}"):
            await asyncio.sleep(0.005)
        return i

    async def one(i: int) -> int:
        async with vigily.trace(f"task-{i}"):
            return await call(i)

    async def main() -> list[int]:
        return await asyncio.gather(*(one(i) for i in range(3)))

    assert asyncio.run(main()) == [0, 1, 2]
    tracing.flush()
    s = collector.by_name()
    assert len(collector.spans) == 9
    for i in range(3):
        task, inner = s[f"task-{i}"], s[f"inner-{i}"]
        calls = [x for x in collector.spans if x.get("parent_id") == task["span_id"]]
        assert len(calls) == 1, "each call is the child of its own task's span"
        assert inner["parent_id"] == calls[0]["span_id"]
        assert inner["trace_id"] == task["trace_id"]
    assert len({s[f"task-{i}"]["trace_id"] for i in range(3)}) == 3


def test_redaction_hook_changes_or_drops_spans(collector):
    def redact(span):
        if span["name"] == "secret":
            return None
        if "input" in span:
            span["input"] = "[redacted]"
        return span

    tracing.set_redactor(redact)
    with vigily.trace("kept") as sp:
        sp.set_input({"email": "user@example.com"})
    with vigily.trace("secret"):
        pass
    tracing.flush()
    assert [x["name"] for x in collector.spans] == ["kept"]
    assert collector.spans[0]["input"] == "[redacted]"


def test_a_failing_redactor_drops_payloads(collector, capsys):
    def boom(span):
        raise RuntimeError("bug in hook")

    tracing.set_redactor(boom)
    with vigily.trace("x") as sp:
        sp.set_input("Question A")
        sp.set_output("Answer A")
        sp.set_attribute("note", "free text")
        sp.set_attribute("emails", ["user@example.com"])
        sp.set_attribute("customer", {"name": "Customer A"})
        sp.set_attribute("temperature", 0.2)
        sp.set_attribute("retries", 2)
        sp.record_error("no customer for user@example.com")
    with vigily.trace("y") as sp:
        sp.record_error("KeyError: user@example.com")
    tracing.flush()
    x, y = collector.spans
    assert "input" not in x and "output" not in x
    assert x["attributes"] == {"temperature": 0.2, "retries": 2}
    assert x["error"] == "error", "free text without an identifier type is dropped"
    assert y["error"] == "KeyError"
    assert "user@example.com" not in json.dumps(collector.spans)
    assert "redactor raised" in capsys.readouterr().err


def test_usage_from_reads_common_response_shapes():
    assert tracing.usage_from(
        {
            "usage": {
                "prompt_tokens": 10,
                "completion_tokens": 2,
                "prompt_tokens_details": {"cached_tokens": 4},
            }
        }
    ) == {"input_tokens": 10, "output_tokens": 2, "cached_tokens": 4}

    class Usage:
        input_tokens = 5
        output_tokens = 1
        cache_read_input_tokens = 20

    class Response:
        usage = Usage()

    assert tracing.usage_from(Response()) == {"input_tokens": 25, "output_tokens": 1, "cached_tokens": 20}


def test_disabled_tracing_sends_nothing(collector):
    tracing.configure(enabled=False)
    try:
        with vigily.trace("off"):
            pass
        tracing.flush()
        assert collector.spans == []
    finally:
        tracing.configure(enabled=True)


def test_evaluate_links_traces_to_items(collector, tmp_path):
    repo = str(tmp_path / "repo")
    ds = Dataset.create("qa", [{"id": "q1", "input": "a", "expected": "a"}], repo=repo)

    def target(x):
        with vigily.trace("answer"):
            return x

    res = evaluate(ds, target, [evaluators.exact_match()], repo=repo, concurrency=1)
    tracing.flush()
    (span,) = collector.spans
    assert span["run_id"] == res.run_id
    assert span["eval_item"] == "q1"
    rows = vigily.Repo(repo).results(res.run_id)
    assert rows[0]["trace_id"] == span["trace_id"]


def test_spans_are_sent_on_normal_exit(tmp_path):
    c = Collector()
    script = textwrap.dedent(
        f"""
        import vigily
        vigily.tracing.configure({c.url!r}, enabled=True, flush_interval=60)
        for i in range(500):
            with vigily.trace(f"s{{i}}"):
                pass
        """
    )
    subprocess.run([sys.executable, "-c", script], check=True, timeout=60)
    assert len(c.spans) == 500
    c.server.shutdown()


def test_run_object_links_traces(collector, tmp_path):
    run = Run(repo=str(tmp_path / "repo"))
    with vigily.trace("x", run=run):
        pass
    run.finish()
    tracing.flush()
    assert collector.spans[0]["run_id"] == run.id


def test_generators_are_traced_over_their_iteration(collector):
    @vigily.trace(kind="retrieval")
    def stream(n: int):
        for i in range(n):
            with vigily.trace(f"item-{i}"):
                pass
            yield i
        return "done"

    with vigily.trace("outer") as outer:
        got = []
        for x in stream(3):
            # The consumer's code is not inside the generator's span.
            assert tracing.current_span() is outer
            got.append(x)
    assert got == [0, 1, 2]

    @vigily.trace
    def broken():
        yield 1
        raise ValueError("synthetic")

    with pytest.raises(ValueError):
        list(broken())
    early = stream(5)
    next(early)
    early.close()
    tracing.flush()
    s = collector.by_name()
    gen = [x for x in collector.spans if x["name"] == stream.__qualname__]
    assert gen[0]["attributes"]["items"] == 3 and gen[0]["output"] == "done"
    assert gen[0]["parent_id"] == outer.span_id
    assert {x["parent_id"] for x in collector.spans if x["name"] == "item-0"} == {g["span_id"] for g in gen}
    assert gen[1]["attributes"]["items"] == 1 and gen[1]["status"] == "ok", "closed early"
    assert s[broken.__qualname__]["status"] == "error"


def test_async_generators_are_traced_over_their_iteration(collector):
    @vigily.trace
    async def agen(n: int):
        for i in range(n):
            await asyncio.sleep(0)
            yield i

    async def main() -> list[int]:
        return [x async for x in agen(4)]

    assert asyncio.run(main()) == [0, 1, 2, 3]
    tracing.flush()
    (span,) = collector.spans
    assert span["attributes"]["items"] == 4
    assert span["end_time"] >= span["start_time"]


def test_a_forked_child_exits_without_waiting_on_the_parent_queue(tmp_path):
    script = textwrap.dedent(
        """
        import os, vigily
        vigily.tracing.configure("http://127.0.0.1:9", enabled=True, flush_interval=60, timeout=30)
        with vigily.trace("parent"):
            pass
        pid = os.fork()
        if pid == 0:
            with vigily.trace("child"):
                pass
            raise SystemExit(0)
        _, status = os.waitpid(pid, 0)
        print(os.waitstatus_to_exitcode(status), flush=True)
        os._exit(0)
        """
    )
    started = __import__("time").monotonic()
    out = subprocess.run([sys.executable, "-c", script], capture_output=True, text=True, timeout=60)
    assert out.stdout.strip() == "0", out.stderr
    assert __import__("time").monotonic() - started < 8


def test_refused_batches_do_not_echo_the_response(capsys):
    class Refuser(BaseHTTPRequestHandler):
        def do_POST(self) -> None:  # noqa: N802
            self.rfile.read(int(self.headers["content-length"]))
            body = b'{"error": "span input was Customer A <user@example.com>"}'
            self.send_response(400)
            self.send_header("content-length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)

        def log_message(self, *args: object) -> None:
            pass

    server = ThreadingHTTPServer(("127.0.0.1", 0), Refuser)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    tracing.configure(f"http://127.0.0.1:{server.server_address[1]}", enabled=True)
    with vigily.trace("x"):
        pass
    tracing.flush()
    err = capsys.readouterr().err
    assert "HTTP 400" in err and "user@example.com" not in err
    server.shutdown()
