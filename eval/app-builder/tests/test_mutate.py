"""採点器の自己テスト: 9 通りの壊し方が、期待した検査で捕まる（GAP-30 §5.4）。

- 静的: 中核の全課題の正解 IR に当てはまる壊し方を全部かけ、記録した ir_lint の出力（tests/fixtures/ir_lint/）で
  採点する。
- 動的: stub のスタックで記録した run（tests/fixtures/runs/）で採点する。`from_port_next` は HEAD の検証では
  保存できない（ir.unknown_port）ので、正解の記録から V0 の動き（`next` の線の先が動かない）を組んで採点する。
- 検査を何もしないように差し替えると、ここのテストが落ちることも確かめる（採点器が本当に効いていること）。
"""

from __future__ import annotations

import copy

import pytest

import recorded
from abeval.grade import dynamic, mutate, static
from abeval.grade.dynamic import RunRecord, StepRecord, grade_dynamic
from abeval.grade.mutate import MUTATIONS, by_name, mutants
from abeval.grade.static import grade_static
from abeval.tasks import offline_irs


def _static_items():
    """(課題, 役割, 壊し方, 壊した IR, 記録の id)。"""
    out = []
    for t in recorded.tasks().values():
        for role, ir in offline_irs(t).items():
            for m, bad in mutants(ir, t.role(role).expect):
                out.append((t, role, m, bad, f"{t.id}/{role}/mut:{m.name}"))
    return out


def check_static() -> list[str]:
    items = _static_items()
    recorded.check_fresh([(i, bad) for _, _, _, bad, i in items])
    lint, cat = recorded.lint(), recorded.catalog()
    failures = []
    for t, role, m, bad, key in items:
        got = grade_static(bad, t.role(role).expect, lint[key], cat).detectors()
        if not m.expect <= got:
            failures.append(f"{key}: 期待 {sorted(m.expect)}・捕まえたもの {sorted(got)}")
    return failures


def _changed_node(before: dict, after: dict) -> dict | None:
    old = {n["id"]: n for n in before["nodes"]}
    for n in after["nodes"]:
        if n != old.get(n["id"]):
            return n
    return None


def _observable(irs_before: dict, irs_after: dict) -> bool:
    """差し込みを壊したノードの出力に、解決した文字が映るか（形のある llm.invoke は stub が形に合う最小の値を返す）。"""
    n = _changed_node(irs_before, irs_after)
    return n is not None and not (
        n["type"] == "llm.invoke" and (n.get("params") or {}).get("output_schema")
    )


def _v0_from_port_next(task_id: str, case_id: str):
    """正解の記録から、V0 の動き（`from_port: next` の線の先が動かない）を組む。"""
    task = recorded.tasks()[task_id]
    irs, _, rec = recorded.run(task_id, "oracle", case_id)
    bad = mutate.from_port_next(copy.deepcopy(irs["main"]), task.role(None).expect)
    edge = next(e for e in bad["edges"] if e.get("from_port") == "next")
    gone = {edge["to"], *dynamic.downstream(bad, edge["to"])}
    rec = copy.deepcopy(rec)
    for run in rec.runs:
        run.steps = [
            s if s.node_id not in gone else StepRecord(s.step_path, s.node_id, "skipped", [], None)
            for s in run.steps
        ]
    # 線の形は from_port だけが違う（ir_lint の出口は HEAD の正解と同じ）。
    ports = recorded.lint()[f"{task_id}/main"].node_ports
    return task, {"main": bad}, {"main": {k: list(v) for k, v in ports.items()}}, rec


def _dyn(task, irs, ports, rec, case):
    return grade_dynamic(
        irs,
        ports,
        {r.role: r.expect for r in task.roles},
        rec,
        restrict=case.llm_dependent,
        delivery=case.expect.get("flow_through", True),
    ).detectors()


def check_dynamic() -> list[str]:
    failures = []
    names = {m.name for m in MUTATIONS}
    for task_id, variant, case_id in recorded.runs():
        if variant not in names:
            continue
        task = recorded.tasks()[task_id]
        case = next(c for c in task.run_cases if c.id == case_id)
        irs, ports, rec = recorded.run(task_id, variant, case_id)
        want = set(by_name(variant).dynamic)
        if variant == "escape_var":
            oracle_irs, _, _ = recorded.run(task_id, "oracle", case_id)
            if not _observable(oracle_irs["main"], irs["main"]):
                want.discard("dyn:leftover")
        if not case.expect.get("flow_through", True):
            want.discard("dyn:not_delivered")
        got = _dyn(task, irs, ports, rec, case)
        if not want <= got:
            failures.append(
                f"{task_id}.{variant}.{case_id}: 期待 {sorted(want)}・捕まえたもの {sorted(got)}"
            )
    for task_id, case_id in (("basic-summary-file", "normal"), ("app02-triage", "billing")):
        task, irs, ports, rec = _v0_from_port_next(task_id, case_id)
        case = next(c for c in task.run_cases if c.id == case_id)
        got = _dyn(task, irs, ports, rec, case)
        if "dyn:dead_edge" not in got:
            failures.append(
                f"{task_id}.from_port_next(V0).{case_id}: dead edge が捕まらない（{sorted(got)}）"
            )
    return failures


