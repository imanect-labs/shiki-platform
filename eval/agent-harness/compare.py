"""版と variant を横に並べた比較表（Markdown）を作る。

使い方: python3 eval/agent-harness/compare.py eval/agent-harness/results/v10-w*.jsonl eval/agent-harness/results/v11-w*.jsonl

行は (窓, 課題)、列は (版, variant)。各マスは「成功 / run 数（平均点）」。プロバイダの一時障害
（ストリームの切断など）で止まった run と、利用枠が尽きて止まった run（402）は分母から除き、
件数を `障害 n`・`中断 n` として添える（ハーネスの失敗ではないため）。最後に窓ごとの合計行を付ける。
"""

import json
import sys
from collections import defaultdict
from pathlib import Path


def is_infra(r: dict) -> bool:
    return "provider unavailable" in r.get("stop", "")


def is_halted(r: dict) -> bool:
    """利用枠（クレジット）が尽きて止まった run（402）。"""
    return "402 Payment Required" in r.get("stop", "")


def cell(rs: list[dict]) -> str:
    ok_runs = [r for r in rs if not is_infra(r) and not is_halted(r)]
    infra = sum(1 for r in rs if is_infra(r))
    halted = sum(1 for r in rs if is_halted(r))
    note = (f" 障害 {infra}" if infra else "") + (f" 中断 {halted}" if halted else "")
    if not ok_runs:
        return f"—{note}" if note else ""
    n = len(ok_runs)
    ok = sum(1 for r in ok_runs if r["success"])
    score = sum(r["score"] for r in ok_runs) / n
    return f"{ok}/{n}（{score:.2f}）{note}"


def main(paths: list[str]) -> None:
    rows: dict[tuple, dict[tuple, list[dict]]] = defaultdict(lambda: defaultdict(list))
    cols: set[tuple] = set()
    for p in paths:
        version = Path(p).stem.split("-w")[0]
        for line in Path(p).read_text(encoding="utf-8").splitlines():
            if not line.strip():
                continue
            r = json.loads(line)
            col = (version, r["variant"])
            cols.add(col)
            rows[(r["window"], r["task"])][col].append(r)
    order = {"baseline": 0, "improved": 1, "full": 2}
    cols_sorted = sorted(cols, key=lambda c: (order.get(c[1], 9), c[0]))
    print("| 窓 | 課題 | " + " | ".join(f"{v} {var}" for v, var in cols_sorted) + " |")
    print("| --- | --- | " + " | ".join("---" for _ in cols_sorted) + " |")
    totals: dict[tuple, list[dict]] = defaultdict(list)
    for key in sorted(rows):
        window, task = key
        cells = []
        for c in cols_sorted:
            rs = rows[key].get(c, [])
            totals[(window, c)].extend(rs)
            cells.append(cell(rs))
        print(f"| {window // 1024}k | {task} | " + " | ".join(cells) + " |")
    for window in sorted({k[0] for k in rows}):
        cells = [cell(totals[(window, c)]) for c in cols_sorted]
        print(f"| {window // 1024}k | **合計** | " + " | ".join(cells) + " |")


if __name__ == "__main__":
    main(sys.argv[1:])
