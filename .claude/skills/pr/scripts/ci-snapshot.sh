#!/usr/bin/env bash
# ci-snapshot.sh — GitHub Actions ワークフローのうち「ローカルで追随すべき部分」を正規化する。
#
#   使い方:
#     ci-snapshot.sh            # 正規化結果を stdout へ出す
#     ci-snapshot.sh --check    # スナップショットと比較。差があれば diff を出して exit 1
#     ci-snapshot.sh --update   # スナップショットを現在のワークフローで更新する
#
# なぜ要るか:
#   Phase 1 のローカルゲートはワークフローを正本として local-gates.sh・references/gates.md・
#   AGENTS.md の検証コマンド表が引き写している。引き写しである以上、CI が変わると黙って腐る。
#   これを人間や LLM の「思い出す」に頼らず、機構で強制検出する。
#   実際の取りこぼし（#455）: ci.yml にステップを足した本人が、同じセッション内で
#   local-gates.sh への追随を忘れた。偶然気づくまで「全ゲート通過」と報告される状態だった。
#
# 設計の要点 — **既定で全部記録し、ごく少数だけ除外する**:
#   初版は「拾うキーを列挙する」許可リスト方式だったが、列挙から漏れたものが
#   *無言で* 監視外になった（working-directory / continue-on-error / needs / strategy /
#   shell / defaults、さらに jobs 先頭のジョブ ID が想定パターンに合わないと
#   そのジョブが丸ごと消える、等）。ゲート自身が取りこぼすのでは存在意義が無い。
#   そこで方式を反転し、YAML を実際に解析して全リーフを記録する。新しいキーが増えても
#   既定で監視対象に入る。
#
#   除外は churn しか生まないものに限る:
#     - timeout-minutes / runs-on / permissions / concurrency
#     - `uses:` はアクション名だけ記録し `@version` は落とす
#       （追加・削除は検出、バージョン bump では落ちない）
#
# なぜ PyYAML を使うか:
#   初版は行ベースで走査していたが、YAML として等価な書き換え（引用符・フロースタイル・
#   コメント・行継続・キー順）で誤検出し、逆にコメント 1 行で項目収集が打ち切られて
#   *無言で* 記録から消える経路があった。構文解析すれば両方消える。
#   PyYAML が無ければ黙って劣化させず、明示的に失敗する（fail-closed）。
set -euo pipefail

# cd する前に解決する。`$0` は起動時の cwd 基準の相対パスになり得るので、`cd` の後に
# 解決すると別の場所を指す（`cd .../scripts && ./ci-snapshot.sh --help` が壊れていた）。
SELF=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/$(basename "${BASH_SOURCE[0]}")

ROOT=$(git rev-parse --show-toplevel 2>/dev/null) || { echo "git リポジトリ内で実行してください。" >&2; exit 2; }
cd "$ROOT"

WORKFLOW_DIR=".github/workflows"
SNAPSHOT=".claude/skills/pr/ci-jobs.snapshot"

[ -d "$WORKFLOW_DIR" ] || { echo "$WORKFLOW_DIR が見つかりません。" >&2; exit 2; }
command -v python3 >/dev/null 2>&1 || { echo "python3 が見つかりません。" >&2; exit 2; }
python3 -c 'import yaml' 2>/dev/null || {
  echo "PyYAML が見つかりません（このゲートは YAML の構文解析が要ります）。" >&2
  echo "  例: pip install --user pyyaml" >&2
  exit 2
}