def test_every_mutation_applies_to_several_core_tasks():
    counts = {m.name: 0 for m in MUTATIONS}
    for _, _, m, _, _ in _static_items():
        counts[m.name] += 1
    assert all(n >= 3 for n in counts.values()), counts


def test_static_mutants_are_caught():
    assert check_static() == []


def test_dynamic_mutants_are_caught():
    assert check_dynamic() == []


def test_recorded_dynamic_mutants_cover_the_dynamic_mutations():
    seen = {v for _, v, _ in recorded.runs()}
    assert {"escape_var", "input_key", "oracle", "null"} <= seen
    # from_port_next は V0 の動きを組んで見る（上の check_dynamic）。
    assert {m.name for m in MUTATIONS if m.dynamic} == {"from_port_next", "escape_var", "input_key"}


def test_from_port_next_is_also_an_unknown_port_statically():
    lint = recorded.lint()
    caught = [k for k in lint if k.endswith("/mut:from_port_next")]
    assert caught
    for k in caught:
        assert "ir.unknown_port" in {e["code"] for e in lint[k].errors}, k


def test_mutations_do_not_apply_where_there_is_nothing_to_break():
    t = recorded.tasks()["basic-map-rows"]
    ir = offline_irs(t)["main"]
    assert mutate.from_port_next(ir, t.role(None).expect) is None  # 線が無い（map の領域だけ）
    assert mutate.llm_path_category(ir, t.role(None).expect) is None
    assert mutate.from_node_id(ir, t.role(None).expect) is None


# --- 採点器が本当に効いていること（検査を何もしないようにすると落ちる） ---


@pytest.mark.parametrize(
    ("module", "name", "replacement", "check"),
    [
        (dynamic, "dead_edges", lambda *a, **k: [], check_dynamic),
        (dynamic, "leftovers", lambda *a, **k: [], check_dynamic),
        (dynamic, "not_delivered", lambda *a, **k: [], check_dynamic),
        (static, "STRICT_WARNINGS", frozenset(), check_static),
        (static, "input_ref_findings", lambda *a, **k: [], check_static),
        (static, "output_path_findings", lambda *a, **k: [], check_static),
        (static, "structure_findings", lambda *a, **k: [], check_static),
    ],
    ids=[
        "dead_edges",
        "leftovers",
        "not_delivered",
        "strict_warnings",
        "input_ref",
        "output_path",
        "structure",
    ],
)
def test_disabling_a_detector_makes_the_self_test_fail(
    monkeypatch, module, name, replacement, check
):
    assert check() == [], "差し替える前は通る"
    monkeypatch.setattr(module, name, replacement)
    assert check(), f"{module.__name__}.{name} を何もしないようにしても自己テストが通ってしまう"


def test_disabling_lint_errors_makes_the_self_test_fail(monkeypatch):
    real = static.classify_lint

    def no_errors(r, unattended=False):
        l1 = real(r, unattended)
        return static.Layer1(
            (),
            l1.run_blockers,
            l1.strict_warnings,
            l1.warnings,
            l1.ignored_by_env,
            l1.input_error,
            l1.enable_blockers,
        )

    monkeypatch.setattr(static, "classify_lint", no_errors)
    failed = check_static()
    for name in ("edge_port_key", "from_port_next", "unknown_var", "from_node_id"):
        assert any(f"mut:{name}:" in f for f in failed), name


def test_not_counting_enable_blockers_lets_a_missing_folder_through(monkeypatch):
    """自動で動く課題で保存先のフォルダを省くと、有効化の前に止まる（S2s）。数えないと見逃す。"""
    assert not [f for f in check_static() if "mut:drop_folder" in f]
    real = static.classify_lint
    monkeypatch.setattr(static, "classify_lint", lambda r, unattended=False: real(r, False))
    failed = check_static()
    assert failed and all("mut:drop_folder" in f for f in failed), failed


def test_enable_blockers_count_only_for_unattended_tasks():
    from abeval.grade.static import LintResult

    lr = LintResult(id="x", enable_blockers=({"code": "ir.write_folder_required"},))
    t = recorded.tasks()["basic-summary-file"]  # 対話の課題
    g = grade_static({"nodes": [], "edges": []}, t.role(None).expect, lr, recorded.catalog())
    assert g.layer1.enable_blockers == () and not g.layer1.s2s
    t = recorded.tasks()["app03-weekly-summary"]  # スケジュールの課題
    g = grade_static({"nodes": [], "edges": []}, t.role(None).expect, lr, recorded.catalog())
    assert g.layer1.enable_blockers == ("ir.write_folder_required",) and g.s2s
    assert "lint:ir.write_folder_required" in g.detectors()


def test_v0_synthesis_really_skips_the_dead_edge_target():
    task, irs, _, rec = _v0_from_port_next("basic-summary-file", "normal")
    assert [s.status for s in rec.runs[0].steps if s.node_id == "save"] == ["skipped"]
    assert isinstance(rec.runs[0], RunRecord)
