"""Mode A を stub のスタックで流す（`pytest -m server`・ABEVAL_API_URL と ir_lint のバイナリが要る）。

例: ABEVAL_API_URL=http://localhost:8080 uv run pytest -m server tests/test_mode_a_it.py -q -s

偽の上流（Anthropic の SSE を返す偽のサーバー）が、依頼文の参考情報の id を入れた正解の IR を emit_workflow の
tool_use で返す。生成は abeval.proxy（テストの中で動かす・上流は偽の上流）を通す。その IR を stub のスタックの
`/workflows/validate` で S1 と判定し、ir_lint で静的に採点し、stub の配管（複製を stub のモデルで保存して
stub_ok の run_case を流す）を通す。壊した IR（無いテーブルの id）は S1 で落ちる。終わった後に、テーブル・
ルート直下のノード・ワークフローの一覧が前と同じに戻る。実 LLM は呼ばない（8787 にも繋がない）。
"""

from __future__ import annotations

import json
import os
import re
import socket
import uuid
from urllib.parse import urlsplit

import pytest

from abeval import fixtures, mode_a
from abeval.anthropic_client import AnthropicClient
from abeval.cache import TrialCache
from abeval.capture import Capture
from abeval.config import load_config
from abeval.grade.static import find_lint_bin, load_catalog, run_ir_lint
from abeval.plan import RunSpec
from abeval.shiki import ShikiClient
from abeval.tasks import load_tasks, substitute
from fake_anthropic import events_emit
from fake_upstream import FakeUpstream, Reply, sse
from inproc import running_proxy

API = os.environ.get("ABEVAL_API_URL", "").rstrip("/")
TASKS = {t.id: t for t in load_tasks()}
PICK = (
    "basic-summary-file",
    "basic-branch-amount",
    "app01-claim-submit",
    "app03-pair",
    "app10-survey-csv",
    "app11-checkout",
)
_LINE = re.compile(r"^- (テーブル|フォルダー|ファイル)「([^」]+)」（id: ([0-9a-f-]{36})", re.M)
KIND = {"テーブル": "tables", "フォルダー": "folders", "ファイル": "csv"}

pytestmark = [
    pytest.mark.server,
    pytest.mark.skipif(
        not API, reason="ABEVAL_API_URL が無いので飛ばします（stub のスタックの URL を渡す）"
    ),
    pytest.mark.skipif(find_lint_bin() is None, reason="ir_lint のバイナリが無いので飛ばします"),
]


def _reachable(url: str) -> bool:
    u = urlsplit(url)
    try:
        with socket.create_connection((u.hostname, u.port or 80), timeout=2):
            return True
    except OSError:
        return False


def ids_from_prompt(task, prompt: str) -> dict[str, str]:
    """依頼文の参考情報から ref → id を読む（偽のモデルが「参考情報の id を使った」ことにする）。"""
    by_name = {(KIND[k], name): i for k, name, i in _LINE.findall(prompt)}
    out = {}
    for key in ("tables", "folders", "csv"):
        for item in task.fixtures.get(key) or []:
            out[item["ref"]] = by_name[(key, item["name"])]
    return out


def responder(state: dict):
    def reply(req) -> Reply:
        prompt = req.json["messages"][0]["content"]
        task = next(
            t for t in TASKS.values() if t.phrasings["flow"] == prompt.split("\n\n参考情報")[0]
        )
        ids = ids_from_prompt(task, prompt)
        irs = [substitute(ir, ids) for ir in task.reference_irs().values()]
        if task.id in state.get("break", ()):
            bad = json.loads(json.dumps(irs[0]).replace(ids[next(iter(ids))], str(uuid.uuid4())))
            irs = [bad]
        events = events_emit(irs[0])
        if len(irs) > 1:  # workflow_set は 1 つの応答で 2 つ emit する
            second = events_emit(irs[1])
            tool = [e for e in second if e.get("index") == 2]
            for e in tool:
                e["index"] = 3
                if e.get("content_block"):
                    e["content_block"]["id"] = "toolu_3"
            events = events[:-2] + tool + events[-2:]
        return Reply(headers={"content-type": "text/event-stream"}, chunks=sse(*events))

    return reply


@pytest.fixture(scope="module")
def alice():
    if not _reachable(API):
        pytest.skip(f"{API} に届かないので飛ばします（stub のスタックを起動する）")
    c = ShikiClient(API, "alice", stub_only=True, timeout=60.0)
    c.login()
    node, made = fixtures.init_sentinel(c)
    yield c
    if made:
        c.delete_folder(node["id"], missing_ok=True)
    c.close()


def _snapshot(c: ShikiClient) -> dict:
    return {
        "tables": sorted(t["id"] for t in c.tables()),
        "nodes": sorted(n["id"] for n in c.nodes() if n.get("name") != fixtures.SENTINEL_NAME),
        "workflows": sorted(w["id"] for w in c.workflows()),
    }


def _capture() -> Capture:
    tools = [
        {
            "name": "emit_workflow",
            "description": "IR を保存する",
            "input_schema": {"type": "object"},
        }
    ]
    return Capture("stub", "it", "評価の確かめ", tools, 8192, {})


def _run(alice, tasks, state):
    cfg = load_config()
    with (
        FakeUpstream(responder(state)) as up,
        running_proxy(up.url) as proxy,
        proxy.control() as ctrl,
    ):
        ctrl.register_run("it-a", "it", 1.0)
        ctx = mode_a.ModeAContext(
            cfg=cfg,
            client=alice,
            anthropic=AnthropicClient(proxy.url),
            capture=_capture(),
            lint=lambda items: run_ir_lint(items)[0],
            catalog=load_catalog(),
            cache=TrialCache(cfg.paths.cache),
            binary_sha="stub-stack",
            config_sha="stub",
            ctrl=ctrl,
            run_id="it-a",
        )
        spec = RunSpec("it", "a", "stub", tasks, ("flow",), 1)
        out = mode_a.run(ctx, spec.trial_specs(), TASKS)
        return out, len(up.received)


def test_oracle_irs_pass_validate_static_and_stub_plumbing(alice):
    before = _snapshot(alice)
    out, sent = _run(alice, PICK, {})
    assert sent == len(PICK)
    assert out.errors == [], out.errors
    by_task = {t["task"]: t for t in out.trials}
    for task_id in PICK:
        t = by_task[task_id]
        print(
            f"\n{task_id}: s1={t['s1']} s2s={t['s2s']} s2d={t['s2d']} s3={t['s3']} headline={t['headline']}"
        )
        assert t["status"] == "emit" and t["s1"], json.dumps(
            t.get("validation"), ensure_ascii=False
        )
        assert t["s2s"] is False, json.dumps(t["static"], ensure_ascii=False)[:2000]
        assert t["s2d"] is False and t["s3"] is True, json.dumps(t["stub"], ensure_ascii=False)[
            :3000
        ]
        assert t["headline"] is True
    assert by_task["app03-pair"]["missing_roles"] == []
    assert _snapshot(alice) == before, "片付けの後に残ったものがあります"


def test_broken_ir_fails_s1(alice):
    before = _snapshot(alice)
    out, _ = _run(alice, ("app01-claim-submit",), {"break": {"app01-claim-submit"}})
    [t] = out.trials
    codes = [c for v in t["validation"].values() for c in v["errors"]]
    print(f"\n壊した IR の検証: {codes}")
    assert t["s1"] is False and codes and t["headline"] is False
    assert "stub" not in t  # 保存できない IR は配管を流さない
    assert _snapshot(alice) == before
