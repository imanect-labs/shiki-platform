#!/usr/bin/env bash
# シャード安全の禁止事項（docs/data/shard-safety.md）のうち、字句だけで判定できるものを検査する
# CI ゲート（Phase 15 Task 15.1）。toolchain 不要（bash+git+grep）なので quality ジョブで最速に落とす。
#
#   S1: crates/data/src が接続プール型 PgPool を直接参照しない（PoolRouter 経由に統一）
#   S2: crates/data/src がシーケンス関数（nextval/currval/lastval/setval）を直接呼ばない
#       （tenant_int / table_int の採番はレジストリの採番サービス crates/storage/src/tenant.rs 経由）
#
# SQL リテラル単位の検査（S3: data_* への SQL は tenant 条件を持つ／S4: data_* 表どうしの JOIN 禁止）は
# 字句解析が要るため crates/data/tests/shard_safety_lint.rs（cargo test）が担う。
set -euo pipefail

cd "$(git rev-parse --show-toplevel)"

TARGET="${SHARD_SAFETY_TARGET:-crates/data/src}"
status=0

if hits=$(git grep -n -E '\bPgPool\b' -- "$TARGET" || true) && [ -n "$hits" ]; then
  echo "❌ S1: $TARGET が PgPool を直接参照しています（storage::PoolRouter 経由にすること）:" >&2
  echo "$hits" >&2
  status=1
fi

if hits=$(git grep -n -E '\b(nextval|currval|lastval|setval)\(' -- "$TARGET" || true) && [ -n "$hits" ]; then
  echo "❌ S2: $TARGET がシーケンス関数を直接呼んでいます（採番は storage::tenant の採番サービス経由）:" >&2
  echo "$hits" >&2
  status=1
fi

if [ "$status" -eq 0 ]; then
  echo "✅ shard-safety gate: $TARGET に S1/S2 の違反なし"
fi
exit "$status"
