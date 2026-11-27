#!/usr/bin/env bash
# 変更行の変異テスト。PR が変えた行にだけ小さな変異（`>` を `>=` に、戻り値を既定値に、など）を入れ、
# テストがそれを捕まえるかを見る。カバレッジは「行が実行されたか」しか見ないが、これは「テストがその行の
# 振る舞いを確かめているか」を見る。
#
# このスクリプトは判定しない（常に 0 で終わる）。結果を mutants-report.json に書くだけで、生き残った変異を
# ワーカーへの差し戻しと検品に使うのはファクトリー（imanect の factory リポジトリの gate）の側。
#
# 使い方: scripts/check-mutants.sh <base> [<head>]
#   MUTANTS_CAP   試す変異の上限（既定 30）。超えたら round-robin で間引く
#   MUTANTS_OUT   cargo-mutants の出力先（既定 target/mutants）
#
# 結合テストが接続先の env で黙ってスキップされると、捕まえられたはずの変異が生き残って見える。
# CI では SHIKI_REQUIRE_IT=1 を立てて、未設定を失敗にした状態で流すこと（crates/testenv）。
set -euo pipefail

base=${1:?base を指定してください}
head=${2:-HEAD}
cap=${MUTANTS_CAP:-30}
out=${MUTANTS_OUT:-target/mutants}
report=${MUTANTS_REPORT:-mutants-report.json}

cd "$(git rev-parse --show-toplevel)"
mkdir -p "$out"
git diff "$base" "$head" -- '*.rs' >"$out/pr.diff"

total=0
if [ -s "$out/pr.diff" ]; then
  total=$(cargo mutants --workspace --in-diff "$out/pr.diff" --list | grep -c ':' || true)
fi

shard=()
if [ "$total" -gt "$cap" ]; then
  k=$(((total + cap - 1) / cap))
  shard=(--shard "0/$k" --sharding round-robin)
fi

code=0
if [ "$total" -gt 0 ]; then
  # 終了コード: 0 全部捕まえた / 2 生き残りあり / 3 時間切れあり / 4 変異なしでもテストが落ちる（baseline）
  cargo mutants --workspace --in-place --in-diff "$out/pr.diff" -o "$out" "${shard[@]}" \
    --minimum-test-timeout 120 || code=$?
fi

python3 - "$out" "$total" "$code" "$report" <<'PY'
import json, os, sys

out, total, code, report = sys.argv[1], int(sys.argv[2]), int(sys.argv[3]), sys.argv[4]
res = {"version": 1, "total": total, "tested": 0, "caught": 0, "unviable": 0, "missed": [], "timeout": []}
if total == 0:
    res["status"] = "no_mutants"
elif code in (0, 2, 3):
    res["status"] = "ok"
elif code == 4:
    res["status"] = "baseline_failed"
else:
    res["status"] = "error"
    res["exit_code"] = code

path = os.path.join(out, "mutants.out", "outcomes.json")
if total and os.path.exists(path):
    with open(path) as f:
        outcomes = json.load(f)["outcomes"]
    for o in outcomes:
        sc = o.get("scenario")
        if not isinstance(sc, dict) or "Mutant" not in sc:
            continue
        m = sc["Mutant"]
        res["tested"] += 1
        kind = o.get("summary")
        if kind == "CaughtMutant":
            res["caught"] += 1
        elif kind == "Unviable":
            res["unviable"] += 1
        elif kind in ("MissedMutant", "Timeout"):
            diff = ""
            if o.get("diff_path"):
                try:
                    # 前後の文脈は落とし、変えた行（-）と変異後の行（+）だけを残す
                    with open(os.path.join(out, "mutants.out", o["diff_path"])) as f:
                        lines = [l for l in f.read().splitlines()
                                 if l[:1] in "+-" and not l.startswith(("---", "+++"))]
                    diff = "\n".join(lines)[:1500]
                except OSError:
                    pass
            item = {
                "name": m["name"],
                "file": m["file"],
                "line": m["span"]["start"]["line"],
                "function": (m.get("function") or {}).get("function_name", ""),
                "genre": m.get("genre", ""),
                "diff": diff,
            }
            res["missed" if kind == "MissedMutant" else "timeout"].append(item)
res["sampled"] = res["tested"] < total

with open(report, "w") as f:
    json.dump(res, f, ensure_ascii=False, indent=1)
viable = res["caught"] + len(res["missed"])
print(f"変更行の変異 {total} 個のうち {res['tested']} 個を試した: 捕まえた {res['caught']}、"
      f"生き残り {len(res['missed'])}、時間切れ {len(res['timeout'])}、コンパイル不可 {res['unviable']}"
      f"（{res['status']}）")
for m in res["missed"]:
    print(f"  生き残り: {m['name']}")
PY
