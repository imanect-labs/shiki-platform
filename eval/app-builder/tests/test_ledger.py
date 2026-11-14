from __future__ import annotations

import json
import os
import subprocess
import sys
import threading

import pytest

from abeval.config import ROOT
from abeval.ledger import (
    Ledger,
    LedgerCorrupt,
    SecretInLedger,
    check_entry,
    read_relay_total_usd,
)


def _entry(cost: int, run: str = "r1", ts: str = "2026-11-06T00:00:00+0000") -> dict:
    return {"ts": ts, "run_id": run, "model": "claude-haiku-5-5", "cost_micros": cost}


def test_append_and_totals(tmp_path):
    led = Ledger(tmp_path / "spend.jsonl")
    assert led.refresh().total_micros == 0
    led.append(_entry(10, "a"))
    led.append(_entry(5, "b"))
    led.append(_entry(7, "a"))
    t = led.refresh()
    assert (t.total_micros, t.lines) == (22, 3)
    assert t.by_run == {"a": 17, "b": 5}
    assert t.run_micros("missing") == 0
    # 続きから読み足す（別のインスタンスが書いた分も拾う）。
    Ledger(led.path).append(_entry(100, "c"))
    t = led.refresh()
    assert (t.total_micros, t.lines) == (122, 4)
    assert len(led.entries()) == 4
    assert oct(os.stat(led.path).st_mode & 0o777) == oct(0o600)


def test_months_are_not_separated(tmp_path):
    led = Ledger(tmp_path / "spend.jsonl")
    led.append(_entry(30, ts="2026-10-31T23:59:59+0900"))
    led.append(_entry(40, ts="2026-11-01T00:00:01+0900"))
    assert led.refresh().total_micros == 70


def test_concurrent_threads(tmp_path):
    led = Ledger(tmp_path / "spend.jsonl")

    def work(i: int):
        for j in range(50):
            led.append(_entry(i * 1000 + j, f"t{i}"))

    threads = [threading.Thread(target=work, args=(i,)) for i in range(8)]
    for t in threads:
        t.start()
    for t in threads:
        t.join()
    totals = Ledger(led.path).refresh()
    assert totals.lines == 400
    assert totals.total_micros == sum(i * 1000 + j for i in range(8) for j in range(50))
    for i in range(8):
        assert totals.by_run[f"t{i}"] == sum(i * 1000 + j for j in range(50))


_CHILD = """
import sys
from pathlib import Path
from abeval.ledger import Ledger
led = Ledger(Path(sys.argv[1]))
proc = int(sys.argv[2])
for j in range(200):
    led.append({"run_id": f"p{proc}", "cost_micros": j + 1, "pad": "x" * 3000})
"""


def test_concurrent_processes(tmp_path):
    path = tmp_path / "spend.jsonl"
    env = {**os.environ, "PYTHONPATH": str(ROOT)}
    procs = [
        subprocess.Popen([sys.executable, "-c", _CHILD, str(path), str(p)], env=env)
        for p in range(2)
    ]
    # 同時にスレッドからも書く。
    led = Ledger(path)
    for _ in range(100):
        led.append(_entry(1, "main"))
    for p in procs:
        assert p.wait(timeout=120) == 0
    totals = Ledger(path).refresh()
    assert totals.lines == 500
    assert totals.by_run == {"p0": 20_100, "p1": 20_100, "main": 100}
    assert totals.total_micros == 40_300
    # どの行も 1 行の JSON として読める（混ざっていない）。
    for line in path.read_bytes().splitlines():
        json.loads(line)


def test_corrupt_line_is_an_error(tmp_path):
    path = tmp_path / "spend.jsonl"
    led = Ledger(path)
    led.append(_entry(10))
    with path.open("a") as f:
        f.write("{not json\n")
    with pytest.raises(LedgerCorrupt):
        Ledger(path).refresh()
    with pytest.raises(LedgerCorrupt):
        led.entries()


@pytest.mark.parametrize(
    "line",
    [
        '{"cost_micros": -1}\n',
        '{"cost_micros": 1.5}\n',
        '{"run_id": "x"}\n',
        "[1, 2]\n",
        "\n",
        '{"cost_micros": 1}',  # 末尾の改行が無い（途中で切れた）
    ],
)
def test_bad_lines_are_errors(tmp_path, line):
    path = tmp_path / "spend.jsonl"
    Ledger(path).append(_entry(1))
    with path.open("a") as f:
        f.write(line)
    led = Ledger(path)
    with pytest.raises(LedgerCorrupt):
        led.refresh()
    # 誤りの後も累計は進めていない。
    assert led._offset == 0


@pytest.mark.parametrize(
    "entry",
    [
        {"cost_micros": 1, "x-api-key": "abeval-dummy"},
        {"cost_micros": 1, "headers": {"Authorization": "Bearer t"}},
        {"cost_micros": 1, "note": "sk-ant-api03-abc"},
        {"cost_micros": 1, "list": ["ok", "SK-ANT-x"]},
        {"cost_micros": 1, "api_key_hash": "abc"},
    ],
)
def test_secret_like_entries_are_refused(tmp_path, entry):
    path = tmp_path / "spend.jsonl"
    with pytest.raises(SecretInLedger):
        Ledger(path).append(entry)
    assert not path.exists()


def test_cost_must_be_a_non_negative_int():
    for bad in (None, -1, 1.0, True, "1"):
        with pytest.raises(ValueError):
            check_entry({"cost_micros": bad})
    check_entry({"cost_micros": 0})


def test_relay_ledger_is_read_only_and_strict(tmp_path):
    path = tmp_path / "anthropic-spend.jsonl"
    path.write_text(
        '{"ts":"x","model":"claude-haiku-5-5","status":200,"tag":"","usage":{},"cost_usd":0.25}\n'
        '{"ts":"y","cost_usd":1.5}\n\n'
    )
    before = path.read_bytes()
    assert read_relay_total_usd(path) == pytest.approx(1.75)
    assert path.read_bytes() == before
    path.write_text(before.decode() + "broken\n")
    with pytest.raises(LedgerCorrupt):
        read_relay_total_usd(path)
    with pytest.raises(FileNotFoundError):
        read_relay_total_usd(tmp_path / "missing.jsonl")
