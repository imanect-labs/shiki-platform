"""abeval.feasibility（版で課題が成り立つか・D-167）と、cli がそれで課題を対象外にすること。"""

from __future__ import annotations

import json

import pytest

from abeval import cli, feasibility
from abeval.grade import GRADER_VERSION
from abeval.tasks import load_tasks

TASKS = load_tasks()


def test_drop_x_order_removes_the_annotation_at_every_level():
    ir = {
        "input_schema": {
            "type": "object",
            "x-order": ["a", "b"],
            "properties": {"a": {"type": "object", "x-order": ["c"], "properties": {}}},
        },
        "nodes": [{"params": {"x-order": 1}}],
    }
    out = feasibility.drop_x_order(ir)
    assert "x-order" not in json.dumps(out)
    assert out["input_schema"]["properties"]["a"]["type"] == "object"
    assert "x-order" in json.dumps(ir), "元の IR は変えない"


def _case(**kw):
    base = {"case": "c1", "dynamic": {"findings": []}, "checks": [], "failed_steps": []}
    base.update(kw)
    return base


def test_judge_reads_the_stub_plumbing_result():
    assert feasibility.judge({"s2d": False, "s4": True, "cases": [_case()]}) is None
    # llm_dependent だけの課題は照合をしない（s4 が None）。成り立つとみなす。
    assert feasibility.judge({"s2d": False, "s4": None, "cases": [_case()]}) is None
    assert feasibility.judge({"skipped": "stub_ok の run_case が無い"}) is None
    saved = feasibility.judge(
        {"error": "CaseError: t/main: 検証を通りません: [{'code': 'ir.unknown_node_type'}]"}
    )
    assert saved and saved.startswith("正解がこの版で保存できない")
    failed = feasibility.judge(
        {
            "s2d": True,
            "s4": False,
            "cases": [
                _case(
                    dynamic={"findings": [{"code": "unexpected_status", "detail": "failed"}]},
                    checks=[{"kind": "rows:count", "ok": False, "detail": "0 行"}],
                    failed_steps=[{"node_id": "log", "error": {"message": "値式が解決できません"}}],
                )
            ],
        }
    )
    assert failed.startswith("正解がこの版で通らない")
    assert "unexpected_status" in failed and "rows:count" in failed and "log" in failed
    # 照合だけが落ちても成り立たない。
    assert feasibility.judge({"s2d": False, "s4": False, "cases": [_case()]})


@pytest.mark.parametrize(
    "err", ["TimeoutError: run x が 180 秒で終わりません", "ShikiError: POST /workflows → 502"]
)
def test_infra_errors_stop_instead_of_marking_the_task_infeasible(err):
    with pytest.raises(feasibility.FeasibilityError):
        feasibility.judge({"error": err})


def test_cache_needs_the_same_grader_and_task_set():
    result = {t.id: None for t in TASKS}
    result[TASKS[0].id] = "正解がこの版で保存できない: x"
    extra = feasibility.record_fields({}, result, TASKS)
    want = [t.id for t in TASKS]
    assert feasibility.cached(extra, TASKS, want) == result
    # 課題を絞って流しても、全体の記録から読める。
    assert feasibility.cached(extra, TASKS, want[:2]) == {i: result[i] for i in want[:2]}
    # 課題集が変われば（課題を 1 つ外した）、確かめ直す。
    assert feasibility.cached(extra, TASKS[1:], want[1:]) is None
    # 採点器の版が違えば、確かめ直す。
    stale = {"feasible": {**extra["feasible"], "grader": GRADER_VERSION - 1}}
    assert feasibility.cached(stale, TASKS, want) is None
    # 記録に無い課題があれば、確かめ直す。
    partial = feasibility.record_fields({}, {want[0]: None}, TASKS)
    assert feasibility.cached(partial, TASKS, want) is None
    # 同じ採点器と課題集なら、後から確かめた分を足す。
    merged = feasibility.record_fields(partial, {want[1]: "x"}, TASKS)
    assert merged["feasible"]["result"] == {want[0]: None, want[1]: "x"}
    # 採点器の版が違う記録には足さず、置き換える。
    replaced = feasibility.record_fields(stale, {want[1]: None}, TASKS)
    assert replaced["feasible"]["result"] == {want[1]: None}


class _Rec:
    def __init__(self, extra):
        self.extra = extra


def test_cli_probes_once_records_the_result_and_reads_it_back(monkeypatch):
    calls = []
    stored = {}

    def probe(client, tasks, lint, catalog):
        calls.append([t.id for t in tasks])
        return {
            t.id: ("正解がこの版で保存できない: x" if t.id == TASKS[0].id else None) for t in tasks
        }

    monkeypatch.setattr(cli, "in_scope", lambda tasks, *a, **k: (list(tasks), []))
    monkeypatch.setattr(cli, "read_record", lambda label: _Rec(dict(stored)))
    monkeypatch.setattr(cli, "update_record", lambda label, fields: stored.update(fields))
    monkeypatch.setattr(feasibility, "probe", probe)
    first = cli._infeasible("v0", None, None, None, {}, None)
    assert first[TASKS[0].id] and not any(first[t.id] for t in TASKS[1:])
    assert len(calls) == 1 and stored["feasible"]["grader"] == GRADER_VERSION
    again = cli._infeasible("v0", None, None, None, {}, None)
    assert again == first and len(calls) == 1, "記録に残した結果を読み直し、流し直さない"


def test_cli_stops_on_infra_errors(monkeypatch):
    def probe(*a, **k):
        raise feasibility.FeasibilityError("t: 正解を流せません（基盤の失敗）")

    monkeypatch.setattr(cli, "in_scope", lambda tasks, *a, **k: (list(tasks), []))
    monkeypatch.setattr(cli, "read_record", lambda label: _Rec({}))
    monkeypatch.setattr(cli, "update_record", lambda *a: pytest.fail("記録を書いた"))
    monkeypatch.setattr(feasibility, "probe", probe)
    assert cli._infeasible("v0", None, None, None, {}, None) is None
