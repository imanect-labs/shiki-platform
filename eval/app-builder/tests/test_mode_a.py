"""Mode A（偽の shiki・abeval.proxy・偽の上流で通す・実 LLM を呼ばない・8787 へ繋がない）。

- 要求の形（temperature・top_p が無い・tool_choice auto・tools は emit_workflow 1 つ・defer_loading を外す）。
- 決まった SSE（tool_use・文だけ・聞き返し・max_tokens・refusal）を 5 つに分ける。
- `run` は `--go` が無ければ計画だけで、上流に 0 回。`--go` で 3 試行が通り、同じ条件でもう一度流すと
  上流に届かず、台帳に費用の行が増えない（キャッシュ・取り込みの $0 の行だけ）。
- 記録の IR は fixture の id を印に戻している。結果に会話の全文（思考・応答の文）が入らない。
- V1 の採点し直し（生成しない・取り込みの sha が違えば拒む）。費用の上限で拒まれたら打ち切る。
"""

from __future__ import annotations

import dataclasses
import json
import re
from pathlib import Path

import httpx
import pytest

from abeval import cli, fixtures, mode_a
from abeval.anthropic_client import AnthropicClient
from abeval.cache import TrialCache
from abeval.capture import Capture
from abeval.config import ModeA, load_config
from abeval.grade.static import LintResult
from abeval.ledger import Ledger
from abeval.plan import RunSpec
from abeval.shiki import ShikiClient
from abeval.tasks import load_tasks
from fake_anthropic import (
    TEXT_MARK,
    THINKING_MARK,
    events_ask,
    events_emit,
    events_max_tokens,
    events_refusal,
    events_text,
    sse_bytes,
)
from fake_chat import ChatFake
from fake_upstream import FakeUpstream, Reply, sse
from inproc import config_for, running_proxy
from recorded import catalog

TASKS = {t.id: t for t in load_tasks()}
TASK = TASKS["basic-summary-file"]
_FOLDER = re.compile(r"フォルダー「要約」（id: ([0-9a-f-]{36})）")


def capture_obj() -> Capture:
    tools = [
        {"name": "tool_search", "description": "s", "input_schema": {"type": "object"}},
        {
            "name": "emit_workflow",
            "description": "保存する",
            "input_schema": {"type": "object"},
            "defer_loading": True,
        },
    ]
    return Capture(
        "v0", "e0", "system の文", tools, 8192, {"fingerprint": {"llm_config_sha": "c" * 64}}
    )


def emit_for(prompt: str) -> list[dict]:
    """依頼文の参考情報から fixture の id を読み、正解の IR に入れて emit する。"""
    ir = json.loads(json.dumps(TASK.reference["ir"]))
    folder = _FOLDER.search(prompt).group(1)
    ir["nodes"][1]["params"]["folder"] = folder
    return events_emit(ir)


def responder(req) -> Reply:
    prompt = req.json["messages"][0]["content"]
    return Reply(headers={"content-type": "text/event-stream"}, chunks=sse(*emit_for(prompt)))


def fake_lint(items):
    return {i: LintResult(id=i) for i, _ in items}


def test_request_shape():
    body = mode_a.build_request(capture_obj(), "依頼", "high")
    assert not {"temperature", "top_p", "top_k"} & set(body)
    assert body["tool_choice"] == {"type": "auto"}
    assert [t["name"] for t in body["tools"]] == ["emit_workflow"]
    assert "defer_loading" not in body["tools"][0]
    assert body["thinking"] == {"type": "adaptive"} and body["output_config"] == {"effort": "high"}
    assert body["max_tokens"] == 8192 and body["system"] == "system の文" and body["stream"] is True
    assert body["messages"] == [{"role": "user", "content": "依頼"}]
    auto = mode_a.build_request(capture_obj(), "依頼", None)
    assert "thinking" not in auto and "output_config" not in auto


