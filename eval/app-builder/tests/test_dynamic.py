"""動的な採点（dead edge・差し込みの残り・入力が届かない・LLM の手前に限る）。合成した記録と、stub のスタックで
記録した run（tests/fixtures/runs/）で確かめる。"""

from __future__ import annotations

import pytest

import recorded
from abeval.grade.dynamic import (
    CaseRecord,
    RunRecord,
    StepRecord,
    before_llm,
    dead_edges,
    downstream,
    grade_dynamic,
    leftovers,
    not_delivered,
)
from abeval.tasks import Expect, InputSpec


def _expect(flow=()) -> Expect:
    return Expect(None, {}, tuple(InputSpec(k, (k,)) for k in flow), tuple(flow))


def _step(node, status="succeeded", ports=("out",), output=None, path=None) -> StepRecord:
    return StepRecord(path or node, node, status, list(ports), output)


def _rec(runs, rows=None, before=None, files=None) -> CaseRecord:
    return CaseRecord(runs, rows or {}, before or {}, files or [])


IR_MAP = {
    "nodes": [
        {"id": "find", "type": "data.query"},
        {"id": "per", "type": "control.map"},
        {"id": "ask", "type": "llm.invoke", "parent": "per"},
        {"id": "save", "type": "data.record.create", "parent": "per"},
        {"id": "after", "type": "storage.write"},
        {"id": "side", "type": "script.run"},
    ],
    "edges": [
        {"from": "find", "to": "per"},
        {"from": "ask", "to": "save"},
        {"from": "per", "to": "after"},
        {"from": "find", "to": "side", "from_port": "error"},
    ],
}


def test_downstream_follows_edges_map_regions_and_the_map_exit():
    assert downstream(IR_MAP, "find") == {"per", "ask", "save", "after", "side"}
    # map の中の LLM の後ろは、領域の中の後続と、map の線の先。
    assert downstream(IR_MAP, "ask") == {"save", "after"}
    assert before_llm(IR_MAP) == {"find", "per", "ask", "side"}
    no_llm = {"nodes": [{"id": "a", "type": "data.query"}], "edges": []}
    assert before_llm(no_llm) == {"a"}


def test_dead_edge_unknown_port_and_untaken_single_exit():
    ir = {
        "nodes": [
            {"id": "a", "type": "llm.invoke"},
            {"id": "b", "type": "storage.write"},
            {"id": "c", "type": "control.branch"},
            {"id": "d", "type": "data.record.create"},
            {"id": "e", "type": "data.record.create"},
            {"id": "f", "type": "data.query", "on_error": "continue"},
            {"id": "g", "type": "script.run"},
        ],
        "edges": [
            {"from": "a", "from_port": "next", "to": "b"},  # (a) 出口に無い
            {"from": "c", "from_port": "true", "to": "d"},
            {"from": "c", "from_port": "false", "to": "e"},  # 分岐の通らなかった出口は見ない
            {"from": "f", "from_port": "error", "to": "g"},  # error は見ない
            {"from": "f", "to": "b"},
        ],
    }  # (b) 出口が 1 つなのに通っていない
    ports = {
        "a": ["out"],
        "b": ["out"],
        "c": ["true", "false"],
        "d": ["out"],
        "e": ["out"],
        "f": ["out", "error"],
        "g": ["out"],
    }
    run = RunRecord(
        "r",
        "succeeded",
        {},
        [
            _step("a"),
            _step("c", ports=["true"]),
            _step("d"),
            _step("f", ports=["error"]),
            _step("g"),
        ],
    )
    found = dead_edges(ir, ports, run)
    assert sorted(f.node_id for f in found) == ["a", "f"], found
    # 成功していない step のノードは見ない・LLM の手前に限ると a の線は見ない。
    assert dead_edges(ir, ports, RunRecord("r", "failed", {}, [_step("a", status="failed")])) == []
    assert [f.node_id for f in dead_edges(ir, ports, run, allowed={"f"})] == ["f"]


def test_leftovers_find_single_brace_names_and_mustache_but_not_json():
    run = RunRecord(
        "r",
        "succeeded",
        {},
        [
            _step("a", output={"text": "回答: 件名: {title} の要約"}),
            _step("b", output={"text": "件名: {{ title }} / {{金額}}"}),
            _step(
                "c",
                output={
                    "items": [{"data": {"a": {"b": 1}}}],
                    "raw": '{"a":{"b":1}}',
                    "set": "{1,2}",
                },
            ),
            _step("d", status="failed", output={"text": "{skip_me}"}),
        ],
    )
    rows = {
        "t": [{"id": "1", "data": {"note": "{leak}"}}, {"id": "2", "data": {"note": "{fixture}"}}]
    }
    before = {"t": [{"id": "2", "data": {"note": "{fixture}"}}]}
    files = [{"folder": None, "name": "要約-{date}.md", "text": "ok"}]
    found = leftovers(_rec([run], rows, before, files))
    hits = sorted(f.detail.split(" に ")[1] for f in found)
    assert hits == [
        "'{date}' が残る",
        "'{leak}' が残る",
        "'{title}' が残る",
        "'{{ title }}' が残る",
        "'{{金額}}' が残る",
    ], hits
    # LLM の手前に限ると、行とファイルは見ない。
    only = leftovers(_rec([run], rows, before, files), allowed={"a"})
    assert [f.node_id for f in only] == ["a"]


