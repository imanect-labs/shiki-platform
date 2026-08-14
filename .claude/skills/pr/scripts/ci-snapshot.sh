#!/usr/bin/env bash
# ci-snapshot.sh — ci.yml のうち「ローカルで追随すべき部分」を正規化する。
#
#   使い方:
#     ci-snapshot.sh            # 正規化結果を stdout へ出す
#     ci-snapshot.sh --check    # スナップショットと比較。差があれば diff を出して exit 1
#     ci-snapshot.sh --update   # スナップショットを現在の ci.yml で更新する
#
# なぜ要るか:
#   Phase 1 のローカルゲートは ci.yml を正本として local-gates.sh と gates.md が
#   引き写している。引き写しである以上、ci.yml が変わると黙って腐る。
#   これを人間や LLM の「思い出す」に頼らず、機構で強制検出する。
#
#   実際の取りこぼし（#455）: ci.yml の quality ジョブにステップを足した本人が、
#   同じセッション内で local-gates.sh への追随を忘れた。偶然気づくまで
#   「全ゲート通過」と報告される状態だった。
#
# 何を含め、何を含めないか:
#   含める  — paths-ignore（web ゲートの述語が写している）・ジョブ ID / 名前・
#             if 条件（実行条件を写している）・ステップの name と run。
#   含めない— uses / with / env / timeout-minutes / runs-on。
#             変更頻度が高い割にローカルコマンドへ写らず、churn だけが増える。
set -euo pipefail

ROOT=$(git rev-parse --show-toplevel 2>/dev/null) || { echo "git リポジトリ内で実行してください。" >&2; exit 2; }
cd "$ROOT"

CI_YML=".github/workflows/ci.yml"
SNAPSHOT=".claude/skills/pr/ci-jobs.snapshot"

[ -f "$CI_YML" ] || { echo "$CI_YML が見つかりません。" >&2; exit 2; }
command -v python3 >/dev/null 2>&1 || { echo "python3 が見つかりません。" >&2; exit 2; }

normalize() {
  python3 - "$CI_YML" <<'PY'
import re, sys

lines = open(sys.argv[1], encoding="utf-8").read().splitlines()

# YAML を構文解析せず、インデントと行頭パターンで拾う。PyYAML への依存を避けるため
# （CI の quality ジョブは追加の pip install をしない）。
out = []

def strip_comment(s):
    # run: の中身に # が出ることがあるので、行全体がコメントの場合だけ落とす。
    return "" if s.lstrip().startswith("#") else s.rstrip()

i = 0
n = len(lines)
in_jobs = False

# --- on: の paths-ignore ---
while i < n:
    line = lines[i]
    if re.match(r"^jobs:\s*$", line):
        in_jobs = True
        break
    if re.match(r"^\s+paths-ignore:\s*$", line):
        # 直上のイベント名（pull_request / push）を遡って特定する。
        event = "?"
        for j in range(i - 1, -1, -1):
            m = re.match(r"^  ([a-z_]+):\s*$", lines[j])
            if m:
                event = m.group(1)
                break
        i += 1
        items = []
        while i < n and re.match(r"^\s+-\s", lines[i]):
            items.append(lines[i].strip())
            i += 1
        out.append(f"on.{event}.paths-ignore:")
        out.extend(f"  {it}" for it in sorted(items))
        continue
    i += 1

if not in_jobs:
    print("jobs: が見つかりません", file=sys.stderr)
    sys.exit(2)

def indent_of(s):
    return len(s) - len(s.lstrip())

def emit_key(key, value, key_indent):
    """`name:` / `if:` / `run:` を 1 件出す。run のブロックスカラーは本文も取る。"""
    global i
    if key != "run":
        out.append(f"  step.{key}: {value.strip()}")
        return
    if re.match(r"^[|>][-+]?$", value.strip()):
        # ブロックスカラー。key より深いインデントの行が本文。
        body = []
        while i < n:
            nxt = lines[i]
            if nxt.strip() and indent_of(nxt) <= key_indent:
                break
            body.append(nxt.strip())
            i += 1
        body = [b for b in body if b and not b.startswith("#")]
        out.append("  run: |")
        out.extend(f"    {b}" for b in body)
    else:
        out.append(f"  run: {value.strip()}")

# --- jobs: 以降 ---
# ステップのキーは「`- ` のインデント + 2」の桁にだけ現れる。`with:` / `env:` などの
# ネストしたマッピングはそれより深いので、桁で弾ける。桁を見ずに `name:` を拾うと
# `with: { name: shiki-server-bin }` をステップ名として誤って記録してしまう。
i += 1
job = None
step_key_indent = None
while i < n:
    raw = lines[i]
    line = strip_comment(raw)
    i += 1
    if not line.strip():
        continue
    ind = indent_of(line)

    m = re.match(r"^  ([a-z][a-z0-9-]*):\s*$", line)
    if m:
        job = m.group(1)
        step_key_indent = None
        out.append("")
        out.append(f"job: {job}")
        continue
    if job is None:
        continue

    # ジョブ直下の name / if（インデント 4）
    m = re.match(r"^    (name|if):\s*(.+)$", line)
    if m:
        out.append(f"  job.{m.group(1)}: {m.group(2).strip()}")
        continue

    # ステップ列の先頭要素（`      - name: ...` / `      - run: ...` / `      - uses: ...`）
    m = re.match(r"^(\s*)-\s+([a-z-]+):\s*(.*)$", line)
    if m:
        step_key_indent = len(m.group(1)) + 2
        key, val = m.group(2), m.group(3)
        if key in ("name", "if", "run"):
            emit_key(key, val, step_key_indent)
        continue

    # 同じステップの後続キー。桁が一致するものだけ拾う（ネストは無視）。
    if step_key_indent is not None and ind == step_key_indent:
        m = re.match(r"^\s*([a-z-]+):\s*(.*)$", line)
        if m and m.group(1) in ("name", "if", "run"):
            emit_key(m.group(1), m.group(2), step_key_indent)
        continue

print("\n".join(out).strip() + "\n", end="")
PY
}

MODE="${1:-print}"
case "$MODE" in
  print)
    normalize
    ;;
  --update)
    mkdir -p "$(dirname "$SNAPSHOT")"
    normalize > "$SNAPSHOT"
    echo "スナップショットを更新しました: $SNAPSHOT"
    echo "⚠️  local-gates.sh と references/gates.md の追随を先に済ませてからコミットすること。"
    ;;
  --check)
    if [ ! -f "$SNAPSHOT" ]; then
      echo "スナップショットがありません: $SNAPSHOT" >&2
      echo "  初回は次で作成してください: $0 --update" >&2
      exit 1
    fi
    # プロセス置換ではなく一時ファイルを使う（diff の exit code を確実に受けるため）。
    tmp=$(mktemp)
    trap 'rm -f "$tmp" "$tmp.diff"' EXIT
    normalize > "$tmp"
    if diff -u "$SNAPSHOT" "$tmp" > "$tmp.diff" 2>&1; then
      echo "ci.yml ドリフトなし"
    else
      echo "ci.yml がスナップショットと食い違っています（- が記録済み / + が現在の ci.yml）:"
      # ヘッダ 2 行（---/+++）は情報量が無いので落とす。
      tail -n +3 "$tmp.diff"
      rm -f "$tmp.diff"
      exit 1
    fi
    rm -f "$tmp.diff"
    ;;
  -h|--help)
    sed -n '2,30p' "$0"
    ;;
  *)
    echo "未知の引数: $MODE" >&2
    exit 2
    ;;
esac
