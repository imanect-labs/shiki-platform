"""S2d（保存できたが実行で壊れている）の検出と、基盤の失敗を採点しないこと（GAP-30 §5.1・§5.3・§4.8）。

- 「入力が届かない」は stub の run だけで見る（実モデルの出力は入力の全文を含まない）。
- stub で流した llm_dependent でない run_case の状態が期待と違えば S2d（想定外の状態）。
- 成功した run の中で、分岐でも error の出口でもない線の先が飛ばされたら S2d（想定外の skipped）。
- 基盤の失敗で流せなかった run_case があれば、S2d・S3・S4 と見出しを決めない（None）。
"""

from __future__ import annotations

import copy
import datetime as dt

import pytest

import recorded
from abeval import grading
from abeval.grade import dynamic
from abeval.grade.dynamic import CaseRecord, RunRecord, StepRecord, unexpected_skips
from abeval.run_cases import grade_case

SUMMARY = "・新しい予約表で受け付ける\n・前日の 17 時までに入れる\n・取り消しも予約表で行う"


def _case(task_id: str, case_id: str):
    task = recorded.tasks()[task_id]
    return task, next(c for c in task.run_cases if c.id == case_id)


def _now(rec: CaseRecord) -> dt.datetime:
    return dt.datetime.fromisoformat(rec.now)


def _real_model_like(rec: CaseRecord) -> CaseRecord:
    """stub の「回答: <prompt>」を、実モデルが書きそうな要約（入力の本文を含まない）に置き換えた記録。"""
    rec = copy.deepcopy(rec)
    for run in rec.runs:
        for s in run.steps:
            if s.node_id == "summarize":
                s.output = {**s.output, "model": "claude-haiku-5-5", "text": SUMMARY}
    for f in rec.files:
        f["text"] = f"# {rec.runs[0].input['title']}\n\n{SUMMARY}"
    return rec


def test_real_model_output_is_not_judged_by_delivery():
    task, case = _case("basic-summary-file", "normal")
    irs, ports, rec = recorded.run("basic-summary-file", "oracle", "normal")
    real = _real_model_like(rec)
    # Mode B の実行（stub=False）: 正しく作った IR は S2d にならず、照合も通る。
    g = grade_case(task, case, real, irs, ports, stub=False, now=_now(rec))
    assert g["s2d"] is False and g["s4"] is True, g["dynamic"]
    assert grading.headline_b(True, False, g["s2d"], g["s4"]) is True
    # 同じ記録を stub の run として見ると、本文が届いていない（stub なら届くはず）ので S2d。
    g_stub = grade_case(task, case, real, irs, ports, stub=True, now=_now(rec))
    assert {f["code"] for f in g_stub["dynamic"]["findings"]} == {"not_delivered"}


def _failed_first(rec: CaseRecord) -> CaseRecord:
    """最初のノードが失敗して、後ろが何も動かない run。"""
    rec = copy.deepcopy(rec)
    run = rec.runs[0]
    first = run.steps[0]
    run.status = "failed"
    run.fail_reason = "step_failed"
    run.steps = [StepRecord(first.step_path, first.node_id, "failed", [], None, {"code": "x"})]
    rec.rows = copy.deepcopy(rec.rows_before)
    rec.files = []
    return rec


@pytest.mark.parametrize(
    ("task_id", "case_id"),
    [("app03-weekly-summary", "last_week"), ("app11-overdue", "morning")],
)
def test_a_run_that_fails_is_s2d_even_without_flow_through(task_id, case_id):
    task, case = _case(task_id, case_id)
    assert not task.role(None).expect.flow_through  # 入力の無い課題（届かないでは捕まらない）
    irs, ports, rec = recorded.run(task_id, "oracle", case_id)
    g = grade_case(task, case, _failed_first(rec), irs, ports, stub=True, now=_now(rec))
    assert g["s3"] is False and g["s2d"] is True
    assert [f["code"] for f in g["dynamic"]["findings"]] == ["unexpected_status"]
    # Mode A の見出しは偽になる。
    assert grading.headline(True, False, g["s2d"]) is False


