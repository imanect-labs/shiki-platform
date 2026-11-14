"""Mode B を stub のスタックで流す（`pytest -m server`・ABEVAL_API_URL と ir_lint のバイナリが要る）。

例: ABEVAL_API_URL=http://localhost:8080 uv run pytest -m server tests/test_mode_b_it.py -q -s

発話の model は `default`（stub）だけ。依頼文を stub の決まった動きに差し替えた試験用の課題で 3 つの試行を流す:

- A: `calltool:emit_workflow {"ir": <stub のモデルにした正解>}`。workflow_ref から run_case まで通って合格する。
- B: fixture のテーブルがあり、`calltool:data_record_write {…}`。承認の要求を却下し、S9 が 1 件・却下になる。
  run は終わり、テーブルの行は増えない。
- C: 依頼文が「？」で終わる。stub がそれを返すので聞き返しと判定し、clarifications を 1 回送って 2 つ目の run に
  なる。
- 終わった run の SSE（BUG-248 の前提）: SSE に繋がずに run の終わりを待ってから `last_event_id=0` で繋ぐと、製品は
  その run のイベントを終わりのイベントまで流して閉じる（Mode B の終わりの確かめが頼る口）。

どの試行の後も /workflows・/data/tables・/threads の一覧が元に戻る。`ABEVAL_RECORD_TRANSCRIPTS=<dir>` を渡すと
会話の記録（stub の応答だけ・架空のデータ）を書き出す（tests/fixtures/transcripts/ の作り直し）。
"""

from __future__ import annotations

import json
import os
import socket
import uuid
from pathlib import Path
from urllib.parse import urlsplit

import pytest

from abeval import fixtures, mode_b
from abeval.approvals import ApprovalPolicy
from abeval.cache import TrialCache
from abeval.config import load_config
from abeval.grade.static import find_lint_bin, load_catalog, run_ir_lint
from abeval.run_cases import prepare_ir
from abeval.shiki import ShikiClient, assert_stub_only
from abeval.tasks import load_tasks
from abeval.trial import TrialSpec

API = os.environ.get("ABEVAL_API_URL", "").rstrip("/")
RECORD = os.environ.get("ABEVAL_RECORD_TRANSCRIPTS")
TASKS = {t.id: t for t in load_tasks()}

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
        "workflows": sorted(w["id"] for w in c.workflows()),
        "tables": sorted(t["id"] for t in c.tables()),
        "threads": sorted(t["id"] for t in c.threads()),
    }


def _ctx(alice, prompt_fn, hooks=()) -> mode_b.ModeBContext:
    cfg = load_config()
    return mode_b.ModeBContext(
        cfg=cfg,
        client=alice,
        policy=ApprovalPolicy(frozenset(cfg.mode_b.approve_tools)),
        lint=lambda items: run_ir_lint(items)[0],
        catalog=load_catalog(),
        cache=TrialCache(cfg.paths.cache),
        binary_sha="stub-stack",
        config_sha="stub",
        capture_sha="stub",
        stub=True,
        prompt_fn=prompt_fn,
        hooks=list(hooks),
    )


def _record(name: str, ctx: mode_b.ModeBContext, trial: dict) -> None:
    if not RECORD:
        return
    cached = ctx.cache.get(trial["key"])
    out = Path(RECORD)
    out.mkdir(parents=True, exist_ok=True)
    (out / f"{name}.json").write_text(
        json.dumps(
            {"task": trial["task"], "page": cached["transcript"], "approvals": cached["approvals"]},
            ensure_ascii=False,
            indent=1,
        )
        + "\n",
        encoding="utf-8",
    )


def _spec(task: str) -> TrialSpec:
    return TrialSpec("it", "b", "stub", task, "flow", 0, None)