def test_not_delivered_checks_llm_writes_rows_and_files():
    ir = {
        "nodes": [
            {"id": "ask", "type": "llm.invoke"},
            {"id": "w", "type": "data.record.create"},
            {"id": "s", "type": "script.run"},
        ],
        "edges": [],
    }
    exp = _expect(["title", "amount", "names", "flag"])
    run = RunRecord(
        "r",
        "succeeded",
        {"title": "会議室", "amount": 80000, "names": ["田中", "山本"], "flag": True},
        [
            _step("ask", output={"text": "回答: 件名: 会議室の予約"}),
            _step("s", output={"names": ["田中", "山本"], "amount": 80000}),
        ],
    )
    found = not_delivered(ir, exp, _rec([run]), run)
    # script の出力は届いた先に数えない。金額・田中・山本が届いていない（真偽は見ない）。
    assert sorted(f.detail.split("=")[0] for f in found) == [
        "入力 amount",
        "入力 names",
        "入力 names",
    ]
    rows = {"t": [{"id": "1", "data": {"amount": "80,000", "who": "田中"}}]}
    files = [{"folder": None, "name": "x.md", "text": "参加者: 山本"}]
    assert not_delivered(ir, exp, _rec([run], rows, {}, files), run) == []
    # LLM の手前に限ると、行とファイルは見ない。
    assert len(not_delivered(ir, exp, _rec([run], rows, {}, files), run, allowed={"ask"})) == 3
    # 書き込みの出力も届いた先に数える。
    run2 = RunRecord(
        "r", "succeeded", {"amount": 80000}, [_step("w", output={"data": {"amount": 80000}})]
    )
    assert not_delivered(ir, _expect(["amount"]), _rec([run2]), run2) == []


def _grade(task_id: str, variant: str, case_id: str):
    task = recorded.tasks()[task_id]
    case = next(c for c in task.run_cases if c.id == case_id)
    irs, ports, rec = recorded.run(task_id, variant, case_id)
    return grade_dynamic(
        irs,
        ports,
        {r.role: r.expect for r in task.roles},
        rec,
        restrict=case.llm_dependent,
        delivery=case.expect.get("flow_through", True),
    )


@pytest.mark.parametrize("key", [r for r in recorded.runs() if r[1] == "oracle"], ids="/".join)
def test_recorded_oracle_runs_are_clean(key):
    g = _grade(*key)
    assert not g.s2d, g.to_dict()
    assert g.s3, g.statuses


def test_recorded_triage_is_limited_to_before_the_llm():
    task = recorded.tasks()["app02-triage"]
    irs, _, rec = recorded.run("app02-triage", "oracle", "billing")
    assert before_llm(irs["main"]) == {"register", "classify"}
    g = _grade("app02-triage", "oracle", "billing")
    assert g.restricted and not g.s2d
    # 制限を外すと、stub の分類（形に合う最小の値）の後ろまで見ることになる（ここでは誤りは出ないが範囲が違う）。
    assert (
        grade_dynamic(
            irs, {}, {r.role: r.expect for r in task.roles}, rec, restrict=False
        ).restricted
        is False
    )


@pytest.mark.parametrize("key", [r for r in recorded.runs() if r[1] == "null"], ids="/".join)
def test_recorded_null_runs_do_not_deliver_inputs(key):
    task = recorded.tasks()[key[0]]
    case = next(c for c in task.run_cases if c.id == key[2])
    g = _grade(*key)
    wants = bool(task.role(None).expect.flow_through) and case.expect.get("flow_through", True)
    assert ("dyn:not_delivered" in g.detectors()) == wants, g.to_dict()


def test_case_record_round_trips_and_builds_the_subject():
    _, _, rec = recorded.run("basic-summary-file", "oracle", "normal")
    again = CaseRecord.from_dict(rec.to_dict())
    assert again == rec
    res = rec.case_result()
    assert res.statuses == ["succeeded"]
    assert res.subject["outputs"]["save"]["name"].startswith("要約-")
    assert res.subject["runs"][0]["input"]["title"] == "会議室の予約方法"
