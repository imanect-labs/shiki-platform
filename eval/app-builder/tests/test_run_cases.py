"""run_case の実行（abeval.run_cases）。サーバーの代わりに、口の形だけをまねた偽のクライアントで流す。"""

from __future__ import annotations

import datetime as dt

import pytest

from abeval import fixtures
from abeval.run_cases import CaseError, execute_case, grade_case, prepare_ir, save_workflows
from abeval.shiki import ShikiError
from abeval.tasks import parse_task

NOW = dt.datetime(2026, 11, 9, tzinfo=dt.UTC)


def _task(**over):
    raw = {
        "id": "t-run",
        "kind": "workflow",
        "requires": [],
        "phrasings": {"flow": "a", "app": "b", "auto": "c"},
        "clarifications": "x",
        "fixtures": {
            "folders": [{"ref": "out", "name": "出力"}],
            "tables": [
                {"ref": "tbl", "name": "台帳", "schema": {"fields": []}, "records": [{"v": 1}]}
            ],
        },
        "expect": {
            "inputs": [
                {"key": "item", "terms": ["品目"]},
                {"key": "folder", "terms": ["フォルダ"], "required": False},
            ],
            "flow_through": ["item"],
        },
        "run_cases": [
            {
                "id": "two",
                "llm_dependent": False,
                "stub_ok": True,
                "sequence": [
                    {"input": {"item": "机", "folder": "@id:out"}},
                    {"input": {"item": "椅子"}, "scheduled_at": "2026-11-09T09:00"},
                ],
                "expect": {
                    "rows": [{"table": "tbl", "count": 3}],
                    "files": [{"folder": None, "name_regex": "^a"}],
                    "no_new_rows": True,
                },
            }
        ],
        "reference": {
            "ir": {
                "name": "x",
                "nodes": [
                    {"id": "ask", "type": "llm.invoke", "params": {"prompt": "p", "effort": "low"}},
                    {"id": "pick", "type": "ai.decide", "params": {}},
                    {
                        "id": "w",
                        "type": "data.record.create",
                        "params": {
                            "table": "@table:tbl",
                            "data": {"item": {"$from": "input", "path": "/item"}},
                        },
                    },
                ],
                "edges": [{"from": "ask", "to": "pick"}, {"from": "pick", "to": "w"}],
            }
        },
    }
    raw.update(over)
    return parse_task(raw)


class FakeClient:
    """execute_case と save_workflows が使う口だけを持つ偽物。起動のたびに行を 1 つ足し、最初の起動でルートにファイルを置く。"""

    user = "alice"

    def __init__(self, *, start_error: int | None = None, validate_errors=None):
        self.rows: dict[str, list] = {}
        self.nodes_by: dict[str | None, list] = {None: []}
        self.started: list[dict] = []
        self.n = 0
        self.start_error = start_error
        self.validate_errors = validate_errors or []
        self.saved: list[dict] = []

    # fixture・番兵
    def node_by_name(self, name, parent_id=None):
        return (
            {"id": "sentinel", "kind": "folder", "name": name}
            if name == fixtures.SENTINEL_NAME
            else None
        )

    def create_folder(self, name, parent_id=None):
        fid = f"folder-{len(self.nodes_by)}"
        self.nodes_by[fid] = []
        return {"id": fid, "name": name}

    def create_table(self, name, schema):
        self.rows["tbl"] = []
        return {"id": "tbl"}

    def create_record(self, table_id, data):
        self.n += 1
        row = {"id": f"row{self.n}", "rev": 1, "data": dict(data)}
        self.rows[table_id].append(row)
        return row

    def records(self, table_id):
        return list(self.rows[table_id])

    def delete_record(self, table_id, record_id, rev, missing_ok=False):
        self.rows[table_id] = [r for r in self.rows[table_id] if r["id"] != record_id]
        return True

    def nodes(self, parent_id=None):
        return list(self.nodes_by.get(parent_id, []))

    def file_text(self, file_id):
        if file_id == "bin":
            raise ShikiError("GET", "/files/bin/text", 415, "")
        return {"text": f"text of {file_id}"}

    # ワークフロー
    def validate(self, ir):
        return {
            "errors": self.validate_errors,
            "node_ports": {n["id"]: ["out"] for n in ir["nodes"]},
        }

    def create_workflow(self, ir):
        self.saved.append(ir)
        return {"id": f"wf{len(self.saved)}", "version": 1}

    def start_run(self, wf_id, input, *, trigger=None, scheduled_at=None):
        if self.start_error:
            raise ShikiError("POST", "/runs", self.start_error, '{"code":"invalid_input"}')
        self.started.append({"wf": wf_id, "input": dict(input), "scheduled_at": scheduled_at})
        self.create_record("tbl", {"item": input.get("item")})
        if len(self.started) == 1:
            self.nodes_by[None].append({"id": "f1", "name": "a.md", "kind": "file"})
            self.nodes_by[None].append({"id": "bin", "name": "a.bin", "kind": "file"})
        return f"run{len(self.started)}"

    def wait_run(self, wf_id, run_id, timeout=180.0):
        return {
            "status": "succeeded",
            "input": self.started[-1]["input"],
            "scheduled_at": None,
            "steps": [
                {
                    "step_path": "w",
                    "node_id": "w",
                    "status": "succeeded",
                    "taken_ports": ["out"],
                    "has_output": True,
                },
                {
                    "step_path": "x",
                    "node_id": "x",
                    "status": "skipped",
                    "taken_ports": [],
                    "has_output": False,
                },
            ],
        }

    def get_step(self, wf_id, run_id, path):
        return {"output": {"data": {"item": self.started[-1]["input"].get("item")}}}


