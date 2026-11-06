"""abeval.budget（評価の台帳・手元の中継の台帳を読むだけ・試行への割り当て）。"""

from __future__ import annotations

import json

import pytest

from abeval import budget
from abeval.config import load_config
from abeval.ledger import Ledger


def _fill(cfg):
    led = Ledger(cfg.paths.ledger)
    led.append({"cost_micros": 1_000, "exp": "e0", "run_id": "r1", "trial": "t1", "session": "th1"})
    led.append({"cost_micros": 2_500, "exp": "e0", "run_id": "r1", "trial": "t1", "session": None})
    led.append(
        {
            "cost_micros": 400,
            "exp": "e1",
            "run_id": "r2",
            "trial": "t1",
            "session": "th9",
            "usage_missing": True,
        }
    )
    led.append(
        {
            "cost_micros": 0,
            "exp": "e1",
            "run_id": "r2",
            "trial": "capture",
            "session": "th2",
            "captured": True,
        }
    )


def test_summary_by_experiment_and_run_and_the_outer_total():
    cfg = load_config()
    _fill(cfg)
    cfg.paths.relay_ledger.parent.mkdir(parents=True, exist_ok=True)
    cfg.paths.relay_ledger.write_text(
        "".join(json.dumps({"cost_usd": c, "tag": "abeval/e0/r1/t1"}) + "\n" for c in (0.5, 1.25))
    )
    b = budget.summarize(cfg)
    assert b.eval_micros == 3_900 and b.lines == 4
    assert b.by_exp == {"e0": 3_500, "e1": 400}
    assert b.by_run["r2"].usage_missing == 1 and b.by_run["r1"].lines == 2
    assert b.eval_left_usd == pytest.approx(35 - 0.0039)
    assert b.relay_usd == pytest.approx(1.75) and b.global_left_usd == pytest.approx(95 - 1.75)
    text = budget.format_budget(b)
    assert "実験 e0" in text and "r2" in text and "予約で数えた 1 行" in text
    d = b.to_dict()
    assert d["by_exp_usd"]["e0"] == pytest.approx(0.0035)


def test_missing_relay_ledger_is_a_note_not_an_error():
    b = budget.summarize(load_config())
    assert b.relay_usd is None and b.global_left_usd is None and b.notes


def test_trial_spend_splits_chat_and_run_cases():
    cfg = load_config()
    _fill(cfg)
    s = budget.trial_spend(cfg, "t1", "r1")
    assert s == {
        "chat_micros": 1_000,
        "run_case_micros": 2_500,
        "lines": 2,
        "usage_missing": 0,
        "total_micros": 3_500,
    }
    # run を指定しなければ、ほかの実行の同じ試行の id の分も入る。
    assert budget.trial_spend(cfg, "t1")["total_micros"] == 3_900


def test_main_prints_json(capsys):
    assert budget.main(["--json"]) == 0
    d = json.loads(capsys.readouterr().out)
    assert d["eval_cap_usd"] == 35 and d["eval_usd"] == 0