def test_status_any_and_llm_dependent_cases_do_not_check_the_status():
    task, case = _case("app06-stock-move", "short")
    assert case.status == "any"
    irs, ports, rec = recorded.run("app06-stock-move", "oracle", "short")
    g = grade_case(task, case, _failed_first(rec), irs, ports, stub=True, now=_now(rec))
    assert "unexpected_status" not in {f["code"] for f in g["dynamic"]["findings"]}
    task, case = _case("app02-triage", "billing")
    irs, ports, rec = recorded.run("app02-triage", "oracle", "billing")
    g = grade_case(task, case, _failed_first(rec), irs, ports, stub=True, now=_now(rec))
    # stub の出力で決まる道の状態は見ない（LLM まで届かないことは「入力が届かない」で捕まる）。
    assert {f["code"] for f in g["dynamic"]["findings"]} == {"not_delivered"}


# --- 想定外の skipped ---

IR = {
    "nodes": [
        {"id": "a", "type": "data.query", "on_error": "continue"},
        {"id": "b", "type": "storage.write"},
        {"id": "c", "type": "data.record.create"},
        {"id": "h", "type": "script.run"},
        {"id": "br", "type": "control.branch"},
        {"id": "t", "type": "data.record.create"},
        {"id": "f", "type": "data.record.create"},
    ],
    "edges": [
        {"from": "a", "to": "b"},
        {"from": "b", "to": "c"},
        {"from": "a", "from_port": "error", "to": "h"},
        {"from": "c", "to": "br"},
        {"from": "br", "from_port": "true", "to": "t"},
        {"from": "br", "from_port": "false", "to": "f"},
    ],
}
PORTS = {
    "a": ["out", "error"],
    "b": ["out"],
    "c": ["out"],
    "h": ["out"],
    "br": ["true", "false"],
    "t": ["out"],
    "f": ["out"],
}


def _run(statuses: dict[str, tuple[str, list[str]]], status: str = "succeeded") -> RunRecord:
    return RunRecord(
        "r",
        status,
        {},
        [StepRecord(n, n, st, ports) for n, (st, ports) in statuses.items()],
    )


def test_branch_and_error_exits_are_expected_skips():
    run = _run(
        {
            "a": ("succeeded", ["out"]),
            "b": ("succeeded", ["out"]),
            "c": ("succeeded", ["out"]),
            "h": ("skipped", []),  # error の出口は通らないのが普通
            "br": ("succeeded", ["true"]),
            "t": ("succeeded", ["out"]),
            "f": ("skipped", []),  # 分岐の通らなかった側
        }
    )
    assert unexpected_skips(IR, PORTS, run) == []


def test_a_continued_failure_skips_downstream_and_only_the_root_is_counted():
    run = _run(
        {
            "a": ("failed", ["error"]),  # on_error: continue で失敗した（run は成功のまま）
            "h": ("succeeded", ["out"]),
            "b": ("skipped", []),
            "c": ("skipped", []),
            "br": ("skipped", []),
            "t": ("skipped", []),
            "f": ("skipped", []),
        }
    )
    found = unexpected_skips(IR, PORTS, run)
    assert [(f.code, f.node_id) for f in found] == [("unexpected_skip", "b")]
    assert "a は failed" in found[0].detail
    # LLM の手前に限るとき、元（a）が範囲の外なら数えない。
    assert unexpected_skips(IR, PORTS, run, allowed={"b", "c"}) == []
    # 失敗した run・期待が succeeded でない run_case では見ない。
    assert unexpected_skips(IR, PORTS, _run({"b": ("skipped", [])}, status="failed")) == []


