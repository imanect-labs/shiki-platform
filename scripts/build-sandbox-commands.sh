#!/usr/bin/env bash
# ゲストコマンドスイート（ls/cd/curl/wget/git/grep 等）を wasm32-wasip1 でビルドし、
# 各 software パッケージの bin/ に配置する（sidecar が dir 記述子で読み、$PATH にリンクする）。
#
# ⚠️ 重いビルド。専用 Docker ステージ / CI ジョブでのみ実行する（高速 unit-test パスには載せない）。
# 必要ツール:
#   - nightly Rust（vendor/secure-exec/registry/native/rust-toolchain.toml で pin・-Z build-std 用）
#   - wasm32-wasip1 ターゲット・wasm-opt（binaryen）
#   - curl 系は wasi-sdk（registry/native/c が自動取得）
#
# 実行時ダウンロードは行わない（PIT-33）。ツールチェーン/wasi-sdk 取得はビルド前段のみ。
set -euo pipefail

ROOT="$(git rev-parse --show-toplevel)"
NATIVE="$ROOT/vendor/secure-exec/registry/native"
SOFTWARE="$ROOT/vendor/secure-exec/registry/software"
COMMANDS_DIR="${COMMANDS_DIR:-$NATIVE/target/wasm32-wasip1/release/commands}"

echo "→ wasm コマンド群をビルド（registry/native）"
make -C "$NATIVE" commands COMMANDS_DIR="$COMMANDS_DIR"

echo "→ 各 software パッケージの bin/ にステージング"
staged=0
for pkgdir in "$SOFTWARE"/*/; do
  manifest="$pkgdir/agentos-package.json"
  [ -f "$manifest" ] || continue
  # agentos-package.json の commands 配列を読む（jq が無ければ python3 で代替）
  cmds="$(jq -r '.commands[]' "$manifest" 2>/dev/null \
          || python3 -c "import json,sys;print('\n'.join(json.load(open('$manifest')).get('commands',[])))")"
  [ -n "$cmds" ] || continue
  mkdir -p "$pkgdir/bin"
  while read -r cmd; do
    [ -n "$cmd" ] || continue
    if [ -f "$COMMANDS_DIR/$cmd" ]; then
      cp "$COMMANDS_DIR/$cmd" "$pkgdir/bin/$cmd"
      staged=$((staged + 1))
    else
      echo "   ⚠️ 未ビルド: $cmd（$(basename "$pkgdir")）" >&2
    fi
  done <<< "$cmds"
done

echo "✅ コマンドステージング完了（$staged binaries）。sidecar には dir 記述子で渡す。"