@pytest.mark.parametrize(
    "events,status",
    [
        (events_emit({"ir_version": 1, "nodes": [], "edges": []}), "emit"),
        (events_text(), "no_emit"),
        (events_ask(), "asked"),
        (events_max_tokens({"ir_version": 1, "nodes": []}), "truncated"),
        (events_refusal(), "refusal"),
    ],
)
def test_classify_the_five_outcomes(events, status):
    tr = httpx.MockTransport(
        lambda r: httpx.Response(
            200, headers={"content-type": "text/event-stream"}, content=sse_bytes(events)
        )
    )
    res = AnthropicClient("http://127.0.0.1:1", transport=tr).create(
        mode_a.build_request(capture_obj(), "x", "medium")
    )
    assert mode_a.classify(res) == status


def test_question_terms_from_the_task():
    tr = httpx.MockTransport(
        lambda r: httpx.Response(
            200,
            headers={"content-type": "text/event-stream"},
            content=sse_bytes(events_text("保存先を教えてください。")),
        )
    )
    res = AnthropicClient("http://127.0.0.1:1", transport=tr).create(
        mode_a.build_request(capture_obj(), "x", "medium")
    )
    assert mode_a.classify(res) == "no_emit"
    assert mode_a.classify(res, ("教えてください",)) == "asked"


class PassingIdentity:
    """確かめ直しがいつも通る版のサーバー（偽の shiki 用）。"""

    def __init__(self):
        self.calls = 0

    def recheck(self):
        self.calls += 1
        return None


@pytest.fixture()
def env(monkeypatch, tmp_path):
    """CLI を偽の shiki・abeval.proxy（一時のポート）・偽の上流で動かす。"""
    with FakeUpstream(responder) as up, running_proxy(up.url) as proxy:
        monkeypatch.setenv("ABEVAL_CONFIG", str(config_for(proxy.url, up.url, tmp_path)))
        fake = ChatFake(llm_url=proxy.url)
        with ShikiClient("http://localhost:8080", "alice", transport=fake.transport()) as c:
            fixtures.init_sentinel(c)

        def client(api, user, stub_only=False, timeout=60.0):
            return ShikiClient(
                api, user, transport=fake.transport(), stub_only=stub_only, timeout=timeout
            )

        versions = {"v0": "b" * 64, "v1": "d" * 64}
        monkeypatch.setattr(cli, "ShikiClient", client)
        monkeypatch.setattr(
            cli,
            "version_info",
            lambda label, stub: cli.Version(label, "c" * 40, versions[label], {}),
        )
        monkeypatch.setattr(cli, "_lint", lambda bin_path=None: fake_lint)
        monkeypatch.setattr(cli, "load_catalog", catalog)
        # 版のサーバーの点検（送り先のポート・プロセスの版と設定）と振る舞いの指紋は、偽の shiki では通らないので
        # 合格にする（点検そのものは tests/test_server_guard.py・test_behavior.py・test_run_guard.py が見る）。
        monkeypatch.setattr(cli.doctor, "version_guard", lambda *a, **k: ([], PassingIdentity()))
        monkeypatch.setattr(cli.behavior, "verify", lambda *a, **k: None)
        monkeypatch.setattr(cli, "grader_version", lambda *a: {"grader": 1, "ir_lint_sha256": None})
        llm = tmp_path / "llm-eval.toml"
        llm.write_text(
            '[[llm.providers]]\nid = "anthropic"\napi_key = "abeval-dummy"\n', encoding="utf-8"
        )
        yield fake, proxy, up, llm


def argv(llm, *extra, label="v0", exp="e1a"):
    return [
        "--mode", "a", "--label", label, "--exp", exp, "--tasks", "basic-summary-file",
        "--phrasings", "flow", "--trials", "3", "--api-url", "http://localhost:8080",
        "--llm-config", str(llm), "--skip-doctor", *extra,
    ]  # fmt: skip


def billed(cfg) -> list[dict]:
    return [e for e in Ledger(cfg.paths.ledger).entries() if not e.get("captured")]


def test_without_go_only_the_plan_and_nothing_reaches_the_upstream(env, capsys):
    fake, _, up, llm = env
    assert cli.main(argv(llm), command="run") == 0
    out = capsys.readouterr().out
    assert "計画:" in out and "--go を付けたときだけ" in out
    assert up.received == [] and fake.llm_replies == []
    assert Ledger(load_config().paths.ledger).entries() == []


