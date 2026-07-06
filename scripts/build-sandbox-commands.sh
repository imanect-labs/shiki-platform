#!/usr/bin/env bash
# ゲストコマンドスイート（ls/cat/grep 等）を wasm32-wasip1 でビルドし、software パッケージ
# （`<name>/package.tar`）としてステージングする。sidecar は tar 投影のみサポートするため、
# 各パッケージは `/agentos-package.json`（name/version を合成）＋ `/bin/<commands>` を含む
# tar に固める。orchestrator は `SANDBOX__SOFTWARE_DIR` に出力ディレクトリを指す。
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
#   SOFTWARE_OUT=/opt/shiki/software scripts/build-sandbox-commands.sh
set -euo pipefail

ROOT="$(git rev-parse --show-toplevel 2>/dev/null || { cd "$(dirname "$0")/.." && pwd; })"
NATIVE="$ROOT/vendor/secure-exec/registry/native"
SOFTWARE="$ROOT/vendor/secure-exec/registry/software"
COMMANDS_DIR="${COMMANDS_DIR:-$NATIVE/target/wasm32-wasip1/release/commands}"
SOFTWARE_OUT="${SOFTWARE_OUT:-$ROOT/target/sandbox-software}"
PKG_VERSION="${PKG_VERSION:-0.0.0}"

if [ "${SKIP_BUILD:-0}" != "1" ]; then
  if [ "${BUILD_C:-0}" = "1" ]; then
    echo "→ wasm コマンド群をビルド（Rust ＋ C ポート・registry/native）"
    make -C "$NATIVE" commands COMMANDS_DIR="$COMMANDS_DIR"
  else
    echo "→ wasm コマンド群をビルド（Rust のみ・registry/native）"
    make -C "$NATIVE" wasm COMMANDS_DIR="$COMMANDS_DIR"
  fi
fi

echo "→ software パッケージ（package.tar）を $SOFTWARE_OUT にステージング"
mkdir -p "$SOFTWARE_OUT"
packed=0
for pkgdir in "$SOFTWARE"/*/; do
  name="$(basename "$pkgdir")"
  manifest="$pkgdir/agentos-package.json"
  [ -f "$manifest" ] || continue

  # commands / aliases / stubs を束ねてコマンド名一覧にする（registry manifest はパッカー向け形式）。
  cmds="$(python3 - "$manifest" <<'PY'
import json, sys
m = json.load(open(sys.argv[1]))
names = list(m.get("commands", [])) + list(m.get("aliases", [])) + list(m.get("stubs", []))
print("\n".join(dict.fromkeys(names)))
PY
)"
  [ -n "$cmds" ] || continue

  stage="$(mktemp -d)"
  mkdir -p "$stage/bin"
  staged_pkg=0
  while read -r cmd; do
    [ -n "$cmd" ] || continue
    if [ -e "$COMMANDS_DIR/$cmd" ]; then
      # symlink（alias/stub）は実体化して tar に入れる（-L）。
      cp -L "$COMMANDS_DIR/$cmd" "$stage/bin/$cmd"
      staged_pkg=$((staged_pkg + 1))
    fi
  done <<< "$cmds"
  if [ "$staged_pkg" -eq 0 ]; then
    rm -rf "$stage"
    continue
  fi

  # sidecar が要求する完全な manifest（name/version）を合成する。
  python3 - "$stage/agentos-package.json" "$name" "$PKG_VERSION" <<'PY'
import json, sys
json.dump({"name": sys.argv[2], "version": sys.argv[3]}, open(sys.argv[1], "w"))
PY

  mkdir -p "$SOFTWARE_OUT/$name"
  tar -cf "$SOFTWARE_OUT/$name/package.tar" -C "$stage" agentos-package.json bin
  rm -rf "$stage"
  packed=$((packed + 1))
  echo "   ✓ $name（$staged_pkg commands）"
done

echo "✅ $packed パッケージを $SOFTWARE_OUT にステージングしました（SANDBOX__SOFTWARE_DIR に指定）。"