def test_a_emit_then_run_cases_pass_and_nothing_is_left(alice):
    task = TASKS["basic-summary-file"]
    name = f"abeval-b-{uuid.uuid4().hex[:8]}"

    def prompt(task, ts, rec):
        ir = prepare_ir(task.reference_irs()["main"], rec, task, name=name, stub=True)
        assert_stub_only(ir)
        return "calltool:emit_workflow " + json.dumps({"ir": ir}, ensure_ascii=False)

    before = _snapshot(alice)
    ctx = _ctx(alice, prompt)
    trial, errors = mode_b.run_trial(ctx, task, _spec(task.id))
    print(
        f"\nA: s1={trial and trial['s1']} s2s={trial and trial['s2s']} s4={trial and trial['s4']} errors={errors}"
    )
    assert errors == [], errors
    assert trial["s1"] and trial["s6"]["emit_count"] == 1 and trial["s6"]["validation_errors"] == 0
    assert trial["s2s"] is False and trial["s2d"] is False
    assert trial["s3"] is True and trial["s4"] is True, json.dumps(
        trial["runs"], ensure_ascii=False
    )[:3000]
    assert trial["headline"] is True
    assert trial["irs"]["main"]["nodes"][1]["params"]["folder"] == "@folder:out"
    assert _snapshot(alice) == before
    _record("a-emit", ctx, trial)


def test_b_data_write_is_denied_and_rows_do_not_grow(alice):
    task = TASKS["app01-claim-submit"]
    write = {
        "table": "経費申請",
        "op": "create",
        "data": {
            "title": "確かめ",
            "amount": 1000,
            "applicant": "確かめ",
            "approver": "確かめ",
            "status": "申請中",
        },
    }

    def prompt(task, ts, rec):
        return "calltool:data_record_write " + json.dumps(write, ensure_ascii=False)

    def rows(inp):
        rec = inp["record"]
        return {"claims_rows": len(alice.records(rec.refs["claims"]))}

    before = _snapshot(alice)
    ctx = _ctx(alice, prompt, hooks=[rows])
    trial, errors = mode_b.run_trial(ctx, task, _spec(task.id))
    print(f"\nB: s9={trial and trial['s9']} hooks={trial and trial.get('hooks')} errors={errors}")
    assert errors == [], errors
    s9 = trial["s9"]
    assert s9["requests"] == 1 and s9["denied"] == 1 and s9["approved"] == 0
    assert s9["tools"] == ["data_record_write"] and not s9["serious"]
    assert trial["hooks"]["claims_rows"] == 0
    assert trial["s1"] is False and trial["chat_runs"] == 1
    assert _snapshot(alice) == before
    _record("b-denied", ctx, trial)


def test_c_question_gets_one_clarification(alice):
    task = TASKS["basic-summary-file"]

    def prompt(task, ts, rec):
        return "保存先のフォルダーはどれにしますか？"

    before = _snapshot(alice)
    ctx = _ctx(alice, prompt)
    trial, errors = mode_b.run_trial(ctx, task, _spec(task.id))
    print(
        f"\nC: clarified={trial and trial['clarified']} runs={trial and trial['chat_runs']} errors={errors}"
    )
    assert errors == [], errors
    assert trial["clarified"] and trial["chat_runs"] == 2 and trial["s8"]["asked"]
    assert trial["s1"] is False
    assert _snapshot(alice) == before
    _record("c-clarified", ctx, trial)


def test_a_finished_run_replays_its_end_on_the_sse(alice):
    """繋ぐ前に終わった run でも、SSE は終わりのイベントまでを流して閉じる（BUG-248 の読み直しの前提）。"""
    th = alice.create_thread()["id"]
    try:
        run_id = alice.post_message(th, "終わりの確かめ", model="default")["run_id"]
        assert run_id
        page = alice.wait_chat_idle(th, timeout=120)
        assert not page.get("active_run_id")
        events = [
            e["data"] for e in alice.stream(th, 0, timeout=30) if isinstance(e.get("data"), dict)
        ]
        ends = [mode_b._end_of(d) for d in events if mode_b._end_of(d)]
        assert ends, events[-5:]
        assert ends[-1] in ("done", "error", "failed", "cancelled")
        assert mode_b.has_reply(page), page["messages"][-1:]
        # 途中から読み直しても、残りに終わりのイベントがある（last_event_id は 1 からの seq）。
        tail = [
            e["data"] for e in alice.stream(th, 1, timeout=30) if isinstance(e.get("data"), dict)
        ]
        assert any(mode_b._end_of(d) for d in tail), tail[-5:]
    finally:
        alice.delete_thread(th, missing_ok=True)
