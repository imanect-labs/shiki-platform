"""指標の集計（S1〜S9・課題ごと・pass@k・対応のあるブートストラップ・GAP-30 §5.1・§7.5）。

- 率の分母: S1・S4・見出しは採点した全試行、S2s・S2w・S2d は保存できた試行（S1）のうち値のあるもの、S3 は
  実行した試行、S5 は IR のある試行。基盤の失敗（errors）は数えない。
- 見出し: Mode A は「正しく保存できる率@1」＝ S1 ∧ ¬S2s ∧ ¬S2d、Mode B は「使えるワークフロー率」＝
  S1 ∧ ¬S2s ∧ ¬S2d ∧ S4（どちらも試行の記録の `headline`）。
- pass@k は課題ごとの不偏推定 1 − C(n−c, k) / C(n, k) の平均（n は試行の数、c は見出しが真の数）。
- 条件の比べ方: 課題を単位にした対応のあるブートストラップ（既定 10,000 回・種は固定）。区間が 0 をまたぐ差は
  「差がある」としない。
"""

from __future__ import annotations

import math
import random
from collections.abc import Iterable, Mapping, Sequence
from typing import Any

BOOTSTRAP_ITERS = 10_000
BOOTSTRAP_SEED = 20261106


def pass_at_k(n: int, c: int, k: int) -> float:
    """1 課題の pass@k の不偏推定（n 回のうち c 回通った）。"""
    if not 0 <= c <= n or k < 1:
        raise ValueError(f"n={n} c={c} k={k}")
    if k > n:
        raise ValueError(f"k（{k}）が試行の数（{n}）より大きい")
    if n - c < k:
        return 1.0
    return 1.0 - math.comb(n - c, k) / math.comb(n, k)


def rate(values: Iterable[Any]) -> dict:
    """真偽の率（None は数えない）。"""
    vals = [bool(v) for v in values if v is not None]
    return {"n": len(vals), "true": sum(vals), "rate": (sum(vals) / len(vals)) if vals else None}


def condition_of(t: Mapping) -> tuple:
    """条件（版・モード・effort・手数・stub の試行か）。"""
    return (
        t.get("label"),
        t.get("mode"),
        t.get("effort"),
        t.get("max_steps"),
        bool(t.get("stub_run")),
    )


def effort_name(mode: str | None, effort: str | None) -> str:
    """effort の表示名。指定の無いときは、Mode B は製品の Auto（auto）、Mode A は思考なし（no_thinking）。"""
    if effort:
        return effort
    return "auto" if mode == "b" else "no_thinking"


def condition_name(cond: Sequence) -> str:
    label, mode, effort, steps = cond[:4]
    stub = len(cond) > 4 and cond[4]
    s = f"{label}/{mode}/{effort_name(mode, effort)}"
    return s + (f"/手数{steps}" if steps else "") + ("/stub" if stub else "")


def dedupe(trials: Iterable[Mapping], *, include_stub: bool = False) -> list[Mapping]:
    """試行をキーで重ねる（後のものを採る・キャッシュから流し直した同じ試行を二重に数えない）。stub の試行
    （`run --stub` の配管の確かめ）は既定で除く（実モデルの条件に混ぜない）。"""
    by_key: dict[str, Mapping] = {}
    rest: list[Mapping] = []
    for t in trials:
        if t.get("stub_run") and not include_stub:
            continue
        key = t.get("key")
        if isinstance(key, str):
            by_key.pop(key, None)
            by_key[key] = t
        else:
            rest.append(t)
    return [*rest, *by_key.values()]


def _s(t: Mapping, name: str) -> Any:
    return t.get(name)


def task_counts(trials: Iterable[Mapping], metric: str = "headline") -> dict[str, tuple[int, int]]:
    """課題 → (試行の数, metric が真の数)。metric が決まらない（None の）試行は数えない。"""
    out: dict[str, list[int]] = {}
    for t in trials:
        if t.get(metric) is None:
            continue
        n_c = out.setdefault(str(t["task"]), [0, 0])
        n_c[0] += 1
        n_c[1] += 1 if t.get(metric) else 0
    return {k: (v[0], v[1]) for k, v in out.items()}


