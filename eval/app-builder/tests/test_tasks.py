"""課題のスキーマ（G1）と置き場所の印。"""

from __future__ import annotations

import copy
import json
import uuid

import pytest

from abeval.tasks import (
    TaskError,
    dummy_ids,
    iter_placeholders,
    load_tasks,
    offline_irs,
    parse_task,
    stub_cases,
    substitute,
)


def task(**over) -> dict:
    t = {
        "id": "t-one",
        "app": None,
        "kind": "workflow",
        "requires": ["node:llm.invoke", "cap:scheduled_at"],
        "phrasings": {
            "flow": "件名を受け取るフロー",
            "app": "件名のアプリ",
            "auto": "件名の自動化",
        },
        "clarifications": "件名だけです。",
        "fixtures": {
            "folders": [{"ref": "out", "name": "要約"}],
            "tables": [{"ref": "tbl", "name": "台帳", "schema": {"fields": []}}],
            "csv": [{"ref": "doc", "name": "a.csv", "text": "a\n1\n"}],
        },
        "expect": {
            "trigger": {"kind": "interactive"},
            "node_types": {"all_of": ["llm.invoke"]},
            "inputs": [
                {"key": "title", "terms": ["件名"]},
                {"key": "note", "terms": ["メモ"], "required": False},
            ],
            "flow_through": ["title"],
        },
        "run_cases": [
            {
                "id": "normal",
                "llm_dependent": False,
                "stub_ok": True,
                "input": {"title": "x"},
                "expect": {
                    "files": [{"folder": "out", "name_regex": "^a"}],
                    "rows": [{"table": "tbl", "count": 0}],
                },
            }
        ],
        "reference": {
            "ir": {
                "nodes": [
                    {
                        "id": "a",
                        "params": {
                            "folder": "@folder:out",
                            "table": "@table:tbl",
                            "file": "@file:doc",
                        },
                    }
                ],
                "edges": [],
            }
        },
    }
    t.update(over)
    return t


def problems(raw: dict) -> list[str]:
    with pytest.raises(TaskError) as e:
        parse_task(raw)
    return e.value.problems


def test_a_valid_task_parses():
    t = parse_task(task())
    assert t.role(None).expect.input_keys == {"title", "note"}
    assert t.role(None).trigger_kind == "interactive"
    assert t.fixture_refs() == {"out": "folder", "tbl": "table", "doc": "file"}
    assert t.run_cases[0].status == "succeeded" and t.run_cases[0].steps[0].input == {"title": "x"}
    assert stub_cases(t) == list(t.run_cases)
    assert t.texts()[-1] == "件名だけです。"


@pytest.mark.parametrize(
    ("change", "needle"),
    [
        (lambda t: t.update(id="Bad_ID"), "id の形"),
        (lambda t: t.update(kind="app"), "kind"),
        (lambda t: t.update(requires=["model:x"]), "requires の形"),
        (lambda t: t["phrasings"].pop("auto"), "phrasings"),
        (lambda t: t.update(clarifications=" "), "clarifications"),
        (lambda t: t.update(extra=1), "知らない項目"),
        (lambda t: t["fixtures"]["tables"].append({"ref": "out", "name": "x"}), "ref が重複"),
        (lambda t: t["fixtures"].update(files=[]), "fixtures に知らない項目"),
        (lambda t: t["expect"].update(trigger={"kind": "schedule", "cron_any": []}), "cron_any"),
        (lambda t: t["expect"]["node_types"].update(maybe=[]), "node_types に知らない項目"),
        (
            lambda t: t["expect"]["inputs"].append({"key": "Title", "terms": ["a"]}),
            "入力の key の形",
        ),
        (lambda t: t["expect"]["inputs"].append({"key": "x", "terms": []}), "terms"),
        (lambda t: t["expect"].update(flow_through=["body"]), "flow_through の 'body'"),
        (lambda t: t["run_cases"][0].pop("stub_ok"), "stub_ok"),
        (lambda t: t["run_cases"][0]["input"].update(other=1), "expect.inputs に無い key"),
        (lambda t: t["run_cases"][0].update(input={}), "必須の入力がありません"),
        (lambda t: t["run_cases"][0]["expect"].update(status="ok"), "status"),
        (
            lambda t: t["run_cases"][0]["expect"]["rows"].append({"table": "out"}),
            "テーブルの ref ではありません",
        ),
        (
            lambda t: t["run_cases"][0]["expect"]["files"].append(
                {"folder": "tbl", "name_regex": "a"}
            ),
            "フォルダーの ref",
        ),
        (lambda t: t["run_cases"][0]["expect"]["files"].append({"name_regex": "("}), "正規表現"),
        (
            lambda t: t["run_cases"][0]["expect"].update(
                checks=[{"op": "equals", "path": "x", "value": 1}]
            ),
            "JSON Pointer",
        ),
        (lambda t: t["run_cases"][0]["expect"].update(no_new_rows="yes"), "no_new_rows"),
        (lambda t: t["run_cases"][0]["expect"].update(flow_through=1), "flow_through は真偽"),
        (
            lambda t: t["run_cases"].append(copy.deepcopy(t["run_cases"][0])),
            "run_case の id が重複",
        ),
        (
            lambda t: t["run_cases"][0].update(sequence=[{"input": {"title": "y"}}]),
            "sequence と input",
        ),
        (lambda t: t.update(reference={}), "reference.ir"),
        (
            lambda t: t["reference"]["ir"]["nodes"][0]["params"].update(x="@table:out"),
            "fixture に無い table",
        ),
        (lambda t: t["reference"]["ir"]["nodes"][0]["params"].update(x="@table:"), "印の形"),
        (lambda t: t.update(roles=[]), "roles は workflow_set だけ"),
    ],
)
def test_shape_errors_are_reported(change, needle):
    raw = task()
    change(raw)
    found = problems(raw)
    assert any(needle in p for p in found), found


