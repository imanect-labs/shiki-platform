#!/usr/bin/env bash
# ゲストコマンドスイート（ls/cat/grep/curl 等）を wasm32-wasip1 でビルドし、**フラットな
# コマンドディレクトリ**にステージングする。orchestrator はこのディレクトリを
# `/__secure_exec/commands/0` に host_dir マウントし（$PATH に載る・kernel 管理 stdio で実行）、
# `SANDBOX__COMMANDS_DIR` で場所を指す。
#
# なぜ tar 投影（packages）でなく host_dir か: package.tar 投影では native wasm コマンドの stdio が
# ProcessOutputEvent に surface せず出力が返らない（#109 の調査）。upstream の実行テストと同じ
# host_dir コマンドルート経路を使う。
#
# ⚠️ 重いビルド。専用 Docker ステージ / CI ジョブでのみ実行する（高速 unit-test パスには載せない）。
# 必要ツール:
#   - nightly Rust（vendor/secure-exec/registry/native/rust-toolchain.toml で pin・-Z build-std 用）
#   - wasm32-wasip1 ターゲット・wasm-opt（binaryen・無ければ cargo install される）
#   - curl/wget 等の C ポートは wasi-sdk（registry/native/c が取得）＋ cmake
#
# 実行時ダウンロードは行わない（PIT-33）。ツールチェーン/wasi-sdk 取得はビルド前段のみ。
#
# 使い方:
#   scripts/build-sandbox-commands.sh                 # Rust コマンドのみ（make wasm）
#   BUILD_C=1 scripts/build-sandbox-commands.sh       # C ポート（curl/wget 等）も含める（make commands）
#   COMMANDS_OUT=/opt/shiki/commands scripts/build-sandbox-commands.sh
set -euo pipefail

ROOT="$(git rev-parse --show-toplevel 2>/dev/null || { cd "$(dirname "$0")/.." && pwd; })"
NATIVE="$ROOT/vendor/secure-exec/registry/native"
COMMANDS_DIR="${COMMANDS_DIR:-$NATIVE/target/wasm32-wasip1/release/commands}"
COMMANDS_OUT="${COMMANDS_OUT:-$ROOT/target/sandbox-commands}"

if [ "${SKIP_BUILD:-0}" != "1" ]; then
  if [ "${BUILD_C:-0}" = "1" ]; then
    echo "→ wasm コマンド群をビルド（Rust ＋ C ポート・registry/native）"
    make -C "$NATIVE" commands COMMANDS_DIR="$COMMANDS_DIR"
  else
    echo "→ wasm コマンド群をビルド（Rust のみ・registry/native）"
    make -C "$NATIVE" wasm COMMANDS_DIR="$COMMANDS_DIR"
  fi
fi

echo "→ コマンドを $COMMANDS_OUT にステージング（フラット・symlink は実体化）"
rm -rf "$COMMANDS_OUT"
mkdir -p "$COMMANDS_OUT"
count=0
for f in "$COMMANDS_DIR"/*; do
  [ -e "$f" ] || continue
  name="$(basename "$f")"
  # alias/stub は symlink のことがあるので -L で実体をコピーする。
  cp -L "$f" "$COMMANDS_OUT/$name" 2>/dev/null || continue
  count=$((count + 1))
done

echo "✅ $count コマンドを $COMMANDS_OUT にステージングしました（SANDBOX__COMMANDS_DIR に指定）。"