def mean_pass_at_k(counts: Mapping[str, tuple[int, int]], k: int) -> float | None:
    vals = [pass_at_k(n, c, k) for n, c in counts.values() if n >= k]
    return sum(vals) / len(vals) if vals else None


def task_means(trials: Iterable[Mapping], metric: str = "headline") -> dict[str, float]:
    return {task: c / n for task, (n, c) in task_counts(trials, metric).items() if n}


def bootstrap_mean(
    values: Mapping[str, float], *, iters: int = BOOTSTRAP_ITERS, seed: int = BOOTSTRAP_SEED
) -> dict:
    """課題を単位にした平均の 95% 区間（課題を復元抽出）。"""
    tasks = sorted(values)
    if not tasks:
        return {"mean": None, "lo": None, "hi": None, "n_tasks": 0}
    rng = random.Random(seed)
    xs = [values[t] for t in tasks]
    n = len(xs)
    means = sorted(sum(xs[rng.randrange(n)] for _ in range(n)) / n for _ in range(iters))
    return {
        "mean": sum(xs) / n,
        "lo": means[int(0.025 * iters)],
        "hi": means[min(int(0.975 * iters), iters - 1)],
        "n_tasks": n,
    }


def paired_bootstrap(
    a: Mapping[str, float],
    b: Mapping[str, float],
    *,
    iters: int = BOOTSTRAP_ITERS,
    seed: int = BOOTSTRAP_SEED,
) -> dict:
    """b − a の差（課題ごとの対応・課題を復元抽出）。共通の課題だけを使う。"""
    tasks = sorted(set(a) & set(b))
    if not tasks:
        return {"diff": None, "lo": None, "hi": None, "n_tasks": 0, "significant": False}
    d = [b[t] - a[t] for t in tasks]
    n = len(d)
    rng = random.Random(seed)
    means = sorted(sum(d[rng.randrange(n)] for _ in range(n)) / n for _ in range(iters))
    lo, hi = means[int(0.025 * iters)], means[min(int(0.975 * iters), iters - 1)]
    return {
        "diff": sum(d) / n,
        "lo": lo,
        "hi": hi,
        "n_tasks": n,
        "significant": not (lo <= 0 <= hi),
    }


def _saved_only(trials: list[Mapping], name: str) -> list:
    return [t.get(name) for t in trials if t.get("s1")]


def _mean(xs: list[float]) -> float | None:
    return sum(xs) / len(xs) if xs else None


