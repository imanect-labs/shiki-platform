"""abeval.metrics（pass@k の既知の値・ブートストラップの決定性・率の分母・集計）。"""

from __future__ import annotations

import math

import pytest

from abeval import metrics as m


@pytest.mark.parametrize(
    "n,c,k,want",
    [
        (3, 0, 1, 0.0),
        (3, 3, 1, 1.0),
        (3, 1, 1, 1 / 3),
        (3, 1, 2, 2 / 3),
        (3, 2, 2, 1.0),  # n − c < k
        (5, 2, 3, 0.9),  # 1 − C(3,3)/C(5,3)
        (10, 3, 5, 1 - math.comb(7, 5) / math.comb(10, 5)),
    ],
)
def test_pass_at_k_known_values(n, c, k, want):
    assert m.pass_at_k(n, c, k) == pytest.approx(want)


def test_pass_at_k_rejects_bad_arguments():
    for args in ((3, 4, 1), (3, 1, 0), (3, 1, 4)):
        with pytest.raises(ValueError):
            m.pass_at_k(*args)


def test_pass_at_1_is_the_success_rate():
    for n in range(1, 8):
        for c in range(n + 1):
            assert m.pass_at_k(n, c, 1) == pytest.approx(c / n)


def test_bootstrap_is_deterministic_with_the_seed():
    a = {f"t{i}": ((i * 7) % 13) / 13 for i in range(20)}
    b = {f"t{i}": ((i * 5) % 11) / 11 for i in range(20)}
    r1 = m.paired_bootstrap(a, b, iters=2000)
    r2 = m.paired_bootstrap(a, b, iters=2000)
    assert r1 == r2
    r3 = m.paired_bootstrap(a, b, iters=2000, seed=1)
    assert r3["diff"] == r1["diff"] and (r3["lo"], r3["hi"]) != (r1["lo"], r1["hi"])
    assert m.bootstrap_mean(a, iters=500) == m.bootstrap_mean(a, iters=500)


def test_bootstrap_detects_a_clear_difference_and_not_noise():
    a = {f"t{i}": 0.0 for i in range(20)}
    b = {f"t{i}": 1.0 for i in range(20)}
    assert m.paired_bootstrap(a, b, iters=1000)["significant"]
    noise = {f"t{i}": (1.0 if i % 2 else 0.0) for i in range(20)}
    flip = {f"t{i}": (0.0 if i % 2 else 1.0) for i in range(20)}
    d = m.paired_bootstrap(noise, flip, iters=1000)
    assert d["diff"] == pytest.approx(0.0) and not d["significant"]
    # 共通の課題だけを使う。
    assert m.paired_bootstrap({"x": 1.0}, {"y": 0.0})["n_tasks"] == 0


def test_rate_ignores_none():
    assert m.rate([True, False, None, True]) == {"n": 3, "true": 2, "rate": 2 / 3}
    assert m.rate([None])["rate"] is None


def trial(
    task, n, *, s1=True, s2s=False, s2d=False, s4=None, mode="a", effort="medium", cost=1000, **kw
):
    t = {
        "task": task,
        "n": n,
        "label": "v0",
        "mode": mode,
        "effort": effort,
        "max_steps": None,
        "s1": s1,
        "s2s": s2s if s1 else None,
        "s2w": False if s1 else None,
        "s2d": s2d if s1 else None,
        "s3": True if s1 else None,
        "s5": True if s1 else None,
        "s4": s4,
        "cost_micros": cost,
        "secs": 2.0,
        "status": "emit" if s1 else "no_emit",
        "s8": {"asked": False},
    }
    hl = s1 and not s2s and not s2d and (s4 is True if mode == "b" else True)
    t["headline"] = hl
    t.update(kw)
    return t


def test_aggregate_denominators_and_pass_at_k():
    ts = [
        trial("a", 0),
        trial("a", 1, s2s=True),
        trial("a", 2, s1=False),
        trial("b", 0, s1=False),
        trial("b", 1, s1=False),
        trial("b", 2, s1=False),
    ]
    [row] = m.aggregate(ts, iters=200)
    assert row["trials"] == 6 and row["tasks"] == 2
    assert row["s1"] == {"n": 6, "true": 2, "rate": 2 / 6}
    # S2s の分母は保存できた試行。
    assert row["s2s"] == {"n": 2, "true": 1, "rate": 0.5}
    assert row["headline"]["true"] == 1
    # pass@1 は課題ごとの率の平均（a は 1/3、b は 0）。
    assert row["pass_at_k"]["1"] == pytest.approx((1 / 3 + 0) / 2)
    assert row["pass_at_k"]["3"] == pytest.approx((1 + 0) / 2)
    assert row["cost_usd"] == pytest.approx(0.006)
    assert row["cost_per_completed_task_usd"] == pytest.approx(0.006)
    assert row["status"] == {"emit": 2, "no_emit": 4}


def test_aggregate_splits_conditions_and_mode_b_fields():
    ts = [trial("a", 0, mode="b", s4=True), trial("a", 0, mode="b", s4=False, effort="high")]
    for t in ts:
        t["s6"] = {
            "emit_count": 2,
            "validation_errors": 1,
            "steps": 3,
            "hit_step_limit": False,
            "emit_loaded": True,
        }
        t["s9"] = {
            "requests": 1,
            "approved": 0,
            "denied": 1,
            "serious": False,
            "tools": ["data_record_write"],
        }
    rows = m.aggregate(ts, iters=100)
    assert [r["condition"] for r in rows] == ["v0/b/high", "v0/b/medium"]
    med = rows[1]
    assert med["s4"]["rate"] == 1.0 and med["s9"]["denied"] == 1 and med["s6"]["emit_count"] == 2


