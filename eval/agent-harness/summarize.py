"""評価結果（JSONL）を集計して Markdown の表にする。

使い方: python3 eval/agent-harness/summarize.py eval/agent-harness/results/*.jsonl
同じ (版, モデル, 課題, variant) の run をまとめ、成功数・平均点・溢れ・平均ステップ・平均費用を出す。
ファイル名の接頭辞（`baseline-w32k` → `baseline`、`v3-w32k` → `v3`）を「版」として扱う。
"""

import json
import sys
from collections import defaultdict
from pathlib import Path


def main(paths: list[str]) -> None:
    rows: dict[tuple, list[dict]] = defaultdict(list)
    for p in paths:
        version = Path(p).stem.split("-w")[0]
        for line in Path(p).read_text(encoding="utf-8").splitlines():
            if not line.strip():
                continue
            r = json.loads(line)
            rows[(version, r["model"], r["task"], r["variant"], r["window"])].append(r)
    print("| 版 | モデル | 課題 | variant | 窓 | 成功 | 平均点 | 溢れ | 障害 | 平均ステップ | 平均圧縮 | 平均読み戻し | 平均費用 |")
    print("| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |")
    for key in sorted(rows):
        rs = rows[key]
        n = len(rs)
        ok = sum(1 for r in rs if r["success"])
        score = sum(r["score"] for r in rs) / n
        overflow = sum(1 for r in rs if r.get("overflow"))
        # プロバイダの一時障害（ストリームの切断など）で止まった run。ハーネスの失敗と分けて数える。
        infra = sum(1 for r in rs if "provider unavailable" in r.get("stop", ""))
        steps = sum(r["steps"] for r in rs) / n
        comp = sum(r.get("compactions", 0) for r in rs) / n
        cost = sum(r.get("cost_usd_est", 0.0) for r in rs) / n
        recalls = sum(r.get("recalls", 0) for r in rs) / n
        version, model, task, variant, window = key
        print(
            f"| {version} | {model} | {task} | {variant} | {window // 1024}k | {ok}/{n} | "
            f"{score:.2f} | {overflow} | {infra} | {steps:.1f} | {comp:.1f} | {recalls:.1f} | ${cost:.4f} |"
        )


if __name__ == "__main__":
    main(sys.argv[1:])
