#!/usr/bin/env bash
# shiki script ゲスト wasm（QuickJS/javy）を再現ビルドする。
#
# wasi-sdk（clang+sysroot）と libclang（bindgen）を要するため CI では実行しない。
# pinned な Docker イメージ内でビルドし、成果物を crates/script-runtime/assets/ へ出力する。
# vendor/secure-exec と同じ「所有フォーク/所有バイナリ」統治モデル（docs/script-runtime-guest.md）。
#
# 使い方: bash scripts/build-qjs-guest.sh
set -euo pipefail

# 再現性のためツールチェーンを固定する（更新時はここと docs を同時に上げる）。
RUST_IMAGE="rust:1.96-bookworm"
TARGET="wasm32-wasip1"

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
GUEST_DIR="$REPO_ROOT/crates/script-runtime/guest"
OUT_DIR="$REPO_ROOT/crates/script-runtime/assets"
OUT_WASM="$OUT_DIR/shiki_qjs_guest.wasm"

mkdir -p "$OUT_DIR"

echo "==> ゲスト wasm を Docker（$RUST_IMAGE）で再現ビルドします"
# ビルド中間成果物（wasi-sdk 等・巨大）はコンテナ内 /tmp に置き、ソースツリーへ残さない。
docker run --rm \
  -v "$GUEST_DIR":/guest:ro \
  -v "$OUT_DIR":/out \
  -w /guest \
  "$RUST_IMAGE" \
  bash -c "
    set -euo pipefail
    apt-get update -qq
    apt-get install -y -qq libclang-dev clang >/dev/null
    rustup target add $TARGET
    cp -r /guest /build && cd /build
    CARGO_TARGET_DIR=/tmp/qjs-target cargo build --release --target $TARGET
    cp /tmp/qjs-target/$TARGET/release/shiki_qjs_guest.wasm /out/shiki_qjs_guest.wasm
  "
SIZE="$(du -h "$OUT_WASM" | cut -f1)"
echo "==> 出力: $OUT_WASM ($SIZE)"
echo "    このバイナリはコミット対象（in-repo vendor）。出所は docs/script-runtime-guest.md 参照。"
