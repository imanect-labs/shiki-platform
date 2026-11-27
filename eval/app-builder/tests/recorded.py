"""記録（tests/fixtures/）を読む補助。記録は scripts/record_fixtures.py が作る。

- `lint()`: 中核の課題の正解・壊し方・null を ir_lint に掛けた結果（id → LintResult）。課題を変えて記録し直して
  いなければ（入力の IR の sha が違えば）止める。
- `catalog()`: `ir_lint --catalog` の記録。
- `run(task, variant, case)`: stub のスタックで流した run の記録（IR・出口・CaseRecord）。
"""

from __future__ import annotations

import json
from functools import lru_cache
from pathlib import Path

from abeval.config import canonical_json, sha256_hex
from abeval.grade.dynamic import CaseRecord
from abeval.grade.static import LintCatalog, LintResult, load_catalog
from abeval.tasks import load_tasks

HERE = Path(__file__).resolve().parent
LINT_DIR = HERE / "fixtures" / "ir_lint"
RUNS_DIR = HERE / "fixtures" / "runs"
RERECORD = "課題か ir_lint が変わっています。uv run python scripts/record_fixtures.py --lint で記録し直します"


@lru_cache(maxsize=1)
def catalog() -> LintCatalog:
    return load_catalog(str(LINT_DIR / "catalog.json"))


@lru_cache(maxsize=1)
def lint_lines() -> dict[str, dict]:
    out = {}
    for line in (LINT_DIR / "core.jsonl").read_text(encoding="utf-8").splitlines():
        d = json.loads(line)
        if "id" in d:
            out[d["id"]] = d
    return out


def lint() -> dict[str, LintResult]:
    return {k: LintResult.from_json(v) for k, v in lint_lines().items()}


def check_fresh(items: list[tuple[str, dict]]) -> None:
    """記録が今の入力と同じ IR のものか（違えば記録し直しを促して落とす）。"""
    lines = lint_lines()
    stale = [
        i
        for i, ir in items
        if i not in lines or lines[i]["ir_sha"] != sha256_hex(canonical_json(ir))
    ]
    assert not stale, f"{RERECORD}: {stale[:5]}"


@lru_cache(maxsize=1)
def tasks() -> dict:
    return {t.id: t for t in load_tasks()}


def run(task: str, variant: str, case: str) -> tuple[dict, dict, CaseRecord]:
    """(役割 → IR, 役割 → 出口, 記録)。"""
    d = json.loads((RUNS_DIR / f"{task}.{variant}.{case}.json").read_text(encoding="utf-8"))
    return d["irs"], d["node_ports"], CaseRecord.from_dict(d["record"])


def runs() -> list[tuple[str, str, str]]:
    out = []
    for p in sorted(RUNS_DIR.glob("*.json")):
        task, variant, case = p.name[: -len(".json")].split(".")
        out.append((task, variant, case))
    return out
