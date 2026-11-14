"""abeval.plan（試行の列・見積もり・上限での拒否・分け方・登録）。"""

from __future__ import annotations

import json

import pytest

from abeval.config import load_config
from abeval.ledger import Ledger
from abeval.plan import (
    PlanError,
    RunSpec,
    format_plan,
    make_plan,
    mode_a_reserve_usd,
    register,
    spent,
)
from abeval.tasks import PHRASINGS
from fake_upstream import FakeUpstream
from inproc import running_proxy

TASKS = tuple(f"task-{i:02d}" for i in range(20))


def spec(**over) -> RunSpec:
    kw = dict(exp="e1a", mode="a", label="v0", tasks=TASKS, trials=3, efforts=("medium",))
    kw.update(over)
    return RunSpec(**kw)


def test_mode_a_crosses_phrasings_and_trials():
    ts = spec().trial_specs()
    assert len(ts) == 20 * 3 * 3
    assert {t.phrasing for t in ts} == set(PHRASINGS)
    assert len({t.trial_id for t in ts}) == len(ts)


def test_mode_b_rotates_phrasings_by_trial_number():
    ts = spec(mode="b", trials=4).trial_specs()
    assert len(ts) == 20 * 4
    first = [t for t in ts if t.task_id == "task-00"]
    assert [t.phrasing for t in first] == ["flow", "app", "auto", "flow"]


def test_spec_validation():
    with pytest.raises(PlanError):
        spec(mode="c")
    with pytest.raises(PlanError):
        spec(efforts=("max",))
    with pytest.raises(PlanError):
        spec(phrasings=("other",))
    assert spec(efforts=(None, "high")).trial_specs()[0].effort is None


def test_estimate_uses_the_table_and_the_reserve():
    cfg = load_config()
    # 見積もりの値は E0 の実測で置き換えるので、eval.toml の表から読んで比べる。
    est = cfg.estimates["a"]
    p = make_plan(spec(), cfg, eval_spent_usd=0, relay_spent_usd=0)
    assert p.ok
    assert p.worst_usd == pytest.approx(180 * est.worst)
    assert p.central_usd == pytest.approx(180 * est.central)
    assert p.cap_usd == pytest.approx(min(180 * est.worst * 1.2, 10))
    # 取り込みから組んだ予約（abeval.proxy と同じ式）が大きければそれを最悪にする。
    r = mode_a_reserve_usd(cfg, 60_000, 8192)
    assert r == pytest.approx((60_000 * 0.10 + 8192 * 0.50) / 1e6, rel=1e-3)
    p2 = make_plan(spec(), cfg, eval_spent_usd=0, relay_spent_usd=0, reserve_usd=0.05)
    assert p2.worst_usd == pytest.approx(180 * 0.05)


def test_effort_and_steps_raise_the_mode_b_worst():
    cfg = load_config()
    base = make_plan(spec(mode="b"), cfg, eval_spent_usd=0, relay_spent_usd=0).worst_usd
    high = make_plan(
        spec(mode="b", efforts=("high",)), cfg, eval_spent_usd=0, relay_spent_usd=0
    ).worst_usd
    steps = make_plan(
        spec(mode="b", max_steps=12), cfg, eval_spent_usd=0, relay_spent_usd=0
    ).worst_usd
    est = cfg.estimates["b"]
    assert base == pytest.approx(60 * est.worst)
    assert high == pytest.approx(60 * est.worst_for(effort="high"))
    assert steps == pytest.approx(60 * est.worst_for(max_steps=12))
    assert high > base and steps > base


def test_refuses_a_run_over_ten_dollars_and_shows_splits():
    cfg = load_config()
    # E1 の Mode B の 3 版分（180 試行）に high を足すと $10 を超える。
    s = spec(mode="b", trials=9, efforts=("medium", "high"))
    p = make_plan(s, cfg, eval_spent_usd=0, relay_spent_usd=0)
    assert not p.ok and p.worst_usd > 10
    assert any("1 回の上限" in x for x in p.problems)
    assert len(p.splits) >= 2
    per_task = p.worst_usd / len(TASKS)
    for g in p.splits:
        assert len(g) * per_task <= 10 + 1e-9
    assert sorted(t for g in p.splits for t in g) == sorted(TASKS)
    text = format_plan(p)
    assert "[拒否]" in text and "--tasks" in text and "実行しません" in text
    # 分けた 1 つ目は通る。
    first = make_plan(
        spec(mode="b", trials=9, efforts=("medium", "high"), tasks=tuple(p.splits[0])),
        cfg,
        eval_spent_usd=0,
        relay_spent_usd=0,
    )
    assert first.ok


def test_refuses_when_the_eval_or_the_global_total_would_be_exceeded(tmp_path):
    cfg = load_config()
    led = Ledger(cfg.paths.ledger)
    led.append({"cost_micros": 33_500_000, "run_id": "old", "exp": "e0"})
    eval_usd, relay, notes = spent(cfg)
    assert eval_usd == pytest.approx(33.5) and relay is None and notes
    p = make_plan(spec(), cfg, eval_spent_usd=eval_usd, relay_spent_usd=relay)
    assert not p.ok and any("評価の上限" in x for x in p.problems)
    cfg.paths.relay_ledger.parent.mkdir(parents=True, exist_ok=True)
    cfg.paths.relay_ledger.write_text(json.dumps({"cost_usd": 94.0, "tag": "x"}) + "\n")
    _, relay2, _ = spent(cfg)
    p2 = make_plan(spec(), cfg, eval_spent_usd=0, relay_spent_usd=relay2)
    assert not p2.ok and any("総額の上限" in x for x in p2.problems)


def test_register_only_ok_plans_with_the_capped_amount():
    cfg = load_config()
    with FakeUpstream() as up, running_proxy(up.url) as proxy, proxy.control() as ctrl:
        p = make_plan(spec(), cfg, eval_spent_usd=0, relay_spent_usd=0)
        register(p, ctrl)
        runs = ctrl.spend()["runs"]
        assert runs[p.run_id]["cap_micros"] == round(p.cap_usd * 1_000_000)
        bad = make_plan(
            spec(mode="b", trials=9, efforts=("medium", "high")),
            cfg,
            eval_spent_usd=0,
            relay_spent_usd=0,
        )
        with pytest.raises(PlanError):
            register(bad, ctrl)
        assert bad.run_id not in ctrl.spend()["runs"]
        assert up.received == []


def test_plan_command_exit_codes(capsys):
    from abeval import cli

    ok = ["--mode", "a", "--label", "v0", "--exp", "e1a", "--tasks", "basic-summary-file"]
    assert cli.main(ok, command="plan") == 0
    big = [
        "--mode",
        "b",
        "--label",
        "v0",
        "--exp",
        "e1b",
        "--trials",
        "9",
        "--effort",
        "medium",
        "--effort",
        "high",
    ]
    assert cli.main(big, command="plan") == 2
    out = capsys.readouterr().out
    assert "[拒否]" in out and "分け方 1" in out
