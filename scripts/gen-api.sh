#!/usr/bin/env bash
# Rust 定義から型契約を再生成する（手書き型を作らない・生成物は commit しない）。
#   1. OpenAPI(utoipa) → web/src/generated/openapi.json
#   2. 認可語彙(ts-rs)  → web/src/generated/authz-vocab.ts
#   3. OpenAPI → TS 型  → web/src/generated/api.d.ts (openapi-typescript)
#
# 使い方: リポジトリルートまたは web/ から `pnpm gen:api`（= 本スクリプト）。
set -euo pipefail

# リポジトリルートを特定（このスクリプトは scripts/ 配下）。
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
OUT="$ROOT/web/src/generated"
mkdir -p "$OUT"

echo "[gen:api] OpenAPI 仕様を出力"
cargo run --quiet --manifest-path "$ROOT/Cargo.toml" -p shiki-api --bin export-openapi >"$OUT/openapi.json"

echo "[gen:api] 認可語彙(TS)を出力"
cargo run --quiet --manifest-path "$ROOT/Cargo.toml" -p shiki-authz --bin export-ts -- "$OUT"

echo "[gen:api] ワークフロー語彙・IR 型(TS)を出力"
cargo run --quiet --manifest-path "$ROOT/Cargo.toml" -p shiki-workflow-engine --bin export-workflow-ts -- "$OUT"

echo "[gen:api] generative UI カタログ・スペック型(TS)を出力"
cargo run --quiet --manifest-path "$ROOT/Cargo.toml" -p shiki-gui --bin export-gui-ts -- "$OUT"

echo "[gen:api] OpenAPI → TypeScript 型"
# web/ にインストール済みの openapi-typescript を使う（cwd 非依存）。
pnpm --dir "$ROOT/web" exec openapi-typescript "$OUT/openapi.json" -o "$OUT/api.d.ts"

echo "[gen:api] 完了: $OUT"
