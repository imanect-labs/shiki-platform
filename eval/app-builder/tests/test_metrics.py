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
