"""abeval.proxy が落ちた後の戻し方（`python -m abeval budget --reconcile`・BUG-250）。

手元の中継の台帳は一時の場所の偽物（conftest が ABEVAL_RELAY_LEDGER を向ける）で、読むだけ。abeval.proxy の
待ち受けは `ss` の出力の偽物で決める（18200 には繋がない）。
"""

from __future__ import annotations

import fcntl
import json
import os
import threading
import time

import pytest

from abeval import budget
from abeval.config import load_config
from abeval.ledger import Ledger, LedgerCorrupt
from abeval.proxy import ProxyState


def relay_lines(cfg, rows: list[tuple[str, float]]) -> None:
    cfg.paths.relay_ledger.parent.mkdir(parents=True, exist_ok=True)
    with cfg.paths.relay_ledger.open("a") as f:
        for tag, usd in rows:
            f.write(
                json.dumps({"ts": "t", "model": "m", "status": 200, "tag": tag, "cost_usd": usd})
            )
            f.write("\n")


@pytest.fixture()
def cfg(monkeypatch):
    c = load_config()
    # abeval.proxy は止まっている（18200 に待ち受けが無い）。
    monkeypatch.setattr("abeval.server_guard.run_ss", lambda port: "")
    return c


def crashed_world(cfg):
    """t1 は評価の台帳にもある。t2 は落ちた中継で処理中だった（手元の中継にだけある）。"""
    led = Ledger(cfg.paths.ledger)
    led.append(
        {"cost_micros": 750_001, "exp": "e0", "run_id": "r1", "trial": "t1",
         "tag": "abeval/e0/r1/t1"}
    )  # fmt: skip
    led.append(
        {"cost_micros": 0, "exp": "e0", "run_id": "r1", "trial": "capture", "captured": True}
    )
    relay_lines(
        cfg,
        [
            ("abeval/e0/r1/t1", 0.5),
            ("abeval/e0/r1/t1", 0.25),
            ("abeval/e0/r1/t2", 0.1),
            ("abeval/e0/r1/-", 0.0000015),
            ("", 3.0),  # 評価でない要求（数えない）
            ("decision-eval-diag", 1.0),
        ],
    )


def test_shortfalls_are_per_tag_and_never_count_less(cfg):
    crashed_world(cfg)
    got = {s.tag: s for s in budget.shortfalls(cfg)}
    # t1 は評価の台帳の方が多い（切り上げ）ので足さない。t2 は全額、"-" は 2 マイクロに切り上げる。
    assert set(got) == {"abeval/e0/r1/t2", "abeval/e0/r1/-"}
    assert got["abeval/e0/r1/t2"].add_micros == 100_000 and got["abeval/e0/r1/t2"].relay_lines == 1
    assert got["abeval/e0/r1/-"].add_micros == 2
    text = budget.format_shortfalls(list(got.values()))
    assert "abeval/e0/r1/t2" in text and "0.100000" in text and "合計" in text


def test_without_yes_nothing_is_written(cfg, capsys):
    crashed_world(cfg)
    before = cfg.paths.ledger.read_bytes()
    relay_before = cfg.paths.relay_ledger.read_bytes()
    assert budget.main(["--reconcile"]) == 0
    out = capsys.readouterr().out
    assert "abeval/e0/r1/t2" in out and "--yes" in out
    assert cfg.paths.ledger.read_bytes() == before
    assert cfg.paths.relay_ledger.read_bytes() == relay_before


def test_yes_adds_marked_lines_and_lets_the_guard_pass(cfg, capsys):
    crashed_world(cfg)
    assert budget.ledger_guard(cfg) is not None
    relay_before = cfg.paths.relay_ledger.read_bytes()
    assert budget.main(["--reconcile", "--yes"]) == 0
    assert "2 行" in capsys.readouterr().out
    added = [e for e in Ledger(cfg.paths.ledger).entries() if e.get("kind") == budget.RECONCILED]
    by_tag = {e["tag"]: e for e in added}
    t2 = by_tag["abeval/e0/r1/t2"]
    assert (t2["exp"], t2["run_id"], t2["trial"], t2["cost_micros"]) == ("e0", "r1", "t2", 100_000)
    assert t2["relay_lines"] == 1 and t2["eval_micros_before"] == 0
    assert by_tag["abeval/e0/r1/-"]["trial"] is None
    assert budget.ledger_guard(cfg) is None
    # 実行ごとの累計にも入る（abeval.proxy の実行の上限も、この分を使ったものとして数える）。
    assert Ledger(cfg.paths.ledger).refresh().run_micros("r1") == 750_001 + 100_000 + 2
    # もう一度流しても足さない。手元の中継の台帳は書かない。
    assert budget.main(["--reconcile", "--yes"]) == 0
    assert "足す行はありません" in capsys.readouterr().out
    assert len(Ledger(cfg.paths.ledger).entries()) == 4
    assert cfg.paths.relay_ledger.read_bytes() == relay_before