normalize() {
  python3 - "$WORKFLOW_DIR" <<'PY'
import sys, pathlib, yaml

WORKFLOW_DIR = pathlib.Path(sys.argv[1])

# churn しか生まないキー。ここに足す時は「ローカルの手順に一切写らない」ことを確認すること。
EXCLUDE = {"timeout-minutes", "runs-on", "permissions", "concurrency"}

out = []

def fmt_key(k):
    # YAML 1.1 では裸の `on:` が真偽値 True として解釈される。文字列に戻さないと
    # トリガ節がまるごと別キーになり、変更が無言で監視外になる。
    if k is True:
        return "on"
    if k is False:
        return "off"
    return str(k)

def emit(path, value):
    if value is None:
        out.append(f"{path} = ~")
        return
    if isinstance(value, bool):
        out.append(f"{path} = {'true' if value else 'false'}")
        return
    s = str(value)
    # 行頭・行末の空白は落とす（整形の揺れで落ちないように）。
    lines = [ln.strip() for ln in s.splitlines()]
    lines = [ln for ln in lines if ln]
    if len(lines) <= 1:
        # `run: cmd` と `run: |` ＋ 1 行は同じものを実行する。同じ表現に畳んで、
        # 書き方を変えただけでドリフト扱いにしない。
        out.append(f"{path} = {lines[0] if lines else ''}")
    else:
        # 複数行スクリプトは行ごとに出す（diff を読める形に保つ）。
        out.append(f"{path} |")
        out.extend(f"    {ln}" for ln in lines)

def walk(path, node, key=None):
    if key in EXCLUDE:
        return
    if key == "uses" and isinstance(node, str):
        # バージョンは落とす。アクションの追加・削除は検出するが bump では落ちない。
        emit(path, node.split("@", 1)[0])
        return
    if isinstance(node, dict):
        for k in sorted(node.keys(), key=fmt_key):
            kk = fmt_key(k)
            if kk in EXCLUDE:
                continue
            walk(f"{path}.{kk}", node[k], kk)
    elif isinstance(node, list):
        # 添字は 0 埋めする。しないと steps[10] が steps[2] より前に並び、
        # ソート済み出力の意味が崩れる。
        width = max(2, len(str(len(node) - 1)))
        for idx, item in enumerate(node):
            walk(f"{path}[{idx:0{width}d}]", item, None)
    else:
        emit(path, node)

files = sorted(p for p in WORKFLOW_DIR.iterdir() if p.suffix in (".yml", ".yaml"))
if not files:
    print("ワークフローが 1 つも見つかりません", file=sys.stderr)
    sys.exit(2)

for f in files:
    try:
        doc = yaml.safe_load(f.read_text(encoding="utf-8"))
    except Exception as e:  # noqa: BLE001 — 解析不能は握り潰さず落とす
        print(f"{f} の解析に失敗しました: {e}", file=sys.stderr)
        sys.exit(2)
    if doc is None:
        print(f"{f} が空です", file=sys.stderr)
        sys.exit(2)
    out.append(f"### {f.name}")
    walk(f.name, doc, None)
    out.append("")

print("\n".join(out).strip() + "\n", end="")
PY
}

MODE="${1:-print}"
case "$MODE" in
  print)
    normalize
    ;;
  --update)
    # ⚠️ `normalize > "$SNAPSHOT"` と直接書かないこと。リダイレクトは normalize の実行前に
    #    ファイルを truncate するため、解析に失敗すると **追跡済みのスナップショットが
    #    0 バイトで残る**（実測 10349 → 0）。`.claude/**` は CI の paths-ignore 対象なので、
    #    空のまま push しても CI は気づかない。一時ファイルへ書いてから差し替える。
    mkdir -p "$(dirname "$SNAPSHOT")"
    tmpdir=$(mktemp -d)
    trap 'rm -rf "$tmpdir"' EXIT
    normalize > "$tmpdir/new"
    mv "$tmpdir/new" "$SNAPSHOT"
    echo "スナップショットを更新しました: $SNAPSHOT"
    echo "⚠️  local-gates.sh・references/gates.md・AGENTS.md の追随を先に済ませてからコミットすること。"
    ;;
  --check)
    if [ ! -f "$SNAPSHOT" ]; then
      echo "スナップショットがありません: $SNAPSHOT" >&2
      echo "  初回は次で作成してください: $SELF --update" >&2
      exit 1
    fi
    # 専用ディレクトリを使う（/tmp に予測可能な名前のファイルを作らない）。
    tmpdir=$(mktemp -d)
    trap 'rm -rf "$tmpdir"' EXIT
    normalize > "$tmpdir/new"
    # diff の exit code は 0=同一 / 1=差あり / 2=エラー。2 を「差あり」と混同しない。
    # stderr は分けて捕まえる（本文と混ぜると後段の整形で消える）。
    set +e
    diff -u "$SNAPSHOT" "$tmpdir/new" > "$tmpdir/diff" 2> "$tmpdir/err"
    rc=$?
    set -e
    case "$rc" in
      0) echo "ワークフローのドリフトなし" ;;
      1)
        echo "ワークフローがスナップショットと食い違っています（- が記録済み / + が現在の定義）:"
        tail -n +3 "$tmpdir/diff"
        exit 1
        ;;
      *)
        echo "diff の実行に失敗しました（rc=$rc）:" >&2
        cat "$tmpdir/err" >&2
        exit 2
        ;;
    esac
    ;;
  -h|--help)
    # コメントヘッダの終わりまで出す（行数を固定で書くとコードが混ざる）。
    sed -n '2,${/^[^#]/q;p;}' "$SELF"
    ;;
  *)
    echo "未知の引数: $MODE" >&2
    exit 2
    ;;
esac