def test_go_runs_three_trials_and_the_rerun_is_served_from_the_cache(env, tmp_path, monkeypatch):
    fake, proxy, up, llm = env
    monkeypatch.setenv("ABEVAL_DENYLIST", str(tmp_path / "no-denylist.txt"))
    cfg = load_config()
    assert cli.main(argv(llm, "--go"), command="run") == 0
    assert len(up.received) == 3
    for r in up.received:
        assert r.headers["x-api-key"] == "abeval-dummy"
        assert r.headers["x-campaign-tag"].startswith("abeval/e1a/e1a.v0.a.")
        assert "temperature" not in r.json
    first = billed(cfg)
    assert len(first) == 3 and all(e["cost_micros"] > 0 for e in first)
    assert {e["trial"] for e in first} == {f"basic-summary-file.flow.{n}.medium" for n in range(3)}
    doc = json.loads((tmp_path / "results" / "e1a.json").read_text())
    assert len(doc["trials"]) == 3
    t0 = doc["trials"][0]
    assert t0["status"] == "emit" and t0["s1"] is True and t0["s2s"] is False
    # 記録の IR は fixture の id を印に戻している。
    assert t0["irs"]["main"]["nodes"][1]["params"]["folder"] == "@folder:out"
    assert doc["versions"]["v0"]["capture_sha"] and doc["aggregates"][0]["trials"] == 3
    # 拒否リスト（G8）が無ければ、飛ばしたことを印に残す（ホームの絶対パスは残さない）。
    assert "G8" in doc["runs"][0]["g8_skipped"]
    raw = (tmp_path / "results" / "e1a.json").read_text()
    assert str(Path.home()) not in raw
    # 会話の全文は結果に入らず、キャッシュにだけある。
    text = (tmp_path / "results" / "e1a.json").read_text()
    assert THINKING_MARK not in text and TEXT_MARK not in text
    cached = "".join(p.read_text() for p in (cfg.paths.cache / "trials").glob("*.json"))
    assert THINKING_MARK in cached
    # 2 回目: 上流に届かず、費用の行が増えない（取り込みの $0 の行だけが足される）。
    lines_before = len(Ledger(cfg.paths.ledger).entries())
    assert cli.main(argv(llm, "--go"), command="run") == 0
    assert len(up.received) == 3
    assert billed(cfg) == first
    new = Ledger(cfg.paths.ledger).entries()[lines_before:]
    assert new and all(e.get("captured") and e["cost_micros"] == 0 for e in new)
    doc2 = json.loads((tmp_path / "results" / "e1a.json").read_text())
    assert len(doc2["trials"]) == 3 and all(t["from_cache"] for t in doc2["trials"])
    # 登録した実行は終わったら外す。
    with proxy.control() as ctrl:
        assert ctrl.spend()["runs"] == {}


def test_regrade_v1_uses_the_v0_generation_and_never_generates(env, tmp_path):
    fake, _, up, llm = env
    assert cli.main(argv(llm, "--go"), command="run") == 0
    n = len(up.received)
    assert cli.main(argv(llm, "--regrade-from", "v0", label="v1", exp="e1a-v1"), command="run") == 0
    assert len(up.received) == n
    doc = json.loads((tmp_path / "results" / "e1a-v1.json").read_text())
    assert len(doc["trials"]) == 3 and {t["label"] for t in doc["trials"]} == {"v1"}
    assert all(t["from_cache"] and t["s1"] for t in doc["trials"])


def test_regrade_refuses_a_different_capture(env, monkeypatch):
    fake, _, up, llm = env
    assert cli.main(argv(llm, "--go"), command="run") == 0
    monkeypatch.setattr("fake_chat.SYSTEM", "説明文が変わった版")
    with pytest.raises(SystemExit):
        cli.main(argv(llm, "--regrade-from", "v0", label="v1", exp="e1a-v1"), command="run")


