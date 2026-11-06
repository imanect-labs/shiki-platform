"""評価の台帳（JSONL・追記だけ・月で区切らない累計）。

- 1 行 = 1 回の要求。費用は `cost_micros`（マイクロ USD の整数）。`run_id` があれば実行ごとの累計にも入る。
- 追記は `fcntl.flock` の排他の錠を取ってから書き、1 行ごとに fsync する（スレッドとプロセスをまたいで正確）。
- 読むときは共有の錠を取る。壊れた行があれば例外にする（少なく数えて上限を越えないため）。
- 鍵の形の値（`sk-ant-` を含む値）と、名前に key・authorization を含む項目は書き込みを拒む。

手元の中継の台帳（`~/.local/state/shiki-campaign/anthropic-spend.jsonl`）は読むだけ（[`read_relay_total_usd`]）。
"""

from __future__ import annotations

import fcntl
import json
import os
import threading
from collections.abc import Mapping
from dataclasses import dataclass, field
from pathlib import Path

LEDGER_NAME_PARTS = ("key", "authorization")
KEY_MARKER = "sk-ant-"


class LedgerCorrupt(ValueError):
    """台帳に読めない行がある（数え直さずに止める）。"""


class SecretInLedger(ValueError):
    """鍵の形の値・項目を台帳に書こうとした。"""


def _secret_like(value: object, at: str = "") -> str | None:
    if isinstance(value, Mapping):
        for k, v in value.items():
            here = f"{at}.{k}" if at else str(k)
            if any(part in str(k).lower() for part in LEDGER_NAME_PARTS):
                return here
            hit = _secret_like(v, here)
            if hit:
                return hit
    elif isinstance(value, list | tuple):
        for i, v in enumerate(value):
            hit = _secret_like(v, f"{at}[{i}]")
            if hit:
                return hit
    elif isinstance(value, str) and KEY_MARKER in value.lower():
        return at or "<value>"
    return None


def check_entry(entry: Mapping) -> None:
    """書いてよい行か確かめる（鍵の形の項目・値が無い・費用が 0 以上の整数）。"""
    hit = _secret_like(entry)
    if hit:
        raise SecretInLedger(f"台帳に鍵の形の項目・値は書けません: {hit}")
    cost = entry.get("cost_micros")
    if isinstance(cost, bool) or not isinstance(cost, int) or cost < 0:
        raise ValueError(f"cost_micros は 0 以上の整数です: {cost!r}")


@dataclass
class Totals:
    total_micros: int = 0
    lines: int = 0
    by_run: dict[str, int] = field(default_factory=dict)

    def run_micros(self, run_id: str) -> int:
        return self.by_run.get(run_id, 0)


def _parse_line(line: bytes, lineno: int, path: Path) -> dict:
    try:
        entry = json.loads(line)
    except ValueError as e:
        raise LedgerCorrupt(f"{path}:{lineno}: JSON として読めません: {e}") from e
    if not isinstance(entry, dict):
        raise LedgerCorrupt(f"{path}:{lineno}: 行がオブジェクトではありません")
    cost = entry.get("cost_micros")
    if isinstance(cost, bool) or not isinstance(cost, int) or cost < 0:
        raise LedgerCorrupt(f"{path}:{lineno}: cost_micros が 0 以上の整数ではありません: {cost!r}")
    return entry


class Ledger:
    """評価の台帳。`refresh()` は前回の続きから読み足す（追記だけなので読み直さない）。"""

    def __init__(self, path: Path):
        self.path = Path(path)
        self._lock = threading.Lock()
        self._offset = 0
        self._totals = Totals()

    def append(self, entry: Mapping) -> None:
        check_entry(entry)
        data = (json.dumps(entry, ensure_ascii=False, sort_keys=True) + "\n").encode()
        self.path.parent.mkdir(parents=True, exist_ok=True)
        fd = os.open(self.path, os.O_WRONLY | os.O_APPEND | os.O_CREAT, 0o600)
        try:
            fcntl.flock(fd, fcntl.LOCK_EX)
            try:
                view = memoryview(data)
                while view:
                    n = os.write(fd, view)
                    view = view[n:]
                os.fsync(fd)
            finally:
                fcntl.flock(fd, fcntl.LOCK_UN)
        finally:
            os.close(fd)

    def _read_from(self, offset: int) -> tuple[bytes, int]:
        try:
            fd = os.open(self.path, os.O_RDONLY)
        except FileNotFoundError:
            return b"", offset
        try:
            fcntl.flock(fd, fcntl.LOCK_SH)
            try:
                os.lseek(fd, offset, os.SEEK_SET)
                chunks = []
                while True:
                    chunk = os.read(fd, 1 << 20)
                    if not chunk:
                        break
                    chunks.append(chunk)
            finally:
                fcntl.flock(fd, fcntl.LOCK_UN)
        finally:
            os.close(fd)
        data = b"".join(chunks)
        return data, offset + len(data)

    def refresh(self) -> Totals:
        """前回の続きから読み足した累計（写し）を返す。"""
        with self._lock:
            data, end = self._read_from(self._offset)
            if data and not data.endswith(b"\n"):
                raise LedgerCorrupt(f"{self.path}: 最後の行が途中で切れています")
            totals = Totals(
                total_micros=self._totals.total_micros,
                lines=self._totals.lines,
                by_run=dict(self._totals.by_run),
            )
            for raw in data.splitlines():
                totals.lines += 1
                if not raw.strip():
                    raise LedgerCorrupt(f"{self.path}:{totals.lines}: 空の行があります")
                entry = _parse_line(raw, totals.lines, self.path)
                totals.total_micros += entry["cost_micros"]
                run_id = entry.get("run_id")
                if isinstance(run_id, str) and run_id:
                    totals.by_run[run_id] = totals.by_run.get(run_id, 0) + entry["cost_micros"]
            self._totals = totals
            self._offset = end
            return Totals(totals.total_micros, totals.lines, dict(totals.by_run))

    def entries(self) -> list[dict]:
        """全部の行（読み直す）。"""
        data, _ = self._read_from(0)
        if data and not data.endswith(b"\n"):
            raise LedgerCorrupt(f"{self.path}: 最後の行が途中で切れています")
        return [_parse_line(raw, i, self.path) for i, raw in enumerate(data.splitlines(), 1)]


def read_relay_total_usd(path: Path) -> float:
    """手元の中継の台帳の累計（USD）。読むだけ。壊れた行があれば例外にする（中継へは繋がない）。"""
    total = 0.0
    with Path(path).open("rb") as f:
        for lineno, raw in enumerate(f, 1):
            if not raw.strip():
                continue
            try:
                entry = json.loads(raw)
                cost = float(entry.get("cost_usd", 0))
            except (ValueError, AttributeError, TypeError) as e:
                raise LedgerCorrupt(f"{path}:{lineno}: 読めません: {e}") from e
            total += cost
    return total