def _setup(client=None):
    c = client or FakeClient()
    t = _task()
    rec = fixtures.create_fixtures(c, t.fixtures, now=NOW)
    return c, t, rec


def test_prepare_ir_substitutes_places_and_switches_llm_nodes_to_the_stub():
    c, t, rec = _setup()
    ir = prepare_ir(t.reference["ir"], rec, t, name="abeval-x", stub=True)
    assert ir["name"] == "abeval-x"
    assert ir["nodes"][0]["params"] == {"prompt": "p", "model": "default"}
    assert ir["nodes"][1]["params"]["model"] == "decide-stub"
    assert ir["nodes"][2]["params"]["table"] == rec.refs["tbl"]
    real = prepare_ir(t.reference["ir"], rec, t, name="abeval-x", stub=False)
    assert (
        "model" not in real["nodes"][0]["params"] and real["nodes"][0]["params"]["effort"] == "low"
    )
    assert t.reference["ir"]["nodes"][0]["params"] == {"prompt": "p", "effort": "low"}, (
        "元の課題は変えない"
    )


def test_save_workflows_records_ids_and_refuses_invalid_irs():
    c, t, rec = _setup()
    saved = save_workflows(c, t, t.reference_irs(), rec, tag="ref-1", stub=True)
    assert saved["main"].id == "wf1" and saved["main"].node_ports["w"] == ["out"]
    assert ("workflow", "wf1", "abeval-ref-1-main") in [(x.kind, x.id, x.name) for x in rec.created]
    c2, t2, rec2 = _setup(FakeClient(validate_errors=[{"code": "ir.bad_ref"}]))
    with pytest.raises(CaseError, match="検証を通りません"):
        save_workflows(c2, t2, t2.reference_irs(), rec2, tag="ref-2", stub=True)


def test_execute_case_runs_the_sequence_and_collects_rows_files_and_the_rerun():
    c, t, rec = _setup()
    saved = save_workflows(c, t, t.reference_irs(), rec, tag="ref-1", stub=True)
    crec = execute_case(c, t, t.run_cases[0], saved, rec, now=NOW)
    # 行は入れ直してから流す（fixture の 1 行 + 起動 2 回）。最後の起動をもう一度流して行を数え直す。
    assert [s["input"]["item"] for s in c.started] == ["机", "椅子", "椅子"]
    assert c.started[0]["input"]["folder"] == rec.refs["out"], "@id: を fixture の id にする"
    assert c.started[1]["scheduled_at"] == "2026-11-09T09:00"
    assert (
        len(crec.rows_before["tbl"]) == 1
        and len(crec.rows["tbl"]) == 3
        and len(crec.rows_rerun["tbl"]) == 4
    )
    assert [r.status for r in crec.runs] == ["succeeded", "succeeded"]
    assert (
        crec.runs[0].steps[0].output == {"data": {"item": "机"}}
        and crec.runs[0].steps[1].output is None
    )
    # ルートに増えたファイル（中身を読めないものも）を集め、片付けの記録に足す。
    assert {(f["folder"], f["name"], f["text"]) for f in crec.files} == {
        (None, "a.md", "text of f1"),
        (None, "a.bin", None),
    }
    assert {"f1", "bin"} <= set(rec.ids("file"))
    g = grade_case(
        t,
        t.run_cases[0],
        crec,
        {"main": saved["main"].ir},
        {"main": saved["main"].node_ports},
        stub=True,
        now=NOW,
    )
    assert g["s3"] and not g["s2d"]
    # 再実行で行が増えるので no_new_rows は落ちる（それ以外の照合は通る）。
    assert g["s4"] is False
    assert [x["kind"] for x in g["checks"] if not x["ok"]] == ["no_new_rows"]


def test_runs_that_are_refused_are_not_started():
    c, t, rec = _setup(FakeClient(start_error=400))
    saved = save_workflows(c, t, t.reference_irs(), rec, tag="ref-1", stub=True)
    crec = execute_case(c, t, t.run_cases[0], saved, rec, now=NOW)
    assert [r.status for r in crec.runs] == ["not_started", "not_started"]
    g = grade_case(t, t.run_cases[0], crec, {"main": saved["main"].ir}, {}, stub=True, now=NOW)
    assert not g["s3"] and g["failed_steps"][0]["run"] == "not_started"
    c5, t5, rec5 = _setup(FakeClient(start_error=500))
    saved5 = save_workflows(c5, t5, t5.reference_irs(), rec5, tag="ref-5", stub=True)
    with pytest.raises(ShikiError):
        execute_case(c5, t5, t5.run_cases[0], saved5, rec5, now=NOW)


def test_llm_dependent_cases_on_the_stub_are_not_matched_unless_forced():
    c, t, rec = _setup()
    case = t.run_cases[0]
    dep = type(case)(case.id, case.steps, True, True, case.expect)
    saved = save_workflows(c, t, t.reference_irs(), rec, tag="ref-1", stub=True)
    crec = execute_case(c, t, dep, saved, rec, now=NOW)
    ir, ports = {"main": saved["main"].ir}, {"main": saved["main"].node_ports}
    g = grade_case(t, dep, crec, ir, ports, stub=True, now=NOW)
    assert g["s4"] is None and g["checks"] is None and g["dynamic"]["restricted_to_before_llm"]
    assert grade_case(t, dep, crec, ir, ports, stub=True, now=NOW, force_checks=True)["s4"] is False
    assert grade_case(t, dep, crec, ir, ports, stub=False, now=NOW)["s4"] is False


def test_unknown_roles_are_a_case_error():
    c, t, rec = _setup()
    with pytest.raises(CaseError, match="役割"):
        execute_case(c, t, t.run_cases[0], {}, rec, now=NOW)