def ctx_for(fake, proxy, run_id, *, cap):
    cfg = load_config()
    client = ShikiClient("http://localhost:8080", "alice", transport=fake.transport())
    fixtures.init_sentinel(client)
    return mode_a.ModeAContext(
        cfg=cfg,
        client=client,
        anthropic=AnthropicClient(proxy.url),
        capture=cap,
        lint=fake_lint,
        catalog=catalog(),
        cache=TrialCache(cfg.paths.cache),
        binary_sha="b" * 64,
        config_sha="c" * 64,
        ctrl=proxy.control(),
        run_id=run_id,
    )


def test_outcomes_other_than_emit_are_model_results_not_errors():
    scenarios = [events_text(), events_ask(), events_refusal(), events_max_tokens({"nodes": []})]
    it = iter(scenarios)
    with (
        FakeUpstream(
            lambda r: Reply(headers={"content-type": "text/event-stream"}, chunks=sse(*next(it)))
        ) as up,
        running_proxy(up.url) as proxy,
    ):
        fake = ChatFake()
        ctx = ctx_for(fake, proxy, "r1", cap=capture_obj())
        # 並列にすると台本の順が試行とずれるので 1 にする（結果は順に依らないが読みやすさのため）。
        ctx.cfg = dataclasses.replace(ctx.cfg, mode_a=ModeA(parallel=1, max_attempts=1))
        proxy.control().register_run("r1", "e0", 1.0)
        spec = RunSpec("e0", "a", "v0", ("basic-summary-file",), ("flow", "app"), 2)
        out = mode_a.run(ctx, spec.trial_specs(), TASKS)
    assert out.errors == []
    statuses = sorted(t["status"] for t in out.trials)
    assert statuses == sorted(["no_emit", "asked", "refusal", "truncated"])
    for t in out.trials:
        assert t["s1"] is False and t["headline"] is False
        assert t["s8"]["refusal"] == (t["status"] == "refusal")
        assert t["s8"]["asked"] == (t["status"] == "asked")


def test_budget_rejection_stops_the_run():
    with FakeUpstream(responder) as up, running_proxy(up.url) as proxy:
        fake = ChatFake()
        ctx = ctx_for(fake, proxy, "tiny", cap=capture_obj())
        proxy.control().register_run("tiny", "e0", 0.000001)
        spec = RunSpec("e0", "a", "v0", ("basic-summary-file", "basic-branch-amount"), ("flow",), 1)
        out = mode_a.run(ctx, spec.trial_specs(), TASKS)
        assert up.received == []
    assert out.trials == [] and out.stopped
    kinds = [e["kind"] for e in out.errors]
    assert kinds == ["budget", "stopped"]


def test_grade_and_report_work_from_the_results(env, tmp_path):
    fake, _, up, llm = env
    assert cli.main(argv(llm, "--go"), command="run") == 0
    assert cli.main(["--exp", "e1a"], command="grade") == 0
    doc = json.loads((tmp_path / "results" / "e1a.json").read_text())
    assert len(doc["trials"]) == 3 and all(t["s2s"] is False for t in doc["trials"])
    assert cli.main([], command="report") == 0
    text = (tmp_path / "results" / "summary.md").read_text()
    assert "v0/a/medium" in text and "## 6. 限界" in text
    assert THINKING_MARK not in text and TEXT_MARK not in text


def test_stub_plumbing_failure_is_an_error_not_a_saved_trial(monkeypatch):
    """stub の配管が基盤の失敗で流せなければ、採点せずに errors（stub_plumbing）に入れる（見出しを真にしない）。"""
    from abeval import grading

    with FakeUpstream(responder) as up, running_proxy(up.url) as proxy:
        ctx = ctx_for(ChatFake(), proxy, "r1", cap=capture_obj())
        proxy.control().register_run("r1", "e0", 1.0)
        monkeypatch.setattr(
            mode_a,
            "stub_plumbing",
            lambda *a, **k: grading._summary([{"case": "normal", "error": "TimeoutError: 180"}]),
        )
        spec = RunSpec("e0", "a", "v0", ("basic-summary-file",), ("flow",), 1)
        out = mode_a.run(ctx, spec.trial_specs(), TASKS)
    assert out.trials == []
    assert [e["kind"] for e in out.errors] == ["stub_plumbing"]
    assert "TimeoutError" in out.errors[0]["detail"]