@pytest.mark.parametrize(
    ("ss_out", "why"),
    [
        ("LISTEN 0 5 127.0.0.1:18200 0.0.0.0:*\n", "動いています"),
        (None, "ss がありません"),
    ],
)
def test_a_running_or_unknown_proxy_refuses_to_write(cfg, monkeypatch, capsys, ss_out, why):
    crashed_world(cfg)
    monkeypatch.setattr("abeval.server_guard.run_ss", lambda port: ss_out)
    before = cfg.paths.ledger.read_bytes()
    assert budget.main(["--reconcile", "--yes"]) == 1
    assert why in capsys.readouterr().out
    assert cfg.paths.ledger.read_bytes() == before


def test_yes_needs_reconcile():
    with pytest.raises(SystemExit):
        budget.main(["--yes"])


def test_the_guard_reason_shows_the_way_back(cfg):
    crashed_world(cfg)
    why = budget.ledger_guard(cfg)
    assert why and "budget --reconcile" in why and "--yes" in why and "上限と台帳" in why


def test_the_write_waits_for_the_ledger_lock(cfg):
    """読み直しと書き込みは評価の台帳の排他の錠（flock）の中で行う（abeval.proxy の書き込みと交ざらない）。"""
    crashed_world(cfg)
    # 錠を取らずに読む（同じプロセスでも、別に開いたものの flock は待つため）。
    before = cfg.paths.ledger.read_bytes()
    fd = os.open(cfg.paths.ledger, os.O_RDONLY)
    fcntl.flock(fd, fcntl.LOCK_EX)
    t = threading.Thread(target=budget.reconcile, args=(cfg,), daemon=True)
    try:
        t.start()
        time.sleep(0.3)
        assert t.is_alive() and cfg.paths.ledger.read_bytes() == before
        # 錠の間に別の書き込み（t2 の分が届いた）があれば、読み直した中身で決める。
        w = os.open(cfg.paths.ledger, os.O_WRONLY | os.O_APPEND)
        os.write(
            w,
            (json.dumps({"cost_micros": 100_000, "tag": "abeval/e0/r1/t2"}) + "\n").encode(),
        )
        os.close(w)
    finally:
        fcntl.flock(fd, fcntl.LOCK_UN)
        os.close(fd)
    t.join(5)
    added = [e for e in Ledger(cfg.paths.ledger).entries() if e.get("kind") == budget.RECONCILED]
    assert [e["tag"] for e in added] == ["abeval/e0/r1/-"]


def test_a_broken_ledger_writes_nothing(cfg):
    crashed_world(cfg)
    with cfg.paths.ledger.open("a") as f:
        f.write('{"cost_micros": 1')
    with pytest.raises(LedgerCorrupt):
        budget.reconcile(cfg)
    assert cfg.paths.ledger.read_text().endswith('{"cost_micros": 1')


def test_a_killed_proxy_is_recovered(cfg):
    """処理中の要求を残したまま中継が落ちた（SIGKILL・abandon_inflight が走らない）場面を組む。"""
    state = ProxyState(cfg, upstream="http://127.0.0.1:9")
    state.register_run({"run_id": "r9", "exp": "e9", "cap_usd": 1.0})
    state.register_session({"session": "th1", "trial": "t9", "run_id": "r9"})
    req = {"model": "claude-haiku-5-5", "max_tokens": 100, "messages": []}
    ticket = state.admit(req, 2_000, "th1", True)
    # 手元の中継は上流に送って課金した（中継の行は tag で数える）。abeval.proxy は SIGKILL で何も書かない。
    exp, run_id, trial = ticket.tag_parts
    relay_lines(cfg, [(f"abeval/{exp}/{run_id}/{trial}", 0.05)])
    del state
    assert budget.ledger_guard(cfg) is not None
    done = budget.reconcile(cfg)
    assert [s.tag for s in done] == ["abeval/e9/r9/t9"] and done[0].add_micros == 50_000
    assert budget.ledger_guard(cfg) is None
