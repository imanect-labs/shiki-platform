#!/usr/bin/env bash
# 結合テストの門番になる環境変数を `std::env::var` で直接読むことを禁じる CI ゲート。
#
# 結合テストは接続先の env が無いとスキップして pass に数えられる。CI ではそれを失敗に変えるため、
# 門番の変数は shiki_testenv::var で読む決まりにしている（crates/testenv/src/lib.rs）。直接読むと、
# CI で env を渡し忘れたときに黙ってスキップされる穴が戻る。
#
# 門番とみなす変数名: *_TEST_URL / *_TEST_DATABASE_URL / *_TEST_WS（外部依存の接続先）と
# SANDBOX_*IT（VM 系サンドボックスの実走フラグ）。toolchain 不要（bash+git+grep）なので quality ジョブで回す。
set -euo pipefail

cd "$(git rev-parse --show-toplevel)"

pattern='(^|[^_[:alnum:]])env::var(_os)?\("([A-Z0-9_]*_TEST_(URL|DATABASE_URL|WS)|SANDBOX_[A-Z0-9_]*IT)"\)'

if hits=$(git grep -n -E "$pattern" -- 'crates/*.rs' ':!crates/testenv/**' || true) && [ -n "$hits" ]; then
  echo "❌ 結合テストの接続先を std::env::var で直接読んでいます（shiki_testenv::var を使うこと）:" >&2
  echo "$hits" >&2
  echo >&2
  echo "直接読むと、CI（SHIKI_REQUIRE_IT=1）で env を渡し忘れても黙ってスキップされます。" >&2
  echo "dev-dependencies に shiki-testenv.workspace = true を足し、shiki_testenv::var(\"…\") に置き換えてください。" >&2
  exit 1
fi

echo "✅ it-env gate: 結合テストの接続先はすべて shiki_testenv::var 経由"