def test_without_stub_plumbing_the_headline_is_undecided():
    with FakeUpstream(responder) as up, running_proxy(up.url) as proxy:
        ctx = ctx_for(ChatFake(), proxy, "r1", cap=capture_obj())
        ctx.stub_plumbing = False
        proxy.control().register_run("r1", "e0", 1.0)
        spec = RunSpec("e0", "a", "v0", ("basic-summary-file",), ("flow",), 1)
        out = mode_a.run(ctx, spec.trial_specs(), TASKS)
    [t] = out.trials
    assert t["s1"] is True and t["s2d"] is None and t["headline"] is None


def test_requires_are_judged_by_the_version_server(env, capsys):
    """requires の node:* は、HEAD のカタログでなく版のサーバーの検証で決める（V0 に無い種類の課題は対象外）。"""
    from abeval import versions

    fake, _, up, llm = env
    v0_missing = {"data.record.upsert", "transform.date"}
    fake.validator = lambda ir: [
        {"code": "ir.unknown_node_type", "node_id": n["id"]}
        for n in ir.get("nodes") or []
        if n.get("type") in v0_missing
    ]
    versions.write_record(
        versions.parse_record(
            {
                "label": "v0",
                "commit": "c" * 40,
                "tree_clean": True,
                "binary_sha256": "b" * 64,
                "built_at": "2026-11-06T00:00:00Z",
                "rustc": "rustc 1.99.0",
            }
        )
    )
    args = argv(llm)
    i = args.index("--tasks")
    args[i + 1] = "basic-summary-file,app01-posting-batch,app03-daily-submit"
    assert cli.main(args, command="run") == 0
    out = capsys.readouterr().out
    assert "[対象外] app01-posting-batch" in out and "[対象外] app03-daily-submit" in out
    assert "[対象外] basic-summary-file" not in out
    rec = versions.read_record("v0")
    assert "data.query" in rec.extra["node_types"]
    assert not v0_missing & set(rec.extra["node_types"])
    assert up.received == []


# --- 版のサーバーの確かめ直し（BUG-246） ---


class ChangesAt(PassingIdentity):
    """n 回目の確かめ直しから「替わった」と答える版のサーバー。"""

    def __init__(self, n: int):
        super().__init__()
        self.n = n

    def recheck(self):
        super().recheck()
        return "版のサーバーが替わりました" if self.calls >= self.n else None


def test_run_rechecks_the_server_before_the_capture(env, monkeypatch, tmp_path, capsys):
    fake, _, up, llm = env
    ident = ChangesAt(1)
    monkeypatch.setattr(cli.doctor, "version_guard", lambda *a, **k: ([], ident))
    assert cli.main(argv(llm, "--go"), command="run") == 1
    assert "取り込みの前の確かめ直し" in capsys.readouterr().out
    assert up.received == [] and fake.llm_replies == []
    assert not (tmp_path / "captures").exists() and not (tmp_path / "results").exists()


def test_run_does_not_write_results_when_the_server_changed_during_the_run(
    env, monkeypatch, tmp_path, capsys
):
    fake, _, up, llm = env
    ident = ChangesAt(2)  # 取り込みの前は通り、結果を書く前に替わっていた
    monkeypatch.setattr(cli.doctor, "version_guard", lambda *a, **k: ([], ident))
    assert cli.main(argv(llm, "--go"), command="run") == 1
    assert "結果を書く前の確かめ直し" in capsys.readouterr().out
    assert not (tmp_path / "results" / "e1a.json").exists()
    assert ident.calls == 2


def test_mode_b_gets_the_recheck_of_the_checked_server(env, monkeypatch):
    fake, _, up, llm = env
    ident = PassingIdentity()
    monkeypatch.setattr(cli.doctor, "version_guard", lambda *a, **k: ([], ident))
    got = {}

    def fake_run(ctx, specs, tasks):
        got["preflight"] = ctx.preflight
        return [], []

    monkeypatch.setattr(cli.mode_b, "run", fake_run)
    args = argv(llm, "--go")
    args[args.index("a")] = "b"
    assert cli.main(args, command="run") == 0
    assert got["preflight"] == ident.recheck