def test_failure_breakdown_counts_codes():
    t = trial("a", 0)
    t["validation"] = {"main": {"errors": ["ir.unknown_port"]}}
    t["static"] = {
        "roles": {
            "main": {
                "lint": {"errors": ["ir.bad_ref"], "strict_warnings": [], "run_blockers": []},
                "task": [{"code": "input_ref"}],
            }
        }
    }
    t["stub"] = {"cases": [{"dynamic": {"findings": [{"code": "dead_edge"}]}}]}
    t2 = trial("b", 0, mode="b")
    t2["s6"] = {"validation_codes": {"ir.unknown_port": 2}}
    fb = m.failure_breakdown([t, t2])
    assert fb["validation"] == {"ir.unknown_port": 3}
    assert fb["lint"] == {"ir.bad_ref": 1} and fb["task"] == {"input_ref": 1}
    assert fb["dynamic"] == {"dead_edge": 1}
    for part in ("stub", "runs"):
        assert fb[f"steps_{part}"] == {} and fb[f"steps_by_node_{part}"] == {}
    assert "steps" not in fb and "steps_by_node" not in fb


def test_failure_breakdown_counts_failed_steps_by_product_code():
    """run_case の実行で落ちた step の誤りを、製品のコードと「ノードの種類/コード」で数える（GAP-111）。"""
    ir = {
        "nodes": [
            {"id": "find", "type": "data.query"},
            {"id": "add", "type": "data.record.create"},
        ]
    }
    unindexed = {
        "code": "invalid",
        "message": "'x' では絞り込み・並べ替えできません",
        "node_id": "find",
    }
    t = trial("a", 0)
    t["irs"] = {"main": ir}
    t["stub"] = {
        "cases": [
            {
                "failed_steps": [
                    {"step": "find", "node_id": "find", "workflow": None, "error": unindexed}
                ]
            },
            {
                "failed_steps": [
                    {"step": "add", "node_id": "add", "error": {"code": "expr_resolve_error"}},
                    {"run": "not_started", "error": "400: 入力が足りません"},
                ]
            },
        ]
    }
    t2 = trial("b", 0, mode="b")
    t2["irs"] = {"main": ir}
    # node_id が step の誤りにしか無い記録・code の無い誤りも数える（文では分けない）。
    t2["runs"] = {
        "cases": [
            {"failed_steps": [{"step": "find", "error": unindexed}, {"step": "zz", "error": "x"}]}
        ]
    }
    fb = m.failure_breakdown([t, t2])
    # stub の配管と実モデルの run_case は別の欄に数える（同じ試行の同じ誤りを 1 つの欄に 2 回数えない）。
    assert fb["steps_stub"] == {"expr_resolve_error": 1, "invalid": 1, "run_not_started": 1}
    assert fb["steps_by_node_stub"] == {
        "data.query/invalid": 1,
        "data.record.create/expr_resolve_error": 1,
    }
    assert fb["steps_runs"] == {"invalid": 1, "unknown": 1}
    assert fb["steps_by_node_runs"] == {"data.query/invalid": 1, "?/unknown": 1}
    assert list(fb["steps_stub"]) == ["expr_resolve_error", "invalid", "run_not_started"]


def test_a_mode_b_trial_counts_the_same_failure_once_per_part():
    """Mode B は 1 試行で stub の配管と実モデルの run_case の両方を流す。同じ誤りを同じ欄に 2 回数えない。"""
    ir = {"nodes": [{"id": "find", "type": "data.query"}]}
    failed = {"failed_steps": [{"step": "find", "node_id": "find", "error": {"code": "invalid"}}]}
    t = trial("b", 0, mode="b")
    t["irs"] = {"main": ir}
    t["stub"] = {"cases": [failed]}
    t["runs"] = {"cases": [failed]}
    fb = m.failure_breakdown([t])
    assert fb["steps_stub"] == {"invalid": 1} and fb["steps_runs"] == {"invalid": 1}
    assert fb["steps_by_node_stub"] == fb["steps_by_node_runs"] == {"data.query/invalid": 1}


def test_failed_steps_from_a_recorded_run_are_broken_down_by_node_type():
    """stub のスタックで記録した run（入力の key を替えた壊し方）の落ちた step を、ノードの種類つきで数える。"""
    import datetime as dt

    import recorded
    from abeval.run_cases import grade_case

    task = recorded.tasks()["app02-triage"]
    irs, ports, rec = recorded.run("app02-triage", "input_key", "billing")
    case = next(c for c in task.run_cases if c.id == "billing")
    g = grade_case(task, case, rec, irs, ports, stub=True, now=dt.datetime.now(dt.UTC))
    assert g["failed_steps"][0]["node_id"] == "register"
    assert g["failed_steps"][0]["workflow"] is None
    t = trial("a", 0)
    t["irs"] = irs
    t["stub"] = {"cases": [g]}
    fb = m.failure_breakdown([t])
    assert fb["steps_stub"] == {"expr_resolve_error": 1} and fb["steps_runs"] == {}
    assert fb["steps_by_node_stub"] == {"data.record.create/expr_resolve_error": 1}