def aggregate(trials: Sequence[Mapping], *, iters: int = BOOTSTRAP_ITERS) -> list[dict]:
    """条件（版・モード・effort・手数）ごとの率・費用・時間・pass@k。"""
    groups: dict[tuple, list[Mapping]] = {}
    for t in trials:
        groups.setdefault(condition_of(t), []).append(t)
    rows = []
    for cond, ts in sorted(groups.items(), key=lambda kv: tuple(str(x) for x in kv[0])):
        mode = cond[1]
        counts = task_counts(ts)
        max_n = max((n for n, _ in counts.values()), default=0)
        cost = sum(_cost_micros(t) for t in ts)
        completed = sum(1 for _, c in counts.values() if c)
        row: dict[str, Any] = {
            "condition": condition_name(cond),
            "label": cond[0],
            "mode": mode,
            "effort": cond[2],
            "max_steps": cond[3],
            "trials": len(ts),
            "tasks": len(counts),
            "s1": rate(_s(t, "s1") for t in ts),
            "s2s": rate(_saved_only(ts, "s2s")),
            "s2w": rate(_saved_only(ts, "s2w")),
            "s2d": rate(_saved_only(ts, "s2d")),
            "s3": rate(_s(t, "s3") for t in ts),
            "s5": rate(_s(t, "s5") for t in ts),
            "headline": rate(_s(t, "headline") for t in ts),
            "headline_ci": bootstrap_mean(task_means(ts), iters=iters),
            "pass_at_k": {str(k): mean_pass_at_k(counts, k) for k in range(1, max_n + 1)},
            "cost_usd": cost / 1_000_000,
            "cost_per_completed_task_usd": (cost / 1_000_000 / completed) if completed else None,
            "mean_secs": _mean(
                [float(t["secs"]) for t in ts if isinstance(t.get("secs"), int | float)]
            ),
            "retries": sum(int(t.get("retries") or 0) for t in ts),
        }
        s8 = [t.get("s8") or {} for t in ts]
        row["s8"] = {k: sum(1 for x in s8 if x.get(k)) for k in sorted({k for x in s8 for k in x})}
        if mode == "a":
            row["status"] = dict(sorted(_count(t.get("status") for t in ts).items()))
        else:
            row["s4"] = rate(_s(t, "s4") for t in ts)
            s6 = [t.get("s6") or {} for t in ts]
            row["s6"] = {
                "emit_count": _mean([x.get("emit_count", 0) for x in s6]),
                "validation_errors": _mean([x.get("validation_errors", 0) for x in s6]),
                "steps": _mean([x.get("steps", 0) for x in s6]),
                "hit_step_limit": rate(x.get("hit_step_limit") for x in s6),
                "emit_loaded": rate(x.get("emit_loaded") for x in s6),
            }
            s9 = [t.get("s9") or {} for t in ts]
            row["s9"] = {
                "requests": sum(x.get("requests", 0) for x in s9),
                "approved": sum(x.get("approved", 0) for x in s9),
                "denied": sum(x.get("denied", 0) for x in s9),
                "serious": sum(1 for x in s9 if x.get("serious")),
                "tools": sorted({n for x in s9 for n in x.get("tools", [])}),
            }
        rows.append(row)
    return rows


def _count(values: Iterable[Any]) -> dict:
    out: dict = {}
    for v in values:
        out[str(v)] = out.get(str(v), 0) + 1
    return out


def _cost_micros(t: Mapping) -> int:
    c = t.get("cost")
    if isinstance(c, Mapping) and isinstance(c.get("total_micros"), int):
        return c["total_micros"]
    v = t.get("cost_micros")
    return v if isinstance(v, int) else 0


def failure_breakdown(trials: Sequence[Mapping]) -> dict:
    """失敗の内訳（検証エラーのコード・静的・動的の検査・生成の結果）。"""
    validation: dict[str, int] = {}
    lint: dict[str, int] = {}
    task: dict[str, int] = {}
    dynamic: dict[str, int] = {}
    for t in trials:
        for v in (t.get("validation") or {}).values():
            for c in v.get("errors") or []:
                validation[c] = validation.get(c, 0) + 1
        for c, n in ((t.get("s6") or {}).get("validation_codes") or {}).items():
            validation[c] = validation.get(c, 0) + n
        for r in ((t.get("static") or {}).get("roles") or {}).values():
            lr = r.get("lint") or {}
            for c in [
                *lr.get("errors", []),
                *lr.get("strict_warnings", []),
                *lr.get("run_blockers", []),
                *lr.get("enable_blockers", []),
                *lr.get("output_paths", []),
            ]:
                lint[c] = lint.get(c, 0) + 1
            for f in r.get("task") or []:
                task[f["code"]] = task.get(f["code"], 0) + 1
        for part in ("stub", "runs"):
            for case in (t.get(part) or {}).get("cases") or []:
                for f in (case.get("dynamic") or {}).get("findings") or []:
                    dynamic[f["code"]] = dynamic.get(f["code"], 0) + 1
    return {
        "validation": dict(sorted(validation.items(), key=lambda kv: -kv[1])),
        "lint": dict(sorted(lint.items(), key=lambda kv: -kv[1])),
        "task": dict(sorted(task.items(), key=lambda kv: -kv[1])),
        "dynamic": dict(sorted(dynamic.items(), key=lambda kv: -kv[1])),
    }
