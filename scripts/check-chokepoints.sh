#!/usr/bin/env bash
# 単一チョークポイント（AGENTS.md の不変条件・docs/design.md §1）を迂回する書き方を字句で止める CI ゲート。
# toolchain 不要（bash+git+grep）なので quality ジョブで回す。
#
# 依存グラフで縛れるもの（aws-sdk-s3・tantivy）は deny.toml の [bans] wrappers が担う。ここは、汎用の
# HTTP クライアント（reqwest）で外部 API を直接叩く経路を見る。reqwest 自体は正当な用途が多く依存では縛れない。
#
#   C1: LLM プロバイダの API を llm-gateway 以外から呼ばない（利用量の記録・上限・監査・プロバイダ制限を素通りする）
#   C2: OpenFGA の HTTP API を authz 以外から呼ばない（テナント名前空間化・整合性モードを素通りする）
#   C3: Qdrant の REST API を rag 以外から呼ばない（二段 authz の pre-filter を素通りする）
#
# 対象は crates/*/src（本番コード）。テスト専用ファイル（tests.rs・*_tests.rs）とコメント行は見ない。本番ファイルの中の
# テスト用文字列など正当な例外は、同じ行に `chokepoint-ok: <理由>` のコメントを書いて外す。
set -euo pipefail

cd "$(git rev-parse --show-toplevel)"

status=0

# $1: 規則名  $2: 許すクレート（crates/<名前>）  $3: 正規表現  $4: 説明
check() {
  local rule=$1 owner=$2 pattern=$3 why=$4 hits
  hits=$(git grep --untracked -n -E "$pattern" -- 'crates/*/src/*.rs' ":!crates/$owner/**" \
           ':!crates/*/src/tests.rs' ':!crates/*/src/*_tests.rs' ':!crates/*/src/*/tests.rs' ':!crates/*/src/*/*_tests.rs' \
         | grep -vE '^[^:]+:[0-9]+:[[:space:]]*//' | grep -v 'chokepoint-ok:' || true)
  if [ -n "$hits" ]; then
    echo "❌ $rule: $why（crates/$owner 経由にすること）:" >&2
    echo "$hits" >&2
    status=1
  fi
}

check C1 llm-gateway \
  '/chat/completions|/v1/messages|/v1/responses|api\.openai\.com|api\.anthropic\.com|generativelanguage\.googleapis\.com' \
  'LLM プロバイダの API を llm-gateway 以外から呼んでいます'
check C2 authz \
  '/stores(/|")' \
  'OpenFGA の HTTP API を authz 以外から呼んでいます'
check C3 rag \
  '/collections/' \
  'Qdrant の REST API を rag 以外から呼んでいます'

if [ "$status" -eq 0 ]; then
  echo "✅ chokepoint gate: LLM・OpenFGA・Qdrant の API を所定のクレート以外から呼ぶ箇所なし"
else
  echo >&2
  echo "テスト用の文字列など正当な例外なら、同じ行に \`// chokepoint-ok: <理由>\` を書いてください。" >&2
fi
exit "$status"