def test_all_problems_are_reported_at_once():
    raw = task(kind="nope", clarifications="")
    raw["phrasings"] = {}
    assert len(problems(raw)) >= 3


def _set(**over) -> dict:
    sub = {
        "trigger": {"kind": "interactive"},
        "inputs": [{"key": "a", "terms": ["A"]}],
        "flow_through": ["a"],
    }
    wk = {"trigger": {"kind": "schedule", "cron_any": ["0 9 * * 1"], "tz": "Asia/Tokyo"}}
    raw = task(
        kind="workflow_set",
        roles=[
            {"role": "submit", "trigger_kind": "interactive", "expect": sub},
            {"role": "weekly", "trigger_kind": "schedule", "expect": wk},
        ],
    )
    raw.pop("expect")
    raw["run_cases"] = [
        {
            "id": "seq",
            "llm_dependent": False,
            "stub_ok": True,
            "sequence": [
                {"workflow": "submit", "input": {"a": 1}},
                {"workflow": "weekly", "scheduled_at": "2026-11-09T09:00"},
            ],
            "expect": {},
        }
    ]
    raw["reference"] = {"irs": {"submit": {"nodes": []}, "weekly": {"nodes": []}}}
    raw.update(over)
    return raw


def test_workflow_set():
    t = parse_task(_set())
    assert [r.role for r in t.roles] == ["submit", "weekly"]
    assert [s.workflow for s in t.run_cases[0].steps] == ["submit", "weekly"]
    assert t.run_cases[0].steps[1].scheduled_at == "2026-11-09T09:00"
    assert set(t.reference_irs()) == {"submit", "weekly"}
    raw = _set()
    raw["run_cases"][0]["sequence"][0].pop("workflow")
    assert any("workflow（役割）が要ります" in p for p in problems(raw))
    raw = _set()
    raw["roles"][1]["trigger_kind"] = "interactive"
    assert any("種類を重ねません" in p for p in problems(raw))
    raw = _set(reference={"irs": {"submit": {"nodes": []}}})
    assert any("役割が roles と違います" in p for p in problems(raw))
    raw = _set(expect={})
    assert any("expect を roles に書きます" in p for p in problems(raw))


def test_placeholders_and_substitution():
    t = parse_task(task())
    found = sorted((k, r) for _, k, r in iter_placeholders(t.reference["ir"]))
    assert found == [("file", "doc"), ("folder", "out"), ("table", "tbl")]
    ids = dummy_ids(t)
    assert ids == dummy_ids(t), "同じ ref なら毎回同じ"
    assert all(uuid.UUID(v) for v in ids.values())
    ir = offline_irs(t)["main"]
    assert ir["nodes"][0]["params"] == {
        "folder": ids["out"],
        "table": ids["tbl"],
        "file": ids["doc"],
    }
    assert t.reference["ir"]["nodes"][0]["params"]["folder"] == "@folder:out", "元の課題は変えない"
    with pytest.raises(KeyError, match="fixture に無い"):
        substitute("@table:zz", ids)
    with pytest.raises(KeyError, match="種類が fixture と違います"):
        substitute("@table:out", ids, t.fixture_refs())
    assert substitute(["@table:tbl", {"x": "@other"}], ids) == [ids["tbl"], {"x": "@other"}]


def test_load_tasks_reports_bad_documents(tmp_path):
    p = tmp_path / "x.json"
    p.write_text(json.dumps({"version": 2, "tasks": []}), encoding="utf-8")
    with pytest.raises(TaskError, match="version"):
        load_tasks(p)
    p.write_text(json.dumps({"version": 1, "tasks": [task(), task()]}), encoding="utf-8")
    with pytest.raises(TaskError, match="重複"):
        load_tasks(p)
    p.write_text(json.dumps({"version": 1, "tasks": [task(), task(id="t-two")]}), encoding="utf-8")
    assert [t.id for t in load_tasks(p)] == ["t-one", "t-two"]
