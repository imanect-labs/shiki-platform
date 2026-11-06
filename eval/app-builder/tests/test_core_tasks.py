"""中核の課題集（data/tasks/core.json・GAP-30 §3.2）のオフラインの受け入れ: G1・G2（記録した ir_lint）・G4・G5・G7・
G8（架空の語の一覧と、手元にあれば本物の拒否リスト）と、課題の決まり（20 課題・スケジュールの scheduled_at・
LLM に依存する分類・このマシンで動かない機能を使わない・中身の無い IR では照合が落ちる）。

正解の IR を stub のスタックで流す確かめは tests/test_reference_it.py（`-m server`）。
"""

from __future__ import annotations

import datetime as dt

import pytest

import recorded
from abeval import gen_checks as g
from abeval.fixtures import resolve_rel
from abeval.grade.checks import CaseResult, evaluate, fixture_value
from abeval.tasks import load_tasks, offline_irs

CORE_IDS = [
    "basic-summary-file",
    "basic-branch-amount",
    "basic-switch-category",
    "basic-map-rows",
    "app01-claim-submit",
    "app01-posting-batch",
    "app02-triage",
    "app03-daily-submit",
    "app03-weekly-summary",
    "app03-pair",
    "app05-screening-text",
    "app06-stock-move",
    "app06-reorder-check",
    "app08-weekly-kpi",
    "app11-checkout",
    "app11-return",
    "app11-overdue",
    "app12-faq-table",
    "app09-minutes-tasks",
    "app10-survey-csv",
]
SCHEDULED = [
    "app01-posting-batch",
    "app03-weekly-summary",
    "app06-reorder-check",
    "app08-weekly-kpi",
    "app11-overdue",
]
# 結果が予定の日時で決まる課題（先週・今日）。scheduled_at の無い版では対象外になる。
DATE_DEPENDENT = ["app03-weekly-summary", "app03-pair", "app08-weekly-kpi", "app11-overdue"]
CLASSIFY = ["app02-triage", "app05-screening-text", "app12-faq-table"]
# このマシンで動かない（RAG・砂箱・Office）か、判定用のモデルが評価の設定に無い（ai.decide）ノード。
EXCLUDED = {
    "ai.decide",
    "rag.search",
    "agent.invoke",
    "sandbox.exec",
    "doc.read",
    "doc.edit",
    "doc.comment",
    "sheet.read",
    "sheet.write",
    "sheet.append",
}
FICTIONAL = ["架空商事", "ほげ省", "Zorblax"]


@pytest.fixture(scope="module")
def tasks():
    return {t.id: t for t in load_tasks()}


def test_g1_the_core_set_loads_and_has_the_twenty_tasks(tasks):
    assert list(tasks) == CORE_IDS


def test_every_task_has_clarifications_flags_and_a_reference(tasks):
    for t in tasks.values():
        assert t.clarifications.strip(), t.id
        assert t.reference_irs(), t.id
        assert any(c.stub_ok for c in t.run_cases), f"{t.id}: stub で流せる run_case が無い"
        for c in t.run_cases:
            assert isinstance(c.llm_dependent, bool) and isinstance(c.stub_ok, bool), (t.id, c.id)


def test_scheduled_tasks_fix_the_date_with_scheduled_at(tasks):
    for tid in SCHEDULED:
        t = tasks[tid]
        assert t.role(None).trigger_kind == "schedule", tid
        for c in t.run_cases:
            assert all(s.scheduled_at for s in c.steps), (tid, c.id)
    weekly = next(s for s in tasks["app03-pair"].run_cases[0].steps if s.workflow == "weekly")
    assert weekly.scheduled_at
    for tid in DATE_DEPENDENT:
        assert "cap:scheduled_at" in tasks[tid].requires, tid


def test_classification_run_cases_are_llm_dependent(tasks):
    for tid in CLASSIFY:
        assert all(c.llm_dependent for c in tasks[tid].run_cases), tid
    # 分類を照合しない課題は、LLM の出力を判定の道に入れない。
    for tid in set(CORE_IDS) - set(CLASSIFY):
        assert not any(c.llm_dependent for c in tasks[tid].run_cases), tid


def test_no_task_needs_what_this_machine_cannot_run(tasks):
    for t in tasks.values():
        for role, ir in t.reference_irs().items():
            used = {n["type"] for n in ir["nodes"]}
            assert not used & EXCLUDED, (t.id, role, used & EXCLUDED)
            assert all(
                f"node:{u}" in t.requires
                or u.startswith("control.")
                or u
                in {
                    "data.record.create",
                    "data.record.update",
                    "data.query",
                    "transform.date",
                    "script.run",
                    "storage.write",
                    "llm.invoke",
                    "data.record.upsert",
                    "data.bulk_upsert",
                    "csv.write",
                    "csv.query",
                }
                for u in used
            ), (t.id, used)
    assert "trigger:event" in tasks["app09-minutes-tasks"].requires
    assert "cap:trigger" in tasks["app09-minutes-tasks"].requires


