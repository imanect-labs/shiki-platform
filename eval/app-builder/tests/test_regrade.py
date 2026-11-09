"""採点し直し（`abeval.regrade`）。stub の配管と静的な採点を差し替えて、出し直すものと残すものを確かめる。

- Mode A: S2d・S3 を stub の配管で出し直す（BUG-267 の map の項目の失敗が見出しを落とす）。S1 は残す。
- Mode B: S2d は「実モデルの run_case の S2d（残す）∨ stub の配管の S2d（出し直す）」。S3・S4・runs は残し、残したことを
  試行の `regraded` に書く（BUG-268 の誤検知が直ると見出しが通る）。
- 基盤の失敗で流せなかった試行が 1 つでもあれば、結果を書かない。静的な層だけの採点し直しは、採点の規則の版が
  結果と同じときだけ。
"""

from __future__ import annotations

import copy

import pytest

from abeval import cli, regrade, report
from abeval.fixtures import FixtureRecord
from abeval.grade import GRADER_VERSION
from abeval.grading import InfraFailure
from abeval.tasks import load_tasks

TASKS = {t.id: t for t in load_tasks()}
PREV = {"grader": GRADER_VERSION - 1, "ir_lint_schema": 3, "ir_lint_sha256": "aa"}
IR = {"main": {"nodes": [], "edges": []}}


def _case(code: str | None) -> dict:
    findings = [{"code": code, "node_id": None, "detail": "x"}] if code else []
    return {
        "case": "c",
        "s2d": bool(code),
        "s3": True,
        "s4": None,
        "dynamic": {"findings": findings},
    }


def _stub(code: str | None) -> dict:
    return {"s2d": bool(code), "s3": True, "s4": None, "cases": [_case(code)]}


def _trial(mode: str, task: str, n: int, **kw) -> dict:
    t = {
        "trial_id": f"{task}.flow.{n}.medium",
        "key": f"k{n:07d}" + "0" * 56,
        "label": "vf",
        "mode": mode,
        "task": task,
        "phrasing": "flow",
        "n": n,
        "status": "emit",
        "irs": copy.deepcopy(IR),
        "s1": True,
        "s2s": False,
        "s2d": False,
        "s3": True,
        "stub": _stub(None),
        "headline": True,
    }
    if mode == "b":
        t.update({"runs": {"s2d": False, "s3": True, "s4": True, "cases": []}, "s4": True})
    t.update(kw)
    return t


@pytest.fixture
def plumbing(monkeypatch):
    """試行の n → 流し直した stub の配管の結果（例外なら基盤の失敗）。"""
    results: dict[int, object] = {}
    calls: list[str] = []

    def fake_stub(client, task, rec, irs, *, tag, lint_ports, now, timeout):
        n = int(tag[1:])
        calls.append(task.id)
        r = results.get(n, _stub(None))
        if isinstance(r, Exception):
            raise r
        return r

    monkeypatch.setattr(regrade, "stub_plumbing", fake_stub)
    monkeypatch.setattr(
        regrade,
        "static_roles",
        lambda task, irs, lint, catalog, tag: {
            "s2s": False,
            "s2w": False,
            "s5": True,
            "roles": {},
            "node_ports": {"main": {}},
        },
    )
    monkeypatch.setattr(
        regrade.fixtures,
        "create_fixtures",
        lambda client, spec, now=None: FixtureRecord(user="alice", started_at="t"),
    )
    monkeypatch.setattr(regrade, "safe_cleanup", lambda client, rec: [])
    return results, calls


def _run(doc: dict):
    return regrade.regrade(doc, TASKS, lint=None, catalog=None, client=object())


def test_mode_a_redoes_s2d_and_keeps_s1(plumbing):
    results, _ = plumbing
    results[0] = _stub("failed_in_success")  # BUG-267: map の項目の失敗
    doc = {
        "grader": PREV,
        "trials": [_trial("a", "app08-weekly-kpi", 0), _trial("a", "app08-weekly-kpi", 1)],
    }
    out = _run(doc)
    assert out.errors == [] and out.trials == 2 and out.stub_trials == 2
    hit, other = doc["trials"]
    assert (hit["s2d"], hit["headline"]) == (True, False)
    assert (other["s2d"], other["headline"]) == (False, True)
    assert out.changed == [("app08-weekly-kpi.flow.0.medium", True, False, ["failed_in_success"])]
    assert hit["regraded"] == {
        "grader": GRADER_VERSION,
        "from_grader": PREV,
        "redone": ["s2s", "s2w", "s5", "stub", "s2d", "s3"],
        "kept": ["s1"],
    }