def test_skips_in_map_elements_are_scoped():
    ir = {
        "nodes": [
            {"id": "m", "type": "control.map"},
            {"id": "x", "type": "data.query", "parent": "m", "on_error": "continue"},
            {"id": "y", "type": "data.record.create", "parent": "m"},
        ],
        "edges": [{"from": "x", "to": "y"}],
    }
    ports = {"m": ["out"], "x": ["out", "error"], "y": ["out"]}
    run = RunRecord(
        "r",
        "succeeded",
        {},
        [
            StepRecord("m", "m", "succeeded", ["out"]),
            StepRecord("m[0].x", "x", "succeeded", ["out"]),
            StepRecord("m[0].y", "y", "succeeded", ["out"]),
            StepRecord("m[1].x", "x", "failed", ["error"]),
            StepRecord("m[1].y", "y", "skipped", []),
        ],
    )
    found = unexpected_skips(ir, ports, run)
    assert [f.detail.split(" ")[0] for f in found] == ["m[1].y"]


def test_v0_from_port_next_is_also_an_unexpected_skip():
    from test_mutate import _v0_from_port_next

    task, irs, ports, rec = _v0_from_port_next("basic-summary-file", "normal")
    case = next(c for c in task.run_cases if c.id == "normal")
    g = grade_case(task, case, rec, irs, ports, stub=True, now=_now(rec))
    codes = {f["code"] for f in g["dynamic"]["findings"]}
    assert {"dead_edge", "unexpected_skip"} <= codes


@pytest.mark.parametrize("name", ["unexpected_status", "unexpected_skips"])
def test_disabling_the_runtime_detectors_lets_breakage_through(monkeypatch, name):
    task, case = _case("app03-weekly-summary", "last_week")
    irs, ports, rec = recorded.run("app03-weekly-summary", "oracle", "last_week")
    broken = _failed_first(rec)
    if name == "unexpected_skips":
        # 最初のノードが on_error: continue で失敗し、run は成功のまま後ろが飛ばされた形。
        broken.runs[0].status = "succeeded"
        broken.runs[0].steps[0].taken_ports = ["error"]
        broken.runs[0].steps += [
            StepRecord(n["id"], n["id"], "skipped", [])
            for n in irs["main"]["nodes"][1:]
            if not n.get("parent")
        ]
    g = grade_case(task, case, broken, irs, ports, stub=True, now=_now(rec))
    assert g["s2d"] is True
    monkeypatch.setattr(dynamic, name, lambda *a, **k: [])
    g = grade_case(task, case, broken, irs, ports, stub=True, now=_now(rec))
    assert g["s2d"] is False, "検査を止めると壊れた実行が見逃される（検査が効いている）"


# --- 基盤の失敗 ---


def test_a_case_that_could_not_run_leaves_everything_undecided():
    ok = {"case": "a", "s2d": False, "s3": True, "s4": True}
    out = grading._summary([ok, {"case": "b", "error": "TimeoutError: 180 秒"}])
    assert (out["s2d"], out["s3"], out["s4"]) == (None, None, None)
    assert "b: TimeoutError" in out["error"]
    assert grading._summary([{"case": "a", "error": "TimeoutError: x"}])["s2d"] is None
    with pytest.raises(grading.InfraFailure) as e:
        grading.raise_if_infra("stub_plumbing", out)
    assert e.value.kind == "stub_plumbing"
    grading.raise_if_infra("run_case", grading._summary([ok]))  # 失敗が無ければ何もしない


@pytest.mark.parametrize(
    ("s1", "s2s", "s2d", "want"),
    [
        (True, False, False, True),
        (True, False, None, None),  # stub の配管が決まらない（真にしない）
        (True, None, False, None),
        (True, False, True, False),
        (True, True, None, False),
        (False, None, None, False),
    ],
)
def test_headline_is_true_only_when_decided(s1, s2s, s2d, want):
    assert grading.headline(s1, s2s, s2d) is want


def test_mode_b_headline_needs_s4():
    assert grading.headline_b(True, False, False, True) is True
    assert grading.headline_b(True, False, False, None) is None
    assert grading.headline_b(True, False, False, False) is False
    assert grading.headline_b(True, False, None, True) is None
    assert grading.headline_b(False, None, None, False) is False
