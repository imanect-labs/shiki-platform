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
        "statuses": {"chat": {}, "run_case": {}},
    }
    # run を指定しなければ、ほかの実行の同じ試行の id の分も入る。
    assert budget.trial_spend(cfg, "t1")["total_micros"] == 3_900


def test_main_prints_json(capsys):
    assert budget.main(["--json"]) == 0
    d = json.loads(capsys.readouterr().out)
    assert d["eval_cap_usd"] == 35 and d["eval_usd"] == 0


# --- 評価の台帳の置き場と、消えた・別の台帳になったときの守り ---


def test_the_default_ledger_lives_outside_the_worktree():
    from pathlib import Path

    from abeval.config import ROOT

    cfg = load_config(env={})
    assert cfg.paths.ledger == Path.home() / ".local" / "share" / "shiki-eval" / "ledger.jsonl"
    assert ROOT not in cfg.paths.ledger.parents


def _relay(cfg, *rows):
    cfg.paths.relay_ledger.parent.mkdir(parents=True, exist_ok=True)
    cfg.paths.relay_ledger.write_text("".join(json.dumps(r) + "\n" for r in rows))


def test_guard_finds_a_ledger_that_was_lost(tmp_path, monkeypatch):
    monkeypatch.setattr(budget, "LEGACY_LEDGER", tmp_path / "no-legacy.jsonl")
    cfg = load_config()
    assert budget.ledger_guard(cfg) is None  # 手元の中継の台帳も無い
    _relay(
        cfg,
        {"cost_usd": 0.5, "tag": "abeval/e0/r1/t1"},
        {"cost_usd": 40.0, "tag": "campaign/x"},  # 評価の分ではない
    )
    why = budget.ledger_guard(cfg)
    assert why and "$0.5000" in why and "すり抜ける" in why
    # 評価の台帳に同じだけあれば通る（丸めの差は許す）。
    Ledger(cfg.paths.ledger).append({"cost_micros": 499_000, "run_id": "r1"})
    assert budget.ledger_guard(cfg) is None
    # 計画も、評価の台帳より大きい方で数える。
    from abeval.plan import spent

    cfg.paths.ledger.write_text("")
    eval_usd, relay, notes = spent(cfg)
    assert eval_usd == pytest.approx(0.5) and relay == pytest.approx(40.5)
    assert any("そちらで数える" in n for n in notes)


def test_guard_refuses_rows_left_in_the_old_place(tmp_path, monkeypatch):
    legacy = tmp_path / "results" / "spend.jsonl"
    legacy.parent.mkdir()
    legacy.write_text(json.dumps({"cost_micros": 1}) + "\n")
    monkeypatch.setattr(budget, "LEGACY_LEDGER", legacy)
    why = budget.ledger_guard(load_config())
    assert why and "前の置き場" in why
    legacy.write_text("")
    assert budget.ledger_guard(load_config()) is None


def test_the_proxy_does_not_start_when_the_ledger_was_lost(tmp_path, monkeypatch, capsys):
    from abeval import proxy

    monkeypatch.setattr(budget, "LEGACY_LEDGER", tmp_path / "no-legacy.jsonl")
    cfg = load_config()
    _relay(cfg, {"cost_usd": 3.0, "tag": "abeval/e1/r9/t"})
    assert proxy.main(["--listen", "127.0.0.1:0"]) == 2
    assert "起動しません" in capsys.readouterr().err
    assert not (cfg.paths.home / "proxy.token").exists()


def test_notes_do_not_carry_the_home_directory():
    from pathlib import Path

    from abeval.config import display_path
    from abeval.plan import spent

    _, _, notes = spent(load_config())  # 手元の中継の台帳が無い（テストの置き場はホームの下）
    assert notes and all(str(Path.home()) not in n for n in notes)
    assert any("~/" in n for n in notes)
    assert display_path(Path.home() / "a" / "b.jsonl") == "~/a/b.jsonl"
    assert display_path("/srv/x") == "/srv/x"