def test_reference_irs_leave_the_model_to_the_organisation(tasks):
    # 正解は llm.invoke の model を書かない（組織の既定で動く・説明文の推奨）。stub では結合テストが default にする。
    for t in tasks.values():
        for ir in t.reference_irs().values():
            for n in ir["nodes"]:
                assert "model" not in (n.get("params") or {}), (t.id, n["id"])


def test_g2_references_pass_ir_lint_and_the_task_checks(tasks):
    recorded.check_fresh(g.lint_corpus(list(tasks.values())))
    res = g.g2_reference_passes(list(tasks.values()), recorded.catalog(), recorded.lint())
    assert all(r.ok for r in res), [r.to_dict() for r in res if not r.ok]


def test_g4_g5_g7(tasks):
    ts = list(tasks.values())
    out = [g.g4_no_leak(t, recorded.catalog()) for t in ts] + [g.g5_values_named(t) for t in ts]
    out += g.g7_not_duplicated(ts)
    assert all(r.ok for r in out), [r.to_dict() for r in out if not r.ok]


def test_g8_with_fictional_words_and_the_local_list_if_present(
    tasks, tmp_path, monkeypatch, capsys
):
    ts = list(tasks.values())
    assert all(g.g8_fictional(t, [w.casefold() for w in FICTIONAL]).ok for t in ts)
    words, why = g.load_denylist()
    if words is None:
        print(f"\n{why}")
    else:
        bad = [r.to_dict() for r in (g.g8_fictional(t, words) for t in ts) if not r.ok]
        assert not bad, bad


def _baseline(task, case, now) -> CaseResult:
    """中身の無い IR を流したときの状態（fixture の行のまま・ファイルは増えない・1 ノードの出力だけ）。"""
    rows = {}
    for t in task.fixtures.get("tables") or []:
        rows[t["ref"]] = [
            {"id": f"r{i}", "data": resolve_rel(r, now)}
            for i, r in enumerate(t.get("records") or [])
        ]
    statuses = ["succeeded"] * len(case.steps)
    subject = {
        "status": "succeeded",
        "runs": [
            {"status": "succeeded", "input": s.input, "outputs": {"compute": {"ok": True}}}
            for s in case.steps
        ],
        "outputs": {"compute": {"ok": True}},
    }
    return CaseResult(statuses, subject, rows, rows, [], rows)


def test_null_fails_s4_for_every_task(tasks):
    now = dt.datetime(2026, 11, 9, 0, 0, tzinfo=dt.UTC)
    for t in tasks.values():
        per_case = {
            c.id: all(r.ok for r in evaluate(c.expect, _baseline(t, c, now), t.fixtures, now))
            for c in t.run_cases
        }
        assert not all(per_case.values()), f"{t.id}: 何もしない IR が照合を全部通る {per_case}"
        for c in t.run_cases:
            if c.expect.get("flow_through", True):
                assert not per_case[c.id], (
                    f"{t.id}/{c.id}: 何かを変えるはずの run_case を何もしない IR が通る"
                )


def test_fixture_expectations_compute_the_hand_checked_totals(tasks):
    now = dt.datetime(2026, 11, 9, tzinfo=dt.UTC)
    fx = tasks["app03-weekly-summary"].fixtures
    rng = {"field": "report_date", "from": "2026-11-02", "to": "2026-11-08"}
    got = {
        (rep, f): fixture_value(
            {"table": "daily", "op": "sum", "field": f, "where": {"rep": rep}, "range": rng},
            fx,
            now,
        )
        for rep in ("佐藤", "鈴木")
        for f in ("visits", "prospect")
    }
    assert got == {
        ("佐藤", "visits"): 9,
        ("佐藤", "prospect"): 150000,
        ("鈴木", "visits"): 6,
        ("鈴木", "prospect"): 420000,
    }
    kpi = tasks["app08-weekly-kpi"].fixtures
    srng = {"field": "sold_on", "from": "2026-11-02", "to": "2026-11-08"}
    counts = {
        r: fixture_value(
            {"table": "sales", "op": "count", "where": {"region": r}, "range": srng}, kpi, now
        )
        for r in "東西北"
    }
    # 北は 5 件未満なので、集計の仕組みが数値を伏せる（PIT-17・既定の K は 5）。
    assert counts == {"東": 5, "西": 6, "北": 2}
    assert not any("aggregate_min_rows" in t["schema"] for t in kpi["tables"]), (
        "K を下げると北が伏せられない"
    )


def test_offline_irs_use_dummy_uuids_only(tasks):
    for t in tasks.values():
        for ir in offline_irs(t).values():
            assert (
                "@table:" not in str(ir) and "@folder:" not in str(ir) and "@file:" not in str(ir)
            ), t.id
