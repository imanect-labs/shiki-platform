#!/usr/bin/env bash
# 結合テストの門番になる環境変数を `std::env::var` で直接読むことを禁じる CI ゲート。
#
# 結合テストは接続先の env が無いとスキップして pass に数えられる。CI ではそれを失敗に変えるため、
# 門番の変数は shiki_testenv::var で読む決まりにしている（crates/testenv/src/lib.rs）。直接読むと、
# CI で env を渡し忘れたときに黙ってスキップされる穴が戻る。
#
# 門番とみなす変数名: `_TEST_` を含み URL / WS / WOPI / FILES / COLLABORA で終わるもの（外部依存の接続先。
# STORAGE_TEST_DATABASE_URL・RAG_TEST_QDRANT_URL・OFFICE_ENGINE_TEST_WS 等）と、SANDBOX_*IT（VM 系
# サンドボックスの実走フラグ）。接続先に付随する任意の値（*_TEST_KEY・*_TEST_MODEL・STORAGE_TEST_S3_ENDPOINT 等）は
# 門番ではないので対象外。toolchain 不要（bash+git+grep）なので quality ジョブで回す。
#
# 字句で判定するので、定数経由（env::var(NAME)）は拾えない。門番の変数名は文字列リテラルで直接書くこと。
set -euo pipefail

cd "$(git rev-parse --show-toplevel)"

# `env::var("…")` に加え、`use std::env::var;` の後の素の `var("…")` も拾う（`::var(` は shiki_testenv 等なので除く）。
# rustfmt が引数を改行することもあるので、ファイル全体を 1 つの文字列として複数行にまたがって照合する。
pattern='(?:(?<![_[:alnum:]])env::|(?<![_[:alnum:]:.]))var(?:_os)?\(\s*"((?:[A-Z0-9_]*_TEST_(?:[A-Z0-9_]*_)?(?:URL|WS|WOPI|FILES|COLLABORA))|SANDBOX_[A-Z0-9_]*IT)"'

export IT_ENV_PATTERN="$pattern"
hits=$(git ls-files -z -- 'crates/*.rs' ':!crates/testenv/**' \
  | xargs -0 perl -0777 -ne '
      my $pat = $ENV{IT_ENV_PATTERN};
      while (/$pat/g) {
        my ($name, $at) = ($1, $-[0]);
        my $line = 1 + (() = substr($_, 0, $at) =~ /\n/g);
        print "$ARGV:$line: $name\n";
      }')
if [ -n "$hits" ]; then
  echo "❌ 結合テストの接続先を std::env::var で直接読んでいます（shiki_testenv::var を使うこと）:" >&2
  echo "$hits" >&2
  echo >&2
  echo "直接読むと、CI（SHIKI_REQUIRE_IT=1）で env を渡し忘れても黙ってスキップされます。" >&2
  echo "dev-dependencies に shiki-testenv.workspace = true を足し、shiki_testenv::var(\"…\") に置き換えてください。" >&2
  exit 1
fi

echo "✅ it-env gate: 結合テストの接続先はすべて shiki_testenv::var 経由"