def test_mode_b_keeps_the_real_runs_and_says_so(plumbing):
    results, _ = plumbing
    # BUG-268: 前の採点では stub の配管が「入力が届かない」を誤って言い、S4 は通っていた。
    fixed = _trial("b", "app12-faq-table", 0, stub=_stub("not_delivered"), s2d=True, headline=False)
    # 実モデルの run_case の S2d が真の試行は、stub の配管が通っても S2d のまま。
    real_bad = _trial("b", "app12-faq-table", 1, runs={"s2d": True, "s3": True, "s4": True})
    real_bad.update({"s2d": True, "headline": False})
    results[0] = _stub(None)
    results[1] = _stub(None)
    runs_before = copy.deepcopy(real_bad["runs"])
    doc = {"grader": PREV, "trials": [fixed, real_bad]}
    out = _run(doc)
    assert out.errors == []
    assert (fixed["s2d"], fixed["s4"], fixed["headline"]) == (False, True, True)
    assert (real_bad["s2d"], real_bad["headline"]) == (True, False)
    assert real_bad["runs"] == runs_before
    assert out.changed == [("app12-faq-table.flow.0.medium", False, True, [])]
    mark = fixed["regraded"]
    assert mark["kept"] == ["s1", "s3", "s4", "runs"] and "会話を流し直さないと" in mark["note"]
    assert mark["redone"] == ["s2s", "s2w", "s5", "stub", "s2d"]


def test_trials_without_plumbing_are_only_regraded_statically(plumbing):
    _, calls = plumbing
    unsaved = _trial("a", "basic-summary-file", 0, s1=False, headline=False)
    unsaved.pop("stub")
    no_plumbing = _trial("b", "basic-summary-file", 1)
    no_plumbing.pop("stub")
    cut = _trial("a", "basic-summary-file", 2, status="truncated")
    doc = {"grader": PREV, "trials": [unsaved, no_plumbing, cut]}
    out = _run(doc)
    assert calls == [] and out.trials == 2 and out.stub_trials == 0
    assert "regraded" not in cut  # emit でない試行は採点し直さない（実行の採点と同じ）
    assert "前の採点のまま" in no_plumbing["regraded"]["note_s2d"]


def test_an_infra_failure_leaves_the_result_unwritten(plumbing, tmp_path, capsys):
    results, _ = plumbing
    results[1] = InfraFailure("stub_plumbing", "TimeoutError: 180 秒")
    doc = {
        "experiment": "e1a-vf",
        "grader": PREV,
        "trials": [_trial("a", "app08-weekly-kpi", 0), _trial("a", "app08-weekly-kpi", 1)],
    }
    out = _run(doc)
    assert out.errors == ["app08-weekly-kpi.flow.1.medium: stub_plumbing: TimeoutError: 180 秒"]
    assert cli._write_regrade(doc, out, "vf", None, ident=None) == 1
    assert report.read_result("e1a-vf") is None
    assert "結果を書きません" in capsys.readouterr().out


def test_static_only_needs_the_same_rules_and_stub_runs_need_one_version():
    with pytest.raises(regrade.RegradeError, match="--stub-runs"):
        regrade.check_rules({"grader": PREV}, stub_runs=False)
    regrade.check_rules({"grader": {**PREV, "grader": GRADER_VERSION}}, stub_runs=False)
    regrade.check_rules({"grader": PREV}, stub_runs=True)
    doc = {"trials": [_trial("a", "app08-weekly-kpi", 0)]}
    assert regrade.stub_label(doc) == "vf"
    doc["trials"].append(_trial("a", "app08-weekly-kpi", 1, label="vm"))
    with pytest.raises(regrade.RegradeError, match="1 つではありません"):
        regrade.stub_label(doc)


def test_the_summary_says_which_experiments_were_regraded_and_what_was_kept():
    b = {
        "experiment": "e1b-vf",
        "regrades": [
            {"grader": GRADER_VERSION, "from_grader": PREV, "stub_runs": True, "label": "vf"}
        ],
        "trials": [{"mode": "b"}],
    }
    a = {
        "experiment": "e1a-x",
        "regrades": [{"grader": GRADER_VERSION, "from_grader": PREV, "stub_runs": False}],
        "trials": [{"mode": "a"}],
    }
    [nb, na] = report.regrade_notes([b, a, {"experiment": "e1a-vm", "trials": []}])
    assert "版 vf のサーバーで stub の配管を流し直して" in nb and "S3・S4" in nb
    assert f"採点器の版 {GRADER_VERSION - 1}）の値のまま" in nb
    assert "静的な層" in na and "S3・S4" not in na
